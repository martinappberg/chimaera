//! Live protocol smoke tests against the REAL installed `claude` and `codex`
//! binaries. They spend a few cents of real usage and need network + auth, so
//! they are `#[ignore]`d in CI; run them with `just chat-smoke` whenever the
//! protocol clients change or the CLIs update.
//!
//! These tests ARE the compatibility contract: if one fails after a CLI
//! update, the wire format drifted and `claude.rs` / `codex.rs` (and their
//! `TESTED_*_VERSION` pins) need re-verification.

use std::time::Duration;

use serde_json::{json, Value};

use chimaera_agent::claude::{chat_args, ClaudeAdapter, ClaudeChat, PermissionDecision};
use chimaera_agent::codex::CodexChat;
use chimaera_agent::driver::SpawnSpec;
use chimaera_agent::model::{AgentCommand, AgentEvent, ContentBlock, UserMessageState};
use chimaera_agent::{ChatManager, EventHook, ExitHook};

const HANDSHAKE: Duration = Duration::from_secs(20);
const TURN: Duration = Duration::from_secs(120);
/// Cheapest real model; keeps a full smoke run in single-digit cents.
const CLAUDE_TEST_MODEL: &str = "haiku";

fn tmpdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("chimaera-chat-smoke-")
        .tempdir()
        .expect("create tempdir")
}

fn spawn_claude(cwd: &std::path::Path, extra_args: &[String]) -> ClaudeChat {
    ClaudeChat::spawn("claude", cwd, Some(CLAUDE_TEST_MODEL), None, extra_args)
        .expect("spawn claude")
}

/// Drive frames until a `result` message; returns (init, result, saw_stream_event).
async fn run_turn_to_result(chat: &mut ClaudeChat) -> (Value, Value, bool) {
    let mut init = Value::Null;
    let mut saw_stream_event = false;
    loop {
        let frame = chat
            .recv(TURN)
            .await
            .expect("recv frame")
            .expect("claude exited before result");
        match frame["type"].as_str() {
            Some("system") if frame["subtype"] == "init" => init = frame,
            Some("stream_event") => saw_stream_event = true,
            Some("result") => return (init, frame, saw_stream_event),
            _ => {}
        }
    }
}

#[tokio::test]
#[ignore = "live: spawns real claude, needs auth"]
async fn claude_handshake_answers_before_any_turn() {
    let dir = tmpdir();
    let mut chat = spawn_claude(dir.path(), &[]);

    let init = chat.initialize(HANDSHAKE).await.expect("initialize");
    let commands = init["commands"]
        .as_array()
        .expect("initialize response carries slash-command catalog");
    assert!(!commands.is_empty(), "expected at least one slash command");
    assert!(
        commands.iter().all(|c| c["name"].is_string()),
        "each command has a name"
    );
    // The account model catalog rides the same response: `value` ids for
    // set_model plus per-model effort levels (the /model + /effort menus).
    let models = init["models"]
        .as_array()
        .expect("initialize response carries the model catalog");
    assert!(!models.is_empty(), "expected at least one model");
    assert!(
        models.iter().all(|m| m["value"].is_string()),
        "each model has a picker value"
    );
    assert!(
        models.iter().any(|m| m["supportedEffortLevels"].is_array()),
        "at least one model reports effort levels"
    );

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

#[tokio::test]
#[ignore = "live: spawns real claude, needs auth, bills one tiny turn"]
async fn claude_echo_turn_streams_deltas_and_reports_cost() {
    let dir = tmpdir();
    let mut chat = spawn_claude(dir.path(), &[]);
    chat.initialize(HANDSHAKE).await.expect("initialize");

    chat.send_user_text("Reply with exactly: ok")
        .await
        .expect("send");
    let (init, result, saw_stream_event) = run_turn_to_result(&mut chat).await;

    let session_id = init["session_id"].as_str().expect("init.session_id");
    assert!(!session_id.is_empty());
    assert!(
        init["slash_commands"].is_array(),
        "init carries slash_commands"
    );
    assert!(saw_stream_event, "--include-partial-messages deltas flowed");
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["session_id"], json!(session_id));
    assert!(
        result["total_cost_usd"].as_f64().unwrap_or(0.0) > 0.0,
        "result reports real cost"
    );

    // 2.1.207+ MAY follow the result with a `post_turn_summary` status line
    // (the SessionStatus event's wire source). Live it fires on
    // workflow-lifecycle turns, NOT on a bare echo turn (verified against
    // 2.1.211, 2026-07-16) — so listen briefly and pin the shape only if
    // one shows up; its absence here is the expected outcome.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            println!("post_turn_summary: not emitted after a bare echo turn (expected)");
            break;
        }
        match chat.recv(left).await {
            Ok(Some(frame))
                if frame["type"] == "system" && frame["subtype"] == "post_turn_summary" =>
            {
                let detail = frame["status_detail"]
                    .as_str()
                    .expect("status_detail is a string");
                assert!(!detail.is_empty(), "status_detail carries a line");
                println!(
                    "post_turn_summary: category={} needs_action={} detail={detail:?}",
                    frame["status_category"], frame["needs_action"]
                );
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) => panic!("claude exited right after the result"),
            Err(_) => {
                println!("post_turn_summary: not emitted after a bare echo turn (expected)");
                break;
            }
        }
    }

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

#[tokio::test]
#[ignore = "live: spawns real claude twice, needs auth, bills two tiny turns"]
async fn claude_permission_roundtrip_allow_then_deny() {
    // Allow: the tool must actually run (file appears on disk).
    let allow_dir = tmpdir();
    let mut chat = spawn_claude(allow_dir.path(), &[]);
    chat.initialize(HANDSHAKE).await.expect("initialize");
    chat.send_user_text(
        "Create a file named probe.txt containing exactly hello, using the Bash tool.",
    )
    .await
    .expect("send");

    let mut permission_seen = false;
    loop {
        let frame = chat
            .recv(TURN)
            .await
            .expect("recv")
            .expect("claude exited before result");
        if frame["type"] == "control_request" && frame["request"]["subtype"] == "can_use_tool" {
            permission_seen = true;
            assert!(
                frame["request"]["tool_use_id"].is_string(),
                "can_use_tool carries tool_use_id (anchors the UI card)"
            );
            let input = frame["request"]["input"].clone();
            chat.respond_permission(
                &frame["request_id"],
                PermissionDecision::Allow {
                    updated_input: input,
                },
            )
            .await
            .expect("respond allow");
        } else if frame["type"] == "result" {
            break;
        }
    }
    assert!(permission_seen, "a can_use_tool request was routed to us");
    assert!(
        allow_dir.path().join("probe.txt").exists(),
        "allowed tool actually ran"
    );
    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");

    // Deny: the tool result must be an error and the file must NOT exist.
    let deny_dir = tmpdir();
    let mut chat = spawn_claude(deny_dir.path(), &[]);
    chat.initialize(HANDSHAKE).await.expect("initialize");
    chat.send_user_text(
        "Create a file named probe.txt containing exactly hello, using the Bash tool.",
    )
    .await
    .expect("send");

    let mut denied_tool_result = false;
    loop {
        let frame = chat
            .recv(TURN)
            .await
            .expect("recv")
            .expect("claude exited before result");
        if frame["type"] == "control_request" && frame["request"]["subtype"] == "can_use_tool" {
            // Deny every attempt — the model may retry with another tool.
            chat.respond_permission(
                &frame["request_id"],
                PermissionDecision::Deny {
                    message: "User rejected this action".into(),
                    interrupt: true,
                },
            )
            .await
            .expect("respond deny");
        } else if frame["type"] == "user" {
            if let Some(blocks) = frame["message"]["content"].as_array() {
                for block in blocks {
                    if block["type"] == "tool_result" && block["is_error"] == json!(true) {
                        denied_tool_result = true;
                    }
                }
            }
        } else if frame["type"] == "result" {
            break;
        }
    }
    assert!(
        denied_tool_result,
        "denial surfaced as an error tool_result"
    );
    assert!(
        !deny_dir.path().join("probe.txt").exists(),
        "denied tool must not run"
    );
    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// The background-task lanes (chimaera's background tray rides these): a
/// backgrounded Bash emits `task_started {task_type:"local_bash",
/// tool_use_id}` + `background_tasks_changed` (the running set) during the
/// turn, and settles OUTSIDE any turn with a set-emptying
/// `background_tasks_changed` plus a `task_notification {status, summary,
/// output_file}` verdict — the frames the driver's departed buffer exists
/// for. Order within the settle is deliberately not pinned here (chimaera
/// tolerates either); presence and shape are the contract.
#[tokio::test]
#[ignore = "live: spawns real claude, needs auth, bills one tiny turn + a 5s background sleep"]
async fn claude_background_task_lanes_start_and_settle() {
    let dir = tmpdir();
    let mut chat = spawn_claude(dir.path(), &[]);
    chat.initialize(HANDSHAKE).await.expect("initialize");
    chat.send_user_text(
        "Run `sleep 5 && echo done` as a BACKGROUND Bash task (run_in_background: true). \
         Confirm it started; do not wait for it or poll it.",
    )
    .await
    .expect("send");

    // Drive the turn: allow the (possibly auto-allowed) Bash call, collect
    // the start-side background frames.
    //
    // The settle frames are collected HERE too, not only in the second loop:
    // a 5s task can finish before the turn's `result` lands (turn length
    // varies with the model's thinking), and frames this loop discarded were
    // then waited for forever. Presence is the contract, not which side of
    // the result they fall on.
    let mut started = Value::Null;
    let mut changed_with_task = false;
    // Every notification seen, not just the last: a second lane's verdict
    // would otherwise clobber the one we're waiting for, and the id check
    // below would then send us back to a 60s wait for a frame already gone.
    let mut notifications: Vec<Value> = Vec::new();
    let mut emptied = false;
    loop {
        let frame = chat
            .recv(TURN)
            .await
            .expect("recv")
            .expect("claude exited before result");
        if frame["type"] == "control_request" && frame["request"]["subtype"] == "can_use_tool" {
            let input = frame["request"]["input"].clone();
            chat.respond_permission(
                &frame["request_id"],
                PermissionDecision::Allow {
                    updated_input: input,
                },
            )
            .await
            .expect("respond allow");
        } else if frame["type"] == "system" && frame["subtype"] == "task_started" {
            started = frame;
        } else if frame["type"] == "system" && frame["subtype"] == "background_tasks_changed" {
            let tasks = frame["tasks"].as_array();
            changed_with_task |= tasks.is_some_and(|t| !t.is_empty());
            // Only counts as the SETTLE emptying once the task was listed.
            emptied |= changed_with_task && tasks.is_some_and(|t| t.is_empty());
        } else if frame["type"] == "system" && frame["subtype"] == "task_notification" {
            notifications.push(frame);
        } else if frame["type"] == "result" {
            break;
        }
    }
    assert_eq!(
        started["task_type"],
        json!("local_bash"),
        "backgrounded bash rides task_started with task_type local_bash"
    );
    assert!(
        started["tool_use_id"].is_string(),
        "task_started binds to the spawning tool_use"
    );
    assert!(
        changed_with_task,
        "background_tasks_changed carried the running set"
    );
    let task_id = started["task_id"].as_str().expect("task_id").to_string();

    // Whatever the turn didn't already carry settles outside it (~5s): the
    // verdict notification plus the set-emptying level-set.
    let mut notification = notifications
        .into_iter()
        .find(|f| f["task_id"].as_str() == Some(task_id.as_str()))
        .unwrap_or(Value::Null);
    while notification.is_null() || !emptied {
        let frame = chat
            .recv(Duration::from_secs(60))
            .await
            .expect("recv")
            .expect("claude exited before the background settle");
        if frame["type"] == "system"
            && frame["subtype"] == "task_notification"
            && frame["task_id"].as_str() == Some(task_id.as_str())
        {
            notification = frame;
        } else if frame["type"] == "system" && frame["subtype"] == "background_tasks_changed" {
            emptied |= frame["tasks"].as_array().is_some_and(|t| t.is_empty());
        }
    }
    assert_eq!(notification["status"], json!("completed"));
    assert!(
        notification["summary"]
            .as_str()
            .is_some_and(|s| s.contains("completed")),
        "the close carries the human summary"
    );
    assert!(
        notification["output_file"].is_string(),
        "the close names the output file"
    );

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// The counter-case that the background tray's whole membership rule rests
/// on: a long-running **foreground** Bash also emits `task_started
/// {task_type:"local_bash", tool_use_id}` — identical in shape to the
/// backgrounded one above — but is NEVER announced in a
/// `background_tasks_changed` set. So `task_started` cannot classify a lane,
/// and only the set change may admit a task to the tray (PROTOCOL.md Pass
/// 23). While the driver adopted from `task_started`, every slow foreground
/// command showed up as "background task running".
///
/// The command is compute-bound rather than a `sleep` so it is unambiguously
/// foreground work, and long enough that the CLI bothers to track it (a fast
/// command emits no `task_started` at all).
#[tokio::test]
#[ignore = "live: spawns real claude, needs auth, bills one tiny turn + a ~20s foreground command"]
async fn claude_foreground_bash_never_enters_the_background_set() {
    let dir = tmpdir();
    let mut chat = spawn_claude(dir.path(), &[]);
    chat.initialize(HANDSHAKE).await.expect("initialize");
    chat.send_user_text(
        "Run this with the Bash tool in the FOREGROUND (do NOT set run_in_background), \
         it is compute-bound and takes ~20s: awk 'BEGIN{for(i=0;i<400000000;i++)s+=i; print s}' \
         Then reply with just OK.",
    )
    .await
    .expect("send");

    let mut started = Value::Null;
    let mut announced_any = false;
    loop {
        let frame = chat
            .recv(Duration::from_secs(120))
            .await
            .expect("recv")
            .expect("claude exited before result");
        if frame["type"] == "control_request" && frame["request"]["subtype"] == "can_use_tool" {
            let input = frame["request"]["input"].clone();
            chat.respond_permission(
                &frame["request_id"],
                PermissionDecision::Allow {
                    updated_input: input,
                },
            )
            .await
            .expect("respond allow");
        } else if frame["type"] == "system" && frame["subtype"] == "task_started" {
            started = frame;
        } else if frame["type"] == "system" && frame["subtype"] == "background_tasks_changed" {
            announced_any |= frame["tasks"].as_array().is_some_and(|t| !t.is_empty());
        } else if frame["type"] == "result" {
            break;
        }
    }
    // THE contract: the absence of an announcement is the only thing that
    // distinguishes a foreground command from backgrounded work.
    assert!(
        !announced_any,
        "a foreground command is never announced in a background_tasks_changed set"
    );
    // The companion fact (foreground rides the SAME task_started shape) is
    // what makes that absence load-bearing — but a CLI that stopped emitting
    // it would be moving in the SAFE direction, so report rather than fail.
    if started.is_null() {
        println!("foreground bash: no task_started emitted (wire moved; harmless)");
    } else {
        assert_eq!(
            started["task_type"],
            json!("local_bash"),
            "a foreground bash rides the SAME task_started shape as a backgrounded one"
        );
    }

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// Feedback-denial semantics: `{behavior:"deny", message, interrupt:false}`
/// must error the tool WITHOUT aborting the turn — the model reads the
/// reason from the tool error and the turn runs on to a SUCCESS result.
/// (The bare deny's `interrupt:true` aborts with an is_error result — the
/// TurnAborted path; that contrast is what this pins.)
#[tokio::test]
#[ignore = "live: spawns real claude, needs auth, bills one tiny turn"]
async fn claude_feedback_denial_keeps_turn_alive() {
    let dir = tmpdir();
    let mut chat = spawn_claude(dir.path(), &[]);
    chat.initialize(HANDSHAKE).await.expect("initialize");
    chat.send_user_text(
        "Create a file named probe.txt containing exactly hello, using the Bash tool.",
    )
    .await
    .expect("send");

    let mut denied_tool_result = false;
    let result = loop {
        let frame = chat
            .recv(TURN)
            .await
            .expect("recv")
            .expect("claude exited before result");
        if frame["type"] == "control_request" && frame["request"]["subtype"] == "can_use_tool" {
            // Deny every attempt with a reason, interrupt:false (the
            // driver's feedback-denial shape).
            chat.respond_permission(
                &frame["request_id"],
                PermissionDecision::Deny {
                    message:
                        "The user doesn't want to proceed with this tool use. \
                        The user's feedback: do NOT create any file; reply with exactly: understood"
                            .into(),
                    interrupt: false,
                },
            )
            .await
            .expect("respond deny with feedback");
        } else if frame["type"] == "user" {
            if let Some(blocks) = frame["message"]["content"].as_array() {
                for block in blocks {
                    if block["type"] == "tool_result" && block["is_error"] == json!(true) {
                        denied_tool_result = true;
                    }
                }
            }
        } else if frame["type"] == "result" {
            break frame;
        }
    };
    assert!(
        denied_tool_result,
        "denial surfaced as an error tool_result"
    );
    assert_eq!(
        result["is_error"],
        json!(false),
        "interrupt:false denial must NOT abort the turn: {result}"
    );
    assert_eq!(result["subtype"], "success");
    assert!(
        !dir.path().join("probe.txt").exists(),
        "denied tool must not run"
    );
    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// Plan approval: in plan mode the CLI proposes its plan via an ExitPlanMode
/// `can_use_tool` whose input carries the plan markdown; an allow whose
/// updatedInput adds userFeedback/userComments (the extension's comment
/// fields) is accepted and the turn completes.
#[tokio::test]
#[ignore = "live: spawns real claude, needs auth, bills one planning turn"]
async fn claude_exit_plan_mode_approval_roundtrip() {
    let dir = tmpdir();
    let mut chat = spawn_claude(dir.path(), &[]);
    chat.initialize(HANDSHAKE).await.expect("initialize");

    let ctl = chat
        .send_control(json!({ "subtype": "set_permission_mode", "mode": "plan" }))
        .await
        .expect("set plan mode");
    await_control_response(&mut chat, &ctl).await;

    chat.send_user_text(
        "Plan how you would create a file named plan-probe.txt containing hello. \
         Keep the plan to two short steps, then present it.",
    )
    .await
    .expect("send");

    let mut plan_seen = false;
    let result = loop {
        let frame = chat
            .recv(TURN)
            .await
            .expect("recv")
            .expect("claude exited before result");
        if frame["type"] == "control_request" && frame["request"]["subtype"] == "can_use_tool" {
            let request = &frame["request"];
            if request["tool_name"] == "ExitPlanMode" {
                plan_seen = true;
                let plan = request["input"]["plan"]
                    .as_str()
                    .expect("ExitPlanMode input carries the plan markdown");
                assert!(!plan.is_empty());
                // Approve with comments riding updatedInput (the driver's
                // plan-approval shape).
                let mut updated = request["input"].clone();
                updated["userFeedback"] = json!("looks good");
                updated["userComments"] = json!("looks good");
                chat.respond_permission(
                    &frame["request_id"],
                    PermissionDecision::Allow {
                        updated_input: updated,
                    },
                )
                .await
                .expect("approve plan");
            } else {
                // Anything else in plan mode (read-only probes): allow as-is.
                let input = request["input"].clone();
                chat.respond_permission(
                    &frame["request_id"],
                    PermissionDecision::Allow {
                        updated_input: input,
                    },
                )
                .await
                .expect("allow");
            }
        } else if frame["type"] == "result" {
            break frame;
        }
    };
    assert!(plan_seen, "an ExitPlanMode can_use_tool was routed to us");
    assert_eq!(
        result["subtype"], "success",
        "plan approval with comment fields completes the turn: {result}"
    );
    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// Chimaera's Tier A state machine is driven by injected `--settings` hooks;
/// the chat mode keeps them (same settings file rides the chat argv). This
/// asserts hooks still fire while stream-json owns stdio.
#[tokio::test]
#[ignore = "live: spawns real claude, needs auth, bills one tiny turn"]
async fn claude_hooks_fire_alongside_stream_json() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind hook listener");
    let port = listener.local_addr().expect("local addr").port();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits_task = std::sync::Arc::clone(&hits);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            hits_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = [0u8; 65536];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    });

    // Mirrors agents::write_settings: http hooks POSTing to the daemon.
    let dir = tmpdir();
    let hook = json!({ "type": "http", "url": format!("http://127.0.0.1:{port}/agent-events/test?key=k"), "timeout": 10 });
    let settings = json!({
        "hooks": {
            "SessionStart": [{ "hooks": [hook] }],
            "UserPromptSubmit": [{ "hooks": [hook] }],
            "Stop": [{ "hooks": [hook] }],
        }
    });
    let settings_path = dir.path().join("settings.json");
    std::fs::write(&settings_path, settings.to_string()).expect("write settings");

    let mut chat = spawn_claude(
        dir.path(),
        &[
            "--settings".to_string(),
            settings_path.display().to_string(),
        ],
    );
    chat.initialize(HANDSHAKE).await.expect("initialize");
    chat.send_user_text("Reply with exactly: ok")
        .await
        .expect("send");
    let (_, result, _) = run_turn_to_result(&mut chat).await;
    assert_eq!(result["subtype"], "success");

    // Stop-hook delivery can trail the result frame briefly.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while hits.load(std::sync::atomic::Ordering::SeqCst) == 0
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        hits.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "at least one hook POST arrived while stream-json owned stdio"
    );

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// Checkpoints end-to-end against the real CLI: a client-minted user-frame
/// uuid anchors `rewind_files`; the dry-run reports the touched file and the
/// apply actually reverts it on disk. Also probes `generate_session_title`
/// and `mcp_status` on the same session (one spawn, one billed turn).
#[tokio::test]
#[ignore = "live: spawns real claude, needs auth, bills one tiny turn"]
async fn claude_rewind_title_and_mcp_controls() {
    let dir = tmpdir();
    let mut chat = spawn_claude(dir.path(), &[]);
    chat.initialize(HANDSHAKE).await.expect("initialize");

    // The Write tool, deliberately: checkpoints track the FILE tools
    // (Write/Edit), not Bash side effects (live: a bash-created file reports
    // filesChanged:[] and survives the rewind).
    let uuid = chat
        .send_user_text_with_uuid(
            "Create a file named probe.txt containing exactly hello, using the Write tool.",
        )
        .await
        .expect("send");

    loop {
        let frame = chat
            .recv(TURN)
            .await
            .expect("recv")
            .expect("claude exited before result");
        if frame["type"] == "control_request" && frame["request"]["subtype"] == "can_use_tool" {
            let input = frame["request"]["input"].clone();
            chat.respond_permission(
                &frame["request_id"],
                PermissionDecision::Allow {
                    updated_input: input,
                },
            )
            .await
            .expect("respond allow");
        } else if frame["type"] == "result" {
            break;
        }
    }
    assert!(dir.path().join("probe.txt").exists(), "tool ran");

    // generate_session_title — the naming-chain source.
    let ctl = chat
        .send_control(json!({
            "subtype": "generate_session_title",
            "description": "Create a probe file",
            "persist": false,
        }))
        .await
        .expect("send title request");
    let title = await_control_response(&mut chat, &ctl).await;
    assert!(
        title["title"].as_str().is_some_and(|t| !t.is_empty()),
        "generate_session_title returned a title: {title}"
    );

    // rewind_files dry-run: the checkpoint keyed by OUR minted uuid must be
    // rewindable and name the file the turn created.
    let ctl = chat
        .send_control(json!({
            "subtype": "rewind_files",
            "user_message_id": uuid,
            "dry_run": true,
        }))
        .await
        .expect("send rewind dry run");
    let report = await_control_response(&mut chat, &ctl).await;
    assert_eq!(
        report["canRewind"],
        json!(true),
        "dry-run reports rewindable: {report}"
    );
    assert!(
        !report["filesChanged"]
            .as_array()
            .unwrap_or(&vec![])
            .is_empty(),
        "dry-run names the Write-tool file: {report}"
    );

    // Apply: the file the turn created must be GONE afterwards.
    let ctl = chat
        .send_control(json!({
            "subtype": "rewind_files",
            "user_message_id": uuid,
            "dry_run": false,
        }))
        .await
        .expect("send rewind apply");
    let applied = await_control_response(&mut chat, &ctl).await;
    assert_eq!(applied["canRewind"], json!(true), "apply: {applied}");
    assert!(
        !dir.path().join("probe.txt").exists(),
        "rewind reverted the Write-tool file"
    );

    // mcp_status answers with the server inventory (empty here, but shaped).
    let ctl = chat
        .send_control(json!({ "subtype": "mcp_status" }))
        .await
        .expect("send mcp_status");
    let mcp = await_control_response(&mut chat, &ctl).await;
    assert!(
        mcp["mcpServers"].is_array(),
        "mcp_status answers .mcpServers: {mcp}"
    );

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// Mid-turn stdin writes are queued by the CLI itself (the official client's
/// only queue): two immediate sends must yield two result frames, in order.
#[tokio::test]
#[ignore = "live: spawns real claude, needs auth, bills two tiny turns"]
async fn claude_mid_turn_send_queues_natively() {
    let dir = tmpdir();
    let mut chat = spawn_claude(dir.path(), &[]);
    chat.initialize(HANDSHAKE).await.expect("initialize");

    chat.send_user_text("Reply with exactly: first")
        .await
        .expect("send 1");
    // No waiting: this lands while turn 1 is still running.
    chat.send_user_text("Reply with exactly: second")
        .await
        .expect("send 2");

    let mut results = 0;
    let mut saw_second_reply = false;
    while results < 2 {
        let frame = chat
            .recv(TURN)
            .await
            .expect("recv")
            .expect("claude exited early");
        match frame["type"].as_str() {
            Some("result") => {
                assert_eq!(frame["subtype"], "success", "turn {results} ok");
                results += 1;
            }
            Some("assistant") if results == 1 => {
                let text = frame["message"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or_default();
                if text.contains("second") {
                    saw_second_reply = true;
                }
            }
            _ => {}
        }
    }
    assert!(
        saw_second_reply,
        "the queued message ran as its own turn after the first result"
    );

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// LIVE PROBE — does `cancel_async_message` actually un-queue a mid-turn send?
/// The subtype is defined in the SDK but never called by the official
/// extension. NOTE: since the hold-until-flush rework the driver no longer
/// sends this control request at all — it HOLDS queued messages and cancels a
/// still-held one locally (nothing ever reached the CLI to un-queue). This
/// probe now only documents the raw CLI's behavior for the record; the driver's
/// CancelQueued no longer depends on it. Queue a distinctively-answered message
/// mid-turn, send the cancel for its uuid, drain to idle, and REPORT whether it
/// ran — asserting only the session-health invariant.
#[tokio::test]
#[ignore = "live: spawns real claude, needs auth, bills tiny turns — reports cancel_async_message behavior"]
async fn claude_cancel_async_message_behavior() {
    let dir = tmpdir();
    let mut chat = spawn_claude(dir.path(), &[]);
    chat.initialize(HANDSHAKE).await.expect("initialize");

    // Turn 1 runs; a second message queues behind it with a known uuid and a
    // distinctive reply we can spot if it ever runs.
    chat.send_user_text("Reply with exactly: first")
        .await
        .expect("send 1");
    let cancel_uuid = chat
        .send_user_text_with_uuid("Reply with exactly: CANCELME")
        .await
        .expect("send 2");

    // Immediately ask the CLI to un-queue that message.
    chat.send_control(json!({
        "subtype": "cancel_async_message",
        "message_uuid": cancel_uuid,
    }))
    .await
    .expect("cancel_async_message");

    // Drain: count result frames and watch for the CANCELME reply. If the CLI
    // honored the cancel, only turn 1 runs (one result, then idle, no CANCELME).
    // If it ignored it, the queued message runs too (a second result + CANCELME).
    let mut results = 0;
    let mut saw_cancelme = false;
    loop {
        match chat.recv(TURN).await {
            Ok(Some(frame)) => match frame["type"].as_str() {
                Some("result") => {
                    results += 1;
                    if results >= 2 {
                        break;
                    }
                }
                Some("assistant") => {
                    let text = frame["message"]["content"][0]["text"]
                        .as_str()
                        .unwrap_or_default();
                    if text.contains("CANCELME") {
                        saw_cancelme = true;
                    }
                }
                _ => {}
            },
            Ok(None) => break, // CLI exited
            Err(_) => break,   // idle: no further frame within TURN
        }
    }

    eprintln!(
        "LIVE cancel_async_message: results={results}, queued_message_ran={saw_cancelme} => \
         cancel_async_message {}",
        if saw_cancelme {
            "does NOT un-queue (the message still ran)"
        } else {
            "UN-QUEUES (the message did not run)"
        }
    );

    // Either outcome is acceptable; the invariant is that the cancel didn't
    // wedge the session — turn 1 completed and the stream stayed healthy.
    assert!(
        results >= 1,
        "at least turn 1 completed (results={results})"
    );

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// Three rapid sends against the REAL claude driver — the user's exact
/// scenario. The hold-until-flush model must (a) settle idle: every turn the
/// CLI opens also ends, and (b) DELIVER every held message: each `queued:true`
/// echo resolves `sent`, none stranded "queued" or wrongly "dropped". The two
/// trailing sends are HELD (never dumped mid-turn) and flushed at the running
/// turn's end, so the CLI can't coalesce them into a bare result and lose one.
/// This drives the full ChatManager pipeline (the same normalized events the
/// UI folds).
#[tokio::test]
#[ignore = "live: spawns real claude, needs auth, bills a few tiny turns"]
async fn driver_rapid_queued_sends_settle_idle() {
    use std::collections::HashMap;
    use std::sync::Arc;

    let dir = tmpdir();
    let on_event: EventHook = Box::new(|_, _| {});
    let on_exit: ExitHook = Box::new(|_, _| {});
    let manager = Arc::new(ChatManager::new(dir.path().join("chat"), on_event, on_exit));

    // The driver's own argv (server-side extras aside): the chat stream-json
    // flags plus the cheap model. env_extra (auto-updater off, checkpointing)
    // is supplied by ClaudeAdapter.
    let mut argv = vec!["claude".to_string()];
    argv.extend(chat_args(Some(CLAUDE_TEST_MODEL), None));
    let spec = SpawnSpec::new("rapid", argv, dir.path().to_path_buf());
    manager
        .spawn(&ClaudeAdapter, spec)
        .expect("spawn claude driver");
    let mut rx = manager.attach("rapid", 0).expect("attach").live;

    // Three sends with no waiting between them: the first opens a turn, the next
    // two are held behind it and flush when it ends.
    for text in [
        "Reply with exactly: one",
        "Reply with exactly: two",
        "Reply with exactly: three",
    ] {
        manager
            .command(
                "rapid",
                AgentCommand::Send {
                    blocks: vec![ContentBlock::Text { text: text.into() }],
                },
            )
            .await
            .expect("send");
    }

    // Drain until the session goes quiet (no event for a settle window); count
    // the turn boundaries and track each queued send's final delivery state.
    let mut opened = 0usize;
    let mut ended = 0usize;
    let mut queued_ids: Vec<String> = Vec::new();
    let mut final_state: HashMap<String, UserMessageState> = HashMap::new();
    let settle = Duration::from_secs(20);
    // Drain until a quiet window elapses with no further event (Err from the
    // timeout) or the broadcast closes — either way the session is idle.
    while let Ok(Ok(entry)) = tokio::time::timeout(settle, rx.recv()).await {
        match &entry.ev {
            AgentEvent::TurnStarted { .. } => opened += 1,
            AgentEvent::TurnCompleted { .. } | AgentEvent::TurnAborted { .. } => ended += 1,
            AgentEvent::UserMessage {
                id: Some(id),
                queued: true,
                ..
            } => queued_ids.push(id.clone()),
            AgentEvent::UserMessageUpdate { id, state } => {
                final_state.insert(id.clone(), *state);
            }
            _ => {}
        }
    }

    assert!(opened >= 1, "at least one turn opened (opened={opened})");
    assert_eq!(
        opened, ended,
        "every opened turn must end — no turn left stuck running \
         (opened={opened}, ended={ended})"
    );
    // The crux of the user's bug: every message queued behind the running turn
    // is DELIVERED — resolved `sent`, never left "queued" and never stranded
    // "not delivered" (dropped).
    assert!(
        !queued_ids.is_empty(),
        "the trailing sends echoed queued (queued_ids={queued_ids:?})"
    );
    for id in &queued_ids {
        assert_eq!(
            final_state.get(id),
            Some(&UserMessageState::Sent),
            "held message {id} must resolve sent, not strand \
             (state={:?})",
            final_state.get(id)
        );
    }

    manager.kill("rapid");
}

/// Wait for a specific control_response and return its inner response value.
async fn await_control_response(chat: &mut ClaudeChat, ctl_id: &str) -> Value {
    loop {
        let frame = chat
            .recv(Duration::from_secs(30))
            .await
            .expect("recv")
            .expect("claude exited awaiting control response");
        if frame["type"] == "control_response" && frame["response"]["request_id"] == json!(ctl_id) {
            assert_eq!(
                frame["response"]["subtype"], "success",
                "control request failed: {frame}"
            );
            return frame["response"]["response"].clone();
        }
    }
}

#[tokio::test]
#[ignore = "live: spawns real codex, needs auth"]
async fn codex_handshake_and_thread_start() {
    let dir = tmpdir();
    let mut chat = CodexChat::spawn("codex", dir.path()).expect("spawn codex");

    let init = chat.initialize(HANDSHAKE).await.expect("initialize");
    assert!(
        init["userAgent"].is_string(),
        "initialize returns userAgent"
    );
    assert!(
        init["codexHome"].is_string(),
        "initialize returns codexHome"
    );

    let thread_id = chat
        .thread_start(dir.path(), HANDSHAKE)
        .await
        .expect("thread/start");
    assert!(!thread_id.is_empty());

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

#[tokio::test]
#[ignore = "live: spawns real codex, needs auth, bills one tiny turn"]
async fn codex_echo_turn_deltas_usage_and_completion() {
    let dir = tmpdir();
    let mut chat = CodexChat::spawn("codex", dir.path()).expect("spawn codex");
    chat.initialize(HANDSHAKE).await.expect("initialize");
    let thread_id = chat
        .thread_start(dir.path(), HANDSHAKE)
        .await
        .expect("thread/start");
    chat.turn_start(&thread_id, "Reply with exactly: ok")
        .await
        .expect("turn/start");

    let mut saw_delta = false;
    let mut saw_usage = false;
    loop {
        let frame = chat
            .recv(TURN)
            .await
            .expect("recv")
            .expect("codex exited before turn completed");
        match frame["method"].as_str() {
            Some("item/agentMessage/delta") => saw_delta = true,
            Some("thread/tokenUsage/updated") => {
                saw_usage = frame["params"]["tokenUsage"]["total"]["totalTokens"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0;
            }
            Some("turn/completed") => {
                assert_eq!(frame["params"]["turn"]["status"], "completed");
                break;
            }
            _ => {}
        }
    }
    assert!(saw_delta, "agentMessage deltas streamed");
    assert!(saw_usage, "token usage notification arrived");

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// The multi-agent (collab) wire facts the driver's thread-scoping leans on
/// (PROTOCOL.md Pass 16): a delegation request produces `subAgentActivity`
/// markers on the parent thread, and the subagent's OWN thread streams its
/// transcript interleaved on this same connection under its own threadId —
/// so the parent's turn end must be filtered by threadId to be found at all.
#[tokio::test]
#[ignore = "live: spawns real codex, needs auth, bills a small multi-agent turn"]
async fn codex_collab_subagent_surface() {
    let dir = tmpdir();
    let mut chat = CodexChat::spawn("codex", dir.path()).expect("spawn codex");
    chat.initialize(HANDSHAKE).await.expect("initialize");
    let thread_id = chat
        .thread_start(dir.path(), HANDSHAKE)
        .await
        .expect("thread/start");
    chat.turn_start(
        &thread_id,
        "Use your subagent/collaboration tools for this: spawn ONE subagent \
         whose task is to compute 6*7 and reply with just the number. Wait \
         for it, then report its answer. Do not run shell commands, do not \
         change files — delegate to a subagent and relay its answer.",
    )
    .await
    .expect("turn/start");

    let mut agent_thread = String::new();
    let mut saw_foreign_frame = false;
    let mut saw_foreign_turn_end = false;
    loop {
        let frame = chat
            .recv(TURN)
            .await
            .expect("recv")
            .expect("codex exited before turn completed");
        let frame_thread = frame["params"]["threadId"].as_str().unwrap_or_default();
        let item = &frame["params"]["item"];
        match frame["method"].as_str() {
            Some("item/completed") if item["type"] == "subAgentActivity" => {
                assert_eq!(frame_thread, thread_id, "activity markers ride the parent");
                if item["kind"] == "started" {
                    agent_thread = item["agentThreadId"]
                        .as_str()
                        .expect("subAgentActivity carries agentThreadId")
                        .to_string();
                    assert_ne!(agent_thread, thread_id, "a subagent is its own thread");
                    assert!(item["agentPath"].is_string(), "agentPath names the agent");
                }
            }
            // The subagent's own transcript multiplexes onto this connection.
            Some("turn/completed") if frame_thread == agent_thread && !agent_thread.is_empty() => {
                saw_foreign_turn_end = true;
            }
            Some(_) if !agent_thread.is_empty() && frame_thread == agent_thread => {
                saw_foreign_frame = true;
            }
            // The parent's end arrives AFTER (and despite) the subagent's —
            // exactly what the driver's threadId gate exists for.
            Some("turn/completed") if frame_thread == thread_id => {
                assert_eq!(frame["params"]["turn"]["status"], "completed");
                break;
            }
            _ => {}
        }
    }
    assert!(
        !agent_thread.is_empty(),
        "the model delegated (subAgentActivity started)"
    );
    assert!(
        saw_foreign_frame,
        "the subagent thread's transcript streamed on this connection"
    );
    assert!(
        saw_foreign_turn_end,
        "the subagent's own turn/completed arrived (threadId-scoped)"
    );

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// Feature-detects the codex control surface the driver leans on: turn/steer
/// (exists; errors sanely without an active turn), thread/settings/update
/// (supported or -32601 → the driver's per-turn fallback), account/read
/// (rate-limit telemetry), model/list (the picker's source of truth).
#[tokio::test]
#[ignore = "live: spawns real codex, needs auth"]
async fn codex_steer_settings_and_account_surface() {
    let dir = tmpdir();
    let mut chat = CodexChat::spawn("codex", dir.path()).expect("spawn codex");
    chat.initialize(HANDSHAKE).await.expect("initialize");
    let thread_id = chat
        .thread_start(dir.path(), HANDSHAKE)
        .await
        .expect("thread/start");

    // steer with no active turn: must NOT be method-not-found — any other
    // error (no active turn / unexpected turn id) proves the method exists.
    let steer = chat
        .request_raw(
            "turn/steer",
            json!({
                "threadId": thread_id,
                "clientUserMessageId": "probe-1",
                "input": [{ "type": "text", "text": "probe" }],
                "expectedTurnId": "turn-does-not-exist",
            }),
            HANDSHAKE,
        )
        .await
        .expect("steer answered");
    let msg = steer["error"]["message"].as_str().unwrap_or_default();
    assert!(
        steer.get("error").is_some(),
        "steer without a turn should error: {steer}"
    );
    assert!(
        !msg.to_lowercase().contains("method not found"),
        "turn/steer must exist on this binary: {msg}"
    );

    // thread/settings/update: supported, or -32601 (per-turn fallback path).
    let settings = chat
        .request_raw(
            "thread/settings/update",
            json!({
                "threadId": thread_id,
                "permissions": ":workspace",
                "approvalPolicy": "on-request",
            }),
            HANDSHAKE,
        )
        .await
        .expect("settings/update answered");
    if let Some(err) = settings.get("error").filter(|e| !e.is_null()) {
        assert_eq!(
            err["code"].as_i64(),
            Some(-32601),
            "settings/update failed in an unexpected way: {settings}"
        );
    }

    // account/read: the rate-limit source (tolerated when absent).
    let account = chat
        .request_raw("account/read", json!({ "refreshToken": false }), HANDSHAKE)
        .await
        .expect("account/read answered");
    if account.get("error").is_none() {
        assert!(
            account["result"].is_object(),
            "account/read returns an object: {account}"
        );
    }

    // model/list feeds the picker with per-model efforts.
    let models = chat
        .request_raw(
            "model/list",
            json!({ "includeHidden": false, "cursor": null, "limit": 100 }),
            HANDSHAKE,
        )
        .await
        .expect("model/list answered");
    assert!(
        models["result"]["data"].is_array(),
        "model/list returns data: {models}"
    );

    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// The codex rewind/compact surface (PROTOCOL.md pass 8): thread/rollback
/// drops trailing turns in place (result = the updated thread), works right
/// after thread/resume (the rewind-respawn path), and thread/compact/start
/// acks then runs the compaction as its own turn with a contextCompaction
/// item (no thread/compacted notification on the pinned version).
#[tokio::test]
#[ignore = "live: spawns real codex twice, needs auth, bills two tiny turns"]
async fn codex_rollback_and_compact_surface() {
    let dir = tmpdir();
    let mut chat = CodexChat::spawn("codex", dir.path()).expect("spawn codex");
    chat.initialize(HANDSHAKE).await.expect("initialize");
    let thread_id = chat
        .thread_start(dir.path(), HANDSHAKE)
        .await
        .expect("thread/start");

    // Two turns of history to roll back / compact.
    for word in ["one", "two"] {
        chat.turn_start(&thread_id, &format!("Reply with exactly: {word}"))
            .await
            .expect("turn/start");
        loop {
            let frame = chat
                .recv(TURN)
                .await
                .expect("recv")
                .expect("codex exited mid-turn");
            if frame["method"] == "turn/completed" {
                break;
            }
        }
    }

    // Rollback drops the last turn in place; the result is the thread object.
    let rollback = chat
        .request_raw(
            "thread/rollback",
            json!({ "threadId": thread_id, "numTurns": 1 }),
            HANDSHAKE,
        )
        .await
        .expect("thread/rollback answered");
    assert!(
        rollback.get("error").is_none(),
        "rollback failed: {rollback}"
    );
    assert_eq!(
        rollback["result"]["thread"]["id"].as_str(),
        Some(thread_id.as_str()),
        "rollback returns the updated thread: {rollback}"
    );

    // Compact acks empty, then runs as its own turn with a contextCompaction
    // item — the driver's "context compacted" notice source.
    let compact = chat
        .request_raw(
            "thread/compact/start",
            json!({ "threadId": thread_id }),
            HANDSHAKE,
        )
        .await
        .expect("thread/compact/start answered");
    assert!(compact.get("error").is_none(), "compact failed: {compact}");
    let mut saw_compaction_item = false;
    loop {
        let frame = chat
            .recv(TURN)
            .await
            .expect("recv")
            .expect("codex exited mid-compaction");
        match frame["method"].as_str() {
            Some("item/completed") if frame["params"]["item"]["type"] == "contextCompaction" => {
                saw_compaction_item = true;
            }
            Some("turn/completed") => break,
            _ => {}
        }
    }
    assert!(
        saw_compaction_item,
        "compaction runs as a turn with a contextCompaction item"
    );
    chat.shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");

    // The rewind-respawn path: a fresh process resumes the thread and rolls
    // back immediately, exactly like the driver's handshake does.
    let mut resumed = CodexChat::spawn("codex", dir.path()).expect("respawn codex");
    resumed.initialize(HANDSHAKE).await.expect("initialize");
    let resume = resumed
        .request_raw(
            "thread/resume",
            json!({ "threadId": thread_id, "cwd": dir.path() }),
            HANDSHAKE,
        )
        .await
        .expect("thread/resume answered");
    assert_eq!(
        resume["result"]["thread"]["id"].as_str(),
        Some(thread_id.as_str()),
        "resume keeps the thread id: {resume}"
    );
    let rollback = resumed
        .request_raw(
            "thread/rollback",
            json!({ "threadId": thread_id, "numTurns": 1 }),
            HANDSHAKE,
        )
        .await
        .expect("post-resume rollback answered");
    assert!(
        rollback.get("error").is_none(),
        "rollback after resume failed: {rollback}"
    );
    resumed
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown");
}

/// The production path end-to-end: ChatManager + ClaudeAdapter driver against
/// the real CLI — pinned --session-id, handshake Init, mapped events, journal
/// replay. This is what a chat session actually runs as.
#[tokio::test]
#[ignore = "live: spawns real claude via the driver, needs auth, bills one tiny turn"]
async fn driver_stack_end_to_end_against_real_claude() {
    use chimaera_agent::claude::{chat_args, ClaudeAdapter};
    use chimaera_agent::driver::SpawnSpec;
    use chimaera_agent::model::{AgentCommand, AgentEvent, ContentBlock};
    use chimaera_agent::ChatManager;
    use std::sync::Arc;

    // Poor-man's v4 uuid from the entropy of two Instants; fine for a test.
    let pinned = {
        let a = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        let b = a.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        format!(
            "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
            (a >> 32) as u32,
            (a >> 16) as u16,
            (a as u16) & 0x0fff,
            ((b >> 48) as u16) & 0x0fff,
            b & 0xffff_ffff_ffff
        )
    };

    let dir = tmpdir();
    let manager = Arc::new(ChatManager::new(
        dir.path().join("chat"),
        Box::new(|_, _| {}),
        Box::new(|id, exit| tracing::info!(%id, ?exit, "driver exit")),
    ));

    let mut argv = vec!["claude".to_string()];
    argv.extend(chat_args(Some(CLAUDE_TEST_MODEL), None));
    argv.push("--session-id".into());
    argv.push(pinned.clone());
    let mut spec = SpawnSpec::new("s-live", argv, dir.path().to_path_buf());
    spec.pinned_native_id = Some(pinned.clone());

    manager.spawn(&ClaudeAdapter, spec).expect("spawn driver");
    let att = manager.attach("s-live", 0).expect("attach");
    let mut rx = att.live;

    manager
        .command(
            "s-live",
            AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "Reply with exactly: ok".into(),
                }],
            },
        )
        .await
        .expect("send");

    let mut saw_message = false;
    let mut saw_completed = false;
    let deadline = tokio::time::Instant::now() + TURN;
    while !(saw_message && saw_completed) {
        let entry = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("timed out waiting for driver events")
            .expect("broadcast closed");
        match &entry.ev {
            AgentEvent::MessageChunk { text, .. } if text.contains("ok") => saw_message = true,
            AgentEvent::TurnCompleted { usage, .. } => {
                assert!(usage.cost_usd.unwrap_or(0.0) > 0.0, "cost mapped");
                saw_completed = true;
            }
            AgentEvent::TurnAborted { reason, .. } => panic!("turn aborted: {reason}"),
            _ => {}
        }
    }

    let info = manager.get("s-live").expect("info");
    assert_eq!(
        info.native_session_id.as_deref(),
        Some(pinned.as_str()),
        "system/init echoed the pinned --session-id"
    );
    assert_eq!(
        manager.index().lookup(&pinned).as_deref(),
        Some("s-live"),
        "resume index recorded"
    );

    // Replay serves the whole conversation for a fresh window.
    let replay = manager.attach("s-live", 0).expect("reattach").replay;
    assert!(replay
        .iter()
        .any(|e| matches!(&e.ev, AgentEvent::UserMessage { text, .. } if text.contains("ok"))));
    assert!(replay
        .iter()
        .any(|e| matches!(e.ev, AgentEvent::TurnCompleted { .. })));

    manager.kill("s-live");
}
