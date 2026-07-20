use super::support::*;
use crate::*;

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
