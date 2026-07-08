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
/// events whose seq is ≤ the last replayed one.
pub struct ChatAttachment {
    pub info: ChatInfo,
    pub replay: Vec<Arc<SeqEvent>>,
    pub live: broadcast::Receiver<Arc<SeqEvent>>,
}

pub struct ChatManager {
    sessions: Mutex<HashMap<String, Arc<ChatSession>>>,
    journal_dir: PathBuf,
    index: JournalIndex,
    on_event: EventHook,
    on_exit: ExitHook,
}

impl ChatManager {
    pub fn new(journal_dir: PathBuf, on_event: EventHook, on_exit: ExitHook) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            index: JournalIndex::load(&journal_dir),
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
            created_at_ms: now_ms(),
            alive: true,
            exit_status: None,
            native_session_id: spec.pinned_native_id.clone(),
            model: None,
            current_mode: None,
            pending_permission: false,
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

        self.sessions
            .lock()
            .expect("sessions lock")
            .insert(id.clone(), Arc::clone(&session));

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                manager.absorb(&id, &session, ev);
            }
            // Driver dropped its event sender: classify the exit.
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
    fn absorb(&self, id: &str, session: &ChatSession, ev: AgentEvent) {
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
                        self.index.record(native_session_id, id);
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
                AgentEvent::PermissionRequest { .. } => info.pending_permission = true,
                AgentEvent::PermissionResolved { .. }
                | AgentEvent::TurnStarted { .. }
                | AgentEvent::TurnCompleted { .. }
                | AgentEvent::TurnAborted { .. } => info.pending_permission = false,
                AgentEvent::Exited { status } => {
                    info.alive = false;
                    info.exit_status = *status;
                }
                _ => {}
            }
        }
        let entry = session.journal.append(ev);
        let _ = session.events_tx.send(Arc::clone(&entry));
        (self.on_event)(id, &entry);
    }

    /// Subscribe-then-replay so nothing is lost between the two; the
    /// returned `live` receiver may overlap the replay tail (dedupe by seq).
    /// Replay may read the journal file — call from a blocking-ok context.
    pub fn attach(&self, id: &str, last_seq: u64) -> Result<ChatAttachment> {
        let session = self.get_session(id)?;
        let live = session.events_tx.subscribe();
        let replay = session.journal.replay_from(last_seq)?;
        let info = session.info.lock().expect("info lock").clone();
        Ok(ChatAttachment { info, replay, live })
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

    pub fn journal_dir(&self) -> &PathBuf {
        &self.journal_dir
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
