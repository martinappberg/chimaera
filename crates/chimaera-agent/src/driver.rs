//! Driver harness: what every structured-agent driver plugs into.
//!
//! A driver is a tokio task that owns one child process's stdio, translates
//! its native protocol into [`AgentEvent`]s, and consumes [`AgentCommand`]s.
//! The harness fixes the contract around it: spawn inputs, the handshake
//! watchdog (a session that can't handshake degrades to a PTY — it must
//! never hang a pane), kill semantics, and exit classification.

use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::model::{AgentCommand, AgentEvent};

/// Cold NFS caches make agent CLIs slow to first output; `--version` alone
/// is budgeted 2s elsewhere in the repo. Generous, but bounded.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// Polite-shutdown grace before SIGKILL.
pub const KILL_GRACE: Duration = Duration::from_secs(3);

/// Everything needed to start a driver. `argv` is COMPLETE (binary plus all
/// flags, already login-shell wrapped by the server) — argv assembly stays
/// in chimaera-server's launcher where it is unit-tested; drivers only speak
/// protocol.
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub session_id: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Env vars to strip from the inherited environment before spawning — the
    /// daemon's own launcher context (e.g. `CLAUDE_CODE_CHILD_SESSION`), which
    /// must never leak into the agent and make it think it's a nested child.
    pub env_remove: Vec<String>,
    /// The agent's native session handle when known at spawn (claude
    /// `--session-id`/`--resume` value, codex thread id to resume).
    pub pinned_native_id: Option<String>,
    pub handshake_timeout: Duration,
}

impl SpawnSpec {
    pub fn new(session_id: impl Into<String>, argv: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            session_id: session_id.into(),
            argv,
            cwd,
            env: Vec::new(),
            env_remove: Vec::new(),
            pinned_native_id: None,
            handshake_timeout: HANDSHAKE_TIMEOUT,
        }
    }
}

/// Channels a driver runs on. Command-channel closure or a `kill` signal
/// both mean "shut the child down politely, then hard".
pub struct DriverIo {
    pub commands: mpsc::Receiver<AgentCommand>,
    pub events: mpsc::Sender<AgentEvent>,
    pub kill: watch::Receiver<bool>,
}

/// How a driver task ended — the server's degrade logic keys on this.
#[derive(Debug)]
pub enum DriverExit {
    /// Child exited on its own (agent finished or crashed mid-session).
    Clean(Option<i32>),
    /// The protocol handshake never completed: wrong binary version, wrong
    /// flags, or a CLI that changed its wire format. Auto-degrade candidate.
    HandshakeFailed { reason: String, stderr_tail: String },
    /// Handshake succeeded but the stream later became unintelligible.
    ProtocolError(String),
    /// We killed it (session close or toggle).
    Killed,
}

pub trait AgentAdapter: Send + Sync {
    fn kind(&self) -> &'static str;
    fn spawn(&self, spec: SpawnSpec, io: DriverIo) -> anyhow::Result<JoinHandle<DriverExit>>;
}
