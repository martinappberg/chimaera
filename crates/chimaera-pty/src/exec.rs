//! The exec engine: type a command into a live interactive shell and return
//! its output, exit code, and timing — the mechanics behind an agent's
//! `run_in_terminal`.
//!
//! Two modes, chosen by what the marks scanner has seen:
//!
//! - **Integrated** (shell integration active, phase `ready`): type the
//!   command; OSC 133 marks delimit output and carry the exit code.
//! - **Sentinel** (no integration, or — when the caller allows it — a
//!   foreground like `ssh` that forwards keystrokes to a markless remote
//!   shell): wrap the command in `printf`-emitted 133;C/D marks, so the
//!   same journal machinery handles it with zero remote install.
//!
//! Execs are serialized per session. A busy integrated shell **queues** the
//! exec until its prompt returns (bounded by `queue_timeout`) — the linked
//! agent waits its turn instead of typing over a running command.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::marks::{CommandView, Marks, ShellPhase};

/// How long a phase-`unknown` session gets to produce its first marks (a
/// freshly spawned integrated shell hasn't prompted yet) before the exec
/// falls back to sentinel mode.
const UNKNOWN_GRACE: Duration = Duration::from_secs(2);
/// Output silence required before sentinel mode will type.
const QUIET_PERIOD: Duration = Duration::from_millis(400);
/// How long after typing the command must visibly start (133;C observed).
const START_WINDOW: Duration = Duration::from_secs(10);
/// Longest single command line accepted.
const MAX_COMMAND_LEN: usize = 8 * 1024;

/// Where an exec currently is, for UI mirroring (queued vs executing chips).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecStage {
    /// Waiting for the shell's prompt (or for a prior exec) — nothing typed.
    Queued,
    /// The command has been typed and is running.
    Executing,
}

/// How the command was delimited.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecMode {
    Integrated,
    Sentinel,
}

#[derive(Clone, Debug)]
pub struct ExecOptions {
    pub command: String,
    /// Max wait for the shell to become ready before typing. `0` means
    /// "only if free right now".
    pub queue_timeout: Duration,
    /// Max wait for the command to finish once typed.
    pub timeout: Duration,
    /// Permit sentinel-typing into a session whose integrated shell reports
    /// a *running* command. Only safe when the foreground forwards input to
    /// a remote shell (ssh and friends) — the caller owns that policy.
    pub allow_sentinel_over_running: bool,
    /// Optional stage reporting (queued -> executing) for UI mirroring.
    pub stage: Option<watch::Sender<ExecStage>>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ExecOutcome {
    /// The journal record (partial when `timed_out`).
    pub record: CommandView,
    pub mode: ExecMode,
    /// Time spent waiting for the shell before typing.
    pub waited_ms: u64,
    /// True when `timeout` elapsed with the command still running; `record`
    /// then holds the output captured so far and no exit code. The command
    /// keeps running in the terminal.
    pub timed_out: bool,
}

#[derive(Debug)]
pub enum ExecError {
    Busy(String),
    InvalidCommand(String),
    SessionGone,
    NeverStarted(Duration),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Busy(why) => write!(f, "shell is busy: {why}"),
            ExecError::InvalidCommand(why) => write!(f, "invalid command: {why}"),
            ExecError::SessionGone => {
                write!(f, "session input channel closed (session exited?)")
            }
            ExecError::NeverStarted(window) => write!(
                f,
                "command never started within {window:?} — the foreground program may \
                 not be a shell (check the terminal's screen with read_terminal)"
            ),
        }
    }
}

impl std::error::Error for ExecError {}

fn validate(command: &str) -> Result<(), ExecError> {
    if command.trim().is_empty() {
        return Err(ExecError::InvalidCommand("empty command".into()));
    }
    if command.len() > MAX_COMMAND_LEN {
        return Err(ExecError::InvalidCommand(format!(
            "command exceeds {MAX_COMMAND_LEN} bytes"
        )));
    }
    if let Some(bad) = command.chars().find(|c| c.is_control()) {
        return Err(ExecError::InvalidCommand(format!(
            "control character {bad:?} not allowed — join lines with ';' or '&&'"
        )));
    }
    Ok(())
}

/// The sentinel wrapper: emit our own 133;C/D marks around the command from
/// whatever POSIX-ish shell is on the other side. Runs in the current shell
/// (no subshell), so `cd`/`export`/`module load` keep their effect.
fn sentinel_line(command: &str) -> String {
    format!("printf '\\033]133;C\\007'; {command}; printf '\\033]133;D;%s\\007' \"$?\"\r")
}

pub(crate) async fn exec(
    marks: Arc<Marks>,
    input: tokio::sync::mpsc::Sender<Bytes>,
    exec_lock: Arc<tokio::sync::Mutex<()>>,
    opts: ExecOptions,
) -> Result<ExecOutcome, ExecError> {
    validate(&opts.command)?;
    let set_stage = |stage: ExecStage| {
        if let Some(tx) = &opts.stage {
            let _ = tx.send(stage);
        }
    };
    set_stage(ExecStage::Queued);
    let start = Instant::now();

    // One exec at a time per session; queued execs wait here first.
    let _guard = exec_lock.lock().await;

    let queue_deadline = start + opts.queue_timeout;
    let mode = wait_ready(&marks, &opts, start, queue_deadline).await?;
    if mode == ExecMode::Sentinel {
        wait_quiet(&marks, queue_deadline).await?;
    }
    let waited_ms = start.elapsed().as_millis() as u64;

    let token = marks.expect_agent_command(opts.command.clone());
    let line = match mode {
        ExecMode::Integrated => format!("{}\r", opts.command),
        ExecMode::Sentinel => sentinel_line(&opts.command),
    };
    if input.send(Bytes::from(line)).await.is_err() {
        marks.clear_agent_expect(token);
        return Err(ExecError::SessionGone);
    }
    set_stage(ExecStage::Executing);

    let overall = Instant::now() + opts.timeout;
    let start_window = START_WINDOW.min(opts.timeout);
    let started_by = Instant::now() + start_window;
    loop {
        let notified = marks.updated();
        if let Some(record) = marks.find_by_token(token) {
            return Ok(ExecOutcome {
                record,
                mode,
                waited_ms,
                timed_out: false,
            });
        }
        let running = marks.running_by_token(token);
        let now = Instant::now();
        if running.is_none() && now >= started_by {
            marks.clear_agent_expect(token);
            return Err(ExecError::NeverStarted(start_window));
        }
        if now >= overall {
            return match running {
                Some(record) => Ok(ExecOutcome {
                    record,
                    mode,
                    waited_ms,
                    timed_out: true,
                }),
                None => {
                    marks.clear_agent_expect(token);
                    Err(ExecError::NeverStarted(start_window))
                }
            };
        }
        let wake = if running.is_none() {
            started_by.min(overall)
        } else {
            overall
        };
        let _ = tokio::time::timeout_at(wake, notified).await;
    }
}

/// Wait until the shell can accept a command, deciding the mode:
/// `ready` -> integrated; never-integrated (after a boot grace) -> sentinel;
/// stuck `running` -> sentinel only when the caller allows it.
async fn wait_ready(
    marks: &Marks,
    opts: &ExecOptions,
    start: Instant,
    queue_deadline: Instant,
) -> Result<ExecMode, ExecError> {
    let mut phase_rx = marks.phase_watch();
    let unknown_grace = start + UNKNOWN_GRACE.min(opts.queue_timeout);
    loop {
        let phase = *phase_rx.borrow_and_update();
        let now = Instant::now();
        match phase {
            ShellPhase::Ready => return Ok(ExecMode::Integrated),
            ShellPhase::Unknown => {
                if now >= unknown_grace {
                    return Ok(ExecMode::Sentinel);
                }
                let _ = tokio::time::timeout_at(unknown_grace, phase_rx.changed()).await;
            }
            ShellPhase::Running => {
                if now >= queue_deadline {
                    return if opts.allow_sentinel_over_running {
                        Ok(ExecMode::Sentinel)
                    } else {
                        Err(ExecError::Busy(
                            "a command is running in this terminal (queue timeout elapsed)".into(),
                        ))
                    };
                }
                let _ = tokio::time::timeout_at(queue_deadline, phase_rx.changed()).await;
            }
        }
    }
}

/// Sentinel gate: refuse to type while the terminal is producing output
/// (keystrokes would land inside who-knows-what).
async fn wait_quiet(marks: &Marks, queue_deadline: Instant) -> Result<(), ExecError> {
    // Always allow one full quiet period even with queue_timeout=0: the
    // check itself needs that long to prove silence.
    let deadline = queue_deadline.max(Instant::now() + QUIET_PERIOD * 2);
    loop {
        let age = marks.last_byte_age();
        if age >= QUIET_PERIOD {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ExecError::Busy(
                "terminal kept producing output; not typing into it".into(),
            ));
        }
        tokio::time::sleep(QUIET_PERIOD - age).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_control_and_empty() {
        assert!(validate("ls -la").is_ok());
        assert!(validate("echo 'a; b' && ls").is_ok());
        assert!(matches!(validate(""), Err(ExecError::InvalidCommand(_))));
        assert!(matches!(
            validate("ls\nrm -rf /"),
            Err(ExecError::InvalidCommand(_))
        ));
        assert!(matches!(
            validate("echo \x1b]0;x\x07"),
            Err(ExecError::InvalidCommand(_))
        ));
        assert!(matches!(
            validate(&"x".repeat(MAX_COMMAND_LEN + 1)),
            Err(ExecError::InvalidCommand(_))
        ));
    }

    #[test]
    fn sentinel_line_wraps_with_marks() {
        let line = sentinel_line("module load samtools");
        assert!(line.starts_with("printf '\\033]133;C\\007'; module load samtools;"));
        assert!(line.ends_with("\"$?\"\r"));
    }
}
