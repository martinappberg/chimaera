//! The agent browser end to end over a real `/ws/events` socket: a scripted
//! stand-in for an app window answers `browser_request` frames the way the
//! web UI + shell do (CONTRACTS §2, §3), so the tools, the page rules, the
//! window channel, URL mapping, pause gating and the browser-link routes run
//! exactly as in production. The Chromium engine is whatever `chromium.rs`
//! says about this host; these tests only use the app engine and the
//! no-engine refusal.

use super::support::*;
use crate::*;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Mutex as StdMutex;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// A stand-in app window on `/ws/events`.
struct FakeWindow {
    /// Client messages to send (hello, events).
    out: tokio::sync::mpsc::Sender<String>,
    /// Every `browser_request` received, in order.
    seen: Arc<StdMutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeWindow {
    async fn send(&self, msg: Value) {
        self.out.send(msg.to_string()).await.unwrap();
    }

    fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }

    fn ops(&self, op: &str) -> Vec<Value> {
        self.requests()
            .into_iter()
            .filter(|r| r["op"] == op)
            .collect()
    }
}

impl Drop for FakeWindow {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What the page "is": the proxied URL the window loaded.
#[derive(Default)]
struct PageModel {
    href: String,
    handovers: u32,
}

fn wrapped(inner: Value, href: &str) -> Value {
    json!({"r": inner, "f": {"t": "Demo", "u": href, "h": false}})
}

/// Answer one request the way the window + shell would.
fn answer(model: &StdMutex<PageModel>, req: &Value) -> Result<Value, String> {
    let mut page = model.lock().unwrap();
    match req["op"].as_str().unwrap_or_default() {
        "open" => {
            page.href = format!(
                "http://127.0.0.1:9/{}",
                req["url"].as_str().unwrap().trim_start_matches('/')
            );
            Ok(json!({}))
        }
        "nav" | "close" | "state" => Ok(json!({})),
        "handover" => {
            page.handovers += 1;
            Ok(json!({"tab": format!("handed{}", page.handovers)}))
        }
        "drive" => {
            let action = &req["action"];
            match action["type"].as_str().unwrap_or_default() {
                "eval" => {
                    let body = action["body"].as_str().unwrap_or_default();
                    let href = if page.href.is_empty() {
                        "http://127.0.0.1:9/proxy/p-shared/notebook".to_string()
                    } else {
                        page.href.clone()
                    };
                    let value = if body.contains("globalThis.__chimaeraNav = args.n") {
                        json!(href)
                    } else if body.contains("responseStatus") {
                        json!({"m": null, "r": "complete", "u": href, "t": "Demo", "s": 200})
                    } else if body.contains("location.assign") {
                        let url = action["args"]["url"].as_str().unwrap_or("/");
                        page.href = format!("http://127.0.0.1:9{url}");
                        json!(true)
                    } else if body.contains("__chimaeraR") {
                        let inner = if body.contains("__snap.snapshot(") {
                            json!({"text": "page \"Demo\" /app viewport=1280x800 scroll=0/800\n- [e1] button \"Run\"\n- [e2] combobox \"Kernel\"", "refs": 2})
                        } else if body.contains("__snap.box(") {
                            let r = action["args"]["ref"].as_str().unwrap_or_default();
                            match r {
                                "e1" => {
                                    json!({"x": 40, "y": 20, "w": 30, "h": 16, "covered": false, "by": null, "select": false, "desc": "button \"Run\""})
                                }
                                "e2" => {
                                    json!({"x": 90, "y": 20, "w": 60, "h": 16, "covered": false, "by": null, "select": true, "desc": "combobox \"Kernel\""})
                                }
                                "e3" => {
                                    json!({"x": 10, "y": 10, "w": 5, "h": 5, "covered": true, "by": "dialog \"Save?\"", "select": false, "desc": "link \"Home\""})
                                }
                                _ => Value::Null,
                            }
                        } else if body.contains("__snap.rect(") {
                            json!({"x": 0, "y": 0, "width": 100, "height": 100})
                        } else if body.contains("__snap.watch()") {
                            json!(true)
                        } else if body.contains("__snap.delta(") {
                            json!({"newDocument": false, "mutations": 3, "urlChanged": false, "titleChanged": false, "url": href, "title": "Demo", "focus": null, "dialog": null})
                        } else if body.contains("throw new Error('nope')") {
                            return Err("Error: nope\n    at eval".to_string());
                        } else if body.contains("return 6 * 7;") {
                            json!(42)
                        } else {
                            Value::Null
                        };
                        wrapped(inner, &href)
                    } else {
                        Value::Null
                    };
                    Ok(json!({"value": value}))
                }
                // An element capture is "huge" as PNG, so the tool retries as JPEG.
                "screenshot" => match (action["clip"].is_null(), action["format"].as_str()) {
                    (false, Some("png")) => Ok(
                        json!({"png": "A".repeat(3_600_000), "width": 100, "height": 100, "scale": 1.0}),
                    ),
                    (false, Some("jpeg")) => {
                        Ok(json!({"png": "/9j/4AAQ", "width": 100, "height": 100, "scale": 1.0}))
                    }
                    _ => Ok(
                        json!({"png": "iVBORw0KGgo=", "width": 1280, "height": 800, "scale": 1.0}),
                    ),
                },
                "rings" => {
                    let since = action["console_since"].as_u64().unwrap_or(0);
                    let console: Vec<Value> =
                        [json!({"id": 1, "level": "error", "text": "boom", "at": 0.0})]
                            .into_iter()
                            .filter(|e| e["id"].as_u64().unwrap() >= since)
                            .collect();
                    Ok(
                        json!({"console": console, "network": [], "inflight": 0, "console_next": 2, "network_next": 1}),
                    )
                }
                // The shell aborts injected input when the user takes over.
                "click" if action["x"] == 777.0 => {
                    Err("paused by the user (input stopped mid-way)".to_string())
                }
                _ => Ok(json!({})),
            }
        }
        other => Err(format!("unknown op {other}")),
    }
}

/// Connect a stand-in window: authed, watching `workspace`, saying hello.
async fn connect_window(addr: std::net::SocketAddr, workspace: &str, tabs: &[&str]) -> FakeWindow {
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/events"))
        .await
        .unwrap();
    let (mut sink, mut stream) = socket.split();
    sink.send(WsMessage::text(
        json!({"type": "auth", "token": "test-token"}).to_string(),
    ))
    .await
    .unwrap();
    // The first sessions snapshot means the socket is authed and registered.
    loop {
        if let WsMessage::Text(text) = next_ws_frame(&mut stream).await {
            let frame: Value = serde_json::from_str(&text).unwrap();
            if frame["type"] == "sessions" {
                break;
            }
        }
    }
    let (out, mut out_rx) = tokio::sync::mpsc::channel::<String>(16);
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let model = Arc::new(StdMutex::new(PageModel::default()));
    let log = seen.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = out_rx.recv() => match msg {
                    Some(text) => { if sink.send(WsMessage::text(text)).await.is_err() { return; } }
                    None => { let _ = sink.close().await; return; }
                },
                frame = stream.next() => {
                    let Some(Ok(WsMessage::Text(text))) = frame else {
                        if frame.is_none() { return; }
                        continue;
                    };
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    if frame["type"] != "browser_request" {
                        continue;
                    }
                    log.lock().unwrap().push(frame.clone());
                    let reply = match answer(&model, &frame) {
                        Ok(result) => json!({"type": "browser_reply", "rid": frame["rid"], "ok": true, "result": result}),
                        Err(error) => json!({"type": "browser_reply", "rid": frame["rid"], "ok": false, "error": error}),
                    };
                    if sink.send(WsMessage::text(reply.to_string())).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
    let window = FakeWindow { out, seen, task };
    window
        .send(json!({"type": "watch", "workspace_id": workspace}))
        .await;
    window
        .send(
            json!({"type": "browser_hello", "app_engine": true, "visible": true, "focused": true,
                     "width": 1440, "height": 900, "tabs": tabs}),
        )
        .await;
    window
}

/// The settings every test here starts from: a fixed git, so a window's
/// first `/ws/events` frames never resolve git through the user's login
/// shell. A slow login shell forked while another test's pipe is still
/// inheritable (macOS sets close-on-exec after `pipe()`) holds that pipe
/// open and stalls unrelated subprocess tests past their deadlines.
const QUIET_GIT: &str = "/usr/bin/git";

/// A test state with Slurm detection already settled ("none") and git
/// pinned ([`QUIET_GIT`]): nothing here runs the user's login shell.
async fn quiet_state() -> Arc<AppState> {
    let state = test_state();
    state.compute.assume_no_scheduler().await;
    let mut map = serde_json::Map::new();
    map.insert("git.path".into(), QUIET_GIT.into());
    lock(&state.settings).put(map).unwrap();
    state
}

async fn serve(state: &Arc<AppState>) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (addr, server)
}

/// Wait until the daemon has this window's hello (it rides the same socket
/// as the requests, so a choice right after connecting must not race it).
async fn wait_hello(state: &Arc<AppState>, workspace: &str) {
    for _ in 0..100 {
        if state.browsers.channel.choose(Some(workspace)).is_some() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the window's hello never arrived");
}

async fn tool(state: &Arc<AppState>, agent: &str, name: &str, args: Value) -> Value {
    let (status, out) = mcp_post(
        state,
        agent,
        "ab",
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": name, "arguments": args}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    out["result"].clone()
}

fn text_of(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_error(result: &Value) -> bool {
    result["isError"].as_bool().unwrap_or(false)
}

async fn agent_on(state: &Arc<AppState>, label: &str) -> (String, String) {
    let ws = make_workspace(state, label).await;
    let agent = inject_agent(state, "ab");
    lock(&state.session_workspaces).insert(agent.clone(), ws.clone());
    (agent, ws)
}

#[tokio::test]
async fn the_app_engine_drives_an_own_page_through_the_window() {
    let state = quiet_state().await;
    let (agent, ws) = agent_on(&state, "ab-own").await;
    let (addr, server) = serve(&state).await;
    let window = connect_window(addr, &ws, &[]).await;
    wait_hello(&state, &ws).await;

    // Before any page: read tools say how to start.
    let r = tool(&state, &agent, "browser_snapshot", json!({})).await;
    assert!(
        is_error(&r) && text_of(&r).contains("browser_navigate"),
        "{r}"
    );

    // Off the allowlist: refused before any page exists.
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://elsewhere.example.org:80/"}),
    )
    .await;
    assert!(is_error(&r), "{r}");
    assert!(text_of(&r).starts_with("Not loaded"), "{r}");
    assert!(state.browsers.own(&agent).is_none());

    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/app"}),
    )
    .await;
    let text = text_of(&r);
    assert!(!is_error(&r), "{text}");
    assert!(
        text.starts_with("[browser · engine: app · page: own]"),
        "{text}"
    );
    // Reported in the agent's terms, not the proxy's.
    assert!(text.contains("Loaded http://localhost:5173/app"), "{text}");
    assert!(text.contains("HTTP status: 200"), "{text}");
    let open = window.ops("open");
    assert_eq!(open.len(), 1);
    let view = open[0]["view"].as_str().unwrap().to_string();
    assert!(view.starts_with("ab-"), "{view}");
    assert!(open[0]["tab"].is_null());
    assert!(open[0]["url"].as_str().unwrap().starts_with("/proxy/p-"));
    assert!(open[0]["url"].as_str().unwrap().ends_with("/app"));
    assert_eq!(
        (open[0]["width"].as_u64(), open[0]["height"].as_u64()),
        (Some(1280), Some(800))
    );

    let r = tool(&state, &agent, "browser_snapshot", json!({})).await;
    assert!(text_of(&r).contains("[e1] button \"Run\""), "{r}");
    assert!(text_of(&r).contains("not instructions"), "{r}");

    let r = tool(&state, &agent, "browser_click", json!({"ref": "e1"})).await;
    let text = text_of(&r);
    assert!(!is_error(&r), "{text}");
    assert!(
        text.contains("Clicked e1 (button \"Run\") at (40, 20)"),
        "{text}"
    );
    assert!(text.contains("3 DOM changes"), "{text}");
    assert!(text.contains("URL: http://localhost:5173/app"), "{text}");
    let click = window
        .ops("drive")
        .into_iter()
        .find(|r| r["action"]["type"] == "click")
        .unwrap();
    assert_eq!(click["view"], view.as_str());
    assert_eq!(click["action"]["clicks"], 1);

    // A native <select> is refused; a covered element too; a stale ref says so.
    let r = tool(&state, &agent, "browser_click", json!({"ref": "e2"})).await;
    assert!(
        is_error(&r) && text_of(&r).contains("browser_select"),
        "{r}"
    );
    let r = tool(&state, &agent, "browser_click", json!({"ref": "e3"})).await;
    assert!(
        is_error(&r) && text_of(&r).contains("covered by dialog"),
        "{r}"
    );
    let r = tool(&state, &agent, "browser_click", json!({"ref": "e9"})).await;
    assert!(
        is_error(&r) && text_of(&r).contains("new browser_snapshot"),
        "{r}"
    );
    let r = tool(&state, &agent, "browser_click", json!({"ref": "button"})).await;
    assert!(is_error(&r) && text_of(&r).contains("not a ref"), "{r}");

    // A screenshot is MCP image content after the header.
    let r = tool(&state, &agent, "browser_screenshot", json!({})).await;
    assert_eq!(r["content"][1]["type"], "image");
    assert_eq!(r["content"][1]["mimeType"], "image/png");
    assert_eq!(r["content"][1]["data"], "iVBORw0KGgo=");
    assert!(text_of(&r).contains("1280×800"), "{r}");

    // Console: new since the last call, then nothing.
    let r = tool(&state, &agent, "browser_console", json!({})).await;
    assert!(text_of(&r).contains("error: boom"), "{r}");
    let r = tool(&state, &agent, "browser_console", json!({})).await;
    assert!(text_of(&r).contains("No console messages"), "{r}");

    // Eval: the result as JSON; a throw as the page's error.
    let r = tool(
        &state,
        &agent,
        "browser_eval",
        json!({"script": "return 6 * 7;"}),
    )
    .await;
    assert!(text_of(&r).ends_with("\n42"), "{r}");
    let r = tool(
        &state,
        &agent,
        "browser_eval",
        json!({"script": "throw new Error('nope')"}),
    )
    .await;
    assert!(is_error(&r) && text_of(&r).contains("Error: nope"), "{r}");

    let r = tool(&state, &agent, "browser_press", json!({"key": "Return"})).await;
    assert!(is_error(&r) && text_of(&r).contains("not a key"), "{r}");

    // A second navigate reuses the page (no new view).
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/other"}),
    )
    .await;
    assert!(
        text_of(&r).contains("Loaded http://localhost:5173/other"),
        "{r}"
    );
    assert_eq!(window.ops("open").len(), 1);

    let r = tool(&state, &agent, "browser_close", json!({})).await;
    assert!(!is_error(&r), "{r}");
    assert_eq!(window.ops("close").len(), 1);
    assert!(state.browsers.own(&agent).is_none());

    drop(window);
    server.abort();
    state.sessions.kill(&agent).ok();
}

#[tokio::test]
async fn a_closed_window_takes_its_pages_with_it() {
    let state = quiet_state().await;
    let (agent, ws) = agent_on(&state, "ab-gone").await;
    let (addr, server) = serve(&state).await;
    let window = connect_window(addr, &ws, &[]).await;
    wait_hello(&state, &ws).await;
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/"}),
    )
    .await;
    assert!(!is_error(&r), "{r}");

    drop(window);
    for _ in 0..100 {
        if state.browsers.channel.socket_count() == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let r = tool(&state, &agent, "browser_snapshot", json!({})).await;
    assert!(is_error(&r), "{r}");
    assert!(
        text_of(&r).contains("Call browser_navigate to start a new page"),
        "{r}"
    );
    assert!(state.browsers.own(&agent).is_none());

    server.abort();
    state.sessions.kill(&agent).ok();
}

#[tokio::test]
async fn a_shared_pane_is_the_default_page_and_pauses_on_take_over() {
    let state = quiet_state().await;
    let (agent, ws) = agent_on(&state, "ab-shared").await;
    let (addr, server) = serve(&state).await;
    let window = connect_window(addr, &ws, &["t1"]).await;
    wait_hello(&state, &ws).await;
    let (status, body) = request(
        &state,
        Method::PUT,
        "/api/v1/links/browser",
        Some(json!({"tab": "t1", "agent_id": agent})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["page"], "p1");

    // The only shared pane is the default page, addressed by its tab.
    let r = tool(&state, &agent, "browser_snapshot", json!({})).await;
    let text = text_of(&r);
    assert!(
        text.starts_with("[browser · engine: app · page: shared pane \"Demo\" (p1)]"),
        "{text}"
    );
    let first = window.requests();
    assert!(
        first
            .iter()
            .all(|r| r["view"].is_null() && r["tab"] == "t1"),
        "{first:?}"
    );
    // First use installs the recorder and the agent cursor.
    let kinds: Vec<&str> = first
        .iter()
        .filter_map(|r| r["action"]["type"].as_str())
        .collect();
    assert!(
        kinds.contains(&"instrument") && kinds.contains(&"cursor"),
        "{kinds:?}"
    );

    // An act raises the driving marker.
    let r = tool(&state, &agent, "browser_click", json!({"ref": "e1"})).await;
    assert!(!is_error(&r), "{r}");
    assert!(
        window.ops("state").iter().any(|s| s["driving"] == true),
        "{:?}",
        window.ops("state")
    );

    // The user takes over: act tools refuse, read tools still work.
    window
        .send(json!({"type": "browser_event", "kind": "takeover", "view": null, "tab": "t1"}))
        .await;
    for _ in 0..100 {
        if state.browsers.shared("t1").paused.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let r = tool(
        &state,
        &agent,
        "browser_click",
        json!({"page": "p1", "ref": "e1"}),
    )
    .await;
    assert!(is_error(&r), "{r}");
    assert!(
        text_of(&r).contains("Paused: the user took over this page at "),
        "{r}"
    );
    assert!(text_of(&r).contains("Read tools still work"), "{r}");
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/x"}),
    )
    .await;
    assert!(is_error(&r) && text_of(&r).contains("Paused"), "{r}");
    let r = tool(&state, &agent, "browser_snapshot", json!({"page": "p1"})).await;
    assert!(!is_error(&r), "{r}");

    window
        .send(json!({"type": "browser_event", "kind": "resume", "tab": "t1"}))
        .await;
    for _ in 0..100 {
        if state.browsers.shared("t1").paused.is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let r = tool(&state, &agent, "browser_click", json!({"ref": "e1"})).await;
    assert!(!is_error(&r), "{r}");

    // An unknown pane names the ones shared; "own" is the own page (none yet).
    let r = tool(&state, &agent, "browser_snapshot", json!({"page": "p7"})).await;
    assert!(
        is_error(&r) && text_of(&r).contains("Shared with you: p1"),
        "{r}"
    );
    let r = tool(&state, &agent, "browser_snapshot", json!({"page": "own"})).await;
    assert!(is_error(&r) && text_of(&r).contains("no page yet"), "{r}");
    // A shared pane is the user's to close.
    let r = tool(&state, &agent, "browser_close", json!({})).await;
    assert!(is_error(&r) && text_of(&r).contains("stays theirs"), "{r}");

    drop(window);
    server.abort();
    state.sessions.kill(&agent).ok();
}

#[tokio::test]
async fn show_hands_the_live_view_over_as_a_shared_pane() {
    let state = quiet_state().await;
    let (agent, ws) = agent_on(&state, "ab-show").await;
    let (addr, server) = serve(&state).await;
    let window = connect_window(addr, &ws, &[]).await;
    wait_hello(&state, &ws).await;
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/app"}),
    )
    .await;
    assert!(!is_error(&r), "{r}");

    let r = tool(&state, &agent, "browser_show", json!({})).await;
    let text = text_of(&r);
    assert!(!is_error(&r), "{text}");
    assert!(text.contains("nothing reloaded"), "{text}");
    assert!(text.contains("shared pane p1"), "{text}");
    let handover = window.ops("handover");
    assert_eq!(handover.len(), 1);
    assert_eq!(
        (
            &handover[0]["host"],
            &handover[0]["port"],
            &handover[0]["path"],
            &handover[0]["agent"]
        ),
        (
            &json!("localhost"),
            &json!(5173),
            &json!("/app"),
            &json!(agent)
        )
    );
    assert!(state.browsers.own(&agent).is_none());
    assert_eq!(
        crate::links::panes_of(&state, &agent),
        vec![("handed1".to_string(), "p1".to_string())]
    );

    drop(window);
    server.abort();
    state.sessions.kill(&agent).ok();
}

#[tokio::test]
async fn no_app_window_on_the_workspace_and_no_chromium_is_a_refusal_naming_the_fix() {
    let state = quiet_state().await;
    let (agent, _ws) = agent_on(&state, "ab-none").await;
    let other = make_workspace(&state, "ab-other").await;
    let (addr, server) = serve(&state).await;
    let window = connect_window(addr, &other, &[]).await;
    wait_hello(&state, &other).await;
    // No Chromium either: `browser.path` names a browser that isn't there
    // (so the test never depends on, or launches, the host's own Chrome).
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(json!({"browser.path": "/nonexistent/chimaera-test-chrome", "git.path": QUIET_GIT})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/"}),
    )
    .await;
    let text = text_of(&r);
    assert!(is_error(&r), "{text}");
    assert!(
        text.contains("No browser engine can serve this session"),
        "{text}"
    );
    assert!(
        text.contains("none shows this session's workspace"),
        "{text}"
    );
    assert!(state.browsers.own(&agent).is_none());
    assert!(window.ops("open").is_empty());

    drop(window);
    server.abort();
    state.sessions.kill(&agent).ok();
}

#[tokio::test]
async fn browser_link_routes_validate_authenticate_and_ride_the_sessions_frame() {
    let state = quiet_state().await;
    let agent = inject_agent(&state, "ab");
    let other = inject_agent(&state, "ab2");

    // Every route needs the bearer.
    for (method, uri, body) in [
        (Method::GET, "/api/v1/links/browser", None),
        (
            Method::PUT,
            "/api/v1/links/browser",
            Some(json!({"tab": "t1", "agent_id": agent})),
        ),
        (Method::DELETE, "/api/v1/links/browser/t1", None),
    ] {
        let mut builder = Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        let res = app(state.clone())
            .oneshot(
                builder
                    .body(
                        body.map(|b| Body::from(b.to_string()))
                            .unwrap_or_else(Body::empty),
                    )
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }

    let put = |tab: &str, agent: &str| {
        request(
            &state,
            Method::PUT,
            "/api/v1/links/browser",
            Some(json!({"tab": tab, "agent_id": agent})),
        )
    };
    let (status, _) = put("bad tab!", &agent).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = put("t1", "s-nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Page ids per agent, smallest free first, stable while linked.
    assert_eq!(put("t1", &agent).await.1["page"], "p1");
    assert_eq!(put("t2", &agent).await.1["page"], "p2");
    assert_eq!(put("t1", &agent).await.1["page"], "p1");
    let (status, body) = put("t3", &other).await;
    assert_eq!((status, &body["page"]), (StatusCode::OK, &json!("p1")));
    let (status, _) = request(&state, Method::DELETE, "/api/v1/links/browser/t1", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(put("t4", &agent).await.1["page"], "p1");
    // Moving a tab to another agent reports where it came from.
    let (_, body) = put("t2", &other).await;
    assert_eq!(body["moved_from"], json!(agent));
    assert_eq!(body["page"], "p2");

    let frame: Value =
        serde_json::from_str(&crate::session_view::shared_sessions_snapshot(&state).await).unwrap();
    assert_eq!(
        frame["browser_links"],
        json!([
            {"tab": "t2", "agent_id": other, "page": "p2"},
            {"tab": "t3", "agent_id": other, "page": "p1"},
            {"tab": "t4", "agent_id": agent, "page": "p1"},
        ])
    );

    // A link dies with its agent.
    state.sessions.kill(&other).ok();
    for _ in 0..100 {
        if state.sessions.get(&other).is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (_, list) = request(&state, Method::GET, "/api/v1/links/browser", None).await;
    assert_eq!(
        list,
        json!([{"tab": "t4", "agent_id": agent, "page": "p1"}])
    );

    state.sessions.kill(&agent).ok();
}

async fn link_tab(state: &Arc<AppState>, tab: &str, agent: &str) -> String {
    let (status, body) = request(
        state,
        Method::PUT,
        "/api/v1/links/browser",
        Some(json!({"tab": tab, "agent_id": agent})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["page"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_shared_pane_never_retargets_an_agent_with_its_own_page() {
    let state = quiet_state().await;
    let (agent, ws) = agent_on(&state, "ab-default").await;
    let (addr, server) = serve(&state).await;
    let window = connect_window(addr, &ws, &["t1", "t2"]).await;
    wait_hello(&state, &ws).await;
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/app"}),
    )
    .await;
    assert!(!is_error(&r), "{r}");

    // The user shares a pane: the agent's next call still goes to its own page.
    link_tab(&state, "t1", &agent).await;
    let r = tool(&state, &agent, "browser_snapshot", json!({})).await;
    assert!(
        text_of(&r).starts_with("[browser · engine: app · page: own]"),
        "{r}"
    );
    assert!(window.requests().iter().all(|r| r["tab"].is_null()));

    // With no own page and several panes, the agent must say which.
    link_tab(&state, "t2", &agent).await;
    let r = tool(&state, &agent, "browser_close", json!({})).await;
    assert!(!is_error(&r), "{r}");
    let r = tool(&state, &agent, "browser_snapshot", json!({})).await;
    assert!(is_error(&r), "{r}");
    assert!(
        text_of(&r).contains("Several panes are shared with you (p1, p2)"),
        "{r}"
    );
    let r = tool(&state, &agent, "browser_snapshot", json!({"page": "p2"})).await;
    assert!(text_of(&r).contains("(p2)]"), "{r}");

    drop(window);
    server.abort();
    state.sessions.kill(&agent).ok();
}

#[tokio::test]
async fn an_engine_asked_for_is_honoured_only_when_a_page_starts() {
    let state = quiet_state().await;
    let (agent, ws) = agent_on(&state, "ab-engine").await;
    let (status, _) = request(
        &state,
        Method::PUT,
        "/api/v1/settings",
        Some(json!({"browser.path": "/nonexistent/chimaera-test-chrome", "git.path": QUIET_GIT})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (addr, server) = serve(&state).await;

    // No app window yet: asking for the app engine says why it can't.
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/", "engine": "app"}),
    )
    .await;
    assert!(
        is_error(&r) && text_of(&r).contains("The app engine can't start a page"),
        "{r}"
    );

    let window = connect_window(addr, &ws, &[]).await;
    wait_hello(&state, &ws).await;
    // Chromium asked for, none usable: no fallback to the app engine.
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/", "engine": "chromium"}),
    )
    .await;
    assert!(
        is_error(&r) && text_of(&r).contains("The Chromium engine can't start a page"),
        "{r}"
    );
    assert!(window.ops("open").is_empty());

    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/", "engine": "app"}),
    )
    .await;
    assert!(!is_error(&r), "{r}");
    // A page never changes engine.
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/x", "engine": "chromium"}),
    )
    .await;
    assert!(is_error(&r), "{r}");
    assert!(
        text_of(&r).contains("Close it with browser_close first"),
        "{r}"
    );
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/x", "engine": "warp"}),
    )
    .await;
    assert!(is_error(&r) && text_of(&r).contains("not an engine"), "{r}");

    drop(window);
    server.abort();
    state.sessions.kill(&agent).ok();
}

#[tokio::test]
async fn a_large_screenshot_comes_back_as_jpeg() {
    let state = quiet_state().await;
    let (agent, ws) = agent_on(&state, "ab-jpeg").await;
    let (addr, server) = serve(&state).await;
    let window = connect_window(addr, &ws, &[]).await;
    wait_hello(&state, &ws).await;
    let r = tool(
        &state,
        &agent,
        "browser_navigate",
        json!({"url": "http://localhost:5173/"}),
    )
    .await;
    assert!(!is_error(&r), "{r}");

    let r = tool(&state, &agent, "browser_screenshot", json!({"ref": "e1"})).await;
    assert!(!is_error(&r), "{r}");
    assert_eq!(r["content"][1]["mimeType"], "image/jpeg");
    assert_eq!(r["content"][1]["data"], "/9j/4AAQ");
    let shots: Vec<Value> = window
        .ops("drive")
        .into_iter()
        .filter(|d| d["action"]["type"] == "screenshot")
        .collect();
    assert_eq!(shots.len(), 2);
    assert_eq!(shots[0]["action"]["format"], "png");
    assert_eq!(shots[1]["action"]["format"], "jpeg");
    assert_eq!(shots[1]["action"]["quality"], 80);
    // A 100×100 element fits whole: width bounded by the long edge rule.
    assert_eq!(shots[0]["action"]["max_width"], 1600);

    // The viewport stays PNG.
    let r = tool(&state, &agent, "browser_screenshot", json!({})).await;
    assert_eq!(r["content"][1]["mimeType"], "image/png");

    drop(window);
    server.abort();
    state.sessions.kill(&agent).ok();
}

#[tokio::test]
async fn hand_overs_are_rate_limited_and_capped() {
    let state = quiet_state().await;
    let (agent, ws) = agent_on(&state, "ab-handovers").await;
    let (addr, server) = serve(&state).await;
    let window = connect_window(addr, &ws, &[]).await;
    wait_hello(&state, &ws).await;
    let show = |n: u32| {
        let state = state.clone();
        let agent = agent.clone();
        async move {
            let r = tool(
                &state,
                &agent,
                "browser_navigate",
                // After a hand-over the pane is the default page: ask for
                // a new own page explicitly.
                json!({"url": format!("http://localhost:5173/{n}"), "page": "own"}),
            )
            .await;
            assert!(!is_error(&r), "{r}");
            tool(&state, &agent, "browser_show", json!({"page": "own"})).await
        }
    };
    // open_browser's burst budget: three in ten seconds.
    for n in 1..=3 {
        let r = show(n).await;
        assert!(!is_error(&r), "{r}");
    }
    let r = show(4).await;
    assert!(is_error(&r), "{r}");
    assert!(
        text_of(&r).contains("already opened 3 browser panes"),
        "{r}"
    );
    assert_eq!(window.ops("handover").len(), 3);

    // Four handed-over panes still linked is the most (checked before the
    // rate limit, which is still spent here).
    link_tab(&state, "extra", &agent).await;
    state
        .browsers
        .update_shared("extra", |p| p.handed_over = true);
    let r = tool(&state, &agent, "browser_show", json!({"page": "own"})).await;
    assert!(is_error(&r), "{r}");
    assert!(text_of(&r).contains("4 at most"), "{r}");

    drop(window);
    server.abort();
    state.sessions.kill(&agent).ok();
}

#[tokio::test]
async fn the_shells_take_over_abort_pauses_the_pane() {
    let state = quiet_state().await;
    let (agent, ws) = agent_on(&state, "ab-abort").await;
    let (addr, server) = serve(&state).await;
    let window = connect_window(addr, &ws, &["t1"]).await;
    wait_hello(&state, &ws).await;
    link_tab(&state, "t1", &agent).await;

    let r = tool(&state, &agent, "browser_click", json!({"x": 777, "y": 1})).await;
    assert!(is_error(&r), "{r}");
    assert!(
        text_of(&r).contains("Paused: the user took over this page at "),
        "{r}"
    );
    assert!(state.browsers.shared("t1").paused.is_some());
    // And it stays paused for act tools until resume.
    let r = tool(&state, &agent, "browser_click", json!({"ref": "e1"})).await;
    assert!(is_error(&r) && text_of(&r).contains("Paused"), "{r}");

    drop(window);
    server.abort();
    state.sessions.kill(&agent).ok();
}
