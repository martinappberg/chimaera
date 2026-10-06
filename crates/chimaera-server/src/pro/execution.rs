//! Managed execution authority. A passive observation can fence execution, but
//! only an authenticated acquire/renew response can create a fresh local lease.
pub(crate) mod installer;
mod launch;
mod lease;
// Read old disk before-images before automatic restoration; no active channel.
#[cfg(all(test, unix))]
mod legacy_parking_fixture;
#[cfg(any(target_os = "linux", all(test, unix)))]
mod maintenance_store;
pub(crate) mod mutation;
#[cfg(any(test, all(unix, feature = "provider-authority-prototype")))]
pub(super) mod provider_protection;
// Original protected admission remains host-owned; vendor consumers are private.
#[cfg(all(unix, feature = "provider-authority-prototype"))]
#[allow(dead_code)]
pub(super) mod provider_client;
#[cfg(all(
    unix,
    feature = "provider-authority-prototype",
    feature = "daemon-extension-fixture"
))]
pub mod provider_fixture_host;
#[cfg(all(unix, feature = "provider-authority-prototype"))]
pub(super) mod provider_ready;
#[cfg(all(unix, feature = "provider-authority-prototype"))]
pub(super) mod provider_startup;
mod restart;
pub(super) mod setup;
pub(super) mod supervisor;
pub(super) use restart::{persist_latch, record_groups, reprobe, shutdown};
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
pub(super) use watchdog::{check_now, start, stop};

#[derive(Default)]
pub(super) struct State {
    proofs: Mutex<HashMap<String, Proof>>,
    commits: mutation::Commits,
    setups: Mutex<HashMap<String, setup::Entry>>,
    launches: Mutex<HashMap<String, (usize, bool, u64)>>,
    pub(super) latched: Mutex<std::collections::HashSet<String>>,
    /// Previous-life process groups not yet proven gone, per workspace. An
    /// empty list means no evidence exists (worker only; see `restore`).
    unclean: Mutex<HashMap<String, Vec<(u32, u64)>>>,
    /// Projects whose enrollment record was lost (see `restore`). Each stays
    /// managed and publishes nothing until an authoritative read restores its
    /// policy; a worker also runs nothing there. Bounded to 128.
    pub(super) uncertain: Mutex<std::collections::HashSet<String>>,
    boot: Option<String>,
    /// Cleanup cannot repair unreadable enrollment or ownership state.
    supervisor_state_invalid: bool,
    supervisor_pending: Mutex<Option<supervisor::CleanupReceipt>>,
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    provider_pending: Mutex<Option<std::sync::Arc<provider_startup::Pending>>>,
    supervisor_ack: Mutex<Option<supervisor::CleanupAck>>,
    /// The watchdog's latest tick (see `thawed`).
    tick: Mutex<Option<std::time::Instant>>,
    /// Fires whenever a proof is installed, fenced or dropped, so a viewer
    /// waiting on a resumed machine's own renewal (`await_renewal`) answers
    /// the moment it lands instead of polling.
    changed: tokio::sync::Notify,
}
#[derive(Clone)]
struct Proof {
    lease: wire::ExecutionLease,
    epoch: u64,
    generation: u64,
    deadline: lease::Deadline,
    server_now: i128,
    stopped: bool,
    /// Set when this process resumed from a freeze with the deadline lapsed:
    /// one renewal at the recorded epoch is tried before the watchdog fences.
    renew_until: Option<std::time::Instant>,
}

/// How long a resumed cloud machine may take to renew its own epoch before
/// the watchdog fences it. The account keeps a suspended owner's lease for it
/// (nobody else can acquire it meanwhile), so this is not a partition window.
pub(super) const RESUME_RENEW: Duration = Duration::from_secs(20);

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

/// A cloud worker executes only under a fresh lease. A personal device is
/// never fenced by account reachability, sign-out, plan changes or a daemon
/// restart (laptop first): only a verified other owner or its own in-progress
/// transfer stops it, through the ownership fences in `may_write`.
pub(super) fn worker(state: &AppState) -> bool {
    crate::cloud::enabled()
        || state.pro.worker.load(Ordering::Acquire)
        || lock(&state.pro.runtime)
            .as_ref()
            .is_some_and(|config| config.role == Role::Worker)
}

/// This project's enrollment record was lost; see `State::uncertain`.
pub(super) fn uncertain(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.execution.uncertain).contains(workspace)
}
pub(super) fn any_uncertain(state: &AppState) -> bool {
    !lock(&state.pro.execution.uncertain).is_empty()
}

/// The account refused a legacy release of this project because it is
/// enrolled, which this daemon had not seen (a lost record, or an enrollment
/// that raced the read). Latch it so `managed`, and with it `effective`, route
/// the project through the newer path from the next pass. In memory only: the
/// state file and its latch are written once an authoritative read has
/// restored the policy (`observe`), never a latch naming a project without
/// one. Returns whether it was newly latched, so the one log line is not
/// repeated.
pub(super) fn require_v2(state: &AppState, workspace: &str) -> bool {
    let mut latched = lock(&state.pro.execution.latched);
    // The persisted latch holds at most 128 projects; more would make every
    // state write fail.
    if !super::valid_id(workspace) || latched.len() >= 128 || !latched.insert(workspace.to_owned())
    {
        return false;
    }
    tracing::info!(
        "The account requires the newer transfer path for a project; using it from the next pass"
    );
    true
}

/// "Keep on this computer only", acknowledged by the account (its privacy
/// fence stops any cloud takeover): the project is out of the lease loop. It
/// holds no lease proof, so its agents are never fenced for a lease nobody
/// renews, and its sessions restore without one (review R3 B4). While the
/// switch is still unacknowledged (`privacy_pending`) the cloud could still
/// take the project, so it keeps renewing like any other.
pub(super) fn kept_here(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.preferences)
        .get(workspace)
        .is_some_and(|p| p.never_mirror && !p.privacy_pending)
}

pub(super) fn managed(state: &AppState, workspace: &str) -> bool {
    let latched = lock(&state.pro.execution.latched).contains(workspace);
    uncertain(state, workspace)
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
    } else {
        let policy = lock(&state.pro.preferences)
            .get(workspace)
            .and_then(|p| p.continuity.clone())
            .context("continuity policy unavailable")?;
        let capability = match policy.mode.as_str() {
            "managed_v1" => wire::ExecutionCapability::managed(),
            "checkpoint_fork_v1" => wire::ExecutionCapability::checkpoint_fork(),
            _ => anyhow::bail!("unsupported continuity policy"),
        };
        result
            .execution
            .as_mut()
            .context("continuity upgrade required")?
            .capability = capability;
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
    let lost = uncertain(state, &baton.workspace_id);
    let mut preferences = lock(&state.pro.preferences);
    let previous = preferences
        .get(&baton.workspace_id)
        .and_then(|p| p.continuity.as_ref());
    let Some(policy) = &baton.continuity else {
        ensure!(
            previous.is_none()
                && !latched
                && !lost
                && baton.execution_capability.is_none()
                && baton.execution_lease.is_none(),
            "continuity downgrade denied"
        );
        return Ok(());
    };
    ensure!(
        config.execution.is_some()
            && policy.version == 2
            && baton
                .execution_capability
                .as_ref()
                .and_then(|cap| cap.policy_mode())
                == Some(policy.mode.as_str())
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
    // The account restored this project's policy: its lost record is resolved.
    lock(&state.pro.execution.uncertain).remove(&baton.workspace_id);
    Ok(())
}

/// Server-relative expiry: the account's own clock decides, never ours.
pub(super) fn expired(baton: &Baton) -> bool {
    match (
        baton.expires_at.as_deref().map(timestamp),
        timestamp(&baton.server_now),
    ) {
        (Some(Ok(expires)), Ok(now)) => expires <= now,
        (None, _) => true,
        _ => false,
    }
}
/// This installation held exactly this epoch and nobody acquired it since
/// (every acquisition advances the epoch). Its own processes and files are the
/// newest state, so re-acquiring needs no hydration and no fork.
pub(super) fn held_here(state: &AppState, config: &Configure, baton: &Baton) -> bool {
    lock(&state.pro.preferences)
        .get(&baton.workspace_id)
        .and_then(|p| p.execution_identity.as_ref())
        .is_some_and(|identity| {
            identity.epoch == baton.epoch
                && identity.holder_id == config.delegation.device_id
                && identity.endpoint == config.endpoint
                && Some(&identity.account_id) == config.account_id.as_ref()
                && baton
                    .holder_id
                    .as_deref()
                    .is_none_or(|holder| holder == config.delegation.device_id)
        })
}
/// A worker's watchdog fenced this epoch; a later renewal of the same epoch
/// must resume what the fence preserved.
/// A personal computer whose agents wait only for its own lease (lapsed, or
/// not verified yet): its plain shells are not managed and keep working.
pub(super) fn shells_allowed(state: &AppState, workspace: &str) -> bool {
    !worker(state)
        && !supervisor::pending(state)
        && !lock(&state.pro.preferences)
            .get(workspace)
            .is_some_and(|p| p.recovery_pending)
}
pub(super) fn fenced(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.execution.proofs)
        .get(workspace)
        .is_some_and(|proof| proof.stopped)
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
        !supervisor::pending(state)
            && (!supervisor::supervised(state) || config.execution.is_some()),
        "supervised execution requires startup cleanup and a negotiated lease"
    );
    // A worker cannot accept (or keep) a lease while it cannot prove old
    // processes stopped. A device keeps running regardless (laptop first).
    ensure!(
        (config.role == Role::Device && !worker(state))
            || (!uncertain(state, &baton.workspace_id) && !unclean(state, &baton.workspace_id)),
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
        lock(&state.pro.opened_here).remove(&baton.workspace_id);
        return Ok(());
    }
    ensure!(
        baton.continuity.is_some()
            && config.execution.as_ref().map(|e| &e.capability)
                == baton.execution_capability.as_ref()
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
    // A canceled HTTP future does not cancel a blocking filesystem commit.
    // Replacing its epoch must wait for that owned reservation to be dropped.
    ensure!(
        proofs.get(&baton.workspace_id).is_some_and(|previous| {
            !previous.stopped
                && previous.epoch == baton.epoch
                && previous.generation == generation
                && previous.lease.id == lease.id
        }) || mutation::idle(state, &baton.workspace_id),
        "previous project mutation is still committing"
    );
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
    // The epoch this installation held last, if this grant continues it with
    // nobody in between (the same epoch renewed, or its own re-acquired: every
    // acquisition advances the epoch by one): what a fence preserved stays
    // resumable. Any other grant settles those entries (review R4 S1).
    let continued = lock(&state.pro.preferences)
        .get(&baton.workspace_id)
        .and_then(|p| p.execution_identity.as_ref())
        .filter(|identity| {
            identity.holder_id == config.delegation.device_id
                && identity.endpoint == config.endpoint
                && Some(&identity.account_id) == config.account_id.as_ref()
                && (identity.epoch == baton.epoch || identity.epoch + 1 == baton.epoch)
        })
        .map(|identity| identity.epoch);
    match continued {
        Some(from) => super::carry_fenced(state, &baton.workspace_id, from, baton.epoch),
        None => super::settle_fenced_here(state, &baton.workspace_id, Some(baton.epoch)),
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
            renew_until: None,
        },
    );
    drop(proofs);
    // This device holds the project now: nothing is left to pull home.
    lock(&state.pro.opened_here).remove(&baton.workspace_id);
    state.pro.execution.changed.notify_waiters();
    Ok(())
}
/// Local execution admission. Ownership transitions (another verified owner,
/// an in-progress transfer) are fenced separately by `may_write`.
pub(super) fn allows(state: &AppState, workspace: &str) -> bool {
    if supervisor::pending(state) {
        return false;
    }
    if !managed(state, workspace) {
        return !supervisor::supervised(state);
    }
    if lock(&state.pro.preferences)
        .get(workspace)
        .is_some_and(|p| p.recovery_pending)
    {
        return false;
    }
    // A resumed machine keeps admitting the input that woke it while it renews
    // its own paused lease; a refused renewal fences it at once. A computer
    // runs its own work until its lease lapsed and it was fenced (`expire`).
    // A computer whose ownership is unverified (after a restart, a wake or a
    // failed flush) starts no agent until the lease loop verified it still
    // holds the project: the cloud may have taken it meanwhile (review R3
    // B3). Signed out and stood down at the account, nothing will verify it
    // and nobody else takes it.
    if !worker(state) {
        // Signed out, but the account never acknowledged that this project
        // stood down: the cloud may still take it, so its agents run only
        // until the kept lease's deadline (review R4 B2).
        if super::sign_out::unreleased(state, workspace) {
            return held_alive(state, workspace);
        }
        let unverified = !super::signed_out(state)
            && matches!(
                lock(&state.pro.ownership).get(workspace),
                Some(super::Ownership::AwaitingVerification { .. })
            );
        return if unverified {
            lease_valid(state, workspace)
        } else {
            !fenced(state, workspace) || resuming(state, workspace)
        };
    }
    lease_valid(state, workspace)
        || resuming(state, workspace)
        || (thawed(state) && resuming(state, workspace))
}
/// Notices a freeze before the watchdog does. The request that woke a
/// suspended machine (and the lease loop) can run in the moments after the
/// thaw before the watchdog's next tick; they must not see a lapsed lease
/// as a lost one. A watchdog silent for longer than a freeze is exactly what
/// its own next tick would report, so this grants the same one bounded
/// renewal (`resumed`) and wakes the lease loop.
pub(super) fn thawed(state: &AppState) -> bool {
    let stale =
        lock(&state.pro.execution.tick).is_some_and(|tick| tick.elapsed() > watchdog::FREEZE);
    if stale && resumed(state, state.pro.generation.load(Ordering::Acquire)) {
        state.pro.renew_now.notify_one();
    }
    stale
}
/// The watchdog ticked (see `thawed`).
pub(super) fn ticked(state: &AppState, at: std::time::Instant) {
    *lock(&state.pro.execution.tick) = Some(at);
}
/// This process resumed from a freeze (see `resumed`) and is renewing its own
/// epoch; the watchdog holds its fence until the renewal answers or the
/// bounded window passes.
pub(super) fn resuming(state: &AppState, workspace: &str) -> bool {
    let generation = state.pro.generation.load(Ordering::Acquire);
    lock(&state.pro.execution.proofs)
        .get(workspace)
        .is_some_and(|proof| {
            !proof.stopped
                && proof.generation == generation
                && proof
                    .renew_until
                    .is_some_and(|until| std::time::Instant::now() < until)
        })
}
/// The epoch of this project's current, unstopped execution proof.
pub(super) fn proof_epoch(state: &AppState, workspace: &str) -> Option<u64> {
    lock(&state.pro.execution.proofs)
        .get(workspace)
        .filter(|proof| !proof.stopped)
        .map(|proof| proof.epoch)
}
/// The watchdog saw the process frozen: a suspended cloud machine resumed, or
/// the clock jumped. Each proof whose deadline lapsed across the freeze gets
/// one bounded renewal at its recorded epoch before any fence (renew before
/// fencing). Returns whether any renewal is now due.
///
/// A personal computer gets that window only while nobody else can have taken
/// the project yet (its lease plus the account's takeover grace, by wall time,
/// since sleep stops the monotonic clock): renewing first then loses nothing.
/// Past that point the cloud may already run the work, so it is fenced at once
/// and resumes only after re-acquiring its own untouched epoch.
pub(super) fn resumed(state: &AppState, generation: u64) -> bool {
    let now = std::time::Instant::now();
    let device = !worker(state);
    let mut due = false;
    for proof in lock(&state.pro.execution.proofs).values_mut() {
        if proof.generation == generation
            && !proof.stopped
            && proof.renew_until.is_none()
            && !proof.deadline.valid()
        {
            let window = if device {
                proof
                    .deadline
                    .before_takeover()
                    .map_or(Duration::ZERO, |left| left.min(RESUME_RENEW))
            } else {
                RESUME_RENEW
            };
            if window.is_zero() {
                continue;
            }
            proof.renew_until = Some(now + window);
            due = true;
        }
    }
    due
}
/// Sessions a previous daemon left running resume only once this life has
/// verified ownership; `resume_unverified` applies the device fallback.
pub(super) fn restorable(state: &AppState, workspace: &str) -> bool {
    (lease_valid(state, workspace) || (!worker(state) && kept_here(state, workspace)))
        && !unclean(state, workspace)
}
/// A fresh, unexpired acquire/renew proof for the current local epoch. It
/// gates publication and forwarded viewers on every host, and all execution
/// on a worker.
pub(super) fn lease_valid(state: &AppState, workspace: &str) -> bool {
    if !managed(state, workspace) {
        return true;
    }
    let generation = state.pro.generation.load(Ordering::Acquire);
    lock(&state.pro.execution.proofs).get(workspace).is_some_and(|proof| !proof.stopped && proof.generation==generation && proof.deadline.valid()
        && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::Local{epoch}|Ownership::SettingUp{epoch}|Ownership::Hydrating{epoch}|Ownership::Transferring{epoch}) if *epoch==proof.epoch))
}
pub(super) fn epoch(state: &AppState, workspace: &str) -> Option<u64> {
    if !allows(state, workspace) || !lease_valid(state, workspace) {
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
/// The user opened this project here and it is not held here yet
/// (`super::note_opened`): it may come home to this computer even when the
/// account's preferred installation is another one.
pub(super) fn opened_here(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.opened_here).contains(workspace)
}
pub(super) fn checkpoint_mode(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.preferences)
        .get(workspace)
        .and_then(|p| p.continuity.as_ref())
        .is_some_and(|p| p.mode == "checkpoint_fork_v1")
}
pub(super) fn resume_allowed(state: &AppState, workspace: &str) -> bool {
    checkpoint_mode(state, workspace)
        || !lock(&state.pro.preferences)
            .get(workspace)
            .is_some_and(|p| p.execution_uncertain)
}
pub(crate) fn recovery_context(state: &AppState, workspace: &str) -> bool {
    checkpoint_mode(state, workspace)
        && lock(&state.pro.preferences)
            .get(workspace)
            .is_some_and(|p| p.execution_uncertain)
}
pub(super) fn fence_workspace(state: &AppState, workspace: &str) {
    if let Some(proof) = lock(&state.pro.execution.proofs).get_mut(workspace) {
        proof.stopped = true;
    }
    state.pro.execution.changed.notify_waiters();
}

/// How a wait for a resumed machine's own renewal ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Renewal {
    /// A fresh proof for the awaited epoch is installed.
    Renewed,
    /// The renewal was refused or the project fenced, replaced or dropped.
    Refused,
    /// The resume window (or the caller's cap) passed without an answer.
    TimedOut,
}

/// This resumed machine is renewing exactly `epoch` of `workspace`: the lease
/// lapsed while it was frozen and one renewal of the recorded epoch is out
/// (see `resumed`). Only then is a scope it cannot admit yet worth waiting for;
/// any other epoch, a device, or a fenced project answers at once.
pub(super) fn renewing(state: &AppState, workspace: &str, epoch: u64) -> bool {
    // A request can arrive before the watchdog's first tick after a thaw.
    thawed(state);
    resuming(state, workspace) && proof_epoch(state, workspace) == Some(epoch)
}

/// Waits (at most `cap`, and never past the resume window) for this resumed
/// machine's own renewal of `epoch` to answer. It admits nothing itself: the
/// caller re-validates its scope after `Renewed`, so a fresh proof must exist
/// first. No lock is held across an await, and dropping it has no effect.
pub(super) async fn await_renewal(
    state: &AppState,
    workspace: &str,
    epoch: u64,
    cap: Duration,
) -> Renewal {
    let generation = state.pro.generation.load(Ordering::Acquire);
    let started = tokio::time::Instant::now();
    loop {
        // Registered before the check, so a proof installed in between still
        // wakes this wait (notify_waiters keeps no permit for later waiters).
        let changed = state.pro.execution.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let until = {
            let proofs = lock(&state.pro.execution.proofs);
            let Some(proof) = proofs.get(workspace) else {
                return Renewal::Refused;
            };
            if proof.stopped || proof.generation != generation || proof.epoch != epoch {
                return Renewal::Refused;
            }
            if proof.deadline.valid() {
                return Renewal::Renewed;
            }
            match proof.renew_until {
                Some(until) => until,
                // Lapsed with no renewal out: the watchdog fences it.
                None => return Renewal::Refused,
            }
        };
        let deadline = tokio::time::Instant::from_std(until).min(started + cap);
        if tokio::time::Instant::now() >= deadline {
            return Renewal::TimedOut;
        }
        tokio::select! {
            () = &mut changed => {}
            () = tokio::time::sleep_until(deadline) => {}
        }
    }
}

/// Processes from a previous daemon life that were not proven gone.
pub(super) fn unclean(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.execution.unclean).contains_key(workspace)
}
/// Agents are the managed workload. Plain shells are never managed: they are
/// neither signalled by a fence nor awaited by a stop.
pub(super) fn managed_session(state: &AppState, id: &str) -> bool {
    state.chat.get(id).is_some() || lock(&state.agents).contains_key(id)
}
pub(super) fn quiescent(state: &AppState, workspace: &str) -> bool {
    if supervisor::pending(state)
        || (uncertain(state, workspace) && worker(state))
        || unclean(state, workspace)
        || !mutation::idle(state, workspace)
        || setup::active(state, workspace)
    {
        return false;
    }
    let ids: Vec<_> = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, w)| w.as_str() == workspace)
        .map(|(id, _)| id.clone())
        .collect();
    ids.into_iter()
        .filter(|id| managed_session(state, id))
        .all(|id| {
            !state.chat.get(&id).is_some_and(|s| s.alive)
                && !state.sessions.get(&id).is_some_and(|s| s.alive)
        })
}
/// Close ingress before signalling. Return each stopped workspace repeatedly
/// until all registered children are gone, including a concurrent in-flight spawn.
///
/// A personal computer is fenced like a cloud machine: its lease ending means
/// the cloud may continue the work, so its own agents stop first (the local
/// deadline keeps a margin before the account's expiry, and the account waits
/// a grace after it), and a turn never runs in two places. Only a successful
/// renewal moves the deadline: no answer, a refusal and the account's own
/// server errors all fence at it, since the account's takeover runs apart
/// from the requests that failed (review R4 B1). Shells keep running.
pub(super) fn expire(state: &AppState, generation: u64) -> Vec<String> {
    let worker = worker(state);
    let mut proofs = lock(&state.pro.execution.proofs);
    if !worker {
        // A project kept on this computer left the lease loop: nothing renews
        // its proof any more and nobody else may take it, so it holds none
        // and its agents are never fenced for it (`kept_here`).
        let preferences = lock(&state.pro.preferences);
        proofs.retain(|workspace, _| {
            !preferences
                .get(workspace)
                .is_some_and(|p| p.never_mirror && !p.privacy_pending)
        });
    }
    let mut expired = Vec::new();
    let mut fenced = false;
    let now = std::time::Instant::now();
    for (workspace, proof) in proofs.iter_mut() {
        if proof.generation == generation && (proof.stopped || !proof.deadline.valid()) {
            // A resumed machine's own renewal is still in flight.
            if !proof.stopped && proof.renew_until.is_some_and(|until| now < until) {
                continue;
            }
            fenced |= !proof.stopped;
            proof.stopped = true;
            // A cloud machine cannot prove its processes stopped; a computer
            // signals its own and resumes them itself once it holds the
            // project again (`watchdog::preserve`).
            if worker {
                lock(&state.pro.preferences)
                    .entry(workspace.clone())
                    .or_default()
                    .execution_uncertain = true;
            }
            expired.push(workspace.clone());
        }
    }
    drop(proofs);
    if fenced {
        state.pro.execution.changed.notify_waiters();
    }
    expired
}
pub(super) fn invalidate(state: &AppState) -> Vec<String> {
    let mut proofs = lock(&state.pro.execution.proofs);
    let mut workspaces = proofs.keys().cloned().collect::<Vec<_>>();
    for proof in proofs.values_mut() {
        proof.stopped = true;
    }
    drop(proofs);
    for workspace in lock(&state.pro.execution.setups).keys() {
        if !workspaces.contains(workspace) {
            workspaces.push(workspace.clone());
        }
    }
    state.pro.execution.changed.notify_waiters();
    workspaces
}
/// The leases this computer holds right now, as sign-out stands them down
/// (`sign_out`): each project's current-generation proof and its epoch.
pub(super) fn held(state: &AppState) -> Vec<(String, u64, HeldProof)> {
    let generation = state.pro.generation.load(Ordering::Acquire);
    lock(&state.pro.execution.proofs)
        .iter()
        .filter(|(_, proof)| proof.generation == generation)
        .map(|(workspace, proof)| (workspace.clone(), proof.epoch, HeldProof(proof.clone())))
        .collect()
}
/// A held lease's proof, carried across sign-out's configuration change.
#[derive(Clone)]
pub(super) struct HeldProof(Proof);
/// Sign-out could not stand a project down at the account: its proof stays,
/// moved to the current generation with its own deadline, so `expire` still
/// fences its agents at that deadline (fail closed) and the watchdog runs.
pub(super) fn keep_held(state: &std::sync::Arc<AppState>, kept: Vec<(String, HeldProof)>) {
    if kept.is_empty() {
        return;
    }
    let generation = state.pro.generation.load(Ordering::Acquire);
    {
        let mut proofs = lock(&state.pro.execution.proofs);
        for (workspace, HeldProof(mut proof)) in kept {
            proof.generation = generation;
            proof.renew_until = None;
            proofs.insert(workspace, proof);
        }
    }
    state.pro.execution.changed.notify_waiters();
    start(state);
}
/// The account acknowledged that a signed-out computer stood a project down:
/// nobody takes it on this computer's behalf, so its proof goes and its
/// agents are no longer fenced for it.
pub(super) fn drop_held(state: &AppState, workspace: &str) {
    lock(&state.pro.execution.proofs).remove(workspace);
    state.pro.execution.changed.notify_waiters();
}
/// A kept proof that `expire` has not fenced yet (see `keep_held`).
pub(super) fn held_alive(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.execution.proofs)
        .get(workspace)
        .is_some_and(|proof| !proof.stopped && proof.deadline.valid())
}
pub(super) fn clear_stopped(state: &AppState) {
    lock(&state.pro.execution.proofs).clear();
    state.pro.execution.changed.notify_waiters();
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

/// First enrollment around agents already running here: they stay on their
/// processes and count as this life's managed workload, exactly as if they
/// had been launched after enrollment. The next state write records their
/// process groups (crash evidence a successor probes), and a fence reaches
/// them by session like any other managed agent.
pub(super) fn adopt_running(state: &AppState, workspace: &str) {
    let ids: Vec<_> = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, w)| w.as_str() == workspace)
        .map(|(id, _)| id.clone())
        .collect();
    let live = ids
        .into_iter()
        .filter(|id| managed_session(state, id))
        .any(|id| {
            state.chat.get(&id).is_some_and(|s| s.alive)
                || state.sessions.get(&id).is_some_and(|s| s.alive)
        });
    if !live {
        return;
    }
    let mut preferences = lock(&state.pro.preferences);
    if preferences.len() >= 128 && !preferences.contains_key(workspace) {
        return;
    }
    let preference = preferences.entry(workspace.to_owned()).or_default();
    preference.execution_active = true;
    preference.execution_boot = state.pro.execution.boot.clone();
}

/// Durable admission precedes spawning. On a crash, same-boot execution remains
/// closed until a trusted process supervisor proves the old workload stopped.
pub(crate) async fn prepare_launch(
    state: &std::sync::Arc<AppState>,
    workspace: &str,
) -> Result<Option<launch::Intent>> {
    if !managed(state, workspace) {
        return Ok(None);
    }
    let _configuration = if mutation::request_reserved() {
        // Configure/stop may already hold this lock while draining our
        // reservation. Do not wait on the operation that is waiting for us.
        state
            .pro
            .configuration
            .try_lock()
            .map_err(|_| mutation::Changed)?
    } else {
        state.pro.configuration.lock().await
    };
    ensure!(
        crate::pro::may_execute(state, workspace),
        "project execution authority unavailable"
    );
    let intent = launch::Intent::begin(state, workspace)?;
    {
        let mut preferences = lock(&state.pro.preferences);
        // An uncertain project (its record was lost) may have no preference
        // row yet; its launch still records evidence rather than failing.
        ensure!(
            preferences.len() < 128 || preferences.contains_key(workspace),
            "managed project identity unavailable"
        );
        let preference = preferences.entry(workspace.to_owned()).or_default();
        preference.execution_active = true;
        preference.execution_boot = state.pro.execution.boot.clone();
        preference.execution_launch_pending = true;
    }
    crate::pro::persist(state).await?;
    ensure!(
        crate::pro::may_execute(state, workspace),
        "execution authority changed during durable launch admission"
    );
    Ok(Some(intent))
}

/// Account unreachability as the lease watchdog sees it: the proof expired
/// and no renewal arrived. Returns what the watchdog would fence.
#[cfg(test)]
pub(crate) fn expired_lease_fixture(state: &AppState, workspace: &str) -> Vec<String> {
    if let Some(proof) = lock(&state.pro.execution.proofs).get_mut(workspace) {
        proof.deadline = lease::Deadline::expired_fixture();
    }
    expire(state, state.pro.generation.load(Ordering::Acquire))
}
#[cfg(test)]
pub(crate) fn lapse_fixture(state: &AppState, workspace: &str) {
    if let Some(proof) = lock(&state.pro.execution.proofs).get_mut(workspace) {
        proof.deadline = lease::Deadline::expired_fixture();
    }
}
/// A suspended machine resumed after its lease deadline passed, as the
/// watchdog's freeze detection sees it. Returns what the watchdog would fence.
#[cfg(any(test, feature = "daemon-extension-fixture"))]
pub(crate) fn resumed_fixture(state: &AppState, workspace: &str) -> Vec<String> {
    if let Some(proof) = lock(&state.pro.execution.proofs).get_mut(workspace) {
        proof.deadline = lease::Deadline::expired_fixture();
    }
    let generation = state.pro.generation.load(Ordering::Acquire);
    resumed(state, generation);
    expire(state, generation)
}
/// A suspended machine just thawed: its lease deadline passed while it was
/// frozen and its watchdog has not ticked since.
#[cfg(any(test, feature = "daemon-extension-fixture"))]
pub(crate) fn thawed_fixture(state: &AppState, workspace: &str) {
    if let Some(proof) = lock(&state.pro.execution.proofs).get_mut(workspace) {
        proof.deadline = lease::Deadline::expired_fixture();
    }
    ticked(
        state,
        std::time::Instant::now() - watchdog::FREEZE - Duration::from_secs(2),
    );
}
/// A verified other owner, as an authenticated baton read records it.
#[cfg(test)]
pub(crate) fn remote_owner_fixture(state: &AppState, workspace: &str, epoch: u64) {
    lock(&state.pro.ownership).insert(
        workspace.into(),
        Ownership::Remote {
            epoch,
            holder: "worker-fixture".into(),
        },
    );
}
/// Strict worker semantics for fixtures that exercise lease fencing.
#[cfg(test)]
pub(crate) fn worker_fixture(state: &AppState) {
    state.pro.worker.store(true, Ordering::Release);
}
/// Shared HTTP/scope fixtures install a normally validated synthetic grant;
/// production validation has no test-only permissive branch.
#[cfg(any(test, feature = "daemon-extension-fixture"))]
pub(crate) fn install_fixture(state: &AppState, workspace: &str, epoch: u64) -> Result<()> {
    grant_fixture(state, workspace, epoch, 1)?;
    lock(&state.pro.ownership).insert(workspace.into(), Ownership::Local { epoch });
    Ok(())
}
/// The account answered a resumed machine's renewal of its own epoch: the
/// lease loop accepts the same lease's next sequence (see `install_fixture`).
#[cfg(test)]
pub(crate) fn renewed_fixture(state: &AppState, workspace: &str, epoch: u64) -> Result<()> {
    let sequence = lock(&state.pro.execution.proofs)
        .get(workspace)
        .context("no execution proof to renew")?
        .lease
        .sequence;
    grant_fixture(state, workspace, epoch, sequence + 1)
}
/// The account refused a resumed machine's renewal (the lease loop fences).
#[cfg(test)]
pub(crate) fn refused_fixture(state: &AppState, workspace: &str) {
    fence_workspace(state, workspace);
}
#[cfg(any(test, feature = "daemon-extension-fixture"))]
fn grant_fixture(state: &AppState, workspace: &str, epoch: u64, sequence: u64) -> Result<()> {
    let config: Configure = serde_json::from_value(json!({
        "account_id":"a-fixture", "role":"device", "endpoint":"http://127.0.0.1:1",
        "keeper_url":"", "hours_exhausted":false,
        "execution":{"version":1,"installation_id":"i-home","capability":wire::ExecutionCapability::managed()},
        "delegation":{"access_token":"synthetic","expires_at":"2099-01-01T00:00:00Z","scope":["baton","mirror"],"device_id":"d-home"}
    }))?;
    let grant: Baton = serde_json::from_value(json!({
        "workspace_id":workspace,"holder_id":"d-home","epoch":epoch,"requires_fork":false,
        "server_now":"2026-09-28T00:00:00Z","expires_at":"2026-09-28T00:01:30Z",
        "continuity":{"version":2,"mode":"managed_v1","policy_revision":1,"preferred_installation_id":"i-home"},
        "execution_capability":wire::ExecutionCapability::managed(),
        "execution_lease":{"id":"lease-fixture","sequence":sequence}
    }))?;
    accept(
        state,
        &config,
        &grant,
        state.pro.generation.load(Ordering::Acquire),
        RequestStart::now(),
    )
}

/// Read off-reactor before any boot resurrection. An unknown existing parking
/// receipt never downgrades to an absent receipt and automatic agent restore.
pub(crate) async fn restore_manual_parking(
    state: &std::sync::Arc<AppState>,
    boot: crate::ledger::BootLedger,
) -> crate::ledger::BootLedger {
    #[cfg(target_os = "linux")]
    {
        let owner = state.clone();
        let fallback = boot.sessions.clone();
        let fallback_links = boot.links.clone();
        let fallback_written_at = boot.written_at;
        let result = tokio::task::spawn_blocking(move || {
            let mut boot = boot;
            if maintenance_store::overlay_boot(&owner, &mut boot).is_err() {
                for entry in &mut boot.sessions {
                    entry.suspended = true;
                    entry.manual_resume_reason = Some("unknown".into());
                }
                tracing::warn!(
                    "maintenance parking receipt unavailable; automatic restoration fenced"
                );
            }
            boot
        })
        .await;
        // A panicked parser worker must not expose a previously owned roster.
        // It normally cannot panic; retain the original roster outside it.
        result.unwrap_or_else(|_| crate::ledger::BootLedger {
            sessions: fallback
                .into_iter()
                .map(|mut entry| {
                    entry.suspended = true;
                    entry.manual_resume_reason = Some("unknown".into());
                    entry
                })
                .collect(),
            links: fallback_links,
            written_at: fallback_written_at,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = state;
        boot
    }
}
