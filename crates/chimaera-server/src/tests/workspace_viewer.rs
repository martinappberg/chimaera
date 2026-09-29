use super::support::*;
use crate::*;
use futures::SinkExt;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

fn fixture() -> (
    Arc<AppState>,
    crate::workspaces::Workspace,
    crate::workspaces::Workspace,
) {
    let state = test_state();
    let root = test_dir("viewer-projects").canonicalize().unwrap();
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::create_dir_all(root.join("project-other")).unwrap();
    let one = lock(&state.workspaces).add(root.join("project")).unwrap();
    let other = lock(&state.workspaces)
        .add(root.join("project-other"))
        .unwrap();
    std::fs::write(one.root.join("note.txt"), "unchanged /project contents").unwrap();
    std::fs::write(other.root.join("private.txt"), "other project").unwrap();
    pro::install_execution_fixture(&state, &one.id, 4).unwrap();
    (state, one, other)
}

#[tokio::test]
async fn delayed_lifecycle_bodies_cannot_mutate_a_replacement_epoch_or_account_generation() {
    for next_epoch in [4, 5] {
        for (method, path, json_body) in [
            (
                Method::POST,
                "/sessions",
                json!({"workspace_id":"WORKSPACE"}),
            ),
            (
                Method::POST,
                "/sessions/s-fixture/exec",
                json!({"command":"printf stale"}),
            ),
            (
                Method::POST,
                "/sessions/s-fixture/view",
                json!({"ui":"chat"}),
            ),
            (
                Method::POST,
                "/sessions/s-fixture/rewind",
                json!({"resume_at":"anchor"}),
            ),
            (
                Method::POST,
                "/sessions/s-fixture/fork",
                json!({"resume_at":"anchor"}),
            ),
            (
                Method::PATCH,
                "/sessions/s-fixture",
                json!({"name":"stale"}),
            ),
            (Method::DELETE, "/sessions/s-fixture", json!({})),
            (
                Method::PUT,
                "/workspaces/WORKSPACE/mastermind",
                json!({"agent":"claude","mode":"ask"}),
            ),
            (
                Method::PUT,
                "/view-state/tabs_WORKSPACE",
                json!({"stale":true}),
            ),
        ] {
            let (state, one, _) = fixture();
            lock(&state.session_workspaces).insert("s-fixture".into(), one.id.clone());
            let bytes = json_body.to_string().replace("WORKSPACE", &one.id);
            let uri = format!("/api/v1{}", path.replace("WORKSPACE", &one.id));
            let (entered, ready) = tokio::sync::oneshot::channel();
            let (finish, wait) = tokio::sync::oneshot::channel();
            let body = Body::from_stream(futures::stream::once(async move {
                entered.send(()).unwrap();
                wait.await.unwrap();
                Ok::<_, std::io::Error>(bytes::Bytes::from(bytes))
            }));
            let request = Request::builder()
                .method(method)
                .uri(&uri)
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-chimaera-workspace", &one.id)
                .header("x-chimaera-epoch", "4")
                .body(body)
                .unwrap();
            let pending = tokio::spawn(app(state.clone()).oneshot(request));
            ready.await.unwrap();
            assert_eq!(
                super::support::request(&state, Method::DELETE, "/api/v1/pro/configure", None)
                    .await
                    .0,
                StatusCode::NO_CONTENT
            );
            pro::install_execution_fixture(&state, &one.id, next_epoch).unwrap();
            finish.send(()).unwrap();
            let response = pending.await.unwrap().unwrap();
            assert_eq!(
                response.status(),
                StatusCode::CONFLICT,
                "{uri}, epoch {next_epoch}"
            );
            assert!(state.sessions.list().is_empty());
            assert!(state.chat.list().is_empty());
            assert!(lock(&state.workspaces)
                .get(&one.id)
                .unwrap()
                .mastermind
                .is_none());
            state
                .stopping
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

#[tokio::test]
async fn scoped_delayed_save_and_upload_cannot_commit_into_a_replacement_epoch() {
    for (upload, next_epoch) in [(false, 4), (false, 5), (true, 4), (true, 5)] {
        let (state, one, _) = fixture();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (finish, wait) = tokio::sync::oneshot::channel();
        let body = Body::from_stream(futures::stream::once(async move {
            entered.send(()).unwrap();
            wait.await.unwrap();
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"stale bytes"))
        }));
        let (method, uri) = if upload {
            (
                Method::POST,
                "/api/v1/fs/upload?dir=%2Fproject&name=late.txt",
            )
        } else {
            (Method::PUT, "/api/v1/fs/file?path=%2Fproject%2Fnote.txt")
        };
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header("x-chimaera-workspace", &one.id)
            .header("x-chimaera-epoch", "4")
            .header("x-chimaera-viewer-root", "L3Byb2plY3Q")
            .body(body)
            .unwrap();
        let pending = tokio::spawn(app(state.clone()).oneshot(request));
        ready.await.unwrap();
        assert_eq!(
            super::support::request(&state, Method::DELETE, "/api/v1/pro/configure", None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        pro::install_execution_fixture(&state, &one.id, next_epoch).unwrap();
        std::fs::write(one.root.join("note.txt"), "new canonical bytes").unwrap();
        finish.send(()).unwrap();
        let response = pending.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "upload={upload}");
        assert_eq!(
            std::fs::read(one.root.join("note.txt")).unwrap(),
            b"new canonical bytes"
        );
        assert!(!one.root.join("late.txt").exists());
        assert!(std::fs::read_dir(&one.root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
        state
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

#[tokio::test]
async fn live_tcp_save_started_before_handoff_cannot_overwrite_new_owner() {
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (state, one, _) = fixture();
    let (entered, mut ready) = tokio::sync::mpsc::channel(1);
    let router = app(state.clone()).layer(axum::middleware::from_fn(
        move |request: Request<Body>, next: axum::middleware::Next| {
            let entered = entered.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = Body::from_stream(body.into_data_stream().inspect(move |_| {
                    let _ = entered.try_send(());
                }));
                next.run(Request::from_parts(parts, body)).await
            }
        },
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
    let headers = format!("PUT /api/v1/fs/file?path=%2Fproject%2Fnote.txt HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer test-token\r\nX-Chimaera-Workspace: {}\r\nX-Chimaera-Epoch: 4\r\nX-Chimaera-Viewer-Root: L3Byb2plY3Q\r\nContent-Length: 11\r\nConnection: close\r\n\r\nstale", one.id);
    client.write_all(headers.as_bytes()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), ready.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        request(&state, Method::DELETE, "/api/v1/pro/configure", None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    pro::install_execution_fixture(&state, &one.id, 5).unwrap();
    std::fs::write(one.root.join("note.txt"), "new canonical bytes").unwrap();
    client.write_all(b" bytes").await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        (&mut client).take(16384).read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 409"),
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert_eq!(
        std::fs::read(one.root.join("note.txt")).unwrap(),
        b"new canonical bytes"
    );
    server.abort();
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}
async fn scoped(
    state: &Arc<AppState>,
    workspace: &str,
    epoch: u64,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Vec<u8>) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, "Bearer test-token")
        .header("x-chimaera-workspace", workspace)
        .header("x-chimaera-epoch", epoch.to_string())
        .header("x-chimaera-viewer-root", "L3Byb2plY3Q")
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            body.map(|v| Body::from(v.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let response = app(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}
#[tokio::test]
async fn scoped_http_viewer_reads_only_registered_project_and_keeps_content_unchanged() {
    let (state, one, other) = fixture();
    let (status, bytes) = scoped(&state, &one.id, 4, Method::GET, "/api/v1/workspaces", None).await;
    assert_eq!(status, StatusCode::OK);
    let rows: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["id"], one.id);
    assert_eq!(rows[0]["root"], "/project");
    let (status, bytes) = scoped(
        &state,
        &one.id,
        4,
        Method::GET,
        "/api/v1/fs/file?path=%2Fproject%2Fnote.txt",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"unchanged /project contents");
    let (_, query) = (
        (),
        workspace_scope::paths::encode_query(&[(
            "path".into(),
            other
                .root
                .join("private.txt")
                .to_string_lossy()
                .into_owned(),
        )]),
    );
    // Outside the project: the owner says whether it has a file there (so a
    // viewer never shows its own same-named file in its place), never what.
    let (status, bytes) = scoped(
        &state,
        &one.id,
        4,
        Method::GET,
        &format!("/api/v1/fs/file?{query}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let refusal: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(refusal, json!({"error":"outside_project"}));
    let missing = workspace_scope::paths::encode_query(&[(
        "path".into(),
        other
            .root
            .join("never-written.txt")
            .to_string_lossy()
            .into_owned(),
    )]);
    let (status, bytes) = scoped(
        &state,
        &one.id,
        4,
        Method::GET,
        &format!("/api/v1/fs/file?{missing}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!({"error":"not_found"})
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&other.root, one.root.join("escape")).unwrap();
        assert_eq!(
            scoped(
                &state,
                &one.id,
                4,
                Method::GET,
                "/api/v1/fs/file?path=%2Fproject%2Fescape%2Fprivate.txt",
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    for uri in [
        "/api/v1/pro/configure",
        "/api/v1/environment",
        "/api/v1/mcp/opaque-other-project",
        "/api/v1/agent-events/opaque-other-project",
        "/proxy/opaque-other-project",
        "/api/v1/fs/file?path=%2Fproject%2F..%2Fproject-other%2Fprivate.txt",
    ] {
        assert_eq!(
            scoped(&state, &one.id, 4, Method::GET, uri, None).await.0,
            StatusCode::FORBIDDEN
        );
    }
    // A compound resolver answers for what it may read and leaves the rest
    // unresolved: one link outside the project never blanks every card, and
    // never reads outside. A base outside the project is never used.
    let outside = other
        .root
        .join("private.txt")
        .to_string_lossy()
        .into_owned();
    for (route, base) in [
        ("validate", "/project"),
        ("resolve_targets", "/project"),
        ("resolve_targets", "/"),
    ] {
        let key = if route == "validate" {
            "candidates"
        } else {
            "targets"
        };
        let (status, bytes) = scoped(
            &state,
            &one.id,
            4,
            Method::POST,
            &format!("/api/v1/fs/{route}"),
            Some(json!({"base":base,key:["~/outside-project", outside, "/project/note.txt"]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{route} {base}");
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("outside-project"), "{route}: {text}");
        assert!(!text.contains("private.txt"), "{route}: {text}");
        assert!(
            text.contains("/project/note.txt"),
            "{route} {base} resolves the project's own file: {text}"
        );
    }
    // A conversation's saved images are readable by this project's viewers,
    // and only this project's.
    for (session, workspace) in [("s-own-chat", &one.id), ("s-other-chat", &other.id)] {
        lock(&state.session_workspaces).insert(session.into(), workspace.clone());
        std::fs::create_dir_all(state.uploads_root.join(session)).unwrap();
        std::fs::write(state.uploads_root.join(session).join("pic.png"), "png").unwrap();
    }
    let picture = |session: &str| {
        let path = state
            .uploads_root
            .join(session)
            .join("pic.png")
            .to_string_lossy()
            .into_owned();
        format!(
            "/api/v1/fs/file?{}",
            workspace_scope::paths::encode_query(&[("path".into(), path)])
        )
    };
    assert_eq!(
        scoped(
            &state,
            &one.id,
            4,
            Method::GET,
            &picture("s-own-chat"),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        scoped(
            &state,
            &one.id,
            4,
            Method::GET,
            &picture("s-other-chat"),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // Reading a saved image never grants writing next to it.
    assert_eq!(
        scoped(
            &state,
            &one.id,
            4,
            Method::PUT,
            &picture("s-own-chat"),
            Some(json!("overwrite"))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // The reading view's document checker is part of the project surface.
    assert_eq!(
        scoped(
            &state,
            &one.id,
            4,
            Method::GET,
            "/api/v1/fs/check_document?path=%2Fproject%2Fnote.txt",
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    // It checks against the project's own folder (never the viewer's root,
    // which names a folder on another machine) and touches no link target
    // outside the project: no existence, no heading names.
    std::fs::write(other.root.join("secret.md"), "# Secret heading name\n").unwrap();
    std::fs::write(
        one.root.join("doc.md"),
        format!(
            "# Doc\n\n[mine](/note.txt)\n\n[theirs]({}#secret-heading-name)\n\n[gone]({})\n",
            other.root.join("secret.md").display(),
            other.root.join("never-written.md").display()
        ),
    )
    .unwrap();
    let (status, bytes) = scoped(
        &state,
        &one.id,
        4,
        Method::GET,
        "/api/v1/fs/check_document?path=%2Fproject%2Fdoc.md&root=%2FUsers%2Fviewer%2Fproject",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(bytes).unwrap();
    let report: Value = serde_json::from_str(&text).unwrap();
    let codes: Vec<&str> = report["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|issue| issue["code"].as_str())
        .collect();
    assert!(codes.contains(&"outside_project"), "{text}");
    assert!(
        !codes.iter().any(|code| code.starts_with("broken")),
        "a root-relative link resolves in the project; an outside one is not probed: {text}"
    );
    assert!(!codes.contains(&"missing-heading"), "{text}");
    assert!(!text.contains("Secret heading name"), "{text}");
    assert_eq!(
        scoped(&state, &one.id, 3, Method::GET, "/api/v1/workspaces", None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert!(
        state.sessions.list().is_empty(),
        "viewing never starts work"
    );
    assert_eq!(
        pro::status(axum::extract::State(state.clone())).await.0["workspaces"][0]["ownership"]
            ["epoch"],
        4
    );
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}
#[tokio::test]
async fn scoped_ticket_and_resume_ids_cannot_select_another_project() {
    let (state, one, other) = fixture();
    let (status, bytes) = scoped(
        &state,
        &one.id,
        4,
        Method::POST,
        "/api/v1/fs/ticket",
        Some(json!({"path":"/project/note.txt"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ticket: Value = serde_json::from_slice(&bytes).unwrap();
    let uri = format!("/raw/{}", ticket["ticket"].as_str().unwrap());
    assert_eq!(
        scoped(&state, &one.id, 4, Method::GET, &uri, None).await.0,
        StatusCode::OK
    );
    pro::install_execution_fixture(&state, &other.id, 7).unwrap();
    assert_eq!(
        scoped(&state, &other.id, 7, Method::GET, &uri, None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(scoped(&state,&one.id,4,Method::POST,"/api/v1/sessions",Some(json!({"workspace_id":one.id,"kind":"agent","agent":"codex","resume":"unknown-native-handle"}))).await.0,StatusCode::FORBIDDEN);
    lock(&state.session_workspaces).insert("s-private".into(), other.id.clone());
    assert_eq!(
        scoped(
            &state,
            &one.id,
            4,
            Method::DELETE,
            "/api/v1/sessions/s-private",
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(state.sessions.list().is_empty());
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}
#[tokio::test]
async fn established_scoped_terminal_refuses_input_after_authority_is_invalidated() {
    let (state, one, _) = fixture();
    let info = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: one.root.clone(),
            name: None,
            cols: 80,
            rows: 24,
            command: Some(vec!["/bin/sh".into()]),
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .unwrap();
    lock(&state.session_workspaces).insert(info.id.clone(), one.id.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws/sessions/{}", info.id))
            .await
            .unwrap();
    socket.send(Message::Text(json!({"type":"auth","token":"test-token","workspace_id":one.id,"epoch":4,"viewer_root":"L3Byb2plY3Q"}).to_string().into())).await.unwrap();
    let ready = next_ws_frame(&mut socket).await;
    let ready: Value = serde_json::from_str(ready.to_text().unwrap()).unwrap();
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["cwd"], "/project");
    assert_eq!(
        request(&state, Method::DELETE, "/api/v1/pro/configure", None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    socket
        .send(Message::Binary(bytes::Bytes::from_static(
            b"touch MUST_NOT_RUN\n",
        )))
        .await
        .unwrap();
    loop {
        let frame = next_ws_frame(&mut socket).await;
        if let Message::Text(text) = frame {
            if text.contains("workspace_scope_changed") {
                break;
            }
        }
    }
    assert!(!one.root.join("MUST_NOT_RUN").exists());
    assert_eq!(
        session_view::sessions_json(&state)
            .iter()
            .find(|r| r["id"] == info.id)
            .unwrap()["last_input_ms"],
        Value::Null
    );
    state.sessions.kill(&info.id).unwrap();
    task.abort();
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}

#[tokio::test]
async fn established_scoped_sockets_cannot_rejoin_a_replacement_account_at_the_same_epoch() {
    for surface in ["sessions", "resize", "chat", "events"] {
        let (state, project, _) = fixture();
        let captured = project.root.join("synthetic-input.txt");
        let id = if surface == "chat" {
            let fake = write_fake_claude("viewer-generation-agent");
            let script = std::fs::read_to_string(&fake).unwrap();
            std::fs::write(
                &fake,
                script.replace("cat >/dev/null", "cat > \"$CHIMAERA_TEST_CAPTURE\""),
            )
            .unwrap();
            let mut spec = chimaera_agent::driver::SpawnSpec::new(
                "s-viewer-chat",
                vec![fake.to_string_lossy().into_owned()],
                project.root.clone(),
            );
            spec.env.push((
                "CHIMAERA_TEST_CAPTURE".into(),
                captured.to_string_lossy().into_owned(),
            ));
            state
                .chat
                .spawn(&chimaera_agent::claude::ClaudeAdapter, spec)
                .unwrap();
            "s-viewer-chat".to_owned()
        } else {
            state
                .sessions
                .spawn(chimaera_pty::SpawnOpts {
                    cwd: project.root.clone(),
                    name: None,
                    cols: 80,
                    rows: 24,
                    command: Some(vec!["/bin/sh".into()]),
                    id: None,
                    env: Vec::new(),
                    env_remove: Vec::new(),
                    scrollback: None,
                })
                .unwrap()
                .id
        };
        lock(&state.session_workspaces).insert(id.clone(), project.id.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = app(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let suffix = if surface == "events" {
            String::new()
        } else {
            format!("/{id}")
        };
        let endpoint = if surface == "resize" {
            "sessions"
        } else {
            surface
        };
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{address}/ws/{endpoint}{suffix}"))
                .await
                .unwrap();
        socket.send(Message::Text(json!({"type":"auth","token":"test-token","workspace_id":project.id,"epoch":4,"viewer_root":"L3Byb2plY3Q"}).to_string().into())).await.unwrap();
        let first = next_ws_frame(&mut socket).await;
        let first: Value = serde_json::from_str(first.to_text().unwrap()).unwrap();
        assert_eq!(
            first["type"],
            if surface == "events" {
                "sessions"
            } else {
                "ready"
            }
        );

        assert_eq!(
            request(&state, Method::DELETE, "/api/v1/pro/configure", None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        // This is deliberately the same workspace and epoch. Fresh authority is
        // valid, but the previous authenticated connection must remain retired.
        pro::install_execution_fixture(&state, &project.id, 4).unwrap();
        pro::validate_execution_scope(&state, &project.id, 4).unwrap();
        let stale = match surface {
            "resize" => Message::Text(
                json!({"type":"resize","cols":177,"rows":63})
                    .to_string()
                    .into(),
            ),
            "sessions" => Message::Binary(bytes::Bytes::from_static(b"touch STALE_WS_INPUT\n")),
            "chat" => Message::Text(
                json!({"type":"send","blocks":[{"type":"text","text":"STALE_WS_MESSAGE"}]})
                    .to_string()
                    .into(),
            ),
            _ => Message::Text(
                json!({"type":"watch","workspace_id":project.id,"files":["/project/note.txt"]})
                    .to_string()
                    .into(),
            ),
        };
        socket.send(stale).await.unwrap();
        loop {
            let frame = next_ws_frame(&mut socket).await;
            if let Message::Text(text) = frame {
                if text.contains("workspace_scope_changed") {
                    break;
                }
            }
        }
        assert!(!project.root.join("STALE_WS_INPUT").exists());
        // Refusal is the socket's retirement, never the process's death:
        // signing out does not stop this computer's own work (laptop first).
        assert!(
            state.chat.get(&id).is_some_and(|s| s.alive)
                || state.sessions.get(&id).is_some_and(|s| s.alive),
            "{surface}: sign-out must not stop the session"
        );
        if surface == "resize" {
            assert_eq!(state.sessions.get(&id).unwrap().cols, 80);
            assert_eq!(state.sessions.get(&id).unwrap().rows, 24);
        }
        assert!(!std::fs::read_to_string(&captured)
            .unwrap_or_default()
            .contains("STALE_WS_MESSAGE"));
        state
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
        if surface == "chat" {
            state.chat.kill(&id);
        } else {
            let _ = state.sessions.kill(&id);
        }
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn laptop_first_sign_out_and_unreachable_account_keep_local_work_running() {
    let (state, project, _) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    // A plain shell through the ordinary spawn route, and a managed chat agent.
    let (status, shell) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(json!({"workspace_id":project.id,"kind":"shell","command":"/bin/sh"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shell}");
    let shell = shell["id"].as_str().unwrap().to_owned();
    // The route starts the user's own login shell, whose startup time is not
    // this test's business; typed input goes to a hermetic plain shell.
    let typed = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: project.root.clone(),
            name: None,
            cols: 80,
            rows: 24,
            command: Some(vec!["/bin/sh".into()]),
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .unwrap()
        .id;
    lock(&state.session_workspaces).insert(typed.clone(), project.id.clone());
    let captured = project.root.join("chat-input.txt");
    let fake = write_fake_claude("laptop-first-agent");
    let script = std::fs::read_to_string(&fake).unwrap();
    std::fs::write(
        &fake,
        script.replace("cat >/dev/null", "cat > \"$CHIMAERA_TEST_CAPTURE\""),
    )
    .unwrap();
    let mut spec = chimaera_agent::driver::SpawnSpec::new(
        "s-laptop-chat",
        vec![fake.to_string_lossy().into_owned()],
        project.root.clone(),
    );
    spec.managed_execution = true;
    spec.env.push((
        "CHIMAERA_TEST_CAPTURE".into(),
        captured.to_string_lossy().into_owned(),
    ));
    state
        .chat
        .spawn(&chimaera_agent::claude::ClaudeAdapter, spec)
        .unwrap();
    lock(&state.session_workspaces).insert("s-laptop-chat".into(), project.id.clone());

    // The account stops answering: the lease lapses without another owner.
    assert!(pro::expire_execution_fixture(&state, &project.id).is_empty());
    assert!(pro::may_execute(&state, &project.id));
    // Then the user signs out (or the plan lapses).
    assert_eq!(
        request(&state, Method::DELETE, "/api/v1/pro/configure", None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(state.sessions.get(&shell).is_some_and(|s| s.alive));
    assert!(state.sessions.get(&typed).is_some_and(|s| s.alive));
    assert!(state.chat.get("s-laptop-chat").is_some_and(|s| s.alive));

    let (mut terminal, _) =
        tokio_tungstenite::connect_async(format!("ws://{address}/ws/sessions/{typed}"))
            .await
            .unwrap();
    terminal
        .send(Message::Text(
            json!({"type":"auth","token":"test-token"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let ready: Value =
        serde_json::from_str(next_ws_frame(&mut terminal).await.to_text().unwrap()).unwrap();
    assert_eq!(ready["type"], "ready");
    let (mut chat, _) =
        tokio_tungstenite::connect_async(format!("ws://{address}/ws/chat/s-laptop-chat"))
            .await
            .unwrap();
    chat.send(Message::Text(
        json!({"type":"auth","token":"test-token"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    chat.send(Message::Text(
        json!({"type":"send","blocks":[{"type":"text","text":"LOCAL_CHAT_AFTER_SIGN_OUT"}]})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let mut tick = 0u32;
        while !project.root.join("LOCAL_AFTER_SIGN_OUT").exists()
            || !std::fs::read_to_string(&captured)
                .unwrap_or_default()
                .contains("LOCAL_CHAT_AFTER_SIGN_OUT")
        {
            if tick.is_multiple_of(100) && !project.root.join("LOCAL_AFTER_SIGN_OUT").exists() {
                terminal
                    .send(Message::Binary(bytes::Bytes::from_static(
                        b"touch LOCAL_AFTER_SIGN_OUT\n",
                    )))
                    .await
                    .unwrap();
            }
            tick += 1;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "local input must still be accepted: terminal={} chat={:?} shell_alive={} chat_alive={}",
            project.root.join("LOCAL_AFTER_SIGN_OUT").exists(),
            std::fs::read_to_string(&captured).ok(),
            state.sessions.get(&typed).is_some_and(|s| s.alive),
            state.chat.get("s-laptop-chat").is_some_and(|s| s.alive),
        )
    });

    // A verified other owner is the one fence: input stops, nothing is killed.
    pro::install_remote_owner_fixture(&state, &project.id, 5);
    assert!(!pro::may_execute(&state, &project.id));
    assert!(state.sessions.get(&shell).is_some_and(|s| s.alive));

    state.chat.kill("s-laptop-chat");
    let _ = state.sessions.kill(&shell);
    let _ = state.sessions.kill(&typed);
    server.abort();
    let _ = server.await;
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}

#[tokio::test]
async fn scoped_connection_reads_remain_valid_while_mutation_capacity_is_full() {
    let (state, project, _) = fixture();
    let scope = crate::workspace_scope::Scope {
        workspace_id: project.id.clone(),
        epoch: 4,
        viewer_root: None,
    };
    let admission = crate::workspace_scope::Mutation::for_scope(&state, scope.clone()).unwrap();
    let guards: Vec<_> = (0..64).map(|_| admission.begin(&state).unwrap()).collect();
    assert!(
        admission.begin(&state).is_err(),
        "mutation ceiling remains enforced"
    );
    assert!(
        admission.validate(&state).is_ok(),
        "existing viewers are still authorized"
    );
    assert!(
        crate::workspace_scope::Mutation::for_scope(&state, scope).is_ok(),
        "read-only authentication needs no mutation slot"
    );
    let terminal = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: project.root.clone(),
            name: None,
            cols: 80,
            rows: 24,
            command: Some(vec!["/bin/sh".into()]),
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .unwrap();
    lock(&state.session_workspaces).insert(terminal.id.clone(), project.id.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{address}/ws/sessions/{}", terminal.id))
            .await
            .unwrap();
    socket
        .send(Message::Text(
            json!({"type":"auth","token":"test-token","workspace_id":project.id,"epoch":4})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let ready = next_ws_frame(&mut socket).await;
    assert_eq!(
        serde_json::from_str::<Value>(ready.to_text().unwrap()).unwrap()["type"],
        "ready"
    );
    socket
        .send(Message::Binary(bytes::Bytes::from_static(
            b"touch BUSY_MUST_NOT_RUN\n",
        )))
        .await
        .unwrap();
    loop {
        let frame = next_ws_frame(&mut socket).await;
        if let Message::Text(text) = frame {
            let frame: Value = serde_json::from_str(&text).unwrap();
            if frame["type"] == "error" {
                assert_eq!(
                    frame["code"], "read_only",
                    "busy input must not report session exit"
                );
                break;
            }
            assert_ne!(frame["type"], "exited");
        }
    }
    assert!(!project.root.join("BUSY_MUST_NOT_RUN").exists());
    assert!(state.sessions.get(&terminal.id).is_some());
    drop(guards);
    assert!(admission.begin(&state).is_ok());
    socket
        .send(Message::Binary(bytes::Bytes::from_static(
            b"touch ACCEPTED_AFTER_BUSY\n",
        )))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !project.root.join("ACCEPTED_AFTER_BUSY").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    state.sessions.kill(&terminal.id).unwrap();
    server.abort();
    let _ = server.await;
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}

/// A project scope the daemon cannot admit yet (a wrong or not-yet-renewed
/// epoch) is a retryable refusal, not a wrong token: the UI reconnects on
/// `workspace_scope_changed` but treats `unauthorized` as final, which made the
/// first sockets after a cloud machine woke fail for good.
#[tokio::test]
async fn refused_socket_scope_is_retryable_and_a_wrong_token_is_not() {
    // The fixture already holds this project's execution grant at epoch 4.
    let (state, project, _) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    for (auth, expected) in [
        (
            json!({"type":"auth","token":"test-token","workspace_id":project.id,"epoch":99,"viewer_root":"L3Byb2plY3Q"}),
            ("code", "workspace_scope_changed"),
        ),
        (
            json!({"type":"auth","token":"wrong"}),
            ("message", "unauthorized"),
        ),
    ] {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws/events"))
            .await
            .unwrap();
        socket
            .send(Message::Text(auth.to_string().into()))
            .await
            .unwrap();
        let frame = next_ws_frame(&mut socket).await;
        let frame: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert_eq!(frame["type"], "error", "{frame}");
        assert_eq!(frame[expected.0], expected.1, "{frame}");
    }
    server.abort();
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}
