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
pub use crate::workspace_maintenance::{PreparedWorkspace, WorkspaceHost};
pub type EnvironmentFuture<'a> =
    Pin<Box<dyn Future<Output = anyhow::Result<Vec<(String, String)>>> + Send + 'a>>;
pub type RuntimeFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// A first-party in-process policy. It never receives AppState, a token or a
/// generic process/HTTP callback. The public host owns its spawned continuation.
pub trait Runtime: Send + Sync + 'static {
    /// Stateless local composition identity (1..=128 visible ASCII bytes),
    /// independent of public SDK parity.
    fn assembly_identity(&self) -> Option<&'static str> {
        None
    }

    /// A transient child-process overlay, never settings, argv or a saved recipe.
    /// The host retains launch admission until the actual session is registered.
    fn session_environment<'a>(
        &'a self,
        _workspace: &'a str,
        _worker: bool,
    ) -> EnvironmentFuture<'a> {
        Box::pin(async { Ok(Vec::new()) })
    }
    /// Selected composition only; mounted beneath /api/v1/extensions with the
    /// ordinary daemon bearer check. An absent extension adds no routes.
    fn workspace_routes(&self, _host: WorkspaceHost) -> axum::Router {
        axum::Router::new()
    }

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

/// Resolve only at the final spawn point, under the original workspace admission.
/// Extension failure never silently starts a child with a different environment.
pub(crate) async fn apply_session_environment(
    state: &crate::AppState,
    workspace: &str,
    env: &mut Vec<(String, String)>,
    remove: &mut Vec<String>,
) -> anyhow::Result<()> {
    let Some(extension) = state.daemon_extension.as_ref() else {
        return Ok(());
    };
    let generation = crate::pro::mutation::generation(state);
    let values = extension
        .session_environment(workspace, crate::pro::mutation::maintenance_worker(state))
        .await
        .map_err(|_| anyhow::anyhow!("workspace environment unavailable"))?;
    anyhow::ensure!(
        generation == crate::pro::mutation::generation(state)
            && crate::pro::may_execute(state, workspace),
        "workspace environment changed"
    );
    validate_environment(&values, env)?;
    for (name, value) in values {
        remove.retain(|old| old != &name);
        env.push((name, value));
    }
    Ok(())
}
fn validate_environment(
    values: &[(String, String)],
    env: &[(String, String)],
) -> anyhow::Result<()> {
    let mut names = std::collections::HashSet::new();
    let valid = values.len() <= 32
        && values.iter().all(|(name, value)| {
            !name.is_empty()
                && name.len() <= 128
                && name.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                })
                && !name.starts_with("CHIMAERA_")
                && value.len() <= 8192
                && !value.contains('\0')
                && names.insert(name.as_str())
                && !env.iter().any(|(old, _)| old == name)
        });
    anyhow::ensure!(valid, "workspace environment refused");
    Ok(())
}

#[cfg(test)]
mod environment_tests {
    use super::*;
    #[tokio::test]
    async fn absent_extension_preserves_existing_free_launch_environment() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-free-environment-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = crate::AppState::new(
            "synthetic".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        );
        let mut env = vec![("EXISTING_VALUE".into(), "x".repeat(9000))];
        let mut remove = vec!["INHERITED_VALUE".into()];
        let original = (env.clone(), remove.clone());
        assert!(crate::pro::mutation::begin_launch(&state, "ordinary-local")
            .unwrap()
            .is_none());
        apply_session_environment(&state, "ordinary-local", &mut env, &mut remove)
            .await
            .unwrap();
        assert_eq!((env, remove), original);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn environment_rejects_ambiguous_or_host_owned_values_without_logging_them() {
        let existing = vec![("PATH".into(), "existing".into())];
        assert!(
            validate_environment(&[("PROJECT_TOKEN".into(), "synthetic".into())], &existing)
                .is_ok()
        );
        for values in [
            vec![("PATH".into(), "synthetic".into())],
            vec![("CHIMAERA_SESSION".into(), "synthetic".into())],
            vec![("A=B".into(), "synthetic".into())],
            vec![("A".into(), "embedded\0value".into())],
            vec![("A".into(), "first".into()), ("A".into(), "second".into())],
            vec![("A".into(), "x".repeat(8193))],
        ] {
            assert_eq!(
                validate_environment(&values, &existing)
                    .unwrap_err()
                    .to_string(),
                "workspace environment refused"
            );
        }
    }
}
