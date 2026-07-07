//! Agent sessions: server-side wrapper state over PTY sessions running the
//! interactive `claude` TUI.
//!
//! The PTY engine stays agent-agnostic. For sessions of kind "agent" the
//! server keeps an [`AgentRecord`] keyed by session id: a per-session secret
//! that authorizes Claude Code hook deliveries, the attention state derived
//! from those hooks, and a display title tail-polled from Claude's own
//! transcript records.
//!
//! Hooks are injected via a generated settings file passed to `claude`
//! with `--settings`, using hook type `"http"` (verified against
//! claude 2.1.196): claude POSTs each hook payload as JSON to
//! `/api/v1/agent-events/{sid}?key={secret}` on the daemon's loopback port.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::AppState;

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

/// Fresh session id in the same format the PTY engine generates.
pub(crate) fn fresh_session_id() -> String {
    format!("s-{}", &chimaera_core::generate_token()[..8])
}

/// Fresh per-session hook secret.
pub(crate) fn fresh_agent_key() -> String {
    chimaera_core::generate_token()[..32].to_string()
}

/// Hook events wired into the generated settings file. Everything the state
/// machine consumes, plus PostToolUse so long tool-free stretches still
/// refresh "running".
const HOOK_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "StopFailure",
    "Notification",
    "SessionEnd",
];

/// Write the per-agent Claude Code settings file (hooks -> daemon ingest URL)
/// into the chimaera runtime dir, mode 0600 (it embeds the session secret).
///
/// `theme` merges the scheme-matched theme ("light"/"dark") into the SAME
/// file the hooks ride — verified against claude 2.1.202: a `"theme"` key
/// in a `--settings` file re-themes the TUI. Callers pass `None` when the
/// user's own settings file already sets a theme (respect an explicit
/// choice; only fill the gap).
pub(crate) fn write_settings(
    session_id: &str,
    key: &str,
    port: u16,
    theme: Option<&str>,
) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let url = format!("http://127.0.0.1:{port}/api/v1/agent-events/{session_id}?key={key}");
    let hook = json!({ "type": "http", "url": url, "timeout": 10 });
    let mut hooks = serde_json::Map::new();
    for event in HOOK_EVENTS {
        hooks.insert(event.to_string(), json!([{ "hooks": [hook] }]));
    }
    let mut settings = json!({ "hooks": hooks });
    if let Some(theme) = theme {
        settings["theme"] = json!(theme);
    }

    let dir = chimaera_core::runtime_dir().join("agents");
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join(format!("{session_id}-settings.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&settings)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to chmod {}", path.display()))?;
    Ok(path)
}

/// Write the per-agent MCP config wiring claude to this daemon's
/// linked-terminal tools (`--mcp-config`; merges with the user's own MCP
/// servers). Mode 0600 — the URL embeds the session secret.
pub(crate) fn write_mcp_config(session_id: &str, key: &str, port: u16) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let url = format!("http://127.0.0.1:{port}/api/v1/mcp/{session_id}?key={key}");
    let config = json!({
        "mcpServers": { "chimaera": { "type": "http", "url": url } }
    });
    let dir = chimaera_core::runtime_dir().join("agents");
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join(format!("{session_id}-mcp.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to chmod {}", path.display()))?;
    Ok(path)
}

#[derive(Deserialize)]
pub(crate) struct IngestQuery {
    #[serde(default)]
    key: String,
}

/// POST /api/v1/agent-events/{id}?key={secret} — Claude Code hook ingestion.
///
/// Not behind bearer auth (claude's hook cannot know the daemon token); the
/// per-session random key embedded in the hook URL authorizes it.
pub(crate) async fn ingest(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<IngestQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    let mut agents = crate::lock(&state.agents);
    let Some(record) = agents.get_mut(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown agent session {id}")})),
        )
            .into_response();
    };
    if record.key != query.key {
        return (StatusCode::FORBIDDEN, Json(json!({"error": "bad key"}))).into_response();
    }

    let mut changed = false;

    // Every hook payload carries the current transcript_path (verified against
    // claude 2.1.196), so capture it from any event — SessionStart does not
    // reliably fire for hooks injected via --settings.
    if let Some(path) = payload.get("transcript_path").and_then(|p| p.as_str()) {
        let path = PathBuf::from(path);
        if record.transcript_path.as_ref() != Some(&path) {
            record.transcript_path = Some(path);
            changed = true;
        }
    }

    let event = payload
        .get("hook_event_name")
        .and_then(|e| e.as_str())
        .unwrap_or("");

    // The first prompt becomes the provisional display name (it loses to any
    // customTitle/aiTitle transcript record; see AgentRecord::display_name).
    if event == "UserPromptSubmit" && record.first_prompt.is_none() {
        if let Some(prompt) = payload.get("prompt").and_then(|p| p.as_str()) {
            let prompt = prompt.trim();
            if !prompt.is_empty() {
                record.first_prompt = Some(prompt.to_string());
                changed = true;
            }
        }
    }

    // File-writing tools feed the touched-files list (clickable "N files"
    // chips in the UI). Real payloads verified against claude 2.1.196: the
    // path lives in tool_input.file_path (notebook_path for NotebookEdit).
    if event == "PostToolUse" {
        if let Some(path) = touched_file(&payload) {
            changed |= record.touch_file(path);
        }
    }

    if let Some(next) = map_event(event, &payload) {
        if record.state != next {
            record.state = next;
            changed = true;
        }
    }
    drop(agents);

    if changed {
        state.changes.notify_waiters();
    }

    // `@term:` mentions in the user's prompt auto-link those terminals to
    // this agent (the mention is the consent — this hook only fires for the
    // human's composer input). The notes flow back as context, so the agent
    // knows the link exists without a discovery round-trip.
    if event == "UserPromptSubmit" {
        if let Some(prompt) = payload.get("prompt").and_then(|p| p.as_str()) {
            let notes = crate::mcp::autolink_mentions(&state, &id, prompt);
            if !notes.is_empty() {
                return Json(json!({
                    "hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit",
                        "additionalContext": notes.join("\n"),
                    }
                }))
                .into_response();
            }
        }
    }

    Json(json!({})).into_response()
}

/// The file path a PostToolUse payload touched, if its tool writes files.
/// Field names verified against real hook payloads: Write/Edit (and
/// MultiEdit, same tool family) carry `tool_input.file_path`; NotebookEdit
/// carries `tool_input.notebook_path`.
fn touched_file(payload: &serde_json::Value) -> Option<&str> {
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
fn map_event(event: &str, payload: &serde_json::Value) -> Option<AgentState> {
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

/// Transcript poll interval: 2s in production, fast in tests. (Shared with
/// the install-session watcher in `runtimes`.)
pub(crate) fn poll_interval() -> Duration {
    if cfg!(test) {
        Duration::from_millis(50)
    } else {
        Duration::from_secs(2)
    }
}

/// Watch one agent session for its lifetime: tail-poll the transcript for
/// title records and retire the agent record into the workspace's recents
/// (broadcasting a change) once the underlying PTY session is gone.
pub(crate) fn spawn_agent_watch(state: Arc<AppState>, session_id: String) {
    tokio::spawn(async move {
        let mut tailed: Option<PathBuf> = None;
        let mut offset: u64 = 0;
        let mut partial = Vec::new();
        // Last name facts seen live, for the recents entry: SessionInfo is
        // gone by the time the death tick fires.
        let mut last_pin: Option<String> = None;
        let mut last_osc: Option<String> = None;
        loop {
            tokio::time::sleep(poll_interval()).await;

            let Some(info) = state.sessions.get(&session_id) else {
                crate::recents::retire(
                    &state,
                    &session_id,
                    last_pin.as_deref(),
                    last_osc.as_deref(),
                );
                return;
            };
            last_pin = info.renamed.then(|| info.name.clone());
            if info.title.is_some() {
                last_osc = info.title;
            }

            let path = {
                let agents = crate::lock(&state.agents);
                let Some(record) = agents.get(&session_id) else {
                    return; // record withdrawn elsewhere
                };
                record.transcript_path.clone()
            };
            let Some(path) = path else { continue };
            if tailed.as_ref() != Some(&path) {
                tailed = Some(path.clone());
                offset = 0;
                partial.clear();
            }

            let lines = match read_appended_lines(&path, &mut offset, &mut partial).await {
                Ok(lines) => lines,
                Err(err) => {
                    tracing::debug!(session = %session_id, path = %path.display(), %err,
                        "transcript tail read failed");
                    continue;
                }
            };
            if lines.is_empty() {
                continue;
            }

            let mut changed = false;
            {
                let mut agents = crate::lock(&state.agents);
                let Some(record) = agents.get_mut(&session_id) else {
                    return;
                };
                for line in &lines {
                    changed |= apply_title_line(line, record);
                }
            }
            if changed {
                state.changes.notify_waiters();
            }
        }
    });
}

/// Read bytes appended to `path` since `offset`, returning complete lines.
/// A trailing partial line is buffered until its newline arrives. Truncation
/// (file shrank below `offset`) restarts from the beginning.
async fn read_appended_lines(
    path: &std::path::Path,
    offset: &mut u64,
    partial: &mut Vec<u8>,
) -> anyhow::Result<Vec<String>> {
    let mut file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    if len < *offset {
        *offset = 0;
        partial.clear();
    }
    if len == *offset {
        return Ok(Vec::new());
    }
    file.seek(std::io::SeekFrom::Start(*offset)).await?;
    let mut buf = Vec::with_capacity((len - *offset) as usize);
    file.read_to_end(&mut buf).await?;
    *offset += buf.len() as u64;

    let mut lines = Vec::new();
    for byte in buf {
        if byte == b'\n' {
            lines.push(String::from_utf8_lossy(partial).into_owned());
            partial.clear();
        } else {
            partial.push(byte);
        }
    }
    Ok(lines)
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

    #[test]
    fn settings_file_embeds_hook_url() {
        let sid = fresh_session_id();
        let key = fresh_agent_key();
        let path = write_settings(&sid, &key, 43999, None).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&contents).unwrap();
        let url = format!("http://127.0.0.1:43999/api/v1/agent-events/{sid}?key={key}");
        for event in HOOK_EVENTS {
            assert_eq!(
                value["hooks"][event][0]["hooks"][0]["type"], "http",
                "{event}"
            );
            assert_eq!(value["hooks"][event][0]["hooks"][0]["url"], json!(url));
        }
        // No theme requested: the settings stay hooks-only (a user with an
        // explicit theme choice is never overridden).
        assert!(value.get("theme").is_none());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_file(&path).ok();
    }

    /// The scheme-matched theme merges into the SAME settings file the hooks
    /// ride (verified against claude 2.1.202: `"theme"` in a `--settings`
    /// file re-themes the TUI).
    #[test]
    fn settings_file_merges_theme_next_to_hooks() {
        let sid = fresh_session_id();
        let key = fresh_agent_key();
        let path = write_settings(&sid, &key, 43999, Some("light")).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["theme"], "light");
        // The hooks still ride along untouched.
        assert_eq!(
            value["hooks"]["SessionStart"][0]["hooks"][0]["type"],
            "http"
        );
        std::fs::remove_file(&path).ok();
    }
}
