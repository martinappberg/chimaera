use super::support::*;
use crate::*;

/// The stateful-restart contract end to end: a live shell recorded in
/// the ledger comes back UNDER THE SAME SESSION ID after a "restart"
/// (a second AppState over the same data dir) — at its cwd, with its
/// pinned name and theme — while a non-resumable agent entry retires
/// into the workspace's recents instead of vanishing.
#[tokio::test]
async fn ledger_resurrects_sessions_across_restart() {
    let data = test_dir("ledger-restart");
    let state = test_state_with_data_dir(0, data.clone());
    // Canonicalized like create_workspace canonicalizes it (macOS /var
    // is a symlink to /private/var).
    let root = std::fs::canonicalize(test_dir("ledger-restart-root")).unwrap();
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let workspace_id = ws["id"].as_str().unwrap().to_string();

    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({
            "workspace_id": workspace_id,
            "name": "data wrangling",
            "theme": "light",
            "cols": 132,
            "rows": 43,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "spawn failed: {session}");
    let sid = session["id"].as_str().unwrap().to_string();

    // Persist the ledger the way the reconcile loop would, then kill the
    // PTY: the "daemon" is going down, taking its children with it.
    let (entries, links) = ledger::snapshot(&state);
    assert_eq!(entries.len(), 1, "the live shell is in the ledger");
    lock(&state.ledger).write_if_changed(&entries, &links);
    state.sessions.kill(&sid).ok();

    // "Restart": a fresh AppState over the same data dir. The boot
    // ledger carries the shell — plus an agent entry we add by hand (a
    // codex conversation the old daemon was running), which cannot
    // resurrect and must retire into recents.
    let state2 = test_state_with_data_dir(0, data);
    let mut boot = lock(&state2.ledger).load_boot();
    assert_eq!(boot.sessions.len(), 1);
    boot.sessions.push(ledger::LedgerEntry {
        id: "s-dead-codex".to_string(),
        workspace_id: workspace_id.clone(),
        cwd: root.clone(),
        pinned_name: None,
        cols: 80,
        rows: 24,
        theme: "dark".to_string(),
        created_at: 0,
        agent: Some(ledger::LedgerAgent {
            kind: agents::AgentKind::Codex,
            resume: None,
            transcript: None,
            title: "port the parser".to_string(),
            ui: chimaera_agent::model::SessionUi::Term,
            model: None,
        }),
    });
    ledger::restore(&state2, boot).await;

    // The shell is back under ITS OLD ID — that identity is what lets
    // every persisted layout tab rebind without migration.
    let infos = state2.sessions.list();
    assert_eq!(infos.len(), 1, "exactly the shell respawned");
    let info = &infos[0];
    assert_eq!(info.id, sid, "session id survives the restart");
    assert_eq!(info.cwd, root);
    assert_eq!(info.name, "data wrangling");
    assert!(info.renamed, "the pinned name stays pinned");
    assert_eq!((info.cols, info.rows), (132, 43));
    assert_eq!(
        lock(&state2.session_workspaces).get(&sid),
        Some(&workspace_id)
    );
    assert_eq!(
        lock(&state2.session_themes).get(&sid).map(String::as_str),
        Some("light"),
        "the spawn theme carries across"
    );

    // The codex conversation retired into recents (resumable rows are
    // the statefulness story for agents that cannot resurrect).
    let recents = lock(&state2.recents).list(&workspace_id);
    assert_eq!(recents.len(), 1);
    assert_eq!(recents[0].title, "port the parser");
    assert_eq!(recents[0].kind, agents::AgentKind::Codex);
    assert!(
        state2
            .recents_epoch
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0,
        "the recents epoch moved so the rail refetches"
    );

    state2.sessions.kill(&sid).ok();
}

/// Restore is opt-out: with `daemon.restoreSessions` false the shell is
/// dropped, but agent conversations still retire into recents — turning
/// restore off must never make history vanish.
#[tokio::test]
async fn ledger_restore_disabled_still_lands_recents() {
    let data = test_dir("ledger-optout");
    let state = test_state_with_data_dir(0, data.clone());
    let root = test_dir("ledger-optout-root");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let workspace_id = ws["id"].as_str().unwrap().to_string();
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({"daemon.restoreSessions": false})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The conversation's transcript exists (hook-recorded path), so its
    // recents row must stay resumable through retirement.
    let transcript = data.join("conv-1.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let boot = ledger::BootLedger {
        sessions: vec![
            ledger::LedgerEntry {
                id: "s-shell".to_string(),
                workspace_id: workspace_id.clone(),
                cwd: root.clone(),
                pinned_name: None,
                cols: 80,
                rows: 24,
                theme: "dark".to_string(),
                created_at: 0,
                agent: None,
            },
            ledger::LedgerEntry {
                id: "s-claude".to_string(),
                workspace_id: workspace_id.clone(),
                cwd: root.clone(),
                pinned_name: None,
                cols: 80,
                rows: 24,
                theme: "dark".to_string(),
                created_at: 0,
                agent: Some(ledger::LedgerAgent {
                    kind: agents::AgentKind::Claude,
                    resume: Some("conv-1".to_string()),
                    transcript: Some(transcript),
                    title: "fix the flaky tests".to_string(),
                    ui: chimaera_agent::model::SessionUi::Term,
                    model: None,
                }),
            },
        ],
        links: std::collections::HashMap::new(),
        written_at: 1_750_000_000,
    };
    ledger::restore(&state, boot).await;

    assert!(state.sessions.list().is_empty(), "nothing respawns");
    let recents = lock(&state.recents).list(&workspace_id);
    assert_eq!(recents.len(), 1, "the conversation is still findable");
    assert_eq!(recents[0].title, "fix the flaky tests");
    assert_eq!(recents[0].resume.as_deref(), Some("conv-1"));
    assert_eq!(recents[0].last_active, 1_750_000_000);
}
