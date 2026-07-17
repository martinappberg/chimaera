use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::workspaces::Workspace;
use crate::AppState;

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
    // `canonicalize` and `is_dir` are blocking fs syscalls on a user-supplied
    // path — a slow or dead NFS mount would otherwise stall the async reactor.
    // Validate off the reactor; the error strings are unchanged.
    let input = body.root.clone();
    let validated = tokio::task::spawn_blocking(move || {
        let root = std::fs::canonicalize(PathBuf::from(&input))
            .map_err(|err| format!("{input}: {err}"))?;
        if !root.is_dir() {
            return Err(format!("{} is not a directory", root.display()));
        }
        Ok::<PathBuf, String>(root)
    })
    .await;
    let root = match validated {
        Ok(Ok(root)) => root,
        Ok(Err(msg)) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response();
        }
        Err(join_err) => {
            tracing::error!(%join_err, "workspace validation task failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "internal error"})),
            )
                .into_response();
        }
    };
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

#[derive(Deserialize)]
pub(crate) struct PutMastermind {
    agent: String,
    /// "ask" | "auto" — taken as a string (not the enum) so a bad value gets
    /// a clean 400 error body instead of a serde rejection.
    mode: String,
    #[serde(default)]
    model: Option<String>,
    /// The client's current color scheme, like `POST /sessions`; dark when
    /// omitted.
    #[serde(default)]
    theme: Option<String>,
}

/// PUT /api/v1/workspaces/{id}/mastermind — appoint the workspace's
/// Mastermind: creates the privileged chat session AND binds it in one step
/// (bind-before-spawn, so the generated gating — claude's settings file,
/// codex's argv — carries the mode before the process exists), retiring any
/// previous Mastermind first. Mode changes are a re-PUT — neither agent
/// re-reads its gating after spawn, so there is no in-place mode mutation.
/// Returns the new session row (`mastermind: true`).
pub(crate) async fn put_mastermind(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<PutMastermind>,
) -> Response {
    let err = |code: StatusCode, msg: String| (code, Json(json!({"error": msg}))).into_response();
    let Some(_guard) = MastermindSwitchGuard::acquire(&state, &id) else {
        return err(
            StatusCode::CONFLICT,
            "a Mastermind change is already in flight for this workspace — try again".to_string(),
        );
    };
    let Some(workspace) = crate::lock(&state.workspaces).get(&id) else {
        return err(StatusCode::NOT_FOUND, format!("unknown workspace {id}"));
    };
    let mode = match body.mode.as_str() {
        "ask" => crate::workspaces::MastermindMode::Ask,
        "auto" => crate::workspaces::MastermindMode::Auto,
        other => {
            return err(
                StatusCode::BAD_REQUEST,
                format!("invalid mode {other:?} (expected ask or auto)"),
            );
        }
    };
    let kind = match crate::agents::AgentKind::parse(&body.agent) {
        Some(kind) => kind,
        None => {
            return err(
                StatusCode::BAD_REQUEST,
                format!("unknown agent {:?}", body.agent),
            );
        }
    };
    // Claude and codex both enforce the mode through their own harness:
    // claude via the generated settings' `permissions.allow`, codex via the
    // driver answering its per-tool MCP elicitations from the recorded mode
    // (`SpawnSpec.mcp_auto_approve` — codex's approval-mode config is
    // parsed but ignored on the app-server surface; the original v1 refusal
    // predated that finding, PROTOCOL.md Pass 19). Other agents have no
    // chat driver and are refused with an honest message.
    if !matches!(
        kind,
        crate::agents::AgentKind::Claude | crate::agents::AgentKind::Codex
    ) {
        return err(
            StatusCode::BAD_REQUEST,
            format!(
                "the Mastermind needs a structured chat driver; {} has none",
                body.agent
            ),
        );
    }
    if let Some(model) = &body.model {
        if !crate::launcher::safe_arg(model) {
            return err(StatusCode::BAD_REQUEST, format!("invalid model {model:?}"));
        }
    }
    let theme = match body.theme.as_deref() {
        None => "dark".to_string(),
        Some(t @ ("light" | "dark")) => t.to_string(),
        Some(other) => {
            return err(
                StatusCode::BAD_REQUEST,
                format!("invalid theme {other:?} (expected light or dark)"),
            );
        }
    };

    // Resolve the one respawn precondition BEFORE retiring anything (the
    // crate's kill-then-respawn rule): a re-PUT whose new agent binary is
    // missing must 409 with the old Mastermind — its session, binding, and
    // conversation — fully intact, not destroy it and roll back to none.
    // The positive result is cached, so spawn_fresh_chat's own detect below
    // is nearly free.
    let detection = crate::launcher::detect(&state, kind, false).await;
    if let Err(msg) = detection.path {
        return err(StatusCode::CONFLICT, msg);
    }

    // Exactly one Mastermind per workspace: retire the old binding AND its
    // session before minting the new one (it is mastermind-only, never a
    // roster session).
    if let Some(old) = workspace.mastermind.clone() {
        crate::lock(&state.workspaces).set_mastermind(&id, None);
        retire_mastermind_session(&state, &old.session_id).await;
    }

    // Bind BEFORE spawning: the settings-ordering trap — the permission
    // pre-allows must exist in the generated settings (and the roster flag
    // must resolve true) before the process spawns. A spawn failure rolls
    // the binding back below.
    let session_id = crate::agents::fresh_session_id();
    if crate::lock(&state.workspaces)
        .set_mastermind(
            &id,
            Some(crate::workspaces::MastermindCfg {
                session_id: session_id.clone(),
                mode,
                agent: kind.as_str().to_string(),
            }),
        )
        .is_none()
    {
        return err(StatusCode::NOT_FOUND, format!("unknown workspace {id}"));
    }
    match crate::chat::spawn_fresh_chat(
        &state,
        workspace,
        crate::chat::FreshChat {
            id: Some(session_id),
            kind,
            model: body.model,
            name: None,
            title_hint: None,
            theme,
            prelude: None,
            mastermind: Some(mode),
        },
    )
    .await
    {
        Ok(row) => Json(row).into_response(),
        Err(failure) => {
            crate::lock(&state.workspaces).set_mastermind(&id, None);
            state.changes.notify_waiters();
            match failure {
                crate::chat::ChatSpawnFailure::AgentUnavailable(msg) => {
                    err(StatusCode::CONFLICT, msg)
                }
                crate::chat::ChatSpawnFailure::Internal(e) => {
                    tracing::error!(%e, "failed to spawn mastermind session");
                    err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                }
            }
        }
    }
}

/// DELETE /api/v1/workspaces/{id}/mastermind — unbind the workspace's
/// Mastermind and retire its session. 404 when none is configured.
pub(crate) async fn delete_mastermind(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(_guard) = MastermindSwitchGuard::acquire(&state, &id) else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "a Mastermind change is already in flight for this workspace — try again"})),
        )
            .into_response();
    };
    let Some(workspace) = crate::lock(&state.workspaces).get(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {id}")})),
        )
            .into_response();
    };
    let Some(cfg) = workspace.mastermind else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no mastermind configured for this workspace"})),
        )
            .into_response();
    };
    crate::lock(&state.workspaces).set_mastermind(&id, None);
    retire_mastermind_session(&state, &cfg.session_id).await;
    StatusCode::NO_CONTENT.into_response()
}

/// One Mastermind change per workspace at a time (the `chat_switching`
/// idiom): the PUT/DELETE flows are multi-step with rollback, and two racing
/// callers would leak the loser's spawned session — worse, the loser's
/// rollback could clobber the winner's fresh binding. RAII so every early
/// return releases.
struct MastermindSwitchGuard {
    state: Arc<AppState>,
    workspace_id: String,
}

impl MastermindSwitchGuard {
    fn acquire(state: &Arc<AppState>, workspace_id: &str) -> Option<Self> {
        if !crate::lock(&state.mastermind_switching).insert(workspace_id.to_string()) {
            return None;
        }
        Some(Self {
            state: state.clone(),
            workspace_id: workspace_id.to_string(),
        })
    }
}

impl Drop for MastermindSwitchGuard {
    fn drop(&mut self) {
        crate::lock(&self.state.mastermind_switching).remove(&self.workspace_id);
    }
}

/// Tear down a Mastermind session deterministically: identity first (so the
/// driver's async exit path finds no record and cannot retire it into
/// Recents — the Mastermind is never a roster conversation), then the
/// process in whichever registry holds it.
async fn retire_mastermind_session(state: &Arc<AppState>, session_id: &str) {
    crate::lock(&state.agents).remove(session_id);
    crate::lock(&state.session_workspaces).remove(session_id);
    crate::lock(&state.chat_recipes).remove(session_id);
    if let Some(info) = state.chat.get(session_id) {
        if info.alive {
            state.chat.kill(session_id);
        } else {
            state.chat.remove(session_id);
        }
    }
    // A degraded/toggled Mastermind can live as a PTY under the same id.
    let _ = state.sessions.kill(session_id);
    // Session-lifetime state goes with the identity (the same cleanup
    // recents::retire would have done).
    crate::upload::prune_session_uploads(state, session_id);
    crate::environment::remove_prelude_file(session_id);
    state.changes.notify_waiters();
}

/// DELETE /api/v1/workspaces/{id} — unregister a workspace (files untouched).
/// Takes the Mastermind guard: removing the record deletes the binding, so a
/// bound Mastermind must be retired WITH it — otherwise the privileged,
/// role-prompted session keeps running and billing while its flag flips to
/// null and no surface tracks it — and an unguarded delete could interleave
/// a PUT's bind→spawn window, minting a Mastermind orphaned from birth.
pub(crate) async fn delete_workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(_guard) = MastermindSwitchGuard::acquire(&state, &id) else {
        return (
            StatusCode::CONFLICT,
            Json(
                json!({"error": "a Mastermind change is in flight for this workspace — try again"}),
            ),
        )
            .into_response();
    };
    let mastermind = crate::lock(&state.workspaces)
        .get(&id)
        .and_then(|w| w.mastermind);
    // Bind before matching: a lock guard in the scrutinee would live across
    // the retire await below and un-Send the handler.
    let removed = crate::lock(&state.workspaces).remove(&id);
    match removed {
        Ok(true) => {
            if let Some(cfg) = mastermind {
                retire_mastermind_session(&state, &cfg.session_id).await;
            }
            // Its environment prelude goes with it. Explicit-delete only —
            // no boot sweep (see `environment::EnvPreludes`).
            crate::lock(&state.env_preludes).remove_workspace(&id);
            StatusCode::NO_CONTENT.into_response()
        }
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
