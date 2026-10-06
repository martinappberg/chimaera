//! Hermetic driver + registry tests against the scripted `fake-claude`
//! binary — the full pipeline (spawn → handshake → mapping → journal →
//! broadcast → hooks) with no network, auth, or billing. Protocol drift
//! against the REAL binaries is covered separately by `just chat-smoke`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use chimaera_agent::claude::ClaudeAdapter;
use chimaera_agent::driver::{AgentAdapter, DriverExit, DriverIo, SpawnSpec};
use chimaera_agent::journal::SeqEvent;
use chimaera_agent::model::{
    AgentCommand, AgentEvent, BackgroundTask, ContentBlock, RemoteControlState, ToolContent,
    ToolStatus, UserMessageState,
};
use chimaera_agent::{
    ChatManager, ClientIdState, CommandQueueFull, EventHook, ExitHook, SendCancelled, SendOutcome,
    SendUncertain, RETAINED_SENDS_MAX,
};

const FAKE: &str = env!("CARGO_BIN_EXE_fake-claude");
/// Ceiling per wait. Every wait here is condition-driven, so this only bounds
/// how long a broken run takes to fail; generous for a loaded, parallel CI run.
const WAIT: Duration = Duration::from_secs(20);

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
    // Cross-built fixtures run the copied fake from their disposable VM share.
    let fake = std::env::var("CHIMAERA_TEST_FAKE_CLAUDE").unwrap_or_else(|_| FAKE.into());
    SpawnSpec::new(id, vec![fake, mode.to_string()], cwd.to_path_buf())
}

/// Durable send receipts belong to managed execution (work that can move to
/// another machine); an ordinary chat keeps its send record in memory.
fn durable_spec(id: &str, cwd: &Path, mode: &str) -> SpawnSpec {
    let mut spec = spec(id, cwd, mode);
    spec.managed_execution = true;
    spec
}

/// The cleanup owner survives removal of the registry row. Synthetic agent,
/// real managed process group; no provider, network or billing.
#[cfg(unix)]
#[tokio::test]
async fn paused_cleanup_tracks_captured_child_after_registry_removal() {
    let fx = fixture();
    let mut launch = spec("s-captured-cleanup", &fx.cwd, "artifacts");
    launch.managed_execution = true;
    fx.manager.spawn(&ClaudeAdapter, launch).unwrap();
    let attached = fx.manager.attach("s-captured-cleanup", 0).unwrap();
    let mut seen = attached.replay;
    let mut events = attached.live;
    if !seen
        .iter()
        .any(|event| matches!(event.ev, AgentEvent::Init { .. }))
    {
        wait_for(&mut events, &mut seen, "initialization", |event| {
            matches!(event, AgentEvent::Init { .. })
        })
        .await;
    }
    let mut captured = fx
        .manager
        .pause_commands("s-captured-cleanup")
        .await
        .unwrap();
    assert!(captured.cleanup_pending());
    assert!(fx.manager.remove("s-captured-cleanup").unwrap().alive);
    assert!(fx.manager.process_group("s-captured-cleanup").is_none());
    assert!(captured.cleanup_pending());
    captured.fence();
    tokio::time::timeout(WAIT, async {
        while captured.cleanup_pending() {
            captured.fence();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!captured.cleanup_pending());
}

/// Real Linux child/pipe/pump path, synthetic Claude protocol only. This is
/// neither real CLI idle acceptance nor the supervisor's full process census.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn maintenance_exact_leader_drains_and_retains_ingress_until_resumed() {
    let fx = fixture();
    let mut launch = spec("s-maintenance", &fx.cwd, "artifacts");
    launch.managed_execution = true;
    launch.agent_version = Some(chimaera_agent::claude::TESTED_CLAUDE_VERSION.into());
    fx.manager.spawn(&ClaudeAdapter, launch).unwrap();
    let attached = fx.manager.attach("s-maintenance", 0).unwrap();
    let mut seen = attached.replay;
    let mut events = attached.live;
    if !seen
        .iter()
        .any(|entry| matches!(entry.ev, AgentEvent::Init { .. }))
    {
        wait_for(&mut events, &mut seen, "initialization", |event| {
            matches!(event, AgentEvent::Init { .. })
        })
        .await;
    }
    assert!(fx.manager.maintenance_idle("s-maintenance").await.is_err());
    fx.manager
        .command(
            "s-maintenance",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "synthetic idle turn".into(),
                }],
            },
        )
        .await
        .unwrap();
    wait_for(&mut events, &mut seen, "completed turn", |event| {
        matches!(event, AgentEvent::TurnCompleted { .. })
    })
    .await;
    let idle = fx.manager.maintenance_idle("s-maintenance").await.unwrap();
    let (mut idle, mut leader) = tokio::task::spawn_blocking(move || {
        let mut leader = idle.pin_process().unwrap();
        leader.request_stop().unwrap();
        (idle, leader)
    })
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !leader.is_stopped().unwrap() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let old_head = fx.manager.attach("s-maintenance", 0).unwrap().head_seq;
    idle.drain(std::time::Instant::now() + std::time::Duration::from_secs(5))
        .await
        .unwrap();
    idle.check().unwrap();
    assert!(fx
        .manager
        .command("s-maintenance", AgentCommand::Interrupt)
        .await
        .is_err());
    assert_eq!(
        fx.manager.attach("s-maintenance", 0).unwrap().head_seq,
        old_head
    );
    leader.resume().unwrap();
    drop(idle);
    assert!(fx.manager.kill("s-maintenance"));
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

/// `seen` with the attach overlap removed: `attach` subscribes to `live`
/// before snapshotting `replay`, so the live tail may re-deliver events the
/// replay already holds (the documented "dedupe by seq" contract). First
/// occurrence wins and arrival order is kept, so a genuinely reordered event
/// survives for the ordering assertions to catch instead of vanishing.
fn dedup_by_seq(seen: &[Arc<SeqEvent>]) -> Vec<&Arc<SeqEvent>> {
    let mut taken = std::collections::HashSet::new();
    seen.iter().filter(|e| taken.insert(e.seq)).collect()
}

/// A turn's surfaced prose so far: its `MessageChunk`s concatenated in
/// arrival order. How many chunks the deltas land in is timing-dependent (see
/// `AgentEvent::MessageChunk`), so tests assert on the concatenation, never on
/// one chunk's text. Not supersede-aware: it fails loudly rather than gluing
/// prose across a `MessagesSuperseded` the client would have dropped.
fn prose(seen: &[Arc<SeqEvent>], turn: &str) -> String {
    let mut out = String::new();
    for e in dedup_by_seq(seen) {
        match &e.ev {
            AgentEvent::MessageChunk { turn_id, text } if turn_id == turn => out.push_str(text),
            AgentEvent::MessagesSuperseded => {
                panic!(
                    "prose() is not supersede-aware; MessagesSuperseded at seq {}",
                    e.seq
                )
            }
            _ => {}
        }
    }
    out
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
    let started = wait_for(&mut rx, &mut seen, "TurnStarted", |ev| {
        matches!(ev, AgentEvent::TurnStarted { .. })
    })
    .await;
    let AgentEvent::TurnStarted { turn_id: turn } = &started.ev else {
        unreachable!()
    };
    // Second Init carries the native session id from system/init.
    wait_for(&mut rx, &mut seen, "Init with native id", |ev| {
        matches!(ev, AgentEvent::Init { native_session_id, .. } if native_session_id == "fake-native-1")
    })
    .await;
    // "hel"/"lo" may surface as one chunk or two (see `AgentEvent::MessageChunk`;
    // a loaded CI runner split them). What IS pinned is the order: the driver
    // flushes the turn's prose before it emits the tool_use's ToolCall, so once
    // the ToolCall is here the prose is complete.
    wait_for(
        &mut rx,
        &mut seen,
        "ToolCall",
        |ev| matches!(ev, AgentEvent::ToolCall { id, .. } if id == "tu-1"),
    )
    .await;
    assert_eq!(
        prose(&seen, turn),
        "hello",
        "streamed deltas coalesce losslessly ahead of the tool call; saw {seen:#?}"
    );
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
    // The assistant frame repeats the streamed text as a `text` block; the
    // driver must not surface it a second time.
    assert_eq!(
        prose(&seen, turn),
        "hello",
        "a turn's prose surfaces exactly once; saw {seen:#?}"
    );

    // After the attach-overlap dedupe, what a client saw must BE the journal:
    // contiguous from seq 1 (this attach started at 0), no reorder, no gap.
    // (A running-max filter would make this vacuous — it hides reorders.)
    let seqs: Vec<u64> = dedup_by_seq(&seen).iter().map(|e| e.seq).collect();
    assert_eq!(seqs.first(), Some(&1), "stream starts at seq 1: {seen:#?}");
    for pair in seqs.windows(2) {
        assert_eq!(pair[1], pair[0] + 1, "reordered or gapped: {seen:#?}");
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
                    monitor: false,
                    ambient: false,
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

/// A daemon-authored event (`annotate`) takes its seq on the pump like any
/// driver event: journaled, broadcast, and replayed on reconnect. A dead
/// session refuses it.
#[tokio::test]
async fn annotate_journals_a_daemon_event_in_seq_order() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-annotate", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-annotate", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;
    if !seen.iter().any(|e| matches!(e.ev, AgentEvent::Init { .. })) {
        wait_for(&mut rx, &mut seen, "Init", |ev| {
            matches!(ev, AgentEvent::Init { .. })
        })
        .await;
    }
    let message = AgentEvent::agent_message(
        12,
        "s-other",
        "loader refactor",
        Some("codex"),
        "the loader returns Result now",
        false,
        false,
        None,
    );
    fx.manager
        .annotate("s-annotate", message.clone())
        .expect("annotate a live session");
    let entry = wait_for(&mut rx, &mut seen, "the annotation", |ev| {
        matches!(ev, AgentEvent::AgentMessage { .. })
    })
    .await;
    assert_eq!(entry.ev, message);
    let head = seen.iter().map(|e| e.seq).max().unwrap();
    assert_eq!(entry.seq, head, "it took the next seq");
    let replay = fx.manager.attach("s-annotate", 0).expect("replay").replay;
    assert!(replay.iter().any(|e| e.ev == message));
    assert!(fx.manager.annotate("s-missing", message).is_err());
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

/// The 2.1.281 transcript surfaces end to end (fake-claude `showcase`):
/// narration journals as PROSE, tool batches get their labels, a subagent
/// closes with exactly one finished line (report + footprint) and a clean
/// row, a Monitor lane is marked as a watch, and background closes speak the
/// CLI's own sentences — while an agent's close never doubles as a
/// background notice.
#[tokio::test]
async fn showcase_turn_maps_every_transcript_surface() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-show", &fx.cwd, "showcase"))
        .expect("spawn");
    let att = fx.manager.attach("s-show", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;
    send_text(&fx, "s-show", "survey the workspace").await;
    // The monitor's close is the last frame the fake emits.
    wait_for(&mut rx, &mut seen, "the monitor close", |ev| {
        matches!(ev, AgentEvent::BackgroundTasks { closed, .. }
            if closed.iter().any(|c| c.id == "mon-1"))
    })
    .await;
    let events: Vec<&AgentEvent> = seen.iter().map(|e| &e.ev).collect();

    let prose: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageChunk { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let thought: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ThoughtChunk { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        prose.contains("I'm handing the size check to a helper agent"),
        "narration is prose: {prose:?}"
    );
    assert!(
        !thought.contains("helper agent"),
        "…and never thought: {thought:?}"
    );
    assert!(
        thought.contains("listing the workspace first"),
        "reasoning stays thought"
    );

    let labels: Vec<(&str, &[String])> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolSummary { summary, tool_ids } => {
                Some((summary.as_str(), tool_ids.as_slice()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(labels.len(), 3, "one label per batch: {labels:?}");
    assert_eq!(labels[2].1, ["tu-bg".to_string(), "tu-mon".to_string()]);

    let finished: Vec<&AgentEvent> = events
        .iter()
        .copied()
        .filter(|e| matches!(e, AgentEvent::SubagentFinished { .. }))
        .collect();
    match finished.as_slice() {
        [AgentEvent::SubagentFinished {
            id,
            label,
            status,
            result,
            stats,
        }] => {
            assert_eq!(id.as_deref(), Some("tu-agent"));
            assert_eq!(label, "Measure file sizes");
            assert_eq!(status, "completed");
            assert!(result
                .as_deref()
                .is_some_and(|r| r.starts_with("Both files are 6 bytes")));
            assert_eq!(stats.as_deref(), Some("2 tools · 12.3k tokens · 4s"));
        }
        other => panic!("exactly one finished line: {other:#?}"),
    }
    let agent_row = events.iter().rev().find_map(|e| match e {
        AgentEvent::ToolCallUpdate {
            id,
            content: Some(ToolContent::Output { text, .. }),
            ..
        } if id == "tu-agent" => Some(text.clone()),
        _ => None,
    });
    assert_eq!(
        agent_row.as_deref(),
        Some(
            "Both files are 6 bytes:\n\n| file | bytes |\n|---|---|\n| a.txt | 6 |\n| b.txt | 6 |"
        ),
        "the row shows the report, not the hand-back scaffolding"
    );

    let monitor_marked = events.iter().any(|e| {
        matches!(e, AgentEvent::BackgroundTasks { tasks, .. }
            if tasks.iter().any(|t| t.id == "mon-1" && t.monitor)
                && tasks.iter().any(|t| t.id == "bg-1" && !t.monitor))
    });
    assert!(
        monitor_marked,
        "the Monitor lane is a watch, the Bash lane a job"
    );
    let closes: Vec<(String, Option<String>)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::BackgroundTasks { closed, .. } => Some(closed),
            _ => None,
        })
        .flatten()
        .map(|c| (c.id.clone(), c.summary.clone()))
        .collect();
    assert_eq!(
        closes,
        [
            (
                "bg-1".to_string(),
                Some("Background command \"Warm the cache\" completed (exit code 0)".to_string())
            ),
            (
                "mon-1".to_string(),
                Some("Monitor \"Watch the build log\" stream ended".to_string())
            ),
        ],
        "background closes only — the agent's close is its finished line"
    );
    assert!(fx.manager.kill("s-show"));
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
async fn startup_hooks_are_visible_before_init_and_replay_without_private_output() {
    let fx = fixture();
    let release = fx.cwd.join("release-startup-hooks");
    let mut launch = spec("hooks", &fx.cwd, "startup-hooks");
    launch.env.push((
        "FAKE_STARTUP_HOOK_RELEASE".into(),
        release.to_string_lossy().into_owned(),
    ));
    fx.manager.spawn(&ClaudeAdapter, launch).unwrap();
    let mut attached = fx.manager.attach("hooks", 0).unwrap();
    let mut seen = attached.replay.clone();
    let is_hook_progress = |ev: &AgentEvent| matches!(ev, AgentEvent::StartupProgress { detail } if detail == "Running startup hooks…");
    if !seen.iter().any(|entry| is_hook_progress(&entry.ev)) {
        wait_for(
            &mut attached.live,
            &mut seen,
            "startup hooks",
            is_hook_progress,
        )
        .await;
    }
    assert!(
        fx.manager.is_unused_startup("hooks"),
        "progress alone must not retain an unused failed chat"
    );
    assert!(!seen
        .iter()
        .any(|entry| matches!(entry.ev, AgentEvent::Init { .. })));
    // Release only after the pre-Init assertions: no scheduler-speed assumption.
    std::fs::write(&release, b"").unwrap();
    wait_for(&mut attached.live, &mut seen, "init", |ev| {
        matches!(ev, AgentEvent::Init { .. })
    })
    .await;
    let replay = fx.manager.attach("hooks", 0).unwrap().replay;
    let phases: Vec<_> = replay
        .iter()
        .filter_map(|entry| match &entry.ev {
            AgentEvent::StartupProgress { detail } => Some(detail.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        phases,
        [
            "Waiting for agent initialization…",
            "Running startup hooks…",
            "Loading agent settings and tools…"
        ]
    );
    assert!(!serde_json::to_string(
        &replay
            .iter()
            .map(|entry| entry.as_ref())
            .collect::<Vec<_>>()
    )
    .unwrap()
    .contains("private hook context"));
    fx.manager.kill("hooks");
}

#[tokio::test]
async fn closing_during_startup_cancels_every_provider_without_a_failure() {
    use chimaera_agent::driver::AgentAdapter;
    let adapters: [&dyn AgentAdapter; 4] = [
        &ClaudeAdapter,
        &chimaera_agent::codex::CodexAdapter,
        &chimaera_agent::acp::ANTIGRAVITY,
        &chimaera_agent::acp::GROK,
    ];
    for adapter in adapters {
        let mut fx = fixture();
        let mut launch = spec("closing", &fx.cwd, "silent");
        launch.initial_model = Some("chosen-before-start".into());
        let info = fx.manager.spawn(adapter, launch).expect("spawn");
        assert_eq!(info.model.as_deref(), Some("chosen-before-start"));
        let mut attached = fx.manager.attach("closing", 0).unwrap();
        let mut seen = attached.replay;
        if !seen
            .iter()
            .any(|entry| matches!(entry.ev, AgentEvent::StartupProgress { .. }))
        {
            wait_for(&mut attached.live, &mut seen, "startup", |ev| {
                matches!(ev, AgentEvent::StartupProgress { .. })
            })
            .await;
        }
        assert!(fx.manager.is_unused_startup("closing"));
        assert!(fx.manager.kill("closing"));
        let exit = tokio::time::timeout(Duration::from_secs(5), fx.exits.recv())
            .await
            .expect("close must not wait for startup timeout")
            .unwrap();
        assert_eq!(exit, "closing:Killed");
        let attached = fx.manager.attach("closing", 0).unwrap();
        assert!(!attached
            .replay
            .iter()
            .any(|entry| matches!(entry.ev, AgentEvent::Error { .. })));
    }
}

#[tokio::test]
async fn handshake_failure_is_classified_for_startup_cleanup() {
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

/// Set a file's mtime to `secs_ago` seconds in the past.
fn backdate(path: &Path, secs_ago: u64) {
    let mtime = std::time::SystemTime::now() - Duration::from_secs(secs_ago);
    std::fs::File::open(path)
        .expect("open to backdate")
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .expect("set mtime");
}

/// Highest seq in a journal file (0 when empty / missing).
fn file_head(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<SeqEvent>(l).ok())
        .map(|e| e.seq)
        .max()
        .unwrap_or(0)
}

/// Chats outlive the daemon now (ledger resurrection), so an idle-but-live
/// session's journal is routinely the OLDEST file in the dir. The budget
/// prune must evict history around it, never the journal a reconnect's gap
/// replay (and the next restart's resurrection) reads from.
#[tokio::test]
async fn journal_prune_never_evicts_a_live_session() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-idle", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-idle", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;
    wait_for(&mut rx, &mut seen, "Init", |ev| {
        matches!(ev, AgentEvent::Init { .. })
    })
    .await;

    // Let the writer settle so the backdated mtime below sticks: the file
    // holds everything the session has journaled so far.
    let dir = fx.manager.journal_dir().clone();
    let live = dir.join("s-idle.jsonl");
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let head = fx
            .manager
            .attach("s-idle", u64::MAX)
            .expect("head")
            .head_seq;
        if head > 0 && file_head(&live) == head {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "journal never caught up"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Idle for a day: older than every history journal below.
    backdate(&live, 86_400);

    let budget = chimaera_agent::journal::DIR_MAX_FILES;
    for i in 0..budget + 5 {
        let path = dir.join(format!("h-{i:03}.jsonl"));
        std::fs::write(&path, b"{\"seq\":1,\"ts\":0}\n").expect("seed history");
        // Oldest history = lowest index, all newer than the live journal.
        backdate(&path, 3_600 - i as u64);
    }

    fx.manager
        .prune_journal_dir(std::collections::HashSet::new());

    assert!(live.exists(), "the live session's journal was pruned");
    // mtime only moves forward, so still being a day old now means it was the
    // oldest file when the prune ran — a late driver write would otherwise
    // make this test pass vacuously.
    let age = std::fs::metadata(&live)
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().unwrap_or_default())
        .expect("live mtime");
    assert!(
        age >= Duration::from_secs(80_000),
        "precondition: the live journal was the oldest"
    );
    let remaining: Vec<String> = std::fs::read_dir(&dir)
        .expect("read dir")
        .flatten()
        .map(|e| e.file_name().into_string().expect("utf-8 name"))
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    assert_eq!(remaining.len(), budget, "the budget still holds");
    assert!(
        !remaining.iter().any(|n| n == "h-000.jsonl"),
        "history is still evicted oldest-first"
    );
    let newest = format!("h-{:03}.jsonl", budget + 4);
    assert!(remaining.contains(&newest), "newest kept");

    assert!(fx.manager.kill("s-idle"));
}

/// Remote Control end to end through the registry: the toggle command rides
/// the command channel, the fake's live-shaped round-trip (ready → ack →
/// connected) journals the level-set states, a fresh attach replays them,
/// and the disable acks Off.
#[tokio::test]
async fn remote_control_toggle_journals_state_and_replays() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-rc", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-rc", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;
    let init = wait_for(&mut rx, &mut seen, "Init", |ev| {
        matches!(ev, AgentEvent::Init { .. })
    })
    .await;
    match &init.ev {
        AgentEvent::Init {
            remote_control_available,
            current_mode,
            ..
        } => {
            assert!(remote_control_available, "the fake offers the bridge");
            assert_eq!(
                current_mode.as_deref(),
                Some("default"),
                "seeded at the handshake"
            );
        }
        _ => unreachable!(),
    }

    fx.manager
        .command(
            "s-rc",
            AgentCommand::SetRemoteControl {
                enabled: true,
                name: Some("chimaera test".into()),
            },
        )
        .await
        .expect("enable");
    let connected = wait_for(&mut rx, &mut seen, "RemoteControl connected", |ev| {
        matches!(
            ev,
            AgentEvent::RemoteControl {
                state: RemoteControlState::Connected,
                ..
            }
        )
    })
    .await;
    match &connected.ev {
        AgentEvent::RemoteControl {
            session_url, name, ..
        } => {
            assert_eq!(
                session_url.as_deref(),
                Some("https://claude.ai/code/session_fake01")
            );
            assert_eq!(name.as_deref(), Some("chimaera · chimaera test"));
        }
        _ => unreachable!(),
    }
    // The per-turn system/init (the fake emits one per turn, like the CLI
    // after the first prompt) must carry the live bridge as a snapshot —
    // consumers reset on Init, and a repeated Init must not blank it.
    send_text(&fx, "s-rc", "hello").await;
    let reinit = wait_for(&mut rx, &mut seen, "Init after enable", |ev| {
        matches!(
            ev,
            AgentEvent::Init {
                remote_control: Some(_),
                ..
            }
        )
    })
    .await;
    match &reinit.ev {
        AgentEvent::Init {
            remote_control: Some(rc),
            ..
        } => {
            assert_eq!(rc.state, RemoteControlState::Connected);
            assert_eq!(
                rc.session_url.as_deref(),
                Some("https://claude.ai/code/session_fake01")
            );
        }
        _ => unreachable!(),
    }
    assert_eq!(
        fx.manager
            .get("s-rc")
            .expect("info")
            .remote_control_url
            .as_deref(),
        Some("https://claude.ai/code/session_fake01"),
        "the rail link survives the repeated Init"
    );
    // Let the fake's canned turn (a permission ask) settle so the disable
    // below lands on an idle driver.
    let permission = wait_for(&mut rx, &mut seen, "PermissionRequest", |ev| {
        matches!(ev, AgentEvent::PermissionRequest { .. })
    })
    .await;
    if let AgentEvent::PermissionRequest { request_id, .. } = &permission.ev {
        fx.manager
            .command(
                "s-rc",
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
    wait_for(&mut rx, &mut seen, "TurnCompleted", |ev| {
        matches!(ev, AgentEvent::TurnCompleted { .. })
    })
    .await;
    // The journal carries the whole ladder: Connecting (optimistic), Connecting
    // + link (ack), Connected (bridge frame).
    let states: Vec<RemoteControlState> = seen
        .iter()
        .filter_map(|e| match &e.ev {
            AgentEvent::RemoteControl { state, .. } => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        [
            RemoteControlState::Connecting,
            RemoteControlState::Connecting,
            RemoteControlState::Connected
        ]
    );

    // A late attach replays the same ladder — the state is journal truth.
    let late = fx.manager.attach("s-rc", 0).expect("attach");
    let replayed: Vec<RemoteControlState> = late
        .replay
        .iter()
        .filter_map(|e| match &e.ev {
            AgentEvent::RemoteControl { state, .. } => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(replayed, states);

    fx.manager
        .command(
            "s-rc",
            AgentCommand::SetRemoteControl {
                enabled: false,
                name: None,
            },
        )
        .await
        .expect("disable");
    wait_for(&mut rx, &mut seen, "RemoteControl off", |ev| {
        matches!(
            ev,
            AgentEvent::RemoteControl {
                state: RemoteControlState::Off,
                ..
            }
        )
    })
    .await;
    assert!(fx.manager.kill("s-rc"));
}

/// A refused enable (the fake speaks the CLI's own "claude.ai subscriptions"
/// sentence) lands as an Error state carrying that sentence — a chip can show
/// it — and a kill afterwards journals no phantom Off.
#[tokio::test]
async fn refused_remote_control_is_an_error_state() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-rcx", &fx.cwd, "rc-refuse"))
        .expect("spawn");
    let att = fx.manager.attach("s-rcx", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;
    fx.manager
        .command(
            "s-rcx",
            AgentCommand::SetRemoteControl {
                enabled: true,
                name: None,
            },
        )
        .await
        .expect("enable");
    let err = wait_for(&mut rx, &mut seen, "RemoteControl error", |ev| {
        matches!(
            ev,
            AgentEvent::RemoteControl {
                state: RemoteControlState::Error,
                ..
            }
        )
    })
    .await;
    match &err.ev {
        AgentEvent::RemoteControl { detail, .. } => {
            assert!(detail
                .as_deref()
                .is_some_and(|d| d.contains("claude.ai subscriptions")));
        }
        _ => unreachable!(),
    }
    assert!(fx.manager.kill("s-rcx"));
    wait_for(&mut rx, &mut seen, "Exited", |ev| {
        matches!(ev, AgentEvent::Exited { .. })
    })
    .await;
    assert!(
        !seen.iter().any(|e| matches!(
            &e.ev,
            AgentEvent::RemoteControl {
                state: RemoteControlState::Off,
                ..
            }
        )),
        "nothing was live, so teardown journals no Off"
    );
}

/// `SpawnSpec.remote_control` turns the bridge on right after the handshake
/// (the embedder's standing choice) — and says why not where the CLI does not
/// offer it.
#[tokio::test]
async fn remote_control_at_start_enables_after_the_handshake() {
    let fx = fixture();
    let mut s = spec("s-rca", &fx.cwd, "normal");
    s.remote_control = Some("chimaera at-start".into());
    fx.manager.spawn(&ClaudeAdapter, s).expect("spawn");
    let att = fx.manager.attach("s-rca", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;
    let connected = wait_for(&mut rx, &mut seen, "RemoteControl connected", |ev| {
        matches!(
            ev,
            AgentEvent::RemoteControl {
                state: RemoteControlState::Connected,
                ..
            }
        )
    })
    .await;
    assert!(matches!(
        &connected.ev,
        AgentEvent::RemoteControl { name: Some(n), .. } if n == "chimaera · chimaera at-start"
    ));
    assert!(fx.manager.kill("s-rca"));

    let fx2 = fixture();
    let mut s = spec("s-rcu", &fx2.cwd, "rc-unavailable");
    s.remote_control = Some("chimaera at-start".into());
    fx2.manager.spawn(&ClaudeAdapter, s).expect("spawn");
    let att = fx2.manager.attach("s-rcu", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;
    wait_for(
        &mut rx,
        &mut seen,
        "not offered notice",
        |ev| matches!(ev, AgentEvent::Notice { text } if text.contains("not offered")),
    )
    .await;
    assert!(fx2.manager.kill("s-rcu"));
}

/// What a daemon restart would cut off is readable off the live session
/// (the ledger snapshots it): the bridge, ultracode, background work, a
/// running turn. The resurrection side round-trips too — ultracode comes
/// back through the handshake without reading as a pick, and a daemon-sent
/// message journals with its origin.
#[tokio::test]
async fn carryover_reports_process_state_and_ultracode_restores() {
    let fx = fixture();
    let mut s = spec("s-carry", &fx.cwd, "background");
    s.remote_control = Some("carry".into());
    s.initial_ultracode = true;
    fx.manager.spawn(&ClaudeAdapter, s).expect("spawn");
    let att = fx.manager.attach("s-carry", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;
    wait_for(&mut rx, &mut seen, "ultracode read-back", |ev| {
        matches!(
            ev,
            AgentEvent::EffortState {
                ultracode: true,
                chosen: false,
                ..
            }
        )
    })
    .await;
    // The bridge and the ultracode read-back race after the handshake.
    let connected = |ev: &AgentEvent| {
        matches!(
            ev,
            AgentEvent::RemoteControl {
                state: RemoteControlState::Connected,
                ..
            }
        )
    };
    if !seen.iter().any(|e| connected(&e.ev)) {
        wait_for(&mut rx, &mut seen, "RemoteControl connected", connected).await;
    }

    fx.manager
        .command_as(
            "s-carry",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "pick your work back up".into(),
                }],
            },
            Some(chimaera_agent::model::ORIGIN_RESTART),
        )
        .await
        .expect("send");
    wait_for(&mut rx, &mut seen, "tagged echo", |ev| {
        matches!(
            ev,
            AgentEvent::UserMessage { origin: Some(o), .. } if o == "restart"
        )
    })
    .await;
    wait_for(&mut rx, &mut seen, "TurnStarted", |ev| {
        matches!(ev, AgentEvent::TurnStarted { .. })
    })
    .await;
    wait_for(
        &mut rx,
        &mut seen,
        "BackgroundTasks",
        |ev| matches!(ev, AgentEvent::BackgroundTasks { tasks, .. } if tasks.len() == 1),
    )
    .await;
    wait_for(&mut rx, &mut seen, "TurnCompleted", |ev| {
        matches!(ev, AgentEvent::TurnCompleted { .. })
    })
    .await;

    let carry = fx.manager.carryover("s-carry").expect("live session");
    assert!(carry.remote_control, "{carry:?}");
    assert!(carry.ultracode, "{carry:?}");
    assert!(!carry.turn_in_flight, "the turn ended: {carry:?}");
    assert_eq!(carry.background.len(), 1, "{carry:?}");
    assert_eq!(carry.background[0].task_type, "local_bash");
    assert!(carry.interrupted_work());

    assert!(fx.manager.kill("s-carry"));
    wait_for(&mut rx, &mut seen, "Exited", |ev| {
        matches!(ev, AgentEvent::Exited { .. })
    })
    .await;
    assert_eq!(
        fx.manager.carryover("s-carry"),
        Some(chimaera_agent::Carryover::default()),
        "nothing outlives the process"
    );
}

/// A user's model pick is remembered per agent kind (the daemon's prefs) so
/// the next chat of that kind starts with it; a reroute (reason present) is
/// not a pick and must not overwrite it. The store is durable: a fresh
/// manager on the same journal dir reads it back.
#[tokio::test]
async fn model_pick_is_remembered_per_agent_kind() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-pref", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-pref", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;
    assert_eq!(fx.manager.prefs("claude").model, None);
    fx.manager
        .command(
            "s-pref",
            AgentCommand::SetModel {
                model_id: "sonnet".into(),
            },
        )
        .await
        .expect("set_model");
    wait_for(
        &mut rx,
        &mut seen,
        "ModelSwitched",
        |ev| matches!(ev, AgentEvent::ModelSwitched { to, .. } if to == "sonnet"),
    )
    .await;
    // The write is detached onto a blocking worker (memory is updated first,
    // the atomic rename lands a moment later): poll BOTH the live store and
    // a fresh manager on the same dir — durability across a restart is the
    // point.
    let dir = fx.manager.journal_dir().to_path_buf();
    let reload = || {
        let (exit_tx, _exits) = mpsc::unbounded_channel::<String>();
        ChatManager::new(
            dir.clone(),
            Box::new(|_, _| {}),
            Box::new(move |id, exit| {
                let _ = exit_tx.send(format!("{id}:{exit:?}"));
            }),
        )
    };
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let live = fx.manager.prefs("claude").model;
        let durable = reload().prefs("claude").model;
        if live.as_deref() == Some("sonnet") && durable.as_deref() == Some("sonnet") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pref never recorded: live={live:?} durable={durable:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(reload().prefs("codex").model, None);
    // A mode pick is remembered too (its ack carries chosen:true).
    fx.manager
        .command(
            "s-pref",
            AgentCommand::SetMode {
                mode_id: "acceptEdits".into(),
            },
        )
        .await
        .expect("set_mode");
    wait_for(&mut rx, &mut seen, "ModeChanged", |ev| {
        matches!(ev, AgentEvent::ModeChanged { mode_id, chosen: true } if mode_id == "acceptEdits")
    })
    .await;
    let deadline = std::time::Instant::now() + WAIT;
    while reload().prefs("claude").mode.as_deref() != Some("acceptEdits") {
        assert!(
            std::time::Instant::now() < deadline,
            "mode pref never recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The spawn's bootstrap effort read-back is NOT a pick: nothing recorded.
    assert_eq!(reload().prefs("claude").effort, None);
    assert!(fx.manager.kill("s-pref"));

    // The next spawn replays the remembered mode through the handshake; that
    // ModeChanged is not a pick (chosen:false) — it must not re-record.
    let mut replay = spec("s-pref2", &fx.cwd, "normal");
    replay.initial_mode = Some("plan".into());
    fx.manager.spawn(&ClaudeAdapter, replay).expect("spawn");
    let att = fx.manager.attach("s-pref2", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;
    wait_for(
        &mut rx,
        &mut seen,
        "replayed ModeChanged",
        |ev| matches!(ev, AgentEvent::ModeChanged { mode_id, chosen: false } if mode_id == "plan"),
    )
    .await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(&e.ev, AgentEvent::ModeChanged { chosen: true, .. })),
        "the handshake replay must not read as a pick"
    );
    assert_eq!(
        reload().prefs("claude").mode.as_deref(),
        Some("acceptEdits")
    );
    assert!(fx.manager.kill("s-pref2"));
}

/// The journal index carries what each native conversation last ran with —
/// model and mode from Init, then every change — so a reopen (resume /
/// rewind / fork / resurrection) can start from it instead of the prefs.
#[tokio::test]
async fn conversation_settings_are_indexed_per_native_id() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-own", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-own", 0).expect("attach");
    let mut seen: Vec<Arc<SeqEvent>> = att.replay.clone();
    let mut rx = att.live;
    // The handshake's first snapshot carries no native id yet; the first
    // turn's system/init re-emits Init with it, and that is the row the
    // index keys (a resume handle only exists once a conversation does).
    fx.manager
        .command(
            "s-own",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "run it".into(),
                }],
            },
        )
        .await
        .expect("send");
    let init = wait_for(&mut rx, &mut seen, "Init with a native id", |ev| {
        matches!(ev, AgentEvent::Init { native_session_id, .. } if !native_session_id.is_empty())
    })
    .await;
    let AgentEvent::Init {
        native_session_id,
        model,
        current_mode,
        ..
    } = &init.ev
    else {
        unreachable!()
    };
    let native = native_session_id.clone();
    let deadline = std::time::Instant::now() + WAIT;
    while fx.manager.index().settings(&native).model != *model
        || fx.manager.index().settings(&native).mode != *current_mode
    {
        assert!(
            std::time::Instant::now() < deadline,
            "Init settings never indexed: index={:?} init model={model:?} mode={current_mode:?}",
            fx.manager.index().settings(&native)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    fx.manager
        .command(
            "s-own",
            AgentCommand::SetMode {
                mode_id: "plan".into(),
            },
        )
        .await
        .expect("set_mode");
    wait_for(
        &mut rx,
        &mut seen,
        "ModeChanged",
        |ev| matches!(ev, AgentEvent::ModeChanged { mode_id, .. } if mode_id == "plan"),
    )
    .await;
    let deadline = std::time::Instant::now() + WAIT;
    while fx.manager.index().settings(&native).mode.as_deref() != Some("plan") {
        assert!(
            std::time::Instant::now() < deadline,
            "mode change never indexed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The other settings survived the mode write.
    assert_eq!(fx.manager.index().settings(&native).model, *model);
    assert!(fx.manager.kill("s-own"));
}

#[tokio::test]
async fn managed_fence_stops_a_synthetic_process_during_stalled_handshake() {
    let f = fixture();
    let mut launch = SpawnSpec::new(
        "managed-stalled",
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "trap '' TERM; sleep 60".into(),
        ],
        f.cwd.clone(),
    );
    launch.managed_execution = true;
    launch.handshake_timeout = Duration::from_secs(60);
    f.manager.spawn(&ClaudeAdapter, launch).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(f.manager.fence("managed-stalled"));
    assert!(f
        .manager
        .command(
            "managed-stalled",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "must not reach process".into()
                }]
            }
        )
        .await
        .is_err());
    tokio::time::timeout(Duration::from_secs(6), async {
        while f
            .manager
            .get("managed-stalled")
            .is_some_and(|info| info.alive)
        {
            f.manager.fence("managed-stalled");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn managed_fence_does_not_claim_containment_of_setsid_descendants() {
    let f = fixture();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("escape-stop"), b"stop");
        }
    }
    let _cleanup = Cleanup(f.cwd.clone());
    let mut launch = spec("managed-escape", &f.cwd, "detached-process-fixture");
    launch.managed_execution = true;
    launch.handshake_timeout = Duration::from_secs(60);
    f.manager.spawn(&ClaudeAdapter, launch).unwrap();
    let pulse = || {
        std::fs::read_to_string(f.cwd.join("escape-heartbeat"))
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    tokio::time::timeout(Duration::from_secs(4), async {
        while pulse() == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(f.manager.fence("managed-escape"));
    tokio::time::timeout(Duration::from_secs(5), async {
        while f
            .manager
            .get("managed-escape")
            .is_some_and(|info| info.alive)
        {
            f.manager.fence("managed-escape");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let stopped_parent_pulse = pulse();
    tokio::time::timeout(Duration::from_secs(1), async {
        while pulse() <= stopped_parent_pulse {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("setsid child demonstrates why expired takeover is not advertised");
    std::fs::write(f.cwd.join("escape-stop"), b"stop").unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !f.cwd.join("escape-done").exists() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
}

fn text_send(text: &str) -> AgentCommand {
    AgentCommand::Send {
        blocks: vec![ContentBlock::Text { text: text.into() }],
    }
}

/// How many messages the journal holds under a client's send id.
fn echoes_of(replay: &[Arc<SeqEvent>], client_id: &str) -> usize {
    replay
        .iter()
        .filter(|e| {
            matches!(&e.ev, AgentEvent::UserMessage { client_id: Some(id), .. } if id == client_id)
        })
        .count()
}

/// A send that arrives while the driver is still in its handshake is accepted
/// at once (queued, with nothing in the journal yet). The client cannot see
/// it there and sends it again under the same id: that copy must be dropped,
/// or the agent would run the message twice.
#[cfg(unix)]
#[tokio::test]
async fn a_send_queued_during_the_handshake_is_accepted_once() {
    let fx = fixture();
    // The agent answers its handshake a moment late.
    let launch = SpawnSpec::new(
        "s-handshake",
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "sleep 1; exec \"$0\" normal".into(),
            FAKE.to_string(),
        ],
        fx.cwd.clone(),
    );
    fx.manager.spawn(&ClaudeAdapter, launch).expect("spawn");
    let att = fx.manager.attach("s-handshake", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;
    assert!(
        !seen.iter().any(|e| matches!(e.ev, AgentEvent::Init { .. })),
        "the handshake must still be running"
    );

    let outcome = fx
        .manager
        .send_from_client("s-handshake", text_send("once"), Some("client-handshake"))
        .await
        .expect("send");
    assert_eq!(outcome, SendOutcome::Accepted);
    assert_eq!(
        fx.manager
            .client_id_state("s-handshake", "client-handshake"),
        Some(ClientIdState::Accepted)
    );
    // The client's resend, and a late cancel: neither changes anything.
    let again = fx
        .manager
        .send_from_client("s-handshake", text_send("once"), Some("client-handshake"))
        .await
        .expect("resend");
    assert_eq!(again, SendOutcome::Duplicate);
    assert!(!fx
        .manager
        .cancel_send("s-handshake", "client-handshake")
        .await
        .unwrap());

    let echo = wait_for(&mut rx, &mut seen, "the echo", |ev| {
        matches!(ev, AgentEvent::UserMessage { .. })
    })
    .await;
    assert!(
        matches!(&echo.ev, AgentEvent::UserMessage { text, client_id: Some(id), .. }
            if text == "once" && id == "client-handshake"),
        "{:?}",
        echo.ev
    );
    wait_for(&mut rx, &mut seen, "PermissionRequest", |ev| {
        matches!(ev, AgentEvent::PermissionRequest { .. })
    })
    .await;
    // One more resend after the echo, then the journal: one message, one turn.
    let late = fx
        .manager
        .send_from_client("s-handshake", text_send("once"), Some("client-handshake"))
        .await
        .expect("late resend");
    assert_eq!(late, SendOutcome::Duplicate);
    let replay = fx.manager.attach("s-handshake", 0).expect("attach").replay;
    assert_eq!(echoes_of(&replay, "client-handshake"), 1);
    assert_eq!(
        replay
            .iter()
            .filter(|e| matches!(e.ev, AgentEvent::UserMessage { .. }))
            .count(),
        1
    );
    assert_eq!(
        replay
            .iter()
            .filter(|e| matches!(e.ev, AgentEvent::TurnStarted { .. }))
            .count(),
        1
    );
    assert!(fx.manager.kill("s-handshake"));
}

/// The record of accepted ids is read back from the journal, so a daemon
/// that restarted (a new manager over the same journal) still drops a resend
/// of a message its previous life delivered.
#[tokio::test]
async fn a_send_id_in_the_journal_is_still_refused_after_a_restart() {
    let mut fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-restart", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-restart", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;
    let outcome = fx
        .manager
        .send_from_client("s-restart", text_send("before"), Some("client-before"))
        .await
        .expect("send");
    assert_eq!(outcome, SendOutcome::Accepted);
    wait_for(&mut rx, &mut seen, "the echo", |ev| {
        matches!(ev, AgentEvent::UserMessage { client_id: Some(id), .. } if id == "client-before")
    })
    .await;
    assert!(fx.manager.kill("s-restart"));
    tokio::time::timeout(WAIT, fx.exits.recv())
        .await
        .expect("exit hook fired")
        .expect("channel open");

    let restarted = Arc::new(ChatManager::new(
        fx.manager.journal_dir().clone(),
        Box::new(|_, _| {}),
        Box::new(|_, _| {}),
    ));
    restarted
        .spawn(&ClaudeAdapter, spec("s-restart", &fx.cwd, "normal"))
        .expect("spawn again");
    assert_eq!(
        restarted.client_id_state("s-restart", "client-before"),
        Some(ClientIdState::Confirmed)
    );
    let again = restarted
        .send_from_client("s-restart", text_send("before"), Some("client-before"))
        .await
        .expect("resend");
    assert_eq!(again, SendOutcome::Duplicate);
    // A different id is a different message.
    let other = restarted
        .send_from_client("s-restart", text_send("after"), Some("client-after1"))
        .await
        .expect("send");
    assert_eq!(other, SendOutcome::Accepted);
    // The reopened journal's history is on disk: read it off the runtime.
    let attach = |manager: Arc<ChatManager>| async move {
        tokio::task::spawn_blocking(move || manager.attach("s-restart", 0).expect("attach"))
            .await
            .expect("attach task")
    };
    let att = attach(Arc::clone(&restarted)).await;
    let mut seen = att.replay.clone();
    let mut rx = att.live;
    if echoes_of(&seen, "client-after1") == 0 {
        wait_for(&mut rx, &mut seen, "the second echo", |ev| {
            matches!(ev, AgentEvent::UserMessage { client_id: Some(id), .. } if id == "client-after1")
        })
        .await;
    }
    let replay = attach(Arc::clone(&restarted)).await.replay;
    assert_eq!(echoes_of(&replay, "client-before"), 1);
    assert_eq!(echoes_of(&replay, "client-after1"), 1);
    assert!(restarted.kill("s-restart"));
}

/// `cancel_send` wins only against a send that was never accepted: from then
/// on that id is refused. Against an accepted one it loses and changes
/// nothing.
#[tokio::test]
async fn cancel_send_wins_before_acceptance_and_loses_after() {
    let fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-cancel", &fx.cwd, "normal"))
        .expect("spawn");

    assert!(fx
        .manager
        .cancel_send("s-cancel", "client-withdrawn")
        .await
        .unwrap());
    let refused = fx
        .manager
        .send_from_client("s-cancel", text_send("late"), Some("client-withdrawn"))
        .await
        .expect_err("a cancelled id is refused");
    assert!(
        refused.downcast_ref::<SendCancelled>().is_some(),
        "{refused}"
    );

    let att = fx.manager.attach("s-cancel", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;
    let outcome = fx
        .manager
        .send_from_client("s-cancel", text_send("kept"), Some("client-accepted"))
        .await
        .expect("send");
    assert_eq!(outcome, SendOutcome::Accepted);
    assert!(!fx
        .manager
        .cancel_send("s-cancel", "client-accepted")
        .await
        .unwrap());
    wait_for(&mut rx, &mut seen, "the echo", |ev| {
        matches!(ev, AgentEvent::UserMessage { client_id: Some(id), .. } if id == "client-accepted")
    })
    .await;
    assert!(!fx
        .manager
        .cancel_send("s-cancel", "client-accepted")
        .await
        .unwrap());
    let replay = fx.manager.attach("s-cancel", 0).expect("attach").replay;
    assert_eq!(echoes_of(&replay, "client-withdrawn"), 0);
    assert!(
        !replay
            .iter()
            .any(|e| matches!(&e.ev, AgentEvent::UserMessage { text, .. } if text == "late")),
        "a cancelled send never reaches the agent"
    );
    assert!(fx.manager.kill("s-cancel"));
}

/// A send at the text limit, full of characters JSON escapes six to one, is
/// several times too long for one journal line. Its echo is still journaled
/// with both ids (the text cut), so the client confirms it, a resend is
/// dropped, and the next daemon life still knows the id.
#[tokio::test]
async fn a_send_too_long_for_one_journal_line_keeps_its_ids_and_runs_once() {
    use chimaera_agent::model::COMMAND_TEXT_TOTAL_MAX;
    let mut fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, spec("s-large", &fx.cwd, "normal"))
        .expect("spawn");
    let att = fx.manager.attach("s-large", 0).expect("attach");
    let mut seen = att.replay.clone();
    let mut rx = att.live;
    let unit = "\u{1}\"\\\u{7f}";
    let mut text = String::from("START ");
    while text.len() + unit.len() + 4 <= COMMAND_TEXT_TOTAL_MAX {
        text.push_str(unit);
    }
    text.push_str(" END");
    let outcome = fx
        .manager
        .send_from_client("s-large", text_send(&text), Some("client-large"))
        .await
        .expect("a send at the limit passes ingress");
    assert_eq!(outcome, SendOutcome::Accepted);
    let echo = wait_for(&mut rx, &mut seen, "the echo", |ev| {
        matches!(
            ev,
            AgentEvent::UserMessage { .. } | AgentEvent::Error { .. }
        )
    })
    .await;
    let AgentEvent::UserMessage {
        text: kept,
        id: Some(_),
        client_id: Some(client_id),
        ..
    } = &echo.ev
    else {
        panic!("the echo lost its ids: {:?}", echo.ev);
    };
    assert_eq!(client_id, "client-large");
    assert!(kept.starts_with("START ") && kept.ends_with(" END"));
    assert!(kept.len() < text.len() && kept.contains("bytes omitted"));
    let again = fx
        .manager
        .send_from_client("s-large", text_send(&text), Some("client-large"))
        .await
        .expect("resend");
    assert_eq!(again, SendOutcome::Duplicate);

    assert!(fx.manager.kill("s-large"));
    tokio::time::timeout(WAIT, fx.exits.recv())
        .await
        .expect("exit hook fired")
        .expect("channel open");
    let restarted = Arc::new(ChatManager::new(
        fx.manager.journal_dir().clone(),
        Box::new(|_, _| {}),
        Box::new(|_, _| {}),
    ));
    restarted
        .spawn(&ClaudeAdapter, spec("s-large", &fx.cwd, "normal"))
        .expect("spawn again");
    let after = restarted
        .send_from_client("s-large", text_send(&text), Some("client-large"))
        .await
        .expect("resend after the restart");
    assert_eq!(after, SendOutcome::Duplicate);
    assert!(restarted.kill("s-large"));
}

/// A confirmed withdrawal belongs to the logical conversation, including its
/// next process and a new daemon manager. A delayed original never runs there.
#[tokio::test]
async fn a_withdrawn_send_id_survives_process_replacement_and_restart() {
    let mut fx = fixture();
    fx.manager
        .spawn(&ClaudeAdapter, durable_spec("s-keeps", &fx.cwd, "normal"))
        .unwrap();
    assert!(fx
        .manager
        .cancel_send("s-keeps", "client-withdrawn")
        .await
        .unwrap());
    assert!(fx.manager.kill("s-keeps"));
    tokio::time::timeout(WAIT, fx.exits.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(fx.manager.remove("s-keeps").is_some());
    fx.manager
        .spawn(&ClaudeAdapter, durable_spec("s-keeps", &fx.cwd, "normal"))
        .unwrap();
    assert_eq!(
        fx.manager.client_id_state("s-keeps", "client-withdrawn"),
        Some(ClientIdState::Cancelled)
    );
    let late = fx
        .manager
        .send_from_client("s-keeps", text_send("late"), Some("client-withdrawn"))
        .await
        .unwrap_err();
    assert!(late.downcast_ref::<SendCancelled>().is_some());
    assert!(fx.manager.kill("s-keeps"));
    tokio::time::timeout(WAIT, fx.exits.recv())
        .await
        .unwrap()
        .unwrap();
    fx.manager.remove("s-keeps");
    let restarted = Arc::new(ChatManager::new(
        fx.manager.journal_dir().clone(),
        Box::new(|_, _| {}),
        Box::new(|_, _| {}),
    ));
    restarted
        .spawn(&ClaudeAdapter, durable_spec("s-keeps", &fx.cwd, "normal"))
        .unwrap();
    assert!(restarted
        .cancel_send("s-keeps", "client-withdrawn")
        .await
        .unwrap());
    let late = restarted
        .send_from_client("s-keeps", text_send("late"), Some("client-withdrawn"))
        .await
        .unwrap_err();
    assert!(late.downcast_ref::<SendCancelled>().is_some());
    assert!(restarted.kill("s-keeps"));
}

/// Receipt boundary fixture: accepts manager commands but emits no optimistic
/// echo. This models an agent receiving stdin just before daemon/driver loss.
struct ReceiptlessAdapter {
    received: mpsc::UnboundedSender<AgentCommand>,
    drain: tokio::sync::watch::Receiver<bool>,
}

struct QueuedReceiptAdapter {
    settle: bool,
    cancel: bool,
}
impl AgentAdapter for QueuedReceiptAdapter {
    fn kind(&self) -> &'static str {
        "claude"
    }
    fn spawn(
        &self,
        _spec: SpawnSpec,
        mut io: DriverIo,
    ) -> anyhow::Result<tokio::task::JoinHandle<DriverExit>> {
        let settle = self.settle;
        let cancel = self.cancel;
        Ok(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = io.kill.changed() => return DriverExit::Killed,
                    command = io.commands.recv() => match command {
                        Some(_) => {
                            let echo = AgentEvent::UserMessage { text:"queued".into(), attachments:0, id:Some("driver-queued".into()), queued:true, after_turn:true, origin:None, client_id:None, attachment_paths:vec![] };
                            if io.events.send(echo).await.is_err() { return DriverExit::Killed; }
                            if settle && io.events.send(AgentEvent::UserMessageUpdate { id:"driver-queued".into(), state:chimaera_agent::model::UserMessageState::Sent }).await.is_err() { return DriverExit::Killed; }
                            if cancel && io.events.send(AgentEvent::UserMessageUpdate { id:"driver-queued".into(), state:chimaera_agent::model::UserMessageState::Cancelled }).await.is_err() { return DriverExit::Killed; }
                        },
                        None => return DriverExit::Clean(None),
                    }
                }
            }
        }))
    }
}

#[tokio::test]
async fn queued_echo_is_uncertain_after_replacement_but_sent_update_confirms_it() {
    for settle in [false, true] {
        let mut fx = fixture();
        let adapter = QueuedReceiptAdapter {
            settle,
            cancel: false,
        };
        fx.manager
            .spawn(
                &adapter,
                durable_spec("s-queued-receipt", &fx.cwd, "normal"),
            )
            .unwrap();
        let mut live = fx.manager.attach("s-queued-receipt", 0).unwrap().live;
        fx.manager
            .send_from_client(
                "s-queued-receipt",
                text_send("queued"),
                Some("client-queued"),
            )
            .await
            .unwrap();
        loop {
            let event = tokio::time::timeout(WAIT, live.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(&event.ev, AgentEvent::UserMessage { client_id:Some(id), queued:true, .. } if id == "client-queued")
                && !settle
            {
                break;
            }
            if settle
                && matches!(
                    &event.ev,
                    AgentEvent::UserMessageUpdate {
                        state: chimaera_agent::model::UserMessageState::Sent,
                        ..
                    }
                )
            {
                break;
            }
        }
        assert_eq!(
            fx.manager
                .client_id_state("s-queued-receipt", "client-queued"),
            Some(if settle {
                ClientIdState::Confirmed
            } else {
                ClientIdState::Accepted
            })
        );
        assert_eq!(
            fx.manager.active_queued_ids("s-queued-receipt"),
            if settle {
                Vec::<String>::new()
            } else {
                vec!["client-queued".to_owned()]
            }
        );
        assert!(fx.manager.kill("s-queued-receipt"));
        tokio::time::timeout(WAIT, fx.exits.recv())
            .await
            .unwrap()
            .unwrap();
        fx.manager.remove("s-queued-receipt");
        let restarted = Arc::new(ChatManager::new(
            fx.manager.journal_dir().clone(),
            Box::new(|_, _| {}),
            Box::new(|_, _| {}),
        ));
        restarted
            .spawn(
                &adapter,
                durable_spec("s-queued-receipt", &fx.cwd, "normal"),
            )
            .unwrap();
        assert_eq!(
            restarted.client_id_state("s-queued-receipt", "client-queued"),
            Some(if settle {
                ClientIdState::Confirmed
            } else {
                ClientIdState::Uncertain
            })
        );
        let resend = restarted
            .send_from_client(
                "s-queued-receipt",
                text_send("queued"),
                Some("client-queued"),
            )
            .await;
        if settle {
            assert_eq!(resend.unwrap(), SendOutcome::Duplicate);
        } else {
            assert!(resend
                .unwrap_err()
                .downcast_ref::<SendUncertain>()
                .is_some());
        }
        assert!(restarted.kill("s-queued-receipt"));
    }
}

#[tokio::test]
async fn live_queued_cancellations_release_the_durable_cap_and_late_cancel_cannot_revoke_sent() {
    for settle in [false, true] {
        let mut fx = fixture();
        let adapter = QueuedReceiptAdapter {
            settle,
            cancel: true,
        };
        fx.manager
            .spawn(&adapter, durable_spec("s-cancel-queue", &fx.cwd, "normal"))
            .unwrap();
        let mut live = fx.manager.attach("s-cancel-queue", 0).unwrap().live;
        for n in 0..70 {
            let id = format!("client-cancel-{n:03}");
            assert_eq!(
                fx.manager
                    .send_from_client("s-cancel-queue", text_send("queued"), Some(&id))
                    .await
                    .unwrap(),
                SendOutcome::Accepted
            );
            loop {
                let event = tokio::time::timeout(WAIT, live.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if matches!(
                    &event.ev,
                    AgentEvent::UserMessageUpdate {
                        state: chimaera_agent::model::UserMessageState::Cancelled,
                        ..
                    }
                ) {
                    break;
                }
            }
            assert_eq!(
                fx.manager.client_id_state("s-cancel-queue", &id),
                Some(if settle {
                    ClientIdState::Confirmed
                } else {
                    ClientIdState::Cancelled
                })
            );
        }
        assert_eq!(
            fx.manager
                .send_from_client(
                    "s-cancel-queue",
                    text_send("still admissible"),
                    Some("client-after-cycles")
                )
                .await
                .unwrap(),
            SendOutcome::Accepted
        );
        assert!(fx.manager.kill("s-cancel-queue"));
        tokio::time::timeout(WAIT, fx.exits.recv())
            .await
            .unwrap()
            .unwrap();
        fx.manager.remove("s-cancel-queue");
        fx.manager
            .spawn(&adapter, durable_spec("s-cancel-queue", &fx.cwd, "normal"))
            .unwrap();
        assert_eq!(
            fx.manager
                .client_id_state("s-cancel-queue", "client-cancel-000"),
            Some(if settle {
                ClientIdState::Confirmed
            } else {
                ClientIdState::Cancelled
            })
        );
        assert!(fx.manager.kill("s-cancel-queue"));
    }
}
impl AgentAdapter for ReceiptlessAdapter {
    fn kind(&self) -> &'static str {
        "claude"
    }
    fn spawn(
        &self,
        _spec: SpawnSpec,
        mut io: DriverIo,
    ) -> anyhow::Result<tokio::task::JoinHandle<DriverExit>> {
        let received = self.received.clone();
        let mut drain = self.drain.clone();
        Ok(tokio::spawn(async move {
            while !*drain.borrow_and_update() {
                tokio::select! {
                    _ = io.kill.changed() => return DriverExit::Killed,
                    _ = drain.changed() => {},
                }
            }
            loop {
                tokio::select! {
                    _ = io.kill.changed() => return DriverExit::Killed,
                    command = io.commands.recv() => match command {
                        Some(command) => { let _ = received.send(command); },
                        None => return DriverExit::Clean(None),
                    }
                }
            }
        }))
    }
}

#[tokio::test]
async fn a_received_send_without_an_echo_is_uncertain_after_restart_and_never_replayed() {
    let mut fx = fixture();
    let (_drain, drain) = tokio::sync::watch::channel(true);
    let (received, mut commands) = mpsc::unbounded_channel();
    let adapter = ReceiptlessAdapter { received, drain };
    fx.manager
        .spawn(&adapter, durable_spec("s-unknown", &fx.cwd, "normal"))
        .unwrap();
    assert_eq!(
        fx.manager
            .send_from_client("s-unknown", text_send("once"), Some("client-unknown"))
            .await
            .unwrap(),
        SendOutcome::Accepted
    );
    tokio::time::timeout(WAIT, commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(fx.manager.kill("s-unknown"));
    tokio::time::timeout(WAIT, fx.exits.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fx.manager.client_id_state("s-unknown", "client-unknown"),
        Some(ClientIdState::Uncertain)
    );
    fx.manager.remove("s-unknown");
    assert_eq!(
        fx.manager.client_id_state("s-unknown", "client-unknown"),
        Some(ClientIdState::Uncertain)
    );
    assert!(fx
        .manager
        .send_from_client("s-unknown", text_send("once"), Some("client-unknown"))
        .await
        .unwrap_err()
        .downcast_ref::<SendUncertain>()
        .is_some());
    assert!(fx
        .manager
        .cancel_send("s-unknown", "client-unknown")
        .await
        .unwrap_err()
        .downcast_ref::<SendUncertain>()
        .is_some());
    let restarted = Arc::new(ChatManager::new(
        fx.manager.journal_dir().clone(),
        Box::new(|_, _| {}),
        Box::new(|_, _| {}),
    ));
    restarted
        .spawn(&adapter, durable_spec("s-unknown", &fx.cwd, "normal"))
        .unwrap();
    let resend = restarted
        .send_from_client("s-unknown", text_send("once"), Some("client-unknown"))
        .await
        .unwrap_err();
    assert!(resend.downcast_ref::<SendUncertain>().is_some());
    let cancel = restarted
        .cancel_send("s-unknown", "client-unknown")
        .await
        .unwrap_err();
    assert!(cancel.downcast_ref::<SendUncertain>().is_some());
    assert!(
        commands.try_recv().is_err(),
        "the agent must not receive a second command"
    );
    assert!(restarted.kill("s-unknown"));
}

#[tokio::test]
async fn an_independent_receipt_without_journal_echo_is_confirmed_and_never_resent() {
    let fx = fixture();
    let payload = serde_json::to_vec(&serde_json::json!({"version":1,"session_id":"s-receipt","entries":[{"id":"client-confirmed","state":"confirmed"}]})).unwrap();
    chimaera_agent::journal::import_send_state(fx.manager.journal_dir(), "s-receipt", &payload)
        .unwrap();
    let (_drain, drain) = tokio::sync::watch::channel(true);
    let (received, mut commands) = mpsc::unbounded_channel();
    let adapter = ReceiptlessAdapter { received, drain };
    fx.manager
        .spawn(&adapter, durable_spec("s-receipt", &fx.cwd, "normal"))
        .unwrap();
    assert_eq!(
        fx.manager.client_id_state("s-receipt", "client-confirmed"),
        Some(ClientIdState::Confirmed)
    );
    assert_eq!(
        fx.manager
            .send_from_client("s-receipt", text_send("once"), Some("client-confirmed"))
            .await
            .unwrap(),
        SendOutcome::Duplicate
    );
    assert!(!fx
        .manager
        .cancel_send("s-receipt", "client-confirmed")
        .await
        .unwrap());
    assert!(commands.try_recv().is_err());
    assert_eq!(
        fx.manager
            .send_from_client(
                "s-receipt",
                text_send(&"x".repeat(chimaera_agent::model::COMMAND_TEXT_TOTAL_MAX + 1)),
                Some("client-confirmed")
            )
            .await
            .unwrap(),
        SendOutcome::Duplicate
    );
    assert!(fx.manager.kill("s-receipt"));
}

#[tokio::test]
async fn canceling_an_enqueue_before_the_channel_permit_is_proven_undispatched() {
    let fx = fixture();
    let (drain, held) = tokio::sync::watch::channel(false);
    let (received, mut commands) = mpsc::unbounded_channel();
    let adapter = ReceiptlessAdapter {
        received,
        drain: held,
    };
    fx.manager
        .spawn(&adapter, durable_spec("s-backpressure", &fx.cwd, "normal"))
        .unwrap();
    // Fill the manager's bounded command channel without letting the driver
    // receive. No persisted dispatch is needed for these unkeyed fixture sends.
    for _ in 0..32 {
        fx.manager
            .command("s-backpressure", text_send("fill"))
            .await
            .unwrap();
    }
    let manager = fx.manager.clone();
    let pending = tokio::spawn(async move {
        manager
            .send_from_client(
                "s-backpressure",
                text_send("retryable"),
                Some("client-not-sent"),
            )
            .await
    });
    tokio::task::yield_now().await;
    pending.abort();
    let _ = pending.await;
    assert_eq!(
        fx.manager
            .client_id_state("s-backpressure", "client-not-sent"),
        None
    );
    drain.send(true).unwrap();
    assert_eq!(
        fx.manager
            .send_from_client(
                "s-backpressure",
                text_send("retryable"),
                Some("client-not-sent")
            )
            .await
            .unwrap(),
        SendOutcome::Accepted
    );
    for _ in 0..33 {
        tokio::time::timeout(WAIT, commands.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(commands.try_recv().is_err());
    assert!(fx.manager.kill("s-backpressure"));
}

#[tokio::test]
async fn failed_dispatch_storage_and_failed_withdrawal_never_reach_the_driver() {
    for cancel in [false, true] {
        let fx = fixture();
        let (_drain, drain) = tokio::sync::watch::channel(true);
        let (received, mut commands) = mpsc::unbounded_channel();
        let adapter = ReceiptlessAdapter { received, drain };
        fx.manager
            .spawn(&adapter, durable_spec("s-storage", &fx.cwd, "normal"))
            .unwrap();
        std::fs::create_dir(fx.manager.journal_dir().join("s-storage.send-state.json")).unwrap();
        if cancel {
            assert!(
                fx.manager
                    .cancel_send("s-storage", "client-storage")
                    .await
                    .is_err(),
                "never acknowledge a withdrawal without durable evidence"
            );
        } else {
            assert!(fx
                .manager
                .send_from_client("s-storage", text_send("never"), Some("client-storage"))
                .await
                .is_err());
        }
        let retry = fx
            .manager
            .send_from_client("s-storage", text_send("never"), Some("client-storage"))
            .await
            .unwrap_err();
        assert!(
            retry.downcast_ref::<SendUncertain>().is_some(),
            "storage uncertainty must fail closed: {retry}"
        );
        assert!(commands.try_recv().is_err());
        assert!(fx.manager.kill("s-storage"));
    }
}

#[tokio::test]
async fn elicitation_replays_without_persisting_the_submitted_values() {
    use chimaera_agent::elicitation::ElicitationAction;
    let f = fixture();
    f.manager
        .spawn(&ClaudeAdapter, spec("elicit", &f.cwd, "elicitation"))
        .expect("spawn");
    let mut rx = f.manager.attach("elicit", 0).unwrap().live;
    let mut seen = Vec::new();
    f.manager
        .command(
            "elicit",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "form".into(),
                }],
            },
        )
        .await
        .unwrap();
    wait_for(&mut rx, &mut seen, "MCP form", |event| {
        matches!(event, AgentEvent::ElicitationRequest { .. })
    })
    .await;
    let replay = f.manager.attach("elicit", 0).unwrap();
    assert!(replay
        .replay
        .iter()
        .any(|event| matches!(event.ev, AgentEvent::ElicitationRequest { .. })));
    f.manager
        .annotate(
            "elicit",
            AgentEvent::ElicitationRequest {
                request_id: "second".into(),
                server: "fixture".into(),
                message: "second ask".into(),
                elicitation: chimaera_agent::elicitation::Elicitation::parse(
                    "url",
                    &serde_json::Value::Null,
                    Some("https://example.com"),
                ),
            },
        )
        .unwrap();
    wait_for(&mut rx, &mut seen, "second MCP ask", |event| matches!(event, AgentEvent::ElicitationRequest { request_id, .. } if request_id == "second")).await;
    f.manager.command("elicit", AgentCommand::Elicitation { request_id: "req-elicit".into(), action: ElicitationAction::Accept, content: serde_json::json!({"name":"private-input-marker","count":0,"enabled":false,"profile":{"owner":"private-owner"}}) }).await.unwrap();
    wait_for(&mut rx, &mut seen, "MCP decision", |event| matches!(event, AgentEvent::ElicitationResolved { action, .. } if action == "accept")).await;
    assert!(
        f.manager.get("elicit").unwrap().pending_permission,
        "one unresolved request still needs attention"
    );
    f.manager
        .annotate(
            "elicit",
            AgentEvent::ElicitationResolved {
                request_id: "second".into(),
                action: "cancel".into(),
            },
        )
        .unwrap();
    wait_for(&mut rx, &mut seen, "second MCP decision", |event| matches!(event, AgentEvent::ElicitationResolved { request_id, .. } if request_id == "second")).await;
    assert!(!f.manager.get("elicit").unwrap().pending_permission);
    let replay = f.manager.attach("elicit", 0).unwrap();
    assert!(!serde_json::to_string(
        &replay
            .replay
            .iter()
            .map(|event| &**event)
            .collect::<Vec<_>>()
    )
    .unwrap()
    .contains("private-input-marker"));
    assert!(f.manager.kill("elicit"));
}
