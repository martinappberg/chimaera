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
use crate::agent_state::{
    cleared_by_output, map_event, now_line_update, statusline_usage, subagent_identity,
    touched_file,
};

/// Fresh session id in the same format the PTY engine generates.
pub(crate) fn fresh_session_id() -> String {
    format!("s-{}", &chimaera_core::generate_token()[..8])
}

/// Fresh per-session hook secret.
pub(crate) fn fresh_agent_key() -> String {
    chimaera_core::generate_token()[..32].to_string()
}

/// The runtime directory holding per-agent `--settings` / `--mcp-config`
/// files. Hot state (scrubbed from `$XDG_RUNTIME_DIR`); reconstructed on
/// respawn when absent.
fn agents_runtime_dir() -> PathBuf {
    chimaera_core::runtime_dir().join("agents")
}

/// Path to a session's generated `--settings` file. The single source for the
/// `{id}-settings.json` convention — the writer ([`write_settings`]) and the
/// respawn reader (`chat::resolve_respawn_inputs`) must agree.
pub(crate) fn settings_path(session_id: &str) -> PathBuf {
    agents_runtime_dir().join(format!("{session_id}-settings.json"))
}

/// Path to a session's generated `--mcp-config` file. The single source for
/// the `{id}-mcp.json` convention (see [`write_mcp_config`]).
pub(crate) fn mcp_config_path(session_id: &str) -> PathBuf {
    agents_runtime_dir().join(format!("{session_id}-mcp.json"))
}

/// Hook events wired into the generated settings file. Everything the state
/// machine consumes, plus PostToolUse so long tool-free stretches still
/// refresh "running", plus SubagentStart/Stop for the live-subagent roster
/// (payloads carry `agent_id`/`agent_type` — verified claude 2.1.20x).
const HOOK_EVENTS: [&str; 10] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "StopFailure",
    "Notification",
    "SessionEnd",
];

/// Path to a session's generated statusline wrapper script (see
/// [`write_settings`]). Lives next to the settings file that references it,
/// so the two are scrubbed and regenerated together.
pub(crate) fn statusline_script_path(session_id: &str) -> PathBuf {
    agents_runtime_dir().join(format!("{session_id}-statusline.sh"))
}

/// Path to a session's statusline auth-header file (mode 0600): it holds the
/// `Authorization: Bearer <key>` line the wrapper's curl reads via `-H @file`.
/// The key rides this file, NOT the curl argv — `/proc/<pid>/cmdline` is
/// world-readable on shared login nodes, so the live curl must never carry
/// the secret as an argument. Same runtime dir / lifecycle as the script.
pub(crate) fn statusline_header_path(session_id: &str) -> PathBuf {
    agents_runtime_dir().join(format!("{session_id}-statusline.hdr"))
}

/// Write the per-agent Claude Code settings file (hooks -> daemon ingest URL)
/// into the chimaera runtime dir, mode 0600 (it embeds the session secret).
///
/// `theme` merges the scheme-matched theme ("light"/"dark") into the SAME
/// file the hooks ride — verified against claude 2.1.202: a `"theme"` key
/// in a `--settings` file re-themes the TUI. Callers pass `None` when the
/// user's own settings file already sets a theme (respect an explicit
/// choice; only fill the gap).
///
/// `user_statusline` is the user's own `statusLine` config (see
/// `runtimes::claude_user_statusline`): the settings also inject a
/// `statusLine` wrapper script that tees claude's statusline JSON to the
/// ingest route (`?event=statusline` — per-turn model/context/cost for TUI
/// rows) while preserving the user's statusline exactly. Injected under the
/// same condition as the hooks themselves; a user statusLine we cannot
/// reproduce (no single-line command string) wins outright — no injection.
///
/// `mastermind` gates the workspace Mastermind's act-tier MCP tools through
/// claude's own permission system (the dashboard plan §6): `Ask` pre-allows
/// ONLY the read tools (reads never nag; every act call raises claude's
/// native permission prompt), `Auto` pre-allows the whole chimaera server.
/// `None` (every non-mastermind session) writes no permissions block. A
/// running claude never re-reads this file, so a mode change re-creates the
/// session (the PUT mastermind route) rather than editing in place.
pub(crate) fn write_settings(
    session_id: &str,
    key: &str,
    port: u16,
    theme: Option<&str>,
    user_statusline: Option<&serde_json::Value>,
    mastermind: Option<crate::workspaces::MastermindMode>,
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
    if let Some(mode) = mastermind {
        let allow = match mode {
            // The shared read-tool list (mcp.rs) — codex's ask-mode argv is
            // generated from the same one, so the two harness gates agree.
            crate::workspaces::MastermindMode::Ask => json!(crate::mcp::MASTERMIND_READ_TOOLS
                .iter()
                .map(|t| format!("mcp__chimaera__{t}"))
                .collect::<Vec<_>>()),
            crate::workspaces::MastermindMode::Auto => json!(["mcp__chimaera"]),
        };
        settings["permissions"] = json!({ "allow": allow });
    }
    if let Some(passthrough) = statusline_passthrough(user_statusline) {
        let script = write_statusline_script(session_id, key, port, passthrough.as_deref())?;
        // claude runs statusLine commands through a shell: quote the path so
        // whitespace in a user-set state home can't split the command.
        settings["statusLine"] = json!({ "type": "command", "command": sh_quote(&script) });
        // Mirror the user's padding so their statusline spacing is unchanged.
        if let Some(padding) = user_statusline.and_then(|s| s.get("padding")) {
            settings["statusLine"]["padding"] = padding.clone();
        }
    }

    let path = settings_path(session_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&settings)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to chmod {}", path.display()))?;
    Ok(path)
}

/// Whether/how to wrap the user's statusline: `Some(passthrough_command)` =
/// inject the wrapper (piping stdin through the command when one exists;
/// with none the wrapper prints nothing and claude renders no statusline —
/// the TUI looks unchanged either way). `None` = the user's statusLine can't
/// be reproduced faithfully (not a single-line `command`), so injection is
/// skipped entirely and their own config applies untouched.
fn statusline_passthrough(user: Option<&serde_json::Value>) -> Option<Option<String>> {
    let Some(user) = user else {
        return Some(None);
    };
    // Only the documented `command` type is reproducible.
    if user
        .get("type")
        .and_then(|t| t.as_str())
        .is_some_and(|t| t != "command")
    {
        return None;
    }
    match user.get("command").and_then(|c| c.as_str()).map(str::trim) {
        None | Some("") => Some(None),
        // A multi-line command can't be embedded on the wrapper's pipe line.
        Some(cmd) if cmd.contains('\n') || cmd.contains('\r') => None,
        Some(cmd) => Some(Some(cmd.to_string())),
    }
}

/// Write the statusline wrapper script (mode 0700 — the URL embeds the
/// session secret): tee stdin to the ingest route in the background, then
/// pipe the same stdin through the user's own statusline command, if any.
/// Shell-single-quote a string for embedding in generated sh source (and
/// for the settings `command`, which claude runs through a shell): the one
/// escape single quotes need is closing, backslash-quoting, reopening.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Returns the script path as the string the settings `command` carries.
fn write_statusline_script(
    session_id: &str,
    key: &str,
    port: u16,
    passthrough: Option<&str>,
) -> anyhow::Result<String> {
    use std::os::unix::fs::PermissionsExt;

    // The URL is SECRET-FREE: the key would otherwise ride the curl argv, and
    // /proc/<pid>/cmdline is world-readable on the shared login nodes — any
    // other user could scrape the session id + key and reach this session's
    // MCP endpoint (for a Mastermind, its act tools). Instead the key lives in
    // a 0600 header file curl reads via `-H @file` (below), off argv entirely.
    let url = format!("http://127.0.0.1:{port}/api/v1/agent-events/{session_id}?event=statusline");
    // Write the Bearer-header file (0600) the wrapper's curl authenticates
    // with. Same runtime dir as the script; regenerated on every spawn.
    let hdr_path = statusline_header_path(session_id);
    if let Some(dir) = hdr_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    std::fs::write(&hdr_path, format!("Authorization: Bearer {key}\n"))
        .with_context(|| format!("failed to write {}", hdr_path.display()))?;
    std::fs::set_permissions(&hdr_path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to chmod {}", hdr_path.display()))?;
    let stamp = statusline_script_path(session_id).with_extension("stamp");
    // The runtime dir usually has a safe charset, but a user-set
    // CHIMAERA_HOME (or a dev build's home dir) can carry spaces or quotes —
    // embed it shell-quoted so the script never breaks on its own path.
    // The URL is generated hex/digits; quoted anyway for uniformity.
    let stamp = sh_quote(&stamp.to_string_lossy());
    let url_q = sh_quote(&url);
    let hdr_q = sh_quote(&hdr_path.to_string_lossy());
    // The POST is throttled to one per 2s via an epoch stamp file: claude
    // repaints the statusline continuously while streaming, and an
    // unthrottled tee would fork curl per repaint on a shared login node.
    // The daemon quantizes to whole percent/cents anyway, so nothing is
    // lost. The user's own statusline (below) still runs on EVERY repaint —
    // their TUI must look unchanged. The stamp read is the shell's `read`
    // builtin (zero forks per repaint — only the gated curl forks); read
    // reports EOF-without-newline as failure while still filling the
    // variable, so the value survives `|| :` and the case guards empty or
    // corrupt content.
    let mut script = format!(
        "#!/bin/sh\n\
         # generated by chimaera — do not edit (rewritten on session spawn)\n\
         # Tees claude's statusline JSON to the chimaera daemon (usage telemetry\n\
         # for the dashboard, at most one POST per 2s); your own statusline, if\n\
         # configured, still renders exactly as before.\n\
         input=$(cat)\n\
         now=$(date +%s)\n\
         last=0\n\
         if [ -f {stamp} ]; then IFS= read -r last < {stamp} || :; fi\n\
         case $last in ''|*[!0-9]*) last=0;; esac\n\
         if [ \"$((now - last))\" -ge 2 ]; then\n\
         printf '%s' \"$now\" > {stamp}\n\
         printf '%s' \"$input\" | curl -s -m 5 -H @{hdr_q} -H 'Content-Type: application/json' \
         --data-binary @- {url_q} >/dev/null 2>&1 &\n\
         fi\n"
    );
    if let Some(cmd) = passthrough {
        // %s\n, not %s: $(cat) stripped the trailing newline claude sent,
        // and a line-oriented user statusline (`read -r line && render`)
        // treats EOF-without-newline as failure and renders nothing.
        script.push_str(&format!("printf '%s\\n' \"$input\" | {cmd}\n"));
    }
    let path = statusline_script_path(session_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    std::fs::write(&path, script).with_context(|| format!("failed to write {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to chmod {}", path.display()))?;
    Ok(path.to_string_lossy().into_owned())
}

/// The per-session MCP endpoint URL with key-in-URL auth — for the claude
/// `--mcp-config` writer, whose file is 0600 (an agent's MCP client cannot
/// know the daemon bearer token).
pub(crate) fn mcp_url(session_id: &str, key: &str, port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/v1/mcp/{session_id}?key={key}")
}

/// The secret-free MCP endpoint URL — for the codex `-c mcp_servers`
/// injection, whose config rides world-readable argv: the key travels in
/// the spawn env instead (`launcher::CODEX_MCP_KEY_ENV` →
/// `bearer_token_env_var` → `Authorization: Bearer`).
pub(crate) fn mcp_url_bare(session_id: &str, port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/v1/mcp/{session_id}")
}

/// Write the per-agent MCP config wiring claude to this daemon's
/// linked-terminal tools (`--mcp-config`; merges with the user's own MCP
/// servers). Mode 0600 — the URL embeds the session secret.
pub(crate) fn write_mcp_config(session_id: &str, key: &str, port: u16) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let url = mcp_url(session_id, key, port);
    let config = json!({
        "mcpServers": { "chimaera": { "type": "http", "url": url } }
    });
    let path = mcp_config_path(session_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
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
    /// Non-hook deliveries on the same route: the statusline wrapper posts
    /// with `event=statusline` (its payload has no meaningful
    /// `hook_event_name` for the state machine).
    #[serde(default)]
    event: Option<String>,
}

/// The bare token from an `Authorization: Bearer <token>` header, if present
/// and well-formed. The statusline wrapper's curl authenticates this way so
/// the key stays off its argv.
fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// POST /api/v1/agent-events/{id}?key={secret} — Claude Code hook ingestion.
///
/// Not behind bearer auth (claude's hook cannot know the daemon token); the
/// per-session random key authorizes it. The hooks (`type:http`, made in
/// claude's own process) pass it as `?key=`; the statusline wrapper, which
/// shells out to `curl`, passes it as `Authorization: Bearer` instead so the
/// secret never rides a world-readable curl argv — both channels carry the
/// same per-session key and either satisfies the check.
pub(crate) async fn ingest(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<IngestQuery>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    // The presented key rides one of two disjoint channels: the `?key=` query
    // param (the http hooks — always present) or, when that is absent, the
    // `Authorization: Bearer` header (the statusline curl, which keeps the key
    // off its argv). Query-first so an unrelated Authorization header (a
    // client that always sends the daemon token) can't shadow the hook key.
    let presented_key = if query.key.is_empty() {
        bearer_token(&headers).unwrap_or_default()
    } else {
        query.key.as_str()
    };
    // Statusline heartbeat (the generated wrapper posts the TUI's statusline
    // JSON with `?event=statusline`): quantized usage telemetry only — it
    // never touches the hook state machine. Liberal ingest: an unknown shape
    // is logged and dropped, never an error back at the wrapper.
    if query.event.as_deref() == Some("statusline") {
        let changed = {
            let mut agents = crate::lock(&state.agents);
            let Some(record) = agents.get_mut(&id) else {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": format!("unknown agent session {id}")})),
                )
                    .into_response();
            };
            if record.key != presented_key {
                return (StatusCode::FORBIDDEN, Json(json!({"error": "bad key"}))).into_response();
            }
            let usage = statusline_usage(&payload);
            if usage.is_empty() {
                tracing::debug!(session = %id, "statusline payload carried no usage fields; dropped");
                false
            } else if record.usage.as_ref() != Some(&usage) {
                record.usage = Some(usage);
                true
            } else {
                false
            }
        };
        if changed {
            state.changes.notify_waiters();
        }
        return Json(json!({})).into_response();
    }

    // `event` comes from the payload, not the record — compute it before the
    // lock so the guard's block below stays tight.
    let event = payload
        .get("hook_event_name")
        .and_then(|e| e.as_str())
        .unwrap_or("");

    let mut changed = false;
    let mut touched_path: Option<String> = None;
    // Mutate the AgentRecord under the agents lock in a block that ENDS —
    // dropping the guard — before the `.await` below. A `std::sync` guard must
    // never be live across an await (the future would be !Send), and an
    // explicit `drop` isn't enough here: the `let-else` borrow keeps the guard
    // in the async state machine, so it must go out of scope lexically.
    let compute_ctx_pending = {
        let mut agents = crate::lock(&state.agents);
        let Some(record) = agents.get_mut(&id) else {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("unknown agent session {id}")})),
            )
                .into_response();
        };
        if record.key != presented_key {
            return (StatusCode::FORBIDDEN, Json(json!({"error": "bad key"}))).into_response();
        }

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
                touched_path = Some(path.to_string());
            }
        }

        // Live-subagent roster for the dashboard cards. Payload identity is
        // parsed liberally (`agent_state::subagent_identity`); an unknown
        // shape is logged and dropped, never an ingest error.
        match event {
            "SubagentStart" => match subagent_identity(&payload) {
                Some((sub_id, label)) => {
                    changed |= record.subagent_started(sub_id, label, crate::session_view::now_ms())
                }
                None => {
                    tracing::debug!(session = %id, "SubagentStart without agent identity; dropped")
                }
            },
            "SubagentStop" => match subagent_identity(&payload) {
                Some((sub_id, _)) => changed |= record.subagent_stopped(sub_id),
                None => {
                    tracing::debug!(session = %id, "SubagentStop without agent identity; dropped")
                }
            },
            // A turn/session end means every live subagent is done — clear
            // stragglers whose SubagentStop never arrived.
            "Stop" | "StopFailure" | "SessionEnd" if !record.subagents.is_empty() => {
                record.subagents.clear();
                changed = true;
            }
            _ => {}
        }

        // One-line "what it's doing now" for the dashboard card, replaced
        // per event and cleared at turn boundaries.
        if let Some(update) = now_line_update(event, &payload) {
            if record.now_line != update {
                record.now_line = update;
                changed = true;
            }
        }

        if let Some(next) = map_event(event, &payload) {
            if record.state != next {
                record.state = next;
                changed = true;
            }
        }

        // Compute-session context wants delivering on whichever carrier
        // fires first (see below); the record's flag is only SET once the
        // context actually exists — checked outside this lock.
        matches!(event, "SessionStart" | "UserPromptSubmit") && !record.compute_ctx_delivered
    };

    if changed {
        state.changes.notify_waiters();
    }

    // An agent writing a file is the signature git refresh trigger: the hook we
    // already ingest IS the mechanism — no polling, no terminal-text parsing.
    if let Some(path) = touched_path {
        crate::git::mark_path_dirty(&state, &path).await;
    }

    // Compute-session context: a daemon running INSIDE a Slurm allocation
    // (a Mode 2 compute-node daemon) tells its agents so, via the hook
    // response's `additionalContext` — the only chimaera-owned channel that
    // reaches BOTH surfaces (TUI and chat ride the same `--settings` hooks)
    // without touching any user-owned file ($HOME is a shared filesystem, so
    // a CLAUDE.md edit would leak into login-node sessions). Delivered once
    // per record, on whichever carrier fires first: SessionStart covers chat
    // mode (hooks fire normally under stream-json EXCEPT UserPromptSubmit —
    // PROTOCOL.md), the first UserPromptSubmit covers TUIs where SessionStart
    // has been unreliable (see the transcript_path note above). Off-cluster
    // `agent_context` is None in one Option check — response unchanged.
    let mut context: Vec<String> = Vec::new();
    if compute_ctx_pending {
        if let Some(ctx) = state.compute.agent_context().await {
            let mut agents = crate::lock(&state.agents);
            // Re-check under the lock: a concurrent hook may have delivered
            // it between the block above and here.
            if let Some(record) = agents.get_mut(&id) {
                if !record.compute_ctx_delivered {
                    record.compute_ctx_delivered = true;
                    context.push(ctx);
                }
            }
        }
    }

    // `@term:` mentions in the user's prompt auto-link those terminals to
    // this agent (the mention is the consent — this hook only fires for the
    // human's composer input). The notes flow back as context, so the agent
    // knows the link exists without a discovery round-trip.
    if event == "UserPromptSubmit" {
        if let Some(prompt) = payload.get("prompt").and_then(|p| p.as_str()) {
            context.extend(crate::mcp::autolink_mentions(&state, &id, prompt));
        }
    }

    // `context` is only ever non-empty for SessionStart/UserPromptSubmit,
    // so `event` is always the right hookEventName here.
    if !context.is_empty() {
        return Json(json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "additionalContext": context.join("\n"),
            }
        }))
        .into_response();
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
                // This loop only retires on PTY absence — a chat driver keeps
                // the session alive via `session_alive`, and its own exit path
                // retires first — so the surface here is always the terminal.
                crate::recents::retire(
                    &state,
                    &session_id,
                    last_pin.as_deref(),
                    last_osc.as_deref(),
                    chimaera_agent::model::SessionUi::Term,
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
        let path = write_settings(&sid, &key, 43999, None, None, None).unwrap();
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
        // Not a mastermind: no permissions block (the pre-mastermind shape).
        assert!(value.get("permissions").is_none());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(statusline_script_path(&sid)).ok();
    }

    /// The scheme-matched theme merges into the SAME settings file the hooks
    /// ride (verified against claude 2.1.202: `"theme"` in a `--settings`
    /// file re-themes the TUI).
    #[test]
    fn settings_file_merges_theme_next_to_hooks() {
        let sid = fresh_session_id();
        let key = fresh_agent_key();
        let path = write_settings(&sid, &key, 43999, Some("light"), None, None).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["theme"], "light");
        // The hooks still ride along untouched.
        assert_eq!(
            value["hooks"]["SessionStart"][0]["hooks"][0]["type"],
            "http"
        );
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(statusline_script_path(&sid)).ok();
    }

    /// The Mastermind's harness gating rides the generated settings: ask
    /// mode pre-allows ONLY the read tools (acts raise claude's native
    /// permission prompt → the attention lane); auto pre-allows the whole
    /// chimaera server. The exact rule strings are the contract claude
    /// matches against — pin them.
    #[test]
    fn settings_permissions_gate_mastermind_modes() {
        use crate::workspaces::MastermindMode;

        let key = fresh_agent_key();

        let sid = fresh_session_id();
        let path =
            write_settings(&sid, &key, 43999, None, None, Some(MastermindMode::Ask)).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value["permissions"]["allow"],
            json!([
                "mcp__chimaera__workspace_status",
                "mcp__chimaera__read_session",
                "mcp__chimaera__list_changed_files",
                "mcp__chimaera__list_terminals",
                "mcp__chimaera__read_terminal",
            ])
        );
        // The hooks still ride along untouched.
        assert_eq!(
            value["hooks"]["SessionStart"][0]["hooks"][0]["type"],
            "http"
        );
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(statusline_script_path(&sid)).ok();

        let sid = fresh_session_id();
        let path =
            write_settings(&sid, &key, 43999, None, None, Some(MastermindMode::Auto)).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["permissions"]["allow"], json!(["mcp__chimaera"]));
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(statusline_script_path(&sid)).ok();
    }

    /// The statusline heartbeat: the settings point `statusLine` at a
    /// generated wrapper that tees stdin to the ingest route in the
    /// background. With no user statusline the wrapper prints nothing; a
    /// user command is piped the same stdin (their TUI looks unchanged); an
    /// un-embeddable user statusLine suppresses injection entirely.
    #[test]
    fn settings_statusline_wrapper_preserves_user_statusline() {
        use std::os::unix::fs::PermissionsExt;

        // (a) No user statusline: wrapper injected, prints nothing itself.
        let sid = fresh_session_id();
        let key = fresh_agent_key();
        let path = write_settings(&sid, &key, 43999, None, None, None).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let script_path = statusline_script_path(&sid);
        assert_eq!(value["statusLine"]["type"], "command");
        // Shell-quoted: claude runs the command through a shell, and the
        // state-home path may carry whitespace.
        assert_eq!(
            value["statusLine"]["command"],
            json!(format!("'{}'", script_path.to_string_lossy()))
        );
        assert!(value["statusLine"].get("padding").is_none());
        let script = std::fs::read_to_string(&script_path).unwrap();
        // The URL is SECRET-FREE — the key must NOT ride the curl argv (it is
        // world-readable via /proc on shared login nodes).
        let url = format!("http://127.0.0.1:43999/api/v1/agent-events/{sid}?event=statusline");
        assert!(script.contains(&format!("'{url}'")), "{script}");
        assert!(
            !script.contains(&key),
            "the key must never appear in the argv: {script}"
        );
        assert!(!script.contains("?key="), "no key in the URL: {script}");
        // The key rides a 0600 header file curl reads via `-H @file`.
        let hdr_path = statusline_header_path(&sid);
        assert!(
            script.contains(&format!("-H @'{}'", hdr_path.to_string_lossy())),
            "{script}"
        );
        let hdr = std::fs::read_to_string(&hdr_path).unwrap();
        assert_eq!(hdr, format!("Authorization: Bearer {key}\n"));
        let hdr_mode = std::fs::metadata(&hdr_path).unwrap().permissions().mode();
        assert_eq!(hdr_mode & 0o777, 0o600, "the header file is owner-only");
        // The POST is backgrounded (never delays a user statusline) and the
        // wrapper's only output line is the optional passthrough — absent here.
        assert!(script.contains(">/dev/null 2>&1 &"), "{script}");
        assert_eq!(script.matches("printf '%s' \"$input\"").count(), 1);
        // The stamp read is the zero-fork `read` builtin, corrupt-safe.
        assert!(script.contains("IFS= read -r last <"), "{script}");
        // Fork throttle: claude repaints continuously while streaming; the
        // POST (and its curl fork) is gated on a ≥2s epoch stamp — login-node
        // fork churn is a review criterion.
        assert!(script.contains("-ge 2"), "{script}");
        assert!(script.contains(".stamp"), "{script}");
        // Executable, owner-only.
        let mode = std::fs::metadata(&script_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        std::fs::remove_file(&hdr_path).ok();
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&script_path).ok();

        // (b) A user statusline command: piped the same stdin, padding kept.
        let sid = fresh_session_id();
        let user = json!({"type": "command", "command": "my-status --flag", "padding": 0});
        let path = write_settings(&sid, &key, 43999, None, Some(&user), None).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["statusLine"]["padding"], 0);
        let script_path = statusline_script_path(&sid);
        let script = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            // %s\n: the command substitution stripped the trailing newline;
            // line-oriented user statuslines need it back.
            script.contains("printf '%s\\n' \"$input\" | my-status --flag"),
            "{script}"
        );
        // The wrapper is a real sh program: it must at least parse.
        let parses = std::process::Command::new("/bin/sh")
            .arg("-n")
            .arg(&script_path)
            .status()
            .unwrap()
            .success();
        assert!(parses, "{script}");
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&script_path).ok();

        // (c) A user statusLine we can't reproduce (multi-line command or a
        // non-command type): no injection at all — theirs applies untouched.
        for user in [
            json!({"type": "command", "command": "line1\nline2"}),
            json!({"type": "widget"}),
        ] {
            let sid = fresh_session_id();
            let path = write_settings(&sid, &key, 43999, None, Some(&user), None).unwrap();
            let value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert!(value.get("statusLine").is_none(), "{user}");
            assert!(!statusline_script_path(&sid).exists());
            std::fs::remove_file(&path).ok();
        }
    }
}
