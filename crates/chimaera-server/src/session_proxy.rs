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
/// Short HTTP work: forwarded requests, roster polls and scope probes.
static REQUESTS: Semaphore = Semaphore::const_new(32);
/// Long-lived forwarded sockets (terminals, chats, event feeds) draw on their
/// own budget, so a window full of open tabs never starves the polls that
/// keep every row available.
static SOCKETS: Semaphore = Semaphore::const_new(128);
/// A positive scope acknowledgment is reused this long for the same project
/// stamp and epoch. The target still checks scope on every request; the probe
/// only proves it understands scope at all.
const ACK_TTL: Duration = Duration::from_secs(15);
/// A forwarded transfer with no progress this long is abandoned.
const IDLE_TRANSFER: Duration = Duration::from_secs(120);
/// Reading or rewriting a small JSON request/response around a forward.
const ADAPTER_DEADLINE: Duration = Duration::from_secs(30);
/// How long a sleeping owner may take to resume before a request fails.
const WAKE_DEADLINE: Duration = Duration::from_secs(120);
/// Set only by the account transport, never by a daemon: `sleeping` while
/// the owner is suspended and the transport answers for it.
const WORKER_STATE_HEADER: &str = "x-chimaera-worker-state";

#[derive(Default)]
pub(crate) struct Store {
    inner: Mutex<Data>,
    started: AtomicBool,
}
#[derive(Default)]
struct Data {
    /// Mints route stamps; never reused, so a retired stamp cannot revive.
    generation: u64,
    routes: HashMap<String, Route>,
    rows: HashMap<String, Value>,
    tickets: HashMap<String, Ticket>,
    /// Recent scope acknowledgments per (host, project): the stamp and epoch
    /// they proved. Bounded by routes × projects and pruned by age.
    acks: HashMap<(String, String), (Stamp, u64, std::time::Instant)>,
}
impl Data {
    fn mint(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }
    /// Whether a captured route still names this project's live registration.
    fn is_current(&self, route: &Route, workspace: &str) -> bool {
        let Some(stamp) = route.stamp(workspace) else {
            return false;
        };
        self.routes
            .get(&route.host_id)
            .is_some_and(|live| live.address.is_some() && live.stamp(workspace) == Some(stamp))
    }
}
#[derive(Clone)]
struct Ticket {
    route: Route,
    workspace: String,
    upstream: String,
    expires: std::time::Instant,
}
/// What a captured connection or ticket proves about its project's route:
/// the host transport and that one project's registration.
type Stamp = (u64, u64);
#[derive(Clone)]
struct Route {
    host_id: String,
    address: Option<std::net::SocketAddr>,
    token: String,
    workspaces: HashMap<String, u64>,
    roots: HashMap<String, std::path::PathBuf>,
    /// Per project: registering, re-registering or retiring one project on a
    /// shared transport never closes a sibling's sockets or preview tickets.
    generations: HashMap<String, u64>,
    /// Changes only with this host's endpoint or credential, which retires
    /// every project's captured connections at once.
    transport: u64,
}
impl Route {
    fn stamp(&self, workspace: &str) -> Option<Stamp> {
        Some((self.transport, *self.generations.get(workspace)?))
    }
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
        let generation = data.mint();
        let workspace = request.workspace_id;
        for existing in data.routes.values_mut() {
            if existing.host_id != request.host_id
                && existing.workspaces.remove(&workspace).is_some()
            {
                existing.roots.remove(&workspace);
                existing.generations.remove(&workspace);
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
                generations: HashMap::new(),
                transport: generation,
            });
        if route.address != Some(address) || route.token != request.token {
            route.address = Some(address);
            route.token = request.token;
            route.transport = generation;
        }
        if route.workspaces.get(&workspace) != Some(&request.epoch)
            || route.roots.get(&workspace) != Some(&local_root)
            || !route.generations.contains_key(&workspace)
        {
            route.roots.insert(workspace.clone(), local_root);
            route.workspaces.insert(workspace.clone(), request.epoch);
            route.generations.insert(workspace.clone(), generation);
        }
        // A project that moved between registered hosts must not keep its rows
        // pointing at the previous owner until the next roster poll.
        let host = route.host_id.clone();
        for row in data.rows.values_mut() {
            if row["workspace_id"].as_str() == Some(workspace.as_str())
                && row["placement"]["remote"].as_str() != Some(host.as_str())
            {
                row["placement"] = json!({"remote": host});
            }
        }
        Ok(())
    }
    fn for_session(&self, id: &str) -> Option<(Route, String)> {
        let data = crate::lock(&self.inner);
        let row = data.rows.get(id)?;
        let host = row["placement"]["remote"].as_str()?;
        let workspace = row["workspace_id"].as_str()?;
        let route = data.routes.get(host)?;
        route
            .generations
            .contains_key(workspace)
            .then(|| (route.clone(), workspace.to_owned()))
    }
    fn current(&self, route: &Route, workspace: &str) -> bool {
        crate::lock(&self.inner).is_current(route, workspace)
    }
    /// A recent probe already proved this exact project registration speaks
    /// scope: skip the extra round trip for the next request or poll.
    fn acknowledged(&self, route: &Route, workspace: &str) -> bool {
        let (Some(stamp), Some(epoch)) = (route.stamp(workspace), route.workspaces.get(workspace))
        else {
            return false;
        };
        let data = crate::lock(&self.inner);
        data.is_current(route, workspace)
            && data
                .acks
                .get(&(route.host_id.clone(), workspace.to_owned()))
                .is_some_and(|(proved, proved_epoch, at)| {
                    *proved == stamp && proved_epoch == epoch && at.elapsed() < ACK_TTL
                })
    }
    fn acknowledge(&self, route: &Route, workspace: &str) {
        let (Some(stamp), Some(epoch)) = (route.stamp(workspace), route.workspaces.get(workspace))
        else {
            return;
        };
        let mut data = crate::lock(&self.inner);
        if !data.is_current(route, workspace) {
            return;
        }
        data.acks.retain(|_, (_, _, at)| at.elapsed() < ACK_TTL);
        data.acks.insert(
            (route.host_id.clone(), workspace.to_owned()),
            (stamp, *epoch, std::time::Instant::now()),
        );
    }
    /// Whether this project currently runs on another registered owner.
    pub(crate) fn routed(&self, workspace: &str) -> bool {
        crate::lock(&self.inner)
            .routes
            .values()
            .any(|route| route.workspaces.contains_key(workspace))
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
        data.rows
            .retain(|_, row| row["workspace_id"].as_str() != Some(workspace));
        for route in data.routes.values_mut() {
            if route.workspaces.remove(workspace).is_some() {
                route.roots.remove(workspace);
                route.generations.remove(workspace);
            }
        }
        // A retired last project must not permanently occupy a bounded host
        // slot. Captured requests/tickets fail current() once the route is gone.
        data.routes.retain(|_, route| !route.workspaces.is_empty());
    }
    fn install_workspace(&self, route: &Route, workspace: &str, rows: Vec<Value>) -> bool {
        let mut data = crate::lock(&self.inner);
        if !data.is_current(route, workspace) {
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
        if !data.is_current(route, workspace) {
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
    let generation = data.mint();
    if let Some(route) = data.routes.get_mut(&host_id) {
        route.address = None;
        route.token.clear();
        route.transport = generation;
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
                    store,
                    &route,
                    &workspace,
                    Request::builder()
                        .uri("/api/v1/sessions")
                        .body(Body::empty())?,
                    Budget::ORDINARY,
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

/// How long a forwarded request may wait for its response head, and how long
/// the upstream connection may sit without progress before that head. After
/// the head, only [`IDLE_TRANSFER`] applies: a body that keeps moving (a large
/// download, a `/raw` stream) has no fixed ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Budget {
    head: Duration,
    quiet: Duration,
}
impl Budget {
    const ORDINARY: Self = Self {
        head: Duration::from_secs(30),
        quiet: Duration::from_secs(30),
    };
    /// The body streams up before the head; its progress keeps the link alive.
    const UPLOAD: Self = Self {
        head: Duration::from_secs(75 * 60),
        quiet: IDLE_TRANSFER,
    };
    /// A command may queue for ten minutes and then run for an hour, silently.
    const EXEC: Self = Self {
        head: Duration::from_secs(75 * 60),
        quiet: Duration::from_secs(75 * 60),
    };
    /// A sleeping owner must resume before it can answer.
    fn waking(self) -> Self {
        Self {
            head: self.head.max(WAKE_DEADLINE),
            quiet: self.quiet.max(WAKE_DEADLINE),
        }
    }
    fn for_request(method: &axum::http::Method, path: &str) -> Self {
        if *method != axum::http::Method::POST {
            return Self::ORDINARY;
        }
        if path == "/fs/upload" {
            return Self::UPLOAD;
        }
        match path
            .strip_prefix("/sessions/")
            .and_then(|rest| rest.split_once('/'))
        {
            Some((_, "upload")) => Self::UPLOAD,
            Some((_, "exec")) => Self::EXEC,
            _ => Self::ORDINARY,
        }
    }
}

async fn request(
    store: &Store,
    route: &Route,
    workspace: &str,
    request: Request<Body>,
    budget: Budget,
) -> Result<Response> {
    // A sleeping owner is still forwarded to: the transport answers passive
    // reads from its cache without waking anything, and wakes the owner for
    // a mutation (or an explicit interactive request) before delivering it.
    let budget = match verify_scope(store, route, workspace).await? {
        Reach::Awake => budget,
        Reach::Sleeping => budget.waking(),
    };
    target_request(route, workspace, request, budget).await
}

/// What the scope probe learned about the project's current owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    /// The owner answered and acknowledged the exact project scope.
    Awake,
    /// The transport reports the owner asleep. The exact-header probe cannot
    /// be answered by a frozen machine, so it is relaxed: whatever reaches the
    /// owner after it wakes is checked against scope by the owner itself.
    Sleeping,
}

async fn verify_scope(store: &Store, route: &Route, workspace: &str) -> Result<Reach> {
    if store.acknowledged(route, workspace) {
        return Ok(Reach::Awake);
    }
    let reach = tokio::time::timeout(Duration::from_secs(5), async {
        let response = target_request(
            route,
            workspace,
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())?,
            Budget::ORDINARY,
        )
        .await?;
        let epoch = route
            .workspaces
            .get(workspace)
            .context("workspace route missing")?
            .to_string();
        let status = response.status();
        let (sleeping, acknowledged) = {
            let headers = response.headers();
            let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
            (
                // Only the transport ever sets this header: a daemon never does.
                header(WORKER_STATE_HEADER) == Some("sleeping"),
                status.is_success()
                    && header("x-chimaera-scope-version") == Some("1")
                    && header(crate::workspace_scope::WORKSPACE_HEADER) == Some(workspace)
                    && header(crate::workspace_scope::EPOCH_HEADER) == Some(epoch.as_str()),
            )
        };
        let body = axum::body::to_bytes(response.into_body(), 16 * 1024).await?;
        let asleep = sleeping
            || status == StatusCode::SERVICE_UNAVAILABLE
                && serde_json::from_slice::<Value>(&body)
                    .is_ok_and(|value| value["error"] == "worker_asleep");
        if asleep {
            return Ok(Reach::Sleeping);
        }
        anyhow::ensure!(acknowledged, "target scope acknowledgment missing");
        Ok::<_, anyhow::Error>(Reach::Awake)
    })
    .await??;
    // Sleeping is never cached: the next request must see the owner wake.
    if reach == Reach::Awake {
        store.acknowledge(route, workspace);
    }
    Ok(reach)
}

/// Upstream socket progress: any byte in either direction pushes the idle
/// deadline out, so only a transfer that has truly stalled is abandoned.
struct Progress {
    start: std::time::Instant,
    last_ms: std::sync::atomic::AtomicU64,
    limit_ms: std::sync::atomic::AtomicU64,
}
impl Progress {
    fn new(limit: Duration) -> Arc<Self> {
        Arc::new(Self {
            start: std::time::Instant::now(),
            last_ms: std::sync::atomic::AtomicU64::new(0),
            limit_ms: std::sync::atomic::AtomicU64::new(limit.as_millis() as u64),
        })
    }
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    fn touch(&self) {
        self.last_ms.store(self.now_ms(), Ordering::Relaxed);
    }
    fn limit(&self, limit: Duration) {
        self.touch();
        self.limit_ms
            .store(limit.as_millis() as u64, Ordering::Relaxed);
    }
    /// Time left before the connection counts as stalled.
    fn remaining(&self) -> Duration {
        let quiet = self
            .now_ms()
            .saturating_sub(self.last_ms.load(Ordering::Relaxed));
        Duration::from_millis(self.limit_ms.load(Ordering::Relaxed).saturating_sub(quiet))
    }
}
struct ProgressIo {
    inner: TcpStream,
    progress: Arc<Progress>,
}
impl tokio::io::AsyncRead for ProgressIo {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let poll = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(poll, std::task::Poll::Ready(Ok(()))) && buf.filled().len() > before {
            this.progress.touch();
        }
        poll
    }
}
impl tokio::io::AsyncWrite for ProgressIo {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        data: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let poll = std::pin::Pin::new(&mut this.inner).poll_write(cx, data);
        if matches!(poll, std::task::Poll::Ready(Ok(written)) if written > 0) {
            this.progress.touch();
        }
        poll
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

async fn target_request(
    route: &Route,
    workspace: &str,
    mut request: Request<Body>,
    budget: Budget,
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
    let progress = Progress::new(budget.quiet);
    let (mut client, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(ProgressIo {
            inner: stream,
            progress: Arc::clone(&progress),
        }))
        .await?;
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
    let watchdog = Arc::clone(&progress);
    tokio::spawn(async move {
        let _permit = permit;
        tokio::pin!(connection);
        loop {
            let left = watchdog.remaining();
            if left.is_zero() {
                // Stalled: dropping the connection fails the pending head or
                // ends the body stream with an error, and frees the permit.
                break;
            }
            tokio::select! {
                _ = &mut connection => break,
                _ = tokio::time::sleep(left) => {}
            }
        }
    });
    let response = tokio::time::timeout(budget.head, client.send_request(request))
        .await
        .context("remote response timed out")??;
    progress.limit(IDLE_TRANSFER);
    Ok(response.map(Body::new))
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
    // A window's active project picks the owner only for paths that belong
    // to it; this computer's own files outside the project stay here.
    let mut fallback = None;
    if id.is_none() && path.starts_with("/fs/") {
        let (parts, body) = incoming.into_parts();
        let (body, bytes) = if fs_json_body(&parts.method, &path) {
            let Ok(bytes) = axum::body::to_bytes(body, MAX_METADATA).await else {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            };
            (Body::from(bytes.clone()), Some(bytes))
        } else {
            (body, None)
        };
        let json = bytes
            .as_ref()
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok());
        let arguments = fs_arguments(&parts.uri, json.as_ref());
        match fs_answerer(&parts.method, &path, arguments, route.alias(&workspace)).await {
            Answerer::Owner => {}
            Answerer::Local => return next.run(Request::from_parts(parts, body)).await,
            Answerer::OwnerThenLocal => fallback = Some(replay(&parts, bytes.as_ref())),
        }
        incoming = Request::from_parts(parts, body);
    }
    // Each step carries its own bound: small JSON adapters get a short one,
    // while an upload streams and an exec runs for as long as they progress
    // under their request budget (a fixed 30 s cap cut both off mid-flight).
    let budget = Budget::for_request(incoming.method(), &path);
    let result: Result<Response> = async {
        let alias = route
            .alias(&workspace)
            .context("project path mapping unavailable")?;
        let (mapped, target_keys) =
            tokio::time::timeout(ADAPTER_DEADLINE, alias_request(incoming, &path, &alias))
                .await??;
        incoming = mapped;
        let path_query = incoming
            .uri()
            .path_and_query()
            .map(|v| v.as_str())
            .unwrap_or("/");
        if !path_query.starts_with("/api/v1/") {
            *incoming.uri_mut() = format!("/api/v1{path_query}").parse()?;
        }
        let response = request(&state.session_proxy, &route, &workspace, incoming, budget).await?;
        tokio::time::timeout(ADAPTER_DEADLINE, async {
            let response = alias_response(response, &path, &alias, &target_keys).await?;
            ticket_response(&state, &route, &workspace, &path, response).await
        })
        .await?
    }
    .await;
    // A read of a path outside the project that its owner does not have (or
    // may not show) is this computer's own file: answer it here.
    if let Some(local) = fallback {
        let owner_declined = result.as_ref().map_or(true, |response| {
            matches!(
                response.status(),
                StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            )
        });
        if owner_declined {
            return next.run(local).await;
        }
    }
    result.unwrap_or_else(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"remote_unavailable"})),
        )
            .into_response()
    })
}

/// Who answers a project-window `/fs/*` request.
enum Answerer {
    /// The project's own paths (and anything with no path at all).
    Owner,
    /// A write outside the project whose folder exists on this computer.
    Local,
    /// A read outside the project: the owner first (a conversation that runs
    /// there links its own files and saved images), else this computer.
    OwnerThenLocal,
}
/// Filesystem routes whose small JSON body names their paths.
fn fs_json_body(method: &axum::http::Method, path: &str) -> bool {
    (*method == axum::http::Method::POST
        && matches!(
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
        ))
        || (*method == axum::http::Method::PUT && path == "/fs/drafts")
}
/// Every filesystem path a request names, for choosing who answers only: the
/// answering daemon validates each one again.
fn fs_arguments(uri: &axum::http::Uri, body: Option<&Value>) -> Vec<String> {
    let mut arguments = Vec::new();
    if let Ok(Query(query)) = Query::<Vec<(String, String)>>::try_from_uri(uri) {
        arguments.extend(
            query
                .into_iter()
                .filter(|(key, _)| key == "path" || key == "dir")
                .map(|(_, value)| value),
        );
    }
    if let Some(body) = body {
        for key in ["path", "from", "to", "base"] {
            if let Some(value) = body[key].as_str() {
                arguments.push(value.to_owned());
            }
        }
        if let Some(bases) = body["bases"].as_array() {
            arguments.extend(bases.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    arguments
}
async fn fs_answerer(
    method: &axum::http::Method,
    path: &str,
    arguments: Vec<String>,
    alias: Option<crate::workspace_scope::paths::Alias>,
) -> Answerer {
    let Some(alias) = alias else {
        return Answerer::Owner;
    };
    // Compound resolvers: the owner answers what it may read and leaves the
    // rest unresolved.
    if matches!(path, "/fs/validate" | "/fs/resolve_targets")
        || arguments.is_empty()
        || arguments
            .iter()
            .any(|argument| alias.input(argument) != *argument)
    {
        return Answerer::Owner;
    }
    let reading = matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD)
        || (*method == axum::http::Method::POST && path == "/fs/ticket");
    if reading {
        return Answerer::OwnerThenLocal;
    }
    // A write goes where its folder exists: this computer's own files stay
    // here, while a path from the owner's machine (a link in a conversation
    // that runs there) goes to the owner.
    let here = tokio::task::spawn_blocking(move || {
        arguments.iter().all(|argument| {
            crate::fs::expand_tilde(argument).is_ok_and(|path| {
                path.is_absolute() && path.parent().is_some_and(std::path::Path::is_dir)
            })
        })
    })
    .await
    .unwrap_or(false);
    if here {
        Answerer::Local
    } else {
        Answerer::Owner
    }
}
/// The same small request again, for this computer to answer.
fn replay(parts: &axum::http::request::Parts, body: Option<&bytes::Bytes>) -> Request<Body> {
    let mut request = Request::new(body.cloned().map_or_else(Body::empty, Body::from));
    *request.method_mut() = parts.method.clone();
    *request.uri_mut() = parts.uri.clone();
    *request.version_mut() = parts.version;
    *request.headers_mut() = parts.headers.clone();
    request
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
                && cached.route.stamp(workspace) == route.stamp(workspace)
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
    if ticket.expires <= std::time::Instant::now()
        || !state
            .session_proxy
            .current(&ticket.route, &ticket.workspace)
    {
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
    // The head is bounded; the body (a video, a large download) then streams
    // for as long as it keeps moving.
    match request(
        &state.session_proxy,
        &ticket.route,
        &ticket.workspace,
        incoming,
        Budget::ORDINARY,
    )
    .await
    {
        Ok(response) => response,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

type Upstream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;
use axum::extract::ws::Message as Down;
use tokio_tungstenite::tungstenite::Message as Up;

/// Typing a viewer did before the owner's terminal answered.
const HELD_TERMINAL_BYTES: usize = 64 * 1024;
/// Chat commands a viewer sent before the owner's chat answered. One maximum
/// structured command is ~10 MiB (images), so the byte bound admits one.
const HELD_CHAT_COMMANDS: usize = 4;
const HELD_CHAT_BYTES: usize = 11 * 1024 * 1024;
/// Owner-socket retry pacing while nobody is typing.
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// Frames larger than this are replay batches, never a `ready` frame.
const READY_SCAN_BYTES: usize = 64 * 1024;

/// Caller has consumed and authenticated the local first frame. Remote tokens
/// replace only that frame's token; application messages remain byte-for-byte.
///
/// The viewer's socket stays open while the owner is unreachable or asleep:
/// attaching is passive, and the first real input (a keystroke, a chat
/// command, a permission answer) is held and carries wake intent. Held input
/// is delivered exactly once after the owner's `ready`, or answered with a
/// visible refusal when it cannot be delivered; nothing is resent.
pub(crate) async fn socket(
    state: &AppState,
    id: &str,
    kind: &str,
    options: &SocketOptions,
    auth: Value,
    downstream: &mut axum::extract::ws::WebSocket,
) -> bool {
    let Some((route, workspace)) = state.session_proxy.for_session(id) else {
        return false;
    };
    let Ok(_permit) = SOCKETS.try_acquire() else {
        let _ = bounded_send(downstream, Down::Text(unavailable().to_string().into())).await;
        return true;
    };
    let link = Link {
        state,
        alias: route.alias(&workspace),
        route,
        workspace,
        session: id.to_owned(),
        path: format!("/ws/{kind}/{id}"),
        auth,
        read_only: options.read_only,
        chat: kind == "chat",
    };
    let wake = !options.read_only && options.wake.as_deref() == Some("interaction");
    relay(&link, wake, downstream).await;
    true
}

/// One viewer socket's fixed route to one session on its current owner.
struct Link<'a> {
    state: &'a AppState,
    route: Route,
    workspace: String,
    session: String,
    path: String,
    auth: Value,
    read_only: bool,
    chat: bool,
    alias: Option<crate::workspace_scope::paths::Alias>,
}
enum Opened {
    Live(Box<Upstream>),
    /// The owner is asleep and this attempt carried no interaction.
    Sleeping,
}
impl Link<'_> {
    fn current(&self) -> bool {
        self.state
            .session_proxy
            .current(&self.route, &self.workspace)
    }
    async fn open(&self, wake: bool) -> Result<Opened> {
        let reach = verify_scope(&self.state.session_proxy, &self.route, &self.workspace).await?;
        // Viewing never wakes a sleeping owner; only interaction may.
        if reach == Reach::Sleeping && !wake {
            return Ok(Opened::Sleeping);
        }
        let address = self.route.address.context("remote placement unavailable")?;
        let query = if self.read_only {
            "?read_only=true"
        } else if wake {
            "?wake=interaction"
        } else {
            ""
        };
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(10 * 1024 * 1024))
            .max_frame_size(Some(10 * 1024 * 1024));
        let deadline = match reach {
            Reach::Awake => Duration::from_secs(15),
            Reach::Sleeping => WAKE_DEADLINE,
        };
        let (mut upstream, _) = tokio::time::timeout(
            deadline,
            tokio_tungstenite::connect_async_with_config(
                format!("ws://{address}{}{query}", self.path),
                Some(config),
                false,
            ),
        )
        .await??;
        let epoch = self
            .route
            .workspaces
            .get(&self.workspace)
            .context("workspace route missing")?;
        let mut auth = self.auth.clone();
        auth["workspace_id"] = json!(self.workspace);
        auth["epoch"] = json!(epoch);
        auth["viewer_root"] = json!("L3Byb2plY3Q");
        auth["token"] = json!(self.route.token);
        bounded_send(&mut upstream, Up::Text(auth.to_string().into())).await?;
        Ok(Opened::Live(Box::new(upstream)))
    }
    /// The owner's frame for the viewer, byte-for-byte except a terminal
    /// `ready`'s paths, and whether it is the `ready` that opens delivery.
    fn downstream_text(&self, text: &str) -> (String, bool) {
        if text.len() <= READY_SCAN_BYTES {
            if let Ok(mut value) = serde_json::from_str::<Value>(text) {
                if value["type"] == "ready" {
                    if let (false, Some(alias)) = (self.chat, &self.alias) {
                        alias.session(&mut value);
                        return (value.to_string(), true);
                    }
                    return (text.to_owned(), true);
                }
            }
        }
        (text.to_owned(), false)
    }
    /// Where the session continues after its project changed owner.
    fn moved(&self) -> Value {
        let to = match self.state.session_proxy.for_session(&self.session) {
            Some((route, _)) if route.host_id.starts_with("worker-") => "cloud",
            Some(_) => "computer",
            None if crate::pro::is_worker(self.state) => "cloud",
            None => "computer",
        };
        json!({"type":"moved","to":to})
    }
    /// Answer input that never reached the owner, so nothing vanishes silently.
    async fn refuse(
        &self,
        downstream: &mut axum::extract::ws::WebSocket,
        count: usize,
    ) -> Result<()> {
        if self.chat {
            for _ in 0..count {
                bounded_send(downstream, Down::Text(not_sent().to_string().into())).await?;
            }
        } else if count > 0 {
            let frame = json!({"type":"error","code":"read_only","reason":"reconnecting",
                "message":"Your project is reconnecting. That input was not sent."});
            bounded_send(downstream, Down::Text(frame.to_string().into())).await?;
        }
        Ok(())
    }
}

/// Input held until the owner's socket is ready. Bounded per socket.
struct Held {
    chat: bool,
    frames: std::collections::VecDeque<Down>,
    bytes: usize,
}
impl Held {
    fn push(&mut self, frame: Down) -> bool {
        let size = match &frame {
            Down::Text(text) => text.len(),
            Down::Binary(bytes) => bytes.len(),
            _ => 0,
        };
        let fits = if self.chat {
            self.frames.len() < HELD_CHAT_COMMANDS && self.bytes + size <= HELD_CHAT_BYTES
        } else {
            self.bytes + size <= HELD_TERMINAL_BYTES
        };
        if fits {
            self.bytes += size;
            self.frames.push_back(frame);
        }
        fits
    }
    fn take(&mut self) -> std::collections::VecDeque<Down> {
        self.bytes = 0;
        std::mem::take(&mut self.frames)
    }
}
fn unavailable() -> Value {
    json!({"type":"error","code":"remote_unavailable","message":"Your project is reconnecting."})
}
fn asleep() -> Value {
    json!({"type":"error","code":"worker_asleep","message":"Your project is paused. Sending a message picks it back up."})
}
/// The existing per-command refusal: the socket stays up, the client keeps
/// the unsent text.
fn not_sent() -> Value {
    json!({"type":"error","code":"command_failed","message":"Not sent. Your project is reconnecting."})
}
fn upward(frame: Down) -> Option<Up> {
    match frame {
        Down::Text(text) => Some(Up::Text(text.as_str().into())),
        Down::Binary(bytes) => Some(Up::Binary(bytes)),
        _ => None,
    }
}
async fn next_up(
    upstream: &mut Option<Box<Upstream>>,
) -> Option<Result<Up, tokio_tungstenite::tungstenite::Error>> {
    match upstream {
        Some(socket) => socket.next().await,
        None => std::future::pending().await,
    }
}

async fn relay(link: &Link<'_>, mut wake: bool, downstream: &mut axum::extract::ws::WebSocket) {
    let mut held = Held {
        chat: link.chat,
        frames: Default::default(),
        bytes: 0,
    };
    let mut upstream: Option<Box<Upstream>> = None;
    let mut ready = false;
    // The last connection status told to the viewer, sent once per change.
    let mut told: Option<&'static str> = None;
    let mut attempt: Option<futures::future::BoxFuture<'_, Result<Opened>>> =
        Some(Box::pin(link.open(wake)));
    let mut backoff = RETRY_MIN;
    let retry = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(retry);
    let mut retry_armed = false;
    let mut ownership = tokio::time::interval(Duration::from_secs(2));
    ownership.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            Some(result) = futures::future::OptionFuture::from(attempt.as_mut()), if attempt.is_some() => {
                attempt = None;
                match result {
                    Ok(Opened::Live(socket)) => {
                        upstream = Some(socket);
                        ready = false;
                        backoff = RETRY_MIN;
                    }
                    Ok(Opened::Sleeping) if !held.frames.is_empty() => {
                        wake = true;
                        attempt = Some(Box::pin(link.open(true)));
                    }
                    Ok(Opened::Sleeping) => {
                        if told != Some("worker_asleep") {
                            told = Some("worker_asleep");
                            if bounded_send(downstream, Down::Text(asleep().to_string().into())).await.is_err() { return; }
                        }
                        retry.as_mut().reset(tokio::time::Instant::now() + RETRY_MAX);
                        retry_armed = true;
                    }
                    Err(_) => {
                        // A failed wake attempt answers what it was carrying.
                        let count = held.take().len();
                        if link.refuse(downstream, count).await.is_err() { return; }
                        wake = false;
                        if told != Some("remote_unavailable") {
                            told = Some("remote_unavailable");
                            if bounded_send(downstream, Down::Text(unavailable().to_string().into())).await.is_err() { return; }
                        }
                        retry.as_mut().reset(tokio::time::Instant::now() + backoff);
                        retry_armed = true;
                        backoff = (backoff * 2).min(RETRY_MAX);
                    }
                }
            }
            _ = &mut retry, if retry_armed => {
                retry_armed = false;
                attempt = Some(Box::pin(link.open(wake)));
            }
            _ = ownership.tick() => {
                if !link.current() {
                    let count = held.take().len();
                    let _ = link.refuse(downstream, count).await;
                    let _ = bounded_send(downstream, Down::Text(link.moved().to_string().into())).await;
                    return;
                }
            }
            next = next_up(&mut upstream) => match next {
                Some(Ok(Up::Text(text))) => {
                    let (frame, is_ready) = link.downstream_text(&text);
                    if bounded_send(downstream, Down::Text(frame.into())).await.is_err() { return; }
                    if is_ready && !ready {
                        ready = true;
                        told = None;
                        // Deliver what the viewer typed while connecting, once, in order.
                        if let Some(socket) = upstream.as_mut() {
                            for frame in held.take() {
                                let Some(frame) = upward(frame) else { continue };
                                if bounded_send(socket.as_mut(), frame).await.is_err() { return; }
                            }
                        }
                    }
                }
                Some(Ok(Up::Binary(bytes))) => {
                    if bounded_send(downstream, Down::Binary(bytes)).await.is_err() { return; }
                }
                Some(Ok(Up::Ping(bytes))) => {
                    if let Some(socket) = upstream.as_mut() {
                        let _ = bounded_send(socket.as_mut(), Up::Pong(bytes)).await;
                    }
                }
                Some(Ok(Up::Close(_))) | None | Some(Err(_)) => {
                    // The owner ended this connection (exit, restart, owner
                    // change). The viewer reconnects and is routed afresh.
                    let count = held.take().len();
                    let _ = link.refuse(downstream, count).await;
                    return;
                }
                _ => {}
            },
            next = downstream.recv() => match next {
                Some(Ok(frame @ (Down::Text(_) | Down::Binary(_)))) => {
                    // A terminal's text frames are grid control (resize, park);
                    // everything a chat sends is the user acting.
                    let input = link.chat || matches!(frame, Down::Binary(_));
                    match upstream.as_mut() {
                        Some(socket) if ready => {
                            if !link.current() {
                                let _ = link.refuse(downstream, usize::from(input)).await;
                                let _ = bounded_send(downstream, Down::Text(link.moved().to_string().into())).await;
                                return;
                            }
                            let Some(frame) = upward(frame) else { continue };
                            if bounded_send(socket.as_mut(), frame).await.is_err() { return; }
                        }
                        _ if input => {
                            if !held.push(frame) && link.refuse(downstream, 1).await.is_err() {
                                return;
                            }
                            // The first real input carries wake intent; an
                            // in-flight passive attempt upgrades when it
                            // reports the owner asleep.
                            if upstream.is_none() && attempt.is_none() {
                                wake = true;
                                retry_armed = false;
                                attempt = Some(Box::pin(link.open(true)));
                            }
                        }
                        // Grid control before the owner answers: the auth frame
                        // already carries the grid; the ready reconcile fixes drift.
                        _ => {}
                    }
                }
                Some(Ok(Down::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
        }
    }
}
/// One project's live frames from its current owner, merged into a window's
/// own `/ws/events` loop. The window's daemon stays authoritative for
/// everything else: the owner's settings, recents, notices, updates and
/// plugin frames describe the owner's machine and are never forwarded, and
/// the feed ending (an owner change, a sleeping owner) never closes the
/// window's socket — its loop simply starts another feed.
pub(crate) enum FeedFrame {
    /// A path-only invalidation, already in this window's paths.
    Fs(Value),
    /// The project's Git epoch on its owner.
    Git(u64),
    /// The project's Timeline epoch on its owner.
    Timeline(u64),
}
pub(crate) struct Feed {
    pub(crate) workspace: String,
    paths: tokio::sync::watch::Sender<(Vec<String>, Vec<String>)>,
    frames: tokio::sync::mpsc::Receiver<FeedFrame>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Feed {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Feed {
    /// Viewing is passive: a feed never wakes a sleeping owner.
    pub(crate) fn start(
        state: Arc<AppState>,
        workspace: String,
        files: Vec<String>,
        dirs: Vec<String>,
    ) -> Self {
        let (paths, watched) = tokio::sync::watch::channel((files, dirs));
        let (sender, frames) = tokio::sync::mpsc::channel(16);
        let task = tokio::spawn({
            let workspace = workspace.clone();
            async move {
                // Returning drops `sender`: the loop sees the feed end.
                let _ = feed(&state, &workspace, watched, sender).await;
            }
        });
        Self {
            workspace,
            paths,
            frames,
            task,
        }
    }
    /// The window's mounted previews and listed folders, in its own paths.
    pub(crate) fn watch(&self, files: Vec<String>, dirs: Vec<String>) {
        let _ = self.paths.send((files, dirs));
    }
    /// `None` once the feed ended.
    pub(crate) async fn next(&mut self) -> Option<FeedFrame> {
        self.frames.recv().await
    }
}
async fn feed(
    state: &AppState,
    workspace: &str,
    mut paths: tokio::sync::watch::Receiver<(Vec<String>, Vec<String>)>,
    frames: tokio::sync::mpsc::Sender<FeedFrame>,
) -> Result<()> {
    let route = state
        .session_proxy
        .for_workspace(workspace)
        .context("project route retired")?;
    let _permit = SOCKETS.try_acquire().context("remote stream limit")?;
    if verify_scope(&state.session_proxy, &route, workspace).await? == Reach::Sleeping {
        bail!("owner asleep");
    }
    let address = route.address.context("remote placement unavailable")?;
    let alias = route
        .alias(workspace)
        .context("project mapping unavailable")?;
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(10 * 1024 * 1024))
        .max_frame_size(Some(10 * 1024 * 1024));
    let (mut upstream, _) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async_with_config(
            format!("ws://{address}/ws/events"),
            Some(config),
            false,
        ),
    )
    .await??;
    let epoch = route
        .workspaces
        .get(workspace)
        .context("workspace route missing")?;
    let auth = json!({"type":"auth","token":route.token,"workspace_id":workspace,
        "epoch":epoch,"viewer_root":"L3Byb2plY3Q"});
    bounded_send(&mut upstream, Up::Text(auth.to_string().into())).await?;
    let registration = |(files, dirs): &(Vec<String>, Vec<String>)| {
        let map = |paths: &[String]| paths.iter().map(|p| alias.input(p)).collect::<Vec<_>>();
        json!({"type":"watch","workspace_id":workspace,"files":map(files),"dirs":map(dirs)})
    };
    let initial = registration(&paths.borrow_and_update());
    bounded_send(&mut upstream, Up::Text(initial.to_string().into())).await?;
    let mut ownership = tokio::time::interval(Duration::from_secs(2));
    ownership.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = ownership.tick() => {
                if !state.session_proxy.current(&route, workspace) {
                    return Ok(());
                }
            }
            changed = paths.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                let watch = registration(&paths.borrow_and_update());
                bounded_send(&mut upstream, Up::Text(watch.to_string().into())).await?;
            }
            next = upstream.next() => match next {
                Some(Ok(Up::Text(text))) => {
                    let Ok(mut value) = serde_json::from_str::<Value>(&text) else { continue };
                    match value["type"].as_str() {
                        Some("sessions") => {
                            // The same rows the roster poll installs, just
                            // sooner; the merged local snapshot stays the one
                            // authority, so a tab whose session lives only
                            // on this computer is never pruned.
                            alias.response("/sessions", &mut value["sessions"]);
                            let rows = serde_json::from_value(value["sessions"].take()).unwrap_or_default();
                            if state.session_proxy.install_workspace(&route, workspace, rows) {
                                state.changes.notify_waiters();
                            }
                        }
                        Some("fs") => {
                            map_fs(&alias, &mut value);
                            frames.send(FeedFrame::Fs(value)).await?;
                        }
                        Some("git") => {
                            if let Some(epoch) = value["epochs"][workspace].as_u64() {
                                frames.send(FeedFrame::Git(epoch)).await?;
                            }
                        }
                        Some("timeline") => {
                            if let Some(epoch) = value["epochs"][workspace].as_u64() {
                                frames.send(FeedFrame::Timeline(epoch)).await?;
                            }
                        }
                        // The owner's connection for this project changed:
                        // end, and let the window's loop start afresh.
                        Some("error") => return Ok(()),
                        // Settings, recents, notices, updates and plugin
                        // frames describe the owner's machine, not this window.
                        _ => {}
                    }
                }
                Some(Ok(Up::Ping(bytes))) => bounded_send(&mut upstream, Up::Pong(bytes)).await?,
                Some(Ok(Up::Close(_))) | None => return Ok(()),
                Some(Err(error)) => return Err(error.into()),
                _ => {}
            },
        }
    }
}

fn map_fs(alias: &crate::workspace_scope::paths::Alias, value: &mut Value) {
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
            probes: AtomicUsize,
        }
        let target = Arc::new(Target::default());
        let remote = Router::new()
            .route(
                "/api/v1/health",
                get(|State(target): State<Arc<Target>>, headers: HeaderMap| async move {
                    assert_eq!(headers[header::AUTHORIZATION], "Bearer synthetic-poll");
                    target.probes.fetch_add(1, Ordering::AcqRel);
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
                    // An unreachable owner fails its roster too, not only the probe.
                    if workspace == "w-a" && target.fail_a.load(Ordering::Acquire) {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    let revision = if workspace == "w-b" {
                        target.b_reads.fetch_add(1, Ordering::AcqRel);
                        target.b_revision.load(Ordering::Acquire)
                    } else {
                        0
                    };
                    Json(json!([{"id":format!("s-{workspace}"),"workspace_id":workspace,"revision":revision}])).into_response()
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
        assert_eq!(
            target.probes.load(Ordering::Acquire),
            2,
            "a recent scope acknowledgment is reused instead of re-probing each poll"
        );

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
    fn uploads_and_exec_are_not_cut_off_by_the_ordinary_head_deadline() {
        use axum::http::Method;
        assert_eq!(
            Budget::for_request(&Method::POST, "/fs/upload"),
            Budget::UPLOAD
        );
        assert_eq!(
            Budget::for_request(&Method::POST, "/sessions/s-1/upload"),
            Budget::UPLOAD
        );
        assert_eq!(
            Budget::for_request(&Method::POST, "/sessions/s-1/exec"),
            Budget::EXEC
        );
        for (method, path) in [
            (Method::GET, "/sessions/s-1/exec"),
            (Method::GET, "/fs/file"),
            (Method::POST, "/fs/ticket"),
            (Method::POST, "/sessions"),
        ] {
            assert_eq!(Budget::for_request(&method, path), Budget::ORDINARY);
        }
        // An upload's body keeps the link alive; only a silent stall ends it.
        assert!(Budget::UPLOAD.quiet < Budget::UPLOAD.head);
        assert!(Budget::EXEC.quiet >= Budget::EXEC.head);
    }

    #[test]
    fn transfer_progress_extends_the_idle_deadline() {
        let progress = Progress::new(Duration::from_millis(80));
        std::thread::sleep(Duration::from_millis(50));
        assert!(progress.remaining() <= Duration::from_millis(30));
        progress.touch();
        assert!(progress.remaining() > Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(90));
        assert!(progress.remaining().is_zero(), "a silent link stalls out");
        progress.limit(Duration::from_secs(60));
        assert!(progress.remaining() > Duration::from_secs(59));
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
        // A slow result for B from before B's own owner changed cannot mark
        // the new owner's rows unavailable.
        store
            .register(
                Registration {
                    host_id: "worker-shared".into(),
                    endpoint: "http://127.0.0.1:1234".into(),
                    token: "fixture".into(),
                    workspace_id: "w-b".into(),
                    epoch: 5,
                },
                "/project".into(),
            )
            .unwrap();
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
            assert!(!store.current(&captured, "w-retired"));
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
                assert!(
                    !store.current(old, "w-reused"),
                    "retired generation must not revive"
                );
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
    fn sibling_projects_on_a_shared_host_keep_their_own_generations() {
        let store = Store::default();
        let registration = |workspace: &str, epoch: u64, token: &str| Registration {
            host_id: "worker-shared".into(),
            endpoint: "http://127.0.0.1:1234".into(),
            token: token.into(),
            workspace_id: workspace.into(),
            epoch,
        };
        store
            .register(registration("w-a", 9, "fixture"), "/a".into())
            .unwrap();
        store
            .register(registration("w-b", 9, "fixture"), "/b".into())
            .unwrap();
        let b = store.for_workspace("w-b").unwrap();
        // Registering, re-registering and retiring a sibling never closes B.
        store
            .register(registration("w-c", 9, "fixture"), "/c".into())
            .unwrap();
        store.clear_workspace("w-a");
        store.clear_workspace("w-c");
        for _ in 0..3 {
            store
                .register(registration("w-b", 9, "fixture"), "/b".into())
                .unwrap();
            assert!(
                store.current(&b, "w-b"),
                "unchanged polling must not reconnect healthy viewers"
            );
        }
        // B's own owner change (a new epoch) retires only B's captured state.
        store
            .register(registration("w-a", 9, "fixture"), "/a".into())
            .unwrap();
        let a = store.for_workspace("w-a").unwrap();
        store
            .register(registration("w-b", 10, "fixture"), "/b".into())
            .unwrap();
        assert!(!store.current(&b, "w-b"));
        assert!(store.current(&a, "w-a"));
        // A new endpoint credential retires every project on that host.
        let a = store.for_workspace("w-a").unwrap();
        let b = store.for_workspace("w-b").unwrap();
        store
            .register(registration("w-b", 10, "rotated"), "/b".into())
            .unwrap();
        assert!(!store.current(&a, "w-a"));
        assert!(!store.current(&b, "w-b"));
        // A captured route never vouches for a project it did not carry.
        assert!(!store.current(&store.for_workspace("w-b").unwrap(), "w-z"));
    }

    #[test]
    fn a_project_moving_between_hosts_rehomes_its_rows_at_once() {
        let store = Store::default();
        let registration = |host: &str, epoch: u64| Registration {
            host_id: host.into(),
            endpoint: "http://127.0.0.1:1234".into(),
            token: "fixture".into(),
            workspace_id: "w-moving".into(),
            epoch,
        };
        store
            .register(registration("device-home", 3), "/p".into())
            .unwrap();
        let home = store.for_workspace("w-moving").unwrap();
        assert!(store.install_workspace(
            &home,
            "w-moving",
            vec![json!({"id":"s-moving","workspace_id":"w-moving"})]
        ));
        store
            .register(registration("worker-cloud", 4), "/p".into())
            .unwrap();
        assert_eq!(store.rows()[0]["placement"]["remote"], "worker-cloud");
        let (route, workspace) = store.for_session("s-moving").unwrap();
        assert_eq!(
            (route.host_id.as_str(), workspace.as_str()),
            ("worker-cloud", "w-moving")
        );
        assert!(!store.current(&home, "w-moving"));
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
