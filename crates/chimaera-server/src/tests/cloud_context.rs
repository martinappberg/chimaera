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
// The default daemon keeps ordinary MCP available without paid guidance.
#[tokio::test]
async fn absent_runtime_real_http_keeps_mcp_and_refuses_paid_profile_tools() {
    let data = test_dir("cloud-context-absent-http")
        .canonicalize()
        .unwrap();
    let state = test_state_with_data_dir(0, data.clone());
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
    let root = data.join("project");
    std::fs::create_dir(&root).unwrap();
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    lock(&state.agents).insert(
        "s-cloud".into(),
        AgentRecord::new("cloud-fixture".into(), AgentKind::Claude),
    );
    lock(&state.session_workspaces).insert("s-cloud".into(), workspace.id.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = crate::app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    // Even a configured worker cannot supply paid descriptions or tools when
    // the optional runtime is absent. This is the same authenticated route.
    let (status, _) = request(&state, Method::POST, "/api/v1/pro/configure", Some(json!({
        "endpoint":format!("http://127.0.0.1:{port}"),"keeper_url":"","account_id":"fixture-account",
        "role":"worker","delegation":{"access_token":"synthetic-fixture","expires_at":"2099-01-01T00:00:00Z",
        "scope":["baton","mirror"],"device_id":"worker-fixture"},"hours_exhausted":false
    }))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = rpc(port, "s-cloud", "wrong", "initialize", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, init) = rpc(port, "s-cloud", "cloud-fixture", "initialize", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let instructions = init["result"]["instructions"].as_str().unwrap_or_default();
    assert!(!instructions.contains("work-capabilities"));
    assert!(!instructions.contains("cloud-profile-data"));
    assert!(!instructions.contains("execution_location"));
    assert!(!init.to_string().contains("synthetic-fixture"));
    let (status, tools) = rpc(port, "s-cloud", "cloud-fixture", "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "read_cloud_profile"
                || tool["name"] == "update_cloud_profile")
    );
    let before = crate::pro::workspace_profile(&state, &workspace.id).unwrap();
    for (name, arguments) in [
        ("read_cloud_profile", json!({})),
        (
            "update_cloud_profile",
            json!({"expected_revision":"0".repeat(64),"profile":{
                "setup_command":"touch forbidden-auto-execution","laptop_only":[],"deferred":[],"missing_environment":[]
            }}),
        ),
    ] {
        let (status, refused) = rpc(
            port,
            "s-cloud",
            "cloud-fixture",
            "tools/call",
            json!({"name":name,"arguments":arguments}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(refused["result"]["isError"], true);
    }
    assert_eq!(
        serde_json::to_value(crate::pro::workspace_profile(&state, &workspace.id).unwrap())
            .unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert!(!root.join("forbidden-auto-execution").exists());
    assert!(state.sessions.list().is_empty() && state.chat.list().is_empty());
    request(&state, Method::DELETE, "/api/v1/pro/configure", None).await;
    server.abort();
    let _ = server.await;
    drop(state);
    std::fs::remove_dir_all(data).unwrap();
}
