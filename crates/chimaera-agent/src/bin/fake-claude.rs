//! A scripted stand-in for the claude CLI's stream-json surface, so driver
//! and registry tests run hermetically in CI (no network, no auth, no
//! billing). It speaks just enough of the live-verified protocol: the
//! initialize handshake, one canned turn with streaming deltas + a Bash
//! tool_use, a can_use_tool permission round-trip, and result frames.
//!
//! Modes (argv[1]): `normal` (default), `silent` (never answers — handshake
//! watchdog tests), `die` (exit 3 immediately — spawn-crash tests).

use std::io::{BufRead, Write};

use serde_json::{json, Value};

fn emit(value: Value) {
    let mut stdout = std::io::stdout().lock();
    let mut line = serde_json::to_vec(&value).expect("serialize");
    line.push(b'\n');
    stdout.write_all(&line).expect("stdout write");
    stdout.flush().expect("stdout flush");
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "normal".into());
    match mode.as_str() {
        "die" => std::process::exit(3),
        "silent" => {
            // Swallow stdin forever; say nothing (handshake must time out).
            let stdin = std::io::stdin().lock();
            for _ in stdin.lines() {}
            return;
        }
        _ => {}
    }

    let stdin = std::io::stdin().lock();
    for line in stdin.lines() {
        let Ok(line) = line else { break };
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        if frame["type"] == "control_request" && frame["request"]["subtype"] == "initialize" {
            emit(json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": frame["request_id"],
                    "response": { "commands": [
                        { "name": "compact", "description": "Compact history" },
                    ]},
                },
            }));
        } else if frame["type"] == "user" {
            run_canned_turn();
        } else if frame["type"] == "control_response" {
            // Only the scripted permission round-trip (req-1) drives the turn;
            // ignore any other control_response so a future non-permission
            // answer (get_settings, title, …) can't corrupt this state machine.
            if frame["response"]["request_id"] == "req-1" {
                let response = &frame["response"]["response"];
                if response["behavior"] == "allow" {
                    finish_turn(true);
                } else if response["interrupt"] == json!(false) {
                    // Feedback-denial (live-verified): the tool errors but the
                    // turn keeps running and ends with a SUCCESS result.
                    finish_feedback_denial();
                } else {
                    finish_turn(false);
                }
            }
        } else if frame["type"] == "control_request" {
            // interrupt / set_permission_mode / set_model: acknowledge.
            emit(json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": frame["request_id"],
                    "response": {},
                },
            }));
        }
    }
}

fn run_canned_turn() {
    emit(json!({
        "type": "system", "subtype": "init",
        "session_id": "fake-native-1", "model": "fake-model",
        "permissionMode": "default", "slash_commands": ["compact"],
    }));
    emit(json!({
        "type": "stream_event",
        "event": { "type": "message_start", "message": { "id": "m1" } },
    }));
    for delta in ["hel", "lo"] {
        emit(json!({
            "type": "stream_event",
            "event": { "type": "content_block_delta",
                       "delta": { "type": "text_delta", "text": delta } },
        }));
    }
    emit(json!({
        "type": "assistant",
        "message": { "id": "m1", "content": [
            { "type": "text", "text": "hello" },
            { "type": "tool_use", "id": "tu-1", "name": "Bash",
              "input": { "command": "touch probe" } },
        ]},
    }));
    emit(json!({
        "type": "control_request",
        "request_id": "req-1",
        "request": {
            "subtype": "can_use_tool",
            "tool_name": "Bash",
            "display_name": "Bash",
            "input": { "command": "touch probe" },
            "tool_use_id": "tu-1",
            "permission_suggestions": [
                { "type": "addRules",
                  "rules": [{ "toolName": "Bash", "ruleContent": "touch *" }],
                  "behavior": "allow", "destination": "localSettings" },
            ],
        },
    }));
}

/// An interrupt:false denial errors the tool, then the model reacts to the
/// feedback and the turn completes normally (NOT the TurnAborted path).
fn finish_feedback_denial() {
    emit(json!({
        "type": "user",
        "message": { "content": [
            { "type": "tool_result", "tool_use_id": "tu-1",
              "content": "User rejected this action", "is_error": true },
        ]},
    }));
    emit(json!({
        "type": "assistant",
        "message": { "id": "m2", "content": [
            { "type": "text", "text": "understood" },
        ]},
    }));
    emit(json!({
        "type": "result", "subtype": "success", "is_error": false,
        "result": "understood", "session_id": "fake-native-1",
        "total_cost_usd": 0.01, "duration_ms": 42,
        "usage": { "input_tokens": 10, "output_tokens": 5 },
    }));
}

fn finish_turn(allowed: bool) {
    if allowed {
        emit(json!({
            "type": "user",
            "message": { "content": [
                { "type": "tool_result", "tool_use_id": "tu-1",
                  "content": "done", "is_error": false },
            ]},
        }));
        emit(json!({
            "type": "result", "subtype": "success", "is_error": false,
            "result": "done", "session_id": "fake-native-1",
            "total_cost_usd": 0.01, "duration_ms": 42,
            "usage": { "input_tokens": 10, "output_tokens": 5 },
        }));
    } else {
        // The driver's deny sends `interrupt:true`, which ABORTS the turn on
        // the real CLI: the tool errors, then the turn ends is_error (the
        // TurnAborted path) — NOT a success result.
        emit(json!({
            "type": "user",
            "message": { "content": [
                { "type": "tool_result", "tool_use_id": "tu-1",
                  "content": "User rejected this action", "is_error": true },
            ]},
        }));
        emit(json!({
            "type": "result", "subtype": "error", "is_error": true,
            "result": "turn aborted by user", "session_id": "fake-native-1",
            "duration_ms": 42,
            "usage": { "input_tokens": 10, "output_tokens": 5 },
        }));
    }
}
