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
    let mut local_workspace = workspace.clone();
    local_workspace.root = test_dir("local-placement").canonicalize().unwrap();
    lock(&local.workspaces)
        .import_exact(local_workspace.clone())
        .unwrap();
    std::fs::write(workspace.root.join("remote.txt"), b"remote file contents").unwrap();
    let (status, row) = request(
        &remote,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id":workspace.id,"cols":80,"rows":24})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    let id = row["id"].as_str().unwrap();
    pro::install_execution_fixture(&remote, &workspace.id, 4).unwrap();
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
    let path = local_workspace
        .root
        .join("remote.txt")
        .to_string_lossy()
        .into_owned();
    let query = crate::workspace_scope::paths::encode_query(&[("path".into(), path.clone())]);
    let response = app(local.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/fs/file?{query}"))
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header("x-chimaera-viewer-workspace", &workspace.id)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let version = response
        .headers()
        .get("x-mtime")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        b"remote file contents"[..]
    );
    let mut tickets = Vec::new();
    for _ in 0..2 {
        let response = app(local.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/fs/ticket")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header("x-chimaera-viewer-workspace", &workspace.id)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"path":path,"version":version}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        tickets.push(value["ticket"].as_str().unwrap().to_owned());
    }
    assert_eq!(
        tickets[0], tickets[1],
        "stable resource keeps its preview URL"
    );
    let response = app(local.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/raw/{}", tickets[0]))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        b"remote file contents"[..]
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

#[tokio::test]
async fn old_target_that_ignores_scope_never_receives_a_mutating_request() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let local = test_state();
    let workspace = lock(&local.workspaces)
        .add(test_dir("old-scope-target").canonicalize().unwrap())
        .unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    let count = writes.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route(
            "/api/v1/health",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"pid":123})) }),
        )
        .route(
            "/api/v1/fs/create",
            axum::routing::post(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }),
        );
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    assert_eq!(request(&local,Method::POST,"/api/v1/pro/placements",Some(serde_json::json!({"host_id":"device-old","endpoint":format!("http://{addr}"),"token":"synthetic","workspace_id":workspace.id,"epoch":4}))).await.0,StatusCode::NO_CONTENT);
    let response = app(local.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/fs/create")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header("x-chimaera-viewer-workspace", &workspace.id)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"path":workspace.root.join("not-created")}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert!(!workspace.root.join("not-created").exists());
    local.stopping.store(true, Ordering::Release);
    task.abort();
}
