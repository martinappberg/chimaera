//! The native shell: app-global state plus the Tauri `Builder` that wires it
//! all together. The module is split into three seams behind this root:
//!
//! - [`commands`] — the IPC command surface the daemon-served UI calls
//!   (`web-ui/src/lib/native.ts` is the other half; change command and event
//!   names in lockstep).
//! - [`connect`] — the connect-flight state machine (one coalesced ssh attempt
//!   per host) and the host-row wire vocabulary.
//! - [`restore`] — opening UI windows, the live-tunnel health monitor, and
//!   reopening the persisted window set at launch.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use chimaera_remote::Tunnel;
use tauri::Manager;

use crate::daemon::LocalDaemon;
use crate::windows::{WindowRecord, WindowRegistry};

mod commands;
mod connect;
mod restore;

pub use restore::open_ui_window;

/// A connect attempt's shared outcome: `None` while in flight, then the
/// result. Joiners wait on this instead of racing their own ssh — see
/// `do_connect`.
type ConnectFlight = tokio::sync::watch::Receiver<Option<Result<(), String>>>;

/// App-global shell state.
pub struct Shell {
    /// The local daemon (mutable: the update affordance replaces it).
    pub local: Mutex<LocalDaemon>,
    /// Live tunnels by host alias.
    tunnels: tokio::sync::Mutex<HashMap<String, Tunnel>>,
    /// In-flight connect per alias. One flight owns the ssh; every other
    /// caller (other windows' reconnects, the home screen, startup restore)
    /// awaits its outcome — so a drop never fans out into an auth-prompt
    /// stampede, and a click during a stuck attempt joins it instead of
    /// bouncing with an error.
    connecting: Mutex<HashMap<String, ConnectFlight>>,
    /// What each open window currently shows, keyed by window label. Drives
    /// focus-existing (open the same workspace → raise its window instead of a
    /// duplicate). The SPA reports its own scope because it swaps `ws`
    /// client-side without a shell round-trip.
    windows: Mutex<HashMap<String, WindowScope>>,
    /// The persisted window set (windows.json) — what the next launch reopens.
    registry: Mutex<WindowRegistry>,
    /// Set on ExitRequested: window teardown during quit must NOT remove
    /// records from the registry, or quitting would forget every window.
    quitting: AtomicBool,
}

/// The (host, workspace) a window is showing. `alias` None = the local
/// daemon; `ws` None = the home screen. `stable_id` is the window's
/// registry/view-state identity — unlike the volatile Tauri label, it
/// survives app restarts.
#[derive(Clone, Default, PartialEq)]
pub struct WindowScope {
    pub alias: Option<String>,
    pub ws: Option<String>,
    pub stable_id: String,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Complete startup once a local daemon exists: manage `Shell` (BEFORE any
/// window opens, so each registers its scope), reopen the persisted window
/// set or the home window, and start the monitors. Two callers: `setup()` on
/// the normal path, and the WSL wizard's finishing command on a Windows
/// first run — where setup() deliberately returned early with no daemon.
pub(crate) fn finish_startup(handle: &tauri::AppHandle, local: LocalDaemon) -> tauri::Result<()> {
    let (port, token) = (local.port, local.token.clone());
    handle.manage(Shell {
        local: Mutex::new(local),
        tunnels: tokio::sync::Mutex::new(HashMap::new()),
        connecting: Mutex::new(HashMap::new()),
        windows: Mutex::new(HashMap::new()),
        registry: Mutex::new(WindowRegistry::load_default()),
        quitting: AtomicBool::new(false),
    });
    // Reopen last session's windows; first launch (or an all-remote set that
    // is still connecting) gets the home window so the app never comes up
    // invisible.
    if !restore::restore_windows(handle, port, &token) {
        open_ui_window(handle, port, &token, &WindowRecord::new(None, None))?;
        tracing::info!("home window open on 127.0.0.1:{port}");
    }
    restore::spawn_health_monitor(handle.clone());
    crate::update::spawn_update_watch(handle.clone());
    // Slow flush for the geometry dirty flag (window drags).
    let flush = handle.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let Some(shell) = flush.try_state::<Shell>() else {
                break;
            };
            lock(&shell.registry).save_if_dirty();
        }
    });
    Ok(())
}

/// The WSL first-run wizard window: shell-local assets (`setup.html`), not
/// the daemon origin — on a fresh Windows machine no daemon exists to serve
/// the UI yet.
fn open_setup_window(handle: &tauri::AppHandle) -> tauri::Result<()> {
    tauri::WebviewWindowBuilder::new(
        handle,
        "wsl-setup",
        tauri::WebviewUrl::App("setup.html".into()),
    )
    .title("chimaera setup")
    .inner_size(780.0, 640.0)
    .min_inner_size(600.0, 520.0)
    .build()?;
    Ok(())
}

/// Build and run the Tauri app.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            commands::list_hosts,
            commands::add_host,
            commands::remove_host,
            commands::connect_host,
            commands::disconnect_host,
            commands::end_host_sessions,
            commands::shutdown_host,
            commands::local_state,
            commands::update_local_daemon,
            commands::remote_workspaces,
            commands::open_window,
            commands::check_app_update,
            commands::begin_update,
            commands::shell_build,
            commands::write_clipboard,
            commands::answer_askpass,
            commands::list_askpass,
            commands::report_window_scope,
            commands::wsl_status,
            commands::wsl_install,
            commands::wsl_install_distro,
            commands::wsl_setup_daemon,
        ])
        .on_window_event(|window, event| {
            let Some(shell) = window.try_state::<Shell>() else {
                return;
            };
            match event {
                // Forget a window's scope once it's gone, so focus-existing
                // never raises a dead label. Destroyed (not CloseRequested,
                // which can be vetoed) fires after teardown completes. The
                // persisted record goes too — a deliberately closed window
                // stays closed (macOS convention) — EXCEPT during quit, when
                // teardown destroys every window and forgetting them would
                // defeat restore.
                tauri::WindowEvent::Destroyed => {
                    let scope = lock(&shell.windows).remove(window.label());
                    if !shell.quitting.load(Ordering::Relaxed) {
                        if let Some(scope) = scope {
                            lock(&shell.registry).remove(&scope.stable_id);
                        }
                    }
                }
                // Track geometry in memory on every move/resize; a slow tick
                // (and exit) persists — never a file write per drag event.
                tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                    let Some(stable_id) = lock(&shell.windows)
                        .get(window.label())
                        .map(|s| s.stable_id.clone())
                    else {
                        return;
                    };
                    let scale = window.scale_factor().unwrap_or(1.0);
                    if let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) {
                        let pos = pos.to_logical::<f64>(scale);
                        let size = size.to_logical::<f64>(scale);
                        lock(&shell.registry).set_geometry(
                            &stable_id,
                            pos.x,
                            pos.y,
                            size.width,
                            size.height,
                        );
                    }
                }
                _ => {}
            }
        })
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
            let mut local =
                match tauri::async_runtime::block_on(crate::daemon::ensure_local_daemon()) {
                    Ok(local) => local,
                    // Windows first run: no WSL / no distro yet, so there is no
                    // daemon origin to load — open the shell-local setup wizard
                    // instead of failing; its finishing command (wsl_setup_daemon)
                    // completes the startup this path skips.
                    Err(e) if e.downcast_ref::<crate::wsl::WslNotReady>().is_some() => {
                        tracing::info!("WSL not ready — opening the setup wizard");
                        open_setup_window(&handle)?;
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::error!("{e:#}");
                        return Err(std::io::Error::other(format!("{e:#}")).into());
                    }
                };
            // A fresh update intent = the user clicked "update" in the OLD
            // process and the app half already swapped; finish the chain by
            // replacing the outdated daemon NOW, before any window loads its
            // (old) embedded UI. The daemon's handoff keeps port + token and
            // its ledger resurrects the sessions, so the windows restored
            // below land on the new daemon with everything where it was.
            if crate::update::consume_intent() && local.outdated {
                tracing::info!("update intent: replacing the outdated local daemon");
                match tauri::async_runtime::block_on(crate::daemon::update_local_daemon()) {
                    Ok(fresh) => local = fresh,
                    // The old daemon keeps serving; the update stays a
                    // visible affordance instead of a silent failure.
                    Err(e) => tracing::warn!("daemon update after app update failed: {e:#}"),
                }
            }
            finish_startup(&handle, local)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building chimaera")
        .run(|app, event| match event {
            // Quit teardown destroys every window; flag it FIRST so those
            // Destroyed events keep the registry intact for the next launch.
            tauri::RunEvent::ExitRequested { .. } => {
                if let Some(state) = app.try_state::<Shell>() {
                    state.quitting.store(true, Ordering::Relaxed);
                    lock(&state.registry).save_if_dirty();
                }
            }
            tauri::RunEvent::Exit => {
                // Kill tunnel children so forwarded ports don't leak past
                // the app (the daemons keep running by design).
                if let Some(state) = app.try_state::<Shell>() {
                    lock(&state.registry).save_if_dirty();
                    tauri::async_runtime::block_on(async {
                        let mut tunnels = state.tunnels.lock().await;
                        for (_, tunnel) in tunnels.drain() {
                            tunnel.close().await;
                        }
                    });
                }
            }
            _ => {}
        });
}
