use super::support::*;
use crate::*;

#[tokio::test]
async fn create_session_validates_chat_ui() {
    let state = test_state();
    let root = test_dir("ws-root");
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let wid = ws["id"].as_str().unwrap().to_string();

    // "chat" is an agent-session surface; a shell cannot have one.
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": wid, "ui": "chat"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": wid, "ui": "bogus"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Unknown session for the view switch.
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/sessions/s-nope/view",
        Some(serde_json::json!({"ui": "chat"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sessions_lifecycle() {
    let state = test_state();
    let root = test_dir("sess-root");

    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let workspace_id = ws["id"].as_str().unwrap().to_string();

    // Spawning against an unknown workspace is a 404.
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": "w-00000000", "name": null})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // POST spawns a real shell.
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": workspace_id, "name": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = session["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("s-"), "bad session id {id}");
    assert_eq!(session["workspace_id"].as_str().unwrap(), workspace_id);
    assert_eq!(session["cols"], 80);
    assert_eq!(session["rows"], 24);
    assert_eq!(session["alive"], true);

    // A fresh shell is named after the shell binary itself (naming rule
    // zero: it sits idle at the workspace root), and nothing is pinned.
    assert_eq!(
        session["display_name"].as_str().unwrap(),
        naming::default_shell_name()
    );
    assert_eq!(session["renamed"], false);

    // GET lists it, alive.
    let (status, list) = request(&state, Method::GET, "/api/v1/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == session["id"])
        .expect("session listed");
    assert_eq!(entry["alive"], true);
    assert_eq!(entry["workspace_id"].as_str().unwrap(), workspace_id);
    assert!(entry["display_name"].is_string());

    // DELETE kills it.
    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/sessions/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Afterwards the session is gone or reported dead (the shell may take
    // a moment to reap, so poll briefly).
    let mut gone_or_dead = false;
    for _ in 0..50 {
        let (_, list) = request(&state, Method::GET, "/api/v1/sessions", None).await;
        match list.as_array().unwrap().iter().find(|s| s["id"] == id) {
            None => gone_or_dead = true,
            Some(entry) if entry["alive"] == false => gone_or_dead = true,
            Some(_) => {}
        }
        if gone_or_dead {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(gone_or_dead, "session still alive after DELETE");
}

#[tokio::test]
async fn session_kind_defaults_to_shell_and_round_trips() {
    let state = test_state();
    let root = test_dir("kind-root");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let workspace_id = ws["id"].as_str().unwrap().to_string();

    // No kind in the body -> shell.
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": workspace_id, "name": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session["kind"], "shell");
    assert_eq!(session["agent_kind"], serde_json::Value::Null);
    assert_eq!(session["agent_state"], serde_json::Value::Null);
    assert_eq!(session["agent_title"], serde_json::Value::Null);
    assert_eq!(session["files_touched"], serde_json::Value::Null);
    // Output-recency activity is an agent-row signal; shells carry null.
    assert_eq!(session["output_active"], serde_json::Value::Null);

    // Explicit kind "shell" round-trips through GET.
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": workspace_id, "kind": "shell"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = session_entry(&state, session["id"].as_str().unwrap()).await;
    assert_eq!(entry["kind"], "shell");
    assert_eq!(entry["agent_state"], serde_json::Value::Null);
    assert_eq!(entry["agent_title"], serde_json::Value::Null);
    assert_eq!(entry["files_touched"], serde_json::Value::Null);

    // An unknown kind is a 400 (serde rejects it).
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": workspace_id, "kind": "bogus"})),
    )
    .await;
    assert_ne!(status, StatusCode::OK);
}

/// Every chimaera session carries CHIMAERA_SESSION / CHIMAERA_THEME /
/// CHIMAERA_SHIMS and the shim-dir PATH prefix — asserted through a
/// real daemon-spawned session's own environment (`/usr/bin/env` in the
/// PTY; the install spawn path shares `session_env` with POST
/// /sessions, and a user shell's rc can stall for a minute on this
/// machine, so the deterministic command is the honest probe).
#[tokio::test]
async fn session_env_reaches_spawned_session() {
    let state = test_state();
    let ws_id = make_workspace(&state, "env-root").await;

    // The assembled contract itself: shims first on PATH, session id,
    // scheme, and the wrap's re-prepend handle.
    let env = api::session_env(&state, "s-envx", "light", None);
    let shims = state.shims_dir.display().to_string();
    assert_eq!(env[0].0, "PATH");
    assert!(env[0].1.starts_with(&format!("{shims}:")), "{env:?}");
    assert!(env.contains(&("CHIMAERA_SESSION".to_string(), "s-envx".to_string())));
    assert!(env.contains(&("CHIMAERA_THEME".to_string(), "light".to_string())));
    assert!(env.contains(&("CHIMAERA_SHIMS".to_string(), shims.clone())));
    // No prelude → no CHIMAERA_PRELUDE at all (zero-delta contract); with
    // one, the path rides along and the DONE guard is scrubbed first.
    assert!(!env.iter().any(|(k, _)| k == "CHIMAERA_PRELUDE"), "{env:?}");
    let env = api::session_env(
        &state,
        "s-envx",
        "light",
        Some(std::path::Path::new("/tmp/p.sh")),
    );
    assert!(env.contains(&("CHIMAERA_PRELUDE".to_string(), "/tmp/p.sh".to_string())));
    let remove = api::spawn_env_remove();
    assert!(remove.iter().any(|n| n == "CHIMAERA_PRELUDE"));
    assert!(remove.iter().any(|n| n == "CHIMAERA_PRELUDE_DONE"));

    // An empty inherited PATH must not leave a trailing colon (an empty
    // member = the cwd on the search path); the fixed system default
    // fills in instead.
    assert_eq!(
        api::spawn_path("/s", ""),
        "/s:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    );
    assert_eq!(api::spawn_path("/s", "/usr/bin:/bin"), "/s:/usr/bin:/bin");

    // Through a real spawn: a stub install session dumps its env into
    // the pane (kept alive so the snapshot — scrollback included — is
    // stable). Grid rows wrap at the pane width, so matching happens on
    // the de-wrapped text.
    let workspace = lock(&state.workspaces).get(&ws_id).unwrap();
    let sid = runtimes::start_install(
        &state,
        agents::AgentKind::Claude,
        &workspace,
        "/usr/bin/env; sleep 30".to_string(),
    )
    .expect("stub env session spawned");
    let needles = [
        format!("CHIMAERA_SESSION={sid}"),
        "CHIMAERA_THEME=dark".to_string(),
        format!("CHIMAERA_SHIMS={shims}"),
        format!("PATH={shims}:"),
    ];
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let att = state.sessions.attach(&sid).expect("attach env session");
        let text = String::from_utf8_lossy(&att.snapshot).replace(['\r', '\n'], "");
        if needles.iter().all(|n| text.contains(n)) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "env dump incomplete; missing {:?}",
            needles
                .iter()
                .filter(|n| !text.contains(*n))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    state.sessions.kill(&sid).ok();

    // An invalid theme on POST /sessions is a 400, not a silent dark.
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({
            "workspace_id": ws_id, "kind": "shell", "theme": "blue",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn session_spawn_size_is_honored_and_clamped() {
    let state = test_state();
    let root = test_dir("size-root");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let workspace_id = ws["id"].as_str().unwrap().to_string();

    let spawn = |body: serde_json::Value| {
        let state = state.clone();
        async move {
            let (status, session) =
                request(&state, Method::POST, "/api/v1/sessions", Some(body)).await;
            assert_eq!(status, StatusCode::OK, "spawn failed: {session}");
            session
        }
    };

    // An in-range size spawns the PTY at exactly that size.
    let session = spawn(serde_json::json!({
        "workspace_id": workspace_id, "cols": 132, "rows": 43,
    }))
    .await;
    assert_eq!(session["cols"], 132);
    assert_eq!(session["rows"], 43);
    let entry = session_entry(&state, session["id"].as_str().unwrap()).await;
    assert_eq!(entry["cols"], 132);
    assert_eq!(entry["rows"], 43);
    state.sessions.kill(session["id"].as_str().unwrap()).ok();

    // Too small clamps up to 20x5; too large clamps down to 500x200.
    let session = spawn(serde_json::json!({
        "workspace_id": workspace_id, "cols": 1, "rows": 1000,
    }))
    .await;
    assert_eq!(session["cols"], 20);
    assert_eq!(session["rows"], 200);
    state.sessions.kill(session["id"].as_str().unwrap()).ok();

    let session = spawn(serde_json::json!({
        "workspace_id": workspace_id, "cols": 501, "rows": 1,
    }))
    .await;
    assert_eq!(session["cols"], 500);
    assert_eq!(session["rows"], 5);
    state.sessions.kill(session["id"].as_str().unwrap()).ok();

    // Omitted sizes keep the 80x24 default.
    let session = spawn(serde_json::json!({"workspace_id": workspace_id})).await;
    assert_eq!(session["cols"], 80);
    assert_eq!(session["rows"], 24);
    state.sessions.kill(session["id"].as_str().unwrap()).ok();
}
