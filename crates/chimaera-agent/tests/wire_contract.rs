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
use chimaera_agent::model::AgentEvent;
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
fn question_resolved_answers_are_additive_on_the_wire() {
    // Old journals (and old daemons) carry no `answers` — they must keep
    // deserializing, and an empty map must keep serializing without the key.
    let old: AgentEvent =
        serde_json::from_value(json!({ "type": "question_resolved", "request_id": "r1" })).unwrap();
    match &old {
        AgentEvent::QuestionResolved {
            request_id,
            answers,
        } => {
            assert_eq!(request_id, "r1");
            assert!(answers.is_empty());
        }
        other => panic!("expected QuestionResolved, got {other:?}"),
    }
    assert_eq!(
        serde_json::to_value(&old).unwrap(),
        json!({ "type": "question_resolved", "request_id": "r1" })
    );

    let mut answers = std::collections::HashMap::new();
    answers.insert("q1".to_string(), vec!["SQLite".to_string()]);
    assert_eq!(
        serde_json::to_value(AgentEvent::QuestionResolved {
            request_id: "r1".into(),
            answers,
        })
        .unwrap(),
        json!({
            "type": "question_resolved",
            "request_id": "r1",
            "answers": { "q1": ["SQLite"] }
        })
    );
}
