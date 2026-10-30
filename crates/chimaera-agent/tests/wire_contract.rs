//! Golden wire-contract tests for the chat event stream.
//!
//! `SeqEvent` and `AgentEvent` are serialized straight onto `/ws/chat/{id}` and
//! into the durable journal — the Svelte chat store reducer folds them directly.
//! No DTO layer, and the client has no component/browser tests (only reducer tests), so a field rename/retag
//! here silently breaks chat mode. These pin the exact shape; a module reshuffle
//! must keep them green, and a deliberate wire change updates them in the same
//! commit. They also pin the invariant `journal::parse_seq` depends on: every
//! serialized `SeqEvent` line begins with `{"seq":` (a release build strips the
//! `debug_assert` that otherwise guards it).

use chimaera_agent::journal::SeqEvent;
use chimaera_agent::model::{
    AgentCommand, AgentEvent, CompactionPhase, CompactionTrigger, ContentBlock, PermissionOption,
    PermissionOptionKind, Question, RemoteControlSnapshot, RemoteControlState, ToolKind,
    ToolStatus, UserMessageState,
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
            trigger: None,
        })
        .unwrap(),
        json!({ "type": "context_compaction", "phase": "started" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::ContextCompaction {
            phase: CompactionPhase::Completed,
            pre_tokens: Some(168_000),
            trigger: Some(CompactionTrigger::Auto),
        })
        .unwrap(),
        json!({
            "type": "context_compaction",
            "phase": "completed",
            "pre_tokens": 168_000,
            "trigger": "auto",
        })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::AutoCompact {
            enabled: false,
            disabled_by: None,
            picked: Some(false),
        })
        .unwrap(),
        json!({ "type": "auto_compact", "enabled": false, "picked": false })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::AutoCompact {
            enabled: false,
            disabled_by: Some("DISABLE_AUTO_COMPACT".into()),
            picked: None,
        })
        .unwrap(),
        json!({
            "type": "auto_compact",
            "enabled": false,
            "disabled_by": "DISABLE_AUTO_COMPACT",
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
            attachment_paths: Vec::new(),
            id: None,
            queued: false,
            after_turn: false,
            origin: None,
        })
        .unwrap(),
        json!({ "type": "user_message", "text": "hi" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::UserMessage {
            text: "hi".into(),
            attachments: 0,
            attachment_paths: Vec::new(),
            id: Some("u1".into()),
            queued: true,
            after_turn: false,
            origin: None,
        })
        .unwrap(),
        json!({ "type": "user_message", "text": "hi", "id": "u1", "queued": true })
    );
    // A message held for after the turn says so; false never serializes.
    assert_eq!(
        serde_json::to_value(AgentEvent::UserMessage {
            text: "hi".into(),
            attachments: 0,
            attachment_paths: Vec::new(),
            id: Some("u1".into()),
            queued: true,
            after_turn: true,
            origin: None,
        })
        .unwrap(),
        json!({ "type": "user_message", "text": "hi", "id": "u1", "queued": true,
                "after_turn": true })
    );
    // An old journal line (no id/queued) still parses.
    let old: AgentEvent =
        serde_json::from_value(json!({ "type": "user_message", "text": "hi" })).unwrap();
    assert_eq!(
        old,
        AgentEvent::UserMessage {
            text: "hi".into(),
            attachments: 0,
            attachment_paths: Vec::new(),
            id: None,
            queued: false,
            after_turn: false,
            origin: None,
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
        command: None,
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

/// `for_message` (Pass 44) is additive to `background_tool`: the UI's own
/// Ctrl-B frame parses and serializes exactly as before.
#[test]
fn background_tool_for_message_is_additive() {
    let ui: AgentCommand =
        serde_json::from_str(r#"{"type":"background_tool","tool_call_id":"tu-1"}"#).unwrap();
    assert_eq!(
        ui,
        AgentCommand::BackgroundTool {
            tool_call_id: "tu-1".into(),
            for_message: false,
        }
    );
    assert_eq!(
        serde_json::to_value(&ui).unwrap(),
        json!({ "type": "background_tool", "tool_call_id": "tu-1" })
    );
    let daemon = AgentCommand::BackgroundTool {
        tool_call_id: "tu-1".into(),
        for_message: true,
    };
    assert_eq!(
        serde_json::to_value(&daemon).unwrap(),
        json!({ "type": "background_tool", "tool_call_id": "tu-1", "for_message": true })
    );
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
/// `send_after_turn` and `send_now` are appended commands: their frames
/// round-trip, and a plain `send` keeps its exact shape.
#[test]
fn send_after_turn_and_send_now_commands_are_additive() {
    let cmd: AgentCommand =
        serde_json::from_str(r#"{"type":"send_after_turn","blocks":[{"type":"text","text":"x"}]}"#)
            .unwrap();
    assert_eq!(
        cmd,
        AgentCommand::SendAfterTurn {
            blocks: vec![ContentBlock::Text { text: "x".into() }],
        }
    );
    assert_eq!(cmd.send_blocks().map(Vec::len), Some(1));
    assert!(cmd.retained_send_bytes().is_some(), "held like any send");
    let cmd: AgentCommand = serde_json::from_str(r#"{"type":"send_now","id":"u1"}"#).unwrap();
    assert_eq!(cmd, AgentCommand::SendNow { id: "u1".into() });
    assert_eq!(
        serde_json::to_value(&cmd).unwrap(),
        json!({ "type": "send_now", "id": "u1" })
    );
    assert_eq!(
        serde_json::to_value(AgentCommand::Send { blocks: Vec::new() }).unwrap(),
        json!({ "type": "send", "blocks": [] })
    );
}

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
        remote_control_available: false,
        remote_control_auto_enable: false,
        remote_control: None,
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
            remote_control_available: false,
            remote_control_auto_enable: false,
            remote_control: None,
        })
        .unwrap()["agent_version"],
        json!("2.1.206 (Claude Code)")
    );
}

/// Remote Control rides three additive shapes: the `Init` offer flags (omitted
/// when false, so the pre-upgrade Init is byte-identical), the level-set
/// `remote_control` event, the `set_remote_control` command, and the
/// `user_message.origin` marker for bridge-injected messages.
#[test]
fn remote_control_wire_shapes_are_additive() {
    assert_eq!(
        serde_json::to_value(AgentEvent::Init {
            native_session_id: "s1".into(),
            model: None,
            modes: vec![],
            current_mode: None,
            slash_commands: vec![],
            models: vec![],
            agent_version: None,
            remote_control_available: true,
            remote_control_auto_enable: true,
            remote_control: None,
        })
        .unwrap(),
        json!({
            "type": "init", "native_session_id": "s1",
            "remote_control_available": true, "remote_control_auto_enable": true,
        })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::RemoteControl {
            state: RemoteControlState::Connected,
            session_url: Some("https://claude.ai/code/session_01X".into()),
            name: Some("chimaera dev".into()),
            detail: None,
        })
        .unwrap(),
        json!({
            "type": "remote_control", "state": "connected",
            "session_url": "https://claude.ai/code/session_01X", "name": "chimaera dev",
        })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::RemoteControl {
            state: RemoteControlState::Off,
            session_url: None,
            name: None,
            detail: None,
        })
        .unwrap(),
        json!({ "type": "remote_control", "state": "off" })
    );
    // Init carries the live bridge as a snapshot (same field names as the
    // event) so a repeated Init never blanks it; absent = no bridge.
    assert_eq!(
        serde_json::to_value(AgentEvent::Init {
            native_session_id: "s1".into(),
            model: None,
            modes: vec![],
            current_mode: None,
            slash_commands: vec![],
            models: vec![],
            agent_version: None,
            remote_control_available: false,
            remote_control_auto_enable: false,
            remote_control: Some(RemoteControlSnapshot {
                state: RemoteControlState::Connected,
                session_url: Some("https://claude.ai/code/session_01X".into()),
                name: None,
            }),
        })
        .unwrap(),
        json!({
            "type": "init", "native_session_id": "s1",
            "remote_control": {
                "state": "connected",
                "session_url": "https://claude.ai/code/session_01X",
            },
        })
    );
    let cmd: AgentCommand =
        serde_json::from_str(r#"{"type":"set_remote_control","enabled":true,"name":"n"}"#).unwrap();
    assert_eq!(
        cmd,
        AgentCommand::SetRemoteControl {
            enabled: true,
            name: Some("n".into()),
        }
    );
    let cmd: AgentCommand =
        serde_json::from_str(r#"{"type":"set_remote_control","enabled":false}"#).unwrap();
    assert_eq!(
        cmd,
        AgentCommand::SetRemoteControl {
            enabled: false,
            name: None,
        }
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::UserMessage {
            text: "from my phone".into(),
            attachments: 0,
            attachment_paths: Vec::new(),
            id: None,
            queued: false,
            after_turn: false,
            origin: Some("remote".into()),
        })
        .unwrap(),
        json!({ "type": "user_message", "text": "from my phone", "origin": "remote" })
    );
}

/// `mode_changed.chosen` / `effort_state.chosen` are additive: omitted when
/// false (byte-identical to the pre-upgrade wire), present only for a pick.
#[test]
fn mode_and_effort_chosen_flags_are_additive() {
    assert_eq!(
        serde_json::to_value(AgentEvent::ModeChanged {
            mode_id: "plan".into(),
            chosen: false,
        })
        .unwrap(),
        json!({ "type": "mode_changed", "mode_id": "plan" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::ModeChanged {
            mode_id: "plan".into(),
            chosen: true,
        })
        .unwrap(),
        json!({ "type": "mode_changed", "mode_id": "plan", "chosen": true })
    );
    let old: AgentEvent =
        serde_json::from_value(json!({ "type": "effort_state", "effort": "high" })).unwrap();
    assert_eq!(
        old,
        AgentEvent::EffortState {
            effort: Some("high".into()),
            ultracode: false,
            chosen: false,
        }
    );
}

/// Pass 31 additions — every one strictly additive (old clients skip the
/// unknown tags; the optional fields vanish when empty).
#[test]
fn transcript_surface_wire_shapes_are_additive() {
    assert_eq!(
        serde_json::to_value(AgentEvent::ToolSummary {
            summary: "Listed files in directory".into(),
            tool_ids: vec!["toolu_1".into()],
        })
        .unwrap(),
        json!({
            "type": "tool_summary", "summary": "Listed files in directory",
            "tool_ids": ["toolu_1"],
        })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::SubagentFinished {
            id: Some("toolu_2".into()),
            label: "Measure file sizes".into(),
            status: "completed".into(),
            result: Some("Both files are 6 bytes.".into()),
            stats: Some("2 tools · 12.3k tokens · 4s".into()),
        })
        .unwrap(),
        json!({
            "type": "subagent_finished", "id": "toolu_2", "label": "Measure file sizes",
            "status": "completed", "result": "Both files are 6 bytes.",
            "stats": "2 tools · 12.3k tokens · 4s",
        })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::SubagentFinished {
            id: None,
            label: "x".into(),
            status: "stopped".into(),
            result: None,
            stats: None,
        })
        .unwrap(),
        json!({ "type": "subagent_finished", "label": "x", "status": "stopped" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::TurnTokens { output: 1240 }).unwrap(),
        json!({ "type": "turn_tokens", "output": 1240 })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::ActivityLine {
            detail: Some("Counting files".into())
        })
        .unwrap(),
        json!({ "type": "activity_line", "detail": "Counting files" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::ActivityLine { detail: None }).unwrap(),
        json!({ "type": "activity_line" })
    );
    let task = |monitor, ambient| chimaera_agent::model::BackgroundTask {
        id: "bg-1".into(),
        task_type: "local_bash".into(),
        description: "Watch the build log".into(),
        status: "running".into(),
        started_at_ms: 1,
        workflow_name: None,
        agents: Vec::new(),
        agents_total: 0,
        agents_done: 0,
        monitor,
        ambient,
        model: None,
        tool_use_id: None,
    };
    let plain = serde_json::to_value(task(false, false)).unwrap();
    assert!(plain.get("monitor").is_none() && plain.get("ambient").is_none());
    let flagged = serde_json::to_value(task(true, true)).unwrap();
    assert_eq!(flagged["monitor"], json!(true));
    assert_eq!(flagged["ambient"], json!(true));
}

/// Loops and goals (PROTOCOL.md Pass 47) ride three additive event shapes —
/// the `schedule` level-set, the latest-wins `goal` with its one-shot
/// `ended`, and `turn_cause` — and four goal commands. Empty parts are
/// omitted, so an empty schedule is just its tag.
#[test]
fn loop_and_goal_wire_shapes_are_additive() {
    use chimaera_agent::model::{GoalEnd, GoalState, ScheduledJob, ScheduledWakeup};
    assert_eq!(
        serde_json::to_value(AgentEvent::Schedule {
            wakeup: None,
            jobs: vec![]
        })
        .unwrap(),
        json!({ "type": "schedule" })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::Schedule {
            wakeup: Some(ScheduledWakeup {
                at_ms: 2,
                scheduled_at_ms: 1,
                reason: "Check the deploy".into(),
                prompt: String::new(),
            }),
            jobs: vec![ScheduledJob {
                id: "f2143f41".into(),
                cron: "*/10 * * * *".into(),
                schedule: "Every 10 minutes".into(),
                prompt: "post the graph".into(),
                recurring: true,
                next_at_ms: None,
                created_at_ms: 1,
            }],
        })
        .unwrap(),
        json!({
            "type": "schedule",
            "wakeup": { "at_ms": 2, "scheduled_at_ms": 1, "reason": "Check the deploy" },
            "jobs": [{ "id": "f2143f41", "cron": "*/10 * * * *", "schedule": "Every 10 minutes",
                       "prompt": "post the graph", "recurring": true, "created_at_ms": 1 }],
        })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::Goal {
            goal: Some(GoalState {
                objective: "all tests pass".into(),
                status: "usageLimited".into(),
                tokens_used: Some(10),
                token_budget: Some(100),
                ..Default::default()
            }),
            ended: None,
        })
        .unwrap(),
        json!({ "type": "goal", "goal": { "objective": "all tests pass", "status": "usageLimited",
                "tokens_used": 10, "token_budget": 100 } })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::Goal {
            goal: None,
            ended: Some(GoalEnd {
                outcome: "achieved".into(),
                objective: "x".into(),
                turns: Some(3),
                ..Default::default()
            }),
        })
        .unwrap(),
        json!({ "type": "goal", "ended": { "outcome": "achieved", "objective": "x", "turns": 3 } })
    );
    assert_eq!(
        serde_json::to_value(AgentEvent::TurnCause {
            turn_id: "t2".into(),
            kind: "job".into(),
            detail: Some("post the graph".into()),
            schedule: Some("Every 10 minutes".into()),
        })
        .unwrap(),
        json!({ "type": "turn_cause", "turn_id": "t2", "kind": "job",
                "detail": "post the graph", "schedule": "Every 10 minutes" })
    );
    for (cmd, wire) in [
        (
            AgentCommand::SetGoal {
                objective: "o".into(),
                token_budget: None,
            },
            json!({ "type": "set_goal", "objective": "o" }),
        ),
        (AgentCommand::PauseGoal, json!({ "type": "pause_goal" })),
        (AgentCommand::ResumeGoal, json!({ "type": "resume_goal" })),
        (AgentCommand::ClearGoal, json!({ "type": "clear_goal" })),
        (
            AgentCommand::SetAutoCompact { enabled: false },
            json!({ "type": "set_auto_compact", "enabled": false }),
        ),
    ] {
        assert_eq!(serde_json::to_value(&cmd).unwrap(), wire);
        assert_eq!(serde_json::from_value::<AgentCommand>(wire).unwrap(), cmd);
    }
    assert!(AgentCommand::SetGoal {
        objective: "  ".into(),
        token_budget: None
    }
    .validate_ingress()
    .is_err());
}

/// Every `AgentEvent` variant, wrapped as the `SeqEvent` the journal and
/// `/ws/chat` carry, written to `web-ui/src/lib/net/fixtures/wire/chat-events.json`
/// for the web UI's consumer tests (`web-ui/src/lib/net/wireFixtures.test.ts`):
/// the chat socket decoder and the store reducer must accept each one. The
/// assertions above pin individual shapes; this pins the whole vocabulary
/// where the client can read it. Adding a variant without a case here is a
/// compile error (the match below is exhaustive by construction: every
/// variant is listed once).
#[test]
fn chat_events_fixture() {
    use chimaera_agent::capabilities::ChatCapabilities;
    use chimaera_agent::elicitation::Elicitation;
    use chimaera_agent::model::{
        BackgroundTask, BackgroundTaskClose, GoalEnd, GoalState, McpServerInfo, ModeInfo,
        ModelInfo, PlanEntry, PlanStatus, QuestionOption, ScheduledJob, ScheduledWakeup, SessionUi,
        SlashCommand, ToolContent, Usage, UsageWindow, WorkflowAgent,
    };
    use chimaera_wire_fixtures::{case, pin, Case};

    let events: Vec<(&'static str, &'static str, AgentEvent)> = vec![
        (
            "startup_progress",
            "latest startup phase while the driver initializes",
            AgentEvent::StartupProgress {
                detail: "Starting claude".into(),
            },
        ),
        (
            "fork_context_consumed",
            "the agent accepted the copied fork context (journal marker)",
            AgentEvent::ForkContextConsumed,
        ),
        (
            "capabilities",
            "the adapter's control snapshot (claude's legacy map)",
            AgentEvent::Capabilities {
                capabilities: ChatCapabilities::legacy("claude"),
            },
        ),
        (
            "catalog",
            "catalog refresh: models, modes, slash commands",
            AgentEvent::Catalog {
                models: vec![ModelInfo {
                    id: "opus".into(),
                    label: "Opus".into(),
                    description: Some("Most capable".into()),
                    resolved: Some("claude-opus-5-5".into()),
                    efforts: vec!["low".into(), "high".into()],
                    default_effort: Some("high".into()),
                }],
                modes: vec![ModeInfo {
                    id: "plan".into(),
                    label: "Plan".into(),
                }],
                slash_commands: vec![SlashCommand {
                    name: "review".into(),
                    description: "Review the diff".into(),
                    skill_path: Some("/home/me/.codex/skills/review".into()),
                }],
            },
        ),
        (
            "init_minimal",
            "driver handshake with nothing optional set",
            AgentEvent::Init {
                native_session_id: "sess-1".into(),
                model: None,
                modes: vec![],
                current_mode: None,
                slash_commands: vec![],
                models: vec![],
                agent_version: None,
                remote_control_available: false,
                remote_control_auto_enable: false,
                remote_control: None,
            },
        ),
        (
            "init_full",
            "driver handshake with the full catalog and a live Remote Control bridge",
            AgentEvent::Init {
                native_session_id: "sess-1".into(),
                model: Some("claude-opus-5-5".into()),
                modes: vec![
                    ModeInfo {
                        id: "default".into(),
                        label: "Default".into(),
                    },
                    ModeInfo {
                        id: "plan".into(),
                        label: "Plan".into(),
                    },
                ],
                current_mode: Some("default".into()),
                slash_commands: vec![SlashCommand {
                    name: "compact".into(),
                    description: String::new(),
                    skill_path: None,
                }],
                models: vec![ModelInfo {
                    id: "sonnet".into(),
                    label: "Sonnet".into(),
                    description: None,
                    resolved: None,
                    efforts: vec![],
                    default_effort: None,
                }],
                agent_version: Some("2.1.283 (Claude Code)".into()),
                remote_control_available: true,
                remote_control_auto_enable: true,
                remote_control: Some(RemoteControlSnapshot {
                    state: RemoteControlState::Connected,
                    session_url: Some("https://claude.ai/code/session_01X".into()),
                    name: Some("chimaera dev".into()),
                }),
            },
        ),
        (
            "turn_started",
            "a turn opened",
            AgentEvent::TurnStarted {
                turn_id: "t1".into(),
            },
        ),
        (
            "message_chunk",
            "a prose delta",
            AgentEvent::MessageChunk {
                turn_id: "t1".into(),
                text: "Hello, ".into(),
            },
        ),
        (
            "message_identity",
            "the native message id a prose run belongs to",
            AgentEvent::MessageIdentity {
                turn_id: "t1".into(),
                message_id: "msg_01".into(),
            },
        ),
        (
            "thought_chunk",
            "a thinking delta",
            AgentEvent::ThoughtChunk {
                turn_id: "t1".into(),
                text: "Considering…".into(),
            },
        ),
        (
            "thinking_tokens",
            "the thinking budget in force",
            AgentEvent::ThinkingTokens { tokens: 31_999 },
        ),
        (
            "user_message_plain",
            "a turn-opening send, nothing optional",
            AgentEvent::UserMessage {
                text: "hi".into(),
                attachments: 0,
                attachment_paths: vec![],
                id: None,
                queued: false,
                after_turn: false,
                origin: None,
            },
        ),
        (
            "user_message_queued_with_images",
            "a mid-turn send parked in the queue, with saved image copies",
            AgentEvent::UserMessage {
                text: "meanwhile do X".into(),
                attachments: 1,
                attachment_paths: vec!["/home/me/.chimaera/uploads/s1/image-1.png".into()],
                id: Some("u1".into()),
                queued: true,
                after_turn: true,
                origin: None,
            },
        ),
        (
            "user_message_from_agent",
            "another agent's message the daemon delivered as a send",
            AgentEvent::UserMessage {
                text: "[from worker] done".into(),
                attachments: 0,
                attachment_paths: vec![],
                id: Some("u2".into()),
                queued: false,
                after_turn: false,
                origin: Some("agent".into()),
            },
        ),
        (
            "tool_call",
            "a tool started, with its command and a touched location",
            AgentEvent::ToolCall {
                id: "toolu_1".into(),
                kind: ToolKind::Execute,
                title: "cargo test".into(),
                locations: vec!["/home/me/proj/Cargo.toml".into()],
                status: ToolStatus::InProgress,
                cross_turn: false,
                command: Some("cargo test -p chimaera-agent".into()),
            },
        ),
        (
            "tool_call_subagent_cross_turn",
            "a delegated agent row that outlives its turn",
            AgentEvent::ToolCall {
                id: "agent:child".into(),
                kind: ToolKind::Agent,
                title: "Agent: child".into(),
                locations: vec![],
                status: ToolStatus::InProgress,
                cross_turn: true,
                command: None,
            },
        ),
        (
            "tool_call_update_output",
            "a tool finished with captured output",
            AgentEvent::ToolCallUpdate {
                id: "toolu_1".into(),
                status: ToolStatus::Completed,
                content: Some(ToolContent::Output {
                    text: "test result: ok. 3 passed\n".into(),
                    truncated: false,
                }),
            },
        ),
        (
            "tool_call_update_diff",
            "an edit finished with a per-file diff",
            AgentEvent::ToolCallUpdate {
                id: "toolu_2".into(),
                status: ToolStatus::Completed,
                content: Some(ToolContent::Diff {
                    path: "/home/me/proj/src/lib.rs".into(),
                    old_text: Some("fn a() {}\n".into()),
                    new_text: "fn a() { b() }\n".into(),
                    truncated: false,
                }),
            },
        ),
        (
            "tool_call_update_batch",
            "codex-style aggregated changes, one truncated",
            AgentEvent::ToolCallUpdate {
                id: "toolu_3".into(),
                status: ToolStatus::Failed,
                content: Some(ToolContent::Batch {
                    diffs: vec![ToolContent::Diff {
                        path: "a.txt".into(),
                        old_text: None,
                        new_text: "x".into(),
                        truncated: true,
                    }],
                }),
            },
        ),
        (
            "tool_call_update_bare",
            "a status change with no content",
            AgentEvent::ToolCallUpdate {
                id: "toolu_1".into(),
                status: ToolStatus::Pending,
                content: None,
            },
        ),
        (
            "tool_render_data",
            "exact native tool input/output for Mod renderers",
            AgentEvent::ToolRenderData {
                id: "toolu_1".into(),
                name: Some("Bash".into()),
                input: Some(json!({ "command": "pwd" })),
                output: Some(json!({ "stdout": "/tmp", "stderr": "" })),
            },
        ),
        (
            "tool_output_delta",
            "streamed tool output",
            AgentEvent::ToolOutputDelta {
                id: "toolu_1".into(),
                text: "Compiling…\n".into(),
            },
        ),
        (
            "plan",
            "the task list with every claude Task* field",
            AgentEvent::Plan {
                entries: vec![
                    PlanEntry {
                        content: "Write tests".into(),
                        status: PlanStatus::InProgress,
                        id: Some("1".into()),
                        active_form: Some("Writing tests".into()),
                        description: Some("Cover the decoder".into()),
                        owner: Some("worker-a".into()),
                        blocked_by: vec![],
                    },
                    PlanEntry {
                        content: "Ship".into(),
                        status: PlanStatus::Todo,
                        blocked_by: vec!["1".into()],
                        ..Default::default()
                    },
                ],
            },
        ),
        (
            "permission_request",
            "a tool permission with the four ACP option kinds",
            AgentEvent::PermissionRequest {
                request_id: "perm-1".into(),
                tool_call_id: Some("toolu_1".into()),
                title: "Bash".into(),
                options: vec![
                    PermissionOption {
                        id: "allow_once".into(),
                        label: "Allow".into(),
                        kind: PermissionOptionKind::AllowOnce,
                    },
                    PermissionOption {
                        id: "allow_always".into(),
                        label: "Always allow".into(),
                        kind: PermissionOptionKind::AllowAlways,
                    },
                    PermissionOption {
                        id: "reject_once".into(),
                        label: "Deny".into(),
                        kind: PermissionOptionKind::RejectOnce,
                    },
                    PermissionOption {
                        id: "reject_always".into(),
                        label: "Never".into(),
                        kind: PermissionOptionKind::RejectAlways,
                    },
                ],
                input_preview: json!({ "command": "rm -rf build" }),
                plan: None,
            },
        ),
        (
            "permission_request_plan",
            "a plan approval (ExitPlanMode) carrying the plan text",
            AgentEvent::PermissionRequest {
                request_id: "perm-2".into(),
                tool_call_id: None,
                title: "ExitPlanMode".into(),
                options: vec![PermissionOption {
                    id: "allow_once".into(),
                    label: "Approve".into(),
                    kind: PermissionOptionKind::AllowOnce,
                }],
                input_preview: json!({}),
                plan: Some("## Plan\n1. Do it".into()),
            },
        ),
        (
            "permission_resolved",
            "the answer to a permission",
            AgentEvent::PermissionResolved {
                request_id: "perm-1".into(),
                option_id: "allow_once".into(),
            },
        ),
        (
            "elicitation_request",
            "an MCP form elicitation",
            AgentEvent::ElicitationRequest {
                request_id: "el-1".into(),
                server: "github".into(),
                message: "Pick a repository".into(),
                elicitation: Elicitation::parse(
                    "form",
                    &json!({
                        "type": "object",
                        "required": ["repo"],
                        "properties": {
                            "repo": { "type": "string", "title": "Repository", "minLength": 1 },
                            "private": { "type": "boolean", "title": "Private", "default": false }
                        }
                    }),
                    None,
                ),
            },
        ),
        (
            "elicitation_resolved",
            "the elicitation's outcome",
            AgentEvent::ElicitationResolved {
                request_id: "el-1".into(),
                action: "accept".into(),
            },
        ),
        (
            "question_request",
            "a structured question with options and a deadline",
            AgentEvent::QuestionRequest {
                request_id: "q-1".into(),
                questions: vec![Question {
                    id: "store".into(),
                    header: "Storage".into(),
                    question: "Which store?".into(),
                    options: vec![
                        QuestionOption {
                            label: "SQLite".into(),
                            description: "Single file".into(),
                        },
                        QuestionOption {
                            label: "JSONL".into(),
                            description: String::new(),
                        },
                    ],
                    multi_select: false,
                }],
                expires_at_ms: Some(1_700_000_005_000),
            },
        ),
        (
            "question_resolved",
            "the answers to a question",
            AgentEvent::QuestionResolved {
                request_id: "q-1".into(),
                answers: std::collections::HashMap::from([(
                    "store".to_string(),
                    vec!["JSONL".to_string()],
                )]),
            },
        ),
        (
            "mode_changed",
            "the user picked a permission mode",
            AgentEvent::ModeChanged {
                mode_id: "plan".into(),
                chosen: true,
            },
        ),
        (
            "effort_state",
            "effort and ultracode as they stand",
            AgentEvent::EffortState {
                effort: Some("xhigh".into()),
                ultracode: true,
                chosen: false,
            },
        ),
        (
            "turn_completed",
            "a turn ended with usage",
            AgentEvent::TurnCompleted {
                turn_id: "t1".into(),
                usage: Usage {
                    cost_usd: Some(0.0123),
                    input_tokens: 1200,
                    output_tokens: 340,
                    total_tokens: 1540,
                    duration_ms: 4200,
                    ..Default::default()
                },
            },
        ),
        (
            "turn_aborted",
            "the user interrupted the turn",
            AgentEvent::TurnAborted {
                turn_id: "t1".into(),
                reason: "interrupted".into(),
                interrupted: true,
            },
        ),
        (
            "context_usage",
            "context window fill, with the threshold the agent compacts at",
            AgentEvent::ContextUsage {
                total_tokens: 84_000,
                max_tokens: 200_000,
                percentage: 42.0,
                auto_compact_at: Some(160_000),
            },
        ),
        (
            "context_compaction",
            "compaction completed",
            AgentEvent::ContextCompaction {
                phase: CompactionPhase::Completed,
                pre_tokens: Some(168_000),
                trigger: Some(CompactionTrigger::Auto),
            },
        ),
        (
            "auto_compact",
            "the user turned auto-compact off for this chat",
            AgentEvent::AutoCompact {
                enabled: false,
                disabled_by: None,
                picked: Some(false),
            },
        ),
        (
            "auto_compact_held_off",
            "the agent's environment holds auto-compact off",
            AgentEvent::AutoCompact {
                enabled: false,
                disabled_by: Some("DISABLE_AUTO_COMPACT".into()),
                picked: None,
            },
        ),
        (
            "usage_report",
            "account usage windows (get_usage)",
            AgentEvent::UsageReport {
                windows: vec![UsageWindow {
                    label: "5h".into(),
                    utilization: 37.0,
                    resets_at: Some("2026-10-16T18:00:00Z".into()),
                }],
            },
        ),
        (
            "rate_limit",
            "a streamed rate-limit event that hit the limit",
            AgentEvent::RateLimit {
                utilization: 1.0,
                resets_at: Some("2026-10-16T18:00:00Z".into()),
                label: Some("5h".into()),
                limit_reached: true,
                windows: vec![UsageWindow {
                    label: "7d".into(),
                    utilization: 61.0,
                    resets_at: None,
                }],
            },
        ),
        (
            "model_switched",
            "the runtime switched models mid-session",
            AgentEvent::ModelSwitched {
                from: Some("claude-opus-5-5".into()),
                to: "claude-sonnet-5-5".into(),
                reason: Some("fast mode".into()),
                retract_current_turn: true,
            },
        ),
        (
            "messages_superseded",
            "the native transcript replaced the shown messages",
            AgentEvent::MessagesSuperseded,
        ),
        (
            "thought_retracted",
            "the tail of a thinking run was withdrawn",
            AgentEvent::ThoughtRetracted {
                turn_id: "t1".into(),
                utf16_len: 12,
            },
        ),
        (
            "session_title",
            "the agent named the conversation",
            AgentEvent::SessionTitle {
                title: "Wire fixtures".into(),
            },
        ),
        (
            "checkpoint",
            "a rewind checkpoint for a user message",
            AgentEvent::Checkpoint {
                user_message_id: "u1".into(),
                preceding_uuid: Some("p0".into()),
            },
        ),
        (
            "rewind_result",
            "a rewind dry run answered",
            AgentEvent::RewindResult {
                user_message_id: "u1".into(),
                can_rewind: true,
                files_changed: vec!["/home/me/proj/src/lib.rs".into()],
                applied: false,
                error: None,
            },
        ),
        (
            "mcp_servers",
            "the MCP inventory",
            AgentEvent::McpServers {
                servers: vec![
                    McpServerInfo {
                        name: "github".into(),
                        status: "connected".into(),
                        tools: 12,
                        error: None,
                    },
                    McpServerInfo {
                        name: "db".into(),
                        status: "failed".into(),
                        tools: 0,
                        error: Some("connection refused".into()),
                    },
                ],
            },
        ),
        (
            "notice",
            "a one-line notice",
            AgentEvent::Notice {
                text: "Context compacted".into(),
            },
        ),
        (
            "prompt_suggestion",
            "a suggested next prompt",
            AgentEvent::PromptSuggestion {
                text: "Run the tests".into(),
            },
        ),
        (
            "truncated",
            "journal history was trimmed",
            AgentEvent::Truncated,
        ),
        (
            "mode_switch",
            "the session moved to a terminal",
            AgentEvent::ModeSwitch {
                to: SessionUi::Term,
            },
        ),
        (
            "forked",
            "this journal was forked from another session",
            AgentEvent::Forked {
                source_agent: "claude".into(),
                source_seq: 118,
                native: true,
            },
        ),
        (
            "error_sign_in",
            "a fatal error asking the user to sign in",
            AgentEvent::Error {
                message: "Not logged in".into(),
                fatal: true,
                sign_in: true,
            },
        ),
        (
            "exited",
            "the driver process ended",
            AgentEvent::Exited { status: Some(0) },
        ),
        (
            "exited_unknown_status",
            "the driver process ended without a status",
            AgentEvent::Exited { status: None },
        ),
        (
            "user_message_update",
            "a queued message was read",
            AgentEvent::UserMessageUpdate {
                id: "u1".into(),
                state: UserMessageState::Sent,
            },
        ),
        (
            "background_tasks",
            "the background level-set: a workflow with agents, a monitor, a close",
            AgentEvent::BackgroundTasks {
                tasks: vec![
                    BackgroundTask {
                        id: "bg-1".into(),
                        task_type: "local_workflow".into(),
                        description: "review-changes".into(),
                        status: "running".into(),
                        started_at_ms: 1_700_000_000_000,
                        workflow_name: Some("review-changes".into()),
                        agents: vec![WorkflowAgent {
                            index: 1,
                            label: "review:bugs".into(),
                            state: "done".into(),
                            result_preview: Some("2 findings".into()),
                        }],
                        agents_total: 2,
                        agents_done: 1,
                        monitor: false,
                        ambient: false,
                        model: None,
                        tool_use_id: None,
                    },
                    BackgroundTask {
                        id: "bg-2".into(),
                        task_type: "local_bash".into(),
                        description: "Watch the build log".into(),
                        status: "running".into(),
                        started_at_ms: 1_700_000_001_000,
                        workflow_name: None,
                        agents: vec![],
                        agents_total: 0,
                        agents_done: 0,
                        monitor: true,
                        ambient: true,
                        model: Some("sonnet".into()),
                        tool_use_id: None,
                    },
                ],
                closed: vec![BackgroundTaskClose {
                    id: "bg-0".into(),
                    description: "cargo build".into(),
                    status: "completed".into(),
                    summary: Some("Finished".into()),
                    output_file: Some("/tmp/bg-0.log".into()),
                    monitor: false,
                }],
            },
        ),
        (
            "session_status",
            "the agent's post-turn status line",
            AgentEvent::SessionStatus {
                category: Some("review_ready".into()),
                detail: "PR open, waiting on review".into(),
                needs_action: true,
            },
        ),
        (
            "remote_control",
            "the Remote Control bridge failed",
            AgentEvent::RemoteControl {
                state: RemoteControlState::Error,
                session_url: None,
                name: Some("chimaera dev".into()),
                detail: Some("refused by policy".into()),
            },
        ),
        (
            "tool_summary",
            "a summary line over a batch of tools",
            AgentEvent::ToolSummary {
                summary: "Listed files in directory".into(),
                tool_ids: vec!["toolu_1".into(), "toolu_2".into()],
            },
        ),
        (
            "subagent_finished",
            "a subagent's closing words",
            AgentEvent::SubagentFinished {
                id: Some("toolu_2".into()),
                label: "Measure file sizes".into(),
                status: "completed".into(),
                result: Some("Both files are 6 bytes.".into()),
                stats: Some("2 tools · 12.3k tokens · 4s".into()),
            },
        ),
        (
            "subagent_info",
            "a subagent's identity once known",
            AgentEvent::SubagentInfo {
                id: "toolu_2".into(),
                agent_id: Some("agent-7f".into()),
                model: Some("claude-sonnet-5-5".into()),
                agent_type: Some("Explore".into()),
            },
        ),
        (
            "turn_tokens",
            "output tokens so far this turn",
            AgentEvent::TurnTokens { output: 1240 },
        ),
        (
            "activity_line",
            "what the agent says it is doing",
            AgentEvent::ActivityLine {
                detail: Some("Counting files".into()),
            },
        ),
        (
            "agent_message",
            "a message from another agent, a reply to an earlier one",
            AgentEvent::agent_message(
                7,
                "s-worker",
                "worker-a",
                Some("codex"),
                "Tests are green.",
                false,
                false,
                Some(3),
            ),
        ),
        (
            "schedule",
            "the loop level-set: a wakeup and a job",
            AgentEvent::Schedule {
                wakeup: Some(ScheduledWakeup {
                    at_ms: 1_700_000_600_000,
                    scheduled_at_ms: 1_700_000_000_000,
                    reason: "Check the deploy".into(),
                    prompt: "/loop check deploy".into(),
                }),
                jobs: vec![ScheduledJob {
                    id: "f2143f41".into(),
                    cron: "*/10 * * * *".into(),
                    schedule: "Every 10 minutes".into(),
                    prompt: "post the graph".into(),
                    recurring: true,
                    next_at_ms: Some(1_700_000_600_000),
                    created_at_ms: 1_700_000_000_000,
                }],
            },
        ),
        (
            "goal",
            "a goal as it stands (codex, with a budget)",
            AgentEvent::Goal {
                goal: Some(GoalState {
                    objective: "all tests pass".into(),
                    status: "active".into(),
                    set_at_ms: 1_700_000_000_000,
                    turns: None,
                    last_check: None,
                    tokens_used: Some(10),
                    token_budget: Some(100),
                    time_used_secs: Some(42),
                }),
                ended: None,
            },
        ),
        (
            "goal_ended",
            "a goal ended (claude, achieved after three checks)",
            AgentEvent::Goal {
                goal: None,
                ended: Some(GoalEnd {
                    outcome: "achieved".into(),
                    objective: "all tests pass".into(),
                    reason: Some("The suite is green.".into()),
                    turns: Some(3),
                    tokens: None,
                    duration_secs: Some(300),
                }),
            },
        ),
        (
            "turn_cause",
            "what resumed the agent on its own",
            AgentEvent::TurnCause {
                turn_id: "t2".into(),
                kind: "job".into(),
                detail: Some("post the graph".into()),
                schedule: Some("Every 10 minutes".into()),
            },
        ),
    ];

    // Exhaustiveness: a new variant must get a case here. Written as a
    // match so the compiler, not a reviewer, notices the gap.
    fn tag(ev: &AgentEvent) -> &'static str {
        match ev {
            AgentEvent::StartupProgress { .. } => "startup_progress",
            AgentEvent::ForkContextConsumed => "fork_context_consumed",
            AgentEvent::Capabilities { .. } => "capabilities",
            AgentEvent::Catalog { .. } => "catalog",
            AgentEvent::Init { .. } => "init",
            AgentEvent::TurnStarted { .. } => "turn_started",
            AgentEvent::MessageChunk { .. } => "message_chunk",
            AgentEvent::MessageIdentity { .. } => "message_identity",
            AgentEvent::ThoughtChunk { .. } => "thought_chunk",
            AgentEvent::ThinkingTokens { .. } => "thinking_tokens",
            AgentEvent::UserMessage { .. } => "user_message",
            AgentEvent::ToolCall { .. } => "tool_call",
            AgentEvent::ToolCallUpdate { .. } => "tool_call_update",
            AgentEvent::ToolRenderData { .. } => "tool_render_data",
            AgentEvent::ToolOutputDelta { .. } => "tool_output_delta",
            AgentEvent::Plan { .. } => "plan",
            AgentEvent::PermissionRequest { .. } => "permission_request",
            AgentEvent::PermissionResolved { .. } => "permission_resolved",
            AgentEvent::ElicitationRequest { .. } => "elicitation_request",
            AgentEvent::ElicitationResolved { .. } => "elicitation_resolved",
            AgentEvent::QuestionRequest { .. } => "question_request",
            AgentEvent::QuestionResolved { .. } => "question_resolved",
            AgentEvent::ModeChanged { .. } => "mode_changed",
            AgentEvent::EffortState { .. } => "effort_state",
            AgentEvent::AutoCompact { .. } => "auto_compact",
            AgentEvent::TurnCompleted { .. } => "turn_completed",
            AgentEvent::TurnAborted { .. } => "turn_aborted",
            AgentEvent::ContextUsage { .. } => "context_usage",
            AgentEvent::ContextCompaction { .. } => "context_compaction",
            AgentEvent::UsageReport { .. } => "usage_report",
            AgentEvent::RateLimit { .. } => "rate_limit",
            AgentEvent::ModelSwitched { .. } => "model_switched",
            AgentEvent::MessagesSuperseded => "messages_superseded",
            AgentEvent::ThoughtRetracted { .. } => "thought_retracted",
            AgentEvent::SessionTitle { .. } => "session_title",
            AgentEvent::Checkpoint { .. } => "checkpoint",
            AgentEvent::RewindResult { .. } => "rewind_result",
            AgentEvent::McpServers { .. } => "mcp_servers",
            AgentEvent::Notice { .. } => "notice",
            AgentEvent::PromptSuggestion { .. } => "prompt_suggestion",
            AgentEvent::Truncated => "truncated",
            AgentEvent::ModeSwitch { .. } => "mode_switch",
            AgentEvent::Forked { .. } => "forked",
            AgentEvent::Error { .. } => "error",
            AgentEvent::Exited { .. } => "exited",
            AgentEvent::UserMessageUpdate { .. } => "user_message_update",
            AgentEvent::BackgroundTasks { .. } => "background_tasks",
            AgentEvent::SessionStatus { .. } => "session_status",
            AgentEvent::RemoteControl { .. } => "remote_control",
            AgentEvent::ToolSummary { .. } => "tool_summary",
            AgentEvent::SubagentFinished { .. } => "subagent_finished",
            AgentEvent::SubagentInfo { .. } => "subagent_info",
            AgentEvent::TurnTokens { .. } => "turn_tokens",
            AgentEvent::ActivityLine { .. } => "activity_line",
            AgentEvent::AgentMessage { .. } => "agent_message",
            AgentEvent::Schedule { .. } => "schedule",
            AgentEvent::Goal { .. } => "goal",
            AgentEvent::TurnCause { .. } => "turn_cause",
        }
    }
    let covered: std::collections::BTreeSet<&str> =
        events.iter().map(|(_, _, ev)| tag(ev)).collect();
    let cases: Vec<Case> = events
        .iter()
        .enumerate()
        .map(|(i, (name, note, ev))| {
            let wire = serde_json::to_value(ev).expect("event serializes");
            // The serde tag is the match arm's tag: the two can't drift.
            assert_eq!(wire["type"], json!(tag(ev)), "{name}");
            // Every event round-trips through its own deserializer (what the
            // journal replays), so the fixture is also what replay yields.
            assert_eq!(
                &serde_json::from_value::<AgentEvent>(wire.clone()).unwrap(),
                ev,
                "{name}"
            );
            case(
                name,
                note,
                serde_json::to_value(SeqEvent {
                    seq: i as u64 + 1,
                    ts: 1_700_000_000_000 + i as u64,
                    ev: ev.clone(),
                })
                .unwrap(),
            )
        })
        .collect();
    assert_eq!(
        covered.len(),
        58,
        "every AgentEvent variant has at least one case"
    );
    pin(
        "chat-events",
        "crates/chimaera-agent/tests/wire_contract.rs",
        "SeqEvent envelopes ({seq, ts, ev}) for every AgentEvent variant, as /ws/chat `ev` and `batch` frames carry them and the journal replays them.",
        cases,
    );
}
