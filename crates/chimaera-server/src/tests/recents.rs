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

/// Other chat providers have no Claude transcript path: its resumable identity lives in
/// ChatInfo and must be carried across registry removal into Recents.
#[tokio::test]
async fn recents_retire_every_non_claude_chat_preserves_native_thread() {
    for kind in [
        agents::AgentKind::Codex,
        agents::AgentKind::Antigravity,
        agents::AgentKind::Grok,
    ] {
        let data_dir = test_dir(&format!("recents-{}-chat", kind.as_str()));
        let state = test_state_with_data_dir(0, data_dir.clone());
        let ws = make_workspace(&state, "recents-codex-chat-root").await;
        plant_agent_record(
            &state,
            "s-cdx-chat",
            &ws,
            kind,
            Some("continue the parser"),
            None,
        );

        recents::retire_with_resume(
            &state,
            "s-cdx-chat",
            None,
            None,
            chimaera_agent::model::SessionUi::Chat,
            Some("thread-cdx-42".to_string()),
        );

        let entries = recents_of(&state, &ws).await;
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0]["kind"], kind.as_str());
        assert_eq!(entries[0]["resume"], "thread-cdx-42");
        assert_eq!(entries[0]["ui"], "chat");

        // The native id is durable, not merely held in the live store.
        let reloaded = test_state_with_data_dir(0, data_dir);
        let entries = recents_of(&reloaded, &ws).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["resume"], "thread-cdx-42");
    }
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

    // Resumed live again: older claude CLIs fork a NEW session id on --resume, so
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

    // Resume-then-end on an older claude CLI: a new session id, and the ANCESTOR
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
fn archive_state(data: &std::path::Path, store: &std::path::Path) -> Arc<AppState> {
    let mut s = AppState::new(
        "test-token".into(),
        "testhost".into(),
        4242,
        0,
        data.to_path_buf(),
        data.join("config"),
    );
    s.claude_projects_dir = store.to_path_buf();
    Arc::new(s)
}

/// Archive only hides: a daemon-remembered conversation and one only the
/// claude store knows both leave Recents, stay listed as archived, survive a
/// restart, come back on unarchive, and nothing on disk is deleted. A
/// conversation resumed under a new id stays hidden (its chain is archived).
#[tokio::test]
async fn archiving_hides_recents_without_deleting_anything() {
    let store = test_dir("recents-archive-store");
    let data = test_dir("recents-archive-data");
    let state = archive_state(&data, &store);
    let root = test_dir("recents-archive-root");
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
    write_transcript(
        &project_dir,
        "store-only",
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"annotate the variants"}}"#,
            "\n",
        ),
        500,
    );
    let kept = project_dir.join("kept-1.jsonl");
    write_transcript(
        &project_dir,
        "kept-1",
        concat!(
            r#"{"type":"ai-title","aiTitle":"t","sessionId":"kept-1"}"#,
            "\n"
        ),
        400,
    );
    plant_agent_record(
        &state,
        "s-kept",
        &ws_id,
        agents::AgentKind::Claude,
        Some("normalize the counts"),
        Some(kept.to_str().unwrap()),
    );
    recents::retire(
        &state,
        "s-kept",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );
    let rows = recents_of(&state, &ws_id).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().all(|r| r["key"].is_string()));
    let epoch = state
        .recents_epoch
        .load(std::sync::atomic::Ordering::Relaxed);

    // "Archive all": both rows.
    let entries: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({"key": r["key"], "kind": r["kind"], "title": r["title"],
                "resume": r["resume"], "ui": r["ui"]})
        })
        .collect();
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/recents/archive",
        Some(serde_json::json!({"workspace_id": ws_id, "entries": entries})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["archived"].as_array().unwrap().len(), 2);
    assert!(recents_of(&state, &ws_id).await.is_empty());
    assert!(
        state
            .recents_epoch
            .load(std::sync::atomic::Ordering::Relaxed)
            > epoch,
        "every window hears about it"
    );
    // Nothing was deleted.
    assert!(kept.is_file());
    assert!(project_dir.join("store-only.jsonl").is_file());

    // Listed as archived; and still archived after a restart.
    let (_, archived) = request(
        &state,
        Method::GET,
        &format!("/api/v1/recents/archived?workspace_id={ws_id}"),
        None,
    )
    .await;
    assert_eq!(archived["archived"].as_array().unwrap().len(), 2);
    let reloaded = archive_state(&data, &store);
    assert!(
        recents_of(&reloaded, &ws_id).await.is_empty(),
        "a claude-store conversation stays archived across a restart"
    );

    // Resumed under a newer id (older CLIs fork one): still hidden.
    write_transcript(&project_dir, "kept-2", "{}\n", 10);
    plant_agent_record(
        &reloaded,
        "s-next",
        &ws_id,
        agents::AgentKind::Claude,
        Some("normalize the counts, again"),
        Some(project_dir.join("kept-2.jsonl").to_str().unwrap()),
    );
    lock(&reloaded.agents)
        .get_mut("s-next")
        .unwrap()
        .resumed_from = Some("kept-1".into());
    recents::retire(
        &reloaded,
        "s-next",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );
    assert!(recents_of(&reloaded, &ws_id).await.is_empty());

    // Unarchive (the Undo, or All sessions' action) brings one back.
    let (status, body) = request(
        &reloaded,
        Method::POST,
        "/api/v1/recents/unarchive",
        Some(serde_json::json!({"workspace_id": ws_id, "keys": ["store-only"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["unarchived"], 1);
    let rows = recents_of(&reloaded, &ws_id).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["key"], "store-only");

    // Deleting the workspace drops its archive.
    let (status, _) = request(
        &reloaded,
        Method::DELETE,
        &format!("/api/v1/workspaces/{ws_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(lock(&reloaded.recents_archive).hidden(&ws_id).is_empty());
}

#[cfg(unix)]
/// Plant `body` at `path` and make it unreadable (mode 000). False when the
/// process can read it anyway (root), so a test can skip rather than lie.
pub(super) fn unreadable_file(path: &std::path::Path, body: &[u8]) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::read(path).is_err()
}

#[cfg(unix)]
/// Give a test's unreadable file back its mode, so it can be read and removed.
pub(super) fn readable_again(path: &std::path::Path) -> Vec<u8> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::read(path).unwrap()
}

/// Valid JSON (`body`) padded with whitespace to one byte past `cap`: it
/// would parse, so only the cap can be what turned it away.
pub(super) fn oversized(body: &str, cap: usize) -> Vec<u8> {
    let mut bytes = body.as_bytes().to_vec();
    bytes.resize(cap + 1, b' ');
    bytes
}

fn recent(title: &str) -> recents::RecentEntry {
    recents::RecentEntry {
        kind: agents::AgentKind::Claude,
        title: title.to_string(),
        resume: Some(format!("id-{title}")),
        supersedes: Vec::new(),
        last_active: 1,
        ui: None,
        transcript: None,
        cwd: None,
    }
}

#[test]
fn recents_store_missing_file_is_empty_and_round_trips() {
    let path = test_dir("recents-missing").join("recents.json");
    let mut store = recents::RecentsStore::load(path.clone());
    assert!(store.list("w").is_empty());
    store.push("w", recent("a"), None);
    let reloaded = recents::RecentsStore::load(path);
    assert_eq!(reloaded.list("w")[0].title, "a");
}

/// Over the cap: an empty history (logged), and the next change replaces
/// the runaway file with a real one.
#[test]
fn recents_store_oversized_file_loads_empty_and_is_replaced() {
    let path = test_dir("recents-big").join("recents.json");
    let body = serde_json::json!({"w": [{"kind": "claude", "title": "old", "last_active": 1}]});
    std::fs::write(
        &path,
        oversized(&body.to_string(), recents::MAX_RECENTS_BYTES),
    )
    .unwrap();
    let mut store = recents::RecentsStore::load(path.clone());
    assert!(store.list("w").is_empty());
    store.push("w", recent("new"), None);
    assert!(std::fs::metadata(&path).unwrap().len() < 4096);
    let reloaded = recents::RecentsStore::load(path);
    assert_eq!(reloaded.list("w").len(), 1);
    assert_eq!(reloaded.list("w")[0].title, "new");
}

#[cfg(unix)]
/// Unreadable (a directory at the path, or a file without read
/// permission): the daemon still serves recents from memory, but never
/// writes over what it couldn't read — until it reads again: then the next
/// push merges the file's rows with this daemon's and lands.
#[test]
fn recents_store_unreadable_file_is_held_until_it_reads_then_merged() {
    let dir = test_dir("recents-dir").join("recents.json");
    std::fs::create_dir_all(&dir).unwrap();
    let mut store = recents::RecentsStore::load(dir.clone());
    store.push("w", recent("a"), None);
    assert_eq!(store.list("w").len(), 1);
    assert!(dir.is_dir());

    let path = test_dir("recents-perm").join("recents.json");
    let body = br#"{"w":[{"kind":"claude","title":"kept","resume":"id-kept","last_active":1}]}"#;
    if !unreadable_file(&path, body) {
        return; // root reads anything: nothing to prove here
    }
    let mut store = recents::RecentsStore::load(path.clone());
    assert!(store.list("w").is_empty());
    store.push("w", recent("new"), None);
    store.push("w", recent("newer"), None);
    assert_eq!(store.list("w").len(), 2);
    assert_eq!(
        readable_again(&path),
        body.to_vec(),
        "held: never written over"
    );

    store.push("w", recent("latest"), None);
    let titles = |s: &recents::RecentsStore| -> Vec<String> {
        s.list("w").into_iter().map(|e| e.title).collect()
    };
    assert_eq!(titles(&store), ["latest", "newer", "new", "kept"]);
    let reloaded = recents::RecentsStore::load(path);
    assert_eq!(titles(&reloaded), ["latest", "newer", "new", "kept"]);
}

#[cfg(unix)]
/// The archive: unreadable or oversized is held (replacing it would un-hide
/// every conversation in it) and served from memory; once the file reads,
/// the next change merges with it and lands — minus what was unarchived
/// meanwhile.
#[tokio::test]
async fn recents_archive_holds_an_unreadable_or_oversized_file_then_merges() {
    let entry = |key: &str| {
        serde_json::json!({"workspace_id": "", "entries": [
            {"key": key, "kind": "claude", "title": "t"}]})
    };
    let archive = |state: Arc<AppState>, ws: String, key: &'static str| {
        let mut body = entry(key);
        body["workspace_id"] = ws.into();
        async move { request(&state, Method::POST, "/api/v1/recents/archive", Some(body)).await }
    };
    let archived = |state: Arc<AppState>, ws: String| async move {
        let (_, listed) = request(
            &state,
            Method::GET,
            &format!("/api/v1/recents/archived?workspace_id={ws}"),
            None,
        )
        .await;
        let mut keys: Vec<String> = listed["archived"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["key"].as_str().unwrap().to_string())
            .collect();
        keys.sort();
        keys
    };

    // Oversized: held, never replaced.
    let data = test_dir("archive-big");
    let file = data.join("recents-archive.json");
    let big = oversized(r#"{"w":[]}"#, recents_archive::MAX_ARCHIVE_BYTES);
    std::fs::write(&file, &big).unwrap();
    let state = test_state_with_data_dir(0, data.clone());
    let ws = make_workspace(&state, "archive-big-root").await;
    assert_eq!(
        archive(state.clone(), ws.clone(), "k-1").await.0,
        StatusCode::OK
    );
    assert_eq!(archived(state.clone(), ws.clone()).await, ["k-1"]);
    assert_eq!(std::fs::metadata(&file).unwrap().len(), big.len() as u64);

    // Unreadable: held; then readable, and the next change merges.
    let data = test_dir("archive-perm");
    let state = test_state_with_data_dir(0, data.clone());
    let ws = make_workspace(&state, "archive-perm-root").await;
    drop(state);
    let file = data.join("recents-archive.json");
    let body = serde_json::json!({ws.clone(): [
        {"key": "old-1", "kind": "claude", "title": "o", "at": 1},
        {"key": "old-2", "kind": "claude", "title": "o", "at": 1}]})
    .to_string();
    if !unreadable_file(&file, body.as_bytes()) {
        return; // root reads anything
    }
    let state = test_state_with_data_dir(0, data.clone());
    assert_eq!(
        archive(state.clone(), ws.clone(), "k-1").await.0,
        StatusCode::OK
    );
    assert_eq!(archived(state.clone(), ws.clone()).await, ["k-1"]);
    // Unarchived while held: the merge must not hide it again.
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/recents/unarchive",
        Some(serde_json::json!({"workspace_id": ws, "keys": ["old-2"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(readable_again(&file), body.as_bytes().to_vec(), "held");

    assert_eq!(
        archive(state.clone(), ws.clone(), "k-2").await.0,
        StatusCode::OK
    );
    assert_eq!(
        archived(state.clone(), ws.clone()).await,
        ["k-1", "k-2", "old-1"]
    );
    let reloaded = test_state_with_data_dir(0, data);
    assert_eq!(archived(reloaded, ws).await, ["k-1", "k-2", "old-1"]);
}
