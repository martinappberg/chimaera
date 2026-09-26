//! Native menu: the shell finally owns the chords a browser reserves.
//! Cmd+W closes the focused VIEW (the web UI decides what that means);
//! Cmd+T / Cmd+Shift+T start sessions; Cmd+Shift+N opens a New Window at Home
//! (or focuses the one unused launcher). Items the page handles are forwarded
//! as a "menu" event to the focused window (see onMenu in native.ts).

use tauri::menu::{MenuBuilder, MenuItem, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::{App, AppHandle, Emitter, Manager, Wry};

/// Handles to menu items whose enabled state tracks runtime context, so
/// [`sync_settings_enabled`] can toggle them. Managed on the app at install.
pub(crate) struct MenuState {
    /// Settings is workspace/daemon-scoped, so it's greyed out unless the
    /// focused window actually has a workspace open (not the home screen).
    settings: MenuItem<Wry>,
}

pub fn install(app: &App) -> tauri::Result<()> {
    let handle = app.handle();

    // One Settings item, shared between the platform submenus below and the
    // managed handle. Starts disabled; the first focused workspace window
    // enables it (see `sync_settings_enabled`).
    let settings = MenuItemBuilder::with_id("settings", "Settings…")
        .accelerator("CmdOrCtrl+,")
        .enabled(false)
        .build(handle)?;

    // A custom Quit (not the predefined one, which quits without the shell
    // deciding anything) so it routes through `request_quit`: windows with
    // unsaved edits are asked first (shell/unsaved.rs), and the quit intent is
    // flagged so the window set survives for the next launch.
    let quit = MenuItemBuilder::with_id("quit", "Quit Chimaera")
        .accelerator("CmdOrCtrl+Q")
        .build(handle)?;

    // The platform convention beside About: an explicit ask that always
    // answers — the page checks the app and its daemon and shows the result,
    // "up to date" included.
    let check_updates =
        MenuItemBuilder::with_id("check-updates", "Check for Updates…").build(handle)?;

    // The application menu (services/hide/show) is a macOS concept; Windows
    // and Linux menubars start at File, where Quit must live instead.
    #[cfg(target_os = "macos")]
    let app_menu = SubmenuBuilder::new(handle, "Chimaera")
        .item(&PredefinedMenuItem::about(handle, None, None)?)
        .item(&check_updates)
        .separator()
        .item(&settings)
        .separator()
        .item(&PredefinedMenuItem::services(handle, None)?)
        .separator()
        .item(&PredefinedMenuItem::hide(handle, None)?)
        .item(&PredefinedMenuItem::hide_others(handle, None)?)
        .item(&PredefinedMenuItem::show_all(handle, None)?)
        .separator()
        .item(&quit)
        .build()?;

    let file = SubmenuBuilder::new(handle, "File")
        .item(
            &MenuItemBuilder::with_id("new-window", "New Window")
                .accelerator("CmdOrCtrl+Shift+N")
                .build(handle)?,
        )
        .separator()
        .item(
            &MenuItemBuilder::with_id("new-terminal", "New Terminal")
                .accelerator("CmdOrCtrl+T")
                .build(handle)?,
        )
        .item(
            &MenuItemBuilder::with_id("new-agent", "New Agent")
                .accelerator("CmdOrCtrl+Shift+T")
                .build(handle)?,
        )
        .separator()
        .item(
            &MenuItemBuilder::with_id("close-view", "Close View")
                .accelerator("CmdOrCtrl+W")
                .build(handle)?,
        )
        .item(&PredefinedMenuItem::close_window(
            handle,
            Some("Close Window"),
        )?);
    #[cfg(not(target_os = "macos"))]
    let file = file.separator().item(&settings).separator().item(&quit);
    let file = file.build()?;

    let edit = SubmenuBuilder::new(handle, "Edit")
        .item(&PredefinedMenuItem::undo(handle, None)?)
        .item(&PredefinedMenuItem::redo(handle, None)?)
        .separator()
        .item(&PredefinedMenuItem::cut(handle, None)?)
        .item(&PredefinedMenuItem::copy(handle, None)?)
        .item(&PredefinedMenuItem::paste(handle, None)?)
        .item(&PredefinedMenuItem::select_all(handle, None)?)
        .build()?;

    let view = SubmenuBuilder::new(handle, "View")
        .item(&PredefinedMenuItem::fullscreen(handle, None)?)
        .build()?;

    let window = SubmenuBuilder::new(handle, "Window")
        .item(&PredefinedMenuItem::minimize(handle, None)?)
        .item(&PredefinedMenuItem::maximize(handle, None)?);
    #[cfg(target_os = "macos")]
    let window = window
        .separator()
        .item(&PredefinedMenuItem::bring_all_to_front(handle, None)?);
    let window = window.build()?;

    // macOS carries About in the application submenu; Windows/Linux lost it
    // with that submenu, so give them the conventional Help menu — About is
    // the only in-app version display, which bug triage depends on.
    #[cfg(not(target_os = "macos"))]
    let help = SubmenuBuilder::new(handle, "Help")
        .item(&check_updates)
        .separator()
        .item(&PredefinedMenuItem::about(handle, None, None)?)
        .build()?;

    let menu = MenuBuilder::new(handle);
    #[cfg(target_os = "macos")]
    let menu = menu.items(&[&app_menu, &file, &edit, &view, &window]);
    #[cfg(not(target_os = "macos"))]
    let menu = menu.items(&[&file, &edit, &view, &window, &help]);
    let menu = menu.build()?;
    app.set_menu(menu)?;
    // AppKit appends the live window list (titles carry the "(N)" needs-you
    // prefix) to whichever submenu is registered as the app's Windows menu —
    // the conventional way back to one specific, possibly minimized, window.
    #[cfg(target_os = "macos")]
    window.set_as_windows_menu_for_nsapp()?;
    app.manage(MenuState { settings });

    app.on_menu_event(|app: &AppHandle, event| {
        let id = event.id().0.as_str();
        match id {
            "new-window" => {
                let _ = crate::shell::show_local_home(app, None);
            }
            "quit" => crate::shell::request_quit(app),
            "check-updates" => {
                // Any window can answer (home or workspace — the toast lives
                // in both); prefer the focused one. With none open, a home
                // window's own mount-time check is the answer.
                let windows = app.webview_windows();
                let target = windows
                    .values()
                    .find(|w| w.is_focused().unwrap_or(false))
                    .or_else(|| windows.values().next());
                match target {
                    Some(window) => {
                        let _ = window.set_focus();
                        let _ = app.emit_to(window.label(), "menu", id);
                    }
                    None => {
                        let _ = crate::shell::show_local_home(app, None);
                    }
                }
            }
            "close-view" | "new-terminal" | "new-agent" | "settings" => {
                // The page knows what "close the focused view" / "open settings"
                // means; the shell only knows which window is focused. emit_to,
                // not emit: a broadcast would act in EVERY window.
                if let Some(window) = app
                    .webview_windows()
                    .into_values()
                    .find(|w| w.is_focused().unwrap_or(false))
                {
                    let _ = app.emit_to(window.label(), "menu", id);
                }
            }
            _ => {}
        }
    });
    Ok(())
}

/// Enable the Settings menu item only when the focused window has a workspace
/// open — it's daemon/workspace-scoped, so on the home screen (or with no window
/// focused) it has nothing to act on and would open an empty surface. Called
/// whenever focus or the focused window's workspace changes. Cheap; a no-op
/// before the menu is managed.
pub(crate) fn sync_settings_enabled(app: &AppHandle) {
    if let Some(state) = app.try_state::<MenuState>() {
        let _ = state
            .settings
            .set_enabled(crate::shell::focused_ws_open(app));
    }
}
