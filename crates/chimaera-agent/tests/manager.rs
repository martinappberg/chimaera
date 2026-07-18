//! Hermetic driver + registry tests against the scripted `fake-claude`
//! binary — the full pipeline (spawn → handshake → mapping → journal →
//! broadcast → hooks) with no network, auth, or billing. Protocol drift
//! against the REAL binaries is covered separately by `just chat-smoke`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use chimaera_agent::claude::ClaudeAdapter;
use chimaera_agent::driver::SpawnSpec;
use chimaera_agent::journal::SeqEvent;
use chimaera_agent::model::{
    AgentCommand, AgentEvent, BackgroundTask, ContentBlock, ToolStatus, UserMessageState,
};
use chimaera_agent::{ChatManager, CommandQueueFull, EventHook, ExitHook, RETAINED_SENDS_MAX};

const FAKE: &str = env!("CARGO_BIN_EXE_fake-claude");
const WAIT: Duration = Duration::from_secs(5);

struct Fixture {
    manager: Arc<ChatManager>,
    exits: mpsc::UnboundedReceiver<String>,
    _dir: tempfile::TempDir,
    cwd: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_path_buf();
    let (exit_tx, exits) = mpsc::unbounded_channel();
    let on_event: EventHook = Box::new(|_, _| {});
    let on_exit: ExitHook = Box::new(move |id, exit| {
        let _ = exit_tx.send(format!("{id}:{exit:?}"));
    });
    let manager = Arc::new(ChatManager::new(dir.path().join("chat"), on_event, on_exit));
    Fixture {
        manager,
        exits,
        _dir: dir,
        cwd,
    }
}

fn spec(id: &str, cwd: &Path, mode: &str) -> SpawnSpec {
    SpawnSpec::new(
        id,
        vec![FAKE.to_string(), mode.to_string()],
        cwd.to_path_buf(),
    )
}

/// Drain live events until the predicate matches; panics on timeout.
async fn wait_for(
    rx: &mut tokio::sync::broadcast::Receiver<Arc<SeqEvent>>,
    seen: &mut Vec<Arc<SeqEvent>>,
    what: &str,
    pred: impl Fn(&AgentEvent) -> bool,
) -> Arc<SeqEvent> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let entry = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}; saw {seen:#?}"))
            .expect("broadcast closed");
        seen.push(Arc::clone(&entry));
        if pred(&entry.ev) {
            return entry;
        }
    }
}

#[tokio::test]
async fn full_turn_with_permission_allow_and_gap_replay() {
    let fx = fixture();
    let info = fx
        .manager
        .spawn(&ClaudeAdapter, spec("s-1", &fx.cwd, "normal"))
        .expect("spawn");
    assert!(info.alive);
    assert_eq!(info.agent, "claude");

    let att = fx.manager.attach("s-1", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;

    // Handshake Init arrives without any user input (watchdog contract).
    if !seen.iter().any(|e| matches!(e.ev, AgentEvent::Init { .. })) {
        wait_for(&mut rx, &mut seen, "Init", |ev| {
            matches!(ev, AgentEvent::Init { .. })
        })
        .await;
    }

    fx.manager
        .command(
            "s-1",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "run it".into(),
                }],
            },
        )
        .await
        .expect("send");

    wait_for(
        &mut rx,
        &mut seen,
        "UserMessage",
        |ev| matches!(ev, AgentEvent::UserMessage { text, .. } if text == "run it"),
    )
    .await;
    wait_for(&mut rx, &mut seen, "TurnStarted", |ev| {
        matches!(ev, AgentEvent::TurnStarted { .. })
    })
    .await;
    // Second Init carries the native session id from system/init.
    wait_for(&mut rx, &mut seen, "Init with native id", |ev| {
        matches!(ev, AgentEvent::Init { native_session_id, .. } if native_session_id == "fake-native-1")
    })
    .await;
    // Deltas coalesce; the timer flush (100ms) or the tool_use flush must
    // surface the streamed text exactly once.
    wait_for(
        &mut rx,
        &mut seen,
        "MessageChunk 'hello'",
        |ev| matches!(ev, AgentEvent::MessageChunk { text, .. } if text == "hello"),
    )
    .await;
    wait_for(
        &mut rx,
        &mut seen,
        "ToolCall",
        |ev| matches!(ev, AgentEvent::ToolCall { id, .. } if id == "tu-1"),
    )
    .await;
    let permission = wait_for(&mut rx, &mut seen, "PermissionRequest", |ev| {
        matches!(ev, AgentEvent::PermissionRequest { .. })
    })
    .await;
    assert!(
        fx.manager.get("s-1").unwrap().pending_permission,
        "info tracks the outstanding permission"
    );

    fx.manager
        .command(
            "s-1",
            AgentCommand::Permission {
                request_id: match &permission.ev {
                    AgentEvent::PermissionRequest { request_id, .. } => request_id.clone(),
                    _ => unreachable!(),
                },
                option_id: "allow_once".into(),
                destination: None,
                feedback: None,
            },
        )
        .await
        .expect("permission");

    wait_for(&mut rx, &mut seen, "PermissionResolved", |ev| {
        matches!(ev, AgentEvent::PermissionResolved { .. })
    })
    .await;
    wait_for(&mut rx, &mut seen, "ToolCallUpdate completed", |ev| {
        matches!(
            ev,
            AgentEvent::ToolCallUpdate { id, status: ToolStatus::Completed, .. } if id == "tu-1"
        )
    })
    .await;
    let completed = wait_for(&mut rx, &mut seen, "TurnCompleted", |ev| {
        matches!(ev, AgentEvent::TurnCompleted { .. })
    })
    .await;
    match &completed.ev {
        AgentEvent::TurnCompleted { usage, .. } => {
            assert_eq!(usage.cost_usd, Some(0.01));
            assert_eq!(usage.output_tokens, 5);
        }
        _ => unreachable!(),
    }
    assert!(!fx.manager.get("s-1").unwrap().pending_permission);

    // Attach subscribes to `live` before snapshotting `replay`, so the live
    // tail may legitimately re-deliver an event already in replay (the
    // documented "dedupe by seq" contract). Apply that dedupe, then assert the
    // remaining stream is strictly increasing.
    let mut ordered: Vec<u64> = Vec::new();
    let mut max_seq = 0u64;
    for e in &seen {
        if e.seq > max_seq {
            ordered.push(e.seq);
            max_seq = e.seq;
        }
    }
    for pair in ordered.windows(2) {
        assert!(pair[1] > pair[0], "non-monotonic: {seen:#?}");
    }

    // Gap replay: a reconnect with last_seq = permission's seq must get
    // exactly the tail, starting right after it.
    let gap = fx.manager.attach("s-1", permission.seq).expect("reattach");
    assert_eq!(gap.replay.first().expect("tail").seq, permission.seq + 1);
    assert!(gap
        .replay
        .iter()
        .any(|e| matches!(e.ev, AgentEvent::TurnCompleted { .. })));

    // Native id landed in the resume index. The write is fire-and-forget (it
    // must never stall the pump), so poll for it rather than assume it's synchronous.
    let mut recorded = None;
    for _ in 0..100 {
        recorded = fx.manager.index().lookup("fake-native-1");
        if recorded.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(recorded.as_deref(), Some("s-1"));
}

/// `post_turn_summary` maps to `SessionStatus` and folds latest-wins into
/// `ChatInfo`: each turn's summary supersedes the last, and a NEW turn
/// clears the needs-action flag while the status line stays as context.
#[tokio::test]
async fn post_turn_summary_folds_latest_wins_session_status() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-st", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-st", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;

    // The fake's first summary carries the live-observed EMPTY needs_action
    // string (= nothing needed); the second a non-empty one — both
    // truthiness mappings covered.
    for (expected, expect_action) in [
        ("turn 1 reviewed, awaiting your look", false),
        ("turn 2 reviewed, awaiting your look", true),
    ] {
        send_text(&fx, "s-st", "run it").await;
        let permission = wait_for(&mut rx, &mut seen, "PermissionRequest", |ev| {
            matches!(ev, AgentEvent::PermissionRequest { .. })
        })
        .await;
        fx.manager
            .command(
                "s-st",
                AgentCommand::Permission {
                    request_id: match &permission.ev {
                        AgentEvent::PermissionRequest { request_id, .. } => request_id.clone(),
                        _ => unreachable!(),
                    },
                    option_id: "allow_once".into(),
                    destination: None,
                    feedback: None,
                },
            )
            .await
            .expect("permission");
        // The summary rides AFTER the result frame (live order) — the
        // TurnCompleted-then-SessionStatus sequence consumers rely on to
        // land attention state on top of the turn's own transition.
        wait_for(&mut rx, &mut seen, "TurnCompleted", |ev| {
            matches!(ev, AgentEvent::TurnCompleted { .. })
        })
        .await;
        let status = wait_for(&mut rx, &mut seen, "SessionStatus", |ev| {
            matches!(ev, AgentEvent::SessionStatus { .. })
        })
        .await;
        match &status.ev {
            AgentEvent::SessionStatus {
                category,
                detail,
                needs_action,
            } => {
                assert_eq!(category.as_deref(), Some("review_ready"));
                assert_eq!(detail, expected);
                assert_eq!(*needs_action, expect_action, "string truthiness maps");
            }
            _ => unreachable!(),
        }
        let info = fx.manager.get("s-st").expect("info");
        assert_eq!(info.status_detail.as_deref(), Some(expected));
        assert_eq!(info.status_category.as_deref(), Some("review_ready"));
        assert_eq!(info.status_needs_action, expect_action);
    }

    // A new turn means the user acted: the flag clears, the line stays.
    send_text(&fx, "s-st", "one more").await;
    wait_for(&mut rx, &mut seen, "TurnStarted", |ev| {
        matches!(ev, AgentEvent::TurnStarted { .. })
    })
    .await;
    let info = fx.manager.get("s-st").expect("info");
    assert!(!info.status_needs_action, "TurnStarted clears needs_action");
    assert_eq!(
        info.status_detail.as_deref(),
        Some("turn 2 reviewed, awaiting your look"),
        "the status line survives as context until superseded"
    );
}

/// A new driver process must neutralize background state left at the journal
/// tail by an old process that could not drain (SIGKILL / power loss).
#[tokio::test]
async fn spawn_neutralizes_stale_background_set_in_reused_journal() {
    let fx = fixture();
    fx.manager
        .seed_journal(
            "s-bg-restart",
            &[AgentEvent::BackgroundTasks {
                tasks: vec![BackgroundTask {
                    id: "dead-bg".into(),
                    task_type: "local_agent".into(),
                    description: "wait for smoke test to finish".into(),
                    status: "running".into(),
                    started_at_ms: 1,
                    workflow_name: None,
                    agents: Vec::new(),
                    agents_total: 0,
                    agents_done: 0,
                    tool_use_id: None,
                }],
                closed: Vec::new(),
            }],
        )
        .expect("seed crash-tailed journal");

    // A daemon crash cannot run drain_pending. Resurrecting the same session
    // id therefore reuses the stale journal, but starts a new driver process
    // whose honest background set is empty.
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-bg-restart", &fx.cwd, "normal"))
        .expect("respawn");
    // The old client already applied seq 1 before the crash. Its reconnect
    // asks only for the gap, which the newly-opened journal serves from its
    // in-memory ring (the same path a real live pane takes).
    let att = fx.manager.attach("s-bg-restart", 1).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    let reset =
        match seen.iter().find(|e| {
            matches!(
                e.ev,
                AgentEvent::BackgroundTasks { ref tasks, .. } if tasks.is_empty()
            )
        }) {
            Some(entry) => Arc::clone(entry),
            None => wait_for(
                &mut rx,
                &mut seen,
                "process-start background reset",
                |ev| matches!(ev, AgentEvent::BackgroundTasks { tasks, .. } if tasks.is_empty()),
            )
            .await,
        };
    let init = match seen
        .iter()
        .find(|e| matches!(e.ev, AgentEvent::Init { .. }))
    {
        Some(entry) => Arc::clone(entry),
        None => {
            wait_for(&mut rx, &mut seen, "Init after reset", |ev| {
                matches!(ev, AgentEvent::Init { .. })
            })
            .await
        }
    };
    assert!(
        reset.seq < init.seq,
        "the lifecycle reset must precede every new-driver event"
    );

    let replay = fx.manager.attach("s-bg-restart", 1).expect("replay").replay;
    let last_set = replay.iter().rev().find_map(|entry| match &entry.ev {
        AgentEvent::BackgroundTasks { tasks, closed } => Some((tasks, closed)),
        _ => None,
    });
    let (tasks, closed) = last_set.expect("background level-set in replay");
    assert!(tasks.is_empty(), "the dead task must not survive replay");
    assert!(
        closed.is_empty(),
        "a process boundary is not a fabricated task verdict"
    );
}

/// Background work is CROSS-TURN: a task started mid-turn is still in the live
/// set after the turn ends, and a second turn adds to it rather than replacing
/// it. That outliving is what every "still working off-screen" cue is gated on
/// (the dashboard card's pulsing dot, the rail glyph's muted breathing) — if
/// the set emptied at the turn boundary they would all read "finished".
#[tokio::test]
async fn background_tasks_outlive_their_turn_and_accumulate() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-bg", &fx.cwd, "background"))
        .expect("spawn");
    let att = fx.manager.attach("s-bg", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;

    // Each turn backgrounds one task, then ends — so after turn N the set
    // holds N running tasks with the turn itself idle.
    for expected in 1..=2usize {
        send_text(&fx, "s-bg", "background this").await;
        let ev = wait_for(
            &mut rx,
            &mut seen,
            "BackgroundTasks",
            |ev| matches!(ev, AgentEvent::BackgroundTasks { tasks, .. } if tasks.len() == expected),
        )
        .await;
        match &ev.ev {
            AgentEvent::BackgroundTasks { tasks, .. } => {
                assert!(
                    tasks.iter().all(|t| t.status == "running"),
                    "the level-set carries only live work: {tasks:#?}"
                );
                assert_eq!(tasks[0].task_type, "local_bash", "the background lane");
            }
            _ => unreachable!(),
        }
        wait_for(&mut rx, &mut seen, "TurnCompleted", |ev| {
            matches!(ev, AgentEvent::TurnCompleted { .. })
        })
        .await;
    }

    // The set survived BOTH turn boundaries — nothing after TurnCompleted
    // shrank it back.
    let last = seen
        .iter()
        .rev()
        .find_map(|e| match &e.ev {
            AgentEvent::BackgroundTasks { tasks, .. } => Some(tasks.clone()),
            _ => None,
        })
        .expect("a background set");
    assert_eq!(last.len(), 2, "turn two ADDED to the set: {last:#?}");

    // And the pump folded that level-set to a COUNT on ChatInfo — the whole
    // point of the fold: readable without attaching a socket, which is how
    // the session row (and so the rail) learns about off-screen work at all.
    let info = fx.manager.get("s-bg").expect("info");
    assert_eq!(info.background_running, 2);

    // The tasks were the CLI's children — they die with it, and the row must
    // say so. (Either mechanism satisfies this: claude's teardown journals an
    // empty level-set, and the pump zeroes on Exited for drivers that don't.
    // The row-level guarantee is what matters here, not which one fired.)
    assert!(fx.manager.kill("s-bg"));
    wait_for(&mut rx, &mut seen, "Exited", |ev| {
        matches!(ev, AgentEvent::Exited { .. })
    })
    .await;
    let info = fx.manager.get("s-bg").expect("info");
    assert_eq!(info.background_running, 0, "an exit clears the count");
    assert!(!info.alive);
}

#[tokio::test]
async fn permission_deny_marks_tool_failed() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-2", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-2", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    fx.manager
        .command(
            "s-2",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text { text: "go".into() }],
            },
        )
        .await
        .expect("send");
    let permission = wait_for(&mut rx, &mut seen, "PermissionRequest", |ev| {
        matches!(ev, AgentEvent::PermissionRequest { .. })
    })
    .await;
    fx.manager
        .command(
            "s-2",
            AgentCommand::Permission {
                request_id: match &permission.ev {
                    AgentEvent::PermissionRequest { request_id, .. } => request_id.clone(),
                    _ => unreachable!(),
                },
                option_id: "reject_once".into(),
                destination: None,
                feedback: None,
            },
        )
        .await
        .expect("deny");

    wait_for(&mut rx, &mut seen, "ToolCallUpdate failed", |ev| {
        matches!(
            ev,
            AgentEvent::ToolCallUpdate {
                status: ToolStatus::Failed,
                ..
            }
        )
    })
    .await;
    // The deny sends interrupt:true, which aborts the turn on the real CLI —
    // the hermetic fake now mirrors that (is_error result → TurnAborted),
    // instead of the TurnCompleted the old success-result deny produced.
    wait_for(&mut rx, &mut seen, "TurnAborted", |ev| {
        matches!(ev, AgentEvent::TurnAborted { .. })
    })
    .await;
}

/// Send a text message into a session.
async fn send_text(fx: &Fixture, id: &str, text: &str) {
    fx.manager
        .command(
            id,
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text { text: text.into() }],
            },
        )
        .await
        .expect("send");
}

/// Drive a session to the mid-turn point (permission outstanding), then send
/// a second message that the CLI queues. Returns the queued message's
/// delivery id and the outstanding permission's request id.
async fn queue_second_send(
    fx: &Fixture,
    id: &str,
    rx: &mut tokio::sync::broadcast::Receiver<Arc<SeqEvent>>,
    seen: &mut Vec<Arc<SeqEvent>>,
) -> (String, String) {
    send_text(fx, id, "first").await;
    let permission = wait_for(rx, seen, "PermissionRequest", |ev| {
        matches!(ev, AgentEvent::PermissionRequest { .. })
    })
    .await;
    let request_id = match &permission.ev {
        AgentEvent::PermissionRequest { request_id, .. } => request_id.clone(),
        _ => unreachable!(),
    };

    // Turn one is mid-flight: this send queues on the (fake) CLI and must
    // echo as queued with a delivery id.
    send_text(fx, id, "second").await;
    let queued = wait_for(
        rx,
        seen,
        "queued UserMessage",
        |ev| matches!(ev, AgentEvent::UserMessage { text, queued: true, .. } if text == "second"),
    )
    .await;
    let queued_id = match &queued.ev {
        AgentEvent::UserMessage { id: Some(id), .. } => id.clone(),
        _ => unreachable!(),
    };
    (queued_id, request_id)
}

/// A mid-turn send echoes queued and is HELD; when the running turn's result
/// lands it resolves `sent` (in one step) and is only then written to the CLI,
/// where it runs as its own follow-up turn. The journal replays the pair (one
/// message, one update) so a reducer renders one bubble in its final state.
#[tokio::test]
async fn queued_send_resolves_sent_and_replays_once() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-q1", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-q1", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    let (queued_id, request_id) = queue_second_send(&fx, "s-q1", &mut rx, &mut seen).await;

    fx.manager
        .command(
            "s-q1",
            AgentCommand::Permission {
                request_id,
                option_id: "allow_once".into(),
                destination: None,
                feedback: None,
            },
        )
        .await
        .expect("permission");

    // Turn one finishes; the held message resolves sent AND is flushed to the
    // CLI now (never mid-turn), opening its own follow-up turn t2.
    wait_for(
        &mut rx,
        &mut seen,
        "TurnCompleted",
        |ev| matches!(ev, AgentEvent::TurnCompleted { turn_id, .. } if turn_id == "t1"),
    )
    .await;
    wait_for(
        &mut rx,
        &mut seen,
        "UserMessageUpdate sent",
        |ev| matches!(ev, AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if *id == queued_id),
    )
    .await;
    wait_for(
        &mut rx,
        &mut seen,
        "queued turn's TurnStarted",
        |ev| matches!(ev, AgentEvent::TurnStarted { turn_id } if turn_id == "t2"),
    )
    .await;
    // t2 is a real turn (the flushed message ran fresh): it makes its own tool
    // call and asks permission. Answer it so the turn completes.
    let t2_perm = wait_for(&mut rx, &mut seen, "t2 PermissionRequest", |ev| {
        matches!(ev, AgentEvent::PermissionRequest { .. })
    })
    .await;
    let t2_request_id = match &t2_perm.ev {
        AgentEvent::PermissionRequest { request_id, .. } => request_id.clone(),
        _ => unreachable!(),
    };
    fx.manager
        .command(
            "s-q1",
            AgentCommand::Permission {
                request_id: t2_request_id,
                option_id: "allow_once".into(),
                destination: None,
                feedback: None,
            },
        )
        .await
        .expect("t2 permission");
    wait_for(
        &mut rx,
        &mut seen,
        "queued turn's TurnCompleted",
        |ev| matches!(ev, AgentEvent::TurnCompleted { turn_id, .. } if turn_id == "t2"),
    )
    .await;

    // Journal replay carries the queued echo + its resolution exactly once:
    // a reducer folding it renders one bubble in its final `sent` state.
    let replay = fx.manager.attach("s-q1", 0).expect("replay").replay;
    let echoes = replay
        .iter()
        .filter(|e| matches!(&e.ev, AgentEvent::UserMessage { text, .. } if text == "second"))
        .count();
    assert_eq!(echoes, 1, "queued-then-sent appears exactly once");
    let updates: Vec<_> = replay
        .iter()
        .filter_map(|e| match &e.ev {
            AgentEvent::UserMessageUpdate { id, state } if *id == queued_id => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(updates, vec![UserMessageState::Sent]);
}

/// The daemon-side guarantee (the "tab hidden" case): a queued send is flushed
/// and resolved `sent` even with NO client attached. The flush fires off the
/// CLI's turn-end result INSIDE the driver — never on a UI event or client
/// timer — so detaching every client after queuing cannot stall it. Queue a
/// message, DROP the only receiver (the tab closes), answer the turn purely
/// through the manager (no attachment needed), then re-attach and confirm the
/// journal recorded the delivery.
#[tokio::test]
async fn queued_send_flushes_with_no_client_attached() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-hidden", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-hidden", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    let (queued_id, request_id) = queue_second_send(&fx, "s-hidden", &mut rx, &mut seen).await;

    // The tab goes away: drop the only client receiver. The daemon session and
    // its driver keep running — windows are just views onto the daemon.
    drop(rx);

    // Answer turn one purely through the manager — no attachment required. Its
    // result lands in the driver and flushes the held send server-side.
    fx.manager
        .command(
            "s-hidden",
            AgentCommand::Permission {
                request_id,
                option_id: "allow_once".into(),
                destination: None,
                feedback: None,
            },
        )
        .await
        .expect("permission");

    // Poll the journal (re-attach) until the held send resolves `sent` — proof
    // the flush was journaled with nobody listening.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let replay = fx.manager.attach("s-hidden", 0).expect("replay").replay;
        let sent = replay.iter().any(|e| {
            matches!(&e.ev,
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if *id == queued_id)
        });
        if sent {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "held send never resolved sent with no client attached"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    fx.manager.kill("s-hidden");
}

/// A user interrupt aborts ONLY the running turn (structurally marked
/// `interrupted`) — the queued message SURVIVES the stop: it flushes right
/// after the abort, runs as its own turn, and replays in its `sent` state.
#[tokio::test]
async fn interrupt_classifies_user_stop_and_queue_still_delivers() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-q2", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-q2", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    let (queued_id, _) = queue_second_send(&fx, "s-q2", &mut rx, &mut seen).await;

    fx.manager
        .command("s-q2", AgentCommand::Interrupt)
        .await
        .expect("interrupt");

    // The abort is a quiet user stop, not a failure — the fake omits the
    // result string, so this proves the structural flag, not a string
    // heuristic.
    let aborted = wait_for(&mut rx, &mut seen, "TurnAborted", |ev| {
        matches!(ev, AgentEvent::TurnAborted { .. })
    })
    .await;
    match &aborted.ev {
        AgentEvent::TurnAborted {
            interrupted,
            reason,
            ..
        } => {
            assert!(interrupted, "user stop carries the structural flag");
            assert_eq!(reason, "interrupted");
        }
        _ => unreachable!(),
    }
    // The stop ended only that turn: the held message flushes `sent`…
    wait_for(
        &mut rx,
        &mut seen,
        "UserMessageUpdate sent",
        |ev| matches!(ev, AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if *id == queued_id),
    )
    .await;
    // …and runs as its own turn against the (fake) CLI. Answer its
    // permission so the session settles idle.
    let perm = wait_for(
        &mut rx,
        &mut seen,
        "flushed turn's PermissionRequest",
        |ev| matches!(ev, AgentEvent::PermissionRequest { .. }),
    )
    .await;
    let request_id = match &perm.ev {
        AgentEvent::PermissionRequest { request_id, .. } => request_id.clone(),
        _ => unreachable!(),
    };
    fx.manager
        .command(
            "s-q2",
            AgentCommand::Permission {
                request_id,
                option_id: "allow_once".into(),
                destination: None,
                feedback: None,
            },
        )
        .await
        .expect("permission");
    wait_for(&mut rx, &mut seen, "flushed turn's TurnCompleted", |ev| {
        matches!(ev, AgentEvent::TurnCompleted { .. })
    })
    .await;

    // Replay: the queued message is echoed exactly once and ends `sent` —
    // never dropped — and the user-stop classification survives.
    let replay = fx.manager.attach("s-q2", 0).expect("replay").replay;
    let echoes = replay
        .iter()
        .filter(|e| matches!(&e.ev, AgentEvent::UserMessage { text, .. } if text == "second"))
        .count();
    assert_eq!(echoes, 1);
    let updates: Vec<_> = replay
        .iter()
        .filter_map(|e| match &e.ev {
            AgentEvent::UserMessageUpdate { id, state } if *id == queued_id => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(updates, vec![UserMessageState::Sent]);
    assert!(
        replay.iter().any(|e| matches!(
            &e.ev,
            AgentEvent::TurnAborted {
                interrupted: true,
                ..
            }
        )),
        "the user-stop classification survives replay"
    );
    // Every opened turn ended — the stop left nothing dangling.
    let (opened, ended) = turn_balance(&replay);
    assert_eq!(opened, ended, "no turn left stuck running: {replay:#?}");
}

/// Opened vs ended turns in a journal — a session is idle only when they
/// balance (every TurnStarted has a matching TurnCompleted/TurnAborted). A
/// dangling open turn is exactly the "stuck running" state.
fn turn_balance(replay: &[Arc<SeqEvent>]) -> (usize, usize) {
    let opened = replay
        .iter()
        .filter(|e| matches!(e.ev, AgentEvent::TurnStarted { .. }))
        .count();
    let ended = replay
        .iter()
        .filter(|e| {
            matches!(
                e.ev,
                AgentEvent::TurnCompleted { .. } | AgentEvent::TurnAborted { .. }
            )
        })
        .count();
    (opened, ended)
}

/// The user's real scenario: SEVERAL messages queued behind a running turn.
/// Each is HELD, then flushed together when the turn ends — every one resolves
/// `sent` exactly once (none stranded "queued"/"not delivered"), and the whole
/// journal balances (every opened turn ends). This is the regression the
/// hold-until-flush model exists to kill: the old eager-dump + FIFO-pop guess
/// could strand a middle message and mint a phantom turn.
#[tokio::test]
async fn several_held_sends_all_resolve_sent_and_none_strand() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-co", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-co", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    // Turn one parks on a permission…
    send_text(&fx, "s-co", "first").await;
    let permission = wait_for(&mut rx, &mut seen, "PermissionRequest", |ev| {
        matches!(ev, AgentEvent::PermissionRequest { .. })
    })
    .await;
    let request_id = match &permission.ev {
        AgentEvent::PermissionRequest { request_id, .. } => request_id.clone(),
        _ => unreachable!(),
    };
    // …while TWO messages queue behind it (both HELD, never dumped mid-turn).
    let mut queued_ids = Vec::new();
    for text in ["second", "third"] {
        send_text(&fx, "s-co", text).await;
        let ev = wait_for(
            &mut rx,
            &mut seen,
            "queued UserMessage",
            |ev| matches!(ev, AgentEvent::UserMessage { text: t, queued: true, .. } if t == text),
        )
        .await;
        match &ev.ev {
            AgentEvent::UserMessage { id: Some(id), .. } => queued_ids.push(id.clone()),
            _ => unreachable!(),
        }
    }

    // Allow turn one: it completes, then BOTH held sends flush — resolving sent
    // and running as their own turns (the CLI queues the second behind the
    // first). Answer each turn's permission so the session settles idle.
    fx.manager
        .command(
            "s-co",
            AgentCommand::Permission {
                request_id,
                option_id: "allow_once".into(),
                destination: None,
                feedback: None,
            },
        )
        .await
        .expect("permission");

    // Two flushed turns follow (t2, t3); each makes a tool call and asks — allow
    // both. (A generous cap: we answer every permission we see until the last
    // send is sent and both follow-up turns have ended.)
    let mut answered = 0;
    let mut sent = std::collections::HashSet::new();
    let mut ended_after_t1 = 0;
    while sent.len() < queued_ids.len() || ended_after_t1 < 2 {
        let ev = wait_for(&mut rx, &mut seen, "flush progress", |ev| {
            matches!(
                ev,
                AgentEvent::PermissionRequest { .. }
                    | AgentEvent::UserMessageUpdate {
                        state: UserMessageState::Sent,
                        ..
                    }
                    | AgentEvent::TurnCompleted { .. }
            )
        })
        .await;
        match &ev.ev {
            AgentEvent::PermissionRequest { request_id, .. } => {
                answered += 1;
                assert!(answered <= 8, "runaway permission loop");
                fx.manager
                    .command(
                        "s-co",
                        AgentCommand::Permission {
                            request_id: request_id.clone(),
                            option_id: "allow_once".into(),
                            destination: None,
                            feedback: None,
                        },
                    )
                    .await
                    .expect("permission");
            }
            AgentEvent::UserMessageUpdate {
                id,
                state: UserMessageState::Sent,
            } => {
                sent.insert(id.clone());
            }
            AgentEvent::TurnCompleted { turn_id, .. } if turn_id != "t1" => {
                ended_after_t1 += 1;
            }
            _ => {}
        }
    }

    let replay = fx.manager.attach("s-co", 0).expect("replay").replay;
    // Every queued message resolved `sent` exactly once — none stranded, none
    // dropped, none resolved twice.
    for id in &queued_ids {
        let states: Vec<_> = replay
            .iter()
            .filter_map(|e| match &e.ev {
                AgentEvent::UserMessageUpdate { id: uid, state } if uid == id => Some(*state),
                _ => None,
            })
            .collect();
        assert_eq!(
            states,
            vec![UserMessageState::Sent],
            "held message {id} resolves sent exactly once"
        );
    }
    // The journal balances: no dangling open turn (no "stuck running").
    let (opened, ended) = turn_balance(&replay);
    assert_eq!(
        opened, ended,
        "opened turns must equal ended turns (idle): {replay:#?}"
    );
}

/// The interrupt watchdog recovers a wedged turn: the fake opens a turn, never
/// ends it, and acks the interrupt with NO result. Without the watchdog the
/// session would stay "running" forever; with it, a `TurnAborted{interrupted}`
/// lands once the grace expires — the user's escape hatch.
#[tokio::test]
async fn interrupt_recovers_a_hung_turn_via_watchdog() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-hang", &fx.cwd, "hang"))
        .expect("spawn");
    let att = fx.manager.attach("s-hang", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    send_text(&fx, "s-hang", "go").await;
    // The turn opens and streams content, then hangs (no result ever).
    wait_for(&mut rx, &mut seen, "TurnStarted", |ev| {
        matches!(ev, AgentEvent::TurnStarted { .. })
    })
    .await;

    fx.manager
        .command("s-hang", AgentCommand::Interrupt)
        .await
        .expect("interrupt");

    // The CLI (fake) acks the interrupt but sends no result — the watchdog is
    // the only thing that can end the turn. It fires after the grace (~1.5s).
    let aborted = wait_for(&mut rx, &mut seen, "watchdog TurnAborted", |ev| {
        matches!(ev, AgentEvent::TurnAborted { .. })
    })
    .await;
    match &aborted.ev {
        AgentEvent::TurnAborted { interrupted, .. } => {
            assert!(interrupted, "the watchdog abort is a structural user stop");
        }
        _ => unreachable!(),
    }

    // The recovered session is idle: opened turns balance ended turns.
    let replay = fx.manager.attach("s-hang", 0).expect("replay").replay;
    let (opened, ended) = turn_balance(&replay);
    assert_eq!(
        (opened, ended),
        (1, 1),
        "the hung turn is closed exactly once: {:#?}",
        replay
    );
}

#[tokio::test]
async fn repeated_valid_sends_hit_the_shared_retained_queue_budget() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-budget", &fx.cwd, "hang"))
        .expect("spawn");
    let att = fx.manager.attach("s-budget", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    send_text(&fx, "s-budget", "hold this turn open").await;
    wait_for(&mut rx, &mut seen, "TurnStarted", |ev| {
        matches!(ev, AgentEvent::TurnStarted { .. })
    })
    .await;

    // Empty sends are individually valid and consume essentially no payload
    // bytes. They exercise the item dimension that prevents an authenticated
    // client growing either driver's pending VecDeque without bound.
    for _ in 0..RETAINED_SENDS_MAX {
        fx.manager
            .command("s-budget", AgentCommand::Send { blocks: vec![] })
            .await
            .expect("send within retained-item budget");
    }
    let err = fx
        .manager
        .command("s-budget", AgentCommand::Send { blocks: vec![] })
        .await
        .expect_err("one more retained send must be refused");
    assert!(
        err.downcast_ref::<CommandQueueFull>().is_some(),
        "refusal stays distinguishable from driver death: {err:#}"
    );
    assert!(fx.manager.kill("s-budget"));
}

/// Feature 2 — cancelling a still-held message un-queues it: the driver emits
/// `Cancelled` (not sent/dropped) with no CLI round-trip (the message was never
/// written), and it resolves exactly once as cancelled on replay.
#[tokio::test]
async fn cancel_queued_removes_a_still_queued_message() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-cx", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-cx", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    let (queued_id, request_id) = queue_second_send(&fx, "s-cx", &mut rx, &mut seen).await;

    // Pull the queued message back BEFORE the running turn finishes.
    fx.manager
        .command(
            "s-cx",
            AgentCommand::CancelQueued {
                id: queued_id.clone(),
            },
        )
        .await
        .expect("cancel");
    wait_for(
        &mut rx,
        &mut seen,
        "UserMessageUpdate cancelled",
        |ev| matches!(ev, AgentEvent::UserMessageUpdate { id, state: UserMessageState::Cancelled } if *id == queued_id),
    )
    .await;

    // Finish turn one. The cancelled message was held, so cancelling simply
    // dropped it before the flush — it is never written to the CLI, no
    // follow-up turn runs for it, and no `sent` ever lands for that id.
    fx.manager
        .command(
            "s-cx",
            AgentCommand::Permission {
                request_id,
                option_id: "allow_once".into(),
                destination: None,
                feedback: None,
            },
        )
        .await
        .expect("permission");
    wait_for(
        &mut rx,
        &mut seen,
        "turn one TurnCompleted",
        |ev| matches!(ev, AgentEvent::TurnCompleted { turn_id, .. } if turn_id == "t1"),
    )
    .await;

    // Replay: the cancelled message is echoed once and resolves ONLY cancelled
    // (a reducer folds the pair to nothing — the bubble vanishes).
    let replay = fx.manager.attach("s-cx", 0).expect("replay").replay;
    let echoes = replay
        .iter()
        .filter(|e| matches!(&e.ev, AgentEvent::UserMessage { text, .. } if text == "second"))
        .count();
    assert_eq!(echoes, 1, "the cancelled message is echoed exactly once");
    let updates: Vec<_> = replay
        .iter()
        .filter_map(|e| match &e.ev {
            AgentEvent::UserMessageUpdate { id, state } if *id == queued_id => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(
        updates,
        vec![UserMessageState::Cancelled],
        "a cancelled message resolves cancelled, never sent/dropped"
    );
    // No phantom turn ran for the un-queued message.
    assert!(
        !replay
            .iter()
            .any(|e| matches!(&e.ev, AgentEvent::TurnStarted { turn_id } if turn_id == "t2")),
        "the un-queued message opened no turn"
    );
}

/// Feature 2 — cancelling a message that already resolved emits the tombstone
/// `Cancelled`. For an already-`sent` id the reducer no-ops (the delivered
/// message stays in the transcript, live and on replay — `sent` precedes the
/// tombstone in seq order); the same event is what dismisses a dropped
/// "not delivered" bubble.
#[tokio::test]
async fn cancel_queued_after_delivery_is_a_reducer_noop_tombstone() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-cn", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-cn", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    let (queued_id, request_id) = queue_second_send(&fx, "s-cn", &mut rx, &mut seen).await;

    // Let turn one finish so the queued message flushes `sent` (delivered).
    fx.manager
        .command(
            "s-cn",
            AgentCommand::Permission {
                request_id,
                option_id: "allow_once".into(),
                destination: None,
                feedback: None,
            },
        )
        .await
        .expect("permission");
    wait_for(
        &mut rx,
        &mut seen,
        "UserMessageUpdate sent",
        |ev| matches!(ev, AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if *id == queued_id),
    )
    .await;

    // Now cancel it — too late to matter: the tombstone `Cancelled` lands
    // AFTER the `sent` in seq order, so a reducer folding the journal keeps
    // the delivered message.
    fx.manager
        .command(
            "s-cn",
            AgentCommand::CancelQueued {
                id: queued_id.clone(),
            },
        )
        .await
        .expect("cancel");
    wait_for(
        &mut rx,
        &mut seen,
        "tombstone Cancelled",
        |ev| matches!(ev, AgentEvent::UserMessageUpdate { id, state: UserMessageState::Cancelled } if *id == queued_id),
    )
    .await;

    let replay = fx.manager.attach("s-cn", 0).expect("replay").replay;
    let updates: Vec<_> = replay
        .iter()
        .filter_map(|e| match &e.ev {
            AgentEvent::UserMessageUpdate { id, state } if *id == queued_id => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(
        updates,
        vec![UserMessageState::Sent, UserMessageState::Cancelled],
        "sent precedes the tombstone on replay, so the delivery wins"
    );
}

#[tokio::test]
async fn permission_deny_with_feedback_continues_turn() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-2f", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-2f", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    fx.manager
        .command(
            "s-2f",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text { text: "go".into() }],
            },
        )
        .await
        .expect("send");
    let permission = wait_for(&mut rx, &mut seen, "PermissionRequest", |ev| {
        matches!(ev, AgentEvent::PermissionRequest { .. })
    })
    .await;
    fx.manager
        .command(
            "s-2f",
            AgentCommand::Permission {
                request_id: match &permission.ev {
                    AgentEvent::PermissionRequest { request_id, .. } => request_id.clone(),
                    _ => unreachable!(),
                },
                option_id: "reject_once".into(),
                destination: None,
                feedback: Some("try a dry run first".into()),
            },
        )
        .await
        .expect("deny with feedback");

    // The reason the model received is journaled as a user message…
    wait_for(
        &mut rx,
        &mut seen,
        "UserMessage feedback",
        |ev| matches!(ev, AgentEvent::UserMessage { text, .. } if text == "try a dry run first"),
    )
    .await;
    wait_for(&mut rx, &mut seen, "ToolCallUpdate failed", |ev| {
        matches!(
            ev,
            AgentEvent::ToolCallUpdate {
                status: ToolStatus::Failed,
                ..
            }
        )
    })
    .await;
    // …and interrupt:false keeps the turn alive to a normal completion
    // (the bare deny's TurnAborted path must NOT fire).
    let completed = wait_for(&mut rx, &mut seen, "TurnCompleted", |ev| {
        matches!(ev, AgentEvent::TurnCompleted { .. })
    })
    .await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e.ev, AgentEvent::TurnAborted { .. })),
        "feedback denial must not abort: {seen:#?}"
    );
    assert!(completed.seq > permission.seq);
}

#[tokio::test]
async fn handshake_failure_is_classified_for_degrade() {
    let mut fx = fixture();
    let mut spec = spec("s-3", &fx.cwd, "silent");
    spec.handshake_timeout = Duration::from_millis(300);
    fx.manager.spawn(&ClaudeAdapter, spec).expect("spawn");

    let exit = tokio::time::timeout(WAIT, fx.exits.recv())
        .await
        .expect("exit hook fired")
        .expect("channel open");
    assert!(
        exit.starts_with("s-3:HandshakeFailed"),
        "expected handshake failure, got {exit}"
    );
    assert!(!fx.manager.get("s-3").unwrap().alive);
}

#[tokio::test]
async fn spawn_crash_reports_handshake_failure_with_stderr() {
    let mut fx = fixture();
    let mut spec = spec("s-4", &fx.cwd, "die");
    spec.handshake_timeout = Duration::from_secs(5);
    fx.manager.spawn(&ClaudeAdapter, spec).expect("spawn");

    let exit = tokio::time::timeout(WAIT, fx.exits.recv())
        .await
        .expect("exit hook fired")
        .expect("channel open");
    assert!(
        exit.starts_with("s-4:HandshakeFailed"),
        "expected handshake failure, got {exit}"
    );
}

/// The journaled face of a startup failure: a fatal `Error` (with the reason)
/// followed by `Exited`. Returns the Error message for content asserts.
fn journaled_startup_failure(replay: &[Arc<SeqEvent>]) -> String {
    let error_at = replay
        .iter()
        .position(|e| matches!(&e.ev, AgentEvent::Error { fatal: true, .. }))
        .unwrap_or_else(|| panic!("no fatal Error journaled; got {replay:#?}"));
    assert!(
        replay[error_at + 1..]
            .iter()
            .any(|e| matches!(e.ev, AgentEvent::Exited { .. })),
        "no Exited after the fatal Error; got {replay:#?}"
    );
    match &replay[error_at].ev {
        AgentEvent::Error { message, .. } => message.clone(),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn handshake_death_journals_a_visible_startup_failure() {
    // `die` exits before answering the handshake — previously nothing reached
    // the journal and an attached pane just showed "agent exited".
    let mut fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-6", &fx.cwd, "die"))
        .expect("spawn");
    let exit = tokio::time::timeout(WAIT, fx.exits.recv())
        .await
        .expect("exit hook fired")
        .expect("channel open");
    assert!(exit.starts_with("s-6:HandshakeFailed"), "got {exit}");

    // The failure is journaled, so replay (a reattach) renders it.
    let att = fx.manager.attach("s-6", 0).expect("attach");
    let message = journaled_startup_failure(&att.replay);
    assert!(
        message.contains("claude failed to start"),
        "message names the agent and the failure: {message}"
    );
}

#[tokio::test]
async fn spawn_failure_journals_a_visible_startup_failure() {
    // argv[0] does not exist: the earliest possible death (JsonlChild::spawn
    // errors before there is a child at all).
    let mut fx = fixture();
    fx.manager
        .spawn(
            &ClaudeAdapter,
            SpawnSpec::new(
                "s-7",
                vec!["/nonexistent/chimaera-fake-agent".to_string()],
                fx.cwd.clone(),
            ),
        )
        .expect("spawn");
    let exit = tokio::time::timeout(WAIT, fx.exits.recv())
        .await
        .expect("exit hook fired")
        .expect("channel open");
    assert!(exit.starts_with("s-7:HandshakeFailed"), "got {exit}");

    let att = fx.manager.attach("s-7", 0).expect("attach");
    let message = journaled_startup_failure(&att.replay);
    assert!(message.contains("spawn failed"), "got {message}");
}

#[tokio::test]
async fn exit_right_after_handshake_is_failure_at_birth_with_stderr() {
    // Handshake succeeds, then the child dies (the post-update codex mode).
    // Previously classified Clean and silently retired, stderr discarded.
    let mut fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-8", &fx.cwd, "die-after-handshake"))
        .expect("spawn");
    let exit = tokio::time::timeout(WAIT, fx.exits.recv())
        .await
        .expect("exit hook fired")
        .expect("channel open");
    assert!(
        exit.starts_with("s-8:HandshakeFailed"),
        "an exit-at-birth must classify as a startup failure, got {exit}"
    );
    assert!(
        exit.contains("kaboom"),
        "the stderr diagnostic must be preserved on the exit: {exit}"
    );

    let att = fx.manager.attach("s-8", 0).expect("attach");
    let message = journaled_startup_failure(&att.replay);
    assert!(
        message.contains("kaboom"),
        "the stderr diagnostic must reach the journal: {message}"
    );
}

/// A binary whose server-probed `--version` differs from the driver's tested
/// pin is warn-not-block, and the warning is a DAEMON LOG LINE only: the
/// session lives, the version is journaled on Init so a later misbehavior is
/// already diagnosed, and NO drift chatter reaches the user-facing event
/// stream (unparsed frames already degrade visibly on their own). Neither
/// wire protocol carries a reliable version, so the value rides
/// `SpawnSpec::agent_version`.
#[tokio::test]
async fn version_drift_is_nonfatal_and_never_reaches_the_stream() {
    let fx = fixture();
    let mut spec = spec("s-9", &fx.cwd, "normal");
    spec.agent_version = Some("9.9.9-fake (Claude Code)".into());
    fx.manager.spawn(&ClaudeAdapter, spec).expect("spawn");

    let att = fx.manager.attach("s-9", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    let init = wait_for(&mut rx, &mut seen, "Init", |ev| {
        matches!(ev, AgentEvent::Init { .. })
    })
    .await;
    match &init.ev {
        AgentEvent::Init { agent_version, .. } => assert_eq!(
            agent_version.as_deref(),
            Some("9.9.9-fake (Claude Code)"),
            "the probed version is journaled on Init"
        ),
        _ => unreachable!(),
    }

    // The drift warning (when raised) would land right after Init — strictly
    // before the Send's UserMessage. A UserMessage with no preceding drift
    // Notice proves none reached the stream.
    fx.manager
        .command(
            "s-9",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text { text: "go".into() }],
            },
        )
        .await
        .expect("send");
    wait_for(&mut rx, &mut seen, "UserMessage", |ev| {
        matches!(ev, AgentEvent::UserMessage { .. })
    })
    .await;

    assert!(
        !seen.iter().any(
            |e| matches!(&e.ev, AgentEvent::Notice { text } if text.contains("verified against"))
        ),
        "version drift must never surface in the event stream; saw {seen:#?}"
    );
    // Warn, don't block: the drift never kills the session.
    assert!(
        fx.manager.get("s-9").unwrap().alive,
        "version drift must not kill the session"
    );
}

#[tokio::test]
async fn answered_question_carries_answers_and_replays() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-q1", &fx.cwd, "question"))
        .expect("spawn");
    let att = fx.manager.attach("s-q1", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    fx.manager
        .command(
            "s-q1",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "pick one".into(),
                }],
            },
        )
        .await
        .expect("send");
    let request = wait_for(&mut rx, &mut seen, "QuestionRequest", |ev| {
        matches!(ev, AgentEvent::QuestionRequest { .. })
    })
    .await;
    let request_id = match &request.ev {
        AgentEvent::QuestionRequest { request_id, .. } => request_id.clone(),
        _ => unreachable!(),
    };
    assert!(
        fx.manager.get("s-q1").unwrap().pending_permission,
        "a pending question flags the session as waiting on a human"
    );

    let mut answers = std::collections::HashMap::new();
    answers.insert("Which database?".to_string(), vec!["SQLite".to_string()]);
    fx.manager
        .command(
            "s-q1",
            AgentCommand::Answer {
                request_id,
                answers,
            },
        )
        .await
        .expect("answer");

    let resolved = wait_for(&mut rx, &mut seen, "QuestionResolved", |ev| {
        matches!(ev, AgentEvent::QuestionResolved { .. })
    })
    .await;
    match &resolved.ev {
        AgentEvent::QuestionResolved { answers, .. } => {
            assert_eq!(
                answers.get("Which database?"),
                Some(&vec!["SQLite".to_string()]),
                "the chosen labels are journaled on the resolution"
            );
        }
        _ => unreachable!(),
    }
    wait_for(&mut rx, &mut seen, "TurnCompleted", |ev| {
        matches!(ev, AgentEvent::TurnCompleted { .. })
    })
    .await;
    assert!(!fx.manager.get("s-q1").unwrap().pending_permission);

    // Replay from zero rebuilds the SAME history: the question AND its
    // answers — a reconnecting client renders the answered card from this.
    let replay = fx.manager.attach("s-q1", 0).expect("reattach").replay;
    let req_at = replay
        .iter()
        .position(|e| matches!(e.ev, AgentEvent::QuestionRequest { .. }))
        .expect("request replayed");
    let res_at = replay
        .iter()
        .position(|e| {
            matches!(
                &e.ev,
                AgentEvent::QuestionResolved { answers, .. }
                    if answers.get("Which database?") == Some(&vec!["SQLite".to_string()])
            )
        })
        .expect("resolution with answers replayed");
    assert!(req_at < res_at);
}

#[tokio::test]
async fn pending_ask_resolves_on_driver_death_and_dead_answer_is_definitive() {
    // The reconnect-stranding scenario end-to-end: ask pending → driver
    // dies → the journal must self-heal (drained resolution before Exited);
    // then a respawned driver answering the OLD id must produce a definitive
    // outcome (resolution + notice), never a silent drop.
    let mut fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-q2", &fx.cwd, "question"))
        .expect("spawn");
    let att = fx.manager.attach("s-q2", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    fx.manager
        .command(
            "s-q2",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "pick one".into(),
                }],
            },
        )
        .await
        .expect("send");
    let request = wait_for(&mut rx, &mut seen, "QuestionRequest", |ev| {
        matches!(ev, AgentEvent::QuestionRequest { .. })
    })
    .await;
    let stale_id = match &request.ev {
        AgentEvent::QuestionRequest { request_id, .. } => request_id.clone(),
        _ => unreachable!(),
    };

    // Driver death drains the pending ask into the journal BEFORE Exited, so
    // no replay of this journal ever ends on a dangling ask.
    assert!(fx.manager.kill("s-q2"));
    let resolved = wait_for(&mut rx, &mut seen, "drained QuestionResolved", |ev| {
        matches!(
            ev,
            AgentEvent::QuestionResolved { request_id, answers }
                if *request_id == stale_id && answers.is_empty()
        )
    })
    .await;
    let exited = wait_for(&mut rx, &mut seen, "Exited", |ev| {
        matches!(ev, AgentEvent::Exited { .. })
    })
    .await;
    assert!(
        resolved.seq < exited.seq,
        "resolution journals before the exit marker"
    );
    let info = fx.manager.get("s-q2").unwrap();
    assert!(!info.alive);
    assert!(
        !info.pending_permission,
        "driver death must clear the waiting-on-human flag"
    );
    let _ = tokio::time::timeout(WAIT, fx.exits.recv()).await;

    // Respawn under the same id (same journal — the view-toggle/resume
    // path): the new driver never issued the old ask.
    assert!(fx.manager.remove("s-q2").is_some());
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-q2", &fx.cwd, "question"))
        .expect("respawn");
    let att = fx.manager.attach("s-q2", exited.seq).expect("reattach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;
    if !seen.iter().any(|e| matches!(e.ev, AgentEvent::Init { .. })) {
        wait_for(&mut rx, &mut seen, "respawn Init", |ev| {
            matches!(ev, AgentEvent::Init { .. })
        })
        .await;
    }

    // Answering the dead ask: definitive outcome, not a swallow.
    let mut answers = std::collections::HashMap::new();
    answers.insert("Which database?".to_string(), vec!["SQLite".to_string()]);
    fx.manager
        .command(
            "s-q2",
            AgentCommand::Answer {
                request_id: stale_id.clone(),
                answers,
            },
        )
        .await
        .expect("answer stale");
    wait_for(&mut rx, &mut seen, "stale-answer QuestionResolved", |ev| {
        matches!(
            ev,
            AgentEvent::QuestionResolved { request_id, answers }
                if *request_id == stale_id && answers.is_empty()
        )
    })
    .await;
    wait_for(
        &mut rx,
        &mut seen,
        "stale-answer Notice",
        |ev| matches!(ev, AgentEvent::Notice { text } if text.contains("no longer active")),
    )
    .await;
}

#[tokio::test]
async fn kill_ends_driver_and_emits_exited() {
    let mut fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-5", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-5", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    assert!(fx.manager.kill("s-5"));
    wait_for(&mut rx, &mut seen, "Exited", |ev| {
        matches!(ev, AgentEvent::Exited { .. })
    })
    .await;
    let exit = tokio::time::timeout(WAIT, fx.exits.recv())
        .await
        .expect("exit hook fired")
        .expect("channel open");
    assert!(exit.starts_with("s-5:Killed"), "got {exit}");
    assert!(!fx.manager.get("s-5").unwrap().alive);

    assert!(fx.manager.remove("s-5").is_some());
    assert!(!fx.manager.contains("s-5"));
}

#[tokio::test]
async fn stale_attach_reports_the_clamped_replay_cursor() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-stale", &fx.cwd, "normal"))
        .expect("spawn");

    // Simulate a warm browser store reconnecting after its on-disk journal
    // was pruned/recreated. The server sends `head` so the store resets, and
    // must also dedupe future live events from the same effective cursor used
    // for replay (0), even when the replay happens to be empty at this instant.
    let att = fx.manager.attach("s-stale", u64::MAX).expect("attach");
    assert_eq!(att.replay_from, 0);
    assert!(att.head_seq < u64::MAX);

    assert!(fx.manager.kill("s-stale"));
}
