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
const MAX_METADATA: usize = 1024 * 1024;
const POLL_CONCURRENCY: usize = 4;
static REQUESTS: Semaphore = Semaphore::const_new(32);

#[derive(Default)]
pub(crate) struct Store {
    inner: Mutex<Data>,
    started: AtomicBool,
}
#[derive(Default)]
struct Data {
    generation: u64,
    routes: HashMap<String, Route>,
    rows: HashMap<String, Value>,
    tickets: HashMap<String, Ticket>,
}
#[derive(Clone)]
struct Ticket {
    route: Route,
    workspace: String,
    upstream: String,
    expires: std::time::Instant,
}
#[derive(Clone)]
struct Route {
    host_id: String,
    address: Option<std::net::SocketAddr>,
    token: String,
    workspaces: HashMap<String, u64>,
    roots: HashMap<String, std::path::PathBuf>,
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
#[derive(Serialize)]
struct RegisteredPlacement {
    host_id: String,
    workspace_id: String,
    epoch: u64,
}
#[derive(Deserialize)]
pub(crate) struct Remove {
    host_id: Option<String>,
    workspace_id: Option<String>,
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
    fn register(&self, request: Registration, local_root: std::path::PathBuf) -> Result<()> {
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
        data.generation = data.generation.wrapping_add(1);
        let generation = data.generation;
        for existing in data.routes.values_mut() {
            if existing.host_id != request.host_id
                && existing.workspaces.remove(&request.workspace_id).is_some()
            {
                existing.roots.remove(&request.workspace_id);
                existing.generation = generation;
            }
        }
        data.routes.retain(|_, route| !route.workspaces.is_empty());
        let route = data
            .routes
            .entry(request.host_id.clone())
            .or_insert_with(|| Route {
                host_id: request.host_id,
                address: None,
                token: String::new(),
                workspaces: HashMap::new(),
                roots: HashMap::new(),
                generation: 0,
            });
        if route.address == Some(address)
            && route.token == request.token
            && route.workspaces.get(&request.workspace_id) == Some(&request.epoch)
            && route.roots.get(&request.workspace_id) == Some(&local_root)
        {
            return Ok(());
        }
        route.address = Some(address);
        route.token = request.token;
        route.roots.insert(request.workspace_id.clone(), local_root);
        route.workspaces.insert(request.workspace_id, request.epoch);
        route.generation = generation;
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
    fn inventory(&self) -> Vec<RegisteredPlacement> {
        let data = crate::lock(&self.inner);
        let mut rows: Vec<_> = data
            .routes
            .values()
            .flat_map(|route| {
                route
                    .workspaces
                    .iter()
                    .map(|(workspace, epoch)| RegisteredPlacement {
                        host_id: route.host_id.clone(),
                        workspace_id: workspace.clone(),
                        epoch: *epoch,
                    })
            })
            .collect();
        rows.sort_by(|a, b| (&a.workspace_id, &a.host_id).cmp(&(&b.workspace_id, &b.host_id)));
        rows
    }
    pub(crate) fn rows(&self) -> Vec<Value> {
        crate::lock(&self.inner).rows.values().cloned().collect()
    }
    pub(crate) fn clear_workspace(&self, workspace: &str) {
        let mut data = crate::lock(&self.inner);
        data.generation = data.generation.wrapping_add(1);
        let generation = data.generation;
        data.rows
            .retain(|_, row| row["workspace_id"].as_str() != Some(workspace));
        for route in data.routes.values_mut() {
            if route.workspaces.remove(workspace).is_some() {
                route.roots.remove(workspace);
                route.generation = generation;
            }
        }
        // A retired last project must not permanently occupy a bounded host
        // slot. Captured requests/tickets fail current() once the route is gone.
        data.routes.retain(|_, route| !route.workspaces.is_empty());
    }
    fn install_workspace(&self, route: &Route, workspace: &str, rows: Vec<Value>) -> bool {
        let mut data = crate::lock(&self.inner);
        if data
            .routes
            .get(&route.host_id)
            .is_none_or(|current| current.generation != route.generation)
        {
            return false;
        }
        let outside = data
            .rows
            .values()
            .filter(|row| row["workspace_id"].as_str() != Some(workspace))
            .count();
        let capacity = MAX_ROWS.saturating_sub(outside);
        let mut incoming = HashMap::new();
        for mut row in rows.into_iter().take(MAX_ROWS) {
            if incoming.len() >= capacity {
                break;
            }
            let Some(id) = row["id"].as_str() else {
                continue;
            };
            if !valid_id(id)
                || row["workspace_id"].as_str() != Some(workspace)
                || data
                    .rows
                    .get(id)
                    .is_some_and(|existing| existing["workspace_id"].as_str() != Some(workspace))
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
            incoming.insert(id, row);
        }
        let previous = data.rows.len() - outside;
        if previous == incoming.len()
            && incoming
                .iter()
                .all(|(id, row)| data.rows.get(id) == Some(row))
        {
            return false;
        }
        data.rows
            .retain(|_, row| row["workspace_id"].as_str() != Some(workspace));
        data.rows.extend(incoming);
        true
    }
    fn unavailable_workspace(&self, route: &Route, workspace: &str) -> bool {
        let mut data = crate::lock(&self.inner);
        if data
            .routes
            .get(&route.host_id)
            .is_none_or(|current| current.generation != route.generation)
        {
            return false;
        }
        let mut changed = false;
        for row in data.rows.values_mut() {
            if row["workspace_id"].as_str() == Some(workspace)
                && row["placement_available"] != false
            {
                row["placement_available"] = json!(false);
                changed = true;
            }
        }
        changed
    }
}
/// Authenticated local inventory lets a restarted native shell retire stale
/// project routes without exposing the transport credentials or real roots.
pub(crate) async fn inventory(State(state): State<Arc<AppState>>) -> Response {
    Json(state.session_proxy.inventory()).into_response()
}
pub(crate) async fn register(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Registration>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&body.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"unknown_workspace"})),
        )
            .into_response();
    };
    match state.session_proxy.register(body, workspace.root) {
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
    if let Some(workspace) = body.workspace_id {
        if body.host_id.is_some() || !valid_id(&workspace) {
            return StatusCode::BAD_REQUEST;
        }
        state.session_proxy.clear_workspace(&workspace);
        state.changes.notify_waiters();
        return StatusCode::NO_CONTENT;
    }
    let Some(host_id) = body.host_id.filter(|id| valid_id(id)) else {
        return StatusCode::BAD_REQUEST;
    };
    let mut data = crate::lock(&state.session_proxy.inner);
    data.generation = data.generation.wrapping_add(1);
    let generation = data.generation;
    if let Some(route) = data.routes.get_mut(&host_id) {
        route.address = None;
        route.token.clear();
        route.generation = generation;
    }
    for row in data.rows.values_mut() {
        if row["placement"]["remote"].as_str() == Some(&host_id) {
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
            poll_workspaces(&state.session_proxy, &state.changes).await;
        }
    });
}
async fn poll_workspaces(store: &Store, changes: &crate::state::ChangeBus) -> bool {
    // Share one bounded route snapshot per host. A slow or failed project must
    // neither suppress its siblings' results nor turn their cached rows offline.
    let jobs: Vec<_> = crate::lock(&store.inner)
        .routes
        .values()
        .filter(|route| route.address.is_some())
        .flat_map(|route| {
            let route = Arc::new(route.clone());
            route
                .workspaces
                .keys()
                .map(|workspace| (Arc::clone(&route), workspace.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    let mut polls = futures::stream::iter(jobs)
        .map(|(route, workspace)| async move {
            let result = tokio::time::timeout(Duration::from_secs(10), async {
                let response = request(
                    &route,
                    &workspace,
                    Request::builder()
                        .uri("/api/v1/sessions")
                        .body(Body::empty())?,
                )
                .await?;
                if !response.status().is_success() {
                    bail!("remote unavailable");
                }
                let bytes = axum::body::to_bytes(response.into_body(), MAX_RESPONSE).await?;
                let mut value: Value = serde_json::from_slice(&bytes)?;
                if let Some(alias) = route.alias(&workspace) {
                    alias.response("/sessions", &mut value);
                }
                Ok::<Vec<Value>, anyhow::Error>(serde_json::from_value(value)?)
            })
            .await;
            (route, workspace, result)
        })
        .buffer_unordered(POLL_CONCURRENCY);
    let mut any_changed = false;
    while let Some((route, workspace, result)) = polls.next().await {
        let changed = match result {
            Ok(Ok(rows)) => store.install_workspace(&route, &workspace, rows),
            _ => store.unavailable_workspace(&route, &workspace),
        };
        if changed {
            // Notify as each project completes; waiting for a stalled sibling
            // must not delay a healthy project's newly available session.
            changes.notify_waiters();
            any_changed = true;
        }
    }
    any_changed
}

async fn request(route: &Route, workspace: &str, request: Request<Body>) -> Result<Response> {
    verify_scope(route, workspace).await?;
    target_request(route, workspace, request).await
}
async fn verify_scope(route: &Route, workspace: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let response = target_request(
            route,
            workspace,
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())?,
        )
        .await?;
        let epoch = route
            .workspaces
            .get(workspace)
            .context("workspace route missing")?
            .to_string();
        anyhow::ensure!(
            response.status().is_success()
                && response
                    .headers()
                    .get("x-chimaera-scope-version")
                    .is_some_and(|v| v == "1")
                && response
                    .headers()
                    .get(crate::workspace_scope::WORKSPACE_HEADER)
                    .and_then(|v| v.to_str().ok())
                    == Some(workspace)
                && response
                    .headers()
                    .get(crate::workspace_scope::EPOCH_HEADER)
                    .and_then(|v| v.to_str().ok())
                    == Some(epoch.as_str()),
            "target scope acknowledgment missing"
        );
        axum::body::to_bytes(response.into_body(), 16 * 1024).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
async fn target_request(
    route: &Route,
    workspace: &str,
    mut request: Request<Body>,
) -> Result<Response> {
    let epoch = route
        .workspaces
        .get(workspace)
        .context("workspace route missing")?;
    request
        .headers_mut()
        .insert(crate::workspace_scope::WORKSPACE_HEADER, workspace.parse()?);
    request.headers_mut().insert(
        crate::workspace_scope::EPOCH_HEADER,
        epoch.to_string().parse()?,
    );
    request
        .headers_mut()
        .insert("x-chimaera-viewer-root", "L3Byb2plY3Q".parse()?);
    request.headers_mut().remove("x-chimaera-viewer-workspace");
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
    // A request already scoped by a gateway is at its target, never a second hop.
    if incoming
        .extensions()
        .get::<crate::workspace_scope::Scope>()
        .is_some()
    {
        return next.run(incoming).await;
    }
    let path = incoming
        .uri()
        .path()
        .strip_prefix("/api/v1")
        .unwrap_or(incoming.uri().path())
        .to_owned();
    let id = session_id(&path);
    let session_route = id.and_then(|id| {
        let data = crate::lock(&state.session_proxy.inner);
        let row = data.rows.get(id)?;
        let workspace = row["workspace_id"].as_str()?.to_owned();
        let route = data
            .routes
            .get(row["placement"]["remote"].as_str()?)?
            .clone();
        Some((route, workspace))
    });
    let hint = incoming
        .headers()
        .get("x-chimaera-viewer-workspace")
        .and_then(|v| v.to_str().ok())
        .filter(|id| valid_id(id))
        .map(str::to_owned);
    let hinted = hint.clone().and_then(|workspace| {
        state
            .session_proxy
            .for_workspace(&workspace)
            .map(|route| (route, workspace))
    });
    let project_resource = path.starts_with("/fs/")
        && !matches!(path.as_str(), "/fs/home" | "/fs/dirs")
        || path.starts_with("/git/")
        || path.starts_with("/workspaces/")
        || path == "/recents";
    let mut target = session_route.or_else(|| project_resource.then_some(hinted).flatten());
    if path == "/sessions" && incoming.method() == axum::http::Method::POST {
        let (parts, body) = incoming.into_parts();
        let Ok(bytes) = axum::body::to_bytes(body, MAX_METADATA).await else {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        };
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            target = value["workspace_id"].as_str().and_then(|workspace| {
                state
                    .session_proxy
                    .for_workspace(workspace)
                    .map(|route| (route, workspace.to_owned()))
            });
        }
        incoming = Request::from_parts(parts, Body::from(bytes));
    }
    let Some((route, workspace)) = target else {
        // A logical project with no reachable owner must not silently become a
        // view or edit of an old local copy. Ordinary local/SSH scopes keep their
        // existing behavior because unmanaged projects may execute locally.
        if project_resource
            && hint
                .as_deref()
                .is_some_and(|workspace| !crate::pro::may_execute(&state, workspace))
        {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"project_unavailable"})),
            )
                .into_response();
        }
        if incoming.method() != axum::http::Method::GET
            && id.is_some_and(|id| !crate::ws::session_writable(&state, id))
        {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"workspace_owned_elsewhere"})),
            )
                .into_response();
        }
        return next.run(incoming).await;
    };
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let alias = route
            .alias(&workspace)
            .context("project path mapping unavailable")?;
        let (mapped, target_keys) = alias_request(incoming, &path, &alias).await?;
        incoming = mapped;
        let path_query = incoming
            .uri()
            .path_and_query()
            .map(|v| v.as_str())
            .unwrap_or("/");
        if !path_query.starts_with("/api/v1/") {
            *incoming.uri_mut() = format!("/api/v1{path_query}").parse()?;
        }
        let response = request(&route, &workspace, incoming).await?;
        let response = alias_response(response, &path, &alias, &target_keys).await?;
        ticket_response(&state, &route, &workspace, &path, response).await
    })
    .await;
    result
        .unwrap_or_else(|_| Err(anyhow::anyhow!("remote response timed out")))
        .unwrap_or_else(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"remote_unavailable"})),
            )
                .into_response()
        })
}
impl Route {
    fn alias(&self, workspace: &str) -> Option<crate::workspace_scope::paths::Alias> {
        Some(crate::workspace_scope::paths::Alias {
            root: std::path::PathBuf::from("/project"),
            viewer: self.roots.get(workspace)?.clone(),
        })
    }
}
async fn alias_request(
    mut incoming: Request<Body>,
    path: &str,
    alias: &crate::workspace_scope::paths::Alias,
) -> Result<(Request<Body>, HashMap<String, String>)> {
    let mut target_keys = HashMap::new();
    if path.starts_with("/fs/") {
        let Query(mut query) = Query::<Vec<(String, String)>>::try_from_uri(incoming.uri())?;
        for (key, value) in &mut query {
            if matches!(key.as_str(), "path" | "dir") {
                *value = alias.input(value);
            }
        }
        let query = crate::workspace_scope::paths::encode_query(&query);
        *incoming.uri_mut() = format!(
            "{}{}{}",
            incoming.uri().path(),
            if query.is_empty() { "" } else { "?" },
            query
        )
        .parse()?;
        if matches!(
            path,
            "/fs/ticket"
                | "/fs/mkdir"
                | "/fs/create"
                | "/fs/rename"
                | "/fs/copy"
                | "/fs/move"
                | "/fs/delete"
                | "/fs/validate"
                | "/fs/resolve_targets"
                | "/fs/drafts"
        ) && matches!(incoming.method().as_str(), "POST" | "PUT")
        {
            let (mut parts, body) = incoming.into_parts();
            let bytes = axum::body::to_bytes(body, MAX_METADATA).await?;
            let mut value: Value = serde_json::from_slice(&bytes)?;
            target_keys = alias.request_body(&mut value);
            parts.headers.remove(header::CONTENT_LENGTH);
            incoming = Request::from_parts(parts, Body::from(serde_json::to_vec(&value)?));
        }
    }
    Ok((incoming, target_keys))
}
async fn alias_response(
    response: Response,
    path: &str,
    alias: &crate::workspace_scope::paths::Alias,
    target_keys: &HashMap<String, String>,
) -> Result<Response> {
    if !response.status().is_success()
        || !matches!(
            path,
            "/fs/list"
                | "/fs/dirs"
                | "/fs/mkdir"
                | "/fs/create"
                | "/fs/rename"
                | "/fs/copy"
                | "/fs/move"
                | "/fs/draft"
                | "/fs/drafts"
                | "/fs/validate"
                | "/fs/resolve_targets"
        )
    {
        return Ok(response);
    }
    let (mut parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, MAX_METADATA).await?;
    let mut value: Value = serde_json::from_slice(&bytes)?;
    crate::workspace_scope::paths::Alias::restore_keys(&mut value, target_keys);
    alias.response(path, &mut value);
    parts.headers.remove(header::CONTENT_LENGTH);
    Ok(Response::from_parts(
        parts,
        Body::from(serde_json::to_vec(&value)?),
    ))
}

async fn ticket_response(
    state: &AppState,
    route: &Route,
    workspace: &str,
    path: &str,
    response: Response,
) -> Result<Response> {
    if !response.status().is_success() || !matches!(path, "/fs/ticket" | "/fs/resolve_targets") {
        return Ok(response);
    }
    let (mut parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, MAX_METADATA).await?;
    let mut value: Value = serde_json::from_slice(&bytes)?;
    let mut data = crate::lock(&state.session_proxy.inner);
    let now = std::time::Instant::now();
    data.tickets.retain(|_, ticket| ticket.expires > now);
    let mut mint = |value: &mut Value| -> Result<()> {
        let Some(upstream) = value.get("ticket").and_then(Value::as_str) else {
            return Ok(());
        };
        anyhow::ensure!(valid_id(upstream), "invalid upstream ticket");
        if let Some((local, cached)) = data.tickets.iter_mut().find(|(_, cached)| {
            cached.workspace == workspace
                && cached.upstream == upstream
                && cached.route.host_id == route.host_id
                && cached.route.generation == route.generation
        }) {
            cached.expires = now + Duration::from_secs(600);
            value["ticket"] = json!(local);
            return Ok(());
        }
        anyhow::ensure!(data.tickets.len() < 512, "remote ticket limit");
        let local = chimaera_core::generate_token();
        data.tickets.insert(
            local.clone(),
            Ticket {
                route: route.clone(),
                workspace: workspace.to_owned(),
                upstream: upstream.to_owned(),
                expires: now + Duration::from_secs(600),
            },
        );
        value["ticket"] = json!(local);
        Ok(())
    };
    if path == "/fs/ticket" {
        mint(&mut value)?;
    } else if let Some(results) = value["results"].as_object_mut() {
        for item in results.values_mut() {
            mint(item)?;
        }
    }
    parts.headers.remove(header::CONTENT_LENGTH);
    Ok(Response::from_parts(
        parts,
        Body::from(serde_json::to_vec(&value)?),
    ))
}
pub(crate) async fn ticket_proxy(
    State(state): State<Arc<AppState>>,
    mut incoming: Request<Body>,
    next: Next,
) -> Response {
    let path = incoming.uri().path().to_owned();
    let pieces: Vec<_> = path.splitn(4, '/').collect();
    if pieces.len() < 3 || !matches!(pieces[1], "raw" | "download") {
        return next.run(incoming).await;
    }
    let ticket = crate::lock(&state.session_proxy.inner)
        .tickets
        .get(pieces[2])
        .cloned();
    let Some(ticket) = ticket else {
        return next.run(incoming).await;
    };
    if ticket.expires <= std::time::Instant::now() || !state.session_proxy.current(&ticket.route) {
        return StatusCode::GONE.into_response();
    }
    let suffix = pieces
        .get(3)
        .map(|rest| format!("/{rest}"))
        .unwrap_or_default();
    let query = incoming
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let uri = format!("/{}/{}{suffix}{query}", pieces[1], ticket.upstream);
    let Ok(uri) = uri.parse() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    *incoming.uri_mut() = uri;
    match tokio::time::timeout(
        Duration::from_secs(30),
        request(&ticket.route, &ticket.workspace, incoming),
    )
    .await
    {
        Ok(Ok(response)) => response,
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// Caller has consumed and authenticated the local first frame. Remote tokens
/// replace only that frame's token; application messages remain byte-for-byte.
pub(crate) async fn socket(
    state: &AppState,
    id: &str,
    kind: &str,
    options: &SocketOptions,
    auth: Value,
    downstream: &mut axum::extract::ws::WebSocket,
) -> bool {
    let Some(route) = state.session_proxy.for_session(id) else {
        return false;
    };
    let workspace = {
        let data = crate::lock(&state.session_proxy.inner);
        data.rows
            .get(id)
            .and_then(|row| row["workspace_id"].as_str())
            .map(str::to_owned)
    };
    let Some(workspace) = workspace else {
        return false;
    };
    socket_route(
        state, route, &workspace, id, kind, options, auth, None, downstream,
    )
    .await;
    true
}
pub(crate) async fn events(
    state: &AppState,
    workspace: &str,
    watch: Value,
    downstream: &mut axum::extract::ws::WebSocket,
) -> bool {
    let Some(route) = state.session_proxy.for_workspace(workspace) else {
        return false;
    };
    socket_route(
        state,
        route,
        workspace,
        "",
        "events",
        &SocketOptions::default(),
        json!({"type":"auth"}),
        Some(watch),
        downstream,
    )
    .await;
    true
}
#[allow(clippy::too_many_arguments)]
async fn socket_route(
    state: &AppState,
    route: Route,
    workspace: &str,
    id: &str,
    kind: &str,
    options: &SocketOptions,
    mut auth: Value,
    initial_watch: Option<Value>,
    downstream: &mut axum::extract::ws::WebSocket,
) {
    use axum::extract::ws::Message as Down;
    use tokio_tungstenite::tungstenite::Message as Up;
    let result: Result<()> = async {
        verify_scope(&route, workspace).await?;
        let _permit = REQUESTS.try_acquire().context("remote stream limit")?;
        let address = route.address.context("remote placement unavailable")?;
        let query = if options.read_only { "?read_only=true" } else if options.wake.as_deref() == Some("interaction") { "?wake=interaction" } else { "" };
        let url = if kind=="events" {format!("ws://{address}/ws/events")} else {format!("ws://{address}/ws/{kind}/{id}{query}")};
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default().max_message_size(Some(10 * 1024 * 1024)).max_frame_size(Some(10 * 1024 * 1024));
        let (mut upstream, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async_with_config(url, Some(config), false)).await??;
        let epoch=route.workspaces.get(workspace).context("workspace route missing")?;
        auth["workspace_id"]=json!(workspace); auth["epoch"]=json!(epoch); auth["viewer_root"]=json!("L3Byb2plY3Q");
        auth["token"] = json!(route.token);
        bounded_send(&mut upstream, Up::Text(auth.to_string().into())).await?;
        let alias=route.alias(workspace).context("project mapping unavailable")?;
        if let Some(mut watch)=initial_watch { map_watch(&alias,&mut watch); bounded_send(&mut upstream,Up::Text(watch.to_string().into())).await?; }

        let mut ownership = tokio::time::interval(Duration::from_secs(2));
        ownership.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ownership.tick() => if !state.session_proxy.current(&route) { bail!("placement changed"); },
                next = upstream.next() => match next {
                    Some(Ok(Up::Text(text))) => {
                        let mut value:Value=serde_json::from_str(&text)?;
                        if kind=="events" {map_event(&alias,&mut value);} else if kind=="sessions" && value["type"]=="ready" {alias.session(&mut value);}
                        bounded_send(downstream,Down::Text(value.to_string().into())).await?;
                    },
                    Some(Ok(Up::Binary(bytes))) => bounded_send(downstream, Down::Binary(bytes)).await?,
                    Some(Ok(Up::Ping(bytes))) => bounded_send(&mut upstream, Up::Pong(bytes)).await?,
                    Some(Ok(Up::Close(_))) | None => break,
                    Some(Err(error)) => return Err(error.into()),
                    _ => {},
                },
                next = downstream.recv() => match next {
                    Some(Ok(Down::Text(text))) => {
                        if !state.session_proxy.current(&route) {bail!("placement changed");}
                        if kind=="events" {
                            let mut watch:Value=serde_json::from_str(&text)?;
                            if watch["type"]!="watch" || watch["workspace_id"].as_str().is_some_and(|id|id!=workspace) {break;}
                            map_watch(&alias,&mut watch);
                            bounded_send(&mut upstream,Up::Text(watch.to_string().into())).await?;
                        } else {bounded_send(&mut upstream,Up::Text(text.to_string().into())).await?;}
                    },
                    Some(Ok(Down::Binary(bytes))) => {if !state.session_proxy.current(&route) {bail!("placement changed");} bounded_send(&mut upstream, Up::Binary(bytes)).await?;},
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
}

fn map_watch(alias: &crate::workspace_scope::paths::Alias, value: &mut Value) {
    for key in ["files", "dirs"] {
        if let Some(paths) = value[key].as_array_mut() {
            for path in paths {
                if let Some(raw) = path.as_str() {
                    *path = json!(alias.input(raw));
                }
            }
        }
    }
}
fn map_event(alias: &crate::workspace_scope::paths::Alias, value: &mut Value) {
    if value["type"] == "sessions" {
        alias.response("/sessions", &mut value["sessions"]);
    }
    if value["type"] == "fs" {
        for key in ["files", "removed", "dirs", "removed_dirs"] {
            if let Some(paths) = value[key].as_array_mut() {
                for path in paths {
                    if let Some(raw) = path.as_str() {
                        *path = json!(alias.output(raw));
                    }
                }
            }
        }
    }
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
    #[tokio::test]
    async fn shared_host_polls_keep_healthy_project_available_and_recover_independently() {
        use axum::{http::HeaderMap, routing::get, Router};
        use std::sync::atomic::AtomicUsize;
        #[derive(Default)]
        struct Target {
            fail_a: AtomicBool,
            b_reads: AtomicUsize,
            b_revision: AtomicUsize,
        }
        let target = Arc::new(Target::default());
        let remote = Router::new()
            .route(
                "/api/v1/health",
                get(|State(target): State<Arc<Target>>, headers: HeaderMap| async move {
                    assert_eq!(headers[header::AUTHORIZATION], "Bearer synthetic-poll");
                    let workspace = &headers[crate::workspace_scope::WORKSPACE_HEADER];
                    if workspace == "w-a" && target.fail_a.load(Ordering::Acquire) {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    let mut response = StatusCode::OK.into_response();
                    response.headers_mut().insert("x-chimaera-scope-version", "1".parse().unwrap());
                    response.headers_mut().insert(crate::workspace_scope::WORKSPACE_HEADER, workspace.clone());
                    response.headers_mut().insert(crate::workspace_scope::EPOCH_HEADER, headers[crate::workspace_scope::EPOCH_HEADER].clone());
                    response
                }),
            )
            .route(
                "/api/v1/sessions",
                get(|State(target): State<Arc<Target>>, headers: HeaderMap| async move {
                    assert_eq!(headers[header::AUTHORIZATION], "Bearer synthetic-poll");
                    let workspace = headers[crate::workspace_scope::WORKSPACE_HEADER].to_str().unwrap();
                    let revision = if workspace == "w-b" {
                        target.b_reads.fetch_add(1, Ordering::AcqRel);
                        target.b_revision.load(Ordering::Acquire)
                    } else {
                        0
                    };
                    Json(json!([{"id":format!("s-{workspace}"),"workspace_id":workspace,"revision":revision}]))
                }),
            )
            .with_state(Arc::clone(&target));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, remote).await.unwrap() });
        let store = Store::default();
        for workspace in ["w-a", "w-b"] {
            store
                .register(
                    Registration {
                        host_id: "worker-shared".into(),
                        endpoint: format!("http://{address}"),
                        token: "synthetic-poll".into(),
                        workspace_id: workspace.into(),
                        epoch: 7,
                    },
                    "/project".into(),
                )
                .unwrap();
        }
        let changes = crate::state::ChangeBus::new();
        let available = |workspace: &str| {
            store
                .rows()
                .into_iter()
                .find(|row| row["workspace_id"] == workspace)
                .unwrap()["placement_available"]
                .clone()
        };
        assert!(poll_workspaces(&store, &changes).await);
        assert_eq!(available("w-a"), true);
        assert_eq!(available("w-b"), true);
        let stable_generation = changes.generation();
        assert!(
            !poll_workspaces(&store, &changes).await,
            "unchanged rosters must not refresh the UI"
        );
        assert_eq!(changes.generation(), stable_generation);

        target.fail_a.store(true, Ordering::Release);
        target.b_revision.store(1, Ordering::Release);
        assert!(poll_workspaces(&store, &changes).await);
        assert_eq!(available("w-a"), false);
        assert_eq!(available("w-b"), true);
        let rows = store.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter()
                .find(|row| row["workspace_id"] == "w-b")
                .unwrap()["revision"],
            1
        );
        assert_eq!(
            target.b_reads.load(Ordering::Acquire),
            3,
            "healthy project still receives fresh HTTP polls"
        );
        assert!(
            !poll_workspaces(&store, &changes).await,
            "a stable failure must not cause recurring UI churn"
        );

        target.fail_a.store(false, Ordering::Release);
        assert!(poll_workspaces(&store, &changes).await);
        assert_eq!(available("w-a"), true);
        assert_eq!(available("w-b"), true);
        assert!(!poll_workspaces(&store, &changes).await);
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn project_poll_results_cannot_replace_siblings_or_revive_retired_routes() {
        let store = Store::default();
        for workspace in ["w-a", "w-b"] {
            store
                .register(
                    Registration {
                        host_id: "worker-shared".into(),
                        endpoint: "http://127.0.0.1:1234".into(),
                        token: "fixture".into(),
                        workspace_id: workspace.into(),
                        epoch: 4,
                    },
                    "/project".into(),
                )
                .unwrap();
        }
        let route = store.for_workspace("w-a").unwrap();
        assert!(store.install_workspace(
            &route,
            "w-b",
            vec![json!({"id":"s-b","workspace_id":"w-b"})]
        ));
        assert!(!store.install_workspace(
            &route,
            "w-a",
            vec![
                json!({"id":"s-extra","workspace_id":"w-b"}),
                json!({"id":"s-b","workspace_id":"w-a"}),
            ]
        ));
        assert_eq!(store.rows().len(), 1);
        assert_eq!(store.rows()[0]["workspace_id"], "w-b");
        store.clear_workspace("w-a");
        assert!(!store.install_workspace(
            &route,
            "w-a",
            vec![json!({"id":"s-a","workspace_id":"w-a"})]
        ));
        assert!(!store.unavailable_workspace(&route, "w-b"));
        assert_eq!(store.rows()[0]["placement_available"], true);
    }

    #[test]
    fn retiring_last_project_releases_the_bounded_host_slot() {
        let store = Store::default();
        for index in 0..=MAX_HOSTS {
            store
                .register(
                    Registration {
                        host_id: format!("worker-{index}"),
                        endpoint: "http://127.0.0.1:1234".into(),
                        token: "fixture".into(),
                        workspace_id: "w-retired".into(),
                        epoch: 1,
                    },
                    "/project".into(),
                )
                .unwrap();
            let captured = store.for_workspace("w-retired").unwrap();
            store.clear_workspace("w-retired");
            assert!(store.inventory().is_empty());
            assert!(!store.current(&captured));
        }
        let mut previous = None;
        for _ in 0..3 {
            store
                .register(
                    Registration {
                        host_id: "worker-reused".into(),
                        endpoint: "http://127.0.0.1:1234".into(),
                        token: "fixture".into(),
                        workspace_id: "w-reused".into(),
                        epoch: 1,
                    },
                    "/project".into(),
                )
                .unwrap();
            if let Some(old) = &previous {
                assert!(!store.current(old), "retired generation must not revive");
            }
            previous = store.for_workspace("w-reused");
            store.clear_workspace("w-reused");
        }
        for index in 0..=MAX_HOSTS {
            store
                .register(
                    Registration {
                        host_id: format!("device-{index}"),
                        endpoint: "http://127.0.0.1:1234".into(),
                        token: "fixture".into(),
                        workspace_id: "w-moving".into(),
                        epoch: index as u64 + 1,
                    },
                    "/project".into(),
                )
                .unwrap();
            assert_eq!(store.inventory().len(), 1);
        }
    }
    #[test]
    fn shared_route_retirement_changes_generation_once_and_steady_polls_do_not() {
        let store = Store::default();
        let registration = |workspace: &str| Registration {
            host_id: "worker-shared".into(),
            endpoint: "http://127.0.0.1:1234".into(),
            token: "fixture".into(),
            workspace_id: workspace.into(),
            epoch: 9,
        };
        store.register(registration("w-a"), "/a".into()).unwrap();
        store.register(registration("w-b"), "/b".into()).unwrap();
        let old = store.for_workspace("w-b").unwrap();
        store.clear_workspace("w-a");
        assert!(!store.current(&old));
        let current = store.for_workspace("w-b").unwrap();
        for _ in 0..3 {
            store.register(registration("w-b"), "/b".into()).unwrap();
            store.clear_workspace("w-a");
            assert!(
                store.current(&current),
                "unchanged polling must not reconnect healthy viewers"
            );
        }
    }
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
        store
            .register(registration("worker-a", 4), "/test".into())
            .unwrap();
        assert!(store
            .register(registration("worker-b", 4), "/test".into())
            .is_err());
        store
            .register(registration("worker-b", 5), "/test".into())
            .unwrap();
        assert!(store
            .register(registration("worker-a", 3), "/test".into())
            .is_err());
        assert!(session_id("/sessions/s-ok/journal").is_some());
        assert!(session_id("/sessions/s-ok/../../shutdown").is_none());
    }
}
