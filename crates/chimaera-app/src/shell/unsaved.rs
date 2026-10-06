//! Window close and app quit never drop unsaved editor text (document
//! workbench plan, Phase 0).
//!
//! **Push, not pull.** Each page reports its unsaved-file count
//! (`report_unsaved`) whenever it changes, so `CloseRequested` and quit decide
//! synchronously from shell state, with no round trip into a webview that may
//! be busy or gone. A window with nothing unsaved closes, and the app quits,
//! exactly as before: zero friction in the common case.
//!
//! **Held close / quit.** A close of a window with unsaved files is
//! prevented and that window is asked (`unsaved-prompt`); it runs the same
//! Save all / Don't save / Cancel dialog the tab close uses and answers with
//! `reply_unsaved`. A quit asks the windows with unsaved files one at a time,
//! most recently focused first; Cancel in any of them abandons the quit, and
//! once every one has proceeded the quit completes with the usual `quitting`
//! semantics (windows stay in the registry for the next launch).
//!
//! **Never a trap.** A prompted page must show it is alive (reply `shown`)
//! within [`ACK_TIMEOUT`]; a page that does not (a hung webview) is closed, or
//! skipped by the quit, anyway. That loses no text: the buffer store journals
//! dirty text to IndexedDB and mirrors it to the daemon about a second after
//! typing stops and again when the page hides (`previews/drafts.ts`), and the
//! reopened file offers the recovered draft. Asking again (a second close
//! click or ⌘Q) re-pings a prompt that was already shown, so a page that hung
//! after showing its dialog is caught by the same timeout; and the third ask
//! of one prompt proceeds outright, so not even a page that keeps answering
//! `shown` without ever showing a usable dialog can hold a window or the app.
//!
//! The decisions live in [`Guard`] (pure, unit-tested); the Tauri glue at the
//! bottom only turns its [`Action`]s into window operations.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use super::{lock, Shell};

/// How long a prompted page has to reply `shown` before the shell treats it
/// as hung and proceeds. Local IPC answers in milliseconds; the margin covers
/// a webview that is busy (a burst of terminal output) but alive.
pub(crate) const ACK_TIMEOUT: Duration = Duration::from_secs(4);

/// The ask of one prompt that proceeds without the page: the user insisting
/// (a third close click, a third ⌘Q) is the escape hatch from a page that
/// acknowledges prompts but never settles them.
pub(crate) const FORCE_AFTER_ASKS: u32 = 3;

/// Two asks closer together than this are one gesture delivered twice (Tauri
/// hands a tray menu event to both the tray's and the app's handler).
const DUPLICATE_ASK: Duration = Duration::from_millis(500);

/// The window-scoped event a held close or quit sends to the page.
pub(crate) const PROMPT_EVENT: &str = "unsaved-prompt";

/// Why a window is being asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Reason {
    Close,
    Quit,
}

/// The page's answer to a prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Reply {
    /// The dialog is on screen: the page is alive, so the shell stops its
    /// hung-page timer and waits for the user.
    Shown,
    /// Save all succeeded, or Don't save: nothing unsaved is left to lose.
    Proceed,
    /// Keep the window (and abandon a quit).
    Cancel,
}

/// Payload of [`PROMPT_EVENT`]. `id` is stable across re-asks of the same
/// prompt; a reply must carry it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PromptEvent {
    pub(crate) id: u64,
    pub(crate) reason: Reason,
}

/// What the glue must do after a [`Guard`] transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Nothing,
    /// Raise `label`, send it the prompt, and arm the hung-page timer for
    /// this `(id, ping)`.
    Ask {
        label: String,
        id: u64,
        reason: Reason,
        ping: u64,
    },
    /// Close `label` for real: its next `CloseRequested` passes.
    Close(String),
    /// Nothing holds the quit (any more): exit now.
    Exit,
}

/// Whether a window's close must wait for its page. `released` = the page
/// (or the hung-page timeout) already let this window go; `quitting` = a
/// confirmed exit is tearing every window down.
pub(crate) fn hold_close(unsaved: u32, released: bool, quitting: bool) -> bool {
    unsaved > 0 && !released && !quitting
}

/// Whether an `ExitRequested` is a quit the guard may hold. `None` is the
/// runtime's "the last window was destroyed", and every window's close
/// already passed [`hold_close`]. A restart (the updater) cannot be
/// prevented at all (`begin_update` refuses up front instead). `quitting`
/// means the guard already let this exit through.
pub(crate) fn exit_may_hold(code: Option<i32>, quitting: bool) -> bool {
    !quitting && code.is_some_and(|code| code != tauri::RESTART_EXIT_CODE)
}

/// The windows a quit must still ask, in `order` (most recently focused
/// first): unsaved files and not yet settled in this quit.
pub(crate) fn quit_blockers(
    order: &[String],
    counts: &HashMap<String, u32>,
    settled: &HashSet<String>,
) -> Vec<String> {
    order
        .iter()
        .filter(|label| counts.get(*label).copied().unwrap_or(0) > 0 && !settled.contains(*label))
        .cloned()
        .collect()
}

struct Prompt {
    id: u64,
    reason: Reason,
    /// Bumped by every re-ask; the hung-page timer only acts on its own ping.
    ping: u64,
    acked: bool,
    /// Close/quit requests that reached this prompt (the one that raised it
    /// included); [`FORCE_AFTER_ASKS`] of them proceed without the page.
    asks: u32,
    /// When the user last asked (None: raised by a quit moving on).
    last_ask: Option<Instant>,
}

/// Unsaved-edit state for every open window, keyed by volatile window label.
#[derive(Default)]
pub(crate) struct Guard {
    counts: HashMap<String, u32>,
    released: HashSet<String>,
    /// At most one outstanding prompt per window.
    prompts: HashMap<String, Prompt>,
    /// Windows already settled in the quit being negotiated; `None` = no
    /// quit in progress.
    quit: Option<HashSet<String>>,
    next_id: u64,
}

impl Guard {
    /// The page's unsaved-file count.
    pub(crate) fn report(&mut self, label: &str, count: u32) {
        self.counts.insert(label.to_string(), count);
    }

    /// Windows currently holding unsaved files.
    pub(crate) fn windows_with_unsaved(&self) -> usize {
        self.counts.values().filter(|count| **count > 0).count()
    }

    /// A close was requested. `None` lets it through; `Some` holds it (the
    /// caller prevents the close) and says what to do. `order` is every open
    /// window, most recently focused first (an insisted-past quit prompt
    /// moves the quit on).
    pub(crate) fn close_requested(
        &mut self,
        label: &str,
        quitting: bool,
        order: &[String],
        now: Instant,
    ) -> Option<Action> {
        let unsaved = self.counts.get(label).copied().unwrap_or(0);
        if !hold_close(unsaved, self.released.contains(label), quitting) {
            return None;
        }
        Some(self.user_ask(label, Reason::Close, order, now))
    }

    /// A quit was requested. [`Action::Exit`] = nothing holds it; anything
    /// else holds it (the caller prevents or defers the exit).
    pub(crate) fn quit_requested(&mut self, order: &[String], now: Instant) -> Action {
        if self.quit.is_some() {
            // Already negotiating: re-ask whoever holds the quit now.
            let current = self
                .prompts
                .iter()
                .find(|(_, prompt)| prompt.reason == Reason::Quit)
                .map(|(label, _)| label.clone());
            return match current {
                Some(label) => self.user_ask(&label, Reason::Quit, order, now),
                None => self.next_quit_step(order),
            };
        }
        let blockers = quit_blockers(order, &self.counts, &HashSet::new());
        let Some(first) = blockers.first() else {
            return Action::Exit;
        };
        self.quit = Some(HashSet::new());
        self.user_ask(first, Reason::Quit, order, now)
    }

    /// The page answered prompt `id`. A stale id (an older prompt) is ignored.
    pub(crate) fn reply(&mut self, label: &str, id: u64, reply: Reply, order: &[String]) -> Action {
        let Some(prompt) = self.prompts.get_mut(label) else {
            return Action::Nothing;
        };
        if prompt.id != id {
            return Action::Nothing;
        }
        match reply {
            Reply::Shown => {
                prompt.acked = true;
                Action::Nothing
            }
            Reply::Cancel => {
                let reason = prompt.reason;
                self.prompts.remove(label);
                if reason == Reason::Quit {
                    self.quit = None;
                }
                Action::Nothing
            }
            Reply::Proceed => {
                self.counts.insert(label.to_string(), 0);
                self.settle(label, order)
            }
        }
    }

    /// The hung-page timer for `(id, ping)` fired. Proceeds unless the page
    /// showed the prompt, answered it, or was asked again since.
    pub(crate) fn ack_timeout(
        &mut self,
        label: &str,
        id: u64,
        ping: u64,
        order: &[String],
    ) -> Action {
        match self.prompts.get(label) {
            Some(prompt) if prompt.id == id && prompt.ping == ping && !prompt.acked => {
                self.settle(label, order)
            }
            _ => Action::Nothing,
        }
    }

    /// `label` was destroyed. If it held the quit, the quit moves on.
    /// `order` must no longer contain it.
    pub(crate) fn forget(&mut self, label: &str, order: &[String]) -> Action {
        self.counts.remove(label);
        self.released.remove(label);
        if let Some(settled) = &mut self.quit {
            settled.remove(label);
        }
        match self.prompts.remove(label) {
            Some(prompt) if prompt.reason == Reason::Quit => self.next_quit_step(order),
            _ => Action::Nothing,
        }
    }

    /// A user's close/quit reaching `label`: ask it, collapse a duplicate
    /// delivery, or — on the [`FORCE_AFTER_ASKS`]th ask — proceed without it.
    fn user_ask(&mut self, label: &str, reason: Reason, order: &[String], now: Instant) -> Action {
        let Some(prompt) = self.prompts.get_mut(label) else {
            let action = self.ask(label, reason);
            if let Some(prompt) = self.prompts.get_mut(label) {
                prompt.last_ask = Some(now);
            }
            return action;
        };
        // A quit reaching a close prompt is a new request however soon it
        // follows; swallowing it would leave the quit with no prompt of its own.
        let takes_over = reason == Reason::Quit && prompt.reason == Reason::Close;
        if !takes_over
            && prompt
                .last_ask
                .is_some_and(|last| now.saturating_duration_since(last) < DUPLICATE_ASK)
        {
            return Action::Nothing;
        }
        prompt.last_ask = Some(now);
        prompt.asks += 1;
        if prompt.asks >= FORCE_AFTER_ASKS {
            if reason == Reason::Quit {
                prompt.reason = Reason::Quit;
            }
            return self.settle(label, order);
        }
        self.ask(label, reason)
    }

    /// Ask `label`, or re-ask its outstanding prompt. A quit takes over a
    /// pending close prompt (one dialog, and settling it serves the quit);
    /// a close never downgrades a quit prompt.
    fn ask(&mut self, label: &str, reason: Reason) -> Action {
        let fresh_id = self.next_id + 1;
        let prompt = self.prompts.entry(label.to_string()).or_insert(Prompt {
            id: fresh_id,
            reason,
            ping: 0,
            acked: false,
            asks: 1,
            last_ask: None,
        });
        if prompt.id == fresh_id {
            self.next_id = fresh_id;
        } else {
            prompt.ping += 1;
            prompt.acked = false;
            if reason == Reason::Quit {
                prompt.reason = Reason::Quit;
            }
        }
        Action::Ask {
            label: label.to_string(),
            id: prompt.id,
            reason: prompt.reason,
            ping: prompt.ping,
        }
    }

    /// `label` let go of its edits (or stopped answering): close it, or move
    /// the quit on.
    fn settle(&mut self, label: &str, order: &[String]) -> Action {
        let Some(prompt) = self.prompts.remove(label) else {
            return Action::Nothing;
        };
        match prompt.reason {
            Reason::Close => {
                self.released.insert(label.to_string());
                Action::Close(label.to_string())
            }
            Reason::Quit => {
                if let Some(settled) = &mut self.quit {
                    settled.insert(label.to_string());
                }
                self.next_quit_step(order)
            }
        }
    }

    fn next_quit_step(&mut self, order: &[String]) -> Action {
        let Some(settled) = &self.quit else {
            return Action::Nothing;
        };
        match quit_blockers(order, &self.counts, settled).first() {
            Some(next) => {
                let next = next.clone();
                self.ask(&next, Reason::Quit)
            }
            None => {
                self.quit = None;
                Action::Exit
            }
        }
    }
}

// --- Tauri glue ---------------------------------------------------------------

/// Every open workbench window, most recently focused first, then the rest by
/// label. Never holds a shell lock while the caller takes the guard's.
fn window_order(shell: &Shell) -> Vec<String> {
    let mut order = lock(&shell.focus_order).clone();
    let mut rest: Vec<String> = lock(&shell.windows).keys().cloned().collect();
    order.retain(|label| rest.contains(label));
    rest.retain(|label| !order.contains(label));
    rest.sort();
    order.extend(rest);
    order
}

/// A window's `CloseRequested`: true = hold it (the caller prevents the
/// close); the prompt is already on its way.
pub(crate) fn hold_window_close(app: &AppHandle, label: &str) -> bool {
    let Some(shell) = app.try_state::<Shell>() else {
        return false;
    };
    let quitting = shell.quitting.load(Ordering::Relaxed);
    let order = window_order(&shell);
    let verdict = lock(&shell.unsaved).close_requested(label, quitting, &order, Instant::now());
    match verdict {
        Some(action) => {
            perform(app, action);
            true
        }
        None => false,
    }
}

/// A quit (menu, tray, ⌘Q, a programmatic exit, macOS terminate). True = it
/// may go ahead now; false = held while windows with unsaved files are
/// asked, and a confirmed answer finishes it through `finish_quit`.
pub(crate) fn quit_may_proceed(app: &AppHandle) -> bool {
    let Some(shell) = app.try_state::<Shell>() else {
        return true;
    };
    let order = window_order(&shell);
    let action = lock(&shell.unsaved).quit_requested(&order, Instant::now());
    if action == Action::Exit {
        return true;
    }
    perform(app, action);
    false
}

/// A window was destroyed: drop its state, and move a quit it held along.
pub(crate) fn window_destroyed(app: &AppHandle, label: &str) {
    let Some(shell) = app.try_state::<Shell>() else {
        return;
    };
    let order = window_order(&shell);
    let action = lock(&shell.unsaved).forget(label, &order);
    perform(app, action);
}

/// The page's `report_unsaved`.
pub(crate) fn report(app: &AppHandle, label: &str, count: u32) {
    if let Some(shell) = app.try_state::<Shell>() {
        lock(&shell.unsaved).report(label, count);
    }
}

/// The page's `reply_unsaved`.
pub(crate) fn reply(app: &AppHandle, label: &str, id: u64, answer: Reply) {
    let Some(shell) = app.try_state::<Shell>() else {
        return;
    };
    let order = window_order(&shell);
    let action = lock(&shell.unsaved).reply(label, id, answer, &order);
    perform(app, action);
}

/// macOS delivers an OS-level quit (Dock › Quit, logging out, restart or
/// shut down, an AppleScript `quit`) as `-[NSApplication terminate:]`, which
/// never becomes a Tauri `ExitRequested`: tao's app delegate does not answer
/// `applicationShouldTerminate:`, so AppKit terminates straight away. Adding
/// that method to the delegate's class routes those quits through the same
/// guard. A held one answers `NSTerminateCancel`, so macOS reports that
/// Chimaera stopped the logout, as for any app with unsaved documents; the
/// user then answers the prompt and quits again. Our own confirmed quit
/// (`finish_quit`) stops the event loop without `terminate:`, so it never
/// comes back through here.
#[cfg(target_os = "macos")]
pub(crate) fn install_os_quit_hook(app: &AppHandle) {
    os_quit::install(app);
}

/// The macOS terminate hook's decision: true = let AppKit terminate now.
#[cfg(target_os = "macos")]
fn os_quit_may_proceed(app: &AppHandle) -> bool {
    let Some(shell) = app.try_state::<Shell>() else {
        return true;
    };
    if shell.quitting.load(Ordering::Relaxed) {
        return true;
    }
    if !quit_may_proceed(app) {
        return false;
    }
    // AppKit ends the process itself (tao turns applicationWillTerminate
    // into RunEvent::Exit, which closes tunnels); flag the quit first so the
    // window set is kept for the next launch, exactly like `finish_quit`.
    shell.quitting.store(true, Ordering::Relaxed);
    lock(&shell.registry).save_if_dirty();
    true
}

#[cfg(target_os = "macos")]
mod os_quit {
    use std::sync::OnceLock;

    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{sel, MainThreadMarker};
    use objc2_app_kit::NSApplication;
    use tauri::AppHandle;

    static APP: OnceLock<AppHandle> = OnceLock::new();

    // NSApplicationTerminateReply.
    const TERMINATE_CANCEL: usize = 0;
    const TERMINATE_NOW: usize = 1;

    /// `- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender`
    extern "C-unwind" fn should_terminate(
        _this: &AnyObject,
        _cmd: Sel,
        _sender: *mut AnyObject,
    ) -> usize {
        // A panic must not unwind into AppKit; failing open keeps the
        // behaviour this hook replaced (terminate).
        let allow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            APP.get().is_none_or(super::os_quit_may_proceed)
        }))
        .unwrap_or(true);
        if allow {
            TERMINATE_NOW
        } else {
            TERMINATE_CANCEL
        }
    }

    pub(super) fn install(app: &AppHandle) {
        let Some(mtm) = MainThreadMarker::new() else {
            tracing::warn!("OS quit hook: setup is not on the main thread");
            return;
        };
        let Some(delegate) = NSApplication::sharedApplication(mtm).delegate() else {
            tracing::warn!("OS quit hook: no application delegate");
            return;
        };
        let _ = APP.set(app.clone());
        let object: &AnyObject = (*delegate).as_ref();
        let class: &AnyClass = object.class();
        let should_terminate: extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize =
            should_terminate;
        // SAFETY: `should_terminate` has the method's C ABI — (id self, SEL
        // _cmd, id sender) -> NSUInteger — and "Q@:@" is its type encoding
        // (NSUInteger is `unsigned long`, encoded "Q" on every 64-bit macOS
        // target). Adding a method to a registered class is what the runtime
        // API is for; it fails (returns NO) rather than replace one the class
        // already defines.
        let imp = unsafe {
            std::mem::transmute::<extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize, Imp>(
                should_terminate,
            )
        };
        let added = unsafe {
            objc2::ffi::class_addMethod(
                (class as *const AnyClass).cast_mut(),
                sel!(applicationShouldTerminate:),
                imp,
                c"Q@:@".as_ptr(),
            )
        };
        if !added.as_bool() {
            tracing::warn!(
                "OS quit hook: the app delegate already answers applicationShouldTerminate:; \
                 OS-level quits skip the unsaved-edits guard"
            );
        }
    }
}

/// Windows holding unsaved files right now (the updater's precondition).
pub(crate) fn windows_with_unsaved(app: &AppHandle) -> usize {
    app.try_state::<Shell>()
        .map(|shell| lock(&shell.unsaved).windows_with_unsaved())
        .unwrap_or(0)
}

fn perform(app: &AppHandle, action: Action) {
    match action {
        Action::Nothing => {}
        Action::Ask {
            label,
            id,
            reason,
            ping,
        } => {
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
            if let Err(error) =
                app.emit_to(label.as_str(), PROMPT_EVENT, PromptEvent { id, reason })
            {
                tracing::warn!(%error, window = %label, "could not ask about unsaved edits");
            }
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(ACK_TIMEOUT).await;
                let Some(shell) = app.try_state::<Shell>() else {
                    return;
                };
                let order = window_order(&shell);
                let action = lock(&shell.unsaved).ack_timeout(&label, id, ping, &order);
                if action != Action::Nothing {
                    // Proceeding is safe: the page's dirty text is in the
                    // draft journal (IndexedDB + the daemon mirror), and
                    // reopening the file offers it back.
                    tracing::warn!(
                        window = %label,
                        "window did not answer the unsaved-edits prompt; proceeding (drafts are journaled)"
                    );
                }
                perform(&app, action);
            });
        }
        Action::Close(label) => {
            if let Some(window) = app.get_webview_window(&label) {
                if let Err(error) = window.close() {
                    tracing::warn!(%error, window = %label, "could not close window");
                }
            }
        }
        Action::Exit => super::finish_quit(app),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|label| label.to_string()).collect()
    }

    /// A clock whose ticks are far enough apart never to read as one
    /// duplicated gesture.
    struct Clock(Instant, u64);

    impl Clock {
        fn new() -> Self {
            Self(Instant::now(), 0)
        }

        fn tick(&mut self) -> Instant {
            self.1 += 1;
            self.0 + Duration::from_secs(self.1)
        }
    }

    fn ask(action: &Action) -> (String, u64, Reason, u64) {
        match action {
            Action::Ask {
                label,
                id,
                reason,
                ping,
            } => (label.clone(), *id, *reason, *ping),
            other => panic!("expected an ask, got {other:?}"),
        }
    }

    fn close(guard: &mut Guard, label: &str, clock: &mut Clock) -> Option<Action> {
        guard.close_requested(label, false, &order(&[label]), clock.tick())
    }

    #[test]
    fn hold_close_only_for_unsaved_unreleased_windows_outside_a_quit() {
        assert!(!hold_close(0, false, false));
        assert!(hold_close(2, false, false));
        assert!(!hold_close(2, true, false));
        assert!(!hold_close(2, false, true));
    }

    #[test]
    fn exit_may_hold_only_a_programmatic_non_restart_exit() {
        assert!(exit_may_hold(Some(0), false));
        assert!(!exit_may_hold(Some(0), true));
        // The last window's Destroyed: every close already passed the guard.
        assert!(!exit_may_hold(None, false));
        // Tauri ignores prevent_exit for a restart.
        assert!(!exit_may_hold(Some(tauri::RESTART_EXIT_CODE), false));
    }

    #[test]
    fn quit_blockers_follow_focus_order_and_skip_clean_or_settled() {
        let counts = HashMap::from([
            ("a".to_string(), 1),
            ("b".to_string(), 0),
            ("c".to_string(), 3),
        ]);
        let none = HashSet::new();
        assert_eq!(
            quit_blockers(&order(&["c", "b", "a", "d"]), &counts, &none),
            order(&["c", "a"])
        );
        let settled = HashSet::from(["c".to_string()]);
        assert_eq!(
            quit_blockers(&order(&["c", "b", "a"]), &counts, &settled),
            order(&["a"])
        );
    }

    #[test]
    fn a_clean_window_closes_without_asking() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        assert_eq!(close(&mut guard, "w", &mut clock), None);
        guard.report("w", 1);
        guard.report("w", 0);
        assert_eq!(close(&mut guard, "w", &mut clock), None);
    }

    #[test]
    fn close_waits_for_save_or_discard_then_passes() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        guard.report("w", 2);
        let (label, id, reason, ping) = ask(&close(&mut guard, "w", &mut clock).unwrap());
        assert_eq!((label.as_str(), reason, ping), ("w", Reason::Close, 0));
        assert_eq!(guard.reply("w", id, Reply::Shown, &[]), Action::Nothing);
        // A user deciding for a long time is not a hung page.
        assert_eq!(guard.ack_timeout("w", id, ping, &[]), Action::Nothing);
        assert_eq!(
            guard.reply("w", id, Reply::Proceed, &[]),
            Action::Close("w".into())
        );
        // The shell's own close now goes through.
        assert_eq!(close(&mut guard, "w", &mut clock), None);
    }

    #[test]
    fn cancel_keeps_the_window_and_the_next_close_asks_again() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        guard.report("w", 1);
        let (_, id, _, _) = ask(&close(&mut guard, "w", &mut clock).unwrap());
        assert_eq!(guard.reply("w", id, Reply::Cancel, &[]), Action::Nothing);
        let (_, again, _, _) = ask(&close(&mut guard, "w", &mut clock).unwrap());
        assert_ne!(again, id);
    }

    #[test]
    fn a_hung_page_is_closed_after_the_ack_timeout() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        guard.report("w", 1);
        let (_, id, _, ping) = ask(&close(&mut guard, "w", &mut clock).unwrap());
        assert_eq!(
            guard.ack_timeout("w", id, ping, &[]),
            Action::Close("w".into())
        );
        assert_eq!(close(&mut guard, "w", &mut clock), None);
    }

    #[test]
    fn asking_again_re_pings_so_a_page_hung_after_showing_cannot_trap() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        guard.report("w", 1);
        let (_, id, _, first) = ask(&close(&mut guard, "w", &mut clock).unwrap());
        guard.reply("w", id, Reply::Shown, &[]);
        let (_, same, _, second) = ask(&close(&mut guard, "w", &mut clock).unwrap());
        assert_eq!(same, id);
        assert!(second > first);
        // The first ask's timer is stale; the re-ask's timer decides.
        assert_eq!(guard.ack_timeout("w", id, first, &[]), Action::Nothing);
        assert_eq!(
            guard.ack_timeout("w", id, second, &[]),
            Action::Close("w".into())
        );
    }

    #[test]
    fn insisting_proceeds_past_a_page_that_acks_but_never_settles() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        guard.report("w", 1);
        let (_, id, _, _) = ask(&close(&mut guard, "w", &mut clock).unwrap());
        guard.reply("w", id, Reply::Shown, &[]);
        ask(&close(&mut guard, "w", &mut clock).unwrap());
        guard.reply("w", id, Reply::Shown, &[]);
        assert_eq!(
            close(&mut guard, "w", &mut clock),
            Some(Action::Close("w".into()))
        );
    }

    #[test]
    fn one_gesture_delivered_twice_is_one_ask() {
        let mut guard = Guard::default();
        guard.report("a", 1);
        let windows = order(&["a"]);
        let now = Instant::now();
        let (_, _, _, ping) = ask(&guard.quit_requested(&windows, now));
        // Tauri hands a tray menu event to two handlers.
        assert_eq!(
            guard.quit_requested(&windows, now + Duration::from_millis(5)),
            Action::Nothing
        );
        let (_, _, _, again) = ask(&guard.quit_requested(&windows, now + Duration::from_secs(2)));
        assert_eq!(again, ping + 1);
    }

    #[test]
    fn a_quit_right_after_a_close_click_still_takes_the_prompt_over() {
        let mut guard = Guard::default();
        guard.report("a", 1);
        let windows = order(&["a"]);
        let now = Instant::now();
        let (_, id, _, _) = ask(&guard.close_requested("a", false, &windows, now).unwrap());
        let (_, same, reason, _) =
            ask(&guard.quit_requested(&windows, now + Duration::from_millis(50)));
        assert_eq!((same, reason), (id, Reason::Quit));
        assert_eq!(guard.reply("a", id, Reply::Proceed, &windows), Action::Exit);
    }

    #[test]
    fn stale_or_foreign_replies_are_ignored() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        guard.report("w", 1);
        let (_, id, _, _) = ask(&close(&mut guard, "w", &mut clock).unwrap());
        assert_eq!(
            guard.reply("w", id + 1, Reply::Proceed, &[]),
            Action::Nothing
        );
        assert_eq!(
            guard.reply("other", id, Reply::Proceed, &[]),
            Action::Nothing
        );
        assert!(close(&mut guard, "w", &mut clock).is_some());
    }

    #[test]
    fn quit_with_nothing_unsaved_exits_at_once() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        guard.report("a", 0);
        assert_eq!(
            guard.quit_requested(&order(&["a", "b"]), clock.tick()),
            Action::Exit
        );
    }

    #[test]
    fn quit_asks_each_unsaved_window_in_turn_then_exits() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        let windows = order(&["b", "a", "c"]);
        guard.report("a", 1);
        guard.report("b", 2);
        guard.report("c", 0);
        let (label, id, reason, _) = ask(&guard.quit_requested(&windows, clock.tick()));
        assert_eq!((label.as_str(), reason), ("b", Reason::Quit));
        let (label, next, reason, _) = ask(&guard.reply("b", id, Reply::Proceed, &windows));
        assert_eq!((label.as_str(), reason), ("a", Reason::Quit));
        assert_eq!(
            guard.reply("a", next, Reply::Proceed, &windows),
            Action::Exit
        );
    }

    #[test]
    fn cancel_in_any_window_abandons_the_quit() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        let windows = order(&["a", "b"]);
        guard.report("a", 1);
        guard.report("b", 1);
        let (_, id, _, _) = ask(&guard.quit_requested(&windows, clock.tick()));
        assert_eq!(
            guard.reply("a", id, Reply::Cancel, &windows),
            Action::Nothing
        );
        // A fresh quit starts over from the first unsaved window.
        let (label, _, _, _) = ask(&guard.quit_requested(&windows, clock.tick()));
        assert_eq!(label, "a");
    }

    #[test]
    fn a_repeated_quit_re_asks_the_current_window() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        let windows = order(&["a", "b"]);
        guard.report("a", 1);
        guard.report("b", 1);
        let (_, id, _, ping) = ask(&guard.quit_requested(&windows, clock.tick()));
        let (label, same, _, again) = ask(&guard.quit_requested(&windows, clock.tick()));
        assert_eq!((label.as_str(), same), ("a", id));
        assert!(again > ping);
    }

    #[test]
    fn a_hung_window_is_skipped_by_the_quit() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        let windows = order(&["a", "b"]);
        guard.report("a", 1);
        guard.report("b", 1);
        let (_, id, _, ping) = ask(&guard.quit_requested(&windows, clock.tick()));
        let (label, next, _, _) = ask(&guard.ack_timeout("a", id, ping, &windows));
        assert_eq!(label, "b");
        assert_eq!(
            guard.reply("b", next, Reply::Proceed, &windows),
            Action::Exit
        );
    }

    #[test]
    fn a_quit_takes_over_a_pending_close_prompt() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        let windows = order(&["a"]);
        guard.report("a", 1);
        let (_, id, _, _) = ask(&close(&mut guard, "a", &mut clock).unwrap());
        let (label, same, reason, _) = ask(&guard.quit_requested(&windows, clock.tick()));
        assert_eq!((label.as_str(), same, reason), ("a", id, Reason::Quit));
        // Settling it serves the quit; the window is not closed first.
        assert_eq!(guard.reply("a", id, Reply::Proceed, &windows), Action::Exit);
        // Clicking close on a quit-prompted window keeps it a quit prompt.
        guard.report("a", 1);
        ask(&guard.quit_requested(&windows, clock.tick()));
        let (_, _, reason, _) = ask(&close(&mut guard, "a", &mut clock).unwrap());
        assert_eq!(reason, Reason::Quit);
    }

    #[test]
    fn a_window_destroyed_mid_quit_moves_the_quit_on() {
        let mut clock = Clock::new();
        let mut guard = Guard::default();
        guard.report("a", 1);
        guard.report("b", 1);
        ask(&guard.quit_requested(&order(&["a", "b"]), clock.tick()));
        let (label, _, _, _) = ask(&guard.forget("a", &order(&["b"])));
        assert_eq!(label, "b");
        assert_eq!(guard.forget("b", &[]), Action::Exit);
        assert_eq!(guard.windows_with_unsaved(), 0);
    }

    #[test]
    fn counts_unsaved_windows_for_the_updater() {
        let mut guard = Guard::default();
        guard.report("a", 3);
        guard.report("b", 0);
        guard.report("c", 1);
        assert_eq!(guard.windows_with_unsaved(), 2);
    }
}
