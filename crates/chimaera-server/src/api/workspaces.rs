use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::workspaces::Workspace;
use crate::AppState;

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
    // `canonicalize` and `is_dir` are blocking fs syscalls on a user-supplied
    // path — a slow or dead NFS mount would otherwise stall the async reactor.
    // Validate off the reactor; the error strings are unchanged.
    let input = body.root.clone();
    let validated = tokio::task::spawn_blocking(move || {
        let root = std::fs::canonicalize(PathBuf::from(&input))
            .map_err(|err| format!("{input}: {err}"))?;
        if !root.is_dir() {
            return Err(format!("{} is not a directory", root.display()));
        }
        Ok::<PathBuf, String>(root)
    })
    .await;
    let root = match validated {
        Ok(Ok(root)) => root,
        Ok(Err(msg)) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response();
        }
        Err(join_err) => {
            tracing::error!(%join_err, "workspace validation task failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "internal error"})),
            )
                .into_response();
        }
    };
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

/// POST /api/v1/workspaces/{id}/open — stamp a workspace as freshly opened
/// (home-screen recency), returning it.
pub(crate) async fn open_workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match crate::lock(&state.workspaces).touch(&id) {
        Some(workspace) => Json(workspace).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown workspace"})),
        )
            .into_response(),
    }
}

/// DELETE /api/v1/workspaces/{id} — unregister a workspace (files untouched).
pub(crate) async fn delete_workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match crate::lock(&state.workspaces).remove(&id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown workspace"})),
        )
            .into_response(),
        Err(err) => {
            tracing::error!(%err, "failed to persist workspace removal");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}
