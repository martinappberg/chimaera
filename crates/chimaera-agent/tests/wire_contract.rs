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
use chimaera_agent::model::{AgentEvent, UserMessageState};
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

/// The delivery fields are strictly additive: defaults serialize to nothing
/// (a plain send is byte-identical to the pre-upgrade wire) and pre-upgrade
/// journal lines deserialize with the defaults.
#[test]
fn user_message_delivery_fields_are_additive() {
    assert_eq!(
        serde_json::to_value(AgentEvent::UserMessage {
            text: "hi".into(),
            attachments: 0,
            id: None,
            queued: false,
        })
        .unwrap(),
        json!({ "type": "user_message", "text": "hi" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::UserMessage {
            text: "hi".into(),
            attachments: 0,
            id: Some("u1".into()),
            queued: true,
        })
        .unwrap(),
        json!({ "type": "user_message", "text": "hi", "id": "u1", "queued": true })
    );
    // An old journal line (no id/queued) still parses.
    let old: AgentEvent =
        serde_json::from_value(json!({ "type": "user_message", "text": "hi" })).unwrap();
    assert_eq!(
        old,
        AgentEvent::UserMessage {
            text: "hi".into(),
            attachments: 0,
            id: None,
            queued: false,
        }
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::UserMessageUpdate {
            id: "u1".into(),
            state: UserMessageState::Sent,
        })
        .unwrap(),
        json!({ "type": "user_message_update", "id": "u1", "state": "sent" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::UserMessageUpdate {
            id: "u1".into(),
            state: UserMessageState::Dropped,
        })
        .unwrap(),
        json!({ "type": "user_message_update", "id": "u1", "state": "dropped" })
    );
}

/// `interrupted` is additive the same way: false vanishes from the wire, old
/// lines deserialize false, and only a deliberate user stop sets it.
#[test]
fn turn_aborted_interrupted_flag_is_additive() {
    assert_eq!(
        serde_json::to_value(AgentEvent::TurnAborted {
            turn_id: "t1".into(),
            reason: "boom".into(),
            interrupted: false,
        })
        .unwrap(),
        json!({ "type": "turn_aborted", "turn_id": "t1", "reason": "boom" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::TurnAborted {
            turn_id: "t1".into(),
            reason: "interrupted".into(),
            interrupted: true,
        })
        .unwrap(),
        json!({
            "type": "turn_aborted", "turn_id": "t1",
            "reason": "interrupted", "interrupted": true
        })
    );
    let old: AgentEvent = serde_json::from_value(
        json!({ "type": "turn_aborted", "turn_id": "t1", "reason": "boom" }),
    )
    .unwrap();
    assert_eq!(
        old,
        AgentEvent::TurnAborted {
            turn_id: "t1".into(),
            reason: "boom".into(),
            interrupted: false,
        }
    );
}
