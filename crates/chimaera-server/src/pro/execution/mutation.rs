//! Short admission locks, never filesystem locks. An admitted commit must finish
//! before this process can install another epoch or publish a clean handoff.
use super::{AppState, Ownership};
use crate::lock;
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc, Mutex},
};

#[derive(Default)]
pub(super) struct Commits(Arc<Mutex<HashMap<String, usize>>>);

pub(crate) struct Guard {
    commits: Arc<Mutex<HashMap<String, usize>>>,
    workspace: String,
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
    if commits.values().sum::<usize>() >= 64 {
        return Err(Changed.into());
    }
    *commits.entry(workspace.to_owned()).or_default() += 1;
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
impl Drop for Guard {
    fn drop(&mut self) {
        let mut commits = lock(&self.commits);
        if let Some(active) = commits.get_mut(&self.workspace) {
            *active -= 1;
            if *active == 0 {
                commits.remove(&self.workspace);
            }
        }
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
    let worker = super::worker(state);
    let installing = lock(&state.pro.installing).contains(workspace);
    let proofs = lock(&state.pro.execution.proofs);
    let ownership = lock(&state.pro.ownership);
    if generation != self::generation(state) {
        return Err(Changed.into());
    }
    match ownership.get(workspace) {
        None if !managed => return Ok(None),
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
    if commits.values().sum::<usize>() >= 64 {
        return Err(Changed.into());
    }
    *commits.entry(workspace.to_owned()).or_default() += 1;
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
    if commits.values().sum::<usize>() >= 64 {
        return Err(Changed.into());
    }
    *commits.entry(workspace.to_owned()).or_default() += 1;
    Ok(Guard {
        commits: state.pro.execution.commits.0.clone(),
        workspace: workspace.to_owned(),
    })
}

#[cfg(test)]
mod tests;
