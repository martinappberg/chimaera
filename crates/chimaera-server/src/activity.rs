//! Real client interaction, separate from output and connection lifetime. An
//! idle window or a terminal's repaint must never keep remote compute awake.
use std::collections::{HashMap, HashSet};

const MAX_SESSIONS: usize = 4096;
#[derive(Default)]
pub(crate) struct Activity {
    input: HashMap<String, u64>,
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
