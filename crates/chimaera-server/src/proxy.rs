//! The browser-pane reverse proxy: ticketed, target-pinned HTTP/WS forwarding
//! to live web apps (Jupyter, marimo, Streamlit, RStudio) running on the
//! daemon's host — or, via a second hop, on a Slurm compute node.
//!
//! Why it exists: the daemon already runs on the host that owns the work, so a
//! proxy route on the daemon makes `localhost:8888` on a login node reachable
//! through the existing tunnel with zero extra plumbing — remote-transparent
//! by construction. The web UI's browser pane iframes `/proxy/{id}/…`.
//!
//! Security model (never an open relay):
//! - Minting a proxy session requires the bearer token; the data plane is
//!   authorized by the unguessable 128-bit id alone (iframes cannot send
//!   Authorization headers — the `/raw/{ticket}` story).
//! - An id is pinned to exactly ONE host:port for its whole life. Nothing a
//!   request carries can change where it is forwarded.
//! - Targets are allowlisted at mint time: loopback and the daemon's own
//!   host always qualify; a node the user's own Slurm jobs run on qualifies
//!   (the compute snapshot is the witness); anything else needs the caller
//!   to send `confirm: true` — the UI only does that after an explicit
//!   user dialog.
//!
//! Resource discipline (login-node rules): bodies stream in both directions
//! (never buffered), upgraded WebSocket tunnels are plain bounded-buffer byte
//! copies with a global cap, the registry is capped and idle-expired, and a
//! relay child (`ssh -N -L`, the compute-node hop) is owned by its entry and
//! killed with it.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpStream;

use crate::{assets, AppState};

/// Hard cap on live proxy sessions (each is one target the user opened).
const MAX_PROXIES: usize = 32;
/// A session unused this long is swept (mounted panes ping /health, so this
/// only reaps targets no window shows anymore).
const IDLE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Global cap on concurrently upgraded (WebSocket) tunnels.
const MAX_TUNNELS: usize = 256;
/// Dialing the target (per request, loopback or cached route).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The first dial of a non-loopback host probes direct TCP this long before
/// falling back to an ssh relay.
const DIRECT_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// How long an ssh relay child gets to bring its forward up.
const RELAY_START_TIMEOUT: Duration = Duration::from_secs(12);
/// Waiting for upstream response HEADERS (bodies stream unbounded after).
const RESPONSE_HEAD_TIMEOUT: Duration = Duration::from_secs(60);
/// The cookie that lets absolute-path apps (Jupyter) escape the /proxy/{id}
/// prefix and still be routed: the SPA fallback rescues unknown paths that
/// carry it. HttpOnly — the workbench JS never needs it.
const RESCUE_COOKIE: &str = "chimaera_proxy";
/// Recursion guard: a request that already went through this proxy once is
/// never proxied again (a target that loops back to the daemon).
const LOOP_GUARD: &str = "x-chimaera-proxied";

// --- the registry -------------------------------------------------------------

/// How a target is reached from the daemon host.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Route {
    /// Plain TCP to host:port.
    Direct,
    /// Through a daemon-owned `ssh -N -L` child; connect loopback:this port.
    Relay(u16),
}

struct ProxyEntry {
    host: String,
    port: u16,
    last_used: Instant,
    /// The proven route, cached after the first successful dial.
    route: Option<Route>,
    /// Single-flight guard for (re)probing the route.
    probe: Arc<tokio::sync::Mutex<()>>,
    /// The ssh relay child, when `route` is `Relay`.
    relay: Option<tokio::process::Child>,
}

/// In-memory registry of proxy sessions. Bearer-authed routes mint and revoke
/// entries; the unauthenticated data plane only ever looks ids up.
#[derive(Default)]
pub(crate) struct ProxyStore {
    entries: HashMap<String, ProxyEntry>,
    tunnels: Arc<AtomicUsize>,
}

impl ProxyStore {
    /// Purge idle entries, returning their relay children for the caller to
    /// kill OUTSIDE the store lock.
    fn purge(&mut self) -> Vec<tokio::process::Child> {
        let now = Instant::now();
        let dead: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| now.duration_since(e.last_used) > IDLE_TTL)
            .map(|(id, _)| id.clone())
            .collect();
        let mut children = Vec::new();
        for id in dead {
            if let Some(mut e) = self.entries.remove(&id) {
                if let Some(child) = e.relay.take() {
                    children.push(child);
                }
            }
        }
        children
    }
}

/// SIGKILL + reap a relay child off the caller's path.
fn kill_relay(mut child: tokio::process::Child) {
    let _ = child.start_kill();
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
}

/// Kill every relay child (graceful daemon shutdown — an orphaned `ssh -N`
/// would linger on the login node).
pub(crate) fn shutdown_relays(state: &AppState) {
    let children: Vec<_> = {
        let mut store = crate::lock(&state.proxies);
        store
            .entries
            .values_mut()
            .filter_map(|e| e.relay.take())
            .collect()
    };
    for child in children {
        kill_relay(child);
    }
}

/// Periodic idle sweep (also kills the swept entries' relay children).
pub(crate) async fn sweeper(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(Duration::from_secs(600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let children = crate::lock(&state.proxies).purge();
        for child in children {
            kill_relay(child);
        }
    }
}

// --- mint-time policy ----------------------------------------------------------

/// Where a requested target host sits relative to this daemon.
#[derive(Debug, PartialEq)]
enum HostClass {
    Loopback,
    SelfHost,
    ComputeNode,
    Other,
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Hostname charset sanity: DNS-ish labels only (also excludes whitespace,
/// `@`, `:` — so no userinfo smuggling into the ssh relay argv).
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' || c == ':')
        && (!host.contains(':') || host.parse::<IpAddr>().is_ok())
}

/// First DNS label, lowercased ("sh03-ln06.int" -> "sh03-ln06").
fn short_label(host: &str) -> String {
    host.split('.').next().unwrap_or(host).to_ascii_lowercase()
}

/// Does `host` name `trusted` closely enough to skip the user confirmation the
/// proxy relies on? Either an exact, case-insensitive match, or a BARE short
/// name (no dot) equal to `trusted`'s first label — the ordinary case where an
/// app prints `sh03-09n14` but the daemon knows the node as `sh03-09n14.int`.
///
/// A DOTTED host is trusted ONLY by exact match. Matching on the short label
/// alone would wave through `trusted.attacker.example`: an attacker can stand
/// up a host whose first label coincides with the daemon's own name or an
/// allocated node, and short-label matching would auto-open it as a
/// same-origin pane without the confirmation gate (P1, codex round 4).
fn host_matches(host: &str, trusted: &str) -> bool {
    host.eq_ignore_ascii_case(trusted)
        || (!host.contains('.') && short_label(host) == short_label(trusted))
}

/// Expand a Slurm nodelist expression conservatively: `a[1-3,7],b02` →
/// a1 a2 a3 a7 b02. Zero-padding is preserved; anything unparseable is kept
/// verbatim (it then only matches exactly). Capped — a giant allocation must
/// not balloon the allowlist.
fn expand_nodelist(list: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut item = String::new();
    let mut items = Vec::new();
    for c in list.chars() {
        match c {
            '[' => {
                depth += 1;
                item.push(c);
            }
            ']' => {
                depth = depth.saturating_sub(1);
                item.push(c);
            }
            ',' if depth == 0 => {
                items.push(std::mem::take(&mut item));
            }
            _ => item.push(c),
        }
    }
    if !item.is_empty() {
        items.push(item);
    }
    for it in items {
        if out.len() >= cap {
            break;
        }
        match (it.find('['), it.rfind(']')) {
            (Some(open), Some(close)) if close == it.len() - 1 && open < close => {
                let prefix = &it[..open];
                let body = &it[open + 1..close];
                for part in body.split(',') {
                    if out.len() >= cap {
                        break;
                    }
                    if let Some((a, b)) = part.split_once('-') {
                        let width = a.len();
                        if let (Ok(lo), Ok(hi)) = (a.parse::<u64>(), b.parse::<u64>()) {
                            // saturating: a pathological node number must not
                            // overflow (debug panic) before the out.len() cap.
                            for n in lo..=hi.min(lo.saturating_add(cap as u64)) {
                                if out.len() >= cap {
                                    break;
                                }
                                out.push(format!("{prefix}{n:0width$}"));
                            }
                            continue;
                        }
                    }
                    out.push(format!("{prefix}{part}"));
                }
            }
            _ => out.push(it),
        }
    }
    out
}

async fn classify_host(state: &AppState, host: &str) -> HostClass {
    if is_loopback_host(host) {
        return HostClass::Loopback;
    }
    if host_matches(host, &state.hostname) {
        return HostClass::SelfHost;
    }
    // A node one of the user's own Slurm jobs runs on is theirs to reach.
    // Only RUNNING jobs have a real nodelist in `nodes`; for a pending job
    // that field is Slurm's pending-reason text, not a nodelist (matches the
    // UI's nodeCandidates filter). The nodelist is short names — a probed
    // target is too (nodeCandidates), so host_matches auto-allows only a bare
    // node name, never `<node>.attacker.example`.
    let snapshot = state.compute.snapshot(false).await;
    if snapshot.scheduler == "slurm" {
        for job in snapshot.jobs.iter().filter(|j| j.state == "RUNNING") {
            for node in expand_nodelist(&job.nodes, 512) {
                if host_matches(host, &node) {
                    return HostClass::ComputeNode;
                }
            }
        }
    }
    HostClass::Other
}

// --- REST: mint / list / revoke / health ----------------------------------------

#[derive(Deserialize)]
pub(crate) struct MintReq {
    host: String,
    port: u16,
    /// The user explicitly confirmed a target outside the auto allowlist.
    #[serde(default)]
    confirm: bool,
}

/// POST /api/v1/proxy {host, port, confirm?} — mint (or refresh) the proxy
/// session for a target. Idempotent per host:port.
pub(crate) async fn create_proxy(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MintReq>,
) -> Response {
    let host = req.host.trim().trim_matches(['[', ']']).to_string();
    if !valid_host(&host) || req.port == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid target host/port"})),
        )
            .into_response();
    }
    let class = classify_host(&state, &host).await;
    if class == HostClass::Other && !req.confirm {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "confirm_required",
                "detail": format!("{host} is not this host, loopback, or one of your compute nodes"),
            })),
        )
            .into_response();
    }
    // The daemon itself is not a proxy target (a same-origin loop).
    if req.port == state.port && matches!(class, HostClass::Loopback | HostClass::SelfHost) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "that port is chimaera itself"})),
        )
            .into_response();
    }

    let (id, evicted) = {
        let mut store = crate::lock(&state.proxies);
        let mut evicted = store.purge();
        if let Some((id, entry)) = store
            .entries
            .iter_mut()
            .find(|(_, e)| e.host == host && e.port == req.port)
        {
            entry.last_used = Instant::now();
            (id.clone(), evicted)
        } else {
            if store.entries.len() >= MAX_PROXIES {
                // Evict the most idle entry — the cap protects memory, not
                // correctness (an evicted pane transparently re-mints).
                if let Some(oldest) = store
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.last_used)
                    .map(|(id, _)| id.clone())
                {
                    if let Some(mut e) = store.entries.remove(&oldest) {
                        if let Some(child) = e.relay.take() {
                            evicted.push(child);
                        }
                    }
                }
            }
            let id = format!("p-{}", &chimaera_core::generate_token()[..32]);
            store.entries.insert(
                id.clone(),
                ProxyEntry {
                    host: host.clone(),
                    port: req.port,
                    last_used: Instant::now(),
                    route: None,
                    probe: Arc::new(tokio::sync::Mutex::new(())),
                    relay: None,
                },
            );
            (id, evicted)
        }
    };
    for child in evicted {
        kill_relay(child);
    }
    tracing::info!(%id, %host, port = req.port, "proxy session minted");
    Json(json!({"id": id, "host": host, "port": req.port, "url": format!("/proxy/{id}/")}))
        .into_response()
}

/// GET /api/v1/proxy — the live sessions (debugging / UI reconciliation).
pub(crate) async fn list_proxies(State(state): State<Arc<AppState>>) -> Response {
    let store = crate::lock(&state.proxies);
    let list: Vec<_> = store
        .entries
        .iter()
        .map(|(id, e)| {
            json!({
                "id": id,
                "host": e.host,
                "port": e.port,
                "idle_secs": e.last_used.elapsed().as_secs(),
                "via": e.route.map(|r| match r { Route::Direct => "direct", Route::Relay(_) => "relay" }),
            })
        })
        .collect();
    Json(json!({"proxies": list})).into_response()
}

/// DELETE /api/v1/proxy/{id} — revoke a session (pane closed).
pub(crate) async fn delete_proxy(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let child = {
        let mut store = crate::lock(&state.proxies);
        store.entries.remove(&id).and_then(|mut e| e.relay.take())
    };
    if let Some(child) = child {
        kill_relay(child);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// GET /api/v1/proxy/{id}/health — dial the target and report. Doubles as the
/// mounted pane's keep-alive (refreshes the idle clock).
pub(crate) async fn proxy_health(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match dial(&state, &id).await {
        Ok((stream, route)) => {
            drop(stream);
            Json(json!({
                "ok": true,
                "via": match route { Route::Direct => "direct", Route::Relay(_) => "relay" },
            }))
            .into_response()
        }
        Err(DialError::Expired) => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "expired"})),
        )
            .into_response(),
        Err(DialError::Unreachable(detail)) => {
            Json(json!({"ok": false, "error": "unreachable", "detail": detail})).into_response()
        }
    }
}

// --- dialing (direct, then the ssh second hop) -----------------------------------

enum DialError {
    /// No such proxy session (expired / revoked / daemon restarted).
    Expired,
    Unreachable(String),
}

/// One entry's dialing ingredients, snapshotted outside the store lock.
struct DialPlan {
    host: String,
    port: u16,
    route: Option<Route>,
    probe: Arc<tokio::sync::Mutex<()>>,
}

fn plan(state: &AppState, id: &str) -> Option<DialPlan> {
    let mut store = crate::lock(&state.proxies);
    let e = store.entries.get_mut(id)?;
    e.last_used = Instant::now();
    Some(DialPlan {
        host: e.host.clone(),
        port: e.port,
        route: e.route,
        probe: e.probe.clone(),
    })
}

async fn connect_route(host: &str, port: u16, route: Route) -> std::io::Result<TcpStream> {
    let attempt = async {
        match route {
            Route::Direct => TcpStream::connect((host, port)).await,
            Route::Relay(local) => TcpStream::connect(("127.0.0.1", local)).await,
        }
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, attempt).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "connect timed out",
        )),
    }
}

/// Dial a proxy session's target: cached route fast path, else probe (direct
/// TCP first, then the daemon-side `ssh -N -L` relay for loopback-bound
/// services on compute nodes). Returns the stream and the route used.
async fn dial(state: &AppState, id: &str) -> Result<(TcpStream, Route), DialError> {
    let p = plan(state, id).ok_or(DialError::Expired)?;

    // Fast path: a proven route, no locks held.
    if let Some(route) = p.route {
        if let Ok(s) = connect_route(&p.host, p.port, route).await {
            return Ok((s, route));
        }
        // Route went stale (server restarted, relay died): clear and reprobe.
        let dead_relay = {
            let mut store = crate::lock(&state.proxies);
            store.entries.get_mut(id).and_then(|e| {
                e.route = None;
                e.relay.take()
            })
        };
        if let Some(child) = dead_relay {
            kill_relay(child);
        }
    }

    // Probe under the entry's single-flight lock.
    let _guard = p.probe.lock().await;
    // Someone else may have proven a route while we waited.
    let fresh = plan(state, id).ok_or(DialError::Expired)?;
    if let Some(route) = fresh.route {
        if let Ok(s) = connect_route(&fresh.host, fresh.port, route).await {
            return Ok((s, route));
        }
    }

    let host = fresh.host;
    let port = fresh.port;
    // Loopback (and the daemon's own host) can only ever be direct.
    let direct_only = is_loopback_host(&host) || host.eq_ignore_ascii_case(&state.hostname);
    let direct_timeout = if direct_only {
        CONNECT_TIMEOUT
    } else {
        DIRECT_PROBE_TIMEOUT
    };
    let direct =
        tokio::time::timeout(direct_timeout, TcpStream::connect((host.as_str(), port))).await;
    match direct {
        Ok(Ok(stream)) => {
            set_route(state, id, Route::Direct, None);
            return Ok((stream, Route::Direct));
        }
        _ if direct_only => {
            return Err(DialError::Unreachable(format!(
                "nothing is listening on {host}:{port}"
            )));
        }
        _ => {}
    }

    // Second hop: the target is a remote node whose service may be bound to
    // its loopback — stand up ssh -N -L and connect through it.
    let (child, local) = start_relay(&host, port)
        .await
        .map_err(DialError::Unreachable)?;
    match connect_route(&host, port, Route::Relay(local)).await {
        Ok(stream) => {
            set_route(state, id, Route::Relay(local), Some(child));
            Ok((stream, Route::Relay(local)))
        }
        Err(err) => {
            kill_relay(child);
            Err(DialError::Unreachable(format!(
                "relay to {host} came up but {host}:{port} refused: {err}"
            )))
        }
    }
}

fn set_route(state: &AppState, id: &str, route: Route, relay: Option<tokio::process::Child>) {
    let orphan = {
        let mut store = crate::lock(&state.proxies);
        match store.entries.get_mut(id) {
            Some(e) => {
                e.route = Some(route);
                std::mem::replace(&mut e.relay, relay)
            }
            // Entry revoked mid-probe: don't leak the child.
            None => relay,
        }
    };
    if let Some(child) = orphan {
        kill_relay(child);
    }
}

/// Spawn `ssh -N -L` to `host` and wait for the local forward to accept.
/// BatchMode: the daemon has no tty and no askpass — login→compute-node ssh
/// is hostbased/agent on clusters; anything interactive fails honestly.
async fn start_relay(host: &str, port: u16) -> Result<(tokio::process::Child, u16), String> {
    let local = {
        let sock = std::net::TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("no local port for the relay: {e}"))?;
        sock.local_addr()
            .map_err(|e| format!("no local port for the relay: {e}"))?
            .port()
    };
    let mut child = tokio::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ExitOnForwardFailure=yes",
            // Login-node OpenSSH can be ancient (Sherlock's rejects
            // `accept-new` — found live, again). `no` + a null known_hosts is
            // the old-ssh-safe form, and right for cluster-internal hops
            // anyway: node host keys churn on reimage, and the login→node
            // trust is hostbased, not TOFU (the chained tunnel rung's exact
            // reasoning — chimaera-remote::spawn_chained_node_tunnel).
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=3",
            "-N",
            "-L",
        ])
        .arg(format!("127.0.0.1:{local}:127.0.0.1:{port}"))
        .arg(host)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not run ssh for the {host} relay: {e}"))?;

    let deadline = Instant::now() + RELAY_START_TIMEOUT;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            // Bounded read of ssh's last words — the honest error.
            let mut msg = String::new();
            if let Some(mut err) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let mut buf = vec![0u8; 4096];
                if let Ok(Ok(n)) =
                    tokio::time::timeout(Duration::from_millis(500), err.read(&mut buf)).await
                {
                    msg = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                }
            }
            return Err(format!(
                "ssh relay to {host} exited ({status}){}",
                if msg.is_empty() {
                    String::new()
                } else {
                    format!(": {msg}")
                }
            ));
        }
        if TcpStream::connect(("127.0.0.1", local)).await.is_ok() {
            return Ok((child, local));
        }
        if Instant::now() >= deadline {
            kill_relay(child);
            return Err(format!("ssh relay to {host} timed out"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// --- header hygiene + rewrites ---------------------------------------------------

const HOP_BY_HOP: [HeaderName; 8] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

/// Remove hop-by-hop headers (the static set plus anything the Connection
/// header names). Returns the Upgrade header value if the message asked for
/// an upgrade, so the caller can re-add the pair toward the other hop.
fn strip_hop_by_hop(headers: &mut HeaderMap) -> Option<HeaderValue> {
    let upgrade = headers.get(header::UPGRADE).cloned();
    let named: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|t| t.trim().parse::<HeaderName>().ok())
        .collect();
    for name in named {
        headers.remove(&name);
    }
    for name in HOP_BY_HOP {
        headers.remove(&name);
    }
    upgrade
}

fn wants_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        && headers.contains_key(header::UPGRADE)
}

/// How the upstream server sees its own host — what its Host/Origin checks
/// validate against. When we reach it over loopback (a local loopback target,
/// or ANY target through the ssh relay, which forwards to the node's
/// 127.0.0.1) it is a loopback-bound server that only trusts localhost:
/// Jupyter's `allow_remote_access` 403s any other Host outright, before the
/// token is even read (found live on a compute node). Present loopback there;
/// keep the real host only for a directly-dialed routable target, which may
/// name-based-vhost on it.
fn upstream_host(host: &str, route: Route) -> String {
    if matches!(route, Route::Relay(_)) || is_loopback_host(host) {
        "127.0.0.1".to_string()
    } else {
        host.to_string()
    }
}

/// Map an absolute-path (or target-absolute) Location back under the proxy
/// prefix. Relative and foreign locations pass through untouched.
fn rewrite_location(loc: &str, id: &str, host: &str, port: u16) -> Option<String> {
    if loc.starts_with('/') && !loc.starts_with("//") {
        return Some(format!("/proxy/{id}{loc}"));
    }
    for scheme in ["http", "https"] {
        for authority in [format!("{host}:{port}"), host.to_string()] {
            let origin = format!("{scheme}://{authority}");
            if let Some(rest) = loc.strip_prefix(&origin) {
                if rest.is_empty() {
                    return Some(format!("/proxy/{id}/"));
                }
                if rest.starts_with('/') {
                    return Some(format!("/proxy/{id}{rest}"));
                }
            }
        }
    }
    None
}

/// Rebuild the Referer a proxied app sees so it looks like a same-origin
/// referer on the app's own authority (some apps CSRF-check it), and so the
/// proxy id never leaks upstream in a URL.
fn map_referer(referer: &str, id: &str, host: &str, port: u16) -> Option<String> {
    let uri: Uri = referer.parse().ok()?;
    let mut path = uri.path_and_query().map(|pq| pq.to_string())?;
    if let Some(rest) = path.strip_prefix(&format!("/proxy/{id}")) {
        path = if rest.is_empty() {
            "/".into()
        } else {
            rest.into()
        };
    }
    Some(format!("http://{host}:{port}{path}"))
}

/// The rescue id named by our cookie, if any.
fn cookie_rescue_id(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(s) = value.to_str() else { continue };
        for pair in s.split(';') {
            if let Some((k, v)) = pair.split_once('=') {
                if k.trim() == RESCUE_COOKIE && !v.trim().is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// The rescue id named by a `/proxy/{id}/…` Referer, if any.
fn referer_rescue_id(headers: &HeaderMap) -> Option<String> {
    let referer = headers.get(header::REFERER)?.to_str().ok()?;
    let uri: Uri = referer.parse().ok()?;
    let rest = uri.path().strip_prefix("/proxy/")?;
    let id = rest.split('/').next()?;
    if id.starts_with("p-") && id.len() > 8 {
        Some(id.to_string())
    } else {
        None
    }
}

/// Strip our own rescue cookie from what goes upstream; every other cookie
/// (the app's _xsrf, session cookies) passes through.
fn strip_rescue_cookie(headers: &mut HeaderMap) {
    let kept: Vec<HeaderValue> = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|s| {
            let kept: Vec<&str> = s
                .split(';')
                .map(str::trim)
                .filter(|p| {
                    p.split_once('=')
                        .is_none_or(|(k, _)| k.trim() != RESCUE_COOKIE)
                })
                .collect();
            if kept.is_empty() {
                None
            } else {
                HeaderValue::from_str(&kept.join("; ")).ok()
            }
        })
        .collect();
    headers.remove(header::COOKIE);
    for v in kept {
        headers.append(header::COOKIE, v);
    }
}

/// Whether this request is a document/iframe navigation (claims the rescue
/// cookie). Sec-Fetch-Dest is the modern truth; the Accept sniff covers
/// clients that omit it.
fn is_doc_navigation(headers: &HeaderMap) -> bool {
    if let Some(dest) = headers.get("sec-fetch-dest").and_then(|v| v.to_str().ok()) {
        return matches!(dest, "document" | "iframe" | "frame");
    }
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"))
}

// --- error pages ------------------------------------------------------------------

/// A quiet, theme-neutral page for proxy failures — this renders inside the
/// pane's iframe, so it must not look like a raw browser error.
fn error_page(status: StatusCode, kind: &str, title: &str, detail: &str) -> Response {
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\">\
         <meta name=\"color-scheme\" content=\"light dark\">\
         <style>body{{display:flex;align-items:center;justify-content:center;height:100vh;\
         margin:0;font:13px/1.6 -apple-system,'Segoe UI',sans-serif;color:#888}}\
         div{{max-width:26rem;text-align:center;padding:0 1rem}}\
         b{{font-weight:600;color:#666}}@media(prefers-color-scheme:dark){{b{{color:#aaa}}}}</style>\
         <div><b>{}</b><br>{}</div>",
        html_escape(title),
        html_escape(detail),
    );
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-chimaera-proxy", kind)
        .body(Body::from(body))
        .unwrap_or_else(|_| status.into_response())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// --- the data plane -----------------------------------------------------------------

/// ANY /proxy/{id} and ANY /proxy/{id}/{*path} — the ticketed forwarder. The
/// wildcard is parsed off the raw URI (axum's Path decodes percent-escapes,
/// which must survive verbatim on a proxy).
pub(crate) async fn data_plane(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let path = req.uri().path();
    let Some(rest) = path.strip_prefix("/proxy/") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (id, upstream_path) = match rest.split_once('/') {
        Some((id, tail)) => (id.to_string(), format!("/{tail}")),
        None => {
            // /proxy/{id} bare: redirect to the slashed form so the app's
            // relative URLs resolve under the prefix.
            let id = rest.to_string();
            let query = req
                .uri()
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default();
            return Response::builder()
                .status(StatusCode::TEMPORARY_REDIRECT)
                .header(header::LOCATION, format!("/proxy/{id}/{query}"))
                .body(Body::empty())
                .unwrap_or_else(|_| StatusCode::TEMPORARY_REDIRECT.into_response());
        }
    };
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    forward(state, id, format!("{upstream_path}{query}"), true, req).await
}

/// The router fallback: embedded UI assets first, then the proxy rescue for
/// absolute-path apps (Jupyter with base_url `/` asks for `/static/…`,
/// `/api/kernels/…` — paths that escape any prefix), then the SPA rules.
pub(crate) async fn fallback(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let path = req.uri().path().to_string();

    let trimmed = path.trim_start_matches('/');
    if !trimmed.is_empty() {
        if let Some(resp) = assets::try_serve(trimmed) {
            return resp;
        }
    }

    // The proxy rescue. Reserved daemon namespaces are never proxied, so a
    // stale cookie cannot shadow real (or future) chimaera surface. /assets
    // is deliberately NOT reserved: chimaera's own chunks were already tried
    // above, and Vite-built apps (marimo) load their bundles from absolute
    // /assets/… paths that need the rescue.
    const RESERVED: [&str; 7] = [
        "/api/v1",
        "/ws/",
        "/proxy/",
        "/raw/",
        "/download/",
        "/agent-events/",
        "/mcp/",
    ];
    let reserved = RESERVED.iter().any(|p| path.starts_with(p));
    // A TOP-LEVEL document navigation is the workbench itself — a pane
    // iframe's navigations arrive as dest=iframe — so rescuing it would hand
    // the whole UI over to the proxied app on the user's next reload. The
    // bare "/" (the app shell) is protected unconditionally, fetch metadata
    // or not.
    let top_level_doc = req
        .headers()
        .get("sec-fetch-dest")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|d| d.eq_ignore_ascii_case("document"));
    if !reserved && path != "/" && !top_level_doc && req.headers().get(LOOP_GUARD).is_none() {
        // The requesting page's Referer names the exact app; the cookie
        // covers what carries no Referer (WebSocket handshakes, redirected
        // navigations). But `/assets/` is chimaera's OWN chunk namespace too:
        // a proxied app's asset carries a `/proxy/{id}` Referer (rescued by
        // the first branch), while a STALE workbench chunk after a redeploy
        // carries the workbench's own Referer and must fall through to the
        // `/assets/` 404 below — never be cookie-rescued to the app (which
        // would hand it 200 HTML, silently breaking the SPA). So skip the
        // bare-cookie fallback under `/assets/`; referer rescue still serves
        // a proxied app's bundles.
        let cookie_ok = !path.starts_with("/assets/");
        let id = referer_rescue_id(req.headers())
            .filter(|id| plan(&state, id).is_some())
            .or_else(|| {
                if cookie_ok {
                    cookie_rescue_id(req.headers()).filter(|id| plan(&state, id).is_some())
                } else {
                    None
                }
            });
        if let Some(id) = id {
            let pq = req
                .uri()
                .path_and_query()
                .map(|pq| pq.to_string())
                .unwrap_or(path.clone());
            return forward(state, id, pq, false, req).await;
        }
    }

    // The SPA rules, exactly as before the proxy existed: /api stays a JSON
    // 404, and a missing hashed build chunk under /assets/ must 404, not fall
    // back to index.html — a browser holding a stale index.html after a
    // redeploy would otherwise get HTML (200, text/html) for an old
    // `/assets/index-*.js`, fail to parse it as a module, and break silently
    // with no signal to hard-reload. SPA routes (extension-less paths) still
    // get index.html so client-side routing works.
    if path.starts_with("/api") {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    }
    if path.starts_with("/assets/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    assets::try_serve("index.html").unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

/// The forwarder: dial, stream the request up, stream the response back,
/// relaying a WebSocket upgrade as a raw byte tunnel.
async fn forward(
    state: Arc<AppState>,
    id: String,
    upstream_pq: String,
    prefixed: bool,
    req: Request,
) -> Response {
    if req.method() == Method::CONNECT {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if req.headers().contains_key(LOOP_GUARD) {
        return error_page(
            StatusCode::BAD_GATEWAY,
            "loop",
            "Proxy loop",
            "This target forwards back to chimaera itself.",
        );
    }

    let (host, port) = match plan(&state, &id) {
        Some(p) => (p.host, p.port),
        None => {
            return error_page(
                StatusCode::NOT_FOUND,
                "expired",
                "Preview session expired",
                "Reopen this page from chimaera to mint a fresh one.",
            )
        }
    };

    let (stream, route) = match dial(&state, &id).await {
        Ok(ok) => ok,
        Err(DialError::Expired) => {
            return error_page(
                StatusCode::NOT_FOUND,
                "expired",
                "Preview session expired",
                "Reopen this page from chimaera to mint a fresh one.",
            )
        }
        Err(DialError::Unreachable(detail)) => {
            return error_page(
                StatusCode::BAD_GATEWAY,
                "unreachable",
                &format!("Can't reach {host}:{port}"),
                &format!("{detail}. Is the server still running?"),
            )
        }
    };

    let (mut send, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await {
        Ok(ok) => ok,
        Err(err) => {
            return error_page(
                StatusCode::BAD_GATEWAY,
                "unreachable",
                &format!("Can't reach {host}:{port}"),
                &format!("handshake failed: {err}"),
            )
        }
    };
    tokio::spawn(async move {
        // with_upgrades: the connection hands the socket over after a 101.
        if let Err(err) = conn.with_upgrades().await {
            tracing::debug!(%err, "proxy upstream connection ended");
        }
    });

    // Build the upstream request from the incoming one.
    let (mut parts, body) = req.into_parts();
    let client_upgrade = parts.extensions.remove::<OnUpgrade>();
    let upgrading = wants_upgrade(&parts.headers);
    let upgrade_proto = strip_hop_by_hop(&mut parts.headers);
    strip_rescue_cookie(&mut parts.headers);

    let up_host = upstream_host(&host, route);
    let authority = format!("{up_host}:{port}");
    if let Ok(v) = HeaderValue::from_str(&authority) {
        parts.headers.insert(header::HOST, v);
    }
    // The proxy is the client from the app's point of view: Origin/Referer
    // move to the app's own authority (Jupyter rejects WS handshakes whose
    // Origin doesn't match its Host), and the proxy id never leaks upstream.
    if parts.headers.contains_key(header::ORIGIN) {
        if let Ok(v) = HeaderValue::from_str(&format!("http://{authority}")) {
            parts.headers.insert(header::ORIGIN, v);
        }
    }
    if let Some(referer) = parts
        .headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    {
        match map_referer(&referer, &id, &up_host, port)
            .and_then(|r| HeaderValue::from_str(&r).ok())
        {
            Some(v) => {
                parts.headers.insert(header::REFERER, v);
            }
            None => {
                parts.headers.remove(header::REFERER);
            }
        }
    }
    parts.headers.insert(
        HeaderName::from_static(LOOP_GUARD),
        HeaderValue::from_static("1"),
    );
    if upgrading {
        if let Some(proto) = upgrade_proto.clone() {
            parts.headers.insert(header::UPGRADE, proto);
            parts
                .headers
                .insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
        }
    }

    let uri: Uri = match upstream_pq.parse() {
        Ok(u) => u,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let mut upstream_req = Request::builder()
        .method(parts.method.clone())
        .uri(uri)
        .version(axum::http::Version::HTTP_11)
        .body(body)
        .expect("proxy request build cannot fail");
    *upstream_req.headers_mut() = parts.headers.clone();

    let resp =
        match tokio::time::timeout(RESPONSE_HEAD_TIMEOUT, send.send_request(upstream_req)).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(err)) => {
                return error_page(
                    StatusCode::BAD_GATEWAY,
                    "unreachable",
                    &format!("Can't reach {host}:{port}"),
                    &format!("{err}"),
                )
            }
            Err(_) => {
                return error_page(
                    StatusCode::GATEWAY_TIMEOUT,
                    "unreachable",
                    &format!("{host}:{port} did not answer"),
                    "The server accepted the connection but never sent response headers.",
                )
            }
        };

    // A 101 switches both sides to a raw byte tunnel.
    if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        let Some(client_upgrade) = client_upgrade else {
            return error_page(
                StatusCode::BAD_GATEWAY,
                "upgrade",
                "Upgrade failed",
                "The app switched protocols but the client connection cannot.",
            );
        };
        let tunnels = crate::lock(&state.proxies).tunnels.clone();
        if tunnels.fetch_add(1, Ordering::SeqCst) >= MAX_TUNNELS {
            tunnels.fetch_sub(1, Ordering::SeqCst);
            return error_page(
                StatusCode::SERVICE_UNAVAILABLE,
                "tunnels",
                "Too many live connections",
                "The daemon's WebSocket tunnel cap was reached.",
            );
        }
        let mut resp = resp;
        let upstream_upgrade = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            struct Release(Arc<AtomicUsize>);
            impl Drop for Release {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _release = Release(tunnels);
            let (client, upstream) = match tokio::join!(client_upgrade, upstream_upgrade) {
                (Ok(c), Ok(u)) => (c, u),
                (c, u) => {
                    tracing::debug!(
                        client_err = c.is_err(),
                        upstream_err = u.is_err(),
                        "proxy upgrade did not complete"
                    );
                    return;
                }
            };
            let mut client = TokioIo::new(client);
            let mut upstream = TokioIo::new(upstream);
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        });

        // Relay the 101 verbatim (Sec-WebSocket-Accept and friends) so the
        // client's own upgrade completes; hyper hands the socket to the task
        // above once this response is written.
        let (parts, _) = resp.into_parts();
        let mut out = Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .body(Body::empty())
            .expect("101 build cannot fail");
        *out.headers_mut() = parts.headers;
        return out;
    }

    // Plain response: stream it back, with header-level fixups only.
    let (mut rparts, rbody) = resp.into_parts();
    strip_hop_by_hop(&mut rparts.headers);
    if prefixed {
        if let Some(loc) = rparts
            .headers
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
        {
            // The app's self-referential absolute redirects use the authority
            // we told it (up_host), so match that when re-prefixing.
            if let Some(mapped) = rewrite_location(loc, &id, &up_host, port) {
                if let Ok(v) = HeaderValue::from_str(&mapped) {
                    rparts.headers.insert(header::LOCATION, v);
                }
            }
        }
    }
    // Documents claim the rescue cookie: from here on, this app owns the
    // origin's unknown-path fallback (until another pane navigates).
    if prefixed && is_doc_navigation(&parts.headers) {
        if let Ok(v) = HeaderValue::from_str(&format!(
            "{RESCUE_COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict"
        )) {
            rparts.headers.append(header::SET_COOKIE, v);
        }
    }
    // Don't leak /proxy/{id} URLs to third-party sites the app links out to.
    if !rparts.headers.contains_key(header::REFERRER_POLICY) {
        rparts.headers.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        );
    }
    let mut out = Response::new(Body::new(rbody));
    *out.status_mut() = rparts.status;
    *out.headers_mut() = rparts.headers;
    out
}

// --- tests ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts_classify() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("sh02-01n58"));
        assert!(!is_loopback_host("10.0.0.1"));
    }

    #[test]
    fn host_charset_rejects_smuggling() {
        assert!(valid_host("sh02-01n58"));
        assert!(valid_host("node.cluster.edu"));
        assert!(valid_host("::1"));
        assert!(!valid_host("user@host"));
        assert!(!valid_host("host -oProxyCommand=evil"));
        assert!(!valid_host("-oProxyCommand=evil"));
        assert!(!valid_host(""));
        assert!(!valid_host("host:22")); // colon only allowed in IP literals
    }

    #[test]
    fn nodelist_expansion() {
        assert_eq!(expand_nodelist("sh02-01n58", 16), vec!["sh02-01n58"]);
        assert_eq!(
            expand_nodelist("sh02-01n[58-60]", 16),
            vec!["sh02-01n58", "sh02-01n59", "sh02-01n60"]
        );
        assert_eq!(
            expand_nodelist("a[01-03,7],b02", 16),
            vec!["a01", "a02", "a03", "a7", "b02"]
        );
        assert_eq!(expand_nodelist("n[1-100000]", 4).len(), 4, "cap holds");
        assert_eq!(expand_nodelist("", 4), Vec::<String>::new());
    }

    #[test]
    fn host_match_requires_exact_or_bare_short() {
        // Bare short name matching the trusted short label — the ordinary case
        // (an app prints `login01`, the daemon knows itself as `login01.int`).
        assert!(host_matches("login01", "login01.cluster.edu"));
        assert!(host_matches("SH03-09N14", "sh03-09n14"), "case-insensitive");
        // Exact canonical match, FQDN or bare.
        assert!(host_matches("login01.cluster.edu", "login01.cluster.edu"));
        assert!(host_matches("login01", "login01"));
        // The exploit: a DOTTED attacker host whose first label coincides with
        // a trusted short name must NOT auto-allow — it needs confirmation.
        assert!(!host_matches("login01.attacker.example", "login01"));
        assert!(!host_matches(
            "login01.attacker.example",
            "login01.cluster.edu"
        ));
        assert!(!host_matches("sh03-09n14.attacker.example", "sh03-09n14"));
        // A different bare name is never a match.
        assert!(!host_matches("login02", "login01"));
    }

    #[test]
    fn upstream_host_presents_loopback_over_loopback_routes() {
        // A relay reaches the node's loopback → present localhost (Jupyter's
        // allow_remote_access 403s any other Host).
        assert_eq!(
            upstream_host("sh04-18n32", Route::Relay(40000)),
            "127.0.0.1"
        );
        // A loopback target is loopback whatever the route.
        assert_eq!(upstream_host("localhost", Route::Direct), "127.0.0.1");
        assert_eq!(upstream_host("127.0.0.1", Route::Direct), "127.0.0.1");
        // A directly-dialed routable target keeps its real host (vhost-safe).
        assert_eq!(
            upstream_host("app.cluster.edu", Route::Direct),
            "app.cluster.edu"
        );
    }

    #[test]
    fn location_rewrites() {
        assert_eq!(
            rewrite_location("/lab?next=x", "p-abc", "127.0.0.1", 8888).as_deref(),
            Some("/proxy/p-abc/lab?next=x")
        );
        assert_eq!(
            rewrite_location("http://127.0.0.1:8888/tree", "p-abc", "127.0.0.1", 8888).as_deref(),
            Some("/proxy/p-abc/tree")
        );
        assert_eq!(
            rewrite_location("https://example.com/x", "p-abc", "127.0.0.1", 8888),
            None,
            "foreign absolute URLs pass through"
        );
        assert_eq!(
            rewrite_location("subpage", "p-abc", "127.0.0.1", 8888),
            None,
            "relative locations pass through"
        );
        assert_eq!(
            rewrite_location("//evil.com/x", "p-abc", "127.0.0.1", 8888),
            None,
            "protocol-relative is not an absolute path"
        );
    }

    #[test]
    fn referer_maps_to_target_authority() {
        assert_eq!(
            map_referer(
                "http://127.0.0.1:9700/proxy/p-abc/lab",
                "p-abc",
                "127.0.0.1",
                8888
            )
            .as_deref(),
            Some("http://127.0.0.1:8888/lab")
        );
        assert_eq!(
            map_referer("http://127.0.0.1:9700/lab/tree", "p-abc", "127.0.0.1", 8888).as_deref(),
            Some("http://127.0.0.1:8888/lab/tree"),
            "root-form (rescued) referers keep their path"
        );
        assert_eq!(
            map_referer(
                "http://127.0.0.1:9700/proxy/p-abc",
                "p-abc",
                "127.0.0.1",
                8888
            )
            .as_deref(),
            Some("http://127.0.0.1:8888/")
        );
    }

    #[test]
    fn rescue_ids_parse() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("foo=bar; chimaera_proxy=p-abc123; _xsrf=x"),
        );
        assert_eq!(cookie_rescue_id(&h).as_deref(), Some("p-abc123"));
        let mut h = HeaderMap::new();
        h.insert(
            header::REFERER,
            HeaderValue::from_static("http://127.0.0.1:9700/proxy/p-abcdef1234/static/main.js"),
        );
        assert_eq!(referer_rescue_id(&h).as_deref(), Some("p-abcdef1234"));
        let mut h = HeaderMap::new();
        h.insert(
            header::REFERER,
            HeaderValue::from_static("http://127.0.0.1:9700/#token=x"),
        );
        assert_eq!(referer_rescue_id(&h), None);
    }

    #[test]
    fn rescue_cookie_stripped_others_kept() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("chimaera_proxy=p-abc; _xsrf=tok; theme=dark"),
        );
        strip_rescue_cookie(&mut h);
        assert_eq!(
            h.get(header::COOKIE).and_then(|v| v.to_str().ok()),
            Some("_xsrf=tok; theme=dark")
        );
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("chimaera_proxy=p-abc"),
        );
        strip_rescue_cookie(&mut h);
        assert!(h.get(header::COOKIE).is_none());
    }

    #[test]
    fn hop_by_hop_stripped_including_connection_named() {
        let mut h = HeaderMap::new();
        h.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, x-custom"),
        );
        h.insert(
            HeaderName::from_static("x-custom"),
            HeaderValue::from_static("v"),
        );
        h.insert(
            header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        strip_hop_by_hop(&mut h);
        assert!(h.get(header::CONNECTION).is_none());
        assert!(h.get("x-custom").is_none());
        assert!(h.get(header::TRANSFER_ENCODING).is_none());
        assert!(h.get(header::CONTENT_TYPE).is_some());
    }

    #[test]
    fn doc_navigation_detection() {
        let mut h = HeaderMap::new();
        h.insert("sec-fetch-dest", HeaderValue::from_static("iframe"));
        assert!(is_doc_navigation(&h));
        let mut h = HeaderMap::new();
        h.insert("sec-fetch-dest", HeaderValue::from_static("empty"));
        assert!(!is_doc_navigation(&h));
        let mut h = HeaderMap::new();
        h.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/html,application/xhtml+xml"),
        );
        assert!(is_doc_navigation(&h));
    }
}
