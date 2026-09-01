//! Per-window view state: opaque JSON blobs keyed by client-generated window
//! ids (layout tree, focus-mode flag, zoom state). Stored as a single JSON
//! object file, `view-state.json`, load-tolerant like `workspaces.json`.

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::AppState;

/// Maximum stored blob size (the raw PUT body).
const MAX_STATE_BYTES: usize = 64 * 1024;
/// Maximum keys retained. Every browser tab ever opened mints its own window
/// key (and one more per workspace it visits), so an uncapped store grows
/// forever on a long-lived daemon — each entry up to 64 KiB, held in RSS and
/// rewritten on every layout change. Live windows re-PUT on every change,
/// which keeps them newest; the eviction only ever reaches tabs long gone.
const MAX_KEYS: usize = 128;

/// In-memory key -> JSON value map backed by a single JSON object file
/// (save-on-change). Values are opaque to the server.
pub(crate) struct ViewStateStore {
    path: PathBuf,
    items: serde_json::Map<String, serde_json::Value>,
    /// Keys oldest-first by last write, for eviction. Load seeds it in the
    /// map's order — alphabetical (`serde_json::Map` is a BTreeMap here), so
    /// hex window ids sort ahead of the `ws_*` workspace fallbacks and a
    /// one-time over-cap trim reaches dead windows first; every PUT moves
    /// its key to the back, so the front is the least recently written.
    recency: VecDeque<String>,
    /// Serializes writers on the blocking pool so a slow NFS rename can never
    /// let an older snapshot land after a newer one (see [`Self::put`]).
    writer: Arc<Mutex<u64>>,
    /// Monotonic per-put stamp; the writer records the last one it flushed.
    seq: u64,
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
        let mut store = ViewStateStore {
            path,
            recency: items.keys().cloned().collect(),
            items,
            writer: Arc::new(Mutex::new(0)),
            seq: 0,
        };
        // A store that grew past the cap under an older daemon shrinks once
        // here; the trimmed file lands on the next put.
        store.evict();
        store
    }

    pub(crate) fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.items.get(key).cloned()
    }

    /// Store `value` under `key` and schedule the persist. Serialization
    /// happens here, under the caller's lock, so the bytes are a consistent
    /// snapshot; the disk write runs on the blocking pool (`~/.chimaera` may
    /// be NFS, and this fires on a 500 ms debounce per divider drag, per
    /// window — never on the reactor). Returns the write task so a handler
    /// can await the outcome.
    pub(crate) fn put(
        &mut self,
        key: String,
        value: serde_json::Value,
    ) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        if self.items.insert(key.clone(), value).is_some() {
            self.recency.retain(|k| k != &key);
        }
        self.recency.push_back(key);
        self.evict();
        self.seq += 1;
        let seq = self.seq;
        let path = self.path.clone();
        let writer = self.writer.clone();
        let bytes = serde_json::to_vec(&self.items);
        tokio::task::spawn_blocking(move || {
            let bytes = bytes?;
            // Writers queue on this lock; a snapshot older than the last one
            // flushed is dropped rather than rolling the file back.
            let mut last = writer.lock().unwrap_or_else(|p| p.into_inner());
            if seq <= *last {
                return Ok(());
            }
            crate::persist::atomic_write_json(&path, bytes)?;
            *last = seq;
            Ok(())
        })
    }

    /// Drop least-recently-written keys past [`MAX_KEYS`].
    fn evict(&mut self) {
        while self.items.len() > MAX_KEYS {
            let Some(oldest) = self.recency.pop_front() else {
                break;
            };
            self.items.remove(&oldest);
        }
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
    // Bound to its own statement: the guard drops before the write is awaited.
    let write = crate::lock(&state.view_state).put(key, value);
    match write.await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(err)) => {
            tracing::error!(%err, "failed to persist view state");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("view-state write failed: {join}")})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(label: &str) -> ViewStateStore {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-view-state-{label}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        ViewStateStore::load(dir.join("view-state.json"))
    }

    /// The cap evicts least-recently-WRITTEN keys, a re-PUT counts as fresh,
    /// and the trimmed set is what reaches disk.
    #[tokio::test]
    async fn put_caps_keys_by_write_recency_and_persists_the_trimmed_set() {
        let mut store = temp_store("cap");
        for i in 0..MAX_KEYS {
            drop(store.put(format!("w-{i}"), json!(i)));
        }
        // Refresh the oldest key, then overflow by two: the two evicted keys
        // are the oldest UNREFRESHED ones.
        drop(store.put("w-0".to_string(), json!("kept")));
        drop(store.put("late-0".to_string(), json!(0)));
        let last = store.put("late-1".to_string(), json!(1));
        last.await.unwrap().unwrap();
        assert_eq!(store.items.len(), MAX_KEYS);
        assert_eq!(store.get("w-0"), Some(json!("kept")));
        assert_eq!(store.get("w-1"), None);
        assert_eq!(store.get("w-2"), None);
        assert_eq!(store.get("w-3"), Some(json!(3)));
        assert_eq!(store.get("late-1"), Some(json!(1)));

        let reloaded = ViewStateStore::load(store.path.clone());
        assert_eq!(reloaded.items.len(), MAX_KEYS);
        assert_eq!(reloaded.get("w-0"), Some(json!("kept")));
        assert_eq!(reloaded.get("w-1"), None);
        std::fs::remove_dir_all(store.path.parent().unwrap()).ok();
    }

    /// An over-cap file from an older daemon shrinks on load.
    #[tokio::test]
    async fn load_trims_an_oversized_store() {
        let mut store = temp_store("trim");
        for i in 0..(MAX_KEYS + 10) {
            drop(store.put(format!("k-{i}"), json!(i)));
        }
        store
            .put("k-last".to_string(), json!("last"))
            .await
            .unwrap()
            .unwrap();
        // Forge a file past the cap (as an older daemon would have left).
        let mut items = store.items.clone();
        for i in 0..20 {
            items.insert(format!("old-{i}"), json!(i));
        }
        crate::persist::atomic_write_json(&store.path, serde_json::to_vec(&items).unwrap())
            .unwrap();
        let reloaded = ViewStateStore::load(store.path.clone());
        assert_eq!(reloaded.items.len(), MAX_KEYS);
        std::fs::remove_dir_all(store.path.parent().unwrap()).ok();
    }
}
