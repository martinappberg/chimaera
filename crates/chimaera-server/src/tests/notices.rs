use super::support::*;
use crate::*;

/// One long-poll round: `GET /notices` for the given cursor.
async fn poll(state: &Arc<AppState>, boot: &str, after: u64) -> serde_json::Value {
    let (status, body) = request(
        state,
        Method::GET,
        &format!("/api/v1/notices?after={after}&boot={boot}&wait=0"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Poll until at least one notice after `after` arrives (the watcher's
/// settle makes edges land ~1s after the state change).
async fn wait_notices(state: &Arc<AppState>, boot: &str, after: u64) -> Vec<serde_json::Value> {
    for _ in 0..60 {
        let body = poll(state, boot, after).await;
        let notices = body["notices"].as_array().cloned().unwrap_or_default();
        if !notices.is_empty() {
            return notices;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("no notice arrived");
}

/// A fresh consumer (no boot) starts at the head — history is never
/// replayed as new alerts.
async fn fresh_boot(state: &Arc<AppState>) -> (String, u64) {
    let (status, body) = request(state, Method::GET, "/api/v1/notices", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["notices"], serde_json::json!([]));
    assert!(body["prefs"]["sound"].as_bool().unwrap());
    (
        body["boot"].as_str().unwrap().to_string(),
        body["head"].as_u64().unwrap(),
    )
}

#[tokio::test]
async fn notices_route_requires_the_bearer_token() {
    let state = test_state();
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/notices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn agent_notify_tool_feeds_the_poll_and_is_rate_limited() {
    let state = test_state();
    let id = inject_agent(&state, "nk");
    let (boot, head) = fresh_boot(&state).await;

    let (is_error, text) = mcp_tool_call(
        &state,
        &id,
        "nk",
        "notify",
        serde_json::json!({"message": "The **nightly** build finished.", "title": "CI"}),
    )
    .await;
    assert!(!is_error, "{text}");

    let notices = wait_notices(&state, &boot, head).await;
    assert_eq!(notices.len(), 1);
    let n = &notices[0];
    assert_eq!(n["kind"], "agent");
    assert_eq!(n["session_id"], serde_json::json!(id));
    assert_eq!(n["title"], "CI");
    // Markdown emphasis is dropped: a notification renders it literally.
    assert_eq!(n["body"], "The nightly build finished.");
    assert_eq!(n["blocking"], false);

    // Straight away again: inside the per-session gap.
    let (is_error, text) = mcp_tool_call(
        &state,
        &id,
        "nk",
        "notify",
        serde_json::json!({"message": "again"}),
    )
    .await;
    assert!(is_error, "{text}");

    // An empty message is refused, not sent.
    let other = inject_agent(&state, "ok2");
    let (is_error, _) = mcp_tool_call(
        &state,
        &other,
        "ok2",
        "notify",
        serde_json::json!({"message": " "}),
    )
    .await;
    assert!(is_error);

    state.sessions.kill(&id).ok();
    state.sessions.kill(&other).ok();
}

#[tokio::test]
async fn agent_notify_respects_the_setting() {
    let state = test_state();
    let id = inject_agent(&state, "nk");
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({"notifications.agentMessages": false})),
    )
    .await;
    assert!(status.is_success(), "{status}");
    let (is_error, text) = mcp_tool_call(
        &state,
        &id,
        "nk",
        "notify",
        serde_json::json!({"message": "hello"}),
    )
    .await;
    assert!(is_error);
    assert!(text.contains("turned off"), "{text}");
    state.sessions.kill(&id).ok();
}

#[tokio::test]
async fn hook_edges_become_settled_notices() {
    let state = test_state();
    let id = inject_agent(&state, "hk");
    tokio::spawn(crate::notices::run(state.clone()));
    let (boot, head) = fresh_boot(&state).await;
    let pause = || tokio::time::sleep(std::time::Duration::from_millis(400));

    // Let the watcher take its baseline, then run a turn.
    pause().await;
    let prompt = serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": "fix it"});
    assert_eq!(post_hook(&state, &id, "hk", prompt).await, StatusCode::OK);
    pause().await;
    let stop = serde_json::json!({
        "hook_event_name": "Stop",
        "last_assistant_message": "Fixed the flaky test; the suite passes.",
    });
    assert_eq!(post_hook(&state, &id, "hk", stop).await, StatusCode::OK);

    let notices = wait_notices(&state, &boot, head).await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    let done = &notices[0];
    assert_eq!(done["kind"], "done");
    assert_eq!(done["body"], "Fixed the flaky test; the suite passes.");
    // Named exactly as the rail names it (which name wins depends on the
    // shell: a prompt title set by the PTY's shell outranks the first prompt).
    let row = session_entry(&state, &id).await;
    assert_eq!(
        done["title"], row["display_name"],
        "named like the rail names it"
    );
    assert!(done["subtitle"].as_str().unwrap().starts_with("Finished"));
    let after = done["id"].as_u64().unwrap();

    // A permission prompt is a blocking notice carrying the hook's words.
    let pre = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash"});
    assert_eq!(post_hook(&state, &id, "hk", pre).await, StatusCode::OK);
    pause().await;
    let ask = serde_json::json!({
        "hook_event_name": "Notification",
        "notification_type": "permission_prompt",
        "message": "Claude needs your permission to use Bash",
    });
    assert_eq!(post_hook(&state, &id, "hk", ask).await, StatusCode::OK);
    let notices = wait_notices(&state, &boot, after).await;
    assert_eq!(notices[0]["kind"], "permission");
    assert_eq!(notices[0]["blocking"], true);
    assert_eq!(
        notices[0]["body"],
        "Claude needs your permission to use Bash"
    );

    // While it waits, the session is in the attention set the Dock badges.
    let body = poll(&state, &boot, notices[0]["id"].as_u64().unwrap()).await;
    let rows = body["attention"]["sessions"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["id"] == serde_json::json!(id)));

    state.sessions.kill(&id).ok();
}

#[tokio::test]
async fn a_quickly_answered_permission_never_notifies() {
    let state = test_state();
    let id = inject_agent(&state, "qk");
    tokio::spawn(crate::notices::run(state.clone()));
    let (boot, head) = fresh_boot(&state).await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let pre = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash"});
    post_hook(&state, &id, "qk", pre.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let ask = serde_json::json!({
        "hook_event_name": "Notification",
        "notification_type": "permission_prompt",
        "message": "Claude needs your permission to use Bash",
    });
    post_hook(&state, &id, "qk", ask).await;
    // Answered well inside the settle: the agent is running again.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    post_hook(&state, &id, "qk", pre).await;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let body = poll(&state, &boot, head).await;
    assert_eq!(body["notices"], serde_json::json!([]), "{body}");

    // The answered prompt's words must not label the turn's end (a Stop
    // hook without the final message says only "Finished").
    post_hook(
        &state,
        &id,
        "qk",
        serde_json::json!({"hook_event_name": "Stop"}),
    )
    .await;
    let notices = wait_notices(&state, &boot, head).await;
    assert_eq!(notices[0]["kind"], "done");
    assert_eq!(notices[0]["body"], "", "{notices:?}");
    state.sessions.kill(&id).ok();
}

/// A routed project's owner notice is taken once (every window's feed
/// delivers it), only for the relayed kinds, only for that project, never
/// for a session that runs here, and only while this computer's settings
/// want that kind.
#[tokio::test]
async fn relayed_owner_notices_are_taken_once_and_never_for_local_sessions() {
    let state = test_state();
    let workspace = lock(&state.workspaces)
        .add(test_dir("relay-notices"))
        .unwrap();
    let (boot, head) = fresh_boot(&state).await;
    let row = |kind: &str, session: &str, at_ms: u64| {
        serde_json::json!({
            "id": 7, "kind": kind, "blocking": kind == "permission",
            "session_id": session, "workspace_id": workspace.id,
            "workspace": "owner's name", "agent": "claude",
            "name": "fix the build", "title": "fix the build",
            "subtitle": "Needs permission · owner's name",
            "body": "Bash: rm -rf build", "at_ms": at_ms, "age_ms": 40,
        })
    };
    let permission = row("permission", "s-remote", 1_000);
    assert!(crate::notices::relay(&state, &workspace.id, &permission));
    // The same notice from a second window's feed: already taken.
    assert!(!crate::notices::relay(&state, &workspace.id, &permission));
    // Not this project, not a relayed kind, stale: never taken.
    assert!(!crate::notices::relay(&state, "w-other", &permission));
    for kind in ["error", "rate_limited", "kept_both", "bogus"] {
        assert!(
            !crate::notices::relay(&state, &workspace.id, &row(kind, "s-remote", 2_000)),
            "{kind}"
        );
    }
    let mut stale = row("done", "s-remote", 3_000);
    stale["age_ms"] = serde_json::json!(11 * 60 * 1000);
    assert!(!crate::notices::relay(&state, &workspace.id, &stale));
    // A session that runs here notifies through this daemon's own watcher.
    let here = inject_agent(&state, "rk");
    assert!(!crate::notices::relay(
        &state,
        &workspace.id,
        &row("done", &here, 4_000)
    ));
    // The same session's next turn is new; so is the agent's own message.
    assert!(crate::notices::relay(
        &state,
        &workspace.id,
        &row("done", "s-remote", 5_000)
    ));
    assert!(crate::notices::relay(
        &state,
        &workspace.id,
        &row("agent", "s-remote", 5_000)
    ));

    let notices = poll(&state, &boot, head).await["notices"]
        .as_array()
        .cloned()
        .unwrap();
    assert_eq!(notices.len(), 3, "{notices:?}");
    let first = &notices[0];
    assert_eq!(first["kind"], "permission");
    assert_eq!(first["blocking"], true);
    assert_eq!(first["session_id"], "s-remote");
    assert_eq!(first["body"], "Bash: rm -rf build");
    // Worded with this computer's name for the project.
    assert_eq!(
        first["subtitle"],
        format!("Needs permission · {}", workspace.name).as_str()
    );
    assert_eq!(notices[1]["kind"], "done");
    assert_eq!(notices[2]["kind"], "agent");

    // This computer's settings decide which kinds alert at all.
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(serde_json::json!({"notifications.needsYou": false})),
    )
    .await;
    assert!(status.is_success(), "{status}");
    assert!(!crate::notices::relay(
        &state,
        &workspace.id,
        &row("question", "s-remote", 6_000)
    ));
    state.sessions.kill(&here).ok();
}
