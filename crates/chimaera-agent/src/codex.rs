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
    SpawnSpec,
};
use crate::model::{
    cap_output, truncate_label, AgentCommand, AgentEvent, ChunkKind, Coalescer, ContentBlock,
    PermissionOption, PermissionOptionKind, ToolContent, ToolKind, ToolStatus, Usage,
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

    // Handshake covers initialize AND thread start/resume — a driver that
    // cannot open a thread is as dead as one that cannot speak at all.
    async fn handshake<'a>(
        &'a self,
        sink: &'a mut JsonlSink,
        stream: &'a mut JsonlStream,
        spec: &'a SpawnSpec,
    ) -> std::result::Result<Handshake<CodexMapper>, String> {
        let (thread_id, models) = codex_handshake(sink, stream, spec).await?;
        Ok(Handshake {
            mapper: CodexMapper::new(thread_id, models),
            initial: Vec::new(),
        })
    }
}

async fn codex_handshake(
    sink: &mut JsonlSink,
    stream: &mut JsonlStream,
    spec: &SpawnSpec,
) -> std::result::Result<(String, Vec<crate::model::ModelInfo>), String> {
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

    // The agent's own catalog beats any curated list; absence (older
    // binaries) is not a handshake failure.
    let mut models = Vec::new();
    if sink
        .send(&json!({
            "id": 2, "method": "model/list",
            "params": { "includeHidden": false, "cursor": null, "limit": 100 },
        }))
        .await
        .is_ok()
    {
        // Per-request cap so a binary that silently drops this unknown method
        // can't wedge the whole handshake until the 20s watchdog fires — the
        // model catalog is optional, so a timeout is just an empty catalog.
        let listed =
            tokio::time::timeout(Duration::from_secs(2), await_rpc_result(stream, 2)).await;
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
    Ok((thread_id, models))
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
}

/// Protocol → normalized-model translator for the app-server stream. Pure
/// state machine (no I/O), mirroring claude's `Mapper`.
struct CodexMapper {
    thread_id: String,
    models: Vec<crate::model::ModelInfo>,
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
    coalescer: Coalescer,
    /// agentMessage item ids that streamed deltas (skip their completed text).
    streamed: HashSet<String>,
    /// Outstanding server approval requests: our request_id →
    /// (JSON-RPC id, option_id → prebuilt decision payload).
    pending_approvals: HashMap<String, (Value, HashMap<String, Value>)>,
    /// Outstanding item/tool/requestUserInput prompts: request_id → rpc id.
    /// Answers go back as {answers:{questionId:{answers:[label,…]}}}.
    pending_questions: HashMap<String, Value>,
    /// One safety-buffering notice per turn (the frame repeats).
    safety_notified: bool,
    pending_rpcs: HashMap<u64, PendingRpc>,
    /// fileChange item id → touched paths (approval titles look them up).
    item_locations: HashMap<String, Vec<String>>,
    /// Live output bytes already streamed per item (caps the deltas; the
    /// completed item's aggregatedOutput replaces them authoritatively).
    out_streamed: HashMap<String, usize>,
    /// Latest cumulative token usage (turn/completed carries none here).
    last_usage: Usage,
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
    fn new(thread_id: String, models: Vec<crate::model::ModelInfo>) -> Self {
        Self {
            thread_id,
            models,
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
            coalescer: Coalescer::new(),
            streamed: HashSet::new(),
            pending_approvals: HashMap::new(),
            pending_questions: HashMap::new(),
            safety_notified: false,
            pending_rpcs: HashMap::new(),
            item_locations: HashMap::new(),
            out_streamed: HashMap::new(),
            last_usage: Usage::default(),
            next_id: 3, // 0/1/2 spent by the handshake (init, thread, model/list)
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
                    step.events
                        .push(AgentEvent::QuestionResolved { request_id });
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
                self.turn_active = false;
                let turn = &frame["params"]["turn"];
                let mut usage = self.last_usage.clone();
                usage.duration_ms = turn["durationMs"].as_u64().unwrap_or(0);
                let turn_id = turn["id"].as_str().unwrap_or(&self.turn_id).to_string();
                if turn["status"] == "interrupted" {
                    step.events.push(AgentEvent::TurnAborted {
                        turn_id,
                        reason: "interrupted".into(),
                    });
                } else {
                    step.events
                        .push(AgentEvent::TurnCompleted { turn_id, usage });
                }
                self.reset_turn_state();
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
            "turn/failed" => {
                if let Some(flushed) = self.coalescer.flush() {
                    step.events.push(flushed);
                }
                self.turn_active = false;
                step.events.push(AgentEvent::TurnAborted {
                    turn_id: self.turn_id.clone(),
                    reason: frame["params"]["error"]["message"]
                        .as_str()
                        .unwrap_or("turn failed")
                        .to_string(),
                });
                // A failed turn must reset per-turn state exactly like a
                // completed one — else the safety notice stays suppressed and
                // stream/location maps leak into the next turn.
                self.reset_turn_state();
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
        // Defensive: a turn that ends without ever emitting turn/started must
        // not leave the start-window flag stuck true.
        self.turn_pending = false;
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
                        self.emit_turn_start(input, client_msg_id, step);
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
                    None if !self.turn_active => self.emit_turn_start(input, client_msg_id, step),
                    None => {
                        step.events.push(AgentEvent::Error {
                            message: format!("steer failed: {msg}"),
                            fatal: false,
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
                    self.emit_turn_start(input, client_msg_id, step);
                } else {
                    step.events.push(AgentEvent::Error {
                        message: format!("steer failed: {}", err["message"]),
                        fatal: false,
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
            (PendingRpc::SettingsUpdate { mode_id, .. }, None) => {
                self.current_mode = mode_id.clone();
                step.events.push(AgentEvent::ModeChanged { mode_id });
            }
            (PendingRpc::AccountRead { report }, None) => {
                self.on_account(&frame["result"], report, step);
            }
            (PendingRpc::TurnStart | PendingRpc::Steer { .. } | PendingRpc::Interrupt, None) => {}
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
                self.pending_questions.insert(request_id.clone(), rpc_id);
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

    /// Route an input into the conversation whatever the turn state: steer
    /// the running turn, buffer through an unidentified start window, or open
    /// a fresh turn. Shared by Send and the decline-feedback delivery.
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
                step.events.push(AgentEvent::UserMessage {
                    text: text.clone(),
                    attachments,
                });
                self.dispatch_input(json!(input), &mut step);
            }
            AgentCommand::Permission {
                request_id,
                option_id,
                feedback,
                ..
            } => {
                let Some((rpc_id, decisions)) = self.pending_approvals.remove(&request_id) else {
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
                        });
                        self.dispatch_input(json!([{ "type": "text", "text": fb }]), &mut step);
                    }
                }
            }
            AgentCommand::Interrupt => {
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
                let Some(rpc_id) = self.pending_questions.remove(&request_id) else {
                    return step;
                };
                let mut map = serde_json::Map::new();
                for (qid, labels) in &answers {
                    map.insert(qid.clone(), json!({ "answers": labels }));
                }
                step.outbound
                    .push(json!({ "id": rpc_id, "result": { "answers": map } }));
                step.events
                    .push(AgentEvent::QuestionResolved { request_id });
            }
            // No codex equivalents on this surface.
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
        CodexMapper::new("thr-1".into(), Vec::new())
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
        assert_eq!(step.outbound[0]["method"], "turn/steer");
        assert_eq!(step.outbound[0]["params"]["expectedTurnId"], "turn-A");
        assert!(step.outbound[0]["params"]["clientUserMessageId"].is_string());
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
        // turn/start (which the server rejects, losing the message) — it buffers.
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "two".into() }],
        });
        assert!(
            step.outbound.is_empty(),
            "second send buffers, no second turn/start: {:?}",
            step.outbound
        );
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

        // Second failure surfaces instead of looping.
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
        assert_eq!(
            step.events[0],
            AgentEvent::UserMessage {
                text: "see".into(),
                attachments: 1
            }
        );
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
            AgentEvent::QuestionResolved { request_id } if request_id == "codex-92"
        ));
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
