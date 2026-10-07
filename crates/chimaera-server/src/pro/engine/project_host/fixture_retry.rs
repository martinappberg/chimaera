//! Fixed original effects/observations for the relocated provider/worker cases.
//! This child is compiled only with the nondefault parent fixture module.
use super::*;
use crate::agents::AgentKind;

impl Scenario {
    pub async fn fixture_claude(&self, script: PathBuf) -> Result<()> {
        let anchor = self.harness.fixture_root().to_path_buf();
        tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                script == anchor.join("claude-fixture"),
                "fixture CLI outside anchor"
            );
            let metadata = std::fs::symlink_metadata(&script)?;
            ensure!(
                metadata.is_file()
                    && metadata.uid() == rustix::process::geteuid().as_raw()
                    && metadata.nlink() == 1
                    && metadata.len() <= 8192
                    && metadata.mode() & 0o777 == 0o700,
                "fixture CLI untrusted"
            );
            Ok::<_, anyhow::Error>(script)
        })
        .await?
        .map(|script| {
            lock(&self.harness.state.agent_bins).insert(
                AgentKind::Claude,
                crate::launcher::AgentDetection {
                    path: Ok(script),
                    version: Some("2.1.204".into()),
                    managed: false,
                    explicit: true,
                    mtime: None,
                },
            );
        })
    }
    pub fn defer_fixture_session(&self, value: Value) -> Result<()> {
        bounded(&value)?;
        let entry = crate::ledger::LedgerEntry::from_json(&value)
            .context("fixture deferred session unreadable")?;
        ensure!(
            entry.workspace_id == self.key && crate::pro::valid_id(&entry.id),
            "fixture deferred session changed project"
        );
        crate::ledger::defer(&self.harness.state, entry)
    }
    pub async fn finish_fixture_hydration(&self, epoch: u64, generation: u64) -> Result<()> {
        super::super::super::finish_hydration(&self.harness.state, &self.key, epoch, generation)
            .await
    }
    pub fn provider_blocks(&self) -> Vec<Value> {
        crate::pro::cloud_provider_blocks(&self.harness.state)
    }
    pub fn session_rows(&self) -> Vec<Value> {
        crate::session_view::sessions_json(&self.harness.state)
    }
    pub fn restored_provider_status(&self) -> Result<Value> {
        // AppState::new uses this exact original ProState constructor/path.
        let restored = crate::pro::ProState::new(self.harness.state.pro().root.clone());
        let statuses = lock(&restored.status);
        let status = statuses
            .get(&self.key)
            .context("fixture restored provider status missing")?;
        // WorkspaceStatus skips this field on the general status wire; the
        // original test read this exact restored typed field directly.
        let value = json!({"blocked_providers": status.blocked_providers});
        bounded(&value)?;
        Ok(value)
    }
    pub fn deferred_sessions_empty(&self) -> bool {
        lock(&self.harness.state.deferred_sessions).is_empty()
    }
    pub fn execution_managed(&self) -> bool {
        execution::managed(&self.harness.state, &self.key)
    }
    pub fn checkpoint_mode(&self) -> bool {
        execution::checkpoint_mode(&self.harness.state, &self.key)
    }
}
