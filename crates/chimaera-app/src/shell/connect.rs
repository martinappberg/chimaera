//! The connect-flight state machine: one coalesced ssh attempt per host,
//! joined by every concurrent caller, plus the host-row wire vocabulary the
//! shell reports to the UI.

use std::collections::HashSet;

use chimaera_remote::hosts::{HostEntry, HostsStore};
use chimaera_remote::{ComputeTunnel, ConnectOpts, Phase};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use super::restore::open_ui_window;
use super::tunnel::Tunnel;
use super::{authorize_scope_origin, lock, ConnectFlight, Shell};
use crate::windows::WindowRecord;

type ConnectFlightOwner = tokio::sync::watch::Sender<Option<Result<(), String>>>;

/// Claim the complete connect invocation synchronously, before its first
/// tunnel lock or liveness-probe await. Home promotion treats this map as the
/// authoritative "SSH may still ask for credentials" set.
fn claim_connect_flight(
    connecting: &std::sync::Mutex<std::collections::HashMap<String, ConnectFlight>>,
    alias: &str,
) -> Result<ConnectFlightOwner, ConnectFlight> {
    let mut connecting = lock(connecting);
    match connecting.get(alias) {
        Some(rx) => Err(rx.clone()),
        None => {
            let (tx, rx) = tokio::sync::watch::channel(None);
            connecting.insert(alias.to_string(), rx);
            Ok(tx)
        }
    }
}

/// Host list entry as the UI sees it (see HostState in native.ts).
#[derive(Clone, Serialize)]
pub struct HostState {
    pub(super) alias: String,
    status: &'static str,
    local_port: Option<u16>,
    last_connected_at: Option<u64>,
    error: Option<String>,
    /// The connected daemon is an older build; live sessions kept connect
    /// from replacing it (the row offers the explicit update).
    outdated: bool,
    remote_build: Option<String>,
    live_sessions: Option<usize>,
    /// The login node the daemon runs on, when the alias names a pool of
    /// login nodes and the connection is pinned to one other than where a
    /// new ssh connection lands (`None` = wherever the alias lands).
    node: Option<String>,
    via_pro: bool,
    kept: bool,
    /// Present only for SSH hosts; older shells/devices do not offer the toggle.
    #[serde(skip_serializing_if = "Option::is_none")]
    direct_ssh: Option<bool>,
    /// Set when the host is a cluster: from this process's connect, else the
    /// scheduler the last connect recorded in hosts.json (a hint until the
    /// next probe). Never for a host the user said isn't one.
    cluster: Option<ClusterWire>,
    /// Legacy field retained on the native wire; saved overrides migrate to login_serve.
    not_cluster: bool,
    cluster_setup_complete: bool,
}

/// A cluster host as the page sees it (see `ClusterHostInfo` in native.ts).
#[derive(Clone, Serialize)]
pub struct ClusterWire {
    scheduler: chimaera_core::slurm::Scheduler,
    login_serve: bool,
    login_daemon: Option<LoginDaemonWire>,
}

#[derive(Clone, Serialize)]
struct LoginDaemonWire {
    node: String,
    pid: u32,
    alive: Option<bool>,
}

impl HostState {
    /// Attach what is known about `entry` being a cluster: this process's
    /// live verdict when there is one, else the hint hosts.json keeps.
    pub(super) fn with_cluster(
        mut self,
        entry: &HostEntry,
        live: Option<&super::cluster::ClusterInfo>,
    ) -> Self {
        self.not_cluster = entry.not_cluster;
        if self.direct_ssh.is_some() {
            self.direct_ssh = Some(entry.direct_ssh);
        }
        let scheduler = live
            .map(|i| i.scheduler)
            .or(entry.scheduler)
            .filter(|_| !entry.not_cluster);
        self.cluster = scheduler
            .filter(|s| s.is_cluster())
            .map(|scheduler| ClusterWire {
                scheduler,
                login_serve: entry.login_serve,
                login_daemon: live
                    .and_then(|i| i.login_daemon.as_ref())
                    .map(|d| LoginDaemonWire {
                        node: d.node.clone(),
                        pid: d.pid,
                        alive: d.alive,
                    }),
            });
        self
    }
}

#[derive(Clone, Serialize)]
struct ConnectProgress {
    alias: String,
    phase: &'static str,
    /// The login node a `routing` phase is reaching.
    #[serde(skip_serializing_if = "Option::is_none")]
    node: Option<String>,
}

/// Live tunnel liveness, pushed as hosts drop or reconnect (see the health
/// monitor and `host-status` in native.ts). `token` rides the `connected`
/// transition so a window whose remote daemon restarted (fresh token) can
/// re-home; it is omitted on `down`. `error` rides the `error` transition (a
/// connect flight failed) so surfaces that only observe events — a home
/// screen watching a startup-restore connect — don't stay "connecting"
/// forever on a failure they never hear about.
#[derive(Clone, Serialize)]
pub(super) struct HostStatus {
    pub(super) alias: String,
    pub(super) status: &'static str,
    pub(super) local_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
    /// Human-facing context for a liveness transition. Unlike `error`, this
    /// does not mean a reconnect attempt failed; it explains why one began.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reason: Option<String>,
    /// Source build behind the tunnel. Open windows use a changed build as a
    /// navigation boundary even when the forward kept its port and token:
    /// hashed UI chunks never span daemon builds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) build: Option<String>,
    /// On `connected`: the login node the tunnel is pinned to (a pool alias
    /// whose daemon runs on another node than where the alias lands).
    /// Every connected event carries it, so a row re-routed by a reconnect it
    /// didn't start never keeps a stale node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) node: Option<String>,
}

fn connected_status(
    alias: &str,
    local_port: u16,
    token: &str,
    build: Option<&str>,
    node: Option<&str>,
) -> HostStatus {
    HostStatus {
        alias: alias.to_string(),
        status: "connected",
        local_port: Some(local_port),
        token: Some(token.to_string()),
        error: None,
        reason: None,
        build: build.map(str::to_string),
        node: node.map(str::to_string),
    }
}

/// A loopback origin is reusable only while it still names the same source
/// build. Replacing the daemon swaps the complete immutable asset set; keeping
/// the port would leave already-open windows requesting the prior build's
/// hashed chunks from the new server.
fn reusable_tunnel_port(port: u16, old_build: Option<&str>, update_daemon: bool) -> Option<u16> {
    (!update_daemon && chimaera_core::builds_match(chimaera_core::BUILD_ID, old_build))
        .then_some(port)
}

pub(super) fn state_for(
    entry: &chimaera_remote::hosts::HostEntry,
    status: &'static str,
    tunnel: Option<&Tunnel>,
) -> HostState {
    HostState {
        alias: entry.alias.clone(),
        status,
        local_port: tunnel.map(|t| t.local_port),
        last_connected_at: entry.last_connected_at,
        error: None,
        outdated: tunnel.is_some_and(|t| t.outdated),
        remote_build: tunnel.and_then(|t| t.remote_build.clone()),
        live_sessions: tunnel.and_then(|t| t.live_sessions),
        node: tunnel.and_then(|t| t.node().map(str::to_string)),
        via_pro: tunnel.is_some_and(|t| t.link_id().is_some()),
        kept: entry.kept,
        direct_ssh: Some(entry.direct_ssh),
        cluster: None,
        not_cluster: entry.not_cluster,
        cluster_setup_complete: entry.cluster_setup_complete,
    }
}

/// Publish the current endpoint identity alongside a successful connect
/// reply. Every success path uses this, including healthy-tunnel reuse and
/// joined flights: a caller may still hold an old daemon token even though
/// another window already rebuilt the shared tunnel.
async fn publish_connected_state(app: &AppHandle, state: &Shell, alias: &str) -> Option<HostState> {
    // The entry the flight stamped; a miss (none this process) is the only
    // publish that reads hosts.json.
    // The guard must not live across the fallback's await.
    let cached = lock(&state.host_entries).get(alias).cloned();
    let entry = match cached {
        Some(entry) => entry,
        None => host_entry(alias).await,
    };
    let (reply, event) = {
        let tunnels = state.tunnels.lock().await;
        let tunnel = tunnels.get(alias)?;
        (
            state_for(&entry, "connected", Some(tunnel)).with_cluster(
                &entry,
                lock(&state.clusters)
                    .get(alias)
                    .and_then(|c| c.info.as_ref()),
            ),
            connected_status(
                alias,
                tunnel.local_port,
                &tunnel.manifest.token,
                tunnel.manifest.build.as_deref(),
                tunnel.node(),
            ),
        )
    };
    // Emit after dropping the tunnel lock. Event handlers can immediately
    // navigate and invoke native commands; none should queue behind delivery.
    lock(&state.unhealthy_tunnels).remove(alias);
    lock(&state.wedge_suspects).remove(alias);
    let _ = app.emit("host-status", event);
    Some(reply)
}

/// One `chimaera_remote::connect` attempt, wiring each phase to a
/// `connect-progress` event. Factored out so a reconnect can retry on a fresh
/// port after a reused one clashes.
async fn run_connect(
    app: &AppHandle,
    alias: &str,
    entry: &chimaera_remote::hosts::HostEntry,
    local_port: Option<u16>,
    update_daemon: bool,
) -> anyhow::Result<Tunnel> {
    // Which home this lands on (dev vs real) is the build's property —
    // `RemoteHome::current` inside connect — so every path into a connect
    // (a row click, the health monitor's reconnect, launch-time window
    // restore) targets the same daemon by construction.
    let opts = ConnectOpts {
        local_port,
        binary: entry.binary.clone(),
        update_daemon,
        login_serve: entry.login_serve,
        not_cluster: entry.not_cluster,
    };
    let progress_app = app.clone();
    let progress_alias = alias.to_string();
    chimaera_remote::connect(alias, opts, move |phase| {
        let (phase, node) = match phase {
            Phase::Probing => ("probing", None),
            // A second authentication prompt may follow — for that node.
            Phase::Routing { node } => ("routing", Some(node)),
            Phase::Updating => ("updating", None),
            Phase::Downloading { .. } => ("downloading", None),
            Phase::Installing { .. } => ("installing", None),
            Phase::Starting => ("starting", None),
            Phase::Tunneling { .. } => ("tunneling", None),
        };
        emit_progress_at(&progress_app, &progress_alias, phase, node);
    })
    .await
    .map(Tunnel::from)
}

/// One `connect-progress` event: the phase label a host row shows.
fn emit_progress(app: &AppHandle, alias: &str, phase: &'static str) {
    emit_progress_at(app, alias, phase, None);
}

fn emit_progress_at(app: &AppHandle, alias: &str, phase: &'static str, node: Option<String>) {
    let _ = app.emit(
        "connect-progress",
        ConnectProgress {
            alias: alias.to_string(),
            phase,
            node,
        },
    );
}

/// The connect flow behind the `connect_host` command, callable from
/// startup window restore too (no `State` extractor). Coalescing: only one
/// attempt per alias runs at a time; every concurrent caller awaits that
/// flight's outcome, so N windows reconnecting share ONE ssh auth flow (one
/// 2FA prompt) instead of stampeding or bouncing with errors.
pub(super) async fn do_connect(
    app: &AppHandle,
    alias: String,
    update_daemon: bool,
) -> Result<HostState, String> {
    connect_with_intent(app, alias, update_daemon, ConnectIntent::Explicit).await
}

pub(super) async fn restore_connect(app: &AppHandle, alias: String) -> Result<HostState, String> {
    connect_with_intent(app, alias, false, ConnectIntent::Restore).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectIntent {
    Explicit,
    Restore,
}
const KEEPER_CONNECT_REQUIRED: &str = "Connect to this host to resume its keeper connection";

async fn connect_with_intent(
    app: &AppHandle,
    alias: String,
    update_daemon: bool,
    intent: ConnectIntent,
) -> Result<HostState, String> {
    let state = app.state::<Shell>();
    tracing::info!("ipc: connect_host {alias} (update_daemon: {update_daemon})");
    loop {
        let tx = match claim_connect_flight(&state.connecting, &alias) {
            // Someone else owns the attempt: await its outcome. The clone
            // frees the watch borrow before we touch `rx` again below.
            Err(mut rx) => {
                let outcome = rx.wait_for(|v| v.is_some()).await.map(|v| v.clone());
                match outcome {
                    Ok(outcome) => match outcome.expect("wait_for guarantees Some") {
                        Ok(()) if !update_daemon => {
                            if let Some(reply) = cluster_state(&state, &alias).await {
                                return Ok(reply);
                            }
                            match publish_connected_state(app, &state, &alias).await {
                                Some(reply) => return Ok(reply),
                                // Disconnected between the flight landing and
                                // us looking — treat like any failed connect.
                                None => {
                                    return Err(format!("{alias} disconnected while connecting"));
                                }
                            }
                        }
                        // An update must still run its own flight — loop and
                        // own the next one.
                        Ok(()) => continue,
                        Err(e)
                            if intent == ConnectIntent::Explicit
                                && e == KEEPER_CONNECT_REQUIRED =>
                        {
                            continue
                        }
                        Err(e) => return Err(e),
                    },
                    // The owner died without reporting (task dropped). Clear
                    // the stale flight so the next iteration can own a fresh
                    // one instead of spinning on a closed channel.
                    Err(_) => {
                        let mut connecting = lock(&state.connecting);
                        if connecting.get(&alias).is_some_and(|r| r.same_channel(&rx)) {
                            connecting.remove(&alias);
                        }
                        continue;
                    }
                }
            }
            Ok(tx) => tx,
        };

        // We own the complete invocation, including the healthy-tunnel probe.
        // Publishing the flight before that first await keeps the only Home
        // fallback alive in case this path proceeds into password/2FA SSH.
        let reused = if update_daemon {
            None
        } else {
            let entry = host_entry(&alias).await;
            let via_pro = (!entry.direct_ssh && entry.kept)
                || keeper_route_selected(
                    entry.direct_ssh,
                    state.pro.client_now().is_some(),
                    lock(&state.pro.hosts)
                        .values()
                        .any(|host| host.alias == alias),
                );
            // Probe liveness WITHOUT holding the tunnels lock: this is a ~2s
            // HTTP round-trip, and holding the map locked across it would
            // stall every other tunnel op. A 401 from a stale/foreign daemon
            // on a recycled port is not a live tunnel.
            let endpoint = state
                .tunnels
                .lock()
                .await
                .get(&alias)
                .filter(|tunnel| tunnel.link_id().is_some() == via_pro)
                .map(|t| (t.local_port, t.manifest.token.clone()));
            if let Some((port, token)) = endpoint {
                if chimaera_remote::http_alive_authed(port, &token).await {
                    publish_connected_state(app, &state, &alias).await
                } else {
                    None
                }
            } else {
                None
            }
        };
        // Every owner path lands in the shared cleanup below so joiners never
        // hang, whether this reused a healthy tunnel or built a new one.
        let result = match reused {
            Some(reply) => Ok(reply),
            None => run_flight(app, &alias, update_daemon, intent).await,
        };
        lock(&state.connecting).remove(&alias);
        let _ = tx.send(Some(match &result {
            Ok(_) => Ok(()),
            Err(e) => Err(e.clone()),
        }));
        // Surfaces that only watch events (a home screen observing a
        // startup-restore connect) need the failure too, or their row sits
        // in "connecting" forever. Windows ignore this status: it carries no
        // port/token to re-home to, and only "down" arms their reconnect.
        if let Err(e) = &result {
            let _ = app.emit(
                "host-status",
                HostStatus {
                    alias: alias.clone(),
                    status: "error",
                    local_port: None,
                    token: None,
                    error: Some(e.clone()),
                    reason: None,
                    build: None,
                    node: None,
                },
            );
        }
        return result;
    }
}

/// One owned connect attempt: tear down the old tunnel, keeping its loopback
/// port only when it already points at this source build, run the connect,
/// install the new tunnel, and reopen this host's persisted windows.
async fn run_flight(
    app: &AppHandle,
    alias: &str,
    update_daemon: bool,
    intent: ConnectIntent,
) -> Result<HostState, String> {
    let state = app.state::<Shell>();
    // Attribute the whole pre-connect stall — the wedge ladder and the old
    // tunnel's teardown below — to the row's "probing" label (the same one
    // `connect` re-emits when it starts).
    emit_progress(app, alias, "probing");
    if let Some((client, host, generation)) = super::pro::connection(&state, alias).await? {
        if !super::tunnel::app_host(&host.kind) {
            // The account's cloud is never connected as a host, so no window
            // (a restored one from an older build, say) shows its own page
            // and no update is ever offered for it. Checked before any
            // reconnect, so nothing wakes it.
            forget_cloud_windows(&state, alias);
            return Err(CLOUD_IS_NOT_A_HOST.into());
        }
        return run_link_flight(app, client, host, generation, update_daemon, intent)
            .await
            .map_err(|error| match error {
                LinkFailure::Transport(error) | LinkFailure::Final(error) => error,
            });
    }
    // Remove under the map lock, then do process/network teardown without it.
    // `Tunnel::close` is bounded but still asynchronous; holding this lock made
    // one dead host freeze health checks and commands for every other host.
    let old = state.tunnels.lock().await.remove(alias);
    // After laptop sleep the host's ControlMaster is often alive on a dead
    // TCP link, and every mux client then queues on it until ssh's own
    // keepalive gives up (~45 s). Clear that first so the reconnect dials
    // fresh instead of stalling into a manual retry. Gated on the monitor's
    // confirmed-down verdict, which `wedge_suspects` keeps until a connect
    // succeeds — the tunnel itself is gone by now (a retry after a failed
    // flight has none), and a launch-time restore is a fresh process with
    // empty sets while ControlPersist keeps a wedged master alive for 10 m;
    // so a flight with no live tunnel at all runs the ladder too (an instant
    // local `-O check` when no master exists, one session open on a live
    // one). It runs BEFORE `old.close()`: that close `-O cancel`s a
    // master-held forward, which hangs on a wedged master until it is
    // `-O exit`ed. A user-initiated connect over a healthy tunnel never
    // pays it.
    let suspect = lock(&state.wedge_suspects).contains(alias);
    // A confirmed-down alias gets the short session bound (the node load that
    // made the health probes miss is the same load that slows `true`); a
    // flight with no verdict — launch restore, a click on a warm master —
    // gets twice that, so a merely loaded node never costs a healthy master.
    let session_bound = if suspect { 15 } else { 30 };
    if (suspect || old.is_none())
        && chimaera_remote::clear_wedged_master(alias, session_bound).await
    {
        tracing::info!("cleared a wedged ControlMaster to {alias} before reconnecting");
        drop_compute_tunnels_of(app, &state, alias).await;
    }
    let reuse_port = match old {
        Some(old) => {
            let port = old.local_port;
            let reuse = reusable_tunnel_port(port, old.manifest.build.as_deref(), update_daemon);
            old.close().await;
            reuse
        }
        None => None,
    };
    let entry = {
        let alias = alias.to_string();
        with_hosts(move |hosts| hosts.add(&alias, None)).await?
    };
    let result = run_connect(app, alias, &entry, reuse_port, update_daemon).await;
    // The reused port was free a moment ago (we just cancelled the forward),
    // but socket teardown can lag; fall back to an OS-assigned port so a
    // reconnect never fails on a transient bind clash. Only forward-phase
    // failures retry — re-running the whole connect after an auth failure
    // or cancel would raise a second 2FA prompt.
    let result = match result {
        Err(e)
            if reuse_port.is_some()
                && e.downcast_ref::<chimaera_remote::TunnelPhaseError>()
                    .is_some() =>
        {
            tracing::warn!("reconnect on port {reuse_port:?} failed ({e:#}); retrying fresh");
            run_connect(app, alias, &entry, None, update_daemon).await
        }
        other => other,
    };

    let tunnel = match result {
        Ok(tunnel) => tunnel,
        Err(e) => match e.downcast_ref::<chimaera_remote::ClusterHost>() {
            Some(found) => return Ok(landed_on_cluster(app, alias, found, None).await),
            None => return Err(format!("{e:#}")),
        },
    };
    if let Some(c) = lock(&state.clusters).get_mut(alias) {
        // A cluster the user allowed a login-node daemon on: still a cluster
        // (the page offers jobs), but no stale "left on the login node" note.
        if let Some(info) = &mut c.info {
            info.login_daemon = None;
        }
    }
    // Stamp the connection and keep the stamped entry for every later publish
    // of this alias (joiners, healthy-tunnel reuse); a failed stamp keeps the
    // pre-connect entry rather than failing a connect that just landed.
    let entry = {
        let alias = alias.to_string();
        with_hosts(move |hosts| hosts.record_connected(&alias)).await
    }
    .unwrap_or_else(|e| {
        tracing::debug!("could not record host {alias}: {e}");
        entry
    });
    lock(&state.host_entries).insert(alias.to_string(), entry);
    authorize_scope_origin(app, Some(alias), tunnel.local_port)
        .map_err(|e| format!("could not authorize {alias}'s daemon origin: {e}"))?;
    let (port, token) = (tunnel.local_port, tunnel.manifest.token.clone());
    state.tunnels.lock().await.insert(alias.to_string(), tunnel);
    // Publish only after installation, through the same path used by reuse
    // and flight joiners. Build identity is independently load-bearing: a
    // window from another immutable asset set must reload even on one port.
    let host_state = publish_connected_state(app, &state, alias)
        .await
        .ok_or_else(|| format!("{alias} disconnected while connecting"))?;
    // Any persisted windows for this host that aren't open come back now.
    // The registry keeps records across a failed launch-time restore
    // precisely so the first connect that DOES land — a home-screen click,
    // a window's reconnect — restores them, not just the next app start.
    reopen_windows(app, alias, port, &token);
    Ok(host_state)
}

/// The answer to a connect aimed at the account's cloud.
const CLOUD_IS_NOT_A_HOST: &str =
    "Your cloud doesn't open as a window here. Chimaera keeps it up to date for you.";

fn keeper_route_selected(direct_ssh: bool, signed_in: bool, known_keeper: bool) -> bool {
    !direct_ssh && signed_in && known_keeper
}

/// Drop saved windows on the account's cloud (an older build could open one):
/// they would only ever show the cloud's own page, which the app never opens.
fn forget_cloud_windows(state: &Shell, alias: &str) {
    let mut registry = lock(&state.registry);
    let saved: Vec<String> = registry
        .list()
        .into_iter()
        .filter(|record| record.alias.as_deref() == Some(alias))
        .map(|record| record.id)
        .collect();
    if !saved.is_empty() {
        tracing::info!(
            "not restoring {} saved window(s) on {alias}: the app never opens the cloud's own page",
            saved.len()
        );
    }
    for id in saved {
        registry.remove(&id);
    }
}

/// All failures of an account-owned route remain on that route. Direct SSH
/// is selected only by the host's explicit per-computer advanced preference.
#[derive(Debug)]
enum LinkFailure {
    Transport(String),
    Final(String),
}
impl From<String> for LinkFailure {
    fn from(error: String) -> Self {
        Self::Final(error)
    }
}
impl From<&str> for LinkFailure {
    fn from(error: &str) -> Self {
        Self::Final(error.into())
    }
}

async fn run_link_flight(
    app: &AppHandle,
    client: chimaera_link::Client,
    mut host: chimaera_link::Host,
    generation: u64,
    reconnect: bool,
    intent: ConnectIntent,
) -> Result<HostState, LinkFailure> {
    let state = app.state::<Shell>();
    let alias = host.alias.clone();
    let transport = |_| LinkFailure::Transport("Chimaera Pro is unreachable".into());
    let current = || state.pro.generation() == generation;
    if !current() {
        return Err("Account changed while connecting".into());
    }
    let accepted = reconnect || !keeper_host_ready(&host);
    if accepted {
        if intent != ConnectIntent::Explicit {
            return Err(KEEPER_CONNECT_REQUIRED.into());
        }
        host = explicit_keeper_connect(&state, &client, host, generation).await?;
    }
    if let Some(cluster) = host
        .cluster
        .as_ref()
        .filter(|cluster| !cluster.not_cluster && !cluster.login_serve)
    {
        if cluster.scheduler != chimaera_link::ClusterScheduler::Slurm {
            return Err("This keeper's cluster type isn't supported by this app".into());
        }
        client.cluster_capabilities().await.map_err(|_| {
            LinkFailure::Final("This keeper needs an update before opening its Jobs page".into())
        })?;
        // Account replacement owns the same operation gate. Keep this owner
        // through local persistence, window retirement and the final event.
        let authority = keeper_landing_authority(&state.pro.operation, current).await?;
        let previous = remove_current_keeper_tunnel(&state.tunnels, &alias, current).await?;
        if let Some(previous) = previous {
            previous.close().await;
        }
        if !current() {
            return Err("Account changed while connecting".into());
        }
        let found = chimaera_remote::ClusterHost {
            host: alias.clone(),
            scheduler: chimaera_core::slurm::Scheduler::Slurm,
            login_daemon: None,
        };
        let mut reply = landed_on_cluster(app, &alias, &found, Some(authority)).await;
        reply.via_pro = true;
        reply.kept = true;
        return Ok(reply);
    }
    let existing = {
        let mut tunnels = state.tunnels.lock().await;
        tunnels
            .get_mut(&alias)
            .filter(|tunnel| tunnel.link_id() == Some(host.id.as_str()))
            .map(|tunnel| {
                tunnel.update_link(&host);
                (tunnel.local_port, tunnel.manifest.token.clone())
            })
    };
    let new = if existing.is_none() {
        let link = chimaera_link::LinkTunnel::bind(client, host.id.clone())
            .await
            .map_err(|error| {
                if accepted {
                    LinkFailure::Final("Chimaera Pro is unreachable".into())
                } else {
                    transport(error)
                }
            })?;
        Some(Tunnel::link(&host, link).map_err(|error| error.to_string())?)
    } else {
        None
    };
    let (port, token) = existing
        .or_else(|| {
            new.as_ref()
                .map(|tunnel| (tunnel.local_port, tunnel.manifest.token.clone()))
        })
        .ok_or("Host is unavailable")?;
    let probe_deadline = tokio::time::Instant::now() + KEEPER_PROBE_WAIT;
    let mut pause = KEEPER_POLL_FIRST;
    while !chimaera_remote::http_alive_authed(port, &token).await {
        let unanswered = "Pro connected, but the host daemon did not answer";
        if !accepted {
            return Err(LinkFailure::Transport(unanswered.into()));
        }
        if !current() {
            return Err("Account changed while connecting".into());
        }
        if tokio::time::Instant::now() >= probe_deadline {
            return Err(unanswered.into());
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(KEEPER_POLL_MAX);
    }
    authorize_scope_origin(app, Some(&alias), port).map_err(|error| error.to_string())?;
    let old = {
        let mut tunnels = state.tunnels.lock().await;
        // Sign-out drains under the same map lock after advancing this epoch.
        // An in-flight health check must never reinstall its old account.
        if state.pro.generation() != generation {
            return Err("Account changed while connecting".into());
        }
        new.and_then(|tunnel| tunnels.insert(alias.clone(), tunnel))
    };
    if let Some(old) = old {
        old.close().await;
    }
    let mut entry = if host.kind == chimaera_link::HostKind::Ssh {
        let saved_alias = alias.clone();
        with_hosts(move |hosts| {
            hosts.set_kept(&saved_alias, true)?;
            hosts.record_connected(&saved_alias)
        })
        .await?
    } else {
        host_entry(&alias).await
    };
    entry.kept = true;
    lock(&state.host_entries).insert(alias.clone(), entry);
    let reply = publish_connected_state(app, &state, &alias)
        .await
        .ok_or("Host disconnected while connecting")?;
    reopen_windows(app, &alias, port, &token);
    Ok(reply)
}

type KeeperLandingAuthority = std::sync::Arc<tokio::sync::OwnedMutexGuard<()>>;

async fn keeper_landing_authority(
    operation: &std::sync::Arc<tokio::sync::Mutex<()>>,
    current: impl Fn() -> bool,
) -> Result<KeeperLandingAuthority, LinkFailure> {
    let guard = operation.clone().lock_owned().await;
    if !current() {
        return Err("Account changed while connecting".into());
    }
    Ok(std::sync::Arc::new(guard))
}

async fn remove_current_keeper_tunnel<T>(
    tunnels: &tokio::sync::Mutex<std::collections::HashMap<String, T>>,
    alias: &str,
    current: impl Fn() -> bool,
) -> Result<Option<T>, LinkFailure> {
    let mut tunnels = tunnels.lock().await;
    if !current() {
        return Err("Account changed while connecting".into());
    }
    Ok(tunnels.remove(alias))
}

fn keeper_host_ready(host: &chimaera_link::Host) -> bool {
    host.status == chimaera_link::HostStatus::Connected
        && (host.daemon.is_some()
            || host
                .cluster
                .as_ref()
                .is_some_and(|cluster| !cluster.login_serve && !cluster.not_cluster))
}

async fn explicit_keeper_connect(
    state: &Shell,
    client: &chimaera_link::Client,
    host: chimaera_link::Host,
    generation: u64,
) -> Result<chimaera_link::Host, LinkFailure> {
    let current = || state.pro.generation() == generation;
    #[cfg(all(feature = "ssh-agent-prototype", unix))]
    if host.kind == chimaera_link::HostKind::Ssh {
        use crate::ssh_agent::{connect, selection};
        let attempt = state
            .pro
            .ssh_authentication(generation)
            .map_err(|_| "Account changed while connecting")?;
        let mut cancellation = attempt.cancellation();
        let caps = client.ssh_auth_capabilities().await.map_err(|_| {
            "This keeper needs an update before connecting with this Mac's SSH keys"
        })?;
        if !caps.route_supported() {
            return Err("This keeper needs an update before native route authentication".into());
        }
        let selection = tokio::select! {
            biased;
            _ = cancellation.wait_for(|value| *value) => return Err("Account changed while connecting".into()),
            result = crate::ssh_agent::route::resolve(&host.alias, caps.keeper_boot) => result,
        }.map_err(|error| match error {
            selection::SelectionFailure::HostTrustRequired => "Confirm every SSH hop's fingerprint on this Mac before connecting through the keeper",
            selection::SelectionFailure::RevokedHost => "An SSH hop's key is revoked on this Mac",
            selection::SelectionFailure::TooManyKeys => "Choose this host's SSH identity in your SSH settings before connecting",
            _ => "This SSH route couldn't be verified on this Mac. Check its settings or choose Direct in advanced host settings",
        })?;
        return connect::authenticate_route(client, &host.id, selection, attempt, || async {
            await_keeper_login(|| client.hosts(), &host.id, current,
                tokio::time::Instant::now() + KEEPER_LOGIN_WAIT, KEEPER_POLL_FIRST)
                .await.map_err(|_| chimaera_link::SshAuthFailure::Unavailable)
        }).await.map_err(|_| "Couldn't authenticate this SSH route through the keeper. Check the hop's trust, SSH key or prompt and connect again.".into());
    }
    if !current() {
        return Err("Account changed while connecting".into());
    }
    client
        .reconnect_host(&host.id)
        .await
        .map_err(|_| LinkFailure::Transport("Chimaera Pro is unreachable".into()))?;
    await_keeper_login(
        || client.hosts(),
        &host.id,
        current,
        tokio::time::Instant::now() + KEEPER_LOGIN_WAIT,
        KEEPER_POLL_FIRST,
    )
    .await
}

/// A keeper login can wait minutes for a password or Duo answer.
const KEEPER_LOGIN_WAIT: std::time::Duration = std::time::Duration::from_secs(180);
/// A freshly connected daemon can take a moment to answer through the route.
const KEEPER_PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
const KEEPER_POLL_FIRST: std::time::Duration = std::time::Duration::from_millis(500);
const KEEPER_POLL_MAX: std::time::Duration = std::time::Duration::from_secs(3);

/// Waits for the keeper's row of a host whose reconnect it accepted. Polls
/// with backoff instead of twice a second for the whole wait. A failed read
/// says nothing about the login the keeper holds, so it never ends the wait
/// early and never yields `Transport`: a direct SSH attempt now would raise a
/// second password/Duo prompt.
async fn await_keeper_login<F, Fut>(
    mut read: F,
    id: &str,
    current: impl Fn() -> bool,
    deadline: tokio::time::Instant,
    first_pause: std::time::Duration,
) -> Result<chimaera_link::Host, LinkFailure>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Vec<chimaera_link::Host>>>,
{
    let mut pause = first_pause;
    let mut last_error = None;
    loop {
        if !current() {
            return Err("Account changed while connecting".into());
        }
        match read().await {
            Ok(hosts) => {
                let host = hosts
                    .into_iter()
                    .find(|candidate| candidate.id == id)
                    .ok_or("Host is no longer kept connected")?;
                if keeper_host_ready(&host) {
                    return Ok(host);
                }
                last_error = host.error;
            }
            Err(error) => tracing::debug!("Pro connection status unavailable: {error:#}"),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(last_error
                .unwrap_or_else(|| "Pro connection timed out".into())
                .into());
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(KEEPER_POLL_MAX);
    }
}

pub(super) fn keeper_state(host: &chimaera_link::Host, tunnel: Option<&Tunnel>) -> HostState {
    let entry = HostEntry {
        alias: host.alias.clone(),
        binary: None,
        added_at: 0,
        last_connected_at: None,
        kept: true,
        direct_ssh: false,
        login_serve: false,
        cluster_setup_complete: false,
        not_cluster: false,
        scheduler: None,
    };
    let status = match host.status {
        chimaera_link::HostStatus::Connecting | chimaera_link::HostStatus::Prompting => {
            "connecting"
        }
        chimaera_link::HostStatus::Connected if tunnel.is_some() => "connected",
        _ => "disconnected",
    };
    let mut state = state_for(&entry, status, tunnel);
    state.via_pro = tunnel.is_none_or(|tunnel| tunnel.link_id().is_some());
    state.error = host.error.clone();
    state.direct_ssh = (host.kind == chimaera_link::HostKind::Ssh).then_some(false);
    state
}

/// Open every persisted window record for `alias` without a live window
/// (matched on the record's stable id — an open window re-homes in place
/// and must not be duplicated).
fn reopen_windows(app: &AppHandle, alias: &str, port: u16, token: &str) {
    let Some(shell) = app.try_state::<Shell>() else {
        return;
    };
    let open: HashSet<String> = lock(&shell.windows)
        .values()
        .map(|s| s.stable_id.clone())
        .collect();
    let records: Vec<WindowRecord> = lock(&shell.registry)
        .list()
        .into_iter()
        // A compute record is scoped to a job's own tunnel, not the login
        // daemon this connect just landed — never reopen it here.
        .filter(|r| {
            r.alias.as_deref() == Some(alias) && !open.contains(&r.id) && r.compute.is_none()
        })
        .collect();
    for record in records {
        if let Err(e) = open_ui_window(app, port, token, &record) {
            tracing::warn!("could not reopen window {}: {e}", record.id);
        }
    }
}

/// `-O exit` on the login master also killed every compute forward riding
/// it (both rungs share the ControlPath). Drop those tunnels now and tell
/// their windows, instead of leaving them to three more monitor misses;
/// their own reconnect flights re-dial through the fresh master.
async fn drop_compute_tunnels_of(app: &AppHandle, state: &Shell, alias: &str) {
    let prefix = format!("{alias}#job");
    let dropped: Vec<(String, ComputeTunnel)> = {
        let mut compute = state.compute_tunnels.lock().await;
        let keys: Vec<String> = compute
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|key| compute.remove(&key).map(|tunnel| (key, tunnel)))
            .collect()
    };
    for (key, tunnel) in dropped {
        lock(&state.unhealthy_tunnels).remove(&key);
        let port = tunnel.local_port;
        // Its forward died with the master; this reaps the child (and its
        // `-O cancel` fails fast against the socket that is no longer there).
        tunnel.close().await;
        tracing::info!("dropped compute tunnel {key}: its login ControlMaster was reset");
        let _ = app.emit(
            "host-status",
            HostStatus {
                alias: key,
                status: "down",
                local_port: Some(port),
                token: None,
                error: None,
                reason: Some(
                    "The login host's ssh connection was reset after a link loss; this job's tunnel went with it and reconnects through the new one."
                        .to_string(),
                ),
                build: None,
                node: None,
            },
        );
    }
}

/// The connect landed on a cluster: nothing was started. Remember what it
/// found, forget windows that pointed at a login-node daemon (there is none
/// now), and answer with the cluster's state — a success, not an error.
async fn landed_on_cluster(
    app: &AppHandle,
    alias: &str,
    found: &chimaera_remote::ClusterHost,
    authority: Option<KeeperLandingAuthority>,
) -> HostState {
    let state = app.state::<Shell>();
    let info = super::cluster::ClusterInfo {
        scheduler: found.scheduler,
        login_daemon: found.login_daemon.clone(),
    };
    lock(&state.clusters)
        .entry(alias.to_string())
        .or_default()
        .info = Some(info.clone());
    let entry = {
        let alias = alias.to_string();
        let scheduler = found.scheduler;
        let persistence_authority = authority.clone();
        with_hosts(move |hosts| {
            // Blocking persistence outlives a canceled native future. It must
            // retain account ownership until its final file write completes.
            let _authority = persistence_authority;
            let stamped = hosts.record_connected(&alias)?;
            Ok(hosts
                .record_scheduler(&alias, scheduler)?
                .unwrap_or(stamped))
        })
        .await
    }
    .unwrap_or_else(|_| HostEntry {
        alias: alias.to_string(),
        binary: None,
        added_at: 0,
        last_connected_at: None,
        kept: false,
        direct_ssh: false,
        login_serve: false,
        cluster_setup_complete: false,
        not_cluster: false,
        scheduler: Some(found.scheduler),
    });
    lock(&state.host_entries).insert(alias.to_string(), entry.clone());
    // Saved windows onto this host's old login-node daemon can't come back.
    {
        let mut registry = lock(&state.registry);
        let stale: Vec<String> = registry
            .list()
            .into_iter()
            .filter(|r| r.alias.as_deref() == Some(alias) && r.compute.is_none())
            .map(|r| r.id)
            .collect();
        for id in stale {
            registry.remove(&id);
        }
    }
    let _ = app.emit(
        "host-status",
        HostStatus {
            alias: alias.to_string(),
            status: "cluster",
            local_port: None,
            token: None,
            error: None,
            reason: None,
            build: None,
            node: None,
        },
    );
    state_for(&entry, "cluster", None).with_cluster(&entry, Some(&info))
}

/// A cluster host's state when this process already knows it is one (a
/// joiner of a flight that landed on a cluster).
async fn cluster_state(state: &Shell, alias: &str) -> Option<HostState> {
    let info = lock(&state.clusters).get(alias)?.info.clone()?;
    if state.tunnels.lock().await.contains_key(alias) {
        return None;
    }
    let entry = lock(&state.host_entries).get(alias).cloned()?;
    Some(state_for(&entry, "cluster", None).with_cluster(&entry, Some(&info)))
}

async fn host_entry(alias: &str) -> HostEntry {
    let owned = alias.to_string();
    with_hosts(move |hosts| Ok(hosts.get(&owned)))
        .await
        .ok()
        .flatten()
        .unwrap_or(HostEntry {
            alias: alias.to_string(),
            binary: None,
            added_at: 0,
            last_connected_at: None,
            kept: false,
            direct_ssh: false,
            login_serve: false,
            cluster_setup_complete: false,
            not_cluster: false,
            scheduler: None,
        })
}

/// Serializes every hosts.json read-modify-write in this process: launch
/// restore runs one connect flight per alias concurrently, and two
/// interleaved load→mutate→save cycles lose an update. (A CLI `connect` in
/// another process writes the same file; the unique tmp name in
/// `HostsStore::save` is what keeps THAT from tearing it.)
static HOSTS_IO: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One load → mutate → save against hosts.json, serialized and off the
/// reactor. `HostsStore` is plain `std::fs` (a read, then an atomic tmp +
/// rename write), and its callers — IPC commands and connect flights — run
/// on the tokio reactor, where a slow home directory would stall every other
/// tunnel op. The closure's error is flattened here so call sites are a
/// plain `?`.
pub(super) async fn with_hosts<T: Send + 'static>(
    f: impl FnOnce(&mut HostsStore) -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    let _serialized = HOSTS_IO.lock().await;
    tokio::task::spawn_blocking(move || f(&mut HostsStore::load_default()))
        .await
        .map_err(|e| format!("hosts.json task failed: {e}"))?
        .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::{
        await_keeper_login, claim_connect_flight, connected_status, reusable_tunnel_port,
        LinkFailure,
    };
    use crate::shell::lock;
    use chimaera_link::{Daemon, Host, HostKind, HostStatus};
    use std::time::Duration;

    #[test]
    fn saved_direct_choice_overrides_known_keeper_without_changing_kept_state() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-direct-choice-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hosts.json");
        let mut hosts = chimaera_remote::hosts::HostsStore::load(path.clone());
        hosts.set_kept("cluster", true).unwrap();
        hosts.set_direct_ssh("cluster", true).unwrap();
        let saved = chimaera_remote::hosts::HostsStore::load(path)
            .get("cluster")
            .unwrap();
        assert!(saved.kept);
        assert!(!super::keeper_route_selected(saved.direct_ssh, true, true));
        assert!(crate::shell::pro::direct_ssh_bypass(saved.direct_ssh, false).unwrap());
        assert!(crate::shell::pro::direct_ssh_bypass(saved.direct_ssh, true).is_err());
        assert!(super::keeper_route_selected(false, true, true));
        assert!(!super::keeper_route_selected(false, false, true));
        let device = Host {
            kind: HostKind::Device,
            ..row(HostStatus::Connected, true)
        };
        let wire = serde_json::to_value(super::keeper_state(&device, None)).unwrap();
        assert!(
            wire.get("direct_ssh").is_none(),
            "device rows must not offer an SSH toggle"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn row(status: HostStatus, daemon: bool) -> Host {
        Host {
            id: "h-1".into(),
            alias: "Sherlock".into(),
            kind: HostKind::Ssh,
            status,
            daemon: daemon.then(|| Daemon {
                token: "t".into(),
                build: "b".into(),
                sessions: 0,
            }),
            error: None,
            cluster: None,
        }
    }

    #[tokio::test]
    async fn old_keeper_flight_cannot_remove_a_replacement_tunnel_after_waiting() {
        use std::sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        };
        let tunnels = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let generation = Arc::new(AtomicU64::new(1));
        let mut held = tunnels.lock().await;
        held.insert("host".into(), "old");
        let waiting = tunnels.clone();
        let epoch = generation.clone();
        let old = tokio::spawn(async move {
            super::remove_current_keeper_tunnel(&waiting, "host", || {
                epoch.load(Ordering::SeqCst) == 1
            })
            .await
        });
        tokio::task::yield_now().await;
        generation.store(2, Ordering::SeqCst);
        held.insert("host".into(), "replacement");
        drop(held);
        assert!(old.await.unwrap().is_err());
        assert_eq!(tunnels.lock().await.get("host"), Some(&"replacement"));
    }

    #[tokio::test]
    async fn keeper_landing_retains_account_authority_through_canceled_hosts_persistence() {
        use std::sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        };
        let operation = Arc::new(tokio::sync::Mutex::new(()));
        let generation = Arc::new(AtomicU64::new(1));
        let authority =
            super::keeper_landing_authority(&operation, || generation.load(Ordering::SeqCst) == 1)
                .await
                .unwrap();
        // The blocking file writer owns its clone even if native IPC is canceled.
        let persistence = authority.clone();
        drop(authority);
        let waiting = operation.clone();
        let epoch = generation.clone();
        let replacement = tokio::spawn(async move {
            let _guard = waiting.lock().await;
            epoch.store(2, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;
        assert_eq!(generation.load(Ordering::SeqCst), 1);
        assert!(!replacement.is_finished());
        drop(persistence);
        replacement.await.unwrap();
        assert!(
            super::keeper_landing_authority(&operation, || generation.load(Ordering::SeqCst) == 1)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn kept_slurm_ready_without_login_daemon_finishes_authentication() {
        let mut cluster = row(HostStatus::Connected, false);
        cluster.cluster = Some(chimaera_link::HostCluster {
            scheduler: chimaera_link::ClusterScheduler::Slurm,
            login_serve: false,
            not_cluster: false,
        });
        let result = await_keeper_login(
            || std::future::ready(Ok(vec![cluster.clone()])),
            "h-1",
            || true,
            tokio::time::Instant::now() + Duration::from_millis(30),
            Duration::from_millis(1),
        )
        .await
        .unwrap();
        assert!(result.daemon.is_none());
        cluster.cluster.as_mut().unwrap().login_serve = true;
        assert!(
            !super::keeper_host_ready(&cluster),
            "login opt-in needs a daemon"
        );
        cluster.cluster.as_mut().unwrap().login_serve = false;
        cluster.cluster.as_mut().unwrap().not_cluster = true;
        assert!(
            !super::keeper_host_ready(&cluster),
            "ordinary remote needs a daemon"
        );
    }

    #[tokio::test]
    async fn failed_reads_during_an_accepted_login_are_waited_out() {
        let mut script = vec![
            Err(anyhow::anyhow!("account unavailable")),
            Ok(vec![row(HostStatus::Prompting, false)]),
            Err(anyhow::anyhow!("keeper unavailable")),
            Ok(vec![row(HostStatus::Connected, true)]),
        ]
        .into_iter();
        let host = await_keeper_login(
            || std::future::ready(script.next().expect("polled past the script")),
            "h-1",
            || true,
            tokio::time::Instant::now() + Duration::from_secs(10),
            Duration::from_millis(1),
        )
        .await
        .expect("the login completes");
        assert_eq!(host.status, HostStatus::Connected);
    }

    #[tokio::test]
    async fn an_accepted_login_never_falls_back_to_direct_ssh() {
        let reads = std::cell::Cell::new(0);
        let failure = await_keeper_login(
            || {
                reads.set(reads.get() + 1);
                std::future::ready(Err(anyhow::anyhow!("account unavailable")))
            },
            "h-1",
            || true,
            tokio::time::Instant::now() + Duration::from_millis(40),
            Duration::from_millis(1),
        )
        .await
        .expect_err("the wait ends at its deadline");
        assert!(matches!(failure, LinkFailure::Final(_)), "{failure:?}");
        assert!(reads.get() > 1, "failed reads are retried");
        let gone = await_keeper_login(
            || std::future::ready(Ok(Vec::new())),
            "h-1",
            || true,
            tokio::time::Instant::now() + Duration::from_secs(10),
            Duration::from_millis(1),
        )
        .await
        .expect_err("the row is gone");
        assert!(matches!(gone, LinkFailure::Final(_)), "{gone:?}");
    }

    #[test]
    fn tunnel_port_is_reused_only_for_the_same_source_build() {
        let current = chimaera_core::BUILD_ID;
        assert_eq!(reusable_tunnel_port(9700, Some(current), false), Some(9700));
        assert_eq!(reusable_tunnel_port(9700, Some("different.1"), false), None);
        assert_eq!(reusable_tunnel_port(9700, None, false), None);
        assert_eq!(reusable_tunnel_port(9700, Some(current), true), None);
    }

    #[test]
    fn connected_status_carries_the_authoritative_endpoint() {
        let status = connected_status(
            "cluster",
            43123,
            "fresh-token",
            Some("build.2"),
            Some("login-a.cluster.example"),
        );
        assert_eq!(status.alias, "cluster");
        assert_eq!(status.status, "connected");
        assert_eq!(status.local_port, Some(43123));
        assert_eq!(status.token.as_deref(), Some("fresh-token"));
        assert_eq!(status.build.as_deref(), Some("build.2"));
        assert_eq!(status.node.as_deref(), Some("login-a.cluster.example"));
    }

    #[test]
    fn connect_flight_is_published_before_async_work_can_begin() {
        let connecting = Mutex::new(HashMap::new());
        let owner = claim_connect_flight(&connecting, "cluster").expect("first caller owns");
        let joiner = claim_connect_flight(&connecting, "cluster").expect_err("second caller joins");

        let registered = lock(&connecting)
            .get("cluster")
            .expect("flight was published synchronously")
            .clone();
        assert!(registered.same_channel(&joiner));
        drop(owner);
    }
}
