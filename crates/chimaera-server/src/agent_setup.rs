//! Noninteractive managed installs. One bounded result per built-in provider,
//! shared host install locks, and no PTY/session/ledger entries. Account and
//! chat readiness are deliberately not inferred from an installer exit code.
//! The operation itself — join, cancel, output cap, the process loop — is
//! `operation`'s; this module owns the install script, the words and the
//! shared install lock.
use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    agents::AgentKind,
    lock,
    operation::{self, CancelRefused, Exit, OutputEnd, Phase, Run, Stdin},
    AppState,
};

const DEADLINE: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Install,
    Update,
    Reinstall,
}

/// The wire record of `GET /agents/{id}/setup` (`operation`) and every
/// POST/DELETE answer; the catalog row carries its `summary`.
#[derive(Clone, Serialize)]
pub(crate) struct Progress {
    id: String,
    action: Action,
    phase: Phase,
    workspace_id: String,
    started_at: u64,
    exit_status: Option<i32>,
    message: String,
    output: String,
    truncated: bool,
}
impl operation::Progress for Progress {
    fn id(&self) -> &str {
        &self.id
    }
    fn phase(&self) -> Phase {
        self.phase
    }
    fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
    }
    fn set_message(&mut self, message: &str) {
        self.message = message.into();
    }
    fn on_output(&mut self, bytes: &[u8]) {
        // Plain text on the wire: the installer's colours and cursor moves
        // go, newlines and tabs stay.
        let text = operation::strip_controls(&String::from_utf8_lossy(bytes));
        if operation::append_capped(&mut self.output, &text) {
            self.truncated = true;
        }
    }
}

pub(crate) type Operations = operation::Operations<Progress>;
type Operation = operation::Operation<Progress>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    workspace_id: String,
    action: Action,
    request_id: String,
}

fn snapshot(state: &AppState, kind: AgentKind) -> Option<Progress> {
    state.agent_setup.snapshot(kind)
}
pub(crate) fn summary(state: &AppState, kind: AgentKind) -> Option<serde_json::Value> {
    state.agent_setup.summary(kind)
}

pub(crate) async fn get(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(kind) = operation::kind(&id) else {
        return operation::error(StatusCode::NOT_FOUND, "Unknown agent.");
    };
    Json(json!({
        "host": hostname::get().unwrap_or_default().to_string_lossy(),
        "root": state.managed_root,
        "operation": snapshot(&state, kind),
    }))
    .into_response()
}

pub(crate) async fn start(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Request>,
) -> Response {
    let Some(kind) = operation::kind(&id) else {
        return operation::error(StatusCode::NOT_FOUND, "Unknown agent.");
    };
    if !operation::valid_request_id(&body.request_id) {
        return operation::error(StatusCode::BAD_REQUEST, "Invalid setup request ID.");
    }
    // A repeated POST after a lost response observes the same operation. A
    // second window joins active work instead of creating a competing run.
    if let Some(p) = state.agent_setup.joinable(kind, &body.request_id) {
        return Json(p).into_response();
    }
    let Some(workspace) = lock(&state.workspaces).get(&body.workspace_id) else {
        return operation::error(
            StatusCode::NOT_FOUND,
            "Open a workspace on this host first.",
        );
    };
    if crate::agent_auth::running(&state, kind) {
        return operation::error(
            StatusCode::CONFLICT,
            "Finish signing in to this agent first.",
        );
    }
    if !matches!(body.action, Action::Install)
        && !crate::launcher::detect(&state, kind, false).await.managed
    {
        return operation::error(StatusCode::BAD_REQUEST, "Chimaera only updates or reinstalls its own copy. Check the executable in Agents settings.");
    }
    let Some(script) = crate::runtimes::install_script(kind, &state.managed_root) else {
        return operation::error(
            StatusCode::BAD_REQUEST,
            "This agent has no managed installer.",
        );
    };
    start_script(state, kind, workspace.root, body, script).await
}

async fn start_script(
    state: Arc<AppState>,
    kind: AgentKind,
    cwd: PathBuf,
    body: Request,
    script: String,
) -> Response {
    let install_lock = match crate::runtimes::lock_install(&state.managed_root, kind).await {
        Ok(file) => file,
        Err(response) => {
            if let Some(p) = state.agent_setup.joinable(kind, &body.request_id) {
                return Json(p).into_response();
            }
            return *response;
        }
    };
    let progress = Progress {
        id: body.request_id,
        action: body.action,
        phase: Phase::Running,
        workspace_id: body.workspace_id,
        started_at: crate::timeline::now_ms(),
        exit_status: None,
        message: "Downloading and installing the official release…".into(),
        output: String::new(),
        truncated: false,
    };
    // Decided under the lock, after the await above: an exceptionally fast
    // first call can already have finished and released the install lock
    // while this idempotent retry waited.
    let operation = match state.agent_setup.start(kind, progress.clone()) {
        Ok(operation) => operation,
        Err(existing) => return Json(existing).into_response(),
    };
    tokio::spawn(async move {
        crate::runtimes::prune_install_versions(state.managed_root.clone(), kind).await;
        let (phase, code, message) = run(&state, &operation, cwd, script).await;
        if phase == Phase::Succeeded {
            crate::runtimes::prune_install_versions(state.managed_root.clone(), kind).await;
        }
        operation.set_message("Refreshing the installed version…");
        // Keep the shared lock through detection; there must be no false
        // completion while a competing workspace changes the executable.
        crate::launcher::detect(&state, kind, true).await;
        let update = state.clone();
        let _ =
            tokio::task::spawn_blocking(move || crate::runtimes::regenerate_shims(&update)).await;
        drop(install_lock);
        operation.update(|p| {
            p.phase = phase;
            p.exit_status = code;
            p.message = message;
        });
        state.changes.notify_waiters();
    });
    Json(progress).into_response()
}

pub(crate) async fn cancel(
    State(state): State<Arc<AppState>>,
    Path((id, operation)): Path<(String, String)>,
) -> Response {
    let Some(kind) = operation::kind(&id) else {
        return operation::error(StatusCode::NOT_FOUND, "Unknown agent.");
    };
    match state
        .agent_setup
        .cancel(kind, &operation, "Stopping the installer…")
    {
        Ok(p) => Json(p).into_response(),
        Err(CancelRefused::Gone) => operation::error(
            StatusCode::NOT_FOUND,
            "This setup result is no longer available.",
        ),
        // An old dialog must never cancel a newer retry.
        Err(CancelRefused::Newer) => operation::error(
            StatusCode::CONFLICT,
            "A newer setup operation has started. Refresh its status.",
        ),
    }
}

async fn run(
    state: &AppState,
    op: &Arc<Operation>,
    cwd: PathBuf,
    script: String,
) -> (Phase, Option<i32>, String) {
    let env = crate::api::session_env(state, &op.snapshot().id, "dark", None);
    let remove = crate::api::spawn_env_remove(&env);
    let argv = vec!["/bin/bash".to_string(), "-c".to_string(), script];
    let exit = op
        .run(Run {
            command: operation::command(&argv, &cwd, env, remove),
            stdin: Stdin::Closed,
            deadline: DEADLINE,
            // The installer's output is the result: read it to the end.
            output: OutputEnd::Drained,
        })
        .await;
    match exit {
        Exit::NotStarted => (
            Phase::Cancelled,
            None,
            "Installation cancelled before it started.".into(),
        ),
        Exit::SpawnFailed(err) => (
            Phase::Failed,
            None,
            format!("Could not start the installer: {err}"),
        ),
        Exit::Exited(Some(0)) => (
            Phase::Succeeded,
            Some(0),
            "Installation finished. Sign-in and chat have not been checked.".into(),
        ),
        Exit::Exited(code) => (
            Phase::Failed,
            code,
            failure_hint(&op.snapshot().output).into(),
        ),
        Exit::Cancelled(code) => (
            Phase::Cancelled,
            code,
            "Installation cancelled. Files may already have changed; check the installed version before retrying.".into(),
        ),
        Exit::TimedOut(code) => (
            Phase::Failed,
            code,
            "The installer exceeded 15 minutes and was stopped. Check the output and connection, then retry.".into(),
        ),
    }
}
fn failure_hint(output: &str) -> &'static str {
    let lower = output.to_ascii_lowercase();
    if lower.contains("no space left") || lower.contains("disk quota") {
        "Not enough storage on this host. Free space or check your quota, then retry. See output for the affected path."
    } else if lower.contains("glibc_")
        || lower.contains("glibcxx_")
        || lower.contains("requires glibc")
    {
        "This release needs system libraries this host does not provide. Reinstalling the same release will not fix it. Use a compatible host or runtime."
    } else if lower.contains("permission denied") {
        "The installer could not access a file. Check the path and permissions in the output before retrying."
    } else {
        "Installation failed. Review the output below, resolve the reported problem, then retry."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::Progress as _;

    fn state() -> Arc<AppState> {
        let root = std::env::temp_dir().join(format!(
            "chimaera-setup-{}",
            chimaera_core::generate_token()
        ));
        Arc::new(AppState::new(
            "token".into(),
            "test".into(),
            1,
            0,
            root.join("data"),
            root.join("config"),
        ))
    }
    fn request(id: &str) -> Request {
        Request {
            workspace_id: "test".into(),
            action: Action::Install,
            request_id: id.into(),
        }
    }
    async fn completed(state: &AppState) -> Progress {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(p) = snapshot(state, AgentKind::Codex).filter(|p| !p.running()) {
                    return p;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn output_and_failed_exit_survive_without_a_session() {
        let state = state();
        let response = start_script(
            state.clone(),
            AgentKind::Codex,
            "/tmp".into(),
            request("one"),
            "echo 'No space left on device' >&2; exit 23".into(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let p = completed(&state).await;
        assert!(p.phase == Phase::Failed);
        assert_eq!(p.exit_status, Some(23));
        assert!(p.output.contains("No space left"));
        assert!(p.message.contains("storage"));
        assert!(state.sessions.list().is_empty());
    }
    #[tokio::test]
    async fn duplicate_requests_join_and_cancel_is_generation_scoped() {
        let state = state();
        start_script(
            state.clone(),
            AgentKind::Codex,
            "/tmp".into(),
            request("one"),
            "trap 'echo cleanup' EXIT; echo started; sleep 120".into(),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while !snapshot(&state, AgentKind::Codex)
                .unwrap()
                .output
                .contains("started")
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        start_script(
            state.clone(),
            AgentKind::Codex,
            "/tmp".into(),
            request("two"),
            "echo WRONG".into(),
        )
        .await;
        assert_eq!(snapshot(&state, AgentKind::Codex).unwrap().id, "one");
        let stale = cancel(State(state.clone()), Path(("codex".into(), "two".into()))).await;
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        assert!(snapshot(&state, AgentKind::Codex).unwrap().running());
        cancel(State(state.clone()), Path(("codex".into(), "one".into()))).await;
        let p = completed(&state).await;
        assert!(p.phase == Phase::Cancelled);
        assert!(p.output.contains("cleanup"));
    }
    #[tokio::test]
    async fn large_output_is_bounded_and_success_never_claims_authentication() {
        let state = state();
        start_script(
            state.clone(),
            AgentKind::Codex,
            "/tmp".into(),
            request("one"),
            "head -c 100000 /dev/zero | tr '\\0' x; echo end".into(),
        )
        .await;
        let p = completed(&state).await;
        assert!(p.phase == Phase::Succeeded);
        assert!(p.output.len() <= operation::OUTPUT_LIMIT);
        assert!(p.output.ends_with("end\n"));
        assert!(p.truncated);
        assert!(p.message.contains("have not been checked"));
        start_script(
            state.clone(),
            AgentKind::Codex,
            "/tmp".into(),
            request("one"),
            "echo WRONG; exit 9".into(),
        )
        .await;
        assert_eq!(snapshot(&state, AgentKind::Codex).unwrap().output, p.output);
    }
    #[test]
    fn incompatible_libraries_do_not_offer_an_unhelpful_retry() {
        assert!(failure_hint("libc.so.6: GLIBC_2.28 not found")
            .contains("Reinstalling the same release will not fix it"));
    }
}
