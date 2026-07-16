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
    /// The claude session id this session resumed (`--resume <id>`), when it
    /// did. Claude forks a NEW session id on resume, so recents needs this
    /// to recognize the live continuation of an old conversation.
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
        }
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
