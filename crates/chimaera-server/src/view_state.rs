//! Per-window view state: opaque JSON blobs keyed by client-generated window
//! ids (layout tree, focus-mode flag, zoom state). Stored as a single JSON
//! object file, `view-state.json`, load-tolerant like `workspaces.json`.

use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::AppState;

/// Maximum stored blob size (the raw PUT body).
const MAX_STATE_BYTES: usize = 64 * 1024;

/// In-memory key -> JSON value map backed by a single JSON object file
/// (save-on-change). Values are opaque to the server.
pub(crate) struct ViewStateStore {
    path: PathBuf,
    items: serde_json::Map<String, serde_json::Value>,
}

impl ViewStateStore {
    /// Load the store from `path`. A missing or corrupt file yields an empty
    /// store (with a warning for the corrupt case).
    pub(crate) fn load(path: PathBuf) -> Self {
        let items = match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
                Ok(serde_json::Value::Object(map)) => map,
                Ok(_) => {
                    tracing::warn!(path = %path.display(), "view-state.json is not a JSON object; starting with empty view state");
                    serde_json::Map::new()
                }
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "corrupt view-state.json; starting with empty view state");
                    serde_json::Map::new()
                }
            },
            Err(err) if err.kind() == ErrorKind::NotFound => serde_json::Map::new(),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "failed to read view-state.json; starting with empty view state");
                serde_json::Map::new()
            }
        };
        ViewStateStore { path, items }
    }

    pub(crate) fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.items.get(key).cloned()
    }

    /// Store `value` under `key` and persist.
    pub(crate) fn put(&mut self, key: String, value: serde_json::Value) -> anyhow::Result<()> {
        self.items.insert(key, value);
        self.save()
    }

    /// Atomically persist the map (tmp file + rename).
    fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&self.items)?)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to rename into {}", self.path.display()))?;
        Ok(())
    }
}

/// Keys are client-generated window ids: `[A-Za-z0-9_-]{1,64}`.
fn valid_key(key: &str) -> bool {
    (1..=64).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn bad_key(key: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": format!("invalid view-state key {key:?} (want [A-Za-z0-9_-]{{1,64}})")})),
    )
        .into_response()
}

/// GET /api/v1/view-state/{key}
pub(crate) async fn get_view_state(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Response {
    if !valid_key(&key) {
        return bad_key(&key);
    }
    match crate::lock(&state.view_state).get(&key) {
        Some(value) => Json(json!({"state": value})).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response(),
    }
}

/// PUT /api/v1/view-state/{key} — store any JSON value up to 64KB; 204 on
/// success.
pub(crate) async fn put_view_state(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    body: Bytes,
) -> Response {
    if !valid_key(&key) {
        return bad_key(&key);
    }
    if body.len() > MAX_STATE_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": format!("view state exceeds {MAX_STATE_BYTES} bytes")})),
        )
            .into_response();
    }
    let value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("invalid JSON: {err}")})),
            )
                .into_response();
        }
    };
    match crate::lock(&state.view_state).put(key, value) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            tracing::error!(%err, "failed to persist view state");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}
