use super::support::*;

#[tokio::test]
async fn shell_display_name_tracks_foreground_command() {
    let state = test_state();
    let root = test_dir("naming-fg");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let workspace_id = ws["id"].as_str().unwrap().to_string();
    let root = std::fs::canonicalize(&root).unwrap();
    let id = inject_shell(&state, &root, &workspace_id);

    // Idle at the workspace root: named after the shell binary.
    wait_display_name(&state, &id, "bash").await;
    assert_eq!(session_entry(&state, &id).await["renamed"], false);

    // A running foreground command takes over the name...
    let att = state.sessions.attach(&id).expect("attach");
    att.input
        .send(bytes::Bytes::from("sleep 5\n"))
        .await
        .expect("send input");
    wait_display_name(&state, &id, "sleep").await;

    // ...and the name falls back to the shell once it exits.
    wait_display_name(&state, &id, "bash").await;

    state.sessions.kill(&id).ok();
}

#[tokio::test]
async fn shell_display_name_uses_workspace_relative_cwd() {
    let state = test_state();
    let root = test_dir("naming-cd");
    std::fs::create_dir(root.join("crates")).unwrap();
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let workspace_id = ws["id"].as_str().unwrap().to_string();
    let root = std::fs::canonicalize(&root).unwrap();
    let id = inject_shell(&state, &root, &workspace_id);

    wait_display_name(&state, &id, "bash").await;

    // cd into a subdirectory: the idle shell is named by where it sits,
    // relative to the workspace root.
    let att = state.sessions.attach(&id).expect("attach");
    att.input
        .send(bytes::Bytes::from("cd crates\n"))
        .await
        .expect("send input");
    wait_display_name(&state, &id, "crates").await;

    // cd back to the root: the shell name again.
    att.input
        .send(bytes::Bytes::from("cd ..\n"))
        .await
        .expect("send input");
    wait_display_name(&state, &id, "bash").await;

    state.sessions.kill(&id).ok();
}

/// End-to-end shell integration: a real bash spawned the way
/// create_session spawns it (integration injected, hermetic HOME) must
/// reach phase `ready` and populate the command journal with command
/// text, output, and exit codes.
#[tokio::test]
async fn integrated_shell_populates_command_journal() {
    use chimaera_pty::ShellPhase;

    let state = test_state();
    let base = test_dir("shellint-base");
    let home = test_dir("shellint-home");
    let launch = chimaera_core::shellint::shell_launch_for("/bin/bash", &base).expect("launch");
    let mut env = launch.env;
    env.push(("HOME".to_string(), home.to_string_lossy().into_owned()));
    let info = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: test_dir("shellint-cwd"),
            name: None,
            cols: 80,
            rows: 24,
            command: Some(launch.argv),
            id: None,
            env,
            env_remove: Vec::new(),
            scrollback: None,
        })
        .expect("spawn integrated bash");
    let marks = state.sessions.marks(&info.id).expect("marks");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while marks.phase() != ShellPhase::Ready {
        assert!(
            tokio::time::Instant::now() < deadline,
            "integrated shell never reached ready (phase {:?})",
            marks.phase()
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let att = state.sessions.attach(&info.id).expect("attach");
    att.input
        .send(bytes::Bytes::from("echo integration-works\n"))
        .await
        .expect("send command");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let entry = loop {
        let done = marks
            .journal(10)
            .into_iter()
            .find(|e| !e.running && e.command.as_deref() == Some("echo integration-works"));
        if let Some(entry) = done {
            break entry;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "journal never recorded the command; journal: {:?}",
            marks.journal(10)
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(entry.exit_code, Some(0), "{entry:?}");
    assert!(entry.output.contains("integration-works"), "{entry:?}");
    assert_eq!(entry.source, chimaera_pty::CommandSource::User);

    state.sessions.kill(&info.id).ok();
}

#[tokio::test]
async fn cwd_current_tracks_shell_cd_and_falls_back_to_spawn_cwd() {
    let state = test_state();
    let root = test_dir("cwd-current");
    std::fs::create_dir(root.join("sub")).unwrap();
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let workspace_id = ws["id"].as_str().unwrap().to_string();
    let root = std::fs::canonicalize(&root).unwrap();
    let id = inject_shell(&state, &root, &workspace_id);

    // The watcher's first poll reports the spawn cwd.
    wait_cwd_current(&state, &id, &root).await;

    // cd into a subdirectory: cwd_current follows...
    let att = state.sessions.attach(&id).expect("attach");
    att.input
        .send(bytes::Bytes::from("cd sub\n"))
        .await
        .expect("send input");
    wait_cwd_current(&state, &id, &root.join("sub")).await;

    // ...and back.
    att.input
        .send(bytes::Bytes::from("cd ..\n"))
        .await
        .expect("send input");
    wait_cwd_current(&state, &id, &root).await;

    state.sessions.kill(&id).ok();

    // No polled value (agents run no cwd watcher; they keep their spawn
    // cwd): the field falls back to the spawn cwd.
    let agent = inject_agent(&state, "k");
    let entry = session_entry(&state, &agent).await;
    assert_eq!(entry["cwd_current"], entry["cwd"]);
    state.sessions.kill(&agent).ok();
}
