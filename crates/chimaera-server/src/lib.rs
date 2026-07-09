mod agent_state;
mod agents;
mod api;
mod assets;
mod chat;
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
mod session_view;
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
    /// Owner of all structured chat sessions (Tier B agent drivers).
    pub(crate) chat: Arc<chimaera_agent::ChatManager>,
    /// The chat manager's hook signals; `chat::spawn_signal_task` (called
    /// from `app()`) takes and consumes this for the daemon's lifetime.
    pub(crate) chat_signals: Mutex<Option<tokio::sync::mpsc::Receiver<chat::ChatSignal>>>,
    /// Respawn ingredients per chat session, for the degrade-to-PTY path.
    pub(crate) chat_recipes: Mutex<HashMap<String, chat::ChatRecipe>>,
    /// Sessions mid view-switch (id -> target ui "chat"|"term"): their
    /// intentional process deaths must not retire records or trigger the
    /// degrade path, and `sessions_json` synthesizes a placeholder row for
    /// the moment the id is in neither registry — a vanishing row would make
    /// every window prune the session's tabs mid-toggle.
    pub(crate) chat_switching: Mutex<HashMap<String, String>>,
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
        let (chat, chat_signals_rx) = chat::new_manager(data_dir.join("chat"));
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
            chat,
            chat_signals: Mutex::new(Some(chat_signals_rx)),
            chat_recipes: Mutex::new(HashMap::new()),
            chat_switching: Mutex::new(HashMap::new()),
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
    // Consumes the chat manager's hook signals for the daemon's lifetime
    // (no-op when already running — tests may build several routers).
    chat::spawn_signal_task(state.clone());
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
        .route("/sessions/{id}/view", post(chat::switch_view))
        .route("/sessions/{id}/rewind", post(chat::rewind_session))
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
        .route("/fs/mkdir", post(fs::mkdir))
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
        .route("/ws/chat/{id}", get(ws::chat_ws))
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

    // Chat sessions are daemon-owned drivers that die with this process, and
    // the ledger does not yet resurrect them (sv-11: real resurrection is a
    // follow-up in `ledger` — snapshot()/restore() cover only PTY sessions).
    // So at a graceful stop (update / restart), retire their conversations
    // into Recents here, so a survivor is offered for manual resume instead of
    // vanishing. Idempotent: a session already retired by an in-band
    // `close-all`/`shutdown` has no AgentRecord left, and `retire` no-ops.
    for info in state.chat.list() {
        recents::retire(&state, &info.id, None, None);
    }

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
mod tests;
