//! Native menu: the shell finally owns the chords a browser reserves.
//! Cmd+W closes the focused VIEW (the web UI decides what that means);
//! Cmd+T / Cmd+Shift+T start sessions; Cmd+Shift+N opens a fresh home
//! window. Items the page handles are forwarded as a "menu" event to the
//! focused window (see onMenu in native.ts).

use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::{App, AppHandle, Emitter, Manager};

pub fn install(app: &App) -> tauri::Result<()> {
    let handle = app.handle();

    // The application menu (services/hide/show) is a macOS concept; Windows
    // and Linux menubars start at File, where Quit must live instead.
    #[cfg(target_os = "macos")]
    let app_menu = SubmenuBuilder::new(handle, "Chimaera")
        .item(&PredefinedMenuItem::about(handle, None, None)?)
        .separator()
        .item(&PredefinedMenuItem::services(handle, None)?)
        .separator()
        .item(&PredefinedMenuItem::hide(handle, None)?)
        .item(&PredefinedMenuItem::hide_others(handle, None)?)
        .item(&PredefinedMenuItem::show_all(handle, None)?)
        .separator()
        .item(&PredefinedMenuItem::quit(handle, None)?)
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
    let file = file
        .separator()
        .item(&PredefinedMenuItem::quit(handle, None)?);
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
        .item(&PredefinedMenuItem::maximize(handle, None)?)
        .build()?;

    let menu = MenuBuilder::new(handle);
    #[cfg(target_os = "macos")]
    let menu = menu.items(&[&app_menu, &file, &edit, &view, &window]);
    #[cfg(not(target_os = "macos"))]
    let menu = menu.items(&[&file, &edit, &view, &window]);
    let menu = menu.build()?;
    app.set_menu(menu)?;

    app.on_menu_event(|app: &AppHandle, event| {
        let id = event.id().0.as_str();
        match id {
            "new-window" => {
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
            "close-view" | "new-terminal" | "new-agent" => {
                // The page knows what "close the focused view" means; the
                // shell only knows which window is focused. emit_to, not
                // emit: a broadcast would close a view in EVERY window.
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
