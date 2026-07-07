//! Shell state and the IPC command surface the daemon-served UI calls
//! (`web-ui/src/lib/native.ts` is the other half of this contract — change
//! command and event names in lockstep).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use chimaera_remote::hosts::HostsStore;
use chimaera_remote::{ConnectOpts, Phase, Tunnel};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::daemon::LocalDaemon;

/// App-global shell state.
pub struct Shell {
    /// The local daemon (mutable: the update affordance replaces it).
    pub local: Mutex<LocalDaemon>,
    /// Live tunnels by host alias.
    tunnels: tokio::sync::Mutex<HashMap<String, Tunnel>>,
    /// Aliases with a connect in flight (guards double-clicks).
    connecting: Mutex<HashSet<String>>,
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
}

#[derive(Clone, Serialize)]
struct ConnectProgress {
    alias: String,
    phase: &'static str,
}

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

static WINDOW_SEQ: AtomicU64 = AtomicU64::new(0);

/// Open a UI window on a daemon: local (`alias` None) or a connected
/// remote's tunnel. `ws` scopes the window to a workspace; None lands on
/// the home screen.
pub fn open_ui_window(
    app: &AppHandle,
    port: u16,
    token: &str,
    host_alias: Option<&str>,
    ws: Option<&str>,
) -> tauri::Result<()> {
    let mut hash = format!("token={}", urlencoding::encode(token));
    if let Some(ws) = ws {
        hash.push_str(&format!("&ws={}", urlencoding::encode(ws)));
    }
    if let Some(alias) = host_alias {
        hash.push_str(&format!("&host={}", urlencoding::encode(alias)));
    }
    let url = format!("http://127.0.0.1:{port}/#{hash}")
        .parse()
        .expect("daemon url is always valid");
    let label = format!("win-{}", WINDOW_SEQ.fetch_add(1, Ordering::Relaxed));
    let title = match host_alias {
        Some(alias) => format!("{alias} — chimaera"),
        None => "chimaera".to_string(),
    };
    WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title(title)
        .inner_size(1280.0, 840.0)
        .min_inner_size(680.0, 440.0)
        .build()?;
    Ok(())
}

fn state_for(
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

#[tauri::command]
async fn list_hosts(state: State<'_, Shell>) -> Result<Vec<HostState>, String> {
    tracing::debug!("ipc: list_hosts");
    let store = HostsStore::load_default();
    let tunnels = state.tunnels.lock().await;
    let connecting = crate::shell::lock(&state.connecting).clone();
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
async fn add_host(alias: String) -> Result<HostState, String> {
    let alias = alias.trim().to_string();
    if alias.is_empty() || alias.starts_with('-') {
        return Err("that does not look like an ssh alias".to_string());
    }
    let mut store = HostsStore::load_default();
    let entry = store.add(&alias, None).map_err(|e| format!("{e:#}"))?;
    Ok(state_for(&entry, "disconnected", None))
}

#[tauri::command]
async fn remove_host(state: State<'_, Shell>, alias: String) -> Result<(), String> {
    if let Some(tunnel) = state.tunnels.lock().await.remove(&alias) {
        tunnel.close().await;
    }
    HostsStore::load_default()
        .remove(&alias)
        .map_err(|e| format!("{e:#}"))?;
    Ok(())
}

#[tauri::command]
async fn connect_host(
    app: AppHandle,
    state: State<'_, Shell>,
    alias: String,
    update_daemon: Option<bool>,
) -> Result<HostState, String> {
    let update_daemon = update_daemon.unwrap_or(false);
    tracing::info!("ipc: connect_host {alias} (update_daemon: {update_daemon})");
    // Reuse a live tunnel; a dead one is torn down and rebuilt. An update
    // request never reuses: the old tunnel points at the daemon being
    // replaced, so it comes down before the connect flow runs.
    {
        let mut tunnels = state.tunnels.lock().await;
        if let Some(t) = tunnels.get(&alias) {
            if !update_daemon && t.is_up().await {
                let entry = host_entry(&alias);
                return Ok(state_for(&entry, "connected", Some(t)));
            }
            if let Some(old) = tunnels.remove(&alias) {
                old.close().await;
            }
        }
    }
    if !lock(&state.connecting).insert(alias.clone()) {
        return Err("a connection attempt is already running".to_string());
    }

    let entry = HostsStore::load_default()
        .add(&alias, None)
        .map_err(|e| format!("{e:#}"))?;
    let opts = ConnectOpts {
        local_port: None,
        binary: entry.binary.clone(),
        update_daemon,
    };
    let progress_app = app.clone();
    let progress_alias = alias.clone();
    let result = chimaera_remote::connect(&alias, opts, move |phase| {
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
    .await;
    lock(&state.connecting).remove(&alias);

    let tunnel = result.map_err(|e| format!("{e:#}"))?;
    if let Err(e) = HostsStore::load_default().record_connected(&alias) {
        tracing::debug!("could not record host {alias}: {e}");
    }
    let entry = host_entry(&alias);
    let host_state = state_for(&entry, "connected", Some(&tunnel));
    state.tunnels.lock().await.insert(alias.clone(), tunnel);
    Ok(host_state)
}

#[tauri::command]
async fn disconnect_host(state: State<'_, Shell>, alias: String) -> Result<(), String> {
    if let Some(tunnel) = state.tunnels.lock().await.remove(&alias) {
        tunnel.close().await;
    }
    Ok(())
}

/// The local daemon's build parity (home screen: quiet update note).
#[tauri::command]
async fn local_state(state: State<'_, Shell>) -> Result<LocalState, String> {
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
async fn update_local_daemon(app: AppHandle, state: State<'_, Shell>) -> Result<(), String> {
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
async fn remote_workspaces(
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

/// Open a new window on the local daemon (`alias` None) or a connected
/// remote. `ws_id` None lands on the home screen.
#[tauri::command]
async fn open_window(
    app: AppHandle,
    state: State<'_, Shell>,
    alias: Option<String>,
    ws_id: Option<String>,
) -> Result<(), String> {
    tracing::info!("ipc: open_window alias={alias:?} ws={ws_id:?}");
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
    open_ui_window(&app, port, &token, host.as_deref(), ws_id.as_deref())
        .map_err(|e| format!("could not open window: {e}"))
}

/// Check GitHub releases for a newer signed app build. Returns the new
/// version string when one is available, `None` when up to date. All
/// updater work runs in Rust; the web UI can only ask, never drive the
/// download — and the download is verified against the embedded minisign
/// pubkey regardless, so only a validly-signed release can ever install.
#[tauri::command]
async fn check_app_update(app: AppHandle) -> Result<Option<String>, String> {
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
async fn install_app_update(app: AppHandle) -> Result<(), String> {
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
/// the user cancelled, which lets the waiting ssh fail cleanly.
#[tauri::command]
async fn answer_askpass(
    askpass: State<'_, crate::askpass::Askpass>,
    id: u64,
    secret: Option<String>,
) -> Result<(), String> {
    askpass.answer(id, secret);
    Ok(())
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

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Build and run the Tauri app.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            list_hosts,
            add_host,
            remove_host,
            connect_host,
            disconnect_host,
            local_state,
            update_local_daemon,
            remote_workspaces,
            open_window,
            check_app_update,
            install_app_update,
            answer_askpass,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            crate::menu::install(app)?;
            // Route ssh auth prompts (password / 2FA) to the UI. Managed
            // before the listener starts so an early prompt finds the state.
            app.manage(crate::askpass::Askpass::default());
            if let Err(e) = crate::askpass::install(&handle) {
                // Non-fatal: hosts with key/agent auth still connect; only
                // password/2FA hosts lose the in-app prompt.
                tracing::warn!("ssh askpass unavailable: {e:#}");
            }
            // The daemon must be up before the first window points at it;
            // block setup on it (fast when a daemon is already running).
            let local = tauri::async_runtime::block_on(crate::daemon::ensure_local_daemon())
                .map_err(|e| {
                    tracing::error!("{e:#}");
                    std::io::Error::other(format!("{e:#}"))
                })?;
            open_ui_window(&handle, local.port, &local.token, None, None)?;
            tracing::info!("home window open on 127.0.0.1:{}", local.port);
            app.manage(Shell {
                local: Mutex::new(local),
                tunnels: tokio::sync::Mutex::new(HashMap::new()),
                connecting: Mutex::new(HashSet::new()),
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building chimaera")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Kill tunnel children so forwarded ports don't leak past
                // the app (the daemons keep running by design).
                if let Some(state) = app.try_state::<Shell>() {
                    tauri::async_runtime::block_on(async {
                        let mut tunnels = state.tunnels.lock().await;
                        for (_, tunnel) in tunnels.drain() {
                            tunnel.close().await;
                        }
                    });
                }
            }
        });
}
