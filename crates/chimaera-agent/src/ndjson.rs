//! Line-oriented JSON transport over a child process's stdio.
//!
//! Shared by both agent clients: Claude's stream-json and Codex's app-server
//! are the same framing (one JSON object per line on stdin/stdout). stderr is
//! kept as a small tail ring — when a handshake fails, those bytes are the
//! only diagnostic the daemon has.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// stderr kept for diagnostics only — a runaway child must not grow memory.
const STDERR_TAIL_BUDGET: usize = 8 * 1024;
/// Empty lines still allocate ring entries; the byte budget alone does not
/// bound them.
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
///
/// The line being assembled lives on the reader, not in the future, so
/// `next_line` is cancel-safe: a driver `select!` or a `recv` timeout that
/// drops a read midway keeps the bytes already consumed for the next call.
struct CappedLines<R> {
    reader: BufReader<R>,
    max: usize,
    partial: Vec<u8>,
    overflowed: bool,
}

impl<R: AsyncRead + Unpin> CappedLines<R> {
    fn new(inner: R, max: usize) -> Self {
        Self {
            reader: BufReader::new(inner),
            max,
            partial: Vec::new(),
            overflowed: false,
        }
    }

    /// Next line without its trailing `\n`. `Ok(None)` = EOF. Invalid UTF-8 is
    /// replaced lossily rather than failing the session.
    async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.partial.is_empty() && !self.overflowed {
                    return Ok(None);
                }
                break;
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    push_capped(
                        &mut self.partial,
                        &available[..pos],
                        self.max,
                        &mut self.overflowed,
                    );
                    self.reader.consume(pos + 1);
                    break;
                }
                None => {
                    let len = available.len();
                    push_capped(&mut self.partial, available, self.max, &mut self.overflowed);
                    self.reader.consume(len);
                }
            }
        }
        if std::mem::take(&mut self.overflowed) {
            tracing::warn!(cap = self.max, "agent output line exceeded cap; truncated");
        }
        Ok(Some(
            String::from_utf8_lossy(&std::mem::take(&mut self.partial)).into_owned(),
        ))
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
        let mut cmd = Command::new(bin);
        // Its own process group: the executable can be a launcher (npm's
        // codex entry is a node script around the native binary), and
        // killing only the direct child would strand that native process,
        // still running and still holding the stdout pipe open.
        #[cfg(unix)]
        cmd.process_group(0);
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
                // The newest line stays even when it alone exceeds the byte
                // budget (a line can be up to MAX_STDERR_LINE_BYTES): popping
                // it would turn a long stack trace into an empty tail.
                while (total > STDERR_TAIL_BUDGET && tail.len() > 1)
                    || tail.len() > STDERR_TAIL_LINES
                {
                    match tail.pop_front() {
                        Some(dropped) => total -= dropped.len(),
                        None => break,
                    }
                }
            }
        });

        Ok(Self {
            sink: JsonlSink { stdin },
            stream: JsonlStream {
                lines: CappedLines::new(stdout, MAX_STDOUT_LINE_BYTES),
            },
            guard: ChildGuard {
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
    child: Child,
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
        if let Some(pid) = self.child.id() {
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
    ///
    /// On unix the whole process group is SIGKILLed once the leader has
    /// exited or the grace has run out, so a launcher's native child or a
    /// helper left in the group does not outlive the session. The leader's
    /// exit is observed without reaping it (`exited_unreaped`), so its pid,
    /// and with it the group id, cannot be recycled before the group kill.
    pub async fn shutdown_with_stderr(mut self, grace: Duration) -> (Option<i32>, String) {
        let deadline = Instant::now() + grace;
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            while Instant::now() < deadline && !exited_unreaped(pid) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let status = match tokio::time::timeout(remaining, self.child.wait()).await {
            Ok(Ok(status)) => status.code(),
            _ => {
                self.child.start_kill().ok();
                self.child.wait().await.ok().and_then(|s| s.code())
            }
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

impl Drop for ChildGuard {
    /// A shutdown future dropped midway (its caller cancelled) or a guard
    /// never shut down still ends the group and its stderr reader: a
    /// descendant holding the stderr pipe would otherwise keep the reader
    /// task alive, and the group running, after the session is gone. A no-op
    /// for a reaped child (tokio clears its pid then).
    fn drop(&mut self) {
        self.stderr_task.abort();
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
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
        // ECHILD: already reaped, nothing left to wait for. Any other error
        // (EINTR, a rejected flag) says nothing about the child; reporting
        // it as an exit would cut the grace short and SIGKILL the group
        // before the leader's own SIGTERM cleanup has run.
        return std::io::Error::last_os_error().raw_os_error() == Some(nix::libc::ECHILD);
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
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn exited_unreaped(_: u32) -> bool {
    false
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn partial_output_survives_canceled_line_reads() {
        let (mut input, output) = tokio::io::duplex(256);
        let mut lines = CappedLines::new(output, 32);
        input.write_all(b"{\"proof\":").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), lines.next_line())
                .await
                .is_err()
        );
        assert!(!lines.partial.is_empty());
        input.write_all(b"true}\n").await.unwrap();
        assert_eq!(
            lines.next_line().await.unwrap().unwrap(),
            "{\"proof\":true}"
        );
        assert!(lines.partial.is_empty());
        input.write_all(&[b'x'; 64]).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), lines.next_line())
                .await
                .is_err()
        );
        assert_eq!(lines.partial.len(), 32);
        assert!(lines.overflowed);
        input.write_all(b"\n{}").await.unwrap();
        assert_eq!(lines.next_line().await.unwrap().unwrap().len(), 32);
    }

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
    async fn an_oversize_stderr_line_is_kept_not_dropped() {
        let over_budget = STDERR_TAIL_BUDGET + 1024;
        let child = JsonlChild::spawn(
            "/bin/sh",
            &[
                "-c".into(),
                format!("head -c {over_budget} /dev/zero | tr '\\0' x >&2; printf '\\n' >&2"),
            ],
            Path::new("/"),
            &[],
            &[],
        )
        .unwrap();
        let (sink, _stream, guard) = child.split();
        drop(sink);
        let (status, tail) = guard.shutdown_with_stderr(Duration::from_secs(5)).await;
        assert_eq!(status, Some(0));
        assert_eq!(tail.trim_end().len(), over_budget, "{}", tail.len());
    }

    #[tokio::test]
    async fn dropping_a_guard_cancels_its_stderr_reader() {
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

    // Model npm's codex entry: a launcher that forwards TERM to its native
    // child, which ignores it. Both hold stdout until they exit; the native
    // child would on its own exit only after 6 s.
    async fn launcher_cleanup(cancel_shutdown: bool) {
        let child = JsonlChild::spawn(
            "/bin/sh",
            &[
                "-c".into(),
                r#"
native=
trap 'if [ -n "$native" ]; then kill -TERM "$native"; fi' TERM
/bin/sh -c 'trap "" TERM INT HUP; printf "{\"ready\":true}\n"; sleep 6' &
native=$!
wait "$native"
wait "$native"
"#
                .into(),
            ],
            Path::new("/"),
            &[],
            &[],
        )
        .unwrap();
        let (sink, mut stream, guard) = child.split();
        let owns_group = {
            let pid = guard.child.id().unwrap() as i32;
            // The unreaped child pins this pid while it is inspected.
            unsafe { nix::libc::getpgid(pid) == pid && pid != nix::libc::getpgrp() }
        };
        let ready = tokio::time::timeout(Duration::from_secs(1), stream.next()).await;
        guard.terminate();
        drop(sink);
        let stopped = if cancel_shutdown {
            // Drop the shutdown future while its grace is still running.
            tokio::time::timeout(
                Duration::from_millis(20),
                guard.shutdown(Duration::from_secs(3)),
            )
            .await
            .is_err()
        } else {
            guard.shutdown(Duration::from_secs(3)).await;
            true
        };
        let eof = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
        let closed_promptly = matches!(eof, Ok(Ok(None)));
        // Without the fix the native child outlives the launcher; let it end
        // on its own before asserting, so a failing run leaves nothing behind.
        let settled = closed_promptly
            || matches!(
                tokio::time::timeout(Duration::from_secs(8), stream.next()).await,
                Ok(Ok(None))
            );
        drop(stream);
        assert!(settled, "finite launcher/native pipe did not settle");
        assert!(matches!(ready, Ok(Ok(Some(ref frame))) if frame["ready"] == true));
        assert!(stopped, "shutdown did not reach its intended cutpoint");
        assert!(
            closed_promptly,
            "native child survived launcher teardown (own group: {owns_group})"
        );
        assert!(owns_group, "the launcher inherited the daemon's group");
    }

    #[tokio::test]
    async fn launcher_shutdown_ends_its_native_child() {
        launcher_cleanup(false).await;
    }

    #[tokio::test]
    async fn canceled_launcher_shutdown_still_ends_its_native_child() {
        launcher_cleanup(true).await;
    }

    /// A child that exits on its own is reaped as soon as it does, and what
    /// it left running in its process group is still ended.
    #[tokio::test]
    async fn an_exited_child_is_reaped_at_once_and_its_group_ended() {
        let child = JsonlChild::spawn(
            "/bin/sh",
            &["-c".into(), "sleep 30 & exit 3".into()],
            Path::new("/"),
            &[],
            &[],
        )
        .unwrap();
        let (_sink, _stream, guard) = child.split();
        let group = guard.child.id().unwrap() as i32;
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
