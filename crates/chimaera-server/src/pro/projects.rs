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
#[derive(Clone, Serialize)]
pub(super) struct Project {
    workspace_id: String,
    name: String,
    host_id: String,
    host_alias: String,
    local_root: Option<PathBuf>,
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
    lock(&state.pro.legacy_pending).contains(workspace)
        || lock(&state.pro.adoptions)
            .get(workspace)
            .is_some_and(|entry| !entry.complete)
}
fn local_root(state: &AppState, workspace: &str) -> Option<PathBuf> {
    if lock(&state.pro.legacy_pending).contains(workspace)
        || !account_matches(state, workspace)
        || (!lock(&state.pro.adoptions).contains_key(workspace)
            && lock(&state.pro.preferences)
                .get(workspace)
                .is_none_or(|entry| entry.account.is_none()))
    {
        return None;
    }
    lock(&state.workspaces)
        .get(workspace)
        .map(|entry| entry.root)
        .or_else(|| {
            lock(&state.pro.adoptions)
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
    let saved = lock(&state.pro.adoptions)
        .get(workspace)
        .map(|entry| entry.account.clone());
    let saved = saved.unwrap_or_else(|| {
        lock(&state.pro.preferences)
            .get(workspace)
            .and_then(|entry| entry.account.clone())
    });
    let Some(saved) = saved else {
        return !lock(&state.pro.adoptions).contains_key(workspace);
    };
    lock(&state.pro.runtime)
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
    let mut preferences = lock(&state.pro.preferences);
    ensure!(
        preferences.len() < MAX_PROJECTS || preferences.contains_key(workspace),
        "Local project limit reached"
    );
    let entry = preferences.entry(workspace.into()).or_default();
    ensure!(
        entry.account.as_ref().is_none_or(|saved| saved == &account),
        "This project belongs to another account"
    );
    entry.account = Some(account);
    Ok(())
}
async fn discover(state: &Arc<AppState>, config: &Configure) -> Result<Vec<Project>> {
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
                let previous = lock(&state.pro.project_cache).projects.clone();
                for mut project in previous
                    .into_iter()
                    .filter(|project| project.host_id == host.id)
                {
                    if projects.len() >= MAX_PROJECTS {
                        break;
                    }
                    if seen.insert(project.workspace_id.clone()) {
                        project.available = false;
                        project.error =
                            Some("Cloud project list is temporarily unavailable".into());
                        project.local_root = local_root(state, &project.workspace_id);
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
                workspace_id: row.id,
                name: row
                    .name
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(128)
                    .collect(),
                host_id: host.id.clone(),
                host_alias: host
                    .alias
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(128)
                    .collect(),
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
    let _guard = state.pro.configuration.lock().await;
    (
        lock(&state.pro.runtime).clone(),
        state.pro.generation.load(Ordering::Acquire),
    )
}
async fn list(state: &Arc<AppState>) -> Cache {
    let (config, generation) = configuration(state).await;
    let Some(config) = config.filter(|config| config.role == Role::Device) else {
        return Cache::default();
    };
    let _guard = state.pro.discovery.lock().await;
    if generation != state.pro.generation.load(Ordering::Acquire) {
        return Cache::default();
    }
    let now = super::now();
    let cached = lock(&state.pro.project_cache).clone();
    if cached.checked_at != 0 && now.saturating_sub(cached.checked_at) < 30 {
        return cached;
    }
    let result = tokio::time::timeout(Duration::from_secs(10), discover(state, &config)).await;
    if generation != state.pro.generation.load(Ordering::Acquire) {
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
    *lock(&state.pro.project_cache) = next.clone();
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
        "Choose an absolute project folder"
    );
    let metadata = std::fs::symlink_metadata(root)
        .context("The project folder is missing; restore it before opening this project")?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "The project folder must be a real directory"
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
        "This folder belongs to another project; choose a separate empty folder"
    );
    ensure!(
        std::fs::read_dir(&root)?.next().is_none(),
        "Choose an empty folder; existing files and repositories will not be replaced"
    );
    ensure!(
        !root
            .ancestors()
            .skip(1)
            .any(|parent| parent.join(".git").exists()),
        "Choose a folder outside an existing Git repository"
    );
    let probe = root.join(format!(
        ".chimaera-write-probe-{}",
        chimaera_core::generate_token()
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .context("The project folder is not writable")?;
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
        "The saved project folder has changed; restore its original location before opening"
    );
    if require_empty {
        ensure!(
            std::fs::read_dir(&root)?.next().is_none(),
            "The selected folder now contains files; nothing was imported"
        );
        ensure!(
            !root
                .ancestors()
                .skip(1)
                .any(|parent| parent.join(".git").exists()),
            "The selected folder is now inside another Git repository"
        );
    }
    Ok(())
}
/// Recheck immediately before filesystem installation, after potentially slow
/// downloads. A durable marker makes partial imports retryable only at this path.
pub(super) async fn begin_install(state: &AppState, workspace: &str, root: &Path) -> Result<()> {
    let Some(destination) = lock(&state.pro.adoptions).get(workspace).cloned() else {
        return Ok(());
    };
    ensure!(
        destination.root == root,
        "A project cannot change its saved local folder during import"
    );
    let workspaces = lock(&state.workspaces).list();
    ensure!(
        !workspaces.iter().any(|entry| entry.id != workspace
            && (root.starts_with(&entry.root) || entry.root.starts_with(root))),
        "The chosen folder now belongs to another project"
    );
    let check = destination.clone();
    tokio::task::spawn_blocking(move || verify(&check, !check.started)).await??;
    let legacy = lock(&state.pro.legacy_pending).remove(workspace);
    if !destination.started || legacy {
        if let Some(entry) = lock(&state.pro.adoptions).get_mut(workspace) {
            entry.started = true;
        }
        super::persist(state).await?;
        lock(&state.pro.project_cache).checked_at = 0;
    }
    Ok(())
}

async fn open(state: &Arc<AppState>, request: Open) -> Result<serde_json::Value> {
    ensure!(
        super::valid_id(&request.workspace_id),
        "Invalid project identity"
    );
    let (config, generation) = configuration(state).await;
    let config = config.context("Sign in to open a cloud project")?;
    ensure!(
        config.role == Role::Device,
        "Open locally is available on a personal device"
    );
    let account =
        account_scope(&config).context("Refresh sign-in before opening a cloud project")?;
    ensure!(
        config.account_id.as_deref() == Some(request.expected_account_id.as_str())
            && config.endpoint.trim_end_matches('/')
                == request.expected_endpoint.trim_end_matches('/'),
        "Account changed; open the project again"
    );
    let project = list(state)
        .await
        .projects
        .into_iter()
        .find(|row| row.workspace_id == request.workspace_id);
    let _jobs = state.pro.jobs.lock().await;
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed; open the project again"
    );
    let explicit_unbound_recovery = lock(&state.pro.adoptions)
        .get(&request.workspace_id)
        .is_some_and(|entry| {
            entry.account.is_none() && request.destination_root.as_ref() == Some(&entry.root)
        });
    ensure!(
        account_matches(state, &request.workspace_id) || explicit_unbound_recovery,
        "This saved project belongs to another account; its local folder will not be changed"
    );
    let existing = lock(&state.workspaces).get(&request.workspace_id);
    let saved = lock(&state.pro.adoptions)
        .get(&request.workspace_id)
        .filter(|entry| entry.started || entry.complete || request.destination_root.is_none())
        .cloned();
    let destination = if let Some(mut saved) = saved {
        ensure!(
            saved.account.as_ref().is_none_or(|owner| owner == &account),
            "This saved project belongs to another account; its local folder will not be changed"
        );
        if saved.account.is_none() {
            ensure!(
                request.destination_root.as_ref() == Some(&saved.root),
                "Explicitly select the original project folder to recover this earlier import"
            );
            saved.account = Some(account.clone());
            lock(&state.pro.adoptions).insert(request.workspace_id.clone(), saved.clone());
            super::persist(state).await?;
        }
        ensure!(
            request
                .destination_root
                .as_ref()
                .is_none_or(|root| root == &saved.root),
            "This project already has a saved local folder"
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
            "This project already has a saved local folder"
        );
        let legacy = lock(&state.pro.legacy_pending).contains(&request.workspace_id)
            || lock(&state.pro.preferences)
                .get(&request.workspace_id)
                .is_none_or(|entry| entry.account.is_none());
        ensure!(
            !legacy || request.destination_root.is_some(),
            "Explicitly select this existing project folder before importing cloud work into it"
        );
        let root = existing.root.clone();
        let (root, device, inode) =
            tokio::task::spawn_blocking(move || check_directory(&root)).await??;
        if legacy {
            lock(&state.pro.adoptions).insert(
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
        ensure!(
            project.is_some(),
            "Cloud project is not available in this account"
        );
        let root = request
            .destination_root
            .context("Choose where to save this project first")?;
        let workspaces = lock(&state.workspaces).list();
        let other_roots = lock(&state.pro.adoptions)
            .iter()
            .filter(|(id, _)| *id != &request.workspace_id)
            .map(|(_, entry)| entry.root.clone())
            .collect::<Vec<_>>();
        ensure!(
            other_roots.len() < MAX_PROJECTS,
            "Local project limit reached"
        );
        let mut destination =
            tokio::task::spawn_blocking(move || reserve(&root, &workspaces, &other_roots))
                .await??;
        destination.account = Some(account.clone());
        let root = destination.root.clone();
        lock(&state.pro.adoptions).insert(request.workspace_id.clone(), destination);
        super::persist(state).await?;
        lock(&state.pro.project_cache).checked_at = 0;
        root
    };
    if let Some(existing) = existing.filter(|_| {
        super::may_write(state, &request.workspace_id)
            && !adoption_pending(state, &request.workspace_id)
    }) {
        return Ok(json!({"workspace_id":existing.id,"root":existing.root,"name":existing.name}));
    }
    let baton: Baton = engine::account(
        &config,
        &format!("/v1/baton/{}", request.workspace_id),
        "GET",
        None,
    )
    .await?
    .json()?;
    ensure!(
        baton.workspace_id == request.workspace_id && !baton.mirror_disabled,
        "Mirroring is not available for this project"
    );
    // A restart verification must not download an older mirror over a locally
    // completed adoption. Reconcile the owned lease, then open existing files.
    if baton.holder_id.as_deref() == Some(&config.delegation.device_id)
        && !adoption_pending(state, &request.workspace_id)
        && lock(&state.workspaces).get(&request.workspace_id).is_some()
    {
        engine::reconcile(state, &config, &request.workspace_id).await?;
        if super::owned_epoch(state, &request.workspace_id).is_some() {
            let workspace = lock(&state.workspaces)
                .get(&request.workspace_id)
                .context("Local project is unavailable")?;
            return Ok(
                json!({"workspace_id":workspace.id,"root":workspace.root,"name":workspace.name}),
            );
        }
    }
    if let Some(holder) = baton
        .holder_id
        .as_deref()
        .filter(|holder| *holder != config.delegation.device_id)
    {
        let project = project
            .as_ref()
            .context("The cloud project is unavailable; try again when it reconnects")?;
        ensure!(
            holder == project.host_id,
            "This project is currently open on another device"
        );
        let response = transport::request(
            &config.keeper_url,
            &format!("/v1/hosts/{}/http/api/v1/pro/handoff", project.host_id),
            "POST",
            &config.delegation.access_token,
            Some(&json!({"workspace_id":request.workspace_id,"expected_epoch":baton.epoch})),
        )
        .await?;
        ensure!(
            response.status != 409,
            "The cloud project is busy; wait for a pause and try again"
        );
        ensure!(
            (200..300).contains(&response.status),
            "Cloud hand-back is temporarily unavailable"
        );
    }
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed; open the project again"
    );
    engine::hydrate(
        state,
        &config,
        &request.workspace_id,
        baton.epoch,
        false,
        Some(&destination),
    )
    .await?;
    if let Some(entry) = lock(&state.pro.adoptions).get_mut(&request.workspace_id) {
        entry.complete = true;
    }
    super::persist(state).await?;
    lock(&state.pro.project_cache).checked_at = 0;
    state.changes.notify_waiters();
    let workspace = lock(&state.workspaces)
        .get(&request.workspace_id)
        .context("Imported project registration is unavailable")?;
    Ok(json!({"workspace_id":workspace.id,"root":workspace.root,"name":workspace.name}))
}
pub(crate) async fn open_project(
    State(state): State<Arc<AppState>>,
    Json(request): Json<Open>,
) -> Response {
    // Bounded operation; caller cancellation may leave a fenced, retryable
    // partial import, never an automatic background adoption.
    match tokio::time::timeout(Duration::from_secs(19 * 60), open(&state, request)).await {
        Ok(Ok(project)) => Json(project).into_response(),
        Ok(Err(error)) => super::routes::failure(error),
        Err(_) => (StatusCode::GATEWAY_TIMEOUT, Json(json!({"error":"Project transfer timed out; retry Open on this Mac to continue at the saved folder"}))).into_response(),
    }
}

#[cfg(test)]
mod tests;
