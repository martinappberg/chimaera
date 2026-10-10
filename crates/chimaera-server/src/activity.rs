//! Real client interaction, separate from output and connection lifetime. An
//! idle window or a terminal's repaint must never keep remote compute awake.
use std::collections::{HashMap, HashSet};

const MAX_SESSIONS: usize = 4096;
#[derive(Default)]
pub struct Activity {
    input: HashMap<String, u64>,
    /// The last authenticated user mutation that is not a session input: a
    /// file save or upload, a Git operation, a session created or closed.
    changed: u64,
}
impl Activity {
    pub(crate) fn record(&mut self, id: &str, at: u64) {
        if self.input.len() < MAX_SESSIONS || self.input.contains_key(id) {
            self.input
                .entry(id.to_owned())
                .and_modify(|v| *v = (*v).max(at))
                .or_insert(at);
        }
    }
    pub fn snapshot<'a>(&mut self, live: impl Iterator<Item = &'a str>) -> HashMap<String, u64> {
        let live: HashSet<_> = live.collect();
        self.input.retain(|id, _| live.contains(id.as_str()));
        self.input.clone()
    }
}
/// Routes whose mutations are the user changing something: files, drafts,
/// Git worktrees, sessions, workspaces and their links. An explicit list,
/// because several read-only helpers are POSTs (`/fs/resolve_targets`,
/// `/fs/ticket`, `/fs/validate`, `/plugins/preview`) and a phone merely
/// viewing a project must not keep a cloud machine awake. Pro transfer
/// control and per-window view state are not interaction either.
const CHANGES: &[&str] = &[
    "/fs/file",
    "/fs/drafts",
    "/fs/draft",
    "/fs/mkdir",
    "/fs/create",
    "/fs/rename",
    "/fs/copy",
    "/fs/move",
    "/fs/delete",
    "/fs/upload",
    "/git/worktrees",
    "/sessions",
    "/workspaces",
    "/links",
];
/// Whether an authenticated request is the user changing something.
pub(crate) fn is_change(method: &axum::http::Method, path: &str) -> bool {
    use axum::http::Method;
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) && CHANGES.iter().any(|route| {
        path == *route
            || path
                .strip_prefix(route)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}
pub(crate) fn touch(state: &crate::AppState) {
    if !state.policy().active(state) {
        return;
    }
    crate::lock(&state.activity).changed = crate::session_view::now_ms();
}
pub fn last_change(state: &crate::AppState) -> Option<u64> {
    let changed = crate::lock(&state.activity).changed;
    (changed > 0).then_some(changed)
}
/// Stamps a session's last input. Deliberately no change notification: the
/// stamp is read when a sessions list is next built (`session_view`) or
/// polled (`GET /sessions`, a cloud machine's idle check), and no window
/// shows it, so a keystroke must never rebuild and push every window's list.
/// Only an active plan reads it (`WorkspacePolicy::active`): elsewhere input costs nothing.
pub(crate) fn record(state: &crate::AppState, id: &str) {
    if !state.policy().active(state) {
        return;
    }
    crate::lock(&state.activity).record(id, crate::session_view::now_ms());
}
/// The chat commands that are the user acting. They count as interaction
/// here; the daemon's relay holds them and wakes a sleeping owner for them,
/// while ordinary input stays with that owner (`session_proxy`). Only explicit
/// Take over changes execution ownership. A keeper holds and wakes sleeping
/// cloud machine sockets for the same list (VIEWING.md, "A sleeping cloud machine's sockets"
/// mirrors it; change both together). The seven settings commands the user
/// gives are held by both as well, without waking or moving anything
/// ([`chat_frame`]). The automatic `SetThinking`, the reads, `cancel_send`
/// (not an `AgentCommand`) and queue housekeeping neither wake nor are held
/// anywhere.
/// `SendAfterTurn` is a send: idle it opens a turn like any other.
pub(crate) fn is_interaction(command: &chimaera_agent::model::AgentCommand) -> bool {
    use chimaera_agent::model::AgentCommand;
    matches!(
        command,
        AgentCommand::Send { .. }
            | AgentCommand::SendAfterTurn { .. }
            | AgentCommand::Permission { .. }
            | AgentCommand::Answer { .. }
            | AgentCommand::Elicitation { .. }
            | AgentCommand::Interrupt
            | AgentCommand::Compact
            | AgentCommand::Rewind { dry_run: false, .. }
            | AgentCommand::BackgroundTool { .. }
            | AgentCommand::StopTask { .. }
    )
}
/// What a chat frame is to whoever holds input for an owner that has not
/// answered (this daemon's relay; a keeper sorts the same way).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatFrame {
    /// The user acting: exactly [`is_interaction`]. Held in order, and a
    /// reason to wake the current owner, never an implicit takeover.
    Acting,
    /// One of the seven settings the user gives. Held (coalesced) and
    /// delivered in order with what the user did, but no reason to wake or
    /// move anything.
    Setting,
    /// Everything else (the automatic `set_thinking`, reads, `cancel_send`,
    /// queue housekeeping, a command nobody knows): never held.
    Passive,
}
/// [`ChatFrame`] from a frame's `type` (and a rewind's `dry_run`) alone, so a
/// relay need not build the command, a send's pictures included, to sort it.
/// `Acting` is [`is_interaction`], which the tests hold it to for every
/// command.
pub fn chat_frame(kind: &str, dry_run: bool) -> ChatFrame {
    match kind {
        "send" | "send_after_turn" | "permission" | "answer" | "elicitation" | "interrupt"
        | "compact" | "background_tool" | "stop_task" => ChatFrame::Acting,
        "rewind" if !dry_run => ChatFrame::Acting,
        "set_model" | "set_mode" | "set_effort" | "set_ultracode" | "set_remote_control"
        | "set_mcp_enabled" | "reconnect_mcp" => ChatFrame::Setting,
        _ => ChatFrame::Passive,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// One of every chat command. The match has no wildcard: a new command
    /// does not compile until it is sorted here, and so in [`chat_frame`].
    fn every_command() -> Vec<chimaera_agent::model::AgentCommand> {
        use chimaera_agent::model::AgentCommand as C;
        let all = vec![
            C::Send { blocks: Vec::new() },
            C::Permission {
                request_id: "r".into(),
                option_id: "o".into(),
                destination: None,
                feedback: None,
            },
            C::Interrupt,
            C::SetMode {
                mode_id: "m".into(),
            },
            C::SetModel {
                model_id: "m".into(),
            },
            C::SetEffort {
                effort_id: "e".into(),
            },
            C::SetThinking { enabled: true },
            C::SetUltracode { enabled: true },
            C::Answer {
                request_id: "r".into(),
                answers: Default::default(),
            },
            C::Elicitation {
                request_id: "r".into(),
                action: chimaera_agent::elicitation::ElicitationAction::Decline,
                content: serde_json::Value::Null,
            },
            C::GetUsage,
            C::Compact,
            C::Rewind {
                user_message_id: "u".into(),
                dry_run: false,
            },
            C::Rewind {
                user_message_id: "u".into(),
                dry_run: true,
            },
            C::BackgroundTool {
                tool_call_id: "t".into(),
            },
            C::StopTask {
                task_id: "t".into(),
            },
            C::GetMcp,
            C::SetMcpEnabled {
                server: "s".into(),
                enabled: true,
            },
            C::ReconnectMcp { server: "s".into() },
            C::CancelQueued { id: "q".into() },
            C::SteerQueued { id: "q".into() },
            C::SetRemoteControl {
                enabled: true,
                name: None,
            },
            C::SendAfterTurn { blocks: Vec::new() },
            C::SendNow { id: "q".into() },
            C::SendIfRunning {
                id: "k".into(),
                blocks: Vec::new(),
            },
        ];
        for command in &all {
            match command {
                C::Send { .. }
                | C::Permission { .. }
                | C::Interrupt
                | C::SetMode { .. }
                | C::SetModel { .. }
                | C::SetEffort { .. }
                | C::SetThinking { .. }
                | C::SetUltracode { .. }
                | C::Answer { .. }
                | C::Elicitation { .. }
                | C::GetUsage
                | C::Compact
                | C::Rewind { .. }
                | C::BackgroundTool { .. }
                | C::StopTask { .. }
                | C::GetMcp
                | C::SetMcpEnabled { .. }
                | C::ReconnectMcp { .. }
                | C::CancelQueued { .. }
                | C::SteerQueued { .. }
                | C::SetRemoteControl { .. }
                | C::SendAfterTurn { .. }
                | C::SendNow { .. }
                | C::SendIfRunning { .. } => {}
            }
        }
        all
    }
    /// What a relay reads from a frame's `type` agrees with the daemon's own
    /// list for every command, and the seven settings are exactly seven.
    #[test]
    fn a_frame_sorted_by_its_type_agrees_with_the_daemons_list() {
        let mut settings = Vec::new();
        for command in every_command() {
            let text = serde_json::to_string(&command).unwrap();
            let tag = crate::ws::command_tag(&text);
            let kind = tag.kind.as_deref().unwrap_or_default();
            let sorted = chat_frame(kind, tag.dry_run);
            assert_eq!(
                sorted == ChatFrame::Acting,
                is_interaction(&command),
                "{text}"
            );
            if sorted == ChatFrame::Setting {
                settings.push(kind.to_owned());
            }
        }
        settings.sort();
        assert_eq!(
            settings,
            [
                "reconnect_mcp",
                "set_effort",
                "set_mcp_enabled",
                "set_mode",
                "set_model",
                "set_remote_control",
                "set_ultracode"
            ]
        );
        for passive in [
            "cancel_send",
            "set_thinking",
            "get_usage",
            "from_the_future",
            "",
        ] {
            assert_eq!(chat_frame(passive, false), ChatFrame::Passive, "{passive}");
        }
    }
    #[test]
    fn user_changes_count_but_transfer_control_and_view_state_do_not() {
        use axum::http::Method;
        assert!(is_change(&Method::PUT, "/api/v1/fs/file"));
        assert!(is_change(&Method::POST, "/sessions/s-a/upload"));
        assert!(is_change(&Method::POST, "/api/v1/git/worktrees"));
        assert!(is_change(&Method::DELETE, "/api/v1/sessions/s-a"));
        assert!(!is_change(&Method::GET, "/api/v1/fs/file"));
        assert!(!is_change(&Method::POST, "/api/v1/pro/drain"));
        assert!(!is_change(&Method::PUT, "/view-state/tabs_w"));
        // Read-only helpers that happen to be POSTs: a viewer is not working.
        for path in [
            "/api/v1/fs/resolve_targets",
            "/api/v1/fs/ticket",
            "/api/v1/fs/validate",
            "/api/v1/plugins/preview",
            "/api/v1/fs/filepath-lookalike",
        ] {
            assert!(!is_change(&Method::POST, path), "{path}");
        }
    }
    #[test]
    fn input_is_monotonic_and_retired_sessions_are_pruned() {
        let mut activity = Activity::default();
        activity.record("a", 10);
        activity.record("a", 5);
        activity.record("retired", 20);
        assert_eq!(
            activity.snapshot(["a"].into_iter()),
            HashMap::from([("a".into(), 10)])
        );
    }
    #[test]
    fn automatic_inventory_reads_do_not_count_as_interaction() {
        use chimaera_agent::model::AgentCommand;
        assert!(!is_interaction(&AgentCommand::GetUsage));
        assert!(!is_interaction(&AgentCommand::GetMcp));
        assert!(!is_interaction(&AgentCommand::SetThinking {
            enabled: true
        }));
        assert!(is_interaction(&AgentCommand::Interrupt));
        // A message held for the end of the turn is still the user's message.
        assert!(is_interaction(&AgentCommand::SendAfterTurn {
            blocks: Vec::new()
        }));
        assert!(is_interaction(&AgentCommand::Send { blocks: Vec::new() }));
    }
    #[test]
    fn identifiers_cannot_grow_unbounded_without_a_snapshot() {
        let mut activity = Activity::default();
        for n in 0..MAX_SESSIONS + 100 {
            activity.record(&n.to_string(), 0);
        }
        assert_eq!(activity.input.len(), MAX_SESSIONS);
    }
}
