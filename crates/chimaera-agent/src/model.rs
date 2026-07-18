//! The normalized agent-event vocabulary — one ACP-shaped model that every
//! driver (claude stream-json, codex app-server, future ACP agents) maps
//! into, and the only thing the journal, the WS channel, and the chat UI
//! ever see. Field taxonomy follows the Agent Client Protocol where it has
//! an opinion (tool kinds, permission option kinds) so a Tier C generic ACP
//! client can slot in without a UI change.
//!
//! Size caps live here, at event construction, because every byte admitted
//! into an `AgentEvent` is a byte the journal stores, the ring holds, and
//! every attached client replays — the daemon shares login nodes, so events
//! are bounded at the source, not at the sinks.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stored tool output cap: enough to read a failure, never a log dump.
pub const TOOL_OUTPUT_HEAD: usize = 12 * 1024;
pub const TOOL_OUTPUT_TAIL: usize = 4 * 1024;
/// Per-file diff cap; a bigger change renders as "open the file".
pub const DIFF_FILE_BUDGET: usize = 64 * 1024;
/// All diff content admitted per turn.
pub const DIFF_TURN_BUDGET: usize = 256 * 1024;
/// Background-task set bound: beyond this the oldest entries drop (the tray
/// is a glance surface; every `BackgroundTasks` event carries the whole set,
/// so the set size is the event size).
pub const BG_TASKS_CAP: usize = 32;
/// One-line cap for background-task descriptions and close summaries.
pub const BG_LABEL_MAX: usize = 200;
/// Bound for a close's output-file PATH — never ellipsized (a truncated
/// path is a corrupt path); an oversized one is dropped whole.
pub const BG_PATH_MAX: usize = 1024;
/// One-line cap for the `SessionStatus` fields (a status line, not prose).
pub const STATUS_DETAIL_MAX: usize = 256;
/// Per-workflow bound on stored per-agent entries — exactly the client's
/// dot-row budget (`DOTS_MAX` in BackgroundTray), so nothing is journaled
/// that never paints. A workflow can spawn up to 1000 agents lifetime; the
/// newest entries win and the `agents_total`/`agents_done` aggregates stay
/// honest beyond the cap.
pub const WF_AGENTS_CAP: usize = 24;
/// Set-wide bound on stored agent entries ACROSS the whole background set.
/// The `BackgroundTasks` event carries the entire set, and the journal
/// replaces any entry over its 256 KiB cap with an Error — which would wipe
/// the tray for every client. This budget keeps the worst-case event far
/// under that cap; when exceeded, the oldest tasks' dot rows are shed
/// (their honest aggregates remain).
pub const WF_AGENTS_SET_BUDGET: usize = 96;
/// One-line cap for a workflow agent's label and result preview.
pub const WF_AGENT_LABEL_MAX: usize = 120;
/// Total serialized-size backstop for a permission/approval input preview
/// ([`cap_preview`]). Leaf-capping alone bounds each string, but a WIDE
/// structure (an array/object with thousands of small leaves — a codex MCP
/// `tool_params`) can still overrun the journal's 256 KiB entry cap, which
/// replaces the whole `PermissionRequest` with an Error while the driver's
/// `pending_approvals` keeps waiting on its now-invisible request id — a turn
/// stuck with no way to approve or deny. Kept well under 256 KiB so the
/// event stays admissible even beside its sibling fields.
pub const PREVIEW_TOTAL_BUDGET: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The driver handshake completed; the session is live.
    Init {
        /// The agent's own session handle (claude session id / codex thread
        /// id) — the resume key for the chat⇄terminal toggle.
        native_session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Permission/approval modes this agent offers (claude permission
        /// modes; codex approval policies). Empty = agent has no mode concept.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        modes: Vec<ModeInfo>,
        #[serde(skip_serializing_if = "Option::is_none")]
        current_mode: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        slash_commands: Vec<SlashCommand>,
        /// Model catalog reported by the agent itself (codex model/list);
        /// empty = the UI falls back to the curated list.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        models: Vec<ModelInfo>,
        /// The agent CLI's `--version` line as the launcher probed it at
        /// spawn (`None` when the probe failed) — journaled so a drifted
        /// binary is diagnosable after the fact; the harness's drift Notice
        /// keys off the same value. Additive: old clients ignore it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_version: Option<String>,
    },
    TurnStarted {
        turn_id: String,
    },
    /// Streamed agent prose (already delta-coalesced by the pump).
    MessageChunk {
        turn_id: String,
        text: String,
    },
    /// Streamed reasoning/thinking.
    ThoughtChunk {
        turn_id: String,
        text: String,
    },
    /// Live thinking-token estimate (claude system/thinking_tokens) — the
    /// "thinking · ~N tokens" status signal; fires even when the thinking
    /// display is summarized and no thought text streams.
    ThinkingTokens {
        tokens: u64,
    },
    /// Echo of what the user sent — journaled so replay rebuilds both sides
    /// of the transcript without a second store.
    UserMessage {
        text: String,
        #[serde(default, skip_serializing_if = "is_zero")]
        attachments: u32,
        /// Client-minted delivery key (claude checkpoint uuid / codex
        /// clientUserMessageId) — what a later `UserMessageUpdate` resolves.
        /// Absent on pre-upgrade journals and transcript-seeded messages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// The agent has NOT consumed this message yet (both drivers hold
        /// mid-turn follow-ups; codex can explicitly promote one via Steer).
        /// Resolved by a `UserMessageUpdate`; default false = delivered.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        queued: bool,
    },
    ToolCall {
        id: String,
        kind: ToolKind,
        title: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        locations: Vec<String>,
        status: ToolStatus,
    },
    ToolCallUpdate {
        id: String,
        status: ToolStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<ToolContent>,
    },
    /// Live tool-output append (codex streams command output as it runs).
    /// Clients append to the tool's output; a later `ToolCallUpdate` with
    /// content is authoritative and replaces the accumulated stream.
    ToolOutputDelta {
        id: String,
        text: String,
    },
    /// Full plan snapshot (agents resend the whole list on change).
    Plan {
        entries: Vec<PlanEntry>,
    },
    PermissionRequest {
        request_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        title: String,
        options: Vec<PermissionOption>,
        /// Raw tool input for the expandable detail view.
        input_preview: Value,
        /// Plan markdown when this request is a plan approval (claude
        /// ExitPlanMode) — present ⇒ the client renders a plan-approval card
        /// instead of the generic permission card. Capped at construction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<String>,
    },
    PermissionResolved {
        request_id: String,
        option_id: String,
    },
    /// The agent asked the user structured questions (claude AskUserQuestion
    /// via can_use_tool; codex item/tool/requestUserInput). Answered with
    /// the `Answer` command.
    QuestionRequest {
        request_id: String,
        questions: Vec<Question>,
        /// Absolute Unix-millisecond deadline for an agent-owned auto-skip
        /// (codex `autoResolutionMs`). Absolute, rather than a duration, so a
        /// reconnect/replay never restarts the countdown. Claude omits it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at_ms: Option<u64>,
    },
    QuestionResolved {
        request_id: String,
        /// The user's chosen labels per question id — journaled so history
        /// (and every replay) can show the question WITH its answer. Empty =
        /// resolved without one (cancelled, expired, or a pre-answers
        /// journal). Additive optional field: old journals deserialize to
        /// empty, old clients ignore it.
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        answers: std::collections::HashMap<String, Vec<String>>,
    },
    ModeChanged {
        mode_id: String,
    },
    /// Effort/ultracode truth read back from the agent (claude get_settings
    /// applied.{effort,ultracode}; re-read after every apply).
    EffortState {
        #[serde(skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ultracode: bool,
    },
    TurnCompleted {
        turn_id: String,
        usage: Usage,
    },
    TurnAborted {
        turn_id: String,
        reason: String,
        /// The abort was a deliberate user stop (the interrupt command), a
        /// fact the driver knows structurally — consumers render it quiet
        /// (idle rail, muted notice), never as a failure. Optional-with-
        /// default so pre-upgrade journals deserialize (false) and failure
        /// aborts serialize byte-identically to before.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        interrupted: bool,
    },
    /// Context-window occupancy after a turn (claude: get_context_usage;
    /// codex: tokenUsage vs modelContextWindow).
    ContextUsage {
        total_tokens: u64,
        max_tokens: u64,
        percentage: f64,
    },
    /// Account usage windows (claude get_usage; rendered by the /usage panel).
    UsageReport {
        windows: Vec<UsageWindow>,
    },
    /// Streamed rate-limit telemetry (claude rate_limit_event; codex
    /// account/rateLimits/updated) — header state, not a transcript block.
    RateLimit {
        /// Most-constrained window, 0-100.
        utilization: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        resets_at: Option<String>,
        /// Which window ("session limit", "weekly limit", …).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// The agent reports the limit as actually hit (requests failing).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        limit_reached: bool,
    },
    /// The serving model changed mid-conversation (claude
    /// model_refusal_fallback / model_consent_fallback; codex
    /// model/rerouted) — keeps the model chip honest. `retract_current_turn`
    /// = the flagged output was withdrawn and the fallback retries (clients
    /// drop the current turn's trailing prose). The agent's own banner text
    /// rides a separate Notice.
    ModelSwitched {
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        to: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        retract_current_turn: bool,
    },
    /// An assistant message superseded earlier output (claude `supersedes`
    /// uuids): the current turn's trailing prose is replaced, not appended.
    MessagesSuperseded,
    /// The agent named this conversation (claude generate_session_title;
    /// codex thread/name/updated) — feeds the workbench naming chain.
    SessionTitle {
        title: String,
    },
    /// A user-message checkpoint anchor. `user_message_id` is the uuid the
    /// client minted onto the outbound user frame (rewind_files key);
    /// `preceding_uuid` is the last transcript message before it (the
    /// conversation-fork resume point). None = nothing precedes it.
    Checkpoint {
        user_message_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preceding_uuid: Option<String>,
    },
    /// Answer to a `Rewind` command. `applied:false` = dry-run report for
    /// the confirm dialog; `applied:true` = files were restored.
    RewindResult {
        user_message_id: String,
        can_rewind: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files_changed: Vec<String>,
        applied: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// MCP server inventory (answer to `GetMcp`; claude mcp_status).
    McpServers {
        servers: Vec<McpServerInfo>,
    },
    /// Informational transcript notice (model rerouted, context compacted…).
    Notice {
        text: String,
    },
    /// The CLI suggests a next prompt (claude prompt_suggestion) — composer
    /// ghost text, accepted with one click/Tab.
    PromptSuggestion {
        text: String,
    },
    /// Journal head marker after compaction: entries before this were dropped.
    Truncated,
    /// The session moved between chat and terminal mode (the toggle).
    ModeSwitch {
        to: SessionUi,
    },
    Error {
        message: String,
        /// Fatal = the driver is dead; non-fatal = noted in the transcript.
        fatal: bool,
    },
    Exited {
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<i32>,
    },
    /// Delivery resolution for a `queued` UserMessage, matched by `id`:
    /// claude flushes its held FIFO after the current turn settles; codex
    /// resolves when the FIFO opens its next turn or the steer RPC answers.
    /// `cancelled` is the user's own pull-back
    /// (`CancelQueued`). Replay is self-correcting — the journal carries the
    /// queued echo and this update through the same reducer, so a
    /// queued-then-sent message renders exactly once, a queued-never-sent one
    /// replays in its final dropped state, and a cancelled one vanishes.
    UserMessageUpdate {
        id: String,
        state: UserMessageState,
    },
    /// The live set of the agent's BACKGROUND tasks — claude's backgrounded
    /// Bash / workflows riding the `task_*` system frames with a
    /// non-`local_agent` task_type (plus anything `background_tasks_changed`
    /// reports, including a backgrounded subagent). LEVEL-SET semantics:
    /// every event carries the WHOLE set (empty = none running), so consumers
    /// replace rather than patch and replay's final state is simply the last
    /// event seen. `closed` rides the tasks that left the set at this event
    /// WITH a verdict — the task_notification close, the only frame that
    /// carries one (set-removals and terminal task_updated patches don't;
    /// they just shrink `tasks`) — one-shot notice material, not state.
    /// APPENDED last: strictly additive, so old journals never carry it and
    /// old clients skip the unknown tag.
    BackgroundTasks {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tasks: Vec<BackgroundTask>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        closed: Vec<BackgroundTaskClose>,
    },
    /// The agent's own post-turn status line for the whole session (claude
    /// `system/post_turn_summary`; codex has no equivalent frame today) —
    /// the at-a-glance "where things stand" row for rails and dashboards.
    /// LATEST-WINS: each event supersedes the previous one, so a consumer
    /// keeps only the newest and replay's final state is the last event
    /// seen. Strictly additive: old journals never carry it, old clients
    /// skip the unknown tag.
    SessionStatus {
        /// Machine-readable category, verbatim (`review_ready`, …).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        category: Option<String>,
        /// The human-readable one-liner — capped at construction.
        detail: String,
        /// The agent flags this status as waiting on the user.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        needs_action: bool,
    },
}

/// One running background task (a `BackgroundTasks` set member).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackgroundTask {
    /// The agent's own task key — what a `StopTask` sends back verbatim.
    pub id: String,
    /// The agent's lane name, verbatim (`local_bash`, `local_workflow`, …).
    pub task_type: String,
    pub description: String,
    /// The agent's own status word (`running` until a task_updated patch).
    pub status: String,
    /// Driver-stamped epoch ms at first sight — the elapsed display's
    /// anchor, journaled so replay shows honest ages (there is no
    /// start-time on the wire).
    pub started_at_ms: u64,
    /// The workflow's `meta.name` (claude `task_started.workflow_name`,
    /// `local_workflow` lanes only) — the row's title. All the workflow
    /// fields below are additive: old journals/clients skip them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    /// Per-agent progress for a workflow lane (claude `task_progress
    /// .workflow_progress`, folded on state transitions only) — the dot
    /// row. Capped at [`WF_AGENTS_CAP`] keeping the newest; the aggregates
    /// below stay honest beyond the cap.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<WorkflowAgent>,
    /// Agents the wire has reported so far (spawned or queued — the CLI
    /// lists an agent once it exists, so "total" grows as the script runs).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub agents_total: u64,
    /// Agents whose wire state is `done`.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub agents_done: u64,
    /// The tool_use that launched this task (claude `task_started
    /// .tool_use_id`) — driver-internal card binding, never on the wire:
    /// it rides the task identity through the live set and the departed
    /// buffer so the close can land a final line on the launching card.
    #[serde(skip)]
    pub tool_use_id: Option<String>,
}

/// One workflow agent's progress (a `BackgroundTask::agents` member).
/// Deliberately excludes the wire's per-tick fields (tokens-while-running,
/// lastProgressAt): every field here changes only on a state transition, so
/// `PartialEq` gating keeps the journal quiet between them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowAgent {
    /// The workflow's own 1-based agent number, verbatim.
    pub index: u64,
    /// The agent's display label (the script's `label` or prompt head).
    pub label: String,
    /// The wire's state word verbatim (`start`, `done`, …) — rendered
    /// generically, never remapped.
    pub state: String,
    /// Head of the agent's final text, once done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_preview: Option<String>,
}

/// A background task leaving the set with a verdict.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackgroundTaskClose {
    pub id: String,
    pub description: String,
    /// completed | failed | stopped — the task_notification verdict,
    /// verbatim.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// File holding the task's full output, when the agent reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
}

/// Final delivery state of a queued user message.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserMessageState {
    /// The agent consumed it (claude flushed it after the current turn; codex
    /// opened its queued turn or steered it).
    Sent,
    /// The agent never saw it (the driver died with a held queue, or a codex
    /// steer failed for good).
    Dropped,
    /// The user pulled it back before the agent consumed it (the `CancelQueued`
    /// command, honored only while the message is still queued). A cancelled
    /// message never happened: clients REMOVE the bubble entirely — from both
    /// the pending stack and the transcript — and replay agrees (the journaled
    /// `UserMessage{queued}` + this update fold to nothing). APPENDED last so
    /// pre-upgrade clients that don't know the variant still deserialize every
    /// other update.
    Cancelled,
}

/// Commands a client sends into a chat session (WS frames deserialize
/// straight into this).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentCommand {
    Send {
        blocks: Vec<ContentBlock>,
    },
    Permission {
        request_id: String,
        option_id: String,
        /// Where an "always allow" rule is saved (claude destinations:
        /// localSettings / userSettings / projectSettings / session).
        /// None = the agent's suggested default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        destination: Option<String>,
        /// Free text riding the decision. On a deny: the user's reason
        /// (claude: appended to the deny message with interrupt:false;
        /// codex: steered into the running turn after the decline). On a
        /// plan approval: comments (claude: updatedInput.userFeedback/
        /// userComments). Empty/None = the bare decision.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
    Interrupt,
    SetMode {
        mode_id: String,
    },
    SetModel {
        model_id: String,
    },
    /// Reasoning-effort selection (codex: rides the next `turn/start`;
    /// drivers without an effort concept ignore it).
    SetEffort {
        effort_id: String,
    },
    /// Extended-thinking toggle (claude: set_max_thinking_tokens 31999/0).
    SetThinking {
        enabled: bool,
    },
    /// Session ultracode toggle (claude apply_flag_settings {ultracode}) —
    /// xhigh effort + standing dynamic-workflow orchestration.
    SetUltracode {
        enabled: bool,
    },
    /// Answer a QuestionRequest: question id → chosen labels (free text
    /// rides as a label).
    Answer {
        request_id: String,
        answers: std::collections::HashMap<String, Vec<String>>,
    },
    /// Ask the agent for account usage (claude get_usage -> UsageReport).
    GetUsage,
    /// Compact the conversation context (codex thread/compact/start; the
    /// compaction runs as its own turn whose contextCompaction item lands
    /// the "context compacted" notice). Claude compacts via its own
    /// `/compact` slash command — the composer sends that as prompt text,
    /// so this command is a documented no-op there.
    Compact,
    /// Checkpoint rewind (claude rewind_files). Dry-run answers with a
    /// `RewindResult` report; apply restores the files on disk. The
    /// conversation-side fork (--fork-session) is a server concern.
    Rewind {
        user_message_id: String,
        #[serde(default)]
        dry_run: bool,
    },
    /// Move a running tool call to the background (claude background_tasks,
    /// the TUI's Ctrl-B).
    BackgroundTool {
        tool_call_id: String,
    },
    /// Stop a running task — a subagent row id or a background task's
    /// native key (claude stop_task, generic over its task registry).
    StopTask {
        task_id: String,
    },
    /// Ask for the MCP server inventory (-> McpServers).
    GetMcp,
    /// Enable/disable an MCP server for this session (claude mcp_toggle).
    SetMcpEnabled {
        server: String,
        enabled: bool,
    },
    /// Reconnect a failed MCP server (claude mcp_reconnect).
    ReconnectMcp {
        server: String,
    },
    /// Pull back a still-queued user message before the agent consumes it
    /// (`id` = the queued `UserMessage.id`). Honored only while the message is
    /// genuinely queued: both drivers remove it from their held FIFO and emit
    /// `UserMessageUpdate{Cancelled}`. Once the
    /// agent has already taken the message, the driver answers with a `Notice`
    /// instead (it can't be un-said). APPENDED last: strictly additive, so a
    /// pre-upgrade client that never sends it is unaffected.
    CancelQueued {
        id: String,
    },
    /// Promote one queued Codex follow-up into the currently running turn.
    /// The Codex driver maps this to `turn/steer`; if the run ended before
    /// the command arrived, the selected message opens the next turn instead.
    /// Claude has no separate queue-vs-steer control and ignores this command.
    /// APPENDED last so existing command tags and clients remain untouched.
    SteerQueued {
        id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UsageWindow {
    pub label: String,
    /// 0-100 (the get_usage scale; the streamed rate_limit_event uses 0-1).
    pub utilization: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Base64 payload, already client-side downscaled/capped; the journal
    /// stores a placeholder, never the bytes.
    Image {
        media_type: String,
        data: String,
    },
}

/// The text of a user message: its text blocks joined, image blocks dropped —
/// the prompt string both drivers hand their child. Shared so the two `Send`
/// handlers can't drift.
pub fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionUi {
    Chat,
    Term,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModeInfo {
    pub id: String,
    pub label: String,
}

/// One model in the agent's own catalog, with its effort menu when the
/// agent scopes efforts per model (claude initialize.models
/// supportedEffortLevels; codex supportedReasoningEfforts).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    /// One-line blurb ("Opus 4.8 with 1M context · Best for…").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The runtime model id this picker value resolves to (claude reports
    /// `resolvedModel` distinct from `value`; the current-model highlight
    /// matches on either).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
}

/// One question in a QuestionRequest (ACP-ish: id + prompt + options).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<QuestionOption>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// Parse the `options` array of a driver's raw question value into normalized
/// [`QuestionOption`]s. Shared by both question mappers (claude
/// `AskUserQuestion`, codex `item/tool/requestUserInput`) — the option shape is
/// identical even though the enclosing `Question` fields differ.
pub fn question_options(q: &Value) -> Vec<QuestionOption> {
    q["options"]
        .as_array()
        .map(|opts| {
            opts.iter()
                .filter_map(|o| {
                    Some(QuestionOption {
                        label: o["label"].as_str()?.to_string(),
                        description: o["description"].as_str().unwrap_or_default().to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One MCP server row in the /mcp panel.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerInfo {
    pub name: String,
    /// connected | failed | pending | disabled | needs-auth (agent's words).
    pub status: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tools: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// ACP tool-kind taxonomy; drivers map their native tool names into this so
/// the UI picks glyphs/renderers without knowing agent specifics.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    /// A subagent (claude Task tool / task_* status frames).
    Agent,
    Other,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolContent {
    Output {
        text: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    Diff {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        old_text: Option<String>,
        new_text: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    /// Codex-style per-turn aggregated changes (one card, many files).
    Batch { diffs: Vec<ToolContent> },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanStatus,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Todo,
    InProgress,
    Done,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
    pub kind: PermissionOptionKind,
}

/// ACP permission-option taxonomy: claude allow/deny and codex
/// accept/acceptForSession/decline/cancel both map onto these four.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub input_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub duration_ms: u64,
    /// Model context window size, when the agent reports it (codex does).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}
fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// v4-shaped uuid from process entropy; drivers mint these onto outbound
/// message frames (claude checkpoint keys, codex clientUserMessageId).
/// Only transcript-uniqueness matters.
pub fn fresh_uuid() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h =
        |r: std::ops::Range<usize>| -> String { b[r].iter().map(|x| format!("{x:02x}")).collect() };
    format!(
        "{}-{}-{}-{}-{}",
        h(0..4),
        h(4..6),
        h(6..8),
        h(8..10),
        h(10..16)
    )
}

/// Head+tail truncation for stored tool output (the marks.rs philosophy:
/// the start explains what ran, the end holds the verdict). Returns the
/// capped text and whether truncation happened.
pub fn cap_output(text: &str) -> (String, bool) {
    cap_head_tail(text, TOOL_OUTPUT_HEAD, TOOL_OUTPUT_TAIL)
}

/// One-line label truncation with a plain ellipsis — for tool titles and
/// command previews, where the head/tail "[N bytes omitted]" marker of
/// [`cap_head_tail`] would be noise. Respects char boundaries.
pub fn truncate_label(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    format!("{}…", &text[..floor_char_boundary(text, max)])
}

/// Cap every string leaf of a permission/approval input preview so a giant
/// payload can't bloat the journaled/replayed event, THEN backstop the whole
/// value's serialized size ([`PREVIEW_TOTAL_BUDGET`]): leaf-capping bounds
/// each string but not the breadth of a wide array/object, which could still
/// overrun the journal cap and strand the approval. Structure is preserved
/// (the UI renders specific fields); only oversized leaves are
/// head/tail-truncated, and only a preview that is STILL too big after that
/// collapses to a `{_truncated, _omitted_bytes}` marker (the field-reading
/// renderers degrade to "nothing recognized" rather than break). Shared by
/// both drivers (the symmetry invariant): claude's Write/Edit inputs and
/// codex's MCP elicitation tool_params are the same attack surface.
pub fn cap_preview(value: &serde_json::Value) -> serde_json::Value {
    let capped = cap_preview_leaves(value);
    // Wide structures survive leaf-capping — bound the whole thing so a
    // preview can never be the reason a PermissionRequest exceeds the cap.
    let len = serde_json::to_vec(&capped).map(|v| v.len()).unwrap_or(0);
    if len > PREVIEW_TOTAL_BUDGET {
        return serde_json::json!({ "_truncated": true, "_omitted_bytes": len });
    }
    capped
}

fn cap_preview_leaves(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(cap_output(s).0),
        Value::Array(arr) => Value::Array(arr.iter().map(cap_preview_leaves).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), cap_preview_leaves(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub fn cap_head_tail(text: &str, head: usize, tail: usize) -> (String, bool) {
    if text.len() <= head + tail {
        return (text.to_string(), false);
    }
    let head_end = floor_char_boundary(text, head);
    let tail_start = ceil_char_boundary(text, text.len() - tail);
    let omitted = tail_start - head_end;
    (
        format!(
            "{}\n… [{} bytes omitted] …\n{}",
            &text[..head_end],
            omitted,
            &text[tail_start..]
        ),
        true,
    )
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Buffers streaming text deltas so seq numbers (and journal lines) land on
/// readable chunks instead of per-token spam. The pump flushes on size here
/// and on its own 100ms tick; a kind/turn switch flushes implicitly.
pub struct Coalescer {
    buf: String,
    turn_id: String,
    kind: ChunkKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkKind {
    Message,
    Thought,
}

/// Flush-on-size threshold; keeps journal lines sparse without adding
/// visible latency (the timer flush covers the slow-stream case).
pub const COALESCE_BYTES: usize = 2 * 1024;
pub const COALESCE_INTERVAL_MS: u64 = 100;

impl Coalescer {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            turn_id: String::new(),
            kind: ChunkKind::Message,
        }
    }

    /// Add a delta. Returns an event to emit first when the buffered chunk
    /// must flush (kind/turn changed or the size threshold was crossed).
    pub fn push(&mut self, turn_id: &str, kind: ChunkKind, text: &str) -> Option<AgentEvent> {
        let flushed = if !self.buf.is_empty() && (self.turn_id != turn_id || self.kind != kind) {
            self.flush()
        } else {
            None
        };
        self.turn_id = turn_id.to_string();
        self.kind = kind;
        self.buf.push_str(text);
        if flushed.is_some() {
            return flushed;
        }
        if self.buf.len() >= COALESCE_BYTES {
            return self.flush();
        }
        None
    }

    /// Emit whatever is buffered (timer tick, block end, turn end).
    pub fn flush(&mut self) -> Option<AgentEvent> {
        if self.buf.is_empty() {
            return None;
        }
        let text = std::mem::take(&mut self.buf);
        let turn_id = self.turn_id.clone();
        Some(match self.kind {
            ChunkKind::Message => AgentEvent::MessageChunk { turn_id, text },
            ChunkKind::Thought => AgentEvent::ThoughtChunk { turn_id, text },
        })
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

impl Default for Coalescer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_serde_round_trips_with_stable_tags() {
        let ev = AgentEvent::ToolCall {
            id: "t1".into(),
            kind: ToolKind::Execute,
            title: "cargo test".into(),
            locations: vec![],
            status: ToolStatus::InProgress,
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "tool_call");
        assert_eq!(json["kind"], "execute");
        assert_eq!(json["status"], "in_progress");
        let back: AgentEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn command_deserializes_from_ws_frame_shape() {
        // The optional fields are strictly additive: an old client's bare
        // frame must keep deserializing (the wire is a public contract).
        let cmd: AgentCommand = serde_json::from_str(
            r#"{"type":"permission","request_id":"r1","option_id":"allow_once"}"#,
        )
        .unwrap();
        assert_eq!(
            cmd,
            AgentCommand::Permission {
                request_id: "r1".into(),
                option_id: "allow_once".into(),
                destination: None,
                feedback: None,
            }
        );

        let cmd: AgentCommand = serde_json::from_str(
            r#"{"type":"permission","request_id":"r1","option_id":"reject_once","feedback":"use rg"}"#,
        )
        .unwrap();
        assert_eq!(
            cmd,
            AgentCommand::Permission {
                request_id: "r1".into(),
                option_id: "reject_once".into(),
                destination: None,
                feedback: Some("use rg".into()),
            }
        );
    }

    #[test]
    fn cap_output_keeps_head_and_tail() {
        let text = format!(
            "{}{}{}",
            "h".repeat(TOOL_OUTPUT_HEAD),
            "m".repeat(10_000),
            "t".repeat(TOOL_OUTPUT_TAIL)
        );
        let (capped, truncated) = cap_output(&text);
        assert!(truncated);
        assert!(capped.starts_with('h'));
        assert!(capped.ends_with('t'));
        assert!(capped.contains("bytes omitted"));
        assert!(capped.len() < text.len());

        let (small, truncated) = cap_output("short output");
        assert!(!truncated);
        assert_eq!(small, "short output");
    }

    #[test]
    fn cap_preview_bounds_a_wide_structure() {
        // A narrow preview passes through with its shape intact (only leaves
        // are capped) — the common case must not collapse.
        let small = serde_json::json!({"cmd": "ls", "args": ["-la", "/tmp"]});
        assert_eq!(cap_preview(&small), small);

        // A giant single leaf is head/tail-capped but structure survives.
        let big_leaf = serde_json::json!({"script": "x".repeat(200 * 1024)});
        let capped = cap_preview(&big_leaf);
        assert!(capped.get("script").is_some(), "leaf-cap keeps the shape");
        assert!(serde_json::to_vec(&capped).unwrap().len() <= PREVIEW_TOTAL_BUDGET);

        // A WIDE structure (many small leaves) survives leaf-capping yet
        // overruns the budget — it collapses to the marker so the event
        // stays under the journal cap instead of stranding the approval.
        let wide = serde_json::json!({
            "items": (0..20_000)
                .map(|i| serde_json::json!({"k": i, "v": "small"}))
                .collect::<Vec<_>>()
        });
        assert!(serde_json::to_vec(&wide).unwrap().len() > PREVIEW_TOTAL_BUDGET);
        let capped = cap_preview(&wide);
        assert_eq!(capped["_truncated"], serde_json::json!(true));
        assert!(serde_json::to_vec(&capped).unwrap().len() <= PREVIEW_TOTAL_BUDGET);
    }

    #[test]
    fn cap_head_tail_respects_char_boundaries() {
        // Multi-byte chars straddling the cut points must not panic.
        let text = "é".repeat(2000);
        let (capped, truncated) = cap_head_tail(&text, 101, 101);
        assert!(truncated);
        assert!(capped.contains("omitted"));
    }

    #[test]
    fn coalescer_merges_within_turn_and_flushes_on_switch() {
        let mut c = Coalescer::new();
        assert!(c.push("turn1", ChunkKind::Message, "hel").is_none());
        assert!(c.push("turn1", ChunkKind::Message, "lo").is_none());

        // Switching to thought flushes the buffered message first.
        let flushed = c.push("turn1", ChunkKind::Thought, "hmm").unwrap();
        assert_eq!(
            flushed,
            AgentEvent::MessageChunk {
                turn_id: "turn1".into(),
                text: "hello".into()
            }
        );

        let flushed = c.flush().unwrap();
        assert_eq!(
            flushed,
            AgentEvent::ThoughtChunk {
                turn_id: "turn1".into(),
                text: "hmm".into()
            }
        );
        assert!(c.flush().is_none());
    }

    #[test]
    fn coalescer_flushes_on_size_threshold() {
        let mut c = Coalescer::new();
        let big = "x".repeat(COALESCE_BYTES);
        let flushed = c.push("t", ChunkKind::Message, &big).unwrap();
        match flushed {
            AgentEvent::MessageChunk { text, .. } => assert_eq!(text.len(), COALESCE_BYTES),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(c.is_empty());
    }
}
