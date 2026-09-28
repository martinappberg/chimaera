use super::{
    authority, engine, execution,
    protocol::{Configure, WorkspaceConfigure},
    transport, Ownership,
};
use crate::{lock, AppState};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::{atomic::Ordering, Arc};

pub(super) fn failure(error: anyhow::Error) -> Response {
    if let Some(blocked) = error.downcast_ref::<super::provider_gate::Blocked>() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"cloud_provider_not_ready","blocked_providers":blocked.0})),
        )
            .into_response();
    }
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":error.to_string().chars().take(256).collect::<String>()})),
    )
        .into_response()
}
#[derive(Deserialize)]
pub(crate) struct WorkspaceQuery {
    workspace_id: String,
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
                || (!state.pro.execution.invalid
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
        *lock(&state.pro.runtime) = Some(config);
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
    *lock(&state.pro.runtime) = Some(config);
    state.pro.configured.store(true, Ordering::Release);
    engine::start(state.clone());
    if managed {
        execution::start(&state);
    }
    response
}
async fn stop_tasks(state: &Arc<AppState>) -> anyhow::Result<()> {
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
    execution::stop(state, &stopping).await?;
    execution::clear_stopped(state);
    Ok(())
}
pub(crate) async fn disconnect(State(state): State<Arc<AppState>>) -> Response {
    let _configuration = state.pro.configuration.lock().await;
    if let Err(error) = stop_tasks(&state).await {
        return failure(error);
    }
    *lock(&state.pro.project_cache) = Default::default();
    *lock(&state.pro.runtime) = None;
    state.pro.configured.store(false, Ordering::Release);
    // Known remote ownership remains fenced across sign-out and restart.
    lock(&state.pro.ownership).retain(|_, owner| !matches!(owner, Ownership::Local { .. }));
    match super::persist(&state).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(error),
    }
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
    let statuses = lock(&state.pro.status).clone();
    Json(
        json!({"configured":state.pro.configured.load(Ordering::Acquire),"workspace_configuration":authority.acknowledgment(),"projects_root":super::projects_root(&state),"projects_root_confirmed":lock(&state.pro.projects_root).is_some(),"sessions":sessions,"workspaces":workspaces.into_iter().filter(|workspace| authority.allows(&workspace.id)).take(128).map(|workspace|json!({"workspace_id":workspace.id,"name":workspace.name,"root":workspace.root,"never_mirror":preferences.get(&workspace.id).is_some_and(|p|p.never_mirror),"privacy_pending":preferences.get(&workspace.id).is_some_and(|p|p.privacy_pending),"ownership":ownership.get(&workspace.id),"continuity":preferences.get(&workspace.id).and_then(|p|p.continuity.as_ref()),"execution_allowed":super::may_execute(&state,&workspace.id),"execution_uncertain":preferences.get(&workspace.id).is_some_and(|p|p.execution_uncertain),"mirror":statuses.get(&workspace.id),"blocked_providers":statuses.get(&workspace.id).map(|status|status.blocked_providers.clone()).unwrap_or_default(),"git_branches":preferences.get(&workspace.id).map(|p|&p.git_branches),"profile":preferences.get(&workspace.id).map(|p|&p.profile)})).collect::<Vec<_>>()}),
    )
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
    match super::persist(&state).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(error),
    }
}
pub(crate) async fn profile(
    State(state): State<Arc<AppState>>,
    Query(query): Query<WorkspaceQuery>,
) -> Response {
    if let Err(error) = authority::workspace(&state, &query.workspace_id) {
        return failure(error);
    }
    if lock(&state.workspaces).get(&query.workspace_id).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(
        lock(&state.pro.preferences)
            .get(&query.workspace_id)
            .map(|p| p.profile.clone())
            .unwrap_or_default(),
    )
    .into_response()
}
pub(crate) async fn put_profile(
    State(state): State<Arc<AppState>>,
    Query(query): Query<WorkspaceQuery>,
    Json(profile): Json<super::policy::CloudProfile>,
) -> Response {
    if let Err(error) = authority::workspace(&state, &query.workspace_id) {
        return failure(error);
    }
    let _configuration = state.pro.configuration.lock().await;
    let Ok(_job) = state.pro.jobs.try_lock() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"Project transfer is active; retry after it finishes"})),
        )
            .into_response();
    };
    if let Err(error) = authority::workspace(&state, &query.workspace_id) {
        return failure(error);
    }
    if lock(&state.workspaces).get(&query.workspace_id).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Err(error) = profile.validate() {
        return failure(error);
    }
    lock(&state.pro.preferences)
        .entry(query.workspace_id)
        .or_default()
        .profile = profile;
    match super::persist(&state).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(error),
    }
}
pub(crate) async fn sleep(State(state): State<Arc<AppState>>) -> Response {
    let Some(config) = lock(&state.pro.runtime).clone() else {
        return StatusCode::NO_CONTENT.into_response();
    };
    if config.hours_exhausted {
        return Json(json!({"handoff":false,"reason":"cloud_hours_exhausted"})).into_response();
    }
    let _guard = state.pro.jobs.lock().await;
    let workspaces = lock(&state.workspaces).list();
    let mut failed = Vec::new();
    for workspace in workspaces
        .into_iter()
        .filter(|workspace| lock(&state.pro.authority).allows(&workspace.id))
        .take(128)
    {
        if lock(&state.pro.preferences)
            .get(&workspace.id)
            .is_some_and(|p| p.never_mirror)
            || super::owned_epoch(&state, &workspace.id).is_none()
        {
            continue;
        }
        if let Err(error) = engine::snapshot(&state, &config, &workspace.id, true).await {
            failed.push(json!({"workspace_id":workspace.id,"error":error.to_string().chars().take(256).collect::<String>()}));
        }
    }
    Json(json!({"handoff":failed.is_empty(),"failed":failed})).into_response()
}
pub(crate) async fn wake(State(state): State<Arc<AppState>>) -> Response {
    state.pro.awake_since.store(super::now(), Ordering::Release);
    {
        let mut ownership = lock(&state.pro.ownership);
        let authority = lock(&state.pro.authority).clone();
        for (_, owner) in ownership.iter_mut().filter(|(id, _)| authority.allows(id)) {
            if let Ownership::Transferring { epoch } = owner {
                *owner = Ownership::AwaitingVerification { epoch: *epoch };
            }
        }
    }
    match super::persist(&state).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(error),
    }
}
pub(crate) async fn hydrate(
    State(state): State<Arc<AppState>>,
    Json(mut request): Json<Hydrate>,
) -> Response {
    if let Err(error) = authority::workspace(&state, &request.workspace_id) {
        return failure(error);
    }
    if !super::valid_id(&request.workspace_id) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let _guard = state.pro.jobs.lock().await;
    let (config, generation) = {
        let _configuration = state.pro.configuration.lock().await;
        let Some(config) = lock(&state.pro.runtime).clone() else {
            return StatusCode::PRECONDITION_FAILED.into_response();
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
        Err(error) => return failure(error),
    };
    if lock(&state.workspaces).get(&request.workspace_id).is_some()
        && matches!(lock(&state.pro.ownership).get(&request.workspace_id),Some(Ownership::SettingUp{epoch}) if *epoch==request.expected_epoch)
    {
        return match engine::hydrate(
            &state,
            &config,
            &request.workspace_id,
            request.expected_epoch,
            false,
            None,
        )
        .await
        {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => failure(error),
        };
    }
    if config.role == super::protocol::Role::Worker
        && lock(&state.workspaces).get(&request.workspace_id).is_some()
        && matches!(lock(&state.pro.ownership).get(&request.workspace_id),Some(Ownership::Local{epoch}|Ownership::AwaitingVerification{epoch}) if *epoch==request.expected_epoch)
    {
        // Only the verified hydration path can prove a same-owner restart.
        // Reconciliation may legitimately do nothing while this route holds
        // the job reservation, so its success alone is not a completed import.
        return match engine::hydrate(
            &state,
            &config,
            &request.workspace_id,
            request.expected_epoch,
            request.requires_fork,
            request.destination_root.as_deref(),
        )
        .await
        {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => failure(error),
        };
    }
    let cache = state
        .pro
        .root
        .join(&request.workspace_id)
        .join("incoming.git");
    let cache_mutex = match state.pro.cache(&request.workspace_id) {
        Ok(cache) => cache,
        Err(error) => return failure(error),
    };
    let cache_guard = Arc::new(cache_mutex.lock_owned().await);
    if generation != state.pro.generation.load(Ordering::Acquire) {
        return failure(anyhow::anyhow!(
            "Account changed while waiting for project cache"
        ));
    }
    if let Err(error) = super::transport::cache_quiescent(&request.workspace_id) {
        return failure(error);
    }
    let manifest = match super::transport::cache_scope(
        &request.workspace_id,
        cache_guard.clone(),
        engine::fetch_snapshot(&config, &request.workspace_id, &cache),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return failure(error),
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
        return (StatusCode::CONFLICT,Json(json!({"error":"root_setup_required","root":required_root,"workspace_id":request.workspace_id}))).into_response();
    }
    if generation != state.pro.generation.load(Ordering::Acquire) {
        return failure(anyhow::anyhow!("Account changed during project transfer"));
    }
    match engine::hydrate(
        &state,
        &config,
        &request.workspace_id,
        request.expected_epoch,
        request.requires_fork,
        request.destination_root.as_deref(),
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
pub(crate) struct Pin {
    session_id: String,
    keep_running: bool,
}
pub(crate) async fn pin(State(state): State<Arc<AppState>>, Json(request): Json<Pin>) -> Response {
    if lock(&state.pro.authority).restricted() {
        let workspace = lock(&state.session_workspaces)
            .get(&request.session_id)
            .cloned();
        if workspace
            .as_ref()
            .is_none_or(|workspace| authority::workspace(&state, workspace).is_err())
        {
            return failure(anyhow::anyhow!("workspace authority denied"));
        }
    }
    if state.sessions.get(&request.session_id).is_none()
        && state.chat.get(&request.session_id).is_none()
        && !lock(&state.deferred_sessions).contains_key(&request.session_id)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    match super::set_keep_running(&state, &request.session_id, request.keep_running).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
pub(crate) struct Power {
    suitable: bool,
}
pub(crate) async fn power(
    State(state): State<Arc<AppState>>,
    Json(power): Json<Power>,
) -> Response {
    if state
        .pro
        .power_suitable
        .swap(power.suitable, Ordering::AcqRel)
        != power.suitable
    {
        state.pro.awake_since.store(super::now(), Ordering::Release);
    }
    StatusCode::NO_CONTENT.into_response()
}
#[derive(Deserialize)]
pub(crate) struct Handoff {
    workspace_id: String,
    expected_epoch: u64,
}
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
    if super::owned_epoch(&state, &request.workspace_id) != Some(request.expected_epoch) {
        return StatusCode::CONFLICT.into_response();
    }
    if !engine::at_pause(&state, &request.workspace_id) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"workspace_busy"})),
        )
            .into_response();
    }
    if execution::managed(&state, &request.workspace_id) {
        if let Err(error) =
            super::validate_execution_scope(&state, &request.workspace_id, request.expected_epoch)
        {
            return failure(error);
        }
    }
    let _guard = state.pro.jobs.lock().await;
    if super::owned_epoch(&state, &request.workspace_id) != Some(request.expected_epoch)
        || !engine::at_pause(&state, &request.workspace_id)
    {
        return StatusCode::CONFLICT.into_response();
    }
    match engine::snapshot(&state, &config, &request.workspace_id, true).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(error),
    }
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
pub(crate) use super::projects::{open_project, project_list};
