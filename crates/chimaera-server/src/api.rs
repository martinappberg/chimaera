use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::workspaces::Workspace;
use crate::AppState;

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
        "hostname": state.hostname,
        "pid": state.pid,
        "uptime_secs": state.started.elapsed().as_secs(),
    }))
}

/// GET /api/v1/workspaces
pub(crate) async fn list_workspaces(State(state): State<Arc<AppState>>) -> Json<Vec<Workspace>> {
    Json(crate::lock(&state.workspaces).list())
}

#[derive(Deserialize)]
pub(crate) struct CreateWorkspace {
    root: String,
}

/// POST /api/v1/workspaces — register a directory, idempotent per canonical root.
pub(crate) async fn create_workspace(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateWorkspace>,
) -> Response {
    let root = match std::fs::canonicalize(PathBuf::from(&body.root)) {
        Ok(root) => root,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("{}: {err}", body.root)})),
            )
                .into_response();
        }
    };
    if !root.is_dir() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("{} is not a directory", root.display())})),
        )
            .into_response();
    }
    match crate::lock(&state.workspaces).add(root) {
        Ok(workspace) => Json(workspace).into_response(),
        Err(err) => {
            tracing::error!(%err, "failed to persist workspace");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}

/// Serialize a `SessionInfo` with the extra `workspace_id` field.
pub(crate) fn session_json(
    info: &chimaera_pty::SessionInfo,
    workspace_id: Option<String>,
) -> serde_json::Value {
    let mut map = match serde_json::to_value(info) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    map.insert(
        "workspace_id".to_string(),
        workspace_id.map_or(serde_json::Value::Null, serde_json::Value::String),
    );
    serde_json::Value::Object(map)
}

/// GET /api/v1/sessions
pub(crate) async fn list_sessions(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let sessions = state.sessions.list();
    let map = crate::lock(&state.session_workspaces);
    let items: Vec<serde_json::Value> = sessions
        .iter()
        .map(|info| session_json(info, map.get(&info.id).cloned()))
        .collect();
    Json(serde_json::Value::Array(items))
}

#[derive(Deserialize)]
pub(crate) struct CreateSession {
    workspace_id: String,
    #[serde(default)]
    name: Option<String>,
}

/// POST /api/v1/sessions — spawn a shell at the workspace root.
pub(crate) async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateSession>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&body.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {}", body.workspace_id)})),
        )
            .into_response();
    };
    let opts = chimaera_pty::SpawnOpts {
        cwd: workspace.root.clone(),
        name: body.name,
        cols: 80,
        rows: 24,
        command: None,
    };
    match state.sessions.spawn(opts) {
        Ok(info) => {
            crate::lock(&state.session_workspaces).insert(info.id.clone(), workspace.id.clone());
            Json(session_json(&info, Some(workspace.id))).into_response()
        }
        Err(err) => {
            tracing::error!(%err, "failed to spawn session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}

/// DELETE /api/v1/sessions/{id}
pub(crate) async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.sessions.kill(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}
