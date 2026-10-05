//! The agent `open_browser` tool end to end: the proxy's mint allowlist
//! applied to agents, the honest no-window answer, the `browser_open` frame
//! on `/ws/events`, and the per-session rate limit.

use super::support::*;
use crate::*;

async fn open(state: &Arc<AppState>, agent: &str, url: &str) -> (bool, String) {
    mcp_tool_call(
        state,
        agent,
        "ob",
        "open_browser",
        serde_json::json!({"url": url}),
    )
    .await
}

#[tokio::test]
async fn open_browser_refuses_what_the_ui_would_need_confirmed() {
    let state = test_state_with_port(9700);
    let agent = inject_agent(&state, "ob");

    // Unparseable / non-http: a clear tool error, nothing pushed.
    for (url, needle) in [
        ("localhost:5173", "http://"),
        ("https://localhost:8443/", "plain http"),
        ("http://node-014/", "port explicitly"),
        ("http://user:pw@localhost:1/", "user name"),
    ] {
        let (is_error, text) = open(&state, &agent, url).await;
        assert!(is_error, "{url}: {text}");
        assert!(text.contains(needle), "{url}: {text}");
    }
    // A host outside the allowlist needs the user's own confirmation, which
    // an agent can never give: refused, with the URL to hand over.
    let (is_error, text) = open(&state, &agent, "http://elsewhere.example.org:8888/x").await;
    assert!(is_error, "{text}");
    assert!(text.contains("Give the user the URL"), "{text}");
    assert!(
        text.contains("http://elsewhere.example.org:8888/x"),
        "{text}"
    );
    // The daemon's own port (9700 in tests) is never a target.
    let (is_error, text) = open(&state, &agent, "http://localhost:9700/").await;
    assert!(is_error, "{text}");
    assert!(text.contains("Chimaera itself"), "{text}");
    // No window connected: honest, and nothing spent or queued.
    let (is_error, text) = open(&state, &agent, "http://localhost:5173/").await;
    assert!(!is_error, "{text}");
    assert!(text.contains("no Chimaera window is connected"), "{text}");
    assert_eq!(state.browser_opens.head(), 0, "nothing queued");

    state.sessions.kill(&agent).ok();
}

#[tokio::test]
async fn open_browser_frame_reaches_connected_windows() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();
    let ws = make_workspace(&state, "open-browser").await;
    let agent = inject_agent(&state, "ob");
    lock(&state.session_workspaces).insert(agent.clone(), ws.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/events"))
        .await
        .unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "test-token"}).to_string(),
        ))
        .await
        .unwrap();
    // The first sessions snapshot means the socket is authed and counted.
    loop {
        if let WsMessage::Text(text) = next_ws_frame(&mut socket).await {
            let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
            if frame["type"] == "sessions" {
                break;
            }
        }
    }
    assert_eq!(state.browser_opens.consumers(), 1);

    let (is_error, text) = open(&state, &agent, "http://LOCALHOST:5173/app?x=1#top").await;
    assert!(!is_error, "{text}");
    assert!(text.starts_with("Requested a browser pane"), "{text}");
    assert!(
        text.contains("nothing about whether the page loads"),
        "{text}"
    );
    let frame = loop {
        if let WsMessage::Text(text) = next_ws_frame(&mut socket).await {
            let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
            if frame["type"] == "browser_open" {
                break frame;
            }
        }
    };
    assert_eq!(
        frame,
        serde_json::json!({
            "type": "browser_open",
            "session_id": agent,
            "workspace_id": ws,
            "host": "localhost",
            "port": 5173,
            "path": "/app?x=1#top",
        })
    );

    // Right again: inside the per-session gap.
    let (is_error, text) = open(&state, &agent, "http://localhost:5174/").await;
    assert!(is_error, "{text}");
    assert!(text.contains("moments ago"), "{text}");

    socket.close(None).await.unwrap();
    // The consumer count follows the socket down (the handler notices the
    // close on its next receive).
    for _ in 0..50 {
        if state.browser_opens.consumers() == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(state.browser_opens.consumers(), 0);
    server.abort();
    state.sessions.kill(&agent).ok();
}
