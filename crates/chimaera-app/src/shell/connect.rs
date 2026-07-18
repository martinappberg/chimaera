//! The connect-flight state machine: one coalesced ssh attempt per host,
//! joined by every concurrent caller, plus the host-row wire vocabulary the
//! shell reports to the UI.

use std::collections::HashSet;

use chimaera_remote::hosts::HostsStore;
use chimaera_remote::{ConnectOpts, Phase, Tunnel};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use super::restore::open_ui_window;
use super::{authorize_scope_origin, lock, Shell};
use crate::windows::WindowRecord;

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
}

#[derive(Clone, Serialize)]
struct ConnectProgress {
    alias: String,
    phase: &'static str,
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
    }
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
    };
    let progress_app = app.clone();
    let progress_alias = alias.to_string();
    chimaera_remote::connect(alias, opts, move |phase| {
        let phase = match phase {
            Phase::Probing => "probing",
            Phase::Updating => "updating",
            Phase::Downloading { .. } => "downloading",
            Phase::Installing { .. } => "installing",
            Phase::Starting => "starting",
            Phase::Tunneling { .. } => "tunneling",
        };
        let _ = progress_app.emit(
            "connect-progress",
            ConnectProgress {
                alias: progress_alias.clone(),
                phase,
            },
        );
    })
    .await
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
        // Reuse a live tunnel. `is_up` is an end-to-end HTTP probe: after
        // laptop sleep ssh's local listener often survives its dead
        // connection, and a bare TCP check here would return "connected"
        // without healing anything. An update never reuses — the old tunnel
        // points at the daemon being replaced.
        if !update_daemon {
            // Probe liveness WITHOUT holding the tunnels lock: `is_up` is a
            // ~2s HTTP round-trip, and holding the map locked across it would
            // stall every other tunnel op (an `open_window`, another window's
            // health check) behind it. Grab the endpoint identity, drop the
            // lock, probe, then re-lock only to build the reply. A 401 from a
            // stale/foreign daemon on a recycled port is not a live tunnel.
            let endpoint = state
                .tunnels
                .lock()
                .await
                .get(&alias)
                .map(|t| (t.local_port, t.manifest.token.clone()));
            if let Some((port, token)) = endpoint {
                if chimaera_remote::http_alive_authed(port, &token).await {
                    let tunnels = state.tunnels.lock().await;
                    if let Some(t) = tunnels.get(&alias) {
                        let entry = host_entry(&alias);
                        return Ok(state_for(&entry, "connected", Some(t)));
                    }
                }
            }
        }
        let flight = {
            let mut connecting = lock(&state.connecting);
            match connecting.get(&alias) {
                Some(rx) => Err(rx.clone()),
                None => {
                    let (tx, rx) = tokio::sync::watch::channel(None);
                    connecting.insert(alias.clone(), rx);
                    Ok(tx)
                }
            }
        };
        let tx = match flight {
            // Someone else owns the attempt: await its outcome. The clone
            // frees the watch borrow before we touch `rx` again below.
            Err(mut rx) => {
                let outcome = rx.wait_for(|v| v.is_some()).await.map(|v| v.clone());
                match outcome {
                    Ok(outcome) => match outcome.expect("wait_for guarantees Some") {
                        Ok(()) if !update_daemon => {
                            let tunnels = state.tunnels.lock().await;
                            match tunnels.get(&alias) {
                                Some(t) => {
                                    let entry = host_entry(&alias);
                                    return Ok(state_for(&entry, "connected", Some(t)));
                                }
                                // Disconnected between the flight landing and
                                // us looking — treat like any failed connect.
                                None => {
                                    return Err(format!("{alias} disconnected while connecting"))
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

        // We own the flight: run it, then publish the outcome — every path
        // out of `run_flight` must land here or joiners would hang.
        let result = run_flight(app, &alias, update_daemon).await;
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
                },
            );
        }
        return result;
    }
}

/// One owned connect attempt: tear down the dead tunnel (keeping its
/// loopback port so open windows heal in place), run the connect, install
/// the new tunnel, and reopen this host's persisted windows.
async fn run_flight(
    app: &AppHandle,
    alias: &str,
    update_daemon: bool,
) -> Result<HostState, String> {
    let state = app.state::<Shell>();
    let reuse_port = {
        let mut tunnels = state.tunnels.lock().await;
        match tunnels.remove(alias) {
            Some(old) => {
                let port = old.local_port;
                old.close().await;
                (!update_daemon).then_some(port)
            }
            None => None,
        }
    };
    let entry = HostsStore::load_default()
        .add(alias, None)
        .map_err(|e| format!("{e:#}"))?;
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

    let tunnel = result.map_err(|e| format!("{e:#}"))?;
    if let Err(e) = HostsStore::load_default().record_connected(alias) {
        tracing::debug!("could not record host {alias}: {e}");
    }
    let entry = host_entry(alias);
    let host_state = state_for(&entry, "connected", Some(&tunnel));
    authorize_scope_origin(app, Some(alias), tunnel.local_port)
        .map_err(|e| format!("could not authorize {alias}'s daemon origin: {e}"))?;
    // Tell open windows on this host to re-home if the port or token moved
    // (daemon restart / update); a same-port+token reconnect is a no-op for
    // them — their WebSocket just reconnects.
    let _ = app.emit(
        "host-status",
        HostStatus {
            alias: alias.to_string(),
            status: "connected",
            local_port: Some(tunnel.local_port),
            token: Some(tunnel.manifest.token.clone()),
            error: None,
        },
    );
    let (port, token) = (tunnel.local_port, tunnel.manifest.token.clone());
    state.tunnels.lock().await.insert(alias.to_string(), tunnel);
    // Any persisted windows for this host that aren't open come back now.
    // The registry keeps records across a failed launch-time restore
    // precisely so the first connect that DOES land — a home-screen click,
    // a window's reconnect — restores them, not just the next app start.
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

fn host_entry(alias: &str) -> chimaera_remote::hosts::HostEntry {
    HostsStore::load_default()
        .get(alias)
        .unwrap_or(chimaera_remote::hosts::HostEntry {
            alias: alias.to_string(),
            binary: None,
            added_at: 0,
            last_connected_at: None,
        })
}
