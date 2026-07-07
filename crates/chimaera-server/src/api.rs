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
/// /ws/events snapshots). Lock order: session_workspaces -> agents ->
/// display_names -> current_cwds -> exec_status.
pub(crate) fn sessions_json(state: &AppState) -> Vec<serde_json::Value> {
    let sessions = state.sessions.list();
    let workspaces = crate::lock(&state.session_workspaces);
    let agents = crate::lock(&state.agents);
    let names = crate::lock(&state.display_names);
    let cwds = crate::lock(&state.current_cwds);
    let execs = crate::lock(&state.exec_status);
    sessions
        .iter()
        .map(|info| {
            session_json(
                info,
                workspaces.get(&info.id).cloned(),
                agents.get(&info.id),
                names.get(&info.id).map(String::as_str),
                cwds.get(&info.id).map(PathBuf::as_path),
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

/// POST /api/v1/sessions — spawn a shell (kind "shell", the default) or an
/// agent TUI (kind "agent") at the workspace root. Claude agents get the
/// injected hook settings (and `--model`/`--resume` when given); codex and
/// gemini spawn their TUIs as plain PTY sessions — hook-driven attention
/// state stays claude-only until their integrations land.
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

    // The client's scheme at spawn; dark when it does not say.
    let theme = match body.theme.as_deref() {
        None => "dark",
        Some(t @ ("light" | "dark")) => t,
        Some(other) => {
            return bad_request(format!("invalid theme {other:?} (expected light or dark)"));
        }
    };

    // Every session gets a pre-picked id: it rides in the spawn env as
    // CHIMAERA_SESSION (shells too — typed agents need their session
    // context) and, for claude, in the hook URL.
    let id = crate::agents::fresh_session_id();
    let mut opts = chimaera_pty::SpawnOpts {
        cwd: workspace.root.clone(),
        name: body.name,
        cols: body.cols.map_or(80, |c| c.clamp(20, 500)),
        rows: body.rows.map_or(24, |r| r.clamp(5, 200)),
        command: None,
        id: Some(id.clone()),
        env: session_env(&state, &id, theme),
    };

    // Plain shells get shell integration injected (OSC 133 journal marks);
    // a failure to materialize the scripts degrades to a plain spawn. Its
    // env lands ON TOP of the session env (shims PATH, CHIMAERA_*) — the
    // two use disjoint variable sets, so nothing is clobbered.
    if body.kind == SessionKind::Shell {
        match chimaera_core::shellint::shell_launch() {
            Ok(launch) => {
                opts.command = Some(launch.argv);
                opts.env.extend(launch.env);
            }
            Err(err) => {
                tracing::warn!(%err, "shell integration unavailable; spawning plain shell");
            }
        }
    }

    // Agent sessions: resolve the agent binary (cached, via the login
    // shell; user install first, managed fallback), and — for claude —
    // generate the per-session settings file that wires its hooks to this
    // daemon and carries the scheme theme.
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
        let key = crate::agents::fresh_agent_key();
        // Hook injection is claude-only: other agents have no hook system
        // to wire, so their sessions stay honestly "unknown". The scheme
        // theme rides in the same settings file — unless the user's own
        // settings already set one (respect the explicit choice).
        let settings = if agent_kind == crate::agents::AgentKind::Claude {
            let settings_theme =
                (!crate::runtimes::claude_user_theme_set(&state.claude_settings_path))
                    .then_some(theme);
            match crate::agents::write_settings(&id, &key, state.port, settings_theme) {
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
        // Codex themes via `-c tui.theme=` (config-file override, verified
        // against codex 0.142.5); skipped when the user's own config.toml
        // picks a theme.
        let codex_theme = (agent_kind == crate::agents::AgentKind::Codex
            && !crate::runtimes::codex_user_theme_set(&state.codex_config_path))
        .then(|| crate::runtimes::codex_theme_name(theme));
        // Claude also carries the linked-terminals MCP config (per-session
        // endpoint + key); other agents' MCP integrations come later.
        let mcp_config = if agent_kind == crate::agents::AgentKind::Claude {
            match crate::agents::write_mcp_config(&id, &key, state.port) {
                Ok(path) => Some(path),
                Err(err) => {
                    tracing::error!(%err, "failed to write agent mcp config");
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
        let mut argv = crate::launcher::build_agent_command(
            agent_kind,
            &bin,
            settings.as_deref(),
            body.model.as_deref(),
            body.resume.as_deref(),
            codex_theme,
        );
        if let Some(mcp) = &mcp_config {
            argv.push("--mcp-config".to_string());
            argv.push(mcp.to_string_lossy().into_owned());
        }
        // Login-shell wrap: agents must see the user's terminal environment
        // (exported API keys, nvm PATHs) — the daemon's own env never
        // sourced their profile.
        opts.command = Some(crate::launcher::wrap_login_shell(
            &crate::launcher::login_shell(),
            argv,
        ));
        // Register the record before spawning so no hook can beat it in.
        let mut record = crate::agents::AgentRecord::new(key.clone(), agent_kind);
        // Claude forks a new session id on --resume; remember the ancestor
        // so recents can hide (and later supersede) the old conversation.
        record.resumed_from = body.resume.clone();
        crate::lock(&state.agents).insert(id.clone(), record);
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
                None, // no exec in flight
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
