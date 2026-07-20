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
use chimaera_agent::model::{
    AgentCommand, AgentEvent, CompactionPhase, PermissionOption, PermissionOptionKind, Question,
    ToolKind, ToolStatus, UserMessageState,
};
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
    assert_eq!(
        serde_json::to_value(AgentEvent::ContextCompaction {
            phase: CompactionPhase::Started,
            pre_tokens: None,
        })
        .unwrap(),
        json!({ "type": "context_compaction", "phase": "started" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::ContextCompaction {
            phase: CompactionPhase::Completed,
            pre_tokens: Some(168_000),
        })
        .unwrap(),
        json!({
            "type": "context_compaction",
            "phase": "completed",
            "pre_tokens": 168_000,
        })
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

/// Cross-turn ownership is an additive tool-call flag: ordinary rows retain
/// their exact pre-upgrade shape, while Codex child-thread rows opt in.
#[test]
fn tool_call_cross_turn_flag_is_additive() {
    let call = |cross_turn| AgentEvent::ToolCall {
        id: "agent:child".into(),
        kind: ToolKind::Agent,
        title: "Agent: child".into(),
        locations: Vec::new(),
        status: ToolStatus::InProgress,
        cross_turn,
    };
    assert_eq!(
        serde_json::to_value(call(false)).unwrap(),
        json!({
            "type": "tool_call",
            "id": "agent:child",
            "kind": "agent",
            "title": "Agent: child",
            "status": "in_progress",
        })
    );
    assert_eq!(
        serde_json::to_value(call(true)).unwrap(),
        json!({
            "type": "tool_call",
            "id": "agent:child",
            "kind": "agent",
            "title": "Agent: child",
            "status": "in_progress",
            "cross_turn": true,
        })
    );
    let old: AgentEvent = serde_json::from_value(json!({
        "type": "tool_call",
        "id": "agent:child",
        "kind": "agent",
        "title": "Agent: child",
        "status": "in_progress",
    }))
    .unwrap();
    assert_eq!(old, call(false));
}

/// `Cancelled` is APPENDED to `UserMessageState`, so the two pre-existing
/// states serialize byte-identically (old journals/clients are untouched) and
/// only the user's own pull-back carries the new tag.
#[test]
fn cancelled_state_is_appended_and_leaves_the_others_intact() {
    // The pre-existing states still round-trip with their exact wire tags.
    for (state, tag) in [
        (UserMessageState::Sent, "sent"),
        (UserMessageState::Dropped, "dropped"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(tag));
    }
    // The appended variant serializes to `cancelled` and folds into the same
    // update event shape.
    assert_eq!(
        serde_json::to_value(UserMessageState::Cancelled).unwrap(),
        json!("cancelled")
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::UserMessageUpdate {
            id: "u1".into(),
            state: UserMessageState::Cancelled,
        })
        .unwrap(),
        json!({ "type": "user_message_update", "id": "u1", "state": "cancelled" })
    );
    // An old journal line carrying a pre-upgrade state still deserializes.
    let old: AgentEvent = serde_json::from_value(
        json!({ "type": "user_message_update", "id": "u1", "state": "sent" }),
    )
    .unwrap();
    assert_eq!(
        old,
        AgentEvent::UserMessageUpdate {
            id: "u1".into(),
            state: UserMessageState::Sent,
        }
    );
}

/// `CancelQueued` is APPENDED to `AgentCommand`: the client frame
/// `{type:"cancel_queued", id}` deserializes into it, and every pre-existing
/// command frame keeps parsing unchanged (the wire is a public contract).
#[test]
fn cancel_queued_command_is_additive() {
    let cmd: AgentCommand = serde_json::from_str(r#"{"type":"cancel_queued","id":"u1"}"#).unwrap();
    assert_eq!(cmd, AgentCommand::CancelQueued { id: "u1".into() });
    // Round-trips back to the same frame shape.
    assert_eq!(
        serde_json::to_value(AgentCommand::CancelQueued { id: "u1".into() }).unwrap(),
        json!({ "type": "cancel_queued", "id": "u1" })
    );
    // A pre-upgrade command frame is untouched by the new variant.
    let interrupt: AgentCommand = serde_json::from_str(r#"{"type":"interrupt"}"#).unwrap();
    assert_eq!(interrupt, AgentCommand::Interrupt);
}

/// `SteerQueued` is the explicit Codex queue-promotion action. It is additive
/// to the public WS command vocabulary; ordinary sends keep their old shape.
#[test]
fn steer_queued_command_is_additive() {
    let cmd: AgentCommand = serde_json::from_str(r#"{"type":"steer_queued","id":"u1"}"#).unwrap();
    assert_eq!(cmd, AgentCommand::SteerQueued { id: "u1".into() });
    assert_eq!(
        serde_json::to_value(AgentCommand::SteerQueued { id: "u1".into() }).unwrap(),
        json!({ "type": "steer_queued", "id": "u1" })
    );
    let send: AgentCommand = serde_json::from_str(r#"{"type":"send","blocks":[]}"#).unwrap();
    assert_eq!(send, AgentCommand::Send { blocks: Vec::new() });
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

/// `expires_at_ms` is an additive hint: Claude and older Codex journals omit
/// it, while current Codex question requests may carry one absolute deadline.
#[test]
fn question_request_expiry_is_additive_on_the_wire() {
    let question = Question {
        id: "q1".into(),
        header: String::new(),
        question: "Continue?".into(),
        options: vec![],
        multi_select: false,
    };
    let base = AgentEvent::QuestionRequest {
        request_id: "r1".into(),
        questions: vec![question.clone()],
        expires_at_ms: None,
    };
    assert_eq!(
        serde_json::to_value(&base).unwrap(),
        json!({
            "type": "question_request",
            "request_id": "r1",
            "questions": [{ "id": "q1", "question": "Continue?" }]
        })
    );
    let old: AgentEvent = serde_json::from_value(json!({
        "type": "question_request",
        "request_id": "r1",
        "questions": [{ "id": "q1", "question": "Continue?" }]
    }))
    .unwrap();
    assert_eq!(old, base);

    assert_eq!(
        serde_json::to_value(AgentEvent::QuestionRequest {
            request_id: "r1".into(),
            questions: vec![question],
            expires_at_ms: Some(1_700_000_005_000),
        })
        .unwrap()["expires_at_ms"],
        json!(1_700_000_005_000_u64)
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

/// `Init.agent_version` is additive: absent by default (old clients/journals
/// never saw it), present only when the launcher probed a version.
#[test]
fn init_agent_version_is_additive_on_the_wire() {
    let base = AgentEvent::Init {
        native_session_id: "s1".into(),
        model: None,
        modes: vec![],
        current_mode: None,
        slash_commands: vec![],
        models: vec![],
        agent_version: None,
    };
    // No version → the key is omitted (byte-identical to the pre-upgrade wire).
    assert_eq!(
        serde_json::to_value(&base).unwrap(),
        json!({ "type": "init", "native_session_id": "s1" })
    );
    // An old journal line lacking agent_version still deserializes (to None).
    let old: AgentEvent =
        serde_json::from_value(json!({ "type": "init", "native_session_id": "s1" })).unwrap();
    assert_eq!(old, base);
    // Present only when probed.
    assert_eq!(
        serde_json::to_value(AgentEvent::Init {
            native_session_id: "s1".into(),
            model: None,
            modes: vec![],
            current_mode: None,
            slash_commands: vec![],
            models: vec![],
            agent_version: Some("2.1.206 (Claude Code)".into()),
        })
        .unwrap()["agent_version"],
        json!("2.1.206 (Claude Code)")
    );
}
