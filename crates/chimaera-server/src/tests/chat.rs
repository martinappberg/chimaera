use super::support::*;
use chimaera_agent::model::{AgentEvent, ToolKind, ToolStatus};

fn tool_call(kind: ToolKind, path: &str) -> AgentEvent {
    AgentEvent::ToolCall {
        id: "t".into(),
        kind,
        title: "tool".into(),
        locations: vec![path.to_string()],
        status: ToolStatus::Completed,
    }
}

/// A chat agent's `Edit` tool call must bump the workspace git epoch — the same
/// live-refresh nudge the TUI path gets from its PostToolUse hook. codex chat
/// has no such hook (and claude's stream-json hook can misfire), so this
/// protocol event is the reliable trigger that lets a preview you have open
/// refresh the moment the agent writes the file. A non-Edit tool call must NOT
/// bump the epoch (a Read touches no disk).
#[tokio::test]
async fn chat_edit_event_bumps_the_git_epoch() {
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

    // A Read tool call touches no disk → no nudge.
    let before = epoch();
    crate::chat::nudge_edited_paths(&state, &tool_call(ToolKind::Read, &edited)).await;
    assert_eq!(epoch(), before, "a Read tool call must not bump the epoch");

    // The Edit tool call bumps the epoch for the touched in-workspace path, so
    // the client is nudged to re-probe its open previews' mtime.
    crate::chat::nudge_edited_paths(&state, &tool_call(ToolKind::Edit, &edited)).await;
    assert!(
        epoch() > before,
        "an Edit tool call must bump the workspace git epoch (before={before}, after={})",
        epoch()
    );
}
