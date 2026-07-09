use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

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

    let outcome = crate::exec::run_exec(
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
