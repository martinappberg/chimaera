//! Internal managed-idle evidence. This is not a CLI capability or a process
//! census. The server must hold lifecycle/ownership and exact pidfd parking.
use super::*;
use std::time::Instant;
use tokio::sync::{oneshot, OwnedMutexGuard};

pub(crate) struct Drain {
    pub(crate) acknowledged: oneshot::Sender<bool>,
    pub(crate) release: oneshot::Receiver<()>,
}
pub(crate) struct PumpDrain {
    pub(crate) deadline: Instant,
    pub(crate) acknowledged: oneshot::Sender<Option<OwnedMutexGuard<()>>>,
    pub(crate) release: oneshot::Receiver<()>,
}
// Portable parking/manual Resume consumes native UUIDs: the daemon's existing
// bundle and parking-record validators require exactly 36 bytes. Evidence may
// accept smaller synthetic IDs, but must not retain a larger untrusted Init ID.
const NATIVE_EVIDENCE_MAX: usize = 36;

#[derive(Default)]
pub(crate) struct Evidence {
    native_init: Option<String>,
    completed: bool,
    failed: bool,
    tasks_present: bool,
}
impl Evidence {
    pub(crate) fn native_init(&self) -> Option<&str> {
        self.native_init.as_deref()
    }
    pub(crate) fn command(&mut self) {
        self.completed = false;
    }
    pub(crate) fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Init {
                native_session_id, ..
            } => {
                self.native_init = (!native_session_id.is_empty()
                    && native_session_id.len() <= NATIVE_EVIDENCE_MAX)
                    .then(|| native_session_id.clone());
            }
            AgentEvent::TurnStarted { .. } => self.completed = false,
            AgentEvent::TurnCompleted { .. } | AgentEvent::TurnAborted { .. } => {
                self.completed = true;
            }
            AgentEvent::Error { .. } | AgentEvent::Exited { .. } => self.failed = true,
            AgentEvent::BackgroundTasks { tasks, .. } => self.tasks_present = !tasks.is_empty(),
            _ => {}
        }
    }
}

/// Exact session ingress ownership, retained through parking and rollback.
/// A harness pause disables ticks and protocol consumption only after draining
/// produced output; no synthetic command or public journal marker is emitted.
pub struct MaintenanceIdle {
    session: Arc<ChatSession>,
    _commands: PausedCommands,
    release: Option<oneshot::Sender<()>>,
    pump: Option<OwnedMutexGuard<()>>,
    pump_release: Option<oneshot::Sender<()>>,
}
impl MaintenanceIdle {
    pub fn check(&self) -> Result<()> {
        let budget = self
            .session
            .command_budget
            .lock()
            .expect("command budget lock");
        let carry = self.session.carryover.lock().expect("carryover lock");
        let info = self.session.info.lock().expect("info lock");
        let idle = self
            .session
            .maintenance_evidence
            .lock()
            .expect("maintenance evidence lock");
        anyhow::ensure!(
            budget.commands_paused
                && budget.sends == 0
                && !budget.awaiting_turn
                && self.session.cmd_tx.capacity() == CMD_QUEUE
                && info.alive
                && self.session.maintenance_protocol_verified
                && info
                    .native_session_id
                    .as_ref()
                    .is_some_and(|id| !id.is_empty())
                && !info.pending_permission
                && !info.status_needs_action
                && info.remote_control_url.is_none()
                && info.background_running == 0
                && !carry.turn_in_flight
                && !carry.remote_control
                && !carry.ultracode
                && carry.background.is_empty()
                && idle.native_init.as_deref() == info.native_session_id.as_deref()
                && idle.completed
                && !idle.failed
                && !idle.tasks_present,
            "structured session idle proof unavailable"
        );
        Ok(())
    }
    /// Only after the positive pump drain owns absorption: match the retained
    /// ledger conversation to the nonempty Init actually emitted by this child.
    /// Pinned/inherited info alone never supplies current-process evidence.
    pub fn check_native(&self, expected: &str) -> Result<()> {
        anyhow::ensure!(self.pump.is_some(), "structured native drain unavailable");
        self.check()?;
        anyhow::ensure!(
            self.session
                .maintenance_evidence
                .lock()
                .expect("maintenance evidence lock")
                .native_init()
                == Some(expected)
                && !expected.is_empty(),
            "structured native identity changed"
        );
        Ok(())
    }
    /// Run off-reactor, with the server's exact lifecycle guard still held.
    #[cfg(target_os = "linux")]
    pub fn pin_process(&self) -> Result<crate::managed_process::ManagedProcess> {
        self.check()?;
        self.session.process_control.pin_process()
    }
    /// Caller must first positively confirm its exact leader stopped. The
    /// original absolute deadline covers driver, pump and journal barriers.
    pub async fn drain(&mut self, deadline: Instant) -> Result<()> {
        anyhow::ensure!(self.release.is_none(), "session already drained");
        let (acknowledged, ack) = oneshot::channel();
        let (release, released) = oneshot::channel();
        self.release = Some(release);
        self.session
            .maintenance_tx
            .try_send(Drain {
                acknowledged,
                release: released,
            })
            .map_err(|_| anyhow::anyhow!("structured session drain unavailable"))?;
        let until = tokio::time::Instant::from_std(deadline);
        let drained = tokio::time::timeout_at(until, ack).await??;
        anyhow::ensure!(drained, "structured session output remains unknown");
        self.drain_pump(deadline).await
    }
    async fn drain_pump(&mut self, deadline: Instant) -> Result<()> {
        // Only the pump can certify that a dequeued event finished absorption.
        // Empty channel capacity cannot observe an event awaiting its lock.
        let (acknowledged, ack) = oneshot::channel();
        let (release, released) = oneshot::channel();
        self.pump_release = Some(release);
        self.session
            .pump_maintenance_tx
            .try_send(PumpDrain {
                deadline,
                acknowledged,
                release: released,
            })
            .map_err(|_| anyhow::anyhow!("structured pump drain unavailable"))?;
        let until = tokio::time::Instant::from_std(deadline);
        self.pump = Some(
            tokio::time::timeout_at(until, ack)
                .await??
                .ok_or_else(|| anyhow::anyhow!("structured pump drain unknown"))?,
        );
        tokio::time::timeout_at(until, self.session.journal.sync_checked()).await??;
        self.check()
    }
}
impl Drop for MaintenanceIdle {
    fn drop(&mut self) {
        // Release the pump before allowing the driver to emit again. The
        // server resumes only its exact old pidfd before dropping this guard.
        self.pump.take();
        self.pump_release.take();
        self.release.take();
    }
}
impl ChatManager {
    pub async fn maintenance_idle(&self, id: &str) -> Result<MaintenanceIdle> {
        let commands = self.pause_commands(id).await?;
        let session = self.get_session(id)?;
        let guard = MaintenanceIdle {
            session,
            _commands: commands,
            release: None,
            pump: None,
            pump_release: None,
        };
        guard.check()?;
        Ok(guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::HeldCommands;

    fn init(native: &str) -> AgentEvent {
        AgentEvent::Init {
            native_session_id: native.into(),
            model: None,
            modes: Vec::new(),
            current_mode: None,
            slash_commands: Vec::new(),
            models: Vec::new(),
            agent_version: None,
            remote_control_available: false,
            remote_control_auto_enable: false,
            remote_control: None,
        }
    }
    #[tokio::test]
    async fn native_proof_rejects_preseeded_empty_init_and_changed_final_pump_identity() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(ChatManager::new(
            dir.path().join("chat"),
            Box::new(|_, _| {}),
            Box::new(|_, _| {}),
        ));
        let adapter = HeldCommands {
            commands: Arc::new(Mutex::new(None)),
        };
        let mut spec = SpawnSpec::new(
            "native-cutpoint",
            vec!["synthetic".into()],
            dir.path().into(),
        );
        spec.agent_version = Some(crate::claude::TESTED_CLAUDE_VERSION.into());
        spec.pinned_native_id = Some("original-native".into());
        manager.spawn(&adapter, spec).unwrap();
        let session = manager.get_session("native-cutpoint").unwrap();
        let events = session.annotate_tx.upgrade().unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        tokio::time::timeout_at(deadline, async {
            while session.journal.last_seq() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let oversized = "n".repeat(NATIVE_EVIDENCE_MAX + 1);
        for native in ["", oversized.as_str(), "original-native"] {
            let before = session.journal.last_seq();
            events.try_send(init(native)).unwrap();
            tokio::time::timeout_at(deadline, async {
                while session.journal.last_seq() <= before {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                manager
                    .resumed_native_ready("native-cutpoint", "original-native")
                    .await
                    .unwrap(),
                native == "original-native"
            );
            if native.is_empty() {
                assert_eq!(
                    session.info.lock().unwrap().native_session_id.as_deref(),
                    Some("original-native")
                );
                assert!(manager.maintenance_idle("native-cutpoint").await.is_err());
            }
            if native != "original-native" {
                assert!(manager.maintenance_idle("native-cutpoint").await.is_err());
                assert!(session
                    .maintenance_evidence
                    .lock()
                    .unwrap()
                    .native_init()
                    .is_none());
            }
        }
        let before = session.journal.last_seq();
        events
            .try_send(AgentEvent::TurnAborted {
                turn_id: "completed-idle".into(),
                reason: "synthetic completion".into(),
                interrupted: false,
            })
            .unwrap();
        tokio::time::timeout_at(deadline, async {
            while session.journal.last_seq() <= before {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut idle = manager.maintenance_idle("native-cutpoint").await.unwrap();
        // The ledger original was captured before this final protocol tail.
        // Positive pump draining must fold it before certifying native identity.
        let blocker = session.absorb_order.clone().lock_owned().await;
        events.try_send(init("different-native")).unwrap();
        tokio::time::timeout_at(deadline, async {
            while events.capacity() != EVENT_QUEUE {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let task = tokio::spawn(async move {
            idle.drain_pump(deadline.into_std()).await.unwrap();
            assert!(idle.check_native("original-native").is_err());
            idle.check_native("different-native").unwrap();
        });
        tokio::time::timeout_at(deadline, async {
            while session.pump_maintenance_tx.capacity() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!task.is_finished());
        drop(blocker);
        tokio::time::timeout_at(deadline, task)
            .await
            .unwrap()
            .unwrap();
        assert!(manager
            .resumed_native_ready("native-cutpoint", "original-native")
            .await
            .is_err());
        manager.kill("native-cutpoint");
    }

    #[tokio::test]
    async fn pump_barrier_folds_dequeued_final_event_before_idle_proof() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(ChatManager::new(
            dir.path().join("chat"),
            Box::new(|_, _| {}),
            Box::new(|_, _| {}),
        ));
        let adapter = HeldCommands {
            commands: Arc::new(Mutex::new(None)),
        };
        let mut spec = SpawnSpec::new("pump-cutpoint", vec!["synthetic".into()], dir.path().into());
        spec.agent_version = Some(crate::claude::TESTED_CLAUDE_VERSION.into());
        manager.spawn(&adapter, spec).unwrap();
        let session = manager.get_session("pump-cutpoint").unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        // The lifecycle reset is fully folded before creating the cutpoint.
        tokio::time::timeout_at(deadline, async {
            while session.journal.last_seq() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let blocker = session.absorb_order.clone().lock_owned().await;
        session.info.lock().unwrap().native_session_id = Some("native-fixture".into());
        *session.maintenance_evidence.lock().unwrap() = Evidence {
            native_init: Some("native-fixture".into()),
            completed: true,
            ..Default::default()
        };
        let mut idle = manager.maintenance_idle("pump-cutpoint").await.unwrap();
        let events = session.annotate_tx.upgrade().unwrap();
        events
            .try_send(AgentEvent::Error {
                message: "synthetic final busy event".into(),
                fatal: false,
            })
            .unwrap();
        tokio::time::timeout_at(deadline, async {
            while events.capacity() != EVENT_QUEUE {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Capacity is already empty, but the pump is blocked before folding
        // that final event. The barrier must stay pending at this exact seam.
        let task = tokio::spawn(async move { idle.drain_pump(deadline.into_std()).await });
        tokio::time::timeout_at(deadline, async {
            while session.pump_maintenance_tx.capacity() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!task.is_finished());
        drop(blocker);
        assert!(tokio::time::timeout_at(deadline, task)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert!(session.maintenance_evidence.lock().unwrap().failed);
        assert!(session.journal.last_seq() >= 2);
        manager.kill("pump-cutpoint");
    }

    #[tokio::test]
    async fn maintenance_idle_requires_positive_protocol_and_releases_failed_ingress() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(ChatManager::new(
            dir.path().join("chat"),
            Box::new(|_, _| {}),
            Box::new(|_, _| {}),
        ));
        let commands = Arc::new(Mutex::new(None));
        let adapter = HeldCommands { commands };
        let mut spec = SpawnSpec::new("idle", vec!["synthetic".into()], dir.path().into());
        spec.agent_version = Some(crate::claude::TESTED_CLAUDE_VERSION.into());
        manager.spawn(&adapter, spec).unwrap();
        let session = manager.get_session("idle").unwrap();
        assert!(manager.maintenance_idle("idle").await.is_err());
        assert!(!session.command_budget.lock().unwrap().commands_paused);
        session.info.lock().unwrap().native_session_id = Some("native-fixture".into());
        *session.maintenance_evidence.lock().unwrap() = Evidence {
            native_init: Some("native-fixture".into()),
            completed: true,
            ..Default::default()
        };
        let guard = manager.maintenance_idle("idle").await.unwrap();
        for reason in 0..7 {
            match reason {
                0 => session.command_budget.lock().unwrap().awaiting_turn = true,
                1 => session.carryover.lock().unwrap().turn_in_flight = true,
                2 => session.info.lock().unwrap().pending_permission = true,
                3 => session.info.lock().unwrap().background_running = 1,
                4 => session.carryover.lock().unwrap().remote_control = true,
                5 => session.info.lock().unwrap().status_needs_action = true,
                _ => session.maintenance_evidence.lock().unwrap().failed = true,
            }
            assert!(guard.check().is_err());
            session.command_budget.lock().unwrap().awaiting_turn = false;
            *session.carryover.lock().unwrap() = Carryover::default();
            let mut info = session.info.lock().unwrap();
            info.pending_permission = false;
            info.background_running = 0;
            info.status_needs_action = false;
            drop(info);
            session.maintenance_evidence.lock().unwrap().failed = false;
            guard.check().unwrap();
        }
        drop(guard);
        assert!(!session.command_budget.lock().unwrap().commands_paused);
        manager.kill("idle");
    }
}
