use super::support::*;
use crate::*;

/// The degrade contract end-to-end minus a real agent: a chat driver
/// whose handshake cannot complete (cat echoes our initialize request
/// back) must be respawned as a PTY session under the SAME id, with the
/// AgentRecord intact, via the signal task.
#[tokio::test]
async fn chat_handshake_failure_degrades_to_pty_on_same_id() {
    use std::os::unix::fs::PermissionsExt;

    let state = test_state();
    chat::spawn_signal_task(state.clone());

    let dir = test_dir("chat-degrade");
    let tui = dir.join("fake-tui.sh");
    std::fs::write(&tui, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&tui, std::fs::Permissions::from_mode(0o755)).unwrap();
    let settings = dir.join("settings.json");
    std::fs::write(&settings, "{}").unwrap();
    let mcp = dir.join("mcp.json");
    std::fs::write(&mcp, "{}").unwrap();

    let id = "s-degrade".to_string();
    crate::lock(&state.agents).insert(
        id.clone(),
        agents::AgentRecord::new("key".into(), agents::AgentKind::Claude),
    );
    crate::lock(&state.chat_recipes).insert(
        id.clone(),
        chat::ChatRecipe {
            workspace_root: dir.clone(),
            workspace_id: "w-test".into(),
            kind: agents::AgentKind::Claude,
            bin: tui.clone(),
            version: None,
            settings: Some(settings),
            mcp_config: Some(mcp),
            model: None,
            resume: None,
            fork_at: None,
            rollback_turns: None,
            theme: "dark".into(),
            prelude: None,
            mastermind: None,
            created_at_ms: None,
        },
    );

    let mut spec = chimaera_agent::driver::SpawnSpec::new(
        id.clone(),
        vec!["/bin/cat".to_string()],
        dir.clone(),
    );
    spec.handshake_timeout = std::time::Duration::from_millis(300);
    state
        .chat
        .spawn(&chimaera_agent::claude::ClaudeAdapter, spec)
        .unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while state.sessions.get(&id).is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "degrade never respawned a PTY session"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(!state.chat.contains(&id), "chat registry slot freed");
    assert!(
        crate::lock(&state.agents).contains_key(&id),
        "agent record survives the degrade"
    );

    // The journal must tell the story a reattach replays: the startup failure
    // (fatal Error, from the driver harness), then the degrade stamped as a
    // ModeSwitch(term) — not a bare fatal tail. The stamp lands right after
    // the PTY registers, so poll briefly.
    let journal = state.chat.journal_dir().join(format!("{id}.jsonl"));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let content = std::fs::read_to_string(&journal).unwrap_or_default();
        if content.contains(r#""type":"mode_switch""#) {
            assert!(
                content.contains(r#""fatal":true"#) && content.contains("failed to start"),
                "startup failure must be journaled before the switch: {content}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "degrade never journaled a mode_switch; journal: {}",
            std::fs::read_to_string(&journal).unwrap_or_default()
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // The degrade-in-progress marker (the WS "degraded" classification) is
    // cleaned up once the successor exists — the removal is async relative to
    // the mode_switch stamp above, so poll rather than race it.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !crate::lock(&state.chat_switching).is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "degrade marker must not outlive the degrade"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let _ = state.sessions.kill(&id);
}

/// A handshake failure with NO respawn recipe must not silently retire the
/// session: it stays registered (dead) with the record Errored — the same
/// keep-visible contract as ProtocolError — so the rail shows the failure
/// and the journaled diagnostic is reachable.
#[tokio::test]
async fn chat_handshake_failure_without_recipe_stays_visible_errored() {
    let state = test_state();
    chat::spawn_signal_task(state.clone());

    let dir = test_dir("chat-norecipe");
    let id = "s-norecipe".to_string();
    crate::lock(&state.agents).insert(
        id.clone(),
        agents::AgentRecord::new("key".into(), agents::AgentKind::Claude),
    );

    let mut spec = chimaera_agent::driver::SpawnSpec::new(
        id.clone(),
        vec!["/bin/cat".to_string()],
        dir.clone(),
    );
    spec.handshake_timeout = std::time::Duration::from_millis(300);
    state
        .chat
        .spawn(&chimaera_agent::claude::ClaudeAdapter, spec)
        .unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let errored = crate::lock(&state.agents)
            .get(&id)
            .is_some_and(|r| r.state == agent_state::AgentState::Errored);
        if errored {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "record never turned Errored"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let info = state.chat.get(&id).expect("session stays registered, dead");
    assert!(!info.alive);
    assert!(
        state.sessions.get(&id).is_none(),
        "no PTY successor without a recipe"
    );
}

/// An agent updated IN PLACE (same path, new binary) must be noticed by the
/// next spawn-time detect: the cached version is re-probed from the new
/// binary without a full re-resolution — the login shell is never consulted,
/// because the path still executes.
#[tokio::test]
async fn detect_reprobes_version_when_binary_changes_in_place() {
    use std::os::unix::fs::PermissionsExt;

    let state = test_state();
    let dir = test_dir("detect-reprobe");
    let bin = dir.join("claude");
    std::fs::write(&bin, "#!/bin/sh\necho '9.9.9 (Updated Code)'\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    // The cache as the daemon left it BEFORE the update: same path, the old
    // version, an mtime stamp that no longer matches the file.
    lock(&state.agent_bins).insert(
        agents::AgentKind::Claude,
        launcher::AgentDetection {
            path: Ok(bin.clone()),
            version: Some("2.1.204 (Claude Code)".into()),
            managed: false,
            explicit: false,
            mtime: Some(std::time::UNIX_EPOCH),
        },
    );

    let detection = launcher::detect(&state, agents::AgentKind::Claude, false).await;
    assert_eq!(detection.path.unwrap(), bin);
    assert_eq!(
        detection.version.as_deref(),
        Some("9.9.9 (Updated Code)"),
        "the in-place update's version must be re-probed"
    );
    // The refreshed entry (new version + current stamp) is what the cache
    // serves from now on.
    let cached = lock(&state.agent_bins)
        .get(&agents::AgentKind::Claude)
        .cloned()
        .unwrap();
    assert_eq!(cached.version.as_deref(), Some("9.9.9 (Updated Code)"));
    assert!(cached.mtime.is_some_and(|m| m != std::time::UNIX_EPOCH));
}

#[tokio::test]
async fn create_agent_without_claude_is_409_with_hint() {
    let state = test_state();
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Err("claude not found via login shell (test)".to_string()),
        None,
    );
    let root = test_dir("agent-409");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws["id"], "kind": "agent"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("claude not found"));
}

#[tokio::test]
async fn create_agent_spawns_command_with_generated_settings() {
    let state = test_state_with_port(45678);
    // A stand-in "claude": exits immediately, but exercises the whole
    // spawn path (settings generation, id pre-pick, record registration).
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(PathBuf::from("/bin/echo")),
        None,
    );
    let root = test_dir("agent-spawn");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws["id"], "kind": "agent"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session["kind"], "agent");
    assert_eq!(session["agent_kind"], "claude");
    assert_eq!(session["agent_state"], "unknown");
    assert_eq!(session["agent_title"], serde_json::Value::Null);
    let id = session["id"].as_str().unwrap().to_string();

    // The generated settings file wires every hook to this daemon+session.
    let settings_path = chimaera_core::runtime_dir()
        .join("agents")
        .join(format!("{id}-settings.json"));
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let key = lock(&state.agents)
        .get(&id)
        .map(|r| r.key.clone())
        .expect("agent record registered");
    let url = settings["hooks"]["SessionStart"][0]["hooks"][0]["url"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        url,
        format!("http://127.0.0.1:45678/api/v1/agent-events/{id}?key={key}")
    );
    std::fs::remove_file(&settings_path).ok();
}

#[tokio::test]
async fn agents_endpoint_lists_catalog_with_installed_and_missing() {
    let state = test_state();
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(PathBuf::from("/bin/echo")),
        Some("2.1.196 (Claude Code)"),
    );
    // Installed but ancient: the npm-era codex predates `codex login`.
    preset_agent(
        &state,
        agents::AgentKind::Codex,
        Ok(PathBuf::from("/bin/echo")),
        Some("0.1.2504161551"),
    );
    preset_agent(
        &state,
        agents::AgentKind::Gemini,
        Err("gemini not found (test)".to_string()),
        None,
    );
    preset_agent(
        &state,
        agents::AgentKind::Antigravity,
        Err("agy not found (test)".to_string()),
        None,
    );
    // Upstream latest knowledge (agent_updates): newer than claude's
    // install, and known for the uninstalled gemini too.
    for (kind, version) in [
        (agents::AgentKind::Claude, "2.1.207"),
        (agents::AgentKind::Gemini, "0.9.0"),
    ] {
        lock(&state.agent_updates).insert(
            kind,
            agent_updates::AgentLatest {
                version: version.to_string(),
                checked_at: 1_000,
            },
        );
    }

    let (status, list) = request(&state, Method::GET, "/api/v1/agents", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 4);
    let ids: Vec<&str> = list.iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["claude", "codex", "gemini", "agy"]);

    // Installed and current: path + version present, no outdated flag.
    // A known-newer upstream release marks it update_available.
    let claude = &list[0];
    assert_eq!(claude["name"], "Claude Code");
    assert_eq!(claude["installed"], true);
    assert_eq!(claude["path"], "/bin/echo");
    assert_eq!(claude["version"], "2.1.196 (Claude Code)");
    assert!(!claude.as_object().unwrap().contains_key("outdated"));
    assert_eq!(claude["latest_version"], "2.1.207");
    assert_eq!(claude["latest_checked_at"], 1_000);
    assert_eq!(claude["update_available"], true);
    assert!(claude["install"]["command"]
        .as_str()
        .unwrap()
        .starts_with("curl "));
    assert!(claude["install"]["url"]
        .as_str()
        .unwrap()
        .starts_with("https://"));

    // Installed but legacy (npm-era codex, no `codex login`): flagged so
    // the UI offers the install command as an update. No upstream check has
    // landed for it → no latest fields, and never a guessed update flag.
    let codex = &list[1];
    assert_eq!(codex["installed"], true);
    assert_eq!(codex["outdated"], true);
    assert_eq!(codex["install"]["command"], "npm install -g @openai/codex");
    assert!(!codex.as_object().unwrap().contains_key("latest_version"));
    assert!(!codex.as_object().unwrap().contains_key("update_available"));

    // Not installed but latest known: the info rides along (the row can say
    // what an install would get), never an update flag.
    let gemini = &list[2];
    assert_eq!(gemini["installed"], false);
    assert_eq!(gemini["latest_version"], "0.9.0");
    assert!(!gemini.as_object().unwrap().contains_key("update_available"));

    // Not installed: muted row material — no path/version, but the
    // install action and docs link are still there.
    let agy = &list[3];
    assert_eq!(agy["name"], "Antigravity CLI");
    assert_eq!(agy["installed"], false);
    let obj = agy.as_object().unwrap();
    assert!(!obj.contains_key("path"), "{agy}");
    assert!(!obj.contains_key("version"), "{agy}");
    assert!(agy["install"]["command"]
        .as_str()
        .unwrap()
        .contains("antigravity.google"));
    assert!(agy["install"]["url"]
        .as_str()
        .unwrap()
        .starts_with("https://"));
}

#[tokio::test]
async fn agent_launcher_endpoints_without_token_are_401() {
    for uri in [
        "/api/v1/agents",
        "/api/v1/agents?refresh=true",
        "/api/v1/agents/claude/sessions?workspace_id=w-x",
        "/api/v1/recents?workspace_id=w-x",
    ] {
        let res = app(test_state())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    // POST install too: it spawns processes, so auth is not optional.
    let res = app(test_state())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/agents/codex/install")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"workspace_id":"w-x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// POST /api/v1/agents/{id}/install: the pinned contract — 404 unknown
/// agent id, 404 unknown workspace, 400 for gemini (no phase-1 managed
/// install), and the session mechanics (ordinary kind-"shell" session
/// with streaming output, one install per agent = 409, watcher cleanup)
/// driven with a stub script so the test never hits the network.
#[tokio::test]
async fn install_endpoint_contract_and_session_mechanics() {
    let state = test_state();
    let ws_id = make_workspace(&state, "install-root").await;

    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/agents/nope/install",
        Some(serde_json::json!({"workspace_id": ws_id})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/agents/codex/install",
        Some(serde_json::json!({"workspace_id": "w-00000000"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/agents/gemini/install",
        Some(serde_json::json!({"workspace_id": ws_id})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("node runtime"),
        "{body}"
    );

    // Stubbed install session: streams like any pane, shows up as a
    // pinned-name shell in the workspace, and blocks a second install.
    let workspace = lock(&state.workspaces).get(&ws_id).unwrap();
    let sid = runtimes::start_install(
        &state,
        agents::AgentKind::Codex,
        &workspace,
        "install",
        "echo stub-install-output; sleep 30".to_string(),
    )
    .expect("stub install spawned");
    let entry = session_entry(&state, &sid).await;
    assert_eq!(entry["kind"], "shell");
    assert_eq!(entry["display_name"], "install codex");
    assert_eq!(entry["renamed"], true);
    assert_eq!(entry["workspace_id"].as_str().unwrap(), ws_id);

    // Installer output streams into the ordinary pane pipeline.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let att = state.sessions.attach(&sid).expect("attach install pane");
        if String::from_utf8_lossy(&att.snapshot).contains("stub-install-output") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "install output never reached the pane"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    // Same agent again while running: 409. Another agent: fine.
    let err = runtimes::start_install(
        &state,
        agents::AgentKind::Codex,
        &workspace,
        "install",
        "echo second".to_string(),
    )
    .expect_err("second install must conflict");
    assert_eq!(err.status(), StatusCode::CONFLICT);
    let other = runtimes::start_install(
        &state,
        agents::AgentKind::Antigravity,
        &workspace,
        "install",
        "echo other; sleep 30".to_string(),
    )
    .expect("other agent installs in parallel");

    // Kill the codex install; the watcher re-detects and clears the
    // slot, so a fresh install may start.
    state.sessions.kill(&sid).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while lock(&state.installs).contains_key(&agents::AgentKind::Codex) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "install watcher never cleared the slot"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let again = runtimes::start_install(
        &state,
        agents::AgentKind::Codex,
        &workspace,
        "install",
        "echo again; sleep 30".to_string(),
    )
    .expect("slot free after the session ended");
    state.sessions.kill(&again).ok();
    state.sessions.kill(&other).ok();
}

/// POST /api/v1/agents/{id}/update: managed-only. 404 unknown agent /
/// unknown workspace; 400 when nothing is installed; 400 when the resolved
/// binary is the user's own (chimaera never touches a binary it doesn't
/// own). Session mechanics ride the same `start_install` slot as installs —
/// driven with a stub script so the test never hits the network; the pane
/// name says "update", and a running update 409s BOTH endpoints.
#[tokio::test]
async fn update_endpoint_is_managed_only_and_shares_the_install_slot() {
    let state = test_state();
    let ws_id = make_workspace(&state, "update-root").await;

    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/agents/nope/update",
        Some(serde_json::json!({"workspace_id": ws_id})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // Nothing installed → 400 pointing at install.
    preset_agent(
        &state,
        agents::AgentKind::Codex,
        Err("not found".to_string()),
        None,
    );
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/agents/codex/update",
        Some(serde_json::json!({"workspace_id": ws_id})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("not installed"),
        "{body}"
    );

    // The user's own binary → 400 naming whose it is.
    preset_agent(
        &state,
        agents::AgentKind::Codex,
        Ok(PathBuf::from("/usr/local/bin/codex")),
        Some("codex-cli 0.144.1"),
    );
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/agents/codex/update",
        Some(serde_json::json!({"workspace_id": ws_id})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("your own install"),
        "{body}"
    );

    // A managed binary passes the gate; an unknown workspace still 404s.
    let managed_bin = runtimes::managed_bin_dir(&state.managed_root).join("codex");
    preset_agent(
        &state,
        agents::AgentKind::Codex,
        Ok(managed_bin),
        Some("codex-cli 0.144.1"),
    );
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/agents/codex/update",
        Some(serde_json::json!({"workspace_id": "w-00000000"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The session mechanics, stubbed (the endpoint's OK path would run the
    // real network-bound script; live verification covers it): the pane is
    // named for what it does, and the one-per-agent slot 409s both verbs.
    let workspace = lock(&state.workspaces).get(&ws_id).unwrap();
    let sid = runtimes::start_install(
        &state,
        agents::AgentKind::Codex,
        &workspace,
        "update",
        "echo stub-update; sleep 30".to_string(),
    )
    .expect("stub update spawned");
    let entry = session_entry(&state, &sid).await;
    assert_eq!(entry["kind"], "shell");
    assert_eq!(entry["display_name"], "update codex");
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/agents/codex/update",
        Some(serde_json::json!({"workspace_id": ws_id})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "update blocks update");
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/agents/codex/install",
        Some(serde_json::json!({"workspace_id": ws_id})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "update blocks install");
    state.sessions.kill(&sid).ok();
}

/// A same-agent POST racing the spawn window must 409, not overwrite:
/// the reservation is inserted before `SessionManager::spawn` registers
/// the session, so a fresh reservation with no visible session is still
/// busy; only one past the grace window is reclaimable.
#[tokio::test]
async fn install_reservation_blocks_the_spawn_registration_race() {
    let state = test_state();
    let ws_id = make_workspace(&state, "install-race").await;
    let workspace = lock(&state.workspaces).get(&ws_id).unwrap();

    // A fresh reservation whose session is not yet registered — the
    // exact in-spawn window.
    lock(&state.installs).insert(
        agents::AgentKind::Codex,
        ("s-notyet00".to_string(), std::time::Instant::now()),
    );
    let err = runtimes::start_install(
        &state,
        agents::AgentKind::Codex,
        &workspace,
        "install",
        "echo racer".to_string(),
    )
    .expect_err("a fresh reservation must read as busy");
    assert_eq!(err.status(), StatusCode::CONFLICT);

    // The same dead reservation past the grace window: stale, reclaimed.
    lock(&state.installs).insert(
        agents::AgentKind::Codex,
        (
            "s-notyet00".to_string(),
            std::time::Instant::now()
                - (runtimes::INSTALL_RESERVATION_GRACE + std::time::Duration::from_secs(1)),
        ),
    );
    let sid = runtimes::start_install(
        &state,
        agents::AgentKind::Codex,
        &workspace,
        "install",
        "echo reclaimed; sleep 30".to_string(),
    )
    .expect("a stale reservation is reclaimable");
    assert_eq!(
        lock(&state.installs)
            .get(&agents::AgentKind::Codex)
            .map(|(s, _)| s.clone()),
        Some(sid.clone()),
        "the reclaim installed its own reservation"
    );
    state.sessions.kill(&sid).ok();
}

/// The scheme theme fills the gap in the generated claude settings —
/// and steps aside when the user's own settings.json picks a theme.
#[tokio::test]
async fn claude_theme_fills_gap_in_generated_settings() {
    let user_settings_dir = test_dir("user-claude");
    let data = test_dir("data");
    let config = data.join("config");
    let mut state = AppState::new(
        "test-token".to_string(),
        "testhost".to_string(),
        4242,
        0,
        data,
        config,
    );
    state.claude_settings_path = user_settings_dir.join("settings.json");
    let state = Arc::new(state);
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(PathBuf::from("/bin/echo")),
        None,
    );
    let ws_id = make_workspace(&state, "theme-root").await;

    let generated_settings = |session: &serde_json::Value| -> serde_json::Value {
        let id = session["id"].as_str().unwrap();
        let path = chimaera_core::runtime_dir()
            .join("agents")
            .join(format!("{id}-settings.json"));
        let value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).ok();
        value
    };

    // No user theme anywhere: the client scheme lands in the settings.
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({
            "workspace_id": ws_id, "kind": "agent", "theme": "light",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let value = generated_settings(&session);
    assert_eq!(value["theme"], "light");
    assert_eq!(
        value["hooks"]["SessionStart"][0]["hooks"][0]["type"], "http",
        "the theme merges into the SAME file the hooks ride"
    );

    // No theme in the body: the default scheme is dark.
    let (_, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws_id, "kind": "agent"})),
    )
    .await;
    assert_eq!(generated_settings(&session)["theme"], "dark");

    // The user set a theme in their own settings.json: hands off.
    std::fs::write(
        &state.claude_settings_path,
        r#"{"theme": "dark", "tui": "fullscreen"}"#,
    )
    .unwrap();
    let (_, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({
            "workspace_id": ws_id, "kind": "agent", "theme": "light",
        })),
    )
    .await;
    assert!(
        generated_settings(&session).get("theme").is_none(),
        "an explicit user theme is never overridden"
    );
}

/// GET /api/v1/agents flags managed installs: `managed: true` when the
/// resolved binary lives under ~/.chimaera/agents, and
/// `managed_install` says whether POST install has a curated recipe.
#[tokio::test]
async fn agents_rows_flag_managed_installs() {
    let state = test_state();
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(PathBuf::from("/Users/u/.local/bin/claude")),
        Some("2.1.202 (Claude Code)"),
    );
    // A managed codex: resolved through the ~/.chimaera/agents/bin swap.
    preset_agent(
        &state,
        agents::AgentKind::Codex,
        Ok(state.managed_root.join("bin/codex")),
        Some("codex-cli 0.142.5"),
    );
    preset_agent(
        &state,
        agents::AgentKind::Gemini,
        Err("gemini not found (test)".to_string()),
        None,
    );
    preset_agent(
        &state,
        agents::AgentKind::Antigravity,
        Err("agy not found (test)".to_string()),
        None,
    );

    let (status, list) = request(&state, Method::GET, "/api/v1/agents", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap();
    let row = |id: &str| {
        list.iter()
            .find(|a| a["id"] == id)
            .unwrap_or_else(|| panic!("{id} row missing"))
            .clone()
    };

    // The user's own claude: installed, not managed.
    let claude = row("claude");
    assert_eq!(claude["installed"], true);
    assert!(!claude.as_object().unwrap().contains_key("managed"));
    assert_eq!(claude["managed_install"], true);
    // The managed codex: flagged per the pinned API contract.
    let codex = row("codex");
    assert_eq!(codex["installed"], true);
    assert_eq!(codex["managed"], true);
    assert_eq!(codex["managed_install"], true);
    // gemini: no curated managed install (node runtime, phase 2).
    assert_eq!(row("gemini")["managed_install"], false);
    assert_eq!(row("agy")["managed_install"], true);
}

/// Detection falls back to ~/.chimaera/agents/bin when the login shell
/// misses (managed installs are deliberately not on the user's PATH),
/// and such rows read as installed + managed.
#[tokio::test]
async fn managed_detection_falls_back_when_login_shell_misses() {
    let state = test_state();
    // Ground truth first: if this host has a real standalone agy CLI on
    // the login-shell PATH, the fallback is unreachable — skip. (The
    // Antigravity IDE's launcher shim is refused by detection, so IDE
    // hosts still exercise the fallback.)
    let probe = launcher::detect(&state, agents::AgentKind::Antigravity, true).await;
    if probe.path.is_ok() {
        eprintln!("skipping: a real agy CLI resolves via the login shell on this host");
        return;
    }

    let managed_bin = runtimes::managed_bin_dir(&state.managed_root);
    std::fs::create_dir_all(&managed_bin).unwrap();
    let fake = managed_bin.join("agy");
    std::fs::write(&fake, "#!/bin/sh\necho agy version 9.9.9\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let detection = launcher::detect(&state, agents::AgentKind::Antigravity, true).await;
    assert_eq!(detection.path.as_deref().ok(), Some(fake.as_path()));
    assert!(detection.managed);
    assert_eq!(detection.version.as_deref(), Some("agy version 9.9.9"));

    // And the catalog row reflects it (served from the refreshed cache).
    let (_, list) = request(&state, Method::GET, "/api/v1/agents", None).await;
    let agy = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "agy")
        .unwrap()
        .clone();
    assert_eq!(agy["installed"], true);
    assert_eq!(agy["managed"], true);
    assert_eq!(agy["path"].as_str().unwrap(), fake.to_string_lossy());
}

#[tokio::test]
async fn create_codex_agent_is_plain_tui_with_agent_kind() {
    let state = test_state();
    preset_agent(
        &state,
        agents::AgentKind::Codex,
        Ok(PathBuf::from("/bin/echo")),
        None,
    );
    let root = test_dir("codex-spawn");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({
            "workspace_id": ws["id"], "kind": "agent",
            "agent": "codex", "model": "o4-mini",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    assert_eq!(session["kind"], "agent");
    assert_eq!(session["agent_kind"], "codex");
    // Hook-driven state is claude-only: codex reads as the honest
    // unknown dot, named after its binary until an OSC title lands.
    assert_eq!(session["agent_state"], "unknown");
    assert_eq!(session["display_name"], "codex");
    // No hook settings file was generated (hooks are claude-only).
    let id = session["id"].as_str().unwrap();
    let settings_path = chimaera_core::runtime_dir()
        .join("agents")
        .join(format!("{id}-settings.json"));
    assert!(!settings_path.exists(), "codex must not get hook settings");
}

#[tokio::test]
async fn create_agent_validates_agent_model_and_resume() {
    let state = test_state();
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(PathBuf::from("/bin/echo")),
        None,
    );
    preset_agent(
        &state,
        agents::AgentKind::Codex,
        Err("codex not found via login shell (test)".to_string()),
        None,
    );
    let root = test_dir("agent-validate");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let sessions_body = |extra: serde_json::Value| {
        let mut body = serde_json::json!({"workspace_id": ws["id"], "kind": "agent"});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };

    // Unknown agent id: 400.
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(sessions_body(serde_json::json!({"agent": "cursor"}))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("unknown agent"));

    // Resume is claude-only: 400 for codex, before any binary lookup.
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(sessions_body(
            serde_json::json!({"agent": "codex", "resume": "abc"}),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("resume"));

    // Flag-shaped model/resume values: 400, never argv.
    for field in ["model", "resume"] {
        let (status, err) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(sessions_body(
                serde_json::json!({field: "--dangerously-skip-permissions"}),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}");
        assert!(
            err["error"].as_str().unwrap().contains("invalid"),
            "{field}"
        );
    }

    // A not-installed agent is a 409 with its own install hint.
    let (status, err) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(sessions_body(serde_json::json!({"agent": "codex"}))),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(err["error"].as_str().unwrap().contains("codex not found"));

    // Valid model + resume for claude spawns (argv is unit-tested in
    // launcher::tests; the API can't observe the child's argv).
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(sessions_body(serde_json::json!({
            "model": "opus",
            "resume": "5e0d64b2-abcd-abcd-abcd-000000000000",
        }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    assert_eq!(session["agent_kind"], "claude");
    let settings_path = chimaera_core::runtime_dir()
        .join("agents")
        .join(format!("{}-settings.json", session["id"].as_str().unwrap()));
    assert!(settings_path.exists(), "claude keeps hook injection");
    std::fs::remove_file(&settings_path).ok();
}

#[tokio::test]
async fn agent_events_rejects_bad_key_and_unknown_session() {
    let state = test_state();
    let id = inject_agent(&state, "right-key");

    let payload = serde_json::json!({"hook_event_name": "Stop"});
    assert_eq!(
        post_hook(&state, &id, "wrong-key", payload.clone()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post_hook(&state, &id, "", payload.clone()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post_hook(&state, "s-00000000", "right-key", payload.clone()).await,
        StatusCode::NOT_FOUND
    );
    // A bad key must not change state.
    assert_eq!(session_entry(&state, &id).await["agent_state"], "unknown");
    // The right key works.
    assert_eq!(
        post_hook(&state, &id, "right-key", payload).await,
        StatusCode::OK
    );
    assert_eq!(session_entry(&state, &id).await["agent_state"], "finished");
    state.sessions.kill(&id).ok();
}

#[tokio::test]
async fn agent_events_state_transitions() {
    let state = test_state();
    let id = inject_agent(&state, "k");

    let cases = [
        (
            serde_json::json!({"hook_event_name": "SessionStart", "source": "startup"}),
            "running",
        ),
        (
            serde_json::json!({
                "hook_event_name": "Notification",
                "notification_type": "permission_prompt",
                "message": "Claude needs your permission to use Bash",
            }),
            "needs_permission",
        ),
        (
            serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash"}),
            "running",
        ),
        (
            serde_json::json!({
                "hook_event_name": "Notification",
                "notification_type": "idle_prompt",
                "message": "Claude is waiting for your input",
            }),
            "idle_prompt",
        ),
        (
            serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}),
            "running",
        ),
        (serde_json::json!({"hook_event_name": "Stop"}), "finished"),
        (
            serde_json::json!({"hook_event_name": "StopFailure", "error_type": "rate_limit"}),
            "rate_limited",
        ),
        (
            serde_json::json!({"hook_event_name": "StopFailure", "error_type": "server_error"}),
            "errored",
        ),
        // SessionEnd keeps the last state.
        (
            serde_json::json!({"hook_event_name": "SessionEnd", "reason": "other"}),
            "errored",
        ),
    ];
    for (payload, expected) in cases {
        let event = payload["hook_event_name"].as_str().unwrap().to_string();
        assert_eq!(post_hook(&state, &id, "k", payload).await, StatusCode::OK);
        assert_eq!(
            session_entry(&state, &id).await["agent_state"],
            *expected,
            "after {event}"
        );
    }
    state.sessions.kill(&id).ok();
}

/// The v0.2 status-feed ingest end to end over the route: SubagentStart/Stop
/// build the live roster + a delegation now-line, PostToolUse replaces the
/// now-line, the statusline heartbeat (`?event=statusline`) lands quantized
/// usage, and Stop clears the turn-scoped fields while usage (session
/// telemetry, not turn state) survives.
#[tokio::test]
async fn agent_events_track_subagents_now_line_and_statusline_usage() {
    let state = test_state();
    let id = inject_agent(&state, "k");

    // Before any hook the state is unknown: `stalled` isn't checkable.
    assert_eq!(
        session_entry(&state, &id).await["stalled"],
        serde_json::Value::Null
    );

    // SubagentStart: identity on the row, plus a delegation now-line.
    let status = post_hook(
        &state,
        &id,
        "k",
        serde_json::json!({
            "hook_event_name": "SubagentStart",
            "agent_id": "sub-1",
            "agent_type": "Explore",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let row = session_entry(&state, &id).await;
    let subs = row["subagents"].as_array().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0]["id"], "sub-1");
    assert_eq!(subs[0]["label"], "Explore");
    assert!(subs[0]["started_at"].as_u64().unwrap() > 0);
    assert_eq!(row["now_line"], "delegating to Explore");
    // Running + a just-spawned (still chatty) PTY: checkable, not stalled.
    assert_eq!(row["agent_state"], "running");
    assert_eq!(row["stalled"], false);

    // A SubagentStart without identity is logged and dropped, never an error.
    let status = post_hook(
        &state,
        &id,
        "k",
        serde_json::json!({"hook_event_name": "SubagentStart", "unexpected": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let row = session_entry(&state, &id).await;
    assert_eq!(row["subagents"].as_array().unwrap().len(), 1);

    // PostToolUse replaces the now-line with the tool summary.
    post_hook(
        &state,
        &id,
        "k",
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {"file_path": "/w/src/lib.rs"},
        }),
    )
    .await;
    assert_eq!(
        session_entry(&state, &id).await["now_line"],
        "edited lib.rs"
    );

    // SubagentStop drops the entry; an empty roster reads null on the wire.
    post_hook(
        &state,
        &id,
        "k",
        serde_json::json!({"hook_event_name": "SubagentStop", "agent_id": "sub-1"}),
    )
    .await;
    assert_eq!(
        session_entry(&state, &id).await["subagents"],
        serde_json::Value::Null
    );

    // The statusline heartbeat authenticates like any hook delivery.
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/agent-events/{id}?key=wrong&event=statusline"),
        Some(serde_json::json!({"cost": {"total_cost_usd": 1.0}})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A good heartbeat lands quantized usage without touching the state.
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/agent-events/{id}?key=k&event=statusline"),
        Some(serde_json::json!({
            "hook_event_name": "Status",
            "model": {"id": "claude-opus-4", "display_name": "Opus"},
            "context_window": {"used_percentage": 41.7},
            "cost": {"total_cost_usd": 0.1249},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let row = session_entry(&state, &id).await;
    assert_eq!(
        row["usage"],
        serde_json::json!({"model": "Opus", "context_pct": 42, "cost_usd": 0.12})
    );
    assert_eq!(row["agent_state"], "running");

    // Stop clears the turn-scoped fields; usage survives the turn.
    post_hook(
        &state,
        &id,
        "k",
        serde_json::json!({"hook_event_name": "Stop"}),
    )
    .await;
    let row = session_entry(&state, &id).await;
    assert_eq!(row["now_line"], serde_json::Value::Null);
    assert_eq!(row["usage"]["model"], "Opus");
    // Finished ≠ a working claim: stalled is null again.
    assert_eq!(row["stalled"], serde_json::Value::Null);

    state.sessions.kill(&id).ok();
}

/// The statusline curl keeps the session key OFF its argv (F1) by sending it
/// as `Authorization: Bearer` instead of `?key=` — /proc/<pid>/cmdline is
/// world-readable on the shared login nodes. The ingest route accepts that
/// channel (no query key at all), and a wrong bearer is still rejected.
#[tokio::test]
async fn statusline_ingest_accepts_the_bearer_key_channel() {
    let state = test_state();
    let id = inject_agent(&state, "k");

    let post = |auth: &'static str| {
        let state = state.clone();
        let id = id.clone();
        async move {
            app(state)
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        // No ?key= — the key rides the Bearer header only.
                        .uri(format!("/api/v1/agent-events/{id}?event=statusline"))
                        .header(header::AUTHORIZATION, auth)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::json!({"cost": {"total_cost_usd": 0.2}}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };

    // The real channel: no query key, the session key in the Bearer header.
    assert_eq!(post("Bearer k").await, StatusCode::OK);
    // The header is really authenticating — a wrong bearer is forbidden.
    assert_eq!(post("Bearer wrong").await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn agent_title_tail_polls_transcript() {
    let state = test_state();
    let id = inject_agent(&state, "k");
    agents::spawn_agent_watch(state.clone(), id.clone());

    // Synthetic SessionStart pointing transcript_path at a fixture file.
    let transcript = test_dir("transcript").join("session.jsonl");
    std::fs::write(&transcript, "{\"type\":\"message\"}\n").unwrap();
    let status = post_hook(
        &state,
        &id,
        "k",
        serde_json::json!({
            "hook_event_name": "SessionStart",
            "source": "startup",
            "session_id": "5e0d64b2-abcd-abcd-abcd-000000000000",
            "transcript_path": transcript.to_string_lossy(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let wait_for_title = |expected: &'static str| {
        let state = state.clone();
        let id = id.clone();
        async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let title = session_entry(&state, &id).await["agent_title"].clone();
                if title == expected {
                    return;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "agent_title stuck at {title}, want {expected}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    };

    // An appended ai-title record becomes the title...
    let mut line = serde_json::json!(
        {"type": "ai-title", "aiTitle": "Fix the flaky tests", "sessionId": "x"}
    )
    .to_string();
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    std::io::Write::write_all(&mut file, line.as_bytes()).unwrap();
    wait_for_title("Fix the flaky tests").await;

    // ...and a later customTitle record wins over it.
    let mut line = serde_json::json!({"type": "custom-title", "customTitle": "My run"}).to_string();
    line.push('\n');
    std::io::Write::write_all(&mut file, line.as_bytes()).unwrap();
    wait_for_title("My run").await;

    state.sessions.kill(&id).ok();
}

#[tokio::test]
async fn agent_first_prompt_is_provisional_display_name() {
    let state = test_state();
    let id = inject_agent(&state, "k");

    // No hook data yet: the generic agent name.
    assert_eq!(session_entry(&state, &id).await["display_name"], "claude");

    // The first UserPromptSubmit becomes the provisional title,
    // truncated near 60 chars at a word boundary.
    let prompt = "please refactor the entire qc pipeline so the reports land in \
                      results/qc and nothing downstream breaks";
    let status = post_hook(
        &state,
        &id,
        "k",
        serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": prompt}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = session_entry(&state, &id).await;
    let display = entry["display_name"].as_str().unwrap();
    assert!(
        display.starts_with("please refactor the entire qc pipeline"),
        "{display}"
    );
    assert!(display.ends_with('…'), "{display}");
    assert!(display.chars().count() <= 61, "{display}");
    assert_eq!(entry["agent_state"], "running");

    // A later prompt does not displace the first.
    let status = post_hook(
        &state,
        &id,
        "k",
        serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": "and again"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        session_entry(&state, &id).await["display_name"],
        display,
        "first prompt must stay the provisional title"
    );

    state.sessions.kill(&id).ok();
}

#[tokio::test]
async fn agent_files_touched_builds_from_post_tool_use_hooks() {
    let state = test_state();
    let id = inject_agent(&state, "k");

    // A fresh agent has an empty list (never null — the UI renders a
    // quiet zero-files chip row, not a missing field).
    assert_eq!(
        session_entry(&state, &id).await["files_touched"],
        serde_json::json!([])
    );

    // Every file-writing tool contributes; non-writing tools do not.
    for (tool, field, path) in [
        ("Write", "file_path", "/w/a.rs"),
        ("Edit", "file_path", "/w/b.rs"),
        ("MultiEdit", "file_path", "/w/c.rs"),
        ("NotebookEdit", "notebook_path", "/w/d.ipynb"),
        ("Bash", "command", "cargo test"),
        ("Read", "file_path", "/w/read-only.rs"),
    ] {
        assert_eq!(
            post_hook(&state, &id, "k", touch_payload(tool, field, path)).await,
            StatusCode::OK
        );
    }
    assert_eq!(
        session_entry(&state, &id).await["files_touched"],
        serde_json::json!(["/w/a.rs", "/w/b.rs", "/w/c.rs", "/w/d.ipynb"])
    );

    // Re-touching an older path moves it to the end: dedupe, newest last.
    post_hook(
        &state,
        &id,
        "k",
        touch_payload("Edit", "file_path", "/w/a.rs"),
    )
    .await;
    assert_eq!(
        session_entry(&state, &id).await["files_touched"],
        serde_json::json!(["/w/b.rs", "/w/c.rs", "/w/d.ipynb", "/w/a.rs"])
    );

    // State changes clear nothing; the list lives as long as the session.
    post_hook(
        &state,
        &id,
        "k",
        serde_json::json!({"hook_event_name": "Stop"}),
    )
    .await;
    let entry = session_entry(&state, &id).await;
    assert_eq!(entry["agent_state"], "finished");
    assert_eq!(
        entry["files_touched"],
        serde_json::json!(["/w/b.rs", "/w/c.rs", "/w/d.ipynb", "/w/a.rs"])
    );

    // The cap keeps the newest 100, oldest dropped first.
    for i in 0..105 {
        post_hook(
            &state,
            &id,
            "k",
            touch_payload("Write", "file_path", &format!("/w/f{i}.rs")),
        )
        .await;
    }
    let entry = session_entry(&state, &id).await;
    let touched = entry["files_touched"].as_array().unwrap();
    assert_eq!(touched.len(), 100);
    // 4 pre-existing + 105 new = 109; the 9 oldest fell off.
    assert_eq!(touched.first().unwrap(), "/w/f5.rs");
    assert_eq!(touched.last().unwrap(), "/w/f104.rs");

    state.sessions.kill(&id).ok();
}

/// End-to-end against the real `claude` binary: spawn kind=agent, watch
/// the TUI come up in the PTY, and wait for a real hook POST to flip the
/// agent state. Gated behind CHIMAERA_TEST_CLAUDE=1 so CI without claude
/// (or without a subscription) stays green.
#[tokio::test]
async fn real_claude_agent_session() {
    if std::env::var("CHIMAERA_TEST_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipping real_claude_agent_session (set CHIMAERA_TEST_CLAUDE=1)");
        return;
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = test_state_with_port(port);
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let root = test_dir("claude-agent");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws["id"], "kind": "agent"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "agent spawn failed: {session}");
    assert_eq!(session["kind"], "agent");
    let id = session["id"].as_str().unwrap().to_string();

    // 1. The claude TUI comes up in the PTY.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "claude TUI never appeared in the PTY snapshot"
        );
        let text = match state.sessions.attach(&id) {
            Ok(att) => String::from_utf8_lossy(&att.snapshot).to_string(),
            Err(_) => String::new(),
        };
        if text.to_lowercase().contains("claude") {
            eprintln!("TUI is up (snapshot contains 'claude')");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    // 2. A real hook POST flips the state away from "unknown". Nudge the
    // TUI if needed: Enter dismisses a possible trust dialog, then a tiny
    // prompt guarantees a UserPromptSubmit hook.
    let attachment = state.sessions.attach(&id).expect("attach for input");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut nudges = 0u32;
    loop {
        let entry = session_entry(&state, &id).await;
        let agent_state = entry["agent_state"].as_str().unwrap_or("").to_string();
        if agent_state != "unknown" {
            eprintln!("hook flipped agent_state to {agent_state}");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no hook POST ever flipped the state"
        );
        let elapsed = 120 - (deadline - tokio::time::Instant::now()).as_secs();
        if elapsed > 10 && nudges == 0 {
            eprintln!("nudge: Enter (possible trust dialog)");
            attachment.input.send(bytes::Bytes::from("\r")).await.ok();
            nudges = 1;
        } else if elapsed > 20 && nudges == 1 {
            eprintln!("nudge: submitting a tiny prompt");
            attachment
                .input
                .send(bytes::Bytes::from("reply with just: ok\r"))
                .await
                .ok();
            nudges = 2;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    // Bonus observation (not asserted): the title tail-poll may pick up
    // claude's ai-title record.
    for _ in 0..30 {
        let entry = session_entry(&state, &id).await;
        if let Some(title) = entry["agent_title"].as_str() {
            eprintln!("observed agent_title from transcript: {title}");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let final_entry = session_entry(&state, &id).await;
    eprintln!(
        "final session entry: state={} title={}",
        final_entry["agent_state"], final_entry["agent_title"]
    );

    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/sessions/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
