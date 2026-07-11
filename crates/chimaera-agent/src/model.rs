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
        /// The agent has NOT consumed this message yet (claude queues
        /// mid-turn stdin frames; codex steers/buffers into a running turn).
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
    /// claude dequeues one message per finished turn (and an aborted turn
    /// drops the whole native queue); codex resolves on the steer RPC
    /// answer. Replay is self-correcting — the journal carries the queued
    /// echo and this update through the same reducer, so a queued-then-sent
    /// message renders exactly once and a queued-never-sent one replays in
    /// its final dropped state.
    UserMessageUpdate {
        id: String,
        state: UserMessageState,
    },
}

/// Final delivery state of a queued user message.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserMessageState {
    /// The agent consumed it (claude ran it as the next turn; codex steered
    /// it into the running turn).
    Sent,
    /// The agent never saw it (claude's queue dies with an aborted turn;
    /// a codex steer failed for good).
    Dropped,
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
    /// Stop a running subagent (claude stop_task).
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
