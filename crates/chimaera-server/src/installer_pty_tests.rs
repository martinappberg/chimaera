//! Legacy shell-pane installs use the same captured authority and owned cleanup.
use super::*;
use std::{path::PathBuf, time::Duration};

fn fixture() -> (Arc<AppState>, crate::workspaces::Workspace, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "chimaera-installer-pty-{}",
        chimaera_core::generate_token()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let state = Arc::new(AppState::new(
        "fixture".into(),
        "fixture".into(),
        1,
        0,
        root.join("data"),
        root.join("config"),
    ));
    let workspace = crate::workspaces::Workspace {
        id: "project".into(),
        root: root.clone(),
        name: "fixture".into(),
        last_opened_at: 0,
        mastermind: None,
        plugins_on: vec![],
        cloud_internal: false,
        hidden: false,
    };
    (state, workspace, root)
}
async fn done(state: &AppState) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while crate::lock(&state.installs).contains_key(&AgentKind::Codex) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn stale_pty_install_capture_is_refused_before_child_spawn() {
    let (state, workspace, root) = fixture();
    crate::pro::install_execution_fixture(&state, "project", 4).unwrap();
    let captured = crate::pro::mutation::Dispatch::capture(&state, "project").unwrap();
    crate::pro::mutation::local_dispatch_owner_fixture(&state, "project", 5);
    let result = start_install_captured(
        &state,
        AgentKind::Codex,
        &workspace,
        "fixture",
        "echo WRONG".into(),
        captured,
    )
    .await;
    assert!(result.is_err());
    assert!(state.sessions.list().is_empty());
    assert!(crate::lock(&state.installs).is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn pty_installer_authority_loss_stops_before_the_deadline() {
    let (state, workspace, root) = fixture();
    crate::pro::install_execution_fixture(&state, "project", 4).unwrap();
    let marker = root.join("ready");
    let script = format!("echo ready > '{}'; sleep 120", marker.display());
    let id = start_install(&state, AgentKind::Codex, &workspace, "fixture", script)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    crate::pro::mutation::local_dispatch_owner_fixture(&state, "project", 5);
    done(&state).await;
    assert!(state.sessions.get(&id).is_none());
    assert!(crate::lock(&state.install_results)[&AgentKind::Codex].exit_status != Some(0));
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn pty_success_drains_background_children_before_completion() {
    let (state, workspace, root) = fixture();
    let release = root.join("release");
    let late = root.join("late");
    let script = format!("(while [ ! -e '{}' ]; do sleep .02; done; echo escaped > '{}') >/dev/null 2>&1 & echo ready; exit 0", release.display(), late.display());
    let id = start_install(&state, AgentKind::Codex, &workspace, "fixture", script)
        .await
        .unwrap();
    done(&state).await;
    assert!(state.sessions.get(&id).is_none());
    assert_eq!(
        crate::lock(&state.install_results)[&AgentKind::Codex].exit_status,
        Some(0)
    );
    assert!(!late.exists());
    std::fs::write(release, "release after cleanup").unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!late.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn actual_owner_keeps_an_aged_invisible_reservation_and_clears_exact_identity() {
    let (state, workspace, root) = fixture();
    crate::lock(&state.installs).insert(
        AgentKind::Codex,
        (
            "old".into(),
            std::time::Instant::now() - Duration::from_secs(30),
        ),
    );
    crate::lock(&state.install_owners).insert(AgentKind::Codex, "old".into());
    let owner = InstallOwner {
        state: state.clone(),
        kind: AgentKind::Codex,
        id: "old".into(),
    };
    let refused = start_install(
        &state,
        AgentKind::Codex,
        &workspace,
        "fixture",
        "echo WRONG".into(),
    )
    .await;
    assert!(refused.is_err());
    assert!(state.sessions.list().is_empty());
    assert_eq!(crate::lock(&state.installs)[&AgentKind::Codex].0, "old");
    drop(owner);
    assert!(crate::lock(&state.installs).is_empty());
    assert!(crate::lock(&state.install_owners).is_empty());
    let id = start_install(
        &state,
        AgentKind::Codex,
        &workspace,
        "fixture",
        "echo accepted".into(),
    )
    .await
    .unwrap();
    done(&state).await;
    assert_eq!(
        crate::lock(&state.install_results)[&AgentKind::Codex].session_id,
        id
    );
    std::fs::remove_dir_all(root).unwrap();
}
