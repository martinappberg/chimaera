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

/// Where-you-run proofs: real agent processes (a recording stand-in for the
/// vendor CLI) started through the ordinary spawn, resume and hook paths.
impl Scenario {
    async fn fixture_agent(&self, kind: AgentKind) -> Result<()> {
        let workspace = lock(&self.harness.state.workspaces)
            .get(&self.key)
            .context("fixture project missing")?;
        let script = workspace.root.join(format!("fixture-{}", kind.as_str()));
        let script = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(&script)?;
            ensure!(
                metadata.is_file()
                    && metadata.uid() == rustix::process::geteuid().as_raw()
                    && metadata.nlink() == 1
                    && metadata.len() <= 64 * 1024
                    && metadata.mode() & 0o777 == 0o700,
                "fixture CLI untrusted"
            );
            Ok::<_, anyhow::Error>(script)
        })
        .await??;
        lock(&self.harness.state.agent_bins).insert(
            kind,
            crate::launcher::AgentDetection {
                path: Ok(script),
                version: Some(match kind {
                    AgentKind::Codex => chimaera_agent::codex::TESTED_CODEX_VERSION.into(),
                    _ => "2.1.283".into(),
                }),
                managed: false,
                explicit: true,
                mtime: None,
            },
        );
        Ok(())
    }
    fn agent_kind(kind: &str) -> Result<AgentKind> {
        match kind {
            "claude" => Ok(AgentKind::Claude),
            "codex" => Ok(AgentKind::Codex),
            _ => anyhow::bail!("unknown fixture agent"),
        }
    }
    /// A new conversation here, as the user starts one (`POST /sessions`).
    pub async fn start_fixture_chat(&self, kind: &str) -> Result<String> {
        self.fixture_agent(Self::agent_kind(kind)?).await?;
        let response = self
            .harness
            .fixed_request(
                "/api/v1/sessions",
                json!({"workspace_id":self.key,"kind":"agent","agent":kind,"ui":"chat"}),
            )
            .await?;
        ensure!(
            response.status == axum::http::StatusCode::OK,
            "fixture chat creation refused: {}",
            response.body
        );
        Ok(response.body["id"]
            .as_str()
            .context("fixture chat identity missing")?
            .to_owned())
    }
    /// A terminal agent here (the PTY/TUI spawn).
    pub async fn start_fixture_terminal(&self, kind: &str) -> Result<String> {
        let kind = Self::agent_kind(kind)?;
        self.fixture_agent(kind).await?;
        let workspace = lock(&self.harness.state.workspaces)
            .get(&self.key)
            .context("fixture project missing")?;
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
                    kind,
                    model: None,
                    resume: None,
                },
            },
        )
        .await
        .map_err(|_| anyhow::anyhow!("fixture terminal agent failed to spawn"))?;
        Ok(row["id"]
            .as_str()
            .context("fixture terminal identity missing")?
            .to_owned())
    }
    /// A Claude conversation arriving here as a move installs it: its native
    /// transcript and a deferred entry carrying the move's origin
    /// (`moved` | `home`) and, mid-turn, the cut-off turn; then the ordinary
    /// deferred resume starts it.
    pub async fn arrive_fixture_chat(
        &self,
        session: &str,
        native: &str,
        transcript: String,
        origin: &str,
        mid_turn: bool,
    ) -> Result<()> {
        ensure!(
            crate::pro::valid_id(session)
                && native.len() == 36
                && native.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
                && matches!(origin, "moved" | "home")
                && transcript.len() <= 64 * 1024,
            "invalid fixture arrival"
        );
        self.fixture_agent(AgentKind::Claude).await?;
        let state = &self.harness.state;
        let workspace = lock(&state.workspaces)
            .get(&self.key)
            .context("fixture project missing")?;
        let path = state
            .claude_projects_dir
            .join(crate::launcher::encode_cwd(&workspace.root))
            .join(format!("{native}.jsonl"));
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(path.parent().context("fixture native parent")?)?;
            std::fs::write(path, transcript)?;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        let carryover = mid_turn.then(|| json!({"turn_in_flight": true}));
        let entry = crate::ledger::LedgerEntry::from_json(&json!({
            "id": session, "suspended": true, "handoff": {"fork": false, "origin": origin, "epoch": 1},
            "workspace_id": self.key, "cwd": workspace.root, "cols": 80, "rows": 24, "theme": "dark",
            "agent": {"kind": "claude", "resume": native, "title": "fixture conversation", "ui": "chat", "carryover": carryover},
        }))
        .context("invalid fixture ledger entry")?;
        lock(&state.deferred_sessions).insert(session.to_owned(), entry);
        crate::ledger::resume_deferred_workspace(state, &self.key).await
    }
    /// Enrolls the project as its first copy or a hydrate would: only an
    /// enrolled project's agents hear where they run (`pro::synced`).
    pub fn enroll_fixture(&self) {
        lock(&self.harness.state.pro.ownership)
            .entry(self.key.clone())
            .or_insert(crate::pro::Ownership::Local { epoch: 1 });
    }
    /// What a move into this machine records before its agents resume.
    pub async fn record_fixture_arrival(&self, left_out: &[&str], os: &str, arch: &str) {
        let left_out: Vec<PathBuf> = left_out.iter().take(64).map(PathBuf::from).collect();
        crate::mcp::cloud_context::record_arrival(
            &self.harness.state,
            &self.key,
            Some(&left_out),
            [Some(os), Some(arch)],
        )
        .await;
    }
    /// What a return that kept both versions of some files reports.
    pub fn report_fixture_kept(&self, copies: &[&str]) {
        let copies: Vec<PathBuf> = copies.iter().take(32).map(PathBuf::from).collect();
        crate::pro::report_return(&self.harness.state, &self.key, (copies.len(), copies), &[]);
    }
    /// Seeds what the last move's configuration export left out.
    pub fn seed_fixture_environment(&self, names: &[&str]) -> Result<()> {
        let mut preferences = lock(&self.harness.state.pro.preferences);
        let names: Vec<String> = names.iter().take(32).map(|n| n.to_string()).collect();
        crate::pro::policy::validate_missing_environment(&names)?;
        preferences
            .entry(self.key.clone())
            .or_default()
            .missing_environment = names;
        Ok(())
    }
}
