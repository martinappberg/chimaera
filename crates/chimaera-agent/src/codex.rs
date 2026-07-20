//! Codex app-server JSON-RPC 2.0 client (JSONL over stdio, `jsonrpc` header
//! omitted on the wire — matches what the binary actually emits).
//!
//! Wire facts live-verified against [`TESTED_CODEX_VERSION`]:
//! - Handshake: `initialize` request → result, then the client MUST send the
//!   `initialized` notification before `thread/start`.
//! - `thread/start {cwd}` → `result.thread.id` (also `sessionId`, rollout
//!   `path` under ~/.codex/sessions/...).
//! - Turn stream: `turn/started`, `item/started|completed` (types seen:
//!   `userMessage`, `reasoning`, `agentMessage`, command/file items),
//!   `item/agentMessage/delta`, `thread/tokenUsage/updated` (totals + last +
//!   modelContextWindow), `account/rateLimits/updated`, `thread/status/changed`,
//!   `turn/completed` (durationMs; token usage arrives via the separate
//!   tokenUsage notification on this version).
//! - Approvals arrive as server→client JSON-RPC *requests* (they carry an
//!   `id` and a `method`) and must be answered by that id.
//! - The connection multiplexes EVERY thread: a collab subagent's whole
//!   transcript streams interleaved with the parent's, distinguished only by
//!   `params.threadId` (multi-agent, live 0.144.2 — PROTOCOL.md Pass 16).
//!   Collab items on the parent thread: `subAgentActivity` (spawn/input/close
//!   markers) and `collabAgentToolCall` (only `wait` seen live).

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::ndjson::JsonlChild;

/// CLI version these frame shapes were verified against (2026-07-16).
pub const TESTED_CODEX_VERSION: &str = "0.144.2";

/// The `initialize` request both the probe client and the driver handshake
/// send. Declares `experimentalApi` so `thread/settings/update` is available
/// (live: -32600 "requires experimentalApi capability" without it).
fn initialize_request(id: u64) -> Value {
    json!({
        "id": id,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "chimaera",
                "title": "Chimaera",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": { "experimentalApi": true },
        },
    })
}

pub struct CodexChat {
    io: JsonlChild,
    next_id: u64,
}

impl CodexChat {
    pub fn spawn(bin: &str, cwd: &Path) -> Result<Self> {
        let args = vec!["app-server".to_string()];
        let io = JsonlChild::spawn(bin, &args, cwd, &[], &[])?;
        Ok(Self { io, next_id: 0 })
    }

    /// `initialize` → await result → `initialized` notification. Returns the
    /// initialize result (userAgent, codexHome, platform). The degrade-to-PTY
    /// signal on failure/timeout.
    pub async fn initialize(&mut self, timeout: Duration) -> Result<Value> {
        let id = self.request_id();
        self.io.send(&initialize_request(id)).await?;
        let result = self.await_result(id, timeout).await?;
        self.io.send(&json!({ "method": "initialized" })).await?;
        Ok(result)
    }

    /// Start (or later: resume) a thread. Returns the thread id used by all
    /// turn methods.
    pub async fn thread_start(&mut self, cwd: &Path, timeout: Duration) -> Result<String> {
        let id = self.request_id();
        self.io
            .send(&json!({
                "id": id,
                "method": "thread/start",
                "params": { "cwd": cwd },
            }))
            .await?;
        let result = self.await_result(id, timeout).await?;
        let thread_id = result["thread"]["id"]
            .as_str()
            .context("thread/start result missing thread.id")?;
        Ok(thread_id.to_string())
    }

    /// Kick off a turn; item/turn notifications then flow through `recv`.
    pub async fn turn_start(&mut self, thread_id: &str, text: &str) -> Result<u64> {
        let id = self.request_id();
        self.io
            .send(&json!({
                "id": id,
                "method": "turn/start",
                "params": {
                    "threadId": thread_id,
                    "input": [{ "type": "text", "text": text }],
                },
            }))
            .await?;
        Ok(id)
    }

    /// Next raw frame (notification, response, or server-initiated request).
    pub async fn recv(&mut self, timeout: Duration) -> Result<Option<Value>> {
        self.io.recv(timeout).await
    }

    /// Arbitrary request; returns the raw response FRAME (result or error) so
    /// probes can feature-detect optional methods without bailing.
    pub async fn request_raw(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.request_id();
        self.io
            .send(&json!({ "id": id, "method": method, "params": params }))
            .await?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .with_context(|| format!("timed out waiting for codex response to {method}"))?;
            match self.io.recv(remaining).await? {
                Some(value) => {
                    if value["id"] == json!(id) && value.get("method").is_none() {
                        return Ok(value);
                    }
                }
                None => bail!("codex exited while awaiting {method}"),
            }
        }
    }

    pub fn stderr_tail(&self) -> String {
        self.io.stderr_tail()
    }

    pub async fn shutdown(self, grace: Duration) -> Result<Option<i32>> {
        self.io.shutdown(grace).await
    }

    /// Drain frames until the response for `id` arrives. Notifications seen
    /// along the way are debug-logged and dropped — fine for lock-step calls
    /// (initialize, thread/start) where no turn is running yet.
    async fn await_result(&mut self, id: u64, timeout: Duration) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .with_context(|| format!("timed out waiting for codex response {id}"))?;
            match self.io.recv(remaining).await? {
                Some(value) => {
                    if value["id"] == json!(id) {
                        if let Some(err) = value.get("error") {
                            bail!("codex request {id} failed: {err}");
                        }
                        return Ok(value["result"].clone());
                    }
                    tracing::debug!(method = %value["method"], "frame skipped while awaiting response");
                }
                None => bail!(
                    "codex exited while awaiting response {id}; stderr: {}",
                    self.io.stderr_tail()
                ),
            }
        }
    }

    fn request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

// --- structured driver -------------------------------------------------------

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use tokio::task::JoinHandle;

use crate::driver::{
    run_driver, AgentAdapter, Driver, DriverExit, DriverIo, DriverStep, Handshake, Mapper,
    SpawnSpec, IDLE_FLUSH_GRACE_TICKS, INTERRUPT_GRACE_TICKS,
};
use crate::model::{
    cap_output, truncate_label, AgentCommand, AgentEvent, ChunkKind, Coalescer, CompactionPhase,
    ContentBlock, PermissionOption, PermissionOptionKind, ToolContent, ToolKind, ToolStatus, Usage,
    UserMessageState,
};
use crate::ndjson::{JsonlSink, JsonlStream};

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> &'static str {
        "codex"
    }

    fn spawn(&self, spec: SpawnSpec, io: DriverIo) -> Result<JoinHandle<DriverExit>> {
        anyhow::ensure!(!spec.argv.is_empty(), "empty argv");
        Ok(tokio::spawn(run_driver(CodexDriver, spec, io)))
    }
}

struct CodexDriver;

impl Driver for CodexDriver {
    type Mapper = CodexMapper;

    fn kind(&self) -> &'static str {
        "codex"
    }

    fn tested_version(&self) -> &'static str {
        TESTED_CODEX_VERSION
    }

    // Handshake covers initialize AND thread start/resume — a driver that
    // cannot open a thread is as dead as one that cannot speak at all.
    async fn handshake<'a>(
        &'a self,
        sink: &'a mut JsonlSink,
        stream: &'a mut JsonlStream,
        spec: &'a SpawnSpec,
    ) -> std::result::Result<Handshake<CodexMapper>, String> {
        let hs = codex_handshake(sink, stream, spec).await?;
        let mut initial = vec![DriverStep {
            events: vec![AgentEvent::EffortState {
                effort: hs.effort.clone(),
                ultracode: false,
            }],
            outbound: Vec::new(),
        }];
        // A failed conversation rewind degrades to a notice, not a dead pane:
        // the thread resumed whole, only the rollback was refused/ignored.
        if let Some(err) = hs.rollback_error {
            initial.push(DriverStep {
                events: vec![AgentEvent::Notice {
                    text: format!(
                        "conversation rewind failed: {err} (the agent may still see the rewound turns)"
                    ),
                }],
                outbound: Vec::new(),
            });
        }
        Ok(Handshake {
            mapper: CodexMapper::new(
                hs.thread_id,
                hs.models,
                hs.model,
                hs.effort,
                spec.agent_version.clone(),
                spec.mcp_auto_approve.clone(),
                hs.next_id,
            ),
            initial,
        })
    }
}

/// What the codex handshake hands the driver: the opened thread, the model
/// catalog, the next free JSON-RPC id, and whether a requested
/// conversation rollback failed (surfaced as a notice, never a dead pane).
struct CodexHandshake {
    thread_id: String,
    models: Vec<crate::model::ModelInfo>,
    model: Option<String>,
    effort: Option<String>,
    next_id: u64,
    rollback_error: Option<String>,
}

async fn codex_handshake(
    sink: &mut JsonlSink,
    stream: &mut JsonlStream,
    spec: &SpawnSpec,
) -> std::result::Result<CodexHandshake, String> {
    if sink.send(&initialize_request(0)).await.is_err() {
        return Err("initialize write failed".into());
    }
    await_rpc_result(stream, 0).await?;
    if sink
        .send(&json!({ "method": "initialized" }))
        .await
        .is_err()
    {
        return Err("initialized write failed".into());
    }

    let open = thread_open_request(spec);
    if sink.send(&open).await.is_err() {
        return Err("thread open write failed".into());
    }
    let result = await_rpc_result(stream, 1).await?;
    let thread_id = result["thread"]["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| format!("thread open result missing thread.id: {result}"))?;
    // Current app-server returns the effective model at the top level. Keep
    // the requested value as a compatibility fallback for older binaries so
    // Init and plan-mode settings still reflect the user's launch choice.
    let model = result["model"]
        .as_str()
        .map(String::from)
        .or_else(|| spec.initial_model.clone());
    let mut effort = result["reasoningEffort"].as_str().map(String::from);
    let mut next_id = 2u64;

    // Conversation rewind: drop the trailing turns right after the resume,
    // while the exchange is still lock-step (live-verified: thread/rollback
    // works immediately after thread/resume; an overcount clamps silently,
    // so the count must be exact — the server derives it from the journal).
    let mut rollback_error = None;
    if let (Some(_), Some(turns)) = (
        &spec.pinned_native_id,
        spec.rollback_turns.filter(|n| *n > 0),
    ) {
        let id = next_id;
        next_id += 1;
        if sink
            .send(&json!({
                "id": id, "method": "thread/rollback",
                "params": { "threadId": thread_id, "numTurns": turns },
            }))
            .await
            .is_err()
        {
            return Err("thread/rollback write failed".into());
        }
        // Bounded like model/list: a binary that silently drops the method
        // must not wedge the handshake until the watchdog fires.
        rollback_error = match tokio::time::timeout(
            Duration::from_secs(5),
            await_rpc_result(stream, id),
        )
        .await
        {
            Ok(Ok(_)) => None,
            Ok(Err(err)) => Some(err),
            Err(_) => Some("no response to thread/rollback (unsupported binary?)".into()),
        };
    }

    // The agent's own catalog beats any curated list; absence (older
    // binaries) is not a handshake failure.
    let list_id = next_id;
    next_id += 1;
    let mut models = Vec::new();
    if sink
        .send(&json!({
            "id": list_id, "method": "model/list",
            "params": { "includeHidden": false, "cursor": null, "limit": 100 },
        }))
        .await
        .is_ok()
    {
        // Per-request cap so a binary that silently drops this unknown method
        // can't wedge the whole handshake until the 20s watchdog fires — the
        // model catalog is optional, so a timeout is just an empty catalog.
        let listed =
            tokio::time::timeout(Duration::from_secs(2), await_rpc_result(stream, list_id)).await;
        if let Ok(Ok(list)) = listed {
            for m in list["data"].as_array().unwrap_or(&Vec::new()) {
                if let Some(id) = m["model"].as_str() {
                    if m["hidden"] == true {
                        continue;
                    }
                    let efforts = m["supportedReasoningEfforts"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|e| e["reasoningEffort"].as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    models.push(crate::model::ModelInfo {
                        id: id.to_string(),
                        label: id.to_string(),
                        description: None,
                        resolved: None,
                        efforts,
                        default_effort: m["defaultReasoningEffort"].as_str().map(String::from),
                    });
                }
            }
        }
    }
    // Current binaries report the effective thread effort directly. Older
    // ones may omit it, in which case the active model's advertised default
    // is the best available truth for the header until the user changes it.
    if effort.is_none() {
        effort = model.as_deref().and_then(|active| {
            models
                .iter()
                .find(|candidate| candidate.id == active)
                .and_then(|candidate| candidate.default_effort.clone())
        });
    }
    Ok(CodexHandshake {
        thread_id,
        models,
        model,
        effort,
        next_id,
        rollback_error,
    })
}

/// Build the thread-open request separately from I/O so the launch recipe's
/// create-time model contract stays hermetically testable.
fn thread_open_request(spec: &SpawnSpec) -> Value {
    let mut open = match (&spec.pinned_native_id, &spec.fork_at) {
        (Some(thread_id), Some(last_turn_id)) => json!({
            "id": 1, "method": "thread/fork",
            "params": {
                "threadId": thread_id,
                "lastTurnId": last_turn_id,
                "cwd": spec.cwd,
                "ephemeral": false,
            },
        }),
        (Some(thread_id), None) => json!({
            "id": 1, "method": "thread/resume",
            "params": { "threadId": thread_id, "cwd": spec.cwd },
        }),
        (None, _) => json!({
            "id": 1, "method": "thread/start",
            "params": { "cwd": spec.cwd },
        }),
    };
    // The launcher's model selection is thread configuration, not argv, for
    // app-server. Passing it while opening the thread avoids the old seam
    // where a Codex chat silently fell back to the user's default until they
    // changed the model a second time in the header.
    if let Some(model) = &spec.initial_model {
        open["params"]["model"] = json!(model);
    }
    open
}

async fn await_rpc_result(stream: &mut JsonlStream, id: u64) -> std::result::Result<Value, String> {
    loop {
        match stream.next().await {
            Ok(Some(frame)) => {
                if frame["id"] == json!(id) && frame.get("method").is_none() {
                    if let Some(err) = frame.get("error") {
                        return Err(format!("codex request {id} failed: {err}"));
                    }
                    return Ok(frame["result"].clone());
                }
            }
            Ok(None) => return Err("codex exited during handshake".into()),
            Err(err) => return Err(format!("{err:#}")),
        }
    }
}

/// What an outstanding client→server JSON-RPC id is waiting for.
enum PendingRpc {
    TurnStart,
    /// Steer retries once with the live turn id parsed from the error.
    Steer {
        input: Value,
        client_msg_id: String,
        retried: bool,
        /// Stable request order across the one allowed expected-turn retry.
        /// Failed steers re-drive in click order before the ordinary FIFO.
        order: u64,
    },
    Interrupt,
    /// thread/settings/update probe; falls back to per-turn fields on
    /// method-not-found (-32601 / "method not found" — the extension's own
    /// feature detection).
    SettingsUpdate {
        mode_id: String,
        per_turn: Value,
    },
    /// Effort is applied eagerly to the thread when supported, with the
    /// guaranteed `turn/start.effort` path retained as the compatibility
    /// fallback. `previous` lets a rejected value restore honest UI state.
    EffortUpdate {
        effort_id: String,
        previous: Option<String>,
    },
    /// account/read — rate-limit telemetry; `report` also renders /usage.
    AccountRead {
        report: bool,
    },
    /// thread/compact/start ack; the compaction itself runs as its own turn
    /// whose contextCompaction item lands the "context compacted" notice.
    Compact,
}

/// A Codex follow-up held by Chimaera until either the current turn finishes
/// (FIFO: it opens the next turn) or the user explicitly promotes it with the
/// native-style Steer action (`turn/steer`). Keeping this client-side is the
/// app-server contract: queuing is a host policy, steering is the protocol RPC.
struct QueuedSend {
    input: Value,
    client_msg_id: String,
    /// A Steer click can race the `turn/start` → `turn/started` window. Keep
    /// the intent on the queued entry and issue the RPC once the turn id lands.
    steer_when_active: bool,
}

/// An outstanding item/tool/requestUserInput prompt.
struct PendingQuestion {
    /// The server request's JSON-RPC id (the answer goes back by it).
    rpc_id: Value,
    /// Auto-skip deadline (the request's `autoResolutionMs`): the official
    /// client sends empty answers when it fires; ours does too, from the
    /// harness tick.
    deadline: Option<std::time::Instant>,
}

/// How many collab subagent rows we track at once. Real fan-outs are a
/// handful; the cap only guards a runaway turn (login-node budgets).
const COLLAB_AGENTS_CAP: usize = 32;

/// One spawned collab subagent (multi-agent, 0.144.x): the parent thread's
/// `subAgentActivity` markers open/close it, and every frame of the agent's
/// OWN thread — which multiplexes onto this same connection — folds into its
/// row. The row is claude's exact subagent surface: an `Agent: {name}` tool
/// card whose output line carries the latest progress.
/// Emit a fresh subagent progress update only when the token total moved by
/// at least this much — the same order of throttle as claude's ~256-token
/// thinking ticks, so a chatty subagent can't flood the journal.
const COLLAB_TOKEN_STEP: u64 = 256;

struct CollabAgent {
    /// The subagent's thread id — the key every foreign frame carries.
    thread_id: String,
    /// Transcript row id of the CURRENT stint (`agent:{thread}` for the
    /// first, `agent:{thread}#N` after a re-open — see `open` below).
    row_id: String,
    /// The model's own name for the agent (the last `agentPath` segment) —
    /// kept so a re-open can title the new row.
    name: String,
    /// The current row renders in-progress (feeds the AgentsTray). A closed
    /// row is final: the UI's tool-status guard is deliberately monotonic
    /// (completed never walks back to running), so a follow-up/resume that
    /// sets a closed agent working again opens a NEW row (next stint) rather
    /// than fighting the guard; late frames from a closed stint fold into
    /// nothing.
    open: bool,
    /// Stint counter — how many rows this agent thread has had.
    stint: u32,
    /// Tool-ish items completed on the agent's thread (cumulative).
    tools: u64,
    /// Latest cumulative token total for the agent's thread.
    tokens: u64,
    /// Token total at the last emitted progress update (the throttle).
    tokens_emitted: u64,
    /// Latest activity label ("thinking", a command title, "answered", …).
    last: String,
}

impl CollabAgent {
    /// The row's one-line progress — claude's task_progress format
    /// ("{last} · {n} tools · {tok} tokens"), driver-built so replay
    /// reproduces it byte-identically.
    fn progress(&self) -> Option<String> {
        let parts: Vec<String> = [
            (!self.last.is_empty()).then(|| self.last.clone()),
            (self.tools > 0).then(|| format!("{} tools", self.tools)),
            (self.tokens > 0).then(|| format!("{} tokens", self.tokens)),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// [`Self::progress`] as row content.
    fn progress_content(&self) -> Option<ToolContent> {
        self.progress().map(|text| ToolContent::Output {
            text,
            truncated: false,
        })
    }

    /// The in-progress row update carrying [`Self::progress`].
    fn progress_event(&mut self) -> AgentEvent {
        self.tokens_emitted = self.tokens;
        AgentEvent::ToolCallUpdate {
            id: self.row_id.clone(),
            status: ToolStatus::InProgress,
            content: self.progress_content(),
        }
    }
}

/// Protocol → normalized-model translator for the app-server stream. Pure
/// state machine (no I/O), mirroring claude's `Mapper`.
struct CodexMapper {
    thread_id: String,
    models: Vec<crate::model::ModelInfo>,
    /// Launcher-probed `--version` line, echoed on every Init (journal truth).
    agent_version: Option<String>,
    model: Option<String>,
    /// Model override for subsequent turns (set_model).
    pending_model: Option<String>,
    /// Effective/selected reasoning effort for subsequent turns. Current
    /// app-servers persist it through thread/settings/update; older ones use
    /// the guaranteed turn/start override.
    pending_effort: Option<String>,
    /// Last effort value journaled to the UI. The nested option distinguishes
    /// "no event emitted yet" from an authoritative `None` read-back.
    reported_effort: Option<Option<String>>,
    /// Composer agent mode (read-only/auto/full-access/plan): the wire
    /// fields ride each turn/start once settings/update proves unsupported.
    current_mode: String,
    mode_per_turn: Option<Value>,
    settings_update_unsupported: bool,
    turn_id: String,
    turn_active: bool,
    /// A turn/start is in flight but turn/started hasn't landed yet. Sends in
    /// this window must NOT fire a second turn/start (the server rejects it and
    /// the already-echoed user message is lost) — they buffer instead.
    turn_pending: bool,
    /// Mid-turn follow-ups, FIFO. Unlike the old type-through path, ordinary
    /// sends stay here for subsequent turns; only `SteerQueued` promotes one
    /// into the running turn. This mirrors Codex's native queue-vs-steer split.
    queued_sends: VecDeque<QueuedSend>,
    /// Steers whose target turn ended before their RPC answer. Multiple Steer
    /// requests can be in flight; collect their failures by original request
    /// order, then start exactly one after every outstanding steer settles.
    /// They outrank the ordinary FIFO because the user explicitly promoted
    /// them into the earlier run.
    deferred_steer_redrives: BTreeMap<u64, QueuedSend>,
    /// Interrupt watchdog: ticks remaining before we synthesize the abort the
    /// app-server never sent. Armed on `Interrupt`, counted down in `tick`,
    /// disarmed when a turn ends (`reset_turn_state`) or a fresh turn opens.
    /// See `INTERRUPT_GRACE_TICKS`.
    interrupt_grace: Option<u32>,
    /// Idle-flush watchdog: ticks remaining before we rescue a stranded
    /// buffer. Managed in `tick`, armed when the driver is idle (no active or
    /// pending turn) with queued or deferred-steer input a turn end left
    /// stranded. Symmetric to claude's idle-flush; see
    /// `IDLE_FLUSH_GRACE_TICKS`.
    idle_flush_grace: Option<u32>,
    coalescer: Coalescer,
    /// agentMessage item ids that streamed deltas (skip their completed text).
    streamed: HashSet<String>,
    /// Outstanding server approval requests: our request_id →
    /// (JSON-RPC id, option_id → prebuilt decision payload).
    pending_approvals: HashMap<String, (Value, HashMap<String, Value>)>,
    /// Outstanding item/tool/requestUserInput prompts by request_id.
    /// Answers go back as {answers:{questionId:{answers:[label,…]}}}.
    pending_questions: HashMap<String, PendingQuestion>,
    /// One safety-buffering notice per turn (the frame repeats).
    safety_notified: bool,
    /// The deprecated thread/compacted notification may overlap the
    /// contextCompaction item pair. Fold both sources to exactly one start
    /// and one completion event per turn.
    compaction_active: bool,
    compaction_completed: bool,
    /// One auto-decline notice per turn: full-access maps to approvalPolicy
    /// "never" (the official extension's table), so a genuinely blocked
    /// action is DECLINED by codex itself with no approval card possible —
    /// without this notice the agent's own "I'm blocked" prose is the only
    /// trace ("harness is blocking").
    decline_notified: bool,
    pending_rpcs: HashMap<u64, PendingRpc>,
    /// Collab subagents by their thread id, insertion-ordered (see
    /// [`CollabAgent`]). Lives per parent turn, like claude's task map.
    collab_agents: Vec<CollabAgent>,
    /// One over-cap notice per turn (only a >32-live-agent turn ever sees it).
    collab_cap_notified: bool,
    /// fileChange item id → touched paths (approval titles look them up).
    item_locations: HashMap<String, Vec<String>>,
    /// Live output bytes already streamed per item (caps the deltas; the
    /// completed item's aggregatedOutput replaces them authoritatively).
    out_streamed: HashMap<String, usize>,
    /// Latest cumulative token usage (turn/completed carries none here).
    last_usage: Usage,
    /// The last turn-OPENING send's minted uuid — the fork anchor chain for
    /// Checkpoint events. Steered sends join a running turn, while a held
    /// follow-up anchors only when promoted to its own turn; thread/rollback
    /// drops whole turns, so only actual turn openers belong in this chain.
    last_checkpoint: Option<String>,
    /// The embedder's standing MCP consent (`SpawnSpec.mcp_auto_approve`):
    /// matching tool-call elicitations are answered accept without a
    /// PermissionRequest.
    mcp_auto_approve: Option<crate::driver::McpAutoApprove>,
    next_id: u64,
}

fn codex_modes() -> Vec<crate::model::ModeInfo> {
    let mode = |id: &str, label: &str| crate::model::ModeInfo {
        id: id.into(),
        label: label.into(),
    };
    vec![
        mode("read-only", "Read only"),
        mode("auto", "Auto (workspace)"),
        mode("auto-review", "Auto review"),
        mode("full-access", "Full access"),
        mode("plan", "Plan mode"),
    ]
}

impl CodexMapper {
    fn new(
        thread_id: String,
        models: Vec<crate::model::ModelInfo>,
        model: Option<String>,
        effort: Option<String>,
        agent_version: Option<String>,
        mcp_auto_approve: Option<crate::driver::McpAutoApprove>,
        next_id: u64,
    ) -> Self {
        Self {
            thread_id,
            models,
            agent_version,
            model,
            pending_model: None,
            pending_effort: effort.clone(),
            // The handshake queues this exact state immediately after Init.
            reported_effort: Some(effort),
            // codex's shipped default: workspace sandbox, on-request asks.
            current_mode: "auto".to_string(),
            mode_per_turn: None,
            settings_update_unsupported: false,
            turn_id: String::new(),
            turn_active: false,
            turn_pending: false,
            queued_sends: VecDeque::new(),
            deferred_steer_redrives: BTreeMap::new(),
            interrupt_grace: None,
            idle_flush_grace: None,
            coalescer: Coalescer::new(),
            streamed: HashSet::new(),
            pending_approvals: HashMap::new(),
            pending_questions: HashMap::new(),
            safety_notified: false,
            compaction_active: false,
            compaction_completed: false,
            decline_notified: false,
            pending_rpcs: HashMap::new(),
            collab_agents: Vec::new(),
            collab_cap_notified: false,
            item_locations: HashMap::new(),
            out_streamed: HashMap::new(),
            last_usage: Usage::default(),
            last_checkpoint: None,
            mcp_auto_approve,
            // The handshake minted 0..next_id (init, thread open, optional
            // rollback, model/list) and hands over the next free id.
            next_id,
        }
    }

    fn init_event(&self) -> AgentEvent {
        AgentEvent::Init {
            native_session_id: self.thread_id.clone(),
            model: self.model.clone(),
            modes: codex_modes(),
            current_mode: Some(self.current_mode.clone()),
            slash_commands: Vec::new(),
            models: self.models.clone(),
            agent_version: self.agent_version.clone(),
        }
    }

    fn emit_effort_state(&mut self, effort: Option<String>, step: &mut DriverStep) {
        if self.reported_effort.as_ref() == Some(&effort) {
            return;
        }
        self.reported_effort = Some(effort.clone());
        step.events.push(AgentEvent::EffortState {
            effort,
            ultracode: false,
        });
    }

    /// Top-level effort is the normal setting. Plan collaboration mode also
    /// embeds it, so update both copies or the stale nested value can win.
    fn effort_wire_fields(&self, effort: &str) -> Value {
        let mut fields = json!({ "effort": effort });
        if self.current_mode == "plan" {
            fields["collaborationMode"] = mode_wire_fields(
                &self.current_mode,
                self.pending_model.as_deref().or(self.model.as_deref()),
                Some(effort),
            )["collaborationMode"]
                .clone();
        }
        fields
    }

    fn refresh_per_turn_mode_effort(&mut self) {
        if self.mode_per_turn.is_some() {
            self.mode_per_turn = Some(mode_wire_fields(
                &self.current_mode,
                self.pending_model.as_deref().or(self.model.as_deref()),
                self.pending_effort.as_deref(),
            ));
        }
    }

    fn flush(&mut self) -> Option<AgentEvent> {
        self.coalescer.flush()
    }

    fn rpc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn on_frame(&mut self, frame: &Value) -> DriverStep {
        let mut step = DriverStep::default();
        let method = frame["method"].as_str().unwrap_or_default();

        // Server→client REQUEST (id + method): approvals.
        if frame.get("id").is_some() && !method.is_empty() {
            self.on_server_request(frame, &mut step);
            return step;
        }
        // Response to one of our requests.
        if method.is_empty() {
            if let Some(id) = frame["id"].as_u64() {
                self.on_response(id, frame, &mut step);
            }
            return step;
        }

        // One app-server connection multiplexes EVERY thread: a collab
        // subagent's whole transcript (turn/*, item/*, tokenUsage) streams
        // interleaved with the parent's, distinguished only by
        // params.threadId (live 0.144.2). Frames for another thread feed the
        // subagent lane — without this gate a subagent's final answer renders
        // as the parent's prose and its turn/completed closes the parent turn
        // early. serverRequest/resolved stays global: JSON-RPC request ids
        // are connection-scoped, and a subagent-thread ask must still
        // withdraw its card.
        if method != "serverRequest/resolved" {
            if let Some(thread) = frame["params"]["threadId"].as_str() {
                if thread != self.thread_id {
                    self.on_foreign_frame(thread, method, frame, &mut step);
                    return step;
                }
            }
        }

        match method {
            "thread/settings/updated" => {
                let settings = &frame["params"]["threadSettings"];
                if let Some(value) = settings.get("effort") {
                    let effort = value.as_str().map(String::from);
                    let effort_update_pending = self
                        .pending_rpcs
                        .values()
                        .any(|pending| matches!(pending, PendingRpc::EffortUpdate { .. }));
                    // A rapid second click may already be in flight when the
                    // first update's notification arrives. Ignore that stale
                    // read-back; the latest selection will reconcile itself.
                    if !effort_update_pending || effort == self.pending_effort {
                        self.pending_effort = effort.clone();
                        self.refresh_per_turn_mode_effort();
                        self.emit_effort_state(effort, &mut step);
                    }
                }
            }
            "turn/started" => {
                self.turn_id = frame["params"]["turn"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                self.turn_active = true;
                self.turn_pending = false;
                self.compaction_active = false;
                self.compaction_completed = false;
                // A fresh turn is a clean slate for the interrupt watchdog: an
                // interrupt armed against a previous (or idle) state must not
                // abort this new turn.
                self.interrupt_grace = None;
                step.events.push(AgentEvent::TurnStarted {
                    turn_id: self.turn_id.clone(),
                });
                // Ordinary follow-ups remain queued for later turns. Only an
                // explicit Steer click captured during the unidentified start
                // window is promoted now that the turn id is known.
                self.flush_requested_steers(&mut step);
            }
            // A new reasoning-summary section: keep the thought stream
            // readable with a paragraph break instead of one run-on blob.
            "item/reasoning/summaryPartAdded" => {
                let turn = self.turn_id.clone();
                if let Some(flushed) = self.coalescer.push(&turn, ChunkKind::Thought, "\n\n") {
                    step.events.push(flushed);
                }
            }
            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                if let Some(delta) = frame["params"]["delta"].as_str() {
                    let turn = self.turn_id.clone();
                    if let Some(flushed) = self.coalescer.push(&turn, ChunkKind::Thought, delta) {
                        step.events.push(flushed);
                    }
                }
            }
            "item/agentMessage/delta" => {
                if let Some(delta) = frame["params"]["delta"].as_str() {
                    if let Some(item) = frame["params"]["itemId"].as_str() {
                        self.streamed.insert(item.to_string());
                    }
                    let turn = self.turn_id.clone();
                    if let Some(flushed) = self.coalescer.push(&turn, ChunkKind::Message, delta) {
                        step.events.push(flushed);
                    }
                }
            }
            // The proposed plan streams as markdown in plan collaboration
            // mode — user-facing prose, so it renders as agent message.
            "item/plan/delta" => {
                if let Some(delta) = frame["params"]["delta"].as_str() {
                    let turn = self.turn_id.clone();
                    if let Some(flushed) = self.coalescer.push(&turn, ChunkKind::Message, delta) {
                        step.events.push(flushed);
                    }
                }
            }
            // Live command output: bounded append stream; the completed
            // item's aggregatedOutput replaces it wholesale.
            "item/commandExecution/outputDelta" => {
                let params = &frame["params"];
                let (Some(item), Some(delta)) =
                    (params["itemId"].as_str(), params["delta"].as_str())
                else {
                    return step;
                };
                let sent = self.out_streamed.entry(item.to_string()).or_insert(0);
                if *sent >= crate::model::TOOL_OUTPUT_HEAD {
                    return step;
                }
                let budget = crate::model::TOOL_OUTPUT_HEAD - *sent;
                let (text, _) = crate::model::cap_head_tail(delta, budget, 0);
                *sent += text.len();
                step.events.push(AgentEvent::ToolOutputDelta {
                    id: item.to_string(),
                    text,
                });
            }
            "item/started" => self.on_item(&frame["params"]["item"], false, &mut step),
            "item/completed" => self.on_item(&frame["params"]["item"], true, &mut step),
            // Live wholesale-replace of a fileChange item's patch (PROTOCOL.md:
            // item/fileChange/patchUpdated). Re-upsert the row's locations and
            // title so an approval arriving after it names the right files.
            "item/fileChange/patchUpdated" => {
                let params = &frame["params"];
                if let Some(item_id) = params["itemId"].as_str() {
                    let changes = params["changes"].as_array().cloned().unwrap_or_default();
                    self.file_change_upsert(item_id, &changes, &mut step);
                }
            }
            // The turn's todo list (entries {step, status}).
            "turn/plan/updated" => {
                if let Some(plan) = frame["params"]["plan"].as_array() {
                    let entries = plan
                        .iter()
                        .filter_map(|p| {
                            Some(crate::model::PlanEntry {
                                content: truncate_label(
                                    p["step"].as_str()?,
                                    crate::model::PLAN_LABEL_MAX,
                                ),
                                status: match p["status"].as_str() {
                                    Some("inProgress") | Some("in_progress") => {
                                        crate::model::PlanStatus::InProgress
                                    }
                                    Some("completed") => crate::model::PlanStatus::Done,
                                    _ => crate::model::PlanStatus::Todo,
                                },
                                // codex's plan is a bare step list — it has no
                                // id/owner/dependency notion to carry.
                                ..Default::default()
                            })
                        })
                        .take(crate::model::PLAN_TASKS_CAP)
                        .collect();
                    step.events.push(AgentEvent::Plan { entries });
                }
            }
            "thread/tokenUsage/updated" => {
                let usage = &frame["params"]["tokenUsage"];
                let total = &usage["total"];
                self.last_usage = Usage {
                    cost_usd: None,
                    input_tokens: total["inputTokens"].as_u64().unwrap_or(0),
                    output_tokens: total["outputTokens"].as_u64().unwrap_or(0),
                    total_tokens: total["totalTokens"].as_u64().unwrap_or(0),
                    duration_ms: 0,
                    context_window: usage["modelContextWindow"].as_u64(),
                };
                // The context meter reads the LAST request's tokens (what is
                // actually in the window), not the cumulative total — the
                // official client's exact math, min'd against the window.
                if let (Some(last), Some(max)) = (
                    usage["last"]["totalTokens"].as_u64(),
                    usage["modelContextWindow"].as_u64(),
                ) {
                    if max > 0 {
                        let in_window = last.min(max);
                        step.events.push(AgentEvent::ContextUsage {
                            total_tokens: in_window,
                            max_tokens: max,
                            percentage: in_window as f64 / max as f64 * 100.0,
                        });
                    }
                }
            }
            // The server settled one of its own requests (timeout, another
            // client answered, interrupt): withdraw the matching card.
            "serverRequest/resolved" => {
                let request_id = format!("codex-{}", frame["params"]["requestId"]);
                if self.pending_questions.remove(&request_id).is_some() {
                    step.events.push(AgentEvent::QuestionResolved {
                        request_id,
                        answers: Default::default(),
                    });
                } else if self.pending_approvals.remove(&request_id).is_some() {
                    step.events.push(AgentEvent::PermissionResolved {
                        request_id,
                        option_id: "cancelled".into(),
                    });
                }
            }
            // Additional safety checks add latency; say so once per turn.
            "model/safetyBuffering/updated" => {
                if frame["params"]["showBufferingUi"] == json!(true) && !self.safety_notified {
                    self.safety_notified = true;
                    step.events.push(AgentEvent::Notice {
                        text: "this request requires additional safety checks, which can take extra time"
                            .into(),
                    });
                }
            }
            // Approval auto-review is its own long-running app-server lane,
            // not a normal tool item. Promote it into the existing tool-card
            // vocabulary so the user can see what Codex is assessing and the
            // eventual risk/verdict instead of waiting behind silent latency.
            "item/autoApprovalReview/started" => {
                self.on_auto_review(&frame["params"], false, &mut step)
            }
            "item/autoApprovalReview/completed" => {
                self.on_auto_review(&frame["params"], true, &mut step)
            }
            "guardianWarning" => {
                if let Some(message) = frame["params"]["message"].as_str() {
                    step.events.push(AgentEvent::Notice {
                        text: format!("auto review: {}", truncate_label(message, 240)),
                    });
                }
            }
            "thread/name/updated" => {
                if let Some(name) = frame["params"]["threadName"].as_str() {
                    if !name.is_empty() {
                        step.events.push(AgentEvent::SessionTitle {
                            title: name.to_string(),
                        });
                    }
                }
            }
            "thread/compacted" => {
                self.compaction_active = false;
                if !self.compaction_completed {
                    self.compaction_completed = true;
                    step.events.push(AgentEvent::ContextCompaction {
                        phase: CompactionPhase::Completed,
                        pre_tokens: None,
                    });
                }
            }
            // Mined shape: {threadId, turnId, fromModel, toModel, reason} —
            // reasons include safety reroutes (e.g. highRiskCyberActivity).
            "model/rerouted" => {
                let params = &frame["params"];
                if let Some(to) = params["toModel"].as_str() {
                    self.model = Some(to.to_string());
                    step.events.push(AgentEvent::ModelSwitched {
                        from: params["fromModel"].as_str().map(String::from),
                        to: to.to_string(),
                        reason: params["reason"].as_str().map(String::from),
                        retract_current_turn: false,
                    });
                    // The official client's divider wording.
                    step.events.push(AgentEvent::Notice {
                        text: format!("Your request was routed to {to}."),
                    });
                    step.events.push(self.init_event());
                }
            }
            // Turn-level error notification: retried errors are transient.
            "error" => {
                let msg = frame["params"]["error"]["message"]
                    .as_str()
                    .unwrap_or("agent error");
                if frame["params"]["willRetry"] == true {
                    step.events.push(AgentEvent::Notice {
                        text: format!("{msg} (retrying)"),
                    });
                } else {
                    step.events.push(AgentEvent::Error {
                        message: msg.to_string(),
                        fatal: false,
                    });
                }
            }
            "turn/completed" => {
                if let Some(flushed) = self.coalescer.flush() {
                    step.events.push(flushed);
                }
                // A turn-end frame arriving AFTER the interrupt watchdog already
                // closed this turn must NOT emit a second end (symmetry with
                // claude on_result's `was_active` guard).
                let was_active = self.turn_active;
                self.turn_active = false;
                if was_active {
                    let turn = &frame["params"]["turn"];
                    let mut usage = self.last_usage.clone();
                    usage.duration_ms = turn["durationMs"].as_u64().unwrap_or(0);
                    let turn_id = turn["id"].as_str().unwrap_or(&self.turn_id).to_string();
                    if turn["status"] == "interrupted" {
                        // The turn died with its subagents still open: close
                        // their rows as failed BEFORE the abort marker, so
                        // the UI's turn-end reconcile can't flip them green
                        // (claude parity).
                        self.fail_dangling_collab_agents(&mut step);
                        // status "interrupted" only follows a turn/interrupt RPC
                        // — codex's wire carries the user-stop fact structurally.
                        step.events.push(AgentEvent::TurnAborted {
                            turn_id,
                            reason: "interrupted".into(),
                            interrupted: true,
                        });
                    } else {
                        step.events
                            .push(AgentEvent::TurnCompleted { turn_id, usage });
                    }
                    // Refresh rate-limit telemetry once per turn (account/read
                    // is the extension's source; tolerated if absent).
                    let id = self.rpc_id();
                    self.pending_rpcs
                        .insert(id, PendingRpc::AccountRead { report: false });
                    step.outbound.push(json!({
                        "id": id, "method": "account/read",
                        "params": { "refreshToken": false },
                    }));
                }
                self.reset_turn_state();
                if was_active {
                    // Queue means NEXT run whether this turn completed or was
                    // stopped: promote one FIFO entry, leave the rest queued.
                    // AFTER reset_turn_state, which would clobber the new
                    // turn_pending start window.
                    self.start_next_queued(&mut step);
                }
            }
            "turn/failed" => {
                if let Some(flushed) = self.coalescer.flush() {
                    step.events.push(flushed);
                }
                // Same `was_active` guard as turn/completed: a turn already
                // closed by the watchdog must not fail a second time.
                let was_active = self.turn_active;
                self.turn_active = false;
                if was_active {
                    // Subagent rows die with the failed turn (claude parity —
                    // never reconciled green by the UI's turn-end sweep).
                    self.fail_dangling_collab_agents(&mut step);
                    step.events.push(AgentEvent::TurnAborted {
                        turn_id: self.turn_id.clone(),
                        reason: frame["params"]["error"]["message"]
                            .as_str()
                            .unwrap_or("turn failed")
                            .to_string(),
                        interrupted: false,
                    });
                }
                // A failed turn must reset per-turn state exactly like a
                // completed one — else the safety notice stays suppressed and
                // stream/location maps leak into the next turn.
                self.reset_turn_state();
                if was_active {
                    // Only THIS turn died; the oldest queued follow-up still
                    // opens the next turn. The rest remain queued.
                    self.start_next_queued(&mut step);
                }
            }
            _ => {}
        }
        step
    }

    /// Clear everything scoped to a single turn. Called at every turn end
    /// (completed OR failed) so nothing leaks across the turn boundary.
    fn reset_turn_state(&mut self) {
        self.streamed.clear();
        self.out_streamed.clear();
        self.safety_notified = false;
        self.decline_notified = false;
        // A missing terminal item must not strand the UI's compaction lane.
        // Turn end clears it client-side; mark this native lifecycle settled
        // too so a late deprecated thread/compacted notification cannot emit
        // a second completion.
        if self.compaction_active {
            self.compaction_active = false;
            self.compaction_completed = true;
        }
        // Defensive: a turn that ends without ever emitting turn/started must
        // not leave the start-window flag stuck true.
        self.turn_pending = false;
        // The turn ended on its own — the interrupt watchdog has nothing left
        // to abort (a real turn is never double-aborted by it).
        self.interrupt_grace = None;
        // Approvals only ever reference items of the current turn; keeping
        // these forever is unbounded growth over a long session.
        self.item_locations.clear();
        // Collab subagents live per parent turn, like claude's task map
        // (wiped on result). Rows still open on a NORMAL end were left
        // running deliberately — the UI's turn-end reconcile closes them
        // (green), exactly as it does claude's; the abort paths fail them
        // explicitly before this runs.
        self.collab_agents.clear();
        self.collab_cap_notified = false;
    }

    /// A response to one of our client→server requests.
    fn on_response(&mut self, id: u64, frame: &Value, step: &mut DriverStep) {
        let Some(pending) = self.pending_rpcs.remove(&id) else {
            return;
        };
        let error = frame.get("error").filter(|e| !e.is_null());
        match (pending, error) {
            (PendingRpc::TurnStart, Some(err)) => {
                self.turn_pending = false;
                let msg = err["message"].as_str().unwrap_or_default();
                // If a turn was already active, the error names it: adopt that
                // turn and service only explicit start-window Steer requests
                // rather than erroring; ordinary follow-ups remain queued.
                if let Some(live) = parse_expected_turn_id(msg) {
                    self.turn_id = live;
                    self.turn_active = true;
                    self.flush_requested_steers(step);
                } else {
                    step.events.push(AgentEvent::Error {
                        message: format!("turn/start failed: {}", err["message"]),
                        fatal: false,
                    });
                    // Promote one queued send so it isn't stranded.
                    if let Some(queued) = self.queued_sends.pop_front() {
                        self.redrive_as_fresh_turn(queued.input, queued.client_msg_id, step);
                    }
                }
            }
            (
                PendingRpc::Steer {
                    input,
                    client_msg_id,
                    retried: false,
                    order,
                },
                Some(err),
            ) => {
                // The extension's retry dance: parse the live turn id out of
                // the error text and steer again, once.
                let msg = err["message"].as_str().unwrap_or_default();
                match parse_expected_turn_id(msg) {
                    Some(live_turn) => {
                        self.turn_id = live_turn.clone();
                        let id = self.rpc_id();
                        self.pending_rpcs.insert(
                            id,
                            PendingRpc::Steer {
                                input: input.clone(),
                                client_msg_id: client_msg_id.clone(),
                                retried: true,
                                order,
                            },
                        );
                        step.outbound.push(json!({
                            "id": id, "method": "turn/steer",
                            "params": {
                                "threadId": self.thread_id,
                                "clientUserMessageId": client_msg_id,
                                "input": input,
                                "expectedTurnId": live_turn,
                            },
                        }));
                    }
                    // Not a turn-id mismatch: if the turn ended between our send
                    // and this steer, re-drive the saved input as a fresh turn
                    // instead of dropping the already-echoed user message. A
                    // sibling steer may still be unresolved, so defer by click
                    // order and let the shared scheduler open only one turn.
                    None if !self.turn_active => {
                        self.defer_steer_redrive(order, input, client_msg_id, step)
                    }
                    None => {
                        step.events.push(AgentEvent::Error {
                            message: format!("steer failed: {msg}"),
                            fatal: false,
                        });
                        // Final failure: the agent never saw this message.
                        step.events.push(AgentEvent::UserMessageUpdate {
                            id: client_msg_id,
                            state: UserMessageState::Dropped,
                        });
                    }
                }
            }
            (
                PendingRpc::Steer {
                    input,
                    client_msg_id,
                    retried: true,
                    order,
                },
                Some(err),
            ) => {
                if !self.turn_active {
                    self.defer_steer_redrive(order, input, client_msg_id, step);
                } else {
                    step.events.push(AgentEvent::Error {
                        message: format!("steer failed: {}", err["message"]),
                        fatal: false,
                    });
                    // Retried and still refused: the message was not consumed.
                    step.events.push(AgentEvent::UserMessageUpdate {
                        id: client_msg_id,
                        state: UserMessageState::Dropped,
                    });
                }
            }
            (PendingRpc::Interrupt, Some(err)) => {
                // "no active turn to interrupt" is a benign race.
                let msg = err["message"].as_str().unwrap_or_default();
                if msg != "no active turn to interrupt" {
                    step.events.push(AgentEvent::Error {
                        message: format!("interrupt failed: {msg}"),
                        fatal: false,
                    });
                }
            }
            (PendingRpc::SettingsUpdate { mode_id, per_turn }, Some(err)) => {
                if is_method_not_found(err, "thread/settings/update") {
                    // Older app-server: the fields ride every turn/start
                    // instead (the extension's own fallback path).
                    self.settings_update_unsupported = true;
                    self.mode_per_turn = Some(per_turn);
                    self.apply_mode(mode_id, step);
                } else {
                    step.events.push(AgentEvent::Error {
                        message: format!("mode change failed: {}", err["message"]),
                        fatal: false,
                    });
                }
            }
            (
                PendingRpc::EffortUpdate {
                    effort_id,
                    previous,
                },
                Some(err),
            ) => {
                if is_method_not_found(err, "thread/settings/update") {
                    // The selected value still has a guaranteed path: every
                    // subsequent turn/start carries it explicitly.
                    self.settings_update_unsupported = true;
                    self.refresh_per_turn_mode_effort();
                    if self.pending_effort.as_deref() == Some(&effort_id) {
                        self.emit_effort_state(Some(effort_id), step);
                    }
                } else {
                    // Do not roll back a newer click whose request is still in
                    // flight; this response owns only its exact selection.
                    if self.pending_effort.as_deref() == Some(&effort_id) {
                        self.pending_effort = previous.clone();
                        self.refresh_per_turn_mode_effort();
                        self.emit_effort_state(previous, step);
                    }
                    step.events.push(AgentEvent::Error {
                        message: format!("effort change failed: {}", err["message"]),
                        fatal: false,
                    });
                }
            }
            (PendingRpc::AccountRead { .. }, Some(_)) => {
                // Absent on older binaries; the chip just stays empty.
            }
            (PendingRpc::Compact, Some(err)) => {
                step.events.push(AgentEvent::Error {
                    message: format!("compact failed: {}", err["message"]),
                    fatal: false,
                });
            }
            (PendingRpc::SettingsUpdate { mode_id, .. }, None) => {
                self.apply_mode(mode_id, step);
            }
            (PendingRpc::EffortUpdate { effort_id, .. }, None) => {
                if self.pending_effort.as_deref() == Some(&effort_id) {
                    self.emit_effort_state(Some(effort_id), step);
                }
            }
            (PendingRpc::AccountRead { report }, None) => {
                self.on_account(&frame["result"], report, step);
            }
            (PendingRpc::Steer { client_msg_id, .. }, None) => {
                // The steer was accepted: the running turn consumed the
                // message (steering has no follow-up wire item — the echoed
                // userMessage item is deliberately ignored to avoid dupes).
                step.events.push(AgentEvent::UserMessageUpdate {
                    id: client_msg_id,
                    state: UserMessageState::Sent,
                });
                // A turn-end frame can precede the steer acknowledgement. It
                // deliberately held the ordinary FIFO; the last steer answer
                // releases exactly one next turn when the driver is idle.
                self.start_next_queued(step);
            }
            // Compact's ack is an empty result; the compaction turn's
            // contextCompaction item carries the visible notice.
            (PendingRpc::TurnStart | PendingRpc::Interrupt | PendingRpc::Compact, None) => {}
        }
    }

    /// account/read result → RateLimit chip (+ /usage windows on request).
    /// Wire shape: rate_limit.{primary_window,secondary_window} each
    /// {used_percent 0-100, limit_window_seconds, reset_at epoch-s}.
    fn on_account(&mut self, result: &Value, report: bool, step: &mut DriverStep) {
        let rl = &result["rate_limit"];
        let window = |w: &Value| -> Option<(f64, Option<String>, String)> {
            let pct = w["used_percent"].as_f64()?;
            let resets = w["reset_at"].as_u64().map(|s| s.to_string());
            let label = match w["limit_window_seconds"].as_u64() {
                Some(secs) if secs <= 6 * 3600 => "session limit".to_string(),
                Some(secs) => format!("{}d limit", secs / 86_400),
                None => "usage limit".to_string(),
            };
            Some((pct, resets, label))
        };
        let windows: Vec<_> = [&rl["primary_window"], &rl["secondary_window"]]
            .into_iter()
            .filter_map(window)
            .collect();
        if let Some((pct, resets, label)) =
            windows.iter().max_by(|a, b| a.0.total_cmp(&b.0)).cloned()
        {
            step.events.push(AgentEvent::RateLimit {
                utilization: pct,
                resets_at: resets,
                label: Some(label),
                limit_reached: rl["limit_reached"] == true
                    || windows.iter().any(|(p, ..)| *p >= 100.0),
            });
        }
        if report {
            step.events.push(AgentEvent::UsageReport {
                windows: windows
                    .into_iter()
                    .map(
                        |(utilization, resets_at, label)| crate::model::UsageWindow {
                            label,
                            utilization,
                            resets_at,
                        },
                    )
                    .collect(),
            });
        }
    }

    /// Upsert a fileChange tool row from its current changes: remember the
    /// touched paths (approval requests reference the item by id only) and
    /// re-emit the in-progress ToolCall so clients update its locations/title.
    /// Shared by item/started and item/fileChange/patchUpdated.
    fn file_change_upsert(&mut self, id: &str, changes: &[Value], step: &mut DriverStep) {
        let locations: Vec<String> = changes
            .iter()
            .filter_map(|c| c["path"].as_str().map(String::from))
            .collect();
        self.item_locations
            .insert(id.to_string(), locations.clone());
        step.events.push(AgentEvent::ToolCall {
            id: id.to_string(),
            kind: ToolKind::Edit,
            title: match locations.as_slice() {
                [only] => only.clone(),
                many => format!("{} files changed", many.len()),
            },
            locations,
            status: ToolStatus::InProgress,
        });
    }

    fn on_item(&mut self, item: &Value, completed: bool, step: &mut DriverStep) {
        let id = item["id"].as_str().unwrap_or_default().to_string();
        match item["type"].as_str() {
            Some("agentMessage") if completed => {
                // Fallback for messages that never streamed (none observed
                // live, but the completed frame is authoritative).
                if !self.streamed.contains(&id) {
                    if let Some(text) = item["text"].as_str() {
                        if !text.is_empty() {
                            let turn = self.turn_id.clone();
                            if let Some(flushed) =
                                self.coalescer.push(&turn, ChunkKind::Message, text)
                            {
                                step.events.push(flushed);
                            }
                        }
                    }
                }
            }
            Some("commandExecution") => {
                if !completed {
                    if let Some(flushed) = self.coalescer.flush() {
                        step.events.push(flushed);
                    }
                    step.events.push(AgentEvent::ToolCall {
                        id,
                        kind: ToolKind::Execute,
                        title: command_title(item),
                        locations: Vec::new(),
                        status: ToolStatus::InProgress,
                    });
                } else {
                    let failed =
                        matches!(item["status"].as_str(), Some("declined") | Some("failed"))
                            || item["exitCode"].as_i64().is_some_and(|c| c != 0);
                    if item["status"] == "declined" {
                        self.note_auto_decline(step);
                    }
                    let output = item["aggregatedOutput"].as_str().unwrap_or_default();
                    let content = if output.is_empty() {
                        None
                    } else {
                        let (text, truncated) = cap_output(output);
                        Some(ToolContent::Output { text, truncated })
                    };
                    step.events.push(AgentEvent::ToolCallUpdate {
                        id,
                        status: if failed {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Completed
                        },
                        content,
                    });
                }
            }
            Some("fileChange") => {
                // Extension-mined shape: changes[{path, diff, kind:{type:
                // add|delete|update, move_path}}]; diff is full content for
                // add/delete, unified hunks for update.
                let changes = item["changes"].as_array().cloned().unwrap_or_default();
                if !completed {
                    self.file_change_upsert(&id, &changes, step);
                } else {
                    // Approval requests reference the item by id only; keep the
                    // touched paths so a late approval card can still name them.
                    let locations: Vec<String> = changes
                        .iter()
                        .filter_map(|c| c["path"].as_str().map(String::from))
                        .collect();
                    self.item_locations.insert(id.clone(), locations);
                    let failed =
                        matches!(item["status"].as_str(), Some("declined") | Some("failed"));
                    if item["status"] == "declined" {
                        self.note_auto_decline(step);
                    }
                    let diffs: Vec<ToolContent> = changes
                        .iter()
                        .filter_map(|c| {
                            let path = c["path"].as_str()?.to_string();
                            let diff = c["diff"].as_str().unwrap_or_default();
                            let (text, truncated) = cap_output(diff);
                            Some(match c["kind"]["type"].as_str() {
                                Some("delete") => ToolContent::Diff {
                                    path,
                                    old_text: Some(text),
                                    new_text: String::new(),
                                    truncated,
                                },
                                // add: full content; update: unified hunks —
                                // both render in the new-pane style.
                                _ => ToolContent::Diff {
                                    path,
                                    old_text: None,
                                    new_text: text,
                                    truncated,
                                },
                            })
                        })
                        .collect();
                    step.events.push(AgentEvent::ToolCallUpdate {
                        id,
                        status: if failed {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Completed
                        },
                        content: match diffs.len() {
                            0 => None,
                            1 => diffs.into_iter().next(),
                            _ => Some(ToolContent::Batch { diffs }),
                        },
                    });
                }
            }
            Some("mcpToolCall") => {
                let title = format!(
                    "{}.{}",
                    item["server"].as_str().unwrap_or("mcp"),
                    item["tool"].as_str().unwrap_or("tool"),
                );
                if !completed {
                    if let Some(flushed) = self.coalescer.flush() {
                        step.events.push(flushed);
                    }
                    step.events.push(AgentEvent::ToolCall {
                        id,
                        kind: ToolKind::Other,
                        title,
                        locations: Vec::new(),
                        status: ToolStatus::InProgress,
                    });
                } else {
                    let failed = item["error"].is_object()
                        || matches!(item["status"].as_str(), Some("failed") | Some("declined"));
                    // MCP CallToolResult: content[{type:"text",text}] parts.
                    let text: String = item["result"]["content"]
                        .as_array()
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    let text = if failed && text.is_empty() {
                        item["error"]["message"].as_str().unwrap_or("").to_string()
                    } else {
                        text
                    };
                    let content = if text.is_empty() {
                        None
                    } else {
                        let (text, truncated) = cap_output(&text);
                        Some(ToolContent::Output { text, truncated })
                    };
                    step.events.push(AgentEvent::ToolCallUpdate {
                        id,
                        status: if failed {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Completed
                        },
                        content,
                    });
                }
            }
            Some("webSearch") => {
                if !completed {
                    if let Some(flushed) = self.coalescer.flush() {
                        step.events.push(flushed);
                    }
                    step.events.push(AgentEvent::ToolCall {
                        id,
                        kind: ToolKind::Fetch,
                        title: format!("search: {}", item["query"].as_str().unwrap_or("the web")),
                        locations: Vec::new(),
                        status: ToolStatus::InProgress,
                    });
                } else {
                    // Honor the item status like every other item type — a
                    // failed search must not render as a successful one.
                    let failed =
                        matches!(item["status"].as_str(), Some("failed") | Some("declined"));
                    step.events.push(AgentEvent::ToolCallUpdate {
                        id,
                        status: if failed {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Completed
                        },
                        content: None,
                    });
                }
            }
            // Generated images: savedPath (fs path) opens in the native
            // image preview via the card's locations.
            Some("imageGeneration") => {
                let saved = item["savedPath"].as_str().unwrap_or_default();
                let failed = matches!(item["status"].as_str(), Some("failed") | Some("declined"));
                if !completed {
                    if let Some(flushed) = self.coalescer.flush() {
                        step.events.push(flushed);
                    }
                    step.events.push(AgentEvent::ToolCall {
                        id,
                        kind: ToolKind::Other,
                        title: "generating image".into(),
                        locations: Vec::new(),
                        status: ToolStatus::InProgress,
                    });
                } else {
                    // Re-emit with the saved path so the open affordance
                    // exists (clients upsert tool rows by id).
                    step.events.push(AgentEvent::ToolCall {
                        id: id.clone(),
                        kind: ToolKind::Other,
                        title: match item["revisedPrompt"].as_str() {
                            Some(p) if !p.is_empty() => {
                                format!("image: {}", truncate_label(p, 120))
                            }
                            _ => "generated image".into(),
                        },
                        locations: if saved.is_empty() {
                            Vec::new()
                        } else {
                            vec![saved.to_string()]
                        },
                        status: ToolStatus::InProgress,
                    });
                    step.events.push(AgentEvent::ToolCallUpdate {
                        id,
                        status: if failed {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Completed
                        },
                        content: if saved.is_empty() {
                            None
                        } else {
                            Some(ToolContent::Output {
                                text: saved.to_string(),
                                truncated: false,
                            })
                        },
                    });
                }
            }
            // A collab tool call the model made (multi-agent, 0.144.x). Live,
            // only "wait" surfaces as an item — spawn/input/close appear as
            // subAgentActivity markers instead — but unseen tools render too.
            Some("collabAgentToolCall") => {
                let tool = item["tool"].as_str().unwrap_or("collab");
                let title = match tool {
                    "wait" => "waiting for subagents".to_string(),
                    other => match item["prompt"].as_str() {
                        Some(p) if !p.is_empty() => {
                            format!("collab {other}: {}", truncate_label(p, 120))
                        }
                        _ => format!("collab {other}"),
                    },
                };
                if let Some(flushed) = self.coalescer.flush() {
                    step.events.push(flushed);
                }
                // Upsert the row even on completed: an instant call may
                // never emit item/started (clients upsert tool rows by id).
                step.events.push(AgentEvent::ToolCall {
                    id: id.clone(),
                    kind: ToolKind::Other,
                    title,
                    locations: Vec::new(),
                    status: ToolStatus::InProgress,
                });
                if completed {
                    let failed =
                        matches!(item["status"].as_str(), Some("failed") | Some("declined"));
                    step.events.push(AgentEvent::ToolCallUpdate {
                        id,
                        status: if failed {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Completed
                        },
                        content: None,
                    });
                }
            }
            // A collab tool acted on a subagent: spawn ("started"), follow-up
            // input ("interacted"), shutdown ("interrupted"). The marker
            // arrives as item/completed only; item.id is the collab CALL id,
            // so the subagent's THREAD id is the stable row key.
            Some("subAgentActivity") if completed => {
                let thread = item["agentThreadId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if thread.is_empty() {
                    return;
                }
                // The model's own name for the agent is the last agentPath
                // segment ("/root/agent_a" → "agent_a").
                let name = item["agentPath"]
                    .as_str()
                    .unwrap_or_default()
                    .rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("subagent")
                    .to_string();
                let name = truncate_label(&name, 60);
                match item["kind"].as_str().unwrap_or_default() {
                    "started" => self.collab_agent_open(&thread, &name, "", step),
                    "interacted" => self.collab_agent_open(&thread, &name, "follow-up input", step),
                    "interrupted" => {
                        self.collab_agent_close(&thread, ToolStatus::Completed, "closed", step)
                    }
                    // Unseen kinds (binary mining also names spawn/compaction
                    // variants): open-or-note with the agent's own word, so a
                    // spawn that arrives under an unmined kind still creates
                    // the row — an invisible subagent is worse than a
                    // spuriously re-opened one (its turn end closes it).
                    other => {
                        let label = truncate_label(other, 40);
                        self.collab_agent_open(&thread, &name, &label, step);
                    }
                }
            }
            Some("contextCompaction") => {
                if completed {
                    self.compaction_active = false;
                    if !self.compaction_completed {
                        self.compaction_completed = true;
                        step.events.push(AgentEvent::ContextCompaction {
                            phase: CompactionPhase::Completed,
                            pre_tokens: None,
                        });
                    }
                } else if !self.compaction_active {
                    self.compaction_active = true;
                    self.compaction_completed = false;
                    step.events.push(AgentEvent::ContextCompaction {
                        phase: CompactionPhase::Started,
                        pre_tokens: None,
                    });
                }
            }
            // enteredReviewMode / exitedReviewMode / sleep / imageGeneration
            // etc. are tolerated silently (the official client renders
            // nothing for them either).
            _ => {}
        }
    }

    /// Render one auto-review lifecycle update through the normalized tool
    /// surface. The upstream payload is explicitly unstable, so this reads
    /// only the documented action discriminator and review summary; unknown
    /// additions degrade to a generic row without changing our public wire.
    fn on_auto_review(&mut self, params: &Value, completed: bool, step: &mut DriverStep) {
        let review_id = params["reviewId"].as_str().unwrap_or_default();
        if review_id.is_empty() {
            return;
        }
        if let Some(flushed) = self.coalescer.flush() {
            step.events.push(flushed);
        }
        let (kind, action, locations) = auto_review_action(&params["action"]);
        let id = format!("auto-review:{review_id}");
        step.events.push(AgentEvent::ToolCall {
            id: id.clone(),
            kind,
            title: format!("auto review · {action}"),
            locations,
            status: ToolStatus::InProgress,
        });
        if !completed {
            return;
        }

        let review = &params["review"];
        let verdict = review["status"].as_str().unwrap_or("completed");
        let mut summary = verdict.to_string();
        if let Some(risk) = review["riskLevel"].as_str().filter(|s| !s.is_empty()) {
            summary.push_str(" · risk ");
            summary.push_str(risk);
        }
        if let Some(rationale) = review["rationale"].as_str().filter(|s| !s.is_empty()) {
            summary.push('\n');
            summary.push_str(rationale);
        }
        let (text, truncated) = cap_output(&summary);
        step.events.push(AgentEvent::ToolCallUpdate {
            id,
            // A denial is a successful safety verdict, not a failed tool.
            // Only a reviewer that timed out/aborted should hold the card open
            // in the UI's failure treatment.
            status: if matches!(verdict, "timedOut" | "aborted") {
                ToolStatus::Failed
            } else {
                ToolStatus::Completed
            },
            content: Some(ToolContent::Output { text, truncated }),
        });
    }

    /// Apply a resolved mode locally: record it, tell the UI (ModeChanged),
    /// and — the first time the user switches INTO full-access — announce the
    /// contract. Full access maps to approvalPolicy "never", so the only other
    /// visible effect is that approval cards stop appearing, which reads as
    /// "nothing happened"; the reactive per-action counterpart is
    /// `note_auto_decline`. Called from all three mode-application paths (the
    /// live settings/update ack, the -32601 fallback, and the
    /// already-unsupported per-turn path) so they stay consistent.
    fn apply_mode(&mut self, mode_id: String, step: &mut DriverStep) {
        let entered_full = mode_id == "full-access" && self.current_mode != "full-access";
        let entered_review = mode_id == "auto-review" && self.current_mode != "auto-review";
        self.current_mode = mode_id.clone();
        step.events.push(AgentEvent::ModeChanged { mode_id });
        if entered_full {
            step.events.push(AgentEvent::Notice {
                text: "full access on — codex will no longer ask for approval; a \
                       genuinely blocked action is auto-declined rather than prompted"
                    .into(),
            });
        }
        if entered_review {
            step.events.push(AgentEvent::Notice {
                text: "auto review on — codex will assess approval requests and show each verdict"
                    .into(),
            });
        }
    }

    /// A declined item in full-access mode was declined by CODEX ITSELF:
    /// approvalPolicy "never" (the official extension's full-access mapping)
    /// means no approval request can exist, so the auto-decline would
    /// otherwise surface only as a failed tool card plus the agent's vague
    /// "I'm blocked" narration. Name the mechanism once per turn. In every
    /// other mode a declined status follows the user's own deny — no notice.
    fn note_auto_decline(&mut self, step: &mut DriverStep) {
        if self.current_mode != "full-access" || self.decline_notified {
            return;
        }
        self.decline_notified = true;
        step.events.push(AgentEvent::Notice {
            text: "codex declined this action itself — full access never asks \
                   for approval (switch to auto mode to be asked instead)"
                .into(),
        });
    }

    /// The tracked agent for a subagent thread id, if any.
    fn collab_agent_mut(&mut self, thread: &str) -> Option<&mut CollabAgent> {
        self.collab_agents
            .iter_mut()
            .find(|a| a.thread_id == thread)
    }

    /// Find-or-create the Agent tool row for a subagent thread and (re)open
    /// it. The row is claude's exact subagent surface — `Agent: {name}`,
    /// `ToolKind::Agent`, progress folded into its output line — which is
    /// also what the AgentsTray derives from. Re-opening a CLOSED agent
    /// (follow-up input, a resumed thread) opens a NEW row for the next
    /// stint: the UI's tool-status guard is monotonic by design (a finished
    /// row never walks back to running), so the fresh work gets a fresh row
    /// instead of an update the client would rightly drop.
    fn collab_agent_open(&mut self, thread: &str, name: &str, note: &str, step: &mut DriverStep) {
        if let Some(idx) = self
            .collab_agents
            .iter()
            .position(|a| a.thread_id == thread)
        {
            let reopened = !self.collab_agents[idx].open;
            if reopened {
                self.collab_agents[idx].stint += 1;
                self.collab_agents[idx].row_id =
                    format!("agent:{thread}#{}", self.collab_agents[idx].stint);
                self.collab_agents[idx].open = true;
                if let Some(flushed) = self.coalescer.flush() {
                    step.events.push(flushed);
                }
                step.events.push(AgentEvent::ToolCall {
                    id: self.collab_agents[idx].row_id.clone(),
                    kind: ToolKind::Agent,
                    title: format!("Agent: {}", self.collab_agents[idx].name),
                    locations: Vec::new(),
                    status: ToolStatus::InProgress,
                });
            }
            let agent = &mut self.collab_agents[idx];
            if !note.is_empty() {
                agent.last = note.to_string();
            }
            if !note.is_empty() || reopened {
                step.events.push(agent.progress_event());
            }
            return;
        }
        // Cap: forget the oldest CLOSED row to make room; when every slot is
        // somehow a live agent, the newest is NOT tracked — a synthetic close
        // would lie about a still-running agent, so say so once instead (its
        // frames fold into nothing until a slot frees up).
        if self.collab_agents.len() >= COLLAB_AGENTS_CAP {
            match self.collab_agents.iter().position(|a| !a.open) {
                Some(idx) => {
                    self.collab_agents.remove(idx);
                }
                None => {
                    if !self.collab_cap_notified {
                        self.collab_cap_notified = true;
                        step.events.push(AgentEvent::Notice {
                            text: format!(
                                "more than {COLLAB_AGENTS_CAP} live subagents — the newest are not tracked"
                            ),
                        });
                    }
                    return;
                }
            }
        }
        let row_id = format!("agent:{thread}");
        if let Some(flushed) = self.coalescer.flush() {
            step.events.push(flushed);
        }
        step.events.push(AgentEvent::ToolCall {
            id: row_id.clone(),
            kind: ToolKind::Agent,
            title: format!("Agent: {name}"),
            locations: Vec::new(),
            status: ToolStatus::InProgress,
        });
        let mut agent = CollabAgent {
            thread_id: thread.to_string(),
            row_id,
            name: name.to_string(),
            open: true,
            stint: 1,
            tools: 0,
            tokens: 0,
            tokens_emitted: 0,
            last: note.to_string(),
        };
        if !note.is_empty() {
            step.events.push(agent.progress_event());
        }
        self.collab_agents.push(agent);
    }

    /// Close a subagent's current row: it answered (its own turn completed),
    /// was shut down ("interrupted" activity), or its turn failed. The entry
    /// stays parked so trailing frames from the closed stint fold into
    /// nothing; new work re-opens as a fresh row (see `collab_agent_open`).
    fn collab_agent_close(
        &mut self,
        thread: &str,
        status: ToolStatus,
        note: &str,
        step: &mut DriverStep,
    ) {
        let Some(agent) = self.collab_agent_mut(thread) else {
            return;
        };
        if !agent.open {
            return;
        }
        agent.open = false;
        agent.last = note.to_string();
        step.events.push(AgentEvent::ToolCallUpdate {
            id: agent.row_id.clone(),
            status,
            content: agent.progress_content(),
        });
    }

    /// Fold an activity label into a subagent's progress line.
    fn collab_agent_note(&mut self, thread: &str, label: &str, step: &mut DriverStep) {
        let Some(agent) = self.collab_agent_mut(thread) else {
            return;
        };
        agent.last = label.to_string();
        if agent.open {
            step.events.push(agent.progress_event());
        }
    }

    /// The parent turn died (interrupt, failure, watchdog): close every
    /// still-open subagent row as failed — their own turn ends will never
    /// render, and the UI's turn-end reconcile would otherwise flip them to a
    /// green "completed". Same wording and semantics as claude's
    /// `fail_dangling_tasks`. Clears the set it drains.
    fn fail_dangling_collab_agents(&mut self, step: &mut DriverStep) {
        for agent in std::mem::take(&mut self.collab_agents) {
            if !agent.open {
                continue;
            }
            step.events.push(AgentEvent::ToolCallUpdate {
                id: agent.row_id,
                status: ToolStatus::Failed,
                content: Some(ToolContent::Output {
                    text: "subagent stopped with the turn".into(),
                    truncated: false,
                }),
            });
        }
    }

    /// A frame for a thread that is not ours: a collab subagent working (its
    /// entire transcript multiplexes onto this one connection, live 0.144.2).
    /// The official surfaces keep subagent transcripts out of the parent's —
    /// claude hides parent_tool_use_id-tagged frames the same way — so
    /// nothing here reaches the main transcript; the visible surface is the
    /// Agent row's progress line.
    fn on_foreign_frame(
        &mut self,
        thread: &str,
        method: &str,
        frame: &Value,
        step: &mut DriverStep,
    ) {
        match method {
            "item/started" | "item/completed" => {
                let item = &frame["params"]["item"];
                let completed = method == "item/completed";
                let (label, is_tool): (String, bool) = match item["type"].as_str() {
                    Some("commandExecution") => (command_title(item), true),
                    Some("fileChange") => {
                        // Record the touched paths even for a SUBAGENT's
                        // fileChange: its requestApproval (dispatched before
                        // the thread gate) resolves the card's file list from
                        // item_locations by itemId — without this the user
                        // approves a subagent's write blind.
                        let changes = item["changes"].as_array().cloned().unwrap_or_default();
                        if let Some(item_id) = item["id"].as_str() {
                            let locations: Vec<String> = changes
                                .iter()
                                .filter_map(|c| c["path"].as_str().map(String::from))
                                .collect();
                            self.item_locations.insert(item_id.to_string(), locations);
                        }
                        let label = if changes.len() > 1 {
                            format!("editing {} files", changes.len())
                        } else {
                            "editing a file".to_string()
                        };
                        (label, true)
                    }
                    Some("mcpToolCall") => (
                        truncate_label(
                            &format!(
                                "{}.{}",
                                item["server"].as_str().unwrap_or("mcp"),
                                item["tool"].as_str().unwrap_or("tool"),
                            ),
                            80,
                        ),
                        true,
                    ),
                    Some("webSearch") => (
                        format!(
                            "search: {}",
                            truncate_label(item["query"].as_str().unwrap_or("the web"), 60),
                        ),
                        true,
                    ),
                    Some("imageGeneration") => ("generating an image".to_string(), true),
                    // A subagent delegating further (nested agents get no
                    // rows of their own; their work reads as this agent's).
                    Some("collabAgentToolCall") => ("delegating".to_string(), true),
                    Some("reasoning") => ("thinking".to_string(), false),
                    Some("agentMessage") => ("replying".to_string(), false),
                    _ => return,
                };
                let Some(agent) = self.collab_agent_mut(thread) else {
                    return;
                };
                agent.last = label;
                // One progress emit per item: the started frame carries the
                // label change; the completed frame only matters when it
                // bumps the tool count (the label is the same item's).
                if completed && is_tool {
                    agent.tools += 1;
                }
                if agent.open && (!completed || is_tool) {
                    step.events.push(agent.progress_event());
                }
            }
            // A new turn on the agent's thread — a follow-up/resume set it
            // working again. A closed agent re-opens as a fresh row (stint).
            "turn/started" => {
                if let Some(agent) = self.collab_agent_mut(thread) {
                    let name = agent.name.clone();
                    self.collab_agent_open(thread, &name, "running", step);
                }
            }
            // The agent's turn ended: it answered and sits idle awaiting
            // follow-ups — the row closes; an "interacted" marker re-opens it.
            // A deliberate stop ("interrupted", the close_agent path) closes
            // quietly with the agent's own word — claude renders stopped
            // subagents the same way (only "failed" verdicts go red).
            "turn/completed" => {
                let word = if frame["params"]["turn"]["status"] == "interrupted" {
                    "interrupted"
                } else {
                    "answered"
                };
                self.collab_agent_close(thread, ToolStatus::Completed, word, step);
            }
            "turn/failed" => {
                let reason = frame["params"]["error"]["message"]
                    .as_str()
                    .unwrap_or("turn failed");
                let reason = truncate_label(reason, 80);
                self.collab_agent_close(thread, ToolStatus::Failed, &reason, step);
            }
            // A turn-level error on the agent's thread (retryable or not):
            // fold the message into the row so the failure isn't invisible —
            // the thread's own turn/failed still closes the row if terminal.
            "error" => {
                let msg = frame["params"]["error"]["message"]
                    .as_str()
                    .unwrap_or("agent error");
                let label = truncate_label(&format!("error: {msg}"), 80);
                self.collab_agent_note(thread, &label, step);
            }
            "thread/tokenUsage/updated" => {
                let Some(agent) = self.collab_agent_mut(thread) else {
                    return;
                };
                if let Some(total) = frame["params"]["tokenUsage"]["total"]["totalTokens"].as_u64()
                {
                    agent.tokens = total;
                }
                // Throttled: token totals tick on every API call the agent
                // makes — only a meaningful move earns a journaled update.
                if agent.open && agent.tokens.abs_diff(agent.tokens_emitted) >= COLLAB_TOKEN_STEP {
                    step.events.push(agent.progress_event());
                }
            }
            // Everything else on a foreign thread (status flips, mcp startup,
            // name updates, deltas, model reroutes) is that thread's own
            // business — never the parent transcript's.
            _ => {}
        }
    }

    fn steer_in_flight(&self) -> bool {
        self.pending_rpcs
            .values()
            .any(|pending| matches!(pending, PendingRpc::Steer { .. }))
    }

    /// A steer missed the turn it targeted. Keep it ahead of the ordinary
    /// FIFO, but do not open a turn until every sibling steer has answered —
    /// otherwise two late failures can race two `turn/start` requests.
    fn defer_steer_redrive(
        &mut self,
        order: u64,
        input: Value,
        client_msg_id: String,
        step: &mut DriverStep,
    ) {
        self.deferred_steer_redrives.insert(
            order,
            QueuedSend {
                input,
                client_msg_id,
                steer_when_active: false,
            },
        );
        self.start_next_queued(step);
    }

    /// Deliver exactly one waiting follow-up as the next fresh turn. Failed
    /// explicit steers go first in click order, then the ordinary FIFO. The
    /// rest stay queued for later turns (native Codex queue semantics).
    ///
    /// A live/pending turn or any unresolved steer gates promotion. A late
    /// steer response calls this again, so the final acknowledgement releases
    /// one request without ever issuing concurrent `turn/start`s. Turn-end
    /// callers run this AFTER `reset_turn_state`, which clears `turn_pending`.
    fn start_next_queued(&mut self, step: &mut DriverStep) {
        if self.turn_active || self.turn_pending || self.steer_in_flight() {
            return;
        }
        let queued = self
            .deferred_steer_redrives
            .pop_first()
            .map(|(_, queued)| queued)
            .or_else(|| self.queued_sends.pop_front());
        let Some(queued) = queued else { return };
        self.redrive_as_fresh_turn(queued.input, queued.client_msg_id, step);
    }

    /// Drop every user message still queued behind the current turn — the
    /// not-yet-sent `queued_sends` and any in-flight `Steer` RPC. This is
    /// the DEAD-OR-UNRESPONSIVE-agent resolution only (teardown, and the
    /// interrupt watchdog whose firing means codex stopped answering): with
    /// no live agent to deliver to or answer a steer, `dropped` is the honest
    /// terminal state — and removing the pending steers stops a late error
    /// from resurrecting a message against a gone process. A LIVE abort
    /// (turn/completed interrupted, turn/failed) starts the next queued turn
    /// instead — see `start_next_queued`.
    fn drain_queued_sends(&mut self) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for (_, queued) in std::mem::take(&mut self.deferred_steer_redrives) {
            events.push(AgentEvent::UserMessageUpdate {
                id: queued.client_msg_id,
                state: UserMessageState::Dropped,
            });
        }
        for queued in std::mem::take(&mut self.queued_sends) {
            events.push(AgentEvent::UserMessageUpdate {
                id: queued.client_msg_id,
                state: UserMessageState::Dropped,
            });
        }
        let steer_ids: Vec<u64> = self
            .pending_rpcs
            .iter()
            .filter(|(_, p)| matches!(p, PendingRpc::Steer { .. }))
            .map(|(id, _)| *id)
            .collect();
        for id in steer_ids {
            if let Some(PendingRpc::Steer { client_msg_id, .. }) = self.pending_rpcs.remove(&id) {
                events.push(AgentEvent::UserMessageUpdate {
                    id: client_msg_id,
                    state: UserMessageState::Dropped,
                });
            }
        }
        events
    }

    /// Teardown resolutions: every pending ask's reply route is this
    /// process's JSON-RPC channel, so the journal must not outlive it with
    /// the ask dangling (a replay would strand the card forever — see the
    /// harness's drain call in `run_driver`).
    fn drain_pending(&mut self) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for request_id in std::mem::take(&mut self.pending_questions).into_keys() {
            events.push(AgentEvent::QuestionResolved {
                request_id,
                answers: Default::default(),
            });
        }
        for request_id in std::mem::take(&mut self.pending_approvals).into_keys() {
            events.push(AgentEvent::PermissionResolved {
                request_id,
                option_id: "expired".into(),
            });
        }
        // The subagents die with the process: a still-open Agent row with no
        // terminal event would replay as running forever (and the UI's
        // turn-end reconcile would flip it green — a crash shown as success).
        let mut step = DriverStep::default();
        self.fail_dangling_collab_agents(&mut step);
        events.extend(step.events);
        // A hard kill mid-queue must not strand a queued message as "queued"
        // forever on replay — resolve them dropped, like a turn abort would.
        events.extend(self.drain_queued_sends());
        events
    }

    /// Server→client approval requests. Decision payloads are prebuilt per
    /// option here (string or object union — snake_case inside the object
    /// variants); the Permission command just looks its option up. Unknown
    /// decision strings are silently treated as decline by the server, so
    /// only mined/verified shapes are ever offered.
    fn on_server_request(&mut self, frame: &Value, step: &mut DriverStep) {
        let rpc_id = frame["id"].clone();
        let params = &frame["params"];
        let method = frame["method"].as_str().unwrap_or_default();
        let request_id = format!("codex-{}", rpc_id);

        // Codex asking the user structured questions (mined: answers keyed
        // by question id, each {answers:[string,…]}).
        if method == "item/tool/requestUserInput" {
            let questions = params["questions"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|q| {
                            Some(crate::model::Question {
                                id: q["id"].as_str()?.to_string(),
                                header: q["header"].as_str().unwrap_or_default().to_string(),
                                question: q["question"].as_str()?.to_string(),
                                options: crate::model::question_options(q),
                                multi_select: false,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !questions.is_empty() {
                // autoResolutionMs: the server wants this prompt auto-skipped
                // after the timeout (the official client's behavior) — the
                // harness tick expires it with empty answers + a notice.
                let auto_resolution_ms = params["autoResolutionMs"].as_u64();
                let deadline = auto_resolution_ms.and_then(|ms| {
                    std::time::Instant::now().checked_add(Duration::from_millis(ms))
                });
                let expires_at_ms = auto_resolution_ms.and_then(|ms| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .and_then(|elapsed| {
                            u64::try_from(elapsed.as_millis())
                                .ok()
                                .and_then(|now| now.checked_add(ms))
                        })
                });
                self.pending_questions
                    .insert(request_id.clone(), PendingQuestion { rpc_id, deadline });
                step.events.push(AgentEvent::QuestionRequest {
                    request_id,
                    questions,
                    expires_at_ms,
                });
            } else {
                // Nothing to ask — answer empty rather than parking.
                step.outbound
                    .push(json!({ "id": rpc_id, "result": { "answers": {} } }));
            }
            return;
        }

        let mut options = Vec::new();
        let mut decisions: HashMap<String, Value> = HashMap::new();
        let opt = |options: &mut Vec<PermissionOption>,
                   decisions: &mut HashMap<String, Value>,
                   id: &str,
                   label: String,
                   kind: PermissionOptionKind,
                   decision: Value| {
            options.push(PermissionOption {
                id: id.into(),
                label,
                kind,
            });
            decisions.insert(id.into(), decision);
        };

        // MCP-server elicitation (mined live, codex 0.144): a per-tool MCP
        // approval arrives as `mcpServer/elicitation/request` with
        // `_meta.codex_approval_kind: "mcp_tool_call"` — NOT as an
        // item/*/requestApproval. Its response is the MCP elicitation shape
        // with a REQUIRED `action` (`accept` / `decline`); an approval-style
        // `{"decision": …}` fails deserialization server-side and codex
        // silently rejects the tool call. `_meta.persist` advertises
        // session/always variants, but their accept payload is unmined —
        // only the verified once/decline pair is offered (unknown decision
        // shapes decline silently, so nothing unverified may ship).
        if method == "mcpServer/elicitation/request" {
            let server = params["serverName"].as_str().unwrap_or("mcp");
            let raw_title = params["message"]
                .as_str()
                .filter(|m| !m.is_empty())
                .map(String::from)
                .unwrap_or_else(|| format!("{server} MCP tool call"));
            // The embedder's standing consent (SpawnSpec.mcp_auto_approve):
            // answered HERE because the app-server elicits every MCP tool
            // call — approval-mode config, the granular approval_policy, and
            // a thread-level approvalPolicy "never" were all live-probed and
            // none gates it (Pass 19). Scoped hard: tool-call approvals only
            // (`codex_approval_kind`), one server, and the tool name parsed
            // against the EXACT pinned message shape with a quote-free
            // charset — the model-requested name is interpolated verbatim
            // into this message, so a last-quoted-span parse would let an
            // injected name like `x" run tool "read_session` impersonate a
            // read tool. A genuine MCP-server form elicitation, another
            // server, a reworded message, or a charset-violating name all
            // still surface to the user (with a warn when a consent existed,
            // so pinned-shape drift is diagnosable instead of silently
            // degrading ask mode to prompt-on-every-read).
            if params["_meta"]["codex_approval_kind"] == json!("mcp_tool_call") {
                if let Some(allow) = self
                    .mcp_auto_approve
                    .as_ref()
                    .filter(|a| a.server == server)
                {
                    let tool = elicitation_tool_name(&raw_title, server);
                    // Whole-server consent doesn't hinge on the name — the
                    // structured serverName + codex_approval_kind already
                    // scope it, and auto mode must survive a rewording.
                    let approved = match (&allow.tools, tool) {
                        (None, _) => true,
                        (Some(list), Some(t)) => list.iter().any(|x| x == t),
                        (Some(_), None) => false,
                    };
                    if approved {
                        step.outbound.push(json!({
                            "id": rpc_id,
                            "result": { "action": "accept", "content": {} },
                        }));
                        return;
                    }
                    if tool.is_none() {
                        tracing::warn!(
                            server = %server,
                            message = %raw_title,
                            "codex MCP elicitation message did not match the pinned \
                             shape; surfacing the prompt instead of pre-approving"
                        );
                    }
                }
            }
            opt(
                &mut options,
                &mut decisions,
                "accept",
                "Allow".into(),
                PermissionOptionKind::AllowOnce,
                json!({ "action": "accept", "content": {} }),
            );
            opt(
                &mut options,
                &mut decisions,
                "decline",
                "Deny".into(),
                PermissionOptionKind::RejectOnce,
                json!({ "action": "decline" }),
            );
            self.pending_approvals
                .insert(request_id.clone(), (rpc_id, decisions));
            step.events.push(AgentEvent::PermissionRequest {
                request_id,
                tool_call_id: None,
                title: truncate_label(&raw_title, 120),
                options,
                input_preview: json!({
                    "server": server,
                    "description": cap_output(
                        params["_meta"]["tool_description"].as_str().unwrap_or_default(),
                    )
                    .0,
                    // Any configured MCP server reaches this arm — cap the
                    // leaves like every other preview (the caps-at-event-
                    // construction invariant; claude's driver caps the same
                    // surface).
                    "params": crate::model::cap_preview(&params["_meta"]["tool_params"]),
                }),
                plan: None,
            });
            return;
        }

        let network_host = params["networkApprovalContext"]["host"].as_str();
        let title;
        let input_preview;
        match method {
            "item/fileChange/requestApproval" => {
                let locations = params["itemId"]
                    .as_str()
                    .and_then(|id| self.item_locations.get(id))
                    .cloned()
                    .unwrap_or_default();
                title = match locations.as_slice() {
                    [] => "apply file changes".to_string(),
                    [only] => format!("apply changes to {only}"),
                    many => format!("apply changes to {} files", many.len()),
                };
                input_preview = json!({
                    "files": locations,
                    "reason": params["reason"],
                    "grantRoot": params["grantRoot"],
                });
                opt(
                    &mut options,
                    &mut decisions,
                    "accept",
                    "Allow".into(),
                    PermissionOptionKind::AllowOnce,
                    json!({ "decision": "accept" }),
                );
                opt(
                    &mut options,
                    &mut decisions,
                    "acceptForSession",
                    "Allow for this session".into(),
                    PermissionOptionKind::AllowAlways,
                    json!({ "decision": "acceptForSession" }),
                );
            }
            _ if network_host.is_some() => {
                let host = network_host.unwrap_or_default().to_string();
                title = format!("network access to {host}");
                input_preview = json!({
                    "host": host,
                    "command": cap_output(params["command"].as_str().unwrap_or_default()).0,
                });
                opt(
                    &mut options,
                    &mut decisions,
                    "accept",
                    "Allow once".into(),
                    PermissionOptionKind::AllowOnce,
                    json!({ "decision": "accept" }),
                );
                opt(
                    &mut options,
                    &mut decisions,
                    "acceptForSession",
                    "Allow this host for this session".into(),
                    PermissionOptionKind::AllowAlways,
                    json!({ "decision": "acceptForSession" }),
                );
                // "Allow this host in the future" — the allow-action
                // amendment goes back verbatim (snake_case payload key).
                let amendment = params["proposedNetworkPolicyAmendments"]
                    .as_array()
                    .and_then(|arr| arr.iter().find(|a| a["action"] == "allow"))
                    .cloned();
                if let Some(amendment) = amendment {
                    opt(
                        &mut options,
                        &mut decisions,
                        "allowHostAlways",
                        "Always allow this host".into(),
                        PermissionOptionKind::AllowAlways,
                        json!({ "decision": {
                            "applyNetworkPolicyAmendment": {
                                "network_policy_amendment": amendment,
                            },
                        }}),
                    );
                }
            }
            _ => {
                // Cap both the title and the previewed command: an approval
                // request can carry a multi-megabyte inline script, and every
                // byte here is journaled, ring-held, and replayed to clients
                // (the caps-at-event-construction invariant).
                let raw_title = params["commandActions"][0]["command"]
                    .as_str()
                    .or(params["command"].as_str())
                    .unwrap_or("codex action");
                title = truncate_label(raw_title, 120);
                input_preview = json!({
                    "command": cap_output(params["command"].as_str().unwrap_or_default()).0,
                    "cwd": params["cwd"],
                    "reason": params["reason"],
                });
                opt(
                    &mut options,
                    &mut decisions,
                    "accept",
                    "Allow".into(),
                    PermissionOptionKind::AllowOnce,
                    json!({ "decision": "accept" }),
                );
                opt(
                    &mut options,
                    &mut decisions,
                    "acceptForSession",
                    "Allow for this session".into(),
                    PermissionOptionKind::AllowAlways,
                    json!({ "decision": "acceptForSession" }),
                );
                // "Don't ask again for commands that start with {prefix}" —
                // token array back verbatim; invalid when joining would
                // break onto a new line (the extension's validity rule).
                let tokens: Vec<String> = params["proposedExecpolicyAmendment"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if !tokens.is_empty() && !tokens.join(" ").contains(['\n', '\r']) {
                    let prefix = quote_tokens(&tokens);
                    opt(
                        &mut options,
                        &mut decisions,
                        "acceptWithAmendment",
                        format!("Always allow: {prefix} …"),
                        PermissionOptionKind::AllowAlways,
                        json!({ "decision": {
                            "acceptWithExecpolicyAmendment": {
                                "execpolicy_amendment": tokens,
                            },
                        }}),
                    );
                }
            }
        }
        opt(
            &mut options,
            &mut decisions,
            "decline",
            "Deny".into(),
            PermissionOptionKind::RejectOnce,
            json!({ "decision": "decline" }),
        );

        self.pending_approvals
            .insert(request_id.clone(), (rpc_id, decisions));
        step.events.push(AgentEvent::PermissionRequest {
            request_id,
            tool_call_id: params["itemId"].as_str().map(String::from),
            title,
            options,
            input_preview,
            plan: None,
        });
    }

    /// Inject a send into the running turn (type-through). A stale
    /// expectedTurnId retries once via the error-parse path in `on_response`.
    fn emit_steer(&mut self, input: Value, client_msg_id: String, step: &mut DriverStep) {
        let id = self.rpc_id();
        self.pending_rpcs.insert(
            id,
            PendingRpc::Steer {
                input: input.clone(),
                client_msg_id: client_msg_id.clone(),
                retried: false,
                order: id,
            },
        );
        step.outbound.push(json!({
            "id": id, "method": "turn/steer",
            "params": {
                "threadId": self.thread_id,
                "clientUserMessageId": client_msg_id,
                "input": input,
                "expectedTurnId": self.turn_id,
            },
        }));
    }

    /// Open a fresh turn. Sets turn_pending so a fast second send buffers
    /// instead of racing a second turn/start into the same window.
    fn emit_turn_start(&mut self, input: Value, client_msg_id: String, step: &mut DriverStep) {
        let id = self.rpc_id();
        let mut params = json!({
            "threadId": self.thread_id,
            "clientUserMessageId": client_msg_id,
            "input": input,
        });
        if let Some(model) = &self.pending_model {
            params["model"] = json!(model);
        }
        if let Some(effort) = &self.pending_effort {
            params["effort"] = json!(effort);
        }
        // Mode fields ride per-turn only when settings/update proved
        // unsupported (the extension's fallback path).
        if let Some(mode_fields) = &self.mode_per_turn {
            if let Some(fields) = mode_fields.as_object() {
                for (k, v) in fields {
                    params[k] = v.clone();
                }
            }
        }
        self.pending_rpcs.insert(id, PendingRpc::TurnStart);
        self.turn_pending = true;
        step.outbound.push(json!({
            "id": id, "method": "turn/start", "params": params,
        }));
    }

    /// A Steer click may arrive after `turn/start` but before `turn/started`.
    /// Promote only explicitly marked entries once the id is known; ordinary
    /// follow-ups stay queued for later turns.
    fn flush_requested_steers(&mut self, step: &mut DriverStep) {
        let mut queued = VecDeque::new();
        for send in std::mem::take(&mut self.queued_sends) {
            if send.steer_when_active {
                self.emit_steer(send.input, send.client_msg_id, step);
            } else {
                queued.push_back(send);
            }
        }
        self.queued_sends = queued;
    }

    /// A queued send whose steer/start window collapsed becomes a fresh
    /// turn's input — the same standing as a fresh send, so its delivery
    /// resolves `sent` here (a fresh send never echoes queued at all; a
    /// later turn/start failure surfaces as an Error notice for both).
    fn redrive_as_fresh_turn(
        &mut self,
        input: Value,
        client_msg_id: String,
        step: &mut DriverStep,
    ) {
        debug_assert!(
            !self.turn_active && !self.turn_pending,
            "fresh-turn redrive must be serialized behind the current turn"
        );
        // A queued message becomes a rewindable boundary only NOW, when it
        // opens its own turn. An explicitly steered message never gets one.
        let preceding = self.last_checkpoint.replace(client_msg_id.clone());
        step.events.push(AgentEvent::Checkpoint {
            user_message_id: client_msg_id.clone(),
            preceding_uuid: preceding,
        });
        step.events.push(AgentEvent::UserMessageUpdate {
            id: client_msg_id.clone(),
            state: UserMessageState::Sent,
        });
        self.emit_turn_start(input, client_msg_id, step);
    }

    /// Route the decline-feedback reason into the conversation whatever the
    /// turn state: steer the running turn, buffer through an unidentified
    /// start window, or open a fresh turn. (Send does its own dispatch inline
    /// so it can thread the delivery-tracking `client_msg_id`; this mints its
    /// own untracked id since the feedback text is not a queued user bubble.)
    fn dispatch_input(&mut self, input: Value, step: &mut DriverStep) {
        let client_msg_id = crate::model::fresh_uuid();
        if self.turn_active && !self.turn_id.is_empty() {
            // Type-through: inject into the RUNNING turn (steer).
            self.emit_steer(input, client_msg_id, step);
        } else if self.turn_pending {
            // Decline feedback is explicit type-through (not a queued user
            // bubble). Remember that intent until turn/started supplies the id.
            self.queued_sends.push_back(QueuedSend {
                input,
                client_msg_id,
                steer_when_active: true,
            });
        } else {
            self.emit_turn_start(input, client_msg_id, step);
        }
    }

    fn send_blocks(
        &mut self,
        blocks: Vec<ContentBlock>,
        display_text: Option<String>,
        step: &mut DriverStep,
    ) {
        let text = crate::model::blocks_text(&blocks);
        // Images ride the input array as data URLs (the extension's non-local
        // path form; local paths need a shared fs).
        let mut input: Vec<Value> = Vec::new();
        if !text.is_empty() {
            input.push(json!({ "type": "text", "text": text }));
        }
        let mut attachments = 0u32;
        for b in &blocks {
            if let ContentBlock::Image { media_type, data } = b {
                attachments += 1;
                input.push(json!({
                    "type": "image",
                    "url": format!("data:{media_type};base64,{data}"),
                }));
            }
        }
        let input = json!(input);
        let client_msg_id = crate::model::fresh_uuid();
        // Queue and steer are separate Codex-native actions. A send during a
        // live/pending run is held for the NEXT turn; the UI's Steer button
        // explicitly promotes it via `turn/steer`.
        let queued = (self.turn_active && !self.turn_id.is_empty()) || self.turn_pending;
        let priming = display_text.is_some();
        step.events.push(AgentEvent::UserMessage {
            text: display_text.unwrap_or_else(|| text.clone()),
            attachments: if priming { 0 } else { attachments },
            id: Some(client_msg_id.clone()),
            queued,
        });
        if queued {
            self.queued_sends.push_back(QueuedSend {
                input,
                client_msg_id,
                steer_when_active: false,
            });
        } else {
            // Only a turn-OPENING send anchors a checkpoint: rewind rolls
            // back whole turns (thread/rollback numTurns), so a steered
            // message — joining a running turn — is not a boundary. Emitted
            // right after UserMessage (journal truncation relies on adjacency).
            let preceding = self.last_checkpoint.replace(client_msg_id.clone());
            step.events.push(AgentEvent::Checkpoint {
                user_message_id: client_msg_id.clone(),
                preceding_uuid: preceding,
            });
            self.emit_turn_start(input, client_msg_id, step);
        }
    }

    fn on_command(&mut self, cmd: AgentCommand) -> DriverStep {
        let mut step = DriverStep::default();
        match cmd {
            AgentCommand::Send { blocks } => self.send_blocks(blocks, None, &mut step),
            AgentCommand::PrimeFork {
                blocks,
                display_text,
            } => self.send_blocks(blocks, Some(display_text), &mut step),
            AgentCommand::Permission {
                request_id,
                option_id,
                feedback,
                ..
            } => {
                let Some((rpc_id, decisions)) = self.pending_approvals.remove(&request_id) else {
                    // The ask predates this driver process (respawn, toggle,
                    // resume) or the server already settled it: the reply
                    // route is gone. Resolve — journaled — plus a notice, so
                    // the click never silently vanishes into a stuck card.
                    step.events.push(AgentEvent::PermissionResolved {
                        request_id,
                        option_id: "expired".into(),
                    });
                    step.events.push(AgentEvent::Notice {
                        text: "that permission prompt is no longer active \
                               (the agent restarted since it asked)"
                            .into(),
                    });
                    return step;
                };
                // Unknown decision strings silently decline server-side, so
                // only prebuilt payloads are sent; a miss declines honestly.
                let result = decisions
                    .get(&option_id)
                    .cloned()
                    .unwrap_or_else(|| json!({ "decision": "decline" }));
                let declined = result["decision"] == json!("decline");
                step.outbound
                    .push(json!({ "id": rpc_id, "result": result }));
                step.events.push(AgentEvent::PermissionResolved {
                    request_id,
                    option_id,
                });
                // Deny feedback: the app-server decline carries no message
                // field, so the reason follows the decline as user input into
                // the (still running) turn — the protocol's equivalent of
                // claude's feedback-denial.
                if declined {
                    if let Some(fb) = feedback
                        .map(|f| f.trim().to_string())
                        .filter(|f| !f.is_empty())
                    {
                        step.events.push(AgentEvent::UserMessage {
                            text: fb.clone(),
                            attachments: 0,
                            id: None,
                            queued: false,
                        });
                        self.dispatch_input(json!([{ "type": "text", "text": fb }]), &mut step);
                    }
                }
            }
            AgentCommand::Interrupt => {
                // Arm the watchdog: "no active turn to interrupt" is a benign
                // no-op on the app-server, and a wedged turn may never emit
                // turn/completed, so `tick` synthesizes the abort if no real
                // turn end lands within the grace — the user's escape from a
                // stuck-running state. A real turn end disarms it first.
                self.interrupt_grace = Some(INTERRUPT_GRACE_TICKS);
                let id = self.rpc_id();
                self.pending_rpcs.insert(id, PendingRpc::Interrupt);
                step.outbound.push(json!({
                    "id": id, "method": "turn/interrupt",
                    "params": { "threadId": self.thread_id, "turnId": self.turn_id },
                }));
            }
            AgentCommand::SetModel { model_id } => {
                self.pending_model = Some(model_id.clone());
                self.model = Some(model_id);
                step.events.push(self.init_event());
            }
            AgentCommand::SetEffort { effort_id } => {
                if self.pending_effort.as_deref() == Some(&effort_id) {
                    self.emit_effort_state(Some(effort_id), &mut step);
                } else {
                    let previous = self.pending_effort.replace(effort_id.clone());
                    self.refresh_per_turn_mode_effort();
                    if self.settings_update_unsupported {
                        // Older app-server: the value is still real and rides
                        // the next turn/start, so journal it immediately.
                        self.emit_effort_state(Some(effort_id), &mut step);
                    } else {
                        let id = self.rpc_id();
                        let fields = self.effort_wire_fields(&effort_id);
                        let mut params = json!({ "threadId": self.thread_id });
                        if let Some(obj) = fields.as_object() {
                            for (key, value) in obj {
                                params[key] = value.clone();
                            }
                        }
                        self.pending_rpcs.insert(
                            id,
                            PendingRpc::EffortUpdate {
                                effort_id,
                                previous,
                            },
                        );
                        step.outbound.push(json!({
                            "id": id,
                            "method": "thread/settings/update",
                            "params": params,
                        }));
                    }
                }
            }
            AgentCommand::SetMode { mode_id } => {
                let fields = mode_wire_fields(
                    &mode_id,
                    self.pending_model.as_deref().or(self.model.as_deref()),
                    self.pending_effort.as_deref(),
                );
                if self.settings_update_unsupported {
                    self.mode_per_turn = Some(fields);
                    self.apply_mode(mode_id, &mut step);
                } else {
                    // Probe thread/settings/update (applies mid-thread); the
                    // response handler falls back to per-turn on -32601.
                    let id = self.rpc_id();
                    let mut params = json!({ "threadId": self.thread_id });
                    if let Some(obj) = fields.as_object() {
                        for (k, v) in obj {
                            params[k] = v.clone();
                        }
                    }
                    self.pending_rpcs.insert(
                        id,
                        PendingRpc::SettingsUpdate {
                            mode_id,
                            per_turn: fields,
                        },
                    );
                    step.outbound.push(json!({
                        "id": id, "method": "thread/settings/update", "params": params,
                    }));
                }
            }
            AgentCommand::GetUsage => {
                let id = self.rpc_id();
                self.pending_rpcs
                    .insert(id, PendingRpc::AccountRead { report: true });
                step.outbound.push(json!({
                    "id": id, "method": "account/read",
                    "params": { "refreshToken": false },
                }));
            }
            AgentCommand::Answer {
                request_id,
                answers,
            } => {
                let Some(question) = self.pending_questions.remove(&request_id) else {
                    // Same stale-ask contract as Permission above: resolve +
                    // notice instead of silently eating the user's answer.
                    step.events.push(AgentEvent::QuestionResolved {
                        request_id,
                        answers: Default::default(),
                    });
                    step.events.push(AgentEvent::Notice {
                        text: "that question is no longer active (the agent \
                               restarted since it asked) — ask again if needed"
                            .into(),
                    });
                    return step;
                };
                let mut map = serde_json::Map::new();
                for (qid, labels) in &answers {
                    map.insert(qid.clone(), json!({ "answers": labels }));
                }
                step.outbound
                    .push(json!({ "id": question.rpc_id, "result": { "answers": map } }));
                // The chosen labels ride the resolution so the transcript
                // (and every replay) shows question + answer, not a vanish.
                step.events.push(AgentEvent::QuestionResolved {
                    request_id,
                    answers,
                });
            }
            AgentCommand::Compact => {
                // Live-verified: the ack is an empty result; the compaction
                // then runs as its own turn whose contextCompaction item maps
                // to the "context compacted" notice (no thread/compacted
                // notification on the pinned version).
                let id = self.rpc_id();
                self.pending_rpcs.insert(id, PendingRpc::Compact);
                step.outbound.push(json!({
                    "id": id, "method": "thread/compact/start",
                    "params": { "threadId": self.thread_id },
                }));
            }
            // Pull back a still-queued message. A send still in the next-run
            // FIFO is genuinely pulled back (it never reached codex). A send
            // whose steer RPC is IN FLIGHT is mid-delivery — it's on the wire
            // and its `sent`/re-drive resolution is still coming, so a
            // tombstone now would vanish a bubble the agent may consume:
            // that one gets a Notice instead. Anything else already resolved:
            // emit `Cancelled` tombstone-style (claude parity) — it dismisses
            // a DROPPED "not delivered" bubble on live and replay alike, and
            // the reducer no-ops for an already-`sent` id (the message is
            // visibly in the transcript, which is its own answer).
            AgentCommand::CancelQueued { id } => {
                let steer_in_flight = self.pending_rpcs.values().any(
                    |p| matches!(p, PendingRpc::Steer { client_msg_id, .. } if *client_msg_id == id),
                );
                if steer_in_flight {
                    step.events.push(AgentEvent::Notice {
                        text: "that message is already on its way to the agent — \
                               too late to cancel it"
                            .into(),
                    });
                } else {
                    self.queued_sends.retain(|send| send.client_msg_id != id);
                    step.events.push(AgentEvent::UserMessageUpdate {
                        id,
                        state: UserMessageState::Cancelled,
                    });
                }
            }
            // Promote one native queued follow-up. With a known active turn it
            // steers now; during the start window it remembers the click until
            // turn/started supplies expectedTurnId; if the run ended in the
            // meantime, the selected message simply opens the next turn.
            AgentCommand::SteerQueued { id } => {
                let steer_in_flight = self.pending_rpcs.values().any(
                    |p| matches!(p, PendingRpc::Steer { client_msg_id, .. } if *client_msg_id == id),
                );
                if steer_in_flight {
                    step.events.push(AgentEvent::Notice {
                        text: "that message is already being steered".into(),
                    });
                } else if let Some(pos) = self
                    .queued_sends
                    .iter()
                    .position(|send| send.client_msg_id == id)
                {
                    if self.turn_active && !self.turn_id.is_empty() {
                        let send = self.queued_sends.remove(pos).expect("position exists");
                        self.emit_steer(send.input, send.client_msg_id, &mut step);
                    } else if self.turn_pending {
                        self.queued_sends[pos].steer_when_active = true;
                    } else {
                        let send = self.queued_sends.remove(pos).expect("position exists");
                        self.redrive_as_fresh_turn(send.input, send.client_msg_id, &mut step);
                    }
                } else {
                    step.events.push(AgentEvent::Notice {
                        text: "that message is no longer queued".into(),
                    });
                }
            }
            // No codex equivalents on this surface. Rewind is not a driver
            // command here: the conversation rewind is the server's respawn
            // recipe (thread/resume + thread/rollback at handshake), and
            // codex has no rewind_files-style file restore to answer with.
            AgentCommand::SetThinking { .. }
            | AgentCommand::SetUltracode { .. }
            | AgentCommand::BackgroundTool { .. }
            | AgentCommand::StopTask { .. }
            | AgentCommand::Rewind { .. }
            | AgentCommand::GetMcp
            | AgentCommand::SetMcpEnabled { .. }
            | AgentCommand::ReconnectMcp { .. } => {}
        }
        step
    }

    /// Auto-skip questions whose `autoResolutionMs` deadline passed: empty
    /// answers back to the server (the official client's behavior), the card
    /// withdrawn, and a visible note in the transcript. Split from [`tick`]
    /// so tests can drive the clock.
    fn expire_questions(&mut self, now: std::time::Instant) -> DriverStep {
        let mut step = DriverStep::default();
        let expired: Vec<String> = self
            .pending_questions
            .iter()
            .filter(|(_, q)| q.deadline.is_some_and(|d| d <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for request_id in expired {
            let Some(question) = self.pending_questions.remove(&request_id) else {
                continue;
            };
            step.outbound
                .push(json!({ "id": question.rpc_id, "result": { "answers": {} } }));
            // Timed-out question resolves with empty answers (the official
            // client's empty-skip) so the transcript/replay shows it withdrawn.
            step.events.push(AgentEvent::QuestionResolved {
                request_id,
                answers: Default::default(),
            });
            step.events.push(AgentEvent::Notice {
                text: "question timed out unanswered — skipped (codex auto-resolution)".into(),
            });
        }
        step
    }

    /// The interrupt watchdog (see `INTERRUPT_GRACE_TICKS`). When the grace
    /// armed on `Interrupt` expires with a turn still open — the app-server
    /// never emitted `turn/completed`, so the session would stay "running"
    /// forever — synthesize the abort it owed: emit `TurnAborted{interrupted}`
    /// and DROP the queue. Dropping (not re-driving) is deliberate here,
    /// unlike the live-abort paths: the watchdog firing means codex stopped
    /// answering, so a steer will never get its ack and a re-driven
    /// `turn/start` would strand "queued" against a wedged process —
    /// `dropped` (dismissible) is the honest state. A real turn end disarms
    /// the grace first, so a live turn is never double-aborted; idle-guarded,
    /// so interrupting nothing stays a no-op.
    fn interrupt_watchdog(&mut self) -> DriverStep {
        let mut step = DriverStep::default();
        let Some(remaining) = self.interrupt_grace else {
            return step;
        };
        if remaining > 1 {
            self.interrupt_grace = Some(remaining - 1);
            return step;
        }
        self.interrupt_grace = None;
        if !self.turn_active {
            // Interrupt while idle is a benign no-op — nothing to abort.
            return step;
        }
        self.turn_active = false;
        // The synthesized abort closes subagent rows exactly like a real one.
        self.fail_dangling_collab_agents(&mut step);
        step.events.push(AgentEvent::TurnAborted {
            turn_id: self.turn_id.clone(),
            reason: "interrupted".into(),
            interrupted: true,
        });
        step.events.extend(self.drain_queued_sends());
        self.reset_turn_state();
        step
    }

    /// The idle-flush (see `IDLE_FLUSH_GRACE_TICKS`). codex normally resolves
    /// every queued message at a well-defined point (a steer's RPC answer, a
    /// turn-end promotion, or the teardown drain), so this is the defensive
    /// rescue for the one seam where it can't: a turn end that fires while a
    /// `turn/start` was still in flight (`turn_pending`) leaves waiting input
    /// stranded with no turn to promote it from. When the driver is idle with
    /// a stranded queue past the grace, open the oldest as a fresh turn; the
    /// rest remain queued. A queued entry was never sent, so we DELIVER it
    /// rather than declare it sent unseen.
    fn idle_flush(&mut self) -> DriverStep {
        let mut step = DriverStep::default();
        let queue_empty = self.queued_sends.is_empty() && self.deferred_steer_redrives.is_empty();
        if self.turn_active || self.turn_pending || self.steer_in_flight() || queue_empty {
            self.idle_flush_grace = None;
            return step;
        }
        match self.idle_flush_grace {
            None => {
                self.idle_flush_grace = Some(IDLE_FLUSH_GRACE_TICKS);
            }
            Some(remaining) if remaining > 1 => {
                self.idle_flush_grace = Some(remaining - 1);
            }
            Some(_) => {
                self.idle_flush_grace = None;
                self.start_next_queued(&mut step);
            }
        }
        step
    }
}

/// Harness adapter: the inherent methods above ARE the state machine; these
/// forward the harness's generic calls to them (inherent methods win in
/// `self.x()` resolution, so there is no recursion).
impl Mapper for CodexMapper {
    fn init_event(&self) -> AgentEvent {
        self.init_event()
    }
    fn on_frame(&mut self, frame: &Value) -> DriverStep {
        self.on_frame(frame)
    }
    fn on_command(&mut self, cmd: AgentCommand) -> DriverStep {
        self.on_command(cmd)
    }
    fn flush(&mut self) -> Option<AgentEvent> {
        self.flush()
    }
    fn drain_pending(&mut self) -> Vec<AgentEvent> {
        self.drain_pending()
    }
    fn tick(&mut self) -> DriverStep {
        let mut step = self.expire_questions(std::time::Instant::now());
        let watchdog = self.interrupt_watchdog();
        step.events.extend(watchdog.events);
        step.outbound.extend(watchdog.outbound);
        let flush = self.idle_flush();
        step.events.extend(flush.events);
        step.outbound.extend(flush.outbound);
        step
    }
}

/// `commandActions[0].command` is the bare command; the raw `command` field
/// carries the `/bin/zsh -lc '…'` wrapper.
fn command_title(item: &Value) -> String {
    let raw = item["commandActions"][0]["command"]
        .as_str()
        .or(item["command"].as_str())
        .unwrap_or("command");
    truncate_label(raw, 120)
}

/// The extension's exact steer-retry extraction: the live turn id sits in
/// the error text as ``expected active turn id `x` but found `y` ``.
fn parse_expected_turn_id(msg: &str) -> Option<String> {
    let marker = "but found `";
    let start = msg.find(marker)? + marker.len();
    let end = msg[start..].find('`')? + start;
    Some(msg[start..end].to_string()).filter(|s| !s.is_empty())
}

/// The extension's feature-detect predicate for optional RPCs: JSON-RPC
/// -32601, or a message naming the method as unknown.
fn is_method_not_found(err: &Value, method: &str) -> bool {
    if err["code"].as_i64() == Some(-32601) {
        return true;
    }
    // Capability-gated builds answer -32600 "… requires experimentalApi
    // capability"; treat it as the same per-turn fallback signal.
    if err["message"]
        .as_str()
        .is_some_and(|m| m.contains("requires experimentalApi"))
    {
        return true;
    }
    let msg = err["message"].as_str().unwrap_or_default().to_lowercase();
    msg.contains("method not found")
        || ((msg.contains("unknown method") || msg.contains("unknown variant"))
            && msg.contains(&method.to_lowercase()))
}

/// Composer-mode → thread settings. Every arm is deliberately complete:
/// switching out of auto-review must restore the human reviewer, switching
/// out of full access must restore approvals, and switching out of plan must
/// clear collaboration mode. Omitting those resets leaves invisible sticky
/// state behind while the header claims a different mode.
fn mode_wire_fields(mode_id: &str, model: Option<&str>, effort: Option<&str>) -> Value {
    match mode_id {
        "read-only" => json!({
            "permissions": ":read-only",
            "approvalPolicy": "on-request",
            "approvalsReviewer": "user",
            "collaborationMode": null,
        }),
        "auto-review" => json!({
            "permissions": ":workspace",
            "approvalPolicy": "on-request",
            "approvalsReviewer": "auto_review",
            "collaborationMode": null,
        }),
        "full-access" => json!({
            "permissions": ":danger-full-access",
            "approvalPolicy": "never",
            "approvalsReviewer": "user",
            "collaborationMode": null,
        }),
        // Plan mode is a collaboration mode, not a permission profile;
        // settings are snake_case inside (mined).
        "plan" => json!({
            "permissions": ":workspace",
            "approvalPolicy": "on-request",
            "approvalsReviewer": "user",
            "collaborationMode": {
                "mode": "plan",
                "settings": {
                    "model": model.unwrap_or("gpt-5.5"),
                    "reasoning_effort": effort.unwrap_or("medium"),
                    "developer_instructions": null,
                },
            },
        }),
        _ => json!({
            "permissions": ":workspace",
            "approvalPolicy": "on-request",
            "approvalsReviewer": "user",
            "collaborationMode": null,
        }),
    }
}

/// The tool name from a codex MCP tool-call elicitation message, parsed
/// against the EXACT pinned shape (Pass 19): `Allow the {server} MCP server
/// to run tool "{name}"?` — prefix/suffix anchored, and the name must be
/// quote-free `[A-Za-z0-9_-]+` (every real MCP tool name; an injected name
/// carrying quotes or spaces fails closed to the user-facing prompt).
fn elicitation_tool_name<'a>(message: &'a str, server: &str) -> Option<&'a str> {
    let prefix = format!("Allow the {server} MCP server to run tool \"");
    let name = message.strip_prefix(&prefix)?.strip_suffix("\"?")?;
    (!name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    .then_some(name)
}

/// Stable, bounded presentation of an unstable auto-review action payload.
/// Returns the normalized tool kind, a one-line action label, and any files
/// the existing open-in-pane affordance can safely expose.
fn auto_review_action(action: &Value) -> (ToolKind, String, Vec<String>) {
    match action["type"].as_str().unwrap_or_default() {
        "command" => (
            ToolKind::Execute,
            truncate_label(action["command"].as_str().unwrap_or("command"), 120),
            Vec::new(),
        ),
        "execve" => {
            let mut command = action["program"].as_str().unwrap_or("command").to_string();
            if let Some(argv) = action["argv"].as_array() {
                for arg in argv.iter().filter_map(Value::as_str).take(12) {
                    command.push(' ');
                    command.push_str(arg);
                }
            }
            (ToolKind::Execute, truncate_label(&command, 120), Vec::new())
        }
        "applyPatch" => {
            let locations: Vec<String> = action["files"]
                .as_array()
                .map(|files| {
                    files
                        .iter()
                        .filter_map(Value::as_str)
                        .take(32)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            let label = match locations.as_slice() {
                [] => "apply patch".to_string(),
                [path] => format!("edit {path}"),
                many => format!("edit {} files", many.len()),
            };
            (ToolKind::Edit, truncate_label(&label, 120), locations)
        }
        "networkAccess" => {
            let host = action["host"].as_str().unwrap_or("network access");
            (
                ToolKind::Fetch,
                truncate_label(&format!("network access to {host}"), 120),
                Vec::new(),
            )
        }
        "mcpToolCall" => {
            let server = action["connectorName"]
                .as_str()
                .or(action["server"].as_str())
                .unwrap_or("MCP");
            let tool = action["toolTitle"]
                .as_str()
                .or(action["toolName"].as_str())
                .unwrap_or("tool");
            (
                ToolKind::Other,
                truncate_label(&format!("{server} · {tool}"), 120),
                Vec::new(),
            )
        }
        "requestPermissions" => (
            ToolKind::Other,
            truncate_label(
                action["reason"]
                    .as_str()
                    .unwrap_or("additional permissions"),
                120,
            ),
            Vec::new(),
        ),
        other => (
            ToolKind::Other,
            truncate_label(
                if other.is_empty() {
                    "approval request"
                } else {
                    other
                },
                120,
            ),
            Vec::new(),
        ),
    }
}

/// Shell-ish quoting for the execpolicy-amendment prefix label.
fn quote_tokens(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|t| {
            if t.chars()
                .all(|c| c.is_alphanumeric() || "-_./:=".contains(c))
            {
                t.clone()
            } else {
                format!("'{}'", t.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper() -> CodexMapper {
        CodexMapper::new("thr-1".into(), Vec::new(), None, None, None, None, 3)
    }

    fn active_turn(m: &mut CodexMapper) {
        m.on_frame(&json!({
            "method": "turn/started",
            "params": { "turn": { "id": "turn-A" } },
        }));
    }

    #[test]
    fn mid_turn_send_queues_until_explicit_steer() {
        let mut m = mapper();
        active_turn(&mut m);
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "also do X".into(),
            }],
        });
        // Enter queues for the next turn; it does not implicitly steer.
        let msg_id = match &step.events[0] {
            AgentEvent::UserMessage { id, queued, .. } => {
                assert!(queued, "a mid-turn send echoes queued");
                id.clone().expect("queued send carries a delivery id")
            }
            other => panic!("expected UserMessage, got {other:?}"),
        };
        assert!(step.outbound.is_empty(), "queue sends nothing to codex yet");
        assert_eq!(m.queued_sends.len(), 1);

        // The separate Steer action promotes exactly this entry.
        let step = m.on_command(AgentCommand::SteerQueued { id: msg_id.clone() });
        assert_eq!(step.outbound[0]["method"], "turn/steer");
        assert_eq!(step.outbound[0]["params"]["expectedTurnId"], "turn-A");
        assert_eq!(
            step.outbound[0]["params"]["clientUserMessageId"],
            json!(msg_id)
        );
        assert!(m.queued_sends.is_empty(), "steered entry leaves the FIFO");

        // Steer accepted → the message resolves sent, exactly once.
        let rpc_id = step.outbound[0]["id"].as_u64().unwrap();
        let step = m.on_frame(&json!({ "id": rpc_id, "result": {} }));
        assert_eq!(
            step.events,
            vec![AgentEvent::UserMessageUpdate {
                id: msg_id,
                state: UserMessageState::Sent,
            }]
        );
    }

    #[test]
    fn fork_prime_sends_full_context_but_journals_only_compact_row() {
        let mut m = mapper();
        let step = m.on_command(AgentCommand::PrimeFork {
            blocks: vec![ContentBlock::Text {
                text: "full portable transcript context".into(),
            }],
            display_text: "Continue from this fork point.".into(),
        });
        assert!(matches!(
            &step.events[0],
            AgentEvent::UserMessage { text, .. }
                if text == "Continue from this fork point."
        ));
        assert_eq!(
            step.outbound[0]["params"]["input"][0]["text"],
            "full portable transcript context"
        );
    }

    #[test]
    fn steer_clicked_during_start_window_waits_for_turn_id() {
        let mut m = mapper();
        // First send: no active turn → turn/start, and the start window opens.
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "one".into() }],
        });
        assert_eq!(step.outbound[0]["method"], "turn/start");
        assert!(m.turn_pending);
        // Second send arrives BEFORE turn/started: it queues without racing a
        // second turn/start into the app-server.
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "two".into() }],
        });
        assert!(
            step.outbound.is_empty(),
            "second send queues, no second turn/start: {:?}",
            step.outbound
        );
        let queued_id = match &step.events[0] {
            AgentEvent::UserMessage { id, queued, .. } => {
                assert!(queued, "the follow-up echoes queued");
                id.clone().unwrap()
            }
            other => panic!("expected UserMessage, got {other:?}"),
        };
        // A Steer click in this unidentified window is remembered, not lost.
        let step = m.on_command(AgentCommand::SteerQueued {
            id: queued_id.clone(),
        });
        assert!(step.outbound.is_empty(), "no expectedTurnId exists yet");

        // turn/started lands: only the explicitly promoted send steers into it.
        let step = m.on_frame(&json!({
            "method": "turn/started",
            "params": { "turn": { "id": "turn-Z" } },
        }));
        assert!(!m.turn_pending);
        let steer = step
            .outbound
            .iter()
            .find(|f| f["method"] == "turn/steer")
            .expect("explicitly promoted send steered");
        assert_eq!(steer["params"]["expectedTurnId"], "turn-Z");
        // …and the flushed steer's success resolves the queued send.
        let rpc_id = steer["id"].as_u64().unwrap();
        let step = m.on_frame(&json!({ "id": rpc_id, "result": {} }));
        assert_eq!(
            step.events,
            vec![AgentEvent::UserMessageUpdate {
                id: queued_id,
                state: UserMessageState::Sent,
            }]
        );
    }

    #[test]
    fn queued_followups_open_one_fresh_turn_at_a_time() {
        let mut m = mapper();
        active_turn(&mut m);
        let queued_id = |step: &DriverStep| match &step.events[0] {
            AgentEvent::UserMessage {
                id: Some(id),
                queued: true,
                ..
            } => id.clone(),
            other => panic!("expected queued UserMessage, got {other:?}"),
        };
        let second = queued_id(&m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "second".into(),
            }],
        }));
        let third = queued_id(&m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "third".into(),
            }],
        }));
        assert_eq!(m.queued_sends.len(), 2);

        // Finishing the current turn promotes only the oldest queue entry.
        let step = m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "completed" } },
        }));
        assert!(step.events.iter().any(|e| matches!(
            e,
            AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if id == &second
        )));
        assert!(step.outbound.iter().any(|o| o["method"] == "turn/start"));
        assert_eq!(m.queued_sends.len(), 1);
        assert_eq!(m.queued_sends[0].client_msg_id, third);

        // The remaining entry does NOT auto-steer into the new turn.
        let step = m.on_frame(&json!({
            "method": "turn/started",
            "params": { "turn": { "id": "turn-B" } },
        }));
        assert!(!step.outbound.iter().any(|o| o["method"] == "turn/steer"));
    }

    #[test]
    fn turn_end_classifies_user_interrupt_vs_failure() {
        // status "interrupted" is codex's structural user-stop signal.
        let mut m = mapper();
        active_turn(&mut m);
        let step = m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "interrupted" } },
        }));
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::TurnAborted {
                    interrupted: true,
                    reason,
                    ..
                } if reason == "interrupted"
            )),
            "user interrupt carries the structural flag: {:?}",
            step.events
        );

        // turn/failed is a genuine failure — never flagged interrupted.
        let mut m = mapper();
        active_turn(&mut m);
        let step = m.on_frame(&json!({
            "method": "turn/failed",
            "params": { "error": { "message": "boom" } },
        }));
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::TurnAborted {
                    interrupted: false,
                    reason,
                    ..
                } if reason == "boom"
            )),
            "failures keep interrupted false: {:?}",
            step.events
        );
    }

    #[test]
    fn file_change_patch_updated_retargets_locations() {
        let mut m = mapper();
        active_turn(&mut m);
        // item/started names one file…
        m.on_frame(&json!({
            "method": "item/started",
            "params": { "item": { "id": "fc-1", "type": "fileChange",
                "changes": [{ "path": "a.rs", "diff": "…", "kind": { "type": "update" } }] } },
        }));
        // …then a live patchUpdated wholesale-replaces it with two files.
        let step = m.on_frame(&json!({
            "method": "item/fileChange/patchUpdated",
            "params": { "itemId": "fc-1", "changes": [
                { "path": "a.rs", "diff": "…", "kind": { "type": "update" } },
                { "path": "b.rs", "diff": "…", "kind": { "type": "add" } },
            ]},
        }));
        match &step.events[0] {
            AgentEvent::ToolCall {
                id,
                locations,
                title,
                ..
            } => {
                assert_eq!(id, "fc-1");
                assert_eq!(locations.len(), 2);
                assert_eq!(title, "2 files changed");
            }
            other => panic!("expected re-upserted ToolCall, got {other:?}"),
        }
        // An approval arriving after the patch now names both files.
        let step = m.on_frame(&json!({
            "id": 80,
            "method": "item/fileChange/requestApproval",
            "params": { "itemId": "fc-1" },
        }));
        match &step.events[0] {
            AgentEvent::PermissionRequest { title, .. } => {
                assert_eq!(title, "apply changes to 2 files");
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[test]
    fn steer_reroutes_to_turn_start_when_turn_ended() {
        let mut m = mapper();
        active_turn(&mut m);
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "go".into() }],
        });
        let queued_id = match &step.events[0] {
            AgentEvent::UserMessage { id: Some(id), .. } => id.clone(),
            other => panic!("expected queued UserMessage, got {other:?}"),
        };
        let step = m.on_command(AgentCommand::SteerQueued { id: queued_id });
        let id = step.outbound[0]["id"].as_u64().unwrap();
        // The turn ended between our send and the steer response.
        m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "completed" } },
        }));
        // A steer failure that is NOT a turn-id mismatch must re-drive the
        // saved input as a fresh turn instead of dropping the user message.
        let step = m.on_frame(&json!({
            "id": id,
            "error": { "message": "no active turn" },
        }));
        assert_eq!(
            step.outbound[0]["method"], "turn/start",
            "saved input re-driven as a new turn: {:?}",
            step.outbound
        );
    }

    #[test]
    fn turn_end_waits_for_inflight_steer_before_fifo_promotion() {
        let mut m = mapper();
        active_turn(&mut m);
        let queued_id = |step: &DriverStep| match &step.events[0] {
            AgentEvent::UserMessage {
                id: Some(id),
                queued: true,
                ..
            } => id.clone(),
            other => panic!("expected queued UserMessage, got {other:?}"),
        };
        let next_id = queued_id(&m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "ordinary next turn".into(),
            }],
        }));
        let steered_id = queued_id(&m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "promote this one".into(),
            }],
        }));
        let step = m.on_command(AgentCommand::SteerQueued {
            id: steered_id.clone(),
        });
        let steer_rpc = step.outbound[0]["id"].as_u64().unwrap();

        // The turn ends before the steer answer. The ordinary FIFO must wait:
        // starting it now would race the steer's possible fresh-turn redrive.
        let step = m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "completed" } },
        }));
        assert!(
            !step.outbound.iter().any(|o| o["method"] == "turn/start"),
            "an unresolved steer gates FIFO promotion: {:?}",
            step.outbound
        );
        assert_eq!(m.queued_sends[0].client_msg_id, next_id);

        // A non-turn-id steer failure re-drives only the explicitly promoted
        // message. The ordinary entry remains queued, so there is exactly one
        // turn/start in flight and user intent stays ordered.
        let step = m.on_frame(&json!({
            "id": steer_rpc,
            "error": { "message": "no active turn" },
        }));
        let starts: Vec<_> = step
            .outbound
            .iter()
            .filter(|o| o["method"] == "turn/start")
            .collect();
        assert_eq!(starts.len(), 1, "only the failed steer re-drives");
        assert_eq!(starts[0]["params"]["clientUserMessageId"], steered_id);
        assert!(m.turn_pending);
        assert_eq!(m.queued_sends.len(), 1);
        assert_eq!(m.queued_sends[0].client_msg_id, next_id);
    }

    #[test]
    fn settled_steer_releases_waiting_fifo_after_turn_end() {
        let mut m = mapper();
        active_turn(&mut m);
        let queued_id = |step: &DriverStep| match &step.events[0] {
            AgentEvent::UserMessage {
                id: Some(id),
                queued: true,
                ..
            } => id.clone(),
            other => panic!("expected queued UserMessage, got {other:?}"),
        };
        let next_id = queued_id(&m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "next".into(),
            }],
        }));
        let steered_id = queued_id(&m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "steered".into(),
            }],
        }));
        let step = m.on_command(AgentCommand::SteerQueued {
            id: steered_id.clone(),
        });
        let steer_rpc = step.outbound[0]["id"].as_u64().unwrap();
        let step = m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "completed" } },
        }));
        assert!(!step.outbound.iter().any(|o| o["method"] == "turn/start"));

        // A successful late ack means the promoted message landed in the old
        // turn. It releases exactly the oldest ordinary follow-up now.
        let step = m.on_frame(&json!({ "id": steer_rpc, "result": {} }));
        assert!(step.events.iter().any(|event| matches!(
            event,
            AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent }
                if id == &steered_id
        )));
        let starts: Vec<_> = step
            .outbound
            .iter()
            .filter(|o| o["method"] == "turn/start")
            .collect();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0]["params"]["clientUserMessageId"], next_id);
        assert!(m.queued_sends.is_empty());
        assert!(m.turn_pending);
    }

    #[test]
    fn failed_concurrent_steers_redrive_in_click_order() {
        let mut m = mapper();
        active_turn(&mut m);
        let queued_id = |step: &DriverStep| match &step.events[0] {
            AgentEvent::UserMessage {
                id: Some(id),
                queued: true,
                ..
            } => id.clone(),
            other => panic!("expected queued UserMessage, got {other:?}"),
        };
        let ordinary_id = queued_id(&m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "ordinary".into(),
            }],
        }));
        let first_steer_id = queued_id(&m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "first steer".into(),
            }],
        }));
        let step = m.on_command(AgentCommand::SteerQueued {
            id: first_steer_id.clone(),
        });
        let first_rpc = step.outbound[0]["id"].as_u64().unwrap();
        let second_steer_id = queued_id(&m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "second steer".into(),
            }],
        }));
        let step = m.on_command(AgentCommand::SteerQueued {
            id: second_steer_id.clone(),
        });
        let second_rpc = step.outbound[0]["id"].as_u64().unwrap();

        m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "completed" } },
        }));
        // Responses arrive in reverse. The second click waits because the
        // earlier steer is unresolved; neither can race the ordinary FIFO.
        let step = m.on_frame(&json!({
            "id": second_rpc,
            "error": { "message": "no active turn" },
        }));
        assert!(!step.outbound.iter().any(|o| o["method"] == "turn/start"));
        let step = m.on_frame(&json!({
            "id": first_rpc,
            "error": { "message": "no active turn" },
        }));
        let starts: Vec<_> = step
            .outbound
            .iter()
            .filter(|o| o["method"] == "turn/start")
            .collect();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0]["params"]["clientUserMessageId"], first_steer_id);
        assert_eq!(m.deferred_steer_redrives.len(), 1);
        assert_eq!(
            m.deferred_steer_redrives
                .first_key_value()
                .unwrap()
                .1
                .client_msg_id,
            second_steer_id
        );
        assert_eq!(m.queued_sends[0].client_msg_id, ordinary_id);
    }

    #[test]
    fn user_interrupt_preserves_queued_steer_via_redrive() {
        // Two-driver symmetry with claude: a stop ends only the CURRENT turn
        // (maintainer decision 2026-07-11). A message whose steer was still in
        // flight when the user stopped is NOT dropped — codex answers the
        // steer with an error (turn gone), and that answer re-drives it as a
        // fresh turn, so the user's message still delivers in full.
        let mut m = mapper();
        active_turn(&mut m);
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "B".into() }],
        });
        let queued_id = match &step.events[0] {
            AgentEvent::UserMessage {
                id: Some(id),
                queued: true,
                ..
            } => id.clone(),
            other => panic!("expected a queued UserMessage, got {other:?}"),
        };
        let step = m.on_command(AgentCommand::SteerQueued {
            id: queued_id.clone(),
        });
        let steer_id = step.outbound[0]["id"].as_u64().unwrap();
        // User hits stop; the turn then ends interrupted. The in-flight steer
        // is left tracked (codex is alive and WILL answer it) — nothing drops.
        m.on_command(AgentCommand::Interrupt);
        let step = m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "interrupted" } },
        }));
        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::UserMessageUpdate { .. })),
            "the stop resolves nothing — the steer's own answer will: {:?}",
            step.events
        );
        // The steer's error answer re-drives B as a fresh turn, resolving it
        // `sent` — the queued message survives the stop.
        let step = m.on_frame(&json!({
            "id": steer_id,
            "error": { "message": "no active turn" },
        }));
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent }
                    if id == &queued_id
            )),
            "the stopped-past steer re-drives and resolves sent: {:?}",
            step.events
        );
        assert!(
            step.outbound.iter().any(|o| o["method"] == "turn/start"),
            "the saved input re-drives as a fresh turn: {:?}",
            step.outbound
        );
    }

    /// A follow-up still held in the next-run FIFO can be cancelled — it
    /// resolves `Cancelled`, leaves the queue, and never reaches Codex.
    #[test]
    fn cancel_queued_removes_a_held_followup() {
        let mut m = mapper();
        // First send opens the turn (turn/start); the start window is now open.
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "A".into() }],
        });
        assert!(m.turn_pending);
        // Second send queues during the window.
        let queued = match &m
            .on_command(AgentCommand::Send {
                blocks: vec![ContentBlock::Text { text: "B".into() }],
            })
            .events[0]
        {
            AgentEvent::UserMessage {
                id, queued: true, ..
            } => id.clone().unwrap(),
            other => panic!("expected a queued UserMessage, got {other:?}"),
        };
        // Cancel it: resolves Cancelled and empties the queue.
        let step = m.on_command(AgentCommand::CancelQueued { id: queued.clone() });
        assert_eq!(
            step.events,
            vec![AgentEvent::UserMessageUpdate {
                id: queued.clone(),
                state: UserMessageState::Cancelled,
            }]
        );
        assert!(m.queued_sends.is_empty(), "the queue is emptied");
        // turn/started now steers NOTHING — the cancelled send is gone.
        let step = m.on_frame(&json!({
            "method": "turn/started",
            "params": { "turn": { "id": "turn-Z" } },
        }));
        assert!(
            !step.outbound.iter().any(|o| o["method"] == "turn/steer"),
            "a cancelled queued send never steers: {:?}",
            step.outbound
        );
    }

    /// Feature 2 (codex): a send already steered into the running turn is
    /// mid-delivery — its steer RPC is on the wire and unanswered, so a
    /// tombstone now could vanish a bubble the agent then consumes.
    /// Cancelling it is a Notice; the steer's own answer resolves it.
    #[test]
    fn cancel_queued_mid_steer_is_a_notice() {
        let mut m = mapper();
        active_turn(&mut m);
        let steered = match &m
            .on_command(AgentCommand::Send {
                blocks: vec![ContentBlock::Text { text: "B".into() }],
            })
            .events[0]
        {
            AgentEvent::UserMessage {
                id, queued: true, ..
            } => id.clone().unwrap(),
            other => panic!("expected a queued UserMessage, got {other:?}"),
        };
        m.on_command(AgentCommand::SteerQueued {
            id: steered.clone(),
        });
        // It was explicitly steered (queue empty; RPC in flight, unanswered).
        assert!(m.queued_sends.is_empty());
        let step = m.on_command(AgentCommand::CancelQueued { id: steered });
        assert!(matches!(
            step.events.as_slice(),
            [AgentEvent::Notice { text }] if text.contains("on its way")
        ));
    }

    /// A cancel for an id that already fully resolved (nothing held, no steer
    /// in flight) is the tombstone `Cancelled`: it dismisses a dropped "not
    /// delivered" bubble on live and replay, and the reducer no-ops for an
    /// already-`sent` id.
    #[test]
    fn cancel_queued_after_resolution_is_a_tombstone() {
        let mut m = mapper();
        let step = m.on_command(AgentCommand::CancelQueued {
            id: "gone-id".into(),
        });
        assert!(step.outbound.is_empty(), "nothing goes to codex");
        assert_eq!(
            step.events,
            vec![AgentEvent::UserMessageUpdate {
                id: "gone-id".into(),
                state: UserMessageState::Cancelled,
            }]
        );
    }

    /// A queued follow-up stranded when a turn ended under a pending
    /// turn/start is rescued by the idle-flush — re-driven as a fresh turn
    /// (delivered, resolves `sent`), never left faded "queued".
    #[test]
    fn idle_flush_redrives_a_stranded_followup() {
        let mut m = mapper();
        // A turn/start in flight, with a follow-up queued behind it…
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "A".into() }],
        });
        assert!(m.turn_pending);
        let queued = match &m
            .on_command(AgentCommand::Send {
                blocks: vec![ContentBlock::Text { text: "B".into() }],
            })
            .events[0]
        {
            AgentEvent::UserMessage {
                id, queued: true, ..
            } => id.clone().unwrap(),
            other => panic!("expected a queued UserMessage, got {other:?}"),
        };
        // …then a turn ends under the pending start (no turn/started ever
        // landed), stranding the queue: reset_turn_state clears turn_pending
        // but leaves queued_sends, and turn_active was never set.
        m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "completed" } },
        }));
        assert!(!m.turn_pending && !m.turn_active);
        assert_eq!(m.queued_sends.len(), 1, "the queue is stranded");

        // Ticks below the grace do nothing…
        for _ in 0..IDLE_FLUSH_GRACE_TICKS {
            assert!(
                m.tick().events.is_empty(),
                "no flush before the idle grace expires"
            );
        }
        // …the expiring tick re-drives it: resolves sent AND opens a real turn
        // (honest delivery — a codex buffer was never sent).
        let step = m.tick();
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if *id == queued
            )),
            "the stranded follow-up resolves sent: {:?}",
            step.events
        );
        assert!(
            step.outbound.iter().any(|o| o["method"] == "turn/start"),
            "and is delivered as a fresh turn: {:?}",
            step.outbound
        );
        assert!(m.queued_sends.is_empty(), "the queue drained");
    }

    /// The interrupt watchdog (two-driver symmetry with claude): when the
    /// app-server never emits turn/completed after an interrupt (wedged turn,
    /// or the benign no-op interrupt), the grace expires and the driver
    /// synthesizes the abort so the user escapes a stuck-running state.
    #[test]
    fn interrupt_watchdog_aborts_a_hung_turn_after_the_grace() {
        let mut m = mapper();
        active_turn(&mut m);
        // A message queued behind the (soon-hung) turn.
        let queued = match &m
            .on_command(AgentCommand::Send {
                blocks: vec![ContentBlock::Text { text: "B".into() }],
            })
            .events[0]
        {
            AgentEvent::UserMessage { id, .. } => id.clone().unwrap(),
            other => panic!("expected UserMessage, got {other:?}"),
        };

        m.on_command(AgentCommand::Interrupt);
        for _ in 0..(INTERRUPT_GRACE_TICKS - 1) {
            assert!(
                m.tick().events.is_empty(),
                "no abort before the grace expires"
            );
        }
        assert!(m.turn_active, "still running until the grace fires");

        let step = m.tick();
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::TurnAborted { interrupted: true, reason, .. } if reason == "interrupted"
            )),
            "the watchdog aborts the hung turn as a user stop: {:?}",
            step.events
        );
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Dropped } if *id == queued
            )),
            "the queue drops with the watchdog abort: {:?}",
            step.events
        );
        assert!(!m.turn_active, "the turn is closed");
        assert!(m.tick().events.is_empty(), "watchdog fires exactly once");

        // A late real turn/completed for the same turn must NOT double-abort —
        // the was_active guard suppresses the second end.
        let step = m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "interrupted" } },
        }));
        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnAborted { .. })),
            "a late real turn end after the watchdog must not double-abort: {:?}",
            step.events
        );
    }

    /// A real turn/completed{interrupted} landing before the grace disarms the
    /// watchdog, so a genuine turn is never double-aborted.
    #[test]
    fn real_turn_end_disarms_the_interrupt_watchdog() {
        let mut m = mapper();
        active_turn(&mut m);
        m.on_command(AgentCommand::Interrupt);
        let step = m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "interrupted" } },
        }));
        assert!(
            step.events
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnAborted { .. })),
            "the real interrupt ends the turn: {:?}",
            step.events
        );
        assert!(m.interrupt_grace.is_none(), "the watchdog is disarmed");
        for _ in 0..(INTERRUPT_GRACE_TICKS + 1) {
            assert!(
                m.tick().events.is_empty(),
                "no double abort after a real turn end"
            );
        }
    }

    /// Interrupt pressed while idle is a no-op: the grace expires without an
    /// abort (there is no turn to stop).
    #[test]
    fn interrupt_while_idle_is_a_watchdog_no_op() {
        let mut m = mapper();
        assert!(!m.turn_active);
        m.on_command(AgentCommand::Interrupt);
        for _ in 0..(INTERRUPT_GRACE_TICKS + 1) {
            assert!(
                m.tick().events.is_empty(),
                "an idle interrupt never fabricates an abort"
            );
        }
        assert!(!m.turn_active);
    }

    #[test]
    fn steer_retries_once_with_parsed_live_turn_id() {
        let mut m = mapper();
        active_turn(&mut m);
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "go".into() }],
        });
        let queued = match &step.events[0] {
            AgentEvent::UserMessage { id: Some(id), .. } => id.clone(),
            other => panic!("expected queued UserMessage, got {other:?}"),
        };
        let step = m.on_command(AgentCommand::SteerQueued { id: queued });
        let id = step.outbound[0]["id"].as_u64().unwrap();

        let step = m.on_frame(&json!({
            "id": id,
            "error": { "message": "expected active turn id `turn-A` but found `turn-B`" },
        }));
        assert_eq!(step.outbound[0]["method"], "turn/steer");
        assert_eq!(step.outbound[0]["params"]["expectedTurnId"], "turn-B");

        // Second failure surfaces instead of looping — and resolves the
        // still-queued message as dropped, not stranded.
        let id2 = step.outbound[0]["id"].as_u64().unwrap();
        let step = m.on_frame(&json!({
            "id": id2,
            "error": { "message": "expected active turn id `turn-B` but found `turn-C`" },
        }));
        assert!(step.outbound.is_empty());
        assert!(matches!(
            &step.events[0],
            AgentEvent::Error { fatal: false, .. }
        ));
        assert!(
            matches!(
                &step.events[1],
                AgentEvent::UserMessageUpdate {
                    state: UserMessageState::Dropped,
                    ..
                }
            ),
            "final steer failure drops the message: {:?}",
            step.events
        );
    }

    #[test]
    fn idle_send_starts_turn_with_images_as_data_urls() {
        let mut m = mapper();
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![
                ContentBlock::Text { text: "see".into() },
                ContentBlock::Image {
                    media_type: "image/png".into(),
                    data: "QUJD".into(),
                },
            ],
        });
        match &step.events[0] {
            AgentEvent::UserMessage {
                text,
                attachments,
                id,
                queued,
            } => {
                assert_eq!(text, "see");
                assert_eq!(*attachments, 1);
                assert!(id.is_some(), "sends carry a delivery id");
                assert!(!queued, "a fresh-turn send is not queued");
            }
            other => panic!("expected UserMessage, got {other:?}"),
        }
        let input = &step.outbound[0]["params"]["input"];
        assert_eq!(input[0]["type"], "text");
        assert_eq!(input[1]["type"], "image");
        assert_eq!(input[1]["url"], "data:image/png;base64,QUJD");
    }

    #[test]
    fn set_mode_falls_back_to_per_turn_on_method_not_found() {
        let mut m = mapper();
        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "read-only".into(),
        });
        assert_eq!(step.outbound[0]["method"], "thread/settings/update");
        assert_eq!(step.outbound[0]["params"]["permissions"], ":read-only");
        let id = step.outbound[0]["id"].as_u64().unwrap();

        let step = m.on_frame(&json!({
            "id": id,
            "error": { "code": -32601, "message": "Method not found" },
        }));
        assert_eq!(
            step.events[0],
            AgentEvent::ModeChanged {
                mode_id: "read-only".into()
            }
        );

        // The fields now ride the next turn/start…
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "hi".into() }],
        });
        assert_eq!(step.outbound[0]["params"]["permissions"], ":read-only");
        assert_eq!(step.outbound[0]["params"]["approvalPolicy"], "on-request");

        // …and later mode changes skip the probe entirely.
        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "full-access".into(),
        });
        assert!(step.outbound.is_empty());
        assert_eq!(
            step.events[0],
            AgentEvent::ModeChanged {
                mode_id: "full-access".into()
            }
        );
    }

    #[test]
    fn effort_updates_thread_and_reconciles_the_server_read_back() {
        let mut m = mapper();
        let step = m.on_command(AgentCommand::SetEffort {
            effort_id: "high".into(),
        });
        assert!(step.events.is_empty(), "the RPC read-back owns truth");
        assert_eq!(step.outbound[0]["method"], "thread/settings/update");
        assert_eq!(step.outbound[0]["params"]["effort"], "high");
        let id = step.outbound[0]["id"].as_u64().unwrap();

        // Current app-servers announce the effective settings. That event is
        // journaled once, survives reconnect, and the RPC ack does not repeat
        // it when the notification arrived first.
        let step = m.on_frame(&json!({
            "method": "thread/settings/updated",
            "params": {
                "threadId": "thr-1",
                "threadSettings": { "effort": "high" },
            },
        }));
        assert_eq!(
            step.events,
            vec![AgentEvent::EffortState {
                effort: Some("high".into()),
                ultracode: false,
            }]
        );
        let step = m.on_frame(&json!({ "id": id, "result": {} }));
        assert!(step.events.is_empty(), "the matching ack is deduplicated");

        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "hi".into() }],
        });
        assert_eq!(step.outbound[0]["params"]["effort"], "high");
    }

    #[test]
    fn effort_fallback_keeps_plan_mode_and_turn_start_in_sync() {
        let mut m = mapper();
        m.current_mode = "plan".into();
        m.settings_update_unsupported = true;
        m.mode_per_turn = Some(mode_wire_fields("plan", Some("gpt-5.5"), Some("medium")));

        let step = m.on_command(AgentCommand::SetEffort {
            effort_id: "xhigh".into(),
        });
        assert!(step.outbound.is_empty());
        assert_eq!(
            step.events,
            vec![AgentEvent::EffortState {
                effort: Some("xhigh".into()),
                ultracode: false,
            }]
        );

        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "plan".into(),
            }],
        });
        let params = &step.outbound[0]["params"];
        assert_eq!(params["effort"], "xhigh");
        assert_eq!(
            params["collaborationMode"]["settings"]["reasoning_effort"],
            "xhigh"
        );
    }

    #[test]
    fn mode_fields_reset_hidden_state_and_offer_auto_review() {
        let auto_review = mode_wire_fields("auto-review", Some("gpt-test"), Some("high"));
        assert_eq!(auto_review["permissions"], ":workspace");
        assert_eq!(auto_review["approvalPolicy"], "on-request");
        assert_eq!(auto_review["approvalsReviewer"], "auto_review");
        assert_eq!(auto_review["collaborationMode"], Value::Null);

        let auto = mode_wire_fields("auto", Some("gpt-test"), Some("high"));
        assert_eq!(auto["approvalsReviewer"], "user");
        assert_eq!(auto["collaborationMode"], Value::Null);

        let full = mode_wire_fields("full-access", Some("gpt-test"), Some("high"));
        assert_eq!(full["permissions"], ":danger-full-access");
        assert_eq!(full["approvalPolicy"], "never");
        assert_eq!(full["approvalsReviewer"], "user");
        assert_eq!(full["collaborationMode"], Value::Null);

        let plan = mode_wire_fields("plan", Some("gpt-test"), Some("high"));
        assert_eq!(plan["approvalsReviewer"], "user");
        assert_eq!(plan["collaborationMode"]["mode"], "plan");
        assert_eq!(plan["collaborationMode"]["settings"]["model"], "gpt-test");
        assert_eq!(
            plan["collaborationMode"]["settings"]["reasoning_effort"],
            "high"
        );

        assert!(codex_modes().iter().any(|mode| mode.id == "auto-review"));
    }

    #[test]
    fn thread_open_carries_the_create_time_model_for_start_and_resume() {
        let mut spec = SpawnSpec::new("session", Vec::new(), std::path::PathBuf::from("/work"));
        spec.initial_model = Some("gpt-test".into());
        let start = thread_open_request(&spec);
        assert_eq!(start["method"], "thread/start");
        assert_eq!(start["params"]["model"], "gpt-test");

        spec.pinned_native_id = Some("thread-old".into());
        let resume = thread_open_request(&spec);
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "thread-old");
        assert_eq!(resume["params"]["model"], "gpt-test");

        spec.fork_at = Some("turn-7".into());
        let fork = thread_open_request(&spec);
        assert_eq!(fork["method"], "thread/fork");
        assert_eq!(fork["params"]["threadId"], "thread-old");
        assert_eq!(fork["params"]["lastTurnId"], "turn-7");
        assert_eq!(fork["params"]["ephemeral"], false);
        assert_eq!(fork["params"]["model"], "gpt-test");
    }

    #[test]
    fn auto_review_lifecycle_and_warning_render_visibly() {
        let mut m = mapper();
        let started = m.on_frame(&json!({
            "method": "item/autoApprovalReview/started",
            "params": {
                "reviewId": "review-1",
                "action": { "type": "applyPatch", "files": ["/work/a.rs", "/work/b.rs"] },
                "review": { "status": "inProgress" },
            },
        }));
        assert!(matches!(
            &started.events[0],
            AgentEvent::ToolCall { id, kind: ToolKind::Edit, title, locations, status: ToolStatus::InProgress }
                if id == "auto-review:review-1"
                    && title == "auto review · edit 2 files"
                    && locations == &vec!["/work/a.rs".to_string(), "/work/b.rs".to_string()]
        ));

        let completed = m.on_frame(&json!({
            "method": "item/autoApprovalReview/completed",
            "params": {
                "reviewId": "review-1",
                "action": { "type": "applyPatch", "files": ["/work/a.rs", "/work/b.rs"] },
                "review": {
                    "status": "denied", "riskLevel": "high",
                    "rationale": "Touches a protected path"
                },
            },
        }));
        assert!(matches!(
            &completed.events[1],
            AgentEvent::ToolCallUpdate {
                id, status: ToolStatus::Completed,
                content: Some(ToolContent::Output { text, .. }),
            } if id == "auto-review:review-1"
                && text.contains("denied · risk high")
                && text.contains("protected path")
        ));

        let warning = m.on_frame(&json!({
            "method": "guardianWarning",
            "params": { "threadId": "thr-1", "message": "Auto review is unavailable" },
        }));
        assert_eq!(
            warning.events,
            vec![AgentEvent::Notice {
                text: "auto review: Auto review is unavailable".into()
            }]
        );
    }

    #[test]
    fn exec_approval_offers_prefix_amendment_and_answers_with_object_decision() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "id": 77,
            "method": "item/commandExecution/requestApproval",
            "params": {
                "itemId": "item-1",
                "command": "/bin/zsh -lc 'cargo test'",
                "commandActions": [{ "type": "run", "command": "cargo test" }],
                "proposedExecpolicyAmendment": ["cargo", "test"],
            },
        }));
        let (request_id, amendment_opt) = match &step.events[0] {
            AgentEvent::PermissionRequest {
                request_id,
                options,
                title,
                ..
            } => {
                assert_eq!(title, "cargo test");
                let amendment = options
                    .iter()
                    .find(|o| o.id == "acceptWithAmendment")
                    .expect("amendment option offered");
                assert!(amendment.label.contains("cargo test"));
                (request_id.clone(), amendment.id.clone())
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        };

        let step = m.on_command(AgentCommand::Permission {
            request_id,
            option_id: amendment_opt,
            destination: None,
            feedback: None,
        });
        assert_eq!(step.outbound[0]["id"], 77);
        assert_eq!(
            step.outbound[0]["result"]["decision"]["acceptWithExecpolicyAmendment"]
                ["execpolicy_amendment"],
            json!(["cargo", "test"])
        );
    }

    /// The MCP tool-call approval is an ELICITATION request, and its answer
    /// is the MCP `action` shape — pinned against a live-mined codex 0.144
    /// frame. Answering with the exec-style `{"decision": …}` deserializes
    /// to nothing server-side and codex silently rejects the tool call
    /// ("user rejected MCP tool call"), so this shape is load-bearing.
    #[test]
    fn mcp_elicitation_approval_answers_with_action_shape() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "id": 79,
            "method": "mcpServer/elicitation/request",
            "params": {
                "threadId": "t-1",
                "turnId": "u-1",
                "serverName": "chimaera",
                "mode": "form",
                "_meta": {
                    "codex_approval_kind": "mcp_tool_call",
                    "persist": ["session", "always"],
                    "tool_description": "List the terminal sessions linked to this agent.",
                    "tool_params": {},
                },
                "message": "Allow the chimaera MCP server to run tool \"list_terminals\"?",
                "requestedSchema": { "type": "object", "properties": {} },
            },
        }));
        let request_id = match &step.events[0] {
            AgentEvent::PermissionRequest {
                request_id,
                title,
                options,
                input_preview,
                ..
            } => {
                assert_eq!(
                    title,
                    "Allow the chimaera MCP server to run tool \"list_terminals\"?"
                );
                // Only the two live-verified payloads are offered — the
                // persist variants stay off until their shape is mined.
                assert_eq!(
                    options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
                    ["accept", "decline"]
                );
                assert_eq!(input_preview["server"], "chimaera");
                request_id.clone()
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        };

        let step = m.on_command(AgentCommand::Permission {
            request_id,
            option_id: "accept".into(),
            destination: None,
            feedback: None,
        });
        assert_eq!(step.outbound[0]["id"], 79);
        assert_eq!(
            step.outbound[0]["result"],
            json!({ "action": "accept", "content": {} })
        );
    }

    /// The mined tool-call elicitation frame, parameterized by tool name.
    fn elicitation_frame(id: u64, tool: &str) -> Value {
        json!({
            "id": id,
            "method": "mcpServer/elicitation/request",
            "params": {
                "threadId": "t-1",
                "turnId": "u-1",
                "serverName": "chimaera",
                "mode": "form",
                "_meta": {
                    "codex_approval_kind": "mcp_tool_call",
                    "persist": ["session", "always"],
                    "tool_description": "d",
                    "tool_params": {},
                },
                "message": format!("Allow the chimaera MCP server to run tool \"{tool}\"?"),
                "requestedSchema": { "type": "object", "properties": {} },
            },
        })
    }

    /// The embedder's pre-approval (`SpawnSpec.mcp_auto_approve`): a listed
    /// tool's elicitation is answered accept with NO PermissionRequest; an
    /// unlisted tool on the same server still surfaces; `tools: None`
    /// pre-approves the whole server; a different server never matches.
    /// This is the codex Mastermind's ask/auto gating — the app-server
    /// elicits every MCP call regardless of approval-mode config (Pass 19).
    #[test]
    fn mcp_elicitation_pre_approval_gates_by_tool_list() {
        let allow = |tools| crate::driver::McpAutoApprove {
            server: "chimaera".into(),
            tools,
        };
        // Ask-mode shape: only the read list is silent.
        let mut m = CodexMapper::new(
            "thr-1".into(),
            Vec::new(),
            None,
            None,
            None,
            Some(allow(Some(vec!["workspace_status".into()]))),
            3,
        );
        let step = m.on_frame(&elicitation_frame(80, "workspace_status"));
        assert!(step.events.is_empty(), "pre-approved tool must not surface");
        assert_eq!(step.outbound[0]["id"], 80);
        assert_eq!(
            step.outbound[0]["result"],
            json!({ "action": "accept", "content": {} })
        );
        let step = m.on_frame(&elicitation_frame(81, "spawn_agent"));
        assert!(
            matches!(&step.events[0], AgentEvent::PermissionRequest { .. }),
            "an unlisted act tool still asks"
        );
        assert!(step.outbound.is_empty());

        // Auto-mode shape: the whole server is silent.
        let mut m = CodexMapper::new(
            "thr-1".into(),
            Vec::new(),
            None,
            None,
            None,
            Some(allow(None)),
            3,
        );
        let step = m.on_frame(&elicitation_frame(82, "spawn_agent"));
        assert!(step.events.is_empty());
        assert_eq!(
            step.outbound[0]["result"],
            json!({ "action": "accept", "content": {} })
        );

        // Another server's tool-call approval never matches the consent.
        let mut m = CodexMapper::new(
            "thr-1".into(),
            Vec::new(),
            None,
            None,
            None,
            Some(allow(None)),
            3,
        );
        let mut frame = elicitation_frame(83, "anything");
        frame["params"]["serverName"] = json!("other");
        let step = m.on_frame(&frame);
        assert!(matches!(
            &step.events[0],
            AgentEvent::PermissionRequest { .. }
        ));

        // Injection: a requested tool name that embeds a quoted read-tool
        // name must SURFACE, not auto-accept — the parse anchors the exact
        // pinned shape and rejects quote/space-carrying names.
        let mut m = CodexMapper::new(
            "thr-1".into(),
            Vec::new(),
            None,
            None,
            None,
            Some(allow(Some(vec!["read_session".into()]))),
            3,
        );
        let step = m.on_frame(&elicitation_frame(84, "x\" run tool \"read_session"));
        assert!(
            matches!(&step.events[0], AgentEvent::PermissionRequest { .. }),
            "an injected quoted name must not be pre-approved"
        );
        assert!(step.outbound.is_empty());

        // A reworded message (pinned-shape drift) surfaces instead of
        // matching — ask mode degrades loudly (tracing), never silently
        // approves.
        let mut m = CodexMapper::new(
            "thr-1".into(),
            Vec::new(),
            None,
            None,
            None,
            Some(allow(Some(vec!["workspace_status".into()]))),
            3,
        );
        let mut frame = elicitation_frame(85, "workspace_status");
        frame["params"]["message"] = json!("Allow chimaera to run the workspace_status tool?");
        let step = m.on_frame(&frame);
        assert!(matches!(
            &step.events[0],
            AgentEvent::PermissionRequest { .. }
        ));
    }

    /// The pinned-shape parser itself: exact anchors, charset-gated name.
    #[test]
    fn elicitation_tool_name_parses_only_the_pinned_shape() {
        let msg = "Allow the chimaera MCP server to run tool \"list_terminals\"?";
        assert_eq!(
            elicitation_tool_name(msg, "chimaera"),
            Some("list_terminals")
        );
        // Wrong server in the prefix → no match.
        assert_eq!(elicitation_tool_name(msg, "other"), None);
        // Injected quotes/spaces in the name → fails closed.
        let inj = "Allow the chimaera MCP server to run tool \"x\" run tool \"read_session\"?";
        assert_eq!(elicitation_tool_name(inj, "chimaera"), None);
        assert_eq!(
            elicitation_tool_name(
                "Allow the chimaera MCP server to run tool \"\"?",
                "chimaera"
            ),
            None
        );
    }

    #[test]
    fn network_approval_offers_host_amendment() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "id": 78,
            "method": "item/commandExecution/requestApproval",
            "params": {
                "itemId": "item-2",
                "command": "curl https://crates.io",
                "networkApprovalContext": { "host": "crates.io" },
                "proposedNetworkPolicyAmendments": [
                    { "action": "deny", "host": "crates.io" },
                    { "action": "allow", "host": "crates.io" },
                ],
            },
        }));
        match &step.events[0] {
            AgentEvent::PermissionRequest { title, options, .. } => {
                assert_eq!(title, "network access to crates.io");
                assert!(options.iter().any(|o| o.id == "allowHostAlways"));
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
        let step = m.on_command(AgentCommand::Permission {
            request_id: "codex-78".into(),
            option_id: "allowHostAlways".into(),
            destination: None,
            feedback: None,
        });
        let amendment = &step.outbound[0]["result"]["decision"]["applyNetworkPolicyAmendment"]
            ["network_policy_amendment"];
        assert_eq!(amendment["action"], "allow");
    }

    #[test]
    fn decline_with_feedback_steers_the_reason_into_the_turn() {
        let mut m = mapper();
        active_turn(&mut m);
        m.on_frame(&json!({
            "id": 80,
            "method": "item/commandExecution/requestApproval",
            "params": {
                "itemId": "item-3",
                "command": "rm -rf build",
                "commandActions": [{ "type": "run", "command": "rm -rf build" }],
            },
        }));
        let step = m.on_command(AgentCommand::Permission {
            request_id: "codex-80".into(),
            option_id: "decline".into(),
            destination: None,
            feedback: Some("  use `just clean` instead  ".into()),
        });
        // The decline answers the rpc first; the reason then steers into the
        // still-running turn as user input.
        assert_eq!(step.outbound[0]["id"], 80);
        assert_eq!(step.outbound[0]["result"]["decision"], "decline");
        assert_eq!(step.outbound[1]["method"], "turn/steer");
        assert_eq!(
            step.outbound[1]["params"]["input"][0]["text"],
            "use `just clean` instead"
        );
        assert!(matches!(
            &step.events[0],
            AgentEvent::PermissionResolved { option_id, .. } if option_id == "decline"
        ));
        assert!(matches!(
            &step.events[1],
            AgentEvent::UserMessage { text, .. } if text == "use `just clean` instead"
        ));

        // Feedback on an ACCEPT is never delivered (nothing to steer).
        m.on_frame(&json!({
            "id": 81,
            "method": "item/commandExecution/requestApproval",
            "params": { "itemId": "item-4", "command": "ls",
                        "commandActions": [{ "type": "run", "command": "ls" }] },
        }));
        let step = m.on_command(AgentCommand::Permission {
            request_id: "codex-81".into(),
            option_id: "accept".into(),
            destination: None,
            feedback: Some("stray text".into()),
        });
        assert_eq!(step.outbound.len(), 1, "accept sends only the decision");
        assert_eq!(step.events.len(), 1, "no user echo on accept");
    }

    #[test]
    fn file_change_approval_uses_tracked_locations() {
        let mut m = mapper();
        m.on_frame(&json!({
            "method": "item/started",
            "params": { "item": {
                "id": "fc-1", "type": "fileChange",
                "changes": [{ "path": "src/a.rs", "diff": "…", "kind": { "type": "update" } }],
            }},
        }));
        let step = m.on_frame(&json!({
            "id": 79,
            "method": "item/fileChange/requestApproval",
            "params": { "itemId": "fc-1" },
        }));
        match &step.events[0] {
            AgentEvent::PermissionRequest { title, options, .. } => {
                assert_eq!(title, "apply changes to src/a.rs");
                // Patch approvals accept only accept/acceptForSession/decline.
                assert!(options.iter().all(|o| o.id != "acceptWithAmendment"));
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[test]
    fn output_deltas_stream_capped_then_result_replaces() {
        let mut m = mapper();
        active_turn(&mut m);
        m.on_frame(&json!({
            "method": "item/started",
            "params": { "item": { "id": "cmd-1", "type": "commandExecution",
                "commandActions": [{ "command": "yes" }] } },
        }));
        let step = m.on_frame(&json!({
            "method": "item/commandExecution/outputDelta",
            "params": { "itemId": "cmd-1", "delta": "yyy\n" },
        }));
        assert_eq!(
            step.events[0],
            AgentEvent::ToolOutputDelta {
                id: "cmd-1".into(),
                text: "yyy\n".into()
            }
        );
        // Beyond the head budget, deltas stop flowing…
        let huge = "x".repeat(crate::model::TOOL_OUTPUT_HEAD);
        m.on_frame(&json!({
            "method": "item/commandExecution/outputDelta",
            "params": { "itemId": "cmd-1", "delta": huge },
        }));
        let step = m.on_frame(&json!({
            "method": "item/commandExecution/outputDelta",
            "params": { "itemId": "cmd-1", "delta": "more" },
        }));
        assert!(step.events.is_empty(), "capped stream stays silent");
        // …and the completed item is authoritative.
        let step = m.on_frame(&json!({
            "method": "item/completed",
            "params": { "item": { "id": "cmd-1", "type": "commandExecution",
                "status": "completed", "exitCode": 0, "aggregatedOutput": "final" } },
        }));
        match &step.events[0] {
            AgentEvent::ToolCallUpdate {
                content: Some(ToolContent::Output { text, .. }),
                ..
            } => assert_eq!(text, "final"),
            other => panic!("expected output update, got {other:?}"),
        }
    }

    #[test]
    fn plan_and_context_events_map() {
        let mut m = mapper();
        active_turn(&mut m);
        let step = m.on_frame(&json!({
            "method": "turn/plan/updated",
            "params": { "plan": [
                { "step": "a", "status": "completed" },
                { "step": "b", "status": "inProgress" },
            ]},
        }));
        assert_eq!(
            step.events[0],
            AgentEvent::Plan {
                entries: vec![
                    crate::model::PlanEntry {
                        content: "a".into(),
                        status: crate::model::PlanStatus::Done,
                        ..Default::default()
                    },
                    crate::model::PlanEntry {
                        content: "b".into(),
                        status: crate::model::PlanStatus::InProgress,
                        ..Default::default()
                    },
                ]
            }
        );
        // Context % reads the LAST request's tokens, not the running total.
        let step = m.on_frame(&json!({
            "method": "thread/tokenUsage/updated",
            "params": { "tokenUsage": {
                "total": { "totalTokens": 999_999 },
                "last": { "totalTokens": 30_000 },
                "modelContextWindow": 300_000,
            }},
        }));
        match &step.events[0] {
            AgentEvent::ContextUsage {
                total_tokens,
                max_tokens,
                percentage,
            } => {
                assert_eq!(*total_tokens, 30_000);
                assert_eq!(*max_tokens, 300_000);
                assert!((percentage - 10.0).abs() < 0.01);
            }
            other => panic!("expected ContextUsage, got {other:?}"),
        }
    }

    #[test]
    fn request_user_input_roundtrips_and_server_resolution_withdraws() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "id": 91,
            "method": "item/tool/requestUserInput",
            "params": {
                "threadId": "thr-1", "turnId": "turn-A", "itemId": "item-q",
                "questions": [{
                    "id": "q1", "header": "Scope", "question": "Which files?",
                    "options": [{ "label": "all", "description": "" }, { "label": "src only" }],
                }],
            },
        }));
        match &step.events[0] {
            AgentEvent::QuestionRequest {
                request_id,
                questions,
                ..
            } => {
                assert_eq!(request_id, "codex-91");
                assert_eq!(questions[0].id, "q1");
                assert_eq!(questions[0].options.len(), 2);
            }
            other => panic!("expected QuestionRequest, got {other:?}"),
        }
        let mut answers = std::collections::HashMap::new();
        answers.insert("q1".to_string(), vec!["src only".to_string()]);
        let step = m.on_command(AgentCommand::Answer {
            request_id: "codex-91".into(),
            answers,
        });
        assert_eq!(step.outbound[0]["id"], 91);
        assert_eq!(
            step.outbound[0]["result"]["answers"]["q1"]["answers"],
            json!(["src only"])
        );
        assert!(matches!(
            &step.events[0],
            AgentEvent::QuestionResolved { request_id, answers }
                if request_id == "codex-91"
                    && answers.get("q1") == Some(&vec!["src only".to_string()])
        ));

        // A second prompt withdrawn by the server (timeout / other client).
        m.on_frame(&json!({
            "id": 92,
            "method": "item/tool/requestUserInput",
            "params": { "questions": [{ "id": "q2", "question": "More?" }] },
        }));
        let step = m.on_frame(&json!({
            "method": "serverRequest/resolved",
            "params": { "threadId": "thr-1", "requestId": 92 },
        }));
        assert!(matches!(
            &step.events[0],
            AgentEvent::QuestionResolved { request_id, answers }
                if request_id == "codex-92" && answers.is_empty()
        ));
    }

    #[test]
    fn stale_answer_and_permission_resolve_definitively() {
        // Mirror of the claude driver's contract: a reply to an ask this
        // process never issued resolves + notices instead of dropping.
        let mut m = mapper();
        let mut answers = std::collections::HashMap::new();
        answers.insert("q".to_string(), vec!["a".to_string()]);
        let step = m.on_command(AgentCommand::Answer {
            request_id: "codex-77".into(),
            answers,
        });
        assert!(step.outbound.is_empty(), "no live request to answer");
        assert!(matches!(
            &step.events[0],
            AgentEvent::QuestionResolved { request_id, answers }
                if request_id == "codex-77" && answers.is_empty()
        ));
        assert!(matches!(
            &step.events[1],
            AgentEvent::Notice { text } if text.contains("no longer active")
        ));

        let step = m.on_command(AgentCommand::Permission {
            request_id: "codex-78".into(),
            option_id: "accept".into(),
            destination: None,
            feedback: None,
        });
        assert!(step.outbound.is_empty());
        assert!(matches!(
            &step.events[0],
            AgentEvent::PermissionResolved { request_id, option_id }
                if request_id == "codex-78" && option_id == "expired"
        ));
        assert!(matches!(
            &step.events[1],
            AgentEvent::Notice { text } if text.contains("no longer active")
        ));
    }

    #[test]
    fn drain_pending_resolves_every_outstanding_ask() {
        let mut m = mapper();
        m.on_frame(&json!({
            "id": 91,
            "method": "item/tool/requestUserInput",
            "params": { "questions": [{ "id": "q1", "question": "Q?" }] },
        }));
        m.on_frame(&json!({
            "id": 92,
            "method": "item/commandExecution/requestApproval",
            "params": { "threadId": "thr-1", "itemId": "item-1", "command": "make" },
        }));
        let events = Mapper::drain_pending(&mut m);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::QuestionResolved { request_id, answers }
                if request_id == "codex-91" && answers.is_empty()
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::PermissionResolved { request_id, option_id }
                if request_id == "codex-92" && option_id == "expired"
        )));
        assert!(
            Mapper::drain_pending(&mut m).is_empty(),
            "drain is exhaustive"
        );
    }

    #[test]
    fn full_access_auto_decline_notices_once_per_turn() {
        let mut m = mapper();
        // full-access → approvalPolicy "never": codex declines by itself.
        m.settings_update_unsupported = true;
        m.on_command(AgentCommand::SetMode {
            mode_id: "full-access".into(),
        });
        active_turn(&mut m);
        let declined = |id: &str| {
            json!({
                "method": "item/completed",
                "params": { "item": {
                    "id": id, "type": "commandExecution", "status": "declined",
                    "command": "curl example.com",
                }},
            })
        };
        let step = m.on_frame(&declined("item-1"));
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::Notice { text } if text.contains("full access never asks")
            )),
            "auto-decline must be named, not just a failed tool card: {:?}",
            step.events
        );
        let step = m.on_frame(&declined("item-2"));
        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::Notice { .. })),
            "one notice per turn"
        );

        // In auto mode a declined item follows the USER's own deny — silent.
        let mut m = mapper();
        active_turn(&mut m);
        let step = m.on_frame(&declined("item-3"));
        assert!(!step
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::Notice { .. })));
    }

    #[test]
    fn turn_opening_send_anchors_checkpoint_but_steer_does_not() {
        let mut m = mapper();
        // First turn-opening send: checkpoint with nothing preceding.
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "one".into() }],
        });
        let first_id = match (&step.events[0], &step.events[1]) {
            (
                AgentEvent::UserMessage { .. },
                AgentEvent::Checkpoint {
                    user_message_id,
                    preceding_uuid,
                },
            ) => {
                assert!(preceding_uuid.is_none(), "nothing precedes the first send");
                user_message_id.clone()
            }
            other => panic!("expected UserMessage+Checkpoint, got {other:?}"),
        };
        // A steered mid-turn send joins the running turn: no checkpoint (the
        // rewind rolls back whole turns, so a steer is not a boundary).
        m.on_frame(&json!({
            "method": "turn/started",
            "params": { "turn": { "id": "turn-A" } },
        }));
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "also".into(),
            }],
        });
        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::Checkpoint { .. })),
            "steered sends must not anchor checkpoints"
        );
        let queued_id = match &step.events[0] {
            AgentEvent::UserMessage { id: Some(id), .. } => id.clone(),
            other => panic!("expected queued UserMessage, got {other:?}"),
        };
        let step = m.on_command(AgentCommand::SteerQueued { id: queued_id });
        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::Checkpoint { .. })),
            "explicit steer must not anchor a checkpoint"
        );
        let steer_rpc = step.outbound[0]["id"].as_u64().unwrap();
        m.on_frame(&json!({ "id": steer_rpc, "result": {} }));
        // The next turn-opening send chains its preceding uuid.
        m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "completed" } },
        }));
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "two".into() }],
        });
        match &step.events[1] {
            AgentEvent::Checkpoint { preceding_uuid, .. } => {
                assert_eq!(preceding_uuid.as_deref(), Some(first_id.as_str()));
            }
            other => panic!("expected Checkpoint, got {other:?}"),
        }
    }

    #[test]
    fn compact_command_sends_thread_compact_start_and_error_surfaces() {
        let mut m = mapper();
        let step = m.on_command(AgentCommand::Compact);
        assert_eq!(step.outbound[0]["method"], "thread/compact/start");
        assert_eq!(step.outbound[0]["params"]["threadId"], "thr-1");
        let id = step.outbound[0]["id"].as_u64().unwrap();
        // The ack is an empty result — nothing to render (the compaction
        // turn's contextCompaction item is the visible notice).
        let step = m.on_frame(&json!({ "id": id, "result": {} }));
        assert!(step.events.is_empty());
        // A refusal surfaces as a non-fatal error.
        let step = m.on_command(AgentCommand::Compact);
        let id = step.outbound[0]["id"].as_u64().unwrap();
        let step = m.on_frame(&json!({
            "id": id,
            "error": { "message": "not now" },
        }));
        assert!(matches!(
            &step.events[0],
            AgentEvent::Error { fatal: false, message } if message.contains("not now")
        ));
    }

    #[test]
    fn context_compaction_item_maps_started_and_completed_without_duplicate() {
        let mut m = mapper();
        m.on_frame(&json!({
            "method": "turn/started",
            "params": { "threadId": "thr-1", "turn": { "id": "compact-turn" } },
        }));
        let started = m.on_frame(&json!({
            "method": "item/started",
            "params": {
                "threadId": "thr-1",
                "item": { "type": "contextCompaction", "id": "compact-1" },
            },
        }));
        assert!(matches!(
            &started.events[0],
            AgentEvent::ContextCompaction {
                phase: CompactionPhase::Started,
                ..
            }
        ));

        let completed = m.on_frame(&json!({
            "method": "item/completed",
            "params": {
                "threadId": "thr-1",
                "item": { "type": "contextCompaction", "id": "compact-1" },
            },
        }));
        assert!(matches!(
            &completed.events[0],
            AgentEvent::ContextCompaction {
                phase: CompactionPhase::Completed,
                ..
            }
        ));

        let deprecated = m.on_frame(&json!({
            "method": "thread/compacted",
            "params": { "threadId": "thr-1" },
        }));
        assert!(deprecated.events.is_empty());
    }

    #[test]
    fn auto_resolution_deadline_skips_question_with_empty_answers() {
        let mut m = mapper();
        let asked = m.on_frame(&json!({
            "id": 93,
            "method": "item/tool/requestUserInput",
            "params": {
                "questions": [{ "id": "q1", "question": "Proceed?" }],
                "autoResolutionMs": 50,
            },
        }));
        assert!(matches!(
            &asked.events[0],
            AgentEvent::QuestionRequest {
                expires_at_ms: Some(_),
                ..
            }
        ));
        // Before the deadline: nothing expires.
        let step = m.expire_questions(std::time::Instant::now());
        assert!(step.outbound.is_empty() && step.events.is_empty());
        // Past the deadline: empty answers + withdrawal + a visible note.
        let later = std::time::Instant::now() + Duration::from_millis(60);
        let step = m.expire_questions(later);
        assert_eq!(step.outbound[0]["id"], 93);
        assert_eq!(step.outbound[0]["result"]["answers"], json!({}));
        assert!(matches!(
            &step.events[0],
            AgentEvent::QuestionResolved { request_id, .. } if request_id == "codex-93"
        ));
        assert!(matches!(
            &step.events[1],
            AgentEvent::Notice { text } if text.contains("auto-resolution")
        ));
        // A question without the field never expires.
        let asked = m.on_frame(&json!({
            "id": 94,
            "method": "item/tool/requestUserInput",
            "params": { "questions": [{ "id": "q2", "question": "Still there?" }] },
        }));
        assert!(matches!(
            &asked.events[0],
            AgentEvent::QuestionRequest {
                expires_at_ms: None,
                ..
            }
        ));
        let far = std::time::Instant::now() + Duration::from_secs(3600);
        let step = m.expire_questions(far);
        assert!(step.outbound.is_empty(), "no deadline, no auto-skip");
    }

    #[test]
    fn image_generation_reemits_with_saved_path() {
        let mut m = mapper();
        m.on_frame(&json!({
            "method": "item/started",
            "params": { "item": { "id": "img-1", "type": "imageGeneration", "status": "inProgress" } },
        }));
        let step = m.on_frame(&json!({
            "method": "item/completed",
            "params": { "item": {
                "id": "img-1", "type": "imageGeneration", "status": "completed",
                "revisedPrompt": "a chimaera in watercolor",
                "savedPath": "/tmp/out/image.png",
            }},
        }));
        match &step.events[0] {
            AgentEvent::ToolCall {
                locations, title, ..
            } => {
                assert_eq!(locations, &vec!["/tmp/out/image.png".to_string()]);
                assert!(title.contains("watercolor"));
            }
            other => panic!("expected upsert ToolCall, got {other:?}"),
        }
        assert!(matches!(
            &step.events[1],
            AgentEvent::ToolCallUpdate {
                status: ToolStatus::Completed,
                ..
            }
        ));
    }

    #[test]
    fn model_rerouted_switches_model_with_reason() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "method": "model/rerouted",
            "params": {
                "threadId": "thr-1", "turnId": "turn-A",
                "fromModel": "gpt-5.5", "toModel": "gpt-5.4-mini",
                "reason": "highRiskCyberActivity",
            },
        }));
        match &step.events[0] {
            AgentEvent::ModelSwitched {
                from, to, reason, ..
            } => {
                assert_eq!(from.as_deref(), Some("gpt-5.5"));
                assert_eq!(to, "gpt-5.4-mini");
                assert_eq!(reason.as_deref(), Some("highRiskCyberActivity"));
            }
            other => panic!("expected ModelSwitched, got {other:?}"),
        }
        assert!(matches!(
            &step.events[1],
            AgentEvent::Notice { text } if text.contains("routed to gpt-5.4-mini")
        ));
        match &step.events[2] {
            AgentEvent::Init { model, .. } => {
                assert_eq!(model.as_deref(), Some("gpt-5.4-mini"));
            }
            other => panic!("expected Init, got {other:?}"),
        }
    }

    #[test]
    fn thread_name_and_account_read_map() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "method": "thread/name/updated",
            "params": { "threadId": "thr-1", "threadName": "Fix the tests" },
        }));
        assert_eq!(
            step.events[0],
            AgentEvent::SessionTitle {
                title: "Fix the tests".into()
            }
        );

        let step = m.on_command(AgentCommand::GetUsage);
        let id = step.outbound[0]["id"].as_u64().unwrap();
        assert_eq!(step.outbound[0]["method"], "account/read");
        let step = m.on_frame(&json!({
            "id": id,
            "result": { "rate_limit": {
                "primary_window": { "used_percent": 42.0, "limit_window_seconds": 18000,
                                    "reset_at": 1800000000u64 },
                "secondary_window": { "used_percent": 12.0, "limit_window_seconds": 604800 },
            }},
        }));
        match &step.events[0] {
            AgentEvent::RateLimit {
                utilization, label, ..
            } => {
                assert!((utilization - 42.0).abs() < 0.01);
                assert_eq!(label.as_deref(), Some("session limit"));
            }
            other => panic!("expected RateLimit, got {other:?}"),
        }
        match &step.events[1] {
            AgentEvent::UsageReport { windows } => {
                assert_eq!(windows.len(), 2);
                assert_eq!(windows[1].label, "7d limit");
            }
            other => panic!("expected UsageReport, got {other:?}"),
        }
    }

    /// Live 0.144.2 shape: the spawn marker on the PARENT thread
    /// (item/completed only; item.id is the collab call id).
    fn sub_agent_activity(kind: &str, agent_thread: &str) -> Value {
        json!({
            "method": "item/completed",
            "params": {
                "threadId": "thr-1",
                "turnId": "turn-A",
                "item": {
                    "type": "subAgentActivity",
                    "id": "call_spawn1",
                    "kind": kind,
                    "agentThreadId": agent_thread,
                    "agentPath": "/root/agent_a",
                },
            },
        })
    }

    #[test]
    fn collab_spawn_creates_an_agent_row_and_foreign_frames_fold_progress() {
        let mut m = mapper();
        active_turn(&mut m);

        let step = m.on_frame(&sub_agent_activity("started", "sub-1"));
        assert_eq!(
            step.events,
            vec![AgentEvent::ToolCall {
                id: "agent:sub-1".into(),
                kind: ToolKind::Agent,
                title: "Agent: agent_a".into(),
                locations: Vec::new(),
                status: ToolStatus::InProgress,
            }]
        );

        // The agent thread's own frames fold into the row's progress line —
        // the label change rides item/started; a non-tool completed emits
        // nothing new (the label is the same item's).
        let step = m.on_frame(&json!({
            "method": "item/started",
            "params": { "threadId": "sub-1", "turnId": "t-s1",
                        "item": { "type": "reasoning", "id": "rs-1" } },
        }));
        assert_eq!(
            step.events,
            vec![AgentEvent::ToolCallUpdate {
                id: "agent:sub-1".into(),
                status: ToolStatus::InProgress,
                content: Some(ToolContent::Output {
                    text: "thinking".into(),
                    truncated: false,
                }),
            }]
        );
        let step = m.on_frame(&json!({
            "method": "item/completed",
            "params": { "threadId": "sub-1", "turnId": "t-s1",
                        "item": { "type": "reasoning", "id": "rs-1" } },
        }));
        assert_eq!(step.events, vec![], "non-tool completed is a no-emit");
        let step = m.on_frame(&json!({
            "method": "thread/tokenUsage/updated",
            "params": { "threadId": "sub-1",
                        "tokenUsage": { "total": { "totalTokens": 1234 } } },
        }));
        match &step.events[0] {
            AgentEvent::ToolCallUpdate {
                content: Some(ToolContent::Output { text, .. }),
                ..
            } => assert_eq!(text, "thinking · 1234 tokens"),
            other => panic!("expected progress update, got {other:?}"),
        }
        // A sub-step token tick is throttled out; a big move emits.
        let step = m.on_frame(&json!({
            "method": "thread/tokenUsage/updated",
            "params": { "threadId": "sub-1",
                        "tokenUsage": { "total": { "totalTokens": 1300 } } },
        }));
        assert_eq!(step.events, vec![], "sub-step token ticks are throttled");
        let step = m.on_frame(&json!({
            "method": "thread/tokenUsage/updated",
            "params": { "threadId": "sub-1",
                        "tokenUsage": { "total": { "totalTokens": 2000 } } },
        }));
        assert_eq!(step.events.len(), 1, "a >=256-token move emits");

        // Its turn completing closes the row (answered, idle)…
        let step = m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "threadId": "sub-1",
                        "turn": { "id": "t-s1", "status": "completed" } },
        }));
        match &step.events[0] {
            AgentEvent::ToolCallUpdate {
                id,
                status: ToolStatus::Completed,
                content: Some(ToolContent::Output { text, .. }),
            } => {
                assert_eq!(id, "agent:sub-1");
                assert!(text.starts_with("answered"), "close note: {text}");
            }
            other => panic!("expected close update, got {other:?}"),
        }
        // …and the parent turn is untouched by any of it.
        assert!(m.turn_active, "foreign turn end must not close the parent");

        // A follow-up (send_input) re-opens as a NEW row (next stint): the
        // UI's tool-status guard is monotonic, so the closed row stays
        // closed and the fresh work gets a fresh card.
        let step = m.on_frame(&sub_agent_activity("interacted", "sub-1"));
        assert_eq!(
            step.events,
            vec![
                AgentEvent::ToolCall {
                    id: "agent:sub-1#2".into(),
                    kind: ToolKind::Agent,
                    title: "Agent: agent_a".into(),
                    locations: Vec::new(),
                    status: ToolStatus::InProgress,
                },
                AgentEvent::ToolCallUpdate {
                    id: "agent:sub-1#2".into(),
                    status: ToolStatus::InProgress,
                    content: Some(ToolContent::Output {
                        // reasoning items aren't tools — only the token total
                        // rides along with the note.
                        text: "follow-up input · 2000 tokens".into(),
                        truncated: false,
                    }),
                },
            ]
        );

        // An explicit shutdown (close_agent) closes the current stint's row.
        let step = m.on_frame(&sub_agent_activity("interrupted", "sub-1"));
        match &step.events[0] {
            AgentEvent::ToolCallUpdate {
                id,
                status: ToolStatus::Completed,
                content: Some(ToolContent::Output { text, .. }),
            } => {
                assert_eq!(id, "agent:sub-1#2");
                assert!(text.starts_with("closed"), "close note: {text}");
            }
            other => panic!("expected close update, got {other:?}"),
        }
    }

    #[test]
    fn teardown_fails_open_agent_rows_and_subagent_filechange_names_paths() {
        let mut m = mapper();
        active_turn(&mut m);
        m.on_frame(&sub_agent_activity("started", "sub-1"));

        // A subagent's fileChange records its paths so a requestApproval
        // resolving by itemId still names the files being touched.
        m.on_frame(&json!({
            "method": "item/started",
            "params": { "threadId": "sub-1", "turnId": "t-s1",
                        "item": { "type": "fileChange", "id": "fc-9",
                                   "changes": [{ "path": "src/a.rs", "diff": "x" }] } },
        }));
        assert_eq!(
            m.item_locations.get("fc-9").map(Vec::as_slice),
            Some(&["src/a.rs".to_string()][..]),
            "subagent fileChange paths feed the approval card"
        );

        // Process death: the open row must get a terminal event before
        // Exited, or replay shows it running forever (and the UI's turn-end
        // reconcile would flip it green).
        let events = m.drain_pending();
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::ToolCallUpdate { id, status: ToolStatus::Failed, .. }
                    if id == "agent:sub-1"
            )),
            "teardown fails the dangling agent row: {events:?}"
        );
    }

    #[test]
    fn foreign_thread_frames_never_touch_the_parent_transcript() {
        let mut m = mapper();
        active_turn(&mut m);

        // A subagent's prose, turn end, name, and reroute frames — all tagged
        // with its own threadId — must not leak into the parent's stream.
        let mut events = Vec::new();
        for frame in [
            json!({ "method": "item/agentMessage/delta",
                    "params": { "threadId": "sub-9", "itemId": "m1", "delta": "42" } }),
            json!({ "method": "item/completed",
                    "params": { "threadId": "sub-9", "turnId": "t",
                                "item": { "type": "agentMessage", "id": "m1", "text": "42" } } }),
            json!({ "method": "turn/completed",
                    "params": { "threadId": "sub-9",
                                "turn": { "id": "t", "status": "completed" } } }),
            json!({ "method": "thread/name/updated",
                    "params": { "threadId": "sub-9", "threadName": "sub thread" } }),
            json!({ "method": "thread/tokenUsage/updated",
                    "params": { "threadId": "sub-9",
                                "tokenUsage": { "total": { "totalTokens": 9 },
                                                 "last": { "totalTokens": 9 },
                                                 "modelContextWindow": 100 } } }),
        ] {
            events.extend(m.on_frame(&frame).events);
            events.extend(m.flush());
        }
        assert_eq!(events, Vec::new(), "foreign frames leaked: {events:?}");
        assert!(m.turn_active, "parent turn survives foreign turn ends");
    }

    #[test]
    fn parent_abort_fails_dangling_agent_rows_before_the_abort_marker() {
        let mut m = mapper();
        active_turn(&mut m);
        m.on_frame(&sub_agent_activity("started", "sub-1"));

        let step = m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "threadId": "thr-1",
                        "turn": { "id": "turn-A", "status": "interrupted",
                                   "durationMs": 10 } },
        }));
        let close = step
            .events
            .iter()
            .position(|e| {
                matches!(e, AgentEvent::ToolCallUpdate { id, status: ToolStatus::Failed, .. }
                             if id == "agent:sub-1")
            })
            .expect("dangling row closes failed");
        let abort = step
            .events
            .iter()
            .position(|e| matches!(e, AgentEvent::TurnAborted { .. }))
            .expect("abort marker");
        assert!(
            close < abort,
            "row must close before the abort so the UI reconcile can't flip it green"
        );
        assert!(m.collab_agents.is_empty(), "the set clears with the turn");
    }

    #[test]
    fn collab_wait_tool_renders_an_upserted_tool_row() {
        let mut m = mapper();
        active_turn(&mut m);
        // Completed WITHOUT a prior item/started (instant call): the arm
        // upserts the row, then resolves it.
        let step = m.on_frame(&json!({
            "method": "item/completed",
            "params": { "threadId": "thr-1", "turnId": "turn-A",
                        "item": { "type": "collabAgentToolCall", "id": "call_w1",
                                   "tool": "wait", "status": "completed" } },
        }));
        assert_eq!(
            step.events,
            vec![
                AgentEvent::ToolCall {
                    id: "call_w1".into(),
                    kind: ToolKind::Other,
                    title: "waiting for subagents".into(),
                    locations: Vec::new(),
                    status: ToolStatus::InProgress,
                },
                AgentEvent::ToolCallUpdate {
                    id: "call_w1".into(),
                    status: ToolStatus::Completed,
                    content: None,
                },
            ]
        );
    }
}
