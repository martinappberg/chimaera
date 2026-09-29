//! Loopback-only keeper fixture. Never enable this module in release bundles.
use crate::*;
use anyhow::{bail, Result};
use axum::{
    extract::{
        ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Path, Query, State,
    },
    http::StatusCode,
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{broadcast, mpsc, oneshot, watch, Mutex, Semaphore},
    time::Instant,
};
use tokio_tungstenite::tungstenite::Message;

pub const STATIC_TOKEN: &str = "fake-keeper-local-token";
const STATIC_REFRESH: &str = "fake-keeper-local-refresh";
#[derive(Clone)]
pub struct FakeKeeper {
    pub endpoint: String,
    inner: Arc<Inner>,
}
struct Inner {
    state: Mutex<Data>,
    events: broadcast::Sender<Event>,
    generation: watch::Sender<u64>,
    serve_generation: watch::Sender<u64>,
    streams: Arc<Semaphore>,
    handoff: crate::fake_handoff::FixtureHandoff,
}
struct Data {
    hosts: HashMap<String, Host>,
    targets: HashMap<String, SocketAddr>,
    codes: HashMap<String, Code>,
    access: HashMap<String, Instant>,
    refresh: HashMap<String, Instant>,
    prompts: HashMap<String, (Instant, Event)>,
    answers: Vec<EventCommand>,
    serve: Option<(String, mpsc::Sender<ServeEvent>)>,
    pending: HashMap<String, (String, oneshot::Sender<WebSocket>)>,
    events_epoch: u64,
}
struct Code {
    challenge: String,
    redirect: String,
    expires: Instant,
}
impl FakeKeeper {
    pub fn new(endpoint: String) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (events, _) = broadcast::channel(256);
        let (generation, _) = watch::channel(0);
        let (serve_generation, _) = watch::channel(0);
        Self {
            endpoint,
            inner: Arc::new(Inner {
                events,
                generation,
                serve_generation,
                streams: Arc::new(Semaphore::new(MAX_STREAMS)),
                handoff: Default::default(),
                state: Mutex::new(Data {
                    hosts: HashMap::new(),
                    targets: HashMap::new(),
                    codes: HashMap::new(),
                    access: HashMap::from([(
                        STATIC_TOKEN.into(),
                        Instant::now() + Duration::from_secs(3600),
                    )]),
                    refresh: HashMap::from([(
                        STATIC_REFRESH.into(),
                        Instant::now() + Duration::from_secs(86400),
                    )]),
                    prompts: HashMap::new(),
                    answers: Vec::new(),
                    serve: None,
                    pending: HashMap::new(),
                    events_epoch: 0,
                }),
            }),
        }
    }
    pub fn tokens() -> Tokens {
        Tokens {
            access_token: STATIC_TOKEN.into(),
            refresh_token: STATIC_REFRESH.into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
        }
    }
    pub async fn add_target(
        &self,
        alias: &str,
        address: SocketAddr,
        daemon: Option<Daemon>,
    ) -> Result<Host> {
        if address.ip() != Ipv4Addr::LOCALHOST {
            bail!("fake targets must be 127.0.0.1");
        }
        if alias.is_empty() || alias.len() > 255 {
            bail!("invalid alias");
        }
        let mut state = self.inner.state.lock().await;
        if state.hosts.len() >= MAX_STREAMS {
            bail!("fixture host limit");
        }
        let host = Host {
            id: format!("h-{}", nonce()),
            alias: alias.into(),
            kind: HostKind::Ssh,
            status: HostStatus::Connected,
            daemon,
            error: None,
        };
        state.targets.insert(host.id.clone(), address);
        state.hosts.insert(host.id.clone(), host.clone());
        let _ = self.inner.events.send(Event::Host { host: host.clone() });
        Ok(host)
    }
    pub(crate) fn invalidate_live_transports(&self) {
        self.inner
            .generation
            .send_modify(|generation| *generation += 1);
    }
    pub(crate) fn handoff(&self) -> &crate::fake_handoff::FixtureHandoff {
        &self.inner.handoff
    }
    /// Another authenticated device for cross-holder conformance checks.
    pub async fn add_device(&self, device_id: &str) -> Result<Tokens> {
        if device_id.is_empty() || device_id.len() > 128 {
            bail!("invalid fixture device");
        }
        let tokens = {
            let mut state = self.inner.state.lock().await;
            issue_tokens(&mut state)
        };
        self.inner
            .handoff
            .bind_device(&tokens.access_token, device_id)
            .await?;
        Ok(tokens)
    }
    pub fn router(&self) -> Router {
        let private = Router::new()
            .merge(crate::fake_handoff::routes())
            .route("/v1/me", get(me))
            .route(
                "/v1/worker/status",
                get(|| async {
                    Json(WorkerStatus {
                        state: WorkerState::Unavailable,
                        reason: Some(WorkerReason::ProvisioningDisabled),
                        phase: None,
                    })
                }),
            )
            .route("/v1/billing/checkout", post(fixture_checkout))
            .route("/v1/billing/portal", post(fixture_portal))
            .route("/v1/hosts", get(hosts).post(add_host))
            .route("/v1/hosts/{id}", axum::routing::delete(delete_host))
            .route("/v1/hosts/{id}/reconnect", post(reconnect))
            .route("/v1/devices", get(devices))
            .route("/v1/devices/{id}", axum::routing::delete(revoke))
            .route("/v1/sign-out-everywhere", post(sign_out))
            .route("/v1/events", get(events))
            .route("/v1/hosts/{id}/tcp", get(tcp))
            .route("/v1/serve", get(serve))
            .route("/v1/serve/{id}", get(reverse))
            .route("/_test/prompt", post(prompt))
            .route("/_test/answers", get(answers))
            .route("/_test/drop-events", post(drop_events))
            .route("/_test/expire-access", post(expire_access))
            .route_layer(middleware::from_fn_with_state(self.clone(), auth));
        private
            .merge(
                Router::new()
                    .route("/v1/oauth/authorize", get(authorize))
                    .route("/v1/oauth/token", post(token))
                    .route("/v1/oauth/refresh", post(refresh)),
            )
            .layer(DefaultBodyLimit::max(MAX_CONTROL_FRAME))
            .with_state(self.clone())
    }
}
async fn fixture_checkout(Json(body): Json<serde_json::Value>) -> Response {
    if !matches!(body["plan"].as_str(), Some("pro" | "max"))
        || !matches!(body["interval"].as_str(), Some("month" | "year"))
        || body["return_to"].as_str() != Some("desktop")
        || !fixture_billing_callback_valid(&body)
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    Json(BillingSession {
        url: "https://checkout.stripe.com/c/pay/fixture".into(),
    })
    .into_response()
}
fn fixture_billing_callback_valid(body: &serde_json::Value) -> bool {
    match body
        .get("desktop_callback")
        .filter(|value| !value.is_null())
    {
        None => true,
        Some(value) => {
            body["return_to"].as_str() == Some("desktop")
                && serde_json::from_value::<DesktopBillingCallback>(value.clone())
                    .is_ok_and(|callback| callback.validate().is_ok())
        }
    }
}
async fn fixture_portal(body: Option<Json<serde_json::Value>>) -> Response {
    if body.is_some_and(|Json(body)| {
        !fixture_billing_callback_valid(&body)
            || body.get("target").is_some_and(|target| {
                !serde_json::from_value::<BillingPortalTarget>(target.clone())
                    .is_ok_and(|target| target.validate().is_ok())
            })
    }) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    Json(BillingSession {
        url: "https://billing.stripe.com/p/session/fixture".into(),
    })
    .into_response()
}

fn nonce() -> String {
    base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        rand::random::<[u8; 32]>(),
    )
}
async fn auth(
    State(keeper): State<FakeKeeper>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let bearer = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let authorized = if let Some(token) = bearer {
        keeper
            .inner
            .state
            .lock()
            .await
            .access
            .get(token)
            .is_some_and(|expiry| *expiry > Instant::now())
    } else {
        false
    };
    let delegated = if let Some(token) = bearer {
        keeper
            .inner
            .handoff
            .allows(token, request.uri().path())
            .await
    } else {
        false
    };
    if authorized || delegated {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}
async fn me(State(keeper): State<FakeKeeper>) -> Json<Account> {
    Json(Account {
        account_id: "fake-account".into(),
        email: "developer@example.invalid".into(),
        plan: Plan::Pro,
        device_id: "fake-device".into(),
        protocol: PROTOCOL_VERSION,
        keeper_url: keeper.endpoint,
        limits: Limits {
            cloud_hours: 100,
            storage_bytes: 20_000_000_000,
        },
        usage: Usage {
            cloud_hours: 0.0,
            storage_bytes: 0,
        },
        hours_exhausted: false,
        payment_due: None,
        subscription_status: None,
        plans: None,
    })
}
async fn hosts(State(keeper): State<FakeKeeper>) -> Json<Vec<Host>> {
    Json(
        keeper
            .inner
            .state
            .lock()
            .await
            .hosts
            .values()
            .cloned()
            .collect(),
    )
}
async fn add_host(State(keeper): State<FakeKeeper>, Json(request): Json<AddHost>) -> Response {
    if request.alias.is_empty() || request.alias.len() > 255 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut state = keeper.inner.state.lock().await;
    if let Some(host) = state.hosts.values().find(|h| h.alias == request.alias) {
        return Json(host.clone()).into_response();
    }
    if state.hosts.len() >= MAX_STREAMS {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let host = Host {
        id: format!("h-{}", nonce()),
        alias: request.alias,
        kind: HostKind::Ssh,
        status: HostStatus::Offline,
        daemon: None,
        error: Some("No --host fixture target for this alias".into()),
    };
    state.hosts.insert(host.id.clone(), host.clone());
    let _ = keeper.inner.events.send(Event::Host { host: host.clone() });
    (StatusCode::CREATED, Json(host)).into_response()
}
async fn delete_host(State(keeper): State<FakeKeeper>, Path(id): Path<String>) -> StatusCode {
    let mut state = keeper.inner.state.lock().await;
    state.targets.remove(&id);
    if state.hosts.remove(&id).is_none() {
        return StatusCode::NOT_FOUND;
    }
    let _ = keeper.inner.events.send(Event::HostRemoved { host_id: id });
    StatusCode::NO_CONTENT
}
async fn reconnect(State(keeper): State<FakeKeeper>, Path(id): Path<String>) -> StatusCode {
    let state = keeper.inner.state.lock().await;
    let Some(host) = state.hosts.get(&id) else {
        return StatusCode::NOT_FOUND;
    };
    let _ = keeper.inner.events.send(Event::Host { host: host.clone() });
    StatusCode::NO_CONTENT
}
async fn devices() -> Json<Vec<Device>> {
    Json(vec![Device {
        id: "fake-device".into(),
        installation_id: None,
        name: "Development device".into(),
        last_seen: "2026-09-27T00:00:00Z".into(),
        current: true,
    }])
}
async fn revoke(State(keeper): State<FakeKeeper>, Path(id): Path<String>) -> StatusCode {
    if id != "fake-device" {
        return StatusCode::NOT_FOUND;
    }
    sign_out(State(keeper)).await
}
async fn sign_out(State(keeper): State<FakeKeeper>) -> StatusCode {
    keeper.inner.handoff.revoke_all().await;
    let mut state = keeper.inner.state.lock().await;
    state.access.clear();
    state.refresh.clear();
    state.prompts.clear();
    state.serve = None;
    state.pending.clear();
    for host in state.hosts.values_mut() {
        host.status = HostStatus::Offline;
        host.daemon = None;
    }
    keeper
        .inner
        .generation
        .send_modify(|generation| *generation += 1);
    StatusCode::NO_CONTENT
}
#[derive(Deserialize)]
struct Authorization {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    code_challenge_method: String,
}
async fn authorize(
    State(keeper): State<FakeKeeper>,
    Query(request): Query<Authorization>,
) -> Response {
    if request.response_type != "code"
        || request.client_id != "chimaera"
        || request.code_challenge_method != "S256"
        || request.code_challenge.len() != 43
        || request.state.len() > 256
        || crate::oauth::validate_redirect(&request.redirect_uri).is_err()
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let code = nonce();
    let mut data = keeper.inner.state.lock().await;
    data.codes.retain(|_, code| code.expires > Instant::now());
    if data.codes.len() >= 64 {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    data.codes.insert(
        code.clone(),
        Code {
            challenge: request.code_challenge,
            redirect: request.redirect_uri.clone(),
            expires: Instant::now() + Duration::from_secs(60),
        },
    );
    let mut redirect = url::Url::parse(&request.redirect_uri).expect("validated redirect");
    redirect
        .query_pairs_mut()
        .extend_pairs([("code", code.as_str()), ("state", request.state.as_str())]);
    let escaped = redirect
        .as_str()
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;");
    Html(format!("<!doctype html><title>Local keeper sign in</title><h1>Development sign in</h1><p>This loopback fixture does not contact an identity provider.</p><a href=\"{escaped}\">Sign in to local keeper</a>")).into_response()
}
fn issue_tokens(data: &mut Data) -> Tokens {
    data.access.retain(|_, expiry| *expiry > Instant::now());
    data.refresh.retain(|_, expiry| *expiry > Instant::now());
    if data.access.len() >= 256 {
        if let Some(oldest) = data
            .access
            .iter()
            .min_by_key(|(_, expiry)| *expiry)
            .map(|(token, _)| token.clone())
        {
            data.access.remove(&oldest);
        }
    }
    let tokens = Tokens {
        access_token: nonce(),
        refresh_token: nonce(),
        token_type: "Bearer".into(),
        expires_in: 900,
    };
    data.access.insert(
        tokens.access_token.clone(),
        Instant::now() + Duration::from_secs(900),
    );
    data.refresh.insert(
        tokens.refresh_token.clone(),
        Instant::now() + Duration::from_secs(86400),
    );
    tokens
}
async fn token(State(keeper): State<FakeKeeper>, Json(request): Json<TokenRequest>) -> Response {
    let mut data = keeper.inner.state.lock().await;
    let Some(code) = data.codes.remove(&request.code) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if request.grant_type != "authorization_code"
        || code.expires <= Instant::now()
        || code.redirect != request.redirect_uri
        || Pkce::challenge_for(&request.code_verifier) != code.challenge
        || request.code_verifier.len() < 43
        || request.code_verifier.len() > 128
        || data.refresh.len() >= 64
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    Json(issue_tokens(&mut data)).into_response()
}
async fn refresh(
    State(keeper): State<FakeKeeper>,
    Json(request): Json<RefreshRequest>,
) -> Response {
    let mut data = keeper.inner.state.lock().await;
    if data
        .refresh
        .remove(&request.refresh_token)
        .is_none_or(|expiry| expiry <= Instant::now())
    {
        // The account service answers every unknown, expired, revoked or
        // replayed refresh token with this exact OAuth error (RFC 6749 5.2).
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "invalid_grant".into(),
            }),
        )
            .into_response();
    }
    Json(issue_tokens(&mut data)).into_response()
}
fn configure(ws: WebSocketUpgrade, control: bool) -> WebSocketUpgrade {
    let size = if control {
        MAX_CONTROL_FRAME
    } else {
        MAX_DATA_FRAME
    };
    ws.max_frame_size(size)
        .max_message_size(size)
        .write_buffer_size(0)
        .max_write_buffer_size((MAX_IN_FLIGHT + 1) * size)
}
async fn events(State(keeper): State<FakeKeeper>, ws: WebSocketUpgrade) -> Response {
    configure(ws, true).on_upgrade(move |socket| event_loop(keeper, socket))
}
async fn event_loop(keeper: FakeKeeper, mut socket: WebSocket) {
    let mut revoked = keeper.inner.generation.subscribe();
    let mut events = keeper.inner.events.subscribe();
    let epoch;
    let initial;
    let prompts;
    {
        let mut data = keeper.inner.state.lock().await;
        data.events_epoch += 1;
        epoch = data.events_epoch;
        initial = data.hosts.values().cloned().collect::<Vec<_>>();
        prompts = data
            .prompts
            .values()
            .filter(|(expiry, _)| *expiry > Instant::now())
            .map(|(_, event)| event.clone())
            .collect::<Vec<_>>();
    }
    for host in initial {
        if send(&mut socket, &Event::Host { host }).await.is_err() {
            return;
        }
    }
    for prompt in prompts {
        if send(&mut socket, &prompt).await.is_err() {
            return;
        }
    }
    let mut expiry_tick = tokio::time::interval(Duration::from_secs(1));
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    let mut pong = Instant::now();
    loop {
        tokio::select! {
            _ = revoked.changed() => break,
            _ = expiry_tick.tick() => {
                let mut data = keeper.inner.state.lock().await;
                let expired = data.prompts.iter().filter(|(_, (expiry, _))| *expiry <= Instant::now()).map(|(id, _)| id.clone()).collect::<Vec<_>>();
                for id in expired { data.prompts.remove(&id); let _ = keeper.inner.events.send(Event::PromptClosed { id }); }
            },
            event = events.recv() => match event {
                Ok(event) => { if send(&mut socket, &event).await.is_err() { break; } }
                Err(_) => break,
            },
            message = socket.next() => match message {
                Some(Ok(AxumMessage::Text(text))) => {
                    let Ok(EventCommand::Answer { id, value }) = serde_json::from_str(&text) else { break; };
                    let mut data = keeper.inner.state.lock().await;
                    if data.prompts.remove(&id).is_some_and(|(expiry, _)| expiry > Instant::now()) {
                        if data.answers.len() >= 64 { data.answers.remove(0); }
                        data.answers.push(EventCommand::Answer { id: id.clone(), value });
                        let _ = keeper.inner.events.send(Event::PromptClosed { id });
                    }
                }
                Some(Ok(AxumMessage::Ping(data))) => { if socket.send(AxumMessage::Pong(data)).await.is_err() { break; } }
                Some(Ok(AxumMessage::Pong(_))) => { pong = Instant::now(); }
                _ => break,
            },
            _ = ping.tick() => {
                if pong.elapsed() >= Duration::from_secs(60) || keeper.inner.state.lock().await.events_epoch != epoch { break; }
                if socket.send(AxumMessage::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
    let _ = socket.close().await;
}
async fn tcp(
    State(keeper): State<FakeKeeper>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let Ok(permit) = keeper.inner.streams.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let mut serve_generation = keeper.inner.serve_generation.subscribe();
    let (target, serving) = {
        let data = keeper.inner.state.lock().await;
        let Some(host) = data.hosts.get(&id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if host.status != HostStatus::Connected {
            return StatusCode::CONFLICT.into_response();
        }
        (data.targets.get(&id).copied(), data.serve.clone())
    };
    if target.is_none() && serving.is_none() {
        return StatusCode::CONFLICT.into_response();
    }
    configure(ws, false).on_upgrade(move |socket| async move {
        let mut revoked = keeper.inner.generation.subscribe();
        let _permit = permit;
        let flow = async {
            if let Some(target) = target {
                if let Ok(Ok(tcp)) =
                    tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(target)).await
                {
                    let _ = bridge_axum(tcp, socket).await;
                }
            } else if let Some((owner, control)) = serving {
                let stream_id = nonce();
                let (tx, rx) = oneshot::channel();
                {
                    let mut data = keeper.inner.state.lock().await;
                    if data.pending.len() >= MAX_STREAMS {
                        return;
                    }
                    data.pending.insert(stream_id.clone(), (owner, tx));
                }
                if control
                    .send(ServeEvent::Open {
                        stream_id: stream_id.clone(),
                    })
                    .await
                    .is_ok()
                {
                    if let Ok(Ok(reverse)) = tokio::time::timeout(Duration::from_secs(15), rx).await
                    {
                        let (a, b) = tokio::io::duplex(MAX_DATA_FRAME);
                        let _ = tokio::join!(bridge_axum(a, socket), bridge_axum(b, reverse));
                    }
                }
                keeper.inner.state.lock().await.pending.remove(&stream_id);
                let _ = control.try_send(ServeEvent::Close { stream_id });
            }
        };
        tokio::select! { _ = revoked.changed() => {}, _ = serve_generation.changed(), if target.is_none() => {}, _ = flow => {} }
    })
}
async fn serve(State(keeper): State<FakeKeeper>, ws: WebSocketUpgrade) -> Response {
    configure(ws, true).on_upgrade(move |socket| serve_loop(keeper, socket))
}
async fn serve_loop(keeper: FakeKeeper, mut socket: WebSocket) {
    let mut revoked = keeper.inner.generation.subscribe();
    let Some(Ok(AxumMessage::Text(text))) =
        tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .ok()
            .flatten()
    else {
        return;
    };
    let Ok(ServeCommand::Register { alias, daemon }) = serde_json::from_str(&text) else {
        return;
    };
    if alias.is_empty() || alias.len() > 255 || daemon.token.len() > 4096 {
        return;
    }
    let owner = nonce();
    let id = "device-fake-device".to_string();
    let (tx, mut rx) = mpsc::channel(16);
    let host = Host {
        id: id.clone(),
        alias,
        kind: HostKind::Device,
        status: HostStatus::Connected,
        daemon: Some(daemon),
        error: None,
    };
    {
        let mut data = keeper.inner.state.lock().await;
        keeper
            .inner
            .serve_generation
            .send_modify(|generation| *generation += 1);
        data.serve = Some((owner.clone(), tx));
        data.pending.clear();
        data.hosts.insert(id.clone(), host.clone());
    }
    let _ = keeper.inner.events.send(Event::Host { host });
    if send(
        &mut socket,
        &ServeEvent::Registered {
            host_id: id.clone(),
        },
    )
    .await
    .is_err()
    {
        return;
    }
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    let mut pong = Instant::now();
    loop {
        tokio::select! {
            _ = revoked.changed() => break,
            event = rx.recv() => match event { Some(event) => if send(&mut socket, &event).await.is_err() { break; }, None => break },
            message = socket.next() => match message {
                Some(Ok(AxumMessage::Ping(data))) => { if socket.send(AxumMessage::Pong(data)).await.is_err() { break; } }
                Some(Ok(AxumMessage::Pong(_))) => { pong = Instant::now(); }
                _ => break,
            },
            _ = ping.tick() => {
                if pong.elapsed() >= Duration::from_secs(60) { break; }
                if socket.send(AxumMessage::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
    let mut data = keeper.inner.state.lock().await;
    if data
        .serve
        .as_ref()
        .is_some_and(|(current, _)| *current == owner)
    {
        data.serve = None;
        keeper
            .inner
            .serve_generation
            .send_modify(|generation| *generation += 1);
        data.pending.clear();
        if let Some(host) = data.hosts.get_mut(&id) {
            host.status = HostStatus::Offline;
            host.daemon = None;
            let _ = keeper.inner.events.send(Event::Host { host: host.clone() });
        }
    }
}
async fn reverse(
    State(keeper): State<FakeKeeper>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let sender = {
        let mut data = keeper.inner.state.lock().await;
        let Some((owner, _)) = &data.serve else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if data
            .pending
            .get(&id)
            .is_none_or(|(pending_owner, _)| pending_owner != owner)
        {
            return StatusCode::NOT_FOUND.into_response();
        }
        data.pending
            .remove(&id)
            .map(|(_, sender)| sender)
            .expect("checked pending stream")
    };
    configure(ws, false).on_upgrade(move |socket| async {
        let _ = sender.send(socket);
    })
}
#[derive(Deserialize, Serialize)]
pub struct PromptRequest {
    pub host_id: String,
    pub prompt: String,
    #[serde(default)]
    pub echo: bool,
}
async fn prompt(State(keeper): State<FakeKeeper>, Json(request): Json<PromptRequest>) -> Response {
    let id = nonce();
    let event = Event::Prompt {
        id: id.clone(),
        host_id: request.host_id.clone(),
        prompt: request.prompt.clone(),
        echo: request.echo,
    };
    {
        let mut data = keeper.inner.state.lock().await;
        data.prompts
            .retain(|_, (expiry, _)| *expiry > Instant::now());
        if !data.hosts.contains_key(&request.host_id) {
            return StatusCode::NOT_FOUND.into_response();
        }
        if data.prompts.len() >= 64 || request.prompt.len() > 8192 {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        data.prompts.insert(
            id.clone(),
            (Instant::now() + Duration::from_secs(179), event.clone()),
        );
    }
    let _ = keeper.inner.events.send(event);
    Json(serde_json::json!({"id": id})).into_response()
}
async fn answers(State(keeper): State<FakeKeeper>) -> Json<Vec<EventCommand>> {
    Json(keeper.inner.state.lock().await.answers.clone())
}
async fn drop_events(State(keeper): State<FakeKeeper>) -> StatusCode {
    keeper.inner.generation.send_modify(|v| *v += 1);
    StatusCode::NO_CONTENT
}
async fn expire_access(State(keeper): State<FakeKeeper>) -> StatusCode {
    keeper.inner.state.lock().await.access.clear();
    StatusCode::NO_CONTENT
}
async fn send<T: Serialize>(socket: &mut WebSocket, value: &T) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(10),
        socket.send(AxumMessage::Text(serde_json::to_string(value)?.into())),
    )
    .await??;
    Ok(())
}
/// Adapter used by the loopback fixture; production servers can use the same
/// bridge with their own authenticated WebSocket upgrade.
pub async fn bridge_axum<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    tcp: T,
    socket: WebSocket,
) -> Result<()> {
    let socket = socket
        .map(|result| {
            result
                .map(|message| match message {
                    AxumMessage::Binary(bytes) => Message::Binary(bytes),
                    AxumMessage::Text(text) => Message::Text(text.to_string().into()),
                    AxumMessage::Ping(bytes) => Message::Ping(bytes),
                    AxumMessage::Pong(bytes) => Message::Pong(bytes),
                    AxumMessage::Close(_) => Message::Close(None),
                })
                .map_err(|e| anyhow::anyhow!("{e}"))
        })
        .sink_map_err(|e| anyhow::anyhow!("{e}"))
        .with(|message| {
            futures::future::ready(Ok::<_, anyhow::Error>(match message {
                Message::Binary(bytes) => AxumMessage::Binary(bytes),
                Message::Text(text) => AxumMessage::Text(text.to_string().into()),
                Message::Ping(bytes) => AxumMessage::Ping(bytes),
                Message::Pong(bytes) => AxumMessage::Pong(bytes),
                _ => AxumMessage::Close(None),
            }))
        });
    bridge(tcp, socket).await
}
