//! Keeper cluster control and exact job routes. No SSH or reconnect lives here.
use super::*;
use chimaera_link::{
    Client, ClusterOperation as Op, ClusterOperationState, ClusterReply as Reply, ClusterSnapshot,
    Host, LinkTunnel,
};
use chimaera_remote::hosts::HostsStore;
use std::sync::{Arc, LazyLock};

const LINK_MAX: usize = 128;
const EFFECT_MAX: usize = 16;
const PENDING_MAX: usize = 32;
static PENDING: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(PENDING_MAX)));
static EFFECTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(EFFECT_MAX)));
const CHANGED: &str = "Account changed while using this cluster";
const UNAVAILABLE: &str = "This cluster connection needs Connect or Reconnect. It cannot authenticate from a job action or refresh.";
const UNCERTAIN: &str = "The cluster hasn't confirmed the previous operation. Refresh before trying again; it will not be submitted twice.";

#[derive(Default)]
pub(super) struct Control {
    // Failed responses retain only native memory. Never create another operation
    // for an unresolved submission: GET refers to this exact original record.
    pending: tokio::sync::Mutex<Vec<Pending>>,
}
struct Pending {
    operation: Op,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
#[derive(Clone)]
pub(super) struct Selected {
    client: Client,
    host: Host,
    generation: u64,
    control: Arc<Control>,
}

pub(super) struct Held {
    host: String,
    generation: u64,
    job: String,
    workspace: Option<String>,
    token: String,
    tunnel: LinkTunnel,
}
impl Held {
    fn matches(
        &self,
        selected: &Selected,
        job: &str,
        workspace: Option<&str>,
        token: &str,
    ) -> bool {
        self.host == selected.host.id
            && self.generation == selected.generation
            && self.job == job
            && self.workspace.as_deref() == workspace
            && self.token == token
    }
}

pub(super) fn close_all(shell: &Shell) {
    for cluster in lock(&shell.clusters).values_mut() {
        // Pending opens are fenced by account generation before publication.
        cluster.kept_link_epoch = cluster.kept_link_epoch.saturating_add(1);
        cluster.kept_links.clear();
        retire_control(cluster);
    }
}
fn retire_control(cluster: &mut ClusterLive) {
    // Settled uncertainty belongs to the old account. In-flight owned tasks
    // retain their old Arc and its permits until their actual work settles.
    cluster.kept_control = Arc::new(Control::default());
    cluster.kept_identity = None;
}
pub(super) fn close_key(shell: &Shell, key: &str) {
    for cluster in lock(&shell.clusters).values_mut() {
        if cluster.kept_links.contains_key(key) || cluster.kept_opening.contains(key) {
            cluster.kept_link_epoch = cluster.kept_link_epoch.saturating_add(1);
            cluster.kept_links.remove(key);
        }
    }
}
pub(super) fn reconcile(shell: &Shell, alias: &str, ov: &ClusterOverview) {
    let generation = shell.pro.generation();
    if let Some(cluster) = lock(&shell.clusters).get_mut(alias) {
        if cluster.endpoints != ov.endpoints || cluster.hosts != ov.hosts {
            cluster.kept_link_epoch = cluster.kept_link_epoch.saturating_add(1);
        }
        cluster.kept_links.retain(|_, held| {
            if held.generation != generation {
                return false;
            }
            match held.workspace.as_deref() {
                Some(wid) => ov
                    .endpoints
                    .get(wid)
                    .is_some_and(|e| e.job == held.job && e.token == held.token),
                None => ov
                    .hosts
                    .get(&held.job)
                    .is_some_and(|e| e.token == held.token),
            }
        });
    }
}

pub(super) async fn select(shell: &Shell, alias: &str) -> Result<Option<Selected>, String> {
    let Some((client, host, generation)) = super::super::pro::connection(shell, alias).await?
    else {
        if let Some(c) = lock(&shell.clusters).get_mut(alias) {
            c.kept_link_epoch = c.kept_link_epoch.saturating_add(1);
            c.kept_links.clear();
            c.kept_identity = None;
        }
        return Ok(None);
    };
    let caps = client.cluster_capabilities().await.map_err(failure)?;
    if !caps.control_supported() {
        return Err("This keeper needs an update to manage cluster jobs".into());
    }
    if shell.pro.generation() != generation {
        return Err(CHANGED.into());
    }
    let control = {
        let mut clusters = lock(&shell.clusters);
        let c = clusters.entry(alias.to_string()).or_default();
        if c.kept_identity.as_ref() != Some(&(host.id.clone(), generation)) {
            c.kept_link_epoch = c.kept_link_epoch.saturating_add(1);
            c.kept_links.clear();
            c.kept_control = Arc::new(Control::default());
            c.kept_identity = Some((host.id.clone(), generation));
        }
        c.kept_control.clone()
    };
    Ok(Some(Selected {
        client,
        host,
        generation,
        control,
    }))
}
fn failure(error: anyhow::Error) -> String {
    if error.is::<chimaera_link::ServiceUnsupported>() {
        "This keeper needs an update to manage cluster jobs".into()
    } else if let Some(error) = error.downcast_ref::<chimaera_link::ClusterRequestError>() {
        error.to_string()
    } else {
        "Couldn't read the cluster through Chimaera Pro. Try again or reconnect.".into()
    }
}
impl Selected {
    pub(super) fn ensure_cluster(&self) -> Result<(), String> {
        let cluster = self.host.cluster.as_ref().ok_or(UNAVAILABLE)?;
        if cluster.scheduler != chimaera_link::ClusterScheduler::Slurm || cluster.not_cluster {
            return Err("This kept host is not an enabled Slurm cluster".into());
        }
        Ok(())
    }
    fn current(&self, shell: &Shell) -> Result<(), String> {
        if shell.pro.generation() == self.generation {
            Ok(())
        } else {
            Err(CHANGED.into())
        }
    }
    pub(super) async fn read(&self, shell: &Shell, operation: Op) -> Result<Reply, String> {
        self.current(shell)?;
        let reply = self
            .client
            .cluster_operation(&self.host.id, &operation)
            .await
            .map_err(failure)?;
        self.current(shell)?;
        Ok(reply)
    }
    pub(super) async fn effect(&self, app: &AppHandle, operation: Op) -> Result<Reply, String> {
        self.current(&app.state::<Shell>())?;
        let selected = self.clone();
        let app = app.clone();
        owned_effect(EFFECTS.clone(), async move {
            selected
                .settle(|| app.state::<Shell>().pro.generation(), operation)
                .await
        })
        .await
    }
    async fn settle(&self, generation: impl Fn() -> u64, operation: Op) -> Result<Reply, String> {
        operation
            .validate()
            .map_err(|_| "This cluster action has invalid or oversized input".to_string())?;
        if generation() != self.generation {
            return Err(CHANGED.into());
        }
        // One admitted effect per host. The budget remains with its owned task
        // through reply loss and cancellation, including queued network work.
        let until = tokio::time::Instant::now() + Duration::from_secs(90);
        let mut pending = tokio::time::timeout_at(until, self.control.pending.lock())
            .await
            .map_err(|_| UNCERTAIN.to_string())?;
        self.settle_locked(&generation, operation, until, &mut pending)
            .await
    }
    async fn settle_locked(
        &self,
        generation: &impl Fn() -> u64,
        operation: Op,
        until: tokio::time::Instant,
        pending: &mut Vec<Pending>,
    ) -> Result<Reply, String> {
        let conflict = pending.iter().position(|previous| match &operation {
            // Stopping another admitted job stays available even when a start
            // or configuration write has an uncertain receipt.
            Op::StopJob { job_id, .. } => {
                matches!(&previous.operation, Op::StopJob { job_id: old, .. } if old == job_id)
            }
            _ => true,
        });
        if let Some(index) = conflict {
            match tokio::time::timeout_at(
                until,
                self.client
                    .cluster_operation_state(&self.host.id, &pending[index].operation),
            )
            .await
            {
                Ok(Ok(ClusterOperationState::Completed { .. })) => {
                    pending.remove(index);
                }
                _ => return Err(UNCERTAIN.into()),
            }
            return Err(
                "The previous cluster operation finished. Refresh before trying again.".into(),
            );
        }
        if pending.len() >= 8 {
            return Err(
                "Too many unconfirmed cluster operations; reconnect to inspect their state".into(),
            );
        }
        if generation() != self.generation {
            return Err(CHANGED.into());
        }
        let index = pending.len();
        let retained = PENDING.clone().try_acquire_owned().map_err(|_| {
            "Too many unconfirmed cluster operations; inspect existing jobs before trying again"
                .to_string()
        })?;
        pending.push(Pending {
            operation: operation.clone(),
            _permit: retained,
        });
        let answer = tokio::time::timeout_at(
            until,
            self.client.cluster_operation(&self.host.id, &operation),
        )
        .await;
        let answer = match answer {
            Ok(Ok(reply)) => {
                pending.remove(index);
                Ok(reply)
            }
            Ok(Err(error))
                if error.is::<chimaera_link::ClusterRequestError>()
                    || error.is::<chimaera_link::ServiceUnsupported>() =>
            {
                pending.remove(index);
                Err(failure(error))
            }
            _ => loop {
                match tokio::time::timeout_at(
                    until,
                    self.client
                        .cluster_operation_state(&self.host.id, &operation),
                )
                .await
                {
                    Ok(Ok(ClusterOperationState::Completed { reply })) => {
                        pending.remove(index);
                        break Ok(*reply);
                    }
                    Ok(Ok(ClusterOperationState::Uncertain | ClusterOperationState::Unknown)) => {
                        break Err(UNCERTAIN.into());
                    }
                    _ if tokio::time::Instant::now() >= until => break Err(UNCERTAIN.into()),
                    _ => tokio::time::sleep(Duration::from_millis(500)).await,
                }
            },
        };
        if generation() != self.generation {
            return Err(CHANGED.into());
        }
        answer
    }
    pub(super) async fn overview(
        &self,
        shell: &Shell,
        refresh: bool,
    ) -> Result<ClusterOverview, String> {
        self.ensure_cluster()?;
        match self.read(shell, Op::Overview { refresh }).await? {
            Reply::Overview { overview } => convert(*overview),
            _ => Err("The keeper returned an unsupported cluster overview".into()),
        }
    }
    pub(super) async fn port(
        &self,
        shell: &Shell,
        alias: &str,
        key: &str,
        job: &str,
        workspace: Option<&str>,
        token: &str,
    ) -> Result<u16, String> {
        self.current(shell)?;
        let reservation = {
            let mut clusters = lock(&shell.clusters);
            let count: usize = clusters
                .values()
                .map(|c| c.kept_links.len() + c.kept_opening.len())
                .sum();
            let c = clusters.entry(alias.to_string()).or_default();
            if let Some(held) = c.kept_links.get(key) {
                if held.matches(self, job, workspace, token) {
                    return Ok(held.tunnel.local_port);
                }
            }
            c.kept_links.remove(key);
            if count >= LINK_MAX {
                return Err("Too many open cluster connections".into());
            }
            if !c.kept_opening.insert(key.to_string()) {
                return Err("This cluster connection is already opening".into());
            }
            Opening {
                shell,
                alias,
                key,
                epoch: c.kept_link_epoch,
            }
        };
        let until = tokio::time::Instant::now() + Duration::from_secs(45);
        if workspace.is_some() {
            // A job-host Open can finish before the keeper observes its new
            // workspace daemon. This explicit action prepares only existing
            // resources and must prove the exact route before opening a window.
            let overview = tokio::time::timeout_at(until, self.overview(shell, true))
                .await
                .map_err(|_| "The keeper is still preparing this workspace route".to_string())??;
            if !route_matches(&overview, job, workspace, token) {
                return Err(
                    "The keeper hasn't confirmed this workspace route yet; try Open again shortly"
                        .into(),
                );
            }
        }
        let tunnel = tokio::time::timeout_at(
            until,
            LinkTunnel::bind_cluster(
                self.client.clone(),
                self.host.id.clone(),
                job.to_string(),
                workspace.map(str::to_string),
            ),
        )
        .await
        .map_err(|_| "This cluster connection took too long to open".to_string())?
        .map_err(failure)?;
        if workspace.is_some()
            && !tokio::time::timeout_at(
                until,
                chimaera_remote::http_alive_authed(tunnel.local_port, token),
            )
            .await
            .unwrap_or(false)
        {
            return Err(
                "The keeper workspace route isn't responding yet; try Open again shortly".into(),
            );
        }
        self.current(shell)?;
        let port = tunnel.local_port;
        let mut clusters = lock(&shell.clusters);
        let c = clusters.get_mut(alias).ok_or(CHANGED)?;
        if !publishable(c, self, reservation.epoch) {
            return Err("This cluster connection was closed while opening".into());
        }
        c.kept_links.insert(
            key.to_string(),
            Held {
                host: self.host.id.clone(),
                generation: self.generation,
                job: job.to_string(),
                workspace: workspace.map(str::to_string),
                token: token.to_string(),
                tunnel,
            },
        );
        drop(clusters);
        drop(reservation);
        Ok(port)
    }
    pub(super) async fn policy(
        &self,
        app: &AppHandle,
        alias: &str,
        change: PolicyChange,
    ) -> Result<chimaera_remote::hosts::HostEntry, String> {
        self.current(&app.state::<Shell>())?;
        let selected = self.clone();
        let app = app.clone();
        let alias = alias.to_string();
        owned_effect(EFFECTS.clone(), async move {
            let (login_serve, not_cluster, _policy) = selected
                .policy_settle(|| app.state::<Shell>().pro.generation(), change)
                .await?;
            let state = app.state::<Shell>();
            let authority = policy_authority(&state.pro.operation, || {
                state.pro.generation() == selected.generation
            })
            .await?;
            let retained = authority.clone();
            let write_app = app.clone();
            let saved_alias = alias.clone();
            let entry = with_hosts(move |hosts| {
                // This clone remains in the actual filesystem worker, even if
                // its observer is aborted while the home directory is stalled.
                let _authority = retained;
                anyhow::ensure!(
                    write_app.state::<Shell>().pro.generation() == selected.generation,
                    CHANGED
                );
                save_policy(hosts, &saved_alias, login_serve, not_cluster)
            })
            .await?;
            lock(&state.host_entries).insert(alias, entry.clone());
            drop(authority);
            Ok(entry)
        })
        .await
    }
    async fn policy_settle(
        &self,
        generation: impl Fn() -> u64,
        change: PolicyChange,
    ) -> Result<(bool, bool, tokio::sync::MutexGuard<'_, Vec<Pending>>), String> {
        let until = tokio::time::Instant::now() + Duration::from_secs(90);
        let mut pending = tokio::time::timeout_at(until, self.control.pending.lock())
            .await
            .map_err(|_| UNCERTAIN.to_string())?;
        if generation() != self.generation {
            return Err(CHANGED.into());
        }
        // Read + derive + effect share the same per-host owner. Independent
        // toggle commands must preserve the other flag's latest accepted value.
        let hosts = tokio::time::timeout_at(until, self.client.hosts())
            .await
            .map_err(|_| UNCERTAIN.to_string())?
            .map_err(failure)?;
        let host = hosts
            .into_iter()
            .find(|h| h.id == self.host.id)
            .filter(|h| h.kind == chimaera_link::HostKind::Ssh && h.alias == self.host.alias)
            .ok_or(UNAVAILABLE)?;
        let cluster = host.cluster.ok_or(UNAVAILABLE)?;
        let (login_serve, not_cluster) = match change {
            PolicyChange::LoginServe(on) => (on, cluster.not_cluster),
            // Existing native intent: marking a host noncluster replaces the
            // warned login-node override. Clearing it preserves the latest flag.
            PolicyChange::NotCluster(on) => (if on { false } else { cluster.login_serve }, on),
        };
        let reply = self
            .settle_locked(
                &generation,
                Op::SetPolicy {
                    operation_id: operation_id(),
                    login_serve,
                    not_cluster,
                },
                until,
                &mut pending,
            )
            .await?;
        match reply {
            Reply::Host { host }
                if host.cluster.as_ref().is_some_and(|c| {
                    c.login_serve == login_serve && c.not_cluster == not_cluster
                }) =>
            {
                Ok((login_serve, not_cluster, pending))
            }
            _ => Err("The keeper didn't confirm the cluster policy".into()),
        }
    }
}
fn save_policy(
    hosts: &mut HostsStore,
    alias: &str,
    login_serve: bool,
    not_cluster: bool,
) -> anyhow::Result<chimaera_remote::hosts::HostEntry> {
    // The legacy noncluster setter clears the override. Apply it first so the
    // local saved pair mirrors the exact policy acknowledged by the keeper.
    hosts.set_not_cluster(alias, not_cluster)?;
    hosts.set_login_serve(alias, login_serve)
}

#[derive(Clone, Copy)]
pub(super) enum PolicyChange {
    LoginServe(bool),
    NotCluster(bool),
}
async fn policy_authority(
    operation: &Arc<tokio::sync::Mutex<()>>,
    current: impl Fn() -> bool,
) -> Result<Arc<tokio::sync::OwnedMutexGuard<()>>, String> {
    let owner = operation.clone().lock_owned().await;
    if !current() {
        return Err(CHANGED.into());
    }
    Ok(Arc::new(owner))
}
async fn owned_effect<F, T>(budget: Arc<tokio::sync::Semaphore>, work: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>> + Send + 'static,
    T: Send + 'static,
{
    let permit = budget
        .try_acquire_owned()
        .map_err(|_| "Too many cluster operations; wait for one to finish".to_string())?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tauri::async_runtime::spawn(async move {
        let _permit = permit;
        let _ = sender.send(work.await);
    });
    receiver
        .await
        .map_err(|_| "The cluster operation ended before it was confirmed".to_string())?
}

struct Opening<'a> {
    shell: &'a Shell,
    alias: &'a str,
    key: &'a str,
    epoch: u64,
}
impl Drop for Opening<'_> {
    fn drop(&mut self) {
        if let Some(c) = lock(&self.shell.clusters).get_mut(self.alias) {
            c.kept_opening.remove(self.key);
        }
    }
}
pub(super) fn operation_id() -> String {
    chimaera_core::generate_token()
}

fn convert(snapshot: ClusterSnapshot) -> Result<ClusterOverview, String> {
    use chimaera_link::{ClusterJobState as J, ClusterScheduler as S, ClusterWorkspaceState as W};
    if snapshot.scheduler != S::Slurm {
        return Err("This keeper doesn't support the cluster scheduler".into());
    }
    let mut overview = ClusterOverview {
        scheduler: chimaera_core::slurm::Scheduler::Slurm,
        login_node: snapshot.login_node,
        home: snapshot.home,
        now_ms: snapshot.now_ms,
        degraded: snapshot.degraded,
        queue_at_ms: snapshot.queue_at_ms,
        config: snapshot.config,
        config_sum: snapshot.config_sum,
        state_unreadable: snapshot.state_unreadable,
        other_jobs: cluster::OtherJobs {
            running: snapshot.other_jobs.running,
            waiting: snapshot.other_jobs.waiting,
        },
        startup: cluster::StartupView {
            cluster: snapshot.startup.cluster,
            workspaces: snapshot.startup.workspaces,
        },
        records: snapshot.records.into_iter().collect(),
        jobs: snapshot
            .jobs
            .into_iter()
            .map(|j| cluster::JobView {
                id: j.id,
                name: j.name,
                state: match j.state {
                    J::Waiting => "waiting",
                    J::Starting => "starting",
                    J::Running => "running",
                    J::Ended => "ended",
                    J::Unknown => "unknown",
                },
                slurm_job_id: j.slurm_job_id,
                node: j.node,
                partition: j.partition,
                cpus: j.cpus,
                mem: j.mem,
                gpus: j.gpus,
                ends_at_ms: j.ends_at_ms,
                reason: j.reason,
                attached: j.attached,
                ended: j.ended,
                ended_at_ms: j.ended_at_ms,
                stopped_by_user: j.stopped_by_user,
                stopping: j.stopping,
                egress: j.egress,
                open: j.open,
                replaces: j.replaces,
                spec: j.spec,
                startup: j.startup,
                submitted_ms: j.submitted_ms,
            })
            .collect(),
        workspaces: snapshot
            .workspaces
            .into_iter()
            .map(|w| cluster::WorkspaceView {
                id: w.id,
                name: w.name,
                path: w.path,
                state: match w.state {
                    W::Open => "open",
                    W::Queued => "queued",
                    W::Closed => "closed",
                    W::Unknown => "unknown",
                },
                job: w.job,
                last_open_ms: w.last_open_ms,
                opening: w.opening,
                closing: w.closing,
                failed: w.failed,
                working: w.working,
            })
            .collect(),
        ..Default::default()
    };
    for route in snapshot.routes {
        let job = overview
            .jobs
            .iter()
            .find(|j| j.id == route.job_id)
            .ok_or("The keeper route names an unknown job")?;
        if job.state != "running" || job.stopping {
            return Err("The keeper route names a job that isn't running".into());
        }
        let slurm = job
            .slurm_job_id
            .clone()
            .ok_or("The keeper route has no scheduler identity")?;
        // A Link route has no SSH destination/port. These values are retained
        // only for the existing window/notification vocabulary, never dialed.
        match route.workspace_id {
            Some(wid) => {
                if !overview.workspaces.iter().any(|w| {
                    w.id == wid && w.job.as_deref() == Some(job.id.as_str()) && w.state == "open"
                }) {
                    return Err("The keeper route doesn't match its open workspace".into());
                }
                if overview
                    .endpoints
                    .insert(
                        wid,
                        cluster::Endpoint {
                            job: job.id.clone(),
                            slurm_job_id: slurm,
                            node: job.node.clone(),
                            port: 0,
                            token: route.daemon.token,
                        },
                    )
                    .is_some()
                {
                    return Err("The keeper repeated a workspace route".into());
                }
            }
            None => {
                if overview
                    .hosts
                    .insert(
                        job.id.clone(),
                        cluster::HostEndpoint {
                            job: job.id.clone(),
                            slurm_job_id: slurm,
                            node: job.node.clone(),
                            port: 0,
                            token: route.daemon.token,
                        },
                    )
                    .is_some()
                {
                    return Err("The keeper repeated a job route".into());
                }
            }
        }
    }
    Ok(overview)
}

/// The existing health loop only probes these exact generation-fenced endpoints.
pub(super) fn endpoints(shell: &Shell) -> Vec<(String, u16, String)> {
    let generation = shell.pro.generation();
    lock(&shell.clusters)
        .values()
        .flat_map(|c| c.kept_links.iter())
        .filter(|(_, held)| held.generation == generation)
        .map(|(key, held)| (key.clone(), held.tunnel.local_port, held.token.clone()))
        .collect()
}
pub(super) fn current(shell: &Shell, key: &str, port: u16, token: &str) -> bool {
    let generation = shell.pro.generation();
    lock(&shell.clusters).values().any(|c| {
        c.kept_links.get(key).is_some_and(|held| {
            held.generation == generation && held.tunnel.local_port == port && held.token == token
        })
    })
}

fn publishable(cluster: &ClusterLive, selected: &Selected, epoch: u64) -> bool {
    epoch != u64::MAX
        && cluster.kept_link_epoch == epoch
        && cluster.kept_identity.as_ref() == Some(&(selected.host.id.clone(), selected.generation))
}

fn route_matches(
    overview: &ClusterOverview,
    job: &str,
    workspace: Option<&str>,
    token: &str,
) -> bool {
    match workspace {
        Some(wid) => overview
            .endpoints
            .get(wid)
            .is_some_and(|endpoint| endpoint.job == job && endpoint.token == token),
        None => overview
            .hosts
            .get(job)
            .is_some_and(|endpoint| endpoint.token == token),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chimaera_core::{cluster::new_job_id, slurm::LaunchSpec};
    use chimaera_link::{
        ClusterCapabilities, ClusterJobState, ClusterJobView, ClusterRoute, ClusterScheduler,
        Daemon, fake::FakeKeeper,
    };
    struct Fixture {
        keeper: FakeKeeper,
        task: tokio::task::JoinHandle<()>,
    }
    impl Fixture {
        async fn new() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let keeper = FakeKeeper::new(format!("http://{}", listener.local_addr().unwrap()));
            let router = keeper.router();
            Self {
                keeper,
                task: tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
            }
        }
        async fn selected(&self) -> Selected {
            let host = self
                .keeper
                .add_cluster_target("cluster", ClusterSnapshot::default())
                .await
                .unwrap();
            Selected {
                client: Client::new(&self.keeper.endpoint, Some(FakeKeeper::tokens())).unwrap(),
                host,
                generation: 7,
                control: Arc::new(Control::default()),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    fn start() -> Op {
        Op::StartJob {
            operation_id: operation_id(),
            job_id: new_job_id(),
            name: None,
            spec: LaunchSpec {
                time: "1:00".into(),
                ..Default::default()
            },
            open: vec![],
            startup: String::new(),
            attached: false,
            replaces: None,
            save_as: None,
        }
    }
    #[tokio::test]
    async fn concurrent_policy_intents_derive_flags_under_the_same_owner() {
        for (first, second, expected) in [
            (
                PolicyChange::LoginServe(true),
                PolicyChange::NotCluster(false),
                (true, false),
            ),
            (
                PolicyChange::NotCluster(true),
                PolicyChange::LoginServe(true),
                (true, true),
            ),
            (
                PolicyChange::LoginServe(true),
                PolicyChange::NotCluster(true),
                (false, true),
            ),
        ] {
            let fixture = Fixture::new().await;
            let selected = fixture.selected().await;
            let stale = selected.clone();
            let (_, _, owner) = selected.policy_settle(|| 7, first).await.unwrap();
            let second = tokio::spawn(async move {
                let (login, other, _owner) = stale.policy_settle(|| 7, second).await?;
                Ok::<_, String>((login, other))
            });
            tokio::task::yield_now().await;
            assert!(!second.is_finished());
            drop(owner);
            assert_eq!(second.await.unwrap().unwrap(), expected);
            let hosts = selected.client.hosts().await.unwrap();
            let host = hosts
                .into_iter()
                .find(|h| h.id == selected.host.id)
                .unwrap();
            let policy = host.cluster.unwrap();
            assert_eq!((policy.login_serve, policy.not_cluster), expected);
            let root = std::env::temp_dir().join(format!("chimaera-policy-{}", operation_id()));
            std::fs::create_dir(&root).unwrap();
            let path = root.join("hosts.json");
            let mut hosts = HostsStore::load(path.clone());
            let entry = save_policy(&mut hosts, "cluster", expected.0, expected.1).unwrap();
            assert_eq!((entry.login_serve, entry.not_cluster), expected);
            let restored = HostsStore::load(path).get("cluster").unwrap();
            assert_eq!((restored.login_serve, restored.not_cluster), expected);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[tokio::test]
    async fn policy_landing_fences_account_change_and_retains_real_blocking_owner() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let operation = Arc::new(tokio::sync::Mutex::new(()));
        let held = operation.clone().lock_owned().await;
        let current = Arc::new(AtomicBool::new(true));
        let waiting = tokio::spawn({
            let operation = operation.clone();
            let current = current.clone();
            async move { policy_authority(&operation, || current.load(Ordering::SeqCst)).await }
        });
        current.store(false, Ordering::SeqCst);
        drop(held);
        assert!(waiting.await.unwrap().is_err());
        assert!(operation.try_lock().is_ok());
        let authority = policy_authority(&operation, || true).await.unwrap();
        let retained = authority.clone();
        let (ready, entered) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        let observer = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                let _authority = retained;
                let _ = ready.send(());
                released.recv().unwrap();
                let _ = done.send(());
            })
            .await
            .unwrap();
        });
        entered.await.unwrap();
        drop(authority);
        observer.abort();
        let _ = observer.await;
        assert!(operation.try_lock().is_err());
        release.send(()).unwrap();
        finished.await.unwrap();
        let _owner = tokio::time::timeout(Duration::from_secs(5), operation.lock())
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn account_stop_retires_settled_pending_budget_but_not_active_owners() {
        let budget = Arc::new(tokio::sync::Semaphore::new(32));
        let mut clusters = Vec::new();
        for i in 0..4 {
            let cluster = ClusterLive {
                kept_identity: Some((format!("old-{i}"), 7)),
                ..Default::default()
            };
            for _ in 0..8 {
                cluster.kept_control.pending.lock().await.push(Pending {
                    operation: start(),
                    _permit: budget.clone().try_acquire_owned().unwrap(),
                });
            }
            clusters.push(cluster);
        }
        assert_eq!(budget.available_permits(), 0);
        let actual_owner = clusters[0].kept_control.clone();
        for cluster in &mut clusters {
            retire_control(cluster);
            assert!(cluster.kept_identity.is_none());
            assert!(cluster.kept_control.pending.lock().await.is_empty());
        }
        assert_eq!(budget.available_permits(), 24);
        assert_eq!(actual_owner.pending.lock().await.len(), 8);
        let new_account = budget.clone().try_acquire_owned().unwrap();
        drop(actual_owner);
        assert_eq!(budget.available_permits(), 31);
        drop(new_account);
        assert_eq!(budget.available_permits(), 32);
    }
    #[tokio::test]
    async fn lost_reply_reconciles_exact_original_job_without_resubmission() {
        let fixture = Fixture::new().await;
        let selected = fixture.selected().await;
        fixture
            .keeper
            .set_cluster_reply(&selected.host.id, Some(json!({"result":"unknown_future"})))
            .await;
        let operation = start();
        let expected = match &operation {
            Op::StartJob { job_id, .. } => job_id.clone(),
            _ => unreachable!(),
        };
        let reply = selected.settle(|| 7, operation).await.unwrap();
        assert!(matches!(reply,Reply::Job {job_id,..} if job_id==expected));
        assert_eq!(
            fixture.keeper.cluster_submissions(&selected.host.id).await,
            1
        );
        assert!(selected.control.pending.lock().await.is_empty());
    }
    #[tokio::test]
    async fn uncertain_start_blocks_new_submission_but_allows_stop_and_generation_fails_closed() {
        let fixture = Fixture::new().await;
        let selected = fixture.selected().await;
        let operation = start();
        let job = match &operation {
            Op::StartJob { job_id, .. } => job_id.clone(),
            _ => unreachable!(),
        };
        selected
            .client
            .cluster_operation(&selected.host.id, &operation)
            .await
            .unwrap();
        fixture
            .keeper
            .set_cluster_operation_state(
                &selected.host.id,
                operation.operation_id().unwrap(),
                ClusterOperationState::Uncertain,
            )
            .await;
        selected.control.pending.lock().await.push(Pending {
            operation,
            _permit: PENDING.clone().acquire_owned().await.unwrap(),
        });
        assert!(selected.settle(|| 7, start()).await.is_err());
        assert_eq!(
            fixture.keeper.cluster_submissions(&selected.host.id).await,
            1
        );
        assert!(
            selected
                .settle(
                    || 8,
                    Op::StopJob {
                        operation_id: operation_id(),
                        job_id: job.clone()
                    }
                )
                .await
                .is_err()
        );
        selected
            .settle(
                || 7,
                Op::StopJob {
                    operation_id: operation_id(),
                    job_id: job,
                },
            )
            .await
            .unwrap();
        assert_eq!(selected.control.pending.lock().await.len(), 1);
    }
    #[tokio::test]
    async fn missing_capability_refuses_effect_and_never_submits() {
        let fixture = Fixture::new().await;
        let selected = fixture.selected().await;
        fixture
            .keeper
            .set_cluster_capabilities(Some(ClusterCapabilities {
                version: 1,
                cluster_control_v1: true,
                job_tunnels_v1: false,
            }))
            .await;
        assert!(selected.settle(|| 7, start()).await.is_err());
        assert_eq!(
            fixture.keeper.cluster_submissions(&selected.host.id).await,
            0
        );
        assert!(selected.control.pending.lock().await.is_empty());
    }
    #[tokio::test]
    async fn cancelled_observer_retains_actual_owned_settlement_and_admission_until_reply() {
        let fixture = Fixture::new().await;
        let selected = fixture.selected().await;
        let operation = start();
        let host = selected.host.id.clone();
        let budget = Arc::new(tokio::sync::Semaphore::new(1));
        let entered = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let finish = Arc::new(tokio::sync::Semaphore::new(0));
        let control = selected.control.clone();
        let observer = tokio::spawn({
            let budget = budget.clone();
            let entered = entered.clone();
            let release = release.clone();
            let finish = finish.clone();
            async move {
                owned_effect(budget, async move {
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    let result = selected.settle(|| 7, operation).await;
                    finish.add_permits(1);
                    result
                })
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(5), entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        observer.abort();
        let _ = observer.await;
        assert_eq!(budget.available_permits(), 0);
        assert!(budget.clone().try_acquire_owned().is_err());
        assert_eq!(fixture.keeper.cluster_submissions(&host).await, 0);
        release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), finish.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        tokio::time::timeout(Duration::from_secs(5), async {
            while budget.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fixture.keeper.cluster_submissions(&host).await, 1);
        assert!(control.pending.lock().await.is_empty());
    }
    #[tokio::test]
    async fn invalid_input_is_refused_before_retaining_operation_or_effect() {
        let fixture = Fixture::new().await;
        let selected = fixture.selected().await;
        let mut operation = start();
        if let Op::StartJob { startup, .. } = &mut operation {
            *startup = "x".repeat(32 * 1024 + 1);
        }
        assert!(selected.settle(|| 7, operation).await.is_err());
        assert!(selected.control.pending.lock().await.is_empty());
        assert_eq!(
            fixture.keeper.cluster_submissions(&selected.host.id).await,
            0
        );
    }
    #[test]
    fn workspace_route_proof_cannot_retarget_job_workspace_or_bearer() {
        let job = new_job_id();
        let wid = "w-12345678";
        let mut overview = ClusterOverview::default();
        overview.endpoints.insert(
            wid.into(),
            cluster::Endpoint {
                job: job.clone(),
                slurm_job_id: "42".into(),
                node: "compute".into(),
                port: 0,
                token: "private-route".into(),
            },
        );
        assert!(route_matches(&overview, &job, Some(wid), "private-route"));
        assert!(!route_matches(
            &overview,
            &new_job_id(),
            Some(wid),
            "private-route"
        ));
        assert!(!route_matches(
            &overview,
            &job,
            Some("w-87654321"),
            "private-route"
        ));
        assert!(!route_matches(&overview, &job, Some(wid), "changed"));
        assert!(!route_matches(&overview, &job, None, "private-route"));
    }
    #[tokio::test]
    async fn exact_scope_and_close_epoch_fence_pending_route_publication() {
        let fixture = Fixture::new().await;
        let selected = fixture.selected().await;
        let mut cluster = ClusterLive {
            kept_identity: Some((selected.host.id.clone(), 7)),
            ..Default::default()
        };
        let job = new_job_id();
        let tunnel = LinkTunnel::bind_cluster(
            selected.client.clone(),
            selected.host.id.clone(),
            job.clone(),
            None,
        )
        .await
        .unwrap();
        let held = Held {
            host: selected.host.id.clone(),
            generation: 7,
            job: job.clone(),
            workspace: None,
            token: "private-route-token".into(),
            tunnel,
        };
        assert!(held.matches(&selected, &job, None, "private-route-token"));
        assert!(!held.matches(
            &selected,
            &job,
            Some("w_0000000000000000"),
            "private-route-token"
        ));
        assert!(!held.matches(&selected, &new_job_id(), None, "private-route-token"));
        assert!(!held.matches(&selected, &job, None, "changed"));
        let mut other = selected.clone();
        other.generation += 1;
        assert!(!held.matches(&other, &job, None, "private-route-token"));
        assert!(publishable(&cluster, &selected, 0));
        cluster.kept_link_epoch += 1;
        assert!(!publishable(&cluster, &selected, 0));
        cluster.kept_link_epoch = u64::MAX;
        assert!(!publishable(&cluster, &selected, u64::MAX));
        cluster.kept_identity = Some(("other-host".into(), 7));
        assert!(!publishable(&cluster, &selected, 1));
        let port = held.tunnel.local_port;
        held.tunnel.close();
        tokio::task::yield_now().await;
        assert!(
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .is_err()
        );
    }
    #[test]
    fn typed_overview_keeps_credentials_native_and_unknown_states_uncertain() {
        let job = new_job_id();
        let mut snapshot = ClusterSnapshot::default();
        snapshot.jobs.push(ClusterJobView {
            id: job.clone(),
            name: "allocation".into(),
            state: ClusterJobState::Running,
            slurm_job_id: Some("42".into()),
            node: "compute".into(),
            partition: String::new(),
            cpus: String::new(),
            mem: String::new(),
            gpus: None,
            ends_at_ms: None,
            reason: String::new(),
            attached: true,
            ended: None,
            ended_at_ms: None,
            stopped_by_user: false,
            stopping: false,
            egress: None,
            open: vec![],
            replaces: None,
            spec: LaunchSpec::default(),
            startup: String::new(),
            submitted_ms: 1,
        });
        snapshot.routes.push(ClusterRoute {
            job_id: job.clone(),
            workspace_id: None,
            daemon: Daemon {
                token: "private-route-token".into(),
                build: "build".into(),
                sessions: 0,
            },
        });
        let overview = convert(snapshot.clone()).unwrap();
        assert_eq!(overview.hosts[&job].token, "private-route-token");
        assert_eq!(overview.hosts[&job].port, 0);
        let wire = serde_json::to_string(&overview).unwrap();
        assert!(!wire.contains("private-route-token"));
        assert!(!wire.contains("routes"));
        snapshot.jobs[0].stopping = true;
        assert!(convert(snapshot.clone()).is_err());
        let mut stopping = snapshot.clone();
        stopping.routes.clear();
        assert!(convert(stopping).unwrap().jobs[0].stopping);
        snapshot.jobs[0].stopping = false;
        snapshot.routes.push(snapshot.routes[0].clone());
        assert!(convert(snapshot.clone()).is_err());
        snapshot.routes.clear();
        snapshot.jobs[0].state = ClusterJobState::Unknown;
        assert_eq!(convert(snapshot.clone()).unwrap().jobs[0].state, "unknown");
        snapshot.routes.push(ClusterRoute {
            job_id: job,
            workspace_id: None,
            daemon: Daemon {
                token: "private-route-token".into(),
                build: "build".into(),
                sessions: 0,
            },
        });
        assert!(convert(snapshot).is_err());
        let wrong = ClusterSnapshot {
            scheduler: ClusterScheduler::Unknown,
            ..Default::default()
        };
        assert!(convert(wrong).is_err());
    }
}
