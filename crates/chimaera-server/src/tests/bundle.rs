use super::support::*;
use crate::*;
use bundle::{ExportMode, ImportOptions, Origin};

/// A metadata failure after file installation is retried from immutable
/// preparation, including evidence held only by the old local journal.
#[tokio::test]
async fn prepared_bundle_recovers_metadata_after_restart_without_starting_an_agent() {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let _serial = crate::bundle::TEST_SERIAL.lock().await;
    let data = test_dir("prepared-bundle-data");
    let target = test_state_with_data_dir(0, data.clone());
    let destination = std::fs::canonicalize(test_dir("prepared-bundle-project")).unwrap();
    let stage = test_dir("prepared-bundle-stage");
    let archive = stage.join("source.zip");
    let journal = target.chat.journal_dir().join("s-prepared.jsonl");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let old = b"{\"seq\":1,\"ts\":1,\"ev\":{\"type\":\"user_message\",\"id\":\"u-old\",\"text\":\"local old echo\",\"client_id\":\"client-local-old\"}}\n";
    std::fs::write(&journal, old).unwrap();
    let incoming = b"{\"seq\":2,\"ts\":2,\"ev\":{\"type\":\"user_message\",\"id\":\"u-new\",\"text\":\"remote echo\",\"client_id\":\"client-remote-new\"}}\n";
    let evidence = br#"{"version":1,"session_id":"s-prepared","entries":[{"id":"client-crash-gap","state":"dispatching"}]}"#;
    let native_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let members: [(&str, &[u8]); 4] = [
        (
            "native.jsonl",
            b"{\"type\":\"user\",\"message\":\"fixture\"}\n",
        ),
        ("journal.jsonl", incoming),
        ("send-state.json", evidence),
        ("index.json", br#"{"model":"fixture-model"}"#),
    ];
    let mut checksums = serde_json::Map::new();
    for (name, bytes) in members {
        let digest: String = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        checksums.insert(
            name.into(),
            serde_json::json!({"bytes":bytes.len(),"sha256":digest}),
        );
    }
    let manifest = serde_json::json!({"version":1,"workspace":{"id":"w-prepared","root":destination,"name":"prepared","last_opened_at":0},
        "session":{"id":"s-prepared","workspace_id":"w-prepared","cwd":destination,"cols":80,"rows":24,
            "agent":{"kind":"claude","resume":native_id,"title":"fixture","ui":"chat"}},
        "source_host":"fixture","source_os":"test","stopped":true,"members":checksums});
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
    let format = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o600);
    zip.start_file("manifest.json", format).unwrap();
    zip.write_all(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    for (name, bytes) in members {
        zip.start_file(name, format).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let options = ImportOptions {
        fork: false,
        origin: Origin::Home,
        epoch: 7,
        defer_start: true,
        destination_root: Some(destination.clone()),
    };
    let prepared = bundle::prepare_import(
        target.clone(),
        &archive,
        options.clone(),
        &stage.join("prepared"),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(&journal).unwrap(),
        old,
        "preparation never mutates destination files"
    );
    assert!(lock(&target.deferred_sessions).is_empty());
    // The file transaction has separate overwrite/crash/CAS tests. Apply its
    // exposed file intents here to exercise the independent metadata phase.
    for write in &prepared.writes {
        let path = write.root.join(&write.relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::copy(write.after.as_ref().unwrap(), path).unwrap();
    }
    let index = target.chat.journal_dir().join("index.json");
    std::fs::create_dir(&index).unwrap();
    assert!(
        prepared.finalize().await.is_err(),
        "failed index persistence must fail the import"
    );
    assert!(lock(&target.deferred_sessions).is_empty());
    assert!(target.chat.get("s-prepared").is_none());
    let stale = bundle::prepare_import(
        target.clone(),
        &archive,
        options.clone(),
        &stage.join("prepared"),
    )
    .await
    .unwrap();
    let (status, _) = request(
        &target,
        axum::http::Method::DELETE,
        "/api/v1/pro/configure",
        None,
    )
    .await;
    assert!(status.is_success());
    assert!(
        stale.finalize().await.is_err(),
        "a prepared session cannot cross account generations"
    );
    assert!(lock(&target.deferred_sessions).is_empty());
    drop(target);
    std::fs::remove_dir(&index).unwrap();
    let restarted = test_state_with_data_dir(0, data);
    let mut prepared = bundle::prepare_import(
        restarted.clone(),
        &archive,
        options,
        &stage.join("prepared"),
    )
    .await
    .unwrap();
    let old_image = prepared
        .writes
        .iter()
        .find(|write| write.relative.file_name().unwrap() == "s-prepared.jsonl")
        .unwrap();
    assert_eq!(
        std::fs::read(old_image.before.as_ref().unwrap()).unwrap(),
        old,
        "retry retained the original before image"
    );
    let (entered, resume) = prepared.hold_finalization();
    let caller = tokio::spawn(prepared.finalize());
    entered.await.unwrap();
    caller.abort();
    assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
    let next_state = restarted.clone();
    let mut next_account = tokio::spawn(async move {
        crate::pro::mutation::begin_import(
            &next_state,
            "w-prepared",
            7,
            crate::pro::mutation::generation(&next_state),
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut next_account)
            .await
            .is_err(),
        "caller cancellation must not release the account fence of an owned finalizer"
    );
    resume.send(()).unwrap();
    drop(
        tokio::time::timeout(std::time::Duration::from_secs(5), next_account)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
    assert!(restarted.chat.get("s-prepared").is_none());
    assert!(lock(&restarted.deferred_sessions).contains_key("s-prepared"));
    assert_eq!(
        restarted.chat.index().settings(native_id).model.as_deref(),
        Some("fixture-model")
    );
    let evidence: serde_json::Value = serde_json::from_slice(
        &chimaera_agent::journal::export_send_state(restarted.chat.journal_dir(), "s-prepared")
            .unwrap(),
    )
    .unwrap();
    assert!(evidence["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["id"] == "client-local-old"));
    assert!(evidence["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["id"] == "client-crash-gap" && entry["state"] == "dispatching"));
}

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
