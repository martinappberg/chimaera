use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::agent_state::{AgentKind, AgentState};
use crate::AppState;

/// Quiet window after the last PTY output chunk before a hook-less agent TUI
/// reads as idle. Must sit ABOVE the ~1 Hz repaint cadence of a working TUI
/// (codex/gemini animate a spinner + elapsed counter while a turn runs) so a
/// mid-turn agent never flaps to idle between repaints, and low enough that
/// the dot settles soon after the turn ends. Tuned against a live codex TUI.
const OUTPUT_ACTIVITY_QUIET: Duration = Duration::from_secs(2);

/// Quiet window after which a claude TUI that CLAIMS to be working (hook
/// state Running) reads as stalled instead. A working claude TUI repaints
/// its spinner continuously, so minutes of PTY silence under a Running state
/// contradict the claim — the one dishonest signal the dashboard must
/// surface. Wide enough that a long silent tool never trips it by itself.
const STALL_QUIET: Duration = Duration::from_secs(180);

/// Milliseconds since the Unix epoch (0 if the clock reads before it) —
/// the same clock the PTY reader stamps `last_output_at` with.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Serialize a `SessionInfo` with the extra `workspace_id`, `kind`,
/// `agent_kind`, `agent_state`, `agent_title`, `files_touched`,
/// `display_name` and `cwd_current` fields.
/// `agent` is the wrapper record for kind "agent" sessions; `None` means a
/// plain shell. `polled` / `polled_cwd` are the shell naming watcher's
/// latest values, if any; `cwd_current` falls back to the spawn cwd (agents,
/// never-polled shells). `mastermind` is the additive wire flag: `true` only
/// when this session is its workspace's bound Mastermind (a chat session
/// normally, but a degraded/toggled Mastermind can live as a PTY), `null`
/// otherwise — the UI hides flagged rows from the roster/rail.
pub(crate) fn session_json(
    info: &chimaera_pty::SessionInfo,
    workspace_id: Option<String>,
    agent: Option<&crate::agents::AgentRecord>,
    polled: Option<&str>,
    polled_cwd: Option<&std::path::Path>,
    exec_stage: Option<chimaera_pty::ExecStage>,
    mastermind: bool,
) -> serde_json::Value {
    let mut map = match serde_json::to_value(info) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    map.insert(
        "cwd_current".to_string(),
        json!(polled_cwd.unwrap_or(&info.cwd)),
    );
    map.insert(
        "exec_stage".to_string(),
        exec_stage.map_or(serde_json::Value::Null, |s| json!(s)),
    );
    map.insert(
        "workspace_id".to_string(),
        workspace_id.map_or(serde_json::Value::Null, serde_json::Value::String),
    );
    map.insert(
        "kind".to_string(),
        json!(if agent.is_some() { "agent" } else { "shell" }),
    );
    // Which agent CLI the session runs ("claude"/"codex"/"gemini") so the
    // UI can glyph rows per agent; null for shells.
    map.insert(
        "agent_kind".to_string(),
        agent.map_or(serde_json::Value::Null, |a| json!(a.kind.as_str())),
    );
    map.insert(
        "agent_state".to_string(),
        agent.map_or(serde_json::Value::Null, |a| json!(a.state.as_str())),
    );
    map.insert(
        "agent_title".to_string(),
        agent
            .and_then(|a| a.title())
            .map_or(serde_json::Value::Null, |t| json!(t)),
    );
    map.insert(
        "files_touched".to_string(),
        agent.map_or(serde_json::Value::Null, |a| json!(a.files_touched)),
    );
    // Hook-less agent TUIs (codex/gemini/agy) never populate `agent_state` —
    // it stays "unknown" for the session's whole life. Derive a busy signal
    // from PTY output recency instead: a working TUI streams tokens and
    // animates its spinner continuously; one that has gone quiet is waiting.
    // Emitted only while the state is Unknown (a claude row past its first
    // hook carries real state), and as a boolean that flips at busy<->idle
    // boundaries so the events-bus snapshot dedupe stays quiet in between.
    let output_active = agent
        .filter(|a| a.state == AgentState::Unknown && info.alive)
        .map(|_| {
            now_ms().saturating_sub(info.last_output_at) <= OUTPUT_ACTIVITY_QUIET.as_millis() as u64
        });
    map.insert(
        "output_active".to_string(),
        output_active.map_or(serde_json::Value::Null, |b| json!(b)),
    );
    // The inverse liveness check for the HOOKS tier: a live claude TUI whose
    // record says Running but whose PTY has been silent past STALL_QUIET is
    // stalled. Boolean only while the claim is checkable (alive, claude,
    // Running); null elsewhere — chat rows never reach this builder. The
    // boundary crossing flips without any hook arriving because the events
    // bus re-runs this builder on its 1s fallback tick (the same mechanism
    // that lets `output_active` go false when output stops).
    let stalled = agent
        .filter(|a| a.kind == AgentKind::Claude && a.state == AgentState::Running && info.alive)
        .map(|_| now_ms().saturating_sub(info.last_output_at) >= STALL_QUIET.as_millis() as u64);
    map.insert(
        "stalled".to_string(),
        stalled.map_or(serde_json::Value::Null, |b| json!(b)),
    );
    // v0.2 status-feed fields, all from the claude TUI hook/statusline ingest
    // (see agents.rs). Null whenever nothing is known: hook-less TUIs never
    // set them, and chat rows carry explicit nulls (the chat client derives
    // richer versions from its journal).
    map.insert(
        "subagents".to_string(),
        agent
            .filter(|a| !a.subagents.is_empty())
            .map_or(serde_json::Value::Null, |a| json!(a.subagents)),
    );
    map.insert(
        "now_line".to_string(),
        agent
            .and_then(|a| a.now_line.as_deref())
            .map_or(serde_json::Value::Null, |l| json!(l)),
    );
    map.insert(
        "usage".to_string(),
        agent
            .and_then(|a| a.usage.as_ref())
            .map_or(serde_json::Value::Null, |u| u.to_json()),
    );
    map.insert(
        "mastermind".to_string(),
        if mastermind {
            json!(true)
        } else {
            serde_json::Value::Null
        },
    );
    // Naming rule zero: the most specific thing known about what the session
    // is DOING. A user-pinned name stays authoritative (`renamed` flags the
    // pin for the UI); agents and shells resolve their own chains.
    let display_name = if info.renamed {
        info.name.clone()
    } else {
        match agent {
            Some(agent) => agent.display_name(info.title.as_deref()),
            None => crate::naming::shell_display_name(info, polled),
        }
    };
    map.insert("display_name".to_string(), json!(display_name));
    serde_json::Value::Object(map)
}

/// The full session list as JSON values (shared by GET /sessions and the
/// /ws/events snapshots): PTY rows plus synthetic rows for structured chat
/// sessions, sorted by creation time so the rail interleaves them honestly.
/// Lock order: workspaces (taken and dropped first, for the mastermind
/// bindings) -> session_workspaces -> agents -> display_names ->
/// current_cwds -> exec_status.
pub(crate) fn sessions_json(state: &AppState) -> Vec<serde_json::Value> {
    let sessions = state.sessions.list();
    let chats = state.chat.list();
    // Mastermind bindings (workspace id -> session id), taken — and dropped —
    // BEFORE the row locks below so the workspace store never nests inside
    // them. Computed per snapshot, so the flag can't disagree with the store.
    let masterminds = crate::lock(&state.workspaces).mastermind_bindings();
    let workspaces = crate::lock(&state.session_workspaces);
    let agents = crate::lock(&state.agents);
    let names = crate::lock(&state.display_names);
    let cwds = crate::lock(&state.current_cwds);
    let execs = crate::lock(&state.exec_status);
    let is_mastermind = |id: &str| {
        workspaces
            .get(id)
            .is_some_and(|ws| masterminds.get(ws).is_some_and(|sid| sid == id))
    };
    let mut rows: Vec<(u64, serde_json::Value)> = sessions
        .iter()
        .map(|info| {
            let mut row = session_json(
                info,
                workspaces.get(&info.id).cloned(),
                agents.get(&info.id),
                names.get(&info.id).map(String::as_str),
                cwds.get(&info.id).map(PathBuf::as_path),
                execs.get(&info.id).copied(),
                is_mastermind(&info.id),
            );
            if let serde_json::Value::Object(map) = &mut row {
                map.insert("ui".to_string(), json!("term"));
                let chat_capable = agents.get(&info.id).is_some_and(|a| a.kind.chat_capable());
                map.insert("chat_capable".to_string(), json!(chat_capable));
            }
            (info.created_at, row)
        })
        .collect();
    rows.extend(chats.iter().map(|info| {
        (
            info.created_at_ms / 1000,
            crate::chat::chat_session_json(
                info,
                workspaces.get(&info.id).cloned(),
                agents.get(&info.id),
                is_mastermind(&info.id),
            ),
        )
    }));
    // Mid view-switch a session lives in NEITHER registry for a moment
    // (old process killed, new one not yet spawned). A vanishing row would
    // make every window prune the session's tabs, so synthesize a
    // placeholder carrying the TARGET surface until the respawn registers.
    for (id, target) in crate::lock(&state.chat_switching).iter() {
        if rows.iter().any(|(_, row)| row["id"] == json!(id)) {
            continue;
        }
        let Some(record) = agents.get(id) else {
            continue;
        };
        rows.push((
            u64::MAX,
            json!({
                "id": id,
                "name": record.kind.as_str(),
                "cwd": "",
                "cols": 0,
                "rows": 0,
                "created_at": 0,
                "alive": true,
                "exit_status": null,
                "title": null,
                "pid": null,
                "renamed": false,
                "phase": "unknown",
                "cwd_current": "",
                "exec_stage": null,
                "workspace_id": workspaces.get(id),
                "kind": "agent",
                "agent_kind": record.kind.as_str(),
                "agent_state": record.state.as_str(),
                "agent_title": record.title(),
                "files_touched": record.files_touched,
                "output_active": null,
                "stalled": null,
                "subagents": null,
                "now_line": null,
                "usage": null,
                "mastermind": if is_mastermind(id) { json!(true) } else { json!(null) },
                "display_name": record.display_name(None),
                "ui": target,
                "chat_capable": true,
            }),
        ));
    }
    rows.sort_by_key(|(created, _)| *created);
    rows.into_iter().map(|(_, row)| row).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_state::{AgentKind, AgentRecord};

    fn info(alive: bool, last_output_at: u64) -> chimaera_pty::SessionInfo {
        chimaera_pty::SessionInfo {
            id: "s1".to_string(),
            name: "codex".to_string(),
            cwd: PathBuf::from("/tmp"),
            cols: 80,
            rows: 24,
            created_at: 0,
            alive,
            exit_status: None,
            title: None,
            pid: None,
            renamed: false,
            phase: chimaera_pty::ShellPhase::Unknown,
            last_output_at,
        }
    }

    /// `output_active` is part of the wire contract: a boolean only for a
    /// LIVE agent row still in state Unknown (the hook-less TUIs), null
    /// everywhere a better signal exists. The UI has no tests of its own —
    /// keep the key name and gating pinned here.
    #[test]
    fn output_active_gates_on_unknown_live_agent_rows() {
        let active = |info: chimaera_pty::SessionInfo, agent: Option<&AgentRecord>| {
            session_json(&info, None, agent, None, None, None, false)["output_active"].clone()
        };

        // Shell rows (no agent record): null.
        assert_eq!(active(info(true, now_ms()), None), json!(null));

        // Unknown-state agent, fresh output: true. Long quiet: false.
        let unknown = AgentRecord::new("k".into(), AgentKind::Codex);
        assert_eq!(active(info(true, now_ms()), Some(&unknown)), json!(true));
        assert_eq!(
            active(info(true, now_ms() - 60_000), Some(&unknown)),
            json!(false)
        );

        // A dead session is never "active", whatever it last wrote.
        assert_eq!(active(info(false, now_ms()), Some(&unknown)), json!(null));

        // A row with real hook/protocol state carries null (the state wins).
        let mut running = AgentRecord::new("k".into(), AgentKind::Claude);
        running.state = AgentState::Running;
        assert_eq!(active(info(true, now_ms()), Some(&running)), json!(null));
    }

    /// `stalled` is part of the wire contract: a boolean only for a LIVE
    /// claude row (the hooks tier) whose state claims Running — true once
    /// the PTY has been silent past the stall window, null everywhere the
    /// claim isn't checkable. Pinned here; the UI has no tests of its own.
    #[test]
    fn stalled_gates_on_running_live_claude_rows() {
        let stalled = |info: chimaera_pty::SessionInfo, agent: Option<&AgentRecord>| {
            session_json(&info, None, agent, None, None, None, false)["stalled"].clone()
        };
        let quiet_ms = now_ms() - STALL_QUIET.as_millis() as u64 - 1_000;

        let mut running = AgentRecord::new("k".into(), AgentKind::Claude);
        running.state = AgentState::Running;
        // Running claude TUI: false while output flows, true once silent
        // past the window.
        assert_eq!(stalled(info(true, now_ms()), Some(&running)), json!(false));
        assert_eq!(stalled(info(true, quiet_ms), Some(&running)), json!(true));
        // Not checkable: shells, dead sessions, non-Running states, and
        // hook-less kinds (whose Running could only be a stale carry-over).
        assert_eq!(stalled(info(true, quiet_ms), None), json!(null));
        assert_eq!(stalled(info(false, quiet_ms), Some(&running)), json!(null));
        let unknown = AgentRecord::new("k".into(), AgentKind::Claude);
        assert_eq!(stalled(info(true, quiet_ms), Some(&unknown)), json!(null));
        let mut codex = AgentRecord::new("k".into(), AgentKind::Codex);
        codex.state = AgentState::Running;
        assert_eq!(stalled(info(true, quiet_ms), Some(&codex)), json!(null));
    }

    /// The v0.2 status-feed fields are part of the wire contract: null when
    /// nothing is known, and exactly these key names/shapes when the claude
    /// TUI ingest populated the record.
    #[test]
    fn status_feed_fields_ride_agent_rows() {
        let row = |agent: Option<&AgentRecord>| {
            session_json(&info(true, now_ms()), None, agent, None, None, None, false)
        };

        // Nothing known: all three are null (shells and fresh agents alike).
        let fresh = AgentRecord::new("k".into(), AgentKind::Claude);
        for key in ["subagents", "now_line", "usage"] {
            assert_eq!(row(None)[key], json!(null), "{key}");
            assert_eq!(row(Some(&fresh))[key], json!(null), "{key}");
        }

        let mut record = AgentRecord::new("k".into(), AgentKind::Claude);
        record.subagent_started("a1", "Explore", 42);
        record.now_line = Some("ran Bash".into());
        record.usage = Some(crate::agent_state::AgentUsage {
            model: Some("Opus".into()),
            context_pct: Some(42),
            cost_cents: Some(12),
        });
        let row = row(Some(&record));
        assert_eq!(
            row["subagents"],
            json!([{"id": "a1", "label": "Explore", "started_at": 42}])
        );
        assert_eq!(row["now_line"], json!("ran Bash"));
        assert_eq!(
            row["usage"],
            json!({"model": "Opus", "context_pct": 42, "cost_usd": 0.12})
        );
    }

    /// `mastermind` is part of the wire contract: `true` only for the
    /// workspace's bound Mastermind (the UI hides flagged rows from the
    /// roster/rail), null on every other row — shells included. Pinned here;
    /// the UI has no tests of its own.
    #[test]
    fn mastermind_flag_rides_session_rows() {
        let record = AgentRecord::new("k".into(), AgentKind::Claude);
        let row = |agent: Option<&AgentRecord>, mastermind: bool| {
            session_json(
                &info(true, now_ms()),
                None,
                agent,
                None,
                None,
                None,
                mastermind,
            )["mastermind"]
                .clone()
        };
        assert_eq!(row(Some(&record), true), json!(true));
        assert_eq!(row(Some(&record), false), json!(null));
        assert_eq!(row(None, false), json!(null));
    }
}
