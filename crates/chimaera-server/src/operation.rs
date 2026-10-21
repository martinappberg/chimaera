//! One bounded, cancellable operation per agent — the engine the managed
//! installs (`agent_setup`) and the sign-ins (`agent_auth`) share. It owns
//! what both routes do the same way: the progress record every window polls,
//! start-or-join under one lock, generation-scoped cancel, a capped output
//! buffer, and the process loop (its own process group; select on exit,
//! cancel and deadline; SIGTERM, a grace, then SIGKILL the whole group; on
//! Windows a Job Object stands in for the group and gets no SIGTERM). What
//! differs — the wire payload, the words, how a clean exit becomes a verdict —
//! stays in the module that owns the route, behind the [`Progress`] trait.
use std::{
    collections::HashMap,
    io,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, watch},
    task::JoinHandle,
};

use crate::{agents::AgentKind, lock};

/// How much of a process's text an operation keeps (the tail): enough for a
/// failure's cause, bounded for the daemon.
pub(crate) const OUTPUT_LIMIT: usize = 64 * 1024;
/// After a cancel or the deadline: SIGTERM, this long for EXIT traps and the
/// pipes to land what they hold, then SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}
impl Phase {
    /// A cancel in flight still counts: the record changes again.
    pub(crate) fn running(self) -> bool {
        matches!(self, Phase::Running | Phase::Cancelling)
    }
}

/// The record a window polls. It stays the owning module's own struct — a
/// route's wire shape is that route's contract — and the engine reads and
/// drives it through these.
pub(crate) trait Progress: Clone + Serialize + Send + 'static {
    fn id(&self) -> &str;
    fn phase(&self) -> Phase;
    fn set_phase(&mut self, phase: Phase);
    fn set_message(&mut self, message: &str);
    /// A chunk the process wrote to stdout or stderr, in the order read.
    fn on_output(&mut self, bytes: &[u8]);
    fn running(&self) -> bool {
        self.phase().running()
    }
}

pub(crate) struct Operation<P> {
    progress: Mutex<P>,
    cancel: watch::Sender<bool>,
    /// Lines for the process's stdin (a pasted code), one at a time.
    input: mpsc::Sender<String>,
    lines: Mutex<Option<mpsc::Receiver<String>>>,
}

impl<P: Progress> Operation<P> {
    fn new(progress: P) -> Arc<Self> {
        let (input, lines) = mpsc::channel(1);
        Arc::new(Self {
            progress: Mutex::new(progress),
            cancel: watch::channel(false).0,
            input,
            lines: Mutex::new(Some(lines)),
        })
    }
    pub(crate) fn snapshot(&self) -> P {
        lock(&self.progress).clone()
    }
    /// Read the record under its lock without cloning it — briefly, never
    /// across an await.
    pub(crate) fn read<T>(&self, f: impl FnOnce(&P) -> T) -> T {
        f(&lock(&self.progress))
    }
    /// Change the record under its lock — briefly, never across an await.
    pub(crate) fn update<T>(&self, f: impl FnOnce(&mut P) -> T) -> T {
        f(&mut lock(&self.progress))
    }
    pub(crate) fn set_message(&self, message: &str) {
        self.update(|p| p.set_message(message));
    }
    /// A line for the process's stdin. `Err` while the previous one is still
    /// being written.
    pub(crate) fn send_line(&self, line: String) -> Result<(), ()> {
        self.input.try_send(line).map_err(|_| ())
    }

    /// Run the process as this operation and say how it ended; the owning
    /// module turns that into a phase and words. Cancel and the deadline
    /// both stop the whole group.
    pub(crate) async fn run(self: &Arc<Self>, mut run: Run) -> Exit {
        let mut cancel = self.cancel.subscribe();
        // A cancel that arrived before anything was started.
        if *cancel.borrow() {
            return Exit::NotStarted;
        }
        run.command
            .stdin(match run.stdin {
                Stdin::Closed => Stdio::null(),
                Stdin::Lines { .. } => Stdio::piped(),
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        run.command.process_group(0);
        #[cfg(windows)]
        run.command
            .creation_flags(chimaera_core::proc::TREE_CREATION_FLAGS);
        let mut child = match run.command.spawn() {
            Ok(child) => child,
            Err(err) => return Exit::SpawnFailed(err),
        };
        let Some(pid) = child.id() else {
            return Exit::SpawnFailed(io::Error::other("the process ended before it started"));
        };
        let group = match chimaera_core::proc::ProcessTree::adopt(pid) {
            Ok(tree) => Group(Some(tree)),
            // A tree the engine can't stop is not run: kill_on_drop ends
            // the leader as `child` drops.
            Err(err) => return Exit::SpawnFailed(err),
        };
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let mut stdin = child.stdin.take();
        let mut lines = lock(&self.lines).take();
        // The pipes are read on their own task, so a chunk never waits on
        // the exit whichever `OutputEnd` the operation chose.
        let mut reads: Option<JoinHandle<()>> = Some(tokio::spawn({
            let op = self.clone();
            async move {
                tokio::join!(drain(stdout, &op), drain(stderr, &op));
            }
        }));
        let exit = {
            // The reader handle stays in `reads` (borrowed, never moved): a
            // cancel that drops this future mid-drain must still find it, to
            // give the pipes their grace before the group is killed.
            let wait = async {
                if let OutputEnd::Drained = run.output {
                    if let Some(handle) = reads.as_mut() {
                        let _ = handle.await;
                    }
                    reads = None;
                }
                child.wait().await
            };
            tokio::pin!(wait);
            let stopped = async {
                if !*cancel.borrow() {
                    let _ = cancel.changed().await;
                }
            };
            tokio::pin!(stopped);
            let deadline = tokio::time::sleep(run.deadline);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    result = &mut wait => break Some(result),
                    _ = &mut stopped => break None,
                    _ = &mut deadline => break None,
                    line = next_line(&mut lines), if stdin.is_some() => {
                        let Some(pipe) = stdin.as_mut() else { continue };
                        let write = async {
                            pipe.write_all(line.as_bytes()).await?;
                            pipe.write_all(b"\n").await?;
                            pipe.flush().await
                        };
                        if write.await.is_err() {
                            if let Stdin::Lines { closed } = run.stdin {
                                self.set_message(closed);
                            }
                        }
                    }
                }
            }
        };
        match exit {
            Some(result) => {
                // The leader is reaped: never signal a possibly recycled
                // group id.
                group.release();
                if let OutputEnd::Grace(grace) = run.output {
                    settle_reads(&mut reads, grace).await;
                }
                Exit::Exited(result.ok().and_then(|s| s.code()))
            }
            None => {
                let cancelled = *cancel.borrow();
                // SIGTERM first so EXIT traps can clean up, the pipes a moment
                // to land the rest, then the whole group goes. Wait AFTER the
                // kill so the group id cannot be recycled in between.
                // Windows has no SIGTERM: a closed stdin is the only notice
                // before the grace runs out and the job is killed.
                group.terminate();
                #[cfg(windows)]
                drop(stdin.take());
                settle_reads(&mut reads, TERM_GRACE).await;
                drop(group);
                let code = child.wait().await.ok().and_then(|s| s.code());
                if cancelled {
                    Exit::Cancelled(code)
                } else {
                    Exit::TimedOut(code)
                }
            }
        }
    }
}

/// Every operation of one kind: at most one per agent, and the last result
/// kept until a new start replaces it or the daemon restarts.
pub(crate) struct Operations<P>(Mutex<HashMap<AgentKind, Arc<Operation<P>>>>);

impl<P> Default for Operations<P> {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

impl<P: Progress> Operations<P> {
    pub(crate) fn get(&self, kind: AgentKind) -> Option<Arc<Operation<P>>> {
        lock(&self.0).get(&kind).cloned()
    }
    pub(crate) fn snapshot(&self, kind: AgentKind) -> Option<P> {
        self.get(kind).map(|op| op.snapshot())
    }
    /// The catalog row's summary of this agent's operation.
    pub(crate) fn summary(&self, kind: AgentKind) -> Option<serde_json::Value> {
        self.get(kind).map(|op| {
            op.read(|p| json!({"id": p.id(), "running": p.running(), "phase": p.phase()}))
        })
    }
    /// The operation a POST joins instead of starting another: one still
    /// running (a second window), or the finished one this same request
    /// started (a retry after a lost reply).
    pub(crate) fn joinable(&self, kind: AgentKind, request_id: &str) -> Option<P> {
        self.get(kind)
            .and_then(|op| op.read(|p| joins(p, request_id).then(|| p.clone())))
    }
    /// Register `progress` as the agent's operation — unless one is joinable,
    /// which comes back instead. Decided under the lock: two first clicks
    /// never both start.
    pub(crate) fn start(&self, kind: AgentKind, progress: P) -> Result<Arc<Operation<P>>, P> {
        let mut map = lock(&self.0);
        if let Some(existing) = map.get(&kind) {
            if let Some(p) = existing.read(|p| joins(p, progress.id()).then(|| p.clone())) {
                return Err(p);
            }
        }
        let op = Operation::new(progress);
        map.insert(kind, op.clone());
        Ok(op)
    }
    /// Ask the operation `id` to stop, with `stopping` as its message. Only
    /// the current generation: an old dialog's cancel never reaches a newer
    /// retry.
    pub(crate) fn cancel(
        &self,
        kind: AgentKind,
        id: &str,
        stopping: &str,
    ) -> Result<P, CancelRefused> {
        let op = self.get(kind).ok_or(CancelRefused::Gone)?;
        let mut p = lock(&op.progress);
        if p.id() != id {
            return Err(CancelRefused::Newer);
        }
        if p.running() {
            p.set_phase(Phase::Cancelling);
            p.set_message(stopping);
            op.cancel.send_replace(true);
        }
        Ok(p.clone())
    }
}

/// Whether a POST carrying `request_id` observes `p` instead of starting
/// anew: still running, or the finished operation this same request started.
fn joins<P: Progress>(p: &P, request_id: &str) -> bool {
    p.running() || p.id() == request_id
}

pub(crate) enum CancelRefused {
    /// No operation for this agent (none yet, or the daemon restarted).
    Gone,
    /// A newer operation has replaced the one named.
    Newer,
}

// ---------------------------------------------------------------- routes --

/// A request id the client made up: short, plain, the same on a retry.
pub(crate) fn valid_request_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
/// A route's `{id}` as a catalog agent.
pub(crate) fn kind(id: &str) -> Option<AgentKind> {
    AgentKind::parse(id).filter(|k| AgentKind::ALL.contains(k))
}
pub(crate) fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

// ---------------------------------------------------------------- output --

/// Append `text` to a buffer that keeps its last `OUTPUT_LIMIT` bytes (cut
/// on a char boundary). True when something was cut.
pub(crate) fn append_capped(buf: &mut String, text: &str) -> bool {
    buf.push_str(text);
    if buf.len() <= OUTPUT_LIMIT {
        return false;
    }
    let mut cut = buf.len() - OUTPUT_LIMIT;
    while !buf.is_char_boundary(cut) {
        cut += 1;
    }
    buf.drain(..cut);
    true
}

/// Terminal output minus its controls: ANSI CSI/OSC sequences and other
/// control characters, newlines and tabs kept.
pub(crate) fn strip_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: parameters/intermediates, then one final byte.
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: until BEL or ST (ESC \).
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            }
            continue;
        }
        if !c.is_control() || matches!(c, '\n' | '\t') {
            out.push(c);
        }
    }
    out
}

// --------------------------------------------------------------- process --

/// A process to run as the operation. The owning module configures the
/// command (program, argv, cwd, env); the engine adds the stdio and the
/// process group.
pub(crate) struct Run {
    pub(crate) command: tokio::process::Command,
    pub(crate) stdin: Stdin,
    pub(crate) deadline: Duration,
    pub(crate) output: OutputEnd,
}

pub(crate) enum Stdin {
    /// `/dev/null`.
    Closed,
    /// Kept open for [`Operation::send_line`]; `closed` is the message when
    /// a line could not be written (the process stopped reading).
    Lines { closed: &'static str },
}

/// When the operation's result is read.
pub(crate) enum OutputEnd {
    /// Once the pipes close: the output IS the result (an installer script),
    /// and the leader stays unreaped while descendants hold them, so a
    /// cancel cannot signal a recycled group id.
    Drained,
    /// As soon as the leader exits, the pipes given this long to land what
    /// they still hold: the verdict comes from elsewhere (a status command),
    /// and a helper the process left holding stdout must not turn a success
    /// into a deadline failure.
    Grace(Duration),
}

pub(crate) enum Exit {
    /// Cancelled before the process was started.
    NotStarted,
    SpawnFailed(io::Error),
    /// The process ended on its own, with its exit code (none for a signal).
    Exited(Option<i32>),
    /// Stopped on a cancel, after the group was signalled.
    Cancelled(Option<i32>),
    /// Stopped at the deadline, after the group was signalled.
    TimedOut(Option<i32>),
}

/// `argv` in `cwd` with exactly `env` (the names in `remove` taken out of
/// the inherited environment).
pub(crate) fn command(
    argv: &[String],
    cwd: &Path,
    env: Vec<(String, String)>,
    remove: Vec<String>,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(cwd).envs(env);
    for name in remove {
        cmd.env_remove(name);
    }
    cmd
}

/// The process group (Windows: the Job Object), killed outright when
/// dropped. Release it once the leader is reaped — its id may be someone
/// else's by then.
struct Group(Option<chimaera_core::proc::ProcessTree>);
impl Group {
    fn terminate(&self) {
        if let Some(tree) = &self.0 {
            tree.terminate();
        }
    }
    /// Let go without killing: a dropped tree never signals anything.
    fn release(mut self) {
        self.0 = None;
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        if let Some(tree) = &self.0 {
            tree.kill();
        }
    }
}

/// Give the pipe readers `grace` to land what the pipes still hold, then end
/// them: a helper the process left behind holding stdout must not keep this
/// task, the pipes and the record alive for its own lifetime.
async fn settle_reads(reads: &mut Option<JoinHandle<()>>, grace: Duration) {
    if let Some(handle) = reads.as_mut() {
        if tokio::time::timeout(grace, &mut *handle).await.is_err() {
            handle.abort();
        }
    }
    *reads = None;
}

async fn drain<P: Progress>(mut pipe: impl AsyncRead + Unpin, op: &Operation<P>) {
    let mut buf = [0; 4096];
    while let Ok(n) = pipe.read(&mut buf).await {
        if n == 0 {
            break;
        }
        op.update(|p| p.on_output(&buf[..n]));
    }
}

/// The next stdin line, or forever when there is no channel (the sender
/// lives as long as the operation, so a closed channel is the same).
async fn next_line(lines: &mut Option<mpsc::Receiver<String>>) -> String {
    match lines {
        Some(rx) => match rx.recv().await {
            Some(line) => line,
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_short_plain_and_never_empty() {
        assert!(valid_request_id("install-1"));
        assert!(valid_request_id(&"a".repeat(128)));
        assert!(!valid_request_id(""));
        assert!(!valid_request_id(&"a".repeat(129)));
        assert!(!valid_request_id("bad id!"));
        assert!(!valid_request_id("a/b"));
    }

    #[test]
    fn the_output_cap_cuts_the_head_on_a_char_boundary() {
        let mut buf = String::new();
        assert!(!append_capped(&mut buf, "head"));
        let cut = append_capped(&mut buf, &"é".repeat(OUTPUT_LIMIT));
        assert!(cut);
        assert!(buf.len() <= OUTPUT_LIMIT);
        assert!(buf.chars().all(|c| c == 'é'));
        assert!(buf.ends_with('é'));
    }

    #[test]
    fn controls_go_text_stays() {
        assert_eq!(
            strip_controls(
                "\x1b[31merror:\x1b[0m x\ty\n\x1b]8;;https://a.test\x07link\x1b]8;;\x07\x07"
            ),
            "error: x\ty\nlink"
        );
    }
}
