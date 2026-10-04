//! Trusted optional daemon composition. Public guards and recovery remain loaded without it.
use std::{future::Future, pin::Pin};

pub use crate::pro::engine::coordinator_host::{
    CoordinatorOwner, CoordinatorProject, CoordinatorProjects, CoordinatorTick, Reconciled,
};

/// Actual WebSocket relay types and original scope/stream reservation. The
/// optional policy is trusted in-process; no token, URL or route registry is
/// exported, and the public request remains the original task owner.
pub mod viewer {
    pub use crate::session_proxy::viewer_host::{
        admission_refusal, OwnerError, OwnerMessage, ViewerAdmission, ViewerDownstream,
        ViewerEnded, ViewerFrameKind, ViewerIntent, ViewerMessage, ViewerOwnerKind, ViewerReach,
        ViewerReservation, ViewerScope, ViewerStream, ViewerUpgrade,
    };
    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    pub use crate::session_proxy::viewer_host::{fixture, legacy_fixture};
}
pub type ViewerFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Original transfer data and fixed Git/cache capabilities. These do not
/// carry live-install, arbitrary process or account-request authority.
pub mod transfer {
    pub use crate::pro::execution::wire::Checkpoint;
    pub use crate::pro::mirror::{copy_tree, Report};
    pub use crate::pro::policy::{
        allowed_path, contains_credential, MAX_FILE_BYTES, MAX_PATHS, REBUILT_DIRS,
    };
    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    pub use crate::pro::shadow_cache::fixture_install as install_shadow;
    pub use crate::pro::shadow_cache::PreparedShadow;
    pub use crate::pro::transfer_dispatch::{TransferFuture, TransferReply, TransferRequest};
    pub use crate::pro::transfer_host::{
        Git, GitLayout, GitOutput, MirrorGrant, MirrorRepository, TransferHost,
    };
    pub use crate::pro::transfer_types::{
        Described, Descriptor, Entry, Incoming, Prepared, ReturnRepository, Snapshot,
        StagingIncoming, StagingStatus, Write,
    };
    pub const PROJECT_STAGING_PREFIX: &str = crate::persist::PROJECT_STAGING_PREFIX;
    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    pub use crate::pro::transfer_host::fixture;
}
/// Original per-project operation capabilities. No raw store or bearer escapes.
pub mod project {
    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    pub use crate::pro::engine::project_host::fixture;
    pub use crate::pro::engine::project_host::move_host::MoveYieldOwner;
    pub use crate::pro::engine::project_host::policy_host::{
        HandbackOwner, MoveRequestOwner, OwnershipObservation, PolicyReply, ReleaseOwner,
        UpgradeRequired,
    };
    pub use crate::pro::engine::project_host::ConfigurationReport;
    pub use crate::pro::engine::project_host::ProjectMetadata;
    pub use crate::pro::engine::project_host::{
        snapshot_failure, SnapshotDescription, SnapshotOwner,
    };
    pub use crate::pro::engine::project_host::{
        IdleWorker, LeaseOperation, ProjectBaton, ProjectConfiguration, ProjectLeaseReply,
        ProjectLeaseStart, ProjectOwner, ProjectRole,
    };
    pub use crate::pro::engine::{Manifest as SnapshotManifest, SessionArchive, Sleep};
    pub use crate::pro::execution::wire::Continuation;
    pub use crate::pro::moves::Decision as MoveDecision;
    pub use crate::pro::policy::CloudProfile;
    pub use crate::pro::Ownership;
}
pub type SnapshotFuture<'a> = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;
pub type ProjectUnitFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;
pub type ProjectFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<Option<u64>>> + Send + 'static>>;
/// Fixed worker-provider controller over the original public work owners.
pub mod providers {
    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    pub use crate::cloud::providers::host::fixture;
    pub use crate::cloud::providers::host::{
        definition, Action, ActivePermit, Connection, Operation, Phase, ProviderDefinition,
        ProviderExecutable, ProviderFuture, ProviderHost, ProviderInvocation, ProviderSession,
        ProviderState, ProviderStatus, ProviderWork, WorkerProvider, WorkerProviders, PROVIDERS,
    };
    pub use crate::cloud::providers::process::{
        group_alive, output, output_tracked, Child, Output, LIMIT, TIMEOUT,
    };
}
pub mod guidance {
    pub use crate::mcp::cloud_context::{GuidanceContext, GuidanceSetup};
    pub use chimaera_agent::model::truncate_label;
}
pub type RuntimeFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// A first-party in-process policy. It never receives AppState, a token or a
/// generic process/HTTP callback. The public host owns its spawned continuation.
pub trait Runtime: Send + Sync + 'static {
    fn guidance_definitions(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }
    fn guidance_arrival(&self, _context: guidance::GuidanceContext<'_>) -> String {
        String::new()
    }
    fn guidance_setup(
        &self,
        _profile: &project::CloudProfile,
        _proposed: Option<&str>,
    ) -> Option<guidance::GuidanceSetup> {
        None
    }

    fn worker_providers(
        &self,
        _host: providers::ProviderHost,
    ) -> Option<std::sync::Arc<dyn providers::WorkerProviders>> {
        None
    }

    fn transfer<'a>(
        &'a self,
        _host: std::sync::Arc<transfer::TransferHost>,
        _request: transfer::TransferRequest<'a>,
    ) -> transfer::TransferFuture<'a> {
        Box::pin(async { Err(anyhow::anyhow!("optional_runtime_unavailable")) })
    }
    fn coordinate(&self, owner: CoordinatorOwner) -> RuntimeFuture;
    fn snapshot<'a>(
        &'a self,
        _owner: project::SnapshotOwner,
        _phase: &'a mut &'static str,
    ) -> SnapshotFuture<'a> {
        Box::pin(async { Err(anyhow::anyhow!("optional_runtime_unavailable")) })
    }
    fn release(
        &self,
        _owner: project::ReleaseOwner,
        _budget: std::time::Duration,
    ) -> ProjectUnitFuture {
        Box::pin(async { Err(anyhow::anyhow!("optional_runtime_unavailable")) })
    }
    fn prepare_handback(&self, _owner: project::HandbackOwner) -> ProjectFuture {
        Box::pin(async { Err(anyhow::anyhow!("optional_runtime_unavailable")) })
    }
    fn request_move(&self, _owner: project::MoveRequestOwner) -> ProjectUnitFuture {
        Box::pin(async { Err(anyhow::anyhow!("optional_runtime_unavailable")) })
    }
    fn move_requested_here(
        &self,
        _observation: &project::OwnershipObservation,
        _now: u64,
    ) -> Option<u64> {
        None
    }
    fn move_decision(&self, _acted: Option<u64>, _requested: u64) -> project::MoveDecision {
        project::MoveDecision::Claim
    }
    fn acquisition_ready(&self, _observation: &project::OwnershipObservation) -> bool {
        false
    }
    fn pull_move<'a>(
        &'a self,
        _owner: project::ProjectOwner,
        _ask: bool,
        _expected_epoch: Option<u64>,
        _bound: std::time::Duration,
        _stage: &'a mut &'static str,
    ) -> SnapshotFuture<'a> {
        Box::pin(async { Err(anyhow::anyhow!("optional_runtime_unavailable")) })
    }
    fn hand_over_move(&self, _owner: project::MoveYieldOwner) -> ProjectUnitFuture {
        Box::pin(async { Err(anyhow::anyhow!("optional_runtime_unavailable")) })
    }
    fn reconcile(&self, _owner: project::ProjectOwner) -> ProjectFuture {
        Box::pin(async { Err(anyhow::anyhow!("optional_runtime_unavailable")) })
    }
    fn viewer<'a>(
        &'a self,
        admission: viewer::ViewerAdmission<'a>,
        downstream: &'a mut viewer::ViewerDownstream,
    ) -> ViewerFuture<'a> {
        Box::pin(admission.optional_unavailable(downstream))
    }
}

#[cfg(all(unix, feature = "daemon-extension-fixture"))]
pub use crate::pro::engine::coordinator_host::fixture;
