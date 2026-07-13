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
use tauri::{AppHandle, Emitter, Manager};

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
    /// The held power assertion for the "caffeinate" toggle — Some = armed
    /// (this machine won't idle/display/system-sleep). Dropped to disarm; the
    /// guard drops on quit, so the assertion never outlives the app.
    caffeinate: Mutex<Option<keepawake::KeepAwake>>,
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

/// Whether the currently-focused window has a workspace open (vs the home
/// screen or no window focused). Drives the menu's Settings item, which is
/// workspace/daemon-scoped. Reads the scope map (populated by `open_ui_window`
/// and `report_window_scope`); false before startup or for the WSL wizard.
pub(crate) fn focused_ws_open(app: &AppHandle) -> bool {
    let Some(focused) = app
        .webview_windows()
        .into_values()
        .find(|w| w.is_focused().unwrap_or(false))
    else {
        return false;
    };
    app.try_state::<Shell>()
        .map(|shell| {
            lock(&shell.windows)
                .get(focused.label())
                .is_some_and(|s| s.ws.is_some())
        })
        .unwrap_or(false)
}

/// Whether the "caffeinate" power assertion is currently held. Reads the
/// managed `Shell` off the app handle so the tray (a sibling module that can't
/// see `Shell`'s private field) can reflect the state; false before startup.
pub(crate) fn caffeinate_armed(app: &AppHandle) -> bool {
    app.try_state::<Shell>()
        .map(|s| lock(&s.caffeinate).is_some())
        .unwrap_or(false)
}

/// Arm/disarm the caffeinate assertion for THIS machine, the shared core behind
/// both the UI command (`commands::set_caffeinate`) and the tray's "Keep Awake"
/// item. While armed the app host won't idle-, display-, or system-sleep —
/// including lid-closed on macOS, though only on AC power (Apple blocks
/// clamshell-awake on battery; no app can override that). Idempotent: re-arming
/// keeps the single held guard, disarming drops it. The resulting state
/// broadcasts on `caffeinate-changed` so every window's toggle AND the tray
/// (icon + menu check) stay in sync regardless of which surface flipped it.
pub(crate) fn apply_caffeinate(app: &AppHandle, on: bool) -> Result<bool, String> {
    let shell = app
        .try_state::<Shell>()
        .ok_or_else(|| "the app is not ready yet".to_string())?;
    let mut guard = lock(&shell.caffeinate);
    if on {
        if guard.is_none() {
            let awake = keepawake::Builder::default()
                .display(true)
                .idle(true)
                .sleep(true)
                .app_name("Chimaera")
                .app_reverse_domain("com.chimaera.app")
                .reason("Caffeinate")
                .create()
                .map_err(|e| format!("{e:#}"))?;
            *guard = Some(awake);
        }
    } else {
        *guard = None; // dropping the guard releases the assertion
    }
    let armed = guard.is_some();
    drop(guard);
    let _ = app.emit("caffeinate-changed", armed);
    Ok(armed)
}

/// Startup completion is a real three-state gate, NOT `try_state::<Shell>()`:
/// the wizard's finishing command must be excluded while another invocation
/// is mid-flight (provisioning runs minutes, and a webview reload re-enables
/// its button), and a finish that failed AFTER manage(Shell) must stay
/// retryable — Shell existing does not mean startup completed.
pub(crate) enum StartupClaim {
    Claimed,
    InFlight,
    Done,
}

static STARTUP_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static STARTUP_DONE: AtomicBool = AtomicBool::new(false);

pub(crate) fn claim_startup() -> StartupClaim {
    if STARTUP_DONE.load(Ordering::SeqCst) {
        return StartupClaim::Done;
    }
    if STARTUP_IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        StartupClaim::Claimed
    } else {
        StartupClaim::InFlight
    }
}

pub(crate) fn release_startup(success: bool) {
    if success {
        STARTUP_DONE.store(true, Ordering::SeqCst);
    }
    STARTUP_IN_FLIGHT.store(false, Ordering::SeqCst);
}

/// Post-completion self-heal: if startup finished but no real (non-wizard)
/// window survived — a partial finish, or every window closed while a stale
/// wizard lingered — open the home window so retiring the wizard can never
/// leave the app window-less (last-window-closed would exit it).
pub(crate) fn recover_windows(handle: &tauri::AppHandle) {
    let Some(shell) = handle.try_state::<Shell>() else {
        return;
    };
    let has_real = handle
        .webview_windows()
        .keys()
        .any(|l| !l.starts_with("wsl-setup"));
    if has_real {
        return;
    }
    let (port, token) = {
        let local = lock(&shell.local);
        (local.port, local.token.clone())
    };
    if let Err(e) = open_ui_window(handle, port, &token, &WindowRecord::new(None, None)) {
        tracing::error!("could not recover a home window: {e}");
    }
}

/// Complete startup once a local daemon exists: manage `Shell` (BEFORE any
/// window opens, so each registers its scope), reopen the persisted window
/// set or the home window, and start the monitors. Two callers: `setup()` on
/// the normal path, and the WSL wizard's finishing command on a Windows
/// first run — where setup() deliberately returned early with no daemon.
/// Re-entrant for the retry-after-partial-failure case: an already-managed
/// Shell keeps its state (monitors are NOT spawned twice); only the daemon
/// handle is refreshed and the windows re-attempted.
pub(crate) fn finish_startup(handle: &tauri::AppHandle, local: LocalDaemon) -> tauri::Result<()> {
    let (port, token) = (local.port, local.token.clone());
    let fresh = handle.try_state::<Shell>().is_none();
    if fresh {
        handle.manage(Shell {
            local: Mutex::new(local),
            tunnels: tokio::sync::Mutex::new(HashMap::new()),
            connecting: Mutex::new(HashMap::new()),
            windows: Mutex::new(HashMap::new()),
            registry: Mutex::new(WindowRegistry::load_default()),
            quitting: AtomicBool::new(false),
            caffeinate: Mutex::new(None),
        });
    } else {
        *lock(&handle.state::<Shell>().local) = local;
    }
    // Reopen last session's windows; first launch (or an all-remote set that
    // is still connecting) gets the home window so the app never comes up
    // invisible.
    if !restore::restore_windows(handle, port, &token) {
        open_ui_window(handle, port, &token, &WindowRecord::new(None, None))?;
        tracing::info!("home window open on 127.0.0.1:{port}");
    }
    if fresh {
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
    }
    // Seed the Settings item's enabled state for the windows just opened, in
    // case a restored workspace window doesn't emit an early Focused event.
    crate::menu::sync_settings_enabled(handle);
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
            commands::set_caffeinate,
            commands::caffeinate_state,
            commands::answer_askpass,
            commands::list_askpass,
            commands::report_window_scope,
            commands::wsl_status,
            commands::wsl_install,
            commands::wsl_update,
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
                        // The tray lists open windows; drop the closed one, and
                        // resync Settings for whatever window is focused now
                        // (or none). Skipped during quit (all windows tear down).
                        crate::tray::rebuild(window.app_handle());
                        crate::menu::sync_settings_enabled(window.app_handle());
                    }
                }
                // Focus moved to this window — Settings tracks whether the now-
                // focused window has a workspace open.
                tauri::WindowEvent::Focused(true) => {
                    crate::menu::sync_settings_enabled(window.app_handle());
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
            // The menu-bar / system-tray status item. Installed before the
            // daemon is up (its click handlers read Shell.local, populated by
            // runtime); non-fatal if the platform tray is unavailable.
            if let Err(e) = crate::tray::install(app) {
                tracing::warn!("system tray unavailable: {e:#}");
            }
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
                    // Windows: anything that needs provisioning — no WSL, no
                    // distro, no (current) daemon — goes through the wizard,
                    // which shows the work; startup itself never downloads or
                    // installs invisibly. Its finishing command
                    // (wsl_setup_daemon) completes the startup this skips.
                    Err(e) if e.downcast_ref::<crate::wsl::WslNotReady>().is_some() => {
                        tracing::info!("WSL daemon not adoptable — opening the setup wizard");
                        // Any pending update intent is moot on this path: the
                        // wizard's ensure replaces an old-build daemon anyway,
                        // and a stale intent must not fire on a LATER launch
                        // detached from the click that authorized it.
                        crate::update::clear_intent();
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
            // Uncontended here (no windows exist yet), but the gate must
            // still be claimed so a stale wizard invocation later reads
            // "done" instead of racing a second startup.
            let _ = claim_startup();
            let finished = finish_startup(&handle, local);
            release_startup(finished.is_ok());
            finished?;
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
