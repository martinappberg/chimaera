//! Real client interaction, separate from output and connection lifetime. An
//! idle window or a terminal's repaint must never keep remote compute awake.
use std::collections::{HashMap, HashSet};

const MAX_SESSIONS: usize = 4096;
#[derive(Default)]
pub(crate) struct Activity {
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
    pub(crate) fn snapshot<'a>(
        &mut self,
        live: impl Iterator<Item = &'a str>,
    ) -> HashMap<String, u64> {
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
    crate::lock(&state.activity).changed = crate::session_view::now_ms();
}
pub(crate) fn last_change(state: &crate::AppState) -> Option<u64> {
    let changed = crate::lock(&state.activity).changed;
    (changed > 0).then_some(changed)
}
pub(crate) fn record(state: &crate::AppState, id: &str) {
    crate::lock(&state.activity).record(id, crate::session_view::now_ms());
    state.changes.notify_waiters();
}
/// The chat commands that are the user acting: they count as interaction
/// here, and they are the ones a keeper that keeps a sleeping cloud machine's
/// sockets holds and wakes the machine for (VIEWING.md, "A sleeping cloud
/// machine's sockets" mirrors this list; change both together). Everything
/// else (reads, settings, queue housekeeping) neither wakes nor is held.
/// `SendAfterTurn` is a send: idle it opens a turn like any other.
pub(crate) fn is_interaction(command: &chimaera_agent::model::AgentCommand) -> bool {
    use chimaera_agent::model::AgentCommand;
    matches!(
        command,
        AgentCommand::Send { .. }
            | AgentCommand::SendAfterTurn { .. }
            | AgentCommand::Permission { .. }
            | AgentCommand::Answer { .. }
            | AgentCommand::Interrupt
            | AgentCommand::Compact
            | AgentCommand::Rewind { dry_run: false, .. }
            | AgentCommand::BackgroundTool { .. }
            | AgentCommand::StopTask { .. }
    )
}
#[cfg(test)]
mod tests {
    use super::*;
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
