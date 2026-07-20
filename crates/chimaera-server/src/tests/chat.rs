use std::collections::HashMap;

use super::support::*;
use chimaera_agent::model::{AgentEvent, ToolKind, ToolStatus};

fn edit_call(id: &str, path: &str, status: ToolStatus) -> AgentEvent {
    AgentEvent::ToolCall {
        id: id.into(),
        kind: ToolKind::Edit,
        title: "edit".into(),
        locations: vec![path.to_string()],
        status,
        cross_turn: false,
    }
}

fn tool_update(id: &str, status: ToolStatus) -> AgentEvent {
    AgentEvent::ToolCallUpdate {
        id: id.into(),
        status,
        content: None,
    }
}

/// A chat agent's `Edit` must bump the workspace git epoch — the same
/// live-refresh nudge the TUI gets from its PostToolUse hook (codex chat has no
/// such hook and claude's stream-json hook can misfire). The subtlety this test
/// pins: the bump must fire on the WRITE's COMPLETION, not on the edit's
/// announcement. Both drivers emit the Edit `ToolCall` with `InProgress` BEFORE
/// the file is written (often behind an approval prompt); the write's
/// completion arrives as a `ToolCallUpdate` with no `locations`. Nudging on the
/// announcement would re-probe an unchanged mtime and then miss the real write.
#[tokio::test]
async fn chat_edit_nudges_on_write_completion_not_announcement() {
    let root = test_dir("chat-edit-epoch");
    std::fs::write(root.join("file.rs"), "x").unwrap();

    let state = test_state();
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({ "root": root.to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ws_id = ws["id"].as_str().unwrap().to_string();
    let canonical_root = std::path::PathBuf::from(ws["root"].as_str().unwrap());
    let edited = canonical_root
        .join("file.rs")
        .to_string_lossy()
        .into_owned();

    let epoch = || {
        state
            .git
            .epochs_snapshot()
            .get(&ws_id)
            .copied()
            .unwrap_or(0)
    };

    let mut pending = HashMap::new();
    let sid = "sess-1";
    let before = epoch();

    // A Read tool call touches no disk → never nudges.
    crate::chat::nudge_on_edit(
        &state,
        &mut pending,
        sid,
        &AgentEvent::ToolCall {
            id: "r".into(),
            kind: ToolKind::Read,
            title: "read".into(),
            locations: vec![edited.clone()],
            status: ToolStatus::Completed,
            cross_turn: false,
        },
    )
    .await;
    assert_eq!(epoch(), before, "a Read tool call must not bump the epoch");

    // The Edit ANNOUNCEMENT (InProgress) must NOT bump yet — nothing is written.
    crate::chat::nudge_on_edit(
        &state,
        &mut pending,
        sid,
        &edit_call("e1", &edited, ToolStatus::InProgress),
    )
    .await;
    assert_eq!(
        epoch(),
        before,
        "an announced-but-unwritten edit must not bump the epoch"
    );

    // The write's COMPLETION (a ToolCallUpdate carrying no locations) nudges the
    // remembered path, so the client re-probes its open preview's mtime.
    crate::chat::nudge_on_edit(
        &state,
        &mut pending,
        sid,
        &tool_update("e1", ToolStatus::Completed),
    )
    .await;
    assert!(
        epoch() > before,
        "the edit's completion must bump the workspace git epoch (before={before}, after={})",
        epoch()
    );
}
