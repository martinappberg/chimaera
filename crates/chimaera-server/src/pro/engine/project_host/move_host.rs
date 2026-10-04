//! Fixed move effects on the original admitted project and yielding tuple.
//! Task/store admission stays in the host; optional policy cannot select a
//! successor configuration, workspace, request path or yielding epoch.
use super::*;

impl ProjectOwner {
    pub(in crate::pro) fn move_capture(
        state: Arc<AppState>,
        config: Configure,
        workspace: String,
        generation: u64,
    ) -> Self {
        Self {
            state,
            config,
            workspace,
            generation,
        }
    }
    pub fn move_current(&self) -> Result<()> {
        ensure!(
            self.generation_current()
                && crate::pro::moves::can_take(&self.state, &self.config, self.id()),
            "Account or project changed while taking over"
        );
        Ok(())
    }
    pub fn move_owned_epoch(&self) -> Option<u64> {
        crate::pro::owned_epoch(&self.state, self.id())
    }
    pub async fn move_read(&self) -> Result<ProjectBaton> {
        let baton: Baton = super::account(
            &self.config,
            &execution::path(&self.config, self.id(), ""),
            "GET",
            None,
        )
        .await?
        .json()
        .context("Could not confirm where your work is running")?;
        ensure!(baton.workspace_id == self.id(), "baton workspace mismatch");
        execution::observe(&self.state, &self.config, &baton)?;
        Ok(ProjectBaton::capture(baton))
    }
    pub fn move_request(
        &self,
        observed: ProjectBaton,
        deadline: tokio::time::Instant,
    ) -> policy_host::MoveRequestOwner {
        policy_host::MoveRequestOwner::capture(
            self.state.clone(),
            self.config.clone(),
            self.workspace.clone(),
            observed.original,
            deadline,
            self.generation,
        )
    }
}

impl ProjectBaton {
    pub fn move_observation(&self) -> &Baton {
        &self.original
    }
}

/// One already-admitted handover task. The original tuple remains in the
/// single host store until this task's final matching retirement.
#[derive(Clone)]
pub struct MoveYieldOwner {
    project: ProjectOwner,
    epoch: u64,
    requested: u64,
}
impl MoveYieldOwner {
    pub(in crate::pro) fn capture(
        state: Arc<AppState>,
        config: Configure,
        workspace: String,
        epoch: u64,
        requested: u64,
    ) -> Self {
        let generation = state.pro.generation.load(Ordering::Acquire);
        Self {
            project: ProjectOwner::move_capture(state, config, workspace, generation),
            epoch,
            requested,
        }
    }
    pub fn requested(&self) -> u64 {
        self.requested
    }
    pub fn current(&self) -> bool {
        let current = lock(&self.project.state.pro.moves.yielding)
            .get(self.project.id())
            .is_some_and(|(held, at, _)| *held == self.epoch && *at == self.requested);
        current && self.project.move_owned_epoch() == Some(self.epoch)
    }
    pub fn acted(&self) -> Option<u64> {
        lock(&self.project.state.pro.moves.acted)
            .get(self.project.id())
            .copied()
    }
    pub fn at_pause(&self) -> bool {
        super::at_pause(&self.project.state, self.project.id())
    }
    pub async fn claim(&self) -> Result<()> {
        crate::pro::moves::claim(&self.project.config, self.project.id(), self.epoch).await
    }
    pub async fn hand_off(&self) -> bool {
        lock(&self.project.state.pro.moves.leaving).insert(self.project.workspace.clone());
        let handed = crate::pro::routes::hand_to_computer(
            &self.project.state,
            &self.project.config,
            self.project.id(),
            self.epoch,
        )
        .await;
        if !handed {
            lock(&self.project.state.pro.moves.leaving).remove(self.project.id());
        }
        handed
    }
    pub(in crate::pro) fn finish(&self) {
        let mut yielding = lock(&self.project.state.pro.moves.yielding);
        if yielding
            .get(self.project.id())
            .is_some_and(|(held, at, _)| *held == self.epoch && *at == self.requested)
        {
            yielding.remove(self.project.id());
        }
    }
}
