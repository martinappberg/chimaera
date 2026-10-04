//! Fixed optional account quit hooks. Unsaved admission and final native
//! registry/window cleanup remain in the original host; the absent free
//! assembly never probes account status or creates a paid quit state machine.
use super::Shell;
use crate::account::QuitIntent;
use tauri::{AppHandle, Manager};

pub(crate) const HANDOFF_WINDOW: &str = "cloud-handoff";

pub(crate) fn hold_quit(app: &AppHandle) -> bool {
    hold(app, QuitIntent::Quit)
}
pub(crate) fn hold_last_close(app: &AppHandle, label: &str) -> bool {
    hold(app, QuitIntent::LastClose(label.to_owned()))
}
#[cfg(target_os = "macos")]
pub(crate) fn hold_os_quit(app: &AppHandle) -> bool {
    hold(app, QuitIntent::OsQuit)
}
fn hold(app: &AppHandle, intent: QuitIntent) -> bool {
    app.try_state::<Shell>()
        .and_then(|shell| shell.pro.owner().cloned())
        .is_some_and(|owner| owner.hold_quit(app.clone(), intent))
}
pub(crate) fn handoff_window_closing(app: &AppHandle) {
    if let Some(owner) = app
        .try_state::<Shell>()
        .and_then(|shell| shell.pro.owner().cloned())
    {
        owner.handoff_window_closing(app.clone());
    }
}
