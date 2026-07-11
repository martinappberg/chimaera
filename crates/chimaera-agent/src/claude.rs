//! Claude Code bidirectional stream-json client and driver.
//!
//! Wire facts below were live-verified against the pinned version (see
//! [`TESTED_CLAUDE_VERSION`]); the protocol is officially *semantically*
//! documented but the frames themselves are unversioned, so any behavior
//! change here must be re-verified with `just chat-smoke`:
//!
//! - `system/init` is NOT emitted at spawn — only once the first user message
//!   arrives. The spawn handshake is therefore a client-initiated `initialize`
//!   control request, which the CLI answers immediately (with the slash-command
//!   catalog) before any turn runs.
//! - `--session-id <uuid>` pins the native session id at spawn (verified:
//!   `system/init` echoes it), so the resume handle for the chat⇄terminal
//!   toggle exists before the first turn.
//! - Permission prompts only flow to the client as `can_use_tool` control
//!   requests when the CLI is started with `--permission-prompt-tool stdio`;
//!   without it the CLI resolves permissions alone (allowlist or deny).
//! - A `control_response` must carry `request_id` NESTED inside `response`
//!   (mirroring the CLI's own frames). A top-level `request_id` is silently
//!   ignored and the CLI hangs waiting for the answer.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use crate::driver::{
    run_driver, AgentAdapter, Driver, DriverExit, DriverIo, DriverStep, Handshake, Mapper,
    SpawnSpec, INTERRUPT_GRACE_TICKS,
};
use crate::model::{
    cap_head_tail, cap_output, truncate_label, AgentCommand, AgentEvent, ChunkKind, Coalescer,
    ContentBlock, ModeInfo, PermissionOption, PermissionOptionKind, PlanEntry, PlanStatus,
    SlashCommand, ToolContent, ToolKind, ToolStatus, Usage, UsageWindow, UserMessageState,
    DIFF_FILE_BUDGET, DIFF_TURN_BUDGET,
};
use crate::ndjson::{JsonlChild, JsonlSink, JsonlStream};

/// CLI version these frame shapes were verified against (2026-07-10).
pub const TESTED_CLAUDE_VERSION: &str = "2.1.206";

/// Arguments for a structured chat session, before server-side extras
/// (`--settings`, `--mcp-config`, `--session-id`) and login-shell wrapping.
pub fn chat_args(model: Option<&str>, resume: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(model) = model {
        args.push("--model".into());
        args.push(model.into());
    }
    if let Some(resume) = resume {
        args.push("--resume".into());
        args.push(resume.into());
    }
    args
}

// --- frame builders (shared by the probe client and the driver) -------------

fn initialize_request(id: &str) -> Value {
    json!({
        "type": "control_request",
        "request_id": id,
        "request": {
            "subtype": "initialize",
            "hooks": {},
            // Declared kinds MUST be answered (undeclared = the CLI parks
            // and decides per settings); we render native cards for both.
            "supportedDialogKinds": ["refusal_fallback_prompt", "fable_overage_consent_prompt"],
        },
    })
}

fn user_message(content: Value) -> Value {
    json!({
        "type": "user",
        "message": { "role": "user", "content": content },
    })
}

/// The driver's user frame carries a client-minted uuid — that uuid IS the
/// checkpoint key for rewind_files (the extension does exactly this:
/// `{type:"user", uuid: crypto.randomUUID(), session_id:"", …}`).
fn user_message_frame(uuid: &str, content: Value) -> Value {
    json!({
        "type": "user",
        "uuid": uuid,
        "session_id": "",
        "parent_tool_use_id": null,
        "message": { "role": "user", "content": content },
    })
}

fn control_request_frame(id: &str, request: Value) -> Value {
    json!({
        "type": "control_request",
        "request_id": id,
        "request": request,
    })
}

fn permission_response_frame(request_id: &Value, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        },
    })
}

/// The answer to a `can_use_tool` control request.
pub enum PermissionDecision {
    /// `updated_input` echoes (or edits) the tool input; the CLI applies it.
    Allow { updated_input: Value },
    /// `message` is shown to the model as the tool error, so it can react.
    /// `interrupt:true` ABORTS the turn (the bare-deny directive path);
    /// `interrupt:false` keeps it alive — the feedback-denial path, where
    /// the message carries the user's reason for the model to react to.
    Deny { message: String, interrupt: bool },
}

impl PermissionDecision {
    fn to_response(&self) -> Value {
        match self {
            PermissionDecision::Allow { updated_input } => {
                json!({ "behavior": "allow", "updatedInput": updated_input })
            }
            PermissionDecision::Deny { message, interrupt } => {
                json!({ "behavior": "deny", "message": message, "interrupt": interrupt })
            }
        }
    }
}

/// The official extension's deny directive: directive stops beat bare
/// rejections (the model otherwise retries with a different tool).
const DENY_DIRECTIVE: &str = "The user doesn't want to proceed with this tool use. \
    The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the \
    file). STOP what you are doing and wait for the user to tell you how to proceed.";

// --- probe client (used by the live smoke tests) ----------------------------

pub struct ClaudeChat {
    io: JsonlChild,
    next_control_id: u64,
}

impl ClaudeChat {
    pub fn spawn(
        bin: &str,
        cwd: &Path,
        model: Option<&str>,
        resume: Option<&str>,
        extra_args: &[String],
    ) -> Result<Self> {
        let mut args = chat_args(model, resume);
        args.extend(extra_args.iter().cloned());
        // The daemon pins what it verified against; the CLI must not swap
        // itself out underneath a live protocol session.
        let env = vec![
            ("DISABLE_AUTOUPDATER".to_string(), "1".to_string()),
            // Checkpoints: same opt-in the driver uses.
            (
                "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING".to_string(),
                "true".to_string(),
            ),
        ];
        let io = JsonlChild::spawn(bin, &args, cwd, &env, &[])?;
        Ok(Self {
            io,
            next_control_id: 0,
        })
    }

    /// The spawn handshake: send an `initialize` control request and wait for
    /// its response. Returns the response payload (slash-command catalog with
    /// descriptions — richer than `system/init`'s bare name list). A timeout
    /// or early exit here is the degrade-to-PTY signal.
    pub async fn initialize(&mut self, timeout: Duration) -> Result<Value> {
        let id = self.control_id();
        self.io.send(&initialize_request(&id)).await?;

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .context("timed out waiting for initialize response")?;
            match self.io.recv(remaining).await? {
                Some(value) => {
                    if value["type"] == "control_response"
                        && value["response"]["request_id"] == json!(id)
                    {
                        if value["response"]["subtype"] != "success" {
                            bail!("initialize rejected: {value}");
                        }
                        return Ok(value["response"]["response"].clone());
                    }
                    tracing::debug!(frame = %value["type"], "pre-initialize frame skipped");
                }
                None => bail!(
                    "claude exited during initialize; stderr: {}",
                    self.io.stderr_tail()
                ),
            }
        }
    }

    /// Send one user turn as content blocks (text, base64 images, documents).
    pub async fn send_user_blocks(&mut self, blocks: Value) -> Result<()> {
        self.io.send(&user_message(blocks)).await
    }

    pub async fn send_user_text(&mut self, text: &str) -> Result<()> {
        self.send_user_blocks(json!([{ "type": "text", "text": text }]))
            .await
    }

    /// Send a user turn with a client-minted uuid (the checkpoint key for
    /// `rewind_files`); returns that uuid.
    pub async fn send_user_text_with_uuid(&mut self, text: &str) -> Result<String> {
        let uuid = crate::model::fresh_uuid();
        self.io
            .send(&user_message_frame(
                &uuid,
                json!([{ "type": "text", "text": text }]),
            ))
            .await?;
        Ok(uuid)
    }

    /// Next raw protocol frame. `Ok(None)` = the CLI exited.
    pub async fn recv(&mut self, timeout: Duration) -> Result<Option<Value>> {
        self.io.recv(timeout).await
    }

    /// Answer a `can_use_tool` control request.
    pub async fn respond_permission(
        &mut self,
        request_id: &Value,
        decision: PermissionDecision,
    ) -> Result<()> {
        self.io
            .send(&permission_response_frame(
                request_id,
                decision.to_response(),
            ))
            .await
    }

    /// Client-initiated control request (interrupt, set_permission_mode,
    /// set_model). Fire-and-forget: the matching control_response surfaces
    /// through `recv` like any other frame.
    pub async fn send_control(&mut self, request: Value) -> Result<String> {
        let id = self.control_id();
        self.io.send(&control_request_frame(&id, request)).await?;
        Ok(id)
    }

    pub fn stderr_tail(&self) -> String {
        self.io.stderr_tail()
    }

    pub async fn shutdown(self, grace: Duration) -> Result<Option<i32>> {
        self.io.shutdown(grace).await
    }

    fn control_id(&mut self) -> String {
        self.next_control_id += 1;
        format!("ctl_{}", self.next_control_id)
    }
}

// --- structured driver -------------------------------------------------------

pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn kind(&self) -> &'static str {
        "claude"
    }

    fn spawn(&self, spec: SpawnSpec, io: DriverIo) -> Result<JoinHandle<DriverExit>> {
        anyhow::ensure!(!spec.argv.is_empty(), "empty argv");
        Ok(tokio::spawn(run_driver(ClaudeDriver, spec, io)))
    }
}

struct ClaudeDriver;

impl Driver for ClaudeDriver {
    type Mapper = ClaudeMapper;

    fn kind(&self) -> &'static str {
        "claude"
    }

    fn tested_version(&self) -> &'static str {
        TESTED_CLAUDE_VERSION
    }

    fn env_extra(&self) -> Vec<(String, String)> {
        vec![
            ("DISABLE_AUTOUPDATER".to_string(), "1".to_string()),
            // File checkpointing (rewind_files) is gated off under -p unless the
            // client opts in — the SDK's own enablement env (live: without it
            // every rewind answers "File rewinding is not enabled.").
            (
                "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING".to_string(),
                "true".to_string(),
            ),
        ]
    }

    async fn handshake<'a>(
        &'a self,
        sink: &'a mut JsonlSink,
        stream: &'a mut JsonlStream,
        spec: &'a SpawnSpec,
    ) -> std::result::Result<Handshake<ClaudeMapper>, String> {
        // The spawn handshake is a client-initiated `initialize` control
        // request; the CLI answers with the slash-command + model catalog.
        if let Err(err) = sink.send(&initialize_request("init")).await {
            return Err(format!("initialize write failed: {err:#}"));
        }
        let commands_catalog = await_initialize(stream).await?;

        let mut mapper = ClaudeMapper::new(
            spec.pinned_native_id.clone(),
            spec.agent_version.clone(),
            &commands_catalog,
        );
        let mut initial: Vec<DriverStep> = Vec::new();
        // Seed the effort/ultracode chips with the CLI's applied settings.
        let mut step = DriverStep::default();
        mapper.request_settings(&mut step);
        initial.push(step);
        // Parked prompts survive client swaps: the initialize response
        // redelivers unanswered permission/dialog requests — replay them
        // through the mapper so a reattach shows the cards, not a wedge.
        for key in [
            "pending_permission_requests",
            "pending_user_dialog_requests",
        ] {
            let Some(parked) = commands_catalog[key].as_array() else {
                continue;
            };
            for envelope in parked {
                let frame = json!({
                    "type": "control_request",
                    "request_id": envelope["request_id"],
                    "request": envelope["request"],
                });
                initial.push(mapper.on_frame(&frame));
            }
        }
        Ok(Handshake { mapper, initial })
    }
}

async fn await_initialize(stream: &mut JsonlStream) -> std::result::Result<Value, String> {
    loop {
        match stream.next().await {
            Ok(Some(frame)) => {
                if frame["type"] == "control_response" && frame["response"]["request_id"] == "init"
                {
                    if frame["response"]["subtype"] != "success" {
                        return Err(format!("initialize rejected: {frame}"));
                    }
                    return Ok(frame["response"]["response"].clone());
                }
            }
            Ok(None) => return Err("claude exited during handshake".to_string()),
            Err(err) => return Err(format!("{err:#}")),
        }
    }
}

/// A parked can_use_tool request: everything the eventual answer needs.
struct PendingPermission {
    /// Tool name — ExitPlanMode answers get plan-approval shaping.
    tool: String,
    /// Verbatim input (an allow response must echo it exactly).
    input: Value,
    /// permission_suggestions from the request — all types:
    /// addRules/addDirectories/setMode.
    suggestions: Vec<Value>,
}

enum PendingControl {
    SetMode(String),
    SetModel(String),
    Interrupt,
    SetThinking,
    ContextUsage,
    GetUsage,
    /// rewind_files round-trip; `dry_run` shapes the RewindResult.
    Rewind {
        user_message_id: String,
        dry_run: bool,
    },
    /// get_settings round-trip → EffortState (applied.{effort,ultracode}).
    Settings,
    /// apply_flag_settings acknowledged → re-read the truth.
    ApplyFlags,
    /// mcp_status round-trip (the /mcp panel).
    McpStatus,
    /// mcp_toggle / mcp_reconnect: on success, refresh the panel.
    McpMutate,
    /// generate_session_title round-trip (feeds the naming chain).
    Title,
    /// background_tasks ack ({backgrounded}) — Ctrl-B parity per tool call.
    Background,
    /// stop_task ack (subagent stop).
    StopTask,
}

/// Protocol → normalized-model translator. Pure state machine: consumes
/// frames/commands, yields events + outbound frames; owns no I/O, so it is
/// testable without a process. Implements the harness [`Mapper`] trait via a
/// thin delegator below.
struct ClaudeMapper {
    native_session_id: Option<String>,
    /// Launcher-probed `--version` line, echoed on every Init (journal truth,
    /// and the harness's drift-notice source). `None` = the probe failed.
    agent_version: Option<String>,
    model: Option<String>,
    current_mode: Option<String>,
    slash_commands: Vec<SlashCommand>,
    /// Account model catalog from the initialize response (value ids the
    /// set_model request accepts, with per-model effort levels).
    models: Vec<crate::model::ModelInfo>,
    coalescer: Coalescer,
    turn_n: u64,
    turn_active: bool,
    /// Message ids whose text/thinking streamed as deltas — their complete
    /// `assistant` frames must not be emitted again.
    streamed: HashSet<String>,
    current_stream_msg: Option<String>,
    /// tool_use_id → kind, to choose result rendering; cleared per turn.
    tool_kinds: HashMap<String, ToolKind>,
    /// Outstanding can_use_tool requests, keyed by request_id.
    pending_permissions: HashMap<String, PendingPermission>,
    /// Outstanding AskUserQuestion prompts: request_id → original input
    /// (echoed back inside updatedInput.questions with the answers).
    pending_questions: HashMap<String, Value>,
    /// Outstanding request_user_dialog prompts (option_id becomes the
    /// completed result string; "dismiss" cancels).
    pending_dialogs: HashMap<String, ()>,
    pending_controls: HashMap<String, PendingControl>,
    /// CLI→client control subtypes we don't handle and have already said so
    /// about — the notice fires once per subtype, not per frame.
    noticed_controls: HashSet<String>,
    /// Subagent task_id → transcript row id. task_id is NOT the Task tool's
    /// tool_use_id (mined: the extension treats it as an opaque key), so
    /// correlation is by description; unmatched tasks get their own row.
    task_rows: HashMap<String, String>,
    /// Open Task tool cards: tool_use_id → description, for that correlation.
    agent_tools: HashMap<String, String>,
    /// User messages the user queued while a turn was running, FIFO of
    /// `(client uuid, stdin content)`. They are HELD here — deliberately NOT
    /// written to the CLI mid-turn — and flushed to stdin all at once the
    /// moment the running turn's result lands (`on_result`), each resolving
    /// `sent` at that same boundary. Holding is what makes delivery
    /// deterministic: the CLI never sees them mid-turn, so it can't coalesce
    /// them into fewer results than messages, so no id can strand. It also
    /// matches the official client, whose queued messages wait for the current
    /// turn to finish rather than steering into it. The queue survives a
    /// stop/failed turn — an abort ends only the CURRENT turn, and the held
    /// messages flush right after it (pull one back with its ✕ instead). Only
    /// genuinely-undeliverable ends drop them `dropped`: process death
    /// (`drain_pending`) or a flush whose write never shipped (`flushing`).
    queued_sends: VecDeque<(String, Value)>,
    /// The uuids of the batch flushed on the most recent turn-end, awaiting
    /// confirmation that their stdin write shipped. `on_result` empties
    /// `queued_sends` into the flush step (writes + `sent` events) BEFORE
    /// `deliver` performs the write — so if that write fails (a child that
    /// wedged or died right after its result), the `sent` events are dropped
    /// and `queued_sends` is already empty, leaving the messages stranded
    /// "queued". Recording them here lets `drain_pending` (teardown) drop them.
    /// Cleared on the next frame: reaching `on_frame` again means `deliver`
    /// returned Ok, so the write shipped. A drop after a successful send is a
    /// harmless no-op (the reducer ignores an update for an already-resolved id).
    flushing: Vec<String>,
    /// We issued an interrupt control request and no result has landed
    /// since — the one deterministic "this abort was user-initiated" fact.
    /// The CLI's result string is free text and cannot carry that signal.
    interrupt_requested: bool,
    /// Interrupt watchdog: ticks remaining before we synthesize the abort the
    /// CLI never sent. Armed on `Interrupt`, counted down in `tick`, disarmed
    /// when a real result lands (a turn end) or a fresh turn opens. See
    /// `INTERRUPT_GRACE_TICKS`.
    interrupt_grace: Option<u32>,
    /// One title request per conversation, fired with the first user send
    /// (the extension's exact moment; description = the message text).
    title_requested: bool,
    /// The transcript-order anchor for conversation forks: the uuid of the
    /// last message seen (ours minted at send, or inbound assistant/user).
    last_msg_uuid: Option<String>,
    /// Last journaled thinking-token estimate (throttles the status frames
    /// to every ~256 tokens; reset per turn).
    thinking_emitted: u64,
    next_ctl: u64,
}

impl ClaudeMapper {
    fn new(
        pinned_native_id: Option<String>,
        agent_version: Option<String>,
        commands_catalog: &Value,
    ) -> Self {
        let slash_commands = commands_catalog["commands"]
            .as_array()
            .map(|cmds| {
                cmds.iter()
                    .filter_map(|c| {
                        Some(SlashCommand {
                            name: c["name"].as_str()?.to_string(),
                            description: c["description"].as_str().unwrap_or_default().to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The initialize response IS the account model catalog: `value` is
        // what set_model accepts; supportedEffortLevels feeds the effort
        // menu (absent = the model has no effort knob, e.g. haiku).
        let models = commands_catalog["models"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        Some(crate::model::ModelInfo {
                            id: m["value"].as_str()?.to_string(),
                            label: m["displayName"]
                                .as_str()
                                .unwrap_or(m["value"].as_str()?)
                                .to_string(),
                            description: m["description"].as_str().map(String::from),
                            resolved: m["resolvedModel"].as_str().map(String::from),
                            efforts: m["supportedEffortLevels"]
                                .as_array()
                                .map(|levels| {
                                    levels
                                        .iter()
                                        .filter_map(|l| l.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            default_effort: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            native_session_id: pinned_native_id,
            agent_version,
            model: None,
            current_mode: None,
            slash_commands,
            models,
            coalescer: Coalescer::new(),
            turn_n: 0,
            turn_active: false,
            streamed: HashSet::new(),
            current_stream_msg: None,
            tool_kinds: HashMap::new(),
            pending_permissions: HashMap::new(),
            pending_questions: HashMap::new(),
            pending_dialogs: HashMap::new(),
            pending_controls: HashMap::new(),
            noticed_controls: HashSet::new(),
            task_rows: HashMap::new(),
            agent_tools: HashMap::new(),
            queued_sends: VecDeque::new(),
            flushing: Vec::new(),
            interrupt_requested: false,
            interrupt_grace: None,
            title_requested: false,
            last_msg_uuid: None,
            thinking_emitted: 0,
            next_ctl: 0,
        }
    }

    fn init_event(&self) -> AgentEvent {
        AgentEvent::Init {
            native_session_id: self.native_session_id.clone().unwrap_or_default(),
            model: self.model.clone(),
            modes: claude_modes(),
            current_mode: self.current_mode.clone(),
            slash_commands: self.slash_commands.clone(),
            models: self.models.clone(),
            agent_version: self.agent_version.clone(),
        }
    }

    /// One get_settings round-trip → EffortState. Issued at spawn and after
    /// every apply_flag_settings, so the chips always show the CLI's truth.
    fn request_settings(&mut self, step: &mut DriverStep) {
        let id = self.ctl_id();
        self.pending_controls
            .insert(id.clone(), PendingControl::Settings);
        step.outbound.push(control_request_frame(
            &id,
            json!({ "subtype": "get_settings" }),
        ));
    }

    fn turn_id(&self) -> String {
        format!("t{}", self.turn_n)
    }

    /// Defensive turn boundary: content/tool frames must never stream into a
    /// turn that was never opened. A wrong queue assumption (the CLI's native
    /// queue after an abort is unverified) or a parked-prompt replay can leave
    /// `turn_active` false when a stream/assistant/tool frame arrives; opening
    /// a boundary here degrades that to a correct TurnStarted instead of a
    /// phantom turn with no start event.
    fn ensure_turn(&mut self, step: &mut DriverStep) {
        if !self.turn_active {
            self.turn_n += 1;
            self.turn_active = true;
            // A turn opening is a clean slate for the interrupt watchdog: an
            // interrupt armed against a previous (or idle) state must not abort
            // this fresh turn.
            self.interrupt_grace = None;
            step.events.push(AgentEvent::TurnStarted {
                turn_id: self.turn_id(),
            });
        }
    }

    /// One cheap `get_context_usage` round-trip after a turn end — the
    /// extension's cadence, kept honest for aborts too (an interrupted turn
    /// still consumed context). Only issued for a turn that was actually open.
    fn refresh_context_usage(&mut self, step: &mut DriverStep) {
        let id = self.ctl_id();
        self.pending_controls
            .insert(id.clone(), PendingControl::ContextUsage);
        step.outbound.push(control_request_frame(
            &id,
            json!({ "subtype": "get_context_usage" }),
        ));
    }

    fn flush(&mut self) -> Option<AgentEvent> {
        self.coalescer.flush()
    }

    fn on_frame(&mut self, frame: &Value) -> DriverStep {
        let mut step = DriverStep::default();
        // Reaching here means `deliver` returned Ok for the previous step, so
        // any batch flushed on the last result has shipped to the CLI — it no
        // longer needs the teardown drop-guard.
        self.flushing.clear();
        // Subagent-attributed transcript frames are hidden (the official
        // client does the same — the visible surface is the Task tool row);
        // only task-status frames pass, whoever they're attributed to.
        if frame["parent_tool_use_id"].is_string()
            && !frame["subtype"]
                .as_str()
                .is_some_and(|s| s.starts_with("task_"))
        {
            return step;
        }
        match frame["type"].as_str() {
            Some("system") => self.on_system(frame, &mut step),
            Some("stream_event") => self.on_stream_event(&frame["event"], &mut step),
            Some("assistant") => self.on_assistant(frame, &mut step),
            Some("user") => self.on_user_frame(frame, &mut step),
            Some("control_request") => self.on_control_request(frame, &mut step),
            Some("control_cancel_request") => {
                let id = frame["request_id"].as_str().unwrap_or_default().to_string();
                if self.pending_questions.remove(&id).is_some() {
                    step.events.push(AgentEvent::QuestionResolved {
                        request_id: id,
                        answers: Default::default(),
                    });
                } else if self.pending_permissions.remove(&id).is_some()
                    || self.pending_dialogs.remove(&id).is_some()
                {
                    step.events.push(AgentEvent::PermissionResolved {
                        request_id: id,
                        option_id: "cancelled".into(),
                    });
                }
            }
            Some("control_response") => self.on_control_response(frame, &mut step),
            Some("result") => self.on_result(frame, &mut step),
            Some("rate_limit_event") => self.on_rate_limit(frame, &mut step),
            Some("prompt_suggestion") => {
                if let Some(text) = frame["suggestion"].as_str() {
                    if !text.is_empty() {
                        step.events.push(AgentEvent::PromptSuggestion {
                            text: text.to_string(),
                        });
                    }
                }
            }
            // thinking_tokens etc. are telemetry the chat surface skips.
            _ => {}
        }
        step
    }

    fn on_system(&mut self, frame: &Value, step: &mut DriverStep) {
        match frame["subtype"].as_str() {
            Some("init") => {
                if let Some(id) = frame["session_id"].as_str() {
                    self.native_session_id = Some(id.to_string());
                }
                self.model = frame["model"].as_str().map(String::from);
                if let Some(mode) = frame["permissionMode"].as_str() {
                    self.current_mode = Some(mode.to_string());
                }
                step.events.push(self.init_event());
            }
            // Subagent status frames — the official surface is the Task tool
            // row ("Agent: …"), never a nested transcript. When the task id
            // matches a Task tool_use id the updates land on that card;
            // otherwise a standalone agent row is synthesized.
            Some("task_started") => {
                if frame["task_type"] != "local_agent" {
                    return;
                }
                let task_id = frame["task_id"].as_str().unwrap_or_default().to_string();
                if task_id.is_empty() {
                    return;
                }
                let description = frame["description"].as_str().unwrap_or("subagent");
                // Prefer landing progress on the Task tool card that spawned
                // this agent (same description string, not yet claimed).
                let claimed: std::collections::HashSet<&String> = self.task_rows.values().collect();
                let existing = self
                    .agent_tools
                    .iter()
                    .find(|(id, desc)| desc.as_str() == description && !claimed.contains(*id))
                    .map(|(id, _)| id.clone());
                match existing {
                    Some(id) => {
                        self.task_rows.insert(task_id, id);
                    }
                    None => {
                        let row = format!("task:{task_id}");
                        self.task_rows.insert(task_id, row.clone());
                        step.events.push(AgentEvent::ToolCall {
                            id: row,
                            kind: ToolKind::Agent,
                            title: format!("Agent: {description}"),
                            locations: Vec::new(),
                            status: ToolStatus::InProgress,
                        });
                    }
                }
            }
            Some("task_progress") => {
                let task_id = frame["task_id"].as_str().unwrap_or_default();
                let Some(row) = self.task_rows.get(task_id).cloned() else {
                    return;
                };
                let usage = &frame["usage"];
                let mut line = String::new();
                if let Some(s) = frame["summary"]
                    .as_str()
                    .or(frame["last_tool_name"].as_str())
                {
                    line.push_str(s);
                }
                if let Some(tools) = usage["tool_uses"].as_u64() {
                    if !line.is_empty() {
                        line.push_str(" · ");
                    }
                    line.push_str(&format!("{tools} tools"));
                }
                if let Some(tok) = usage["total_tokens"].as_u64() {
                    if !line.is_empty() {
                        line.push_str(" · ");
                    }
                    line.push_str(&format!("{tok} tokens"));
                }
                step.events.push(AgentEvent::ToolCallUpdate {
                    id: row,
                    status: ToolStatus::InProgress,
                    content: if line.is_empty() {
                        None
                    } else {
                        Some(ToolContent::Output {
                            text: line,
                            truncated: false,
                        })
                    },
                });
            }
            // Safety flagged the reply: the CLI switches model, withdraws
            // the flagged output (direction "retry"/"revert"), and retries.
            // `content` is the CLI's own user-facing banner.
            Some("model_refusal_fallback") => {
                let to = frame["fallback_model"].as_str().unwrap_or_default();
                if to.is_empty() {
                    return;
                }
                let direction = frame["direction"].as_str().unwrap_or_default();
                let retracted = frame["retracted_message_uuids"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty());
                self.model = Some(to.to_string());
                step.events.push(AgentEvent::ModelSwitched {
                    from: frame["original_model"].as_str().map(String::from),
                    to: to.to_string(),
                    reason: frame["api_refusal_category"]
                        .as_str()
                        .map(String::from)
                        .or(Some("safety flag".into())),
                    retract_current_turn: retracted
                        || direction == "retry"
                        || direction == "revert",
                });
                if let Some(banner) = frame["content"].as_str() {
                    if !banner.is_empty() {
                        step.events.push(AgentEvent::Notice {
                            text: banner.to_string(),
                        });
                    }
                }
                step.events.push(self.init_event());
            }
            // Fable needed usage credits; the CLI switched to the default
            // model (choice: consent/switch_default/cancelled).
            Some("model_consent_fallback") => {
                let to = frame["fallback_model"].as_str().unwrap_or_default();
                if to.is_empty() {
                    return;
                }
                self.model = Some(to.to_string());
                step.events.push(AgentEvent::ModelSwitched {
                    from: None,
                    to: to.to_string(),
                    reason: Some("Fable 5 requires usage credits".into()),
                    retract_current_turn: false,
                });
                step.events.push(self.init_event());
            }
            // Mode changes the CLI makes on its own (plan exits, applied
            // setMode suggestions) ride system/status.
            Some("status") => {
                if let Some(mode) = frame["permissionMode"].as_str() {
                    if self.current_mode.as_deref() != Some(mode) {
                        self.current_mode = Some(mode.to_string());
                        step.events.push(AgentEvent::ModeChanged {
                            mode_id: mode.to_string(),
                        });
                    }
                }
            }
            Some("compact_boundary") => {
                let pre = frame["compact_metadata"]["pre_tokens"].as_u64();
                step.events.push(AgentEvent::Notice {
                    text: match pre {
                        Some(pre) => format!("context compacted ({pre} tokens summarized)"),
                        None => "context compacted".to_string(),
                    },
                });
            }
            // Thinking progress rides its own system frames — present even
            // when the display is summarized and no thought text streams,
            // so the status row never claims "starting" through a think.
            Some("thinking_tokens") => {
                let tokens = frame["estimated_tokens"].as_u64().unwrap_or(0);
                if tokens >= self.thinking_emitted + 256 || self.thinking_emitted == 0 {
                    self.thinking_emitted = tokens.max(1);
                    step.events.push(AgentEvent::ThinkingTokens { tokens });
                }
            }
            Some("task_notification") => {
                let task_id = frame["task_id"].as_str().unwrap_or_default();
                if let Some(row) = self.task_rows.remove(task_id) {
                    // Only close synthesized rows: a real Task tool card gets
                    // its authoritative completion from the tool_result.
                    if row.starts_with("task:") {
                        step.events.push(AgentEvent::ToolCallUpdate {
                            id: row,
                            status: ToolStatus::Completed,
                            content: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// `{type:"rate_limit_event", rate_limit_info:{status, rateLimitType,
    /// utilization (0-1), resetsAt (epoch s), overageInUse}}`. status
    /// "allowed" means the window is fine (the header chip still updates);
    /// "rejected" means requests are being refused.
    fn on_rate_limit(&mut self, frame: &Value, step: &mut DriverStep) {
        let info = &frame["rate_limit_info"];
        if !info.is_object() {
            return;
        }
        let status = info["status"].as_str().unwrap_or_default();
        let label = match info["rateLimitType"].as_str() {
            Some("five_hour") => Some("session limit"),
            Some("seven_day") => Some("weekly limit"),
            Some("seven_day_opus") => Some("weekly Opus limit"),
            Some("seven_day_sonnet") => Some("weekly Sonnet limit"),
            Some("seven_day_overage_included") => Some("Fable 5 limit"),
            Some("overage") => Some("usage credit limit"),
            other => other,
        };
        step.events.push(AgentEvent::RateLimit {
            utilization: (info["utilization"].as_f64().unwrap_or(0.0) * 100.0).clamp(0.0, 100.0),
            resets_at: info["resetsAt"].as_u64().map(|secs| secs.to_string()),
            label: label.map(String::from),
            limit_reached: status == "rejected",
        });
    }

    /// `user` frames: tool results, plus the message uuid that moves the
    /// fork anchor (the transcript position rewinds resume AT).
    fn on_user_frame(&mut self, frame: &Value, step: &mut DriverStep) {
        if let Some(uuid) = frame["uuid"].as_str() {
            self.last_msg_uuid = Some(uuid.to_string());
        }
        self.on_tool_results(&frame["message"], step);
    }

    fn on_stream_event(&mut self, event: &Value, step: &mut DriverStep) {
        match event["type"].as_str() {
            Some("message_start") => {
                self.current_stream_msg = event["message"]["id"].as_str().map(String::from);
            }
            Some("content_block_delta") => {
                self.ensure_turn(step);
                let turn = self.turn_id();
                let (kind, text) = match event["delta"]["type"].as_str() {
                    Some("text_delta") => (ChunkKind::Message, event["delta"]["text"].as_str()),
                    Some("thinking_delta") => {
                        (ChunkKind::Thought, event["delta"]["thinking"].as_str())
                    }
                    _ => return,
                };
                let Some(text) = text else { return };
                if let Some(id) = &self.current_stream_msg {
                    self.streamed.insert(id.clone());
                }
                if let Some(flushed) = self.coalescer.push(&turn, kind, text) {
                    step.events.push(flushed);
                }
            }
            _ => {}
        }
    }

    fn on_assistant(&mut self, frame: &Value, step: &mut DriverStep) {
        self.ensure_turn(step);
        if let Some(uuid) = frame["uuid"].as_str() {
            self.last_msg_uuid = Some(uuid.to_string());
        }
        // `supersedes` uuids: this message REPLACES earlier output (refusal
        // retries resend on the fallback model) — the client drops the
        // turn's trailing prose before this content lands.
        if frame["supersedes"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            if let Some(flushed) = self.coalescer.flush() {
                step.events.push(flushed);
            }
            step.events.push(AgentEvent::MessagesSuperseded);
        }
        let message = &frame["message"];
        // Every assistant message names the model that ACTUALLY served it —
        // the chip follows any auto-switch (safety reroute, capacity
        // fallback) the moment it happens, not at the next turn's init.
        if let Some(served) = message["model"].as_str() {
            if !served.is_empty() && self.model.as_deref() != Some(served) {
                self.model = Some(served.to_string());
                step.events.push(self.init_event());
            }
        }
        let msg_id = message["id"].as_str().unwrap_or_default();
        let streamed = self.streamed.contains(msg_id);
        let Some(blocks) = message["content"].as_array() else {
            return;
        };
        for block in blocks {
            match block["type"].as_str() {
                Some("text") if !streamed => {
                    if let Some(text) = block["text"].as_str() {
                        if let Some(flushed) =
                            self.coalescer
                                .push(&self.turn_id().clone(), ChunkKind::Message, text)
                        {
                            step.events.push(flushed);
                        }
                    }
                }
                Some("thinking") if !streamed => {
                    if let Some(text) = block["thinking"].as_str() {
                        if let Some(flushed) =
                            self.coalescer
                                .push(&self.turn_id().clone(), ChunkKind::Thought, text)
                        {
                            step.events.push(flushed);
                        }
                    }
                }
                Some("tool_use") => {
                    // Order matters in the transcript: prose before the tool.
                    if let Some(flushed) = self.coalescer.flush() {
                        step.events.push(flushed);
                    }
                    self.on_tool_use(block, step);
                }
                _ => {}
            }
        }
    }

    fn on_tool_use(&mut self, block: &Value, step: &mut DriverStep) {
        let id = block["id"].as_str().unwrap_or_default().to_string();
        let name = block["name"].as_str().unwrap_or_default();
        let input = &block["input"];

        // The todo list is a first-class plan panel, not a tool card.
        if name == "TodoWrite" {
            if let Some(todos) = input["todos"].as_array() {
                let entries = todos
                    .iter()
                    .filter_map(|t| {
                        Some(PlanEntry {
                            content: t["content"].as_str()?.to_string(),
                            status: match t["status"].as_str() {
                                Some("in_progress") => PlanStatus::InProgress,
                                Some("completed") => PlanStatus::Done,
                                _ => PlanStatus::Todo,
                            },
                        })
                    })
                    .collect();
                step.events.push(AgentEvent::Plan { entries });
            }
            self.tool_kinds.insert(id, ToolKind::Think);
            return;
        }

        // Questions are a first-class card (QuestionRequest via the
        // can_use_tool path), not a tool row: a bare "AskUserQuestion" card
        // with a stuck spinner next to the real question card is noise. Its
        // tool_result still resolves via tool_kinds like TodoWrite's does
        // (the client ignores updates for rows it never rendered).
        if name == "AskUserQuestion" {
            self.tool_kinds.insert(id, ToolKind::Other);
            return;
        }

        let kind = tool_kind(name);
        self.tool_kinds.insert(id.clone(), kind);
        if name == "Task" {
            if let Some(desc) = input["description"].as_str() {
                self.agent_tools.insert(id.clone(), desc.to_string());
            }
        }
        step.events.push(AgentEvent::ToolCall {
            id: id.clone(),
            kind,
            title: tool_title(name, input),
            locations: tool_locations(input),
            status: ToolStatus::InProgress,
        });
        if let Some(diff) = edit_diff_content(name, input) {
            step.events.push(AgentEvent::ToolCallUpdate {
                id,
                status: ToolStatus::InProgress,
                content: Some(diff),
            });
        }
    }

    fn on_tool_results(&mut self, message: &Value, step: &mut DriverStep) {
        let Some(blocks) = message["content"].as_array() else {
            return;
        };
        for block in blocks {
            if block["type"] != "tool_result" {
                continue;
            }
            let id = block["tool_use_id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let failed = block["is_error"] == json!(true);
            let kind = self.tool_kinds.get(&id).copied();
            // Edit-family cards already carry their diff; the "File updated"
            // acknowledgement adds nothing.
            let content = if matches!(kind, Some(ToolKind::Edit)) && !failed {
                None
            } else {
                let text = tool_result_text(block);
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

    fn on_control_request(&mut self, frame: &Value, step: &mut DriverStep) {
        let request = &frame["request"];
        let request_id = frame["request_id"].as_str().unwrap_or_default().to_string();
        if request["subtype"] == "request_user_dialog" {
            self.on_user_dialog(&request_id, request, step);
            return;
        }
        if request["subtype"] != "can_use_tool" {
            // Deliberately NOT answered: the CLI parks an unanswered control
            // request until its own deadline (or another attached client)
            // settles it, and an error reply here could break flows that work
            // via that fallback (mined subtypes: hook_callback, mcp_message,
            // elicitation, oauth refreshes). But never park SILENTLY — the
            // agent's later "I was blocked" prose must not be the only trace
            // the ask existed. One notice per subtype, not per frame.
            let subtype = request["subtype"].as_str().unwrap_or("unknown");
            // `subtype` is agent-influenced; bound the once-per-subtype dedupe
            // set (real streams carry a handful of distinct control subtypes)
            // so a buggy/hostile stream can't grow it without end.
            const MAX_NOTICED_CONTROLS: usize = 64;
            let fresh = self.noticed_controls.len() < MAX_NOTICED_CONTROLS
                && self.noticed_controls.insert(subtype.to_string());
            if fresh {
                step.events.push(AgentEvent::Notice {
                    text: format!(
                        "claude sent a request chimaera doesn't handle yet ({subtype}) — \
                         the agent may wait on it until it times out"
                    ),
                });
            }
            return;
        }
        // AskUserQuestion rides the permission path but is a QUESTION, not a
        // permission: option buttons, answered via updatedInput.answers
        // keyed by the question TEXT with ", "-joined labels (mined).
        if request["tool_name"] == "AskUserQuestion" {
            let input = request["input"].clone();
            let questions = input["questions"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|q| {
                            Some(crate::model::Question {
                                id: q["question"].as_str()?.to_string(),
                                header: q["header"].as_str().unwrap_or_default().to_string(),
                                question: q["question"].as_str()?.to_string(),
                                options: crate::model::question_options(q),
                                multi_select: q["multiSelect"] == json!(true),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !questions.is_empty() {
                self.pending_questions.insert(request_id.clone(), input);
                step.events.push(AgentEvent::QuestionRequest {
                    request_id,
                    questions,
                });
                return;
            }
        }
        let tool_use_id = request["tool_use_id"].as_str().map(String::from);
        let tool = request["tool_name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let input = request["input"].clone();
        // All suggestion types ride "always allow": addRules,
        // addDirectories, setMode (the extension sends the full set back).
        let suggestions = request["permission_suggestions"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        // ExitPlanMode is a plan approval, not a tool permission: the card
        // shows the plan itself with the official extension's three answers.
        // "auto-accept edits" resolves as allow + a set_permission_mode
        // follow-up; "keep planning" is the deny path (comments ride the
        // feedback-denial message).
        let (options, plan) = if tool == "ExitPlanMode" {
            let plan = input["plan"].as_str().map(|p| cap_output(p).0);
            let options = vec![
                PermissionOption {
                    id: "allow_accept_edits".into(),
                    label: "Yes, and auto-accept edits".into(),
                    kind: PermissionOptionKind::AllowAlways,
                },
                PermissionOption {
                    id: "allow_once".into(),
                    label: "Yes, manually approve".into(),
                    kind: PermissionOptionKind::AllowOnce,
                },
                PermissionOption {
                    id: "reject_once".into(),
                    label: "No, keep planning".into(),
                    kind: PermissionOptionKind::RejectOnce,
                },
            ];
            (options, plan)
        } else {
            let mut options = vec![PermissionOption {
                id: "allow_once".into(),
                label: "Allow".into(),
                kind: PermissionOptionKind::AllowOnce,
            }];
            if !suggestions.is_empty() {
                options.push(PermissionOption {
                    id: "allow_always".into(),
                    label: "Always allow".into(),
                    kind: PermissionOptionKind::AllowAlways,
                });
            }
            options.push(PermissionOption {
                id: "reject_once".into(),
                label: "Deny".into(),
                kind: PermissionOptionKind::RejectOnce,
            });
            (options, None)
        };

        let title = request["display_name"]
            .as_str()
            .or(request["tool_name"].as_str())
            .unwrap_or("tool")
            .to_string();
        // The verbatim input stays in pending_permissions (the allow response
        // must echo updatedInput exactly); the PREVIEW is capped so a
        // multi-megabyte Write `content` can't bloat the journaled/replayed
        // event (caps-at-event-construction).
        let mut input_preview = cap_preview(&input);
        if plan.is_some() {
            // The plan already rides its own (capped) field — carrying it in
            // the preview too would double-store it in the journal.
            if let Some(obj) = input_preview.as_object_mut() {
                obj.remove("plan");
            }
        }
        self.pending_permissions.insert(
            request_id.clone(),
            PendingPermission {
                tool,
                input,
                suggestions,
            },
        );
        step.events.push(AgentEvent::PermissionRequest {
            request_id,
            tool_call_id: tool_use_id,
            title,
            options,
            input_preview,
            plan,
        });
    }

    /// request_user_dialog → a decision card (mined kinds + exact result
    /// strings; anything else answers cancelled so nothing parks).
    fn on_user_dialog(&mut self, request_id: &str, request: &Value, step: &mut DriverStep) {
        let payload = &request["payload"];
        let opt = |id: &str, label: String, kind: PermissionOptionKind| PermissionOption {
            id: id.into(),
            label,
            kind,
        };
        let (title, options) = match request["dialog_kind"].as_str() {
            Some("fable_overage_consent_prompt") => (
                "Fable 5 requires usage credits".to_string(),
                vec![
                    opt(
                        "consent",
                        "Continue on Fable 5 with usage credits".into(),
                        PermissionOptionKind::AllowOnce,
                    ),
                    opt(
                        "switch_default",
                        "Switch to the default model".into(),
                        PermissionOptionKind::AllowAlways,
                    ),
                    opt(
                        "dismiss",
                        "Dismiss".into(),
                        PermissionOptionKind::RejectOnce,
                    ),
                ],
            ),
            Some("refusal_fallback_prompt") => {
                let fallback = payload["fallbackModel"]
                    .as_str()
                    .unwrap_or("a fallback model");
                let original = payload["originalModel"]
                    .as_str()
                    .unwrap_or("the original model");
                (
                    "Safety systems flagged this exchange".to_string(),
                    vec![
                        opt(
                            "retry_fallback",
                            format!("Switch to {fallback}"),
                            PermissionOptionKind::AllowOnce,
                        ),
                        opt(
                            "edit_prompt",
                            format!("Edit prompt and retry with {original}"),
                            PermissionOptionKind::AllowAlways,
                        ),
                        opt(
                            "dismiss",
                            "Dismiss".into(),
                            PermissionOptionKind::RejectOnce,
                        ),
                    ],
                )
            }
            other => {
                // Unknown kind: answer cancelled immediately — declared kinds
                // must never park — but say so visibly. A silent cancel left
                // the agent narrating "I'm blocked" with no trace of WHAT
                // asked (the "harness is blocking" mystery).
                let kind = other.unwrap_or("unknown");
                tracing::debug!(kind, "unsupported user dialog kind cancelled");
                step.outbound.push(permission_response_frame(
                    &json!(request_id),
                    json!({ "behavior": "cancelled" }),
                ));
                step.events.push(AgentEvent::Notice {
                    text: format!(
                        "claude asked something chimaera doesn't support yet ({kind}) — \
                         dismissed automatically"
                    ),
                });
                return;
            }
        };
        self.pending_dialogs.insert(request_id.to_string(), ());
        step.events.push(AgentEvent::PermissionRequest {
            request_id: request_id.to_string(),
            tool_call_id: request["tool_use_id"].as_str().map(String::from),
            title,
            options,
            input_preview: payload.clone(),
            plan: None,
        });
    }

    fn on_control_response(&mut self, frame: &Value, step: &mut DriverStep) {
        let id = frame["response"]["request_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let Some(pending) = self.pending_controls.remove(&id) else {
            return;
        };
        if frame["response"]["subtype"] != "success" {
            // A failed rewind must resolve the client's confirm flow, not
            // just leave an error notice.
            if let PendingControl::Rewind {
                user_message_id, ..
            } = pending
            {
                step.events.push(AgentEvent::RewindResult {
                    user_message_id,
                    can_rewind: false,
                    files_changed: Vec::new(),
                    applied: false,
                    error: Some(format!("{}", frame["response"]["error"])),
                });
                return;
            }
            step.events.push(AgentEvent::Error {
                message: format!("control request failed: {}", frame["response"]),
                fatal: false,
            });
            return;
        }
        let payload = &frame["response"]["response"];
        match pending {
            PendingControl::SetMode(mode) => {
                self.current_mode = Some(mode.clone());
                step.events.push(AgentEvent::ModeChanged { mode_id: mode });
            }
            PendingControl::SetModel(model) => {
                self.model = Some(model);
                step.events.push(self.init_event());
            }
            PendingControl::Interrupt | PendingControl::SetThinking => {}
            PendingControl::ContextUsage => {
                let usage = if payload.get("usage").is_some() {
                    &payload["usage"]
                } else {
                    payload
                };
                if let (Some(total), Some(max)) = (
                    usage["totalTokens"].as_u64(),
                    usage["rawMaxTokens"].as_u64(),
                ) {
                    step.events.push(AgentEvent::ContextUsage {
                        total_tokens: total,
                        max_tokens: max,
                        percentage: usage["percentage"]
                            .as_f64()
                            .unwrap_or_else(|| total as f64 / max.max(1) as f64 * 100.0),
                    });
                }
            }
            PendingControl::GetUsage => {
                let limits = &payload["rate_limits"];
                let mut windows = Vec::new();
                for (key, label) in [
                    ("five_hour", "session (5h)"),
                    ("seven_day", "weekly"),
                    ("seven_day_sonnet", "weekly sonnet"),
                    ("seven_day_opus", "weekly opus"),
                    ("extra_usage", "extra usage"),
                ] {
                    if let Some(u) = limits[key]["utilization"].as_f64() {
                        windows.push(UsageWindow {
                            label: label.to_string(),
                            utilization: u,
                            resets_at: limits[key]["resets_at"].as_str().map(String::from),
                        });
                    }
                }
                if let Some(scoped) = limits["model_scoped"].as_array() {
                    for m in scoped {
                        if let (Some(name), Some(u)) =
                            (m["display_name"].as_str(), m["utilization"].as_f64())
                        {
                            windows.push(UsageWindow {
                                label: format!("weekly {name}"),
                                utilization: u,
                                resets_at: m["resets_at"].as_str().map(String::from),
                            });
                        }
                    }
                }
                step.events.push(AgentEvent::UsageReport { windows });
            }
            PendingControl::Rewind {
                user_message_id,
                dry_run,
            } => {
                let can_rewind = payload["canRewind"].as_bool().unwrap_or(false);
                let files_changed = payload["filesChanged"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|f| f.as_str().or(f["path"].as_str()).map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                step.events.push(AgentEvent::RewindResult {
                    user_message_id,
                    can_rewind,
                    files_changed,
                    applied: !dry_run && can_rewind,
                    error: None,
                });
            }
            PendingControl::McpStatus => {
                let servers = payload["mcpServers"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|s| {
                                Some(crate::model::McpServerInfo {
                                    name: s["name"].as_str()?.to_string(),
                                    status: s["status"].as_str().unwrap_or("unknown").to_string(),
                                    tools: s["tools"]
                                        .as_array()
                                        .map(|t| t.len() as u32)
                                        .or(s["toolCount"].as_u64().map(|n| n as u32))
                                        .unwrap_or(0),
                                    error: s["error"].as_str().map(String::from),
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                step.events.push(AgentEvent::McpServers { servers });
            }
            PendingControl::McpMutate => {
                // The panel re-reads after every mutation so the user sees
                // the server's real post-toggle state, not an optimistic one.
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::McpStatus);
                step.outbound.push(control_request_frame(
                    &id,
                    json!({ "subtype": "mcp_status" }),
                ));
            }
            PendingControl::Title => {
                if let Some(title) = payload["title"].as_str() {
                    if !title.is_empty() {
                        step.events.push(AgentEvent::SessionTitle {
                            title: title.to_string(),
                        });
                    }
                }
            }
            PendingControl::Settings => {
                let applied = &payload["applied"];
                step.events.push(AgentEvent::EffortState {
                    effort: applied["effort"].as_str().map(String::from),
                    ultracode: applied["ultracode"] == json!(true),
                });
            }
            PendingControl::Background => {
                // Missing flag ⇒ backgrounded (the extension's read).
                if payload["backgrounded"] == json!(false) {
                    step.events.push(AgentEvent::Notice {
                        text: "the tool could not be moved to the background".into(),
                    });
                } else {
                    step.events.push(AgentEvent::Notice {
                        text: "tool moved to the background".into(),
                    });
                }
            }
            PendingControl::StopTask => {}
            PendingControl::ApplyFlags => {
                // The apply is fire-and-ack; the truth comes from re-reading.
                self.request_settings(step);
            }
        }
    }

    fn on_result(&mut self, frame: &Value, step: &mut DriverStep) {
        if let Some(flushed) = self.coalescer.flush() {
            step.events.push(flushed);
        }
        let turn_id = self.turn_id();
        // Capture BEFORE clearing. A result can arrive with NO turn open: the
        // real CLI coalesces rapid queued sends (live-verified: 3 sends → 2
        // results), so a queued message can resolve `sent` off a turn whose
        // content never streamed — a bare result. `was_active` gates the
        // turn-END events so that bare result never emits a phantom
        // TurnCompleted/TurnAborted. A genuine turn always streamed content,
        // so `ensure_turn` fired and this is true — the normal case is
        // unchanged.
        let was_active = self.turn_active;
        self.turn_active = false;
        self.tool_kinds.clear();
        self.streamed.clear();
        // Task maps live per turn (the extension wipes its map on result).
        self.task_rows.clear();
        self.agent_tools.clear();
        self.thinking_emitted = 0;
        // A real result is the turn end — disarm the interrupt watchdog (the
        // interrupt's own is_error result lands here too, so a genuine turn is
        // never double-aborted by the watchdog).
        self.interrupt_grace = None;

        // Consumed at EVERY result, both branches: an interrupt whose turn
        // ended before the control request landed must not mislabel the
        // NEXT turn's genuine failure as a quiet stop.
        let interrupted = std::mem::take(&mut self.interrupt_requested);

        if frame["is_error"] == json!(true) {
            // Only an OPEN turn can be aborted — a bare is_error result with
            // no turn open must not synthesize a phantom abort (nor a context
            // refresh for a turn that never ran here).
            if was_active {
                step.events.push(AgentEvent::TurnAborted {
                    turn_id,
                    reason: frame["result"]
                        .as_str()
                        .unwrap_or(if interrupted {
                            "interrupted"
                        } else {
                            "turn failed"
                        })
                        .to_string(),
                    interrupted,
                });
                self.refresh_context_usage(step);
            }
        } else if was_active {
            // Only an OPEN turn completes — see `was_active`.
            let usage = &frame["usage"];
            step.events.push(AgentEvent::TurnCompleted {
                turn_id,
                usage: Usage {
                    cost_usd: frame["total_cost_usd"].as_f64(),
                    input_tokens: usage["input_tokens"].as_u64().unwrap_or(0)
                        + usage["cache_read_input_tokens"].as_u64().unwrap_or(0)
                        + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0),
                    output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
                    total_tokens: 0,
                    duration_ms: frame["duration_ms"].as_u64().unwrap_or(0),
                    context_window: None,
                },
            });
            self.refresh_context_usage(step);
        }
        // The running turn has ended — however it ended — so NOW flush
        // everything the user queued behind it. A stop/failure ends only the
        // CURRENT turn (maintainer decision 2026-07-11): the held messages were
        // never part of it (never written), so they still deliver, in full,
        // right after the abort — Stop is not "discard my queue" (the ✕ on each
        // bubble is). Each held message is written to the CLI here (the first
        // moment it is idle for them) and resolves `sent` in the same step.
        // Two things make this deterministic where the old per-result FIFO pop
        // was a guess: (1) we never dumped them mid-turn, so the CLI cannot
        // coalesce them into fewer results than messages and strand an id; and
        // (2) marking `sent` is tied to OUR write, not to counting the CLI's
        // results. The response turn's boundary still opens LAZILY — a synthetic
        // TurnStarted here (with no matching result if the CLI coalesces the
        // flushed batch) was the old "stuck running" bug; `ensure_turn` fires
        // instead when the first real response frame streams. The `sent` events
        // land after this turn's end event, so each bubble enters the
        // transcript AFTER the finished turn, never spliced into its output.
        for (id, content) in std::mem::take(&mut self.queued_sends) {
            step.outbound.push(user_message_frame(&id, content));
            // Stage for the teardown drop-guard until the write is confirmed
            // shipped (cleared on the next frame). If `deliver`'s write fails,
            // the `sent` event below never leaves and `drain_pending` drops it.
            self.flushing.push(id.clone());
            step.events.push(AgentEvent::UserMessageUpdate {
                id,
                state: UserMessageState::Sent,
            });
        }
    }

    fn on_command(&mut self, cmd: AgentCommand) -> DriverStep {
        let mut step = DriverStep::default();
        match cmd {
            AgentCommand::Send { blocks } => {
                let text = crate::model::blocks_text(&blocks);
                let attachments = blocks
                    .iter()
                    .filter(|b| matches!(b, ContentBlock::Image { .. }))
                    .count() as u32;
                let content: Vec<Value> = blocks
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
                        ContentBlock::Image { media_type, data } => json!({
                            "type": "image",
                            "source": { "type": "base64", "media_type": media_type, "data": data },
                        }),
                    })
                    .collect();
                let uuid = crate::model::fresh_uuid();
                let preceding = self.last_msg_uuid.replace(uuid.clone());
                step.events.push(AgentEvent::UserMessage {
                    text: text.clone(),
                    attachments,
                    id: Some(uuid.clone()),
                    queued: self.turn_active,
                });
                step.events.push(AgentEvent::Checkpoint {
                    user_message_id: uuid.clone(),
                    preceding_uuid: preceding,
                });
                if self.turn_active {
                    // A turn is running: HOLD this message (do NOT write it to
                    // the CLI now). It flushes to stdin when the running turn's
                    // result lands, which also resolves it `sent`. Holding — vs
                    // the official client's own mid-turn queue — is what keeps
                    // the CLI from coalescing rapid sends into fewer results and
                    // stranding one, and it keeps the delivered bubble from
                    // splicing into the still-streaming turn.
                    self.queued_sends.push_back((uuid.clone(), json!(content)));
                } else {
                    // Idle: this send opens a fresh turn and goes to the CLI
                    // immediately. An interrupt sent while idle (benign no-op on
                    // the CLI) must not relabel this fresh turn's genuine failure
                    // as a quiet stop, nor let its armed watchdog abort it.
                    self.interrupt_requested = false;
                    self.interrupt_grace = None;
                    self.turn_n += 1;
                    self.turn_active = true;
                    step.events.push(AgentEvent::TurnStarted {
                        turn_id: self.turn_id(),
                    });
                    step.outbound
                        .push(user_message_frame(&uuid, json!(content)));
                }
                // Name the conversation off the first message (the
                // extension's moment and shape: description = message text).
                if !self.title_requested && !text.trim().is_empty() {
                    self.title_requested = true;
                    let id = self.ctl_id();
                    self.pending_controls
                        .insert(id.clone(), PendingControl::Title);
                    step.outbound.push(control_request_frame(
                        &id,
                        json!({
                            "subtype": "generate_session_title",
                            "description": text,
                            "persist": false,
                        }),
                    ));
                }
            }
            AgentCommand::Permission {
                request_id,
                option_id,
                destination,
                feedback,
            } => {
                // Decision dialogs (refusal fallback, Fable overage): the
                // option id IS the completed result string.
                if self.pending_dialogs.remove(&request_id).is_some() {
                    let response = if option_id == "dismiss" {
                        json!({ "behavior": "cancelled" })
                    } else {
                        json!({ "behavior": "completed", "result": option_id })
                    };
                    step.outbound
                        .push(permission_response_frame(&json!(request_id), response));
                    step.events.push(AgentEvent::PermissionResolved {
                        request_id,
                        option_id,
                    });
                    return step;
                }
                let Some(PendingPermission {
                    tool,
                    mut input,
                    mut suggestions,
                }) = self.pending_permissions.remove(&request_id)
                else {
                    // The ask predates this driver process (respawn, toggle,
                    // resume): its reply route died with that process. The
                    // definitive, JOURNALED resolution below is what un-wedges
                    // every attached client and every future replay —
                    // silently dropping the click was the "UI stuck on a
                    // permission card" bug.
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
                let feedback = feedback
                    .map(|f| f.trim().to_string())
                    .filter(|f| !f.is_empty());
                // The destination cycler: the user's chosen save target
                // replaces the CLI's suggested one — except on setMode
                // suggestions, which keep their own (the extension's exact
                // stamping rule).
                if let Some(dest) = &destination {
                    for s in &mut suggestions {
                        if s["type"] != "setMode" && s.get("destination").is_some() {
                            s["destination"] = json!(dest);
                        }
                    }
                }
                let allowed = option_id.starts_with("allow");
                // Plan-approval comments ride updatedInput.{userFeedback,
                // userComments} (the extension's fields). ONLY ExitPlanMode
                // input takes injected keys — for real tools updatedInput is
                // the input the CLI executes, and must echo verbatim.
                let delivered_feedback = if tool == "ExitPlanMode" && allowed {
                    if let Some(fb) = &feedback {
                        input["userFeedback"] = json!(fb);
                        input["userComments"] = json!(fb);
                    }
                    feedback.clone()
                } else if !allowed {
                    feedback.clone()
                } else {
                    None
                };
                let response = match option_id.as_str() {
                    "allow_always" if !suggestions.is_empty() => json!({
                        "behavior": "allow",
                        "updatedInput": input,
                        "updatedPermissions": suggestions,
                    }),
                    id if id.starts_with("allow") => json!({
                        "behavior": "allow",
                        "updatedInput": input,
                    }),
                    // Feedback-denial: the reason rides the deny message and
                    // interrupt:false keeps the turn alive so the model
                    // reacts to it in place (the extension's semantics).
                    _ => match &feedback {
                        Some(fb) => json!({
                            "behavior": "deny",
                            "message": format!("{DENY_DIRECTIVE}\n\nThe user's feedback: {fb}"),
                            "interrupt": false,
                        }),
                        // Bare deny: the directive constant, aborting the turn.
                        None => json!({
                            "behavior": "deny",
                            "message": DENY_DIRECTIVE,
                            "interrupt": true,
                        }),
                    },
                };
                step.outbound
                    .push(permission_response_frame(&json!(request_id), response));
                // "Yes, and auto-accept edits": the mode change is a separate
                // verified control; its ack lands as ModeChanged.
                if tool == "ExitPlanMode" && option_id == "allow_accept_edits" {
                    let id = self.ctl_id();
                    self.pending_controls
                        .insert(id.clone(), PendingControl::SetMode("acceptEdits".into()));
                    step.outbound.push(control_request_frame(
                        &id,
                        json!({ "subtype": "set_permission_mode", "mode": "acceptEdits" }),
                    ));
                }
                step.events.push(AgentEvent::PermissionResolved {
                    request_id,
                    option_id,
                });
                // Feedback the model actually received is transcript truth —
                // echo it as the user message it is (replay rebuilds it too).
                if let Some(fb) = delivered_feedback {
                    step.events.push(AgentEvent::UserMessage {
                        text: fb,
                        attachments: 0,
                        id: None,
                        queued: false,
                    });
                }
            }
            AgentCommand::Interrupt => {
                // Recorded so on_result can stamp the abort as user-initiated
                // (TurnAborted.interrupted) — the ack itself says nothing and
                // the result string is free text.
                self.interrupt_requested = true;
                // Arm the watchdog: if the CLI never answers with an is_error
                // result (an interrupt it treats as a no-op, or a wedged
                // turn), `tick` synthesizes the abort so the user can escape a
                // stuck-running state. A real result disarms it first.
                self.interrupt_grace = Some(INTERRUPT_GRACE_TICKS);
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::Interrupt);
                step.outbound.push(control_request_frame(
                    &id,
                    json!({ "subtype": "interrupt" }),
                ));
            }
            AgentCommand::SetMode { mode_id } => {
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::SetMode(mode_id.clone()));
                step.outbound.push(control_request_frame(
                    &id,
                    json!({ "subtype": "set_permission_mode", "mode": mode_id }),
                ));
            }
            AgentCommand::SetModel { model_id } => {
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::SetModel(model_id.clone()));
                step.outbound.push(control_request_frame(
                    &id,
                    json!({ "subtype": "set_model", "model": model_id }),
                ));
            }
            // Session-scoped effort: the extension's exact control
            // (apply_flag_settings never persists to settings files here).
            AgentCommand::SetEffort { effort_id } => {
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::ApplyFlags);
                step.outbound.push(control_request_frame(
                    &id,
                    json!({
                        "subtype": "apply_flag_settings",
                        "settings": { "effortLevel": effort_id },
                    }),
                ));
            }
            // Ultracode: xhigh effort + standing workflow orchestration,
            // session-scoped by design (mined: "interactive toggles never
            // persist it").
            AgentCommand::SetUltracode { enabled } => {
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::ApplyFlags);
                step.outbound.push(control_request_frame(
                    &id,
                    json!({
                        "subtype": "apply_flag_settings",
                        "settings": { "ultracode": enabled },
                    }),
                ));
            }
            AgentCommand::SetThinking { enabled } => {
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::SetThinking);
                // 31999 is the extension's extended-thinking budget constant;
                // "summarized" streams thought summaries as visible deltas
                // (null hides the text entirely — the status row would be
                // the only sign of a think).
                step.outbound.push(control_request_frame(
                    &id,
                    json!({
                        "subtype": "set_max_thinking_tokens",
                        "max_thinking_tokens": if enabled { 31999 } else { 0 },
                        "thinking_display": if enabled { json!("summarized") } else { serde_json::Value::Null },
                    }),
                ));
            }
            AgentCommand::Answer {
                request_id,
                answers,
            } => {
                let Some(input) = self.pending_questions.remove(&request_id) else {
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
                // Mined contract: allow with updatedInput {questions (echoed),
                // answers: {questionText: labels ", "-joined}}.
                let mut answer_map = serde_json::Map::new();
                for (question, labels) in &answers {
                    answer_map.insert(question.clone(), json!(labels.join(", ")));
                }
                step.outbound.push(permission_response_frame(
                    &json!(request_id),
                    json!({
                        "behavior": "allow",
                        "updatedInput": {
                            "questions": input["questions"],
                            "answers": answer_map,
                        },
                    }),
                ));
                // The chosen labels ride the resolution so the transcript
                // (and every replay) shows question + answer, not a vanish.
                step.events.push(AgentEvent::QuestionResolved {
                    request_id,
                    answers,
                });
            }
            AgentCommand::GetUsage => {
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::GetUsage);
                step.outbound.push(control_request_frame(
                    &id,
                    json!({ "subtype": "get_usage" }),
                ));
            }
            AgentCommand::BackgroundTool { tool_call_id } => {
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::Background);
                step.outbound.push(control_request_frame(
                    &id,
                    json!({ "subtype": "background_tasks", "tool_use_id": tool_call_id }),
                ));
            }
            AgentCommand::StopTask { task_id } => {
                // The client sends the transcript ROW id (all it ever sees).
                // Resolve it to the native task key: task_rows maps task_id →
                // row for both synthesized ("task:{id}") and Task-tool-card
                // rows; the prefix strip covers a row whose map entry is gone.
                let native = self
                    .task_rows
                    .iter()
                    .find(|(_, row)| **row == task_id)
                    .map(|(key, _)| key.clone())
                    .or_else(|| task_id.strip_prefix("task:").map(String::from))
                    .unwrap_or(task_id);
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::StopTask);
                step.outbound.push(control_request_frame(
                    &id,
                    json!({ "subtype": "stop_task", "task_id": native }),
                ));
            }
            // Compact rides claude's own slash catalog: the composer sends
            // "/compact" as prompt text, so the control channel has nothing
            // to do (this command is the codex path).
            AgentCommand::Compact => {}
            AgentCommand::Rewind {
                user_message_id,
                dry_run,
            } => {
                let id = self.ctl_id();
                self.pending_controls.insert(
                    id.clone(),
                    PendingControl::Rewind {
                        user_message_id: user_message_id.clone(),
                        dry_run,
                    },
                );
                step.outbound.push(control_request_frame(
                    &id,
                    json!({
                        "subtype": "rewind_files",
                        "user_message_id": user_message_id,
                        "dry_run": dry_run,
                    }),
                ));
            }
            AgentCommand::GetMcp => {
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::McpStatus);
                step.outbound.push(control_request_frame(
                    &id,
                    json!({ "subtype": "mcp_status" }),
                ));
            }
            AgentCommand::SetMcpEnabled { server, enabled } => {
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::McpMutate);
                step.outbound.push(control_request_frame(
                    &id,
                    json!({ "subtype": "mcp_toggle", "serverName": server, "enabled": enabled }),
                ));
            }
            AgentCommand::ReconnectMcp { server } => {
                let id = self.ctl_id();
                self.pending_controls
                    .insert(id.clone(), PendingControl::McpMutate);
                step.outbound.push(control_request_frame(
                    &id,
                    json!({ "subtype": "mcp_reconnect", "serverName": server }),
                ));
            }
            // Pull back a still-queued message. Because queued messages are HELD
            // (never written to the CLI until the running turn ends), a cancel is
            // a pure local removal — there is nothing in the CLI to un-queue, so
            // the pull-back is guaranteed, with no `cancel_async_message`
            // round-trip that could race or fail. The `Cancelled` resolution is
            // emitted unconditionally, tombstone-style: for a held message it
            // pulls it back before the flush; for a DROPPED one (process died —
            // its ✕ is the "dismiss" affordance) it clears the "not delivered"
            // bubble on live and replay alike; for one that already flushed
            // `sent` (a late click racing the flush) the reducer no-ops — the
            // message is visibly in the transcript, which is its own answer.
            AgentCommand::CancelQueued { id } => {
                self.queued_sends.retain(|(q, _)| q != &id);
                step.events.push(AgentEvent::UserMessageUpdate {
                    id,
                    state: UserMessageState::Cancelled,
                });
            }
        }
        step
    }

    fn ctl_id(&mut self) -> String {
        self.next_ctl += 1;
        format!("ctl_{}", self.next_ctl)
    }

    /// Teardown resolutions: every pending ask's reply route is this
    /// process's stdin, so the journal must not outlive it with the ask
    /// dangling (a replay would strand the card forever — see the harness's
    /// drain call in `run_driver`).
    fn drain_pending(&mut self) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for request_id in std::mem::take(&mut self.pending_questions).into_keys() {
            events.push(AgentEvent::QuestionResolved {
                request_id,
                answers: Default::default(),
            });
        }
        let permissions = std::mem::take(&mut self.pending_permissions).into_keys();
        let dialogs = std::mem::take(&mut self.pending_dialogs).into_keys();
        for request_id in permissions.chain(dialogs) {
            events.push(AgentEvent::PermissionResolved {
                request_id,
                option_id: "expired".into(),
            });
        }
        // A hard kill mid-queue must not strand a held message as "queued"
        // forever on replay — drop what the CLI never got (it was never even
        // written), the same resolution an interrupt's is_error result gives.
        for (id, _content) in std::mem::take(&mut self.queued_sends) {
            events.push(AgentEvent::UserMessageUpdate {
                id,
                state: UserMessageState::Dropped,
            });
        }
        // Also drop any batch flushed on the final result whose write never
        // confirmed shipped (no frame followed): the process is gone, so those
        // ids would otherwise strand "queued". A drop for one that DID ship (it
        // already resolved `sent`) is a harmless no-op in the reducer.
        for id in self.flushing.drain(..) {
            events.push(AgentEvent::UserMessageUpdate {
                id,
                state: UserMessageState::Dropped,
            });
        }
        events
    }

    /// Harness timer tick: just the interrupt watchdog. Queued sends need no
    /// timer — they are held and flushed deterministically on the running turn's
    /// result, so there is no coalesced surplus for a timer to reconcile.
    fn tick(&mut self) -> DriverStep {
        self.interrupt_watchdog()
    }

    /// The interrupt watchdog. Interrupting a claude turn is only observable
    /// through the CLI's own is_error `result` (there's no interrupt-specific
    /// ack), so an interrupt the CLI treats as a no-op — or a wedged turn —
    /// would leave the session "running" with no escape. When the grace armed
    /// on `Interrupt` expires with a turn still open, synthesize the abort the
    /// CLI never sent (`TurnAborted{interrupted}`) — and, like every turn end,
    /// flush the held queue: a stop ends only the current turn, never the
    /// user's queued messages. The write is best-effort against a possibly
    /// wedged child — a failed/timed-out write tears the driver down and the
    /// `flushing` stage drops the batch honestly. Idle-guarded, so an
    /// interrupt pressed with nothing running stays a no-op.
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
            // Interrupt while idle is a CLI no-op — nothing to abort.
            return step;
        }
        let turn_id = self.turn_id();
        self.turn_active = false;
        self.interrupt_requested = false;
        // Same per-turn cleanup a real result performs.
        self.tool_kinds.clear();
        self.streamed.clear();
        self.task_rows.clear();
        self.agent_tools.clear();
        self.thinking_emitted = 0;
        step.events.push(AgentEvent::TurnAborted {
            turn_id,
            reason: "interrupted".into(),
            interrupted: true,
        });
        for (id, content) in std::mem::take(&mut self.queued_sends) {
            step.outbound.push(user_message_frame(&id, content));
            self.flushing.push(id.clone());
            step.events.push(AgentEvent::UserMessageUpdate {
                id,
                state: UserMessageState::Sent,
            });
        }
        step
    }
}

/// Harness adapter: the inherent methods above ARE the state machine; these
/// forward the harness's generic calls to them (inherent methods win in
/// `self.x()` resolution, so there is no recursion).
impl Mapper for ClaudeMapper {
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
        self.tick()
    }
}

fn claude_modes() -> Vec<ModeInfo> {
    vec![
        ModeInfo {
            id: "default".into(),
            label: "Ask each time".into(),
        },
        ModeInfo {
            id: "acceptEdits".into(),
            label: "Accept edits".into(),
        },
        ModeInfo {
            id: "plan".into(),
            label: "Plan mode".into(),
        },
        ModeInfo {
            id: "auto".into(),
            label: "Auto (Claude decides)".into(),
        },
        ModeInfo {
            id: "dontAsk".into(),
            label: "Don't ask".into(),
        },
        ModeInfo {
            id: "bypassPermissions".into(),
            label: "Bypass permissions".into(),
        },
    ]
}

// The tool-block translators below are `pub(crate)` because the offline
// transcript importer (`transcript.rs`) reuses them verbatim: history imported
// from claude's own transcript must render identically to a live session and
// stay correct as this mapping evolves.
pub(crate) fn tool_kind(name: &str) -> ToolKind {
    match name {
        "Bash" | "BashOutput" | "KillShell" => ToolKind::Execute,
        "Read" => ToolKind::Read,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ToolKind::Edit,
        "Grep" | "Glob" => ToolKind::Search,
        "WebFetch" | "WebSearch" => ToolKind::Fetch,
        "Task" => ToolKind::Agent,
        _ => ToolKind::Other,
    }
}

pub(crate) fn tool_title(name: &str, input: &Value) -> String {
    let detail = match name {
        "Bash" => input["command"].as_str(),
        "Read" | "Edit" | "Write" | "MultiEdit" => input["file_path"].as_str(),
        "NotebookEdit" => input["notebook_path"].as_str(),
        "Grep" | "Glob" => input["pattern"].as_str(),
        "WebFetch" => input["url"].as_str(),
        "WebSearch" => input["query"].as_str(),
        "Task" => input["description"].as_str(),
        _ => None,
    };
    match detail {
        Some(detail) => format!("{name}: {}", truncate_label(detail, 120)),
        None => name.to_string(),
    }
}

pub(crate) fn tool_locations(input: &Value) -> Vec<String> {
    ["file_path", "path", "notebook_path"]
        .iter()
        .filter_map(|key| input[key].as_str())
        .map(String::from)
        .collect()
}

/// Edit-family inputs carry the change itself — surface it as diff content
/// without waiting for the (uninformative) tool result.
pub(crate) fn edit_diff_content(name: &str, input: &Value) -> Option<ToolContent> {
    let path = input["file_path"].as_str()?.to_string();
    match name {
        "Write" => {
            let (new_text, truncated) = cap_diff(input["content"].as_str()?);
            Some(ToolContent::Diff {
                path,
                old_text: None,
                new_text,
                truncated,
            })
        }
        "Edit" => {
            let (new_text, truncated) = cap_diff(input["new_string"].as_str()?);
            let (old_text, old_truncated) = cap_diff(input["old_string"].as_str()?);
            Some(ToolContent::Diff {
                path,
                old_text: Some(old_text),
                new_text,
                truncated: truncated || old_truncated,
            })
        }
        "MultiEdit" => {
            let edits = input["edits"].as_array()?;
            // A MultiEdit can carry an unbounded number of edits, each up to the
            // per-file cap — enforce the per-turn diff budget so one tool call
            // can't produce a tens-of-megabytes event.
            let mut diffs: Vec<ToolContent> = Vec::new();
            let mut used = 0usize;
            for e in edits {
                let (Some(new_str), Some(old_str)) =
                    (e["new_string"].as_str(), e["old_string"].as_str())
                else {
                    continue;
                };
                if used + new_str.len() + old_str.len() > DIFF_TURN_BUDGET && !diffs.is_empty() {
                    // Mark the batch as truncated via the last kept diff.
                    if let Some(ToolContent::Diff { truncated, .. }) = diffs.last_mut() {
                        *truncated = true;
                    }
                    break;
                }
                let (new_text, truncated) = cap_diff(new_str);
                let (old_text, old_truncated) = cap_diff(old_str);
                used += new_text.len() + old_text.len();
                diffs.push(ToolContent::Diff {
                    path: path.clone(),
                    old_text: Some(old_text),
                    new_text,
                    truncated: truncated || old_truncated,
                });
            }
            if diffs.is_empty() {
                None
            } else {
                Some(ToolContent::Batch { diffs })
            }
        }
        _ => None,
    }
}

fn cap_diff(text: &str) -> (String, bool) {
    if text.len() <= DIFF_FILE_BUDGET {
        (text.to_string(), false)
    } else {
        cap_head_tail(text, DIFF_FILE_BUDGET, 0)
    }
}

pub(crate) fn tool_result_text(block: &Value) -> String {
    match &block["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        // Absent content (`content` omitted → Null) is empty output, not the
        // literal string "null"; other structured values stringify as-is.
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Cap every string leaf of a permission input so a giant Write/Edit payload
/// can't bloat the journaled/replayed event. Structure is preserved (the UI
/// renders specific fields); only oversized leaves are head/tail-truncated.
/// The verbatim input is kept separately for the allow-response echo.
fn cap_preview(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(crate::model::cap_output(s).0),
        Value::Array(arr) => Value::Array(arr.iter().map(cap_preview).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), cap_preview(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper() -> ClaudeMapper {
        ClaudeMapper::new(
            Some("native-1".into()),
            None,
            &json!({ "commands": [{ "name": "compact", "description": "Compact history" }] }),
        )
    }

    #[test]
    fn send_command_emits_user_message_checkpoint_and_turn_start() {
        let mut m = mapper();
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        });
        match &step.events[0] {
            AgentEvent::UserMessage {
                text,
                attachments,
                id,
                queued,
            } => {
                assert_eq!(text, "hello");
                assert_eq!(*attachments, 0);
                assert!(id.is_some(), "sends carry a delivery id");
                assert!(!queued, "a fresh-turn send is not queued");
            }
            other => panic!("expected UserMessage, got {other:?}"),
        }
        let uuid = match &step.events[1] {
            AgentEvent::Checkpoint {
                user_message_id,
                preceding_uuid,
            } => {
                assert!(preceding_uuid.is_none(), "nothing precedes the first send");
                user_message_id.clone()
            }
            other => panic!("expected Checkpoint, got {other:?}"),
        };
        assert!(matches!(step.events[2], AgentEvent::TurnStarted { .. }));
        // Outbound: the uuid-stamped user frame + the one-shot title request.
        assert_eq!(step.outbound[0]["type"], "user");
        assert_eq!(step.outbound[0]["uuid"], json!(uuid));
        assert_eq!(
            step.outbound[1]["request"]["subtype"],
            "generate_session_title"
        );
        assert_eq!(step.outbound[1]["request"]["description"], "hello");

        // A mid-turn send is queued by the CLI: no second TurnStarted until
        // the running turn's result lands.
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "and this".into(),
            }],
        });
        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnStarted { .. })),
            "mid-turn send must not open a turn"
        );
        let queued_id = match &step.events[0] {
            AgentEvent::UserMessage { id, queued, .. } => {
                assert!(queued, "a mid-turn send echoes queued");
                id.clone().unwrap()
            }
            other => panic!("expected UserMessage, got {other:?}"),
        };
        match &step.events[1] {
            AgentEvent::Checkpoint { preceding_uuid, .. } => {
                assert_eq!(preceding_uuid.as_deref(), Some(uuid.as_str()));
            }
            other => panic!("expected Checkpoint, got {other:?}"),
        }
        let step = m.on_frame(&json!({
            "type": "result", "is_error": false,
            "usage": { "output_tokens": 1 }, "duration_ms": 10,
        }));
        // The result resolves the queued message `sent` but opens NO turn: the
        // boundary is LAZY (a synthetic TurnStarted here per queued message was
        // the "stuck running" bug, since the CLI coalesces rapid queued sends).
        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnStarted { .. })),
            "queued pop must NOT eagerly open a turn: {:?}",
            step.events
        );
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessageUpdate {
                    id,
                    state: UserMessageState::Sent,
                } if *id == queued_id
            )),
            "the dequeued message resolves sent at that boundary: {:?}",
            step.events
        );
        // The turn opens only when the queued message's first real frame
        // streams (ensure_turn) — then it is t2.
        let step = m.on_frame(&json!({
            "type": "stream_event",
            "event": { "type": "content_block_delta",
                       "delta": { "type": "text_delta", "text": "hi" } },
        }));
        assert!(
            step.events
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnStarted { turn_id } if turn_id == "t2")),
            "the queued turn opens lazily on its first real frame: {:?}",
            step.events
        );
    }

    /// Two mid-turn sends: the CLI's native queue is FIFO, so each finished
    /// turn resolves the OLDEST queued id and opens the next boundary.
    /// Queued sends are HELD, then flushed together the moment the running
    /// turn's result lands: every held id resolves `sent` in that one step and
    /// each is written to the CLI right then. No per-result FIFO guessing (the
    /// off-by-one that could strand a middle message), no result-count vs
    /// message-count race — a single result flushes the whole held batch.
    #[test]
    fn queued_sends_flush_together_on_turn_end() {
        let mut m = mapper();
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "run".into() }],
        });
        let queued_id = |step: &DriverStep| match &step.events[0] {
            AgentEvent::UserMessage {
                id, queued: true, ..
            } => id.clone().unwrap(),
            other => panic!("expected queued UserMessage, got {other:?}"),
        };
        let second = queued_id(&m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "then this".into(),
            }],
        }));
        let third = queued_id(&m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "and this".into(),
            }],
        }));
        // Held, not dumped: nothing reached the CLI while the turn ran.
        assert_eq!(m.queued_sends.len(), 2, "both sends are held, not written");

        let result = json!({
            "type": "result", "is_error": false,
            "usage": { "output_tokens": 1 }, "duration_ms": 10,
        });
        let step = m.on_frame(&result);
        // BOTH held messages resolve sent on the single turn-end result…
        for id in [&second, &third] {
            assert!(
                step.events.iter().any(|e| matches!(
                    e,
                    AgentEvent::UserMessageUpdate { id: got, state: UserMessageState::Sent } if got == id
                )),
                "every held send flushes sent on the turn's result: {:?}",
                step.events
            );
        }
        // …and each is written to the CLI at that flush (a `user` frame per id).
        let flushed: Vec<&str> = step
            .outbound
            .iter()
            .filter(|o| o["type"] == "user")
            .filter_map(|o| o["uuid"].as_str())
            .collect();
        assert_eq!(
            flushed,
            vec![second.as_str(), third.as_str()],
            "both held sends are written to the CLI, in order: {:?}",
            step.outbound
        );
        assert!(m.queued_sends.is_empty(), "the held queue drained");

        // Queue drained: the next turn's result opens no phantom boundary and
        // resolves nothing.
        let step = m.on_frame(&result);
        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnStarted { .. }))
                && !step
                    .events
                    .iter()
                    .any(|e| matches!(e, AgentEvent::UserMessageUpdate { .. })),
            "no phantom turn or resolution after the queue drains: {:?}",
            step.events
        );
    }

    /// The flush stages its batch until the write is confirmed shipped: if the
    /// child wedges/dies right after its result, `deliver`'s stdin write fails
    /// and the `sent` events never leave — so the teardown MUST drop the staged
    /// batch, not strand it "queued" forever. And once a later frame confirms
    /// the ship, the teardown drops nothing.
    #[test]
    fn a_flush_whose_write_never_ships_is_dropped_on_teardown() {
        let result = json!({
            "type": "result", "is_error": false,
            "usage": { "output_tokens": 1 }, "duration_ms": 10,
        });

        // Write-failure path: flush, then NO confirming frame → teardown drops.
        let mut m = mapper();
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "run".into() }],
        });
        let queued = match &m
            .on_command(AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "held".into(),
                }],
            })
            .events[0]
        {
            AgentEvent::UserMessage { id, .. } => id.clone().unwrap(),
            other => panic!("expected UserMessage, got {other:?}"),
        };
        // Turn ends: on_result stages the batch (write + sent are in the step,
        // but `deliver` performs the write AFTER — it can still fail).
        let step = m.on_frame(&result);
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if *id == queued
            )),
            "the flush built a sent event: {:?}",
            step.events
        );
        assert_eq!(m.flushing, vec![queued.clone()], "the batch is staged");
        // The write failed (child gone): no frame ever confirms it, and teardown
        // drops the staged id rather than leaving it stranded "queued".
        let drained = m.drain_pending();
        assert!(
            drained.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Dropped } if *id == queued
            )),
            "the un-shipped flush drops on teardown: {drained:?}"
        );

        // Ship-confirmed path: a later frame clears the stage, so teardown drops
        // nothing (the message already resolved sent).
        let mut m = mapper();
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "run".into() }],
        });
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "held".into(),
            }],
        });
        m.on_frame(&result); // flush stages the batch
        assert!(!m.flushing.is_empty(), "staged after the flush");
        // The CLI responds — a subsequent frame proves the write shipped.
        m.on_frame(&json!({
            "type": "stream_event",
            "event": { "type": "content_block_delta",
                       "delta": { "type": "text_delta", "text": "hi" } },
        }));
        assert!(m.flushing.is_empty(), "a confirming frame clears the stage");
        assert!(
            m.drain_pending()
                .iter()
                .all(|e| !matches!(e, AgentEvent::UserMessageUpdate { .. })),
            "a shipped flush is not re-dropped on teardown"
        );
    }

    /// A user interrupt aborts ONLY the running turn: the abort carries the
    /// structural `interrupted` flag, and the held queue SURVIVES — it flushes
    /// (written + `sent`) right after the abort, so a stop never un-delivers
    /// the user's queued messages (maintainer decision 2026-07-11).
    #[test]
    fn interrupt_marks_abort_user_initiated_and_preserves_queue() {
        let mut m = mapper();
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "run".into() }],
        });
        let step = m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "queued".into(),
            }],
        });
        let queued = match &step.events[0] {
            AgentEvent::UserMessage {
                id, queued: true, ..
            } => id.clone().unwrap(),
            other => panic!("expected queued UserMessage, got {other:?}"),
        };

        let step = m.on_command(AgentCommand::Interrupt);
        assert_eq!(step.outbound[0]["request"]["subtype"], "interrupt");

        // The CLI ends the turn with an is_error result; its `result` string
        // is free text (often absent) — the flag, not the string, classifies.
        let step = m.on_frame(&json!({ "type": "result", "is_error": true }));
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::TurnAborted { interrupted: true, reason, .. } if reason == "interrupted"
            )),
            "a user stop is structurally marked: {:?}",
            step.events
        );
        // The held message survives the stop: it flushes to the CLI and
        // resolves `sent` in the same step — never `dropped`.
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if *id == queued
            )),
            "the queued send still delivers after the stop: {:?}",
            step.events
        );
        assert!(
            step.outbound
                .iter()
                .any(|o| o["type"] == "user" && o["uuid"] == json!(queued.as_str())),
            "the held send is written to the CLI at the abort flush: {:?}",
            step.outbound
        );
        // The abort precedes the flush in the step, so the delivered bubble
        // lands AFTER the aborted turn in the transcript.
        let abort_pos = step
            .events
            .iter()
            .position(|e| matches!(e, AgentEvent::TurnAborted { .. }))
            .unwrap();
        let sent_pos = step
            .events
            .iter()
            .position(|e| matches!(e, AgentEvent::UserMessageUpdate { .. }))
            .unwrap();
        assert!(abort_pos < sent_pos, "abort first, then the flush");

        // The flag is consumed: a later genuine failure stays a failure.
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "again".into(),
            }],
        });
        let step = m.on_frame(&json!({ "type": "result", "is_error": true }));
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::TurnAborted { interrupted: false, reason, .. } if reason == "turn failed"
            )),
            "the next failure is not mislabeled a stop: {:?}",
            step.events
        );
    }

    /// An interrupt raced past the turn end (benign on the CLI) must not
    /// relabel the NEXT turn's outcome: the flag clears at every result and
    /// at every fresh-turn open.
    #[test]
    fn stale_interrupt_never_relabels_the_next_turn() {
        let mut m = mapper();
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "run".into() }],
        });
        m.on_command(AgentCommand::Interrupt);
        // The turn completed normally before the interrupt reached the CLI.
        m.on_frame(&json!({
            "type": "result", "is_error": false,
            "usage": { "output_tokens": 1 }, "duration_ms": 10,
        }));
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text {
                text: "next".into(),
            }],
        });
        let step = m.on_frame(&json!({ "type": "result", "is_error": true }));
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::TurnAborted {
                    interrupted: false,
                    ..
                }
            )),
            "stale interrupt cleared at the result boundary: {:?}",
            step.events
        );
    }

    /// The `was_active` guard: a result can arrive with NO turn open — e.g. the
    /// CLI answered a flushed message with an empty response, so `ensure_turn`
    /// never fired. The held queue still flushes on that result, but a bare
    /// (turn-less) result must NOT emit a phantom TurnCompleted/TurnAborted —
    /// that stray turn-end was the "stuck running" symptom.
    #[test]
    fn bare_result_with_no_open_turn_emits_no_phantom_turn_end() {
        let mut m = mapper();
        assert!(!m.turn_active, "no turn is open");
        // Seed a held send directly (as if queued behind a turn that then ended
        // without ever opening a boundary of its own).
        m.queued_sends.push_back(("qid".into(), json!([])));

        let step = m.on_frame(&json!({
            "type": "result", "is_error": false,
            "usage": { "output_tokens": 1 }, "duration_ms": 10,
        }));
        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnCompleted { .. })),
            "a bare result opens no phantom TurnCompleted: {:?}",
            step.events
        );
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if id == "qid"
            )),
            "the held id still flushes sent, guard or not: {:?}",
            step.events
        );

        // Same for an is_error bare result: no phantom abort — and the held
        // queue STILL flushes (an error ends only the turn, never the queue).
        m.queued_sends.push_back(("qid2".into(), json!([])));
        let step = m.on_frame(&json!({ "type": "result", "is_error": true }));
        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnAborted { .. })),
            "a bare is_error result opens no phantom TurnAborted: {:?}",
            step.events
        );
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if id == "qid2"
            )),
            "the held id still flushes sent, error or not: {:?}",
            step.events
        );
    }

    /// The interrupt watchdog: when the CLI never answers an interrupt with a
    /// result (a no-op interrupt, or a wedged turn), the grace expires and the
    /// driver synthesizes the abort so the user escapes a stuck-running state.
    #[test]
    fn interrupt_watchdog_aborts_a_hung_turn_after_the_grace() {
        let mut m = mapper();
        // A running turn with a queued message behind it.
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "go".into() }],
        });
        let queued = match &m
            .on_command(AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "queued".into(),
                }],
            })
            .events[0]
        {
            AgentEvent::UserMessage { id, .. } => id.clone().unwrap(),
            other => panic!("expected UserMessage, got {other:?}"),
        };

        // Interrupt: the CLI (fake) never answers with a result. Ticks below
        // the grace do nothing…
        let step = m.on_command(AgentCommand::Interrupt);
        assert_eq!(step.outbound[0]["request"]["subtype"], "interrupt");
        for _ in 0..(INTERRUPT_GRACE_TICKS - 1) {
            let step = m.tick();
            assert!(step.events.is_empty(), "no abort before the grace expires");
        }
        assert!(m.turn_active, "still running until the grace fires");

        // …the expiring tick synthesizes the abort — and, like every turn end,
        // flushes the held queue (best-effort write; `sent`). A stop never
        // un-delivers the user's queued messages.
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
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if *id == queued
            )),
            "the queue still delivers after the watchdog abort: {:?}",
            step.events
        );
        assert!(
            step.outbound
                .iter()
                .any(|o| o["type"] == "user" && o["uuid"] == json!(queued.as_str())),
            "the held send is written at the watchdog flush: {:?}",
            step.outbound
        );
        assert!(!m.turn_active, "the turn is closed");

        // Idempotent: a further tick does nothing (grace disarmed).
        assert!(m.tick().events.is_empty(), "watchdog fires exactly once");
    }

    /// A real result landing before the grace expires disarms the watchdog, so
    /// a genuine turn is never double-aborted.
    #[test]
    fn real_result_disarms_the_interrupt_watchdog() {
        let mut m = mapper();
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "go".into() }],
        });
        m.on_command(AgentCommand::Interrupt);
        // The CLI answers the interrupt with its is_error result…
        let step = m.on_frame(&json!({ "type": "result", "is_error": true }));
        assert!(
            step.events
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnAborted { .. })),
            "the real interrupt result aborts the turn: {:?}",
            step.events
        );
        assert!(m.interrupt_grace.is_none(), "the watchdog is disarmed");
        // …so ticking past the grace produces no second abort.
        for _ in 0..(INTERRUPT_GRACE_TICKS + 1) {
            assert!(
                m.tick().events.is_empty(),
                "no double abort after a real end"
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

    /// Feature 1 (unit): a coalesced surplus flushes `sent` when the driver
    /// idles. Turn one's result pops the OLDEST queued id; the surplus never
    /// gets a result of its own (the CLI coalesced it), so the idle-flush
    /// resolves it once the grace expires — it must not stick "queued".
    /// Feature 2 (unit): cancelling a still-held message removes it locally and
    /// resolves it `Cancelled` — no `cancel_async_message` round-trip (the CLI
    /// never received it) — and the turn-end flush then delivers only the
    /// SURVIVING held message, never the cancelled one.
    #[test]
    fn cancel_queued_removes_a_held_send() {
        let mut m = mapper();
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "run".into() }],
        });
        let queued_id = |step: &DriverStep| match &step.events[0] {
            AgentEvent::UserMessage {
                id, queued: true, ..
            } => id.clone().unwrap(),
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

        // Cancel the FIRST held ("second"): it leaves the queue and emits
        // Cancelled — with NO outbound (the CLI never had it to un-queue).
        let step = m.on_command(AgentCommand::CancelQueued { id: second.clone() });
        assert_eq!(
            step.events,
            vec![AgentEvent::UserMessageUpdate {
                id: second.clone(),
                state: UserMessageState::Cancelled,
            }]
        );
        assert!(
            step.outbound.is_empty(),
            "a held cancel needs no CLI round-trip: {:?}",
            step.outbound
        );

        // The turn-end flush now delivers only "third" — the cancelled one is
        // gone, so it neither resolves sent nor gets written to the CLI.
        let step = m.on_frame(&json!({
            "type": "result", "is_error": false,
            "usage": { "output_tokens": 1 }, "duration_ms": 10,
        }));
        assert!(
            step.events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessageUpdate { id, state: UserMessageState::Sent } if *id == third
            )),
            "the surviving message resolves sent: {:?}",
            step.events
        );
        assert!(
            !step.events.iter().any(|e| matches!(
                e,
                AgentEvent::UserMessageUpdate { id, .. } if *id == second
            )),
            "the cancelled message never resolves again: {:?}",
            step.events
        );
        let flushed: Vec<&str> = step
            .outbound
            .iter()
            .filter(|o| o["type"] == "user")
            .filter_map(|o| o["uuid"].as_str())
            .collect();
        assert_eq!(
            flushed,
            vec![third.as_str()],
            "only the surviving held send is written to the CLI: {:?}",
            step.outbound
        );
    }

    /// Cancelling a message that already resolved emits a tombstone
    /// `Cancelled` (no CLI frame): for an already-`sent` id the reducer
    /// no-ops (the message is visibly in the transcript), and for a DROPPED
    /// one the same event is the ✕-dismiss that clears the "not delivered"
    /// bubble on live and replay alike.
    #[test]
    fn cancel_queued_after_delivery_is_a_reducer_noop_tombstone() {
        let mut m = mapper();
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "run".into() }],
        });
        let queued = match &m
            .on_command(AgentCommand::Send {
                blocks: vec![ContentBlock::Text {
                    text: "queued".into(),
                }],
            })
            .events[0]
        {
            AgentEvent::UserMessage { id, .. } => id.clone().unwrap(),
            other => panic!("expected UserMessage, got {other:?}"),
        };
        // Turn one's result flushes it `sent` — now it's delivered.
        m.on_frame(&json!({
            "type": "result", "is_error": false,
            "usage": { "output_tokens": 1 }, "duration_ms": 10,
        }));
        // A late cancel: the tombstone `Cancelled`, nothing to the CLI. The
        // reducer ignores a cancel for an id no longer pending, so the
        // delivered message is untouched.
        let step = m.on_command(AgentCommand::CancelQueued { id: queued.clone() });
        assert!(step.outbound.is_empty(), "nothing goes to the CLI");
        assert_eq!(
            step.events,
            vec![AgentEvent::UserMessageUpdate {
                id: queued,
                state: UserMessageState::Cancelled,
            }]
        );
    }

    #[test]
    fn tool_use_maps_kind_title_and_diff() {
        let mut m = mapper();
        m.turn_active = true; // a real tool_use always lands inside a turn
        let frame = json!({
            "type": "assistant",
            "message": { "id": "m1", "content": [
                { "type": "tool_use", "id": "tu1", "name": "Edit",
                  "input": { "file_path": "/tmp/a.rs", "old_string": "x", "new_string": "y" } },
            ]},
        });
        let step = m.on_frame(&frame);
        match &step.events[0] {
            AgentEvent::ToolCall {
                kind,
                title,
                locations,
                ..
            } => {
                assert_eq!(*kind, ToolKind::Edit);
                assert!(title.starts_with("Edit: /tmp/a.rs"));
                assert_eq!(locations, &vec!["/tmp/a.rs".to_string()]);
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        match &step.events[1] {
            AgentEvent::ToolCallUpdate {
                content:
                    Some(ToolContent::Diff {
                        old_text, new_text, ..
                    }),
                ..
            } => {
                assert_eq!(old_text.as_deref(), Some("x"));
                assert_eq!(new_text, "y");
            }
            other => panic!("expected diff update, got {other:?}"),
        }
    }

    #[test]
    fn todo_write_becomes_plan() {
        let mut m = mapper();
        m.turn_active = true;
        let frame = json!({
            "type": "assistant",
            "message": { "id": "m1", "content": [
                { "type": "tool_use", "id": "tu1", "name": "TodoWrite",
                  "input": { "todos": [
                      { "content": "a", "status": "completed" },
                      { "content": "b", "status": "in_progress" },
                  ]}},
            ]},
        });
        let step = m.on_frame(&frame);
        assert_eq!(
            step.events,
            vec![AgentEvent::Plan {
                entries: vec![
                    PlanEntry {
                        content: "a".into(),
                        status: PlanStatus::Done
                    },
                    PlanEntry {
                        content: "b".into(),
                        status: PlanStatus::InProgress
                    },
                ]
            }]
        );
    }

    #[test]
    fn streamed_messages_are_not_duplicated_by_complete_frames() {
        let mut m = mapper();
        m.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: "go".into() }],
        });
        m.on_frame(&json!({
            "type": "stream_event",
            "event": { "type": "message_start", "message": { "id": "m1" } },
        }));
        let step = m.on_frame(&json!({
            "type": "stream_event",
            "event": { "type": "content_block_delta",
                       "delta": { "type": "text_delta", "text": "hi" } },
        }));
        assert!(step.events.is_empty(), "small delta stays buffered");

        // The complete assistant frame for the same message must be skipped…
        let step = m.on_frame(&json!({
            "type": "assistant",
            "message": { "id": "m1", "content": [{ "type": "text", "text": "hi there" }] },
        }));
        assert!(step.events.is_empty());

        // …and the buffered delta text flushes with the result.
        let step = m.on_frame(&json!({
            "type": "result", "is_error": false, "total_cost_usd": 0.01,
            "usage": { "input_tokens": 5, "output_tokens": 2 }, "duration_ms": 100,
        }));
        assert_eq!(
            step.events[0],
            AgentEvent::MessageChunk {
                turn_id: "t1".into(),
                text: "hi".into()
            }
        );
        match &step.events[1] {
            AgentEvent::TurnCompleted { usage, .. } => {
                assert_eq!(usage.cost_usd, Some(0.01));
                assert_eq!(usage.output_tokens, 2);
            }
            other => panic!("expected TurnCompleted, got {other:?}"),
        }
    }

    #[test]
    fn permission_request_roundtrip_allow_always_carries_rules() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "type": "control_request",
            "request_id": "req-1",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "display_name": "Bash",
                "input": { "command": "make" },
                "tool_use_id": "tu1",
                "permission_suggestions": [
                    { "type": "addRules", "rules": [{ "toolName": "Bash", "ruleContent": "make *" }],
                      "behavior": "allow", "destination": "localSettings" },
                ],
            },
        }));
        match &step.events[0] {
            AgentEvent::PermissionRequest {
                request_id,
                tool_call_id,
                options,
                ..
            } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(tool_call_id.as_deref(), Some("tu1"));
                assert_eq!(options.len(), 3, "allow / always-allow / deny");
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }

        let step = m.on_command(AgentCommand::Permission {
            request_id: "req-1".into(),
            option_id: "allow_always".into(),
            destination: None,
            feedback: None,
        });
        let response = &step.outbound[0]["response"]["response"];
        assert_eq!(response["behavior"], "allow");
        assert!(response["updatedPermissions"].is_array());
        assert_eq!(
            step.events[0],
            AgentEvent::PermissionResolved {
                request_id: "req-1".into(),
                option_id: "allow_always".into()
            }
        );
    }

    #[test]
    fn deny_with_feedback_keeps_turn_alive_and_echoes_user_text() {
        let mut m = mapper();
        m.on_frame(&json!({
            "type": "control_request",
            "request_id": "req-1",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "input": { "command": "rm -rf build" },
                "tool_use_id": "tu1",
            },
        }));
        let step = m.on_command(AgentCommand::Permission {
            request_id: "req-1".into(),
            option_id: "reject_once".into(),
            destination: None,
            feedback: Some("  use `just clean` instead  ".into()),
        });
        let response = &step.outbound[0]["response"]["response"];
        assert_eq!(response["behavior"], "deny");
        assert_eq!(
            response["interrupt"],
            json!(false),
            "feedback-denials must not abort the turn"
        );
        let message = response["message"].as_str().unwrap();
        assert!(message.starts_with(DENY_DIRECTIVE));
        assert!(message.contains("use `just clean` instead"));
        assert!(matches!(
            &step.events[0],
            AgentEvent::PermissionResolved { option_id, .. } if option_id == "reject_once"
        ));
        assert_eq!(
            step.events[1],
            AgentEvent::UserMessage {
                text: "use `just clean` instead".into(),
                attachments: 0,
                id: None,
                queued: false,
            }
        );

        // A bare deny keeps the aborting directive shape.
        m.on_frame(&json!({
            "type": "control_request",
            "request_id": "req-2",
            "request": { "subtype": "can_use_tool", "tool_name": "Bash",
                         "input": { "command": "ls" }, "tool_use_id": "tu2" },
        }));
        let step = m.on_command(AgentCommand::Permission {
            request_id: "req-2".into(),
            option_id: "reject_once".into(),
            destination: None,
            feedback: Some("   ".into()), // whitespace = no feedback
        });
        let response = &step.outbound[0]["response"]["response"];
        assert_eq!(response["message"], DENY_DIRECTIVE);
        assert_eq!(response["interrupt"], json!(true));
        assert_eq!(step.events.len(), 1, "no user echo without feedback");
    }

    #[test]
    fn exit_plan_mode_maps_to_plan_approval_card() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "type": "control_request",
            "request_id": "req-p",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "ExitPlanMode",
                "input": { "plan": "## Plan\n1. add the field" },
                "tool_use_id": "tu-p",
            },
        }));
        match &step.events[0] {
            AgentEvent::PermissionRequest {
                options,
                plan,
                input_preview,
                ..
            } => {
                assert_eq!(plan.as_deref(), Some("## Plan\n1. add the field"));
                let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
                assert_eq!(
                    ids,
                    ["allow_accept_edits", "allow_once", "reject_once"],
                    "the official plan-approval option set, in order"
                );
                assert_eq!(options[0].label, "Yes, and auto-accept edits");
                assert_eq!(options[1].label, "Yes, manually approve");
                assert_eq!(options[2].label, "No, keep planning");
                assert!(
                    input_preview["plan"].is_null(),
                    "the plan rides its own field, not the preview too"
                );
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }

        // "Yes, and auto-accept edits" with comments: allow echoes the input
        // plus userFeedback/userComments, and a set_permission_mode follow-up
        // rides the same step.
        let step = m.on_command(AgentCommand::Permission {
            request_id: "req-p".into(),
            option_id: "allow_accept_edits".into(),
            destination: None,
            feedback: Some("also update the docs".into()),
        });
        let response = &step.outbound[0]["response"]["response"];
        assert_eq!(response["behavior"], "allow");
        assert_eq!(
            response["updatedInput"]["plan"],
            "## Plan\n1. add the field"
        );
        assert_eq!(
            response["updatedInput"]["userFeedback"],
            "also update the docs"
        );
        assert_eq!(
            response["updatedInput"]["userComments"],
            "also update the docs"
        );
        assert_eq!(
            step.outbound[1]["request"]["subtype"],
            "set_permission_mode"
        );
        assert_eq!(step.outbound[1]["request"]["mode"], "acceptEdits");
        assert!(matches!(
            &step.events[1],
            AgentEvent::UserMessage { text, .. } if text == "also update the docs"
        ));
        // The follow-up's ack resolves to ModeChanged(acceptEdits).
        let ctl = step.outbound[1]["request_id"].as_str().unwrap().to_string();
        let step = m.on_frame(&json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": ctl, "response": {} },
        }));
        assert_eq!(
            step.events[0],
            AgentEvent::ModeChanged {
                mode_id: "acceptEdits".into()
            }
        );
    }

    #[test]
    fn plan_approval_manual_and_keep_planning_paths() {
        let plan_request = |id: &str| {
            json!({
                "type": "control_request",
                "request_id": id,
                "request": {
                    "subtype": "can_use_tool",
                    "tool_name": "ExitPlanMode",
                    "input": { "plan": "the plan" },
                    "tool_use_id": "tu-p",
                },
            })
        };

        // "Yes, manually approve": plain allow, no mode follow-up.
        let mut m = mapper();
        m.on_frame(&plan_request("req-p"));
        let step = m.on_command(AgentCommand::Permission {
            request_id: "req-p".into(),
            option_id: "allow_once".into(),
            destination: None,
            feedback: None,
        });
        let response = &step.outbound[0]["response"]["response"];
        assert_eq!(response["behavior"], "allow");
        assert!(
            response["updatedInput"]["userFeedback"].is_null(),
            "no comments ⇒ no injected fields"
        );
        assert_eq!(step.outbound.len(), 1, "manual approve sets no mode");

        // "No, keep planning" with comments: the feedback-denial shape.
        let mut m = mapper();
        m.on_frame(&plan_request("req-p"));
        let step = m.on_command(AgentCommand::Permission {
            request_id: "req-p".into(),
            option_id: "reject_once".into(),
            destination: None,
            feedback: Some("split step 1 into two".into()),
        });
        let response = &step.outbound[0]["response"]["response"];
        assert_eq!(response["behavior"], "deny");
        assert_eq!(response["interrupt"], json!(false));
        assert!(response["message"]
            .as_str()
            .unwrap()
            .contains("split step 1 into two"));
    }

    #[test]
    fn tool_results_cap_output_and_mark_failures() {
        let mut m = mapper();
        m.on_frame(&json!({
            "type": "assistant",
            "message": { "id": "m1", "content": [
                { "type": "tool_use", "id": "tu1", "name": "Bash", "input": { "command": "ls" } },
            ]},
        }));
        let step = m.on_frame(&json!({
            "type": "user",
            "message": { "content": [
                { "type": "tool_result", "tool_use_id": "tu1",
                  "content": "boom", "is_error": true },
            ]},
        }));
        match &step.events[0] {
            AgentEvent::ToolCallUpdate {
                id,
                status,
                content: Some(ToolContent::Output { text, .. }),
            } => {
                assert_eq!(id, "tu1");
                assert_eq!(*status, ToolStatus::Failed);
                assert_eq!(text, "boom");
            }
            other => panic!("expected failed update, got {other:?}"),
        }
    }

    #[test]
    fn set_mode_resolves_via_control_response() {
        let mut m = mapper();
        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "acceptEdits".into(),
        });
        let ctl_id = step.outbound[0]["request_id"].as_str().unwrap().to_string();
        assert_eq!(
            step.outbound[0]["request"]["subtype"],
            "set_permission_mode"
        );

        let step = m.on_frame(&json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": ctl_id, "response": {} },
        }));
        assert_eq!(
            step.events[0],
            AgentEvent::ModeChanged {
                mode_id: "acceptEdits".into()
            }
        );
    }

    #[test]
    fn ask_user_question_roundtrips_as_question() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "type": "control_request",
            "request_id": "req-q",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "AskUserQuestion",
                "tool_use_id": "tu-q",
                "input": { "questions": [{
                    "question": "Which database?",
                    "header": "Storage",
                    "options": [
                        { "label": "SQLite", "description": "single file" },
                        { "label": "Postgres" },
                    ],
                    "multiSelect": false,
                }]},
            },
        }));
        match &step.events[0] {
            AgentEvent::QuestionRequest {
                request_id,
                questions,
            } => {
                assert_eq!(request_id, "req-q");
                assert_eq!(questions[0].id, "Which database?");
                assert_eq!(questions[0].options.len(), 2);
                assert!(!questions[0].multi_select);
            }
            other => panic!("expected QuestionRequest, got {other:?}"),
        }

        let mut answers = std::collections::HashMap::new();
        answers.insert("Which database?".to_string(), vec!["SQLite".to_string()]);
        let step = m.on_command(AgentCommand::Answer {
            request_id: "req-q".into(),
            answers,
        });
        let response = &step.outbound[0]["response"]["response"];
        assert_eq!(response["behavior"], "allow");
        assert_eq!(
            response["updatedInput"]["answers"]["Which database?"],
            "SQLite"
        );
        assert!(response["updatedInput"]["questions"].is_array());
        match &step.events[0] {
            AgentEvent::QuestionResolved {
                request_id,
                answers,
            } => {
                assert_eq!(request_id, "req-q");
                assert_eq!(
                    answers.get("Which database?"),
                    Some(&vec!["SQLite".to_string()]),
                    "the chosen labels ride the resolution for history/replay"
                );
            }
            other => panic!("expected QuestionResolved, got {other:?}"),
        }
    }

    #[test]
    fn ask_user_question_tool_use_emits_no_tool_row() {
        let mut m = mapper();
        m.turn_active = true;
        let step = m.on_frame(&json!({
            "type": "assistant",
            "message": { "id": "m1", "content": [
                { "type": "tool_use", "id": "tu-q", "name": "AskUserQuestion",
                  "input": { "questions": [{ "question": "Q?" }] } },
            ]},
        }));
        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCall { .. })),
            "the question card is the surface, not a bare tool row: {:?}",
            step.events
        );
    }

    #[test]
    fn stale_answer_and_permission_resolve_definitively() {
        // A reply whose request_id no pending map knows (the ask predates
        // this driver process) must produce a journaled resolution + notice,
        // never a silent drop — the reconnect-stranding bug.
        let mut m = mapper();
        let mut answers = std::collections::HashMap::new();
        answers.insert("q".to_string(), vec!["a".to_string()]);
        let step = m.on_command(AgentCommand::Answer {
            request_id: "gone-q".into(),
            answers,
        });
        assert!(step.outbound.is_empty(), "no live request to answer");
        assert!(matches!(
            &step.events[0],
            AgentEvent::QuestionResolved { request_id, answers } if request_id == "gone-q" && answers.is_empty()
        ));
        assert!(matches!(
            &step.events[1],
            AgentEvent::Notice { text } if text.contains("no longer active")
        ));

        let step = m.on_command(AgentCommand::Permission {
            request_id: "gone-p".into(),
            option_id: "allow_once".into(),
            destination: None,
            feedback: None,
        });
        assert!(step.outbound.is_empty());
        assert!(matches!(
            &step.events[0],
            AgentEvent::PermissionResolved { request_id, option_id }
                if request_id == "gone-p" && option_id == "expired"
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
            "type": "control_request",
            "request_id": "req-q",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "AskUserQuestion",
                "input": { "questions": [{ "question": "Q?", "options": [{ "label": "A" }] }] },
            },
        }));
        m.on_frame(&json!({
            "type": "control_request",
            "request_id": "req-p",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "input": { "command": "make" },
            },
        }));
        let events = Mapper::drain_pending(&mut m);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::QuestionResolved { request_id, answers } if request_id == "req-q" && answers.is_empty()
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::PermissionResolved { request_id, option_id }
                if request_id == "req-p" && option_id == "expired"
        )));
        assert!(
            Mapper::drain_pending(&mut m).is_empty(),
            "drain is exhaustive"
        );
    }

    #[test]
    fn unknown_control_subtype_notices_once() {
        let mut m = mapper();
        let frame = json!({
            "type": "control_request",
            "request_id": "req-m",
            "request": { "subtype": "mcp_message" },
        });
        let step = m.on_frame(&frame);
        assert!(
            step.outbound.is_empty(),
            "unknown subtypes park (the CLI's own deadline settles them)"
        );
        assert!(matches!(
            &step.events[0],
            AgentEvent::Notice { text } if text.contains("mcp_message")
        ));
        let step = m.on_frame(&frame);
        assert!(
            step.events.is_empty(),
            "one notice per subtype, not per frame"
        );
    }

    #[test]
    fn user_dialog_roundtrips_completed_and_cancelled() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "type": "control_request",
            "request_id": "req-d",
            "request": {
                "subtype": "request_user_dialog",
                "dialog_kind": "refusal_fallback_prompt",
                "payload": { "originalModel": "claude-fable-5", "fallbackModel": "claude-opus-4-8" },
            },
        }));
        match &step.events[0] {
            AgentEvent::PermissionRequest { options, .. } => {
                assert!(options.iter().any(|o| o.id == "retry_fallback"));
                assert!(options.iter().any(|o| o.label.contains("claude-opus-4-8")));
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
        let step = m.on_command(AgentCommand::Permission {
            request_id: "req-d".into(),
            option_id: "retry_fallback".into(),
            destination: None,
            feedback: None,
        });
        let response = &step.outbound[0]["response"]["response"];
        assert_eq!(response["behavior"], "completed");
        assert_eq!(response["result"], "retry_fallback");

        // Unknown kinds must answer cancelled immediately (never park) AND
        // say so — a silent cancel leaves the agent's "I'm blocked" prose as
        // the only trace the ask existed.
        let step = m.on_frame(&json!({
            "type": "control_request",
            "request_id": "req-x",
            "request": { "subtype": "request_user_dialog", "dialog_kind": "mystery" },
        }));
        assert_eq!(
            step.outbound[0]["response"]["response"]["behavior"],
            "cancelled"
        );
        assert!(matches!(
            &step.events[0],
            AgentEvent::Notice { text } if text.contains("mystery")
        ));
    }

    #[test]
    fn background_tool_acks_with_notice() {
        let mut m = mapper();
        let step = m.on_command(AgentCommand::BackgroundTool {
            tool_call_id: "tu-1".into(),
        });
        assert_eq!(step.outbound[0]["request"]["subtype"], "background_tasks");
        assert_eq!(step.outbound[0]["request"]["tool_use_id"], "tu-1");
        let ctl = step.outbound[0]["request_id"].as_str().unwrap().to_string();
        let step = m.on_frame(&json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": ctl, "response": { "backgrounded": true } },
        }));
        assert!(matches!(
            &step.events[0],
            AgentEvent::Notice { text } if text.contains("background")
        ));
    }

    #[test]
    fn stop_task_resolves_transcript_row_ids_to_native_task_keys() {
        let mut m = mapper();
        // A subagent with no matching Task card synthesizes a "task:{id}" row.
        m.on_frame(&json!({
            "type": "system", "subtype": "task_started",
            "task_type": "local_agent", "task_id": "tk-9",
            "description": "summarize the docs",
        }));
        // The client stops by the ROW id it sees; the wire carries the key.
        let step = m.on_command(AgentCommand::StopTask {
            task_id: "task:tk-9".into(),
        });
        assert_eq!(step.outbound[0]["request"]["subtype"], "stop_task");
        assert_eq!(step.outbound[0]["request"]["task_id"], "tk-9");

        // A subagent that landed on its Task tool card: the row id is the
        // tool_use_id, reverse-mapped through task_rows.
        m.on_frame(&json!({
            "type": "assistant",
            "message": { "id": "m1", "content": [{
                "type": "tool_use", "id": "tu-7", "name": "Task",
                "input": { "description": "audit the tests", "prompt": "…" },
            }]},
        }));
        m.on_frame(&json!({
            "type": "system", "subtype": "task_started",
            "task_type": "local_agent", "task_id": "tk-10",
            "description": "audit the tests",
        }));
        let step = m.on_command(AgentCommand::StopTask {
            task_id: "tu-7".into(),
        });
        assert_eq!(step.outbound[0]["request"]["task_id"], "tk-10");
    }

    #[test]
    fn refusal_fallback_switches_model_and_retracts() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "type": "system",
            "subtype": "model_refusal_fallback",
            "direction": "retry",
            "original_model": "claude-fable-5",
            "fallback_model": "claude-opus-4-8",
            "content": "Safety systems flagged this; retrying on Opus.",
            "api_refusal_category": "bio",
            "retracted_message_uuids": ["u-1", "u-2"],
            "uuid": "f-1",
            "session_id": "s",
        }));
        match &step.events[0] {
            AgentEvent::ModelSwitched {
                from,
                to,
                reason,
                retract_current_turn,
            } => {
                assert_eq!(from.as_deref(), Some("claude-fable-5"));
                assert_eq!(to, "claude-opus-4-8");
                assert_eq!(reason.as_deref(), Some("bio"));
                assert!(retract_current_turn);
            }
            other => panic!("expected ModelSwitched, got {other:?}"),
        }
        assert!(matches!(
            &step.events[1],
            AgentEvent::Notice { text } if text.contains("retrying on Opus")
        ));
        match &step.events[2] {
            AgentEvent::Init { model, .. } => {
                assert_eq!(
                    model.as_deref(),
                    Some("claude-opus-4-8"),
                    "chip follows truth"
                );
            }
            other => panic!("expected Init, got {other:?}"),
        }
    }

    #[test]
    fn consent_fallback_and_status_mode_change_map() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "type": "system",
            "subtype": "model_consent_fallback",
            "choice": "switch_default",
            "fallback_model": "default",
            "persisted_as_default": false,
        }));
        assert!(matches!(
            &step.events[0],
            AgentEvent::ModelSwitched { to, retract_current_turn: false, .. } if to == "default"
        ));

        let step = m.on_frame(&json!({
            "type": "system",
            "subtype": "status",
            "status": null,
            "permissionMode": "acceptEdits",
        }));
        assert_eq!(
            step.events[0],
            AgentEvent::ModeChanged {
                mode_id: "acceptEdits".into()
            }
        );
        // Unchanged mode re-announcements stay silent.
        let step = m.on_frame(&json!({
            "type": "system", "subtype": "status", "permissionMode": "acceptEdits",
        }));
        assert!(step.events.is_empty());
    }

    #[test]
    fn superseding_assistant_message_retracts_before_appending() {
        let mut m = mapper();
        m.turn_active = true;
        let step = m.on_frame(&json!({
            "type": "assistant",
            "uuid": "a-2",
            "supersedes": ["a-1"],
            "message": { "id": "m2", "content": [{ "type": "text", "text": "replacement" }] },
        }));
        assert!(matches!(step.events[0], AgentEvent::MessagesSuperseded));
    }

    #[test]
    fn orphan_frame_opens_a_defensive_turn() {
        // A stream/assistant frame arriving with no active turn (a wrong queue
        // assumption or a parked-prompt replay) must open a TurnStarted rather
        // than stream into a phantom turn.
        let mut m = mapper();
        assert!(!m.turn_active);
        let step = m.on_frame(&json!({
            "type": "assistant",
            "message": { "id": "m1", "content": [{ "type": "tool_use", "id": "tu1",
                "name": "Bash", "input": { "command": "ls" } }] },
        }));
        assert!(
            matches!(&step.events[0], AgentEvent::TurnStarted { turn_id } if turn_id == "t1"),
            "orphan frame opens a turn first: {:?}",
            step.events
        );
        assert!(m.turn_active);
    }

    #[test]
    fn compact_boundary_maps_to_notice() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "type": "system",
            "subtype": "compact_boundary",
            "compact_metadata": { "trigger": "auto", "pre_tokens": 168000 },
        }));
        assert!(matches!(
            &step.events[0],
            AgentEvent::Notice { text } if text.contains("168000")
        ));
    }

    #[test]
    fn subagent_frames_are_skipped() {
        let mut m = mapper();
        let step = m.on_frame(&json!({
            "type": "assistant",
            "parent_tool_use_id": "tu-parent",
            "message": { "id": "m9", "content": [{ "type": "text", "text": "sub" }] },
        }));
        assert!(step.events.is_empty());
    }
}
