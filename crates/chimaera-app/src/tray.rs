//! The macOS menu-bar / Windows-Linux system-tray status item: a persistent
//! entry point that stays put when windows come and go. Its menu lists the open
//! workspace windows (click one to raise it), opens a fresh window, and — on
//! macOS — carries the "Keep Awake" (caffeinate) toggle so the state is both
//! shown (the icon fills in when armed) and flippable without a window focused.
//! The daemons keep running regardless; this is only a window/status affordance.
//!
//! The icon is a real brand-mark template (a "C"-in-hexagon monogram, black on
//! transparent) so macOS tints it to the menu-bar theme instead of showing the
//! full app icon rendered — as a solid blob — through the template mask.
//!
//! Installed once at setup, before the daemon is up (its click handlers read
//! `Shell`, populated by the time any click fires). The menu is rebuilt on the
//! events that change what it shows — a window opens/closes/renames, or the
//! caffeinate state flips from any surface — via [`rebuild`].

use tauri::image::Image;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{App, AppHandle, Listener, Manager, Wry};

const TRAY_ID: &str = "chimaera-tray";

pub fn install(app: &App) -> tauri::Result<()> {
    let handle = app.handle();
    let menu = build_menu(handle)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(tooltip(false))
        .icon(icon(false))
        .icon_as_template(true)
        .menu(&menu)
        .on_menu_event(|app: &AppHandle, event| match event.id().0.as_str() {
            "tray-new-window" => open_new_window(app),
            // Toggle the shared assertion; the resulting `caffeinate-changed`
            // broadcast is what rebuilds the tray (icon + check) below.
            "tray-caffeinate" => {
                let _ = crate::shell::apply_caffeinate(app, !crate::shell::caffeinate_armed(app));
            }
            other => {
                if let Some(label) = other.strip_prefix("tray-win:") {
                    focus_window(app, label);
                }
            }
        })
        .build(app)?;
    // Keep the icon + "Keep Awake" check in sync when caffeinate flips from the
    // in-window toggle (the tray-driven flip lands here too, harmlessly).
    let sync = app.handle().clone();
    app.listen("caffeinate-changed", move |_| rebuild(&sync));
    Ok(())
}

/// Rebuild the tray menu + icon from live state (open windows, caffeinate
/// armed). Cheap and idempotent; a no-op before the tray exists. Marshalled to
/// the main thread because menu/tray mutation must run there on macOS, and this
/// is called from command/event threads.
pub fn rebuild(app: &AppHandle) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        let Some(tray) = app.tray_by_id(TRAY_ID) else {
            return;
        };
        match build_menu(&app) {
            Ok(menu) => {
                let _ = tray.set_menu(Some(menu));
            }
            Err(e) => tracing::warn!("tray menu rebuild failed: {e:#}"),
        }
        let armed = crate::shell::caffeinate_armed(&app);
        let _ = tray.set_icon(Some(icon(armed)));
        let _ = tray.set_icon_as_template(true);
        let _ = tray.set_tooltip(Some(tooltip(armed)));
    });
}

/// The current menu: [macOS: Keep Awake ✓] · one item per open workspace window
/// · New Window · Quit. Rebuilt (not mutated) on every change so the check state
/// and window list are always freshly correct.
fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let mut b = MenuBuilder::new(app);

    #[cfg(target_os = "macos")]
    {
        use tauri::menu::CheckMenuItemBuilder;
        let keep = CheckMenuItemBuilder::with_id("tray-caffeinate", "Keep Awake")
            .checked(crate::shell::caffeinate_armed(app))
            .build(app)?;
        b = b.item(&keep).separator();
    }

    // Open windows, oldest first (labels are "win-N"), excluding the WSL wizard.
    let mut wins: Vec<(String, String)> = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| !label.starts_with("wsl-setup"))
        .map(|(label, win)| {
            let title = win.title().unwrap_or_default();
            let title = if title.trim().is_empty() {
                "Chimaera".to_string()
            } else {
                title
            };
            (label, title)
        })
        .collect();
    wins.sort_by_key(|(label, _)| seq_of(label));
    for (label, title) in &wins {
        let item = MenuItemBuilder::with_id(format!("tray-win:{label}"), title).build(app)?;
        b = b.item(&item);
    }
    if !wins.is_empty() {
        b = b.separator();
    }

    let new_window = MenuItemBuilder::with_id("tray-new-window", "New Window").build(app)?;
    b = b
        .item(&new_window)
        .separator()
        .item(&PredefinedMenuItem::quit(app, Some("Quit Chimaera"))?);
    b.build()
}

/// Sort key from a "win-N" label so the tray lists windows in open order.
fn seq_of(label: &str) -> u64 {
    label
        .strip_prefix("win-")
        .and_then(|n| n.parse().ok())
        .unwrap_or(u64::MAX)
}

/// The template icon: outline monogram idle, filled hexagon while caffeinated —
/// a menu-bar-legible "on" indicator. Both are black-on-transparent so macOS
/// tints them to the bar theme (`icon_as_template`).
fn icon(armed: bool) -> Image<'static> {
    if armed {
        tauri::include_image!("icons/tray-awake.png")
    } else {
        tauri::include_image!("icons/tray-idle.png")
    }
}

fn tooltip(armed: bool) -> &'static str {
    if armed {
        "Chimaera — keeping this Mac awake"
    } else {
        "Chimaera"
    }
}

/// Show + focus a specific window by label (a tray window-list click).
fn focus_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Open a fresh home window on the local daemon (same path as File → New
/// Window). Handles the "all windows closed, app still resident" case.
fn open_new_window(app: &AppHandle) {
    if let Some(shell) = app.try_state::<crate::shell::Shell>() {
        let (port, token) = {
            let local = crate::shell::lock(&shell.local);
            (local.port, local.token.clone())
        };
        let _ = crate::shell::open_ui_window(
            app,
            port,
            &token,
            &crate::windows::WindowRecord::new(None, None),
        );
    }
}
