use super::{
    authority, detached, engine, execution,
    protocol::{Configure, WorkspaceConfigure},
    transport, Ownership,
};
use crate::{lock, AppState};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::{atomic::Ordering, Arc};

pub(super) fn failure(error: anyhow::Error) -> Response {
    outcome(error).into_response()
}
/// `failure` plus a route-specific stable `error_code` (additive: `code` stays
/// the shared diagnostic category, and clients that ignore it are unaffected).
pub(super) fn failure_with_code(error: anyhow::Error, route_code: &'static str) -> Response {
    outcome_with(error, Some(route_code)).into_response()
}
fn outcome(error: anyhow::Error) -> detached::Outcome {
    outcome_with(error, None)
}
fn outcome_with(error: anyhow::Error, route_code: Option<&'static str>) -> detached::Outcome {
    if let Some(blocked) = error.downcast_ref::<super::provider_gate::Blocked>() {
        let mut body = json!({"error":"cloud_provider_not_ready","blocked_providers":blocked.0});
        if let Some(route_code) = route_code {
            body["error_code"] = route_code.into();
        }
        return detached::Outcome::refused(StatusCode::CONFLICT, Some(body));
    }
    let mut body = json!({
        "error": error.to_string().chars().take(256).collect::<String>(),
        "code": error_code(&error),
    });
    if let Some(route_code) = route_code {
        body["error_code"] = route_code.into();
    }
    detached::Outcome::refused(StatusCode::BAD_REQUEST, Some(body))
}
/// Stable codes a client maps to its own plain words (additive `code`); the
/// English `error` text remains only for older clients and is not a contract.
pub(super) fn error_code(error: &anyhow::Error) -> &'static str {
    let text = error.to_string();
    let has = |needle: &str| text.contains(needle);
    if has("optional_runtime_unavailable") {
        "optional_runtime_unavailable"
    } else if error
        .downcast_ref::<super::provider_gate::Blocked>()
        .is_some()
    {
        "cloud_provider_not_ready"
    } else if engine::upgrade_required(error) {
        // The next pass retries through the newer path on its own, so it reads
        // as the copy still being saved rather than a problem.
        "checkpoint_pending"
    } else if has(transport::RETURN_WINDOW_ENDED) {
        "return_window_ended"
    } else if has("Account changed") || has("account changed") {
        "account_changed"
    } else if has("previous managed processes") || has("previous execution is stopping") {
        "previous_processes_running"
    } else if has("execution authority") || has("ownership has not been verified") {
        "ownership_unverified"
    } else if has("ownership changed") || has("names another owner") || has("owned elsewhere") {
        "ownership_changed"
    } else if has("checkpoint") && (has("not available") || has("required") || has("pending")) {
        "checkpoint_pending"
    } else if has("needs Git 2.36") {
        "git_too_old"
    } else if has("credential path") {
        "credential_in_history"
    } else if has("root_setup_required") {
        "root_setup_required"
    } else if has("Mirror helper cleanup could not be verified") {
        "cache_recovery_needed"
    } else if has("session archive")
        || has("transcript is unavailable")
        || has("rollout is unavailable")
    {
        "conversation_not_saved"
    } else {
        engine::failure_code(error)
    }
}
fn result(result: anyhow::Result<()>) -> detached::Outcome {
    match result {
        Ok(()) => detached::Outcome::done(),
        Err(error) => outcome(error),
    }
}
#[derive(Deserialize)]
pub(crate) struct Hydrate {
    #[serde(default)]
    destination_root: Option<std::path::PathBuf>,
    workspace_id: String,
    expected_epoch: u64,
    #[serde(default)]
    requires_fork: bool,
}
#[derive(Deserialize)]
pub(crate) struct Privacy {
    workspace_id: String,
    never_mirror: bool,
    #[serde(default)]
    confirmed: bool,
}

pub(crate) use execution::recovery::recover as recover_execution;
pub(crate) async fn configure(
    State(state): State<Arc<AppState>>,
    Json(config): Json<Configure>,
) -> Response {
    if config.execution.is_some() {
        return failure(anyhow::anyhow!(
            "execution authority requires negotiated configuration"
        ));
    }
    configure_inner(state, config, None).await
}
pub(crate) async fn configure_workspace(
    State(state): State<Arc<AppState>>,
    Json(request): Json<WorkspaceConfigure>,
) -> Response {
    if request.config.execution.is_some() {
        return failure(anyhow::anyhow!(
            "execution authority requires negotiated configuration"
        ));
    }
    configure_inner(state, request.config, Some(request.workspace_root)).await
}
#[derive(Deserialize)]
pub(crate) struct ExecutionConfigure {
    #[serde(flatten)]
    config: Configure,
    #[serde(default)]
    workspace_root: Option<std::path::PathBuf>,
}
pub(crate) async fn configure_execution(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ExecutionConfigure>,
) -> Response {
    if request.config.execution.is_none() {
        return failure(anyhow::anyhow!("execution configuration required"));
    }
    configure_inner(state, request.config, request.workspace_root).await
}
fn configure_ack(
    config: &Configure,
    workspace: Option<super::protocol::WorkspaceConfigureAck>,
) -> Response {
    if let Some(execution) = &config.execution {
        return Json(json!({"execution_authority":1,"execution":execution,"workspace_configuration":workspace})).into_response();
    }
    match workspace {
        Some(value) => Json(value).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}
async fn configure_inner(
    state: Arc<AppState>,
    mut config: Configure,
    root: Option<std::path::PathBuf>,
) -> Response {
    let _configuration = state.pro.configuration.lock().await;
    if root.is_none()
        && (config.delegation.workspace.is_some() || lock(&state.pro.authority).restricted())
    {
        return failure(anyhow::anyhow!(
            "workspace authority requires scoped configuration"
        ));
    }
    let validation = (|| -> anyhow::Result<()> {
        execution::validate_configuration(&config)?;
        let enrolled = !lock(&state.pro.execution.latched).is_empty();
        anyhow::ensure!(
            config.execution.is_some()
                || (!execution::any_uncertain(&state)
                    && !enrolled
                    && !lock(&state.pro.preferences)
                        .values()
                        .any(|p| p.continuity.is_some())),
            "continuity upgrade required"
        );
        anyhow::ensure!(
            config.account_id.as_deref().is_none_or(super::valid_id),
            "invalid account identity"
        );
        config.endpoint = transport::endpoint(&config.endpoint)?;
        if !config.keeper_url.is_empty() {
            config.keeper_url = transport::endpoint(&config.keeper_url)?;
            anyhow::ensure!(
                !config.endpoint.starts_with("https:") || config.keeper_url.starts_with("https:"),
                "keeper TLS downgrade"
            );
        }
        anyhow::ensure!(
            super::valid_id(&config.delegation.device_id)
                && !config.delegation.access_token.is_empty()
                && config.delegation.access_token.len() <= 8192
                && !config.delegation.access_token.chars().any(char::is_control),
            "invalid daemon delegation"
        );
        anyhow::ensure!(
            config.delegation.scope.iter().any(|s| s == "baton")
                && config.delegation.scope.iter().any(|s| s == "mirror"),
            "daemon delegation needs baton and mirror scopes"
        );
        Ok(())
    })();
    if let Err(error) = validation {
        return failure(error);
    }
    let accepted = if let Some(root) = root {
        match authority::prepare(&state, &config, root).await {
            Ok(value) => Some(value),
            Err(error) => return failure(error),
        }
    } else {
        None
    };
    if let Err(error) = super::ensure_root(&state.pro.root).await {
        return failure(error);
    }
    if execution::supervisor::pending(&state) && config.execution.is_none() {
        return failure(anyhow::anyhow!(
            "supervisor cleanup requires execution configuration"
        ));
    }
    // Configure serializes this startup-only cleanup with all replacement.
    // No execution lease or lost enrollment policy is created here.
    if let Err(error) = execution::supervisor::apply(&state, accepted.as_ref()).await {
        return failure(error);
    }
    // Refreshing the same negotiated identity only replaces the credential;
    // it must not interrupt running work or reset the local lease deadline.
    let same = lock(&state.pro.runtime).as_ref().is_some_and(|old| {
        old.endpoint == config.endpoint
            && old.account_id == config.account_id
            && old.role == config.role
            && old.keeper_url == config.keeper_url
            && old.execution == config.execution
            && old.delegation.device_id == config.delegation.device_id
            && old.delegation.workspace == config.delegation.workspace
            && old.delegation.scope == config.delegation.scope
            && config.execution.is_some()
    });
    if same && state.pro.configured.load(Ordering::Acquire) {
        let response = configure_ack(&config, accepted.as_ref().map(|v| v.ack()));
        // A new delegation or name: the reverse link reconnects with it.
        let relink = lock(&state.pro.runtime).as_ref().is_some_and(|old| {
            old.delegation.access_token != config.delegation.access_token
                || old.alias != config.alias
        });
        *lock(&state.pro.runtime) = Some(config);
        state.pro.delegation_refused.store(false, Ordering::Release);
        if relink {
            super::reach::start(&state);
        }
        return response;
    }
    if let Err(error) = stop_tasks(&state).await {
        return failure(error);
    }
    *lock(&state.pro.runtime) = None;
    state.pro.configured.store(false, Ordering::Release);
    if let Some(value) = &accepted {
        *lock(&state.pro.authority) = authority::Authority::Invalid;
        if let Err(error) = authority::save(&state, value).await {
            return failure(error);
        }
        *lock(&state.pro.authority) = authority::Authority::Bound(value.clone());
    }
    *lock(&state.pro.project_cache) = Default::default();
    let response = configure_ack(&config, accepted.as_ref().map(|v| v.ack()));
    let managed = config.execution.is_some();
    if config.role == super::protocol::Role::Worker
        && !state.pro.worker.swap(true, Ordering::AcqRel)
    {
        if let Err(error) = super::persist(&state).await {
            return failure(error);
        }
        // Its updates are the service's from now on; tell attached windows.
        crate::update::became_managed(&state);
    }
    *lock(&state.pro.runtime) = Some(config);
    state.pro.delegation_refused.store(false, Ordering::Release);
    if state.pro.signed_out.swap(false, Ordering::AcqRel) {
        if let Err(error) = super::persist(&state).await {
            return failure(error);
        }
    }
    state.pro.configured.store(true, Ordering::Release);
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    execution::provider_ready::configured(&state);
    engine::start(state.clone());
    // Every managed host is fenced by lease expiry: a computer whose lease
    // lapsed stops its own agents so the cloud continues exactly once.
    if managed {
        execution::start(&state);
    }
    // A personal Pro computer keeps itself reachable from other devices and
    // hands work over before it sleeps, with or without the app (`reach`,
    // `sleep_watch`).
    super::reach::start(&state);
    if !execution::worker(&state)
        && lock(&state.pro.runtime)
            .as_ref()
            .is_some_and(|config| config.delegation.workspace.is_none())
    {
        super::sleep_watch::start(&state);
    }
    response
}
pub(super) async fn stop_tasks(state: &Arc<AppState>) -> anyhow::Result<()> {
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    execution::provider_ready::replacing(state);
    // Captured before the runtime changes: replacing or removing a device's
    // configuration never stops its local work (laptop first).
    let worker = execution::worker(state);
    super::reach::stop(state);
    let stopping = execution::invalidate(state);
    state.pro.configured.store(false, Ordering::Release);
    state.pro.generation.fetch_add(1, Ordering::AcqRel);
    let lease_task = lock(&state.pro.task).take();
    let mirror_task = lock(&state.pro.mirror_task).take();
    for task in [&lease_task, &mirror_task].into_iter().flatten() {
        task.abort();
    }
    for task in [lease_task, mirror_task].into_iter().flatten() {
        let _ = task.await;
    }
    for (_, task) in lock(&state.pro.reconciling).drain() {
        task.abort();
    }
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    execution::provider_ready::settle(state).await?;
    if worker {
        execution::stop(state, &stopping).await?;
    }
    execution::clear_stopped(state);
    Ok(())
}
pub(crate) async fn disconnect(State(state): State<Arc<AppState>>) -> Response {
    let _configuration = state.pro.configuration.lock().await;
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    execution::provider_ready::retire(&state);
    if let Err(error) = stop_tasks(&state).await {
        return failure(error);
    }
    *lock(&state.pro.project_cache) = Default::default();
    *lock(&state.pro.runtime) = None;
    state.pro.configured.store(false, Ordering::Release);
    // Known remote ownership remains fenced across sign-out and restart. A
    // local or interrupted local transfer becomes ordinary local work: signing
    // out publishes nothing more, but never stops this computer's agents. On a
    // personal computer that includes a return this computer itself started
    // (Hydrating/SettingUp): no account is left to finish it, so it must not
    // stay fenced. Sessions a transfer stopped continue here.
    let device = !execution::worker(&state);
    lock(&state.pro.release_pending).clear();
    lock(&state.pro.opened_here).clear();
    super::moves::forget(&state);
    // No account is left to keep anything in the cloud or to reach this
    // computer through: the reverse link ends with the configuration.
    super::reach::stop(&state);
    lock(&state.pro.parked).clear();
    lock(&state.pro.reclaim).clear();
    lock(&state.pro.holders).clear();
    state.pro.reasons.clear();
    // Recorded before the state write below, so a restart remembers that the
    // computer's interrupted work is its own again (`resume_unverified`).
    state.pro.signed_out.store(true, Ordering::Release);
    let mut dropped = Vec::new();
    let mut returned: Vec<String> = {
        let mut ownership = lock(&state.pro.ownership);
        ownership.retain(|id, owner| {
            let local = matches!(owner, Ownership::Local { .. });
            if local {
                dropped.push(id.clone());
            }
            !local
        });
        let mut returned = Vec::new();
        for (id, owner) in ownership.iter_mut() {
            match owner {
                Ownership::Transferring { epoch } => {
                    *owner = Ownership::AwaitingVerification { epoch: *epoch };
                    returned.push(id.clone());
                }
                Ownership::Hydrating { epoch } | Ownership::SettingUp { epoch } if device => {
                    *owner = Ownership::AwaitingVerification { epoch: *epoch };
                    returned.push(id.clone());
                }
                _ => {}
            }
        }
        returned
    };
    if let Err(error) = super::persist(&state).await {
        return failure(error);
    }
    // No project file names stay behind for agents of a later sign-in.
    crate::mcp::cloud_context::forget_all(&state).await;
    if device {
        // A project this computer owned can still hold sessions that were
        // due to resume: a return's resume still in flight (one resumer per
        // session, `ledger::resume_one`), or a verified grant's that
        // `stop_tasks` just aborted. No owner is left to resume them, so they
        // resume here now, as that resume would have (an uncertain
        // continuation stays deferred, as it would there).
        let waiting: std::collections::HashSet<String> = lock(&state.deferred_sessions)
            .values()
            .map(|entry| entry.workspace_id.clone())
            .collect();
        returned.extend(
            dropped
                .into_iter()
                .filter(|id| waiting.contains(id) && execution::resume_allowed(&state, id)),
        );
    }
    if device && !returned.is_empty() {
        let owner = state.clone();
        tokio::spawn(async move {
            for workspace in returned {
                if let Err(error) =
                    crate::ledger::resume_deferred_workspace(&owner, &workspace).await
                {
                    tracing::warn!(%error, "Could not resume a project's sessions after sign-out");
                }
            }
        });
    }
    StatusCode::NO_CONTENT.into_response()
}
pub(crate) async fn status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let authority = lock(&state.pro.authority).clone();
    let sessions: Vec<_> = crate::session_view::sessions_json(&state)
        .into_iter()
        .filter(|row| {
            !authority.restricted()
                || row["workspace_id"]
                    .as_str()
                    .is_some_and(|id| authority.allows(id))
        })
        .collect();
    let workspaces = lock(&state.workspaces).list();
    let preferences = lock(&state.pro.preferences).clone();
    let ownership = lock(&state.pro.ownership).clone();
    let mut statuses = lock(&state.pro.status).clone();
    if state.daemon_extension.is_none() && state.pro.configured.load(Ordering::Acquire) {
        for workspace in &workspaces {
            if ownership.contains_key(&workspace.id)
                || preferences
                    .get(&workspace.id)
                    .is_some_and(|p| p.account.is_some())
            {
                let status = statuses.entry(workspace.id.clone()).or_default();
                // Preserve enrollment/proof fields; this is missing policy
                // capability, never an account/ownership downgrade.
                if status.error.is_none() {
                    status.error = Some("optional_runtime_unavailable".into());
                    status.error_code = Some("optional_runtime_unavailable");
                }
            }
        }
    }
    // A delegation the account refused, or one that expired unrenewed, can do
    // nothing: report the daemon as not configured (and why, additively) so
    // the native app mints a fresh one instead of trusting its cached stamp.
    let renewal_failed = super::delegation_lapsed(&state);
    let config = lock(&state.pro.runtime).clone();
    // Additive per row: `place` (where its work runs: here, the cloud, or
    // another computer by name), `reason` (why the cloud is not running work
    // that needed it, or why "Run in the cloud" could not start), and the
    // two choices that apply now, `run_here` and `run_in_cloud`.
    Json(
        json!({"configured":state.pro.configured.load(Ordering::Acquire) && !renewal_failed,"renewal_failed":renewal_failed,"workspace_configuration":authority.acknowledgment(),"projects_root":super::projects_root(&state),"projects_root_confirmed":lock(&state.pro.projects_root).is_some(),"sessions":sessions,"workspaces":workspaces.into_iter().filter(|workspace| authority.allows(&workspace.id)).take(128).map(|workspace|json!({"workspace_id":workspace.id,"name":workspace.name,"root":workspace.root,"never_mirror":preferences.get(&workspace.id).is_some_and(|p|p.never_mirror),"privacy_pending":preferences.get(&workspace.id).is_some_and(|p|p.privacy_pending),"ownership":ownership.get(&workspace.id),"continuity":preferences.get(&workspace.id).and_then(|p|p.continuity.as_ref()),"local_copy":super::local_copy_view(&state,&workspace.id),"git_staging":preferences.get(&workspace.id).and_then(|p|p.git_staging.as_ref()),"execution_allowed":super::may_execute(&state,&workspace.id),"bundle_import":state.bundle_imports.view(&workspace.id),"execution_uncertain":preferences.get(&workspace.id).is_some_and(|p|p.execution_uncertain),"mirror":statuses.get(&workspace.id),"blocked_providers":statuses.get(&workspace.id).map(|status|status.blocked_providers.clone()).unwrap_or_default(),"git_branches":preferences.get(&workspace.id).map(|p|&p.git_branches),"place":super::place::place(&state,&workspace.id),"reason":state.pro.reasons.get(&workspace.id),"run_here":super::place::may_run_here(&state,config.as_ref(),&workspace.id),"run_in_cloud":!renewal_failed && super::place::may_run_in_cloud(&state,config.as_ref(),&workspace.id)})).collect::<Vec<_>>()}),
    )
}
/// A project a sleep flush or "Run in the cloud" may hand over: this computer owns it, the
/// workspace is in scope and copying it is allowed.
pub(super) fn flushable(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.authority).allows(workspace)
        && !lock(&state.pro.preferences)
            .get(workspace)
            .is_some_and(|p| p.never_mirror)
        && super::owned_epoch(state, workspace).is_some()
}
pub(crate) async fn privacy(
    State(state): State<Arc<AppState>>,
    Json(request): Json<Privacy>,
) -> Response {
    if let Err(error) = authority::workspace(&state, &request.workspace_id) {
        return failure(error);
    }
    if !super::valid_id(&request.workspace_id)
        || lock(&state.workspaces).get(&request.workspace_id).is_none()
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    if super::drain::draining(&state) {
        return super::drain::refusal().into_response();
    }
    let _guard = state.pro.jobs.lock().await;
    {
        let mut preferences = lock(&state.pro.preferences);
        if preferences.len() >= 128 && !preferences.contains_key(&request.workspace_id) {
            return StatusCode::INSUFFICIENT_STORAGE.into_response();
        }
        let preference = preferences.entry(request.workspace_id.clone()).or_default();
        preference.never_mirror = request.never_mirror;
        preference.privacy_pending = request.never_mirror && !request.confirmed;
    }
    if request.never_mirror && request.confirmed {
        let mut ownership = lock(&state.pro.ownership);
        // An acknowledged account privacy fence prevents a cloud takeover.
        // Leave remote and unverified writers fenced; a verified local writer
        // can thereafter restart without needing the optional cloud account.
        if matches!(
            ownership.get(&request.workspace_id),
            Some(Ownership::Local { .. })
        ) {
            ownership.remove(&request.workspace_id);
        }
    }
    if request.never_mirror {
        // Kept on this computer from now on: nothing of its moves is told.
        crate::mcp::cloud_context::forget(&state, &request.workspace_id).await;
    }
    match super::persist(&state).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(error),
    }
}
#[derive(Default, Deserialize)]
struct SleepRequest {
    /// How long the caller will wait before the computer sleeps (additive).
    #[serde(default)]
    deadline_ms: Option<u64>,
}
/// Time kept back from the caller's deadline for the reply itself.
const SLEEP_REPLY_MARGIN: std::time::Duration = std::time::Duration::from_millis(1500);
/// Default budget for callers that send no deadline (macOS allows ~25 s).
pub(super) const SLEEP_DEFAULT_MS: u64 = 25_000;

/// `POST /pro/sleep {deadline_ms?}`: this computer is about to sleep. The
/// daemon's own sleep watcher (`sleep_watch`, macOS) calls [`sleeping`]
/// directly; this route is the same function for tests and for hosts that
/// learn of sleep another way.
pub(crate) async fn sleep(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    let request: SleepRequest = if body.iter().all(u8::is_ascii_whitespace) {
        SleepRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(request) => request,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    };
    if lock(&state.pro.runtime).is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }
    let budget = std::time::Duration::from_millis(
        request
            .deadline_ms
            .unwrap_or(SLEEP_DEFAULT_MS)
            .clamp(1_000, 120_000),
    );
    Json(sleeping(&state, budget).await).into_response()
}

/// Hands projects with running agents to the cloud before this computer
/// sleeps, within one budget: the periodic pass is preempted, every flush runs
/// in parallel as an owned task (stopping its agents first), projects with
/// live agents go first, and a flush that outlives the budget still finishes
/// or recovers on its own instead of leaving half a transfer. The same
/// conversation continues in the cloud from current files, without a fork.
/// Nothing here depends on the app.
pub(super) async fn sleeping(
    state: &Arc<AppState>,
    budget: std::time::Duration,
) -> serde_json::Value {
    let Some(config) = lock(&state.pro.runtime).clone() else {
        return json!({"handoff":false,"reason":"not_configured","failed":[]});
    };
    // Asleep is not reachable: the guard for bringing work home starts over.
    super::reach::unreachable(state);
    if config.hours_exhausted {
        return json!({"handoff":false,"reason":"cloud_hours_exhausted","failed":[]});
    }
    if super::drain::draining(state) {
        return json!({"handoff":false,"reason":"draining","failed":[]});
    }
    let budget = budget.saturating_sub(SLEEP_REPLY_MARGIN);
    let deadline = tokio::time::Instant::now() + budget;
    let handover = Handover {
        deadline,
        park: false,
    };
    let (coordinator, unavailable) = match hand_over(state, config, handover, None).await {
        Ok(started) => started,
        Err(reason) => return json!({"handoff":false,"reason":reason,"failed":[]}),
    };
    let failed = |results: Vec<(String, anyhow::Result<()>)>| -> Vec<serde_json::Value> {
        unavailable
            .iter()
            .map(|id| json!({"workspace_id":id,"error":"unavailable"}))
            .chain(results.into_iter().filter_map(|(workspace, result)| {
                result.err().map(
                    |error| json!({"workspace_id":workspace,"error":engine::failure_code(&error)}),
                )
            }))
            .collect()
    };
    match tokio::time::timeout_at(deadline + SLEEP_REPLY_MARGIN / 2, coordinator).await {
        Ok(Ok(results)) => {
            let failed = failed(results);
            json!({"handoff":failed.is_empty(),"failed":failed})
        }
        Ok(Err(_)) => {
            json!({"handoff":false,"reason":"transfer_interrupted","failed":failed(Vec::new())})
        }
        Err(_) => {
            // Still publishing: the flushes finish or recover on their own.
            let pending: Vec<_> = lock(&state.pro.sleeping).iter().cloned().collect();
            json!({"handoff":false,"reason":"deadline","pending":pending,"failed":failed(Vec::new())})
        }
    }
}
/// Each flush's result, by project.
pub(super) type Flushes = tokio::task::JoinHandle<Vec<(String, anyhow::Result<()>)>>;

/// How a handover started: what `hand_over` needs besides its projects.
#[derive(Clone, Copy)]
pub(super) struct Handover {
    pub deadline: tokio::time::Instant,
    /// "Run in the cloud": exactly the listed projects, parked.
    pub park: bool,
}

/// Starts handing projects to the cloud within `deadline`: the sleep flush,
/// and with `park` "Run in the cloud" (exactly `workspace_ids` then). Returns
/// the running flushes and the listed projects this computer cannot hand
/// over, or why nothing started (`transfer_busy`).
pub(super) async fn hand_over(
    state: &Arc<AppState>,
    config: Configure,
    handover: Handover,
    workspace_ids: Option<Vec<String>>,
) -> Result<(Flushes, Vec<String>), &'static str> {
    let Handover { deadline, park } = handover;
    // Only a wake advances this: a sleep flush and "Run in the cloud" running
    // at once never cancel each other (review R3 S1).
    let generation = state.pro.sleep_generation.load(Ordering::Acquire);
    let sleep = engine::Sleep {
        generation,
        deadline,
        park,
    };
    // The periodic pass holds the job reservation across every project; a
    // sleep preempts it rather than queueing behind a long push. Anything it
    // leaves half-done recovers on the next pass (locks, fences, returns).
    let jobs = state.pro.jobs.clone();
    let guard = match tokio::time::timeout(
        std::time::Duration::from_millis(500),
        jobs.clone().lock_owned(),
    )
    .await
    {
        Ok(guard) => guard,
        Err(_) => {
            let pass = lock(&state.pro.mirror_task).take();
            if let Some(pass) = pass {
                pass.abort();
                let _ = pass.await;
            }
            match tokio::time::timeout_at(deadline, jobs.lock_owned()).await {
                Ok(guard) => guard,
                Err(_) => return Err("transfer_busy"),
            }
        }
    };
    let mut active = Vec::new();
    let mut quiet = Vec::new();
    for workspace in lock(&state.workspaces).list().into_iter().take(128) {
        if workspace_ids
            .as_ref()
            .is_some_and(|ids| !ids.contains(&workspace.id))
            || !flushable(state, &workspace.id)
            // Already being handed over (a "Run in the cloud" claim, or an
            // earlier flush still running): never flushed twice at once.
            || (!park && lock(&state.pro.sleeping).contains(&workspace.id))
        {
            continue;
        }
        if engine::live_agents(state, &workspace.id) {
            active.push(workspace.id);
        } else {
            quiet.push(workspace.id);
        }
    }
    // A listed project this computer cannot hand over (not owned here, kept
    // on this computer, unknown) keeps working here; the caller hears so.
    let unavailable: Vec<String> = workspace_ids
        .into_iter()
        .flatten()
        .filter(|id| !active.contains(id) && !quiet.contains(id))
        .collect();
    let owner = state.clone();
    let coordinator = tokio::spawn(async move {
        let _guard = guard;
        let flush = |workspace: String| {
            let owner = owner.clone();
            let config = config.clone();
            async move {
                if sleep.woke(&owner) {
                    return (workspace, Ok(()));
                }
                if sleep.park {
                    // Parked before its agents stop, so the lease loop and the
                    // return path leave it alone from the moment it leaves.
                    if !super::park(&owner, &workspace, sleep.generation) {
                        return (workspace, Ok(()));
                    }
                    if let Err(error) = super::persist(&owner).await {
                        tracing::warn!(%error, "Could not save that a project moved to the cloud");
                    }
                }
                lock(&owner.pro.sleeping).insert(workspace.clone());
                // Owned: the deadline returning early never cancels a flush.
                let task = tokio::spawn({
                    let owner = owner.clone();
                    let workspace = workspace.clone();
                    async move { engine::sleep_flush(&owner, &config, &workspace, sleep).await }
                });
                let result = task
                    .await
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("transfer_interrupted")));
                if sleep.park && result.is_err() {
                    // Not handed over: its work continues on this computer.
                    super::unpark(&owner, &workspace);
                    if let Err(error) = super::persist(&owner).await {
                        tracing::warn!(%error, "Could not save that a project stays here");
                    }
                }
                lock(&owner.pro.sleeping).remove(&workspace);
                (workspace, result)
            }
        };
        if sleep.park {
            // The user asked for exactly these projects to move; the time
            // rule below is only for sleep's opportunistic copies.
            return futures::future::join_all(active.into_iter().chain(quiet).map(flush)).await;
        }
        let mut results = futures::future::join_all(active.into_iter().map(flush)).await;
        // Projects without running agents are only worth publishing while time
        // remains; the cloud would not start them anyway.
        if tokio::time::Instant::now() + std::time::Duration::from_secs(5) < sleep.deadline
            && !sleep.woke(&owner)
        {
            results.extend(futures::future::join_all(quiet.into_iter().map(flush)).await);
        }
        results
    });
    Ok((coordinator, unavailable))
}
/// This computer woke (the daemon's sleep watcher, `sleep_watch`): a flush
/// still running for the sleep that just ended keeps its publication but no
/// longer releases, and returns its project itself. A project whose flush
/// ended without handing over waits for the lease loop to verify who holds
/// it now (`AwaitingVerification`): the cloud may have taken it while this
/// computer slept, so nothing resumes here on sight. The guard for bringing
/// work home starts over (a lid opened for a moment pulls nothing home).
pub(super) async fn woke(state: &Arc<AppState>) {
    state.pro.sleep_generation.fetch_add(1, Ordering::AcqRel);
    super::reach::unreachable(state);
    lock(&state.pro.release_pending).clear();
    {
        let mut ownership = lock(&state.pro.ownership);
        for owner in ownership.values_mut() {
            if let Ownership::Transferring { epoch } = owner {
                *owner = Ownership::AwaitingVerification { epoch: *epoch };
            }
        }
    }
    if let Err(error) = super::persist(state).await {
        tracing::warn!(%error, "Could not save project ownership after waking");
    }
    state.pro.renew_now.notify_waiters();
}
pub(crate) async fn hydrate(
    State(state): State<Arc<AppState>>,
    Json(request): Json<Hydrate>,
) -> Response {
    if let Err(error) = authority::workspace(&state, &request.workspace_id) {
        return failure(error);
    }
    if !super::valid_id(&request.workspace_id) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let workspace = request.workspace_id.clone();
    let epoch = request.expected_epoch;
    let owner = state.clone();
    let checked = state.clone();
    detached::run(
        &state,
        ("hydrate", false),
        &workspace,
        epoch,
        || super::drain::draining(&checked).then(super::drain::refusal),
        move || hydrate_owned(owner, request),
    )
    .await
    .into_response()
}
async fn hydrate_owned(state: Arc<AppState>, mut request: Hydrate) -> detached::Outcome {
    let Some(_guard) = super::drain::reserve(&state).await else {
        return super::drain::refusal();
    };
    let (config, generation) = {
        let _configuration = state.pro.configuration.lock().await;
        let Some(config) = lock(&state.pro.runtime).clone() else {
            return detached::Outcome::refused(StatusCode::PRECONDITION_FAILED, None);
        };
        (config, state.pro.generation.load(Ordering::Acquire))
    };
    request.destination_root = match authority::destination(
        &state,
        &config,
        &request.workspace_id,
        request.destination_root.as_deref(),
    )
    .await
    {
        Ok(root) => root,
        Err(error) => return outcome(error),
    };
    // Running here with some sessions waiting for a provider: after sign-in
    // resume the ready ones; nothing is fetched or reinstalled.
    if config.role == super::protocol::Role::Worker
        && lock(&state.workspaces).get(&request.workspace_id).is_some()
        && matches!(lock(&state.pro.ownership).get(&request.workspace_id),Some(Ownership::Local{epoch}) if *epoch==request.expected_epoch)
        && lock(&state.pro.status)
            .get(&request.workspace_id)
            .is_some_and(|status| !status.blocked_providers.is_empty())
    {
        return result(super::provider_gate::resume_ready(&state, &request.workspace_id).await);
    }
    if lock(&state.workspaces).get(&request.workspace_id).is_some()
        && matches!(lock(&state.pro.ownership).get(&request.workspace_id),Some(Ownership::SettingUp{epoch}) if *epoch==request.expected_epoch)
    {
        return result(
            engine::hydrate(
                &state,
                &config,
                &request.workspace_id,
                request.expected_epoch,
                false,
                None,
            )
            .await,
        );
    }
    if config.role == super::protocol::Role::Worker
        && lock(&state.workspaces).get(&request.workspace_id).is_some()
        && matches!(lock(&state.pro.ownership).get(&request.workspace_id),Some(Ownership::Local{epoch}|Ownership::AwaitingVerification{epoch}) if *epoch==request.expected_epoch)
    {
        // Only the verified hydration path can prove a same-owner restart.
        // Reconciliation may legitimately do nothing while this route holds
        // the job reservation, so its success alone is not a completed import.
        return result(
            engine::hydrate(
                &state,
                &config,
                &request.workspace_id,
                request.expected_epoch,
                request.requires_fork,
                request.destination_root.as_deref(),
            )
            .await,
        );
    }
    let cache = state
        .pro
        .root
        .join(&request.workspace_id)
        .join("incoming.git");
    let cache_mutex = match state.pro.cache(&request.workspace_id) {
        Ok(cache) => cache,
        Err(error) => return outcome(error),
    };
    let cache_guard = Arc::new(cache_mutex.lock_owned().await);
    if generation != state.pro.generation.load(Ordering::Acquire) {
        return outcome(anyhow::anyhow!(
            "Account changed while waiting for project cache"
        ));
    }
    if let Err(error) = super::transport::cache_quiescent(&request.workspace_id) {
        return outcome(error);
    }
    let manifest = match super::transport::cache_scope(
        &request.workspace_id,
        cache_guard.clone(),
        engine::fetch_snapshot(
            &state,
            &config,
            &request.workspace_id,
            &cache,
            cache_guard.clone(),
            generation,
        ),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return outcome(error),
    };
    drop(cache_guard);
    let root = request
        .destination_root
        .clone()
        .or_else(|| {
            lock(&state.workspaces)
                .get(&request.workspace_id)
                .map(|workspace| workspace.root)
        })
        .unwrap_or_else(|| manifest.root.clone());
    let required_root = root.clone();
    let ready = tokio::task::spawn_blocking(move || {
        if !root.is_dir() {
            return false;
        }
        let probe = root.join(format!(
            ".chimaera-root-probe-{}",
            chimaera_core::generate_token()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(_) => {
                let _ = std::fs::remove_file(probe);
                true
            }
            Err(_) => false,
        }
    })
    .await
    .unwrap_or(false);
    if !ready {
        return detached::Outcome::refused(
            StatusCode::CONFLICT,
            Some(
                json!({"error":"root_setup_required","root":required_root,"workspace_id":request.workspace_id}),
            ),
        );
    }
    if generation != state.pro.generation.load(Ordering::Acquire) {
        return outcome(anyhow::anyhow!("Account changed during project transfer"));
    }
    result(
        engine::hydrate(
            &state,
            &config,
            &request.workspace_id,
            request.expected_epoch,
            request.requires_fork,
            request.destination_root.as_deref(),
        )
        .await,
    )
}

#[derive(Deserialize)]
pub(crate) struct Handoff {
    workspace_id: String,
    expected_epoch: u64,
}
/// The clean flush is an owned task: its caller (the keeper relay, a worker
/// supervisor) may give up, and the flush still completes or recovers. A
/// repeated request for the same epoch joins it; a completed success answers
/// 204 again instead of a bare conflict.
pub(crate) async fn handoff(
    State(state): State<Arc<AppState>>,
    Json(request): Json<Handoff>,
) -> Response {
    if let Err(error) = authority::workspace(&state, &request.workspace_id) {
        return failure(error);
    }
    let Some(config) = lock(&state.pro.runtime).clone() else {
        return StatusCode::PRECONDITION_FAILED.into_response();
    };
    let workspace = request.workspace_id.clone();
    let epoch = request.expected_epoch;
    // A machine this request just woke is still renewing its own lease: let
    // that answer first (bounded) instead of refusing the return it came for.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while execution::resuming(&state, &workspace) {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await;
    let owner = state.clone();
    let checked = state.clone();
    let key = request.workspace_id;
    detached::run(
        &state,
        ("handoff", true),
        &key,
        epoch,
        || handoff_refusal(&checked, &key, epoch),
        move || async move {
            let Some(_guard) = super::drain::reserve(&owner).await else {
                return super::drain::refusal();
            };
            if let Some(refusal) = handoff_refusal(&owner, &workspace, epoch) {
                return refusal;
            }
            result(engine::snapshot(&owner, &config, &workspace, true).await)
        },
    )
    .await
    .into_response()
}
/// Hands a project this computer runs to another of the user's computers,
/// whose user acted on it (`moves`): the same owned clean flush as
/// `/pro/handoff` (stop at the pause, publish, release), keyed the same way,
/// so a repeated attempt joins it. `true` once released; a failed flush has
/// already recovered here and the work stays.
pub(super) async fn hand_to_computer(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    epoch: u64,
) -> bool {
    let owner = state.clone();
    let config = config.clone();
    let key = workspace.to_owned();
    detached::run(
        state,
        ("handoff", true),
        workspace,
        epoch,
        || handoff_refusal(state, workspace, epoch),
        move || async move {
            let Some(_guard) = super::drain::reserve(&owner).await else {
                return super::drain::refusal();
            };
            if let Some(refusal) = handoff_refusal(&owner, &key, epoch) {
                return refusal;
            }
            result(engine::snapshot(&owner, &config, &key, true).await)
        },
    )
    .await
    .ok()
}
fn handoff_refusal(state: &AppState, workspace: &str, epoch: u64) -> Option<detached::Outcome> {
    if super::drain::draining(state) {
        return Some(super::drain::refusal());
    }
    if super::owned_epoch(state, workspace) != Some(epoch) {
        return Some(detached::Outcome::refused(StatusCode::CONFLICT, None));
    }
    if !engine::at_pause(state, workspace) {
        return Some(detached::Outcome::refused(
            StatusCode::CONFLICT,
            Some(json!({"error":"workspace_busy"})),
        ));
    }
    if execution::managed(state, workspace) {
        if let Err(error) = super::validate_execution_scope(state, workspace, epoch) {
            return Some(outcome(error));
        }
    }
    None
}

#[derive(Deserialize)]
pub(crate) struct Projects {
    root: std::path::PathBuf,
}
pub(crate) async fn projects(
    State(state): State<Arc<AppState>>,
    Json(request): Json<Projects>,
) -> Response {
    if lock(&state.pro.authority).restricted() {
        return failure(anyhow::anyhow!("workspace destination is fixed"));
    }
    if !request.root.is_absolute()
        || request
            .root
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let root = request.root;
    let checked = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let canonical = std::fs::canonicalize(root)?;
        anyhow::ensure!(canonical.is_dir(), "projects folder is not a directory");
        Ok(canonical)
    })
    .await;
    match checked {
        Ok(Ok(root)) => *lock(&state.pro.projects_root) = Some(root),
        Ok(Err(error)) => return failure(error),
        Err(error) => return failure(error.into()),
    };
    match super::persist(&state).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(error),
    }
}

// Discovery is passive; adoption has a separate, explicit local action.
pub(crate) use super::projects::{copy_project, open_project, project_list, takeover_project};
