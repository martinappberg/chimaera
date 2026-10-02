use super::super::{execution, mutation, Ownership};
use super::*;
use std::path::PathBuf;

fn fixture() -> (Arc<AppState>, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "chimaera-project-copy-authority-{}",
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
    (state, root)
}
fn checkpoint() -> Checkpoint {
    Checkpoint {
        id: "c-copy".into(),
        sequence: 1,
        source_holder_id: "d-source".into(),
        source_epoch: 4,
        working_tree_oid: "a".repeat(40),
        config_oid: "b".repeat(40),
        handoff_oid: "c".repeat(40),
        continuation: execution::wire::Continuation::Idle,
    }
}
fn enroll(state: &AppState) {
    lock(&state.pro.preferences)
        .entry("w-copy".into())
        .or_default()
        .copy = Some(CopyState {
        checkpoint: Some(checkpoint()),
        pending: Some(checkpoint()),
        ready: false,
        takeover_requested: false,
        takeover_request: None,
        owner_epoch: Some(4),
    });
    lock(&state.pro.legacy_pending).insert("w-copy".into());
}

#[tokio::test]
async fn copy_conflict_report_survives_restart_and_transaction_replay() {
    let (state, root) = fixture();
    enroll(&state);
    let prior = PathBuf::from("notes.md.mine-20261002-0600");
    super::super::report_return(&state, "w-copy", (1, vec![prior.clone()]), &[]);
    super::super::persist(&state).await.unwrap();
    drop(state);
    let restored = Arc::new(AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.clone(),
        root.join("config"),
    ));
    assert_eq!(
        carry_kept(&restored, "w-copy", (0, vec![])),
        (1, vec![prior.clone()])
    );
    let new = PathBuf::from("more.md.mine-20261002-0601");
    let staged = carry_kept(&restored, "w-copy", (1, vec![new.clone()]));
    assert_eq!(staged, (2, vec![prior, new]));
    // An interrupted commit replays the stage, never combines it a second time.
    for _ in 0..2 {
        super::super::report_return(&restored, "w-copy", staged.clone(), &[]);
        super::super::persist(&restored).await.unwrap();
    }
    assert_eq!(lock(&restored.pro.status)["w-copy"].kept_both, Some(2));
    assert_eq!(lock(&restored.pro.status)["w-copy"].kept_paths, staged.1);
    let many = carry_kept(
        &restored,
        "w-copy",
        (
            40,
            (0..32)
                .map(|i| PathBuf::from(format!("{i}.mine-20261002-0602")))
                .collect(),
        ),
    );
    assert_eq!(many.0, 42);
    assert_eq!(many.1.len(), 32);
    assert_eq!(
        carry_kept(&restored, "w-ordinary", (0, vec![])),
        (0, vec![])
    );
    drop(restored);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn copy_fence_survives_signout_and_ordinary_state_corruption() {
    let (state, root) = fixture();
    enroll(&state);
    lock(&state.pro.ownership).insert("w-copy".into(), Ownership::Local { epoch: 4 });
    super::super::persist(&state).await.unwrap();
    assert!(!super::super::may_write(&state, "w-copy"));
    assert!(!super::super::may_execute(&state, "w-copy"));
    assert!(!super::super::may_restore(&state, "w-copy"));
    assert!(mutation::capture(&state, "w-copy").is_err());
    assert!(mutation::begin_launch(&state, "w-copy").is_err());
    assert!(!super::super::may_import(&state, "w-copy", 4));
    assert!(super::super::may_write(&state, "w-free"));
    *lock(&state.pro.runtime) = None;
    let role_path = state.pro.root.join("state.json");
    std::fs::write(&role_path, b"not valid json").unwrap();
    drop(state);
    let reloaded = Arc::new(AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.clone(),
        root.join("config"),
    ));
    assert!(copy_only(&reloaded, "w-copy"));
    assert!(!super::super::may_execute(&reloaded, "w-copy"));
    assert_eq!(
        view(&reloaded, "w-copy").unwrap()["state"],
        "recovery_needed"
    );
    assert!(super::super::may_execute(&reloaded, "w-free"));
    drop(reloaded);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn copy_admission_is_counted_exact_and_excludes_existing_commits() {
    let (state, root) = fixture();
    execution::install_fixture(&state, "w-copy", 4).unwrap();
    let earlier = mutation::begin(&state, "w-copy", 4, 0).unwrap();
    enroll(&state);
    assert!(
        mutation::begin_copy(&state, "w-copy", 0, checkpoint(), root.clone())
            .await
            .is_err()
    );
    drop(earlier);
    let copy = mutation::begin_copy(&state, "w-copy", 0, checkpoint(), root.clone())
        .await
        .unwrap();
    assert!(!execution::quiescent(&state, "w-copy"));
    assert!(copy.check(&state).is_ok());
    assert!(
        copy.check_files(&state).is_err(),
        "no selected inode means no file admission"
    );
    lock(&state.pro.preferences)
        .get_mut("w-copy")
        .unwrap()
        .copy
        .as_mut()
        .unwrap()
        .pending
        .as_mut()
        .unwrap()
        .sequence = 2;
    assert!(copy.check(&state).is_err());
    lock(&state.pro.preferences)
        .get_mut("w-copy")
        .unwrap()
        .copy
        .as_mut()
        .unwrap()
        .pending = Some(checkpoint());
    state
        .pro
        .generation
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    assert!(copy.check(&state).is_err());
    drop(copy);
    assert!(execution::quiescent(&state, "w-copy"));
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn promotion_requires_explicit_intent_and_retires_both_copy_fences() {
    let (state, root) = fixture();
    execution::install_fixture(&state, "w-copy", 4).unwrap();
    enroll(&state);
    lock(&state.pro.ownership).insert("w-copy".into(), Ownership::Hydrating { epoch: 4 });
    super::super::persist(&state).await.unwrap();
    assert!(mutation::begin_import(&state, "w-copy", 4, 0)
        .await
        .is_err());
    lock(&state.pro.preferences)
        .get_mut("w-copy")
        .unwrap()
        .copy
        .as_mut()
        .unwrap()
        .takeover_requested = true;
    let guard = mutation::begin_import(&state, "w-copy", 4, 0)
        .await
        .unwrap();
    assert!(!execution::quiescent(&state, "w-copy"));
    promote(&state, "w-copy", &guard).await.unwrap();
    assert!(!copy_only(&state, "w-copy"));
    assert!(!lock(&state.pro.legacy_pending).contains("w-copy"));
    assert!(matches!(
        lock(&state.pro.ownership).get("w-copy"),
        Some(Ownership::SettingUp { epoch: 4 })
    ));
    assert!(
        !super::super::may_execute(&state, "w-copy"),
        "setup remains a separate admission"
    );
    drop(guard);
    drop(state);
    let reloaded = AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.clone(),
        root.join("config"),
    );
    assert!(!copy_only(&reloaded, "w-copy"));
    drop(reloaded);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn failed_role_persistence_restores_copy_fence_before_releasing_admission() {
    let (state, root) = fixture();
    execution::install_fixture(&state, "w-copy", 4).unwrap();
    enroll(&state);
    lock(&state.pro.preferences)
        .get_mut("w-copy")
        .unwrap()
        .copy
        .as_mut()
        .unwrap()
        .takeover_requested = true;
    lock(&state.pro.ownership).insert("w-copy".into(), Ownership::Hydrating { epoch: 4 });
    super::super::persist(&state).await.unwrap();
    let state_file = state.pro.root.join("state.json");
    std::fs::remove_file(&state_file).unwrap();
    std::fs::create_dir(&state_file).unwrap();
    let guard = mutation::begin_import(&state, "w-copy", 4, 0)
        .await
        .unwrap();
    assert!(promote(&state, "w-copy", &guard).await.is_err());
    assert!(copy_only(&state, "w-copy"));
    assert!(lock(&state.pro.legacy_pending).contains("w-copy"));
    assert!(matches!(
        lock(&state.pro.ownership).get("w-copy"),
        Some(Ownership::Hydrating { epoch: 4 })
    ));
    assert!(!super::super::may_execute(&state, "w-copy"));
    assert!(!execution::quiescent(&state, "w-copy"));
    drop(guard);
    drop(state);
    let reloaded = AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.clone(),
        root.join("config"),
    );
    assert!(copy_only(&reloaded, "w-copy"));
    assert!(!super::super::may_execute(&reloaded, "w-copy"));
    drop(reloaded);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn failed_old_takeover_completion_cannot_clear_a_new_intent() {
    let (state, root) = fixture();
    enroll(&state);
    {
        let mut preferences = lock(&state.pro.preferences);
        let copy = preferences
            .get_mut("w-copy")
            .unwrap()
            .copy
            .as_mut()
            .unwrap();
        copy.takeover_requested = true;
        copy.takeover_request = Some("new-request".into());
    }
    cancel_takeover(&state, "w-copy", 0, "old-request")
        .await
        .unwrap();
    assert!(
        lock(&state.pro.preferences)
            .get("w-copy")
            .unwrap()
            .copy
            .as_ref()
            .unwrap()
            .takeover_requested
    );
    cancel_takeover(&state, "w-copy", 0, "new-request")
        .await
        .unwrap();
    assert!(
        !lock(&state.pro.preferences)
            .get("w-copy")
            .unwrap()
            .copy
            .as_ref()
            .unwrap()
            .takeover_requested
    );
    assert!(copy_only(&state, "w-copy"));
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn pre_start_cleanup_preserves_an_already_active_same_intent() {
    let (state, root) = fixture();
    enroll(&state);
    {
        let mut preferences = lock(&state.pro.preferences);
        let copy = preferences
            .get_mut("w-copy")
            .unwrap()
            .copy
            .as_mut()
            .unwrap();
        copy.takeover_requested = true;
        copy.takeover_request = Some("shared-request".into());
    }
    super::super::moves::pending_fixture(&state, "w-copy");
    cancel_unstarted_takeover(&state, "w-copy", 0, "shared-request")
        .await
        .unwrap();
    assert_eq!(
        lock(&state.pro.preferences)
            .get("w-copy")
            .unwrap()
            .copy
            .as_ref()
            .unwrap()
            .takeover_request
            .as_deref(),
        Some("shared-request")
    );
    // The pull's own settled failure still owns unconditional retirement.
    cancel_takeover(&state, "w-copy", 0, "shared-request")
        .await
        .unwrap();
    assert!(
        !lock(&state.pro.preferences)
            .get("w-copy")
            .unwrap()
            .copy
            .as_ref()
            .unwrap()
            .takeover_requested
    );
    super::super::moves::settle_fixture(&state, "w-copy", super::super::moves::Outcome::Refused);
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn completed_old_launch_cannot_hide_a_live_process_from_final_copy_admission() {
    use chimaera_pty::SpawnOpts;
    let (state, root) = fixture();
    execution::install_fixture(&state, "w-copy", 4).unwrap();
    let previous = mutation::begin_launch(&state, "w-copy").unwrap().unwrap();
    // The copy's early scan can happen before an older admitted launch registers.
    assert!(!live_processes(&state, "w-copy"));
    enroll(&state);
    let session = state
        .sessions
        .spawn(SpawnOpts {
            cwd: root.clone(),
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
    lock(&state.session_workspaces).insert(session.id.clone(), "w-copy".into());
    drop(previous);
    // Even after the old reservation completed, its child prevents any copy
    // installation. Counts alone cannot stand in for the live registry.
    assert!(
        mutation::begin_copy(&state, "w-copy", 0, checkpoint(), root.clone())
            .await
            .is_err()
    );
    state.sessions.kill(&session.id).unwrap();
    lock(&state.session_workspaces).remove(&session.id);
    let guard = mutation::begin_copy(&state, "w-copy", 0, checkpoint(), root.clone())
        .await
        .unwrap();
    lock(&state.pro.preferences)
        .get_mut("w-copy")
        .unwrap()
        .execution_launch_pending = true;
    assert!(
        guard.check(&state).is_err(),
        "unsettled durable launch evidence cannot prove no writers"
    );
    drop(guard);
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}
