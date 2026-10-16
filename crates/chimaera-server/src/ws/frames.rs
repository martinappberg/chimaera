//! Every daemon→UI frame of the three WebSocket feeds, built here and
//! nowhere else. The handlers in `ws.rs` and the bus writers (`notices`,
//! `browser_open`, `plugins::*`) call these functions, and the wire-fixture
//! tests serialize the same functions into
//! `web-ui/src/lib/net/fixtures/wire/*.json` — so a fixture can only ever
//! show what the daemon really sends, and the web UI's consumer tests
//! (`web-ui/src/lib/net/wireFixtures.test.ts`) feed exactly that through the
//! client decoders. A change here is a daemon↔UI contract change (root
//! `AGENTS.md`): it rewrites a fixture, the diff shows in review, and the UI
//! tests say whether the client still accepts it.
//!
//! Shapes are deliberately plain: a `type` tag, snake_case keys, optional
//! fields absent rather than null unless the frame already carried `null`.
//! Clients ignore types and keys they don't know (additive evolution).

use std::sync::Arc;
use std::time::Instant;

use chimaera_agent::journal::SeqEvent;
use serde::Serialize;
use serde_json::{json, Value};

/// A rejection with no code (bad auth, server failure): every client treats
/// it as fatal and stops reconnecting.
pub(crate) fn error(message: &str) -> Value {
    json!({"type": "error", "message": message})
}

/// A coded error. `unknown_session` is retryable (mid view-switch the id
/// exists but its process is being respawned); `invalid_command` and
/// `command_failed` refuse one chat command and leave the socket healthy.
pub(crate) fn error_coded(code: &str, message: &str) -> Value {
    json!({"type": "error", "code": code, "message": message})
}

/// `/ws/sessions` and `/ws/chat`: the process ended. `None` = no status is
/// known (the session is simply gone).
pub(crate) fn exited(status: Option<i32>) -> Value {
    json!({"type": "exited", "status": status})
}

// ---- /ws/sessions/{id} ---------------------------------------------------

/// Attach accepted: `{"type":"ready", ...SessionInfo fields..., "cwd_current"}`.
/// `cwd_current` is the same field as the REST/events session JSON: the
/// polled cwd (shell naming watcher), falling back to the spawn cwd. The
/// snapshot binary frame follows (unless the client attached parked).
pub(crate) fn pty_ready(info: &chimaera_pty::SessionInfo, cwd_current: &std::path::Path) -> Value {
    let mut ready = match serde_json::to_value(info) {
        Ok(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    ready.insert("type".to_string(), json!("ready"));
    ready.insert("cwd_current".to_string(), json!(cwd_current));
    Value::Object(ready)
}

/// A repaint: the client resizes to this grid, resets, and the NEXT binary
/// frame is the complete snapshot rendered at it.
pub(crate) fn pty_resync(cols: u16, rows: u16) -> Value {
    json!({"type": "resync", "cols": cols, "rows": rows})
}

/// A session's out-of-band event, verbatim (`title`, `resized`, `exited`,
/// `shell`): chimaera-pty's `SessionEvent` IS the frame.
pub(crate) fn pty_event(event: &chimaera_pty::SessionEvent) -> Result<Value, serde_json::Error> {
    serde_json::to_value(event)
}

// ---- /ws/chat/{id} -------------------------------------------------------

/// Attach accepted. `replay_from` is the cursor the daemon honored (it can
/// differ from the client's `last_seq` after a journal reset); `head` the
/// journal's highest seq now — a client whose own cursor exceeds it is stale
/// and hard-resets instead of silently dropping every replayed event.
pub(crate) fn chat_ready(info: &chimaera_agent::ChatInfo, replay_from: u64, head: u64) -> Value {
    json!({
        "type": "ready",
        "session": info,
        "replay_from": replay_from,
        "head": head,
    })
}

/// One live journal entry.
pub(crate) fn chat_event(entry: &SeqEvent) -> Value {
    json!({"type": "ev", "seq": entry.seq, "ts": entry.ts, "ev": entry.ev})
}

/// A bounded slice of journal replay, in order.
pub(crate) fn chat_batch(entries: &[Arc<SeqEvent>]) -> Value {
    let events: Vec<Value> = entries
        .iter()
        .map(|e| json!({"seq": e.seq, "ts": e.ts, "ev": e.ev}))
        .collect();
    json!({"type": "batch", "events": events})
}

/// The session degraded (or toggled) to a terminal under the same id.
pub(crate) fn chat_degraded() -> Value {
    json!({"type": "degraded"})
}

/// A Claude Mod UI event for this client (`chimaera_agent::native_ui`).
pub(crate) fn native_ui(event: &impl Serialize) -> Value {
    json!({"type": "native_ui", "event": event})
}

/// The Mod UI event stream lagged: the client drops its request state.
pub(crate) fn native_ui_reset() -> Value {
    json!({"type": "native_ui_reset"})
}

// ---- /ws/events ----------------------------------------------------------

/// The full session roster (PTY rows + chat rows, `session_view`) and the
/// agent↔terminal links. Re-sent whole on every change; no diffs.
pub(crate) fn sessions(sessions: Vec<Value>, links: Vec<Value>) -> Value {
    json!({"type": "sessions", "sessions": sessions, "links": links})
}

/// The full settings map (settings.json ground truth).
pub(crate) fn settings(map: &impl Serialize) -> Value {
    json!({"type": "settings", "settings": map})
}

/// Git invalidation: per-workspace epochs and, additively, per-repository
/// epochs (workspace → top level → epoch). The status itself never rides
/// the bus; the client refetches.
pub(crate) fn git(epochs: &impl Serialize, repos: &impl Serialize) -> Value {
    json!({"type": "git", "epochs": epochs, "repos": repos})
}

/// Timeline invalidation: workspace id → epoch.
pub(crate) fn timeline(epochs: &impl Serialize) -> Value {
    json!({"type": "timeline", "epochs": epochs})
}

/// Agent-communication invalidation: workspace id → epoch.
pub(crate) fn comms(epochs: &impl Serialize) -> Value {
    json!({"type": "comms", "epochs": epochs})
}

/// Recents invalidation (daemon-wide epoch).
pub(crate) fn recents(epoch: u64) -> Value {
    json!({"type": "recents", "epoch": epoch})
}

/// Agent-plugin (install/trust) invalidation (daemon-wide epoch).
pub(crate) fn agent_plugins(epoch: u64) -> Value {
    json!({"type": "agent_plugins", "epoch": epoch})
}

/// Path-only filesystem invalidation for this window's mounted paths.
pub(crate) fn fs(changes: &crate::fs_watch::FsChanges) -> Value {
    json!({
        "type": "fs",
        "files": changes.files,
        "removed": changes.removed,
        "dirs": changes.dirs,
        "removed_dirs": changes.removed_dirs,
    })
}

/// Release knowledge: `GET /api/v1/update`'s body (`update::UpdateStatus::to_json`)
/// tagged `update`, so the client has one parser for both.
pub(crate) fn update(mut status: Value) -> Value {
    status["type"] = json!("update");
    status
}

/// New notices since the client's mark. `age_ms` is measured on this
/// daemon's clock (`now`) so a remote window can judge freshness.
pub(crate) fn notices(notices: &[Arc<crate::notices::Notice>], now: Instant) -> Value {
    json!({
        "type": "notices",
        "notices": notices.iter().map(|n| n.to_json(now)).collect::<Vec<_>>(),
    })
}

/// An agent asked for a browser pane on `host:port/path`.
pub(crate) fn browser_open(
    session_id: &str,
    workspace_id: Option<&str>,
    host: &impl Serialize,
    port: &impl Serialize,
    path: &impl Serialize,
) -> Value {
    json!({
        "type": "browser_open",
        "session_id": session_id,
        "workspace_id": workspace_id,
        "host": host,
        "port": port,
        "path": path,
    })
}

/// A plugin screen changed: windows showing it render it again.
pub(crate) fn plugin_view(plugin: &str, workspace: &str, view: &str) -> Value {
    json!({"type": "view", "plugin": plugin, "workspace": workspace, "view": view})
}

/// A plugin (un)published a data surface key.
pub(crate) fn plugin_surface(
    plugin: &impl Serialize,
    workspace: &impl Serialize,
    surface: &impl Serialize,
    key: &impl Serialize,
) -> Value {
    json!({
        "type": "surface",
        "plugin": plugin,
        "workspace": workspace,
        "surface": surface,
        "key": key,
    })
}

/// A plugin's own `emit` event. The host's keys land last, so an event can't
/// pose as another frame.
pub(crate) fn plugin_emit(
    fields: serde_json::Map<String, Value>,
    plugin: &str,
    workspace: &str,
) -> Value {
    let mut frame = Value::Object(fields);
    frame["type"] = json!("plugin");
    frame["plugin"] = json!(plugin);
    frame["workspace"] = json!(workspace);
    frame
}

/// A plugin job's status (`plugins::jobs::Job::json`) tagged for the bus.
pub(crate) fn plugin_job(mut job: Value, plugin: &str, workspace: &str) -> Value {
    job["type"] = json!("job");
    job["plugin"] = json!(plugin);
    job["workspace"] = json!(workspace);
    job
}

/// Sample values the wire-fixture tests share (`ws.rs`, `session_view.rs`):
/// every field set to something a client would see, with stable timestamps
/// so the fixtures are byte-stable across runs.
#[cfg(test)]
pub(crate) mod samples {
    use chimaera_agent::{ChatInfo, GoalBrief, NextWake};

    /// A live shell at its prompt. `last_output_at` stays off the wire.
    pub(crate) fn session_info(id: &str, alive: bool) -> chimaera_pty::SessionInfo {
        chimaera_pty::SessionInfo {
            id: id.to_string(),
            name: "proj".to_string(),
            cwd: "/home/u/proj".into(),
            cols: 120,
            rows: 40,
            created_at: 1_700_000_000,
            alive,
            exit_status: if alive { None } else { Some(1) },
            title: Some("vim lib.rs".to_string()),
            pid: alive.then_some(4242),
            renamed: false,
            phase: chimaera_pty::ShellPhase::Ready,
            last_output_at: 1_700_000_000_000,
        }
    }

    /// A chat session; `rich` fills every additive field a running claude
    /// chat can carry (status line, a pending wakeup, a goal, Remote Control).
    pub(crate) fn chat_info(id: &str, rich: bool) -> ChatInfo {
        ChatInfo {
            id: id.to_string(),
            agent: "claude".to_string(),
            cwd: "/home/u/proj".into(),
            created_at_ms: 1_700_000_000_000,
            alive: true,
            exit_status: None,
            native_session_id: Some("3f2c7e1a-0000-4000-8000-000000000001".to_string()),
            model: Some("claude-opus-5-5".to_string()),
            current_mode: Some("default".to_string()),
            pending_permission: rich,
            status_detail: rich.then(|| "PR open, waiting on review".to_string()),
            status_category: rich.then(|| "review_ready".to_string()),
            status_needs_action: rich,
            remote_control_url: rich.then(|| "https://claude.ai/code/session_01X".to_string()),
            background_running: if rich { 2 } else { 0 },
            next_wake: rich.then(|| NextWake {
                at_ms: Some(1_700_000_600_000),
                from_ms: Some(1_700_000_000_000),
                label: "Check the deploy".to_string(),
                count: 2,
            }),
            goal: rich.then(|| GoalBrief {
                objective: "all tests pass".to_string(),
                status: "active".to_string(),
            }),
        }
    }
}
