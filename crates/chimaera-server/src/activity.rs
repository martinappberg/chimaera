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
/// Whether an authenticated request is the user changing something. Pro
/// transfer control (the supervisor's own calls) and per-window view state
/// are not interaction.
pub(crate) fn is_change(method: &axum::http::Method, path: &str) -> bool {
    use axum::http::Method;
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) && !path.starts_with("/pro/")
        && !path.starts_with("/view-state")
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
pub(crate) fn is_interaction(command: &chimaera_agent::model::AgentCommand) -> bool {
    use chimaera_agent::model::AgentCommand;
    matches!(
        command,
        AgentCommand::Send { .. }
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
        assert!(is_change(&Method::POST, "/api/v1/git/commit"));
        assert!(!is_change(&Method::GET, "/api/v1/fs/file"));
        assert!(!is_change(&Method::POST, "/api/v1/pro/drain"));
        assert!(!is_change(&Method::PUT, "/view-state/tabs_w"));
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
        assert!(is_interaction(&AgentCommand::Interrupt));
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
