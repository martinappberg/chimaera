//! Explicit takeover moves execution; opening and ordinary input do not.
//!
//! **Between computers.** Opening creates a local copy. Ordinary input stays
//! with its current owner. Explicit Take over posts the existing account move
//! request, waits for the holder to drain/release, then hydrates the checkpoint.
//! The last actor rule still lets the current owner keep its work.
//!
//! **From a phone.** When a phone acts on a project a sleeping cloud machine
//! holds, the account names one of the user's online computers (`move_to`,
//! reason `phone`); that computer takes the project at once, without waking
//! the cloud machine and without the five-minute settle wait.
//!
//! Only negotiated (v2) personal computers take part; the account keeps the
//! request and refuses anyone else's acquire while it is fresh.
use super::{engine, protocol::Baton, protocol::Configure, protocol::Role, Ownership};
use crate::{lock, AppState};
use anyhow::{ensure, Context, Result};
use serde_json::json;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::watch;

/// How long a request waits for the other computer to let go: the same bound
/// a computer gives its agents to reach a pause once another owner is verified.
pub(super) const BOUND: Duration = Duration::from_secs(300);
/// A phone's request is answered within the account's own short wait (it wakes
/// the cloud machine after about twenty seconds); this computer stops trying
/// shortly after.
const PHONE_BOUND: Duration = Duration::from_secs(90);
const LIMIT: usize = 32;
/// Two readings of one request's time on this clock differ by the round trips
/// that carried them; a new request from the same computer is further apart.
const SAME_REQUEST_MS: u64 = 5_000;

/// Where a request to bring the work here stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Waiting,
    /// The project is this computer's now and its sessions resumed.
    Here,
    /// The other computer kept it (its user acted, it did not reach a pause in
    /// time, or its handover failed), or the account refused the request.
    Refused,
}

/// What the holder does about a request naming another computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// This computer's user acted after the request: the work stays here and
    /// the request is cancelled.
    Claim,
    /// Finish the current step, then hand the work over.
    Yield,
}

#[derive(Default)]
pub(super) struct Moves {
    /// Projects this computer is bringing here, and who is waiting for them.
    pulls: std::sync::Mutex<HashMap<String, watch::Sender<Outcome>>>,
    /// When this computer's own user last acted in each project (unix ms).
    pub(in crate::pro) acted: std::sync::Mutex<HashMap<String, u64>>,
    /// Projects this computer is handing to another computer: the epoch, the
    /// request's time on this computer's clock (unix ms) and the holder.
    pub(in crate::pro) yielding: std::sync::Mutex<HashMap<String, (u64, u64, String)>>,
    /// Projects whose work left this computer for another of the user's
    /// computers (not the cloud): what a viewer is told it continues on.
    pub(in crate::pro) leaving: std::sync::Mutex<std::collections::HashSet<String>>,
    /// The account's requests this computer already answered (by project, the
    /// request's time as the account wrote it): one attempt per request.
    answered: std::sync::Mutex<HashMap<String, String>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// When the account recorded a request, on this computer's clock: its age by
/// the account's own clock (`server_now` minus the request time), taken back
/// from now. Two clocks are never compared directly.
pub(super) fn requested_here(state: &AppState, baton: &Baton, now_ms: u64) -> Option<u64> {
    state
        .daemon_extension
        .as_ref()?
        .move_requested_here(baton, now_ms)
}

/// The last actor wins: this computer's user acting after the request keeps
/// the work here; otherwise the holder yields at its next pause.
pub(super) fn decide(state: &AppState, acted_ms: Option<u64>, requested_ms: u64) -> Decision {
    state
        .daemon_extension
        .as_ref()
        .map_or(Decision::Claim, |runtime| {
            runtime.move_decision(acted_ms, requested_ms)
        })
}

/// The user acted on this computer in `workspace` (a chat command or typing
/// that arrived on this daemon's own sockets, not a forwarded viewer's).
pub(crate) fn acted_here(state: &AppState, workspace: &str) {
    if workspace.is_empty() || lock(&state.pro.runtime).is_none() {
        return;
    }
    let mut acted = lock(&state.pro.moves.acted);
    if acted.len() >= 128 && !acted.contains_key(workspace) {
        acted.clear();
    }
    acted.insert(workspace.to_owned(), now_ms());
}

/// This project's work left (or is leaving) this computer for another of the
/// user's computers rather than the cloud.
pub(crate) fn other_computer(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.moves.leaving).contains(workspace)
}

/// Whether this computer may take `workspace` when the user acts on it here:
/// a configured personal computer on the negotiated protocol, the project
/// registered here, allowed to leave its computer and not handed to the
/// cloud on quit.
pub(in crate::pro) fn can_take(state: &AppState, config: &Configure, workspace: &str) -> bool {
    config.role == Role::Device
        && config.execution.is_some()
        && config.delegation.workspace.is_none()
        && !config.recovery
        && lock(&state.workspaces).get(workspace).is_some()
        && super::projects::account_matches(state, workspace)
        && !super::parked(state, workspace)
        && !lock(&state.pro.preferences)
            .get(workspace)
            .is_some_and(|p| p.never_mirror)
}

/// The passive ownership read's additive query: whether this computer could
/// take the project now (so a phone's action may be sent here rather than
/// waking the cloud) and whether it is on power. Only a personal computer on
/// the negotiated protocol says anything.
pub(super) fn watch_query(state: &AppState, config: &Configure, workspace: &str) -> &'static str {
    if super::project_copy::copy_only(state, workspace) || !can_take(state, config, workspace) {
        return "";
    }
    if state
        .pro
        .power_suitable
        .load(std::sync::atomic::Ordering::Acquire)
    {
        "?ready=1&power=ac"
    } else {
        "?ready=1&power=battery"
    }
}

/// A signed-in personal computer on the negotiated protocol whose account is
/// at `endpoint`: one that may take a project another computer runs, for
/// fixtures that exercise its current-owner viewer relay.
#[cfg(any(test, feature = "daemon-extension-fixture"))]
pub(crate) fn device_fixture(state: &AppState, endpoint: &str) {
    let config = serde_json::from_value(json!({
        "account_id": "a-fixture",
        "role": "device",
        "endpoint": endpoint,
        "keeper_url": "",
        "hours_exhausted": false,
        "execution": {
            "version": 1,
            "installation_id": "i-home",
            "capability": super::execution::wire::ExecutionCapability::checkpoint_fork(),
        },
        "delegation": {
            "access_token": "synthetic",
            "expires_at": "2099-01-01T00:00:00Z",
            "scope": ["baton", "mirror"],
            "device_id": "d-home",
        },
    }))
    .expect("device configuration fixture");
    *lock(&state.pro.runtime) = Some(config);
}

/// Ends the request under way to bring `workspace` here with `outcome`, for
/// fixtures that need a move to settle without an account behind it.
#[cfg(test)]
pub(crate) fn settle_fixture(state: &AppState, workspace: &str, outcome: Outcome) {
    if let Some(sender) = lock(&state.pro.moves.pulls).remove(workspace) {
        let _ = sender.send(outcome);
    }
}

/// The account asked this computer to take `workspace` (a phone acted while
/// the cloud slept, or this computer's own earlier request outlived the
/// daemon that made it): take it at once, unless a request is under way.
pub(super) fn answer(state: &Arc<AppState>, config: &Configure, workspace: &str, baton: &Baton) {
    if !can_take(state, config, workspace)
        || pulling(state, workspace)
        || (super::project_copy::copy_only(state, workspace)
            && !lock(&state.pro.preferences)
                .get(workspace)
                .and_then(|p| p.copy.as_ref())
                .is_some_and(|copy| copy.takeover_requested))
    {
        return;
    }
    let request = baton.move_requested_at.clone().unwrap_or_default();
    {
        let mut answered = lock(&state.pro.moves.answered);
        if answered.get(workspace) == Some(&request) {
            return;
        }
        if answered.len() >= LIMIT && !answered.contains_key(workspace) {
            answered.clear();
        }
        answered.insert(workspace.to_owned(), request);
    }
    let bound = if baton.move_reason.as_deref() == Some("phone") {
        PHONE_BOUND
    } else {
        BOUND
    };
    let _ = start(state, config.clone(), workspace, false, None, bound);
}

fn start(
    state: &Arc<AppState>,
    config: Configure,
    workspace: &str,
    ask: bool,
    expected_epoch: Option<u64>,
    bound: Duration,
) -> Option<watch::Receiver<Outcome>> {
    let mut pulls = lock(&state.pro.moves.pulls);
    if let Some(waiting) = pulls.get(workspace) {
        if *waiting.borrow() == Outcome::Waiting {
            return Some(waiting.subscribe());
        }
    }
    pulls.retain(|_, sender| *sender.borrow() == Outcome::Waiting);
    if pulls.len() >= LIMIT {
        return None;
    }
    // Admission and idle-only intent retirement share this lock. A copy must
    // still have an explicit durable intent when its pull becomes active.
    let copy_request = lock(&state.pro.preferences)
        .get(workspace)
        .and_then(|p| p.copy.as_ref())
        .filter(|copy| copy.takeover_requested)
        .and_then(|copy| copy.takeover_request.clone());
    if super::project_copy::copy_only(state, workspace) && copy_request.is_none() {
        return None;
    }
    let (sender, receiver) = watch::channel(Outcome::Waiting);
    pulls.insert(workspace.to_owned(), sender);
    drop(pulls);
    let generation = state
        .pro
        .generation
        .load(std::sync::atomic::Ordering::Acquire);
    let owner = state.clone();
    let workspace = workspace.to_owned();
    tokio::spawn(async move {
        let mut stage = "read";
        let project = engine::project_host::ProjectOwner::move_capture(
            owner.clone(),
            config.clone(),
            workspace.clone(),
            generation,
        );
        let result = match owner.daemon_extension.as_ref() {
            Some(runtime) => {
                runtime
                    .pull_move(project, ask, expected_epoch, bound, &mut stage)
                    .await
            }
            None => Err(anyhow::anyhow!("optional_runtime_unavailable")),
        };
        let outcome = match result {
            Ok(()) => Outcome::Here,
            Err(error) => {
                tracing::info!(
                    phase = "move_here",
                    stage,
                    category = engine::failure_code(&error),
                    "The work stayed on the other computer"
                );
                // A request nobody will act on must not keep the project
                // reserved for this computer.
                if ask {
                    let _ = cancel(&config, &workspace).await;
                }
                Outcome::Refused
            }
        };
        if outcome == Outcome::Refused {
            if let Some(request) = copy_request {
                let _ =
                    super::project_copy::cancel_takeover(&owner, &workspace, generation, &request)
                        .await;
            }
        }
        if let Some(sender) = lock(&owner.pro.moves.pulls).remove(&workspace) {
            let _ = sender.send(outcome);
        }
    });
    Some(receiver)
}

/// Signing out: nothing this computer asked for or was asked about carries
/// over to another account. A request under way ends on its own (its
/// transfer checks the account generation).
pub(super) fn forget(state: &AppState) {
    lock(&state.pro.moves.answered).clear();
    lock(&state.pro.moves.acted).clear();
    lock(&state.pro.moves.yielding).clear();
    lock(&state.pro.moves.leaving).clear();
}

/// Whether a request to bring this project here is under way (a return pass
/// leaves it to that request).
pub(super) fn pulling(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro.moves.pulls)
        .get(workspace)
        .is_some_and(|sender| *sender.borrow() == Outcome::Waiting)
}

/// Serialize a pre-start retirement against pull admission. The callback only
/// mutates in-memory intent; persistence happens after this synchronous lock.
pub(super) fn if_idle<T>(
    state: &AppState,
    workspace: &str,
    retire: impl FnOnce() -> T,
) -> Option<T> {
    let pulls = lock(&state.pro.moves.pulls);
    if pulls
        .get(workspace)
        .is_some_and(|sender| *sender.borrow() == Outcome::Waiting)
    {
        return None;
    }
    Some(retire())
}

/// Explicit local takeover preserves the existing account move body and checks
/// the epoch the user saw before submitting that move.
pub(super) async fn take_over_here(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    expected_epoch: u64,
) -> Result<()> {
    ensure!(
        can_take(state, config, workspace),
        "Take over is unavailable on this device"
    );
    let mut receiver = start(
        state,
        config.clone(),
        workspace,
        true,
        Some(expected_epoch),
        BOUND,
    )
    .context("Too many takeover requests are pending")?;
    loop {
        let outcome = *receiver.borrow_and_update();
        match outcome {
            Outcome::Here => return Ok(()),
            Outcome::Refused => anyhow::bail!("The account kept execution with the current owner"),
            Outcome::Waiting => receiver.changed().await.context("Take over stopped")?,
        }
    }
}

/// Withdraws this computer's request, if it is still the one waiting.
async fn cancel(config: &Configure, workspace: &str) -> Result<()> {
    engine::account(
        config,
        &format!("/v2/baton/{workspace}/move"),
        "DELETE",
        None,
    )
    .await?;
    Ok(())
}

/// The holder's user acted after another computer asked for the work: keep it
/// here and cancel that request (the account clears it for its own holder).
pub(in crate::pro) async fn claim(config: &Configure, workspace: &str, epoch: u64) -> Result<()> {
    let response = engine::account(
        config,
        &format!("/v2/baton/{workspace}/move"),
        "POST",
        Some(&json!({"holder_id": config.delegation.device_id, "epoch": epoch})),
    )
    .await?;
    ensure!(response.status == 200, "the account kept the request");
    Ok(())
}

/// A renewal of a project this computer runs: act on a request naming another
/// computer. No request (or it lapsed) clears what an earlier one left.
pub(super) fn consider(state: &Arc<AppState>, config: &Configure, workspace: &str, grant: &Baton) {
    if config.role != Role::Device || config.execution.is_none() {
        return;
    }
    let me = config.delegation.device_id.as_str();
    let target = grant
        .move_to
        .as_deref()
        .filter(|target| *target != me && super::valid_id(target));
    let Some(target) = target else {
        lock(&state.pro.moves.yielding).remove(workspace);
        lock(&state.pro.moves.leaving).remove(workspace);
        return;
    };
    let now = now_ms();
    let Some(requested) = requested_here(state, grant, now) else {
        return;
    };
    let acted = lock(&state.pro.moves.acted).get(workspace).copied();
    match decide(state, acted, requested) {
        Decision::Claim => {
            lock(&state.pro.moves.yielding).remove(workspace);
            let config = config.clone();
            let workspace = workspace.to_owned();
            let epoch = grant.epoch;
            tokio::spawn(async move {
                if let Err(error) = claim(&config, &workspace, epoch).await {
                    tracing::info!(
                        phase = "keep_work",
                        category = engine::failure_code(&error),
                        "Could not keep the work on this computer"
                    );
                }
            });
        }
        Decision::Yield => {
            {
                let mut yielding = lock(&state.pro.moves.yielding);
                // The same request, placed again from the next renewal (its
                // age on this clock moves with each round trip).
                if yielding.get(workspace).is_some_and(|(epoch, at, holder)| {
                    *epoch == grant.epoch
                        && holder == target
                        && at.abs_diff(requested) < SAME_REQUEST_MS
                }) {
                    return;
                }
                if yielding.len() >= LIMIT && !yielding.contains_key(workspace) {
                    return;
                }
                yielding.insert(
                    workspace.to_owned(),
                    (grant.epoch, requested, target.to_owned()),
                );
            }
            let owner = state.clone();
            let config = config.clone();
            let workspace = workspace.to_owned();
            let epoch = grant.epoch;
            let admitted = engine::project_host::move_host::MoveYieldOwner::capture(
                owner.clone(),
                config,
                workspace,
                epoch,
                requested,
            );
            tokio::spawn(async move {
                let cleanup = admitted.clone();
                if let Some(runtime) = owner.daemon_extension.as_ref() {
                    let _ = runtime.hand_over_move(admitted).await;
                }
                cleanup.finish();
            });
        }
    }
}

/// A project this computer released for another computer that never took it
/// (its request was withdrawn or lapsed): the work is still here, so the
/// lease loop takes the released epoch back (no install, no fork) and resumes
/// what the handover stopped.
pub(super) fn abandoned(state: &AppState, baton: &Baton, previous: Option<&Ownership>) -> bool {
    baton.holder_id.is_none()
        && baton.move_to.is_none()
        && matches!(previous, Some(Ownership::Transferring { epoch }) if *epoch == baton.epoch)
        && other_computer(state, &baton.workspace_id)
}

#[cfg(test)]
pub(super) fn fill_requests_fixture(state: &AppState) {
    let mut pulls = lock(&state.pro.moves.pulls);
    for index in 0..LIMIT {
        pulls.insert(
            format!("w-pending-{index}"),
            watch::channel(Outcome::Waiting).0,
        );
    }
}

#[cfg(test)]
pub(super) fn pending_fixture(state: &AppState, workspace: &str) {
    lock(&state.pro.moves.pulls).insert(workspace.into(), watch::channel(Outcome::Waiting).0);
}

#[cfg(all(unix, feature = "daemon-extension-fixture"))]
pub(in crate::pro) fn seed_actor_fixture(state: &AppState, workspace: &str, acted_ms: Option<u64>) {
    let mut records = lock(&state.pro.moves.acted);
    if let Some(at) = acted_ms {
        records.insert(workspace.into(), at);
    } else {
        records.remove(workspace);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_older_account_without_the_request_fields_still_decodes() {
        let grant: Baton = serde_json::from_value(json!({
            "workspace_id": "w-project",
            "holder_id": "d-home",
            "epoch": 4,
            "requires_fork": false,
            "server_now": "2026-09-30T00:00:00Z",
            "expires_at": "2026-09-30T00:01:30Z",
        }))
        .unwrap();
        assert!(grant.move_to.is_none() && grant.move_requested_at.is_none());
    }
}
