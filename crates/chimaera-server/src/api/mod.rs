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
        if state.policy().active(&state)
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
    // Manual resume is Pro's too (`api/manual_resume.rs`).
    let manual_resume = path
        .strip_prefix("/sessions/")
        .is_some_and(|rest| rest.split('/').nth(1) == Some("resume"));
    if (path.starts_with("/pro/") || manual_resume) && !state.policy().composed(&state) {
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
    // Additive fields a composed extension serves (nothing without one).
    state.policy().health(&state, &mut value);
    Json(value)
}
