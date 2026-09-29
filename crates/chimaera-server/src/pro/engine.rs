use super::{
    authority, config, execution, mirror,
    protocol::{Baton, Configure, MirrorCredentials, Role},
    transport, Ownership, WorkspaceStatus,
};
use crate::{lock, AppState};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

#[path = "handback.rs"]
mod handback;
#[path = "release.rs"]
mod release;
#[path = "snapshot_diagnostics.rs"]
mod snapshot_diagnostics;

#[derive(Serialize, Deserialize)]
pub(super) struct Manifest {
    version: u32,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    repository_origin: Option<String>,
    #[serde(default)]
    repository: Option<super::repository::Snapshot>,
    workspace_id: String,
    pub root: PathBuf,
    name: String,
    epoch: u64,
    clean: bool,
    #[serde(default)]
    continuation: execution::wire::Continuation,
    profile: super::policy::CloudProfile,
    sessions: Vec<SessionArchive>,
}
#[derive(Serialize, Deserialize)]
struct SessionArchive {
    id: String,
    archive: String,
}

pub(super) async fn account(
    config: &Configure,
    path: &str,
    method: &str,
    body: Option<&serde_json::Value>,
) -> Result<transport::Response> {
    if config.recovery {
        ensure!(
            method == "POST"
                && matches!(
                    path,
                    "/v2/recovery/mirror/credentials"
                        | "/v2/recovery/checkpoint"
                        | "/v2/recovery/release"
                ),
            "recovery authority cannot execute ordinary account requests"
        );
    }
    authority::account_request(config, path, method, body)?;
    transport::request(
        &config.endpoint,
        path,
        method,
        &config.delegation.access_token,
        body,
    )
    .await
}
async fn credentials(
    config: &Configure,
    workspace: &str,
    epoch: Option<u64>,
) -> Result<MirrorCredentials> {
    authority::config_workspace(config, workspace)?;
    let mut body = json!({"workspace_id":workspace});
    if let Some(epoch) = epoch {
        body["epoch"] = epoch.into();
    }
    let credentials: MirrorCredentials = account(
        config,
        if config.recovery {
            "/v2/recovery/mirror/credentials"
        } else if config.execution.is_some() {
            "/v2/mirror/credentials"
        } else {
            "/v1/mirror/credentials"
        },
        "POST",
        Some(&body),
    )
    .await?
    .json()?;
    ensure!(
        credentials.workspace_id == workspace
            && credentials.storage_limit_bytes > 0
            && credentials.max_file_bytes > 0,
        "invalid mirror grant"
    );
    for raw in [&credentials.repository_url, &credentials.working_tree_url] {
        let url = transport::endpoint(raw)?;
        ensure!(
            !config.endpoint.starts_with("https:") || url.starts_with("https:"),
            "mirror TLS downgrade"
        );
    }
    ensure!(
        credentials.read_only == epoch.is_none()
            && !credentials.password.is_empty()
            && credentials.password.len() <= 8192
            && credentials.username.len() <= 512
            && !credentials.password.chars().any(char::is_control)
            && !credentials.username.chars().any(char::is_control),
        "invalid mirror credential scope"
    );
    Ok(credentials)
}
pub(super) fn start(state: Arc<AppState>) {
    let generation = state.pro.generation.load(Ordering::Acquire);
    let task_state = state.clone();
    let task = tokio::spawn(async move {
        let state = task_state;
        let mut last_mirror = 0;
        let mut renewed = super::now();
        loop {
            if state.stopping.load(Ordering::Relaxed)
                || generation != state.pro.generation.load(Ordering::Acquire)
            {
                return;
            }
            let Some(config) = lock(&state.pro.runtime).clone() else {
                return;
            };
            if super::now().saturating_sub(renewed) >= 3600 {
                if let Ok(response) =
                    account(&config, "/v1/delegations/renew", "POST", Some(&json!({}))).await
                {
                    if let Ok(delegation) = response.json::<super::protocol::Delegation>() {
                        if authority::install_renewal(
                            &state,
                            generation,
                            &config.delegation,
                            delegation,
                        ) {
                            renewed = super::now();
                        }
                    }
                }
            }
            // Previous-life processes that have exited release their fence.
            execution::reprobe(&state);
            let workspaces = lock(&state.workspaces).list();
            for workspace in workspaces
                .into_iter()
                .filter(|workspace| eligible(&state, workspace))
                .take(128)
            {
                if lock(&state.pro.preferences)
                    .get(&workspace.id)
                    .is_some_and(|p| p.never_mirror)
                    && !matches!(
                        lock(&state.pro.ownership).get(&workspace.id),
                        Some(Ownership::AwaitingVerification { .. })
                    )
                {
                    continue;
                }
                if let Err(error) = reconcile(&state, &config, &workspace.id).await {
                    record_error(&state, &workspace.id, &error);
                }
            }
            if super::now().saturating_sub(last_mirror) >= 120
                && lock(&state.pro.mirror_task)
                    .as_ref()
                    .is_none_or(|task| task.is_finished())
            {
                let owner = state.clone();
                let config = config.clone();
                let task = tokio::spawn(async move {
                    let _guard = owner.pro.jobs.lock().await;
                    if let Err(error) = lazy_handback(&owner, &config).await {
                        tracing::warn!(phase="locate_return", error=%error, "Could not locate returning projects");
                    }
                    let workspaces = lock(&owner.workspaces).list();
                    for workspace in workspaces
                        .into_iter()
                        .filter(|workspace| eligible(&owner, workspace))
                        .take(128)
                    {
                        if generation != owner.pro.generation.load(Ordering::Acquire) {
                            return;
                        }
                        // An unrenewed lease stops publication, never local work;
                        // the lease loop re-establishes it quietly.
                        if super::owned_epoch(&owner, &workspace.id).is_none()
                            || !execution::lease_valid(&owner, &workspace.id)
                            || lock(&owner.pro.preferences)
                                .get(&workspace.id)
                                .is_some_and(|p| p.never_mirror)
                        {
                            continue;
                        }
                        if let Err(error) = snapshot(&owner, &config, &workspace.id, false).await {
                            record_error(&owner, &workspace.id, &error);
                        }
                    }
                });
                *lock(&state.pro.mirror_task) = Some(task);
                last_mirror = super::now();
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    if let Some(old) = lock(&state.pro.task).replace(task) {
        old.abort();
    }
}
fn record_error(state: &AppState, workspace: &str, error: &anyhow::Error) {
    let message: String = error.to_string().chars().take(256).collect();
    lock(&state.pro.status)
        .entry(workspace.into())
        .or_default()
        .error = Some(message);
}
pub(super) async fn reconcile(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
) -> Result<()> {
    let generation = state.pro.generation.load(Ordering::Acquire);
    reconcile_generation(state, config, workspace, generation)
        .await
        .map(|_| ())
}
async fn reconcile_generation(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    generation: u64,
) -> Result<Option<u64>> {
    authority::config_matches(state, config, workspace)?;
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed during project transfer"
    );
    ensure!(
        super::projects::account_matches(state, workspace),
        "This project belongs to another account"
    );
    let baton: Baton = account(config, &execution::path(config, workspace, ""), "GET", None)
        .await?
        .json()?;
    ensure!(baton.workspace_id == workspace, "baton workspace mismatch");
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed during project transfer"
    );
    execution::observe(state, config, &baton)?;
    if baton.continuity.is_some() {
        super::persist(state).await?;
    }
    let operation_config = if baton.holder_id.is_none() && config.role == Role::Device {
        config.clone()
    } else {
        execution::effective(state, config, workspace)?
    };
    let holder = &config.delegation.device_id;
    let previous = lock(&state.pro.ownership).get(workspace).cloned();
    if baton.mirror_disabled {
        if config.role == Role::Worker {
            lock(&state.pro.ownership).insert(
                workspace.into(),
                Ownership::PrivacyDisabled { epoch: baton.epoch },
            );
            super::persist(state).await?;
            suspend_workspace(state, workspace).await?;
            anyhow::bail!("Mirroring is disabled for this project");
        }
        {
            let mut preferences = lock(&state.pro.preferences);
            let preference = preferences.entry(workspace.into()).or_default();
            preference.never_mirror = true;
            preference.privacy_pending = false;
        }
        super::persist(state).await?;
    }

    if baton.holder_id.as_deref().is_some_and(|id| id != holder) {
        // A worker never steals an active owner. Devices observe remote work
        // immediately; hand-back is a separate coordinated stop/release path.
        lock(&state.pro.ownership).insert(
            workspace.into(),
            Ownership::Remote {
                epoch: baton.epoch,
                holder: baton.holder_id.clone().unwrap_or_default(),
            },
        );
        super::persist(state).await?;
        stop_after_verified_owner(state, config, workspace).await?;
        return Ok(None);
    }
    // A worker may sleep through an entire device tenure without ever observing
    // its remote owner. Only hydration may reacquire its saved work. Negotiated
    // checkpoint execution retains its dedicated hydration protocol below.
    if config.role == Role::Worker
        && baton.holder_id.is_none()
        && !execution::checkpoint_mode(state, workspace)
    {
        // Snapshot recovery can call reconciliation while already holding jobs.
        // Leave that operation alone; the next poll or hydrate owns the retry.
        let Ok(_job) = state.pro.jobs.try_lock() else {
            return Ok(None);
        };
        let fenced = {
            let _configuration = state.pro.configuration.lock().await;
            ensure!(
                generation == state.pro.generation.load(Ordering::Acquire),
                "Account changed during project transfer"
            );
            let mut ownership = lock(&state.pro.ownership);
            if ownership.get(workspace) == previous.as_ref()
                && !matches!(
                    previous,
                    Some(
                        Ownership::Transferring { .. }
                            | Ownership::Hydrating { .. }
                            | Ownership::SettingUp { .. }
                    )
                )
            {
                ownership.insert(
                    workspace.into(),
                    Ownership::Hydrating { epoch: baton.epoch },
                );
                true
            } else {
                false
            }
        };
        if fenced {
            super::persist(state).await?;
            ensure!(
                generation == state.pro.generation.load(Ordering::Acquire),
                "Account changed during project transfer"
            );
            suspend_workspace(state, workspace).await?;
        }
        return Ok(None);
    }
    // A remote release means its saved work must be hydrated first. The lease
    // loop must not race hand-back and resume this machine's older journal.
    if config.role == Role::Device
        && baton.holder_id.is_none()
        && matches!(previous, Some(Ownership::Remote { .. }))
    {
        return Ok(None);
    }
    let owned = baton.holder_id.as_deref() == Some(holder);
    let transferring = matches!(
        previous,
        Some(
            Ownership::Transferring { .. }
                | Ownership::Hydrating { .. }
                | Ownership::SettingUp { .. }
        )
    );
    if transferring && !owned {
        return Ok(None);
    }

    let operation = if owned
        && baton
            .expires_at
            .as_ref()
            .is_some_and(|expiry| expiry > &baton.server_now)
    {
        "renew"
    } else {
        "acquire"
    };
    // Re-acquiring the epoch this device itself held (its own clean release,
    // or its own lease that lapsed while it kept working) continues its own
    // newer files and conversations: no checkpoint install, no fork.
    let own_epoch = config.role == Role::Device && execution::held_here(state, config, &baton);
    if operation == "acquire" && execution::checkpoint_mode(state, workspace) && !own_epoch {
        ensure!(
            baton.checkpoint.is_some(),
            "a durable project checkpoint is not available yet"
        );
        // An expired/replaced executor must install the selected canonical
        // checkpoint, even when it is the same physical worker or home device.
        let Ok(_job) = state.pro.jobs.try_lock() else {
            return Ok(None);
        };
        lock(&state.pro.ownership).insert(
            workspace.into(),
            Ownership::AwaitingVerification { epoch: baton.epoch },
        );
        super::persist(state).await?;
        // The grant's own requires_fork decides; hydrate applies it.
        Box::pin(hydrate(state, config, workspace, baton.epoch, false, None)).await?;
        return Ok(super::owned_epoch(state, workspace));
    }
    let body = execution::body(&operation_config, baton.epoch, operation == "acquire");
    if operation_config.execution.is_some() && baton.continuity.is_none() {
        lock(&state.pro.ownership).insert(
            workspace.into(),
            Ownership::AwaitingVerification { epoch: baton.epoch },
        );
        super::persist(state).await?;
        suspend_workspace(state, workspace).await?;
        execution::stop(state, &[workspace.to_owned()]).await?;
    }
    // Never take or extend a lease this worker could not accept: an expired
    // one lets the laptop (or a clean worker) continue instead.
    ensure!(
        !execution::worker(state)
            || (!state.pro.execution.invalid && !execution::unclean(state, workspace)),
        "previous managed processes are still stopping"
    );
    let was_fenced = execution::fenced(state, workspace);
    let request_start = execution::RequestStart::now();
    let response = account(
        &operation_config,
        &execution::path(&operation_config, workspace, operation),
        "POST",
        Some(&body),
    )
    .await
    .context("Could not renew project ownership")?;
    // The account's reconnect grace after a lapsed lease is a normal wait,
    // not a failure; the next tick asks again while local work continues.
    if response.status == 409
        && serde_json::from_slice::<serde_json::Value>(&response.body)
            .is_ok_and(|value| value["error"] == "takeover_grace")
    {
        return Ok(None);
    }
    let grant: Baton = response
        .json()
        .context("Could not confirm project ownership")?;
    ensure!(
        grant.workspace_id == workspace && grant.holder_id.as_deref() == Some(holder),
        "baton grant names another owner"
    );
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed while verifying project ownership"
    );
    ensure!(
        grant.workspace_id == workspace,
        "execution grant workspace mismatch"
    );
    execution::accept(state, &operation_config, &grant, generation, request_start)?;
    super::projects::bind_workspace_account(state, config, workspace)?;
    if operation_config.execution.is_some() {
        super::persist(state).await?;
    }
    if transferring {
        if let Some(Ownership::SettingUp { epoch }) = previous {
            ensure!(
                grant.epoch == epoch,
                "Project ownership changed; retry the handoff"
            );
        }
        return Ok(Some(grant.epoch));
    }
    if config.role == Role::Worker
        && (!matches!(previous, Some(Ownership::Local { .. }))
            || !super::provider_gate::required(state, workspace).is_empty())
    {
        ensure!(
            generation == state.pro.generation.load(Ordering::Acquire),
            "Account changed while verifying project ownership"
        );
        lock(&state.pro.ownership).insert(
            workspace.into(),
            Ownership::SettingUp { epoch: grant.epoch },
        );
        finish_hydration(state, workspace, grant.epoch, generation, async { Ok(()) }).await?;
        return Ok(Some(grant.epoch));
    }
    if baton.mirror_disabled
        && !matches!(
            previous,
            Some(Ownership::Remote { .. } | Ownership::Hydrating { .. })
        )
    {
        lock(&state.pro.ownership).remove(workspace);
    } else {
        lock(&state.pro.ownership)
            .insert(workspace.into(), Ownership::Local { epoch: grant.epoch });
    }
    super::persist(state).await?;
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed during project transfer"
    );
    // A renewal after a same-epoch fence resumes what the fence preserved.
    if (!matches!(previous, Some(Ownership::Local { .. })) || was_fenced)
        && execution::resume_allowed(state, workspace)
    {
        crate::ledger::resume_deferred_workspace(state, workspace).await?;
    }
    Ok(Some(grant.epoch))
}
/// Upper bound on how long a device's agent may finish its turn after another
/// owner was verified. Its input is already refused (`may_write`).
const VERIFIED_OWNER_PAUSE_WAIT: u64 = 300;

/// A verified other owner fences input at once. A device's agents then stop at
/// their next safe pause (or after a bounded wait) and are preserved for the
/// return; a worker stops immediately. Plain shells are never stopped.
async fn stop_after_verified_owner(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
) -> Result<()> {
    let live = sessions(state, workspace).into_iter().any(|id| {
        state.chat.get(&id).is_some_and(|s| s.alive)
            || state.sessions.get(&id).is_some_and(|s| s.alive)
    });
    if !live {
        lock(&state.pro.remote_since).remove(workspace);
        return Ok(());
    }
    if config.role == Role::Device && !at_pause(state, workspace) {
        let since = {
            let mut waiting = lock(&state.pro.remote_since);
            if waiting.len() >= 128 && !waiting.contains_key(workspace) {
                waiting.clear();
            }
            *waiting.entry(workspace.into()).or_insert_with(super::now)
        };
        if super::now().saturating_sub(since) < VERIFIED_OWNER_PAUSE_WAIT {
            return Ok(());
        }
    }
    lock(&state.pro.remote_since).remove(workspace);
    suspend_workspace(state, workspace).await
}

async fn suspend_workspace(state: &Arc<AppState>, workspace: &str) -> Result<()> {
    for id in sessions(state, workspace) {
        match crate::bundle::export(state.clone(), &id, crate::bundle::ExportMode::Stop).await {
            Ok(archive) => {
                let _ = tokio::fs::remove_file(archive).await;
            }
            Err(_) => {
                // Losing a verified lease must stop an agent even before its
                // first native conversation id exists. Preserve its ledger
                // identity; absence of an exportable handle cannot authorize a
                // second writer to continue running.
                if let Some(mut entry) = crate::ledger::snapshot(state)
                    .0
                    .into_iter()
                    .find(|entry| entry.id == id)
                {
                    entry.suspended = true;
                    entry.handoff = None;
                    lock(&state.deferred_sessions).insert(id.clone(), entry);
                }
                if state.chat.get(&id).is_some() {
                    state.chat.kill(&id);
                } else {
                    let _ = state.sessions.kill(&id);
                }
            }
        }
    }
    let owner = state.clone();
    tokio::task::spawn_blocking(move || {
        let (entries, links) = crate::ledger::snapshot(&owner);
        lock(&owner.ledger).write_checked(&entries, &links)
    })
    .await??;
    Ok(())
}

/// Running agents make a project worth handing to the cloud before sleep.
pub(super) fn live_agents(state: &AppState, workspace: &str) -> bool {
    sessions(state, workspace).into_iter().any(|id| {
        state.chat.get(&id).is_some_and(|s| s.alive)
            || state.sessions.get(&id).is_some_and(|s| s.alive)
    })
}

fn sessions(state: &AppState, workspace: &str) -> Vec<String> {
    let ids: Vec<_> = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, id)| id.as_str() == workspace)
        .map(|(id, _)| id.clone())
        .take(64)
        .collect();
    let agents = lock(&state.agents);
    ids.into_iter()
        .filter(|id| agents.contains_key(id) || state.chat.get(id).is_some())
        .collect()
}
/// A clean flush before this computer sleeps: one shared deadline, and no
/// release once the computer woke again (the project simply stays here).
#[derive(Clone, Copy)]
pub(super) struct Sleep {
    pub generation: u64,
    pub deadline: tokio::time::Instant,
}
impl Sleep {
    pub fn woke(&self, state: &AppState) -> bool {
        state.pro.sleep_generation.load(Ordering::Acquire) != self.generation
    }
}
pub(super) fn failure_code(error: &anyhow::Error) -> &'static str {
    snapshot_diagnostics::category(error)
}
pub(super) async fn snapshot(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    clean: bool,
) -> Result<()> {
    snapshot_before(state, config, workspace, clean, None).await
}
pub(super) async fn sleep_flush(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    sleep: Sleep,
) -> Result<()> {
    let result = snapshot_before(state, config, workspace, true, Some(sleep)).await;
    // Woken during the flush: its publication stands, the project stays here.
    if sleep.woke(state) {
        let _configuration = state.pro.configuration.lock().await;
        let mut ownership = lock(&state.pro.ownership);
        if let Some(Ownership::Transferring { epoch }) = ownership.get(workspace).cloned() {
            ownership.insert(workspace.into(), Ownership::AwaitingVerification { epoch });
        }
    }
    result
}
async fn snapshot_before(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    clean: bool,
    sleep: Option<Sleep>,
) -> Result<()> {
    let mut phase = "ownership";
    let result = snapshot_inner(state, config, workspace, clean, sleep, &mut phase).await;
    if let Err(error) = &result {
        // Recovery may clear the transient status while resuming an idle session.
        // The fixed phase/category survives that recovery without recording data.
        tracing::warn!(
            phase,
            category = snapshot_diagnostics::category(error),
            clean,
            "Project snapshot failed"
        );
    }
    result
}

async fn snapshot_inner(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    clean: bool,
    sleep: Option<Sleep>,
    phase: &mut &'static str,
) -> Result<()> {
    let generation = state.pro.generation.load(Ordering::Acquire);
    let cache = Arc::new(state.pro.cache(workspace)?.lock_owned().await);
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed while waiting for project cache"
    );
    transport::cache_quiescent(workspace)?;
    transport::cache_scope(
        workspace,
        cache,
        snapshot_inner_scoped(state, config, workspace, clean, sleep, phase),
    )
    .await
}

async fn snapshot_inner_scoped(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    clean: bool,
    sleep: Option<Sleep>,
    phase: &mut &'static str,
) -> Result<()> {
    authority::config_matches(state, config, workspace)?;
    let effective = execution::effective(state, config, workspace)?;
    let config = &effective;
    ensure!(
        config.recovery || execution::lease_valid(state, workspace),
        "execution authority expired before publication"
    );
    if config.recovery {
        ensure!(
            execution::quiescent(state, workspace)
                && lock(&state.pro.preferences)
                    .get(workspace)
                    .is_some_and(|p| p.recovery_pending),
            "recovery execution has not stopped"
        );
    }
    let generation = state.pro.generation.load(Ordering::Acquire);
    let epoch = super::owned_epoch(state, workspace).context("workspace is not locally owned")?;
    let workspace = lock(&state.workspaces)
        .get(workspace)
        .context("unknown workspace")?;
    *phase = "destination";
    authority::destination(state, config, &workspace.id, Some(&workspace.root)).await?;
    *phase = "credentials";
    let grant = credentials(config, &workspace.id, Some(epoch)).await?;
    let root = state.pro.root.join(&workspace.id);
    let shadow = root.join("working-tree.git");
    *phase = "initialize";
    let interrupted = root.clone();
    tokio::task::spawn_blocking(move || mirror::clear_interrupted(&interrupted)).await??;
    mirror::initialize(&shadow).await?;
    if mirror::set_aside_damaged(&shadow).await? {
        tracing::warn!("Rebuilding a damaged outgoing project mirror from its published copy");
        mirror::initialize(&shadow).await?;
        mirror::fetch_published(&shadow, &grant).await?;
    }
    let staging = root.join(format!("stage-{}", chimaera_core::generate_token()));
    tokio::fs::create_dir_all(&staging).await?;
    let result = async {
        let agent_ids=sessions(state,&workspace.id);
        let continuation=continuation(state,&workspace.id);
        let mut has_agents=false;
        let session_ids:Vec<_>=lock(&state.session_workspaces).iter().filter(|(_,workspace_id)|*workspace_id==&workspace.id).map(|(id,_)|id.clone()).take(64).collect();
        let mut stopped=std::collections::HashMap::new();
        if clean {
            *phase = "stop_sessions";
            lock(&state.pro.ownership).insert(workspace.id.clone(),Ownership::Transferring{epoch});super::persist(state).await?;
            for id in &session_ids {
                let Some(path)=crate::bundle::export_for_mirror(state.clone(),id,crate::bundle::ExportMode::Stop).await? else {continue;};
                let target=staging.join(format!("stopped-{id}.zip"));tokio::fs::rename(path,&target).await?;stopped.insert(id.clone(),target);
            }
        }
        *phase = "stop_execution";
        if clean && config.execution.is_some(){execution::stop(state,std::slice::from_ref(&workspace.id)).await?;}
        *phase = "inventory";
        let paths = mirror::inventory(&workspace.root, &shadow).await?;
        let project = workspace.root.clone(); let destination = staging.join("tree");
        let budget = grant.storage_limit_bytes; let max_file = grant.max_file_bytes;
        *phase = "copy_files";
        let report = tokio::task::spawn_blocking(move || mirror::copy_tree(&project, &destination, paths, budget, max_file)).await??;
        *phase = "export_config";
        let home = state.claude_settings_path.parent().and_then(Path::parent).context("agent home unavailable")?.to_path_buf();
        let sources = config::Sources { home, claude:state.claude_settings_path.parent().unwrap().to_path_buf(), codex:state.codex_config_path.parent().context("codex home unavailable")?.to_path_buf(), workspace:workspace.root.clone() };
        let destination = staging.join("config");
        let config_report = tokio::task::spawn_blocking(move || config::export(sources, &destination, budget.saturating_sub(report.bytes))).await??;
        let mut profile = lock(&state.pro.preferences).entry(workspace.id.clone()).or_default().profile.clone();
        profile.missing_environment = config_report.missing_environment;
        let command_sessions:Vec<_>=lock(&state.session_workspaces).iter().filter(|(_,id)|*id==&workspace.id).map(|(id,_)|id.clone()).take(64).collect();
        for id in command_sessions {if let Some(marks)=state.sessions.marks(&id) {for command in marks.journal(32) {if let Some(command)=command.command.as_deref(){profile.observe_command(command);}}}}
        *phase = "archive_sessions";
        let handoff = staging.join("handoff"); tokio::fs::create_dir_all(handoff.join("bundles")).await?;
        let mut archives = Vec::new(); let mut archive_bytes = 0u64;
        for id in session_ids {
            let Some(path) = (if clean {stopped.remove(&id)} else {crate::bundle::export_for_mirror(state.clone(), &id, crate::bundle::ExportMode::Snapshot).await?}) else {continue;};
            has_agents |= agent_ids.contains(&id);
            let length = tokio::fs::metadata(&path).await?.len();
            if length > max_file { let _ = tokio::fs::remove_file(path).await; anyhow::bail!("session archive exceeds mirror file limit"); }
            archive_bytes = archive_bytes.saturating_add(length);
            ensure!(archive_bytes + report.bytes + config_report.bytes <= budget, "workspace and conversations exceed mirror storage quota");
            let archive = format!("bundles/{id}.zip");
            tokio::fs::rename(path, handoff.join(&archive)).await?;
            archives.push(SessionArchive {id,archive});
        }
        // Automatic continuation requires this per-epoch eligibility on the
        // account in both protocol versions: its offline wake and the worker's
        // discovery read only these flags. Publish it while this epoch is
        // still owned and before any snapshot bytes leave, so a refusal cannot
        // strand a published checkpoint that nothing will ever continue.
        if !config.recovery {
            *phase = "update_policy";
            publish_policy(config, &workspace.id, epoch, has_agents).await?;
        }
        *phase = "capture_repository";
        let branch=transport::git_output(transport::git(&workspace.root,None).await?,&["symbolic-ref","-q","HEAD"],vec![]).await.ok().and_then(|bytes|String::from_utf8(bytes).ok()).map(|text|text.trim().to_string());
        let repository_origin=mirror::repository_origin(&workspace.root).await;
        let repository=super::repository::capture(&workspace.root).await?;
        let manifest = Manifest {version:1,branch,repository_origin,repository,workspace_id:workspace.id.clone(),root:workspace.root.clone(),name:workspace.name.clone(),epoch,clean,continuation,profile:profile.clone(),sessions:archives};
        tokio::fs::write(handoff.join("manifest.json"), serde_json::to_vec(&manifest)?).await?;
        *phase = "mirror_repository";
        mirror::mirror_repository(&workspace.root, &root.join("repository.git"), &grant).await?;
        *phase = "commit_files";
        let tree_oid=mirror::commit_tree(&shadow, &staging.join("tree"), "main").await?;
        *phase = "commit_config";
        let config_oid=mirror::commit_tree(&shadow, &staging.join("config"), "config").await?;
        *phase = "commit_sessions";
        let handoff_oid=mirror::commit_tree(&shadow, &handoff, "handoff").await?;
        *phase = "publish_snapshot";
        mirror::push(&shadow, &grant, &["refs/heads/main", "refs/heads/config", "refs/heads/handoff"]).await?;
        *phase = "confirm_checkpoint";
        if config.execution.is_some(){execution::receipt::published(config,&workspace.id,epoch,[&tree_oid,&config_oid,&handoff_oid],continuation).await?;}
        lock(&state.pro.preferences).entry(workspace.id.clone()).or_default().profile = profile;
        lock(&state.pro.status).insert(workspace.id.clone(), WorkspaceStatus {report,last_mirrored_at:Some(super::now()),storage_limit_bytes:budget,error:None,blocked_providers:Vec::new()});
        *phase = "persist_snapshot";
        super::persist(state).await?;
        if clean && !sleep.is_some_and(|sleep| sleep.woke(state)) {
            *phase = "release";
            // Before sleep, the account's short publication fence is not
            // waited out past the deadline: an unreleased lease simply lapses
            // and the cloud continues from this acknowledged checkpoint.
            let budget = sleep.map_or(Duration::from_secs(15), |sleep| {
                sleep.deadline.saturating_duration_since(tokio::time::Instant::now())
            });
            release::after_publication(config, &workspace.id, epoch, budget, || {
                generation == state.pro.generation.load(Ordering::Acquire)
                    && !sleep.is_some_and(|sleep| sleep.woke(state))
                    && matches!(lock(&state.pro.ownership).get(&workspace.id), Some(Ownership::Transferring { epoch: current }) if *current == epoch)
            }).await?;
        }
        Ok::<_,anyhow::Error>(())
    }.await;
    let _ = tokio::fs::remove_dir_all(staging).await;
    if result.is_err() && clean && !config.recovery {
        // A failed flush must not strand a stopped laptop agent, but a changed
        // account must never recover using the previous account's credentials.
        let recover = {
            let _configuration = state.pro.configuration.lock().await;
            let mut ownership = lock(&state.pro.ownership);
            if generation == state.pro.generation.load(Ordering::Acquire)
                && matches!(ownership.get(&workspace.id), Some(Ownership::Transferring { epoch: current }) if *current == epoch)
            {
                ownership.insert(
                    workspace.id.clone(),
                    Ownership::AwaitingVerification { epoch },
                );
                true
            } else {
                false
            }
        };
        if recover {
            let _ = reconcile_generation(state, config, &workspace.id, generation).await;
        }
    }
    result
}

/// The account's policy route is the same `/v1` resource for both protocol
/// versions; it checks the exact live holder and epoch itself.
async fn publish_policy(
    config: &Configure,
    workspace: &str,
    epoch: u64,
    has_agents: bool,
) -> Result<()> {
    let continuation = !config.hours_exhausted;
    let policy = account(
        config,
        &format!("/v1/baton/{workspace}/policy"),
        "PUT",
        Some(&json!({
            "holder_id": config.delegation.device_id,
            "epoch": epoch,
            "handoff_enabled": continuation,
            "offline_takeover": continuation,
            "has_agents": has_agents,
        })),
    )
    .await?;
    ensure!(
        (200..300).contains(&policy.status),
        "mirror policy update failed"
    );
    Ok(())
}

pub(super) async fn fetch_snapshot(
    config: &Configure,
    workspace: &str,
    cache: &Path,
) -> Result<Manifest> {
    authority::config_workspace(config, workspace)?;
    let mut effective = config.clone();
    let receipt = if config.execution.is_some() {
        let baton: Baton = account(config, &execution::path(config, workspace, ""), "GET", None)
            .await?
            .json()?;
        ensure!(
            baton.workspace_id == workspace,
            "checkpoint workspace mismatch"
        );
        if baton.continuity.is_some() {
            Some(baton.checkpoint.context("durable checkpoint required")?)
        } else {
            effective.execution = None;
            None
        }
    } else {
        None
    };
    fetch_snapshot_at(&effective, workspace, cache, receipt.as_ref()).await
}
async fn fetch_snapshot_at(
    config: &Configure,
    workspace: &str,
    cache: &Path,
    receipt: Option<&execution::wire::Checkpoint>,
) -> Result<Manifest> {
    if let Some(receipt) = receipt {
        execution::receipt::validate(receipt)?;
    }
    let grant = credentials(config, workspace, None).await?;
    mirror::initialize(cache).await?;
    let url = transport::endpoint(&grant.working_tree_url)?;
    transport::git_output(
        transport::git(cache, Some((&grant.username, &grant.password))).await?,
        &["fetch", "--no-tags", &url, "+refs/heads/*:refs/heads/*"],
        vec![],
    )
    .await?;
    if let Some(receipt) = receipt {
        transport::git_output(
            transport::git(cache, Some((&grant.username, &grant.password))).await?,
            &[
                "fetch",
                "--no-tags",
                &url,
                &format!(
                    "+refs/chimaera/checkpoints/{}/*:refs/chimaera/checkpoints/{}/*",
                    receipt.id, receipt.id
                ),
            ],
            vec![],
        )
        .await?;
        execution::receipt::pin(cache, receipt).await?;
    }
    let bytes = transport::git_output(
        transport::git(cache, None).await?,
        &[
            "show",
            &format!(
                "{}:manifest.json",
                execution::receipt::revision(receipt, "handoff")?
            ),
        ],
        vec![],
    )
    .await?;
    ensure!(bytes.len() <= 256 * 1024, "handoff manifest exceeds limit");
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest.version == 1
            && manifest.workspace_id == workspace
            && manifest.sessions.len() <= 64
            && manifest.root.is_absolute(),
        "invalid handoff manifest"
    );
    if let Some(receipt) = receipt {
        ensure!(
            manifest.epoch == receipt.source_epoch && manifest.continuation == receipt.continuation,
            "handoff receipt does not match manifest"
        );
    }
    for entry in &manifest.sessions {
        ensure!(
            super::valid_id(&entry.id) && entry.archive == format!("bundles/{}.zip", entry.id),
            "invalid handoff archive path"
        );
    }
    Ok(manifest)
}

pub(super) async fn hydrate(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    expected_epoch: u64,
    fork: bool,
    destination_root: Option<&Path>,
) -> Result<()> {
    let generation = state.pro.generation.load(Ordering::Acquire);
    let bound_destination =
        authority::destination(state, config, workspace, destination_root).await?;
    let destination_root = bound_destination.as_deref();
    let cache = Arc::new(state.pro.cache(workspace)?.lock_owned().await);
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed while waiting for project cache"
    );
    transport::cache_quiescent(workspace)?;
    transport::cache_scope(
        workspace,
        cache.clone(),
        hydrate_scoped(
            state,
            config,
            workspace,
            expected_epoch,
            fork,
            destination_root,
            cache,
        ),
    )
    .await
}

async fn hydrate_scoped(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    expected_epoch: u64,
    fork: bool,
    destination_root: Option<&Path>,
    mut cache_guard: Arc<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<()> {
    let generation = state.pro.generation.load(Ordering::Acquire);
    let install_epoch = std::sync::atomic::AtomicU64::new(0);
    let current = || -> Result<()> {
        let epoch = install_epoch.load(Ordering::Acquire);
        ensure!(
            epoch == 0 || execution::valid_grant(state, workspace, epoch),
            "execution authority expired during project transfer"
        );
        ensure!(
            generation == state.pro.generation.load(Ordering::Acquire),
            "Account changed during project transfer; open the project again"
        );
        Ok(())
    };
    if lock(&state.workspaces).get(workspace).is_some()
        && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::SettingUp{epoch}) if *epoch==expected_epoch)
    {
        reconcile(state, config, workspace).await?;
        return finish_hydration(
            state,
            workspace,
            expected_epoch,
            generation,
            run_profile_steps(state, config, workspace),
        )
        .await;
    }
    // Existing durable worker work must never be replaced with an older remote
    // snapshot after a restart. The normal grant path resumes its own ledger.
    if lock(&state.workspaces).get(workspace).is_some()
        && config.role == Role::Worker
        && !execution::managed(state, workspace)
        && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::Local{epoch}|Ownership::AwaitingVerification{epoch}) if *epoch==expected_epoch)
    {
        let baton: Baton = account(config, &execution::path(config, workspace, ""), "GET", None)
            .await?
            .json()?;
        ensure!(baton.workspace_id == workspace, "baton workspace mismatch");
        current()?;
        if baton.holder_id.as_deref() == Some(&config.delegation.device_id)
            && baton.epoch == expected_epoch
        {
            let verified = reconcile_generation(state, config, workspace, generation).await?;
            if verified == Some(expected_epoch)
                && super::owned_epoch(state, workspace) == Some(expected_epoch)
            {
                return Ok(());
            }
        }
    }
    if execution::managed(state, workspace) {
        // No canonical files are installed while an old local managed executor
        // can still write them. An unclean same-boot registry remains blocked.
        execution::fence_workspace(state, workspace);
        execution::stop(state, &[workspace.to_owned()]).await?;
    }
    let interrupted = state.pro.root.join(workspace);
    tokio::task::spawn_blocking(move || mirror::clear_interrupted(&interrupted)).await??;
    let cache = state.pro.root.join(workspace).join("incoming.git");
    let manifest = fetch_snapshot(config, workspace, &cache).await?;
    let destination_root = destination_root
        .map(Path::to_path_buf)
        .or_else(|| {
            lock(&state.workspaces)
                .get(workspace)
                .map(|workspace| workspace.root)
        })
        .unwrap_or_else(|| manifest.root.clone());
    ensure!(
        destination_root.is_absolute(),
        "destination root must be absolute"
    );
    ensure!(
        tokio::fs::metadata(&destination_root)
            .await
            .is_ok_and(|metadata| metadata.is_dir()),
        "root_setup_required"
    );
    let probe = destination_root.join(format!(
        ".chimaera-write-probe-{}",
        chimaera_core::generate_token()
    ));
    tokio::fs::write(&probe, b"")
        .await
        .context("root_setup_required")?;
    tokio::fs::remove_file(&probe).await?;
    current()?;
    let existing: Baton = account(config, &execution::path(config, workspace, ""), "GET", None)
        .await?
        .json()?;
    execution::observe(state, config, &existing)?;
    let effective = execution::effective(state, config, workspace)?;
    let config = &effective;
    let request_start = execution::RequestStart::now();
    let grant: Baton = if existing.holder_id.as_deref() == Some(&config.delegation.device_id)
        && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::Hydrating{epoch}) if *epoch==existing.epoch)
    {
        account(
            config,
            &execution::path(config, workspace, "renew"),
            "POST",
            Some(&execution::body(config, existing.epoch, false)),
        )
        .await?
        .json()?
    } else {
        account(
            config,
            &execution::path(config, workspace, "acquire"),
            "POST",
            Some(&execution::body(config, expected_epoch, true)),
        )
        .await?
        .json()?
    };
    ensure!(
        grant.workspace_id == workspace
            && grant.holder_id.as_deref() == Some(&config.delegation.device_id),
        "invalid handoff ownership grant"
    );
    current()?;
    execution::accept(state, config, &grant, generation, request_start)?;
    install_epoch.store(grant.epoch, Ordering::Release);
    let receipt = if config.execution.is_some() {
        let receipt = grant
            .checkpoint
            .as_ref()
            .context("acquired grant has no durable checkpoint")?;
        ensure!(
            receipt.source_epoch <= grant.epoch,
            "checkpoint is newer than its execution grant"
        );
        Some(receipt)
    } else {
        None
    };
    lock(&state.pro.ownership).insert(
        workspace.into(),
        Ownership::Hydrating { epoch: grant.epoch },
    );
    super::persist(state).await?;
    let manifest = if let Some(receipt) = receipt {
        fetch_snapshot_at(config, workspace, &cache, Some(receipt)).await?
    } else {
        manifest
    };
    let stage = state
        .pro
        .root
        .join(workspace)
        .join(format!("hydrate-{}", chimaera_core::generate_token()));
    let result = async {
        let read_grant = credentials(config, workspace, None).await?;
        for (branch, folder) in [
            ("main", "tree"),
            ("config", "config"),
            ("handoff", "handoff"),
        ] {
            mirror::validate_tree(
                &cache,
                execution::receipt::revision(receipt, branch)?,
                read_grant.storage_limit_bytes,
                read_grant.max_file_bytes,
            )
            .await?;
            let destination = stage.join(folder);
            tokio::fs::create_dir_all(&destination).await?;
            let mut command = transport::git(&cache, None).await?;
            command.env("GIT_WORK_TREE", &destination);
            transport::git_output(
                command,
                &[
                    "--work-tree",
                    destination.to_str().context("invalid stage path")?,
                    "checkout",
                    execution::receipt::revision(receipt, branch)?,
                    "--",
                    ".",
                ],
                vec![],
            )
            .await?;
        }
        current()?;
        authority::destination(state, config, workspace, Some(&destination_root)).await?;
        super::projects::begin_install(state, workspace, &destination_root).await?;
        current()?;
        let git_branches = super::repository::receive(
            &destination_root,
            &state
                .pro
                .root
                .join(workspace)
                .join("incoming-repository.git"),
            &read_grant,
            manifest.branch.as_deref(),
            manifest.repository_origin.as_deref(),
            manifest.repository.as_ref(),
            &current,
        )
        .await?;
        let baseline = stage.join("baseline");
        let local_shadow = state.pro.root.join(workspace).join("working-tree.git");
        let old_shadow = super::shadow_cache::baseline(&local_shadow).await?;
        let has_baseline = old_shadow.is_some();
        if let Some(old_shadow) = &old_shadow {
            tokio::fs::create_dir_all(&baseline).await?;
            let mut command = transport::git(old_shadow, None).await?;
            command.env("GIT_WORK_TREE", &baseline);
            transport::git_output(
                command,
                &[
                    "--work-tree",
                    baseline.to_str().context("invalid baseline path")?,
                    "checkout",
                    "refs/heads/main",
                    "--",
                    ".",
                ],
                vec![],
            )
            .await?;
        }
        current()?;
        authority::destination(state, config, workspace, Some(&destination_root)).await?;
        let tree = stage.join("tree");
        let destination = destination_root.clone();
        let local_conflicts = (config.role == Role::Device
            && execution::checkpoint_mode(state, workspace))
        .then(|| state.pro.root.join(workspace).join("local-conflicts"));
        tokio::task::spawn_blocking(move || {
            install_tree(
                &tree,
                &destination,
                has_baseline.then_some(baseline).as_deref(),
                local_conflicts.as_deref(),
            )
        })
        .await??;
        if let Some(repair) = super::shadow_cache::prepare(&local_shadow, &cache, cache_guard.clone()).await? {
            let configuration = state.pro.configuration.clone().lock_owned().await;
            let owner = state.clone();
            let workspace = workspace.to_owned();
            let epoch = grant.epoch;
            cache_guard = super::shadow_cache::install(repair, cache_guard, configuration, move || {
                transport::cache_quiescent(&workspace)?;
                ensure!(generation == owner.pro.generation.load(Ordering::Acquire), "Account changed during shadow recovery");
                ensure!(execution::valid_grant(&owner, &workspace, epoch), "Execution authority expired during shadow recovery");
                ensure!(matches!(lock(&owner.pro.ownership).get(&workspace), Some(Ownership::Hydrating { epoch: current }) if *current == epoch), "Workspace ownership changed during shadow recovery");
                Ok(())
            }).await?;
        }
        current()?;
        ensure!(matches!(lock(&state.pro.ownership).get(workspace), Some(Ownership::Hydrating { epoch }) if *epoch == grant.epoch), "Workspace ownership changed during hydration");
        let overlay = stage.join("config");
        let home = state
            .claude_settings_path
            .parent()
            .and_then(Path::parent)
            .context("agent home unavailable")?
            .to_path_buf();
        current()?;
        let account_state = state.clone();
        let config_workspace = destination_root.clone();
        tokio::task::spawn_blocking(move || {
            ensure!(
                generation == account_state.pro.generation.load(Ordering::Acquire),
                "Account changed during project transfer"
            );
            config::import(&overlay, &home, &config_workspace)
        })
        .await??;
        current()?;
        let new_workspace = crate::workspaces::Workspace {
            id: workspace.into(),
            root: destination_root.clone(),
            name: manifest.name,
            last_opened_at: super::now(),
            mastermind: None,
            plugins_on: Vec::new(),
            cloud_internal: false,
        };
        let owner = state.clone();
        tokio::task::spawn_blocking(move || lock(&owner.workspaces).import_exact(new_workspace))
            .await??;
        lock(&state.pro.preferences)
            .entry(workspace.into())
            .or_default()
            .execution_uncertain = grant.requires_fork
            || receipt.is_some_and(|receipt| {
                receipt.continuation == execution::wire::Continuation::Uncertain
            });
        for archive in manifest.sessions {
            current()?;
            crate::bundle::import(
                state.clone(),
                &stage.join("handoff").join(archive.archive),
                crate::bundle::ImportOptions {
                    defer_start: true,
                    destination_root: Some(destination_root.clone()),
                    fork: fork || grant.requires_fork,
                    origin: if config.role == Role::Worker {
                        crate::bundle::Origin::Moved
                    } else {
                        crate::bundle::Origin::Home
                    },
                    epoch: grant.epoch,
                },
            )
            .await?;
        }
        {
            let mut preferences = lock(&state.pro.preferences);
            let preference = preferences.entry(workspace.into()).or_default();
            preference.profile = manifest.profile;
            preference.git_branches = git_branches;
        }
        current()?;
        finish_hydration(
            state,
            workspace,
            grant.epoch,
            generation,
            run_profile_steps(state, config, workspace),
        )
        .await?;
        drop(cache_guard);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(stage).await;
    result
}
fn install_tree(
    source: &Path,
    destination: &Path,
    baseline: Option<&Path>,
    local_conflicts: Option<&Path>,
) -> Result<()> {
    let mut conflicts = local_conflicts
        .map(super::canonical::Conflicts::open)
        .transpose()?;
    if let Some(baseline) = baseline {
        let mut pending = vec![(baseline.to_path_buf(), PathBuf::new())];
        let mut count = 0;
        while let Some((directory, relative)) = pending.pop() {
            for entry in std::fs::read_dir(directory)? {
                count += 1;
                ensure!(
                    count <= super::policy::MAX_PATHS,
                    "baseline tree exceeds limit"
                );
                let entry = entry?;
                let relative = relative.join(entry.file_name());
                ensure!(
                    super::policy::allowed_path(&relative),
                    "unsafe baseline path"
                );
                let kind = entry.file_type()?;
                ensure!(!kind.is_symlink(), "baseline tree contains symlink");
                if kind.is_dir() {
                    pending.push((entry.path(), relative));
                    continue;
                }
                if !kind.is_file() || source.join(&relative).try_exists()? {
                    continue;
                }
                let target = destination.join(&relative);
                let mut cursor = destination.to_path_buf();
                let safe = relative.components().all(|component| {
                    cursor.push(component);
                    !std::fs::symlink_metadata(&cursor)
                        .is_ok_and(|metadata| metadata.file_type().is_symlink())
                });
                // A deletion is safe only when the local file still equals
                // the shared baseline. Local edits and symlinks always win.
                if safe && target.try_exists()? {
                    if same_file(&target, &entry.path())? {
                        std::fs::remove_file(target)?;
                    } else if let Some(conflicts) = conflicts.as_mut() {
                        conflicts.preserve(&target, &relative)?;
                        std::fs::remove_file(target)?;
                    }
                }
            }
        }
    }
    let mut pending = vec![(source.to_path_buf(), PathBuf::new())];
    let mut count = 0;
    while let Some((directory, relative)) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            count += 1;
            ensure!(
                count <= super::policy::MAX_PATHS,
                "hydrated tree exceeds limit"
            );
            let entry = entry?;
            let relative = relative.join(entry.file_name());
            ensure!(
                super::policy::allowed_path(&relative),
                "unsafe hydrated path"
            );
            let kind = entry.file_type()?;
            ensure!(!kind.is_symlink(), "hydrated tree contains symlink");
            let target = destination.join(&relative);
            ensure!(
                !std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()),
                "destination contains symlink"
            );
            if kind.is_dir() {
                std::fs::create_dir_all(&target)?;
                pending.push((entry.path(), relative));
            } else if kind.is_file() {
                if target.exists()
                    && !same_file(&target, &entry.path())?
                    && !baseline.is_some_and(|root| {
                        same_file(&target, &root.join(&relative)).unwrap_or(false)
                    })
                {
                    if let Some(conflicts) = conflicts.as_mut() {
                        conflicts.preserve(&target, &relative)?;
                        std::fs::copy(entry.path(), target)?;
                    } else {
                        let preserved = target.with_file_name(format!(
                            "{}.cloud-{}",
                            entry.file_name().to_string_lossy(),
                            super::now()
                        ));
                        std::fs::copy(entry.path(), preserved)?;
                    }
                } else {
                    std::fs::copy(entry.path(), target)?;
                }
            }
        }
    }
    Ok(())
}

fn chat_at_pause(
    chat: &chimaera_agent::ChatInfo,
    carry: Option<&chimaera_agent::Carryover>,
    queued_input: bool,
    agent_state: Option<crate::agent_state::AgentState>,
) -> bool {
    let Some(carry) = carry else {
        return false;
    };
    chat.background_running == 0
        && carry.background.is_empty()
        && !queued_input
        && (chat.pending_permission
            || chat.status_needs_action
            || (!carry.turn_in_flight
                && (chat.status_category.as_deref() == Some("idle")
                    || matches!(
                        agent_state,
                        Some(
                            crate::agent_state::AgentState::Finished
                                | crate::agent_state::AgentState::IdlePrompt
                        )
                    ))))
}

pub(super) fn at_pause(state: &AppState, workspace: &str) -> bool {
    sessions(state, workspace).into_iter().all(|id| {
        if let Some(chat) = state.chat.get(&id) {
            let activity = state.chat.input_activity(&id);
            let agent_state = lock(&state.agents).get(&id).map(|agent| agent.state);
            chat_at_pause(
                &chat,
                activity.as_ref().map(|(carry, _)| carry),
                activity.as_ref().is_none_or(|(_, pending)| *pending),
                agent_state,
            )
        } else {
            lock(&state.agents).get(&id).is_none_or(|agent| {
                matches!(
                    agent.state,
                    crate::agent_state::AgentState::IdlePrompt
                        | crate::agent_state::AgentState::Finished
                        | crate::agent_state::AgentState::NeedsPermission
                        | crate::agent_state::AgentState::Errored
                )
            })
        }
    })
}
/// A device waits this long between automatic attempts to finish one return.
const RETURN_BACKOFF_MAX: u64 = 1800;

pub(super) async fn lazy_handback(state: &Arc<AppState>, config: &Configure) -> Result<()> {
    if config.delegation.workspace.is_some() || config.role != Role::Device {
        return Ok(());
    }
    // Moving live cloud work home waits until this computer has been awake
    // and on power for a while (both protocol versions); work the cloud is
    // not running returns at once (laptop first).
    let settled = state.pro.power_suitable.load(Ordering::Acquire)
        && super::now().saturating_sub(state.pro.awake_since.load(Ordering::Acquire)) >= 300;
    let candidates: Vec<_> = lock(&state.pro.ownership)
        .iter()
        .filter_map(|(id, owner)| match owner {
            Ownership::Remote { epoch, holder } => Some((id.clone(), *epoch, Some(holder.clone()))),
            Ownership::Hydrating { epoch } => Some((id.clone(), *epoch, None)),
            _ => None,
        })
        .take(128)
        .collect();
    let mut hosts: Option<Vec<super::protocol::Host>> = None;
    for (workspace, epoch, holder) in candidates {
        // Only a project already registered on this device may return automatically.
        // Discovery and old global-folder preferences never authorize adoption.
        if lock(&state.workspaces).get(&workspace).is_none()
            || super::projects::adoption_pending(state, &workspace)
            || !super::projects::account_matches(state, &workspace)
        {
            continue;
        }
        if !execution::preferred_here(state, config, &workspace)
            || lock(&state.pro.preferences)
                .get(&workspace)
                .is_some_and(|p| p.never_mirror)
        {
            continue;
        }
        if lock(&state.pro.return_backoff)
            .get(&workspace)
            .is_some_and(|(next, _)| *next > super::now())
        {
            continue;
        }
        let result = async {
            let operation_config = execution::effective(state, config, &workspace)?;
            let baton: Baton = account(
                &operation_config,
                &execution::path(&operation_config, &workspace, ""),
                "GET",
                None,
            )
            .await
            .context("Could not check where your work is running")?
            .json()
            .context("Could not confirm where your work is running")?;
            ensure!(baton.workspace_id == workspace, "baton workspace mismatch");
            execution::observe(state, config, &baton)?;
            let mine = baton.holder_id.as_deref() == Some(&config.delegation.device_id);
            let target = match (&holder, baton.holder_id.as_deref()) {
                // Released by the cloud: nothing runs there, hydrate now.
                (_, None) => Some(baton.epoch),
                // A return this device already acquired did not finish.
                (None, Some(_)) if mine && baton.epoch == epoch => Some(epoch),
                (None, _) => None,
                // The cloud's lease lapsed (it stopped or lost the account):
                // take the project home from its last acknowledged checkpoint.
                (Some(_), Some(_)) if !mine && execution::expired(&baton) => Some(baton.epoch),
                (Some(recorded), Some(current)) if current == recorded && settled => {
                    if hosts.is_none() {
                        hosts = Some(
                            transport::request(
                                &config.keeper_url,
                                "/v1/hosts",
                                "GET",
                                &config.delegation.access_token,
                                None,
                            )
                            .await
                            .context("Could not reconnect to your saved work")?
                            .json()
                            .context("Could not read your connected workspaces")?,
                        );
                    }
                    let host = hosts.as_ref().and_then(|hosts| {
                        hosts
                            .iter()
                            .find(|host| {
                                host.worker_holder() == Some(current) && host.status == "connected"
                            })
                            .cloned()
                    });
                    match host {
                        Some(host) => {
                            handback::prepare(
                                state,
                                config,
                                &workspace,
                                &host,
                                current,
                                baton.epoch,
                            )
                            .await?
                        }
                        None => None,
                    }
                }
                _ => None,
            };
            let Some(epoch) = target else {
                return Ok::<_, anyhow::Error>(());
            };
            hydrate(state, config, &workspace, epoch, false, None)
                .await
                .context("Could not restore your saved work on this computer")?;
            if let Some(status) = lock(&state.pro.status).get_mut(&workspace) {
                status.error = None;
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                lock(&state.pro.return_backoff).remove(&workspace);
            }
            Err(error) => {
                {
                    let mut backoff = lock(&state.pro.return_backoff);
                    if backoff.len() >= 128 && !backoff.contains_key(&workspace) {
                        backoff.clear();
                    }
                    let delay = backoff
                        .get(&workspace)
                        .map_or(120, |(_, delay)| (delay * 2).min(RETURN_BACKOFF_MAX));
                    backoff.insert(workspace.clone(), (super::now() + delay, delay));
                }
                record_error(state, &workspace, &error);
                tracing::warn!(phase="automatic_return", error=%error, "Project return did not complete");
            }
        }
    }
    Ok(())
}

fn same_file(left: &Path, right: &Path) -> Result<bool> {
    use std::io::Read;
    let open = |path: &Path| -> Result<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            );
        }
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() <= super::policy::MAX_FILE_BYTES,
            "comparison requires bounded regular files"
        );
        Ok(file)
    };
    let mut left = open(left)?;
    let mut right = open(right)?;
    if left.metadata()?.len() != right.metadata()?.len() {
        return Ok(false);
    }
    let mut a = [0u8; 65536];
    let mut b = [0u8; 65536];
    loop {
        let count = left.read(&mut a)?;
        if count == 0 {
            return Ok(true);
        }
        right.read_exact(&mut b[..count])?;
        if a[..count] != b[..count] {
            return Ok(false);
        }
    }
}

async fn finish_hydration(
    state: &Arc<AppState>,
    workspace: &str,
    epoch: u64,
    generation: u64,
    setup: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    finish_hydration_checked(
        state,
        workspace,
        epoch,
        generation,
        setup,
        super::provider_gate::check(state, workspace, true),
    )
    .await
}

async fn finish_hydration_checked(
    state: &Arc<AppState>,
    workspace: &str,
    epoch: u64,
    generation: u64,
    setup: impl std::future::Future<Output = Result<()>>,
    providers: impl std::future::Future<Output = Vec<super::provider_gate::BlockedProvider>>,
) -> Result<()> {
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed during project setup"
    );
    ensure!(
        matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::Hydrating{epoch:current} | Ownership::SettingUp{epoch:current}) if *current==epoch),
        "Project ownership changed before setup"
    );
    lock(&state.pro.ownership).insert(workspace.into(), Ownership::SettingUp { epoch });
    super::persist(state).await?;
    if let Err(error) = super::PROFILE_SETUP
        .scope((workspace.to_owned(), generation), setup)
        .await
    {
        record_error(state, workspace, &error);
        state.changes.notify_waiters();
        return Err(error);
    }
    let blocked = providers.await;
    // Account replacement cannot race a successful readiness check into a new
    // writer grant. Checks are outside this lock; the local transition is not.
    let _configuration = state.pro.configuration.lock().await;
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire)
            && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::SettingUp {epoch:current}) if *current==epoch),
        "Project ownership changed during setup"
    );
    if let Err(error) = super::provider_gate::record(state, workspace, blocked) {
        super::persist(state).await?;
        return Err(error);
    }
    {
        let mut ownership = lock(&state.pro.ownership);
        ensure!(
            matches!(ownership.get(workspace), Some(Ownership::SettingUp { epoch: current }) if *current == epoch),
            "Project ownership changed before resume"
        );
        ownership.insert(workspace.into(), Ownership::Local { epoch });
    }
    super::persist(state).await?;
    if let Some(status) = lock(&state.pro.status).get_mut(workspace) {
        status.error = None;
    }
    // Each managed child takes this lock for durable launch admission. Release
    // it before restoring sessions; their admission rechecks the current grant.
    drop(_configuration);
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed before project resume"
    );
    if execution::resume_allowed(state, workspace) {
        crate::ledger::resume_deferred_workspace(state, workspace).await?;
    }
    Ok(())
}

async fn run_profile_steps(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
) -> Result<()> {
    // Deferred laptop steps are agent guidance, never daemon auto-exec.
    if config.role != Role::Worker {
        return Ok(());
    }
    let profile = lock(&state.pro.preferences)
        .get(workspace)
        .map(|preference| preference.profile.clone())
        .unwrap_or_default();
    let commands: Vec<String> = profile.setup_command.into_iter().collect();
    if commands.is_empty() {
        return Ok(());
    }
    let workspace_record = lock(&state.workspaces)
        .get(workspace)
        .context("unknown workspace")?;
    let row = crate::spawn::spawn_session(
        state,
        crate::spawn::SpawnSpec {
            native_cwd: None,
            workspace: workspace_record,
            id: None,
            name: Some(
                if config.role == Role::Worker {
                    "Cloud setup"
                } else {
                    "Deferred laptop steps"
                }
                .into(),
            ),
            cwd: None,
            cols: None,
            rows: None,
            theme: "dark".into(),
            title_hint: None,
            prelude: None,
            kind: crate::spawn::SpawnKind::Shell,
            fork_head: false,
        },
    )
    .await
    .map_err(|_| anyhow::anyhow!("could not create project setup terminal"))?;
    let id = row["id"]
        .as_str()
        .context("setup terminal has no identity")?;
    for command in commands {
        ensure!(
            super::may_write(state, workspace),
            "Account changed before project setup execution"
        );
        let outcome =
            crate::exec::run_exec(state, id, command.clone(), Some(600_000), Some(15_000))
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if outcome.record.exit_code != Some(0) || outcome.timed_out {
            anyhow::bail!("project setup needs attention in its terminal");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "continuity_tests.rs"]
pub(super) mod continuity_tests;
#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;

pub(super) fn eligible(state: &AppState, workspace: &crate::workspaces::Workspace) -> bool {
    if authority::registered_root(state, &workspace.id, &workspace.root).is_err() {
        return false;
    }
    if crate::cloud::is_onboarding_workspace(workspace)
        || lock(&state.pro.legacy_pending).contains(&workspace.id)
        || !super::projects::account_matches(state, &workspace.id)
    {
        return false;
    }
    let home = state.claude_settings_path.parent().and_then(Path::parent);
    if home.is_some_and(|home| home.starts_with(&workspace.root)) {
        return false;
    }
    if workspace.root.components().any(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some(
                ".ssh"
                    | ".aws"
                    | ".azure"
                    | ".gnupg"
                    | ".config"
                    | ".codex"
                    | ".claude"
                    | ".chimaera"
                    | "Library"
            )
        )
    }) {
        return false;
    }
    workspace.last_opened_at.saturating_add(30 * 24 * 3600) >= super::now()
        || lock(&state.session_workspaces)
            .values()
            .any(|id| id == &workspace.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn setup_failure_fences_agents_until_success_and_laptop_steps_never_autoplay() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-setup-fence-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        lock(&state.workspaces)
            .import_exact(crate::workspaces::Workspace {
                id: "w-project".into(),
                root: root.clone(),
                name: "Fixture".into(),
                last_opened_at: super::super::now(),
                mastermind: None,
                plugins_on: vec![],
                cloud_internal: false,
            })
            .unwrap();
        lock(&state.pro.ownership).insert("w-project".into(), Ownership::Hydrating { epoch: 3 });
        let owner = state.clone();
        let result = finish_hydration(&state, "w-project", 3, 0, async move {
            assert!(super::super::may_write(&owner, "w-project"));
            let outside = owner.clone();
            assert!(
                !tokio::spawn(async move { super::super::may_write(&outside, "w-project") })
                    .await
                    .unwrap()
            );
            anyhow::bail!("fixture setup failed")
        })
        .await;
        assert!(result.is_err());
        assert!(!super::super::may_write(&state, "w-project"));
        assert_eq!(super::super::owned_epoch(&state, "w-project"), None);
        assert!(lock(&state.pro.status)
            .get("w-project")
            .unwrap()
            .error
            .is_some());
        let restarted = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        assert!(!super::super::may_write(&restarted, "w-project"));
        finish_hydration(&state, "w-project", 3, 0, async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(super::super::owned_epoch(&state, "w-project"), Some(3));
        let config = Configure {
            recovery: false,
            execution: None,
            account_id: None,
            role: Role::Device,
            endpoint: String::new(),
            keeper_url: String::new(),
            hours_exhausted: false,
            delegation: super::super::protocol::Delegation {
                workspace: None,
                access_token: String::new(),
                expires_at: String::new(),
                scope: vec![],
                device_id: String::new(),
            },
        };
        lock(&state.pro.preferences)
            .entry("w-project".into())
            .or_default()
            .profile
            .deferred = vec!["must-not-be-executed".into()];
        run_profile_steps(&state, &config, "w-project")
            .await
            .unwrap();
        assert_eq!(
            lock(&state.pro.preferences)
                .get("w-project")
                .unwrap()
                .profile
                .deferred,
            ["must-not-be-executed"]
        );
        drop(restarted);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn three_way_return_preserves_local_conflicts_and_applies_unmodified_files() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-pro-merge-{}",
            chimaera_core::generate_token()
        ));
        let base = root.join("base");
        let local = root.join("local");
        let cloud = root.join("cloud");
        for dir in [&base, &local, &cloud] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for name in ["same.txt", "conflict.txt"] {
            std::fs::write(base.join(name), "base").unwrap();
            std::fs::write(cloud.join(name), "cloud").unwrap();
        }
        std::fs::write(local.join("same.txt"), "base").unwrap();
        std::fs::write(local.join("conflict.txt"), "local").unwrap();
        for name in ["removed.txt", "removed-but-edited.txt"] {
            std::fs::write(base.join(name), "base").unwrap();
            std::fs::write(
                local.join(name),
                if name == "removed.txt" {
                    "base"
                } else {
                    "local"
                },
            )
            .unwrap();
        }
        install_tree(&cloud, &local, Some(&base), None).unwrap();
        assert_eq!(
            std::fs::read_to_string(local.join("same.txt")).unwrap(),
            "cloud"
        );
        assert_eq!(
            std::fs::read_to_string(local.join("conflict.txt")).unwrap(),
            "local"
        );
        assert!(!local.join("removed.txt").exists());
        assert_eq!(
            std::fs::read_to_string(local.join("removed-but-edited.txt")).unwrap(),
            "local"
        );
        assert!(std::fs::read_dir(&local).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("conflict.txt.cloud-")));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn canonical_return_uses_cloud_files_and_preserves_unpublished_local_edits() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-canonical-return-{}",
            chimaera_core::generate_token()
        ));
        let local = root.join("local");
        let cloud = root.join("cloud");
        let base = root.join("base");
        let conflicts = root.join("private-conflicts");
        for dir in [&local, &cloud, &base] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for name in ["changed", "deleted"] {
            std::fs::write(base.join(name), "base").unwrap();
            std::fs::write(local.join(name), "unpublished local").unwrap();
        }
        std::fs::write(cloud.join("changed"), "canonical cloud").unwrap();
        install_tree(&cloud, &local, Some(&base), Some(&conflicts)).unwrap();
        assert_eq!(
            std::fs::read_to_string(local.join("changed")).unwrap(),
            "canonical cloud"
        );
        assert!(!local.join("deleted").exists());
        let mut preserved = Vec::new();
        for dir in std::fs::read_dir(&conflicts).unwrap() {
            for file in std::fs::read_dir(dir.unwrap().path()).unwrap() {
                preserved.push(std::fs::read_to_string(file.unwrap().path()).unwrap());
            }
        }
        assert_eq!(preserved, vec!["unpublished local", "unpublished local"]);
        assert_eq!(
            std::fs::read_dir(local).unwrap().count(),
            1,
            "conflicts are outside the canonical mirror"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod pause_tests {
    use super::*;
    #[test]
    fn completed_chat_needs_no_vendor_status_but_active_work_stays_blocked() {
        let mut chat = chimaera_agent::ChatInfo {
            id: "s-codex".into(),
            agent: "codex".into(),
            cwd: "/tmp".into(),
            created_at_ms: 0,
            alive: true,
            exit_status: None,
            native_session_id: None,
            model: None,
            current_mode: None,
            pending_permission: false,
            status_detail: None,
            status_category: None,
            status_needs_action: false,
            remote_control_url: None,
            background_running: 0,
        };
        let mut carry = chimaera_agent::Carryover::default();
        let finished = Some(crate::agent_state::AgentState::Finished);
        assert!(chat_at_pause(&chat, Some(&carry), false, finished));
        assert!(!chat_at_pause(&chat, Some(&carry), true, finished));
        carry.turn_in_flight = true;
        assert!(!chat_at_pause(&chat, Some(&carry), false, finished));
        carry.turn_in_flight = false;
        chat.background_running = 1;
        assert!(!chat_at_pause(&chat, Some(&carry), false, finished));
        chat.background_running = 0;
        assert!(!chat_at_pause(
            &chat,
            Some(&carry),
            false,
            Some(crate::agent_state::AgentState::Running)
        ));
        assert!(!chat_at_pause(&chat, None, false, finished));
        chat.pending_permission = true;
        carry.turn_in_flight = true;
        assert!(chat_at_pause(
            &chat,
            Some(&carry),
            false,
            Some(crate::agent_state::AgentState::NeedsPermission)
        ));
        assert!(!chat_at_pause(&chat, Some(&carry), true, finished));
    }
}

fn continuation(state: &AppState, workspace: &str) -> execution::wire::Continuation {
    use execution::wire::Continuation;
    let mut result = Continuation::Idle;
    for agent in crate::ledger::snapshot(state)
        .0
        .into_iter()
        .filter(|entry| entry.workspace_id == workspace)
        .filter_map(|entry| entry.agent)
    {
        match agent.carryover {
            None => return Continuation::Uncertain,
            Some(carry) if carry.interrupted_work() => result = Continuation::Interrupted,
            _ => {}
        }
    }
    result
}
