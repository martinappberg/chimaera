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
    workspace_maintenance: std::collections::HashSet<String>,
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
        if commits.workspace_maintenance.contains(workspace)
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
    pub(crate) fn into_resume(self) -> ResumeGuard {
        let Self {
            _commit,
            _configuration,
            workspace,
            generation,
            ..
        } = self;
        drop(_configuration);
        ResumeGuard {
            commit: _commit,
            workspace,
            generation,
        }
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
    if commits.workspace_maintenance.contains(workspace)
        || commits.counts.values().sum::<usize>() >= 64
    {
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
/// Import recovery keeps its original account generation even for a local
/// project with no ordinary Pro enrollment. Only its admitted import can mint
/// this consume-once owner; dropping it releases the retained commit count.
pub(crate) struct ResumeGuard {
    commit: Guard,
    workspace: String,
    generation: u64,
}

struct ResumeAdmission {
    dispatch: Dispatch,
    imported: Option<(String, u64)>,
}
impl ResumeAdmission {
    fn check(&self, state: &AppState, workspace: &str) -> anyhow::Result<()> {
        let import_matches = || {
            self.imported.as_ref().is_none_or(|(original, captured)| {
                original == workspace && *captured == generation(state)
            })
        };
        if self.dispatch.workspace != workspace || !import_matches() {
            return Err(Changed.into());
        }
        self.dispatch.check(state)?;
        if !import_matches() {
            return Err(Changed.into());
        }
        Ok(())
    }
}
tokio::task_local! {
    static REQUEST_RESERVED: ();
    static IMPORT_RESUME: ResumeAdmission;
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
        .try_with(|captured| captured.check(state, workspace))
        .unwrap_or(Ok(()))
}
pub(crate) async fn resume_import<F: std::future::Future>(
    guard: ResumeGuard,
    captured: Dispatch,
    operation: F,
) -> F::Output {
    let ResumeGuard {
        commit,
        workspace,
        generation,
    } = guard;
    IMPORT_RESUME
        .scope(
            ResumeAdmission {
                dispatch: captured,
                imported: Some((workspace, generation)),
            },
            reserved_request(commit, operation),
        )
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

/// An asynchronous daemon send retains this project's original account participation
/// and exact ownership; unrelated account changes cannot fence free local work.
/// Local devices still use ownership alone; workers additionally need a live proof
/// when begin() reserves the final dispatch. No presentation phase is authority.
#[derive(Clone)]
pub(crate) struct Dispatch {
    workspace: String,
    generation: Option<u64>,
    ownership: Option<Ownership>,
}
impl Dispatch {
    /// Existing final spawn checks consume the originally admitted authority,
    /// including local manual restoration where no managed lease guard exists.
    /// A narrower dispatch cannot discard an enclosing import's original identity.
    pub(crate) async fn run<F: std::future::Future>(&self, operation: F) -> F::Output {
        let imported = IMPORT_RESUME
            .try_with(|admission| admission.imported.clone())
            .unwrap_or(None);
        IMPORT_RESUME
            .scope(
                ResumeAdmission {
                    dispatch: self.clone(),
                    imported,
                },
                operation,
            )
            .await
    }
    pub(crate) fn capture(state: &AppState, workspace: &str) -> anyhow::Result<Self> {
        let generation = generation(state);
        let ownership = lock(&state.pro.ownership).get(workspace).cloned();
        let captured = Self {
            workspace: workspace.to_owned(),
            generation: account_bound(state, workspace, &ownership).then_some(generation),
            ownership,
        };
        captured.check(state)?;
        Ok(captured)
    }
    pub(crate) fn check(&self, state: &AppState) -> anyhow::Result<()> {
        let ownership = lock(&state.pro.ownership).get(&self.workspace).cloned();
        if self
            .generation
            .is_some_and(|captured| captured != generation(state))
            || ownership != self.ownership
            || self.generation.is_some() != account_bound(state, &self.workspace, &ownership)
            || !crate::pro::may_execute(state, &self.workspace)
            || self
                .generation
                .is_some_and(|captured| captured != generation(state))
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

// Only this project's account participation pins the daemon-wide generation.
// A free installer must survive an unrelated Configure/sign-out, but cannot
// silently adopt this project's first enrollment while its child is running.
fn account_bound(state: &AppState, workspace: &str, ownership: &Option<Ownership>) -> bool {
    ownership.is_some()
        || super::managed(state, workspace)
        || lock(&state.pro.preferences)
            .get(workspace)
            .is_some_and(|preference| preference.account.is_some())
        || lock(&state.pro.adoptions).contains_key(workspace)
        || {
            let authority = lock(&state.pro.authority);
            authority.restricted() && authority.allows(workspace)
        }
}

fn tracked(state: &AppState, workspace: &str) -> bool {
    super::managed(state, workspace)
        || (lock(&state.pro.runtime).is_some()
            && (lock(&state.pro.preferences)
                .get(workspace)
                .is_some_and(|p| p.account.is_some())
                || lock(&state.pro.adoptions).contains_key(workspace)
                || lock(&state.pro.opened_here).contains(workspace)))
}

pub(crate) fn maintenance_worker(state: &AppState) -> bool {
    super::worker(state)
}

fn ordinary_worker(state: &AppState, workspace: &str, tracked: bool) -> bool {
    maintenance_worker(state)
        && state.daemon_extension.is_some()
        && !tracked
        && lock(&state.workspaces).get(workspace).is_some()
        && !account_bound(state, workspace, &None)
        && !lock(&state.pro.authority).restricted()
}

/// Count the final synchronous spawn/registration window so a clean transfer
/// cannot mistake a not-yet-registered child for an empty workload. Ordinary
/// unconfigured local sessions remain inert; device work needs ownership alone.
pub(crate) fn begin_launch(state: &AppState, workspace: &str) -> anyhow::Result<Option<Guard>> {
    begin_launch_admission(state, workspace, true)
}

/// Plain shells retain their existing execution allowance during a worker's
/// bounded renewal window, while sharing the same counted maintenance gate.
pub(crate) fn begin_shell_launch(
    state: &AppState,
    workspace: &str,
) -> anyhow::Result<Option<Guard>> {
    begin_launch_admission(state, workspace, false)
}

fn begin_launch_admission(
    state: &AppState,
    workspace: &str,
    require_agent_proof: bool,
) -> anyhow::Result<Option<Guard>> {
    // Plain shells are never managed: a computer whose agents wait for its
    // own lease keeps its terminals (`pro::may_run_shell`).
    let allowed = if require_agent_proof {
        crate::pro::may_execute(state, workspace)
    } else {
        crate::pro::may_run_shell(state, workspace)
    };
    if workspace_closed(state, workspace) || !allowed {
        return Err(Changed.into());
    }
    let generation = generation(state);
    let managed = super::managed(state, workspace);
    let tracked = tracked(state, workspace);
    let worker = super::worker(state);
    let ordinary_worker = ordinary_worker(state, workspace, tracked);
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
        None if ordinary_worker => {}
        None if !tracked => return Ok(None),
        None if !worker => {}
        Some(Ownership::Local { .. } | Ownership::SettingUp { .. }) => {}
        Some(Ownership::AwaitingVerification { .. }) if !worker && !installing => {}
        _ => return Err(Changed.into()),
    }
    if require_agent_proof && managed && worker {
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
    if commits.workspace_maintenance.contains(workspace)
        || commits.counts.values().sum::<usize>() >= 64
    {
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
    if commits.workspace_maintenance.contains(workspace)
        || commits.counts.values().sum::<usize>() >= 64
    {
        return Err(Changed.into());
    }
    *commits.counts.entry(workspace.to_owned()).or_default() += 1;
    Ok(Guard {
        commits: state.pro.execution.commits.0.clone(),
        workspace: workspace.to_owned(),
    })
}

/// One exclusive workspace operation shares the original mutation count store.
/// Configuration is retained by the caller; no account or execution proof is minted.
pub(crate) struct WorkspaceMutation {
    guard: Guard,
}
impl WorkspaceMutation {
    pub(crate) fn current(&self) -> bool {
        let commits = lock(&self.guard.commits);
        commits
            .workspace_maintenance
            .contains(&self.guard.workspace)
            && commits.counts.get(&self.guard.workspace) == Some(&1)
    }
}
impl Drop for WorkspaceMutation {
    fn drop(&mut self) {
        lock(&self.guard.commits)
            .workspace_maintenance
            .remove(&self.guard.workspace);
        // The original counted Guard releases only after the exclusive marker.
    }
}
pub(crate) fn workspace_closed(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.execution.commits.0)
        .workspace_maintenance
        .contains(workspace)
}
pub(crate) fn begin_workspace_maintenance(
    state: &AppState,
    workspace: &str,
) -> anyhow::Result<WorkspaceMutation> {
    // Ordinary shared workers have no managed lease. Only their selected
    // extension may reserve an unbound project; managed ownership stays strict.
    let ordinary_worker = ordinary_worker(state, workspace, tracked(state, workspace));
    let ownership = lock(&state.pro.ownership);
    match ownership.get(workspace) {
        None if ordinary_worker => {}
        Some(Ownership::Local { .. }) => {}
        _ => return Err(Changed.into()),
    }
    let mut commits = lock(&state.pro.execution.commits.0);
    if commits.workspace_maintenance.contains(workspace)
        || commits.counts.get(workspace).copied().unwrap_or(0) != 0
        || commits.counts.values().sum::<usize>() >= 64
    {
        return Err(Changed.into());
    }
    commits.workspace_maintenance.insert(workspace.to_owned());
    commits.counts.insert(workspace.to_owned(), 1);
    Ok(WorkspaceMutation {
        guard: Guard {
            commits: state.pro.execution.commits.0.clone(),
            workspace: workspace.to_owned(),
        },
    })
}

#[cfg(test)]
mod tests;
#[cfg(all(test, unix))]
mod workspace_tests;
