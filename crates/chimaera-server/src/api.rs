use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::workspaces::Workspace;
use crate::AppState;

/// Require `Authorization: Bearer {token}` on /api/v1 routes.
pub(crate) async fn auth(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {}", state.token));

    if authorized {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response()
    }
}

/// GET /api/v1/health
pub(crate) async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(json!({
        "name": "chimaera",
        "version": chimaera_core::VERSION,
        // The build id lets clients spot daemon/client skew (semver is the
        // 0.0.1 sentinel on every dev build, so it cannot).
        "build": chimaera_core::BUILD_ID,
        "hostname": state.hostname,
        "pid": state.pid,
        "uptime_secs": state.started.elapsed().as_secs(),
    }))
}

/// GET /api/v1/workspaces
pub(crate) async fn list_workspaces(State(state): State<Arc<AppState>>) -> Json<Vec<Workspace>> {
    Json(crate::lock(&state.workspaces).list())
}

#[derive(Deserialize)]
pub(crate) struct CreateWorkspace {
    root: String,
}

/// POST /api/v1/workspaces — register a directory, idempotent per canonical root.
pub(crate) async fn create_workspace(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateWorkspace>,
) -> Response {
    let root = match std::fs::canonicalize(PathBuf::from(&body.root)) {
        Ok(root) => root,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("{}: {err}", body.root)})),
            )
                .into_response();
        }
    };
    if !root.is_dir() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("{} is not a directory", root.display())})),
        )
            .into_response();
    }
    match crate::lock(&state.workspaces).add(root) {
        Ok(workspace) => Json(workspace).into_response(),
        Err(err) => {
            tracing::error!(%err, "failed to persist workspace");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}

/// POST /api/v1/workspaces/{id}/open — stamp a workspace as freshly opened
/// (home-screen recency), returning it.
pub(crate) async fn open_workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match crate::lock(&state.workspaces).touch(&id) {
        Some(workspace) => Json(workspace).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown workspace"})),
        )
            .into_response(),
    }
}

/// DELETE /api/v1/workspaces/{id} — unregister a workspace (files untouched).
pub(crate) async fn delete_workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match crate::lock(&state.workspaces).remove(&id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown workspace"})),
        )
            .into_response(),
        Err(err) => {
            tracing::error!(%err, "failed to persist workspace removal");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}

/// Serialize a `SessionInfo` with the extra `workspace_id`, `kind`,
/// `agent_kind`, `agent_state`, `agent_title`, `files_touched`,
/// `display_name` and `cwd_current` fields.
/// `agent` is the wrapper record for kind "agent" sessions; `None` means a
/// plain shell. `polled` / `polled_cwd` are the shell naming watcher's
/// latest values, if any; `cwd_current` falls back to the spawn cwd (agents,
/// never-polled shells).
pub(crate) fn session_json(
    info: &chimaera_pty::SessionInfo,
    workspace_id: Option<String>,
    agent: Option<&crate::agents::AgentRecord>,
    polled: Option<&str>,
    polled_cwd: Option<&std::path::Path>,
    exec_stage: Option<chimaera_pty::ExecStage>,
) -> serde_json::Value {
    let mut map = match serde_json::to_value(info) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    map.insert(
        "cwd_current".to_string(),
        json!(polled_cwd.unwrap_or(&info.cwd)),
    );
    map.insert(
        "exec_stage".to_string(),
        exec_stage.map_or(serde_json::Value::Null, |s| json!(s)),
    );
    map.insert(
        "workspace_id".to_string(),
        workspace_id.map_or(serde_json::Value::Null, serde_json::Value::String),
    );
    map.insert(
        "kind".to_string(),
        json!(if agent.is_some() { "agent" } else { "shell" }),
    );
    // Which agent CLI the session runs ("claude"/"codex"/"gemini") so the
    // UI can glyph rows per agent; null for shells.
    map.insert(
        "agent_kind".to_string(),
        agent.map_or(serde_json::Value::Null, |a| json!(a.kind.as_str())),
    );
    map.insert(
        "agent_state".to_string(),
        agent.map_or(serde_json::Value::Null, |a| json!(a.state.as_str())),
    );
    map.insert(
        "agent_title".to_string(),
        agent
            .and_then(|a| a.title())
            .map_or(serde_json::Value::Null, |t| json!(t)),
    );
    map.insert(
        "files_touched".to_string(),
        agent.map_or(serde_json::Value::Null, |a| json!(a.files_touched)),
    );
    // Naming rule zero: the most specific thing known about what the session
    // is DOING. A user-pinned name stays authoritative (`renamed` flags the
    // pin for the UI); agents and shells resolve their own chains.
    let display_name = if info.renamed {
        info.name.clone()
    } else {
        match agent {
            Some(agent) => agent.display_name(info.title.as_deref()),
            None => crate::naming::shell_display_name(info, polled),
        }
    };
    map.insert("display_name".to_string(), json!(display_name));
    serde_json::Value::Object(map)
}

/// The full session list as JSON values (shared by GET /sessions and the
/// /ws/events snapshots): PTY rows plus synthetic rows for structured chat
/// sessions, sorted by creation time so the rail interleaves them honestly.
/// Lock order: session_workspaces -> agents -> display_names ->
/// current_cwds -> exec_status.
pub(crate) fn sessions_json(state: &AppState) -> Vec<serde_json::Value> {
    let sessions = state.sessions.list();
    let chats = state.chat.list();
    let workspaces = crate::lock(&state.session_workspaces);
    let agents = crate::lock(&state.agents);
    let names = crate::lock(&state.display_names);
    let cwds = crate::lock(&state.current_cwds);
    let execs = crate::lock(&state.exec_status);
    let mut rows: Vec<(u64, serde_json::Value)> = sessions
        .iter()
        .map(|info| {
            let mut row = session_json(
                info,
                workspaces.get(&info.id).cloned(),
                agents.get(&info.id),
                names.get(&info.id).map(String::as_str),
                cwds.get(&info.id).map(PathBuf::as_path),
                execs.get(&info.id).copied(),
            );
            if let serde_json::Value::Object(map) = &mut row {
                map.insert("ui".to_string(), json!("term"));
                let chat_capable = agents.get(&info.id).is_some_and(|a| {
                    matches!(
                        a.kind,
                        crate::agents::AgentKind::Claude | crate::agents::AgentKind::Codex
                    )
                });
                map.insert("chat_capable".to_string(), json!(chat_capable));
            }
            (info.created_at, row)
        })
        .collect();
    rows.extend(chats.iter().map(|info| {
        (
            info.created_at_ms / 1000,
            crate::chat::chat_session_json(
                info,
                workspaces.get(&info.id).cloned(),
                agents.get(&info.id),
            ),
        )
    }));
    // Mid view-switch a session lives in NEITHER registry for a moment
    // (old process killed, new one not yet spawned). A vanishing row would
    // make every window prune the session's tabs, so synthesize a
    // placeholder carrying the TARGET surface until the respawn registers.
    for (id, target) in crate::lock(&state.chat_switching).iter() {
        if rows.iter().any(|(_, row)| row["id"] == json!(id)) {
            continue;
        }
        let Some(record) = agents.get(id) else {
            continue;
        };
        rows.push((
            u64::MAX,
            json!({
                "id": id,
                "name": record.kind.as_str(),
                "cwd": "",
                "cols": 0,
                "rows": 0,
                "created_at": 0,
                "alive": true,
                "exit_status": null,
                "title": null,
                "pid": null,
                "renamed": false,
                "phase": "unknown",
                "cwd_current": "",
                "exec_stage": null,
                "workspace_id": workspaces.get(id),
                "kind": "agent",
                "agent_kind": record.kind.as_str(),
                "agent_state": record.state.as_str(),
                "agent_title": record.title(),
                "files_touched": record.files_touched,
                "display_name": record.display_name(None),
                "ui": target,
                "chat_capable": true,
            }),
        ));
    }
    rows.sort_by_key(|(created, _)| *created);
    rows.into_iter().map(|(_, row)| row).collect()
}

/// GET /api/v1/sessions
pub(crate) async fn list_sessions(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // Mid-restore the roster is a lie — and this endpoint feeds the remote
    // update decision, where an undercount reads as "safe to replace the
    // daemon" and would kill the very sessions being resurrected.
    state.wait_restored().await;
    Json(serde_json::Value::Array(sessions_json(&state)))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionKind {
    #[default]
    Shell,
    Agent,
}

#[derive(Deserialize)]
pub(crate) struct CreateSession {
    workspace_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: SessionKind,
    /// Which agent CLI a kind "agent" session runs; "claude" when omitted.
    #[serde(default)]
    agent: Option<String>,
    /// Model id passed as `--model <id>` (curated list per agent from
    /// GET /api/v1/agents); the agent's own default when omitted.
    #[serde(default)]
    model: Option<String>,
    /// Claude session id to resume (`--resume <id>`, claude-only), from
    /// GET /api/v1/agents/claude/sessions.
    #[serde(default)]
    resume: Option<String>,
    /// Initial PTY size, clamped to sane bounds. The UI passes the focused
    /// pane's fitted size so TUIs never boot at a wrong size (claude's boot
    /// banner rendered at 80x24 then reflowed was a real observed artifact).
    #[serde(default)]
    cols: Option<u16>,
    #[serde(default)]
    rows: Option<u16>,
    /// The client's current color scheme ("light"|"dark", from
    /// prefers-color-scheme at spawn). Drives `CHIMAERA_THEME` in the
    /// session env and the per-CLI theme injection; defaults to dark.
    #[serde(default)]
    theme: Option<String>,
    /// Which surface an agent session runs behind: "term" (default — the
    /// real TUI in a PTY) or "chat" (the structured stream-json driver).
    /// Old clients never send it and keep getting terminals.
    #[serde(default)]
    ui: Option<String>,
}

/// The environment every chimaera-spawned session gets: the shim dir
/// prepended to PATH (theming + future adoption for typed agents; spawn env
/// only — user dotfiles are never touched), the session's own id, and the
/// client's color scheme. `CHIMAERA_SHIMS` lets the login-shell wrap
/// re-prepend the shim dir after profile init reorders PATH.
pub(crate) fn session_env(
    state: &AppState,
    session_id: &str,
    theme: &str,
) -> Vec<(String, String)> {
    let shims = state.shims_dir.display().to_string();
    let inherited = std::env::var("PATH").unwrap_or_default();
    vec![
        ("PATH".to_string(), spawn_path(&shims, &inherited)),
        ("CHIMAERA_SESSION".to_string(), session_id.to_string()),
        ("CHIMAERA_THEME".to_string(), theme.to_string()),
        ("CHIMAERA_SHIMS".to_string(), shims),
    ]
}

/// Inherited env vars to REMOVE from every spawned session: markers that
/// describe the DAEMON's launcher, not the session. When the daemon was
/// started from inside a Claude Code session (dev loops do this all the
/// time), `CLAUDE_CODE_SESSION_ID`/`CLAUDE_CODE_CHILD_SESSION` leak through
/// and make every claude spawned under it believe it is a nested child
/// session — and a child-marked interactive claude persists NO transcript
/// (verified against claude 2.1.204; the two markers bisected live), so
/// conversations silently lose `--resume`. The whole CLAUDE* family goes:
/// none of it can describe a chimaera session truthfully, and anything the
/// user set in their own profile comes back through the login-shell wrap.
pub(crate) fn launcher_context_env() -> Vec<String> {
    std::env::vars()
        .map(|(name, _)| name)
        .filter(|name| {
            name == "CLAUDECODE"
                || name == "CLAUDE_EFFORT"
                || name == "AI_AGENT"
                || name.starts_with("CLAUDE_CODE_")
                || name.starts_with("CLAUDE_AGENT_")
        })
        .collect()
}

/// The spawned session's PATH: shim dir first, then the daemon's inherited
/// PATH — or the fixed system default when that is empty. A bare
/// "{shims}:" tail is an empty final PATH member, which sh searches as
/// the cwd (measured: a repo-local ./curl ran inside an install session).
pub(crate) fn spawn_path(shims: &str, inherited: &str) -> String {
    if inherited.is_empty() {
        format!("{shims}:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
    } else {
        format!("{shims}:{inherited}")
    }
}

#[cfg(test)]
mod scrub_tests {
    /// The scrub list contains exactly the launcher-context markers present
    /// in the daemon's environment — nothing else (a session must keep the
    /// rest of its inherited env).
    #[test]
    fn launcher_context_env_matches_claude_markers_only() {
        // Unique-ish names; set_var is process-global but nothing else in
        // this test binary reads them.
        std::env::set_var("CLAUDE_CODE_CHILD_SESSION", "1");
        std::env::set_var("CLAUDE_AGENT_SDK_VERSION", "0.0.0");
        std::env::set_var("CHIMAERA_SCRUB_TEST_INNOCENT", "keep");
        let scrub = super::launcher_context_env();
        assert!(scrub.iter().any(|n| n == "CLAUDE_CODE_CHILD_SESSION"));
        assert!(scrub.iter().any(|n| n == "CLAUDE_AGENT_SDK_VERSION"));
        assert!(
            !scrub.iter().any(|n| n.starts_with("CHIMAERA")),
            "only claude-context markers are scrubbed: {scrub:?}"
        );
        assert!(!scrub.iter().any(|n| n == "PATH" || n == "HOME"));
        std::env::remove_var("CLAUDE_CODE_CHILD_SESSION");
        std::env::remove_var("CLAUDE_AGENT_SDK_VERSION");
        std::env::remove_var("CHIMAERA_SCRUB_TEST_INNOCENT");
    }
}

/// POST /api/v1/sessions — spawn a shell (kind "shell", the default) or an
/// agent TUI (kind "agent") at the workspace root. Claude agents get the
/// injected hook settings (and `--model`/`--resume` when given); codex and
/// gemini spawn their TUIs as plain PTY sessions — hook-driven attention
/// state stays claude-only until their integrations land. Validation lives
/// here; the spawn itself is `spawn::spawn_session`, shared with boot
/// resurrection.
pub(crate) async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateSession>,
) -> Response {
    // Touch doubles as the lookup: activity in a workspace is what "recently
    // used" means on the home screen.
    let Some(workspace) = crate::lock(&state.workspaces).touch(&body.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {}", body.workspace_id)})),
        )
            .into_response();
    };

    let bad_request =
        |msg: String| (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response();

    // The client's scheme at spawn; dark when it does not say. Owned (not a
    // borrow of `body`) so the chat path can move `body` while still naming it.
    let theme_owned: String = match body.theme.as_deref() {
        None => "dark".to_string(),
        Some(t @ ("light" | "dark")) => t.to_string(),
        Some(other) => {
            return bad_request(format!("invalid theme {other:?} (expected light or dark)"));
        }
    };
    let theme: &str = &theme_owned;

    // Structured chat surface: an agent driven over stream-json/app-server,
    // not a PTY. It is resolved and spawned here (returning early); the TUI
    // path — shells and terminal agents — flows through spawn::spawn_session
    // below, shared with boot resurrection.
    let chat_ui = match body.ui.as_deref() {
        None | Some("term") => false,
        Some("chat") => {
            if body.kind != SessionKind::Agent {
                return bad_request("ui \"chat\" requires kind \"agent\"".to_string());
            }
            true
        }
        Some(other) => {
            return bad_request(format!("invalid ui {other:?} (expected chat or term)"));
        }
    };
    if chat_ui {
        return spawn_chat_ui(&state, body, workspace, theme).await;
    }

    let kind = match body.kind {
        SessionKind::Shell => crate::spawn::SpawnKind::Shell,
        SessionKind::Agent => {
            let agent_kind = match &body.agent {
                None => crate::agents::AgentKind::Claude,
                Some(name) => match crate::agents::AgentKind::parse(name) {
                    Some(kind) => kind,
                    None => {
                        // Derived, not hand-written: the catalog moves.
                        let known: Vec<&str> = crate::agents::AgentKind::ALL
                            .iter()
                            .map(|k| k.as_str())
                            .collect();
                        return bad_request(format!(
                            "unknown agent {name:?} (expected one of {})",
                            known.join(", ")
                        ));
                    }
                },
            };
            if body.resume.is_some() && agent_kind != crate::agents::AgentKind::Claude {
                return bad_request("resume is only supported for claude sessions".to_string());
            }
            // Argv cannot shell-inject, but flag-shaped or control-byte values
            // have no business in --model/--resume.
            for (field, value) in [("model", &body.model), ("resume", &body.resume)] {
                if let Some(value) = value {
                    if !crate::launcher::safe_arg(value) {
                        return bad_request(format!("invalid {field} {value:?}"));
                    }
                }
            }
            crate::spawn::SpawnKind::Agent {
                kind: agent_kind,
                model: body.model,
                resume: body.resume,
            }
        }
    };

    let spec = crate::spawn::SpawnSpec {
        workspace,
        id: None,
        name: body.name,
        cwd: None,
        cols: body.cols,
        rows: body.rows,
        theme: theme.to_string(),
        title_hint: None,
        kind,
    };
    match crate::spawn::spawn_session(&state, spec).await {
        Ok(session) => Json(session).into_response(),
        Err(crate::spawn::SpawnFailure::AgentUnavailable(msg)) => {
            (StatusCode::CONFLICT, Json(json!({"error": msg}))).into_response()
        }
        Err(crate::spawn::SpawnFailure::Internal(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

/// Spawn an agent on the structured chat surface (claude stream-json / codex
/// app-server), returning the same JSON `GET /sessions` lists it with. Same
/// identity plumbing as `spawn::spawn_session`'s agent arm (id, hook key,
/// settings/mcp files, AgentRecord, workspace mapping, watcher), but the
/// process is a driver owned by the chat manager, not a PTY. The caller has
/// already validated that this is an agent session with `ui:"chat"`.
async fn spawn_chat_ui(
    state: &Arc<AppState>,
    body: CreateSession,
    workspace: crate::workspaces::Workspace,
    theme: &str,
) -> Response {
    let bad_request =
        |msg: String| (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response();
    let internal = |err: anyhow::Error| {
        tracing::error!(%err, "failed to spawn chat session");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response()
    };

    let agent_kind = match &body.agent {
        None => crate::agents::AgentKind::Claude,
        Some(name) => match crate::agents::AgentKind::parse(name) {
            Some(kind) => kind,
            None => {
                let known: Vec<&str> = crate::agents::AgentKind::ALL
                    .iter()
                    .map(|k| k.as_str())
                    .collect();
                return bad_request(format!(
                    "unknown agent {name:?} (expected one of {})",
                    known.join(", ")
                ));
            }
        },
    };
    // Only claude and codex speak a structured protocol today.
    if !matches!(
        agent_kind,
        crate::agents::AgentKind::Claude | crate::agents::AgentKind::Codex
    ) {
        return bad_request(format!(
            "chat view not yet available for {}",
            agent_kind.as_str()
        ));
    }
    // Resume: claude anywhere (transcript store); codex only through the chat
    // driver (thread/resume is in-protocol). Gemini et al. can't resume here.
    if body.resume.is_some()
        && !matches!(
            agent_kind,
            crate::agents::AgentKind::Claude | crate::agents::AgentKind::Codex
        )
    {
        return bad_request("resume is not supported for this agent".to_string());
    }
    for (field, value) in [("model", &body.model), ("resume", &body.resume)] {
        if let Some(value) = value {
            if !crate::launcher::safe_arg(value) {
                return bad_request(format!("invalid {field} {value:?}"));
            }
        }
    }

    let id = crate::agents::fresh_session_id();
    let bin = match crate::launcher::detect(state, agent_kind, false).await.path {
        Ok(path) => path,
        Err(msg) => return (StatusCode::CONFLICT, Json(json!({"error": msg}))).into_response(),
    };
    let key = crate::agents::fresh_agent_key();
    // Hook injection + theme (claude-only), unless the user's settings pick a
    // theme themselves.
    let settings = if agent_kind == crate::agents::AgentKind::Claude {
        let settings_theme =
            (!crate::runtimes::claude_user_theme_set(&state.claude_settings_path)).then_some(theme);
        match crate::agents::write_settings(&id, &key, state.port, settings_theme) {
            Ok(path) => Some(path),
            Err(err) => return internal(err),
        }
    } else {
        None
    };
    let mcp_config = if agent_kind == crate::agents::AgentKind::Claude {
        match crate::agents::write_mcp_config(&id, &key, state.port) {
            Ok(path) => Some(path),
            Err(err) => return internal(err),
        }
    } else {
        None
    };

    let mut record = crate::agents::AgentRecord::new(key, agent_kind);
    record.resumed_from = body.resume.clone();
    crate::lock(&state.agents).insert(id.clone(), record.clone());
    crate::lock(&state.session_workspaces).insert(id.clone(), workspace.id.clone());
    let recipe = crate::chat::ChatRecipe {
        workspace_root: workspace.root.clone(),
        kind: agent_kind,
        bin,
        settings,
        mcp_config,
        model: body.model.clone(),
        resume: body.resume.clone(),
        fork_at: None,
        theme: theme.to_string(),
    };
    match crate::chat::spawn_chat_session(state, id.clone(), recipe, None) {
        Ok(info) => {
            crate::agents::spawn_agent_watch(state.clone(), id.clone());
            state.changes.notify_waiters();
            Json(crate::chat::chat_session_json(
                &info,
                Some(workspace.id),
                Some(&record),
            ))
            .into_response()
        }
        Err(err) => {
            crate::lock(&state.agents).remove(&id);
            crate::lock(&state.session_workspaces).remove(&id);
            internal(err)
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct RenameSession {
    name: String,
}

/// PATCH /api/v1/sessions/{id} — pin a user-chosen display name. Works for
/// EVERY session kind (shells, codex, gemini, agy — not just claude, whose
/// own /rename happens to flow through OSC titles); the pin outranks every
/// derived name on every surface.
pub(crate) async fn rename_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<RenameSession>,
) -> Response {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 200 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "name must be 1-200 characters"})),
        )
            .into_response();
    }
    // Chat sessions have no PTY row, so the pin lives on the AgentRecord —
    // `customTitle` outranks every derived name (same field claude's own
    // /rename writes). This is what makes `/rename` and the rail's inline
    // rename work for chat sessions, codex included (it has no in-agent
    // rename at all).
    if state.chat.get(&id).is_some() {
        let mut agents = crate::lock(&state.agents);
        return match agents.get_mut(&id) {
            Some(record) => {
                record.custom_title = Some(name.to_string());
                drop(agents);
                state.changes.notify_waiters();
                StatusCode::NO_CONTENT.into_response()
            }
            None => (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("unknown session {id}")})),
            )
                .into_response(),
        };
    }
    match state.sessions.rename(&id, name.to_string()) {
        Ok(()) => {
            state.changes.notify_waiters();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE /api/v1/sessions/{id}
pub(crate) async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    // Chat sessions: ask the driver to stop (the exit path retires the
    // record). A driver already dead (kept visible after a protocol error)
    // is removed and retired directly.
    if let Some(info) = state.chat.get(&id) {
        if info.alive {
            state.chat.kill(&id);
        } else {
            state.chat.remove(&id);
            crate::recents::retire(&state, &id, None, None);
        }
        state.changes.notify_waiters();
        return StatusCode::NO_CONTENT.into_response();
    }
    match state.sessions.kill(&id) {
        Ok(()) => {
            state.changes.notify_waiters();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE /api/v1/sessions — end every live session; the daemon stays up.
/// This is "kill everything here" without the teardown: the caller can start
/// fresh work immediately, no reconnect. Returns how many were ended.
pub(crate) async fn delete_all_sessions(State(state): State<Arc<AppState>>) -> Response {
    let killed = state.sessions.kill_all();
    if killed > 0 {
        state.changes.notify_waiters();
    }
    Json(json!({ "killed": killed })).into_response()
}

/// POST /api/v1/shutdown — end every session, then stop the daemon.
///
/// The kill has to complete BEFORE the process exits: a session that ignores
/// SIGHUP is force-killed on a detached thread (see `kill_all`) that would die
/// with the daemon, so exiting immediately could orphan it and reparent it to
/// init. So we SIGHUP everything now, reply at once (the caller's tunnel is
/// about to drop with the daemon), and let a background task outlast the
/// escalation grace before tripping the graceful-shutdown future.
pub(crate) async fn shutdown(State(state): State<Arc<AppState>>) -> Response {
    let killed = state.sessions.kill_all();
    if killed > 0 {
        state.changes.notify_waiters();
    }
    tracing::info!("in-band shutdown requested; ending {killed} session(s) then stopping");
    tokio::spawn(async move {
        tokio::time::sleep(
            chimaera_pty::KILL_ESCALATION_GRACE + std::time::Duration::from_millis(500),
        )
        .await;
        state.shutdown.notify_one();
    });
    Json(json!({ "killed": killed, "shutdown": true })).into_response()
}

/// Foreground commands that forward keystrokes to a shell somewhere else
/// (remote or containerized), making sentinel-typing over a `running` phase
/// safe. Anything else running in the foreground (sleep, vim, tail) refuses.
const SENTINEL_FOREGROUNDS: &[&str] = &[
    "ssh",
    "mosh",
    "mosh-client",
    "et",
    "docker",
    "podman",
    "kubectl",
    "oc",
    "singularity",
    "apptainer",
];

#[derive(Deserialize)]
pub(crate) struct ExecBody {
    command: String,
    /// Max runtime once typed (default 30s, capped at 1h).
    #[serde(default)]
    timeout_ms: Option<u64>,
    /// Max wait for the shell's prompt before typing (default 15s, capped at
    /// 10m). `0` means "only if free right now".
    #[serde(default)]
    queue_timeout_ms: Option<u64>,
}

/// POST /api/v1/sessions/{id}/exec — type a command into a live shell
/// session and wait for its outcome (the `run_in_terminal` mechanics).
pub(crate) async fn exec_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<ExecBody>,
) -> Response {
    // Agent sessions host a TUI, not a shell; typing commands into claude
    // would be chaos. Links (and this endpoint) are for terminals only.
    if crate::lock(&state.agents).contains_key(&id) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "cannot exec into an agent session"})),
        )
            .into_response();
    }
    if state.sessions.get(&id).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown session {id}")})),
        )
            .into_response();
    }

    let outcome = run_exec(
        &state,
        &id,
        body.command,
        body.timeout_ms,
        body.queue_timeout_ms,
    )
    .await;

    match outcome {
        Ok(outcome) => Json(json!(outcome)).into_response(),
        Err(err) => {
            let code = match &err {
                chimaera_pty::ExecError::Busy(_) => StatusCode::CONFLICT,
                chimaera_pty::ExecError::InvalidCommand(_) => StatusCode::BAD_REQUEST,
                chimaera_pty::ExecError::SessionGone => StatusCode::NOT_FOUND,
                chimaera_pty::ExecError::NeverStarted(_) => StatusCode::GATEWAY_TIMEOUT,
            };
            (code, Json(json!({"error": err.to_string()}))).into_response()
        }
    }
}

/// Run an exec with the server's policy attached: sentinel over a *running*
/// phase only when the foreground forwards keystrokes elsewhere (ssh and
/// friends), and the stage (queued/executing) mirrored into session
/// snapshots for the UI's linked-terminal chips. Shared by the REST
/// endpoint and the MCP `run_in_terminal` tool.
pub(crate) async fn run_exec(
    state: &Arc<AppState>,
    id: &str,
    command: String,
    timeout_ms: Option<u64>,
    queue_timeout_ms: Option<u64>,
) -> Result<chimaera_pty::ExecOutcome, chimaera_pty::ExecError> {
    let allow_sentinel_over_running = state
        .sessions
        .foreground_pid(id)
        .and_then(crate::naming::comm_name)
        .is_some_and(|comm| SENTINEL_FOREGROUNDS.contains(&comm.as_str()));

    let (stage_tx, mut stage_rx) = tokio::sync::watch::channel(chimaera_pty::ExecStage::Queued);
    let mirror = {
        let state = state.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            loop {
                let stage = *stage_rx.borrow_and_update();
                crate::lock(&state.exec_status).insert(id.clone(), stage);
                state.changes.notify_waiters();
                if stage_rx.changed().await.is_err() {
                    return;
                }
            }
        })
    };

    let outcome = state
        .sessions
        .exec(
            id,
            chimaera_pty::ExecOptions {
                command,
                queue_timeout: std::time::Duration::from_millis(
                    queue_timeout_ms.unwrap_or(15_000).min(600_000),
                ),
                timeout: std::time::Duration::from_millis(
                    timeout_ms.unwrap_or(30_000).min(3_600_000),
                ),
                allow_sentinel_over_running,
                stage: Some(stage_tx),
            },
        )
        .await;

    mirror.abort();
    crate::lock(&state.exec_status).remove(id);
    state.changes.notify_waiters();
    outcome
}

#[derive(Deserialize)]
pub(crate) struct JournalQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// GET /api/v1/sessions/{id}/journal — the session's command journal (what
/// `read_terminal` returns): recent commands with output, exit codes, cwd.
pub(crate) async fn session_journal(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<JournalQuery>,
) -> Response {
    let Some(marks) = state.sessions.marks(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown session {id}")})),
        )
            .into_response();
    };
    let limit = query.limit.unwrap_or(20).min(200);
    Json(json!({
        "phase": marks.phase(),
        "entries": marks.journal(limit),
    }))
    .into_response()
}
