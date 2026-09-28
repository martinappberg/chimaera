//! Managed execution authority. A passive observation can fence execution, but
//! only an authenticated acquire/renew response can create a fresh local lease.
mod lease;
mod restart;
pub(super) use restart::persist_latch;
pub(super) mod receipt;
pub(super) mod recovery;
#[cfg(test)]
mod tests;
mod watchdog;
pub(crate) mod wire;

use super::{
    protocol::{Baton, Configure, Role},
    Ownership,
};
use crate::{lock, AppState};
use anyhow::{ensure, Context, Result};
pub(super) use lease::RequestStart;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Mutex},
    time::Duration,
};
pub(super) use watchdog::{start, stop};

#[derive(Default)]
pub(super) struct State {
    proofs: Mutex<HashMap<String, Proof>>,
    pub(super) latched: Mutex<std::collections::HashSet<String>>,
    unclean: std::collections::HashSet<String>,
    pub(super) invalid: bool,
    boot: Option<String>,
}
struct Proof {
    lease: wire::ExecutionLease,
    epoch: u64,
    generation: u64,
    deadline: lease::Deadline,
    server_now: i128,
    stopped: bool,
}

pub(super) fn validate_configuration(config: &Configure) -> Result<()> {
    let Some(execution) = &config.execution else {
        return Ok(());
    };
    ensure!(
        cfg!(any(target_os = "linux", target_os = "macos"))
            && execution.version == 1
            && execution.capability.supported(),
        "execution capability not supported"
    );
    ensure!(
        config.account_id.as_deref().is_some_and(super::valid_id),
        "execution account identity required"
    );
    match config.role {
        Role::Device => ensure!(
            execution
                .installation_id
                .as_deref()
                .is_some_and(|id| id.starts_with("i-") && super::valid_id(id)),
            "installation binding required"
        ),
        Role::Worker => ensure!(
            execution.installation_id.is_none(),
            "worker cannot claim a preferred installation"
        ),
    }
    Ok(())
}

pub(super) fn managed(state: &AppState, workspace: &str) -> bool {
    let latched = lock(&state.pro.execution.latched).contains(workspace);
    state.pro.execution.invalid
        || latched
        || lock(&state.pro.preferences)
            .get(workspace)
            .is_some_and(|p| p.continuity.is_some())
}

pub(super) fn effective(
    state: &AppState,
    config: &Configure,
    workspace: &str,
) -> Result<Configure> {
    let managed = managed(state, workspace);
    ensure!(
        !managed || config.execution.is_some(),
        "continuity upgrade required"
    );
    let mut result = config.clone();
    if !managed {
        result.execution = None;
    }
    Ok(result)
}
pub(super) fn path(config: &Configure, workspace: &str, action: &str) -> String {
    let version = if config.execution.is_some() { 2 } else { 1 };
    if action.is_empty() {
        format!("/v{version}/baton/{workspace}")
    } else {
        format!("/v{version}/baton/{workspace}/{action}")
    }
}
pub(super) fn body(config: &Configure, epoch: u64, acquire: bool) -> Value {
    let mut value = json!({"holder_id":config.delegation.device_id});
    value[if acquire { "expected_epoch" } else { "epoch" }] = epoch.into();
    if let Some(execution) = &config.execution {
        value["execution_capability"] = json!(execution.capability);
    }
    value
}

/// Persisted policy prevents a later old/malformed response from downgrading an
/// enrolled workspace. It is metadata only: GET never installs a deadline.
pub(super) fn observe(state: &AppState, config: &Configure, baton: &Baton) -> Result<()> {
    ensure!(
        super::valid_id(&baton.workspace_id),
        "invalid continuity workspace"
    );
    let latched = lock(&state.pro.execution.latched).contains(&baton.workspace_id);
    let mut preferences = lock(&state.pro.preferences);
    let previous = preferences
        .get(&baton.workspace_id)
        .and_then(|p| p.continuity.as_ref());
    let Some(policy) = &baton.continuity else {
        ensure!(
            previous.is_none()
                && !latched
                && !state.pro.execution.invalid
                && baton.execution_capability.is_none()
                && baton.execution_lease.is_none(),
            "continuity downgrade denied"
        );
        return Ok(());
    };
    ensure!(
        config.execution.is_some()
            && policy.version == 2
            && policy.mode == "managed_v1"
            && policy.policy_revision > 0
            && policy
                .preferred_installation_id
                .as_deref()
                .is_none_or(|id| id.starts_with("i-") && super::valid_id(id))
            && baton
                .execution_capability
                .as_ref()
                .is_some_and(wire::ExecutionCapability::supported),
        "invalid execution authority"
    );
    if let Some(previous) = previous {
        ensure!(
            policy.policy_revision >= previous.policy_revision,
            "stale continuity policy"
        );
        ensure!(
            policy.policy_revision != previous.policy_revision || policy == previous,
            "continuity policy changed without revision"
        );
    }
    ensure!(
        preferences.len() < 128 || preferences.contains_key(&baton.workspace_id),
        "continuity workspace limit"
    );
    preferences
        .entry(baton.workspace_id.clone())
        .or_default()
        .continuity = Some(policy.clone());
    drop(preferences);
    lock(&state.pro.execution.latched).insert(baton.workspace_id.clone());
    Ok(())
}

fn timestamp(value: &str) -> Result<i128> {
    ensure!(
        value.len() <= 64 && (value.ends_with('Z') || value.ends_with("+00:00")),
        "invalid execution timestamp"
    );
    Ok(
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .context("invalid execution timestamp")?
            .unix_timestamp_nanos(),
    )
}

/// `start` is captured BEFORE sending the mutating request. Only the exact
/// current generation and strictly newer renewal can extend authority.
pub(super) fn accept(
    state: &AppState,
    config: &Configure,
    baton: &Baton,
    generation: u64,
    start: RequestStart,
) -> Result<()> {
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "stale execution generation"
    );
    ensure!(
        !state.pro.execution.invalid && !state.pro.execution.unclean.contains(&baton.workspace_id),
        "previous managed processes require supervisor cleanup"
    );
    observe(state, config, baton)?;
    ensure!(
        !lock(&state.pro.preferences)
            .get(&baton.workspace_id)
            .is_some_and(|p| p.recovery_pending),
        "project recovery is pending"
    );
    if config.execution.is_none() {
        return Ok(());
    }
    ensure!(
        baton.continuity.is_some()
            && baton.holder_id.as_deref() == Some(&config.delegation.device_id)
            && !baton.mirror_disabled
            && generation == state.pro.generation.load(Ordering::Acquire),
        "execution ownership changed"
    );
    let lease = baton
        .execution_lease
        .as_ref()
        .context("execution lease required")?;
    ensure!(
        super::valid_id(&lease.id) && lease.sequence > 0 && baton.epoch > 0,
        "invalid execution lease"
    );
    let server_now = timestamp(&baton.server_now)?;
    let expires = timestamp(
        baton
            .expires_at
            .as_deref()
            .context("execution expiry required")?,
    )?;
    let remaining = u64::try_from(expires - server_now)
        .ok()
        .map(Duration::from_nanos)
        .context("expired execution lease")?;
    let deadline = lease::Deadline::from_response(start, remaining)
        .context("execution lease arrived too late")?;
    let mut proofs = lock(&state.pro.execution.proofs);
    ensure!(
        proofs.len() < 128 || proofs.contains_key(&baton.workspace_id),
        "execution workspace limit"
    );
    if let Some(previous) = proofs.get(&baton.workspace_id) {
        ensure!(
            previous.generation == generation
                && baton.epoch >= previous.epoch
                && server_now >= previous.server_now,
            "stale execution grant"
        );
        if baton.epoch == previous.epoch {
            ensure!(
                lease.id == previous.lease.id && lease.sequence > previous.lease.sequence,
                "execution lease replay"
            );
        } else {
            ensure!(
                lease.id != previous.lease.id,
                "execution lease identity reused"
            );
        }
        // Stopping is not complete merely because a kill signal was queued.
        ensure!(
            !previous.stopped || quiescent(state, &baton.workspace_id),
            "previous execution is stopping"
        );
    }
    lock(&state.pro.preferences)
        .entry(baton.workspace_id.clone())
        .or_default()
        .execution_identity = Some(wire::Identity {
        endpoint: config.endpoint.clone(),
        account_id: config
            .account_id
            .clone()
            .context("execution account identity required")?,
        installation_id: config
            .execution
            .as_ref()
            .and_then(|e| e.installation_id.clone()),
        holder_id: config.delegation.device_id.clone(),
        epoch: baton.epoch,
    });
    proofs.insert(
        baton.workspace_id.clone(),
        Proof {
            lease: lease.clone(),
            epoch: baton.epoch,
            generation,
            deadline,
            server_now,
            stopped: false,
        },
    );
    Ok(())
}
pub(super) fn allows(state: &AppState, workspace: &str) -> bool {
    if !managed(state, workspace) {
        return true;
    }
    if lock(&state.pro.preferences)
        .get(workspace)
        .is_some_and(|p| p.recovery_pending)
    {
        return false;
    }
    let generation = state.pro.generation.load(Ordering::Acquire);
    lock(&state.pro.execution.proofs).get(workspace).is_some_and(|proof| !proof.stopped && proof.generation==generation && proof.deadline.valid()
        && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::Local{epoch}|Ownership::SettingUp{epoch}|Ownership::Hydrating{epoch}|Ownership::Transferring{epoch}) if *epoch==proof.epoch))
}
pub(super) fn epoch(state: &AppState, workspace: &str) -> Option<u64> {
    if !allows(state, workspace) {
        return None;
    }
    super::owned_epoch(state, workspace)
}
pub(super) fn preferred_here(state: &AppState, config: &Configure, workspace: &str) -> bool {
    let policy = lock(&state.pro.preferences)
        .get(workspace)
        .and_then(|p| p.continuity.clone());
    match policy {
        None => true,
        Some(policy) => config
            .execution
            .as_ref()
            .and_then(|e| e.installation_id.as_ref())
            .is_some_and(|id| Some(id) == policy.preferred_installation_id.as_ref()),
    }
}
pub(super) fn resume_allowed(state: &AppState, workspace: &str) -> bool {
    !lock(&state.pro.preferences)
        .get(workspace)
        .is_some_and(|p| p.execution_uncertain)
}

pub(super) fn quiescent(state: &AppState, workspace: &str) -> bool {
    if state.pro.execution.invalid || state.pro.execution.unclean.contains(workspace) {
        return false;
    }
    let ids: Vec<_> = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, w)| w.as_str() == workspace)
        .map(|(id, _)| id.clone())
        .collect();
    ids.into_iter().all(|id| {
        !state.chat.get(&id).is_some_and(|s| s.alive)
            && !state.sessions.get(&id).is_some_and(|s| s.alive)
    })
}
/// Close ingress before signalling. Return each stopped workspace repeatedly
/// until all registered children are gone, including a concurrent in-flight spawn.
pub(super) fn expire(state: &AppState, generation: u64) -> Vec<String> {
    let mut proofs = lock(&state.pro.execution.proofs);
    let mut expired = Vec::new();
    for (workspace, proof) in proofs.iter_mut() {
        if proof.generation == generation && (proof.stopped || !proof.deadline.valid()) {
            proof.stopped = true;
            lock(&state.pro.preferences)
                .entry(workspace.clone())
                .or_default()
                .execution_uncertain = true;
            expired.push(workspace.clone());
        }
    }
    expired
}
pub(super) fn invalidate(state: &AppState) -> Vec<String> {
    let mut proofs = lock(&state.pro.execution.proofs);
    let workspaces = proofs.keys().cloned().collect::<Vec<_>>();
    for proof in proofs.values_mut() {
        proof.stopped = true;
    }
    workspaces
}
pub(super) fn clear_stopped(state: &AppState) {
    lock(&state.pro.execution.proofs).clear();
}

pub(super) fn valid_grant(state: &AppState, workspace: &str, epoch: u64) -> bool {
    if !managed(state, workspace) {
        return true;
    }
    let generation = state.pro.generation.load(Ordering::Acquire);
    lock(&state.pro.execution.proofs)
        .get(workspace)
        .is_some_and(|proof| {
            !proof.stopped
                && proof.generation == generation
                && proof.epoch == epoch
                && proof.deadline.valid()
        })
}

/// Durable admission precedes spawning. On a crash, same-boot execution remains
/// closed until a trusted process supervisor proves the old workload stopped.
pub(crate) async fn prepare_launch(
    state: &std::sync::Arc<AppState>,
    workspace: &str,
) -> Result<()> {
    if !managed(state, workspace) {
        return Ok(());
    }
    let _configuration = state.pro.configuration.lock().await;
    ensure!(
        crate::pro::may_execute(state, workspace),
        "project execution authority unavailable"
    );
    {
        let mut preferences = lock(&state.pro.preferences);
        let preference = preferences
            .get_mut(workspace)
            .context("managed project identity unavailable")?;
        preference.execution_active = true;
        preference.execution_boot = state.pro.execution.boot.clone();
    }
    crate::pro::persist(state).await?;
    ensure!(
        crate::pro::may_execute(state, workspace),
        "execution authority changed during durable launch admission"
    );
    Ok(())
}
