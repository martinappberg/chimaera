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
async fn a_sleeping_owner_still_receives_mutations_and_passive_reads_never_wake_it() {
    use std::sync::Mutex;
    type Seen = Arc<Mutex<Vec<(String, Option<String>, Option<String>)>>>;
    // The transport answers for a suspended owner in both documented ways: a
    // cached health reply marked sleeping, or 503 worker_asleep.
    for cached_health in [true, false] {
        let local = test_state();
        let workspace = lock(&local.workspaces)
            .add(test_dir("sleeping-owner").canonicalize().unwrap())
            .unwrap();
        let seen: Seen = Arc::default();
        let record = |seen: Seen| {
            move |request: Request<Body>| {
                let seen = seen.clone();
                async move {
                    let header = |name: &str| {
                        request
                            .headers()
                            .get(name)
                            .map(|v| v.to_str().unwrap().to_owned())
                    };
                    seen.lock().unwrap().push((
                        format!("{} {}", request.method(), request.uri().path()),
                        header("x-chimaera-workspace"),
                        header("x-chimaera-wake"),
                    ));
                    if request.method() == Method::GET {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            axum::Json(serde_json::json!({"error":"worker_asleep"})),
                        )
                            .into_response();
                    }
                    axum::Json(serde_json::json!({"path":"/project/created.txt"})).into_response()
                }
            }
        };
        use axum::response::IntoResponse;
        let router = axum::Router::new()
            .route(
                "/api/v1/health",
                axum::routing::get(move || async move {
                    if cached_health {
                        let mut response = axum::Json(serde_json::json!({"pid":1})).into_response();
                        response
                            .headers_mut()
                            .insert("x-chimaera-worker-state", "sleeping".parse().unwrap());
                        response
                    } else {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            axum::Json(serde_json::json!({"error":"worker_asleep"})),
                        )
                            .into_response()
                    }
                }),
            )
            .route(
                "/api/v1/fs/create",
                axum::routing::post(record(seen.clone())),
            )
            .route("/api/v1/fs/file", axum::routing::get(record(seen.clone())));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        assert_eq!(request(&local,Method::POST,"/api/v1/pro/placements",Some(serde_json::json!({"host_id":"worker-asleep","endpoint":format!("http://{addr}"),"token":"synthetic","workspace_id":workspace.id,"epoch":4}))).await.0,StatusCode::NO_CONTENT);
        let created = workspace.root.join("created.txt");
        let response = app(local.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/fs/create")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header("x-chimaera-viewer-workspace", &workspace.id)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({"path":created}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a mutation reaches the owner"
        );
        let body: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["path"], created.to_string_lossy().as_ref());
        let query = crate::workspace_scope::paths::encode_query(&[(
            "path".into(),
            created.to_string_lossy().into_owned(),
        )]);
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
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![
                (
                    "POST /api/v1/fs/create".into(),
                    Some(workspace.id.clone()),
                    None
                ),
                (
                    "GET /api/v1/fs/file".into(),
                    Some(workspace.id.clone()),
                    None
                ),
            ],
            "both forwarded with scope; the proxy never adds a wake marker"
        );
        assert!(!created.exists(), "nothing is written to the local copy");
        local
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
        task.abort();
    }
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
    // Signing out keeps the laptop's own copy usable (laptop first).
    assert_eq!(
        request(&local, Method::DELETE, "/api/v1/pro/configure", None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(pro::may_execute(&local, &workspace.id));
    // Once another owner is verified, the local copy is stale.
    pro::install_remote_owner_fixture(&local, &workspace.id, 10);
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

/// A window whose project runs elsewhere still reads and saves this
/// computer's own files outside the project here, while the project's own
/// paths (and its document checker) go to the owner.
#[tokio::test]
async fn a_routed_window_keeps_this_computers_files_outside_the_project_local() {
    let remote = test_state();
    let local = test_state();
    let workspace = lock(&remote.workspaces)
        .add(test_dir("fs-owner").canonicalize().unwrap())
        .unwrap();
    std::fs::write(workspace.root.join("note.txt"), "owner copy").unwrap();
    let mut viewing = workspace.clone();
    viewing.root = test_dir("fs-viewer").canonicalize().unwrap();
    std::fs::write(viewing.root.join("note.txt"), "stale local copy").unwrap();
    lock(&local.workspaces)
        .import_exact(viewing.clone())
        .unwrap();
    pro::install_execution_fixture(&remote, &workspace.id, 4).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let router = app(remote.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    assert_eq!(request(&local,Method::POST,"/api/v1/pro/placements",Some(serde_json::json!({"host_id":"worker-fs","endpoint":format!("http://{remote_addr}"),"token":"test-token","workspace_id":workspace.id,"epoch":4}))).await.0,StatusCode::NO_CONTENT);
    let elsewhere = test_dir("fs-viewer-home").canonicalize().unwrap();
    let own = elsewhere.join("own.txt");
    std::fs::write(&own, "this computer's file").unwrap();
    let call = |method: Method, route: &str, path: &std::path::Path, body: Body| {
        let query = crate::workspace_scope::paths::encode_query(&[(
            "path".into(),
            path.to_string_lossy().into_owned(),
        )]);
        let request = Request::builder()
            .method(method)
            .uri(format!("/api/v1{route}?{query}"))
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header("x-chimaera-viewer-workspace", &workspace.id)
            .body(body)
            .unwrap();
        let local = local.clone();
        async move {
            let response = app(local).oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }
    };
    // The project's own file comes from where the project runs.
    let (status, body) = call(
        Method::GET,
        "/fs/file",
        &viewing.root.join("note.txt"),
        Body::empty(),
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "owner copy"));
    // This computer's own files and folders outside the project stay here.
    let (status, body) = call(Method::GET, "/fs/file", &own, Body::empty()).await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::OK, "this computer's file")
    );
    let (status, body) = call(Method::GET, "/fs/list", &elsewhere, Body::empty()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("own.txt"), "{body}");
    let (status, _) = call(
        Method::PUT,
        "/fs/file",
        &own,
        Body::from("saved on this computer"),
    )
    .await;
    assert!(status.is_success(), "{status}");
    assert_eq!(
        std::fs::read_to_string(&own).unwrap(),
        "saved on this computer"
    );
    // The reading view's checker works for a project file too.
    let (status, body) = call(
        Method::GET,
        "/fs/check_document",
        &viewing.root.join("note.txt"),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for state in [&remote, &local] {
        state
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// A window watching a project that runs on another daemon gets that
/// project's frames merged into its own events stream — never the owner's
/// settings, recents or notices — and its socket survives an owner change.
#[tokio::test]
async fn events_for_a_routed_project_merge_only_its_frames_and_survive_an_owner_change() {
    use std::sync::atomic::Ordering;
    let remote = test_state();
    let local = test_state();
    let workspace = lock(&remote.workspaces)
        .add(test_dir("events-remote").canonicalize().unwrap())
        .unwrap();
    let mut viewing = workspace.clone();
    viewing.root = test_dir("events-local").canonicalize().unwrap();
    lock(&local.workspaces)
        .import_exact(viewing.clone())
        .unwrap();
    let note = workspace.root.join("note.txt");
    std::fs::write(&note, "v0").unwrap();
    pro::install_execution_fixture(&remote, &workspace.id, 4).unwrap();
    // A session on the owner, so it can raise a notice of its own.
    let shell = remote
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: workspace.root.clone(),
            name: None,
            cols: 80,
            rows: 24,
            command: None,
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .unwrap()
        .id;
    lock(&remote.session_workspaces).insert(shell.clone(), workspace.id.clone());
    lock(&remote.agents).insert(
        shell.clone(),
        agents::AgentRecord::new("key".into(), agents::AgentKind::Claude),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let router = app(remote.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let register = |host: &'static str, epoch: u64| {
        let local = local.clone();
        let workspace = workspace.id.clone();
        async move {
            request(
                &local,
                Method::POST,
                "/api/v1/pro/placements",
                Some(serde_json::json!({
                    "host_id":host,"endpoint":format!("http://{remote_addr}"),
                    "token":"test-token","workspace_id":workspace,"epoch":epoch
                })),
            )
            .await
            .0
        }
    };
    assert_eq!(register("worker-events", 4).await, StatusCode::NO_CONTENT);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    let router = app(local.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{local_addr}/ws/events"))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            serde_json::json!({"type":"auth","token":"test-token"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let viewer_note = viewing.root.join("note.txt").to_string_lossy().into_owned();
    socket
        .send(Message::Text(
            serde_json::json!({"type":"watch","workspace_id":workspace.id,"files":[viewer_note],"dirs":[]})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    // Owner-side changes that must never reach this window...
    let (status, _) = request(
        &remote,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({"test.owner_marker":"owner-only"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    remote.recents_epoch.fetch_add(7, Ordering::Relaxed);
    // ...and one that must: the project's own file changing on its owner.
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut writes = 0;
    let mut noticed = false;
    'fs: loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no fs frame for the owner's change; saw {seen:?}"
        );
        writes += 1;
        std::fs::write(&note, format!("v{writes}")).unwrap();
        if !noticed {
            noticed =
                crate::notices::push_agent_notice(&remote, &shell, None, "owner notice").is_ok();
            remote.changes.notify_waiters();
        }
        let wait = tokio::time::Instant::now() + std::time::Duration::from_millis(700);
        while let Ok(Some(Ok(Message::Text(text)))) =
            tokio::time::timeout_at(wait, futures::StreamExt::next(&mut socket)).await
        {
            let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
            seen.push(frame["type"].as_str().unwrap_or_default().to_owned());
            assert_ne!(frame["type"], "error", "{frame}");
            assert_ne!(
                frame["type"], "notices",
                "owner notices never reach this window"
            );
            if frame["type"] == "settings" {
                assert!(
                    frame["settings"].get("test.owner_marker").is_none(),
                    "the owner's settings must not replace this window's"
                );
            }
            if frame["type"] == "recents" {
                assert_ne!(frame["epoch"], 7, "the owner's recents epoch leaked");
            }
            if frame["type"] == "fs"
                && frame["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|path| path == viewer_note.as_str())
            {
                break 'fs;
            }
        }
    }
    assert!(noticed, "the owner raised a notice while viewed");

    // The project changes owner. The window's own socket stays and keeps
    // serving this daemon's frames.
    assert_eq!(register("worker-other", 5).await, StatusCode::NO_CONTENT);
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let (status, _) = request(
        &local,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({"test.viewer_marker":"still-here"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "error", "{frame}");
        if frame["type"] == "settings" && frame["settings"]["test.viewer_marker"] == "still-here" {
            break;
        }
    }
    remote.sessions.kill(&shell).ok();
    for state in [&remote, &local] {
        state.stopping.store(true, Ordering::Release);
    }
}

/// A routed window with one tab outside the project (an upload, a note in the
/// home folder) keeps hearing about both: the project's file from its owner,
/// the outside file from this computer. The owner never sees the outside
/// path, and never closes the feed over it.
#[tokio::test]
async fn a_routed_window_with_an_outside_tab_hears_owner_and_local_changes() {
    let remote = test_state();
    let local = test_state();
    let workspace = lock(&remote.workspaces)
        .add(test_dir("mixed-remote").canonicalize().unwrap())
        .unwrap();
    let mut viewing = workspace.clone();
    viewing.root = test_dir("mixed-local").canonicalize().unwrap();
    lock(&local.workspaces)
        .import_exact(viewing.clone())
        .unwrap();
    let note = workspace.root.join("note.txt");
    std::fs::write(&note, "v0").unwrap();
    let outside = test_dir("mixed-outside")
        .canonicalize()
        .unwrap()
        .join("mine.txt");
    std::fs::write(&outside, "v0").unwrap();
    pro::install_execution_fixture(&remote, &workspace.id, 4).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let router = app(remote.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    assert_eq!(
        request(
            &local,
            Method::POST,
            "/api/v1/pro/placements",
            Some(serde_json::json!({
                "host_id":"worker-mixed","endpoint":format!("http://{remote_addr}"),
                "token":"test-token","workspace_id":workspace.id,"epoch":4
            })),
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    let router = app(local.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{local_addr}/ws/events"))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            serde_json::json!({"type":"auth","token":"test-token"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let viewer_note = viewing.root.join("note.txt").to_string_lossy().into_owned();
    let outside_path = outside.to_string_lossy().into_owned();
    socket
        .send(Message::Text(
            serde_json::json!({"type":"watch","workspace_id":workspace.id,
                "files":[viewer_note, outside_path],"dirs":[]})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let (mut owner_seen, mut local_seen) = (false, false);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut writes = 0;
    while !(owner_seen && local_seen) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "owner change seen: {owner_seen}, local change seen: {local_seen}"
        );
        writes += 1;
        std::fs::write(&note, format!("v{writes}")).unwrap();
        std::fs::write(&outside, format!("v{writes}")).unwrap();
        let wait = tokio::time::Instant::now() + std::time::Duration::from_millis(700);
        while let Ok(Some(Ok(Message::Text(text)))) =
            tokio::time::timeout_at(wait, futures::StreamExt::next(&mut socket)).await
        {
            let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_ne!(frame["type"], "error", "{frame}");
            if frame["type"] != "fs" {
                continue;
            }
            for path in frame["files"].as_array().unwrap() {
                owner_seen |= path == viewer_note.as_str();
                local_seen |= path == outside_path.as_str();
            }
        }
    }
    for state in [&remote, &local] {
        state
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// An in-process stand-in for the account transport in front of a real
/// daemon: while "asleep" it answers health for the owner (marked sleeping)
/// and refuses socket upgrades that carry no interaction, exactly as the
/// transport contract says. A wake-marked upgrade resumes the owner.
#[derive(Default)]
struct FakeTransport {
    asleep: std::sync::atomic::AtomicBool,
    refuse_wake: std::sync::atomic::AtomicBool,
    /// Keep a waking owner waking until the test releases it.
    hold_wake: std::sync::atomic::AtomicBool,
    release: tokio::sync::Notify,
    upgrades: std::sync::Mutex<Vec<String>>,
}
impl FakeTransport {
    fn serve(self: &Arc<Self>, daemon: Arc<AppState>) -> axum::Router {
        use axum::response::IntoResponse;
        use std::sync::atomic::Ordering;
        let transport = self.clone();
        app(daemon).layer(axum::middleware::from_fn(
            move |request: Request<Body>, next: axum::middleware::Next| {
                let transport = transport.clone();
                async move {
                    let path = request.uri().path().to_owned();
                    let query = request.uri().query().unwrap_or_default().to_owned();
                    if path.starts_with("/ws/") {
                        transport.upgrades.lock().unwrap().push(query.clone());
                        if transport.asleep.load(Ordering::Acquire) {
                            if !query.contains("wake=interaction")
                                || transport.refuse_wake.load(Ordering::Acquire)
                            {
                                return (
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    axum::Json(serde_json::json!({"error":"worker_asleep"})),
                                )
                                    .into_response();
                            }
                            if transport.hold_wake.load(Ordering::Acquire) {
                                transport.release.notified().await;
                            }
                            transport.asleep.store(false, Ordering::Release);
                        }
                    } else if path == "/api/v1/health" && transport.asleep.load(Ordering::Acquire) {
                        let mut response = axum::Json(serde_json::json!({"pid":1})).into_response();
                        response
                            .headers_mut()
                            .insert("x-chimaera-worker-state", "sleeping".parse().unwrap());
                        return response;
                    }
                    next.run(request).await
                }
            },
        ))
    }
}

struct SleepingChat {
    remote: Arc<AppState>,
    local: Arc<AppState>,
    workspace: String,
    id: String,
    capture: PathBuf,
    transport: Arc<FakeTransport>,
    transport_addr: std::net::SocketAddr,
    local_addr: std::net::SocketAddr,
}
impl Drop for SleepingChat {
    fn drop(&mut self) {
        self.remote.chat.kill(&self.id);
        self.remote.sessions.kill(&self.id).ok();
        for state in [&self.remote, &self.local] {
            state
                .stopping
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

/// A remote chat whose fake agent writes its stdin to `capture`, served
/// behind a sleeping fake transport, with a viewing daemon routed to it.
async fn sleeping_remote_chat(label: &str) -> SleepingChat {
    sleeping_remote(label, false).await
}

/// [`sleeping_remote_chat`], or a terminal whose process writes its input
/// to `capture`.
async fn sleeping_remote(label: &str, terminal: bool) -> SleepingChat {
    let remote = test_state();
    let local = test_state();
    let workspace = lock(&remote.workspaces)
        .add(test_dir(&format!("{label}-remote")).canonicalize().unwrap())
        .unwrap();
    let mut viewing = workspace.clone();
    viewing.root = test_dir(&format!("{label}-local")).canonicalize().unwrap();
    lock(&local.workspaces).import_exact(viewing).unwrap();
    let capture = workspace.root.join("agent-stdin.txt");
    let fake = write_fake_claude(&format!("{label}-agent"));
    let script = std::fs::read_to_string(&fake).unwrap();
    std::fs::write(
        &fake,
        script.replace("cat >/dev/null", "cat > \"$CHIMAERA_TEST_CAPTURE\""),
    )
    .unwrap();
    let id = format!("s-{label}");
    if terminal {
        remote
            .sessions
            .spawn(chimaera_pty::SpawnOpts {
                cwd: workspace.root.clone(),
                name: None,
                cols: 80,
                rows: 24,
                command: Some(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("cat > '{}'", capture.display()),
                ]),
                id: Some(id.clone()),
                env: Vec::new(),
                env_remove: Vec::new(),
                scrollback: None,
            })
            .unwrap();
    } else {
        let mut spec = chimaera_agent::driver::SpawnSpec::new(
            id.clone(),
            vec![fake.to_string_lossy().into_owned()],
            workspace.root.clone(),
        );
        spec.env.push((
            "CHIMAERA_TEST_CAPTURE".into(),
            capture.to_string_lossy().into_owned(),
        ));
        remote
            .chat
            .spawn(&chimaera_agent::claude::ClaudeAdapter, spec)
            .unwrap();
    }
    lock(&remote.session_workspaces).insert(id.clone(), workspace.id.clone());
    pro::install_execution_fixture(&remote, &workspace.id, 4).unwrap();
    let transport = Arc::new(FakeTransport::default());
    transport
        .asleep
        .store(true, std::sync::atomic::Ordering::Release);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let transport_addr = listener.local_addr().unwrap();
    let router = transport.serve(remote.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (status, error) = request(
        &local,
        Method::POST,
        "/api/v1/pro/placements",
        Some(serde_json::json!({
            "host_id":"worker-sleepy","endpoint":format!("http://{transport_addr}"),
            "token":"test-token","workspace_id":workspace.id,"epoch":4
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{error}");
    // The roster poll is a passive read: the transport answers it and the
    // session becomes routable without anything waking.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !session_view::sessions_json(&local)
        .iter()
        .any(|row| row["id"] == id.as_str())
    {
        assert!(tokio::time::Instant::now() < deadline, "row never appeared");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    let router = app(local.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    SleepingChat {
        remote,
        local,
        workspace: workspace.id,
        id,
        capture,
        transport,
        transport_addr,
        local_addr,
    }
}

async fn next_json<S>(socket: &mut S) -> serde_json::Value
where
    S: futures::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    loop {
        if let Message::Text(text) = next_ws_frame(socket).await {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn open_chat(
    fixture: &SleepingChat,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (mut socket, _) = tokio_tungstenite::connect_async(format!(
        "ws://{}/ws/chat/{}",
        fixture.local_addr, fixture.id
    ))
    .await
    .unwrap();
    socket
        .send(Message::Text(
            serde_json::json!({"type":"auth","token":"test-token","last_seq":0})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    socket
}

/// User turns the fake agent received carrying `text` (the driver's own
/// title request also quotes the text, so count only user messages).
fn user_turns(capture: &std::path::Path, text: &str) -> usize {
    std::fs::read_to_string(capture)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(r#""type":"user""#) && line.contains(text))
        .count()
}

fn send_text(text: &str) -> Message {
    Message::Text(
        serde_json::json!({"type":"send","blocks":[{"type":"text","text":text}]})
            .to_string()
            .into(),
    )
}

#[tokio::test]
async fn a_viewer_send_wakes_a_sleeping_owner_once_and_an_owner_change_says_moved() {
    let fixture = sleeping_remote_chat("wake-chat").await;
    let mut socket = open_chat(&fixture).await;
    // Opening the conversation is passive: the owner stays asleep.
    let first = next_json(&mut socket).await;
    assert_eq!(first["type"], "error");
    assert_eq!(first["code"], "worker_asleep");
    assert!(
        fixture.transport.upgrades.lock().unwrap().is_empty(),
        "attaching never tried to reach, or wake, the owner"
    );
    // The first real input carries wake intent and is delivered after ready.
    socket.send(send_text("WAKE_MESSAGE")).await.unwrap();
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "exited");
        assert_ne!(frame["code"], "command_failed", "{frame}");
        if frame["type"] == "ready" {
            break;
        }
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let delivered = user_turns(&fixture.capture, "WAKE_MESSAGE");
        if delivered > 0 {
            assert_eq!(delivered, 1, "delivered once");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "held send never delivered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        *fixture.transport.upgrades.lock().unwrap(),
        vec!["wake=interaction".to_string()],
        "one connection, opened by the interaction"
    );

    // The project moves to another owner under the established socket: the
    // viewer is told the session continues, never that it exited.
    let (status, _) = request(
        &fixture.local,
        Method::POST,
        "/api/v1/pro/placements",
        Some(serde_json::json!({
            "host_id":"worker-next","endpoint":format!("http://{}", fixture.transport_addr),
            "token":"test-token","workspace_id":fixture.workspace,"epoch":5
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "exited", "a move is not an exit");
        if frame["type"] == "moved" {
            assert_eq!(frame["to"], "cloud");
            break;
        }
    }
    assert_eq!(
        user_turns(&fixture.capture, "WAKE_MESSAGE"),
        1,
        "nothing is resent across the change"
    );
}

#[tokio::test]
async fn a_send_that_cannot_reach_the_owner_is_answered_not_dropped() {
    let fixture = sleeping_remote_chat("refused-chat").await;
    fixture
        .transport
        .refuse_wake
        .store(true, std::sync::atomic::Ordering::Release);
    let mut socket = open_chat(&fixture).await;
    assert_eq!(next_json(&mut socket).await["code"], "worker_asleep");
    socket.send(send_text("LOST_MESSAGE")).await.unwrap();
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "ready");
        if frame["code"] == "command_failed" {
            break;
        }
    }
    // The socket survives the refusal and says what it is waiting for.
    assert_eq!(next_json(&mut socket).await["code"], "remote_unavailable");
    assert_eq!(user_turns(&fixture.capture, "LOST_MESSAGE"), 0);
}

/// While a viewer's first send wakes the owner, the viewer is told so and a
/// second send is refused with a plain reason instead of being held: once
/// the owner answers, exactly one message is delivered.
#[tokio::test]
async fn a_second_send_while_waking_is_refused_not_queued() {
    use std::sync::atomic::Ordering;
    let fixture = sleeping_remote_chat("waking-chat").await;
    fixture.transport.hold_wake.store(true, Ordering::Release);
    let mut socket = open_chat(&fixture).await;
    assert_eq!(next_json(&mut socket).await["code"], "worker_asleep");
    socket.send(send_text("FIRST_SEND")).await.unwrap();
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "ready", "the owner is still waking");
        if frame["type"] == "waking" {
            break;
        }
    }
    socket.send(send_text("SECOND_SEND")).await.unwrap();
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "ready", "the owner is still waking");
        if frame["code"] == "command_failed" {
            assert_eq!(frame["reason"], "waking", "{frame}");
            break;
        }
    }
    fixture.transport.release.notify_one();
    loop {
        if next_json(&mut socket).await["type"] == "ready" {
            break;
        }
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while user_turns(&fixture.capture, "FIRST_SEND") == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "first send never delivered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(user_turns(&fixture.capture, "FIRST_SEND"), 1);
    assert_eq!(user_turns(&fixture.capture, "SECOND_SEND"), 0);
}

/// A terminal waking its owner holds the typing that woke it and refuses
/// the rest (with a plain note) until the owner answers: nothing typed
/// twice is ever run twice.
#[tokio::test]
async fn a_waking_terminal_holds_one_burst_and_refuses_the_rest() {
    use std::sync::atomic::Ordering;
    let fixture = sleeping_remote("waking-term", true).await;
    fixture.transport.hold_wake.store(true, Ordering::Release);
    let (mut socket, _) = tokio_tungstenite::connect_async(format!(
        "ws://{}/ws/sessions/{}",
        fixture.local_addr, fixture.id
    ))
    .await
    .unwrap();
    socket
        .send(Message::Text(
            serde_json::json!({"type":"auth","token":"test-token","cols":80,"rows":24})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert_eq!(next_json(&mut socket).await["code"], "worker_asleep");
    socket
        .send(Message::Binary(b"FIRST_BURST\r".to_vec().into()))
        .await
        .unwrap();
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "ready", "the owner is still waking");
        if frame["type"] == "waking" {
            break;
        }
    }
    socket
        .send(Message::Binary(b"SECOND_BURST\r".to_vec().into()))
        .await
        .unwrap();
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "ready", "the owner is still waking");
        if frame["type"] == "error" {
            assert_eq!(frame["reason"], "waking", "{frame}");
            break;
        }
    }
    fixture.transport.release.notify_one();
    loop {
        if next_json(&mut socket).await["type"] == "ready" {
            break;
        }
    }
    let captured = || std::fs::read_to_string(&fixture.capture).unwrap_or_default();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !captured().contains("FIRST_BURST") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "held typing never delivered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(captured().matches("FIRST_BURST").count(), 1);
    assert!(!captured().contains("SECOND_BURST"));
}
