//! chimaera-pty: the persistent terminal engine.
//!
//! A [`SessionManager`] owns long-lived PTY sessions. Each session runs a
//! command (or the user's interactive shell) attached to a native PTY and
//! mirrors every byte of output into a headless [`alacritty_terminal::Term`]
//! so the full screen state (scrollback, colors, cursor, title) survives with
//! zero attached clients. Clients are ephemeral: [`SessionManager::attach`]
//! returns a snapshot escape stream that reconstructs the terminal in a fresh
//! xterm.js instance, plus live output/event receivers and an input sender.

pub mod exec;
pub mod marks;
mod session;
mod snapshot;

pub use exec::{ExecError, ExecMode, ExecOptions, ExecOutcome, ExecStage};
pub use marks::{CommandSource, CommandView, Marks, ShellPhase};
pub use session::KILL_ESCALATION_GRACE;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{anyhow, Context};

pub type SessionId = String;

/// Metadata describing a session, live or exited.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    pub created_at: u64,
    pub alive: bool,
    pub exit_status: Option<i32>,
    pub title: Option<String>,
    /// OS pid of the direct child (the shell or command), when known.
    pub pid: Option<u32>,
    /// True when `name` was pinned explicitly (spawn name / user rename)
    /// rather than derived from the cwd; pinned names stay authoritative.
    pub renamed: bool,
    /// Shell phase from OSC 133 marks (`unknown` without shell integration).
    pub phase: ShellPhase,
    /// Unix ms of the most recent PTY output chunk (the spawn instant until
    /// the first byte arrives). Kept OFF the wire: a raw timestamp would
    /// defeat the events-bus snapshot dedupe on every tick — the daemon
    /// serializes a derived activity flag instead (`session_view`).
    #[serde(skip)]
    pub last_output_at: u64,
}

/// Options for spawning a new session.
#[derive(Clone, Debug)]
pub struct SpawnOpts {
    pub cwd: PathBuf,
    pub name: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// `None` spawns the user's shell (`$SHELL`, else `/bin/bash`) as an
    /// interactive shell.
    pub command: Option<Vec<String>>,
    /// Caller-chosen session id (must be unused). `None` generates one. This
    /// lets callers embed the id in the command/environment before spawning.
    pub id: Option<SessionId>,
    /// Extra environment variables for the child, applied on top of the
    /// inherited environment (a pair with an existing name overrides it, so
    /// callers can e.g. prepend to PATH). The daemon uses this to inject
    /// `CHIMAERA_SESSION`/`CHIMAERA_THEME`, the shim dir, and the shell
    /// integration bootstrap into every session it spawns.
    pub env: Vec<(String, String)>,
    /// Inherited variables to REMOVE from the child's environment. The
    /// daemon uses this to scrub launcher-context markers (e.g. a Claude
    /// Code session having started the daemon) that would make spawned
    /// programs believe they run nested inside that launcher.
    pub env_remove: Vec<String>,
    /// Scrollback lines kept server-side. `None` = the default (10k).
    pub scrollback: Option<usize>,
}

/// Out-of-band events emitted by a session.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    Title {
        title: String,
    },
    Resized {
        cols: u16,
        rows: u16,
    },
    Exited {
        status: Option<i32>,
    },
    /// Shell phase change from OSC 133 marks (prompt <-> running).
    Shell {
        phase: ShellPhase,
    },
}

/// A live view onto a session, handed out by [`SessionManager::attach`].
pub struct Attachment {
    pub info: SessionInfo,
    /// ANSI escape stream reconstructing the session's current screen state
    /// (scrollback, visible grid, SGR attributes, cursor, title) in a fresh
    /// terminal of `info.cols` x `info.rows`.
    pub snapshot: Vec<u8>,
    pub output: tokio::sync::broadcast::Receiver<bytes::Bytes>,
    pub events: tokio::sync::broadcast::Receiver<SessionEvent>,
    pub input: tokio::sync::mpsc::Sender<bytes::Bytes>,
}

/// The final screen of an exited session, kept briefly so late attachers (a
/// client whose tab opened just as the process died — fast agent failures)
/// still see what happened instead of a blank pane.
#[derive(Clone)]
pub struct LastWords {
    /// The session's final info (`alive: false`, exit status recorded).
    pub info: SessionInfo,
    /// ANSI escape stream of the final screen, same form as an attach
    /// snapshot.
    pub snapshot: Vec<u8>,
}

/// Bound on remembered last-words snapshot bytes (oldest evicted first).
const LAST_WORDS_MAX_BYTES: usize = 2 * 1024 * 1024;

/// Owner of all PTY sessions. Sessions keep running while unattached; when a
/// session's child exits it is unregistered (tmux semantics: an exited shell
/// vanishes). Attached clients receive `SessionEvent::Exited` first.
pub struct SessionManager {
    sessions: Mutex<HashMap<SessionId, Arc<session::Session>>>,
    /// Final screens of recently exited sessions, oldest first.
    last_words: Mutex<VecDeque<LastWords>>,
}

/// Lock a mutex, recovering the guard if a panicking thread poisoned it.
pub(crate) fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SessionManager {
    pub fn new() -> Arc<Self> {
        Arc::new(SessionManager {
            sessions: Mutex::new(HashMap::new()),
            last_words: Mutex::new(VecDeque::new()),
        })
    }

    /// Spawn a new session and register it. Returns its initial info. The
    /// session unregisters itself when its child exits.
    pub fn spawn(self: &Arc<Self>, opts: SpawnOpts) -> anyhow::Result<SessionInfo> {
        let id = match &opts.id {
            Some(id) => {
                if lock_unpoisoned(&self.sessions).contains_key(id) {
                    return Err(anyhow!("session id {id} already in use"));
                }
                id.clone()
            }
            None => self.unused_id(),
        };
        let mgr = Arc::downgrade(self);
        let exit_id = id.clone();
        let on_exit = Box::new(move || {
            if let Some(m) = mgr.upgrade() {
                // Snapshot the final screen BEFORE unregistering, so there is
                // no moment where a session is neither live nor remembered.
                if let Some(session) = m.session(&exit_id) {
                    let attachment = session.attach();
                    m.remember_last_words(LastWords {
                        info: attachment.info,
                        snapshot: attachment.snapshot,
                    });
                }
                lock_unpoisoned(&m.sessions).remove(&exit_id);
            }
        });
        let session = session::Session::spawn(id.clone(), &opts, on_exit)
            .with_context(|| format!("failed to spawn session in {}", opts.cwd.display()))?;
        let info = session.info();
        lock_unpoisoned(&self.sessions).insert(id, session);
        Ok(info)
    }

    /// List all live sessions, sorted by creation time.
    pub fn list(&self) -> Vec<SessionInfo> {
        let mut infos: Vec<SessionInfo> = lock_unpoisoned(&self.sessions)
            .values()
            .map(|s| s.info())
            .collect();
        infos.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        infos
    }

    pub fn get(&self, id: &str) -> Option<SessionInfo> {
        self.session(id).map(|s| s.info())
    }

    /// Attach to a session: subscribe to live output/events and render a
    /// snapshot of the current screen state. Multiple concurrent attachments
    /// are allowed; sessions run fine with zero attachments.
    pub fn attach(&self, id: &str) -> anyhow::Result<Attachment> {
        let session = self
            .session(id)
            .ok_or_else(|| anyhow!("unknown session: {id}"))?;
        Ok(session.attach())
    }

    /// Resize the PTY and the server-side terminal together. The last resize
    /// wins for every attachment; a `Resized` event is broadcast.
    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> anyhow::Result<()> {
        if cols == 0 || rows == 0 {
            return Err(anyhow!("invalid size {cols}x{rows}"));
        }
        let session = self
            .session(id)
            .ok_or_else(|| anyhow!("unknown session: {id}"))?;
        session.resize(cols, rows)
    }

    /// Pid of the foreground process group on the session's tty
    /// (`tcgetpgrp` on the PTY master). `None` for unknown sessions or when
    /// the platform/tty cannot answer. For an idle shell this is the shell's
    /// own pid; while a command runs it is (the group leader of) that command.
    pub fn foreground_pid(&self, id: &str) -> Option<i32> {
        self.session(id).and_then(|s| s.foreground_pid())
    }

    /// Pin a user-chosen display name on a session (`renamed` becomes true;
    /// the name outranks every derived name from then on).
    pub fn rename(&self, id: &str, name: String) -> anyhow::Result<()> {
        let session = self
            .session(id)
            .ok_or_else(|| anyhow!("unknown session: {id}"))?;
        session.rename(name);
        Ok(())
    }

    /// Shell-integration marks (phase + command journal) for a session.
    pub fn marks(&self, id: &str) -> Option<Arc<Marks>> {
        self.session(id).map(|s| s.marks())
    }

    /// Plain-text rendering of the last `last_n` logical lines a session
    /// shows (what a human sees; scrollback included, wraps joined).
    pub fn screen_text(&self, id: &str, last_n: usize) -> Option<String> {
        self.session(id).map(|s| s.screen_text(last_n))
    }

    /// Type a command into a session's shell and wait for its outcome (see
    /// [`exec`] for the mode/queue semantics). Execs are serialized per
    /// session; a busy integrated shell queues up to `opts.queue_timeout`.
    pub async fn exec(&self, id: &str, opts: ExecOptions) -> Result<ExecOutcome, ExecError> {
        let session = self.session(id).ok_or(ExecError::SessionGone)?;
        exec::exec(session.marks(), session.input(), session.exec_lock(), opts).await
    }

    /// Signal the session's child to terminate (SIGHUP); the wait thread
    /// reaps it and the session unregisters itself. Killing an unknown or
    /// already-exited session is a no-op, so deletes are idempotent.
    pub fn kill(&self, id: &str) -> anyhow::Result<()> {
        if let Some(session) = self.session(id) {
            session.kill();
        }
        Ok(())
    }

    /// Terminate every live session (same SIGHUP-then-SIGKILL-escalation as
    /// [`kill`](Self::kill), per session). Returns how many were signalled.
    /// Idempotent. The daemon-wide "end all sessions" and shutdown paths use
    /// this; a caller that then exits the process must first wait
    /// [`KILL_ESCALATION_GRACE`] so the detached escalation can land.
    pub fn kill_all(&self) -> usize {
        let sessions: Vec<Arc<session::Session>> =
            lock_unpoisoned(&self.sessions).values().cloned().collect();
        for session in &sessions {
            session.kill();
        }
        sessions.len()
    }

    /// The final screen of a recently exited session, if still remembered
    /// (bounded by [`LAST_WORDS_MAX_BYTES`] of snapshot data).
    pub fn last_words(&self, id: &str) -> Option<LastWords> {
        lock_unpoisoned(&self.last_words)
            .iter()
            .rev()
            .find(|w| w.info.id == id)
            .cloned()
    }

    fn remember_last_words(&self, words: LastWords) {
        let mut remembered = lock_unpoisoned(&self.last_words);
        remembered.push_back(words);
        let mut total: usize = remembered.iter().map(|w| w.snapshot.len()).sum();
        while total > LAST_WORDS_MAX_BYTES && remembered.len() > 1 {
            if let Some(dropped) = remembered.pop_front() {
                total -= dropped.snapshot.len();
            }
        }
    }

    pub(crate) fn session(&self, id: &str) -> Option<Arc<session::Session>> {
        lock_unpoisoned(&self.sessions).get(id).cloned()
    }

    fn unused_id(&self) -> SessionId {
        let sessions = lock_unpoisoned(&self.sessions);
        loop {
            let id = format!("s-{:08x}", rand::random::<u32>());
            if !sessions.contains_key(&id) {
                return id;
            }
        }
    }
}
