use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

/// Foreground commands that forward keystrokes to a shell somewhere else
/// (remote or containerized), making sentinel-typing over a `running` phase
/// safe. Anything else running in the foreground (sleep, vim, tail) refuses.
const SENTINEL_FOREGROUNDS: &[&str] = &[
    "ssh",
    "mosh",
    "mosh-client",
    "et",
    "docker",
    "podman",
    "kubectl",
    "oc",
    "singularity",
    "apptainer",
];

#[derive(Deserialize)]
pub(crate) struct ExecBody {
    command: String,
    /// Max runtime once typed (default 30s, capped at 1h).
    #[serde(default)]
    timeout_ms: Option<u64>,
    /// Max wait for the shell's prompt before typing (default 15s, capped at
    /// 10m). `0` means "only if free right now".
    #[serde(default)]
    queue_timeout_ms: Option<u64>,
}

/// POST /api/v1/sessions/{id}/exec — type a command into a live shell
/// session and wait for its outcome (the `run_in_terminal` mechanics).
pub(crate) async fn exec_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<ExecBody>,
) -> Response {
    // Agent sessions host a TUI, not a shell; typing commands into claude
    // would be chaos. Links (and this endpoint) are for terminals only.
    if crate::lock(&state.agents).contains_key(&id) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "cannot exec into an agent session"})),
        )
            .into_response();
    }
    if state.sessions.get(&id).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown session {id}")})),
        )
            .into_response();
    }

    let outcome = run_exec(
        &state,
        &id,
        body.command,
        body.timeout_ms,
        body.queue_timeout_ms,
    )
    .await;

    match outcome {
        Ok(outcome) => Json(json!(outcome)).into_response(),
        Err(err) => {
            let code = match &err {
                chimaera_pty::ExecError::Busy(_) => StatusCode::CONFLICT,
                chimaera_pty::ExecError::InvalidCommand(_) => StatusCode::BAD_REQUEST,
                chimaera_pty::ExecError::SessionGone => StatusCode::NOT_FOUND,
                chimaera_pty::ExecError::NeverStarted(_) => StatusCode::GATEWAY_TIMEOUT,
            };
            (code, Json(json!({"error": err.to_string()}))).into_response()
        }
    }
}

/// Run an exec with the server's policy attached: sentinel over a *running*
/// phase only when the foreground forwards keystrokes elsewhere (ssh and
/// friends), and the stage (queued/executing) mirrored into session
/// snapshots for the UI's linked-terminal chips. Shared by the REST
/// endpoint and the MCP `run_in_terminal` tool.
pub(crate) async fn run_exec(
    state: &Arc<AppState>,
    id: &str,
    command: String,
    timeout_ms: Option<u64>,
    queue_timeout_ms: Option<u64>,
) -> Result<chimaera_pty::ExecOutcome, chimaera_pty::ExecError> {
    let allow_sentinel_over_running = state
        .sessions
        .foreground_pid(id)
        .and_then(crate::naming::comm_name)
        .is_some_and(|comm| SENTINEL_FOREGROUNDS.contains(&comm.as_str()));

    let (stage_tx, mut stage_rx) = tokio::sync::watch::channel(chimaera_pty::ExecStage::Queued);
    let mirror = {
        let state = state.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            loop {
                let stage = *stage_rx.borrow_and_update();
                crate::lock(&state.exec_status).insert(id.clone(), stage);
                state.changes.notify_waiters();
                if stage_rx.changed().await.is_err() {
                    return;
                }
            }
        })
    };

    let outcome = state
        .sessions
        .exec(
            id,
            chimaera_pty::ExecOptions {
                command,
                queue_timeout: std::time::Duration::from_millis(
                    queue_timeout_ms.unwrap_or(15_000).min(600_000),
                ),
                timeout: std::time::Duration::from_millis(
                    timeout_ms.unwrap_or(30_000).min(3_600_000),
                ),
                allow_sentinel_over_running,
                stage: Some(stage_tx),
            },
        )
        .await;

    mirror.abort();
    crate::lock(&state.exec_status).remove(id);
    state.changes.notify_waiters();
    outcome
}

#[derive(Deserialize)]
pub(crate) struct JournalQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// GET /api/v1/sessions/{id}/journal — the session's command journal (what
/// `read_terminal` returns): recent commands with output, exit codes, cwd.
pub(crate) async fn session_journal(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<JournalQuery>,
) -> Response {
    let Some(marks) = state.sessions.marks(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown session {id}")})),
        )
            .into_response();
    };
    let limit = query.limit.unwrap_or(20).min(200);
    Json(json!({
        "phase": marks.phase(),
        "entries": marks.journal(limit),
    }))
    .into_response()
}
