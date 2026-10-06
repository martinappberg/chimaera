//! Deadline checks run outside Tokio and never wait for network or Git work.
use crate::{lock, AppState};
use std::{
    collections::HashSet,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

/// A 100 ms tick that took this long was not scheduled normally: the process
/// was frozen.
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
    for mut entry in crate::ledger::snapshot(state).0 {
        if workspaces.contains(&entry.workspace_id) {
            entry.suspended = true;
            entry.handoff = None;
            lock(&state.deferred_sessions).insert(entry.id.clone(), entry);
        }
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
            std::thread::sleep(Duration::from_millis(100));
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
