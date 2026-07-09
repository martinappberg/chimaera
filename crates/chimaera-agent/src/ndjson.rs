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
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// stderr kept for diagnostics only — a runaway child must not grow memory.
const STDERR_TAIL_BUDGET: usize = 8 * 1024;
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
        let mut cmd = Command::new(bin);
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
        tokio::spawn(async move {
            let mut lines = CappedLines::new(stderr, MAX_STDERR_LINE_BYTES);
            while let Ok(Some(line)) = lines.next_line().await {
                let mut tail = tail.lock().expect("stderr tail lock");
                tail.push_back(line);
                let mut total: usize = tail.iter().map(|l| l.len()).sum();
                while total > STDERR_TAIL_BUDGET {
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
            guard: ChildGuard { child, stderr_tail },
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

/// Owns the child for lifecycle: bounded shutdown, kill, stderr diagnostics.
pub struct ChildGuard {
    child: Child,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

impl ChildGuard {
    /// Give the child a grace period after sinks were dropped, then kill. The
    /// harness's only reap path — an unbounded wait can't leak a lingering
    /// child (a normally-exiting one returns its status within the grace).
    pub async fn shutdown(mut self, grace: Duration) -> Option<i32> {
        match tokio::time::timeout(grace, self.child.wait()).await {
            Ok(Ok(status)) => status.code(),
            _ => {
                self.child.start_kill().ok();
                self.child.wait().await.ok().and_then(|s| s.code())
            }
        }
    }

    pub fn stderr_tail(&self) -> String {
        let tail = self.stderr_tail.lock().expect("stderr tail lock");
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
