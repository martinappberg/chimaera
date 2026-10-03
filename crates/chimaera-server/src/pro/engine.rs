use super::{
    authority, config, execution, mirror,
    protocol::{Baton, Configure, MirrorCredentials, Role},
    transport, Ownership, WorkspaceStatus,
};
use crate::{lock, AppState};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

#[path = "handback.rs"]
mod handback;
#[path = "release.rs"]
mod release;
#[path = "snapshot_diagnostics.rs"]
mod snapshot_diagnostics;

#[derive(Serialize, Deserialize)]
pub(super) struct Manifest {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project: Option<super::projects::catalog::Metadata>,
    #[serde(default)]
    pub(super) branch: Option<String>,
    #[serde(default)]
    pub(super) repository_origin: Option<String>,
    #[serde(default)]
    pub(super) repository: Option<super::repository::Snapshot>,
    pub(super) workspace_id: String,
    pub root: PathBuf,
    pub(super) name: String,
    pub(super) epoch: u64,
    clean: bool,
    #[serde(default)]
    continuation: execution::wire::Continuation,
    profile: super::policy::CloudProfile,
    sessions: Vec<SessionArchive>,
    /// Additive: project paths this snapshot deliberately left out. Only a
    /// snapshot that carries this inventory can show that a file is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) left_out: Option<Vec<PathBuf>>,
}
#[derive(Serialize, Deserialize)]
struct SessionArchive {
    id: String,
    archive: String,
}

pub(super) async fn account(
    config: &Configure,
    path: &str,
    method: &str,
    body: Option<&serde_json::Value>,
) -> Result<transport::Response> {
    if config.recovery {
        ensure!(
            method == "POST"
                && matches!(
                    path,
                    "/v2/recovery/mirror/credentials"
                        | "/v2/recovery/checkpoint"
                        | "/v2/recovery/release"
                ),
            "recovery authority cannot execute ordinary account requests"
        );
    }
    authority::account_request(config, path, method, body)?;
    let response = transport::request(
        &config.endpoint,
        path,
        method,
        &config.delegation.access_token,
        body,
    )
    .await?;
    ensure!(
        !transport::return_window_ended(&response),
        transport::RETURN_WINDOW_ENDED
    );
    Ok(response)
}
pub(super) async fn credentials(
    config: &Configure,
    workspace: &str,
    epoch: Option<u64>,
) -> Result<MirrorCredentials> {
    authority::config_workspace(config, workspace)?;
    let mut body = json!({"workspace_id":workspace});
    if let Some(epoch) = epoch {
        body["epoch"] = epoch.into();
    }
    let credentials: MirrorCredentials = account(
        config,
        if config.recovery {
            "/v2/recovery/mirror/credentials"
        } else if config.execution.is_some() {
            "/v2/mirror/credentials"
        } else {
            "/v1/mirror/credentials"
        },
        "POST",
        Some(&body),
    )
    .await?
    .json()?;
    ensure!(
        credentials.workspace_id == workspace
            && credentials.storage_limit_bytes > 0
            && credentials.max_file_bytes > 0,
        "invalid mirror grant"
    );
    for raw in [&credentials.repository_url, &credentials.working_tree_url] {
        let url = transport::endpoint(raw)?;
        ensure!(
            !config.endpoint.starts_with("https:") || url.starts_with("https:"),
            "mirror TLS downgrade"
        );
    }
    ensure!(
        credentials.read_only == epoch.is_none()
            && !credentials.password.is_empty()
            && credentials.password.len() <= 8192
            && credentials.username.len() <= 512
            && !credentials.password.chars().any(char::is_control)
            && !credentials.username.chars().any(char::is_control),
        "invalid mirror credential scope"
    );
    Ok(credentials)
}
/// Every project is copied this often whatever its agents do: the backstop
/// for work no agent did (the user's own edits, a plain shell).
const TIMED_COPY: u64 = 120;
/// A copy this soon after the last one started would mostly republish it; a
/// run of short turns is copied at most this often.
const TURN_COPY_GAP: u64 = 20;

/// Which projects had an agent finish a step since their last copy. A
/// finished turn is the moment its work is whole, so that project is copied
/// then instead of at the next timer pass: after a sudden loss of this
/// computer the cloud continues from the end of the last turn, not from up to
/// two minutes before it. Fed by the lease loop each tick; holds only ids the
/// loop iterates.
#[derive(Default)]
struct TurnEnds {
    working: BTreeSet<String>,
    ended: BTreeSet<String>,
}

impl TurnEnds {
    fn observe(&mut self, workspace: &str, working: bool) {
        if working {
            self.working.insert(workspace.to_owned());
        } else if self.working.remove(workspace) {
            self.ended.insert(workspace.to_owned());
        }
    }

    /// Projects the loop no longer iterates are forgotten.
    fn keep(&mut self, seen: &BTreeSet<String>) {
        self.working.retain(|id| seen.contains(id));
        self.ended.retain(|id| seen.contains(id));
    }

    /// Whether a copy should start, given the seconds since the last one
    /// started: `Some(None)` is every project (the timer), `Some(Some(ids))`
    /// the projects whose turn ended, `None` not yet.
    fn due(&self, since_last: u64) -> Option<Option<BTreeSet<String>>> {
        if since_last >= TIMED_COPY {
            Some(None)
        } else if !self.ended.is_empty() && since_last >= TURN_COPY_GAP {
            Some(Some(self.ended.clone()))
        } else {
            None
        }
    }

    /// A copy started for these projects (every project when `None`). A turn
    /// that ends while it runs is seen by a later tick and copied next.
    fn copied(&mut self, only: &Option<BTreeSet<String>>) {
        match only {
            None => self.ended.clear(),
            Some(ids) => self.ended.retain(|id| !ids.contains(id)),
        }
    }
}

pub(super) fn start(state: Arc<AppState>) {
    let generation = state.pro.generation.load(Ordering::Acquire);
    let task_state = state.clone();
    let task = tokio::spawn(async move {
        let state = task_state;
        let mut last_mirror = 0;
        let mut turns = TurnEnds::default();
        let mut renewed = super::now();
        let mut next_renewal = 0;
        let mut unauthorized = false;
        loop {
            if state.stopping.load(Ordering::Relaxed)
                || generation != state.pro.generation.load(Ordering::Acquire)
            {
                return;
            }
            let Some(config) = lock(&state.pro.runtime).clone() else {
                return;
            };
            // Hourly, and at once after the account refused this credential
            // (then at most once a minute while it keeps failing).
            if (super::now().saturating_sub(renewed) >= 3600 || unauthorized)
                && super::now() >= next_renewal
            {
                if renew_delegation(&state, &config, generation).await {
                    renewed = super::now();
                } else {
                    next_renewal = super::now() + 60;
                }
            }
            unauthorized = false;
            // Previous-life processes that have exited release their fence.
            execution::reprobe(&state);
            let workspaces = lock(&state.workspaces).list();
            let mut seen = BTreeSet::new();
            for workspace in workspaces
                .into_iter()
                .filter(|workspace| eligible(&state, workspace))
                .take(128)
            {
                if lock(&state.pro.preferences)
                    .get(&workspace.id)
                    .is_some_and(|p| p.never_mirror)
                    && !matches!(
                        lock(&state.pro.ownership).get(&workspace.id),
                        Some(Ownership::AwaitingVerification { .. })
                    )
                {
                    continue;
                }
                if let Err(error) = reconcile(&state, &config, &workspace.id).await {
                    unauthorized |= error
                        .chain()
                        .any(|cause| cause.to_string() == transport::UNAUTHORIZED);
                    record_error(&state, &workspace.id, &error);
                }
                turns.observe(
                    &workspace.id,
                    super::owned_epoch(&state, &workspace.id).is_some()
                        && !working_agents(&state, &workspace.id).is_empty(),
                );
                seen.insert(workspace.id);
            }
            turns.keep(&seen);
            let copy = turns.due(super::now().saturating_sub(last_mirror));
            if let Some(only) = copy.filter(|_| {
                !super::drain::draining(&state)
                    && lock(&state.pro.mirror_task)
                        .as_ref()
                        .is_none_or(|task| task.is_finished())
            }) {
                turns.copied(&only);
                let owner = state.clone();
                let config = config.clone();
                let task = tokio::spawn(async move {
                    let _guard = owner.pro.jobs.lock().await;
                    // Locating returning projects stays on the timer: a copy
                    // after a turn publishes that project and nothing else.
                    if only.is_none() {
                        if let Err(error) = lazy_handback(&owner, &config).await {
                            tracing::warn!(phase="locate_return", error=%error, "Could not locate returning projects");
                        }
                    }
                    let workspaces = lock(&owner.workspaces).list();
                    for workspace in workspaces
                        .into_iter()
                        .filter(|workspace| eligible(&owner, workspace))
                        .filter(|workspace| {
                            only.as_ref().is_none_or(|ids| ids.contains(&workspace.id))
                        })
                        .take(128)
                    {
                        if generation != owner.pro.generation.load(Ordering::Acquire) {
                            return;
                        }
                        // An unrenewed lease stops publication, never local work;
                        // the lease loop re-establishes it quietly.
                        if super::owned_epoch(&owner, &workspace.id).is_none()
                            || !execution::lease_valid(&owner, &workspace.id)
                            || lock(&owner.pro.preferences)
                                .get(&workspace.id)
                                .is_some_and(|p| p.never_mirror)
                        {
                            continue;
                        }
                        if let Err(error) = snapshot(&owner, &config, &workspace.id, false).await {
                            record_error(&owner, &workspace.id, &error);
                        }
                    }
                });
                *lock(&state.pro.mirror_task) = Some(task);
                last_mirror = super::now();
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(5)) => {}
                () = state.pro.renew_now.notified() => {}
            }
        }
    });
    if let Some(old) = lock(&state.pro.task).replace(task) {
        old.abort();
    }
}
/// Renews this daemon's delegation. A definitive refusal (401/403) marks it
/// refused so `/pro/status` tells the native app to mint a new one; a
/// transport failure only retries later.
pub(super) async fn renew_delegation(
    state: &AppState,
    config: &Configure,
    generation: u64,
) -> bool {
    match account(config, "/v1/delegations/renew", "POST", Some(&json!({}))).await {
        Ok(response) if matches!(response.status, 401 | 403) => {
            state.pro.delegation_refused.store(true, Ordering::Release);
            state.changes.notify_waiters();
            false
        }
        Ok(response) => {
            let installed =
                response
                    .json::<super::protocol::Delegation>()
                    .is_ok_and(|delegation| {
                        authority::install_renewal(
                            state,
                            generation,
                            &config.delegation,
                            delegation,
                        )
                    });
            if installed {
                state.pro.delegation_refused.store(false, Ordering::Release);
            }
            installed
        }
        Err(_) => false,
    }
}

fn record_error(state: &AppState, workspace: &str, error: &anyhow::Error) {
    let message: String = error.to_string().chars().take(256).collect();
    let mut statuses = lock(&state.pro.status);
    let status = statuses.entry(workspace.into()).or_default();
    status.error = Some(message);
    status.error_code = Some(super::routes::error_code(error));
}
pub(super) async fn reconcile(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
) -> Result<()> {
    let generation = state.pro.generation.load(Ordering::Acquire);
    reconcile_generation(state, config, workspace, generation)
        .await
        .map(|_| ())
}
async fn reconcile_generation(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    generation: u64,
) -> Result<Option<u64>> {
    authority::config_matches(state, config, workspace)?;
    // A local copy never renews/acquires execution or advertises a return
    // destination. Only the explicit takeover operation may hydrate it.
    if super::project_copy::copy_only(state, workspace) {
        ensure!(
            generation == state.pro.generation.load(Ordering::Acquire)
                && super::projects::account_matches(state, workspace),
            "Account changed during copy owner read"
        );
        let baton: Baton = account(config, &execution::path(config, workspace, ""), "GET", None)
            .await?
            .json()?;
        ensure!(
            baton.workspace_id == workspace
                && generation == state.pro.generation.load(Ordering::Acquire),
            "Copy owner read changed"
        );
        let _configuration = state.pro.configuration.lock().await;
        ensure!(
            generation == state.pro.generation.load(Ordering::Acquire)
                && super::projects::account_matches(state, workspace),
            "Account changed during copy owner read"
        );
        if !super::project_copy::copy_only(state, workspace) {
            return Ok(None);
        }
        // An older passive answer must not regress an admitted takeover. An
        // active owned move already observes its own epoch through hydration.
        if super::moves::pulling(state, workspace) {
            return Ok(None);
        }
        let previous = lock(&state.pro.ownership).get(workspace).cloned();
        let newer = previous.as_ref().is_some_and(|ownership| match ownership {
            Ownership::Local { epoch }
            | Ownership::Remote { epoch, .. }
            | Ownership::Hydrating { epoch }
            | Ownership::SettingUp { epoch }
            | Ownership::Transferring { epoch }
            | Ownership::AwaitingVerification { epoch }
            | Ownership::PrivacyDisabled { epoch } => *epoch > baton.epoch,
        });
        if newer {
            return Ok(None);
        }
        if let Some(copy) = lock(&state.pro.preferences)
            .get_mut(workspace)
            .and_then(|p| p.copy.as_mut())
        {
            copy.owner_epoch = Some(baton.epoch);
        }
        if let Some(holder) = baton
            .holder_id
            .as_ref()
            .filter(|holder| *holder != &config.delegation.device_id)
        {
            lock(&state.pro.ownership).insert(
                workspace.to_owned(),
                Ownership::Remote {
                    epoch: baton.epoch,
                    holder: holder.clone(),
                },
            );
        } else if baton.holder_id.is_none() && matches!(previous, Some(Ownership::Remote { .. })) {
            lock(&state.pro.ownership).remove(workspace);
        }
        let requested = lock(&state.pro.preferences)
            .get(workspace)
            .and_then(|p| p.copy.as_ref())
            .is_some_and(|copy| copy.takeover_requested);
        super::persist(state).await?;
        if requested
            && (baton.move_to.as_deref() == Some(config.delegation.device_id.as_str())
                || baton.holder_id.as_deref() == Some(config.delegation.device_id.as_str()))
        {
            super::moves::answer(state, config, workspace, &baton);
        }
        return Ok(None);
    }
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed during project transfer"
    );
    ensure!(
        super::projects::account_matches(state, workspace),
        "This project belongs to another account"
    );
    // Stopped for sleep and not handed over: renewing now would keep the lease
    // from lapsing (delaying the cloud) and could resume agents before sleep.
    if lock(&state.pro.release_pending).contains(workspace) {
        return Ok(None);
    }
    // The passive read also tells the account whether this computer could
    // take the project now (`moves::watch_query`), so a phone's action on a
    // sleeping cloud can be sent here instead of waking it.
    let path = format!(
        "{}{}",
        execution::path(config, workspace, ""),
        super::moves::watch_query(state, config, workspace)
    );
    let baton: Baton = account(config, &path, "GET", None).await?.json()?;
    ensure!(baton.workspace_id == workspace, "baton workspace mismatch");
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed during project transfer"
    );
    {
        // The account answered: from now on the verified path decides what
        // resumes here, not the unverified boot fallback.
        let mut answered = lock(&state.pro.answered);
        if answered.len() < 128 || answered.contains(workspace) {
            answered.insert(workspace.to_owned());
        }
    }
    execution::observe(state, config, &baton)?;
    if baton.continuity.is_some() {
        super::persist(state).await?;
    }
    let operation_config = if baton.holder_id.is_none() && config.role == Role::Device {
        config.clone()
    } else {
        execution::effective(state, config, workspace)?
    };
    let holder = &config.delegation.device_id;
    // The account asks this computer to take the project (the user acted on
    // it here, or a phone acted while the cloud slept): take it now, without
    // the settle wait a return has (`moves`).
    if config.role == Role::Device
        && baton.move_to.as_deref() == Some(holder.as_str())
        && baton.holder_id.as_deref() != Some(holder.as_str())
    {
        super::moves::answer(state, config, workspace, &baton);
    }
    let previous = lock(&state.pro.ownership).get(workspace).cloned();
    if baton.mirror_disabled {
        if config.role == Role::Worker {
            lock(&state.pro.ownership).insert(
                workspace.into(),
                Ownership::PrivacyDisabled { epoch: baton.epoch },
            );
            super::persist(state).await?;
            suspend_workspace(state, workspace).await?;
            anyhow::bail!("Mirroring is disabled for this project");
        }
        {
            let mut preferences = lock(&state.pro.preferences);
            let preference = preferences.entry(workspace.into()).or_default();
            preference.never_mirror = true;
            preference.privacy_pending = false;
        }
        super::persist(state).await?;
    }

    if baton.holder_id.as_deref().is_some_and(|id| id != holder) {
        // A worker never steals an active owner. Devices observe remote work
        // immediately; hand-back is a separate coordinated stop/release path.
        // A resumed machine that finds another owner does not renew: fence.
        if execution::resuming(state, workspace) {
            execution::fence_workspace(state, workspace);
        }
        lock(&state.pro.ownership).insert(
            workspace.into(),
            Ownership::Remote {
                epoch: baton.epoch,
                holder: baton.holder_id.clone().unwrap_or_default(),
            },
        );
        super::persist(state).await?;
        stop_after_verified_owner(state, config, workspace).await?;
        return Ok(None);
    }
    // A worker may sleep through an entire device tenure without ever observing
    // its remote owner. Only hydration may reacquire its saved work. Negotiated
    // checkpoint execution retains its dedicated hydration protocol below.
    if config.role == Role::Worker
        && baton.holder_id.is_none()
        && !execution::checkpoint_mode(state, workspace)
    {
        // Snapshot recovery can call reconciliation while already holding jobs.
        // Leave that operation alone; the next poll or hydrate owns the retry.
        let Ok(_job) = state.pro.jobs.try_lock() else {
            return Ok(None);
        };
        let fenced = {
            let _configuration = state.pro.configuration.lock().await;
            ensure!(
                generation == state.pro.generation.load(Ordering::Acquire),
                "Account changed during project transfer"
            );
            let mut ownership = lock(&state.pro.ownership);
            if ownership.get(workspace) == previous.as_ref()
                && !matches!(
                    previous,
                    Some(
                        Ownership::Transferring { .. }
                            | Ownership::Hydrating { .. }
                            | Ownership::SettingUp { .. }
                    )
                )
            {
                ownership.insert(
                    workspace.into(),
                    Ownership::Hydrating { epoch: baton.epoch },
                );
                true
            } else {
                false
            }
        };
        if fenced {
            super::persist(state).await?;
            ensure!(
                generation == state.pro.generation.load(Ordering::Acquire),
                "Account changed during project transfer"
            );
            suspend_workspace(state, workspace).await?;
        }
        return Ok(None);
    }
    // Released for another computer that never took it (its request was
    // withdrawn or lapsed): the work is still here, so take the released
    // epoch back below (no install, no fork) and resume what stopped.
    let previous = if config.role == Role::Device
        && super::moves::abandoned(state, &baton, previous.as_ref())
    {
        let back = Ownership::AwaitingVerification { epoch: baton.epoch };
        lock(&state.pro.ownership).insert(workspace.into(), back.clone());
        Some(back)
    } else {
        previous
    };
    // A remote release means its saved work must be hydrated first. The lease
    // loop must not race hand-back and resume this machine's older journal.
    if config.role == Role::Device
        && baton.holder_id.is_none()
        && matches!(previous, Some(Ownership::Remote { .. }))
    {
        return Ok(None);
    }
    let owned = baton.holder_id.as_deref() == Some(holder);
    let transferring = matches!(
        previous,
        Some(
            Ownership::Transferring { .. }
                | Ownership::Hydrating { .. }
                | Ownership::SettingUp { .. }
        )
    );
    if transferring && !owned {
        return Ok(None);
    }
    // Handed to the cloud when the app quit: until the app returns
    // (`/pro/wake`) this computer neither takes the project back nor renews
    // it. A handover still in flight (Transferring) keeps renewing its own
    // lease until it releases.
    if config.role == Role::Device && !transferring && super::parked(state, workspace) {
        return Ok(None);
    }

    // A cloud machine resuming from suspension renews its own recorded epoch
    // even though the lease reads expired: the account kept it as a paused
    // owner (same epoch, no fork). Acquiring instead would install the
    // checkpoint over its own newer work. Only a refused renewal fences. The
    // loop may run before the watchdog noticed the freeze.
    if config.role == Role::Worker {
        execution::thawed(state);
    }
    let resuming = config.role == Role::Worker
        && owned
        && execution::resuming(state, workspace)
        && execution::proof_epoch(state, workspace) == Some(baton.epoch);
    let operation = if resuming
        || (owned
            && baton
                .expires_at
                .as_ref()
                .is_some_and(|expiry| expiry > &baton.server_now))
    {
        "renew"
    } else {
        "acquire"
    };
    // Re-acquiring the epoch this installation itself held (its own clean
    // release, or its own lease that lapsed while it kept working) continues
    // its own newer files and conversations: no checkpoint install, no fork,
    // no second transfer pickup. That includes a cloud machine thawed from a
    // suspension whose renewal window was missed (the lease loop can run
    // before the watchdog notices the freeze): the account turns a paused
    // owner's acquire into a renewal of the same epoch. A cloud machine whose
    // own arrival was interrupted (still installing) installs it again.
    let own_epoch = execution::held_here(state, config, &baton)
        && (config.role == Role::Device || !transferring);
    if operation == "acquire" && execution::checkpoint_mode(state, workspace) && !own_epoch {
        ensure!(
            baton.checkpoint.is_some(),
            "a durable project checkpoint is not available yet"
        );
        // An expired/replaced executor must install the selected canonical
        // checkpoint, even when it is the same physical worker or home device.
        let Ok(job) = state.pro.jobs.clone().try_lock_owned() else {
            return Ok(None);
        };
        // Fenced from the moment the install is scheduled: canonical files
        // are about to replace this project's, so nothing new may start (and
        // no stale boot-deferred turn may resume) before hydrate's own fence.
        lock(&state.pro.installing).insert(workspace.to_owned());
        lock(&state.pro.ownership).insert(
            workspace.into(),
            Ownership::AwaitingVerification { epoch: baton.epoch },
        );
        if let Err(error) = super::persist(state).await {
            lock(&state.pro.installing).remove(workspace);
            return Err(error);
        }
        // Installing runs as its own owned task: lease renewal for every
        // other project continues meanwhile. The grant's own requires_fork
        // decides; hydrate applies it.
        let owner = state.clone();
        let config = config.clone();
        let key = workspace.to_owned();
        let epoch = baton.epoch;
        tokio::spawn(async move {
            super::detached::run(
                &owner.clone(),
                ("install", false),
                &key.clone(),
                epoch,
                || None,
                move || async move {
                    let _job = job;
                    let result = install_owned(owner.clone(), config, key.clone(), epoch).await;
                    lock(&owner.pro.installing).remove(&key);
                    if let Err(error) = result {
                        record_error(&owner, &key, &error);
                        return super::detached::Outcome::refused(
                            axum::http::StatusCode::CONFLICT,
                            None,
                        );
                    }
                    super::detached::Outcome::done()
                },
            )
            .await
        });
        return Ok(None);
    }
    let body = execution::body(&operation_config, baton.epoch, operation == "acquire");
    // First enrollment happens around work already running here: its agents
    // keep their processes (a stop and restart would resend a billed pickup
    // turn) and become this life's managed workload, recorded as crash
    // evidence with the state write below.
    if operation_config.execution.is_some() && baton.continuity.is_none() {
        execution::adopt_running(state, workspace);
        super::persist(state).await?;
    }
    // Never take or extend a lease this worker could not accept: an expired
    // one lets the laptop (or a clean worker) continue instead.
    ensure!(
        !execution::worker(state)
            || (!execution::uncertain(state, workspace) && !execution::unclean(state, workspace)),
        "previous managed processes are still stopping"
    );
    let was_fenced = execution::fenced(state, workspace);
    let request_start = execution::RequestStart::now();
    let response = account(
        &operation_config,
        &execution::path(&operation_config, workspace, operation),
        "POST",
        Some(&body),
    )
    .await
    .context("Could not renew project ownership")?;
    // The account's reconnect grace after a lapsed lease is a normal wait,
    // not a failure; the next tick asks again while local work continues.
    if response.status == 409
        && serde_json::from_slice::<serde_json::Value>(&response.body)
            .is_ok_and(|value| value["error"] == "takeover_grace")
    {
        return Ok(None);
    }
    // The account refused this resumed machine's own epoch (someone else
    // took the project while it slept): fence now, not at the window's end.
    if resuming && (400..500).contains(&response.status) {
        execution::fence_workspace(state, workspace);
    }
    let grant: Baton = response
        .json()
        .context("Could not confirm project ownership")?;
    ensure!(
        grant.workspace_id == workspace && grant.holder_id.as_deref() == Some(holder),
        "baton grant names another owner"
    );
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed while verifying project ownership"
    );
    ensure!(
        grant.workspace_id == workspace,
        "execution grant workspace mismatch"
    );
    execution::accept(state, &operation_config, &grant, generation, request_start)?;
    super::projects::bind_workspace_account(state, config, workspace)?;
    if operation_config.execution.is_some() {
        super::persist(state).await?;
    }
    if transferring {
        if let Some(Ownership::SettingUp { epoch }) = previous {
            ensure!(
                grant.epoch == epoch,
                "Project ownership changed; retry the handoff"
            );
        }
        return Ok(Some(grant.epoch));
    }
    // A project running here with sessions still waiting for a provider stays
    // Local; those sessions resume through `provider_gate::resume_ready`
    // after sign-in, not by re-probing every lease tick.
    if config.role == Role::Worker && !matches!(previous, Some(Ownership::Local { .. })) {
        ensure!(
            generation == state.pro.generation.load(Ordering::Acquire),
            "Account changed while verifying project ownership"
        );
        lock(&state.pro.ownership).insert(
            workspace.into(),
            Ownership::SettingUp { epoch: grant.epoch },
        );
        finish_hydration(state, workspace, grant.epoch, generation, async { Ok(()) }).await?;
        return Ok(Some(grant.epoch));
    }
    if baton.mirror_disabled
        && !matches!(
            previous,
            Some(Ownership::Remote { .. } | Ownership::Hydrating { .. })
        )
    {
        lock(&state.pro.ownership).remove(workspace);
    } else {
        lock(&state.pro.ownership)
            .insert(workspace.into(), Ownership::Local { epoch: grant.epoch });
    }
    super::persist(state).await?;
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed during project transfer"
    );
    // A renewal after a same-epoch fence resumes what the fence preserved.
    if (!matches!(previous, Some(Ownership::Local { .. })) || was_fenced)
        && execution::resume_allowed(state, workspace)
    {
        crate::ledger::resume_deferred_workspace(state, workspace).await?;
    }
    // Another computer asked for this project (its user acted there): yield
    // at the next pause, unless this computer's user acted after it asked.
    if config.role == Role::Device && matches!(previous, Some(Ownership::Local { .. })) {
        super::moves::consider(state, config, workspace, &grant);
    }
    Ok(Some(grant.epoch))
}
/// A named boxed future breaks the reconcile -> hydrate -> reconcile type cycle
/// for a spawned install.
fn install_owned(
    state: Arc<AppState>,
    config: Configure,
    workspace: String,
    epoch: u64,
) -> futures::future::BoxFuture<'static, Result<()>> {
    Box::pin(async move { hydrate(&state, &config, &workspace, epoch, false, None).await })
}

/// Upper bound on how long a device's agent may finish its turn after another
/// owner was verified. Its input is already refused (`may_write`).
const VERIFIED_OWNER_PAUSE_WAIT: u64 = 300;

/// A verified other owner fences input at once. A device's agents then stop at
/// their next safe pause (or after a bounded wait) and are preserved for the
/// return; a worker stops immediately. Plain shells are never stopped.
async fn stop_after_verified_owner(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
) -> Result<()> {
    let live = sessions(state, workspace).into_iter().any(|id| {
        state.chat.get(&id).is_some_and(|s| s.alive)
            || state.sessions.get(&id).is_some_and(|s| s.alive)
    });
    if !live {
        lock(&state.pro.remote_since).remove(workspace);
        return Ok(());
    }
    if config.role == Role::Device && !at_pause(state, workspace) {
        let since = {
            let mut waiting = lock(&state.pro.remote_since);
            if waiting.len() >= 128 && !waiting.contains_key(workspace) {
                waiting.clear();
            }
            *waiting.entry(workspace.into()).or_insert_with(super::now)
        };
        if super::now().saturating_sub(since) < VERIFIED_OWNER_PAUSE_WAIT {
            return Ok(());
        }
    }
    lock(&state.pro.remote_since).remove(workspace);
    // Stopping agents can take seconds each; the renewal loop moves on.
    let owner = state.clone();
    let key = workspace.to_owned();
    tokio::spawn(async move {
        super::detached::run(
            &owner.clone(),
            ("suspend", false),
            &key.clone(),
            0,
            || None,
            move || async move {
                if let Err(error) = suspend_workspace(&owner, &key).await {
                    record_error(&owner, &key, &error);
                    return super::detached::Outcome::refused(
                        axum::http::StatusCode::CONFLICT,
                        None,
                    );
                }
                super::detached::Outcome::done()
            },
        )
        .await
    });
    Ok(())
}

async fn suspend_workspace(state: &Arc<AppState>, workspace: &str) -> Result<()> {
    for id in sessions(state, workspace) {
        match crate::bundle::export_durable(state.clone(), &id, crate::bundle::ExportMode::Stop)
            .await
        {
            Ok(archive) => {
                let _ = tokio::fs::remove_file(archive).await;
            }
            Err(_) => {
                // Losing a verified lease must stop an agent even before its
                // first native conversation id exists. Preserve its ledger
                // identity; absence of an exportable handle cannot authorize a
                // second writer to continue running.
                park_here(state, &id).await;
            }
        }
    }
    let owner = state.clone();
    tokio::task::spawn_blocking(move || {
        let (entries, links) = crate::ledger::snapshot(&owner);
        lock(&owner.ledger).write_durable(&entries, &links)
    })
    .await??;
    Ok(())
}

/// Stops an agent that cannot be exported and keeps it here as a paused row
/// with its identity (it resumes when the project is this computer's again).
/// A terminal is never stopped.
async fn park_here(state: &Arc<AppState>, id: &str) {
    if !(state.chat.get(id).is_some() || lock(&state.agents).contains_key(id)) {
        return;
    }
    if let Some(mut entry) = crate::ledger::snapshot(state)
        .0
        .into_iter()
        .find(|entry| entry.id == id)
    {
        entry.suspended = true;
        entry.handoff = None;
        lock(&state.deferred_sessions).insert(id.to_owned(), entry);
    }
    if state.chat.get(id).is_some() {
        state.chat.kill(id);
    } else {
        let _ = state.sessions.kill(id);
    }
}

/// Running agents make a project worth handing to the cloud before sleep.
pub(super) fn live_agents(state: &AppState, workspace: &str) -> bool {
    sessions(state, workspace).into_iter().any(|id| {
        state.chat.get(&id).is_some_and(|s| s.alive)
            || state.sessions.get(&id).is_some_and(|s| s.alive)
    })
}

fn sessions(state: &AppState, workspace: &str) -> Vec<String> {
    let ids: Vec<_> = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, id)| id.as_str() == workspace)
        .map(|(id, _)| id.clone())
        .collect();
    let agents = lock(&state.agents);
    ids.into_iter()
        .filter(|id| agents.contains_key(id) || state.chat.get(id).is_some())
        .collect()
}

fn transfer_session_ids(state: &AppState, workspace: &str) -> Result<Vec<String>> {
    let ids: Vec<_> = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, id)| id.as_str() == workspace)
        .map(|(id, _)| id.clone())
        .take(65)
        .collect();
    ensure!(
        ids.len() <= 64,
        "Project transfer supports at most 64 sessions; close some sessions and try again"
    );
    Ok(ids)
}
/// A clean flush before this computer sleeps: one shared deadline, and no
/// release once the computer woke again (the project simply stays here).
#[derive(Clone, Copy)]
pub(super) struct Sleep {
    pub generation: u64,
    pub deadline: tokio::time::Instant,
    /// The app is quitting, not the computer sleeping: a flush that cannot
    /// hand over recovers at once and its work continues here, instead of
    /// waiting for a wake with its lease left to lapse.
    pub park: bool,
}
impl Sleep {
    pub fn woke(&self, state: &AppState) -> bool {
        state.pro.sleep_generation.load(Ordering::Acquire) != self.generation
    }
}
/// The account required the newer path for a legacy release (see
/// `release::UpgradeRequired`); the project retries through it by itself.
pub(super) fn upgrade_required(error: &anyhow::Error) -> bool {
    error.is::<release::UpgradeRequired>()
}
pub(super) fn failure_code(error: &anyhow::Error) -> &'static str {
    snapshot_diagnostics::category(error)
}
pub(super) async fn snapshot(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    clean: bool,
) -> Result<()> {
    snapshot_before(state, config, workspace, clean, None).await
}
pub(super) async fn sleep_flush(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    sleep: Sleep,
) -> Result<()> {
    let result = snapshot_before(state, config, workspace, true, Some(sleep)).await;
    // Woken during the flush: its publication stands, the project stays here.
    if sleep.woke(state) {
        let _configuration = state.pro.configuration.lock().await;
        let mut ownership = lock(&state.pro.ownership);
        if let Some(Ownership::Transferring { epoch }) = ownership.get(workspace).cloned() {
            ownership.insert(workspace.into(), Ownership::AwaitingVerification { epoch });
        }
    }
    result
}
async fn snapshot_before(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    clean: bool,
    sleep: Option<Sleep>,
) -> Result<()> {
    let mut phase = "ownership";
    let result = snapshot_inner(state, config, workspace, clean, sleep, &mut phase).await;
    if let Err(error) = &result {
        // Recovery may clear the transient status while resuming an idle session.
        // The fixed phase/category survives that recovery without recording data.
        tracing::warn!(
            phase,
            category = snapshot_diagnostics::category(error),
            clean,
            "Project snapshot failed"
        );
    }
    result
}

async fn snapshot_inner(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    clean: bool,
    sleep: Option<Sleep>,
    phase: &mut &'static str,
) -> Result<()> {
    let generation = state.pro.generation.load(Ordering::Acquire);
    let cache = Arc::new(state.pro.cache(workspace)?.lock_owned().await);
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed while waiting for project cache"
    );
    transport::cache_quiescent(workspace)?;
    transport::cache_scope(
        workspace,
        cache,
        snapshot_inner_scoped(state, config, workspace, clean, sleep, phase),
    )
    .await
}

async fn snapshot_inner_scoped(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    clean: bool,
    sleep: Option<Sleep>,
    phase: &mut &'static str,
) -> Result<()> {
    ensure!(
        !super::project_copy::copy_only(state, workspace),
        "local copy cannot publish execution state"
    );
    authority::config_matches(state, config, workspace)?;
    let effective = execution::effective(state, config, workspace)?;
    // The unrefined configuration, for a reconcile after the account has
    // required the newer path (`effective` below is legacy for this project).
    let requested = config;
    let config = &effective;
    ensure!(
        config.recovery || execution::lease_valid(state, workspace),
        "execution authority expired before publication"
    );
    if config.recovery {
        ensure!(
            execution::quiescent(state, workspace)
                && lock(&state.pro.preferences)
                    .get(workspace)
                    .is_some_and(|p| p.recovery_pending),
            "recovery execution has not stopped"
        );
    }
    let generation = state.pro.generation.load(Ordering::Acquire);
    let epoch = super::owned_epoch(state, workspace).context("workspace is not locally owned")?;
    let workspace = lock(&state.workspaces)
        .get(workspace)
        .context("unknown workspace")?;
    // Refuse before stopping or publishing anything. Taking a prefix could
    // release legacy ownership with an omitted agent still running locally.
    let session_ids = transfer_session_ids(state, &workspace.id)?;
    *phase = "destination";
    authority::destination(state, config, &workspace.id, Some(&workspace.root)).await?;
    *phase = "credentials";
    let grant = credentials(config, &workspace.id, Some(epoch)).await?;
    let root = state.pro.root.join(&workspace.id);
    let shadow = root.join("working-tree.git");
    *phase = "initialize";
    let interrupted = root.clone();
    tokio::task::spawn_blocking(move || mirror::clear_interrupted(&interrupted)).await??;
    mirror::initialize(&shadow).await?;
    if mirror::set_aside_damaged(&shadow).await? {
        tracing::warn!("Rebuilding a damaged outgoing project mirror from its published copy");
        mirror::initialize(&shadow).await?;
        mirror::fetch_published(&shadow, &grant).await?;
    }
    let staging = root.join(format!("stage-{}", chimaera_core::generate_token()));
    tokio::fs::create_dir_all(&staging).await?;
    let woke = || sleep.is_some_and(|sleep| sleep.woke(state));
    // Sessions this flush stopped; a wake returns them to this computer.
    let stopped_ids = std::sync::Mutex::new(Vec::<String>::new());
    let mut published = false;
    let result = async {
        let agent_ids=sessions(state,&workspace.id);
        let continuation=continuation(state,&workspace.id);
        let mut has_agents=false;
        let mut stopped=std::collections::HashMap::new();
        if clean {
            *phase = "stop_sessions";
            lock(&state.pro.ownership).insert(workspace.id.clone(),Ownership::Transferring{epoch});super::persist(state).await?;
            ensure!(transfer_session_ids(state,&workspace.id)?.iter().all(|id|session_ids.contains(id)), "Project sessions changed during transfer; try again");
            for id in &session_ids {
                // Woken mid-flush: stop no further sessions.
                if woke() { break; }
                let path = match crate::bundle::export_for_mirror(state.clone(),id,crate::bundle::ExportMode::Stop).await {
                    Ok(Some(path)) => path,
                    Ok(None) => continue,
                    Err(error) => {
                        // One conversation that cannot travel (no transcript
                        // yet, too large) never fails the project: it stays
                        // here, stopped and paused with its identity, for
                        // when the project comes back.
                        tracing::warn!(category = snapshot_diagnostics::category(&error), "A conversation stays paused here instead of moving");
                        park_here(state, id).await;
                        lock(&stopped_ids).push(id.clone());
                        continue;
                    }
                };
                lock(&stopped_ids).push(id.clone());
                let target=staging.join(format!("stopped-{id}.zip"));tokio::fs::rename(path,&target).await?;stopped.insert(id.clone(),target);
            }
        }
        *phase = "stop_execution";
        if clean && !woke() {
            execution::stop(state,std::slice::from_ref(&workspace.id)).await?;
            ensure!(transfer_session_ids(state,&workspace.id)?.iter().all(|id|session_ids.contains(id)), "Project sessions changed during transfer; try again");
        }
        *phase = "inventory";
        let paths = mirror::inventory(&workspace.root, &shadow).await?;
        let project = workspace.root.clone(); let destination = staging.join("tree");
        let budget = grant.storage_limit_bytes; let max_file = grant.max_file_bytes;
        *phase = "copy_files";
        let report = tokio::task::spawn_blocking(move || mirror::copy_tree(&project, &destination, paths, budget, max_file)).await??;
        *phase = "export_config";
        let home = state.claude_settings_path.parent().and_then(Path::parent).context("agent home unavailable")?.to_path_buf();
        let sources = config::Sources { home, claude:state.claude_settings_path.parent().unwrap().to_path_buf(), codex:state.codex_config_path.parent().context("codex home unavailable")?.to_path_buf(), workspace:workspace.root.clone() };
        let destination = staging.join("config");
        let config_report = tokio::task::spawn_blocking(move || config::export(sources, &destination, budget.saturating_sub(report.bytes))).await??;
        let mut profile = lock(&state.pro.preferences).entry(workspace.id.clone()).or_default().profile.clone();
        profile.missing_environment = config_report.missing_environment;
        let command_sessions:Vec<_>=lock(&state.session_workspaces).iter().filter(|(_,id)|*id==&workspace.id).map(|(id,_)|id.clone()).take(64).collect();
        for id in command_sessions {if let Some(marks)=state.sessions.marks(&id) {for command in marks.journal(32) {if let Some(command)=command.command.as_deref(){profile.observe_command(command);}}}}
        *phase = "archive_sessions";
        let handoff = staging.join("handoff"); tokio::fs::create_dir_all(handoff.join("bundles")).await?;
        let mut archives = Vec::new(); let mut archive_bytes = 0u64;
        for id in session_ids {
            let path = if clean {
                stopped.remove(&id)
            } else {
                // A conversation that cannot be saved right now is left out of
                // this copy (and kept running); the project's files still go.
                crate::bundle::export_for_mirror(state.clone(), &id, crate::bundle::ExportMode::Snapshot).await.unwrap_or_else(|error| {
                    tracing::warn!(category = snapshot_diagnostics::category(&error), "A conversation was left out of this project copy");
                    None
                })
            };
            let Some(path) = path else {continue;};
            let length = tokio::fs::metadata(&path).await?.len();
            // Too large for the copy: leave that conversation out (a moved
            // one stays paused here), never fail the project.
            if length > max_file || archive_bytes + length + report.bytes + config_report.bytes > budget {
                let _ = tokio::fs::remove_file(path).await;
                tracing::warn!("A conversation was too large for the project copy");
                continue;
            }
            has_agents |= agent_ids.contains(&id);
            archive_bytes = archive_bytes.saturating_add(length);
            let archive = format!("bundles/{id}.zip");
            tokio::fs::rename(path, handoff.join(&archive)).await?;
            archives.push(SessionArchive {id,archive});
        }
        // Automatic continuation requires this per-epoch eligibility on the
        // account in both protocol versions: its offline wake and the worker's
        // discovery read only these flags. Publish it while this epoch is
        // still owned and before any snapshot bytes leave, so a refusal cannot
        // strand a published checkpoint that nothing will ever continue.
        if !config.recovery {
            *phase = "update_policy";
            publish_policy(config, &workspace.id, epoch, has_agents).await?;
        }
        *phase = "capture_repository";
        let super::repository::Described { branch, origin: repository_origin, snapshot: mut repository } =
            super::repository::describe(&state.pro, &workspace.id, &workspace.root).await?;
        let mut staging_bytes = 0u64;
        if let Some(repository) = repository.as_mut() {
            let remaining = budget.saturating_sub(report.bytes).saturating_sub(config_report.bytes).saturating_sub(archive_bytes);
            let (descriptor, bytes) = super::repository::staging::capture(&workspace.root, &handoff, remaining, max_file).await?;
            staging_bytes = bytes;
            repository.staging = Some(descriptor);
        }
        let manifest = Manifest {version:1,project:super::projects::catalog::metadata(&workspace.name,!workspace.cloud_internal && !workspace.hidden),branch,repository_origin,repository,workspace_id:workspace.id.clone(),root:workspace.root.clone(),name:workspace.name.clone(),epoch,clean,continuation,profile:profile.clone(),sessions:archives,left_out:report.left_out.clone()};
        let manifest_bytes = serde_json::to_vec(&manifest)?;
        ensure!(manifest_bytes.len() <= 256*1024, "handoff manifest exceeds limit");
        let used = report.bytes.checked_add(config_report.bytes).and_then(|bytes|bytes.checked_add(archive_bytes)).and_then(|bytes|bytes.checked_add(staging_bytes)).and_then(|bytes|bytes.checked_add(manifest_bytes.len() as u64)).context("snapshot size overflow")?;
        ensure!(used <= budget, "snapshot exceeds combined storage limit");
        tokio::fs::write(handoff.join("manifest.json"), manifest_bytes).await?;
        *phase = "mirror_repository";
        mirror::mirror_repository(&workspace.root, &root.join("repository.git"), &grant).await?;
        *phase = "commit_files";
        let tree_oid=mirror::commit_tree(&shadow, &staging.join("tree"), "main").await?;
        *phase = "commit_config";
        let config_oid=mirror::commit_tree(&shadow, &staging.join("config"), "config").await?;
        *phase = "commit_sessions";
        let handoff_oid=mirror::commit_tree(&shadow, &handoff, "handoff").await?;
        *phase = "publish_snapshot";
        mirror::push(&shadow, &grant, &["refs/heads/main", "refs/heads/config", "refs/heads/handoff"]).await?;
        *phase = "confirm_checkpoint";
        if config.execution.is_some(){execution::receipt::published(config,&workspace.id,epoch,[&tree_oid,&config_oid,&handoff_oid],continuation).await?;}
        {
            // The three-way baseline for a later return is what was actually
            // published and acknowledged, never a local commit whose push failed.
            let mut preferences = lock(&state.pro.preferences);
            let preference = preferences.entry(workspace.id.clone()).or_default();
            preference.profile = profile;
            preference.published_tree = Some(tree_oid.clone());
            if config.execution.is_some() { preference.published_handoff = Some(handoff_oid.clone()); }
        }
        {
            // The last return's report stays until the next return replaces it.
            let mut statuses = lock(&state.pro.status);
            let previous = statuses.remove(&workspace.id).unwrap_or_default();
            statuses.insert(workspace.id.clone(), WorkspaceStatus {report,last_mirrored_at:Some(super::now()),storage_limit_bytes:budget,error:None,error_code:None,git_staging:previous.git_staging,kept_both:previous.kept_both,kept_paths:previous.kept_paths,kept_at:previous.kept_at,kept_total:previous.kept_total,blocked_providers:Vec::new()});
        }
        *phase = "persist_snapshot";
        super::persist(state).await?;
        published = true;
        if clean && !woke() {
            *phase = "release";
            // Before sleep, the account's short publication fence is not
            // waited out past the deadline: an unreleased lease simply lapses
            // and the cloud continues from this acknowledged checkpoint.
            let budget = sleep.map_or(Duration::from_secs(15), |sleep| {
                sleep.deadline.saturating_duration_since(tokio::time::Instant::now())
            });
            release::after_publication(config, &workspace.id, epoch, budget, || {
                generation == state.pro.generation.load(Ordering::Acquire)
                    && !woke()
                    && matches!(lock(&state.pro.ownership).get(&workspace.id), Some(Ownership::Transferring { epoch: current }) if *current == epoch)
            }).await?;
        }
        Ok::<_,anyhow::Error>(())
    }.await;
    let _ = tokio::fs::remove_dir_all(staging).await;
    // Recognised before any recovery below, so every path that follows (a wake,
    // a sleep window, the reconcile) already sees the project as enrolled.
    let must_upgrade = result.as_ref().err().is_some_and(upgrade_required);
    if must_upgrade {
        execution::require_v2(state, &workspace.id);
    }
    let stopped_ids = stopped_ids.into_inner().unwrap_or_default();
    if clean && woke() {
        // The computer woke during this flush: whatever publication did, the
        // project stays here and the sessions it stopped continue locally,
        // without waiting for the account.
        if !stopped_ids.is_empty() {
            if let Err(error) =
                crate::ledger::resume_deferred_sessions(state, &workspace.id, &stopped_ids).await
            {
                tracing::warn!(%error, "Could not resume sessions after waking");
            }
        }
        return result;
    }
    if clean && !config.recovery && sleep.is_some_and(|sleep| !sleep.park || published) {
        // Inside the sleep window a flush never renews or resumes (that would
        // restart agents seconds before sleep and keep the lease from lapsing):
        // published but unreleased, the cloud continues once the lease lapses;
        // failed, the project waits. Either way the next wake returns it here.
        // A quit handover whose copy is published is the same once the account
        // cannot confirm the release in time: the user chose the cloud, so the
        // project stays parked, its lease lapses, and the cloud continues from
        // this acknowledged copy within a couple of minutes, as after a lost
        // connection, instead of the work quietly staying on a computer whose
        // app is gone.
        if result.is_err()
            && matches!(lock(&state.pro.ownership).get(&workspace.id), Some(Ownership::Transferring { epoch: current }) if *current == epoch)
        {
            lock(&state.pro.release_pending).insert(workspace.id.clone());
            if published {
                tracing::info!("Project published before sleep; its release will lapse");
                return Ok(());
            }
        }
        return result;
    }
    if result.is_err() && clean && !config.recovery {
        // A quit handover whose copy never published is not parked: the
        // renewal below must be allowed to keep the work here, and the app
        // says so.
        if sleep.is_some_and(|sleep| sleep.park) {
            super::unpark(state, &workspace.id);
        }
        // A failed flush must not strand a stopped laptop agent, but a changed
        // account must never recover using the previous account's credentials.
        let recover = {
            let _configuration = state.pro.configuration.lock().await;
            let mut ownership = lock(&state.pro.ownership);
            if generation == state.pro.generation.load(Ordering::Acquire)
                && matches!(ownership.get(&workspace.id), Some(Ownership::Transferring { epoch: current }) if *current == epoch)
            {
                ownership.insert(
                    workspace.id.clone(),
                    Ownership::AwaitingVerification { epoch },
                );
                true
            } else {
                false
            }
        };
        if recover {
            // A legacy configuration would read the project over the path the
            // account just refused and be denied as a downgrade.
            let config = if must_upgrade { requested } else { config };
            let _ = reconcile_generation(state, config, &workspace.id, generation).await;
        }
    }
    result
}

/// The account's policy route is the same `/v1` resource for both protocol
/// versions; it checks the exact live holder and epoch itself.
async fn publish_policy(
    config: &Configure,
    workspace: &str,
    epoch: u64,
    has_agents: bool,
) -> Result<()> {
    let continuation = !config.hours_exhausted;
    let policy = account(
        config,
        &format!("/v1/baton/{workspace}/policy"),
        "PUT",
        Some(&json!({
            "holder_id": config.delegation.device_id,
            "epoch": epoch,
            "handoff_enabled": continuation,
            "offline_takeover": continuation,
            "has_agents": has_agents,
        })),
    )
    .await?;
    ensure!(
        (200..300).contains(&policy.status),
        "mirror policy update failed"
    );
    Ok(())
}

pub(super) async fn fetch_snapshot(
    config: &Configure,
    workspace: &str,
    cache: &Path,
) -> Result<Manifest> {
    authority::config_workspace(config, workspace)?;
    let mut effective = config.clone();
    let receipt = if config.execution.is_some() {
        let baton: Baton = account(config, &execution::path(config, workspace, ""), "GET", None)
            .await?
            .json()?;
        ensure!(
            baton.workspace_id == workspace,
            "checkpoint workspace mismatch"
        );
        if baton.continuity.is_some() {
            Some(baton.checkpoint.context("durable checkpoint required")?)
        } else {
            effective.execution = None;
            None
        }
    } else {
        None
    };
    fetch_snapshot_at(&effective, workspace, cache, receipt.as_ref()).await
}
pub(super) async fn fetch_snapshot_at(
    config: &Configure,
    workspace: &str,
    cache: &Path,
    receipt: Option<&execution::wire::Checkpoint>,
) -> Result<Manifest> {
    if let Some(receipt) = receipt {
        execution::receipt::validate(receipt)?;
    }
    let grant = credentials(config, workspace, None).await?;
    mirror::initialize(cache).await?;
    let url = transport::endpoint(&grant.working_tree_url)?;
    transport::git_output(
        transport::git(cache, Some((&grant.username, &grant.password))).await?,
        &["fetch", "--no-tags", &url, "+refs/heads/*:refs/heads/*"],
        vec![],
    )
    .await?;
    if let Some(receipt) = receipt {
        transport::git_output(
            transport::git(cache, Some((&grant.username, &grant.password))).await?,
            &[
                "fetch",
                "--no-tags",
                &url,
                &format!(
                    "+refs/chimaera/checkpoints/{}/*:refs/chimaera/checkpoints/{}/*",
                    receipt.id, receipt.id
                ),
            ],
            vec![],
        )
        .await?;
        execution::receipt::pin(cache, receipt).await?;
    }
    let bytes = transport::git_output(
        transport::git(cache, None).await?,
        &[
            "show",
            &format!(
                "{}:manifest.json",
                execution::receipt::revision(receipt, "handoff")?
            ),
        ],
        vec![],
    )
    .await?;
    ensure!(bytes.len() <= 256 * 1024, "handoff manifest exceeds limit");
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest.version == 1
            && manifest.workspace_id == workspace
            && manifest.sessions.len() <= 64
            && manifest.root.is_absolute(),
        "invalid handoff manifest"
    );
    if let Some(receipt) = receipt {
        ensure!(
            manifest.epoch == receipt.source_epoch && manifest.continuation == receipt.continuation,
            "handoff receipt does not match manifest"
        );
    }
    for entry in &manifest.sessions {
        ensure!(
            super::valid_id(&entry.id) && entry.archive == format!("bundles/{}.zip", entry.id),
            "invalid handoff archive path"
        );
    }
    Ok(manifest)
}

pub(super) async fn hydrate(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    expected_epoch: u64,
    fork: bool,
    destination_root: Option<&Path>,
) -> Result<()> {
    ensure!(
        !super::project_copy::copy_only(state, workspace)
            || lock(&state.pro.preferences)
                .get(workspace)
                .and_then(|p| p.copy.as_ref())
                .is_some_and(|copy| copy.takeover_requested),
        "Explicit Take over is required for a local copy"
    );
    let generation = state.pro.generation.load(Ordering::Acquire);
    let bound_destination =
        authority::destination(state, config, workspace, destination_root).await?;
    let destination_root = bound_destination.as_deref();
    let cache = Arc::new(state.pro.cache(workspace)?.lock_owned().await);
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed while waiting for project cache"
    );
    transport::cache_quiescent(workspace)?;
    let result = transport::cache_scope(
        workspace,
        cache.clone(),
        hydrate_scoped(
            state,
            config,
            workspace,
            expected_epoch,
            fork,
            destination_root,
            cache,
        ),
    )
    .await;
    if let Err(error) = &result {
        // Keep worker failures diagnosable without logging helper stderr,
        // credentials, project paths or native transcript contents.
        let phase = if error
            .chain()
            .any(|cause| cause.to_string() == "repository return preparation failed")
        {
            "repository_prepare"
        } else {
            "hydrate"
        };
        tracing::warn!(
            phase,
            category = super::routes::error_code(error),
            epoch = expected_epoch,
            "project hydration failed; installation remains fenced"
        );
    }
    result
}

type ReturnReport = (Vec<String>, (usize, Vec<PathBuf>));

async fn hydrate_scoped(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    expected_epoch: u64,
    fork: bool,
    destination_root: Option<&Path>,
    mut cache_guard: Arc<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<()> {
    let generation = state.pro.generation.load(Ordering::Acquire);
    let install_epoch = std::sync::atomic::AtomicU64::new(0);
    let current = || -> Result<()> {
        let epoch = install_epoch.load(Ordering::Acquire);
        ensure!(
            epoch == 0 || execution::valid_grant(state, workspace, epoch),
            "execution authority expired during project transfer"
        );
        ensure!(
            generation == state.pro.generation.load(Ordering::Acquire),
            "Account changed during project transfer; open the project again"
        );
        Ok(())
    };
    if !super::project_copy::copy_only(state, workspace)
        && lock(&state.workspaces).get(workspace).is_some()
        && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::SettingUp{epoch}) if *epoch==expected_epoch)
    {
        reconcile(state, config, workspace).await?;
        let path = state.pro.root.join(workspace).join("return-install");
        let endpoint = config.endpoint.clone();
        let account = config.account_id.clone();
        let workspace_id = workspace.to_owned();
        tokio::task::spawn_blocking(move || {
            super::install::Transaction::cleanup_committed(
                &path,
                &endpoint,
                account.as_deref(),
                &workspace_id,
                expected_epoch,
            )
        })
        .await??;
        let _ =
            tokio::fs::remove_dir_all(state.pro.root.join(workspace).join("return-stage")).await;
        finish_hydration(
            state,
            workspace,
            expected_epoch,
            generation,
            run_profile_steps(state, config, workspace),
        )
        .await?;
        return Ok(());
    }
    // Existing durable worker work must never be replaced with an older remote
    // snapshot after a restart. The normal grant path resumes its own ledger.
    // A managed project whose current epoch this machine verifiably holds is
    // the same no-op (its running agents are not stopped, nothing is
    // reinstalled); one with uncertain or unproven old processes still takes
    // the checkpoint.
    if lock(&state.workspaces).get(workspace).is_some()
        && config.role == Role::Worker
        && (!execution::managed(state, workspace)
            || (!execution::uncertain(state, workspace) && !execution::unclean(state, workspace)))
        && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::Local{epoch}|Ownership::AwaitingVerification{epoch}) if *epoch==expected_epoch)
    {
        let baton: Baton = account(config, &execution::path(config, workspace, ""), "GET", None)
            .await?
            .json()?;
        ensure!(baton.workspace_id == workspace, "baton workspace mismatch");
        current()?;
        if baton.holder_id.as_deref() == Some(&config.delegation.device_id)
            && baton.epoch == expected_epoch
        {
            let verified = reconcile_generation(state, config, workspace, generation).await?;
            if verified == Some(expected_epoch)
                && super::owned_epoch(state, workspace) == Some(expected_epoch)
            {
                return Ok(());
            }
        }
    }
    if execution::managed(state, workspace) {
        // No canonical files are installed while an old local managed executor
        // can still write them. An unclean same-boot registry remains blocked.
        execution::fence_workspace(state, workspace);
        execution::stop(state, &[workspace.to_owned()]).await?;
    }
    let interrupted = state.pro.root.join(workspace);
    tokio::task::spawn_blocking(move || mirror::clear_interrupted(&interrupted)).await??;
    let cache = state.pro.root.join(workspace).join("incoming.git");
    let manifest = fetch_snapshot(config, workspace, &cache).await?;
    let destination_root = destination_root
        .map(Path::to_path_buf)
        .or_else(|| {
            lock(&state.workspaces)
                .get(workspace)
                .map(|workspace| workspace.root)
        })
        .unwrap_or_else(|| manifest.root.clone());
    ensure!(
        destination_root.is_absolute(),
        "destination root must be absolute"
    );
    ensure!(
        tokio::fs::metadata(&destination_root)
            .await
            .is_ok_and(|metadata| metadata.is_dir()),
        "root_setup_required"
    );
    let probe = destination_root.join(format!(
        ".chimaera-write-probe-{}",
        chimaera_core::generate_token()
    ));
    tokio::fs::write(&probe, b"")
        .await
        .context("root_setup_required")?;
    tokio::fs::remove_file(&probe).await?;
    current()?;
    let existing: Baton = account(config, &execution::path(config, workspace, ""), "GET", None)
        .await?
        .json()?;
    execution::observe(state, config, &existing)?;
    let effective = execution::effective(state, config, workspace)?;
    let config = &effective;
    let request_start = execution::RequestStart::now();
    let grant: Baton = if existing.holder_id.as_deref() == Some(&config.delegation.device_id)
        && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::Hydrating{epoch}) if *epoch==existing.epoch)
    {
        account(
            config,
            &execution::path(config, workspace, "renew"),
            "POST",
            Some(&execution::body(config, existing.epoch, false)),
        )
        .await?
        .json()?
    } else {
        account(
            config,
            &execution::path(config, workspace, "acquire"),
            "POST",
            Some(&execution::body(config, expected_epoch, true)),
        )
        .await?
        .json()?
    };
    ensure!(
        grant.workspace_id == workspace
            && grant.holder_id.as_deref() == Some(&config.delegation.device_id),
        "invalid handoff ownership grant"
    );
    current()?;
    execution::accept(state, config, &grant, generation, request_start)?;
    // A project taken here (a return, an adoption, a reopened folder) is this
    // device's from now on: bind it to the account at once, as reconcile does
    // after its own acquire, so the cloud-project listing can match the folder
    // immediately instead of after the next renewal.
    super::projects::bind_workspace_account(state, config, workspace)?;
    install_epoch.store(grant.epoch, Ordering::Release);
    if config.role == Role::Device {
        let mut returned = lock(&state.pro.returned);
        if returned.len() < 128 || returned.contains(workspace) {
            returned.insert(workspace.to_owned());
        }
    }
    let receipt = if config.execution.is_some() {
        let receipt = grant
            .checkpoint
            .as_ref()
            .context("acquired grant has no durable checkpoint")?;
        ensure!(
            receipt.source_epoch <= grant.epoch,
            "checkpoint is newer than its execution grant"
        );
        Some(receipt)
    } else {
        None
    };
    lock(&state.pro.ownership).insert(
        workspace.into(),
        Ownership::Hydrating { epoch: grant.epoch },
    );
    super::persist(state).await?;
    let manifest = if let Some(receipt) = receipt {
        fetch_snapshot_at(config, workspace, &cache, Some(receipt)).await?
    } else {
        manifest
    };
    // This stage is a durable part of the installation journal. The generic
    // interrupted-helper sweep deliberately does not remove return-stage.
    let stage = state.pro.root.join(workspace).join("return-stage");
    let transaction_root = state.pro.root.join(workspace).join("return-install");
    let checkpoint_binding = if let Some(receipt) = receipt {
        receipt.id.clone()
    } else {
        String::from_utf8(
            transport::git_output(
                transport::git(&cache, None).await?,
                &[
                    "rev-parse",
                    "refs/heads/main",
                    "refs/heads/config",
                    "refs/heads/handoff",
                ],
                vec![],
            )
            .await?,
        )?
    };
    let binding = super::install::Binding {
        endpoint: config.endpoint.clone(),
        account: config.account_id.clone(),
        workspace: workspace.into(),
        epoch: grant.epoch,
        receipt: Some(checkpoint_binding),
    };
    let (journal_path, journal_binding) = (transaction_root.clone(), binding.clone());
    let mut transaction = tokio::task::spawn_blocking(move || {
        super::install::Transaction::open(&journal_path, &journal_binding)
    })
    .await??;
    let recovering = transaction.is_some();
    let result = async {
        let read_grant = credentials(config, workspace, None).await?;
        let local_shadow = state.pro.root.join(workspace).join("working-tree.git");
        let mut planned = Vec::new();
        let (git_branches,kept,git_staging) = if !recovering {
            if tokio::fs::try_exists(&stage).await? { tokio::fs::remove_dir_all(&stage).await?; }
            let private_stage=stage.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                use std::os::unix::fs::PermissionsExt;
                std::fs::create_dir_all(&private_stage)?;
                std::fs::set_permissions(&private_stage,std::fs::Permissions::from_mode(0o700))?;
                Ok(())
            }).await??;
            let transfer_budget = read_grant.storage_limit_bytes.min(1024*1024*1024);
            let mut transfer_bytes = 0u64;
            for branch in ["main", "config", "handoff"] {
                let bytes = mirror::validate_tree_bytes(&cache,execution::receipt::revision(receipt, branch)?,transfer_budget,read_grant.max_file_bytes).await?;
                transfer_bytes = transfer_bytes.checked_add(bytes).context("snapshot size overflow")?;
                ensure!(transfer_bytes <= transfer_budget, "snapshot exceeds combined storage limit");
            }
            for (branch, folder) in [("main", "tree"),("config", "config"),("handoff", "handoff")] {
                let destination = stage.join(folder);
                tokio::fs::create_dir_all(&destination).await?;
                let mut command = transport::git(&cache, None).await?;
                command.env("GIT_WORK_TREE", &destination);
                transport::git_output(command,&["--work-tree",destination.to_str().context("invalid stage path")?,"checkout",execution::receipt::revision(receipt, branch)?,"--","."],vec![]).await?;
            }
            current()?;
            authority::destination(state,config,workspace,Some(&destination_root)).await?;
            super::projects::begin_install(state,workspace,&destination_root).await?;
            let original = destination_root.clone();
            let before = stage.join("tree-before");
            let checkout = stage.join("checkout");
            let budget = read_grant.storage_limit_bytes;
            let (before_copy,checkout_copy)=(before.clone(),checkout.clone());
            tokio::task::spawn_blocking(move || -> Result<()> {
                super::install::snapshot(&original,&before_copy,&|path| super::policy::allowed_path(path) || path.file_name().and_then(|name|name.to_str()).is_some_and(super::canonical::kept_copy_name),budget)?;
                super::install::snapshot(&before_copy,&checkout_copy,&|_|true,budget)
            }).await??;
            let (acknowledged, copy_checkpoint) = {
                let preferences = lock(&state.pro.preferences);
                let preference = preferences.get(workspace);
                (preference.and_then(|p| p.published_handoff.clone()), preference.and_then(|p| p.copy.as_ref()).and_then(|copy| copy.checkpoint.clone()))
            };
            let copied_baseline = if let Some(checkpoint) = &copy_checkpoint {
                let copy_cache = state.pro.root.join(workspace).join("copy-incoming.git");
                Some(takeover_copy_baseline(&copy_cache, &stage, checkpoint, budget).await?)
            } else { None };
            let staging_baseline = if let Some(copied) = copied_baseline {
                copied
            } else {
                staging_baseline(&local_shadow, &stage.join("baseline-handoff"), acknowledged, budget).await?
            };
            let super::repository::Prepared {branches:git_branches,writes:git_writes,staging:git_staging} = super::repository::prepare_receive(
                &destination_root,&checkout,&stage.join("repository"),
                super::repository::Incoming {cache:&state.pro.root.join(workspace).join("incoming-repository.git"),credentials:&read_grant,branch:manifest.branch.as_deref(),origin:manifest.repository_origin.as_deref(),snapshot:manifest.repository.as_ref(),staging:Some(super::repository::StagingIncoming {handoff:&stage.join("handoff"),baseline:staging_baseline.as_ref().map(|(path,descriptor)|(path.as_path(),descriptor))})},&current,
            ).await.context("repository return preparation failed")?;
            planned.extend(git_writes);
        let baseline = stage.join("baseline");
        let old_shadow = if copy_checkpoint.is_none() { super::shadow_cache::baseline(&local_shadow).await? } else { None };
        let has_baseline = copy_checkpoint.is_some() || old_shadow.is_some();
        if let Some(old_shadow) = &old_shadow {
            tokio::fs::create_dir_all(&baseline).await?;
            // The last snapshot this computer published successfully. A local
            // commit whose push failed is newer than anything the other side
            // saw; using it would silently overwrite edits made since.
            let published = lock(&state.pro.preferences)
                .get(workspace)
                .and_then(|p| p.published_tree.clone());
            let revision = baseline_revision(old_shadow, published).await?;
            let mut command = transport::git(old_shadow, None).await?;
            command.env("GIT_WORK_TREE", &baseline);
            transport::git_output(
                command,
                &[
                    "--work-tree",
                    baseline.to_str().context("invalid baseline path")?,
                    "checkout",
                    &revision,
                    "--",
                    ".",
                ],
                vec![],
            )
            .await?;
        }
        current()?;
        authority::destination(state, config, workspace, Some(&destination_root)).await?;
        let tree = stage.join("tree");
        let destination = checkout.clone();
        let left_out = manifest.left_out.clone();
        let kept = tokio::task::spawn_blocking(move || {
            install_tree(
                &tree,
                &destination,
                has_baseline.then_some(baseline).as_deref(),
                left_out.as_deref(),
            )
        })
        .await??;
        let (root,before_copy,checkout_copy)=(destination_root.clone(),before.clone(),checkout.clone());
        planned.extend(tokio::task::spawn_blocking(move || -> Result<_> {
            let mut writes = super::install::changes(&root,&before_copy,&checkout_copy)?;
            writes.retain(|write| !write.relative.starts_with(".git"));
            Ok(writes)
        }).await??);
        let home = state.claude_settings_path.parent().and_then(Path::parent).context("agent home unavailable")?.to_path_buf();
        let (overlay,config_workspace,config_stage)=(stage.join("config"),destination_root.clone(),stage.join("configuration"));
        let budget = read_grant.storage_limit_bytes;
        planned.extend(tokio::task::spawn_blocking(move ||config::prepare_import(&overlay,&home,&config_workspace,&config_stage,budget)).await??);
        let (marker_root,marker_checkout,marker_stage,marker_id)=(destination_root.clone(),checkout.clone(),stage.join("marker"),workspace.to_owned());
        planned.push(tokio::task::spawn_blocking(move ||prepare_marker(&marker_root,&marker_checkout,&marker_stage,&marker_id)).await??);
        let kept = super::project_copy::carry_kept(state,workspace,kept);
        let report = serde_json::to_vec(&(git_branches.clone(),kept.clone()))?;
        let report_path = stage.join("report.json");
        tokio::task::spawn_blocking(move ||crate::persist::atomic_write_json_durable(&report_path,report)).await??;
        let report_path = stage.join("staging-report.json");
        let report = serde_json::to_vec(&git_staging)?;
        tokio::task::spawn_blocking(move ||crate::persist::atomic_write_json_durable(&report_path,report)).await??;
        (git_branches,kept,git_staging)
        } else {
            let report_path = stage.join("report.json");
            let (branches,kept) = tokio::task::spawn_blocking(move || -> Result<ReturnReport> {
                let (file,meta)=crate::fs::open_regular(&report_path)?;
                ensure!(meta.len() <= 64*1024,"return report exceeds limit");
                Ok(serde_json::from_reader(file)?)
            }).await??;
            let report_path = stage.join("staging-report.json");
            let staging = tokio::task::spawn_blocking(move || -> Result<_> {
                match crate::fs::open_regular(&report_path) {
                    Ok((file,meta)) => { ensure!(meta.len() <= 32*1024,"staging return report exceeds limit"); Ok(serde_json::from_reader(file)?) },
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(super::repository::StagingStatus::Uncaptured),
                    Err(error) => Err(error.into()),
                }
            }).await??;
            (branches,kept,staging)
        };
        // All archives are validated before the first target mutation. Recovery
        // requires their original immutable metadata; it never recaptures a
        // partially installed journal as its own before-image.
        let mut sessions = Vec::new();
        for archive in &manifest.sessions {
            current()?;
            let session_stage = stage.join("sessions").join(&archive.id);
            if recovering {
                ensure!(tokio::fs::try_exists(session_stage.join("metadata.json")).await?,"session preparation missing; return recovery retained");
            }
            let mut prepared = crate::bundle::prepare_import(state.clone(),&stage.join("handoff").join(&archive.archive),crate::bundle::ImportOptions {
                defer_start:true,destination_root:Some(destination_root.clone()),fork:fork||grant.requires_fork,
                origin:if config.role == Role::Worker {crate::bundle::Origin::Moved} else {crate::bundle::Origin::Home},epoch:grant.epoch,
            },&session_stage).await?;
            if !recovering { planned.append(&mut prepared.writes); }
            sessions.push(prepared);
        }
        if transaction.is_none() {
            let budget_stage=stage.clone();
            tokio::task::spawn_blocking(move ||super::install::stage_budget(&budget_stage)).await??;
            let (root,binding,budget)=(transaction_root.clone(),binding.clone(),read_grant.storage_limit_bytes);
            transaction=Some(tokio::task::spawn_blocking(move ||super::install::Transaction::prepare(&root,binding,planned,budget)).await??);
        }
        current()?;
        authority::destination(state,config,workspace,Some(&destination_root)).await?;
        let mut install = transaction.take().context("return installation unavailable")?;
        let git_roots = super::repository::install_roots(&destination_root).await?;
        let guard_state=state.clone();
        let epoch=grant.epoch;
        let file_guard=super::mutation::begin_import(state,workspace,epoch,generation).await?;
        install=tokio::task::spawn_blocking(move || -> Result<_> {
            let admitted=|| file_guard.check(&guard_state);
            install.reserve_git(git_roots,&admitted)?;
            install.apply(&admitted)?;
            drop(file_guard);
            Ok(install)
        }).await??;
        current()?;
        let new_workspace = crate::workspaces::Workspace {
            id:workspace.into(),root:destination_root.clone(),name:manifest.name.clone(),last_opened_at:super::now(),
            mastermind:None,plugins_on:Vec::new(),cloud_internal:false,hidden:false,
        };
        let owner = state.clone();
        let registration=super::mutation::begin_import(state,workspace,epoch,generation).await?;
        tokio::task::spawn_blocking(move || -> Result<()> {registration.check(&owner)?;lock(&owner.workspaces).import_exact(new_workspace)?;drop(registration);Ok(())}).await??;
        for prepared in sessions { current()?; prepared.finalize().await?; }
        if let Some(repair) = super::shadow_cache::prepare(&local_shadow, &cache, cache_guard.clone()).await? {
            let configuration = state.pro.configuration.clone().lock_owned().await;
            let owner = state.clone();
            let workspace = workspace.to_owned();
            let epoch = grant.epoch;
            cache_guard = super::shadow_cache::install(repair, cache_guard, configuration, move || {
                transport::cache_quiescent(&workspace)?;
                ensure!(generation == owner.pro.generation.load(Ordering::Acquire), "Account changed during shadow recovery");
                ensure!(execution::valid_grant(&owner, &workspace, epoch), "Execution authority expired during shadow recovery");
                ensure!(matches!(lock(&owner.pro.ownership).get(&workspace), Some(Ownership::Hydrating { epoch: current }) if *current == epoch), "Workspace ownership changed during shadow recovery");
                Ok(())
            }).await?;
        }
        // Both sides now share the installed tree: it is the baseline for the
        // next return until this computer publishes again.
        mirror::initialize(&local_shadow).await?;
        let published_tree = {
            let source = cache.to_str().context("invalid cache path")?.to_owned();
            let main = execution::receipt::revision(receipt, "main")?;
            let handoff = execution::receipt::revision(receipt, "handoff")?;
            transport::git_output(
                transport::git(&local_shadow, None).await?,
                &["fetch", "--no-tags", &source, &format!("+{main}:refs/chimaera/baseline"), &format!("+{handoff}:refs/chimaera/baseline-handoff")],
                vec![],
            )
            .await?;
            let installed = transport::git_output(
                transport::git(&local_shadow, None).await?,
                &["rev-parse", "--verify", "refs/chimaera/baseline^{commit}"],
                vec![],
            )
            .await?;
            Some(String::from_utf8(installed)?.trim().to_owned())
        };
        current()?;
        let execution_uncertain = grant.requires_fork
            || receipt.is_some_and(|receipt|receipt.continuation==execution::wire::Continuation::Uncertain);
        let profile=manifest.profile;
        let received_handoff = receipt.map(|receipt|receipt.handoff_oid.clone());
        // Installation commits before profile commands can have external effects
        // and before any deferred agent is eligible to start.
        let commit_guard=super::mutation::begin_import(state,workspace,epoch,generation).await?;
        let commit_state=state.clone();
        let commit_workspace=workspace.to_owned();
        // Keep admission through the durable ownership transition, even if the
        // requesting browser disconnects after the blocking commit starts.
        install=tokio::spawn(async move {
            commit_guard.check(&commit_state)?;
            super::report_return(&commit_state,&commit_workspace,kept,&git_branches);
            {
                let mut preferences=lock(&commit_state.pro.preferences);
                let preference=preferences.entry(commit_workspace.clone()).or_default();
                preference.execution_uncertain=execution_uncertain;
                preference.profile=profile;
                preference.git_branches=git_branches;
            }
            super::persist(&commit_state).await?;
            let worker_state=commit_state.clone();
            let (install,commit_guard)=tokio::task::spawn_blocking(move || -> Result<_> {
                install.commit(&||commit_guard.check(&worker_state))?;
                Ok((install,commit_guard))
            }).await??;
            {
                let mut preferences=lock(&commit_state.pro.preferences);
                let preference=preferences.entry(commit_workspace.clone()).or_default();
                if let Some(tree)=published_tree { preference.published_tree=Some(tree); }
                if let Some(handoff)=received_handoff { preference.published_handoff=Some(handoff); }
                preference.git_staging=Some(git_staging.clone());
            }
            lock(&commit_state.pro.status).entry(commit_workspace.clone()).or_default().git_staging=Some(git_staging);
            super::project_copy::promote(&commit_state,&commit_workspace,&commit_guard).await?;
            super::persist(&commit_state).await?;
            drop(commit_guard);
            Ok::<_,anyhow::Error>(install)
        }).await??;
        tokio::task::spawn_blocking(move ||install.cleanup()).await??;
        finish_hydration(
            state,
            workspace,
            grant.epoch,
            generation,
            run_profile_steps(state, config, workspace),
        )
        .await?;
        drop(cache_guard);
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_ok() {
        let _ = tokio::fs::remove_dir_all(stage).await;
    }
    result
}
pub(super) fn prepare_marker(
    root: &Path,
    checkout: &Path,
    stage: &Path,
    workspace: &str,
) -> Result<super::install::Write> {
    use std::io::Write;
    std::fs::create_dir_all(stage)?;
    let target = if checkout.join(".git").is_dir() && !root.join(".git").exists() {
        root.join(".git/chimaera-workspace")
    } else {
        crate::workspaces::identity::marker_path(root)
    };
    let (target_root, relative) = if target.starts_with(root) {
        (root.to_path_buf(), target.strip_prefix(root)?.to_path_buf())
    } else {
        (
            std::fs::canonicalize(target.parent().context("invalid project marker")?)?,
            target.file_name().context("invalid project marker")?.into(),
        )
    };
    let before = stage.join("before");
    let before = if target.try_exists()? {
        let parent = target.parent().context("invalid project marker")?;
        let parent = std::fs::canonicalize(parent)?;
        let before_dir = stage.join("original");
        let name = target.file_name().context("invalid project marker")?;
        super::install::snapshot(&parent, &before_dir, &|path| path == Path::new(name), 4096)?;
        let copied = before_dir.join(name);
        ensure!(copied.is_file(), "project marker is not a regular file");
        std::fs::rename(copied, &before)?;
        Some(before)
    } else {
        None
    };
    let after = stage.join("after");
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&after)?;
    use std::os::unix::fs::PermissionsExt;
    output.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    output.write_all(&serde_json::to_vec(&crate::workspaces::identity::Marker {
        id: workspace.into(),
        written_at: super::now(),
    })?)?;
    output.sync_all()?;
    Ok(super::install::Write {
        root: target_root,
        relative,
        before,
        after: Some(after),
    })
}
/// A completed copy has its own immutable receipt baseline. Takeover must
/// preserve edits against that copy, never a newer cloud tip or local shadow.
async fn takeover_copy_baseline(
    cache: &Path,
    stage: &Path,
    checkpoint: &execution::wire::Checkpoint,
    budget: u64,
) -> Result<Option<(PathBuf, super::repository::staging::Descriptor)>> {
    execution::receipt::validate(checkpoint)?;
    let mut bytes = 0_u64;
    for revision in [&checkpoint.working_tree_oid, &checkpoint.handoff_oid] {
        bytes = bytes
            .checked_add(
                mirror::validate_tree_bytes(
                    cache,
                    revision,
                    budget.min(1024 * 1024 * 1024),
                    super::policy::MAX_FILE_BYTES,
                )
                .await?,
            )
            .context("copy baseline exceeds quota")?;
    }
    ensure!(bytes <= budget, "combined copy baseline exceeds quota");
    let baseline = stage.join("baseline");
    tokio::fs::create_dir_all(&baseline).await?;
    let mut command = transport::git(cache, None).await?;
    command.env("GIT_WORK_TREE", &baseline);
    transport::git_output(
        command,
        &[
            "--work-tree",
            baseline.to_str().context("invalid copy baseline path")?,
            "checkout",
            &checkpoint.working_tree_oid,
            "--",
            ".",
        ],
        vec![],
    )
    .await?;
    staging_baseline(
        cache,
        &stage.join("baseline-handoff"),
        Some(checkpoint.handoff_oid.clone()),
        budget,
    )
    .await
}

/// Missing immutable staging baseline stays unknown. Never replace it with the
/// shadow's newest (possibly unacknowledged) local handoff commit.
pub(super) async fn staging_baseline(
    shadow: &Path,
    stage: &Path,
    published: Option<String>,
    budget: u64,
) -> Result<Option<(PathBuf, super::repository::staging::Descriptor)>> {
    let Some(revision) = published else {
        return Ok(None);
    };
    ensure!(
        revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid acknowledged staging baseline"
    );
    if !tokio::fs::try_exists(shadow.join("HEAD")).await? {
        return Ok(None);
    }
    let mut probe = transport::git(shadow, None).await?;
    probe.args(["cat-file", "-e", &format!("{revision}^{{commit}}")]);
    if !transport::run(probe, vec![], Duration::from_secs(10), 256)
        .await?
        .success
    {
        return Ok(None);
    }
    let bytes = transport::git_output(
        transport::git(shadow, None).await?,
        &["show", &format!("{revision}:manifest.json")],
        vec![],
    )
    .await?;
    ensure!(bytes.len() <= 256 * 1024, "baseline manifest exceeds limit");
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    let Some(descriptor) = manifest
        .repository
        .and_then(|repository| repository.staging)
    else {
        return Ok(None);
    };
    mirror::validate_tree(
        shadow,
        &revision,
        budget.min(1024 * 1024 * 1024),
        super::policy::MAX_FILE_BYTES,
    )
    .await?;
    tokio::fs::create_dir_all(stage).await?;
    let mut command = transport::git(shadow, None).await?;
    command.env("GIT_WORK_TREE", stage);
    transport::git_output(
        command,
        &[
            "--work-tree",
            stage.to_str().context("invalid baseline stage")?,
            "checkout",
            &revision,
            "--",
            "git",
        ],
        vec![],
    )
    .await?;
    super::repository::staging::require_service_format(stage, &descriptor).await?;
    Ok(Some((stage.to_owned(), descriptor)))
}

/// The published commit when this shadow still has it, else its main tip
/// (older state files recorded no publication).
async fn baseline_revision(shadow: &Path, published: Option<String>) -> Result<String> {
    if let Some(published) = published {
        ensure!(
            published.len() >= 40 && published.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid published baseline"
        );
        let mut probe = transport::git(shadow, None).await?;
        probe.args(["cat-file", "-e", &format!("{published}^{{tree}}")]);
        if transport::run(probe, vec![], Duration::from_secs(10), 256)
            .await?
            .success
        {
            return Ok(published);
        }
    }
    Ok("refs/heads/main".to_owned())
}

/// Three-way install of an incoming tree over the local project. Returns how
/// many local files were kept alongside an incoming version ("kept both"),
/// with up to 32 of their paths.
///
/// - incoming == baseline: the other side never touched it; local wins.
/// - local == baseline: only the other side changed it; incoming wins.
/// - both changed: incoming takes the path, and the user's own version is
///   kept right beside it (`<name>.mine-<yyyymmdd-hhmm>`, never mirrored).
/// - absent from incoming: deleted locally only when the incoming snapshot
///   carries an inventory (`left_out`) that shows it gone; a local edit is
///   kept beside it the same way first.
///
/// The report lists the kept copies' own paths (up to 32).
pub(super) fn install_tree(
    source: &Path,
    destination: &Path,
    baseline: Option<&Path>,
    left_out: Option<&[PathBuf]>,
) -> Result<(usize, Vec<PathBuf>)> {
    let mut kept = (0usize, Vec::new());
    let mut keep = |copy: PathBuf| {
        kept.0 += 1;
        if kept.1.len() < 32 {
            kept.1.push(copy);
        }
    };
    let mut copies = super::canonical::KeptCopies::new();
    let left_out: Option<std::collections::HashSet<&Path>> =
        left_out.map(|paths| paths.iter().map(PathBuf::as_path).collect());
    if let Some(baseline) = baseline {
        let mut pending = vec![(baseline.to_path_buf(), PathBuf::new())];
        let mut count = 0;
        while let Some((directory, relative)) = pending.pop() {
            for entry in std::fs::read_dir(directory)? {
                count += 1;
                ensure!(
                    count <= super::policy::MAX_PATHS,
                    "baseline tree exceeds limit"
                );
                let entry = entry?;
                let relative = relative.join(entry.file_name());
                ensure!(
                    super::policy::allowed_path(&relative),
                    "unsafe baseline path"
                );
                let kind = entry.file_type()?;
                ensure!(!kind.is_symlink(), "baseline tree contains symlink");
                if kind.is_dir() {
                    pending.push((entry.path(), relative));
                    continue;
                }
                if !kind.is_file() || source.join(&relative).try_exists()? {
                    continue;
                }
                // No inventory, or the other side still has it but left it
                // out of its snapshot: absence is not deletion.
                if left_out
                    .as_ref()
                    .is_none_or(|left_out| left_out.contains(relative.as_path()))
                {
                    continue;
                }
                let target = destination.join(&relative);
                let mut cursor = destination.to_path_buf();
                let safe = relative.components().all(|component| {
                    cursor.push(component);
                    !std::fs::symlink_metadata(&cursor)
                        .is_ok_and(|metadata| metadata.file_type().is_symlink())
                });
                // A deletion is safe only when the local file still equals
                // the shared baseline. Local edits and symlinks always win.
                if safe && target.try_exists()? {
                    if !same_file(&target, &entry.path())? {
                        keep(copies.keep(&target, &relative)?);
                    }
                    std::fs::remove_file(target)?;
                }
            }
        }
    }
    let mut pending = vec![(source.to_path_buf(), PathBuf::new())];
    let mut count = 0;
    while let Some((directory, relative)) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            count += 1;
            ensure!(
                count <= super::policy::MAX_PATHS,
                "hydrated tree exceeds limit"
            );
            let entry = entry?;
            let relative = relative.join(entry.file_name());
            ensure!(
                super::policy::allowed_path(&relative),
                "unsafe hydrated path"
            );
            let kind = entry.file_type()?;
            ensure!(!kind.is_symlink(), "hydrated tree contains symlink");
            let target = destination.join(&relative);
            ensure!(
                !std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()),
                "destination contains symlink"
            );
            if kind.is_dir() {
                std::fs::create_dir_all(&target)?;
                pending.push((entry.path(), relative));
            } else if kind.is_file() {
                let base = baseline.map(|root| root.join(&relative));
                let unchanged_remotely = base.as_ref().is_some_and(|base| {
                    base.is_file() && same_file(&entry.path(), base).unwrap_or(false)
                });
                if target.exists() && !same_file(&target, &entry.path())? {
                    let unchanged_locally = base
                        .as_ref()
                        .is_some_and(|base| same_file(&target, base).unwrap_or(false));
                    if unchanged_locally {
                        std::fs::copy(entry.path(), target)?;
                    } else if unchanged_remotely {
                        // Only this computer changed it: keep the local edit.
                    } else {
                        keep(copies.keep(&target, &relative)?);
                        std::fs::copy(entry.path(), target)?;
                    }
                } else if !target.exists() && unchanged_remotely {
                    // Deleted here, untouched there: the local deletion stands.
                } else {
                    std::fs::copy(entry.path(), target)?;
                }
            }
        }
    }
    Ok(kept)
}

fn chat_at_pause(
    chat: &chimaera_agent::ChatInfo,
    carry: Option<&chimaera_agent::Carryover>,
    queued_input: bool,
    agent_state: Option<crate::agent_state::AgentState>,
) -> bool {
    let Some(carry) = carry else {
        return false;
    };
    chat.background_running == 0
        && carry.background.is_empty()
        && !queued_input
        && (chat.pending_permission
            || chat.status_needs_action
            || (!carry.turn_in_flight
                && (chat.status_category.as_deref() == Some("idle")
                    || matches!(
                        agent_state,
                        Some(
                            crate::agent_state::AgentState::Finished
                                | crate::agent_state::AgentState::IdlePrompt
                        )
                    ))))
}

pub(super) fn at_pause(state: &AppState, workspace: &str) -> bool {
    sessions(state, workspace).into_iter().all(|id| {
        if let Some(chat) = state.chat.get(&id) {
            let activity = state.chat.input_activity(&id);
            let agent_state = lock(&state.agents).get(&id).map(|agent| agent.state);
            chat_at_pause(
                &chat,
                activity.as_ref().map(|(carry, _)| carry),
                activity.as_ref().is_none_or(|(_, pending)| *pending),
                agent_state,
            )
        } else {
            // Cloned first: the terminal registry has its own locks.
            let Some(agent) = lock(&state.agents).get(&id).cloned() else {
                return true;
            };
            let Some(info) = state.sessions.get(&id) else {
                return true;
            };
            crate::agent_state::tui_at_pause(
                &agent,
                info.alive,
                info.last_output_at,
                info.pid,
                state.sessions.foreground_pid(&id),
                crate::session_view::now_ms(),
            )
        }
    })
}
/// A chat running work right now: a turn in flight, input queued for the
/// next one, or background work still going. A turn parked on a permission
/// or a question waits on the user, which is not work.
fn chat_working(
    chat: &chimaera_agent::ChatInfo,
    carry: Option<&chimaera_agent::Carryover>,
    queued_input: bool,
) -> bool {
    chat.alive
        && !chat.pending_permission
        && (queued_input
            || chat.background_running > 0
            || carry.is_some_and(|carry| carry.turn_in_flight || !carry.background.is_empty()))
}

/// The agents (`claude`, `codex`, ...) running work in this project right
/// now, each named once: what the native app's quit question names. The
/// inverse of `at_pause` per session, except that a session nothing is known
/// about is not counted as working.
pub(super) fn working_agents(state: &AppState, workspace: &str) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    for id in sessions(state, workspace) {
        let (working, kind) = if let Some(chat) = state.chat.get(&id) {
            let activity = state.chat.input_activity(&id);
            let working = chat_working(
                &chat,
                activity.as_ref().map(|(carry, _)| carry),
                activity.as_ref().is_some_and(|(_, pending)| *pending),
            );
            let kind = lock(&state.agents)
                .get(&id)
                .map_or(chat.agent, |record| record.kind.as_str().to_owned());
            (working, kind)
        } else {
            // Cloned first: the terminal registry has its own locks.
            let record = lock(&state.agents).get(&id).cloned();
            let (Some(record), Some(info)) = (record, state.sessions.get(&id)) else {
                continue;
            };
            let working = info.alive
                && !crate::agent_state::tui_at_pause(
                    &record,
                    info.alive,
                    info.last_output_at,
                    info.pid,
                    state.sessions.foreground_pid(&id),
                    crate::session_view::now_ms(),
                );
            (working, record.kind.as_str().to_owned())
        };
        if working && !kind.is_empty() && kind.len() <= 32 && !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}
/// A device waits this long between automatic attempts to finish one return.
const RETURN_BACKOFF_MAX: u64 = 1800;

/// Whether the account reports this project's owner as a cloud machine that
/// is suspended but keeps ownership (placement availability `suspended`).
/// Passive: reading placement never wakes anything. Any failure reads as not
/// suspended, which keeps the earlier behavior.
async fn owner_suspended(config: &Configure, workspace: &str) -> bool {
    if config.execution.is_none() {
        return false;
    }
    let Ok(response) = account(
        config,
        &format!("/v2/workspaces/{workspace}/placement"),
        "GET",
        None,
    )
    .await
    else {
        return false;
    };
    response.json::<serde_json::Value>().is_ok_and(|placement| {
        placement["workspace_id"] == workspace && placement["availability"] == "suspended"
    })
}

/// How long this computer must have been awake on power before live cloud
/// work moves home: five minutes. A development build (the loopback
/// end-to-end harness) may shorten it with `CHIMAERA_PRO_SETTLE_SECS`;
/// release builds ignore the variable.
fn settle_seconds() -> u64 {
    static SETTLE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *SETTLE.get_or_init(|| {
        settle_override(
            chimaera_core::is_dev_build(),
            std::env::var("CHIMAERA_PRO_SETTLE_SECS").ok().as_deref(),
        )
    })
}
fn settle_override(dev: bool, value: Option<&str>) -> u64 {
    const SETTLE: u64 = 300;
    value
        .filter(|_| dev)
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(SETTLE, |seconds| seconds.min(SETTLE))
}

pub(super) async fn lazy_handback(state: &Arc<AppState>, config: &Configure) -> Result<()> {
    if config.delegation.workspace.is_some() || config.role != Role::Device {
        return Ok(());
    }
    // Moving live cloud work home waits until this computer has been awake
    // and on power for a while (both protocol versions); work the cloud is
    // not running returns at once (laptop first).
    let settled = state.pro.power_suitable.load(Ordering::Acquire)
        && super::now().saturating_sub(state.pro.awake_since.load(Ordering::Acquire))
            >= settle_seconds();
    let candidates: Vec<_> = lock(&state.pro.ownership)
        .iter()
        .filter_map(|(id, owner)| match owner {
            Ownership::Remote { epoch, holder } => Some((id.clone(), *epoch, Some(holder.clone()))),
            Ownership::Hydrating { epoch } => Some((id.clone(), *epoch, None)),
            _ => None,
        })
        .take(128)
        .collect();
    let mut hosts: Option<Vec<super::protocol::Host>> = None;
    for (workspace, epoch, holder) in candidates {
        if super::project_copy::copy_only(state, &workspace) {
            continue;
        }
        // Only a project already registered on this device may return automatically.
        // Discovery and old global-folder preferences never authorize adoption.
        if lock(&state.workspaces).get(&workspace).is_none()
            || super::projects::adoption_pending(state, &workspace)
            || !super::projects::account_matches(state, &workspace)
        {
            continue;
        }
        // Handed to the cloud when the app quit: it stays there, live or
        // released, until the app returns (`/pro/wake`). A project being
        // brought here because the user acted on it is that request's.
        if super::parked(state, &workspace) || super::moves::pulling(state, &workspace) {
            continue;
        }
        // The account's preferred installation is the latest computer that
        // had the project; one the user opened it on since may pull it home
        // too, once nothing live holds it (the settle rule below still
        // decides moving live cloud work).
        if !(execution::preferred_here(state, config, &workspace)
            || execution::opened_here(state, &workspace))
            || lock(&state.pro.preferences)
                .get(&workspace)
                .is_some_and(|p| p.never_mirror)
        {
            continue;
        }
        if lock(&state.pro.return_backoff)
            .get(&workspace)
            .is_some_and(|(next, _)| *next > super::now())
        {
            continue;
        }
        let result = async {
            let operation_config = execution::effective(state, config, &workspace)?;
            let baton: Baton = account(
                &operation_config,
                &execution::path(&operation_config, &workspace, ""),
                "GET",
                None,
            )
            .await
            .context("Could not check where your work is running")?
            .json()
            .context("Could not confirm where your work is running")?;
            ensure!(baton.workspace_id == workspace, "baton workspace mismatch");
            execution::observe(state, config, &baton)?;
            let mine = baton.holder_id.as_deref() == Some(&config.delegation.device_id);
            // A cloud machine asleep with ownership reads expired too, but the
            // account refuses to let anyone else acquire it (409 `held`): it
            // must be woken and asked to hand back, like live cloud work.
            let suspended = holder.is_some()
                && baton.holder_id.is_some()
                && !mine
                && execution::expired(&baton)
                && owner_suspended(&operation_config, &workspace).await;
            let target = match (&holder, baton.holder_id.as_deref()) {
                // Released by the cloud: nothing runs there, hydrate now.
                (_, None) => Some(baton.epoch),
                // A return this device already acquired did not finish.
                (None, Some(_)) if mine && baton.epoch == epoch => Some(epoch),
                (None, _) => None,
                // The cloud's lease lapsed (it stopped or lost the account):
                // take the project home from its last acknowledged checkpoint.
                (Some(_), Some(_)) if !mine && execution::expired(&baton) && !suspended => {
                    Some(baton.epoch)
                }
                (Some(recorded), Some(current)) if current == recorded && settled => {
                    if hosts.is_none() {
                        let response = transport::request(
                            &config.keeper_url,
                            "/v1/hosts",
                            "GET",
                            &config.delegation.access_token,
                            None,
                        )
                        .await
                        .context("Could not reconnect to your saved work")?;
                        // The account is down: the keeper holds on and says
                        // so. A quiet wait; the next pass asks again.
                        if transport::account_unavailable(&response) {
                            return Ok(());
                        }
                        hosts = Some(
                            response
                                .json()
                                .context("Could not read your connected workspaces")?,
                        );
                    }
                    let host = hosts.as_ref().and_then(|hosts| {
                        hosts
                            .iter()
                            .find(|host| {
                                host.worker_holder() == Some(current) && host.status == "connected"
                            })
                            .cloned()
                    });
                    match host {
                        Some(host) => {
                            handback::prepare(
                                state,
                                config,
                                &workspace,
                                &host,
                                current,
                                baton.epoch,
                            )
                            .await?
                        }
                        None => None,
                    }
                }
                _ => None,
            };
            let Some(epoch) = target else {
                return Ok::<_, anyhow::Error>(());
            };
            hydrate(state, config, &workspace, epoch, false, None)
                .await
                .context("Could not restore your saved work on this computer")?;
            if let Some(status) = lock(&state.pro.status).get_mut(&workspace) {
                status.error = None;
                status.error_code = None;
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                lock(&state.pro.return_backoff).remove(&workspace);
            }
            Err(error) => {
                {
                    let mut backoff = lock(&state.pro.return_backoff);
                    if backoff.len() >= 128 && !backoff.contains_key(&workspace) {
                        backoff.clear();
                    }
                    // This computer's own unfinished return keeps its project
                    // fenced here, so it retries quickly (15 s doubling to two
                    // minutes); moving cloud work home can wait longer.
                    let (first, most) = if holder.is_none() {
                        (15, 120)
                    } else {
                        (120, RETURN_BACKOFF_MAX)
                    };
                    let delay = backoff
                        .get(&workspace)
                        .map_or(first, |(_, delay)| (delay * 2).min(most));
                    backoff.insert(workspace.clone(), (super::now() + delay, delay));
                }
                record_error(state, &workspace, &error);
                tracing::warn!(phase="automatic_return", error=%error, "Project return did not complete");
            }
        }
    }
    Ok(())
}

fn same_file(left: &Path, right: &Path) -> Result<bool> {
    use std::io::Read;
    let open = |path: &Path| -> Result<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            );
        }
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() <= super::policy::MAX_FILE_BYTES,
            "comparison requires bounded regular files"
        );
        Ok(file)
    };
    let mut left = open(left)?;
    let mut right = open(right)?;
    if left.metadata()?.len() != right.metadata()?.len() {
        return Ok(false);
    }
    let mut a = [0u8; 65536];
    let mut b = [0u8; 65536];
    loop {
        let count = left.read(&mut a)?;
        if count == 0 {
            return Ok(true);
        }
        right.read_exact(&mut b[..count])?;
        if a[..count] != b[..count] {
            return Ok(false);
        }
    }
}

async fn finish_hydration(
    state: &Arc<AppState>,
    workspace: &str,
    epoch: u64,
    generation: u64,
    setup: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    finish_hydration_checked(
        state,
        workspace,
        epoch,
        generation,
        setup,
        super::provider_gate::check(state, workspace, true),
    )
    .await
}

async fn finish_hydration_checked(
    state: &Arc<AppState>,
    workspace: &str,
    epoch: u64,
    generation: u64,
    setup: impl std::future::Future<Output = Result<()>>,
    providers: impl std::future::Future<Output = Vec<super::provider_gate::BlockedProvider>>,
) -> Result<()> {
    {
        let _configuration = state.pro.configuration.lock().await;
        ensure!(
            generation == state.pro.generation.load(Ordering::Acquire),
            "Account changed during project setup"
        );
        {
            let mut ownership = lock(&state.pro.ownership);
            ensure!(
                matches!(ownership.get(workspace),Some(Ownership::Hydrating{epoch:current} | Ownership::SettingUp{epoch:current}) if *current==epoch),
                "Project ownership changed before setup"
            );
            ownership.insert(workspace.into(), Ownership::SettingUp { epoch });
        }
        super::persist(state).await?;
    }
    if let Err(error) = super::PROFILE_SETUP
        .scope((workspace.to_owned(), generation), setup)
        .await
    {
        record_error(state, workspace, &error);
        state.changes.notify_waiters();
        return Err(error);
    }
    let blocked = providers.await;
    // Account replacement cannot race a successful readiness check into a new
    // writer grant. Checks are outside this lock; the local transition is not.
    let _configuration = state.pro.configuration.lock().await;
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire)
            && matches!(lock(&state.pro.ownership).get(workspace),Some(Ownership::SettingUp {epoch:current}) if *current==epoch),
        "Project ownership changed during setup"
    );
    // A provider that is not signed in holds back only its own sessions: the
    // project and every other conversation continue (paused rows name it).
    super::provider_gate::record(state, workspace, blocked.clone());
    {
        let mut ownership = lock(&state.pro.ownership);
        ensure!(
            matches!(ownership.get(workspace), Some(Ownership::SettingUp { epoch: current }) if *current == epoch),
            "Project ownership changed before resume"
        );
        ownership.insert(workspace.into(), Ownership::Local { epoch });
    }
    super::persist(state).await?;
    if blocked.is_empty() {
        if let Some(status) = lock(&state.pro.status).get_mut(workspace) {
            status.error = None;
            status.error_code = None;
        }
    }
    // Each managed child takes this lock for durable launch admission. Release
    // it before restoring sessions; their admission rechecks the current grant.
    drop(_configuration);
    ensure!(
        generation == state.pro.generation.load(Ordering::Acquire),
        "Account changed before project resume"
    );
    if execution::resume_allowed(state, workspace) {
        // Its own task: this usually runs in the mirror task, which sign-out
        // aborts (`stop_tasks`), and a respawn cut half way would leave the
        // returned sessions deferred as "moved". The caller still waits.
        let owner = state.clone();
        let workspace = workspace.to_owned();
        tokio::spawn(async move {
            crate::ledger::resume_deferred_filtered(&owner, &workspace, |entry| {
                !super::provider_gate::waits_for_provider(entry, &blocked)
            })
            .await
        })
        .await
        .context("project resume stopped")??;
    }
    Ok(())
}

async fn run_profile_steps(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
) -> Result<()> {
    // Deferred laptop steps are agent guidance, never daemon auto-exec.
    if config.role != Role::Worker {
        return Ok(());
    }
    let profile = lock(&state.pro.preferences)
        .get(workspace)
        .map(|preference| preference.profile.clone())
        .unwrap_or_default();
    let Some(command) = profile.setup_command else {
        return Ok(());
    };
    let root = lock(&state.workspaces)
        .get(workspace)
        .context("unknown workspace")?
        .root;
    ensure!(
        super::may_write(state, workspace),
        "Account changed before project setup execution"
    );
    // Setup is the system's job, not a terminal the user must watch: it runs
    // in the background in the user's login shell; a failure leaves one plain
    // status line and its output tail in the project's setup log.
    let log = state.pro.root.join(workspace).join("setup.log");
    // Boxed: this runs inside the hydrate future, which callers hold inline.
    let guard = execution::setup::Guard::begin(state, workspace).await?;
    Box::pin(run_setup_command_guarded(
        &root,
        &command,
        &log,
        Some(guard),
    ))
    .await
}

/// How long a project's setup may run, and how much of its output is kept.
const SETUP_DEADLINE: Duration = Duration::from_secs(600);
const SETUP_LOG_BYTES: usize = 64 * 1024;

#[cfg(test)]
async fn run_setup_command(root: &Path, command: &str, log: &Path) -> Result<()> {
    run_setup_command_guarded(root, command, log, None).await
}

async fn run_setup_command_guarded(
    root: &Path,
    command: &str,
    log: &Path,
    guard: Option<execution::setup::Guard>,
) -> Result<()> {
    use tokio::io::AsyncReadExt;
    if let Some(guard) = &guard {
        guard.check()?;
    }
    let mut child = tokio::process::Command::new(crate::launcher::login_shell())
        .args(["-lc", command])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("project setup could not start")?;
    let group = child.id();
    if let Some(guard) = &guard {
        guard.attach(group.context("project setup group unavailable")?);
        guard.check()?;
    }
    let tail = std::sync::Mutex::new(Vec::<u8>::new());
    let keep = |bytes: &[u8]| {
        let mut tail = lock(&tail);
        tail.extend_from_slice(bytes);
        if tail.len() > SETUP_LOG_BYTES {
            let excess = tail.len() - SETUP_LOG_BYTES;
            tail.drain(..excess);
        }
    };
    let (mut stdout, mut stderr) = (child.stdout.take(), child.stderr.take());
    let drain = |stream: Option<tokio::process::ChildStdout>| async {
        let Some(mut stream) = stream else { return };
        let mut block = vec![0u8; 8192];
        while let Ok(count) = stream.read(&mut block).await {
            if count == 0 {
                break;
            }
            keep(&block[..count]);
        }
    };
    let drain_err = |stream: Option<tokio::process::ChildStderr>| async {
        let Some(mut stream) = stream else { return };
        let mut block = vec![0u8; 8192];
        while let Ok(count) = stream.read(&mut block).await {
            if count == 0 {
                break;
            }
            keep(&block[..count]);
        }
    };
    let finished = tokio::time::timeout(SETUP_DEADLINE, async {
        let wait = async {
            let status = child.wait().await;
            if let Some(guard) = &guard { guard.kill(); }
            status
        };
        let joined = async {
            let (_, _, status) = tokio::join!(drain(stdout.take()), drain_err(stderr.take()), wait);
            status
        };
        tokio::pin!(joined);
        loop {
            tokio::select! {
                status = &mut joined => return status,
                () = tokio::time::sleep(Duration::from_millis(100)), if guard.is_some() => {
                    if guard.as_ref().unwrap().check().is_err() {
                        guard.as_ref().unwrap().kill();
                        return Err(std::io::Error::other("project setup execution authority changed"));
                    }
                }
            }
        }
    })
    .await;
    let succeeded = match finished {
        Ok(Ok(status)) => status.success(),
        _ => {
            // Timed out: stop the whole setup process group.
            if let Some(group) = group.and_then(|id| i32::try_from(id).ok()) {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(group),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
            let _ = child.kill().await;
            false
        }
    };
    let output = std::mem::take(&mut *lock(&tail));
    let log = log.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || {
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(log, output)
    })
    .await;
    if let Some(guard) = guard {
        guard.finish().await?;
    }
    ensure!(succeeded, "project setup did not finish");
    Ok(())
}

#[cfg(test)]
#[path = "continuity_tests.rs"]
pub(super) mod continuity_tests;
#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;

pub(super) fn eligible(state: &AppState, workspace: &crate::workspaces::Workspace) -> bool {
    if super::project_copy::copy_only(state, &workspace.id) {
        return false;
    }
    if authority::registered_root(state, &workspace.id, &workspace.root).is_err() {
        return false;
    }
    if workspace.hidden
        || crate::cloud::is_onboarding_workspace(workspace)
        || lock(&state.pro.legacy_pending).contains(&workspace.id)
        || !super::projects::account_matches(state, &workspace.id)
    {
        return false;
    }
    let home = state.claude_settings_path.parent().and_then(Path::parent);
    if home.is_some_and(|home| home.starts_with(&workspace.root)) {
        return false;
    }
    if workspace.root.components().any(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some(
                ".ssh"
                    | ".aws"
                    | ".azure"
                    | ".gnupg"
                    | ".config"
                    | ".codex"
                    | ".claude"
                    | ".chimaera"
                    | "Library"
            )
        )
    }) {
        return false;
    }
    workspace.last_opened_at.saturating_add(30 * 24 * 3600) >= super::now()
        || lock(&state.session_workspaces)
            .values()
            .any(|id| id == &workspace.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transfer_overflow_preserves_work_and_plain_shells_never_hide_a_live_agent() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-cap-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("work.txt"), b"local work").unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        lock(&state.workspaces)
            .import_exact(crate::workspaces::Workspace {
                id: "w-project".into(),
                root: root.clone(),
                name: "Fixture".into(),
                last_opened_at: super::super::now(),
                mastermind: None,
                plugins_on: vec![],
                cloud_internal: false,
                hidden: false,
            })
            .unwrap();
        lock(&state.pro.ownership).insert("w-project".into(), Ownership::Local { epoch: 7 });
        {
            let mut registry = lock(&state.session_workspaces);
            for index in 0..65 {
                registry.insert(format!("s-fixture-{index}"), "w-project".into());
            }
        }
        // Pick the entry the previous take(64)-before-filter implementation
        // omitted, without depending on HashMap's random iteration order.
        let id = lock(&state.session_workspaces)
            .keys()
            .last()
            .unwrap()
            .clone();
        let session = state
            .sessions
            .spawn(chimaera_pty::SpawnOpts {
                cwd: root.clone(),
                name: None,
                cols: 80,
                rows: 24,
                command: Some(vec!["/bin/sleep".into(), "30".into()]),
                id: Some(id.clone()),
                env: vec![],
                env_remove: vec![],
                scrollback: None,
            })
            .unwrap();
        let mut agent =
            crate::agents::AgentRecord::new("fixture".into(), crate::agents::AgentKind::Claude);
        agent.state = crate::agent_state::AgentState::Running;
        lock(&state.agents).insert(id.clone(), agent);
        assert!(sessions(&state, "w-project") == vec![id.clone()]);
        assert!(live_agents(&state, "w-project"));
        assert!(!at_pause(&state, "w-project"));
        assert!(!working_agents(&state, "w-project").is_empty());
        assert!(transfer_session_ids(&state, "w-project").is_err());
        let config:Configure = serde_json::from_value(json!({"endpoint":"http://127.0.0.1:1","keeper_url":"","delegation":{"access_token":"fixture","expires_at":"2099-01-01T00:00:00Z","scope":["baton","mirror"],"device_id":"device"}})).unwrap();
        let error = snapshot(&state, &config, "w-project", true)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("at most 64 sessions"));
        assert!(state
            .sessions
            .get(&session.id)
            .is_some_and(|info| info.alive));
        assert!(matches!(
            lock(&state.pro.ownership).get("w-project"),
            Some(Ownership::Local { epoch: 7 })
        ));
        assert_eq!(std::fs::read(root.join("work.txt")).unwrap(), b"local work");
        assert!(!state.pro.root.join("w-project/working-tree.git").exists());
        let omitted_shell = lock(&state.session_workspaces)
            .keys()
            .find(|candidate| **candidate != id)
            .unwrap()
            .clone();
        lock(&state.session_workspaces).remove(&omitted_shell);
        assert_eq!(transfer_session_ids(&state, "w-project").unwrap().len(), 64);
        state.sessions.kill(&session.id).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_finished_turn_is_copied_soon_and_the_timer_stays_the_backstop() {
        let mut turns = TurnEnds::default();
        let ids = |list: &[&str]| {
            list.iter()
                .map(|id| id.to_string())
                .collect::<BTreeSet<_>>()
        };
        // Nothing ended: only the timer copies.
        turns.observe("a", true);
        turns.observe("b", false);
        assert_eq!(turns.due(TURN_COPY_GAP), None);
        assert_eq!(turns.due(TIMED_COPY), Some(None));
        // The agent in `a` finishes: that project is due once the gap passed,
        // and only that project.
        turns.observe("a", false);
        assert_eq!(turns.due(TURN_COPY_GAP - 1), None);
        assert_eq!(turns.due(TURN_COPY_GAP), Some(Some(ids(&["a"]))));
        // A copy that started is not repeated; a turn that ends while it runs
        // is copied next.
        turns.observe("b", true);
        turns.copied(&Some(ids(&["a"])));
        assert_eq!(turns.due(TURN_COPY_GAP), None);
        turns.observe("b", false);
        assert_eq!(turns.due(TURN_COPY_GAP), Some(Some(ids(&["b"]))));
        // The timer pass covers everything pending.
        turns.copied(&None);
        assert_eq!(turns.due(TURN_COPY_GAP), None);
        // A project the loop no longer iterates is forgotten.
        turns.observe("c", true);
        turns.observe("c", false);
        turns.keep(&ids(&["a", "b"]));
        assert_eq!(turns.due(TURN_COPY_GAP), None);
    }
    #[tokio::test]
    async fn setup_failure_fences_agents_until_success_and_laptop_steps_never_autoplay() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-setup-fence-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        lock(&state.workspaces)
            .import_exact(crate::workspaces::Workspace {
                id: "w-project".into(),
                root: root.clone(),
                name: "Fixture".into(),
                last_opened_at: super::super::now(),
                mastermind: None,
                plugins_on: vec![],
                cloud_internal: false,
                hidden: false,
            })
            .unwrap();
        lock(&state.pro.ownership).insert("w-project".into(), Ownership::Hydrating { epoch: 3 });
        let owner = state.clone();
        let result = finish_hydration(&state, "w-project", 3, 0, async move {
            assert!(super::super::may_write(&owner, "w-project"));
            let outside = owner.clone();
            assert!(
                !tokio::spawn(async move { super::super::may_write(&outside, "w-project") })
                    .await
                    .unwrap()
            );
            anyhow::bail!("fixture setup failed")
        })
        .await;
        assert!(result.is_err());
        assert!(!super::super::may_write(&state, "w-project"));
        assert_eq!(super::super::owned_epoch(&state, "w-project"), None);
        assert!(lock(&state.pro.status)
            .get("w-project")
            .unwrap()
            .error
            .is_some());
        let restarted = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        assert!(!super::super::may_write(&restarted, "w-project"));
        finish_hydration(&state, "w-project", 3, 0, async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(super::super::owned_epoch(&state, "w-project"), Some(3));
        let config = Configure {
            recovery: false,
            execution: None,
            account_id: None,
            role: Role::Device,
            endpoint: String::new(),
            keeper_url: String::new(),
            hours_exhausted: false,
            delegation: super::super::protocol::Delegation {
                workspace: None,
                access_token: String::new(),
                expires_at: String::new(),
                scope: vec![],
                device_id: String::new(),
            },
        };
        lock(&state.pro.preferences)
            .entry("w-project".into())
            .or_default()
            .profile
            .deferred = vec!["must-not-be-executed".into()];
        run_profile_steps(&state, &config, "w-project")
            .await
            .unwrap();
        assert_eq!(
            lock(&state.pro.preferences)
                .get("w-project")
                .unwrap()
                .profile
                .deferred,
            ["must-not-be-executed"]
        );
        drop(restarted);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
    /// A project's cloud setup runs in the background, never as a terminal
    /// the user has to watch; a failure leaves a status code and a log.
    #[tokio::test]
    async fn cloud_setup_runs_in_the_background_without_a_terminal() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-setup-background-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("setup.log");
        run_setup_command(&root, "echo installed > marker; echo done", &log)
            .await
            .unwrap();
        assert!(root.join("marker").exists(), "ran in the project root");
        let error = run_setup_command(&root, "echo broken; exit 3", &log)
            .await
            .unwrap_err();
        assert_eq!(
            super::super::routes::error_code(&error),
            "cloud_setup_failed"
        );
        assert!(std::fs::read_to_string(&log).unwrap().contains("broken"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn managed_setup_drains_descendants_on_expiry_cancellation_and_shell_exit() {
        for fault in ["expired", "cancelled", "success"] {
            let root = std::env::temp_dir().join(format!(
                "chimaera-setup-group-{}",
                chimaera_core::generate_token()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let state = Arc::new(AppState::new(
                "fixture".into(),
                "fixture".into(),
                4242,
                0,
                root.clone(),
                root.join("config"),
            ));
            execution::install_fixture(&state, "w-project", 4).unwrap();
            execution::worker_fixture(&state);
            lock(&state.pro.ownership)
                .insert("w-project".into(), Ownership::SettingUp { epoch: 4 });
            let guard = super::super::PROFILE_SETUP
                .scope(
                    ("w-project".into(), 0),
                    execution::setup::Guard::begin(&state, "w-project"),
                )
                .await
                .unwrap();
            let restarted = Arc::new(AppState::new(
                "fixture".into(),
                "fixture".into(),
                4242,
                0,
                root.clone(),
                root.join("config"),
            ));
            assert!(
                execution::unclean(&restarted, "w-project"),
                "durable pre-spawn intent must remain unknown after crash"
            );
            let cwd = root.clone();
            let command = if fault == "success" {
                "sleep 30 & echo $! > descendant; echo ready > started"
            } else {
                "sleep 30 & echo $! > descendant; echo ready > started; wait"
            };
            let task = tokio::spawn(async move {
                run_setup_command_guarded(&cwd, command, &cwd.join("setup.log"), Some(guard)).await
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                while !root.join("started").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(!execution::quiescent(&state, "w-project") || fault == "success");
            match fault {
                "expired" => {
                    super::super::expire_execution_fixture(&state, "w-project");
                    assert!(task.await.unwrap().is_err());
                }
                "cancelled" => {
                    task.abort();
                    assert!(task.await.unwrap_err().is_cancelled());
                }
                _ => {
                    task.await.unwrap().unwrap();
                }
            }
            tokio::time::timeout(Duration::from_secs(3), async {
                while !execution::quiescent(&state, "w-project") {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            assert!(!execution::setup::active(&state, "w-project"));
            let pending = lock(&state.pro.preferences)
                .get("w-project")
                .unwrap()
                .execution_launch_pending;
            assert_eq!(
                pending,
                fault == "cancelled",
                "only observed cleanup durably settles intent"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    /// Sign-out aborts the mirror task that finishes a return. The returned
    /// sessions that finish was respawning start anyway: a respawn cut half
    /// way would leave them deferred, answering "moved" forever.
    #[tokio::test]
    async fn aborting_a_finished_return_still_resumes_its_sessions() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "chimaera-return-abort-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        lock(&state.workspaces)
            .import_exact(crate::workspaces::Workspace {
                id: "w-project".into(),
                root: root.clone(),
                name: "Fixture".into(),
                last_opened_at: super::super::now(),
                mastermind: None,
                plugins_on: vec![],
                cloud_internal: false,
                hidden: false,
            })
            .unwrap();
        let claude = root.join("claude");
        std::fs::write(
            &claude,
            "#!/bin/sh\n\
             printf '%s\\n' '{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"init\",\"response\":{\"commands\":[]}}}'\n\
             cat >/dev/null\n",
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        lock(&state.agent_bins).insert(
            crate::agents::AgentKind::Claude,
            crate::launcher::AgentDetection {
                path: Ok(claude),
                version: Some("9.9.9-fake".into()),
                managed: false,
                explicit: true,
                mtime: None,
            },
        );
        // An enrolled project: each agent launch waits for launch admission.
        super::super::install_execution_fixture(&state, "w-project", 3).unwrap();
        lock(&state.pro.ownership).insert("w-project".into(), Ownership::SettingUp { epoch: 3 });
        crate::ledger::defer(
            &state,
            crate::ledger::LedgerEntry {
                id: "s-returned".into(),
                suspended: true,
                manual_resume_reason: None,
                handoff: Some(crate::bundle::HandoffResume {
                    fork: false,
                    origin: crate::bundle::Origin::Home,
                    epoch: 3,
                }),
                workspace_id: "w-project".into(),
                cwd: root.clone(),
                pinned_name: None,
                cols: 80,
                rows: 24,
                theme: "dark".into(),
                created_at: 1,
                agent: Some(crate::ledger::LedgerAgent {
                    kind: crate::agents::AgentKind::Claude,
                    resume: None,
                    transcript: None,
                    native_cwd: None,
                    title: "Fixture".into(),
                    ui: chimaera_agent::model::SessionUi::Chat,
                    model: None,
                    carryover: None,
                }),
            },
        )
        .unwrap();
        let owner = state.clone();
        let task = tokio::spawn(async move {
            finish_hydration(&owner, "w-project", 3, 0, async { Ok(()) }).await
        });
        // Queued while the finish makes the project Local, so it is handed
        // over the moment the finish releases it: the resume has started
        // and waits for this admission when the task is aborted.
        while super::super::owned_epoch(&state, "w-project").is_none() {
            tokio::task::yield_now().await;
        }
        let admission = state.pro.configuration.lock().await;
        task.abort();
        let _ = task.await;
        drop(admission);
        tokio::time::timeout(Duration::from_secs(10), async {
            while !state.chat.get("s-returned").is_some_and(|chat| chat.alive)
                || lock(&state.deferred_sessions).contains_key("s-returned")
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the aborted finish's resume completes on its own");
        state.chat.kill("s-returned");
        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn three_way_return_preserves_local_conflicts_and_applies_unmodified_files() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-pro-merge-{}",
            chimaera_core::generate_token()
        ));
        let base = root.join("base");
        let local = root.join("local");
        let cloud = root.join("cloud");
        for dir in [&base, &local, &cloud] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for name in ["same.txt", "conflict.txt"] {
            std::fs::write(base.join(name), "base").unwrap();
            std::fs::write(cloud.join(name), "cloud").unwrap();
        }
        std::fs::write(local.join("same.txt"), "base").unwrap();
        std::fs::write(local.join("conflict.txt"), "local").unwrap();
        for name in ["removed.txt", "removed-but-edited.txt"] {
            std::fs::write(base.join(name), "base").unwrap();
            std::fs::write(
                local.join(name),
                if name == "removed.txt" {
                    "base"
                } else {
                    "local"
                },
            )
            .unwrap();
        }
        let (count, kept) = install_tree(&cloud, &local, Some(&base), Some(&[])).unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            std::fs::read_to_string(local.join("same.txt")).unwrap(),
            "cloud"
        );
        // Both changed: the incoming version takes the path and the user's
        // own version sits right beside it; the report names the copy.
        assert_eq!(
            std::fs::read_to_string(local.join("conflict.txt")).unwrap(),
            "cloud"
        );
        assert!(!local.join("removed.txt").exists());
        assert!(!local.join("removed-but-edited.txt").exists());
        for (original, body) in [
            ("conflict.txt", "local"),
            ("removed-but-edited.txt", "local"),
        ] {
            let copy = kept
                .iter()
                .find(|path| {
                    path.to_string_lossy()
                        .starts_with(&format!("{original}.mine-"))
                })
                .unwrap_or_else(|| panic!("{original}: {kept:?}"));
            assert_eq!(std::fs::read_to_string(local.join(copy)).unwrap(), body);
            assert!(
                !super::super::policy::allowed_path(copy),
                "a kept copy stays on this computer"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn canonical_return_uses_cloud_files_and_preserves_unpublished_local_edits() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-canonical-return-{}",
            chimaera_core::generate_token()
        ));
        let local = root.join("local");
        let cloud = root.join("cloud");
        let base = root.join("base");
        for dir in [&local, &cloud, &base] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for name in ["changed", "deleted"] {
            std::fs::write(base.join(name), "base").unwrap();
            std::fs::write(local.join(name), "unpublished local").unwrap();
        }
        std::fs::write(cloud.join("changed"), "canonical cloud").unwrap();
        let kept = install_tree(&cloud, &local, Some(&base), Some(&[])).unwrap();
        assert_eq!(kept.0, 2);
        assert_eq!(
            std::fs::read_to_string(local.join("changed")).unwrap(),
            "canonical cloud"
        );
        assert!(!local.join("deleted").exists());
        let mut preserved: Vec<_> = kept
            .1
            .iter()
            .map(|copy| std::fs::read_to_string(local.join(copy)).unwrap())
            .collect();
        preserved.sort();
        assert_eq!(preserved, vec!["unpublished local", "unpublished local"]);
        assert!(
            kept.1
                .iter()
                .all(|copy| !super::super::policy::allowed_path(copy)),
            "kept copies are never published as canonical project files"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod return_tests {
    use super::*;
    fn tree(root: &Path, files: &[(&str, &str)]) -> PathBuf {
        std::fs::create_dir_all(root).unwrap();
        for (name, body) in files {
            std::fs::write(root.join(name), body).unwrap();
        }
        root.to_path_buf()
    }
    #[test]
    fn edits_since_the_last_published_snapshot_survive_a_return() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-return-edits-{}",
            chimaera_core::generate_token()
        ));
        // Published T0; the laptop then edited notes (its T1 push failed) and
        // the cloud, continuing from T0, changed only report.
        let base = tree(&root.join("base"), &[("notes", "t0"), ("report", "t0")]);
        let local = tree(
            &root.join("local"),
            &[
                ("notes", "edited after t0"),
                ("report", "t0"),
                ("new", "local only"),
            ],
        );
        let cloud = tree(&root.join("cloud"), &[("notes", "t0"), ("report", "cloud")]);
        let kept = install_tree(&cloud, &local, Some(&base), Some(&[])).unwrap();
        assert_eq!(kept.0, 0, "no conflict: each side changed different files");
        assert_eq!(
            std::fs::read_to_string(local.join("notes")).unwrap(),
            "edited after t0"
        );
        assert_eq!(
            std::fs::read_to_string(local.join("report")).unwrap(),
            "cloud"
        );
        assert_eq!(
            std::fs::read_to_string(local.join("new")).unwrap(),
            "local only"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn a_file_left_out_of_the_snapshot_is_never_deleted_locally() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-return-left-out-{}",
            chimaera_core::generate_token()
        ));
        for (label, left_out, deleted) in [
            (
                "excluded",
                Some(vec![PathBuf::from("secret-looking")]),
                false,
            ),
            ("legacy", None, false),
            ("deleted", Some(vec![]), true),
        ] {
            let base = tree(
                &root.join(label).join("base"),
                &[("secret-looking", "same")],
            );
            let local = tree(
                &root.join(label).join("local"),
                &[("secret-looking", "same")],
            );
            let cloud = tree(&root.join(label).join("cloud"), &[]);
            install_tree(&cloud, &local, Some(&base), left_out.as_deref()).unwrap();
            assert_eq!(!local.join("secret-looking").exists(), deleted, "{label}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn the_baseline_is_the_published_commit_not_a_failed_local_one() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-return-baseline-{}",
            chimaera_core::generate_token()
        ));
        let shadow = root.join("working-tree.git");
        mirror::initialize(&shadow).await.unwrap();
        let published =
            mirror::commit_tree(&shadow, &tree(&root.join("t0"), &[("f", "t0")]), "main")
                .await
                .unwrap();
        let unpushed =
            mirror::commit_tree(&shadow, &tree(&root.join("t1"), &[("f", "t1")]), "main")
                .await
                .unwrap();
        assert_ne!(published, unpushed);
        assert_eq!(
            baseline_revision(&shadow, Some(published.clone()))
                .await
                .unwrap(),
            published
        );
        assert_eq!(
            baseline_revision(&shadow, None).await.unwrap(),
            "refs/heads/main"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn staging_baseline_uses_only_exact_acknowledged_handoff_and_never_the_newest_tip() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-staging-baseline-{}",
            chimaera_core::generate_token()
        ));
        tree(&root.join("source"), &[("file", "t0")]);
        let root = root.canonicalize().unwrap();
        let source = root.join("source");
        transport::git_output(
            transport::git(&source, None).await.unwrap(),
            &["init", "--quiet"],
            vec![],
        )
        .await
        .unwrap();
        transport::git_output(
            transport::git(&source, None).await.unwrap(),
            &["add", "file"],
            vec![],
        )
        .await
        .unwrap();
        let shadow = root.join("working-tree.git");
        mirror::initialize(&shadow).await.unwrap();
        let mut acknowledged = None;
        let mut copy_checkpoint = None;
        let mut expected = Vec::new();
        for version in ["t0", "t1"] {
            std::fs::write(source.join("file"), version).unwrap();
            transport::git_output(
                transport::git(&source, None).await.unwrap(),
                &["add", "file"],
                vec![],
            )
            .await
            .unwrap();
            let handoff = root.join(version);
            let (descriptor, _) = super::super::repository::staging::capture(
                &source,
                &handoff,
                1024 * 1024,
                1024 * 1024,
            )
            .await
            .unwrap();
            let manifest = Manifest {
                version: 1,
                project: None,
                branch: None,
                repository_origin: None,
                repository: Some(super::super::repository::Snapshot {
                    head: None,
                    config: vec![],
                    staging: Some(descriptor),
                }),
                workspace_id: "w-fixture".into(),
                root: source.clone(),
                name: "Fixture".into(),
                epoch: 1,
                clean: true,
                continuation: execution::wire::Continuation::Idle,
                profile: Default::default(),
                sessions: vec![],
                left_out: Some(vec![]),
            };
            std::fs::write(
                handoff.join("manifest.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            let oid = mirror::commit_tree(&shadow, &handoff, "handoff")
                .await
                .unwrap();
            let working = mirror::commit_tree(
                &shadow,
                &tree(
                    &root.join(format!("working-{version}")),
                    &[("file", version), ("unchanged", "base")],
                ),
                "main",
            )
            .await
            .unwrap();
            if version == "t0" {
                copy_checkpoint = Some(execution::wire::Checkpoint {
                    id: "cp-fixture".into(),
                    sequence: 1,
                    source_holder_id: "d-fixture".into(),
                    source_epoch: 1,
                    working_tree_oid: working.clone(),
                    config_oid: working,
                    handoff_oid: oid.clone(),
                    continuation: execution::wire::Continuation::Idle,
                });
                acknowledged = Some(oid);
                expected = std::fs::read(handoff.join("git/index.json")).unwrap();
            }
        }
        let baseline = staging_baseline(&shadow, &root.join("baseline"), acknowledged, 1024 * 1024)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read(baseline.0.join("git/index.json")).unwrap(),
            expected
        );
        assert!(
            staging_baseline(&shadow, &root.join("unknown"), None, 1024 * 1024)
                .await
                .unwrap()
                .is_none()
        );
        assert!(staging_baseline(
            &shadow,
            &root.join("missing"),
            Some("a".repeat(40)),
            1024 * 1024
        )
        .await
        .unwrap()
        .is_none());
        let checkpoint = copy_checkpoint.unwrap();
        let takeover = root.join("takeover");
        let copied = takeover_copy_baseline(&shadow, &takeover, &checkpoint, 1024 * 1024)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read(copied.0.join("git/index.json")).unwrap(),
            expected
        );
        assert_eq!(
            std::fs::read(takeover.join("baseline/file")).unwrap(),
            b"t0"
        );
        let local = tree(
            &root.join("local-copy"),
            &[("file", "local edit"), ("unchanged", "base")],
        );
        let incoming = tree(
            &root.join("incoming-copy"),
            &[("file", "t0"), ("unchanged", "cloud edit")],
        );
        assert!(
            install_tree(&incoming, &local, Some(&takeover.join("baseline")), None)
                .unwrap()
                .1
                .is_empty()
        );
        assert_eq!(std::fs::read(local.join("file")).unwrap(), b"local edit");
        assert_eq!(
            std::fs::read(local.join("unchanged")).unwrap(),
            b"cloud edit"
        );
        let mut missing = checkpoint;
        missing.working_tree_oid = "b".repeat(40);
        assert!(
            takeover_copy_baseline(&shadow, &root.join("missing-copy"), &missing, 1024 * 1024)
                .await
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod pause_tests {
    use super::*;
    #[test]
    fn completed_chat_needs_no_vendor_status_but_active_work_stays_blocked() {
        let mut chat = chimaera_agent::ChatInfo {
            id: "s-codex".into(),
            agent: "codex".into(),
            cwd: "/tmp".into(),
            created_at_ms: 0,
            alive: true,
            exit_status: None,
            native_session_id: None,
            model: None,
            current_mode: None,
            pending_permission: false,
            status_detail: None,
            status_category: None,
            status_needs_action: false,
            remote_control_url: None,
            background_running: 0,
        };
        let mut carry = chimaera_agent::Carryover::default();
        let finished = Some(crate::agent_state::AgentState::Finished);
        assert!(chat_at_pause(&chat, Some(&carry), false, finished));
        assert!(!chat_at_pause(&chat, Some(&carry), true, finished));
        carry.turn_in_flight = true;
        assert!(!chat_at_pause(&chat, Some(&carry), false, finished));
        carry.turn_in_flight = false;
        chat.background_running = 1;
        assert!(!chat_at_pause(&chat, Some(&carry), false, finished));
        chat.background_running = 0;
        assert!(!chat_at_pause(
            &chat,
            Some(&carry),
            false,
            Some(crate::agent_state::AgentState::Running)
        ));
        assert!(!chat_at_pause(&chat, None, false, finished));
        chat.pending_permission = true;
        carry.turn_in_flight = true;
        assert!(chat_at_pause(
            &chat,
            Some(&carry),
            false,
            Some(crate::agent_state::AgentState::NeedsPermission)
        ));
        assert!(!chat_at_pause(&chat, Some(&carry), true, finished));
    }
}

fn continuation(state: &AppState, workspace: &str) -> execution::wire::Continuation {
    use execution::wire::Continuation;
    let mut result = Continuation::Idle;
    for agent in crate::ledger::snapshot(state)
        .0
        .into_iter()
        .filter(|entry| entry.workspace_id == workspace)
        .filter_map(|entry| entry.agent)
    {
        match agent.carryover {
            None => return Continuation::Uncertain,
            Some(carry) if carry.interrupted_work() => result = Continuation::Interrupted,
            _ => {}
        }
    }
    result
}
