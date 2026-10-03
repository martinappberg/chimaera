//! Capability-gated cluster operations. Protected routes are native memory only.
use crate::{Daemon, Host};
use anyhow::{Result, bail, ensure};
use chimaera_core::{
    cluster::{self, ClusterConfig, ClusterFacts, ClusterWorkspace, DirListing, JobRecord},
    slurm::LaunchSpec,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CLUSTER_VERSION: u32 = 1;
pub const CLUSTER_BODY_MAX: usize = 64 * 1024;
pub const CLUSTER_REPLY_MAX: usize = 2 * 1024 * 1024;
pub const CLUSTER_ITEMS_MAX: usize = 128;
pub const CLUSTER_TEXT_MAX: usize = 32 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClusterCapabilities {
    pub version: u32,
    #[serde(default)]
    pub cluster_control_v1: bool,
    #[serde(default)]
    pub job_tunnels_v1: bool,
}
impl ClusterCapabilities {
    pub fn control_supported(&self) -> bool {
        self.version == CLUSTER_VERSION && self.cluster_control_v1
    }
    pub fn jobs_supported(&self) -> bool {
        self.control_supported() && self.job_tunnels_v1
    }
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClusterPolicy {
    #[serde(default)]
    pub login_serve: bool,
    #[serde(default)]
    pub not_cluster: bool,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClusterScheduler {
    Slurm,
    #[serde(other)]
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostCluster {
    pub scheduler: ClusterScheduler,
    #[serde(default)]
    pub login_serve: bool,
    #[serde(default)]
    pub not_cluster: bool,
}

/// Requests contain explicit user text, so they intentionally omit Debug.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClusterOperation {
    Overview {
        #[serde(default)]
        refresh: bool,
    },
    Facts {
        #[serde(default)]
        refresh: bool,
    },
    ReadConfig {},
    WriteConfig {
        operation_id: String,
        expected_sum: String,
        config: ClusterConfig,
    },
    AddWorkspace {
        operation_id: String,
        path: String,
        name: String,
    },
    RemoveWorkspace {
        operation_id: String,
        workspace_id: String,
    },
    ListDir {
        path: String,
    },
    StartJob {
        operation_id: String,
        job_id: String,
        name: Option<String>,
        spec: LaunchSpec,
        open: Vec<String>,
        startup: String,
        attached: bool,
        replaces: Option<String>,
        save_as: Option<String>,
    },
    StopJob {
        operation_id: String,
        job_id: String,
    },
    DismissJob {
        operation_id: String,
        job_id: String,
    },
    QueueOpen {
        operation_id: String,
        job_id: String,
        workspace_id: String,
    },
    SetStartup {
        operation_id: String,
        workspace_id: Option<String>,
        text: String,
    },
    ForgetSetup {
        operation_id: String,
        name: String,
    },
    SetAgentRules {
        operation_id: String,
        text: String,
        file: Option<String>,
    },
    SetPolicy {
        operation_id: String,
        login_serve: bool,
        not_cluster: bool,
    },
    StopLoginDaemon {
        operation_id: String,
    },
    StartEstimate {
        job_id: String,
    },
}
impl ClusterOperation {
    pub fn operation_id(&self) -> Option<&str> {
        match self {
            Self::WriteConfig { operation_id, .. }
            | Self::AddWorkspace { operation_id, .. }
            | Self::RemoveWorkspace { operation_id, .. }
            | Self::StartJob { operation_id, .. }
            | Self::StopJob { operation_id, .. }
            | Self::DismissJob { operation_id, .. }
            | Self::QueueOpen { operation_id, .. }
            | Self::SetStartup { operation_id, .. }
            | Self::ForgetSetup { operation_id, .. }
            | Self::SetAgentRules { operation_id, .. }
            | Self::SetPolicy { operation_id, .. }
            | Self::StopLoginDaemon { operation_id } => Some(operation_id),
            _ => None,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if let Some(id) = self.operation_id() {
            ensure!(valid_operation_id(id), "invalid cluster operation id");
        }
        let workspace = |id: &str| -> Result<()> {
            ensure!(
                cluster::valid_workspace_id(id),
                "invalid cluster workspace id"
            );
            Ok(())
        };
        let job = |id: &str| -> Result<()> {
            ensure!(cluster::valid_job_id(id), "invalid cluster job id");
            Ok(())
        };
        match self {
            Self::WriteConfig {
                expected_sum,
                config,
                ..
            } => {
                ensure!(
                    expected_sum.len() <= 32 && expected_sum.bytes().all(|b| b.is_ascii_digit()),
                    "invalid cluster checksum"
                );
                validate_config(config)?;
            }
            Self::AddWorkspace { path, name, .. } => {
                validate_path(path)?;
                validate_name(name)?;
            }
            Self::ListDir { path } => validate_path(path)?,
            Self::RemoveWorkspace { workspace_id, .. } => workspace(workspace_id)?,
            Self::StartJob {
                job_id,
                name,
                spec,
                open,
                startup,
                replaces,
                save_as,
                ..
            } => {
                job(job_id)?;
                spec.validate()
                    .map_err(|_| anyhow::anyhow!("invalid cluster launch specification"))?;
                ensure!(
                    open.len() <= CLUSTER_ITEMS_MAX,
                    "too many cluster workspaces"
                );
                for id in open {
                    workspace(id)?;
                }
                if let Some(id) = replaces {
                    job(id)?;
                    ensure!(id != job_id, "a job cannot replace itself");
                }
                for name in [name, save_as].into_iter().flatten() {
                    validate_name(name)?;
                }
                validate_text(startup)?;
            }
            Self::StopJob { job_id, .. }
            | Self::DismissJob { job_id, .. }
            | Self::StartEstimate { job_id } => job(job_id)?,
            Self::QueueOpen {
                job_id,
                workspace_id,
                ..
            } => {
                job(job_id)?;
                workspace(workspace_id)?;
            }
            Self::SetStartup {
                workspace_id, text, ..
            } => {
                if let Some(id) = workspace_id {
                    workspace(id)?;
                }
                validate_text(text)?;
            }
            Self::ForgetSetup { name, .. } => validate_name(name)?,
            Self::SetAgentRules { text, file, .. } => {
                validate_text(text)?;
                if let Some(path) = file {
                    ensure!(
                        path.starts_with('/'),
                        "cluster rules require an absolute path"
                    );
                    validate_path(path)?;
                }
            }
            _ => (),
        }
        ensure!(
            serde_json::to_vec(self)
                .map_err(|_| anyhow::anyhow!("invalid cluster request"))?
                .len()
                <= CLUSTER_BODY_MAX,
            "cluster request exceeds 64 KiB"
        );
        Ok(())
    }
    /// A well-formed response can still belong to another request or job.
    pub fn accepts(&self, reply: &ClusterReply) -> bool {
        match (self, reply) {
            (Self::Overview { .. }, ClusterReply::Overview { .. })
            | (Self::Facts { .. }, ClusterReply::Facts { .. })
            | (Self::ReadConfig {}, ClusterReply::Config { .. })
            | (Self::ListDir { .. }, ClusterReply::Directory { .. })
            | (Self::AddWorkspace { .. }, ClusterReply::Workspace { .. })
            | (
                Self::WriteConfig { .. }
                | Self::RemoveWorkspace { .. }
                | Self::DismissJob { .. }
                | Self::QueueOpen { .. }
                | Self::SetStartup { .. }
                | Self::ForgetSetup { .. }
                | Self::SetAgentRules { .. },
                ClusterReply::Saved,
            )
            | (Self::SetPolicy { .. } | Self::StopLoginDaemon { .. }, ClusterReply::Host { .. }) => {
                true
            }
            (
                Self::StartJob {
                    job_id, attached, ..
                },
                ClusterReply::Job {
                    job_id: actual,
                    attached: actual_attached,
                    ..
                },
            ) => job_id == actual && attached == actual_attached,
            (
                Self::StartJob {
                    job_id,
                    attached: false,
                    ..
                },
                ClusterReply::Refused {
                    job_id: actual,
                    refusal,
                    slurm_job_id,
                },
            ) => {
                job_id == actual && *refusal != BatchRefusalKind::Unknown && slurm_job_id.is_none()
            }
            (
                Self::StopJob { job_id, .. },
                ClusterReply::StopPending { job_id: actual }
                | ClusterReply::Stopped { job_id: actual },
            )
            | (Self::StartEstimate { job_id }, ClusterReply::Estimate { job_id: actual, .. }) => {
                job_id == actual
            }
            _ => false,
        }
    }
}
pub fn valid_operation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 512 && !name.chars().any(char::is_control),
        "invalid cluster name"
    );
    Ok(())
}
fn validate_path(path: &str) -> Result<()> {
    ensure!(
        !path.trim().is_empty() && path.len() <= 1024 && !path.chars().any(char::is_control),
        "invalid cluster path"
    );
    Ok(())
}
fn validate_text(text: &str) -> Result<()> {
    ensure!(
        text.len() <= CLUSTER_TEXT_MAX && !text.contains('\0'),
        "cluster text exceeds limit or contains NUL"
    );
    Ok(())
}
fn validate_config(config: &ClusterConfig) -> Result<()> {
    ensure!(
        config.workspaces.len() <= CLUSTER_ITEMS_MAX && config.setups.len() <= CLUSTER_ITEMS_MAX,
        "cluster configuration exceeds limit"
    );
    for w in &config.workspaces {
        ensure!(
            cluster::valid_workspace_id(&w.id),
            "invalid cluster workspace id"
        );
        validate_name(&w.name)?;
        validate_path(&w.path)?;
    }
    for setup in &config.setups {
        validate_name(&setup.name)?;
        setup
            .spec
            .validate()
            .map_err(|_| anyhow::anyhow!("invalid cluster launch specification"))?;
    }
    if let Some(spec) = &config.last_spec {
        spec.validate()
            .map_err(|_| anyhow::anyhow!("invalid cluster launch specification"))?;
    }
    validate_text(&config.agent_rules.text)?;
    if let Some(path) = &config.agent_rules.file {
        ensure!(
            path.starts_with('/'),
            "cluster rules require an absolute path"
        );
        validate_path(path)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClusterJobState {
    Waiting,
    Starting,
    Running,
    Ended,
    #[serde(other)]
    Unknown,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClusterWorkspaceState {
    Open,
    Queued,
    Closed,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ClusterJobView {
    pub id: String,
    pub name: String,
    pub state: ClusterJobState,
    #[serde(default)]
    pub slurm_job_id: Option<String>,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub partition: String,
    #[serde(default)]
    pub cpus: String,
    #[serde(default)]
    pub mem: String,
    #[serde(default)]
    pub gpus: Option<u32>,
    #[serde(default)]
    pub ends_at_ms: Option<u64>,
    #[serde(default)]
    pub reason: String,
    pub attached: bool,
    #[serde(default)]
    pub ended: Option<String>,
    #[serde(default)]
    pub ended_at_ms: Option<u64>,
    pub stopped_by_user: bool,
    #[serde(default)]
    pub stopping: bool,
    #[serde(default)]
    pub egress: Option<bool>,
    pub open: Vec<String>,
    #[serde(default)]
    pub replaces: Option<String>,
    pub spec: LaunchSpec,
    pub startup: String,
    pub submitted_ms: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ClusterWorkspaceView {
    pub id: String,
    pub name: String,
    pub path: String,
    pub state: ClusterWorkspaceState,
    #[serde(default)]
    pub job: Option<String>,
    #[serde(default)]
    pub last_open_ms: Option<u64>,
    #[serde(default)]
    pub opening: bool,
    #[serde(default)]
    pub closing: bool,
    #[serde(default)]
    pub failed: Option<String>,
    #[serde(default)]
    pub working: Option<u32>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ClusterOtherJobs {
    pub running: usize,
    pub waiting: usize,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ClusterStartup {
    pub cluster: String,
    pub workspaces: BTreeMap<String, String>,
}

/// A positively framed batch refusal, never arbitrary SSH failure text.
/// Unknown classifications cannot authorize allocation/hold cleanup.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BatchRefusalKind {
    BatchNotAllowed,
    AccountRequired,
    QosRequired,
    ConstraintRequired,
    Other,
    #[serde(other)]
    Unknown,
}

// No Debug on a route or any enclosing response: these carry daemon bearers.
#[derive(Clone, Serialize, Deserialize)]
pub struct ClusterRoute {
    pub job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub daemon: Daemon,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ClusterSnapshot {
    pub scheduler: ClusterScheduler,
    pub login_node: String,
    pub home: String,
    pub now_ms: u64,
    pub jobs: Vec<ClusterJobView>,
    pub workspaces: Vec<ClusterWorkspaceView>,
    pub other_jobs: ClusterOtherJobs,
    pub degraded: bool,
    pub queue_at_ms: u64,
    pub config: ClusterConfig,
    pub startup: ClusterStartup,
    pub config_sum: String,
    pub state_unreadable: bool,
    pub records: BTreeMap<String, JobRecord>,
    pub routes: Vec<ClusterRoute>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ClusterReply {
    Overview {
        overview: Box<ClusterSnapshot>,
    },
    Facts {
        facts: ClusterFacts,
    },
    Config {
        config: ClusterConfig,
        config_sum: String,
    },
    Directory {
        directory: DirListing,
    },
    Workspace {
        workspace: ClusterWorkspace,
    },
    Saved,
    Job {
        job_id: String,
        #[serde(default)]
        slurm_job_id: Option<String>,
        attached: bool,
    },
    Refused {
        job_id: String,
        refusal: BatchRefusalKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slurm_job_id: Option<String>,
    },
    StopPending {
        job_id: String,
    },
    Stopped {
        job_id: String,
    },
    Host {
        host: Host,
    },
    Estimate {
        job_id: String,
        at_ms: Option<u64>,
    },
    #[serde(other)]
    Unknown,
}
impl ClusterReply {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Overview { overview: o } => {
                ensure!(
                    o.jobs.len() <= CLUSTER_ITEMS_MAX
                        && o.workspaces.len() <= CLUSTER_ITEMS_MAX
                        && o.records.len() <= CLUSTER_ITEMS_MAX
                        && o.routes.len() <= CLUSTER_ITEMS_MAX,
                    "cluster snapshot exceeds item limit"
                );
                validate_config(&o.config)?;
                for j in &o.jobs {
                    ensure!(cluster::valid_job_id(&j.id), "invalid cluster job id");
                }
                for w in &o.workspaces {
                    ensure!(
                        cluster::valid_workspace_id(&w.id),
                        "invalid cluster workspace id"
                    );
                }
                for (id, r) in &o.records {
                    ensure!(
                        cluster::valid_job_id(id) && id == &r.id,
                        "mismatched cluster record"
                    );
                }
                let mut seen = std::collections::BTreeSet::new();
                for r in &o.routes {
                    ensure!(
                        seen.insert((&r.job_id, &r.workspace_id)),
                        "duplicate cluster route"
                    );
                    ensure!(
                        o.records.get(&r.job_id).is_some_and(|v| v.ended.is_none())
                            && o.jobs
                                .iter()
                                .any(|v| v.id == r.job_id && v.state == ClusterJobState::Running),
                        "cluster route is not ready"
                    );
                    ensure!(
                        cluster::valid_job_id(&r.job_id) && o.records.contains_key(&r.job_id),
                        "invalid cluster route job"
                    );
                    if let Some(w) = &r.workspace_id {
                        ensure!(
                            cluster::valid_workspace_id(w)
                                && o.workspaces.iter().any(|v| &v.id == w
                                    && v.state == ClusterWorkspaceState::Open
                                    && v.job.as_ref() == Some(&r.job_id)),
                            "invalid cluster route workspace"
                        );
                    }
                    ensure!(
                        !r.daemon.token.is_empty()
                            && r.daemon.token.len() <= 4096
                            && !r.daemon.token.chars().any(char::is_control),
                        "invalid cluster daemon credential"
                    );
                }
            }
            Self::Config { config, .. } => validate_config(config)?,
            Self::Workspace { workspace } => ensure!(
                cluster::valid_workspace_id(&workspace.id),
                "invalid cluster workspace id"
            ),
            Self::Job {
                job_id,
                slurm_job_id,
                ..
            } => {
                ensure!(cluster::valid_job_id(job_id), "invalid cluster job id");
                if let Some(id) = slurm_job_id {
                    ensure!(cluster::valid_slurm_job_id(id), "invalid scheduler job id");
                }
            }
            Self::Refused {
                job_id,
                refusal,
                slurm_job_id,
            } => {
                ensure!(
                    slurm_job_id.is_none(),
                    "refusal contains scheduler identity"
                );
                ensure!(cluster::valid_job_id(job_id), "invalid cluster job id");
                ensure!(
                    *refusal != BatchRefusalKind::Unknown,
                    "unsupported cluster refusal"
                );
            }
            Self::StopPending { job_id }
            | Self::Stopped { job_id }
            | Self::Estimate { job_id, .. } => {
                ensure!(cluster::valid_job_id(job_id), "invalid cluster job id")
            }
            Self::Unknown => bail!("unsupported cluster reply"),
            _ => (),
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ClusterOperationState {
    Pending,
    Completed {
        reply: Box<ClusterReply>,
    },
    Uncertain,
    #[serde(other)]
    Unknown,
}
impl ClusterOperationState {
    pub fn validate_for(&self, operation: &ClusterOperation) -> Result<()> {
        if let Self::Completed { reply } = self {
            reply.validate()?;
            ensure!(
                operation.accepts(reply),
                "cluster reply does not match operation"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClusterErrorCode {
    UnsupportedClusterPolicy,
    OperationChanged,
    ClusterRequiresJob,
    JobsHeld,
    JobUnavailable,
    JobsChanged,
    RolloutPending,
    #[serde(other)]
    Unknown,
}
#[derive(Debug)]
pub struct ClusterRequestError {
    pub status: u16,
    pub code: ClusterErrorCode,
}
impl std::fmt::Display for ClusterRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cluster request rejected ({}, {:?})",
            self.status, self.code
        )
    }
}
impl std::error::Error for ClusterRequestError {}
