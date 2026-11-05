//! Pane views: a page shown inside a pane but rendered by a web view of its
//! own, laid over the pane's rectangle.
//!
//! Why: an `<iframe>` runs in its window's web process, on the main thread
//! that also serves chat, terminals and every other pane. A 40 MB report
//! (3.4 M elements) cost that process 1–1.7 GB, pushed it into WebKit's
//! memory-pressure handling (which drops style caches), and from then on an
//! ordinary mouse move restyled the whole report — the window froze, chat
//! included (measured 2026-11-04). A view of its own gets its own web
//! process, so a heavy page can only slow itself down.
//!
//! The page owns layout: it reports the pane's rect (CSS px of the window's
//! web view, which are the window's points) and whether the view is shown;
//! the shell creates, moves, hides and closes. A view belongs to the window
//! web view that asked for it (its owner) and closes with it, and when that
//! page navigates or reloads. The page reaches into a view only through the
//! pane bridge (`chimaera_core::PANE_BRIDGE_JS`): selections, a region's
//! text, focus and chords come back as `native-view` events on the owner;
//! snapshots, the reference pill and back/forward go the other way.
//!
//! macOS only for now (`mac.rs`: a `WKWebView` subview the shell owns, not a
//! Tauri web view — see its header for why). Elsewhere `native_view_show`
//! refuses and the page keeps its iframe.
//!
//! Security: a view's main frame loads only its owner's daemon origin, under
//! `/raw/` (a ticketed file, served with a sandbox CSP) or `/proxy/` (the
//! browser pane's reverse proxy). A proxied app's root-form navigation is
//! put back under its `/proxy/{id}`; a clicked link anywhere else goes to the
//! user's browser through `open_external`'s rules, and a script's own
//! navigation away is just cancelled. The view is not a Tauri web view, so
//! it has no IPC at all.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, Webview};

#[cfg(target_os = "macos")]
mod mac;

use super::{daemon_navigation_allowed, lock};

/// What a page asks for: the pane's rect in CSS px.
#[derive(Debug, Clone, Copy, Deserialize)]
pub(super) struct ViewRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl ViewRect {
    fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite() && v.abs() < 100_000.0)
            && self.width >= 0.0
            && self.height >= 0.0
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn parts(&self) -> (f64, f64, f64, f64) {
        (self.x, self.y, self.width, self.height)
    }
}

/// The box a view shows through: `rect` cut to `clip` (the visible part of
/// the scroller an embed card sits in), or `None` when nothing of it is
/// left. The page keeps `rect`'s position inside that box, so a card half
/// scrolled out is cropped rather than squeezed or shifted.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn clip_box(rect: ViewRect, clip: Option<ViewRect>) -> Option<ViewRect> {
    let Some(clip) = clip else {
        return Some(rect);
    };
    let x = rect.x.max(clip.x);
    let y = rect.y.max(clip.y);
    let right = (rect.x + rect.width).min(clip.x + clip.width);
    let bottom = (rect.y + rect.height).min(clip.y + clip.height);
    (right - x >= 1.0 && bottom - y >= 1.0).then_some(ViewRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

/// One view: whose it is, its page-chosen id, and the URL the page last
/// asked for. In-page navigation leaves the asked-for URL alone, so asking
/// again for the same URL never reloads the view.
struct View {
    owner: String,
    id: String,
    url: String,
}

/// The views this app holds, by child label.
#[derive(Default)]
pub(super) struct NativeViews {
    views: Mutex<HashMap<String, View>>,
}

/// `(owner label, view id)` of a live view.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn owner_of(app: &AppHandle, label: &str) -> Option<(String, String)> {
    let views = app.try_state::<NativeViews>()?;
    let views = lock(&views.views);
    views.get(label).map(|v| (v.owner.clone(), v.id.clone()))
}

/// `native_view_nav`'s actions.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum NavAction {
    Back,
    Forward,
    Reload,
    Stop,
}

/// What a view does with a main-frame navigation.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, PartialEq)]
enum Policy {
    Allow,
    /// A proxied app's root-form URL, put back under its prefix.
    Rescue(tauri::Url),
    /// Not for this view: the user's browser, if the user clicked it.
    HandOff(tauri::Url),
}

/// A page-chosen view id: short, label-safe.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn child_label(owner: &str, id: &str) -> String {
    format!("{owner}--nv-{id}")
}

/// The paths a view may show on its owner's daemon origin.
fn view_path_allowed(url: &tauri::Url) -> bool {
    let path = url.path();
    path.starts_with("/raw/") || path.starts_with("/proxy/")
}

/// `/proxy/{id}` of a proxied app's URL, or `None` for anything else.
fn proxy_base(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let path = &rest[rest.find('/')?..];
    let id = path
        .strip_prefix("/proxy/")?
        .split(['/', '?', '#'])
        .next()?;
    (!id.is_empty()).then(|| format!("/proxy/{id}"))
}

/// The pure part of the policy: `same_origin` = `url` is on the owner's
/// daemon origin; `asked` = the URL the page last asked the view for.
fn decide(asked: &str, same_origin: bool, url: &tauri::Url) -> Policy {
    // The only non-daemon document a view's main frame may hold: its own
    // empty start. A page's `blob:`/`data:` frames are subframes (allowed
    // there); a top-level one would be a script navigating away.
    if url.as_str() == "about:blank" {
        return Policy::Allow;
    }
    if same_origin && view_path_allowed(url) {
        return Policy::Allow;
    }
    // A proxied app built for `/` navigating its whole document to a
    // root-form path (`/lab/tree/x`) reaches the daemon's own root; the
    // daemon only rescues subresources, so the view puts it back.
    if same_origin {
        if let Some(base) = proxy_base(asked) {
            let mut rescued = url.clone();
            rescued.set_path(&format!("{base}{}", url.path()));
            return Policy::Rescue(rescued);
        }
    }
    Policy::HandOff(url.clone())
}

/// The policy for a main-frame navigation in view `label`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn main_frame_policy(app: &AppHandle, label: &str, url: &tauri::Url) -> Policy {
    let Some(views) = app.try_state::<NativeViews>() else {
        return Policy::HandOff(url.clone());
    };
    let (owner, asked) = {
        let views = lock(&views.views);
        match views.get(label) {
            Some(view) => (view.owner.clone(), view.url.clone()),
            None => return Policy::HandOff(url.clone()),
        }
    };
    decide(&asked, daemon_navigation_allowed(app, &owner, url), url)
}

/// A link the user clicked lets that URL out to their browser this long.
const LINK_FRESH: Duration = Duration::from_millis(1500);

/// Whether a navigation to `url` may go to the user's browser: it is the
/// link the user clicked at `at` (a trusted click the bridge reported), the
/// same URL bar the fragment, and recent. A script's `a.click()` is a link
/// activation to WebKit too, so this — not `navigationType` — is the gate.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn link_lets_out(href: &str, at: Instant, now: Instant, url: &tauri::Url) -> bool {
    if now.saturating_duration_since(at) >= LINK_FRESH {
        return false;
    }
    href.parse::<tauri::Url>().is_ok_and(|mut h| {
        let mut u = url.clone();
        h.set_fragment(None);
        u.set_fragment(None);
        h == u
    })
}

/// A link out of a view: the user's real browser for web URLs (validated
/// the same way as every other rendered link), nothing for the rest.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn hand_off(url: &tauri::Url) {
    if matches!(url.scheme(), "http" | "https") {
        let _ = super::commands::open_external(url.to_string());
    }
}

/// The owner's own view `id`'s label, if the id is well-formed.
fn own_label(webview: &Webview, id: &str) -> Result<String, String> {
    if !valid_id(id) {
        return Err("bad view request".into());
    }
    Ok(child_label(webview.label(), id))
}

#[cfg(not(target_os = "macos"))]
const UNSUPPORTED: &str = "pane views are macOS-only for now";

/// What `native_view_show` answers: the placement options this shell
/// honours. A page that needs `clip`/`passive` (an embed card) checks it:
/// an older shell drops unknown arguments without a word, and a card it
/// laid out uncropped would draw over the window's chrome.
#[derive(Debug, Serialize)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) struct Shown {
    fit: bool,
}

/// Create the view, or point an existing one at `url` (only when it
/// changed), then place and show it.
// One named field per page-side option: a struct would only rename them.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(super) fn native_view_show(
    app: AppHandle,
    webview: Webview,
    id: String,
    url: String,
    rect: ViewRect,
    hidden: Option<bool>,
    clip: Option<ViewRect>,
    passive: Option<bool>,
) -> Result<Shown, String> {
    let label = own_label(&webview, &id)?;
    let hidden = hidden.unwrap_or(false);
    if !rect.valid() || clip.is_some_and(|c| !c.valid()) {
        return Err("bad view request".into());
    }
    let owner = webview.label().to_string();
    let parsed = webview
        .url()
        .map_err(|e| e.to_string())?
        .join(&url)
        .map_err(|_| "not a URL".to_string())?;
    if !daemon_navigation_allowed(&app, &owner, &parsed) || !view_path_allowed(&parsed) {
        return Err("a pane view shows only this daemon's files and proxied apps".into());
    }
    tracing::debug!(%label, path = parsed.path(), ?rect, "native view: show");
    let views = app.state::<NativeViews>();
    let previous = lock(&views.views).insert(
        label.clone(),
        View {
            owner,
            id,
            url: parsed.to_string(),
        },
    );
    let navigate = previous.is_some_and(|v| v.url != parsed.as_str());
    #[cfg(target_os = "macos")]
    {
        let shown = mac::show(
            &app,
            &webview,
            label.clone(),
            parsed,
            navigate,
            rect,
            clip,
            passive.unwrap_or(false),
            hidden,
        );
        if shown.is_err() {
            // Never made (the owner is going): no record of a view that
            // doesn't exist.
            lock(&views.views).remove(&label);
        }
        shown.map(|()| Shown { fit: true })
    }
    #[cfg(not(target_os = "macos"))]
    {
        lock(&views.views).remove(&label);
        let _ = (navigate, hidden, clip, passive);
        Err(UNSUPPORTED.into())
    }
}

/// Move a view, or hide it (`rect` absent): a parked tab, an overlay that
/// must draw above it, a load still under way. `clip` crops it to the part
/// of its scroller on screen (an embed card); `passive` hands the wheel
/// over it back to the page, so a transcript scrolls past a card instead of
/// sticking in it until the card is clicked.
#[tauri::command]
pub(super) fn native_view_place(
    webview: Webview,
    id: String,
    rect: Option<ViewRect>,
    clip: Option<ViewRect>,
    passive: Option<bool>,
) -> Result<(), String> {
    let label = own_label(&webview, &id)?;
    if rect.is_some_and(|r| !r.valid()) || clip.is_some_and(|c| !c.valid()) {
        return Err("bad view request".into());
    }
    tracing::trace!(%label, ?rect, ?clip, ?passive, "native view: place");
    #[cfg(target_os = "macos")]
    {
        mac::place(&webview, label, rect, clip, passive.unwrap_or(false))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (label, rect, clip, passive);
        Ok(())
    }
}

/// Close a view (its tab closed, or the page no longer wants it).
#[tauri::command]
pub(super) fn native_view_close(
    app: AppHandle,
    webview: Webview,
    id: String,
) -> Result<(), String> {
    let label = own_label(&webview, &id)?;
    close_view(&app, &label);
    Ok(())
}

/// The view's visible viewport as a PNG (the area tool draws over it).
#[tauri::command]
pub(super) async fn native_view_snapshot(
    app: AppHandle,
    webview: Webview,
    id: String,
) -> Result<serde_json::Value, String> {
    let label = own_label(&webview, &id)?;
    #[cfg(target_os = "macos")]
    {
        let snapshot = mac::snapshot(&app, label).await?;
        serde_json::to_value(snapshot).map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, label);
        Err(UNSUPPORTED.into())
    }
}

/// The reference pill the bridge draws by the page's selection (`null`
/// removes it): the page knows the agent and the chord, the bridge doesn't.
#[tauri::command]
pub(super) async fn native_view_chip(
    app: AppHandle,
    webview: Webview,
    id: String,
    chip: Option<serde_json::Value>,
) -> Result<(), String> {
    let label = own_label(&webview, &id)?;
    let chip = chip.unwrap_or(serde_json::Value::Null);
    if chip.to_string().len() > 4096 {
        return Err("bad view request".into());
    }
    #[cfg(target_os = "macos")]
    {
        mac::call(&app, label, "chip", chip).await.map(|_| ())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, label, chip);
        Err(UNSUPPORTED.into())
    }
}

/// The visible text inside `rect` (view CSS px), with the page's scroll
/// offsets: the area tool's text and locator.
#[tauri::command]
pub(super) async fn native_view_region(
    app: AppHandle,
    webview: Webview,
    id: String,
    rect: ViewRect,
) -> Result<serde_json::Value, String> {
    let label = own_label(&webview, &id)?;
    if !rect.valid() {
        return Err("bad view request".into());
    }
    let arg =
        serde_json::json!({"x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height});
    #[cfg(target_os = "macos")]
    {
        mac::call(&app, label, "region", arg).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, label, arg);
        Err(UNSUPPORTED.into())
    }
}

/// Back, forward, reload or stop the view's page.
#[tauri::command]
pub(super) fn native_view_nav(
    app: AppHandle,
    webview: Webview,
    id: String,
    action: NavAction,
) -> Result<(), String> {
    let label = own_label(&webview, &id)?;
    #[cfg(target_os = "macos")]
    {
        mac::nav(&app, label, action)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, label, action);
        Err(UNSUPPORTED.into())
    }
}

fn close_view(app: &AppHandle, label: &str) {
    if let Some(views) = app.try_state::<NativeViews>() {
        if lock(&views.views).remove(label).is_none() {
            return;
        }
    }
    tracing::debug!(%label, "native view: close");
    #[cfg(target_os = "macos")]
    mac::close(app, label.to_string());
}

/// Close every view `owner` holds: its page navigated or reloaded (the new
/// page knows nothing of them), or its window is going.
pub(super) fn close_owned(app: &AppHandle, owner: &str) {
    let Some(views) = app.try_state::<NativeViews>() else {
        return;
    };
    let labels: Vec<String> = lock(&views.views)
        .iter()
        .filter(|(_, v)| v.owner == owner)
        .map(|(label, _)| label.clone())
        .collect();
    for label in labels {
        close_view(app, &label);
    }
}

/// The owner page's load hook: a new document starting in the window's own
/// web view drops the old page's views.
pub(super) fn owner_page_load(app: &AppHandle, owner: &str, event: PageLoadEvent) {
    if matches!(event, PageLoadEvent::Started) {
        close_owned(app, owner);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        child_label, clip_box, decide, link_lets_out, proxy_base, valid_id, view_path_allowed,
        Policy, ViewRect,
    };

    fn r(x: f64, y: f64, width: f64, height: f64) -> ViewRect {
        ViewRect {
            x,
            y,
            width,
            height,
        }
    }

    fn parts(v: Option<ViewRect>) -> Option<(f64, f64, f64, f64)> {
        v.map(|v| v.parts())
    }

    #[test]
    fn a_card_is_cropped_to_its_scroller() {
        let card = r(10.0, 80.0, 400.0, 360.0);
        // No clip: the view is the card.
        assert_eq!(parts(clip_box(card, None)), Some(card.parts()));
        // Half scrolled out of the top: only the part still on screen.
        assert_eq!(
            parts(clip_box(card, Some(r(0.0, 200.0, 800.0, 600.0)))),
            Some((10.0, 200.0, 400.0, 240.0))
        );
        // Wholly on screen: unchanged.
        assert_eq!(
            parts(clip_box(card, Some(r(0.0, 0.0, 800.0, 900.0)))),
            Some(card.parts())
        );
        // Scrolled out, or a sliver under a pixel: nothing to show.
        assert_eq!(
            parts(clip_box(card, Some(r(0.0, 440.0, 800.0, 300.0)))),
            None
        );
        assert_eq!(
            parts(clip_box(card, Some(r(0.0, 439.5, 800.0, 300.0)))),
            None
        );
    }

    #[test]
    fn only_the_link_the_user_clicked_goes_out() {
        let url = |s: &str| -> tauri::Url { s.parse().unwrap() };
        let at = Instant::now();
        let soon = at + Duration::from_millis(200);
        let target = url("https://example.com/a?b=1");
        assert!(link_lets_out(
            "https://example.com/a?b=1",
            at,
            soon,
            &target
        ));
        // The fragment doesn't make it another place.
        assert!(link_lets_out(
            "https://example.com/a?b=1#x",
            at,
            soon,
            &target
        ));
        // Anything else — another URL, a stale click, garbage — stays in.
        assert!(!link_lets_out(
            "https://example.com/a?b=2",
            at,
            soon,
            &target
        ));
        assert!(!link_lets_out(
            "https://evil.example/a?b=1",
            at,
            soon,
            &target
        ));
        assert!(!link_lets_out(
            "https://example.com/a?b=1",
            at,
            at + Duration::from_secs(2),
            &target
        ));
        assert!(!link_lets_out("not a url", at, soon, &target));
    }

    #[test]
    fn ids_are_label_safe() {
        assert!(valid_id("html-1"));
        assert!(valid_id("a_B-9"));
        for bad in ["", "a b", "a/b", "a:b", "é", &"x".repeat(65)] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert_eq!(child_label("win-3", "html-1"), "win-3--nv-html-1");
    }

    #[test]
    fn only_files_and_proxied_apps() {
        for ok in [
            "http://127.0.0.1:9/raw/t-1/report.html",
            "http://127.0.0.1:9/proxy/p1/app/",
        ] {
            assert!(view_path_allowed(&ok.parse().unwrap()), "{ok}");
        }
        for bad in [
            "http://127.0.0.1:9/",
            "http://127.0.0.1:9/api/v1/fs/file",
            "http://127.0.0.1:9/download/t-1",
            "http://127.0.0.1:9/rawx/t-1",
        ] {
            assert!(!view_path_allowed(&bad.parse().unwrap()), "{bad}");
        }
    }

    #[test]
    fn a_proxied_apps_base() {
        assert_eq!(
            proxy_base("http://127.0.0.1:9/proxy/p1/lab?x=1").as_deref(),
            Some("/proxy/p1")
        );
        assert_eq!(
            proxy_base("http://127.0.0.1:9/proxy/p2").as_deref(),
            Some("/proxy/p2")
        );
        assert_eq!(proxy_base("http://127.0.0.1:9/raw/t-1/r.html"), None);
        assert_eq!(proxy_base("http://127.0.0.1:9/proxy/"), None);
    }

    #[test]
    fn the_main_frame_policy() {
        let url = |s: &str| -> tauri::Url { s.parse().unwrap() };
        let app = "http://127.0.0.1:9/proxy/p1/lab";
        let report = "http://127.0.0.1:9/raw/t-1/r.html";
        // Its own kind of page stays.
        assert_eq!(
            decide(report, true, &url("http://127.0.0.1:9/raw/t-1/fig/a.html")),
            Policy::Allow
        );
        assert_eq!(
            decide(app, true, &url("http://127.0.0.1:9/proxy/p1/tree")),
            Policy::Allow
        );
        assert_eq!(decide(report, false, &url("about:blank")), Policy::Allow);
        assert_eq!(
            decide(report, false, &url("data:text/html,x")),
            Policy::HandOff(url("data:text/html,x"))
        );
        // A proxied app's root-form document goes back under its prefix…
        assert_eq!(
            decide(app, true, &url("http://127.0.0.1:9/lab/tree/x?y=1")),
            Policy::Rescue(url("http://127.0.0.1:9/proxy/p1/lab/tree/x?y=1"))
        );
        // …a report's does not (it has no app to put it under).
        assert_eq!(
            decide(report, true, &url("http://127.0.0.1:9/api/v1/x")),
            Policy::HandOff(url("http://127.0.0.1:9/api/v1/x"))
        );
        // The web is never loaded in a view.
        assert_eq!(
            decide(app, false, &url("https://example.com/")),
            Policy::HandOff(url("https://example.com/"))
        );
        // Another daemon's origin is not this one's.
        assert_eq!(
            decide(app, false, &url("http://127.0.0.1:10/proxy/p1/x")),
            Policy::HandOff(url("http://127.0.0.1:10/proxy/p1/x"))
        );
    }
}
