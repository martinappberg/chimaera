//! In-process workspace maintenance for a trusted optional daemon assembly.
//! The caller owns its retained apply task; HTTP observer loss never drops this
//! capability between durable worker admission and commit.
use crate::{chat::ChatSwitchGuard, ledger, lock, pro::mutation, AppState};
use anyhow::{ensure, Context, Result};
use chimaera_agent::{maintenance_idle::MaintenanceIdle, PausedCommands};
use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tokio::sync::OwnedMutexGuard;

#[derive(Clone)]
pub struct WorkspaceHost {
    state: Arc<AppState>,
}
impl WorkspaceHost {
    pub(crate) fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    pub async fn prepare(
        &self,
        workspace: &str,
        force: bool,
        deadline: Instant,
    ) -> Result<PreparedWorkspace> {
        ensure!(
            Instant::now() < deadline,
            "workspace maintenance deadline elapsed"
        );
        let configuration = tokio::time::timeout_at(
            deadline.into(),
            crate::pro::manual_resume_configuration(&self.state).lock_owned(),
        )
        .await
        .context("workspace maintenance admission timed out")?;
        ensure!(
            crate::pro::is_worker(&self.state),
            "workspace maintenance requires configured worker"
        );
        let root = lock(&self.state.workspaces)
            .get(workspace)
            .context("workspace maintenance project absent")?
            .root;
        let dispatch = mutation::Dispatch::capture(&self.state, workspace)?;
        let generation = mutation::generation(&self.state);
        let reservation = mutation::begin_workspace_maintenance(&self.state, workspace)?;
        let mut prepared = PreparedWorkspace {
            state: self.state.clone(),
            workspace: workspace.to_owned(),
            root,
            generation,
            dispatch,
            reservation,
            _configuration: configuration,
            deadline,
            force,
            slots: Vec::new(),
            stop_started: false,
            settled: false,
        };
        prepared.current()?;
        let ids: Vec<_> = lock(&self.state.session_workspaces)
            .iter()
            .filter(|(_, bound)| bound.as_str() == workspace)
            .map(|(id, _)| id.clone())
            .collect();
        ensure!(ids.len() <= 64, "workspace maintenance session limit");
        for id in ids {
            let chat = self.state.chat.get(&id).filter(|info| info.alive);
            let terminal = self.state.sessions.get(&id).filter(|info| info.alive);
            if chat.is_none() && terminal.is_none() {
                continue;
            }
            ensure!(
                chat.is_none() || terminal.is_none(),
                "workspace maintenance surface ambiguous"
            );
            // Live terminal ingress has no positive queue-idle proof. Never
            // mistake a quiet screen or prompt mark for an empty PTY workload.
            ensure!(
                force || terminal.is_none(),
                "workspace terminal work is active"
            );
            let lifecycle = ChatSwitchGuard::acquire(&self.state, &id, "workspace-maintenance")
                .context("workspace session lifecycle busy")?;
            let ingress = if chat.is_some() {
                Some(if force {
                    Ingress::Force(
                        tokio::time::timeout_at(
                            deadline.into(),
                            self.state.chat.pause_commands(&id),
                        )
                        .await
                        .context("workspace command fence timed out")??,
                    )
                } else {
                    let native = chat
                        .as_ref()
                        .and_then(|info| info.native_session_id.as_deref())
                        .context("workspace native idle identity absent")?;
                    let mut idle = tokio::time::timeout_at(
                        deadline.into(),
                        self.state.chat.maintenance_idle(&id),
                    )
                    .await
                    .context("workspace idle proof timed out")??;
                    idle.drain_native(native, deadline).await?;
                    Ingress::Idle(idle)
                })
            } else {
                None
            };
            prepared.slots.push(Slot {
                id,
                chat,
                terminal,
                ingress,
                _lifecycle: lifecycle,
            });
        }
        prepared.current()?;
        Ok(prepared)
    }
}

enum Ingress {
    Idle(MaintenanceIdle),
    Force(PausedCommands),
}
impl Ingress {
    fn fence(&mut self) {
        match self {
            Self::Idle(owner) => owner.fence(),
            Self::Force(owner) => owner.fence(),
        }
    }
    fn pending(&self) -> bool {
        match self {
            Self::Idle(owner) => owner.cleanup_pending(),
            Self::Force(owner) => owner.cleanup_pending(),
        }
    }
}
struct Slot {
    id: String,
    chat: Option<chimaera_agent::ChatInfo>,
    terminal: Option<chimaera_pty::SessionInfo>,
    ingress: Option<Ingress>,
    _lifecycle: ChatSwitchGuard,
}

/// No serialization or credential getters: this is the original local owner.
/// Keep it through worker commit, including errors/observer disconnection.
pub struct PreparedWorkspace {
    state: Arc<AppState>,
    workspace: String,
    root: std::path::PathBuf,
    generation: u64,
    dispatch: mutation::Dispatch,
    reservation: mutation::WorkspaceMutation,
    _configuration: OwnedMutexGuard<()>,
    deadline: Instant,
    force: bool,
    slots: Vec<Slot>,
    stop_started: bool,
    settled: bool,
}
impl PreparedWorkspace {
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    pub fn current(&self) -> Result<()> {
        ensure!(
            Instant::now() < self.deadline
                && !self.state.stopping.load(Ordering::Acquire)
                && crate::pro::is_worker(&self.state)
                && mutation::generation(&self.state) == self.generation
                && self.reservation.current()
                && lock(&self.state.workspaces)
                    .get(&self.workspace)
                    .is_some_and(|workspace| workspace.root == self.root),
            "workspace maintenance owner changed"
        );
        self.dispatch.check(&self.state)?;
        self.same_sessions()?;
        for slot in &self.slots {
            if !self.force && !self.stop_started {
                if let Some(Ingress::Idle(owner)) = &slot.ingress {
                    owner.check_native(
                        slot.chat
                            .as_ref()
                            .and_then(|info| info.native_session_id.as_deref())
                            .context("workspace captured native identity absent")?,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn same_sessions(&self) -> Result<()> {
        for slot in &self.slots {
            ensure!(
                lock(&self.state.session_workspaces)
                    .get(&slot.id)
                    .is_none_or(|workspace| workspace == &self.workspace),
                "workspace session moved"
            );
            ensure!(
                self.state
                    .chat
                    .get(&slot.id)
                    .is_none_or(|now| slot
                        .chat
                        .as_ref()
                        .is_some_and(|old| now.created_at_ms == old.created_at_ms
                            && now.cwd == old.cwd
                            && now.native_session_id == old.native_session_id)),
                "workspace structured owner changed"
            );
            ensure!(
                self.state.sessions.get(&slot.id).is_none_or(|now| slot
                    .terminal
                    .as_ref()
                    .is_some_and(|old| now.pid == old.pid
                        && now.created_at == old.created_at
                        && now.cwd == old.cwd)),
                "workspace terminal owner changed"
            );
        }
        Ok(())
    }

    /// Durably retain original conversations before signalling any child.
    /// Only captured workspace owners are stopped; unrelated sessions continue.
    pub async fn stop(&mut self) -> Result<()> {
        self.current()?;
        if self.settled {
            return Ok(());
        }
        if self.stop_started {
            return self.wait_stopped().await;
        }
        let original = ledger::snapshot(&self.state).0;
        let mut deferred = Vec::with_capacity(self.slots.len());
        for slot in &self.slots {
            let mut entry = original
                .iter()
                .find(|entry| entry.id == slot.id && entry.workspace_id == self.workspace)
                .cloned()
                .context("workspace original session ledger absent")?;
            if let Some(Ingress::Idle(owner)) = &slot.ingress {
                let native = slot
                    .chat
                    .as_ref()
                    .and_then(|info| info.native_session_id.as_deref())
                    .context("workspace captured native identity absent")?;
                owner.check_native(native)?;
                ensure!(
                    entry
                        .agent
                        .as_ref()
                        .is_some_and(|agent| agent.ui == chimaera_agent::model::SessionUi::Chat
                            && agent.resume.as_deref() == Some(native)),
                    "workspace ledger native identity changed"
                );
            }
            // Only qualified structured conversations use same-ID manual Resume.
            // Forced PTY/TUI rows preserve existing explicit reopen; suspension
            // prevents boot from automatically respawning their old environment.
            entry.suspended = true;
            #[cfg(unix)]
            let manual = entry
                .agent
                .as_ref()
                .is_some_and(|agent| agent.ui == chimaera_agent::model::SessionUi::Chat)
                && ledger::manual::Receipt::for_entry(&entry).is_ok();
            #[cfg(not(unix))]
            let manual = false;
            if manual {
                // Explicit stop needs no idle proof, but manual Resume must
                // still name the conversation captured from this live owner.
                let native = slot
                    .chat
                    .as_ref()
                    .and_then(|info| info.native_session_id.as_deref())
                    .context("workspace captured native identity absent")?;
                ensure!(
                    entry
                        .agent
                        .as_ref()
                        .and_then(|agent| agent.resume.as_deref())
                        == Some(native),
                    "workspace ledger native identity changed"
                );
            }
            entry.manual_resume_reason = manual.then(|| "project_secrets_idle".into());
            deferred.push(entry);
        }
        {
            let mut entries = lock(&self.state.deferred_sessions);
            ensure!(
                entries.len().saturating_add(deferred.len()) <= 512,
                "workspace deferred session limit"
            );
            for entry in deferred {
                entries.insert(entry.id.clone(), entry);
            }
        }
        let state = self.state.clone();
        // Await actual write settlement, even if the absolute action deadline
        // expires. The enclosing retained task still owns our mutation guard.
        tokio::task::spawn_blocking(move || {
            let (entries, links) = ledger::snapshot(&state);
            #[cfg(unix)]
            {
                lock(&state.ledger).write_maintenance_durable(&entries, &links)
            }
            #[cfg(not(unix))]
            {
                lock(&state.ledger).write_durable(&entries, &links)
            }
        })
        .await
        .context("workspace ledger worker failed")??;
        self.current()?;
        // From this cutpoint current() must not reinterpret killed children as
        // a newly idle proof. Their original ingress handles prove settlement.
        self.stop_started = true;
        for slot in &mut self.slots {
            if let Some(owner) = &mut slot.ingress {
                owner.fence();
            }
            if slot.terminal.is_some() {
                self.state.sessions.fence(&slot.id)?;
            }
        }
        self.wait_stopped().await
    }

    async fn wait_stopped(&mut self) -> Result<()> {
        loop {
            self.same_sessions()?;
            if self.slots.iter().all(|slot| {
                slot.ingress.as_ref().is_none_or(|owner| !owner.pending())
                    && self.state.sessions.get(&slot.id).is_none()
                    && self.state.chat.get(&slot.id).is_none_or(|info| !info.alive)
            }) {
                self.settled = true;
                return self.current();
            }
            ensure!(
                Instant::now() < self.deadline,
                "workspace child cleanup timed out"
            );
            tokio::time::sleep_until(
                (Instant::now() + Duration::from_millis(25))
                    .min(self.deadline)
                    .into(),
            )
            .await;
        }
    }
}
