//! Server glue for structured chat sessions (Tier B).
//!
//! `chimaera-agent` owns the drivers, journal, and registry; this module
//! wires them into the daemon: spawn recipes for the degrade-to-PTY path,
//! AgentState derivation from protocol events, retire-on-exit, the synthetic
//! session rows, and the view-switch endpoint.
//!
//! ChatManager hooks run on the pump task and must stay cheap, so they only
//! push [`ChatSignal`]s onto a channel; [`spawn_signal_task`] consumes them
//! with full async access to `AppState` (degrading a session respawns a PTY,
//! which no sync hook could do).

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use chimaera_agent::driver::DriverExit;
use chimaera_agent::journal::SeqEvent;
use chimaera_agent::model::AgentEvent;
use chimaera_agent::{ChatInfo, ChatManager};

use crate::agents::{AgentKind, AgentState};
use crate::AppState;

/// What the ChatManager hooks emit; consumed by the signal task.
pub(crate) enum ChatSignal {
    Event(String, Arc<SeqEvent>),
    Exit(String, DriverExit),
}

/// Everything needed to respawn a chat session as a PTY TUI (the
/// handshake-failure degrade path and, later, the view toggle).
#[derive(Clone)]
pub(crate) struct ChatRecipe {
    pub(crate) workspace_root: PathBuf,
    pub(crate) kind: AgentKind,
    pub(crate) bin: PathBuf,
    pub(crate) settings: Option<PathBuf>,
    pub(crate) mcp_config: Option<PathBuf>,
    pub(crate) model: Option<String>,
    pub(crate) resume: Option<String>,
    /// Rewind fork point: respawn with `--fork-session --resume-session-at
    /// <uuid>` so the conversation truncates at that message (claude only).
    pub(crate) fork_at: Option<String>,
    pub(crate) theme: String,
}

/// Build the manager with hooks that forward into the signal channel.
/// Returns the receiver for [`spawn_signal_task`] to consume.
pub(crate) fn new_manager(
    journal_dir: PathBuf,
) -> (
    Arc<ChatManager>,
    tokio::sync::mpsc::UnboundedReceiver<ChatSignal>,
) {
    if let Err(err) = chimaera_agent::journal::prune_dir(
        &journal_dir,
        chimaera_agent::journal::DIR_MAX_BYTES,
        chimaera_agent::journal::DIR_MAX_FILES,
    ) {
        tracing::warn!(%err, "chat journal prune failed");
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let event_tx = tx.clone();
    let manager = Arc::new(ChatManager::new(
        journal_dir,
        Box::new(move |id, entry| {
            let _ = event_tx.send(ChatSignal::Event(id.to_string(), Arc::clone(entry)));
        }),
        Box::new(move |id, exit| {
            // DriverExit isn't Clone; re-materialize the parts the server
            // acts on.
            let owned = match exit {
                DriverExit::Clean(status) => DriverExit::Clean(*status),
                DriverExit::HandshakeFailed {
                    reason,
                    stderr_tail,
                } => DriverExit::HandshakeFailed {
                    reason: reason.clone(),
                    stderr_tail: stderr_tail.clone(),
                },
                DriverExit::ProtocolError(msg) => DriverExit::ProtocolError(msg.clone()),
                DriverExit::Killed => DriverExit::Killed,
            };
            let _ = tx.send(ChatSignal::Exit(id.to_string(), owned));
        }),
    ));
    (manager, rx)
}

/// Consume chat signals for the daemon's lifetime. Called once from `app()`;
/// the receiver is stashed in AppState so construction stays sync.
pub(crate) fn spawn_signal_task(state: Arc<AppState>) {
    let Some(mut rx) = crate::lock(&state.chat_signals).take() else {
        return; // already running (app() called twice only in tests)
    };
    tokio::spawn(async move {
        while let Some(signal) = rx.recv().await {
            match signal {
                ChatSignal::Event(id, entry) => {
                    apply_chat_event(&state, &id, &entry.ev);
                    // @term: grants ride the protocol input in chat mode —
                    // the UserPromptSubmit hook (the TUI's autolink path)
                    // does not fire under -p stream-json. Runs outside
                    // apply_chat_event: autolink takes the agents lock.
                    if let AgentEvent::UserMessage { text, .. } = &entry.ev {
                        let notes = crate::mcp::autolink_mentions(&state, &id, text);
                        for note in notes {
                            tracing::info!(%id, %note, "chat @term mention linked");
                        }
                    }
                    state.changes.notify_waiters();
                }
                ChatSignal::Exit(id, exit) => {
                    handle_chat_exit(&state, &id, exit).await;
                    state.changes.notify_waiters();
                }
            }
        }
    });
}

/// Fold a protocol event into the AgentRecord state machine.
///
/// In chat mode the protocol is authoritative for the FULL lifecycle, for
/// every agent kind: hooks are unreliable under `-p` stream-json (live-found:
/// UserPromptSubmit never fires, and Stop misses too — a session sat
/// "running" half an hour after its turn ended). Hook events that do arrive
/// agree with these transitions, so order doesn't matter.
fn apply_chat_event(state: &Arc<AppState>, id: &str, ev: &AgentEvent) {
    let mut agents = crate::lock(&state.agents);
    let Some(record) = agents.get_mut(id) else {
        return;
    };
    let next = match ev {
        AgentEvent::PermissionRequest { .. } => Some(AgentState::NeedsPermission),
        AgentEvent::PermissionResolved { .. } => Some(AgentState::Running),
        AgentEvent::Error { fatal: true, .. } => Some(AgentState::Errored),
        AgentEvent::TurnStarted { .. } => Some(AgentState::Running),
        AgentEvent::TurnCompleted { .. } => Some(AgentState::Finished),
        AgentEvent::TurnAborted { .. } => Some(AgentState::Errored),
        // Telemetry says the account limit is actually blocking requests —
        // the same rail state the TUI hooks derive from StopFailure.
        AgentEvent::RateLimit {
            limit_reached: true,
            ..
        } => Some(AgentState::RateLimited),
        _ => None,
    };
    if let Some(next) = next {
        record.state = next;
    }
    // First prompt from the protocol input: the UserPromptSubmit hook does
    // not fire under -p stream-json, so this is the chat path for every
    // agent (a hook duplicate would be a no-op — first write wins).
    if record.first_prompt.is_none() {
        if let AgentEvent::UserMessage { text, .. } = ev {
            let text = text.trim();
            if !text.is_empty() {
                record.first_prompt = Some(text.to_string());
            }
        }
    }
    match ev {
        // The agent named the conversation (claude generate_session_title,
        // codex thread/name/updated): same slot the TUI's transcript
        // ai-title records land in, so display_name resolution — custom >
        // OSC > ai — is identical across surfaces.
        AgentEvent::SessionTitle { title } => {
            let title = title.trim();
            if !title.is_empty() {
                record.ai_title = Some(title.to_string());
            }
        }
        // File tracking without hooks: codex chat sessions have no
        // PostToolUse channel, so Edit-kind tool locations feed
        // files_touched here (claude's hook writes dedup via touch_file).
        AgentEvent::ToolCall {
            kind: chimaera_agent::model::ToolKind::Edit,
            locations,
            ..
        } => {
            for path in locations {
                record.touch_file(path);
            }
        }
        _ => {}
    }
}

/// Driver ended: degrade a failed handshake into a PTY TUI on the same
/// session id (one attempt), otherwise retire the session like the PTY
/// watcher would.
async fn handle_chat_exit(state: &Arc<AppState>, id: &str, exit: DriverExit) {
    // A view switch kills the driver on purpose: free the registry slot for
    // the respawn and keep the AgentRecord/workspace mapping intact.
    if crate::lock(&state.chat_switching).contains_key(id) {
        state.chat.remove(id);
        return;
    }
    let recipe = crate::lock(&state.chat_recipes).remove(id);
    match exit {
        DriverExit::HandshakeFailed {
            reason,
            stderr_tail,
        } => {
            tracing::warn!(%id, %reason, %stderr_tail, "chat handshake failed; degrading to terminal");
            state.chat.remove(id);
            match recipe {
                Some(recipe) => {
                    // Carry any pinned name onto the fallback PTY (usually none
                    // this early, but resurrection could have set one).
                    let pinned = crate::lock(&state.agents)
                        .get(id)
                        .and_then(|r| r.custom_title.clone());
                    degrade_to_pty(state, id, recipe, pinned).await
                }
                None => crate::recents::retire(state, id, None, None),
            }
        }
        DriverExit::ProtocolError(reason) => {
            // Mid-session breakage: no auto-restart (loop risk). The session
            // stays in the registry, dead, so the chat surface can show the
            // failure and offer resume choices.
            tracing::warn!(%id, %reason, "chat driver protocol error");
            if let Some(record) = crate::lock(&state.agents).get_mut(id) {
                record.state = AgentState::Errored;
            }
        }
        DriverExit::Clean(_) | DriverExit::Killed => {
            state.chat.remove(id);
            crate::recents::retire(state, id, None, None);
        }
    }
}

/// Respawn the session as a Tier A PTY TUI with the same identity: same
/// session id, same AgentRecord (hooks/links/titles keep working), same
/// settings/mcp files, original resume target.
async fn degrade_to_pty(
    state: &Arc<AppState>,
    id: &str,
    recipe: ChatRecipe,
    pinned_name: Option<String>,
) {
    let mut argv = if recipe.kind == AgentKind::Codex {
        // The codex TUI resumes via a subcommand (`codex resume <uuid>`),
        // not a flag — build_agent_command's flag surface is claude-shaped.
        let mut argv = vec![recipe.bin.to_string_lossy().into_owned()];
        if let Some(resume) = &recipe.resume {
            argv.push("resume".to_string());
            argv.push(resume.clone());
        }
        argv
    } else {
        crate::launcher::build_agent_command(
            recipe.kind,
            &recipe.bin,
            recipe.settings.as_deref(),
            recipe.model.as_deref(),
            recipe.resume.as_deref(),
            None,
        )
    };
    if let Some(mcp) = &recipe.mcp_config {
        argv.push("--mcp-config".to_string());
        argv.push(mcp.to_string_lossy().into_owned());
    }
    let opts = chimaera_pty::SpawnOpts {
        cwd: recipe.workspace_root,
        // Carry the user's pinned name across the toggle so the PTY row keeps
        // it (and reports `renamed` truthfully); None leaves it deriving.
        name: pinned_name,
        // The UI adopts real dims on first attach; these only cover the gap.
        cols: 80,
        rows: 24,
        command: Some(crate::launcher::wrap_login_shell(
            &crate::launcher::login_shell(),
            argv,
        )),
        id: Some(id.to_string()),
        env: crate::api::session_env(state, id, &recipe.theme),
        env_remove: crate::api::launcher_context_env(),
        scrollback: crate::lock(&state.settings).scrollback_lines(),
    };
    match state.sessions.spawn(opts) {
        Ok(_) => {
            tracing::info!(%id, "chat session degraded to PTY TUI");
        }
        Err(err) => {
            tracing::error!(%id, %err, "degrade respawn failed");
            crate::recents::retire(state, id, None, None);
        }
    }
}

/// Alive in EITHER registry — the PTY watcher's liveness check must not
/// retire a session that lives as a chat driver (and vice versa during a
/// view switch). Chat-registry *presence* counts even when the driver died:
/// a ProtocolError session is deliberately kept visible so the chat surface
/// can show the failure; closing it is the user's call (DELETE).
pub(crate) fn session_alive(state: &AppState, id: &str) -> bool {
    state.sessions.get(id).is_some()
        || state.chat.contains(id)
        || crate::lock(&state.chat_switching).contains_key(id)
}

/// Synthetic session row for a chat session — same shape as the PTY rows in
/// `sessions_json` (the client's Session type is one interface), with
/// `ui:"chat"` marking the surface.
pub(crate) fn chat_session_json(
    info: &ChatInfo,
    workspace_id: Option<String>,
    agent: Option<&crate::agents::AgentRecord>,
) -> serde_json::Value {
    let display_name = agent
        .map(|a| a.display_name(None))
        .unwrap_or_else(|| info.agent.clone());
    json!({
        "id": info.id,
        "name": info.agent,
        "cwd": info.cwd,
        "cols": 0,
        "rows": 0,
        "created_at": info.created_at_ms / 1000,
        "alive": info.alive,
        "exit_status": info.exit_status,
        "title": null,
        "pid": null,
        // A user-pinned customTitle marks the row as renamed (the rail shows
        // the pin and stops deriving a name over it).
        "renamed": agent.is_some_and(|a| a.custom_title.is_some()),
        "phase": "unknown",
        "cwd_current": info.cwd,
        "exec_stage": null,
        "workspace_id": workspace_id,
        "kind": "agent",
        "agent_kind": agent.map(|a| a.kind.as_str()),
        "agent_state": agent.map(|a| a.state.as_str()),
        "agent_title": agent.and_then(|a| a.title()),
        "files_touched": agent.map(|a| &a.files_touched),
        "display_name": display_name,
        "ui": "chat",
        "chat_capable": true,
        "chat_model": info.model,
        "chat_mode": info.current_mode,
        "pending_permission": info.pending_permission,
    })
}

/// UUID v4 shaped from the daemon's own entropy source — claude's
/// `--session-id` pins the native session id at spawn, so the resume handle
/// exists before the first turn.
pub(crate) fn fresh_native_uuid() -> String {
    let hex = chimaera_core::generate_token();
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32],
    )
}

#[derive(Deserialize)]
pub(crate) struct SwitchView {
    ui: String,
    #[serde(default)]
    force: bool,
}

/// POST /api/v1/sessions/{id}/view — switch a session between the chat and
/// terminal surfaces by stopping the current process and resuming the same
/// conversation in the other mode (same chimaera session id throughout).
pub(crate) async fn switch_view(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<SwitchView>,
) -> Response {
    let err = |code: StatusCode, msg: String| (code, Json(json!({"error": msg}))).into_response();
    if body.ui != "chat" && body.ui != "term" {
        return err(
            StatusCode::BAD_REQUEST,
            format!("invalid ui {:?} (expected chat or term)", body.ui),
        );
    }
    let chat_info = state.chat.get(&id);
    let currently_chat = chat_info.as_ref().is_some_and(|c| c.alive);
    let pty_alive = state.sessions.get(&id).is_some();
    if !currently_chat && !pty_alive && chat_info.is_none() {
        return err(StatusCode::NOT_FOUND, format!("unknown session {id}"));
    }
    let target_chat = body.ui == "chat";
    if target_chat == currently_chat && (currently_chat || pty_alive) {
        return StatusCode::NO_CONTENT.into_response();
    }

    let Some(record) = crate::lock(&state.agents).get(&id).cloned() else {
        return err(
            StatusCode::BAD_REQUEST,
            "only agent sessions can switch views".to_string(),
        );
    };
    if !matches!(record.kind, AgentKind::Claude | AgentKind::Codex) {
        return err(
            StatusCode::BAD_REQUEST,
            format!("chat view not yet available for {}", record.kind.as_str()),
        );
    }
    // Busy guard: interrupting a mid-task agent is a user decision.
    if record.state == AgentState::Running && !body.force {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "agent is mid-task", "busy": true})),
        )
            .into_response();
    }

    // The resume handle: chat side knows its native id from Init; TUI side
    // from the transcript path hooks report. Missing handle = fresh start in
    // the other mode (still the same chimaera session identity).
    let resume = if currently_chat {
        chat_info.as_ref().and_then(|c| c.native_session_id.clone())
    } else {
        record.resume_id()
    };

    let workspace_id = crate::lock(&state.session_workspaces).get(&id).cloned();
    let Some(workspace_root) = workspace_id.as_ref().and_then(|wid| {
        crate::lock(&state.workspaces)
            .get(wid)
            .map(|w| w.root.clone())
    }) else {
        return err(
            StatusCode::NOT_FOUND,
            "session has no workspace".to_string(),
        );
    };

    // Mark the switch so the exit path neither retires nor degrades it.
    crate::lock(&state.chat_switching).insert(id.clone(), body.ui.clone());
    state.changes.notify_waiters();
    let result = perform_switch(
        &state,
        &id,
        target_chat,
        currently_chat,
        resume,
        workspace_root,
        &record,
    )
    .await;
    crate::lock(&state.chat_switching).remove(&id);

    match result {
        Ok(()) => {
            state.changes.notify_waiters();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(msg) => err(StatusCode::INTERNAL_SERVER_ERROR, msg),
    }
}

async fn perform_switch(
    state: &Arc<AppState>,
    id: &str,
    target_chat: bool,
    currently_chat: bool,
    resume: Option<String>,
    workspace_root: PathBuf,
    record: &crate::agents::AgentRecord,
) -> Result<(), String> {
    // A user-pinned name lives in different stores per surface: chat sessions
    // pin it on the AgentRecord (customTitle), PTY sessions on SessionInfo.name
    // (renamed). Capture it from whichever holds it NOW so the toggle carries
    // it across — otherwise a rename made in one mode vanishes in the other.
    let pinned_name = if currently_chat {
        record.custom_title.clone()
    } else {
        state
            .sessions
            .get(id)
            .filter(|i| i.renamed)
            .map(|i| i.name.clone())
    };
    // Land it on the AgentRecord too, so the customTitle chain is the single
    // authority the chat row and the (post-degrade) PTY row both read.
    if let Some(name) = &pinned_name {
        if let Some(rec) = crate::lock(&state.agents).get_mut(id) {
            rec.custom_title = Some(name.clone());
        }
    }

    // Stop the current process and wait for its slot to free (same-id
    // respawn requires deregistration; SessionManager unregisters on reap).
    if currently_chat {
        state.chat.kill(id);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.chat.contains(id) {
            if tokio::time::Instant::now() >= deadline {
                return Err("chat driver did not stop in time".to_string());
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    } else if state.sessions.get(id).is_some() {
        let _ = state.sessions.kill(id);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.sessions.get(id).is_some() {
            if tokio::time::Instant::now() >= deadline {
                return Err("terminal session did not stop in time".to_string());
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    // Stamp the transition in the journal while no live Journal owns the
    // file: replayers then read "continued in terminal/chat" instead of a
    // bare process exit (and the chat store clears its exited banner).
    if let Err(err) = chimaera_agent::journal::append_marker(
        state.chat.journal_dir(),
        id,
        chimaera_agent::model::AgentEvent::ModeSwitch {
            to: if target_chat {
                chimaera_agent::model::SessionUi::Chat
            } else {
                chimaera_agent::model::SessionUi::Term
            },
        },
    ) {
        tracing::warn!(%id, %err, "failed to journal the view switch");
    }

    // Existing per-session files still on disk carry the same hook key.
    let runtime_dir = chimaera_core::runtime_dir().join("agents");
    let settings = Some(runtime_dir.join(format!("{id}-settings.json"))).filter(|p| p.exists());
    let mcp_config = Some(runtime_dir.join(format!("{id}-mcp.json"))).filter(|p| p.exists());
    let bin = crate::launcher::detect(state, record.kind, false)
        .await
        .path
        .map_err(|e| e.to_string())?;
    let theme = "dark".to_string(); // scheme re-injection needs a client hint; TUI re-themes on attach

    if target_chat {
        let recipe = ChatRecipe {
            workspace_root: workspace_root.clone(),
            kind: record.kind,
            bin: bin.clone(),
            settings: settings.clone(),
            mcp_config: mcp_config.clone(),
            model: None,
            resume: resume.clone(),
            fork_at: None,
            theme: theme.clone(),
        };
        spawn_chat_session(state, id.to_string(), recipe, None).map_err(|e| e.to_string())?;
    } else {
        let recipe = ChatRecipe {
            workspace_root,
            kind: record.kind,
            bin,
            settings,
            mcp_config,
            model: None,
            resume,
            fork_at: None,
            theme,
        };
        degrade_to_pty(state, id, recipe, pinned_name).await;
    }
    Ok(())
}

#[derive(Deserialize)]
pub(crate) struct RewindBody {
    /// The message uuid the forked conversation resumes AT (the message
    /// preceding the rewound-to user message — the client computes it from
    /// its Checkpoint anchors).
    resume_at: String,
}

/// POST /api/v1/sessions/{id}/rewind — fork the conversation at a checkpoint.
///
/// The file-restore half already happened through the chat socket
/// (`Rewind { dry_run:false }` → the CLI's `rewind_files`); this endpoint
/// does the conversation half: stop the driver and respawn the SAME chimaera
/// session as `--resume <native> --fork-session --resume-session-at <uuid>`,
/// which truncates the transcript at that message on a fresh native id.
pub(crate) async fn rewind_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<RewindBody>,
) -> Response {
    let err = |code: StatusCode, msg: String| (code, Json(json!({"error": msg}))).into_response();
    let Some(info) = state.chat.get(&id) else {
        return err(
            StatusCode::NOT_FOUND,
            format!("no chat session {id} (rewind is a chat-surface action)"),
        );
    };
    let Some(record) = crate::lock(&state.agents).get(&id).cloned() else {
        return err(StatusCode::BAD_REQUEST, "not an agent session".to_string());
    };
    if record.kind != AgentKind::Claude {
        return err(
            StatusCode::BAD_REQUEST,
            "checkpoint rewind is a claude feature".to_string(),
        );
    }
    let Some(native) = info.native_session_id.clone() else {
        return err(
            StatusCode::CONFLICT,
            "session has no resume handle yet".to_string(),
        );
    };
    let workspace_id = crate::lock(&state.session_workspaces).get(&id).cloned();
    let Some(workspace_root) = workspace_id.as_ref().and_then(|wid| {
        crate::lock(&state.workspaces)
            .get(wid)
            .map(|w| w.root.clone())
    }) else {
        return err(
            StatusCode::NOT_FOUND,
            "session has no workspace".to_string(),
        );
    };

    // Same choreography as the view switch: flag the deliberate stop so the
    // exit path neither retires nor degrades, stop, respawn.
    crate::lock(&state.chat_switching).insert(id.clone(), "chat".to_string());
    let result = async {
        state.chat.kill(&id);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.chat.contains(&id) {
            if tokio::time::Instant::now() >= deadline {
                return Err("chat driver did not stop in time".to_string());
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        if let Err(e) = chimaera_agent::journal::append_marker(
            state.chat.journal_dir(),
            &id,
            chimaera_agent::model::AgentEvent::Notice {
                text: "rewound to checkpoint".to_string(),
            },
        ) {
            tracing::warn!(%id, %e, "failed to journal the rewind");
        }
        let runtime_dir = chimaera_core::runtime_dir().join("agents");
        let settings = Some(runtime_dir.join(format!("{id}-settings.json"))).filter(|p| p.exists());
        let mcp_config = Some(runtime_dir.join(format!("{id}-mcp.json"))).filter(|p| p.exists());
        let bin = crate::launcher::detect(&state, record.kind, false)
            .await
            .path
            .map_err(|e| e.to_string())?;
        let recipe = ChatRecipe {
            workspace_root,
            kind: record.kind,
            bin,
            settings,
            mcp_config,
            model: None,
            resume: Some(native),
            fork_at: Some(body.resume_at.clone()),
            theme: "dark".to_string(),
        };
        spawn_chat_session(&state, id.clone(), recipe, None).map_err(|e| e.to_string())?;
        Ok(())
    }
    .await;
    crate::lock(&state.chat_switching).remove(&id);
    state.changes.notify_waiters();
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(msg) => err(StatusCode::INTERNAL_SERVER_ERROR, msg),
    }
}

/// Spawn (or respawn) a chat driver for `id` from a recipe. Shared by
/// create_session and the view switch. `pinned_override` lets create pass a
/// fresh --session-id uuid; resumes leave it None (claude forks a new id).
pub(crate) fn spawn_chat_session(
    state: &Arc<AppState>,
    id: String,
    recipe: ChatRecipe,
    pinned_override: Option<String>,
) -> anyhow::Result<ChatInfo> {
    let (argv, pinned): (Vec<String>, Option<String>) = match recipe.kind {
        AgentKind::Claude => {
            let (settings, mcp) = (
                recipe
                    .settings
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("chat session needs a settings file"))?,
                recipe
                    .mcp_config
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("chat session needs an mcp config"))?,
            );
            let pinned = match (&pinned_override, &recipe.resume) {
                (Some(uuid), _) => Some(uuid.clone()),
                // Resume forks a NEW native id; it arrives via system/init.
                (None, Some(_)) => None,
                (None, None) => Some(fresh_native_uuid()),
            };
            (
                crate::launcher::build_chat_command(
                    &recipe.bin,
                    settings,
                    mcp,
                    recipe.model.as_deref(),
                    recipe.resume.as_deref(),
                    pinned.as_deref(),
                    recipe.fork_at.as_deref(),
                ),
                pinned,
            )
        }
        AgentKind::Codex => {
            // The app-server resumes in-protocol (thread/resume keeps the
            // id), so the resume handle IS the pinned native id.
            (
                vec![
                    recipe.bin.to_string_lossy().into_owned(),
                    "app-server".to_string(),
                ],
                recipe.resume.clone(),
            )
        }
        other => anyhow::bail!("no chat driver for {}", other.as_str()),
    };
    let argv = crate::launcher::wrap_login_shell(&crate::launcher::login_shell(), argv);
    let mut spec =
        chimaera_agent::driver::SpawnSpec::new(id.clone(), argv, recipe.workspace_root.clone());
    spec.env = crate::api::session_env(state, &id, &recipe.theme);
    // Strip the daemon's own launcher context (same set the PTY path removes)
    // so a chimaera launched from inside an agent can't leak that context into
    // the chat agent it spawns.
    spec.env_remove = crate::api::launcher_context_env();
    spec.pinned_native_id = pinned;

    // Resuming a finished conversation mints a NEW chimaera session id, and
    // the agents replay no history over the wire — seed the new journal
    // from the previous life's (the native-id index knows which one).
    // Journal::open continues sequence numbers from the copied file, so
    // attach replays the whole conversation before the fresh Init.
    if let Some(native) = &recipe.resume {
        if let Some(old_id) = state.chat.index().lookup(native) {
            let dir = state.chat.journal_dir();
            let old_path = dir.join(format!("{old_id}.jsonl"));
            let new_path = dir.join(format!("{id}.jsonl"));
            if old_id != id && old_path.exists() && !new_path.exists() {
                if let Err(err) = std::fs::copy(&old_path, &new_path) {
                    tracing::warn!(%id, %old_id, %err, "journal seed copy failed");
                } else {
                    tracing::info!(%id, %old_id, "seeded chat journal from previous life");
                }
            }
        }
    }

    crate::lock(&state.chat_recipes).insert(id.clone(), recipe.clone());
    let info = match recipe.kind {
        AgentKind::Claude => state
            .chat
            .spawn(&chimaera_agent::claude::ClaudeAdapter, spec),
        _ => state.chat.spawn(&chimaera_agent::codex::CodexAdapter, spec),
    };
    if info.is_err() {
        crate::lock(&state.chat_recipes).remove(&id);
    }
    info
}
