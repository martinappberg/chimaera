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
