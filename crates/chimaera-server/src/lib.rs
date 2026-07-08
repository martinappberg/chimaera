mod agents;
mod api;
mod assets;
mod fs;
mod git;
mod launcher;
mod ledger;
mod links;
mod mcp;
mod naming;
mod quickopen;
mod recents;
mod runtimes;
mod settings;
mod spawn;
mod update;
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

/// Upper bound on how long a sessions snapshot waits for ledger restore.
/// Restore is normally sub-second; past this the snapshot serves whatever
/// truth exists rather than blanking the UI behind a wedged respawn.
const RESTORE_WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(15);

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
    /// Ended agent conversations per workspace (the rail's Recents section),
    /// persisted to `recents.json` on change.
    pub(crate) recents: Mutex<recents::RecentsStore>,
    /// Bumped whenever the recents store changes; `/ws/events` pushes a
    /// `recents` frame so the rail refetches instead of guessing at timing.
    pub(crate) recents_epoch: std::sync::atomic::AtomicU64,
    /// Durable session ledger (`sessions.json`): reconciled from live state,
    /// consumed at boot to resurrect sessions across restarts. See `ledger`.
    pub(crate) ledger: Mutex<ledger::LedgerStore>,
    /// session id -> the scheme ("light"/"dark") it was spawned/themed for;
    /// resurrection re-themes successors with it. Pruned by the reconciler.
    pub(crate) session_themes: Mutex<HashMap<String, String>>,
    /// What the daemon knows about newer releases (see `update`).
    pub(crate) update: Mutex<update::UpdateStatus>,
    /// Bumped when the update status changes; drives the `update` ws frame.
    pub(crate) update_epoch: std::sync::atomic::AtomicU64,
    /// User settings (the settings.json ground truth), stored in the config
    /// dir; mtime-checked on read so hand-edits surface without a restart.
    pub(crate) settings: Mutex<settings::SettingsStore>,
    /// Owner of all PTY sessions; outlives any client connection.
    pub(crate) sessions: Arc<chimaera_pty::SessionManager>,
    /// session id -> workspace id.
    pub(crate) session_workspaces: Mutex<HashMap<String, String>>,
    /// session id -> agent wrapper state (kind "agent" sessions only).
    pub(crate) agents: Mutex<HashMap<String, agents::AgentRecord>>,
    /// session id -> polled shell display name (naming rule zero); written
    /// by the per-session watcher in `naming`, read by `session_json`.
    pub(crate) display_names: Mutex<HashMap<String, String>>,
    /// session id -> polled current working directory (shell sessions only);
    /// written by the same watcher, surfaced as `cwd_current` on session JSON
    /// (agents and never-polled shells fall back to the spawn cwd).
    pub(crate) current_cwds: Mutex<HashMap<String, PathBuf>>,
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
    /// Read-only git service (status/diff): discovery cache, per-workspace nudge
    /// epochs, and a bounded pool for `git` child processes. Never persisted.
    pub(crate) git: git::GitService,
    /// Signalled whenever the session list / agent state / titles change;
    /// wakes /ws/events subscribers (a 1s tick catches anything missed).
    pub(crate) changes: tokio::sync::Notify,
    /// False while the boot ledger is being consumed (sessions resurrected /
    /// retired). Sessions snapshots wait for it: serving starts concurrently
    /// with resurrection, and a snapshot taken mid-restore reads as "those
    /// sessions are gone" — the UI would prune their tabs out of restored
    /// layouts. Defaults true (no restore pending); `run` flips it false
    /// before the listener accepts, `ledger::run` back once restore is done.
    pub(crate) restored: tokio::sync::watch::Sender<bool>,
    /// Signalled by `POST /shutdown` to trigger graceful exit in-band (the
    /// only non-signal way to stop the daemon). Awaited alongside SIGINT/
    /// SIGTERM by the server's graceful-shutdown future.
    pub(crate) shutdown: tokio::sync::Notify,
    /// Agent binaries resolved via the login shell (with `--version`),
    /// cached per agent for the daemon's lifetime;
    /// `GET /api/v1/agents?refresh=true` bypasses and refills it.
    pub(crate) agent_bins: Mutex<HashMap<agents::AgentKind, launcher::AgentDetection>>,
    /// Root of Claude Code's per-project transcript store, normally
    /// `~/.claude/projects`; tests point it at a fixture dir.
    pub(crate) claude_projects_dir: PathBuf,
    /// Managed-runtime prefix (`~/.chimaera/agents`): curated installs land
    /// in `<agent>/<version>/bin/` here, activated via per-agent symlinks
    /// in `bin/`. Derived from the data dir, so tests are isolated for free.
    pub(crate) managed_root: PathBuf,
    /// Managed-worktree prefix (`~/.chimaera/worktrees/<repo>/<branch>`).
    /// Chimaera creates worktrees ONLY here, and removes ONLY what is under
    /// here — the containment check is what keeps `worktree remove` from ever
    /// touching the user's own checkouts. Derived from the data dir, so an
    /// isolated `CHIMAERA_HOME` (and every test) is sandboxed for free.
    pub(crate) worktrees_root: PathBuf,
    /// Theming-shim dir (`~/.chimaera/shims`), prepended to every session's
    /// PATH via spawn env only — user dotfiles are never touched.
    pub(crate) shims_dir: PathBuf,
    /// Live install sessions, one per agent (POST /agents/{id}/install
    /// answers 409 while one runs): session id + reservation time. The id
    /// registers in `SessionManager` only after spawn, so a reservation
    /// younger than `runtimes::INSTALL_RESERVATION_GRACE` is busy even with
    /// no visible session. Cleaned up by the install watcher.
    pub(crate) installs: Mutex<HashMap<agents::AgentKind, (String, Instant)>>,
    /// The user's own Claude Code settings file (`~/.claude/settings.json`);
    /// an explicit theme there suppresses chimaera's theme injection. Tests
    /// point it at a fixture.
    pub(crate) claude_settings_path: PathBuf,
    /// The user's codex config (`~/.codex/config.toml`); same respect rule.
    pub(crate) codex_config_path: PathBuf,
}

impl AppState {
    pub(crate) fn new(
        token: String,
        hostname: String,
        pid: u32,
        port: u16,
        data_dir: PathBuf,
        config_dir: PathBuf,
    ) -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
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
            recents: Mutex::new(recents::RecentsStore::load(data_dir.join("recents.json"))),
            recents_epoch: std::sync::atomic::AtomicU64::new(0),
            ledger: Mutex::new(ledger::LedgerStore::new(data_dir.join("sessions.json"))),
            session_themes: Mutex::new(HashMap::new()),
            update: Mutex::new(update::UpdateStatus::default()),
            update_epoch: std::sync::atomic::AtomicU64::new(0),
            settings: Mutex::new(settings::SettingsStore::load(
                config_dir.join("settings.json"),
            )),
            sessions: chimaera_pty::SessionManager::new(),
            session_workspaces: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            display_names: Mutex::new(HashMap::new()),
            current_cwds: Mutex::new(HashMap::new()),
            exec_status: Mutex::new(HashMap::new()),
            links: Mutex::new(HashMap::new()),
            tickets: Mutex::new(fs::TicketStore::default()),
            quickopen: Mutex::new(quickopen::QuickOpenCache::default()),
            git: git::GitService::new(),
            changes: tokio::sync::Notify::new(),
            restored: tokio::sync::watch::channel(true).0,
            shutdown: tokio::sync::Notify::new(),
            agent_bins: Mutex::new(HashMap::new()),
            claude_projects_dir: home.join(".claude").join("projects"),
            managed_root: data_dir.join("agents"),
            worktrees_root: data_dir.join("worktrees"),
            shims_dir: data_dir.join("shims"),
            installs: Mutex::new(HashMap::new()),
            claude_settings_path: home.join(".claude").join("settings.json"),
            codex_config_path: home.join(".codex").join("config.toml"),
        }
    }

    /// Wait (bounded by `RESTORE_WAIT_CAP`) until the boot ledger has been
    /// consumed. Every surface that reports the session list calls this
    /// first, so a client connecting during resurrection never sees — and
    /// acts on — a half-restored roster.
    pub(crate) async fn wait_restored(&self) {
        let mut rx = self.restored.subscribe();
        let _ = tokio::time::timeout(RESTORE_WAIT_CAP, rx.wait_for(|done| *done)).await;
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
        .route("/workspaces/{id}", delete(api::delete_workspace))
        .route("/workspaces/{id}/open", post(api::open_workspace))
        .route(
            "/sessions",
            get(api::list_sessions)
                .post(api::create_session)
                .delete(api::delete_all_sessions),
        )
        .route(
            "/sessions/{id}",
            delete(api::delete_session).patch(api::rename_session),
        )
        // In-band graceful shutdown: end every session, then stop the daemon.
        // The only way (besides an OS signal) to bring the daemon down — the
        // app drives it through the tunnel to shut a remote host down.
        .route("/shutdown", post(api::shutdown))
        .route("/sessions/{id}/exec", post(api::exec_session))
        .route("/sessions/{id}/journal", get(api::session_journal))
        .route("/links", get(links::list_links).put(links::put_link))
        .route("/links/{terminal_id}", delete(links::delete_link))
        .route("/agents", get(launcher::list_agents))
        .route(
            "/agents/{id}/install",
            post(runtimes::install_agent).delete(runtimes::uninstall_agent),
        )
        .route("/agents/claude/sessions", get(launcher::claude_resumables))
        .route("/recents", get(recents::list_recents))
        .route("/update", get(update::get_update))
        .route(
            "/view-state/{key}",
            get(view_state::get_view_state).put(view_state::put_view_state),
        )
        .route(
            "/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        .route("/fs/home", get(fs::home))
        .route("/fs/dirs", get(fs::dirs))
        .route("/fs/list", get(fs::list))
        .route("/fs/file", get(fs::file).put(fs::put_file))
        .route("/fs/markdown", get(fs::markdown))
        .route("/fs/table", get(fs::table))
        .route("/fs/quickopen", get(quickopen::quickopen))
        .route("/fs/validate", post(fs::validate))
        .route("/git/status", get(git::status))
        .route("/git/diff", get(git::diff))
        .route(
            "/git/worktrees",
            get(git::worktrees)
                .post(git::create_worktree)
                .delete(git::remove_worktree),
        )
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
    // A predecessor that stopped gracefully left a handoff: rebind its port
    // with its token so ssh forwards stay valid and every client heals with
    // a plain reconnect — the "update without losing your windows" half of
    // the restart story (the ledger is the sessions half). An explicit
    // conflicting --port wins over the handoff; a crash never leaves one.
    let (listener, token) = match chimaera_core::Handoff::consume()
        .filter(|h| cfg.port.is_none() || cfg.port == Some(h.port))
    {
        Some(handoff) => match rebind(handoff.port).await {
            Some(listener) => (listener, handoff.token),
            None => {
                tracing::warn!(
                    port = handoff.port,
                    "handoff port still busy; starting fresh"
                );
                (
                    fresh_listener(cfg.port).await?,
                    chimaera_core::generate_token(),
                )
            }
        },
        None => (
            fresh_listener(cfg.port).await?,
            chimaera_core::generate_token(),
        ),
    };
    let port = listener.local_addr()?.port();

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
        build: Some(chimaera_core::BUILD_ID.to_string()),
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
        chimaera_core::config_dir(),
    ));

    // Theming shims: regenerated at every daemon start (and after installs /
    // uninstalls / settings edits) so they always match this build's resolution
    // and the current managed-install + explicit-path picture.
    runtimes::regenerate_shims(&state);

    // Backstop poll for out-of-band git changes (external editor, terminal
    // `git` commands); event-driven refresh covers the rest. Idle-cheap.
    tokio::spawn(git::backstop_poll(state.clone()));

    // Session ledger: consume what the previous daemon left (resurrect /
    // retire), then keep sessions.json reconciled until shutdown. Flip
    // `restored` false HERE, before the listener accepts: the spawned task
    // may not have run yet when the first client connects, and that client's
    // sessions snapshot must wait out the resurrection (see AppState).
    state.restored.send_replace(false);
    tokio::spawn(ledger::run(state.clone()));

    // Release awareness (GET /api/v1/update + the `update` ws frame).
    tokio::spawn(update::run_checker(state.clone()));

    // `state.clone()` (not a move) so the post-serve ledger snapshot + handoff
    // below still own it after graceful shutdown returns.
    axum::serve(listener, app(state.clone()))
        .with_graceful_shutdown(shutdown_signal(state.clone()))
        .await
        .context("server error")?;

    // Graceful stop = planned: flush the ledger (the reconciler's last write
    // may be a few seconds stale) and leave a handoff so a successor within
    // the freshness window keeps this port + token. Sessions die with this
    // process — the ledger written here is exactly what resurrects them.
    let (entries, links) = ledger::snapshot(&state);
    lock(&state.ledger).write_if_changed(&entries, &links);
    if let Err(err) = chimaera_core::Handoff::new(port, state.token.clone()).write() {
        tracing::warn!(%err, "failed to write restart handoff");
    }

    chimaera_core::Manifest::remove().context("failed to remove manifest")?;
    tracing::info!("chimaera daemon stopped");
    Ok(())
}

async fn fresh_listener(port: Option<u16>) -> anyhow::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
        .await
        .context("failed to bind 127.0.0.1")
}

/// Try the handoff port for ~5s: the predecessor releases it at exit, but
/// its teardown can lag the successor's start.
async fn rebind(port: u16) -> Option<TcpListener> {
    for _ in 0..20 {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)).await {
            return Some(listener);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    None
}

/// Resolve when SIGINT (ctrl-c) or SIGTERM is received, or when an in-band
/// `POST /shutdown` signals `state.shutdown`.
async fn shutdown_signal(state: Arc<AppState>) {
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
        _ = state.shutdown.notified() => {},
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
    fn test_state_with_claude_store(store: PathBuf) -> Arc<AppState> {
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
    async fn workspaces_open_and_delete() {
        let state = test_state();
        let root = test_dir("ws-open-del");

        let (status, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = ws["id"].as_str().unwrap().to_string();
        let stamped = ws["last_opened_at"].as_u64().unwrap();
        assert!(stamped > 0, "registration stamps last_opened_at");

        // Touch returns the workspace with a fresh (>=) stamp.
        let (status, touched) = request(
            &state,
            Method::POST,
            &format!("/api/v1/workspaces/{id}/open"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(touched["id"], ws["id"]);
        assert!(touched["last_opened_at"].as_u64().unwrap() >= stamped);

        // Unknown ids 404 on both endpoints.
        let (status, _) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces/w-00000000/open",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = request(
            &state,
            Method::DELETE,
            "/api/v1/workspaces/w-00000000",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // DELETE unregisters (files untouched) and the list empties.
        let (status, _) = request(
            &state,
            Method::DELETE,
            &format!("/api/v1/workspaces/{id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(root.is_dir(), "delete never touches the directory");
        let (status, list) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 0);
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
                env_remove: Vec::new(),
                scrollback: None,
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
        // No naming watcher runs for this session, so the ready frame's
        // cwd_current falls back to the spawn cwd.
        assert_eq!(ready["cwd_current"], ready["cwd"]);

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

    /// Attaching to a session that already died replays its final screen
    /// (last words) and closes as exited — never a blank pane. This is the
    /// fast-agent-failure path: codex without OPENAI_API_KEY printed its
    /// error and exited before the client's tab could connect.
    #[tokio::test]
    async fn ws_attach_to_dead_session_replays_last_words() {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let state = test_state();
        let info = state
            .sessions
            .spawn(chimaera_pty::SpawnOpts {
                cwd: test_dir("ws-dead-cwd"),
                name: None,
                cols: 80,
                rows: 24,
                command: Some(vec![
                    "/bin/bash".to_string(),
                    "--norc".to_string(),
                    "--noprofile".to_string(),
                    "-c".to_string(),
                    "echo Missing API key; exit 1".to_string(),
                ]),
                id: None,
                env: Vec::new(),
                env_remove: Vec::new(),
                scrollback: None,
            })
            .expect("spawn session");

        // Wait for the fast death to unregister the session.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while state.sessions.get(&info.id).is_some() {
            assert!(tokio::time::Instant::now() < deadline, "session never died");
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let url = format!("ws://{addr}/ws/sessions/{}", info.id);
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        socket
            .send(WsMessage::text(
                serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
            ))
            .await
            .unwrap();

        // ready (alive: false) -> final-screen binary -> exited, then close.
        let ready = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            other => panic!("expected ready text frame, got {other:?}"),
        };
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready["alive"], false);
        let snapshot = match next_ws_frame(&mut socket).await {
            WsMessage::Binary(bytes) => bytes,
            other => panic!("expected last-words binary frame, got {other:?}"),
        };
        assert!(
            String::from_utf8_lossy(&snapshot).contains("Missing API key"),
            "final screen missing the process's output"
        );
        let exited = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            other => panic!("expected exited text frame, got {other:?}"),
        };
        assert_eq!(exited["type"], "exited");
        assert_eq!(exited["status"], 1);
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
    fn preset_agent(
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
            },
        );
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
        assert_eq!(session["agent_kind"], serde_json::Value::Null);
        assert_eq!(session["agent_state"], serde_json::Value::Null);
        assert_eq!(session["agent_title"], serde_json::Value::Null);
        assert_eq!(session["files_touched"], serde_json::Value::Null);

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
        assert_eq!(entry["files_touched"], serde_json::Value::Null);

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
        preset_agent(
            &state,
            agents::AgentKind::Claude,
            Err("claude not found via login shell (test)".to_string()),
            None,
        );
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
        preset_agent(
            &state,
            agents::AgentKind::Claude,
            Ok(PathBuf::from("/bin/echo")),
            None,
        );
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
        assert_eq!(session["agent_kind"], "claude");
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

    #[tokio::test]
    async fn agents_endpoint_lists_catalog_with_installed_and_missing() {
        let state = test_state();
        preset_agent(
            &state,
            agents::AgentKind::Claude,
            Ok(PathBuf::from("/bin/echo")),
            Some("2.1.196 (Claude Code)"),
        );
        // Installed but ancient: the npm-era codex predates `codex login`.
        preset_agent(
            &state,
            agents::AgentKind::Codex,
            Ok(PathBuf::from("/bin/echo")),
            Some("0.1.2504161551"),
        );
        preset_agent(
            &state,
            agents::AgentKind::Gemini,
            Err("gemini not found (test)".to_string()),
            None,
        );
        preset_agent(
            &state,
            agents::AgentKind::Antigravity,
            Err("agy not found (test)".to_string()),
            None,
        );

        let (status, list) = request(&state, Method::GET, "/api/v1/agents", None).await;
        assert_eq!(status, StatusCode::OK);
        let list = list.as_array().unwrap();
        assert_eq!(list.len(), 4);
        let ids: Vec<&str> = list.iter().map(|a| a["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["claude", "codex", "gemini", "agy"]);

        // Installed and current: path + version present, no outdated flag.
        let claude = &list[0];
        assert_eq!(claude["name"], "Claude Code");
        assert_eq!(claude["installed"], true);
        assert_eq!(claude["path"], "/bin/echo");
        assert_eq!(claude["version"], "2.1.196 (Claude Code)");
        assert!(!claude.as_object().unwrap().contains_key("outdated"));
        assert!(claude["install"]["command"]
            .as_str()
            .unwrap()
            .starts_with("curl "));
        assert!(claude["install"]["url"]
            .as_str()
            .unwrap()
            .starts_with("https://"));

        // Installed but legacy (npm-era codex, no `codex login`): flagged so
        // the UI offers the install command as an update.
        let codex = &list[1];
        assert_eq!(codex["installed"], true);
        assert_eq!(codex["outdated"], true);
        assert_eq!(codex["install"]["command"], "npm install -g @openai/codex");

        // Not installed: muted row material — no path/version, but the
        // install action and docs link are still there.
        let agy = &list[3];
        assert_eq!(agy["name"], "Antigravity CLI");
        assert_eq!(agy["installed"], false);
        let obj = agy.as_object().unwrap();
        assert!(!obj.contains_key("path"), "{agy}");
        assert!(!obj.contains_key("version"), "{agy}");
        assert!(agy["install"]["command"]
            .as_str()
            .unwrap()
            .contains("antigravity.google"));
        assert!(agy["install"]["url"]
            .as_str()
            .unwrap()
            .starts_with("https://"));
    }

    #[tokio::test]
    async fn agent_launcher_endpoints_without_token_are_401() {
        for uri in [
            "/api/v1/agents",
            "/api/v1/agents?refresh=true",
            "/api/v1/agents/claude/sessions?workspace_id=w-x",
            "/api/v1/recents?workspace_id=w-x",
        ] {
            let res = app(test_state())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
        // POST install too: it spawns processes, so auth is not optional.
        let res = app(test_state())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/agents/codex/install")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"workspace_id":"w-x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    /// POST /api/v1/agents/{id}/install: the pinned contract — 404 unknown
    /// agent id, 404 unknown workspace, 400 for gemini (no phase-1 managed
    /// install), and the session mechanics (ordinary kind-"shell" session
    /// with streaming output, one install per agent = 409, watcher cleanup)
    /// driven with a stub script so the test never hits the network.
    #[tokio::test]
    async fn install_endpoint_contract_and_session_mechanics() {
        let state = test_state();
        let ws_id = make_workspace(&state, "install-root").await;

        let (status, body) = request(
            &state,
            Method::POST,
            "/api/v1/agents/nope/install",
            Some(serde_json::json!({"workspace_id": ws_id})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

        let (status, _) = request(
            &state,
            Method::POST,
            "/api/v1/agents/codex/install",
            Some(serde_json::json!({"workspace_id": "w-00000000"})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, body) = request(
            &state,
            Method::POST,
            "/api/v1/agents/gemini/install",
            Some(serde_json::json!({"workspace_id": ws_id})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body["error"].as_str().unwrap().contains("node runtime"),
            "{body}"
        );

        // Stubbed install session: streams like any pane, shows up as a
        // pinned-name shell in the workspace, and blocks a second install.
        let workspace = lock(&state.workspaces).get(&ws_id).unwrap();
        let sid = runtimes::start_install(
            &state,
            agents::AgentKind::Codex,
            &workspace,
            "echo stub-install-output; sleep 30".to_string(),
        )
        .expect("stub install spawned");
        let entry = session_entry(&state, &sid).await;
        assert_eq!(entry["kind"], "shell");
        assert_eq!(entry["display_name"], "install codex");
        assert_eq!(entry["renamed"], true);
        assert_eq!(entry["workspace_id"].as_str().unwrap(), ws_id);

        // Installer output streams into the ordinary pane pipeline.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let att = state.sessions.attach(&sid).expect("attach install pane");
            if String::from_utf8_lossy(&att.snapshot).contains("stub-install-output") {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "install output never reached the pane"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        // Same agent again while running: 409. Another agent: fine.
        let err = runtimes::start_install(
            &state,
            agents::AgentKind::Codex,
            &workspace,
            "echo second".to_string(),
        )
        .expect_err("second install must conflict");
        assert_eq!(err.status(), StatusCode::CONFLICT);
        let other = runtimes::start_install(
            &state,
            agents::AgentKind::Antigravity,
            &workspace,
            "echo other; sleep 30".to_string(),
        )
        .expect("other agent installs in parallel");

        // Kill the codex install; the watcher re-detects and clears the
        // slot, so a fresh install may start.
        state.sessions.kill(&sid).unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        while lock(&state.installs).contains_key(&agents::AgentKind::Codex) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "install watcher never cleared the slot"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let again = runtimes::start_install(
            &state,
            agents::AgentKind::Codex,
            &workspace,
            "echo again; sleep 30".to_string(),
        )
        .expect("slot free after the session ended");
        state.sessions.kill(&again).ok();
        state.sessions.kill(&other).ok();
    }

    /// A same-agent POST racing the spawn window must 409, not overwrite:
    /// the reservation is inserted before `SessionManager::spawn` registers
    /// the session, so a fresh reservation with no visible session is still
    /// busy; only one past the grace window is reclaimable.
    #[tokio::test]
    async fn install_reservation_blocks_the_spawn_registration_race() {
        let state = test_state();
        let ws_id = make_workspace(&state, "install-race").await;
        let workspace = lock(&state.workspaces).get(&ws_id).unwrap();

        // A fresh reservation whose session is not yet registered — the
        // exact in-spawn window.
        lock(&state.installs).insert(
            agents::AgentKind::Codex,
            ("s-notyet00".to_string(), std::time::Instant::now()),
        );
        let err = runtimes::start_install(
            &state,
            agents::AgentKind::Codex,
            &workspace,
            "echo racer".to_string(),
        )
        .expect_err("a fresh reservation must read as busy");
        assert_eq!(err.status(), StatusCode::CONFLICT);

        // The same dead reservation past the grace window: stale, reclaimed.
        lock(&state.installs).insert(
            agents::AgentKind::Codex,
            (
                "s-notyet00".to_string(),
                std::time::Instant::now()
                    - (runtimes::INSTALL_RESERVATION_GRACE + std::time::Duration::from_secs(1)),
            ),
        );
        let sid = runtimes::start_install(
            &state,
            agents::AgentKind::Codex,
            &workspace,
            "echo reclaimed; sleep 30".to_string(),
        )
        .expect("a stale reservation is reclaimable");
        assert_eq!(
            lock(&state.installs)
                .get(&agents::AgentKind::Codex)
                .map(|(s, _)| s.clone()),
            Some(sid.clone()),
            "the reclaim installed its own reservation"
        );
        state.sessions.kill(&sid).ok();
    }

    /// Every chimaera session carries CHIMAERA_SESSION / CHIMAERA_THEME /
    /// CHIMAERA_SHIMS and the shim-dir PATH prefix — asserted through a
    /// real daemon-spawned session's own environment (`/usr/bin/env` in the
    /// PTY; the install spawn path shares `session_env` with POST
    /// /sessions, and a user shell's rc can stall for a minute on this
    /// machine, so the deterministic command is the honest probe).
    #[tokio::test]
    async fn session_env_reaches_spawned_session() {
        let state = test_state();
        let ws_id = make_workspace(&state, "env-root").await;

        // The assembled contract itself: shims first on PATH, session id,
        // scheme, and the wrap's re-prepend handle.
        let env = api::session_env(&state, "s-envx", "light");
        let shims = state.shims_dir.display().to_string();
        assert_eq!(env[0].0, "PATH");
        assert!(env[0].1.starts_with(&format!("{shims}:")), "{env:?}");
        assert!(env.contains(&("CHIMAERA_SESSION".to_string(), "s-envx".to_string())));
        assert!(env.contains(&("CHIMAERA_THEME".to_string(), "light".to_string())));
        assert!(env.contains(&("CHIMAERA_SHIMS".to_string(), shims.clone())));

        // An empty inherited PATH must not leave a trailing colon (an empty
        // member = the cwd on the search path); the fixed system default
        // fills in instead.
        assert_eq!(
            api::spawn_path("/s", ""),
            "/s:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        );
        assert_eq!(api::spawn_path("/s", "/usr/bin:/bin"), "/s:/usr/bin:/bin");

        // Through a real spawn: a stub install session dumps its env into
        // the pane (kept alive so the snapshot — scrollback included — is
        // stable). Grid rows wrap at the pane width, so matching happens on
        // the de-wrapped text.
        let workspace = lock(&state.workspaces).get(&ws_id).unwrap();
        let sid = runtimes::start_install(
            &state,
            agents::AgentKind::Claude,
            &workspace,
            "/usr/bin/env; sleep 30".to_string(),
        )
        .expect("stub env session spawned");
        let needles = [
            format!("CHIMAERA_SESSION={sid}"),
            "CHIMAERA_THEME=dark".to_string(),
            format!("CHIMAERA_SHIMS={shims}"),
            format!("PATH={shims}:"),
        ];
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let att = state.sessions.attach(&sid).expect("attach env session");
            let text = String::from_utf8_lossy(&att.snapshot).replace(['\r', '\n'], "");
            if needles.iter().all(|n| text.contains(n)) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "env dump incomplete; missing {:?}",
                needles
                    .iter()
                    .filter(|n| !text.contains(*n))
                    .collect::<Vec<_>>()
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        state.sessions.kill(&sid).ok();

        // An invalid theme on POST /sessions is a 400, not a silent dark.
        let (status, body) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({
                "workspace_id": ws_id, "kind": "shell", "theme": "blue",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    /// The scheme theme fills the gap in the generated claude settings —
    /// and steps aside when the user's own settings.json picks a theme.
    #[tokio::test]
    async fn claude_theme_fills_gap_in_generated_settings() {
        let user_settings_dir = test_dir("user-claude");
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
        state.claude_settings_path = user_settings_dir.join("settings.json");
        let state = Arc::new(state);
        preset_agent(
            &state,
            agents::AgentKind::Claude,
            Ok(PathBuf::from("/bin/echo")),
            None,
        );
        let ws_id = make_workspace(&state, "theme-root").await;

        let generated_settings = |session: &serde_json::Value| -> serde_json::Value {
            let id = session["id"].as_str().unwrap();
            let path = chimaera_core::runtime_dir()
                .join("agents")
                .join(format!("{id}-settings.json"));
            let value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            std::fs::remove_file(&path).ok();
            value
        };

        // No user theme anywhere: the client scheme lands in the settings.
        let (status, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({
                "workspace_id": ws_id, "kind": "agent", "theme": "light",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{session}");
        let value = generated_settings(&session);
        assert_eq!(value["theme"], "light");
        assert_eq!(
            value["hooks"]["SessionStart"][0]["hooks"][0]["type"], "http",
            "the theme merges into the SAME file the hooks ride"
        );

        // No theme in the body: the default scheme is dark.
        let (_, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({"workspace_id": ws_id, "kind": "agent"})),
        )
        .await;
        assert_eq!(generated_settings(&session)["theme"], "dark");

        // The user set a theme in their own settings.json: hands off.
        std::fs::write(
            &state.claude_settings_path,
            r#"{"theme": "dark", "tui": "fullscreen"}"#,
        )
        .unwrap();
        let (_, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({
                "workspace_id": ws_id, "kind": "agent", "theme": "light",
            })),
        )
        .await;
        assert!(
            generated_settings(&session).get("theme").is_none(),
            "an explicit user theme is never overridden"
        );
    }

    /// GET /api/v1/agents flags managed installs: `managed: true` when the
    /// resolved binary lives under ~/.chimaera/agents, and
    /// `managed_install` says whether POST install has a curated recipe.
    #[tokio::test]
    async fn agents_rows_flag_managed_installs() {
        let state = test_state();
        preset_agent(
            &state,
            agents::AgentKind::Claude,
            Ok(PathBuf::from("/Users/u/.local/bin/claude")),
            Some("2.1.202 (Claude Code)"),
        );
        // A managed codex: resolved through the ~/.chimaera/agents/bin swap.
        preset_agent(
            &state,
            agents::AgentKind::Codex,
            Ok(state.managed_root.join("bin/codex")),
            Some("codex-cli 0.142.5"),
        );
        preset_agent(
            &state,
            agents::AgentKind::Gemini,
            Err("gemini not found (test)".to_string()),
            None,
        );
        preset_agent(
            &state,
            agents::AgentKind::Antigravity,
            Err("agy not found (test)".to_string()),
            None,
        );

        let (status, list) = request(&state, Method::GET, "/api/v1/agents", None).await;
        assert_eq!(status, StatusCode::OK);
        let list = list.as_array().unwrap();
        let row = |id: &str| {
            list.iter()
                .find(|a| a["id"] == id)
                .unwrap_or_else(|| panic!("{id} row missing"))
                .clone()
        };

        // The user's own claude: installed, not managed.
        let claude = row("claude");
        assert_eq!(claude["installed"], true);
        assert!(!claude.as_object().unwrap().contains_key("managed"));
        assert_eq!(claude["managed_install"], true);
        // The managed codex: flagged per the pinned API contract.
        let codex = row("codex");
        assert_eq!(codex["installed"], true);
        assert_eq!(codex["managed"], true);
        assert_eq!(codex["managed_install"], true);
        // gemini: no curated managed install (node runtime, phase 2).
        assert_eq!(row("gemini")["managed_install"], false);
        assert_eq!(row("agy")["managed_install"], true);
    }

    /// Detection falls back to ~/.chimaera/agents/bin when the login shell
    /// misses (managed installs are deliberately not on the user's PATH),
    /// and such rows read as installed + managed.
    #[tokio::test]
    async fn managed_detection_falls_back_when_login_shell_misses() {
        let state = test_state();
        // Ground truth first: if this host has a real standalone agy CLI on
        // the login-shell PATH, the fallback is unreachable — skip. (The
        // Antigravity IDE's launcher shim is refused by detection, so IDE
        // hosts still exercise the fallback.)
        let probe = launcher::detect(&state, agents::AgentKind::Antigravity, true).await;
        if probe.path.is_ok() {
            eprintln!("skipping: a real agy CLI resolves via the login shell on this host");
            return;
        }

        let managed_bin = runtimes::managed_bin_dir(&state.managed_root);
        std::fs::create_dir_all(&managed_bin).unwrap();
        let fake = managed_bin.join("agy");
        std::fs::write(&fake, "#!/bin/sh\necho agy version 9.9.9\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let detection = launcher::detect(&state, agents::AgentKind::Antigravity, true).await;
        assert_eq!(detection.path.as_deref().ok(), Some(fake.as_path()));
        assert!(detection.managed);
        assert_eq!(detection.version.as_deref(), Some("agy version 9.9.9"));

        // And the catalog row reflects it (served from the refreshed cache).
        let (_, list) = request(&state, Method::GET, "/api/v1/agents", None).await;
        let agy = list
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == "agy")
            .unwrap()
            .clone();
        assert_eq!(agy["installed"], true);
        assert_eq!(agy["managed"], true);
        assert_eq!(agy["path"].as_str().unwrap(), fake.to_string_lossy());
    }

    #[tokio::test]
    async fn create_codex_agent_is_plain_tui_with_agent_kind() {
        let state = test_state();
        preset_agent(
            &state,
            agents::AgentKind::Codex,
            Ok(PathBuf::from("/bin/echo")),
            None,
        );
        let root = test_dir("codex-spawn");
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
            Some(serde_json::json!({
                "workspace_id": ws["id"], "kind": "agent",
                "agent": "codex", "model": "o4-mini",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{session}");
        assert_eq!(session["kind"], "agent");
        assert_eq!(session["agent_kind"], "codex");
        // Hook-driven state is claude-only: codex reads as the honest
        // unknown dot, named after its binary until an OSC title lands.
        assert_eq!(session["agent_state"], "unknown");
        assert_eq!(session["display_name"], "codex");
        // No hook settings file was generated (hooks are claude-only).
        let id = session["id"].as_str().unwrap();
        let settings_path = chimaera_core::runtime_dir()
            .join("agents")
            .join(format!("{id}-settings.json"));
        assert!(!settings_path.exists(), "codex must not get hook settings");
    }

    /// Register a workspace and return its id.
    async fn make_workspace(state: &Arc<AppState>, label: &str) -> String {
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
    fn plant_agent_record(
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

    async fn recents_of(state: &Arc<AppState>, workspace_id: &str) -> Vec<serde_json::Value> {
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

    #[tokio::test]
    async fn recents_retire_round_trips_and_persists() {
        let data_dir = test_dir("recents");
        let state = test_state_with_data_dir(0, data_dir.clone());
        let ws = make_workspace(&state, "recents-root").await;
        // The transcript must exist on disk: retire only mints resume ids a
        // click can deliver (claude 2.1.204 interactive sessions persist no
        // transcript, so unverified ids are common, not corrupt).
        let transcript = data_dir.join("abc-123.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        plant_agent_record(
            &state,
            "s-1",
            &ws,
            agents::AgentKind::Claude,
            Some("fix the flaky test"),
            Some(transcript.to_str().unwrap()),
        );

        recents::retire(&state, "s-1", None, None);

        let entries = recents_of(&state, &ws).await;
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0]["kind"], "claude");
        assert_eq!(entries[0]["title"], "fix the flaky test");
        assert_eq!(entries[0]["resume"], "abc-123");
        assert!(entries[0]["last_active"].as_u64().unwrap() > 0);
        // The record and its workspace mapping are gone (retire IS the
        // cleanup path).
        assert!(lock(&state.agents).get("s-1").is_none());
        assert!(lock(&state.session_workspaces).get("s-1").is_none());

        // Daemon restart: a fresh state over the same data dir still has it.
        let reloaded = test_state_with_data_dir(0, data_dir);
        // Same workspace registry file, so the id resolves.
        let entries = recents_of(&reloaded, &ws).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["resume"], "abc-123");
    }

    #[tokio::test]
    async fn recents_skip_untitled_claude_keep_codex_and_pins_win() {
        let state = test_state();
        let ws = make_workspace(&state, "recents-mixed").await;

        // Claude empty boot: fallback name, no transcript — not a memory.
        plant_agent_record(
            &state,
            "s-empty",
            &ws,
            agents::AgentKind::Claude,
            None,
            None,
        );
        recents::retire(&state, "s-empty", None, None);
        assert!(recents_of(&state, &ws).await.is_empty());

        // Codex has no title machinery: its bare-name row still counts.
        plant_agent_record(&state, "s-cdx", &ws, agents::AgentKind::Codex, None, None);
        recents::retire(&state, "s-cdx", None, None);

        // A user-renamed claude keeps its pinned name, and an OSC title
        // beats the ai title (same precedence as live display names).
        plant_agent_record(&state, "s-pin", &ws, agents::AgentKind::Claude, None, None);
        recents::retire(&state, "s-pin", Some("bio-evolve run"), None);

        let entries = recents_of(&state, &ws).await;
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!(entries[0]["title"], "bio-evolve run"); // newest first
        assert_eq!(entries[0]["kind"], "claude");
        assert_eq!(entries[0]["resume"], serde_json::Value::Null);
        assert_eq!(entries[1]["kind"], "codex");
        assert_eq!(entries[1]["title"], "codex");
    }

    #[tokio::test]
    async fn recents_hide_live_conversations_and_dedupe_resumes() {
        let state = test_state();
        let ws = make_workspace(&state, "recents-live").await;
        let store = test_dir("recents-live-transcripts");
        let transcript = store.join("conv-9.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let transcript = transcript.to_str().unwrap();

        plant_agent_record(
            &state,
            "s-a",
            &ws,
            agents::AgentKind::Claude,
            Some("hooks online"),
            Some(transcript),
        );
        recents::retire(&state, "s-a", None, None);
        assert_eq!(recents_of(&state, &ws).await.len(), 1);

        // The same conversation resumed in a live session: hidden, not lost.
        plant_agent_record(
            &state,
            "s-b",
            &ws,
            agents::AgentKind::Claude,
            Some("hooks online"),
            Some(transcript),
        );
        assert!(recents_of(&state, &ws).await.is_empty());

        // It ends again with a newer title: back in the list, still one entry.
        crate::lock(&state.agents).get_mut("s-b").unwrap().ai_title =
            Some("hooks online v2".to_string());
        recents::retire(&state, "s-b", None, None);
        let entries = recents_of(&state, &ws).await;
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0]["title"], "hooks online v2");

        // Resumed live again: claude forks a NEW session id on --resume, so
        // until hooks report the new transcript the live record knows only
        // what it resumed from — that alone must hide the ancestor entry.
        plant_agent_record(&state, "s-c", &ws, agents::AgentKind::Claude, None, None);
        lock(&state.agents).get_mut("s-c").unwrap().resumed_from = Some("conv-9".to_string());
        assert!(
            recents_of(&state, &ws).await.is_empty(),
            "resumed_from must hide the ancestor entry"
        );

        // It ends under its new id: the ancestor entry is superseded by the
        // continuation — one entry, newest title, resumable via the NEW id.
        let continuation = store.join("conv-10.jsonl");
        std::fs::write(&continuation, "{}\n").unwrap();
        {
            let mut agents_map = lock(&state.agents);
            let record = agents_map.get_mut("s-c").unwrap();
            record.ai_title = Some("hooks online v3".to_string());
            record.transcript_path = Some(continuation);
        }
        recents::retire(&state, "s-c", None, None);
        let entries = recents_of(&state, &ws).await;
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0]["title"], "hooks online v3");
        assert_eq!(entries[0]["resume"], "conv-10");
    }

    /// GET /recents merges the daemon's own history with the claude
    /// transcript store (the popover has no resume list of its own): daemon
    /// entries win identity collisions, transcript-only conversations appear
    /// as claude rows, live conversations are hidden from both sources.
    #[tokio::test]
    async fn recents_merge_daemon_history_with_transcript_store() {
        let store = test_dir("recents-merge-store");
        let state = test_state_with_claude_store(store.clone());
        let root = test_dir("recents-merge-root");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let ws_id = ws["id"].as_str().unwrap().to_string();
        let project_dir = store.join(launcher::encode_cwd(std::path::Path::new(
            ws["root"].as_str().unwrap(),
        )));
        std::fs::create_dir_all(&project_dir).unwrap();

        // A conversation only the transcript store knows (ended before the
        // daemon existed, or claude run outside chimaera).
        write_transcript(
            &project_dir,
            "hist-1",
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"annotate the variants"}}"#,
                "\n",
            ),
            500,
        );
        // A conversation BOTH know: transcript says one title, the daemon
        // watched it end with a newer one — the daemon entry must win.
        let both_path = project_dir.join("both-2.jsonl");
        write_transcript(
            &project_dir,
            "both-2",
            concat!(
                r#"{"type":"ai-title","aiTitle":"stale title","sessionId":"both-2"}"#,
                "\n"
            ),
            400,
        );
        plant_agent_record(
            &state,
            "s-both",
            &ws_id,
            agents::AgentKind::Claude,
            Some("fresh daemon title"),
            Some(both_path.to_str().unwrap()),
        );
        recents::retire(&state, "s-both", None, None);
        // And one daemon-only codex conversation (no transcript machinery).
        plant_agent_record(
            &state,
            "s-cdx",
            &ws_id,
            agents::AgentKind::Codex,
            None,
            None,
        );
        recents::retire(&state, "s-cdx", None, None);

        let entries = recents_of(&state, &ws_id).await;
        let titles: Vec<&str> = entries
            .iter()
            .map(|e| e["title"].as_str().unwrap())
            .collect();
        assert_eq!(entries.len(), 3, "{entries:?}");
        // Newest first: the two just-retired daemon entries, then history.
        assert!(titles.contains(&"fresh daemon title"), "{titles:?}");
        assert!(titles.contains(&"codex"), "{titles:?}");
        assert_eq!(*titles.last().unwrap(), "annotate the variants");
        assert!(!titles.contains(&"stale title"), "daemon entry must win");
        let hist = entries.last().unwrap();
        assert_eq!(hist["kind"], "claude");
        assert_eq!(hist["resume"], "hist-1");
        assert!(hist["last_active"].as_u64().unwrap() > 0);

        // A live session on the transcript-only conversation hides it too.
        plant_agent_record(
            &state,
            "s-live",
            &ws_id,
            agents::AgentKind::Claude,
            None,
            Some(project_dir.join("hist-1.jsonl").to_str().unwrap()),
        );
        let entries = recents_of(&state, &ws_id).await;
        assert!(
            !entries.iter().any(|e| e["resume"] == "hist-1"),
            "{entries:?}"
        );
        crate::lock(&state.agents).remove("s-live");

        // Resume-then-end: claude forks a new session id and the ANCESTOR
        // transcript stays on disk in the scanned dir. The superseded
        // ancestor must NOT resurrect from the scan — clicking it would
        // fork the conversation from its pre-resume state (review blocker).
        write_transcript(
            &project_dir,
            "both-2b",
            concat!(
                r#"{"type":"ai-title","aiTitle":"fresh daemon title v2","sessionId":"both-2b"}"#,
                "\n",
            ),
            10,
        );
        plant_agent_record(
            &state,
            "s-resumed",
            &ws_id,
            agents::AgentKind::Claude,
            Some("fresh daemon title v2"),
            Some(project_dir.join("both-2b.jsonl").to_str().unwrap()),
        );
        crate::lock(&state.agents)
            .get_mut("s-resumed")
            .unwrap()
            .resumed_from = Some("both-2".to_string());
        recents::retire(&state, "s-resumed", None, None);

        let entries = recents_of(&state, &ws_id).await;
        let lineage: Vec<&serde_json::Value> = entries
            .iter()
            .filter(|e| e["resume"] == "both-2" || e["resume"] == "both-2b")
            .collect();
        assert_eq!(lineage.len(), 1, "ancestor resurrected: {entries:?}");
        assert_eq!(lineage[0]["resume"], "both-2b");
        assert_eq!(lineage[0]["title"], "fresh daemon title v2");
    }

    /// PATCH /sessions/{id} pins a display name for ANY session kind — the
    /// chimaera-owned rename (claude's /rename flows via OSC; codex, gemini,
    /// agy, and shells have nothing, so the app must own it).
    #[tokio::test]
    async fn rename_session_pins_name_for_any_kind() {
        let state = test_state();
        let root = test_dir("rename-root");
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
            Some(serde_json::json!({"workspace_id": ws["id"]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{session}");
        let id = session["id"].as_str().unwrap().to_string();
        assert_eq!(session["renamed"], false);

        let (status, _) = request(
            &state,
            Method::PATCH,
            &format!("/api/v1/sessions/{id}"),
            Some(serde_json::json!({"name": "  qc pipeline  "})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let entry = session_entry(&state, &id).await;
        assert_eq!(entry["renamed"], true);
        assert_eq!(entry["name"], "qc pipeline"); // trimmed
                                                  // The pin outranks every derived name.
        assert_eq!(entry["display_name"], "qc pipeline");

        // Guardrails: empty and unknown.
        let (status, _) = request(
            &state,
            Method::PATCH,
            &format!("/api/v1/sessions/{id}"),
            Some(serde_json::json!({"name": "   "})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = request(
            &state,
            Method::PATCH,
            "/api/v1/sessions/s-nope",
            Some(serde_json::json!({"name": "x"})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        state.sessions.kill(&id).ok();
    }

    #[tokio::test]
    async fn recents_unknown_workspace_is_404() {
        let (status, body) = request(
            &test_state(),
            Method::GET,
            "/api/v1/recents?workspace_id=w-nope",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }

    #[tokio::test]
    async fn create_agent_validates_agent_model_and_resume() {
        let state = test_state();
        preset_agent(
            &state,
            agents::AgentKind::Claude,
            Ok(PathBuf::from("/bin/echo")),
            None,
        );
        preset_agent(
            &state,
            agents::AgentKind::Codex,
            Err("codex not found via login shell (test)".to_string()),
            None,
        );
        let root = test_dir("agent-validate");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let sessions_body = |extra: serde_json::Value| {
            let mut body = serde_json::json!({"workspace_id": ws["id"], "kind": "agent"});
            body.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            body
        };

        // Unknown agent id: 400.
        let (status, err) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(sessions_body(serde_json::json!({"agent": "cursor"}))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("unknown agent"));

        // Resume is claude-only: 400 for codex, before any binary lookup.
        let (status, err) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(sessions_body(
                serde_json::json!({"agent": "codex", "resume": "abc"}),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("resume"));

        // Flag-shaped model/resume values: 400, never argv.
        for field in ["model", "resume"] {
            let (status, err) = request(
                &state,
                Method::POST,
                "/api/v1/sessions",
                Some(sessions_body(
                    serde_json::json!({field: "--dangerously-skip-permissions"}),
                )),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{field}");
            assert!(
                err["error"].as_str().unwrap().contains("invalid"),
                "{field}"
            );
        }

        // A not-installed agent is a 409 with its own install hint.
        let (status, err) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(sessions_body(serde_json::json!({"agent": "codex"}))),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(err["error"].as_str().unwrap().contains("codex not found"));

        // Valid model + resume for claude spawns (argv is unit-tested in
        // launcher::tests; the API can't observe the child's argv).
        let (status, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(sessions_body(serde_json::json!({
                "model": "opus",
                "resume": "5e0d64b2-abcd-abcd-abcd-000000000000",
            }))),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{session}");
        assert_eq!(session["agent_kind"], "claude");
        let settings_path = chimaera_core::runtime_dir()
            .join("agents")
            .join(format!("{}-settings.json", session["id"].as_str().unwrap()));
        assert!(settings_path.exists(), "claude keeps hook injection");
        std::fs::remove_file(&settings_path).ok();
    }

    /// Write one transcript fixture and backdate its mtime.
    fn write_transcript(dir: &std::path::Path, name: &str, body: &str, secs_ago: u64) {
        let path = dir.join(format!("{name}.jsonl"));
        std::fs::write(&path, body).unwrap();
        let mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
    }

    #[tokio::test]
    async fn claude_resumables_titles_order_and_noise() {
        let store = test_dir("claude-store");
        let state = test_state_with_claude_store(store.clone());
        let root = test_dir("resume-root");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let workspace_id = ws["id"].as_str().unwrap().to_string();
        // The store dir is keyed by the *canonical* workspace root, encoded
        // with claude's every-non-alphanumeric-to-dash rule.
        let project_dir = store.join(launcher::encode_cwd(std::path::Path::new(
            ws["root"].as_str().unwrap(),
        )));
        std::fs::create_dir_all(&project_dir).unwrap();

        // Oldest: prompt-only -> truncated first prompt is the title.
        write_transcript(
            &project_dir,
            "aaaa",
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"please refactor the entire qc pipeline so the reports land in results/qc and nothing downstream breaks"}}"#,
                "\n",
            ),
            300,
        );
        // Middle: ai-title outranks the prompt; user+assistant lines count.
        write_transcript(
            &project_dir,
            "bbbb",
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"fix the STAR index"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"on it"}]}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"Fix the STAR index","sessionId":"bbbb"}"#,
                "\n",
            ),
            200,
        );
        // Newest: a rename (custom-title) wins over everything.
        write_transcript(
            &project_dir,
            "cccc",
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"align the fastqs"}}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"Align fastqs","sessionId":"cccc"}"#,
                "\n",
                r#"{"type":"custom-title","customTitle":"Pinned by hand","sessionId":"cccc"}"#,
                "\n",
            ),
            100,
        );
        // Noise: a titleless boot transcript (skipped), a non-jsonl file
        // and a subdirectory (ignored).
        write_transcript(
            &project_dir,
            "dddd",
            "{\"type\":\"mode\",\"mode\":\"normal\"}\n",
            50,
        );
        std::fs::write(project_dir.join("notes.txt"), "not a transcript").unwrap();
        std::fs::create_dir_all(project_dir.join("memory")).unwrap();

        let uri = format!("/api/v1/agents/claude/sessions?workspace_id={workspace_id}");
        let (status, list) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        let list = list.as_array().unwrap();
        let ids: Vec<&str> = list.iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["cccc", "bbbb", "aaaa"], "newest first, noise skipped");

        assert_eq!(list[0]["title"], "Pinned by hand");
        assert_eq!(list[0]["approx_messages"], 1);
        assert_eq!(list[1]["title"], "Fix the STAR index");
        assert_eq!(list[1]["approx_messages"], 3);
        let truncated = list[2]["title"].as_str().unwrap();
        assert!(
            truncated.starts_with("please refactor the entire qc pipeline"),
            "{truncated}"
        );
        assert!(truncated.ends_with('…'), "{truncated}");
        assert!(truncated.chars().count() <= 61, "{truncated}");
        assert_eq!(list[2]["approx_messages"], 1);

        // mtimes are unix seconds matching the backdated fixtures.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for (entry, secs_ago) in [(&list[0], 100u64), (&list[1], 200), (&list[2], 300)] {
            let mtime = entry["mtime"].as_u64().unwrap();
            let expect = now - secs_ago;
            assert!(
                mtime.abs_diff(expect) < 30,
                "mtime {mtime} not within 30s of {expect}"
            );
        }

        // A workspace never used with claude: empty list, not an error.
        let bare_root = test_dir("resume-bare");
        let (_, bare) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": bare_root.to_string_lossy()})),
        )
        .await;
        let uri = format!(
            "/api/v1/agents/claude/sessions?workspace_id={}",
            bare["id"].as_str().unwrap()
        );
        let (status, list) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list, serde_json::json!([]));

        // Unknown workspace: 404.
        let (status, _) = request(
            &state,
            Method::GET,
            "/api/v1/agents/claude/sessions?workspace_id=w-00000000",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn claude_resumables_cap_at_twenty_newest() {
        let store = test_dir("claude-store-cap");
        let state = test_state_with_claude_store(store.clone());
        let root = test_dir("resume-cap-root");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let project_dir = store.join(launcher::encode_cwd(std::path::Path::new(
            ws["root"].as_str().unwrap(),
        )));
        std::fs::create_dir_all(&project_dir).unwrap();

        for i in 0..25u64 {
            write_transcript(
                &project_dir,
                &format!("f{i:02}"),
                &format!(
                    "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"prompt {i}\"}}}}\n"
                ),
                (i + 1) * 60, // f00 newest .. f24 oldest
            );
        }

        let uri = format!(
            "/api/v1/agents/claude/sessions?workspace_id={}",
            ws["id"].as_str().unwrap()
        );
        let (status, list) = request(&state, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        let list = list.as_array().unwrap();
        assert_eq!(list.len(), 20, "capped at 20");
        let ids: Vec<&str> = list.iter().map(|s| s["id"].as_str().unwrap()).collect();
        let expect: Vec<String> = (0..20).map(|i| format!("f{i:02}")).collect();
        assert_eq!(ids, expect, "the 20 newest, newest first");
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
                env_remove: Vec::new(),
                scrollback: None,
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

    /// Poll GET /api/v1/sessions until the session's cwd_current matches.
    async fn wait_cwd_current(state: &Arc<AppState>, id: &str, expected: &std::path::Path) {
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
        let launch = chimaera_core::shellint::shell_launch_for("/bin/bash", &base).expect("launch");
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
                env_remove: Vec::new(),
                scrollback: None,
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
                env_remove: Vec::new(),
                scrollback: None,
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
            assert!(
                tokio::time::Instant::now() < deadline,
                "sleep never started"
            );
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
            out["record"]["output"]
                .as_str()
                .unwrap()
                .contains("queued-ran"),
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
            assert!(
                tokio::time::Instant::now() < deadline,
                "sleep never started"
            );
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
                    env_remove: Vec::new(),
                    scrollback: None,
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
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
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
        let (is_error, text) = mcp_tool_call(
            &state,
            &agent,
            "mk",
            "list_terminals",
            serde_json::json!({}),
        )
        .await;
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
    async fn cwd_current_tracks_shell_cd_and_falls_back_to_spawn_cwd() {
        let state = test_state();
        let root = test_dir("cwd-current");
        std::fs::create_dir(root.join("sub")).unwrap();
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

        // The watcher's first poll reports the spawn cwd.
        wait_cwd_current(&state, &id, &root).await;

        // cd into a subdirectory: cwd_current follows...
        let att = state.sessions.attach(&id).expect("attach");
        att.input
            .send(bytes::Bytes::from("cd sub\n"))
            .await
            .expect("send input");
        wait_cwd_current(&state, &id, &root.join("sub")).await;

        // ...and back.
        att.input
            .send(bytes::Bytes::from("cd ..\n"))
            .await
            .expect("send input");
        wait_cwd_current(&state, &id, &root).await;

        state.sessions.kill(&id).ok();

        // No polled value (agents run no cwd watcher; they keep their spawn
        // cwd): the field falls back to the spawn cwd.
        let agent = inject_agent(&state, "k");
        let entry = session_entry(&state, &agent).await;
        assert_eq!(entry["cwd_current"], entry["cwd"]);
        state.sessions.kill(&agent).ok();
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

    /// Synthetic PostToolUse payload for a file-writing tool, shaped like
    /// the real hook payloads (top-level tool_name + tool_input).
    fn touch_payload(tool: &str, field: &str, path: &str) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": tool,
            "tool_input": { field: path },
        })
    }

    #[tokio::test]
    async fn agent_files_touched_builds_from_post_tool_use_hooks() {
        let state = test_state();
        let id = inject_agent(&state, "k");

        // A fresh agent has an empty list (never null — the UI renders a
        // quiet zero-files chip row, not a missing field).
        assert_eq!(
            session_entry(&state, &id).await["files_touched"],
            serde_json::json!([])
        );

        // Every file-writing tool contributes; non-writing tools do not.
        for (tool, field, path) in [
            ("Write", "file_path", "/w/a.rs"),
            ("Edit", "file_path", "/w/b.rs"),
            ("MultiEdit", "file_path", "/w/c.rs"),
            ("NotebookEdit", "notebook_path", "/w/d.ipynb"),
            ("Bash", "command", "cargo test"),
            ("Read", "file_path", "/w/read-only.rs"),
        ] {
            assert_eq!(
                post_hook(&state, &id, "k", touch_payload(tool, field, path)).await,
                StatusCode::OK
            );
        }
        assert_eq!(
            session_entry(&state, &id).await["files_touched"],
            serde_json::json!(["/w/a.rs", "/w/b.rs", "/w/c.rs", "/w/d.ipynb"])
        );

        // Re-touching an older path moves it to the end: dedupe, newest last.
        post_hook(
            &state,
            &id,
            "k",
            touch_payload("Edit", "file_path", "/w/a.rs"),
        )
        .await;
        assert_eq!(
            session_entry(&state, &id).await["files_touched"],
            serde_json::json!(["/w/b.rs", "/w/c.rs", "/w/d.ipynb", "/w/a.rs"])
        );

        // State changes clear nothing; the list lives as long as the session.
        post_hook(
            &state,
            &id,
            "k",
            serde_json::json!({"hook_event_name": "Stop"}),
        )
        .await;
        let entry = session_entry(&state, &id).await;
        assert_eq!(entry["agent_state"], "finished");
        assert_eq!(
            entry["files_touched"],
            serde_json::json!(["/w/b.rs", "/w/c.rs", "/w/d.ipynb", "/w/a.rs"])
        );

        // The cap keeps the newest 100, oldest dropped first.
        for i in 0..105 {
            post_hook(
                &state,
                &id,
                "k",
                touch_payload("Write", "file_path", &format!("/w/f{i}.rs")),
            )
            .await;
        }
        let entry = session_entry(&state, &id).await;
        let touched = entry["files_touched"].as_array().unwrap();
        assert_eq!(touched.len(), 100);
        // 4 pre-existing + 105 new = 109; the 9 oldest fell off.
        assert_eq!(touched.first().unwrap(), "/w/f5.rs");
        assert_eq!(touched.last().unwrap(), "/w/f104.rs");

        state.sessions.kill(&id).ok();
    }

    #[tokio::test]
    async fn ws_events_pushes_files_touched_changes() {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let state = test_state();
        let id = inject_agent(&state, "k");

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

        // Settings frame first (contract), then the initial snapshot: the
        // agent session with an empty touched list.
        let settings = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            other => panic!("expected settings text frame, got {other:?}"),
        };
        assert_eq!(settings["type"], "settings");
        let snapshot = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            other => panic!("expected sessions text frame, got {other:?}"),
        };
        let entry = snapshot["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .expect("agent session in snapshot");
        assert_eq!(entry["files_touched"], serde_json::json!([]));

        // A file touch nudges the bus: a fresh snapshot carries the path.
        let status = post_hook(
            &state,
            &id,
            "k",
            touch_payload("Write", "file_path", "/w/touched.rs"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no snapshot with the touched file"
            );
            let frame = match next_ws_frame(&mut socket).await {
                WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                _ => continue,
            };
            // settings/git frames interleave on this bus; only sessions matter here.
            if frame["type"] != "sessions" {
                continue;
            }
            let done = frame["sessions"].as_array().unwrap().iter().any(|s| {
                s["id"] == id && s["files_touched"] == serde_json::json!(["/w/touched.rs"])
            });
            if done {
                break;
            }
        }

        state.sessions.kill(&id).ok();
    }

    /// End-to-end against a REAL repo: status must resolve the branch and
    /// classify a modified tracked file vs an untracked one. A directory that
    /// is not a repo answers `{"repo":false}` rather than failing.
    #[tokio::test]
    async fn git_status_reads_a_real_repo() {
        let repo = test_dir("gitrepo");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(&repo)
                // Hermetic: never read the developer's global/system config.
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
        std::fs::write(repo.join("tracked.txt"), "one\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-qm", "init"]);
        // A tracked file modified in the worktree, plus a brand-new file.
        std::fs::write(repo.join("tracked.txt"), "two\n").unwrap();
        std::fs::write(repo.join("new.txt"), "hi\n").unwrap();

        let state = test_state();
        let (status, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": repo.to_string_lossy()})),
        )
        .await;
        assert!(status.is_success(), "workspace create failed: {ws}");
        let ws_id = ws["id"].as_str().unwrap().to_string();

        let (status, body) = request(
            &state,
            Method::GET,
            &format!("/api/v1/git/status?workspace_id={ws_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["repo"], true);
        assert_eq!(body["branch"], "main");
        assert_eq!(body["detached"], false);

        let entries = body["entries"].as_array().unwrap();
        let modified = entries
            .iter()
            .find(|e| e["rel"] == "tracked.txt")
            .expect("modified file present");
        assert_eq!(modified["unstaged"], true);
        assert_eq!(modified["staged"], false);
        assert_eq!(modified["untracked"], false);
        assert_eq!(modified["y"], "M");

        let untracked = entries
            .iter()
            .find(|e| e["rel"] == "new.txt")
            .expect("untracked file present");
        assert_eq!(untracked["untracked"], true);
        assert_eq!(body["counts"]["untracked"], 1);
        assert_eq!(body["counts"]["unstaged"], 1);

        // The diff endpoint returns both blob sides for the modified file.
        let path = repo.join("tracked.txt");
        let (status, diff) = request(
            &state,
            Method::GET,
            &format!(
                "/api/v1/git/diff?workspace_id={ws_id}&path={}&mode=unstaged",
                urlencode(&path.to_string_lossy())
            ),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(diff["binary"], false);
        assert_eq!(diff["a"], "one\n");
        assert_eq!(diff["b"], "two\n");
    }

    /// A workspace opened AT A LINKED WORKTREE — how Chimaera itself is
    /// developed, so the common case, not the edge. Status must resolve that
    /// worktree's own branch, and the worktree list must see the whole repo.
    #[tokio::test]
    async fn git_worktrees_from_a_linked_worktree() {
        let repo = test_dir("gitwtrepo");
        let linked = test_dir("gitwtlinked").join("linked");
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
        git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/x",
            &linked.to_string_lossy(),
        ]);

        // The workspace is the LINKED worktree, not the main checkout.
        let state = test_state();
        let (status, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": linked.to_string_lossy()})),
        )
        .await;
        assert!(status.is_success(), "workspace create failed: {ws}");
        let ws_id = ws["id"].as_str().unwrap().to_string();

        // Status reports the LINKED worktree's branch, not main's.
        let (status, body) = request(
            &state,
            Method::GET,
            &format!("/api/v1/git/status?workspace_id={ws_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["repo"], true);
        assert_eq!(body["branch"], "feat/x");

        // The worktree list sees the whole repo, and marks OUR worktree current.
        let (status, body) = request(
            &state,
            Method::GET,
            &format!("/api/v1/git/worktrees?workspace_id={ws_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let list = body["worktrees"].as_array().unwrap();
        assert_eq!(list.len(), 2, "main + linked");
        let branches: Vec<&str> = list.iter().filter_map(|w| w["branch"].as_str()).collect();
        assert!(branches.contains(&"main"));
        assert!(branches.contains(&"feat/x"));
        let current: Vec<&str> = list
            .iter()
            .filter(|w| w["current"] == true)
            .filter_map(|w| w["branch"].as_str())
            .collect();
        assert_eq!(current, vec!["feat/x"], "the opened worktree is current");
    }

    /// Create a temp repo with one commit; returns (repo dir, git runner).
    fn init_temp_repo(label: &str) -> PathBuf {
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

    /// Worktree CREATE: lands under the managed root, checks out the new branch,
    /// and registers a workspace so the branch is immediately openable.
    #[tokio::test]
    async fn git_worktree_create_is_managed_and_registered() {
        let repo = init_temp_repo("wtcreate");
        let state = test_state();
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": repo.to_string_lossy()})),
        )
        .await;
        let ws_id = ws["id"].as_str().unwrap().to_string();

        // A name git rejects never reaches `worktree add`.
        let (status, _) = request(
            &state,
            Method::POST,
            "/api/v1/git/worktrees",
            Some(serde_json::json!({"workspace_id": ws_id, "branch": "bad..name"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "invalid branch rejected");

        let (status, body) = request(
            &state,
            Method::POST,
            "/api/v1/git/worktrees",
            Some(serde_json::json!({"workspace_id": ws_id, "branch": "feat/x"})),
        )
        .await;
        assert!(status.is_success(), "create failed: {body}");
        let wt_path = PathBuf::from(body["worktree"]["path"].as_str().unwrap());
        assert_eq!(body["worktree"]["branch"], "feat/x");
        assert!(wt_path.join("a.txt").exists(), "worktree is checked out");
        // Confined to the managed root.
        let managed = std::fs::canonicalize(&state.worktrees_root).unwrap();
        assert!(
            wt_path.starts_with(&managed),
            "{wt_path:?} under {managed:?}"
        );
        // And registered as a workspace you can open.
        let new_ws = body["workspace"]["id"].as_str().unwrap();
        assert!(crate::lock(&state.workspaces).get(new_ws).is_some());

        // The list marks it managed (the UI only offers remove where the daemon allows it).
        let (_, list) = request(
            &state,
            Method::GET,
            &format!("/api/v1/git/worktrees?workspace_id={ws_id}"),
            None,
        )
        .await;
        let entry = list["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["branch"] == "feat/x")
            .expect("new worktree listed");
        assert_eq!(entry["managed"], true);
        let main_entry = list["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["branch"] == "main")
            .unwrap();
        assert_eq!(
            main_entry["managed"], false,
            "the user's checkout is not ours"
        );

        // A second create for the same branch collides rather than clobbering.
        let (status, _) = request(
            &state,
            Method::POST,
            "/api/v1/git/worktrees",
            Some(serde_json::json!({"workspace_id": ws_id, "branch": "feat/x"})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    /// Worktree REMOVE is destructive, so every fence gets a test: unmanaged
    /// paths are refused outright, uncommitted work blocks it, and only then
    /// does it delete (leaving the branch, and unregistering the workspace).
    #[tokio::test]
    async fn git_worktree_remove_is_fenced() {
        let repo = init_temp_repo("wtremove");
        let state = test_state();
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": repo.to_string_lossy()})),
        )
        .await;
        let ws_id = ws["id"].as_str().unwrap().to_string();
        let (_, body) = request(
            &state,
            Method::POST,
            "/api/v1/git/worktrees",
            Some(serde_json::json!({"workspace_id": ws_id, "branch": "feat/y"})),
        )
        .await;
        let wt_path = body["worktree"]["path"].as_str().unwrap().to_string();
        let new_ws = body["workspace"]["id"].as_str().unwrap().to_string();

        // Fence 1: a checkout chimaera did not create is never removed — even
        // though it IS a real worktree of this repo.
        let (status, err) = request(
            &state,
            Method::DELETE,
            "/api/v1/git/worktrees",
            Some(serde_json::json!({"workspace_id": ws_id, "path": repo.to_string_lossy()})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{err}");
        assert!(repo.join("a.txt").exists(), "the user's checkout survived");

        // Fence 4: uncommitted work blocks removal.
        std::fs::write(PathBuf::from(&wt_path).join("scratch.txt"), "wip\n").unwrap();
        let (status, err) = request(
            &state,
            Method::DELETE,
            "/api/v1/git/worktrees",
            Some(serde_json::json!({"workspace_id": ws_id, "path": wt_path})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{err}");
        assert!(PathBuf::from(&wt_path).exists(), "dirty worktree survived");

        // Clean it, and the removal goes through.
        std::fs::remove_file(PathBuf::from(&wt_path).join("scratch.txt")).unwrap();
        let (status, err) = request(
            &state,
            Method::DELETE,
            "/api/v1/git/worktrees",
            Some(serde_json::json!({"workspace_id": ws_id, "path": wt_path})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{err}");
        assert!(!PathBuf::from(&wt_path).exists(), "worktree dir removed");
        assert!(
            crate::lock(&state.workspaces).get(&new_ws).is_none(),
            "its workspace registration is gone"
        );
        // The branch itself is untouched — removing a worktree is not rm -rf history.
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "--verify", "--quiet", "refs/heads/feat/y"])
            .output()
            .unwrap();
        assert!(out.status.success(), "branch feat/y still exists");
    }

    /// Fence 3: a live session inside a managed worktree blocks removal — pulling
    /// the directory out from under someone's shell is never acceptable.
    #[tokio::test]
    async fn git_worktree_remove_refuses_with_a_live_session_inside() {
        let repo = init_temp_repo("wtsession");
        let state = test_state();
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": repo.to_string_lossy()})),
        )
        .await;
        let ws_id = ws["id"].as_str().unwrap().to_string();
        let (_, body) = request(
            &state,
            Method::POST,
            "/api/v1/git/worktrees",
            Some(serde_json::json!({"workspace_id": ws_id, "branch": "feat/z"})),
        )
        .await;
        let wt_path = body["worktree"]["path"].as_str().unwrap().to_string();
        let new_ws = body["workspace"]["id"].as_str().unwrap().to_string();

        // A shell living in the new worktree.
        let (status, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({"workspace_id": new_ws, "kind": "shell"})),
        )
        .await;
        assert!(status.is_success(), "spawn failed: {session}");
        let sid = session["id"].as_str().unwrap().to_string();

        let (status, err) = request(
            &state,
            Method::DELETE,
            "/api/v1/git/worktrees",
            Some(serde_json::json!({"workspace_id": ws_id, "path": wt_path, "force": true})),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "even force must not evict a session"
        );
        assert!(err["error"].as_str().unwrap().contains("live session"));
        assert!(PathBuf::from(&wt_path).exists());

        state.sessions.kill(&sid).ok();
    }

    #[tokio::test]
    async fn git_status_on_a_non_repo_says_so() {
        let plain = test_dir("notarepo");
        let state = test_state();
        let (status, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": plain.to_string_lossy()})),
        )
        .await;
        assert!(status.is_success(), "workspace create failed: {ws}");
        let ws_id = ws["id"].as_str().unwrap();
        let (status, body) = request(
            &state,
            Method::GET,
            &format!("/api/v1/git/status?workspace_id={ws_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["repo"], false);
    }

    /// Percent-encode a path for a query string (tests only).
    fn urlencode(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
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

        // Settings frame first, then the initial full sessions snapshot.
        let settings = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            other => panic!("expected settings text frame, got {other:?}"),
        };
        assert_eq!(settings["type"], "settings");
        assert!(settings["settings"].is_object());
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
            // settings/git frames interleave on this bus; only sessions matter here.
            if frame["type"] != "sessions" {
                continue;
            }
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
            // settings/git frames interleave on this bus; only sessions matter here.
            if frame["type"] != "sessions" {
                continue;
            }
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
    async fn settings_round_trip_and_validation() {
        let state = test_state();

        // Fresh daemon: empty settings object.
        let (status, body) = request(&state, Method::GET, "/api/v1/settings", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"settings": {}}));

        // PUT stores the sparse map verbatim (unknown keys preserved).
        let map = serde_json::json!({
            "terminal.fontSize": 15,
            "appearance.theme": "dark",
            "future.unknownKey": [1, 2, 3],
        });
        let (status, _) = request(&state, Method::PUT, "/api/v1/settings", Some(map.clone())).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, body) = request(&state, Method::GET, "/api/v1/settings", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["settings"], map);

        // Non-object bodies are rejected.
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/settings",
            Some(serde_json::json!([1, 2])),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Daemon-consumed keys parse with clamping; garbage yields None.
        let (_, _) = request(
            &state,
            Method::PUT,
            "/api/v1/settings",
            Some(serde_json::json!({
                "daemon.scrollbackLines": 50,
                "quickOpen.ignoreDirs": ["node_modules", "", "a/b", ".git"],
            })),
        )
        .await;
        assert_eq!(lock(&state.settings).scrollback_lines(), Some(200));
        assert_eq!(
            lock(&state.settings).quickopen_ignore_dirs(),
            Some(vec!["node_modules".to_string(), ".git".to_string()]),
        );
        let (_, _) = request(
            &state,
            Method::PUT,
            "/api/v1/settings",
            Some(serde_json::json!({"daemon.scrollbackLines": "lots"})),
        )
        .await;
        assert_eq!(lock(&state.settings).scrollback_lines(), None);
    }

    #[tokio::test]
    async fn settings_hand_edit_on_disk_is_picked_up() {
        let data_dir = test_dir("settings-disk");
        let state = test_state_with_data_dir(0, data_dir.clone());

        // Simulate `vim ~/.config/chimaera/settings.json`.
        let path = data_dir.join("config").join("settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"terminal.fontSize": 16}"#).unwrap();
        // Filesystem mtime granularity can swallow same-instant rewrites in
        // this synthetic test; force the stat cache stale. Real hand-edits
        // happen seconds apart and are caught by the mtime check alone.
        lock(&state.settings).force_stale_for_tests();

        let (status, body) = request(&state, Method::GET, "/api/v1/settings", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["settings"]["terminal.fontSize"], 16);
    }

    #[tokio::test]
    async fn ws_events_pushes_settings_changes() {
        use futures::SinkExt;
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
                serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
            ))
            .await
            .unwrap();

        // Initial settings frame (empty map on a fresh daemon).
        let settings = match next_ws_frame(&mut socket).await {
            WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            other => panic!("expected settings text frame, got {other:?}"),
        };
        assert_eq!(settings["type"], "settings");
        assert_eq!(settings["settings"], serde_json::json!({}));

        // A PUT wakes the bus with the fresh map.
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/settings",
            Some(serde_json::json!({"appearance.theme": "dark"})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no settings frame after PUT"
            );
            let frame = match next_ws_frame(&mut socket).await {
                WsMessage::Text(text) => serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                _ => continue,
            };
            if frame["type"] == "settings" {
                assert_eq!(frame["settings"]["appearance.theme"], "dark");
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
        // The ticket mint and the link-provider validation are POSTs and
        // equally protected.
        for (uri, body) in [
            ("/api/v1/fs/ticket", r#"{"path":"/etc/hosts"}"#),
            (
                "/api/v1/fs/validate",
                r#"{"candidates":["hosts"],"base":"/etc"}"#,
            ),
        ] {
            let res = app(test_state())
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(uri)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
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

    #[tokio::test]
    async fn fs_validate_resolves_relative_absolute_missing_and_dirs() {
        let state = test_state();
        let base = test_dir("validate-base");
        std::fs::create_dir(base.join("sub")).unwrap();
        std::fs::write(base.join("sub").join("real.txt"), "x").unwrap();
        std::fs::write(base.join("top.rs"), "x").unwrap();
        // The base may itself be uncanonical (macOS /var -> /private/var);
        // resolved paths in the answer are always canonical.
        let canon = std::fs::canonicalize(&base).unwrap();
        let abs = canon.join("top.rs").to_string_lossy().into_owned();

        let (status, body) = request(
            &state,
            Method::POST,
            "/api/v1/fs/validate",
            Some(serde_json::json!({
                "candidates": [
                    "sub/real.txt",     // relative file
                    "sub",              // relative dir
                    abs,                // absolute file
                    "missing.txt",      // nonexistent -> absent
                    "./sub/../top.rs",  // dot segments resolve away
                    "",                 // empty -> absent
                ],
                "base": base.to_string_lossy(),
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let valid = body["valid"].as_object().unwrap();
        assert_eq!(valid.len(), 4, "{body}");
        assert_eq!(
            valid["sub/real.txt"]["path"],
            serde_json::json!(canon.join("sub").join("real.txt").to_string_lossy())
        );
        assert_eq!(valid["sub/real.txt"]["kind"], "file");
        assert_eq!(
            valid["sub"]["path"],
            serde_json::json!(canon.join("sub").to_string_lossy())
        );
        assert_eq!(valid["sub"]["kind"], "dir");
        assert_eq!(valid[&abs]["path"], serde_json::json!(abs));
        assert_eq!(valid[&abs]["kind"], "file");
        assert_eq!(valid["./sub/../top.rs"]["path"], serde_json::json!(abs));
        assert!(!valid.contains_key("missing.txt"), "{body}");
    }

    #[tokio::test]
    async fn fs_validate_caps_candidates_and_rejects_relative_base() {
        let state = test_state();
        let base = test_dir("validate-cap");
        std::fs::write(base.join("real.txt"), "x").unwrap();

        // Candidates past the 50 cap are ignored, even valid ones.
        let mut candidates: Vec<String> = (0..50).map(|i| format!("nope-{i}")).collect();
        candidates.push("real.txt".to_string());
        let (status, body) = request(
            &state,
            Method::POST,
            "/api/v1/fs/validate",
            Some(serde_json::json!({
                "candidates": candidates,
                "base": base.to_string_lossy(),
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["valid"].as_object().unwrap().is_empty(), "{body}");

        // A non-absolute base is a 400 (candidates would resolve nowhere).
        let (status, body) = request(
            &state,
            Method::POST,
            "/api/v1/fs/validate",
            Some(serde_json::json!({
                "candidates": ["real.txt"],
                "base": "relative/base",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].is_string(), "{body}");
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

    /// The stateful-restart contract end to end: a live shell recorded in
    /// the ledger comes back UNDER THE SAME SESSION ID after a "restart"
    /// (a second AppState over the same data dir) — at its cwd, with its
    /// pinned name and theme — while a non-resumable agent entry retires
    /// into the workspace's recents instead of vanishing.
    #[tokio::test]
    async fn ledger_resurrects_sessions_across_restart() {
        let data = test_dir("ledger-restart");
        let state = test_state_with_data_dir(0, data.clone());
        // Canonicalized like create_workspace canonicalizes it (macOS /var
        // is a symlink to /private/var).
        let root = std::fs::canonicalize(test_dir("ledger-restart-root")).unwrap();
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let workspace_id = ws["id"].as_str().unwrap().to_string();

        let (status, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({
                "workspace_id": workspace_id,
                "name": "data wrangling",
                "theme": "light",
                "cols": 132,
                "rows": 43,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "spawn failed: {session}");
        let sid = session["id"].as_str().unwrap().to_string();

        // Persist the ledger the way the reconcile loop would, then kill the
        // PTY: the "daemon" is going down, taking its children with it.
        let (entries, links) = ledger::snapshot(&state);
        assert_eq!(entries.len(), 1, "the live shell is in the ledger");
        lock(&state.ledger).write_if_changed(&entries, &links);
        state.sessions.kill(&sid).ok();

        // "Restart": a fresh AppState over the same data dir. The boot
        // ledger carries the shell — plus an agent entry we add by hand (a
        // codex conversation the old daemon was running), which cannot
        // resurrect and must retire into recents.
        let state2 = test_state_with_data_dir(0, data);
        let mut boot = lock(&state2.ledger).load_boot();
        assert_eq!(boot.sessions.len(), 1);
        boot.sessions.push(ledger::LedgerEntry {
            id: "s-dead-codex".to_string(),
            workspace_id: workspace_id.clone(),
            cwd: root.clone(),
            pinned_name: None,
            cols: 80,
            rows: 24,
            theme: "dark".to_string(),
            agent: Some(ledger::LedgerAgent {
                kind: agents::AgentKind::Codex,
                resume: None,
                transcript: None,
                title: "port the parser".to_string(),
            }),
        });
        ledger::restore(&state2, boot).await;

        // The shell is back under ITS OLD ID — that identity is what lets
        // every persisted layout tab rebind without migration.
        let infos = state2.sessions.list();
        assert_eq!(infos.len(), 1, "exactly the shell respawned");
        let info = &infos[0];
        assert_eq!(info.id, sid, "session id survives the restart");
        assert_eq!(info.cwd, root);
        assert_eq!(info.name, "data wrangling");
        assert!(info.renamed, "the pinned name stays pinned");
        assert_eq!((info.cols, info.rows), (132, 43));
        assert_eq!(
            lock(&state2.session_workspaces).get(&sid),
            Some(&workspace_id)
        );
        assert_eq!(
            lock(&state2.session_themes).get(&sid).map(String::as_str),
            Some("light"),
            "the spawn theme carries across"
        );

        // The codex conversation retired into recents (resumable rows are
        // the statefulness story for agents that cannot resurrect).
        let recents = lock(&state2.recents).list(&workspace_id);
        assert_eq!(recents.len(), 1);
        assert_eq!(recents[0].title, "port the parser");
        assert_eq!(recents[0].kind, agents::AgentKind::Codex);
        assert!(
            state2
                .recents_epoch
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0,
            "the recents epoch moved so the rail refetches"
        );

        state2.sessions.kill(&sid).ok();
    }

    /// Restore is opt-out: with `daemon.restoreSessions` false the shell is
    /// dropped, but agent conversations still retire into recents — turning
    /// restore off must never make history vanish.
    #[tokio::test]
    async fn ledger_restore_disabled_still_lands_recents() {
        let data = test_dir("ledger-optout");
        let state = test_state_with_data_dir(0, data.clone());
        let root = test_dir("ledger-optout-root");
        let (_, ws) = request(
            &state,
            Method::POST,
            "/api/v1/workspaces",
            Some(serde_json::json!({"root": root.to_string_lossy()})),
        )
        .await;
        let workspace_id = ws["id"].as_str().unwrap().to_string();
        let (status, _) = request(
            &state,
            Method::PUT,
            "/api/v1/settings",
            Some(serde_json::json!({"daemon.restoreSessions": false})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // The conversation's transcript exists (hook-recorded path), so its
        // recents row must stay resumable through retirement.
        let transcript = data.join("conv-1.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let boot = ledger::BootLedger {
            sessions: vec![
                ledger::LedgerEntry {
                    id: "s-shell".to_string(),
                    workspace_id: workspace_id.clone(),
                    cwd: root.clone(),
                    pinned_name: None,
                    cols: 80,
                    rows: 24,
                    theme: "dark".to_string(),
                    agent: None,
                },
                ledger::LedgerEntry {
                    id: "s-claude".to_string(),
                    workspace_id: workspace_id.clone(),
                    cwd: root.clone(),
                    pinned_name: None,
                    cols: 80,
                    rows: 24,
                    theme: "dark".to_string(),
                    agent: Some(ledger::LedgerAgent {
                        kind: agents::AgentKind::Claude,
                        resume: Some("conv-1".to_string()),
                        transcript: Some(transcript),
                        title: "fix the flaky tests".to_string(),
                    }),
                },
            ],
            links: std::collections::HashMap::new(),
            written_at: 1_750_000_000,
        };
        ledger::restore(&state, boot).await;

        assert!(state.sessions.list().is_empty(), "nothing respawns");
        let recents = lock(&state.recents).list(&workspace_id);
        assert_eq!(recents.len(), 1, "the conversation is still findable");
        assert_eq!(recents[0].title, "fix the flaky tests");
        assert_eq!(recents[0].resume.as_deref(), Some("conv-1"));
        assert_eq!(recents[0].last_active, 1_750_000_000);
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
