//! User settings: one flat JSON object of dotted keys ("terminal.fontSize")
//! persisted at `~/.config/chimaera/settings.json` — the ground truth every
//! surface reads. The file stores only explicitly-set values; defaults live
//! in the web-ui schema (web-ui/src/lib/settings/schema.ts). Values are
//! opaque to the server except for the few daemon-consumed keys below.
//!
//! Hand-edits are first-class: reads re-stat the file and pick up external
//! changes, and /ws/events broadcasts a fresh `{"type":"settings"}` frame
//! whenever the content generation moves (PUT or on-disk edit).

use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Context;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::AppState;

/// Maximum stored settings size (the raw PUT body / on-disk file).
const MAX_SETTINGS_BYTES: usize = 256 * 1024;

/// In-memory settings map backed by `settings.json`, mtime-checked on read
/// so external edits (vim over SSH) surface without a daemon restart.
pub(crate) struct SettingsStore {
    path: PathBuf,
    map: serde_json::Map<String, serde_json::Value>,
    /// mtime of the file the cached map was read from (None = no file).
    mtime: Option<SystemTime>,
    /// Bumped on every observed content change; /ws/events diffs against it.
    generation: u64,
}

impl SettingsStore {
    /// Load the store from `path`. Missing, oversized, or corrupt files yield
    /// an empty map (with a warning for the corrupt case) — settings must
    /// never brick the daemon.
    pub(crate) fn load(path: PathBuf) -> Self {
        let mut store = SettingsStore {
            path,
            map: serde_json::Map::new(),
            mtime: None,
            generation: 0,
        };
        store.read_from_disk();
        store
    }

    fn read_from_disk(&mut self) {
        let (map, mtime) = match std::fs::read(&self.path) {
            Ok(bytes) if bytes.len() > MAX_SETTINGS_BYTES => {
                tracing::warn!(path = %self.path.display(), "settings.json exceeds {MAX_SETTINGS_BYTES} bytes; ignoring");
                (serde_json::Map::new(), file_mtime(&self.path))
            }
            Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(serde_json::Value::Object(map)) => (map, file_mtime(&self.path)),
                Ok(_) => {
                    tracing::warn!(path = %self.path.display(), "settings.json is not a JSON object; ignoring");
                    (serde_json::Map::new(), file_mtime(&self.path))
                }
                Err(err) => {
                    tracing::warn!(path = %self.path.display(), %err, "corrupt settings.json; ignoring");
                    (serde_json::Map::new(), file_mtime(&self.path))
                }
            },
            Err(err) if err.kind() == ErrorKind::NotFound => (serde_json::Map::new(), None),
            Err(err) => {
                tracing::warn!(path = %self.path.display(), %err, "failed to read settings.json");
                (serde_json::Map::new(), None)
            }
        };
        if map != self.map {
            self.generation += 1;
        }
        self.map = map;
        self.mtime = mtime;
    }

    /// Re-read when the file changed on disk since the cached read.
    fn refresh(&mut self) {
        if file_mtime(&self.path) != self.mtime {
            self.read_from_disk();
        }
    }

    /// The current settings map (mtime-checked against on-disk edits).
    pub(crate) fn current(&mut self) -> &serde_json::Map<String, serde_json::Value> {
        self.refresh();
        &self.map
    }

    /// The current content generation (mtime-checked).
    pub(crate) fn generation(&mut self) -> u64 {
        self.refresh();
        self.generation
    }

    /// Replace the whole map and persist (pretty-printed, atomic rename).
    pub(crate) fn put(
        &mut self,
        map: serde_json::Map<String, serde_json::Value>,
    ) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let mut body = serde_json::to_vec_pretty(&map)?;
        body.push(b'\n');
        std::fs::write(&tmp, body).with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to rename into {}", self.path.display()))?;
        if map != self.map {
            self.generation += 1;
        }
        self.map = map;
        self.mtime = file_mtime(&self.path);
        Ok(())
    }

    /// Daemon-consumed key: scrollback lines for newly spawned sessions.
    pub(crate) fn scrollback_lines(&mut self) -> Option<usize> {
        let v = self.current().get("daemon.scrollbackLines")?.as_u64()?;
        Some(v.clamp(200, 1_000_000) as usize)
    }

    /// Daemon-consumed key: resurrect sessions from the ledger when the
    /// daemon restarts (see `ledger`).
    pub(crate) fn restore_sessions(&mut self) -> bool {
        self.current()
            .get("daemon.restoreSessions")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// Daemon-consumed key: periodically check GitHub for newer releases
    /// (see `update`).
    pub(crate) fn update_auto_check(&mut self) -> bool {
        self.current()
            .get("update.autoCheck")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// Daemon-consumed key: an explicit path to the `git` binary. `None` (unset
    /// or blank) means "resolve git from the login shell, then PATH". Set on HPC
    /// login nodes whose stock `/usr/bin/git` is too old for the git service.
    pub(crate) fn git_path(&mut self) -> Option<String> {
        let s = self.current().get("git.path")?.as_str()?.trim();
        (!s.is_empty()).then(|| s.to_string())
    }

    /// Daemon-consumed key: directory names quick-open skips while walking.
    /// None = the built-in default list.
    pub(crate) fn quickopen_ignore_dirs(&mut self) -> Option<Vec<String>> {
        let list = self.current().get("quickOpen.ignoreDirs")?.as_array()?;
        Some(
            list.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty() && !s.contains('/'))
                .map(str::to_owned)
                .collect(),
        )
    }
}

impl SettingsStore {
    /// Invalidate the mtime cache so the next read hits the disk. Tests
    /// rewrite the file within one mtime granule; real edits never do.
    #[cfg(test)]
    pub(crate) fn force_stale_for_tests(&mut self) {
        self.mtime = Some(std::time::UNIX_EPOCH);
    }
}

fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// GET /api/v1/settings
pub(crate) async fn get_settings(State(state): State<Arc<AppState>>) -> Response {
    let map = crate::lock(&state.settings).current().clone();
    Json(json!({"settings": map})).into_response()
}

/// PUT /api/v1/settings — replace the whole map (a JSON object ≤ 256KB);
/// 204 on success. Unknown keys are preserved verbatim (forward compat).
pub(crate) async fn put_settings(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    if body.len() > MAX_SETTINGS_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": format!("settings exceed {MAX_SETTINGS_BYTES} bytes")})),
        )
            .into_response();
    }
    let map = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(serde_json::Value::Object(map)) => map,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "settings must be a JSON object"})),
            )
                .into_response();
        }
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("invalid JSON: {err}")})),
            )
                .into_response();
        }
    };
    match crate::lock(&state.settings).put(map) {
        Ok(()) => {
            // Wake /ws/events subscribers so every window converges live.
            state.changes.notify_waiters();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => {
            tracing::error!(%err, "failed to persist settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}
