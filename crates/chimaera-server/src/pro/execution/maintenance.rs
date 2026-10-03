//! Disabled inherited-maintenance admission foundation. No process, journal,
//! channel or Prepared effects exist here; launch binding is not idle proof.
use super::{mutation, AppState, Ownership};
use crate::lock;
use chimaera_core::project_secret_idle::{AttemptIdentity, Binding, Prepare, Request};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Unstarted,
    Parking,
    RecoveryRequired,
}
pub(super) struct Entry {
    identity: AttemptIdentity,
    owner: String,
    phase: Phase,
    deadline: Instant,
}
/// Sealed observation of this configured supervisor launch, not a caller's
/// asserted tuple. Account/execution changes invalidate it before admission.
#[derive(Clone)]
pub(super) struct Launch {
    binding: Binding,
    generation: u64,
    epoch: u64,
}
impl Launch {
    pub(super) fn capture(state: &AppState, binding: &Binding) -> anyhow::Result<Self> {
        binding.validate()?;
        if !super::supervisor::matches_maintenance(state, binding) {
            return Err(mutation::Changed.into());
        }
        let generation = mutation::generation(state);
        let proofs = lock(&state.pro.execution.proofs);
        let ownership = lock(&state.pro.ownership);
        let proof = proofs.get(&binding.workspace_id).ok_or(mutation::Changed)?;
        if !proof.stopped
            && proof.generation == generation
            && proof.deadline.valid()
            && matches!(ownership.get(&binding.workspace_id), Some(Ownership::Local { epoch }) if *epoch == proof.epoch)
            && generation == mutation::generation(state)
        {
            Ok(Self {
                binding: binding.clone(),
                generation,
                epoch: proof.epoch,
            })
        } else {
            Err(mutation::Changed.into())
        }
    }
}

/// The actual maintenance actor retains this owner through waits and rollback.
/// An observer dropping its future cannot open the latch. Unknown owner loss
/// leaves a counted recovery fence rather than treating work as cleaned up.
pub(super) struct Owner {
    state: Arc<AppState>,
    identity: AttemptIdentity,
    owner: String,
    launch: Launch,
}
impl Owner {
    pub(super) fn begin(
        state: &Arc<AppState>,
        launch: &Launch,
        prepare: &Prepare,
        original_deadline: Instant,
    ) -> anyhow::Result<Self> {
        Request::Prepare(prepare.clone()).validate()?;
        if prepare.binding != launch.binding
            || original_deadline <= Instant::now()
            || mutation::generation(state) != launch.generation
            || !super::supervisor::matches_maintenance(state, &launch.binding)
        {
            return Err(mutation::Changed.into());
        }
        let proofs = lock(&state.pro.execution.proofs);
        let ownership = lock(&state.pro.ownership);
        if !proofs
            .get(&launch.binding.workspace_id)
            .is_some_and(|proof| {
                !proof.stopped
                    && proof.generation == launch.generation
                    && proof.epoch == launch.epoch
                    && proof.deadline.valid()
            })
            || !matches!(ownership.get(&launch.binding.workspace_id), Some(Ownership::Local { epoch }) if *epoch == launch.epoch)
        {
            return Err(mutation::Changed.into());
        }
        let mut admission = lock(&state.pro.execution.commits.0);
        let workspace = &launch.binding.workspace_id;
        if original_deadline <= Instant::now()
            || mutation::generation(state) != launch.generation
            || admission.maintenance.contains_key(workspace)
            || admission.counts.get(workspace).copied().unwrap_or(0) != 0
            || admission.counts.values().sum::<usize>() >= 64
        {
            return Err(mutation::Changed.into());
        }
        let identity = prepare.identity();
        let owner = chimaera_core::generate_token();
        admission.counts.insert(workspace.clone(), 1);
        let deadline =
            original_deadline.min(Instant::now() + Duration::from_millis(prepare.expires_in_ms));
        admission.maintenance.insert(
            workspace.clone(),
            Entry {
                identity: identity.clone(),
                owner: owner.clone(),
                phase: Phase::Unstarted,
                deadline,
            },
        );
        Ok(Self {
            state: state.clone(),
            identity,
            owner,
            launch: launch.clone(),
        })
    }
    /// Must precede any stop or metadata effect. A canceled/failed effect has
    /// no positive rollback proof, so this foundation cannot release it.
    pub(super) fn parking_started(&self) -> anyhow::Result<()> {
        if mutation::generation(&self.state) != self.launch.generation
            || !super::supervisor::matches_maintenance(&self.state, &self.launch.binding)
        {
            return Err(mutation::Changed.into());
        }
        let proofs = lock(&self.state.pro.execution.proofs);
        let ownership = lock(&self.state.pro.ownership);
        if !proofs
            .get(&self.launch.binding.workspace_id)
            .is_some_and(|proof| {
                !proof.stopped
                    && proof.generation == self.launch.generation
                    && proof.epoch == self.launch.epoch
                    && proof.deadline.valid()
            })
            || !matches!(ownership.get(&self.launch.binding.workspace_id), Some(Ownership::Local { epoch }) if *epoch == self.launch.epoch)
        {
            return Err(mutation::Changed.into());
        }
        let mut admission = lock(&self.state.pro.execution.commits.0);
        let entry = admission
            .maintenance
            .get_mut(&self.identity.binding.workspace_id)
            .filter(|entry| entry.owner == self.owner && entry.identity == self.identity)
            .ok_or(mutation::Changed)?;
        if entry.phase != Phase::Unstarted
            || entry.deadline <= Instant::now()
            || mutation::generation(&self.state) != self.launch.generation
        {
            return Err(mutation::Changed.into());
        }
        entry.phase = Phase::Parking;
        Ok(())
    }
    /// Positive rollback is currently available only before any parking
    /// effects. Later exact pidfd/metadata rollback needs its own sealed proof.
    pub(super) fn abort_unstarted(self) -> anyhow::Result<()> {
        let workspace = &self.identity.binding.workspace_id;
        let mut admission = lock(&self.state.pro.execution.commits.0);
        let matching = admission.maintenance.get(workspace).is_some_and(|entry| {
            entry.owner == self.owner
                && entry.identity == self.identity
                && entry.phase == Phase::Unstarted
        });
        if !matching || admission.counts.get(workspace) != Some(&1) {
            return Err(mutation::Changed.into());
        }
        admission.maintenance.remove(workspace);
        admission.counts.remove(workspace);
        drop(admission);
        Ok(())
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let mut admission = lock(&self.state.pro.execution.commits.0);
        if let Some(entry) = admission
            .maintenance
            .get_mut(&self.identity.binding.workspace_id)
            .filter(|entry| entry.owner == self.owner && entry.identity == self.identity)
        {
            entry.phase = Phase::RecoveryRequired;
        }
    }
}
pub(in crate::pro) fn closed(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.execution.commits.0)
        .maintenance
        .contains_key(workspace)
}

#[cfg(all(test, unix))]
#[path = "maintenance_tests.rs"]
pub(super) mod tests;
