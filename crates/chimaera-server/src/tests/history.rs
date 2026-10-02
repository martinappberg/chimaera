//! Session history against a real `AppState`: a record opens and closes
//! with its session, carries who started it, survives a daemon that died
//! (closed `retired` at the next boot, the resurrected session under a new
//! record), lists through the route, audits the Mastermind's acts, warns two
//! sessions writing the same file, extracts edits, and totals cost.

use super::support::*;
use crate::history::{self, Line, Outcome, StartedBy};
use crate::*;

fn lines(state: &Arc<AppState>, ws: &str) -> Vec<Line> {
    state.history.flush(std::time::Duration::from_secs(5));
    history::read_lines(&state.history.path(ws))
}

fn records(state: &Arc<AppState>, ws: &str) -> Vec<history::Record> {
    history::merge(lines(state, ws)).records
}

#[tokio::test]
async fn a_record_opens_and_closes_with_its_session() {
    let state = test_state();
    let ws = make_workspace(&state, "history-lifecycle").await;
    let root = lock(&state.workspaces).get(&ws).unwrap().root;
    let sid = "s-hist-1";
    plant_agent_record(
        &state,
        sid,
        &ws,
        agents::AgentKind::Claude,
        Some("Fix the QC filter"),
        Some("/tmp/does-not-exist/abc123.jsonl"),
    );
    history::open(&state, sid, StartedBy::Mastermind);
    let opened = records(&state, &ws);
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].started_by, "mastermind");
    assert_eq!(opened[0].ended, None);

    // A claude TUI: the statusline paints 0 before the first prompt, then
    // the running total grows.
    history::observe_statusline(
        &state,
        sid,
        &serde_json::json!({"model": {"display_name": "Opus"}, "cost": {"total_cost_usd": 0.0}}),
    );
    history::observe_hook(&state, sid, "UserPromptSubmit", None, None);
    let qc = root.join("src/qc.py").to_string_lossy().into_owned();
    history::observe_hook(&state, sid, "PostToolUse", None, Some(&qc));
    history::observe_statusline(
        &state,
        sid,
        &serde_json::json!({
            "model": {"display_name": "Opus"},
            "cost": {"total_cost_usd": 0.42},
            "context_window": {"total_input_tokens": 1200, "total_output_tokens": 300},
        }),
    );
    {
        let mut agents = lock(&state.agents);
        let r = agents.get_mut(sid).unwrap();
        r.first_prompt = Some("tighten   the QC\nfilter".into());
        r.touch_file(&qc);
    }

    recents::retire(
        &state,
        sid,
        None,
        None,
        chimaera_agent::model::SessionUi::Term,
    );
    let closed = records(&state, &ws);
    assert_eq!(closed.len(), 1, "open and close merge into one record");
    let rec = &closed[0];
    assert_eq!(rec.outcome, Some(Outcome::Exited));
    assert!(rec.ended.is_some());
    assert_eq!(rec.title.as_deref(), Some("Fix the QC filter"));
    assert_eq!(rec.first_prompt.as_deref(), Some("tighten the QC filter"));
    assert_eq!(rec.models, ["Opus"]);
    assert_eq!(rec.usage.cost_usd, Some(0.42));
    assert_eq!(rec.usage.tokens_in, Some(1200));
    assert_eq!(rec.usage.tokens_out, Some(300));
    assert_eq!(rec.usage.turns, Some(1));
    assert_eq!(rec.files.n, 1);
    assert_eq!(rec.files.top, ["src/qc.py"], "workspace-relative");
    let t = rec.transcript.as_ref().unwrap();
    assert_eq!(t.kind, "claude");
    assert_eq!(t.native.as_deref(), Some("abc123"));
    assert_eq!(rec.git, None, "no git anchors until Part 1 fills the seam");

    // The route lists it, and says plainly the transcript is gone.
    let (status, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/history"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = &body["records"][0];
    assert_eq!(row["id"], sid);
    assert_eq!(row["live"], false);
    assert!(row["reopen"]["resume"].is_null());
    assert!(
        row["reopen"]["gone"]
            .as_str()
            .unwrap()
            .contains("cleanupPeriodDays"),
        "{row}"
    );
    assert!(row.get("totals").is_none(), "raw totals stay off the wire");

    // Search and the agent filter.
    let (_, hit) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/history?q=qc%20filter"),
        None,
    )
    .await;
    assert_eq!(hit["records"].as_array().unwrap().len(), 1);
    let (_, miss) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/history?agent=codex"),
        None,
    )
    .await;
    assert!(miss["records"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_crash_is_recorded_until_the_session_recovers() {
    let state = test_state();
    let ws = make_workspace(&state, "history-crash").await;
    plant_agent_record(&state, "s-crash", &ws, agents::AgentKind::Codex, None, None);
    history::open(&state, "s-crash", StartedBy::You);
    history::observe_exit(
        &state,
        "s-crash",
        &chimaera_agent::driver::DriverExit::ProtocolError("garbled".into()),
    );
    recents::retire(
        &state,
        "s-crash",
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );
    let rec = &records(&state, &ws)[0];
    assert_eq!(rec.outcome, Some(Outcome::Crashed));
    // A codex TUI-shaped record reports nothing: unknown, never zero.
    assert_eq!(rec.usage.cost_usd, None);
    assert_eq!(rec.usage.tokens_in, None);
}

#[tokio::test]
async fn a_daemon_that_died_closes_its_records_at_the_next_boot() {
    let data = test_dir("history-boot");
    let state = test_state_with_data_dir(0, data.clone());
    let ws = make_workspace(&state, "history-boot-ws").await;
    plant_agent_record(
        &state,
        "s-boot",
        &ws,
        agents::AgentKind::Claude,
        Some("long run"),
        None,
    );
    history::open(&state, "s-boot", StartedBy::You);
    history::observe_statusline(
        &state,
        "s-boot",
        &serde_json::json!({"cost": {"total_cost_usd": 0.0}}),
    );
    history::observe_hook(&state, "s-boot", "UserPromptSubmit", None, None);
    history::observe_statusline(
        &state,
        "s-boot",
        &serde_json::json!({"cost": {"total_cost_usd": 1.5}}),
    );
    // The checkpoint the history task writes every 30 s; then the daemon
    // "dies" (no graceful stop, nothing closed).
    state.history.checkpoint();
    state.history.flush(std::time::Duration::from_secs(5));

    let state2 = test_state_with_data_dir(0, data);
    let ws2 = lock(&state2.workspaces).get(&ws).unwrap().id;
    history::boot_close(&state2).await;
    let recs = records(&state2, &ws2);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].outcome, Some(Outcome::Retired));
    assert!(recs[0].ended.is_some());
    assert_eq!(
        recs[0].usage.cost_usd,
        Some(1.5),
        "the checkpointed usage survives"
    );
    assert_eq!(
        recs[0].transcript.as_ref().map(|t| t.kind.as_str()),
        Some("claude"),
        "and so does where the conversation lives"
    );

    // The resurrected session continues under a NEW record.
    plant_agent_record(
        &state2,
        "s-boot",
        &ws2,
        agents::AgentKind::Claude,
        Some("long run"),
        None,
    );
    history::open(&state2, "s-boot", StartedBy::Restart);
    let recs = records(&state2, &ws2);
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[1].started_by, "restart");
    assert_ne!(recs[0].rid, recs[1].rid);

    // A second boot finds nothing left over (the checkpoint was cleared).
    history::boot_close(&state2).await;
    assert_eq!(records(&state2, &ws2).len(), 2);
}

#[tokio::test]
async fn a_graceful_stop_retires_every_open_record() {
    let state = test_state();
    let ws = make_workspace(&state, "history-stop").await;
    for sid in ["s-stop-a", "s-stop-b"] {
        plant_agent_record(&state, sid, &ws, agents::AgentKind::Claude, Some(sid), None);
        history::open(&state, sid, StartedBy::You);
    }
    history::close_all_for_exit(&state);
    let recs = records(&state, &ws);
    assert_eq!(recs.len(), 2);
    assert!(recs.iter().all(|r| r.outcome == Some(Outcome::Retired)));
}

#[tokio::test]
async fn a_resurrected_chat_opens_a_restart_record() {
    let data = test_dir("history-resurrect");
    let state = test_state_with_data_dir(0, data);
    let ws = make_workspace(&state, "history-resurrect-ws").await;
    let root = lock(&state.workspaces).get(&ws).unwrap().root;
    preset_agent(
        &state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude("history-resurrect-fake")),
        Some("9.9.9-fake"),
    );
    let boot = ledger::BootLedger {
        sessions: vec![ledger::LedgerEntry {
            id: "s-back".to_string(),
            workspace_id: ws.clone(),
            cwd: root,
            pinned_name: None,
            cols: 120,
            rows: 40,
            theme: "dark".to_string(),
            created_at: 0,
            agent: Some(ledger::LedgerAgent {
                kind: agents::AgentKind::Claude,
                resume: None,
                transcript: None,
                title: "claude".to_string(),
                ui: chimaera_agent::model::SessionUi::Chat,
                model: None,
                carryover: None,
            }),
        }],
        links: std::collections::HashMap::new(),
        written_at: 0,
    };
    state.restored.send_replace(false);
    ledger::consume_boot(&state, boot).await;
    assert!(state.chat.contains("s-back"));
    let recs = records(&state, &ws);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].started_by, "restart");
    assert_eq!(recs[0].ui, "chat");

    // A fresh chat from the launcher is the user's.
    let (status, row) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws, "kind": "agent", "ui": "chat"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    let fresh = row["id"].as_str().unwrap().to_string();
    let recs = records(&state, &ws);
    let rec = recs.iter().find(|r| r.id == fresh).unwrap();
    assert_eq!(rec.started_by, "you");

    // Live records list at the top, flagged.
    let (_, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/history"),
        None,
    )
    .await;
    assert_eq!(body["records"][0]["live"], true);
    assert!(body["records"][0]["reopen"].is_null());

    state.chat.kill("s-back");
    state.chat.kill(&fresh);
}

#[tokio::test]
async fn mastermind_acts_land_in_the_audit_trail() {
    let state = test_state();
    let ws = make_workspace(&state, "history-acts").await;
    history::act(
        &state,
        &ws,
        "s-mm",
        "message_agent",
        Some("s-worker"),
        Some("re-run   DE\nwithout S3"),
    );
    history::act(&state, &ws, "you", "deliver_note", Some("s-mm"), None);
    // Acts are queued to the background writer; the route reads durable
    // history. Synchronize those writes before asserting the route's result.
    state.history.flush(std::time::Duration::from_secs(5));
    let (_, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/history?acts=true"),
        None,
    )
    .await;
    let acts = body["acts"].as_array().unwrap();
    assert_eq!(acts.len(), 2);
    assert_eq!(acts[0]["act"], "deliver_note", "newest first");
    assert_eq!(acts[1]["detail"], "re-run DE without S3");
    // Without acts=true the list carries none.
    let (_, plain) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/history"),
        None,
    )
    .await;
    assert!(plain["acts"].is_null());
}

#[tokio::test]
async fn two_sessions_writing_one_file_hear_about_each_other_once() {
    let state = test_state();
    let ws = make_workspace(&state, "history-same-file").await;
    let a = inject_silent_agent(&state, "ka");
    let b = inject_silent_agent(&state, "kb");
    for (sid, name) in [(&a, "fix normalization"), (&b, "plot the QC")] {
        lock(&state.session_workspaces).insert(sid.clone(), ws.clone());
        lock(&state.agents)
            .get_mut(sid.as_str())
            .unwrap()
            .custom_title = Some(name.into());
        history::open(&state, sid, StartedBy::You);
    }
    let write = |path: &str| {
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Write",
            "tool_input": {"file_path": path, "content": "x"},
        })
    };
    let hook = |sid: &str, key: &str, payload: serde_json::Value| {
        let state = state.clone();
        let uri = format!("/api/v1/agent-events/{sid}?key={key}");
        async move { request(&state, Method::POST, &uri, Some(payload)).await }
    };

    // No overlap: the answer is exactly what it always was.
    let (status, body) = hook(&a, "ka", write("/w/qc.py")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({}));
    let (_, body) = hook(&b, "kb", write("/w/other.py")).await;
    assert_eq!(body, serde_json::json!({}));

    // B writes the same file: B hears about A.
    let (_, body) = hook(&b, "kb", write("/w/qc.py")).await;
    let ctx = body["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("no context: {body}"));
    assert_eq!(body["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    assert!(ctx.contains("'fix normalization'"), "{ctx}");
    assert!(ctx.contains("/w/qc.py"), "{ctx}");
    // Once per file per pair.
    let (_, body) = hook(&b, "kb", write("/w/qc.py")).await;
    assert_eq!(body, serde_json::json!({}));
    // A hears about B on its next hook, whatever the tool.
    let (_, body) = hook(
        &a,
        "ka",
        serde_json::json!({"hook_event_name": "PostToolUse", "tool_name": "Bash",
            "tool_input": {"command": "ls"}}),
    )
    .await;
    let ctx = body["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("no context: {body}"));
    assert!(ctx.contains("'plot the QC'"), "{ctx}");

    // The UI's notice reads the same pairs, both directions, with times.
    let (status, body) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/same-file"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pairs = body["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 2, "{body}");
    assert!(pairs
        .iter()
        .all(|p| p["path"] == "/w/qc.py" && p["at"].as_u64().unwrap() > 0));
    assert!(pairs
        .iter()
        .any(|p| p["session"] == a.as_str() && p["other"] == b.as_str()));

    state.sessions.kill(&a).ok();
    state.sessions.kill(&b).ok();
}

#[tokio::test]
async fn edits_come_from_the_chat_journal_live_or_ended() {
    let state = test_state();
    let ws = make_workspace(&state, "history-edits").await;
    let sid = "s-edits";
    plant_agent_record(&state, sid, &ws, agents::AgentKind::Codex, None, None);
    let journal = state.chat.journal_dir().join(format!("{sid}.jsonl"));
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let rows = [
        serde_json::json!({"seq": 1, "ts": 10, "ev": {"type": "tool_call", "id": "e1",
            "kind": "edit", "title": "Edit qc.py", "status": "in_progress"}}),
        serde_json::json!({"seq": 2, "ts": 11, "ev": {"type": "tool_call_update", "id": "e1",
            "status": "completed", "content": {"kind": "diff", "path": "/w/qc.py",
            "old_text": "mad = 3", "new_text": "mad = 2.5"}}}),
    ];
    let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
    std::fs::write(&journal, body).unwrap();

    let (status, out) = request(
        &state,
        Method::GET,
        &format!("/api/v1/sessions/{sid}/edits"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["source"], "chat");
    assert_eq!(out["files"][0]["path"], "/w/qc.py");
    assert_eq!(out["files"][0]["edits"][0]["old_text"], "mad = 3");

    // Ended: the record points the way.
    history::open(&state, sid, StartedBy::You);
    recents::retire(
        &state,
        sid,
        None,
        None,
        chimaera_agent::model::SessionUi::Chat,
    );
    state.history.flush(std::time::Duration::from_secs(5));
    let (status, out) = request(
        &state,
        Method::GET,
        &format!("/api/v1/sessions/{sid}/edits?workspace_id={ws}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["edits"], 1);

    // No record, no journal: 404.
    let (status, _) = request(
        &state,
        Method::GET,
        &format!("/api/v1/sessions/s-nobody/edits?workspace_id={ws}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn usage_totals_span_workspaces_and_export_as_csv() {
    let state = test_state();
    let ws1 = make_workspace(&state, "history-usage-1").await;
    let ws2 = make_workspace(&state, "history-usage-2").await;
    for (ws, sid, cost) in [(&ws1, "s-u1", 0.75), (&ws2, "s-u2", 0.25)] {
        plant_agent_record(&state, sid, ws, agents::AgentKind::Claude, Some(sid), None);
        history::open(&state, sid, StartedBy::You);
        history::observe_statusline(
            &state,
            sid,
            &serde_json::json!({"cost": {"total_cost_usd": 0.0}}),
        );
        history::observe_hook(&state, sid, "UserPromptSubmit", None, None);
        history::observe_statusline(
            &state,
            sid,
            &serde_json::json!({"cost": {"total_cost_usd": cost}}),
        );
        recents::retire(
            &state,
            sid,
            None,
            None,
            chimaera_agent::model::SessionUi::Term,
        );
    }
    // A codex TUI: no telemetry — unknown, never zero.
    plant_agent_record(&state, "s-u3", &ws1, agents::AgentKind::Codex, None, None);
    history::open(&state, "s-u3", StartedBy::You);
    state.history.flush(std::time::Duration::from_secs(5));

    let (status, all) = request(&state, Method::GET, "/api/v1/activity", None).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(all["basis"], "estimated at API prices");
    assert_eq!(all["totals"]["sessions"], 3);
    assert_eq!(all["totals"]["cost_usd"], 1.0);
    assert_eq!(all["totals"]["unknown_cost_sessions"], 1);
    assert_eq!(all["today"]["sessions"], 3);
    assert_eq!(all["workspaces"].as_array().unwrap().len(), 2);

    let (_, one) = request(
        &state,
        Method::GET,
        &format!("/api/v1/activity?workspace_id={ws2}"),
        None,
    )
    .await;
    assert_eq!(one["totals"]["cost_usd"], 0.25);

    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/activity/csv")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/csv"));
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert_eq!(text.lines().count(), 4, "{text}");

    // Every route is bearer-authed.
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/activity")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn read_session_reads_an_ended_sessions_record() {
    let state = test_state();
    let ws = make_workspace(&state, "history-mm-read").await;
    plant_agent_record(
        &state,
        "s-gone",
        &ws,
        agents::AgentKind::Claude,
        Some("port the parser"),
        None,
    );
    history::open(&state, "s-gone", StartedBy::You);
    recents::retire(
        &state,
        "s-gone",
        None,
        None,
        chimaera_agent::model::SessionUi::Term,
    );
    state.history.flush(std::time::Duration::from_secs(5));
    let text = history::routes::ended_session_text(&state, &ws, "s-gone")
        .await
        .unwrap();
    assert!(text.contains("has ended"), "{text}");
    assert!(text.contains("port the parser"), "{text}");
    assert!(
        text.contains("cost —"),
        "unknown cost reads as a dash: {text}"
    );
    assert!(history::routes::ended_session_text(&state, &ws, "s-never")
        .await
        .is_none());
}

#[tokio::test]
async fn deleting_a_workspace_deletes_its_history() {
    let state = test_state();
    let ws = make_workspace(&state, "history-delete").await;
    history::act(&state, &ws, "you", "deliver_note", None, None);
    state.history.flush(std::time::Duration::from_secs(5));
    let path = state.history.path(&ws);
    assert!(path.exists());
    let (status, _) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/workspaces/{ws}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    history::act(&state, &ws, "you", "racing the delete", None, None);
    state.history.flush(std::time::Duration::from_secs(5));
    assert!(
        !path.exists(),
        "nothing resurrects a deleted workspace's file"
    );
}

#[tokio::test]
async fn acp_turns_without_usage_do_not_claim_zero_tokens() {
    use chimaera_agent::model::{AgentEvent, Usage};
    let state = test_state();
    let ws = make_workspace(&state, "history-acp-usage").await;
    for kind in [agents::AgentKind::Antigravity, agents::AgentKind::Grok] {
        let sid = format!("s-{}-usage", kind.as_str());
        plant_agent_record(&state, &sid, &ws, kind, Some("Usage check"), None);
        history::open(&state, &sid, StartedBy::You);
        history::observe_chat(
            &state,
            &sid,
            &AgentEvent::TurnCompleted {
                turn_id: "turn-1".into(),
                usage: Usage::default(),
            },
        );
    }
    let rows = records(&state, &ws);
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row.usage.tokens_in, None);
        assert_eq!(row.usage.tokens_out, None);
    }
}
