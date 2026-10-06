//! The daemon hears system sleep itself, so closing the lid hands work over
//! cleanly whether or not the app is open (macOS only).
//!
//! Just before the computer sleeps, macOS waits up to about 30 seconds for an
//! acknowledgment: the daemon hands its working projects to the cloud within
//! that budget (`routes::sleeping`: the same conversation continues there on
//! current files, no fork) and then always acknowledges. On wake it lets the
//! lease loop verify who holds each project (`routes::woke`). Other platforms
//! rely on the lease lapsing: Linux would need D-Bus in a daemon that must stay
//! lean, and on Windows the daemon runs inside WSL, which never sees host
//! power events.
//!
//! `IORegisterForSystemPower` needs only a CoreFoundation run loop on a
//! dedicated thread, not a window. It starts once per process, and only for a
//! personal computer configured for Pro with the Runtime composed in; a free
//! daemon never registers.
use crate::AppState;
use std::sync::{Arc, Weak};

/// What the flush may spend before acknowledging: macOS allows about 30 s;
/// the rest is margin for the acknowledgment itself.
pub(super) const BUDGET: std::time::Duration = std::time::Duration::from_secs(23);

/// The power messages this watcher acts on (IOKit `IOMessage` values).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Event {
    /// `kIOMessageCanSystemSleep`: idle sleep may proceed (always allowed).
    CanSleep,
    /// `kIOMessageSystemWillSleep`: hand over, then acknowledge.
    WillSleep,
    /// `kIOMessageSystemHasPoweredOn`.
    PoweredOn,
}
pub(super) fn event(message: u32) -> Option<Event> {
    match message {
        0xe000_0270 => Some(Event::CanSleep),
        0xe000_0280 => Some(Event::WillSleep),
        0xe000_0300 => Some(Event::PoweredOn),
        _ => None,
    }
}

/// What one event does, with the acknowledgment passed in: a will-sleep
/// acknowledges after the flush whatever it found (an OS waiting on a daemon
/// that cannot reach the account must still sleep), a can-sleep at once.
pub(super) async fn handle(state: Weak<AppState>, event: Event, acknowledge: impl FnOnce()) {
    match event {
        Event::CanSleep => acknowledge(),
        Event::WillSleep => {
            if let Some(state) = state.upgrade() {
                let flush = super::routes::sleeping(&state, BUDGET);
                let _ = tokio::time::timeout(BUDGET, flush).await;
            }
            acknowledge();
        }
        Event::PoweredOn => {
            if let Some(state) = state.upgrade() {
                super::routes::woke(&state).await;
            }
        }
    }
}

/// Starts the watcher for this process if this daemon may hand work over.
pub(super) fn start(state: &Arc<AppState>) {
    if state.daemon_extension.is_none() {
        return;
    }
    #[cfg(target_os = "macos")]
    platform::start(state);
}

#[cfg(target_os = "macos")]
mod platform {
    use crate::AppState;
    use std::{
        ffi::c_void,
        sync::{
            atomic::{AtomicU32, Ordering},
            Arc, Once, Weak,
        },
    };
    type Pointer = *mut c_void;
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IORegisterForSystemPower(
            context: Pointer,
            port: *mut Pointer,
            callback: extern "C" fn(Pointer, u32, u32, Pointer),
            notifier: *mut u32,
        ) -> u32;
        fn IONotificationPortGetRunLoopSource(port: Pointer) -> Pointer;
        fn IOAllowPowerChange(connection: u32, notification: isize) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: Pointer;
        fn CFRunLoopGetCurrent() -> Pointer;
        fn CFRunLoopAddSource(runloop: Pointer, source: Pointer, mode: Pointer);
        fn CFRunLoopRun();
    }
    struct Context {
        state: Weak<AppState>,
        runtime: tokio::runtime::Handle,
        connection: AtomicU32,
    }
    extern "C" fn changed(raw: Pointer, _service: u32, message: u32, argument: Pointer) {
        // SAFETY: the dedicated process-lifetime run loop owns this boxed context.
        let context = unsafe { &*(raw as *const Context) };
        let Some(event) = super::event(message) else {
            return;
        };
        let connection = context.connection.load(Ordering::Acquire);
        let notification = argument as isize;
        let acknowledge = move || {
            // SAFETY: the connection and notification are the ones IOKit
            // delivered with this message; acknowledging twice is harmless.
            unsafe {
                IOAllowPowerChange(connection, notification);
            }
        };
        let state = context.state.clone();
        context
            .runtime
            .spawn(async move { super::handle(state, event, acknowledge).await });
    }
    pub(super) fn start(state: &Arc<AppState>) {
        static STARTED: Once = Once::new();
        let state = Arc::downgrade(state);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        STARTED.call_once(move || {
            std::thread::spawn(move || {
                let mut context = Box::new(Context {
                    state,
                    runtime,
                    connection: AtomicU32::new(0),
                });
                let mut port = std::ptr::null_mut();
                let mut notifier = 0;
                // SAFETY: signatures match the system SDK; the context stays
                // pinned in this thread until its run loop ends at exit.
                unsafe {
                    let connection = IORegisterForSystemPower(
                        (&mut *context as *mut Context).cast(),
                        &mut port,
                        changed,
                        &mut notifier,
                    );
                    if connection == 0 || port.is_null() {
                        tracing::warn!("Could not register for system sleep notices");
                        return;
                    }
                    context.connection.store(connection, Ordering::Release);
                    CFRunLoopAddSource(
                        CFRunLoopGetCurrent(),
                        IONotificationPortGetRunLoopSource(port),
                        kCFRunLoopDefaultMode,
                    );
                    tracing::info!(
                        target: "chimaera_server::pro::sleep_watch",
                        "listening for system sleep"
                    );
                    CFRunLoopRun();
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_power_messages_are_acted_on() {
        assert_eq!(event(0xe000_0270), Some(Event::CanSleep));
        assert_eq!(event(0xe000_0280), Some(Event::WillSleep));
        assert_eq!(event(0xe000_0300), Some(Event::PoweredOn));
        assert_eq!(event(0xe000_0320), None);
    }

    #[tokio::test]
    async fn sleep_is_always_acknowledged_even_without_a_daemon_or_account() {
        let gone: Weak<AppState> = Weak::new();
        let (sent, mut heard) = tokio::sync::oneshot::channel();
        handle(gone, Event::WillSleep, move || {
            let _ = sent.send(());
        })
        .await;
        assert!(heard.try_recv().is_ok());
        let (sent, mut heard) = tokio::sync::oneshot::channel();
        handle(Weak::new(), Event::CanSleep, move || {
            let _ = sent.send(());
        })
        .await;
        assert!(heard.try_recv().is_ok());
    }
}
