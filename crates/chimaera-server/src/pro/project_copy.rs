//! Local copies consume an immutable read grant, never an execution lease.
use super::{execution::wire::Checkpoint, Preference};
use crate::{lock, AppState};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, io::Read, path::Path, sync::Arc};
use tokio::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CopyState {
    pub checkpoint: Option<Checkpoint>,
    #[serde(default)]
    pub pending: Option<Checkpoint>,
    pub ready: bool,
    /// Only the explicit takeover route may set this durable admission.
    pub takeover_requested: bool,
    #[serde(default)]
    pub takeover_request: Option<String>,
    #[serde(default)]
    pub owner_epoch: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Latch {
    version: u16,
    workspaces: std::collections::BTreeSet<String>,
}
pub(super) struct Enrollment {
    ids: std::sync::Mutex<HashSet<String>>,
    unknown: bool,
    written: Arc<Mutex<Option<Vec<u8>>>>,
}
impl Enrollment {
    pub(super) fn load(root: &Path) -> Self {
        let loaded = (|| -> Result<Option<(HashSet<String>, Vec<u8>)>> {
            let path = root.join("copy-authority.json");
            match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                other => {
                    ensure!(other?.file_type().is_file(), "invalid copy enrollment");
                }
            }
            let (file, _) = crate::fs::open_regular(&path)?;
            let mut bytes = Vec::new();
            file.take(16385).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 16384, "copy enrollment exceeds limit");
            let latch: Latch = serde_json::from_slice(&bytes)?;
            ensure!(
                latch.version == 1
                    && latch.workspaces.len() <= 128
                    && latch.workspaces.iter().all(|id| super::valid_id(id)),
                "invalid copy enrollment"
            );
            Ok(Some((latch.workspaces.into_iter().collect(), bytes)))
        })();
        let unknown = loaded.is_err();
        let (ids, written) = loaded
            .ok()
            .flatten()
            .map_or_else(|| (HashSet::new(), None), |(ids, bytes)| (ids, Some(bytes)));
        Self {
            ids: std::sync::Mutex::new(ids),
            unknown,
            written: Arc::new(Mutex::new(written)),
        }
    }
}

pub(super) fn copy_only(state: &AppState, workspace: &str) -> bool {
    if lock(&state.pro.copies.ids).contains(workspace) {
        return true;
    }
    let preferences = lock(&state.pro.preferences);
    let known = preferences.get(workspace);
    if known.is_some_and(|entry| entry.copy.is_some()) {
        return true;
    }
    let known_account = known.is_some_and(|entry| entry.account.is_some());
    drop(preferences);
    // An unreadable copy latch keeps every project Pro has a record of
    // read-only. Only when Pro's own records are unreadable too can a copy
    // hide among ordinary projects, and only then is every registered one
    // fenced; a project Pro never enrolled is otherwise never touched.
    state.pro.copies.unknown
        && (known_account
            || lock(&state.pro.adoptions).contains_key(workspace)
            || super::execution::managed(state, workspace)
            || state.pro.records_unknown
                && state
                    .workspaces
                    .try_lock()
                    .map_or(true, |workspaces| workspaces.get(workspace).is_some()))
}

/// State corruption cannot discard an enrolled copy's execution restriction.
/// The owned I/O gate remains in the blocking writer if its caller disappears.
pub(super) async fn persist_latch(state: &AppState) -> Result<()> {
    let mut written = state.pro.copies.written.clone().lock_owned().await;
    let bytes = {
        let mut ids = lock(&state.pro.copies.ids);
        ids.extend(
            lock(&state.pro.preferences)
                .iter()
                .filter(|(_, entry)| entry.copy.is_some())
                .map(|(id, _)| id.clone()),
        );
        ensure!(!state.pro.copies.unknown, "copy enrollment is unreadable");
        if ids.is_empty() && written.is_none() {
            return Ok(());
        }
        ensure!(ids.len() <= 128, "copy enrollment exceeds limit");
        serde_json::to_vec(&Latch {
            version: 1,
            workspaces: ids.iter().cloned().collect(),
        })?
    };
    ensure!(bytes.len() <= 16384, "copy enrollment exceeds limit");
    if written.as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    let path = state.pro.root.join("copy-authority.json");
    tokio::task::spawn_blocking(move || -> Result<()> {
        crate::persist::atomic_write_json_durable(&path, bytes.clone())?;
        *written = Some(bytes);
        Ok(())
    })
    .await??;
    Ok(())
}

pub(super) fn ready(preference: &Preference) -> bool {
    preference.copy.as_ref().is_some_and(|copy| copy.ready)
}

#[derive(Serialize, Deserialize)]
struct Report {
    name: String,
    branches: Vec<String>,
    kept: (usize, Vec<std::path::PathBuf>),
    staging: super::repository::StagingStatus,
}

/// Carry unresolved local-copy conflicts into its next copy or takeover.
/// Call only while preparing a new transaction: the combined report is saved
/// in that transaction's stage, so a retry cannot count its files twice.
pub(super) fn carry_kept(
    state: &AppState,
    workspace: &str,
    kept: (usize, Vec<std::path::PathBuf>),
) -> (usize, Vec<std::path::PathBuf>) {
    if !copy_only(state, workspace) {
        return kept;
    }
    let statuses = lock(&state.pro.status);
    let Some(previous) = statuses
        .get(workspace)
        .filter(|status| status.kept_both.is_some())
    else {
        return kept;
    };
    let mut paths = previous.kept_paths.clone();
    let duplicates = kept.1.iter().filter(|path| paths.contains(path)).count();
    let count = previous
        .kept_both
        .unwrap_or(0)
        .saturating_add(kept.0.saturating_sub(duplicates));
    for path in kept.1 {
        if paths.len() >= 32 {
            break;
        }
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    (count, paths)
}

/// Consume only the chosen immutable publication. Neither this path nor its
/// retry installs sessions, agent configuration, a proof or a preferred home.
pub(super) struct Selection {
    pub checkpoint: Checkpoint,
    pub holder: Option<String>,
    pub epoch: u64,
}

pub(super) async fn sync(
    state: &Arc<AppState>,
    config: &super::protocol::Configure,
    workspace: &str,
    destination: std::path::PathBuf,
    generation: u64,
    selection: Selection,
) -> Result<serde_json::Value> {
    use super::{engine, execution, install, repository, transport};
    use anyhow::Context;
    use std::sync::atomic::Ordering;
    let Selection {
        checkpoint: latest,
        holder,
        epoch,
    } = selection;
    execution::receipt::validate(&latest)?;
    let cache_guard = Arc::new(state.pro.cache(workspace)?.lock_owned().await);
    transport::cache_quiescent(workspace)?;
    let transfer = super::transfer_dispatch::TransferScope::capture(
        state,
        workspace,
        Some(&destination),
        cache_guard.clone(),
        generation,
    )
    .await?;
    let original_transfer = transfer.host.clone();
    transport::cache_scope(workspace, cache_guard, super::transfer_dispatch::scope(transfer, async {
        let existing_transaction=tokio::fs::try_exists(state.pro.root.join(workspace).join("copy-install")).await?;
        let selected = {
            let _configuration = state.pro.configuration.lock().await;
            ensure!(generation == state.pro.generation.load(Ordering::Acquire), "Account changed during copy selection");
            ensure!(super::projects::account_matches(state, workspace), "Account changed during copy selection");
            ensure!(!live_processes(state,workspace), "A live local project process prevents copy installation");
            let selected = {
                let mut preferences = lock(&state.pro.preferences);
                let preference = preferences.entry(workspace.to_owned()).or_default();
                ensure!(!preference.copy.as_ref().is_some_and(|copy|copy.takeover_requested), "Take over is already in progress");
                preference.account = config.account_id.as_ref().map(|account|format!("{}/{}",config.endpoint.trim_end_matches('/'),account));
                let copy = preference.copy.get_or_insert(CopyState {checkpoint:None,pending:None,ready:false,takeover_requested:false,takeover_request:None,owner_epoch:Some(epoch)});
                let selected = copy.pending.clone().or_else(|| existing_transaction.then(||copy.checkpoint.clone()).flatten()).unwrap_or(latest);
                copy.pending = Some(selected.clone());
                copy.ready = false;
                copy.owner_epoch=Some(epoch);
                selected
            };
            if let Some(holder)=holder.as_ref().filter(|holder|*holder!=&config.delegation.device_id) {
                lock(&state.pro.ownership).insert(workspace.to_owned(),super::Ownership::Remote {epoch,holder:holder.clone()});
            }
            // Older daemons understand this fence even though they do not know
            // the additive copy role. Never clear it during a copy import.
            lock(&state.pro.legacy_pending).insert(workspace.to_owned());
            super::persist(state).await?;
            selected
        };
        let current = || -> Result<()> {
            ensure!(generation == state.pro.generation.load(Ordering::Acquire)
                && super::projects::account_matches(state, workspace)
                && lock(&state.pro.preferences).get(workspace).and_then(|p|p.copy.as_ref()).is_some_and(|copy|!copy.takeover_requested && copy.pending.as_ref()==Some(&selected)), "Local copy authority changed");
            Ok(())
        };
        current()?;
        let cache = state.pro.root.join(workspace).join("copy-incoming.git");
        let manifest = engine::fetch_snapshot_at(config, workspace, &cache, Some(&selected)).await?;
        let grant = engine::credentials(config, workspace, None).await?;
        ensure!(grant.read_only, "Local copy requires a read-only grant");
        let stage = state.pro.root.join(workspace).join("copy-stage");
        let transaction_root = state.pro.root.join(workspace).join("copy-install");
        let binding = install::Binding {endpoint:config.endpoint.clone(),account:config.account_id.clone(),workspace:workspace.to_owned(),epoch:selected.source_epoch,receipt:Some(selected.id.clone())};
        let (path, bound) = (transaction_root.clone(), binding.clone());
        let mut transaction = tokio::task::spawn_blocking(move ||install::Transaction::open(&path,&bound)).await??;
        let report = if transaction.is_none() {
            if tokio::fs::try_exists(&stage).await? { tokio::fs::remove_dir_all(&stage).await?; }
            let private = stage.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                use std::os::unix::fs::PermissionsExt;
                std::fs::create_dir_all(&private)?;
                std::fs::set_permissions(&private,std::fs::Permissions::from_mode(0o700))?;
                Ok(())
            }).await??;
            let mut incoming_bytes=0u64;
            for branch in ["main","config","handoff"] {
                incoming_bytes=incoming_bytes.checked_add(super::mirror::validate_tree_bytes(&cache,execution::receipt::revision(Some(&selected),branch)?,grant.storage_limit_bytes.min(1024*1024*1024),grant.max_file_bytes).await?).context("copy checkpoint exceeds quota")?;
            }
            ensure!(incoming_bytes<=grant.storage_limit_bytes,"combined copy checkpoint exceeds quota");
            for (branch,folder) in [("main","tree"),("handoff","handoff")] {
                checkout_tree(&cache,execution::receipt::revision(Some(&selected),branch)?,&stage.join(folder),&grant).await?;
            }
            current()?;
            super::projects::begin_copy_install(state,workspace,&destination).await?;
            let (root,before,checkout)=(destination.clone(),stage.join("tree-before"),stage.join("checkout"));
            let budget=grant.storage_limit_bytes;
            tokio::task::spawn_blocking(move || -> Result<()> {
                install::snapshot(&root,&before,&|path|super::policy::allowed_path(path)||path.file_name().and_then(|name|name.to_str()).is_some_and(super::canonical::kept_copy_name),budget)?;
                install::snapshot(&before,&checkout,&|_|true,budget)
            }).await??;
            let baseline = lock(&state.pro.preferences).get(workspace).and_then(|p|p.copy.as_ref()).and_then(|copy|copy.checkpoint.clone());
            let mut baseline_snapshot=None;
            if let Some(baseline)=&baseline {
                let old_manifest=engine::fetch_snapshot_at(config,workspace,&cache,Some(baseline)).await?;
                checkout_tree(&cache,&baseline.working_tree_oid,&stage.join("baseline"),&grant).await?;
                checkout_tree(&cache,&baseline.handoff_oid,&stage.join("baseline-handoff"),&grant).await?;
                baseline_snapshot=old_manifest.repository.and_then(|repository|repository.staging);
            }
            current()?;
            let prepared=repository::prepare_receive(&destination,&stage.join("checkout"),&stage.join("repository"),repository::Incoming {
                cache:&state.pro.root.join(workspace).join("copy-repository.git"),credentials:&grant,branch:manifest.branch.as_deref(),origin:manifest.repository_origin.as_deref(),snapshot:manifest.repository.as_ref(),
                staging:Some(repository::StagingIncoming {handoff:&stage.join("handoff"),baseline:baseline_snapshot.as_ref().map(|descriptor|(stage.join("baseline-handoff"),descriptor)).as_ref().map(|(path,descriptor)|(path.as_path(),*descriptor))}),
            },&current).await?;
            let (tree,checkout,base,left_out)=(stage.join("tree"),stage.join("checkout"),baseline.as_ref().map(|_|stage.join("baseline")),manifest.left_out);
            let kept=tokio::task::spawn_blocking(move ||engine::install_tree(&tree,&checkout,base.as_deref(),left_out.as_deref())).await??;
            let mut writes=prepared.writes;
            let (root,before,checkout)=(destination.clone(),stage.join("tree-before"),stage.join("checkout"));
            writes.extend(tokio::task::spawn_blocking(move || -> Result<_> {
                let mut writes=install::changes(&root,&before,&checkout)?;
                writes.retain(|write|!write.relative.starts_with(".git"));
                Ok(writes)
            }).await??);
            let (root,checkout,marker,id)=(destination.clone(),stage.join("checkout"),stage.join("marker"),workspace.to_owned());
            writes.push(tokio::task::spawn_blocking(move ||engine::prepare_marker(&root,&checkout,&marker,&id)).await??);
            let kept=carry_kept(state,workspace,kept);
            let report=Report {name:manifest.name,branches:prepared.branches,kept,staging:prepared.staging};
            let report_path=stage.join("report.json");
            let bytes=serde_json::to_vec(&report)?;
            tokio::task::spawn_blocking(move ||crate::persist::atomic_write_json_durable(&report_path,bytes)).await??;
            let checked=stage.clone();
            tokio::task::spawn_blocking(move ||install::stage_budget(&checked)).await??;
            let (path,budget)=(transaction_root.clone(),grant.storage_limit_bytes);
            transaction=Some(tokio::task::spawn_blocking(move ||install::Transaction::prepare(&path,binding,writes,budget)).await??);
            report
        } else {
            let report_path=stage.join("report.json");
            tokio::task::spawn_blocking(move || -> Result<Report> {
                let (file,meta)=crate::fs::open_regular(&report_path)?;
                ensure!(meta.len()<=1024*1024,"copy report exceeds limit");
                Ok(serde_json::from_reader(file)?)
            }).await??
        };
        current()?;
        let mut transaction=transaction.take().context("copy installation unavailable")?;
        let git_roots=repository::install_roots(&destination).await?;
        let guard=super::mutation::begin_copy(state,workspace,generation,selected.clone(),destination.clone()).await?;
        let owner=state.clone();
        let id=workspace.to_owned();
        // Commit and its durable ready receipt have one owner even when the
        // browser disappears. Existing user edits never become a new baseline
        // until the transaction committed its exact before/after images.
        let retained_transfer = original_transfer.clone();
        let result=tokio::spawn(async move {
            let _retained_transfer = retained_transfer;
            guard.check(&owner)?;
            let worker=owner.clone();
            let (transaction,guard)=tokio::task::spawn_blocking(move || -> Result<_> {
                if !transaction.committed() {
                    transaction.reserve_git(git_roots,&||guard.check_files(&worker))?;
                    transaction.apply(&||guard.check_files(&worker))?;
                    transaction.commit(&||guard.check_files(&worker))?;
                }
                Ok((transaction,guard))
            }).await??;
            guard.check(&owner)?;
            let workspace=crate::workspaces::Workspace {id:id.clone(),root:destination.clone(),name:report.name.clone(),last_opened_at:super::now(),mastermind:None,plugins_on:Vec::new(),cloud_internal:false,hidden:false};
            let registered=owner.clone();
            let guard=tokio::task::spawn_blocking(move || -> Result<_> {
                guard.check_files(&registered)?;
                lock(&registered.workspaces).import_exact(workspace)?;
                Ok(guard)
            }).await??;
            {
                let mut preferences=lock(&owner.pro.preferences);
                let preference=preferences.get_mut(&id).context("copy receipt disappeared")?;
                preference.copy=Some(CopyState {checkpoint:Some(selected.clone()),pending:None,ready:true,takeover_requested:false,takeover_request:None,owner_epoch:Some(epoch)});
                preference.git_staging=Some(report.staging.clone());
                preference.git_branches=report.branches.clone();
            }
            super::report_return(&owner,&id,report.kept.clone(),&report.branches);
            super::projects::complete_copy(&owner,&id);
            // Failed persistence retains the completed transaction for exact
            // retry; an in-memory ready copy still cannot execute or publish.
            super::persist(&owner).await?;
            drop(guard);
            tokio::task::spawn_blocking(move ||transaction.cleanup()).await??;
            owner.changes.notify_waiters();
            Ok::<_,anyhow::Error>(serde_json::json!({"copy_version":1,"state":"local_copy","workspace_id":id,"root":destination,"name":report.name,"checkpoint":selected,"git_staging":report.staging,"kept_files":report.kept.0,"local_copy":view(&owner,&id)}))
        }).await??;
        Ok(result)
    })).await
}

async fn checkout_tree(
    cache: &Path,
    revision: &str,
    destination: &Path,
    grant: &super::protocol::MirrorCredentials,
) -> Result<()> {
    use anyhow::Context;
    super::mirror::validate_tree(
        cache,
        revision,
        grant.storage_limit_bytes.min(1024 * 1024 * 1024),
        grant.max_file_bytes,
    )
    .await?;
    tokio::fs::create_dir_all(destination).await?;
    let mut command = super::transport::git(cache, None).await?;
    command.env("GIT_WORK_TREE", destination);
    super::transport::git_output(
        command,
        &[
            "--work-tree",
            destination.to_str().context("invalid copy stage path")?,
            "checkout",
            revision,
            "--",
            ".",
        ],
        vec![],
    )
    .await?;
    Ok(())
}

/// Only an admitted, committed takeover may retire the independent copy fence.
/// The independent latch retires only after durable SettingUp; an interrupted
/// retirement remains recovery-needed rather than relying on ordinary state.
pub(super) async fn promote(
    state: &AppState,
    workspace: &str,
    guard: &super::mutation::ImportGuard,
) -> Result<()> {
    use anyhow::Context;
    guard.check(state)?;
    let previous = lock(&state.pro.preferences)
        .get(workspace)
        .and_then(|p| p.copy.clone());
    if previous.is_none() {
        ensure!(
            !copy_only(state, workspace),
            "Local copy authority needs explicit recovery"
        );
        return guard.setting_up(state);
    }
    ensure!(
        previous
            .as_ref()
            .is_some_and(|copy| copy.takeover_requested),
        "Explicit Take over is required"
    );
    guard.setting_up(state)?;
    lock(&state.pro.preferences)
        .get_mut(workspace)
        .context("copy authority disappeared")?
        .copy = None;
    lock(&state.pro.legacy_pending).remove(workspace);
    // The independent enrollment stays in place until the new role/SettingUp
    // record is durable. A crash in this window is recovery-needed, never an
    // unguarded legacy fallback after ordinary state damage.
    let finish = async {
        super::persist(state).await?;
        guard.check_setting_up(state)?;
        let mut written = state.pro.copies.written.clone().lock_owned().await;
        let ids = {
            ensure!(!state.pro.copies.unknown, "copy enrollment is unreadable");
            let mut ids = lock(&state.pro.copies.ids).clone();
            ids.remove(workspace);
            ids
        };
        let bytes = serde_json::to_vec(&Latch {
            version: 1,
            workspaces: ids.iter().cloned().collect(),
        })?;
        let path = state.pro.root.join("copy-authority.json");
        tokio::task::spawn_blocking(move || -> Result<()> {
            crate::persist::atomic_write_json_durable(&path, bytes.clone())?;
            *written = Some(bytes);
            Ok(())
        })
        .await??;
        *lock(&state.pro.copies.ids) = ids;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = finish {
        lock(&state.pro.preferences)
            .entry(workspace.to_owned())
            .or_default()
            .copy = previous;
        lock(&state.pro.legacy_pending).insert(workspace.to_owned());
        lock(&state.pro.copies.ids).insert(workspace.to_owned());
        let ownership = lock(&state.pro.ownership).get(workspace).cloned();
        if let Some(super::Ownership::SettingUp { epoch }) = ownership {
            lock(&state.pro.ownership)
                .insert(workspace.to_owned(), super::Ownership::Hydrating { epoch });
        }
        let _ = persist_latch(state).await;
        return Err(error);
    }
    Ok(())
}

pub(super) fn view(state: &AppState, workspace: &str) -> Option<serde_json::Value> {
    if !copy_only(state, workspace) {
        return None;
    }
    let copy = lock(&state.pro.preferences)
        .get(workspace)
        .and_then(|p| p.copy.clone());
    Some(match copy {
        Some(copy) => {
            serde_json::json!({"state":if copy.takeover_requested {"taking_over"} else if copy.ready {"ready"} else {"pending"},"ready":copy.ready,"checkpoint":copy.checkpoint,"owner_epoch":copy.owner_epoch})
        }
        None => serde_json::json!({"state":"recovery_needed","ready":false}),
    })
}

#[cfg(test)]
mod tests;

/// A failed bounded move retires only its own intent. A later explicit request
/// must never be cleared by an earlier asynchronous completion.
pub(super) async fn cancel_takeover(
    state: &Arc<AppState>,
    workspace: &str,
    generation: u64,
    request: &str,
) -> Result<()> {
    retire_takeover(state, workspace, generation, request, false).await
}

/// A route which failed before starting must not invalidate a concurrent route
/// already attached to the same active pull.
pub(super) async fn cancel_unstarted_takeover(
    state: &Arc<AppState>,
    workspace: &str,
    generation: u64,
    request: &str,
) -> Result<()> {
    retire_takeover(state, workspace, generation, request, true).await
}

async fn retire_takeover(
    state: &Arc<AppState>,
    workspace: &str,
    generation: u64,
    request: &str,
    only_idle: bool,
) -> Result<()> {
    let _configuration = state.pro.configuration.lock().await;
    if generation
        != state
            .pro
            .generation
            .load(std::sync::atomic::Ordering::Acquire)
    {
        return Ok(());
    }
    let retire = || {
        let mut preferences = lock(&state.pro.preferences);
        if let Some(copy) = preferences
            .get_mut(workspace)
            .and_then(|p| p.copy.as_mut())
            .filter(|copy| copy.takeover_request.as_deref() == Some(request))
        {
            copy.takeover_requested = false;
            copy.takeover_request = None;
            true
        } else {
            false
        }
    };
    let changed = if only_idle {
        super::moves::if_idle(state, workspace, retire).unwrap_or(false)
    } else {
        retire()
    };
    if changed {
        super::persist(state).await?;
    }
    Ok(())
}

/// Historical/moved rows are views. Every still-live registered process matters;
/// the scan intentionally does not stop at the archive serialization ceiling.
pub(super) fn live_processes(state: &AppState, workspace: &str) -> bool {
    let ids: Vec<_> = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, project)| project.as_str() == workspace)
        .map(|(id, _)| id.clone())
        .collect();
    ids.into_iter().any(|id| {
        state.chat.get(&id).is_some_and(|chat| chat.alive)
            || state.sessions.get(&id).is_some_and(|session| session.alive)
    })
}
