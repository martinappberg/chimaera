//! Deterministic cluster contract fixture, not an SSH/Slurm implementation.
use crate::{fake::FakeKeeper, *};
use axum::{
    extract::{DefaultBodyLimit, Extension, Path, State, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chimaera_core::{
    cluster::{ClusterWorkspace, JobRecord},
    slurm::Scheduler,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    net::{Ipv4Addr, SocketAddr},
};
use tokio::sync::{watch, Mutex};

#[derive(Default)]
pub(crate) struct FixtureCluster {
    data: Mutex<Data>,
}
#[derive(Default)]
struct Data {
    capabilities: Option<ClusterCapabilities>,
    hosts: HashMap<String, FixtureHost>,
}
struct FixtureHost {
    snapshot: ClusterSnapshot,
    host: Host,
    operations: HashMap<String, Operation>,
    targets: HashMap<(String, Option<String>), Target>,
    submissions: usize,
    reply_override: Option<serde_json::Value>,
    batch_refusal: Option<BatchRefusalKind>,
}
struct Target {
    address: SocketAddr,
    stop: watch::Sender<bool>,
}
struct Operation {
    hash: [u8; 32],
    state: ClusterOperationState,
}
impl FixtureCluster {
    pub(crate) async fn add_host(&self, host: Host, snapshot: ClusterSnapshot) {
        let mut data = self.data.lock().await;
        data.capabilities = Some(ClusterCapabilities {
            version: 1,
            cluster_control_v1: true,
            job_tunnels_v1: true,
        });
        data.hosts.insert(
            host.id.clone(),
            FixtureHost {
                snapshot,
                host,
                operations: HashMap::new(),
                targets: HashMap::new(),
                submissions: 0,
                reply_override: None,
                batch_refusal: None,
            },
        );
    }
    pub(crate) async fn capabilities(&self, value: Option<ClusterCapabilities>) {
        self.data.lock().await.capabilities = value;
    }
    pub(crate) async fn target(
        &self,
        host: &str,
        job: &str,
        workspace: Option<&str>,
        address: SocketAddr,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            address.ip() == Ipv4Addr::LOCALHOST,
            "fixture target requires loopback"
        );
        let mut data = self.data.lock().await;
        let host = data
            .hosts
            .get_mut(host)
            .ok_or_else(|| anyhow::anyhow!("unknown fixture cluster"))?;
        anyhow::ensure!(
            host.snapshot.records.contains_key(job),
            "unknown fixture job"
        );
        if let Some(id) = workspace {
            anyhow::ensure!(
                chimaera_core::cluster::valid_workspace_id(id)
                    && host.snapshot.workspaces.iter().any(|w| w.id == id)
                    && host.snapshot.records[job].open.iter().any(|w| w == id),
                "unknown fixture workspace"
            );
        }
        anyhow::ensure!(
            host.snapshot.records[job].ended.is_none(),
            "fixture job ended"
        );
        if let Some(id) = workspace {
            host.targets.retain(|(old_job, old_workspace), old| {
                let moved = old_job != job && old_workspace.as_deref() == Some(id);
                if moved {
                    let _ = old.stop.send(true);
                }
                !moved
            });
        }
        let key = (job.into(), workspace.map(str::to_owned));
        if let Some(old) = host.targets.insert(
            key,
            Target {
                address,
                stop: watch::channel(false).0,
            },
        ) {
            let _ = old.stop.send(true);
        }
        if let Some(view) = host.snapshot.jobs.iter_mut().find(|v| v.id == job) {
            view.state = ClusterJobState::Running;
        }
        if let Some(id) = workspace {
            host.snapshot
                .routes
                .retain(|r| r.workspace_id.as_deref() != Some(id));
            if let Some(view) = host.snapshot.workspaces.iter_mut().find(|v| v.id == id) {
                view.job = Some(job.into());
                view.state = ClusterWorkspaceState::Open;
            }
        }
        host.snapshot
            .routes
            .retain(|r| !(r.job_id == job && r.workspace_id.as_deref() == workspace));
        host.snapshot.routes.push(ClusterRoute {
            job_id: job.into(),
            workspace_id: workspace.map(str::to_owned),
            daemon: Daemon {
                token: "fixture-job-daemon-token".into(),
                build: "fixture".into(),
                sessions: 0,
            },
        });
        Ok(())
    }
    pub(crate) async fn remove_if_idle(&self, id: &str) -> Result<(), ()> {
        let mut data = self.data.lock().await;
        if data
            .hosts
            .get(id)
            .is_some_and(|h| h.snapshot.records.values().any(|r| r.ended.is_none()))
        {
            return Err(());
        }
        if let Some(host) = data.hosts.remove(id) {
            for target in host.targets.values() {
                let _ = target.stop.send(true);
            }
        }
        Ok(())
    }
    pub(crate) async fn submissions(&self, host: &str) -> usize {
        self.data
            .lock()
            .await
            .hosts
            .get(host)
            .map_or(0, |h| h.submissions)
    }
    pub(crate) async fn batch_refusal(
        &self,
        host: &str,
        refusal: Option<BatchRefusalKind>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            refusal != Some(BatchRefusalKind::Unknown),
            "unknown fixture batch refusal"
        );
        let mut data = self.data.lock().await;
        let host = data
            .hosts
            .get_mut(host)
            .ok_or_else(|| anyhow::anyhow!("unknown fixture cluster"))?;
        host.batch_refusal = refusal;
        Ok(())
    }
    pub(crate) async fn override_reply(&self, host: &str, reply: Option<serde_json::Value>) {
        if let Some(host) = self.data.lock().await.hosts.get_mut(host) {
            host.reply_override = reply;
        }
    }
    pub(crate) async fn state(&self, host: &str, id: &str, state: ClusterOperationState) {
        if let Some(host) = self.data.lock().await.hosts.get_mut(host) {
            if let Some(operation) = host.operations.get_mut(id) {
                operation.state = state;
            }
        }
    }
    pub(crate) async fn snapshot(&self, host: &str) -> Option<ClusterSnapshot> {
        self.data
            .lock()
            .await
            .hosts
            .get(host)
            .map(|h| h.snapshot.clone())
    }
}
impl Default for ClusterSnapshot {
    fn default() -> Self {
        Self {
            scheduler: ClusterScheduler::Slurm,
            login_node: "login.fixture.test".into(),
            home: "/home/fixture".into(),
            now_ms: 0,
            jobs: vec![],
            workspaces: vec![],
            other_jobs: Default::default(),
            degraded: false,
            queue_at_ms: 0,
            config: Default::default(),
            startup: Default::default(),
            config_sum: "1".into(),
            state_unreadable: false,
            records: BTreeMap::new(),
            routes: vec![],
        }
    }
}
pub(crate) fn router() -> Router<FakeKeeper> {
    Router::new()
        .route("/v1/cluster/capabilities", get(capabilities))
        .route(
            "/v1/hosts/{id}/cluster/operations",
            post(operation).layer(DefaultBodyLimit::max(CLUSTER_BODY_MAX)),
        )
        .route(
            "/v1/hosts/{id}/cluster/operations/{operation}",
            get(history),
        )
        .route("/v1/hosts/{id}/jobs/{job}/tcp", get(job_tcp))
        .route(
            "/v1/hosts/{id}/jobs/{job}/workspaces/{workspace}/tcp",
            get(workspace_tcp),
        )
}
fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(serde_json::json!({"error":code}))).into_response()
}
async fn capabilities(State(keeper): State<FakeKeeper>) -> Response {
    match keeper.clusters().data.lock().await.capabilities.as_ref() {
        Some(capabilities) => Json(capabilities.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn operation(
    State(keeper): State<FakeKeeper>,
    Path(id): Path<String>,
    body: Result<Json<ClusterOperation>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(operation)) = body else {
        return error(StatusCode::BAD_REQUEST, "invalid_operation");
    };
    if operation.validate().is_err() {
        return error(StatusCode::BAD_REQUEST, "invalid_operation");
    }
    let mut data = keeper.clusters().data.lock().await;
    if !data
        .capabilities
        .as_ref()
        .is_some_and(ClusterCapabilities::control_supported)
    {
        return error(StatusCode::NOT_FOUND, "unsupported");
    }
    if matches!(operation, ClusterOperation::StartJob { .. })
        && !data
            .capabilities
            .as_ref()
            .is_some_and(ClusterCapabilities::jobs_supported)
    {
        return error(StatusCode::CONFLICT, "unsupported");
    }
    let global_holders = data
        .hosts
        .values()
        .map(|h| {
            h.snapshot
                .records
                .values()
                .filter(|r| r.ended.is_none())
                .count()
        })
        .sum::<usize>();
    let Some(host) = data.hosts.get_mut(&id) else {
        return error(StatusCode::NOT_FOUND, "host_unavailable");
    };
    let hash: [u8; 32] =
        Sha256::digest(serde_json::to_vec(&operation).expect("validated fixture operation")).into();
    if let Some(operation_id) = operation.operation_id() {
        if let Some(previous) = host.operations.get(operation_id) {
            if previous.hash != hash {
                return error(StatusCode::CONFLICT, "operation_changed");
            }
            return match &previous.state {
                ClusterOperationState::Completed { reply } => Json(reply.as_ref()).into_response(),
                _ => error(StatusCode::CONFLICT, "operation_uncertain"),
            };
        }
        if host.operations.len() >= 128 {
            return error(StatusCode::TOO_MANY_REQUESTS, "operation_limit");
        }
    }
    if matches!(operation, ClusterOperation::StartJob { .. })
        && (global_holders >= 16
            || host
                .snapshot
                .records
                .values()
                .filter(|r| r.ended.is_none())
                .count()
                >= 8)
    {
        return error(StatusCode::TOO_MANY_REQUESTS, "jobs_held");
    }
    let result = apply(host, &operation, &id);
    let reply = match result {
        Ok(reply) => reply,
        Err((status, code)) => return error(status, code),
    };
    if let Some(id) = operation.operation_id() {
        host.operations.insert(
            id.into(),
            Operation {
                hash,
                state: ClusterOperationState::Completed {
                    reply: Box::new(reply.clone()),
                },
            },
        );
    }
    let override_reply = host.reply_override.clone();
    let updated = matches!(
        operation,
        ClusterOperation::SetPolicy { .. } | ClusterOperation::StopLoginDaemon { .. }
    )
    .then(|| host.host.clone());
    drop(data);
    if let Some(host) = updated {
        keeper.cluster_host_updated(host).await;
    }
    if let Some(override_reply) = override_reply {
        return Json(override_reply).into_response();
    }
    Json(reply).into_response()
}
fn apply(
    host: &mut FixtureHost,
    operation: &ClusterOperation,
    _host_id: &str,
) -> Result<ClusterReply, (StatusCode, &'static str)> {
    use ClusterOperation::*;
    let o = &mut host.snapshot;
    Ok(match operation {
        Overview { .. } => ClusterReply::Overview {
            overview: Box::new(o.clone()),
        },
        Facts { .. } => ClusterReply::Facts {
            facts: chimaera_core::cluster::ClusterFacts {
                scheduler: Scheduler::Slurm,
                ..Default::default()
            },
        },
        ReadConfig {} => ClusterReply::Config {
            config: o.config.clone(),
            config_sum: o.config_sum.clone(),
        },
        WriteConfig {
            config,
            expected_sum,
            ..
        } => {
            if expected_sum != &o.config_sum {
                return Err((StatusCode::CONFLICT, "config_changed"));
            }
            o.config = config.clone();
            o.config_sum = (o.config_sum.parse::<u64>().unwrap_or(0) + 1).to_string();
            ClusterReply::Saved
        }
        AddWorkspace { path, name, .. } => {
            let workspace = ClusterWorkspace {
                id: chimaera_core::cluster::new_workspace_id(),
                path: path.clone(),
                name: name.clone(),
                created_ms: 0,
            };
            o.config.workspaces.push(workspace.clone());
            o.workspaces.push(ClusterWorkspaceView {
                id: workspace.id.clone(),
                name: workspace.name.clone(),
                path: workspace.path.clone(),
                state: ClusterWorkspaceState::Closed,
                job: None,
                last_open_ms: None,
                opening: false,
                closing: false,
                failed: None,
                working: None,
            });
            ClusterReply::Workspace { workspace }
        }
        RemoveWorkspace { workspace_id, .. } => {
            o.config.workspaces.retain(|w| &w.id != workspace_id);
            o.workspaces.retain(|w| &w.id != workspace_id);
            o.routes
                .retain(|r| r.workspace_id.as_ref() != Some(workspace_id));
            host.targets.retain(|(_, workspace), target| {
                if workspace.as_ref() == Some(workspace_id) {
                    let _ = target.stop.send(true);
                    false
                } else {
                    true
                }
            });
            ClusterReply::Saved
        }
        ListDir { path } => ClusterReply::Directory {
            directory: chimaera_core::cluster::DirListing {
                path: path.clone(),
                parent: None,
                workspace: None,
                folders: vec![],
                truncated: false,
            },
        },
        StartJob {
            job_id,
            name,
            spec,
            open,
            startup,
            attached,
            replaces,
            ..
        } => {
            if o.records.contains_key(job_id) {
                return Err((StatusCode::CONFLICT, "job_unavailable"));
            }
            if o.records.len() >= CLUSTER_ITEMS_MAX {
                return Err((StatusCode::TOO_MANY_REQUESTS, "jobs_held"));
            }
            if !attached {
                if let Some(refusal) = host.batch_refusal {
                    return Ok(ClusterReply::Refused {
                        job_id: job_id.clone(),
                        refusal,
                        slurm_job_id: None,
                    });
                }
            }
            host.submissions += 1;
            o.jobs.push(ClusterJobView {
                id: job_id.clone(),
                name: name.clone().unwrap_or_default(),
                state: ClusterJobState::Waiting,
                slurm_job_id: None,
                node: String::new(),
                partition: String::new(),
                cpus: String::new(),
                mem: String::new(),
                gpus: None,
                ends_at_ms: None,
                reason: String::new(),
                attached: *attached,
                ended: None,
                ended_at_ms: None,
                stopped_by_user: false,
                egress: None,
                open: open.clone(),
                replaces: replaces.clone(),
                spec: spec.clone(),
                startup: startup.clone(),
                submitted_ms: 0,
            });
            let slurm_job_id = (!attached).then(|| host.submissions.to_string());
            o.records.insert(
                job_id.clone(),
                JobRecord {
                    id: job_id.clone(),
                    name: name.clone().unwrap_or_default(),
                    job_name: format!("chimaera-fixture~{}", &job_id[2..]),
                    spec: spec.clone(),
                    open: open.clone(),
                    startup: startup.clone(),
                    attached: *attached,
                    replaces: replaces.clone(),
                    slurm_job_id: slurm_job_id.clone(),
                    ..Default::default()
                },
            );
            ClusterReply::Job {
                job_id: job_id.clone(),
                slurm_job_id,
                attached: *attached,
            }
        }
        StopJob { job_id, .. } => {
            let Some(record) = o.records.get_mut(job_id) else {
                return Err((StatusCode::CONFLICT, "job_unavailable"));
            };
            if let Some(view) = o.jobs.iter_mut().find(|v| &v.id == job_id) {
                view.state = ClusterJobState::Ended;
                view.ended = Some("CANCELLED".into());
                view.stopped_by_user = true;
            }
            record.ended = Some(chimaera_core::cluster::Ended {
                state: "CANCELLED".into(),
                at_ms: 0,
            });
            host.targets.retain(|(job, _), target| {
                if job == job_id {
                    let _ = target.stop.send(true);
                    false
                } else {
                    true
                }
            });
            o.routes.retain(|r| &r.job_id != job_id);
            ClusterReply::Stopped {
                job_id: job_id.clone(),
            }
        }
        DismissJob { job_id, .. } => {
            if o.records.get(job_id).is_some_and(|r| r.ended.is_none()) {
                return Err((StatusCode::CONFLICT, "jobs_held"));
            }
            o.records.remove(job_id);
            o.jobs.retain(|v| &v.id != job_id);
            ClusterReply::Saved
        }
        QueueOpen {
            job_id,
            workspace_id,
            ..
        } => {
            let Some(record) = o.records.get_mut(job_id) else {
                return Err((StatusCode::CONFLICT, "job_unavailable"));
            };
            if !record.open.contains(workspace_id) {
                record.open.push(workspace_id.clone());
            }
            ClusterReply::Saved
        }
        SetStartup {
            workspace_id, text, ..
        } => {
            if let Some(w) = workspace_id {
                o.startup.workspaces.insert(w.clone(), text.clone());
            } else {
                o.startup.cluster = text.clone();
            }
            ClusterReply::Saved
        }
        ForgetSetup { name, .. } => {
            o.config.setups.retain(|s| &s.name != name);
            ClusterReply::Saved
        }
        SetAgentRules { text, file, .. } => {
            o.config.agent_rules = chimaera_core::cluster::AgentRules {
                text: text.clone(),
                file: file.clone(),
            };
            ClusterReply::Saved
        }
        SetPolicy {
            login_serve,
            not_cluster,
            ..
        } => {
            if let Some(c) = &mut host.host.cluster {
                c.login_serve = *login_serve;
                c.not_cluster = *not_cluster;
            }
            ClusterReply::Host {
                host: host.host.clone(),
            }
        }
        StopLoginDaemon { .. } => {
            host.host.daemon = None;
            if let Some(c) = &mut host.host.cluster {
                c.login_serve = false;
            }
            ClusterReply::Host {
                host: host.host.clone(),
            }
        }
        StartEstimate { job_id } => ClusterReply::Estimate {
            job_id: job_id.clone(),
            at_ms: None,
        },
    })
}
async fn history(
    State(keeper): State<FakeKeeper>,
    Path((id, operation)): Path<(String, String)>,
) -> Response {
    if !valid_operation_id(&operation) {
        return error(StatusCode::BAD_REQUEST, "invalid_operation");
    }
    let data = keeper.clusters().data.lock().await;
    let Some(host) = data.hosts.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(
        host.operations
            .get(&operation)
            .map_or(ClusterOperationState::Unknown, |o| o.state.clone()),
    )
    .into_response()
}
async fn job_tcp(
    State(keeper): State<FakeKeeper>,
    Extension(authority): Extension<crate::fake::AuthenticatedGeneration>,
    Path((id, job)): Path<(String, String)>,
    ws: WebSocketUpgrade,
) -> Response {
    tcp(keeper, id, job, None, ws, authority).await
}
async fn workspace_tcp(
    State(keeper): State<FakeKeeper>,
    Extension(authority): Extension<crate::fake::AuthenticatedGeneration>,
    Path((id, job, workspace)): Path<(String, String, String)>,
    ws: WebSocketUpgrade,
) -> Response {
    tcp(keeper, id, job, Some(workspace), ws, authority).await
}
async fn tcp(
    keeper: FakeKeeper,
    id: String,
    job: String,
    workspace: Option<String>,
    ws: WebSocketUpgrade,
    authority: crate::fake::AuthenticatedGeneration,
) -> Response {
    let target = {
        let data = keeper.clusters().data.lock().await;
        if !data
            .capabilities
            .as_ref()
            .is_some_and(ClusterCapabilities::jobs_supported)
        {
            return error(StatusCode::CONFLICT, "unsupported");
        }
        let Some(host) = data.hosts.get(&id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        host.targets
            .get(&(job, workspace))
            .map(|target| (target.address, target.stop.subscribe()))
    };
    let Some((target, stopped)) = target else {
        return error(StatusCode::CONFLICT, "job_unavailable");
    };
    keeper
        .cluster_fixture_tcp(ws, target, stopped, authority)
        .await
}
