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

pub(crate) use crate::agent_state::{apply_title_line, truncate_prompt, AgentKind, AgentRecord};
use crate::agent_state::{cleared_by_output, map_event, touched_file};

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
    let mut touched_path: Option<String> = None;
    if event == "PostToolUse" {
        if let Some(path) = touched_file(&payload) {
            changed |= record.touch_file(path);
            touched_path = Some(path.to_string());
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

    // An agent writing a file is the signature git refresh trigger: the hook we
    // already ingest IS the mechanism — no polling, no terminal-text parsing.
    if let Some(path) = touched_path {
        crate::git::mark_path_dirty(&state, &path);
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

            // Liveness spans BOTH registries: an agent session may run as a
            // PTY TUI or as a structured chat driver (and hop between them
            // via the view toggle) under one id. Retiring on PTY absence
            // alone would kill every chat session ~2s after spawn.
            let info = state.sessions.get(&session_id);
            if info.is_none() && !crate::chat::session_alive(&state, &session_id) {
                crate::recents::retire(
                    &state,
                    &session_id,
                    last_pin.as_deref(),
                    last_osc.as_deref(),
                );
                return;
            }
            if let Some(info) = info {
                last_pin = info.renamed.then(|| info.name.clone());
                if info.title.is_some() {
                    last_osc = info.title;
                }
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
                // Reaching here means the transcript grew: the agent is
                // producing content, so clear a stale "needs you" no hook
                // flipped back (see `cleared_by_output`).
                if let Some(next) = cleared_by_output(record.state) {
                    record.state = next;
                    changed = true;
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

#[cfg(test)]
mod tests {
    use super::*;

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
