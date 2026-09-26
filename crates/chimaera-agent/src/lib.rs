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

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tokio::sync::{broadcast, mpsc, watch};

use driver::{AgentAdapter, DriverExit, DriverIo, SpawnSpec};
use journal::{Journal, JournalIndex, SeqEvent};
use model::{AgentCommand, AgentEvent, ModelInfo};

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
/// Bulk Send payload retained across the manager channel and either driver's
/// pending FIFO. This is deliberately far below the daemon's ~150 MiB RSS
/// target; one maximum browser send is ~8.25 MiB.
pub const RETAINED_SEND_BYTES_MAX: usize = 32 * 1024 * 1024;
/// A second dimension for empty/tiny sends, which otherwise consume little of
/// the byte budget but still grow driver metadata and journal work forever.
pub const RETAINED_SENDS_MAX: usize = 64;

/// A valid command was refused because this session already holds the maximum
/// amount of undelivered user input. Transports should report this as a
/// recoverable one-command failure, not as a dead driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandQueueFull;

impl fmt::Display for CommandQueueFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "queued input limit reached ({} MiB or {} messages); wait for delivery or cancel a queued message",
            RETAINED_SEND_BYTES_MAX / (1024 * 1024),
            RETAINED_SENDS_MAX
        )
    }
}

impl std::error::Error for CommandQueueFull {}

#[derive(Clone, Copy, Debug)]
struct SendReservation {
    token: u64,
    bytes: usize,
    /// Who sent it when the daemon did so itself (`UserMessage.origin`),
    /// stamped onto the driver's echo — the one event this reservation pairs
    /// with, so neither driver has to carry the tag.
    origin: Option<&'static str>,
}

#[derive(Default)]
struct CommandBudget {
    bytes: usize,
    sends: usize,
    next_token: u64,
    /// Commands accepted by the manager but not echoed by the driver yet.
    unassigned: VecDeque<SendReservation>,
    /// Driver-held sends, keyed by the delivery id resolved in the journal.
    queued: HashMap<String, SendReservation>,
}

impl CommandBudget {
    fn reserve(
        &mut self,
        bytes: usize,
        origin: Option<&'static str>,
    ) -> Result<u64, CommandQueueFull> {
        if self.sends >= RETAINED_SENDS_MAX
            || bytes > RETAINED_SEND_BYTES_MAX.saturating_sub(self.bytes)
        {
            return Err(CommandQueueFull);
        }
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        self.sends = self.sends.saturating_add(1);
        self.unassigned.push_back(SendReservation {
            token,
            bytes,
            origin,
        });
        Ok(token)
    }

    fn release(&mut self, reservation: SendReservation) {
        self.bytes = self.bytes.saturating_sub(reservation.bytes);
        self.sends = self.sends.saturating_sub(1);
    }

    fn release_unassigned(&mut self, token: u64) {
        if let Some(pos) = self.unassigned.iter().position(|r| r.token == token) {
            if let Some(reservation) = self.unassigned.remove(pos) {
                self.release(reservation);
            }
        }
    }

    /// Pair a Send echo with its reservation. Mutable so the echo can take
    /// the reservation's origin before it is journaled.
    fn observe(&mut self, event: &mut AgentEvent) {
        match event {
            // Command-originated Send echoes always carry their driver-minted
            // delivery id. Id-less UserMessages come from transcript import
            // or permission-denial feedback and must not consume the next
            // reservation waiting for a real Send echo.
            AgentEvent::UserMessage {
                id: Some(id),
                queued,
                origin,
                ..
            } => {
                let Some(reservation) = self.unassigned.pop_front() else {
                    return;
                };
                if origin.is_none() {
                    *origin = reservation.origin.map(str::to_string);
                }
                if *queued {
                    if let Some(previous) = self.queued.remove(id) {
                        self.release(previous);
                    }
                    self.queued.insert(id.clone(), reservation);
                    return;
                }
                // An immediately delivered send cannot leave bulk input in a
                // driver FIFO.
                self.release(reservation);
            }
            AgentEvent::UserMessageUpdate { id, .. } => {
                if let Some(reservation) = self.queued.remove(id) {
                    self.release(reservation);
                }
            }
            AgentEvent::Exited { .. } => self.clear(),
            _ => {}
        }
    }

    fn clear(&mut self) {
        self.bytes = 0;
        self.sends = 0;
        self.unassigned.clear();
        self.queued.clear();
    }
}

/// Cancellation-safe ownership of a reservation that has not reached the
/// driver channel yet. Dropping an async `command` future while `send().await`
/// is backpressured must release its quota just like an explicit send error.
struct EnqueueReservation<'a> {
    budget: &'a Mutex<CommandBudget>,
    token: Option<u64>,
}

impl EnqueueReservation<'_> {
    fn disarm(&mut self) {
        self.token = None;
    }
}

impl Drop for EnqueueReservation<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token {
            self.budget
                .lock()
                .expect("command budget lock")
                .release_unassigned(token);
        }
    }
}

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
    /// The session's Remote Control page (claude `session_url`) while the
    /// bridge is connecting/connected — the rail's "take it with you" badge.
    /// `None` when off, refused, or the process is gone. Additive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_control_url: Option<String>,
    /// How many process-owned jobs are still running outside the parent turn:
    /// backgrounded Bash/workflows plus cross-turn delegated agents.
    ///
    /// A count, not the set: this rides the session-list snapshot to every
    /// window on every change, and the surfaces that read it (the rail glyph,
    /// a dashboard card with no warm store) only need "is work still going".
    /// Anything wanting the rows themselves is attached to the chat socket,
    /// where the full level-set already lives. Cross-turn by nature — it
    /// survives turn ends and dies with the process.
    pub background_running: usize,
}

/// Private detail behind [`ChatInfo::background_running`]. The session-list
/// wire only needs a count, but the pump must keep the two independent sources
/// separate so a Claude background-task level-set cannot overwrite Codex
/// delegated agents (or vice versa), and keyed agent upserts never double
/// count.
#[derive(Default)]
struct BackgroundWork {
    tasks: usize,
    agents: HashSet<String>,
}

impl BackgroundWork {
    fn observe(&mut self, ev: &AgentEvent) -> usize {
        use model::{ToolKind, ToolStatus, BG_TASKS_CAP};

        match ev {
            AgentEvent::BackgroundTasks { tasks, .. } => {
                self.tasks = tasks.iter().filter(|task| task.status == "running").count();
            }
            AgentEvent::ToolCall {
                id,
                kind: ToolKind::Agent,
                status: ToolStatus::Pending | ToolStatus::InProgress,
                cross_turn: true,
                ..
            } => {
                // Codex already caps its live registry, but keep this generic
                // fold independently bounded if another driver adopts the
                // additive flag later.
                if self.agents.contains(id) || self.agents.len() < BG_TASKS_CAP {
                    self.agents.insert(id.clone());
                }
            }
            AgentEvent::ToolCallUpdate {
                id,
                status: ToolStatus::Completed | ToolStatus::Failed,
                ..
            } => {
                self.agents.remove(id);
            }
            AgentEvent::Exited { .. } => {
                self.tasks = 0;
                self.agents.clear();
            }
            _ => {}
        }
        self.tasks.saturating_add(self.agents.len())
    }
}

/// What a session's driver PROCESS holds that dies with it — the part of the
/// session a daemon restart cannot resume from the conversation alone. The
/// server's ledger snapshots it, and resurrection re-establishes what it can
/// (the bridge, ultracode) and tells the agent about what it cannot (its
/// background work, a cut-off turn). Folded from the same events as
/// [`ChatInfo`] but kept off the session-list wire: only the ledger reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Carryover {
    /// The Remote Control bridge was on (connecting or connected).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remote_control: bool,
    /// Ultracode was on (claude's session-scoped flag).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ultracode: bool,
    /// A turn was running, including one parked on a permission or question.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub turn_in_flight: bool,
    /// Background work still running: the level-set minus settled and
    /// ambient tasks. Bounded by the drivers' `BG_TASKS_CAP`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background: Vec<CarriedTask>,
    /// When this process was handed the daemon's restart pick-up message
    /// (epoch ms; 0 = never) — how the next resurrection tells a restart
    /// LOOP from a series of updates, since each pick-up starts a billed turn.
    #[serde(default, skip_serializing_if = "is_zero_ms")]
    pub pickup_at_ms: u64,
}

fn is_zero_ms(ms: &u64) -> bool {
    *ms == 0
}

/// One running background task, as much of it as a successor needs to name
/// it: the ids the agent's own tool results used, and its label. Workflow
/// progress is deliberately dropped — it changes on every tick, and the
/// ledger that stores this rewrites on change.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CarriedTask {
    pub id: String,
    pub task_type: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub monitor: bool,
}

impl Carryover {
    fn observe(&mut self, ev: &AgentEvent) {
        use model::RemoteControlState as Rc;

        match ev {
            // Claude re-emits Init mid-process; the snapshot it carries is
            // the bridge as it stands, so this never wipes a live one.
            AgentEvent::Init { remote_control, .. } => {
                self.remote_control = remote_control
                    .as_ref()
                    .is_some_and(|rc| matches!(rc.state, Rc::Connecting | Rc::Connected));
            }
            AgentEvent::RemoteControl { state, .. } => {
                self.remote_control = matches!(state, Rc::Connecting | Rc::Connected);
            }
            AgentEvent::EffortState { ultracode, .. } => self.ultracode = *ultracode,
            AgentEvent::UserMessage {
                origin: Some(origin),
                ..
            } if origin == model::ORIGIN_RESTART => self.pickup_at_ms = now_ms(),
            AgentEvent::TurnStarted { .. } => self.turn_in_flight = true,
            AgentEvent::TurnCompleted { .. } | AgentEvent::TurnAborted { .. } => {
                self.turn_in_flight = false;
            }
            AgentEvent::BackgroundTasks { tasks, .. } => {
                self.background = tasks
                    .iter()
                    .filter(|t| t.status == "running" && !t.ambient)
                    .map(|t| CarriedTask {
                        id: t.id.clone(),
                        task_type: t.task_type.clone(),
                        description: t.description.clone(),
                        workflow_name: t.workflow_name.clone(),
                        monitor: t.monitor,
                    })
                    .collect();
            }
            AgentEvent::Exited { .. } => *self = Carryover::default(),
            _ => {}
        }
    }

    /// Work the restart cut off: something the agent itself has to pick up.
    pub fn interrupted_work(&self) -> bool {
        self.turn_in_flight || !self.background.is_empty()
    }
}

/// Fold the driver's authoritative model/mode state into the lightweight
/// session-list snapshot. The attached chat store consumes the same events;
/// keeping this fold beside `ChatInfo` prevents dashboard rows and fresh WS
/// ready metadata from lagging behind a live model reroute.
fn fold_session_metadata(info: &mut ChatInfo, ev: &AgentEvent) {
    match ev {
        AgentEvent::Init {
            model,
            current_mode,
            remote_control,
            ..
        } => {
            use crate::model::RemoteControlState as Rc;
            // Init is a complete process snapshot. Clear values that a fresh
            // driver no longer advertises instead of retaining stale state
            // from a resumed/restarted process. The Remote Control bridge is
            // process-owned, so a new process starts with none.
            info.model = model.clone();
            info.current_mode = current_mode.clone();
            // Init is re-emitted mid-process (claude's per-turn system/init),
            // so the bridge rides the snapshot rather than being cleared.
            info.remote_control_url = remote_control.as_ref().and_then(|rc| match rc.state {
                Rc::Connecting | Rc::Connected => rc.session_url.clone(),
                Rc::Off | Rc::Error => None,
            });
        }
        AgentEvent::RemoteControl {
            state, session_url, ..
        } => {
            use crate::model::RemoteControlState as Rc;
            info.remote_control_url = match state {
                Rc::Connecting | Rc::Connected => session_url.clone(),
                Rc::Off | Rc::Error => None,
            };
        }
        AgentEvent::ModelSwitched { to, .. } => {
            info.model = Some(to.clone());
        }
        AgentEvent::ModeChanged {
            mode_id,
            chosen: false,
        } => {
            info.current_mode = Some(mode_id.clone());
        }
        // The bridge is process-owned: a dead process has no live link, even
        // if the driver never got to journal its Off (a hard kill).
        AgentEvent::Exited { .. } => {
            info.remote_control_url = None;
        }
        _ => {}
    }
}

struct ChatSession {
    info: Mutex<ChatInfo>,
    background_work: Mutex<BackgroundWork>,
    carryover: Mutex<Carryover>,
    journal: Arc<Journal>,
    cmd_tx: mpsc::Sender<AgentCommand>,
    events_tx: broadcast::Sender<Arc<SeqEvent>>,
    kill_tx: watch::Sender<bool>,
    /// Serializes reservations with channel enqueue so driver echoes consume
    /// the manager's FIFO in exactly the command order.
    command_order: tokio::sync::Mutex<()>,
    command_budget: Mutex<CommandBudget>,
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
    /// Effective replay cursor. Usually the caller's `last_seq`; reset to 0
    /// when that cursor is ahead of a recreated journal's head. The WS bridge
    /// must dedupe live events against THIS value, not the stale client value
    /// (an empty replay otherwise leaves the stale cursor in force forever).
    pub replay_from: u64,
    pub head_seq: u64,
}

/// One conversation-settings write (keyed by the native id) queued by
/// `absorb` for the blocking lane. `effort: Some(None)` records "no effort".
#[derive(Default)]
struct IndexUpdate {
    native: String,
    model: Option<String>,
    effort: Option<Option<String>>,
    mode: Option<String>,
}

/// One per-agent preference write queued by `absorb` for the blocking lane.
#[derive(Default)]
struct PrefsUpdate {
    agent: String,
    model: Option<String>,
    effort: Option<String>,
    mode: Option<String>,
}

pub struct ChatManager {
    sessions: Mutex<HashMap<String, Arc<ChatSession>>>,
    journal_dir: PathBuf,
    index: Arc<JournalIndex>,
    on_event: EventHook,
    on_exit: ExitHook,
    /// The newest model catalog each agent kind advertised on an `Init`
    /// (claude `initialize.models`, codex `model/list`) — the launcher's
    /// live source for a launch-time model list, replacing the curated
    /// fallback once one chat has handshaked. Bounded by construction (the
    /// drivers cap the catalog); process-lifetime only (the next chat's Init
    /// refreshes it after a restart).
    latest_models: Mutex<HashMap<String, Vec<ModelInfo>>>,
    /// Last user-chosen model / effort per agent kind (see
    /// [`journal::AgentPrefsStore`]) — what a new chat of that kind starts
    /// with. Fed from user-initiated `ModelSwitched` and `EffortState`.
    prefs: Arc<journal::AgentPrefsStore>,
}

impl ChatManager {
    pub fn new(journal_dir: PathBuf, on_event: EventHook, on_exit: ExitHook) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            index: Arc::new(JournalIndex::load(&journal_dir)),
            prefs: Arc::new(journal::AgentPrefsStore::load(&journal_dir)),
            journal_dir,
            on_event,
            on_exit,
            latest_models: Mutex::new(HashMap::new()),
        }
    }

    /// The last model / effort the user chose on this agent kind (`"claude"`
    /// / `"codex"`); defaults when none was ever chosen.
    pub fn prefs(&self, agent: &str) -> journal::AgentPrefs {
        self.prefs.get(agent)
    }

    /// The newest model catalog an agent kind (`"claude"` / `"codex"`)
    /// advertised on any Init this process saw; empty = none yet.
    pub fn latest_models(&self, agent: &str) -> Vec<ModelInfo> {
        self.latest_models
            .lock()
            .expect("latest_models lock")
            .get(agent)
            .cloned()
            .unwrap_or_default()
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

        // Background work survives TURNS, not driver processes. A hard daemon
        // stop cannot run the old mapper's teardown, so the reused journal may
        // end with a non-empty level-set even though those child tasks died
        // with the old process. Put the empty set ahead of every event the new
        // driver can emit: replay then converges on the new process's truth,
        // and a resurrected pane cannot offer a stop button for a dead task.
        // Queueing it before adapter.spawn also means a synchronous spawn
        // failure drops the reset along with this unregistered session.
        ev_tx
            .try_send(AgentEvent::BackgroundTasks {
                tasks: Vec::new(),
                closed: Vec::new(),
            })
            .expect("fresh driver event queue has room for lifecycle reset");

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
            remote_control_url: None,
            background_running: 0,
        };
        let session = Arc::new(ChatSession {
            info: Mutex::new(info.clone()),
            background_work: Mutex::new(BackgroundWork::default()),
            carryover: Mutex::new(Carryover::default()),
            journal: Arc::clone(&journal),
            cmd_tx,
            events_tx: events_tx.clone(),
            kill_tx,
            command_order: tokio::sync::Mutex::new(()),
            command_budget: Mutex::new(CommandBudget::default()),
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
            session
                .command_budget
                .lock()
                .expect("command budget lock")
                .clear();
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
    async fn absorb(&self, id: &str, session: &ChatSession, mut ev: AgentEvent) {
        session
            .command_budget
            .lock()
            .expect("command budget lock")
            .observe(&mut ev);
        session
            .carryover
            .lock()
            .expect("carryover lock")
            .observe(&ev);
        let background_running = session
            .background_work
            .lock()
            .expect("background work lock")
            .observe(&ev);
        // Native id to record in the resume index, captured under the info
        // lock but recorded AFTER it drops: index.record does a blocking
        // atomic write on possibly-NFS `~/.chimaera`, and holding the info
        // lock across it would let a slow write freeze the whole manager
        // (list() takes info locks under the sessions lock).
        let mut native_to_index: Option<String> = None;
        let mut index_to_record: Option<IndexUpdate> = None;
        let mut catalog_to_record: Option<String> = None;
        let mut prefs_to_record: Option<PrefsUpdate> = None;
        {
            let mut info = session.info.lock().expect("info lock");
            fold_session_metadata(&mut info, &ev);
            info.background_running = background_running;
            match &ev {
                AgentEvent::Init {
                    native_session_id,
                    models,
                    ..
                } => {
                    if !native_session_id.is_empty() {
                        info.native_session_id = Some(native_session_id.clone());
                        native_to_index = Some(native_session_id.clone());
                        // The process's opening model/mode are what a reopen
                        // of THIS native conversation must come back with
                        // (fold_session_metadata already copied them in).
                        index_to_record = Some(IndexUpdate {
                            native: native_session_id.clone(),
                            model: info.model.clone(),
                            mode: info.current_mode.clone(),
                            ..Default::default()
                        });
                    }
                    if !models.is_empty() {
                        // Recorded AFTER the info lock drops (below): the
                        // catalog clone and the second mutex stay out of the
                        // registry's critical section.
                        catalog_to_record = Some(info.agent.clone());
                    }
                }
                AgentEvent::EffortState { effort, chosen, .. } => {
                    // The effort in effect is conversation-local (keyed by the
                    // native id): a reopen of this conversation — codex's
                    // thread/start effort, claude's post-handshake apply —
                    // starts from it. `None` records "no effort in effect".
                    if let Some(native) = &info.native_session_id {
                        index_to_record = Some(IndexUpdate {
                            native: native.clone(),
                            effort: Some(effort.clone()),
                            ..Default::default()
                        });
                    }
                    // The user's own effort pick is what the next chat of this
                    // kind starts with (both agents' in-session efforts are
                    // session-scoped, so chimaera remembers them). A bootstrap
                    // read or a model-switch reset (`chosen: false`) is not.
                    if let (true, Some(effort)) = (*chosen, effort) {
                        prefs_to_record = Some(PrefsUpdate {
                            agent: info.agent.clone(),
                            effort: Some(effort.clone()),
                            ..Default::default()
                        });
                    }
                }
                // The model in effect is the conversation's own (any reason);
                // only a user's pick (no reason) is a preference — a safety
                // reroute or a credits fallback (reason present) is not.
                AgentEvent::ModelSwitched { to, reason, .. } => {
                    if let Some(native) = &info.native_session_id {
                        index_to_record = Some(IndexUpdate {
                            native: native.clone(),
                            model: Some(to.clone()),
                            ..Default::default()
                        });
                    }
                    if reason.is_none() {
                        prefs_to_record = Some(PrefsUpdate {
                            agent: info.agent.clone(),
                            model: Some(to.clone()),
                            ..Default::default()
                        });
                    }
                }
                // Likewise the permission/approval mode: the conversation's
                // own either way; a preference only when the user picked it
                // (not one the agent switched on its own, e.g. a plan exit).
                AgentEvent::ModeChanged { mode_id, chosen } => {
                    if let Some(native) = &info.native_session_id {
                        index_to_record = Some(IndexUpdate {
                            native: native.clone(),
                            mode: Some(mode_id.clone()),
                            ..Default::default()
                        });
                    }
                    if *chosen {
                        prefs_to_record = Some(PrefsUpdate {
                            agent: info.agent.clone(),
                            mode: Some(mode_id.clone()),
                            ..Default::default()
                        });
                    }
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
                AgentEvent::Exited { status } => {
                    info.alive = false;
                    info.exit_status = *status;
                }
                _ => {}
            }
        }
        if let Some(agent) = catalog_to_record {
            if let AgentEvent::Init { models, .. } = &ev {
                self.latest_models
                    .lock()
                    .expect("latest_models lock")
                    .insert(agent, models.clone());
            }
        }
        if native_to_index.is_some() || index_to_record.is_some() {
            let index = Arc::clone(&self.index);
            let session_id = id.to_string();
            // Detached on purpose: the index write can block on NFS, and the
            // event fan-out below must not wait on it. `record` is idempotent,
            // so a detached late write is harmless. Awaiting the JoinHandle here
            // (the old code) re-coupled the pump to that write. The mapping and
            // the settings an Init carries ride one hop so they cannot land
            // out of order.
            tokio::task::spawn_blocking(move || {
                if let Some(native) = native_to_index {
                    index.record(&native, &session_id);
                }
                if let Some(update) = index_to_record {
                    index.record_settings(&update.native, &session_id, |s| {
                        if let Some(model) = update.model {
                            s.model = Some(model);
                        }
                        if let Some(effort) = update.effort {
                            s.effort = effort;
                        }
                        if let Some(mode) = update.mode {
                            s.mode = Some(mode);
                        }
                    });
                }
            });
        }
        if let Some(update) = prefs_to_record {
            let prefs = Arc::clone(&self.prefs);
            // Same NFS rule: never park the pump on the atomic rewrite.
            tokio::task::spawn_blocking(move || {
                if let Some(model) = update.model {
                    prefs.record_model(&update.agent, &model);
                }
                if let Some(effort) = update.effort {
                    prefs.record_effort(&update.agent, &effort);
                }
                if let Some(mode) = update.mode {
                    prefs.record_mode(&update.agent, &mode);
                }
            });
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
            replay_from: from,
            head_seq,
        })
    }

    pub async fn command(&self, id: &str, cmd: AgentCommand) -> Result<()> {
        self.command_as(id, cmd, None).await
    }

    /// [`Self::command`] for a send the DAEMON makes on its own (not a
    /// client's): its echo journals with `UserMessage.origin = origin`, so the
    /// transcript says who spoke. Ignored for commands that are not sends.
    pub async fn command_as(
        &self,
        id: &str,
        cmd: AgentCommand,
        origin: Option<&'static str>,
    ) -> Result<()> {
        cmd.validate_ingress().context("invalid agent command")?;
        let session = self.get_session(id)?;
        let _order = session.command_order.lock().await;
        let mut reservation = if let Some(bytes) = cmd.retained_send_bytes() {
            let token = session
                .command_budget
                .lock()
                .expect("command budget lock")
                .reserve(bytes, origin)?;
            Some(EnqueueReservation {
                budget: &session.command_budget,
                token: Some(token),
            })
        } else {
            None
        };
        session.cmd_tx.send(cmd).await.context("driver gone")?;
        if let Some(reservation) = &mut reservation {
            // The driver channel now owns the command. Keep its quota until
            // the pump observes delivery/update/exit.
            reservation.disarm();
        }
        Ok(())
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

    /// The live process's [`Carryover`] — what a restart would cut off.
    pub fn carryover(&self, id: &str) -> Option<Carryover> {
        self.sessions
            .lock()
            .expect("sessions lock")
            .get(id)
            .map(|s| s.carryover.lock().expect("carryover lock").clone())
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

    /// Enforce the chat journal directory's byte/file budget with
    /// [`journal::prune_dir`], evicting history (dead sessions' journals,
    /// kept on disk to seed resumes) oldest-first. Every session in this
    /// registry is protected; `keep` adds the ids only the caller knows are
    /// still in use — a session between processes (respawn, view switch,
    /// resurrection) owns a journal it is about to reopen. Blocking fs: run
    /// it off the reactor.
    pub fn prune_journal_dir(&self, keep: &HashSet<String>) {
        let mut keep = keep.clone();
        keep.extend(self.sessions.lock().expect("sessions lock").keys().cloned());
        if let Err(err) = journal::prune_dir(
            &self.journal_dir,
            journal::DIR_MAX_BYTES,
            journal::DIR_MAX_FILES,
            &keep,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_info_with_metadata(model: Option<&str>, mode: Option<&str>) -> ChatInfo {
        ChatInfo {
            id: "chat-1".into(),
            agent: "codex".into(),
            cwd: PathBuf::from("/workspace"),
            created_at_ms: 0,
            alive: true,
            exit_status: None,
            native_session_id: None,
            model: model.map(str::to_string),
            current_mode: mode.map(str::to_string),
            pending_permission: false,
            status_detail: None,
            status_category: None,
            status_needs_action: false,
            remote_control_url: None,
            background_running: 0,
        }
    }

    #[test]
    fn session_metadata_tracks_authoritative_init_and_model_switches() {
        let mut info = chat_info_with_metadata(Some("old-model"), Some("old-mode"));
        fold_session_metadata(
            &mut info,
            &AgentEvent::Init {
                native_session_id: "thread-1".into(),
                model: None,
                modes: Vec::new(),
                current_mode: None,
                slash_commands: Vec::new(),
                models: Vec::new(),
                agent_version: None,
                remote_control_available: false,
                remote_control_auto_enable: false,
                remote_control: None,
            },
        );
        assert_eq!(info.model, None, "a complete Init clears stale model state");
        assert_eq!(
            info.current_mode, None,
            "a complete Init clears stale mode state"
        );

        fold_session_metadata(
            &mut info,
            &AgentEvent::ModelSwitched {
                from: None,
                to: "rerouted-model".into(),
                reason: Some("server reroute".into()),
                retract_current_turn: false,
            },
        );
        assert_eq!(info.model.as_deref(), Some("rerouted-model"));

        fold_session_metadata(
            &mut info,
            &AgentEvent::ModeChanged {
                mode_id: "auto-review".into(),
                chosen: false,
            },
        );
        assert_eq!(info.current_mode.as_deref(), Some("auto-review"));
    }

    #[test]
    fn background_work_counts_cross_turn_agents_until_their_own_close() {
        let mut work = BackgroundWork::default();
        let opened = AgentEvent::ToolCall {
            id: "agent:sub-1".into(),
            kind: model::ToolKind::Agent,
            title: "Agent: analyst".into(),
            locations: Vec::new(),
            status: model::ToolStatus::InProgress,
            cross_turn: true,
        };
        assert_eq!(work.observe(&opened), 1);
        assert_eq!(work.observe(&opened), 1, "a row upsert never double-counts");

        assert_eq!(
            work.observe(&AgentEvent::TurnCompleted {
                turn_id: "parent".into(),
                usage: model::Usage::default(),
            }),
            1,
            "the parent turn boundary does not end its child thread"
        );
        assert_eq!(
            work.observe(&AgentEvent::ToolCallUpdate {
                id: "agent:sub-1".into(),
                status: model::ToolStatus::Completed,
                content: None,
            }),
            0,
            "the child's own terminal update clears the count"
        );
    }

    #[test]
    fn command_budget_bounds_bytes_and_releases_on_delivery() {
        let mut budget = CommandBudget::default();
        let first = budget.reserve(RETAINED_SEND_BYTES_MAX - 1, None).unwrap();
        assert_eq!(budget.reserve(2, None), Err(CommandQueueFull));
        budget.observe(&mut AgentEvent::UserMessage {
            text: "queued".to_string(),
            attachments: 0,
            id: Some("q1".to_string()),
            queued: true,
            origin: None,
        });
        assert_eq!(budget.bytes, RETAINED_SEND_BYTES_MAX - 1);
        budget.observe(&mut AgentEvent::UserMessageUpdate {
            id: "q1".to_string(),
            state: model::UserMessageState::Sent,
        });
        assert_eq!(budget.bytes, 0);
        assert_eq!(budget.sends, 0);
        // The reservation was assigned, so a late enqueue-failure cleanup is
        // harmless rather than releasing some unrelated future command.
        budget.release_unassigned(first);
        assert_eq!(budget.sends, 0);
    }

    #[test]
    fn command_budget_bounds_tiny_send_count_and_clears_on_exit() {
        let mut budget = CommandBudget::default();
        for _ in 0..RETAINED_SENDS_MAX {
            budget.reserve(0, None).unwrap();
        }
        assert_eq!(budget.reserve(0, None), Err(CommandQueueFull));
        budget.observe(&mut AgentEvent::Exited { status: None });
        assert_eq!(budget.bytes, 0);
        assert_eq!(budget.sends, 0);
        assert!(budget.unassigned.is_empty());
    }

    #[test]
    fn idless_feedback_does_not_consume_a_send_reservation() {
        let mut budget = CommandBudget::default();
        budget.reserve(1024, None).unwrap();
        budget.observe(&mut AgentEvent::UserMessage {
            text: "try a dry run first".to_string(),
            attachments: 0,
            id: None,
            queued: false,
            origin: None,
        });
        assert_eq!(budget.bytes, 1024);
        assert_eq!(budget.unassigned.len(), 1);

        budget.observe(&mut AgentEvent::UserMessage {
            text: "the actual send".to_string(),
            attachments: 0,
            id: Some("q1".to_string()),
            queued: true,
            origin: None,
        });
        assert!(budget.unassigned.is_empty());
        assert!(budget.queued.contains_key("q1"));
    }

    #[test]
    fn enqueue_reservation_drop_releases_quota_unless_disarmed() {
        let budget = Mutex::new(CommandBudget::default());
        let token = budget.lock().unwrap().reserve(1024, None).unwrap();
        {
            let _guard = EnqueueReservation {
                budget: &budget,
                token: Some(token),
            };
        }
        assert_eq!(budget.lock().unwrap().bytes, 0);

        let token = budget.lock().unwrap().reserve(2048, None).unwrap();
        {
            let mut guard = EnqueueReservation {
                budget: &budget,
                token: Some(token),
            };
            guard.disarm();
        }
        assert_eq!(budget.lock().unwrap().bytes, 2048);
    }

    #[test]
    fn daemon_origin_rides_the_echo_its_reservation_pairs_with() {
        let mut budget = CommandBudget::default();
        budget.reserve(64, Some(model::ORIGIN_RESTART)).unwrap();
        budget.reserve(64, None).unwrap();
        // Id-less feedback consumes nothing, so it cannot steal the tag.
        let mut feedback = AgentEvent::UserMessage {
            text: "feedback".to_string(),
            attachments: 0,
            id: None,
            queued: false,
            origin: None,
        };
        budget.observe(&mut feedback);
        let origin_of = |ev: &AgentEvent| match ev {
            AgentEvent::UserMessage { origin, .. } => origin.clone(),
            _ => unreachable!(),
        };
        assert_eq!(origin_of(&feedback), None);
        let echo = |id: &str| AgentEvent::UserMessage {
            text: "sent".to_string(),
            attachments: 0,
            id: Some(id.to_string()),
            queued: false,
            origin: None,
        };
        let mut first = echo("m1");
        budget.observe(&mut first);
        assert_eq!(origin_of(&first).as_deref(), Some(model::ORIGIN_RESTART));
        let mut second = echo("m2");
        budget.observe(&mut second);
        assert_eq!(origin_of(&second), None, "a client send stays untagged");
    }

    fn running_task(id: &str, task_type: &str) -> model::BackgroundTask {
        model::BackgroundTask {
            id: id.into(),
            task_type: task_type.into(),
            description: format!("{id} work"),
            status: "running".into(),
            started_at_ms: 1,
            workflow_name: None,
            agents: Vec::new(),
            agents_total: 0,
            agents_done: 0,
            monitor: false,
            ambient: false,
            tool_use_id: None,
        }
    }

    #[test]
    fn carryover_tracks_what_dies_with_the_process() {
        use model::RemoteControlState as Rc;

        let mut carry = Carryover::default();
        assert!(!carry.interrupted_work());

        carry.observe(&AgentEvent::RemoteControl {
            state: Rc::Connected,
            session_url: Some("https://claude.ai/code/session_x".into()),
            name: None,
            detail: None,
        });
        carry.observe(&AgentEvent::EffortState {
            effort: Some("xhigh".into()),
            ultracode: true,
            chosen: true,
        });
        let mut monitor = running_task("bg-2", "local_bash");
        monitor.monitor = true;
        let mut settled = running_task("bg-3", "local_bash");
        settled.status = "completed".into();
        let mut ambient = running_task("bg-4", "local_bash");
        ambient.ambient = true;
        carry.observe(&AgentEvent::BackgroundTasks {
            tasks: vec![
                running_task("bg-1", "local_bash"),
                monitor,
                settled,
                ambient,
            ],
            closed: Vec::new(),
        });
        assert!(carry.remote_control && carry.ultracode);
        assert_eq!(
            carry
                .background
                .iter()
                .map(|t| (t.id.as_str(), t.monitor))
                .collect::<Vec<_>>(),
            [("bg-1", false), ("bg-2", true)],
            "settled and ambient tasks are not work to pick up"
        );
        assert!(carry.interrupted_work(), "background work alone counts");

        carry.observe(&AgentEvent::TurnStarted {
            turn_id: "t1".into(),
        });
        assert!(carry.turn_in_flight);
        // Claude's mid-process Init repeats the live bridge; it never wipes it.
        carry.observe(&AgentEvent::Init {
            native_session_id: "native".into(),
            model: None,
            modes: Vec::new(),
            current_mode: None,
            slash_commands: Vec::new(),
            models: Vec::new(),
            agent_version: None,
            remote_control_available: true,
            remote_control_auto_enable: false,
            remote_control: Some(model::RemoteControlSnapshot {
                state: Rc::Connected,
                session_url: None,
                name: None,
            }),
        });
        assert!(carry.remote_control);
        carry.observe(&AgentEvent::TurnAborted {
            turn_id: "t1".into(),
            reason: "interrupted".into(),
            interrupted: true,
        });
        assert!(!carry.turn_in_flight);

        assert_eq!(carry.pickup_at_ms, 0);
        carry.observe(&AgentEvent::UserMessage {
            text: "pick up".into(),
            attachments: 0,
            id: Some("m1".into()),
            queued: false,
            origin: Some(model::ORIGIN_RESTART.into()),
        });
        assert!(carry.pickup_at_ms > 0, "the pick-up is stamped");

        carry.observe(&AgentEvent::Exited { status: Some(0) });
        assert_eq!(carry, Carryover::default(), "nothing outlives the process");
    }
}
