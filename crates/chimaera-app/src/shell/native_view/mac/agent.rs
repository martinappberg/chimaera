//! macOS half of `native_view_drive`: real input into a pane view, evals,
//! screenshots, the rings, the recorder and the agent cursor.
//!
//! Input is NSEvents delivered by calling the view's own responder methods
//! (`mouseDown:` …), which the page sees as trusted (measured against
//! Jupyter, marimo and Streamlit, #273). What that measurement taught:
//! - Hover: a `WKWebView` ignores `mouseMoved:`; its tracking-area owner
//!   (`WKMouseTrackingObserver`) takes it.
//! - Keys sent back to back are dropped: one press, then ~12 ms on a timer
//!   (never a blocked main thread). `insertText:` is the fast path for text.
//! - Wheel: a CGEvent pixel scroll made into an NSEvent for `scrollWheel:`.
//! - A click makes the view first responder, which takes the user's
//!   keyboard: the window's previous first responder goes back after every
//!   action.
//! - There is no right button and no way to open a native `<select>` here:
//!   both run an NSMenu's nested loop on the main thread. HTML5 drag-and-drop
//!   needs a real drag session and doesn't happen.
//!
//! Paced work (keys, drag steps) runs as main-thread hops between tokio
//! sleeps, so the main thread is never held, and stops when the user takes
//! the page over (`drive::PAUSED`).
//!
//! Guards (#273 review):
//! - Native menus: a ctrl-click is refused at parse; before a click (or a
//!   drag's press) the agent world hit-tests the point and refuses a popup
//!   `<select>` or a color input, and before Enter/Space/an arrow it refuses
//!   a focused popup select — each would run an NSMenu (or the color
//!   panel) loop on the main thread.
//! - Keys the page leaves unhandled: WebKit re-sends an unhandled keyDown
//!   to the app (`[NSApp sendEvent:]`, so menu key equivalents work from a
//!   web view). For an injected key that would reach the menu bar (⌘W, ⌘Q…)
//!   and the window's first responder — Chimaera's own UI. Every injected
//!   keyDown is remembered (`INJECTED_KEYS`, kept alive so no real event
//!   can reuse its address) and the app's `sendEvent:` drops it
//!   (`guard_resend`). An agent's own view also ends its responder chain at
//!   a sink (`super::set_chain`). Chords that touch the clipboard are
//!   refused at parse.
//! - Encoding a screenshot (PNG/JPEG + base64) happens off the main thread.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use base64::Engine as _;
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{msg_send, sel, AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSBitmapImageFileType, NSBitmapImageRep, NSEvent, NSEventModifierFlags,
    NSEventType, NSImage, NSImageCompressionFactor, NSResponder, NSScreen, NSWindow,
};
use objc2_core_graphics::{CGEvent, CGEventField, CGImage, CGScrollEventUnit};
use objc2_foundation::{
    NSDictionary, NSError, NSNumber, NSPoint, NSProcessInfo, NSRect, NSSize, NSString,
};
use objc2_web_kit::{WKSnapshotConfiguration, WKWebView};
use serde_json::{json, Value};
use tauri::AppHandle;

use super::super::drive::{
    self, Clip, DriveAction, InputKind, KeyStroke, Mods, ShotFormat, Stroke, World,
};
use super::{
    fire, instrument, js, on_main, once, uninstrument, with_pane, with_pane_mut, JsFail, Realm,
};

/// Between paced key presses.
const KEY_GAP: Duration = Duration::from_millis(12);
/// Between drag steps.
const DRAG_GAP: Duration = Duration::from_millis(16);
/// After an injected action, the bridge's gesture reports are its doing
/// for this long: they arrive a few ms later, and a selection counts for
/// 1.5 s after trusted input (plus its 120 ms debounce) — so an agent's
/// click followed by a scripted selection can't take the reference.
const QUIET: Duration = Duration::from_millis(2000);
/// A guard's hit test answers within this, or the press goes ahead (a
/// page too busy to answer is too busy to open a menu).
const GUARD_TIMEOUT: Duration = Duration::from_secs(2);
/// An agent view put out of sight while it grows for a full-page shot.
const OFF_BOUNDS_GAP: f64 = 64.0;

/// How the agent cursor looks on a watched pane.
#[derive(Debug, Clone)]
pub(in crate::shell::native_view) struct CursorStyle {
    color: Option<String>,
    label: Option<String>,
}

/// A window's first responder, to put back after injected input.
pub(in crate::shell::native_view) struct Saved {
    window: Retained<NSWindow>,
    prior: Option<Retained<NSResponder>>,
}

impl Saved {
    fn take(wk: &WKWebView) -> Option<Saved> {
        let window = wk.window()?;
        Some(Saved {
            prior: window.firstResponder(),
            window,
        })
    }

    fn restore(self) {
        let now = self.window.firstResponder();
        let same = match (&now, &self.prior) {
            (Some(a), Some(b)) => Retained::as_ptr(a) == Retained::as_ptr(b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.window.makeFirstResponder(self.prior.as_deref());
        }
    }
}

/// Act on view `label`. Results per `native_view_drive`'s contract.
pub(in crate::shell::native_view) async fn drive(
    app: &AppHandle,
    label: String,
    action: DriveAction,
) -> Result<Value, String> {
    match action {
        DriveAction::Eval {
            body,
            args,
            world,
            timeout_ms,
        } => eval(app, label, body, args, world, timeout_ms)
            .await
            .map(|value| json!({ "value": value })),
        DriveAction::Click {
            x,
            y,
            clicks,
            modifiers,
        } => {
            let mods = Mods::of(&modifiers);
            guard_point(app, &label, x, y).await?;
            input(app, label, move |wk, mtm, label| {
                cursor_at(label, wk, mtm, x, y, true);
                let kinds = vec![InputKind::Pointer; usize::from(clicks)];
                record(label, &kinds);
                let saved = Saved::take(wk);
                hover(wk, x, y, mods);
                for n in 1..=isize::from(clicks) {
                    if let Some(down) = mouse(wk, NSEventType::LeftMouseDown, x, y, n, mods) {
                        wk.mouseDown(&down);
                    }
                    if let Some(up) = mouse(wk, NSEventType::LeftMouseUp, x, y, n, mods) {
                        wk.mouseUp(&up);
                    }
                }
                restore(saved);
            })
            .await
        }
        DriveAction::Move { x, y } => {
            input(app, label, move |wk, mtm, label| {
                cursor_at(label, wk, mtm, x, y, false);
                record(label, &[]);
                hover(wk, x, y, Mods::default());
            })
            .await
        }
        DriveAction::Scroll { x, y, dx, dy } => {
            input(app, label, move |wk, mtm, label| {
                cursor_at(label, wk, mtm, x, y, false);
                hover(wk, x, y, Mods::default());
                if dx == 0.0 && dy == 0.0 {
                    return;
                }
                if let Some(event) = wheel(wk, mtm, x, y, dx, dy) {
                    record(label, &[InputKind::Wheel]);
                    let saved = Saved::take(wk);
                    wk.scrollWheel(&event);
                    restore(saved);
                }
            })
            .await
        }
        DriveAction::Drag { from, to, steps } => drag(app, label, from, to, steps).await,
        DriveAction::Key { key, modifiers } => {
            let stroke = drive::key_stroke(&key, Mods::of(&modifiers))?;
            if drive::may_open_menu(&stroke) {
                guard_focus(app, &label).await?;
            }
            input(app, label, move |wk, mtm, label| {
                press(label, wk, mtm, &stroke)
            })
            .await
        }
        DriveAction::Text {
            text,
            per_key: false,
        } => {
            input(app, label, move |wk, mtm, label| {
                press(label, wk, mtm, &Stroke::Insert(text))
            })
            .await
        }
        DriveAction::Text {
            text,
            per_key: true,
        } => {
            let started = Instant::now();
            for (i, stroke) in drive::strokes(&text).into_iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(KEY_GAP).await;
                }
                if drive::may_open_menu(&stroke) {
                    guard_focus(app, &label).await?;
                }
                step(app, label.clone(), started, move |wk, mtm, label| {
                    press(label, wk, mtm, &stroke)
                })
                .await?;
            }
            Ok(json!({}))
        }
        DriveAction::Screenshot {
            clip,
            full_page,
            max_width,
            format,
            quality,
        } => {
            let encoding = Encoding { format, quality };
            screenshot(app, label, clip, full_page, max_width, encoding).await
        }
        DriveAction::Rings {
            console_since,
            network_since,
        } => {
            let rings = super::call(
                app,
                label,
                "rings",
                json!({"console_since": console_since, "network_since": network_since}),
            )
            .await?;
            if rings.is_object() {
                Ok(rings)
            } else {
                Err("the page's bridge isn't there yet (still loading?)".into())
            }
        }
        DriveAction::Instrument => {
            on_main(app, move |mtm| {
                if with_pane(&label, |_| ()).is_none() {
                    return Err("no such view".to_string());
                }
                instrument(&label, mtm);
                Ok(json!({}))
            })
            .await
        }
        DriveAction::Uninstrument => {
            on_main(app, move |mtm| {
                if with_pane(&label, |_| ()).is_none() {
                    return Err("no such view".to_string());
                }
                uninstrument(&label, mtm);
                Ok(json!({}))
            })
            .await
        }
        DriveAction::Cursor {
            show,
            color,
            label: name,
        } => {
            on_main(app, move |mtm| {
                let wk = with_pane_mut(&label, |pane| {
                    pane.cursor = show.then(|| CursorStyle {
                        color: color.clone(),
                        label: name.clone(),
                    });
                    pane.wk.clone()
                })
                .ok_or("no such view")?;
                if !show {
                    // SAFETY: main thread, a live view.
                    unsafe { fire(&wk, mtm, "cursor", &Value::Null) };
                }
                Ok(json!({}))
            })
            .await
        }
    }
}

/// One main-thread input step on a live view; answers `{}`.
async fn input(
    app: &AppHandle,
    label: String,
    f: impl FnOnce(&WKWebView, MainThreadMarker, &str) + Send + 'static,
) -> Result<Value, String> {
    on_main(app, move |mtm| {
        // Cloned out: input can spin a nested run loop that reaches the
        // registry (the bridge's messages), which must not find it borrowed.
        let wk = with_pane(&label, |pane| pane.wk.clone()).ok_or("no such view")?;
        f(&wk, mtm, &label);
        Ok(json!({}))
    })
    .await
}

/// A paced step of an action that began at `started`: refused once the
/// user has used the page since (take-over).
async fn step(
    app: &AppHandle,
    label: String,
    started: Instant,
    f: impl FnOnce(&WKWebView, MainThreadMarker, &str) + Send + 'static,
) -> Result<Value, String> {
    on_main(app, move |mtm| {
        let (wk, paused) = with_pane(&label, |pane| {
            (pane.wk.clone(), pane.injections.user_since(started))
        })
        .ok_or("no such view")?;
        if paused {
            return Err(drive::PAUSED.to_string());
        }
        f(&wk, mtm, &label);
        Ok(json!({}))
    })
    .await
}

/// The agent world's verdict on a guard script → go on, or the refusal.
async fn guard(app: &AppHandle, label: &str, source: &str, args: String) -> Result<(), String> {
    let run = js(
        app,
        label.to_string(),
        Realm::Agent,
        source.to_string(),
        vec![("__chimaeraArgs", args)],
    );
    match tokio::time::timeout(GUARD_TIMEOUT, run).await {
        Ok(Ok(Some(verdict))) => drive::menu_verdict(&verdict),
        Ok(Err(JsFail::Gone(why))) => Err(why),
        // No document to ask (loading), or no answer: nothing can open a
        // menu from there.
        _ => Ok(()),
    }
}

/// Refuse a press at `(x, y)` that would open a native menu or panel.
async fn guard_point(app: &AppHandle, label: &str, x: f64, y: f64) -> Result<(), String> {
    guard(
        app,
        label,
        drive::MENU_AT,
        json!({"x": x, "y": y}).to_string(),
    )
    .await
}

/// Refuse a key that would open the focused popup select's menu.
async fn guard_focus(app: &AppHandle, label: &str) -> Result<(), String> {
    guard(app, label, drive::MENU_FOCUSED, "{}".into()).await
}

/// Note injected input (and start the quiet window) before delivering it.
fn record(label: &str, kinds: &[InputKind]) {
    let now = Instant::now();
    with_pane_mut(label, |pane| {
        pane.injections.drove(now);
        for kind in kinds {
            pane.injections.push(*kind, now);
        }
        pane.quiet_until = Some(now + QUIET);
    });
}

fn restore(saved: Option<Saved>) {
    if let Some(saved) = saved {
        saved.restore();
    }
}

/// A key press, or text inserted.
fn press(label: &str, wk: &WKWebView, mtm: MainThreadMarker, stroke: &Stroke) {
    match stroke {
        Stroke::Key(key) => {
            let Some((down, up)) = key_events(wk, key) else {
                return;
            };
            remember_key(&down, mtm);
            record(label, &[InputKind::Key]);
            let saved = Saved::take(wk);
            wk.keyDown(&down);
            wk.keyUp(&up);
            restore(saved);
        }
        Stroke::Insert(text) => {
            // `insertText:` fires trusted beforeinput/input, no keydown.
            record(label, &[]);
            let saved = Saved::take(wk);
            let text = NSString::from_str(text);
            // SAFETY: WKWebView implements NSResponder's `insertText:`.
            let _: () = unsafe { msg_send![wk, insertText: &*text] };
            restore(saved);
        }
    }
}

/// A drag: press at `from`, `steps` moves 16 ms apart, release at `to`.
async fn drag(
    app: &AppHandle,
    label: String,
    from: [f64; 2],
    to: [f64; 2],
    steps: u16,
) -> Result<Value, String> {
    let [x0, y0] = from;
    let [x1, y1] = to;
    guard_point(app, &label, x0, y0).await?;
    let started = Instant::now();
    input(app, label.clone(), move |wk, mtm, label| {
        cursor_at(label, wk, mtm, x0, y0, false);
        record(label, &[InputKind::Pointer]);
        let saved = Saved::take(wk);
        with_pane_mut(label, |pane| pane.drag_prior = saved);
        hover(wk, x0, y0, Mods::default());
        if let Some(down) = mouse(wk, NSEventType::LeftMouseDown, x0, y0, 1, Mods::default()) {
            wk.mouseDown(&down);
        }
    })
    .await?;
    let mut moved = Ok(json!({}));
    let mut at = (x0, y0);
    for i in 1..=steps {
        tokio::time::sleep(DRAG_GAP).await;
        let f = f64::from(i) / f64::from(steps);
        let (x, y) = (x0 + (x1 - x0) * f, y0 + (y1 - y0) * f);
        moved = step(app, label.clone(), started, move |wk, mtm, label| {
            cursor_at(label, wk, mtm, x, y, false);
            record(label, &[]);
            if let Some(event) = mouse(wk, NSEventType::LeftMouseDragged, x, y, 1, Mods::default())
            {
                wk.mouseDragged(&event);
            }
        })
        .await;
        if moved.is_err() {
            break;
        }
        at = (x, y);
    }
    // Released where the pointer got to (and the responder given back) even
    // after a failed or interrupted step, if the view lives.
    let (rx, ry) = at;
    let released = input(app, label, move |wk, _mtm, label| {
        record(label, &[]);
        if let Some(up) = mouse(wk, NSEventType::LeftMouseUp, rx, ry, 1, Mods::default()) {
            wk.mouseUp(&up);
        }
        restore(with_pane_mut(label, |pane| pane.drag_prior.take()).flatten());
    })
    .await;
    moved.and(released)
}

// --- events -----------------------------------------------------------------------

static EVENT_NUMBER: AtomicIsize = AtomicIsize::new(1);

fn uptime() -> f64 {
    NSProcessInfo::processInfo().systemUptime()
}

fn flags(m: Mods) -> NSEventModifierFlags {
    let mut f = NSEventModifierFlags::empty();
    if m.shift {
        f |= NSEventModifierFlags::Shift;
    }
    if m.ctrl {
        f |= NSEventModifierFlags::Control;
    }
    if m.alt {
        f |= NSEventModifierFlags::Option;
    }
    if m.meta {
        f |= NSEventModifierFlags::Command;
    }
    f
}

/// CSS px of the view's viewport (top left origin) → its window's
/// coordinates (the view's own when it is in no window).
fn window_point(wk: &WKWebView, x: f64, y: f64) -> NSPoint {
    let p = NSPoint::new(
        x,
        if wk.isFlipped() {
            y
        } else {
            wk.bounds().size.height - y
        },
    );
    if wk.window().is_some() {
        wk.convertPoint_toView(p, None)
    } else {
        p
    }
}

fn window_number(wk: &WKWebView) -> isize {
    wk.window().map_or(0, |w| w.windowNumber())
}

fn mouse(
    wk: &WKWebView,
    kind: NSEventType,
    x: f64,
    y: f64,
    clicks: isize,
    mods: Mods,
) -> Option<Retained<NSEvent>> {
    let pressure = if kind == NSEventType::LeftMouseDown || kind == NSEventType::LeftMouseDragged {
        1.0
    } else {
        0.0
    };
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind,
        window_point(wk, x, y),
        flags(mods),
        uptime(),
        window_number(wk),
        None,
        EVENT_NUMBER.fetch_add(1, Ordering::Relaxed),
        clicks,
        pressure,
    )
}

/// The pointer over `(x, y)`: `:hover`, mouseover/mousemove. Sent to the
/// view's tracking-area owner; the web view itself ignores `mouseMoved:`.
fn hover(wk: &WKWebView, x: f64, y: f64, mods: Mods) {
    let Some(event) = mouse(wk, NSEventType::MouseMoved, x, y, 0, mods) else {
        return;
    };
    let me: *const AnyObject = (wk as *const WKWebView).cast();
    for area in wk.trackingAreas().iter() {
        let Some(owner) = area.owner() else { continue };
        if std::ptr::eq(Retained::as_ptr(&owner), me) {
            continue;
        }
        // SAFETY: `respondsToSelector:` on a live object; `mouseMoved:` is
        // NSResponder's, taking an NSEvent, once it says it responds.
        unsafe {
            let responds: bool = msg_send![&*owner, respondsToSelector: sel!(mouseMoved:)];
            if responds {
                let _: () = msg_send![&*owner, mouseMoved: &*event];
                return;
            }
        }
    }
    wk.mouseMoved(&event);
}

fn key_events(wk: &WKWebView, key: &KeyStroke) -> Option<(Retained<NSEvent>, Retained<NSEvent>)> {
    let chars = NSString::from_str(&key.chars);
    let ignoring = NSString::from_str(&key.ignoring);
    let make = |kind: NSEventType| {
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind,
            NSPoint::new(0.0, 0.0),
            flags(key.mods),
            uptime(),
            window_number(wk),
            None,
            &chars,
            &ignoring,
            false,
            key.code,
        )
    };
    Some((make(NSEventType::KeyDown)?, make(NSEventType::KeyUp)?))
}

/// A pixel wheel event over `(x, y)`; `dy > 0` scrolls the page down.
fn wheel(
    wk: &WKWebView,
    mtm: MainThreadMarker,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
) -> Option<Retained<NSEvent>> {
    // A wheel's positive delta moves content down, i.e. scrolls up.
    let delta = |v: f64| (-v).round().clamp(-100_000.0, 100_000.0) as i32;
    let event = CGEvent::new_scroll_wheel_event2(
        None,
        CGScrollEventUnit::Pixel,
        2,
        delta(dy),
        delta(dx),
        0,
    )?;
    if let Some(window) = wk.window() {
        let at = window.convertPointToScreen(window_point(wk, x, y));
        // CG's global space has its origin at the top left of the primary
        // screen (AppKit's at its bottom left).
        let primary = NSScreen::screens(mtm)
            .firstObject()
            .map_or(0.0, |s| s.frame().size.height);
        CGEvent::set_location(Some(&*event), NSPoint::new(at.x, primary - at.y));
        let number = window.windowNumber() as i64;
        CGEvent::set_integer_value_field(
            Some(&*event),
            CGEventField::MouseEventWindowUnderMousePointer,
            number,
        );
        CGEvent::set_integer_value_field(
            Some(&*event),
            CGEventField::MouseEventWindowUnderMousePointerThatCanHandleThisEvent,
            number,
        );
    }
    NSEvent::eventWithCGEvent(&event)
}

/// The agent cursor at `(x, y)` (a ripple when `click`), on a view that
/// draws it.
fn cursor_at(label: &str, wk: &WKWebView, mtm: MainThreadMarker, x: f64, y: f64, click: bool) {
    let Some(style) = with_pane(label, |pane| pane.cursor.clone()).flatten() else {
        return;
    };
    let arg = json!({"x": x, "y": y, "click": click, "color": style.color, "label": style.label});
    // SAFETY: main thread, a live view.
    unsafe { fire(wk, mtm, "cursor", &arg) };
}

// --- eval -------------------------------------------------------------------------

async fn eval(
    app: &AppHandle,
    label: String,
    body: String,
    args: Option<Value>,
    world: World,
    timeout_ms: u64,
) -> Result<Value, String> {
    let world = match world {
        World::Isolated => Realm::Agent,
        World::Page => Realm::Page,
    };
    let params = drive::eval_params(args.as_ref());
    let source = drive::eval_source(&body, &params);
    let args_json = args.map_or_else(|| "{}".to_string(), |a| a.to_string());
    let run = js(
        app,
        label.clone(),
        world,
        source,
        vec![("__chimaeraArgs", args_json)],
    );
    match tokio::time::timeout(Duration::from_millis(timeout_ms), run).await {
        Err(_) => Err(format!("the script did not finish within {timeout_ms} ms")),
        Ok(Ok(Some(text))) => drive::eval_answer(&text),
        Ok(Ok(None)) => Err("the page answered something else".into()),
        Ok(Err(JsFail::Gone(why))) => Err(why),
        Ok(Err(JsFail::Script(webkit))) => {
            // The wrapper catches every throw, so this is a body that
            // doesn't parse: ask the engine to name the error.
            let check = js(
                app,
                label,
                world,
                drive::SYNTAX_CHECK_SOURCE.to_string(),
                vec![
                    ("__chimaeraParams", params.join(", ")),
                    ("__chimaeraBody", body),
                ],
            );
            match tokio::time::timeout(Duration::from_secs(2), check).await {
                Ok(Ok(Some(message))) if !message.is_empty() => Err(cap(message)),
                _ => Err(format!("the script did not run: {webkit}")),
            }
        }
    }
}

fn cap(mut s: String) -> String {
    if s.len() > drive::ERROR_MAX {
        let mut end = drive::ERROR_MAX - 3;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push('…');
    }
    s
}

// --- screenshots ------------------------------------------------------------------

const MEASURE: &str = "return JSON.stringify({\
       h: Math.max(document.documentElement?.scrollHeight ?? 0, document.body?.scrollHeight ?? 0),\
       x: scrollX, y: scrollY });";
/// Two frames (or 150 ms, for a throttled view) for the grown layout, then
/// the document's height in it.
const SETTLE: &str = "await new Promise((r) => {\
       let done = false;\
       const go = () => { if (!done) { done = true; r(); } };\
       requestAnimationFrame(() => requestAnimationFrame(go));\
       setTimeout(go, 150);\
     });\
     return JSON.stringify(Math.max(document.documentElement?.scrollHeight ?? 0, document.body?.scrollHeight ?? 0));";
const SCROLL_BACK: &str = "const p = JSON.parse(__chimaeraArgs); scrollTo(p.x, p.y); return '';";

/// How a screenshot is encoded.
#[derive(Debug, Clone, Copy)]
struct Encoding {
    format: ShotFormat,
    quality: u8,
}

async fn screenshot(
    app: &AppHandle,
    label: String,
    clip: Option<Clip>,
    full_page: bool,
    max_width: u32,
    encoding: Encoding,
) -> Result<Value, String> {
    if !full_page {
        return snap(app, label, clip, max_width, None, encoding).await;
    }
    let measured = js(app, label.clone(), Realm::Agent, MEASURE.into(), vec![])
        .await
        .map_err(JsFail::text)?
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or(Value::Null);
    let num = |k: &str| measured.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let (height, scroll) = (num("h"), (num("x"), num("y")));
    let target = height.clamp(1.0, drive::FULL_PAGE_MAX);
    let frames = {
        let label = label.clone();
        on_main(app, move |_mtm| grow(&label, target)).await?
    };
    // Grown only if the page is taller than its viewport.
    let mut tall = None;
    if frames.is_some() {
        if let Ok(Some(t)) = js(app, label.clone(), Realm::Agent, SETTLE.into(), vec![]).await {
            tall = t.parse::<f64>().ok();
        }
    }
    let limit = tall.map(|h| h.clamp(1.0, drive::FULL_PAGE_MAX));
    let shot = snap(app, label.clone(), clip, max_width, limit, encoding).await;
    if let Some(grown) = frames {
        let back = label.clone();
        let _ = on_main(app, move |_mtm| {
            with_pane(&back, |pane| {
                // A page that placed the view meanwhile keeps its place.
                if pane.clip.frame() == grown.box_grown {
                    pane.clip.setFrame(grown.box_saved);
                    pane.wk.setFrame(grown.page_saved);
                }
            });
            Ok(())
        })
        .await;
        let _ = js(
            app,
            label,
            Realm::Agent,
            SCROLL_BACK.into(),
            vec![(
                "__chimaeraArgs",
                json!({"x": scroll.0, "y": scroll.1}).to_string(),
            )],
        )
        .await;
    }
    shot
}

/// Grow the view to `height` CSS px, out of sight beside its owner (a pane
/// the user sees mustn't spill 8000 px over the workbench): its clip box
/// moves there at the grown size, the page at the box's origin. The frames
/// to put back, or `None` when it is already that tall.
fn grow(label: &str, height: f64) -> Result<Option<Grown>, String> {
    let (wk, clip) =
        with_pane(label, |pane| (pane.wk.clone(), pane.clip.clone())).ok_or("no such view")?;
    let page_saved = wk.frame();
    if height <= page_saved.size.height + 0.5 {
        return Ok(None);
    }
    let box_saved = clip.frame();
    // SAFETY: the clip box's superview is the owner's live web view (we
    // added it).
    let owner_width = unsafe { clip.superview() }.map_or(0.0, |o| o.bounds().size.width);
    let box_grown = NSRect::new(
        NSPoint::new(
            owner_width.max(box_saved.origin.x + box_saved.size.width) + OFF_BOUNDS_GAP,
            0.0,
        ),
        NSSize::new(page_saved.size.width, height),
    );
    clip.setFrame(box_grown);
    wk.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), box_grown.size));
    Ok(Some(Grown {
        box_saved,
        page_saved,
        box_grown,
    }))
}

/// A view grown for a full-page screenshot: its clip box's and page's
/// frames before, and the box's grown frame (a page that placed the view
/// meanwhile has moved the box off it).
#[derive(Clone, Copy)]
struct Grown {
    box_saved: NSRect,
    page_saved: NSRect,
    box_grown: NSRect,
}

/// A snapshot's pixels on their way off the main thread.
struct Frame<T>(T);

// SAFETY: a CGImage is immutable and safe to use from any thread (Quartz's
// documented contract); nothing else is in a Frame.
unsafe impl<T> Send for Frame<T> {}

/// A snapshot handed off the main thread, or why there is none.
type Shot = Result<Frame<Retained<CGImage>>, String>;

/// Snapshot the viewport (or `clip` of it, or the top `limit` px of a grown
/// view) at one pixel per CSS px, no wider than `max_width`, encoded off
/// the main thread.
async fn snap(
    app: &AppHandle,
    label: String,
    clip: Option<Clip>,
    max_width: u32,
    limit: Option<f64>,
    encoding: Encoding,
) -> Result<Value, String> {
    let (tx, rx) = tokio::sync::oneshot::channel::<Shot>();
    let css_width = on_main(app, move |mtm| {
        let wk = with_pane(&label, |pane| pane.wk.clone()).ok_or("no such view")?;
        let bounds = wk.bounds();
        let height = limit.map_or(bounds.size.height, |l| l.min(bounds.size.height));
        let whole = Clip {
            x: 0.0,
            y: 0.0,
            width: bounds.size.width,
            height,
        };
        let region = match clip {
            Some(c) => drive::clip_within(c, whole.width, whole.height)
                .ok_or("the clip is outside the page's viewport")?,
            None => whole,
        };
        let px_width = drive::shot_width(region.width, max_width);
        // WebKit renders at the window's backing scale: ask for points that
        // come out as `px_width` pixels.
        let scale = wk.window().map_or(1.0, |w| w.backingScaleFactor()).max(1.0);
        let rect = NSRect::new(
            NSPoint::new(
                region.x,
                if wk.isFlipped() {
                    region.y
                } else {
                    bounds.size.height - region.y - region.height
                },
            ),
            NSSize::new(region.width, region.height),
        );
        let send = once(Box::new(move |r: Shot| {
            let _ = tx.send(r);
        }) as Box<dyn FnOnce(Shot) + Send>);
        let block = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            // SAFETY: WebKit hands either a live image or a live error.
            let result = match unsafe { (image.as_ref(), error.as_ref()) } {
                // SAFETY: a null proposed rect asks for the image's own best
                // representation; no context or hints.
                (Some(image), _) => unsafe {
                    image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None)
                }
                .map(Frame)
                .ok_or_else(|| "no image data".to_string()),
                (None, Some(error)) => Err(error.localizedDescription().to_string()),
                (None, None) => Err("the view drew nothing".to_string()),
            };
            send(result);
        });
        // SAFETY: a live WKWebView on the main thread; a fresh configuration.
        unsafe {
            let config = WKSnapshotConfiguration::new(mtm);
            config.setRect(rect);
            config.setSnapshotWidth(Some(&NSNumber::new_f64(px_width / scale)));
            wk.takeSnapshotWithConfiguration_completionHandler(Some(&config), &block);
        }
        Ok(region.width)
    })
    .await?;
    let frame = rx.await.map_err(|_| "the view closed".to_string())??;
    tokio::task::spawn_blocking(move || {
        let frame = frame;
        objc2::rc::autoreleasepool(|_| encode(&frame.0, css_width, encoding))
    })
    .await
    .map_err(|_| "encoding the screenshot failed".to_string())?
}

fn encode(cg: &CGImage, css_width: f64, encoding: Encoding) -> Result<Value, String> {
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), cg);
    let (kind, quality) = match encoding.format {
        ShotFormat::Png => (NSBitmapImageFileType::PNG, None),
        ShotFormat::Jpeg => (
            NSBitmapImageFileType::JPEG,
            Some(NSNumber::new_f64(f64::from(encoding.quality) / 100.0)),
        ),
    };
    let properties = match &quality {
        // SAFETY: an AppKit constant, read on any thread.
        Some(q) => NSDictionary::from_slices(
            &[unsafe { NSImageCompressionFactor }],
            &[q.as_ref() as &AnyObject],
        ),
        None => NSDictionary::new(),
    };
    // SAFETY: the properties are empty, or JPEG's compression factor as an
    // NSNumber.
    let data = unsafe { rep.representationUsingType_properties(kind, &properties) }
        .ok_or("encoding the screenshot failed")?;
    let (width, height) = (rep.pixelsWide(), rep.pixelsHigh());
    let name = encoding.format.name();
    Ok(json!({
        name: base64::engine::general_purpose::STANDARD.encode(data.to_vec()),
        "format": name,
        "width": width,
        "height": height,
        // Image px per CSS px: divide a point in the image by it to click it.
        "scale": if css_width > 0.0 { width as f64 / css_width } else { 1.0 },
    }))
}

// --- keys WebKit hands back ---------------------------------------------------------

/// How long an injected keyDown is remembered (WebKit re-sends it once the
/// page has answered; a hung page can take a while).
const RESENT_WINDOW: Duration = Duration::from_secs(30);
const RESENT_MAX: usize = 512;

thread_local! {
    /// Main thread only: injected keyDowns, retained so no real event can
    /// take one's address while it is remembered.
    static INJECTED_KEYS: RefCell<VecDeque<(Retained<NSEvent>, Instant)>> =
        const { RefCell::new(VecDeque::new()) };
}

/// The app's `sendEvent:` before `guard_resend` wrapped it (`None`: not
/// wrapped — the class has no such method).
static PREVIOUS_SEND_EVENT: OnceLock<Option<Imp>> = OnceLock::new();

type SendEventImp = unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut NSEvent);

fn remember_key(event: &Retained<NSEvent>, mtm: MainThreadMarker) {
    guard_resend(mtm);
    let now = Instant::now();
    INJECTED_KEYS.with(|keys| {
        let mut keys = keys.borrow_mut();
        while keys
            .front()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) > RESENT_WINDOW)
            || keys.len() >= RESENT_MAX
        {
            keys.pop_front();
        }
        keys.push_back((event.clone(), now));
    });
}

/// Wrap the app's `sendEvent:` once so WebKit's re-send of an injected
/// keyDown goes nowhere (see the module header).
fn guard_resend(mtm: MainThreadMarker) {
    PREVIOUS_SEND_EVENT.get_or_init(|| {
        let app = NSApplication::sharedApplication(mtm);
        let class: &AnyClass = app.class();
        // SAFETY: the runtime's own lookups on a live class; `send_event`
        // has `sendEvent:`'s C ABI — (id self, SEL _cmd, NSEvent *) -> void
        // — which "v@:@" spells. The previous implementation is read before
        // it is replaced, and is the class's own or the one it inherits.
        unsafe {
            let method = objc2::ffi::class_getInstanceMethod(class, sel!(sendEvent:));
            if method.is_null() {
                tracing::warn!("agent keys: the app has no sendEvent: to guard");
                return None;
            }
            let previous = objc2::ffi::method_getImplementation(method)?;
            let imp = std::mem::transmute::<SendEventImp, Imp>(send_event);
            objc2::ffi::class_replaceMethod(
                (class as *const AnyClass).cast_mut(),
                sel!(sendEvent:),
                imp,
                c"v@:@".as_ptr(),
            );
            Some(previous)
        }
    });
}

/// `-[NSApplication sendEvent:]`, wrapped: an injected keyDown (WebKit
/// re-sending it, the only way one gets here) is dropped; everything else
/// goes on as before.
unsafe extern "C-unwind" fn send_event(this: &AnyObject, cmd: Sel, event: *mut NSEvent) {
    let ours = !event.is_null()
        && INJECTED_KEYS.with(|keys| {
            keys.borrow()
                .iter()
                .any(|(e, _)| std::ptr::eq(Retained::as_ptr(e), event.cast_const()))
        });
    if ours {
        return;
    }
    if let Some(Some(previous)) = PREVIOUS_SEND_EVENT.get() {
        // SAFETY: the implementation this one replaced, with its own ABI.
        unsafe {
            let previous = std::mem::transmute::<Imp, SendEventImp>(*previous);
            previous(this, cmd, event);
        }
    }
}
