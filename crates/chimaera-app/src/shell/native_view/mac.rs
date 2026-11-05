//! macOS pane views: a `WKWebView` the shell creates itself and adds as a
//! subview of its owner window's web view, laid over the pane's rect.
//!
//! Not a Tauri child web view, on purpose: Tauri treats a window as a
//! "webview window" only while every web view in it carries the window's
//! label (`Window::is_webview_window`), so a child made `get_webview_window`
//! — and every `deliver_to`, prompt and window lookup built on it — miss
//! that window. A plain subview is invisible to Tauri. A `WKWebView` with a
//! configuration of its own still gets a web process of its own (measured:
//! window ~40 MB, report ~950 MB).
//!
//! The pane bridge (`chimaera_core::PANE_BRIDGE_JS`) is a user script in
//! the `chimaera` content world with a `chimaeraPaneView` message handler in
//! the same world, both added before the first load: the page's own scripts
//! can't see the handler, the bridge's listeners or its `__chimaeraPaneView`
//! object. Each message becomes a `native-view` event on the owner only
//! (`deliver_to`). The navigation policy (`super::main_frame_policy`) runs in
//! the view's navigation delegate; popups go through its UI delegate.
//!
//! Each `WKWebView` sits in a clip box of its own (a plain `NSView` that
//! clips to its bounds), the box being the owner's subview: an embed card
//! half scrolled out of its transcript is cropped to what is still on screen
//! (`super::clip_box`) while its page keeps its place. A passive view's wheel
//! goes to the owner instead (`wheel_monitor`): WebKit never chains a wheel
//! out of a view the way it chains one out of an iframe, so a transcript
//! would otherwise stick at every card it scrolls past.
//!
//! Every `WKWebView` touch is on the main thread: views live in a
//! main-thread registry, reached through `run_on_main_thread`, and the async
//! commands wait on a oneshot for a completion handler's answer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBitmapImageFileType, NSBitmapImageRep, NSEvent, NSEventMask, NSImage,
    NSResponder, NSView,
};
use objc2_foundation::{
    NSDictionary, NSError, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    NSURLRequest, NSURL,
};
use objc2_web_kit::{
    WKContentWorld, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKScriptMessage, WKScriptMessageHandler, WKUIDelegate, WKUserContentController, WKUserScript,
    WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration, WKWindowFeatures,
};
use serde::Serialize;
use tauri::{AppHandle, Webview};
use tokio::sync::oneshot;

use super::{NavAction, Policy, ViewRect};

/// The content world the bridge lives in.
const WORLD: &str = "chimaera";
/// The message handler's name inside that world.
const HANDLER: &str = "chimaeraPaneView";
/// A message larger than this is dropped (the bridge caps its own text far
/// below it; this bounds what a misbehaving page could make us parse).
const MESSAGE_MAX: usize = 256 * 1024;

static APP: OnceLock<AppHandle> = OnceLock::new();

/// A view whose own process dies this many times within `CRASH_WINDOW` is
/// left down (and the page told) instead of reloaded again: a page over
/// WebKit's per-process limit would otherwise crash, re-download and crash.
const CRASH_LIMIT: usize = 2;
const CRASH_WINDOW: Duration = Duration::from_secs(60);

/// One live view, the delegate it holds only weakly, and what the bridge
/// and WebKit told the shell about it.
struct Pane {
    wk: Retained<WKWebView>,
    /// The clip box `wk` sits in (the owner's subview).
    clip: Retained<NSView>,
    /// The wheel over it goes to the owner page (an embed card nobody has
    /// clicked into yet).
    passive: bool,
    _delegate: Retained<Delegate>,
    /// The last link the user clicked (trusted, reported by the bridge).
    link: Option<(String, Instant)>,
    /// When its web process died, newest last, within `CRASH_WINDOW`.
    crashes: Vec<Instant>,
}

thread_local! {
    /// Main thread only: child label → its view.
    static PANES: RefCell<HashMap<String, Pane>> = RefCell::new(HashMap::new());
    /// The wheel monitor is installed (once, for the app's life).
    static WHEEL_MONITOR: Cell<bool> = const { Cell::new(false) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ChimaeraPaneViewBridge"]
    #[ivars = String]
    /// Receives the bridge's messages for one view (ivar: its label).
    struct Bridge;

    unsafe impl NSObjectProtocol for Bridge {}

    unsafe impl WKScriptMessageHandler for Bridge {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _controller: &WKUserContentController, message: &WKScriptMessage) {
            // A panic must not unwind into WebKit.
            let _ = catch_unwind(AssertUnwindSafe(|| forward(self.ivars(), message)));
        }
    }
);

impl Bridge {
    fn new(mtm: MainThreadMarker, label: String) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(label);
        // SAFETY: NSObject's designated initializer on a freshly allocated
        // instance whose ivars are set.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ChimaeraPaneViewDelegate"]
    #[ivars = String]
    /// Navigation and UI delegate for one view (ivar: its label).
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl WKNavigationDelegate for Delegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(
            &self,
            _wk: &WKWebView,
            action: &WKNavigationAction,
            handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let allow =
                catch_unwind(AssertUnwindSafe(|| decide(self.ivars(), action))).unwrap_or(false);
            handler.call((if allow {
                WKNavigationActionPolicy::Allow
            } else {
                WKNavigationActionPolicy::Cancel
            },));
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, wk: &WKWebView) {
            // The page's own process died (WebKit's memory limit on a giant
            // page, a crash); nothing in the window was touched. Load it
            // again — but not forever.
            let label = self.ivars();
            let now = Instant::now();
            let again = PANES.with(|panes| {
                let mut panes = panes.borrow_mut();
                let pane = panes.get_mut(label)?;
                pane.crashes
                    .retain(|t| now.duration_since(*t) < CRASH_WINDOW);
                pane.crashes.push(now);
                Some(pane.crashes.len() <= CRASH_LIMIT)
            });
            if again == Some(true) {
                tracing::warn!(%label, "pane view: its web process ended; reloading");
                // SAFETY: a live WKWebView on the main thread.
                let _ = unsafe { wk.reload() };
            } else if again == Some(false) {
                tracing::warn!(%label, "pane view: its web process keeps ending; leaving it down");
                notify(label, serde_json::json!({"kind": "crashed"}));
            }
        }
    }

    unsafe impl WKUIDelegate for Delegate {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            _wk: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            // `target="_blank"` and `window.open`: never a new view. A click
            // on a web link goes to the user's browser; a same-app page
            // opens in this view.
            let _ = catch_unwind(AssertUnwindSafe(|| popup(self.ivars(), action)));
            None
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, label: String) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(label);
        // SAFETY: as for `Bridge::new`.
        unsafe { msg_send![super(this), init] }
    }
}

/// The URL an action asks for, parsed.
fn action_url(action: &WKNavigationAction) -> Option<tauri::Url> {
    // SAFETY: WKNavigationAction accessors on the main thread.
    let url = unsafe { action.request().URL() }?;
    url.absoluteString()?.to_string().parse().ok()
}

/// Whether `url` is the link the user just clicked in view `label` (the
/// bridge reports trusted clicks; WebKit calls a script's `a.click()` a
/// link activation too, so `navigationType` alone can't tell). Consumed: one
/// click lets one URL out.
fn clicked(label: &str, url: &tauri::Url) -> bool {
    PANES.with(|panes| {
        let mut panes = panes.borrow_mut();
        let Some(pane) = panes.get_mut(label) else {
            return false;
        };
        match pane.link.take() {
            Some((href, at)) => super::link_lets_out(&href, at, Instant::now(), url),
            None => false,
        }
    })
}

fn decide(label: &str, action: &WKNavigationAction) -> bool {
    let Some(app) = APP.get() else { return false };
    // SAFETY: as above.
    let main_frame = unsafe { action.targetFrame() }.is_some_and(|f| unsafe { f.isMainFrame() });
    let Some(url) = action_url(action) else {
        return false;
    };
    // A frame inside the page loads as it would in an iframe: the page is
    // already sandboxed, and its own frames are its business.
    if !main_frame {
        return url.scheme() != "file";
    }
    match super::main_frame_policy(app, label, &url) {
        Policy::Allow => true,
        Policy::Rescue(rescued) => {
            // Re-issued after this decision returns: loading from inside
            // WebKit's policy callback would re-enter it.
            let label = label.to_string();
            let _ = app.run_on_main_thread(move || load(&label, &rescued));
            false
        }
        Policy::HandOff(out) => {
            if clicked(label, &out) {
                super::hand_off(&out);
            }
            false
        }
    }
}

fn popup(label: &str, action: &WKNavigationAction) {
    let Some(app) = APP.get() else { return };
    let Some(url) = action_url(action) else {
        return;
    };
    match super::main_frame_policy(app, label, &url) {
        Policy::Allow => {
            let label = label.to_string();
            let _ = app.run_on_main_thread(move || load(&label, &url));
        }
        Policy::Rescue(rescued) => {
            let label = label.to_string();
            let _ = app.run_on_main_thread(move || load(&label, &rescued));
        }
        Policy::HandOff(out) => {
            if clicked(label, &out) {
                super::hand_off(&out);
            }
        }
    }
}

/// One bridge message → a `native-view` event on the view's owner.
fn forward(label: &str, message: &WKScriptMessage) {
    let Some(app) = APP.get() else { return };
    // SAFETY: WKScriptMessage accessors on the main thread, during delivery.
    let body = unsafe { message.body() };
    let Some(text) = body.downcast_ref::<NSString>().map(|s| s.to_string()) else {
        return;
    };
    if text.len() > MESSAGE_MAX {
        return;
    }
    let Ok(serde_json::Value::Object(event)) = serde_json::from_str::<serde_json::Value>(&text)
    else {
        return;
    };
    // A trusted link click is the shell's own business (see `clicked`).
    if event.get("kind").and_then(|k| k.as_str()) == Some("link") {
        if let Some(href) = event.get("href").and_then(|h| h.as_str()) {
            let href = href.to_string();
            PANES.with(|panes| {
                if let Some(pane) = panes.borrow_mut().get_mut(label) {
                    pane.link = Some((href, Instant::now()));
                }
            });
        }
        return;
    }
    let _ = app;
    notify(label, serde_json::Value::Object(event));
}

/// A `native-view` event for view `label`'s owner, tagged with its id.
fn notify(label: &str, event: serde_json::Value) {
    let Some(app) = APP.get() else { return };
    let serde_json::Value::Object(mut event) = event else {
        return;
    };
    let Some((owner, id)) = super::owner_of(app, label) else {
        return;
    };
    event.insert("id".into(), id.into());
    let _ = super::super::deliver_to(app, &owner, "native-view", &event);
}

fn world(mtm: MainThreadMarker) -> Retained<WKContentWorld> {
    // SAFETY: a named world is created once per name and shared.
    unsafe { WKContentWorld::worldWithName(&NSString::from_str(WORLD), mtm) }
}

fn request(url: &tauri::Url) -> Option<Retained<NSURLRequest>> {
    let url = NSURL::URLWithString(&NSString::from_str(url.as_str()))?;
    Some(NSURLRequest::requestWithURL(&url))
}

fn load(label: &str, url: &tauri::Url) {
    let Some(request) = request(url) else { return };
    with_pane(label, |pane| {
        // SAFETY: a live WKWebView on the main thread.
        let _ = unsafe { pane.wk.loadRequest(&request) };
    });
}

fn with_pane<T>(label: &str, f: impl FnOnce(&Pane) -> T) -> Option<T> {
    PANES.with(|panes| panes.borrow().get(label).map(f))
}

/// The frame for `rect` (CSS px from the owner's top left) in the owner
/// web view's own coordinates, flipped or not.
fn frame_in(owner: &NSView, rect: ViewRect) -> NSRect {
    let (x, y, w, h) = rect.parts();
    let y = if owner.isFlipped() {
        y
    } else {
        owner.bounds().size.height - y - h
    };
    NSRect::new(NSPoint::new(x, y), NSSize::new(w.max(1.0), h.max(1.0)))
}

/// Lay `pane` out for `rect` cut to `clip`: the box at the visible part, the
/// page inside it at `rect`'s own place. False when nothing is visible.
fn arrange(owner: &NSView, pane: &Pane, rect: ViewRect, clip: Option<ViewRect>) -> bool {
    let Some(shown) = super::clip_box(rect, clip) else {
        return false;
    };
    pane.clip.setFrame(frame_in(owner, shown));
    // The box is not flipped: the page's top sits `rect.y - shown.y` below
    // the box's top.
    let (x, y, w, h) = rect.parts();
    let (sx, sy, _, sh) = shown.parts();
    let inner = NSRect::new(
        NSPoint::new(x - sx, sh - (y - sy) - h),
        NSSize::new(w.max(1.0), h.max(1.0)),
    );
    // SAFETY: NSView geometry on the main thread.
    pane.wk.setFrame(inner);
    true
}

/// A clip box: an `NSView` whose subviews draw only inside it.
fn clip_view(mtm: MainThreadMarker) -> Retained<NSView> {
    let view = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
    view.setWantsLayer(true);
    // `clipsToBounds` is macOS 14+ (and defaults off from that SDK on);
    // before it AppKit clipped subviews already. The layer's mask covers a
    // layer-hosting child (the web view) either way.
    // SAFETY: selector checked before the call; `layer` is a CALayer or nil.
    unsafe {
        if responds(&*view, objc2::sel!(setClipsToBounds:)) {
            view.setClipsToBounds(true);
        }
        let layer: *mut AnyObject = msg_send![&*view, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
    }
    view
}

/// The owner page under a passive view takes its wheel: a scroll there goes
/// to the owner's web view, which hit-tests the card's placeholder and
/// scrolls the transcript around it. Hover and clicks still reach the view.
fn wheel_monitor() {
    if WHEEL_MONITOR.with(|on| on.replace(true)) {
        return;
    }
    let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands a live event for the duration of the call.
        let ev = unsafe { event.as_ref() };
        let owner = catch_unwind(AssertUnwindSafe(|| passive_owner_under(ev)))
            .ok()
            .flatten();
        match owner {
            Some(owner) => {
                owner.scrollWheel(ev);
                std::ptr::null_mut()
            }
            None => event.as_ptr(),
        }
    });
    // SAFETY: the handler returns the event it was given or null. The
    // monitor stays for the app's life (one per app, nothing to remove).
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::ScrollWheel, &block)
    };
    std::mem::forget(monitor);
}

/// The owner web view of a shown, passive view under `ev`'s pointer.
fn passive_owner_under(ev: &NSEvent) -> Option<Retained<NSResponder>> {
    let mtm = MainThreadMarker::new()?;
    let window = ev.window(mtm)?;
    let at = ev.locationInWindow();
    PANES.with(|panes| {
        panes.borrow().values().find_map(|pane| {
            if !pane.passive || pane.clip.isHidden() {
                return None;
            }
            let same = pane
                .clip
                .window()
                .is_some_and(|w| Retained::as_ptr(&w) == Retained::as_ptr(&window));
            if !same {
                return None;
            }
            let p = pane.clip.convertPoint_fromView(at, None);
            let b = pane.clip.bounds();
            let inside = p.x >= b.origin.x
                && p.y >= b.origin.y
                && p.x < b.origin.x + b.size.width
                && p.y < b.origin.y + b.size.height;
            if !inside {
                return None;
            }
            // SAFETY: NSView accessor on the main thread.
            unsafe { pane.clip.superview() }.map(Retained::into_super)
        })
    })
}

/// Run `f` on the main thread with the owner's WKWebView (the superview).
fn on_owner(
    owner: &Webview,
    f: impl FnOnce(&WKWebView, MainThreadMarker) + Send + 'static,
) -> Result<(), String> {
    owner
        .with_webview(move |platform| {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            // SAFETY: on macOS `inner()` is the owner's live WKWebView, valid
            // for this call (with_webview runs on the main thread).
            let wk = unsafe { &*platform.inner().cast::<WKWebView>() };
            let _ = catch_unwind(AssertUnwindSafe(|| f(wk, mtm)));
        })
        .map_err(|e| e.to_string())
}

/// Place `label` at `rect` (cut to `clip`): create it on `url` (bridge
/// first), or point an existing one at `url` when the page asked for a new
/// URL. `hidden` keeps it hidden (the page reveals it once its document
/// reports in, so nothing flashes over the loading state).
#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    app: &AppHandle,
    owner: &Webview,
    label: String,
    url: tauri::Url,
    navigate: bool,
    rect: ViewRect,
    clip: Option<ViewRect>,
    passive: bool,
    hidden: bool,
) -> Result<(), String> {
    let _ = APP.set(app.clone());
    on_owner(owner, move |owner_wk, mtm| {
        if passive {
            wheel_monitor();
        }
        let existing = PANES.with(|panes| {
            panes.borrow_mut().get_mut(&label).map(|pane| {
                pane.passive = passive;
                let visible = arrange(owner_wk, pane, rect, clip);
                pane.clip.setHidden(hidden || !visible);
            })
        });
        if existing.is_some() {
            if navigate {
                load(&label, &url);
            }
            return;
        }
        // SAFETY: WebKit object creation and configuration on the main
        // thread; the delegates are retained in `PANES` for the view's life
        // (WebKit holds them weakly).
        unsafe {
            let config = WKWebViewConfiguration::new(mtm);
            let controller = config.userContentController();
            let world = world(mtm);
            let bridge = Bridge::new(mtm, label.clone());
            controller.addScriptMessageHandler_contentWorld_name(
                ProtocolObject::from_ref(&*bridge),
                &world,
                &NSString::from_str(HANDLER),
            );
            let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                WKUserScript::alloc(mtm),
                &NSString::from_str(chimaera_core::PANE_BRIDGE_JS),
                WKUserScriptInjectionTime::AtDocumentStart,
                true,
                &world,
            );
            controller.addUserScript(&script);
            let wk = WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                frame_in(owner_wk, rect),
                &config,
            );
            let delegate = Delegate::new(mtm, label.clone());
            wk.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            wk.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            if cfg!(debug_assertions) {
                wk.setInspectable(true);
            }
            // A file dragged in from Finder is the workbench's to take (it
            // uploads to the right host); the view would swallow it.
            let _: () = msg_send![&*wk, unregisterDraggedTypes];
            let pane = Pane {
                wk,
                clip: clip_view(mtm),
                passive,
                _delegate: delegate,
                link: None,
                crashes: Vec::new(),
            };
            pane.clip.addSubview(&pane.wk);
            let visible = arrange(owner_wk, &pane, rect, clip);
            pane.clip.setHidden(hidden || !visible);
            owner_wk.addSubview(&pane.clip);
            if let Some(request) = request(&url) {
                let _ = pane.wk.loadRequest(&request);
            }
            PANES.with(|panes| panes.borrow_mut().insert(label, pane));
        }
    })
}

/// Move a view (cut to `clip`), or hide it (`None`).
pub(super) fn place(
    owner: &Webview,
    label: String,
    rect: Option<ViewRect>,
    clip: Option<ViewRect>,
    passive: bool,
) -> Result<(), String> {
    on_owner(owner, move |owner_wk, _mtm| {
        if passive {
            wheel_monitor();
        }
        PANES.with(|panes| {
            let mut panes = panes.borrow_mut();
            let Some(pane) = panes.get_mut(&label) else {
                return;
            };
            pane.passive = passive;
            let visible = rect.is_some_and(|rect| arrange(owner_wk, pane, rect, clip));
            pane.clip.setHidden(!visible);
        });
    })
}

/// Take a view out of its window for good, and its web process with it.
pub(super) fn close(app: &AppHandle, label: String) {
    let _ = app.run_on_main_thread(move || {
        let Some(pane) = PANES.with(|panes| panes.borrow_mut().remove(&label)) else {
            return;
        };
        // SAFETY: main thread, a live WKWebView. Delegates first: the end
        // below reports a termination synchronously, and a closing view
        // must not be reloaded.
        unsafe {
            pane.wk.setNavigationDelegate(None);
            pane.wk.setUIDelegate(None);
            pane.wk.stopLoading();
        }
        // Out of its window first, so the windows' web views `end_process`
        // checks are everyone else's.
        pane.clip.removeFromSuperview();
        if let Some(mtm) = MainThreadMarker::new() {
            end_process(&pane.wk, mtm);
        }
    });
}

/// Whether `obj` answers `sel` (an SPI WebKit may drop or rename).
fn responds<T: objc2::Message>(obj: &T, sel: objc2::runtime::Sel) -> bool {
    // SAFETY: `respondsToSelector:` is NSObject's, safe on any object.
    unsafe { msg_send![obj, respondsToSelector: sel] }
}

/// The web processes `view` runs pages in now: a `WKWebView`'s page and,
/// mid-navigation, its provisional page. `None` for any other view, or when
/// WebKit can no longer say (the SPI is gone).
fn web_pids(view: &NSView) -> Option<Vec<i32>> {
    if !responds(view, objc2::sel!(_webProcessIdentifier)) {
        return None;
    }
    // SAFETY: both getters checked; each returns a pid_t (0 = no process).
    let mut pids: Vec<i32> = vec![unsafe { msg_send![view, _webProcessIdentifier] }];
    if responds(view, objc2::sel!(_provisionalWebProcessIdentifier)) {
        pids.push(unsafe { msg_send![view, _provisionalWebProcessIdentifier] });
    }
    pids.retain(|pid| *pid > 0);
    Some(pids)
}

/// Every web process the app's windows show a page in: each window's own web
/// view, every pane view, any other web view in them.
fn window_pids(mtm: MainThreadMarker) -> Vec<i32> {
    let mut pids = Vec::new();
    let mut stack: Vec<Retained<NSView>> = NSApplication::sharedApplication(mtm)
        .windows()
        .iter()
        .filter_map(|window| window.contentView())
        .collect();
    while let Some(view) = stack.pop() {
        pids.extend(web_pids(&view).unwrap_or_default());
        // A window's web view holds its pane views' clip boxes.
        stack.extend(view.subviews().iter());
    }
    pids
}

/// End a closing view's web process now. Dropped alone, the view leaves its
/// process to exit once it handles WebKit's close message, and a busy one —
/// laying out a huge report, or swapping — takes minutes to get to it
/// (a 9.5 GB report process lived 4 minutes past its view, 2026-11-04).
///
/// `_killWebContentProcessAndResetState` SIGKILLs the page's process (and a
/// provisional page's) at once and leaves the shared website data alone; the
/// plain `_killWebContentProcess` left the process running (macOS 26). Each
/// view gets a process of its own (measured: distinct pids for views on one
/// origin, the window's included), but the kill would take down any other
/// page in that process, so it's skipped when any web view left in the app's
/// windows reports the same pid. Fails closed: no SPI, no pid, no kill — the
/// view just closes as before.
fn end_process(wk: &WKWebView, mtm: MainThreadMarker) {
    let Some(ours) = web_pids(wk).filter(|pids| !pids.is_empty()) else {
        return;
    };
    let others = window_pids(mtm);
    if ours.iter().any(|pid| others.contains(pid)) {
        tracing::warn!(
            ?ours,
            "pane view: its web process is shared; closing without ending it"
        );
        return;
    }
    if responds(wk, objc2::sel!(_killWebContentProcessAndResetState)) {
        // SAFETY: checked above; takes and returns nothing. No `PANES`
        // borrow is held: the termination is reported synchronously.
        let _: () = unsafe { msg_send![wk, _killWebContentProcessAndResetState] };
        tracing::debug!(?ours, "pane view: ended its web process");
    }
}

/// Run `f` on the main thread with a live view; its answer comes back
/// through `f`'s sender (dropped unsent = the view is gone).
async fn ask<T: Send + 'static>(
    app: &AppHandle,
    label: String,
    f: impl FnOnce(&WKWebView, MainThreadMarker, Box<dyn FnOnce(Result<T, String>) + Send>)
        + Send
        + 'static,
) -> Result<T, String> {
    let (tx, rx) = oneshot::channel::<Result<T, String>>();
    app.run_on_main_thread(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let wk = with_pane(&label, |pane| pane.wk.clone());
        let Some(wk) = wk else {
            let _ = tx.send(Err("no such view".into()));
            return;
        };
        let send: Box<dyn FnOnce(Result<T, String>) + Send> = Box::new(move |result| {
            let _ = tx.send(result);
        });
        let _ = catch_unwind(AssertUnwindSafe(|| f(&wk, mtm, send)));
    })
    .map_err(|e| e.to_string())?;
    rx.await.map_err(|_| "the view closed".to_string())?
}

/// A completion handler can run more than once in principle: answer once.
fn once<T: Send + 'static>(
    send: Box<dyn FnOnce(Result<T, String>) + Send>,
) -> impl Fn(Result<T, String>) {
    let slot = Mutex::new(Some(send));
    move |result| {
        if let Some(send) = slot.lock().ok().and_then(|mut s| s.take()) {
            send(result);
        }
    }
}

/// What `native_view_snapshot` answers.
#[derive(Serialize)]
pub(super) struct Snapshot {
    /// Base64 PNG of the visible viewport, at the display's pixel scale.
    png: String,
    /// The viewport in CSS px (points).
    width: f64,
    height: f64,
}

/// The view's visible viewport, as rendered now.
pub(super) async fn snapshot(app: &AppHandle, label: String) -> Result<Snapshot, String> {
    ask(app, label, |wk, _mtm, send| {
        let send = once(send);
        let block = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            // SAFETY: WebKit hands either a live image or a live error.
            let result = match unsafe { (image.as_ref(), error.as_ref()) } {
                (Some(image), _) => png_of(image),
                (None, Some(error)) => Err(error.localizedDescription().to_string()),
                (None, None) => Err("the view drew nothing".to_string()),
            };
            send(result);
        });
        // SAFETY: a live WKWebView on the main thread; nil = the visible bounds.
        unsafe { wk.takeSnapshotWithConfiguration_completionHandler(None, &block) };
    })
    .await
}

fn png_of(image: &NSImage) -> Result<Snapshot, String> {
    let size = image.size();
    let tiff = image.TIFFRepresentation().ok_or("no image data")?;
    let rep = NSBitmapImageRep::imageRepWithData(&tiff).ok_or("no bitmap")?;
    // SAFETY: an empty properties dictionary is valid for PNG.
    let png = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .ok_or("PNG encoding failed")?;
    Ok(Snapshot {
        png: base64::engine::general_purpose::STANDARD.encode(png.to_vec()),
        width: size.width,
        height: size.height,
    })
}

/// Call `__chimaeraPaneView.<method>(arg)` in the bridge's world and answer
/// its JSON result. `arg` crosses as a JSON string argument, never spliced
/// into source.
pub(super) async fn call(
    app: &AppHandle,
    label: String,
    method: &'static str,
    arg: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let arg = arg.to_string();
    ask(app, label, move |wk, mtm, send| {
        let send = once(send);
        let body = NSString::from_str(&format!(
            "const b = globalThis.__chimaeraPaneView;\
             if (b === undefined) return 'null';\
             return JSON.stringify(b.{method}(JSON.parse(arg)) ?? null);"
        ));
        let key = NSString::from_str("arg");
        let value = NSString::from_str(&arg);
        let value: &AnyObject = &value;
        let arguments = NSDictionary::<NSString, AnyObject>::from_slices(&[&*key], &[value]);
        let block = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
            // SAFETY: WebKit hands either a result (or nil) or an error.
            let answer = match unsafe { (result.as_ref(), error.as_ref()) } {
                (_, Some(error)) => Err(error.localizedDescription().to_string()),
                (Some(result), None) => result
                    .downcast_ref::<NSString>()
                    .map(|s| {
                        serde_json::from_str(&s.to_string()).unwrap_or(serde_json::Value::Null)
                    })
                    .ok_or_else(|| "unexpected answer".to_string()),
                (None, None) => Ok(serde_json::Value::Null),
            };
            send(answer);
        });
        // SAFETY: a live WKWebView on the main thread; `arguments` maps a
        // string key to a string, which WebKit bridges to a JS string.
        unsafe {
            wk.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                &body,
                Some(&arguments),
                None,
                &world(mtm),
                Some(&block),
            );
        }
    })
    .await
}

/// Back, forward, reload or stop.
pub(super) fn nav(app: &AppHandle, label: String, action: NavAction) -> Result<(), String> {
    app.run_on_main_thread(move || {
        with_pane(&label, |pane| {
            // SAFETY: navigation calls on a live WKWebView on the main thread.
            unsafe {
                match action {
                    NavAction::Back => drop(pane.wk.goBack()),
                    NavAction::Forward => drop(pane.wk.goForward()),
                    NavAction::Reload => drop(pane.wk.reload()),
                    NavAction::Stop => pane.wk.stopLoading(),
                }
            }
        });
    })
    .map_err(|e| e.to_string())
}
