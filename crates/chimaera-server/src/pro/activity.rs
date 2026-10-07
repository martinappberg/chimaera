//! One verdict on what a session is doing, for every transfer decision: the
//! pause a hand-back waits for (`at_pause`), the work the coordinator copies
//! and "Run in the cloud" carries (`sessions_at_work`), the continuation a
//! receipt records and the session rows' additive `at_pause`. Each of those
//! used to re-derive it and they disagreed on edge cases (an errored turn, a
//! chat nothing was known about), which left returns waiting forever.
use crate::{agent_state::AgentState, lock, AppState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    /// A turn in flight, input queued for the next one, or background work.
    Working,
    /// Parked on a permission or a question: the user's move. A pause for
    /// transfer purposes, but still worth starting the cloud for.
    WaitingOnUser,
    /// Nothing runs until the user sends again: a finished, interrupted,
    /// errored or rate-limited turn, an idle prompt, or nothing known.
    Paused,
}

/// A chat: the driver's carryover is the authority on what is in flight. An
/// agent state of `Running` with no turn in flight is still treated as work
/// (the event may lead the carryover by a moment); every other state with
/// nothing in flight, `Unknown` included, is a pause.
pub(super) fn chat(
    chat: &chimaera_agent::ChatInfo,
    carry: Option<&chimaera_agent::Carryover>,
    queued_input: bool,
    agent_state: Option<AgentState>,
) -> Activity {
    if !chat.alive {
        return Activity::Paused;
    }
    // Queued input runs next even behind a permission prompt.
    if queued_input
        || chat.background_running > 0
        || carry.is_some_and(|c| !c.background.is_empty())
    {
        return Activity::Working;
    }
    if chat.pending_permission || chat.status_needs_action {
        return Activity::WaitingOnUser;
    }
    if carry.is_some_and(carried) || agent_state == Some(AgentState::Running) {
        return Activity::Working;
    }
    Activity::Paused
}

/// A carried record says work was cut off: the same rule a live chat uses,
/// applied to what a stopped one left (`continuation`).
pub(super) fn carried(carry: &chimaera_agent::Carryover) -> bool {
    carry.interrupted_work()
}

/// One session, chat or terminal. A session with no agent record (a plain
/// shell) is paused: shells are never waited for.
pub(crate) fn session(state: &AppState, id: &str) -> Activity {
    if let Some(info) = state.chat.get(id) {
        let activity = state.chat.input_activity(id);
        let agent_state = lock(&state.agents).get(id).map(|agent| agent.state);
        return chat(
            &info,
            activity.as_ref().map(|(carry, _)| carry),
            activity.as_ref().is_some_and(|(_, queued)| *queued),
            agent_state,
        );
    }
    // Cloned first: the terminal registry has its own locks.
    let Some(record) = lock(&state.agents).get(id).cloned() else {
        return Activity::Paused;
    };
    let Some(info) = state.sessions.get(id) else {
        return Activity::Paused;
    };
    if !info.alive {
        return Activity::Paused;
    }
    if record.state == AgentState::NeedsPermission {
        return Activity::WaitingOnUser;
    }
    if crate::agent_state::tui_at_pause(
        &record,
        info.alive,
        info.last_output_at,
        info.pid,
        state.sessions.foreground_pid(id),
        crate::session_view::now_ms(),
    ) {
        Activity::Paused
    } else {
        Activity::Working
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> chimaera_agent::ChatInfo {
        chimaera_agent::ChatInfo {
            id: "s-claude".into(),
            agent: "claude".into(),
            cwd: "/tmp".into(),
            created_at_ms: 0,
            alive: true,
            exit_status: None,
            native_session_id: None,
            model: None,
            current_mode: None,
            pending_permission: false,
            status_detail: None,
            status_category: None,
            status_needs_action: false,
            remote_control_url: None,
            background_running: 0,
        }
    }

    /// Every combination the transfer decisions meet, against one table.
    #[test]
    fn one_verdict_over_every_combination() {
        use Activity::*;
        let states = [
            None,
            Some(AgentState::Running),
            Some(AgentState::NeedsPermission),
            Some(AgentState::IdlePrompt),
            Some(AgentState::Finished),
            Some(AgentState::Errored),
            Some(AgentState::RateLimited),
            Some(AgentState::Unknown),
        ];
        let mut checked = 0;
        for agent in states {
            for in_flight in [false, true] {
                for queued in [false, true] {
                    for background in [false, true] {
                        for waiting in [false, true] {
                            for known in [false, true] {
                                let mut chat_info = info();
                                chat_info.background_running = usize::from(background);
                                chat_info.pending_permission = waiting;
                                let carry = chimaera_agent::Carryover {
                                    turn_in_flight: in_flight,
                                    ..Default::default()
                                };
                                let verdict =
                                    chat(&chat_info, known.then_some(&carry), queued, agent);
                                let in_flight = in_flight && known;
                                let expected = if queued || background {
                                    Working
                                } else if waiting {
                                    WaitingOnUser
                                } else if in_flight || agent == Some(AgentState::Running) {
                                    Working
                                } else {
                                    Paused
                                };
                                assert_eq!(
                                    verdict, expected,
                                    "agent {agent:?} in_flight {in_flight} queued {queued} background {background} waiting {waiting} known {known}"
                                );
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 256);
    }

    #[test]
    fn an_ended_turn_is_a_pause_and_a_dead_chat_never_works() {
        let carry = chimaera_agent::Carryover::default();
        for ended in [
            AgentState::Finished,
            AgentState::IdlePrompt,
            AgentState::Errored,
            AgentState::RateLimited,
            AgentState::Unknown,
        ] {
            assert_eq!(
                chat(&info(), Some(&carry), false, Some(ended)),
                Activity::Paused
            );
            // Input queued behind the failure runs next.
            assert_eq!(
                chat(&info(), Some(&carry), true, Some(ended)),
                Activity::Working
            );
        }
        // Nothing known about a resumed chat with nothing in flight: a pause.
        assert_eq!(chat(&info(), None, false, None), Activity::Paused);
        let mut dead = info();
        dead.alive = false;
        dead.background_running = 2;
        assert_eq!(
            chat(&dead, Some(&carry), true, Some(AgentState::Running)),
            Activity::Paused
        );
        // A question waits on the user too.
        let mut asking = info();
        asking.status_needs_action = true;
        let in_flight = chimaera_agent::Carryover {
            turn_in_flight: true,
            ..Default::default()
        };
        assert_eq!(
            chat(&asking, Some(&in_flight), false, None),
            Activity::WaitingOnUser
        );
    }
}
