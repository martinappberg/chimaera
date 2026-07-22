use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::{
    agent_updates, agents, chat, compute, environment, fs, git, launcher, ledger, quickopen,
    recents, settings, update, view_state, workspaces,
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
    /// workspace id -> board nudge epoch, bumped on every board mutation
    /// (`board::bump_board_epoch`). Same invalidate-and-pull contract as the
    /// git epochs: `/ws/events` carries only the number, the pane refetches
    /// `/board/render`. Bounded by the workspace count.
    pub(crate) board_epochs: Mutex<HashMap<String, u64>>,
    /// board path -> deadline for the deferred git bump after a board edit
    /// (`board::schedule_git_settle`). Entries live ~1s (removed when the
    /// settle timer fires) and the scheduler flushes immediately past a hard
    /// cap, so the map stays bounded by the per-second edit fan-out.
    pub(crate) board_git_settle: Mutex<HashMap<String, tokio::time::Instant>>,
    /// Compute-scheduler awareness (Slurm detection + the user's queue),
    /// cached + single-flight; empty-tagged on a laptop. Never persisted.
    pub(crate) compute: compute::ComputeService,
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
    /// Per-session upload landing pad (`~/.chimaera/uploads/<session-id>/`):
    /// OS-desktop drops and pasted screenshots stream here so their PATHS can
    /// be referenced in prompts/shells. Under the data dir (not runtime_dir):
    /// uploads must live as long as their session, and runtime tmp gets
    /// night-scrubbed on HPC. Size-capped per file and per session (`upload`),
    /// pruned when the session ends and at boot.
    pub(crate) uploads_root: PathBuf,
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
            mastermind_switching: Mutex::new(std::collections::HashSet::new()),
            spawn_reservations: Mutex::new(HashMap::new()),
            session_workspaces: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            display_names: Mutex::new(HashMap::new()),
            current_cwds: Mutex::new(HashMap::new()),
            exec_status: Mutex::new(HashMap::new()),
            links: Mutex::new(HashMap::new()),
            tickets: Mutex::new(fs::TicketStore::default()),
            quickopen: Mutex::new(quickopen::QuickOpenCache::default()),
            git: git::GitService::new(),
            board_epochs: Mutex::new(HashMap::new()),
            board_git_settle: Mutex::new(HashMap::new()),
            compute: compute::ComputeService::new(),
            changes: tokio::sync::Notify::new(),
            restored: tokio::sync::watch::channel(true).0,
            shutdown: tokio::sync::Notify::new(),
            agent_bins: Mutex::new(HashMap::new()),
            claude_projects_dir: home.join(".claude").join("projects"),
            managed_root: data_dir.join("agents"),
            worktrees_root: data_dir.join("worktrees"),
            shims_dir: data_dir.join("shims"),
            uploads_root: data_dir.join("uploads"),
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
