//! The macOS menu-bar / Windows-Linux system-tray status item: a persistent
//! entry point when no window is focused (or all are closed but the app is
//! still resident). Clicking it opens a small menu — raise the app, open a
//! fresh window, or quit. The daemons keep running regardless; this is only a
//! window affordance.
//!
//! Installed once at setup (beside `menu::install`). The menu-event handlers
//! read `Shell.local` for the port + token — populated by the time any click
//! fires at runtime, even though the tray is created before the daemon is up.

use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{App, AppHandle, Manager};

pub fn install(app: &App) -> tauri::Result<()> {
    let handle = app.handle();

    let show = MenuItemBuilder::with_id("tray-show", "Show Chimaera").build(handle)?;
    let new_window = MenuItemBuilder::with_id("tray-new-window", "New Window").build(handle)?;
    let quit = PredefinedMenuItem::quit(handle, Some("Quit Chimaera"))?;
    let menu = MenuBuilder::new(handle)
        .item(&show)
        .item(&new_window)
        .separator()
        .item(&quit)
        .build()?;

    let mut builder = TrayIconBuilder::with_id("chimaera-tray")
        .tooltip("Chimaera")
        .menu(&menu)
        .on_menu_event(|app: &AppHandle, event| match event.id().0.as_str() {
            "tray-show" => raise_a_window(app),
            "tray-new-window" => open_new_window(app),
            _ => {}
        });
    // Reuse the app icon; `icon_as_template` lets macOS tint it to the menu-bar
    // theme (light/dark) instead of showing a fixed-color glyph. A no-op on
    // Windows/Linux, and skipped entirely if there is no bundled icon.
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon).icon_as_template(true);
    }
    builder.build(app)?;
    Ok(())
}

/// Show + focus a window: the focused one if any, else any existing window,
/// else open a fresh one. Handles the "all windows closed, app still alive"
/// case (macOS keeps the process resident after the last window closes).
fn raise_a_window(app: &AppHandle) {
    let win = app
        .webview_windows()
        .into_values()
        .find(|w| w.is_focused().unwrap_or(false))
        .or_else(|| app.webview_windows().into_values().next());
    match win {
        Some(w) => {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
        None => open_new_window(app),
    }
}

/// Open a fresh home window on the local daemon (same path as the File →
/// New Window item).
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
