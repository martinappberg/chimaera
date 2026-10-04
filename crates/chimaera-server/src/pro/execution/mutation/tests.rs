use super::super::{clear_stopped, fence_workspace, install_fixture, quiescent};
use super::*;

fn fixture() -> (Arc<AppState>, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "chimaera-mutation-{}",
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
    install_fixture(&state, "w-a", 4).unwrap();
    (state, root)
}

#[test]
fn stopped_commit_blocks_replacement_even_after_proof_is_cleared() {
    let (state, root) = fixture();
    let commit = begin(&state, "w-a", 4, generation(&state)).unwrap();
    fence_workspace(&state, "w-a");
    assert!(begin(&state, "w-a", 4, generation(&state)).is_err());
    assert!(!quiescent(&state, "w-a"));
    clear_stopped(&state);
    assert!(install_fixture(&state, "w-a", 5).is_err());
    drop(commit);
    assert!(quiescent(&state, "w-a"));
    install_fixture(&state, "w-a", 5).unwrap();
    assert!(begin(&state, "w-a", 4, generation(&state)).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn commits_are_bounded_and_old_account_generations_cannot_enter() {
    let (state, root) = fixture();
    let commits: Vec<_> = (0..64)
        .map(|_| begin(&state, "w-a", 4, 0).unwrap())
        .collect();
    assert!(begin(&state, "w-a", 4, 0).is_err());
    drop(commits);
    assert!(idle(&state, "w-a"));
    state.pro.generation.fetch_add(1, Ordering::AcqRel);
    assert!(begin(&state, "w-a", 4, 0).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_final_launch_is_drained_after_its_child_registers() {
    use crate::agent_state::AgentKind;
    use chimaera_pty::SpawnOpts;
    use std::time::Duration;
    let (state, root) = fixture();
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    install_fixture(&state, &workspace.id, 4).unwrap();
    // Laptop work must remain available even when the publication lease lapses.
    lock(&state.pro.execution.proofs)
        .get_mut(&workspace.id)
        .unwrap()
        .deadline = super::super::lease::Deadline::expired_fixture();
    let guard = begin_launch(&state, &workspace.id).unwrap().unwrap();
    assert!(begin_launch(&state, "w-free").unwrap().is_none());
    let owner = state.clone();
    let workspace_id = workspace.id.clone();
    let cwd = root.clone();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (resume, paused) = std::sync::mpsc::channel();
    let launch = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        entered.send(()).unwrap();
        paused.recv_timeout(Duration::from_secs(5)).unwrap();
        let session = owner
            .sessions
            .spawn(SpawnOpts {
                cwd,
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
        lock(&owner.agents).insert(
            session.id.clone(),
            crate::agent_state::AgentRecord::new("fixture".into(), AgentKind::Claude),
        );
        lock(&owner.session_workspaces).insert(session.id.clone(), workspace_id.clone());
        assert!(!crate::pro::may_execute(&owner, &workspace_id));
        // This is the production spawn's post-registration authority check.
        owner.sessions.fence(&session.id).unwrap();
        session.id
    });
    ready.await.unwrap();
    launch.abort();
    lock(&state.pro.ownership).insert(workspace.id.clone(), Ownership::Transferring { epoch: 4 });
    assert!(begin_launch(&state, &workspace.id).is_err());
    let stopping = state.clone();
    let workspace_id = workspace.id.clone();
    let stop = tokio::spawn(async move { super::super::stop(&stopping, &[workspace_id]).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !stop.is_finished(),
        "drain must see the not-yet-registered launch"
    );
    assert!(!quiescent(&state, &workspace.id));
    resume.send(()).unwrap();
    let id = launch.await.unwrap();
    stop.await.unwrap().unwrap();
    assert!(!state.sessions.get(&id).is_some_and(|session| session.alive));
    assert!(quiescent(&state, &workspace.id));
    assert!(idle(&state, &workspace.id));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn final_worker_launch_still_requires_its_live_proof() {
    let (state, root) = fixture();
    super::super::worker_fixture(&state);
    let guard = begin_launch(&state, "w-a").unwrap().unwrap();
    drop(guard);
    lock(&state.pro.execution.proofs)
        .get_mut("w-a")
        .unwrap()
        .deadline = super::super::lease::Deadline::expired_fixture();
    assert!(begin_launch(&state, "w-a").is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn plain_shell_renewal_window_keeps_original_execution_and_counted_maintenance() {
    let (state, root) = fixture();
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    install_fixture(&state, &workspace.id, 4).unwrap();
    super::super::worker_fixture(&state);
    lock(&state.pro.execution.proofs)
        .get_mut(&workspace.id)
        .unwrap()
        .deadline = super::super::lease::Deadline::expired_fixture();
    assert!(!crate::pro::may_execute(&state, &workspace.id));
    assert!(super::super::resumed(&state, generation(&state)));
    assert!(super::super::resuming(&state, &workspace.id));
    assert!(crate::pro::may_execute(&state, &workspace.id));
    assert!(begin_launch(&state, &workspace.id).is_err());
    assert!(begin_shell_launch(&state, "w-free").unwrap().is_none());

    let shell = begin_shell_launch(&state, &workspace.id).unwrap().unwrap();
    assert!(!idle(&state, &workspace.id));
    assert!(begin_workspace_maintenance(&state, &workspace.id).is_err());
    drop(shell);
    assert!(idle(&state, &workspace.id));
    let maintenance = begin_workspace_maintenance(&state, &workspace.id).unwrap();
    assert!(begin_shell_launch(&state, &workspace.id).is_err());
    drop(maintenance);

    // Exercise the actual shared spawn path, not just the new entry point.
    let id = "s-renewing-shell";
    let result = crate::spawn::spawn_session(
        &state,
        crate::spawn::SpawnSpec {
            workspace: workspace.clone(),
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
    )
    .await;
    assert!(result.is_ok());
    assert!(state.sessions.get(id).is_some_and(|session| session.alive));
    assert!(idle(&state, &workspace.id));
    assert!(begin_launch(&state, &workspace.id).is_err());
    state.sessions.kill(id).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while state.sessions.get(id).is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    lock(&state.pro.ownership).insert(
        workspace.id.clone(),
        Ownership::Remote {
            epoch: 5,
            holder: "other-worker".into(),
        },
    );
    assert!(!crate::pro::may_execute(&state, &workspace.id));
    assert!(begin_shell_launch(&state, &workspace.id).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn import_reservation_accepts_hydration_and_rejects_stale_generation_epoch_and_expiry() {
    let (state, root) = fixture();
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Hydrating { epoch: 4 });
    let guard = begin_import(&state, "w-a", 4, 0).await.unwrap();
    guard.check(&state).unwrap();
    assert!(!idle(&state, "w-a"));
    assert!(state.pro.configuration.try_lock().is_err());
    lock(&state.pro.execution.proofs)
        .get_mut("w-a")
        .unwrap()
        .deadline = super::super::lease::Deadline::expired_fixture();
    assert!(guard.check(&state).is_err());
    drop(guard);
    assert!(begin_import(&state, "w-a", 4, 0).await.is_err());
    super::super::renewed_fixture(&state, "w-a", 4).unwrap();
    assert!(begin_import(&state, "w-a", 5, 0).await.is_err());
    let guard = begin_import(&state, "w-a", 4, 0).await.unwrap();
    state.pro.generation.fetch_add(1, Ordering::AcqRel);
    assert!(guard.check(&state).is_err());
    drop(guard);
    assert!(begin_import(&state, "w-a", 4, 0).await.is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn owned_import_survives_caller_cancellation_and_blocks_replacement_until_settled() {
    let (state, root) = fixture();
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Hydrating { epoch: 4 });
    let guard = begin_import(&state, "w-a", 4, 0).await.unwrap();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (resume, paused) = std::sync::mpsc::channel();
    let task = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        entered.send(()).unwrap();
        paused
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
    });
    ready.await.unwrap();
    task.abort();
    fence_workspace(&state, "w-a");
    clear_stopped(&state);
    assert!(!quiescent(&state, "w-a"));
    assert!(install_fixture(&state, "w-a", 5).is_err());
    assert!(state.pro.configuration.try_lock().is_err());
    resume.send(()).unwrap();
    task.await.unwrap();
    assert!(idle(&state, "w-a"));
    assert!(state.pro.configuration.try_lock().is_ok());
    install_fixture(&state, "w-a", 5).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn setup_transition_rechecks_commit_authority_and_keeps_its_reservation() {
    for fault in ["none", "expired", "stopped", "generation", "epoch", "local"] {
        let (state, root) = fixture();
        lock(&state.pro.ownership).insert("w-a".into(), Ownership::Hydrating { epoch: 4 });
        let guard = begin_import(&state, "w-a", 4, 0).await.unwrap();
        match fault {
            "expired" => {
                lock(&state.pro.execution.proofs)
                    .get_mut("w-a")
                    .unwrap()
                    .deadline = super::super::lease::Deadline::expired_fixture();
            }
            "stopped" => fence_workspace(&state, "w-a"),
            "generation" => {
                state.pro.generation.fetch_add(1, Ordering::AcqRel);
            }
            "epoch" => {
                lock(&state.pro.ownership).insert("w-a".into(), Ownership::Hydrating { epoch: 5 });
            }
            "local" => {
                lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 4 });
            }
            _ => {}
        }
        let previous = lock(&state.pro.ownership).get("w-a").cloned();
        let result = guard.setting_up(&state);
        if fault == "none" {
            result.unwrap();
            assert!(matches!(
                lock(&state.pro.ownership).get("w-a"),
                Some(Ownership::SettingUp { epoch: 4 })
            ));
            assert!(guard.setting_up(&state).is_err());
            assert!(
                guard.check(&state).is_err(),
                "setup must not admit more import writes"
            );
        } else {
            assert!(result.is_err(), "{fault} cannot publish setup");
            assert!(lock(&state.pro.ownership).get("w-a").cloned() == previous);
        }
        assert!(!idle(&state, "w-a"));
        assert!(state.pro.configuration.try_lock().is_err());
        drop(guard);
        assert!(idle(&state, "w-a"));
        assert!(state.pro.configuration.try_lock().is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn unconfigured_legacy_import_is_still_generation_bound_and_counted() {
    let (state, root) = fixture();
    let guard = begin_import(&state, "w-legacy", 7, 0).await.unwrap();
    guard.check(&state).unwrap();
    assert!(!idle(&state, "w-legacy"));
    state.pro.generation.fetch_add(1, Ordering::AcqRel);
    assert!(guard.check(&state).is_err());
    drop(guard);
    assert!(idle(&state, "w-legacy"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelling_a_caller_keeps_its_blocking_commit_reserved_until_finished() {
    let (state, root) = fixture();
    let owner = state.clone();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (finish, wait) = std::sync::mpsc::channel();
    let task = tokio::task::spawn_blocking(move || {
        let _commit = begin(&owner, "w-a", 4, 0).unwrap();
        entered.send(()).unwrap();
        wait.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
    });
    ready.await.unwrap();
    task.abort();
    fence_workspace(&state, "w-a");
    assert!(!quiescent(&state, "w-a"));
    clear_stopped(&state);
    assert!(install_fixture(&state, "w-a", 5).is_err());
    finish.send(()).unwrap();
    task.await.unwrap();
    install_fixture(&state, "w-a", 5).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reserved_launch_never_waits_on_configuration_that_is_draining_it() {
    let (state, root) = fixture();
    let guard = begin(&state, "w-a", 4, generation(&state)).unwrap();
    let _configuration = state.pro.configuration.lock().await;
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        reserved_request(guard, super::super::prepare_launch(&state, "w-a")),
    )
    .await;
    assert!(result.unwrap().is_err());
    assert!(idle(&state, "w-a"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn queued_real_shell_command_keeps_original_generation_and_epoch() {
    use chimaera_pty::{ExecError, ExecStage, SpawnOpts};
    use std::time::Duration;
    for epoch in [4, 5] {
        let (state, root) = fixture();
        // A worker's queued command keeps its lease epoch; a device's local
        // commands are admitted by ownership alone (laptop first).
        super::super::worker_fixture(&state);
        let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
        install_fixture(&state, &workspace.id, 4).unwrap();
        let session = state
            .sessions
            .spawn(SpawnOpts {
                cwd: root.clone(),
                name: None,
                cols: 80,
                rows: 24,
                command: Some(vec![
                    "/bin/bash".into(),
                    "--norc".into(),
                    "--noprofile".into(),
                ]),
                id: None,
                env: vec![],
                env_remove: vec![],
                scrollback: None,
            })
            .unwrap();
        lock(&state.session_workspaces).insert(session.id.clone(), workspace.id.clone());
        let mut attached = state.sessions.attach_quiet(&session.id).unwrap();
        attached.input.send(bytes::Bytes::from_static(b"printf '\\033]133;C\\007'; printf 'QUEUE-%s\\n' ready; while [ ! -e allow-prompt ]; do sleep 0.01; done; printf '\\033]133;A\\007'\r")).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut output = String::new();
            while !output.contains("QUEUE-ready") {
                output.push_str(&String::from_utf8_lossy(
                    &attached.output.recv().await.unwrap(),
                ));
            }
        })
        .await
        .unwrap();
        let owner = state.clone();
        let id = session.id.clone();
        let pending = tokio::spawn(async move {
            crate::exec::run_exec(
                &owner,
                &id,
                "printf stale > stale-command.txt".into(),
                Some(2000),
                Some(5000),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !matches!(
                lock(&state.exec_status).get(&session.id),
                Some(ExecStage::Queued)
            ) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // Deliberately leave the old shell alive: admission must work even
        // before managed process shutdown gets a scheduling opportunity.
        state.pro.generation.fetch_add(1, Ordering::AcqRel);
        lock(&state.pro.execution.proofs).clear();
        install_fixture(&state, &workspace.id, epoch).unwrap();
        assert!(crate::pro::may_execute(&state, &workspace.id));
        std::fs::write(root.join("allow-prompt"), b"").unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(6), pending)
                .await
                .unwrap()
                .unwrap(),
            Err(ExecError::Busy(_))
        ));
        assert!(!root.join("stale-command.txt").exists());
        state.sessions.kill(&session.id).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while state.sessions.get(&session.id).is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn asynchronous_dispatch_keeps_exact_ownership_account_and_worker_deadline() {
    let (state, root) = fixture();
    let admission = Dispatch::capture(&state, "w-a").unwrap();
    // A laptop's expired publication lease must not stop its own local work.
    lock(&state.pro.execution.proofs)
        .get_mut("w-a")
        .unwrap()
        .deadline = super::super::lease::Deadline::expired_fixture();
    drop(admission.begin(&state).unwrap());
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 5 });
    assert!(admission.begin(&state).is_err());
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::AwaitingVerification { epoch: 5 });
    let awaiting = Dispatch::capture(&state, "w-a").unwrap();
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::AwaitingVerification { epoch: 6 });
    assert!(awaiting.begin(&state).is_err());
    let free = Dispatch::capture(&state, "free").unwrap();
    assert!(free.begin(&state).unwrap().is_none());
    state.pro.generation.fetch_add(1, Ordering::AcqRel);
    assert!(free.begin(&state).unwrap().is_none());
    assert!(admission.begin(&state).is_err());
    install_fixture(&state, "free", 9).unwrap();
    assert!(free.begin(&state).is_err());
    std::fs::remove_dir_all(root).unwrap();
    let (state, root) = fixture();
    super::super::worker_fixture(&state);
    let admission = Dispatch::capture(&state, "w-a").unwrap();
    lock(&state.pro.execution.proofs)
        .get_mut("w-a")
        .unwrap()
        .deadline = super::super::lease::Deadline::expired_fixture();
    assert!(admission.begin(&state).is_err());
    assert!(idle(&state, "w-a"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn imported_resume_retains_exact_account_epoch_but_never_inherits_into_child_tasks() {
    let (state, root) = fixture();
    let current = generation(&state);
    let guard = begin_import(&state, "w-a", 4, current).await.unwrap();
    let dispatch = Dispatch::capture(&state, "w-a").unwrap();
    let commit = guard.into_resume();
    resume_import(commit, dispatch, async {
        assert!(!super::super::quiescent(&state, "w-a"));
        assert!(check_import_resume(&state, "w-a").is_ok());
        assert!(check_import_resume(&state, "unrelated").is_err());
        let child = state.clone();
        // Task-local scope never creates a transferable grant. An unrelated
        // child uses its own ordinary final admission, without this request's scope.
        assert!(tokio::spawn(async move {
            IMPORT_RESUME.try_with(|_| ()).is_err()
                && check_import_resume(&child, "unrelated").is_ok()
        })
        .await
        .unwrap());
        local_dispatch_owner_fixture(&state, "w-a", 5);
        assert!(check_import_resume(&state, "w-a").is_err());
        local_dispatch_owner_fixture(&state, "w-a", 4);
        state.pro.generation.fetch_add(1, Ordering::AcqRel);
        assert!(check_import_resume(&state, "w-a").is_err());
    })
    .await;
    assert!(super::super::quiescent(&state, "w-a"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelled_import_resume_releases_count_only_after_its_owned_future_is_dropped() {
    let (state, root) = fixture();
    let guard = begin_import(&state, "w-a", 4, generation(&state))
        .await
        .unwrap();
    let dispatch = Dispatch::capture(&state, "w-a").unwrap();
    let commit = guard.into_resume();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(resume_import(commit, dispatch, async move {
        let _ = entered.send(());
        std::future::pending::<()>().await;
    }));
    ready.await.unwrap();
    assert!(!super::super::quiescent(&state, "w-a"));
    task.abort();
    assert!(task.await.is_err());
    assert!(super::super::quiescent(&state, "w-a"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn free_dispatch_refuses_same_project_account_enrollment_and_copy_transition() {
    for transition in 0..4 {
        let (state, root) = fixture();
        let captured = Dispatch::capture(&state, "free").unwrap();
        match transition {
            0 => {
                lock(&state.pro.preferences)
                    .entry("free".into())
                    .or_default()
                    .account = Some("https://account.invalid/a-fixture".into());
            }
            1 => {
                lock(&state.pro.execution.latched).insert("free".into());
            }
            2 => {
                lock(&state.pro.ownership).insert("free".into(), Ownership::Local { epoch: 1 });
            }
            _ => {
                lock(&state.pro.preferences)
                    .entry("free".into())
                    .or_default()
                    .copy = Some(crate::pro::project_copy::CopyState {
                    checkpoint: None,
                    pending: None,
                    ready: false,
                    takeover_requested: false,
                    takeover_request: None,
                    owner_epoch: None,
                });
            }
        }
        assert!(captured.begin(&state).is_err(), "transition {transition}");
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn scoped_account_authority_pins_dispatch_before_policy_or_ownership() {
    let (state, root) = fixture();
    let unbound = Dispatch::capture(&state, "free").unwrap();
    assert!(unbound.begin(&state).unwrap().is_none());
    std::fs::create_dir_all(&state.pro.root).unwrap();
    let path = state.pro.root.join("workspace-authority.json");
    std::fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "workspace": {"workspace_id": "free", "revision": 1},
            "account_id": "a-fixture", "endpoint": "http://127.0.0.1:9",
            "root": root, "identity": [1, 1]
        }))
        .unwrap(),
    )
    .unwrap();
    *lock(&state.pro.authority) = crate::pro::authority::Authority::load(&state.pro.root);
    state.pro.configured.store(true, Ordering::Release);
    assert!(lock(&state.pro.authority).restricted());
    assert!(lock(&state.pro.authority).allows("free"));
    assert!(!super::super::managed(&state, "free"));
    assert!(!lock(&state.pro.preferences).contains_key("free"));
    assert!(!lock(&state.pro.ownership).contains_key("free"));
    // The accepted scoped record is already account authority: no old free
    // dispatch may adopt it, even before ownership/policy publication.
    assert!(unbound.begin(&state).is_err());
    let scoped = Dispatch::capture(&state, "free").unwrap();
    assert!(scoped.begin(&state).unwrap().is_none());
    state.pro.generation.fetch_add(1, Ordering::AcqRel);
    assert!(scoped.begin(&state).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn free_import_resume_keeps_original_generation_without_fencing_free_dispatch() {
    let (state, root) = fixture();
    let free = Dispatch::capture(&state, "free").unwrap();
    let imported = begin_import(&state, "free", 1, generation(&state))
        .await
        .unwrap();
    assert!(free.generation.is_none());
    resume_import(imported.into_resume(), free.clone(), async {
        assert!(!quiescent(&state, "free"));
        assert!(check_import_resume(&state, "free").is_ok());
        free.run(async {
            assert!(check_import_resume(&state, "free").is_ok());
            let unrelated = Dispatch::capture(&state, "unrelated").unwrap();
            unrelated
                .run(async {
                    assert!(check_import_resume(&state, "free").is_err());
                    assert!(check_import_resume(&state, "unrelated").is_err());
                })
                .await;
            state.pro.generation.fetch_add(1, Ordering::AcqRel);
            // Unrelated account replacement still leaves ordinary free work alone.
            assert!(free.begin(&state).unwrap().is_none());
            // Nested dispatch cannot discard recovery's original import identity.
            assert!(check_import_resume(&state, "free").is_err());
        })
        .await;
        assert!(check_import_resume(&state, "free").is_err());
        assert!(!quiescent(&state, "free"));
    })
    .await;
    assert!(quiescent(&state, "free"));

    let imported = begin_import(&state, "free", 1, generation(&state))
        .await
        .unwrap();
    let unrelated = Dispatch::capture(&state, "unrelated").unwrap();
    resume_import(imported.into_resume(), unrelated, async {
        assert!(check_import_resume(&state, "free").is_err());
        assert!(check_import_resume(&state, "unrelated").is_err());
        assert!(!quiescent(&state, "free"));
    })
    .await;
    assert!(quiescent(&state, "free"));
    std::fs::remove_dir_all(root).unwrap();
}
