use super::support::*;
use crate::*;
use futures::SinkExt;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn remote_session_preserves_identity_and_read_only_cannot_resize_or_type() {
    let remote = test_state();
    let local = test_state();
    let root = std::fs::canonicalize(test_dir("remote-placement")).unwrap();
    let workspace = lock(&remote.workspaces).add(root).unwrap();
    lock(&local.workspaces)
        .import_exact(workspace.clone())
        .unwrap();
    let (status, row) = request(
        &remote,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id":workspace.id,"cols":80,"rows":24})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    let id = row["id"].as_str().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let router = app(remote.clone());
    let remote_task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (status,error)=request(&local,Method::POST,"/api/v1/pro/placements",Some(serde_json::json!({"host_id":"worker-fixture","endpoint":format!("http://{remote_addr}"),"token":"test-token","workspace_id":workspace.id,"epoch":4}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{error}");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        if session_view::sessions_json(&local)
            .iter()
            .any(|r| r["id"] == id)
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        local.sessions.list().is_empty(),
        "forwarding does not create local sessions"
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    let router = app(local.clone());
    let local_task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (mut socket, _) = tokio_tungstenite::connect_async(format!(
        "ws://{local_addr}/ws/sessions/{id}?read_only=true&wake=interaction"
    ))
    .await
    .unwrap();
    socket
        .send(Message::Text(
            serde_json::json!({"type":"auth","token":"test-token","cols":140,"rows":45})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let ready = next_ws_frame(&mut socket).await;
    assert!(ready.to_text().unwrap().contains("ready"));
    assert_eq!(
        remote.sessions.get(id).map(|s| (s.cols, s.rows)),
        Some((80, 24)),
        "viewer auth never changes grid"
    );
    socket
        .send(Message::Text(
            serde_json::json!({"type":"resize","cols":150,"rows":50})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    socket
        .send(Message::Binary(bytes::Bytes::from_static(
            b"echo must-not-run\n",
        )))
        .await
        .unwrap();
    loop {
        let message = next_ws_frame(&mut socket).await;
        if let Message::Text(text) = message {
            if text.contains("read_only") {
                break;
            }
        }
    }
    assert_eq!(
        remote.sessions.get(id).map(|s| (s.cols, s.rows)),
        Some((80, 24))
    );
    let row = session_view::sessions_json(&remote)
        .into_iter()
        .find(|r| r["id"] == id)
        .unwrap();
    assert_eq!(row["last_input_ms"], serde_json::Value::Null);
    let (status, _) = request(
        &local,
        Method::DELETE,
        "/api/v1/pro/placements?host_id=worker-fixture",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let row = session_view::sessions_json(&local)
        .into_iter()
        .find(|r| r["id"] == id)
        .unwrap();
    assert_eq!(row["placement_available"], false);
    let (status, _) = request(
        &local,
        Method::POST,
        &format!("/api/v1/sessions/{id}/exec"),
        Some(serde_json::json!({"command":"true"})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    remote.sessions.kill(id).unwrap();
    remote_task.abort();
    local_task.abort();
    local
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
    remote
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}
