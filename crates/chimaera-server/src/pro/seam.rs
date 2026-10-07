//! The Pro host behind the workspace-admission hook (`crate::policy`). Shared
//! daemon code reaches Pro only through this implementation, installed when
//! an extension is composed (`AppState::install_policy`).
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::{execution, mutation, Tier};
use crate::policy::{
    Admission, AdmissionToken, BoxFuture, Hold, Installer, InstallerToken, Launch, LaunchContext,
    LaunchKind, LaunchToken, Need, Reservation, WorkspacePolicy,
};
use crate::AppState;

/// Pro's workspace policy. Holds the one-shot launcher record read before
/// the daemon's state existed, staged once the policy is installed.
#[derive(Default)]
pub(crate) struct ProPolicy {
    startup: Mutex<Option<execution::supervisor::Startup>>,
}

impl ProPolicy {
    /// Consume the trusted launcher's one-shot pipe before restore or helpers
    /// can inherit it (a cloud machine's supervisor; none anywhere else).
    pub(crate) async fn prepare() -> anyhow::Result<Self> {
        Ok(Self {
            startup: Mutex::new(super::read_supervisor_cleanup().await?),
        })
    }
}

/// Ready Pro on a state it now governs.
pub(crate) fn compose(state: &AppState) {
    // An unreadable import-recovery record fences what it might name; an
    // installation Pro never enrolled a project on has nothing it could
    // name, so ordinary work goes on.
    if !super::any_enrolled(state) {
        state.bundle_imports.release_unknown_fence();
    }
}

impl ProPolicy {
    pub(crate) fn stage(&self, state: &AppState) -> anyhow::Result<()> {
        let startup = crate::lock(&self.startup).take();
        super::stage_supervisor_cleanup(state, startup)
    }
}

struct Dispatched(mutation::Dispatch);
impl AdmissionToken for Dispatched {
    fn check(&self, state: &AppState) -> anyhow::Result<()> {
        self.0.check(state)
    }
    fn begin(&self, state: &AppState) -> anyhow::Result<Option<Reservation>> {
        Ok(self.0.begin(state)?.map(Reservation::new))
    }
    fn managed(&self, state: &AppState) -> bool {
        super::managed_execution(state, self.0.workspace())
    }
    fn installer<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        workspace: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Installer>> {
        Box::pin(async move {
            let running =
                execution::installer::Running::begin(state, workspace, self.0.clone()).await?;
            Ok(Installer::with(
                Admission::with(workspace, Arc::new(Dispatched(self.0.clone()))),
                state.clone(),
                Box::new(running),
            ))
        })
    }
}
impl InstallerToken for execution::installer::Running {
    fn attach(&mut self, group: u32) {
        execution::installer::Running::attach(self, group);
    }
    fn finish(self: Box<Self>) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin((*self).finish())
    }
    fn guarded(&self) -> bool {
        self.guarded()
    }
}

struct Launched {
    managed: bool,
    intent: Option<execution::launch::Intent>,
}
impl LaunchToken for Launched {
    fn managed(&self) -> bool {
        self.managed
    }
    fn check(&self) -> anyhow::Result<()> {
        self.intent.as_ref().map_or(Ok(()), |intent| intent.check())
    }
    fn registered(self: Box<Self>, id: String) {
        if let Some(intent) = self.intent {
            intent.registered(id);
        }
    }
}

impl WorkspacePolicy for ProPolicy {
    fn composed(&self, state: &AppState) -> bool {
        super::tier(state) != Tier::Free
    }
    fn active(&self, state: &AppState) -> bool {
        super::tier(state) == Tier::Active
    }
    fn allows(&self, state: &AppState, workspace: &str, need: Need) -> bool {
        match need {
            Need::Execute => super::may_execute(state, workspace),
            Need::Shell => super::may_run_shell(state, workspace),
            Need::Restore => super::may_restore(state, workspace),
        }
    }
    fn reserve(
        &self,
        state: &AppState,
        workspace: &str,
        kind: LaunchKind,
    ) -> anyhow::Result<Option<Reservation>> {
        let guard = match kind {
            LaunchKind::Agent => mutation::begin_launch(state, workspace)?,
            LaunchKind::Shell => mutation::begin_shell_launch(state, workspace)?,
        };
        Ok(guard.map(Reservation::new))
    }
    fn admit_launch<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        workspace: &'a str,
        kind: LaunchKind,
    ) -> BoxFuture<'a, anyhow::Result<(Launch, Option<Reservation>)>> {
        Box::pin(async move {
            // Plain shells are never managed: no fence signals them and no
            // stop waits for them. Only agents carry execution evidence.
            let managed = kind == LaunchKind::Agent && super::managed_execution(state, workspace);
            let intent = if managed {
                super::prepare_managed_launch(state, workspace).await?
            } else {
                None
            };
            let guard = self.reserve(state, workspace, kind)?;
            let launched = Launched { managed, intent };
            launched.check()?;
            Ok((Launch::with(Box::new(launched)), guard))
        })
    }
    fn hold_session<'a>(
        &'a self,
        state: &'a AppState,
        workspace: &str,
        session: &str,
        native: Option<&str>,
        read: bool,
    ) -> anyhow::Result<Hold<'a>> {
        mutation::check_import_resume(state, workspace)?;
        Ok(if read {
            Hold::new(
                state
                    .bundle_imports
                    .read_admission(workspace, session, native)?,
            )
        } else {
            Hold::new(state.bundle_imports.admit(workspace, session, native)?)
        })
    }
    fn check_import(
        &self,
        state: &AppState,
        session: &str,
        native: Option<&str>,
    ) -> anyhow::Result<()> {
        state.bundle_imports.check_session(session, native)
    }
    fn capture(&self, state: &AppState, workspace: &str) -> anyhow::Result<Admission> {
        let dispatch = mutation::Dispatch::capture(state, workspace)?;
        Ok(Admission::with(workspace, Arc::new(Dispatched(dispatch))))
    }
    fn launch_context(&self, state: &AppState, workspace: &str) -> LaunchContext {
        LaunchContext {
            recovery: super::checkpoint_recovery_context(state, workspace),
            tools: super::workspace_in_scope(state, workspace),
        }
    }
    fn launch_env<'a>(
        &'a self,
        state: &'a AppState,
        workspace: &'a str,
        env: &'a mut Vec<(String, String)>,
        remove: &'a mut Vec<String>,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(crate::daemon_extension::apply_session_environment(
            state, workspace, env, remove,
        ))
    }
    fn updates_managed(&self, state: &AppState) -> bool {
        super::updates_managed(state)
    }
    fn session_pause(
        &self,
        state: &AppState,
        id: &str,
        entry: Option<&crate::ledger::LedgerEntry>,
    ) -> Option<serde_json::Value> {
        super::pause::pause_for(state, id, entry).map(|pause| pause.frame())
    }
    fn owner(&self, state: &AppState, workspace: &str) -> Option<&'static str> {
        super::owner_kind(state, workspace)
    }
    fn refusal(&self, state: &AppState, id: &str, watching: bool) -> serde_json::Value {
        super::pause::refusal(state, id, watching)
    }
    fn acted(&self, state: &AppState, workspace: &str) {
        super::acted_here(state, workspace);
    }
    fn decorate_sessions(
        &self,
        state: &AppState,
        rows: &mut Vec<(u64, serde_json::Value)>,
    ) -> Vec<serde_json::Value> {
        // Input times are for Pro's idle checks only. Without an active plan
        // the field stays null, so a keystroke never changes the shared
        // sessions frame and every window is not sent a new one while
        // someone types.
        let activity = if super::tier(state) == Tier::Active {
            crate::lock(&state.activity)
                .snapshot(rows.iter().filter_map(|(_, row)| row["id"].as_str()))
        } else {
            Default::default()
        };
        // A daemon below the extension sends the rows exactly as before:
        // the additive Pro fields are absent, not null.
        if super::tier(state) != Tier::Free {
            for (_, row) in rows.iter_mut() {
                let at = row["id"].as_str().and_then(|id| activity.get(id)).copied();
                row["last_input_ms"] = json!(at);
                row["placement"] = json!("here");
                // Additive: the one pause verdict transfers use, so a cloud
                // machine's idle check never re-derives it from `agent_state`.
                if let Some(id) = row["id"].as_str() {
                    let paused =
                        super::activity::session(state, id) != super::activity::Activity::Working;
                    row["at_pause"] = json!(paused);
                }
            }
        }
        let deferred: Vec<_> = crate::lock(&state.deferred_sessions)
            .values()
            .cloned()
            .collect();
        for entry in &deferred {
            // A maintenance-parked leader can still exist stopped until the
            // supervisor's positive cleanup. Its live registry row must not
            // hide the durable manual-resume fence or suggest writable
            // execution.
            if entry.manual_resume_reason.is_some() {
                rows.retain(|(_, row)| row["id"] != entry.id);
            }
            if !rows.iter().any(|(_, row)| row["id"] == entry.id) {
                let label = super::paused_label(state, entry);
                let mut row = crate::bundle::paused_row(entry, label);
                // Additive: why it is paused, the same shape its socket says
                // it in, so a pane that shows no socket (a paused terminal)
                // can too.
                if let Some(pause) = super::pause::pause_for(state, &entry.id, Some(entry)) {
                    row["pause"] = pause.frame();
                }
                // Additive: the provider this paused session waits for (its
                // project otherwise runs), so the page can say what to connect.
                if let Some(provider) = super::blocking_provider(state, entry) {
                    row["blocked_provider"] = json!(provider);
                }
                rows.push((entry.created_at, row));
            }
        }
        state.session_proxy.rows()
    }
    fn decorate_workspace(&self, state: &AppState, workspace: &str, value: &mut serde_json::Value) {
        if let Some(copy) = super::local_copy_view(state, workspace) {
            value
                .as_object_mut()
                .expect("workspace object")
                .insert("local_copy".into(), copy);
        }
    }
    fn reads_folder_identity(&self, state: &AppState) -> bool {
        super::tier(state) != Tier::Free
    }
    fn workspace_opened<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        workspace: &'a crate::workspaces::Workspace,
        registered: Option<bool>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            use crate::workspaces::identity;
            match registered {
                // A folder registered (or re-registered) here.
                Some(write_marker) => {
                    // The marker exists for Pro's cloud copy: only a project
                    // Pro enrolled carries one, so a free user's folders
                    // never change.
                    if write_marker && super::marks_folder(state, &workspace.id) {
                        let (root, id) = (workspace.root.clone(), workspace.id.clone());
                        // Best effort, off the reactor and off the store's lock.
                        tokio::task::spawn_blocking(move || identity::write(&root, &id))
                            .await
                            .ok();
                    }
                }
                // A registered workspace opened again: an enrolled project's
                // folder with no marker yet gets its own (one stat, best
                // effort, never awaited).
                None => {
                    if super::marks_folder(state, &workspace.id) {
                        let (root, id) = (workspace.root.clone(), workspace.id.clone());
                        tokio::task::spawn_blocking(move || identity::backfill(&root, &id));
                    }
                }
            }
        })
    }
    fn workspace_known(&self, state: &AppState, workspace: &str) {
        // The user opened this project on this computer: it may come home
        // here (`note_opened`).
        super::note_opened(state, workspace);
    }
    fn health(&self, state: &AppState, body: &mut serde_json::Value) {
        // Assembly presence is independent of SDK build compatibility; a
        // daemon without the extension answers exactly as before.
        if state.daemon_extension.is_some() {
            body["daemon_extension"] = json!(true);
        }
        if let Some(identity) = state
            .daemon_extension
            .as_ref()
            .and_then(|runtime| runtime.assembly_identity())
        {
            body["daemon_assembly"] = json!(identity);
        }
        if let Some(ack) = super::supervisor_cleanup_ack(state) {
            body["supervisor_cleanup"] = json!(ack);
        }
        if crate::cloud::enabled() {
            body["pro_cloud_operations"] =
                json!(crate::cloud::active_operations() + super::active_operations(state));
            // Additive: the last user change that is not session input
            // (saves, uploads, Git operations), for the machine's idle
            // decision.
            body["last_activity_ms"] = json!(crate::activity::last_change(state));
        }
    }
    fn held_at_boot(&self, state: &AppState, entry: &crate::ledger::LedgerEntry) {
        // A returned session whose resume a sign-out or crash cut short is no
        // hand-off in flight any more: like any session the previous daemon
        // left, it waits for this life's ownership proof and the device
        // fallback, instead of answering "moved" forever.
        let interrupted = entry.suspended && super::interrupted_return(state, entry);
        if entry.manual_resume_reason.is_none() && (!entry.suspended || interrupted) {
            super::defer_boot_session(state, &entry.id);
        }
    }
    fn restored(&self, state: &Arc<AppState>) {
        // Laptop first: restart-deferred work resumes even when the account
        // never answers. A verified grant usually resumes it well before
        // this. Only `restore` defers, so a boot that deferred nothing
        // starts no fallback timer.
        if super::any_restart_deferred(state) {
            let owner = state.clone();
            tokio::spawn(async move {
                tokio::time::sleep(super::BOOT_VERIFICATION_GRACE).await;
                if !owner.stopping.load(std::sync::atomic::Ordering::Acquire) {
                    super::resume_unverified(&owner).await;
                }
            });
        }
    }
    fn may_resume(&self, state: &AppState, entry: &crate::ledger::LedgerEntry) -> bool {
        super::fence_current(state, entry)
    }
    fn resume_check(
        &self,
        state: &AppState,
        entry: &crate::ledger::LedgerEntry,
    ) -> anyhow::Result<()> {
        if entry.handoff.as_ref().is_some_and(|handoff| {
            super::owned_epoch(state, &entry.workspace_id)
                .is_some_and(|epoch| epoch != handoff.epoch)
        }) {
            anyhow::bail!("deferred bundle epoch is stale");
        }
        Ok(())
    }
    fn started(&self, state: &Arc<AppState>) -> anyhow::Result<()> {
        self.stage(state)?;
        // Transfer leftovers (staging copies, Git locks, temporary archives)
        // from a previous daemon life that never finished them.
        super::sweep_leftovers(state);
        Ok(())
    }
    fn stopping(&self, state: &AppState) {
        let _ = state;
    }
    fn shutdown<'a>(&'a self, state: &'a Arc<AppState>) -> BoxFuture<'a, ()> {
        // Managed agents are proven stopped here, so a same-boot successor
        // (an update or restart) does not treat their launch evidence as a
        // crash.
        Box::pin(super::shutdown(state))
    }
    fn routes(&self) -> axum::Router<Arc<AppState>> {
        super::routes::router()
    }
}
