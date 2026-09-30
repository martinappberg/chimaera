use super::support::*;
use crate::{
    agents::{AgentKind, AgentRecord},
    lock,
};
use serde_json::{json, Value};

async fn rpc(
    port: u16,
    session: &str,
    key: &str,
    method: &str,
    params: Value,
) -> (StatusCode, Value) {
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .unwrap();
    let task = tokio::spawn(connection);
    let response = sender
        .send_request(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/mcp/{session}"))
                .header("host", "127.0.0.1")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    task.abort();
    (status, serde_json::from_slice(&body).unwrap())
}
fn content(result: &Value) -> Value {
    serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[cfg(unix)]
async fn tui_mcp(
    state: &Arc<crate::AppState>,
    workspace: &crate::workspaces::Workspace,
    enabled: bool,
) {
    use std::os::unix::fs::PermissionsExt;
    let script = workspace.root.join("fixture-codex");
    let capture = workspace.root.join("codex-args");
    let present = workspace.root.join("codex-key-present");
    let quote =
        |path: &std::path::Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    std::fs::write(&script,format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nif [ -n \"$CHIMAERA_MCP_KEY\" ]; then printf yes > {}; else printf no > {}; fi\nexec /bin/sleep 30\n",quote(&capture),quote(&present),quote(&present))).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    preset_agent(
        state,
        AgentKind::Codex,
        Ok(script),
        Some(chimaera_agent::codex::TESTED_CODEX_VERSION),
    );
    let row = crate::spawn::spawn_session(
        state,
        crate::spawn::SpawnSpec {
            workspace: workspace.clone(),
            started_by: crate::history::StartedBy::You,
            id: None,
            name: None,
            cwd: None,
            native_cwd: None,
            cols: None,
            rows: None,
            theme: "dark".into(),
            title_hint: None,
            prelude: None,
            fork_head: false,
            kind: crate::spawn::SpawnKind::Agent {
                kind: AgentKind::Codex,
                model: None,
                resume: None,
            },
        },
    )
    .await
    .unwrap_or_else(|_| panic!("fixture TUI failed to spawn"));
    let id = row["id"].as_str().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if tokio::fs::try_exists(&present).await.unwrap_or(false) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let args = std::fs::read_to_string(&capture).unwrap();
    // Agent communication (on by default) hands every Codex TUI the chimaera
    // endpoint and its key; the cloud profile tools are never pre-approved.
    assert!(args.contains("mcp_servers.chimaera.url="));
    assert!(!args.contains("update_cloud_profile"));
    assert_eq!(std::fs::read_to_string(&present).unwrap(), "yes");
    // The notify hook (pause state, restart resume) is a Pro-project feature:
    // elsewhere a Codex TUI keeps the user's own notify.
    assert_eq!(args.contains("notify=["), enabled);
    state.sessions.kill(id).unwrap();
    std::fs::remove_file(capture).unwrap();
    std::fs::remove_file(present).unwrap();
}

#[tokio::test]
async fn cloud_context_real_http_is_scoped_revisioned_and_never_executes_on_save() {
    let data = test_dir("cloud-context-http").canonicalize().unwrap();
    let state = test_state_with_data_dir(0, data.clone());
    // Exercise the real router without starting account reconciliation or CLIs.
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
    let root = data.join("project");
    std::fs::create_dir(&root).unwrap();
    let other_root = data.join("other");
    std::fs::create_dir(&other_root).unwrap();
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    let other = lock(&state.workspaces).add(other_root).unwrap();
    for (id, key, workspace) in [
        ("s-cloud", "cloud-fixture", &workspace.id),
        ("s-other", "other-fixture", &other.id),
    ] {
        lock(&state.agents).insert(id.into(), AgentRecord::new(key.into(), AgentKind::Claude));
        lock(&state.session_workspaces).insert(id.into(), workspace.clone());
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = crate::app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (status,_) = request(&state,Method::POST,"/api/v1/pro/configure",Some(json!({"endpoint":format!("http://127.0.0.1:{port}"),"keeper_url":"","account_id":"fixture-account","role":"worker","delegation":{"access_token":"synthetic-fixture","expires_at":"2099-01-01T00:00:00Z","scope":["baton","mirror"],"device_id":"worker-fixture"},"hours_exhausted":false}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = rpc(port, "s-cloud", "wrong", "initialize", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, init) = rpc(port, "s-cloud", "cloud-fixture", "initialize", json!({})).await;
    let text = init["result"]["instructions"].as_str().unwrap();
    assert!(
        text.contains("\"execution_location\":\"cloud\"") && text.contains(std::env::consts::ARCH)
    );
    assert!(text.contains("work-capabilities") && text.contains("normal permissions"));
    assert!(!text.contains("synthetic-fixture"));
    let (_, tools) = rpc(port, "s-cloud", "cloud-fixture", "tools/list", json!({})).await;
    assert!(tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "update_cloud_profile"));
    let (_, first) = rpc(
        port,
        "s-cloud",
        "cloud-fixture",
        "tools/call",
        json!({"name":"read_cloud_profile","arguments":{}}),
    )
    .await;
    let initial = content(&first);
    let profile = json!({"setup_command":"touch forbidden-auto-execution","laptop_only":["xcodebuild test"],"deferred":["xcodebuild test"],"missing_environment":["PROJECT_API_TOKEN"]});
    let update = json!({"expected_revision":initial["revision"],"profile":profile});
    let (_, saved) = rpc(
        port,
        "s-cloud",
        "cloud-fixture",
        "tools/call",
        json!({"name":"update_cloud_profile","arguments":update}),
    )
    .await;
    assert_eq!(content(&saved)["executed"], false);
    assert_eq!(content(&saved)["awaiting_confirmation"], true);
    assert!(!root.join("forbidden-auto-execution").exists());
    assert!(state.sessions.list().is_empty() && state.chat.list().is_empty());
    let (_, next) = rpc(
        port,
        "s-cloud",
        "cloud-fixture",
        "tools/call",
        json!({"name":"read_cloud_profile","arguments":{}}),
    )
    .await;
    // An agent's setup command is only a proposal until the user confirms it.
    let mut proposed = profile.clone();
    proposed["setup_command"] = Value::Null;
    proposed["pending_setup_command"] = json!("touch forbidden-auto-execution");
    assert_eq!(content(&next)["profile"], proposed);
    let stored = crate::pro::workspace_profile(&state, &workspace.id).unwrap();
    assert_eq!(stored.setup_command, None);
    // An edit that leaves the setup command alone keeps the proposal waiting.
    let (_, kept) = rpc(port,"s-cloud","cloud-fixture","tools/call",json!({"name":"update_cloud_profile","arguments":{"expected_revision":content(&next)["revision"],"profile":{"setup_command":null,"laptop_only":["xcodebuild test"],"deferred":["xcodebuild test"],"missing_environment":["PROJECT_API_TOKEN"]}}})).await;
    assert_eq!(content(&kept)["awaiting_confirmation"], true);
    let stored = crate::pro::workspace_profile(&state, &workspace.id).unwrap();
    assert_eq!(stored.setup_command, None);
    assert_eq!(
        stored.pending_setup_command.as_deref(),
        Some("touch forbidden-auto-execution")
    );
    let (_, next) = rpc(
        port,
        "s-cloud",
        "cloud-fixture",
        "tools/call",
        json!({"name":"read_cloud_profile","arguments":{}}),
    )
    .await;
    assert!(content(&next)["context"]
        .as_str()
        .unwrap()
        .contains("\"proposed_setup_command_awaiting_user\":true"));
    let (_, unrelated) = rpc(
        port,
        "s-other",
        "other-fixture",
        "tools/call",
        json!({"name":"read_cloud_profile","arguments":{}}),
    )
    .await;
    assert!(content(&unrelated)["profile"]["setup_command"].is_null());
    let (_, stale) = rpc(
        port,
        "s-cloud",
        "cloud-fixture",
        "tools/call",
        json!({"name":"update_cloud_profile","arguments":update}),
    )
    .await;
    assert_eq!(stale["result"]["isError"], true);
    let (_, foreign) = rpc(
        port,
        "s-cloud",
        "cloud-fixture",
        "tools/call",
        json!({"name":"read_cloud_profile","arguments":{"workspace_id":other.id}}),
    )
    .await;
    assert_eq!(foreign["result"]["isError"], true);
    let (_,invalid) = rpc(port,"s-cloud","cloud-fixture","tools/call",json!({"name":"update_cloud_profile","arguments":{"expected_revision":content(&next)["revision"],"profile":{"setup_command":"sk-abcdefghijklmnopqrstuv","laptop_only":[],"deferred":[],"missing_environment":[]}}})).await;
    assert_eq!(invalid["result"]["isError"], true);
    assert!(!invalid.to_string().contains("sk-abcdefghijklmnopqrstuv"));
    let (_, refreshed) = rpc(port, "s-cloud", "cloud-fixture", "initialize", json!({})).await;
    let text = refreshed["result"]["instructions"].as_str().unwrap();
    assert!(
        text.contains("PROJECT_API_TOKEN")
            && text.contains("untrusted data")
            && text.contains("xcodebuild")
    );
    #[cfg(unix)]
    tui_mcp(&state, &workspace, true).await;
    let (status,_) = request(&state,Method::POST,"/api/v1/pro/configure",Some(json!({"endpoint":format!("http://127.0.0.1:{port}"),"keeper_url":"","account_id":"fixture-account","role":"device","delegation":{"access_token":"synthetic-return","expires_at":"2099-01-01T00:00:00Z","scope":["baton","mirror"],"device_id":"device-fixture"},"hours_exhausted":false}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // On the user's own computer an agent gets no brief about where it runs...
    let (_, home) = rpc(port, "s-cloud", "cloud-fixture", "initialize", json!({})).await;
    let text = home["result"]["instructions"].as_str().unwrap_or_default();
    assert!(!text.contains("work-capabilities") && !text.contains("cloud-profile-data"));
    // ...unless its project came back from the cloud, when the old cloud
    // assumptions must be replaced.
    lock(&state.pro.returned).insert(workspace.id.clone());
    let (_, returned) = rpc(port, "s-cloud", "cloud-fixture", "initialize", json!({})).await;
    let text = returned["result"]["instructions"].as_str().unwrap();
    assert!(
        text.contains("\"execution_location\":\"device\"")
            && text.contains("replaces earlier cloud-only assumptions")
    );
    assert!(!text.contains("This is a headless environment") && !text.contains("synthetic-return"));
    let (_, read) = rpc(
        port,
        "s-cloud",
        "cloud-fixture",
        "tools/call",
        json!({"name":"read_cloud_profile","arguments":{}}),
    )
    .await;
    let read = content(&read);
    assert!(read["context"]
        .as_str()
        .unwrap()
        .contains("\"execution_location\":\"device\""));
    assert!(
        !read.to_string().contains("worker-fixture")
            && !read.to_string().contains("device-fixture")
    );
    assert!(
        state.chat.list().is_empty(),
        "context refresh must not start a model turn"
    );
    request(&state, Method::DELETE, "/api/v1/pro/configure", None).await;
    let (_, disconnected) = rpc(
        port,
        "s-cloud",
        "cloud-fixture",
        "tools/call",
        json!({"name":"read_cloud_profile","arguments":{}}),
    )
    .await;
    assert_eq!(disconnected["result"]["isError"], true);
    #[cfg(unix)]
    tui_mcp(&state, &workspace, false).await;
    server.abort();
    let _ = server.await;
    drop(state);
    std::fs::remove_dir_all(data).unwrap();
}
