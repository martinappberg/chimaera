//! Stable session identities routed to another authenticated daemon. Credentials
//! and loopback listeners are memory-only; cached rows never imply local ownership.
use crate::AppState;
use anyhow::{bail, Context, Result};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use futures::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{net::TcpStream, sync::Semaphore};

const MAX_HOSTS: usize = 32;
const MAX_ROWS: usize = 512;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
static REQUESTS: Semaphore = Semaphore::const_new(32);

#[derive(Default)]
pub(crate) struct Store {
    inner: Mutex<Data>,
    started: AtomicBool,
}
#[derive(Default)]
struct Data {
    routes: HashMap<String, Route>,
    rows: HashMap<String, Value>,
}
#[derive(Clone)]
struct Route {
    host_id: String,
    address: Option<std::net::SocketAddr>,
    token: String,
    workspaces: HashMap<String, u64>,
    generation: u64,
}
#[derive(Deserialize)]
pub(crate) struct Registration {
    host_id: String,
    endpoint: String,
    token: String,
    workspace_id: String,
    epoch: u64,
}
#[derive(Deserialize)]
pub(crate) struct Remove {
    host_id: String,
}
#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct SocketOptions {
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub wake: Option<String>,
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
}
fn endpoint(raw: &str) -> Result<std::net::SocketAddr> {
    let address: std::net::SocketAddr = raw
        .strip_prefix("http://")
        .context("placement endpoint must be HTTP loopback")?
        .trim_end_matches('/')
        .parse()?;
    if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
        bail!("placement endpoint must be literal 127.0.0.1");
    }
    Ok(address)
}
impl Store {
    fn register(&self, request: Registration) -> Result<()> {
        if !valid_id(&request.host_id)
            || !valid_id(&request.workspace_id)
            || request.token.is_empty()
            || request.token.len() > 8192
            || request.token.bytes().any(|b| b.is_ascii_control())
        {
            bail!("invalid placement registration");
        }
        let address = endpoint(&request.endpoint)?;
        let mut data = crate::lock(&self.inner);
        if !data.routes.contains_key(&request.host_id) && data.routes.len() >= MAX_HOSTS {
            bail!("placement host limit");
        }
        for existing in data.routes.values() {
            if existing.host_id != request.host_id
                && existing
                    .workspaces
                    .get(&request.workspace_id)
                    .is_some_and(|epoch| *epoch >= request.epoch)
            {
                bail!("stale placement epoch");
            }
        }
        if let Some(existing) = data.routes.get(&request.host_id) {
            if existing
                .workspaces
                .get(&request.workspace_id)
                .is_some_and(|epoch| *epoch > request.epoch)
            {
                bail!("stale placement epoch");
            }
            if !existing.workspaces.contains_key(&request.workspace_id)
                && existing.workspaces.len() >= 128
            {
                bail!("placement workspace limit");
            }
        }
        for existing in data.routes.values_mut() {
            if existing.host_id != request.host_id
                && existing.workspaces.remove(&request.workspace_id).is_some()
            {
                existing.generation = existing.generation.wrapping_add(1);
            }
        }
        let route = data
            .routes
            .entry(request.host_id.clone())
            .or_insert_with(|| Route {
                host_id: request.host_id,
                address: None,
                token: String::new(),
                workspaces: HashMap::new(),
                generation: 0,
            });
        if route.address == Some(address)
            && route.token == request.token
            && route.workspaces.get(&request.workspace_id) == Some(&request.epoch)
        {
            return Ok(());
        }
        route.address = Some(address);
        route.token = request.token;
        route.workspaces.insert(request.workspace_id, request.epoch);
        route.generation = route.generation.wrapping_add(1);
        Ok(())
    }
    fn for_session(&self, id: &str) -> Option<Route> {
        let data = crate::lock(&self.inner);
        let row = data.rows.get(id)?;
        let host = row["placement"]["remote"].as_str()?;
        let route = data.routes.get(host)?;
        route
            .workspaces
            .contains_key(row["workspace_id"].as_str()?)
            .then(|| route.clone())
    }
    fn current(&self, route: &Route) -> bool {
        crate::lock(&self.inner)
            .routes
            .get(&route.host_id)
            .is_some_and(|r| r.generation == route.generation && r.address.is_some())
    }
    fn for_workspace(&self, workspace: &str) -> Option<Route> {
        crate::lock(&self.inner)
            .routes
            .values()
            .find(|r| r.workspaces.contains_key(workspace))
            .cloned()
    }
    pub(crate) fn rows(&self) -> Vec<Value> {
        crate::lock(&self.inner).rows.values().cloned().collect()
    }
    pub(crate) fn clear_workspace(&self, workspace: &str) {
        let mut data = crate::lock(&self.inner);
        data.rows
            .retain(|_, row| row["workspace_id"].as_str() != Some(workspace));
        for route in data.routes.values_mut() {
            if route.workspaces.remove(workspace).is_some() {
                route.generation = route.generation.wrapping_add(1);
            }
        }
    }
    fn install(&self, route: &Route, rows: Vec<Value>) {
        let mut data = crate::lock(&self.inner);
        if data
            .routes
            .get(&route.host_id)
            .is_none_or(|current| current.generation != route.generation)
        {
            return;
        }
        data.rows
            .retain(|_, row| row["placement"]["remote"].as_str() != Some(&route.host_id));
        for mut row in rows.into_iter().take(MAX_ROWS) {
            if data.rows.len() >= MAX_ROWS {
                break;
            }
            let (Some(id), Some(workspace)) = (row["id"].as_str(), row["workspace_id"].as_str())
            else {
                continue;
            };
            if !valid_id(id)
                || !route.workspaces.contains_key(workspace)
                || !serde_json::to_vec(&row).is_ok_and(|bytes| bytes.len() <= 64 * 1024)
            {
                continue;
            }
            let id = id.to_string();
            let Some(map) = row.as_object_mut() else {
                continue;
            };
            map.insert("placement".into(), json!({"remote": route.host_id}));
            map.insert("placement_available".into(), json!(true));
            data.rows.insert(id, row);
        }
    }
}
pub(crate) async fn register(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Registration>,
) -> Response {
    if crate::lock(&state.workspaces)
        .get(&body.workspace_id)
        .is_none()
    {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"unknown_workspace"})),
        )
            .into_response();
    }
    match state.session_proxy.register(body) {
        Ok(()) => {
            state.changes.notify_waiters();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => (
            StatusCode::CONFLICT,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}
pub(crate) async fn remove(
    State(state): State<Arc<AppState>>,
    Query(body): Query<Remove>,
) -> StatusCode {
    let mut data = crate::lock(&state.session_proxy.inner);
    if let Some(route) = data.routes.get_mut(&body.host_id) {
        route.address = None;
        route.token.clear();
        route.generation = route.generation.wrapping_add(1);
    }
    for row in data.rows.values_mut() {
        if row["placement"]["remote"].as_str() == Some(&body.host_id) {
            row["placement_available"] = json!(false);
        }
    }
    drop(data);
    state.changes.notify_waiters();
    StatusCode::NO_CONTENT
}
pub(crate) fn start(state: Arc<AppState>) {
    if state.session_proxy.started.swap(true, Ordering::AcqRel) {
        return;
    }
    // Deliberate visibility exemption: one daemon-wide directory keeps remote
    // ownership visible after windows close; no poll wakes a sleeping worker.
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(5));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if state.stopping.load(Ordering::Acquire) {
                return;
            }
            let routes: Vec<_> = crate::lock(&state.session_proxy.inner)
                .routes
                .values()
                .filter(|r| r.address.is_some())
                .cloned()
                .collect();
            for route in routes {
                let result = tokio::time::timeout(Duration::from_secs(10), async {
                    let response = request(
                        &route,
                        Request::builder()
                            .uri("/api/v1/sessions")
                            .body(Body::empty())?,
                    )
                    .await?;
                    if !response.status().is_success() {
                        bail!("remote unavailable");
                    }
                    let bytes = axum::body::to_bytes(response.into_body(), MAX_RESPONSE).await?;
                    Ok::<Vec<Value>, anyhow::Error>(serde_json::from_slice(&bytes)?)
                })
                .await;
                match result {
                    Ok(Ok(rows)) => state.session_proxy.install(&route, rows),
                    _ => {
                        let mut data = crate::lock(&state.session_proxy.inner);
                        if data
                            .routes
                            .get(&route.host_id)
                            .is_none_or(|current| current.generation != route.generation)
                        {
                            continue;
                        }
                        for row in data.rows.values_mut() {
                            if row["placement"]["remote"].as_str() == Some(&route.host_id) {
                                row["placement_available"] = json!(false);
                            }
                        }
                    }
                }
            }
            if !crate::lock(&state.session_proxy.inner).routes.is_empty() {
                state.changes.notify_waiters();
            }
        }
    });
}
async fn request(route: &Route, mut request: Request<Body>) -> Result<Response> {
    let address = route.address.context("remote placement unavailable")?;
    let permit = REQUESTS.try_acquire().context("remote request limit")?;
    let stream =
        tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(address)).await??;
    let (mut client, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    request.headers_mut().remove(header::AUTHORIZATION);
    request.headers_mut().remove(header::COOKIE);
    request.headers_mut().remove(header::HOST);
    request
        .headers_mut()
        .insert(header::HOST, address.to_string().parse()?);
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {}", route.token).parse()?,
    );
    request
        .headers_mut()
        .insert(header::CONNECTION, "close".parse()?);
    tokio::spawn(async move {
        let _permit = permit;
        let _ = tokio::time::timeout(Duration::from_secs(120), connection).await;
    });
    Ok(client.send_request(request).await?.map(Body::new))
}
fn session_id(path: &str) -> Option<&str> {
    let rest = path
        .strip_prefix("/api/v1")
        .unwrap_or(path)
        .strip_prefix("/sessions/")?;
    let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
    (valid_id(id)
        && matches!(
            tail,
            "" | "journal" | "view" | "rewind" | "fork" | "exec" | "upload"
        ))
    .then_some(id)
}
pub(crate) async fn api_proxy(
    State(state): State<Arc<AppState>>,
    mut incoming: Request<Body>,
    next: Next,
) -> Response {
    let id = session_id(incoming.uri().path()).map(str::to_owned);
    let mut route = id
        .as_deref()
        .and_then(|id| state.session_proxy.for_session(id));
    if route.is_none() && incoming.method() != axum::http::Method::GET {
        if id
            .as_deref()
            .is_some_and(|id| !crate::ws::session_writable(&state, id))
        {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"workspace_owned_elsewhere"})),
            )
                .into_response();
        }
        if matches!(incoming.uri().path(), "/sessions" | "/api/v1/sessions")
            && incoming.method() == axum::http::Method::POST
        {
            let (parts, body) = incoming.into_parts();
            let Ok(bytes) = axum::body::to_bytes(body, MAX_RESPONSE).await else {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            };
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                route = value["workspace_id"]
                    .as_str()
                    .and_then(|workspace| state.session_proxy.for_workspace(workspace));
            }
            incoming = Request::from_parts(parts, Body::from(bytes));
        }
    }
    let Some(route) = route else {
        return next.run(incoming).await;
    };
    let path = incoming
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    if !path.starts_with("/api/v1/") {
        match format!("/api/v1{path}").parse() {
            Ok(uri) => *incoming.uri_mut() = uri,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    }
    match tokio::time::timeout(Duration::from_secs(30), request(&route, incoming)).await {
        Ok(Ok(response)) => response,
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"remote_unavailable"})),
        )
            .into_response(),
    }
}

/// Caller has consumed and authenticated the local first frame. Remote tokens
/// replace only that frame's token; application messages remain byte-for-byte.
pub(crate) async fn socket(
    state: &AppState,
    id: &str,
    kind: &str,
    options: &SocketOptions,
    mut auth: Value,
    downstream: &mut axum::extract::ws::WebSocket,
) -> bool {
    use axum::extract::ws::Message as Down;
    use tokio_tungstenite::tungstenite::Message as Up;
    let Some(route) = state.session_proxy.for_session(id) else {
        return false;
    };
    let result: Result<()> = async {
        let _permit = REQUESTS.try_acquire().context("remote stream limit")?;
        let address = route.address.context("remote placement unavailable")?;
        let query = if options.read_only { "?read_only=true" } else if options.wake.as_deref() == Some("interaction") { "?wake=interaction" } else { "" };
        let url = format!("ws://{address}/ws/{kind}/{id}{query}");
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default().max_message_size(Some(10 * 1024 * 1024)).max_frame_size(Some(10 * 1024 * 1024));
        let (mut upstream, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async_with_config(url, Some(config), false)).await??;
        auth["token"] = json!(route.token);
        bounded_send(&mut upstream, Up::Text(auth.to_string().into())).await?;
        let mut ownership = tokio::time::interval(Duration::from_secs(2));
        ownership.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ownership.tick() => if !state.session_proxy.current(&route) { bail!("placement changed"); },
                next = upstream.next() => match next {
                    Some(Ok(Up::Text(text))) => bounded_send(downstream, Down::Text(text.to_string().into())).await?,
                    Some(Ok(Up::Binary(bytes))) => bounded_send(downstream, Down::Binary(bytes)).await?,
                    Some(Ok(Up::Ping(bytes))) => bounded_send(&mut upstream, Up::Pong(bytes)).await?,
                    Some(Ok(Up::Close(_))) | None => break,
                    Some(Err(error)) => return Err(error.into()),
                    _ => {},
                },
                next = downstream.recv() => match next {
                    Some(Ok(Down::Text(text))) => bounded_send(&mut upstream, Up::Text(text.to_string().into())).await?,
                    Some(Ok(Down::Binary(bytes))) => bounded_send(&mut upstream, Up::Binary(bytes)).await?,
                    Some(Ok(Down::Ping(bytes))) => bounded_send(downstream, Down::Pong(bytes)).await?,
                    Some(Ok(Down::Close(_))) | None => break,
                    Some(Err(error)) => return Err(error.into()),
                    _ => {},
                },
            }
        }
        Ok(())
    }.await;
    if result.is_err() {
        let _ = bounded_send(downstream, Down::Text(json!({"type":"error","code":"remote_unavailable","message":"Session host is unavailable"}).to_string().into())).await;
    }
    true
}

async fn bounded_send<S, M>(sink: &mut S, message: M) -> Result<()>
where
    S: futures::Sink<M> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    tokio::time::timeout(Duration::from_secs(20), sink.send(message)).await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destination_and_epoch_checks_prevent_route_confusion() {
        assert!(endpoint("http://127.0.0.1:1234").is_ok());
        for unsafe_url in [
            "http://localhost:80",
            "http://127.0.0.1:80/path",
            "https://example.com",
            "http://user@127.0.0.1:80",
        ] {
            assert!(endpoint(unsafe_url).is_err());
        }
        let store = Store::default();
        let registration = |host: &str, epoch| Registration {
            host_id: host.into(),
            endpoint: "http://127.0.0.1:1234".into(),
            token: "fixture".into(),
            workspace_id: "w-test".into(),
            epoch,
        };
        store.register(registration("worker-a", 4)).unwrap();
        assert!(store.register(registration("worker-b", 4)).is_err());
        store.register(registration("worker-b", 5)).unwrap();
        assert!(store.register(registration("worker-a", 3)).is_err());
        assert!(session_id("/sessions/s-ok/journal").is_some());
        assert!(session_id("/sessions/s-ok/../../shutdown").is_none());
    }
}
