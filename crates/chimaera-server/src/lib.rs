mod agents;
mod api;
mod assets;
mod fs;
mod links;
mod mcp;
mod naming;
mod quickopen;
mod view_state;
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
    /// Per-window view state (layout trees etc.), persisted to
    /// `view-state.json` on change.
    pub(crate) view_state: Mutex<view_state::ViewStateStore>,
    /// Owner of all PTY sessions; outlives any client connection.
    pub(crate) sessions: Arc<chimaera_pty::SessionManager>,
    /// session id -> workspace id.
    pub(crate) session_workspaces: Mutex<HashMap<String, String>>,
    /// session id -> agent wrapper state (kind "agent" sessions only).
    pub(crate) agents: Mutex<HashMap<String, agents::AgentRecord>>,
    /// session id -> polled shell display name (naming rule zero); written
    /// by the per-session watcher in `naming`, read by `session_json`.
    pub(crate) display_names: Mutex<HashMap<String, String>>,
    /// session id -> stage of a currently in-flight agent exec (queued /
    /// executing); drives the linked-terminal chips in the UI.
    pub(crate) exec_status: Mutex<HashMap<String, chimaera_pty::ExecStage>>,
    /// terminal session id -> agent session id: the linked-terminal edges
    /// (one agent per terminal; see the `links` module).
    pub(crate) links: Mutex<HashMap<String, String>>,
    /// Short-lived raw-access tickets for /raw/{ticket} (in-memory only).
    pub(crate) tickets: Mutex<fs::TicketStore>,
    /// Quick-open walk cache (short TTL, per workspace).
    pub(crate) quickopen: Mutex<quickopen::QuickOpenCache>,
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
        data_dir: PathBuf,
    ) -> Self {
        AppState {
            token,
            started: Instant::now(),
            hostname,
            pid,
            port,
            workspaces: Mutex::new(workspaces::WorkspaceStore::load(
                data_dir.join("workspaces.json"),
            )),
            view_state: Mutex::new(view_state::ViewStateStore::load(
                data_dir.join("view-state.json"),
            )),
            sessions: chimaera_pty::SessionManager::new(),
            session_workspaces: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            display_names: Mutex::new(HashMap::new()),
            exec_status: Mutex::new(HashMap::new()),
            links: Mutex::new(HashMap::new()),
            tickets: Mutex::new(fs::TicketStore::default()),
            quickopen: Mutex::new(quickopen::QuickOpenCache::default()),
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
        .route("/sessions/{id}/exec", post(api::exec_session))
        .route("/sessions/{id}/journal", get(api::session_journal))
        .route("/links", get(links::list_links).put(links::put_link))
        .route("/links/{terminal_id}", delete(links::delete_link))
        .route(
            "/view-state/{key}",
            get(view_state::get_view_state).put(view_state::put_view_state),
        )
        .route("/fs/home", get(fs::home))
        .route("/fs/dirs", get(fs::dirs))
        .route("/fs/list", get(fs::list))
        .route("/fs/file", get(fs::file).put(fs::put_file))
        .route("/fs/markdown", get(fs::markdown))
        .route("/fs/table", get(fs::table))
        .route("/fs/quickopen", get(quickopen::quickopen))
        .route("/fs/ticket", post(fs::create_ticket))
        .route_layer(middleware::from_fn_with_state(state.clone(), api::auth))
        // Registered after route_layer, so hook ingestion is NOT behind bearer
        // auth: claude's hooks cannot know the daemon token, so the random
        // per-session key embedded in the hook URL authorizes them instead.
        .route("/agent-events/{id}", post(agents::ingest))
        // Same key-in-URL auth story as agent-events: claude's MCP client
        // cannot know the daemon bearer token.
        .route("/mcp/{id}", post(mcp::mcp))
        .with_state(state.clone());

    // The WS routes stay outside the bearer-header middleware: browsers cannot
    // set headers on a WebSocket, so they authenticate via their first frame.
    // /raw/{ticket} is also unauthenticated: iframes and img tags cannot send
    // Authorization headers, so a short-lived single-path ticket (minted via
    // the bearer-authed POST /api/v1/fs/ticket) authorizes each fetch instead.
    let ws = Router::new()
        .route("/ws/sessions/{id}", get(ws::session_ws))
        .route("/ws/events", get(ws::events_ws))
        .route("/raw/{ticket}", get(fs::raw))
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
        chimaera_core::data_dir(),
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
        test_state_with_data_dir(port, test_dir("data"))
    }

    fn test_state_with_data_dir(port: u16, data_dir: PathBuf) -> Arc<AppState> {
        Arc::new(AppState::new(
            "test-token".to_string(),
            "testhost".to_string(),
            4242,
            port,
            data_dir,
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

        // A fresh shell is named after the shell binary itself (naming rule
        // zero: it sits idle at the workspace root), and nothing is pinned.
        assert_eq!(
            session["display_name"].as_str().unwrap(),
            naming::default_shell_name()
        );
        assert_eq!(session["renamed"], false);

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
        assert!(entry["display_name"].is_string());

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
                env: Vec::new(),
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
                env: Vec::new(),
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

    /// Spawn a real bash (no rc files, so no OSC titles interfere) at `root`,
    /// map it to `workspace_id`, and start the naming watcher — the shell
    /// equivalent of `inject_agent`.
    fn inject_shell(state: &Arc<AppState>, root: &std::path::Path, workspace_id: &str) -> String {
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
            })
            .expect("spawn shell");
        lock(&state.session_workspaces).insert(info.id.clone(), workspace_id.to_string());
        naming::spawn_shell_watch(state.clone(), info.id.clone());
        info.id
    }

    /// Poll GET /api/v1/sessions until the session's display_name matches.
    async fn wait_display_name(state: &Arc<AppState>, id: &str, expected: &str) {
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

    #[tokio::test]
    async fn shell_display_name_tracks_foreground_command() {
        let state = test_state();
        let root = test_dir("naming-fg");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let workspace_id = ws["id"].as_str().unwrap().to_string();
        let root = std::fs::canonicalize(&root).unwrap();
        let id = inject_shell(&state, &root, &workspace_id);

        // Idle at the workspace root: named after the shell binary.
        wait_display_name(&state, &id, "bash").await;
        assert_eq!(session_entry(&state, &id).await["renamed"], false);

        // A running foreground command takes over the name...
        let att = state.sessions.attach(&id).expect("attach");
        att.input
            .send(bytes::Bytes::from("sleep 5\n"))
            .await
            .expect("send input");
        wait_display_name(&state, &id, "sleep").await;

        // ...and the name falls back to the shell once it exits.
        wait_display_name(&state, &id, "bash").await;

        state.sessions.kill(&id).ok();
    }

    #[tokio::test]
    async fn shell_display_name_uses_workspace_relative_cwd() {
        let state = test_state();
        let root = test_dir("naming-cd");
        std::fs::create_dir(root.join("crates")).unwrap();
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let workspace_id = ws["id"].as_str().unwrap().to_string();
        let root = std::fs::canonicalize(&root).unwrap();
        let id = inject_shell(&state, &root, &workspace_id);

        wait_display_name(&state, &id, "bash").await;

        // cd into a subdirectory: the idle shell is named by where it sits,
        // relative to the workspace root.
        let att = state.sessions.attach(&id).expect("attach");
        att.input
            .send(bytes::Bytes::from("cd crates\n"))
            .await
            .expect("send input");
        wait_display_name(&state, &id, "crates").await;

        // cd back to the root: the shell name again.
        att.input
            .send(bytes::Bytes::from("cd ..\n"))
            .await
            .expect("send input");
        wait_display_name(&state, &id, "bash").await;

        state.sessions.kill(&id).ok();
    }

    /// End-to-end shell integration: a real bash spawned the way
    /// create_session spawns it (integration injected, hermetic HOME) must
    /// reach phase `ready` and populate the command journal with command
    /// text, output, and exit codes.
    #[tokio::test]
    async fn integrated_shell_populates_command_journal() {
        use chimaera_pty::ShellPhase;

        let state = test_state();
        let base = test_dir("shellint-base");
        let home = test_dir("shellint-home");
        let launch =
            chimaera_core::shellint::shell_launch_for("/bin/bash", &base).expect("launch");
        let mut env = launch.env;
        env.push(("HOME".to_string(), home.to_string_lossy().into_owned()));
        let info = state
            .sessions
            .spawn(chimaera_pty::SpawnOpts {
                cwd: test_dir("shellint-cwd"),
                name: None,
                cols: 80,
                rows: 24,
                command: Some(launch.argv),
                id: None,
                env,
            })
            .expect("spawn integrated bash");
        let marks = state.sessions.marks(&info.id).expect("marks");

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        while marks.phase() != ShellPhase::Ready {
            assert!(
                tokio::time::Instant::now() < deadline,
                "integrated shell never reached ready (phase {:?})",
                marks.phase()
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let att = state.sessions.attach(&info.id).expect("attach");
        att.input
            .send(bytes::Bytes::from("echo integration-works\n"))
            .await
            .expect("send command");

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        let entry = loop {
            let done = marks
                .journal(10)
                .into_iter()
                .find(|e| !e.running && e.command.as_deref() == Some("echo integration-works"));
            if let Some(entry) = done {
                break entry;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "journal never recorded the command; journal: {:?}",
                marks.journal(10)
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        assert_eq!(entry.exit_code, Some(0), "{entry:?}");
        assert!(entry.output.contains("integration-works"), "{entry:?}");
        assert_eq!(entry.source, chimaera_pty::CommandSource::User);

        state.sessions.kill(&info.id).ok();
    }

    /// Spawn an integrated bash with a hermetic HOME and wait for `ready`.
    async fn spawn_integrated_bash(state: &Arc<AppState>, label: &str) -> String {
        let base = test_dir(&format!("{label}-base"));
        let home = test_dir(&format!("{label}-home"));
        let launch =
            chimaera_core::shellint::shell_launch_for("/bin/bash", &base).expect("launch");
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

    /// Exec into a shell with NO integration: the engine must fall back to
    /// sentinel mode and still deliver output + exit code through the
    /// printf-emitted marks.
    #[tokio::test]
    async fn exec_sentinel_round_trip_on_plain_shell() {
        let state = test_state();
        let info = state
            .sessions
            .spawn(chimaera_pty::SpawnOpts {
                cwd: test_dir("exec-sentinel"),
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
            })
            .expect("spawn plain bash");
        let id = info.id;

        let (status, out) = request(
            &state,
            Method::POST,
            &format!("/api/v1/sessions/{id}/exec"),
            Some(serde_json::json!({"command": "echo sentinel-ran && false"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["mode"], "sentinel", "{out}");
        assert_eq!(out["timed_out"], false, "{out}");
        assert_eq!(out["record"]["exit_code"], 1, "{out}");
        assert_eq!(out["record"]["source"], "agent", "{out}");
        assert!(
            out["record"]["output"]
                .as_str()
                .unwrap()
                .contains("sentinel-ran"),
            "{out}"
        );

        state.sessions.kill(&id).ok();
    }

    /// The author decision in action: an exec against a busy integrated
    /// shell QUEUES until the prompt returns, then runs in integrated mode.
    #[tokio::test]
    async fn exec_queues_behind_running_command() {
        let state = test_state();
        let id = spawn_integrated_bash(&state, "exec-queue").await;

        let att = state.sessions.attach(&id).expect("attach");
        att.input
            .send(bytes::Bytes::from("sleep 2\n"))
            .await
            .expect("start user command");
        // Give the sleep a moment to actually start (phase -> running).
        let marks = state.sessions.marks(&id).unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while marks.phase() != chimaera_pty::ShellPhase::Running {
            assert!(tokio::time::Instant::now() < deadline, "sleep never started");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let (status, out) = request(
            &state,
            Method::POST,
            &format!("/api/v1/sessions/{id}/exec"),
            Some(serde_json::json!({
                "command": "echo queued-ran",
                "queue_timeout_ms": 15000,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["mode"], "integrated", "{out}");
        assert!(
            out["record"]["output"].as_str().unwrap().contains("queued-ran"),
            "{out}"
        );
        // It genuinely waited for the sleep instead of typing over it.
        assert!(
            out["waited_ms"].as_u64().unwrap() >= 1000,
            "expected a queue wait, got {out}"
        );

        state.sessions.kill(&id).ok();
    }

    /// With a short queue timeout and no remote-forwarding foreground, a
    /// busy shell is a 409 — never typed into.
    #[tokio::test]
    async fn exec_busy_is_409_without_sentinel_permission() {
        let state = test_state();
        let id = spawn_integrated_bash(&state, "exec-busy").await;

        let att = state.sessions.attach(&id).expect("attach");
        att.input
            .send(bytes::Bytes::from("sleep 5\n"))
            .await
            .expect("start user command");
        let marks = state.sessions.marks(&id).unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while marks.phase() != chimaera_pty::ShellPhase::Running {
            assert!(tokio::time::Instant::now() < deadline, "sleep never started");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let (status, out) = request(
            &state,
            Method::POST,
            &format!("/api/v1/sessions/{id}/exec"),
            Some(serde_json::json!({
                "command": "echo should-not-run",
                "queue_timeout_ms": 300,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{out}");

        state.sessions.kill(&id).ok();
    }

    #[tokio::test]
    async fn exec_into_agent_session_is_409_and_journal_endpoint_reads() {
        let state = test_state();
        let agent_id = inject_agent(&state, "k");
        let (status, out) = request(
            &state,
            Method::POST,
            &format!("/api/v1/sessions/{agent_id}/exec"),
            Some(serde_json::json!({"command": "echo nope"})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{out}");

        // Journal endpoint: exec into a shell, then read it back over HTTP.
        let id = spawn_integrated_bash(&state, "journal-ep").await;
        let (status, out) = request(
            &state,
            Method::POST,
            &format!("/api/v1/sessions/{id}/exec"),
            Some(serde_json::json!({"command": "echo journaled"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");

        let (status, journal) = request(
            &state,
            Method::GET,
            &format!("/api/v1/sessions/{id}/journal?limit=5"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{journal}");
        assert_eq!(journal["phase"], "ready", "{journal}");
        let entries = journal["entries"].as_array().unwrap();
        let entry = entries
            .iter()
            .find(|e| e["command"] == "echo journaled")
            .expect("journaled entry");
        assert_eq!(entry["source"], "agent", "{journal}");
        assert_eq!(entry["exit_code"], 0, "{journal}");

        // Unknown session is a 404.
        let (status, _) = request(
            &state,
            Method::GET,
            "/api/v1/sessions/s-00000000/journal",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        state.sessions.kill(&agent_id).ok();
        state.sessions.kill(&id).ok();
    }

    #[tokio::test]
    async fn links_lifecycle_validation_and_move() {
        let state = test_state();
        let agent_a = inject_agent(&state, "ka");
        let agent_b = inject_agent(&state, "kb");
        let shell = {
            let info = state
                .sessions
                .spawn(chimaera_pty::SpawnOpts {
                    cwd: test_dir("links-shell"),
                    name: None,
                    cols: 80,
                    rows: 24,
                    command: None,
                    id: None,
                    env: Vec::new(),
                })
                .expect("spawn shell");
            info.id
        };

        // Link shell -> agent A.
        let (status, out) = request(
            &state,
            Method::PUT,
            "/api/v1/links",
            Some(serde_json::json!({"terminal_id": shell, "agent_id": agent_a})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["moved_from"], serde_json::Value::Null);

        let (_, list) = request(&state, Method::GET, "/api/v1/links", None).await;
        assert_eq!(
            list,
            serde_json::json!([{"terminal_id": shell, "agent_id": agent_a}])
        );

        // Re-linking to agent B MOVES the leash (one agent per terminal).
        let (status, out) = request(
            &state,
            Method::PUT,
            "/api/v1/links",
            Some(serde_json::json!({"terminal_id": shell, "agent_id": agent_b})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["moved_from"], agent_a, "{out}");
        assert_eq!(links::terminals_of(&state, &agent_b), vec![shell.clone()]);
        assert!(links::terminals_of(&state, &agent_a).is_empty());

        // A shell can't play agent; an agent can't play terminal.
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/links",
            Some(serde_json::json!({"terminal_id": shell, "agent_id": shell})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/links",
            Some(serde_json::json!({"terminal_id": agent_a, "agent_id": agent_b})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Unknown sessions are 404s.
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/links",
            Some(serde_json::json!({"terminal_id": "s-00000000", "agent_id": agent_a})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Unlink is idempotent.
        for _ in 0..2 {
            let (status, _) = request(
                &state,
                Method::DELETE,
                &format!("/api/v1/links/{shell}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT);
        }
        let (_, list) = request(&state, Method::GET, "/api/v1/links", None).await;
        assert_eq!(list, serde_json::json!([]));

        // A link dies with its terminal session (pruned on read).
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/links",
            Some(serde_json::json!({"terminal_id": shell, "agent_id": agent_a})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        state.sessions.kill(&shell).ok();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let (_, list) = request(&state, Method::GET, "/api/v1/links", None).await;
            if list == serde_json::json!([]) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "link survived its dead terminal: {list}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        state.sessions.kill(&agent_a).ok();
        state.sessions.kill(&agent_b).ok();
    }

    /// POST one JSON-RPC message to an agent's MCP endpoint.
    async fn mcp_post(
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
    async fn mcp_tool_call(
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
        let text = result["content"][0]["text"].as_str().unwrap_or("").to_string();
        (is_error, text)
    }

    #[tokio::test]
    async fn mcp_handshake_auth_and_tool_listing() {
        let state = test_state();
        let id = inject_agent(&state, "mk");

        // Wrong key is a 403; unknown agent a 404.
        let init = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"},
        });
        let (status, _) = mcp_post(&state, &id, "wrong", init.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = mcp_post(&state, "s-00000000", "mk", init.clone()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Initialize echoes the protocol version and carries instructions.
        let (status, out) = mcp_post(&state, &id, "mk", init).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(out["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(out["result"]["serverInfo"]["name"], "chimaera");
        assert!(out["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("@term:"));

        // Notifications (no id) are 202-acknowledged.
        let (status, _) = mcp_post(
            &state,
            &id,
            "mk",
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);

        // tools/list names the three linked-terminal tools.
        let (status, out) = mcp_post(
            &state,
            &id,
            "mk",
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let names: Vec<&str> = out["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["list_terminals", "run_in_terminal", "read_terminal"]
        );

        state.sessions.kill(&id).ok();
    }

    /// The full agent-side story over MCP: unlinked -> helpful error;
    /// linked -> list, exec (by display name), and journal read all work
    /// and stay scoped.
    #[tokio::test]
    async fn mcp_tools_scoped_to_links_and_exec_round_trip() {
        let state = test_state();
        let agent = inject_agent(&state, "mk");
        let shell = spawn_integrated_bash(&state, "mcp-shell").await;

        // Unlinked: every tool refuses with linking guidance.
        let (is_error, text) = mcp_tool_call(
            &state,
            &agent,
            "mk",
            "run_in_terminal",
            serde_json::json!({"terminal": shell, "command": "echo hi"}),
        )
        .await;
        assert!(is_error, "{text}");
        assert!(text.contains("no terminals are linked"), "{text}");

        // Link, then exec by session id.
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/links",
            Some(serde_json::json!({"terminal_id": shell, "agent_id": agent})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (is_error, text) = mcp_tool_call(
            &state,
            &agent,
            "mk",
            "run_in_terminal",
            serde_json::json!({"terminal": shell, "command": "echo mcp-ran && false"}),
        )
        .await;
        assert!(!is_error, "{text}");
        assert!(text.starts_with("exit 1"), "{text}");
        assert!(text.contains("integrated mode"), "{text}");
        assert!(text.contains("mcp-ran"), "{text}");

        // list_terminals shows the linked shell with its last command.
        let (is_error, text) =
            mcp_tool_call(&state, &agent, "mk", "list_terminals", serde_json::json!({})).await;
        assert!(!is_error, "{text}");
        assert!(text.contains(&shell), "{text}");
        assert!(text.contains("echo mcp-ran && false"), "{text}");

        // read_terminal returns the journal with agent attribution upstream.
        let (is_error, text) = mcp_tool_call(
            &state,
            &agent,
            "mk",
            "read_terminal",
            serde_json::json!({"terminal": shell, "commands": 3}),
        )
        .await;
        assert!(!is_error, "{text}");
        assert!(text.contains("phase: ready"), "{text}");
        assert!(text.contains("echo mcp-ran && false"), "{text}");
        assert!(text.contains("exit 1"), "{text}");

        // Screen mode reads the visible grid.
        let (is_error, text) = mcp_tool_call(
            &state,
            &agent,
            "mk",
            "read_terminal",
            serde_json::json!({"terminal": shell, "screen": true}),
        )
        .await;
        assert!(!is_error, "{text}");
        assert!(text.contains("mcp-ran"), "{text}");

        // A second, unlinked shell stays out of reach — scope is the links.
        let other = spawn_integrated_bash(&state, "mcp-other").await;
        let (is_error, text) = mcp_tool_call(
            &state,
            &agent,
            "mk",
            "run_in_terminal",
            serde_json::json!({"terminal": other, "command": "echo nope"}),
        )
        .await;
        assert!(is_error, "{text}");

        state.sessions.kill(&agent).ok();
        state.sessions.kill(&shell).ok();
        state.sessions.kill(&other).ok();
    }

    /// `@term:` mentions in a user prompt auto-link (mention = consent) and
    /// the hook response tells the agent via additionalContext.
    #[tokio::test]
    async fn user_prompt_mention_autolinks_terminal() {
        let state = test_state();
        let agent = inject_agent(&state, "mk");
        let root = test_dir("mention-root");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let root = std::fs::canonicalize(&root).unwrap();
        let shell = inject_shell(&state, &root, ws["id"].as_str().unwrap());
        wait_display_name(&state, &shell, "bash").await;

        // Mention by display name links it and reports back as context.
        let (status, out) = request(
            &state,
            Method::POST,
            &format!("/api/v1/agent-events/{agent}?key=mk"),
            Some(serde_json::json!({
                "hook_event_name": "UserPromptSubmit",
                "prompt": "run squeue in @term:bash please",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or("");
        assert!(context.contains("Linked terminal 'bash'"), "{out}");
        let (_, list) = request(&state, Method::GET, "/api/v1/links", None).await;
        assert_eq!(
            list,
            serde_json::json!([{"terminal_id": shell, "agent_id": agent}])
        );

        // A repeated mention of an already-linked terminal stays silent.
        let (_, out) = request(
            &state,
            Method::POST,
            &format!("/api/v1/agent-events/{agent}?key=mk"),
            Some(serde_json::json!({
                "hook_event_name": "UserPromptSubmit",
                "prompt": "again in @term:bash",
            })),
        )
        .await;
        assert_eq!(out["hookSpecificOutput"], serde_json::Value::Null, "{out}");

        // Unknown mentions surface as context too (the agent should know).
        let (_, out) = request(
            &state,
            Method::POST,
            &format!("/api/v1/agent-events/{agent}?key=mk"),
            Some(serde_json::json!({
                "hook_event_name": "UserPromptSubmit",
                "prompt": "and @term:doesnotexist",
            })),
        )
        .await;
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or("");
        assert!(context.contains("no terminal 'doesnotexist'"), "{out}");

        state.sessions.kill(&agent).ok();
        state.sessions.kill(&shell).ok();
    }

    #[tokio::test]
    async fn agent_first_prompt_is_provisional_display_name() {
        let state = test_state();
        let id = inject_agent(&state, "k");

        // No hook data yet: the generic agent name.
        assert_eq!(session_entry(&state, &id).await["display_name"], "claude");

        // The first UserPromptSubmit becomes the provisional title,
        // truncated near 60 chars at a word boundary.
        let prompt = "please refactor the entire qc pipeline so the reports land in \
                      results/qc and nothing downstream breaks";
        let status = post_hook(
            &state,
            &id,
            "k",
            serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": prompt}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let entry = session_entry(&state, &id).await;
        let display = entry["display_name"].as_str().unwrap();
        assert!(
            display.starts_with("please refactor the entire qc pipeline"),
            "{display}"
        );
        assert!(display.ends_with('…'), "{display}");
        assert!(display.chars().count() <= 61, "{display}");
        assert_eq!(entry["agent_state"], "running");

        // A later prompt does not displace the first.
        let status = post_hook(
            &state,
            &id,
            "k",
            serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": "and again"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            session_entry(&state, &id).await["display_name"],
            display,
            "first prompt must stay the provisional title"
        );

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
        for uri in [
            "/api/v1/fs/home",
            "/api/v1/fs/dirs?path=/",
            "/api/v1/fs/list?path=/",
            "/api/v1/fs/file?path=/etc/hosts",
            "/api/v1/fs/markdown?path=/x.md",
            "/api/v1/fs/table?path=/x.csv",
            "/api/v1/fs/quickopen?workspace_id=w-x&q=main",
        ] {
            let res = app(test_state())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
        // The ticket mint is a POST and equally protected.
        let res = app(test_state())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/fs/ticket")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"path":"/etc/hosts"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // So is the file write.
        let res = app(test_state())
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/v1/fs/file?path=/tmp/x.txt")
                    .body(Body::from("data"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    /// Like `request`, but returns the raw response: status, headers, bytes.
    /// `token: None` sends no Authorization header (for /raw).
    async fn request_bytes(
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

    fn header_str<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> &'a str {
        headers
            .get(name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .unwrap()
    }

    #[tokio::test]
    async fn fs_list_dirs_first_sorted_with_metadata() {
        let state = test_state();
        let root = test_dir("fs-full-list");
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::create_dir(root.join("Docs")).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join("README.md"), "hello").unwrap();
        std::fs::write(root.join("app.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join(".env"), "SECRET=1").unwrap();
        std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).unwrap();

        let canonical = std::fs::canonicalize(&root).unwrap();
        let uri = format!("/api/v1/fs/list?path={}", root.to_string_lossy());
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["path"].as_str().unwrap(), canonical.to_str().unwrap());
        assert_eq!(
            json["parent"].as_str().unwrap(),
            canonical.parent().unwrap().to_str().unwrap()
        );

        // Dirs first (case-insensitive), then files; dot entries and broken
        // symlinks excluded.
        let entries = json["entries"].as_array().unwrap();
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Docs", "src", "app.rs", "README.md"]);
        assert_eq!(entries[0]["kind"], "dir");
        assert_eq!(entries[1]["kind"], "dir");
        assert_eq!(entries[2]["kind"], "file");
        assert_eq!(entries[3]["kind"], "file");
        assert_eq!(entries[3]["size"], 5); // "hello"
        assert!(entries[3]["mtime"].as_u64().unwrap() > 0);
        assert_eq!(
            entries[2]["path"].as_str().unwrap(),
            canonical.join("app.rs").to_str().unwrap()
        );

        // hidden=true adds the dot entries in their sorted spots.
        let uri = format!(
            "/api/v1/fs/list?path={}&hidden=true",
            root.to_string_lossy()
        );
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        let names: Vec<&str> = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [".git", "Docs", "src", ".env", "app.rs", "README.md"]
        );
    }

    #[tokio::test]
    async fn fs_list_rejects_files_and_missing_paths() {
        let state = test_state();
        let root = test_dir("fs-list-bad");
        let file = root.join("plain.txt");
        std::fs::write(&file, "x").unwrap();

        for path in [
            file.to_string_lossy().into_owned(),
            "/definitely/not/a/dir".into(),
        ] {
            let uri = format!("/api/v1/fs/list?path={path}");
            let (status, err) = request(&state, Method::GET, &uri, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert!(err["error"].is_string());
        }
    }

    #[tokio::test]
    async fn fs_file_serves_slices_with_size_headers() {
        let state = test_state();
        let root = test_dir("fs-file");
        let path = root.join("notes.txt");
        std::fs::write(&path, "0123456789").unwrap();
        let path = path.to_string_lossy();

        // Whole file by default.
        let uri = format!("/api/v1/fs/file?path={path}");
        let (status, headers, body) =
            request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"0123456789");
        assert!(header_str(&headers, "content-type").starts_with("text/plain"));
        assert_eq!(header_str(&headers, "x-file-size"), "10");
        assert_eq!(header_str(&headers, "x-truncated"), "false");

        // A middle slice reports truncation.
        let uri = format!("/api/v1/fs/file?path={path}&offset=3&limit=4");
        let (status, headers, body) =
            request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"3456");
        assert_eq!(header_str(&headers, "x-file-size"), "10");
        assert_eq!(header_str(&headers, "x-truncated"), "true");

        // A slice ending exactly at EOF is not truncated.
        let uri = format!("/api/v1/fs/file?path={path}&offset=6&limit=4");
        let (status, headers, body) =
            request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"6789");
        assert_eq!(header_str(&headers, "x-truncated"), "false");

        // An offset past EOF yields an empty, non-truncated body.
        let uri = format!("/api/v1/fs/file?path={path}&offset=100");
        let (status, headers, body) =
            request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty());
        assert_eq!(header_str(&headers, "x-truncated"), "false");
    }

    #[tokio::test]
    async fn fs_file_limit_is_capped_at_2mb() {
        let state = test_state();
        let root = test_dir("fs-file-cap");
        let path = root.join("big.bin");
        std::fs::write(&path, vec![0x42u8; 3 * 1024 * 1024]).unwrap();

        let uri = format!(
            "/api/v1/fs/file?path={}&limit=99999999",
            path.to_string_lossy()
        );
        let (status, headers, body) =
            request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.len(), 2 * 1024 * 1024);
        assert_eq!(
            header_str(&headers, "x-file-size"),
            (3 * 1024 * 1024).to_string()
        );
        assert_eq!(header_str(&headers, "x-truncated"), "true");
    }

    #[tokio::test]
    async fn fs_file_rejects_dirs_and_missing_paths() {
        let state = test_state();
        let root = test_dir("fs-file-bad");

        for path in [
            root.to_string_lossy().into_owned(),
            "/no/such/file.txt".into(),
        ] {
            let uri = format!("/api/v1/fs/file?path={path}");
            let (status, err) = request(&state, Method::GET, &uri, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert!(err["error"].is_string());
        }
    }

    #[tokio::test]
    async fn fs_markdown_renders_gfm_and_sanitizes() {
        let state = test_state();
        let root = test_dir("fs-md");
        let path = root.join("doc.md");
        std::fs::write(
            &path,
            concat!(
                "# Title\n\n",
                "~~old~~ new, see https://example.com\n\n",
                "| a | b |\n|---|---|\n| 1 | 2 |\n\n",
                "<script>alert('xss')</script>\n\n",
                "<img src=\"x.png\" onerror=\"alert('xss')\">\n",
            ),
        )
        .unwrap();

        let uri = format!("/api/v1/fs/markdown?path={}", path.to_string_lossy());
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        let html = json["html"].as_str().unwrap();

        // GFM features render.
        assert!(html.contains("<h1>Title</h1>"), "no heading in {html}");
        assert!(
            html.contains("<del>old</del>"),
            "no strikethrough in {html}"
        );
        assert!(html.contains("<table>"), "no table in {html}");
        assert!(
            html.contains("<a href=\"https://example.com\""),
            "no autolink in {html}"
        );
        // Sanitization strips script tags and event handlers but keeps the img.
        assert!(!html.contains("<script"), "script survived in {html}");
        assert!(!html.contains("onerror"), "onerror survived in {html}");
        assert!(!html.contains("alert("), "alert survived in {html}");
        assert!(
            html.contains("<img src=\"x.png\""),
            "img stripped in {html}"
        );
    }

    #[tokio::test]
    async fn fs_markdown_rejects_oversize_dirs_and_missing() {
        let state = test_state();
        let root = test_dir("fs-md-bad");

        // One byte over the 4MB limit is a 400.
        let big = root.join("big.md");
        std::fs::write(&big, "a".repeat(4 * 1024 * 1024 + 1)).unwrap();
        let uri = format!("/api/v1/fs/markdown?path={}", big.to_string_lossy());
        let (status, err) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("too large"));

        for path in [
            root.to_string_lossy().into_owned(),
            "/no/such/doc.md".into(),
        ] {
            let uri = format!("/api/v1/fs/markdown?path={path}");
            let (status, err) = request(&state, Method::GET, &uri, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert!(err["error"].is_string());
        }
    }

    #[tokio::test]
    async fn fs_table_pages_csv_with_header() {
        let state = test_state();
        let root = test_dir("fs-table");
        let path = root.join("data.csv");
        let mut csv = String::from("name,value,note\n");
        for i in 0..8 {
            csv.push_str(&format!("row{i},{i},\"has, comma\"\n"));
        }
        std::fs::write(&path, csv).unwrap();
        let path = path.to_string_lossy();

        // Defaults: all 8 rows fit in one 200-row page.
        let uri = format!("/api/v1/fs/table?path={path}");
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json["columns"],
            serde_json::json!(["name", "value", "note"])
        );
        assert_eq!(json["rows"].as_array().unwrap().len(), 8);
        assert_eq!(
            json["rows"][0],
            serde_json::json!(["row0", "0", "has, comma"])
        );
        assert_eq!(json["offset"], 0);
        assert_eq!(json["truncated"], false);

        // A limited page is truncated.
        let uri = format!("/api/v1/fs/table?path={path}&limit_rows=3");
        let (_, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(json["rows"].as_array().unwrap().len(), 3);
        assert_eq!(json["rows"][2][0], "row2");
        assert_eq!(json["truncated"], true);

        // The final short page is not.
        let uri = format!("/api/v1/fs/table?path={path}&offset_rows=6&limit_rows=3");
        let (_, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(json["rows"].as_array().unwrap().len(), 2);
        assert_eq!(json["rows"][0][0], "row6");
        assert_eq!(json["offset"], 6);
        assert_eq!(json["truncated"], false);
    }

    #[tokio::test]
    async fn fs_table_sniffs_delimiters() {
        let state = test_state();
        let root = test_dir("fs-table-sniff");

        // .tsv extension forces tabs.
        let tsv = root.join("data.tsv");
        std::fs::write(&tsv, "a\tb\n1\t2\n").unwrap();
        let uri = format!("/api/v1/fs/table?path={}", tsv.to_string_lossy());
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["columns"], serde_json::json!(["a", "b"]));
        assert_eq!(json["rows"][0], serde_json::json!(["1", "2"]));

        // Unknown extension: a tab in the first line wins over commas.
        let weird = root.join("export.data");
        std::fs::write(&weird, "x\ty\n3\t4\n").unwrap();
        let uri = format!("/api/v1/fs/table?path={}", weird.to_string_lossy());
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["columns"], serde_json::json!(["x", "y"]));

        // Explicit delim=tab overrides a .csv extension.
        let mixed = root.join("tabs.csv");
        std::fs::write(&mixed, "p\tq\n5\t6\n").unwrap();
        let uri = format!(
            "/api/v1/fs/table?path={}&delim=tab",
            mixed.to_string_lossy()
        );
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["columns"], serde_json::json!(["p", "q"]));

        // An unsupported delim value is a 400.
        let uri = format!("/api/v1/fs/table?path={}&delim=pipe", tsv.to_string_lossy());
        let (status, err) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("delimiter"));
    }

    #[tokio::test]
    async fn fs_table_caps_rows_and_rejects_corrupt_gz_dirs_missing() {
        let state = test_state();
        let root = test_dir("fs-table-bad");

        // limit_rows above the 1000 cap clamps to 1000.
        let big = root.join("big.csv");
        let mut csv = String::from("n\n");
        for i in 0..1200 {
            csv.push_str(&format!("{i}\n"));
        }
        std::fs::write(&big, csv).unwrap();
        let uri = format!(
            "/api/v1/fs/table?path={}&limit_rows=1200",
            big.to_string_lossy()
        );
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["rows"].as_array().unwrap().len(), 1000);
        assert_eq!(json["truncated"], true);

        // A .gz that is not actually gzip is a clean 400, not a hang or 500.
        let gz = root.join("data.csv.gz");
        std::fs::write(&gz, b"totally not gzip bytes").unwrap();
        let uri = format!("/api/v1/fs/table?path={}", gz.to_string_lossy());
        let (status, err) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].is_string());

        for path in [
            root.to_string_lossy().into_owned(),
            "/no/such/data.csv".into(),
        ] {
            let uri = format!("/api/v1/fs/table?path={path}");
            let (status, err) = request(&state, Method::GET, &uri, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert!(err["error"].is_string());
        }
    }

    /// Gzip `content`, optionally recording `fname` as the member's FNAME.
    fn gzip_bytes(content: &[u8], fname: Option<&str>) -> Vec<u8> {
        use std::io::Write;
        let mut builder = flate2::GzBuilder::new();
        if let Some(name) = fname {
            builder = builder.filename(name);
        }
        let mut encoder = builder.write(Vec::new(), flate2::Compression::default());
        encoder.write_all(content).unwrap();
        encoder.finish().unwrap()
    }

    #[tokio::test]
    async fn fs_table_pages_tsv_gz_including_multimember() {
        let state = test_state();
        let root = test_dir("fs-table-gz");

        // Single member: pages exactly like the plain-file test.
        let mut tsv = String::from("name\tvalue\n");
        for i in 0..8 {
            tsv.push_str(&format!("row{i}\t{i}\n"));
        }
        let single = root.join("data.tsv.gz");
        std::fs::write(&single, gzip_bytes(tsv.as_bytes(), None)).unwrap();
        let path = single.to_string_lossy();

        let uri = format!("/api/v1/fs/table?path={path}");
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["columns"], serde_json::json!(["name", "value"]));
        assert_eq!(json["rows"].as_array().unwrap().len(), 8);
        assert_eq!(json["rows"][0], serde_json::json!(["row0", "0"]));
        assert_eq!(json["truncated"], false);

        let uri = format!("/api/v1/fs/table?path={path}&limit_rows=3");
        let (_, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(json["rows"].as_array().unwrap().len(), 3);
        assert_eq!(json["truncated"], true);

        let uri = format!("/api/v1/fs/table?path={path}&offset_rows=6&limit_rows=3");
        let (_, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(json["rows"].as_array().unwrap().len(), 2);
        assert_eq!(json["rows"][0][0], "row6");
        assert_eq!(json["offset"], 6);
        assert_eq!(json["truncated"], false);

        // Multi-member (bgzip-style concatenated gzip streams), with the
        // member boundary cutting a row in half: the decode is seamless.
        let mut multi = gzip_bytes(b"a\tb\nrow0\t0\nro", None);
        multi.extend(gzip_bytes(b"w1\t1\nrow2\t2\n", None));
        let multi_path = root.join("multi.tsv.gz");
        std::fs::write(&multi_path, multi).unwrap();
        let uri = format!("/api/v1/fs/table?path={}", multi_path.to_string_lossy());
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["columns"], serde_json::json!(["a", "b"]));
        assert_eq!(
            json["rows"],
            serde_json::json!([["row0", "0"], ["row1", "1"], ["row2", "2"]])
        );

        // .bgz reads the same as .gz.
        let bgz = root.join("data.tsv.bgz");
        std::fs::write(&bgz, gzip_bytes(b"x\ty\n1\t2\n", None)).unwrap();
        let uri = format!("/api/v1/fs/table?path={}", bgz.to_string_lossy());
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["columns"], serde_json::json!(["x", "y"]));
    }

    #[tokio::test]
    async fn fs_table_gz_sniffs_inner_name() {
        let state = test_state();
        let root = test_dir("fs-table-gz-sniff");

        // Outer name says nothing ("blob.gz"), but the member FNAME says
        // .csv — comma wins even though the first line contains a tab
        // (content-sniffing alone would have picked tab).
        let blob = root.join("blob.gz");
        std::fs::write(&blob, gzip_bytes(b"a,b\tc\n1,2\t3\n", Some("data.csv"))).unwrap();
        let uri = format!("/api/v1/fs/table?path={}", blob.to_string_lossy());
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["columns"], serde_json::json!(["a", "b\tc"]));

        // No FNAME, no inner extension: the first decoded line is sniffed.
        let mystery = root.join("mystery.gz");
        std::fs::write(&mystery, gzip_bytes(b"x\ty\n3\t4\n", None)).unwrap();
        let uri = format!("/api/v1/fs/table?path={}", mystery.to_string_lossy());
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["columns"], serde_json::json!(["x", "y"]));
    }

    #[tokio::test]
    async fn fs_file_gz_serves_decompressed_slices() {
        let state = test_state();
        let root = test_dir("fs-file-gz");
        let path = root.join("notes.txt.gz");
        std::fs::write(&path, gzip_bytes(b"abcdefghij", None)).unwrap();
        let path = path.to_string_lossy();

        // Whole file: decompressed bytes, inner-name content type, exact size.
        let uri = format!("/api/v1/fs/file?path={path}");
        let (status, headers, body) =
            request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"abcdefghij");
        assert!(header_str(&headers, "content-type").starts_with("text/plain"));
        assert_eq!(header_str(&headers, "x-truncated"), "false");
        assert_eq!(header_str(&headers, "x-file-size"), "10");
        assert!(header_str(&headers, "x-mtime").parse::<u128>().unwrap() > 0);

        // A head slice: truncated, and the total size is honestly unknown.
        let uri = format!("/api/v1/fs/file?path={path}&limit=4");
        let (status, headers, body) =
            request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"abcd");
        assert_eq!(header_str(&headers, "x-truncated"), "true");
        assert!(headers.get("x-file-size").is_none());

        // Offsets address decompressed bytes (sequential skip).
        let uri = format!("/api/v1/fs/file?path={path}&offset=4&limit=4");
        let (_, headers, body) = request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        assert_eq!(&body[..], b"efgh");
        assert_eq!(header_str(&headers, "x-truncated"), "true");

        // A slice ending exactly at EOF is not truncated, and knows the size.
        let uri = format!("/api/v1/fs/file?path={path}&offset=6&limit=4");
        let (_, headers, body) = request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        assert_eq!(&body[..], b"ghij");
        assert_eq!(header_str(&headers, "x-truncated"), "false");
        assert_eq!(header_str(&headers, "x-file-size"), "10");

        // An offset past decompressed EOF: empty, non-truncated.
        let uri = format!("/api/v1/fs/file?path={path}&offset=100");
        let (_, headers, body) = request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        assert!(body.is_empty());
        assert_eq!(header_str(&headers, "x-truncated"), "false");
        assert_eq!(header_str(&headers, "x-file-size"), "10");

        // Multi-member decodes seamlessly here too.
        let multi_path = root.join("hello.txt.gz");
        let mut multi = gzip_bytes(b"hello ", None);
        multi.extend(gzip_bytes(b"world", None));
        std::fs::write(&multi_path, multi).unwrap();
        let uri = format!("/api/v1/fs/file?path={}", multi_path.to_string_lossy());
        let (status, _, body) = request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"hello world");
    }

    /// PUT raw bytes with the bearer token; returns status, headers, body.
    async fn put_raw(
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

    #[tokio::test]
    async fn fs_put_file_round_trip_atomic_with_mtime_chain() {
        let state = test_state();
        let root = test_dir("fs-put");
        let path = root.join("notes.txt");
        let uri = |extra: &str| format!("/api/v1/fs/file?path={}{extra}", path.to_string_lossy());

        // Create (parent exists, file does not): 204 + the new mtime token.
        let (status, headers, body) = put_raw(&state, &uri(""), b"hello v1".to_vec()).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(body.is_empty());
        let mtime1 = header_str(&headers, "x-mtime").to_string();
        assert!(mtime1.parse::<u128>().unwrap() > 0);

        // GET reports the same token, so the editor can start a save chain.
        let (status, headers, body) =
            request_bytes(&state, Method::GET, &uri(""), Some("test-token")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"hello v1");
        assert_eq!(header_str(&headers, "x-mtime"), mtime1);

        // Save with a matching expect_mtime: accepted, token advances.
        let (status, headers, _) = put_raw(
            &state,
            &uri(&format!("&expect_mtime={mtime1}")),
            b"hello v2".to_vec(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let mtime2 = header_str(&headers, "x-mtime").to_string();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello v2");

        // Chained save against the returned token still works.
        let (status, _, _) = put_raw(
            &state,
            &uri(&format!("&expect_mtime={mtime2}")),
            b"hello v3".to_vec(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello v3");

        // Atomicity hygiene: no tmp siblings survive the writes.
        let names: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["notes.txt"], "leftover files: {names:?}");
    }

    #[tokio::test]
    async fn fs_put_file_conflict_is_409_and_leaves_disk_untouched() {
        let state = test_state();
        let root = test_dir("fs-put-conflict");
        let path = root.join("doc.md");
        std::fs::write(&path, "original").unwrap();

        let uri = format!("/api/v1/fs/file?path={}", path.to_string_lossy());
        let (_, headers, _) = request_bytes(&state, Method::GET, &uri, Some("test-token")).await;
        let stale = header_str(&headers, "x-mtime").to_string();

        // Another writer touches the file (mtime moves past the token).
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        std::fs::write(&path, "external edit").unwrap();

        let (status, _, body) = put_raw(
            &state,
            &format!("{uri}&expect_mtime={stale}"),
            b"my edit".to_vec(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(err, serde_json::json!({"error": "file changed on disk"}));
        // The refused write changed nothing on disk.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external edit");

        // A file deleted since the editor loaded it is a conflict too.
        let gone = root.join("gone.txt");
        let (status, _, _) = put_raw(
            &state,
            &format!(
                "/api/v1/fs/file?path={}&expect_mtime=12345",
                gone.to_string_lossy()
            ),
            b"x".to_vec(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(!gone.exists());

        // Without expect_mtime the check is skipped (explicit overwrite).
        let (status, _, _) = put_raw(&state, &uri, b"forced".to_vec()).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "forced");
    }

    #[tokio::test]
    async fn fs_put_file_rejects_dirs_and_missing_parents() {
        let state = test_state();
        let root = test_dir("fs-put-bad");

        // Writing over a directory is refused.
        let uri = format!("/api/v1/fs/file?path={}", root.to_string_lossy());
        let (status, _, body) = put_raw(&state, &uri, b"x".to_vec()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(err["error"].as_str().unwrap().contains("directory"));

        // Creating a file whose parent directory does not exist is refused
        // (no implicit mkdir -p).
        let orphan = root.join("no/such/dir/file.txt");
        let uri = format!("/api/v1/fs/file?path={}", orphan.to_string_lossy());
        let (status, _, _) = put_raw(&state, &uri, b"x".to_vec()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!orphan.exists());
    }

    #[tokio::test]
    async fn fs_put_file_caps_at_1mb() {
        let state = test_state();
        let root = test_dir("fs-put-cap");
        let path = root.join("big.txt");
        let uri = format!("/api/v1/fs/file?path={}", path.to_string_lossy());

        // Exactly 1MB is fine.
        let (status, _, _) = put_raw(&state, &uri, vec![b'a'; 1024 * 1024]).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 1024 * 1024);

        // One byte over is a 413, and the file is untouched.
        let (status, _, body) = put_raw(&state, &uri, vec![b'b'; 1024 * 1024 + 1]).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(err["error"].as_str().unwrap().contains("too large"));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 1024 * 1024);
    }

    /// Age a file's mtime by `secs` so second-resolution ranking tests do not
    /// have to sleep.
    fn age_file(path: &std::path::Path, secs: u64) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs))
            .unwrap();
    }

    #[tokio::test]
    async fn fs_quickopen_ranks_matches_and_ignores() {
        let state = test_state();
        let root = test_dir("quickopen");
        for dir in [
            "src",
            "map",
            "docs",
            "node_modules",
            "target",
            ".git",
            "work",
            "dist",
            "__pycache__",
            ".venv",
            "venv",
            ".snakemake",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        // Tier 0 (name-prefix), newer beats older within the tier.
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/main_test.rs"), "#[test]").unwrap();
        age_file(&root.join("src/main_test.rs"), 3600);
        // Tier 1 (name-substring): "domain" contains "main".
        std::fs::write(root.join("src/domain.rs"), "struct D;").unwrap();
        // Tier 2 (path-subsequence): m-a-i-n spread across "map/init.txt".
        std::fs::write(root.join("map/init.txt"), "x").unwrap();
        // Non-match.
        std::fs::write(root.join("docs/other.txt"), "y").unwrap();
        // Ignored directories, all with tempting matches inside.
        for ignored in [
            "node_modules/main.js",
            "target/main.rs",
            ".git/main",
            "work/main.txt",
            "dist/main.css",
            "__pycache__/main.pyc",
            ".venv/main.py",
            "venv/main.py",
            ".snakemake/main.log",
        ] {
            std::fs::write(root.join(ignored), "z").unwrap();
        }

        let (status, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let ws_id = ws["id"].as_str().unwrap().to_string();

        // Ranked: prefix (mtime-tiebroken) > substring > subsequence, and
        // nothing from the ignored directories leaks in.
        let uri = format!("/api/v1/fs/quickopen?workspace_id={ws_id}&q=main");
        let (status, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        let rels: Vec<&str> = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["rel"].as_str().unwrap())
            .collect();
        assert_eq!(
            rels,
            [
                "src/main.rs",
                "src/main_test.rs",
                "src/domain.rs",
                "map/init.txt"
            ]
        );
        let first = &json["entries"][0];
        assert_eq!(first["name"], "main.rs");
        assert_eq!(
            first["path"].as_str().unwrap(),
            std::fs::canonicalize(&root)
                .unwrap()
                .join("src/main.rs")
                .to_str()
                .unwrap()
        );
        assert!(first["mtime"].as_u64().unwrap() > 0);

        // Matching is case-insensitive.
        let uri = format!("/api/v1/fs/quickopen?workspace_id={ws_id}&q=MAIN");
        let (_, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(json["entries"][0]["rel"], "src/main.rs");

        // Empty query: every indexed file, most recent first.
        let uri = format!("/api/v1/fs/quickopen?workspace_id={ws_id}&q=");
        let (_, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(json["entries"].as_array().unwrap().len(), 5);

        // limit is honored.
        let uri = format!("/api/v1/fs/quickopen?workspace_id={ws_id}&q=main&limit=2");
        let (_, json) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(json["entries"].as_array().unwrap().len(), 2);

        // Unknown workspaces are 404s.
        let (status, err) = request(
            &state,
            Method::GET,
            "/api/v1/fs/quickopen?workspace_id=w-nope&q=x",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(err["error"].as_str().unwrap().contains("w-nope"));
    }

    #[tokio::test]
    async fn raw_serves_byte_ranges() {
        let state = test_state();
        let root = test_dir("fs-raw-range");
        let path = root.join("doc.pdf");
        std::fs::write(&path, b"0123456789").unwrap();

        let (_, json) = request(
            &state,
            Method::POST,
            "/api/v1/fs/ticket",
            Some(serde_json::json!({"path": path.to_string_lossy()})),
        )
        .await;
        let ticket = json["ticket"].as_str().unwrap();
        let uri = format!("/raw/{ticket}");

        let ranged = |range: &'static str| {
            let state = state.clone();
            let uri = uri.clone();
            async move {
                let res = app(state)
                    .oneshot(
                        Request::builder()
                            .uri(&uri)
                            .header(header::RANGE, range)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = res.status();
                let headers = res.headers().clone();
                let bytes = res.into_body().collect().await.unwrap().to_bytes();
                (status, headers, bytes)
            }
        };

        // Full fetch advertises range support.
        let (status, headers, body) = request_bytes(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"0123456789");
        assert_eq!(header_str(&headers, "accept-ranges"), "bytes");

        // bounded, open-ended, and suffix forms.
        let (status, headers, body) = ranged("bytes=2-5").await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(&body[..], b"2345");
        assert_eq!(header_str(&headers, "content-range"), "bytes 2-5/10");
        assert_eq!(header_str(&headers, "content-type"), "application/pdf");

        let (status, headers, body) = ranged("bytes=7-").await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(&body[..], b"789");
        assert_eq!(header_str(&headers, "content-range"), "bytes 7-9/10");

        let (status, _, body) = ranged("bytes=-3").await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(&body[..], b"789");

        // An end past EOF clamps.
        let (status, headers, body) = ranged("bytes=8-999").await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(&body[..], b"89");
        assert_eq!(header_str(&headers, "content-range"), "bytes 8-9/10");

        // A start past EOF is unsatisfiable.
        let (status, headers, _) = ranged("bytes=100-").await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(header_str(&headers, "content-range"), "bytes */10");

        // Malformed and multipart ranges fall back to the whole file.
        for odd in ["bytes=nope", "bytes=1-2,4-5", "chapters=1-2"] {
            let res = app(state.clone())
                .oneshot(
                    Request::builder()
                        .uri(&uri)
                        .header(header::RANGE, odd)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{odd}");
        }
    }

    #[tokio::test]
    async fn fs_ticket_mints_and_raw_serves_without_auth() {
        let state = test_state();
        let root = test_dir("fs-ticket");
        let path = root.join("pic.png");
        std::fs::write(&path, b"\x89PNG fake image bytes").unwrap();

        // Mint (bearer-authed).
        let (status, json) = request(
            &state,
            Method::POST,
            "/api/v1/fs/ticket",
            Some(serde_json::json!({"path": path.to_string_lossy()})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let ticket = json["ticket"].as_str().unwrap().to_string();
        assert!(ticket.starts_with("t-"), "bad ticket {ticket}");
        assert_eq!(ticket.len(), 34, "bad ticket {ticket}");
        assert!(ticket[2..]
            .chars()
            .all(|c| matches!(c, '0'..='9' | 'a'..='f')));

        // Fetch with NO Authorization header.
        let uri = format!("/raw/{ticket}");
        let (status, headers, body) = request_bytes(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"\x89PNG fake image bytes");
        assert_eq!(header_str(&headers, "content-type"), "image/png");
        assert!(headers.get("content-security-policy").is_none());

        // Tickets are reusable within their TTL (an <img> may refetch).
        let (status, _, _) = request_bytes(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);

        // Unknown tickets are 404s.
        let (status, _, _) = request_bytes(
            &state,
            Method::GET,
            "/raw/t-00000000000000000000000000000000",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // A file that vanished after minting is a 404 too.
        std::fs::remove_file(&path).unwrap();
        let (status, _, _) = request_bytes(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Minting for a directory or a missing file is a 400.
        for bad in [
            root.to_string_lossy().into_owned(),
            "/no/such/pic.png".into(),
        ] {
            let (status, err) = request(
                &state,
                Method::POST,
                "/api/v1/fs/ticket",
                Some(serde_json::json!({"path": bad})),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(err["error"].is_string());
        }
    }

    #[tokio::test]
    async fn fs_ticket_expires() {
        let state = test_state();
        let root = test_dir("fs-ticket-expiry");
        let path = root.join("page.txt");
        std::fs::write(&path, "still here").unwrap();

        let (status, json) = request(
            &state,
            Method::POST,
            "/api/v1/fs/ticket",
            Some(serde_json::json!({"path": path.to_string_lossy()})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let ticket = json["ticket"].as_str().unwrap().to_string();

        let uri = format!("/raw/{ticket}");
        let (status, _, _) = request_bytes(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);

        // Once expired the ticket is gone for good, even though the file
        // still exists.
        lock(&state.tickets).expire(&ticket);
        let (status, _, _) = request_bytes(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn raw_html_is_sandboxed() {
        let state = test_state();
        let root = test_dir("fs-raw-html");
        let path = root.join("report.html");
        std::fs::write(&path, "<h1>hi</h1><script>runs_in_sandbox()</script>").unwrap();

        let (_, json) = request(
            &state,
            Method::POST,
            "/api/v1/fs/ticket",
            Some(serde_json::json!({"path": path.to_string_lossy()})),
        )
        .await;
        let ticket = json["ticket"].as_str().unwrap();

        let uri = format!("/raw/{ticket}");
        let (status, headers, body) = request_bytes(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(header_str(&headers, "content-type"), "text/html");
        assert_eq!(
            header_str(&headers, "content-security-policy"),
            "sandbox allow-scripts"
        );
        assert_eq!(header_str(&headers, "referrer-policy"), "no-referrer");
        // Raw bytes pass through unmodified — the sandbox does the confining.
        assert_eq!(&body[..], b"<h1>hi</h1><script>runs_in_sandbox()</script>");
    }

    #[tokio::test]
    async fn view_state_put_get_round_trip_and_persists() {
        let data_dir = test_dir("view-state");
        let state = test_state_with_data_dir(0, data_dir.clone());

        let blob = serde_json::json!({
            "layout": {"type": "pane", "tabs": [{"surface": "terminal", "session": "s-1"}]},
            "focusMode": false,
            "zoom": serde_json::Value::Null,
        });
        let (status, body) = request(
            &state,
            Method::PUT,
            "/api/v1/view-state/win-abc_123",
            Some(blob.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(body, serde_json::Value::Null);

        let (status, body) =
            request(&state, Method::GET, "/api/v1/view-state/win-abc_123", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"state": blob}));

        // A second PUT overwrites.
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/view-state/win-abc_123",
            Some(serde_json::json!({"v": 2})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = request(&state, Method::GET, "/api/v1/view-state/win-abc_123", None).await;
        assert_eq!(body, serde_json::json!({"state": {"v": 2}}));

        // Other keys are independent.
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/view-state/win-other",
            Some(serde_json::json!(true)),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = request(&state, Method::GET, "/api/v1/view-state/win-abc_123", None).await;
        assert_eq!(body, serde_json::json!({"state": {"v": 2}}));

        // Survives a daemon restart (fresh state over the same data dir).
        let reloaded = test_state_with_data_dir(0, data_dir);
        let (status, body) = request(
            &reloaded,
            Method::GET,
            "/api/v1/view-state/win-abc_123",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"state": {"v": 2}}));
    }

    #[tokio::test]
    async fn view_state_unknown_key_is_404() {
        let (status, body) = request(
            &test_state(),
            Method::GET,
            "/api/v1/view-state/win-nope",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, serde_json::json!({"error": "not found"}));
    }

    #[tokio::test]
    async fn view_state_bad_key_is_400() {
        let state = test_state();
        let too_long = "a".repeat(65);
        // "sp%20ace" percent-decodes to a key with a space.
        for key in ["bad.key", "sp%20ace", too_long.as_str()] {
            let uri = format!("/api/v1/view-state/{key}");
            let (status, err) = request(&state, Method::GET, &uri, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "GET {key}");
            assert!(err["error"].is_string());
            let (status, err) =
                request(&state, Method::PUT, &uri, Some(serde_json::json!({}))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "PUT {key}");
            assert!(err["error"].is_string());
        }
        // A 64-char key is still fine.
        let max_key = "k".repeat(64);
        let uri = format!("/api/v1/view-state/{max_key}");
        let (status, _) = request(&state, Method::PUT, &uri, Some(serde_json::json!(1))).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn view_state_rejects_non_json_body() {
        let res = app(test_state())
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/v1/view-state/win-raw")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::from("not json at all"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn view_state_oversize_is_413_and_not_stored() {
        let state = test_state();

        // A body of exactly 64KB is accepted: {"blob":"x..."} is 11 bytes of
        // scaffolding around the payload string.
        let fitting = serde_json::json!({"blob": "x".repeat(64 * 1024 - 11)});
        assert_eq!(fitting.to_string().len(), 64 * 1024);
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/view-state/win-fits",
            Some(fitting),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // One byte over is a 413 and nothing is stored.
        let oversize = serde_json::json!({"blob": "x".repeat(64 * 1024 - 10)});
        assert_eq!(oversize.to_string().len(), 64 * 1024 + 1);
        let (status, err) = request(
            &state,
            Method::PUT,
            "/api/v1/view-state/win-big",
            Some(oversize),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(err["error"].is_string());
        let (status, _) = request(&state, Method::GET, "/api/v1/view-state/win-big", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn view_state_without_token_is_401() {
        for method in [Method::GET, Method::PUT] {
            let res = app(test_state())
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri("/api/v1/view-state/win-abc")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method}");
        }
    }

    #[tokio::test]
    async fn session_spawn_size_is_honored_and_clamped() {
        let state = test_state();
        let root = test_dir("size-root");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let workspace_id = ws["id"].as_str().unwrap().to_string();

        let spawn = |body: serde_json::Value| {
            let state = state.clone();
            async move {
                let (status, session) =
                    request(&state, Method::POST, "/api/v1/sessions", Some(body)).await;
                assert_eq!(status, StatusCode::OK, "spawn failed: {session}");
                session
            }
        };

        // An in-range size spawns the PTY at exactly that size.
        let session = spawn(serde_json::json!({
            "workspace_id": workspace_id, "cols": 132, "rows": 43,
        }))
        .await;
        assert_eq!(session["cols"], 132);
        assert_eq!(session["rows"], 43);
        let entry = session_entry(&state, session["id"].as_str().unwrap()).await;
        assert_eq!(entry["cols"], 132);
        assert_eq!(entry["rows"], 43);
        state.sessions.kill(session["id"].as_str().unwrap()).ok();

        // Too small clamps up to 20x5; too large clamps down to 500x200.
        let session = spawn(serde_json::json!({
            "workspace_id": workspace_id, "cols": 1, "rows": 1000,
        }))
        .await;
        assert_eq!(session["cols"], 20);
        assert_eq!(session["rows"], 200);
        state.sessions.kill(session["id"].as_str().unwrap()).ok();

        let session = spawn(serde_json::json!({
            "workspace_id": workspace_id, "cols": 501, "rows": 1,
        }))
        .await;
        assert_eq!(session["cols"], 500);
        assert_eq!(session["rows"], 5);
        state.sessions.kill(session["id"].as_str().unwrap()).ok();

        // Omitted sizes keep the 80x24 default.
        let session = spawn(serde_json::json!({"workspace_id": workspace_id})).await;
        assert_eq!(session["cols"], 80);
        assert_eq!(session["rows"], 24);
        state.sessions.kill(session["id"].as_str().unwrap()).ok();
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
