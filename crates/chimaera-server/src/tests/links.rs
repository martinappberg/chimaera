use super::support::*;
use crate::*;

#[tokio::test]
async fn links_lifecycle_validation_and_move() {
    let state = test_state();
    let agent_a = inject_agent(&state, "ka");
    let agent_b = inject_agent(&state, "kb");
    let shell = {
        let info = state
            .sessions
            .spawn(chimaera_pty::SpawnOpts {
                cwd: test_dir("links-shell"),
                name: None,
                cols: 80,
                rows: 24,
                command: None,
                id: None,
                env: Vec::new(),
                env_remove: Vec::new(),
                scrollback: None,
            })
            .expect("spawn shell");
        info.id
    };

    // Link shell -> agent A.
    let (status, out) = request(
        &state,
        Method::PUT,
        "/api/v1/links",
        Some(serde_json::json!({"terminal_id": shell, "agent_id": agent_a})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["moved_from"], serde_json::Value::Null);

    let (_, list) = request(&state, Method::GET, "/api/v1/links", None).await;
    assert_eq!(
        list,
        serde_json::json!([{"terminal_id": shell, "agent_id": agent_a}])
    );

    // Re-linking to agent B MOVES the leash (one agent per terminal).
    let (status, out) = request(
        &state,
        Method::PUT,
        "/api/v1/links",
        Some(serde_json::json!({"terminal_id": shell, "agent_id": agent_b})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["moved_from"], agent_a, "{out}");
    assert_eq!(links::terminals_of(&state, &agent_b), vec![shell.clone()]);
    assert!(links::terminals_of(&state, &agent_a).is_empty());

    // A shell can't play agent; an agent can't play terminal.
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/links",
        Some(serde_json::json!({"terminal_id": shell, "agent_id": shell})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/links",
        Some(serde_json::json!({"terminal_id": agent_a, "agent_id": agent_b})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Unknown sessions are 404s.
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/links",
        Some(serde_json::json!({"terminal_id": "s-00000000", "agent_id": agent_a})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Unlink is idempotent.
    for _ in 0..2 {
        let (status, _) = request(
            &state,
            Method::DELETE,
            &format!("/api/v1/links/{shell}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (_, list) = request(&state, Method::GET, "/api/v1/links", None).await;
    assert_eq!(list, serde_json::json!([]));

    // A link dies with its terminal session (pruned on read).
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/links",
        Some(serde_json::json!({"terminal_id": shell, "agent_id": agent_a})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    state.sessions.kill(&shell).ok();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (_, list) = request(&state, Method::GET, "/api/v1/links", None).await;
        if list == serde_json::json!([]) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "link survived its dead terminal: {list}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    state.sessions.kill(&agent_a).ok();
    state.sessions.kill(&agent_b).ok();
}

/// `@term:` mentions in a user prompt auto-link (mention = consent) and
/// the hook response tells the agent via additionalContext.
#[tokio::test]
async fn user_prompt_mention_autolinks_terminal() {
    let state = test_state();
    let agent = inject_agent(&state, "mk");
    let root = test_dir("mention-root");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let root = std::fs::canonicalize(&root).unwrap();
    let shell = inject_shell(&state, &root, ws["id"].as_str().unwrap());
    wait_display_name(&state, &shell, "bash").await;

    // Mention by display name links it and reports back as context.
    let (status, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/agent-events/{agent}?key=mk"),
        Some(serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": "run squeue in @term:bash please",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let context = out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("");
    assert!(context.contains("Linked terminal 'bash'"), "{out}");
    let (_, list) = request(&state, Method::GET, "/api/v1/links", None).await;
    assert_eq!(
        list,
        serde_json::json!([{"terminal_id": shell, "agent_id": agent}])
    );

    // A repeated mention of an already-linked terminal stays silent.
    let (_, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/agent-events/{agent}?key=mk"),
        Some(serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": "again in @term:bash",
        })),
    )
    .await;
    assert_eq!(out["hookSpecificOutput"], serde_json::Value::Null, "{out}");

    // Unknown mentions surface as context too (the agent should know).
    let (_, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/agent-events/{agent}?key=mk"),
        Some(serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": "and @term:doesnotexist",
        })),
    )
    .await;
    let context = out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("");
    assert!(context.contains("no terminal 'doesnotexist'"), "{out}");

    state.sessions.kill(&agent).ok();
    state.sessions.kill(&shell).ok();
}
