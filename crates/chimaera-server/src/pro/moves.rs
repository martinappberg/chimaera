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
use super::{engine, execution, protocol::Baton, protocol::Configure, protocol::Role, Ownership};
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
/// How often a waiting request re-reads ownership (a passive read).
const POLL: Duration = Duration::from_secs(2);
/// A take that failed (the account kept the project, the copy did not fetch)
/// is tried again this much later, at most `TAKES` times per request: each
/// try fetches the project's copy.
const RETAKE: Duration = Duration::from_secs(10);
const TAKES: usize = 3;
/// How often the holder checks for a pause while it yields.
const PAUSE_POLL: Duration = Duration::from_secs(1);
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
pub(super) enum Decision {
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
    acted: std::sync::Mutex<HashMap<String, u64>>,
    /// Projects this computer is handing to another computer: the epoch, the
    /// request's time on this computer's clock (unix ms) and the holder.
    yielding: std::sync::Mutex<HashMap<String, (u64, u64, String)>>,
    /// Projects whose work left this computer for another of the user's
    /// computers (not the cloud): what a viewer is told it continues on.
    leaving: std::sync::Mutex<std::collections::HashSet<String>>,
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

fn timestamp_ms(value: &str) -> Option<i128> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|at| at.unix_timestamp_nanos() / 1_000_000)
}

/// When the account recorded a request, on this computer's clock: its age by
/// the account's own clock (`server_now` minus the request time), taken back
/// from now. Two clocks are never compared directly.
pub(super) fn requested_here(baton: &Baton, now_ms: u64) -> Option<u64> {
    let server = timestamp_ms(&baton.server_now)?;
    let requested = timestamp_ms(baton.move_requested_at.as_deref()?)?;
    let age = u64::try_from((server - requested).max(0)).ok()?;
    Some(now_ms.saturating_sub(age))
}

/// The last actor wins: this computer's user acting after the request keeps
/// the work here; otherwise the holder yields at its next pause.
pub(super) fn decide(acted_ms: Option<u64>, requested_ms: u64) -> Decision {
    if acted_ms.is_some_and(|acted| acted > requested_ms) {
        Decision::Claim
    } else {
        Decision::Yield
    }
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
fn can_take(state: &AppState, config: &Configure, workspace: &str) -> bool {
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
#[cfg(test)]
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
            "capability": execution::wire::ExecutionCapability::checkpoint_fork(),
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
        let result = pull(
            &owner,
            &config,
            &workspace,
            PullRequest {
                generation,
                ask,
                expected_epoch,
                bound,
            },
            &mut stage,
        )
        .await;
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

/// The negotiated ownership read. Only negotiated computers take part
/// (`can_take`), so it is the v2 resource even before this computer has seen
/// the project's policy; the answer records it (`execution::observe`) like the
/// lease loop's own read.
async fn read(state: &AppState, config: &Configure, workspace: &str) -> Result<Baton> {
    let baton: Baton =
        engine::account(config, &execution::path(config, workspace, ""), "GET", None)
            .await?
            .json()
            .context("Could not confirm where your work is running")?;
    ensure!(baton.workspace_id == workspace, "baton workspace mismatch");
    execution::observe(state, config, &baton)?;
    Ok(baton)
}

struct PullRequest {
    generation: u64,
    ask: bool,
    expected_epoch: Option<u64>,
    bound: Duration,
}
async fn pull(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    request: PullRequest,
    stage: &mut &'static str,
) -> Result<()> {
    let PullRequest {
        generation,
        ask,
        expected_epoch,
        bound,
    } = request;
    let me = config.delegation.device_id.clone();
    let deadline = tokio::time::Instant::now() + bound;
    let current = || -> Result<()> {
        ensure!(
            generation
                == state
                    .pro
                    .generation
                    .load(std::sync::atomic::Ordering::Acquire)
                && can_take(state, config, workspace),
            "Account or project changed while taking over"
        );
        Ok(())
    };
    current()?;
    if ask {
        let baton = read(state, config, workspace).await?;
        current()?;
        ensure!(
            expected_epoch.is_none_or(|epoch| epoch == baton.epoch),
            "Project ownership changed before Take over"
        );
        *stage = "ask";
        if baton.holder_id.as_deref() == Some(me.as_str())
            && super::owned_epoch(state, workspace) == Some(baton.epoch)
        {
            return Ok(());
        }
        ask_to_move(config, workspace, &baton, deadline, &current).await?;
    }
    let mut last: Option<anyhow::Error> = None;
    let mut takes = 0;
    loop {
        *stage = "wait";
        current()?;
        ensure!(
            tokio::time::Instant::now() < deadline,
            "Take over timed out"
        );
        let baton = read(state, config, workspace).await?;
        current()?;
        let mine = baton.holder_id.as_deref() == Some(me.as_str());
        if mine && super::owned_epoch(state, workspace) == Some(baton.epoch) {
            return Ok(());
        }
        // Cancelled: the other computer's user acted after this request, or
        // another request replaced it.
        ensure!(
            mine || baton.move_to.as_deref() == Some(me.as_str()),
            "the other computer kept the work"
        );
        let mut wait = POLL;
        if (!mine && acquisition_ready(&baton))
            || (mine
                && super::project_copy::copy_only(state, workspace)
                && lock(&state.pro.preferences)
                    .get(workspace)
                    .and_then(|p| p.copy.as_ref())
                    .is_some_and(|copy| copy.takeover_requested))
        {
            // Released (or a paused cloud machine the account lets this
            // computer take): take it the way a return does.
            *stage = "take";
            ensure!(takes < TAKES, "the project could not be taken here");
            takes += 1;
            match engine::hydrate(state, config, workspace, baton.epoch, false, None).await {
                Ok(()) => continue,
                Err(error) => {
                    last = Some(error);
                    wait = RETAKE;
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(last
                .unwrap_or_else(|| anyhow::anyhow!("the other computer did not let go in time")));
        }
        tokio::time::sleep(wait).await;
    }
}

/// A revoked device's live lease can outlast sign-out. Only an explicit,
/// exact-conflict retry hint distinguishes that wait from a refused cloud move.
async fn ask_to_move(
    config: &Configure,
    workspace: &str,
    observed: &Baton,
    deadline: tokio::time::Instant,
    current: &(impl Fn() -> Result<()> + Sync),
) -> Result<()> {
    tokio::time::timeout_at(deadline, async {
        loop {
            current()?;
            let response = engine::account(
                config,
                &format!("/v2/baton/{workspace}/move"),
                "POST",
                Some(&json!({"holder_id": config.delegation.device_id, "epoch": observed.epoch})),
            )
            .await?;
            current()?;
            if response.status == 200 {
                let grant: Baton = response.json()?;
                ensure!(
                    grant.workspace_id == workspace && grant.epoch == observed.epoch,
                    "Project ownership changed before Take over"
                );
                return Ok(());
            }
            let delay = move_retry(&response, workspace, observed)
                .context("The account refused the request to move the work")?
                .max(POLL);
            let wake = (tokio::time::Instant::now() + delay).min(deadline);
            while tokio::time::Instant::now() < wake {
                current()?;
                tokio::time::sleep_until((tokio::time::Instant::now() + POLL).min(wake)).await;
            }
        }
    })
    .await
    .context("The previous computer's lease did not expire in time")?
}

fn move_retry(
    response: &super::transport::Response,
    workspace: &str,
    observed: &Baton,
) -> Option<Duration> {
    #[derive(serde::Deserialize)]
    struct Conflict {
        error: String,
        baton: Baton,
        retry_after_ms: u64,
    }
    if response.status != 409 {
        return None;
    }
    let conflict: Conflict = serde_json::from_slice(&response.body).ok()?;
    let baton = conflict.baton;
    let now = timestamp_ms(&baton.server_now)?;
    let expires = timestamp_ms(baton.expires_at.as_deref()?)?;
    (conflict.error == "held"
        && baton.workspace_id == workspace
        && baton.epoch == observed.epoch
        && baton.holder_id.is_some()
        && baton.holder_id == observed.holder_id
        && !baton.mirror_disabled
        && expires > now
        && (1..=BOUND.as_millis() as u64).contains(&conflict.retry_after_ms))
    .then(|| Duration::from_millis(conflict.retry_after_ms))
}

/// The negotiated account contract retains a thirty-second reconnect grace.
/// Wait cheaply on ownership before spending a bounded hydration attempt.
fn acquisition_ready(baton: &Baton) -> bool {
    // A phone request is issued only after the account proves a cleanly
    // suspended cloud owner. Its named destination may acquire immediately;
    // the account rechecks that proof atomically with acquisition. Waiting for
    // lease expiry here would outlive the phone's twenty-second fallback.
    if baton.holder_id.is_none() || baton.move_reason.as_deref() == Some("phone") {
        return true;
    }
    matches!((timestamp_ms(&baton.server_now), baton.expires_at.as_deref().and_then(timestamp_ms)),
        (Some(now), Some(expires)) if now >= expires + 30_000)
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
async fn claim(config: &Configure, workspace: &str, epoch: u64) -> Result<()> {
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
    let Some(requested) = requested_here(grant, now) else {
        return;
    };
    let acted = lock(&state.pro.moves.acted).get(workspace).copied();
    match decide(acted, requested) {
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
            tokio::spawn(async move {
                hand_over(&owner, &config, &workspace, epoch, requested).await;
            });
        }
    }
}

/// Finishes the current step here and hands the project over: at the next
/// pause (a chat's turn ends, a terminal agent pauses; plain shells never
/// wait), publish and release exactly like the clean handoff. Gives up when
/// the request is withdrawn, this computer's user acts, the project changes
/// hands, or the bound passes (the requesting computer gives up then too).
async fn hand_over(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    epoch: u64,
    requested: u64,
) {
    let deadline = requested.saturating_add(BOUND.as_millis() as u64);
    loop {
        let current = lock(&state.pro.moves.yielding)
            .get(workspace)
            .is_some_and(|(held, at, _)| *held == epoch && *at == requested);
        if !current || super::owned_epoch(state, workspace) != Some(epoch) || now_ms() > deadline {
            break;
        }
        if decide(
            lock(&state.pro.moves.acted).get(workspace).copied(),
            requested,
        ) == Decision::Claim
        {
            if let Err(error) = claim(config, workspace, epoch).await {
                tracing::info!(
                    phase = "keep_work",
                    category = engine::failure_code(&error),
                    "Could not keep the work on this computer"
                );
            }
            break;
        }
        if engine::at_pause(state, workspace) {
            lock(&state.pro.moves.leaving).insert(workspace.to_owned());
            if !super::routes::hand_to_computer(state, config, workspace, epoch).await {
                // Nothing moved (the flush recovered here): the other computer
                // hears it at once instead of waiting out its bound.
                lock(&state.pro.moves.leaving).remove(workspace);
                let _ = claim(config, workspace, epoch).await;
            }
            break;
        }
        tokio::time::sleep(PAUSE_POLL).await;
    }
    let mut yielding = lock(&state.pro.moves.yielding);
    if yielding
        .get(workspace)
        .is_some_and(|(held, at, _)| *held == epoch && *at == requested)
    {
        yielding.remove(workspace);
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

#[cfg(test)]
mod tests {
    use super::*;
    fn baton(server_now: &str, requested: Option<&str>) -> Baton {
        serde_json::from_value(json!({
            "workspace_id": "w-project",
            "holder_id": "d-home",
            "epoch": 4,
            "requires_fork": false,
            "server_now": server_now,
            "expires_at": "2026-09-30T00:01:30Z",
            "move_to": "d-other",
            "move_requested_at": requested,
            "move_reason": "computer",
        }))
        .unwrap()
    }

    #[test]
    fn the_last_actor_wins() {
        // Acting after the request keeps the work here; acting before it (or
        // never) hands it over at the next pause.
        assert_eq!(decide(Some(10_001), 10_000), Decision::Claim);
        assert_eq!(decide(Some(10_000), 10_000), Decision::Yield);
        assert_eq!(decide(Some(9_000), 10_000), Decision::Yield);
        assert_eq!(decide(None, 10_000), Decision::Yield);
    }

    #[test]
    fn a_request_is_placed_on_this_clock_by_its_age_on_the_accounts() {
        // The account's clock runs an hour ahead of this computer's: only the
        // request's age (30 s by the account) carries over.
        let grant = baton("2026-09-30T01:00:30Z", Some("2026-09-30T01:00:00Z"));
        assert_eq!(requested_here(&grant, 100_000), Some(70_000));
        // A request stamped after the account's own "now" is treated as new.
        let skewed = baton("2026-09-30T01:00:00Z", Some("2026-09-30T01:00:05Z"));
        assert_eq!(requested_here(&skewed, 100_000), Some(100_000));
        assert_eq!(
            requested_here(&baton("2026-09-30T01:00:00Z", None), 1),
            None
        );
        assert_eq!(
            requested_here(&baton("not a time", Some("2026-09-30T01:00:05Z")), 1),
            None
        );
        // Typing after the request (by the account's clock) wins.
        let requested = requested_here(&grant, 100_000).unwrap();
        assert_eq!(decide(Some(70_500), requested), Decision::Claim);
        assert_eq!(decide(Some(69_500), requested), Decision::Yield);
    }

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
    fn retry_body() -> serde_json::Value {
        json!({"error":"held", "retry_after_ms":1, "baton": {
            "workspace_id":"w-project", "holder_id":"opaque-device", "epoch":4,
            "requires_fork":false, "server_now":"2026-10-02T00:00:00Z",
            "expires_at":"2026-10-02T00:01:30Z"
        }})
    }
    #[test]
    fn only_explicit_same_owner_conflicts_are_retryable() {
        let body = retry_body();
        let observed: Baton = serde_json::from_value(body["baton"].clone()).unwrap();
        let response = |body: &serde_json::Value| super::super::transport::Response {
            status: 409,
            body: serde_json::to_vec(body).unwrap(),
        };
        assert_eq!(
            move_retry(&response(&body), "w-project", &observed),
            Some(Duration::from_millis(1))
        );
        for (pointer, value) in [
            ("/retry_after_ms", json!(null)),
            ("/retry_after_ms", json!(0)),
            ("/retry_after_ms", json!(-1)),
            ("/retry_after_ms", json!(300001)),
            ("/retry_after_ms", json!(1.5)),
            ("/error", json!("stale_epoch")),
            ("/baton/workspace_id", json!("w-other")),
            ("/baton/holder_id", json!("other")),
            ("/baton/holder_id", json!(null)),
            ("/baton/epoch", json!(5)),
            ("/baton/expires_at", json!("2026-10-01T23:59:59Z")),
            ("/baton/server_now", json!("invalid")),
        ] {
            let mut bad = body.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                move_retry(&response(&bad), "w-project", &observed).is_none(),
                "{pointer}"
            );
        }
        let mut bad = body.clone();
        bad["baton"]["mirror_disabled"] = json!(true);
        assert!(move_retry(&response(&bad), "w-project", &observed).is_none());
    }
    #[test]
    fn lapsed_owner_waits_the_account_grace_before_hydration() {
        let mut value = retry_body()["baton"].clone();
        for (now, ready) in [
            ("2026-10-02T00:01:29Z", false),
            ("2026-10-02T00:01:30Z", false),
            ("2026-10-02T00:01:59.999Z", false),
            ("2026-10-02T00:02:00Z", true),
        ] {
            value["server_now"] = json!(now);
            assert_eq!(
                acquisition_ready(&serde_json::from_value(value.clone()).unwrap()),
                ready
            );
        }
        value["holder_id"] = serde_json::Value::Null;
        assert!(acquisition_ready(&serde_json::from_value(value).unwrap()));
    }

    #[test]
    fn an_account_admitted_phone_return_does_not_wait_for_the_cloud_lease() {
        let mut grant = baton("2026-09-30T00:00:00Z", Some("2026-09-30T00:00:00Z"));
        assert!(!acquisition_ready(&grant));
        grant.move_reason = Some("phone".into());
        assert!(acquisition_ready(&grant));
        grant.move_reason = Some("unknown".into());
        assert!(!acquisition_ready(&grant));
    }
    #[tokio::test]
    async fn live_account_retry_is_bounded_and_account_change_stops_it() {
        use super::super::engine::continuity_tests::{device, FakeAccount};
        use std::sync::atomic::{AtomicBool, Ordering};
        let body = retry_body();
        let observed: Baton = serde_json::from_value(body["baton"].clone()).unwrap();
        let fake = FakeAccount::start(body["baton"].clone()).await;
        let path = "/v2/baton/w-project/move";
        fake.script("POST", path, 409, body.clone());
        let config = device(&fake.endpoint);
        let replacement = async {
            loop {
                if !fake.calls("POST", path).is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            fake.script("POST", path, 200, body["baton"].clone());
        };
        let (result, ()) = tokio::join!(
            ask_to_move(
                &config,
                "w-project",
                &observed,
                tokio::time::Instant::now() + Duration::from_secs(10),
                &|| Ok(())
            ),
            replacement
        );
        result.unwrap();
        assert!(fake.calls("POST", path).len() >= 2);
        for body in fake.calls("POST", path) {
            assert_eq!(body, json!({"holder_id":"d-home","epoch":4}));
        }
        lock(&fake.requests).clear();
        let mut held = retry_body();
        held["retry_after_ms"] = json!(5000);
        fake.script("POST", path, 409, held.clone());
        let current = AtomicBool::new(true);
        let changed = async {
            while fake.calls("POST", path).is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            current.store(false, Ordering::Release);
        };
        let check = || {
            ensure!(current.load(Ordering::Acquire), "account changed");
            Ok(())
        };
        let (result, ()) = tokio::join!(
            ask_to_move(
                &config,
                "w-project",
                &observed,
                tokio::time::Instant::now() + Duration::from_secs(10),
                &check
            ),
            changed
        );
        assert!(result.unwrap_err().to_string().contains("account changed"));
        assert_eq!(fake.calls("POST", path).len(), 1);
        lock(&fake.requests).clear();
        assert!(ask_to_move(
            &config,
            "w-project",
            &observed,
            tokio::time::Instant::now() + Duration::from_secs(1),
            &|| Ok(())
        )
        .await
        .is_err());
        assert_eq!(fake.calls("POST", path).len(), 1);
        lock(&fake.requests).clear();
        held.as_object_mut().unwrap().remove("retry_after_ms");
        fake.script("POST", path, 409, held);
        assert!(ask_to_move(
            &config,
            "w-project",
            &observed,
            tokio::time::Instant::now() + Duration::from_secs(10),
            &|| Ok(())
        )
        .await
        .is_err());
        assert_eq!(fake.calls("POST", path).len(), 1);
    }
}
