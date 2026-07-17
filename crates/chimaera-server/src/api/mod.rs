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
mod sessions;
mod shutdown;
mod workspaces;

pub(crate) use env::{session_env, spawn_env_remove};
// `spawn_path` is exercised only by the lib.rs router tests.
#[cfg(test)]
pub(crate) use env::spawn_path;
pub(crate) use exec::{exec_session, session_journal};
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
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response()
    }
}

/// GET /api/v1/health
pub(crate) async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(json!({
        "name": "chimaera",
        "version": chimaera_core::VERSION,
        // The build id lets clients spot daemon/client skew (semver is the
        // 0.0.1 sentinel on every dev build, so it cannot).
        "build": chimaera_core::BUILD_ID,
        "hostname": state.hostname,
        "pid": state.pid,
        "uptime_secs": state.started.elapsed().as_secs(),
    }))
}
