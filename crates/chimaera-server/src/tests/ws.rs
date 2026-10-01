use super::support::*;
use crate::*;

/// Stopping a session for a transfer (what a Pro handoff export does: mark
/// it transferring, park its ledger entry, stop the process) must tell every
/// attached view that it moved — not that it exited — both on the live
/// socket and on every reconnect until it runs somewhere again.
#[tokio::test]
async fn ws_sessions_stopped_for_a_transfer_say_moved_not_exited() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    async fn next_json<S>(socket: &mut S) -> serde_json::Value
    where
        S: futures::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        loop {
            if let WsMessage::Text(text) = next_ws_frame(socket).await {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }
    let state = test_state();
    let cwd = test_dir("ws-moved");
    let terminal = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: cwd.clone(),
            name: None,
            cols: 80,
            rows: 24,
            command: None,
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .expect("spawn session")
        .id;
    let chat = "s-moved-chat".to_string();
    state
        .chat
        .spawn(
            &chimaera_agent::claude::ClaudeAdapter,
            chimaera_agent::driver::SpawnSpec::new(
                chat.clone(),
                vec![write_fake_claude("ws-moved-agent")
                    .to_string_lossy()
                    .into_owned()],
                cwd.clone(),
            ),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let connect = |path: String| async move {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}{path}"))
            .await
            .unwrap();
        socket
            .send(WsMessage::text(
                serde_json::json!({"type": "auth", "token": "test-token", "last_seq": 0})
                    .to_string(),
            ))
            .await
            .unwrap();
        socket
    };
    for (id, path) in [
        (terminal.clone(), format!("/ws/sessions/{terminal}")),
        (chat.clone(), format!("/ws/chat/{chat}")),
    ] {
        let mut socket = connect(path.clone()).await;
        assert_eq!(next_json(&mut socket).await["type"], "ready");
        let guard = crate::chat::ChatSwitchGuard::acquire(&state, &id, "transfer").unwrap();
        crate::ledger::defer(
            &state,
            crate::ledger::LedgerEntry {
                id: id.clone(),
                suspended: true,
                handoff: None,
                workspace_id: "w-moving".into(),
                cwd: cwd.clone(),
                pinned_name: None,
                cols: 80,
                rows: 24,
                theme: "dark".into(),
                created_at: 0,
                agent: None,
            },
        )
        .unwrap();
        if id == chat {
            state.chat.kill(&id);
        } else {
            state.sessions.kill(&id).unwrap();
        }
        loop {
            let frame = next_json(&mut socket).await;
            assert_ne!(frame["type"], "exited", "{path}: a move is not an exit");
            if frame["type"] == "moved" {
                assert_eq!(frame["to"], "cloud");
                break;
            }
        }
        drop(guard);
        // The project now runs in the cloud. Reconnecting keeps saying so,
        // never replaying a stopped driver as `ready {alive:false}`.
        crate::pro::install_remote_owner_fixture(&state, "w-moving", 5);
        let mut again = connect(path.clone()).await;
        let frame = next_json(&mut again).await;
        assert_eq!(frame["type"], "moved", "{path}: {frame}");
        assert_eq!(frame["to"], "cloud", "{path}: {frame}");
    }
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}

/// A session waiting out a daemon restart on the computer that owns its
/// project did not move anywhere: its views say it is reconnecting after an
/// update (`paused`, reason `restarting`), on the socket and on its row.
#[tokio::test]
async fn a_restart_deferred_session_is_paused_here_not_moved() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let cwd = test_dir("ws-restart-deferred");
    let workspace = lock(&state.workspaces).add(cwd.clone()).unwrap();
    let entry = |id: &str, agent| crate::ledger::LedgerEntry {
        id: id.into(),
        suspended: true,
        handoff: None,
        workspace_id: workspace.id.clone(),
        cwd: cwd.clone(),
        pinned_name: None,
        cols: 80,
        rows: 24,
        theme: "dark".into(),
        created_at: 0,
        agent,
    };
    let agent = crate::ledger::LedgerAgent {
        kind: crate::agents::AgentKind::Claude,
        resume: None,
        transcript: None,
        native_cwd: None,
        title: "restart".into(),
        ui: chimaera_agent::model::SessionUi::Chat,
        model: None,
        carryover: None,
    };
    crate::ledger::defer(&state, entry("s-restart-chat", Some(agent))).unwrap();
    crate::ledger::defer(&state, entry("s-restart-term", None)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    for path in ["/ws/chat/s-restart-chat", "/ws/sessions/s-restart-term"] {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}{path}"))
            .await
            .unwrap();
        socket
            .send(WsMessage::text(
                serde_json::json!({"type": "auth", "token": "test-token", "last_seq": 0})
                    .to_string(),
            ))
            .await
            .unwrap();
        let frame = loop {
            if let WsMessage::Text(text) = next_ws_frame(&mut socket).await {
                break serde_json::from_str::<serde_json::Value>(&text).unwrap();
            }
        };
        assert_eq!(
            frame,
            serde_json::json!({"type":"paused","reason":"restarting"}),
            "{path}"
        );
    }
    let rows = crate::session_view::sessions_json(&state);
    let row = rows
        .iter()
        .find(|row| row["id"] == "s-restart-chat")
        .unwrap();
    assert_eq!(row["suspended"], true);
    assert_eq!(row["pause"]["type"], "paused");
    assert_eq!(row["pause"]["reason"], "restarting");
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}

#[tokio::test]
async fn ws_agent_plugins_invalidation_without_session_changes() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/events"))
        .await
        .unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();

    for expected in 0..=1 {
        loop {
            if let WsMessage::Text(text) = next_ws_frame(&mut socket).await {
                let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
                if frame["type"] == "agent_plugins" {
                    assert_eq!(frame["epoch"], expected);
                    break;
                }
            }
        }
        if expected == 0 {
            state.probes.changed();
            state.changes.notify_waiters();
        }
    }
    socket.close(None).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn ws_bridge_auth_snapshot_and_echo() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let cwd = test_dir("ws-cwd");
    let info = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd,
            name: None,
            cols: 80,
            rows: 24,
            command: None,
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .expect("spawn session");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://{addr}/ws/sessions/{}", info.id);
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // 1. First-frame auth.
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();

    // 2. Ready text frame with the SessionInfo fields.
    let ready = match next_ws_frame(&mut socket).await {
        WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        other => panic!("expected ready text frame, got {other:?}"),
    };
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["id"].as_str().unwrap(), info.id);
    // No naming watcher runs for this session, so the ready frame's
    // cwd_current falls back to the spawn cwd.
    assert_eq!(ready["cwd_current"], ready["cwd"]);

    // 3. Snapshot as one binary frame.
    match next_ws_frame(&mut socket).await {
        WsMessage::Binary(_) => {}
        other => panic!("expected snapshot binary frame, got {other:?}"),
    }

    // 4. Send input; the echoed output must come back as binary frames.
    socket
        .send(WsMessage::binary(&b"echo ws-test\n"[..]))
        .await
        .unwrap();

    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !String::from_utf8_lossy(&collected).contains("ws-test") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no ws-test output; got: {}",
            String::from_utf8_lossy(&collected)
        );
        match next_ws_frame(&mut socket).await {
            WsMessage::Binary(bytes) => collected.extend_from_slice(&bytes),
            WsMessage::Text(_) => {} // events are fine to interleave
            other => panic!("unexpected frame {other:?}"),
        }
    }

    state.sessions.kill(&info.id).ok();
}

/// The park protocol, end to end on the real bridge: `auth.parked` attaches
/// with NO snapshot frame (attach_quiet), output produced while parked is
/// withheld, and the first `unpark` repaints — a `resync` text frame with
/// the adjacent full snapshot carrying everything produced meanwhile.
#[tokio::test]
async fn ws_parked_attach_withholds_until_unpark_repaints() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let info = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: test_dir("ws-park-cwd"),
            name: None,
            cols: 80,
            rows: 24,
            command: None,
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .expect("spawn session");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://{addr}/ws/sessions/{}", info.id);
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token", "parked": true}).to_string(),
        ))
        .await
        .unwrap();

    // Ready arrives — and nothing else: no snapshot binary follows a parked
    // attach, and output typed into the PTY meanwhile is withheld.
    let ready = match next_ws_frame(&mut socket).await {
        WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        other => panic!("expected ready text frame, got {other:?}"),
    };
    assert_eq!(ready["type"], "ready");

    // Produce output while parked (typed via a second, visible attachment's
    // input handle so this connection's own pipe stays quiet).
    let mut side = state.sessions.attach(&info.id).expect("side attach");
    side.input
        .send(bytes::Bytes::from_static(b"echo park-withheld-marker\n"))
        .await
        .expect("side input");
    // Wait until the session demonstrably produced the output.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, side.output.recv()).await {
            Ok(Ok(chunk)) => {
                if String::from_utf8_lossy(&chunk).contains("park-withheld-marker") {
                    break;
                }
            }
            Ok(Err(_)) => panic!("side output channel closed early"),
            Err(_) => panic!("timed out waiting for the marker on the side attachment"),
        }
    }

    // The parked connection must receive NO output (binary) frames and no
    // resync. Events still flow by design — cheap JSON like the `title`
    // frame the CI runner's bash PROMPT_COMMAND emits — so drain text
    // frames for the quiet window instead of asserting total silence.
    let quiet_until = tokio::time::Instant::now() + std::time::Duration::from_millis(300);
    loop {
        use futures::StreamExt;
        match tokio::time::timeout_at(quiet_until, socket.next()).await {
            Ok(Some(Ok(WsMessage::Binary(bytes)))) => {
                panic!(
                    "a parked connection must withhold output; got {} bytes",
                    bytes.len()
                );
            }
            Ok(Some(Ok(WsMessage::Text(text)))) => {
                let v = serde_json::from_str::<serde_json::Value>(&text).unwrap();
                assert_ne!(v["type"], "resync", "no repaint may start while parked");
            }
            Ok(other) => panic!("unexpected frame while parked: {other:?}"),
            Err(_) => break, // the quiet window elapsed with no output
        }
    }

    // Unpark: a parked attach has no grid yet, so the server repaints —
    // resync text frame, then the adjacent snapshot binary containing the
    // withheld output.
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "unpark"}).to_string(),
        ))
        .await
        .unwrap();
    // Events queued before the unpark was processed (e.g. a title change)
    // may precede the resync; the repaint's own resync + snapshot pair is
    // adjacent once it starts.
    loop {
        match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => {
                let v = serde_json::from_str::<serde_json::Value>(&text).unwrap();
                if v["type"] == "resync" {
                    break;
                }
            }
            other => panic!("expected text frames until resync, got {other:?}"),
        }
    }
    match next_ws_frame(&mut socket).await {
        WsMessage::Binary(bytes) => {
            let snapshot = String::from_utf8_lossy(&bytes).into_owned();
            assert!(
                snapshot.contains("park-withheld-marker"),
                "unpark snapshot must carry the withheld output; got: {snapshot:?}"
            );
        }
        other => panic!("expected snapshot binary after resync, got {other:?}"),
    }

    state.sessions.kill(&info.id).ok();
}

/// A live connection that parks mid-stream stops receiving output, and a
/// small backlog resumes from the ring on unpark (no repaint) — the frames
/// after unpark are plain binary chunks, not a resync.
#[tokio::test]
async fn ws_park_frame_stops_output_and_unpark_resumes_from_ring() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let info = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: test_dir("ws-park-resume-cwd"),
            name: None,
            cols: 80,
            rows: 24,
            command: None,
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .expect("spawn session");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://{addr}/ws/sessions/{}", info.id);
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();
    // Consume the visible handshake: ready + snapshot.
    match next_ws_frame(&mut socket).await {
        WsMessage::Text(_) => {}
        other => panic!("expected ready, got {other:?}"),
    }
    match next_ws_frame(&mut socket).await {
        WsMessage::Binary(_) => {}
        other => panic!("expected snapshot, got {other:?}"),
    }

    // Park, then produce a bounded burst of output.
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "park"}).to_string(),
        ))
        .await
        .unwrap();
    let mut side = state.sessions.attach(&info.id).expect("side attach");
    side.input
        .send(bytes::Bytes::from_static(b"echo ring-resume-marker\n"))
        .await
        .expect("side input");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, side.output.recv()).await {
            Ok(Ok(chunk)) => {
                if String::from_utf8_lossy(&chunk).contains("ring-resume-marker") {
                    break;
                }
            }
            Ok(Err(_)) => panic!("side output channel closed early"),
            Err(_) => panic!("timed out waiting for the marker on the side attachment"),
        }
    }

    // Unpark: a small backlog replays from the ring as ordinary binary
    // frames — no resync (the client's grid is current, nothing reflowed).
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "unpark"}).to_string(),
        ))
        .await
        .unwrap();
    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !String::from_utf8_lossy(&collected).contains("ring-resume-marker") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "ring replay never delivered the marker; got: {}",
            String::from_utf8_lossy(&collected)
        );
        match next_ws_frame(&mut socket).await {
            WsMessage::Binary(bytes) => collected.extend_from_slice(&bytes),
            WsMessage::Text(text) => {
                let v = serde_json::from_str::<serde_json::Value>(&text).unwrap();
                assert_ne!(
                    v["type"], "resync",
                    "a small ring backlog must resume, not repaint"
                );
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }

    state.sessions.kill(&info.id).ok();
}

/// A client resize round-trips through the off-reactor resize path: the
/// server winches the PTY + headless grid and broadcasts a `resized` event
/// back to every attachment (including the initiator, which uses it as its
/// dims echo). Guards the spawn_blocking resize seam in the bridge loop.
#[tokio::test]
async fn ws_resize_round_trip() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let info = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: test_dir("ws-resize-cwd"),
            name: None,
            cols: 80,
            rows: 24,
            command: None,
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .expect("spawn session");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://{addr}/ws/sessions/{}", info.id);
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();
    match next_ws_frame(&mut socket).await {
        WsMessage::Text(text) => {
            let ready: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(ready["type"], "ready");
            assert_eq!(ready["cols"], 80);
        }
        other => panic!("expected ready text frame, got {other:?}"),
    }
    match next_ws_frame(&mut socket).await {
        WsMessage::Binary(_) => {}
        other => panic!("expected snapshot binary frame, got {other:?}"),
    }

    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "resize", "cols": 100, "rows": 30}).to_string(),
        ))
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no resized event frame"
        );
        let frame = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            _ => continue, // shell output interleaves as binary frames
        };
        if frame["type"] == "resized" {
            assert_eq!(frame["cols"], 100);
            assert_eq!(frame["rows"], 30);
            break;
        }
    }
    assert_eq!(state.sessions.get(&info.id).map(|i| i.cols), Some(100));

    state.sessions.kill(&info.id).ok();
}

/// Attaching to a session that already died replays its final screen
/// (last words) and closes as exited — never a blank pane. This is the
/// fast-agent-failure path: codex without OPENAI_API_KEY printed its
/// error and exited before the client's tab could connect.
#[tokio::test]
async fn ws_attach_to_dead_session_replays_last_words() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let info = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: test_dir("ws-dead-cwd"),
            name: None,
            cols: 80,
            rows: 24,
            command: Some(vec![
                "/bin/bash".to_string(),
                "--norc".to_string(),
                "--noprofile".to_string(),
                "-c".to_string(),
                "echo Missing API key; exit 1".to_string(),
            ]),
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .expect("spawn session");

    // Wait for the fast death to unregister the session.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while state.sessions.get(&info.id).is_some() {
        assert!(tokio::time::Instant::now() < deadline, "session never died");
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://{addr}/ws/sessions/{}", info.id);
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();

    // ready (alive: false) -> final-screen binary -> exited, then close.
    let ready = match next_ws_frame(&mut socket).await {
        WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        other => panic!("expected ready text frame, got {other:?}"),
    };
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["alive"], false);
    let snapshot = match next_ws_frame(&mut socket).await {
        WsMessage::Binary(bytes) => bytes,
        other => panic!("expected last-words binary frame, got {other:?}"),
    };
    assert!(
        String::from_utf8_lossy(&snapshot).contains("Missing API key"),
        "final screen missing the process's output"
    );
    let exited = match next_ws_frame(&mut socket).await {
        WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        other => panic!("expected exited text frame, got {other:?}"),
    };
    assert_eq!(exited["type"], "exited");
    assert_eq!(exited["status"], 1);
}

#[tokio::test]
async fn ws_bad_token_is_rejected() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://{addr}/ws/sessions/s-00000000");
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "wrong"}).to_string(),
        ))
        .await
        .unwrap();

    let frame = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
        .await
        .expect("ws frame timeout")
        .expect("ws stream ended")
        .expect("ws frame error");
    match frame {
        WsMessage::Text(text) => {
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(json["type"], "error");
            assert_eq!(json["message"], "unauthorized");
        }
        other => panic!("expected error text frame, got {other:?}"),
    }
}

#[tokio::test]
async fn ws_events_pushes_files_touched_changes() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let id = inject_agent(&state, "k");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://{addr}/ws/events");
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();

    // Settings frame first (contract), then the initial snapshot: the
    // agent session with an empty touched list.
    let settings = match next_ws_frame(&mut socket).await {
        WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        other => panic!("expected settings text frame, got {other:?}"),
    };
    assert_eq!(settings["type"], "settings");
    let snapshot = match next_ws_frame(&mut socket).await {
        WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        other => panic!("expected sessions text frame, got {other:?}"),
    };
    let entry = snapshot["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .expect("agent session in snapshot");
    assert_eq!(entry["files_touched"], serde_json::json!([]));

    // A file touch nudges the bus: a fresh snapshot carries the path.
    let status = post_hook(
        &state,
        &id,
        "k",
        touch_payload("Write", "file_path", "/w/touched.rs"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no snapshot with the touched file"
        );
        let frame = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            _ => continue,
        };
        // settings/git frames interleave on this bus; only sessions matter here.
        if frame["type"] != "sessions" {
            continue;
        }
        let done =
            frame["sessions"].as_array().unwrap().iter().any(|s| {
                s["id"] == id && s["files_touched"] == serde_json::json!(["/w/touched.rs"])
            });
        if done {
            break;
        }
    }

    state.sessions.kill(&id).ok();
}

/// `/ws/events` watches only mounted file/listing paths, but those watches are
/// independent of Git: repeated writes to an already-dirty file and new output
/// in a non-repository directory both produce exact fs invalidations.
#[tokio::test]
async fn ws_events_pushes_mounted_disk_changes_outside_git() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let root = test_dir("ws-fs-watch");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("already-dirty.txt");
    std::fs::write(&file, b"first").unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://{addr}/ws/events");
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();

    // Drain the deterministic initial snapshots through recents, then register
    // a mounted file + visible directory (no workspace/Git required).
    loop {
        let frame = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            _ => continue,
        };
        if frame["type"] == "recents" {
            break;
        }
    }
    socket
        .send(WsMessage::text(
            serde_json::json!({
                "type": "watch",
                "workspace_id": null,
                "files": [file.to_string_lossy()],
                "dirs": [root.to_string_lossy()],
            })
            .to_string(),
        ))
        .await
        .unwrap();

    let initial = loop {
        let frame = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            _ => continue,
        };
        if frame["type"] == "fs" {
            break frame;
        }
    };
    assert_eq!(initial["files"], serde_json::json!([file]));
    assert_eq!(initial["dirs"], serde_json::json!([root]));

    // A second content change does not alter porcelain's M status; the mounted
    // metadata watch must still see it. A sibling create changes the listing.
    std::fs::write(&file, b"second-and-longer").unwrap();
    let child = root.join("ignored-output.bin");
    std::fs::write(&child, b"x").unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
    let changed = loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no fs frame for external changes"
        );
        let frame = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            _ => continue,
        };
        if frame["type"] == "fs" {
            break frame;
        }
    };
    assert_eq!(changed["files"], serde_json::json!([file]));
    assert_eq!(changed["dirs"], serde_json::json!([root]));

    std::fs::remove_file(&file).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no fs frame for external deletion"
        );
        let frame = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            _ => continue,
        };
        if frame["type"] == "fs" {
            assert_eq!(frame["removed"], serde_json::json!([file]));
            break;
        }
    }
}

/// A write the daemon hears about (`git::mark_path_dirty` — agent hooks,
/// chat edits, saves) reaches a window watching that exact file right away,
/// well inside the two-second poll ceiling the registration just started.
#[tokio::test]
async fn ws_events_pushes_daemon_observed_writes_without_waiting_for_the_poll() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let root = test_dir("ws-fs-touched");
    let file = root.join("agent-edited.md");
    std::fs::write(&file, b"before").unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let url = format!("ws://{addr}/ws/events");
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();
    let next_json = |text: &str| serde_json::from_str::<serde_json::Value>(text).unwrap();
    loop {
        if let WsMessage::Text(text) = next_ws_frame(&mut socket).await {
            if next_json(&text)["type"] == "recents" {
                break;
            }
        }
    }
    socket
        .send(WsMessage::text(
            serde_json::json!({
                "type": "watch",
                "workspace_id": null,
                "files": [file.to_string_lossy()],
                "dirs": [],
            })
            .to_string(),
        ))
        .await
        .unwrap();
    // The registration's baseline frame starts the poll ceiling.
    loop {
        if let WsMessage::Text(text) = next_ws_frame(&mut socket).await {
            if next_json(&text)["type"] == "fs" {
                break;
            }
        }
    }

    std::fs::write(&file, b"after, from an agent").unwrap();
    let marked = tokio::time::Instant::now();
    crate::git::mark_path_dirty(&state, &file.to_string_lossy()).await;
    let frame = loop {
        if let WsMessage::Text(text) = next_ws_frame(&mut socket).await {
            let frame = next_json(&text);
            if frame["type"] == "fs" {
                break frame;
            }
        }
    };
    assert_eq!(frame["files"], serde_json::json!([file]));
    assert!(
        marked.elapsed() < std::time::Duration::from_millis(1500),
        "fs frame took {:?} — the poll, not the fast path",
        marked.elapsed()
    );
}

#[tokio::test]
async fn ws_events_auth_snapshot_and_change_push() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let first = inject_agent(&state, "k"); // one agent session pre-existing

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://{addr}/ws/events");
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();

    // Settings frame first, then the initial full sessions snapshot.
    let settings = match next_ws_frame(&mut socket).await {
        WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        other => panic!("expected settings text frame, got {other:?}"),
    };
    assert_eq!(settings["type"], "settings");
    assert!(settings["settings"].is_object());
    let snapshot = match next_ws_frame(&mut socket).await {
        WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        other => panic!("expected sessions text frame, got {other:?}"),
    };
    assert_eq!(snapshot["type"], "sessions");
    let entry = snapshot["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == first)
        .expect("existing session in snapshot");
    assert_eq!(entry["kind"], "agent");
    assert_eq!(entry["agent_state"], "unknown");

    // A state change pushes a fresh snapshot.
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/agent-events/{first}?key=k"),
        Some(serde_json::json!({"hook_event_name": "Stop"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no snapshot with finished state"
        );
        let frame = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            _ => continue,
        };
        // settings/git frames interleave on this bus; only sessions matter here.
        if frame["type"] != "sessions" {
            continue;
        }
        let done = frame["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == first && s["agent_state"] == "finished");
        if done {
            break;
        }
    }

    // A disappearing session (killed PTY) is caught by the fallback tick.
    state.sessions.kill(&first).ok();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "killed session never left the snapshot"
        );
        let frame = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            _ => continue,
        };
        // settings/git frames interleave on this bus; only sessions matter here.
        if frame["type"] != "sessions" {
            continue;
        }
        let gone = !frame["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == first);
        if gone {
            break;
        }
    }
}

#[tokio::test]
async fn ws_events_pushes_settings_changes() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://{addr}/ws/events");
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();

    // Initial settings frame (empty map on a fresh daemon).
    let settings = match next_ws_frame(&mut socket).await {
        WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        other => panic!("expected settings text frame, got {other:?}"),
    };
    assert_eq!(settings["type"], "settings");
    assert_eq!(settings["settings"], serde_json::json!({}));

    // A PUT wakes the bus with the fresh map.
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({"appearance.theme": "dark"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no settings frame after PUT"
        );
        let frame = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            _ => continue,
        };
        if frame["type"] == "settings" {
            assert_eq!(frame["settings"]["appearance.theme"], "dark");
            break;
        }
    }
}

#[tokio::test]
async fn ws_events_bad_token_is_rejected() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let url = format!("ws://{addr}/ws/events");
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "wrong"}).to_string(),
        ))
        .await
        .unwrap();
    let frame = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
        .await
        .expect("ws frame timeout")
        .expect("ws stream ended")
        .expect("ws frame error");
    match frame {
        WsMessage::Text(text) => {
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(json["type"], "error");
            assert_eq!(json["message"], "unauthorized");
        }
        other => panic!("expected error text frame, got {other:?}"),
    }
}

/// The chat socket's send ids, end to end against one daemon: `ready` says
/// the daemon has them, a send's echo carries its client's id, the same id
/// sent again starts nothing, `cancel_send` loses against an accepted send
/// and wins against one that never arrived (which is then refused, named by
/// its id), every refusal of a send carries the id, and the journal keeps it
/// for the next attach.
#[tokio::test]
async fn ws_chat_sends_are_accepted_once_under_their_client_id() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    async fn next_json<S>(socket: &mut S) -> serde_json::Value
    where
        S: futures::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        loop {
            if let WsMessage::Text(text) = next_ws_frame(socket).await {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }
    /// The next frame that is not journal traffic (an `ev` or a `batch`).
    async fn next_answer<S>(socket: &mut S) -> serde_json::Value
    where
        S: futures::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        loop {
            let frame = next_json(socket).await;
            if frame["type"] != "ev" && frame["type"] != "batch" {
                return frame;
            }
        }
    }
    /// Every `user_message` in a frame, as (text, client_id).
    fn echoes(frame: &serde_json::Value) -> Vec<(String, Option<String>)> {
        frame["events"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| &entry["ev"])
            .chain([&frame["ev"]])
            .filter(|ev| ev["type"] == "user_message")
            .map(|ev| {
                (
                    ev["text"].as_str().unwrap_or_default().to_owned(),
                    ev["client_id"].as_str().map(str::to_owned),
                )
            })
            .collect()
    }
    fn send_as(text: &str, client_id: &str) -> WsMessage {
        WsMessage::text(
            serde_json::json!({
                "type":"send","blocks":[{"type":"text","text":text}],"client_id":client_id
            })
            .to_string(),
        )
    }
    fn cancel(client_id: &str) -> WsMessage {
        WsMessage::text(serde_json::json!({"type":"cancel_send","client_id":client_id}).to_string())
    }

    let state = test_state();
    let cwd = test_dir("ws-send-ids");
    let capture = cwd.join("agent-stdin.txt");
    let fake = write_fake_claude("ws-send-ids-agent");
    let script = std::fs::read_to_string(&fake).unwrap();
    std::fs::write(
        &fake,
        script.replace("cat >/dev/null", "cat > \"$CHIMAERA_TEST_CAPTURE\""),
    )
    .unwrap();
    let id = "s-send-ids".to_string();
    let mut spec = chimaera_agent::driver::SpawnSpec::new(
        id.clone(),
        vec![fake.to_string_lossy().into_owned()],
        cwd.clone(),
    );
    spec.env.push((
        "CHIMAERA_TEST_CAPTURE".into(),
        capture.to_string_lossy().into_owned(),
    ));
    state
        .chat
        .spawn(&chimaera_agent::claude::ClaudeAdapter, spec)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let connect = |query: &'static str| {
        let id = id.clone();
        async move {
            let (mut socket, _) =
                tokio_tungstenite::connect_async(format!("ws://{addr}/ws/chat/{id}{query}"))
                    .await
                    .unwrap();
            socket
                .send(WsMessage::text(
                    serde_json::json!({"type": "auth", "token": "test-token", "last_seq": 0})
                        .to_string(),
                ))
                .await
                .unwrap();
            socket
        }
    };
    let user_turns = |text: &str| {
        std::fs::read_to_string(&capture)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(r#""type":"user""#) && line.contains(text))
            .count()
    };

    let mut socket = connect("").await;
    let ready = next_json(&mut socket).await;
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["send_ids"], true, "{ready}");

    // One send, sent three times under one id, then a second message: the
    // journal holds the first once, with its id.
    for _ in 0..3 {
        socket.send(send_as("ONCE", "client-once-1")).await.unwrap();
    }
    socket.send(send_as("NEXT", "client-next-1")).await.unwrap();
    let mut seen = Vec::new();
    while !seen.iter().any(|(text, _)| text == "NEXT") {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "error", "{frame}");
        seen.extend(echoes(&frame));
    }
    assert_eq!(
        seen,
        [
            ("ONCE".to_string(), Some("client-once-1".to_string())),
            ("NEXT".to_string(), Some("client-next-1".to_string())),
        ]
    );

    // Too late to withdraw the accepted one; an id that never arrived is
    // withdrawn, and a send that then arrives under it is refused by name.
    socket.send(cancel("client-once-1")).await.unwrap();
    assert_eq!(
        next_answer(&mut socket).await,
        serde_json::json!({"type":"send_cancelled","client_id":"client-once-1","cancelled":false})
    );
    socket.send(cancel("client-gone-1")).await.unwrap();
    assert_eq!(
        next_answer(&mut socket).await,
        serde_json::json!({"type":"send_cancelled","client_id":"client-gone-1","cancelled":true})
    );
    socket.send(send_as("GONE", "client-gone-1")).await.unwrap();
    let refused = next_answer(&mut socket).await;
    assert_eq!(refused["type"], "error", "{refused}");
    assert_eq!(refused["code"], "command_failed", "{refused}");
    assert_eq!(refused["command"], "send", "{refused}");
    assert_eq!(refused["client_id"], "client-gone-1", "{refused}");

    // An id that is not well formed is refused and never echoed back; a
    // `cancel_send` without a usable id likewise.
    socket.send(send_as("BAD", "not an id")).await.unwrap();
    let refused = next_answer(&mut socket).await;
    assert_eq!(refused["code"], "invalid_command", "{refused}");
    assert_eq!(refused["command"], "send", "{refused}");
    assert!(refused.get("client_id").is_none(), "{refused}");
    socket.send(cancel("short")).await.unwrap();
    let refused = next_answer(&mut socket).await;
    assert_eq!(refused["code"], "invalid_command", "{refused}");
    assert_eq!(refused["command"], "cancel_send", "{refused}");

    // A socket that may not act: its refusal names the send too, and it can
    // withdraw nothing.
    let mut watching = connect("?read_only=true").await;
    assert_eq!(next_json(&mut watching).await["type"], "ready");
    watching
        .send(send_as("WATCHED", "client-watch-1"))
        .await
        .unwrap();
    let refused = next_answer(&mut watching).await;
    assert_eq!(refused["code"], "read_only", "{refused}");
    assert_eq!(refused["command"], "send", "{refused}");
    assert_eq!(refused["client_id"], "client-watch-1", "{refused}");
    watching.send(cancel("client-watch-2")).await.unwrap();
    let refused = next_answer(&mut watching).await;
    assert_eq!(refused["code"], "read_only", "{refused}");
    assert_eq!(refused["command"], "cancel_send", "{refused}");
    assert_eq!(
        state.chat.client_id_state(&id, "client-watch-2"),
        None,
        "a watching socket changes nothing"
    );

    // What the agent received (its turn never ends in this fixture, so the
    // second message waits in the driver), and what the next attach replays.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while user_turns("ONCE") == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "ONCE never delivered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(user_turns("ONCE"), 1);
    for never in ["GONE", "BAD", "WATCHED"] {
        assert_eq!(user_turns(never), 0, "{never}");
    }
    let mut again = connect("").await;
    assert_eq!(next_json(&mut again).await["type"], "ready");
    let mut replayed = Vec::new();
    while replayed.len() < 2 {
        replayed.extend(echoes(&next_json(&mut again).await));
    }
    assert_eq!(replayed, seen);
    state.chat.kill(&id);
}
