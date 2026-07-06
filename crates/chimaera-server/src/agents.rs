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

/// Server-side wrapper state for one agent session.
#[derive(Clone, Debug)]
pub(crate) struct AgentRecord {
    /// Per-session secret embedded in the hook URL; authorizes ingestion.
    pub(crate) key: String,
    pub(crate) state: AgentState,
    /// Latest transcript path reported by any hook payload.
    pub(crate) transcript_path: Option<PathBuf>,
    /// Latest `customTitle` transcript record (wins over `ai_title`).
    pub(crate) custom_title: Option<String>,
    /// Latest `{"type":"ai-title"}` transcript record.
    pub(crate) ai_title: Option<String>,
}

impl AgentRecord {
    pub(crate) fn new(key: String) -> Self {
        AgentRecord {
            key,
            state: AgentState::Unknown,
            transcript_path: None,
            custom_title: None,
            ai_title: None,
        }
    }

    /// Display title: latest customTitle wins over latest aiTitle.
    pub(crate) fn title(&self) -> Option<&str> {
        self.custom_title.as_deref().or(self.ai_title.as_deref())
    }
}

/// Fresh session id in the same format the PTY engine generates.
pub(crate) fn fresh_session_id() -> String {
    format!("s-{}", &chimaera_core::generate_token()[..8])
}

/// Fresh per-session hook secret.
pub(crate) fn fresh_agent_key() -> String {
    chimaera_core::generate_token()[..32].to_string()
}

/// Resolve the `claude` binary through the user's login shell. Field-tested:
/// `claude` is not on the non-interactive ssh PATH on HPC login nodes, so a
/// plain `which` from the daemon's environment is not enough.
pub(crate) async fn resolve_claude() -> Result<PathBuf, String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let output = tokio::process::Command::new(&shell)
        .arg("-lc")
        .arg("command -v claude")
        .output()
        .await;
    let not_found = || {
        format!(
            "claude not found via `{shell} -lc 'command -v claude'`; install Claude Code \
             (https://claude.com/claude-code) or make `claude` resolvable from your login shell"
        )
    };
    match output {
        Ok(out) if out.status.success() => {
            // Login shells may print banners; the path is the last non-empty line.
            let stdout = String::from_utf8_lossy(&out.stdout);
            let path = stdout
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            if path.starts_with('/') {
                Ok(PathBuf::from(path))
            } else {
                Err(not_found())
            }
        }
        Ok(_) => Err(not_found()),
        Err(err) => Err(format!("failed to run login shell {shell}: {err}")),
    }
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
pub(crate) fn write_settings(session_id: &str, key: &str, port: u16) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let url = format!("http://127.0.0.1:{port}/api/v1/agent-events/{session_id}?key={key}");
    let hook = json!({ "type": "http", "url": url, "timeout": 10 });
    let mut hooks = serde_json::Map::new();
    for event in HOOK_EVENTS {
        hooks.insert(event.to_string(), json!([{ "hooks": [hook] }]));
    }
    let settings = json!({ "hooks": hooks });

    let dir = chimaera_core::runtime_dir().join("agents");
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join(format!("{session_id}-settings.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&settings)?)
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
    Json(json!({})).into_response()
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

/// Transcript poll interval: 2s in production, fast in tests.
fn poll_interval() -> Duration {
    if cfg!(test) {
        Duration::from_millis(50)
    } else {
        Duration::from_secs(2)
    }
}

/// Watch one agent session for its lifetime: tail-poll the transcript for
/// title records and drop the agent record (broadcasting a change) once the
/// underlying PTY session is gone.
pub(crate) fn spawn_agent_watch(state: Arc<AppState>, session_id: String) {
    tokio::spawn(async move {
        let mut tailed: Option<PathBuf> = None;
        let mut offset: u64 = 0;
        let mut partial = Vec::new();
        loop {
            tokio::time::sleep(poll_interval()).await;

            if state.sessions.get(&session_id).is_none() {
                if crate::lock(&state.agents).remove(&session_id).is_some() {
                    state.changes.notify_waiters();
                }
                return;
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
/// null clears it).
fn apply_title_line(line: &str, record: &mut AgentRecord) -> bool {
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
    fn titles_latest_custom_wins_over_latest_ai() {
        let mut record = AgentRecord::new("k".into());
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
    fn settings_file_embeds_hook_url() {
        let sid = fresh_session_id();
        let key = fresh_agent_key();
        let path = write_settings(&sid, &key, 43999).unwrap();
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
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_file(&path).ok();
    }
}
