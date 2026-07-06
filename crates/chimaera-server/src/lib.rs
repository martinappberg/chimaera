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
use axum::routing::{delete, get};
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
    /// Registered workspaces, persisted to `workspaces.json` on change.
    pub(crate) workspaces: Mutex<workspaces::WorkspaceStore>,
    /// Owner of all PTY sessions; outlives any client connection.
    pub(crate) sessions: Arc<chimaera_pty::SessionManager>,
    /// session id -> workspace id.
    pub(crate) session_workspaces: Mutex<HashMap<String, String>>,
}

impl AppState {
    pub(crate) fn new(token: String, hostname: String, pid: u32, workspaces_path: PathBuf) -> Self {
        AppState {
            token,
            started: Instant::now(),
            hostname,
            pid,
            workspaces: Mutex::new(workspaces::WorkspaceStore::load(workspaces_path)),
            sessions: chimaera_pty::SessionManager::new(),
            session_workspaces: Mutex::new(HashMap::new()),
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
        .with_state(state.clone());

    // The WS route stays outside the bearer-header middleware: browsers cannot
    // set headers on a WebSocket, so it authenticates via its first frame.
    let ws = Router::new()
        .route("/ws/sessions/{id}", get(ws::session_ws))
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
        Arc::new(AppState::new(
            "test-token".to_string(),
            "testhost".to_string(),
            4242,
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
            serde_json::from_slice(&bytes).unwrap()
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
