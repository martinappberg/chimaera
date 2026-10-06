//! An agent-CLI install is execution too: the lease watchdog and clean
//! handoff must see its process group and background descendants, including
//! the spawn/journal crash window.
use super::*;
use std::sync::Arc;

pub(super) struct Entry {
    pub(super) group: Option<(u32, u64)>,
    held: bool,
}

pub(in crate::pro) struct Guard {
    state: Arc<AppState>,
    workspace: String,
    generation: u64,
    _commit: mutation::Guard,
    installer: mutation::Dispatch,
}

fn alive(group: (u32, u64)) -> bool {
    !restart::surviving(&[group]).is_empty()
}
pub(in crate::pro) fn signal(state: &AppState, workspace: &str) {
    if let Some(group) = lock(&state.pro.execution.setups)
        .get(workspace)
        .and_then(|entry| entry.group)
    {
        if alive(group) {
            if let Ok(group) = i32::try_from(group.0) {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(group),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
    }
}
pub(in crate::pro) fn active(state: &AppState, workspace: &str) -> bool {
    let mut setups = lock(&state.pro.execution.setups);
    match setups.get(workspace) {
        Some(entry) if entry.group.is_some_and(|group| !alive(group)) => {
            if !entry.held {
                setups.remove(workspace);
            }
            false
        }
        Some(_) => true,
        None => false,
    }
}
impl Guard {
    pub(super) async fn installer(
        state: &Arc<AppState>,
        workspace: &str,
        captured: mutation::Dispatch,
    ) -> Result<Option<Self>> {
        let _configuration = state.pro.configuration.lock().await;
        let commit = captured.begin(state)?;
        // Free local installs do not create enrollment or durable Pro state.
        let Some(commit) = commit else {
            return Ok(None);
        };
        {
            let mut setups = lock(&state.pro.execution.setups);
            ensure!(
                setups.len() < 64 && !setups.contains_key(workspace),
                "previous project setup is still stopping"
            );
            setups.insert(
                workspace.to_owned(),
                Entry {
                    group: None,
                    held: true,
                },
            );
        }
        let guard = Self {
            state: state.clone(),
            workspace: workspace.to_owned(),
            generation: mutation::generation(state),
            _commit: commit,
            installer: captured,
        };
        guard.check()?;
        {
            let mut preferences = lock(&state.pro.preferences);
            ensure!(
                preferences.len() < 128 || preferences.contains_key(workspace),
                "project setup identity limit"
            );
            let preference = preferences.entry(workspace.to_owned()).or_default();
            preference.execution_active = true;
            preference.execution_boot = state.pro.execution.boot.clone();
            preference.execution_launch_pending = true;
        }
        crate::pro::persist(state).await?;
        guard.check()?;
        Ok(Some(guard))
    }
    pub(in crate::pro) fn check(&self) -> Result<()> {
        self.installer.check(&self.state)
    }
    /// No await may separate spawning the group from registering it.
    pub(in crate::pro) fn attach(&self, group: u32) {
        let start = i32::try_from(group)
            .ok()
            .and_then(restart::leader_start)
            .unwrap_or(0);
        lock(&self.state.pro.execution.setups)
            .get_mut(&self.workspace)
            .expect("setup reservation remains held")
            .group = Some((group, start));
    }
    pub(super) fn no_process(&self) {
        let mut setups = lock(&self.state.pro.execution.setups);
        if setups
            .get(&self.workspace)
            .is_some_and(|entry| entry.group.is_none())
        {
            setups.remove(&self.workspace);
        }
    }
    pub(in crate::pro) fn kill(&self) {
        signal(&self.state, &self.workspace);
    }
    pub(in crate::pro) async fn finish(self) -> Result<()> {
        self.kill();
        tokio::time::timeout(Duration::from_secs(2), async {
            while active(&self.state, &self.workspace) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("project setup cleanup could not be verified")?;
        if self.generation == mutation::generation(&self.state)
            && super::launch::settled(&self.state, &self.workspace)
        {
            lock(&self.state.pro.preferences)
                .entry(self.workspace.clone())
                .or_default()
                .execution_launch_pending = false;
            if let Err(error) = crate::pro::persist(&self.state).await {
                lock(&self.state.pro.preferences)
                    .entry(self.workspace.clone())
                    .or_default()
                    .execution_launch_pending = true;
                return Err(error);
            }
        }
        Ok(())
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.kill();
        let mut setups = lock(&self.state.pro.execution.setups);
        if let Some(entry) = setups.get_mut(&self.workspace) {
            entry.held = false;
        }
        if setups
            .get(&self.workspace)
            .is_some_and(|entry| entry.group.is_none_or(|group| !alive(group)))
        {
            setups.remove(&self.workspace);
        }
        // The pending marker remains after cancellation. A successor cannot
        // interpret partially journaled group evidence as a complete receipt.
    }
}
