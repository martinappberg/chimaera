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
    let url = format!("http://127.0.0.1:{port}/#{hash}");
    let title = match &record.alias {
        Some(alias) => format!("{alias} — chimaera"),
        None => "chimaera".to_string(),
    };
    open_shell_window(app, &url, &title, record, record.alias.clone())
}

/// Open a window on a compute-node daemon (Mode 2). Same shell wiring as
/// `open_ui_window`, but the URL is the ComputeTunnel's own (token + host +
/// job + node already ride its hash; only `win=` is added here) and the
/// tracked scope alias is the composite `"{alias}#job{id}"` so focus-existing
/// never confuses a job window with the login host's.
pub(super) fn open_compute_window(
    app: &AppHandle,
    url: &str,
    title: &str,
    record: &WindowRecord,
    scope_alias: &str,
) -> tauri::Result<()> {
    let url = format!("{url}&win={}", urlencoding::encode(&record.id));
    open_shell_window(app, &url, title, record, Some(scope_alias.to_string()))
}

fn open_shell_window(
    app: &AppHandle,
    url: &str,
    title: &str,
    record: &WindowRecord,
    scope_alias: Option<String>,
) -> tauri::Result<()> {
    let url = url.parse().expect("daemon url is always valid");
    let label = format!("win-{}", WINDOW_SEQ.fetch_add(1, Ordering::Relaxed));
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
                alias: scope_alias,
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
            // Snapshot under the locks, then drop them before probing
            // sockets. Compute tunnels ride the same loop under their
            // composite key ("alias#job{id}") — a job window listens on
            // that key, NEVER on the login alias (listening on the alias
            // made every login-tunnel blip re-home job windows onto the
            // login daemon — found live). Their `token` field stays None
            // on the wire: compute tokens never leave Rust, and a probe
            // is authed with the tunnel's own token instead (identity,
            // not just liveness — a stale relay can answer for the port).
            let mut snap: Vec<(String, u16, String, bool)> = {
                let tunnels = shell.tunnels.lock().await;
                tunnels
                    .iter()
                    .map(|(a, t)| (a.clone(), t.local_port, t.manifest.token.clone(), false))
                    .collect()
            };
            {
                let compute = shell.compute_tunnels.lock().await;
                snap.extend(
                    compute
                        .iter()
                        .map(|(k, t)| (k.clone(), t.local_port, t.token.clone(), true)),
                );
            }
            // Keys gone from the maps were disconnected by the user; forget
            // them without emitting a spurious `down`.
            prev.retain(|a, _| snap.iter().any(|(s, ..)| s == a));
            for (key, port, token, is_compute) in &snap {
                let up = if *is_compute {
                    chimaera_remote::http_alive_authed(*port, token).await
                } else {
                    chimaera_remote::http_alive(*port).await
                };
                if prev.insert(key.clone(), up) == Some(up) {
                    continue; // no transition
                }
                let _ = handle.emit(
                    "host-status",
                    HostStatus {
                        alias: key.clone(),
                        status: if up { "connected" } else { "down" },
                        local_port: Some(*port),
                        token: (up && !*is_compute).then(|| token.clone()),
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
        // A compute window was a view onto a walltime-bounded job tunnel that
        // did not survive the restart — walltime death is honest, and the
        // home-screen card is the reconnect path. Purge the record so
        // windows.json doesn't accumulate dead jobs across quits.
        if record.compute.is_some() {
            let shell = handle.state::<Shell>();
            lock(&shell.registry).remove(&record.id);
            continue;
        }
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
