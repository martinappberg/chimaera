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

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::ndjson::JsonlChild;

/// CLI version these frame shapes were verified against (2026-07-07).
pub const TESTED_CODEX_VERSION: &str = "0.142.5";

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

use std::collections::{HashMap, HashSet};

use tokio::task::JoinHandle;

use crate::driver::{
    run_driver, AgentAdapter, Driver, DriverExit, DriverIo, DriverStep, Handshake, Mapper,
    SpawnSpec, IDLE_FLUSH_GRACE_TICKS, INTERRUPT_GRACE_TICKS,
};
use crate::model::{
    cap_output, truncate_label, AgentCommand, AgentEvent, ChunkKind, Coalescer, ContentBlock,
    PermissionOption, PermissionOptionKind, ToolContent, ToolKind, ToolStatus, Usage,
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
        // A failed conversation rewind degrades to a notice, not a dead pane:
        // the thread resumed whole, only the rollback was refused/ignored.
        let initial = hs
            .rollback_error
            .into_iter()
            .map(|err| DriverStep {
                events: vec![AgentEvent::Notice {
                    text: format!(
                        "conversation rewind failed: {err} (the agent may still see the rewound turns)"
                    ),
                }],
                outbound: Vec::new(),
            })
            .collect();
        Ok(Handshake {
            mapper: CodexMapper::new(
                hs.thread_id,
                hs.models,
                spec.agent_version.clone(),
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

    let open = match &spec.pinned_native_id {
        Some(thread_id) => json!({
            "id": 1, "method": "thread/resume",
            "params": { "threadId": thread_id, "cwd": spec.cwd },
        }),
        None => json!({
            "id": 1, "method": "thread/start",
            "params": { "cwd": spec.cwd },
        }),
    };
    if sink.send(&open).await.is_err() {
        return Err("thread open write failed".into());
    }
    let result = await_rpc_result(stream, 1).await?;
    let thread_id = result["thread"]["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| format!("thread open result missing thread.id: {result}"))?;
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
    Ok(CodexHandshake {
        thread_id,
        models,
        next_id,
        rollback_error,
    })
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
    },
    Interrupt,
    /// thread/settings/update probe; falls back to per-turn fields on
    /// method-not-found (-32601 / "method not found" — the extension's own
    /// feature detection).
    SettingsUpdate {
        mode_id: String,
        per_turn: Value,
    },
    /// account/read — rate-limit telemetry; `report` also renders /usage.
    AccountRead {
        report: bool,
    },
    /// thread/compact/start ack; the compaction itself runs as its own turn
    /// whose contextCompaction item lands the "context compacted" notice.
    Compact,
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
    /// Reasoning effort for subsequent turns (live-verified: turn/start
    /// accepts an "effort" param).
    pending_effort: Option<String>,
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
    /// Sends captured during the turn/start→turn/started window; flushed as
    /// steers once the turn id arrives (or re-driven if the start fails).
    buffered_sends: Vec<(Value, String)>,
    /// Interrupt watchdog: ticks remaining before we synthesize the abort the
    /// app-server never sent. Armed on `Interrupt`, counted down in `tick`,
    /// disarmed when a turn ends (`reset_turn_state`) or a fresh turn opens.
    /// See `INTERRUPT_GRACE_TICKS`.
    interrupt_grace: Option<u32>,
    /// Idle-flush watchdog: ticks remaining before we rescue a stranded
    /// buffer. Managed in `tick`, armed when the driver is idle (no active or
    /// pending turn) with `buffered_sends` still holding messages a turn end
    /// left un-steered. Symmetric to claude's idle-flush; see
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
    /// One auto-decline notice per turn: full-access maps to approvalPolicy
    /// "never" (the official extension's table), so a genuinely blocked
    /// action is DECLINED by codex itself with no approval card possible —
    /// without this notice the agent's own "I'm blocked" prose is the only
    /// trace ("harness is blocking").
    decline_notified: bool,
    pending_rpcs: HashMap<u64, PendingRpc>,
    /// fileChange item id → touched paths (approval titles look them up).
    item_locations: HashMap<String, Vec<String>>,
    /// Live output bytes already streamed per item (caps the deltas; the
    /// completed item's aggregatedOutput replaces them authoritatively).
    out_streamed: HashMap<String, usize>,
    /// Latest cumulative token usage (turn/completed carries none here).
    last_usage: Usage,
    /// The last turn-OPENING send's minted uuid — the fork anchor chain for
    /// Checkpoint events. Steered/buffered sends join a running turn, and
    /// thread/rollback drops whole turns, so only turn openers anchor.
    last_checkpoint: Option<String>,
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
        mode("full-access", "Full access"),
        mode("plan", "Plan mode"),
    ]
}

impl CodexMapper {
    fn new(
        thread_id: String,
        models: Vec<crate::model::ModelInfo>,
        agent_version: Option<String>,
        next_id: u64,
    ) -> Self {
        Self {
            thread_id,
            models,
            agent_version,
            model: None,
            pending_model: None,
            pending_effort: None,
            // codex's shipped default: workspace sandbox, on-request asks.
            current_mode: "auto".to_string(),
            mode_per_turn: None,
            settings_update_unsupported: false,
            turn_id: String::new(),
            turn_active: false,
            turn_pending: false,
            buffered_sends: Vec::new(),
            interrupt_grace: None,
            idle_flush_grace: None,
            coalescer: Coalescer::new(),
            streamed: HashSet::new(),
            pending_approvals: HashMap::new(),
            pending_questions: HashMap::new(),
            safety_notified: false,
            decline_notified: false,
            pending_rpcs: HashMap::new(),
            item_locations: HashMap::new(),
            out_streamed: HashMap::new(),
            last_usage: Usage::default(),
            last_checkpoint: None,
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

        match method {
            "turn/started" => {
                self.turn_id = frame["params"]["turn"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                self.turn_active = true;
                self.turn_pending = false;
                // A fresh turn is a clean slate for the interrupt watchdog: an
                // interrupt armed against a previous (or idle) state must not
                // abort this new turn.
                self.interrupt_grace = None;
                step.events.push(AgentEvent::TurnStarted {
                    turn_id: self.turn_id.clone(),
                });
                // Sends captured during the start window steer into the
                // now-identified turn instead of being lost.
                self.flush_buffered(&mut step);
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
                                content: p["step"].as_str()?.to_string(),
                                status: match p["status"].as_str() {
                                    Some("inProgress") | Some("in_progress") => {
                                        crate::model::PlanStatus::InProgress
                                    }
                                    Some("completed") => crate::model::PlanStatus::Done,
                                    _ => crate::model::PlanStatus::Todo,
                                },
                            })
                        })
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
                step.events.push(AgentEvent::Notice {
                    text: "context compacted".into(),
                });
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
                let mut aborted = false;
                if was_active {
                    let turn = &frame["params"]["turn"];
                    let mut usage = self.last_usage.clone();
                    usage.duration_ms = turn["durationMs"].as_u64().unwrap_or(0);
                    let turn_id = turn["id"].as_str().unwrap_or(&self.turn_id).to_string();
                    if turn["status"] == "interrupted" {
                        // status "interrupted" only follows a turn/interrupt RPC
                        // — codex's wire carries the user-stop fact structurally.
                        step.events.push(AgentEvent::TurnAborted {
                            turn_id,
                            reason: "interrupted".into(),
                            interrupted: true,
                        });
                        aborted = true;
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
                if aborted {
                    // A stop ends only THIS turn, never the user's queued
                    // messages (claude parity, maintainer decision 2026-07-11):
                    // re-drive the buffered queue as the next turn, and leave
                    // in-flight steers tracked — codex is alive, so their
                    // post-abort error answers re-drive them through the same
                    // fresh-turn path. AFTER reset_turn_state, which would
                    // clobber the re-drive's turn_pending start window.
                    self.redrive_queued_after_abort(&mut step);
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
                    // Only THIS turn died — the queued messages behind it still
                    // deliver (claude parity): re-drive the buffer as the next
                    // turn (after reset_turn_state, which would clobber its
                    // turn_pending window); in-flight steers stay tracked and
                    // re-drive off their own error answers.
                    self.redrive_queued_after_abort(&mut step);
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
        // Defensive: a turn that ends without ever emitting turn/started must
        // not leave the start-window flag stuck true.
        self.turn_pending = false;
        // The turn ended on its own — the interrupt watchdog has nothing left
        // to abort (a real turn is never double-aborted by it).
        self.interrupt_grace = None;
        // Approvals only ever reference items of the current turn; keeping
        // these forever is unbounded growth over a long session.
        self.item_locations.clear();
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
                // turn and steer the buffered sends rather than erroring.
                if let Some(live) = parse_expected_turn_id(msg) {
                    self.turn_id = live;
                    self.turn_active = true;
                    self.flush_buffered(step);
                } else {
                    step.events.push(AgentEvent::Error {
                        message: format!("turn/start failed: {}", err["message"]),
                        fatal: false,
                    });
                    // Re-drive any buffered sends so they aren't stranded.
                    if !self.buffered_sends.is_empty() {
                        let (input, client_msg_id) = self.buffered_sends.remove(0);
                        self.redrive_as_fresh_turn(input, client_msg_id, step);
                    }
                }
            }
            (
                PendingRpc::Steer {
                    input,
                    client_msg_id,
                    retried: false,
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
                    // instead of dropping the already-echoed user message.
                    None if !self.turn_active => {
                        self.redrive_as_fresh_turn(input, client_msg_id, step)
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
                },
                Some(err),
            ) => {
                if !self.turn_active {
                    self.redrive_as_fresh_turn(input, client_msg_id, step);
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
                    self.current_mode = mode_id.clone();
                    step.events.push(AgentEvent::ModeChanged { mode_id });
                } else {
                    step.events.push(AgentEvent::Error {
                        message: format!("mode change failed: {}", err["message"]),
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
                self.current_mode = mode_id.clone();
                step.events.push(AgentEvent::ModeChanged { mode_id });
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
            Some("contextCompaction") if completed => {
                step.events.push(AgentEvent::Notice {
                    text: "context compacted".into(),
                });
            }
            // enteredReviewMode / exitedReviewMode / sleep / imageGeneration
            // etc. are tolerated silently (the official client renders
            // nothing for them either).
            _ => {}
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

    /// Teardown resolutions: every pending ask's reply route is this
    /// process's JSON-RPC channel, so the journal must not outlive it with
    /// the ask dangling (a replay would strand the card forever — see the
    /// harness's drain call in `run_driver`).
    /// After a user stop or a failed turn, the not-yet-delivered queue still
    /// delivers — the abort ends only the CURRENT turn (claude parity,
    /// maintainer decision 2026-07-11). Re-drive the FIRST buffered send as a
    /// fresh turn; the rest stay buffered and steer into it once its
    /// `turn/started` lands (the existing start-window path). In-flight steer
    /// RPCs are deliberately NOT touched: codex is alive on these paths, so it
    /// answers each steer — success resolves `sent` (it landed before the
    /// abort), and an error re-drives it as a fresh turn via `on_response`'s
    /// steer-error arms. Call AFTER `reset_turn_state` (which clears the
    /// `turn_pending` window this re-drive opens).
    fn redrive_queued_after_abort(&mut self, step: &mut DriverStep) {
        if self.buffered_sends.is_empty() {
            return;
        }
        let (input, client_msg_id) = self.buffered_sends.remove(0);
        self.redrive_as_fresh_turn(input, client_msg_id, step);
    }

    /// Drop every user message still queued behind the current turn — the
    /// not-yet-sent `buffered_sends` and any in-flight `Steer` RPC. This is
    /// the DEAD-OR-UNRESPONSIVE-agent resolution only (teardown, and the
    /// interrupt watchdog whose firing means codex stopped answering): with
    /// no live agent to deliver to or answer a steer, `dropped` is the honest
    /// terminal state — and removing the pending steers stops a late error
    /// from resurrecting a message against a gone process. A LIVE abort
    /// (turn/completed interrupted, turn/failed) re-drives instead — see
    /// `redrive_queued_after_abort`.
    fn drain_queued_sends(&mut self) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for (_input, client_msg_id) in std::mem::take(&mut self.buffered_sends) {
            events.push(AgentEvent::UserMessageUpdate {
                id: client_msg_id,
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
                let deadline = params["autoResolutionMs"]
                    .as_u64()
                    .map(|ms| std::time::Instant::now() + Duration::from_millis(ms));
                self.pending_questions
                    .insert(request_id.clone(), PendingQuestion { rpc_id, deadline });
                step.events.push(AgentEvent::QuestionRequest {
                    request_id,
                    questions,
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

    /// Steer every send buffered during the start window into the turn whose
    /// id just became known.
    fn flush_buffered(&mut self, step: &mut DriverStep) {
        for (input, client_msg_id) in std::mem::take(&mut self.buffered_sends) {
            self.emit_steer(input, client_msg_id, step);
        }
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
            // A turn/start is in flight but unidentified: buffer to steer
            // once turn/started lands, so we don't race a second turn/start
            // (rejected) and lose the echoed message.
            self.buffered_sends.push((input, client_msg_id));
        } else {
            self.emit_turn_start(input, client_msg_id, step);
        }
    }

    fn on_command(&mut self, cmd: AgentCommand) -> DriverStep {
        let mut step = DriverStep::default();
        match cmd {
            AgentCommand::Send { blocks } => {
                let text = crate::model::blocks_text(&blocks);
                // Images ride the input array as data URLs (the extension's
                // non-local-path form; local paths need a shared fs).
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
                // A steered/buffered send is not consumed by the agent yet:
                // it echoes queued and resolves via UserMessageUpdate when
                // the steer RPC answers (or the buffered flush's steer does).
                let queued = (self.turn_active && !self.turn_id.is_empty()) || self.turn_pending;
                step.events.push(AgentEvent::UserMessage {
                    text: text.clone(),
                    attachments,
                    id: Some(client_msg_id.clone()),
                    queued,
                });
                if self.turn_active && !self.turn_id.is_empty() {
                    // Type-through: inject into the RUNNING turn (steer).
                    self.emit_steer(input, client_msg_id, &mut step);
                } else if self.turn_pending {
                    // A turn/start is in flight but unidentified: buffer to
                    // steer once turn/started lands, so we don't race a second
                    // turn/start (rejected) and lose the echoed message.
                    self.buffered_sends.push((input, client_msg_id));
                } else {
                    // Only a turn-OPENING send anchors a checkpoint: rewind
                    // rolls back whole turns (thread/rollback numTurns), so a
                    // steered message — joining a running turn — is not a
                    // rewindable boundary. Emitted right after UserMessage
                    // (the journal-truncation cut relies on the adjacency).
                    let preceding = self.last_checkpoint.replace(client_msg_id.clone());
                    step.events.push(AgentEvent::Checkpoint {
                        user_message_id: client_msg_id.clone(),
                        preceding_uuid: preceding,
                    });
                    self.emit_turn_start(input, client_msg_id, &mut step);
                }
            }
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
                self.pending_effort = Some(effort_id);
            }
            AgentCommand::SetMode { mode_id } => {
                let fields = mode_wire_fields(
                    &mode_id,
                    self.pending_model.as_deref().or(self.model.as_deref()),
                    self.pending_effort.as_deref(),
                );
                if self.settings_update_unsupported {
                    self.mode_per_turn = Some(fields);
                    self.current_mode = mode_id.clone();
                    step.events.push(AgentEvent::ModeChanged { mode_id });
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
            // Pull back a still-queued message. A send still in the pre-steer
            // buffer is genuinely pulled back (it never reached codex). A send
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
                    self.buffered_sends.retain(|(_, cid)| cid != &id);
                    step.events.push(AgentEvent::UserMessageUpdate {
                        id,
                        state: UserMessageState::Cancelled,
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
    /// live abort's re-drive, or the teardown drain), so this is the defensive
    /// rescue for the one seam where it can't: a turn end that fires while a
    /// `turn/start` was still in flight (`turn_pending`) leaves
    /// `buffered_sends` stranded with no turn to steer them into. When the
    /// driver is idle with a stranded buffer past the grace, re-drive the
    /// oldest as a fresh turn (delivering it, resolving `sent`); the rest
    /// re-buffer and steer once it starts — the same path a `turn/start` error
    /// takes. A codex buffer was never sent, so we DELIVER it rather than
    /// declare it sent unseen.
    fn idle_flush(&mut self) -> DriverStep {
        let mut step = DriverStep::default();
        if self.turn_active || self.turn_pending || self.buffered_sends.is_empty() {
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
                let (input, client_msg_id) = self.buffered_sends.remove(0);
                self.redrive_as_fresh_turn(input, client_msg_id, &mut step);
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

/// Approval-mode → wire fields (the extension's exact table). `permissions`
/// is a profile id; sandboxPolicy is implied by it, so only these two ride.
fn mode_wire_fields(mode_id: &str, model: Option<&str>, effort: Option<&str>) -> Value {
    match mode_id {
        "read-only" => json!({
            "permissions": ":read-only",
            "approvalPolicy": "on-request",
            "collaborationMode": null,
        }),
        "full-access" => json!({
            "permissions": ":danger-full-access",
            "approvalPolicy": "never",
            "collaborationMode": null,
        }),
        // Plan mode is a collaboration mode, not a permission profile;
        // settings are snake_case inside (mined).
        "plan" => json!({
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
            "collaborationMode": null,
        }),
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
        CodexMapper::new("thr-1".into(), Vec::new(), None, 3)
    }

    fn active_turn(m: &mut CodexMapper) {
        m.on_frame(&json!({
            "method": "turn/started",
            "params": { "turn": { "id": "turn-A" } },
        }));
    }

    #[test]
    fn mid_turn_send_steers_with_expected_turn_id() {
        let mut m = mapper();
        active_turn(&mut m);
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "also do X".into(),
            }],
        });
        // The echo marks the message queued until the steer RPC answers.
        let msg_id = match &step.events[0] {
            AgentEvent::UserMessage { id, queued, .. } => {
                assert!(queued, "a steered send echoes queued");
                id.clone().expect("steered send carries a delivery id")
            }
            other => panic!("expected UserMessage, got {other:?}"),
        };
        assert_eq!(step.outbound[0]["method"], "turn/steer");
        assert_eq!(step.outbound[0]["params"]["expectedTurnId"], "turn-A");
        assert_eq!(
            step.outbound[0]["params"]["clientUserMessageId"],
            json!(msg_id)
        );

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
    fn send_during_start_window_buffers_then_steers_on_turn_started() {
        let mut m = mapper();
        // First send: no active turn → turn/start, and the start window opens.
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "one".into() }],
        });
        assert_eq!(step.outbound[0]["method"], "turn/start");
        assert!(m.turn_pending);
        // Second send arrives BEFORE turn/started: it must NOT fire a second
        // turn/start (which the server rejects, losing the message) — it buffers,
        // echoed queued until its flushed steer resolves.
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "two".into() }],
        });
        assert!(
            step.outbound.is_empty(),
            "second send buffers, no second turn/start: {:?}",
            step.outbound
        );
        let buffered_id = match &step.events[0] {
            AgentEvent::UserMessage { id, queued, .. } => {
                assert!(queued, "a buffered send echoes queued");
                id.clone().unwrap()
            }
            other => panic!("expected UserMessage, got {other:?}"),
        };
        // turn/started lands: the buffered send flushes as a steer into it.
        let step = m.on_frame(&json!({
            "method": "turn/started",
            "params": { "turn": { "id": "turn-Z" } },
        }));
        assert!(!m.turn_pending);
        let steer = step
            .outbound
            .iter()
            .find(|f| f["method"] == "turn/steer")
            .expect("buffered send steered");
        assert_eq!(steer["params"]["expectedTurnId"], "turn-Z");
        // …and the flushed steer's success resolves the buffered send.
        let rpc_id = steer["id"].as_u64().unwrap();
        let step = m.on_frame(&json!({ "id": rpc_id, "result": {} }));
        assert_eq!(
            step.events,
            vec![AgentEvent::UserMessageUpdate {
                id: buffered_id,
                state: UserMessageState::Sent,
            }]
        );
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
        let steer_id = step.outbound[0]["id"].as_u64().unwrap();
        let queued_id = match &step.events[0] {
            AgentEvent::UserMessage {
                id: Some(id),
                queued: true,
                ..
            } => id.clone(),
            other => panic!("expected a queued UserMessage, got {other:?}"),
        };
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

    /// Feature 2 (codex): a send still sitting in the pre-steer buffer can be
    /// cancelled — it resolves `Cancelled`, leaves the buffer, and never steers
    /// into the turn once it starts.
    #[test]
    fn cancel_queued_drops_a_buffered_send() {
        let mut m = mapper();
        // First send opens the turn (turn/start); the start window is now open.
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "A".into() }],
        });
        assert!(m.turn_pending);
        // Second send buffers during the window.
        let buffered = match &m
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
        // Cancel it: resolves Cancelled and empties the buffer.
        let step = m.on_command(AgentCommand::CancelQueued {
            id: buffered.clone(),
        });
        assert_eq!(
            step.events,
            vec![AgentEvent::UserMessageUpdate {
                id: buffered.clone(),
                state: UserMessageState::Cancelled,
            }]
        );
        assert!(m.buffered_sends.is_empty(), "the buffer is emptied");
        // turn/started now steers NOTHING — the cancelled send is gone.
        let step = m.on_frame(&json!({
            "method": "turn/started",
            "params": { "turn": { "id": "turn-Z" } },
        }));
        assert!(
            !step.outbound.iter().any(|o| o["method"] == "turn/steer"),
            "a cancelled buffered send never steers: {:?}",
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
        // It was steered (buffer is empty; the RPC is in flight, unanswered).
        assert!(m.buffered_sends.is_empty());
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

    /// Feature 1 (codex): a buffer stranded when a turn ended under a pending
    /// turn/start is rescued by the idle-flush — re-driven as a fresh turn
    /// (delivered, resolves `sent`), never left faded "queued". Symmetric to
    /// claude's idle-flush.
    #[test]
    fn idle_flush_redrives_a_stranded_buffer() {
        let mut m = mapper();
        // A turn/start in flight, with a send buffered behind it…
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "A".into() }],
        });
        assert!(m.turn_pending);
        let buffered = match &m
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
        // landed), stranding the buffer: reset_turn_state clears turn_pending
        // but leaves buffered_sends, and turn_active was never set.
        m.on_frame(&json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-A", "status": "completed" } },
        }));
        assert!(!m.turn_pending && !m.turn_active);
        assert_eq!(m.buffered_sends.len(), 1, "the buffer is stranded");

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
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if *id == buffered
            )),
            "the stranded buffer resolves sent: {:?}",
            step.events
        );
        assert!(
            step.outbound.iter().any(|o| o["method"] == "turn/start"),
            "and is delivered as a fresh turn: {:?}",
            step.outbound
        );
        assert!(m.buffered_sends.is_empty(), "the buffer drained");
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
                        status: crate::model::PlanStatus::Done
                    },
                    crate::model::PlanEntry {
                        content: "b".into(),
                        status: crate::model::PlanStatus::InProgress
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
    fn auto_resolution_deadline_skips_question_with_empty_answers() {
        let mut m = mapper();
        m.on_frame(&json!({
            "id": 93,
            "method": "item/tool/requestUserInput",
            "params": {
                "questions": [{ "id": "q1", "question": "Proceed?" }],
                "autoResolutionMs": 50,
            },
        }));
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
        m.on_frame(&json!({
            "id": 94,
            "method": "item/tool/requestUserInput",
            "params": { "questions": [{ "id": "q2", "question": "Still there?" }] },
        }));
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
}
