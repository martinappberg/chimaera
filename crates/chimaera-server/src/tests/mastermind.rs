//! The workspace Mastermind (v1): the PUT/DELETE binding routes, the wire
//! flag, and the MCP tier ("read for all, act for one" — v1 scopes both new
//! tiers to the one bound Mastermind). Route-level where a spawn is needed,
//! the scripted `write_fake_claude` stands in for the real CLI.

use super::support::*;
use crate::*;

/// PUT the mastermind and return (status, body).
async fn put_mastermind(
    state: &Arc<AppState>,
    ws: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    request(
        state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/mastermind"),
        Some(body),
    )
    .await
}

/// The workspace entry from GET /workspaces.
async fn workspace_entry(state: &Arc<AppState>, ws: &str) -> serde_json::Value {
    let (status, list) = request(state, Method::GET, "/api/v1/workspaces", None).await;
    assert_eq!(status, StatusCode::OK);
    list.as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == ws)
        .cloned()
        .unwrap_or_else(|| panic!("workspace {ws} not listed in {list}"))
}

/// Poll GET /sessions until `id` disappears (chat teardown reaps async).
async fn wait_session_gone(state: &Arc<AppState>, id: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (status, list) = request(state, Method::GET, "/api/v1/sessions", None).await;
        assert_eq!(status, StatusCode::OK);
        if !list.as_array().unwrap().iter().any(|s| s["id"] == id) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session {id} never left the roster: {list}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn put_mastermind_validates_and_rolls_back() {
    let state = test_state();
    let ws = make_workspace(&state, "mm-validate").await;

    // Unknown workspace.
    let (status, _) = put_mastermind(
        &state,
        "w-00000000",
        serde_json::json!({"agent": "claude", "mode": "ask"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Bad mode / bad model / unknown agent.
    for (body, needle) in [
        (
            serde_json::json!({"agent": "claude", "mode": "yolo"}),
            "invalid mode",
        ),
        (
            serde_json::json!({"agent": "claude", "mode": "ask", "model": "--evil"}),
            "invalid model",
        ),
        (
            serde_json::json!({"agent": "clippy", "mode": "ask"}),
            "unknown agent",
        ),
    ] {
        let (status, err) = put_mastermind(&state, &ws, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
        assert!(
            err["error"].as_str().unwrap().contains(needle),
            "{err} missing {needle:?}"
        );
    }

    // Codex is ACCEPTED (its MCP approval config enforces the mode — the
    // per-tool prompt is an elicitation): with no codex binary preset the
    // request clears validation and fails at the spawn with an honest 409,
    // never the old 400 refusal.
    preset_agent(
        &state,
        agents::AgentKind::Codex,
        Err("codex not found".to_string()),
        None,
    );
    let (status, err) = put_mastermind(
        &state,
        &ws,
        serde_json::json!({"agent": "codex", "mode": "ask"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert!(workspace_entry(&state, &ws).await["mastermind"].is_null());

    // Agents without a chat driver are refused with an explanation.
    let (status, err) = put_mastermind(
        &state,
        &ws,
        serde_json::json!({"agent": "gemini", "mode": "ask"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("chat driver"),
        "{err} should explain the missing driver"
    );

    // No claude binary: 409, and the just-made binding is rolled back — a
    // binding without a session would be a ghost dock.
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Err("claude not found".to_string()),
        None,
    );
    let (status, err) = put_mastermind(
        &state,
        &ws,
        serde_json::json!({"agent": "claude", "mode": "ask"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert!(workspace_entry(&state, &ws).await["mastermind"].is_null());

    // DELETE with nothing bound is a 404.
    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/workspaces/{ws}/mastermind"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The full lifecycle against a scripted claude: PUT creates-and-binds (ask
/// settings, wire flag), re-PUT retires the old session (no Recents leak)
/// and rebinds with auto settings, DELETE unbinds and kills.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_mastermind_creates_binds_reputs_and_deletes() {
    let state = test_state();
    let ws = make_workspace(&state, "mm-lifecycle").await;
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude("mm-fake")),
        Some("9.9.9-fake"),
    );

    // PUT (ask): the response is the new session row.
    let (status, row) = put_mastermind(
        &state,
        &ws,
        serde_json::json!({"agent": "claude", "mode": "ask"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    let first = row["id"].as_str().unwrap().to_string();
    assert_eq!(row["mastermind"], serde_json::json!(true), "{row}");
    assert_eq!(row["ui"], "chat");
    assert_eq!(row["agent_kind"], "claude");
    assert_eq!(row["alive"], serde_json::json!(true));

    // Bound and persisted (the wire carries it to the dock).
    let entry = workspace_entry(&state, &ws).await;
    assert_eq!(entry["mastermind"]["session_id"], serde_json::json!(first));
    assert_eq!(entry["mastermind"]["mode"], "ask");

    // The generated settings pre-allow ONLY the read tools in ask mode.
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(agents::settings_path(&first)).unwrap())
            .unwrap();
    let allow = settings["permissions"]["allow"].as_array().unwrap();
    assert!(allow.contains(&serde_json::json!("mcp__chimaera__workspace_status")));
    assert!(!allow.contains(&serde_json::json!("mcp__chimaera")));

    // The roster row carries the flag too (both builders).
    assert_eq!(
        session_entry(&state, &first).await["mastermind"],
        serde_json::json!(true)
    );

    // Re-PUT (auto): a NEW session, the old one retired — and never into
    // Recents (the Mastermind is the observer, not a roster conversation).
    let (status, row) = put_mastermind(
        &state,
        &ws,
        serde_json::json!({"agent": "claude", "mode": "auto"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    let second = row["id"].as_str().unwrap().to_string();
    assert_ne!(second, first, "a mode change re-creates the session");
    let entry = workspace_entry(&state, &ws).await;
    assert_eq!(entry["mastermind"]["session_id"], serde_json::json!(second));
    assert_eq!(entry["mastermind"]["mode"], "auto");
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(agents::settings_path(&second)).unwrap())
            .unwrap();
    assert_eq!(
        settings["permissions"]["allow"],
        serde_json::json!(["mcp__chimaera"])
    );
    wait_session_gone(&state, &first).await;
    assert!(
        recents_of(&state, &ws).await.is_empty(),
        "a retired mastermind must not land in Recents"
    );

    // DELETE: unbound, session killed.
    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/workspaces/{ws}/mastermind"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(workspace_entry(&state, &ws).await["mastermind"].is_null());
    wait_session_gone(&state, &second).await;
    assert!(recents_of(&state, &ws).await.is_empty());
}

/// Bind a planted PTY agent session as `ws`'s Mastermind (no driver needed —
/// the MCP tier keys on the binding, not on how the session runs).
fn bind_as_mastermind(state: &Arc<AppState>, ws: &str, sid: &str) {
    lock(&state.session_workspaces).insert(sid.to_string(), ws.to_string());
    lock(&state.workspaces).set_mastermind(
        ws,
        Some(workspaces::MastermindCfg {
            session_id: sid.to_string(),
            mode: workspaces::MastermindMode::Ask,
        }),
    );
}

async fn tool_names(state: &Arc<AppState>, sid: &str, key: &str) -> Vec<String> {
    let (status, out) = mcp_post(
        state,
        sid,
        key,
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    out["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

/// The tier in both directions: the Mastermind sees (and can call) the
/// workspace tools; a sibling worker neither sees them nor gets past the
/// call gate; both keep the base linked-terminal tools.
#[tokio::test]
async fn mcp_tier_gates_on_the_binding() {
    let state = test_state();
    let ws = make_workspace(&state, "mm-tier").await;
    let mastermind = inject_agent(&state, "mmk");
    let worker = inject_agent(&state, "wk");
    lock(&state.session_workspaces).insert(worker.clone(), ws.clone());
    bind_as_mastermind(&state, &ws, &mastermind);

    let mm_tools = tool_names(&state, &mastermind, "mmk").await;
    for tool in [
        "list_terminals",
        "run_in_terminal",
        "read_terminal",
        "workspace_status",
        "read_session",
        "list_changed_files",
        "spawn_agent",
        "spawn_terminal",
        "message_agent",
        "interrupt_agent",
    ] {
        assert!(mm_tools.contains(&tool.to_string()), "{mm_tools:?}");
    }
    let worker_tools = tool_names(&state, &worker, "wk").await;
    assert_eq!(
        worker_tools,
        ["list_terminals", "run_in_terminal", "read_terminal"],
        "workers get the base tier only"
    );

    // The call gate matches the listing: a worker naming a mastermind tool
    // gets a JSON-RPC error that names the Mastermind.
    let (status, out) = mcp_post(
        &state,
        &worker,
        "wk",
        serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "workspace_status", "arguments": {}},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        out["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Mastermind"),
        "{out}"
    );

    // The Mastermind's own call goes through: a JSON digest naming the
    // worker (and never the Mastermind's own row).
    let (is_error, text) = mcp_tool_call(
        &state,
        &mastermind,
        "mmk",
        "workspace_status",
        serde_json::json!({}),
    )
    .await;
    assert!(!is_error, "{text}");
    let digest: serde_json::Value = serde_json::from_str(&text).unwrap();
    let ids: Vec<&str> = digest["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&worker.as_str()), "{digest}");
    assert!(!ids.contains(&mastermind.as_str()), "{digest}");

    // Unbind (fire the Mastermind): the tier drops on the very next call.
    lock(&state.workspaces).set_mastermind(&ws, None);
    let fired = tool_names(&state, &mastermind, "mmk").await;
    assert_eq!(
        fired,
        ["list_terminals", "run_in_terminal", "read_terminal"]
    );

    state.sessions.kill(&mastermind).ok();
    state.sessions.kill(&worker).ok();
}

/// read_session reads any same-workspace session's screen (agent TUIs
/// included — read-only is safe), and the workspace scope walls off
/// everything else.
#[tokio::test]
async fn read_session_scopes_to_the_workspace() {
    let state = test_state();
    let ws = make_workspace(&state, "mm-read").await;
    let other_ws = make_workspace(&state, "mm-read-other").await;
    let mastermind = inject_agent(&state, "mmk");
    bind_as_mastermind(&state, &ws, &mastermind);

    let shell = spawn_integrated_bash(&state, "mm-read-shell").await;
    lock(&state.session_workspaces).insert(shell.clone(), ws.clone());
    let outsider = inject_agent(&state, "ok");
    lock(&state.session_workspaces).insert(outsider.clone(), other_ws.clone());

    // Type something recognizable, then read the screen.
    let _ =
        crate::exec::run_exec(&state, &shell, "echo mm-sees-this".to_string(), None, None).await;
    let (is_error, text) = mcp_tool_call(
        &state,
        &mastermind,
        "mmk",
        "read_session",
        serde_json::json!({"session": shell}),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(text.contains("mm-sees-this"), "{text}");

    // Cross-workspace target: refused with guidance.
    let (is_error, text) = mcp_tool_call(
        &state,
        &mastermind,
        "mmk",
        "read_session",
        serde_json::json!({"session": outsider}),
    )
    .await;
    assert!(is_error, "{text}");
    assert!(text.contains("workspace_status"), "{text}");

    state.sessions.kill(&mastermind).ok();
    state.sessions.kill(&shell).ok();
    state.sessions.kill(&outsider).ok();
}

/// message_agent delivers through the chat command path — the journal gets
/// the same UserMessage stamp a /ws/chat Send produces, prefixed with the
/// Mastermind attribution — and the TUI wall holds (propose-only).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_agent_stamps_journal_and_walls_tuis() {
    let state = test_state();
    let ws = make_workspace(&state, "mm-msg").await;
    let mastermind = inject_agent(&state, "mmk");
    bind_as_mastermind(&state, &ws, &mastermind);

    // A scripted claude worker on the chat surface.
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude("mm-msg-fake")),
        Some("9.9.9-fake"),
    );
    let (status, worker) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws, "kind": "agent", "ui": "chat"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{worker}");
    let worker_id = worker["id"].as_str().unwrap().to_string();

    let (is_error, text) = mcp_tool_call(
        &state,
        &mastermind,
        "mmk",
        "message_agent",
        serde_json::json!({"session": worker_id, "text": "status check: report progress"}),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(text.contains("delivered"), "{text}");

    // The journal shows it as a normal user turn WITH the attribution line —
    // exactly what every attached UI replays.
    let journal = state.chat.journal_dir().join(format!("{worker_id}.jsonl"));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let content = std::fs::read_to_string(&journal).unwrap_or_default();
        if content.contains("[from the workspace Mastermind]")
            && content.contains("status check: report progress")
            && content.contains("user_message")
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "journal never stamped the mastermind send: {content}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // A TUI agent target is propose-only (the exec-409 wall).
    let tui = inject_agent(&state, "tk");
    lock(&state.session_workspaces).insert(tui.clone(), ws.clone());
    let (is_error, text) = mcp_tool_call(
        &state,
        &mastermind,
        "mmk",
        "message_agent",
        serde_json::json!({"session": tui, "text": "do the thing"}),
    )
    .await;
    assert!(is_error, "{text}");
    assert!(text.contains("never types into a TUI"), "{text}");

    // interrupt_agent hits the same wall for TUIs.
    let (is_error, text) = mcp_tool_call(
        &state,
        &mastermind,
        "mmk",
        "interrupt_agent",
        serde_json::json!({"session": tui}),
    )
    .await;
    assert!(is_error, "{text}");

    // And the Mastermind cannot message itself.
    let (is_error, text) = mcp_tool_call(
        &state,
        &mastermind,
        "mmk",
        "message_agent",
        serde_json::json!({"session": mastermind, "text": "hi me"}),
    )
    .await;
    assert!(is_error, "{text}");

    state.chat.kill(&worker_id);
    state.sessions.kill(&mastermind).ok();
    state.sessions.kill(&tui).ok();
}

/// spawn_terminal opens a real shell in the workspace; spawn_agent refuses
/// junk and spawns a scripted worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_tools_create_workspace_sessions() {
    let state = test_state();
    let ws = make_workspace(&state, "mm-spawn").await;
    let mastermind = inject_agent(&state, "mmk");
    bind_as_mastermind(&state, &ws, &mastermind);

    let (is_error, text) = mcp_tool_call(
        &state,
        &mastermind,
        "mmk",
        "spawn_terminal",
        serde_json::json!({"name": "mm shell"}),
    )
    .await;
    assert!(!is_error, "{text}");
    let sid = text
        .split('[')
        .nth(1)
        .and_then(|s| s.split(']').next())
        .expect("spawned terminal id in the answer")
        .to_string();
    let row = session_entry(&state, &sid).await;
    assert_eq!(row["workspace_id"], serde_json::json!(ws));
    assert_eq!(row["kind"], "shell");

    // Bad agent / flag-shaped model are model-facing errors, not spawns.
    let (is_error, text) = mcp_tool_call(
        &state,
        &mastermind,
        "mmk",
        "spawn_agent",
        serde_json::json!({"agent": "gemini"}),
    )
    .await;
    assert!(is_error, "{text}");
    let (is_error, text) = mcp_tool_call(
        &state,
        &mastermind,
        "mmk",
        "spawn_agent",
        serde_json::json!({"agent": "claude", "model": "--oops"}),
    )
    .await;
    assert!(is_error, "{text}");

    // A scripted claude worker spawns as a NORMAL chat session (not a
    // mastermind — its row carries no flag and the binding is untouched).
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude("mm-spawn-fake")),
        Some("9.9.9-fake"),
    );
    let (is_error, text) = mcp_tool_call(
        &state,
        &mastermind,
        "mmk",
        "spawn_agent",
        serde_json::json!({"agent": "claude", "name": "worker one"}),
    )
    .await;
    assert!(!is_error, "{text}");
    let wid = text
        .split('[')
        .nth(1)
        .and_then(|s| s.split(']').next())
        .expect("spawned worker id in the answer")
        .to_string();
    let row = session_entry(&state, &wid).await;
    assert_eq!(row["ui"], "chat");
    assert_eq!(row["mastermind"], serde_json::Value::Null);
    assert_eq!(
        lock(&state.workspaces)
            .get(&ws)
            .unwrap()
            .mastermind
            .unwrap()
            .session_id,
        mastermind,
        "spawn_agent must never re-bind the mastermind"
    );

    state.chat.kill(&wid);
    state.sessions.kill(&sid).ok();
    state.sessions.kill(&mastermind).ok();
}

/// One Mastermind change per workspace at a time: the routes are multi-step
/// (retire → bind → spawn, with rollback), so a second caller mid-flight gets
/// a 409 instead of racing — and the guard releases on every exit path.
#[tokio::test]
async fn mastermind_changes_are_serialized_per_workspace() {
    let state = test_state();
    let ws = make_workspace(&state, "mm-race").await;
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude("mm-race-fake")),
        Some("9.9.9-fake"),
    );

    // Simulate an in-flight change (the guard is held across the PUT body).
    assert!(lock(&state.mastermind_switching).insert(ws.clone()));
    let (status, body) = put_mastermind(
        &state,
        &ws,
        serde_json::json!({"agent":"claude","mode":"ask"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().unwrap_or("").contains("in flight"),
        "{body}"
    );
    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/workspaces/{ws}/mastermind"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    lock(&state.mastermind_switching).remove(&ws);

    // Released: the same PUT now proceeds (and its own guard releases —
    // a follow-up DELETE reaches the 404-no-binding arm, not a 409).
    let (status, body) = put_mastermind(
        &state,
        &ws,
        serde_json::json!({"agent":"claude","mode":"ask"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mm = body["id"].as_str().unwrap().to_string();
    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/workspaces/{ws}/mastermind"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    wait_session_gone(&state, &mm).await;
}
