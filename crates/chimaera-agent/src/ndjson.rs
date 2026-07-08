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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// stderr kept for diagnostics only — a runaway child must not grow memory.
const STDERR_TAIL_BUDGET: usize = 8 * 1024;

/// A spawned agent process speaking newline-delimited JSON on stdio.
pub struct JsonlChild {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
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
            let mut lines = BufReader::new(stderr).lines();
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
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr_tail,
        })
    }

    /// Write one JSON value as a single line and flush it.
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

    /// Next JSON line from stdout. `Ok(None)` means EOF (child closed stdout).
    /// Non-JSON lines are skipped with a warning rather than failing the
    /// session — one stray diagnostic line must not kill a conversation.
    pub async fn recv(&mut self, timeout: Duration) -> Result<Option<Value>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let line = tokio::time::timeout_at(deadline, self.lines.next_line())
                .await
                .context("timed out waiting for agent output")?
                .context("agent stdout read")?;
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

    /// Last stderr lines, for handshake-failure diagnostics.
    pub fn stderr_tail(&self) -> String {
        let tail = self.stderr_tail.lock().expect("stderr tail lock");
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// Close stdin (the polite shutdown for both protocols), give the child a
    /// grace period to exit, then kill it.
    pub async fn shutdown(mut self, grace: Duration) -> Result<Option<i32>> {
        drop(self.stdin);
        match tokio::time::timeout(grace, self.child.wait()).await {
            Ok(status) => Ok(status.context("await agent exit")?.code()),
            Err(_) => {
                self.child.start_kill().ok();
                let status = self.child.wait().await.context("await killed agent")?;
                Ok(status.code())
            }
        }
    }

    /// Split into independently-owned halves so a driver can `select!` over
    /// inbound frames and outbound commands without fighting the borrow of a
    /// single struct.
    pub fn split(self) -> (JsonlSink, JsonlStream, ChildGuard) {
        (
            JsonlSink { stdin: self.stdin },
            JsonlStream {
                lines: self.lines,
                stderr_tail: Arc::clone(&self.stderr_tail),
            },
            ChildGuard {
                child: self.child,
                stderr_tail: self.stderr_tail,
            },
        )
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

/// Read half of a split [`JsonlChild`].
pub struct JsonlStream {
    lines: Lines<BufReader<ChildStdout>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
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

    pub fn stderr_tail(&self) -> String {
        let tail = self.stderr_tail.lock().expect("stderr tail lock");
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

/// Owns the child for lifecycle: wait, kill, stderr diagnostics.
pub struct ChildGuard {
    child: Child,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

impl ChildGuard {
    /// Give the child a grace period after sinks were dropped, then kill.
    pub async fn shutdown(mut self, grace: Duration) -> Option<i32> {
        match tokio::time::timeout(grace, self.child.wait()).await {
            Ok(Ok(status)) => status.code(),
            _ => {
                self.child.start_kill().ok();
                self.child.wait().await.ok().and_then(|s| s.code())
            }
        }
    }

    pub async fn wait(&mut self) -> Option<i32> {
        self.child.wait().await.ok().and_then(|s| s.code())
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
