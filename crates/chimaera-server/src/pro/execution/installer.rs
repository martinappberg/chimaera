//! Installer authority is captured before detection and host-lock waits. Its
//! actual owned process/cleanup remains counted; an HTTP observer owns no child.
use super::{mutation, setup};
use crate::AppState;
use anyhow::Result;
use std::{sync::Arc, time::Duration};

pub(crate) struct Running {
    state: Arc<AppState>,
    workspace: String,
    captured: mutation::Dispatch,
    guard: Option<setup::Guard>,
    spawned: bool,
}
impl Running {
    pub(crate) async fn begin(
        state: &Arc<AppState>,
        workspace: &str,
        captured: mutation::Dispatch,
    ) -> Result<Self> {
        captured.check(state)?;
        let guard = setup::Guard::installer(state, workspace, captured.clone()).await?;
        let running = Self {
            state: state.clone(),
            workspace: workspace.into(),
            captured,
            guard,
            spawned: false,
        };
        running.check()?;
        Ok(running)
    }
    pub(crate) fn check(&self) -> Result<()> {
        self.captured.check(&self.state)
    }
    /// Attach synchronously after spawn, before allowing any authority await.
    pub(crate) fn attach(&mut self, group: u32) {
        self.spawned = true;
        if let Some(guard) = &self.guard {
            guard.attach(group);
        }
    }
    pub(crate) async fn finish(mut self) -> Result<()> {
        let Some(guard) = self.guard.take() else {
            return Ok(());
        };
        if !self.spawned {
            guard.no_process();
        }

        settle(self.state.clone(), self.workspace.clone(), guard).await
    }
}
async fn settle(state: Arc<AppState>, workspace: String, guard: setup::Guard) -> Result<()> {
    // Do not release the admission on a timer: unknown cleanup remains counted
    // and visible to stop, with a fixed poll floor and at most 64 admissions.
    guard.kill();
    while setup::active(&state, &workspace) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        guard.kill();
    }
    guard.finish().await
}
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(guard) = self.guard.take() {
            if !self.spawned {
                guard.no_process();
            }

            let state = self.state.clone();
            let workspace = self.workspace.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = settle(state, workspace, guard).await;
                });
            }
        }
    }
}

#[cfg(test)]
#[path = "installer_tests.rs"]
mod tests;
