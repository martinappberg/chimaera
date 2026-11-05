//! The agent browser (#273): one set of MCP tools (`browser_*`, `tools`)
//! over two engines that each drive one page per agent.
//!
//! - The **app engine** (`app_engine`) drives a pane view in a connected
//!   macOS app window: a shell-owned `WKWebView` the window's page creates
//!   off its own bounds (or a pane the user shared). Requests ride
//!   `/ws/events` to that window (`channel`) and come back on the same socket.
//! - The **Chromium engine** (`chromium`, `cdp`) drives a headless Chrome or
//!   Chromium already on the daemon's host over `--remote-debugging-pipe`.
//!
//! The app engine is used whenever a capable window of the session's
//! workspace is connected, Chromium otherwise. Every result names the
//! engine, and a page never moves between engines: the two hold different
//! page state (a shared pane carries the user's logins).
//!
//! Navigation obeys the proxy's mint allowlist (`proxy::check_target`) on
//! both engines. What a page says is data, never instructions.
//!
//! Pages (this file's [`Browsers`]): each agent has at most one page of its
//! own (an off-screen app view or a Chromium target, bound to its engine and
//! window for life), plus the panes the user shared with it through the
//! chain link (`links`, addressed `p1`, `p2`…; always app engine). A shared
//! pane the user acts in is paused for the agent's act tools until the user
//! resumes it (CONTRACTS §6). Bounded: one own page per agent, at most
//! [`APP_VIEWS_MAX`] app views per daemon, own pages reaped after
//! [`IDLE_REAP`] idle, every map pruned when its agent or link goes.

pub(crate) mod app_engine;
mod cdp;
pub(crate) mod channel;
pub(crate) mod chromium;
pub(crate) mod tools;

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which engine answered: named in every tool result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum EngineKind {
    App,
    Chromium,
}

impl fmt::Display for EngineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EngineKind::App => "app",
            EngineKind::Chromium => "chromium",
        })
    }
}

/// Where an `Eval` runs: the engine's isolated world (the shell's agent
/// content world on the app engine, separate from the pane bridge's so an
/// agent can't forge bridge reports; an isolated world on Chromium), or the
/// page's own world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum World {
    Isolated,
    Page,
}

/// An allowlisted target, as the agent named it (`http://host:port/path`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TargetUrl {
    pub(crate) host: String,
    pub(crate) port: u16,
    /// Path + query + fragment, always starting with `/`.
    pub(crate) path: String,
}

impl TargetUrl {
    pub(crate) fn url(&self) -> String {
        if self.host.contains(':') {
            format!("http://[{}]:{}{}", self.host, self.port, self.path)
        } else {
            format!("http://{}:{}{}", self.host, self.port, self.path)
        }
    }
}

impl From<crate::browser_open::Target> for TargetUrl {
    fn from(t: crate::browser_open::Target) -> Self {
        TargetUrl {
            host: t.host,
            port: t.port,
            path: t.path,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum HistoryOp {
    Back,
    Forward,
    Reload,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Modifier {
    Shift,
    Meta,
    Ctrl,
    Alt,
}

/// CSS px in the page's viewport.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

/// A screenshot's encoding (wire: `"format": "png" | "jpeg"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    pub(crate) fn mime(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
        }
    }
}

/// What an engine does to one page. Coordinates are CSS px in the page's
/// viewport. The app engine forwards the input/eval/screenshot/rings
/// variants verbatim as `native_view_drive` actions (CONTRACTS §3).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Action {
    /// Create the page (or re-point it) at an allowlisted target.
    Open {
        url: TargetUrl,
        width: u32,
        height: u32,
    },
    Navigate {
        url: TargetUrl,
    },
    History {
        op: HistoryOp,
    },
    /// `body` is an async function body; resolves to its return value (JSON).
    Eval {
        world: World,
        body: String,
        args: Value,
        timeout_ms: u64,
    },
    Click {
        x: f64,
        y: f64,
        clicks: u8,
        modifiers: Vec<Modifier>,
    },
    // Both engines implement it (hover); no tool sends a bare move yet.
    #[allow(dead_code)]
    Move {
        x: f64,
        y: f64,
    },
    // Both engines implement it; no tool drives a pointer drag yet (HTML5
    // drag-and-drop never works with injected input, CONTRACTS spike facts).
    #[allow(dead_code)]
    Drag {
        from: (f64, f64),
        to: (f64, f64),
        steps: u16,
    },
    /// A named key ("Enter", "Tab", "Escape", "Backspace", "Delete",
    /// "ArrowUp".."ArrowRight", "Home", "End", "PageUp", "PageDown") or one
    /// character.
    Key {
        key: String,
        modifiers: Vec<Modifier>,
    },
    /// `per_key: false` inserts the text (fast, `beforeinput`/`input` only);
    /// `true` sends paced key events.
    Text {
        text: String,
        per_key: bool,
    },
    Scroll {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    /// `max_width` is in image px; `quality` (1–100) applies to JPEG only.
    /// The result is `ActionOut::Png` carrying the requested format's bytes.
    Screenshot {
        clip: Option<Rect>,
        full_page: bool,
        max_width: u32,
        format: ImageFormat,
        quality: Option<u8>,
    },
    Rings {
        console_since: u64,
        network_since: u64,
    },
    Close,
}

/// One console message or uncaught error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ConsoleEntry {
    pub(crate) id: u64,
    /// "log" | "info" | "warn" | "error" | "debug" | "exception"
    pub(crate) level: String,
    pub(crate) text: String,
    /// Milliseconds since the Unix epoch.
    pub(crate) at: f64,
}

/// One failed (status >= 400, network error) or slow (>= 1 s) request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct NetworkEntry {
    pub(crate) id: u64,
    pub(crate) method: String,
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) status: Option<u16>,
    #[serde(default)]
    pub(crate) error: Option<String>,
    #[serde(default)]
    pub(crate) ms: Option<f64>,
    pub(crate) at: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct RingsOut {
    pub(crate) console: Vec<ConsoleEntry>,
    pub(crate) network: Vec<NetworkEntry>,
    /// Requests in flight now (for `browser_wait` network idle).
    pub(crate) inflight: u32,
    pub(crate) console_next: u64,
    pub(crate) network_next: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ActionOut {
    Done,
    Value(Value),
    Png {
        b64: String,
        width: u32,
        height: u32,
    },
    Rings(RingsOut),
}

/// Every failure, as the model will read it (`Display`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BrowserError {
    /// No engine can serve this session (no capable window, no Chromium).
    NoEngine(String),
    /// Not allowed (allowlist, login node, paused by the user, limits).
    Refused(String),
    /// The page is gone (its window closed or reloaded, the browser died).
    Gone(String),
    Timeout(String),
    /// The page itself failed (a script threw, a navigation failed).
    Page(String),
    /// The engine failed (a shell or CDP error).
    Engine(String),
}

impl fmt::Display for BrowserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BrowserError::NoEngine(s)
            | BrowserError::Refused(s)
            | BrowserError::Gone(s)
            | BrowserError::Timeout(s)
            | BrowserError::Page(s)
            | BrowserError::Engine(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for BrowserError {}

// ---- pages ------------------------------------------------------------------

/// App-engine agent views per daemon (each is a web process in a window).
pub(crate) const APP_VIEWS_MAX: usize = 6;
/// An own page unused this long is closed.
pub(crate) const IDLE_REAP: Duration = Duration::from_secs(10 * 60);
/// A shared pane's "driving" marker stays up this long after the agent's
/// last act (and a take-over within it counts, CONTRACTS §6).
const DRIVING_LINGER: Duration = Duration::from_secs(15);
/// The reaper's tick; pages are reaped every [`REAP_EVERY`] ticks.
const REAP_TICK: Duration = Duration::from_secs(5);
const REAP_EVERY: u32 = 12;

/// Where an own page lives, for its whole life (a page never moves).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Bound {
    App {
        socket: channel::SocketId,
        view: String,
    },
    Chromium {
        key: String,
    },
}

impl Bound {
    pub(crate) fn engine(&self) -> EngineKind {
        match self {
            Bound::App { .. } => EngineKind::App,
            Bound::Chromium { .. } => EngineKind::Chromium,
        }
    }
}

/// Read positions in a page's console/network rings (`id >= since` is new).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Cursors {
    pub(crate) console: u64,
    pub(crate) network: u64,
}

/// An agent's own page.
#[derive(Debug, Clone)]
pub(crate) struct OwnPage {
    pub(crate) bound: Bound,
    pub(crate) last_used: Instant,
    pub(crate) cursors: Cursors,
    /// The last target it was pointed at (maps rescued paths back).
    pub(crate) target: Option<TargetUrl>,
    pub(crate) size: (u32, u32),
}

/// What the daemon keeps about a pane the user shared (keyed by tab).
#[derive(Debug, Clone, Default)]
pub(crate) struct SharedPane {
    /// The user took over at this time; act tools refuse until resume.
    pub(crate) paused: Option<SystemTime>,
    pub(crate) last_act: Option<Instant>,
    /// The window currently showing the "driving" marker for it.
    pub(crate) driving_shown: Option<channel::SocketId>,
    /// The window that installed the recorder and agent cursor in it.
    pub(crate) prepared: Option<channel::SocketId>,
    pub(crate) cursors: Cursors,
    /// The last target its URL mapped to (maps rescued paths back).
    pub(crate) origin: Option<TargetUrl>,
    /// The agent's own page handed to the user (`browser_show`), counted
    /// against [`HANDED_OVER_MAX`] while it stays linked.
    pub(crate) handed_over: bool,
    /// The page's last non-empty title (a handed-over pane's first answers
    /// can come before its window reports one).
    pub(crate) title: Option<String>,
}

/// Handed-over panes still linked to one agent, at most.
pub(crate) const HANDED_OVER_MAX: usize = 4;

#[derive(Default)]
struct Pages {
    own: HashMap<String, OwnPage>,
    shared: HashMap<String, SharedPane>,
    /// The engine of each agent's last own page: a new page on another
    /// engine says so.
    last_engine: HashMap<String, EngineKind>,
    /// One browser call at a time per agent.
    locks: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// Per agent: the `vN` id of each unshared pane it can see (stable while
    /// the tab stays listed).
    visible_ids: HashMap<String, HashMap<String, u32>>,
}

/// The agent browser's daemon-wide state (`AppState::browsers`).
pub(crate) struct Browsers {
    pub(crate) channel: channel::Channel,
    pub(crate) chromium: chromium::Chromium,
    pages: Mutex<Pages>,
}

impl Browsers {
    pub(crate) fn new() -> Self {
        Browsers {
            channel: channel::Channel::default(),
            chromium: chromium::Chromium::new(),
            pages: Mutex::new(Pages::default()),
        }
    }

    /// The lock serializing `agent`'s browser calls.
    pub(crate) fn agent_lock(&self, agent: &str) -> Arc<tokio::sync::Mutex<()>> {
        crate::lock(&self.pages)
            .locks
            .entry(agent.to_string())
            .or_default()
            .clone()
    }

    pub(crate) fn own(&self, agent: &str) -> Option<OwnPage> {
        crate::lock(&self.pages).own.get(agent).cloned()
    }

    pub(crate) fn set_own(&self, agent: &str, page: OwnPage) {
        let mut pages = crate::lock(&self.pages);
        pages
            .last_engine
            .insert(agent.to_string(), page.bound.engine());
        pages.own.insert(agent.to_string(), page);
    }

    pub(crate) fn remove_own(&self, agent: &str) -> Option<OwnPage> {
        crate::lock(&self.pages).own.remove(agent)
    }

    pub(crate) fn last_engine(&self, agent: &str) -> Option<EngineKind> {
        crate::lock(&self.pages).last_engine.get(agent).copied()
    }

    /// Change `agent`'s own page in place (no-op when it has none).
    pub(crate) fn update_own(&self, agent: &str, f: impl FnOnce(&mut OwnPage)) {
        if let Some(page) = crate::lock(&self.pages).own.get_mut(agent) {
            f(page);
        }
    }

    pub(crate) fn app_views(&self) -> usize {
        crate::lock(&self.pages)
            .own
            .values()
            .filter(|p| matches!(p.bound, Bound::App { .. }))
            .count()
    }

    pub(crate) fn shared(&self, tab: &str) -> SharedPane {
        crate::lock(&self.pages)
            .shared
            .get(tab)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn update_shared(&self, tab: &str, f: impl FnOnce(&mut SharedPane)) {
        let mut pages = crate::lock(&self.pages);
        f(pages.shared.entry(tab.to_string()).or_default());
    }

    /// `agent`'s ids for the visible panes `tabs`: kept for tabs still
    /// listed, the smallest free `vN` for new ones. Returns (tab, id) in id
    /// order.
    pub(crate) fn visible_ids(&self, agent: &str, tabs: &[String]) -> Vec<(String, String)> {
        let mut pages = crate::lock(&self.pages);
        let ids = pages.visible_ids.entry(agent.to_string()).or_default();
        ids.retain(|tab, _| tabs.contains(tab));
        for tab in tabs {
            if !ids.contains_key(tab) {
                let used: std::collections::HashSet<u32> = ids.values().copied().collect();
                let n = (1..).find(|n| !used.contains(n)).unwrap_or(1);
                ids.insert(tab.clone(), n);
            }
        }
        let mut out: Vec<(u32, String)> = ids.iter().map(|(t, n)| (*n, t.clone())).collect();
        out.sort();
        out.into_iter().map(|(n, t)| (t, format!("v{n}"))).collect()
    }

    /// Drop what was kept about a tab (its link moved or ended).
    pub(crate) fn forget_shared(&self, tab: &str) {
        crate::lock(&self.pages).shared.remove(tab);
    }

    /// The agent whose own view `view` in `socket`'s window is.
    fn own_by_view(&self, socket: channel::SocketId, view: &str) -> Option<String> {
        crate::lock(&self.pages)
            .own
            .iter()
            .find(|(_, p)| {
                matches!(&p.bound, Bound::App { socket: s, view: v } if *s == socket && v == view)
            })
            .map(|(agent, _)| agent.clone())
    }
}

/// A window's `browser_event` (CONTRACTS §2, §6). Handled off the socket's
/// loop: answering it sends a request on that same socket.
pub(crate) fn on_event(
    state: &Arc<crate::AppState>,
    socket: channel::SocketId,
    kind: &str,
    view: Option<&str>,
    tab: Option<&str>,
) {
    match (kind, tab) {
        ("takeover", Some(tab)) => {
            if crate::links::browser_link_of(state, tab).is_none() {
                return;
            }
            state.browsers.update_shared(tab, |p| {
                p.paused = Some(SystemTime::now());
                p.driving_shown = None;
            });
            spawn_state(state, socket, tab, false, true);
        }
        ("resume", Some(tab)) => {
            if crate::links::browser_link_of(state, tab).is_none() {
                return;
            }
            state.browsers.update_shared(tab, |p| p.paused = None);
            spawn_state(state, socket, tab, false, false);
        }
        ("view_lost", _) => {
            if let Some(agent) = view.and_then(|v| state.browsers.own_by_view(socket, v)) {
                state.browsers.remove_own(&agent);
            }
        }
        _ => {}
    }
}

fn spawn_state(
    state: &Arc<crate::AppState>,
    socket: channel::SocketId,
    tab: &str,
    driving: bool,
    paused: bool,
) {
    let state = state.clone();
    let tab = tab.to_string();
    tokio::spawn(async move {
        if let Err(e) = app_engine::set_state(&state, socket, &tab, driving, paused).await {
            tracing::debug!(%tab, error = %e, "agent browser: pane state not delivered");
        }
    });
}

/// The agent browser's housekeeping: drops a shared pane's "driving" marker
/// [`DRIVING_LINGER`] after the agent's last act, forgets panes no longer
/// linked, and every minute closes own pages that are idle
/// ([`IDLE_REAP`]), whose agent ended, or whose window went away, then lets
/// the Chromium engine reap.
pub(crate) async fn reaper(state: Arc<crate::AppState>) {
    let mut tick = tokio::time::interval(REAP_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut n: u32 = 0;
    loop {
        tick.tick().await;
        n = n.wrapping_add(1);
        sweep_shared(&state);
        if n.is_multiple_of(REAP_EVERY) {
            reap_pages(&state).await;
            state.browsers.chromium.reap_idle().await;
        }
    }
}

fn sweep_shared(state: &Arc<crate::AppState>) {
    // Kept for shared panes and for panes agents can see read-only (their
    // console/network cursors).
    let mut linked: std::collections::HashSet<String> =
        crate::lock(&state.browser_links).keys().cloned().collect();
    linked.extend(state.browsers.channel.listed_tabs());
    let mut lower = Vec::new();
    {
        let mut pages = crate::lock(&state.browsers.pages);
        pages.shared.retain(|tab, _| linked.contains(tab));
        for (tab, pane) in pages.shared.iter_mut() {
            let idle = pane
                .last_act
                .is_none_or(|at| at.elapsed() >= DRIVING_LINGER);
            if idle {
                if let Some(socket) = pane.driving_shown.take() {
                    lower.push((socket, tab.clone(), pane.paused.is_some()));
                }
            }
        }
    }
    for (socket, tab, paused) in lower {
        spawn_state(state, socket, &tab, false, paused);
    }
}

async fn reap_pages(state: &Arc<crate::AppState>) {
    let doomed: Vec<(String, OwnPage)> = {
        let mut pages = crate::lock(&state.browsers.pages);
        let dead: Vec<String> = pages
            .own
            .iter()
            .filter(|(agent, page)| {
                page.last_used.elapsed() >= IDLE_REAP
                    || !crate::chat::session_alive(state, agent)
                    || matches!(&page.bound, Bound::App { socket, .. } if !state.browsers.channel.alive(*socket))
            })
            .map(|(agent, _)| agent.clone())
            .collect();
        let doomed = dead
            .into_iter()
            .filter_map(|agent| pages.own.remove(&agent).map(|p| (agent, p)))
            .collect();
        let Pages {
            own,
            locks,
            last_engine,
            visible_ids,
            ..
        } = &mut *pages;
        // A lock someone holds or waits on stays; the rest go with their page.
        locks.retain(|agent, lock| {
            own.contains_key(agent)
                || Arc::strong_count(lock) > 1
                || crate::chat::session_alive(state, agent)
        });
        last_engine.retain(|agent, _| crate::chat::session_alive(state, agent));
        visible_ids.retain(|agent, _| crate::chat::session_alive(state, agent));
        doomed
    };
    for (agent, page) in doomed {
        tracing::info!(%agent, engine = %page.bound.engine(), "agent browser: closing an idle page");
        close_page(state, &page.bound).await;
    }
}

/// Close an own page on its engine (best effort: it may be gone already).
pub(crate) async fn close_page(state: &Arc<crate::AppState>, bound: &Bound) {
    let result = match bound {
        Bound::App { socket, view } => {
            if !state.browsers.channel.alive(*socket) {
                return;
            }
            let addr = app_engine::Addr {
                socket: *socket,
                view: Some(view.clone()),
                tab: None,
            };
            app_engine::act(state, &addr, Action::Close).await
        }
        Bound::Chromium { key } => state.browsers.chromium.act(state, key, Action::Close).await,
    };
    if let Err(e) = result {
        tracing::debug!(error = %e, "agent browser: close failed");
    }
}

/// `s` cut to at most `max` bytes on a char boundary, with an ellipsis.
pub(crate) fn clip_chars(s: &str, max: usize) -> Cow<'_, str> {
    if s.len() <= max {
        return Cow::Borrowed(s);
    }
    let mut end = max.saturating_sub(3);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Owned(format!("{}…", &s[..end]))
}

/// `at` as the daemon host's local wall-clock time, `HH:MM:SS` (the UTC
/// offset asked of `date +%z` once; UTC, said so, if that fails).
pub(crate) async fn clock(at: SystemTime) -> String {
    static OFFSET: OnceLock<Option<i64>> = OnceLock::new();
    let offset = match OFFSET.get() {
        Some(o) => *o,
        None => {
            let o = local_offset().await;
            *OFFSET.get_or_init(|| o)
        }
    };
    let secs = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let day = (secs + offset.unwrap_or(0)).rem_euclid(86_400);
    let hms = format!("{:02}:{:02}:{:02}", day / 3600, day / 60 % 60, day % 60);
    match offset {
        Some(_) => hms,
        None => format!("{hms} UTC"),
    }
}

async fn local_offset() -> Option<i64> {
    let out = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("date")
            .arg("+%z")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    parse_offset(String::from_utf8_lossy(&out.stdout).trim())
}

/// `+0100` / `-0830` → seconds east of UTC.
fn parse_offset(s: &str) -> Option<i64> {
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_parse_and_clips_respect_char_boundaries() {
        assert_eq!(parse_offset("+0100"), Some(3600));
        assert_eq!(parse_offset("-0830"), Some(-(8 * 3600 + 30 * 60)));
        assert_eq!(parse_offset("+0000"), Some(0));
        assert_eq!(parse_offset("0100"), None);
        assert_eq!(parse_offset("+1"), None);
        assert_eq!(clip_chars("hello", 10), "hello");
        let clipped = clip_chars("ééééé", 6);
        assert!(clipped.len() <= 6 && clipped.ends_with('…'), "{clipped}");
    }

    #[test]
    fn target_urls_bracket_ipv6() {
        let t = |host: &str| TargetUrl {
            host: host.into(),
            port: 8888,
            path: "/lab".into(),
        };
        assert_eq!(t("::1").url(), "http://[::1]:8888/lab");
        assert_eq!(t("localhost").url(), "http://localhost:8888/lab");
    }
}
