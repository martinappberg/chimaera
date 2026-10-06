//! Deadline checks run outside Tokio and never wait for network or Git work.
use crate::{lock, AppState};
use std::{
    collections::HashSet,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

/// A tick (100 ms, or 1 s on a computer holding no lease; see `tick`) that took this long
/// was not scheduled normally: the process was frozen.
pub(super) const FREEZE: Duration = Duration::from_secs(3);
/// Wall and monotonic time disagreeing by this much across one tick is a
/// clock step (a resumed machine correcting its clock), as the lease sees it.
const JUMP: Duration = Duration::from_secs(1);

fn signal(state: &AppState, workspace: &str) {
    super::setup::signal(state, workspace);
    let ids: Vec<_> = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, id)| id.as_str() == workspace)
        .map(|(id, _)| id.clone())
        .collect();
    // Process registries are visited completely: truncating this list would leave
    // the tail executing forever after the one authority transition.
    for id in ids
        .into_iter()
        .filter(|id| super::managed_session(state, id))
    {
        if state.chat.get(&id).is_some_and(|s| s.alive) {
            state.chat.fence(&id);
        } else if state.sessions.get(&id).is_some_and(|s| s.alive) {
            let _ = state.sessions.fence(&id);
        }
    }
}
fn preserve(state: &AppState, workspaces: &[String]) {
    // The epoch each fence stopped its project at: what it preserves resumes
    // or travels only while this computer still holds it (review R4 S1).
    let epochs: std::collections::HashMap<String, u64> = {
        let proofs = lock(&state.pro.execution.proofs);
        workspaces
            .iter()
            .filter_map(|workspace| Some((workspace.clone(), proofs.get(workspace)?.epoch)))
            .collect()
    };
    for mut entry in crate::ledger::snapshot(state).0 {
        if workspaces.contains(&entry.workspace_id) {
            entry.suspended = true;
            entry.handoff = None;
            if entry.agent.is_some() {
                // A conversation already preserved keeps its first fence.
                entry.fence_epoch = entry
                    .fence_epoch
                    .or_else(|| epochs.get(&entry.workspace_id).copied());
                // Its process is stopped mid-turn: the row must not keep
                // saying it runs. Unknown raises no notice.
                if let Some(record) = lock(&state.agents).get_mut(&entry.id) {
                    if record.state == crate::agent_state::AgentState::Running {
                        record.state = crate::agent_state::AgentState::Unknown;
                    }
                }
            }
            lock(&state.deferred_sessions).insert(entry.id.clone(), entry);
        }
    }
}
/// The watchdog's tick: 100 ms on a cloud machine and on a computer while it
/// holds any lease proof, so thawed agents stop within a tick of the thaw
/// (review R4 S5); 1 s otherwise, keeping an idle laptop's CPU asleep.
const BUSY_TICK: Duration = Duration::from_millis(100);
const IDLE_TICK: Duration = Duration::from_secs(1);
pub(super) fn tick(state: &AppState) -> Duration {
    if super::worker(state) || !lock(&state.pro.execution.proofs).is_empty() {
        BUSY_TICK
    } else {
        IDLE_TICK
    }
}
/// A wake (`routes::woke`) checks every deadline at once instead of at the
/// next tick: the machine was frozen, so a lease that lapsed past anyone's
/// takeover fences now, before thawed agents run on, and one still inside
/// that window gets its one renewal first (`resumed`), as a tick would.
pub(in crate::pro) fn check_now(state: &AppState) {
    let generation = state.pro.generation.load(Ordering::Acquire);
    if super::resumed(state, generation) {
        state.pro.renew_now.notify_one();
    }
    for workspace in super::expire(state, generation) {
        signal(state, &workspace);
    }
}
pub(in crate::pro) fn start(state: &Arc<AppState>) {
    let weak = Arc::downgrade(state);
    let runtime = tokio::runtime::Handle::current();
    let generation = state.pro.generation.load(Ordering::Acquire);
    std::thread::spawn(move || {
        let mut recorded = HashSet::new();
        let mut last = (std::time::Instant::now(), std::time::SystemTime::now());
        loop {
            let tick = weak.upgrade().map_or(IDLE_TICK, |state| tick(&state));
            std::thread::sleep(tick);
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.stopping.load(Ordering::Acquire)
                || state.pro.generation.load(Ordering::Acquire) != generation
            {
                return;
            }
            // A tick far longer than its sleep, or the two clocks disagreeing
            // about it, means this process was frozen (a suspended machine
            // resumed) or the clock jumped. Renew before fencing: the account
            // kept a suspended owner's lease for it.
            let now = (std::time::Instant::now(), std::time::SystemTime::now());
            let monotonic = now.0.saturating_duration_since(last.0);
            let frozen = monotonic > FREEZE
                || now
                    .1
                    .duration_since(last.1)
                    .map_or(true, |wall| wall.abs_diff(monotonic) > JUMP);
            last = now;
            if frozen {
                // A frozen computer was not reachable: the guard for bringing
                // work home starts over.
                super::super::reach::unreachable(&state);
                if super::resumed(&state, generation) {
                    state.pro.renew_now.notify_one();
                }
            }
            super::ticked(&state, now.0);
            let expired = super::expire(&state, generation);
            // Signal every expired workspace before any durable journal work.
            for workspace in &expired {
                signal(&state, workspace);
            }
            let fresh: Vec<_> = expired
                .into_iter()
                .filter(|workspace| recorded.insert(workspace.clone()))
                .collect();
            if fresh.is_empty() {
                continue;
            }
            preserve(&state, &fresh);
            state.changes.notify_waiters();
            let owner = state.clone();
            runtime.spawn(async move {
                let _ = super::super::persist(&owner).await;
            });
        }
    });
}
/// Configuration replacement cannot dispose of its watchdog while old managed
/// processes are still running. Timeout retains a closed execution gate.
pub(in crate::pro) async fn stop(
    state: &Arc<AppState>,
    workspaces: &[String],
) -> anyhow::Result<()> {
    if workspaces.is_empty() {
        return Ok(());
    }
    for workspace in workspaces {
        signal(state, workspace);
    }
    preserve(state, workspaces);
    super::super::persist(state).await?;
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            super::reprobe(state);
            if workspaces
                .iter()
                .all(|workspace| super::quiescent(state, workspace))
            {
                return;
            }
            for workspace in workspaces {
                signal(state, workspace);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("previous execution is still stopping"))?;
    {
        let mut preferences = lock(&state.pro.preferences);
        for workspace in workspaces {
            if let Some(preference) = preferences.get_mut(workspace) {
                preference.execution_active = false;
            }
        }
    }
    for workspace in workspaces {
        super::launch::stopped(state, workspace);
    }
    super::super::persist(state).await?;
    Ok(())
}
