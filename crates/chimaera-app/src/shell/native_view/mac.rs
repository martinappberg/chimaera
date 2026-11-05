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
//! Every `WKWebView` touch is on the main thread: views live in a
//! main-thread registry, reached through `run_on_main_thread`, and the async
//! commands wait on a oneshot for a completion handler's answer.
//!
//! Agent driving (`native_view_drive`, `agent.rs`) injects real NSEvents
//! into a view. The bridge reports trusted input back (`input`), which the
//! shell matches against what it injected (take-over: an unmatched report is
//! the user, `user_input` on the owner); `input` itself never reaches the
//! page. While an agent drives a view — always for an agent's own view, and
//! for a short quiet window after each injected action on a shared one — the
//! bridge's focus, link, chord and selection reports are the agent's doing
//! and are dropped, so an agent's click can't focus a pane, open the user's
//! browser, close a tab or take the reference.

use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

mod agent;
pub(super) use agent::drive as agent_drive;

use base64::Engine as _;
use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSView};
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

use super::drive::{Injections, InputKind, World};
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
    _delegate: Retained<Delegate>,
    /// The last link the user clicked (trusted, reported by the bridge).
    link: Option<(String, Instant)>,
    /// When its web process died, newest last, within `CRASH_WINDOW`.
    crashes: Vec<Instant>,
    /// An agent's own view (off-bounds, never the user's to touch): every
    /// focus/link/chord/selection report is the agent's.
    agent: bool,
    /// The page-world recorder is installed for its loads.
    instrumented: bool,
    /// Draw the agent cursor for injected pointer input (a pane the user
    /// watches), in this style.
    cursor: Option<agent::CursorStyle>,
    /// Injected input waiting for the bridge's report (take-over).
    injections: Injections,
    /// Reports that an injected action causes are dropped until then.
    quiet_until: Option<Instant>,
    /// The window's first responder before a drag took it, put back at the
    /// drag's end.
    drag_prior: Option<agent::Saved>,
}

impl Pane {
    fn new(wk: Retained<WKWebView>, delegate: Retained<Delegate>, agent: bool) -> Self {
        Pane {
            wk,
            _delegate: delegate,
            link: None,
            crashes: Vec::new(),
            agent,
            instrumented: false,
            cursor: None,
            injections: Injections::default(),
            quiet_until: None,
            drag_prior: None,
        }
    }

    /// Whether the bridge's user-gesture reports now come from the agent.
    fn agent_active(&self, now: Instant) -> bool {
        self.agent || self.quiet_until.is_some_and(|t| now < t)
    }
}

thread_local! {
    /// Main thread only: child label → its view.
    static PANES: RefCell<HashMap<String, Pane>> = RefCell::new(HashMap::new());
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
    let kind = event.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let now = Instant::now();
    // Trusted input, for take-over: matched against what the shell
    // injected; only the user's (unmatched) becomes an event.
    if kind == "input" {
        let reported = event
            .get("type")
            .and_then(|t| t.as_str())
            .and_then(InputKind::of_report);
        let user = reported.is_some_and(|input| {
            PANES.with(|panes| {
                panes
                    .borrow_mut()
                    .get_mut(label)
                    .is_some_and(|pane| pane.injections.report(input, now))
            })
        });
        if user {
            notify(label, serde_json::json!({"kind": "user_input"}));
        }
        return;
    }
    if matches!(kind, "focus" | "link" | "key" | "selection" | "reference")
        && with_pane(label, |pane| pane.agent_active(now)).unwrap_or(false)
    {
        return;
    }
    // A trusted link click is the shell's own business (see `clicked`).
    if kind == "link" {
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

/// Keep `f` short and free of WebKit calls that could spin a run loop: the
/// registry stays borrowed while it runs.
fn with_pane_mut<T>(label: &str, f: impl FnOnce(&mut Pane) -> T) -> Option<T> {
    PANES.with(|panes| panes.borrow_mut().get_mut(label).map(f))
}

/// The page-world recorder as a user script, for every later load.
///
/// # Safety
/// Main thread, a live configuration's controller.
unsafe fn add_recorder(controller: &WKUserContentController, mtm: MainThreadMarker) {
    let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
        WKUserScript::alloc(mtm),
        &NSString::from_str(chimaera_core::PANE_AGENT_JS),
        WKUserScriptInjectionTime::AtDocumentStart,
        true,
        &WKContentWorld::pageWorld(mtm),
    );
    controller.addUserScript(&script);
}

/// Install the recorder in view `label` for its future loads (once) and in
/// the page it shows now.
fn instrument(label: &str, mtm: MainThreadMarker) {
    let Some((wk, fresh)) = with_pane_mut(label, |pane| {
        let fresh = !pane.instrumented;
        pane.instrumented = true;
        (pane.wk.clone(), fresh)
    }) else {
        return;
    };
    // SAFETY: main thread; a live view's configuration (WebKit hands a
    // copy whose controller is the view's own).
    unsafe {
        if fresh {
            add_recorder(&wk.configuration().userContentController(), mtm);
        }
        // The recorder guards against running twice in one document.
        run_js(
            &wk,
            mtm,
            World::Page,
            chimaera_core::PANE_AGENT_JS,
            &[],
            None,
        );
    }
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

/// How `show` treats a view.
#[derive(Debug, Clone, Copy)]
pub(super) struct ShowAs {
    /// Load the URL into an existing view (the page asked for a new one).
    pub(super) navigate: bool,
    /// Keep it hidden (the page reveals it once its document reports in,
    /// so nothing flashes over the loading state).
    pub(super) hidden: bool,
    /// An agent's own view (`Pane::agent`), with the page-world recorder.
    pub(super) agent: bool,
}

/// Place `label` at `rect`: create it on `url` (bridge first), or point an
/// existing one at `url` when the page asked for a new URL.
pub(super) fn show(
    app: &AppHandle,
    owner: &Webview,
    label: String,
    url: tauri::Url,
    rect: ViewRect,
    how: ShowAs,
) -> Result<(), String> {
    let _ = APP.set(app.clone());
    let ShowAs {
        navigate,
        hidden,
        agent,
    } = how;
    on_owner(owner, move |owner_wk, mtm| {
        let frame = frame_in(owner_wk, rect);
        let existing = with_pane_mut(&label, |pane| {
            // SAFETY: NSView geometry on the main thread.
            pane.wk.setFrame(frame);
            pane.wk.setHidden(hidden);
            // A handed-over view is shown again without `agent`: from
            // then on it is the user's pane (the recorder stays).
            pane.agent = agent;
            pane.instrumented
        });
        if let Some(instrumented) = existing {
            if agent && !instrumented {
                instrument(&label, mtm);
            }
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
            if agent {
                add_recorder(&controller, mtm);
            }
            let wk = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config);
            let delegate = Delegate::new(mtm, label.clone());
            wk.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            wk.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            if cfg!(debug_assertions) {
                wk.setInspectable(true);
            }
            // A file dragged in from Finder is the workbench's to take (it
            // uploads to the right host); the view would swallow it.
            let _: () = msg_send![&*wk, unregisterDraggedTypes];
            wk.setHidden(hidden);
            owner_wk.addSubview(&wk);
            if let Some(request) = request(&url) {
                let _ = wk.loadRequest(&request);
            }
            let mut pane = Pane::new(wk, delegate, agent);
            pane.instrumented = agent;
            PANES.with(|panes| panes.borrow_mut().insert(label, pane));
        }
    })
}

/// Move a view, or hide it (`None`).
pub(super) fn place(owner: &Webview, label: String, rect: Option<ViewRect>) -> Result<(), String> {
    on_owner(owner, move |owner_wk, _mtm| {
        with_pane(&label, |pane| match rect {
            Some(rect) => {
                pane.wk.setFrame(frame_in(owner_wk, rect));
                pane.wk.setHidden(false);
            }
            None => pane.wk.setHidden(true),
        });
    })
}

/// Take a view out of its window for good.
pub(super) fn close(app: &AppHandle, label: String) {
    let _ = app.run_on_main_thread(move || {
        if let Some(pane) = PANES.with(|panes| panes.borrow_mut().remove(&label)) {
            // SAFETY: main thread; stop the page before the view goes so
            // its process is released with it.
            unsafe { pane.wk.stopLoading() };
            pane.wk.removeFromSuperview();
        }
    });
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

/// Why a script call failed: the view is gone, or WebKit refused the
/// script (it didn't parse, or threw outside a wrapper; WebKit's message is
/// opaque).
#[derive(Debug)]
pub(super) enum JsFail {
    Gone(String),
    Script(String),
}

impl JsFail {
    fn text(self) -> String {
        match self {
            JsFail::Gone(s) | JsFail::Script(s) => s,
        }
    }
}

/// Where a script call's answer goes.
type JsDone = Box<dyn FnOnce(Result<Option<String>, JsFail>) + Send>;

/// Start `source` (an async function body) in `world` with string
/// arguments, answering through `done` (`Ok(None)` = it returned nothing,
/// or something other than a string). Arguments cross as JS strings, never
/// spliced into source.
///
/// # Safety
/// Main thread, a live view.
unsafe fn run_js(
    wk: &WKWebView,
    mtm: MainThreadMarker,
    world: World,
    source: &str,
    args: &[(&str, &str)],
    done: Option<JsDone>,
) {
    let keys: Vec<Retained<NSString>> = args.iter().map(|(k, _)| NSString::from_str(k)).collect();
    let values: Vec<Retained<NSString>> = args.iter().map(|(_, v)| NSString::from_str(v)).collect();
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let value_refs: Vec<&AnyObject> = values
        .iter()
        .map(|v| {
            let o: &AnyObject = v;
            o
        })
        .collect();
    let arguments = NSDictionary::<NSString, AnyObject>::from_slices(&key_refs, &value_refs);
    let world = match world {
        World::Bridge => self::world(mtm),
        World::Page => WKContentWorld::pageWorld(mtm),
    };
    let block = done.map(|done| {
        let done =
            once(
                Box::new(move |r: Result<Option<String>, String>| done(r.map_err(JsFail::Script)))
                    as Box<dyn FnOnce(Result<Option<String>, String>) + Send>,
            );
        RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
            // SAFETY: WebKit hands either a result (or nil) or an error.
            let answer = match unsafe { (result.as_ref(), error.as_ref()) } {
                (_, Some(error)) => Err(error.localizedDescription().to_string()),
                (Some(result), None) => {
                    Ok(result.downcast_ref::<NSString>().map(|s| s.to_string()))
                }
                (None, None) => Ok(None),
            };
            done(answer);
        })
    });
    wk.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
        &NSString::from_str(source),
        Some(&arguments),
        None,
        &world,
        block.as_deref(),
    );
}

/// Run `source` in view `label` and wait for its answer.
pub(super) async fn js(
    app: &AppHandle,
    label: String,
    world: World,
    source: String,
    args: Vec<(&'static str, String)>,
) -> Result<Option<String>, JsFail> {
    let (tx, rx) = oneshot::channel::<Result<Option<String>, JsFail>>();
    app.run_on_main_thread(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(wk) = with_pane(&label, |pane| pane.wk.clone()) else {
            let _ = tx.send(Err(JsFail::Gone("no such view".into())));
            return;
        };
        let pairs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let done: JsDone = Box::new(move |r| {
            let _ = tx.send(r);
        });
        // SAFETY: main thread, a live view.
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            run_js(&wk, mtm, world, &source, &pairs, Some(done))
        }));
    })
    .map_err(|e| JsFail::Gone(e.to_string()))?;
    rx.await
        .map_err(|_| JsFail::Gone("the view closed".into()))?
}

/// The source that calls `__chimaeraPaneView.<method>(JSON.parse(arg))`
/// and answers its JSON.
fn bridge_call(method: &str) -> String {
    format!(
        "const b = globalThis.__chimaeraPaneView;\
         if (b === undefined || typeof b.{method} !== 'function') return 'null';\
         return JSON.stringify(b.{method}(JSON.parse(arg)) ?? null);"
    )
}

/// Call `__chimaeraPaneView.<method>(arg)` in the bridge's world and answer
/// its JSON result (`null` when the bridge isn't there yet).
pub(super) async fn call(
    app: &AppHandle,
    label: String,
    method: &'static str,
    arg: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let answer = js(
        app,
        label,
        World::Bridge,
        bridge_call(method),
        vec![("arg", arg.to_string())],
    )
    .await
    .map_err(JsFail::text)?;
    match answer {
        Some(text) => Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)),
        None => Ok(serde_json::Value::Null),
    }
}

/// Call a bridge method without waiting for it (the agent cursor).
///
/// # Safety
/// Main thread, a live view.
unsafe fn fire(wk: &WKWebView, mtm: MainThreadMarker, method: &str, arg: &serde_json::Value) {
    run_js(
        wk,
        mtm,
        World::Bridge,
        &bridge_call(method),
        &[("arg", &arg.to_string())],
        None,
    );
}

/// Run `f` on the main thread and wait for its answer.
pub(super) async fn on_main<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(MainThreadMarker) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = oneshot::channel::<Result<T, String>>();
    app.run_on_main_thread(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let result = catch_unwind(AssertUnwindSafe(|| f(mtm)))
            .unwrap_or_else(|_| Err("the shell failed on that action".into()));
        let _ = tx.send(result);
    })
    .map_err(|e| e.to_string())?;
    rx.await.map_err(|_| "the view closed".to_string())?
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
