//! Finite paid repository operations over one original transfer owner.
//! These internal values do not create account, process or installation authority.
use super::{
    transfer_host::{MirrorGrant, TransferHost},
    transfer_types::*,
};
use anyhow::{bail, Result};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

pub type TransferFuture<'a> = Pin<Box<dyn Future<Output = Result<TransferReply>> + Send + 'a>>;
pub enum TransferRequest<'a> {
    PrepareReturnRepository(ReturnRepository<'a>),
    StagingBaseline {
        shadow: &'a Path,
        stage: &'a Path,
        published: Option<&'a str>,
        budget: u64,
    },
    CopyBaseline {
        shadow: &'a Path,
        stage: &'a Path,
        checkpoint: &'a super::execution::wire::Checkpoint,
        budget: u64,
    },
    PrepareShadow {
        shadow: &'a Path,
        incoming: &'a Path,
    },
    Initialize {
        path: &'a Path,
        format: &'a str,
    },
    SetAsideDamaged {
        shadow: &'a Path,
    },
    FetchPublished {
        shadow: &'a Path,
        grant: &'a MirrorGrant,
    },
    Inventory {
        root: &'a Path,
        shadow: &'a Path,
    },
    CommitTree {
        repository: &'a Path,
        tree: &'a Path,
        branch: &'a str,
    },
    Push {
        repository: &'a Path,
        grant: &'a MirrorGrant,
        branches: &'a [&'a str],
    },
    MirrorRepository {
        root: &'a Path,
        cache: &'a Path,
        grant: &'a MirrorGrant,
    },
    ValidateTree {
        repository: &'a Path,
        revision: &'a str,
        budget: u64,
        max_file: u64,
    },
    RepositoryOrigin {
        root: &'a Path,
    },
    Describe {
        root: &'a Path,
    },
    KeepSourceHead {
        root: &'a Path,
        cache: &'a Path,
    },
    PrepareReceive {
        original: &'a Path,
        checkout: &'a Path,
        stage: &'a Path,
        incoming: Incoming<'a>,
        check: &'a (dyn Fn() -> Result<()> + Sync),
    },
    CaptureStaging {
        root: &'a Path,
        artifact: &'a Path,
        budget: u64,
        max_file: u64,
    },
    RequireServiceFormat {
        artifact: &'a Path,
        descriptor: &'a Descriptor,
    },
}
pub enum TransferReply {
    ReturnRepository(Prepared, bool),
    StagingBaseline(Option<(PathBuf, Descriptor)>),
    ShadowPrepared(Option<super::shadow_cache::PreparedShadow>),
    Unit,
    Damaged(bool),
    Paths(Vec<PathBuf>, Vec<PathBuf>),
    Object(String),
    Bytes(u64),
    Origin(Option<String>),
    Described(Described),
    Prepared(Prepared),
    Captured(Descriptor, u64),
}
impl TransferReply {
    pub(crate) fn unit(self) -> Result<()> {
        match self {
            Self::Unit => Ok(()),
            _ => bail!("optional transfer runtime returned an invalid result"),
        }
    }
}

/// This scope borrows the host's exact cache owner. It is not another state
/// store; all authoritative reads and writes still use the original AppState.
#[derive(Clone)]
pub(super) struct TransferScope {
    pub runtime: Arc<dyn crate::daemon_extension::Runtime>,
    pub host: Arc<TransferHost>,
}
impl TransferScope {
    pub async fn capture(
        state: &Arc<crate::AppState>,
        workspace: &str,
        source: Option<&Path>,
        cache: Arc<tokio::sync::OwnedMutexGuard<()>>,
        generation: u64,
    ) -> Result<Self> {
        let runtime = state
            .daemon_extension
            .clone()
            .ok_or_else(|| anyhow::anyhow!("optional_runtime_unavailable"))?;
        let host =
            TransferHost::capture(state.clone(), workspace, source, cache, generation).await?;
        Ok(Self { runtime, host })
    }
}
tokio::task_local! { static TRANSFER: TransferScope; }
pub(super) async fn scope<T>(owner: TransferScope, future: impl Future<Output = T>) -> T {
    TRANSFER.scope(owner, future).await
}
pub(super) fn owner() -> Result<TransferScope> {
    TRANSFER
        .try_with(Clone::clone)
        .map_err(|_| anyhow::anyhow!("optional_runtime_unavailable"))
}
pub(super) async fn call(request: TransferRequest<'_>) -> Result<TransferReply> {
    let original = owner()?;
    original.host.current()?;
    original.runtime.transfer(original.host, request).await
}
