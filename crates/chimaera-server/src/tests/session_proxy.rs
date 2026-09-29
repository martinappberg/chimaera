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

#[tokio::test]
async fn retiring_stale_project_preserves_live_sibling_on_shared_host() {
    let remote = test_state();
    let local = test_state();
    let mut projects = Vec::new();
    for name in ["retired", "healthy"] {
        let workspace = lock(&remote.workspaces)
            .add(
                test_dir(&format!("shared-{name}-remote"))
                    .canonicalize()
                    .unwrap(),
            )
            .unwrap();
        let mut viewing = workspace.clone();
        viewing.root = test_dir(&format!("shared-{name}-local"))
            .canonicalize()
            .unwrap();
        lock(&local.workspaces)
            .import_exact(viewing.clone())
            .unwrap();
        projects.push((workspace, viewing));
    }
    let (healthy, viewing) = &projects[1];
    std::fs::write(
        healthy.root.join("proof.txt"),
        b"still on the current owner",
    )
    .unwrap();
    pro::install_execution_fixture(&remote, &healthy.id, 9).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let target = app(remote.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, target).await.unwrap();
    });
    for (workspace, _) in &projects {
        assert_eq!(
            request(
                &local,
                Method::POST,
                "/api/v1/pro/placements",
                Some(serde_json::json!({
                    "host_id":"worker-shared","endpoint":format!("http://{addr}"),
                    "token":"test-token","workspace_id":workspace.id,"epoch":9
                }))
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
    }
    let stale_query = crate::workspace_scope::paths::encode_query(&[(
        "path".into(),
        projects[0]
            .1
            .root
            .join("unavailable.txt")
            .to_string_lossy()
            .into_owned(),
    )]);
    let stale = app(local.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/fs/file?{stale_query}"))
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header("x-chimaera-viewer-workspace", &projects[0].0.id)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::SERVICE_UNAVAILABLE);
    let unauthenticated = app(local.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/pro/placements")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    let scoped = app(remote.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/pro/placements")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header("x-chimaera-workspace", &healthy.id)
                .header("x-chimaera-epoch", "9")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(scoped.status(), StatusCode::FORBIDDEN);
    // This read is also what a newly started native shell sees: no remembered
    // tunnel state is needed, and no token, URL or filesystem root is disclosed.
    // A preview the healthy project minted before its sibling is retired must
    // keep working afterwards: retirement is per project, not per host.
    let proof = viewing
        .root
        .join("proof.txt")
        .to_string_lossy()
        .into_owned();
    let minted = app(local.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/fs/ticket")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header("x-chimaera-viewer-workspace", &healthy.id)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"path":proof}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(minted.status(), StatusCode::OK);
    let minted: serde_json::Value =
        serde_json::from_slice(&minted.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let preview = format!("/raw/{}", minted["ticket"].as_str().unwrap());
    let (status, inventory) = request(&local, Method::GET, "/api/v1/pro/placements", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inventory.as_array().unwrap().len(), 2);
    for row in inventory.as_array().unwrap() {
        assert_eq!(row.as_object().unwrap().len(), 3);
        assert_eq!(row["host_id"], "worker-shared");
        assert_eq!(row["epoch"], 9);
        assert!(row["workspace_id"].is_string());
    }
    assert_eq!(
        request(
            &local,
            Method::DELETE,
            &format!("/api/v1/pro/placements?workspace_id={}", projects[0].0.id),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (_, inventory) = request(&local, Method::GET, "/api/v1/pro/placements", None).await;
    assert_eq!(
        inventory,
        serde_json::json!([{
            "host_id":"worker-shared","workspace_id":healthy.id,"epoch":9
        }])
    );
    let query = crate::workspace_scope::paths::encode_query(&[(
        "path".into(),
        viewing
            .root
            .join("proof.txt")
            .to_string_lossy()
            .into_owned(),
    )]);
    let response = app(local.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/fs/file?{query}"))
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header("x-chimaera-viewer-workspace", &healthy.id)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        b"still on the current owner"[..]
    );
    let response = app(local.clone())
        .oneshot(
            Request::builder()
                .uri(&preview)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "sibling preview survives"
    );
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        b"still on the current owner"[..]
    );
    local
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
    remote
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
    task.abort();
}

#[tokio::test]
async fn unavailable_logical_project_never_falls_back_to_stale_local_files() {
    let local = test_state();
    let workspace = lock(&local.workspaces)
        .add(test_dir("unavailable-local-copy").canonicalize().unwrap())
        .unwrap();
    let path = workspace.root.join("copy.txt");
    std::fs::write(&path, b"preserved local copy").unwrap();
    let query = crate::workspace_scope::paths::encode_query(&[(
        "path".into(),
        path.to_string_lossy().into_owned(),
    )]);
    let read = || {
        Request::builder()
            .uri(format!("/api/v1/fs/file?{query}"))
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header("x-chimaera-viewer-workspace", &workspace.id)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        app(local.clone()).oneshot(read()).await.unwrap().status(),
        StatusCode::OK,
        "ordinary non-Pro local file access stays unchanged"
    );
    pro::install_execution_fixture(&local, &workspace.id, 9).unwrap();
    assert_eq!(
        request(&local, Method::DELETE, "/api/v1/pro/configure", None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(!pro::may_execute(&local, &workspace.id));
    assert_eq!(
        app(local.clone()).oneshot(read()).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let save = Request::builder()
        .method(Method::PUT)
        .uri(format!("/api/v1/fs/file?{query}"))
        .header(header::AUTHORIZATION, "Bearer test-token")
        .header("x-chimaera-viewer-workspace", &workspace.id)
        .body(Body::from("must not silently save here"))
        .unwrap();
    assert_eq!(
        app(local.clone()).oneshot(save).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(std::fs::read(path).unwrap(), b"preserved local copy");
    local
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}
