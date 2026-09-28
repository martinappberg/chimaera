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
    assert_eq!(
        scoped(
            &state,
            &one.id,
            4,
            Method::GET,
            &format!("/api/v1/fs/file?{query}"),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
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
    for route in ["validate", "resolve_targets"] {
        let body = if route == "validate" {
            json!({"base":"/project","candidates":["~/outside-project"]})
        } else {
            json!({"base":"/project","targets":["~/outside-project"]})
        };
        assert_eq!(
            scoped(
                &state,
                &one.id,
                4,
                Method::POST,
                &format!("/api/v1/fs/{route}"),
                Some(body)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
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
