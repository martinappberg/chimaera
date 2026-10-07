//! Cloud discovery is a read. Only an explicit local action can adopt a project.
use super::{
    engine,
    protocol::{Baton, Configure, Host, Role},
    transport,
};
use crate::{lock, AppState};
use anyhow::{ensure, Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

const MAX_PROJECTS: usize = 128;
pub(super) mod catalog;
static TAKEOVER_ACTIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(32);

/// Why an open refused, as the stable `error_code` on the route's failure body
/// (`open_error_code`; the list is in the pro map). Tagged where each refusal is
/// raised so a client never has to read the sentence, which stays diagnostic.
#[derive(Debug)]
struct Refused {
    code: &'static str,
    message: &'static str,
}
impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for Refused {}
fn refuse(code: &'static str, message: &'static str) -> anyhow::Error {
    Refused { code, message }.into()
}
/// The stable code for a failed open. A failure raised in this file names its
/// own; one from the transfer engine falls back to its diagnostic category.
pub(super) fn open_error_code(error: &anyhow::Error) -> &'static str {
    if let Some(refused) = error.downcast_ref::<Refused>() {
        return refused.code;
    }
    match super::routes::error_code(error) {
        "account_changed" => "account_changed",
        "return_window_ended" => "return_window_ended",
        "ownership_changed" | "ownership_unverified" => "owned_elsewhere",
        "previous_processes_running" | "checkpoint_pending" => "busy",
        _ => "failed",
    }
}
#[derive(Clone, Serialize)]
pub(super) struct Project {
    workspace_id: String,
    name: String,
    host_id: Option<String>,
    host_alias: Option<String>,
    local_root: Option<PathBuf>,
    destination_saved: bool,
    available: bool,
    error: Option<String>,
}
#[derive(Clone, Default, Serialize)]
pub(super) struct Cache {
    projects: Vec<Project>,
    error: Option<String>,
    #[serde(skip)]
    checked_at: u64,
}
/// A folder explicitly selected on this device. Legacy global/import roots are
/// deliberately not migrated into this authorization record.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Destination {
    root: PathBuf,
    #[serde(default)]
    account: Option<String>,
    device: u64,
    inode: u64,
    #[serde(default)]
    started: bool,
    #[serde(default)]
    complete: bool,
}
#[derive(Deserialize)]
pub(crate) struct Open {
    expected_account_id: String,
    expected_endpoint: String,
    workspace_id: String,
    destination_root: Option<PathBuf>,
}

pub(super) fn adoption_pending(state: &AppState, workspace: &str) -> bool {
    if lock(&state.pro().preferences)
        .get(workspace)
        .is_some_and(super::project_copy::ready)
    {
        return false;
    }
    lock(&state.pro().legacy_pending).contains(workspace)
        || lock(&state.pro().adoptions)
            .get(workspace)
            .is_some_and(|entry| !entry.complete)
}
pub(in crate::pro) fn local_root(state: &AppState, workspace: &str) -> Option<PathBuf> {
    if (lock(&state.pro().legacy_pending).contains(workspace)
        && !lock(&state.pro().preferences)
            .get(workspace)
            .is_some_and(super::project_copy::ready))
        || !account_matches(state, workspace)
        || (!lock(&state.pro().adoptions).contains_key(workspace)
            && lock(&state.pro().preferences)
                .get(workspace)
                .is_none_or(|entry| entry.account.is_none()))
    {
        return None;
    }
    lock(&state.workspaces)
        .get(workspace)
        .map(|entry| entry.root)
        .or_else(|| {
            lock(&state.pro().adoptions)
                .get(workspace)
                .filter(|entry| entry.started || entry.complete)
                .map(|entry| entry.root.clone())
        })
}

fn account_scope(config: &Configure) -> Option<String> {
    config
        .account_id
        .as_deref()
        .filter(|id| super::valid_id(id))
        .map(|id| format!("{}/{}", config.endpoint.trim_end_matches('/'), id))
}
pub(super) fn account_matches(state: &AppState, workspace: &str) -> bool {
    let saved = lock(&state.pro().adoptions)
        .get(workspace)
        .map(|entry| entry.account.clone());
    let saved = saved.unwrap_or_else(|| {
        lock(&state.pro().preferences)
            .get(workspace)
            .and_then(|entry| entry.account.clone())
    });
    let Some(saved) = saved else {
        return !lock(&state.pro().adoptions).contains_key(workspace);
    };
    lock(&state.pro().runtime)
        .as_ref()
        .and_then(account_scope)
        .as_ref()
        == Some(&saved)
}
pub(super) fn bind_workspace_account(
    state: &AppState,
    config: &Configure,
    workspace: &str,
) -> Result<()> {
    if config.role != Role::Device {
        return Ok(());
    }
    let Some(account) = account_scope(config) else {
        return Ok(());
    };
    let mut preferences = lock(&state.pro().preferences);
    ensure!(
        preferences.len() < MAX_PROJECTS || preferences.contains_key(workspace),
        refuse("limit_reached", "Local project limit reached")
    );
    let entry = preferences.entry(workspace.into()).or_default();
    ensure!(
        entry.account.as_ref().is_none_or(|saved| saved == &account),
        refuse("other_account", "This project belongs to another account")
    );
    let newly_enrolled = entry.account.is_none();
    entry.account = Some(account);
    drop(preferences);
    if newly_enrolled {
        mark_folder(state, workspace);
    }
    Ok(())
}
/// The project was just enrolled: from now on its folder carries its id, so
/// a reinstall or another computer finds the same cloud copy
/// (`workspaces::identity`). Free projects never get one. Best effort, off
/// the reactor and never awaited.
pub(super) fn mark_folder(state: &AppState, workspace: &str) {
    let Some(root) = lock(&state.workspaces).get(workspace).map(|w| w.root) else {
        return;
    };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let id = workspace.to_owned();
    runtime.spawn_blocking(move || crate::workspaces::identity::write(&root, &id));
}
async fn discover(state: &Arc<AppState>, config: &Configure) -> Result<Vec<Project>> {
    anyhow::ensure!(
        config.delegation.workspace.is_none(),
        "workspace authority does not permit discovery"
    );
    if let Some(projects) = catalog::list(state, config).await? {
        return Ok(projects);
    }
    let hosts: Vec<Host> = transport::request(
        &config.keeper_url,
        "/v1/hosts",
        "GET",
        &config.delegation.access_token,
        None,
    )
    .await?
    .json()?;
    let mut projects = Vec::new();
    let mut seen = HashSet::new();
    #[derive(Deserialize)]
    struct Remote {
        id: String,
        name: String,
        #[serde(default)]
        cloud_internal: bool,
    }
    for host in hosts
        .into_iter()
        .filter(|host| super::valid_id(&host.id) && host.kind == "worker")
        .take(8)
    {
        // No wake header, mutation, mirror download or ownership request here.
        let response = transport::request(
            &config.keeper_url,
            &format!("/v1/hosts/{}/http/api/v1/workspaces", host.id),
            "GET",
            &config.delegation.access_token,
            None,
        )
        .await;
        let rows = response.and_then(|response| response.json::<Vec<Remote>>());
        let rows = match rows {
            Ok(rows) => rows,
            Err(_) => {
                let previous = lock(&state.pro().project_cache).projects.clone();
                for mut project in previous
                    .into_iter()
                    .filter(|project| project.host_id.as_deref() == Some(host.id.as_str()))
                {
                    if projects.len() >= MAX_PROJECTS {
                        break;
                    }
                    if seen.insert(project.workspace_id.clone()) {
                        project.available = false;
                        project.error =
                            Some("Cloud project list is temporarily unavailable".into());
                        project.local_root = local_root(state, &project.workspace_id);
                        project.destination_saved = account_matches(state, &project.workspace_id)
                            && lock(&state.pro().adoptions).contains_key(&project.workspace_id);
                        projects.push(project);
                    }
                }
                continue;
            }
        };
        for row in rows.into_iter().take(MAX_PROJECTS) {
            if projects.len() >= MAX_PROJECTS {
                break;
            }
            if row.cloud_internal || !super::valid_id(&row.id) || !seen.insert(row.id.clone()) {
                continue;
            }
            projects.push(Project {
                local_root: local_root(state, &row.id),
                destination_saved: account_matches(state, &row.id)
                    && lock(&state.pro().adoptions).contains_key(&row.id),
                workspace_id: row.id,
                name: row
                    .name
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(128)
                    .collect(),
                host_id: Some(host.id.clone()),
                host_alias: Some(
                    host.alias
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(128)
                        .collect(),
                ),
                available: true,
                error: None,
            });
        }
    }
    Ok(projects)
}
async fn configuration(state: &AppState) -> (Option<Configure>, u64) {
    // stop_tasks changes generation before replacing the runtime. Snapshot the
    // pair under the same lock so a queued request cannot mix two accounts.
    let _guard = state.pro().configuration.lock().await;
    (
        lock(&state.pro().runtime).clone(),
        state.pro().generation.load(Ordering::Acquire),
    )
}
pub(in crate::pro) async fn list(state: &Arc<AppState>) -> Cache {
    let (config, generation) = configuration(state).await;
    let Some(config) = config.filter(|config| config.role == Role::Device) else {
        return Cache::default();
    };
    let _guard = state.pro().discovery.lock().await;
    if generation != state.pro().generation.load(Ordering::Acquire) {
        return Cache::default();
    }
    let now = super::now();
    let cached = lock(&state.pro().project_cache).clone();
    if cached.checked_at != 0 && now.saturating_sub(cached.checked_at) < 30 {
        return cached;
    }
    let result = tokio::time::timeout(Duration::from_secs(10), discover(state, &config)).await;
    if generation != state.pro().generation.load(Ordering::Acquire) {
        return Cache::default();
    }
    let next = match result {
        Ok(Ok(projects)) => Cache {
            projects,
            error: None,
            checked_at: now,
        },
        _ => Cache {
            projects: cached
                .projects
                .into_iter()
                .map(|mut row| {
                    row.available = false;
                    row.error = Some("Cloud project list is temporarily unavailable".into());
                    row
                })
                .collect(),
            error: Some("Cloud project list is temporarily unavailable".into()),
            checked_at: now,
        },
    };
    *lock(&state.pro().project_cache) = next.clone();
    next
}
pub(crate) async fn project_list(State(state): State<Arc<AppState>>) -> Response {
    Json(list(&state).await).into_response()
}

fn identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        (0, 0)
    }
}
fn check_directory(root: &Path) -> Result<(PathBuf, u64, u64)> {
    ensure!(
        root.is_absolute()
            && !root
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir)),
        refuse("folder_unusable", "Choose an absolute project folder")
    );
    let metadata = std::fs::symlink_metadata(root).context(Refused {
        code: "folder_missing",
        message: "The project folder is missing; restore it before opening this project",
    })?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        refuse(
            "folder_unusable",
            "The project folder must be a real directory"
        )
    );
    let canonical = std::fs::canonicalize(root)?;
    let (device, inode) = identity(&metadata);
    Ok((canonical, device, inode))
}
fn reserve(
    root: &Path,
    workspaces: &[crate::workspaces::Workspace],
    other_roots: &[PathBuf],
) -> Result<Destination> {
    let (root, device, inode) = check_directory(root)?;
    ensure!(
        !workspaces
            .iter()
            .any(|entry| root.starts_with(&entry.root) || entry.root.starts_with(&root))
            && !other_roots
                .iter()
                .any(|other| root.starts_with(other) || other.starts_with(&root)),
        refuse(
            "folder_nested",
            "This folder belongs to another project; choose a separate empty folder"
        )
    );
    ensure!(
        std::fs::read_dir(&root)?.next().is_none(),
        refuse(
            "folder_not_empty",
            "Choose an empty folder; existing files and repositories will not be replaced"
        )
    );
    ensure!(
        !root
            .ancestors()
            .skip(1)
            .any(|parent| parent.join(".git").exists()),
        refuse(
            "folder_nested",
            "Choose a folder outside an existing Git repository"
        )
    );
    let probe = root.join(format!(
        ".chimaera-write-probe-{}",
        chimaera_core::generate_token()
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .context(Refused {
            code: "folder_unusable",
            message: "The project folder is not writable",
        })?;
    std::fs::remove_file(probe)?;
    Ok(Destination {
        root,
        account: None,
        device,
        inode,
        started: false,
        complete: false,
    })
}
fn verify(destination: &Destination, require_empty: bool) -> Result<()> {
    let (root, device, inode) = check_directory(&destination.root)?;
    ensure!(
        root == destination.root && device == destination.device && inode == destination.inode,
        refuse(
            "folder_moved",
            "The saved project folder has changed; restore its original location before opening"
        )
    );
    if require_empty {
        ensure!(
            std::fs::read_dir(&root)?.next().is_none(),
            refuse(
                "folder_not_empty",
                "The selected folder now contains files; nothing was imported"
            )
        );
        ensure!(
            !root
                .ancestors()
                .skip(1)
                .any(|parent| parent.join(".git").exists()),
            refuse(
                "folder_nested",
                "The selected folder is now inside another Git repository"
            )
        );
    }
    Ok(())
}

pub(super) fn check_copy_destination(state: &AppState, workspace: &str, root: &Path) -> Result<()> {
    let destination = lock(&state.pro().adoptions)
        .get(workspace)
        .cloned()
        .context("local copy destination is unavailable")?;
    ensure!(
        destination.root == root && account_matches(state, workspace),
        "local copy destination authority changed"
    );
    verify(&destination, false)
}
/// Recheck immediately before filesystem installation, after potentially slow
/// downloads. A durable marker makes partial imports retryable only at this path.
pub(super) async fn begin_install(state: &AppState, workspace: &str, root: &Path) -> Result<()> {
    let Some(destination) = lock(&state.pro().adoptions).get(workspace).cloned() else {
        return Ok(());
    };
    ensure!(
        destination.root == root,
        refuse(
            "folder_mismatch",
            "A project cannot change its saved local folder during import"
        )
    );
    let workspaces = lock(&state.workspaces).list();
    ensure!(
        !workspaces.iter().any(|entry| entry.id != workspace
            && (root.starts_with(&entry.root) || entry.root.starts_with(root))),
        refuse(
            "folder_nested",
            "The chosen folder now belongs to another project"
        )
    );
    let check = destination.clone();
    tokio::task::spawn_blocking(move || verify(&check, !check.started)).await??;
    let legacy = !super::project_copy::copy_only(state, workspace)
        && lock(&state.pro().legacy_pending).remove(workspace);
    if !destination.started || legacy {
        if let Some(entry) = lock(&state.pro().adoptions).get_mut(workspace) {
            entry.started = true;
        }
        super::persist(state).await?;
        lock(&state.pro().project_cache).checked_at = 0;
    }
    Ok(())
}

pub(in crate::pro) async fn copy(
    state: &Arc<AppState>,
    request: Open,
) -> Result<serde_json::Value> {
    anyhow::ensure!(
        !crate::lock(&state.pro().authority).restricted(),
        refuse("unavailable", "workspace destination is fixed")
    );
    ensure!(
        super::valid_id(&request.workspace_id),
        refuse("not_a_project", "Invalid project identity")
    );
    let (config, generation) = configuration(state).await;
    let config = config.context(Refused {
        code: "signed_out",
        message: "Sign in to open a cloud project",
    })?;
    ensure!(
        config.role == Role::Device,
        refuse(
            "unavailable",
            "Open locally is available on a personal device"
        )
    );
    let account = account_scope(&config).context(Refused {
        code: "signed_out",
        message: "Refresh sign-in before opening a cloud project",
    })?;
    ensure!(
        config.account_id.as_deref() == Some(request.expected_account_id.as_str())
            && config.endpoint.trim_end_matches('/')
                == request.expected_endpoint.trim_end_matches('/'),
        refuse("account_changed", "Account changed; open the project again")
    );

    let _jobs = state.pro().jobs.lock().await;
    ensure!(
        generation == state.pro().generation.load(Ordering::Acquire),
        refuse("account_changed", "Account changed; open the project again")
    );
    let explicit_unbound_recovery = lock(&state.pro().adoptions)
        .get(&request.workspace_id)
        .is_some_and(|entry| {
            entry.account.is_none() && request.destination_root.as_ref() == Some(&entry.root)
        });
    ensure!(
        account_matches(state, &request.workspace_id) || explicit_unbound_recovery,
        refuse(
            "other_account",
            "This saved project belongs to another account; its local folder will not be changed"
        )
    );
    let existing = lock(&state.workspaces).get(&request.workspace_id);
    let saved = lock(&state.pro().adoptions)
        .get(&request.workspace_id)
        .filter(|entry| entry.started || entry.complete || request.destination_root.is_none())
        .cloned();
    let destination = if let Some(mut saved) = saved {
        ensure!(
            saved.account.as_ref().is_none_or(|owner| owner == &account),
            refuse(
                "other_account",
                "This saved project belongs to another account; its local folder will not be changed"
            )
        );
        if saved.account.is_none() {
            ensure!(
                request.destination_root.as_ref() == Some(&saved.root),
                refuse(
                    "folder_required",
                    "Explicitly select the original project folder to recover this earlier import"
                )
            );
            saved.account = Some(account.clone());
            lock(&state.pro().adoptions).insert(request.workspace_id.clone(), saved.clone());
            super::persist(state).await?;
        }
        ensure!(
            request
                .destination_root
                .as_ref()
                .is_none_or(|root| root == &saved.root),
            refuse(
                "folder_mismatch",
                "This project already has a saved local folder"
            )
        );
        let check = saved.clone();
        tokio::task::spawn_blocking(move || verify(&check, !check.started)).await??;
        saved.root
    } else if let Some(existing) = &existing {
        ensure!(
            request
                .destination_root
                .as_ref()
                .is_none_or(|root| root == &existing.root),
            refuse(
                "folder_mismatch",
                "This project already has a saved local folder"
            )
        );
        let legacy = lock(&state.pro().legacy_pending).contains(&request.workspace_id)
            || lock(&state.pro().preferences)
                .get(&request.workspace_id)
                .is_none_or(|entry| entry.account.is_none());
        ensure!(
            !legacy || request.destination_root.is_some(),
            refuse(
                "folder_required",
                "Explicitly select this existing project folder before importing cloud work into it"
            )
        );
        let root = existing.root.clone();
        let (root, device, inode) =
            tokio::task::spawn_blocking(move || check_directory(&root)).await??;
        if legacy || !super::may_write(state, &request.workspace_id) {
            lock(&state.pro().adoptions).insert(
                request.workspace_id.clone(),
                Destination {
                    root: root.clone(),
                    account: Some(account.clone()),
                    device,
                    inode,
                    started: true,
                    complete: false,
                },
            );
            super::persist(state).await?;
        }
        root
    } else {
        // Worker discovery is a passive presentation cache, never account
        // authority. A new device may copy a project whose worker is asleep or
        // whose executor is another device, using its scoped read grant.
        let baton: Baton = engine::account(
            &config,
            &format!("/v2/baton/{}", request.workspace_id),
            "GET",
            None,
        )
        .await?
        .json()?;
        ensure!(
            baton.workspace_id == request.workspace_id
                && !baton.mirror_disabled
                && baton.checkpoint.is_some(),
            refuse(
                "checkpoint_pending",
                "No durable project copy is available yet"
            )
        );
        let grant = engine::credentials(&config, &request.workspace_id, None).await?;
        ensure!(
            grant.read_only && generation == state.pro().generation.load(Ordering::Acquire),
            "Account changed before copy folder enrollment"
        );
        let root = request.destination_root.context(Refused {
            code: "folder_required",
            message: "Choose where to save this project first",
        })?;
        let workspaces = lock(&state.workspaces).list();
        let other_roots = lock(&state.pro().adoptions)
            .iter()
            .filter(|(id, _)| *id != &request.workspace_id)
            .map(|(_, entry)| entry.root.clone())
            .collect::<Vec<_>>();
        ensure!(
            other_roots.len() < MAX_PROJECTS,
            refuse("limit_reached", "Local project limit reached")
        );
        let mut destination =
            tokio::task::spawn_blocking(move || reserve(&root, &workspaces, &other_roots))
                .await??;
        destination.account = Some(account.clone());
        let root = destination.root.clone();
        lock(&state.pro().adoptions).insert(request.workspace_id.clone(), destination);
        super::persist(state).await?;
        lock(&state.pro().project_cache).checked_at = 0;
        root
    };
    if let Some(existing) = existing.filter(|_| {
        super::may_write(state, &request.workspace_id)
            && super::owned_epoch(state, &request.workspace_id).is_some()
            && !adoption_pending(state, &request.workspace_id)
    }) {
        return Ok(
            json!({"workspace_id":existing.id,"root":existing.root,"name":existing.name,"copy_version":1,"state":"owned_local"}),
        );
    }
    let baton: Baton = engine::account(
        &config,
        &format!("/v2/baton/{}", request.workspace_id),
        "GET",
        None,
    )
    .await?
    .json()?;
    ensure!(
        baton.workspace_id == request.workspace_id && !baton.mirror_disabled,
        refuse("privacy", "Mirroring is not available for this project")
    );
    let receipt = baton.checkpoint.context(Refused {
        code: "checkpoint_pending",
        message: "No durable project copy is available yet; try again after the owner publishes",
    })?;
    super::project_copy::sync(
        state,
        &config,
        &request.workspace_id,
        destination,
        generation,
        super::project_copy::Selection {
            checkpoint: receipt,
            holder: baton.holder_id,
            epoch: baton.epoch,
        },
    )
    .await
}

#[derive(Deserialize)]
pub(crate) struct CopyRequest {
    copy_version: u16,
    #[serde(flatten)]
    project: Open,
}
pub(crate) async fn copy_project(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CopyRequest>,
) -> Response {
    if request.copy_version != 1 {
        return (StatusCode::UPGRADE_REQUIRED, Json(json!({"error":"Unsupported project-copy capability","error_code":"copy_upgrade_required","copy_version":1}))).into_response();
    }
    match tokio::time::timeout(Duration::from_secs(19 * 60), copy(&state, request.project)).await {
        Ok(Ok(project)) => Json(project).into_response(),
        Ok(Err(error)) => {
            let code = open_error_code(&error);
            super::routes::failure_with_code(error, code)
        }
        Err(_) => (StatusCode::GATEWAY_TIMEOUT, Json(json!({"error":"Project copy timed out; retry Open at the saved folder","error_code":"timed_out","copy_version":1}))).into_response(),
    }
}

pub(super) async fn begin_copy_install(
    state: &AppState,
    workspace: &str,
    root: &Path,
) -> Result<()> {
    let destination = lock(&state.pro().adoptions)
        .get(workspace)
        .cloned()
        .context("local copy destination is unavailable")?;
    ensure!(
        destination.root == root && account_matches(state, workspace),
        "local copy destination authority changed"
    );
    let workspaces = lock(&state.workspaces).list();
    ensure!(
        !workspaces.iter().any(|entry| entry.id != workspace
            && (root.starts_with(&entry.root) || entry.root.starts_with(root))),
        "local copy destination overlaps another project"
    );
    let check = destination.clone();
    tokio::task::spawn_blocking(move || verify(&check, !check.started)).await??;
    if !destination.started {
        lock(&state.pro().adoptions)
            .get_mut(workspace)
            .context("local copy destination disappeared")?
            .started = true;
        super::persist(state).await?;
    }
    Ok(())
}

pub(super) fn complete_copy(state: &AppState, workspace: &str) {
    if let Some(entry) = lock(&state.pro().adoptions).get_mut(workspace) {
        entry.complete = true;
    }
    lock(&state.pro().project_cache).checked_at = 0;
}

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
pub(crate) struct TakeoverRequest {
    expected_account_id: String,
    expected_endpoint: String,
    workspace_id: String,
    expected_epoch: u64,
}
async fn takeover(state: &Arc<AppState>, request: TakeoverRequest) -> Result<serde_json::Value> {
    ensure!(
        super::valid_id(&request.workspace_id),
        "Invalid project identity"
    );
    let (config, generation) = configuration(state).await;
    let config = config.context("Sign in before taking over execution")?;
    ensure!(
        config.account_id.as_deref() == Some(request.expected_account_id.as_str())
            && config.endpoint.trim_end_matches('/')
                == request.expected_endpoint.trim_end_matches('/')
            && account_matches(state, &request.workspace_id),
        "Account changed before Take over"
    );
    ensure!(
        config.role == Role::Device && config.execution.is_some(),
        "Take over requires the negotiated device protocol"
    );
    let workspace = lock(&state.workspaces)
        .get(&request.workspace_id)
        .context("Open a local copy before taking over")?;
    if super::may_execute(state, &workspace.id) {
        return Ok(
            json!({"workspace_id":workspace.id,"root":workspace.root,"name":workspace.name,"state":"owned_local"}),
        );
    }
    let mut intent = String::new();
    let prepared = async {
        let _configuration = state.pro().configuration.lock().await;
        ensure!(
            generation == state.pro().generation.load(Ordering::Acquire),
            "Account changed before Take over"
        );
        let (owner, id, root) = (state.clone(), workspace.id.clone(), workspace.root.clone());
        tokio::task::spawn_blocking(move || check_copy_destination(&owner, &id, &root)).await??;
        let pending_install =
            tokio::fs::try_exists(state.pro().root.join(&workspace.id).join("copy-install"))
                .await?;
        ensure!(
            !pending_install,
            "Finish the interrupted local copy before Take over"
        );
        {
            let mut preferences = lock(&state.pro().preferences);
            let copy = preferences
                .get_mut(&workspace.id)
                .and_then(|p| p.copy.as_mut())
                .context("Open a local copy before taking over")?;
            ensure!(
                copy.ready && copy.pending.is_none(),
                "Finish the local copy before Take over"
            );
            copy.takeover_requested = true;
            if copy.takeover_request.is_none() {
                copy.takeover_request = Some(chimaera_core::generate_token());
            }
            intent = copy
                .takeover_request
                .clone()
                .context("Takeover intent disappeared")?;
        }
        super::persist(state).await
    }
    .await;
    if let Err(error) = prepared {
        if !intent.is_empty() {
            let _ = super::project_copy::cancel_unstarted_takeover(
                state,
                &workspace.id,
                generation,
                &intent,
            )
            .await;
        }
        return Err(error);
    }
    // The existing validated move request owns the drain/acquire protocol. A
    // user-visible action supplies the observed epoch; no input triggers it.
    if let Err(error) =
        super::moves::take_over_here(state, &config, &workspace.id, request.expected_epoch).await
    {
        let _ = super::project_copy::cancel_unstarted_takeover(
            state,
            &workspace.id,
            generation,
            &intent,
        )
        .await;
        return Err(error);
    }
    ensure!(
        super::may_execute(state, &workspace.id),
        "Take over did not establish local execution"
    );
    Ok(
        json!({"workspace_id":workspace.id,"root":workspace.root,"name":workspace.name,"state":"owned_local"}),
    )
}
pub(crate) async fn takeover_project(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TakeoverRequest>,
) -> Response {
    let Ok(permit) = TAKEOVER_ACTIONS.try_acquire() else {
        return super::routes::failure_with_code(
            anyhow::anyhow!("Too many takeover requests are pending"),
            "busy",
        );
    };
    // The bounded intent and its cleanup outlive a disconnected caller.
    match tokio::spawn(async move {
        let _permit = permit;
        takeover(&state, request).await
    })
    .await
    {
        Ok(result) => match result {
            Ok(result) => Json(result).into_response(),
            Err(error) => {
                let code = open_error_code(&error);
                super::routes::failure_with_code(error, code)
            }
        },
        Err(error) => super::routes::failure(error.into()),
    }
}
