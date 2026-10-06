//! Original real daemon/agent transport setup shared only by explicit fixtures
//! and the unchanged public Feed tests. Private policy assertions live privately.
use super::super::*;
use crate::router::app;
use crate::*;
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use std::{path::PathBuf, sync::Arc};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;
fn test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "chimaera-viewer-test-{}-{label}-{}",
        std::process::id(),
        chimaera_core::generate_token()
    ));
    std::fs::create_dir(&dir).unwrap();
    dir
}
fn fixture_state() -> AppState {
    let data = test_dir("data");
    let mut state = AppState::new(
        "test-token".into(),
        "testhost".into(),
        4242,
        0,
        data.clone(),
        data.join("config"),
    );
    state.claude_projects_dir = data.join("provider-home/.claude/projects");
    state.claude_settings_path = data.join("provider-home/.claude/settings.json");
    state.codex_config_path = data.join("provider-home/.codex/config.toml");
    state.managed_root = data.join("managed-agents");
    state.legacy_managed_root = None;
    state
}
fn test_state() -> Arc<AppState> {
    Arc::new(fixture_state())
}
fn write_fake_claude(label: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = test_dir(label).join("claude");
    std::fs::write(&path, "#!/bin/sh\n\
         printf '%s\\n' '{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"init\",\"response\":{\"commands\":[]}}}'\n\
         cat >/dev/null\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}
async fn register_request(
    state: &Arc<AppState>,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    assert_eq!(method, Method::POST);
    assert_eq!(path, "/api/v1/pro/placements");
    // Use the original authenticated router path: constructing it starts the
    // viewing daemon's passive roster poll before we wait for its remote row.
    let body_value = body.unwrap();
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, "Bearer test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body_value.to_string()))
        .unwrap();
    let router = app(state.clone());
    let response = if state.daemon_extension.is_none() {
        // The legacy scenario: a viewing daemon without the extension, which
        // has no `/pro/*` routes, holding a placement it was given directly.
        let registration = serde_json::from_value(body_value).unwrap();
        crate::session_proxy::register(
            axum::extract::State(state.clone()),
            axum::Json(registration),
        )
        .await
    } else {
        router.oneshot(request).await.unwrap()
    };
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, value)
}
async fn next_ws_frame<S>(socket: &mut S) -> Message
where
    S: futures::Stream<Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("ws frame timeout")
        .expect("ws stream ended")
        .expect("ws frame error")
}
/// An in-process stand-in for the account transport in front of a real
/// daemon: while "asleep" it answers health for the owner (marked sleeping)
/// and refuses socket upgrades that carry no interaction, exactly as the
/// transport contract says. A wake-marked upgrade resumes the owner.
///
/// With `front_door` it is the newer transport instead (VIEWING.md, "A
/// sleeping cloud machine's sockets"): it takes every socket upgrade itself
/// whether the owner sleeps or not, marks it `X-Chimaera-Sockets: kept`, and
/// keeps the viewer's side open across the owner's suspend and resume (see
/// [`FakeTransport::keep`]).
#[derive(Default)]
pub struct FakeTransport {
    pub asleep: std::sync::atomic::AtomicBool,
    pub refuse_wake: std::sync::atomic::AtomicBool,
    /// Keep a waking owner waking until the test releases it.
    pub hold_wake: std::sync::atomic::AtomicBool,
    pub release: tokio::sync::Notify,
    pub upgrades: std::sync::Mutex<Vec<String>>,
    pub front_door: std::sync::atomic::AtomicBool,
    /// The scope probe fails outright: the owner cannot be reached.
    pub fail_health: std::sync::atomic::AtomicBool,
    /// The daemon itself, for the front door's own attaches.
    daemon: std::sync::OnceLock<std::net::SocketAddr>,
    /// Sockets the front door took, by path.
    pub kept: std::sync::Mutex<Vec<String>>,
    /// Times the front door attached a kept socket to the daemon.
    pub attaches: std::sync::atomic::AtomicUsize,
    /// Frames a viewer sent after its first while the owner slept, as
    /// `(path, was binary)`: what the front door had to hold or drop.
    pub detached: std::sync::Mutex<Vec<(String, bool)>>,
    /// Scope probes (`/api/v1/health`) that reached the transport.
    pub probes: std::sync::atomic::AtomicUsize,
    /// Accept a socket upgrade and drop it at once.
    pub drop_accepted: std::sync::atomic::AtomicBool,
    /// Close every kept socket (the front door's side ends).
    pub close_kept: std::sync::atomic::AtomicBool,
    /// The authentication each kept terminal would attach with now.
    pub remembered: std::sync::Mutex<Vec<serde_json::Value>>,
    /// Accept a socket upgrade that carries no wake marker, unmarked, and
    /// say nothing on it until the test releases it (it then closes).
    pub stall_accepted: std::sync::atomic::AtomicBool,
    pub unstall: tokio::sync::Notify,
}
impl FakeTransport {
    fn serve(self: &Arc<Self>, daemon: Arc<AppState>) -> axum::Router {
        use axum::response::IntoResponse;
        use std::sync::atomic::Ordering;
        let transport = self.clone();
        app(daemon).layer(axum::middleware::from_fn(
            move |request: Request<Body>, next: axum::middleware::Next| {
                let transport = transport.clone();
                async move {
                    let path = request.uri().path().to_owned();
                    let query = request.uri().query().unwrap_or_default().to_owned();
                    if path == "/api/v1/health" {
                        transport.probes.fetch_add(1, Ordering::AcqRel);
                    }
                    if path.starts_with("/ws/") {
                        transport.upgrades.lock().unwrap().push(query.clone());
                        let front_door = transport.front_door.load(Ordering::Acquire);
                        let dropped = transport.drop_accepted.load(Ordering::Acquire);
                        let stalled = transport.stall_accepted.load(Ordering::Acquire)
                            && !query.contains("wake=interaction");
                        if front_door || dropped || stalled {
                            use axum::extract::FromRequestParts;
                            let (mut parts, _) = request.into_parts();
                            let upgrade = axum::extract::WebSocketUpgrade::from_request_parts(
                                &mut parts,
                                &(),
                            )
                            .await
                            .unwrap();
                            let target = if query.is_empty() {
                                path.clone()
                            } else {
                                format!("{path}?{query}")
                            };
                            let mut response = if dropped {
                                upgrade.on_upgrade(|socket| async move { drop(socket) })
                            } else if stalled {
                                upgrade.on_upgrade(move |mut socket| async move {
                                    tokio::select! {
                                        _ = transport.unstall.notified() => {}
                                        _ = async { while socket.recv().await.is_some() {} } => {}
                                    }
                                })
                            } else {
                                transport.kept.lock().unwrap().push(path);
                                upgrade
                                    .max_message_size(16 * 1024 * 1024)
                                    .on_upgrade(move |socket| transport.keep(socket, target))
                            };
                            // Every upgrade a keeping transport accepts says so.
                            if front_door {
                                response
                                    .headers_mut()
                                    .insert("x-chimaera-sockets", "kept".parse().unwrap());
                            }
                            return response;
                        }
                        if transport.asleep.load(Ordering::Acquire) {
                            if !query.contains("wake=interaction")
                                || transport.refuse_wake.load(Ordering::Acquire)
                            {
                                return (
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    axum::Json(serde_json::json!({"error":"worker_asleep"})),
                                )
                                    .into_response();
                            }
                            if transport.hold_wake.load(Ordering::Acquire) {
                                transport.release.notified().await;
                            }
                            transport.asleep.store(false, Ordering::Release);
                        }
                    } else if path == "/api/v1/health"
                        && transport.fail_health.load(Ordering::Acquire)
                    {
                        return StatusCode::BAD_GATEWAY.into_response();
                    } else if path == "/api/v1/health" && transport.asleep.load(Ordering::Acquire) {
                        let mut response = axum::Json(serde_json::json!({"pid":1})).into_response();
                        response
                            .headers_mut()
                            .insert("x-chimaera-worker-state", "sleeping".parse().unwrap());
                        return response;
                    }
                    next.run(request).await
                }
            },
        ))
    }

    /// One viewer socket behind the front door. The first frame is its
    /// authentication and is remembered. Until the daemon has answered
    /// `ready` (asleep, waking, or just attached), what a chat sends and what
    /// a terminal types is held, and wakes a sleeping owner (unless
    /// `hold_wake`), which the viewer hears once as `waking`. A terminal's
    /// grid control is never held: it is folded into the remembered
    /// authentication; an events registration is neither held nor a reason
    /// to wake. Awake, the socket is attached with the remembered frame (a
    /// chat's `last_seq` raised to what this viewer already received) and,
    /// after the daemon's `ready` (an events socket has none), what was held
    /// is delivered once, in order. A suspension drops only the attach. When
    /// the viewer's side closes, what is still held is discarded.
    async fn keep(self: Arc<Self>, mut viewer: axum::extract::ws::WebSocket, target: String) {
        use axum::extract::ws::Message as Viewer;
        use futures::StreamExt;
        use std::sync::atomic::Ordering;
        let path = target.split('?').next().unwrap_or_default().to_owned();
        let chat = path.starts_with("/ws/chat/");
        let events = path == "/ws/events";
        let Some(Ok(Viewer::Text(first))) = viewer.recv().await else {
            return;
        };
        let mut auth: serde_json::Value = serde_json::from_str(&first).unwrap();
        let terminal = !chat && !events;
        let slot = terminal.then(|| {
            let mut remembered = self.remembered.lock().unwrap();
            remembered.push(auth.clone());
            remembered.len() - 1
        });
        let mut seen = 0u64;
        let mut held: std::collections::VecDeque<Message> = Default::default();
        let mut upstream = None;
        let mut ready = false;
        let mut said_waking = false;
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(20));
        loop {
            if self.close_kept.load(Ordering::Acquire) {
                return;
            }
            let asleep = self.asleep.load(Ordering::Acquire);
            if asleep && upstream.is_some() {
                upstream = None;
                ready = false;
                said_waking = false;
            }
            if !asleep && upstream.is_none() {
                let daemon = self.daemon.get().unwrap();
                let (mut socket, _) =
                    tokio_tungstenite::connect_async(format!("ws://{daemon}{target}"))
                        .await
                        .unwrap();
                if chat {
                    auth["last_seq"] =
                        serde_json::json!(seen.max(auth["last_seq"].as_u64().unwrap_or(0)));
                }
                socket
                    .send(Message::Text(auth.to_string().into()))
                    .await
                    .unwrap();
                self.attaches.fetch_add(1, Ordering::AcqRel);
                ready = events;
                upstream = Some(socket);
            }
            tokio::select! {
                _ = tick.tick() => {}
                frame = viewer.recv() => {
                    let frame = match frame {
                        Some(Ok(Viewer::Text(text))) => Message::Text(text.as_str().into()),
                        Some(Ok(Viewer::Binary(bytes))) => Message::Binary(bytes),
                        Some(Ok(Viewer::Close(_))) | None | Some(Err(_)) => return,
                        _ => continue,
                    };
                    if let (Some(slot), Message::Text(text)) = (slot, &frame) {
                        let control: serde_json::Value =
                            serde_json::from_str(text).unwrap_or_default();
                        match control["type"].as_str() {
                            Some("resize") => {
                                auth["cols"] = control["cols"].clone();
                                auth["rows"] = control["rows"].clone();
                            }
                            Some("park") => auth["parked"] = serde_json::json!(true),
                            Some("unpark") => auth["parked"] = serde_json::json!(false),
                            _ => {}
                        }
                        self.remembered.lock().unwrap()[slot] = auth.clone();
                    }
                    if let (Some(socket), true) = (upstream.as_mut(), ready) {
                        socket.send(frame).await.unwrap();
                        continue;
                    }
                    let binary = matches!(frame, Message::Binary(_));
                    self.detached.lock().unwrap().push((path.clone(), binary));
                    if !(chat || binary) {
                        continue;
                    }
                    held.push_back(frame);
                    if !self.hold_wake.load(Ordering::Acquire) {
                        self.asleep.store(false, Ordering::Release);
                    }
                    if !said_waking {
                        said_waking = true;
                        let waking = serde_json::json!({"type":"waking"}).to_string();
                        if viewer.send(Viewer::Text(waking.into())).await.is_err() {
                            return;
                        }
                    }
                }
                frame = async { upstream.as_mut().unwrap().next().await }, if upstream.is_some() => {
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            let value: serde_json::Value =
                                serde_json::from_str(&text).unwrap_or_default();
                            for event in value["events"].as_array().into_iter().flatten().chain([&value]) {
                                seen = seen.max(event["seq"].as_u64().unwrap_or(0));
                            }
                            if viewer.send(Viewer::Text(text.as_str().into())).await.is_err() {
                                return;
                            }
                            if value["type"] == "ready" && !ready {
                                ready = true;
                                let socket = upstream.as_mut().unwrap();
                                for frame in held.drain(..) {
                                    socket.send(frame).await.unwrap();
                                }
                            }
                        }
                        Some(Ok(Message::Binary(bytes))) => {
                            if viewer.send(Viewer::Binary(bytes)).await.is_err() {
                                return;
                            }
                        }
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                        _ => {}
                    }
                }
            }
        }
    }
}

pub struct SleepingChat {
    pub(crate) remote: Arc<AppState>,
    pub(crate) local: Arc<AppState>,
    pub workspace: String,
    pub id: String,
    #[cfg(feature = "daemon-extension-fixture")]
    pub capture: PathBuf,
    pub transport: Arc<FakeTransport>,
    #[cfg(feature = "daemon-extension-fixture")]
    pub transport_addr: std::net::SocketAddr,
    pub local_addr: std::net::SocketAddr,
}
impl Drop for SleepingChat {
    fn drop(&mut self) {
        self.remote.chat.kill(&self.id);
        self.remote.sessions.kill(&self.id).ok();
        for state in [&self.remote, &self.local] {
            state
                .stopping
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

/// A remote chat whose fake agent writes its stdin to `capture`, served
/// behind a sleeping fake transport, with a viewing daemon routed to it.
pub async fn sleeping_remote_chat(label: &str) -> SleepingChat {
    sleeping_remote(label, false).await
}

/// [`sleeping_remote_chat`], or a terminal whose process writes its input
/// to `capture`.
pub async fn sleeping_remote(label: &str, terminal: bool) -> SleepingChat {
    routed_remote(label, terminal, "worker-sleepy", true, None).await
}

/// A remote session behind a fake transport, routed to from a viewing daemon
/// as host `host` (`worker-…` is a cloud machine, `device-…` another of the
/// user's computers), asleep or awake.
pub async fn routed_remote(
    label: &str,
    terminal: bool,
    host: &str,
    asleep: bool,
    factory: Option<fn() -> Arc<dyn crate::daemon_extension::Runtime>>,
) -> SleepingChat {
    let remote = test_state();
    let mut local = fixture_state();
    local.daemon_extension = factory.map(|factory| factory());
    let local = Arc::new(local);
    let workspace = lock(&remote.workspaces)
        .add(test_dir(&format!("{label}-remote")).canonicalize().unwrap())
        .unwrap();
    let mut viewing = workspace.clone();
    viewing.root = test_dir(&format!("{label}-local")).canonicalize().unwrap();
    lock(&local.workspaces).import_exact(viewing).unwrap();
    let capture = workspace.root.join("agent-stdin.txt");
    let fake = write_fake_claude(&format!("{label}-agent"));
    let script = std::fs::read_to_string(&fake).unwrap();
    std::fs::write(
        &fake,
        script.replace("cat >/dev/null", "cat > \"$CHIMAERA_TEST_CAPTURE\""),
    )
    .unwrap();
    let id = format!("s-{label}");
    if terminal {
        remote
            .sessions
            .spawn(chimaera_pty::SpawnOpts {
                cwd: workspace.root.clone(),
                name: None,
                cols: 80,
                rows: 24,
                command: Some(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("cat > '{}'", capture.display()),
                ]),
                id: Some(id.clone()),
                env: Vec::new(),
                env_remove: Vec::new(),
                scrollback: None,
            })
            .unwrap();
    } else {
        let mut spec = chimaera_agent::driver::SpawnSpec::new(
            id.clone(),
            vec![fake.to_string_lossy().into_owned()],
            workspace.root.clone(),
        );
        spec.env.push((
            "CHIMAERA_TEST_CAPTURE".into(),
            capture.to_string_lossy().into_owned(),
        ));
        remote
            .chat
            .spawn(&chimaera_agent::claude::ClaudeAdapter, spec)
            .unwrap();
    }
    lock(&remote.session_workspaces).insert(id.clone(), workspace.id.clone());
    pro::install_execution_fixture(&remote, &workspace.id, 4).unwrap();
    let transport = Arc::new(FakeTransport::default());
    transport
        .asleep
        .store(asleep, std::sync::atomic::Ordering::Release);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let transport_addr = listener.local_addr().unwrap();
    let router = transport.serve(remote.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    // The daemon without the transport in front: where a front door attaches.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    transport
        .daemon
        .set(listener.local_addr().unwrap())
        .unwrap();
    let router = app(remote.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (status, error) = register_request(
        &local,
        Method::POST,
        "/api/v1/pro/placements",
        Some(serde_json::json!({
            "host_id":host,"endpoint":format!("http://{transport_addr}"),
            "token":"test-token","workspace_id":workspace.id,"epoch":4
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{error}");
    // The roster poll is a passive read: the transport answers it and the
    // session becomes routable without anything waking.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !session_view::sessions_json(&local)
        .iter()
        .any(|row| row["id"] == id.as_str())
    {
        assert!(tokio::time::Instant::now() < deadline, "row never appeared");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    let router = app(local.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    SleepingChat {
        remote,
        local,
        workspace: workspace.id,
        id,
        #[cfg(feature = "daemon-extension-fixture")]
        capture,
        transport,
        #[cfg(feature = "daemon-extension-fixture")]
        transport_addr,
        local_addr,
    }
}

pub async fn next_json<S>(socket: &mut S) -> serde_json::Value
where
    S: futures::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    loop {
        if let Message::Text(text) = next_ws_frame(socket).await {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

pub async fn open_chat(
    fixture: &SleepingChat,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (mut socket, _) = tokio_tungstenite::connect_async(format!(
        "ws://{}/ws/chat/{}",
        fixture.local_addr, fixture.id
    ))
    .await
    .unwrap();
    socket
        .send(Message::Text(
            serde_json::json!({"type":"auth","token":"test-token","last_seq":0})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    socket
}

/// User turns the fake agent received carrying `text` (the driver's own
/// title request also quotes the text, so count only user messages).
#[cfg(feature = "daemon-extension-fixture")]
pub fn user_turns(capture: &std::path::Path, text: &str) -> usize {
    std::fs::read_to_string(capture)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(r#""type":"user""#) && line.contains(text))
        .count()
}

#[cfg(feature = "daemon-extension-fixture")]
pub fn send_text(text: &str) -> Message {
    Message::Text(
        serde_json::json!({"type":"send","blocks":[{"type":"text","text":text}]})
            .to_string()
            .into(),
    )
}

/// A send made under the id its client minted for it.
#[cfg(feature = "daemon-extension-fixture")]
pub fn send_as(text: &str, client_id: &str) -> Message {
    command(serde_json::json!({
        "type":"send","blocks":[{"type":"text","text":text}],"client_id":client_id
    }))
}

#[cfg(feature = "daemon-extension-fixture")]
pub fn command(frame: serde_json::Value) -> Message {
    Message::Text(frame.to_string().into())
}

/// The chat commands that are neither the user acting nor a setting: the
/// automatic thinking push, the reads, withdrawing a send, and a command
/// this daemon has never heard of.
#[cfg(feature = "daemon-extension-fixture")]
pub fn passive_commands(cancel: &str) -> Vec<Message> {
    vec![
        command(serde_json::json!({"type":"set_thinking","enabled":true})),
        command(serde_json::json!({"type":"get_usage"})),
        command(serde_json::json!({"type":"get_mcp"})),
        command(serde_json::json!({"type":"from_the_future","x":1})),
        command(serde_json::json!({"type":"cancel_send","client_id":cancel})),
    ]
}

#[cfg(feature = "daemon-extension-fixture")]
impl SleepingChat {
    pub async fn viewer_marker(&self) -> StatusCode {
        crate::settings::put_settings(
            State(self.local.clone()),
            bytes::Bytes::from_static(br#"{"test.viewer_marker":"still-here"}"#),
        )
        .await
        .status()
    }
    pub async fn replace_owner(&self) -> StatusCode {
        let body = super::super::Registration {
            host_id: "worker-next".into(),
            endpoint: format!("http://{}", self.transport_addr),
            token: "test-token".into(),
            workspace_id: self.workspace.clone(),
            epoch: 5,
        };
        super::super::register(State(self.local.clone()), Json(body))
            .await
            .status()
    }
    pub fn eligible_device(&self, endpoint: &str) {
        pro::device_fixture(&self.local, endpoint);
    }
    pub fn local_chat_absent(&self) -> bool {
        self.local.chat.get(&self.id).is_none()
    }
    pub fn local_sessions_empty(&self) -> bool {
        self.local.sessions.list().is_empty()
    }
    pub fn confirmed(&self, id: &str) -> bool {
        self.remote.chat.client_id_state(&self.id, id)
            == Some(chimaera_agent::ClientIdState::Confirmed)
    }
    pub fn unknown(&self, id: &str) -> bool {
        self.remote.chat.client_id_state(&self.id, id).is_none()
    }
}
