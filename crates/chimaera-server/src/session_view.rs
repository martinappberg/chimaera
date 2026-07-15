use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::agent_state::AgentState;
use crate::AppState;

/// Quiet window after the last PTY output chunk before a hook-less agent TUI
/// reads as idle. Must sit ABOVE the ~1 Hz repaint cadence of a working TUI
/// (codex/gemini animate a spinner + elapsed counter while a turn runs) so a
/// mid-turn agent never flaps to idle between repaints, and low enough that
/// the dot settles soon after the turn ends. Tuned against a live codex TUI.
const OUTPUT_ACTIVITY_QUIET: Duration = Duration::from_secs(2);

/// Serialize a `SessionInfo` with the extra `workspace_id`, `kind`,
/// `agent_kind`, `agent_state`, `agent_title`, `files_touched`,
/// `display_name` and `cwd_current` fields.
/// `agent` is the wrapper record for kind "agent" sessions; `None` means a
/// plain shell. `polled` / `polled_cwd` are the shell naming watcher's
/// latest values, if any; `cwd_current` falls back to the spawn cwd (agents,
/// never-polled shells).
pub(crate) fn session_json(
    info: &chimaera_pty::SessionInfo,
    workspace_id: Option<String>,
    agent: Option<&crate::agents::AgentRecord>,
    polled: Option<&str>,
    polled_cwd: Option<&std::path::Path>,
    exec_stage: Option<chimaera_pty::ExecStage>,
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
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            now_ms.saturating_sub(info.last_output_at) <= OUTPUT_ACTIVITY_QUIET.as_millis() as u64
        });
    map.insert(
        "output_active".to_string(),
        output_active.map_or(serde_json::Value::Null, |b| json!(b)),
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
/// Lock order: session_workspaces -> agents -> display_names ->
/// current_cwds -> exec_status.
pub(crate) fn sessions_json(state: &AppState) -> Vec<serde_json::Value> {
    let sessions = state.sessions.list();
    let chats = state.chat.list();
    let workspaces = crate::lock(&state.session_workspaces);
    let agents = crate::lock(&state.agents);
    let names = crate::lock(&state.display_names);
    let cwds = crate::lock(&state.current_cwds);
    let execs = crate::lock(&state.exec_status);
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
                "display_name": record.display_name(None),
                "ui": target,
                "chat_capable": true,
            }),
        ));
    }
    rows.sort_by_key(|(created, _)| *created);
    rows.into_iter().map(|(_, row)| row).collect()
}
