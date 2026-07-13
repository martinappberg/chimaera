//! Opening UI windows, the live-tunnel health monitor, and reopening the
//! persisted window set at launch. The single health-monitor task is the sole
//! emitter of `host-status`, so windows and the home screen just listen —
//! there is no per-window reconnect stampede.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use super::connect::{do_connect, HostStatus};
use super::{lock, Shell, WindowScope};
use crate::windows::WindowRecord;

static WINDOW_SEQ: AtomicU64 = AtomicU64::new(0);

/// Open a UI window on a daemon: local (`record.alias` None) or a connected
/// remote's tunnel. `record.ws` scopes the window to a workspace; None lands
/// on the home screen. The record's stable id rides the hash as `win=` — the
/// SPA keys its daemon-side view state on it, so reopening this record IS
/// reopening this window — and its geometry (when present: a restore)
/// replaces the default placement.
pub fn open_ui_window(
    app: &AppHandle,
    port: u16,
    token: &str,
    record: &WindowRecord,
) -> tauri::Result<()> {
    let mut hash = format!("token={}", urlencoding::encode(token));
    hash.push_str(&format!("&win={}", urlencoding::encode(&record.id)));
    if let Some(ws) = &record.ws {
        hash.push_str(&format!("&ws={}", urlencoding::encode(ws)));
    }
    if let Some(alias) = &record.alias {
        hash.push_str(&format!("&host={}", urlencoding::encode(alias)));
    }
    let url = format!("http://127.0.0.1:{port}/#{hash}")
        .parse()
        .expect("daemon url is always valid");
    let label = format!("win-{}", WINDOW_SEQ.fetch_add(1, Ordering::Relaxed));
    let title = match &record.alias {
        Some(alias) => format!("{alias} — chimaera"),
        None => "chimaera".to_string(),
    };
    let mut builder = WebviewWindowBuilder::new(app, label.clone(), WebviewUrl::External(url))
        .title(title)
        // Tauri's own drag-drop handler intercepts OS file drops and suppresses
        // the webview's DOM drop events. The workbench handles drops itself in
        // the web UI (upload to the session's owning host, then reference the
        // path) — one code path for browser and native shell, correct for local
        // AND tunneled-remote windows by construction (a bare local path means
        // nothing on a remote host). Disabling it hands HTML5 dnd back to the UI.
        .disable_drag_drop_handler()
        .inner_size(1280.0, 840.0)
        .min_inner_size(680.0, 440.0);
    if let (Some(w), Some(h)) = (record.width, record.height) {
        builder = builder.inner_size(w, h);
    }
    if let (Some(x), Some(y)) = (record.x, record.y) {
        builder = builder.position(x, y);
    }
    builder.build()?;
    // Track the new window's scope so focus-existing can raise it, and
    // persist it so the next launch reopens it. Startup manages Shell before
    // opening any window, so every window registers.
    if let Some(shell) = app.try_state::<Shell>() {
        lock(&shell.windows).insert(
            label,
            WindowScope {
                alias: record.alias.clone(),
                ws: record.ws.clone(),
                stable_id: record.id.clone(),
                // Named by the SPA once it mounts (report_window_scope); until
                // then the tray falls back to "Home"/"Loading…" by scope.
                label: String::new(),
            },
        );
        lock(&shell.registry).upsert(record.clone());
    }
    // A new window changes the tray's open-windows list.
    crate::tray::rebuild(app);
    Ok(())
}

/// Watch live tunnels and broadcast `host-status` on up↔down transitions.
/// Without this a dropped forward (remote daemon or ssh died) goes unnoticed
/// until the user clicks. A single task is the sole emitter — windows and the
/// home screen just listen, so there is no per-window reconnect stampede. The
/// probe is an end-to-end HTTP health check on the loopback port (no extra
/// ssh child): a bare TCP connect would keep reporting "up" after laptop
/// sleep, when ssh's local listener survives its dead connection — the state
/// that made reconnect a silent no-op.
pub(super) fn spawn_health_monitor(handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut prev: HashMap<String, bool> = HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            // No managed Shell means the app is tearing down — stop the loop.
            let Some(shell) = handle.try_state::<Shell>() else {
                break;
            };
            // Snapshot under the lock, then drop it before probing sockets.
            let snap: Vec<(String, u16, String)> = {
                let tunnels = shell.tunnels.lock().await;
                tunnels
                    .iter()
                    .map(|(a, t)| (a.clone(), t.local_port, t.manifest.token.clone()))
                    .collect()
            };
            // Aliases gone from the map were disconnected by the user; forget
            // them without emitting a spurious `down`.
            prev.retain(|a, _| snap.iter().any(|(s, ..)| s == a));
            for (alias, port, token) in &snap {
                let up = chimaera_remote::http_alive(*port).await;
                if prev.insert(alias.clone(), up) == Some(up) {
                    continue; // no transition
                }
                let _ = handle.emit(
                    "host-status",
                    HostStatus {
                        alias: alias.clone(),
                        status: if up { "connected" } else { "down" },
                        local_port: Some(*port),
                        token: up.then(|| token.clone()),
                        error: None,
                    },
                );
            }
        }
    });
}

/// Reopen the persisted window set: local-daemon windows immediately;
/// remote windows as their host tunnels come up (one connect per alias, in
/// the background — an unreachable host must not hold up launch). Returns
/// whether any window opens immediately, so startup can fall back to a home
/// window and never come up invisible.
pub(super) fn restore_windows(handle: &AppHandle, port: u16, token: &str) -> bool {
    let records = {
        let shell = handle.state::<Shell>();
        let records = lock(&shell.registry).list();
        records
    };
    let mut opened = false;
    let mut remote_aliases: Vec<String> = Vec::new();
    for record in &records {
        match &record.alias {
            None => match open_ui_window(handle, port, token, record) {
                Ok(()) => opened = true,
                Err(e) => tracing::warn!("could not reopen window {}: {e}", record.id),
            },
            Some(alias) => {
                if !remote_aliases.contains(alias) {
                    remote_aliases.push(alias.clone());
                }
            }
        }
    }
    for alias in remote_aliases {
        let handle = handle.clone();
        tauri::async_runtime::spawn(async move {
            // Window reopening rides the connect itself (`reopen_windows` in
            // `run_flight`), and the records survive failure: the windows
            // come back with the first connect that lands — a home-screen
            // click, a window's reconnect, or the next launch.
            if let Err(e) = do_connect(&handle, alias.clone(), false).await {
                tracing::warn!("could not reconnect {alias} to restore windows: {e}");
            }
        });
    }
    opened
}
