//! The IPC command surface the daemon-served UI calls
//! (`web-ui/src/lib/native.ts` is the other half of this contract — change
//! command and event names in lockstep). Thin delegators over the connect
//! flight state machine and the window/tunnel state.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use chimaera_remote::hosts::HostsStore;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use super::connect::{do_connect, state_for, HostState, HostStatus};
use super::restore::{open_compute_window, open_ui_window};
use super::{authorize_scope_origin, lock, ComputeEndpoint, Shell, WindowScope};
use crate::windows::{ComputeScope, WindowRecord};

/// The local daemon's build parity, as the home screen sees it.
#[derive(Clone, Serialize)]
pub struct LocalState {
    outdated: bool,
    build: Option<String>,
    live_sessions: Option<usize>,
    /// This app is a dev build (never release-stamped): every connection it
    /// makes targets the isolated `~/.chimaera-dev` homes (both ends), so
    /// the UI badges hosts and hides release-update affordances.
    dev_build: bool,
}

/// Payload of the `local-daemon-updated` broadcast: every window on the
/// local daemon re-homes itself to the new port + token.
#[derive(Clone, Serialize)]
struct LocalDaemonMoved {
    port: u16,
    token: String,
    build: Option<String>,
}

/// The label of an open window showing `(alias, ws)`, if any, excluding
/// `exclude` — used to raise an already-open workspace instead of duplicating.
fn find_by_scope(
    windows: &Mutex<HashMap<String, WindowScope>>,
    alias: &Option<String>,
    ws: &Option<String>,
    exclude: Option<&str>,
) -> Option<String> {
    lock(windows)
        .iter()
        .find(|(label, scope)| {
            exclude != Some(label.as_str()) && &scope.alias == alias && &scope.ws == ws
        })
        .map(|(label, _)| label.clone())
}

/// The label of an open window on `alias`, whatever workspace it shows. The
/// raise for job windows, whose identity IS their composite alias
/// (`"{alias}#job{id}"`): the SPA overwrites the stored `ws` once a workspace
/// opens inside one, so an exact `(alias, ws)` match would miss it and open a
/// duplicate window for the same job on every reconnect.
fn find_by_alias(windows: &Mutex<HashMap<String, WindowScope>>, alias: &str) -> Option<String> {
    lock(windows)
        .iter()
        .find(|(_, scope)| scope.alias.as_deref() == Some(alias))
        .map(|(label, _)| label.clone())
}

#[tauri::command]
pub(super) async fn list_hosts(state: State<'_, Shell>) -> Result<Vec<HostState>, String> {
    tracing::debug!("ipc: list_hosts");
    let store = HostsStore::load_default();
    let tunnels = state.tunnels.lock().await;
    let connecting: HashSet<String> = lock(&state.connecting).keys().cloned().collect();
    Ok(store
        .list()
        .iter()
        .map(|h| {
            if let Some(t) = tunnels.get(&h.alias) {
                state_for(h, "connected", Some(t))
            } else if connecting.contains(&h.alias) {
                state_for(h, "connecting", None)
            } else {
                state_for(h, "disconnected", None)
            }
        })
        .collect())
}

/// Which home a connect targets is the BUILD's property (a dev build always
/// talks to `~/.chimaera-dev` on both ends — see `RemoteHome::current`), so
/// there is nothing dev-related to save per host.
#[tauri::command]
pub(super) async fn add_host(alias: String) -> Result<HostState, String> {
    let alias = alias.trim().to_string();
    if alias.is_empty() || alias.starts_with('-') {
        return Err("that does not look like an ssh alias".to_string());
    }
    let mut store = HostsStore::load_default();
    let entry = store.add(&alias, None).map_err(|e| format!("{e:#}"))?;
    Ok(state_for(&entry, "disconnected", None))
}

#[tauri::command]
pub(super) async fn remove_host(state: State<'_, Shell>, alias: String) -> Result<(), String> {
    let tunnel = state.tunnels.lock().await.remove(&alias);
    if let Some(tunnel) = tunnel {
        tunnel.close().await;
    }
    HostsStore::load_default()
        .remove(&alias)
        .map_err(|e| format!("{e:#}"))?;
    Ok(())
}

#[tauri::command]
pub(super) async fn connect_host(
    app: AppHandle,
    alias: String,
    update_daemon: Option<bool>,
) -> Result<HostState, String> {
    // On Windows ssh MUST run inside the WSL distro (Win32-OpenSSH has no
    // ControlMaster). If the transport never got wired, fail with the real
    // reason instead of letting host ssh produce baffling per-host errors.
    #[cfg(windows)]
    if !chimaera_remote::wsl_transport_ready() {
        return Err(
            "remote hosts need the WSL2 daemon running (its distro carries ssh); \
             restart chimaera or finish WSL setup first"
                .to_string(),
        );
    }
    do_connect(&app, alias, update_daemon.unwrap_or(false)).await
}

#[tauri::command]
pub(super) async fn disconnect_host(state: State<'_, Shell>, alias: String) -> Result<(), String> {
    let tunnel = state.tunnels.lock().await.remove(&alias);
    if let Some(tunnel) = tunnel {
        tunnel.close().await;
    }
    Ok(())
}

/// End every session on a connected host — its daemon and our tunnel stay up.
/// "Kill everything running here" without the teardown, so the user can start
/// fresh immediately (no reconnect). Proxied in-band through the tunnel.
#[tauri::command]
pub(super) async fn end_host_sessions(
    state: State<'_, Shell>,
    alias: String,
) -> Result<(), String> {
    let (port, token) = {
        let tunnels = state.tunnels.lock().await;
        let t = tunnels
            .get(&alias)
            .ok_or_else(|| format!("{alias} is not connected"))?;
        (t.local_port, t.manifest.token.clone())
    };
    let sent = tokio::task::spawn_blocking(move || {
        crate::http::agent()
            .delete(&format!("http://127.0.0.1:{port}/api/v1/sessions"))
            .header("Authorization", &format!("Bearer {token}"))
            .config()
            .timeout_global(Some(Duration::from_secs(15)))
            .build()
            .call()
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    sent.map_err(|e| format!("could not end sessions on {alias}: {e}"))
}

/// Shut a connected host down: end every session AND stop its daemon, then
/// drop the tunnel. Unlike `disconnect_host` (which deliberately leaves the
/// daemon and its sessions running), this is the real off switch — reconnecting
/// later starts a fresh daemon. Driven in-band via `POST /shutdown` through the
/// tunnel: the daemon replies before it exits, then we cancel the forward.
#[tauri::command]
pub(super) async fn shutdown_host(state: State<'_, Shell>, alias: String) -> Result<(), String> {
    let (port, token) = {
        let tunnels = state.tunnels.lock().await;
        let t = tunnels
            .get(&alias)
            .ok_or_else(|| format!("{alias} is not connected"))?;
        (t.local_port, t.manifest.token.clone())
    };
    let sent = tokio::task::spawn_blocking(move || {
        crate::http::agent()
            .post(&format!("http://127.0.0.1:{port}/api/v1/shutdown"))
            .header("Authorization", &format!("Bearer {token}"))
            .config()
            .timeout_global(Some(Duration::from_secs(15)))
            .build()
            .send_empty()
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    sent.map_err(|e| format!("could not shut down {alias}: {e}"))?;
    // The daemon is on its way out; cancel our forward so the host reads as
    // down instead of lingering on a socket that's about to close.
    let tunnel = state.tunnels.lock().await.remove(&alias);
    if let Some(tunnel) = tunnel {
        tunnel.close().await;
    }
    Ok(())
}

/// The local daemon's build parity (home screen: quiet update note).
#[tauri::command]
pub(super) async fn local_state(state: State<'_, Shell>) -> Result<LocalState, String> {
    let d = lock(&state.local).clone();
    Ok(LocalState {
        outdated: d.outdated,
        build: d.build,
        live_sessions: d.live_sessions,
        dev_build: chimaera_core::is_dev_build(),
    })
}

/// Explicit local-daemon update: graceful stop, respawn our build, then
/// broadcast the new port + token so every window on the local daemon can
/// re-home itself (the old origin is gone).
#[tauri::command]
pub(super) async fn update_local_daemon(
    app: AppHandle,
    state: State<'_, Shell>,
) -> Result<(), String> {
    tracing::info!("ipc: update_local_daemon");
    let fresh = crate::daemon::update_local_daemon()
        .await
        .map_err(|e| format!("{e:#}"))?;
    let moved = LocalDaemonMoved {
        port: fresh.port,
        token: fresh.token.clone(),
        build: fresh.build.clone(),
    };
    authorize_scope_origin(&app, None, fresh.port)
        .map_err(|e| format!("could not authorize the updated daemon origin: {e}"))?;
    *lock(&state.local) = fresh;
    let _ = app.emit("local-daemon-updated", moved);
    Ok(())
}

/// The connected host's registered workspaces, proxied through the tunnel
/// (the home page's own origin cannot reach another daemon's port).
#[tauri::command]
pub(super) async fn remote_workspaces(
    state: State<'_, Shell>,
    alias: String,
) -> Result<serde_json::Value, String> {
    let (port, token) = {
        let tunnels = state.tunnels.lock().await;
        let t = tunnels
            .get(&alias)
            .ok_or_else(|| format!("{alias} is not connected"))?;
        (t.local_port, t.manifest.token.clone())
    };
    tokio::task::spawn_blocking(move || {
        let mut response = crate::http::agent()
            .get(&format!("http://127.0.0.1:{port}/api/v1/workspaces"))
            .header("Authorization", &format!("Bearer {token}"))
            .config()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .call()
            .map_err(|e| format!("could not list workspaces: {e}"))?;
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("could not read workspaces: {e}"))?;
        serde_json::from_str(&body).map_err(|e| format!("bad workspaces payload: {e}"))
    })
    .await
    .map_err(|e| format!("{e}"))?
}

/// Composite key for per-job compute state: distinct from the login alias so
/// a job window never collides with the host's own in focus-existing.
fn compute_key(alias: &str, job_id: &str) -> String {
    format!("{alias}#job{job_id}")
}

/// Releases a job's slot in `compute_connecting` on drop, so every exit path
/// out of the build (including `?`) clears the one-connect-per-job guard. Its
/// drop sits AFTER the tunnel lands in `compute_tunnels`: releasing between
/// build and insert left a gap where a concurrent connect passed both the map
/// check and the guard, built a duplicate tunnel, and its insert dropped the
/// first (kill_on_drop ssh) out from under the window just opened on it.
struct ConnectingGuard<'a> {
    connecting: &'a Mutex<HashSet<String>>,
    key: &'a str,
}

impl Drop for ConnectingGuard<'_> {
    fn drop(&mut self) {
        lock(self.connecting).remove(self.key);
    }
}

/// Same digit gate as the daemon's cancel route — the id lands in URL paths
/// and the composite tunnel key.
fn valid_job_id(job_id: &str) -> bool {
    !job_id.is_empty() && job_id.chars().all(|c| c.is_ascii_digit())
}

/// Surface the daemon's own `{"error": …}` body on an HTTP error — a launch
/// rejection carries the real reason ("invalid partition …"), not just a code.
fn compute_response_body(
    context: &str,
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<String, String> {
    let status = response.status();
    let body = response.body_mut().read_to_string();
    if status.is_success() {
        return body.map_err(|e| format!("{context}: could not read response: {e}"));
    }
    let message = body
        .ok()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .and_then(|value| {
            value
                .get("error")
                .and_then(|message| message.as_str())
                .map(str::to_string)
        });
    Err(match message {
        Some(message) => format!("{context}: {message}"),
        None => format!("{context}: HTTP {}", status.as_u16()),
    })
}

/// GET the login daemon's compute registry through the tunnel, cache each
/// session's node endpoint in Rust, and scrub `port`/`token` from the JSON:
/// the webview never sees compute tokens — the window URL built by
/// `connect_compute_session` is their only way out of this process.
async fn fetch_compute_sessions(state: &Shell, alias: &str) -> Result<serde_json::Value, String> {
    let (port, token) = {
        let tunnels = state.tunnels.lock().await;
        let t = tunnels
            .get(alias)
            .ok_or_else(|| format!("{alias} is not connected"))?;
        (t.local_port, t.manifest.token.clone())
    };
    let mut payload: serde_json::Value = tokio::task::spawn_blocking(move || {
        let response = crate::http::agent()
            .get(&format!("http://127.0.0.1:{port}/api/v1/compute/sessions"))
            .header("Authorization", &format!("Bearer {token}"))
            .config()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .build()
            .call()
            .map_err(|e| format!("could not list compute sessions: {e}"))?;
        let body = compute_response_body("could not list compute sessions", response)?;
        serde_json::from_str(&body).map_err(|e| format!("bad compute sessions payload: {e}"))
    })
    .await
    .map_err(|e| format!("{e}"))??;
    let mut fresh: Vec<(String, ComputeEndpoint)> = Vec::new();
    if let Some(sessions) = payload.get_mut("sessions").and_then(|s| s.as_array_mut()) {
        for session in sessions {
            let Some(obj) = session.as_object_mut() else {
                continue;
            };
            let job_id = obj
                .get("job_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let node = obj
                .get("node")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let routable = obj
                .get("routable")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Keep the endpoint Rust-side; JS gets the scrubbed card.
            let port = obj
                .remove("port")
                .and_then(|v| v.as_u64())
                .and_then(|p| u16::try_from(p).ok());
            let token = obj
                .remove("token")
                .and_then(|v| v.as_str().map(str::to_string));
            if let (Some(port), Some(token)) = (port, token) {
                if !job_id.is_empty() {
                    fresh.push((
                        job_id,
                        ComputeEndpoint {
                            node,
                            port,
                            token,
                            routable,
                        },
                    ));
                }
            }
        }
    }
    // REPLACE this alias's endpoint set under one lock so the cache always
    // mirrors the latest list: merging left an ended job's endpoint resolving
    // forever, which sent `connect_compute_session`'s tunnel ladder at a dead
    // node instead of letting its "no longer in the queue" arm tell the truth.
    let mut endpoints = lock(&state.compute_endpoints);
    endpoints.retain(|(a, _), _| a.as_str() != alias);
    for (job_id, endpoint) in fresh {
        endpoints.insert((alias.to_string(), job_id), endpoint);
    }
    Ok(payload)
}

/// The connected host's compute-session registry (Mode 2 cards), proxied
/// through the login tunnel like `remote_workspaces`. Returns
/// `{scheduler, sessions, partitions}` with each session's `port`/`token`
/// stripped (cached in Rust for `connect_compute_session`).
#[tauri::command]
pub(super) async fn remote_compute_sessions(
    state: State<'_, Shell>,
    alias: String,
) -> Result<serde_json::Value, String> {
    fetch_compute_sessions(&state, &alias).await
}

/// Submit a compute session (a chimaera daemon as a Slurm job) through the
/// login tunnel; returns the job id. The spec passes through verbatim — the
/// daemon owns validation (charset gates on every sbatch directive).
#[tauri::command]
pub(super) async fn launch_compute_session(
    state: State<'_, Shell>,
    alias: String,
    spec: serde_json::Value,
) -> Result<String, String> {
    tracing::info!("ipc: launch_compute_session on {alias}");
    let (port, token) = {
        let tunnels = state.tunnels.lock().await;
        let t = tunnels
            .get(&alias)
            .ok_or_else(|| format!("{alias} is not connected"))?;
        (t.local_port, t.manifest.token.clone())
    };
    tokio::task::spawn_blocking(move || {
        let response = crate::http::agent()
            .post(&format!("http://127.0.0.1:{port}/api/v1/compute/sessions"))
            .header("Authorization", &format!("Bearer {token}"))
            .config()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .send_json(spec)
            .map_err(|e| format!("could not launch the compute session: {e}"))?;
        let body = compute_response_body("could not launch the compute session", response)?;
        let v: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| format!("bad launch payload: {e}"))?;
        v.get("job_id")
            .and_then(|j| j.as_str())
            .map(str::to_string)
            .ok_or_else(|| "launch returned no job_id".to_string())
    })
    .await
    .map_err(|e| format!("{e}"))?
}

/// scancel a compute session through the login tunnel. Any live tunnel to
/// that job goes too — the daemon behind it is on its way out.
#[tauri::command]
pub(super) async fn cancel_compute_session(
    state: State<'_, Shell>,
    alias: String,
    job_id: String,
) -> Result<(), String> {
    if !valid_job_id(&job_id) {
        return Err("invalid job id".to_string());
    }
    tracing::info!("ipc: cancel_compute_session {alias} job {job_id}");
    let (port, token) = {
        let tunnels = state.tunnels.lock().await;
        let t = tunnels
            .get(&alias)
            .ok_or_else(|| format!("{alias} is not connected"))?;
        (t.local_port, t.manifest.token.clone())
    };
    let job = job_id.clone();
    tokio::task::spawn_blocking(move || {
        let response = crate::http::agent()
            .delete(&format!(
                "http://127.0.0.1:{port}/api/v1/compute/sessions/{job}"
            ))
            .header("Authorization", &format!("Bearer {token}"))
            .config()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .build()
            .call()
            .map_err(|e| format!("could not cancel the compute session: {e}"))?;
        compute_response_body("could not cancel the compute session", response).map(|_| ())
    })
    .await
    .map_err(|e| format!("{e}"))??;
    let tunnel = state
        .compute_tunnels
        .lock()
        .await
        .remove(&compute_key(&alias, &job_id));
    if let Some(tunnel) = tunnel {
        tunnel.close().await;
    }
    Ok(())
}

/// Build (or reuse) the tunnel to a job's compute-node daemon and open a
/// window on it. The endpoint comes from the Rust-side cache that
/// `remote_compute_sessions` fills — re-fetched once when missing (a connect
/// straight after an app restart).
#[tauri::command]
pub(super) async fn connect_compute_session(
    app: AppHandle,
    state: State<'_, Shell>,
    alias: String,
    job_id: String,
) -> Result<(), String> {
    if !valid_job_id(&job_id) {
        return Err("invalid job id".to_string());
    }
    tracing::info!("ipc: connect_compute_session {alias} job {job_id}");
    let key = compute_key(&alias, &job_id);
    // NOTE: no focus-existing early-return here — the tunnel is ensured
    // FIRST, so clicking open on a job whose window sits on a dead tunnel
    // repairs the tunnel instead of just raising a broken window (found
    // live: the raise-first order made a wedged window unrecoverable from
    // the card). The raise happens below, once the tunnel is proven.
    //
    // Reuse a live tunnel, probed end-to-end WITH identity (authed 200 —
    // after laptop sleep the local listener can outlive its dead
    // connection, and a bare liveness probe can be answered by the wrong
    // daemon through a stale relay) and without holding the lock across
    // the probe. A dead one is torn down and rebuilt.
    let existing = {
        let tunnels = state.compute_tunnels.lock().await;
        tunnels.get(&key).map(|t| (t.local_port, t.token.clone()))
    };
    if let Some((port, token)) = existing {
        if !chimaera_remote::http_alive_authed(port, &token).await {
            let tunnel = state.compute_tunnels.lock().await.remove(&key);
            if let Some(tunnel) = tunnel {
                tunnel.close().await;
            }
        }
    }
    if state.compute_tunnels.lock().await.get(&key).is_none() {
        // One connect per job at a time: the first live test produced eight
        // rapid clicks racing eight tunnel builds. The guard drops on every
        // exit path — but only after the insert into `compute_tunnels` below,
        // so there is no window where the job is neither "connecting" nor in
        // the map (see `ConnectingGuard`).
        {
            let mut connecting = lock(&state.compute_connecting);
            if !connecting.insert(key.clone()) {
                return Err(format!("already connecting to job {job_id} — hold on"));
            }
        }
        let _connecting = ConnectingGuard {
            connecting: &state.compute_connecting,
            key: &key,
        };
        let result = async {
            // The re-list below rides the LOGIN tunnel, and after laptop
            // sleep both tunnels are typically dead — nothing else heals the
            // login one for a job window, which listens only on its composite
            // key (never the login alias). Re-establish it through the same
            // coalesced flight as `connect_host`, so a concurrent
            // user-initiated connect joins the attempt instead of
            // double-building. Probe like the health monitor does: ssh's
            // local listener can outlive its dead connection, so absence
            // alone is not the test.
            let login_endpoint = {
                let tunnels = state.tunnels.lock().await;
                tunnels
                    .get(&alias)
                    .map(|t| (t.local_port, t.manifest.token.clone()))
            };
            let login_up = match login_endpoint {
                Some((port, token)) => chimaera_remote::http_alive_authed(port, &token).await,
                None => false,
            };
            if !login_up {
                do_connect(&app, alias.clone(), false).await?;
            }
            // ALWAYS re-list before tunneling: the endpoint cache may hold a
            // previous life of this queue, and a pending job must fail with
            // the truth ("still queued"), never a stale/foreign endpoint.
            let payload = fetch_compute_sessions(&state, &alias).await?;
            let endpoint = lock(&state.compute_endpoints)
                .get(&(alias.clone(), job_id.clone()))
                .cloned();
            let Some(endpoint) = endpoint else {
                let state_word = payload
                    .get("sessions")
                    .and_then(|s| s.as_array())
                    .and_then(|arr| {
                        arr.iter().find(|s| {
                            s.get("job_id").and_then(|j| j.as_str()) == Some(job_id.as_str())
                        })
                    })
                    .and_then(|s| s.get("state").and_then(|v| v.as_str()))
                    .unwrap_or("gone");
                return Err(match state_word {
                    "PENDING" => format!(
                        "job {job_id} is still queued — it can be opened once it starts running"
                    ),
                    "gone" => {
                        format!("job {job_id} is no longer in the queue (ended or cancelled)")
                    }
                    other => format!("job {job_id} is {other} — its daemon isn't reachable yet"),
                });
            };
            chimaera_remote::connect_compute_node(
                &alias,
                &endpoint.node,
                &job_id,
                endpoint.port,
                &endpoint.token,
                endpoint.routable,
            )
            .await
            .map_err(|e| format!("{e:#}"))
        }
        .await;
        let tunnel = result?;
        state
            .compute_tunnels
            .lock()
            .await
            .insert(key.clone(), tunnel);
    }
    // Snapshot what the window needs; the tunnel stays owned by the map.
    let (url, node, local_port) = {
        let tunnels = state.compute_tunnels.lock().await;
        let t = tunnels
            .get(&key)
            .ok_or_else(|| format!("{key} disconnected while connecting"))?;
        (t.url(), t.node.clone(), t.local_port)
    };
    authorize_scope_origin(&app, Some(&key), local_port)
        .map_err(|e| format!("could not authorize {key}'s daemon origin: {e}"))?;
    // A window already on this job → raise it; the status ping below tells
    // it the (possibly rebuilt) tunnel's port, and it re-homes itself if
    // that moved. Otherwise open a fresh window on the tunnel URL. Matched on
    // the composite alias ALONE — whichever workspace the window shows now.
    let raised =
        find_by_alias(&state.windows, &key).and_then(|label| app.get_webview_window(&label));
    match raised {
        Some(win) => {
            win.set_focus()
                .map_err(|e| format!("could not focus window: {e}"))?;
        }
        None => {
            let mut record = WindowRecord::new(Some(alias.clone()), None);
            record.compute = Some(ComputeScope {
                job_id: job_id.clone(),
                node: node.clone(),
            });
            let title = format!("{alias} › {node} — chimaera");
            open_compute_window(&app, &url, &title, &record, &key)
                .map_err(|e| format!("could not open window: {e}"))?;
        }
    }
    // Cheap status ping so a home screen can flip the card to "connected".
    // No token: compute tokens stay in Rust — the window URL above is the
    // only carrier, and only for the window that needs it.
    let _ = app.emit(
        "host-status",
        HostStatus {
            alias: key,
            status: "connected",
            local_port: Some(local_port),
            token: None,
            error: None,
            reason: None,
            build: None,
        },
    );
    Ok(())
}

/// Open a window on the local daemon (`alias` None) or a connected remote.
/// `ws_id` None lands on the home screen. Unless `new_window`, an existing
/// window already showing this `(alias, ws)` is raised instead of duplicated.
#[tauri::command]
pub(super) async fn open_window(
    app: AppHandle,
    state: State<'_, Shell>,
    alias: Option<String>,
    ws_id: Option<String>,
    new_window: Option<bool>,
) -> Result<(), String> {
    let new_window = new_window.unwrap_or(false);
    tracing::info!("ipc: open_window alias={alias:?} ws={ws_id:?} new_window={new_window}");
    if !new_window {
        if let Some(label) = find_by_scope(&state.windows, &alias, &ws_id, None) {
            if let Some(win) = app.get_webview_window(&label) {
                return win
                    .set_focus()
                    .map_err(|e| format!("could not focus window: {e}"));
            }
        }
    }
    let (port, token, host) = match alias {
        None => {
            let local = lock(&state.local);
            (local.port, local.token.clone(), None)
        }
        Some(alias) => {
            let tunnels = state.tunnels.lock().await;
            let t = tunnels
                .get(&alias)
                .ok_or_else(|| format!("{alias} is not connected"))?;
            (t.local_port, t.manifest.token.clone(), Some(alias.clone()))
        }
    };
    open_ui_window(&app, port, &token, &WindowRecord::new(host, ws_id))
        .map_err(|e| format!("could not open window: {e}"))
}

/// Check GitHub releases for a newer signed app build. Returns the new
/// version string when one is available, `None` when up to date. All
/// updater work runs in Rust; the web UI can only ask, never drive the
/// download — and the download is verified against the embedded minisign
/// pubkey regardless, so only a validly-signed release can ever install.
#[tauri::command]
pub(super) async fn check_app_update(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_updater::UpdaterExt;
    // Dev is dev: offering a release to a dev build would swap the build
    // under test (and the daemon it spawns) for a download — never an
    // "update". The signed-release channel is for stamped builds only.
    if chimaera_core::is_dev_build() {
        return Ok(None);
    }
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(update.version)),
        Ok(None) => Ok(None),
        // A missing/unreachable endpoint (no releases yet) is "no update",
        // not an error the user should see on every launch.
        Err(e) => {
            tracing::debug!("update check unavailable: {e}");
            Ok(None)
        }
    }
}

/// Answer an in-flight SSH auth prompt (see `askpass`): `secret` None means
/// the user cancelled, which lets the waiting ssh fail cleanly. The done
/// scoped completion event dismisses the prompt in every eligible window
/// showing it.
#[tauri::command]
pub(super) async fn answer_askpass(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    askpass: State<'_, crate::askpass::Askpass>,
    id: u64,
    secret: Option<String>,
) -> Result<(), String> {
    let scope = state
        .window_scope(webview.label())
        .ok_or_else(|| "this window is not registered".to_string())?;
    match askpass.answer_scoped(id, secret, scope.alias.as_deref(), scope.ws.as_deref()) {
        crate::askpass::AnswerResult::Answered(alias) => {
            crate::askpass::emit_done(&app, id, alias.as_deref());
            Ok(())
        }
        crate::askpass::AnswerResult::Missing => Ok(()),
        crate::askpass::AnswerResult::Forbidden => {
            Err("that authentication prompt is not available to this window".to_string())
        }
    }
}

/// SSH prompts still awaiting an answer. Each eligible window fetches this on mount:
/// the `ssh-askpass` emit reaches only windows that already exist, and
/// startup window restore starts connecting before the first webview has
/// loaded — without this, that prompt is lost and the host sits in
/// "connecting" until ssh times out, with nothing for the user to answer.
#[tauri::command]
pub(super) fn list_askpass(
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    askpass: State<'_, crate::askpass::Askpass>,
) -> Result<Vec<crate::askpass::PromptEvent>, String> {
    let scope = state
        .window_scope(webview.label())
        .ok_or_else(|| "this window is not registered".to_string())?;
    Ok(askpass.pending_scoped(scope.alias.as_deref(), scope.ws.as_deref()))
}

/// The SPA reporting what this window now shows — it swaps `ws` client-side,
/// so the shell can't see it otherwise. Keyed by the calling window's label;
/// the persisted record follows so the next launch reopens the window on
/// what it was ACTUALLY showing, not what it was opened on.
#[tauri::command]
pub(super) fn report_window_scope(
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    alias: Option<String>,
    ws: Option<String>,
    label: Option<String>,
) -> Result<(), String> {
    let mut windows = lock(&state.windows);
    let scope = windows
        .get_mut(webview.label())
        .ok_or_else(|| "this window is not registered".to_string())?;
    // The shell fixed the host when it created this window. A daemon-served
    // page may report workspace/label changes, but must never rewrite its
    // host to gain another remote's native command scope.
    if scope.alias != alias {
        return Err("a window cannot change its registered host".to_string());
    }
    scope.ws = ws.clone();
    scope.label = label.unwrap_or_default();
    let stable_id = scope.stable_id.clone();
    let registered_alias = scope.alias.clone();
    drop(windows);
    if !stable_id.is_empty() {
        lock(&state.registry).set_scope(&stable_id, registered_alias, ws);
    }
    // The reported label names this window in the tray's list; rebuild so it
    // shows the fresh name (the store above happened before this call).
    crate::tray::rebuild(webview.app_handle());
    // Its workspace also decides whether Settings applies (home screen = no).
    crate::menu::sync_settings_enabled(webview.app_handle());
    Ok(())
}

/// The UI's caffeinate toggle. The real work — and the same `caffeinate-changed`
/// broadcast the tray's "Keep Awake" item drives — lives in `apply_caffeinate`
/// so both surfaces share one guard and stay in sync.
#[tauri::command]
pub(super) fn set_caffeinate(app: AppHandle, on: bool) -> Result<bool, String> {
    super::apply_caffeinate(&app, on)
}

/// Whether the caffeinate assertion is currently held. Each window reads this on
/// mount to render its toggle; live changes ride the `caffeinate-changed` event.
#[tauri::command]
pub(super) fn caffeinate_state(state: State<'_, Shell>) -> bool {
    lock(&state.caffeinate).is_some()
}

/// This app binary's build id, for daemon-skew detection in the UI (the
/// daemon's own build rides GET /api/v1/health).
#[tauri::command]
pub(super) fn shell_build() -> String {
    chimaera_core::BUILD_ID.to_string()
}

/// Write text to the OS clipboard from the Rust process. The daemon-served UI
/// calls this for agent-initiated OSC 52 and copy-on-select: WKWebView rejects
/// `navigator.clipboard.writeText` from a non-gesture callback (a socket
/// message, a selection change) with NotAllowedError, so on a remote (app-only)
/// window those writes silently failed — "copy from the TUI doesn't reach the
/// clipboard". Running the write here has no transient-activation gate. Only
/// writes are exposed (OSC 52 reads are refused UI-side), so an agent can set
/// the clipboard but never read it back over the PTY.
#[tauri::command]
pub(super) fn write_clipboard(app: AppHandle, text: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

/// The one-click update chain, step one: record the user's consent (the
/// intent file), then download, verify, and install the app bundle and
/// relaunch into it. Step two — replacing the local daemon — happens in the
/// NEW process's startup, which consumes the intent; the daemon's restart
/// handoff + session ledger are what make that step keep every window,
/// tab, and session. Diverges on success; on failure the intent is cleared
/// so nothing acts on it later.
#[tauri::command]
pub(super) async fn begin_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    tracing::info!("ipc: begin_update");
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no update available".to_string())?;
    crate::update::write_intent().map_err(|e| format!("{e:#}"))?;
    match update
        .download_and_install(|_downloaded, _total| {}, || {})
        .await
    {
        Ok(()) => {
            tracing::info!("app update {} installed; relaunching", update.version);
            app.restart();
        }
        Err(e) => {
            crate::update::clear_intent();
            Err(e.to_string())
        }
    }
}

// --- WSL setup (the Windows first-run wizard; clean errors elsewhere) -----

/// Detection report for the wizard: registry facts plus the async WSL
/// version gate (never blocks on wsl.exe when the package is absent).
#[tauri::command]
pub(super) async fn wsl_status() -> Result<crate::wsl::WslReport, String> {
    tracing::debug!("ipc: wsl_status");
    Ok(crate::wsl::full_report().await)
}

/// One-time WSL enablement (UAC prompt; a reboot usually follows).
#[tauri::command]
pub(super) async fn wsl_install() -> Result<(), String> {
    tracing::info!("ipc: wsl_install");
    crate::wsl::launch_wsl_install()
        .await
        .map_err(|e| format!("{e:#}"))
}

/// `wsl --update` for the needs-update wizard state (pre-2.1.1 WSL breaks
/// daemonized processes after sleep/resume).
#[tauri::command]
pub(super) async fn wsl_update() -> Result<(), String> {
    tracing::info!("ipc: wsl_update");
    crate::wsl::launch_wsl_update()
        .await
        .map_err(|e| format!("{e:#}"))
}

/// Kick off the Ubuntu distro install; the wizard polls `wsl_status` until
/// the distro registers (the image download runs minutes).
#[tauri::command]
pub(super) async fn wsl_install_distro() -> Result<(), String> {
    tracing::info!("ipc: wsl_install_distro");
    crate::wsl::launch_distro_install()
        .await
        .map_err(|e| format!("{e:#}"))
}

/// Provision + start + adopt the daemon in `distro` (None = persisted/
/// default), then complete the startup the wizard interrupted and close the
/// wizard window. Emits `wsl-setup` phase events for the wizard's progress
/// line. Concurrency and retries are governed by the shell's startup gate —
/// `try_state::<Shell>()` alone can neither exclude a concurrent invocation
/// (minutes-long await) nor distinguish "done" from "failed after manage".
#[tauri::command]
pub(super) async fn wsl_setup_daemon(app: AppHandle, distro: Option<String>) -> Result<(), String> {
    tracing::info!("ipc: wsl_setup_daemon ({distro:?})");
    let claimed = match super::claim_startup() {
        super::StartupClaim::Claimed => true,
        super::StartupClaim::InFlight => {
            return Err("setup is already running".to_string());
        }
        // Startup already finished — recover any missing home window (a
        // partial finish can leave Shell managed with zero real windows),
        // then just retire the wizard below.
        super::StartupClaim::Done => {
            super::recover_windows(&app);
            false
        }
    };
    if claimed {
        let progress = {
            let app = app.clone();
            move |phase: &str| {
                let _ = app.emit("wsl-setup", phase.to_string());
            }
        };
        let result = async {
            let local = crate::wsl::ensure_daemon(distro, false, &progress)
                .await
                .map_err(|e| format!("{e:#}"))?;
            super::finish_startup(&app, local).map_err(|e| format!("{e:#}"))
        }
        .await;
        super::release_startup(result.is_ok());
        result?;
    }
    for (label, w) in app.webview_windows() {
        if label.starts_with("wsl-setup") {
            let _ = w.close();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::compute_response_body;

    fn response(status: u16, body: &str) -> ureq::http::Response<ureq::Body> {
        ureq::http::Response::builder()
            .status(status)
            .body(ureq::Body::builder().data(body.as_bytes().to_vec()))
            .expect("test response")
    }

    #[test]
    fn compute_response_preserves_daemon_error_message() {
        let error = compute_response_body(
            "could not launch",
            response(422, r#"{"error":"invalid partition debug"}"#),
        )
        .expect_err("422 must fail");

        assert_eq!(error, "could not launch: invalid partition debug");
    }

    #[test]
    fn compute_response_falls_back_to_status_and_reads_success() {
        assert_eq!(
            compute_response_body("could not list", response(503, "unavailable"))
                .expect_err("503 must fail"),
            "could not list: HTTP 503"
        );
        assert_eq!(
            compute_response_body("could not list", response(200, r#"{"sessions":[]}"#))
                .expect("200 response"),
            r#"{"sessions":[]}"#
        );
    }
}
