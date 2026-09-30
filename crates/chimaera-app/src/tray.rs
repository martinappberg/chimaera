//! The macOS menu-bar / Windows-Linux system-tray status item: a persistent
//! entry point that stays put when windows come and go. Its menu lists the open
//! workspace windows (click one to raise it) and opens a fresh window. The
//! daemons keep running regardless; this is only a window/status affordance.
//!
//! The icon is a real brand-mark template (a "C"-in-hexagon monogram, black on
//! transparent) so macOS tints it to the menu-bar theme instead of showing the
//! full app icon rendered — as a solid blob — through the template mask.
//!
//! Installed once at setup, before the daemon is up (its click handlers read
//! `Shell`, populated by the time any click fires). The menu is rebuilt on the
//! events that change what it shows — a window opens/closes/renames, or the
//! approval counts move — via [`rebuild`].

use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{App, AppHandle, Manager, Wry};

const TRAY_ID: &str = "chimaera-tray";

pub fn install(app: &App) -> tauri::Result<()> {
    let handle = app.handle();
    let menu = build_menu(handle)?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(tooltip(0))
        .menu(&menu)
        .on_menu_event(|app: &AppHandle, event| match event.id().0.as_str() {
            "quit" => crate::shell::request_quit(app),
            "tray-new-window" => show_home(app),
            other => {
                if let Some(label) = other.strip_prefix("tray-win:") {
                    focus_window(app, label);
                }
            }
        });
    // macOS tints a template glyph to the menu-bar theme. Off macOS, template
    // tinting doesn't apply — a black-on-transparent glyph vanishes on a dark
    // taskbar/panel — so use the full-colour app icon there instead.
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .icon(tauri::include_image!("icons/tray.png"))
            .icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(app_icon) = app.default_window_icon().cloned() {
        builder = builder.icon(app_icon);
    }
    builder.build(app)?;
    Ok(())
}

/// Rebuild the tray menu + tooltip from live state (open windows, approval
/// counts). Cheap and idempotent; a no-op before the tray exists. Marshalled to
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
        let waiting = crate::shell::notices::attention_total(&app);
        let _ = tray.set_tooltip(Some(tooltip(waiting)));
    });
}

/// The current menu: one item per open workspace window · New Window · Quit.
/// Rebuilt (not mutated) on every change so the window list is always freshly
/// correct.
fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let mut b = MenuBuilder::new(app);

    // Open windows, oldest first (labels are "win-N"), each named by the
    // SPA-reported label (the workspace name, or "Home") — never the racy OS
    // titlebar, which lags the async setTitle and falls back to "chimaera".
    let mut wins = crate::shell::tray_windows(app);
    wins.sort_by_key(|(label, _)| seq_of(label));
    for (label, name) in &wins {
        // Which window needs you: its count of agents blocked on an
        // approval rides the entry.
        let text = match crate::shell::notices::window_attention(app, label) {
            0 => name.clone(),
            n => format!("{name} — {n} awaiting approval"),
        };
        let item = MenuItemBuilder::with_id(format!("tray-win:{label}"), text).build(app)?;
        b = b.item(&item);
    }
    if !wins.is_empty() {
        b = b.separator();
    }

    let new_window = MenuItemBuilder::with_id("tray-new-window", "New Window").build(app)?;
    // Custom Quit (not predefined) so it goes through `request_quit`, which
    // asks windows with unsaved edits first and flags the quit intent — see
    // menu.rs and shell/unsaved.rs.
    let quit = MenuItemBuilder::with_id("quit", "Quit Chimaera").build(app)?;
    b = b.item(&new_window).separator().item(&quit);
    b.build()
}

/// Sort key from a "win-N" label so the tray lists windows in open order.
fn seq_of(label: &str) -> u64 {
    label
        .strip_prefix("win-")
        .and_then(|n| n.parse().ok())
        .unwrap_or(u64::MAX)
}

fn tooltip(waiting: usize) -> String {
    let mut parts = vec!["Chimaera".to_string()];
    match waiting {
        0 => {}
        1 => parts.push("1 agent awaiting approval".to_string()),
        n => parts.push(format!("{n} agents awaiting approval")),
    }
    parts.join(" — ")
}

/// Show + focus a specific window by label (a tray window-list click).
fn focus_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Open a new launcher at Home, or focus the existing unused launcher.
fn show_home(app: &AppHandle) {
    let _ = crate::shell::show_local_home(app, None);
}
