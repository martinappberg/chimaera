use super::*;
use crate::workspace_maintenance::WorkspaceHost;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn fixture() -> (Arc<AppState>, PathBuf, String, String) {
    let root = std::env::temp_dir().join(format!(
        "chimaera-workspace-maintenance-{}",
        chimaera_core::generate_token()
    ));
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    let state = Arc::new(AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.clone(),
        root.join("config"),
    ));
    let a = lock(&state.workspaces).add(root.join("a")).unwrap().id;
    let b = lock(&state.workspaces).add(root.join("b")).unwrap().id;
    super::super::install_fixture(&state, &a, 4).unwrap();
    super::super::install_fixture(&state, &b, 4).unwrap();
    *lock(&state.pro().runtime) = Some(serde_json::from_value(serde_json::json!({
        "role":"worker", "account_id":"a-fixture", "endpoint":"http://127.0.0.1:1", "keeper_url":"",
        "delegation":{"access_token":"synthetic", "expires_at":"2099-01-01T00:00:00Z", "scope":["baton","mirror"],"device_id":"d-home"}
    })).unwrap());
    (state, root, a, b)
}
fn terminal(state: &Arc<AppState>, workspace: &str, cwd: PathBuf) -> String {
    let info = state
        .sessions
        .spawn_managed(chimaera_pty::SpawnOpts {
            cwd,
            name: None,
            cols: 80,
            rows: 24,
            command: Some(vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()]),
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .unwrap();
    lock(&state.session_workspaces).insert(info.id.clone(), workspace.to_owned());
    info.id
}
async fn cleanup(state: &Arc<AppState>) {
    state.sessions.kill_all();
    for info in state.chat.list() {
        state.chat.fence(&info.id);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    tokio::time::timeout_at(deadline, async {
        while !state.sessions.list().is_empty()
            || state
                .chat
                .list()
                .iter()
                .any(|info| info.alive || state.chat.process_group(&info.id).is_some())
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn exclusive_workspace_uses_original_mutation_store_and_preserves_sibling() {
    let (state, root, a, b) = fixture();
    let host = WorkspaceHost::new(state.clone());
    let prepared = host
        .prepare(&a, false, Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
    assert!(prepared.current().is_ok());
    assert_eq!(prepared.workspace(), a);
    assert!(begin_launch(&state, &a).is_err());
    assert!(begin(&state, &a, 4, generation(&state)).is_err());
    assert!(begin_launch(&state, &b).unwrap().is_some());
    assert!(begin_workspace_maintenance(&state, &a).is_err());
    assert!(crate::pro::manual_resume_configuration(&state)
        .try_lock_owned()
        .is_err());
    drop(prepared);
    assert!(begin_launch(&state, &a).unwrap().is_some());
    assert!(crate::pro::manual_resume_configuration(&state)
        .try_lock_owned()
        .is_ok());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn idle_refuses_live_terminal_but_force_persists_original_before_owned_stop() {
    let (state, root, a, b) = fixture();
    let original = terminal(&state, &a, root.join("a"));
    let sibling = terminal(&state, &b, root.join("b"));
    let tui = terminal(&state, &a, root.join("a"));
    let native = "22222222-3333-4444-8555-666666666666";
    let transcript = root.join(format!("{native}.jsonl"));
    std::fs::write(&transcript, b"retained synthetic TUI transcript\n").unwrap();
    let mut record =
        crate::agent_state::AgentRecord::new("fixture".into(), crate::agents::AgentKind::Claude);
    record.transcript_path = Some(transcript.clone());
    record.resumed_from = Some(native.into());
    record.native_cwd = Some(root.join("a"));
    lock(&state.agents).insert(tui.clone(), record);
    let host = WorkspaceHost::new(state.clone());
    let deadline = Instant::now() + Duration::from_secs(8);
    let owner = state.clone();
    let owned_root = root.clone();
    let outcome = async move {
        let state = owner;
        let root = owned_root;
        assert!(host.prepare(&a, false, deadline).await.is_err());
        assert!(state.sessions.get(&original).unwrap().alive);
        assert!(state.sessions.get(&sibling).unwrap().alive);
        assert!(lock(&state.deferred_sessions).is_empty());
        let mut prepared = host.prepare(&a, true, deadline).await.unwrap();
        assert!(state.sessions.get(&original).unwrap().alive);
        prepared.stop().await.unwrap();
        assert!(state.sessions.get(&original).is_none());
        assert!(state.sessions.get(&tui).is_none());
        assert!(state.sessions.get(&sibling).unwrap().alive);
        assert!(begin_launch(&state, &a).is_err());
        let rows = crate::ledger::LedgerStore::new(root.join("sessions.json"))
            .load_boot()
            .sessions;
        let entry = rows.iter().find(|entry| entry.id == original).unwrap();
        assert_eq!(entry.workspace_id, a);
        assert!(entry.suspended);
        assert!(entry.manual_resume_reason.is_none());
        let tui_entry = rows.iter().find(|entry| entry.id == tui).unwrap();
        assert!(tui_entry.suspended);
        assert!(tui_entry.manual_resume_reason.is_none());
        let tui_agent = tui_entry.agent.as_ref().unwrap();
        assert_eq!(tui_agent.ui, chimaera_agent::model::SessionUi::Term);
        assert_eq!(tui_agent.resume.as_deref(), Some(native));
        assert_eq!(tui_agent.transcript.as_ref(), Some(&transcript));
        assert_eq!(tui_agent.native_cwd, Some(root.join("a")));
        assert!(rows
            .iter()
            .find(|entry| entry.id == sibling)
            .unwrap()
            .manual_resume_reason
            .is_none());
        prepared.stop().await.unwrap();
        drop(prepared);
        assert!(begin_launch(&state, &a).unwrap().is_some());
    };
    // The test owns both actual children through cleanup even on assertion loss.
    let result = tokio::spawn(outcome).await;
    cleanup(&state).await;
    std::fs::remove_dir_all(root).unwrap();
    result.unwrap();
}

#[tokio::test]
async fn stale_deadline_and_non_worker_cannot_publish_a_workspace_reservation() {
    let (state, root, a, _) = fixture();
    let host = WorkspaceHost::new(state.clone());
    assert!(host.prepare(&a, false, Instant::now()).await.is_err());
    *lock(&state.pro().runtime) = None;
    assert!(host
        .prepare(&a, false, Instant::now() + Duration::from_secs(5))
        .await
        .is_err());
    assert!(!workspace_closed(&state, &a));
    assert!(begin_launch(&state, &a).unwrap().is_some());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn structured_idle_rejects_mismatched_ledger_native_before_owned_stop() {
    use chimaera_agent::{claude::ClaudeAdapter, driver::SpawnSpec, model::AgentEvent};
    use std::os::unix::fs::PermissionsExt;
    const NATIVE: &str = "11111111-2222-4333-8444-555555555555";
    const ID: &str = "s-workspace-idle";
    let (state, root, a, _) = fixture();
    let bin = root.join("synthetic-claude");
    std::fs::write(&bin, format!(
        "#!/bin/sh\n\
         printf '%s\\n' '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"init\",\"response\":{{\"commands\":[]}}}}}}'\n\
         printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"{NATIVE}\",\"model\":\"synthetic\",\"permissionMode\":\"default\",\"slash_commands\":[]}}'\n\
         printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"id\":\"completed-message\",\"content\":[{{\"type\":\"text\",\"text\":\"synthetic completed turn\"}}]}}}}'\n\
         printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"{NATIVE}\",\"result\":\"done\",\"num_turns\":1}}'\n\
         cat >/dev/null\n")).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    lock(&state.session_workspaces).insert(ID.into(), a.clone());
    let mut spec = SpawnSpec::new(ID, vec![bin.to_string_lossy().into_owned()], root.join("a"));
    spec.managed_execution = true;
    spec.agent_version = Some(chimaera_agent::claude::TESTED_CLAUDE_VERSION.into());
    state.chat.spawn(&ClaudeAdapter, spec).unwrap();
    let worker = state.clone();
    let result = tokio::spawn(async move {
        let attached = worker.chat.attach(ID, 0).unwrap();
        let mut live = attached.live;
        let deadline = Instant::now() + Duration::from_secs(8);
        if !attached
            .replay
            .iter()
            .any(|event| matches!(event.ev, AgentEvent::TurnCompleted { .. }))
        {
            tokio::time::timeout_at(deadline.into(), async {
                while !matches!(
                    live.recv().await.unwrap().ev,
                    AgentEvent::TurnCompleted { .. }
                ) {}
            })
            .await
            .unwrap();
        }
        let mut prepared = WorkspaceHost::new(worker.clone())
            .prepare(&a, false, deadline)
            .await
            .unwrap();
        assert!(worker.chat.pause_commands(ID).await.is_err());
        assert!(worker.chat.get(ID).unwrap().alive);
        let mut wrong = crate::ledger::snapshot(&worker)
            .0
            .into_iter()
            .find(|entry| entry.id == ID)
            .unwrap();
        wrong.agent.as_mut().unwrap().resume = Some("22222222-3333-4444-8555-666666666666".into());
        wrong.manual_resume_reason = Some("project_secrets_idle".into());
        lock(&worker.deferred_sessions).insert(ID.into(), wrong.clone());
        assert_eq!(
            prepared.stop().await.unwrap_err().to_string(),
            "workspace ledger native identity changed"
        );
        assert!(worker.chat.get(ID).unwrap().alive);
        assert_eq!(lock(&worker.deferred_sessions).get(ID), Some(&wrong));
        lock(&worker.deferred_sessions).remove(ID);
        drop(prepared);
        // Force does not require idle, but cannot park a different native
        // conversation from a stale deferred ledger entry for manual Resume.
        let mut forced = WorkspaceHost::new(worker.clone())
            .prepare(&a, true, deadline)
            .await
            .unwrap();
        lock(&worker.deferred_sessions).insert(ID.into(), wrong.clone());
        assert_eq!(
            forced.stop().await.unwrap_err().to_string(),
            "workspace ledger native identity changed"
        );
        assert!(worker.chat.get(ID).unwrap().alive);
        assert_eq!(lock(&worker.deferred_sessions).get(ID), Some(&wrong));
        lock(&worker.deferred_sessions).remove(ID);
        drop(forced);
        let mut prepared = WorkspaceHost::new(worker.clone())
            .prepare(&a, false, deadline)
            .await
            .unwrap();
        prepared.stop().await.unwrap();
        assert!(!worker.chat.get(ID).is_some_and(|info| info.alive));
        assert!(worker.chat.process_group(ID).is_none());
        let entry = lock(&worker.deferred_sessions).get(ID).unwrap().clone();
        assert_eq!(entry.id, ID);
        assert_eq!(entry.workspace_id, a);
        assert_eq!(
            entry.agent.as_ref().unwrap().resume.as_deref(),
            Some(NATIVE)
        );
        assert_eq!(
            entry.manual_resume_reason.as_deref(),
            Some("project_secrets_idle")
        );
        assert!(lock(&worker.chat_switching).contains_key(ID));
        assert!(begin_launch(&worker, &a).is_err());
        drop(prepared);
        assert!(!lock(&worker.chat_switching).contains_key(ID));
        assert!(begin_launch(&worker, &a).unwrap().is_some());
    })
    .await;
    cleanup(&state).await;
    std::fs::remove_dir_all(root).unwrap();
    result.unwrap();
}

struct SelectedRuntime;
impl crate::daemon_extension::Runtime for SelectedRuntime {
    fn coordinate(
        &self,
        _owner: crate::daemon_extension::CoordinatorOwner,
    ) -> crate::daemon_extension::RuntimeFuture {
        Box::pin(async {})
    }
}
fn ordinary_fixture(
    extension: Option<Arc<dyn crate::daemon_extension::Runtime>>,
) -> (Arc<AppState>, PathBuf, String, String) {
    let root = std::env::temp_dir().join(format!(
        "chimaera-ordinary-maintenance-{}",
        chimaera_core::generate_token()
    ));
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    let state = AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.clone(),
        root.join("config"),
    );
    if let Some(runtime) = extension {
        state.pro().set_runtime(runtime);
    }
    let state = Arc::new(state);
    let a = lock(&state.workspaces).add(root.join("a")).unwrap().id;
    let b = lock(&state.workspaces).add(root.join("b")).unwrap().id;
    (state, root, a, b)
}

#[tokio::test]
async fn ordinary_selected_worker_maintenance_preserves_sibling_and_original_generation() {
    let (state, root, a, b) = ordinary_fixture(Some(Arc::new(SelectedRuntime)));
    state.pro().worker.store(true, Ordering::Release);
    let sibling = terminal(&state, &b, root.join("b"));
    let worker = state.clone();
    let result = tokio::spawn(async move {
        assert!(lock(&worker.pro().runtime).is_none());
        assert!(lock(&worker.pro().ownership).is_empty());
        let mut prepared = WorkspaceHost::new(worker.clone())
            .prepare(&a, false, Instant::now() + Duration::from_secs(5))
            .await
            .unwrap();
        assert!(prepared.current().is_ok());
        assert!(begin_launch(&worker, &a).is_err());
        assert!(begin_shell_launch(&worker, &a).is_err());
        assert!(begin_launch(&worker, &b).unwrap().is_some());
        prepared.stop().await.unwrap();
        assert!(worker.sessions.get(&sibling).unwrap().alive);
        assert!(lock(&worker.pro().runtime).is_none());
        assert!(lock(&worker.pro().ownership).is_empty());
        worker.pro().generation.fetch_add(1, Ordering::AcqRel);
        assert!(prepared.current().is_err());
        drop(prepared);
        assert!(begin_launch(&worker, &a).unwrap().is_some());
    })
    .await;
    cleanup(&state).await;
    std::fs::remove_dir_all(root).unwrap();
    result.unwrap();
}

struct HeldEnvironment {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    worker: Arc<std::sync::atomic::AtomicBool>,
}
impl crate::daemon_extension::Runtime for HeldEnvironment {
    fn coordinate(
        &self,
        _owner: crate::daemon_extension::CoordinatorOwner,
    ) -> crate::daemon_extension::RuntimeFuture {
        Box::pin(async {})
    }
    fn session_environment<'a>(
        &'a self,
        _workspace: &'a str,
        worker: bool,
    ) -> crate::daemon_extension::EnvironmentFuture<'a> {
        Box::pin(async move {
            self.worker.store(worker, Ordering::Release);
            self.entered.notify_one();
            self.release.notified().await;
            Ok(Vec::new())
        })
    }
}

#[tokio::test]
async fn ordinary_worker_launch_blocks_maintenance_through_actual_environment_wait() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let observed_worker = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (state, root, a, b) = ordinary_fixture(Some(Arc::new(HeldEnvironment {
        entered: entered.clone(),
        release: release.clone(),
        worker: observed_worker.clone(),
    })));
    state.pro().worker.store(true, Ordering::Release);
    let sibling = terminal(&state, &b, root.join("b"));
    let worker = state.clone();
    let result = tokio::spawn(async move {
        let workspace = lock(&worker.workspaces).get(&a).unwrap();
        let id = "s-ordinary-counted-launch";
        let mut launch = Box::pin(crate::spawn::spawn_session(
            &worker,
            crate::spawn::SpawnSpec {
                workspace,
                id: Some(id.into()),
                name: None,
                cwd: None,
                cols: None,
                rows: None,
                theme: "dark".into(),
                title_hint: None,
                prelude: None,
                kind: crate::spawn::SpawnKind::Shell,
                fork_head: false,
                native_cwd: None,
                started_by: crate::history::StartedBy::You,
            },
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                _ = entered.notified() => {},
                _ = &mut launch => panic!("launch did not retain its environment wait"),
            }
        })
        .await
        .unwrap();
        assert!(observed_worker.load(Ordering::Acquire));
        assert!(!idle(&worker, &a));
        assert!(worker.sessions.get(id).is_none());
        assert!(WorkspaceHost::new(worker.clone())
            .prepare(&a, false, Instant::now() + Duration::from_secs(5))
            .await
            .is_err());
        assert!(!workspace_closed(&worker, &a));
        assert!(begin_shell_launch(&worker, &b).unwrap().is_some());
        assert!(worker.sessions.get(&sibling).unwrap().alive);
        release.notify_one();
        assert!(tokio::time::timeout(Duration::from_secs(5), &mut launch)
            .await
            .unwrap()
            .is_ok());
        assert!(worker.sessions.get(id).is_some_and(|row| row.alive));
        assert!(idle(&worker, &a));
        assert!(WorkspaceHost::new(worker.clone())
            .prepare(&a, false, Instant::now() + Duration::from_secs(5))
            .await
            .is_err());
        let mut prepared = WorkspaceHost::new(worker.clone())
            .prepare(&a, true, Instant::now() + Duration::from_secs(5))
            .await
            .unwrap();
        prepared.stop().await.unwrap();
        assert!(worker.sessions.get(id).is_none());
        assert!(worker.sessions.get(&sibling).unwrap().alive);
        assert!(begin_shell_launch(&worker, &a).is_err());
        drop(prepared);
        assert!(begin_shell_launch(&worker, &a).unwrap().is_some());
    })
    .await;
    cleanup(&state).await;
    std::fs::remove_dir_all(root).unwrap();
    result.unwrap();
}

#[tokio::test]
async fn ordinary_maintenance_does_not_admit_absent_extension_or_account_bound_projects() {
    let (state, root, a, _) = ordinary_fixture(None);
    assert!(begin_launch(&state, &a).unwrap().is_none());
    state.pro().worker.store(true, Ordering::Release);
    assert!(begin_launch(&state, &a).unwrap().is_none());
    assert!(WorkspaceHost::new(state.clone())
        .prepare(&a, false, Instant::now() + Duration::from_secs(5))
        .await
        .is_err());
    assert!(!workspace_closed(&state, &a));
    std::fs::remove_dir_all(root).unwrap();

    let (state, root, a, _) = ordinary_fixture(Some(Arc::new(SelectedRuntime)));
    assert!(begin_launch(&state, &a).unwrap().is_none());
    assert!(WorkspaceHost::new(state.clone())
        .prepare(&a, false, Instant::now() + Duration::from_secs(5))
        .await
        .is_err());
    state.pro().worker.store(true, Ordering::Release);
    lock(&state.pro().preferences)
        .entry(a.clone())
        .or_default()
        .account = Some("a-foreign".into());
    assert!(begin_workspace_maintenance(&state, &a).is_err());
    assert!(WorkspaceHost::new(state.clone())
        .prepare(&a, false, Instant::now() + Duration::from_secs(5))
        .await
        .is_err());
    assert!(!workspace_closed(&state, &a));
    std::fs::remove_dir_all(root).unwrap();
}
