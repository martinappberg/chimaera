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
    start_as(state, port, workspace, "s-cloud").await
}

/// [`agent_start`] for one session: a fresh record, as each agent process
/// (a spawn, a restart's resurrection, a view switch's respawn) makes one.
async fn start_as(
    state: &Arc<AppState>,
    port: u16,
    workspace: &str,
    session: &str,
) -> (Value, Vec<String>, Value, Value, Vec<String>) {
    lock(&state.agents).insert(
        session.into(),
        AgentRecord::new("cloud-fixture".into(), AgentKind::Claude),
    );
    lock(&state.session_workspaces).insert(session.into(), workspace.into());
    let (_, init) = rpc(port, session, "cloud-fixture", "initialize", json!({})).await;
    let (_, tools) = rpc(port, session, "cloud-fixture", "tools/list", json!({})).await;
    let names = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect();
    let start = hook(
        state,
        session,
        "cloud-fixture",
        json!({"hook_event_name":"SessionStart","source":"startup"}),
    )
    .await;
    let prompt = hook(
        state,
        session,
        "cloud-fixture",
        json!({"hook_event_name":"UserPromptSubmit","prompt":"where am I?"}),
    )
    .await;
    let allowed = crate::plugins::spawn_allow(state, workspace).await;
    (init["result"].clone(), names, start, prompt, allowed)
}

fn recipe(workspace: &str, root: &std::path::Path, kind: AgentKind) -> crate::chat::ChatRecipe {
    crate::chat::ChatRecipe {
        workspace_root: root.to_path_buf(),
        workspace_id: workspace.into(),
        kind,
        bin: "/bin/cat".into(),
        version: None,
        settings: None,
        mcp_config: None,
        model: None,
        resume: Some("saved-native-id".into()),
        fork_at: None,
        fork_head: false,
        rollback_turns: None,
        revert_before_turn: None,
        remote_control: crate::chat::RemoteControlAtStart::No,
        carry_ultracode: false,
        theme: "dark".into(),
        prelude: None,
        mastermind: None,
        portable_context: None,
        created_at_ms: None,
    }
}

fn sha256_hex(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The free values, written out. They are what `origin/main` (2fc83a19)
/// hands an agent in a plugin-free workspace with agent communication on:
/// the producing code (`mcp::INSTRUCTIONS`, `DOCUMENTS_INSTRUCTIONS`,
/// `comms::instructions`, `tool_defs`, `plugins::spawn_allow`, the codex
/// cluster note in `chat::spawn_chat_session`) is unchanged from there apart
/// from branches the optional Runtime gates. A change here changes what free
/// users' agents see: it must be deliberate.
const FREE_TOOLS: &[&str] = &[
    "list_terminals",
    "run_in_terminal",
    "read_terminal",
    "document_guide",
    "check_document",
    "notify",
    "open_browser",
    "workspace_agents",
    "read_agent",
    "message_agent",
    "read_messages",
];
const FREE_ALLOWED: &[&str] = &[
    "workspace_agents",
    "read_agent",
    "message_agent",
    "read_messages",
];
/// The 3,023-byte instructions for session `s-cloud`.
const FREE_INSTRUCTIONS_SHA256: &str =
    "e75881513b56637978c0b6122acf53e46764104f10fc38ee3e453f3b863a389b";

fn assert_free(label: &str, start: &(Value, Vec<String>, Value, Value, Vec<String>)) {
    let (init, names, start_hook, prompt_hook, allowed) = start;
    let instructions = init["instructions"].as_str().unwrap();
    assert_eq!(names, FREE_TOOLS, "{label}: tools/list");
    assert_eq!(allowed, FREE_ALLOWED, "{label}: pre-approved tools");
    assert_eq!(*start_hook, json!({}), "{label}: SessionStart answer");
    assert_eq!(*prompt_hook, json!({}), "{label}: UserPromptSubmit answer");
    assert_eq!(
        sha256_hex(instructions),
        FREE_INSTRUCTIONS_SHA256,
        "{label}: MCP initialize instructions:\n{instructions}"
    );
    assert!(!instructions.contains("where_am_i") && !instructions.contains("cloud machine"));
    assert_eq!(init["capabilities"], json!({"tools":{}}), "{label}");
}

/// The free contract: a daemon without the optional Runtime hands its agents
/// exactly what `origin/main` does, even for a project whose account is
/// configured and enrolled and that has a move recorded. Pinned as literal
/// values: the hook answers, the MCP instructions, the tools, the
/// pre-approved tools and the Codex developer note; `where_am_i` is an
/// unknown tool, and no record is written.
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
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = crate::app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let codex = recipe(&workspace.id, &root, AgentKind::Codex);
    let check = |label: &'static str| {
        let state = state.clone();
        let workspace = workspace.id.clone();
        let codex = codex.clone();
        async move {
            let start = agent_start(&state, port, &workspace).await;
            assert_free(label, &start);
            let (note, placement) =
                crate::chat::codex_developer_note(&state, &codex, "s-cloud").await;
            assert_eq!(note, None, "{label}: codex developer note");
            assert!(placement.is_none());
            let (status, answer) = rpc(
                port,
                "s-cloud",
                "cloud-fixture",
                "tools/call",
                json!({"name":"where_am_i","arguments":{}}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                answer["error"],
                json!({"code":-32602,"message":"unknown tool: where_am_i"}),
                "{label}: {answer}"
            );
        }
    };
    check("never configured").await;
    for role in ["worker", "device"] {
        // A free daemon has no `/pro/*`: no account can configure it.
        let (status, _) = request(
            &state,
            Method::POST,
            "/api/v1/pro/configure",
            Some(json!({"role": role})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{role}");
        crate::pro::enroll_for_tests(&state, &workspace.id);
        crate::mcp::cloud_context::record_arrival(
            &state,
            &workspace.id,
            Some(&[PathBuf::from("data.csv")]),
            [Some("macos"), Some("aarch64")],
        )
        .await;
        check(role).await;
        assert!(crate::mcp::cloud_context::note(&state, &workspace.id)
            .await
            .is_none());
        assert!(!crate::mcp::cloud_context::available_in(
            &state,
            &workspace.id
        ));
    }
    assert!(!crate::pro::storage(&state).join(&workspace.id).exists());
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

/// With the Runtime, only an enrolled project's agents hear where they run.
/// Each conversation hears it once per change of machine: at its first
/// start (SessionStart, or the first prompt where SessionStart did not
/// fire), not again when its process restarts or switches view, never on a
/// subagent's hook, and again after a move or a new kept-both pair. The
/// Codex developer note follows the same rule, and signing out removes what
/// was recorded.
#[tokio::test]
async fn a_synced_projects_agents_hear_where_they_run_once_per_change() {
    let data = test_dir("cloud-context-carrier").canonicalize().unwrap();
    let state = test_state_with_runtime(data.clone(), Arc::new(Wording));
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
    let root = data.join("project");
    std::fs::create_dir(&root).unwrap();
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = crate::app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    configure(&state, port, "worker").await;

    // Signed in, but the project never enrolled: exactly the free start.
    let (_, names, start, prompt, allowed) = agent_start(&state, port, &workspace.id).await;
    assert!(!names.contains(&"where_am_i".to_owned()));
    assert!(!allowed.contains(&"where_am_i".to_owned()));
    assert_eq!((start, prompt), (json!({}), json!({})));
    assert!(crate::mcp::cloud_context::note(&state, &workspace.id)
        .await
        .is_none());

    crate::pro::enroll_for_tests(&state, &workspace.id);
    let (init, names, start, prompt, allowed) = agent_start(&state, port, &workspace.id).await;
    assert!(
        !init.to_string().contains("PLACE cloud"),
        "never in instructions"
    );
    assert_eq!(names.iter().filter(|name| *name == "where_am_i").count(), 1);
    assert!(allowed.contains(&"where_am_i".to_owned()));
    assert_eq!(context(&start), "PLACE cloud=true from=None kept=0");
    assert_eq!(prompt, json!({}), "not repeated within one start");

    // The same conversation's next process (a restart, a view switch): its
    // history already holds the note, so nothing is added.
    let (_, _, again, again_prompt, _) = agent_start(&state, port, &workspace.id).await;
    assert_eq!((again, again_prompt), (json!({}), json!({})));
    assert!(crate::pro::storage(&state)
        .join(&workspace.id)
        .join("told.json")
        .is_file());

    // A subagent's hook speaks for the subagent: never the carrier.
    lock(&state.agents).insert(
        "s-tui".into(),
        AgentRecord::new("cloud-fixture".into(), AgentKind::Claude),
    );
    lock(&state.session_workspaces).insert("s-tui".into(), workspace.id.clone());
    let sub = hook(
        &state,
        "s-tui",
        "cloud-fixture",
        json!({"hook_event_name":"SessionStart","agent_id":"a-1","agent_type":"Explore"}),
    )
    .await;
    assert!(!context(&sub).contains("PLACE cloud"));
    // A terminal where SessionStart did not fire: the first prompt carries it.
    let first = hook(
        &state,
        "s-tui",
        "cloud-fixture",
        json!({"hook_event_name":"UserPromptSubmit","prompt":"hi"}),
    )
    .await;
    assert_eq!(context(&first), "PLACE cloud=true from=None kept=0");

    // A Codex chat: the developer note once, then not on its next spawn.
    let codex = recipe(&workspace.id, &root, AgentKind::Codex);
    let (note, placement) = crate::chat::codex_developer_note(&state, &codex, "s-codex").await;
    assert_eq!(note.as_deref(), Some("PLACE cloud=true from=None kept=0"));
    crate::mcp::cloud_context::told(&state, &placement.unwrap()).await;
    let (note, placement) = crate::chat::codex_developer_note(&state, &codex, "s-codex").await;
    assert_eq!((note, placement.is_none()), (None, true));
    let claude = recipe(&workspace.id, &root, AgentKind::Claude);
    assert!(
        crate::chat::codex_developer_note(&state, &claude, "s-claude")
            .await
            .0
            .is_none()
    );

    // The project comes home: a new move into a different machine, so the
    // same conversation hears where it runs now.
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
    // A new kept-both pair reaches the same process at its next prompt.
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
    let (_, _, quiet, quiet_prompt, _) = agent_start(&state, port, &workspace.id).await;
    assert_eq!((quiet, quiet_prompt), (json!({}), json!({})));
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

    // Signing out leaves no project file names behind.
    request(&state, Method::DELETE, "/api/v1/pro/configure", None).await;
    let kept = crate::pro::storage(&state).join(&workspace.id);
    assert!(!kept.join("arrival.json").exists() && !kept.join("told.json").exists());
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
