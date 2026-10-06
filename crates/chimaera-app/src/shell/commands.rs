//! The IPC command surface the daemon-served UI calls
//! (`web-ui/src/lib/native.ts` is the other half of this contract — change
//! command and event names in lockstep). Thin delegators over the connect
//! flight state machine and the window/tunnel state.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use super::connect::{do_connect, state_for, with_hosts, HostState};
use super::restore::open_ui_window;
use super::{authorize_scope_origin, lock, Shell, WindowScope};
use crate::windows::WindowRecord;

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
            // A detached solo window shares its scope with the real workspace
            // window; raising it in the workspace's stead would be wrong.
            exclude != Some(label.as_str())
                && !scope.detached
                && &scope.alias == alias
                && &scope.ws == ws
        })
        .map(|(label, _)| label.clone())
}

/// The label of an open window on `alias`, whatever workspace it shows. The
/// raise for job windows, whose identity IS their composite alias
/// (`"{alias}#job{id}"`): the SPA overwrites the stored `ws` once a workspace
/// opens inside one, so an exact `(alias, ws)` match would miss it and open a
/// duplicate window for the same job on every reconnect.
pub(super) fn find_by_alias(
    windows: &Mutex<HashMap<String, WindowScope>>,
    alias: &str,
) -> Option<String> {
    lock(windows)
        .iter()
        .find(|(_, scope)| !scope.detached && scope.alias.as_deref() == Some(alias))
        .map(|(label, _)| label.clone())
}

/// Only a local page with no workspace can reclaim the singleton Home
/// launcher. Keep this predicate beside the command that takes the Home gate
/// so future scope variants cannot silently bypass its serialization.
fn report_can_reclaim_local_home(alias: &Option<String>, ws: &Option<String>) -> bool {
    alias.is_none() && ws.is_none()
}

#[tauri::command]
pub(super) async fn list_hosts(state: State<'_, Shell>) -> Result<Vec<HostState>, String> {
    tracing::debug!("ipc: list_hosts");
    let hosts = with_hosts(|hosts| Ok(hosts.list())).await?;
    let tunnels = state.tunnels.lock().await;
    let connecting: HashSet<String> = lock(&state.connecting).keys().cloned().collect();
    let unhealthy = lock(&state.unhealthy_tunnels).clone();
    let clusters: HashMap<String, super::cluster::ClusterInfo> = lock(&state.clusters)
        .iter()
        .filter_map(|(alias, c)| c.info.clone().map(|i| (alias.clone(), i)))
        .collect();
    Ok(hosts
        .iter()
        .map(|h| {
            let live = clusters.get(&h.alias);
            let state = if connecting.contains(&h.alias) {
                state_for(h, "connecting", None)
            } else if let Some(t) = tunnels
                .get(&h.alias)
                .filter(|_| !unhealthy.contains(&h.alias))
            {
                state_for(h, "connected", Some(t))
            } else if live.is_some() {
                // This process connected to it as a cluster: nothing to
                // tunnel, the ControlMaster carries every command.
                state_for(h, "cluster", None)
            } else {
                state_for(h, "disconnected", None)
            };
            state.with_cluster(h, live)
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
    let entry = with_hosts(move |hosts| hosts.add(&alias, None)).await?;
    Ok(state_for(&entry, "disconnected", None))
}

#[tauri::command]
pub(super) async fn remove_host(state: State<'_, Shell>, alias: String) -> Result<(), String> {
    let tunnel = state.tunnels.lock().await.remove(&alias);
    lock(&state.unhealthy_tunnels).remove(&alias);
    lock(&state.wedge_suspects).remove(&alias);
    lock(&state.host_entries).remove(&alias);
    if let Some(tunnel) = tunnel {
        tunnel.close().await;
    }
    with_hosts(move |hosts| hosts.remove(&alias)).await?;
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
    lock(&state.unhealthy_tunnels).remove(&alias);
    // A deliberate teardown: the next connect is a first connect, not a
    // suspect's reconnect.
    lock(&state.wedge_suspects).remove(&alias);
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
    lock(&state.unhealthy_tunnels).remove(&alias);
    lock(&state.wedge_suspects).remove(&alias);
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
    // Home is the one exception to explicit `new_window`: it is the app's
    // singleton navigation hub, so every route raises or recreates that same
    // surface instead of multiplying blank launchers.
    if alias.is_none() && ws_id.is_none() {
        return super::show_local_home(&app, None).map_err(|e| format!("could not show Home: {e}"));
    }
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

/// Navigate the unused Home launcher to the local launcher, a connected
/// remote's detail page, or a workspace on either daemon. Browsing keeps the
/// singleton launcher identity; a workspace consumes it and promotes the
/// window into an ordinary workbench.
#[tauri::command]
pub(super) async fn navigate_home(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    alias: Option<String>,
    ws_id: Option<String>,
) -> Result<(), String> {
    let (port, token) = match alias.as_deref() {
        None => {
            let local = lock(&state.local);
            (local.port, local.token.clone())
        }
        Some(alias) => {
            let tunnels = state.tunnels.lock().await;
            let tunnel = tunnels
                .get(alias)
                .ok_or_else(|| format!("{alias} is not connected"))?;
            (tunnel.local_port, tunnel.manifest.token.clone())
        }
    };
    super::navigate_home_hub(&app, &webview, port, &token, alias, ws_id)
        .map_err(|e| format!("could not navigate Home: {e}"))
}

/// Where a detached window opens: the drop point in the CALLER's client
/// coords plus the desired inner size (client px, logical).
#[derive(serde::Deserialize)]
pub(super) struct DetachAt {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// Open a DETACHED window on a pre-seeded view-state id (the calling window
/// PUT the `dt:1` solo layout blob under `win_id` before invoking). The host
/// is the CALLER's registered scope — never a parameter — so a daemon-served
/// page cannot mint a window onto another remote's tunnel.
#[tauri::command]
pub(super) async fn open_detached_window(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    ws_id: String,
    win_id: String,
    at: DetachAt,
) -> Result<(), String> {
    tracing::info!("ipc: open_detached_window ws={ws_id} win={win_id}");
    if win_id.is_empty()
        || win_id.len() > 64
        || !win_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("invalid window id".to_string());
    }
    let scope = state
        .window_scope(webview.label())
        .ok_or_else(|| "this window is not registered".to_string())?;
    // The id must be FRESH: an upsert on an existing record would silently
    // rebind a sibling window's registry identity (sibling ids are learnable
    // via list_scope_windows), and its later close would then drop the
    // shared record.
    if lock(&state.windows).values().any(|s| s.stable_id == win_id)
        || lock(&state.registry).contains(&win_id)
    {
        return Err("that window id is already in use".to_string());
    }
    // A job window's daemon is a walltime-bounded compute tunnel with its own
    // URL vocabulary (job/node hash params) — a detached window opened onto
    // it with plain host wiring would misidentify itself. Refuse for now; the
    // tab stays where it is.
    if lock(&state.registry).is_compute(&scope.stable_id) {
        return Err("detaching from a job window isn't supported yet".to_string());
    }
    let (port, token) = match &scope.alias {
        None => {
            let local = lock(&state.local);
            (local.port, local.token.clone())
        }
        Some(alias) => {
            let tunnels = state.tunnels.lock().await;
            let t = tunnels
                .get(alias)
                .ok_or_else(|| format!("{alias} is not connected"))?;
            (t.local_port, t.manifest.token.clone())
        }
    };
    // Place the new window at the drop point: lift the caller's client coords
    // into screen space, then store as logical px (the registry's unit).
    let mut record = WindowRecord::new(scope.alias.clone(), Some(ws_id));
    record.id = win_id;
    record.detached = true;
    record.width = Some(at.width.max(680.0));
    record.height = Some(at.height.max(440.0));
    if let Some(rect) = super::drag::rect_of(&webview) {
        let (gx, gy) = super::drag::global_of_client(&rect, at.x, at.y);
        let (lx, ly) = super::drag::logical_of_global(&rect, gx, gy);
        record.x = Some(lx);
        record.y = Some(ly);
    }
    // The drop math ran in the INNER frame, but WindowRecord.x/y is OUTER
    // (builder.position and the Moved handler both use the frame origin).
    // Shift by this window's own decoration delta — zero on the macOS
    // overlay, the titlebar height where real decorations exist; exact when
    // both windows share a decoration style.
    if let (Ok(outer), Ok(inner), Ok(scale)) = (
        webview.outer_position(),
        webview.inner_position(),
        webview.scale_factor(),
    ) {
        let o = outer.to_logical::<f64>(scale);
        let i = inner.to_logical::<f64>(scale);
        record.x = record.x.map(|x| x - (i.x - o.x));
        record.y = record.y.map(|y| y - (i.y - o.y));
    }
    super::restore::open_detached_ui_window(&app, port, &token, &record)
        .map_err(|e| format!("could not open window: {e}"))
}

// --- cross-window drag routing ---------------------------------------------
//
// The source window streams its out-of-viewport pointer here; the shell —
// the only party that knows every window's position, scale, and scope —
// hit-tests sibling windows and forwards hover/drop as window-targeted
// `xdrag` events in the TARGET's client coords. Adoption is sender-removes-
// on-ack: the `xdrag-ack` relay is what authorizes the source to drop its
// copies (see crossWindow.ts for the timeout policy).

/// The `xdrag` event a target window receives, all coords in ITS client px.
#[derive(Clone, Serialize)]
struct XdragEvent {
    phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transfer: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<serde_json::Value>,
}

#[derive(Clone, Serialize)]
struct XdragAck {
    transfer: u64,
    ok: bool,
}

/// Sibling windows a drag from `source` may target: same (alias, ws), live,
/// not minimized (a hidden rect would swallow desktop drops). Detached solo
/// windows ARE targets — moving a tab into one is a legitimate merge.
fn target_rects(
    app: &AppHandle,
    state: &Shell,
    source: &str,
    scope: &WindowScope,
) -> Vec<super::drag::WinRect> {
    let labels: Vec<String> = lock(&state.windows)
        .iter()
        .filter(|(label, s)| label.as_str() != source && s.alias == scope.alias && s.ws == scope.ws)
        .map(|(label, _)| label.clone())
        .collect();
    labels
        .iter()
        .filter_map(|label| {
            let win = app.get_webview_window(label)?;
            if win.is_minimized().unwrap_or(false) {
                return None;
            }
            super::drag::rect_of(&win)
        })
        .collect()
}

fn emit_xdrag(app: &AppHandle, label: &str, event: XdragEvent) {
    let _ = app.emit_to(label, "xdrag", event);
}

/// Unlight a hovered target. Every path that ends a drag must send this —
/// including the source window dying (shell.rs's Destroyed arm), which is why
/// it is visible outside this module.
pub(super) fn emit_drag_leave(app: &AppHandle, label: &str) {
    emit_xdrag(
        app,
        label,
        XdragEvent {
            phase: "leave",
            x: None,
            y: None,
            transfer: None,
            payload: None,
        },
    );
}

/// A late rAF `drag_track` must not act after its drag ended: drops and
/// cancels record the ended id here, and any track carrying an id at or
/// below it is ignored (see Shell::done_drags).
fn drag_ended(state: &Shell, label: &str, drag: u64) -> bool {
    lock(&state.done_drags)
        .get(label)
        .is_some_and(|done| drag <= *done)
}

fn mark_drag_done(state: &Shell, label: &str, drag: u64) {
    let mut done = lock(&state.done_drags);
    let entry = done.entry(label.to_string()).or_insert(0);
    *entry = (*entry).max(drag);
}

/// Track an out-of-window drag: hit-test siblings at the pointer, keep the
/// hovered target's highlight current (over/leave events), and tell the
/// source whether anything is under it (its ghost flips "new window" ↔
/// "move into window"). Called rAF-throttled by the source.
#[tauri::command]
pub(super) fn drag_track(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    x: f64,
    y: f64,
    drag: u64,
) -> Result<bool, String> {
    // A cancel/drop for this drag may already have landed — this call was in
    // flight when it did. Acting now would re-light the target it cleared.
    if drag_ended(&state, webview.label(), drag) {
        return Ok(false);
    }
    let scope = state
        .window_scope(webview.label())
        .ok_or_else(|| "this window is not registered".to_string())?;
    let Some(src) = super::drag::rect_of(&webview) else {
        return Ok(false);
    };
    let (gx, gy) = super::drag::global_of_client(&src, x, y);
    let rects = target_rects(&app, &state, webview.label(), &scope);
    let focus = lock(&state.focus_order).clone();
    let hit = super::drag::hit_test(&rects, &focus, gx, gy);
    let hit_label = hit.map(|r| r.label.clone());
    let prev = {
        let mut drags = lock(&state.drags);
        // Re-check under the lock: the cancel may have raced in between the
        // gate above and here. Lock order is drags → done_drags; nothing
        // acquires them reversed while holding either.
        if drag_ended(&state, webview.label(), drag) {
            return Ok(false);
        }
        drags
            .insert(webview.label().to_string(), hit_label.clone())
            .flatten()
    };
    if prev != hit_label {
        if let Some(prev) = prev {
            emit_drag_leave(&app, &prev);
        }
    }
    if let Some(rect) = hit {
        let (cx, cy) = super::drag::client_of_global(rect, gx, gy);
        emit_xdrag(
            &app,
            &rect.label,
            XdragEvent {
                phase: "over",
                x: Some(cx),
                y: Some(cy),
                transfer: None,
                payload: None,
            },
        );
    }
    Ok(hit_label.is_some())
}

/// Route an out-of-window RELEASE. Hit → forward the drop (carrying the
/// SENDER-minted `transfer` id — minted client-side so the sender can arm
/// its ledger BEFORE this call, closing the race where the target's ack
/// beats the invoke's own response) to the target and raise it. No hit →
/// `routed:false`, and the caller opens a detached window instead.
#[derive(Serialize)]
pub(super) struct DropOutcome {
    routed: bool,
}

/// The release point in the SOURCE window's client coords.
#[derive(serde::Deserialize)]
pub(super) struct DropAt {
    x: f64,
    y: f64,
}

#[tauri::command]
pub(super) fn drag_drop(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    at: DropAt,
    drag: u64,
    transfer: u64,
    payload: serde_json::Value,
) -> Result<DropOutcome, String> {
    let scope = state
        .window_scope(webview.label())
        .ok_or_else(|| "this window is not registered".to_string())?;
    let prev = lock(&state.drags).remove(webview.label()).flatten();
    mark_drag_done(&state, webview.label(), drag);
    let Some(src) = super::drag::rect_of(&webview) else {
        // The drag still ended: the hovered sibling must unlight even though
        // this window could not report its own geometry.
        if let Some(prev) = prev {
            emit_drag_leave(&app, &prev);
        }
        return Ok(DropOutcome { routed: false });
    };
    let (gx, gy) = super::drag::global_of_client(&src, at.x, at.y);
    // Re-hit-test at the release point against LIVE windows — the hover
    // target may have closed or moved since the last track.
    let rects = target_rects(&app, &state, webview.label(), &scope);
    let focus = lock(&state.focus_order).clone();
    let hit = super::drag::hit_test(&rects, &focus, gx, gy);
    if let Some(prev) = prev {
        if hit.map(|r| r.label.as_str()) != Some(prev.as_str()) {
            emit_drag_leave(&app, &prev);
        }
    }
    let Some(rect) = hit else {
        return Ok(DropOutcome { routed: false });
    };
    register_transfer(&state, transfer, webview.label(), &rect.label)?;
    let (cx, cy) = super::drag::client_of_global(rect, gx, gy);
    emit_xdrag(
        &app,
        &rect.label,
        XdragEvent {
            phase: "drop",
            x: Some(cx),
            y: Some(cy),
            transfer: Some(transfer),
            payload: Some(payload),
        },
    );
    // The drop lands content in the target: focus follows it.
    if let Some(win) = app.get_webview_window(&rect.label) {
        let _ = win.set_focus();
    }
    Ok(DropOutcome { routed: true })
}

/// Record a sender-minted transfer awaiting the target's ack. Ids are minted
/// from crypto randomness client-side; refusing a duplicate (rather than
/// overwriting) means a colliding or replayed id can never re-route an
/// existing pending move's ack.
fn register_transfer(
    state: &Shell,
    transfer: u64,
    source: &str,
    target: &str,
) -> Result<(), String> {
    let mut transfers = lock(&state.transfers);
    transfers.retain(|_, t| t.started.elapsed() < super::TRANSFER_TTL);
    if transfers.contains_key(&transfer) {
        return Err("that transfer id is already pending".to_string());
    }
    transfers.insert(
        transfer,
        super::Transfer {
            source: source.to_string(),
            target: target.to_string(),
            started: std::time::Instant::now(),
        },
    );
    Ok(())
}

/// The drag ended without a routed drop (Escape, or an in-window drop):
/// clear the hovered target's highlight. Tolerates no drag in flight — the
/// source calls this unconditionally on drag end.
#[tauri::command]
pub(super) fn drag_cancel(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    drag: u64,
) -> Result<(), String> {
    let removed = lock(&state.drags).remove(webview.label());
    mark_drag_done(&state, webview.label(), drag);
    if let Some(Some(target)) = removed {
        emit_drag_leave(&app, &target);
    }
    Ok(())
}

/// The TARGET window's verdict on an adoption, relayed to the source. Only
/// the transfer's recorded target may answer it — another window acking a
/// guessed id must not be able to make a source drop its tabs.
#[tauri::command]
pub(super) fn adopt_ack(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    transfer: u64,
    ok: bool,
) -> Result<(), String> {
    let entry = {
        let mut transfers = lock(&state.transfers);
        // Opportunistic sweep: without it an unanswered transfer between two
        // long-lived windows would sit until the next insert.
        transfers.retain(|_, t| t.started.elapsed() < super::TRANSFER_TTL);
        match transfers.get(&transfer) {
            None => return Ok(()), // pruned or already answered: the source's timeout owns it
            Some(t) if t.target != webview.label() => {
                return Err("that transfer is not addressed to this window".to_string());
            }
            Some(_) => transfers.remove(&transfer).expect("checked present"),
        }
    };
    let _ = app.emit_to(
        entry.source.as_str(),
        "xdrag-ack",
        XdragAck { transfer, ok },
    );
    Ok(())
}

/// A sibling window tabs can move to, for the "Move to window…" menu.
#[derive(Serialize)]
pub(super) struct ScopeWindow {
    win_id: String,
    label: String,
    detached: bool,
}

/// Live windows sharing the caller's (alias, ws), excluding the caller.
#[tauri::command]
pub(super) fn list_scope_windows(
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
) -> Result<Vec<ScopeWindow>, String> {
    let scope = state
        .window_scope(webview.label())
        .ok_or_else(|| "this window is not registered".to_string())?;
    Ok(lock(&state.windows)
        .iter()
        .filter(|(label, s)| {
            label.as_str() != webview.label() && s.alias == scope.alias && s.ws == scope.ws
        })
        .map(|(_, s)| ScopeWindow {
            win_id: s.stable_id.clone(),
            label: s.label.clone(),
            detached: s.detached,
        })
        .collect())
}

/// Menu-path adoption: route `payload` (a serialized solo-layout blob) into
/// the window whose stable id is `target_win_id`, raise it, and mint the
/// transfer the caller awaits — the same drop/ack machinery as a drag,
/// minus coordinates (the target lands the tabs on its focused pane).
#[tauri::command]
pub(super) fn adopt_tab(
    app: AppHandle,
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    target_win_id: String,
    transfer: u64,
    payload: serde_json::Value,
) -> Result<(), String> {
    let scope = state
        .window_scope(webview.label())
        .ok_or_else(|| "this window is not registered".to_string())?;
    let target_label = lock(&state.windows)
        .iter()
        .find(|(label, s)| {
            label.as_str() != webview.label()
                && s.stable_id == target_win_id
                && s.alias == scope.alias
                && s.ws == scope.ws
        })
        .map(|(label, _)| label.clone())
        .ok_or_else(|| "that window is no longer open".to_string())?;
    register_transfer(&state, transfer, webview.label(), &target_label)?;
    emit_xdrag(
        &app,
        &target_label,
        XdragEvent {
            phase: "drop",
            x: None,
            y: None,
            transfer: Some(transfer),
            payload: Some(payload),
        },
    );
    if let Some(win) = app.get_webview_window(&target_label) {
        let _ = win.set_focus();
    }
    Ok(())
}

/// Check GitHub releases for a newer signed app build. Returns the new
/// version string when one is available, `None` when up to date. All
/// updater work runs in Rust; the web UI can only ask, never drive the
/// download — and the download is verified against the embedded minisign
/// pubkey regardless, so only a validly-signed release can ever install.
/// A dev build answers `None` without asking: offering a release to it would
/// swap the build under test (and the daemon it spawns) for a download.
#[tauri::command]
pub(super) async fn check_app_update(app: AppHandle) -> Result<Option<String>, String> {
    // An unreachable endpoint is "no update" here (the home screen's quiet
    // line); `app_update_status` is where a failure is reported as one.
    Ok(crate::update::check(&app).await.available)
}

/// "Is there an app update?" with the whole answer: this version, what the
/// last check found, when, and why it failed if it did. `refresh` checks
/// first (a "check now"); otherwise the cached outcome answers instantly —
/// how a window opened after the periodic `app-update` broadcast learns of it.
#[tauri::command]
pub(super) async fn app_update_status(
    app: AppHandle,
    refresh: bool,
) -> Result<crate::update::AppUpdateStatus, String> {
    Ok(if refresh {
        crate::update::check(&app).await
    } else {
        crate::update::status(&app)
    })
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
    match askpass.answer_scoped(id, secret, &scope) {
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
    Ok(askpass.pending_scoped(&scope))
}

/// Remember the daemon-confirmed first-paint palette outside web storage.
/// The calling window's shell-owned host scope is the cache key, so a remote
/// page cannot overwrite another host's bootstrap by claiming an alias.
#[tauri::command]
pub(super) fn cache_appearance(
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    appearance: crate::appearance::AppearanceBootstrap,
) -> Result<(), String> {
    let scope = state
        .window_scope(webview.label())
        .ok_or_else(|| "this window is not registered".to_string())?;
    if scope.navigation_pending() {
        return Err("this window is still navigating".to_string());
    }
    lock(&state.appearance)
        .set(scope.appearance_alias(), appearance)
        .map_err(|error| format!("could not persist appearance: {error:#}"))
}

/// The SPA reporting what this window now shows — it swaps `ws` client-side,
/// so the shell can't see it otherwise. Keyed by the calling window's label;
/// the persisted record follows so the next launch reopens the window on
/// what it was ACTUALLY showing, not what it was opened on.
#[tauri::command]
pub(super) fn report_window_scope(
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    askpass: State<'_, crate::askpass::Askpass>,
    alias: Option<String>,
    ws: Option<String>,
    label: Option<String>,
    detached: Option<bool>,
) -> Result<(), String> {
    // New Window takes `home_opening` before inspecting `windows`. Match that
    // lock order and hold the gate until this local-empty report has retired
    // any older launcher and marked its claimant. Otherwise New Window can
    // observe the handoff's temporary zero-hub state and create a duplicate.
    let home_reclaim_gate =
        report_can_reclaim_local_home(&alias, &ws).then(|| lock(&state.home_opening));
    let mut windows = lock(&state.windows);
    let current = windows
        .get(webview.label())
        .ok_or_else(|| "this window is not registered".to_string())?;
    // The shell fixed the host when it created/navigated this window. A
    // daemon-served page may report workspace/label changes, but must never
    // rewrite its host to gain another remote's native command scope.
    if current.alias != alias {
        return Err("a window cannot change its registered host".to_string());
    }
    if current.navigation_pending() {
        return Err("this window is still navigating".to_string());
    }
    // A local Home may still be the only eligible surface for password/2FA
    // while an SSH flight is running. Before consuming that launcher, arrange
    // for a fresh Home to inherit fallback duty; otherwise both already-shown
    // and later sequential prompts become unanswerable.
    let local_home_promotion = current.home_hub && current.alias.is_none() && ws.is_some();
    let pending_before_promotion = if local_home_promotion {
        askpass.pending_scoped(current)
    } else {
        Vec::new()
    };
    let promotion_needs_auth_home = local_home_promotion
        && (!lock(&state.connecting).is_empty() || !pending_before_promotion.is_empty());
    let pre_promotion_scope = promotion_needs_auth_home.then(|| current.clone());
    // A promoted local workbench whose workspace disappeared is visibly Home
    // again. If another unused launcher already exists, retire that older
    // surface and let this in-place Home reclaim the singleton identity. The
    // old hub cannot carry editor state, so its close does not cross the
    // workbench's beforeunload guard.
    let existing_home = (current.alias.is_none()
        && ws.is_none()
        && !current.home_hub
        // A detached window reporting an empty scope (its workspace vanished)
        // is NOT reclaiming Home — it must never retire the real launcher.
        && !current.detached)
        .then(|| {
            windows
                .iter()
                .find(|(window_label, scope)| {
                    window_label.as_str() != webview.label() && scope.home_hub
                })
                .map(|(window_label, _)| window_label.clone())
        })
        .flatten();
    if let Some(existing_label) = existing_home {
        let existing_scope = windows
            .get_mut(&existing_label)
            .expect("the Home label came from this scope map");
        let previous_home = existing_scope.clone();
        existing_scope.relinquish_home();
        drop(windows);
        if let Some(existing_window) = webview.app_handle().get_webview_window(&existing_label) {
            if let Err(error) = existing_window.close() {
                lock(&state.windows).insert(existing_label, previous_home);
                return Err(format!("could not close the previous Home: {error}"));
            }
        } else {
            // A Destroyed event normally owns this cleanup. If the webview
            // disappeared just before this report, remove its stale scope and
            // record here so it cannot return on restart.
            lock(&state.windows).remove(&existing_label);
            lock(&state.registry).remove(&previous_home.stable_id);
        }
        windows = lock(&state.windows);
    }

    let scope = windows
        .get_mut(webview.label())
        .ok_or_else(|| "this window is not registered".to_string())?;
    let was_home_hub = scope.home_hub;
    scope.report_page_scope(ws.clone(), label.unwrap_or_default());
    let reclaimed_home = !was_home_hub && scope.home_hub;
    // Two-way, record-backed: a solo window converting to a full workbench
    // (workspace switch) reports false; a torn-off pane reports true. The
    // worst a page can do with either direction is change its own
    // raise-eligibility — never its host or askpass scope (report_page_scope
    // ignores hub promotion while detached).
    let detached_changed = detached.is_some_and(|d| d != scope.detached);
    if let Some(d) = detached {
        scope.detached = d;
    }
    let detached_now = scope.detached;
    let stable_id = scope.stable_id.clone();
    let registered_alias = scope.alias.clone();
    let home_hub = scope.home_hub;
    let reclaimed_scope = reclaimed_home.then(|| scope.clone());
    drop(windows);
    // The singleton identity is now visible atomically to New Window. Do not
    // retain its gate across persistence, prompt replay, or tray rebuilding.
    drop(home_reclaim_gate);
    if let Some(previous_scope) = pre_promotion_scope {
        if let Err(error) = super::show_local_home(webview.app_handle(), None) {
            // Creating the successor failed: restore prompt eligibility on
            // this window rather than stranding an in-flight ssh process.
            lock(&state.windows).insert(webview.label().to_string(), previous_scope);
            return Err(format!(
                "could not keep Home available for SSH authentication: {error}"
            ));
        }
        // The replacement Home re-lists these prompts on mount. Remove their
        // stale copies from the promoted workbench, whose narrowed scope can
        // no longer answer them.
        for prompt in pending_before_promotion {
            if let Err(error) = webview.emit("ssh-askpass-done", prompt.id()) {
                tracing::warn!(
                    %error,
                    window = webview.label(),
                    "could not dismiss transferred SSH prompt"
                );
            }
        }
    }
    // A notification click that opened this window is owed a focus.
    super::notices::window_scoped(webview.app_handle(), webview.label(), &alias, &ws);
    if (!home_hub || reclaimed_home) && !stable_id.is_empty() {
        let mut registry = lock(&state.registry);
        registry.set_scope(&stable_id, registered_alias, ws);
        // Detachedness follows into the record so the NEXT launch reopens
        // the window in the mode it actually ended up in.
        if detached_changed {
            registry.set_detached(&stable_id, detached_now);
        }
    }
    // `AskpassModal` fetched pending prompts when this document mounted, back
    // while a promoted workbench was ineligible. Re-emit every now-authorized
    // prompt after it reclaims Home so closing the previous launcher cannot
    // strand an already-waiting SSH password/2FA request.
    if let Some(scope) = reclaimed_scope {
        for prompt in askpass.pending_scoped(&scope) {
            if let Err(error) = webview.emit("ssh-askpass", prompt) {
                tracing::warn!(%error, window = webview.label(), "could not replay SSH prompt");
            }
        }
    }
    // The reported label names this window in the tray's list; rebuild so it
    // shows the fresh name (the store above happened before this call).
    crate::tray::rebuild(webview.app_handle());
    // A newly scoped window may now enable Settings (any daemon page does).
    crate::menu::sync_settings_enabled(webview.app_handle());
    Ok(())
}

/// What this window shows right now: the session in each pane's active tab.
/// The notifier drops a notice about one of these while the window has focus
/// (a workspace window also covers its hidden tabs; a torn-off one only these),
/// and a focused window's report clears their delivered alerts.
#[tauri::command]
pub(super) fn report_window_view(
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    visible: Vec<String>,
) -> Result<(), String> {
    // A page shows a handful of panes; anything longer is not a view.
    const MAX_VISIBLE: usize = 32;
    let visible: Vec<String> = visible.into_iter().take(MAX_VISIBLE).collect();
    let alias = {
        let mut windows = lock(&state.windows);
        let scope = windows
            .get_mut(webview.label())
            .ok_or_else(|| "this window is not registered".to_string())?;
        scope.visible.clone_from(&visible);
        scope.alias.clone()
    };
    if webview.is_focused().unwrap_or(false) {
        super::notices::mark_seen(webview.app_handle(), &alias, &visible);
    }
    Ok(())
}

/// How many files hold unsaved edits in this window, pushed by the page
/// whenever that changes, so a close or quit decides without asking the page
/// first (see `unsaved`). Keyed by the calling window: a page can only speak
/// for itself.
#[tauri::command]
pub(super) fn report_unsaved(
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    count: u32,
) -> Result<(), String> {
    if state.window_scope(webview.label()).is_none() {
        return Err("this window is not registered".to_string());
    }
    super::unsaved::report(webview.app_handle(), webview.label(), count);
    Ok(())
}

/// This window's answer to an `unsaved-prompt`: `shown` (the dialog is up),
/// `proceed` (saved all, or don't save) or `cancel`. A reply to anything but
/// the window's current prompt is ignored.
#[tauri::command]
pub(super) fn reply_unsaved(webview: tauri::WebviewWindow, id: u64, reply: super::unsaved::Reply) {
    super::unsaved::reply(webview.app_handle(), webview.label(), id, reply);
}

/// The session a notification click opened this window for (see
/// `notices::take_pending_focus`); `None` once taken or when nothing is owed.
#[tauri::command]
pub(super) fn take_pending_focus(webview: tauri::WebviewWindow) -> Option<String> {
    super::notices::take_pending_focus(webview.app_handle(), webview.label())
}

/// Whether the OS lets Chimaera post notifications (for the settings page).
#[tauri::command]
pub(super) async fn notification_permission() -> crate::notify::Permission {
    crate::notify::permission().await
}

/// Ask the OS for notification permission now (the settings page's button;
/// otherwise the first notification asks).
#[tauri::command]
pub(super) async fn request_notification_permission() -> crate::notify::Permission {
    crate::notify::request_permission().await
}

/// Post a sample notification (the settings page's "Send test"), so the user
/// can see what alerts look like — and trigger the OS permission prompt — on
/// demand. Clicking it just brings the app forward.
#[tauri::command]
pub(super) fn test_notification() {
    crate::notify::post(crate::notify::Toast {
        id: format!("chimaera-test-{}", super::next_test_notification_id()),
        thread: "chimaera-test".to_string(),
        title: "Chimaera".to_string(),
        subtitle: "Notifications are on".to_string(),
        body: "You'll hear from agents here when they finish or need you.".to_string(),
        sound: true,
    });
}

/// Open the OS's notification settings for Chimaera (macOS: System Settings
/// → Notifications → Chimaera) — the only place a denial can be undone.
#[tauri::command]
pub(super) fn open_notification_settings() {
    crate::notify::open_settings();
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

/// The path of a file on THIS machine that a window may hand to the OS.
///
/// A daemon-served page names the path, and remote hosts' pages hold the same
/// command grants — so only a window the shell registered on the local daemon
/// may ask (its paths are this machine's; a remote page's would name whatever
/// happens to sit at that path here). On Windows the local daemon lives inside
/// WSL2, so its paths are not host paths either.
fn local_file_path(
    webview: &tauri::WebviewWindow,
    state: &Shell,
    path: &str,
) -> Result<std::path::PathBuf, String> {
    if cfg!(windows) {
        return Err("not available on Windows".to_string());
    }
    let local = state
        .window_scope(webview.label())
        .is_some_and(|scope| scope.alias.is_none() && !scope.navigation_pending());
    if !local {
        return Err("only a window on this machine can do that".to_string());
    }
    existing_absolute_path(path)
}

/// `path` as an absolute path to something that exists (a dangling symlink
/// counts: it is still an entry the file manager can show).
fn existing_absolute_path(path: &str) -> Result<std::path::PathBuf, String> {
    let path = std::path::PathBuf::from(path);
    if !path.is_absolute() {
        return Err("not an absolute path".to_string());
    }
    if std::fs::symlink_metadata(&path).is_err() {
        return Err("no such file".to_string());
    }
    Ok(path)
}

/// Put a file or folder itself on the OS clipboard, the way the platform's
/// file manager does: pasting into Finder, a mail or a chat app attaches it.
/// The in-app Copy only filled Chimaera's own clipboard, so nothing outside
/// the workbench could paste what the user had just copied.
#[tauri::command]
pub(super) async fn copy_file_to_clipboard(
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    path: String,
) -> Result<(), String> {
    let path = local_file_path(&webview, &state, &path)?;
    // X11 and Wayland serve a selection from the process that owns it, so the
    // clipboard that set it must outlive this call.
    static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);
    tauri::async_runtime::spawn_blocking(move || {
        let mut slot = lock(&CLIPBOARD);
        if slot.is_none() {
            *slot = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
        }
        let clipboard = slot.as_mut().expect("just filled");
        clipboard
            .set()
            .file_list(&[path])
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Show a file or folder in the platform's file manager, selected in its
/// parent folder (macOS Finder; a Linux file manager that speaks the
/// freedesktop `FileManager1` interface, else the parent folder just opens).
#[tauri::command]
pub(super) async fn reveal_in_file_manager(
    webview: tauri::WebviewWindow,
    state: State<'_, Shell>,
    path: String,
) -> Result<(), String> {
    let path = local_file_path(&webview, &state, &path)?;
    tauri::async_runtime::spawn_blocking(move || reveal(&path))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
fn reveal(path: &std::path::Path) -> Result<(), String> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg("--")
        .arg(path)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("Finder could not show that file".to_string())
    }
}

#[cfg(not(target_os = "macos"))]
fn reveal(path: &std::path::Path) -> Result<(), String> {
    let shown = tauri::Url::from_file_path(path).is_ok_and(|url| {
        std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
            ])
            // dbus-send splits an array on commas, and a file URL keeps a
            // name's comma literal: escaped, the name stays one item.
            .arg(format!("array:string:{}", url.as_str().replace(',', "%2C")))
            .arg("string:")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    if shown {
        return Ok(());
    }
    // No file manager answered: opening the folder still gets the user there.
    let parent = path.parent().unwrap_or(path);
    open::that_detached(parent).map_err(|e| e.to_string())
}

/// Hand a web URL to the user's real browser.
///
/// The shell's navigation guard admits only the exact daemon origin, and a
/// `target="_blank"` new-window request has nothing wired to receive it — so
/// in the app an external link was simply swallowed (found live). Every
/// rendered link surface (chat prose, markdown previews, the browser pane's
/// "open externally") routes here instead.
///
/// **Only http/https.** The href is attacker-influenced — agents author chat
/// prose and markdown — and `open::that` hands its argument to the platform
/// opener, which would happily act on `file:`, a `.desktop`, or an
/// application scheme. Anything but a well-formed web URL is refused here,
/// where the rule is enforced once for every caller.
#[tauri::command]
pub(super) fn open_external(url: String) -> Result<(), String> {
    // The href is agent-authored; cap it before it becomes a process argument
    // (an oversized argv fails with E2BIG deep in the platform opener).
    const MAX_URL_BYTES: usize = 8 * 1024;
    if url.len() > MAX_URL_BYTES {
        return Err("URL is too long to open".to_string());
    }
    let parsed = tauri::Url::parse(&url).map_err(|_| "not a URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("refusing to open a {} URL", parsed.scheme()));
    }
    tracing::info!(
        host = parsed.host_str().unwrap_or("?"),
        "ipc: open_external"
    );
    // Pass the REPARSED url: normalization strips anything the platform
    // opener might otherwise interpret (stray whitespace, control bytes).
    open::that_detached(parsed.as_str()).map_err(|e| e.to_string())
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
    // The relaunch at the end is a restart, which Tauri will not let the
    // unsaved-edits guard hold, so refuse up front instead of dropping them.
    match super::unsaved::windows_with_unsaved(&app) {
        0 => {}
        1 => return Err("A window has unsaved edits — save or discard them, then update.".into()),
        n => {
            return Err(format!(
                "{n} windows have unsaved edits — save or discard them, then update."
            ))
        }
    }
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
    use super::{existing_absolute_path, report_can_reclaim_local_home};

    #[test]
    fn a_file_handed_to_the_os_must_be_an_existing_absolute_path() {
        let dir = std::env::temp_dir();
        assert_eq!(
            existing_absolute_path(dir.to_str().unwrap()),
            Ok(dir.clone())
        );
        assert!(existing_absolute_path("relative/file.png").is_err());
        assert!(
            existing_absolute_path(dir.join("chimaera-no-such-file").to_str().unwrap()).is_err()
        );
    }

    #[test]
    fn only_local_empty_reports_enter_the_home_reclamation_gate() {
        assert!(report_can_reclaim_local_home(&None, &None));
        assert!(!report_can_reclaim_local_home(
            &None,
            &Some("workspace".into())
        ));
        assert!(!report_can_reclaim_local_home(
            &Some("cluster".into()),
            &None
        ));
        assert!(!report_can_reclaim_local_home(
            &Some("cluster".into()),
            &Some("workspace".into())
        ));
    }
}
