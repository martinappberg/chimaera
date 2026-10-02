//! Line-oriented JSON transport over a child process's stdio.
//!
//! Shared by both agent clients: Claude's stream-json and Codex's app-server
//! are the same framing (one JSON object per line on stdin/stdout). stderr is
//! kept as a small tail ring — when a handshake fails, those bytes are the
//! only diagnostic the daemon has.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// stderr kept for diagnostics only — a runaway child must not grow memory.
const STDERR_TAIL_BUDGET: usize = 8 * 1024;
// Empty lines still allocate ring entries; byte limits alone do not bound them.
const STDERR_TAIL_LINES: usize = 256;
/// Hard ceiling on a single stdout line. A real stream-json / app-server frame
/// (diffs, small inline images) fits well under this; a child that emits bytes
/// without a newline (binary garbage, a wedged CLI) must never grow the read
/// buffer without bound on a shared login node — the overflow is discarded.
const MAX_STDOUT_LINE_BYTES: usize = 8 * 1024 * 1024;
/// stderr is diagnostics only; a much tighter per-line cap suffices.
const MAX_STDERR_LINE_BYTES: usize = 16 * 1024;

/// A length-capped async line reader. Unlike [`tokio::io::Lines`], the buffer
/// for one line can never exceed `max`: once a line reaches the cap the reader
/// keeps consuming (and discarding) input until the next newline, so a child
/// that never emits `\n` cannot blow the daemon's RSS budget.
struct CappedLines<R> {
    reader: BufReader<R>,
    max: usize,
}

impl<R: AsyncRead + Unpin> CappedLines<R> {
    fn new(inner: R, max: usize) -> Self {
        Self {
            reader: BufReader::new(inner),
            max,
        }
    }

    /// Next line without its trailing `\n`. `Ok(None)` = EOF. Invalid UTF-8 is
    /// replaced lossily rather than failing the session.
    async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        let mut buf: Vec<u8> = Vec::new();
        let mut overflowed = false;
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if buf.is_empty() && !overflowed {
                    return Ok(None);
                }
                break;
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    push_capped(&mut buf, &available[..pos], self.max, &mut overflowed);
                    self.reader.consume(pos + 1);
                    break;
                }
                None => {
                    let len = available.len();
                    push_capped(&mut buf, available, self.max, &mut overflowed);
                    self.reader.consume(len);
                }
            }
        }
        if overflowed {
            tracing::warn!(cap = self.max, "agent output line exceeded cap; truncated");
        }
        Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
    }
}

/// Append `chunk` to `buf` but never past `max`; flag once truncation begins.
fn push_capped(buf: &mut Vec<u8>, chunk: &[u8], max: usize, overflowed: &mut bool) {
    let room = max.saturating_sub(buf.len());
    if chunk.len() > room {
        buf.extend_from_slice(&chunk[..room]);
        *overflowed = true;
    } else {
        buf.extend_from_slice(chunk);
    }
}

/// A spawned agent process speaking newline-delimited JSON on stdio. Holds
/// its three independently-owned halves so framing, spawn, and shutdown have
/// exactly one implementation, shared by the probe clients (which use the
/// whole child) and the driver harness (which [`split`](Self::split)s it).
pub struct JsonlChild {
    sink: JsonlSink,
    stream: JsonlStream,
    guard: ChildGuard,
}

impl JsonlChild {
    pub fn spawn(
        bin: &str,
        args: &[String],
        cwd: &Path,
        env: &[(String, String)],
        env_remove: &[String],
    ) -> Result<Self> {
        Self::spawn_controlled(bin, args, cwd, env, env_remove, None)
    }
    pub(crate) fn spawn_controlled(
        bin: &str,
        args: &[String],
        cwd: &Path,
        env: &[(String, String)],
        env_remove: &[String],
        control: Option<&Arc<ProcessControl>>,
    ) -> Result<Self> {
        anyhow::ensure!(
            control.is_none_or(|c| !c.fenced()),
            "managed process was fenced before spawn"
        );
        let mut cmd = Command::new(bin);
        #[cfg(unix)]
        if control.is_some() {
            cmd.process_group(0);
        }

        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The daemon owns the child's lifetime; a dropped session must
            // never leave an orphaned agent billing in the background.
            .kill_on_drop(true);
        for (k, v) in env {
            cmd.env(k, v);
        }
        // Strip inherited launcher-context vars AFTER the adds (disjoint sets
        // today, but removal winning is the safe invariant).
        for k in env_remove {
            cmd.env_remove(k);
        }
        // Startup-only supervisor input is consumed by the daemon, never an agent.
        cmd.env_remove("CHIMAERA_SUPERVISOR_CLEANUP_FD");
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn {bin}"))?;

        let stdin = child.stdin.take().context("child stdin unavailable")?;
        let stdout = child.stdout.take().context("child stdout unavailable")?;
        let stderr = child.stderr.take().context("child stderr unavailable")?;

        let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::default();
        let tail = Arc::clone(&stderr_tail);
        let stderr_task = tokio::spawn(async move {
            let mut lines = CappedLines::new(stderr, MAX_STDERR_LINE_BYTES);
            while let Ok(Some(line)) = lines.next_line().await {
                let mut tail = tail.lock().expect("stderr tail lock");
                tail.push_back(line);
                let mut total: usize = tail.iter().map(|l| l.len()).sum();
                while total > STDERR_TAIL_BUDGET || tail.len() > STDERR_TAIL_LINES {
                    match tail.pop_front() {
                        Some(dropped) => total -= dropped.len(),
                        None => break,
                    }
                }
            }
        });

        let child = Arc::new(Mutex::new(child));
        if let Some(control) = control {
            control.attach(&child);
        }
        Ok(Self {
            sink: JsonlSink { stdin },
            stream: JsonlStream {
                lines: CappedLines::new(stdout, MAX_STDOUT_LINE_BYTES),
            },
            guard: ChildGuard {
                managed_group: control.is_some(),
                child,
                stderr_tail,
                stderr_task,
            },
        })
    }

    /// Write one JSON value as a single line and flush it.
    pub async fn send(&mut self, value: &Value) -> Result<()> {
        self.sink.send(value).await
    }

    /// Next JSON line from stdout, bounded by `timeout`. `Ok(None)` means EOF
    /// (child closed stdout). Non-JSON lines are skipped with a warning rather
    /// than failing the session — one stray diagnostic line must not kill a
    /// conversation.
    pub async fn recv(&mut self, timeout: Duration) -> Result<Option<Value>> {
        tokio::time::timeout(timeout, self.stream.next())
            .await
            .context("timed out waiting for agent output")?
    }

    /// Last stderr lines, for handshake-failure diagnostics.
    pub fn stderr_tail(&self) -> String {
        self.guard.stderr_tail()
    }

    /// Close stdin (the polite shutdown for both protocols), give the child a
    /// grace period to exit, then kill it.
    pub async fn shutdown(self, grace: Duration) -> Result<Option<i32>> {
        drop(self.sink);
        Ok(self.guard.shutdown(grace).await)
    }

    /// Split into independently-owned halves so a driver can `select!` over
    /// inbound frames and outbound commands without fighting the borrow of a
    /// single struct.
    pub fn split(self) -> (JsonlSink, JsonlStream, ChildGuard) {
        (self.sink, self.stream, self.guard)
    }
}

/// Write half of a split [`JsonlChild`].
pub struct JsonlSink {
    stdin: ChildStdin,
}

impl JsonlSink {
    pub async fn send(&mut self, value: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .context("agent stdin write")?;
        self.stdin.flush().await.context("agent stdin flush")?;
        Ok(())
    }
}

/// Read half of a split [`JsonlChild`]. stderr diagnostics stay with the
/// [`ChildGuard`] half, so the split read loop carries only stdout framing.
pub struct JsonlStream {
    lines: CappedLines<ChildStdout>,
}

impl JsonlStream {
    /// Next JSON frame, no deadline — an idle agent is silent for as long as
    /// the user thinks. `Ok(None)` = EOF.
    pub async fn next(&mut self) -> Result<Option<Value>> {
        loop {
            let line = self.lines.next_line().await.context("agent stdout read")?;
            let Some(line) = line else {
                return Ok(None);
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&line) {
                Ok(value) => return Ok(Some(value)),
                Err(err) => {
                    tracing::warn!(%err, line = %truncate(&line, 200), "skipping non-JSON agent output line");
                }
            }
        }
    }
}

/// Bound on waiting for the stderr reader to drain after the child died —
/// EOF is immediate once the last write end closes; the bound covers a
/// grandchild that inherited the fd and outlives the agent.
const STDERR_SETTLE: Duration = Duration::from_secs(1);

/// Owns the child for lifecycle: bounded shutdown, kill, stderr diagnostics.
pub struct ChildGuard {
    managed_group: bool,
    child: Arc<Mutex<Child>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    stderr_task: tokio::task::JoinHandle<()>,
}

impl ChildGuard {
    /// Ask the child to stop: SIGTERM, the stop both CLIs handle. Claude's
    /// handler ends its background tasks (Bash shells and Monitors, which it
    /// starts DETACHED, in their own sessions) and exits in ~0.3 s; a closed
    /// stdin alone left its background shell running past the grace, stopped
    /// only the Monitor, and woke a (billed) turn to react to that stop — and
    /// the grace's SIGKILL then orphans whatever is left on the host. Codex
    /// exits 0 either way. Live-probed claude 2.1.281 / codex 0.156.1
    /// (PROTOCOL.md Pass 32). A no-op once the child has been reaped (tokio
    /// clears the pid then, so a recycled pid is never signalled).
    pub fn terminate(&self) {
        if let Some(pid) = self.child.lock().expect("child lifecycle lock").id() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
    }

    /// Give the child a grace period after sinks were dropped, then kill. The
    /// harness's only reap path — an unbounded wait can't leak a lingering
    /// child (a normally-exiting one returns its status within the grace).
    pub async fn shutdown(self, grace: Duration) -> Option<i32> {
        self.shutdown_with_stderr(grace).await.0
    }

    /// Reap like [`Self::shutdown`] and return the SETTLED stderr tail with
    /// the status: the reader task gets a bounded moment to drain the pipe
    /// after the child died. A fast-crashing child otherwise loses the race
    /// and its failure diagnostics read as an empty tail.
    pub async fn shutdown_with_stderr(mut self, grace: Duration) -> (Option<i32>, String) {
        if self.managed_group {
            // Wait for the child to exit or the deadline, whichever comes
            // first (a clean stop takes ~0.3 s; every stop, view switch and
            // rewind used to wait the full two seconds), then end whatever
            // else is left in its process group.
            // The exit is observed without reaping (WNOWAIT), so the group id
            // cannot be recycled before the group kill below.
            let deadline = Instant::now() + grace.min(Duration::from_secs(2));
            let pid = self.child.lock().expect("child lifecycle lock").id();
            while Instant::now() < deadline && !pid.is_some_and(exited_unreaped) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let child = self.child.lock().expect("child lifecycle lock");
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
        let deadline = Instant::now() + grace;
        let status = loop {
            let observed = self.child.lock().expect("child lifecycle lock").try_wait();
            match observed {
                Ok(Some(status)) => break status.code(),
                Err(_) => break None,
                Ok(None) => {}
            }
            if Instant::now() >= deadline {
                let _ = self
                    .child
                    .lock()
                    .expect("child lifecycle lock")
                    .start_kill();
            }
            // Keep the synchronous control lock free while waiting. A lease
            // watchdog can stop the owned handle during handshake or IO stalls.
            if Instant::now() >= deadline + Duration::from_secs(5) {
                break None;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let _ = tokio::time::timeout(STDERR_SETTLE, &mut self.stderr_task).await;
        let tail = self.stderr_tail();
        (status, tail)
    }

    pub fn stderr_tail(&self) -> String {
        let tail = self.stderr_tail.lock().expect("stderr tail lock");
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

/// Whether our direct child has exited, without reaping it (its pid, and so
/// its process-group id, stays reserved until the real wait).
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exited_unreaped(pid: u32) -> bool {
    let mut info = std::mem::MaybeUninit::<nix::libc::siginfo_t>::zeroed();
    // SAFETY: waitid writes into `info`; WNOWAIT leaves the child waitable.
    let result = unsafe {
        nix::libc::waitid(
            nix::libc::P_PID,
            pid,
            info.as_mut_ptr(),
            nix::libc::WEXITED | nix::libc::WNOHANG | nix::libc::WNOWAIT,
        )
    };
    if result != 0 {
        // Already reaped or not our child: nothing left to wait for.
        return true;
    }
    // SAFETY: zero-initialised and possibly written by waitid above.
    let info = unsafe { info.assume_init() };
    #[cfg(target_os = "macos")]
    let observed = info.si_pid;
    #[cfg(target_os = "linux")]
    // SAFETY: si_pid is valid for a WEXITED waitid result.
    let observed = unsafe { info.si_pid() };
    observed == pid as i32
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exited_unreaped(_: u32) -> bool {
    false
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Opt-in managed-process fence; ordinary CLI/probe shutdown retains its normal
/// SIGTERM behavior. No cached PID is signalled after the Child was reaped.
#[derive(Default)]
pub struct ProcessControl {
    state: Mutex<ControlState>,
}
#[derive(Default)]
struct ControlState {
    child: Weak<Mutex<Child>>,
    fenced_at: Option<Instant>,
}
impl ProcessControl {
    fn fenced(&self) -> bool {
        self.state
            .lock()
            .expect("process control lock")
            .fenced_at
            .is_some()
    }
    fn attach(&self, child: &Arc<Mutex<Child>>) {
        let mut state = self.state.lock().expect("process control lock");
        state.child = Arc::downgrade(child);
        if state.fenced_at.is_some() {
            Self::signal(&state);
        }
    }
    /// The owned process group's id (the child's pid) while it is attached and
    /// unreaped, so a daemon can record restart evidence for it.
    pub fn process_group(&self) -> Option<u32> {
        let child = self
            .state
            .lock()
            .expect("process control lock")
            .child
            .upgrade()?;
        let id = child.lock().expect("child lifecycle lock").id();
        id
    }
    /// Called repeatedly during the bounded stop window. First SIGTERM lets
    /// the official CLI stop detached helpers; the owned group gets SIGKILL
    /// after two seconds. This is not containment of arbitrary setsid children.
    pub fn fence(&self) {
        let mut state = self.state.lock().expect("process control lock");
        state.fenced_at.get_or_insert_with(Instant::now);
        Self::signal(&state);
    }
    fn signal(state: &ControlState) {
        let Some(child) = state.child.upgrade() else {
            return;
        };
        let mut child = child.lock().expect("child lifecycle lock");
        let Some(pid) = child.id() else {
            return;
        };
        let force = state
            .fenced_at
            .is_some_and(|at| at.elapsed() >= Duration::from_secs(2));
        #[cfg(unix)]
        {
            let signal = if force {
                nix::sys::signal::Signal::SIGKILL
            } else {
                nix::sys::signal::Signal::SIGTERM
            };
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid as i32), signal);
        }
        if force {
            let _ = child.start_kill();
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // A detached descendant may retain stderr after the bounded drain.
        // The reader belongs to this guard, including canceled shutdowns.
        self.stderr_task.abort();
        if !self.managed_group {
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = self.child.lock().expect("child lifecycle lock").id() {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blank_stderr_lines_cannot_grow_the_diagnostic_ring() {
        let child = JsonlChild::spawn(
            "/bin/sh",
            &["-c".into(), "i=0; while [ $i -lt 10000 ]; do printf '\\n' >&2; i=$((i+1)); done; printf 'tail-marker\\n' >&2".into()],
            Path::new("/"),
            &[],
            &[],
        )
        .unwrap();
        let (sink, _stream, guard) = child.split();
        drop(sink);
        let (status, tail) = guard.shutdown_with_stderr(Duration::from_secs(5)).await;
        assert_eq!(status, Some(0));
        assert!(tail.ends_with("tail-marker"));
        assert!(tail.lines().count() <= STDERR_TAIL_LINES);
    }

    #[tokio::test]
    async fn dropping_a_guard_cancels_its_owned_stderr_reader() {
        let child = JsonlChild::spawn(
            "/bin/sh",
            &["-c".into(), "read ignored".into()],
            Path::new("/"),
            &[],
            &[],
        )
        .unwrap();
        let (_sink, _stream, mut guard) = child.split();
        // Model a pipe kept open by a detached descendant: killing the direct
        // child cannot make this reader finish by itself.
        let reader = std::mem::replace(
            &mut guard.stderr_task,
            tokio::spawn(std::future::pending::<()>()),
        );
        reader.abort();
        let reader = guard.stderr_task.abort_handle();
        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !reader.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    /// A managed child that exits on its own is reaped at once (a stop, view
    /// switch or rewind no longer waits out a fixed two seconds), and what it
    /// left running in its process group is still ended.
    #[tokio::test]
    async fn a_managed_child_that_exits_is_reaped_at_once_and_its_group_ended() {
        let control = Arc::new(ProcessControl::default());
        let child = JsonlChild::spawn_controlled(
            "/bin/sh",
            &["-c".into(), "sleep 30 & exit 3".into()],
            Path::new("/"),
            &[],
            &[],
            Some(&control),
        )
        .unwrap();
        let (_sink, _stream, guard) = child.split();
        let group = guard
            .child
            .lock()
            .expect("child lifecycle lock")
            .id()
            .unwrap() as i32;
        let started = Instant::now();
        let status = guard.shutdown(Duration::from_secs(10)).await;
        assert_eq!(status, Some(3));
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "{:?}",
            started.elapsed()
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while nix::sys::signal::killpg(nix::unistd::Pid::from_raw(group), None).is_ok() {
            assert!(Instant::now() < deadline, "the background sleep survived");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[cfg(test)]
mod supervisor_channel_tests {
    use super::*;
    #[tokio::test]
    async fn startup_cleanup_marker_never_reaches_agent_child() {
        let mut child = JsonlChild::spawn(
            "/bin/sh",
            &[
                "-c".into(),
                r#"printf '{"marker":"%s"}\n' "${CHIMAERA_SUPERVISOR_CLEANUP_FD-absent}""#.into(),
            ],
            &std::env::temp_dir(),
            &[("CHIMAERA_SUPERVISOR_CLEANUP_FD".into(), "0".into())],
            &[],
        )
        .unwrap();
        assert_eq!(
            child.recv(Duration::from_secs(5)).await.unwrap().unwrap()["marker"],
            "absent"
        );
        child.shutdown(Duration::from_secs(1)).await.unwrap();
    }
}
