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
    // Both daemons share this test's filesystem, so the owner has a file at
    // this outside path too: the window says so instead of showing this
    // computer's same-named file in its place (the fake-owner test below
    // covers the owner having nothing there).
    let (status, body) = call(Method::GET, "/fs/file", &own, Body::empty()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("on_other_machine"), "{body}");
    assert!(!body.contains("this computer's file"), "{body}");
    // Saving where the folder exists on this computer stays here.
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

/// A routed project's Git status and Timeline pages carry the owner's epoch
/// in a range of their own (what its events nudges carry too), so switching
/// between this computer's copy and the owner always reads as a change.
#[tokio::test]
async fn a_routed_projects_epochs_never_collide_with_this_computers() {
    let remote = test_state();
    let local = test_state();
    let workspace = lock(&remote.workspaces)
        .add(test_dir("epochs-remote").canonicalize().unwrap())
        .unwrap();
    let mut viewing = workspace.clone();
    viewing.root = test_dir("epochs-local").canonicalize().unwrap();
    lock(&local.workspaces).import_exact(viewing).unwrap();
    pro::install_execution_fixture(&remote, &workspace.id, 4).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let router = app(remote.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let routes = [
        format!("/api/v1/git/status?workspace_id={}", workspace.id),
        format!("/api/v1/workspaces/{}/timeline", workspace.id),
    ];
    let id = workspace.id.clone();
    let epoch = move |state: Arc<AppState>, uri: String| {
        let id = id.clone();
        async move {
            let request = Request::builder()
                .uri(uri)
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header("x-chimaera-viewer-workspace", &id)
                .body(Body::empty())
                .unwrap();
            let response = app(state).oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["epoch"]
                .as_u64()
                .unwrap()
        }
    };
    let here: Vec<u64> =
        futures::future::join_all(routes.iter().map(|uri| epoch(local.clone(), uri.clone()))).await;
    assert_eq!(
        request(
            &local,
            Method::POST,
            "/api/v1/pro/placements",
            Some(serde_json::json!({
                "host_id":"worker-epochs","endpoint":format!("http://{remote_addr}"),
                "token":"test-token","workspace_id":workspace.id,"epoch":4
            })),
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    for (uri, local_epoch) in routes.iter().zip(here) {
        let owner = epoch(remote.clone(), uri.clone()).await;
        let routed = epoch(local.clone(), uri.clone()).await;
        assert_ne!(routed, local_epoch, "{uri}");
        assert!(routed > u64::from(u32::MAX), "{uri}: {routed}");
        assert_eq!(routed & 0xFFFF_FFFF, owner, "{uri}");
    }
    for state in [&remote, &local] {
        state
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// A routed window's read of a file outside the project: the owner's copy
/// when it may show it; this computer's own file when the owner has nothing
/// there; and when the owner has a different file at that path it may not
/// show, a plain answer — never this computer's same-named file in its place.
#[tokio::test]
async fn an_outside_read_the_owner_cannot_show_never_serves_this_computers_file() {
    use axum::{extract::Query, http::HeaderMap, response::IntoResponse, routing::get, Router};
    let local = test_state();
    let workspace = lock(&local.workspaces)
        .add(test_dir("other-machine-viewer").canonicalize().unwrap())
        .unwrap();
    let elsewhere = test_dir("other-machine-home").canonicalize().unwrap();
    for name in ["mine.txt", "theirs.txt", "old-owner.txt"] {
        std::fs::write(elsewhere.join(name), format!("this computer's {name}")).unwrap();
    }
    let owner = Router::new()
        .route(
            "/api/v1/health",
            get(|headers: HeaderMap| async move {
                let mut response = StatusCode::OK.into_response();
                for name in [
                    crate::workspace_scope::WORKSPACE_HEADER,
                    crate::workspace_scope::EPOCH_HEADER,
                ] {
                    response.headers_mut().insert(name, headers[name].clone());
                }
                response
                    .headers_mut()
                    .insert("x-chimaera-scope-version", "1".parse().unwrap());
                response
            }),
        )
        .route(
            "/api/v1/fs/file",
            get(
                |Query(query): Query<std::collections::HashMap<String, String>>| async move {
                    let path = query.get("path").cloned().unwrap_or_default();
                    let refuse = |status: StatusCode, error: &str| {
                        (status, axum::Json(serde_json::json!({ "error": error }))).into_response()
                    };
                    if path.starts_with("/project/") {
                        "owner copy".into_response()
                    } else if path.ends_with("theirs.txt") {
                        refuse(StatusCode::FORBIDDEN, "outside_project")
                    } else if path.ends_with("old-owner.txt") {
                        // An owner from before this distinction refuses alike.
                        refuse(StatusCode::FORBIDDEN, "workspace_scope_changed")
                    } else {
                        refuse(StatusCode::NOT_FOUND, "not_found")
                    }
                },
            ),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let owner_addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, owner).await.unwrap() });
    assert_eq!(
        request(
            &local,
            Method::POST,
            "/api/v1/pro/placements",
            Some(serde_json::json!({
                "host_id":"worker-fake","endpoint":format!("http://{owner_addr}"),
                "token":"fixture","workspace_id":workspace.id,"epoch":4
            })),
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let read = |path: std::path::PathBuf| {
        let query = crate::workspace_scope::paths::encode_query(&[(
            "path".into(),
            path.to_string_lossy().into_owned(),
        )]);
        let request = Request::builder()
            .uri(format!("/api/v1/fs/file?{query}"))
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header("x-chimaera-viewer-workspace", &workspace.id)
            .body(Body::empty())
            .unwrap();
        let local = local.clone();
        async move {
            let response = app(local).oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }
    };
    assert_eq!(
        read(workspace.root.join("note.txt")).await,
        (StatusCode::OK, "owner copy".to_owned())
    );
    assert_eq!(
        read(elsewhere.join("mine.txt")).await,
        (StatusCode::OK, "this computer's mine.txt".to_owned())
    );
    let (status, body) = read(elsewhere.join("theirs.txt")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"error":"on_other_machine"})
    );
    assert_eq!(
        read(elsewhere.join("old-owner.txt")).await,
        (StatusCode::OK, "this computer's old-owner.txt".to_owned())
    );
    local
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}

/// A window watching a project that runs on another daemon gets that
/// project's frames merged into its own events stream — never the owner's
/// settings or recents, and its notices only as this daemon's own (relayed
/// once into this daemon's feed: see the next test) — and its socket
/// survives an owner change.
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
            if frame["type"] == "notices" {
                // Only as this daemon's own relay of it: its ring holds it.
                let relayed = local.notices.since(0);
                assert!(
                    relayed.iter().any(|n| n.session_id == shell),
                    "{frame} did not come through this daemon's feed"
                );
            }
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
    assert!(
        local.notices.since(0).len() <= 1,
        "the owner's one notice is taken here at most once"
    );

    // The project changes owner. The window's own socket stays and keeps
    // serving this daemon's frames once its feed for the old owner has ended.
    let retired = local.session_proxy.feeds_retired.load(Ordering::Acquire);
    assert_eq!(register("worker-other", 5).await, StatusCode::NO_CONTENT);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while local.session_proxy.feeds_retired.load(Ordering::Acquire) == retired {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the old owner's feed never ended"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
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

/// A conversation running on the project's owner that needs a permission
/// reaches this computer's own notice feed (so the native app and browser
/// tabs alert about it) exactly once, however many windows watch the
/// project; it counts as an approval here while it waits and leaves the
/// count once answered on the owner.
#[tokio::test]
async fn a_routed_conversations_permission_reaches_this_computer_once() {
    use std::sync::atomic::Ordering;
    let remote = test_state();
    let local = test_state();
    let workspace = lock(&remote.workspaces)
        .add(test_dir("relay-remote").canonicalize().unwrap())
        .unwrap();
    let mut viewing = workspace.clone();
    viewing.root = test_dir("relay-local").canonicalize().unwrap();
    lock(&local.workspaces)
        .import_exact(viewing.clone())
        .unwrap();
    pro::install_execution_fixture(&remote, &workspace.id, 4).unwrap();
    let shell = remote
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: workspace.root.clone(),
            name: None,
            cols: 80,
            rows: 24,
            command: Some(vec!["sleep".into(), "600".into()]),
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
    tokio::spawn(crate::notices::run(remote.clone()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let router = app(remote.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (status, _) = request(
        &local,
        Method::POST,
        "/api/v1/pro/placements",
        Some(serde_json::json!({
            "host_id":"worker-relay","endpoint":format!("http://{remote_addr}"),
            "token":"test-token","workspace_id":workspace.id,"epoch":4
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    let router = app(local.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    // Two windows on the project: two feeds from the owner.
    let mut windows = Vec::new();
    for _ in 0..2 {
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{local_addr}/ws/events"))
                .await
                .unwrap();
        for frame in [
            serde_json::json!({"type":"auth","token":"test-token"}),
            serde_json::json!({"type":"watch","workspace_id":workspace.id,"files":[],"dirs":[]}),
        ] {
            socket
                .send(Message::Text(frame.to_string().into()))
                .await
                .unwrap();
        }
        windows.push(socket);
    }
    let (_, body) = request(&local, Method::GET, "/api/v1/notices", None).await;
    let boot = body["boot"].as_str().unwrap().to_owned();
    let head = body["head"].as_u64().unwrap();
    let poll = |after: u64| {
        let local = local.clone();
        let boot = boot.clone();
        async move {
            request(
                &local,
                Method::GET,
                &format!("/api/v1/notices?after={after}&boot={boot}&wait=0"),
                None,
            )
            .await
            .1
        }
    };
    // Both feeds are up once the owner's rows arrived here and they had a
    // moment to register; the watcher on the owner has its baseline.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while !local
        .session_proxy
        .rows()
        .iter()
        .any(|row| row["id"] == shell.as_str())
    {
        assert!(tokio::time::Instant::now() < deadline, "no routed rows");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    {
        let mut agents = lock(&remote.agents);
        let record = agents.get_mut(&shell).unwrap();
        record.state = crate::agent_state::AgentState::NeedsPermission;
        record.notice_note = Some(crate::agent_state::NoticeNote {
            text: "Bash: cargo publish".into(),
            question: false,
        });
    }
    remote.changes.notify_waiters();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let notice = loop {
        let body = poll(head).await;
        if let Some(notice) = body["notices"].as_array().and_then(|n| n.first()) {
            break notice.clone();
        }
        assert!(tokio::time::Instant::now() < deadline, "no relayed notice");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert_eq!(notice["kind"], "permission");
    assert_eq!(notice["blocking"], true);
    assert_eq!(notice["session_id"], shell.as_str());
    assert_eq!(notice["workspace_id"], workspace.id.as_str());
    assert_eq!(notice["body"], "Bash: cargo publish");
    assert_eq!(
        notice["subtitle"],
        format!("Needs permission · {}", viewing.name).as_str()
    );
    // Both windows' feeds carried it; this computer took it once.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let body = poll(head).await;
    assert_eq!(body["notices"].as_array().unwrap().len(), 1, "{body}");
    // In-app: a window's own events socket gets it as a notices frame.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    'frame: loop {
        assert!(tokio::time::Instant::now() < deadline, "no notices frame");
        let frame = next_json(&mut windows[0]).await;
        if frame["type"] == "notices" {
            assert_eq!(frame["notices"][0]["session_id"], shell.as_str());
            break 'frame;
        }
    }
    // While it waits it is an approval here (the Dock's count); answered
    // on the owner, it leaves the set, which takes the alert back.
    let waiting = |body: &serde_json::Value| {
        body["attention"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == shell.as_str())
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while !waiting(&poll(head).await) {
        assert!(tokio::time::Instant::now() < deadline, "not counted");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    lock(&remote.agents).get_mut(&shell).unwrap().state = crate::agent_state::AgentState::Running;
    remote.changes.notify_waiters();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    while waiting(&poll(head).await) {
        assert!(tokio::time::Instant::now() < deadline, "still counted");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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
///
/// With `front_door` it is the newer transport instead (VIEWING.md, "A
/// sleeping cloud machine's sockets"): it takes every socket upgrade itself
/// whether the owner sleeps or not, marks it `X-Chimaera-Sockets: kept`, and
/// keeps the viewer's side open across the owner's suspend and resume (see
/// [`FakeTransport::keep`]).
#[derive(Default)]
struct FakeTransport {
    asleep: std::sync::atomic::AtomicBool,
    refuse_wake: std::sync::atomic::AtomicBool,
    /// Keep a waking owner waking until the test releases it.
    hold_wake: std::sync::atomic::AtomicBool,
    release: tokio::sync::Notify,
    upgrades: std::sync::Mutex<Vec<String>>,
    front_door: std::sync::atomic::AtomicBool,
    /// The scope probe fails outright: the owner cannot be reached.
    fail_health: std::sync::atomic::AtomicBool,
    /// The daemon itself, for the front door's own attaches.
    daemon: std::sync::OnceLock<std::net::SocketAddr>,
    /// Sockets the front door took, by path.
    kept: std::sync::Mutex<Vec<String>>,
    /// Times the front door attached a kept socket to the daemon.
    attaches: std::sync::atomic::AtomicUsize,
    /// Frames a viewer sent after its first while the owner slept, as
    /// `(path, was binary)`: what the front door had to hold or drop.
    detached: std::sync::Mutex<Vec<(String, bool)>>,
    /// Scope probes (`/api/v1/health`) that reached the transport.
    probes: std::sync::atomic::AtomicUsize,
    /// Accept a socket upgrade and drop it at once.
    drop_accepted: std::sync::atomic::AtomicBool,
    /// Close every kept socket (the front door's side ends).
    close_kept: std::sync::atomic::AtomicBool,
    /// The authentication each kept terminal would attach with now.
    remembered: std::sync::Mutex<Vec<serde_json::Value>>,
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
                    if path == "/api/v1/health" {
                        transport.probes.fetch_add(1, Ordering::AcqRel);
                    }
                    if path.starts_with("/ws/") {
                        transport.upgrades.lock().unwrap().push(query.clone());
                        let front_door = transport.front_door.load(Ordering::Acquire);
                        let dropped = transport.drop_accepted.load(Ordering::Acquire);
                        if front_door || dropped {
                            use axum::extract::FromRequestParts;
                            let (mut parts, _) = request.into_parts();
                            let upgrade = axum::extract::WebSocketUpgrade::from_request_parts(
                                &mut parts,
                                &(),
                            )
                            .await
                            .unwrap();
                            let target = if query.is_empty() {
                                path.clone()
                            } else {
                                format!("{path}?{query}")
                            };
                            let mut response = if dropped {
                                upgrade.on_upgrade(|socket| async move { drop(socket) })
                            } else {
                                transport.kept.lock().unwrap().push(path);
                                upgrade
                                    .max_message_size(16 * 1024 * 1024)
                                    .on_upgrade(move |socket| transport.keep(socket, target))
                            };
                            // Every upgrade a keeping transport accepts says so.
                            if front_door {
                                response
                                    .headers_mut()
                                    .insert("x-chimaera-sockets", "kept".parse().unwrap());
                            }
                            return response;
                        }
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
                    } else if path == "/api/v1/health"
                        && transport.fail_health.load(Ordering::Acquire)
                    {
                        return StatusCode::BAD_GATEWAY.into_response();
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

    /// One viewer socket behind the front door. The first frame is its
    /// authentication and is remembered. Until the daemon has answered
    /// `ready` (asleep, waking, or just attached), what a chat sends and what
    /// a terminal types is held, and wakes a sleeping owner (unless
    /// `hold_wake`), which the viewer hears once as `waking`. A terminal's
    /// grid control is never held: it is folded into the remembered
    /// authentication; an events registration is neither held nor a reason
    /// to wake. Awake, the socket is attached with the remembered frame (a
    /// chat's `last_seq` raised to what this viewer already received) and,
    /// after the daemon's `ready` (an events socket has none), what was held
    /// is delivered once, in order. A suspension drops only the attach. When
    /// the viewer's side closes, what is still held is discarded.
    async fn keep(self: Arc<Self>, mut viewer: axum::extract::ws::WebSocket, target: String) {
        use axum::extract::ws::Message as Viewer;
        use futures::StreamExt;
        use std::sync::atomic::Ordering;
        let path = target.split('?').next().unwrap_or_default().to_owned();
        let chat = path.starts_with("/ws/chat/");
        let events = path == "/ws/events";
        let Some(Ok(Viewer::Text(first))) = viewer.recv().await else {
            return;
        };
        let mut auth: serde_json::Value = serde_json::from_str(&first).unwrap();
        let terminal = !chat && !events;
        let slot = terminal.then(|| {
            let mut remembered = self.remembered.lock().unwrap();
            remembered.push(auth.clone());
            remembered.len() - 1
        });
        let mut seen = 0u64;
        let mut held: std::collections::VecDeque<Message> = Default::default();
        let mut upstream = None;
        let mut ready = false;
        let mut said_waking = false;
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(20));
        loop {
            if self.close_kept.load(Ordering::Acquire) {
                return;
            }
            let asleep = self.asleep.load(Ordering::Acquire);
            if asleep && upstream.is_some() {
                upstream = None;
                ready = false;
                said_waking = false;
            }
            if !asleep && upstream.is_none() {
                let daemon = self.daemon.get().unwrap();
                let (mut socket, _) =
                    tokio_tungstenite::connect_async(format!("ws://{daemon}{target}"))
                        .await
                        .unwrap();
                if chat {
                    auth["last_seq"] =
                        serde_json::json!(seen.max(auth["last_seq"].as_u64().unwrap_or(0)));
                }
                socket
                    .send(Message::Text(auth.to_string().into()))
                    .await
                    .unwrap();
                self.attaches.fetch_add(1, Ordering::AcqRel);
                ready = events;
                upstream = Some(socket);
            }
            tokio::select! {
                _ = tick.tick() => {}
                frame = viewer.recv() => {
                    let frame = match frame {
                        Some(Ok(Viewer::Text(text))) => Message::Text(text.as_str().into()),
                        Some(Ok(Viewer::Binary(bytes))) => Message::Binary(bytes),
                        Some(Ok(Viewer::Close(_))) | None | Some(Err(_)) => return,
                        _ => continue,
                    };
                    if let (Some(slot), Message::Text(text)) = (slot, &frame) {
                        let control: serde_json::Value =
                            serde_json::from_str(text).unwrap_or_default();
                        match control["type"].as_str() {
                            Some("resize") => {
                                auth["cols"] = control["cols"].clone();
                                auth["rows"] = control["rows"].clone();
                            }
                            Some("park") => auth["parked"] = serde_json::json!(true),
                            Some("unpark") => auth["parked"] = serde_json::json!(false),
                            _ => {}
                        }
                        self.remembered.lock().unwrap()[slot] = auth.clone();
                    }
                    if let (Some(socket), true) = (upstream.as_mut(), ready) {
                        socket.send(frame).await.unwrap();
                        continue;
                    }
                    let binary = matches!(frame, Message::Binary(_));
                    self.detached.lock().unwrap().push((path.clone(), binary));
                    if !(chat || binary) {
                        continue;
                    }
                    held.push_back(frame);
                    if !self.hold_wake.load(Ordering::Acquire) {
                        self.asleep.store(false, Ordering::Release);
                    }
                    if !said_waking {
                        said_waking = true;
                        let waking = serde_json::json!({"type":"waking"}).to_string();
                        if viewer.send(Viewer::Text(waking.into())).await.is_err() {
                            return;
                        }
                    }
                }
                frame = async { upstream.as_mut().unwrap().next().await }, if upstream.is_some() => {
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            let value: serde_json::Value =
                                serde_json::from_str(&text).unwrap_or_default();
                            for event in value["events"].as_array().into_iter().flatten().chain([&value]) {
                                seen = seen.max(event["seq"].as_u64().unwrap_or(0));
                            }
                            if viewer.send(Viewer::Text(text.as_str().into())).await.is_err() {
                                return;
                            }
                            if value["type"] == "ready" && !ready {
                                ready = true;
                                let socket = upstream.as_mut().unwrap();
                                for frame in held.drain(..) {
                                    socket.send(frame).await.unwrap();
                                }
                            }
                        }
                        Some(Ok(Message::Binary(bytes))) => {
                            if viewer.send(Viewer::Binary(bytes)).await.is_err() {
                                return;
                            }
                        }
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                        _ => {}
                    }
                }
            }
        }
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
    // The daemon without the transport in front: where a front door attaches.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    transport
        .daemon
        .set(listener.local_addr().unwrap())
        .unwrap();
    let router = app(remote.clone());
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
    // The one attach it tried carried no wake intent, and this transport
    // (which keeps no sockets for a sleeping owner) refused it.
    assert_eq!(
        *fixture.transport.upgrades.lock().unwrap(),
        vec![String::new()],
        "attaching never tried to wake the owner"
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
        vec![String::new(), "wake=interaction".to_string()],
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
            // Tagged, so the viewer hands back only a refused send's text.
            assert_eq!(frame["command"], "send", "{frame}");
            break;
        }
    }
    // The socket survives the refusal and says what it is waiting for: the
    // relay's own lasting state, marked so (a keeping transport's hand-back
    // uses the same code without the reason).
    let waiting = next_json(&mut socket).await;
    assert_eq!(waiting["code"], "remote_unavailable");
    assert_eq!(waiting["reason"], "reconnecting");
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
            assert_eq!(frame["command"], "send", "{frame}");
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
            // Where the refused typing would have run: a cloud machine.
            assert_eq!(frame["owner"], "cloud", "{frame}");
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

/// A permission (or question) answered from another device while the owner
/// is suspended wakes it exactly like a send: the answer carries wake intent.
#[tokio::test]
async fn a_permission_answer_to_a_suspended_owner_wakes_it() {
    let fixture = sleeping_remote_chat("wake-permission").await;
    let mut socket = open_chat(&fixture).await;
    assert_eq!(next_json(&mut socket).await["code"], "worker_asleep");
    assert_eq!(
        *fixture.transport.upgrades.lock().unwrap(),
        vec![String::new()]
    );
    socket
        .send(Message::Text(
            serde_json::json!({"type":"permission","request_id":"ask-1","option_id":"allow_once"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    loop {
        let frame = next_json(&mut socket).await;
        if frame["type"] == "ready" {
            break;
        }
    }
    assert_eq!(
        *fixture.transport.upgrades.lock().unwrap(),
        vec![String::new(), "wake=interaction".to_string()],
        "the answer opened the connection with wake intent"
    );
}

/// Frames until the socket has been quiet for `quiet`; the socket must still
/// be open then.
async fn until_quiet<S>(socket: &mut S, quiet: std::time::Duration) -> Vec<serde_json::Value>
where
    S: futures::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures::StreamExt;
    let mut frames = Vec::new();
    loop {
        match tokio::time::timeout(quiet, socket.next()).await {
            Err(_) => return frames,
            Ok(Some(Ok(Message::Text(text)))) => frames.push(serde_json::from_str(&text).unwrap()),
            Ok(Some(Ok(Message::Close(frame)))) => panic!("the socket closed: {frame:?}"),
            Ok(Some(Ok(_))) => {}
            Ok(other) => panic!("the socket ended: {other:?}"),
        }
    }
}

/// `user_message` echoes of `text` in one chat frame (`ev`, or a replay `batch`).
fn echoes(frame: &serde_json::Value, text: &str) -> usize {
    frame["events"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| &entry["ev"])
        .chain([&frame["ev"]])
        .filter(|ev| ev["type"] == "user_message" && ev["text"] == text)
        .count()
}

async fn delivered_once(capture: &std::path::Path, text: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while user_turns(capture, text) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{text} never delivered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(user_turns(capture, text), 1, "{text} delivered once");
}

/// A transport that keeps a sleeping cloud machine's sockets: the relay
/// attaches at once (passively), says nothing to the viewer, holds nothing
/// itself and passes the viewer's frames straight through; the transport
/// wakes the owner and delivers them. The viewer's socket then stays open
/// across the owner's next suspension and resume.
#[tokio::test]
async fn a_transport_that_keeps_a_sleeping_owners_socket_gets_the_viewers_frames_at_once() {
    use std::sync::atomic::Ordering;
    let quiet = std::time::Duration::from_millis(600);
    let fixture = sleeping_remote_chat("front-door-chat").await;
    fixture.transport.front_door.store(true, Ordering::Release);
    let mut socket = open_chat(&fixture).await;
    // Opening is passive and quiet: no "asleep", no "reconnecting", nothing.
    assert_eq!(
        until_quiet(&mut socket, quiet).await,
        Vec::<serde_json::Value>::new()
    );
    assert_eq!(
        *fixture.transport.upgrades.lock().unwrap(),
        vec![String::new()],
        "one attach, without wake intent"
    );
    assert!(
        fixture.transport.asleep.load(Ordering::Acquire),
        "viewing woke nothing"
    );
    assert_eq!(fixture.transport.attaches.load(Ordering::Acquire), 0);

    // Two sends while it sleeps and wakes: both reach the transport at once
    // and neither is refused by the relay.
    socket.send(send_text("FIRST_THROUGH")).await.unwrap();
    socket.send(send_text("SECOND_THROUGH")).await.unwrap();
    let mut waking = 0;
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "error", "{frame}");
        if frame["type"] == "waking" {
            waking += 1;
        }
        if frame["type"] == "ready" {
            break;
        }
    }
    assert_eq!(waking, 1, "the transport's one status, passed through");
    // The agent got the first; the second waits its turn on the owner (a
    // mid-turn send), which its echo proves.
    delivered_once(&fixture.capture, "FIRST_THROUGH").await;
    let mut echoed = (0, 0);
    while echoed != (1, 1) {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "error", "{frame}");
        echoed.0 += echoes(&frame, "FIRST_THROUGH");
        echoed.1 += echoes(&frame, "SECOND_THROUGH");
    }
    assert_eq!(
        *fixture.transport.detached.lock().unwrap(),
        vec![
            (format!("/ws/chat/{}", fixture.id), false),
            (format!("/ws/chat/{}", fixture.id), false)
        ],
        "the relay held neither send back"
    );

    // The owner suspends: the transport drops only its own attach, and the
    // viewer's socket stays open and quiet.
    until_quiet(&mut socket, quiet).await;
    fixture.transport.asleep.store(true, Ordering::Release);
    assert_eq!(
        until_quiet(&mut socket, quiet).await,
        Vec::<serde_json::Value>::new()
    );

    // A send on that same socket wakes it again; the second `ready` and the
    // replayed gap pass through and the message is delivered once.
    socket.send(send_text("AFTER_SUSPEND")).await.unwrap();
    let (mut readies, mut echoed) = (0, 0);
    while echoed == 0 {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "error", "{frame}");
        readies += usize::from(frame["type"] == "ready");
        echoed += echoes(&frame, "AFTER_SUSPEND");
        assert_eq!(
            echoes(&frame, "FIRST_THROUGH"),
            0,
            "only the gap is replayed"
        );
    }
    assert_eq!(readies, 1, "delivered after the second `ready`");
    assert_eq!(user_turns(&fixture.capture, "FIRST_THROUGH"), 1);
    assert_eq!(fixture.transport.attaches.load(Ordering::Acquire), 2);
    assert_eq!(
        *fixture.transport.upgrades.lock().unwrap(),
        vec![String::new()],
        "the relay never redialed"
    );
}

/// The same for a terminal: typing goes to the transport at once and none
/// of it is refused while the owner wakes; grid control goes to the
/// transport too, which holds none of it and attaches with the grid it names.
#[tokio::test]
async fn a_kept_terminal_takes_typing_at_once_and_refuses_none_of_it() {
    use std::sync::atomic::Ordering;
    let fixture = sleeping_remote("front-door-term", true).await;
    fixture.transport.front_door.store(true, Ordering::Release);
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
    assert_eq!(
        until_quiet(&mut socket, std::time::Duration::from_millis(600)).await,
        Vec::<serde_json::Value>::new()
    );
    socket
        .send(Message::Text(
            serde_json::json!({"type":"resize","cols":100,"rows":30})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    socket
        .send(Message::Binary(b"FIRST_BURST\r".to_vec().into()))
        .await
        .unwrap();
    socket
        .send(Message::Binary(b"SECOND_BURST\r".to_vec().into()))
        .await
        .unwrap();
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "error", "{frame}");
        if frame["type"] == "ready" {
            break;
        }
    }
    let captured = || std::fs::read_to_string(&fixture.capture).unwrap_or_default();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !captured().contains("SECOND_BURST") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "typing never delivered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let captured = captured();
    assert_eq!(captured.matches("FIRST_BURST").count(), 1);
    assert!(
        captured.find("FIRST_BURST") < captured.find("SECOND_BURST"),
        "in order"
    );
    let path = format!("/ws/sessions/{}", fixture.id);
    assert_eq!(
        *fixture.transport.detached.lock().unwrap(),
        vec![(path.clone(), false), (path.clone(), true), (path, true)],
        "the resize and both bursts reached the transport before the owner answered"
    );
    let remembered = fixture.transport.remembered.lock().unwrap()[0].clone();
    assert_eq!(
        (remembered["cols"].as_u64(), remembered["rows"].as_u64()),
        (Some(100), Some(30))
    );
}

/// A window watching a sleeping cloud project: its feed attaches passively
/// to a transport that keeps the socket and sends nothing it would have to
/// hold. When the owner wakes the same feed registers the window's paths,
/// and it registers them again after the next suspension, because a
/// registration lives on the owner's side of one attach.
#[tokio::test]
async fn a_kept_feed_registers_when_the_owner_wakes_and_again_after_each_suspension() {
    use std::sync::atomic::Ordering;
    let fixture = sleeping_remote_chat("front-door-feed").await;
    fixture.transport.front_door.store(true, Ordering::Release);
    let owner_root = lock(&fixture.remote.workspaces)
        .get(&fixture.workspace)
        .unwrap()
        .root;
    let viewer_root = lock(&fixture.local.workspaces)
        .get(&fixture.workspace)
        .unwrap()
        .root;
    let note = owner_root.join("note.txt");
    std::fs::write(&note, "v0").unwrap();
    let viewer_note = viewer_root.join("note.txt").to_string_lossy().into_owned();
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{}/ws/events", fixture.local_addr))
            .await
            .unwrap();
    for frame in [
        serde_json::json!({"type":"auth","token":"test-token"}),
        serde_json::json!({"type":"watch","workspace_id":fixture.workspace,"files":[viewer_note],"dirs":[]}),
    ] {
        socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
    }
    let kept = || {
        fixture
            .transport
            .kept
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.as_str() == "/ws/events")
            .count()
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while kept() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the feed never attached"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert!(
        fixture.transport.asleep.load(Ordering::Acquire),
        "watching woke nothing"
    );
    assert!(
        fixture.transport.detached.lock().unwrap().is_empty(),
        "nothing was sent for the transport to hold"
    );

    // The owner's change reaches the window once it is awake: its paths were
    // registered by the same feed. `rounds` suspends and resumes once more.
    let mut writes = 0;
    for round in 0..2 {
        fixture.transport.asleep.store(false, Ordering::Release);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        'fs: loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no fs frame after wake {round}"
            );
            writes += 1;
            std::fs::write(&note, format!("v{writes}")).unwrap();
            let wait = tokio::time::Instant::now() + std::time::Duration::from_millis(700);
            while let Ok(Some(Ok(Message::Text(text)))) =
                tokio::time::timeout_at(wait, futures::StreamExt::next(&mut socket)).await
            {
                let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_ne!(frame["type"], "error", "{frame}");
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
        assert_eq!(
            fixture.transport.attaches.load(Ordering::Acquire),
            round + 1
        );
        fixture.transport.asleep.store(true, Ordering::Release);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    assert_eq!(kept(), 1, "one feed, kept open across the suspension");
}

/// A transport that keeps no sockets for a sleeping owner refuses the feed's
/// passive attach: the feed ends and is retried without another attach, the
/// window's own socket stays, and nothing asks the owner to wake.
#[tokio::test]
async fn a_refused_feed_for_a_sleeping_owner_leaves_the_window_connected() {
    use std::sync::atomic::Ordering;
    let fixture = sleeping_remote_chat("refused-feed").await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{}/ws/events", fixture.local_addr))
            .await
            .unwrap();
    for frame in [
        serde_json::json!({"type":"auth","token":"test-token"}),
        serde_json::json!({"type":"watch","workspace_id":fixture.workspace,"files":[],"dirs":[]}),
    ] {
        socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while fixture.transport.upgrades.lock().unwrap().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the feed never tried"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (status, _) = request(
        &fixture.local,
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
    assert!(fixture.transport.asleep.load(Ordering::Acquire));
    // The refusal is remembered for the host: the feed's retries (after one
    // second, then two) and a viewer's socket cost no further upgrade.
    tokio::time::sleep(std::time::Duration::from_millis(3500)).await;
    let mut chat = open_chat(&fixture).await;
    assert_eq!(next_json(&mut chat).await["code"], "worker_asleep");
    assert_eq!(
        *fixture.transport.upgrades.lock().unwrap(),
        vec![String::new()],
        "one passive attach, without wake intent, and no more"
    );
}

/// A viewer already told the owner cannot be reached is not left saying so
/// once a keeping transport takes the socket: nothing takes "reconnecting"
/// back before `ready`, which a sleeping owner does not send. The relay ends
/// that socket instead, and the viewer's reconnect attaches quietly.
#[tokio::test]
async fn a_viewer_told_reconnecting_is_reconnected_into_the_kept_socket() {
    use futures::StreamExt;
    use std::sync::atomic::Ordering;
    let fixture = sleeping_remote_chat("front-door-late").await;
    fixture.transport.front_door.store(true, Ordering::Release);
    fixture.transport.fail_health.store(true, Ordering::Release);
    let mut socket = open_chat(&fixture).await;
    assert_eq!(next_json(&mut socket).await["code"], "remote_unavailable");
    fixture
        .transport
        .fail_health
        .store(false, Ordering::Release);
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
            .await
            .expect("the socket never ended")
        {
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
            Some(Ok(Message::Text(text))) => panic!("unexpected frame {text}"),
            Some(Ok(_)) => {}
        }
    }
    let mut socket = open_chat(&fixture).await;
    assert_eq!(
        until_quiet(&mut socket, std::time::Duration::from_millis(600)).await,
        Vec::<serde_json::Value>::new()
    );
    assert!(fixture.transport.asleep.load(Ordering::Acquire));
}

/// A scope answer cached while the machine was awake says nothing about now:
/// a viewer attaching seconds after it suspended reaches a keeping transport
/// through that stale "awake", and its send must still go straight to the
/// transport (which wakes the machine), not into this relay's own hold where
/// nothing would ever ask for a wake.
#[tokio::test]
async fn a_stale_awake_answer_still_passes_input_to_a_keeping_transport() {
    use std::sync::atomic::Ordering;
    let quiet = std::time::Duration::from_millis(600);
    let fixture = sleeping_remote_chat("front-door-stale").await;
    fixture.transport.front_door.store(true, Ordering::Release);
    // Awake: the first viewer's attach leaves a fresh scope acknowledgment.
    fixture.transport.asleep.store(false, Ordering::Release);
    let mut first = open_chat(&fixture).await;
    while next_json(&mut first).await["type"] != "ready" {}
    drop(first);
    // It suspends; the next viewer arrives well inside the cached answer.
    fixture.transport.asleep.store(true, Ordering::Release);
    let probes = fixture.transport.probes.load(Ordering::Acquire);
    let mut socket = open_chat(&fixture).await;
    assert_eq!(
        until_quiet(&mut socket, quiet).await,
        Vec::<serde_json::Value>::new()
    );
    assert_eq!(
        fixture.transport.probes.load(Ordering::Acquire),
        probes,
        "the attach rode the cached answer"
    );
    socket.send(send_text("STALE_AWAKE")).await.unwrap();
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["type"], "error", "{frame}");
        if frame["type"] == "ready" {
            break;
        }
    }
    delivered_once(&fixture.capture, "STALE_AWAKE").await;
    assert!(
        fixture
            .transport
            .detached
            .lock()
            .unwrap()
            .contains(&(format!("/ws/chat/{}", fixture.id), false)),
        "the send reached the transport while nothing was attached"
    );
}

/// A transport that accepts a passive attach to a sleeping machine without
/// saying it keeps sockets, and then drops it, keeps none: the viewer's
/// socket ends once, and from then on that host is on the hold-and-wake
/// path without another passive attempt.
#[tokio::test]
async fn a_transport_that_accepts_and_drops_is_remembered_as_keeping_no_sockets() {
    use futures::StreamExt;
    use std::sync::atomic::Ordering;
    let fixture = sleeping_remote_chat("accept-then-close").await;
    fixture
        .transport
        .drop_accepted
        .store(true, Ordering::Release);
    let mut socket = open_chat(&fixture).await;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
            .await
            .expect("the socket never ended")
        {
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
            Some(Ok(Message::Text(text))) => panic!("unexpected frame {text}"),
            Some(Ok(_)) => {}
        }
    }
    fixture
        .transport
        .drop_accepted
        .store(false, Ordering::Release);
    let mut socket = open_chat(&fixture).await;
    assert_eq!(next_json(&mut socket).await["code"], "worker_asleep");
    assert_eq!(
        *fixture.transport.upgrades.lock().unwrap(),
        vec![String::new()],
        "no second passive attempt"
    );
    socket.send(send_text("AFTER_DROP")).await.unwrap();
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["code"], "command_failed", "{frame}");
        if frame["type"] == "ready" {
            break;
        }
    }
    delivered_once(&fixture.capture, "AFTER_DROP").await;
}

/// Input a keeping transport still holds when the socket ends is discarded
/// there, never delivered later: this relay closes the viewer's socket (its
/// client hands the text back when the next `ready` replays no echo). Both
/// ways a passed-through socket ends: the transport's side closing, and the
/// project changing owner under it (which also says `moved`).
#[tokio::test]
async fn passed_through_input_is_never_delivered_after_its_socket_ended() {
    use futures::StreamExt;
    use std::sync::atomic::Ordering;
    for moved in [false, true] {
        let fixture = sleeping_remote_chat(if moved {
            "through-moved"
        } else {
            "through-closed"
        })
        .await;
        fixture.transport.front_door.store(true, Ordering::Release);
        fixture.transport.hold_wake.store(true, Ordering::Release);
        let mut socket = open_chat(&fixture).await;
        socket.send(send_text("NEVER_LATE")).await.unwrap();
        assert_eq!(next_json(&mut socket).await["type"], "waking");
        if moved {
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
        } else {
            fixture.transport.close_kept.store(true, Ordering::Release);
        }
        let mut frames = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
                .await
                .expect("the socket never ended")
            {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(Message::Text(text))) => {
                    frames.push(serde_json::from_str::<serde_json::Value>(&text).unwrap())
                }
                Some(Ok(_)) => {}
            }
        }
        let types: Vec<_> = frames
            .iter()
            .map(|frame| frame["type"].as_str().unwrap_or_default().to_owned())
            .collect();
        let expected: Vec<String> = if moved { vec!["moved".into()] } else { vec![] };
        assert_eq!(types, expected, "{frames:?}");
        // The machine wakes afterwards: nothing held for the ended socket
        // reaches it.
        fixture.transport.close_kept.store(false, Ordering::Release);
        fixture.transport.hold_wake.store(false, Ordering::Release);
        fixture.transport.asleep.store(false, Ordering::Release);
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        assert_eq!(user_turns(&fixture.capture, "NEVER_LATE"), 0);
    }
}
