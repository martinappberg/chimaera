//! Session internals: PTY plumbing, the headless terminal, and the pump
//! threads that keep them in sync.
//!
//! Lock ordering (to avoid deadlocks): `term` -> `master` -> `state` ->
//! `title`. Never acquire an earlier lock while holding a later one.

use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use anyhow::Context;
use bytes::Bytes;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tokio::sync::{broadcast, mpsc};

use crate::{
    lock_unpoisoned, snapshot, Attachment, SessionEvent, SessionId, SessionInfo, SpawnOpts,
};

/// Scrollback history kept per session.
const SCROLLBACK_LINES: usize = 10_000;
/// Capacity of the live-output broadcast channel (in chunks, not bytes).
const OUTPUT_CHANNEL_CAPACITY: usize = 4096;
/// Capacity of the session-event broadcast channel.
const EVENT_CHANNEL_CAPACITY: usize = 256;
/// Capacity of the stdin mpsc channel.
const INPUT_CHANNEL_CAPACITY: usize = 256;

/// Minimal `Dimensions` impl for constructing/resizing the headless `Term`.
struct TermDimensions {
    cols: u16,
    rows: u16,
}

impl Dimensions for TermDimensions {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// Mutable per-session state guarded by one mutex.
struct SessionState {
    /// Display name; renameable for the session's whole life.
    name: String,
    /// Whether `name` was pinned explicitly (spawn name or user rename).
    renamed: bool,
    cols: u16,
    rows: u16,
    alive: bool,
    exit_status: Option<i32>,
}

/// Receives events from the headless `Term` (OSC title changes, bell, ...)
/// while the vte parser is advancing it.
#[derive(Clone)]
pub(crate) struct EventProxy {
    events_tx: broadcast::Sender<SessionEvent>,
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        match event {
            TermEvent::Title(title) => {
                *lock_unpoisoned(&self.title) = Some(title.clone());
                let _ = self.events_tx.send(SessionEvent::Title { title });
            }
            TermEvent::ResetTitle => {
                *lock_unpoisoned(&self.title) = None;
            }
            // Bell is intentionally ignored for now.
            TermEvent::Bell => {}
            _ => {}
        }
    }
}

pub(crate) struct Session {
    id: SessionId,
    cwd: PathBuf,
    created_at: u64,
    /// OS pid of the direct child, captured at spawn.
    child_pid: Option<u32>,
    term: Arc<Mutex<Term<EventProxy>>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    state: Arc<Mutex<SessionState>>,
    title: Arc<Mutex<Option<String>>>,
    input_tx: mpsc::Sender<Bytes>,
    output_tx: broadcast::Sender<Bytes>,
    events_tx: broadcast::Sender<SessionEvent>,
}

impl Session {
    pub(crate) fn spawn(
        id: SessionId,
        opts: &SpawnOpts,
        on_exit: Box<dyn FnOnce() + Send + 'static>,
    ) -> anyhow::Result<Arc<Session>> {
        let cwd = opts.cwd.clone();
        let explicit_name = opts.name.clone().filter(|n| !n.is_empty());
        let renamed = explicit_name.is_some();
        let name = explicit_name.unwrap_or_else(|| {
            cwd.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| cwd.to_string_lossy().into_owned())
        });

        let pty = native_pty_system()
            .openpty(PtySize {
                rows: opts.rows,
                cols: opts.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty failed")?;

        let mut cmd = match &opts.command {
            Some(argv) => {
                let program = argv
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("empty command"))?;
                let mut cmd = CommandBuilder::new(program);
                cmd.args(&argv[1..]);
                cmd
            }
            None => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
                // A shell whose stdin is a PTY starts in interactive mode.
                CommandBuilder::new(shell)
            }
        };
        cmd.cwd(&cwd);
        cmd.env("TERM", "xterm-256color");
        for (name, value) in &opts.env {
            cmd.env(name, value);
        }

        let mut child = pty
            .slave
            .spawn_command(cmd)
            .context("failed to spawn command in pty")?;
        // Drop the slave side so the master reader sees EOF when the child
        // (and any descendants holding the tty) exit.
        drop(pty.slave);
        let master = pty.master;

        let mut reader = master
            .try_clone_reader()
            .context("failed to clone pty reader")?;
        let mut writer = master.take_writer().context("failed to take pty writer")?;
        let killer = child.clone_killer();
        let child_pid = child.process_id();

        let (output_tx, _) = broadcast::channel::<Bytes>(OUTPUT_CHANNEL_CAPACITY);
        let (events_tx, _) = broadcast::channel::<SessionEvent>(EVENT_CHANNEL_CAPACITY);
        let (input_tx, mut input_rx) = mpsc::channel::<Bytes>(INPUT_CHANNEL_CAPACITY);

        let title = Arc::new(Mutex::new(None));
        let proxy = EventProxy {
            events_tx: events_tx.clone(),
            title: Arc::clone(&title),
        };
        let term_config = TermConfig {
            scrolling_history: SCROLLBACK_LINES,
            ..TermConfig::default()
        };
        let dims = TermDimensions {
            cols: opts.cols,
            rows: opts.rows,
        };
        let term = Arc::new(Mutex::new(Term::new(term_config, &dims, proxy)));

        let state = Arc::new(Mutex::new(SessionState {
            name: name.clone(),
            renamed,
            cols: opts.cols,
            rows: opts.rows,
            alive: true,
            exit_status: None,
        }));

        // Reader thread: pump PTY output into the headless Term and the live
        // broadcast channel. Advancing the parser and broadcasting happen
        // under the term lock so attach() (which subscribes and renders the
        // snapshot under the same lock) sees each chunk exactly once: either
        // in the snapshot or on the live stream, never both or neither.
        {
            let term = Arc::clone(&term);
            let output_tx = output_tx.clone();
            let id = id.clone();
            std::thread::Builder::new()
                .name(format!("pty-read-{id}"))
                .spawn(move || {
                    let mut parser: Processor<StdSyncHandler> = Processor::new();
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let mut term = lock_unpoisoned(&term);
                                parser.advance(&mut *term, &buf[..n]);
                                let _ = output_tx.send(Bytes::copy_from_slice(&buf[..n]));
                            }
                            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                            Err(e) => {
                                tracing::debug!(session = %id, error = %e, "pty reader stopped");
                                break;
                            }
                        }
                    }
                    tracing::debug!(session = %id, "pty reader thread exited");
                })
                .context("failed to spawn pty reader thread")?;
        }

        // Writer thread: drain the input channel into the PTY master. The
        // portable-pty writer is blocking, so this lives on its own thread.
        {
            let id = id.clone();
            std::thread::Builder::new()
                .name(format!("pty-write-{id}"))
                .spawn(move || {
                    while let Some(data) = input_rx.blocking_recv() {
                        if let Err(e) = writer.write_all(&data).and_then(|()| writer.flush()) {
                            tracing::debug!(session = %id, error = %e, "pty writer stopped");
                            break;
                        }
                    }
                })
                .context("failed to spawn pty writer thread")?;
        }

        // Wait thread: reap the child, record its exit status, publish
        // Exited, then hand control back to the owner (which unregisters the
        // session — exited shells vanish, tmux-style).
        {
            let state = Arc::clone(&state);
            let events_tx = events_tx.clone();
            let id = id.clone();
            std::thread::Builder::new()
                .name(format!("pty-wait-{id}"))
                .spawn(move || {
                    let status = match child.wait() {
                        Ok(status) => {
                            if status.signal().is_some() {
                                None
                            } else {
                                Some(status.exit_code() as i32)
                            }
                        }
                        Err(e) => {
                            tracing::warn!(session = %id, error = %e, "failed to wait for child");
                            None
                        }
                    };
                    {
                        let mut state = lock_unpoisoned(&state);
                        state.alive = false;
                        state.exit_status = status;
                    }
                    let _ = events_tx.send(SessionEvent::Exited { status });
                    tracing::info!(session = %id, exit_status = ?status, "session exited");
                    // Give the reader thread a beat to drain the PTY's final
                    // bytes into the grid: the owner snapshots the screen as
                    // the session's last words before unregistering it.
                    std::thread::sleep(std::time::Duration::from_millis(60));
                    on_exit();
                })
                .context("failed to spawn child wait thread")?;
        }

        tracing::info!(session = %id, name = %name, cwd = %cwd.display(), "spawned session");

        Ok(Arc::new(Session {
            id,
            cwd,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            child_pid,
            term,
            master: Mutex::new(master),
            killer: Mutex::new(killer),
            state,
            title,
            input_tx,
            output_tx,
            events_tx,
        }))
    }

    pub(crate) fn info(&self) -> SessionInfo {
        let (name, renamed, cols, rows, alive, exit_status) = {
            let state = lock_unpoisoned(&self.state);
            (
                state.name.clone(),
                state.renamed,
                state.cols,
                state.rows,
                state.alive,
                state.exit_status,
            )
        };
        SessionInfo {
            id: self.id.clone(),
            name,
            cwd: self.cwd.clone(),
            cols,
            rows,
            created_at: self.created_at,
            alive,
            exit_status,
            title: lock_unpoisoned(&self.title).clone(),
            pid: self.child_pid,
            renamed,
        }
    }

    /// Pin a user-chosen display name; it stays authoritative over every
    /// derived name (OSC titles, agent titles, foreground commands) for the
    /// session's whole life.
    pub(crate) fn rename(&self, name: String) {
        let mut state = lock_unpoisoned(&self.state);
        state.name = name;
        state.renamed = true;
    }

    /// Foreground process group on the tty (`tcgetpgrp` on the master fd).
    /// `None` when the platform or tty cannot answer.
    pub(crate) fn foreground_pid(&self) -> Option<i32> {
        #[cfg(unix)]
        {
            lock_unpoisoned(&self.master).process_group_leader()
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    pub(crate) fn attach(&self) -> Attachment {
        // Hold the term lock across subscribe + render so no output chunk can
        // land between the snapshot and the live stream (see reader thread).
        let term = lock_unpoisoned(&self.term);
        let output = self.output_tx.subscribe();
        let events = self.events_tx.subscribe();
        let title = lock_unpoisoned(&self.title).clone();
        let snapshot = snapshot::render_snapshot(&term, title.as_deref());
        drop(term);
        Attachment {
            info: self.info(),
            snapshot,
            output,
            events,
            input: self.input_tx.clone(),
        }
    }

    pub(crate) fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        {
            let mut term = lock_unpoisoned(&self.term);
            term.resize(TermDimensions { cols, rows });
            lock_unpoisoned(&self.master)
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .context("failed to resize pty")?;
        }
        {
            let mut state = lock_unpoisoned(&self.state);
            state.cols = cols;
            state.rows = rows;
        }
        let _ = self.events_tx.send(SessionEvent::Resized { cols, rows });
        Ok(())
    }

    /// Best-effort child termination (SIGHUP via portable-pty). Reaping and
    /// state bookkeeping happen on the wait thread. Killing a session whose
    /// child already exited is a no-op.
    pub(crate) fn kill(&self) {
        if !lock_unpoisoned(&self.state).alive {
            return;
        }
        if let Err(e) = lock_unpoisoned(&self.killer).kill() {
            // Racing with a natural exit is fine; the wait thread has the
            // authoritative outcome either way.
            tracing::debug!(session = %self.id, error = %e, "kill failed (child likely already exited)");
        }
    }

    /// Test-only access to the live headless terminal.
    #[cfg(test)]
    pub(crate) fn with_term<R>(&self, f: impl FnOnce(&Term<EventProxy>) -> R) -> R {
        let term = lock_unpoisoned(&self.term);
        f(&term)
    }
}
