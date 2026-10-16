//! Golden wire-contract tests for the PTY types.
//!
//! The daemon serializes these types STRAIGHT to the daemon<->UI wire — session
//! rows (`GET /api/v1/sessions`), `/ws/sessions` events, and exec responses — with
//! NO DTO layer in between. So a field rename/removal/retag here silently breaks
//! the Svelte client, which has reducer tests but no component/browser tests. These pin the exact
//! JSON shape: a behavior-preserving refactor (e.g. splitting this crate's modules)
//! MUST keep them green, and a *deliberate* wire change updates them in the same
//! commit so the break is visible in review.
//!
//! They test the crate's PUBLIC re-exports, so they survive a module reshuffle.

use chimaera_pty::{
    CommandSource, CommandView, ExecMode, ExecOutcome, SessionEvent, SessionInfo, ShellPhase,
};
use serde_json::json;

#[test]
fn session_info_wire_shape() {
    let info = SessionInfo {
        id: "s-abc123".to_string(),
        name: "web-ui".to_string(),
        cwd: "/home/u/proj".into(),
        cols: 80,
        rows: 24,
        created_at: 1_700_000_000,
        alive: true,
        exit_status: None,
        title: Some("vim".to_string()),
        pid: Some(4242),
        renamed: false,
        phase: ShellPhase::Ready,
        // serde(skip): output recency stays server-side; the daemon derives
        // an activity flag for the wire instead. Absent from the JSON below.
        last_output_at: 1_700_000_000_000,
    };
    assert_eq!(
        serde_json::to_value(&info).unwrap(),
        json!({
            "id": "s-abc123",
            "name": "web-ui",
            "cwd": "/home/u/proj",
            "cols": 80,
            "rows": 24,
            "created_at": 1_700_000_000_u64,
            "alive": true,
            "exit_status": null,
            "title": "vim",
            "pid": 4242,
            "renamed": false,
            "phase": "ready"
        })
    );
}

#[test]
fn session_event_wire_shapes() {
    // Internally tagged with `type`, snake_case.
    assert_eq!(
        serde_json::to_value(SessionEvent::Title { title: "t".into() }).unwrap(),
        json!({ "type": "title", "title": "t" })
    );
    assert_eq!(
        serde_json::to_value(SessionEvent::Resized {
            cols: 100,
            rows: 40
        })
        .unwrap(),
        json!({ "type": "resized", "cols": 100, "rows": 40 })
    );
    assert_eq!(
        serde_json::to_value(SessionEvent::Exited { status: Some(0) }).unwrap(),
        json!({ "type": "exited", "status": 0 })
    );
    assert_eq!(
        serde_json::to_value(SessionEvent::Exited { status: None }).unwrap(),
        json!({ "type": "exited", "status": null })
    );
    assert_eq!(
        serde_json::to_value(SessionEvent::Shell {
            phase: ShellPhase::Running
        })
        .unwrap(),
        json!({ "type": "shell", "phase": "running" })
    );
}

#[test]
fn shell_phase_wire_values() {
    assert_eq!(
        serde_json::to_value(ShellPhase::Unknown).unwrap(),
        json!("unknown")
    );
    assert_eq!(
        serde_json::to_value(ShellPhase::Ready).unwrap(),
        json!("ready")
    );
    assert_eq!(
        serde_json::to_value(ShellPhase::Running).unwrap(),
        json!("running")
    );
}

#[test]
fn command_source_wire_values() {
    assert_eq!(
        serde_json::to_value(CommandSource::User).unwrap(),
        json!("user")
    );
    assert_eq!(
        serde_json::to_value(CommandSource::Agent).unwrap(),
        json!("agent")
    );
}

#[test]
fn exec_outcome_wire_shape() {
    let outcome = ExecOutcome {
        record: CommandView {
            seq: 7,
            command: Some("ls -la".to_string()),
            source: CommandSource::Agent,
            cwd: Some("/home/u".to_string()),
            exit_code: Some(0),
            output: "total 0\n".to_string(),
            truncated_bytes: 0,
            started_at_ms: 1_700_000_000_000,
            ended_at_ms: Some(1_700_000_000_100),
            running: false,
        },
        mode: ExecMode::Integrated,
        waited_ms: 12,
        timed_out: false,
    };
    assert_eq!(
        serde_json::to_value(&outcome).unwrap(),
        json!({
            "record": {
                "seq": 7,
                "command": "ls -la",
                "source": "agent",
                "cwd": "/home/u",
                "exit_code": 0,
                "output": "total 0\n",
                "truncated_bytes": 0,
                "started_at_ms": 1_700_000_000_000_u64,
                "ended_at_ms": 1_700_000_000_100_u64,
                "running": false
            },
            "mode": "integrated",
            "waited_ms": 12,
            "timed_out": false
        })
    );
    // ExecMode's other variant.
    assert_eq!(
        serde_json::to_value(ExecMode::Sentinel).unwrap(),
        json!("sentinel")
    );
}

/// The PTY types as the daemon puts them on the wire, written to
/// `web-ui/src/lib/net/fixtures/wire/pty-types.json` for the web UI's
/// consumer tests: `SessionEvent` frames go through `terminal/ws.ts`
/// verbatim (`/ws/sessions` text frames), `SessionInfo` is the body of every
/// session row and the `ready` frame, and `ExecOutcome` answers
/// `POST /sessions/{id}/exec`.
#[test]
fn pty_types_fixture() {
    use chimaera_wire_fixtures::{case, pin};
    let info = |alive: bool, exit_status: Option<i32>| SessionInfo {
        id: "s-abc123".to_string(),
        name: "web-ui".to_string(),
        cwd: "/home/u/proj".into(),
        cols: 120,
        rows: 40,
        created_at: 1_700_000_000,
        alive,
        exit_status,
        title: Some("vim".to_string()),
        pid: alive.then_some(4242),
        renamed: false,
        phase: ShellPhase::Ready,
        last_output_at: 1_700_000_000_000,
    };
    fn to<T: serde::Serialize>(v: &T) -> serde_json::Value {
        serde_json::to_value(v).unwrap()
    }
    pin(
        "pty-types",
        "crates/chimaera-pty/tests/wire_contract.rs",
        "chimaera-pty's serialized types: SessionInfo (the row body + ready frame), the SessionEvent text frames of /ws/sessions, and ExecOutcome (POST /sessions/{id}/exec).",
        vec![
            case("session_info_live", "a live shell at its prompt", to(&info(true, None))),
            case(
                "session_info_exited",
                "a shell that exited 1",
                to(&SessionInfo {
                    renamed: true,
                    title: None,
                    ..info(false, Some(1))
                }),
            ),
            case(
                "event_title",
                "the terminal title changed (OSC 0/2)",
                to(&SessionEvent::Title {
                    title: "vim lib.rs".into(),
                }),
            ),
            case(
                "event_resized",
                "another client resized the grid",
                to(&SessionEvent::Resized {
                    cols: 100,
                    rows: 40,
                }),
            ),
            case(
                "event_exited",
                "the child exited 0",
                to(&SessionEvent::Exited { status: Some(0) }),
            ),
            case(
                "event_exited_unknown",
                "the child is gone without a status",
                to(&SessionEvent::Exited { status: None }),
            ),
            case(
                "event_shell_running",
                "OSC 133: a command is running",
                to(&SessionEvent::Shell {
                    phase: ShellPhase::Running,
                }),
            ),
            case(
                "exec_outcome",
                "an integrated-shell exec that finished",
                to(&ExecOutcome {
                    record: CommandView {
                        seq: 7,
                        command: Some("ls -la".to_string()),
                        source: CommandSource::Agent,
                        cwd: Some("/home/u".to_string()),
                        exit_code: Some(0),
                        output: "total 0\n".to_string(),
                        truncated_bytes: 0,
                        started_at_ms: 1_700_000_000_000,
                        ended_at_ms: Some(1_700_000_000_100),
                        running: false,
                    },
                    mode: ExecMode::Integrated,
                    waited_ms: 12,
                    timed_out: false,
                }),
            ),
            case(
                "exec_outcome_timed_out",
                "a sentinel exec still running when the wait expired",
                to(&ExecOutcome {
                    record: CommandView {
                        seq: 8,
                        command: Some("sleep 100".to_string()),
                        source: CommandSource::User,
                        cwd: None,
                        exit_code: None,
                        output: String::new(),
                        truncated_bytes: 2048,
                        started_at_ms: 1_700_000_000_000,
                        ended_at_ms: None,
                        running: true,
                    },
                    mode: ExecMode::Sentinel,
                    waited_ms: 5000,
                    timed_out: true,
                }),
            ),
        ],
    );
}
