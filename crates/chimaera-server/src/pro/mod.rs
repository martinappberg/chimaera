//! Optional daemon-owned mirrors and workspace handoff. No credential is durable.
mod authority;
mod canonical;
mod companion;
mod config;
#[cfg(any(target_os = "macos", test))]
mod config_exec;
mod config_wire;
mod detached;
mod drain;
pub(crate) mod engine;
pub(crate) mod execution;
pub(crate) mod install;
mod kept;
pub(crate) mod mirror;
pub(crate) mod moves;
mod place;
pub(crate) mod policy;
mod project_copy;
mod projects;
mod protocol;
mod provider_gate;
mod reach;
mod repository;
pub(crate) mod routes;
pub(crate) mod shadow_cache;
mod sleep_watch;
pub(crate) mod transfer_dispatch;
pub(crate) mod transfer_host;
pub(crate) mod transfer_types;
mod transport;
mod trash;
pub(crate) use drain::{cancel as cancel_drain, start as drain};
pub(crate) use kept::{
    file as kept_file, list as kept_list, resolve as kept_resolve, resolve_all as kept_resolve_all,
};
#[cfg(feature = "daemon-extension-fixture")]
pub(crate) use moves::device_fixture;
pub(crate) use moves::{acted_here, other_computer};
pub(crate) use place::{run_here, run_in_cloud};
pub(crate) use provider_gate::{
    blocking_provider, cloud_provider_blocks, workspace_provider_blocks,
};
pub(crate) use routes::*;

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
    /// The user signed out of Pro on this computer (`/pro/disconnect`), and
    /// has not signed in since. Persisted: after a restart, only this (never
    /// a configuration that has not arrived yet) lets interrupted sessions
    /// resume without the account (`resume_unverified`).
    signed_out: AtomicBool,
    generation: AtomicU64,
    runtime: Mutex<Option<protocol::Configure>>,
    authority: Mutex<authority::Authority>,
    execution: execution::State,
    ownership: Mutex<HashMap<String, Ownership>>,
    preferences: Mutex<HashMap<String, Preference>>,
    projects_root: Mutex<Option<PathBuf>>,
    adoptions: Mutex<HashMap<String, projects::Destination>>,
    legacy_pending: Mutex<std::collections::HashSet<String>>,
    copies: project_copy::Enrollment,
    project_cache: Mutex<projects::Cache>,
    discovery: AsyncMutex<()>,
    status: Mutex<HashMap<String, WorkspaceStatus>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    mirror_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The lease loop's per-project passes still running after their pass
    /// moved on (`CoordinatorTick::reconcile`): owned, so a slow project is
    /// never cancelled halfway, and skipped by later passes until it ends.
    /// Aborted with the lease loop (`routes::stop_tasks`). At most one per
    /// project.
    reconciling: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    jobs: Arc<AsyncMutex<()>>,
    /// Advanced by every wake (only): a flush started before it keeps its
    /// publication but never releases a project after the wake.
    sleep_generation: AtomicU64,
    sleeping: Mutex<std::collections::HashSet<String>>,
    /// Projects a sleep flush stopped but did not hand over (its release ran
    /// out of time, or the flush failed): no renewal or resume happens inside
    /// the sleep window; the next wake returns them to this computer locally.
    release_pending: Mutex<std::collections::HashSet<String>>,
    /// Projects the user chose to run in the cloud (`place::run_in_cloud`):
    /// this computer neither renews nor takes them back until "Run here",
    /// the cloud saying it cannot run them, or sign-out. Persisted, so a
    /// daemon restart keeps them there too.
    parked: Mutex<std::collections::HashSet<String>>,
    /// The plain reason each project's work is not where it would be, and the
    /// kind and name of what holds each one elsewhere, both from the last
    /// ownership read (`place::observed`). Hot state, bounded.
    reasons: place::Reasons,
    /// Sessions a lease fence stopped and kept for resuming here
    /// (`execution::watchdog::preserve`). They resume only while this computer
    /// still holds the epoch they ran under: once another machine held the
    /// project, or a return installed its own copy, they are stale and dropped
    /// (`drop_fenced`), so a finished turn never runs again. Hot, bounded.
    fenced_sessions: Mutex<std::collections::HashSet<String>>,
    holders: Mutex<HashMap<String, (String, Option<String>)>>,
    /// The last bytes written, so an unchanged tick costs no disk sync.
    persistence: Arc<AsyncMutex<Option<Vec<u8>>>>,
    #[cfg(test)]
    persistence_pause: Mutex<Option<PersistencePause>>,
    configuration: Arc<AsyncMutex<()>>,
    caches: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
    boot_deferred: Mutex<std::collections::HashSet<String>>,
    /// Projects whose checkpoint install is scheduled or running: fenced from
    /// the moment it is scheduled, before its own Hydrating fence exists.
    installing: Mutex<std::collections::HashSet<String>>,
    /// Projects the user opened on this computer that are not held here yet
    /// (`note_opened`): the project comes home to the computer it was last
    /// opened on, whether or not the account calls this installation the
    /// preferred one. Cleared when this device acquires or holds the project
    /// (`execution::accept`) and on sign-out.
    opened_here: Mutex<std::collections::HashSet<String>>,
    /// Projects the account answered for since this daemon started; the
    /// unverified-resume fallback leaves those to the verified path.
    answered: Mutex<std::collections::HashSet<String>>,
    operations: detached::Operations,
    drain: Mutex<Option<drain::Drain>>,
    /// Serializes drain requests; see `drain::start`.
    drain_gate: AsyncMutex<()>,
    /// Wakes transfers waiting for the job reservation when a drain takes it.
    drain_started: tokio::sync::Notify,
    remote_since: Mutex<HashMap<String, u64>>,
    return_backoff: Mutex<HashMap<String, (u64, u64)>>,
    /// Whether this computer can reach the account, from the lease loop's own
    /// calls (`reach`): since when it has answered without a gap (0: not now).
    reachable_since: AtomicU64,
    /// The daemon-owned reverse link to the keeper (`reach::Link`).
    link: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Projects being brought back here ("Run here", or the cloud could not
    /// run them): they come back at once, whatever the guard. Hot state,
    /// bounded by enrolled projects.
    reclaim: Mutex<std::collections::HashSet<String>>,
    /// When the last return-only pass started (`CoordinatorTick::start_return`).
    return_pass: AtomicU64,
    /// Wakes the lease loop at once: a resumed machine renews before fencing.
    renew_now: tokio::sync::Notify,
    /// The account refused this daemon's delegation (401/403 on renewal):
    /// `/pro/status` reports it so the native app mints a new one.
    delegation_refused: AtomicBool,
    /// Acting on another computer brings the work there (`moves`).
    moves: moves::Moves,
    /// Projects whose folder a snapshot last found not to be a Git
    /// repository (`repository::describe`): the log says so once, not on
    /// every pass. Hot state, bounded by enrolled projects.
    plain_folders: Mutex<std::collections::HashSet<String>>,
    /// The home Trash a discarded kept copy goes to (`trash::home`). Tests
    /// never reach the real one: they point it at a fixture, or leave it
    /// unset (no Trash, so a discarded copy is deleted).
    trash: Option<PathBuf>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Ownership {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    copy: Option<project_copy::CopyState>,
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
    /// The start time of each recorded group's leader (same order; 0 when
    /// unknown), so a reused group id is not mistaken for old work.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    execution_starts: Vec<u64>,
    /// More live groups existed than fit in the bounded evidence. A same-boot
    /// successor cannot infer termination from the recorded prefix alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    execution_groups_overflow: bool,
    /// An agent/setup launch or cleanup was not durably settled. A same-boot restart
    /// cannot infer completeness from the older agent-group prefix.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    execution_launch_pending: bool,
    #[serde(default)]
    execution_identity: Option<execution::wire::Identity>,
    #[serde(default)]
    supervisor_generation: Option<u64>,
    #[serde(default)]
    recovery_pending: bool,
    #[serde(default)]
    never_mirror: bool,
    #[serde(default)]
    privacy_pending: bool,
    #[serde(default)]
    git_branches: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_staging: Option<repository::StagingStatus>,
    /// The working-tree commit of the last acknowledged publication: the
    /// three-way baseline when work returns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    published_tree: Option<String>,
    /// Exact acknowledged handoff baseline, including portable Git staging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    published_handoff: Option<String>,
    /// Environment variable names the last move's configuration export left
    /// out (`policy::validate_missing_environment`), for the agents' note.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    missing_environment: Vec<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    git_staging: Option<repository::StagingStatus>,
    /// Additive: files the last return kept in both versions that still
    /// wait for a choice (`kept.rs` settles them one by one), and up to 32 of
    /// their project-relative paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    kept_both: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    kept_paths: Vec<PathBuf>,
    /// Additive: when that return happened (Unix ms) and how many files it
    /// kept in both versions then, which choices never lower.
    #[serde(skip_serializing_if = "Option::is_none")]
    kept_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kept_total: Option<usize>,
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
    /// The last return's kept-both report per project, so the project's row
    /// still says it after a restart (the kept copies are still on disk).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    kept_both: HashMap<String, KeptRecord>,
    /// Projects the user chose to run in the cloud (see `ProState::parked`);
    /// sorted so an unchanged set writes the same bytes.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    parked: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    signed_out: bool,
}
/// A return's kept-both report as persisted: the count, and the kept copies'
/// project-relative paths (fewer than `files` when bounded, see
/// [`persisted_kept`]).
#[derive(Clone, Default, Serialize, Deserialize)]
struct KeptRecord {
    files: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    paths: Vec<PathBuf>,
    /// When the return happened (Unix ms); 0 in a report saved before it was
    /// recorded.
    #[serde(default)]
    at: u64,
    /// How many files that return kept (`files` counts those still open).
    #[serde(default)]
    total: usize,
}
/// Kept-both reports share `state.json`'s 1 MiB cap with ownership state, so
/// they get a small fixed share of it: a report that would push past it keeps
/// its count and drops its names, never the ownership write.
const KEPT_PERSIST_BYTES: usize = 64 * 1024;
/// A single kept path longer than this is not persisted (its count is).
const KEPT_PATH_PERSIST_MAX: usize = 1024;
/// The kept-both reports to persist, bounded by [`KEPT_PERSIST_BYTES`] in
/// project-id order (so the choice is stable across writes).
fn persisted_kept(statuses: &HashMap<String, WorkspaceStatus>) -> HashMap<String, KeptRecord> {
    let mut reports: Vec<_> = statuses
        .iter()
        .filter_map(|(id, status)| Some((id, status.kept_both?, status)))
        .collect();
    reports.sort_by(|a, b| a.0.cmp(b.0));
    let mut budget = KEPT_PERSIST_BYTES;
    reports
        .into_iter()
        .take(128)
        .map(|(id, files, status)| {
            let paths = status
                .kept_paths
                .iter()
                .filter(|path| path.as_os_str().len() <= KEPT_PATH_PERSIST_MAX)
                .take_while(|path| {
                    let cost = path.as_os_str().len() + 8;
                    let fits = cost <= budget;
                    if fits {
                        budget -= cost;
                    }
                    fits
                })
                .cloned()
                .collect();
            (
                id.clone(),
                KeptRecord {
                    files,
                    paths,
                    at: status.kept_at.unwrap_or(0),
                    total: status.kept_total.unwrap_or(files).max(files),
                },
            )
        })
        .collect()
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
        // The input is already bounded to 1 MiB. Runtime admission ceilings
        // cannot discard an existing ownership/privacy/account fence on load.
        let legacy_pending = disk
            .legacy_pending
            .into_iter()
            .chain(disk.import_roots.into_keys())
            .filter(|id| valid_id(id))
            .collect();
        // A project handed to the cloud on quit stays away across a restart:
        // its finished transfer keeps its fence until the app returns. A
        // park whose flush never finished (the daemon died mid-flush) is not
        // a handover and is dropped, so that project is ordinary local work.
        let parked: std::collections::HashSet<String> = disk
            .parked
            .into_iter()
            .filter(|id| {
                valid_id(id)
                    && matches!(
                        disk.ownership.get(id),
                        Some(Ownership::Transferring { .. } | Ownership::Remote { .. })
                    )
            })
            .collect();
        let ownership: HashMap<_, _> = disk
            .ownership
            .into_iter()
            .map(|(id, owner)| {
                let owner = match owner {
                    Ownership::Transferring { epoch } if parked.contains(&id) => {
                        Ownership::Transferring { epoch }
                    }
                    Ownership::Local { epoch } | Ownership::Transferring { epoch } => {
                        Ownership::AwaitingVerification { epoch }
                    }
                    owner => owner,
                };
                (id, owner)
            })
            .collect();
        let mut status: HashMap<String, WorkspaceStatus> = disk
            .provider_blocks
            .into_iter()
            .take(128)
            .filter_map(|(id, blocks)| {
                let blocked_providers = provider_gate::restored(blocks);
                (matches!(
                    ownership.get(&id),
                    Some(Ownership::SettingUp { .. } | Ownership::AwaitingVerification { .. })
                ) && !blocked_providers.is_empty())
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
        for (id, kept) in disk.kept_both.into_iter().take(128) {
            if kept.files == 0 || !valid_id(&id) {
                continue;
            }
            let entry = status.entry(id).or_default();
            entry.kept_both = Some(kept.files);
            entry.kept_paths = kept.paths.into_iter().take(32).collect();
            entry.kept_at = (kept.at > 0).then_some(kept.at);
            entry.kept_total = Some(kept.total.max(kept.files));
        }
        for (id, preference) in &disk.preferences {
            if let Some(staging) = &preference.git_staging {
                status.entry(id.clone()).or_default().git_staging = Some(staging.clone());
            }
        }
        let authority = authority::Authority::load(&root);
        let execution = execution::State::restore(
            &root,
            &disk.preferences,
            disk.worker || crate::cloud::enabled(),
            unknown,
        );
        let copies = project_copy::Enrollment::load(&root);
        Self {
            root,
            configured: AtomicBool::new(false),
            worker: AtomicBool::new(disk.worker),
            signed_out: AtomicBool::new(disk.signed_out),
            generation: AtomicU64::new(0),
            runtime: Mutex::new(None),
            authority: Mutex::new(authority),
            execution,
            ownership: Mutex::new(ownership),
            preferences: Mutex::new(disk.preferences),
            projects_root: Mutex::new(disk.projects_root),
            adoptions: Mutex::new(disk.adoptions),
            legacy_pending: Mutex::new(legacy_pending),
            copies,
            project_cache: Mutex::new(projects::Cache::default()),
            discovery: AsyncMutex::new(()),
            status: Mutex::new(status),
            task: Mutex::new(None),
            mirror_task: Mutex::new(None),
            reconciling: Mutex::new(HashMap::new()),
            jobs: Arc::new(AsyncMutex::new(())),
            sleep_generation: AtomicU64::new(0),
            sleeping: Mutex::new(Default::default()),
            release_pending: Mutex::new(Default::default()),
            parked: Mutex::new(parked),
            reasons: Default::default(),
            fenced_sessions: Mutex::new(Default::default()),
            holders: Mutex::new(HashMap::new()),
            persistence: Arc::new(AsyncMutex::new(None)),
            #[cfg(test)]
            persistence_pause: Mutex::new(None),
            configuration: Arc::new(AsyncMutex::new(())),
            caches: Mutex::new(HashMap::new()),
            boot_deferred: Mutex::new(Default::default()),
            installing: Mutex::new(Default::default()),
            opened_here: Mutex::new(Default::default()),
            answered: Mutex::new(Default::default()),
            operations: Default::default(),
            drain: Mutex::new(None),
            drain_gate: AsyncMutex::new(()),
            drain_started: tokio::sync::Notify::new(),
            remote_since: Mutex::new(HashMap::new()),
            return_backoff: Mutex::new(HashMap::new()),
            reachable_since: AtomicU64::new(0),
            link: Mutex::new(None),
            reclaim: Mutex::new(Default::default()),
            return_pass: AtomicU64::new(0),
            renew_now: tokio::sync::Notify::new(),
            delegation_refused: AtomicBool::new(false),
            moves: Default::default(),
            plain_folders: Mutex::new(Default::default()),
            trash: if cfg!(test) { None } else { trash::home() },
        }
    }
}

/// The user opened this project on this computer (registering its folder from
/// its identity marker, or opening a registered workspace) and this computer
/// does not hold it: from now on the project may come home here (`lazy_handback`)
/// even when the account's preferred installation is another one — the
/// latest computer that had the project is the one it returns to. Bounded;
/// inert without Pro.
pub(crate) fn note_opened(state: &crate::AppState, workspace: &str) {
    if project_copy::copy_only(state, workspace) {
        return;
    }
    if matches!(
        crate::lock(&state.pro.ownership).get(workspace),
        Some(Ownership::Local { .. })
    ) {
        return;
    }
    let mut opened = crate::lock(&state.pro.opened_here);
    if opened.len() < 128 || opened.contains(workspace) {
        opened.insert(workspace.to_owned());
    }
}
#[cfg(test)]
pub(crate) fn opened_here(state: &crate::AppState, workspace: &str) -> bool {
    crate::lock(&state.pro.opened_here).contains(workspace)
}

/// Only a verified ownership transition or an explicit clean handoff fences a
/// device's writer; a cloud worker additionally waits for restart
/// verification. Connectivity loss never pauses local work.
pub(crate) fn local_copy_view(
    state: &crate::AppState,
    workspace: &str,
) -> Option<serde_json::Value> {
    project_copy::view(state, workspace)
}

pub(crate) fn may_write(state: &crate::AppState, workspace: &str) -> bool {
    if execution::supervisor::pending(state)
        || project_copy::copy_only(state, workspace)
        || state.bundle_imports.blocks_workspace(workspace)
    {
        return false;
    }
    if authority::workspace(state, workspace).is_err()
        || (crate::lock(&state.pro.authority).restricted()
            && !state
                .pro
                .configured
                .load(std::sync::atomic::Ordering::Acquire))
    {
        return false;
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
        // working until an authenticated read shows another owner, unless a
        // checkpoint install is already scheduled (before its own fence).
        Some(Ownership::AwaitingVerification { .. }) => {
            !execution::worker(state) && !crate::lock(&state.pro.installing).contains(workspace)
        }
        _ => true,
    }
}
/// Execution has a stricter lease boundary than local file editing.
#[cfg(any(test, feature = "daemon-extension-fixture"))]
pub(crate) use execution::install_fixture as install_execution_fixture;
pub(crate) use execution::installer;
pub(crate) use execution::mutation;
pub(crate) use execution::prepare_launch as prepare_managed_launch;
pub(crate) use execution::recovery_context as checkpoint_recovery_context;
#[cfg(test)]
pub(crate) use execution::remote_owner_fixture as install_remote_owner_fixture;
pub(crate) use execution::supervisor::{
    ack as supervisor_cleanup_ack, read_startup as read_supervisor_cleanup,
    stage_startup as stage_supervisor_cleanup,
};
#[cfg(test)]
pub(crate) use execution::{
    refused_fixture as refuse_renewal_fixture, renewed_fixture as renew_execution_fixture,
    resumed_fixture as resume_execution_fixture, worker_fixture as worker_execution_fixture,
};
/// The account itself answered the lease loop with a server error (its own
/// marker) just now.
#[cfg(test)]
pub(crate) fn account_failing_fixture(state: &crate::AppState) {
    reach::answered(state, 503, true);
}
/// The user signed out on this computer (as `/pro/disconnect` records it).
#[cfg(test)]
pub(crate) fn signed_out_fixture(state: &crate::AppState) {
    state
        .pro
        .signed_out
        .store(true, std::sync::atomic::Ordering::Release);
}
/// This life has not renewed a project's lease yet (its deadline passed), as
/// after a restart, without the watchdog having fenced anything.
#[cfg(test)]
pub(crate) fn lapse_execution_fixture(state: &crate::AppState, workspace: &str) {
    execution::lapse_fixture(state, workspace);
}
pub(crate) fn managed_execution(state: &crate::AppState, workspace: &str) -> bool {
    execution::managed(state, workspace)
}
/// This daemon's delegation can no longer act: the account refused it, or it
/// expired without a renewal. Only meaningful while configured.
pub(super) fn delegation_lapsed(state: &crate::AppState) -> bool {
    if state
        .pro
        .delegation_refused
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return true;
    }
    crate::lock(&state.pro.runtime)
        .as_ref()
        .and_then(|config| {
            time::OffsetDateTime::parse(
                &config.delegation.expires_at,
                &time::format_description::well_known::Rfc3339,
            )
            .ok()
        })
        .is_some_and(|expires| expires <= time::OffsetDateTime::now_utc())
}

pub(crate) fn may_execute(state: &crate::AppState, workspace: &str) -> bool {
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    if !execution::provider_startup::allows(state) {
        return false;
    }
    may_write(state, workspace) && execution::allows(state, workspace)
}
/// A plain shell's gate. Plain shells are never managed: a computer whose
/// agents wait for its own lease (lapsed, or not verified yet after a wake)
/// keeps its terminals working. Every ownership fence (`may_write`) applies
/// as to agents. Without Pro this is exactly `may_execute`.
pub(crate) fn may_run_shell(state: &crate::AppState, workspace: &str) -> bool {
    may_execute(state, workspace)
        || (may_write(state, workspace) && execution::shells_allowed(state, workspace))
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
/// Where a project's work stands, from this daemon's recorded ownership, in
/// the terms a viewer needs (why a paused session is paused). Read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Runs here (or no Pro ownership at all).
    Here,
    /// Here, waiting for the account to confirm after a restart or wake.
    Verifying,
    /// Being handed to another machine.
    Leaving,
    /// Another machine runs it.
    Elsewhere,
    /// Arriving here from another machine (files installing, setup).
    Arriving,
}
pub(crate) fn ownership_phase(state: &crate::AppState, workspace: &str) -> Phase {
    match crate::lock(&state.pro.ownership).get(workspace) {
        None | Some(Ownership::Local { .. } | Ownership::PrivacyDisabled { .. }) => Phase::Here,
        Some(Ownership::AwaitingVerification { .. }) => Phase::Verifying,
        Some(Ownership::Transferring { .. }) => Phase::Leaving,
        Some(Ownership::Remote { .. }) => Phase::Elsewhere,
        Some(Ownership::Hydrating { .. } | Ownership::SettingUp { .. }) => Phase::Arriving,
    }
}
/// A session a return imported here (it keeps its hand-off record) that
/// never resumed because a sign-out or crash cut the return's resume short,
/// in a project no other machine holds or is taking. It is this computer's
/// interrupted work, so at boot it waits like any session the previous
/// daemon left (`defer_boot_session`). A cloud machine resumes nothing
/// without its lease, and an uncertain continuation stays deferred, as the
/// return itself would have left it.
pub(crate) fn interrupted_return(
    state: &crate::AppState,
    entry: &crate::ledger::LedgerEntry,
) -> bool {
    entry.handoff.is_some()
        && !execution::worker(state)
        && matches!(
            crate::lock(&state.pro.ownership).get(&entry.workspace_id),
            None | Some(Ownership::Local { .. } | Ownership::AwaitingVerification { .. })
        )
        && may_execute(state, &entry.workspace_id)
        && execution::resume_allowed(state, &entry.workspace_id)
}

/// Presentation evidence only: the current daemon's role and an explicitly
/// recorded move to another computer are known. An opaque remote holder
/// cannot identify a cloud machine or a computer, so its owner is null.
pub(crate) fn owner_kind(state: &crate::AppState, workspace: &str) -> Option<&'static str> {
    match ownership_phase(state, workspace) {
        Phase::Elsewhere | Phase::Leaving => {
            moves::other_computer(state, workspace).then_some("computer")
        }
        Phase::Here | Phase::Verifying | Phase::Arriving => Some(if execution::worker(state) {
            "cloud"
        } else {
            "computer"
        }),
    }
}
/// Whether this session waits at boot for this life's ownership proof.
pub(crate) fn restart_deferred(state: &crate::AppState, session_id: &str) -> bool {
    crate::lock(&state.pro.boot_deferred).contains(session_id)
}
pub(crate) fn defer_boot_session(state: &crate::AppState, session: &str) {
    let mut deferred = crate::lock(&state.pro.boot_deferred);
    if deferred.len() < 512 {
        deferred.insert(session.to_owned());
    }
}
/// Restart-deferred sessions of a synced project on a device resume without
/// the account only when the user signed out (`signed_out`, written by
/// `/pro/disconnect` and persisted, so a restart before the configuration
/// arrives is never mistaken for it): sign-out never stops a computer's
/// work. Anything else proves nothing (the cloud may run the work by now), so
/// those sessions wait for the verified path (exactly once). Unsynced
/// projects resume unless old processes may still be running. Sessions a
/// clean handoff suspended stay suspended; they belong to whoever now owns
/// the project.
pub(crate) async fn resume_unverified(state: &std::sync::Arc<crate::AppState>) {
    if execution::worker(state) {
        return;
    }
    let waiting = resume_unverified_once(state).await;
    if waiting.is_empty() {
        return;
    }
    // Re-run the fallback once the recorded groups exit (bounded to 10 min).
    crate::lock(&state.pro.boot_deferred).extend(waiting);
    let owner = state.clone();
    tokio::spawn(async move {
        for _ in 0..120 {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if owner.stopping.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            execution::reprobe(&owner);
            let cleared = crate::lock(&owner.pro.boot_deferred).iter().any(|id| {
                crate::lock(&owner.deferred_sessions)
                    .get(id)
                    .is_some_and(|entry| !execution::unclean(&owner, &entry.workspace_id))
            });
            if cleared {
                let left = resume_unverified_once(&owner).await;
                if left.is_empty() {
                    return;
                }
                crate::lock(&owner.pro.boot_deferred).extend(left);
            }
        }
    });
}
/// One fallback pass; returns the sessions still waiting on old processes.
async fn resume_unverified_once(state: &std::sync::Arc<crate::AppState>) -> Vec<String> {
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
    let mut waiting = Vec::new();
    for (workspace, ids) in workspaces {
        // The account answered for this project (the verified path decides),
        // or a checkpoint install is replacing its files: not ours to resume.
        if crate::lock(&state.pro.answered).contains(&workspace)
            || crate::lock(&state.pro.installing).contains(&workspace)
        {
            continue;
        }
        if execution::unclean(state, &workspace) {
            // Old processes may still run: retry once they are proven gone.
            waiting.extend(ids);
            continue;
        }
        if !may_execute(state, &workspace) {
            continue;
        }
        // A synced project waits for the account unless the user signed out
        // (or it is kept on this computer: nobody else may take it).
        if execution::managed(state, &workspace)
            && !signed_out(state)
            && !execution::kept_here(state, &workspace)
        {
            // Still waiting for the verified path (or a later fallback).
            crate::lock(&state.pro.boot_deferred).extend(ids);
            continue;
        }
        if let Err(error) = crate::ledger::resume_deferred_sessions(state, &workspace, &ids).await {
            tracing::warn!(%error, "Interrupted sessions could not resume");
        }
    }
    waiting
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
/// A forwarded viewer's scope this machine cannot admit yet only because it
/// just thawed and its own renewal of exactly that epoch is still out. Worth
/// waiting for (`await_scope_renewal`), never refusing on sight: the account
/// keeps a suspended owner's lease, so the renewal normally lands in one round
/// trip. Any other epoch, or a machine not renewing, is answered at once.
pub(crate) fn scope_renewing(state: &crate::AppState, workspace: &str, epoch: u64) -> bool {
    execution::managed(state, workspace) && execution::renewing(state, workspace, epoch)
}
/// Waits for that renewal, never past the resume window the watchdog fences
/// at. `true` means a fresh proof for the epoch now exists and the caller
/// validates its scope again; a refused, fenced or unanswered renewal is
/// `false` as soon as it is known.
pub(crate) async fn await_scope_renewal(
    state: &crate::AppState,
    workspace: &str,
    epoch: u64,
) -> bool {
    execution::await_renewal(state, workspace, epoch, execution::RESUME_RENEW).await
        == execution::Renewal::Renewed
}
pub(crate) fn owned_epoch(state: &crate::AppState, workspace: &str) -> Option<u64> {
    if project_copy::copy_only(state, workspace) {
        return None;
    }
    match crate::lock(&state.pro.ownership).get(workspace) {
        Some(Ownership::Local { epoch }) => Some(*epoch),
        _ => None,
    }
}
/// The user chose to run this project in the cloud ("Run in the cloud").
fn parked(state: &crate::AppState, workspace: &str) -> bool {
    crate::lock(&state.pro.parked).contains(workspace)
}
/// Marks a "Run in the cloud" handover as it starts, unless another sleep or
/// wake advanced the generation since it began (`generation` is the
/// handover's): the newer one decides. The caller persists.
fn park(state: &crate::AppState, workspace: &str, generation: u64) -> bool {
    let mut parked = crate::lock(&state.pro.parked);
    if state
        .pro
        .sleep_generation
        .load(std::sync::atomic::Ordering::Acquire)
        != generation
    {
        return false;
    }
    if parked.len() < 128 || parked.contains(workspace) {
        parked.insert(workspace.to_owned());
    }
    true
}
/// The user signed out of Pro on this computer and has not signed in since
/// (persisted across restarts).
pub(crate) fn signed_out(state: &crate::AppState) -> bool {
    state
        .pro
        .signed_out
        .load(std::sync::atomic::Ordering::Acquire)
}
/// A fence's preserved sessions of `workspace` that nothing replaced: the
/// project ran elsewhere since (another holder, or a return that installed its
/// own copy, whose imported sessions carry a hand-off record), so they are
/// dropped instead of resuming a stale turn (review R3, harness cause 2).
pub(super) fn drop_fenced(state: &crate::AppState, workspace: &str) {
    let mut fenced = crate::lock(&state.pro.fenced_sessions);
    if fenced.is_empty() {
        return;
    }
    let mut deferred = crate::lock(&state.deferred_sessions);
    fenced.retain(|id| match deferred.get(id) {
        Some(entry) if entry.workspace_id == workspace => {
            if entry.handoff.is_none() {
                deferred.remove(id);
            }
            false
        }
        Some(_) => true,
        None => false,
    });
}
/// No longer kept in the cloud: "Run here", a handover that did not
/// complete, or the cloud saying it cannot run the project.
fn unpark(state: &crate::AppState, workspace: &str) {
    crate::lock(&state.pro.parked).remove(workspace);
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
#[cfg(test)]
struct PersistencePause {
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

async fn persist(state: &crate::AppState) -> anyhow::Result<()> {
    let mut written = state.pro.persistence.clone().lock_owned().await;
    ensure_root(&state.pro.root).await?;
    project_copy::persist_latch(state).await?;
    execution::record_groups(state);
    let ownership = crate::lock(&state.pro.ownership).clone();
    let preferences = crate::lock(&state.pro.preferences).clone();
    let projects_root = crate::lock(&state.pro.projects_root).clone();
    let adoptions = crate::lock(&state.pro.adoptions).clone();
    let legacy_pending = crate::lock(&state.pro.legacy_pending).clone();
    let parked = crate::lock(&state.pro.parked).iter().cloned().collect();
    let (provider_blocks, kept_both) = {
        let statuses = crate::lock(&state.pro.status);
        let blocks = statuses
            .iter()
            .filter(|(_, status)| !status.blocked_providers.is_empty())
            .take(128)
            .map(|(id, status)| (id.clone(), status.blocked_providers.clone()))
            .collect();
        (blocks, persisted_kept(&statuses))
    };
    let bytes = serde_json::to_vec(&DiskState {
        kept_both,
        provider_blocks,
        legacy_pending,
        import_roots: HashMap::new(),
        adoptions,
        projects_root,
        worker: state.pro.worker.load(std::sync::atomic::Ordering::Acquire),
        ownership,
        preferences,
        parked,
        signed_out: signed_out(state),
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
    #[cfg(test)]
    let pause = crate::lock(&state.pro.persistence_pause).take();
    // Aborting a coordinator/request cannot release this writer while its
    // blocking rename/fsync still owns state.json.tmp. Return the SAME guard
    // after settlement and retain it through the original state→latch order.
    written = tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        if let Some(pause) = pause {
            let _ = pause.entered.send(());
            pause
                .release
                .recv_timeout(std::time::Duration::from_secs(5))?;
        }
        crate::persist::atomic_write_json_durable(&path, copy)?;
        Ok::<_, anyhow::Error>(written)
    })
    .await??;
    written = execution::persist_latch(state, written).await?;
    *written = Some(bytes);
    Ok(())
}

/// A public archive's recovery belongs to the actual configured account,
/// while an ordinary unconfigured local import has no invented identity.
pub(crate) fn bundle_install_binding(
    state: &crate::AppState,
    workspace: &str,
    epoch: u64,
    digest: String,
) -> anyhow::Result<install::Binding> {
    authority::workspace(state, workspace)?;
    let config = crate::lock(&state.pro.runtime);
    Ok(install::Binding {
        endpoint: config
            .as_ref()
            .map(|c| c.endpoint.clone())
            .unwrap_or_default(),
        account: config.as_ref().and_then(|c| c.account_id.clone()),
        workspace: workspace.to_owned(),
        epoch,
        receipt: Some(digest),
    })
}

pub(crate) fn may_import(state: &crate::AppState, workspace: &str, epoch: u64) -> bool {
    if project_copy::copy_only(state, workspace)
        && !crate::lock(&state.pro.preferences)
            .get(workspace)
            .and_then(|p| p.copy.as_ref())
            .is_some_and(|copy| copy.takeover_requested)
    {
        return false;
    }
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

/// A paused row's fallback title uses this daemon's known role or an
/// explicit destination, and keeps opaque remote owners neutral.
pub(crate) fn paused_label(
    state: &crate::AppState,
    entry: &crate::ledger::LedgerEntry,
) -> &'static str {
    if matches!(
        ownership_phase(state, &entry.workspace_id),
        Phase::Elsewhere | Phase::Leaving
    ) {
        return if moves::other_computer(state, &entry.workspace_id) {
            "Continuing on your other computer"
        } else {
            "Continuing elsewhere"
        };
    }
    if execution::worker(state) {
        return if entry.agent.is_some() {
            "Starting in the cloud"
        } else {
            "Terminal on your computer"
        };
    }
    "Paused"
}

/// Graceful daemon stop: clear managed-execution evidence once this life's
/// agents are proven stopped, so a same-boot successor is not fenced.
pub(crate) async fn shutdown(state: &std::sync::Arc<crate::AppState>) {
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    if execution::provider_ready::stop(state).await.is_err() {
        tracing::warn!("Provider startup cleanup could not be confirmed");
    }
    if let Err(error) = execution::shutdown(state).await {
        tracing::warn!(%error, "Project execution state could not be saved at shutdown");
    }
}

/// Records what a return kept beside the user's own work: the files kept as
/// `.mine-…` siblings and the other machine's diverged branches
/// (`repository::receive`'s `<branch>@cloud-<commit>` names), so one notice
/// covers the whole return.
///
/// The report replaces the previous return's and is persisted with the rest
/// of the Pro state (`persist`, which the return runs before it resumes
/// work), so the row keeps saying it across a restart. When the return kept
/// anything, one `kept_both` notice goes to the notice feed: the native app's
/// OS notification and browser tabs' in-app alert — a `.mine-…` copy
/// appearing in the file tree should never be the first the user hears of it.
pub(super) fn report_return(
    state: &crate::AppState,
    workspace: &str,
    kept: (usize, Vec<PathBuf>),
    cloud_branches: &[String],
) {
    let (files, paths) = kept;
    {
        let mut statuses = crate::lock(&state.pro.status);
        let status = statuses.entry(workspace.into()).or_default();
        status.kept_both = (files > 0).then_some(files);
        status.kept_paths = paths.clone();
        status.kept_at = (files > 0).then(crate::session_view::now_ms);
        status.kept_total = (files > 0).then_some(files);
    }
    if crate::notices::push_kept_both(state, workspace, files, &paths, cloud_branches).is_none() {
        state.changes.notify_waiters();
    }
}

/// Work in flight that an idle decision must wait for: the job reservation
/// (unless a drain itself holds it), transfer tasks, sleep flushes, held
/// project caches (finalizers keep theirs past a canceled caller) and Git
/// helpers. A completed drain reports zero.
pub(crate) fn active_operations(state: &crate::AppState) -> usize {
    let active = project_operations(state) + transport::helpers_busy();
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    let active = active + execution::provider_ready::active(state);
    active
}

#[cfg(all(unix, feature = "provider-authority-prototype"))]
pub(crate) fn retire_provider_startup(state: &crate::AppState) {
    execution::provider_ready::retire(state);
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

pub(crate) async fn ensure_root(root: &std::path::Path) -> anyhow::Result<()> {
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

/// The same serialized configuration boundary used by imports and parking.
pub(crate) fn manual_resume_configuration(state: &crate::AppState) -> Arc<AsyncMutex<()>> {
    state.pro.configuration.clone()
}

pub(crate) fn manual_resume_storage(state: &crate::AppState) -> &std::path::Path {
    &state.pro.root
}
/// Where per-project transfer state lives (`<data>/pro`), outside every project.
pub(crate) fn storage(state: &crate::AppState) -> &std::path::Path {
    &state.pro.root
}

pub(crate) use execution::restore_manual_parking;

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

/// Whether the native app configured this daemon for Pro at all.
pub(crate) fn configured(state: &crate::AppState) -> bool {
    state
        .pro
        .configured
        .load(std::sync::atomic::Ordering::Acquire)
}
/// This daemon runs as the account's cloud (configured as the worker now or
/// before, or started as one): the service updates it, so it never checks
/// for, or offers, its own releases (`update.rs`).
pub(crate) fn updates_managed(state: &crate::AppState) -> bool {
    execution::worker(state)
}
pub(crate) fn is_worker(state: &crate::AppState) -> bool {
    crate::lock(&state.pro.runtime)
        .as_ref()
        .is_some_and(|config| config.role == protocol::Role::Worker)
}
/// An account is configured here and this project is in its scope (whether
/// or not it ever enrolled).
pub(crate) fn workspace_in_scope(state: &crate::AppState, workspace: &str) -> bool {
    authority::workspace(state, workspace).is_ok()
        && state
            .pro
            .configured
            .load(std::sync::atomic::Ordering::Acquire)
        && projects::account_matches(state, workspace)
        && crate::lock(&state.workspaces).get(workspace).is_some()
}
/// A project that actually moves between this computer and the cloud: an
/// account is configured for it here (or, after a restart, was and did not
/// sign out), it is enrolled (this daemon holds an
/// ownership record for it, as only a first copy, a move or a hydrate
/// writes), it is not kept on this computer, and it is not the cloud's own
/// setup scratch project. `workspace_in_scope` alone also answers for a
/// project that never enrolled. Answers the kept-both copies still waiting
/// for a choice, each with the file it sits beside (project-relative).
pub(crate) fn synced(state: &crate::AppState, workspace: &str) -> Option<Vec<(PathBuf, PathBuf)>> {
    // After a restart the configuration arrives a few seconds later; an
    // enrolled project of an account that did not sign out is still synced
    // meanwhile, so a conversation started then hears where it runs (review
    // R3, harness cause 3). Once configured, the account must match.
    let restarting = crate::lock(&state.pro.runtime).is_none()
        && !signed_out(state)
        && authority::workspace(state, workspace).is_ok()
        && crate::lock(&state.pro.preferences)
            .get(workspace)
            .is_some_and(|p| p.account.is_some());
    if !(workspace_in_scope(state, workspace) || restarting)
        || !crate::lock(&state.pro.ownership).contains_key(workspace)
        || crate::lock(&state.pro.preferences)
            .get(workspace)
            .is_some_and(|p| p.never_mirror)
        || crate::lock(&state.workspaces)
            .get(workspace)
            .is_none_or(|w| w.cloud_internal)
    {
        return None;
    }
    let kept = crate::lock(&state.pro.status)
        .get(workspace)
        .map(|status| status.kept_paths.clone())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|copy| {
            let name = copy.file_name()?.to_str()?;
            let original = copy.with_file_name(canonical::original_name(name)?);
            Some((copy, original))
        })
        .collect();
    Some(kept)
}
/// The environment variable names the last move into this project left out.
pub(crate) fn missing_environment(state: &crate::AppState, workspace: &str) -> Vec<String> {
    crate::lock(&state.pro.preferences)
        .get(workspace)
        .map(|entry| entry.missing_environment.clone())
        .unwrap_or_default()
}
/// Enrolls a project as its first copy would, for tests of what enrolled
/// projects get.
#[cfg(test)]
pub(crate) fn enroll_for_tests(state: &crate::AppState, workspace: &str) {
    crate::lock(&state.pro.ownership).insert(workspace.into(), Ownership::Local { epoch: 1 });
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Review R3 harness cause 3: after a restart the configuration arrives
    /// a little later, and a conversation started meanwhile in an enrolled
    /// project still hears where it runs. A recorded sign-out ends that.
    #[tokio::test]
    async fn a_restarted_computer_tells_its_agents_before_it_is_set_up_again() {
        struct Noting;
        impl crate::daemon_extension::Runtime for Noting {
            fn coordinate(
                &self,
                _owner: crate::daemon_extension::CoordinatorOwner,
            ) -> crate::daemon_extension::RuntimeFuture {
                Box::pin(async {})
            }
            fn placement_note(
                &self,
                _facts: &crate::daemon_extension::guidance::Facts,
            ) -> Option<String> {
                Some("PLACE".into())
            }
        }
        let root = std::env::temp_dir().join(format!(
            "chimaera-restart-note-{}",
            chimaera_core::generate_token()
        ));
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut state = crate::AppState::new(
            "local-test".into(),
            "test-host".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        );
        state.daemon_extension = Some(Arc::new(Noting));
        let state = Arc::new(state);
        let workspace = crate::lock(&state.workspaces).add(project).unwrap().id;
        // Enrolled for an account in an earlier life; not configured yet.
        enroll_for_tests(&state, &workspace);
        crate::lock(&state.pro.preferences)
            .entry(workspace.clone())
            .or_default()
            .account = Some("a-fixture".into());
        assert!(crate::lock(&state.pro.runtime).is_none());
        assert_eq!(
            crate::mcp::cloud_context::note(&state, &workspace)
                .await
                .as_deref(),
            Some("PLACE")
        );
        // Signed out: nothing is synced any more.
        signed_out_fixture(&state);
        assert_eq!(
            crate::mcp::cloud_context::note(&state, &workspace).await,
            None
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
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
    async fn cancelled_persistence_observer_retains_original_writer_until_durable_settlement() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-writer-{}",
            chimaera_core::generate_token()
        ));
        let state = state(&root);
        let workspace = "p-writer";
        crate::lock(&state.pro.ownership).insert(workspace.into(), Ownership::Local { epoch: 1 });
        crate::lock(&state.pro.execution.latched).insert(workspace.into());
        let (entered, reached) = tokio::sync::oneshot::channel();
        let (release, resume) = std::sync::mpsc::channel();
        *crate::lock(&state.pro.persistence_pause) = Some(PersistencePause {
            entered,
            release: resume,
        });
        let original = state.clone();
        let first = tokio::spawn(async move { persist(&original).await });
        let mut second = None;
        let outcome: anyhow::Result<()> = async {
            tokio::time::timeout(std::time::Duration::from_secs(3), reached).await??;
            // Publication changed while the ORIGINAL blocking writer remains
            // held. Losing its coordinator observer must not authorize a new
            // writer to rename the same temporary file or overtake this write.
            crate::lock(&state.pro.ownership).insert(workspace.into(), Ownership::Remote {
                epoch: 2, holder: "d-other".into()
            });
            first.abort();
            while !first.is_finished() { tokio::task::yield_now().await; }
            anyhow::ensure!(state.pro.persistence.try_lock().is_err(), "original blocking writer was released");
            let successor = state.clone();
            second = Some(tokio::spawn(async move { persist(&successor).await }));
            anyhow::ensure!(
                tokio::time::timeout(std::time::Duration::from_millis(50), second.as_mut().unwrap()).await.is_err(),
                "successor overtook the original writer"
            );
            release.send(())?;
            tokio::time::timeout(std::time::Duration::from_secs(3), second.take().unwrap()).await???;
            let bytes = std::fs::read(root.join("pro/state.json"))?;
            let disk: DiskState = serde_json::from_slice(&bytes)?;
            anyhow::ensure!(matches!(disk.ownership.get(workspace), Some(Ownership::Remote { epoch: 2, holder }) if holder == "d-other"), "durable successor state was lost");
            let latch: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("pro/execution-authority.json"))?)?;
            anyhow::ensure!(latch["workspaces"] == serde_json::json!([workspace]), "original execution latch was not settled");
            anyhow::ensure!(state.pro.persistence.lock().await.as_deref() == Some(bytes.as_slice()), "cached write precedes durable settlement");
            Ok(())
        }.await;
        // Always unblock the test-owned worker and settle observers on failure.
        let _ = release.send(());
        first.abort();
        let _ = first.await;
        if let Some(task) = second {
            task.abort();
            let _ = task.await;
        }
        if outcome.is_ok() {
            std::fs::remove_dir_all(&root).unwrap();
        }
        assert!(
            outcome.is_ok(),
            "retained durable-writer regression failed; owned root retained"
        );
    }
    #[test]
    fn bounded_saved_state_preserves_fences_beyond_runtime_admission_cap() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-pro-load-cap-{}",
            chimaera_core::generate_token()
        ));
        let mut saved = DiskState::default();
        for index in 0..129 {
            let id = format!("w-parked-{index}");
            saved
                .ownership
                .insert(id.clone(), Ownership::Transferring { epoch: 4 });
            saved.parked.insert(id.clone());
            saved.legacy_pending.insert(id.clone());
            saved.preferences.insert(
                id.clone(),
                Preference {
                    account: Some("saved-account".into()),
                    never_mirror: true,
                    privacy_pending: true,
                    execution_active: true,
                    execution_launch_pending: true,
                    ..Default::default()
                },
            );
            saved.adoptions.insert(
                id,
                serde_json::from_value(serde_json::json!({
                    "root":root.join(format!("project-{index}")),
                    "account":"saved-account", "device":1, "inode":2,
                    "started":true, "complete":false
                }))
                .unwrap(),
            );
            let owner = match index % 3 {
                0 => Ownership::Remote {
                    epoch: 4,
                    holder: "another-device".into(),
                },
                1 => Ownership::PrivacyDisabled { epoch: 4 },
                _ => Ownership::Hydrating { epoch: 4 },
            };
            saved.ownership.insert(format!("w-fenced-{index}"), owner);
        }
        let bytes = serde_json::to_vec(&saved).unwrap();
        assert!(bytes.len() < 1024 * 1024);
        std::fs::create_dir_all(root.join("pro")).unwrap();
        std::fs::write(root.join("pro/state.json"), bytes).unwrap();
        let restored = state(&root);
        assert_eq!(crate::lock(&restored.pro.ownership).len(), 258);
        assert_eq!(crate::lock(&restored.pro.preferences).len(), 129);
        assert_eq!(crate::lock(&restored.pro.adoptions).len(), 129);
        assert_eq!(crate::lock(&restored.pro.legacy_pending).len(), 129);
        assert_eq!(crate::lock(&restored.pro.parked).len(), 129);
        crate::lock(&restored.pro.legacy_pending).clear();
        for index in 0..129 {
            let id = format!("w-parked-{index}");
            assert!(!may_write(&restored, &id));
            assert!(!may_write(&restored, &format!("w-fenced-{index}")));
            assert!(execution::unclean(&restored, &id));
            let preferences = crate::lock(&restored.pro.preferences);
            let preference = &preferences[&id];
            assert!(preference.never_mirror && preference.privacy_pending);
            assert_eq!(preference.account.as_deref(), Some("saved-account"));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn ownership_reads_as_a_viewer_phase() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-pro-phase-{}",
            chimaera_core::generate_token()
        ));
        let state = state(&root);
        assert_eq!(ownership_phase(&state, "w-a"), Phase::Here);
        for (owner, phase) in [
            (Ownership::Local { epoch: 1 }, Phase::Here),
            (Ownership::PrivacyDisabled { epoch: 1 }, Phase::Here),
            (
                Ownership::AwaitingVerification { epoch: 1 },
                Phase::Verifying,
            ),
            (Ownership::Transferring { epoch: 1 }, Phase::Leaving),
            (
                Ownership::Remote {
                    epoch: 1,
                    holder: "worker-a".into(),
                },
                Phase::Elsewhere,
            ),
            (Ownership::Hydrating { epoch: 1 }, Phase::Arriving),
            (Ownership::SettingUp { epoch: 1 }, Phase::Arriving),
        ] {
            crate::lock(&state.pro.ownership).insert("w-a".into(), owner);
            assert_eq!(ownership_phase(&state, "w-a"), phase);
        }
        crate::lock(&state.pro.ownership).insert(
            "w-a".into(),
            Ownership::Remote {
                epoch: 2,
                holder: "worker-a".into(),
            },
        );
        assert_eq!(owner_kind(&state, "w-a"), None, "an opaque other owner");
        assert_eq!(
            owner_kind(&state, "w-unknown"),
            Some("computer"),
            "runs here"
        );
        crate::lock(&state.pro.ownership).insert("w-a".into(), Ownership::Hydrating { epoch: 3 });
        assert_eq!(owner_kind(&state, "w-a"), Some("computer"), "arriving here");
        execution::worker_fixture(&state);
        assert_eq!(
            owner_kind(&state, "w-a"),
            Some("cloud"),
            "a cloud machine's own"
        );
        crate::lock(&state.pro.ownership).insert(
            "w-a".into(),
            Ownership::Remote {
                epoch: 4,
                holder: "d-home".into(),
            },
        );
        assert_eq!(
            owner_kind(&state, "w-a"),
            None,
            "a cloud machine also cannot identify an opaque other owner"
        );
        let entry = crate::ledger::LedgerEntry {
            id: "s-a".into(),
            suspended: true,
            manual_resume_reason: None,
            handoff: None,
            workspace_id: "w-a".into(),
            cwd: root.clone(),
            pinned_name: None,
            cols: 80,
            rows: 24,
            theme: "dark".into(),
            created_at: 0,
            agent: None,
        };
        assert_eq!(
            paused_label(&state, &entry),
            "Continuing elsewhere",
            "a worker's remote row is not starting here"
        );
        assert!(!restart_deferred(&state, "s-a"));
        defer_boot_session(&state, "s-a");
        assert!(restart_deferred(&state, "s-a"));
        let _ = std::fs::remove_dir_all(root);
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
            alias: None,
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
    /// A return that kept both versions says so once, through the notice
    /// feed (with the kept copies for the UI), and its report survives a
    /// restart; a return that kept nothing says nothing.
    #[tokio::test]
    async fn a_kept_both_return_notifies_once_and_its_report_survives_restart() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-pro-kept-{}",
            chimaera_core::generate_token()
        ));
        let old = state(&root);
        let folder = root.join("thesis");
        std::fs::create_dir_all(&folder).unwrap();
        let workspace = crate::lock(&old.workspaces).add(folder).unwrap();
        let head = old.notices.head();
        let kept: Vec<PathBuf> = [
            "ch1.md.mine-20260929-1412",
            "refs/refs.bib.mine-20260929-1412",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        report_return(&old, &workspace.id, (3, kept.clone()), &[]);
        let notices = old.notices.since(head);
        assert_eq!(notices.len(), 1, "one notice per return, not per file");
        let wire = notices[0].to_json(std::time::Instant::now());
        assert_eq!(wire["kind"], "kept_both");
        assert_eq!(wire["blocking"], false);
        assert_eq!(wire["workspace_id"], workspace.id.as_str());
        assert_eq!(wire["title"], "Kept both versions of 3 files");
        assert_eq!(wire["subtitle"], workspace.name.as_str());
        assert_eq!(
            wire["body"],
            "Your versions are saved beside them (ch1.md.mine-20260929-1412, \
             refs.bib.mine-20260929-1412 and 1 more)."
        );
        assert_eq!(
            wire["kept"],
            serde_json::json!({
                "files": 3,
                "paths": ["ch1.md.mine-20260929-1412", "refs/refs.bib.mine-20260929-1412"],
                "branches": [],
            })
        );
        // Not a session: a per-project key a newer return replaces.
        assert_eq!(
            wire["session_id"],
            format!("kept-both-{}", workspace.id).as_str()
        );
        // Counts mean approvals: a kept-both return never adds to them.
        assert!(crate::lock(&old.agents).is_empty());

        persist(&old).await.unwrap();
        let restored = state(&root);
        {
            let statuses = crate::lock(&restored.pro.status);
            let status = statuses.get(&workspace.id).expect("report restored");
            assert_eq!(status.kept_both, Some(3));
            assert_eq!(status.kept_paths, kept);
        }
        // A restart replays no notice (the feed never replays history).
        assert_eq!(restored.notices.since(0).len(), 0);

        // The next return that keeps nothing clears the report, silently.
        let head = old.notices.head();
        report_return(&old, &workspace.id, (0, Vec::new()), &[]);
        assert!(old.notices.since(head).is_empty());
        persist(&old).await.unwrap();
        let restored = state(&root);
        assert!(crate::lock(&restored.pro.status)
            .get(&workspace.id)
            .is_none_or(|status| status.kept_both.is_none()));

        // Branches kept beside the user's are named in the same one notice.
        report_return(
            &old,
            &workspace.id,
            (1, vec![PathBuf::from("a.txt.mine-20260929-1500")]),
            &["main@cloud-1a2b3c4d5e6f".to_owned()],
        );
        let notices = old.notices.since(head);
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].title,
            "Kept both versions of 1 file and 1 branch"
        );
        assert_eq!(
            notices[0].body,
            "Your version is saved beside it (a.txt.mine-20260929-1500). The other \
             machine's version is kept as branch main@cloud-1a2b3c4d5e6f."
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn persisted_kept_reports_stay_within_their_share_of_the_state_file() {
        let long = PathBuf::from("x".repeat(1000));
        let statuses: HashMap<String, WorkspaceStatus> = (0..128)
            .map(|n| {
                (
                    format!("w-{n:03}"),
                    WorkspaceStatus {
                        kept_both: Some(40),
                        kept_paths: vec![long.clone(); 32],
                        ..Default::default()
                    },
                )
            })
            .collect();
        let kept = persisted_kept(&statuses);
        assert_eq!(kept.len(), 128, "every count is kept");
        assert!(kept.values().all(|record| record.files == 40));
        let bytes = serde_json::to_vec(&kept).unwrap().len();
        assert!(
            bytes < KEPT_PERSIST_BYTES + 128 * 64,
            "{bytes} bytes of kept reports"
        );
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
