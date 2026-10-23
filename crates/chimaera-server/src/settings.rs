//! User settings: one flat JSON object of dotted keys ("terminal.fontSize")
//! persisted at `~/.config/chimaera/settings.json` — the ground truth every
//! surface reads. The file stores only explicitly-set values; defaults live
//! in the web-ui schema (web-ui/src/lib/settings/schema.ts). Values are
//! opaque to the server except for the few daemon-consumed keys below.
//!
//! Hand-edits are first-class, read stale-while-revalidate: hand-edits reach
//! the cache within [`SETTINGS_POLL`] (the off-reactor poll,
//! [`watch_external_edits`]); routes refresh on read; PUT installs at once.
//! Every daemon-side getter (`scrollback_lines`, `git_path`, …) reads only
//! the cache — no stat or read under the store mutex, so a hung disk never
//! reaches a spawn or the reactor. /ws/events broadcasts a fresh
//! `{"type":"settings"}` frame whenever the content generation moves (PUT or
//! on-disk edit); a new window's first frame refreshes first.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::persist::{DiskGate, Encode, Hook, Look};
use crate::AppState;

/// Maximum stored settings size (the raw PUT body / on-disk file).
const MAX_SETTINGS_BYTES: usize = 256 * 1024;

type Map = serde_json::Map<String, serde_json::Value>;

/// In-memory settings map backed by `settings.json`; external edits (vim
/// over SSH) surface without a daemon restart through [`refresh`].
///
/// All disk work after boot goes through [`refresh`] and [`put_settings`]: on
/// the blocking pool, one job at a time (`gate`), with this struct's mutex
/// held only to snapshot and to install — never across a stat, read or
/// write. The getters read the cached map only.
pub(crate) struct SettingsStore {
    path: PathBuf,
    map: Map,
    /// mtime of the file the cached map was read from (None = no file).
    mtime: Option<SystemTime>,
    /// Bumped on every observed content change; /ws/events diffs against it.
    generation: u64,
    gate: DiskGate,
    /// Test hooks run on the blocking pool (see [`crate::persist::Hook`]):
    /// before each off-lock read, and after a PUT's file lands.
    before_read: Option<Hook>,
    after_write: Option<Hook>,
}

/// One look at settings.json (environment.rs's store reads the same way).
type Disk = Look<Map>;

/// [`crate::persist::look`] for settings.json: a non-object or corrupt file
/// is an empty map.
fn look(path: &Path, recorded: Option<SystemTime>, hook: Option<&Hook>) -> Disk {
    crate::persist::look(
        path,
        recorded,
        MAX_SETTINGS_BYTES,
        "settings.json",
        hook,
        |bytes| match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(serde_json::Value::Object(map)) => map,
            Ok(_) => {
                tracing::warn!(path = %path.display(), "settings.json is not a JSON object; ignoring");
                Map::new()
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "corrupt settings.json; ignoring");
                Map::new()
            }
        },
    )
}

impl SettingsStore {
    /// Load the store from `path`. Missing, oversized, or corrupt files yield
    /// an empty map (with a warning for the corrupt case) — settings must
    /// never brick the daemon.
    pub(crate) fn load(path: PathBuf) -> Self {
        let mut store = SettingsStore {
            path,
            map: Map::new(),
            mtime: None,
            generation: 0,
            gate: DiskGate::default(),
            before_read: None,
            after_write: None,
        };
        store.refresh();
        store
    }

    /// Fold one look in; true when the content generation moved.
    fn install(&mut self, disk: Disk) -> bool {
        let Disk::Loaded(map, mtime) = disk else {
            return false;
        };
        self.mtime = mtime;
        if map == self.map {
            return false;
        }
        self.generation += 1;
        self.map = map;
        true
    }

    /// Re-read when the file changed on disk since the cached read. Blocking:
    /// only [`Self::load`], before the store is shared.
    fn refresh(&mut self) {
        let disk = look(&self.path, self.mtime, None);
        self.install(disk);
    }

    /// The cached map, never a disk re-stat — safe on the reactor and under
    /// any lock. External edits are folded in by [`watch_external_edits`]
    /// (off-reactor); a caller that must see a hand-edit made in the last
    /// [`SETTINGS_POLL`] awaits [`refresh`] first.
    pub(crate) fn map_cached(&self) -> &Map {
        &self.map
    }

    /// The cached content generation (same contract as [`Self::map_cached`]).
    pub(crate) fn generation_cached(&self) -> u64 {
        self.generation
    }

    /// Replace the whole map and persist (pretty-printed, atomic rename).
    /// Blocking, under the caller's lock: the route goes through
    /// [`put_settings`] instead.
    #[cfg(test)]
    pub(crate) fn put(&mut self, map: Map) -> anyhow::Result<()> {
        let body = encode(&map).map_err(|err| match err {
            WriteError::TooLarge => anyhow::anyhow!("settings exceed {MAX_SETTINGS_BYTES} bytes"),
            WriteError::Failed(err) => err,
        })?;
        let mtime = crate::persist::atomic_write_json_modified(&self.path, body)?;
        self.install(Disk::Loaded(map, mtime));
        Ok(())
    }

    /// Daemon-consumed key: scrollback lines for newly spawned sessions.
    pub(crate) fn scrollback_lines(&self) -> Option<usize> {
        let v = self.map.get("daemon.scrollbackLines")?.as_u64()?;
        // Keep hand-edited / agent-edited settings on the same resource
        // boundary the settings UI advertises. The previous 1,000,000-line
        // server ceiling silently accepted 5x the UI maximum per session.
        Some(v.clamp(1_000, chimaera_pty::MAX_SCROLLBACK_LINES as u64) as usize)
    }

    /// Daemon-consumed key: resurrect sessions from the ledger when the
    /// daemon restarts (see `ledger`).
    pub(crate) fn restore_sessions(&self) -> bool {
        self.map
            .get("daemon.restoreSessions")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// Daemon-consumed key: turn Claude's Remote Control on for every new
    /// structured chat session right after its handshake (see
    /// `chat::spawn_chat_session`). Default off: enabling registers the
    /// session with claude.ai, which is the user's call, not the daemon's.
    pub(crate) fn remote_control_at_start(&self) -> bool {
        self.map
            .get("chat.remoteControlAtStart")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Native defaults opt out only of cross-conversation model and effort
    /// preferences. Explicit picks, resumed settings and permission modes
    /// keep their separate precedence in `chat::resolve_start_settings`.
    pub(crate) fn remember_chat_model(&self) -> bool {
        self.map
            .get("chat.newSessionModel")
            .and_then(|v| v.as_str())
            != Some("agent")
    }

    /// A launch-only override; absent/unknown preserves the native Codex
    /// configuration, including a user who already opted into instant input.
    pub(crate) fn codex_instant_interrupt(&self) -> Option<bool> {
        match self.map.get("chat.codexSteering").and_then(|v| v.as_str()) {
            Some("immediate") => Some(true),
            Some("next_step") => Some(false),
            _ => None,
        }
    }

    /// Daemon-consumed key: what a new Claude session's `--settings` file
    /// says about auto-compact (PROTOCOL.md Pass 50). On (the default) writes
    /// nothing, so the user's own Claude Code settings stay in charge (their
    /// default is on); Off writes `autoCompactEnabled: false` into the
    /// per-session file, which outranks user and project settings and never
    /// touches them. Codex has no switch.
    pub(crate) fn claude_auto_compact(&self) -> Option<bool> {
        let on = self
            .map
            .get("agents.autoCompact")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        (!on).then_some(false)
    }

    /// Daemon-consumed key: when a restart cut a chat's work off (a running
    /// turn, background commands / monitors / workflows), send the resumed
    /// agent one message saying what stopped (see `chat::resurrect_chat`).
    /// Default on: it only fires when there is work to pick up, and that is
    /// the point of the restart surviving at all; each one starts a turn.
    pub(crate) fn resume_after_restart(&self) -> bool {
        self.map
            .get("chat.resumeAfterRestart")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// Daemon-consumed key: ask Claude for its per-batch tool labels (the
    /// transcript's tool-group titles). Default on: the labels are what the
    /// official clients show; each costs one small-model call.
    pub(crate) fn tool_summaries(&self) -> bool {
        self.map
            .get("chat.toolSummaries")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// Daemon-consumed key: periodically check GitHub for newer releases
    /// (see `update`).
    pub(crate) fn update_auto_check(&self) -> bool {
        self.map
            .get("update.autoCheck")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// Daemon-consumed key: an explicit path to the `git` binary. `None` (unset
    /// or blank) means "resolve git from the login shell, then PATH". Set on HPC
    /// login nodes whose stock `/usr/bin/git` is too old for the git service.
    pub(crate) fn git_path(&self) -> Option<String> {
        let s = self.map.get("git.path")?.as_str()?.trim();
        (!s.is_empty()).then(|| s.to_string())
    }

    /// Daemon-consumed key: an explicit path to an agent's binary
    /// (`agents.<kind>.path`). `None` (unset or blank) means "resolve from the
    /// login shell, then a chimaera-managed install". The same escape hatch as
    /// [`git_path`], per agent — for hosts where the agent lives somewhere the
    /// login shell doesn't surface.
    pub(crate) fn agent_path(&self, kind: crate::agents::AgentKind) -> Option<String> {
        let key = format!("agents.{}.path", kind.as_str());
        let s = self.map.get(&key)?.as_str()?.trim();
        (!s.is_empty()).then(|| s.to_string())
    }

    /// Daemon-consumed key: directory names quick-open skips while walking.
    /// None = the built-in default list.
    pub(crate) fn quickopen_ignore_dirs(&self) -> Option<Vec<String>> {
        let list = self.map.get("quickOpen.ignoreDirs")?.as_array()?;
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

    /// Run `hook` on the blocking pool right before each off-lock read (see
    /// [`crate::persist::Hook`]).
    #[cfg(test)]
    pub(crate) fn set_before_read_for_tests(&mut self, hook: Hook) {
        self.before_read = Some(hook);
    }

    /// Run `hook` on the blocking pool right after a PUT's file lands, before
    /// the store installs it.
    #[cfg(test)]
    pub(crate) fn set_after_write_for_tests(&mut self, hook: Hook) {
        self.after_write = Some(hook);
    }
}

/// Why a settings write didn't land.
enum WriteError {
    /// The encoded file would exceed what a read accepts: writing it would
    /// turn the next read into an empty map.
    TooLarge,
    Failed(anyhow::Error),
}

/// The pretty-printed file body, refused over the read cap.
fn encode(map: &Map) -> Result<Vec<u8>, WriteError> {
    crate::persist::encode_pretty(map, MAX_SETTINGS_BYTES).map_err(|err| match err {
        Encode::TooLarge => WriteError::TooLarge,
        Encode::Failed(err) => WriteError::Failed(err.into()),
    })
}

/// One off-lock refresh, side effects included. Blocking; runs inside the
/// store's [`DiskGate`]. The mutex is taken twice, briefly: to snapshot the
/// path and recorded mtime, and to install. Every install runs inside the
/// same gate, so the mtime re-check before installing is a guard, not a
/// race anyone expects to lose. True when the content changed.
///
/// The quick-open clear and the `/ws/events` wake happen HERE, not in the
/// awaiting handler: a handler dropped mid-job (a closed window) must not
/// lose them once the change is installed.
fn refresh_job(state: &AppState) -> bool {
    let (path, recorded, hook) = {
        let store = crate::lock(&state.settings);
        (store.path.clone(), store.mtime, store.before_read.clone())
    };
    let disk = look(&path, recorded, hook.as_ref());
    if !matches!(disk, Disk::Loaded(..)) {
        return false;
    }
    let (changed, ignore_changed) = {
        let mut store = crate::lock(&state.settings);
        if store.mtime != recorded {
            return false;
        }
        let ignore_before = store.map.get("quickOpen.ignoreDirs").cloned();
        let changed = store.install(disk);
        (
            changed,
            ignore_before != store.map.get("quickOpen.ignoreDirs").cloned(),
        )
    };
    if ignore_changed {
        // Same rule as the PUT path: an index built under the old ignore
        // list must not be served for its freshness window.
        crate::lock(&state.quickopen).clear();
    }
    if changed {
        state.changes.notify_waiters();
    }
    changed
}

/// Fold an on-disk edit into the store, off the reactor, and wake
/// `/ws/events` when the content changed; true when it did.
pub(crate) async fn refresh(state: &Arc<AppState>) -> bool {
    let gate = crate::lock(&state.settings).gate.clone();
    let job_state = state.clone();
    gate.run(move || refresh_job(&job_state))
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "settings refresh failed");
            false
        })
}

/// Cadence of the external-edit poll — the staleness bound of every cached
/// read of settings.json and env-profiles.json. Hand-edits are rare and a
/// couple of seconds' surfacing latency changes nothing a user can see, while
/// each pass costs one stat per file, off the reactor, instead of one per
/// reader.
pub(crate) const SETTINGS_POLL: Duration = Duration::from_secs(2);

/// Fold hand-edits of settings.json AND env-profiles.json into their caches
/// without any reactor stat: one task, one sleep, each store's look through
/// its own [`DiskGate`]. The two looks of a pass run side by side, but the
/// loop awaits both: while one file's read hangs, no later pass runs for
/// either, and the other store's hand-edits wait for its route (GET refreshes
/// on read) or the hang to clear. Accepted: both files sit in the same config
/// directory, so a hang on one rarely spares the other. The getters and spawn
/// paths read only the caches — the old stat-under-the-mutex ran ON the
/// reactor, where one NFS hiccup stalled everything — so this task owns
/// steady-state external-edit detection. The loop has no exit path; a failed
/// pass is logged inside each refresh.
pub(crate) async fn watch_external_edits(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(SETTINGS_POLL).await;
        poll_external_edit(&state).await;
    }
}

/// One poll pass over both stores: true when either on-disk file changed its
/// store's content (a settings change is already broadcast by [`refresh`]).
/// A failed stat — an NFS hiccup, not the file being absent, which is a
/// legitimate state — is no change.
pub(crate) async fn poll_external_edit(state: &Arc<AppState>) -> bool {
    let (settings, environment) = tokio::join!(refresh(state), crate::environment::refresh(state));
    settings || environment
}

/// GET /api/v1/settings
pub(crate) async fn get_settings(State(state): State<Arc<AppState>>) -> Response {
    refresh(&state).await;
    let map = crate::lock(&state.settings).map.clone();
    Json(json!({"settings": map})).into_response()
}

/// The PUT's disk work and everything that follows from it. Blocking; runs
/// inside the store's [`DiskGate`], so it is ordered against every other
/// refresh and write of this store, and it runs to the end even when the
/// handler awaiting it is dropped (a superseded save aborted on page hide):
/// the shim rebuild, the quick-open clear and the wake can't be lost once
/// the file is written. A hand-edit is folded in first so the diff is
/// against what was really on disk, and `before` is taken BEFORE the write,
/// from the cache only this gate installs into.
fn put_job(state: &AppState, map: Map) -> Result<(), WriteError> {
    refresh_job(state);
    let body = encode(&map)?;
    let (path, before, ignore_before, after_write) = {
        let store = crate::lock(&state.settings);
        (
            store.path.clone(),
            agent_path_snapshot(&store.map),
            store.map.get("quickOpen.ignoreDirs").cloned(),
            store.after_write.clone(),
        )
    };
    let mtime =
        crate::persist::atomic_write_json_modified(&path, body).map_err(WriteError::Failed)?;
    crate::persist::run_hook(after_write.as_ref());
    let agent_paths_changed = before != agent_path_snapshot(&map);
    let ignore_dirs_changed = ignore_before.as_ref() != map.get("quickOpen.ignoreDirs");
    crate::lock(&state.settings).install(Disk::Loaded(map, mtime));
    // The settings mutex is released: regenerate_shims re-locks it.
    if agent_paths_changed {
        // A per-agent binary override moved: rebuild the shims (add/remove/point
        // one) and drop the detection cache so the next spawn resolves anew.
        crate::runtimes::regenerate_shims(state);
        crate::lock(&state.agent_bins).clear();
        crate::launcher::forget_misses(state);
    }
    if ignore_dirs_changed {
        // The quick-open index is served stale for its whole freshness window;
        // an index built under the old ignore list must not outlive the edit.
        crate::lock(&state.quickopen).clear();
    }
    // Wake /ws/events subscribers so every window converges live.
    state.changes.notify_waiters();
    Ok(())
}

fn too_large() -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(json!({"error": format!("settings exceed {MAX_SETTINGS_BYTES} bytes")})),
    )
        .into_response()
}

/// PUT /api/v1/settings — replace the whole map (a JSON object ≤ 256KB);
/// 204 on success. Unknown keys are preserved verbatim (forward compat).
pub(crate) async fn put_settings(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    if body.len() > MAX_SETTINGS_BYTES {
        return too_large();
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
    let gate = crate::lock(&state.settings).gate.clone();
    let job_state = state.clone();
    match gate.run(move || put_job(&job_state, map)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(WriteError::TooLarge)) => too_large(),
        Ok(Err(WriteError::Failed(err))) | Err(err) => {
            tracing::error!(%err, "failed to persist settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}

/// The `agents.<kind>.path` entries of a settings map, for diffing a PUT: only
/// these keys drive shim regeneration.
fn agent_path_snapshot(map: &Map) -> std::collections::BTreeMap<String, String> {
    map.iter()
        .filter(|(k, _)| k.starts_with("agents.") && k.ends_with(".path"))
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}
