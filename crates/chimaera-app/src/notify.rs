//! The platform half of notifications: post an OS notification, take one
//! back, route a click, badge and bounce the Dock. What to post — and whether
//! the user is already looking — is decided in `shell::notices`; this module
//! only speaks each platform's notification API.
//!
//! - **macOS** — `UNUserNotificationCenter` (the supported API: click
//!   responses, banners while Chimaera is frontmost, per-workspace grouping,
//!   removal of stale alerts). It raises an Objective-C exception when the
//!   process isn't running from an `.app` bundle, so an unbundled `cargo run`
//!   degrades to no notifications instead of crashing (see [`available`]).
//!   The Dock badge/bounce go straight to `NSApp`'s dock tile rather than
//!   through a window, so they don't depend on which windows exist.
//! - **Linux / Windows** — `notify-rust` (freedesktop D-Bus / WinRT toasts);
//!   a bounded number of shown notifications wait for their click.
//!
//! A notification's identifier carries its route (host, workspace, session),
//! so a click on an alert posted by an earlier run of the app still lands.

use tauri::AppHandle;

/// Where a notification's click should take the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Route {
    /// The window scope's host: `None` = the local daemon.
    pub(crate) alias: Option<String>,
    pub(crate) ws: Option<String>,
    pub(crate) session: String,
}

/// One notification to post.
#[derive(Clone, Debug)]
pub(crate) struct Toast {
    /// Unique per notice; built by [`toast_id`] so it round-trips the route.
    pub(crate) id: String,
    /// Grouping key (Notification Center stacks a workspace's alerts).
    pub(crate) thread: String,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) body: String,
    pub(crate) sound: bool,
}

/// Field separator inside identifiers: ASCII unit separator, which no ssh
/// alias, workspace id, or session id can contain.
const SEP: char = '\u{1f}';
const ID_PREFIX: &str = "chimaera";

/// The identifier for one notice, encoding its click route.
pub(crate) fn toast_id(route: &Route, boot: &str, notice_id: u64) -> String {
    format!(
        "{ID_PREFIX}{SEP}{}{SEP}{}{SEP}{}{SEP}{boot}-{notice_id}",
        route.alias.as_deref().unwrap_or(""),
        route.ws.as_deref().unwrap_or(""),
        route.session,
    )
}

/// Recover the route from an identifier made by [`toast_id`].
pub(crate) fn parse_route(id: &str) -> Option<Route> {
    let mut parts = id.split(SEP);
    if parts.next()? != ID_PREFIX {
        return None;
    }
    let alias = parts.next()?;
    let ws = parts.next()?;
    let session = parts.next()?;
    parts.next()?;
    if session.is_empty() {
        return None;
    }
    let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
    Some(Route {
        alias: opt(alias),
        ws: opt(ws),
        session: session.to_string(),
    })
}

/// What the OS currently allows, for the settings page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Permission {
    Granted,
    Denied,
    /// Never asked — the first notification (or the settings button) asks.
    NotDetermined,
    /// No notification service here (an unbundled dev build, no D-Bus).
    Unsupported,
}

/// A click reached us: hand the route to the shell (which owns windows).
fn clicked(app: &AppHandle, id: &str) {
    match parse_route(id) {
        Some(route) => crate::shell::notices::route_click(app, route),
        // Not ours (or unparseable): still bring the app forward.
        None => crate::shell::activate_app(app, false),
    }
}

pub(crate) use platform::{
    bounce, init, open_settings, permission, post, remove, request_permission, set_badge,
};

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::{Mutex, OnceLock};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
    use objc2_foundation::{NSArray, NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
        UNNotification, UNNotificationPresentationOptions, UNNotificationRequest,
        UNNotificationResponse, UNNotificationSettings, UNNotificationSound,
        UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use tauri::AppHandle;

    use super::{Permission, Toast};

    static APP: OnceLock<AppHandle> = OnceLock::new();

    define_class!(
        // SAFETY: NSObject has no subclassing requirements and this class
        // holds no ivars and no Drop.
        #[unsafe(super(NSObject))]
        #[name = "ChimaeraNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            // Present banners even while Chimaera is frontmost: the shell has
            // already dropped alerts for what the user is looking at, so
            // anything that reaches here is about another tab or window.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                handler.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List
                    | UNNotificationPresentationOptions::Sound,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                handler: &block2::DynBlock<dyn Fn()>,
            ) {
                let id = response.notification().request().identifier().to_string();
                let dismissed = response.actionIdentifier().to_string()
                    == "com.apple.UNNotificationDismissActionIdentifier";
                if !dismissed {
                    if let Some(app) = APP.get() {
                        super::clicked(app, &id);
                    }
                }
                handler.call(());
            }
        }
    );

    /// UNUserNotificationCenter throws unless the process runs from a real
    /// `.app` bundle with an identifier (the released app, and the isolated
    /// dev app's generated wrapper bundle). A bare `cargo run` binary has
    /// neither — notifications are then simply unavailable.
    pub(crate) fn available() -> bool {
        static OK: OnceLock<bool> = OnceLock::new();
        *OK.get_or_init(|| {
            let bundle = NSBundle::mainBundle();
            bundle.bundleIdentifier().is_some() && bundle.bundlePath().to_string().ends_with(".app")
        })
    }

    fn center() -> Option<Retained<UNUserNotificationCenter>> {
        available().then(UNUserNotificationCenter::currentNotificationCenter)
    }

    /// Install the click delegate. Must run at launch, before the app
    /// finishes launching, so a click that LAUNCHED the app is delivered.
    pub(crate) fn init(app: &AppHandle) {
        let _ = APP.set(app.clone());
        let Some(center) = center() else {
            tracing::info!("notifications unavailable: not running from an .app bundle");
            return;
        };
        let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(), init] };
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center holds its delegate weakly; this one lives as long as
        // the process.
        std::mem::forget(delegate);
    }

    fn options() -> UNAuthorizationOptions {
        UNAuthorizationOptions::Alert
            | UNAuthorizationOptions::Sound
            | UNAuthorizationOptions::Badge
    }

    /// Post a notification. Authorization is requested on the way (the
    /// system prompt appears once, on the first notification ever; later
    /// calls return the stored decision at once).
    pub(crate) fn post(toast: Toast) {
        let Some(center) = center() else {
            return;
        };
        let block = RcBlock::new(move |granted: Bool, err: *mut NSError| {
            if !granted.as_bool() {
                // Once per process: the user said no (or hasn't answered);
                // the settings page carries the way back.
                static SAID: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    // SAFETY: null or a valid NSError for the call.
                    let why = unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string());
                    tracing::info!(?why, "notifications are not authorized; dropping alerts");
                }
                return;
            }
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&toast.title));
            if !toast.subtitle.is_empty() {
                content.setSubtitle(&NSString::from_str(&toast.subtitle));
            }
            content.setBody(&NSString::from_str(&toast.body));
            content.setThreadIdentifier(&NSString::from_str(&toast.thread));
            if toast.sound {
                content.setSound(Some(&UNNotificationSound::defaultSound()));
            }
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(&toast.id),
                &content,
                None,
            );
            UNUserNotificationCenter::currentNotificationCenter()
                .addNotificationRequest_withCompletionHandler(
                    &request,
                    Some(&RcBlock::new(|err: *mut NSError| {
                        // SAFETY: null or a valid NSError for the call.
                        if let Some(err) = unsafe { err.as_ref() } {
                            tracing::warn!("notification not delivered: {err:?}");
                        }
                    })),
                );
        });
        center.requestAuthorizationWithOptions_completionHandler(options(), &block);
    }

    /// Take delivered notifications back out of Notification Center.
    pub(crate) fn remove(ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let Some(center) = center() else {
            return;
        };
        let ids: Vec<Retained<NSString>> = ids.iter().map(|id| NSString::from_str(id)).collect();
        center.removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(&ids));
    }

    /// The current authorization, without prompting.
    pub(crate) async fn permission() -> Permission {
        // The Objective-C half stays synchronous: its objects are not Send,
        // and a Tauri command's future must be.
        match query_permission() {
            Some(rx) => rx.await.unwrap_or(Permission::Unsupported),
            None => Permission::Unsupported,
        }
    }

    fn query_permission() -> Option<tokio::sync::oneshot::Receiver<Permission>> {
        let center = center()?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Mutex::new(Some(tx));
        let block = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
            // SAFETY: the center hands the block a valid settings object
            // for the duration of the call.
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            let answer = if status == UNAuthorizationStatus::NotDetermined {
                Permission::NotDetermined
            } else if status == UNAuthorizationStatus::Denied {
                Permission::Denied
            } else {
                Permission::Granted
            };
            if let Some(tx) = crate::shell::lock(&tx).take() {
                let _ = tx.send(answer);
            }
        });
        center.getNotificationSettingsWithCompletionHandler(&block);
        Some(rx)
    }

    /// Ask for permission now (the system prompt, if never answered).
    pub(crate) async fn request_permission() -> Permission {
        let Some(rx) = ask_permission() else {
            return Permission::Unsupported;
        };
        let _ = rx.await;
        permission().await
    }

    fn ask_permission() -> Option<tokio::sync::oneshot::Receiver<()>> {
        let center = center()?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Mutex::new(Some(tx));
        let block = RcBlock::new(move |_granted: Bool, _err: *mut NSError| {
            if let Some(tx) = crate::shell::lock(&tx).take() {
                let _ = tx.send(());
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(options(), &block);
        Some(rx)
    }

    /// The app's page in System Settings → Notifications.
    pub(crate) fn open_settings() {
        let id = NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let url =
            format!("x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={id}");
        if let Err(e) = open::that_detached(url) {
            tracing::warn!("could not open notification settings: {e}");
        }
    }

    /// The Dock tile's badge: the count of agents waiting on the user.
    pub(crate) fn set_badge(app: &AppHandle, count: usize) {
        let _ = app.run_on_main_thread(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
            NSApplication::sharedApplication(mtm)
                .dockTile()
                .setBadgeLabel(label.as_deref());
        });
    }

    /// Bounce the Dock icon once (a no-op while Chimaera is frontmost).
    pub(crate) fn bounce(app: &AppHandle) {
        let _ = app.run_on_main_thread(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            NSApplication::sharedApplication(mtm)
                .requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
        });
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;

    use tauri::{AppHandle, Manager};

    use super::{Permission, Toast};

    static APP: OnceLock<AppHandle> = OnceLock::new();
    /// Shown notifications still waiting for a click. Each wait parks a
    /// thread until the notification closes, and some notification servers
    /// keep alerts until dismissed — past this many, new ones show without a
    /// click route rather than growing threads without bound.
    static WAITING: AtomicUsize = AtomicUsize::new(0);
    const MAX_WAITING: usize = 8;

    pub(crate) fn init(app: &AppHandle) {
        let _ = APP.set(app.clone());
    }

    pub(crate) fn post(toast: Toast) {
        std::thread::spawn(move || {
            // No subtitle line on these platforms: it leads the body.
            let body = match (toast.subtitle.is_empty(), toast.body.is_empty()) {
                (false, false) => format!("{}\n{}", toast.subtitle, toast.body),
                (false, true) => toast.subtitle.clone(),
                _ => toast.body.clone(),
            };
            let mut n = notify_rust::Notification::new();
            n.appname("Chimaera").summary(&toast.title).body(&body);
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                n.action("default", "Open");
                if !toast.sound {
                    n.hint(notify_rust::Hint::SuppressSound(true));
                }
            }
            let handle = match n.show() {
                Ok(handle) => handle,
                Err(e) => {
                    tracing::warn!("could not show a notification: {e}");
                    return;
                }
            };
            if WAITING.fetch_add(1, Ordering::Relaxed) >= MAX_WAITING {
                WAITING.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            let id = toast.id;
            handle.wait_for_action(|action| {
                if action != "__closed" {
                    if let Some(app) = APP.get() {
                        super::clicked(app, &id);
                    }
                }
            });
            WAITING.fetch_sub(1, Ordering::Relaxed);
        });
    }

    /// Not supported here: freedesktop servers own their history.
    pub(crate) fn remove(_ids: Vec<String>) {}

    pub(crate) async fn permission() -> Permission {
        Permission::Granted
    }

    pub(crate) async fn request_permission() -> Permission {
        Permission::Granted
    }

    pub(crate) fn open_settings() {}

    /// The launcher badge (Unity launcher API on Linux, the taskbar overlay
    /// on Windows) hangs off a window; with none open there is nothing to
    /// badge — and closing the last window exits the app here anyway.
    pub(crate) fn set_badge(app: &AppHandle, count: usize) {
        let count = (count > 0).then_some(count as i64);
        for window in app.webview_windows().values() {
            let _ = window.set_badge_count(count);
        }
    }

    pub(crate) fn bounce(app: &AppHandle) {
        if let Some(window) = app.webview_windows().values().next() {
            let _ = window.request_user_attention(Some(tauri::UserAttentionType::Informational));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_round_trips_through_the_identifier() {
        let remote = Route {
            alias: Some("Sherlock".into()),
            ws: Some("w-1".into()),
            session: "s-abc".into(),
        };
        let id = toast_id(&remote, "boot1", 7);
        assert_eq!(parse_route(&id), Some(remote));

        let local = Route {
            alias: None,
            ws: None,
            session: "s-x".into(),
        };
        assert_eq!(parse_route(&toast_id(&local, "b", 1)), Some(local));

        assert_eq!(parse_route("something-else"), None);
        assert_eq!(parse_route(&format!("{ID_PREFIX}{SEP}a{SEP}b")), None);
    }
}
