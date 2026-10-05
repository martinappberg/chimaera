use super::{
    authority, config, execution, mirror,
    protocol::{Baton, Configure, MirrorCredentials, Role},
    transport, Ownership, WorkspaceStatus,
};
use crate::{lock, AppState};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeSet,
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
pub struct Manifest {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<super::projects::catalog::Metadata>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub repository_origin: Option<String>,
    #[serde(default)]
    pub repository: Option<super::repository::Snapshot>,
    pub workspace_id: String,
    pub root: PathBuf,
    pub name: String,
    pub epoch: u64,
    pub clean: bool,
    #[serde(default)]
    pub continuation: execution::wire::Continuation,
    /// Environment variable names the sender's configuration export left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_environment: Vec<String>,
    pub sessions: Vec<SessionArchive>,
    /// Additive: project paths this snapshot deliberately left out. Only a
    /// snapshot that carries this inventory can show that a file is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_out: Option<Vec<PathBuf>>,
    /// Additive: the sending machine's OS and CPU (`std::env::consts`), so
    /// the receiver can tell its agents what the other side is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_arch: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct SessionArchive {
    pub id: String,
    pub archive: String,
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
    let response = transport::request(
        &config.endpoint,
        path,
        method,
        &config.delegation.access_token,
        body,
    )
    .await?;
    ensure!(
        !transport::return_window_ended(&response),
        transport::RETURN_WINDOW_ENDED
    );
    Ok(response)
}
pub(super) async fn credentials(
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
pub(crate) mod coordinator_host;
pub(crate) mod project_host;

pub(super) fn start(state: Arc<AppState>) {
    let Some(runtime) = state.daemon_extension.clone() else {
        return;
    };
    let owner = coordinator_host::CoordinatorOwner::capture(state.clone());
    let task = tokio::spawn(runtime.coordinate(owner));
    if let Some(old) = lock(&state.pro.task).replace(task) {
        old.abort();
    }
}
/// Renews this daemon's delegation. A definitive refusal (401/403) marks it
/// refused so `/pro/status` tells the native app to mint a new one; a
/// transport failure only retries later.
pub(super) async fn renew_delegation(
    state: &AppState,
    config: &Configure,
    generation: u64,
) -> bool {
    match account(config, "/v1/delegations/renew", "POST", Some(&json!({}))).await {
        Ok(response) if matches!(response.status, 401 | 403) => {
            state.pro.delegation_refused.store(true, Ordering::Release);
            state.changes.notify_waiters();
            false
        }
        Ok(response) => {
            let installed =
                response
                    .json::<super::protocol::Delegation>()
                    .is_ok_and(|delegation| {
                        authority::install_renewal(
                            state,
                            generation,
                            &config.delegation,
                            delegation,
                        )
                    });
            if installed {
                state.pro.delegation_refused.store(false, Ordering::Release);
            }
            installed
        }
        Err(_) => false,
    }
}

fn record_error(state: &AppState, workspace: &str, error: &anyhow::Error) {
    let message: String = error.to_string().chars().take(256).collect();
    let mut statuses = lock(&state.pro.status);
    let status = statuses.entry(workspace.into()).or_default();
    status.error = Some(message);
    status.error_code = Some(super::routes::error_code(error));
}
#[cfg(test)]
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
    let owner = project_host::ProjectOwner::capture(
        state.clone(),
        config.clone(),
        workspace.to_owned(),
        generation,
    )?;
    let runtime = state
        .daemon_extension
        .as_ref()
        .context("optional_runtime_unavailable")?;
    runtime.reconcile(owner).await
}
/// A named boxed future breaks the reconcile -> hydrate -> reconcile type cycle
/// for a spawned install.
fn install_owned(
    state: Arc<AppState>,
    config: Configure,
    workspace: String,
    epoch: u64,
) -> futures::future::BoxFuture<'static, Result<()>> {
    Box::pin(async move { hydrate(&state, &config, &workspace, epoch, false, None).await })
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
    // Stopping agents can take seconds each; the renewal loop moves on.
    let owner = state.clone();
    let key = workspace.to_owned();
    tokio::spawn(async move {
        super::detached::run(
            &owner.clone(),
            ("suspend", false),
            &key.clone(),
            0,
            || None,
            move || async move {
                if let Err(error) = suspend_workspace(&owner, &key).await {
                    record_error(&owner, &key, &error);
                    return super::detached::Outcome::refused(
                        axum::http::StatusCode::CONFLICT,
                        None,
                    );
                }
                super::detached::Outcome::done()
            },
        )
        .await
    });
    Ok(())
}

async fn suspend_workspace(state: &Arc<AppState>, workspace: &str) -> Result<()> {
    for id in sessions(state, workspace) {
        match crate::bundle::export_durable(state.clone(), &id, crate::bundle::ExportMode::Stop)
            .await
        {
            Ok(archive) => {
                let _ = tokio::fs::remove_file(archive).await;
            }
            Err(_) => {
                // Losing a verified lease must stop an agent even before its
                // first native conversation id exists. Preserve its ledger
                // identity; absence of an exportable handle cannot authorize a
                // second writer to continue running.
                park_here(state, &id).await;
            }
        }
    }
    let owner = state.clone();
    tokio::task::spawn_blocking(move || {
        let (entries, links) = crate::ledger::snapshot(&owner);
        lock(&owner.ledger).write_durable(&entries, &links)
    })
    .await??;
    Ok(())
}

/// Stops an agent that cannot be exported and keeps it here as a paused row
/// with its identity (it resumes when the project is this computer's again).
/// A terminal is never stopped.
async fn park_here(state: &Arc<AppState>, id: &str) {
    if !(state.chat.get(id).is_some() || lock(&state.agents).contains_key(id)) {
        return;
    }
    if let Some(mut entry) = crate::ledger::snapshot(state)
        .0
        .into_iter()
        .find(|entry| entry.id == id)
    {
        entry.suspended = true;
        entry.handoff = None;
        lock(&state.deferred_sessions).insert(id.to_owned(), entry);
    }
    if state.chat.get(id).is_some() {
        state.chat.kill(id);
    } else {
        let _ = state.sessions.kill(id);
    }
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
        .collect();
    let agents = lock(&state.agents);
    ids.into_iter()
        .filter(|id| agents.contains_key(id) || state.chat.get(id).is_some())
        .collect()
}

fn transfer_session_ids(state: &AppState, workspace: &str) -> Result<Vec<String>> {
    let ids: Vec<_> = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, id)| id.as_str() == workspace)
        .map(|(id, _)| id.clone())
        .take(65)
        .collect();
    ensure!(
        ids.len() <= 64,
        "Project transfer supports at most 64 sessions; close some sessions and try again"
    );
    Ok(ids)
}
/// A clean flush before this computer sleeps: one shared deadline, and no
/// release once the computer woke again (the project simply stays here).
#[derive(Clone, Copy)]
pub struct Sleep {
    pub generation: u64,
    pub deadline: tokio::time::Instant,
    /// The app is quitting, not the computer sleeping: a flush that cannot
    /// hand over recovers at once and its work continues here, instead of
    /// waiting for a wake with its lease left to lapse.
    pub park: bool,
    /// A browser asked the cloud to run this project after the app quit
    /// (`leave::open_elsewhere`): it moves even when no conversation does,
    /// since the account wakes the cloud for that request itself.
    pub opened: bool,
}
impl Sleep {
    pub(super) fn woke(&self, state: &AppState) -> bool {
        state.pro.sleep_generation.load(Ordering::Acquire) != self.generation
    }
}
/// The account required the newer path for a legacy release (see
/// `release::UpgradeRequired`); the project retries through it by itself.
pub(super) fn upgrade_required(error: &anyhow::Error) -> bool {
    error.is::<release::UpgradeRequired>()
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
        cache.clone(),
        snapshot_inner_scoped(
            state,
            config,
            workspace,
            clean,
            sleep,
            phase,
            (cache, generation),
        ),
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
    admission: (Arc<tokio::sync::OwnedMutexGuard<()>>, u64),
) -> Result<()> {
    let (cache_guard, original_generation) = admission;
    ensure!(
        !super::project_copy::copy_only(state, workspace),
        "local copy cannot publish execution state"
    );
    authority::config_matches(state, config, workspace)?;
    let effective = execution::effective(state, config, workspace)?;
    // The unrefined configuration, for a reconcile after the account has
    // required the newer path (`effective` below is legacy for this project).
    let requested = config;
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
    // Refuse before stopping or publishing anything. Taking a prefix could
    // release legacy ownership with an omitted agent still running locally.
    let session_ids = transfer_session_ids(state, &workspace.id)?;
    *phase = "destination";
    authority::destination(state, config, &workspace.id, Some(&workspace.root)).await?;
    *phase = "credentials";
    let grant = credentials(config, &workspace.id, Some(epoch)).await?;
    *phase = "companion";
    let companion = super::companion::preflight().await?;
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed during companion capture"
    );
    authority::config_matches(state, requested, &workspace.id)?;
    ensure!(
        config.recovery || execution::lease_valid(state, &workspace.id),
        "execution authority expired during companion capture"
    );
    ensure!(
        super::owned_epoch(state, &workspace.id) == Some(epoch),
        "Project ownership changed during companion capture"
    );
    let captured_sessions = transfer_session_ids(state, &workspace.id)?;
    ensure!(
        captured_sessions.len() == session_ids.len()
            && captured_sessions.iter().all(|id| session_ids.contains(id)),
        "Project sessions changed during companion capture"
    );
    let transfer = super::transfer_dispatch::TransferScope::capture(
        state,
        &workspace.id,
        Some(&workspace.root),
        cache_guard,
        original_generation,
    )
    .await?;
    let grant = transfer.host.grant(grant)?;
    // A quit handover carries every conversation that made it move, or
    // moves nothing (the private snapshot checks before stopping any).
    let must_carry = if sleep.is_some_and(|sleep| sleep.park && !sleep.opened) {
        leaving_sessions(state, &workspace.id)
    } else {
        Vec::new()
    };
    let owner = project_host::SnapshotOwner {
        project: project_host::ProjectOwner::capture(
            state.clone(),
            requested.clone(),
            workspace.id.clone(),
            generation,
        )?,
        effective: config.clone(),
        requested: requested.clone(),
        workspace,
        epoch,
        must_carry,
        session_ids,
        companion: std::sync::Mutex::new(Some(companion)),
        transfer: transfer.clone(),
        grant,
        sleep,
        clean,
    };
    let runtime = transfer.runtime.clone();
    super::transfer_dispatch::scope(transfer, runtime.snapshot(owner, phase)).await
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

/// Read-only checkpoint and credential admission. This owns no cache, live
/// destination or execution grant; materialization consumes the exact result.
struct SnapshotRead {
    config: Configure,
    receipt: Option<execution::wire::Checkpoint>,
    grant: MirrorCredentials,
}
async fn read_snapshot(config: &Configure, workspace: &str) -> Result<SnapshotRead> {
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
    read_snapshot_at(&effective, workspace, receipt.as_ref()).await
}
async fn read_snapshot_at(
    config: &Configure,
    workspace: &str,
    receipt: Option<&execution::wire::Checkpoint>,
) -> Result<SnapshotRead> {
    if let Some(receipt) = receipt {
        execution::receipt::validate(receipt)?;
    }
    let grant = credentials(config, workspace, None).await?;
    Ok(SnapshotRead {
        config: config.clone(),
        receipt: receipt.cloned(),
        grant,
    })
}
pub(super) async fn fetch_snapshot(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    cache: &Path,
    cache_guard: Arc<tokio::sync::OwnedMutexGuard<()>>,
    generation: u64,
) -> Result<Manifest> {
    let admission = read_snapshot(config, workspace).await?;
    // Initial HTTP hydration has no existing project transfer scope. Admit the
    // read first, then retain its original cache and generation for materialization.
    let transfer = super::transfer_dispatch::TransferScope::capture(
        state,
        workspace,
        None,
        cache_guard,
        generation,
    )
    .await?;
    super::transfer_dispatch::scope(
        transfer,
        fetch_snapshot_admitted(workspace, cache, admission),
    )
    .await
}
pub(super) async fn fetch_snapshot_at(
    config: &Configure,
    workspace: &str,
    cache: &Path,
    receipt: Option<&execution::wire::Checkpoint>,
) -> Result<Manifest> {
    let admission = read_snapshot_at(config, workspace, receipt).await?;
    fetch_snapshot_admitted(workspace, cache, admission).await
}
async fn fetch_snapshot_admitted(
    workspace: &str,
    cache: &Path,
    admission: SnapshotRead,
) -> Result<Manifest> {
    let SnapshotRead {
        config,
        receipt,
        grant,
    } = admission;
    authority::config_workspace(&config, workspace)?;
    let receipt = receipt.as_ref();
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
    Box::pin(hydrate_generation(
        state,
        config,
        workspace,
        expected_epoch,
        fork,
        destination_root,
        generation,
    ))
    .await
}
async fn hydrate_generation(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    expected_epoch: u64,
    fork: bool,
    destination_root: Option<&Path>,
    generation: u64,
) -> Result<()> {
    ensure!(
        !super::project_copy::copy_only(state, workspace)
            || lock(&state.pro.preferences)
                .get(workspace)
                .and_then(|p| p.copy.as_ref())
                .is_some_and(|copy| copy.takeover_requested),
        "Explicit Take over is required for a local copy"
    );
    let bound_destination =
        authority::destination(state, config, workspace, destination_root).await?;
    let destination_root = bound_destination.as_deref();
    let cache = Arc::new(state.pro.cache(workspace)?.lock_owned().await);
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed while waiting for project cache"
    );
    transport::cache_quiescent(workspace)?;
    let result = transport::cache_scope(
        workspace,
        cache.clone(),
        hydrate_scoped(
            state,
            config,
            workspace,
            expected_epoch,
            fork,
            destination_root,
            (cache, generation),
        ),
    )
    .await;
    if let Err(error) = &result {
        // Keep worker failures diagnosable without logging helper stderr,
        // credentials, project paths or native transcript contents.
        let phase = if error
            .chain()
            .any(|cause| cause.to_string() == "repository return preparation failed")
        {
            "repository_prepare"
        } else {
            "hydrate"
        };
        tracing::warn!(
            phase,
            category = super::routes::error_code(error),
            epoch = expected_epoch,
            "project hydration failed; installation remains fenced"
        );
    }
    result
}

type ReturnReport = (Vec<String>, (usize, Vec<PathBuf>));

async fn hydrate_scoped(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    expected_epoch: u64,
    fork: bool,
    destination_root: Option<&Path>,
    original: (Arc<tokio::sync::OwnedMutexGuard<()>>, u64),
) -> Result<()> {
    let (mut cache_guard, generation) = original;
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
    if !super::project_copy::copy_only(state, workspace)
        && lock(&state.workspaces).get(workspace).is_some()
        && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::SettingUp{epoch}) if *epoch==expected_epoch)
    {
        reconcile_generation(state, config, workspace, generation).await?;
        let path = state.pro.root.join(workspace).join("return-install");
        let endpoint = config.endpoint.clone();
        let account = config.account_id.clone();
        let workspace_id = workspace.to_owned();
        tokio::task::spawn_blocking(move || {
            super::install::Transaction::cleanup_committed(
                &path,
                &endpoint,
                account.as_deref(),
                &workspace_id,
                expected_epoch,
            )
        })
        .await??;
        let _ =
            tokio::fs::remove_dir_all(state.pro.root.join(workspace).join("return-stage")).await;
        finish_hydration(state, workspace, expected_epoch, generation).await?;
        return Ok(());
    }
    // Existing durable worker work must never be replaced with an older remote
    // snapshot after a restart. The normal grant path resumes its own ledger.
    // A managed project whose current epoch this machine verifiably holds is
    // the same no-op (its running agents are not stopped, nothing is
    // reinstalled); one with uncertain or unproven old processes still takes
    // the checkpoint.
    if lock(&state.workspaces).get(workspace).is_some()
        && config.role == Role::Worker
        && (!execution::managed(state, workspace)
            || (!execution::uncertain(state, workspace) && !execution::unclean(state, workspace)))
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
    // Preserve the original read admission before optional artifact discovery.
    // A missing helper refuses before Git/cache or executor effects, without
    // suppressing the account's checkpoint/credential refusal and backoff path.
    let snapshot = read_snapshot(config, workspace).await?;
    current()?;
    let companion = super::companion::preflight().await?;
    current()?;
    let transfer = super::transfer_dispatch::TransferScope::capture(
        state,
        workspace,
        None,
        cache_guard.clone(),
        generation,
    )
    .await?;
    let original_transfer = transfer.host.clone();
    super::transfer_dispatch::scope(transfer, async {
    if execution::managed(state, workspace) {
        // No canonical files are installed while an old local managed executor
        // can still write them. An unclean same-boot registry remains blocked.
        execution::fence_workspace(state, workspace);
        execution::stop(state, &[workspace.to_owned()]).await?;
    }
    let interrupted = state.pro.root.join(workspace);
    tokio::task::spawn_blocking(move || mirror::clear_interrupted(&interrupted)).await??;
    let cache = state.pro.root.join(workspace).join("incoming.git");
    let manifest = fetch_snapshot_admitted(workspace, &cache, snapshot).await?;
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
    original_transfer.bind_source(&destination_root).await?;
    current()?;
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
    // A project taken here (a return, an adoption, a reopened folder) is this
    // device's from now on: bind it to the account at once, as reconcile does
    // after its own acquire, so the cloud-project listing can match the folder
    // immediately instead of after the next renewal.
    super::projects::bind_workspace_account(state, config, workspace)?;
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
    // This stage is a durable part of the installation journal. The generic
    // interrupted-helper sweep deliberately does not remove return-stage.
    let stage = state.pro.root.join(workspace).join("return-stage");
    let transaction_root = state.pro.root.join(workspace).join("return-install");
    let checkpoint_binding = if let Some(receipt) = receipt {
        receipt.id.clone()
    } else {
        String::from_utf8(
            transport::git_output(
                transport::git(&cache, None).await?,
                &[
                    "rev-parse",
                    "refs/heads/main",
                    "refs/heads/config",
                    "refs/heads/handoff",
                ],
                vec![],
            )
            .await?,
        )?
    };
    let binding = super::install::Binding {
        endpoint: config.endpoint.clone(),
        account: config.account_id.clone(),
        workspace: workspace.into(),
        epoch: grant.epoch,
        receipt: Some(checkpoint_binding),
    };
    let (journal_path, journal_binding) = (transaction_root.clone(), binding.clone());
    let mut transaction = tokio::task::spawn_blocking(move || {
        super::install::Transaction::open(&journal_path, &journal_binding)
    })
    .await??;
    let recovering = transaction.is_some();
    let result = async {
        let read_grant = credentials(config, workspace, None).await?;
        let local_shadow = state.pro.root.join(workspace).join("working-tree.git");
        let mut planned = Vec::new();
        let (git_branches,kept,git_staging) = if !recovering {
            if tokio::fs::try_exists(&stage).await? { tokio::fs::remove_dir_all(&stage).await?; }
            let private_stage=stage.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                use std::os::unix::fs::PermissionsExt;
                std::fs::create_dir_all(&private_stage)?;
                std::fs::set_permissions(&private_stage,std::fs::Permissions::from_mode(0o700))?;
                Ok(())
            }).await??;
            let transfer_budget = read_grant.storage_limit_bytes.min(1024*1024*1024);
            let mut transfer_bytes = 0u64;
            for branch in ["main", "config", "handoff"] {
                let bytes = mirror::validate_tree_bytes(&cache,execution::receipt::revision(receipt, branch)?,transfer_budget,read_grant.max_file_bytes).await?;
                transfer_bytes = transfer_bytes.checked_add(bytes).context("snapshot size overflow")?;
                ensure!(transfer_bytes <= transfer_budget, "snapshot exceeds combined storage limit");
            }
            for (branch, folder) in [("main", "tree"),("config", "config"),("handoff", "handoff")] {
                let destination = stage.join(folder);
                tokio::fs::create_dir_all(&destination).await?;
                let mut command = transport::git(&cache, None).await?;
                command.env("GIT_WORK_TREE", &destination);
                transport::git_output(command,&["--work-tree",destination.to_str().context("invalid stage path")?,"checkout",execution::receipt::revision(receipt, branch)?,"--","."],vec![]).await?;
            }
            current()?;
            authority::destination(state,config,workspace,Some(&destination_root)).await?;
            super::projects::begin_install(state,workspace,&destination_root).await?;
            let original = destination_root.clone();
            let before = stage.join("tree-before");
            let checkout = stage.join("checkout");
            let budget = read_grant.storage_limit_bytes;
            let (before_copy,checkout_copy)=(before.clone(),checkout.clone());
            tokio::task::spawn_blocking(move || -> Result<()> {
                super::install::snapshot(&original,&before_copy,&|path| super::policy::allowed_path(path) || path.file_name().and_then(|name|name.to_str()).is_some_and(super::canonical::kept_copy_name),budget)?;
                super::install::snapshot(&before_copy,&checkout_copy,&|_|true,budget)
            }).await??;
            let (acknowledged, copy_checkpoint) = {
                let preferences = lock(&state.pro.preferences);
                let preference = preferences.get(workspace);
                (preference.and_then(|p| p.published_handoff.clone()), preference.and_then(|p| p.copy.as_ref()).and_then(|copy| copy.checkpoint.clone()))
            };
            if let Some(checkpoint) = &copy_checkpoint {
                execution::receipt::validate(checkpoint)?;
            }
            let grant = original_transfer.grant(read_grant.clone())?;
            let repository_cache = state.pro.root.join(workspace).join("incoming-repository.git");
            let answer = super::transfer_dispatch::call(super::transfer_dispatch::TransferRequest::PrepareReturnRepository(super::transfer_types::ReturnRepository {
                original: &destination_root,
                checkout: &checkout,
                stage: &stage,
                incoming: super::transfer_types::Incoming {
                    cache: &repository_cache, credentials: &grant,
                    branch: manifest.branch.as_deref(), origin: manifest.repository_origin.as_deref(),
                    snapshot: manifest.repository.as_ref(), staging: None,
                },
                published_handoff: acknowledged.as_deref(),
                copy_checkpoint: copy_checkpoint.as_ref(),
                check: &current,
            })).await?;
            let (super::repository::Prepared { branches: git_branches, writes: git_writes, staging: git_staging }, has_baseline) = match answer {
                super::transfer_dispatch::TransferReply::ReturnRepository(prepared, has_baseline) => (prepared, has_baseline),
                _ => bail!("optional transfer runtime returned an invalid result"),
            };
            planned.extend(git_writes);
            let baseline = stage.join("baseline");
        current()?;
        authority::destination(state, config, workspace, Some(&destination_root)).await?;
        let tree = stage.join("tree");
        let destination = checkout.clone();
        let left_out = manifest.left_out.clone();
        let kept = tokio::task::spawn_blocking(move || {
            install_tree(
                &tree,
                &destination,
                has_baseline.then_some(baseline).as_deref(),
                left_out.as_deref(),
            )
        })
        .await??;
        let (root,before_copy,checkout_copy)=(destination_root.clone(),before.clone(),checkout.clone());
        planned.extend(tokio::task::spawn_blocking(move || -> Result<_> {
            let mut writes = super::install::changes(&root,&before_copy,&checkout_copy)?;
            writes.retain(|write| !write.relative.starts_with(".git"));
            Ok(writes)
        }).await??);
        let home = state.claude_settings_path.parent().and_then(Path::parent).context("agent home unavailable")?.to_path_buf();
        let (overlay,config_workspace,config_stage)=(stage.join("config"),destination_root.clone(),stage.join("configuration"));
        let budget = read_grant.storage_limit_bytes;
        planned.extend(config::prepare_import_with_image(&overlay,&home,&config_workspace,&config_stage,budget,companion).await?);
        let (marker_root,marker_checkout,marker_stage,marker_id)=(destination_root.clone(),checkout.clone(),stage.join("marker"),workspace.to_owned());
        planned.push(tokio::task::spawn_blocking(move ||prepare_marker(&marker_root,&marker_checkout,&marker_stage,&marker_id)).await??);
        let kept = super::project_copy::carry_kept(state,workspace,kept);
        let report = serde_json::to_vec(&(git_branches.clone(),kept.clone()))?;
        let report_path = stage.join("report.json");
        tokio::task::spawn_blocking(move ||crate::persist::atomic_write_json_durable(&report_path,report)).await??;
        let report_path = stage.join("staging-report.json");
        let report = serde_json::to_vec(&git_staging)?;
        tokio::task::spawn_blocking(move ||crate::persist::atomic_write_json_durable(&report_path,report)).await??;
        (git_branches,kept,git_staging)
        } else {
            let report_path = stage.join("report.json");
            let (branches,kept) = tokio::task::spawn_blocking(move || -> Result<ReturnReport> {
                let (file,meta)=crate::fs::open_regular(&report_path)?;
                ensure!(meta.len() <= 64*1024,"return report exceeds limit");
                Ok(serde_json::from_reader(file)?)
            }).await??;
            let report_path = stage.join("staging-report.json");
            let staging = tokio::task::spawn_blocking(move || -> Result<_> {
                match crate::fs::open_regular(&report_path) {
                    Ok((file,meta)) => { ensure!(meta.len() <= 32*1024,"staging return report exceeds limit"); Ok(serde_json::from_reader(file)?) },
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(super::repository::StagingStatus::Uncaptured),
                    Err(error) => Err(error.into()),
                }
            }).await??;
            (branches,kept,staging)
        };
        // All archives are validated before the first target mutation. Recovery
        // requires their original immutable metadata; it never recaptures a
        // partially installed journal as its own before-image.
        let mut sessions = Vec::new();
        for archive in &manifest.sessions {
            current()?;
            let session_stage = stage.join("sessions").join(&archive.id);
            if recovering {
                ensure!(tokio::fs::try_exists(session_stage.join("metadata.json")).await?,"session preparation missing; return recovery retained");
            }
            let mut prepared = crate::bundle::prepare_import(state.clone(),&stage.join("handoff").join(&archive.archive),crate::bundle::ImportOptions {
                defer_start:true,destination_root:Some(destination_root.clone()),fork:fork||grant.requires_fork,
                origin:if config.role == Role::Worker {crate::bundle::Origin::Moved} else {crate::bundle::Origin::Home},epoch:grant.epoch,
            },&session_stage).await?;
            if !recovering { planned.append(&mut prepared.writes); }
            sessions.push(prepared);
        }
        if transaction.is_none() {
            let budget_stage=stage.clone();
            tokio::task::spawn_blocking(move ||super::install::stage_budget(&budget_stage)).await??;
            let (root,binding,budget)=(transaction_root.clone(),binding.clone(),read_grant.storage_limit_bytes);
            transaction=Some(tokio::task::spawn_blocking(move ||super::install::Transaction::prepare(&root,binding,planned,budget)).await??);
        }
        current()?;
        authority::destination(state,config,workspace,Some(&destination_root)).await?;
        let mut install = transaction.take().context("return installation unavailable")?;
        let git_roots = super::repository::install_roots(&destination_root).await?;
        let guard_state=state.clone();
        let epoch=grant.epoch;
        let file_guard=super::mutation::begin_import(state,workspace,epoch,generation).await?;
        install=tokio::task::spawn_blocking(move || -> Result<_> {
            let admitted=|| file_guard.check(&guard_state);
            install.reserve_git(git_roots,&admitted)?;
            install.apply(&admitted)?;
            drop(file_guard);
            Ok(install)
        }).await??;
        current()?;
        let new_workspace = crate::workspaces::Workspace {
            id:workspace.into(),root:destination_root.clone(),name:manifest.name.clone(),last_opened_at:super::now(),
            mastermind:None,plugins_on:Vec::new(),cloud_internal:false,hidden:false,
        };
        let owner = state.clone();
        let registration=super::mutation::begin_import(state,workspace,epoch,generation).await?;
        tokio::task::spawn_blocking(move || -> Result<()> {registration.check(&owner)?;lock(&owner.workspaces).import_exact(new_workspace)?;drop(registration);Ok(())}).await??;
        for prepared in sessions { current()?; prepared.finalize().await?; }
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
        // Both sides now share the installed tree: it is the baseline for the
        // next return until this computer publishes again.
        mirror::initialize(&local_shadow).await?;
        let published_tree = {
            let source = cache.to_str().context("invalid cache path")?.to_owned();
            let main = execution::receipt::revision(receipt, "main")?;
            let handoff = execution::receipt::revision(receipt, "handoff")?;
            transport::git_output(
                transport::git(&local_shadow, None).await?,
                &["fetch", "--no-tags", &source, &format!("+{main}:refs/chimaera/baseline"), &format!("+{handoff}:refs/chimaera/baseline-handoff")],
                vec![],
            )
            .await?;
            let installed = transport::git_output(
                transport::git(&local_shadow, None).await?,
                &["rev-parse", "--verify", "refs/chimaera/baseline^{commit}"],
                vec![],
            )
            .await?;
            Some(String::from_utf8(installed)?.trim().to_owned())
        };
        current()?;
        let execution_uncertain = grant.requires_fork
            || receipt.is_some_and(|receipt|receipt.continuation==execution::wire::Continuation::Uncertain);
        let missing_environment=manifest.missing_environment;
        let received_handoff = receipt.map(|receipt|receipt.handoff_oid.clone());
        // Installation commits before any deferred agent is eligible to start.
        let commit_guard=super::mutation::begin_import(state,workspace,epoch,generation).await?;
        let commit_state=state.clone();
        let commit_workspace=workspace.to_owned();
        // Keep admission through the durable ownership transition, even if the
        // requesting browser disconnects after the blocking commit starts.
        let retained_transfer = original_transfer.clone();
        install=tokio::spawn(async move {
            let _retained_transfer = retained_transfer;
            commit_guard.check(&commit_state)?;
            super::report_return(&commit_state,&commit_workspace,kept,&git_branches);
            {
                let mut preferences=lock(&commit_state.pro.preferences);
                let preference=preferences.entry(commit_workspace.clone()).or_default();
                preference.execution_uncertain=execution_uncertain;
                preference.missing_environment=missing_environment;
                preference.git_branches=git_branches;
            }
            super::persist(&commit_state).await?;
            let worker_state=commit_state.clone();
            let (install,commit_guard)=tokio::task::spawn_blocking(move || -> Result<_> {
                install.commit(&||commit_guard.check(&worker_state))?;
                Ok((install,commit_guard))
            }).await??;
            {
                let mut preferences=lock(&commit_state.pro.preferences);
                let preference=preferences.entry(commit_workspace.clone()).or_default();
                if let Some(tree)=published_tree { preference.published_tree=Some(tree); }
                if let Some(handoff)=received_handoff { preference.published_handoff=Some(handoff); }
                preference.git_staging=Some(git_staging.clone());
            }
            lock(&commit_state.pro.status).entry(commit_workspace.clone()).or_default().git_staging=Some(git_staging);
            super::project_copy::promote(&commit_state,&commit_workspace,&commit_guard).await?;
            super::persist(&commit_state).await?;
            drop(commit_guard);
            Ok::<_,anyhow::Error>(install)
        }).await??;
        let retained_transfer = original_transfer.clone();
        tokio::task::spawn_blocking(move || { let _retained_transfer = retained_transfer; install.cleanup() }).await??;
        // What this machine's agents are told about the move, recorded before
        // any of them resumes (`mcp::cloud_context`).
        crate::mcp::cloud_context::record_arrival(
            state,
            workspace,
            manifest.left_out.as_deref(),
            [manifest.source_os.as_deref(), manifest.source_arch.as_deref()],
        )
        .await;
        finish_hydration(
            state,
            workspace,
            grant.epoch,
            generation,
        )
        .await?;
        drop(cache_guard);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_ok() {
        let _ = tokio::fs::remove_dir_all(stage).await;
    }
    result

    }).await
}
pub(super) fn prepare_marker(
    root: &Path,
    checkout: &Path,
    stage: &Path,
    workspace: &str,
) -> Result<super::install::Write> {
    use std::io::Write;
    std::fs::create_dir_all(stage)?;
    let target = if checkout.join(".git").is_dir() && !root.join(".git").exists() {
        root.join(".git/chimaera-workspace")
    } else {
        crate::workspaces::identity::marker_path(root)
    };
    let (target_root, relative) = if target.starts_with(root) {
        (root.to_path_buf(), target.strip_prefix(root)?.to_path_buf())
    } else {
        (
            std::fs::canonicalize(target.parent().context("invalid project marker")?)?,
            target.file_name().context("invalid project marker")?.into(),
        )
    };
    let before = stage.join("before");
    let before = if target.try_exists()? {
        let parent = target.parent().context("invalid project marker")?;
        let parent = std::fs::canonicalize(parent)?;
        let before_dir = stage.join("original");
        let name = target.file_name().context("invalid project marker")?;
        super::install::snapshot(&parent, &before_dir, &|path| path == Path::new(name), 4096)?;
        let copied = before_dir.join(name);
        ensure!(copied.is_file(), "project marker is not a regular file");
        std::fs::rename(copied, &before)?;
        Some(before)
    } else {
        None
    };
    let after = stage.join("after");
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&after)?;
    use std::os::unix::fs::PermissionsExt;
    output.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    output.write_all(&serde_json::to_vec(&crate::workspaces::identity::Marker {
        id: workspace.into(),
        written_at: super::now(),
    })?)?;
    output.sync_all()?;
    Ok(super::install::Write {
        root: target_root,
        relative,
        before,
        after: Some(after),
    })
}
/// A completed copy has its own immutable receipt baseline. Takeover must
/// preserve edits against that copy, never a newer cloud tip or local shadow.
#[cfg(all(unix, feature = "daemon-extension-fixture"))]
pub(in crate::pro) async fn takeover_copy_baseline(
    cache: &Path,
    stage: &Path,
    checkpoint: &execution::wire::Checkpoint,
    budget: u64,
) -> Result<Option<(PathBuf, super::repository::staging::Descriptor)>> {
    execution::receipt::validate(checkpoint)?;
    match super::transfer_dispatch::call(super::transfer_dispatch::TransferRequest::CopyBaseline {
        shadow: cache,
        stage,
        checkpoint,
        budget,
    })
    .await?
    {
        super::transfer_dispatch::TransferReply::StagingBaseline(value) => Ok(value),
        _ => bail!("optional transfer runtime returned an invalid result"),
    }
}
/// Only the exact acknowledged checkpoint selects a baseline.
#[cfg(all(unix, feature = "daemon-extension-fixture"))]
pub(super) async fn staging_baseline(
    shadow: &Path,
    stage: &Path,
    published: Option<String>,
    budget: u64,
) -> Result<Option<(PathBuf, super::repository::staging::Descriptor)>> {
    match super::transfer_dispatch::call(
        super::transfer_dispatch::TransferRequest::StagingBaseline {
            shadow,
            stage,
            published: published.as_deref(),
            budget,
        },
    )
    .await?
    {
        super::transfer_dispatch::TransferReply::StagingBaseline(value) => Ok(value),
        _ => bail!("optional transfer runtime returned an invalid result"),
    }
}

/// Three-way install of an incoming tree over the local project. Returns how
/// many local files were kept alongside an incoming version ("kept both"),
/// with up to 32 of their paths.
///
/// - incoming == baseline: the other side never touched it; local wins.
/// - local == baseline: only the other side changed it; incoming wins.
/// - both changed: incoming takes the path, and the user's own version is
///   kept right beside it (`<name>.mine-<yyyymmdd-hhmm>`, never mirrored).
/// - absent from incoming: deleted locally only when the incoming snapshot
///   carries an inventory (`left_out`) that shows it gone; a local edit is
///   kept beside it the same way first.
///
/// The report lists the kept copies' own paths (up to 32).
pub(super) fn install_tree(
    source: &Path,
    destination: &Path,
    baseline: Option<&Path>,
    left_out: Option<&[PathBuf]>,
) -> Result<(usize, Vec<PathBuf>)> {
    let mut kept = (0usize, Vec::new());
    let mut keep = |copy: PathBuf| {
        kept.0 += 1;
        if kept.1.len() < 32 {
            kept.1.push(copy);
        }
    };
    let mut copies = super::canonical::KeptCopies::new();
    let left_out: Option<std::collections::HashSet<&Path>> =
        left_out.map(|paths| paths.iter().map(PathBuf::as_path).collect());
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
                // No inventory, or the other side still has it but left it
                // out of its snapshot: absence is not deletion.
                if left_out
                    .as_ref()
                    .is_none_or(|left_out| left_out.contains(relative.as_path()))
                {
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
                    if !same_file(&target, &entry.path())? {
                        keep(copies.keep(&target, &relative)?);
                    }
                    std::fs::remove_file(target)?;
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
                let base = baseline.map(|root| root.join(&relative));
                let unchanged_remotely = base.as_ref().is_some_and(|base| {
                    base.is_file() && same_file(&entry.path(), base).unwrap_or(false)
                });
                if target.exists() && !same_file(&target, &entry.path())? {
                    let unchanged_locally = base
                        .as_ref()
                        .is_some_and(|base| same_file(&target, base).unwrap_or(false));
                    if unchanged_locally {
                        std::fs::copy(entry.path(), target)?;
                    } else if unchanged_remotely {
                        // Only this computer changed it: keep the local edit.
                    } else {
                        keep(copies.keep(&target, &relative)?);
                        std::fs::copy(entry.path(), target)?;
                    }
                } else if !target.exists() && unchanged_remotely {
                    // Deleted here, untouched there: the local deletion stands.
                } else {
                    std::fs::copy(entry.path(), target)?;
                }
            }
        }
    }
    Ok(kept)
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
            // Cloned first: the terminal registry has its own locks.
            let Some(agent) = lock(&state.agents).get(&id).cloned() else {
                return true;
            };
            let Some(info) = state.sessions.get(&id) else {
                return true;
            };
            crate::agent_state::tui_at_pause(
                &agent,
                info.alive,
                info.last_output_at,
                info.pid,
                state.sessions.foreground_pid(&id),
                crate::session_view::now_ms(),
            )
        }
    })
}
/// A chat running work right now: a turn in flight, input queued for the
/// next one, or background work still going. A turn parked on a permission
/// or a question waits on the user, which is not work.
fn chat_working(
    chat: &chimaera_agent::ChatInfo,
    carry: Option<&chimaera_agent::Carryover>,
    queued_input: bool,
) -> bool {
    chat.alive
        && !chat.pending_permission
        && (queued_input
            || chat.background_running > 0
            || carry.is_some_and(|carry| carry.turn_in_flight || !carry.background.is_empty()))
}

/// The agents (`claude`, `codex`, ...) running work in this project right
/// now, each named once (the coordinator's turn-end copies; leaving uses
/// [`leaving_agents`]). The inverse of `at_pause` per session, except that a session nothing is known
/// about is not counted as working.
pub(super) fn working_agents(state: &AppState, workspace: &str) -> Vec<String> {
    agents_at_work(state, workspace, false)
}
/// The agents working or waiting on the user (a permission or a question)
/// in this project: what leaving moves, since the user will want to answer
/// from a browser.
pub(super) fn leaving_agents(state: &AppState, workspace: &str) -> Vec<String> {
    agents_at_work(state, workspace, true)
}
fn agents_at_work(state: &AppState, workspace: &str, waiting_counts: bool) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    for (_, kind) in sessions_at_work(state, workspace, waiting_counts) {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}
/// The conversations a quit moves: those of a kind the cloud continues
/// (`leave::movable`) that are working or waiting on the user. A quit
/// handover carries every one of them or nothing (`SnapshotOwner::must_carry`).
pub(super) fn leaving_sessions(state: &AppState, workspace: &str) -> Vec<String> {
    sessions_at_work(state, workspace, true)
        .into_iter()
        .filter(|(_, kind)| super::leave::movable(kind))
        .map(|(id, _)| id)
        .collect()
}
/// Each session working (or, with `waiting_counts`, waiting on the user),
/// with its agent kind.
fn sessions_at_work(
    state: &AppState,
    workspace: &str,
    waiting_counts: bool,
) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for id in sessions(state, workspace) {
        let (working, kind) = if let Some(chat) = state.chat.get(&id) {
            let activity = state.chat.input_activity(&id);
            let working = chat_working(
                &chat,
                activity.as_ref().map(|(carry, _)| carry),
                activity.as_ref().is_some_and(|(_, pending)| *pending),
            ) || (waiting_counts && chat.alive && chat.pending_permission);
            let kind = lock(&state.agents)
                .get(&id)
                .map_or(chat.agent, |record| record.kind.as_str().to_owned());
            (working, kind)
        } else {
            // Cloned first: the terminal registry has its own locks.
            let record = lock(&state.agents).get(&id).cloned();
            let (Some(record), Some(info)) = (record, state.sessions.get(&id)) else {
                continue;
            };
            let working = info.alive
                && ((waiting_counts
                    && record.state == crate::agent_state::AgentState::NeedsPermission)
                    || !crate::agent_state::tui_at_pause(
                        &record,
                        info.alive,
                        info.last_output_at,
                        info.pid,
                        state.sessions.foreground_pid(&id),
                        crate::session_view::now_ms(),
                    ));
            (working, record.kind.as_str().to_owned())
        };
        if working && !kind.is_empty() && kind.len() <= 32 {
            found.push((id, kind));
        }
    }
    found
}
/// A device waits this long between automatic attempts to finish one return.
const RETURN_BACKOFF_MAX: u64 = 1800;

/// Whether the account reports this project's owner as a cloud machine that
/// is suspended but keeps ownership (placement availability `suspended`).
/// Passive: reading placement never wakes anything. Any failure reads as not
/// suspended, which keeps the earlier behavior.
async fn owner_suspended(config: &Configure, workspace: &str) -> bool {
    if config.execution.is_none() {
        return false;
    }
    let Ok(response) = account(
        config,
        &format!("/v2/workspaces/{workspace}/placement"),
        "GET",
        None,
    )
    .await
    else {
        return false;
    };
    response.json::<serde_json::Value>().is_ok_and(|placement| {
        placement["workspace_id"] == workspace && placement["availability"] == "suspended"
    })
}

/// How long the app must have been here before live cloud work moves home:
/// a short guard, so a computer opened for a moment (lid lifted and closed
/// again, a wake the user did not ask for) does not pull work home only to
/// send it straight back. Power no longer matters: an open app brings its
/// work home on battery too. A development build (the loopback end-to-end
/// harness) may change it with `CHIMAERA_PRO_SETTLE_SECS`, up to five
/// minutes; release builds ignore the variable.
pub(super) fn settle_seconds() -> u64 {
    static SETTLE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *SETTLE.get_or_init(|| {
        settle_override(
            chimaera_core::is_dev_build(),
            std::env::var("CHIMAERA_PRO_SETTLE_SECS").ok().as_deref(),
        )
    })
}
fn settle_override(dev: bool, value: Option<&str>) -> u64 {
    const SETTLE: u64 = 20;
    value
        .filter(|_| dev)
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(SETTLE, |seconds| seconds.min(300))
}

pub(super) async fn lazy_handback(state: &Arc<AppState>, config: &Configure) -> Result<()> {
    if config.delegation.workspace.is_some() || config.role != Role::Device {
        return Ok(());
    }
    // Live cloud work moves home once the app has been here for the short
    // guard (`settle_seconds`), at the conversation's next pause (the cloud
    // refuses a hand-back while busy, and the next pass asks again); work
    // the cloud is not running returns at once (laptop first). A project
    // the cloud did not take or cannot run after the app left comes back at
    // once, app or not (`leave::take_back`).
    let settled = super::leave::app_settled(state);
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
        if super::project_copy::copy_only(state, &workspace) {
            continue;
        }
        // Only a project already registered on this device may return automatically.
        // Discovery and old global-folder preferences never authorize adoption.
        if lock(&state.workspaces).get(&workspace).is_none()
            || super::projects::adoption_pending(state, &workspace)
            || !super::projects::account_matches(state, &workspace)
        {
            continue;
        }
        // Handed to the cloud when the app quit: it stays there, live or
        // released, until the app returns (`/pro/wake`). A project being
        // brought here because the user acted on it is that request's.
        let reclaiming = lock(&state.pro.reclaim).contains(&workspace);
        if (super::parked(state, &workspace) && !reclaiming)
            || super::moves::pulling(state, &workspace)
        {
            continue;
        }
        // The account's preferred installation is the latest computer that
        // had the project; one the user opened it on since may pull it home
        // too, once nothing live holds it (the settle rule below still
        // decides moving live cloud work).
        if !(execution::preferred_here(state, config, &workspace)
            || execution::opened_here(state, &workspace))
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
            // A cloud machine asleep with ownership reads expired too, but the
            // account refuses to let anyone else acquire it (409 `held`): it
            // must be woken and asked to hand back, like live cloud work.
            let suspended = holder.is_some()
                && baton.holder_id.is_some()
                && !mine
                && execution::expired(&baton)
                && owner_suspended(&operation_config, &workspace).await;
            let target = match (&holder, baton.holder_id.as_deref()) {
                // Released by the cloud: nothing runs there, hydrate now.
                (_, None) => Some(baton.epoch),
                // A return this device already acquired did not finish.
                (None, Some(_)) if mine && baton.epoch == epoch => Some(epoch),
                (None, _) => None,
                // The cloud's lease lapsed (it stopped or lost the account):
                // take the project home from its last acknowledged checkpoint.
                (Some(_), Some(_)) if !mine && execution::expired(&baton) && !suspended => {
                    Some(baton.epoch)
                }
                (Some(recorded), Some(current))
                    if current == recorded && (settled || reclaiming) =>
                {
                    if hosts.is_none() {
                        let response = transport::request(
                            &config.keeper_url,
                            "/v1/hosts",
                            "GET",
                            &config.delegation.access_token,
                            None,
                        )
                        .await
                        .context("Could not reconnect to your saved work")?;
                        // The account is down: the keeper holds on and says
                        // so. A quiet wait; the next pass asks again.
                        if transport::account_unavailable(&response) {
                            return Ok(());
                        }
                        hosts = Some(
                            response
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
                status.error_code = None;
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                lock(&state.pro.return_backoff).remove(&workspace);
                if super::owned_epoch(state, &workspace).is_some()
                    && lock(&state.pro.reclaim).remove(&workspace)
                {
                    super::leave::back_here(state, config, &workspace).await;
                }
            }
            Err(error) => {
                {
                    let mut backoff = lock(&state.pro.return_backoff);
                    if backoff.len() >= 128 && !backoff.contains_key(&workspace) {
                        backoff.clear();
                    }
                    // This computer's own unfinished return keeps its project
                    // fenced here, so it retries quickly (15 s doubling to two
                    // minutes); moving cloud work home can wait longer.
                    let (first, most) = if holder.is_none() {
                        (15, 120)
                    } else {
                        (120, RETURN_BACKOFF_MAX)
                    };
                    let delay = backoff
                        .get(&workspace)
                        .map_or(first, |(_, delay)| (delay * 2).min(most));
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
) -> Result<()> {
    finish_hydration_checked(
        state,
        workspace,
        epoch,
        generation,
        super::provider_gate::check(state, workspace, true),
    )
    .await
}

async fn finish_hydration_checked(
    state: &Arc<AppState>,
    workspace: &str,
    epoch: u64,
    generation: u64,
    providers: impl std::future::Future<Output = Vec<super::provider_gate::BlockedProvider>>,
) -> Result<()> {
    {
        let _configuration = state.pro.configuration.lock().await;
        ensure!(
            generation == state.pro.generation.load(Ordering::Acquire),
            "Account changed during project setup"
        );
        {
            let mut ownership = lock(&state.pro.ownership);
            ensure!(
                matches!(ownership.get(workspace),Some(Ownership::Hydrating{epoch:current} | Ownership::SettingUp{epoch:current}) if *current==epoch),
                "Project ownership changed before setup"
            );
            ownership.insert(workspace.into(), Ownership::SettingUp { epoch });
        }
        super::persist(state).await?;
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
    // A provider that is not signed in holds back only its own sessions: the
    // project and every other conversation continue (paused rows name it).
    super::provider_gate::record(state, workspace, blocked.clone());
    {
        let mut ownership = lock(&state.pro.ownership);
        ensure!(
            matches!(ownership.get(workspace), Some(Ownership::SettingUp { epoch: current }) if *current == epoch),
            "Project ownership changed before resume"
        );
        ownership.insert(workspace.into(), Ownership::Local { epoch });
    }
    super::persist(state).await?;
    if blocked.is_empty() {
        if let Some(status) = lock(&state.pro.status).get_mut(workspace) {
            status.error = None;
            status.error_code = None;
        }
    }
    // Each managed child takes this lock for durable launch admission. Release
    // it before restoring sessions; their admission rechecks the current grant.
    drop(_configuration);
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed before project resume"
    );
    let held_back = !blocked.is_empty();
    if execution::resume_allowed(state, workspace) {
        // Its own task: this usually runs in the mirror task, which sign-out
        // aborts (`stop_tasks`), and a respawn cut half way would leave the
        // returned sessions deferred as "moved". The caller still waits.
        let owner = state.clone();
        let workspace = workspace.to_owned();
        tokio::spawn(async move {
            crate::ledger::resume_deferred_filtered(&owner, &workspace, |entry| {
                !super::provider_gate::waits_for_provider(entry, &blocked)
            })
            .await
        })
        .await
        .context("project resume stopped")??;
    }
    // On the cloud machine: say whether it runs the work that arrived.
    super::leave::arrived(state, workspace, held_back);
    Ok(())
}

#[cfg(test)]
#[path = "continuity_tests.rs"]
pub(super) mod continuity_tests;
#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;

pub(super) fn eligible(state: &AppState, workspace: &crate::workspaces::Workspace) -> bool {
    if super::project_copy::copy_only(state, &workspace.id) {
        return false;
    }
    if authority::registered_root(state, &workspace.id, &workspace.root).is_err() {
        return false;
    }
    if workspace.hidden
        || crate::cloud::is_onboarding_workspace(workspace)
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
    async fn transfer_overflow_preserves_work_and_plain_shells_never_hide_a_live_agent() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-cap-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("work.txt"), b"local work").unwrap();
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
                hidden: false,
            })
            .unwrap();
        lock(&state.pro.ownership).insert("w-project".into(), Ownership::Local { epoch: 7 });
        {
            let mut registry = lock(&state.session_workspaces);
            for index in 0..65 {
                registry.insert(format!("s-fixture-{index}"), "w-project".into());
            }
        }
        // Pick the entry the previous take(64)-before-filter implementation
        // omitted, without depending on HashMap's random iteration order.
        let id = lock(&state.session_workspaces)
            .keys()
            .last()
            .unwrap()
            .clone();
        let session = state
            .sessions
            .spawn(chimaera_pty::SpawnOpts {
                cwd: root.clone(),
                name: None,
                cols: 80,
                rows: 24,
                command: Some(vec!["/bin/sleep".into(), "30".into()]),
                id: Some(id.clone()),
                env: vec![],
                env_remove: vec![],
                scrollback: None,
            })
            .unwrap();
        let mut agent =
            crate::agents::AgentRecord::new("fixture".into(), crate::agents::AgentKind::Claude);
        agent.state = crate::agent_state::AgentState::Running;
        lock(&state.agents).insert(id.clone(), agent);
        assert!(sessions(&state, "w-project") == vec![id.clone()]);
        assert!(live_agents(&state, "w-project"));
        assert!(!at_pause(&state, "w-project"));
        assert!(!working_agents(&state, "w-project").is_empty());
        assert!(transfer_session_ids(&state, "w-project").is_err());
        let config:Configure = serde_json::from_value(json!({"endpoint":"http://127.0.0.1:1","keeper_url":"","delegation":{"access_token":"fixture","expires_at":"2099-01-01T00:00:00Z","scope":["baton","mirror"],"device_id":"device"}})).unwrap();
        let error = snapshot(&state, &config, "w-project", true)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("at most 64 sessions"));
        assert!(state
            .sessions
            .get(&session.id)
            .is_some_and(|info| info.alive));
        assert!(matches!(
            lock(&state.pro.ownership).get("w-project"),
            Some(Ownership::Local { epoch: 7 })
        ));
        assert_eq!(std::fs::read(root.join("work.txt")).unwrap(), b"local work");
        assert!(!state.pro.root.join("w-project/working-tree.git").exists());
        let omitted_shell = lock(&state.session_workspaces)
            .keys()
            .find(|candidate| **candidate != id)
            .unwrap()
            .clone();
        lock(&state.session_workspaces).remove(&omitted_shell);
        assert_eq!(transfer_session_ids(&state, "w-project").unwrap().len(), 64);
        state.sessions.kill(&session.id).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Sign-out aborts the mirror task that finishes a return. The returned
    /// sessions that finish was respawning start anyway: a respawn cut half
    /// way would leave them deferred, answering "moved" forever.
    #[tokio::test]
    async fn aborting_a_finished_return_still_resumes_its_sessions() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "chimaera-return-abort-{}",
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
                hidden: false,
            })
            .unwrap();
        let claude = root.join("claude");
        std::fs::write(
            &claude,
            "#!/bin/sh\n\
             printf '%s\\n' '{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"init\",\"response\":{\"commands\":[]}}}'\n\
             cat >/dev/null\n",
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        lock(&state.agent_bins).insert(
            crate::agents::AgentKind::Claude,
            crate::launcher::AgentDetection {
                path: Ok(claude),
                version: Some("9.9.9-fake".into()),
                managed: false,
                explicit: true,
                mtime: None,
            },
        );
        // An enrolled project: each agent launch waits for launch admission.
        super::super::install_execution_fixture(&state, "w-project", 3).unwrap();
        lock(&state.pro.ownership).insert("w-project".into(), Ownership::SettingUp { epoch: 3 });
        crate::ledger::defer(
            &state,
            crate::ledger::LedgerEntry {
                id: "s-returned".into(),
                suspended: true,
                manual_resume_reason: None,
                handoff: Some(crate::bundle::HandoffResume {
                    fork: false,
                    origin: crate::bundle::Origin::Home,
                    epoch: 3,
                }),
                workspace_id: "w-project".into(),
                cwd: root.clone(),
                pinned_name: None,
                cols: 80,
                rows: 24,
                theme: "dark".into(),
                created_at: 1,
                agent: Some(crate::ledger::LedgerAgent {
                    kind: crate::agents::AgentKind::Claude,
                    resume: None,
                    transcript: None,
                    native_cwd: None,
                    title: "Fixture".into(),
                    ui: chimaera_agent::model::SessionUi::Chat,
                    model: None,
                    carryover: None,
                }),
            },
        )
        .unwrap();
        let owner = state.clone();
        let task = tokio::spawn(async move { finish_hydration(&owner, "w-project", 3, 0).await });
        // Queued while the finish makes the project Local, so it is handed
        // over the moment the finish releases it: the resume has started
        // and waits for this admission when the task is aborted.
        while super::super::owned_epoch(&state, "w-project").is_none() {
            tokio::task::yield_now().await;
        }
        let admission = state.pro.configuration.lock().await;
        task.abort();
        let _ = task.await;
        drop(admission);
        tokio::time::timeout(Duration::from_secs(10), async {
            while !state.chat.get("s-returned").is_some_and(|chat| chat.alive)
                || lock(&state.deferred_sessions).contains_key("s-returned")
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the aborted finish's resume completes on its own");
        state.chat.kill("s-returned");
        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn three_way_return_preserves_local_conflicts_and_applies_unmodified_files() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
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
        let (count, kept) = install_tree(&cloud, &local, Some(&base), Some(&[])).unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            std::fs::read_to_string(local.join("same.txt")).unwrap(),
            "cloud"
        );
        // Both changed: the incoming version takes the path and the user's
        // own version sits right beside it; the report names the copy.
        assert_eq!(
            std::fs::read_to_string(local.join("conflict.txt")).unwrap(),
            "cloud"
        );
        assert!(!local.join("removed.txt").exists());
        assert!(!local.join("removed-but-edited.txt").exists());
        for (original, body) in [
            ("conflict.txt", "local"),
            ("removed-but-edited.txt", "local"),
        ] {
            let copy = kept
                .iter()
                .find(|path| {
                    path.to_string_lossy()
                        .starts_with(&format!("{original}.mine-"))
                })
                .unwrap_or_else(|| panic!("{original}: {kept:?}"));
            assert_eq!(std::fs::read_to_string(local.join(copy)).unwrap(), body);
            assert!(
                !super::super::policy::allowed_path(copy),
                "a kept copy stays on this computer"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn canonical_return_uses_cloud_files_and_preserves_unpublished_local_edits() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "chimaera-canonical-return-{}",
            chimaera_core::generate_token()
        ));
        let local = root.join("local");
        let cloud = root.join("cloud");
        let base = root.join("base");
        for dir in [&local, &cloud, &base] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for name in ["changed", "deleted"] {
            std::fs::write(base.join(name), "base").unwrap();
            std::fs::write(local.join(name), "unpublished local").unwrap();
        }
        std::fs::write(cloud.join("changed"), "canonical cloud").unwrap();
        let kept = install_tree(&cloud, &local, Some(&base), Some(&[])).unwrap();
        assert_eq!(kept.0, 2);
        assert_eq!(
            std::fs::read_to_string(local.join("changed")).unwrap(),
            "canonical cloud"
        );
        assert!(!local.join("deleted").exists());
        let mut preserved: Vec<_> = kept
            .1
            .iter()
            .map(|copy| std::fs::read_to_string(local.join(copy)).unwrap())
            .collect();
        preserved.sort();
        assert_eq!(preserved, vec!["unpublished local", "unpublished local"]);
        assert!(
            kept.1
                .iter()
                .all(|copy| !super::super::policy::allowed_path(copy)),
            "kept copies are never published as canonical project files"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod return_tests {
    use super::*;
    fn tree(root: &Path, files: &[(&str, &str)]) -> PathBuf {
        std::fs::create_dir_all(root).unwrap();
        for (name, body) in files {
            std::fs::write(root.join(name), body).unwrap();
        }
        root.to_path_buf()
    }
    #[test]
    fn edits_since_the_last_published_snapshot_survive_a_return() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-return-edits-{}",
            chimaera_core::generate_token()
        ));
        // Published T0; the laptop then edited notes (its T1 push failed) and
        // the cloud, continuing from T0, changed only report.
        let base = tree(&root.join("base"), &[("notes", "t0"), ("report", "t0")]);
        let local = tree(
            &root.join("local"),
            &[
                ("notes", "edited after t0"),
                ("report", "t0"),
                ("new", "local only"),
            ],
        );
        let cloud = tree(&root.join("cloud"), &[("notes", "t0"), ("report", "cloud")]);
        let kept = install_tree(&cloud, &local, Some(&base), Some(&[])).unwrap();
        assert_eq!(kept.0, 0, "no conflict: each side changed different files");
        assert_eq!(
            std::fs::read_to_string(local.join("notes")).unwrap(),
            "edited after t0"
        );
        assert_eq!(
            std::fs::read_to_string(local.join("report")).unwrap(),
            "cloud"
        );
        assert_eq!(
            std::fs::read_to_string(local.join("new")).unwrap(),
            "local only"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn a_file_left_out_of_the_snapshot_is_never_deleted_locally() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-return-left-out-{}",
            chimaera_core::generate_token()
        ));
        for (label, left_out, deleted) in [
            (
                "excluded",
                Some(vec![PathBuf::from("secret-looking")]),
                false,
            ),
            ("legacy", None, false),
            ("deleted", Some(vec![]), true),
        ] {
            let base = tree(
                &root.join(label).join("base"),
                &[("secret-looking", "same")],
            );
            let local = tree(
                &root.join(label).join("local"),
                &[("secret-looking", "same")],
            );
            let cloud = tree(&root.join(label).join("cloud"), &[]);
            install_tree(&cloud, &local, Some(&base), left_out.as_deref()).unwrap();
            assert_eq!(!local.join("secret-looking").exists(), deleted, "{label}");
        }
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
