//! Integration tests running real PTYs. These exercise the whole engine:
//! spawn, attach, detach/reattach persistence, snapshot fidelity, resize,
//! and multi-attach fan-out.

use std::time::Duration;

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, StdSyncHandler};
use bytes::Bytes;
use tokio::sync::broadcast;

use crate::{Attachment, SessionEvent, SessionManager, SpawnOpts};

const TIMEOUT: Duration = Duration::from_secs(15);

/// Style flags compared in fidelity assertions (mirrors the set the snapshot
/// renderer serializes).
const STYLE_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::DOUBLE_UNDERLINE)
    .union(Flags::UNDERCURL)
    .union(Flags::DOTTED_UNDERLINE)
    .union(Flags::DASHED_UNDERLINE)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

fn opts(command: Option<Vec<String>>) -> SpawnOpts {
    SpawnOpts {
        cwd: std::env::temp_dir(),
        name: None,
        cols: 80,
        rows: 24,
        command,
        id: None,
    }
}

fn bash() -> Option<Vec<String>> {
    Some(vec![
        "/bin/bash".to_string(),
        "--norc".to_string(),
        "--noprofile".to_string(),
    ])
}

/// Accumulate broadcast output (lossy UTF-8) until it contains `needle`.
async fn read_until(rx: &mut broadcast::Receiver<Bytes>, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let mut acc = String::new();
    loop {
        if acc.contains(needle) {
            return acc;
        }
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(chunk)) => acc.push_str(&String::from_utf8_lossy(&chunk)),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                panic!("output channel closed before {needle:?} appeared; got: {acc:?}")
            }
            Err(_) => panic!("timed out waiting for {needle:?}; got: {acc:?}"),
        }
    }
}

/// Wait for the first event matching `pred`.
async fn wait_for_event(
    rx: &mut broadcast::Receiver<SessionEvent>,
    mut pred: impl FnMut(&SessionEvent) -> bool,
) -> SessionEvent {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => {
                if pred(&event) {
                    return event;
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => panic!("event channel closed"),
            Err(_) => panic!("timed out waiting for matching event"),
        }
    }
}

/// Poll until the session unregisters itself (reap-on-exit semantics:
/// an exited session vanishes from the registry, tmux-style).
async fn wait_gone(mgr: &SessionManager, id: &str) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while mgr.get(id).is_some() {
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for session {id} to unregister");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Poll fresh attachments until the snapshot contains `needle`.
async fn attach_when_snapshot_contains(mgr: &SessionManager, id: &str, needle: &str) -> Attachment {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let att = mgr.attach(id).expect("attach failed");
        if String::from_utf8_lossy(&att.snapshot).contains(needle) {
            return att;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for snapshot to contain {needle:?}; snapshot: {:?}",
                String::from_utf8_lossy(&att.snapshot)
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// --- snapshot fidelity plumbing ------------------------------------------

struct NoopListener;

impl EventListener for NoopListener {
    fn send_event(&self, _event: TermEvent) {}
}

struct TestDims {
    cols: u16,
    rows: u16,
}

impl Dimensions for TestDims {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// Feed a snapshot byte stream into a brand-new headless terminal, the same
/// way xterm.js would consume it.
fn replay_snapshot(snapshot: &[u8], cols: u16, rows: u16) -> Term<NoopListener> {
    let config = TermConfig {
        scrolling_history: 10_000,
        ..TermConfig::default()
    };
    let mut term = Term::new(config, &TestDims { cols, rows }, NoopListener);
    let mut parser: Processor<StdSyncHandler> = Processor::new();
    parser.advance(&mut term, snapshot);
    term
}

type CellSnap = (char, Color, Color, Flags);

/// Extract the visible grid as (char, fg, bg, style-flags) per cell.
fn visible_cells<T>(term: &Term<T>) -> Vec<Vec<CellSnap>> {
    let grid = term.grid();
    (0..grid.screen_lines() as i32)
        .map(|line| {
            let row = &grid[Line(line)];
            (0..grid.columns())
                .map(|col| {
                    let cell = &row[Column(col)];
                    (cell.c, cell.fg, cell.bg, cell.flags & STYLE_FLAGS)
                })
                .collect()
        })
        .collect()
}

fn visible_text<T>(term: &Term<T>) -> Vec<String> {
    visible_cells(term)
        .iter()
        .map(|row| {
            row.iter()
                .map(|&(c, ..)| c)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

// --- tests -----------------------------------------------------------------

/// (a) A plain command produces output on the live stream and an Exited event,
/// then the session unregisters itself.
#[tokio::test]
async fn spawn_echo_collects_output_and_exited_event() {
    let mgr = SessionManager::new();
    let info = mgr
        .spawn(opts(Some(vec![
            "/bin/echo".to_string(),
            "hello-from-pty".to_string(),
        ])))
        .expect("spawn failed");
    assert!(info.id.starts_with("s-"));
    assert!(info.alive);

    let mut att = mgr.attach(&info.id).expect("attach failed");
    // The output may already be in the snapshot if echo raced ahead of us.
    let mut seen = String::from_utf8_lossy(&att.snapshot).into_owned();
    if !seen.contains("hello-from-pty") {
        seen.push_str(&read_until(&mut att.output, "hello-from-pty").await);
    }
    assert!(seen.contains("hello-from-pty"), "output was: {seen:?}");

    let event = wait_for_event(&mut att.events, |e| {
        matches!(e, SessionEvent::Exited { .. })
    })
    .await;
    match event {
        SessionEvent::Exited { status } => assert_eq!(status, Some(0)),
        other => panic!("unexpected event: {other:?}"),
    }

    // After Exited is published the session reaps itself out of the registry.
    wait_gone(&mgr, &info.id).await;
}

/// (b) Server-side state survives detach: output produced while attached is
/// present in the snapshot of a later, completely fresh attachment.
#[tokio::test]
async fn detached_session_state_survives_in_snapshot() {
    let mgr = SessionManager::new();
    let info = mgr.spawn(opts(bash())).expect("spawn failed");

    {
        let mut att = mgr.attach(&info.id).expect("attach failed");
        // $((6*7)) distinguishes the command echo from its output.
        att.input
            .send(Bytes::from_static(b"echo marker-$((6*7))\n"))
            .await
            .expect("input send failed");
        read_until(&mut att.output, "marker-42").await;
        // Attachment fully dropped here: receivers and input sender all gone.
    }

    let att = mgr.attach(&info.id).expect("re-attach failed");
    let snapshot = String::from_utf8_lossy(&att.snapshot).into_owned();
    assert!(
        snapshot.contains("marker-42"),
        "fresh snapshot must contain output produced before detach; snapshot: {snapshot:?}"
    );
    assert!(mgr.get(&info.id).expect("session still listed").alive);

    mgr.kill(&info.id).expect("kill failed");
    wait_gone(&mgr, &info.id).await;
    // Killing an unregistered session is Ok(()) — deletes stay idempotent.
    mgr.kill(&info.id).expect("second kill must be a no-op");
}

/// (c) Snapshot fidelity: replaying the snapshot into a fresh terminal
/// reproduces the live session's visible grid cell-for-cell, including
/// color/bold attributes and the cursor position.
#[tokio::test]
async fn snapshot_replay_matches_live_grid() {
    let mgr = SessionManager::new();
    // The trailing sleep keeps the session alive (reap-on-exit would remove
    // it) while leaving exactly the printf output on the grid.
    let script =
        "printf 'plain \\033[31mred\\033[0m \\033[1mbold\\033[0m\\nsecond line\\n'; sleep 30";
    let info = mgr
        .spawn(opts(Some(vec![
            "/bin/bash".to_string(),
            "-c".to_string(),
            script.to_string(),
        ])))
        .expect("spawn failed");

    let att = attach_when_snapshot_contains(&mgr, &info.id, "second line").await;

    let replayed = replay_snapshot(&att.snapshot, info.cols, info.rows);
    let session = mgr.session(&info.id).expect("session present");
    let (live_cells, live_cursor): (Vec<Vec<CellSnap>>, Point) =
        session.with_term(|term| (visible_cells(term), term.grid().cursor.point));

    let replay_text = visible_text(&replayed);
    assert_eq!(replay_text[0], "plain red bold");
    assert_eq!(replay_text[1], "second line");

    // Explicit attribute spot-checks: 'r' of "red" is red, 'b' of "bold" is
    // bold ("plain " = cols 0..6, "red" = 6..9, "bold" starts at col 10).
    let replay_cells = visible_cells(&replayed);
    let (c, fg, _, flags) = replay_cells[0][6];
    assert_eq!(c, 'r');
    assert_eq!(fg, Color::Named(NamedColor::Red));
    assert!(!flags.contains(Flags::BOLD));
    let (c, _, _, flags) = replay_cells[0][10];
    assert_eq!(c, 'b');
    assert!(flags.contains(Flags::BOLD));

    // Cell-for-cell equality with the live server-side grid.
    assert_eq!(replay_cells.len(), live_cells.len());
    for (row_idx, (replay_row, live_row)) in replay_cells.iter().zip(live_cells.iter()).enumerate()
    {
        assert_eq!(
            replay_row, live_row,
            "row {row_idx} differs between replayed snapshot and live grid"
        );
    }
    assert_eq!(replayed.grid().cursor.point, live_cursor);

    mgr.kill(&info.id).expect("kill failed");
    wait_gone(&mgr, &info.id).await;
}

/// (d) resize() changes the child's winsize (visible via `stty size`) and
/// broadcasts a Resized event.
#[tokio::test]
async fn resize_reaches_child_and_broadcasts_event() {
    let mgr = SessionManager::new();
    let info = mgr.spawn(opts(bash())).expect("spawn failed");
    let mut att = mgr.attach(&info.id).expect("attach failed");

    mgr.resize(&info.id, 100, 30).expect("resize failed");
    let event = wait_for_event(&mut att.events, |e| {
        matches!(e, SessionEvent::Resized { .. })
    })
    .await;
    match event {
        SessionEvent::Resized { cols, rows } => {
            assert_eq!((cols, rows), (100, 30));
        }
        other => panic!("unexpected event: {other:?}"),
    }

    att.input
        .send(Bytes::from_static(b"stty size\n"))
        .await
        .expect("input send failed");
    // stty prints "rows cols".
    read_until(&mut att.output, "30 100").await;

    let info = mgr.get(&info.id).expect("session present");
    assert_eq!((info.cols, info.rows), (100, 30));

    mgr.kill(&info.id).expect("kill failed");
    wait_gone(&mgr, &info.id).await;
}

/// (e) Two concurrent attachments both receive subsequent output.
#[tokio::test]
async fn multi_attach_fans_out_output() {
    let mgr = SessionManager::new();
    let info = mgr.spawn(opts(bash())).expect("spawn failed");

    let mut att1 = mgr.attach(&info.id).expect("first attach failed");
    let mut att2 = mgr.attach(&info.id).expect("second attach failed");

    att1.input
        .send(Bytes::from_static(b"echo multi-$((5*9))\n"))
        .await
        .expect("input send failed");

    let out1 = read_until(&mut att1.output, "multi-45").await;
    let out2 = read_until(&mut att2.output, "multi-45").await;
    assert!(out1.contains("multi-45"));
    assert!(out2.contains("multi-45"));

    mgr.kill(&info.id).expect("kill failed");
    wait_gone(&mgr, &info.id).await;
}
