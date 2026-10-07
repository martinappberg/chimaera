//! A bounded durable intent covers the interval before a driver's process
//! group can be recorded. A cancelled or ambiguous intent never proves stop.
use super::*;
use std::sync::Arc;

pub(crate) struct Intent {
    state: Arc<AppState>,
    workspace: String,
    generation: u64,
    revision: u64,
    ownership: Option<Ownership>,
    settled: bool,
}
pub(super) fn settled(state: &AppState, workspace: &str) -> bool {
    lock(&state.pro().execution.launches)
        .get(workspace)
        .is_none_or(|(count, abandoned, _)| *count == 0 && !abandoned)
}
pub(super) fn stopped(state: &AppState, workspace: &str) {
    let mut launches = lock(&state.pro().execution.launches);
    // No old intent can exist without an entry. Stop must not enroll identities
    // for workspaces that have never used this bounded registry.
    let exhausted = launches.get_mut(workspace).is_some_and(|entry| {
        // An exhausted identity stays closed instead of reusing an older
        // intent's identity.
        *entry = match entry.2.checked_add(1) {
            Some(revision) => (0, false, revision),
            None => (64, true, u64::MAX),
        };
        entry.1
    });
    drop(launches);
    if let Some(preference) = lock(&state.pro().preferences).get_mut(workspace) {
        preference.execution_launch_pending = exhausted;
    }
}
impl Intent {
    pub(super) fn begin(state: &Arc<AppState>, workspace: &str) -> Result<Self> {
        let ownership = lock(&state.pro().ownership).get(workspace).cloned();
        let mut launches = lock(&state.pro().execution.launches);
        ensure!(
            launches.len() < 128 || launches.contains_key(workspace),
            "managed launch identity limit"
        );
        ensure!(
            launches.values().map(|(count, _, _)| *count).sum::<usize>() < 64,
            "managed launch intent limit"
        );
        let entry = launches.entry(workspace.to_owned()).or_default();
        entry.0 += 1;
        let revision = entry.2;
        Ok(Self {
            state: state.clone(),
            workspace: workspace.to_owned(),
            generation: mutation::generation(state),
            revision,
            ownership,
            settled: false,
        })
    }
    pub(crate) fn check(&self) -> Result<()> {
        let ownership = lock(&self.state.pro().ownership)
            .get(&self.workspace)
            .cloned();
        let current_revision = lock(&self.state.pro().execution.launches)
            .get(&self.workspace)
            .map(|entry| entry.2);
        ensure!(
            self.generation == mutation::generation(&self.state)
                && ownership == self.ownership
                && current_revision == Some(self.revision),
            "managed launch authority changed"
        );
        Ok(())
    }
    pub(crate) fn registered(mut self, id: String) {
        tokio::spawn(async move {
            // Chat registration can precede its asynchronous driver spawn.
            // Keep its durable intent until a group or terminal death is known.
            for _ in 0..100 {
                if self.check().is_err() {
                    return;
                }
                let group = self.state.chat.process_group(&id).or_else(|| {
                    self.state
                        .sessions
                        .get(&id)
                        .filter(|session| session.alive)
                        .and_then(|session| session.pid)
                });
                let dead = self
                    .state
                    .chat
                    .get(&id)
                    .is_some_and(|session| !session.alive)
                    || self
                        .state
                        .sessions
                        .get(&id)
                        .is_some_and(|session| !session.alive);
                if group.is_some() || dead {
                    let _configuration = self.state.pro().configuration.clone().lock_owned().await;
                    if self.check().is_err() {
                        return;
                    }
                    // Stop must drain this evidence write too. Acquire only
                    // after configuration, because account replacement can
                    // hold that lock while waiting for counted admissions.
                    let Ok(_commit) = mutation::begin_launch(&self.state, &self.workspace) else {
                        return;
                    };
                    if self.check().is_err() {
                        return;
                    }
                    {
                        let mut launches = lock(&self.state.pro().execution.launches);
                        if let Some((count, _, revision)) = launches.get_mut(&self.workspace) {
                            if *revision != self.revision {
                                return;
                            }
                            *count = count.saturating_sub(1);
                        }
                        self.settled = true;
                    }
                    if settled(&self.state, &self.workspace)
                        && !setup::active(&self.state, &self.workspace)
                    {
                        lock(&self.state.pro().preferences)
                            .entry(self.workspace.clone())
                            .or_default()
                            .execution_launch_pending = false;
                    }
                    if crate::pro::persist(&self.state).await.is_err() {
                        lock(&self.state.pro().preferences)
                            .entry(self.workspace.clone())
                            .or_default()
                            .execution_launch_pending = true;
                        lock(&self.state.pro().execution.launches)
                            .entry(self.workspace.clone())
                            .or_default()
                            .1 = true;
                    }
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
    }
}
impl Drop for Intent {
    fn drop(&mut self) {
        if !self.settled {
            let mut launches = lock(&self.state.pro().execution.launches);
            let Some((count, abandoned, revision)) = launches.get_mut(&self.workspace) else {
                return;
            };
            if *revision != self.revision {
                return;
            }
            *count = count.saturating_sub(1);
            *abandoned = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn abandoned_intent_cannot_be_cleared_by_a_later_registered_group() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-launch-intent-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        install_fixture(&state, "w-project", 4).unwrap();
        let cancelled = prepare_launch(&state, "w-project").await.unwrap().unwrap();
        // Older evidence can contain groups that are all already gone. It
        // cannot establish completeness for this new unregistered launch.
        lock(&state.pro().preferences)
            .get_mut("w-project")
            .unwrap()
            .execution_groups = vec![u32::MAX];
        crate::pro::persist(&state).await.unwrap();
        for worker in [false, true] {
            let restored = State::restore(
                &state.pro().root,
                &lock(&state.pro().preferences),
                worker,
                false,
            );
            assert!(lock(&restored.unclean)["w-project"].is_empty());
        }
        drop(cancelled);
        let later = prepare_launch(&state, "w-project").await.unwrap().unwrap();
        let session = state
            .sessions
            .spawn_managed(chimaera_pty::SpawnOpts {
                cwd: root.clone(),
                name: None,
                cols: 80,
                rows: 24,
                command: Some(vec!["/bin/sleep".into(), "30".into()]),
                id: None,
                env: vec![],
                env_remove: vec![],
                scrollback: None,
            })
            .unwrap();
        lock(&state.agents).insert(
            session.id.clone(),
            crate::agent_state::AgentRecord::new(
                "fixture".into(),
                crate::agent_state::AgentKind::Claude,
            ),
        );
        lock(&state.session_workspaces).insert(session.id.clone(), "w-project".into());
        later.registered(session.id.clone());
        tokio::time::timeout(Duration::from_secs(3), async {
            while lock(&state.pro().execution.launches)["w-project"].0 != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(lock(&state.pro().preferences)["w-project"].execution_launch_pending);
        state.sessions.fence(&session.id).unwrap();
        stop(&state, &["w-project".into()]).await.unwrap();
        assert!(!lock(&state.pro().preferences)["w-project"].execution_launch_pending);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn stopped_intents_cannot_spawn_or_settle_a_new_launch_revision() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-launch-revision-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        install_fixture(&state, "w-project", 4).unwrap();
        let old = prepare_launch(&state, "w-project").await.unwrap().unwrap();
        stop(&state, &["w-project".into()]).await.unwrap();
        // Even reacquiring the same epoch cannot revive a pre-stop admission.
        renewed_fixture(&state, "w-project", 4).unwrap();
        assert!(old.check().is_err());
        let current = prepare_launch(&state, "w-project").await.unwrap().unwrap();
        let session = state
            .sessions
            .spawn_managed(chimaera_pty::SpawnOpts {
                cwd: root.clone(),
                name: None,
                cols: 80,
                rows: 24,
                command: Some(vec!["/bin/sleep".into(), "30".into()]),
                id: None,
                env: vec![],
                env_remove: vec![],
                scrollback: None,
            })
            .unwrap();
        // A late old receipt must neither decrement the new reservation nor
        // poison it as abandoned when its task drops the stale intent.
        old.registered(session.id.clone());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(lock(&state.pro().execution.launches)["w-project"].0, 1);
        assert!(!lock(&state.pro().execution.launches)["w-project"].1);
        assert!(lock(&state.pro().preferences)["w-project"].execution_launch_pending);
        // Epoch replacement also invalidates an intent without a stop/reset.
        lock(&state.pro().ownership).insert("w-project".into(), Ownership::Local { epoch: 5 });
        assert!(mutation::begin_launch(&state, "w-project").is_ok());
        assert!(current.check().is_err());
        drop(current);
        state.sessions.fence(&session.id).unwrap();
        stop(&state, &["w-project".into()]).await.unwrap();
        let device = prepare_launch(&state, "w-project").await.unwrap().unwrap();
        // A computer's lapsed lease fences it like a cloud machine: no new
        // launch starts until it holds the project again.
        expired_lease_fixture(&state, "w-project");
        assert!(mutation::begin_launch(&state, "w-project").is_err());
        drop(device);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn pending_intents_are_bounded_and_free_work_has_no_intent() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-launch-cap-{}",
            chimaera_core::generate_token()
        ));
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        assert!(prepare_launch(&state, "w-free").await.unwrap().is_none());
        let intents: Vec<_> = (0..64)
            .map(|_| Intent::begin(&state, "w-project").unwrap())
            .collect();
        assert!(Intent::begin(&state, "w-project").is_err());
        drop(intents);
        assert!(!settled(&state, "w-project"));
    }
}
