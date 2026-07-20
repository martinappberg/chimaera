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
use std::{collections::HashMap, fmt};

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
/// Bound on tracked plan entries. Same reasoning as [`BG_TASKS_CAP`]: the plan
/// is a glance surface and every `Plan` event carries the whole list, so the
/// list size IS the event size. Claude's task list is agent-created and
/// unbounded upstream; the oldest entries are shed past this.
pub const PLAN_TASKS_CAP: usize = 64;
/// One-line cap for a plan entry's subject / active form / owner.
pub const PLAN_LABEL_MAX: usize = 200;
/// Cap for a plan entry's description (a sentence or two in a panel, not
/// prose). Worst case `PLAN_TASKS_CAP` × this stays far under the journal's
/// 256 KiB per-entry cap, which would otherwise replace the whole `Plan`
/// event with an Error and blank the panel for every client.
pub const PLAN_DESC_MAX: usize = 500;
/// Bound on a plan entry's `blocked_by` list. Ids are tiny ("1", "2"); this
/// only stops a hostile stream from growing the set without end.
pub const PLAN_BLOCKED_CAP: usize = 16;
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
/// Agent-provided composer catalog bound. Codex skills and Claude commands
/// are filesystem/plugin influenced and the complete Init event is journaled.
/// These maxima keep even a fully saturated catalog comfortably below the
/// journal's 256 KiB line cap.
pub const SLASH_COMMANDS_CAP: usize = 64;
pub const SLASH_NAME_MAX: usize = 128;
pub const SLASH_DESCRIPTION_MAX: usize = 320;
pub const SKILL_PATH_MAX: usize = 1024;

// AgentCommand ingress budgets. These are enforced again by ChatManager
// immediately before enqueue, so non-WebSocket callers cannot bypass them.
// The browser's current legitimate maximum is one text block, eight selected
// skills, and four images.
pub const COMMAND_BLOCKS_MAX: usize = 13;
pub const COMMAND_IMAGES_MAX: usize = 4;
pub const COMMAND_SKILLS_MAX: usize = 8;
pub const COMMAND_TEXT_BLOCK_MAX: usize = 256 * 1024;
pub const COMMAND_TEXT_TOTAL_MAX: usize = 256 * 1024;
pub const COMMAND_IMAGE_BASE64_MAX: usize = 2 * 1024 * 1024;
pub const COMMAND_IMAGE_BASE64_TOTAL_MAX: usize = 8 * 1024 * 1024;
pub const COMMAND_MEDIA_TYPE_MAX: usize = 64;
pub const COMMAND_ID_MAX: usize = 1024;
pub const COMMAND_SELECTOR_MAX: usize = 256;
pub const COMMAND_DESTINATION_MAX: usize = 64;
pub const COMMAND_FEEDBACK_MAX: usize = 64 * 1024;
pub const COMMAND_ANSWER_QUESTIONS_MAX: usize = 16;
pub const COMMAND_ANSWER_CHOICES_MAX: usize = 16;
pub const COMMAND_ANSWER_VALUE_MAX: usize = 16 * 1024;
pub const COMMAND_ANSWER_TOTAL_MAX: usize = 64 * 1024;
pub const COMMAND_MCP_SERVER_MAX: usize = 512;

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
        /// This tool owns process-scoped work that may outlive the parent
        /// turn. Codex collab agents are real child threads and keep sending
        /// frames after the parent answers; clients must not reconcile their
        /// rows at the parent's turn boundary. Additive/false by default so
        /// old journals and ordinary tools keep their existing semantics.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        cross_turn: bool,
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
    /// applied.{effort,ultracode}; codex thread open/settings read-backs).
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
    /// The agent is summarizing its conversation to reclaim context. This is
    /// a real, sometimes-long lifecycle rather than a transcript notice:
    /// claude reports `system/status` compacting + compact_result/boundary,
    /// while codex reports a contextCompaction item started/completed pair.
    /// Journal it so reconnect/replay cannot turn active compaction back into
    /// an unexplained "working" spinner.
    ContextCompaction {
        phase: CompactionPhase,
        /// Claude's compact_boundary reports how much context was summarized.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pre_tokens: Option<u64>,
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
    /// Boundary between a copied source-journal prefix and the destination
    /// session's own events. A portable handoff's old native ids are display
    /// history only; consumers use this marker to keep them from becoming
    /// actionable rewind/fork points in the fresh agent conversation.
    Forked {
        source_agent: String,
        source_seq: u64,
        native: bool,
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
    /// Server-owned first turn for a transcript fork. `blocks` is the
    /// canonical handoff the destination agent receives; `display_text` is
    /// the compact user row journaled in its place so the copied transcript
    /// is not duplicated into one enormous bubble. The variant cannot be
    /// deserialized from the public WS wire — only trusted server glue may
    /// construct it.
    #[serde(skip_deserializing)]
    PrimeFork {
        blocks: Vec<ContentBlock>,
        display_text: String,
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
    /// Reasoning-effort selection (codex: updates thread settings, with the
    /// next `turn/start` as a compatibility fallback; drivers without an
    /// effort concept ignore it).
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
        answers: HashMap<String, Vec<String>>,
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

/// A client command exceeded one of the daemon's bounded-ingress budgets.
/// Kept separate from driver availability errors so transports can report a
/// precise one-command refusal without terminating a healthy agent session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandValidationError {
    message: String,
}

impl CommandValidationError {
    fn limit(field: &str, actual: usize, max: usize) -> Self {
        Self {
            message: format!("{field} exceeds {max} byte/item limit (got {actual})"),
        }
    }
}

impl fmt::Display for CommandValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandValidationError {}

fn check_command_len(field: &str, actual: usize, max: usize) -> Result<(), CommandValidationError> {
    if actual > max {
        Err(CommandValidationError::limit(field, actual, max))
    } else {
        Ok(())
    }
}

impl AgentCommand {
    /// Validate every client-controlled collection and string before this
    /// command enters the driver's bounded channel. WebSocket framing has its
    /// own earlier byte ceiling; this remains authoritative for programmatic
    /// callers such as the workspace Mastermind MCP.
    pub fn validate_ingress(&self) -> Result<(), CommandValidationError> {
        match self {
            Self::Send { blocks } | Self::PrimeFork { blocks, .. } => {
                check_command_len("send blocks", blocks.len(), COMMAND_BLOCKS_MAX)?;
                let mut images = 0usize;
                let mut skills = 0usize;
                let mut text_total = 0usize;
                let mut image_total = 0usize;
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            check_command_len("text block", text.len(), COMMAND_TEXT_BLOCK_MAX)?;
                            text_total = text_total.saturating_add(text.len());
                        }
                        ContentBlock::Image { media_type, data } => {
                            images = images.saturating_add(1);
                            check_command_len(
                                "image media_type",
                                media_type.len(),
                                COMMAND_MEDIA_TYPE_MAX,
                            )?;
                            check_command_len(
                                "image base64",
                                data.len(),
                                COMMAND_IMAGE_BASE64_MAX,
                            )?;
                            image_total = image_total.saturating_add(data.len());
                        }
                        ContentBlock::Skill { name, path } => {
                            skills = skills.saturating_add(1);
                            check_command_len("skill name", name.len(), SLASH_NAME_MAX)?;
                            check_command_len("skill path", path.len(), SKILL_PATH_MAX)?;
                        }
                    }
                }
                check_command_len("send images", images, COMMAND_IMAGES_MAX)?;
                check_command_len("send skills", skills, COMMAND_SKILLS_MAX)?;
                check_command_len("send text", text_total, COMMAND_TEXT_TOTAL_MAX)?;
                check_command_len(
                    "send image base64",
                    image_total,
                    COMMAND_IMAGE_BASE64_TOTAL_MAX,
                )?;
                if let Self::PrimeFork { display_text, .. } = self {
                    check_command_len(
                        "fork display text",
                        display_text.len(),
                        COMMAND_TEXT_BLOCK_MAX,
                    )?;
                }
            }
            Self::Permission {
                request_id,
                option_id,
                destination,
                feedback,
            } => {
                check_command_len("request_id", request_id.len(), COMMAND_ID_MAX)?;
                check_command_len("option_id", option_id.len(), COMMAND_SELECTOR_MAX)?;
                if let Some(destination) = destination {
                    check_command_len(
                        "permission destination",
                        destination.len(),
                        COMMAND_DESTINATION_MAX,
                    )?;
                }
                if let Some(feedback) = feedback {
                    check_command_len("permission feedback", feedback.len(), COMMAND_FEEDBACK_MAX)?;
                }
            }
            Self::SetMode { mode_id } => {
                check_command_len("mode_id", mode_id.len(), COMMAND_SELECTOR_MAX)?;
            }
            Self::SetModel { model_id } => {
                check_command_len("model_id", model_id.len(), COMMAND_SELECTOR_MAX)?;
            }
            Self::SetEffort { effort_id } => {
                check_command_len("effort_id", effort_id.len(), COMMAND_SELECTOR_MAX)?;
            }
            Self::Answer {
                request_id,
                answers,
            } => {
                check_command_len("request_id", request_id.len(), COMMAND_ID_MAX)?;
                check_command_len(
                    "answer questions",
                    answers.len(),
                    COMMAND_ANSWER_QUESTIONS_MAX,
                )?;
                let mut answer_total = 0usize;
                for (question_id, choices) in answers {
                    check_command_len("question id", question_id.len(), COMMAND_ID_MAX)?;
                    check_command_len("answer choices", choices.len(), COMMAND_ANSWER_CHOICES_MAX)?;
                    for choice in choices {
                        check_command_len("answer value", choice.len(), COMMAND_ANSWER_VALUE_MAX)?;
                        answer_total = answer_total.saturating_add(choice.len());
                    }
                }
                check_command_len("answer text", answer_total, COMMAND_ANSWER_TOTAL_MAX)?;
            }
            Self::Rewind {
                user_message_id, ..
            } => {
                check_command_len("user_message_id", user_message_id.len(), COMMAND_ID_MAX)?;
            }
            Self::BackgroundTool { tool_call_id } => {
                check_command_len("tool_call_id", tool_call_id.len(), COMMAND_ID_MAX)?;
            }
            Self::StopTask { task_id } => {
                check_command_len("task_id", task_id.len(), COMMAND_ID_MAX)?;
            }
            Self::SetMcpEnabled { server, .. } | Self::ReconnectMcp { server } => {
                check_command_len("MCP server", server.len(), COMMAND_MCP_SERVER_MAX)?;
            }
            Self::CancelQueued { id } | Self::SteerQueued { id } => {
                check_command_len("queued message id", id.len(), COMMAND_ID_MAX)?;
            }
            Self::Interrupt
            | Self::SetThinking { .. }
            | Self::SetUltracode { .. }
            | Self::GetUsage
            | Self::Compact
            | Self::GetMcp => {}
        }
        Ok(())
    }

    /// Heap bytes this command can leave resident while waiting for the agent
    /// to consume it. Only sends retain bulk payloads; the other variants are
    /// independently leaf-capped and live only in the bounded command channel.
    pub fn retained_send_bytes(&self) -> Option<usize> {
        let (blocks, display_bytes) = match self {
            Self::Send { blocks } => (blocks, 0),
            Self::PrimeFork {
                blocks,
                display_text,
            } => (blocks, display_text.len()),
            _ => return None,
        };
        let mut bytes = std::mem::size_of_val(blocks.as_slice()) + display_bytes;
        for block in blocks {
            bytes = bytes.saturating_add(match block {
                ContentBlock::Text { text } => text.len(),
                ContentBlock::Image { media_type, data } => {
                    media_type.len().saturating_add(data.len())
                }
                ContentBlock::Skill { name, path } => name.len().saturating_add(path.len()),
            });
        }
        Some(bytes)
    }
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
    /// Codex `skills/list` selection. The text block retains the user's
    /// visible `/skill-name` token; this companion block is the app-server's
    /// native invocation signal.
    Skill {
        name: String,
        path: String,
    },
}

/// The text of a user message: its text blocks joined, non-text blocks dropped —
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionPhase {
    Started,
    Completed,
    Failed,
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
    /// Present for Codex `skills/list` rows. Additive on the daemon↔UI wire;
    /// Claude's own slash catalog leaves it absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_path: Option<String>,
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

/// One plan row. `content` is the display text every source fills (claude's
/// task `subject`, a TodoWrite `content`, a codex plan `step`); everything
/// below it is richness only claude's `Task*` family carries, so it is all
/// optional-and-omitted. That is what keeps older claude CLIs (TodoWrite) and
/// codex serializing byte-identically to before these fields existed.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanStatus,
    /// The agent's own task id ("1", "2", …) — the key `TaskUpdate` addresses
    /// and the id `blocked_by` refers to. Absent for TodoWrite/codex plans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Present-continuous form ("Running tests") shown while in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    /// What the task actually entails — the detail behind the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Owning agent name, once claimed. The multi-agent "who has this" signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Ids of still-open tasks that must finish first. Orthogonal to `status`
    /// (a blocked task is still `Todo`) — the client derives its own glyph, so
    /// "not started" and "can't start" stop looking identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    #[default]
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
    use rand::Rng;
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
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
            cross_turn: false,
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "tool_call");
        assert_eq!(json["kind"], "execute");
        assert_eq!(json["status"], "in_progress");
        assert!(
            json.get("cross_turn").is_none(),
            "the additive false default stays off old wire shapes"
        );
        let back: AgentEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, ev);

        let detached = AgentEvent::ToolCall {
            id: "agent:child".into(),
            kind: ToolKind::Agent,
            title: "Agent: child".into(),
            locations: vec![],
            status: ToolStatus::InProgress,
            cross_turn: true,
        };
        assert_eq!(serde_json::to_value(&detached).unwrap()["cross_turn"], true);
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

        assert!(
            serde_json::from_str::<AgentCommand>(
                r#"{"type":"prime_fork","blocks":[],"display_text":"hide me"}"#,
            )
            .is_err(),
            "the server-owned fork primer must not be callable from the public WS wire"
        );
    }

    #[test]
    fn command_ingress_accepts_the_browser_send_budget() {
        let mut blocks = vec![ContentBlock::Text {
            text: "x".repeat(COMMAND_TEXT_TOTAL_MAX),
        }];
        blocks.extend((0..COMMAND_IMAGES_MAX).map(|_| ContentBlock::Image {
            media_type: "image/png".to_string(),
            data: "x".repeat(COMMAND_IMAGE_BASE64_MAX),
        }));
        blocks.extend((0..COMMAND_SKILLS_MAX).map(|i| ContentBlock::Skill {
            name: format!("skill-{i}"),
            path: format!("/skills/{i}/SKILL.md"),
        }));
        assert!(AgentCommand::Send { blocks }.validate_ingress().is_ok());
    }

    #[test]
    fn command_ingress_rejects_oversized_send_fields_and_aggregates() {
        let oversized_image = AgentCommand::Send {
            blocks: vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "x".repeat(COMMAND_IMAGE_BASE64_MAX + 1),
            }],
        };
        assert!(oversized_image.validate_ingress().is_err());

        let too_many_blocks = AgentCommand::Send {
            blocks: (0..COMMAND_BLOCKS_MAX + 1)
                .map(|_| ContentBlock::Text {
                    text: String::new(),
                })
                .collect(),
        };
        assert!(too_many_blocks.validate_ingress().is_err());

        let aggregate_text = AgentCommand::Send {
            blocks: vec![
                ContentBlock::Text {
                    text: "x".repeat(COMMAND_TEXT_TOTAL_MAX / 2 + 1),
                },
                ContentBlock::Text {
                    text: "x".repeat(COMMAND_TEXT_TOTAL_MAX / 2),
                },
            ],
        };
        assert!(aggregate_text.validate_ingress().is_err());

        let too_many_skills = AgentCommand::Send {
            blocks: (0..COMMAND_SKILLS_MAX + 1)
                .map(|i| ContentBlock::Skill {
                    name: format!("skill-{i}"),
                    path: format!("/skills/{i}/SKILL.md"),
                })
                .collect(),
        };
        assert!(too_many_skills.validate_ingress().is_err());

        let oversized_skill_path = AgentCommand::Send {
            blocks: vec![ContentBlock::Skill {
                name: "skill".to_string(),
                path: "x".repeat(SKILL_PATH_MAX + 1),
            }],
        };
        assert!(oversized_skill_path.validate_ingress().is_err());
    }

    #[test]
    fn command_ingress_bounds_non_send_collections_and_strings() {
        let oversized_selector = AgentCommand::SetModel {
            model_id: "x".repeat(COMMAND_SELECTOR_MAX + 1),
        };
        assert!(oversized_selector.validate_ingress().is_err());

        let answers = (0..COMMAND_ANSWER_QUESTIONS_MAX + 1)
            .map(|i| (format!("q{i}"), vec!["yes".to_string()]))
            .collect();
        let too_many_answers = AgentCommand::Answer {
            request_id: "r".to_string(),
            answers,
        };
        assert!(too_many_answers.validate_ingress().is_err());
    }

    #[test]
    fn retained_send_bytes_counts_bulk_payload_and_excludes_controls() {
        let command = AgentCommand::Send {
            blocks: vec![
                ContentBlock::Text {
                    text: "hello".to_string(),
                },
                ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: "base64".to_string(),
                },
                ContentBlock::Skill {
                    name: "review".to_string(),
                    path: "/skills/review.md".to_string(),
                },
            ],
        };
        assert!(command.retained_send_bytes().unwrap() >= 5 + 9 + 6 + 6 + 17);
        assert_eq!(AgentCommand::Interrupt.retained_send_bytes(), None);
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
