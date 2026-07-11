//! Driver harness: what every structured-agent driver plugs into.
//!
//! A driver is a tokio task that owns one child process's stdio, translates
//! its native protocol into [`AgentEvent`]s, and consumes [`AgentCommand`]s.
//! The harness fixes the contract around it: spawn inputs, the handshake
//! watchdog (a session that can't handshake degrades to a PTY — it must
//! never hang a pane), kill semantics, and exit classification.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::model::{cap_output, AgentCommand, AgentEvent, COALESCE_INTERVAL_MS};
use crate::ndjson::{JsonlChild, JsonlSink, JsonlStream};

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
    /// The binary's `--version` line as the server probed it (`None` when
    /// the probe failed). Neither wire protocol offers a reliable version
    /// handshake (see PROTOCOL.md), so the server-side probe is the source:
    /// journaled on `Init`, compared against the driver's tested pin for
    /// the non-fatal drift notice.
    pub agent_version: Option<String>,
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
            agent_version: None,
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

/// How a driver task ended — the server's degrade logic keys on this. Clone
/// so the server can fan the outcome to both its exit-handler and its
/// broadcast without re-materializing the parts by hand.
#[derive(Debug, Clone)]
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

/// A driver's protocol translator: the pure, I/O-free state machine each
/// agent implements. The harness ([`run_driver`]) owns the process, the
/// select pump, bounded stdin writes, and exit classification; the mapper
/// only turns inbound frames and client commands into outbound frames +
/// [`AgentEvent`]s. Both drivers implement this over the same [`DriverStep`].
pub trait Mapper: Send {
    /// The `Init` event emitted immediately after a successful handshake.
    fn init_event(&self) -> AgentEvent;
    /// Translate one inbound protocol frame.
    fn on_frame(&mut self, frame: &Value) -> DriverStep;
    /// Translate one client command.
    fn on_command(&mut self, cmd: AgentCommand) -> DriverStep;
    /// Emit any buffered coalesced text (the harness's timer tick + teardown).
    fn flush(&mut self) -> Option<AgentEvent>;
    /// Resolution events for asks whose reply route dies with this mapper.
    /// The pending question/permission maps are the ONLY route back to the
    /// child, so when the driver ends they must resolve into the journal —
    /// otherwise every future replay re-delivers an ask that can never be
    /// answered (the "stuck permission card" bug). The harness calls this
    /// once, right before `Exited`.
    fn drain_pending(&mut self) -> Vec<AgentEvent> {
        Vec::new()
    }
}

/// A mapper's output for one input: outbound frames to write to the child,
/// then events to broadcast. Order is load-bearing — the write happens before
/// its optimistic events, so a failed write drops those events (see
/// [`deliver`]): a dropped permission answer must never read as resolved.
#[derive(Default)]
pub struct DriverStep {
    pub events: Vec<AgentEvent>,
    pub outbound: Vec<Value>,
}

/// What a concrete driver hands back from its handshake: the built mapper and
/// any steps to deliver right after the `Init` event (claude seeds effort
/// settings and replays parked permission prompts; codex has none).
pub struct Handshake<M> {
    pub mapper: M,
    pub initial: Vec<DriverStep>,
}

/// Everything a concrete agent driver supplies; the harness wraps the process
/// lifecycle, the select pump, and exit classification around it so the
/// spawn/supervision/teardown logic lives exactly once.
pub trait Driver: Send {
    type Mapper: Mapper;

    /// The agent kind ("claude"/"codex") — names the agent in the
    /// harness-emitted startup-failure and version-drift events. No default:
    /// the compiler enforces both drivers carry it.
    fn kind(&self) -> &'static str;

    /// The CLI version this driver's wire facts were live-verified against
    /// (`TESTED_*_VERSION`) — the drift notice's comparison pin. No default,
    /// so a new driver cannot ship unpinned.
    fn tested_version(&self) -> &'static str;

    /// Env vars appended to the spawn (claude pins the auto-updater off and
    /// enables SDK file checkpointing; codex needs none).
    fn env_extra(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Prove the wire works, build the mapper, and return any post-`Init`
    /// steps. An `Err` is the degrade-to-PTY signal. The harness applies the
    /// handshake timeout around this, so it may block on stream reads without
    /// its own deadline.
    fn handshake<'a>(
        &'a self,
        sink: &'a mut JsonlSink,
        stream: &'a mut JsonlStream,
        spec: &'a SpawnSpec,
    ) -> impl Future<Output = std::result::Result<Handshake<Self::Mapper>, String>> + Send + 'a;
}

/// Bounded outbound write. A child that stops draining stdin while the mapper
/// hands us a large frame would otherwise wedge the whole select loop (the
/// kill/command arms starve). Generous for a local pipe; expiry is fatal.
const SINK_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of shipping one [`DriverStep`].
enum Delivery {
    Ok,
    /// A stdin write failed or timed out. Fatal: the child can no longer be
    /// driven, so the harness breaks with `ProtocolError` and the step's
    /// optimistic events are intentionally NOT emitted — a dropped permission
    /// answer must never surface to the UI as resolved.
    WriteFailed(String),
    /// The event receiver is gone: the session was torn down.
    ReceiverGone,
}

/// Ship a mapper step: outbound frames first (bounded), then events. A write
/// failure returns before any event is emitted.
async fn deliver(sink: &mut JsonlSink, io: &DriverIo, step: DriverStep) -> Delivery {
    for frame in &step.outbound {
        match tokio::time::timeout(SINK_WRITE_TIMEOUT, sink.send(frame)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Delivery::WriteFailed(format!("stdin write failed: {err:#}")),
            Err(_) => return Delivery::WriteFailed("stdin write timed out".to_string()),
        }
    }
    for ev in step.events {
        if io.events.send(ev).await.is_err() {
            return Delivery::ReceiverGone;
        }
    }
    Delivery::Ok
}

/// Journal + broadcast the client-visible face of a startup failure, then the
/// terminal `Exited`. The handshake-failure exit paths previously returned
/// before any event reached the pump: the reason and stderr tail went only to
/// the daemon log and the chat pane just showed a bare "agent exited" — the
/// reported post-update failure mode. `fatal:true` renders as the chat
/// surface's error banner (and errors the rail row); a later reattach replays
/// it from the journal.
async fn report_startup_failure(
    io: &DriverIo,
    kind: &str,
    reason: &str,
    stderr_tail: &str,
    status: Option<i32>,
) {
    // The stderr ring is already bounded (ndjson), but run it through the
    // shared cap so this event obeys caps-at-construction even if that
    // budget ever grows.
    let (tail, _) = cap_output(stderr_tail);
    let tail = tail.trim();
    let mut message = format!("{kind} failed to start: {reason}");
    if !tail.is_empty() {
        message.push_str("\n\n");
        message.push_str(tail);
    }
    message.push_str(&format!(
        "\n\nIf {kind} was just updated, its chat protocol may have changed — \
         reopen this session as a terminal, or check that `{kind}` still runs in one."
    ));
    let _ = io
        .events
        .send(AgentEvent::Error {
            message,
            fatal: true,
        })
        .await;
    let _ = io.events.send(AgentEvent::Exited { status }).await;
}

/// An agent that handshakes and then exits almost immediately never served a
/// conversation — that exit is a startup failure (the plausible post-update
/// crash mode), not an end-of-life, and its stderr is the only diagnostic.
/// Time-based because neither protocol has a "goodbye" frame: generous enough
/// for slow first traffic, far shorter than any real session.
pub const FAILURE_AT_BIRTH_WINDOW: Duration = Duration::from_secs(10);

/// The shared driver harness: spawn the child, run the driver's handshake
/// under the watchdog, then pump frames/commands/kills through the mapper
/// until the child, the client, or a kill signal ends the session. Every exit
/// path closes stdin and reaps the child with a bounded wait.
pub async fn run_driver<D: Driver>(driver: D, spec: SpawnSpec, mut io: DriverIo) -> DriverExit {
    let mut env = spec.env.clone();
    env.extend(driver.env_extra());
    let child = match JsonlChild::spawn(
        &spec.argv[0],
        &spec.argv[1..],
        &spec.cwd,
        &env,
        &spec.env_remove,
    ) {
        Ok(child) => child,
        Err(err) => {
            let reason = format!("spawn failed: {err:#}");
            report_startup_failure(&io, driver.kind(), &reason, "", None).await;
            return DriverExit::HandshakeFailed {
                reason,
                stderr_tail: String::new(),
            };
        }
    };
    let (mut sink, mut stream, guard) = child.split();

    // Handshake watchdog: a session that cannot prove the protocol works must
    // fail fast so the server can respawn it as a PTY instead of hanging a pane.
    let handshake = tokio::time::timeout(
        spec.handshake_timeout,
        driver.handshake(&mut sink, &mut stream, &spec),
    )
    .await;
    let Handshake {
        mut mapper,
        initial,
    } = match handshake {
        Ok(Ok(hs)) => hs,
        Ok(Err(reason)) => {
            let (status, tail) = guard.shutdown_with_stderr(Duration::ZERO).await;
            report_startup_failure(&io, driver.kind(), &reason, &tail, status).await;
            return DriverExit::HandshakeFailed {
                reason,
                stderr_tail: tail,
            };
        }
        Err(_) => {
            let reason = format!("no handshake within {:?}", spec.handshake_timeout);
            let (status, tail) = guard.shutdown_with_stderr(Duration::ZERO).await;
            report_startup_failure(&io, driver.kind(), &reason, &tail, status).await;
            return DriverExit::HandshakeFailed {
                reason,
                stderr_tail: tail,
            };
        }
    };

    if io.events.send(mapper.init_event()).await.is_err() {
        guard.shutdown(Duration::ZERO).await;
        return DriverExit::Killed;
    }
    // Version drift is warn-not-block: the wire is only VERIFIED against the
    // pinned TESTED_*_VERSION, but most updates stay compatible — refusing to
    // spawn would break every routine update. The journaled notice is the
    // ready-made diagnosis when a drifted binary later misbehaves. Substring
    // match because the probe line is the CLI's own phrasing
    // ("2.1.204 (Claude Code)", "codex-cli 0.142.5").
    if let Some(detected) = spec.agent_version.as_deref() {
        if !detected.contains(driver.tested_version()) {
            let text = format!(
                "{kind} {detected} detected; chat mode was verified against {kind} {tested} — \
                 if this session misbehaves, that's likely why",
                kind = driver.kind(),
                tested = driver.tested_version(),
            );
            if io.events.send(AgentEvent::Notice { text }).await.is_err() {
                guard.shutdown(Duration::ZERO).await;
                return DriverExit::Killed;
            }
        }
    }
    // Post-handshake seeding/replay. A write failing THIS early means the
    // child died right after answering the handshake (the post-update crash
    // mode) — surface it as the startup failure it is, not a silent teardown;
    // a gone receiver means the session itself was torn down.
    for step in initial {
        match deliver(&mut sink, &io, step).await {
            Delivery::Ok => {}
            Delivery::WriteFailed(err) => {
                let reason = format!("agent exited during startup ({err})");
                let (status, tail) = guard.shutdown_with_stderr(Duration::ZERO).await;
                report_startup_failure(&io, driver.kind(), &reason, &tail, status).await;
                return DriverExit::HandshakeFailed {
                    reason,
                    stderr_tail: tail,
                };
            }
            Delivery::ReceiverGone => {
                guard.shutdown(Duration::ZERO).await;
                return DriverExit::Killed;
            }
        }
    }

    // Failure-at-birth clock: starts once the handshake has proven the wire,
    // so the epilogue can tell end-of-life from a binary that handshakes and
    // then crashes (see FAILURE_AT_BIRTH_WINDOW).
    let born = tokio::time::Instant::now();

    let mut tick = tokio::time::interval(Duration::from_millis(COALESCE_INTERVAL_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let exit = loop {
        tokio::select! {
            frame = stream.next() => match frame {
                Ok(Some(frame)) => {
                    let out = mapper.on_frame(&frame);
                    match deliver(&mut sink, &io, out).await {
                        Delivery::Ok => {}
                        Delivery::WriteFailed(reason) => break DriverExit::ProtocolError(reason),
                        Delivery::ReceiverGone => break DriverExit::Killed,
                    }
                }
                // Stdout EOF: the child closed its output. The uniform epilogue
                // closes stdin and reaps with a bounded wait, so a child that
                // only closed stdout but lingers can't leak the driver.
                Ok(None) => break DriverExit::Clean(None),
                Err(err) => break DriverExit::ProtocolError(format!("{err:#}")),
            },
            cmd = io.commands.recv() => match cmd {
                Some(cmd) => {
                    let out = mapper.on_command(cmd);
                    match deliver(&mut sink, &io, out).await {
                        Delivery::Ok => {}
                        Delivery::WriteFailed(reason) => break DriverExit::ProtocolError(reason),
                        Delivery::ReceiverGone => break DriverExit::Killed,
                    }
                }
                None => break DriverExit::Killed,
            },
            _ = io.kill.changed() => break DriverExit::Killed,
            _ = tick.tick() => {
                if let Some(ev) = mapper.flush() {
                    if io.events.send(ev).await.is_err() {
                        break DriverExit::Killed;
                    }
                }
            }
        }
    };

    // Age measured at loop exit (before the reap's grace wait), so the
    // failure-at-birth window is about the child's behavior, not ours.
    let age_at_exit = born.elapsed();
    if let Some(ev) = mapper.flush() {
        let _ = io.events.send(ev).await;
    }
    // A dying driver takes its reply routes with it: journal a definitive
    // resolution for every pending ask so no attached client — nor any future
    // replay of this journal — is left holding a card that cannot be answered.
    // A respawn's handshake re-delivers still-parked prompts as fresh
    // requests (claude pending_permission_requests), so nothing answerable is
    // lost by resolving here.
    for ev in mapper.drain_pending() {
        if io.events.send(ev).await.is_err() {
            break;
        }
    }
    // Close stdin (the polite shutdown both protocols honor) so a child blocked
    // on read wakes, then reap with a bounded wait. A normally-exiting child
    // returns its real status at once; a lingerer is SIGKILLed after the grace.
    drop(sink);
    let (status, stderr_tail) = guard.shutdown_with_stderr(KILL_GRACE).await;
    // Exit-at-birth reclassification: a bare "Clean" here would conflate
    // end-of-life with failure-at-birth and discard the stderr diagnostic
    // that HandshakeFailed preserves. A genuine 0 exit is respected.
    if matches!(exit, DriverExit::Clean(_))
        && age_at_exit < FAILURE_AT_BIRTH_WINDOW
        && status != Some(0)
    {
        let reason = format!(
            "exited {}s after startup (status {})",
            age_at_exit.as_secs(),
            status.map_or_else(|| "unknown".to_string(), |s| s.to_string()),
        );
        report_startup_failure(&io, driver.kind(), &reason, &stderr_tail, status).await;
        return DriverExit::HandshakeFailed {
            reason,
            stderr_tail,
        };
    }
    let _ = io.events.send(AgentEvent::Exited { status }).await;
    match exit {
        DriverExit::Clean(_) => DriverExit::Clean(status),
        other => other,
    }
}
