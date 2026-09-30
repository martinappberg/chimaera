//! Agent communication end to end (plan: docs/agent-communication-plan.md):
//! messages on the Timeline, carried to claude on its next hook exactly
//! once, broadcasts, the wake policy for idle chats (ask / never / auto,
//! and a reply the reader asked for), the user's hand-over and wake
//! answers, the chat journal annotation, and the switch.

use super::support::*;
use crate::*;

/// Both settings at once (the settings PUT replaces the whole map).
async fn set_comms(state: &Arc<AppState>, enabled: bool, wakes: &str) {
    let (status, body) = request(
        state,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({
            "agents.communication.enabled": enabled,
            "agents.communication.wakes": wakes,
        })),
    )
    .await;
    assert!(status.is_success(), "{status} {body}");
}

/// A claude terminal agent that never writes (it reads as idle), in `ws`.
fn tui(state: &Arc<AppState>, ws: &str, key: &str) -> String {
    let sid = inject_silent_agent(state, key);
    lock(&state.session_workspaces).insert(sid.clone(), ws.to_string());
    sid
}

/// A scripted claude chat session in `ws` (idle: the fake never starts a
/// turn) and its MCP/hook key.
async fn chat(state: &Arc<AppState>, ws: &str, label: &str) -> (String, String) {
    preset_agent(
        state,
        agents::AgentKind::Claude,
        Ok(write_fake_claude(label)),
        Some("9.9.9-fake"),
    );
    let (status, body) = request(
        state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": ws, "kind": "agent", "ui": "chat"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sid = body["id"].as_str().unwrap().to_string();
    let key = lock(&state.agents).get(&sid).unwrap().key.clone();
    (sid, key)
}

/// What a claude hook answered with as `additionalContext` ("" = nothing).
async fn hook(state: &Arc<AppState>, sid: &str, key: &str, event: &str) -> String {
    let (status, answer) = request(
        state,
        Method::POST,
        &format!("/api/v1/agent-events/{sid}?key={key}"),
        Some(serde_json::json!({"hook_event_name": event, "tool_name": "Bash", "prompt": "go"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

async fn send(
    state: &Arc<AppState>,
    from: &str,
    key: &str,
    args: serde_json::Value,
) -> (bool, String) {
    mcp_tool_call(state, from, key, "message_agent", args).await
}

async fn comms(state: &Arc<AppState>, ws: &str) -> serde_json::Value {
    let (status, body) = request(
        state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/comms"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Poll a chat session's journal until it holds `needle`.
async fn journal_has(state: &Arc<AppState>, sid: &str, needle: &str) -> String {
    let journal = state.chat.journal_dir().join(format!("{sid}.jsonl"));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let content = std::fs::read_to_string(&journal).unwrap_or_default();
        if content.contains(needle) {
            return content;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "journal never got {needle:?}: {content}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

fn journal(state: &Arc<AppState>, sid: &str) -> String {
    std::fs::read_to_string(state.chat.journal_dir().join(format!("{sid}.jsonl")))
        .unwrap_or_default()
}

/// A message is a Timeline entry first, then rides the reader's next hook
/// — once, even when claude fires two hooks at once (parallel tool calls).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_reaches_a_claude_terminal_on_its_next_hook_exactly_once() {
    let data = test_dir("comms-hook-data");
    let state = test_state_with_data_dir(0, data.clone());
    let ws = make_workspace(&state, "comms-hook").await;
    let a = tui(&state, &ws, "ka");
    let b = tui(&state, &ws, "kb");

    let (is_err, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": b, "text": "the loader returns Result now"}),
    )
    .await;
    assert!(!is_err, "{text}");
    assert!(text.starts_with("Sent (#"), "{text}");
    assert_eq!(comms(&state, &ws).await["unread"][&b], 1);

    let (_, page) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/timeline"),
        None,
    )
    .await;
    let note = &page["entries"][0]["note"];
    assert_eq!(note["from_sid"], a.as_str());
    assert_eq!(note["to"], b.as_str());
    assert_eq!(note["from_agent"], "claude");
    assert!(note["delivery"].is_string(), "{note}");

    let (one, two) = tokio::join!(
        hook(&state, &b, "kb", "PostToolUse"),
        hook(&state, &b, "kb", "PostToolUse"),
    );
    let carried: Vec<&String> = [&one, &two]
        .into_iter()
        .filter(|c| c.contains("the loader returns Result now"))
        .collect();
    assert_eq!(
        carried.len(),
        1,
        "exactly one hook carries it: {one:?} / {two:?}"
    );
    let context = carried[0];
    assert!(
        context.contains("> the loader returns Result now"),
        "quoted body: {context}"
    );
    assert!(
        context.contains(&format!("({a}, claude) to you")),
        "{context}"
    );
    assert_eq!(
        hook(&state, &b, "kb", "PostToolUse").await,
        "",
        "carried once"
    );
    assert!(comms(&state, &ws).await["unread"].get(&b).is_none());

    let (_, text) = mcp_tool_call(&state, &b, "kb", "read_messages", serde_json::json!({})).await;
    assert!(text.contains("No messages waiting"), "{text}");
    let (_, text) = mcp_tool_call(
        &state,
        &b,
        "kb",
        "read_messages",
        serde_json::json!({"all": true}),
    )
    .await;
    assert!(text.contains("the loader returns Result now"), "{text}");
    // The sender never hears its own message.
    assert_eq!(hook(&state, &a, "ka", "PostToolUse").await, "");

    // The read state is written beside the Timeline.
    let path = data.join("workspace");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let found = walk_for(&path, "comms.json");
        if let Some(file) = found {
            let body = std::fs::read_to_string(file).unwrap();
            if body.contains(&b) {
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "comms.json never written"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    for sid in [a, b] {
        state.sessions.kill(&sid).ok();
    }
}

fn walk_for(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = walk_for(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
    }
    None
}

/// workspace_agents names everyone else with an id and how a message reaches
/// them; addressing mistakes come back as words the model can act on.
#[tokio::test]
async fn workspace_agents_and_addressing() {
    let state = test_state();
    let ws = make_workspace(&state, "comms-list").await;
    let other_ws = make_workspace(&state, "comms-list-other").await;
    let a = tui(&state, &ws, "ka");
    let b = tui(&state, &ws, "kb");
    let outsider = tui(&state, &other_ws, "ko");

    let (is_err, text) =
        mcp_tool_call(&state, &a, "ka", "workspace_agents", serde_json::json!({})).await;
    assert!(!is_err, "{text}");
    assert!(text.starts_with(&format!("You are {a}")), "{text}");
    assert!(text.contains(&format!("- {b} ")), "{text}");
    assert!(
        !text.contains(&outsider),
        "other workspaces stay out: {text}"
    );
    assert!(text.contains("claude · terminal"), "{text}");

    for (args, needle) in [
        (
            serde_json::json!({"to": outsider, "text": "hi"}),
            "workspace_agents",
        ),
        (
            serde_json::json!({"to": a, "text": "hi"}),
            "that session is you",
        ),
        (
            serde_json::json!({"to": b, "text": "  "}),
            "missing required argument: text",
        ),
        (
            serde_json::json!({"to": b, "text": "x".repeat(3000)}),
            "under",
        ),
        (
            serde_json::json!({"to": b, "text": "hi", "reply_to": 999}),
            "no message #999",
        ),
        (
            serde_json::json!({"to": "mastermind", "text": "hi"}),
            "no Mastermind",
        ),
        (
            serde_json::json!({"text": "hi"}),
            "missing required argument: to",
        ),
    ] {
        let (is_err, text) = send(&state, &a, "ka", args).await;
        assert!(is_err && text.contains(needle), "{needle}: {text}");
    }

    for sid in [a, b, outsider] {
        state.sessions.kill(&sid).ok();
    }
}

/// "everyone" reaches every other agent's carrier, never the sender, and
/// never wakes an idle chat — even with wakes on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcasts_reach_everyone_but_never_wake() {
    let state = test_state();
    set_comms(&state, true, "auto").await;
    let ws = make_workspace(&state, "comms-all").await;
    let a = tui(&state, &ws, "ka");
    let b = tui(&state, &ws, "kb");
    let (c, _) = chat(&state, &ws, "comms-all-fake").await;

    let (is_err, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": "everyone", "text": "I'm changing the loader API"}),
    )
    .await;
    assert!(!is_err, "{text}");
    assert!(text.contains("to everyone"), "{text}");
    assert!(hook(&state, &b, "kb", "PostToolUse")
        .await
        .contains("to everyone"));
    assert_eq!(hook(&state, &a, "ka", "PostToolUse").await, "");
    assert_eq!(comms(&state, &ws).await["unread"][&c], 1);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !journal(&state, &c).contains("changing the loader API"),
        "a broadcast never starts a turn"
    );

    state.chat.kill(&c);
    for sid in [a, b] {
        state.sessions.kill(&sid).ok();
    }
}

/// An idle chat and the wake policy: "ask" puts a request in Needs you and
/// the user's Wake delivers; "never" leaves it for the user's hand-over;
/// "auto" wakes at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wake_policy_decides_what_an_idle_chat_gets() {
    let state = test_state();
    let ws = make_workspace(&state, "comms-wake").await;
    let a = tui(&state, &ws, "ka");
    let (c, _) = chat(&state, &ws, "comms-wake-fake").await;

    // Ask (the default).
    let (is_err, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": c, "text": "please rerun the QC"}),
    )
    .await;
    assert!(!is_err, "{text}");
    assert!(text.contains("the user was asked"), "{text}");
    let body = comms(&state, &ws).await;
    let requests = body["wake_requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1, "{body}");
    assert_eq!(requests[0]["to_sid"], c.as_str());
    assert_eq!(requests[0]["reason"], "ask");
    let wid = requests[0]["id"].as_str().unwrap().to_string();
    assert!(!journal(&state, &c).contains("please rerun the QC"));
    let (status, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/comms/wakes/{wid}"),
        Some(serde_json::json!({"wake": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["delivered"], 1);
    let content = journal_has(&state, &c, "please rerun the QC").await;
    assert!(content.contains("\"origin\":\"agent\""), "{content}");
    let body = comms(&state, &ws).await;
    assert!(
        body["wake_requests"].as_array().unwrap().is_empty(),
        "{body}"
    );
    assert!(body["unread"].get(&c).is_none(), "{body}");

    // The fake never ends the turn the wake opened; a real one would.
    lock(&state.agents).get_mut(&c).unwrap().state = agent_state::AgentState::Finished;

    // Never: it waits for the user's hand-over.
    set_comms(&state, true, "never").await;
    let (_, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": c, "text": "the batch finished"}),
    )
    .await;
    assert!(text.contains("inbox"), "{text}");
    assert!(comms(&state, &ws).await["wake_requests"]
        .as_array()
        .unwrap()
        .is_empty());
    let (status, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/comms/deliver"),
        Some(serde_json::json!({"session": c})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    journal_has(&state, &c, "the batch finished").await;
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/comms/deliver"),
        Some(serde_json::json!({"session": c})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "nothing left to hand over");

    // Auto: woken straight away.
    lock(&state.agents).get_mut(&c).unwrap().state = agent_state::AgentState::Finished;
    set_comms(&state, true, "auto").await;
    let (_, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": c, "text": "figures are in figs/"}),
    )
    .await;
    assert!(text.contains("started a turn"), "{text}");
    journal_has(&state, &c, "figures are in figs/").await;

    state.chat.kill(&c);
    state.sessions.kill(&a).ok();
}

/// A reply to a question the reader asked (`expect_reply`) wakes it even
/// when the policy asks first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_the_asker_wanted_wakes_it_without_asking() {
    let state = test_state();
    let ws = make_workspace(&state, "comms-reply").await;
    let b = tui(&state, &ws, "kb");
    let (c, ck) = chat(&state, &ws, "comms-reply-fake").await;

    let (is_err, text) = send(
        &state,
        &c,
        &ck,
        serde_json::json!({"to": b, "text": "which column holds the batch id?", "expect_reply": true}),
    )
    .await;
    assert!(!is_err, "{text}");
    assert!(text.contains("reply_to"), "{text}");
    let asked = text
        .split("Sent (#")
        .nth(1)
        .and_then(|t| t.split(')').next())
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap();
    let context = hook(&state, &b, "kb", "PostToolUse").await;
    assert!(context.contains("They asked for a reply"), "{context}");

    let (is_err, text) = send(
        &state,
        &b,
        "kb",
        serde_json::json!({"to": c, "text": "it's `batch_id`", "reply_to": asked}),
    )
    .await;
    assert!(!is_err, "{text}");
    assert!(text.contains("started a turn"), "{text}");
    let content = journal_has(&state, &c, "batch_id").await;
    assert!(content.contains(&format!("re #{asked}")), "{content}");
    assert!(comms(&state, &ws).await["wake_requests"]
        .as_array()
        .unwrap()
        .is_empty());

    state.chat.kill(&c);
    state.sessions.kill(&b).ok();
}

/// A working claude chat hears a message on its next hook — and its
/// transcript shows it (a journaled `agent_message`). A turn that ends
/// with a message still unread meets the wake policy then.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_working_chat_hears_it_at_its_next_step_and_its_transcript_shows_it() {
    let state = test_state();
    set_comms(&state, true, "auto").await;
    let ws = make_workspace(&state, "comms-busy").await;
    let a = tui(&state, &ws, "ka");
    let (c, ck) = chat(&state, &ws, "comms-busy-fake").await;
    lock(&state.agents).get_mut(&c).unwrap().state = agent_state::AgentState::Running;

    let (_, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": c, "text": "heads-up: qc/ moved"}),
    )
    .await;
    assert!(text.contains("next step"), "{text}");
    let context = hook(&state, &c, &ck, "PostToolUse").await;
    assert!(context.contains("heads-up: qc/ moved"), "{context}");
    let content = journal_has(&state, &c, "\"type\":\"agent_message\"").await;
    assert!(content.contains("heads-up: qc/ moved"), "{content}");

    // Still working, a second message; then the turn ends before any hook.
    let b = tui(&state, &ws, "kb");
    let (_, text) = send(
        &state,
        &b,
        "kb",
        serde_json::json!({"to": c, "text": "and the tests are green"}),
    )
    .await;
    assert!(text.contains("next step"), "{text}");
    lock(&state.agents).get_mut(&c).unwrap().state = agent_state::AgentState::Finished;
    crate::comms::on_chat_event(
        &state,
        &c,
        &chimaera_agent::model::AgentEvent::TurnCompleted {
            turn_id: "t1".into(),
            usage: Default::default(),
        },
    );
    let content = journal_has(&state, &c, "and the tests are green").await;
    assert!(content.contains("when your turn ended"), "{content}");

    state.chat.kill(&c);
    for sid in [a, b] {
        state.sessions.kill(&sid).ok();
    }
}

/// Switched off: nothing is offered, carried, sent or woken — and what was
/// waiting is still there when it comes back.
#[tokio::test]
async fn switched_off_nothing_moves() {
    let state = test_state();
    let ws = make_workspace(&state, "comms-off").await;
    let a = tui(&state, &ws, "ka");
    let b = tui(&state, &ws, "kb");
    let (is_err, _) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": b, "text": "before the switch"}),
    )
    .await;
    assert!(!is_err);

    set_comms(&state, false, "ask").await;
    assert_eq!(hook(&state, &b, "kb", "PostToolUse").await, "");
    let (is_err, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": b, "text": "during"}),
    )
    .await;
    assert!(is_err && text.contains("Settings → Agents"), "{text}");
    let body = comms(&state, &ws).await;
    assert_eq!(body["enabled"], false);
    assert!(body["unread"].as_object().unwrap().is_empty());

    set_comms(&state, true, "ask").await;
    assert!(hook(&state, &b, "kb", "PostToolUse")
        .await
        .contains("before the switch"));

    for sid in [a, b] {
        state.sessions.kill(&sid).ok();
    }
}
