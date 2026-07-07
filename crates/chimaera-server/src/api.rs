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
/// `agent_state`, `agent_title` and `display_name` fields. `agent` is the
/// wrapper record for kind "agent" sessions; `None` means a plain shell.
/// `polled` is the shell naming watcher's latest value, if any.
pub(crate) fn session_json(
    info: &chimaera_pty::SessionInfo,
    workspace_id: Option<String>,
    agent: Option<&crate::agents::AgentRecord>,
    polled: Option<&str>,
    exec_stage: Option<chimaera_pty::ExecStage>,
) -> serde_json::Value {
    let mut map = match serde_json::to_value(info) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
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
/// display_names.
pub(crate) fn sessions_json(state: &AppState) -> Vec<serde_json::Value> {
    let sessions = state.sessions.list();
    let workspaces = crate::lock(&state.session_workspaces);
    let agents = crate::lock(&state.agents);
    let names = crate::lock(&state.display_names);
    let execs = crate::lock(&state.exec_status);
    sessions
        .iter()
        .map(|info| {
            session_json(
                info,
                workspaces.get(&info.id).cloned(),
                agents.get(&info.id),
                names.get(&info.id).map(String::as_str),
                execs.get(&info.id).copied(),
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
    /// Initial PTY size, clamped to sane bounds. The UI passes the focused
    /// pane's fitted size so TUIs never boot at a wrong size (claude's boot
    /// banner rendered at 80x24 then reflowed was a real observed artifact).
    #[serde(default)]
    cols: Option<u16>,
    #[serde(default)]
    rows: Option<u16>,
}

/// POST /api/v1/sessions — spawn a shell (kind "shell", the default) or the
/// interactive claude TUI with injected hooks (kind "agent") at the
/// workspace root.
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

    let mut opts = chimaera_pty::SpawnOpts {
        cwd: workspace.root.clone(),
        name: body.name,
        cols: body.cols.map_or(80, |c| c.clamp(20, 500)),
        rows: body.rows.map_or(24, |r| r.clamp(5, 200)),
        command: None,
        id: None,
        env: Vec::new(),
    };

    // Plain shells get shell integration injected (OSC 133 journal marks);
    // a failure to materialize the scripts degrades to a plain spawn.
    if body.kind == SessionKind::Shell {
        match chimaera_core::shellint::shell_launch() {
            Ok(launch) => {
                opts.command = Some(launch.argv);
                opts.env = launch.env;
            }
            Err(err) => {
                tracing::warn!(%err, "shell integration unavailable; spawning plain shell");
            }
        }
    }

    // Agent sessions: resolve claude (once per daemon, via the login shell),
    // pre-pick the session id so the hook URL can embed it, and generate the
    // per-session settings file that wires claude's hooks to this daemon.
    let mut agent_key = None;
    if body.kind == SessionKind::Agent {
        let claude = state
            .claude_bin
            .get_or_init(crate::agents::resolve_claude)
            .await;
        let claude = match claude {
            Ok(path) => path.clone(),
            Err(msg) => {
                return (StatusCode::CONFLICT, Json(json!({"error": msg}))).into_response();
            }
        };
        let id = crate::agents::fresh_session_id();
        let key = crate::agents::fresh_agent_key();
        let settings = match crate::agents::write_settings(&id, &key, state.port) {
            Ok(path) => path,
            Err(err) => {
                tracing::error!(%err, "failed to write agent settings");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": err.to_string()})),
                )
                    .into_response();
            }
        };
        let mcp_config = match crate::agents::write_mcp_config(&id, &key, state.port) {
            Ok(path) => path,
            Err(err) => {
                tracing::error!(%err, "failed to write agent mcp config");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": err.to_string()})),
                )
                    .into_response();
            }
        };
        opts.command = Some(vec![
            claude.to_string_lossy().into_owned(),
            "--settings".to_string(),
            settings.to_string_lossy().into_owned(),
            "--mcp-config".to_string(),
            mcp_config.to_string_lossy().into_owned(),
        ]);
        opts.id = Some(id.clone());
        // Register the record before spawning so no hook can beat it in.
        crate::lock(&state.agents).insert(id, crate::agents::AgentRecord::new(key.clone()));
        agent_key = Some(key);
    }

    match state.sessions.spawn(opts.clone()) {
        Ok(info) => {
            crate::lock(&state.session_workspaces).insert(info.id.clone(), workspace.id.clone());
            let agent = agent_key.map(crate::agents::AgentRecord::new);
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
                None,
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

    let outcome = run_exec(&state, &id, body.command, body.timeout_ms, body.queue_timeout_ms).await;

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
