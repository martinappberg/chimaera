//! Structured agent-session engine (Tier B).
//!
//! Drives coding agents through their structured protocols instead of a PTY:
//! Claude Code over bidirectional `stream-json` (the surface the official
//! VS Code extension speaks) and Codex over `codex app-server` JSON-RPC.
//! Both are newline-delimited JSON over the child's stdio.
//!
//! Neither wire format carries a stability guarantee, so every client here is
//! written against a pinned, live-verified CLI version (`TESTED_*_VERSION` in
//! each module) and the repo's `just chat-smoke` runs the `tests/live.rs`
//! suite against the real installed binaries. Run it whenever these modules
//! change — hermetic tests cannot catch upstream protocol drift.
//!
//! [`ChatManager`] is this crate's `SessionManager` equivalent: an id-keyed
//! registry of live structured sessions, each one a driver task (child
//! process + protocol translation) feeding a pump that assigns sequence
//! numbers, journals every event, and fans out to attached clients.

pub mod claude;
pub mod codex;
pub mod driver;
pub mod journal;
pub mod model;
pub mod ndjson;
pub mod transcript;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tokio::sync::{broadcast, mpsc, watch};

use driver::{AgentAdapter, DriverExit, DriverIo, SpawnSpec};
use journal::{Journal, JournalIndex, SeqEvent};
use model::{AgentCommand, AgentEvent};

/// Called after every journaled event (server: derive AgentState, poke the
/// event bus). Runs on the pump task — keep it cheap.
pub type EventHook = Box<dyn Fn(&str, &Arc<SeqEvent>) + Send + Sync>;
/// Called once when a driver ends (server: degrade-to-PTY on handshake
/// failure, retire recents).
pub type ExitHook = Box<dyn Fn(&str, &DriverExit) + Send + Sync>;

/// Bounded channels: an unresponsive driver stalls its callers instead of
/// growing queues (login-node discipline).
const CMD_QUEUE: usize = 32;
const EVENT_QUEUE: usize = 256;
const BROADCAST_QUEUE: usize = 256;

#[derive(Clone, Debug, serde::Serialize)]
pub struct ChatInfo {
    pub id: String,
    pub agent: String,
    pub cwd: PathBuf,
    pub created_at_ms: u64,
    pub alive: bool,
    pub exit_status: Option<i32>,
    /// The agent's own session/thread id — the toggle's resume handle.
    pub native_session_id: Option<String>,
    pub model: Option<String>,
    pub current_mode: Option<String>,
    pub pending_permission: bool,
    /// Latest `SessionStatus` fold (latest-wins): the agent's own post-turn
    /// status line, `None` until one arrives. Additive wire fields — old
    /// clients ignore them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_category: Option<String>,
    /// The latest status flagged "waiting on the user"; cleared when a new
    /// turn starts (the user acted) so it never badges a running session.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub status_needs_action: bool,
    /// How many background tasks (backgrounded Bash / workflows) are running
    /// right now — the `BackgroundTasks` level-set folded to a COUNT.
    ///
    /// A count, not the set: this rides the session-list snapshot to every
    /// window on every change, and the surfaces that read it (the rail glyph,
    /// a dashboard card with no warm store) only need "is work still going".
    /// Anything wanting the rows themselves is attached to the chat socket,
    /// where the full level-set already lives. Cross-turn by nature — it
    /// survives turn ends and dies with the process.
    pub background_running: usize,
}

struct ChatSession {
    info: Mutex<ChatInfo>,
    journal: Arc<Journal>,
    cmd_tx: mpsc::Sender<AgentCommand>,
    events_tx: broadcast::Sender<Arc<SeqEvent>>,
    kill_tx: watch::Sender<bool>,
}

/// What a WS bridge gets on attach. `replay` covers everything after the
/// client's `last_seq`; `live` may overlap its tail — consumers drop live
/// events whose seq is ≤ the last replayed one. `head_seq` is the journal's
/// highest seq at attach time: a client whose own `last_seq` exceeds it is
/// stale (its journal was recreated) and must hard-reset.
pub struct ChatAttachment {
    pub info: ChatInfo,
    pub replay: Vec<Arc<SeqEvent>>,
    pub live: broadcast::Receiver<Arc<SeqEvent>>,
    pub head_seq: u64,
}

pub struct ChatManager {
    sessions: Mutex<HashMap<String, Arc<ChatSession>>>,
    journal_dir: PathBuf,
    index: Arc<JournalIndex>,
    on_event: EventHook,
    on_exit: ExitHook,
}

impl ChatManager {
    pub fn new(journal_dir: PathBuf, on_event: EventHook, on_exit: ExitHook) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            index: Arc::new(JournalIndex::load(&journal_dir)),
            journal_dir,
            on_event,
            on_exit,
        }
    }

    /// Spawn a structured session. The driver owns the child; the pump owns
    /// seq assignment, the journal, fan-out, and info upkeep.
    pub fn spawn(
        self: &Arc<Self>,
        adapter: &dyn AgentAdapter,
        spec: SpawnSpec,
    ) -> Result<ChatInfo> {
        let id = spec.session_id.clone();
        {
            let sessions = self.sessions.lock().expect("sessions lock");
            anyhow::ensure!(
                !sessions.contains_key(&id),
                "chat session {id} already exists"
            );
        }

        let journal = Arc::new(Journal::open(&self.journal_dir, &id)?);
        let (cmd_tx, cmd_rx) = mpsc::channel(CMD_QUEUE);
        let (ev_tx, mut ev_rx) = mpsc::channel::<AgentEvent>(EVENT_QUEUE);
        let (events_tx, _) = broadcast::channel(BROADCAST_QUEUE);
        let (kill_tx, kill_rx) = watch::channel(false);

        let info = ChatInfo {
            id: id.clone(),
            agent: adapter.kind().to_string(),
            cwd: spec.cwd.clone(),
            // A resurrected session carries its ORIGINAL creation time so its
            // age survives the restart; a fresh spawn stamps now.
            created_at_ms: spec.created_at_ms.unwrap_or_else(now_ms),
            alive: true,
            exit_status: None,
            native_session_id: spec.pinned_native_id.clone(),
            model: None,
            current_mode: None,
            pending_permission: false,
            status_detail: None,
            status_category: None,
            status_needs_action: false,
            background_running: 0,
        };
        let session = Arc::new(ChatSession {
            info: Mutex::new(info.clone()),
            journal: Arc::clone(&journal),
            cmd_tx,
            events_tx: events_tx.clone(),
            kill_tx,
        });

        let handle = adapter
            .spawn(
                spec,
                DriverIo {
                    commands: cmd_rx,
                    events: ev_tx,
                    kill: kill_rx,
                },
            )
            .context("spawn agent driver")?;

        // Authoritative uniqueness check: the early `ensure!` is only a
        // fast-path guard, so re-validate at insert time. A concurrent spawn
        // of the same id that slipped past the first check would otherwise
        // orphan this just-spawned driver (its Arc dropped from the registry
        // while the child keeps running). On a lost race, signal the driver to
        // shut down and bail — no unkillable orphan.
        {
            use std::collections::hash_map::Entry;
            let mut sessions = self.sessions.lock().expect("sessions lock");
            match sessions.entry(id.clone()) {
                Entry::Occupied(_) => {
                    let _ = session.kill_tx.send(true);
                    anyhow::bail!("chat session {id} already exists");
                }
                Entry::Vacant(slot) => {
                    slot.insert(Arc::clone(&session));
                }
            }
        }

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                manager.absorb(&id, &session, ev).await;
            }
            // Driver dropped its event sender. Drain the journal writer before
            // announcing the exit, so the registry slot only frees once the
            // file is settled — otherwise a view toggle's append_marker could
            // reopen it while the writer is still flushing and collide seqs.
            session.journal.sync_async().await;
            // Classify the exit.
            let exit = match handle.await {
                Ok(exit) => exit,
                Err(err) => DriverExit::ProtocolError(format!("driver task panicked: {err}")),
            };
            {
                let mut info = session.info.lock().expect("info lock");
                info.alive = false;
                if let DriverExit::Clean(status) = &exit {
                    info.exit_status = *status;
                }
            }
            (manager.on_exit)(&id, &exit);
        });

        Ok(info)
    }

    /// Journal + broadcast one event and fold it into the session info.
    async fn absorb(&self, id: &str, session: &ChatSession, ev: AgentEvent) {
        // Native id to record in the resume index, captured under the info
        // lock but recorded AFTER it drops: index.record does a blocking
        // atomic write on possibly-NFS `~/.chimaera`, and holding the info
        // lock across it would let a slow write freeze the whole manager
        // (list() takes info locks under the sessions lock).
        let mut native_to_index: Option<String> = None;
        {
            let mut info = session.info.lock().expect("info lock");
            match &ev {
                AgentEvent::Init {
                    native_session_id,
                    model,
                    current_mode,
                    ..
                } => {
                    if !native_session_id.is_empty() {
                        info.native_session_id = Some(native_session_id.clone());
                        native_to_index = Some(native_session_id.clone());
                    }
                    if model.is_some() {
                        info.model = model.clone();
                    }
                    if current_mode.is_some() {
                        info.current_mode = current_mode.clone();
                    }
                }
                AgentEvent::ModeChanged { mode_id } => {
                    info.current_mode = Some(mode_id.clone());
                }
                // "Pending permission" really means "waiting on a human
                // decision" — structured questions block the turn exactly
                // like permission prompts, so they set the same flag.
                AgentEvent::PermissionRequest { .. } | AgentEvent::QuestionRequest { .. } => {
                    info.pending_permission = true
                }
                AgentEvent::PermissionResolved { .. }
                | AgentEvent::QuestionResolved { .. }
                | AgentEvent::TurnCompleted { .. }
                | AgentEvent::TurnAborted { .. } => info.pending_permission = false,
                // A new turn also clears the "needs action" flag — the user
                // acted — while the status LINE stays as context until the
                // next summary supersedes it (latest-wins).
                AgentEvent::TurnStarted { .. } => {
                    info.pending_permission = false;
                    info.status_needs_action = false;
                }
                AgentEvent::SessionStatus {
                    category,
                    detail,
                    needs_action,
                } => {
                    info.status_detail = Some(detail.clone());
                    info.status_category = category.clone();
                    info.status_needs_action = *needs_action;
                }
                // LEVEL-SET: the event carries the whole set, so replace the
                // count rather than patching it. Deliberately NOT reset on a
                // turn boundary — background work is cross-turn, and that
                // outliving is the entire signal ("idle turn, still working").
                AgentEvent::BackgroundTasks { tasks, .. } => {
                    info.background_running =
                        tasks.iter().filter(|t| t.status == "running").count();
                }
                AgentEvent::Exited { status } => {
                    info.alive = false;
                    info.exit_status = *status;
                    // Belt-and-braces, not the primary path: claude's teardown
                    // journals an empty level-set (drain_pending), which the
                    // arm above already folds to 0. But that is per-DRIVER
                    // politeness, and this fold is driver-agnostic — a driver
                    // that dies without draining must not leave a dead row
                    // claiming live work forever.
                    info.background_running = 0;
                }
                _ => {}
            }
        }
        if let Some(native) = native_to_index {
            let index = Arc::clone(&self.index);
            let session_id = id.to_string();
            // Fire-and-forget: the native-id index is a side-index consulted
            // only to seed a resume (`lookup`). Detach the (blocking, possibly
            // NFS) write so it can NEVER stall the pump — the journal append and
            // event fan-out below must not wait on it. `record` is idempotent,
            // so a detached late write is harmless. Awaiting the JoinHandle here
            // (the old code) re-coupled the pump to that write.
            tokio::task::spawn_blocking(move || index.record(&native, &session_id));
        }
        let entry = session.journal.append(ev).await;
        let _ = session.events_tx.send(Arc::clone(&entry));
        (self.on_event)(id, &entry);
    }

    /// Subscribe-then-replay so nothing is lost between the two; the
    /// returned `live` receiver may overlap the replay tail (dedupe by seq).
    /// Replay may read the journal file — call from a blocking-ok context.
    pub fn attach(&self, id: &str, last_seq: u64) -> Result<ChatAttachment> {
        let session = self.get_session(id)?;
        let live = session.events_tx.subscribe();
        let head_seq = session.journal.last_seq();
        // A client claiming a seq beyond the journal's head is stale: its
        // journal was pruned/recreated and numbering restarted lower. Replay
        // from 0 (it hard-resets on head_seq < its last_seq) rather than
        // silently dropping every "already seen" event and freezing the pane.
        let from = if last_seq > head_seq { 0 } else { last_seq };
        let replay = session.journal.replay_from(from)?;
        let info = session.info.lock().expect("info lock").clone();
        Ok(ChatAttachment {
            info,
            replay,
            live,
            head_seq,
        })
    }

    pub async fn command(&self, id: &str, cmd: AgentCommand) -> Result<()> {
        let tx = self.get_session(id)?.cmd_tx.clone();
        tx.send(cmd).await.context("driver gone")
    }

    /// Ask the driver to shut the child down (polite, then SIGKILL after the
    /// grace period). The pump reports the exit through the usual hooks.
    pub fn kill(&self, id: &str) -> bool {
        match self.get_session(id) {
            Ok(session) => session.kill_tx.send(true).is_ok(),
            Err(_) => false,
        }
    }

    /// Drop a (dead) session from the registry. The journal file stays — it
    /// seeds history if the conversation is resumed.
    pub fn remove(&self, id: &str) -> Option<ChatInfo> {
        self.sessions
            .lock()
            .expect("sessions lock")
            .remove(id)
            .map(|s| s.info.lock().expect("info lock").clone())
    }

    pub fn get(&self, id: &str) -> Option<ChatInfo> {
        self.sessions
            .lock()
            .expect("sessions lock")
            .get(id)
            .map(|s| s.info.lock().expect("info lock").clone())
    }

    pub fn list(&self) -> Vec<ChatInfo> {
        let mut infos: Vec<ChatInfo> = self
            .sessions
            .lock()
            .expect("sessions lock")
            .values()
            .map(|s| s.info.lock().expect("info lock").clone())
            .collect();
        infos.sort_by_key(|i| i.created_at_ms);
        infos
    }

    pub fn contains(&self, id: &str) -> bool {
        self.sessions
            .lock()
            .expect("sessions lock")
            .contains_key(id)
    }

    /// The native-session-id → chimaera-session index (resume seeding).
    pub fn index(&self) -> &JournalIndex {
        &self.index
    }

    /// The chat journal directory. Enforce its byte/file budget with
    /// [`journal::prune_dir`]; safe to call periodically since dead sessions'
    /// journals stay on disk to seed resumes.
    pub fn prune_journal_dir(&self) {
        if let Err(err) = journal::prune_dir(
            &self.journal_dir,
            journal::DIR_MAX_BYTES,
            journal::DIR_MAX_FILES,
        ) {
            tracing::warn!(%err, "chat journal prune failed");
        }
    }

    pub fn journal_dir(&self) -> &PathBuf {
        &self.journal_dir
    }

    /// Seed a not-yet-spawned session's journal from a pre-built event history
    /// (transcript import) — see [`journal::seed_journal`]. Blocking fs; the
    /// caller must run it off the reactor and before spawning the session, so
    /// [`Journal::open`] picks up the seeded seq tail.
    pub fn seed_journal(&self, session_id: &str, events: &[AgentEvent]) -> Result<()> {
        journal::seed_journal(&self.journal_dir, session_id, events)
    }

    fn get_session(&self, id: &str) -> Result<Arc<ChatSession>> {
        self.sessions
            .lock()
            .expect("sessions lock")
            .get(id)
            .cloned()
            .with_context(|| format!("no chat session {id}"))
    }
}

/// Wall-clock epoch ms — shared by the pump's timestamps and the claude
/// driver's background-task start stamps (journal.rs keeps its own private
/// copy to stay self-contained).
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
