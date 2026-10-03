use super::super::{install_fixture, mutation, quiescent, Ownership};
use super::*;
use crate::lock;
use std::os::unix::process::CommandExt;

fn fixture() -> (Arc<AppState>, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "chimaera-installer-{}",
        chimaera_core::generate_token()
    ));
    std::fs::create_dir_all(&root).unwrap();
    (
        Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            1,
            0,
            root.join("data"),
            root.join("config"),
        )),
        root,
    )
}
#[tokio::test]
async fn installer_keeps_original_authority_through_configuration_wait() {
    let (state, root) = fixture();
    install_fixture(&state, "project", 4).unwrap();
    let captured = mutation::Dispatch::capture(&state, "project").unwrap();
    let configuration = state.pro.configuration.clone().lock_owned().await;
    let owned = state.clone();
    let task = tokio::spawn(async move { Running::begin(&owned, "project", captured).await });
    lock(&state.pro.ownership).insert(
        "project".into(),
        Ownership::Remote {
            holder: "other".into(),
            epoch: 5,
        },
    );
    drop(configuration);
    assert!(task.await.unwrap().is_err());
    assert!(lock(&state.pro.execution.setups).is_empty());
    assert!(mutation::idle(&state, "project"));
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn abandoned_installer_cleans_actual_group_and_durable_pending_marker() {
    let (state, root) = fixture();
    install_fixture(&state, "project", 4).unwrap();
    let captured = mutation::Dispatch::capture(&state, "project").unwrap();
    let mut running = Running::begin(&state, "project", captured).await.unwrap();
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    running.attach(child.id());
    assert!(!quiescent(&state, "project"));
    assert!(lock(&state.pro.preferences)["project"].execution_launch_pending);
    let restored =
        super::super::State::restore(&state.pro.root, &lock(&state.pro.preferences), true, false);
    assert!(lock(&restored.unclean).contains_key("project"));
    drop(running);
    tokio::task::spawn_blocking(move || child.wait())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !mutation::idle(&state, "project") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(!lock(&state.pro.preferences)["project"].execution_launch_pending);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn free_installer_does_not_create_pro_state_and_unspawned_managed_cleans_pending() {
    let (state, root) = fixture();
    let free = Running::begin(
        &state,
        "free",
        mutation::Dispatch::capture(&state, "free").unwrap(),
    )
    .await
    .unwrap();
    free.finish().await.unwrap();
    assert!(lock(&state.pro.preferences).is_empty());
    assert!(lock(&state.pro.execution.setups).is_empty());
    install_fixture(&state, "project", 4).unwrap();
    let managed = Running::begin(
        &state,
        "project",
        mutation::Dispatch::capture(&state, "project").unwrap(),
    )
    .await
    .unwrap();
    assert!(lock(&state.pro.preferences)["project"].execution_launch_pending);
    managed.finish().await.unwrap();
    assert!(!lock(&state.pro.preferences)["project"].execution_launch_pending);
    assert!(mutation::idle(&state, "project"));
    std::fs::remove_dir_all(root).unwrap();
}
