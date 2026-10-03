//! Owned, reversible parking transaction for the trusted inherited channel.
//! Structured idle and exact leader stops are not the supervisor's full census.
use super::{
    maintenance::{Launch, Owner},
    AppState,
};
use crate::{chat::ChatSwitchGuard, ledger::LedgerEntry, lock};
use chimaera_agent::{maintenance_idle::MaintenanceIdle, managed_process::ManagedProcess};
use chimaera_core::project_secret_idle::{BusyReason, Leader, Prepare};
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::OwnedMutexGuard;

fn fence() -> String {
    let hex = chimaera_core::generate_token();
    let bytes: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("fixed core token"))
        .collect();
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
}

pub(super) struct Parking {
    state: Arc<AppState>,
    prepare: Prepare,
    deadline: Instant,
    owner: Option<Owner>,
    _configuration: OwnedMutexGuard<()>,
    sessions: Vec<Paused>,
    originals: Vec<LedgerEntry>,
    deferred_before: HashMap<String, Option<LedgerEntry>>,
    pub(super) fence_id: String,
    record: Option<super::maintenance_store::Record>,
    worker_unknown: bool,
}
struct Paused {
    id: String,
    _lifecycle: ChatSwitchGuard,
    idle: MaintenanceIdle,
    process: Option<ManagedProcess>,
}
impl Parking {
    pub(super) async fn admit(
        state: &Arc<AppState>,
        prepare: &Prepare,
        deadline: Instant,
    ) -> Result<Self, BusyReason> {
        let configuration = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            state.pro.configuration.clone().lock_owned(),
        )
        .await
        .map_err(|_| BusyReason::Expired)?;
        let launch = Launch::capture(state, &prepare.binding)
            .map_err(|_| BusyReason::LifecycleOrTransfer)?;
        let owner = Owner::begin(state, &launch, prepare, deadline)
            .map_err(|_| BusyReason::SetupOrMutation)?;
        Ok(Self {
            state: state.clone(),
            prepare: prepare.clone(),
            deadline,
            owner: Some(owner),
            _configuration: configuration,
            sessions: Vec::new(),
            originals: Vec::new(),
            deferred_before: HashMap::new(),
            fence_id: fence(),
            record: None,
            worker_unknown: false,
        })
    }
    fn current(&self) -> Result<(), BusyReason> {
        if Instant::now() >= self.deadline {
            return Err(BusyReason::Expired);
        }
        if self
            .state
            .stopping
            .load(std::sync::atomic::Ordering::Acquire)
            || !*self.state.restored.borrow()
            || super::unclean(&self.state, &self.prepare.binding.workspace_id)
            || super::setup::active(&self.state, &self.prepare.binding.workspace_id)
            || crate::lock(&self.state.pro.installing).contains(&self.prepare.binding.workspace_id)
        {
            return Err(BusyReason::LifecycleOrTransfer);
        }
        self.owner
            .as_ref()
            .ok_or(BusyReason::ProcessUnknown)?
            .current()
            .map_err(|_| BusyReason::LifecycleOrTransfer)
    }
    pub(super) async fn prepare(&mut self) -> Result<Vec<Leader>, BusyReason> {
        self.current()?;
        let workspace = &self.prepare.binding.workspace_id;
        let ids: Vec<_> = lock(&self.state.session_workspaces)
            .iter()
            .filter(|(_, w)| *w == workspace)
            .map(|(id, _)| id.clone())
            .collect();
        if ids
            .iter()
            .any(|id| self.state.sessions.get(id).is_some_and(|row| row.alive))
        {
            return Err(BusyReason::TerminalWork);
        }
        let live: Vec<_> = ids
            .into_iter()
            .filter(|id| self.state.chat.get(id).is_some_and(|row| row.alive))
            .collect();
        if live.len() > 64 {
            return Err(BusyReason::LimitReached);
        }
        let ledger = crate::ledger::snapshot(&self.state).0;
        for id in live {
            let lifecycle = ChatSwitchGuard::acquire(&self.state, &id, "project-secrets-idle")
                .ok_or(BusyReason::LifecycleOrTransfer)?;
            let idle = tokio::time::timeout_at(
                tokio::time::Instant::from_std(self.deadline),
                self.state.chat.maintenance_idle(&id),
            )
            .await
            .map_err(|_| BusyReason::Expired)?
            .map_err(|_| BusyReason::PendingInput)?;
            self.current()?;
            let entry = ledger
                .iter()
                .find(|entry| entry.id == id && entry.workspace_id == *workspace)
                .cloned()
                .ok_or(BusyReason::Unresumable)?;
            if entry.agent.as_ref().is_none_or(|agent| {
                agent.ui != chimaera_agent::model::SessionUi::Chat || agent.resume.is_none()
            }) {
                return Err(BusyReason::Unresumable);
            }
            self.originals.push(entry);
            self.sessions.push(Paused {
                id,
                _lifecycle: lifecycle,
                idle,
                process: None,
            });
        }
        // The checked export resolver is reused without starting a transfer,
        // moving ownership, killing a process or producing a pickup prompt.
        let state = self.state.clone();
        let entries = self.originals.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            for entry in entries {
                let native = crate::bundle::native_path(&state, &entry)?
                    .ok_or_else(|| anyhow::anyhow!("native resume missing"))?;
                let (file, meta) = crate::fs::open_regular(&native)?;
                anyhow::ensure!(
                    meta.len() > 0 && meta.len() <= 100 * 1024 * 1024,
                    "native resume unavailable"
                );
                file.sync_all()?;
            }
            Ok(())
        })
        .await
        .map_err(|_| BusyReason::Unresumable)?
        .map_err(|_| BusyReason::Unresumable)?;
        self.current()?;
        self.owner
            .as_ref()
            .unwrap()
            .parking_started()
            .map_err(|_| BusyReason::LifecycleOrTransfer)?;
        // Keep the entire owned continuation alive across blocking operations.
        // A deadline error is observed only after the actual worker settles.
        let sessions = std::mem::take(&mut self.sessions);
        let worker = tokio::task::spawn_blocking(move || {
            let mut sessions = sessions;
            let result = (|| -> anyhow::Result<()> {
                for slot in &mut sessions {
                    let mut process = slot.idle.pin_process()?;
                    process.request_stop()?;
                    slot.process = Some(process);
                }
                Ok(())
            })();
            (sessions, result)
        })
        .await;
        let (sessions, stopped) = match worker {
            Ok(result) => result,
            Err(_) => {
                self.worker_unknown = true;
                return Err(BusyReason::ProcessUnknown);
            }
        };
        self.sessions = sessions;
        stopped.map_err(|_| BusyReason::ProcessUnknown)?;
        loop {
            self.current()?;
            let sessions = std::mem::take(&mut self.sessions);
            let worker = tokio::task::spawn_blocking(move || {
                let stopped = sessions.iter().all(|slot| {
                    slot.process
                        .as_ref()
                        .is_some_and(|process| process.is_stopped().unwrap_or(false))
                });
                (sessions, stopped)
            })
            .await;
            let (sessions, stopped) = match worker {
                Ok(result) => result,
                Err(_) => {
                    self.worker_unknown = true;
                    return Err(BusyReason::ProcessUnknown);
                }
            };
            self.sessions = sessions;
            if stopped {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        for slot in &mut self.sessions {
            slot.idle
                .drain(self.deadline)
                .await
                .map_err(|_| BusyReason::ProcessUnknown)?;
            slot.idle.check().map_err(|_| BusyReason::PendingInput)?;
            let expected = self
                .originals
                .iter()
                .find(|entry| entry.id == slot.id)
                .and_then(|entry| entry.agent.as_ref())
                .and_then(|agent| agent.resume.as_deref())
                .ok_or(BusyReason::Unresumable)?;
            slot.idle
                .check_native(expected)
                .map_err(|_| BusyReason::Unresumable)?;
        }
        // Native bytes may have a final protocol tail after the initial check.
        // Recheck and sync only after exact stop plus bounded journal draining.
        let state = self.state.clone();
        let entries = self.originals.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            for entry in entries {
                let native = crate::bundle::native_path(&state, &entry)?
                    .ok_or_else(|| anyhow::anyhow!("native resume missing"))?;
                let (file, meta) = crate::fs::open_regular(&native)?;
                anyhow::ensure!(
                    meta.len() > 0 && meta.len() <= 100 * 1024 * 1024,
                    "native resume unavailable"
                );
                file.sync_all()?;
                std::fs::File::open(
                    native
                        .parent()
                        .ok_or_else(|| anyhow::anyhow!("native resume storage missing"))?,
                )?
                .sync_all()?;
            }
            Ok(())
        })
        .await
        .map_err(|_| BusyReason::ProcessUnknown)?
        .map_err(|_| BusyReason::Unresumable)?;
        self.current()?;
        let leaders: Vec<_> = self
            .sessions
            .iter()
            .map(|slot| {
                let (namespace_pid, start_ticks) = slot.process.as_ref().unwrap().identity();
                Leader {
                    session_id: slot.id.clone(),
                    namespace_pid,
                    start_ticks,
                }
            })
            .collect();
        // The durable receipt precedes the ledger effect. A crash at any point
        // leaves an exact roster which startup must keep manually parked.
        let record = super::maintenance_store::Record::new(
            self.prepare.clone(),
            self.fence_id.clone(),
            leaders.clone(),
            &self.originals,
        )
        .map_err(|_| BusyReason::Unresumable)?;
        self.record = Some(record.clone());
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || super::maintenance_store::write(&state, &record))
            .await
            .map_err(|_| BusyReason::ProcessUnknown)?
            .map_err(|_| BusyReason::ProcessUnknown)?;
        self.current()?;
        let state = self.state.clone();
        let entries = self.originals.clone();
        {
            let mut deferred = lock(&self.state.deferred_sessions);
            if deferred.len()
                + entries
                    .iter()
                    .filter(|entry| !deferred.contains_key(&entry.id))
                    .count()
                > 512
            {
                return Err(BusyReason::LimitReached);
            }
            for mut entry in entries {
                self.deferred_before
                    .insert(entry.id.clone(), deferred.get(&entry.id).cloned());
                entry.suspended = true;
                entry.manual_resume_reason = Some("project_secrets_idle".into());
                deferred.insert(entry.id.clone(), entry);
            }
        }
        tokio::task::spawn_blocking(move || {
            let (entries, links) = crate::ledger::snapshot(&state);
            lock(&state.ledger).write_maintenance_durable(&entries, &links)
        })
        .await
        .map_err(|_| BusyReason::ProcessUnknown)?
        .map_err(|_| BusyReason::ProcessUnknown)?;
        self.current()?;
        Ok(leaders)
    }
    /// Positive rollback is exact old pidfds plus durable ledger before-images.
    /// No spawned replacement can enter while this admission owner remains.
    pub(super) async fn rollback(mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.worker_unknown,
            "maintenance process ownership unknown"
        );
        {
            let mut deferred = lock(&self.state.deferred_sessions);
            for (id, entry) in self.deferred_before.drain() {
                match entry {
                    Some(entry) => {
                        deferred.insert(id, entry);
                    }
                    None => {
                        deferred.remove(&id);
                    }
                }
            }
        }
        if self.record.is_some() || !self.originals.is_empty() {
            let state = self.state.clone();
            tokio::task::spawn_blocking(move || {
                let (entries, links) = crate::ledger::snapshot(&state);
                lock(&state.ledger).write_maintenance_durable(&entries, &links)
            })
            .await??;
        }
        let sessions = std::mem::take(&mut self.sessions);
        let owner = self.owner.take().unwrap();
        let (sessions, owner, resumed) = tokio::task::spawn_blocking(move || {
            let mut sessions = sessions;
            let result = (|| -> anyhow::Result<()> {
                for slot in &mut sessions {
                    if let Some(process) = &mut slot.process {
                        owner.resume_process(process)?;
                    }
                }
                Ok(())
            })();
            (sessions, owner, result)
        })
        .await?;
        self.sessions = sessions;
        self.owner = Some(owner);
        if let Err(error) = resumed {
            let mut deferred = lock(&self.state.deferred_sessions);
            for mut entry in self.originals.clone() {
                entry.suspended = true;
                entry.manual_resume_reason = Some("project_secrets_idle".into());
                deferred.insert(entry.id.clone(), entry);
            }
            return Err(error);
        }
        if let Some(record) = self.record.take() {
            let state = self.state.clone();
            tokio::task::spawn_blocking(move || super::maintenance_store::remove(&state, &record))
                .await??;
        }
        self.owner.take().unwrap().restored()?;
        // Destruction releases driver/pump and lifecycle only after positive
        // exact resume and stable before-images, never before cleanup settles.
        Ok(())
    }
}
