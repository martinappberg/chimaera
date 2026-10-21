//! Agent communication end to end (plan: docs/design/agent-communication-plan.md):
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

#[cfg(unix)]
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
#[cfg(unix)]
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

#[cfg(unix)]
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

    // A hook fired inside one of b's subagents is never the carrier: the
    // message waits for b's own next step.
    let (status, answer) = request(
        &state,
        Method::POST,
        &format!("/api/v1/agent-events/{b}?key=kb"),
        Some(serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "agent_id": "a55d52603606673c2",
            "agent_type": "general-purpose",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert!(answer.get("hookSpecificOutput").is_none(), "{answer}");
    assert_eq!(comms(&state, &ws).await["unread"][&b], 1);

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

#[cfg(unix)]
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

#[cfg(unix)]
/// An idle chat and the wake policy: "ask" puts a request in Needs you (and
/// announces it) and the user's Wake delivers; "never" leaves it for the
/// user's hand-over; "auto" (the default) wakes at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wake_policy_decides_what_an_idle_chat_gets() {
    let state = test_state();
    let ws = make_workspace(&state, "comms-wake").await;
    let a = tui(&state, &ws, "ka");
    let (c, _) = chat(&state, &ws, "comms-wake-fake").await;
    assert_eq!(comms(&state, &ws).await["wakes"], "auto", "the default");

    // Ask.
    set_comms(&state, true, "ask").await;
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
    let announced: Vec<_> = state
        .notices
        .since(0)
        .into_iter()
        .filter(|n| n.kind == notices::NoticeKind::Wake)
        .collect();
    assert_eq!(announced.len(), 1, "the ask is announced once");
    assert_eq!(announced[0].session_id, c, "a click opens the idle agent");
    assert!(announced[0].body.contains("please rerun the QC"));
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

#[cfg(unix)]
/// Inside the per-pair gap a fresh message is not stranded: it wakes its
/// idle reader once the gap has passed — and another sender's message to
/// the same reader isn't held by it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wake_inside_the_gap_comes_when_it_passes() {
    let state = test_state();
    set_comms(&state, true, "auto").await;
    let ws = make_workspace(&state, "comms-gap").await;
    let a = tui(&state, &ws, "ka");
    let b = tui(&state, &ws, "kb");
    let (c, _) = chat(&state, &ws, "comms-gap-fake").await;
    let idle = |state: &Arc<AppState>| {
        lock(&state.agents).get_mut(&c).unwrap().state = agent_state::AgentState::Finished;
    };

    let (_, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": c, "text": "first-task"}),
    )
    .await;
    assert!(text.contains("started a turn"), "{text}");
    journal_has(&state, &c, "first-task").await;
    idle(&state);

    let (_, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": c, "text": "second-task"}),
    )
    .await;
    assert!(text.contains("wakes it again in"), "{text}");
    assert!(!journal(&state, &c).contains("second-task"));
    assert!(
        comms(&state, &ws).await["wake_requests"]
            .as_array()
            .unwrap()
            .is_empty(),
        "held back, not asked"
    );
    let content = journal_has(&state, &c, "second-task").await;
    assert!(content.contains("while you were idle"), "{content}");
    idle(&state);

    let (_, text) = send(
        &state,
        &b,
        "kb",
        serde_json::json!({"to": c, "text": "other-sender"}),
    )
    .await;
    assert!(
        text.contains("started a turn"),
        "another sender's gap is its own: {text}"
    );
    journal_has(&state, &c, "other-sender").await;

    state.chat.kill(&c);
    for sid in [a, b] {
        state.sessions.kill(&sid).ok();
    }
}

#[cfg(unix)]
/// Agents get a round of wakes between two of the user's own actions in a
/// workspace. Past it the user is asked once — one notification however
/// many agents wait — and one Continue lets every held agent go on; a
/// prompt the user sends (a chat's socket, a terminal's own hook) starts a
/// new round too, while a chat's hook (it fires for a wake as well) doesn't.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn past_the_unattended_limit_the_user_is_asked_once_and_continue_lets_all_go_on() {
    let state = test_state();
    set_comms(&state, true, "auto").await;
    let ws = make_workspace(&state, "comms-unattended").await;
    let a = tui(&state, &ws, "ka");
    let (c1, _) = chat(&state, &ws, "comms-unattended-1").await;
    let (c2, _) = chat(&state, &ws, "comms-unattended-2").await;
    crate::comms::use_unattended_wakes_for_tests(&state, &ws);

    let (_, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": c1, "text": "held-one"}),
    )
    .await;
    assert!(text.contains("keep going"), "{text}");
    let (_, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": c2, "text": "held-two"}),
    )
    .await;
    assert!(text.contains("keep going"), "{text}");
    let body = comms(&state, &ws).await;
    let requests = body["wake_requests"].as_array().unwrap();
    assert_eq!(requests.len(), 2, "{body}");
    assert!(
        requests.iter().all(|r| r["reason"] == "unattended"),
        "{body}"
    );
    let announced = state
        .notices
        .since(0)
        .into_iter()
        .filter(|n| n.kind == notices::NoticeKind::Wake)
        .count();
    assert_eq!(announced, 1, "asked once, however many agents wait");

    let wid = requests
        .iter()
        .find(|r| r["to_sid"] == c1.as_str())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, out) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/comms/wakes/{wid}"),
        Some(serde_json::json!({"wake": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    journal_has(&state, &c1, "held-one").await;
    let content = journal_has(&state, &c2, "held-two").await;
    assert!(content.contains("keep going"), "{content}");
    assert!(comms(&state, &ws).await["wake_requests"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        crate::comms::unattended_wakes_for_tests(&state, &ws),
        1,
        "a new round; the released agent's wake counts against it"
    );

    // The user's own prompt in a chat (its socket's send) starts a new round.
    {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/chat/{c1}"))
            .await
            .unwrap();
        socket
            .send(WsMessage::text(
                serde_json::json!({"type": "auth", "token": "test-token", "last_seq": 0})
                    .to_string(),
            ))
            .await
            .unwrap();
        socket
            .send(WsMessage::text(
                serde_json::json!({"type": "send", "blocks": [{"type": "text", "text": "carry on"}]})
                    .to_string(),
            ))
            .await
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while crate::comms::unattended_wakes_for_tests(&state, &ws) != 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the chat prompt didn't count"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        server.abort();
    }

    // So does the user's own prompt in a terminal agent.
    crate::comms::use_unattended_wakes_for_tests(&state, &ws);
    hook(&state, &a, "ka", "UserPromptSubmit").await;
    assert_eq!(crate::comms::unattended_wakes_for_tests(&state, &ws), 0);
    // A chat's own UserPromptSubmit also fires for a wake: it never counts.
    crate::comms::use_unattended_wakes_for_tests(&state, &ws);
    let key = lock(&state.agents).get(&c2).unwrap().key.clone();
    hook(&state, &c2, &key, "UserPromptSubmit").await;
    assert_ne!(crate::comms::unattended_wakes_for_tests(&state, &ws), 0);

    state.chat.kill(&c1);
    state.chat.kill(&c2);
    state.sessions.kill(&a).ok();
}

#[cfg(unix)]
/// A reply to a question the reader asked (`expect_reply`) wakes it even
/// when the policy asks first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_the_asker_wanted_wakes_it_without_asking() {
    let state = test_state();
    set_comms(&state, true, "ask").await;
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
    assert!(
        content.contains("it answers a question you asked"),
        "{content}"
    );
    assert!(comms(&state, &ws).await["wake_requests"]
        .as_array()
        .unwrap()
        .is_empty());

    // A follow-up question right away: its answer wakes the asker again —
    // a reply is bounded by its conversation, not the per-sender gap.
    lock(&state.agents).get_mut(&c).unwrap().state = agent_state::AgentState::Finished;
    let (is_err, text) = send(
        &state,
        &c,
        &ck,
        serde_json::json!({"to": b, "text": "and the value column?", "expect_reply": true}),
    )
    .await;
    assert!(!is_err, "{text}");
    let again = text
        .split("Sent (#")
        .nth(1)
        .and_then(|t| t.split(')').next())
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap();
    assert!(hook(&state, &b, "kb", "PostToolUse")
        .await
        .contains("and the value column?"));
    let (is_err, text) = send(
        &state,
        &b,
        "kb",
        serde_json::json!({"to": c, "text": "it's `value_col`", "reply_to": again}),
    )
    .await;
    assert!(!is_err, "{text}");
    assert!(text.contains("started a turn"), "{text}");
    journal_has(&state, &c, "value_col").await;

    state.chat.kill(&c);
    state.sessions.kill(&b).ok();
}

#[cfg(unix)]
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

/// Pass 44: a claude terminal blocked on a foreground subagent has no next
/// step until it returns, and nothing can free it. The sender and the roster
/// say so; the parent's own PostToolUse for that call is the carrier, never
/// the subagent's hooks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_terminal_waiting_on_a_subagent_is_named_and_reads_when_it_returns() {
    let state = test_state();
    set_comms(&state, true, "ask").await;
    let ws = make_workspace(&state, "comms-subagent").await;
    let a = tui(&state, &ws, "ka");
    let b = tui(&state, &ws, "kb");
    let post = |payload: serde_json::Value| {
        let state = state.clone();
        let b = b.clone();
        async move {
            let (status, answer) = request(
                &state,
                Method::POST,
                &format!("/api/v1/agent-events/{b}?key=kb"),
                Some(payload),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            answer["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap_or("")
                .to_string()
        }
    };
    post(serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": "audit"})).await;
    post(serde_json::json!({
        "hook_event_name": "PreToolUse", "tool_name": "Agent", "tool_use_id": "tu-agent",
        "tool_input": {"description": "audit the loader", "prompt": "…"},
    }))
    .await;
    assert_eq!(
        lock(&state.agents).get(&b).unwrap().state,
        agent_state::AgentState::Running
    );

    let (_, roster) =
        mcp_tool_call(&state, &a, "ka", "workspace_agents", serde_json::json!({})).await;
    assert!(
        roster.contains("working: waiting on a subagent for under a minute (audit the loader)"),
        "{roster}"
    );
    assert!(
        roster.contains("reads messages when its subagent returns"),
        "{roster}"
    );
    let (is_err, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": b, "text": "is target/ still in use?"}),
    )
    .await;
    assert!(!is_err, "{text}");
    assert!(
        text.contains("waiting on a subagent") && text.contains("when that subagent returns"),
        "{text}"
    );

    // The subagent's own calls never carry it.
    let inside = post(serde_json::json!({
        "hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_use_id": "tu-sub",
        "agent_id": "a55d52603606673c2", "agent_type": "general-purpose",
    }))
    .await;
    assert_eq!(inside, "");
    // The Agent call returning is the parent's next step.
    let back = post(serde_json::json!({
        "hook_event_name": "PostToolUse", "tool_name": "Agent", "tool_use_id": "tu-agent",
    }))
    .await;
    assert!(back.contains("is target/ still in use?"), "{back}");
    assert!(lock(&state.agents).get(&b).unwrap().main_calls.is_empty());
    for sid in [a, b] {
        state.sessions.kill(&sid).ok();
    }
}

/// `message_agent` waits for a "now" delivery to be read: the settle (here
/// the hook carrier's `finish`) must wake that wait at once, not let it run
/// to its 8 s limit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_settle_wakes_a_sender_waiting_for_the_read() {
    let state = test_state();
    set_comms(&state, true, "ask").await;
    let ws = make_workspace(&state, "comms-read-wait").await;
    let a = tui(&state, &ws, "ka");
    let b = tui(&state, &ws, "kb");
    let (_, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": b, "text": "ping"}),
    )
    .await;
    let seq: u64 = text
        .strip_prefix("Sent (#")
        .and_then(|rest| rest.split(')').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{text}"));
    let reader = crate::comms::reader(&state, &b).expect("b reads");
    let limit = std::time::Duration::from_secs(8);
    let waiter = {
        let state = state.clone();
        let reader = reader.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let read = crate::comms::read_within(&state, &reader, seq, limit).await;
            (read, started.elapsed())
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let carried = hook(&state, &b, "kb", "PostToolUse").await;
    assert!(carried.contains("ping"), "{carried}");
    let (read, took) = waiter.await.unwrap();
    assert!(read, "the hook settled it");
    assert!(
        took < std::time::Duration::from_secs(2),
        "woke after {took:?}"
    );
    // Already settled: no wait at all.
    let started = std::time::Instant::now();
    assert!(crate::comms::read_within(&state, &reader, seq, limit).await);
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
    for sid in [a, b] {
        state.sessions.kill(&sid).ok();
    }
}

#[cfg(unix)]
/// ACP chats cannot be steered mid-turn. Their direct messages must still meet
/// the wake policy at turn end, without being lost or asking more than once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_acp_messages_reach_the_wake_policy_after_the_turn() {
    for kind in [agents::AgentKind::Antigravity, agents::AgentKind::Grok] {
        let state = test_state();
        set_comms(&state, true, "ask").await;
        let ws = make_workspace(&state, "comms-acp-busy").await;
        let a = tui(&state, &ws, "ka");
        // The fake supplies a live chat transport; the reader's native identity
        // drives delivery, independently of the driver behind that transport.
        let (c, _) = chat(&state, &ws, "comms-acp-fake").await;
        {
            let mut agents = lock(&state.agents);
            let record = agents.get_mut(&c).unwrap();
            record.kind = kind;
            record.state = agent_state::AgentState::Running;
        }
        let (failed, text) = send(
            &state,
            &a,
            "ka",
            serde_json::json!({"to":c,"text":"queued-acp-message"}),
        )
        .await;
        assert!(!failed, "{text}");
        assert!(!text.contains("next step"), "{text}");
        assert!(comms(&state, &ws).await["wake_requests"]
            .as_array()
            .unwrap()
            .is_empty());
        lock(&state.agents).get_mut(&c).unwrap().state = agent_state::AgentState::Finished;
        crate::comms::on_chat_event(
            &state,
            &c,
            &chimaera_agent::model::AgentEvent::TurnCompleted {
                turn_id: "t".into(),
                usage: Default::default(),
            },
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let body = comms(&state, &ws).await;
            if !body["wake_requests"].as_array().unwrap().is_empty() {
                assert_eq!(body["wake_requests"].as_array().unwrap().len(), 1);
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "queued message was stranded: {body}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            !journal(&state, &c).contains("queued-acp-message"),
            "Ask must not start a turn"
        );
        state.chat.kill(&c);
        state.sessions.kill(&a).ok();
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

#[cfg(unix)]
/// Only a message that reached a working chat and went unread meets the
/// wake policy at its turn end — once. A broadcast never wakes it, and a
/// message the user left in the inbox isn't asked about again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_end_decides_only_what_missed_the_turn_and_only_once() {
    let state = test_state();
    set_comms(&state, true, "auto").await;
    let ws = make_workspace(&state, "comms-turn-end").await;
    let a = tui(&state, &ws, "ka");
    let (c, _) = chat(&state, &ws, "comms-turn-end-fake").await;
    let turn_end = |state: &Arc<AppState>| {
        lock(&state.agents).get_mut(&c).unwrap().state = agent_state::AgentState::Finished;
        crate::comms::on_chat_event(
            state,
            &c,
            &chimaera_agent::model::AgentEvent::TurnCompleted {
                turn_id: "t".into(),
                usage: Default::default(),
            },
        );
    };

    // A broadcast while C works, still unread at its turn end: no wake.
    lock(&state.agents).get_mut(&c).unwrap().state = agent_state::AgentState::Running;
    let (is_err, _) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": "everyone", "text": "broadcast-only"}),
    )
    .await;
    assert!(!is_err);
    turn_end(&state);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        !journal(&state, &c).contains("broadcast-only"),
        "a broadcast woke it"
    );

    // Ask: a direct message while it works → at the turn end, one request;
    // the user leaves it in the inbox; the next turn end asks nothing.
    set_comms(&state, true, "ask").await;
    lock(&state.agents).get_mut(&c).unwrap().state = agent_state::AgentState::Running;
    let (_, text) = send(
        &state,
        &a,
        "ka",
        serde_json::json!({"to": c, "text": "left-in-inbox"}),
    )
    .await;
    assert!(text.contains("next step"), "{text}");
    turn_end(&state);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let wid = loop {
        let body = comms(&state, &ws).await;
        if let Some(r) = body["wake_requests"].as_array().unwrap().first() {
            break r["id"].as_str().unwrap().to_string();
        }
        assert!(tokio::time::Instant::now() < deadline, "no request: {body}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/comms/wakes/{wid}"),
        Some(serde_json::json!({"wake": false})),
    )
    .await;
    assert!(status.is_success());
    lock(&state.agents).get_mut(&c).unwrap().state = agent_state::AgentState::Running;
    turn_end(&state);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let body = comms(&state, &ws).await;
    assert!(
        body["wake_requests"].as_array().unwrap().is_empty(),
        "asked again: {body}"
    );
    assert_eq!(
        body["unread"][&c], 2,
        "both still wait in its inbox: {body}"
    );
    assert!(!journal(&state, &c).contains("left-in-inbox"));

    state.chat.kill(&c);
    state.sessions.kill(&a).ok();
}

/// read_messages is bounded: 20 at most per call, oldest first, and only
/// what it shows is marked read.
#[tokio::test]
async fn read_messages_is_bounded_and_settles_only_what_it_shows() {
    let state = test_state();
    let ws = make_workspace(&state, "comms-read-cap").await;
    let b = tui(&state, &ws, "kb");
    // 25 senders (the post window is per sender, 10 a minute).
    let mut senders = Vec::new();
    for i in 0..25 {
        let key = format!("ks{i}");
        let sid = tui(&state, &ws, &key);
        let (is_err, text) = send(
            &state,
            &sid,
            &key,
            serde_json::json!({"to": b, "text": format!("note number {i}")}),
        )
        .await;
        assert!(!is_err, "{text}");
        senders.push(sid);
    }
    let (_, first) = mcp_tool_call(&state, &b, "kb", "read_messages", serde_json::json!({})).await;
    assert!(first.contains("note number 0\n"), "oldest first: {first}");
    assert!(!first.contains("note number 24"), "{first}");
    assert!(first.contains("5 more unread"), "{first}");
    let (_, second) = mcp_tool_call(&state, &b, "kb", "read_messages", serde_json::json!({})).await;
    assert!(second.contains("note number 24"), "{second}");
    assert!(!second.contains("note number 0\n"), "{second}");
    let (_, third) = mcp_tool_call(&state, &b, "kb", "read_messages", serde_json::json!({})).await;
    assert!(third.contains("No messages waiting"), "{third}");
    for sid in senders.into_iter().chain([b]) {
        state.sessions.kill(&sid).ok();
    }
}
