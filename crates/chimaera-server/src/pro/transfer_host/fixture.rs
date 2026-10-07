//! Explicit disposable-root fixture effects for original repository tests.
//! These entry points are absent from ordinary builds and use the same live
//! installation and retained transport implementations as the public host.
use super::*;
use crate::pro::install;

/// Original synthetic wire credentials, sealed to the fixture's one owner.
/// This is not a production enrollment or a login receipt.
pub fn grant(owner: &Arc<TransferHost>, wire: serde_json::Value) -> Result<MirrorGrant> {
    ensure!(
        serde_json::to_vec(&wire)?.len() <= 4096,
        "fixture grant exceeds bound"
    );
    let mut credentials: MirrorCredentials = serde_json::from_value(wire)?;
    ensure!(
        credentials.workspace_id == "w-fixture",
        "unknown synthetic fixture grant"
    );
    // Each disposable test owns a distinct real cache/quarantine key. The
    // original fixtures use this fixed synthetic label, never an account proof.
    credentials.workspace_id = owner.workspace.clone();
    owner.grant(credentials)
}
pub fn temp_name(value: &OsStr) -> OsString {
    crate::persist::project_temp_name(value)
}

#[derive(Clone)]
pub struct Binding {
    pub endpoint: String,
    pub account: Option<String>,
    pub workspace: String,
    pub epoch: u64,
    pub receipt: Option<String>,
}
impl Binding {
    fn original(&self) -> install::Binding {
        install::Binding {
            endpoint: self.endpoint.clone(),
            account: self.account.clone(),
            workspace: self.workspace.clone(),
            epoch: self.epoch,
            receipt: self.receipt.clone(),
        }
    }
}
/// No duplicate transaction interpreter. The original cache owner remains
/// attached to the real public transaction until its final cleanup/drop.
pub struct Transaction {
    owner: Arc<TransferHost>,
    inner: install::Transaction,
}
impl Transaction {
    pub fn prepare(
        owner: Arc<TransferHost>,
        path: &Path,
        binding: Binding,
        writes: Vec<install::Write>,
        budget: u64,
    ) -> Result<Self> {
        owner.staging(path)?;
        for write in &writes {
            owner.path(&write.root)?;
            for staged in [write.before.as_ref(), write.after.as_ref()]
                .into_iter()
                .flatten()
            {
                owner.path(staged.parent().context("fixture write parent absent")?)?;
            }
        }
        Ok(Self {
            owner,
            inner: install::Transaction::prepare(path, binding.original(), writes, budget)?,
        })
    }
    pub fn open(owner: Arc<TransferHost>, path: &Path, binding: &Binding) -> Result<Option<Self>> {
        owner.path(path)?;
        Ok(install::Transaction::open(path, &binding.original())?
            .map(|inner| Self { owner, inner }))
    }
    pub fn reserve_git(
        &mut self,
        roots: Vec<PathBuf>,
        current: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        for root in &roots {
            self.owner.path(root)?;
        }
        self.inner.reserve_git(roots, current)
    }
    pub fn apply(&mut self, current: &dyn Fn() -> Result<()>) -> Result<()> {
        self.owner.current()?;
        self.inner.apply(current)
    }
    pub fn commit(&mut self, current: &dyn Fn() -> Result<()>) -> Result<()> {
        self.owner.current()?;
        self.inner.commit(current)
    }
    pub fn cleanup(self) -> Result<()> {
        self.inner.cleanup()
    }
}

/// Fixed baseline call over the same original fixture transfer scope.
pub async fn staging_baseline(
    owner: Arc<TransferHost>,
    shadow: &Path,
    stage: &Path,
    published: Option<String>,
    budget: u64,
) -> Result<Option<(PathBuf, super::super::transfer_types::Descriptor)>> {
    validate_baseline_paths(owner.clone(), shadow.to_owned(), stage.to_owned()).await?;
    let runtime = owner
        .state
        .pro()
        .runtime()
        .cloned()
        .context("optional_runtime_unavailable")?;
    super::super::transfer_dispatch::scope(
        super::super::transfer_dispatch::TransferScope {
            runtime,
            host: owner,
        },
        super::super::engine::staging_baseline(shadow, stage, published, budget),
    )
    .await
}
pub async fn copy_baseline(
    owner: Arc<TransferHost>,
    shadow: &Path,
    stage: &Path,
    checkpoint: &super::super::execution::wire::Checkpoint,
    budget: u64,
) -> Result<Option<(PathBuf, super::super::transfer_types::Descriptor)>> {
    validate_baseline_paths(owner.clone(), shadow.to_owned(), stage.to_owned()).await?;
    let runtime = owner
        .state
        .pro()
        .runtime()
        .cloned()
        .context("optional_runtime_unavailable")?;
    super::super::transfer_dispatch::scope(
        super::super::transfer_dispatch::TransferScope {
            runtime,
            host: owner,
        },
        super::super::engine::takeover_copy_baseline(shadow, stage, checkpoint, budget),
    )
    .await
}
async fn validate_baseline_paths(
    owner: Arc<TransferHost>,
    shadow: PathBuf,
    stage: PathBuf,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        owner.current()?;
        owner.path(&shadow)?;
        // The original baseline operation creates this directory only after
        // validating the acknowledged commit. Admission must not create it.
        owner.staging(&stage)
    })
    .await?
}
/// The original public live-file CAS, confined to this retained fixture root.
pub async fn install_tree(
    owner: Arc<TransferHost>,
    incoming: PathBuf,
    local: PathBuf,
    baseline: Option<PathBuf>,
    left_out: Option<Vec<PathBuf>>,
) -> Result<(usize, Vec<PathBuf>)> {
    tokio::task::spawn_blocking(move || {
        owner.current()?;
        for path in [&incoming, &local].into_iter().chain(baseline.as_ref()) {
            owner.path(path)?;
        }
        super::super::engine::install_tree(
            &incoming,
            &local,
            baseline.as_deref(),
            left_out.as_deref(),
        )
    })
    .await?
}
