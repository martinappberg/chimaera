//! User settings: one flat JSON object of dotted keys ("terminal.fontSize")
//! persisted at `~/.config/chimaera/settings.json` — the ground truth every
//! surface reads. The file stores only explicitly-set values; defaults live
//! in the web-ui schema (web-ui/src/lib/settings/schema.ts). Values are
//! opaque to the server except for the few daemon-consumed keys below.
//!
//! Hand-edits are first-class: REST reads re-stat the file and pick up
//! external changes, a slow off-reactor poll ([`watch_external_edits`])
//! catches edits no read observes, and /ws/events broadcasts a fresh
//! `{"type":"settings"}` frame whenever the content generation moves (PUT or
//! on-disk edit). The events path itself never stats — see
//! [`SettingsStore::generation_cached`].

use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

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

    /// The cached map WITHOUT the disk re-stat — for the `/ws/events` hot
    /// path, which runs per client on every wake and must never do blocking
    /// fs on the reactor. External edits are folded in by
    /// [`watch_external_edits`] (off-reactor), not by this read.
    pub(crate) fn map_cached(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.map
    }

    /// The cached content generation WITHOUT the disk re-stat (same contract
    /// as [`Self::map_cached`]).
    pub(crate) fn generation_cached(&self) -> u64 {
        self.generation
    }

    /// Replace the whole map and persist (pretty-printed, atomic rename).
    pub(crate) fn put(
        &mut self,
        map: serde_json::Map<String, serde_json::Value>,
    ) -> anyhow::Result<()> {
        let mut body = serde_json::to_vec_pretty(&map)?;
        body.push(b'\n');
        crate::persist::atomic_write_json(&self.path, body)?;
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
        // Keep hand-edited / agent-edited settings on the same resource
        // boundary the settings UI advertises. The previous 1,000,000-line
        // server ceiling silently accepted 5x the UI maximum per session.
        Some(v.clamp(1_000, chimaera_pty::MAX_SCROLLBACK_LINES as u64) as usize)
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

    /// Daemon-consumed key: an explicit path to an agent's binary
    /// (`agents.<kind>.path`). `None` (unset or blank) means "resolve from the
    /// login shell, then a chimaera-managed install". The same escape hatch as
    /// [`git_path`], per agent — for hosts where the agent lives somewhere the
    /// login shell doesn't surface.
    pub(crate) fn agent_path(&mut self, kind: crate::agents::AgentKind) -> Option<String> {
        let key = format!("agents.{}.path", kind.as_str());
        let s = self.current().get(&key)?.as_str()?.trim();
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

/// Cadence of the external-edit poll. Hand-edits are rare and were already
/// only broadcast on the next events wake, so a couple of seconds' surfacing
/// latency changes nothing a user can see — while each pass costs exactly one
/// stat, off the reactor, instead of one per client per wake.
const SETTINGS_POLL: Duration = Duration::from_secs(2);

/// Surface hand-edits of settings.json to `/ws/events` without any reactor
/// stat. The events hot path (`ws::send_settings_snapshot`) reads only the
/// cached generation/map — the old stat-on-every-wake ran per client under
/// the settings mutex ON the reactor, where one NFS hiccup stalls everything
/// — so this task owns the steady-state external-edit detection instead.
/// Scope note: REST reads (`current` — GET /settings, the daemon-consumed
/// key getters, `git::configured_git`) still refresh inline, and PUT still
/// writes, on the reactor; those are rare, user-driven paths, accepted
/// as-is. The loop has no exit path; a failed poll pass is logged inside
/// [`poll_external_edit`] rather than silently swallowed.
pub(crate) async fn watch_external_edits(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(SETTINGS_POLL).await;
        if poll_external_edit(&state).await {
            state.changes.notify_waiters();
        }
    }
}

/// One poll pass: true when the on-disk file changed the settings content
/// (the caller broadcasts). Stat and conditional reload run in ONE
/// `spawn_blocking` hop; the settings mutex is held only for the reload, and
/// only when the mtime actually moved — a rare, bounded window (≤256 KiB
/// read), not a stat per events wake. A FAILED stat (an NFS hiccup — not the
/// file being absent, which is a legitimate state) is treated as no-change:
/// reloading through a failing filesystem would broadcast an empty map and
/// flash every window's theme/font back to defaults until the next pass.
pub(crate) async fn poll_external_edit(state: &Arc<AppState>) -> bool {
    let task = {
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            let (path, recorded) = {
                let store = crate::lock(&state.settings);
                (store.path.clone(), store.mtime)
            };
            let on_disk = match std::fs::metadata(&path) {
                Ok(meta) => meta.modified().ok(),
                Err(err) if err.kind() == ErrorKind::NotFound => None,
                Err(err) => {
                    tracing::debug!(path = %path.display(), %err,
                        "settings watcher stat failed; skipping this pass");
                    return false;
                }
            };
            if on_disk == recorded {
                return false;
            }
            let mut store = crate::lock(&state.settings);
            let before = store.generation;
            store.read_from_disk();
            store.generation != before
        })
    };
    match task.await {
        Ok(changed) => changed,
        Err(err) => {
            tracing::warn!(%err, "settings watcher poll task failed");
            false
        }
    }
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
    // Persist, tracking whether any per-agent path override moved so we can
    // rebuild the shims for it. Scoped so the settings lock is released before
    // regenerate_shims (which re-locks settings) — no deadlock.
    let (agent_paths_changed, ignore_dirs_changed) = {
        let mut settings = crate::lock(&state.settings);
        let before = agent_path_snapshot(settings.current());
        let ignore_before = settings.current().get("quickOpen.ignoreDirs").cloned();
        if let Err(err) = settings.put(map) {
            tracing::error!(%err, "failed to persist settings");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
        (
            before != agent_path_snapshot(settings.current()),
            ignore_before != settings.current().get("quickOpen.ignoreDirs").cloned(),
        )
    };
    if agent_paths_changed {
        // A per-agent binary override moved: rebuild the shims (add/remove/point
        // one) and drop the detection cache so the next spawn resolves anew.
        crate::runtimes::regenerate_shims(&state);
    }
    if ignore_dirs_changed {
        // The quick-open index is served stale for its whole freshness window;
        // an index built under the old ignore list must not outlive the edit.
        crate::lock(&state.quickopen).clear();
    }
    // Wake /ws/events subscribers so every window converges live.
    state.changes.notify_waiters();
    StatusCode::NO_CONTENT.into_response()
}

/// The `agents.<kind>.path` entries of a settings map, for diffing a PUT: only
/// these keys drive shim regeneration.
fn agent_path_snapshot(
    map: &serde_json::Map<String, serde_json::Value>,
) -> std::collections::BTreeMap<String, String> {
    map.iter()
        .filter(|(k, _)| k.starts_with("agents.") && k.ends_with(".path"))
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}
