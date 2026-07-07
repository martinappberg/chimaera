use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Request, State};
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
/// /ws/events snapshots). Lock order: session_workspaces -> agents ->
/// display_names -> current_cwds.
pub(crate) fn sessions_json(state: &AppState) -> Vec<serde_json::Value> {
    let sessions = state.sessions.list();
    let workspaces = crate::lock(&state.session_workspaces);
    let agents = crate::lock(&state.agents);
    let names = crate::lock(&state.display_names);
    let cwds = crate::lock(&state.current_cwds);
    sessions
        .iter()
        .map(|info| {
            session_json(
                info,
                workspaces.get(&info.id).cloned(),
                agents.get(&info.id),
                names.get(&info.id).map(String::as_str),
                cwds.get(&info.id).map(PathBuf::as_path),
            )
        })
        .collect()
}

/// GET /api/v1/sessions
pub(crate) async fn list_sessions(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
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
}

/// POST /api/v1/sessions — spawn a shell (kind "shell", the default) or an
/// agent TUI (kind "agent") at the workspace root. Claude agents get the
/// injected hook settings (and `--model`/`--resume` when given); codex and
/// gemini spawn their TUIs as plain PTY sessions — hook-driven attention
/// state stays claude-only until their integrations land.
pub(crate) async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateSession>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&body.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {}", body.workspace_id)})),
        )
            .into_response();
    };

    let bad_request =
        |msg: String| (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response();

    let mut opts = chimaera_pty::SpawnOpts {
        cwd: workspace.root.clone(),
        name: body.name,
        cols: body.cols.map_or(80, |c| c.clamp(20, 500)),
        rows: body.rows.map_or(24, |r| r.clamp(5, 200)),
        command: None,
        id: None,
    };

    // Agent sessions: resolve the agent binary (cached, via the login
    // shell), pre-pick the session id so the hook URL can embed it, and —
    // for claude — generate the per-session settings file that wires its
    // hooks to this daemon.
    let mut spawned_agent = None;
    if body.kind == SessionKind::Agent {
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

        let bin = match crate::launcher::detect(&state, agent_kind, false)
            .await
            .path
        {
            Ok(path) => path,
            Err(msg) => {
                return (StatusCode::CONFLICT, Json(json!({"error": msg}))).into_response();
            }
        };
        let id = crate::agents::fresh_session_id();
        let key = crate::agents::fresh_agent_key();
        // Hook injection is claude-only: other agents have no hook system
        // to wire, so their sessions stay honestly "unknown".
        let settings = if agent_kind == crate::agents::AgentKind::Claude {
            match crate::agents::write_settings(&id, &key, state.port) {
                Ok(path) => Some(path),
                Err(err) => {
                    tracing::error!(%err, "failed to write agent settings");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": err.to_string()})),
                    )
                        .into_response();
                }
            }
        } else {
            None
        };
        // Login-shell wrap: agents must see the user's terminal environment
        // (exported API keys, nvm PATHs) — the daemon's own env never
        // sourced their profile.
        opts.command = Some(crate::launcher::wrap_login_shell(
            &crate::launcher::login_shell(),
            crate::launcher::build_agent_command(
                agent_kind,
                &bin,
                settings.as_deref(),
                body.model.as_deref(),
                body.resume.as_deref(),
            ),
        ));
        opts.id = Some(id.clone());
        // Register the record before spawning so no hook can beat it in.
        let mut record = crate::agents::AgentRecord::new(key.clone(), agent_kind);
        // Claude forks a new session id on --resume; remember the ancestor
        // so recents can hide (and later supersede) the old conversation.
        record.resumed_from = body.resume.clone();
        crate::lock(&state.agents).insert(id, record);
        spawned_agent = Some((key, agent_kind));
    }

    match state.sessions.spawn(opts.clone()) {
        Ok(info) => {
            crate::lock(&state.session_workspaces).insert(info.id.clone(), workspace.id.clone());
            let agent = spawned_agent.map(|(key, kind)| crate::agents::AgentRecord::new(key, kind));
            let mut polled = None;
            if agent.is_some() {
                crate::agents::spawn_agent_watch(state.clone(), info.id.clone());
            } else {
                // Prime the display name (a fresh shell sits at the root, so
                // it is the shell itself) and start the naming watcher.
                let shell = crate::naming::default_shell_name();
                crate::lock(&state.display_names).insert(info.id.clone(), shell.clone());
                polled = Some(shell);
                crate::naming::spawn_shell_watch(state.clone(), info.id.clone());
            }
            state.changes.notify_waiters();
            Json(session_json(
                &info,
                Some(workspace.id),
                agent.as_ref(),
                polled.as_deref(),
                None, // fresh session: cwd_current is the spawn cwd
            ))
            .into_response()
        }
        Err(err) => {
            if let Some(id) = &opts.id {
                crate::lock(&state.agents).remove(id);
            }
            tracing::error!(%err, "failed to spawn session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}

/// DELETE /api/v1/sessions/{id}
pub(crate) async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
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
