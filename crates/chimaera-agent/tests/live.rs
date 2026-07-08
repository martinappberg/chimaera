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

use chimaera_agent::claude::{ClaudeChat, PermissionDecision};
use chimaera_agent::codex::CodexChat;

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
