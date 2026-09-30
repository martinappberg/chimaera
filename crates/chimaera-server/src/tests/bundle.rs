use super::support::*;
use crate::*;
use bundle::{ExportMode, ImportOptions, Origin};

/// A terminal that wandered outside its project (`cd ~`) or into a folder the
/// destination lacks (ignored build output) never fails the move: it opens in
/// the nearest folder that exists inside the project.
#[tokio::test]
async fn a_wandering_terminal_never_fails_a_project_move() {
    let _serial = crate::bundle::TEST_SERIAL.lock().await;
    let source = test_state();
    let root = std::fs::canonicalize(test_dir("wandering-project")).unwrap();
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    let (_, workspace) = request(
        &source,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root":root})),
    )
    .await;
    let workspace_id = workspace["id"].as_str().unwrap().to_owned();
    for (label, polled) in [
        ("outside", std::env::temp_dir()),
        ("ignored", root.join("target/debug")),
    ] {
        let (status, session) = request(
            &source,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({"workspace_id":workspace_id})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{session}");
        let id = session["id"].as_str().unwrap().to_owned();
        lock(&source.current_cwds).insert(id.clone(), polled);
        let path = bundle::export(source.clone(), &id, ExportMode::Snapshot)
            .await
            .unwrap();
        // The destination has the project but not its ignored build folder.
        let destination =
            std::fs::canonicalize(test_dir(&format!("wandering-destination-{label}"))).unwrap();
        let target = test_state();
        let imported = bundle::import(
            target.clone(),
            &path,
            ImportOptions {
                destination_root: Some(destination.clone()),
                defer_start: true,
                fork: false,
                origin: Origin::Moved,
                epoch: 1,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("{label}: {error:#}"));
        let entry = lock(&target.deferred_sessions)[&imported.id].clone();
        assert_eq!(entry.cwd, destination, "{label}");
        source.sessions.kill(&id).ok();
    }
}

#[tokio::test]
async fn bundle_preserves_shell_identity_and_defers_moved_terminal() {
    let _serial = crate::bundle::TEST_SERIAL.lock().await;
    let source = test_state();
    let target = test_state();
    let root = std::fs::canonicalize(test_dir("bundle-project")).unwrap();
    let (_, workspace) = request(
        &source,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root":root})),
    )
    .await;
    let workspace_id = workspace["id"].as_str().unwrap();
    let (status,session)=request(&source,Method::POST,"/api/v1/sessions",Some(serde_json::json!({"workspace_id":workspace_id,"name":"pinned shell","cols":101,"rows":31}))).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let id = session["id"].as_str().unwrap();
    let path = bundle::export(source.clone(), id, ExportMode::Stop)
        .await
        .unwrap();
    assert!(
        source.sessions.get(id).unwrap().alive,
        "plain terminal stays on laptop"
    );
    // The registering daemon recorded the id in the folder; a folder that
    // lost it gets it back when a session import registers the workspace.
    std::fs::remove_file(root.join(".chimaera-workspace")).unwrap();
    let imported = bundle::import(
        target.clone(),
        &path,
        ImportOptions {
            destination_root: None,
            defer_start: false,
            fork: false,
            origin: Origin::Moved,
            epoch: 1,
        },
    )
    .await
    .unwrap();
    assert_eq!(imported.id, id);
    assert_eq!(imported.workspace_id, workspace_id);
    assert_eq!(
        crate::workspaces::identity::read(&root).unwrap().id,
        workspace_id
    );
    assert!(imported.paused);
    assert!(target.sessions.get(id).is_none());
    let suspended = ledger::snapshot(&target).0;
    assert_eq!(suspended.len(), 1);
    assert!(suspended[0].suspended);
    let row = session_view::sessions_json(&target)
        .into_iter()
        .find(|row| row["id"] == id)
        .unwrap();
    assert_eq!(row["suspended"], true);
    assert_eq!(row["alive"], false);
    ledger::resume_deferred_workspace(&target, workspace_id)
        .await
        .unwrap();
    assert!(
        target.sessions.get(id).is_none(),
        "moved shells remain on the source even after ownership grant"
    );
    let imported = bundle::import(
        target.clone(),
        &path,
        ImportOptions {
            destination_root: None,
            defer_start: false,
            fork: false,
            origin: Origin::Home,
            epoch: 2,
        },
    )
    .await
    .unwrap();
    assert!(!imported.paused);
    let restored = target.sessions.get(id).unwrap();
    assert_eq!(restored.id, id);
    assert_eq!(restored.name, "pinned shell");
    assert_eq!(restored.cwd, root);
    assert_eq!((restored.cols, restored.rows), (101, 31));
    bundle::import(
        target.clone(),
        &path,
        ImportOptions {
            destination_root: None,
            defer_start: false,
            fork: false,
            origin: Origin::Home,
            epoch: 2,
        },
    )
    .await
    .expect("exact successful retry is idempotent");
    assert!(
        bundle::import(
            target.clone(),
            &path,
            ImportOptions {
                destination_root: None,
                defer_start: false,
                fork: false,
                origin: Origin::Home,
                epoch: 3
            }
        )
        .await
        .is_err(),
        "different ownership operation cannot replace a live process"
    );
    source.sessions.kill(id).unwrap();
    target.sessions.kill(id).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn suspended_ledger_never_respawns_until_verified_resume() {
    let state = test_state();
    let root = std::fs::canonicalize(test_dir("deferred-root")).unwrap();
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    let entry = ledger::LedgerEntry {
        id: "s-deferred".into(),
        suspended: true,
        handoff: None,
        workspace_id: workspace.id.clone(),
        cwd: root,
        pinned_name: None,
        cols: 80,
        rows: 24,
        theme: "dark".into(),
        created_at: 1,
        agent: None,
    };
    ledger::restore(
        &state,
        ledger::BootLedger {
            sessions: vec![entry],
            ..Default::default()
        },
    )
    .await;
    assert!(state.sessions.list().is_empty());
    assert_eq!(ledger::snapshot(&state).0.len(), 1);
    ledger::resume_deferred_workspace(&state, &workspace.id)
        .await
        .unwrap();
    assert!(state.sessions.get("s-deferred").is_some());
    assert!(lock(&state.deferred_sessions).is_empty());
    state.sessions.kill("s-deferred").unwrap();
}

#[tokio::test]
async fn destination_remap_preserves_relative_cwd_and_stages_before_resume() {
    let _serial = crate::bundle::TEST_SERIAL.lock().await;
    let source = test_state();
    let target = test_state();
    let root = std::fs::canonicalize(test_dir("remap-source")).unwrap();
    let destination = std::fs::canonicalize(test_dir("remap-target")).unwrap();
    let (_, workspace) = request(
        &source,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root":root})),
    )
    .await;
    let (_, session) = request(
        &source,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id":workspace["id"],"name":"remapped shell"})),
    )
    .await;
    let id = session["id"].as_str().unwrap();
    let archive = bundle::export(source.clone(), id, ExportMode::Snapshot)
        .await
        .unwrap();
    let result = bundle::import(
        target.clone(),
        &archive,
        ImportOptions {
            fork: false,
            origin: Origin::Home,
            epoch: 1,
            defer_start: true,
            destination_root: Some(destination.clone()),
        },
    )
    .await
    .unwrap();
    assert!(result.paused);
    assert!(target.sessions.get(id).is_none());
    assert_eq!(
        lock(&target.workspaces)
            .get(workspace["id"].as_str().unwrap())
            .unwrap()
            .root,
        destination
    );
    ledger::resume_deferred_workspace(&target, workspace["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(target.sessions.get(id).unwrap().cwd, destination);
    assert!(lock(&target.deferred_sessions).get(id).is_none());
    source.sessions.kill(id).unwrap();
    target.sessions.kill(id).unwrap();
    std::fs::remove_file(archive).unwrap();
}
