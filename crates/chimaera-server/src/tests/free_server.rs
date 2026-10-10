//! A daemon without the Pro extension keeps main's server behaviour on the
//! paths the Pro merge touched: Codex TUI identity, Recents resume ids and the
//! manual-resume route.
use super::support::*;
use crate::*;

const THREAD: &str = "0199a1b2-c3d4-4e5f-8a6b-7c8d9e0f1a2b";

/// A verified rollout for `THREAD` in `cwd`, under the fixture's Codex home.
fn plant_rollout(state: &Arc<AppState>, cwd: &std::path::Path) -> PathBuf {
    let home = state.codex_config_path.parent().unwrap().to_path_buf();
    let day = home.join("sessions/2026/10/06");
    std::fs::create_dir_all(&day).unwrap();
    let path = day.join(format!("rollout-2026-10-06T00-00-00-{THREAD}.jsonl"));
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::json!({"type":"session_meta","payload":{"id":THREAD,"cwd":cwd}})
        ),
    )
    .unwrap();
    assert!(codex_rollout::find_rollout(&home, THREAD, cwd).is_some());
    path
}

/// No notify shim on a free daemon, so a resumed Codex TUI never scans the
/// rollout store: its record has no transcript or thread identity, as on main.
#[tokio::test]
async fn free_codex_tui_spawn_carries_no_rollout_identity() {
    let state = test_state();
    preset_agent(
        &state,
        agents::AgentKind::Codex,
        Ok(PathBuf::from("/bin/echo")),
        None,
    );
    let root = test_dir("free-codex-identity");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let cwd = lock(&state.workspaces)
        .get(ws["id"].as_str().unwrap())
        .unwrap()
        .root;
    plant_rollout(&state, &cwd);
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({
            "workspace_id": ws["id"], "kind": "agent",
            "agent": "codex", "resume": THREAD,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let id = session["id"].as_str().unwrap();
    {
        let agents = lock(&state.agents);
        let record = agents.get(id).expect("the spawned TUI has a record");
        assert_eq!(record.transcript_path, None);
        assert_eq!(record.codex_thread_id, None);
        assert_eq!(record.resumed_from.as_deref(), Some(THREAD));
    }
    state.sessions.kill(id).ok();
}

/// Outside a Pro-scoped project a retired Codex TUI keeps main's resume id
/// (the hint, else its resumed-from ancestor), even with a rollout on record.
#[tokio::test]
async fn free_codex_tui_recents_keep_the_ancestor_resume() {
    let state = test_state();
    let ws = make_workspace(&state, "free-codex-recents").await;
    let cwd = lock(&state.workspaces).get(&ws).unwrap().root;
    let rollout = plant_rollout(&state, &cwd);
    let mut record = agents::AgentRecord::new("k".into(), agents::AgentKind::Codex);
    record.resumed_from = Some("ancestor-thread".into());
    record.codex_thread_id = Some(THREAD.into());
    record.transcript_path = Some(rollout);
    lock(&state.agents).insert("s-free-cdx".into(), record);
    lock(&state.session_workspaces).insert("s-free-cdx".into(), ws.clone());
    recents::retire(
        &state,
        "s-free-cdx",
        None,
        None,
        chimaera_agent::model::SessionUi::Term,
    );
    let entries = recents_of(&state, &ws).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["kind"], "codex");
    assert_eq!(entries[0]["resume"], "ancestor-thread");
}

/// Manual resume is Pro-only: a free daemon answers 404 before it takes the
/// view-switch lock or creates `<data>/pro`.
#[tokio::test]
async fn free_manual_resume_is_missing_and_creates_no_pro_state() {
    let state = test_state();
    let ws = make_workspace(&state, "free-manual-resume").await;
    let cwd = lock(&state.workspaces).get(&ws).unwrap().root;
    let entry = ledger::LedgerEntry {
        id: "s-free-manual".into(),
        suspended: true,
        manual_resume_reason: Some("project_secrets_idle".into()),
        fence_epoch: None,
        handoff: None,
        workspace_id: ws.clone(),
        cwd,
        pinned_name: None,
        cols: 80,
        rows: 24,
        theme: "dark".into(),
        created_at: 1,
        agent: Some(ledger::LedgerAgent {
            kind: agents::AgentKind::Claude,
            resume: Some(THREAD.into()),
            transcript: None,
            native_cwd: None,
            title: "synthetic".into(),
            ui: chimaera_agent::model::SessionUi::Chat,
            model: None,
            carryover: None,
        }),
    };
    lock(&state.deferred_sessions).insert(entry.id.clone(), entry.clone());
    lock(&state.session_workspaces).insert(entry.id.clone(), ws);
    let (status, body) = request(
        &state,
        Method::POST,
        &format!("/api/v1/sessions/{}/resume", entry.id),
        None,
    )
    .await;
    // Only an extension adds the route (`WorkspacePolicy::routes`); the
    // inert policy adds none, so the generic fallback answers.
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "not found");
    assert!(state.extension.get().is_none());
    assert!(!lock(&state.chat_switching).contains_key(&entry.id));
}

/// Nothing resumes a deferred entry without an extension, so a conversation
/// an earlier composed daemon suspended for a manual resume is ordinary local
/// work for a free daemon: it lands in Recents (restore is off here) instead
/// of being stranded, and starting it again from there is not refused.
#[tokio::test]
async fn free_restore_does_not_strand_a_suspended_conversation() {
    let state = test_state();
    let ws = make_workspace(&state, "free-suspended-restore").await;
    let cwd = lock(&state.workspaces).get(&ws).unwrap().root;
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({"daemon.restoreSessions": false})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let entry = ledger::LedgerEntry {
        id: "s-free-suspended".into(),
        suspended: true,
        manual_resume_reason: Some("project_secrets_idle".into()),
        fence_epoch: None,
        handoff: None,
        workspace_id: ws.clone(),
        cwd,
        pinned_name: None,
        cols: 80,
        rows: 24,
        theme: "dark".into(),
        created_at: 1,
        agent: Some(ledger::LedgerAgent {
            kind: agents::AgentKind::Codex,
            resume: Some(THREAD.into()),
            transcript: None,
            native_cwd: None,
            title: "suspended work".into(),
            ui: chimaera_agent::model::SessionUi::Chat,
            model: None,
            carryover: None,
        }),
    };
    let boot = ledger::BootLedger {
        sessions: vec![entry.clone()],
        links: Default::default(),
        written_at: 1_750_000_000,
    };
    ledger::restore(&state, boot).await;

    assert!(
        lock(&state.deferred_sessions).is_empty(),
        "nothing deferred"
    );
    let (live, _) = ledger::snapshot(&state);
    assert!(live.is_empty(), "the ledger does not carry it forward");
    let recents = lock(&state.recents).list(&ws);
    assert_eq!(recents.len(), 1, "the conversation is findable in Recents");
    assert_eq!(recents[0].resume.as_deref(), Some(THREAD));

    // The Recents row starts again; the 409 manual-resume guard is inert.
    ledger::check_manual_native(&state, None, agents::AgentKind::Codex, Some(THREAD))
        .expect("a free daemon never requires a manual resume");
    lock(&state.deferred_sessions).insert(entry.id.clone(), entry);
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({
            "workspace_id": ws, "kind": "agent", "agent": "codex", "resume": THREAD,
        })),
    )
    .await;
    assert_ne!(status, StatusCode::CONFLICT, "{body}");
    assert_ne!(body["error"], "manual_resume_required");
    if let Some(id) = body["id"].as_str() {
        state.sessions.kill(id).ok();
    }
}

/// The policy is chosen once: installing after `policy()` defaulted fails
/// loudly instead of silently keeping the inert policy.
#[test]
#[should_panic(expected = "workspace policy installed twice")]
fn install_policy_after_first_use_panics() {
    let state = test_state();
    let _ = state.policy().composed(&state);
    state.install_policy(None, &state.data_dir.clone());
}

/// Every Codex rollout lookup goes through one capture (the Codex home a
/// terminal sees, the shared lookup limit); a known path must still verify.
#[tokio::test]
async fn codex_rollout_capture_verifies_and_finds() {
    let state = test_state();
    let ws = make_workspace(&state, "free-codex-capture").await;
    let cwd = lock(&state.workspaces).get(&ws).unwrap().root;
    let rollout = plant_rollout(&state, &cwd);
    assert_eq!(
        codex_rollout::capture(&state, THREAD, cwd.clone(), None).await,
        Some(rollout.clone())
    );
    let elsewhere = test_dir("free-codex-capture-other");
    assert_eq!(
        codex_rollout::capture(&state, THREAD, elsewhere, Some(rollout.clone())).await,
        None,
        "a known path must still verify against the cwd"
    );
}
