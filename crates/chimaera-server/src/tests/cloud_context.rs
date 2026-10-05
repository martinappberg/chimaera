use super::support::*;
use crate::{
    agents::{AgentKind, AgentRecord},
    lock, AppState,
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

/// What a claude hook answered (the whole body; `{}` = nothing added).
async fn hook(state: &Arc<AppState>, id: &str, key: &str, payload: Value) -> Value {
    let (status, answer) = request(
        state,
        Method::POST,
        &format!("/api/v1/agent-events/{id}?key={key}"),
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    answer
}

fn context(answer: &Value) -> &str {
    answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("")
}

async fn configure(state: &Arc<AppState>, port: u16, role: &str) {
    let (status, body) = request(state, Method::POST, "/api/v1/pro/configure", Some(json!({
        "endpoint":format!("http://127.0.0.1:{port}"),"keeper_url":"","account_id":"fixture-account",
        "role":role,"delegation":{"access_token":"synthetic-fixture","expires_at":"2099-01-01T00:00:00Z",
        "scope":["baton","mirror"],"device_id":format!("{role}-fixture")},"hours_exhausted":false
    }))).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

/// What every agent start in this workspace is handed: the server
/// instructions, its tools, the hook answers and the pre-approved tools.
async fn agent_start(
    state: &Arc<AppState>,
    port: u16,
    workspace: &str,
) -> (Value, Vec<String>, Value, Value, Vec<String>) {
    // A fresh record per start, as a spawn makes one.
    lock(&state.agents).insert(
        "s-cloud".into(),
        AgentRecord::new("cloud-fixture".into(), AgentKind::Claude),
    );
    let (_, init) = rpc(port, "s-cloud", "cloud-fixture", "initialize", json!({})).await;
    let (_, tools) = rpc(port, "s-cloud", "cloud-fixture", "tools/list", json!({})).await;
    let names = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect();
    let start = hook(
        state,
        "s-cloud",
        "cloud-fixture",
        json!({"hook_event_name":"SessionStart","source":"startup"}),
    )
    .await;
    let prompt = hook(
        state,
        "s-cloud",
        "cloud-fixture",
        json!({"hook_event_name":"UserPromptSubmit","prompt":"where am I?"}),
    )
    .await;
    let allowed = crate::plugins::spawn_allow(state, workspace).await;
    (init["result"].clone(), names, start, prompt, allowed)
}

/// The free contract: a daemon without the optional Runtime hands its agents
/// exactly what it did before Pro existed (`origin/main`), even for a project
/// whose account is configured. Nothing is appended to the MCP instructions,
/// no tool is added or pre-approved, no hook answer carries a note, no Codex
/// developer note is made and no arrival record is written.
#[tokio::test]
async fn absent_runtime_hands_agents_exactly_what_a_free_daemon_does() {
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
    lock(&state.session_workspaces).insert("s-cloud".into(), workspace.id.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = crate::app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let free = agent_start(&state, port, &workspace.id).await;
    assert_eq!(free.2, json!({}), "a free start hook adds nothing");
    assert_eq!(free.3, json!({}), "a free prompt hook adds nothing");
    for role in ["worker", "device"] {
        configure(&state, port, role).await;
        crate::mcp::cloud_context::record_arrival(
            &state,
            &workspace.id,
            Some(&[PathBuf::from("data.csv")]),
            [Some("macos"), Some("aarch64")],
        )
        .await;
        assert_eq!(
            agent_start(&state, port, &workspace.id).await,
            free,
            "{role}"
        );
        assert!(crate::mcp::cloud_context::note(&state, &workspace.id)
            .await
            .is_none());
        assert!(!crate::mcp::cloud_context::available_in(
            &state,
            &workspace.id
        ));
    }
    assert!(!crate::pro::storage(&state).join(&workspace.id).exists());
    let before = crate::pro::workspace_profile(&state, &workspace.id).unwrap();
    for (name, arguments) in [
        ("where_am_i", json!({})),
        (
            "update_cloud_profile",
            json!({"expected_revision":"0".repeat(64),"setup_command":"touch forbidden-auto-execution"}),
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

/// A stand-in Runtime whose words show which facts it was handed.
struct Wording;
impl crate::daemon_extension::Runtime for Wording {
    fn coordinate(
        &self,
        _owner: crate::daemon_extension::CoordinatorOwner,
    ) -> crate::daemon_extension::RuntimeFuture {
        Box::pin(async {})
    }
    fn guidance_definitions(&self) -> Vec<Value> {
        vec![json!({"name":"where_am_i","description":"fixture","inputSchema":{"type":"object"}})]
    }
    fn placement_note(&self, facts: &crate::daemon_extension::guidance::Facts) -> Option<String> {
        Some(format!(
            "PLACE cloud={} from={:?} kept={}",
            facts.cloud,
            facts.arrival.as_ref().and_then(|a| a.from_os.clone()),
            facts.kept_both.len()
        ))
    }
}

/// With the Runtime and a synced project, each agent start hears where it
/// runs once (SessionStart, or the first prompt where SessionStart did not
/// fire), a subagent's hook never carries it, and a changed note reaches the
/// next start: what a return home relies on.
#[tokio::test]
async fn a_synced_projects_agents_hear_where_they_run_once_per_start() {
    let data = test_dir("cloud-context-carrier").canonicalize().unwrap();
    let state = test_state_with_runtime(data.clone(), Arc::new(Wording));
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
    let root = data.join("project");
    std::fs::create_dir(&root).unwrap();
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    lock(&state.session_workspaces).insert("s-cloud".into(), workspace.id.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = crate::app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    configure(&state, port, "worker").await;

    let (init, names, start, prompt, allowed) = agent_start(&state, port, &workspace.id).await;
    assert!(
        !init.to_string().contains("PLACE cloud"),
        "never in instructions"
    );
    assert!(names.contains(&"where_am_i".to_owned()));
    assert!(allowed.contains(&"where_am_i".to_owned()));
    assert_eq!(context(&start), "PLACE cloud=true from=None kept=0");
    assert_eq!(prompt, json!({}), "not repeated within one start");

    // A subagent's hook speaks for the subagent: never the carrier.
    lock(&state.agents).insert(
        "s-cloud".into(),
        AgentRecord::new("cloud-fixture".into(), AgentKind::Claude),
    );
    let sub = hook(
        &state,
        "s-cloud",
        "cloud-fixture",
        json!({"hook_event_name":"SessionStart","agent_id":"a-1","agent_type":"Explore"}),
    )
    .await;
    assert!(!context(&sub).contains("PLACE cloud"));

    // TUIs where SessionStart did not fire: the first prompt carries it.
    let first = hook(
        &state,
        "s-cloud",
        "cloud-fixture",
        json!({"hook_event_name":"UserPromptSubmit","prompt":"hi"}),
    )
    .await;
    assert_eq!(context(&first), "PLACE cloud=true from=None kept=0");

    // The project comes home: the device's next start hears the newer note.
    configure(&state, port, "device").await;
    crate::mcp::cloud_context::record_arrival(
        &state,
        &workspace.id,
        None,
        [Some("linux"), Some("x86_64")],
    )
    .await;
    let (_, _, start, _, _) = agent_start(&state, port, &workspace.id).await;
    assert_eq!(
        context(&start),
        "PLACE cloud=false from=Some(\"linux\") kept=0"
    );
    // The same record hears a changed note at its next prompt.
    crate::pro::report_return(
        &state,
        &workspace.id,
        (1, vec![PathBuf::from("notes.md.mine-20261005-1402")]),
        &[],
    );
    let changed = hook(
        &state,
        "s-cloud",
        "cloud-fixture",
        json!({"hook_event_name":"UserPromptSubmit","prompt":"again"}),
    )
    .await;
    assert_eq!(
        context(&changed),
        "PLACE cloud=false from=Some(\"linux\") kept=1"
    );
    let (_, lookup) = rpc(
        port,
        "s-cloud",
        "cloud-fixture",
        "tools/call",
        json!({"name":"where_am_i","arguments":{}}),
    )
    .await;
    let lookup: Value =
        serde_json::from_str(lookup["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(lookup["facts"]["cloud"], false);
    assert_eq!(lookup["facts"]["kept_both"][0]["file"], "notes.md");
    assert_eq!(lookup["facts"]["arrival"]["from_os"], "linux");
    assert!(
        state.chat.list().is_empty(),
        "no hook or lookup starts a turn"
    );

    request(&state, Method::DELETE, "/api/v1/pro/configure", None).await;
    server.abort();
    let _ = server.await;
    drop(state);
    std::fs::remove_dir_all(data).unwrap();
}

/// What a move left behind is a file in the daemon's data directory, so a
/// restarted daemon tells its agents the same (no in-memory "returned" flag).
#[tokio::test]
async fn what_a_move_left_behind_survives_a_restart() {
    let data = test_dir("cloud-context-restart").canonicalize().unwrap();
    let first = test_state_with_runtime(data.clone(), Arc::new(Wording));
    crate::mcp::cloud_context::record_arrival(
        &first,
        "w-restart",
        Some(&[PathBuf::from("data/raw.csv")]),
        [Some("linux"), Some("x86_64")],
    )
    .await;
    drop(first);
    let second = test_state_with_runtime(data.clone(), Arc::new(Wording));
    let arrival = crate::mcp::cloud_context::read_arrival(&second, "w-restart")
        .await
        .unwrap();
    assert_eq!(arrival.from_os.as_deref(), Some("linux"));
    assert_eq!(arrival.left_out, ["data/raw.csv"]);
    assert_eq!(arrival.left_out_total, 1);
    // Never in the project folder, and only under a well-formed id.
    assert!(crate::pro::storage(&second)
        .join("w-restart/arrival.json")
        .is_file());
    assert!(
        crate::mcp::cloud_context::read_arrival(&second, "../w-restart")
            .await
            .is_none()
    );
    drop(second);
    std::fs::remove_dir_all(data).unwrap();
}
