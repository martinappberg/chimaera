//! Finite coordinator effects over the same public authority and task owners.
use super::*;

#[cfg(all(unix, feature = "daemon-extension-fixture"))]
pub mod fixture;

/// Captured only by public engine::start from its original state/generation.
pub struct CoordinatorOwner {
    state: Arc<AppState>,
    generation: u64,
}
/// One original configuration capture, not a successor credential or authority.
pub struct CoordinatorTick {
    state: Arc<AppState>,
    config: Configure,
    generation: u64,
}
/// Descriptive roster evidence; construction is restricted to the public host.
pub struct CoordinatorProject {
    id: String,
    state: Arc<AppState>,
    generation: u64,
}
/// Original roster iteration stays lazy across reconciliation awaits, so a
/// preference/root change is read at the same project cutpoint as before.
pub struct CoordinatorProjects {
    state: Arc<AppState>,
    generation: u64,
    workspaces: std::vec::IntoIter<crate::workspaces::Workspace>,
    remaining: usize,
}
impl Iterator for CoordinatorProjects {
    type Item = CoordinatorProject;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        for workspace in self.workspaces.by_ref() {
            if eligible(&self.state, &workspace) {
                self.remaining -= 1;
                return Some(CoordinatorProject {
                    id: workspace.id,
                    state: self.state.clone(),
                    generation: self.generation,
                });
            }
        }
        None
    }
}
pub struct Reconciled {
    pub unauthorized: bool,
    pub working: bool,
}
impl CoordinatorProject {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn never_mirror(&self) -> bool {
        lock(&self.state.pro.preferences)
            .get(&self.id)
            .is_some_and(|p| p.never_mirror)
    }
    pub fn awaiting_verification(&self) -> bool {
        matches!(
            lock(&self.state.pro.ownership).get(&self.id),
            Some(Ownership::AwaitingVerification { .. })
        )
    }
}
impl CoordinatorOwner {
    pub(super) fn capture(state: Arc<AppState>) -> Self {
        let generation = state.pro.generation.load(Ordering::Acquire);
        Self { state, generation }
    }
    pub fn current(&self) -> bool {
        !self.state.stopping.load(Ordering::Relaxed)
            && self.generation == self.state.pro.generation.load(Ordering::Acquire)
    }
    pub fn now(&self) -> u64 {
        super::super::now()
    }
    pub fn tick(&self) -> Option<CoordinatorTick> {
        if !self.current() {
            return None;
        }
        let config = lock(&self.state.pro.runtime).clone()?;
        Some(CoordinatorTick {
            state: self.state.clone(),
            config,
            generation: self.generation,
        })
    }
    pub async fn wait_tick(&self) {
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(5)) => {}
            () = self.state.pro.renew_now.notified() => {}
        }
    }
}
impl CoordinatorTick {
    pub fn generation_current(&self) -> bool {
        !self.state.stopping.load(Ordering::Relaxed)
            && self.generation == self.state.pro.generation.load(Ordering::Acquire)
    }
    pub async fn renew_delegation(&self) -> bool {
        if !self.generation_current() {
            return false;
        }
        super::renew_delegation(&self.state, &self.config, self.generation).await
    }
    pub fn reprobe(&self) {
        execution::reprobe(&self.state);
    }
    pub fn projects(&self) -> CoordinatorProjects {
        let workspaces = lock(&self.state.workspaces).list();
        CoordinatorProjects {
            state: self.state.clone(),
            generation: self.generation,
            workspaces: workspaces.into_iter(),
            remaining: 128,
        }
    }
    pub async fn reconcile(&self, project: &CoordinatorProject) -> Reconciled {
        let mut unauthorized = false;
        if self.generation_current() && project.generation == self.generation {
            if let Err(error) =
                reconcile_generation(&self.state, &self.config, &project.id, self.generation).await
            {
                unauthorized = error
                    .chain()
                    .any(|cause| cause.to_string() == transport::UNAUTHORIZED);
                record_error(&self.state, &project.id, &error);
            }
        }
        Reconciled {
            unauthorized,
            working: super::super::owned_epoch(&self.state, &project.id).is_some()
                && !working_agents(&self.state, &project.id).is_empty(),
        }
    }
    pub fn can_mirror(&self) -> bool {
        self.generation_current()
            && !super::super::drain::draining(&self.state)
            && lock(&self.state.pro.mirror_task)
                .as_ref()
                .is_none_or(|task| task.is_finished())
    }
    pub fn start_mirror(&self, only: Option<BTreeSet<String>>) -> bool {
        if !self.can_mirror()
            || only.as_ref().is_some_and(|ids| {
                ids.len() > 128 || ids.iter().any(|id| !super::super::valid_id(id))
            })
        {
            return false;
        }
        let owner = self.state.clone();
        let config = self.config.clone();
        let generation = self.generation;
        let task = tokio::spawn(async move {
            let _guard = owner.pro.jobs.lock().await;
            if only.is_none() {
                if let Err(error) = lazy_handback(&owner, &config).await {
                    tracing::warn!(phase="locate_return", error=%error, "Could not locate returning projects");
                }
            }
            let workspaces = lock(&owner.workspaces).list();
            for workspace in workspaces
                .into_iter()
                .filter(|workspace| eligible(&owner, workspace))
                .filter(|workspace| only.as_ref().is_none_or(|ids| ids.contains(&workspace.id)))
                .take(128)
            {
                if generation != owner.pro.generation.load(Ordering::Acquire) {
                    return;
                }
                if super::super::owned_epoch(&owner, &workspace.id).is_none()
                    || !execution::lease_valid(&owner, &workspace.id)
                    || lock(&owner.pro.preferences)
                        .get(&workspace.id)
                        .is_some_and(|p| p.never_mirror)
                {
                    continue;
                }
                if let Err(error) = snapshot(&owner, &config, &workspace.id, false).await {
                    record_error(&owner, &workspace.id, &error);
                }
            }
        });
        *lock(&self.state.pro.mirror_task) = Some(task);
        true
    }
    /// A pass that only brings work home (`lazy_handback`), between the copy
    /// passes (which run it every two minutes): while the app is here and
    /// settled and the cloud holds a project, so its work comes home at the
    /// conversation's next pause, or while a project the cloud did not keep
    /// is being taken back. At most every ten seconds; never beside another
    /// pass.
    pub fn start_return(&self) -> bool {
        const EVERY: u64 = 10;
        if !self.can_mirror() || self.config.role != Role::Device {
            return false;
        }
        let wanted = !lock(&self.state.pro.reclaim).is_empty()
            || (super::super::leave::app_settled(&self.state)
                && lock(&self.state.pro.ownership).values().any(|owner| {
                    matches!(
                        owner,
                        Ownership::Remote { .. } | Ownership::Hydrating { .. }
                    )
                }));
        let now = super::super::now();
        if !wanted || now.saturating_sub(self.state.pro.return_pass.load(Ordering::Acquire)) < EVERY
        {
            return false;
        }
        self.state.pro.return_pass.store(now, Ordering::Release);
        let owner = self.state.clone();
        let config = self.config.clone();
        let task = tokio::spawn(async move {
            let _guard = owner.pro.jobs.lock().await;
            if let Err(error) = lazy_handback(&owner, &config).await {
                tracing::warn!(phase="locate_return", error=%error, "Could not locate returning projects");
            }
        });
        *lock(&self.state.pro.mirror_task) = Some(task);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, Json};

    #[tokio::test]
    async fn absent_runtime_preserves_enrollment_remote_fence_and_free_project() {
        let root = continuity_tests::temp("absent-policy");
        let state = continuity_tests::state(&root);
        let account = continuity_tests::FakeAccount::start(serde_json::json!({})).await;
        let managed_root = root.join("managed");
        let free_root = root.join("free");
        std::fs::create_dir_all(&managed_root).unwrap();
        std::fs::create_dir_all(&free_root).unwrap();
        let managed = lock(&state.workspaces).add(managed_root).unwrap();
        let free = lock(&state.workspaces).add(free_root).unwrap();
        lock(&state.pro.ownership).insert(
            managed.id.clone(),
            Ownership::Remote {
                epoch: 7,
                holder: "d-other".into(),
            },
        );
        let config = serde_json::from_value(serde_json::json!({
            "account_id":"a-fixture", "endpoint":account.endpoint,
            "keeper_url":"", "role":"device",
            "delegation":{"access_token":"synthetic","device_id":"d-home",
                "expires_at":"2099-01-01T00:00:00Z","scope":["baton","mirror"]}
        }))
        .unwrap();
        let configured =
            super::super::super::routes::configure(State(state.clone()), Json(config)).await;
        assert!(configured.status().is_success());
        let status = super::super::super::routes::status(State(state.clone()))
            .await
            .0;
        assert_eq!(
            status["configured"], true,
            "configuration is still enrolled"
        );
        let rows = status["workspaces"].as_array().unwrap();
        let row = |id: &str| rows.iter().find(|row| row["workspace_id"] == id).unwrap();
        assert_eq!(
            row(&managed.id)["mirror"]["error_code"],
            "optional_runtime_unavailable"
        );
        assert_eq!(row(&managed.id)["ownership"]["epoch"], 7);
        assert!(row(&free.id)["mirror"].is_null());
        assert!(!super::super::super::may_write(&state, &managed.id));
        assert!(super::super::super::may_write(&state, &free.id));
        assert!(super::super::super::may_execute(&state, &free.id));
        assert!(lock(&state.pro.task).is_none());
        assert!(
            lock(&account.requests).is_empty(),
            "None does not start account policy"
        );
        drop(account);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
