//! Real ConPTY sessions on Windows: Windows PowerShell 5.1 (always in
//! System32) and cmd.exe. Each test drives a shell that has no client
//! attached until the test chooses to attach, the state the daemon spawns
//! every session in.

use std::time::Duration;

use alacritty_terminal::index::Point;
use bytes::Bytes;

use super::{opts, replay_snapshot, visible_cells, wait_for_event, wait_gone, CellSnap};
use crate::{ExecMode, ExecOptions, SessionEvent, SessionManager, ShellPhase};

/// Windows PowerShell takes 8-16 s to reach its first prompt in the test
/// VM, more with several starting at once.
const TIMEOUT: Duration = Duration::from_secs(90);

fn powershell() -> Option<Vec<String>> {
    Some(vec![
        "powershell.exe".to_string(),
        "-NoLogo".to_string(),
        "-NoProfile".to_string(),
    ])
}

/// Poll the server-side screen (no attachment, so no live receiver) until
/// it shows `needle`.
async fn wait_screen(mgr: &SessionManager, id: &str, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let text = mgr.screen_text(id, 200).expect("session vanished");
        if text.contains(needle) {
            return text;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for {needle:?} on screen; screen: {text:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn type_line(mgr: &SessionManager, id: &str, line: &str) {
    mgr.session(id)
        .expect("session vanished")
        .input()
        .send(Bytes::from(format!("{line}\r")))
        .await
        .expect("input send failed");
}

async fn wait_phase(mgr: &SessionManager, id: &str, phase: ShellPhase) {
    let marks = mgr.marks(id).expect("session vanished");
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while marks.phase() != phase {
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for phase {phase:?}; screen: {:?}",
                mgr.screen_text(id, 50)
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn exec_opts(command: &str) -> ExecOptions {
    ExecOptions {
        command: command.to_string(),
        queue_timeout: Duration::from_secs(15),
        timeout: Duration::from_secs(90),
        allow_sentinel_over_running: false,
        stage: None,
    }
}

/// ConPTY asks for the cursor position at startup and draws nothing until
/// answered. With no client attached the headless terminal must answer, or
/// the prompt never appears. Then the shell takes input, follows a resize,
/// and a snapshot rebuilds the live grid.
#[tokio::test]
async fn headless_powershell_prompts_runs_resizes_and_snapshots() {
    let mgr = SessionManager::new();
    let info = mgr.spawn(opts(powershell())).expect("spawn failed");

    wait_screen(&mgr, &info.id, "PS ").await;
    // $(6*7) keeps the typed line distinct from its output.
    type_line(&mgr, &info.id, "echo chimaera-$(6*7)").await;
    wait_screen(&mgr, &info.id, "chimaera-42").await;

    let mut att = mgr.attach(&info.id).expect("attach failed");
    mgr.resize(&info.id, 100, 30).expect("resize failed");
    match wait_for_event(&mut att.events, |e| {
        matches!(e, SessionEvent::Resized { .. })
    })
    .await
    {
        SessionEvent::Resized { cols, rows } => assert_eq!((cols, rows), (100, 30)),
        other => panic!("unexpected event: {other:?}"),
    }
    drop(att);
    type_line(
        &mgr,
        &info.id,
        "echo \"size=$([Console]::WindowWidth)x$([Console]::WindowHeight)\"",
    )
    .await;
    wait_screen(&mgr, &info.id, "size=100x30").await;
    let info = mgr.get(&info.id).expect("session present");
    assert_eq!((info.cols, info.rows), (100, 30));

    // Snapshot round-trip once the shell is idle: ConPTY repaints, so the
    // grid is compared after the output settles.
    let session = mgr.session(&info.id).expect("session present");
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let att = mgr.attach(&info.id).expect("attach failed");
        let (live, cursor): (Vec<Vec<CellSnap>>, Point) =
            session.with_term(|t| (visible_cells(t), t.grid().cursor.point));
        let replayed = replay_snapshot(&att.snapshot, info.cols, info.rows);
        if visible_cells(&replayed) == live && replayed.grid().cursor.point == cursor {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "snapshot replay never matched the live grid"
        );
    }

    mgr.kill(&info.id).expect("kill failed");
    wait_gone(&mgr, &info.id).await;
}

/// Killing a session ends the shell's whole process tree, not just the
/// shell: a ping started from it dies too.
#[tokio::test]
async fn kill_ends_the_process_tree() {
    let mgr = SessionManager::new();
    let info = mgr.spawn(opts(powershell())).expect("spawn failed");
    wait_screen(&mgr, &info.id, "PS ").await;
    type_line(
        &mgr,
        &info.id,
        "$p = Start-Process ping -ArgumentList '-n','30','127.0.0.1' -NoNewWindow -PassThru; \"ping-pid=$($p.Id);\"",
    )
    .await;
    // The typed line also shows `ping-pid=$(`; only the output has digits.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let pid: u32 = loop {
        let screen = mgr.screen_text(&info.id, 200).expect("session vanished");
        if let Some(pid) = screen
            .split("ping-pid=")
            .skip(1)
            .find_map(|rest| rest.split(';').next()?.trim().parse().ok())
        {
            break pid;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no ping pid on screen: {screen:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(chimaera_core::proc::pid_alive(pid), "ping not running");

    mgr.kill(&info.id).expect("kill failed");
    wait_gone(&mgr, &info.id).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while chimaera_core::proc::pid_alive(pid) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "ping {pid} outlived its session"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A typed Ctrl+C interrupts the running program even when the daemon
/// ignores Ctrl+C, as one started in its own process group does: the
/// attribute is inherited, and a session must not hand it to its shell.
#[tokio::test]
async fn ctrl_c_interrupts_a_program_when_the_daemon_ignores_it() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    // SAFETY: a null handler registers nothing; TRUE only sets this
    // process's ignore-Ctrl+C attribute, as CREATE_NEW_PROCESS_GROUP does.
    assert_ne!(unsafe { SetConsoleCtrlHandler(None, 1) }, 0);

    let (mgr, id) = mgr_spawn(opts(powershell()));
    wait_screen(&mgr, &id, "PS ").await;
    type_line(&mgr, &id, "ping -t 127.0.0.1").await;
    // Only ping's replies carry TTL=, in every display language.
    wait_screen(&mgr, &id, "TTL=").await;
    mgr.session(&id)
        .expect("session vanished")
        .input()
        .send(Bytes::from_static(b"\x03"))
        .await
        .expect("input send failed");
    // The prompt is back once ping stops; until then every line it prints
    // is a reply. Type only then: a Ctrl+C flushes the console's pending input.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let screen = mgr.screen_text(&id, 200).expect("session vanished");
        if screen.trim_end().ends_with('>') {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "ping ignored the Ctrl+C; screen: {screen:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    type_line(&mgr, &id, "echo after-$(2*21)").await;
    wait_screen(&mgr, &id, "after-42").await;

    mgr.kill(&id).expect("kill failed");
    wait_gone(&mgr, &id).await;
}

/// The PowerShell integration's marks survive ConPTY: the prompt reads as
/// ready, a typed command lands in the journal with its command line and
/// native exit code, and an agent exec runs in integrated mode.
#[tokio::test]
async fn powershell_integration_marks_reach_the_journal() {
    let base = std::env::temp_dir().join(format!("chimaera-pty-psint-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let launch =
        chimaera_core::shellint::shell_launch_for("powershell.exe", &base).expect("shell launch");
    let mut o = opts(Some(launch.argv));
    o.env = launch.env;
    let info = mgr_spawn(o);
    let (mgr, id) = (&info.0, info.1.as_str());

    wait_phase(mgr, id, ShellPhase::Ready).await;
    type_line(mgr, id, "cmd /c exit 3").await;
    let marks = mgr.marks(id).expect("marks");
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let record = loop {
        if let Some(r) = marks.journal(10).into_iter().find(|r| !r.running) {
            break r;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no finished record; screen: {:?}",
            mgr.screen_text(id, 50)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(record.command.as_deref(), Some("cmd /c exit 3"));
    assert_eq!(record.exit_code, Some(3));
    let cwd = record.cwd.expect("OSC 7 cwd");
    assert!(
        cwd.as_bytes().get(1) == Some(&b':') && !cwd.contains('\\'),
        "cwd not in wire form: {cwd}"
    );

    let outcome = mgr
        .exec(id, exec_opts("echo agent-$(2+3)"))
        .await
        .expect("exec");
    assert_eq!(outcome.mode, ExecMode::Integrated);
    assert_eq!(outcome.record.exit_code, Some(0));
    assert!(
        outcome.record.output.contains("agent-5"),
        "{:?}",
        outcome.record.output
    );

    mgr.kill(id).expect("kill failed");
    wait_gone(mgr, id).await;
    std::fs::remove_dir_all(&base).ok();
}

/// Without integration, execs fall back to the sentinel line in the
/// shell's own language and still carry the output and the exit code.
#[tokio::test]
async fn sentinel_execs_in_plain_powershell_and_cmd() {
    for (shell, ready, command, output, code) in [
        (
            powershell(),
            "PS ",
            "echo ps-$(3*3); cmd /c exit 4",
            "ps-9",
            4,
        ),
        (
            Some(vec!["cmd.exe".to_string()]),
            ">",
            "echo cmd-out & cmd /c exit 5",
            "cmd-out",
            5,
        ),
    ] {
        let (mgr, id) = mgr_spawn(opts(shell));
        wait_screen(&mgr, &id, ready).await;
        let outcome = match mgr.exec(&id, exec_opts(command)).await {
            Ok(o) => o,
            Err(e) => panic!("{command}: {e}; screen: {:?}", mgr.screen_text(&id, 50)),
        };
        assert_eq!(outcome.mode, ExecMode::Sentinel);
        assert_eq!(outcome.record.exit_code, Some(code), "{command}");
        assert!(
            outcome.record.output.contains(output),
            "{command}: {:?}",
            outcome.record.output
        );
        mgr.kill(&id).expect("kill failed");
        wait_gone(&mgr, &id).await;
    }
}

fn mgr_spawn(o: crate::SpawnOpts) -> (std::sync::Arc<SessionManager>, String) {
    let mgr = SessionManager::new();
    let id = mgr.spawn(o).expect("spawn failed").id;
    (mgr, id)
}
