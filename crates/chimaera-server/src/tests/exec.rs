use super::support::*;

/// Exec into a shell with NO integration: the engine must fall back to
/// sentinel mode and still deliver output + exit code through the
/// printf-emitted marks.
#[tokio::test]
async fn exec_sentinel_round_trip_on_plain_shell() {
    let state = test_state();
    let info = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: test_dir("exec-sentinel"),
            name: None,
            cols: 80,
            rows: 24,
            command: Some(vec![
                "/bin/bash".to_string(),
                "--noprofile".to_string(),
                "--norc".to_string(),
            ]),
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .expect("spawn plain bash");
    let id = info.id;

    let (status, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/sessions/{id}/exec"),
        Some(serde_json::json!({"command": "echo sentinel-ran && false"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["mode"], "sentinel", "{out}");
    assert_eq!(out["timed_out"], false, "{out}");
    assert_eq!(out["record"]["exit_code"], 1, "{out}");
    assert_eq!(out["record"]["source"], "agent", "{out}");
    assert!(
        out["record"]["output"]
            .as_str()
            .unwrap()
            .contains("sentinel-ran"),
        "{out}"
    );

    state.sessions.kill(&id).ok();
}

/// The author decision in action: an exec against a busy integrated
/// shell QUEUES until the prompt returns, then runs in integrated mode.
#[tokio::test]
async fn exec_queues_behind_running_command() {
    let state = test_state();
    let id = spawn_integrated_bash(&state, "exec-queue").await;

    let att = state.sessions.attach(&id).expect("attach");
    att.input
        .send(bytes::Bytes::from("sleep 2\n"))
        .await
        .expect("start user command");
    // Give the sleep a moment to actually start (phase -> running).
    let marks = state.sessions.marks(&id).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while marks.phase() != chimaera_pty::ShellPhase::Running {
        assert!(
            tokio::time::Instant::now() < deadline,
            "sleep never started"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let (status, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/sessions/{id}/exec"),
        Some(serde_json::json!({
            "command": "echo queued-ran",
            "queue_timeout_ms": 15000,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["mode"], "integrated", "{out}");
    assert!(
        out["record"]["output"]
            .as_str()
            .unwrap()
            .contains("queued-ran"),
        "{out}"
    );
    // It genuinely waited for the sleep instead of typing over it.
    assert!(
        out["waited_ms"].as_u64().unwrap() >= 1000,
        "expected a queue wait, got {out}"
    );

    state.sessions.kill(&id).ok();
}

/// With a short queue timeout and no remote-forwarding foreground, a
/// busy shell is a 409 — never typed into.
#[tokio::test]
async fn exec_busy_is_409_without_sentinel_permission() {
    let state = test_state();
    let id = spawn_integrated_bash(&state, "exec-busy").await;

    let att = state.sessions.attach(&id).expect("attach");
    att.input
        .send(bytes::Bytes::from("sleep 5\n"))
        .await
        .expect("start user command");
    let marks = state.sessions.marks(&id).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while marks.phase() != chimaera_pty::ShellPhase::Running {
        assert!(
            tokio::time::Instant::now() < deadline,
            "sleep never started"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let (status, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/sessions/{id}/exec"),
        Some(serde_json::json!({
            "command": "echo should-not-run",
            "queue_timeout_ms": 300,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");

    state.sessions.kill(&id).ok();
}

#[tokio::test]
async fn exec_into_agent_session_is_409_and_journal_endpoint_reads() {
    let state = test_state();
    let agent_id = inject_agent(&state, "k");
    let (status, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/sessions/{agent_id}/exec"),
        Some(serde_json::json!({"command": "echo nope"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");

    // Journal endpoint: exec into a shell, then read it back over HTTP.
    let id = spawn_integrated_bash(&state, "journal-ep").await;
    let (status, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/sessions/{id}/exec"),
        Some(serde_json::json!({"command": "echo journaled"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");

    let (status, journal) = request(
        &state,
        Method::GET,
        &format!("/api/v1/sessions/{id}/journal?limit=5"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{journal}");
    assert_eq!(journal["phase"], "ready", "{journal}");
    let entries = journal["entries"].as_array().unwrap();
    let entry = entries
        .iter()
        .find(|e| e["command"] == "echo journaled")
        .expect("journaled entry");
    assert_eq!(entry["source"], "agent", "{journal}");
    assert_eq!(entry["exit_code"], 0, "{journal}");

    // Unknown session is a 404.
    let (status, _) = request(
        &state,
        Method::GET,
        "/api/v1/sessions/s-00000000/journal",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    state.sessions.kill(&agent_id).ok();
    state.sessions.kill(&id).ok();
}
