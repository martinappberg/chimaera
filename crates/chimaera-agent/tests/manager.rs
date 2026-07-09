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
use chimaera_agent::model::{AgentCommand, AgentEvent, ContentBlock, ToolStatus};
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

    // Native id landed in the resume index.
    assert_eq!(
        fx.manager.index().lookup("fake-native-1").as_deref(),
        Some("s-1")
    );
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
