//! Fixed transport effects for the original paid retry/return policy. These
//! owners retain one captured configuration; response bytes inherit the host's
//! existing transport cap and are never diagnostic output.
pub use super::super::release::UpgradeRequired;
use super::*;
pub use crate::pro::protocol::Baton as OwnershipObservation;

pub struct PolicyReply {
    pub status: u16,
    pub body: Vec<u8>,
}
impl PolicyReply {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        ensure!(
            (200..300).contains(&self.status),
            "service request returned HTTP {}",
            self.status
        );
        serde_json::from_slice(&self.body).context("invalid service response")
    }
    fn from_response(response: transport::Response) -> Self {
        Self {
            status: response.status,
            body: response.body,
        }
    }
    pub fn account_unavailable(&self) -> bool {
        self.status == 503
            && serde_json::from_slice::<serde_json::Value>(&self.body)
                .is_ok_and(|value| value["error"] == "account_unavailable")
    }
}
/// The original publication lease, not a newly selected generation or epoch.
pub struct ReleaseOwner {
    project: ProjectOwner,
    config: Configure,
    epoch: u64,
    sleep: Option<Sleep>,
    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    fixture_current: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl ReleaseOwner {
    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    pub(super) fn fixture(
        project: ProjectOwner,
        epoch: u64,
        current: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            config: project.config.clone(),
            project,
            epoch,
            sleep: None,
            fixture_current: Some(current),
        }
    }
    pub fn workspace(&self) -> &str {
        self.project.id()
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn holder(&self) -> &str {
        &self.config.delegation.device_id
    }
    pub fn recovery(&self) -> bool {
        self.config.recovery
    }
    pub fn negotiated(&self) -> bool {
        self.config.execution.is_some()
    }
    pub fn current(&self) -> bool {
        #[cfg(all(unix, feature = "daemon-extension-fixture"))]
        if let Some(current) = &self.fixture_current {
            return self.project.generation_current() && current.load(Ordering::Acquire);
        }
        self.project.generation_current()
            && !self
                .sleep
                .is_some_and(|sleep| sleep.woke(&self.project.state))
            && matches!(self.project.ownership(),Some(Ownership::Transferring { epoch }) if epoch==self.epoch)
    }
    pub async fn request(&self) -> Result<PolicyReply> {
        let mut body = execution::body(&self.config, self.epoch, false);
        let path = if self.config.recovery {
            body["workspace_id"] = self.workspace().into();
            body.as_object_mut().unwrap().remove("execution_capability");
            "/v2/recovery/release".into()
        } else {
            execution::path(&self.config, self.workspace(), "release")
        };
        Ok(PolicyReply::from_response(
            super::super::account(&self.config, &path, "POST", Some(&body)).await?,
        ))
    }
}
impl SnapshotOwner {
    pub fn release_owner(&self) -> ReleaseOwner {
        ReleaseOwner {
            project: self.project.clone(),
            config: self.effective.clone(),
            epoch: self.epoch,
            sleep: self.sleep,
            #[cfg(all(unix, feature = "daemon-extension-fixture"))]
            fixture_current: None,
        }
    }
}
/// One exact worker/holder selected by the original lazy-return attempt.
pub struct HandbackOwner {
    project: ProjectOwner,
    host: crate::pro::protocol::Host,
    holder: String,
    initial_epoch: u64,
}
impl HandbackOwner {
    pub(in crate::pro::engine) fn capture(
        state: Arc<AppState>,
        config: Configure,
        workspace: String,
        host: crate::pro::protocol::Host,
        holder: String,
        initial_epoch: u64,
    ) -> Result<Self> {
        ensure!(
            host.worker_holder() == Some(holder.as_str()),
            "Project connection changed"
        );
        let generation = state.pro.generation.load(Ordering::Acquire);
        Ok(Self {
            project: ProjectOwner {
                state,
                config,
                workspace,
                generation,
            },
            host,
            holder,
            initial_epoch,
        })
    }
    pub fn workspace(&self) -> &str {
        self.project.id()
    }
    pub fn holder(&self) -> &str {
        &self.holder
    }
    pub fn initial_epoch(&self) -> u64 {
        self.initial_epoch
    }
    pub fn current(&self) -> bool {
        self.project.generation_current()
            && self.project.account_matches()
            && !lock(&self.project.state.pro.preferences)
                .get(self.workspace())
                .is_some_and(|p| p.never_mirror)
    }
    pub async fn read(&self) -> Result<OwnershipObservation> {
        let config =
            execution::effective(&self.project.state, &self.project.config, self.workspace())?;
        super::super::account(
            &config,
            &execution::path(&config, self.workspace(), ""),
            "GET",
            None,
        )
        .await
        .context("Could not check where your work is running")?
        .json()
        .context("Could not confirm where your work is running")
    }
    pub fn observe(&self, observation: &OwnershipObservation) -> Result<()> {
        // Same original execution validator; private policy performs the
        // original workspace/epoch comparison after this observation cutpoint.
        execution::observe(&self.project.state, &self.project.config, observation)
    }
    pub async fn persist_remote(&self, epoch: u64) -> Result<()> {
        let _configuration = self.project.state.pro.configuration.lock().await;
        ensure!(self.current(), "Account or project changed during return");
        self.project.set_ownership(Some(Ownership::Remote {
            epoch,
            holder: self.holder.clone(),
        }));
        self.project.persist().await
    }
    pub async fn request_handoff(&self, epoch: u64) -> Result<PolicyReply> {
        Ok(PolicyReply::from_response(
            transport::request_waking(
                &self.project.config.keeper_url,
                &format!("/v1/hosts/{}/http/api/v1/pro/handoff", self.host.id),
                "POST",
                &self.project.config.delegation.access_token,
                Some(&json!({"workspace_id":self.workspace(),"expected_epoch":epoch})),
            )
            .await?,
        ))
    }
}
/// The original request deadline and observed epoch survive every retry.
pub struct MoveRequestOwner {
    project: ProjectOwner,
    observed: OwnershipObservation,
    deadline: tokio::time::Instant,
    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    fixture_current: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl MoveRequestOwner {
    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    pub(super) fn fixture(
        project: ProjectOwner,
        observed: OwnershipObservation,
        deadline: tokio::time::Instant,
        current: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            project,
            observed,
            deadline,
            fixture_current: Some(current),
        }
    }
    pub(in crate::pro) fn capture(
        state: Arc<AppState>,
        config: Configure,
        workspace: String,
        observed: OwnershipObservation,
        deadline: tokio::time::Instant,
        generation: u64,
    ) -> Self {
        Self {
            project: ProjectOwner {
                state,
                config,
                workspace,
                generation,
            },
            observed,
            deadline,
            #[cfg(all(unix, feature = "daemon-extension-fixture"))]
            fixture_current: None,
        }
    }
    pub fn workspace(&self) -> &str {
        self.project.id()
    }
    pub fn holder(&self) -> &str {
        self.project.holder()
    }
    pub fn observed(&self) -> &OwnershipObservation {
        &self.observed
    }
    pub fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }
    pub fn current(&self) -> Result<()> {
        #[cfg(all(unix, feature = "daemon-extension-fixture"))]
        if let Some(current) = &self.fixture_current {
            ensure!(
                self.project.generation_current() && current.load(Ordering::Acquire),
                "account changed"
            );
            return Ok(());
        }
        ensure!(
            self.project.generation_current()
                && crate::pro::moves::can_take(
                    &self.project.state,
                    &self.project.config,
                    self.workspace()
                ),
            "Account or project changed while taking over"
        );
        Ok(())
    }
    pub async fn request(&self) -> Result<PolicyReply> {
        Ok(PolicyReply::from_response(
            super::super::account(
                &self.project.config,
                &format!("/v2/baton/{}/move", self.workspace()),
                "POST",
                Some(&json!({"holder_id":self.holder(),"epoch":self.observed.epoch})),
            )
            .await?,
        ))
    }
}
