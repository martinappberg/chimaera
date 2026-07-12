use super::support::*;
use crate::*;

#[tokio::test]
async fn recents_retire_round_trips_and_persists() {
    let data_dir = test_dir("recents");
    let state = test_state_with_data_dir(0, data_dir.clone());
    let ws = make_workspace(&state, "recents-root").await;
    // The transcript must exist on disk: retire only mints resume ids a
    // click can deliver (claude 2.1.204 interactive sessions persist no
    // transcript, so unverified ids are common, not corrupt).
    let transcript = data_dir.join("abc-123.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    plant_agent_record(
        &state,
        "s-1",
        &ws,
        agents::AgentKind::Claude,
        Some("fix the flaky test"),
        Some(transcript.to_str().unwrap()),
    );

    recents::retire(
        &state,
        "s-1",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );

    let entries = recents_of(&state, &ws).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["kind"], "claude");
    assert_eq!(entries[0]["title"], "fix the flaky test");
    assert_eq!(entries[0]["resume"], "abc-123");
    assert!(entries[0]["last_active"].as_u64().unwrap() > 0);
    // The last-used surface rode along, so the row reopens in the same mode.
    assert_eq!(entries[0]["ui"], "chat");
    // The record and its workspace mapping are gone (retire IS the
    // cleanup path).
    assert!(lock(&state.agents).get("s-1").is_none());
    assert!(lock(&state.session_workspaces).get("s-1").is_none());

    // Daemon restart: a fresh state over the same data dir still has it.
    let reloaded = test_state_with_data_dir(0, data_dir);
    // Same workspace registry file, so the id resolves.
    let entries = recents_of(&reloaded, &ws).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["resume"], "abc-123");
    // The `ui` field persists through the store file across a restart.
    assert_eq!(entries[0]["ui"], "chat");
}

#[tokio::test]
async fn recents_skip_untitled_claude_keep_codex_and_pins_win() {
    let state = test_state();
    let ws = make_workspace(&state, "recents-mixed").await;

    // Claude empty boot: fallback name, no transcript — not a memory.
    plant_agent_record(
        &state,
        "s-empty",
        &ws,
        agents::AgentKind::Claude,
        None,
        None,
    );
    recents::retire(
        &state,
        "s-empty",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );
    assert!(recents_of(&state, &ws).await.is_empty());

    // Codex has no title machinery: its bare-name row still counts.
    plant_agent_record(&state, "s-cdx", &ws, agents::AgentKind::Codex, None, None);
    recents::retire(
        &state,
        "s-cdx",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );

    // A user-renamed claude keeps its pinned name, and an OSC title
    // beats the ai title (same precedence as live display names).
    plant_agent_record(&state, "s-pin", &ws, agents::AgentKind::Claude, None, None);
    recents::retire(
        &state,
        "s-pin",
        Some("bio-evolve run"),
        None,
        chimaera_agent::model::SessionUi::Chat,
    );

    let entries = recents_of(&state, &ws).await;
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[0]["title"], "bio-evolve run"); // newest first
    assert_eq!(entries[0]["kind"], "claude");
    assert_eq!(entries[0]["resume"], serde_json::Value::Null);
    assert_eq!(entries[1]["kind"], "codex");
    assert_eq!(entries[1]["title"], "codex");
}

#[tokio::test]
async fn recents_hide_live_conversations_and_dedupe_resumes() {
    let state = test_state();
    let ws = make_workspace(&state, "recents-live").await;
    let store = test_dir("recents-live-transcripts");
    let transcript = store.join("conv-9.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let transcript = transcript.to_str().unwrap();

    plant_agent_record(
        &state,
        "s-a",
        &ws,
        agents::AgentKind::Claude,
        Some("hooks online"),
        Some(transcript),
    );
    recents::retire(
        &state,
        "s-a",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );
    assert_eq!(recents_of(&state, &ws).await.len(), 1);

    // The same conversation resumed in a live session: hidden, not lost.
    plant_agent_record(
        &state,
        "s-b",
        &ws,
        agents::AgentKind::Claude,
        Some("hooks online"),
        Some(transcript),
    );
    assert!(recents_of(&state, &ws).await.is_empty());

    // It ends again with a newer title: back in the list, still one entry.
    crate::lock(&state.agents).get_mut("s-b").unwrap().ai_title =
        Some("hooks online v2".to_string());
    recents::retire(
        &state,
        "s-b",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );
    let entries = recents_of(&state, &ws).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["title"], "hooks online v2");

    // Resumed live again: claude forks a NEW session id on --resume, so
    // until hooks report the new transcript the live record knows only
    // what it resumed from — that alone must hide the ancestor entry.
    plant_agent_record(&state, "s-c", &ws, agents::AgentKind::Claude, None, None);
    lock(&state.agents).get_mut("s-c").unwrap().resumed_from = Some("conv-9".to_string());
    assert!(
        recents_of(&state, &ws).await.is_empty(),
        "resumed_from must hide the ancestor entry"
    );

    // It ends under its new id: the ancestor entry is superseded by the
    // continuation — one entry, newest title, resumable via the NEW id.
    let continuation = store.join("conv-10.jsonl");
    std::fs::write(&continuation, "{}\n").unwrap();
    {
        let mut agents_map = lock(&state.agents);
        let record = agents_map.get_mut("s-c").unwrap();
        record.ai_title = Some("hooks online v3".to_string());
        record.transcript_path = Some(continuation);
    }
    recents::retire(
        &state,
        "s-c",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );
    let entries = recents_of(&state, &ws).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["title"], "hooks online v3");
    assert_eq!(entries[0]["resume"], "conv-10");
}

/// GET /recents merges the daemon's own history with the claude
/// transcript store (the popover has no resume list of its own): daemon
/// entries win identity collisions, transcript-only conversations appear
/// as claude rows, live conversations are hidden from both sources.
#[tokio::test]
async fn recents_merge_daemon_history_with_transcript_store() {
    let store = test_dir("recents-merge-store");
    let state = test_state_with_claude_store(store.clone());
    let root = test_dir("recents-merge-root");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let ws_id = ws["id"].as_str().unwrap().to_string();
    let project_dir = store.join(launcher::encode_cwd(std::path::Path::new(
        ws["root"].as_str().unwrap(),
    )));
    std::fs::create_dir_all(&project_dir).unwrap();

    // A conversation only the transcript store knows (ended before the
    // daemon existed, or claude run outside chimaera).
    write_transcript(
        &project_dir,
        "hist-1",
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"annotate the variants"}}"#,
            "\n",
        ),
        500,
    );
    // A conversation BOTH know: transcript says one title, the daemon
    // watched it end with a newer one — the daemon entry must win.
    let both_path = project_dir.join("both-2.jsonl");
    write_transcript(
        &project_dir,
        "both-2",
        concat!(
            r#"{"type":"ai-title","aiTitle":"stale title","sessionId":"both-2"}"#,
            "\n"
        ),
        400,
    );
    plant_agent_record(
        &state,
        "s-both",
        &ws_id,
        agents::AgentKind::Claude,
        Some("fresh daemon title"),
        Some(both_path.to_str().unwrap()),
    );
    recents::retire(
        &state,
        "s-both",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );
    // And one daemon-only codex conversation (no transcript machinery).
    plant_agent_record(
        &state,
        "s-cdx",
        &ws_id,
        agents::AgentKind::Codex,
        None,
        None,
    );
    recents::retire(
        &state,
        "s-cdx",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );

    let entries = recents_of(&state, &ws_id).await;
    let titles: Vec<&str> = entries
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert_eq!(entries.len(), 3, "{entries:?}");
    // Newest first: the two just-retired daemon entries, then history.
    assert!(titles.contains(&"fresh daemon title"), "{titles:?}");
    assert!(titles.contains(&"codex"), "{titles:?}");
    assert_eq!(*titles.last().unwrap(), "annotate the variants");
    assert!(!titles.contains(&"stale title"), "daemon entry must win");
    let hist = entries.last().unwrap();
    assert_eq!(hist["kind"], "claude");
    assert_eq!(hist["resume"], "hist-1");
    assert!(hist["last_active"].as_u64().unwrap() > 0);

    // A live session on the transcript-only conversation hides it too.
    plant_agent_record(
        &state,
        "s-live",
        &ws_id,
        agents::AgentKind::Claude,
        None,
        Some(project_dir.join("hist-1.jsonl").to_str().unwrap()),
    );
    let entries = recents_of(&state, &ws_id).await;
    assert!(
        !entries.iter().any(|e| e["resume"] == "hist-1"),
        "{entries:?}"
    );
    crate::lock(&state.agents).remove("s-live");

    // Resume-then-end: claude forks a new session id and the ANCESTOR
    // transcript stays on disk in the scanned dir. The superseded
    // ancestor must NOT resurrect from the scan — clicking it would
    // fork the conversation from its pre-resume state (review blocker).
    write_transcript(
        &project_dir,
        "both-2b",
        concat!(
            r#"{"type":"ai-title","aiTitle":"fresh daemon title v2","sessionId":"both-2b"}"#,
            "\n",
        ),
        10,
    );
    plant_agent_record(
        &state,
        "s-resumed",
        &ws_id,
        agents::AgentKind::Claude,
        Some("fresh daemon title v2"),
        Some(project_dir.join("both-2b.jsonl").to_str().unwrap()),
    );
    crate::lock(&state.agents)
        .get_mut("s-resumed")
        .unwrap()
        .resumed_from = Some("both-2".to_string());
    recents::retire(
        &state,
        "s-resumed",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );

    let entries = recents_of(&state, &ws_id).await;
    let lineage: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|e| e["resume"] == "both-2" || e["resume"] == "both-2b")
        .collect();
    assert_eq!(lineage.len(), 1, "ancestor resurrected: {entries:?}");
    assert_eq!(lineage[0]["resume"], "both-2b");
    assert_eq!(lineage[0]["title"], "fresh daemon title v2");
}

/// PATCH /sessions/{id} pins a display name for ANY session kind — the
/// chimaera-owned rename (claude's /rename flows via OSC; codex, gemini,
/// agy, and shells have nothing, so the app must own it).
#[tokio::test]
async fn rename_session_pins_name_for_any_kind() {
    let state = test_state();
    let root = test_dir("rename-root");
    let (_, ws) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws["id"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let id = session["id"].as_str().unwrap().to_string();
    assert_eq!(session["renamed"], false);

    let (status, _) = request(
        &state,
        Method::PATCH,
        &format!("/api/v1/sessions/{id}"),
        Some(serde_json::json!({"name": "  qc pipeline  "})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let entry = session_entry(&state, &id).await;
    assert_eq!(entry["renamed"], true);
    assert_eq!(entry["name"], "qc pipeline"); // trimmed
                                              // The pin outranks every derived name.
    assert_eq!(entry["display_name"], "qc pipeline");

    // Guardrails: empty and unknown.
    let (status, _) = request(
        &state,
        Method::PATCH,
        &format!("/api/v1/sessions/{id}"),
        Some(serde_json::json!({"name": "   "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        &state,
        Method::PATCH,
        "/api/v1/sessions/s-nope",
        Some(serde_json::json!({"name": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    state.sessions.kill(&id).ok();
}

#[tokio::test]
async fn recents_unknown_workspace_is_404() {
    let (status, body) = request(
        &test_state(),
        Method::GET,
        "/api/v1/recents?workspace_id=w-nope",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}
