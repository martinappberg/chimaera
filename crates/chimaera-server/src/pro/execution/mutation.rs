//! Short admission locks, never filesystem locks. An admitted commit must finish
//! before this process can install another epoch or publish a clean handoff.
use super::{AppState, Ownership};
use crate::lock;
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc, Mutex},
};

#[derive(Default)]
pub(super) struct Commits(pub(super) Arc<Mutex<Admission>>);

#[derive(Default)]
pub(super) struct Admission {
    pub(super) counts: HashMap<String, usize>,
    pub(super) maintenance: HashMap<String, super::maintenance::Entry>,
}

pub(crate) struct Guard {
    commits: Arc<Mutex<Admission>>,
    workspace: String,
}

/// Copy installation counts toward stop/replacement drainage but never admits
/// execution or a publication. Its immutable pending checkpoint is its scope.
pub(in crate::pro) struct CopyGuard {
    _commit: Guard,
    _configuration: tokio::sync::OwnedMutexGuard<()>,
    workspace: String,
    generation: u64,
    checkpoint: super::wire::Checkpoint,
    root: std::path::PathBuf,
}
impl CopyGuard {
    pub(in crate::pro) fn check(&self, state: &AppState) -> anyhow::Result<()> {
        let matching = lock(&state.pro.preferences)
            .get(&self.workspace)
            .filter(|preference| {
                !preference.never_mirror
                    && !preference.privacy_pending
                    && !preference.execution_launch_pending
                    && !preference.execution_groups_overflow
            })
            .and_then(|preference| preference.copy.as_ref())
            .is_some_and(|copy| {
                !copy.takeover_requested && copy.pending.as_ref() == Some(&self.checkpoint)
            });
        if generation(state) != self.generation
            || !matching
            || crate::pro::project_copy::live_processes(state, &self.workspace)
            || super::unclean(state, &self.workspace)
            || super::setup::active(state, &self.workspace)
            || !crate::pro::project_copy::copy_only(state, &self.workspace)
            || !crate::pro::projects::account_matches(state, &self.workspace)
        {
            return Err(Changed.into());
        }
        Ok(())
    }
    /// Blocking filesystem callers revalidate the selected inode too.
    pub(in crate::pro) fn check_files(&self, state: &AppState) -> anyhow::Result<()> {
        self.check(state)?;
        crate::pro::projects::check_copy_destination(state, &self.workspace, &self.root)?;
        self.check(state)
    }
}

pub(in crate::pro) async fn begin_copy(
    state: &Arc<AppState>,
    workspace: &str,
    generation: u64,
    checkpoint: super::wire::Checkpoint,
    root: std::path::PathBuf,
) -> anyhow::Result<CopyGuard> {
    let configuration = state.pro.configuration.clone().lock_owned().await;
    super::receipt::validate(&checkpoint)?;
    let commit = {
        let mut commits = lock(&state.pro.execution.commits.0);
        if commits.maintenance.contains_key(workspace)
            || commits.counts.get(workspace).copied().unwrap_or(0) > 0
            || commits.counts.values().sum::<usize>() >= 64
        {
            return Err(Changed.into());
        }
        *commits.counts.entry(workspace.to_owned()).or_default() += 1;
        Guard {
            commits: state.pro.execution.commits.0.clone(),
            workspace: workspace.to_owned(),
        }
    };
    let guard = CopyGuard {
        _commit: commit,
        _configuration: configuration,
        workspace: workspace.to_owned(),
        generation,
        checkpoint,
        root,
    };
    guard.check(state)?;
    Ok(guard)
}

/// Return installation belongs to its admitted account/epoch through filesystem
/// waits and the durable setup transition. Configuration replacement is
/// serialized; stop/replacement also sees the counted commit until its owned
/// task finishes.
pub(crate) struct ImportGuard {
    _commit: Guard,
    _configuration: tokio::sync::OwnedMutexGuard<()>,
    workspace: String,
    epoch: u64,
    generation: u64,
}
impl ImportGuard {
    /// Execution preparation must not reacquire our configuration lock. Keep
    /// the counted reservation until its final captured launch admission settles.
    pub(crate) fn into_resume(self) -> Guard {
        let Self {
            _commit,
            _configuration,
            ..
        } = self;
        drop(_configuration);
        _commit
    }
    /// Publish setup only for the hydration whose durable commit we admitted.
    /// The caller retains this guard through persistence, so account replacement
    /// cannot observe a released reservation between commit and this transition.
    pub(crate) fn setting_up(&self, state: &AppState) -> anyhow::Result<()> {
        if self.generation != generation(state)
            || !crate::pro::may_import(state, &self.workspace, self.epoch)
        {
            return Err(Changed.into());
        }
        let managed = super::managed(state, &self.workspace);
        let proofs = lock(&state.pro.execution.proofs);
        let mut ownership = lock(&state.pro.ownership);
        if self.generation != generation(state)
            || !matches!(ownership.get(&self.workspace), Some(Ownership::Hydrating { epoch }) if *epoch == self.epoch)
            || (managed
                && !proofs.get(&self.workspace).is_some_and(|proof| {
                    !proof.stopped
                        && proof.generation == self.generation
                        && proof.epoch == self.epoch
                        && proof.deadline.valid()
                }))
        {
            return Err(Changed.into());
        }
        ownership.insert(
            self.workspace.clone(),
            Ownership::SettingUp { epoch: self.epoch },
        );
        Ok(())
    }

    /// The copy-role retirement follows a durable setup transition. Validate
    /// that exact transition without admitting new filesystem or execution work.
    pub(in crate::pro) fn check_setting_up(&self, state: &AppState) -> anyhow::Result<()> {
        let managed = super::managed(state, &self.workspace);
        let proofs = lock(&state.pro.execution.proofs);
        let ownership = lock(&state.pro.ownership);
        if self.generation != generation(state)
            || !matches!(ownership.get(&self.workspace),Some(Ownership::SettingUp {epoch}) if *epoch==self.epoch)
            || (managed
                && !proofs.get(&self.workspace).is_some_and(|proof| {
                    !proof.stopped
                        && proof.generation == self.generation
                        && proof.epoch == self.epoch
                        && proof.deadline.valid()
                }))
        {
            return Err(Changed.into());
        }
        Ok(())
    }

    pub(crate) fn check(&self, state: &AppState) -> anyhow::Result<()> {
        if self.generation != generation(state)
            || !crate::pro::may_import(state, &self.workspace, self.epoch)
            || !super::valid_grant(state, &self.workspace, self.epoch)
        {
            return Err(Changed.into());
        }
        Ok(())
    }
}

pub(crate) async fn begin_import(
    state: &Arc<AppState>,
    workspace: &str,
    epoch: u64,
    generation: u64,
) -> anyhow::Result<ImportGuard> {
    let configuration = state.pro.configuration.clone().lock_owned().await;
    if generation != self::generation(state) || !crate::pro::may_import(state, workspace, epoch) {
        return Err(Changed.into());
    }
    let managed = super::managed(state, workspace);
    // Match the ordinary admission's proof -> ownership -> commits order.
    // Holding these locks makes reservation atomic against fence/epoch change.
    let proofs = lock(&state.pro.execution.proofs);
    let ownership = lock(&state.pro.ownership);
    if managed
        && !(proofs.get(workspace).is_some_and(|proof| {
            !proof.stopped
                && proof.generation == generation
                && proof.epoch == epoch
                && proof.deadline.valid()
        }) && matches!(ownership.get(workspace), Some(Ownership::Local {epoch:current} | Ownership::Hydrating {epoch:current}) if *current == epoch))
    {
        return Err(Changed.into());
    }
    if generation != self::generation(state) {
        return Err(Changed.into());
    }
    let mut commits = lock(&state.pro.execution.commits.0);
    if commits.maintenance.contains_key(workspace) || commits.counts.values().sum::<usize>() >= 64 {
        return Err(Changed.into());
    }
    *commits.counts.entry(workspace.to_owned()).or_default() += 1;
    Ok(ImportGuard {
        _commit: Guard {
            commits: state.pro.execution.commits.0.clone(),
            workspace: workspace.to_owned(),
        },
        _configuration: configuration,
        workspace: workspace.to_owned(),
        epoch,
        generation,
    })
}
tokio::task_local! {
    static REQUEST_RESERVED: ();
    static IMPORT_RESUME: Dispatch;
}
pub(crate) fn request_reserved() -> bool {
    REQUEST_RESERVED.try_with(|_| ()).is_ok()
}
pub(crate) async fn reserved_request<F: std::future::Future>(
    guard: Guard,
    operation: F,
) -> F::Output {
    REQUEST_RESERVED
        .scope((), async move {
            let _guard = guard;
            operation.await
        })
        .await
}
pub(crate) fn check_import_resume(state: &AppState, workspace: &str) -> anyhow::Result<()> {
    IMPORT_RESUME
        .try_with(|captured| {
            if captured.workspace != workspace {
                return Err(Changed.into());
            }
            captured.check(state)
        })
        .unwrap_or(Ok(()))
}
pub(crate) async fn resume_import<F: std::future::Future>(
    guard: Guard,
    captured: Dispatch,
    operation: F,
) -> F::Output {
    IMPORT_RESUME
        .scope(captured, reserved_request(guard, operation))
        .await
}
impl Drop for Guard {
    fn drop(&mut self) {
        let mut commits = lock(&self.commits);
        if let Some(active) = commits.counts.get_mut(&self.workspace) {
            *active -= 1;
            if *active == 0 {
                commits.counts.remove(&self.workspace);
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn local_dispatch_owner_fixture(state: &AppState, workspace: &str, epoch: u64) {
    lock(&state.pro.ownership).insert(workspace.to_owned(), Ownership::Local { epoch });
}

/// An asynchronous daemon send retains its original account and exact ownership.
/// Local devices still use ownership alone; workers additionally need a live proof
/// when begin() reserves the final dispatch. No presentation phase is authority.
#[derive(Clone)]
pub(crate) struct Dispatch {
    workspace: String,
    generation: u64,
    ownership: Option<Ownership>,
}
impl Dispatch {
    pub(crate) fn capture(state: &AppState, workspace: &str) -> anyhow::Result<Self> {
        let captured = Self {
            workspace: workspace.to_owned(),
            generation: generation(state),
            ownership: lock(&state.pro.ownership).get(workspace).cloned(),
        };
        captured.check(state)?;
        Ok(captured)
    }
    pub(crate) fn check(&self, state: &AppState) -> anyhow::Result<()> {
        if self.generation != generation(state)
            || lock(&state.pro.ownership).get(&self.workspace) != self.ownership.as_ref()
            || !crate::pro::may_execute(state, &self.workspace)
            || self.generation != generation(state)
        {
            return Err(Changed.into());
        }
        Ok(())
    }
    pub(crate) fn begin(&self, state: &AppState) -> anyhow::Result<Option<Guard>> {
        self.check(state)?;
        let guard = begin_launch(state, &self.workspace)?;
        // Reservation orders subsequent stop/replacement against this send. A
        // transition between check and reservation must not refresh the intent.
        self.check(state)?;
        Ok(guard)
    }
}

/// Count the final synchronous spawn/registration window so a clean transfer
/// cannot mistake a not-yet-registered child for an empty workload. Ordinary
/// unconfigured local sessions remain inert; device work needs ownership alone.
pub(crate) fn begin_launch(state: &AppState, workspace: &str) -> anyhow::Result<Option<Guard>> {
    if !crate::pro::may_execute(state, workspace) {
        return Err(Changed.into());
    }
    let generation = generation(state);
    let managed = super::managed(state, workspace);
    let tracked = managed
        || (lock(&state.pro.runtime).is_some()
            && (lock(&state.pro.preferences)
                .get(workspace)
                .is_some_and(|p| p.account.is_some())
                || lock(&state.pro.adoptions).contains_key(workspace)
                || lock(&state.pro.opened_here).contains(workspace)));
    let worker = super::worker(state);
    let installing = lock(&state.pro.installing).contains(workspace);
    let proofs = lock(&state.pro.execution.proofs);
    let ownership = lock(&state.pro.ownership);
    if generation != self::generation(state) {
        return Err(Changed.into());
    }
    if crate::pro::project_copy::copy_only(state, workspace) {
        return Err(Changed.into());
    }
    match ownership.get(workspace) {
        None if !tracked => return Ok(None),
        None if !worker => {}
        Some(Ownership::Local { .. } | Ownership::SettingUp { .. }) => {}
        Some(Ownership::AwaitingVerification { .. }) if !worker && !installing => {}
        _ => return Err(Changed.into()),
    }
    if managed && worker {
        let allowed = proofs.get(workspace).is_some_and(|proof| {
            !proof.stopped
                && proof.generation == generation
                && proof.deadline.valid()
                && matches!(ownership.get(workspace), Some(Ownership::Local { epoch } | Ownership::SettingUp { epoch }) if *epoch == proof.epoch)
        });
        if !allowed {
            return Err(Changed.into());
        }
    }
    let mut commits = lock(&state.pro.execution.commits.0);
    if commits.maintenance.contains_key(workspace) || commits.counts.values().sum::<usize>() >= 64 {
        return Err(Changed.into());
    }
    *commits.counts.entry(workspace.to_owned()).or_default() += 1;
    Ok(Some(Guard {
        commits: state.pro.execution.commits.0.clone(),
        workspace: workspace.to_owned(),
    }))
}
#[derive(Debug)]
pub(crate) struct Changed;
impl std::fmt::Display for Changed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("workspace execution authority changed")
    }
}
impl std::error::Error for Changed {}

pub(crate) fn generation(state: &AppState) -> u64 {
    state.pro.generation.load(Ordering::Acquire)
}
pub(crate) fn capture(state: &AppState, workspace: &str) -> anyhow::Result<Option<(u64, u64)>> {
    if crate::pro::project_copy::copy_only(state, workspace) {
        return Err(Changed.into());
    }
    // A device's own local commands are admitted by ownership alone (laptop
    // first); only a worker ties them to its live lease epoch.
    if !super::managed(state, workspace) || !super::worker(state) {
        return Ok(None);
    }
    let generation = generation(state);
    match lock(&state.pro.ownership).get(workspace) {
        Some(Ownership::Local { epoch }) => Ok(Some((*epoch, generation))),
        _ => Err(Changed.into()),
    }
}
pub(super) fn idle(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.execution.commits.0)
        .counts
        .get(workspace)
        .copied()
        .unwrap_or(0)
        == 0
}
pub(crate) fn begin(
    state: &AppState,
    workspace: &str,
    epoch: u64,
    generation: u64,
) -> anyhow::Result<Guard> {
    if crate::pro::project_copy::copy_only(state, workspace) {
        return Err(Changed.into());
    }
    // Proof -> ownership is the same order as lease validation. Holding both
    // through the reservation orders admission against stop and epoch changes.
    let proofs = lock(&state.pro.execution.proofs);
    let ownership = lock(&state.pro.ownership);
    let allowed = proofs.get(workspace).is_some_and(|proof| {
        !proof.stopped
            && proof.generation == generation
            && proof.epoch == epoch
            && generation == self::generation(state)
            && proof.deadline.valid()
    }) && matches!(ownership.get(workspace), Some(Ownership::Local { epoch: current }) if *current == epoch);
    if !allowed {
        return Err(Changed.into());
    }
    let mut commits = lock(&state.pro.execution.commits.0);
    // At most 64 irreversible operations can be outstanding, even if a shared
    // filesystem stalls. No mutex or reactor thread waits for their I/O.
    if commits.maintenance.contains_key(workspace) || commits.counts.values().sum::<usize>() >= 64 {
        return Err(Changed.into());
    }
    *commits.counts.entry(workspace.to_owned()).or_default() += 1;
    Ok(Guard {
        commits: state.pro.execution.commits.0.clone(),
        workspace: workspace.to_owned(),
    })
}

#[cfg(test)]
mod tests;
