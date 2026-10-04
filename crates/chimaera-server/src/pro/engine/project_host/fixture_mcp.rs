//! Fixed original MCP session/TUI effects for the private real-HTTP case.
//! No fixture function exports the daemon store or a configurable process.
use super::*;
use crate::agents::{AgentKind, AgentRecord};

fn mcp_key(id: &str) -> Result<&'static str> {
    match id {
        "s-cloud" => Ok("cloud-fixture"),
        "s-other" => Ok("other-fixture"),
        _ => anyhow::bail!("unknown fixture MCP identity"),
    }
}
impl Harness {
    pub fn session_counts(&self) -> (usize, usize) {
        (
            self.state.sessions.list().len(),
            self.state.chat.list().len(),
        )
    }
}
impl Scenario {
    pub fn seed_mcp_session(&self, id: &str, key: &str) -> Result<()> {
        ensure!(key == mcp_key(id)?, "fixture MCP key changed");
        let state = &self.harness.state;
        ensure!(
            lock(&state.workspaces).get(&self.key).is_some(),
            "fixture project missing"
        );
        let mut agents = lock(&state.agents);
        let mut membership = lock(&state.session_workspaces);
        ensure!(
            !agents.contains_key(id) && !membership.contains_key(id),
            "fixture MCP identity already present"
        );
        agents.insert(id.into(), AgentRecord::new(key.into(), AgentKind::Claude));
        membership.insert(id.into(), self.key.clone());
        Ok(())
    }
    pub fn remove_mcp_session(&self, id: &str) -> Result<()> {
        let key = mcp_key(id)?;
        let state = &self.harness.state;
        let mut agents = lock(&state.agents);
        let mut membership = lock(&state.session_workspaces);
        ensure!(
            agents
                .get(id)
                .is_some_and(|record| record.key == key && record.kind == AgentKind::Claude)
                && membership.get(id) == Some(&self.key),
            "fixture MCP original record changed"
        );
        ensure!(
            state.sessions.get(id).is_none() && state.chat.get(id).is_none(),
            "fixture MCP identity acquired a process"
        );
        agents.remove(id);
        membership.remove(id);
        Ok(())
    }
    pub fn mark_returned(&self) -> Result<()> {
        ensure!(
            lock(&self.harness.state.workspaces)
                .get(&self.key)
                .is_some(),
            "fixture project missing"
        );
        lock(&self.harness.state.pro.returned).insert(self.key.clone());
        Ok(())
    }
    pub fn cloud_profile(&self) -> Option<crate::pro::policy::CloudProfile> {
        crate::pro::workspace_profile(&self.harness.state, &self.key)
    }
    pub async fn spawn_fixture_codex(&self, script: PathBuf) -> Result<String> {
        let workspace = lock(&self.harness.state.workspaces)
            .get(&self.key)
            .context("fixture project missing")?;
        ensure!(
            script == workspace.root.join("fixture-codex"),
            "fixture CLI changed"
        );
        let script = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::MetadataExt;
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
        .await??;
        lock(&self.harness.state.agent_bins).insert(
            AgentKind::Codex,
            crate::launcher::AgentDetection {
                path: Ok(script),
                version: Some(chimaera_agent::codex::TESTED_CODEX_VERSION.into()),
                managed: false,
                explicit: false,
                mtime: None,
            },
        );
        let row = crate::spawn::spawn_session(
            &self.harness.state,
            crate::spawn::SpawnSpec {
                workspace,
                started_by: crate::history::StartedBy::You,
                id: None,
                name: None,
                cwd: None,
                native_cwd: None,
                cols: None,
                rows: None,
                theme: "dark".into(),
                title_hint: None,
                prelude: None,
                fork_head: false,
                kind: crate::spawn::SpawnKind::Agent {
                    kind: AgentKind::Codex,
                    model: None,
                    resume: None,
                },
            },
        )
        .await
        .map_err(|_| anyhow::anyhow!("fixture TUI failed to spawn"))?;
        Ok(row["id"]
            .as_str()
            .context("fixture TUI identity missing")?
            .to_owned())
    }
}
