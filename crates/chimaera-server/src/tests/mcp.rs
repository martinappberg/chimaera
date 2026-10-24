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
    // A terminal session has no plan card to draw: no paragraph asks for one.
    let text = out["result"]["instructions"].as_str().unwrap();
    assert!(!text.contains("Plan card:"), "{text}");

    // Notifications (no id) are 202-acknowledged.
    let (status, _) = mcp_post(
        &state,
        &id,
        "mk",
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // tools/list names the three linked-terminal tools, the two document
    // tools, notify, open_browser, and agent communication's four — and no
    // update_plan for a terminal session.
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
        vec![
            "list_terminals",
            "run_in_terminal",
            "read_terminal",
            "document_guide",
            "check_document",
            "notify",
            "open_browser",
            "workspace_agents",
            "read_agent",
            "message_agent",
            "read_messages",
        ]
    );

    // MCP annotations: the read-only tools say so (claude 2.1.307+ retries a
    // call cut off by a closed connection only for read-only or idempotent
    // tools); read_messages settles what it shows, so it must never be
    // retried; a command run in a terminal carries the defaults.
    let annotations = |name: &str| {
        out["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .map(|t| t["annotations"].clone())
            .unwrap()
    };
    for name in [
        "list_terminals",
        "read_terminal",
        "document_guide",
        "check_document",
        "workspace_agents",
        "read_agent",
    ] {
        assert_eq!(
            annotations(name),
            serde_json::json!({"readOnlyHint": true}),
            "{name}"
        );
    }
    assert_eq!(
        annotations("read_messages"),
        serde_json::json!({"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false})
    );
    for name in ["run_in_terminal", "notify", "open_browser", "message_agent"] {
        assert!(annotations(name).is_null(), "{name}");
    }

    state.sessions.kill(&id).ok();
}

/// Every tool an ask-mode Mastermind may call unprompted is read-only, save
/// read_messages (it settles the messages it shows).
#[test]
fn the_mastermind_read_tools_are_annotated_read_only() {
    for name in crate::mcp::MASTERMIND_READ_TOOLS {
        assert_eq!(
            crate::mcp::READ_ONLY_TOOLS.contains(&name),
            name != "read_messages",
            "{name}"
        );
    }
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

/// `update_plan` draws the plan card: a chat session's call journals one
/// `plan` level-set (1-based ids, the three statuses), a malformed call is
/// refused with the reason, and a terminal session (no chat journal) is told
/// to keep its plan in the reply instead.
#[tokio::test]
async fn update_plan_journals_a_plan_level_set_for_chats_only() {
    let state = test_state();
    let root = test_dir("mcp-plan-ws");
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert!(status.is_success(), "{ws}");
    let ws = ws["id"].as_str().unwrap().to_string();
    preset_agent(
        &state,
        crate::agents::AgentKind::Claude,
        Ok(write_fake_claude("mcp-plan-fake")),
        Some("9.9.9-fake"),
    );
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws, "kind": "agent", "ui": "chat"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sid = body["id"].as_str().unwrap().to_string();
    let key = crate::lock(&state.agents).get(&sid).unwrap().key.clone();

    // A Claude chat is the one session offered the tool, and asked for the
    // card in its words.
    let (status, out) = mcp_post(
        &state,
        &sid,
        &key,
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let text = out["result"]["instructions"].as_str().unwrap();
    assert!(
        text.contains("Plan card:")
            && text.contains("update_plan TOOL from the chimaera MCP server"),
        "{text}"
    );
    let (_, out) = mcp_post(
        &state,
        &sid,
        &key,
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    assert!(
        out["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "update_plan"),
        "{out}"
    );

    let (is_error, text) = mcp_tool_call(
        &state,
        &sid,
        &key,
        "update_plan",
        serde_json::json!({"items": [
            {"content": "Read the config", "status": "completed"},
            {"content": "Run the tests", "status": "in_progress"},
            {"content": "Write the report", "status": "pending"},
        ]}),
    )
    .await;
    assert!(!is_error, "{text}");
    assert_eq!(text, "Plan shown (1/3 done).");
    let journal = state.chat.journal_dir().join(format!("{sid}.jsonl"));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let plan = loop {
        let content = std::fs::read_to_string(&journal).unwrap_or_default();
        if let Some(line) = content.lines().find(|l| l.contains("\"type\":\"plan\"")) {
            break serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no plan event: {content}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let entries = plan["ev"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["status"], "done");
    assert_eq!(entries[1]["status"], "in_progress");
    assert_eq!(entries[1]["content"], "Run the tests");
    assert_eq!(entries[2]["status"], "todo");
    assert_eq!(entries[2]["id"], "3");

    // Malformed: refused with the reason, nothing journaled.
    for (args, hint) in [
        (serde_json::json!({}), "needs `items`"),
        (serde_json::json!({"items": []}), "1 to 32 steps"),
        (
            serde_json::json!({"items": [{"content": "x", "status": "soon"}]}),
            "status",
        ),
        (
            serde_json::json!({"items": [{"content": "  ", "status": "pending"}]}),
            "no content",
        ),
    ] {
        let (is_error, text) = mcp_tool_call(&state, &sid, &key, "update_plan", args).await;
        assert!(is_error && text.contains(hint), "{text}");
    }
    state.sessions.kill(&sid).ok();

    // A terminal agent (hook key, no chat journal): told, not pretended.
    let tui = inject_agent(&state, "plan-key");
    let (is_error, text) = mcp_tool_call(
        &state,
        &tui,
        "plan-key",
        "update_plan",
        serde_json::json!({"items": [{"content": "a", "status": "pending"}]}),
    )
    .await;
    assert!(is_error && text.contains("terminal session"), "{text}");
}
