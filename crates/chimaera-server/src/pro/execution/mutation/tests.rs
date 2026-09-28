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
