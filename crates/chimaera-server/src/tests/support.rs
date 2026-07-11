use crate::*;

pub(super) use std::path::PathBuf;
pub(super) use std::sync::Arc;
pub(super) use tokio::net::TcpListener;

pub(super) use axum::body::Body;
pub(super) use axum::http::{header, Method, Request, StatusCode};
pub(super) use http_body_util::BodyExt;
pub(super) use tower::ServiceExt;

/// Fresh temp directory, unique per call within this test process.
pub(super) fn test_dir(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "chimaera-server-test-{}-{label}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Test state with its workspace registry persisted under a temp dir
/// (equivalent to pointing data_dir at a temp HOME, without the global
/// env-var mutation that races across parallel tests).
pub(super) fn test_state() -> Arc<AppState> {
    test_state_with_port(0)
}

pub(super) fn test_state_with_port(port: u16) -> Arc<AppState> {
    test_state_with_data_dir(port, test_dir("data"))
}

pub(super) fn test_state_with_data_dir(port: u16, data_dir: PathBuf) -> Arc<AppState> {
    let config_dir = data_dir.join("config");
    Arc::new(AppState::new(
        "test-token".to_string(),
        "testhost".to_string(),
        4242,
        port,
        data_dir,
        config_dir,
    ))
}

/// Test state with the Claude transcript store pointed at a fixture dir
/// (equivalent to pointing HOME at a temp dir, without the global
/// env-var mutation that races across parallel tests).
pub(super) fn test_state_with_claude_store(store: PathBuf) -> Arc<AppState> {
    let data = test_dir("data");
    let config = data.join("config");
    let mut state = AppState::new(
        "test-token".to_string(),
        "testhost".to_string(),
        4242,
        0,
        data,
        config,
    );
    state.claude_projects_dir = store;
    Arc::new(state)
}

pub(super) async fn request(
    state: &Arc<AppState>,
    method: Method,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, "Bearer test-token");
    let body = match body {
        Some(json) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let res = app(state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        // Non-JSON bodies (e.g. axum's plain-text extractor rejections)
        // come back as a JSON string so callers can still assert on them.
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
        })
    };
    (status, json)
}

/// Next frame from a tungstenite client stream, with a 10s timeout.
pub(super) async fn next_ws_frame<S>(socket: &mut S) -> tokio_tungstenite::tungstenite::Message
where
    S: futures::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures::StreamExt;
    tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
        .await
        .expect("ws frame timeout")
        .expect("ws stream ended")
        .expect("ws frame error")
}

/// Spawn a real shell session tagged as an agent (synthetic record with a
/// known hook key), without needing a claude binary.
pub(super) fn inject_agent(state: &Arc<AppState>, key: &str) -> String {
    let info = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: test_dir("agent-cwd"),
            name: None,
            cols: 80,
            rows: 24,
            command: None,
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .expect("spawn session");
    lock(&state.agents).insert(
        info.id.clone(),
        agents::AgentRecord::new(key.to_string(), agents::AgentKind::Claude),
    );
    info.id
}

/// Preset the launcher's detection cache for one agent, so tests never
/// hit the real login shell (the same isolation idea as
/// `test_state_with_data_dir`: no global env mutation).
pub(super) fn preset_agent(
    state: &Arc<AppState>,
    kind: agents::AgentKind,
    path: Result<PathBuf, String>,
    version: Option<&str>,
) {
    let managed = path
        .as_ref()
        .is_ok_and(|p| runtimes::is_managed(p, &state.managed_root));
    lock(&state.agent_bins).insert(
        kind,
        launcher::AgentDetection {
            path,
            version: version.map(str::to_string),
            managed,
            explicit: false,
            // No staleness stamp: preset paths are often symbolic
            // (nonexistent), and `mtime: None` entries skip cache validation
            // — presets must never fall through to the real login shell.
            mtime: None,
        },
    );
}

/// The session entry for `id` from GET /api/v1/sessions.
pub(super) async fn session_entry(state: &Arc<AppState>, id: &str) -> serde_json::Value {
    let (status, list) = request(state, Method::GET, "/api/v1/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    list.as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("session {id} not listed in {list}"))
}

/// Register a workspace and return its id.
pub(super) async fn make_workspace(state: &Arc<AppState>, label: &str) -> String {
    let root = test_dir(label);
    let (status, ws) = request(
        state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    ws["id"].as_str().unwrap().to_string()
}

/// Plant a dead-session-to-be agent record: the maps hold what the watch
/// loop would see at the death tick (no live PTY needed — retire never
/// touches the session manager).
pub(super) fn plant_agent_record(
    state: &Arc<AppState>,
    session_id: &str,
    workspace_id: &str,
    kind: agents::AgentKind,
    ai_title: Option<&str>,
    transcript: Option<&str>,
) {
    let mut record = agents::AgentRecord::new("k".to_string(), kind);
    record.ai_title = ai_title.map(str::to_string);
    record.transcript_path = transcript.map(PathBuf::from);
    lock(&state.agents).insert(session_id.to_string(), record);
    lock(&state.session_workspaces).insert(session_id.to_string(), workspace_id.to_string());
}

pub(super) async fn recents_of(
    state: &Arc<AppState>,
    workspace_id: &str,
) -> Vec<serde_json::Value> {
    let (status, body) = request(
        state,
        Method::GET,
        &format!("/api/v1/recents?workspace_id={workspace_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body.as_array().unwrap().clone()
}

/// Write one transcript fixture and backdate its mtime.
pub(super) fn write_transcript(dir: &std::path::Path, name: &str, body: &str, secs_ago: u64) {
    let path = dir.join(format!("{name}.jsonl"));
    std::fs::write(&path, body).unwrap();
    let mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
}

/// POST a synthetic hook payload to the ingest endpoint.
pub(super) async fn post_hook(
    state: &Arc<AppState>,
    id: &str,
    key: &str,
    payload: serde_json::Value,
) -> StatusCode {
    let (status, _) = request(
        state,
        Method::POST,
        &format!("/api/v1/agent-events/{id}?key={key}"),
        Some(payload),
    )
    .await;
    status
}

/// Spawn a real bash (no rc files, so no OSC titles interfere) at `root`,
/// map it to `workspace_id`, and start the naming watcher — the shell
/// equivalent of `inject_agent`.
pub(super) fn inject_shell(
    state: &Arc<AppState>,
    root: &std::path::Path,
    workspace_id: &str,
) -> String {
    let info = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: root.to_path_buf(),
            name: None,
            cols: 80,
            rows: 24,
            command: Some(vec![
                "/bin/bash".to_string(),
                "--noprofile".to_string(),
                "--norc".to_string(),
            ]),
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .expect("spawn shell");
    lock(&state.session_workspaces).insert(info.id.clone(), workspace_id.to_string());
    naming::spawn_shell_watch(state.clone(), info.id.clone());
    info.id
}

/// Poll GET /api/v1/sessions until the session's display_name matches.
pub(super) async fn wait_display_name(state: &Arc<AppState>, id: &str, expected: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let entry = session_entry(state, id).await;
        if entry["display_name"] == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "display_name stuck at {}, want {expected:?}",
            entry["display_name"]
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Poll GET /api/v1/sessions until the session's cwd_current matches.
pub(super) async fn wait_cwd_current(state: &Arc<AppState>, id: &str, expected: &std::path::Path) {
    let expected = serde_json::json!(expected);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let entry = session_entry(state, id).await;
        if entry["cwd_current"] == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "cwd_current stuck at {}, want {expected}",
            entry["cwd_current"]
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Spawn an integrated bash with a hermetic HOME and wait for `ready`.
pub(super) async fn spawn_integrated_bash(state: &Arc<AppState>, label: &str) -> String {
    let base = test_dir(&format!("{label}-base"));
    let home = test_dir(&format!("{label}-home"));
    let launch = chimaera_core::shellint::shell_launch_for("/bin/bash", &base).expect("launch");
    let mut env = launch.env;
    env.push(("HOME".to_string(), home.to_string_lossy().into_owned()));
    let info = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: test_dir(&format!("{label}-cwd")),
            name: None,
            cols: 80,
            rows: 24,
            command: Some(launch.argv),
            id: None,
            env,
            env_remove: Vec::new(),
            scrollback: None,
        })
        .expect("spawn integrated bash");
    let marks = state.sessions.marks(&info.id).expect("marks");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while marks.phase() != chimaera_pty::ShellPhase::Ready {
        assert!(
            tokio::time::Instant::now() < deadline,
            "shell never reached ready"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    info.id
}

/// POST one JSON-RPC message to an agent's MCP endpoint.
pub(super) async fn mcp_post(
    state: &Arc<AppState>,
    agent_id: &str,
    key: &str,
    message: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    request(
        state,
        Method::POST,
        &format!("/api/v1/mcp/{agent_id}?key={key}"),
        Some(message),
    )
    .await
}

/// Call an MCP tool and return (isError, text content).
pub(super) async fn mcp_tool_call(
    state: &Arc<AppState>,
    agent_id: &str,
    key: &str,
    tool: &str,
    args: serde_json::Value,
) -> (bool, String) {
    let (status, out) = mcp_post(
        state,
        agent_id,
        key,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": tool, "arguments": args},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let result = &out["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string();
    (is_error, text)
}

/// Synthetic PostToolUse payload for a file-writing tool, shaped like
/// the real hook payloads (top-level tool_name + tool_input).
pub(super) fn touch_payload(tool: &str, field: &str, path: &str) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": tool,
        "tool_input": { field: path },
    })
}

/// Create a temp repo with one commit; returns (repo dir, git runner).
pub(super) fn init_temp_repo(label: &str) -> PathBuf {
    let repo = test_dir(label);
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .args(args)
            .output()
            .expect("git must be installed");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-qm", "init"]);
    repo
}

/// Percent-encode a path for a query string (tests only).
pub(super) fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Like `request`, but returns the raw response: status, headers, bytes.
/// `token: None` sends no Authorization header (for /raw).
pub(super) async fn request_bytes(
    state: &Arc<AppState>,
    method: Method,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, bytes::Bytes) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let res = app(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes)
}

pub(super) fn header_str<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .unwrap_or_else(|| panic!("missing header {name}"))
        .to_str()
        .unwrap()
}

/// Gzip `content`, optionally recording `fname` as the member's FNAME.
pub(super) fn gzip_bytes(content: &[u8], fname: Option<&str>) -> Vec<u8> {
    use std::io::Write;
    let mut builder = flate2::GzBuilder::new();
    if let Some(name) = fname {
        builder = builder.filename(name);
    }
    let mut encoder = builder.write(Vec::new(), flate2::Compression::default());
    encoder.write_all(content).unwrap();
    encoder.finish().unwrap()
}

/// PUT raw bytes with the bearer token; returns status, headers, body.
pub(super) async fn put_raw(
    state: &Arc<AppState>,
    uri: &str,
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, bytes::Bytes) {
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(uri)
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes)
}

/// Age a file's mtime by `secs` so second-resolution ranking tests do not
/// have to sleep.
pub(super) fn age_file(path: &std::path::Path, secs: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs))
        .unwrap();
}
