//! Noninteractive managed installs. One bounded result per built-in provider,
//! shared host install locks, and no PTY/session/ledger entries. Account and
//! chat readiness are deliberately not inferred from an installer exit code.
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::watch,
};

use crate::{agents::AgentKind, lock, AppState};

const LOG_LIMIT: usize = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Install,
    Update,
    Reinstall,
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

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
impl Progress {
    fn running(&self) -> bool {
        matches!(self.phase, Phase::Running | Phase::Cancelling)
    }
}

pub(crate) struct Operation {
    progress: Mutex<Progress>,
    cancel: watch::Sender<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    workspace_id: String,
    action: Action,
    request_id: String,
}

fn kind(id: &str) -> Option<AgentKind> {
    AgentKind::parse(id).filter(|k| AgentKind::ALL.contains(k))
}
fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}
fn snapshot(state: &AppState, kind: AgentKind) -> Option<Progress> {
    lock(&state.agent_setup)
        .get(&kind)
        .map(|op| lock(&op.progress).clone())
}
pub(crate) fn summary(state: &AppState, kind: AgentKind) -> Option<serde_json::Value> {
    snapshot(state, kind).map(|p| json!({"id": p.id, "running": p.running(), "phase": p.phase}))
}

pub(crate) async fn get(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(kind) = kind(&id) else {
        return error(StatusCode::NOT_FOUND, "Unknown agent.");
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
    let Some(kind) = kind(&id) else {
        return error(StatusCode::NOT_FOUND, "Unknown agent.");
    };
    if body.request_id.is_empty()
        || body.request_id.len() > 128
        || !body
            .request_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return error(StatusCode::BAD_REQUEST, "Invalid setup request ID.");
    }
    // A repeated POST after a lost response observes the same operation. A
    // second window joins active work instead of creating a competing run.
    if let Some(p) = snapshot(&state, kind).filter(|p| p.running() || p.id == body.request_id) {
        return Json(p).into_response();
    }
    let Some(workspace) = lock(&state.workspaces).get(&body.workspace_id) else {
        return error(
            StatusCode::NOT_FOUND,
            "Open a workspace on this host first.",
        );
    };
    if !matches!(body.action, Action::Install)
        && !crate::launcher::detect(&state, kind, false).await.managed
    {
        return error(StatusCode::BAD_REQUEST, "Chimaera only updates or reinstalls its own copy. Check the executable in Agents settings.");
    }
    let Some(script) = crate::runtimes::install_script(kind, &state.managed_root) else {
        return error(
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
            if let Some(p) =
                snapshot(&state, kind).filter(|p| p.running() || p.id == body.request_id)
            {
                return Json(p).into_response();
            }
            return *response;
        }
    };
    // Re-check after the await: an exceptionally fast first call can already
    // have finished and released the lock while this idempotent retry waited.
    if let Some(p) = snapshot(&state, kind).filter(|p| p.running() || p.id == body.request_id) {
        return Json(p).into_response();
    }
    let (cancel, receiver) = watch::channel(false);
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
    let operation = Arc::new(Operation {
        progress: Mutex::new(progress.clone()),
        cancel,
    });
    lock(&state.agent_setup).insert(kind, operation.clone());
    tokio::spawn(async move {
        crate::runtimes::prune_install_versions(state.managed_root.clone(), kind).await;
        let (phase, code, message) = run(&state, &operation, receiver, cwd, script).await;
        if phase == Phase::Succeeded {
            crate::runtimes::prune_install_versions(state.managed_root.clone(), kind).await;
        }
        lock(&operation.progress).message = "Refreshing the installed version…".into();
        // Keep the shared lock through detection; there must be no false
        // completion while a competing workspace changes the executable.
        crate::launcher::detect(&state, kind, true).await;
        let update = state.clone();
        let _ =
            tokio::task::spawn_blocking(move || crate::runtimes::regenerate_shims(&update)).await;
        drop(install_lock);
        finish(&operation, phase, code, &message);
        state.changes.notify_waiters();
    });
    Json(progress).into_response()
}

pub(crate) async fn cancel(
    State(state): State<Arc<AppState>>,
    Path((id, operation)): Path<(String, String)>,
) -> Response {
    let Some(kind) = kind(&id) else {
        return error(StatusCode::NOT_FOUND, "Unknown agent.");
    };
    let Some(op) = lock(&state.agent_setup).get(&kind).cloned() else {
        return error(
            StatusCode::NOT_FOUND,
            "This setup result is no longer available.",
        );
    };
    let mut p = lock(&op.progress);
    // An old dialog must never cancel a newer retry.
    if p.id != operation {
        return error(
            StatusCode::CONFLICT,
            "A newer setup operation has started. Refresh its status.",
        );
    }
    if p.running() {
        p.phase = Phase::Cancelling;
        p.message = "Stopping the installer…".into();
        op.cancel.send_replace(true);
    }
    Json(p.clone()).into_response()
}

fn append(op: &Operation, bytes: &[u8]) {
    let mut p = lock(&op.progress);
    // Render plain text only. Keep newlines/tabs but remove terminal controls.
    p.output.extend(
        String::from_utf8_lossy(bytes)
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t')),
    );
    if p.output.len() > LOG_LIMIT {
        let mut cut = p.output.len() - LOG_LIMIT;
        while !p.output.is_char_boundary(cut) {
            cut += 1;
        }
        p.output.drain(..cut);
        p.truncated = true;
    }
}
async fn drain(mut pipe: impl AsyncRead + Unpin, op: &Operation) {
    let mut buf = [0; 4096];
    while let Ok(n) = pipe.read(&mut buf).await {
        if n == 0 {
            break;
        }
        append(op, &buf[..n]);
    }
}

struct Group(nix::unistd::Pid);
impl Drop for Group {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(self.0, nix::sys::signal::Signal::SIGKILL);
    }
}

async fn run(
    state: &AppState,
    op: &Operation,
    mut cancel: watch::Receiver<bool>,
    cwd: PathBuf,
    script: String,
) -> (Phase, Option<i32>, String) {
    if *cancel.borrow() {
        return (
            Phase::Cancelled,
            None,
            "Installation cancelled before it started.".into(),
        );
    }
    let env = crate::api::session_env(state, &lock(&op.progress).id, "dark", None);
    let remove = crate::api::spawn_env_remove(&env);
    let mut cmd = tokio::process::Command::new("/bin/bash");
    cmd.args(["-c", &script])
        .current_dir(cwd)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    for name in remove {
        cmd.env_remove(name);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return (
                Phase::Failed,
                None,
                format!("Could not start the installer: {err}"),
            );
        }
    };
    let group = Group(nix::unistd::Pid::from_raw(child.id().unwrap() as i32));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let reads = async {
        tokio::join!(drain(stdout, op), drain(stderr, op));
    };
    tokio::pin!(reads);
    // Cancellation also handles a request received before the process started.
    let stopped = async {
        if !*cancel.borrow() {
            let _ = cancel.changed().await;
        }
    };
    let result = tokio::select! {
        // Keep the leader unreaped while descendants hold the pipes open, so
        // cancellation cannot signal a recycled process-group ID.
        result = async { (&mut reads).await; child.wait().await } => Some(result),
        _ = stopped => None,
        _ = tokio::time::sleep(DEADLINE) => None,
    };
    if let Some(result) = result {
        // The leader is reaped; do not signal a potentially recycled group ID.
        std::mem::forget(group);
        let code = result.ok().and_then(|s| s.code());
        if code == Some(0) {
            (
                Phase::Succeeded,
                code,
                "Installation finished. Sign-in and chat have not been checked.".into(),
            )
        } else {
            let hint = failure_hint(&lock(&op.progress).output);
            (Phase::Failed, code, hint.into())
        }
    } else {
        let cancelled = *cancel.borrow();
        // Give EXIT traps time to remove staging downloads, then stop the
        // entire group. Wait AFTER kill so the group ID cannot be recycled.
        let _ = nix::sys::signal::killpg(group.0, nix::sys::signal::Signal::SIGTERM);
        let _ = tokio::time::timeout(Duration::from_secs(2), &mut reads).await;
        drop(group);
        let code = child.wait().await.ok().and_then(|s| s.code());
        (
            if cancelled {
                Phase::Cancelled
            } else {
                Phase::Failed
            },
            code,
            if cancelled {
                "Installation cancelled. Files may already have changed; check the installed version before retrying."
            } else {
                "The installer exceeded 15 minutes and was stopped. Check the output and connection, then retry."
            }.into(),
        )
    }
}
fn finish(op: &Operation, phase: Phase, code: Option<i32>, message: &str) {
    let mut p = lock(&op.progress);
    p.phase = phase;
    p.exit_status = code;
    p.message = message.into();
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
        assert!(p.output.len() <= LOG_LIMIT);
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
