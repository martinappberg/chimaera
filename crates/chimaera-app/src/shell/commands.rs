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

use super::connect::{do_connect, state_for, HostState};
use super::restore::open_ui_window;
use super::{lock, Shell, WindowScope};
use crate::windows::WindowRecord;

/// The local daemon's build parity, as the home screen sees it.
#[derive(Clone, Serialize)]
pub struct LocalState {
    outdated: bool,
    build: Option<String>,
    live_sessions: Option<usize>,
}

/// Payload of the `local-daemon-updated` broadcast: every window on the
/// local daemon re-homes itself to the new port + token.
#[derive(Clone, Serialize)]
struct LocalDaemonMoved {
    port: u16,
    token: String,
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
    if let Some(tunnel) = state.tunnels.lock().await.remove(&alias) {
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
    do_connect(&app, alias, update_daemon.unwrap_or(false)).await
}

#[tauri::command]
pub(super) async fn disconnect_host(state: State<'_, Shell>, alias: String) -> Result<(), String> {
    if let Some(tunnel) = state.tunnels.lock().await.remove(&alias) {
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
        ureq::delete(&format!("http://127.0.0.1:{port}/api/v1/sessions"))
            .set("Authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(15))
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
        ureq::post(&format!("http://127.0.0.1:{port}/api/v1/shutdown"))
            .set("Authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(15))
            .call()
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?;
    sent.map_err(|e| format!("could not shut down {alias}: {e}"))?;
    // The daemon is on its way out; cancel our forward so the host reads as
    // down instead of lingering on a socket that's about to close.
    if let Some(tunnel) = state.tunnels.lock().await.remove(&alias) {
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
    };
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
        let body = ureq::get(&format!("http://127.0.0.1:{port}/api/v1/workspaces"))
            .set("Authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(10))
            .call()
            .map_err(|e| format!("could not list workspaces: {e}"))?
            .into_string()
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

/// Download, verify, and install the pending app update, then relaunch into
/// it. Diverges on success (the process restarts); returns `Err` only when
/// the download or signature check fails.
#[tauri::command]
pub(super) async fn install_app_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no update available".to_string())?;
    tracing::info!("installing app update {}", update.version);
    update
        .download_and_install(|_downloaded, _total| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    // Bundle swapped and verified; relaunch into the new build (diverges).
    app.restart();
}

/// Answer an in-flight SSH auth prompt (see `askpass`): `secret` None means
/// the user cancelled, which lets the waiting ssh fail cleanly. The done
/// broadcast dismisses the prompt in every other window showing it.
#[tauri::command]
pub(super) async fn answer_askpass(
    app: AppHandle,
    askpass: State<'_, crate::askpass::Askpass>,
    id: u64,
    secret: Option<String>,
) -> Result<(), String> {
    if askpass.answer(id, secret) {
        let _ = app.emit("ssh-askpass-done", id);
    }
    Ok(())
}

/// SSH prompts still awaiting an answer. Each window fetches this on mount:
/// the `ssh-askpass` emit reaches only windows that already exist, and
/// startup window restore starts connecting before the first webview has
/// loaded — without this, that prompt is lost and the host sits in
/// "connecting" until ssh times out, with nothing for the user to answer.
#[tauri::command]
pub(super) fn list_askpass(
    askpass: State<'_, crate::askpass::Askpass>,
) -> Vec<crate::askpass::PromptEvent> {
    askpass.pending()
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
) {
    let mut windows = lock(&state.windows);
    let stable_id = windows
        .get(webview.label())
        .map(|s| s.stable_id.clone())
        .unwrap_or_default();
    windows.insert(
        webview.label().to_string(),
        WindowScope {
            alias: alias.clone(),
            ws: ws.clone(),
            stable_id: stable_id.clone(),
        },
    );
    drop(windows);
    if !stable_id.is_empty() {
        lock(&state.registry).set_scope(&stable_id, alias, ws);
    }
}

/// This app binary's build id, for daemon-skew detection in the UI (the
/// daemon's own build rides GET /api/v1/health).
#[tauri::command]
pub(super) fn shell_build() -> String {
    chimaera_core::BUILD_ID.to_string()
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
