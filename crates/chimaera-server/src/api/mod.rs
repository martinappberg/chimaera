use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::AppState;

mod env;
mod exec;
mod manual_resume;
mod sessions;
mod shutdown;
mod workspaces;

pub(crate) use env::{launcher_context_env, session_env, spawn_env_remove};
// `spawn_path` is exercised only by the lib.rs router tests.
#[cfg(test)]
pub(crate) use env::spawn_path;
pub(crate) use exec::{exec_session, session_journal};
pub(crate) use manual_resume::resume as resume_manual_session;
pub(crate) use sessions::{create_session, delete_session, list_sessions, rename_session};
pub(crate) use shutdown::{delete_all_sessions, shutdown};
pub(crate) use workspaces::{
    create_workspace, delete_mastermind, delete_workspace, list_workspaces, open_workspace,
    put_mastermind,
};

/// Require `Authorization: Bearer {token}` on /api/v1 routes.
pub(crate) async fn auth(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {}", state.token));

    if authorized {
        if crate::pro::tier(&state) == crate::pro::Tier::Active
            && crate::activity::is_change(req.method(), req.uri().path())
        {
            crate::activity::touch(&state);
        }
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response()
    }
}

/// `/pro/*` exists only on a daemon with the extension (or the account's
/// cloud): a daemon without it answers those paths as any unknown route.
pub(crate) async fn pro_routes(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    if path.starts_with("/pro/") && crate::pro::tier(&state) == crate::pro::Tier::Free {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    }
    next.run(req).await
}

/// GET /api/v1/health
pub(crate) async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut value = json!({
        "name": "chimaera",
        "version": chimaera_core::VERSION,
        // The build id lets clients spot daemon/client skew (semver is the
        // 0.0.1 sentinel on every dev build, so it cannot).
        "build": chimaera_core::BUILD_ID,
        "hostname": state.hostname,
        "pid": state.pid,
        "uptime_secs": state.started.elapsed().as_secs(),
    });
    if state.daemon_extension.is_some() {
        // Additive, and only when composed: a daemon without the extension
        // answers exactly as before. Assembly presence is independent of SDK
        // build compatibility.
        value["daemon_extension"] = json!(true);
    }
    if let Some(identity) = state
        .daemon_extension
        .as_ref()
        .and_then(|runtime| runtime.assembly_identity())
    {
        value["daemon_assembly"] = json!(identity);
    }
    if let Some(ack) = crate::pro::supervisor_cleanup_ack(&state) {
        value["supervisor_cleanup"] = json!(ack);
    }
    if crate::cloud::enabled() {
        value["pro_cloud_operations"] =
            json!(crate::cloud::active_operations() + crate::pro::active_operations(&state));
        // Additive: the last user change that is not session input (saves,
        // uploads, Git operations), for the machine's idle decision.
        value["last_activity_ms"] = json!(crate::activity::last_change(&state));
    }
    Json(value)
}
