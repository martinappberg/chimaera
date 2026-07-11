use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::session_view::sessions_json;
use crate::AppState;

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
    /// A display title to seed a resumed conversation with — the recents row's
    /// title, carried across the fresh session id an "open recent" mints. Seeds
    /// the soft `ai_title` (a later `generate_session_title` can still refine
    /// it), unlike `name`, which pins `custom_title`. Bounded server-side.
    #[serde(default)]
    title_hint: Option<String>,
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
        title_hint: body
            .title_hint
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(crate::agents::truncate_prompt),
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
    // Only chat-capable agents (claude stream-json / codex app-server) reach
    // the chat surface. Both support resume here — claude via its transcript
    // store, codex in-protocol (thread/resume) — so no separate resume guard
    // is needed past this point (gemini et al. are refused outright).
    if !agent_kind.chat_capable() {
        return bad_request(format!(
            "chat view not yet available for {}",
            agent_kind.as_str()
        ));
    }
    for (field, value) in [("model", &body.model), ("resume", &body.resume)] {
        if let Some(value) = value {
            if !crate::launcher::safe_arg(value) {
                return bad_request(format!("invalid {field} {value:?}"));
            }
        }
    }

    let id = crate::agents::fresh_session_id();
    // Take the path AND its probed version from one detection so the chat
    // driver's version notice reflects the binary it actually spawns.
    let detection = crate::launcher::detect(state, agent_kind, false).await;
    let agent_version = detection.version.clone();
    let bin = match detection.path {
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
    // A name supplied at creation pins the row (customTitle authority), the
    // same as the TUI path's SpawnOpts.name — otherwise it is silently dropped
    // for chat sessions.
    if let Some(name) = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        record.custom_title = Some(name.to_string());
    }
    // A resumed recent carries its rail title so the restored conversation is
    // not a bare "claude" until a new turn regenerates one. Seed the soft
    // ai_title (a later `generate_session_title` still refines it) unless the
    // caller also pinned a name above, which outranks it.
    if record.custom_title.is_none() {
        if let Some(hint) = body
            .title_hint
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            record.ai_title = Some(crate::agents::truncate_prompt(hint));
        }
    }
    crate::lock(&state.agents).insert(id.clone(), record.clone());
    crate::lock(&state.session_workspaces).insert(id.clone(), workspace.id.clone());
    let recipe = crate::chat::ChatRecipe {
        workspace_root: workspace.root.clone(),
        kind: agent_kind,
        bin,
        version: agent_version,
        settings,
        mcp_config,
        model: body.model.clone(),
        resume: body.resume.clone(),
        fork_at: None,
        rollback_turns: None,
        theme: theme.to_string(),
    };

    // A resumed recent replays no history over the wire — seed the new journal
    // now (copy a prior chimaera journal, or reconstruct claude's own transcript)
    // so the chat can replay it. If a CLAUDE recent has nothing we can put in the
    // chat surface (transcript gone / unreadable / too old), open it in the
    // TERMINAL instead of a blank chat — claude's own TUI renders the resumed
    // conversation natively. codex resumes in-protocol, so it always chats.
    let seeded =
        tokio::task::block_in_place(|| crate::chat::seed_resumed_journal(state, &id, &recipe));
    if agent_kind == crate::agents::AgentKind::Claude && recipe.resume.is_some() && !seeded {
        tracing::info!(%id, "resumed claude recent has no chat history to render; opening in the terminal");
        crate::lock(&state.agents).remove(&id);
        crate::lock(&state.session_workspaces).remove(&id);
        // Discard the chat-only scaffolding (the temp settings/mcp for the id we drop).
        for path in [recipe.settings.as_deref(), recipe.mcp_config.as_deref()]
            .into_iter()
            .flatten()
        {
            let _ = std::fs::remove_file(path);
        }
        let spec = crate::spawn::SpawnSpec {
            workspace,
            id: None,
            name: body.name,
            cwd: None,
            cols: body.cols,
            rows: body.rows,
            theme: theme.to_string(),
            title_hint: body
                .title_hint
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(crate::agents::truncate_prompt),
            kind: crate::spawn::SpawnKind::Agent {
                kind: agent_kind,
                model: body.model,
                resume: body.resume,
            },
        };
        return match crate::spawn::spawn_session(state, spec).await {
            Ok(session) => Json(session).into_response(),
            Err(crate::spawn::SpawnFailure::AgentUnavailable(msg)) => {
                (StatusCode::CONFLICT, Json(json!({ "error": msg }))).into_response()
            }
            Err(crate::spawn::SpawnFailure::Internal(err)) => internal(err),
        };
    }

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
            // Shells never pass through `recents::retire` (that hook is
            // agent-only), so their uploads are pruned here.
            crate::upload::prune_session_uploads(&state, &id);
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
