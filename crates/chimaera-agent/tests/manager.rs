//! Hermetic driver + registry tests against the scripted `fake-claude`
//! binary — the full pipeline (spawn → handshake → mapping → journal →
//! broadcast → hooks) with no network, auth, or billing. Protocol drift
//! against the REAL binaries is covered separately by `just chat-smoke`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use chimaera_agent::claude::{ClaudeAdapter, TESTED_CLAUDE_VERSION};
use chimaera_agent::driver::SpawnSpec;
use chimaera_agent::journal::SeqEvent;
use chimaera_agent::model::{AgentCommand, AgentEvent, ContentBlock, ToolStatus, UserMessageState};
use chimaera_agent::{ChatManager, EventHook, ExitHook};

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

/// A mid-turn send echoes queued, resolves `sent` when the running turn's
/// result lands, and the journal replays the pair (one message, one update).
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

    // Turn one finishes; the queued message dequeues: sent, then its turn.
    wait_for(&mut rx, &mut seen, "TurnCompleted", |ev| {
        matches!(ev, AgentEvent::TurnCompleted { .. })
    })
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

/// A user interrupt aborts the turn (structurally marked `interrupted`) and
/// drops the CLI's queue: the queued message replays in its dropped state.
#[tokio::test]
async fn interrupt_drops_queued_send_and_classifies_user_stop() {
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

    // The queue dies with the aborted turn (dropped precedes the abort)…
    wait_for(
        &mut rx,
        &mut seen,
        "UserMessageUpdate dropped",
        |ev| matches!(ev, AgentEvent::UserMessageUpdate { id, state: UserMessageState::Dropped } if *id == queued_id),
    )
    .await;
    // …and the abort is a quiet user stop, not a failure — the fake omits
    // the result string, so this proves the structural flag, not a string
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

    // Replay: queued-never-sent ends dropped, echoed exactly once.
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
    assert_eq!(updates, vec![UserMessageState::Dropped]);
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
/// pin gets a NON-FATAL drift notice (warn, don't refuse): the session still
/// lives, the version is journaled on Init so a later misbehavior is already
/// diagnosed, and a Notice names both versions. Neither wire protocol carries
/// a reliable version, so the value rides `SpawnSpec::agent_version`.
#[tokio::test]
async fn version_drift_emits_nonfatal_notice_and_journals_version() {
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

    // The drift signal is a Notice (informational), never a fatal Error, and
    // names both the detected version and the tested pin.
    let notice = wait_for(
        &mut rx,
        &mut seen,
        "drift Notice",
        |ev| matches!(ev, AgentEvent::Notice { text } if text.contains("verified against")),
    )
    .await;
    match &notice.ev {
        AgentEvent::Notice { text } => {
            assert!(
                text.contains("9.9.9-fake"),
                "notice names the detected version: {text}"
            );
            assert!(
                text.contains(TESTED_CLAUDE_VERSION),
                "notice names the tested pin: {text}"
            );
        }
        _ => unreachable!(),
    }
    // Warn, don't block: the drift never kills the session.
    assert!(
        fx.manager.get("s-9").unwrap().alive,
        "version drift must not kill the session"
    );
}

/// A probed version that CONTAINS the tested pin raises no drift notice — the
/// substring match tolerates the CLI's own phrasing ("2.1.204 (Claude Code)")
/// around the pinned version.
#[tokio::test]
async fn matching_version_emits_no_drift_notice() {
    let fx = fixture();
    let mut spec = spec("s-10", &fx.cwd, "normal");
    spec.agent_version = Some(format!("{TESTED_CLAUDE_VERSION} (Claude Code)"));
    fx.manager.spawn(&ClaudeAdapter, spec).expect("spawn");

    let att = fx.manager.attach("s-10", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;

    wait_for(&mut rx, &mut seen, "Init", |ev| {
        matches!(ev, AgentEvent::Init { .. })
    })
    .await;
    // The drift notice, when raised, is emitted right after Init — strictly
    // before the Send is processed into a UserMessage. So a UserMessage with
    // no preceding drift Notice proves none was raised.
    fx.manager
        .command(
            "s-10",
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
        "a matching version must raise no drift notice; saw {seen:#?}"
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
