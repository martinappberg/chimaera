//! Optional daemon-owned mirrors and workspace handoff. No credential is durable.
mod authority;
mod config;
mod engine;
mod mirror;
mod policy;
mod projects;
mod protocol;
mod provider_gate;
mod repository;
mod routes;
mod transport;
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
        Mutex,
    },
};
use tokio::sync::Mutex as AsyncMutex;

pub(crate) struct ProState {
    root: PathBuf,
    configured: AtomicBool,
    generation: AtomicU64,
    runtime: Mutex<Option<protocol::Configure>>,
    authority: Mutex<authority::Authority>,
    ownership: Mutex<HashMap<String, Ownership>>,
    preferences: Mutex<HashMap<String, Preference>>,
    projects_root: Mutex<Option<PathBuf>>,
    adoptions: Mutex<HashMap<String, projects::Destination>>,
    legacy_pending: Mutex<std::collections::HashSet<String>>,
    project_cache: Mutex<projects::Cache>,
    discovery: AsyncMutex<()>,
    keep_running: Mutex<std::collections::HashSet<String>>,
    status: Mutex<HashMap<String, WorkspaceStatus>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    mirror_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    jobs: AsyncMutex<()>,
    persistence: AsyncMutex<()>,
    configuration: AsyncMutex<()>,
    awake_since: AtomicU64,
    power_suitable: AtomicBool,
}
#[derive(Clone, Serialize, Deserialize)]
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
    never_mirror: bool,
    #[serde(default)]
    privacy_pending: bool,
    #[serde(default)]
    git_branches: Vec<String>,
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
    #[serde(default)]
    keep_running: std::collections::HashSet<String>,
    ownership: HashMap<String, Ownership>,
    preferences: HashMap<String, Preference>,
}
impl ProState {
    pub(crate) fn new(root: PathBuf) -> Self {
        // Construction already happens on the daemon's startup blocking path.
        // A capped record can gate restore without needing an account token.
        let disk = std::fs::File::open(root.join("state.json"))
            .ok()
            .and_then(|file| {
                use std::io::Read;
                let mut bytes = Vec::new();
                file.take(1024 * 1024 + 1).read_to_end(&mut bytes).ok()?;
                (bytes.len() <= 1024 * 1024)
                    .then(|| serde_json::from_slice::<DiskState>(&bytes).ok())
                    .flatten()
            })
            .unwrap_or_default();
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
                        ..Default::default()
                    },
                ))
            })
            .collect();
        let authority = authority::Authority::load(&root);
        Self {
            root,
            configured: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            runtime: Mutex::new(None),
            authority: Mutex::new(authority),
            ownership: Mutex::new(ownership),
            preferences: Mutex::new(disk.preferences.into_iter().take(128).collect()),
            projects_root: Mutex::new(disk.projects_root),
            adoptions: Mutex::new(disk.adoptions.into_iter().take(128).collect()),
            legacy_pending: Mutex::new(legacy_pending),
            project_cache: Mutex::new(projects::Cache::default()),
            discovery: AsyncMutex::new(()),
            keep_running: Mutex::new(disk.keep_running.into_iter().take(512).collect()),
            status: Mutex::new(status),
            task: Mutex::new(None),
            mirror_task: Mutex::new(None),
            jobs: AsyncMutex::new(()),
            persistence: AsyncMutex::new(()),
            configuration: AsyncMutex::new(()),
            awake_since: AtomicU64::new(now()),
            power_suitable: AtomicBool::new(false),
        }
    }
}

/// Only a verified ownership transition, restart verification, or explicit
/// clean handoff fences a writer. Connectivity loss never pauses local work.
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
    !crate::lock(&state.pro.legacy_pending).contains(workspace)
        && !matches!(
            crate::lock(&state.pro.ownership).get(workspace),
            Some(
                Ownership::Remote { .. }
                    | Ownership::PrivacyDisabled { .. }
                    | Ownership::Hydrating { .. }
                    | Ownership::SettingUp { .. }
                    | Ownership::Transferring { .. }
                    | Ownership::AwaitingVerification { .. }
            )
        )
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
    let _guard = state.pro.persistence.lock().await;
    ensure_root(&state.pro.root).await?;
    let keep_running = crate::lock(&state.pro.keep_running).clone();
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
        keep_running,
        ownership,
        preferences,
    })?;
    anyhow::ensure!(bytes.len() <= 1024 * 1024, "mirror settings exceed limit");
    let path = state.pro.root.join("state.json");
    tokio::task::spawn_blocking(move || crate::persist::atomic_write_json(&path, bytes)).await??;
    Ok(())
}

pub(crate) fn keep_running(state: &crate::AppState, session_id: &str) -> bool {
    crate::lock(&state.pro.keep_running).contains(session_id)
}
pub(crate) async fn set_keep_running(
    state: &std::sync::Arc<crate::AppState>,
    session_id: &str,
    value: bool,
) -> anyhow::Result<()> {
    authority::session(state, session_id)?;
    anyhow::ensure!(valid_id(session_id), "invalid session identity");
    {
        let mut pins = crate::lock(&state.pro.keep_running);
        if value {
            anyhow::ensure!(
                pins.len() < 512 || pins.contains(session_id),
                "session pin limit"
            );
            pins.insert(session_id.into());
        } else {
            pins.remove(session_id);
        }
    }
    persist(state).await?;
    state.changes.notify_waiters();
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

pub(crate) fn active_operations(state: &crate::AppState) -> usize {
    usize::from(state.pro.jobs.try_lock().is_err())
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
        set_keep_running(&old, "s-pinned", true).await.unwrap();
        persist(&old).await.unwrap();
        let text = std::fs::read_to_string(root.join("pro/state.json")).unwrap();
        assert!(!text.contains("MUST_NEVER_PERSIST"));
        let restored = state(&root);
        assert!(!may_write(&restored, "w-owned"));
        assert!(!may_write(&restored, "w-loading"));
        assert!(!may_import(&restored, "w-owned", 7));
        assert!(may_import(&restored, "w-loading", 8));
        assert!(!may_import(&restored, "w-loading", 7));
        assert!(keep_running(&restored, "s-pinned"));
        crate::lock(&restored.pro.ownership)
            .insert("w-owned".into(), Ownership::Local { epoch: 9 });
        assert!(may_write(&restored, "w-owned"));
        assert_eq!(owned_epoch(&restored, "w-owned"), Some(9));
        std::fs::remove_dir_all(root).unwrap();
    }
}
