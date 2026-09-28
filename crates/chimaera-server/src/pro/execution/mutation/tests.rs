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
