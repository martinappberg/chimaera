//! Golden wire-contract tests for the chat event stream.
//!
//! `SeqEvent` and `AgentEvent` are serialized straight onto `/ws/chat/{id}` and
//! into the durable journal — the Svelte chat store reducer folds them directly.
//! No DTO layer, and the client has no automated tests, so a field rename/retag
//! here silently breaks chat mode. These pin the exact shape; a module reshuffle
//! must keep them green, and a deliberate wire change updates them in the same
//! commit. They also pin the invariant `journal::parse_seq` depends on: every
//! serialized `SeqEvent` line begins with `{"seq":` (a release build strips the
//! `debug_assert` that otherwise guards it).

use chimaera_agent::journal::SeqEvent;
use chimaera_agent::model::{AgentEvent, PermissionOption, PermissionOptionKind};
use serde_json::json;

#[test]
fn seq_event_serializes_with_seq_first() {
    let ev = SeqEvent {
        seq: 42,
        ts: 1_700_000_000_000,
        ev: AgentEvent::TurnStarted {
            turn_id: "t1".into(),
        },
    };
    let s = serde_json::to_string(&ev).unwrap();
    assert!(
        s.starts_with("{\"seq\":42,"),
        "SeqEvent must serialize with `seq` as the first key (parse_seq depends on it): {s}"
    );
    assert_eq!(
        serde_json::to_value(&ev).unwrap(),
        json!({
            "seq": 42,
            "ts": 1_700_000_000_000_u64,
            "ev": { "type": "turn_started", "turn_id": "t1" }
        })
    );
}

#[test]
fn agent_event_wire_shapes() {
    // Internally tagged with `type`, snake_case variant names.
    assert_eq!(
        serde_json::to_value(AgentEvent::TurnStarted {
            turn_id: "t".into()
        })
        .unwrap(),
        json!({ "type": "turn_started", "turn_id": "t" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::MessageChunk {
            turn_id: "t".into(),
            text: "hi".into()
        })
        .unwrap(),
        json!({ "type": "message_chunk", "turn_id": "t", "text": "hi" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::ThoughtChunk {
            turn_id: "t".into(),
            text: "hmm".into()
        })
        .unwrap(),
        json!({ "type": "thought_chunk", "turn_id": "t", "text": "hmm" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::ThinkingTokens { tokens: 128 }).unwrap(),
        json!({ "type": "thinking_tokens", "tokens": 128 })
    );
}

#[test]
fn permission_request_wire_shape_is_additive() {
    // The plan field is strictly additive: absent for ordinary permissions
    // (an old client's shape, byte for byte), present only for plan approvals.
    let base = AgentEvent::PermissionRequest {
        request_id: "r1".into(),
        tool_call_id: None,
        title: "Bash".into(),
        options: vec![PermissionOption {
            id: "allow_once".into(),
            label: "Allow".into(),
            kind: PermissionOptionKind::AllowOnce,
        }],
        input_preview: json!({ "command": "ls" }),
        plan: None,
    };
    assert_eq!(
        serde_json::to_value(&base).unwrap(),
        json!({
            "type": "permission_request",
            "request_id": "r1",
            "title": "Bash",
            "options": [{ "id": "allow_once", "label": "Allow", "kind": "allow_once" }],
            "input_preview": { "command": "ls" }
        })
    );

    let planned = AgentEvent::PermissionRequest {
        request_id: "r2".into(),
        tool_call_id: None,
        title: "ExitPlanMode".into(),
        options: vec![],
        input_preview: json!({}),
        plan: Some("## Plan".into()),
    };
    assert_eq!(
        serde_json::to_value(&planned).unwrap()["plan"],
        json!("## Plan")
    );
}
