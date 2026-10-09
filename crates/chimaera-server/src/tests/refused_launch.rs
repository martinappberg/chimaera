//! A launch the workspace policy refuses after the session was registered
//! leaves nothing behind: no agent record (so no live MCP/hook key), no
//! workspace binding, no chat recipe.
use super::support::*;
use crate::*;

fn refusing_state() -> Arc<AppState> {
    let state = test_state();
    use_test_policy(
        &state,
        TestPolicy {
            refuse_hold: true,
            ..Default::default()
        },
    );
    state
}

fn mcp_init() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18"},
    })
}

#[tokio::test]
async fn refused_terminal_spawn_forgets_its_agent_record() {
    let state = refusing_state();
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(PathBuf::from("/bin/echo")),
        None,
    );
    let ws = make_workspace(&state, "refused-tui").await;
    let workspace = lock(&state.workspaces).get(&ws).unwrap();
    let spec = spawn::SpawnSpec {
        native_cwd: None,
        fork_head: false,
        workspace,
        id: Some("s-refused-tui".into()),
        name: None,
        cwd: None,
        cols: Some(80),
        rows: Some(24),
        theme: "dark".into(),
        title_hint: None,
        prelude: None,
        kind: spawn::SpawnKind::Agent {
            kind: agents::AgentKind::Claude,
            model: None,
            resume: None,
        },
        started_by: history::StartedBy::You,
    };
    let Err(spawn::SpawnFailure::Internal(error)) = spawn::spawn_session(&state, spec).await else {
        panic!("the launch must be refused");
    };
    assert!(
        error.to_string().contains("refused by the test policy"),
        "{error}"
    );
    assert!(lock(&state.agents).is_empty(), "the record is gone");
    assert!(!lock(&state.session_workspaces).contains_key("s-refused-tui"));
    assert!(state.sessions.get("s-refused-tui").is_none());
    let (status, _) = mcp_post(&state, "s-refused-tui", "any", mcp_init()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no key authenticates");
}

#[tokio::test]
async fn refused_chat_spawn_forgets_its_recipe() {
    let state = refusing_state();
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude("refused-chat")),
        Some("9.9.9-fake"),
    );
    let ws = make_workspace(&state, "refused-chat").await;
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws, "kind": "agent", "ui": "chat"})),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert!(
        body.to_string().contains("refused by the test policy"),
        "{body}"
    );
    assert!(lock(&state.agents).is_empty(), "the record is gone");
    assert!(lock(&state.chat_recipes).is_empty(), "the recipe is gone");
    assert!(lock(&state.session_workspaces).is_empty());
}
