//! Optional daemon-owned mirrors and workspace handoff. No credential is durable.
mod authority;
mod canonical;
mod config;
mod detached;
mod drain;
mod engine;
mod execution;
mod mirror;
mod policy;
mod projects;
mod protocol;
mod provider_gate;
mod repository;
mod routes;
mod shadow_cache;
mod transport;
pub(crate) use drain::{cancel as cancel_drain, start as drain};
pub(crate) use policy::CloudProfile;
pub(crate) use provider_gate::{cloud_provider_blocks, workspace_provider_blocks};
pub(crate) use routes::*;
tokio::task_local! { static PROFILE_SETUP: (String, u64); }

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64},
        Arc, Mutex, Weak,
    },
};
use tokio::sync::Mutex as AsyncMutex;

pub(crate) struct ProState {
    root: PathBuf,
    configured: AtomicBool,
    worker: AtomicBool,
    generation: AtomicU64,
    runtime: Mutex<Option<protocol::Configure>>,
    authority: Mutex<authority::Authority>,
    execution: execution::State,
    ownership: Mutex<HashMap<String, Ownership>>,
    preferences: Mutex<HashMap<String, Preference>>,
    projects_root: Mutex<Option<PathBuf>>,
    adoptions: Mutex<HashMap<String, projects::Destination>>,
    legacy_pending: Mutex<std::collections::HashSet<String>>,
    project_cache: Mutex<projects::Cache>,
    discovery: AsyncMutex<()>,
    status: Mutex<HashMap<String, WorkspaceStatus>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    mirror_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    jobs: Arc<AsyncMutex<()>>,
    /// Advanced by every sleep and wake: a flush started for an older sleep
    /// keeps its publication but never releases a project after the wake.
    sleep_generation: AtomicU64,
    sleeping: Mutex<std::collections::HashSet<String>>,
    /// The last bytes written, so an unchanged tick costs no disk sync.
    persistence: AsyncMutex<Option<Vec<u8>>>,
    configuration: Arc<AsyncMutex<()>>,
    caches: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
    boot_deferred: Mutex<std::collections::HashSet<String>>,
    operations: detached::Operations,
    drain: Mutex<Option<drain::Drain>>,
    remote_since: Mutex<HashMap<String, u64>>,
    return_backoff: Mutex<HashMap<String, (u64, u64)>>,
    awake_since: AtomicU64,
    power_suitable: AtomicBool,
    /// Wakes the lease loop at once: a resumed machine renews before fencing.
    renew_now: tokio::sync::Notify,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Ownership {
    PrivacyDisabled { epoch: u64 },
    Hydrating { epoch: u64 },
    SettingUp { epoch: u64 },
    AwaitingVerification { epoch: u64 },
    Local { epoch: u64 },
    Remote { epoch: u64, holder: String },
    Transferring { epoch: u64 },
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Preference {
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    continuity: Option<execution::wire::Continuity>,
    #[serde(default)]
    execution_uncertain: bool,
    #[serde(default)]
    execution_active: bool,
    #[serde(default)]
    execution_boot: Option<String>,
    /// Process groups of live managed agents, for a same-boot successor's
    /// probe after a crash (bounded to 64 per workspace).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    execution_groups: Vec<u32>,
    #[serde(default)]
    execution_identity: Option<execution::wire::Identity>,
    #[serde(default)]
    recovery_pending: bool,
    #[serde(default)]
    never_mirror: bool,
    #[serde(default)]
    privacy_pending: bool,
    #[serde(default)]
    git_branches: Vec<String>,
    /// The working-tree commit of the last acknowledged publication: the
    /// three-way baseline when work returns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    published_tree: Option<String>,
    #[serde(default)]
    profile: policy::CloudProfile,
}
#[derive(Clone, Default, Serialize)]
struct WorkspaceStatus {
    #[serde(flatten)]
    report: mirror::Report,
    last_mirrored_at: Option<u64>,
    storage_limit_bytes: u64,
    error: Option<String>,
    /// Additive: a stable code for `error` (see `routes::error_code`).
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<&'static str>,
    /// Additive: files the last return kept in both versions, and up to 32
    /// of their project-relative paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    kept_both: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    kept_paths: Vec<PathBuf>,
    #[serde(skip)]
    blocked_providers: Vec<provider_gate::BlockedProvider>,
}
#[derive(Default, Serialize, Deserialize)]
struct DiskState {
    #[serde(default)]
    provider_blocks: HashMap<String, Vec<provider_gate::BlockedProvider>>,
    #[serde(default)]
    projects_root: Option<PathBuf>,
    #[serde(default)]
    adoptions: HashMap<String, projects::Destination>,
    #[serde(default)]
    legacy_pending: std::collections::HashSet<String>,
    #[serde(default, skip_serializing)]
    import_roots: HashMap<String, PathBuf>,
    /// Once configured as a cloud worker, this installation stays strict
    /// across restarts even before its supervisor configures it again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    worker: bool,
    ownership: HashMap<String, Ownership>,
    preferences: HashMap<String, Preference>,
}
impl ProState {
    fn cache(&self, workspace: &str) -> anyhow::Result<Arc<AsyncMutex<()>>> {
        anyhow::ensure!(valid_id(workspace), "invalid workspace cache identity");
        let mut caches = crate::lock(&self.caches);
        caches.retain(|_, cache| cache.strong_count() > 0);
        if let Some(cache) = caches.get(workspace).and_then(Weak::upgrade) {
            return Ok(cache);
        }
        anyhow::ensure!(
            caches.len() < 128,
            "workspace cache capacity is busy; retry shortly"
        );
        let cache = Arc::new(AsyncMutex::new(()));
        caches.insert(workspace.into(), Arc::downgrade(&cache));
        Ok(cache)
    }
    pub(crate) fn new(root: PathBuf) -> Self {
        // Construction already happens on the daemon's startup blocking path.
        // A capped record can gate restore without needing an account token.
        let path = root.join("state.json");
        let read = || -> std::io::Result<Option<Vec<u8>>> {
            use std::io::Read;
            let file = match std::fs::File::open(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                other => other?,
            };
            let mut bytes = Vec::new();
            file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        };
        // Only an existing file that does not parse (or exceeds its cap) is
        // damage. An I/O error (an NFS home briefly answering EIO/ESTALE) is
        // retried; if it persists the state is unknown and fails closed, but
        // the file is left in place.
        let mut attempts = 0;
        let bytes = loop {
            match read() {
                Err(_) if attempts < 4 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                result => break result,
            }
        };
        let (loaded, unknown) = match bytes {
            Ok(None) => (Some(DiskState::default()), false),
            Ok(Some(bytes)) => {
                let parsed = (bytes.len() <= 1024 * 1024)
                    .then(|| serde_json::from_slice::<DiskState>(&bytes).ok())
                    .flatten();
                // Unreadable ownership state fails closed: enrolled projects
                // are verified again rather than silently losing their fences.
                // The damaged copy is kept (one slot) instead of overwritten.
                if parsed.is_none() {
                    let _ = std::fs::rename(&path, root.join("state.json.damaged"));
                }
                let damaged = parsed.is_none();
                (parsed, damaged)
            }
            Err(_) => (None, true),
        };
        let disk = loaded.unwrap_or_default();
        let legacy_pending = disk
            .legacy_pending
            .into_iter()
            .chain(disk.import_roots.into_keys())
            .filter(|id| valid_id(id))
            .take(128)
            .collect();
        let ownership: HashMap<_, _> = disk
            .ownership
            .into_iter()
            .take(128)
            .map(|(id, owner)| {
                let owner = match owner {
                    Ownership::Local { epoch } | Ownership::Transferring { epoch } => {
                        Ownership::AwaitingVerification { epoch }
                    }
                    owner => owner,
                };
                (id, owner)
            })
            .collect();
        let status = disk
            .provider_blocks
            .into_iter()
            .take(128)
            .filter_map(|(id, blocks)| {
                let blocked_providers = provider_gate::restored(blocks);
                (matches!(ownership.get(&id), Some(Ownership::SettingUp { .. }))
                    && !blocked_providers.is_empty())
                .then_some((
                    id,
                    WorkspaceStatus {
                        blocked_providers,
                        error: Some("cloud_provider_not_ready".into()),
                        error_code: Some("cloud_provider_not_ready"),
                        ..Default::default()
                    },
                ))
            })
            .collect();
        let authority = authority::Authority::load(&root);
        let execution = execution::State::restore(
            &root,
            &disk.preferences,
            disk.worker || crate::cloud::enabled(),
            unknown,
        );
        Self {
            root,
            configured: AtomicBool::new(false),
            worker: AtomicBool::new(disk.worker),
            generation: AtomicU64::new(0),
            runtime: Mutex::new(None),
            authority: Mutex::new(authority),
            execution,
            ownership: Mutex::new(ownership),
            preferences: Mutex::new(disk.preferences.into_iter().take(128).collect()),
            projects_root: Mutex::new(disk.projects_root),
            adoptions: Mutex::new(disk.adoptions.into_iter().take(128).collect()),
            legacy_pending: Mutex::new(legacy_pending),
            project_cache: Mutex::new(projects::Cache::default()),
            discovery: AsyncMutex::new(()),
            status: Mutex::new(status),
            task: Mutex::new(None),
            mirror_task: Mutex::new(None),
            jobs: Arc::new(AsyncMutex::new(())),
            sleep_generation: AtomicU64::new(0),
            sleeping: Mutex::new(Default::default()),
            persistence: AsyncMutex::new(None),
            configuration: Arc::new(AsyncMutex::new(())),
            caches: Mutex::new(HashMap::new()),
            boot_deferred: Mutex::new(Default::default()),
            operations: Default::default(),
            drain: Mutex::new(None),
            remote_since: Mutex::new(HashMap::new()),
            return_backoff: Mutex::new(HashMap::new()),
            awake_since: AtomicU64::new(now()),
            power_suitable: AtomicBool::new(false),
            renew_now: tokio::sync::Notify::new(),
        }
    }
}

/// Only a verified ownership transition or an explicit clean handoff fences a
/// device's writer; a cloud worker additionally waits for restart
/// verification. Connectivity loss never pauses local work.
pub(crate) fn may_write(state: &crate::AppState, workspace: &str) -> bool {
    if authority::workspace(state, workspace).is_err()
        || (crate::lock(&state.pro.authority).restricted()
            && !state
                .pro
                .configured
                .load(std::sync::atomic::Ordering::Acquire))
    {
        return false;
    }
    if matches!(
        crate::lock(&state.pro.ownership).get(workspace),
        Some(Ownership::SettingUp { .. })
    ) {
        return PROFILE_SETUP
            .try_with(|(id, generation)| {
                id == workspace
                    && *generation
                        == state
                            .pro
                            .generation
                            .load(std::sync::atomic::Ordering::Acquire)
            })
            .unwrap_or(false);
    }
    if crate::lock(&state.pro.legacy_pending).contains(workspace) {
        return false;
    }
    match crate::lock(&state.pro.ownership).get(workspace) {
        Some(
            Ownership::Remote { .. }
            | Ownership::PrivacyDisabled { .. }
            | Ownership::Hydrating { .. }
            | Ownership::SettingUp { .. }
            | Ownership::Transferring { .. },
        ) => false,
        // Unverified after a restart, wake or failed flush: a device keeps
        // working until an authenticated read shows another owner.
        Some(Ownership::AwaitingVerification { .. }) => !execution::worker(state),
        _ => true,
    }
}
#[cfg(test)]
pub(crate) use execution::expired_lease_fixture as expire_execution_fixture;
/// Execution has a stricter lease boundary than local file editing.
#[cfg(test)]
pub(crate) use execution::install_fixture as install_execution_fixture;
pub(crate) use execution::mutation;
pub(crate) use execution::prepare_launch as prepare_managed_launch;
pub(crate) use execution::recovery_context as checkpoint_recovery_context;
#[cfg(test)]
pub(crate) use execution::remote_owner_fixture as install_remote_owner_fixture;
pub(crate) fn managed_execution(state: &crate::AppState, workspace: &str) -> bool {
    execution::managed(state, workspace)
}
pub(crate) fn may_execute(state: &crate::AppState, workspace: &str) -> bool {
    may_write(state, workspace) && execution::allows(state, workspace)
}
/// Sessions left by a previous daemon wait for this life's ownership proof, so
/// a project the cloud took over while this computer was off never resumes a
/// stale turn here. New work is not held back (`may_execute`).
pub(crate) fn may_restore(state: &crate::AppState, workspace: &str) -> bool {
    may_execute(state, workspace)
        && (!execution::managed(state, workspace) || execution::restorable(state, workspace))
}
/// How long a personal device waits for the account to confirm ownership
/// before resuming its own interrupted sessions anyway (laptop first).
pub(crate) const BOOT_VERIFICATION_GRACE: std::time::Duration = std::time::Duration::from_secs(60);
pub(crate) fn defer_boot_session(state: &crate::AppState, session: &str) {
    let mut deferred = crate::lock(&state.pro.boot_deferred);
    if deferred.len() < 512 {
        deferred.insert(session.to_owned());
    }
}
/// Restart-deferred sessions on a device resume when ownership was not
/// verified in time, unless another owner was verified meanwhile or old
/// processes may still be running. Sessions a clean handoff suspended stay
/// suspended; they belong to whoever now owns the project.
pub(crate) async fn resume_unverified(state: &std::sync::Arc<crate::AppState>) {
    if execution::worker(state) {
        return;
    }
    let pending: Vec<String> = crate::lock(&state.pro.boot_deferred).drain().collect();
    let mut workspaces: HashMap<String, Vec<String>> = HashMap::new();
    for id in pending {
        let workspace = crate::lock(&state.deferred_sessions)
            .get(&id)
            .map(|entry| entry.workspace_id.clone());
        if let Some(workspace) = workspace {
            workspaces.entry(workspace).or_default().push(id);
        }
    }
    for (workspace, ids) in workspaces {
        if !may_execute(state, &workspace) || execution::unclean(state, &workspace) {
            continue;
        }
        if let Err(error) = crate::ledger::resume_deferred_sessions(state, &workspace, &ids).await {
            tracing::warn!(%error, "Interrupted sessions could not resume");
        }
    }
}
pub(crate) fn validate_execution_scope(
    state: &crate::AppState,
    workspace: &str,
    epoch: u64,
) -> anyhow::Result<()> {
    authority::workspace(state, workspace)?;
    anyhow::ensure!(
        execution::managed(state, workspace)
            && may_execute(state, workspace)
            && execution::epoch(state, workspace) == Some(epoch),
        "workspace execution authority changed"
    );
    Ok(())
}
pub(crate) fn owned_epoch(state: &crate::AppState, workspace: &str) -> Option<u64> {
    match crate::lock(&state.pro.ownership).get(workspace) {
        Some(Ownership::Local { epoch }) => Some(*epoch),
        _ => None,
    }
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
async fn persist(state: &crate::AppState) -> anyhow::Result<()> {
    let mut written = state.pro.persistence.lock().await;
    ensure_root(&state.pro.root).await?;
    execution::record_groups(state);
    let ownership = crate::lock(&state.pro.ownership).clone();
    let preferences = crate::lock(&state.pro.preferences).clone();
    let projects_root = crate::lock(&state.pro.projects_root).clone();
    let adoptions = crate::lock(&state.pro.adoptions).clone();
    let legacy_pending = crate::lock(&state.pro.legacy_pending).clone();
    let provider_blocks = crate::lock(&state.pro.status)
        .iter()
        .filter(|(_, status)| !status.blocked_providers.is_empty())
        .take(128)
        .map(|(id, status)| (id.clone(), status.blocked_providers.clone()))
        .collect();
    let bytes = serde_json::to_vec(&DiskState {
        provider_blocks,
        legacy_pending,
        import_roots: HashMap::new(),
        adoptions,
        projects_root,
        worker: state.pro.worker.load(std::sync::atomic::Ordering::Acquire),
        ownership,
        preferences,
    })?;
    anyhow::ensure!(bytes.len() <= 1024 * 1024, "mirror settings exceed limit");
    // State first, then the enrollment latch: a crash between them leaves a
    // policy the latch does not list yet (restored as enrolled), never a latch
    // naming a workspace whose policy is missing (which fails closed).
    if written.as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    let path = state.pro.root.join("state.json");
    let copy = bytes.clone();
    tokio::task::spawn_blocking(move || crate::persist::atomic_write_json_durable(&path, copy))
        .await??;
    execution::persist_latch(state).await?;
    *written = Some(bytes);
    Ok(())
}

pub(crate) fn may_import(state: &crate::AppState, workspace: &str, epoch: u64) -> bool {
    if authority::workspace(state, workspace).is_err()
        || (crate::lock(&state.pro.authority).restricted()
            && !state
                .pro
                .configured
                .load(std::sync::atomic::Ordering::Acquire))
    {
        return false;
    }
    match crate::lock(&state.pro.ownership).get(workspace) {
        Some(Ownership::Local { epoch: current } | Ownership::Hydrating { epoch: current }) => {
            *current == epoch
        }
        None => !state
            .pro
            .configured
            .load(std::sync::atomic::Ordering::Acquire),
        _ => false,
    }
}

/// Commands known to require the laptop are retained in the mirrored profile.
/// This hook handles API/MCP execution; journal learning covers typed commands.
pub(crate) async fn defer_command(
    state: &std::sync::Arc<crate::AppState>,
    session: &str,
    command: &str,
) -> anyhow::Result<bool> {
    let worker = crate::lock(&state.pro.runtime)
        .as_ref()
        .is_some_and(|config| config.role == protocol::Role::Worker);
    if !worker {
        return Ok(false);
    }
    let Some(workspace) = crate::lock(&state.session_workspaces).get(session).cloned() else {
        return Ok(false);
    };
    authority::workspace(state, &workspace)?;
    let should_defer = {
        let mut preferences = crate::lock(&state.pro.preferences);
        let profile = &mut preferences.entry(workspace).or_default().profile;
        profile.observe_command(command);
        let known = profile.laptop_only.iter().any(|entry| entry == command);
        if known && !profile.deferred.iter().any(|entry| entry == command) {
            anyhow::ensure!(profile.deferred.len() < 64, "deferred step limit");
            profile.deferred.push(command.into());
        }
        known
    };
    if should_defer {
        persist(state).await?;
    }
    Ok(should_defer)
}

/// Leftovers of transfers a previous daemon life never finished: staging
/// copies and Git locks per project (each under its cache guard, so a transfer
/// that starts meanwhile is never touched) and old temporary bundle archives.
pub(crate) fn sweep_leftovers(state: &std::sync::Arc<crate::AppState>) {
    let owner = state.clone();
    tokio::spawn(async move {
        let root = owner.pro.root.clone();
        let projects = tokio::task::spawn_blocking(move || {
            std::fs::read_dir(root)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                        .filter_map(|entry| entry.file_name().into_string().ok())
                        .filter(|name| valid_id(name))
                        .take(256)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        for workspace in projects {
            let Ok(cache) = owner.pro.cache(&workspace) else {
                continue;
            };
            let _guard = cache.lock().await;
            if transport::cache_quiescent(&workspace).is_err() {
                continue;
            }
            let directory = owner.pro.root.join(&workspace);
            let _ =
                tokio::task::spawn_blocking(move || mirror::clear_interrupted(&directory)).await;
        }
        crate::bundle::sweep_temporary(&owner).await;
    });
}

/// A paused row's name where nothing better exists, in words for where it is
/// shown: a cloud machine holds terminals that stay with your computer; a
/// computer shows work the cloud is continuing, or work about to resume.
pub(crate) fn paused_label(
    state: &crate::AppState,
    entry: &crate::ledger::LedgerEntry,
) -> &'static str {
    if execution::worker(state) {
        return if entry.agent.is_some() {
            "Starting here"
        } else {
            "Terminal on your computer"
        };
    }
    match crate::lock(&state.pro.ownership).get(&entry.workspace_id) {
        Some(Ownership::Remote { .. }) => "Continuing in the cloud",
        _ => "Paused",
    }
}

/// Graceful daemon stop: clear managed-execution evidence once this life's
/// agents are proven stopped, so a same-boot successor is not fenced.
pub(crate) async fn shutdown(state: &std::sync::Arc<crate::AppState>) {
    if let Err(error) = execution::shutdown(state).await {
        tracing::warn!(%error, "Project execution state could not be saved at shutdown");
    }
}

/// The bounded return report: how many files a return kept in both versions
/// and which (up to 32, project-relative). `/pro/status` carries it on the
/// project's mirror row so the Pro page can say "Kept both versions of N files".
fn return_report(state: &crate::AppState, workspace: &str, kept: (usize, Vec<PathBuf>)) {
    let mut statuses = crate::lock(&state.pro.status);
    let status = statuses.entry(workspace.into()).or_default();
    status.kept_both = (kept.0 > 0).then_some(kept.0);
    status.kept_paths = kept.1;
    drop(statuses);
    state.changes.notify_waiters();
}

/// Work in flight that an idle decision must wait for: the job reservation
/// (unless a drain itself holds it), transfer tasks, sleep flushes, held
/// project caches (finalizers keep theirs past a canceled caller) and Git
/// helpers. A completed drain reports zero.
pub(crate) fn active_operations(state: &crate::AppState) -> usize {
    project_operations(state) + transport::helpers_busy()
}
fn project_operations(state: &crate::AppState) -> usize {
    let draining = drain::draining(state);
    usize::from(!draining && state.pro.jobs.try_lock().is_err())
        + detached::running(state)
        + crate::lock(&state.pro.sleeping).len()
        + crate::lock(&state.pro.caches)
            .values()
            .filter(|cache| cache.strong_count() > 0)
            .count()
}

async fn ensure_root(root: &std::path::Path) -> anyhow::Result<()> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&root)?;
        anyhow::ensure!(
            !std::fs::symlink_metadata(&root)?.file_type().is_symlink(),
            "mirror state root cannot be a symlink"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

fn projects_root(state: &crate::AppState) -> PathBuf {
    crate::lock(&state.pro.projects_root)
        .clone()
        .unwrap_or_else(|| {
            state
                .claude_settings_path
                .parent()
                .and_then(std::path::Path::parent)
                .unwrap_or(&state.pro.root)
                .join("chimaera")
        })
}

pub(crate) fn profile_generation(state: &crate::AppState) -> u64 {
    state
        .pro
        .generation
        .load(std::sync::atomic::Ordering::Acquire)
}
pub(crate) fn cloud_hours_exhausted(state: &crate::AppState) -> Option<bool> {
    crate::lock(&state.pro.runtime)
        .as_ref()
        .map(|config| config.hours_exhausted)
}
pub(crate) fn is_worker(state: &crate::AppState) -> bool {
    crate::lock(&state.pro.runtime)
        .as_ref()
        .is_some_and(|config| config.role == protocol::Role::Worker)
}
pub(crate) fn workspace_profile(state: &crate::AppState, workspace: &str) -> Option<CloudProfile> {
    authority::workspace(state, workspace).ok()?;
    if !state
        .pro
        .configured
        .load(std::sync::atomic::Ordering::Acquire)
        || !projects::account_matches(state, workspace)
        || crate::lock(&state.workspaces).get(workspace).is_none()
    {
        return None;
    }
    Some(
        crate::lock(&state.pro.preferences)
            .get(workspace)
            .map(|entry| entry.profile.clone())
            .unwrap_or_default(),
    )
}
pub(crate) async fn save_workspace_profile(
    state: &std::sync::Arc<crate::AppState>,
    workspace: &str,
    expected_generation: u64,
    expected: &CloudProfile,
    updated: CloudProfile,
) -> anyhow::Result<()> {
    authority::workspace(state, workspace)?;
    updated.validate()?;
    let _configuration = state.pro.configuration.lock().await;
    anyhow::ensure!(
        profile_generation(state) == expected_generation,
        "Account changed; read the cloud profile again before updating"
    );
    let _job = state
        .pro
        .jobs
        .try_lock()
        .map_err(|_| anyhow::anyhow!("Project transfer is active; retry after it finishes"))?;
    anyhow::ensure!(
        may_write(state, workspace) && projects::account_matches(state, workspace),
        "Project is currently read-only"
    );
    let current = workspace_profile(state, workspace)
        .ok_or_else(|| anyhow::anyhow!("Cloud profile is unavailable"))?;
    anyhow::ensure!(
        &current == expected,
        "Cloud profile changed; read it again before updating"
    );
    let previous = {
        let mut preferences = crate::lock(&state.pro.preferences);
        anyhow::ensure!(
            preferences.len() < 128 || preferences.contains_key(workspace),
            "Cloud profile limit reached"
        );
        let previous = preferences.get(workspace).cloned();
        preferences.entry(workspace.into()).or_default().profile = updated;
        previous
    };
    if let Err(error) = persist(state).await {
        let mut preferences = crate::lock(&state.pro.preferences);
        if let Some(previous) = previous {
            preferences.insert(workspace.into(), previous);
        } else {
            preferences.remove(workspace);
        }
        return Err(error);
    }
    state.changes.notify_waiters();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn state(root: &std::path::Path) -> Arc<crate::AppState> {
        Arc::new(crate::AppState::new(
            "local-test".into(),
            "test-host".into(),
            4242,
            0,
            root.to_path_buf(),
            root.join("config"),
        ))
    }
    #[tokio::test]
    async fn restart_fences_old_conversations_and_never_persists_tokens() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-pro-state-{}",
            chimaera_core::generate_token()
        ));
        let old = state(&root);
        assert!(may_write(&old, "w-new"));
        crate::lock(&old.pro.ownership).insert("w-owned".into(), Ownership::Local { epoch: 7 });
        crate::lock(&old.pro.ownership)
            .insert("w-loading".into(), Ownership::Hydrating { epoch: 8 });
        crate::lock(&old.pro.runtime).replace(protocol::Configure {
            recovery: false,
            execution: None,
            account_id: None,
            endpoint: "http://127.0.0.1:1".into(),
            keeper_url: String::new(),
            role: protocol::Role::Worker,
            hours_exhausted: false,
            delegation: protocol::Delegation {
                workspace: None,
                access_token: "MUST_NEVER_PERSIST".into(),
                device_id: "worker-test".into(),
                expires_at: String::new(),
                scope: vec!["baton".into(), "mirror".into()],
            },
        });
        // Configuring a worker records its strict role (routes::configure_inner).
        execution::worker_fixture(&old);
        persist(&old).await.unwrap();
        let text = std::fs::read_to_string(root.join("pro/state.json")).unwrap();
        assert!(!text.contains("MUST_NEVER_PERSIST"));
        let restored = state(&root);
        assert!(!may_write(&restored, "w-owned"));
        assert!(!may_write(&restored, "w-loading"));
        assert!(!may_import(&restored, "w-owned", 7));
        assert!(may_import(&restored, "w-loading", 8));
        assert!(!may_import(&restored, "w-loading", 7));
        crate::lock(&restored.pro.ownership)
            .insert("w-owned".into(), Ownership::Local { epoch: 9 });
        assert!(may_write(&restored, "w-owned"));
        assert_eq!(owned_epoch(&restored, "w-owned"), Some(9));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn a_restarted_device_keeps_working_until_another_owner_is_verified() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-pro-device-{}",
            chimaera_core::generate_token()
        ));
        let old = state(&root);
        crate::lock(&old.pro.ownership).insert("w-owned".into(), Ownership::Local { epoch: 7 });
        crate::lock(&old.pro.ownership)
            .insert("w-flushing".into(), Ownership::Transferring { epoch: 3 });
        crate::lock(&old.pro.ownership).insert(
            "w-cloud".into(),
            Ownership::Remote {
                epoch: 4,
                holder: "worker-a".into(),
            },
        );
        persist(&old).await.unwrap();
        let restored = state(&root);
        // Laptop first: an unverified restart never locks the user out.
        assert!(may_write(&restored, "w-owned"));
        assert!(may_write(&restored, "w-flushing"));
        // A verified other owner remains fenced across restart.
        assert!(!may_write(&restored, "w-cloud"));
        // Publication still needs this life's verified epoch.
        assert_eq!(owned_epoch(&restored, "w-owned"), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
