//! A scripted stand-in for the claude CLI's stream-json surface, so driver
//! and registry tests run hermetically in CI (no network, no auth, no
//! billing). It speaks just enough of the live-verified protocol: the
//! initialize handshake, one canned turn with streaming deltas + a Bash
//! tool_use, a can_use_tool permission round-trip, and result frames (each
//! successful turn is followed by a `post_turn_summary` status line — the
//! live 2.1.207 order).
//! Back-to-back user frames queue natively (one content-bearing follow-up turn
//! each, after the running turn's) and an interrupt ends the turn with an
//! is_error result that drops the queue — mirroring the real CLI. The driver
//! HOLDS queued sends and flushes them only at a turn boundary, so it never
//! writes a frame mid-turn; the queue here only fills when the driver flushes
//! two-or-more held sends back-to-back and the CLI queues the later ones.
//!
//! Modes (argv[1]): `normal` (default), `background` (the turn backgrounds a
//! Bash task and then ENDS, leaving an idle turn with live background work —
//! the state the dashboard/rail "still working off-screen" cues render),
//! `question` (the turn asks an
//! AskUserQuestion instead of running a tool — ask-lifecycle tests), `plan`
//! (publishes a TodoWrite level-set and parks on ExitPlanMode approval),
//! `subagent` (parks a turn with one live Task row and progress), `hang`
//! (opens a turn, streams content, never ends it, and acks an interrupt with NO
//! result — the interrupt-watchdog recovery tests), `silent` (never answers —
//! handshake watchdog tests), `die` (exit 3 immediately — spawn-crash tests),
//! `die-after-handshake` (answer initialize, print a diagnostic on stderr, exit
//! 2 — the post-update failure-at-birth tests).

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

    // The real CLI's queueing state: mid-turn user frames wait their turn
    // (one result each after the running turn's), and an aborted turn drops
    // the queue with it.
    let mut turn_active = false;
    let mut queued = 0u32;
    // Background mode gives each turn its own task id, so a second prompt
    // adds to the live set rather than re-listing the same one.
    let mut turn_count = 0u32;

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
            // Failure-at-birth: the handshake succeeds, then the "updated"
            // binary crashes before serving anything — stderr is the only
            // diagnostic (the driver must preserve it).
            if mode == "die-after-handshake" {
                eprintln!("fake-claude: post-update crash (kaboom)");
                std::process::exit(2);
            }
        } else if frame["type"] == "user" {
            if turn_active {
                // Mid-turn frames queue natively in the real CLI; count them
                // so the scripted turn-end runs each as its own follow-up turn.
                queued += 1;
            } else {
                turn_active = true;
                match mode.as_str() {
                    "question" => run_question_turn(),
                    "plan" => run_plan_turn(),
                    "subagent" => run_subagent_turn(),
                    // Streams content, then never ends — the driver's interrupt
                    // watchdog is the only thing that can recover it.
                    "hang" => run_hang_turn(),
                    // Ends its own turn (no permission round-trip), so the
                    // session settles idle with the task still running.
                    "background" => {
                        run_background_turn(turn_count);
                        turn_active = false;
                        turn_count += 1;
                    }
                    _ => run_canned_turn(),
                }
            }
        } else if frame["type"] == "control_response" {
            // Only the scripted round-trips (req-1 permission, req-q1
            // question) drive the turn; ignore any other control_response so
            // a future non-permission answer (get_settings, title, …) can't
            // corrupt this state machine.
            if frame["response"]["request_id"] == "req-1" {
                let response = &frame["response"]["response"];
                let allowed = response["behavior"] == "allow";
                let feedback_denial = !allowed && response["interrupt"] == json!(false);
                if allowed {
                    finish_turn(true);
                } else if feedback_denial {
                    // Feedback-denial (live-verified): the tool errors but the
                    // turn keeps running and ends with a SUCCESS result.
                    finish_feedback_denial();
                } else {
                    finish_turn(false);
                }
                turn_active = false;
                if allowed || feedback_denial {
                    // A successful turn end (allow, or a feedback-denial that
                    // keeps the turn running) dequeues each pending message —
                    // the sends the driver flushed back-to-back and this CLI
                    // queued behind the running turn — as its own content-
                    // bearing follow-up turn.
                    while queued > 0 {
                        queued -= 1;
                        emit_followup_turn();
                    }
                } else {
                    // The deny's interrupt:true aborted the turn — the CLI's
                    // native queue dies with it.
                    queued = 0;
                }
            } else if frame["response"]["request_id"] == "req-q1" {
                // The AskUserQuestion answer (allow + updatedInput.answers)
                // ends the turn with a plain success, like the real CLI. Reset
                // turn state so a subsequent user frame starts a fresh turn.
                turn_active = false;
                emit(json!({
                    "type": "result", "subtype": "success", "is_error": false,
                    "result": "noted", "session_id": "fake-native-1",
                    "total_cost_usd": 0.001, "duration_ms": 7,
                    "usage": { "input_tokens": 3, "output_tokens": 2 },
                }));
            } else if frame["response"]["request_id"] == "req-plan" {
                turn_active = false;
                emit(json!({
                    "type": "result", "subtype": "success", "is_error": false,
                    "result": "plan approved", "session_id": "fake-native-1",
                    "total_cost_usd": 0.001, "duration_ms": 9,
                    "usage": { "input_tokens": 5, "output_tokens": 3 },
                }));
            }
        } else if frame["type"] == "control_request" && frame["request"]["subtype"] == "interrupt" {
            emit(json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": frame["request_id"],
                    "response": {},
                },
            }));
            if turn_active && mode == "hang" {
                // A wedged turn: ack the interrupt but emit NO result, exactly
                // the state the interrupt watchdog exists to recover. turn_active
                // stays true on the fake side — the fake never speaks again.
            } else if turn_active {
                // The real CLI ends the interrupted turn with an is_error
                // result whose `result` string is free text — omitted here so
                // tests exercise the driver's structural classification, not
                // a string heuristic. The queue drops with the turn.
                turn_active = false;
                queued = 0;
                emit(json!({
                    "type": "result", "subtype": "error_during_execution",
                    "is_error": true, "session_id": "fake-native-1",
                    "duration_ms": 7,
                    "usage": { "input_tokens": 10, "output_tokens": 2 },
                }));
            }
        } else if frame["type"] == "control_request" {
            // set_permission_mode / set_model / …: acknowledge.
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

/// A dequeued (previously mid-turn) message runs as its own follow-up turn.
/// It MUST stream content before the result: the driver opens the turn
/// boundary LAZILY (ensure_turn, on the first real frame), so a bare result
/// would resolve the message `sent` without ever opening a turn — exactly the
/// live CLI's shape, where every real turn produces content.
fn emit_followup_turn() {
    emit(json!({
        "type": "stream_event",
        "event": { "type": "message_start", "message": { "id": "mq" } },
    }));
    emit(json!({
        "type": "stream_event",
        "event": { "type": "content_block_delta",
                   "delta": { "type": "text_delta", "text": "ok" } },
    }));
    emit(json!({
        "type": "result", "subtype": "success", "is_error": false,
        "result": "done", "session_id": "fake-native-1",
        "total_cost_usd": 0.005, "duration_ms": 21,
        "usage": { "input_tokens": 4, "output_tokens": 2 },
    }));
}

/// Open a turn and stream content, then say nothing more — no result ever.
/// Paired with the hang-mode interrupt branch (ack, no result), this is the
/// wedged-turn state the driver's interrupt watchdog must recover from.
fn run_hang_turn() {
    emit(json!({
        "type": "system", "subtype": "init",
        "session_id": "fake-native-1", "model": "fake-model",
        "permissionMode": "default", "slash_commands": ["compact"],
    }));
    emit(json!({
        "type": "stream_event",
        "event": { "type": "message_start", "message": { "id": "mh" } },
    }));
    emit(json!({
        "type": "stream_event",
        "event": { "type": "content_block_delta",
                   "delta": { "type": "text_delta", "text": "working" } },
    }));
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

/// A turn that backgrounds a Bash task and then ends. Background work is
/// cross-turn — once the result lands the session is idle with the task still
/// in the live set. That is the exact state the "still working off-screen"
/// cues render, and nothing clears it until the process dies.
///
/// The frames mirror the real spawn order (PROTOCOL.md Pass 23):
/// `background_tasks_changed` carrying the task arrives FIRST and is what
/// admits it to the set, then `task_started` enriches it. `task_started`
/// alone can't mean background — the real CLI emits an identical one for a
/// long-running FOREGROUND Bash. Since the set change is a REPLACE, each turn
/// re-announces every task spawned so far, which is what lets them accumulate.
fn run_background_turn(n: u32) {
    emit(json!({
        "type": "system", "subtype": "init",
        "session_id": "fake-native-1", "model": "fake-model",
        "permissionMode": "default", "slash_commands": ["compact"],
    }));
    emit(json!({
        "type": "stream_event",
        "event": { "type": "message_start", "message": { "id": "mb" } },
    }));
    emit(json!({
        "type": "stream_event",
        "event": { "type": "content_block_delta",
                   "delta": { "type": "text_delta", "text": "running that in the background" } },
    }));
    // The authoritative level-set: every task spawned so far (this frame is a
    // REPLACE, so dropping the earlier ones would retire them).
    let tasks: Vec<Value> = (0..=n)
        .map(|i| {
            json!({
                "task_id": format!("bg-{i}"),
                "task_type": "local_bash",
                "description": format!("npm run build ({i})"),
            })
        })
        .collect();
    emit(json!({
        "type": "system", "subtype": "background_tasks_changed", "tasks": tasks,
    }));
    emit(json!({
        "type": "system", "subtype": "task_started",
        "task_id": format!("bg-{n}"),
        "task_type": "local_bash",
        "description": format!("npm run build ({n})"),
    }));
    emit(json!({
        "type": "result", "subtype": "success", "is_error": false,
        "result": "backgrounded", "session_id": "fake-native-1",
        "total_cost_usd": 0.002, "duration_ms": 12,
        "usage": { "input_tokens": 5, "output_tokens": 4 },
    }));
}

/// A turn that parks on an AskUserQuestion (the mined can_use_tool shape) —
/// it stays pending until a control_response for req-q1 lands or the
/// process dies, which is exactly the lifecycle the ask tests exercise.
fn run_question_turn() {
    emit(json!({
        "type": "system", "subtype": "init",
        "session_id": "fake-native-1", "model": "fake-model",
        "permissionMode": "default", "slash_commands": ["compact"],
    }));
    emit(json!({
        "type": "control_request",
        "request_id": "req-q1",
        "request": {
            "subtype": "can_use_tool",
            "tool_name": "AskUserQuestion",
            "tool_use_id": "tu-q1",
            "input": { "questions": [{
                "question": "Which database?",
                "header": "Storage",
                "options": [
                    { "label": "SQLite", "description": "single file" },
                    { "label": "Postgres" },
                ],
                "multiSelect": false,
            }]},
        },
    }));
}

/// Publish the plan level-set and park on the dedicated ExitPlanMode prompt.
/// This gives UI/live tests both plan surfaces in one deterministic turn: the
/// pinned progress tray and the markdown approval card with its three official
/// choices. Any response settles the fake turn; response-shape correctness is
/// covered by the driver's focused mapper tests.
fn run_plan_turn() {
    emit(json!({
        "type": "system", "subtype": "init",
        "session_id": "fake-native-1", "model": "fake-model",
        "permissionMode": "plan", "slash_commands": ["compact"],
    }));
    emit(json!({
        "type": "assistant",
        "message": { "id": "m-plan", "content": [{
            "type": "tool_use", "id": "tu-todos", "name": "TodoWrite",
            "input": { "todos": [
                { "content": "Audit the current behavior", "status": "completed" },
                { "content": "Implement the robust path", "status": "in_progress",
                  "activeForm": "Implementing the robust path" },
                { "content": "Verify every lifecycle", "status": "pending" }
            ]}
        }]}
    }));
    emit(json!({
        "type": "control_request",
        "request_id": "req-plan",
        "request": {
            "subtype": "can_use_tool",
            "tool_name": "ExitPlanMode",
            "tool_use_id": "tu-plan",
            "input": { "plan": "## Plan\n\n1. Audit the current behavior.\n2. Implement the robust path.\n3. Verify every lifecycle." }
        }
    }));
}

/// Park a turn with one live subagent and a representative progress update.
/// The ordinary interrupt branch settles it, exercising the tray's complete
/// running → reconciled lifecycle without a real child process.
fn run_subagent_turn() {
    emit(json!({
        "type": "system", "subtype": "init",
        "session_id": "fake-native-1", "model": "fake-model",
        "permissionMode": "default", "slash_commands": ["compact"],
    }));
    emit(json!({
        "type": "assistant",
        "message": { "id": "m-agent", "content": [{
            "type": "tool_use", "id": "tu-agent", "name": "Task",
            "input": { "description": "audit helper", "prompt": "inspect the lifecycle" }
        }]}
    }));
    emit(json!({
        "type": "system", "subtype": "task_started",
        "task_type": "local_agent", "task_id": "task-agent",
        "tool_use_id": "tu-agent", "description": "audit helper",
    }));
    emit(json!({
        "type": "system", "subtype": "task_progress", "task_id": "task-agent",
        "summary": "checking lifecycle edges",
        "usage": { "tool_uses": 3, "total_tokens": 1200, "duration_ms": 7000 },
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
        // Post-turn status line, AFTER the result — the live order (real
        // CLIs emit it on workflow-lifecycle turns). The counter makes each
        // turn's summary distinct so latest-wins folds are assertable;
        // `needs_action` is a STRING on the live wire (empty = nothing
        // needed) — the first turn's is empty, later ones non-empty, so both
        // truthiness mappings get exercised; summarizes_uuid mirrors the
        // live shape and must be dropped by the driver.
        use std::sync::atomic::{AtomicU32, Ordering};
        static TURNS: AtomicU32 = AtomicU32::new(0);
        let n = TURNS.fetch_add(1, Ordering::Relaxed) + 1;
        emit(json!({
            "type": "system", "subtype": "post_turn_summary",
            "session_id": "fake-native-1",
            "summarizes_uuid": "uuid-m1",
            "status_category": "review_ready",
            "status_detail": format!("turn {n} reviewed, awaiting your look"),
            "needs_action": if n == 1 { "" } else { "review the workflow output" },
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
