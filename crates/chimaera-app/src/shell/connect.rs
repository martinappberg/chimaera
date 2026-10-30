//! The connect-flight state machine: one coalesced ssh attempt per host,
//! joined by every concurrent caller, plus the host-row wire vocabulary the
//! shell reports to the UI.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use chimaera_remote::hosts::{HostEntry, HostsStore};
use chimaera_remote::{ComputeTunnel, ConnectOpts, Phase, Tunnel};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use super::restore::open_ui_window;
use super::{authorize_scope_origin, lock, ConnectFlight, Shell};
use crate::windows::WindowRecord;

type ConnectFlightOwner = tokio::sync::watch::Sender<Option<Result<(), String>>>;

/// One flight's cancel switch: `true` once the user disconnected mid-flight.
/// Shared by the owner (which races its work against it and re-reads it
/// under the tunnels lock before installing anything) and `cancel_connect`.
type FlightCancel = Arc<tokio::sync::watch::Sender<bool>>;
type CancelMap = Mutex<HashMap<String, FlightCancel>>;

/// The cancel switch of every owned flight, by alias. Beside `Shell::
/// connecting` rather than inside it so the flight type joiners wait on stays
/// a plain receiver; lock order is always `connecting`, then this.
static FLIGHT_CANCELS: LazyLock<CancelMap> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// What a cancelled flight answers its owner and every joiner.
const CANCELLED: &str = "connect cancelled";

/// Claim the complete connect invocation synchronously, before its first
/// tunnel lock or liveness-probe await. Home promotion treats this map as the
/// authoritative "SSH may still ask for credentials" set. The owner's cancel
/// switch is registered under the same lock, so a cancel can never fall
/// between a flight becoming visible and becoming cancellable.
fn claim_connect_flight(
    connecting: &Mutex<HashMap<String, ConnectFlight>>,
    cancels: &CancelMap,
    alias: &str,
) -> Result<(ConnectFlightOwner, FlightCancel), ConnectFlight> {
    let mut connecting = lock(connecting);
    match connecting.get(alias) {
        Some(rx) => Err(rx.clone()),
        None => {
            let (tx, rx) = tokio::sync::watch::channel(None);
            connecting.insert(alias.to_string(), rx);
            let cancel: FlightCancel = Arc::new(tokio::sync::watch::channel(false).0);
            lock(cancels).insert(alias.to_string(), cancel.clone());
            Ok((tx, cancel))
        }
    }
}

/// Flip the in-flight cancel switch for `alias` (false: no flight owns it).
fn cancel_flight(
    connecting: &Mutex<HashMap<String, ConnectFlight>>,
    cancels: &CancelMap,
    alias: &str,
) -> bool {
    let _connecting = lock(connecting);
    match lock(cancels).remove(alias) {
        Some(cancel) => {
            cancel.send_replace(true);
            true
        }
        None => false,
    }
}

/// The owner is done: drop its switch, unless a newer flight already holds
/// the alias's slot.
fn release_flight(cancels: &CancelMap, alias: &str, mine: &FlightCancel) {
    let mut cancels = lock(cancels);
    if cancels.get(alias).is_some_and(|c| Arc::ptr_eq(c, mine)) {
        cancels.remove(alias);
    }
}

/// The owner's cleanup: free the alias for the next flight and tell every
/// joiner how this one went.
fn finish_flight<T>(
    connecting: &Mutex<HashMap<String, ConnectFlight>>,
    cancels: &CancelMap,
    alias: &str,
    tx: &ConnectFlightOwner,
    cancel: &FlightCancel,
    result: &Result<T, String>,
) {
    lock(connecting).remove(alias);
    release_flight(cancels, alias, cancel);
    let _ = tx.send(Some(match result {
        Ok(_) => Ok(()),
        Err(e) => Err(e.clone()),
    }));
}

fn is_cancelled(cancel: &FlightCancel) -> bool {
    *cancel.borrow()
}

/// Run `work` unless the flight is cancelled first. A cancel drops `work`
/// at its current await: every ssh/scp child it holds is `kill_on_drop`, and
/// nothing after that await — installing a tunnel, reopening windows — runs.
async fn cancellable<T>(
    cancel: &FlightCancel,
    work: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let mut switch = cancel.subscribe();
    tokio::select! {
        // A cancel already flipped wins over work that is also ready.
        biased;
        _ = switch.wait_for(|cancelled| *cancelled) => Err(CANCELLED.to_string()),
        result = work => result,
    }
}

/// Abort the in-flight connect for `alias`: joiners get [`CANCELLED`], its
/// ssh children die with the dropped future, its pending askpass prompts are
/// withdrawn, and it never installs a tunnel or reopens windows afterwards
/// (the owner re-checks the switch under the tunnels lock before installing,
/// so a caller that takes that lock after this returns sees any tunnel the
/// flight did install). True when there was a flight to cancel.
pub(super) fn cancel_connect(app: &AppHandle, alias: &str) -> bool {
    let state = app.state::<Shell>();
    let cancelled = cancel_flight(&state.connecting, &FLIGHT_CANCELS, alias);
    if cancelled {
        tracing::info!("cancelled the in-flight connect to {alias}");
        if let Some(askpass) = app.try_state::<crate::askpass::Askpass>() {
            askpass.discard_alias(alias);
        }
    }
    cancelled
}

/// Send a `host-status` to the windows allowed to see it: the full event
/// (with any token) to `status.alias`'s own windows, a tokenless copy to
/// local windows (Home's rows), nothing to other hosts' windows.
pub(super) fn emit_host_status(app: &AppHandle, status: HostStatus) {
    let public = HostStatus {
        token: None,
        ..status.clone()
    };
    let alias = status.alias.clone();
    super::emit_host_scoped(app, "host-status", &alias, status, Some(public));
}

/// Host list entry as the UI sees it (see HostState in native.ts).
#[derive(Clone, Serialize)]
pub struct HostState {
    alias: String,
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
        node: tunnel.and_then(|t| t.route.node().map(str::to_string)),
        cluster: None,
        not_cluster: entry.not_cluster,
        cluster_setup_complete: entry.cluster_setup_complete,
    }
}

impl HostState {
    /// A job window's key after its in-place update (no hosts.json entry).
    pub(super) fn compute(alias: String, local_port: u16) -> Self {
        HostState {
            alias,
            status: "connected",
            local_port: Some(local_port),
            last_connected_at: None,
            error: None,
            outdated: false,
            remote_build: None,
            live_sessions: None,
            node: None,
            cluster: None,
            not_cluster: false,
            cluster_setup_complete: false,
        }
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
                tunnel.route.node(),
            ),
        )
    };
    // Emit after dropping the tunnel lock. Event handlers can immediately
    // navigate and invoke native commands; none should queue behind delivery.
    lock(&state.unhealthy_tunnels).remove(alias);
    lock(&state.wedge_suspects).remove(alias);
    emit_host_status(app, event);
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
    let state = app.state::<Shell>();
    tracing::info!("ipc: connect_host {alias} (update_daemon: {update_daemon})");
    loop {
        let (tx, cancel) = match claim_connect_flight(&state.connecting, &FLIGHT_CANCELS, &alias) {
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
        let owned = async {
            let reused = if update_daemon {
                None
            } else {
                // Probe liveness WITHOUT holding the tunnels lock: this is a
                // ~2s HTTP round-trip, and holding the map locked across it
                // would stall every other tunnel op. A 401 from a
                // stale/foreign daemon on a recycled port is not a live
                // tunnel.
                let endpoint = state
                    .tunnels
                    .lock()
                    .await
                    .get(&alias)
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
            match reused {
                Some(reply) => Ok(reply),
                None => run_flight(app, &alias, update_daemon, &cancel).await,
            }
        };
        // Every owner path lands in the shared cleanup below so joiners never
        // hang: a reused tunnel, a new one, a failure, or a cancel.
        let result = cancellable(&cancel, owned).await;
        finish_flight(
            &state.connecting,
            &FLIGHT_CANCELS,
            &alias,
            &tx,
            &cancel,
            &result,
        );
        // Surfaces that only watch events (a home screen observing a
        // startup-restore connect) need the failure too, or their row sits
        // in "connecting" forever. Windows ignore this status: it carries no
        // port/token to re-home to, and only "down" arms their reconnect.
        if let Err(e) = &result {
            emit_host_status(
                app,
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
    cancel: &FlightCancel,
) -> Result<HostState, String> {
    let state = app.state::<Shell>();
    // Attribute the whole pre-connect stall — the wedge ladder and the old
    // tunnel's teardown below — to the row's "probing" label (the same one
    // `connect` re-emits when it starts).
    emit_progress(app, alias, "probing");
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
            Some(found) => return Ok(landed_on_cluster(app, alias, found).await),
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
    {
        // The switch is read under the lock a disconnect takes next, so a
        // cancel either stops this install or finds the tunnel to remove.
        let mut tunnels = state.tunnels.lock().await;
        if is_cancelled(cancel) {
            drop(tunnels);
            tunnel.close().await;
            return Err(CANCELLED.to_string());
        }
        tunnels.insert(alias.to_string(), tunnel);
    }
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
    if is_cancelled(cancel) {
        return Err(CANCELLED.to_string());
    }
    reopen_windows(app, alias, port, &token);
    Ok(host_state)
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
        emit_host_status(
            app,
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
        with_hosts(move |hosts| {
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
    emit_host_status(
        app,
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

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::{
        cancel_flight, cancellable, claim_connect_flight, connected_status, finish_flight,
        is_cancelled, release_flight, reusable_tunnel_port, CANCELLED,
    };
    use crate::shell::lock;

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
        let cancels = Mutex::new(HashMap::new());
        let owner =
            claim_connect_flight(&connecting, &cancels, "cluster").expect("first caller owns");
        let joiner = claim_connect_flight(&connecting, &cancels, "cluster")
            .expect_err("second caller joins");
        assert!(
            lock(&cancels).contains_key("cluster"),
            "the flight is cancellable from the moment it is visible"
        );

        let registered = lock(&connecting)
            .get("cluster")
            .expect("flight was published synchronously")
            .clone();
        assert!(registered.same_channel(&joiner));
        drop(owner);
    }

    #[test]
    fn a_cancel_flips_only_the_flight_that_owns_the_alias() {
        let connecting = Mutex::new(HashMap::new());
        let cancels = Mutex::new(HashMap::new());
        assert!(!cancel_flight(&connecting, &cancels, "host"), "no flight");
        let (_tx, first) = claim_connect_flight(&connecting, &cancels, "host").unwrap();
        let (_other_tx, other) = claim_connect_flight(&connecting, &cancels, "other").unwrap();
        assert!(cancel_flight(&connecting, &cancels, "host"));
        assert!(is_cancelled(&first));
        assert!(!is_cancelled(&other), "another host's flight runs on");
        assert!(
            !cancel_flight(&connecting, &cancels, "host"),
            "one cancel per flight"
        );

        // The cancelled owner's late release must not drop a newer flight's
        // switch.
        lock(&connecting).remove("host");
        let (_tx2, second) = claim_connect_flight(&connecting, &cancels, "host").unwrap();
        release_flight(&cancels, "host", &first);
        assert!(lock(&cancels)
            .get("host")
            .is_some_and(|c| Arc::ptr_eq(c, &second)));
    }

    /// Dropped with the work future, as a `kill_on_drop` ssh child would be.
    struct Child(Arc<AtomicBool>);
    impl Drop for Child {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn cancelling_mid_await_drops_the_work_and_fails_every_joiner() {
        let connecting = Arc::new(Mutex::new(HashMap::new()));
        let cancels = Arc::new(Mutex::new(HashMap::new()));
        let killed = Arc::new(AtomicBool::new(false));
        let installed = Arc::new(AtomicBool::new(false));
        let (tx, cancel) = claim_connect_flight(&connecting, &cancels, "host").unwrap();
        let mut joiner = claim_connect_flight(&connecting, &cancels, "host").unwrap_err();

        let owner = {
            let (connecting, cancels) = (connecting.clone(), cancels.clone());
            let (killed, installed) = (killed.clone(), installed.clone());
            tokio::spawn(async move {
                let work = async move {
                    let _child = Child(killed);
                    std::future::pending::<()>().await;
                    installed.store(true, Ordering::SeqCst);
                    Ok::<(), String>(())
                };
                let result = cancellable(&cancel, work).await;
                finish_flight(&connecting, &cancels, "host", &tx, &cancel, &result);
                result
            })
        };
        tokio::task::yield_now().await;
        assert!(cancel_flight(&connecting, &cancels, "host"));

        assert_eq!(owner.await.unwrap(), Err(CANCELLED.to_string()));
        let outcome = joiner.wait_for(|v| v.is_some()).await.unwrap().clone();
        assert_eq!(outcome, Some(Err(CANCELLED.to_string())));
        assert!(
            killed.load(Ordering::SeqCst),
            "the work's children die with it"
        );
        assert!(
            !installed.load(Ordering::SeqCst),
            "nothing after the await runs"
        );
        assert!(lock(&connecting).is_empty() && lock(&cancels).is_empty());
    }

    #[tokio::test]
    async fn a_cancel_before_the_first_poll_still_wins() {
        let cancels = Mutex::new(HashMap::new());
        let connecting = Mutex::new(HashMap::new());
        let (_tx, cancel) = claim_connect_flight(&connecting, &cancels, "host").unwrap();
        cancel_flight(&connecting, &cancels, "host");
        // Work that is ready at once still loses to an earlier cancel.
        let result = cancellable(&cancel, async { Ok::<u8, String>(1) }).await;
        assert_eq!(result, Err(CANCELLED.to_string()));
    }
}
