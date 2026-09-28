use super::{
    config, mirror,
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
    let mut body = json!({"workspace_id":workspace});
    if let Some(epoch) = epoch {
        body["epoch"] = epoch.into();
    }
    let credentials: MirrorCredentials =
        account(config, "/v1/mirror/credentials", "POST", Some(&body))
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
                    if let Ok(delegation) = response.json() {
                        if let Some(runtime) = lock(&state.pro.runtime).as_mut() {
                            runtime.delegation = delegation;
                        }
                        renewed = super::now();
                    }
                }
            }
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
                    let _ = lazy_handback(&owner, &config).await;
                    let workspaces = lock(&owner.workspaces).list();
                    for workspace in workspaces
                        .into_iter()
                        .filter(|workspace| eligible(&owner, workspace))
                        .take(128)
                    {
                        if generation != owner.pro.generation.load(Ordering::Acquire) {
                            return;
                        }
                        if super::owned_epoch(&owner, &workspace.id).is_none()
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
    ensure!(
        super::projects::account_matches(state, workspace),
        "This project belongs to another account"
    );
    let baton: Baton = account(config, &format!("/v1/baton/{workspace}"), "GET", None)
        .await?
        .json()?;
    ensure!(baton.workspace_id == workspace, "baton workspace mismatch");
    super::projects::bind_workspace_account(state, config, workspace)?;
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
        if matches!(previous, Some(Ownership::Local { .. })) {
            suspend_workspace(state, workspace).await?;
        }
        return Ok(());
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
        return Ok(());
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
    let body = if operation == "renew" {
        json!({"holder_id":holder,"epoch":baton.epoch})
    } else {
        json!({"holder_id":holder,"expected_epoch":baton.epoch})
    };
    let grant: Baton = account(
        config,
        &format!("/v1/baton/{workspace}/{operation}"),
        "POST",
        Some(&body),
    )
    .await?
    .json()?;
    ensure!(
        grant.holder_id.as_deref() == Some(holder),
        "baton grant names another owner"
    );
    if transferring {
        return Ok(());
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
    if !matches!(previous, Some(Ownership::Local { .. })) {
        crate::ledger::resume_deferred_workspace(state, workspace).await?;
    }
    Ok(())
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
pub(super) async fn snapshot(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    clean: bool,
) -> Result<()> {
    let epoch = super::owned_epoch(state, workspace).context("workspace is not locally owned")?;
    let workspace = lock(&state.workspaces)
        .get(workspace)
        .context("unknown workspace")?;
    let grant = credentials(config, &workspace.id, Some(epoch)).await?;
    let root = state.pro.root.join(&workspace.id);
    let shadow = root.join("working-tree.git");
    mirror::initialize(&shadow).await?;
    let staging = root.join(format!("stage-{}", chimaera_core::generate_token()));
    tokio::fs::create_dir_all(&staging).await?;
    let result = async {
        let has_agents=!sessions(state,&workspace.id).is_empty();
        let session_ids:Vec<_>=lock(&state.session_workspaces).iter().filter(|(_,workspace_id)|*workspace_id==&workspace.id).map(|(id,_)|id.clone()).take(64).collect();
        let mut stopped=std::collections::HashMap::new();
        if clean {
            lock(&state.pro.ownership).insert(workspace.id.clone(),Ownership::Transferring{epoch});super::persist(state).await?;
            for id in &session_ids {
                let path=crate::bundle::export(state.clone(),id,crate::bundle::ExportMode::Stop).await?;
                let target=staging.join(format!("stopped-{id}.zip"));tokio::fs::rename(path,&target).await?;stopped.insert(id.clone(),target);
            }
        }
        let paths = mirror::inventory(&workspace.root, &shadow).await?;
        let project = workspace.root.clone(); let destination = staging.join("tree");
        let budget = grant.storage_limit_bytes; let max_file = grant.max_file_bytes;
        let report = tokio::task::spawn_blocking(move || mirror::copy_tree(&project, &destination, paths, budget, max_file)).await??;
        let home = state.claude_settings_path.parent().and_then(Path::parent).context("agent home unavailable")?.to_path_buf();
        let sources = config::Sources { home, claude:state.claude_settings_path.parent().unwrap().to_path_buf(), codex:state.codex_config_path.parent().context("codex home unavailable")?.to_path_buf(), workspace:workspace.root.clone() };
        let destination = staging.join("config");
        let config_report = tokio::task::spawn_blocking(move || config::export(sources, &destination, budget.saturating_sub(report.bytes))).await??;
        let mut profile = lock(&state.pro.preferences).entry(workspace.id.clone()).or_default().profile.clone();
        profile.missing_environment = config_report.missing_environment;
        let command_sessions:Vec<_>=lock(&state.session_workspaces).iter().filter(|(_,id)|*id==&workspace.id).map(|(id,_)|id.clone()).take(64).collect();
        for id in command_sessions {if let Some(marks)=state.sessions.marks(&id) {for command in marks.journal(32) {if let Some(command)=command.command.as_deref(){profile.observe_command(command);}}}}
        let handoff = staging.join("handoff"); tokio::fs::create_dir_all(handoff.join("bundles")).await?;
        let mut archives = Vec::new(); let mut archive_bytes = 0u64;
        for id in session_ids {
            let path = if clean {stopped.remove(&id).context("stopped archive missing")?} else {crate::bundle::export(state.clone(), &id, crate::bundle::ExportMode::Snapshot).await?};
            let length = tokio::fs::metadata(&path).await?.len();
            if length > max_file { let _ = tokio::fs::remove_file(path).await; anyhow::bail!("session archive exceeds mirror file limit"); }
            archive_bytes = archive_bytes.saturating_add(length);
            ensure!(archive_bytes + report.bytes + config_report.bytes <= budget, "workspace and conversations exceed mirror storage quota");
            let archive = format!("bundles/{id}.zip");
            tokio::fs::rename(path, handoff.join(&archive)).await?;
            archives.push(SessionArchive {id,archive});
        }
        let branch=transport::git_output(transport::git(&workspace.root,None),&["symbolic-ref","-q","HEAD"],vec![]).await.ok().and_then(|bytes|String::from_utf8(bytes).ok()).map(|text|text.trim().to_string());
        let repository_origin=mirror::repository_origin(&workspace.root).await;
        let repository=super::repository::capture(&workspace.root).await?;
        let manifest = Manifest {version:1,branch,repository_origin,repository,workspace_id:workspace.id.clone(),root:workspace.root.clone(),name:workspace.name.clone(),epoch,clean,profile:profile.clone(),sessions:archives};
        tokio::fs::write(handoff.join("manifest.json"), serde_json::to_vec(&manifest)?).await?;
        mirror::mirror_repository(&workspace.root, &root.join("repository.git"), &grant).await?;
        mirror::commit_tree(&shadow, &staging.join("tree"), "main").await?;
        mirror::commit_tree(&shadow, &staging.join("config"), "config").await?;
        mirror::commit_tree(&shadow, &handoff, "handoff").await?;
        mirror::push(&shadow, &grant, &["refs/heads/main", "refs/heads/config", "refs/heads/handoff"]).await?;
        let policy = account(config, &format!("/v1/baton/{}/policy", workspace.id), "PUT", Some(&json!({"holder_id":config.delegation.device_id,"epoch":epoch,"handoff_enabled":!config.hours_exhausted,"offline_takeover":!config.hours_exhausted,"has_agents":has_agents}))).await?;
        ensure!((200..300).contains(&policy.status), "mirror policy update failed");
        lock(&state.pro.preferences).entry(workspace.id.clone()).or_default().profile = profile;
        lock(&state.pro.status).insert(workspace.id.clone(), WorkspaceStatus {report,last_mirrored_at:Some(super::now()),storage_limit_bytes:budget,error:None});
        super::persist(state).await?;
        if clean {
            let released = account(config, &format!("/v1/baton/{}/release",workspace.id), "POST", Some(&json!({"holder_id":config.delegation.device_id,"epoch":epoch}))).await?;
            ensure!((200..300).contains(&released.status), "workspace release failed");
        }
        Ok::<_,anyhow::Error>(())
    }.await;
    let _ = tokio::fs::remove_dir_all(staging).await;
    if result.is_err() && clean {
        // A failed flush must not strand a stopped laptop agent. Verify that
        // the baton is still ours before resuming its durable suspended entry.
        lock(&state.pro.ownership).insert(
            workspace.id.clone(),
            Ownership::AwaitingVerification { epoch },
        );
        let _ = reconcile(state, config, &workspace.id).await;
    }
    result
}

pub(super) async fn fetch_snapshot(
    config: &Configure,
    workspace: &str,
    cache: &Path,
) -> Result<Manifest> {
    let grant = credentials(config, workspace, None).await?;
    mirror::initialize(cache).await?;
    let url = transport::endpoint(&grant.working_tree_url)?;
    transport::git_output(
        transport::git(cache, Some((&grant.username, &grant.password))),
        &["fetch", "--no-tags", &url, "+refs/heads/*:refs/heads/*"],
        vec![],
    )
    .await?;
    let bytes = transport::git_output(
        transport::git(cache, None),
        &["show", "refs/heads/handoff:manifest.json"],
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
    let current = || -> Result<()> {
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
        && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::Local{epoch}|Ownership::AwaitingVerification{epoch}) if *epoch==expected_epoch)
    {
        return reconcile(state, config, workspace).await;
    }
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
    let existing: Baton = account(config, &format!("/v1/baton/{workspace}"), "GET", None)
        .await?
        .json()?;
    let grant: Baton = if existing.holder_id.as_deref() == Some(&config.delegation.device_id)
        && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::Hydrating{epoch}) if *epoch==existing.epoch)
    {
        account(
            config,
            &format!("/v1/baton/{workspace}/renew"),
            "POST",
            Some(&json!({"holder_id":config.delegation.device_id,"epoch":existing.epoch})),
        )
        .await?
        .json()?
    } else {
        account(
            config,
            &format!("/v1/baton/{workspace}/acquire"),
            "POST",
            Some(&json!({"holder_id":config.delegation.device_id,"expected_epoch":expected_epoch})),
        )
        .await?
        .json()?
    };
    ensure!(
        grant.holder_id.as_deref() == Some(&config.delegation.device_id),
        "invalid handoff ownership grant"
    );
    current()?;
    lock(&state.pro.ownership).insert(
        workspace.into(),
        Ownership::Hydrating { epoch: grant.epoch },
    );
    super::persist(state).await?;
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
                &format!("refs/heads/{branch}"),
                read_grant.storage_limit_bytes,
                read_grant.max_file_bytes,
            )
            .await?;
            let destination = stage.join(folder);
            tokio::fs::create_dir_all(&destination).await?;
            let mut command = transport::git(&cache, None);
            command.env("GIT_WORK_TREE", &destination);
            transport::git_output(
                command,
                &[
                    "--work-tree",
                    destination.to_str().context("invalid stage path")?,
                    "checkout",
                    &format!("refs/heads/{branch}"),
                    "--",
                    ".",
                ],
                vec![],
            )
            .await?;
        }
        current()?;
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
        let has_baseline = tokio::fs::try_exists(local_shadow.join("HEAD")).await?;
        if has_baseline {
            tokio::fs::create_dir_all(&baseline).await?;
            let mut command = transport::git(&local_shadow, None);
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
        let tree = stage.join("tree");
        let destination = destination_root.clone();
        tokio::task::spawn_blocking(move || {
            install_tree(
                &tree,
                &destination,
                has_baseline.then_some(baseline).as_deref(),
            )
        })
        .await??;
        mirror::initialize(&local_shadow).await?;
        transport::git_output(
            transport::git(&local_shadow, None),
            &[
                "fetch",
                "--no-tags",
                cache.to_str().context("invalid cache path")?,
                "+refs/heads/*:refs/heads/*",
            ],
            vec![],
        )
        .await?;
        let overlay = stage.join("config");
        let home = state
            .claude_settings_path
            .parent()
            .and_then(Path::parent)
            .context("agent home unavailable")?
            .to_path_buf();
        current()?;
        let account_state = state.clone();
        tokio::task::spawn_blocking(move || {
            ensure!(
                generation == account_state.pro.generation.load(Ordering::Acquire),
                "Account changed during project transfer"
            );
            config::import(&overlay, &home)
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
        };
        let owner = state.clone();
        tokio::task::spawn_blocking(move || lock(&owner.workspaces).import_exact(new_workspace))
            .await??;
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
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(stage).await;
    result
}
fn install_tree(source: &Path, destination: &Path, baseline: Option<&Path>) -> Result<()> {
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
                if safe && target.try_exists()? && same_file(&target, &entry.path())? {
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
                if target.exists()
                    && !same_file(&target, &entry.path())?
                    && !baseline.is_some_and(|root| {
                        same_file(&target, &root.join(&relative)).unwrap_or(false)
                    })
                {
                    let preserved = target.with_file_name(format!(
                        "{}.cloud-{}",
                        entry.file_name().to_string_lossy(),
                        super::now()
                    ));
                    std::fs::copy(entry.path(), preserved)?;
                } else {
                    std::fs::copy(entry.path(), target)?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn at_pause(state: &AppState, workspace: &str) -> bool {
    sessions(state, workspace).into_iter().all(|id| {
        if let Some(chat) = state.chat.get(&id) {
            chat.background_running == 0
                && (chat.pending_permission
                    || chat.status_needs_action
                    || chat.status_category.as_deref() == Some("idle"))
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
pub(super) async fn lazy_handback(state: &Arc<AppState>, config: &Configure) -> Result<()> {
    if config.role != Role::Device
        || !state.pro.power_suitable.load(Ordering::Acquire)
        || super::now().saturating_sub(state.pro.awake_since.load(Ordering::Acquire)) < 300
    {
        return Ok(());
    }
    let remote: Vec<_> = lock(&state.pro.ownership)
        .iter()
        .filter_map(|(id, owner)| match owner {
            Ownership::Remote { epoch, holder } => Some((id.clone(), *epoch, holder.clone())),
            _ => None,
        })
        .take(128)
        .collect();
    let hosts: Vec<super::protocol::Host> = transport::request(
        &config.keeper_url,
        "/v1/hosts",
        "GET",
        &config.delegation.access_token,
        None,
    )
    .await?
    .json()?;
    for (workspace, epoch, holder) in remote {
        // Only a project already registered on this device may return automatically.
        // Discovery and old global-folder preferences never authorize adoption.
        if lock(&state.workspaces).get(&workspace).is_none()
            || super::projects::adoption_pending(state, &workspace)
            || !super::projects::account_matches(state, &workspace)
        {
            continue;
        }
        if lock(&state.pro.preferences)
            .get(&workspace)
            .is_some_and(|p| p.never_mirror)
        {
            continue;
        }
        let Some(host) = hosts.iter().find(|host| {
            host.id == holder
                && super::valid_id(&host.id)
                && host.kind == "worker"
                && host.status == "connected"
        }) else {
            continue;
        };
        let response = transport::request(
            &config.keeper_url,
            &format!("/v1/hosts/{}/http/api/v1/pro/handoff", host.id),
            "POST",
            &config.delegation.access_token,
            Some(&json!({"workspace_id":workspace,"expected_epoch":epoch})),
        )
        .await?;
        if response.status == 409 {
            continue;
        }
        ensure!(
            (200..300).contains(&response.status),
            "cloud hand-back is unavailable"
        );
        hydrate(state, config, &workspace, epoch, false, None).await?;
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
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire)
            && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::SettingUp {epoch:current}) if *current==epoch),
        "Project ownership changed during setup"
    );
    lock(&state.pro.ownership).insert(workspace.into(), Ownership::Local { epoch });
    super::persist(state).await?;
    if let Some(status) = lock(&state.pro.status).get_mut(workspace) {
        status.error = None;
    }
    crate::ledger::resume_deferred_workspace(state, workspace).await?;
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

pub(super) fn eligible(state: &AppState, workspace: &crate::workspaces::Workspace) -> bool {
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
            account_id: None,
            role: Role::Device,
            endpoint: String::new(),
            keeper_url: String::new(),
            hours_exhausted: false,
            delegation: super::super::protocol::Delegation {
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
        install_tree(&cloud, &local, Some(&base)).unwrap();
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
}
