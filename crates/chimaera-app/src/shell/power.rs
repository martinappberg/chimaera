//! System sleep gives the daemon a bounded opportunity to publish its final
//! snapshot. The OS is always acknowledged, even if the service is offline.
#[cfg(target_os = "macos")]
mod platform {
    use std::{
        ffi::{c_char, c_void},
        sync::{
            atomic::{AtomicU32, Ordering},
            Once,
        },
        time::Duration,
    };
    use tauri::Manager;
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
        fn IOPSCopyPowerSourcesInfo() -> Pointer;
        fn IOPSGetProvidingPowerSourceType(snapshot: Pointer) -> Pointer;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: Pointer;
        fn CFRunLoopGetCurrent() -> Pointer;
        fn CFRunLoopAddSource(runloop: Pointer, source: Pointer, mode: Pointer);
        fn CFRunLoopRun();
        fn CFStringGetCString(
            string: Pointer,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> bool;
        fn CFRelease(value: Pointer);
    }
    struct Context {
        app: tauri::AppHandle,
        connection: AtomicU32,
    }
    extern "C" fn changed(raw: Pointer, _service: u32, message: u32, argument: Pointer) {
        // SAFETY: the dedicated process-lifetime runloop owns this boxed context.
        let context = unsafe { &*(raw as *const Context) };
        let connection = context.connection.load(Ordering::Acquire);
        let notification = argument as isize;
        match message {
            0xe0000270 => unsafe {
                IOAllowPowerChange(connection, notification);
            },
            0xe0000280 => {
                let app = context.app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<super::super::Shell>();
                    // macOS waits up to 30 s for this acknowledgment; keep a
                    // margin and tell the daemon exactly what it has.
                    let budget = Duration::from_secs(25);
                    let _ = tokio::time::timeout(budget, async {
                        if state.pro.client_now().is_some() {
                            let _ = super::super::pro::daemon_request(
                                &state,
                                "POST",
                                "/pro/sleep",
                                Some(super::sleep_body(budget)),
                            )
                            .await;
                        }
                    })
                    .await;
                    unsafe {
                        IOAllowPowerChange(connection, notification);
                    }
                });
            }
            0xe0000300 => {
                let app = context.app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<super::super::Shell>();
                    if state.pro.client_now().is_some() {
                        let _ =
                            super::super::pro::daemon_request(&state, "POST", "/pro/wake", None)
                                .await;
                    }
                });
            }
            _ => {}
        }
    }
    pub(super) fn install(app: &tauri::AppHandle) {
        static STARTED: Once = Once::new();
        STARTED.call_once(|| {
            let app = app.clone();
            std::thread::spawn(move || {
                let mut context = Box::new(Context {
                    app,
                    connection: AtomicU32::new(0),
                });
                let mut port = std::ptr::null_mut();
                let mut notifier = 0;
                // SAFETY: signatures match the system SDK; context remains pinned
                // in this thread until its CF runloop ends at process exit.
                unsafe {
                    let connection = IORegisterForSystemPower(
                        (&mut *context as *mut Context).cast(),
                        &mut port,
                        changed,
                        &mut notifier,
                    );
                    if connection == 0 || port.is_null() {
                        return;
                    }
                    context.connection.store(connection, Ordering::Release);
                    CFRunLoopAddSource(
                        CFRunLoopGetCurrent(),
                        IONotificationPortGetRunLoopSource(port),
                        kCFRunLoopDefaultMode,
                    );
                    CFRunLoopRun();
                }
            });
        });
    }
    pub(super) fn suitable() -> bool {
        unsafe {
            let snapshot = IOPSCopyPowerSourcesInfo();
            if snapshot.is_null() {
                return false;
            }
            let source = IOPSGetProvidingPowerSourceType(snapshot);
            let mut buffer = [0i8; 64];
            let copied = !source.is_null()
                && CFStringGetCString(
                    source,
                    buffer.as_mut_ptr(),
                    buffer.len() as isize,
                    0x08000100,
                );
            CFRelease(snapshot);
            copied && std::ffi::CStr::from_ptr(buffer.as_ptr()).to_bytes() == b"AC Power"
        }
    }
}
#[cfg(target_os = "linux")]
mod platform {
    use dbus::{arg::OwnedFd, blocking::Connection, message::MatchRule};
    use std::{
        sync::{mpsc, Once},
        time::Duration,
    };

    fn inhibit(connection: &Connection) -> Result<OwnedFd, dbus::Error> {
        let proxy = connection.with_proxy(
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            Duration::from_secs(2),
        );
        let (fd,): (OwnedFd,) = proxy.method_call(
            "org.freedesktop.login1.Manager",
            "Inhibit",
            ("sleep", "chimaera", "Save workspace handoff", "delay"),
        )?;
        Ok(fd)
    }
    /// logind's configured ceiling for delay inhibitors (InhibitDelayMaxSec,
    /// 5 s by default). The daemon gets exactly this budget, not a guess.
    fn delay_budget(connection: &Connection) -> Duration {
        use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
        let proxy = connection.with_proxy(
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            Duration::from_secs(2),
        );
        let micros: Result<u64, dbus::Error> =
            proxy.get("org.freedesktop.login1.Manager", "InhibitDelayMaxUSec");
        super::logind_budget(micros.ok())
    }
    fn watch(app: &tauri::AppHandle) -> Result<(), dbus::Error> {
        let connection = Connection::new_system()?;
        let budget = delay_budget(&connection);
        // Holding a delay inhibitor before subscribing closes the race where
        // logind suspends before our PrepareForSleep callback can run.
        let mut inhibitor = Some(inhibit(&connection)?);
        let (send, receive) = mpsc::sync_channel(4);
        let mut rule = MatchRule::new_signal("org.freedesktop.login1.Manager", "PrepareForSleep");
        rule.sender = Some("org.freedesktop.login1".into());
        rule.path = Some("/org/freedesktop/login1".into());
        connection.add_match(rule, move |(sleeping,): (bool,), _, _| {
            let _ = send.try_send(sleeping);
            true
        })?;
        loop {
            connection.process(Duration::from_secs(1))?;
            while let Ok(sleeping) = receive.try_recv() {
                if sleeping {
                    // logind independently enforces its delay ceiling (normally
                    // five seconds); the app always releases its descriptor.
                    super::notify(app, "/pro/sleep", budget);
                    inhibitor.take();
                } else {
                    inhibitor = Some(inhibit(&connection)?);
                    super::notify(app, "/pro/wake", Duration::from_secs(4));
                }
            }
        }
    }
    pub(super) fn install(app: &tauri::AppHandle) {
        static STARTED: Once = Once::new();
        STARTED.call_once(|| {
            let app = app.clone();
            std::thread::spawn(move || loop {
                if watch(&app).is_err() {
                    tracing::debug!("system sleep monitor unavailable; retrying");
                }
                // Missing logind, policy denials and bus restarts must not
                // become busy loops or interrupt otherwise working sessions.
                std::thread::sleep(Duration::from_secs(30));
            });
        });
    }
    pub(super) fn suitable() -> bool {
        let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
            return false;
        };
        let mut battery = false;
        for entry in entries.flatten().take(32) {
            let kind = std::fs::read_to_string(entry.path().join("type")).unwrap_or_default();
            if kind.trim() == "Battery" {
                battery = true;
            }
            if kind.trim() == "Mains"
                && std::fs::read_to_string(entry.path().join("online"))
                    .is_ok_and(|value| value.trim() == "1")
            {
                return true;
            }
        }
        !battery
    }
}
#[cfg(target_os = "windows")]
mod platform {
    use std::{ffi::c_void, sync::Once, time::Duration};
    use windows_sys::Win32::{
        System::Power::{
            GetSystemPowerStatus, PowerRegisterSuspendResumeNotification,
            DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, SYSTEM_POWER_STATUS,
        },
        UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND},
    };
    unsafe extern "system" fn changed(context: *const c_void, event: u32, _: *const c_void) -> u32 {
        // SAFETY: the registration thread keeps this boxed AppHandle alive for
        // the entire process lifetime, including all callback invocations.
        let app = unsafe { &*context.cast::<tauri::AppHandle>() };
        match event {
            // Windows gives suspend handlers a short best-effort window. The
            // callback cannot veto sleep or wait for an unbounded network call.
            PBT_APMSUSPEND => super::notify(app, "/pro/sleep", Duration::from_millis(1200)),
            PBT_APMRESUMEAUTOMATIC => super::notify(app, "/pro/wake", Duration::from_millis(1200)),
            _ => {}
        }
        0
    }
    pub(super) fn install(app: &tauri::AppHandle) {
        static STARTED: Once = Once::new();
        STARTED.call_once(|| {
            let app = app.clone();
            std::thread::spawn(move || {
                let mut app = Box::new(app);
                let mut parameters = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                    Callback: Some(changed),
                    Context: (&mut *app as *mut tauri::AppHandle).cast(),
                };
                let mut registration = std::ptr::null_mut();
                // SAFETY: the callback ABI matches the SDK and both pointers
                // remain valid in this parked process-lifetime thread.
                let result = unsafe {
                    PowerRegisterSuspendResumeNotification(
                        DEVICE_NOTIFY_CALLBACK,
                        (&mut parameters as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS).cast(),
                        &mut registration,
                    )
                };
                if result != 0 {
                    tracing::warn!(code = result, "system sleep monitor unavailable");
                    return;
                }
                loop {
                    std::thread::park();
                }
            });
        });
    }
    pub(super) fn suitable() -> bool {
        let mut status = SYSTEM_POWER_STATUS::default();
        // Unknown AC status (255) does not trigger automatic hand-back.
        unsafe { GetSystemPowerStatus(&mut status) != 0 && status.ACLineStatus == 1 }
    }
}
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn notify(app: &tauri::AppHandle, route: &'static str, budget: std::time::Duration) {
    use tauri::Manager;
    let app = app.clone();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    tauri::async_runtime::spawn(async move {
        let _ = tokio::time::timeout(budget, async {
            let state = app.state::<super::Shell>();
            if state.pro.client_now().is_some() {
                let body = (route == "/pro/sleep").then(|| sleep_body(budget));
                let _ = super::pro::daemon_request(&state, "POST", route, body).await;
            }
        })
        .await;
        let _ = send.send(());
    });
    let _ = receive.recv_timeout(budget);
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod platform {
    pub(super) fn install(_app: &tauri::AppHandle) {}
    pub(super) fn suitable() -> bool {
        false
    }
}
/// What the daemon may spend before the OS sleeps: the platform's real
/// budget minus a margin for the answer and the OS acknowledgment, so the
/// daemon releases its projects cleanly instead of being frozen mid-save.
/// (Additive: an older daemon ignores the body and uses its own pacing.)
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
    allow(dead_code)
)]
fn sleep_body(budget: std::time::Duration) -> serde_json::Value {
    let margin = std::time::Duration::from_secs(2).min(budget / 4);
    serde_json::json!({ "deadline_ms": budget.saturating_sub(margin).as_millis() as u64 })
}

/// logind reports its delay ceiling in microseconds; an unreadable value
/// falls back to logind's documented default. Never beyond what the shell
/// itself waits for.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn logind_budget(micros: Option<u64>) -> std::time::Duration {
    micros
        .map(std::time::Duration::from_micros)
        .unwrap_or(std::time::Duration::from_secs(5))
        .clamp(
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(25),
        )
}

pub(super) fn install(app: &tauri::AppHandle) {
    platform::install(app);
}
pub(super) fn suitable() -> bool {
    platform::suitable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn the_daemon_is_told_the_real_budget_minus_a_margin() {
        for (budget, expected) in [
            (Duration::from_secs(25), 23_000),
            (Duration::from_secs(5), 3_750),
            (Duration::from_millis(1200), 900),
        ] {
            assert_eq!(sleep_body(budget)["deadline_ms"], expected);
        }
        assert_eq!(logind_budget(Some(5_000_000)), Duration::from_secs(5));
        assert_eq!(logind_budget(None), Duration::from_secs(5));
        assert_eq!(logind_budget(Some(0)), Duration::from_secs(1));
        assert_eq!(logind_budget(Some(u64::MAX)), Duration::from_secs(25));
    }
}
