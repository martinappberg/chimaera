use super::support::*;
use crate::*;
use bundle::{ExportMode, ImportOptions, Origin};
use serde_json::json;

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

fn public_fixture(root: &std::path::Path, index: &[u8], view: Option<&[u8]>) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let path = test_dir("public-bundle-archive").join("source.zip");
    let mut members = vec![
        (
            "native.jsonl",
            b"{\"type\":\"user\",\"message\":\"fixture\"}\n".as_slice(),
        ),
        (
            "journal.jsonl",
            b"{\"seq\":1,\"ts\":1,\"ev\":{\"type\":\"notice\",\"text\":\"incoming\"}}\n".as_slice(),
        ),
        ("index.json", index),
    ];
    if let Some(view) = view {
        members.push(("view.json", view));
    }
    let mut checksums = serde_json::Map::new();
    for (name, bytes) in &members {
        let digest: String = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        checksums.insert((*name).into(), json!({"bytes":bytes.len(),"sha256":digest}));
    }
    let manifest = json!({"version":1,"workspace":{"id":"w-public","root":root,"name":"public","last_opened_at":0},
        "session":{"id":"s-public","workspace_id":"w-public","cwd":root,"cols":80,"rows":24,
            "agent":{"kind":"claude","resume":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb","title":"fixture","ui":"chat"}},
        "source_host":"fixture","source_os":"test","stopped":true,"members":checksums});
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("manifest.json", options).unwrap();
    zip.write_all(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    for (name, bytes) in members {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    path
}
fn public_options(root: &std::path::Path) -> ImportOptions {
    ImportOptions {
        fork: false,
        origin: Origin::Home,
        epoch: 5,
        defer_start: true,
        destination_root: Some(root.to_owned()),
    }
}

#[tokio::test]
async fn public_bundle_validates_all_metadata_before_canonical_writes() {
    let _serial = bundle::TEST_SERIAL.lock().await;
    for (index, view) in [
        (b"broken".as_slice(), None),
        (b"{}".as_slice(), Some(b"broken".as_slice())),
    ] {
        let state = test_state();
        let project = std::fs::canonicalize(test_dir("public-bad-metadata")).unwrap();
        let archive = public_fixture(&project, index, view);
        let journal = state.chat.journal_dir().join("s-public.jsonl");
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        std::fs::write(&journal, b"original").unwrap();
        assert!(
            bundle::import(state.clone(), &archive, public_options(&project))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&journal).unwrap(), b"original");
        assert!(lock(&state.workspaces).get("w-public").is_none());
        assert!(!project.join(".chimaera-workspace").exists());
        assert!(crate::pro::may_execute(&state, "w-public"));
    }
}

#[tokio::test]
async fn public_bundle_failed_metadata_is_fenced_across_restart_and_exact_retry() {
    let _serial = bundle::TEST_SERIAL.lock().await;
    let data = test_dir("public-recover-data");
    let state = test_state_with_data_dir(0, data.clone());
    let project = std::fs::canonicalize(test_dir("public-recover-project")).unwrap();
    let archive = public_fixture(&project, b"{\"model\":\"fixture\"}", None);
    let journal = state.chat.journal_dir().join("s-public.jsonl");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(&journal, b"original local history").unwrap();
    let index = state.chat.journal_dir().join("index.json");
    std::fs::create_dir(&index).unwrap();
    let response = bundle::import_route(
        axum::extract::State(state.clone()),
        axum::extract::Query(public_options(&project)),
        axum::body::Body::from(std::fs::read(&archive).unwrap()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let error: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error["error_code"], "bundle_import_pending");
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("Retry the same archive"));
    let (status, status_body) = request(&state, Method::GET, "/api/v1/pro/status", None).await;
    assert!(status.is_success());
    let row = status_body["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["workspace_id"] == "w-public")
        .unwrap();
    assert_eq!(row["bundle_import"]["state"], "recovery_needed");
    assert!(!crate::pro::may_execute(&state, "w-public"));
    assert!(state
        .bundle_imports
        .check_session("s-public", None)
        .is_err());
    assert!(state.chat.get("s-public").is_none());
    let before = state
        .chat
        .journal_dir()
        .parent()
        .unwrap()
        .join("bundles/imports/prepared/s-public/stage/1.before");
    assert_eq!(std::fs::read(before).unwrap(), b"original local history");
    drop(state);
    std::fs::remove_dir(&index).unwrap();
    let restarted = test_state_with_data_dir(0, data);
    assert!(!crate::pro::may_execute(&restarted, "w-public"));
    let mut different = public_options(&project);
    different.epoch += 1;
    assert!(bundle::import(restarted.clone(), &archive, different)
        .await
        .is_err());
    let imported = bundle::import(restarted.clone(), &archive, public_options(&project))
        .await
        .unwrap();
    assert!(imported.paused);
    assert!(restarted.chat.get("s-public").is_none());
    assert!(crate::pro::may_execute(&restarted, "w-public"));
    assert_eq!(
        crate::workspaces::identity::read(&project).unwrap().id,
        "w-public"
    );
    assert!(lock(&restarted.deferred_sessions).contains_key("s-public"));
    // Exit/replacement is irrelevant to a positive committed receipt. Preserve
    // native/journal progress made after the completed import on exact replay.
    std::fs::write(&journal, b"newer canonical history").unwrap();
    bundle::import(restarted.clone(), &archive, public_options(&project))
        .await
        .unwrap();
    assert_eq!(std::fs::read(journal).unwrap(), b"newer canonical history");
}

#[tokio::test]
async fn malformed_pending_import_record_blocks_restoration_and_new_workspace_launch() {
    let data = test_dir("public-damaged-pending");
    std::fs::create_dir(data.join("bundles")).unwrap();
    std::fs::write(data.join("bundles/pending.json"), b"broken").unwrap();
    let state = test_state_with_data_dir(0, data);
    assert!(!crate::pro::may_execute(&state, "unrelated-free-project"));
    assert!(state.bundle_imports.admit("w-new", "s-new", None).is_err());
    assert_eq!(state.bundle_imports.view("w-new").unwrap()["damaged"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_bundle_caller_cancellation_retains_config_and_pending_until_owned_commit() {
    let _serial = bundle::TEST_SERIAL.lock().await;
    let state = test_state();
    let project = std::fs::canonicalize(test_dir("public-cancel-project")).unwrap();
    let archive = public_fixture(&project, b"{}", None);
    let (entered, release) = bundle::hold_public(&state, "s-public", false);
    let importer = state.clone();
    let options = public_options(&project);
    let call = tokio::spawn(async move { bundle::import(importer, &archive, options).await });
    entered.await.unwrap();
    call.abort();
    assert!(!crate::pro::may_execute(&state, "w-public"));
    let config = state.clone();
    let mut disable = tokio::spawn(async move {
        request(&config, Method::DELETE, "/api/v1/pro/configure", None).await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut disable)
            .await
            .is_err(),
        "caller cancellation cannot release configuration before owned metadata writes settle"
    );
    release.send(()).unwrap();
    assert!(disable.await.unwrap().0.is_success());
    assert!(!state.bundle_imports.blocks_workspace("w-public"));
    assert!(lock(&state.deferred_sessions).contains_key("s-public"));
    assert!(state.chat.get("s-public").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_bundle_resume_refuses_account_replacement_after_async_preparation() {
    let _serial = bundle::TEST_SERIAL.lock().await;
    let source = test_state();
    let target = test_state();
    let project = std::fs::canonicalize(test_dir("public-resume-account")).unwrap();
    let (_, workspace) = request(
        &source,
        Method::POST,
        "/api/v1/workspaces",
        Some(json!({"root":project})),
    )
    .await;
    let (_, session) = request(
        &source,
        Method::POST,
        "/api/v1/sessions",
        Some(json!({"workspace_id":workspace["id"]})),
    )
    .await;
    let id = session["id"].as_str().unwrap().to_owned();
    let archive = bundle::export(source.clone(), &id, ExportMode::Snapshot)
        .await
        .unwrap();
    let (entered, release) = bundle::hold_public(&target, &id, true);
    let importer = target.clone();
    let call = tokio::spawn(async move {
        bundle::import(
            importer,
            &archive,
            ImportOptions {
                fork: false,
                origin: Origin::Home,
                epoch: 1,
                defer_start: false,
                destination_root: None,
            },
        )
        .await
    });
    entered.await.unwrap();
    // Local disconnect keeps laptop processes intact, but still replaces the
    // account generation. Final actual spawn must retain the old admission.
    assert!(
        request(&target, Method::DELETE, "/api/v1/pro/configure", None)
            .await
            .0
            .is_success()
    );
    release.send(()).unwrap();
    assert!(call.await.unwrap().is_err());
    assert!(target.sessions.get(&id).is_none());
    assert!(lock(&target.deferred_sessions).contains_key(&id));
    source.sessions.kill(&id).unwrap();
}

#[tokio::test]
async fn public_bundle_view_failure_retry_flushes_the_existing_cached_layout() {
    let _serial = bundle::TEST_SERIAL.lock().await;
    let data = test_dir("public-view-failure");
    let state = test_state_with_data_dir(0, data.clone());
    let project = std::fs::canonicalize(test_dir("public-view-project")).unwrap();
    let archive = public_fixture(&project, b"{}", Some(b"{\"layout\":\"incoming\"}"));
    let view = data.join("view-state.json");
    std::fs::create_dir(&view).unwrap();
    assert!(
        bundle::import(state.clone(), &archive, public_options(&project))
            .await
            .is_err()
    );
    assert!(lock(&state.view_state).get("ws_w-public").is_some());
    assert!(!crate::pro::may_execute(&state, "w-public"));
    std::fs::remove_dir(&view).unwrap();
    bundle::import(state.clone(), &archive, public_options(&project))
        .await
        .unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(view).unwrap()).unwrap();
    assert_eq!(saved["ws_w-public"]["layout"], "incoming");
    assert!(crate::pro::may_execute(&state, "w-public"));
}

#[tokio::test]
async fn public_bundle_cannot_replace_a_live_resumed_chat_before_its_native_init() {
    use chimaera_agent::driver::{AgentAdapter, DriverExit, DriverIo, SpawnSpec};
    struct Quiet;
    impl AgentAdapter for Quiet {
        fn kind(&self) -> &'static str {
            "claude"
        }
        fn spawn(
            &self,
            _spec: SpawnSpec,
            mut io: DriverIo,
        ) -> anyhow::Result<tokio::task::JoinHandle<DriverExit>> {
            Ok(tokio::spawn(async move {
                let events = io.events;
                let commands = io.commands;
                let _ = io.kill.changed().await;
                drop(events);
                drop(commands);
                DriverExit::Killed
            }))
        }
    }
    let _serial = bundle::TEST_SERIAL.lock().await;
    let state = test_state();
    let project = std::fs::canonicalize(test_dir("public-live-resume")).unwrap();
    let archive = public_fixture(&project, b"{}", None);
    let id = "s-already-resuming";
    lock(&state.chat_recipes).insert(
        id.into(),
        crate::chat::ChatRecipe {
            workspace_root: project.clone(),
            workspace_id: "w-old".into(),
            kind: crate::agents::AgentKind::Claude,
            bin: "/bin/false".into(),
            version: None,
            settings: None,
            mcp_config: None,
            model: None,
            resume: Some("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into()),
            fork_at: None,
            fork_head: false,
            rollback_turns: None,
            revert_before_turn: None,
            remote_control: crate::chat::RemoteControlAtStart::No,
            carry_ultracode: false,
            theme: "dark".into(),
            prelude: None,
            mastermind: None,
            portable_context: None,
            created_at_ms: None,
        },
    );
    state
        .chat
        .spawn(&Quiet, SpawnSpec::new(id, vec![], project.clone()))
        .unwrap();
    assert!(state.chat.get(id).unwrap().native_session_id.is_none());
    assert!(state.chat.get(id).unwrap().alive);
    let error = bundle::import(state.clone(), &archive, public_options(&project))
        .await
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("native conversation is active"),
        "{error}"
    );
    assert!(!state.chat.journal_dir().join("s-public.jsonl").exists());
    assert!(crate::pro::may_execute(&state, "w-public"));
    state.chat.fence(id);
}
