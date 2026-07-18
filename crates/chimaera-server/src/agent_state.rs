//! The pure agent-session state core: the [`AgentKind`] / [`AgentState`]
//! enums, the [`AgentRecord`] wrapper, and the pure hook->state and
//! title-resolution logic. No transport, filesystem, or `AppState` — the leaf
//! that both the chat glue and the PTY glue depend on, so neither has to
//! depend on the other (this is what breaks the old agents<->chat cycle).

use std::path::PathBuf;

/// Which agent CLI a kind-"agent" session runs. The id doubles as the binary
/// name probed on the login-shell PATH. Hook-driven attention state exists
/// only for Claude; other agents surface `agent_state: "unknown"` (the muted
/// dot in the UI) until their integrations land.
///
/// Gemini CLI's Google sign-in was retired for individual accounts on
/// 2026-06-18 (API-key auth still works); Google's successor is the
/// Antigravity CLI (binary `agy`). Both stay in the catalog — the rows
/// carry official docs links, not editorials.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AgentKind {
    Claude,
    Codex,
    Gemini,
    Antigravity,
}

impl AgentKind {
    /// The launcher catalog: what the popover offers and detection probes.
    pub(crate) const ALL: [AgentKind; 4] = [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Gemini,
        AgentKind::Antigravity,
    ];

    /// Stable id — also the binary name (`claude`, `codex`, `agy`, `gemini`).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Antigravity => "agy",
            AgentKind::Gemini => "gemini",
        }
    }

    /// Human product name for launcher rows and error messages.
    pub(crate) fn product_name(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::Antigravity => "Antigravity CLI",
            AgentKind::Gemini => "Gemini CLI",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<AgentKind> {
        AgentKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Whether this agent can run as a structured chat session — a driver over
    /// stream-json (claude) / app-server (codex), not a PTY TUI. The single
    /// predicate the whole server routes through, so adding the next
    /// structured agent is a one-line change here rather than five scattered
    /// `matches!` edits. (The client-facing `AgentInfo.chatCapable` the
    /// launcher computes is this AND path-ok AND !outdated — a composite the
    /// UI consumes; this is just the protocol-capability half.)
    pub(crate) fn chat_capable(self) -> bool {
        matches!(self, AgentKind::Claude | AgentKind::Codex)
    }
}

/// Attention state of an agent session, derived from Claude Code hook events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentState {
    Running,
    NeedsPermission,
    IdlePrompt,
    Finished,
    Errored,
    RateLimited,
    /// No hook data yet (freshly spawned, or events lost).
    Unknown,
}

impl AgentState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            AgentState::Running => "running",
            AgentState::NeedsPermission => "needs_permission",
            AgentState::IdlePrompt => "idle_prompt",
            AgentState::Finished => "finished",
            AgentState::Errored => "errored",
            AgentState::RateLimited => "rate_limited",
            AgentState::Unknown => "unknown",
        }
    }
}

/// Cap on the per-session touched-files list.
const FILES_TOUCHED_CAP: usize = 100;

/// Cap on the live-subagent set per session — bounded memory: a runaway
/// fan-out must not bloat the record or every events-bus snapshot.
const SUBAGENTS_CAP: usize = 32;

/// Cap (chars) on the small derived strings kept on the record (`now_line`,
/// subagent labels, usage model name): they ride every snapshot.
const SMALL_STRING_MAX: usize = 80;

/// One live subagent of a claude TUI session, from `SubagentStart` hook
/// payloads. Serialized straight onto the session row (`subagents[]`) —
/// additive wire shape, don't rename fields.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct SubagentInfo {
    pub(crate) id: String,
    pub(crate) label: String,
    /// ms since the Unix epoch, stamped at ingest (same clock as the PTY's
    /// `last_output_at`).
    pub(crate) started_at: u64,
}

/// Statusline-heartbeat telemetry for a claude TUI session. Values are
/// quantized at ingest (whole percent, whole cents) so micro-changes between
/// heartbeats don't defeat the events-bus snapshot dedupe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentUsage {
    pub(crate) model: Option<String>,
    /// Context-window use, whole percent 0–100.
    pub(crate) context_pct: Option<u8>,
    /// Session cost, whole US cents (serialized as dollars).
    pub(crate) cost_cents: Option<u64>,
}

impl AgentUsage {
    pub(crate) fn is_empty(&self) -> bool {
        self.model.is_none() && self.context_pct.is_none() && self.cost_cents.is_none()
    }

    /// The wire shape: `{model, context_pct, cost_usd}`, every field optional.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "model": self.model,
            "context_pct": self.context_pct,
            "cost_usd": self.cost_cents.map(|c| c as f64 / 100.0),
        })
    }
}

/// Server-side wrapper state for one agent session.
#[derive(Clone, Debug)]
pub(crate) struct AgentRecord {
    /// Per-session secret embedded in the hook URL; authorizes ingestion.
    pub(crate) key: String,
    /// Which agent CLI this session runs (the UI glyphs sessions by it).
    pub(crate) kind: AgentKind,
    pub(crate) state: AgentState,
    /// Latest transcript path reported by any hook payload.
    pub(crate) transcript_path: Option<PathBuf>,
    /// Latest `customTitle` transcript record (wins over `ai_title`).
    pub(crate) custom_title: Option<String>,
    /// Latest `{"type":"ai-title"}` transcript record.
    pub(crate) ai_title: Option<String>,
    /// First prompt seen in a `UserPromptSubmit` hook payload; provisional
    /// display name until a real title exists.
    pub(crate) first_prompt: Option<String>,
    /// Native conversation id this session resumed, when it did. Claude forks
    /// a NEW session id on resume, while Codex continues the same thread id;
    /// Recents needs the ancestor to recognize the live continuation.
    pub(crate) resumed_from: Option<String>,
    /// Files this session has written (PostToolUse hooks for file-writing
    /// tools): ordered, de-duplicated, most recently touched last, capped at
    /// [`FILES_TOUCHED_CAP`]. Never cleared — the list lives as long as the
    /// session.
    pub(crate) files_touched: Vec<String>,
    /// Whether the compute-session context (`compute::agent_context`) has
    /// been delivered to this session via a hook response. Once per record:
    /// the context lands in the conversation itself, so later hooks — and a
    /// view switch, which respawns the process but keeps the record and the
    /// conversation — must not repeat it. Never true off-cluster.
    pub(crate) compute_ctx_delivered: bool,
    /// Live subagents (SubagentStart/Stop hooks), capped at
    /// [`SUBAGENTS_CAP`]; cleared when the turn ends (Stop) or the session
    /// exits (the record dies with it).
    pub(crate) subagents: Vec<SubagentInfo>,
    /// One-line "what it's doing now" from the most recent hook event
    /// (claude TUIs only — chat rows derive richer state from the journal);
    /// replaced per event, cleared on Stop/exit.
    pub(crate) now_line: Option<String>,
    /// Latest statusline-heartbeat telemetry (claude TUIs only), quantized
    /// at ingest.
    pub(crate) usage: Option<AgentUsage>,
}

impl AgentRecord {
    pub(crate) fn new(key: String, kind: AgentKind) -> Self {
        AgentRecord {
            key,
            kind,
            state: AgentState::Unknown,
            transcript_path: None,
            custom_title: None,
            ai_title: None,
            first_prompt: None,
            resumed_from: None,
            files_touched: Vec::new(),
            compute_ctx_delivered: false,
            subagents: Vec::new(),
            now_line: None,
            usage: None,
        }
    }

    /// Record a subagent start: an already-known id refreshes its label in
    /// place; past the cap the OLDEST entry falls off (a stuck stale row
    /// must never block the live one). Returns whether anything changed.
    pub(crate) fn subagent_started(&mut self, id: &str, label: &str, started_at: u64) -> bool {
        let label: String = label.chars().take(SMALL_STRING_MAX).collect();
        if let Some(existing) = self.subagents.iter_mut().find(|s| s.id == id) {
            if existing.label == label {
                return false;
            }
            existing.label = label;
            return true;
        }
        if self.subagents.len() >= SUBAGENTS_CAP {
            self.subagents.remove(0);
        }
        self.subagents.push(SubagentInfo {
            id: id.to_string(),
            label,
            started_at,
        });
        true
    }

    /// Drop a subagent by id; returns whether it was present.
    pub(crate) fn subagent_stopped(&mut self, id: &str) -> bool {
        let before = self.subagents.len();
        self.subagents.retain(|s| s.id != id);
        self.subagents.len() != before
    }

    /// Record a file write: a re-touched path moves to the end (newest last),
    /// a new one appends, and the oldest entries fall off past the cap.
    /// Returns whether the list changed (re-touching the newest is a no-op).
    pub(crate) fn touch_file(&mut self, path: &str) -> bool {
        if self.files_touched.last().is_some_and(|last| last == path) {
            return false;
        }
        if let Some(pos) = self.files_touched.iter().position(|p| p == path) {
            self.files_touched.remove(pos);
        }
        self.files_touched.push(path.to_string());
        if self.files_touched.len() > FILES_TOUCHED_CAP {
            self.files_touched.remove(0);
        }
        true
    }

    /// Display title: latest customTitle wins over latest aiTitle.
    pub(crate) fn title(&self) -> Option<&str> {
        self.custom_title.as_deref().or(self.ai_title.as_deref())
    }

    /// Display name, naming rule zero for agents: customTitle > the agent's
    /// own live terminal title > aiTitle > first prompt (truncated) > the
    /// agent binary name ("claude"/"codex"/"gemini").
    /// The OSC title is how `/rename` (and any in-TUI naming) propagates
    /// instantly without depending on semi-documented transcript records —
    /// found in the field when a `/rename git` left the row on its
    /// first-prompt name while the subtitle already showed "✳ git".
    pub(crate) fn display_name(&self, osc_title: Option<&str>) -> String {
        self.custom_title
            .clone()
            .or_else(|| osc_title.and_then(cleaned_osc_title))
            .or_else(|| self.ai_title.clone())
            .or_else(|| self.first_prompt.as_deref().map(truncate_prompt))
            .unwrap_or_else(|| self.kind.as_str().to_string())
    }

    /// The session id `claude --resume` accepts: the transcript filename stem
    /// (hooks report the transcript as `<store>/<cwd-key>/<session-id>.jsonl`).
    pub(crate) fn resume_id(&self) -> Option<String> {
        self.transcript_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .map(str::to_string)
    }
}

/// Claude's terminal title, stripped of its spark prefix; `None` for empty
/// or default titles that carry no session-specific information.
fn cleaned_osc_title(raw: &str) -> Option<String> {
    let t = raw
        .trim_start_matches(['✳', '✻', '*', '⏺'])
        .trim()
        .to_string();
    match t.as_str() {
        "" | "Claude Code" | "claude" => None,
        _ => Some(t),
    }
}

/// Provisional-title cap (chars); truncation backs off to a word boundary.
const PROMPT_TITLE_MAX: usize = 60;

/// Collapse whitespace and truncate to ~60 chars at a word boundary,
/// appending an ellipsis when anything was cut. Shared with the launcher's
/// resumable-session titles so both surfaces truncate identically.
pub(crate) fn truncate_prompt(prompt: &str) -> String {
    let flat = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= PROMPT_TITLE_MAX {
        return flat;
    }
    let mut out = String::new();
    for word in flat.split(' ') {
        let sep = usize::from(!out.is_empty());
        if out.chars().count() + sep + word.chars().count() > PROMPT_TITLE_MAX {
            break;
        }
        if sep == 1 {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        // A single giant word: hard-cut it.
        out = flat.chars().take(PROMPT_TITLE_MAX).collect();
    }
    out.push('…');
    out
}

/// The file path a PostToolUse payload touched, if its tool writes files.
/// Field names verified against real hook payloads: Write/Edit (and
/// MultiEdit, same tool family) carry `tool_input.file_path`; NotebookEdit
/// carries `tool_input.notebook_path`.
pub(crate) fn touched_file(payload: &serde_json::Value) -> Option<&str> {
    let field = match payload.get("tool_name")?.as_str()? {
        "Write" | "Edit" | "MultiEdit" => "file_path",
        "NotebookEdit" => "notebook_path",
        _ => return None,
    };
    payload
        .get("tool_input")?
        .get(field)?
        .as_str()
        .filter(|path| !path.is_empty())
}

/// The subagent identity in a `SubagentStart`/`SubagentStop` payload.
/// Verified shape (claude 2.1.20x): `agent_id` + `agent_type`; parsed
/// liberally (camelCase accepted, label optional) — `None` means an unknown
/// shape the caller logs and drops.
pub(crate) fn subagent_identity(payload: &serde_json::Value) -> Option<(&str, &str)> {
    let id = payload
        .get("agent_id")
        .or_else(|| payload.get("agentId"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?;
    let label = payload
        .get("agent_type")
        .or_else(|| payload.get("agentType"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("subagent");
    Some((id, label))
}

/// The `now_line` update a hook event implies: `Some(Some(line))` replaces
/// it, `Some(None)` clears it (a turn boundary — the old line is stale),
/// `None` keeps the current one. Tool events yield the task's one-line
/// answer to "what is it doing": "edited foo.rs" beats "ran Edit".
pub(crate) fn now_line_update(event: &str, payload: &serde_json::Value) -> Option<Option<String>> {
    let tool = || {
        payload
            .get("tool_name")
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty())
    };
    let line = |s: String| Some(Some(s.chars().take(SMALL_STRING_MAX).collect()));
    match event {
        "PreToolUse" => match touched_file(payload).and_then(file_basename) {
            Some(name) => line(format!("editing {name}")),
            None => tool().and_then(|t| line(format!("running {t}"))),
        },
        "PostToolUse" => match touched_file(payload).and_then(file_basename) {
            Some(name) => line(format!("edited {name}")),
            None => tool().and_then(|t| line(format!("ran {t}"))),
        },
        "SubagentStart" => {
            let (_, label) = subagent_identity(payload)?;
            line(format!("delegating to {label}"))
        }
        // A new prompt or a turn/session end: whatever the line said is over.
        "UserPromptSubmit" | "Stop" | "StopFailure" | "SessionEnd" => Some(None),
        _ => None,
    }
}

/// The path's final component, for the compact now-line.
fn file_basename(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Parse a statusline-heartbeat payload into quantized usage. Liberal on
/// purpose — every field optional, unknown shapes yield an empty usage the
/// caller drops. Field names per the claude statusline stdin JSON
/// (`model.display_name`, `context_window.used_percentage`,
/// `cost.total_cost_usd`), with a tokens-ratio fallback for the percentage.
pub(crate) fn statusline_usage(payload: &serde_json::Value) -> AgentUsage {
    let model = payload
        .get("model")
        .and_then(|m| {
            m.get("display_name")
                .or_else(|| m.get("id"))
                .and_then(|v| v.as_str())
        })
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(SMALL_STRING_MAX).collect());
    let window = payload.get("context_window");
    let context_pct = window
        .and_then(|w| {
            w.get("used_percentage")
                .or_else(|| w.get("used_pct"))
                .and_then(|v| v.as_f64())
        })
        .or_else(|| {
            // Some builds report tokens, not a percentage.
            let used = window?.get("used_tokens")?.as_f64()?;
            let max = window?.get("max_tokens")?.as_f64()?;
            (max > 0.0).then(|| used / max * 100.0)
        })
        .filter(|p| p.is_finite())
        .map(|p| p.round().clamp(0.0, 100.0) as u8);
    let cost_cents = payload
        .get("cost")
        .and_then(|c| c.get("total_cost_usd"))
        .and_then(|v| v.as_f64())
        .filter(|c| c.is_finite() && (0.0..1e9).contains(c))
        .map(|c| (c * 100.0).round() as u64);
    AgentUsage {
        model,
        context_pct,
        cost_cents,
    }
}

/// Map a hook event to the agent state it implies, if any. `SessionEnd`
/// intentionally maps to `None`: the last state is kept, and the PTY exit
/// still reaps the whole session (a closed claude TUI vanishes).
pub(crate) fn map_event(event: &str, payload: &serde_json::Value) -> Option<AgentState> {
    match event {
        "SessionStart" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse"
        | "PostToolUseFailure" | "PostToolBatch" | "SubagentStart" | "SubagentStop"
        | "PreCompact" | "PostCompact" => Some(AgentState::Running),
        "Notification" => {
            let kind = payload
                .get("notification_type")
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let message = payload
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if kind == "permission_prompt" || message.contains("permission") {
                Some(AgentState::NeedsPermission)
            } else if kind == "idle_prompt" || message.contains("waiting for your input") {
                Some(AgentState::IdlePrompt)
            } else {
                None
            }
        }
        "Stop" => Some(AgentState::Finished),
        "StopFailure" => {
            let error_type = payload
                .get("error_type")
                .and_then(|t| t.as_str())
                .unwrap_or("");
            if matches!(error_type, "rate_limit" | "overloaded") {
                Some(AgentState::RateLimited)
            } else {
                Some(AgentState::Errored)
            }
        }
        _ => None,
    }
}

/// A stale "needs you" state that fresh transcript output invalidates. New
/// conversation content means the agent is actively producing — it is running,
/// not blocked on the user — so a `needs you` that no hook flipped back is
/// stale and gets cleared. This closes the gap where a long tool-free stretch
/// (streaming prose, or an auto-continuing agent that fires no
/// `UserPromptSubmit`) leaves the session pinned at `idle_prompt`/`errored`.
///
/// `NeedsPermission` and `Finished` are deliberately excluded: a pending
/// permission appends nothing to the transcript while it blocks, and a
/// finished run is meant to sit silently until its own next-turn hook.
pub(crate) fn cleared_by_output(state: AgentState) -> Option<AgentState> {
    matches!(state, AgentState::IdlePrompt | AgentState::Errored).then_some(AgentState::Running)
}

/// Apply one transcript line to the record's title fields. Returns whether
/// the effective title changed. Recognizes `{"type":"ai-title","aiTitle":..}`
/// records and any record carrying a top-level `customTitle` (string sets it,
/// null clears it). Shared with the launcher's resumable-session scan so the
/// live tail and the resume list resolve titles identically.
pub(crate) fn apply_title_line(line: &str, record: &mut AgentRecord) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    let before = record.title().map(str::to_string);
    match value.get("customTitle") {
        Some(serde_json::Value::String(title)) => record.custom_title = Some(title.clone()),
        Some(serde_json::Value::Null) => record.custom_title = None,
        _ => {}
    }
    if value.get("type").and_then(|t| t.as_str()) == Some("ai-title") {
        if let Some(title) = value.get("aiTitle").and_then(|t| t.as_str()) {
            record.ai_title = Some(title.to_string());
        }
    }
    record.title() != before.as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn map_event_covers_contract() {
        let empty = json!({});
        assert_eq!(map_event("SessionStart", &empty), Some(AgentState::Running));
        assert_eq!(
            map_event("UserPromptSubmit", &empty),
            Some(AgentState::Running)
        );
        assert_eq!(map_event("PreToolUse", &empty), Some(AgentState::Running));
        assert_eq!(map_event("Stop", &empty), Some(AgentState::Finished));
        assert_eq!(
            map_event("StopFailure", &json!({"error_type": "rate_limit"})),
            Some(AgentState::RateLimited)
        );
        assert_eq!(
            map_event("StopFailure", &json!({"error_type": "overloaded"})),
            Some(AgentState::RateLimited)
        );
        assert_eq!(
            map_event("StopFailure", &json!({"error_type": "server_error"})),
            Some(AgentState::Errored)
        );
        assert_eq!(map_event("StopFailure", &empty), Some(AgentState::Errored));
        assert_eq!(
            map_event(
                "Notification",
                &json!({"notification_type": "permission_prompt"})
            ),
            Some(AgentState::NeedsPermission)
        );
        assert_eq!(
            map_event("Notification", &json!({"notification_type": "idle_prompt"})),
            Some(AgentState::IdlePrompt)
        );
        assert_eq!(
            map_event(
                "Notification",
                &json!({"message": "Claude needs your permission to use Bash"})
            ),
            Some(AgentState::NeedsPermission)
        );
        assert_eq!(
            map_event(
                "Notification",
                &json!({"notification_type": "auth_success"})
            ),
            None
        );
        assert_eq!(map_event("SessionEnd", &empty), None);
        assert_eq!(map_event("SomethingNew", &empty), None);
    }

    #[test]
    fn fresh_output_clears_only_stale_waiting_states() {
        // A "needs you" that no hook cleared is stale once the agent resumes
        // writing to the transcript — it is running again.
        assert_eq!(
            cleared_by_output(AgentState::IdlePrompt),
            Some(AgentState::Running)
        );
        assert_eq!(
            cleared_by_output(AgentState::Errored),
            Some(AgentState::Running)
        );
        // A pending permission writes no transcript while it blocks, and a
        // finished run stays silent: neither is cleared by output alone.
        assert_eq!(cleared_by_output(AgentState::NeedsPermission), None);
        assert_eq!(cleared_by_output(AgentState::Finished), None);
        // States that are not "needs you" are left untouched.
        assert_eq!(cleared_by_output(AgentState::Running), None);
        assert_eq!(cleared_by_output(AgentState::RateLimited), None);
        assert_eq!(cleared_by_output(AgentState::Unknown), None);
    }

    #[test]
    fn touched_file_reads_the_right_field_per_tool() {
        // Shaped like real claude 2.1.196 PostToolUse payloads.
        let payload = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": "/w/a.rs", "content": "hi"},
        });
        assert_eq!(touched_file(&payload), Some("/w/a.rs"));
        let payload = json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": "/w/b.rs", "old_string": "x", "new_string": "y"},
        });
        assert_eq!(touched_file(&payload), Some("/w/b.rs"));
        let payload = json!({
            "tool_name": "MultiEdit",
            "tool_input": {"file_path": "/w/c.rs", "edits": []},
        });
        assert_eq!(touched_file(&payload), Some("/w/c.rs"));
        let payload = json!({
            "tool_name": "NotebookEdit",
            "tool_input": {"notebook_path": "/w/d.ipynb", "new_source": ""},
        });
        assert_eq!(touched_file(&payload), Some("/w/d.ipynb"));
        // Non-writing tools, wrong field, empty path, missing input: no touch.
        assert_eq!(
            touched_file(&json!({"tool_name": "Bash", "tool_input": {"command": "ls"}})),
            None
        );
        assert_eq!(
            touched_file(&json!({"tool_name": "Write", "tool_input": {"notebook_path": "/x"}})),
            None
        );
        assert_eq!(
            touched_file(&json!({"tool_name": "Write", "tool_input": {"file_path": ""}})),
            None
        );
        assert_eq!(touched_file(&json!({"tool_name": "Write"})), None);
        assert_eq!(touched_file(&json!({})), None);
    }

    #[test]
    fn touch_file_dedupes_orders_and_caps() {
        let mut record = AgentRecord::new("k".into(), AgentKind::Claude);
        assert!(record.touch_file("/w/a"));
        assert!(record.touch_file("/w/b"));
        // Re-touching the newest entry changes nothing.
        assert!(!record.touch_file("/w/b"));
        assert_eq!(record.files_touched, ["/w/a", "/w/b"]);
        // Re-touching an older entry moves it to the end (newest last).
        assert!(record.touch_file("/w/a"));
        assert_eq!(record.files_touched, ["/w/b", "/w/a"]);
        // The cap drops the oldest entries, keeping the newest 100.
        for i in 0..150 {
            record.touch_file(&format!("/w/f{i}"));
        }
        assert_eq!(record.files_touched.len(), FILES_TOUCHED_CAP);
        assert_eq!(record.files_touched.first().unwrap(), "/w/f50");
        assert_eq!(record.files_touched.last().unwrap(), "/w/f149");
    }

    #[test]
    fn subagent_set_dedupes_caps_and_stops() {
        let mut record = AgentRecord::new("k".into(), AgentKind::Claude);
        assert!(record.subagent_started("a1", "Explore", 10));
        assert!(record.subagent_started("a2", "Plan", 20));
        // Same id, same label: no change. Same id, new label: refreshed.
        assert!(!record.subagent_started("a1", "Explore", 30));
        assert!(record.subagent_started("a1", "Explore v2", 30));
        assert_eq!(record.subagents.len(), 2);
        assert_eq!(record.subagents[0].started_at, 10); // start time kept
                                                        // Stop removes by id; a second stop is a no-op.
        assert!(record.subagent_stopped("a1"));
        assert!(!record.subagent_stopped("a1"));
        assert_eq!(record.subagents.len(), 1);
        // The cap drops the OLDEST entry, never blocks the live one.
        for i in 0..40 {
            record.subagent_started(&format!("s{i}"), "w", i);
        }
        assert_eq!(record.subagents.len(), SUBAGENTS_CAP);
        assert_eq!(record.subagents.last().unwrap().id, "s39");
    }

    #[test]
    fn subagent_identity_is_liberal_but_requires_an_id() {
        assert_eq!(
            subagent_identity(&json!({"agent_id": "a1", "agent_type": "Explore"})),
            Some(("a1", "Explore"))
        );
        // camelCase accepted; a missing label falls back generically.
        assert_eq!(
            subagent_identity(&json!({"agentId": "a2"})),
            Some(("a2", "subagent"))
        );
        // No id (or an empty one): unknown shape, caller logs and drops.
        assert_eq!(subagent_identity(&json!({"agent_type": "Explore"})), None);
        assert_eq!(subagent_identity(&json!({"agent_id": ""})), None);
    }

    #[test]
    fn now_line_tracks_tools_and_clears_on_boundaries() {
        let edit = json!({"tool_name": "Edit", "tool_input": {"file_path": "/w/src/foo.rs"}});
        assert_eq!(
            now_line_update("PreToolUse", &edit),
            Some(Some("editing foo.rs".into()))
        );
        assert_eq!(
            now_line_update("PostToolUse", &edit),
            Some(Some("edited foo.rs".into()))
        );
        let bash = json!({"tool_name": "Bash", "tool_input": {"command": "ls"}});
        assert_eq!(
            now_line_update("PreToolUse", &bash),
            Some(Some("running Bash".into()))
        );
        assert_eq!(
            now_line_update("PostToolUse", &bash),
            Some(Some("ran Bash".into()))
        );
        assert_eq!(
            now_line_update(
                "SubagentStart",
                &json!({"agent_id": "a", "agent_type": "Explore"})
            ),
            Some(Some("delegating to Explore".into()))
        );
        // Turn boundaries clear; everything else keeps the current line.
        for boundary in ["UserPromptSubmit", "Stop", "StopFailure", "SessionEnd"] {
            assert_eq!(now_line_update(boundary, &json!({})), Some(None));
        }
        assert_eq!(now_line_update("Notification", &json!({})), None);
        assert_eq!(now_line_update("PreToolUse", &json!({})), None);
        // Long values are capped — these strings ride every snapshot.
        let long = json!({"tool_name": "x".repeat(500)});
        let line = now_line_update("PreToolUse", &long).unwrap().unwrap();
        assert!(line.chars().count() <= SMALL_STRING_MAX);
    }

    #[test]
    fn statusline_usage_quantizes_and_tolerates_unknown_shapes() {
        // The documented statusline stdin shape.
        let usage = statusline_usage(&json!({
            "hook_event_name": "Status",
            "model": {"id": "claude-opus-4", "display_name": "Opus"},
            "context_window": {"used_percentage": 41.7},
            "cost": {"total_cost_usd": 0.1249},
        }));
        assert_eq!(usage.model.as_deref(), Some("Opus"));
        assert_eq!(usage.context_pct, Some(42)); // whole percent
        assert_eq!(usage.cost_cents, Some(12)); // whole cents
        assert_eq!(
            usage.to_json(),
            json!({"model": "Opus", "context_pct": 42, "cost_usd": 0.12})
        );
        // Tokens-ratio fallback; model id when no display name.
        let usage = statusline_usage(&json!({
            "model": {"id": "claude-sonnet-4"},
            "context_window": {"used_tokens": 50_000, "max_tokens": 200_000},
        }));
        assert_eq!(usage.model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(usage.context_pct, Some(25));
        assert_eq!(usage.cost_cents, None);
        // Hostile/odd values are clamped or dropped, never trusted.
        let usage = statusline_usage(&json!({
            "context_window": {"used_percentage": 1e12},
            "cost": {"total_cost_usd": -3.0},
        }));
        assert_eq!(usage.context_pct, Some(100));
        assert_eq!(usage.cost_cents, None);
        // Unknown shape: empty, and the caller drops it.
        assert!(statusline_usage(&json!({"whatever": true})).is_empty());
        assert!(statusline_usage(&json!("not an object")).is_empty());
    }

    #[test]
    fn titles_latest_custom_wins_over_latest_ai() {
        let mut record = AgentRecord::new("k".into(), AgentKind::Claude);
        assert!(apply_title_line(
            r#"{"type":"ai-title","aiTitle":"First ai","sessionId":"x"}"#,
            &mut record
        ));
        assert_eq!(record.title(), Some("First ai"));
        assert!(apply_title_line(
            r#"{"type":"custom-title","customTitle":"Mine"}"#,
            &mut record
        ));
        assert_eq!(record.title(), Some("Mine"));
        // A later ai-title is recorded but does not displace the custom title.
        assert!(!apply_title_line(
            r#"{"type":"ai-title","aiTitle":"Second ai"}"#,
            &mut record
        ));
        assert_eq!(record.title(), Some("Mine"));
        // Clearing the custom title falls back to the latest ai title.
        assert!(apply_title_line(r#"{"customTitle":null}"#, &mut record));
        assert_eq!(record.title(), Some("Second ai"));
        // Garbage lines are ignored.
        assert!(!apply_title_line("not json", &mut record));
    }

    #[test]
    fn display_name_resolution_chain() {
        let mut record = AgentRecord::new("k".into(), AgentKind::Claude);
        assert_eq!(record.display_name(None), "claude");
        record.first_prompt = Some("fix the flaky tests".into());
        assert_eq!(record.display_name(None), "fix the flaky tests");
        record.ai_title = Some("Fixing tests".into());
        assert_eq!(record.display_name(None), "Fixing tests");
        // Claude's live terminal title (e.g. after /rename) outranks aiTitle
        // and the first prompt; spark prefixes are stripped and default
        // titles carry no information.
        assert_eq!(record.display_name(Some("✳ git")), "git");
        assert_eq!(record.display_name(Some("✳ Claude Code")), "Fixing tests");
        assert_eq!(record.display_name(Some("  ")), "Fixing tests");
        record.custom_title = Some("My run".into());
        assert_eq!(record.display_name(Some("✳ git")), "My run");
    }

    #[test]
    fn truncate_prompt_cuts_at_word_boundaries() {
        // Short prompts pass through (whitespace collapsed).
        assert_eq!(truncate_prompt("short prompt"), "short prompt");
        assert_eq!(
            truncate_prompt("  collapse \n\t whitespace  "),
            "collapse whitespace"
        );
        // Long prompts are cut at a word boundary near 60 chars, with an
        // ellipsis marking the cut.
        let out = truncate_prompt(&"word ".repeat(30));
        assert!(out.chars().count() <= PROMPT_TITLE_MAX + 1, "{out}");
        assert!(out.ends_with(" word…"), "{out}");
        // A single giant token is hard-cut rather than dropped.
        let out = truncate_prompt(&"x".repeat(200));
        assert_eq!(out.chars().count(), PROMPT_TITLE_MAX + 1);
        assert!(out.ends_with('…'));
    }
}
