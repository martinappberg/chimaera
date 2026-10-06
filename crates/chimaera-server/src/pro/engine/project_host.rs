//! Original per-project authority and effects for trusted optional policy.
//! No store, credential, arbitrary request or successor project is exported.
use super::*;
#[cfg(all(unix, feature = "daemon-extension-fixture"))]
pub mod fixture;
pub(crate) mod move_host;
pub(crate) mod policy_host;
pub use crate::pro::config_wire::Report as ConfigurationReport;
pub use crate::pro::place::ConversationStays;
pub use crate::pro::projects::catalog::Metadata as ProjectMetadata;

/// One public route/coordinator admission. Cloning retains the same original
/// state/configuration/key; it does not capture a successor configuration.
#[derive(Clone)]
pub struct ProjectOwner {
    state: Arc<AppState>,
    config: Configure,
    workspace: String,
    generation: u64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProjectRole {
    Device,
    Worker,
}
/// Account observation fields are descriptive. Original proof bytes remain
/// private to the host and are used for accept/observe on this same owner.
pub struct ProjectBaton {
    pub workspace_id: String,
    pub holder_id: Option<String>,
    pub epoch: u64,
    pub expires_at: Option<String>,
    pub server_now: String,
    pub mirror_disabled: bool,
    pub move_to: Option<String>,
    pub continuity: bool,
    pub checkpoint: bool,
    original: Baton,
}
impl ProjectBaton {
    fn capture(original: Baton) -> Self {
        Self {
            workspace_id: original.workspace_id.clone(),
            holder_id: original.holder_id.clone(),
            epoch: original.epoch,
            expires_at: original.expires_at.clone(),
            server_now: original.server_now.clone(),
            mirror_disabled: original.mirror_disabled,
            move_to: original.move_to.clone(),
            continuity: original.continuity.is_some(),
            checkpoint: original.checkpoint.is_some(),
            original,
        }
    }
}
/// Effective authority remains sealed, tied to the exact original capture.
pub struct ProjectConfiguration {
    original: Configure,
    owner: ProjectOwner,
}
impl ProjectConfiguration {
    pub fn negotiated(&self) -> bool {
        self.original.execution.is_some()
    }
}
#[derive(Clone, Copy)]
pub enum LeaseOperation {
    Acquire,
    Renew,
}
pub struct ProjectLeaseReply {
    response: transport::Response,
    started: execution::RequestStart,
}
impl ProjectLeaseReply {
    pub fn status(&self) -> u16 {
        self.response.status
    }
    pub fn takeover_grace(&self) -> bool {
        self.response.status == 409
            && serde_json::from_slice::<serde_json::Value>(&self.response.body)
                .is_ok_and(|value| value["error"] == "takeover_grace")
    }
    pub fn baton(self) -> Result<(ProjectBaton, ProjectLeaseStart)> {
        Ok((
            ProjectBaton::capture(
                self.response
                    .json()
                    .context("Could not confirm project ownership")?,
            ),
            ProjectLeaseStart(self.started),
        ))
    }
}
pub struct ProjectLeaseStart(execution::RequestStart);
pub struct IdleWorker {
    _job: tokio::sync::OwnedMutexGuard<()>,
    fenced: bool,
}
impl IdleWorker {
    pub fn fenced(&self) -> bool {
        self.fenced
    }
}

impl ProjectOwner {
    pub(super) fn capture(
        state: Arc<AppState>,
        config: Configure,
        workspace: String,
        generation: u64,
    ) -> Result<Self> {
        authority::config_matches(&state, &config, &workspace)?;
        Ok(Self {
            state,
            config,
            workspace,
            generation,
        })
    }
    fn check_configuration(&self, configuration: &ProjectConfiguration) -> Result<()> {
        ensure!(
            Arc::ptr_eq(&self.state, &configuration.owner.state)
                && self.workspace == configuration.owner.workspace
                && self.generation == configuration.owner.generation,
            "project configuration owner changed"
        );
        Ok(())
    }
    pub fn id(&self) -> &str {
        &self.workspace
    }
    pub fn generation_current(&self) -> bool {
        self.generation == self.state.pro.generation.load(Ordering::Acquire)
    }
    pub fn role(&self) -> ProjectRole {
        if self.config.role == Role::Worker {
            ProjectRole::Worker
        } else {
            ProjectRole::Device
        }
    }
    pub fn holder(&self) -> &str {
        &self.config.delegation.device_id
    }
    pub fn copy_only(&self) -> bool {
        super::super::project_copy::copy_only(&self.state, &self.workspace)
    }
    pub fn account_matches(&self) -> bool {
        super::super::projects::account_matches(&self.state, &self.workspace)
    }
    pub fn pulling(&self) -> bool {
        super::super::moves::pulling(&self.state, &self.workspace)
    }
    pub fn release_pending(&self) -> bool {
        lock(&self.state.pro.release_pending).contains(&self.workspace)
    }
    pub fn ownership(&self) -> Option<Ownership> {
        lock(&self.state.pro.ownership)
            .get(&self.workspace)
            .cloned()
    }
    pub fn set_ownership(&self, value: Option<Ownership>) {
        let mut entries = lock(&self.state.pro.ownership);
        match value {
            Some(value) => {
                entries.insert(self.workspace.clone(), value);
            }
            None => {
                entries.remove(&self.workspace);
            }
        }
    }
    pub fn copy_epoch(&self, epoch: u64) {
        if let Some(copy) = lock(&self.state.pro.preferences)
            .get_mut(&self.workspace)
            .and_then(|p| p.copy.as_mut())
        {
            copy.owner_epoch = Some(epoch);
        }
    }
    pub fn takeover_requested(&self) -> bool {
        lock(&self.state.pro.preferences)
            .get(&self.workspace)
            .and_then(|p| p.copy.as_ref())
            .is_some_and(|copy| copy.takeover_requested)
    }
    pub fn privacy_disabled(&self) {
        let mut entries = lock(&self.state.pro.preferences);
        let preference = entries.entry(self.workspace.clone()).or_default();
        preference.never_mirror = true;
        preference.privacy_pending = false;
    }
    pub fn answered(&self) {
        let mut entries = lock(&self.state.pro.answered);
        if entries.len() < 128 || entries.contains(&self.workspace) {
            entries.insert(self.workspace.clone());
        }
    }
    pub async fn configuration(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.state.pro.configuration.clone().lock_owned().await
    }
    pub async fn persist(&self) -> Result<()> {
        super::super::persist(&self.state).await
    }
    pub async fn read_owner(&self) -> Result<ProjectBaton> {
        let path = execution::path(&self.config, &self.workspace, "");
        let response = self.reached(super::account(&self.config, &path, "GET", None).await)?;
        Ok(ProjectBaton::capture(response.json()?))
    }
    /// The lease loop's own account calls are what "reachable" means
    /// (`reach`): any answer counts, a server error or no answer does not.
    fn reached(
        &self,
        response: Result<super::super::transport::Response>,
    ) -> Result<super::super::transport::Response> {
        match &response {
            Ok(response) => super::super::reach::answered(&self.state, response.status),
            Err(_) => super::super::reach::unreachable(&self.state),
        }
        response
    }
    pub fn observe(&self, baton: &ProjectBaton) -> Result<()> {
        ensure!(
            baton.original.workspace_id == self.workspace,
            "project observation changed"
        );
        execution::observe(&self.state, &self.config, &baton.original)?;
        super::super::place::observed(&self.state, &self.config, &baton.original);
        Ok(())
    }
    pub fn effective(&self, unowned_device: bool) -> Result<ProjectConfiguration> {
        Ok(ProjectConfiguration {
            owner: self.clone(),
            original: if unowned_device {
                self.config.clone()
            } else {
                execution::effective(&self.state, &self.config, &self.workspace)?
            },
        })
    }
    pub fn answer_move(&self, baton: &ProjectBaton) {
        super::super::moves::answer(&self.state, &self.config, &self.workspace, &baton.original);
    }
    pub fn abandoned_move(&self, baton: &ProjectBaton, previous: Option<&Ownership>) -> bool {
        super::super::moves::abandoned(&self.state, &baton.original, previous)
    }
    pub fn consider_move(&self, baton: &ProjectBaton) {
        super::super::moves::consider(&self.state, &self.config, &self.workspace, &baton.original);
    }
    pub fn resuming(&self) -> bool {
        execution::resuming(&self.state, &self.workspace)
    }
    pub fn fence(&self) {
        execution::fence_workspace(&self.state, &self.workspace);
    }
    pub fn checkpoint_mode(&self) -> bool {
        execution::checkpoint_mode(&self.state, &self.workspace)
    }
    pub fn parked(&self) -> bool {
        super::super::parked(&self.state, &self.workspace)
    }
    pub fn thawed(&self) {
        execution::thawed(&self.state);
    }
    pub fn proof_epoch(&self) -> Option<u64> {
        execution::proof_epoch(&self.state, &self.workspace)
    }
    pub fn held_here(&self, baton: &ProjectBaton) -> bool {
        execution::held_here(&self.state, &self.config, &baton.original)
    }
    pub fn adopt_running(&self) {
        execution::adopt_running(&self.state, &self.workspace);
    }
    pub fn worker(&self) -> bool {
        execution::worker(&self.state)
    }
    pub fn uncertain(&self) -> bool {
        execution::uncertain(&self.state, &self.workspace)
    }
    pub fn unclean(&self) -> bool {
        execution::unclean(&self.state, &self.workspace)
    }
    pub fn fenced(&self) -> bool {
        execution::fenced(&self.state, &self.workspace)
    }
    pub async fn request_lease(
        &self,
        config: &ProjectConfiguration,
        epoch: u64,
        operation: LeaseOperation,
    ) -> Result<ProjectLeaseReply> {
        self.check_configuration(config)?;
        let acquire = matches!(operation, LeaseOperation::Acquire);
        let body = execution::body(&config.original, epoch, acquire);
        let started = execution::RequestStart::now();
        let response = self
            .reached(
                super::account(
                    &config.original,
                    &execution::path(
                        &config.original,
                        &self.workspace,
                        if acquire { "acquire" } else { "renew" },
                    ),
                    "POST",
                    Some(&body),
                )
                .await,
            )
            .context("Could not renew project ownership")?;
        Ok(ProjectLeaseReply { response, started })
    }
    pub fn accept(
        &self,
        config: &ProjectConfiguration,
        baton: &ProjectBaton,
        started: ProjectLeaseStart,
    ) -> Result<()> {
        self.check_configuration(config)?;
        ensure!(
            baton.original.workspace_id == self.workspace,
            "project grant changed"
        );
        execution::accept(
            &self.state,
            &config.original,
            &baton.original,
            self.generation,
            started.0,
        )
    }
    pub fn bind_account(&self) -> Result<()> {
        super::super::projects::bind_workspace_account(&self.state, &self.config, &self.workspace)
    }
    pub async fn stop_after_verified_owner(&self) -> Result<()> {
        super::stop_after_verified_owner(&self.state, &self.config, &self.workspace).await
    }
    pub async fn suspend(&self) -> Result<()> {
        super::suspend_workspace(&self.state, &self.workspace).await
    }
    /// The original operation generation survives destination/cache admission.
    /// A private policy cannot adopt a successor while this effect waits.
    pub async fn hydrate(&self, epoch: u64, fork: bool, destination: Option<&Path>) -> Result<()> {
        ensure!(
            self.generation_current(),
            "Account changed during project transfer"
        );
        Box::pin(super::hydrate_generation(
            &self.state,
            &self.config,
            &self.workspace,
            epoch,
            fork,
            destination,
            self.generation,
        ))
        .await
    }
    pub async fn finish_existing_hydration(&self, epoch: u64) -> Result<()> {
        super::finish_hydration(&self.state, &self.workspace, epoch, self.generation).await
    }
    pub async fn resume_deferred(&self) -> Result<()> {
        crate::ledger::resume_deferred_workspace(&self.state, &self.workspace).await
    }
    pub fn resume_allowed(&self) -> bool {
        execution::resume_allowed(&self.state, &self.workspace)
    }
    /// The original worker idle compare and jobs owner remain one host effect.
    pub async fn fence_idle_worker(
        &self,
        previous: &Option<Ownership>,
        epoch: u64,
    ) -> Result<Option<IdleWorker>> {
        let Ok(job) = self.state.pro.jobs.clone().try_lock_owned() else {
            return Ok(None);
        };
        let _configuration = self.state.pro.configuration.lock().await;
        ensure!(
            self.generation_current(),
            "Account changed during project transfer"
        );
        let mut ownership = lock(&self.state.pro.ownership);
        let fenced = ownership.get(&self.workspace) == previous.as_ref()
            && !matches!(
                previous,
                Some(
                    Ownership::Transferring { .. }
                        | Ownership::Hydrating { .. }
                        | Ownership::SettingUp { .. }
                )
            );
        if fenced {
            ownership.insert(self.workspace.clone(), Ownership::Hydrating { epoch });
        }
        Ok(Some(IdleWorker { _job: job, fenced }))
    }
    /// Jobs admission, immediate fence and original detached task stay public.
    pub async fn schedule_install(&self, epoch: u64) -> Result<bool> {
        let Ok(job) = self.state.pro.jobs.clone().try_lock_owned() else {
            return Ok(false);
        };
        lock(&self.state.pro.installing).insert(self.workspace.clone());
        self.set_ownership(Some(Ownership::AwaitingVerification { epoch }));
        if let Err(error) = self.persist().await {
            lock(&self.state.pro.installing).remove(&self.workspace);
            return Err(error);
        }
        let owner = self.state.clone();
        let config = self.config.clone();
        let key = self.workspace.clone();
        tokio::spawn(async move {
            super::super::detached::run(
                &owner.clone(),
                ("install", false),
                &key.clone(),
                epoch,
                || None,
                move || async move {
                    let _job = job;
                    let result =
                        super::install_owned(owner.clone(), config, key.clone(), epoch).await;
                    lock(&owner.pro.installing).remove(&key);
                    if let Err(error) = result {
                        super::record_error(&owner, &key, &error);
                        return super::super::detached::Outcome::refused(
                            axum::http::StatusCode::CONFLICT,
                            None,
                        );
                    }
                    super::super::detached::Outcome::done()
                },
            )
            .await
        });
        Ok(true)
    }
}

/// A publication admission carries the already checked root, original session
/// roster, compatible image and original transfer/cache owner through policy.
pub struct SnapshotOwner {
    pub(super) project: ProjectOwner,
    pub(super) effective: Configure,
    pub(super) requested: Configure,
    pub(super) workspace: crate::workspaces::Workspace,
    pub(super) epoch: u64,
    pub(super) session_ids: Vec<String>,
    /// The conversations "Run in the cloud" must carry (`engine::leaving_sessions`).
    pub(super) must_carry: Vec<String>,
    /// A conversation was working or waiting on the user when this copy
    /// started (`engine::needs_cloud`), captured before anything stopped.
    pub(super) needs_cloud: bool,
    pub(super) companion: std::sync::Mutex<Option<super::super::companion::CompatibleImage>>,
    pub(super) transfer: super::super::transfer_dispatch::TransferScope,
    pub(super) grant: super::super::transfer_host::MirrorGrant,
    pub(super) sleep: Option<Sleep>,
    pub(super) clean: bool,
}
impl SnapshotOwner {
    pub fn project(&self) -> &ProjectOwner {
        &self.project
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn name(&self) -> &str {
        &self.workspace.name
    }
    pub fn visible(&self) -> bool {
        !self.workspace.cloud_internal && !self.workspace.hidden
    }
    pub fn root(&self) -> &Path {
        &self.workspace.root
    }
    pub fn sessions(&self) -> &[String] {
        &self.session_ids
    }
    pub fn clean(&self) -> bool {
        self.clean
    }
    pub fn recovery(&self) -> bool {
        self.effective.recovery
    }
    pub fn negotiated(&self) -> bool {
        self.effective.execution.is_some()
    }
    pub fn sleep(&self) -> Option<Sleep> {
        self.sleep
    }
    pub fn woke(&self) -> bool {
        self.sleep
            .is_some_and(|sleep| sleep.woke(&self.project.state))
    }
    pub fn transfer(&self) -> Arc<super::super::transfer_host::TransferHost> {
        self.transfer.host.clone()
    }
    pub fn grant(&self) -> &super::super::transfer_host::MirrorGrant {
        &self.grant
    }
    pub fn cache_root(&self) -> Result<PathBuf> {
        self.transfer
            .host
            .captured_path(&self.project.state.pro.root.join(self.project.id()))
    }
    pub async fn clear_interrupted(&self) -> Result<()> {
        let owner = self.transfer.host.clone();
        let root = self.cache_root()?;
        tokio::task::spawn_blocking(move || {
            owner.current()?;
            let _retained = owner;
            mirror::clear_interrupted(&root)
        })
        .await?
    }
    pub fn agent_ids(&self) -> Vec<String> {
        super::sessions(&self.project.state, self.project.id())
    }
    pub fn continuation(&self) -> execution::wire::Continuation {
        super::continuation(&self.project.state, self.project.id())
    }
    pub fn check_roster(&self) -> Result<()> {
        ensure!(
            super::transfer_session_ids(&self.project.state, self.project.id())?
                .iter()
                .all(|id| self.session_ids.contains(id)),
            "Project sessions changed during transfer; try again"
        );
        Ok(())
    }
    pub async fn export_session(&self, id: &str) -> Result<Option<PathBuf>> {
        ensure!(
            self.session_ids.iter().any(|original| original == id),
            "Session is not in the original transfer roster"
        );
        crate::bundle::export_for_mirror(
            self.project.state.clone(),
            id,
            if self.clean {
                crate::bundle::ExportMode::Stop
            } else {
                crate::bundle::ExportMode::Snapshot
            },
        )
        .await
    }
    /// Whether the cloud should start for this copy if this computer goes
    /// away: published as the account's `has_agents`, so an idle project
    /// costs nothing until someone opens it.
    pub fn needs_cloud(&self) -> bool {
        self.needs_cloud
    }
    /// The working or waiting conversations "Run in the cloud" carries: every
    /// one of them or none ([`ConversationStays`]), decided before anything
    /// stops. Empty for every other flush.
    pub fn must_carry(&self) -> &[String] {
        &self.must_carry
    }
    /// Whether a conversation can travel, without stopping it: the size of
    /// a fresh copy of it (the copy itself is removed at once).
    pub async fn probe_session(&self, id: &str) -> Result<u64> {
        ensure!(
            self.session_ids.iter().any(|original| original == id),
            "Session is not in the original transfer roster"
        );
        let path = crate::bundle::export_for_mirror(
            self.project.state.clone(),
            id,
            crate::bundle::ExportMode::Snapshot,
        )
        .await?
        .ok_or(ConversationStays::NotSaved)?;
        let length = tokio::fs::metadata(&path).await.map(|meta| meta.len());
        let _ = tokio::fs::remove_file(&path).await;
        Ok(length?)
    }
    pub async fn park_session(&self, id: &str) {
        if self.session_ids.iter().any(|original| original == id) {
            super::park_here(&self.project.state, id).await;
        }
    }
    pub async fn stop_execution(&self) -> Result<()> {
        execution::stop(
            &self.project.state,
            std::slice::from_ref(&self.project.workspace),
        )
        .await
    }
    pub async fn export_configuration(
        &self,
        destination: &Path,
        budget: u64,
    ) -> Result<config::Report> {
        let image = lock(&self.companion)
            .take()
            .context("Original companion already consumed")?;
        let state = &self.project.state;
        let home = state
            .claude_settings_path
            .parent()
            .and_then(Path::parent)
            .context("agent home unavailable")?
            .to_path_buf();
        let sources = config::Sources {
            home,
            claude: state.claude_settings_path.parent().unwrap().to_path_buf(),
            codex: state
                .codex_config_path
                .parent()
                .context("codex home unavailable")?
                .to_path_buf(),
            workspace: self.workspace.root.clone(),
        };
        config::export_with_image(sources, destination, budget, image).await
    }
    pub fn metadata(&self) -> Option<super::super::projects::catalog::Metadata> {
        super::super::projects::catalog::metadata(self.name(), self.visible())
    }
    pub async fn describe_repository(&self) -> Result<super::super::repository::Described> {
        super::super::repository::describe(&self.project.state.pro, self.project.id(), self.root())
            .await
    }
    pub async fn publish_policy(&self, has_agents: bool) -> Result<()> {
        super::publish_policy(&self.effective, self.project.id(), self.epoch, has_agents).await
    }
    pub async fn confirm_checkpoint(
        &self,
        objects: [&str; 3],
        continuation: execution::wire::Continuation,
    ) -> Result<()> {
        execution::receipt::published(
            &self.effective,
            self.project.id(),
            self.epoch,
            objects,
            continuation,
        )
        .await
        .map(|_| ())
    }
    pub fn record_publication(
        &self,
        missing_environment: Vec<String>,
        tree: String,
        handoff: String,
        report: super::super::mirror::Report,
    ) {
        let state = &self.project.state;
        let workspace = self.project.id();
        {
            let mut entries = lock(&state.pro.preferences);
            let preference = entries.entry(workspace.into()).or_default();
            preference.missing_environment = missing_environment;
            preference.published_tree = Some(tree);
            if self.negotiated() {
                preference.published_handoff = Some(handoff);
            }
        }
        {
            let mut statuses = lock(&state.pro.status);
            let previous = statuses.remove(workspace).unwrap_or_default();
            statuses.insert(
                workspace.into(),
                WorkspaceStatus {
                    report,
                    last_mirrored_at: Some(super::super::now()),
                    storage_limit_bytes: self.grant.storage_limit_bytes(),
                    error: None,
                    error_code: None,
                    git_staging: previous.git_staging,
                    kept_both: previous.kept_both,
                    kept_paths: previous.kept_paths,
                    kept_at: previous.kept_at,
                    kept_total: previous.kept_total,
                    blocked_providers: Vec::new(),
                },
            );
        }
    }
    pub async fn release(&self, budget: Duration) -> Result<()> {
        self.project
            .state
            .daemon_extension
            .as_ref()
            .context("optional_runtime_unavailable")?
            .release(self.release_owner(), budget)
            .await
    }
    pub fn upgrade_required(&self, error: &anyhow::Error) -> bool {
        super::upgrade_required(error)
    }
    pub fn require_upgrade(&self) {
        execution::require_v2(&self.project.state, self.project.id());
    }
    pub async fn resume_stopped(&self, ids: &[String]) -> Result<()> {
        ensure!(
            ids.iter().all(|id| self.session_ids.contains(id)),
            "Session is not in the original transfer roster"
        );
        crate::ledger::resume_deferred_sessions(&self.project.state, self.project.id(), ids).await
    }
    pub fn release_pending(&self) {
        lock(&self.project.state.pro.release_pending).insert(self.project.workspace.clone());
    }
    pub fn unpark(&self) {
        super::super::unpark(&self.project.state, self.project.id());
    }
    pub async fn recover_failed_publication(&self, must_upgrade: bool) -> Result<()> {
        let recover = {
            let _configuration = self.project.state.pro.configuration.lock().await;
            let mut ownership = lock(&self.project.state.pro.ownership);
            if self.project.generation_current()
                && matches!(ownership.get(self.project.id()), Some(Ownership::Transferring{epoch}) if *epoch==self.epoch)
            {
                ownership.insert(
                    self.project.workspace.clone(),
                    Ownership::AwaitingVerification { epoch: self.epoch },
                );
                true
            } else {
                false
            }
        };
        if recover {
            let config = if must_upgrade {
                &self.requested
            } else {
                &self.effective
            };
            let _ = super::reconcile_generation(
                &self.project.state,
                config,
                self.project.id(),
                self.project.generation,
            )
            .await;
        }
        Ok(())
    }
}

/// Descriptive snapshot fields; these create no installation authority.
pub struct SnapshotDescription {
    pub id: String,
    pub root: PathBuf,
    pub name: String,
}
pub fn snapshot_failure(error: &anyhow::Error) -> &'static str {
    super::failure_code(error)
}
