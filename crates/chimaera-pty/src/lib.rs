//! chimaera-pty: the persistent terminal engine.
//!
//! A [`SessionManager`] owns long-lived PTY sessions. Each session runs a
//! command (or the user's interactive shell) attached to a native PTY and
//! mirrors every byte of output into a headless [`alacritty_terminal::Term`]
//! so the full screen state (scrollback, colors, cursor, title) survives with
//! zero attached clients. Clients are ephemeral: [`SessionManager::attach`]
//! returns a snapshot escape stream that reconstructs the terminal in a fresh
//! xterm.js instance, plus live output/event receivers and an input sender.

mod session;
mod snapshot;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
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
}

/// Out-of-band events emitted by a session.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    Title { title: String },
    Resized { cols: u16, rows: u16 },
    Exited { status: Option<i32> },
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

/// Owner of all PTY sessions. Sessions keep running while unattached and are
/// retained (with `alive = false`) after their child exits.
pub struct SessionManager {
    sessions: Mutex<HashMap<SessionId, Arc<session::Session>>>,
}

/// Lock a mutex, recovering the guard if a panicking thread poisoned it.
pub(crate) fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SessionManager {
    pub fn new() -> Arc<Self> {
        Arc::new(SessionManager {
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Spawn a new session and register it. Returns its initial info.
    pub fn spawn(&self, opts: SpawnOpts) -> anyhow::Result<SessionInfo> {
        let id = self.unused_id();
        let session = session::Session::spawn(id.clone(), &opts)
            .with_context(|| format!("failed to spawn session in {}", opts.cwd.display()))?;
        let info = session.info();
        lock_unpoisoned(&self.sessions).insert(id, session);
        Ok(info)
    }

    /// List all sessions (alive and exited), sorted by creation time.
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

    /// Signal the session's child to terminate (SIGHUP). The session record
    /// is kept (`alive = false`, `exit_status` set once reaped). Killing an
    /// already-dead session is a no-op.
    pub fn kill(&self, id: &str) -> anyhow::Result<()> {
        let session = self
            .session(id)
            .ok_or_else(|| anyhow!("unknown session: {id}"))?;
        session.kill();
        Ok(())
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
