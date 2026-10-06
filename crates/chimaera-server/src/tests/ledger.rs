use super::support::*;
use crate::*;

/// The stateful-restart contract end to end: a live shell recorded in
/// the ledger comes back UNDER THE SAME SESSION ID after a "restart"
/// (a second AppState over the same data dir) — at its cwd, with its
/// pinned name and theme — while an agent entry lacking a captured native
/// handle retires into the workspace's recents instead of vanishing.
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
        suspended: false,
        manual_resume_reason: None,
        handoff: None,
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
            native_cwd: None,
            title: "port the parser".to_string(),
            ui: chimaera_agent::model::SessionUi::Term,
            model: None,
            carryover: None,
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
                suspended: false,
                manual_resume_reason: None,
                handoff: None,
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
                suspended: false,
                manual_resume_reason: None,
                handoff: None,
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
                    native_cwd: None,
                    title: "fix the flaky tests".to_string(),
                    ui: chimaera_agent::model::SessionUi::Term,
                    model: None,
                    carryover: None,
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

/// Chats outlive the daemon, so the journals a boot ledger resurrects are
/// routinely the OLDEST in the chat dir — and they are what those chats
/// replay and resume from. The dir budget must spare them at construction
/// (before restore has run) and across the resurrection spawns themselves
/// (the first chat's spawn must not evict the second's journal), while
/// history is still evicted oldest-first once the roster is whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn journal_budget_spares_chats_the_ledger_resurrects() {
    use chimaera_agent::journal::{seed_journal, DIR_MAX_FILES};
    use chimaera_agent::model::AgentEvent;

    fn backdate(path: &std::path::Path, secs_ago: u64) {
        let mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        std::fs::File::open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
    }
    fn journals(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".jsonl"))
            .collect()
    }

    let data = test_dir("ledger-journal-budget");
    let chat_dir = data.join("chat");
    // Two chats the previous daemon was running, idle for a day...
    for (id, text) in [("s-chat-a", "alpha history"), ("s-chat-b", "beta history")] {
        seed_journal(
            &chat_dir,
            id,
            &[AgentEvent::MessageChunk {
                turn_id: "t1".into(),
                text: text.into(),
            }],
        )
        .unwrap();
        backdate(&chat_dir.join(format!("{id}.jsonl")), 86_400);
    }
    // ...then more (newer) history than the budget holds.
    for i in 0..DIR_MAX_FILES + 5 {
        let path = chat_dir.join(format!("h-{i:03}.jsonl"));
        std::fs::write(&path, b"{\"seq\":1,\"ts\":0}\n").unwrap();
        backdate(&path, 3_600 - i as u64);
    }

    let state = test_state_with_data_dir(0, data);
    assert!(
        chat_dir.join("s-chat-a.jsonl").exists() && chat_dir.join("s-chat-b.jsonl").exists(),
        "constructing the daemon must not prune before the ledger is restored"
    );

    let workspace_id = make_workspace(&state, "ledger-journal-budget-ws").await;
    let root = lock(&state.workspaces).get(&workspace_id).unwrap().root;
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude("ledger-journal-budget-fake")),
        Some("9.9.9-fake"),
    );
    let chat_entry = |id: &str| ledger::LedgerEntry {
        suspended: false,
        manual_resume_reason: None,
        handoff: None,
        id: id.to_string(),
        workspace_id: workspace_id.clone(),
        cwd: root.clone(),
        pinned_name: None,
        cols: 120,
        rows: 40,
        theme: "dark".to_string(),
        created_at: 0,
        agent: Some(ledger::LedgerAgent {
            kind: agents::AgentKind::Claude,
            resume: None,
            transcript: None,
            native_cwd: None,
            title: "claude".to_string(),
            ui: chimaera_agent::model::SessionUi::Chat,
            model: None,
            carryover: None,
        }),
    };
    let boot = ledger::BootLedger {
        sessions: vec![chat_entry("s-chat-a"), chat_entry("s-chat-b")],
        links: std::collections::HashMap::new(),
        written_at: 0,
    };
    // As `lifecycle::serve` does before the ledger task runs.
    state.restored.send_replace(false);
    ledger::consume_boot(&state, boot).await;

    for (id, text) in [("s-chat-a", "alpha history"), ("s-chat-b", "beta history")] {
        assert!(state.chat.contains(id), "{id} resurrected");
        let journal = std::fs::read_to_string(chat_dir.join(format!("{id}.jsonl")))
            .unwrap_or_else(|e| panic!("{id}'s journal was pruned: {e}"));
        assert!(journal.contains(text), "{id} kept its history");
    }
    let remaining = journals(&chat_dir);
    assert_eq!(remaining.len(), DIR_MAX_FILES, "the budget still holds");
    assert!(
        !remaining.contains(&"h-000.jsonl".to_string()),
        "history is evicted oldest-first"
    );
    assert!(remaining.contains(&format!("h-{:03}.jsonl", DIR_MAX_FILES + 4)));

    state.chat.kill("s-chat-a");
    state.chat.kill("s-chat-b");
}

/// Laptop first across a restart: a Pro-managed project's previous agents
/// wait for this daemon life to verify ownership (so a project the cloud took
/// over never resumes a stale turn here), then resume anyway when the account
/// cannot confirm in time. A verified other owner keeps them suspended. Plain
/// shells never wait: they come back at boot.
#[tokio::test]
async fn restart_deferred_sessions_resume_unless_another_owner_is_verified() {
    for remote in [false, true] {
        let data = test_dir("ledger-verification");
        let state = test_state_with_data_dir(0, data.clone());
        let root = std::fs::canonicalize(test_dir("ledger-verification-root")).unwrap();
        let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
        preset_agent(
            &state,
            agents::AgentKind::Claude,
            Ok(write_fake_claude("ledger-verification-fake")),
            Some("9.9.9-fake"),
        );
        // Enrolled earlier; this life has not renewed its lease yet.
        pro::install_execution_fixture(&state, &workspace.id, 3).unwrap();
        pro::expire_execution_fixture(&state, &workspace.id);
        let entry = |id: &str, agent: Option<ledger::LedgerAgent>| ledger::LedgerEntry {
            suspended: false,
            manual_resume_reason: None,
            handoff: None,
            id: id.to_string(),
            workspace_id: workspace.id.clone(),
            cwd: root.clone(),
            pinned_name: None,
            cols: 80,
            rows: 24,
            theme: "dark".to_string(),
            created_at: 0,
            agent,
        };
        let chat = ledger::LedgerAgent {
            kind: agents::AgentKind::Claude,
            resume: None,
            transcript: None,
            native_cwd: None,
            title: "claude".to_string(),
            ui: chimaera_agent::model::SessionUi::Chat,
            model: None,
            carryover: None,
        };
        let boot = ledger::BootLedger {
            sessions: vec![
                entry("s-restart-shell", None),
                entry("s-restart-chat", Some(chat)),
            ],
            links: std::collections::HashMap::new(),
            written_at: 1_750_000_000,
        };
        ledger::restore(&state, boot).await;
        assert!(
            state.sessions.get("s-restart-shell").is_some(),
            "a plain shell never waits"
        );
        assert!(
            !state.chat.contains("s-restart-chat"),
            "waits for verification"
        );
        assert!(lock(&state.deferred_sessions).contains_key("s-restart-chat"));
        assert!(pro::any_restart_deferred(&state), "the fallback has work");
        if remote {
            pro::install_remote_owner_fixture(&state, &workspace.id, 4);
        }
        pro::resume_unverified(&state).await;
        assert_eq!(
            state.chat.contains("s-restart-chat"),
            !remote,
            "remote={remote}"
        );
        assert_eq!(
            lock(&state.deferred_sessions).contains_key("s-restart-chat"),
            remote
        );
        let _ = state.sessions.kill("s-restart-shell");
        state.chat.kill("s-restart-chat");
        state
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// A free daemon's boot defers nothing, so it starts no one-minute fallback
/// timer: its chat comes back at boot like its shell.
#[tokio::test]
async fn a_free_boot_defers_nothing() {
    let state = test_state_with_data_dir(0, test_dir("ledger-free-boot"));
    let root = std::fs::canonicalize(test_dir("ledger-free-boot-root")).unwrap();
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude("ledger-free-boot-fake")),
        Some("9.9.9-fake"),
    );
    let boot = ledger::BootLedger {
        sessions: vec![ledger::LedgerEntry {
            suspended: false,
            manual_resume_reason: None,
            handoff: None,
            id: "s-free-chat".to_string(),
            workspace_id: workspace.id.clone(),
            cwd: root.clone(),
            pinned_name: None,
            cols: 80,
            rows: 24,
            theme: "dark".to_string(),
            created_at: 0,
            agent: Some(ledger::LedgerAgent {
                kind: agents::AgentKind::Claude,
                resume: None,
                transcript: None,
                native_cwd: None,
                title: "claude".to_string(),
                ui: chimaera_agent::model::SessionUi::Chat,
                model: None,
                carryover: None,
            }),
        }],
        links: std::collections::HashMap::new(),
        written_at: 1_750_000_000,
    };
    ledger::restore(&state, boot).await;
    assert!(state.chat.contains("s-free-chat"), "comes back at boot");
    assert!(!pro::any_restart_deferred(&state));
    state.chat.kill("s-free-chat");
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}

/// A chat a return from the cloud imported here, still waiting for that
/// return's resume (what `finish_hydration` resumes once the project is
/// this computer's).
fn returned_chat(id: &str, workspace: &str, root: &std::path::Path) -> ledger::LedgerEntry {
    ledger::LedgerEntry {
        suspended: true,
        manual_resume_reason: None,
        handoff: Some(crate::bundle::HandoffResume {
            fork: false,
            origin: crate::bundle::Origin::Home,
            epoch: 3,
        }),
        id: id.to_string(),
        workspace_id: workspace.to_string(),
        cwd: root.to_path_buf(),
        pinned_name: None,
        cols: 80,
        rows: 24,
        theme: "dark".to_string(),
        created_at: 0,
        agent: Some(ledger::LedgerAgent {
            kind: agents::AgentKind::Claude,
            resume: None,
            transcript: None,
            native_cwd: None,
            title: "claude".to_string(),
            ui: chimaera_agent::model::SessionUi::Chat,
            model: None,
            carryover: None,
        }),
    }
}

async fn wait_resumed(state: &Arc<AppState>, id: &str) {
    let resumed = || {
        state.chat.get(id).is_some_and(|chat| chat.alive)
            && !lock(&state.deferred_sessions).contains_key(id)
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !resumed() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        resumed(),
        "{id} stayed deferred: {:?}",
        crate::ws::pause_state(state, id).map(|pause| pause.frame())
    );
}

/// Sign-out right after a return: the mirror task running the return's
/// resume is aborted and the project's `Local` ownership dropped. The
/// returned conversation must not stay deferred as "moved to computer"
/// forever: sign-out resumes it here, and its socket greets with `ready`.
#[tokio::test]
async fn sign_out_resumes_a_returned_session_instead_of_stranding_it() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let root = std::fs::canonicalize(test_dir("ledger-signout-return-root")).unwrap();
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude("ledger-signout-return-fake")),
        Some("9.9.9-fake"),
    );
    // The return installed the project and made it this computer's; its
    // conversation had not respawned yet when the user signed out.
    pro::install_execution_fixture(&state, &workspace.id, 3).unwrap();
    ledger::defer(&state, returned_chat("s-returned", &workspace.id, &root)).unwrap();
    assert_eq!(
        crate::ws::pause_state(&state, "s-returned").map(|pause| pause.frame()["type"].clone()),
        Some(serde_json::json!("moved"))
    );

    let (status, _) = request(&state, Method::DELETE, "/api/v1/pro/configure", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    wait_resumed(&state, "s-returned").await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws/chat/s-returned"))
            .await
            .unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token", "last_seq": 0}).to_string(),
        ))
        .await
        .unwrap();
    let frame = loop {
        if let WsMessage::Text(text) = next_ws_frame(&mut socket).await {
            break serde_json::from_str::<serde_json::Value>(&text).unwrap();
        }
    };
    assert_eq!(frame["type"], "ready", "{frame}");
    state.chat.kill("s-returned");
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}

/// A returned conversation a sign-out or crash left deferred comes back at
/// the next boot like any session the previous daemon left: it waits for
/// this life's ownership proof (paused as restarting, never "moved") and
/// the device fallback resumes it — enrolled or not, signed in or not.
#[tokio::test]
async fn boot_resumes_a_returned_session_left_deferred() {
    for enrolled in [false, true] {
        let state = test_state();
        let root = std::fs::canonicalize(test_dir("ledger-stranded-root")).unwrap();
        let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
        preset_agent(
            &state,
            agents::AgentKind::Claude,
            Ok(write_fake_claude("ledger-stranded-fake")),
            Some("9.9.9-fake"),
        );
        if enrolled {
            // Enrolled, then signed out: no ownership and no lease remain.
            pro::install_execution_fixture(&state, &workspace.id, 3).unwrap();
            let (status, _) = request(&state, Method::DELETE, "/api/v1/pro/configure", None).await;
            assert_eq!(status, StatusCode::NO_CONTENT);
        }
        let boot = ledger::BootLedger {
            sessions: vec![returned_chat("s-stranded", &workspace.id, &root)],
            links: std::collections::HashMap::new(),
            written_at: 1_750_000_000,
        };
        ledger::restore(&state, boot).await;
        let pause = crate::ws::pause_state(&state, "s-stranded").map(|pause| pause.frame());
        assert_eq!(
            pause.as_ref().map(|frame| frame["reason"].clone()),
            Some(serde_json::json!("restarting")),
            "enrolled={enrolled}: {pause:?}"
        );
        pro::resume_unverified(&state).await;
        wait_resumed(&state, "s-stranded").await;
        state.chat.kill("s-stranded");
        state
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Two resumes racing for one deferred session (a return's own and a
/// sign-out's) start it once. Without that, the loser's failed spawn tears
/// down the winner's agent record and project mapping.
#[tokio::test]
async fn racing_resumes_start_a_deferred_session_once() {
    let state = test_state();
    let root = std::fs::canonicalize(test_dir("ledger-race-root")).unwrap();
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude("ledger-race-fake")),
        Some("9.9.9-fake"),
    );
    ledger::defer(&state, returned_chat("s-raced", &workspace.id, &root)).unwrap();
    let (first, second) = tokio::join!(
        ledger::resume_deferred_workspace(&state, &workspace.id),
        ledger::resume_deferred_workspace(&state, &workspace.id)
    );
    assert!(first.is_ok() && second.is_ok(), "{first:?} {second:?}");
    assert!(state.chat.get("s-raced").is_some_and(|chat| chat.alive));
    assert!(!lock(&state.deferred_sessions).contains_key("s-raced"));
    assert!(lock(&state.agents).contains_key("s-raced"));
    assert_eq!(
        lock(&state.session_workspaces).get("s-raced"),
        Some(&workspace.id)
    );
    state.chat.kill("s-raced");
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}

/// A conversation resumed here that has not run a turn yet (claude reports
/// its native id only with its first turn) keeps the id it was resumed from
/// in the ledger, so it can move on to another machine (the export finds its
/// transcript) and survive a restart with its history.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resumed_chat_keeps_its_conversation_before_its_first_turn() {
    let state = test_state();
    let root = std::fs::canonicalize(test_dir("ledger-resumed-tip-root")).unwrap();
    let workspace = lock(&state.workspaces).add(root.clone()).unwrap();
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude("ledger-resumed-tip-fake")),
        Some("9.9.9-fake"),
    );
    let native = "3f1c2b8e-0000-4000-8000-000000000001";
    let store = state
        .claude_projects_dir
        .join(crate::launcher::encode_cwd(&root));
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join(format!("{native}.jsonl")),
        format!("{{\"type\":\"user\",\"sessionId\":\"{native}\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n"),
    )
    .unwrap();
    let mut entry = returned_chat("s-tip", &workspace.id, &root);
    entry.agent.as_mut().unwrap().resume = Some(native.to_string());
    ledger::defer(&state, entry).unwrap();
    ledger::resume_deferred_workspace(&state, &workspace.id)
        .await
        .unwrap();
    wait_resumed(&state, "s-tip").await;
    assert!(
        state
            .chat
            .get("s-tip")
            .is_some_and(|chat| chat.native_session_id.is_none()),
        "no turn ran, so the agent has not reported its id"
    );
    let (entries, _) = ledger::snapshot(&state);
    let agent = entries
        .iter()
        .find(|entry| entry.id == "s-tip")
        .and_then(|entry| entry.agent.clone())
        .expect("the resumed chat is in the ledger");
    assert_eq!(agent.resume.as_deref(), Some(native));
    state.chat.kill("s-tip");
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
}

/// The native app's hidden workspace (a cluster's login-node terminal):
/// never listed, and its sessions never enter the ledger — a restart must
/// not bring that terminal back as a plain local shell. Registration is
/// idempotent per root and stays hidden.
#[tokio::test]
async fn hidden_workspace_is_unlisted_and_its_sessions_stay_out_of_the_ledger() {
    let state = test_state();
    let root = std::fs::canonicalize(test_dir("ledger-hidden-root")).unwrap();
    let body = serde_json::json!({"root": root.to_string_lossy(), "hidden": true});
    let (status, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ws}");
    assert_eq!(ws["hidden"], true);
    let hidden_id = ws["id"].as_str().unwrap().to_string();
    let (_, again) = request(&state, Method::POST, "/api/v1/workspaces", Some(body)).await;
    assert_eq!(
        again["id"].as_str(),
        Some(hidden_id.as_str()),
        "idempotent per root"
    );

    let shown = std::fs::canonicalize(test_dir("ledger-shown-root")).unwrap();
    let (_, plain) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": shown.to_string_lossy()})),
    )
    .await;
    assert!(
        plain.get("hidden").is_none(),
        "a user workspace's wire shape is unchanged"
    );
    let shown_id = plain["id"].as_str().unwrap().to_string();

    let (_, listed) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
    let ids: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec![shown_id.as_str()],
        "only the user's workspace is listed"
    );

    let mut sids = Vec::new();
    for ws in [&hidden_id, &shown_id] {
        let (status, session) = request(
            &state,
            Method::POST,
            "/api/v1/sessions",
            Some(serde_json::json!({ "workspace_id": ws })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "spawn failed: {session}");
        sids.push(session["id"].as_str().unwrap().to_string());
    }
    let (entries, _) = ledger::snapshot(&state);
    let in_ledger: Vec<&str> = entries.iter().map(|e| e.workspace_id.as_str()).collect();
    assert_eq!(
        in_ledger,
        vec![shown_id.as_str()],
        "the hidden session stays out"
    );
    for sid in sids {
        state.sessions.kill(&sid).ok();
    }
}
