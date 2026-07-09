use super::support::*;

#[tokio::test]
async fn mcp_handshake_auth_and_tool_listing() {
    let state = test_state();
    let id = inject_agent(&state, "mk");

    // Wrong key is a 403; unknown agent a 404.
    let init = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18"},
    });
    let (status, _) = mcp_post(&state, &id, "wrong", init.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = mcp_post(&state, "s-00000000", "mk", init.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Initialize echoes the protocol version and carries instructions.
    let (status, out) = mcp_post(&state, &id, "mk", init).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(out["result"]["serverInfo"]["name"], "chimaera");
    assert!(out["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("@term:"));

    // Notifications (no id) are 202-acknowledged.
    let (status, _) = mcp_post(
        &state,
        &id,
        "mk",
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // tools/list names the three linked-terminal tools.
    let (status, out) = mcp_post(
        &state,
        &id,
        "mk",
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = out["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["list_terminals", "run_in_terminal", "read_terminal"]
    );

    state.sessions.kill(&id).ok();
}

/// The full agent-side story over MCP: unlinked -> helpful error;
/// linked -> list, exec (by display name), and journal read all work
/// and stay scoped.
#[tokio::test]
async fn mcp_tools_scoped_to_links_and_exec_round_trip() {
    let state = test_state();
    let agent = inject_agent(&state, "mk");
    let shell = spawn_integrated_bash(&state, "mcp-shell").await;

    // Unlinked: every tool refuses with linking guidance.
    let (is_error, text) = mcp_tool_call(
        &state,
        &agent,
        "mk",
        "run_in_terminal",
        serde_json::json!({"terminal": shell, "command": "echo hi"}),
    )
    .await;
    assert!(is_error, "{text}");
    assert!(text.contains("no terminals are linked"), "{text}");

    // Link, then exec by session id.
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/links",
        Some(serde_json::json!({"terminal_id": shell, "agent_id": agent})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (is_error, text) = mcp_tool_call(
        &state,
        &agent,
        "mk",
        "run_in_terminal",
        serde_json::json!({"terminal": shell, "command": "echo mcp-ran && false"}),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(text.starts_with("exit 1"), "{text}");
    assert!(text.contains("integrated mode"), "{text}");
    assert!(text.contains("mcp-ran"), "{text}");

    // list_terminals shows the linked shell with its last command.
    let (is_error, text) = mcp_tool_call(
        &state,
        &agent,
        "mk",
        "list_terminals",
        serde_json::json!({}),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(text.contains(&shell), "{text}");
    assert!(text.contains("echo mcp-ran && false"), "{text}");

    // read_terminal returns the journal with agent attribution upstream.
    let (is_error, text) = mcp_tool_call(
        &state,
        &agent,
        "mk",
        "read_terminal",
        serde_json::json!({"terminal": shell, "commands": 3}),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(text.contains("phase: ready"), "{text}");
    assert!(text.contains("echo mcp-ran && false"), "{text}");
    assert!(text.contains("exit 1"), "{text}");

    // Screen mode reads the visible grid.
    let (is_error, text) = mcp_tool_call(
        &state,
        &agent,
        "mk",
        "read_terminal",
        serde_json::json!({"terminal": shell, "screen": true}),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(text.contains("mcp-ran"), "{text}");

    // A second, unlinked shell stays out of reach — scope is the links.
    let other = spawn_integrated_bash(&state, "mcp-other").await;
    let (is_error, text) = mcp_tool_call(
        &state,
        &agent,
        "mk",
        "run_in_terminal",
        serde_json::json!({"terminal": other, "command": "echo nope"}),
    )
    .await;
    assert!(is_error, "{text}");

    state.sessions.kill(&agent).ok();
    state.sessions.kill(&shell).ok();
    state.sessions.kill(&other).ok();
}
