mod agents;
mod api;
mod assets;
mod fs;
mod workspaces;
mod ws;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use axum::routing::{delete, get, post};
use axum::{middleware, Router};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

/// Configuration for the chimaera daemon.
pub struct ServerConfig {
    /// Port to bind on 127.0.0.1. `None` lets the OS assign a free port.
    pub port: Option<u16>,
}

/// Shared state for request handlers.
pub(crate) struct AppState {
    pub(crate) token: String,
    pub(crate) started: Instant,
    pub(crate) hostname: String,
    pub(crate) pid: u32,
    /// Port the daemon listens on; embedded in generated agent hook URLs.
    pub(crate) port: u16,
    /// Registered workspaces, persisted to `workspaces.json` on change.
    pub(crate) workspaces: Mutex<workspaces::WorkspaceStore>,
    /// Owner of all PTY sessions; outlives any client connection.
    pub(crate) sessions: Arc<chimaera_pty::SessionManager>,
    /// session id -> workspace id.
    pub(crate) session_workspaces: Mutex<HashMap<String, String>>,
    /// session id -> agent wrapper state (kind "agent" sessions only).
    pub(crate) agents: Mutex<HashMap<String, agents::AgentRecord>>,
    /// Signalled whenever the session list / agent state / titles change;
    /// wakes /ws/events subscribers (a 1s tick catches anything missed).
    pub(crate) changes: tokio::sync::Notify,
    /// `claude` binary resolved once per daemon via the login shell.
    pub(crate) claude_bin: tokio::sync::OnceCell<Result<PathBuf, String>>,
}

impl AppState {
    pub(crate) fn new(
        token: String,
        hostname: String,
        pid: u32,
        port: u16,
        workspaces_path: PathBuf,
    ) -> Self {
        AppState {
            token,
            started: Instant::now(),
            hostname,
            pid,
            port,
            workspaces: Mutex::new(workspaces::WorkspaceStore::load(workspaces_path)),
            sessions: chimaera_pty::SessionManager::new(),
            session_workspaces: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            changes: tokio::sync::Notify::new(),
            claude_bin: tokio::sync::OnceCell::new(),
        }
    }
}

/// Lock a mutex, recovering from poisoning (our critical sections cannot leave
/// the data in a broken state, so a poisoned lock is still usable).
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Build the axum router (factored out so tests can drive it with `oneshot`).
pub(crate) fn app(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/health", get(api::health))
        .route(
            "/workspaces",
            get(api::list_workspaces).post(api::create_workspace),
        )
        .route(
            "/sessions",
            get(api::list_sessions).post(api::create_session),
        )
        .route("/sessions/{id}", delete(api::delete_session))
        .route("/fs/home", get(fs::home))
        .route("/fs/dirs", get(fs::dirs))
        .route_layer(middleware::from_fn_with_state(state.clone(), api::auth))
        // Registered after route_layer, so hook ingestion is NOT behind bearer
        // auth: claude's hooks cannot know the daemon token, so the random
        // per-session key embedded in the hook URL authorizes them instead.
        .route("/agent-events/{id}", post(agents::ingest))
        .with_state(state.clone());

    // The WS routes stay outside the bearer-header middleware: browsers cannot
    // set headers on a WebSocket, so they authenticate via their first frame.
    let ws = Router::new()
        .route("/ws/sessions/{id}", get(ws::session_ws))
        .route("/ws/events", get(ws::events_ws))
        .with_state(state);

    Router::new()
        .nest("/api/v1", api)
        .merge(ws)
        .fallback(assets::static_handler)
        .layer(TraceLayer::new_for_http())
}

/// Bind on 127.0.0.1, write the manifest, and serve until SIGINT/SIGTERM.
pub async fn run(cfg: ServerConfig) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", cfg.port.unwrap_or(0)))
        .await
        .context("failed to bind 127.0.0.1")?;
    let port = listener.local_addr()?.port();

    let token = chimaera_core::generate_token();
    let hostname = hostname::get()
        .context("failed to read hostname")?
        .to_string_lossy()
        .into_owned();
    let pid = std::process::id();
    let started_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();

    let manifest = chimaera_core::Manifest {
        hostname: hostname.clone(),
        port,
        token: token.clone(),
        pid,
        version: chimaera_core::VERSION.to_string(),
        started_at,
    };
    manifest.write().context("failed to write manifest")?;

    println!("chimaera daemon listening on 127.0.0.1:{port}");
    println!("http://127.0.0.1:{port}/#token={token}");

    let state = Arc::new(AppState::new(
        token,
        hostname,
        pid,
        port,
        chimaera_core::data_dir().join("workspaces.json"),
    ));

    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    chimaera_core::Manifest::remove().context("failed to remove manifest")?;
    tracing::info!("chimaera daemon stopped");
    Ok(())
}

/// Resolve when SIGINT (ctrl-c) or SIGTERM is received.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "failed to install ctrl-c handler");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                tracing::error!(%err, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Fresh temp directory, unique per call within this test process.
    fn test_dir(label: &str) -> PathBuf {
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
    fn test_state() -> Arc<AppState> {
        test_state_with_port(0)
    }

    fn test_state_with_port(port: u16) -> Arc<AppState> {
        Arc::new(AppState::new(
            "test-token".to_string(),
            "testhost".to_string(),
            4242,
            port,
            test_dir("data").join("workspaces.json"),
        ))
    }

    async fn request(
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

    #[tokio::test]
    async fn health_without_token_is_401() {
        let res = app(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json, serde_json::json!({"error": "unauthorized"}));
    }

    #[tokio::test]
    async fn health_with_token_is_200() {
        let res = app(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "chimaera");
        assert_eq!(json["version"], chimaera_core::VERSION);
        assert_eq!(json["hostname"], "testhost");
        assert_eq!(json["pid"], 4242);
        assert!(json["uptime_secs"].is_u64());
    }

    #[tokio::test]
    async fn workspaces_with_token_is_empty_list() {
        let res = app(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workspaces")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json, serde_json::json!([]));
    }

    #[tokio::test]
    async fn root_serves_html_without_auth() {
        let res = app(test_state())
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let content_type = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();
        assert!(content_type.starts_with("text/html"));
        let body = res.into_body().collect().await.unwrap().to_bytes();
        assert!(!body.is_empty());
    }

    #[tokio::test]
    async fn spa_fallback_serves_index() {
        let res = app(test_state())
            .oneshot(
                Request::builder()
                    .uri("/some/client/route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let content_type = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();
        assert!(content_type.starts_with("text/html"));
    }

    #[tokio::test]
    async fn workspaces_post_get_round_trip() {
        let state = test_state();
        let root = test_dir("ws-root");
        let root_str = root.to_string_lossy().into_owned();

        // POST registers the directory.
        let (status, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root_str})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = ws["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("w-") && id.len() == 10, "bad id {id}");
        assert!(id[2..].chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
        assert_eq!(
            ws["name"].as_str().unwrap(),
            root.file_name().unwrap().to_str().unwrap()
        );
        assert_eq!(
            ws["root"].as_str().unwrap(),
            std::fs::canonicalize(&root).unwrap().to_str().unwrap()
        );

        // POST again with the same root is idempotent.
        let (status, again) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root_str})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again["id"], ws["id"]);

        // GET lists it.
        let (status, list) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
        assert_eq!(status, StatusCode::OK);
        let list = list.as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["id"], ws["id"]);

        // Nonexistent root is a 400 with an error body.
        let (status, err) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": "/definitely/not/a/dir"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].is_string());
    }

    #[tokio::test]
    async fn sessions_lifecycle() {
        let state = test_state();
        let root = test_dir("sess-root");

        let (status, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let workspace_id = ws["id"].as_str().unwrap().to_string();

        // Spawning against an unknown workspace is a 404.
        let (status, _) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({"workspace_id": "w-00000000", "name": null})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // POST spawns a real shell.
        let (status, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({"workspace_id": workspace_id, "name": null})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = session["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("s-"), "bad session id {id}");
        assert_eq!(session["workspace_id"].as_str().unwrap(), workspace_id);
        assert_eq!(session["cols"], 80);
        assert_eq!(session["rows"], 24);
        assert_eq!(session["alive"], true);

        // GET lists it, alive.
        let (status, list) = request(&state, Method::GET, "/api/v1/sessions", None).await;
        assert_eq!(status, StatusCode::OK);
        let entry = list
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == session["id"])
            .expect("session listed");
        assert_eq!(entry["alive"], true);
        assert_eq!(entry["workspace_id"].as_str().unwrap(), workspace_id);

        // DELETE kills it.
        let (status, _) = request(
            &state,
            Method::DELETE,
            &format!("/api/v1/sessions/{id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Afterwards the session is gone or reported dead (the shell may take
        // a moment to reap, so poll briefly).
        let mut gone_or_dead = false;
        for _ in 0..50 {
            let (_, list) = request(&state, Method::GET, "/api/v1/sessions", None).await;
            match list.as_array().unwrap().iter().find(|s| s["id"] == id) {
                None => gone_or_dead = true,
                Some(entry) if entry["alive"] == false => gone_or_dead = true,
                Some(_) => {}
            }
            if gone_or_dead {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(gone_or_dead, "session still alive after DELETE");
    }

    /// Next frame from a tungstenite client stream, with a 10s timeout.
    async fn next_ws_frame<S>(socket: &mut S) -> tokio_tungstenite::tungstenite::Message
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

    #[tokio::test]
    async fn ws_bridge_auth_snapshot_and_echo() {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let state = test_state();
        let cwd = test_dir("ws-cwd");
        let info = state
            .sessions
            .spawn(chimaera_pty::SpawnOpts {
                cwd,
                name: None,
                cols: 80,
                rows: 24,
                command: None,
                id: None,
            })
            .expect("spawn session");

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let url = format!("ws://{addr}/ws/sessions/{}", info.id);
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // 1. First-frame auth.
        socket
            .send(WsMessage::text(
                serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
            ))
            .await
            .unwrap();

        // 2. Ready text frame with the SessionInfo fields.
        let ready = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            other => panic!("expected ready text frame, got {other:?}"),
        };
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready["id"].as_str().unwrap(), info.id);

        // 3. Snapshot as one binary frame.
        match next_ws_frame(&mut socket).await {
            WsMessage::Binary(_) => {}
            other => panic!("expected snapshot binary frame, got {other:?}"),
        }

        // 4. Send input; the echoed output must come back as binary frames.
        socket
            .send(WsMessage::binary(&b"echo ws-test\n"[..]))
            .await
            .unwrap();

        let mut collected = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !String::from_utf8_lossy(&collected).contains("ws-test") {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no ws-test output; got: {}",
                String::from_utf8_lossy(&collected)
            );
            match next_ws_frame(&mut socket).await {
                WsMessage::Binary(bytes) => collected.extend_from_slice(&bytes),
                WsMessage::Text(_) => {} // events are fine to interleave
                other => panic!("unexpected frame {other:?}"),
            }
        }

        state.sessions.kill(&info.id).ok();
    }

    #[tokio::test]
    async fn ws_bad_token_is_rejected() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let state = test_state();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let url = format!("ws://{addr}/ws/sessions/s-00000000");
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        socket
            .send(WsMessage::text(
                serde_json::json!({"type": "auth", "token": "wrong"}).to_string(),
            ))
            .await
            .unwrap();

        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
            .await
            .expect("ws frame timeout")
            .expect("ws stream ended")
            .expect("ws frame error");
        match frame {
            WsMessage::Text(text) => {
                let json: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(json["type"], "error");
                assert_eq!(json["message"], "unauthorized");
            }
            other => panic!("expected error text frame, got {other:?}"),
        }
    }

    /// Spawn a real shell session tagged as an agent (synthetic record with a
    /// known hook key), without needing a claude binary.
    fn inject_agent(state: &Arc<AppState>, key: &str) -> String {
        let info = state
            .sessions
            .spawn(chimaera_pty::SpawnOpts {
                cwd: test_dir("agent-cwd"),
                name: None,
                cols: 80,
                rows: 24,
                command: None,
                id: None,
            })
            .expect("spawn session");
        lock(&state.agents).insert(info.id.clone(), agents::AgentRecord::new(key.to_string()));
        info.id
    }

    /// The session entry for `id` from GET /api/v1/sessions.
    async fn session_entry(state: &Arc<AppState>, id: &str) -> serde_json::Value {
        let (status, list) = request(state, Method::GET, "/api/v1/sessions", None).await;
        assert_eq!(status, StatusCode::OK);
        list.as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("session {id} not listed in {list}"))
    }

    #[tokio::test]
    async fn session_kind_defaults_to_shell_and_round_trips() {
        let state = test_state();
        let root = test_dir("kind-root");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let workspace_id = ws["id"].as_str().unwrap().to_string();

        // No kind in the body -> shell.
        let (status, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({"workspace_id": workspace_id, "name": null})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(session["kind"], "shell");
        assert_eq!(session["agent_state"], serde_json::Value::Null);
        assert_eq!(session["agent_title"], serde_json::Value::Null);

        // Explicit kind "shell" round-trips through GET.
        let (status, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({"workspace_id": workspace_id, "kind": "shell"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let entry = session_entry(&state, session["id"].as_str().unwrap()).await;
        assert_eq!(entry["kind"], "shell");
        assert_eq!(entry["agent_state"], serde_json::Value::Null);
        assert_eq!(entry["agent_title"], serde_json::Value::Null);

        // An unknown kind is a 400 (serde rejects it).
        let (status, _) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({"workspace_id": workspace_id, "kind": "bogus"})),
        )
        .await;
        assert_ne!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn create_agent_without_claude_is_409_with_hint() {
        let state = test_state();
        state
            .claude_bin
            .set(Err("claude not found via login shell (test)".to_string()))
            .expect("preset claude_bin");
        let root = test_dir("agent-409");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let (status, body) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({"workspace_id": ws["id"], "kind": "agent"})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("claude not found"));
    }

    #[tokio::test]
    async fn create_agent_spawns_command_with_generated_settings() {
        let state = test_state_with_port(45678);
        // A stand-in "claude": exits immediately, but exercises the whole
        // spawn path (settings generation, id pre-pick, record registration).
        state
            .claude_bin
            .set(Ok(PathBuf::from("/bin/echo")))
            .expect("preset claude_bin");
        let root = test_dir("agent-spawn");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let (status, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({"workspace_id": ws["id"], "kind": "agent"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(session["kind"], "agent");
        assert_eq!(session["agent_state"], "unknown");
        assert_eq!(session["agent_title"], serde_json::Value::Null);
        let id = session["id"].as_str().unwrap().to_string();

        // The generated settings file wires every hook to this daemon+session.
        let settings_path = chimaera_core::runtime_dir()
            .join("agents")
            .join(format!("{id}-settings.json"));
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        let key = lock(&state.agents)
            .get(&id)
            .map(|r| r.key.clone())
            .expect("agent record registered");
        let url = settings["hooks"]["SessionStart"][0]["hooks"][0]["url"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            url,
            format!("http://127.0.0.1:45678/api/v1/agent-events/{id}?key={key}")
        );
        std::fs::remove_file(&settings_path).ok();
    }

    /// POST a synthetic hook payload to the ingest endpoint.
    async fn post_hook(
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

    #[tokio::test]
    async fn agent_events_rejects_bad_key_and_unknown_session() {
        let state = test_state();
        let id = inject_agent(&state, "right-key");

        let payload = serde_json::json!({"hook_event_name": "Stop"});
        assert_eq!(
            post_hook(&state, &id, "wrong-key", payload.clone()).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            post_hook(&state, &id, "", payload.clone()).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            post_hook(&state, "s-00000000", "right-key", payload.clone()).await,
            StatusCode::NOT_FOUND
        );
        // A bad key must not change state.
        assert_eq!(session_entry(&state, &id).await["agent_state"], "unknown");
        // The right key works.
        assert_eq!(
            post_hook(&state, &id, "right-key", payload).await,
            StatusCode::OK
        );
        assert_eq!(session_entry(&state, &id).await["agent_state"], "finished");
        state.sessions.kill(&id).ok();
    }

    #[tokio::test]
    async fn agent_events_state_transitions() {
        let state = test_state();
        let id = inject_agent(&state, "k");

        let cases = [
            (
                serde_json::json!({"hook_event_name": "SessionStart", "source": "startup"}),
                "running",
            ),
            (
                serde_json::json!({
                    "hook_event_name": "Notification",
                    "notification_type": "permission_prompt",
                    "message": "Claude needs your permission to use Bash",
                }),
                "needs_permission",
            ),
            (
                serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash"}),
                "running",
            ),
            (
                serde_json::json!({
                    "hook_event_name": "Notification",
                    "notification_type": "idle_prompt",
                    "message": "Claude is waiting for your input",
                }),
                "idle_prompt",
            ),
            (
                serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}),
                "running",
            ),
            (serde_json::json!({"hook_event_name": "Stop"}), "finished"),
            (
                serde_json::json!({"hook_event_name": "StopFailure", "error_type": "rate_limit"}),
                "rate_limited",
            ),
            (
                serde_json::json!({"hook_event_name": "StopFailure", "error_type": "server_error"}),
                "errored",
            ),
            // SessionEnd keeps the last state.
            (
                serde_json::json!({"hook_event_name": "SessionEnd", "reason": "other"}),
                "errored",
            ),
        ];
        for (payload, expected) in cases {
            let event = payload["hook_event_name"].as_str().unwrap().to_string();
            assert_eq!(post_hook(&state, &id, "k", payload).await, StatusCode::OK);
            assert_eq!(
                session_entry(&state, &id).await["agent_state"],
                *expected,
                "after {event}"
            );
        }
        state.sessions.kill(&id).ok();
    }

    #[tokio::test]
    async fn agent_title_tail_polls_transcript() {
        let state = test_state();
        let id = inject_agent(&state, "k");
        agents::spawn_agent_watch(state.clone(), id.clone());

        // Synthetic SessionStart pointing transcript_path at a fixture file.
        let transcript = test_dir("transcript").join("session.jsonl");
        std::fs::write(&transcript, "{\"type\":\"message\"}\n").unwrap();
        let status = post_hook(
            &state,
            &id,
            "k",
            serde_json::json!({
                "hook_event_name": "SessionStart",
                "source": "startup",
                "session_id": "5e0d64b2-abcd-abcd-abcd-000000000000",
                "transcript_path": transcript.to_string_lossy(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let wait_for_title = |expected: &'static str| {
            let state = state.clone();
            let id = id.clone();
            async move {
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    let title = session_entry(&state, &id).await["agent_title"].clone();
                    if title == expected {
                        return;
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "agent_title stuck at {title}, want {expected}"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        };

        // An appended ai-title record becomes the title...
        let mut line = serde_json::json!(
            {"type": "ai-title", "aiTitle": "Fix the flaky tests", "sessionId": "x"}
        )
        .to_string();
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        std::io::Write::write_all(&mut file, line.as_bytes()).unwrap();
        wait_for_title("Fix the flaky tests").await;

        // ...and a later customTitle record wins over it.
        let mut line =
            serde_json::json!({"type": "custom-title", "customTitle": "My run"}).to_string();
        line.push('\n');
        std::io::Write::write_all(&mut file, line.as_bytes()).unwrap();
        wait_for_title("My run").await;

        state.sessions.kill(&id).ok();
    }

    #[tokio::test]
    async fn ws_events_auth_snapshot_and_change_push() {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let state = test_state();
        let first = inject_agent(&state, "k"); // one agent session pre-existing

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let url = format!("ws://{addr}/ws/events");
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        socket
            .send(WsMessage::text(
                serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
            ))
            .await
            .unwrap();

        // Initial full snapshot arrives immediately after auth.
        let snapshot = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            other => panic!("expected sessions text frame, got {other:?}"),
        };
        assert_eq!(snapshot["type"], "sessions");
        let entry = snapshot["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == first)
            .expect("existing session in snapshot");
        assert_eq!(entry["kind"], "agent");
        assert_eq!(entry["agent_state"], "unknown");

        // A state change pushes a fresh snapshot.
        let (status, _) = request(
            &state,
            Method::POST,
            &format!("/api/v1/agent-events/{first}?key=k"),
            Some(serde_json::json!({"hook_event_name": "Stop"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no snapshot with finished state"
            );
            let frame = match next_ws_frame(&mut socket).await {
                WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                _ => continue,
            };
            let done = frame["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["id"] == first && s["agent_state"] == "finished");
            if done {
                break;
            }
        }

        // A disappearing session (killed PTY) is caught by the fallback tick.
        state.sessions.kill(&first).ok();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "killed session never left the snapshot"
            );
            let frame = match next_ws_frame(&mut socket).await {
                WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                _ => continue,
            };
            let gone = !frame["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["id"] == first);
            if gone {
                break;
            }
        }
    }

    #[tokio::test]
    async fn ws_events_bad_token_is_rejected() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let state = test_state();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let url = format!("ws://{addr}/ws/events");
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        socket
            .send(WsMessage::text(
                serde_json::json!({"type": "auth", "token": "wrong"}).to_string(),
            ))
            .await
            .unwrap();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
            .await
            .expect("ws frame timeout")
            .expect("ws stream ended")
            .expect("ws frame error");
        match frame {
            WsMessage::Text(text) => {
                let json: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(json["type"], "error");
                assert_eq!(json["message"], "unauthorized");
            }
            other => panic!("expected error text frame, got {other:?}"),
        }
    }

    /// End-to-end against the real `claude` binary: spawn kind=agent, watch
    /// the TUI come up in the PTY, and wait for a real hook POST to flip the
    /// agent state. Gated behind CHIMAERA_TEST_CLAUDE=1 so CI without claude
    /// (or without a subscription) stays green.
    #[tokio::test]
    async fn real_claude_agent_session() {
        if std::env::var("CHIMAERA_TEST_CLAUDE").as_deref() != Ok("1") {
            eprintln!("skipping real_claude_agent_session (set CHIMAERA_TEST_CLAUDE=1)");
            return;
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = test_state_with_port(port);
        let router = app(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let root = test_dir("claude-agent");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let (status, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({"workspace_id": ws["id"], "kind": "agent"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "agent spawn failed: {session}");
        assert_eq!(session["kind"], "agent");
        let id = session["id"].as_str().unwrap().to_string();

        // 1. The claude TUI comes up in the PTY.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "claude TUI never appeared in the PTY snapshot"
            );
            let text = match state.sessions.attach(&id) {
                Ok(att) => String::from_utf8_lossy(&att.snapshot).to_string(),
                Err(_) => String::new(),
            };
            if text.to_lowercase().contains("claude") {
                eprintln!("TUI is up (snapshot contains 'claude')");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        // 2. A real hook POST flips the state away from "unknown". Nudge the
        // TUI if needed: Enter dismisses a possible trust dialog, then a tiny
        // prompt guarantees a UserPromptSubmit hook.
        let attachment = state.sessions.attach(&id).expect("attach for input");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut nudges = 0u32;
        loop {
            let entry = session_entry(&state, &id).await;
            let agent_state = entry["agent_state"].as_str().unwrap_or("").to_string();
            if agent_state != "unknown" {
                eprintln!("hook flipped agent_state to {agent_state}");
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no hook POST ever flipped the state"
            );
            let elapsed = 120 - (deadline - tokio::time::Instant::now()).as_secs();
            if elapsed > 10 && nudges == 0 {
                eprintln!("nudge: Enter (possible trust dialog)");
                attachment.input.send(bytes::Bytes::from("\r")).await.ok();
                nudges = 1;
            } else if elapsed > 20 && nudges == 1 {
                eprintln!("nudge: submitting a tiny prompt");
                attachment
                    .input
                    .send(bytes::Bytes::from("reply with just: ok\r"))
                    .await
                    .ok();
                nudges = 2;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        // Bonus observation (not asserted): the title tail-poll may pick up
        // claude's ai-title record.
        for _ in 0..30 {
            let entry = session_entry(&state, &id).await;
            if let Some(title) = entry["agent_title"].as_str() {
                eprintln!("observed agent_title from transcript: {title}");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        let final_entry = session_entry(&state, &id).await;
        eprintln!(
            "final session entry: state={} title={}",
            final_entry["agent_state"], final_entry["agent_title"]
        );

        let (status, _) = request(
            &state,
            Method::DELETE,
            &format!("/api/v1/sessions/{id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn fs_home_returns_real_home() {
        let (status, json) = request(&test_state(), Method::GET, "/api/v1/fs/home", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json["path"].as_str().unwrap(),
            std::env::var("HOME").unwrap()
        );
    }

    #[tokio::test]
    async fn fs_dirs_lists_only_directories_sorted() {
        let state = test_state();
        let root = test_dir("fs-list");
        std::fs::create_dir(root.join("Zebra")).unwrap();
        std::fs::create_dir(root.join("apple")).unwrap();
        std::fs::create_dir(root.join("Mango")).unwrap();
        std::fs::create_dir(root.join(".config")).unwrap();
        std::fs::write(root.join("notes.txt"), "not a dir").unwrap();
        std::os::unix::fs::symlink(root.join("apple"), root.join("orchard")).unwrap();
        std::os::unix::fs::symlink(root.join("notes.txt"), root.join("shortcut")).unwrap();
        std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).unwrap();

        let canonical = std::fs::canonicalize(&root).unwrap();
        let names = |json: &serde_json::Value| -> Vec<String> {
            json["dirs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d["name"].as_str().unwrap().to_string())
                .collect()
        };

        // Default: dot-directories hidden; files and non-dir symlinks never
        // listed; case-insensitive order (byte order would put Mango first).
        let uri = format!("/api/v1/fs/dirs?path={}", root.to_string_lossy());
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["path"].as_str().unwrap(), canonical.to_str().unwrap());
        assert_eq!(
            json["parent"].as_str().unwrap(),
            canonical.parent().unwrap().to_str().unwrap()
        );
        assert_eq!(names(&json), ["apple", "Mango", "orchard", "Zebra"]);
        assert_eq!(
            json["dirs"][0]["path"].as_str().unwrap(),
            canonical.join("apple").to_str().unwrap()
        );

        // hidden=true adds the dot-directory; still no files.
        let uri = format!(
            "/api/v1/fs/dirs?path={}&hidden=true",
            root.to_string_lossy()
        );
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            names(&json),
            [".config", "apple", "Mango", "orchard", "Zebra"]
        );
    }

    #[tokio::test]
    async fn fs_dirs_expands_tilde() {
        let (status, json) =
            request(&test_state(), Method::GET, "/api/v1/fs/dirs?path=~", None).await;
        assert_eq!(status, StatusCode::OK);
        let home = std::fs::canonicalize(std::env::var("HOME").unwrap()).unwrap();
        assert_eq!(json["path"].as_str().unwrap(), home.to_str().unwrap());
        assert!(json["parent"].is_string());
        assert!(json["dirs"].is_array());
    }

    #[tokio::test]
    async fn fs_dirs_rejects_files_and_missing_paths() {
        let state = test_state();
        let root = test_dir("fs-bad");
        let file = root.join("plain.txt");
        std::fs::write(&file, "x").unwrap();

        let uri = format!("/api/v1/fs/dirs?path={}", file.to_string_lossy());
        let (status, err) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].is_string());

        let (status, err) = request(
            &state,
            Method::GET,
            "/api/v1/fs/dirs?path=/definitely/not/a/dir",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].is_string());
    }

    #[tokio::test]
    async fn fs_endpoints_without_token_are_401() {
        for uri in ["/api/v1/fs/home", "/api/v1/fs/dirs?path=/"] {
            let res = app(test_state())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
    }

    #[tokio::test]
    async fn unknown_api_path_is_404() {
        let res = app(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
