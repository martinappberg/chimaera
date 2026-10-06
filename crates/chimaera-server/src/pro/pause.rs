//! Why a session has no process here, in the words its viewers get: the
//! `moved` / `paused` frames and the refusal a read-only socket answers.
//! Only Pro transfer and ownership paths populate what this reads.
use serde_json::json;

use crate::AppState;

/// Why a session with no process here is not an exit. `Moved`: it continues on
/// another machine (`to` is where it is going, in the viewer's words;
/// `"other"` is another of the user's computers, sent as `to:"computer"` with
/// the additive `other:true`).
/// `Paused`: it stays here and resumes on its own — after this daemon restarts
/// (`restarting`), once its agent is signed in on this cloud machine
/// (`needs_provider`), while its transfer finishes opening it (`importing`), or
/// never here at all (`stays_on_computer`: a plain terminal that moved with its
/// project waits for the computer). Both frames are additive; older clients
/// ignore them and reconnect. Only Pro transfer and ownership paths ever
/// populate the registries this reads, so free users never see either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pause {
    Moved(&'static str),
    Paused {
        reason: &'static str,
        provider: Option<String>,
    },
}
impl Pause {
    pub(crate) fn frame(&self) -> serde_json::Value {
        match self {
            Pause::Moved("other") => json!({"type":"moved","to":"computer","other":true}),
            Pause::Moved(to) => json!({"type":"moved","to":to}),
            Pause::Paused { reason, provider } => {
                let mut frame = json!({"type":"paused","reason":reason});
                if let Some(provider) = provider {
                    frame["provider"] = json!(provider);
                }
                frame
            }
        }
    }
}

/// What [`classify_pause`] decides from, gathered from daemon state.
#[derive(Clone, Debug, Default)]
struct PauseFacts {
    /// This daemon is a cloud machine; its own arriving sessions are known.
    worker: bool,
    /// The project's work left this computer for another of the user's
    /// computers (acting there brought it there), not for the cloud.
    other: bool,
    /// A transfer holds this session's lifecycle right now: the source is
    /// exporting it, or the destination is opening it.
    transfer: bool,
    /// The session's process is known to this daemon life.
    known: bool,
    entry: Option<EntryFacts>,
}
#[derive(Clone, Debug, Default)]
struct EntryFacts {
    /// Waiting at boot for this daemon life's ownership proof (a restart or
    /// update), not moving anywhere.
    restarting: bool,
    /// Its project is arriving here (files installing, setup running): the
    /// entry left from the original move is about to be replaced.
    arriving: bool,
    /// Imported here by a transfer and not started yet.
    arrived: bool,
    /// A plain terminal that moved with its project; it only runs on a computer.
    moved_shell: bool,
    /// The agent CLI this session runs, when it is an agent.
    provider: Option<String>,
    /// That agent is not ready on this cloud machine.
    blocked: bool,
    /// This machine may run the project right now.
    writable: bool,
}

/// Decide what a viewer of a stopped session is told. Deliberately pure so
/// every transfer phase is covered by unit tests without Pro fixtures.
///
/// A session that stopped HERE (no import) moved away only while a transfer
/// exports it or when this machine may no longer run its project; one this
/// machine still owns is waiting out a restart check. A cloud machine cannot
/// tell "restart awaiting verification" from "returned to the computer" by
/// ownership alone (both refuse writes): a session that ran in this daemon
/// life was stopped by a hand-back, one that did not is restart-deferred.
/// [`ProState`](crate::pro) exposing the ownership phase would settle both
/// without that inference.
fn classify_pause(facts: &PauseFacts, ran_here: impl FnOnce() -> bool) -> Option<Pause> {
    let away = if facts.other {
        Pause::Moved("other")
    } else {
        // New reason on the existing paused frame: old clients stay neutral
        // instead of interpreting an unknown moved destination as cloud.
        Pause::Paused {
            reason: "elsewhere",
            provider: None,
        }
    };
    let Some(entry) = &facts.entry else {
        // Opening a transfer before its entry is recorded: say so rather than
        // "unknown session", which a client gives up on after a few retries.
        return (facts.transfer && !facts.known).then_some(Pause::Paused {
            reason: "importing",
            provider: None,
        });
    };
    if entry.restarting {
        return Some(Pause::Paused {
            reason: "restarting",
            provider: None,
        });
    }
    if !facts.worker && entry.arriving && !entry.arrived {
        // Taking its work back: the old entry says so until the import lands.
        return Some(Pause::Moved("computer"));
    }
    if entry.arrived {
        return Some(if facts.worker && entry.moved_shell {
            Pause::Paused {
                reason: "stays_on_computer",
                provider: None,
            }
        } else if facts.worker && entry.blocked {
            Pause::Paused {
                reason: "needs_provider",
                provider: entry.provider.clone(),
            }
        } else if !facts.worker {
            // Work coming back to this computer.
            Pause::Moved("computer")
        } else {
            Pause::Paused {
                reason: "importing",
                provider: None,
            }
        });
    }
    if facts.transfer {
        return Some(away);
    }
    if entry.writable || (facts.worker && !ran_here()) {
        return Some(Pause::Paused {
            reason: "restarting",
            provider: None,
        });
    }
    Some(away)
}

fn entry_facts(state: &AppState, entry: &crate::ledger::LedgerEntry) -> EntryFacts {
    let provider = entry
        .agent
        .as_ref()
        .map(|agent| agent.kind.as_str().to_owned());
    let blocked = provider.as_deref().is_some_and(|provider| {
        super::workspace_provider_blocks(state, &entry.workspace_id)
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|block| block["id"] == provider))
    });
    EntryFacts {
        // Recorded ownership decides the words, never a guess from which
        // processes this daemon life happens to remember.
        restarting: super::restart_deferred(state, &entry.id),
        arriving: super::ownership_phase(state, &entry.workspace_id) == super::Phase::Arriving,
        arrived: entry.handoff.is_some(),
        moved_shell: entry.agent.is_none()
            && entry
                .handoff
                .as_ref()
                .is_some_and(|handoff| handoff.origin == crate::bundle::Origin::Moved),
        provider,
        blocked,
        writable: super::may_write(state, &entry.workspace_id),
    }
}

/// The pause state of session `id`, if it has no process here for a reason
/// that is not an exit.
#[cfg(test)]
pub(crate) fn pause_state(state: &AppState, id: &str) -> Option<Pause> {
    let entry = crate::lock(&state.deferred_sessions).get(id).cloned();
    pause_for(state, id, entry.as_ref())
}

/// [`pause_state`] for a deferred entry the caller already holds (a paused
/// session row).
pub(crate) fn pause_for(
    state: &AppState,
    id: &str,
    entry: Option<&crate::ledger::LedgerEntry>,
) -> Option<Pause> {
    let facts = PauseFacts {
        worker: super::is_worker(state),
        other: crate::lock(&state.session_workspaces)
            .get(id)
            .or(entry.map(|entry| &entry.workspace_id))
            .is_some_and(|workspace| super::other_computer(state, workspace)),
        transfer: crate::lock(&state.chat_switching)
            .get(id)
            .map(String::as_str)
            == Some("transfer"),
        known: state.chat.get(id).is_some() || state.sessions.get(id).is_some(),
        entry: entry.map(|entry| entry_facts(state, entry)),
    };
    classify_pause(&facts, || {
        state.chat.get(id).is_some()
            || state.sessions.get(id).is_some()
            || state.sessions.last_words(id).is_some()
    })
}

/// Input this socket may not deliver. The additive `reason` lets a client say
/// why in its own words (`watching`: the viewer chose to watch; `elsewhere`:
/// the project runs on the other machine right now), and the additive
/// `owner` (`"cloud"` | `"computer"` | null, [`session_owner`]) where it runs, so the
/// words need no guess; `message` stays plain.
pub(crate) fn refusal(state: &AppState, id: &str, watching: bool) -> serde_json::Value {
    let owner = session_owner(state, id);
    if watching {
        json!({"type":"error","code":"read_only","reason":"watching","owner":owner,
               "message":"You're watching. Take control to type."})
    } else {
        let workspace = crate::lock(&state.session_workspaces)
            .get(id)
            .cloned()
            .unwrap_or_default();
        let place = if owner == Some("cloud") {
            "in the cloud"
        } else if super::other_computer(state, &workspace) {
            "on your other computer"
        } else if owner == Some("computer") {
            "on your computer"
        } else {
            "elsewhere"
        };
        json!({"type":"error","code":"read_only","reason":"elsewhere","owner":owner,
               "message":format!("This project is running {place} right now. That was not sent.")})
    }
}

/// Where the project of session `id` runs now, when known (otherwise null):
/// (`pro::owner_kind`; a session with no project runs where this daemon is).
pub(crate) fn session_owner(state: &AppState, id: &str) -> Option<&'static str> {
    let workspace = crate::lock(&state.session_workspaces)
        .get(id)
        .cloned()
        .unwrap_or_default();
    super::owner_kind(state, &workspace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paused(reason: &'static str) -> Option<Pause> {
        Some(Pause::Paused {
            reason,
            provider: None,
        })
    }

    #[test]
    fn work_that_left_for_another_computer_says_so() {
        // Acting on another of the user's computers brought the work there:
        // this computer's own views say it continues on the other computer
        // (older clients read `to:"computer"`), never "in the cloud".
        let left = |transfer, writable| PauseFacts {
            other: true,
            transfer,
            entry: Some(EntryFacts {
                writable,
                provider: Some("claude".into()),
                ..EntryFacts::default()
            }),
            ..PauseFacts::default()
        };
        for (transfer, writable) in [(true, true), (false, false)] {
            let pause = classify_pause(&left(transfer, writable), || true);
            assert_eq!(pause, Some(Pause::Moved("other")));
            assert_eq!(
                pause.unwrap().frame(),
                json!({"type":"moved","to":"computer","other":true})
            );
        }
        // The explicit move destination remains known on either source role.
        let worker = PauseFacts {
            worker: true,
            ..left(true, true)
        };
        assert_eq!(
            classify_pause(&worker, || true),
            Some(Pause::Moved("other"))
        );
    }

    #[test]
    fn an_unknown_move_uses_a_neutral_backward_compatible_pause() {
        let facts = PauseFacts {
            transfer: true,
            entry: Some(EntryFacts::default()),
            ..PauseFacts::default()
        };
        for worker in [false, true] {
            let pause = classify_pause(
                &PauseFacts {
                    worker,
                    ..facts.clone()
                },
                || true,
            )
            .unwrap();
            assert_eq!(pause.frame(), json!({"type":"paused","reason":"elsewhere"}));
        }
    }

    #[test]
    fn only_a_real_transfer_says_moved_and_it_names_where_the_session_goes() {
        let stopped = |worker, writable| PauseFacts {
            worker,
            entry: Some(EntryFacts {
                writable,
                provider: Some("claude".into()),
                ..EntryFacts::default()
            }),
            ..PauseFacts::default()
        };
        // Exporting for a transfer: away from this machine.
        let exporting = |worker| PauseFacts {
            transfer: true,
            ..stopped(worker, true)
        };
        assert_eq!(
            classify_pause(&exporting(false), || true),
            paused("elsewhere")
        );
        assert_eq!(
            classify_pause(&exporting(true), || true),
            paused("elsewhere")
        );
        // Stopped because another machine now runs the project.
        assert_eq!(
            classify_pause(&stopped(false, false), || true),
            paused("elsewhere")
        );
        assert_eq!(
            classify_pause(&stopped(true, false), || true),
            paused("elsewhere")
        );
        // Waiting out a restart on the machine that owns the project: after
        // every Pro update, a computer's own chats are not "in the cloud".
        assert_eq!(
            classify_pause(&stopped(false, true), || false),
            paused("restarting")
        );
        // A cloud machine that restarted refuses writes until verified, yet
        // its sessions did not go anywhere.
        assert_eq!(
            classify_pause(&stopped(true, false), || false),
            paused("restarting")
        );
        // No entry and no transfer: an ordinary session, nothing to say.
        assert_eq!(classify_pause(&PauseFacts::default(), || true), None);
        // A transfer opening a session before its entry exists.
        let opening = PauseFacts {
            transfer: true,
            ..PauseFacts::default()
        };
        assert_eq!(classify_pause(&opening, || false), paused("importing"));
        let snapshot_of_a_live_session = PauseFacts {
            known: true,
            ..opening
        };
        assert_eq!(classify_pause(&snapshot_of_a_live_session, || true), None);
    }

    #[test]
    fn work_arriving_is_opening_or_waiting_never_moving_away() {
        let arrived = |worker, blocked, moved_shell| PauseFacts {
            worker,
            transfer: true,
            entry: Some(EntryFacts {
                arrived: true,
                blocked,
                moved_shell,
                provider: (!moved_shell).then(|| "codex".to_owned()),
                ..EntryFacts::default()
            }),
            ..PauseFacts::default()
        };
        assert_eq!(
            classify_pause(&arrived(true, false, false), || true),
            paused("importing")
        );
        assert_eq!(
            classify_pause(&arrived(true, true, false), || true),
            Some(Pause::Paused {
                reason: "needs_provider",
                provider: Some("codex".into())
            })
        );
        assert_eq!(
            classify_pause(&arrived(true, false, true), || true),
            paused("stays_on_computer")
        );
        // A computer receiving its work back.
        assert_eq!(
            classify_pause(&arrived(false, false, false), || true),
            Some(Pause::Moved("computer"))
        );
        let frame = Pause::Paused {
            reason: "needs_provider",
            provider: Some("claude".into()),
        }
        .frame();
        assert_eq!(
            frame,
            json!({"type":"paused","reason":"needs_provider","provider":"claude"})
        );
        assert_eq!(
            Pause::Moved("cloud").frame(),
            json!({"type":"moved","to":"cloud"})
        );
    }
}
