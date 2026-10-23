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
    /// Native conversation handle to resume (Claude session id / Codex thread
    /// id), from Recents or the Claude transcript catalog.
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
    /// Launch-scope environment prelude: opaque shell text run once before
    /// this session's shell/agent, after the host + workspace preludes (see
    /// `environment`). Old clients never send it; nothing changes shape.
    #[serde(default)]
    prelude: Option<String>,
    /// Where the session starts, when not the workspace root: a folder
    /// inside the workspace or one of its repository's worktrees (validated —
    /// never an arbitrary path). Old clients never send it.
    #[serde(default)]
    cwd: Option<String>,
    /// Start this agent in a NEW Chimaera-managed worktree of its own (a
    /// fresh `chimaera/<agent>-<hex>` branch off the workspace repository's
    /// HEAD; see `git::create_session_worktree`). Agents only, never with
    /// `resume` or `cwd`. Old clients never send it.
    #[serde(default)]
    worktree: bool,
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

    // Launch-scope prelude: opaque shell text by design, so only size and
    // NUL are policed (the same caps the stored scopes get on PUT).
    if let Some(p) = &body.prelude {
        if p.len() > crate::environment::MAX_SCOPE_BYTES {
            return bad_request(format!(
                "prelude exceeds {} bytes",
                crate::environment::MAX_SCOPE_BYTES
            ));
        }
        if p.contains('\0') {
            return bad_request("prelude text contains a NUL byte".to_string());
        }
    }

    // A requested starting folder must sit inside the workspace or one of
    // its worktrees; resolved (canonical) before either surface spawns.
    let mut start_cwd = match body.cwd.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        None => None,
        Some(raw) => match crate::git::allowed_session_cwd(&state, &workspace, raw).await {
            Ok(path) if path == workspace.root => None,
            Ok(path) => Some(path),
            Err(msg) => return bad_request(msg),
        },
    };

    // A new agent in a worktree of its own. Its branch and folder are settled
    // here, before either surface spawns. A terminal waits for the checkout;
    // a chat opens at once and its process starts once the files are there
    // (`AppState::worktree_setups`): the checkout grows with the repository,
    // and a click that shows nothing for seconds reads as a click that
    // didn't take.
    let mut made: Option<crate::git::Created> = None;
    let mut setup: Option<WorktreeSetup> = None;
    if body.worktree {
        if body.kind != SessionKind::Agent {
            return bad_request("worktree requires kind \"agent\"".to_string());
        }
        if body.resume.is_some() || body.cwd.is_some() {
            return bad_request("worktree cannot be combined with resume or cwd".to_string());
        }
        let name = body.agent.as_deref().unwrap_or("claude");
        let Some(kind) = crate::agents::AgentKind::parse(name) else {
            return bad_request(format!("unknown agent {name:?}"));
        };
        // Refuse before the worktree exists: a missing agent must not leave
        // an empty worktree and branch behind.
        if let Err(msg) = crate::launcher::detect(&state, kind, false).await.path {
            return (StatusCode::CONFLICT, Json(json!({"error": msg}))).into_response();
        }
        let plan = match crate::git::plan_session_worktree(&state, &workspace, kind.as_str()).await
        {
            Ok(plan) => plan,
            Err((status, msg)) => return (status, Json(json!({"error": msg}))).into_response(),
        };
        start_cwd = Some(plan.path.clone());
        if body.ui.as_deref() == Some("chat") {
            setup = Some(WorktreeSetup::hold(&state, plan));
        } else {
            match plan.check_out(&state).await {
                Ok(created) => {
                    tracing::info!(workspace = %workspace.id, branch = %created.branch,
                        path = %created.path.display(), "new agent session worktree");
                    made = Some(created);
                }
                Err((status, msg)) => return (status, Json(json!({"error": msg}))).into_response(),
            }
        }
    }

    // Everything past this point can fail; a worktree made for this spawn
    // must not outlive a spawn that never happened (it is new, clean and
    // empty, so the fenced removal takes it, branch included). One still to
    // be checked out simply never is.
    let preset_id = setup.as_ref().map(|s| s.id.clone());
    let response = spawn_requested(
        &state,
        body,
        workspace.clone(),
        theme_owned,
        start_cwd,
        preset_id,
    )
    .await;
    if !response.status().is_success() {
        if let Some(created) = made {
            crate::git::discard_session_worktree(&state, &workspace, &created).await;
        }
        if let Some(setup) = setup {
            crate::lock(&state.worktree_setups).remove(&setup.id);
        }
        return response;
    }
    if let Some(setup) = setup {
        tokio::spawn(setup.finish(Arc::clone(&state), workspace));
    }
    response
}

/// A chat's own worktree, planned but not yet checked out: the chat's id,
/// minted ahead so its driver finds the gate at spawn, and the sender that
/// opens it.
struct WorktreeSetup {
    id: String,
    plan: crate::git::PlannedWorktree,
    ready: tokio::sync::watch::Sender<Option<Result<(), String>>>,
}

impl WorktreeSetup {
    fn hold(state: &AppState, plan: crate::git::PlannedWorktree) -> Self {
        let id = crate::agents::fresh_session_id();
        let (ready, gate) = tokio::sync::watch::channel(None);
        crate::lock(&state.worktree_setups).insert(
            id.clone(),
            crate::state::WorktreeSetupGate {
                gate: chimaera_agent::driver::StartGate {
                    waiting: "Setting up worktree…".to_string(),
                    ready: gate,
                },
                mirrors: plan.source().to_path_buf(),
            },
        );
        Self { id, plan, ready }
    }

    /// The slow half, after the chat has opened: check the worktree out,
    /// then let the chat's process start there, or end its startup with
    /// git's words. A chat closed while it waited never used the worktree
    /// (nothing holds the gate any more), so the worktree goes again.
    async fn finish(self, state: Arc<AppState>, workspace: crate::workspaces::Workspace) {
        let Self { id, plan, ready } = self;
        let checked_out = plan.check_out(&state).await;
        crate::lock(&state.worktree_setups).remove(&id);
        match checked_out {
            Ok(created) => {
                tracing::info!(workspace = %workspace.id, branch = %created.branch,
                    path = %created.path.display(), "new agent session worktree");
                if ready.send(Some(Ok(()))).is_err() {
                    crate::git::discard_session_worktree(&state, &workspace, &created).await;
                    return;
                }
                // The tracker may already have looked while the folder was
                // no repository yet.
                state.git.sessions.recheck(&id);
                state.changes.notify_waiters();
            }
            Err((_, msg)) => {
                tracing::warn!(workspace = %workspace.id, %msg,
                    "a chat's worktree could not be checked out");
                let _ = ready.send(Some(Err(format!(
                    "Couldn't set up this chat's worktree: {msg}"
                ))));
            }
        }
    }
}

/// The rest of `create_session` once the workspace, theme, prelude and
/// starting folder are settled: resume placement, then the chat or TUI
/// spawn. Split out so the caller can undo a worktree it made for a spawn
/// that failed here, on whichever path.
async fn spawn_requested(
    state: &Arc<AppState>,
    body: CreateSession,
    workspace: crate::workspaces::Workspace,
    theme_owned: String,
    start_cwd: Option<std::path::PathBuf>,
    preset_id: Option<String>,
) -> Response {
    let bad_request =
        |msg: String| (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response();
    let theme: &str = &theme_owned;

    // Reopening a conversation that ran in a worktree (its Recents row
    // remembers where) resumes it there while that folder still exists and
    // still belongs to the workspace; otherwise at the root.
    let resume_cwd = match body.resume.as_deref().filter(|_| start_cwd.is_none()) {
        Some(id) => {
            let remembered = crate::lock(&state.recents).cwd_of(&workspace.id, id);
            match remembered {
                Some(dir) => {
                    let raw = dir.to_string_lossy().into_owned();
                    crate::git::allowed_session_cwd(state, &workspace, &raw)
                        .await
                        .ok()
                        .filter(|p| *p != workspace.root)
                }
                None => None,
            }
        }
        None => None,
    };

    // Resuming an archived conversation brings it back: it is live again,
    // and once it ends it belongs in Recents like any other.
    if let Some(resume) = body.resume.as_deref().filter(|r| !r.is_empty()) {
        crate::recents_archive::forget_resumed(state, &workspace.id, resume).await;
    }

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
        return spawn_chat_ui(
            state, body, workspace, theme, start_cwd, resume_cwd, preset_id,
        )
        .await;
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
            if body.resume.is_some()
                && !matches!(
                    agent_kind,
                    crate::agents::AgentKind::Claude | crate::agents::AgentKind::Codex
                )
            {
                return bad_request(format!(
                    "resume is not supported for {} sessions",
                    agent_kind.as_str()
                ));
            }
            // Argv cannot shell-inject, but flag-shaped or control-byte values
            // have no business in --model/--resume.
            for (field, value, valid) in [
                (
                    "model",
                    &body.model,
                    crate::launcher::safe_model_arg as fn(&str) -> bool,
                ),
                ("resume", &body.resume, crate::launcher::safe_arg),
            ] {
                if let Some(value) = value {
                    if !valid(value) {
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
        cwd: start_cwd.or(resume_cwd),
        cols: body.cols,
        rows: body.rows,
        theme: theme.to_string(),
        title_hint: body
            .title_hint
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(crate::agents::truncate_prompt),
        prelude: body.prelude.filter(|p| !p.trim().is_empty()),
        kind,
        started_by: crate::history::StartedBy::You,
    };
    match crate::spawn::spawn_session(state, spec).await {
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
    start_cwd: Option<std::path::PathBuf>,
    resume_cwd: Option<std::path::PathBuf>,
    preset_id: Option<String>,
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
    for (field, value, valid) in [
        (
            "model",
            &body.model,
            crate::launcher::safe_model_arg as fn(&str) -> bool,
        ),
        ("resume", &body.resume, crate::launcher::safe_arg),
    ] {
        if let Some(value) = value {
            if !valid(value) {
                return bad_request(format!("invalid {field} {value:?}"));
            }
        }
    }

    // No resume: the shared fresh-spawn plumbing (also the mastermind PUT's
    // and the Mastermind spawn_agent tool's path). Everything below this
    // point is the resume story: journal seeding + the TUI fallback.
    if body.resume.is_none() {
        return match crate::chat::spawn_fresh_chat_at(
            state,
            workspace,
            crate::chat::FreshChat {
                id: preset_id,
                kind: agent_kind,
                model: body.model,
                name: body.name,
                title_hint: body.title_hint,
                theme: theme.to_string(),
                prelude: body.prelude.filter(|p| !p.trim().is_empty()),
                mastermind: None,
                fork: None,
                started_by: crate::history::StartedBy::You,
            },
            start_cwd,
        )
        .await
        {
            Ok(row) => Json(row).into_response(),
            Err(crate::chat::ChatSpawnFailure::AgentUnavailable(msg)) => {
                (StatusCode::CONFLICT, Json(json!({"error": msg}))).into_response()
            }
            Err(crate::chat::ChatSpawnFailure::Internal(err)) => internal(err),
        };
    }

    // A resumed chat reopens where its conversation lives (claude files
    // transcripts by folder); a different starting folder would lose it.
    if start_cwd.is_some() {
        return bad_request("cwd cannot be combined with resume for a chat".to_string());
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
    // theme themselves. A resumed recent is never a Mastermind (masterminds
    // never retire into recents), so no mastermind gating here.
    let settings = if agent_kind == crate::agents::AgentKind::Claude {
        let (theme_set, user_statusline) =
            crate::runtimes::claude_settings_gates(&state.claude_settings_path, &workspace.root)
                .await;
        let settings_theme = (!theme_set).then_some(theme);
        let plugin_tools = crate::plugins::spawn_allow(state, &workspace.id).await;
        match crate::agents::write_settings(
            &id,
            &key,
            state.port,
            settings_theme,
            user_statusline.as_ref(),
            None,
            &plugin_tools,
            crate::lock(&state.settings).claude_auto_compact(),
        ) {
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
        // Where the conversation last ran (a worktree that still exists),
        // else the root — claude resumes by id from either.
        workspace_root: resume_cwd.unwrap_or_else(|| workspace.root.clone()),
        workspace_id: workspace.id.clone(),
        kind: agent_kind,
        bin,
        version: agent_version,
        settings,
        mcp_config,
        model: body.model.clone(),
        resume: body.resume.clone(),
        fork_at: None,
        rollback_turns: None,
        revert_before_turn: None,
        remote_control: crate::chat::RemoteControlAtStart::Setting,
        carry_ultracode: false,
        carried: Default::default(),
        theme: theme.to_string(),
        prelude: body.prelude.clone().filter(|p| !p.trim().is_empty()),
        // A resumed recent is never the Mastermind: retiring one clears the
        // binding and skips Recents, so there is no binding to resolve here.
        mastermind: None,
        portable_context: None,
        // A fresh session id resuming an old conversation — "now" is correct.
        created_at_ms: None,
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
        tracing::info!(%id, "resumed claude chat history could not be loaded");
        crate::lock(&state.agents).remove(&id);
        crate::lock(&state.session_workspaces).remove(&id);
        // Discard the chat-only scaffolding (the temp settings/mcp for the id we drop).
        for path in [recipe.settings.as_deref(), recipe.mcp_config.as_deref()]
            .into_iter()
            .flatten()
        {
            let _ = std::fs::remove_file(path);
        }
        return (StatusCode::CONFLICT, Json(json!({"error":
            "This conversation's chat history could not be loaded. Its terminal conversation is still available; reopen it in terminal."
        }))).into_response();
    }

    match crate::chat::spawn_chat_session(state, id.clone(), recipe, None).await {
        Ok(info) => {
            crate::agents::spawn_agent_watch(
                state.clone(),
                id.clone(),
                crate::history::StartedBy::You,
            );
            state.changes.notify_waiters();
            Json(crate::chat::chat_session_json(
                &info,
                Some(workspace.id),
                Some(&record),
                false,
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
            crate::recents::retire_with_resume(
                &state,
                &id,
                None,
                None,
                chimaera_agent::model::SessionUi::Chat,
                info.native_session_id,
            );
        }
        state.changes.notify_waiters();
        return StatusCode::NO_CONTENT.into_response();
    }
    match state.sessions.kill(&id) {
        Ok(()) => {
            // Shells never pass through `recents::retire` (that hook is
            // agent-only), so their uploads (and prelude file) are pruned
            // here.
            crate::upload::prune_session_uploads(&state, &id);
            crate::environment::remove_prelude_file(&id);
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
