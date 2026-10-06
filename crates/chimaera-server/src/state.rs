use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::{
    agent_probe, agent_updates, agents, chat, comms, compute, environment, episodes, fs, git,
    knowledge, launcher, ledger, plugins, proxy, quickopen, recents, settings, timeline, update,
    view_state, workspaces,
};

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
    /// Conversations the user archived out of Recents, per workspace
    /// (`recents-archive.json`; see `recents_archive`). Hidden, never deleted.
    pub(crate) recents_archive: Mutex<crate::recents_archive::ArchiveStore>,
    /// Serializes the archive's file writes (each snapshots under it).
    pub(crate) recents_archive_write: tokio::sync::Mutex<()>,
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
    /// The newest known upstream release per agent CLI (see `agent_updates`):
    /// filled by its slow checker and Settings' inline `?check=true`, read by
    /// the GET /agents row builder. Bounded: one entry per known agent.
    pub(crate) agent_updates: Mutex<HashMap<agents::AgentKind, agent_updates::AgentLatest>>,
    /// Bumped when the update status changes; drives the `update` ws frame.
    pub(crate) update_epoch: std::sync::atomic::AtomicU64,
    /// Serializes release checks (the periodic one and any "check now"), so
    /// concurrent asks share one fetch — see `update::check_now`.
    pub(crate) update_check: tokio::sync::Mutex<()>,
    /// User settings (the settings.json ground truth), stored in the config
    /// dir; mtime-checked on read so hand-edits surface without a restart.
    pub(crate) settings: Mutex<settings::SettingsStore>,
    /// Environment preludes (`env-profiles.json`, config dir): startup
    /// commands concatenated host ⊕ workspace ⊕ launch into each spawn's
    /// `CHIMAERA_PRELUDE` file. Same hand-edit story as settings.
    pub(crate) env_preludes: Mutex<environment::EnvPreludeStore>,
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
    /// Durable public imports gate execution before boot ledger restoration.
    pub(crate) bundle_imports: Lazy<crate::bundle::PendingImports>,
    /// Workspaces with a Mastermind PUT/DELETE in flight. The routes are
    /// multi-step (retire old → bind → spawn, with rollback); two racing
    /// callers would leak the loser's spawned session and could clobber the
    /// winner's binding on rollback — so per workspace, one change at a time
    /// (the `chat_switching` idiom).
    pub(crate) mastermind_switching: Mutex<std::collections::HashSet<String>>,
    /// workspace id -> Mastermind spawns in flight (reserved but not yet in a
    /// registry). The spawn ceiling is check-then-act across real awaits
    /// (detect + file IO + process spawn), so parallel tool calls must
    /// count-and-reserve under one lock or they all pass the check — the
    /// wall would be advisory exactly for the runaway fan-out it exists to
    /// stop (`mcp::SpawnReservation`).
    pub(crate) spawn_reservations: Mutex<HashMap<String, usize>>,
    /// session id -> workspace id.
    pub(crate) session_workspaces: Mutex<HashMap<String, String>>,
    pub(crate) activity: Mutex<crate::activity::Activity>,
    /// Pro's state, read from disk only when the Pro policy first uses it:
    /// a daemon without the extension never touches it.
    pub(crate) pro: Lazy<crate::pro::ProState>,
    /// The workspace-admission hook (`policy`); the inert default unless a
    /// composition installs one at startup.
    pub(crate) policy: std::sync::OnceLock<Arc<dyn crate::policy::WorkspacePolicy>>,
    pub(crate) daemon_extension: Option<Arc<dyn crate::daemon_extension::Runtime>>,
    pub(crate) cloud_providers: crate::cloud::providers::ProviderSlot,
    pub(crate) deferred_sessions: Mutex<HashMap<String, crate::ledger::LedgerEntry>>,
    /// session id -> the turn its resumers take (`ledger::resume_one`).
    pub(crate) resuming: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub(crate) session_proxy: crate::session_proxy::Store,
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
    /// Browser-pane proxy sessions (/proxy/{id} targets; in-memory only —
    /// panes re-mint transparently after a restart). See `proxy`.
    pub(crate) proxies: Mutex<proxy::ProxyStore>,
    /// Per-workspace quick-open index: served stale-while-revalidating,
    /// single-flighted per workspace, dropped when idle or deleted. See
    /// `quickopen`.
    pub(crate) quickopen: Mutex<quickopen::QuickOpenCache>,
    /// Read-only git service (status/diff): discovery cache, per-workspace nudge
    /// epochs, and a bounded pool for `git` child processes. Never persisted.
    pub(crate) git: git::GitService,
    /// Compute-scheduler awareness (Slurm detection + the user's queue),
    /// cached + single-flight; empty-tagged on a laptop. Never persisted.
    pub(crate) compute: compute::ComputeService,
    /// Signalled whenever the session list / agent state / titles change;
    /// wakes /ws/events subscribers (a 1s tick catches anything missed).
    /// Every wake stamps a change generation — the shared sessions-snapshot
    /// cache's key, so N connected windows cost one snapshot build per
    /// change instead of one each.
    pub(crate) changes: ChangeBus,
    /// The `/ws/events` sessions-frame cache (see `session_view`): built once
    /// per change generation and fanned out to every connected window.
    pub(crate) sessions_snapshot: crate::session_view::SnapshotCache,
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
    /// Set once the shutdown signal has fired (any source), so long-held
    /// requests — the notices long-poll — return instead of stalling the
    /// graceful drain.
    pub(crate) stopping: std::sync::atomic::AtomicBool,
    /// The notice feed (agent finished / needs you / agent `notify`).
    pub(crate) notices: crate::notices::Notices,
    /// Agent `open_browser` requests on their way to the windows, plus how
    /// many `/ws/events` consumers are connected (see `browser_open`).
    pub(crate) browser_opens: crate::browser_open::BrowserOpens,
    /// Agent binaries resolved via the login shell (with `--version`),
    /// cached per agent for the daemon's lifetime;
    /// `GET /api/v1/agents?refresh=true` bypasses and refills it.
    pub(crate) agent_bins: Mutex<HashMap<agents::AgentKind, launcher::AgentDetection>>,
    /// Root of Claude Code's per-project transcript store, normally
    /// `~/.claude/projects`; tests point it at a fixture dir.
    pub(crate) claude_projects_dir: PathBuf,
    /// Managed-runtime prefix (`~/.chimaera/agents`): curated installs land
    /// in `<agent>/<version>/bin/` here, activated via per-agent symlinks
    /// in `bin/`. Shared by cluster workspaces; explicit/dev homes stay isolated.
    pub(crate) managed_root: PathBuf,
    /// Read fallback for installs made by older workspace-scoped daemons.
    pub(crate) legacy_managed_root: Option<PathBuf>,
    /// Managed-worktree prefix (`~/.chimaera/worktrees/<repo>/<branch>`).
    /// Chimaera creates worktrees ONLY here, and removes ONLY what is under
    /// here — the containment check is what keeps `worktree remove` from ever
    /// touching the user's own checkouts. Derived from the data dir, so an
    /// isolated `CHIMAERA_HOME` (and every test) is sandboxed for free.
    pub(crate) worktrees_root: PathBuf,
    /// Theming-shim dir (`~/.chimaera/shims`), prepended to every session's
    /// PATH via spawn env only — user dotfiles are never touched.
    pub(crate) shims_dir: PathBuf,
    /// Per-session upload landing pad (`~/.chimaera/uploads/<session-id>/`):
    /// OS-desktop drops and pasted screenshots stream here so their PATHS can
    /// be referenced in prompts/shells. Under the data dir (not runtime_dir):
    /// uploads must live as long as their session, and runtime tmp gets
    /// night-scrubbed on HPC. Size-capped per file and per session (`upload`),
    /// pruned when the session ends and at boot.
    pub(crate) uploads_root: PathBuf,
    /// Draft mirror (`~/.chimaera/drafts/`): unsaved editor text mirrored by
    /// the client so a window on another origin can recover it. Capped and
    /// evicted in `drafts`.
    pub(crate) drafts_root: PathBuf,
    /// Paths an agent or a save just wrote (`git::mark_path_dirty`). Every
    /// `/ws/events` client subscribes and re-stats the ones it watches right
    /// away instead of on its next poll. Bounded; a lagging receiver just
    /// falls back to that poll. See `fs_watch::TOUCHED_CAPACITY`.
    pub(crate) fs_touched: tokio::sync::broadcast::Sender<crate::fs_watch::Touched>,
    /// Live install sessions, one per agent (POST /agents/{id}/install
    /// answers 409 while one runs): session id + reservation time. The id
    /// registers in `SessionManager` only after spawn, so a reservation
    /// younger than `runtimes::INSTALL_RESERVATION_GRACE` is busy even with
    /// no visible session. Cleaned up by the install watcher.
    pub(crate) installs: Mutex<HashMap<agents::AgentKind, (String, Instant)>>,
    /// Exact live owner through preparation and cleanup, bounded by the agent
    /// catalog. A grace timer cannot reclaim an owner whose child is not visible.
    pub(crate) install_owners: Mutex<HashMap<agents::AgentKind, String>>,
    /// Latest installer result per built-in agent; bounded by the catalog.
    pub(crate) install_results: Mutex<HashMap<agents::AgentKind, crate::runtimes::InstallResult>>,
    pub(crate) agent_setup: Mutex<HashMap<agents::AgentKind, Arc<crate::agent_setup::Operation>>>,
    /// The user's own Claude Code settings file (`~/.claude/settings.json`);
    /// an explicit theme there suppresses chimaera's theme injection. Tests
    /// point it at a fixture.
    pub(crate) claude_settings_path: PathBuf,
    /// The user's codex config (`$CODEX_HOME/config.toml`, default
    /// `~/.codex`); same respect rule.
    pub(crate) codex_config_path: PathBuf,
    /// The per-workspace Timeline (`<data_dir>/workspace/<ws>/timeline.jsonl`):
    /// what happened, written from signals the daemon already receives. Its
    /// per-workspace epochs drive the `/ws/events` timeline frame.
    pub(crate) timeline: timeline::TimelineService,
    /// Session history (`<data_dir>/workspace/<ws>/sessions.jsonl`): one
    /// record per agent session, opened at start and closed at end; only
    /// the open ones live here. See `history`.
    pub(crate) history: crate::history::HistoryService,
    /// The plugin catalog (see `plugins::Catalog`): the embedded plugins
    /// merged with the installed copies under `<data_dir>/plugins`, reloaded
    /// after every install, update, rollback or remove.
    pub(crate) plugin_catalog: plugins::Catalog,
    /// Who approved what each plugin can do, the admin policy and the kill
    /// switch (see `plugins::trust`, `plugins::revoke`).
    pub(crate) plugin_guard: plugins::trust::Guard,
    /// Plugin release knowledge (see `plugins::releases`): the newer
    /// versions a check found, and the one-change-at-a-time lock. Hot state.
    pub(crate) plugin_releases: plugins::releases::Releases,
    /// Per-workspace plugin footprint detection (see `plugins`): refreshed
    /// off the reactor, read-only on the MCP hot path.
    pub(crate) plugin_detect: Mutex<plugins::DetectCache>,
    /// The plugin host's per-daemon half (see `plugins::runtime`): live
    /// WASM instances (≤ 64, idle-evicted), fault counts, what each plugin
    /// offers, the `emit` ring. Hot state; nothing persisted.
    pub(crate) plugin_runtime: plugins::runtime::PluginRuntime,
    /// What plugins keep per workspace through the host (`state-put`):
    /// 64 KiB per (plugin, workspace), in memory.
    pub(crate) plugin_state: Mutex<plugins::hostfns::PluginStates>,
    /// The plugin platform (see `plugins::platform::Platform`): output
    /// folders, durable plugin data, surfaces, screens, file events.
    pub(crate) plugin_platform: plugins::platform::Platform,
    /// Hook-driven turns of claude TUIs in flight (the Timeline's hooks
    /// tier; see `episodes`). Bounded by live sessions.
    pub(crate) tui_episodes: Mutex<episodes::TuiEpisodes>,
    /// Turn starts and ends waiting on their workspace's Knowledge check,
    /// one FIFO per workspace (see `episodes`).
    pub(crate) episode_queue: episodes::EpisodeQueue,
    /// The Timeline's Slurm job task is running (idempotent start: tests
    /// build several routers over one state).
    pub(crate) timeline_jobs_started: std::sync::atomic::AtomicBool,
    /// Cached answers from the agents' own CLIs (plugins, skills, hooks) —
    /// see `agent_probe`.
    pub(crate) probes: agent_probe::ProbeState,
    /// Live claude chat sessions' slash/skill catalogs (from their handshake
    /// Init), for the Skills view's "built into the agent" group. Bounded by
    /// live sessions; dropped on exit.
    pub(crate) chat_catalogs: Mutex<HashMap<String, Vec<(String, String)>>>,
    /// Agent communication (see `comms`): post rate windows (shared with
    /// plugins' Timeline appends), wake caps and requests, steers in flight
    /// (memory), and each workspace's read state (`comms.json`). Messages
    /// themselves live on the Timeline.
    pub(crate) comms: comms::Comms,
    /// Knowledge-provider cache, the Timeline's diff baseline, and who
    /// recorded what (see `knowledge`). Hot state; rebuilt from the files.
    pub(crate) knowledge: Mutex<knowledge::KnowledgeState>,
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
        let plugin_catalog = plugins::Catalog::load(data_dir.join("plugins"));
        let plugin_guard = plugins::trust::Guard::load(&plugin_catalog);
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
            recents_archive: Mutex::new(crate::recents_archive::ArchiveStore::load(
                data_dir.join("recents-archive.json"),
            )),
            recents_archive_write: tokio::sync::Mutex::new(()),
            recents_epoch: std::sync::atomic::AtomicU64::new(0),
            ledger: Mutex::new(ledger::LedgerStore::new(data_dir.join("sessions.json"))),
            session_themes: Mutex::new(HashMap::new()),
            update: Mutex::new(update::UpdateStatus::default()),
            update_epoch: std::sync::atomic::AtomicU64::new(0),
            update_check: tokio::sync::Mutex::new(()),
            agent_updates: Mutex::new(HashMap::new()),
            settings: Mutex::new(settings::SettingsStore::load(
                config_dir.join("settings.json"),
            )),
            env_preludes: Mutex::new(environment::EnvPreludeStore::load(
                config_dir.join("env-profiles.json"),
            )),
            sessions: chimaera_pty::SessionManager::new(),
            chat,
            chat_signals: Mutex::new(Some(chat_signals_rx)),
            chat_recipes: Mutex::new(HashMap::new()),
            chat_switching: Mutex::new(HashMap::new()),
            bundle_imports: Lazy::new({
                let data_dir = data_dir.clone();
                move || crate::bundle::PendingImports::load(&data_dir)
            }),
            mastermind_switching: Mutex::new(std::collections::HashSet::new()),
            spawn_reservations: Mutex::new(HashMap::new()),
            session_workspaces: Mutex::new(HashMap::new()),
            activity: Mutex::new(crate::activity::Activity::default()),
            pro: Lazy::new({
                let root = data_dir.join("pro");
                move || crate::pro::ProState::new(root)
            }),
            policy: std::sync::OnceLock::new(),
            daemon_extension: None,
            cloud_providers: crate::cloud::providers::ProviderSlot::default(),
            deferred_sessions: Mutex::new(HashMap::new()),
            resuming: Mutex::new(HashMap::new()),
            session_proxy: crate::session_proxy::Store::default(),
            agents: Mutex::new(HashMap::new()),
            display_names: Mutex::new(HashMap::new()),
            current_cwds: Mutex::new(HashMap::new()),
            exec_status: Mutex::new(HashMap::new()),
            links: Mutex::new(HashMap::new()),
            tickets: Mutex::new(fs::TicketStore::default()),
            proxies: Mutex::new(proxy::ProxyStore::default()),
            quickopen: Mutex::new(quickopen::QuickOpenCache::default()),
            git: git::GitService::new(),
            compute: compute::ComputeService::new(),
            changes: ChangeBus::new(),
            sessions_snapshot: crate::session_view::SnapshotCache::new(),
            restored: tokio::sync::watch::channel(true).0,
            shutdown: tokio::sync::Notify::new(),
            stopping: std::sync::atomic::AtomicBool::new(false),
            notices: crate::notices::Notices::new(),
            browser_opens: crate::browser_open::BrowserOpens::default(),
            agent_bins: Mutex::new(HashMap::new()),
            claude_projects_dir: home.join(".claude").join("projects"),
            managed_root: data_dir.join("agents"),
            legacy_managed_root: None,
            worktrees_root: data_dir.join("worktrees"),
            shims_dir: data_dir.join("shims"),
            uploads_root: data_dir.join("uploads"),
            drafts_root: data_dir.join("drafts"),
            fs_touched: tokio::sync::broadcast::channel(crate::fs_watch::TOUCHED_CAPACITY).0,
            installs: Mutex::new(HashMap::new()),
            install_owners: Mutex::new(HashMap::new()),
            install_results: Mutex::new(HashMap::new()),
            agent_setup: Mutex::new(HashMap::new()),
            claude_settings_path: home.join(".claude").join("settings.json"),
            codex_config_path: codex_home(&home, std::env::var_os("CODEX_HOME"))
                .join("config.toml"),
            timeline: timeline::TimelineService::new(data_dir.join("workspace")),
            history: crate::history::HistoryService::new(&data_dir),
            plugin_catalog,
            plugin_guard,
            plugin_releases: plugins::releases::Releases::default(),
            plugin_detect: Mutex::new(plugins::DetectCache::default()),
            plugin_runtime: plugins::runtime::PluginRuntime::default(),
            plugin_state: Mutex::new(plugins::hostfns::PluginStates::default()),
            plugin_platform: plugins::platform::Platform::new(&data_dir),
            tui_episodes: Mutex::new(episodes::TuiEpisodes::default()),
            episode_queue: episodes::EpisodeQueue::default(),
            timeline_jobs_started: std::sync::atomic::AtomicBool::new(false),
            probes: agent_probe::ProbeState::default(),
            chat_catalogs: Mutex::new(HashMap::new()),
            comms: comms::Comms::new(data_dir.join("workspace")),
            knowledge: Mutex::new(knowledge::KnowledgeState::default()),
        }
    }

    /// The installed workspace policy, or the inert one.
    pub(crate) fn policy(&self) -> &dyn crate::policy::WorkspacePolicy {
        self.policy.get_or_init(|| default_policy(self)).as_ref()
    }

    /// Install the policy this daemon runs with (once, at startup): the Pro
    /// host when an extension is composed, otherwise the inert policy with
    /// the durable fence read from `data_dir`.
    pub(crate) fn install_policy(
        &self,
        composed: Option<crate::pro::ProPolicy>,
        data_dir: &std::path::Path,
    ) {
        let policy: Arc<dyn crate::policy::WorkspacePolicy> = if let Some(pro) = composed {
            crate::pro::compose(self);
            Arc::new(pro)
        } else {
            Arc::new(crate::policy::Inert::new(
                crate::policy::fence::Fence::load(data_dir),
            ))
        };
        let _ = self.policy.set(policy);
    }

    /// Tests: run this state with the inert policy (no extension).
    #[cfg(test)]
    pub(crate) fn use_inert_policy(&self, fence: crate::policy::fence::Fence) {
        assert!(
            self.policy
                .set(Arc::new(crate::policy::Inert::new(fence)))
                .is_ok(),
            "policy already chosen"
        );
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

/// Tests exercise the Pro host directly unless they install the inert
/// policy; a real daemon chooses at startup (`lifecycle`).
#[cfg(test)]
fn default_policy(state: &AppState) -> Arc<dyn crate::policy::WorkspacePolicy> {
    crate::pro::compose(state);
    Arc::new(crate::pro::ProPolicy::default())
}
/// A state built outside `lifecycle` (a nondefault fixture) runs the Pro
/// host when it carries an extension, as the composed daemon would.
#[cfg(not(test))]
fn default_policy(state: &AppState) -> Arc<dyn crate::policy::WorkspacePolicy> {
    if state.daemon_extension.is_some() {
        crate::pro::compose(state);
        Arc::new(crate::pro::ProPolicy::default())
    } else {
        Arc::new(crate::policy::Inert::new(Default::default()))
    }
}

/// A value built on first use, then shared. Field access derefs through it.
pub(crate) struct Lazy<T> {
    cell: std::sync::OnceLock<T>,
    init: Mutex<Option<Box<dyn FnOnce() -> T + Send>>>,
}
impl<T> Lazy<T> {
    pub(crate) fn new(init: impl FnOnce() -> T + Send + 'static) -> Self {
        Self {
            cell: std::sync::OnceLock::new(),
            init: Mutex::new(Some(Box::new(init))),
        }
    }
    /// Whether anything has used it yet.
    #[cfg(test)]
    pub(crate) fn initialized(&self) -> bool {
        self.cell.get().is_some()
    }
}
impl<T> std::ops::Deref for Lazy<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.cell.get_or_init(|| {
            let init = crate::lock(&self.init)
                .take()
                .expect("lazy value initialised once");
            init()
        })
    }
}

impl<T> std::ops::DerefMut for Lazy<T> {
    fn deref_mut(&mut self) -> &mut T {
        let _ = &**self;
        self.cell.get_mut().expect("initialised above")
    }
}

/// The change-notification bus: a `Notify` plus a monotonically increasing
/// change generation, bumped on every wake. The generation is a cache key,
/// not a truth signal — `/ws/events` clients still wake on the 1s fallback
/// tick and each keeps its own last-sent compare; the generation only lets
/// the sessions-snapshot build run once per change instead of once per
/// connected window (see `session_view::SnapshotCache`).
pub(crate) struct ChangeBus {
    notify: tokio::sync::Notify,
    generation: std::sync::atomic::AtomicU64,
}

impl ChangeBus {
    pub(crate) fn new() -> Self {
        ChangeBus {
            notify: tokio::sync::Notify::new(),
            generation: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Announce a change: bump the generation, then wake every waiter. The
    /// bump comes first so a woken client that reads the generation always
    /// sees a value at least as new as the change that woke it.
    pub(crate) fn notify_waiters(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        self.notify.notify_waiters();
    }

    /// Wait for the next `notify_waiters` (same semantics as
    /// `Notify::notified`: only waiters registered at wake time are woken —
    /// the 1s events tick remains the backstop for missed edges).
    pub(crate) async fn notified(&self) {
        self.notify.notified().await
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Acquire)
    }

    /// The raw wake future, for a loop that must `enable()` it BEFORE
    /// reading [`Self::generation`] so no change between the two is missed
    /// (the git session tracker).
    pub(crate) fn subscribe(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }
}

/// Lock a mutex, recovering from poisoning (our critical sections cannot leave
/// the data in a broken state, so a poisoned lock is still usable).
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Codex's own home: `CODEX_HOME` when set and non-empty, else `~/.codex`.
/// Matches the shim's `${CODEX_HOME:-$HOME/.codex}`, so the daemon and the
/// terminal agree on whether the user set a theme.
fn codex_home(home: &std::path::Path, env: Option<std::ffi::OsString>) -> PathBuf {
    env.filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The never-stale argument of the shared snapshot cache rests on this
    /// ordering: by the time a waiter wakes from `notify_waiters`, the
    /// generation ALREADY reflects the change that woke it — a woken client
    /// can never rebuild keyed to the pre-change generation.
    #[tokio::test]
    async fn change_bus_stamps_generation_before_waking() {
        let bus = ChangeBus::new();
        assert_eq!(bus.generation(), 0);
        let notified = bus.notified();
        tokio::pin!(notified);
        // Register the waiter before the wake (Notify wakes only waiters
        // registered at notify time — same contract as the events loop).
        assert!(futures::poll!(notified.as_mut()).is_pending());
        bus.notify_waiters();
        notified.await;
        assert_eq!(
            bus.generation(),
            1,
            "a woken waiter must observe the generation stamped by its wake"
        );
    }

    #[test]
    fn codex_home_honours_codex_home_like_the_shim() {
        let home = std::path::Path::new("/home/u");
        assert_eq!(codex_home(home, None), home.join(".codex"));
        assert_eq!(codex_home(home, Some("".into())), home.join(".codex"));
        assert_eq!(
            codex_home(home, Some("/opt/codex-home".into())),
            PathBuf::from("/opt/codex-home")
        );
    }
}
