//! Opening UI windows, the live-tunnel health monitor, and reopening the
//! persisted window set at launch. The single health-monitor task is the sole
//! emitter of `host-status`, so windows and the home screen just listen —
//! there is no per-window reconnect stampede.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use super::connect::{do_connect, HostStatus};
use super::{authorize_daemon_origin, daemon_navigation_allowed, lock, Shell, WindowScope};
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
    let scope = WindowScope::new(record.alias.clone(), record.ws.clone(), record.id.clone());
    open_shell_window(app, &url, &title, record, scope)
}

/// Open a window on a compute-node daemon (Mode 2). Same shell wiring as
/// `open_ui_window`, but the URL is the ComputeTunnel's own (token + host +
/// job + node already ride its hash; only `win=` is added here) and the
/// tracked scope alias is the composite `"{alias}#job{id}"` so focus-existing
/// never confuses a job window with the login host's. Its askpass identity is
/// recorded separately as the login alias; it is never inferred by parsing
/// that composite key.
pub(super) fn open_compute_window(
    app: &AppHandle,
    url: &str,
    title: &str,
    record: &WindowRecord,
    scope_alias: &str,
) -> tauri::Result<()> {
    let url = format!("{url}&win={}", urlencoding::encode(&record.id));
    let login_alias = record
        .alias
        .clone()
        .expect("compute windows always carry their login alias");
    let scope = WindowScope::new_compute(
        scope_alias.to_string(),
        login_alias,
        record.ws.clone(),
        record.id.clone(),
    );
    open_shell_window(app, &url, title, record, scope)
}

fn open_shell_window(
    app: &AppHandle,
    url: &str,
    title: &str,
    record: &WindowRecord,
    window_scope: WindowScope,
) -> tauri::Result<()> {
    let url: tauri::Url = url.parse().expect("daemon url is always valid");
    let port = url
        .port()
        .expect("daemon URLs always carry an explicit loopback port");
    let label = format!("win-{}", WINDOW_SEQ.fetch_add(1, Ordering::Relaxed));
    authorize_daemon_origin(app, &label, port)?;
    let navigation_app = app.clone();
    let navigation_label = label.clone();
    let mut builder = WebviewWindowBuilder::new(app, label.clone(), WebviewUrl::External(url))
        .on_navigation(move |url| {
            daemon_navigation_allowed(&navigation_app, &navigation_label, url)
        })
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
    // The workbench already carries its workspace identity in the rail and
    // pane tabs, so a separate title row only repeats context and steals
    // vertical room. On macOS keep the native traffic lights, but let them
    // overlay the webview and keep the dynamic title as OS metadata only.
    // App.svelte provides the custom drag region Overlay requires.
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    if let (Some(w), Some(h)) = (record.width, record.height) {
        builder = builder.inner_size(w, h);
    }
    if let (Some(x), Some(y)) = (record.x, record.y) {
        builder = builder.position(x, y);
    }
    // Register the immutable host scope before the webview can execute a
    // command. `build()` starts navigation, so inserting afterward leaves a
    // short startup race where `list_askpass` rejects a legitimate window and
    // the only visible authentication prompt is missed.
    if let Some(shell) = app.try_state::<Shell>() {
        // The scope already carries both the window identity and its separate
        // immutable askpass authorization. Register it before navigation can
        // execute a native command.
        lock(&shell.windows).insert(label.clone(), window_scope);
    }
    if let Err(error) = builder.build() {
        if let Some(shell) = app.try_state::<Shell>() {
            lock(&shell.allowed_daemon_ports).remove(&label);
            lock(&shell.windows).remove(&label);
        }
        return Err(error);
    }
    // Persist the new window so the next launch reopens it. Startup manages
    // Shell before opening any window, so every daemon window registered
    // above has an authoritative scope before its first native command.
    if let Some(shell) = app.try_state::<Shell>() {
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
            // login daemon — found live). Their `token` field stays None on
            // the wire. Every probe is authed with its tunnel's own token:
            // identity, not just liveness, because a stale relay or unrelated
            // loopback server can answer on a recycled port.
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
                let up = chimaera_remote::http_alive_authed(*port, token).await;
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
                        reason: (!up).then(|| {
                            "The SSH tunnel or remote daemon stopped answering health checks."
                                .to_string()
                        }),
                        build: None,
                    },
                );
            }
        }
    });
}

/// Reopen the persisted window set: local-daemon windows immediately;
/// remote windows as their host tunnels come up (one connect per alias, in
/// the background — an unreachable host must not hold up launch). A local
/// home window is registered before those connects start whenever no restored
/// home can receive their startup askpass prompts.
pub(super) fn restore_windows(handle: &AppHandle, port: u16, token: &str) -> tauri::Result<()> {
    let records = {
        let shell = handle.state::<Shell>();
        let records = lock(&shell.registry).list();
        records
    };
    let mut opened = false;
    let mut home_opened = false;
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
                Ok(()) => {
                    opened = true;
                    home_opened |= record.ws.is_none();
                }
                Err(e) => tracing::warn!("could not reopen window {}: {e}", record.id),
            },
            Some(alias) => {
                if !remote_aliases.contains(alias) {
                    remote_aliases.push(alias.clone());
                }
            }
        }
    }

    // Register the safe cross-host askpass surface before spawning any ssh.
    // A local workspace is already visible, but deliberately cannot observe
    // another host's prompt; treating it as the startup fallback strands a
    // password/2FA connect until the 180-second askpass timeout.
    if needs_startup_home(opened, home_opened, !remote_aliases.is_empty()) {
        open_ui_window(handle, port, token, &WindowRecord::new(None, None))?;
        tracing::info!("startup home window open on 127.0.0.1:{port}");
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
    Ok(())
}

fn needs_startup_home(opened: bool, home_opened: bool, has_remote: bool) -> bool {
    !opened || (has_remote && !home_opened)
}

#[cfg(test)]
mod tests {
    use super::needs_startup_home;

    #[test]
    fn startup_home_precedes_remote_auth_when_no_home_was_restored() {
        assert!(needs_startup_home(false, false, false));
        assert!(!needs_startup_home(true, false, false));
        assert!(needs_startup_home(true, false, true));
        assert!(!needs_startup_home(true, true, true));
    }
}
