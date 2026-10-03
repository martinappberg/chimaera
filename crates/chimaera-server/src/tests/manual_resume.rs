//! Real local driver/child/journal/REST path with a synthetic protocol.
//! This does not certify the pinned real agent CLI's idle/resume behavior.
use super::support::*;
use crate::*;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

const NATIVE: &str = "11111111-2222-4333-8444-555555555555";

fn fixture() -> (Arc<AppState>, ledger::LedgerEntry, PathBuf) {
    let mut state = test_state();
    let root = test_dir("manual-resume-project");
    let isolated = Arc::get_mut(&mut state).unwrap();
    isolated.managed_root = root.join("managed-runtime");
    isolated.legacy_managed_root = None;
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    let gate = root.join("ready");
    let bin = root.join("synthetic-claude");
    std::fs::write(&bin, format!(
        "#!/bin/sh\n\
         printf '%s\\n' '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"init\",\"response\":{{\"commands\":[]}}}}}}'\n\
         while [ ! -f '{}' ]; do sleep 0.02; done\n\
         printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"{NATIVE}\",\"model\":\"synthetic\",\"permissionMode\":\"default\",\"slash_commands\":[]}}'\n\
         cat >/dev/null\n", gate.display())).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    lock(&state.agent_bins).insert(
        agents::AgentKind::Claude,
        launcher::AgentDetection {
            path: Ok(bin),
            version: Some(chimaera_agent::claude::TESTED_CLAUDE_VERSION.into()),
            managed: false,
            explicit: true,
            mtime: None,
        },
    );
    let transcript = state
        .claude_projects_dir
        .join(launcher::encode_cwd(&root))
        .join(format!("{NATIVE}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, format!("{{\"type\":\"summary\",\"sessionId\":\"{NATIVE}\",\"summary\":\"retained synthetic conversation\"}}\n")).unwrap();
    let entry = ledger::LedgerEntry {
        id: "s-manual-resume".into(),
        suspended: true,
        manual_resume_reason: Some("project_secrets_idle".into()),
        handoff: None,
        workspace_id: workspace.id,
        cwd: root,
        pinned_name: Some("original conversation".into()),
        cols: 80,
        rows: 24,
        theme: "dark".into(),
        created_at: 12,
        agent: Some(ledger::LedgerAgent {
            kind: agents::AgentKind::Claude,
            resume: Some(NATIVE.into()),
            transcript: Some(transcript),
            native_cwd: None,
            title: "retained synthetic conversation".into(),
            ui: chimaera_agent::model::SessionUi::Chat,
            model: None,
            carryover: None,
        }),
    };
    lock(&state.deferred_sessions).insert(entry.id.clone(), entry.clone());
    lock(&state.session_workspaces).insert(entry.id.clone(), entry.workspace_id.clone());
    (state, entry, gate)
}
async fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "synthetic resume condition timed out"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_resume_same_id_survives_observer_loss_and_duplicate_never_respawns() {
    let (state, entry, gate) = fixture();
    let owner = state.clone();
    let id = entry.id.clone();
    let mut observer = tokio::spawn(async move {
        request(
            &owner,
            Method::POST,
            &format!("/api/v1/sessions/{id}/resume"),
            None,
        )
        .await
    });
    tokio::select! {
        outcome = &mut observer => panic!("manual synthetic spawn refused: {outcome:?}"),
        _ = wait(|| state.chat.get(&entry.id).is_some_and(|info| info.alive)) => (),
    }
    assert!(!ws::session_writable(&state, &entry.id));
    assert!(!state
        .chat
        .resumed_native_ready(&entry.id, NATIVE)
        .await
        .unwrap());
    assert!(lock(&state.deferred_sessions).contains_key(&entry.id));
    observer.abort();
    let _ = observer.await;
    std::fs::write(gate, b"release").unwrap();
    wait(|| !lock(&state.deferred_sessions).contains_key(&entry.id)).await;
    wait(|| !lock(&state.chat_switching).contains_key(&entry.id)).await;
    assert!(state
        .chat
        .resumed_native_ready(&entry.id, NATIVE)
        .await
        .unwrap());
    assert!(ws::session_writable(&state, &entry.id));
    let before = state.chat.get(&entry.id).unwrap().created_at_ms;
    let process = state.chat.process_group(&entry.id).unwrap();
    let head = state.chat.attach(&entry.id, 0).unwrap().head_seq;
    let (status, row) = request(
        &state,
        Method::POST,
        &format!("/api/v1/sessions/{}/resume", entry.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    assert_eq!(row["id"], entry.id);
    assert_eq!(state.chat.get(&entry.id).unwrap().created_at_ms, before);
    assert_eq!(state.chat.process_group(&entry.id), Some(process));
    assert_eq!(state.chat.attach(&entry.id, 0).unwrap().head_seq, head);
    assert!(ledger::manual::load(&state).unwrap().contains(&entry));
    assert!(lock(&state.ledger)
        .load_boot()
        .sessions
        .iter()
        .any(|row| row.id == entry.id
            && row.manual_resume_reason.is_none()
            && row.agent.as_ref().unwrap().resume.as_deref() == Some(NATIVE)));
    state.chat.fence(&entry.id);
    wait(|| state.chat.process_group(&entry.id).is_none()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_resume_lost_original_authority_reaps_and_keeps_manual_ledger() {
    let (state, entry, gate) = fixture();
    pro::mutation::local_dispatch_owner_fixture(&state, &entry.workspace_id, 1);
    let owner = state.clone();
    let id = entry.id.clone();
    let mut observer = tokio::spawn(async move {
        request(
            &owner,
            Method::POST,
            &format!("/api/v1/sessions/{id}/resume"),
            None,
        )
        .await
    });
    tokio::select! {
        outcome = &mut observer => panic!("manual synthetic spawn refused: {outcome:?}"),
        _ = wait(|| state.chat.get(&entry.id).is_some_and(|info| info.alive)) => (),
    }
    pro::mutation::local_dispatch_owner_fixture(&state, &entry.workspace_id, 2);
    std::fs::write(gate, b"release").unwrap();
    let (status, _) = tokio::time::timeout(Duration::from_secs(10), observer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(lock(&state.deferred_sessions)
        .get(&entry.id)
        .unwrap()
        .manual_resume_reason
        .is_some());
    assert!(!ws::session_writable(&state, &entry.id));
    assert!(state.chat.process_group(&entry.id).is_none());
    assert!(lock(&state.ledger)
        .load_boot()
        .sessions
        .iter()
        .any(|row| row.id == entry.id && row.manual_resume_reason.is_some()));
}

#[tokio::test]
async fn manual_resume_refuses_unknown_reason_body_missing_and_scoped_foreign_session() {
    let (state, mut entry, _) = fixture();
    let uri = format!("/api/v1/sessions/{}/resume", entry.id);
    entry.manual_resume_reason = Some("unknown".into());
    lock(&state.deferred_sessions).insert(entry.id.clone(), entry.clone());
    assert_eq!(
        request(&state, Method::POST, &uri, None).await.0,
        StatusCode::CONFLICT
    );
    entry.manual_resume_reason = Some("project_secrets_idle".into());
    lock(&state.deferred_sessions).insert(entry.id.clone(), entry.clone());
    assert_eq!(
        request(&state, Method::POST, &uri, Some(serde_json::json!({})))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &state,
            Method::POST,
            "/api/v1/sessions/s-absent/resume",
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let response = app(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(&uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        ledger::check_manual_native(&state, None, agents::AgentKind::Claude, Some(NATIVE)).is_err()
    );
    assert!(ledger::check_manual_native(
        &state,
        Some(&entry.id),
        agents::AgentKind::Claude,
        Some(NATIVE)
    )
    .is_err());
    assert!(
        ledger::check_manual_native(&state, Some(&entry.id), agents::AgentKind::Claude, None)
            .is_err()
    );
    assert!(ledger::manual_resumption(entry.id.clone(), async {
        ledger::check_manual_native(
            &state,
            Some(&entry.id),
            agents::AgentKind::Claude,
            Some(NATIVE),
        )
    })
    .await
    .is_ok());
    assert!(state.chat.get(&entry.id).is_none());
    let other = lock(&state.workspaces)
        .add(test_dir("manual-foreign-scope"))
        .unwrap();
    pro::install_execution_fixture(&state, &other.id, 4).unwrap();
    let response = app(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(&uri)
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header("x-chimaera-workspace", &other.id)
                .header("x-chimaera-epoch", "4")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::OK);
    assert!(state.chat.get(&entry.id).is_none());
}

#[tokio::test]
async fn manual_receipts_refuse_unknown_storage_and_bound_live_originals() {
    let (state, entry, _) = fixture();
    pro::ensure_root(pro::manual_resume_storage(&state))
        .await
        .unwrap();
    let mut receipts = ledger::manual::load(&state).unwrap();
    receipts.retain(&state, &entry).unwrap();
    ledger::manual::save(&state, &receipts).unwrap();
    assert!(ledger::manual::load(&state).unwrap().contains(&entry));
    for index in 1..64 {
        let mut next = entry.clone();
        next.id = format!("s-manual-{index}");
        lock(&state.deferred_sessions).insert(next.id.clone(), next.clone());
        receipts.retain(&state, &next).unwrap();
    }
    let mut overflow = entry.clone();
    overflow.id = "s-manual-overflow".into();
    assert!(receipts.retain(&state, &overflow).is_err());
    ledger::manual::save(&state, &receipts).unwrap();
    let path = pro::manual_resume_storage(&state).join("manual-resumes.json");
    for bytes in [
        b"{\"version\":1,\"entries\":[],\"unknown\":1}".as_slice(),
        b"not json",
        b"{\"version\":1,\"entries\":[]} trailing",
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert!(ledger::manual::load(&state).is_err());
    }
    std::fs::write(&path, vec![b' '; 128 * 1024 + 1]).unwrap();
    assert!(ledger::manual::load(&state).is_err());
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("absent", &path).unwrap();
    assert!(ledger::manual::load(&state).is_err());
    std::fs::remove_file(&path).unwrap();
    let fifo = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { nix::libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let started = std::time::Instant::now();
    assert!(ledger::manual::load(&state).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}
