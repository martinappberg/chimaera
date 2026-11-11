//! The window request channel (CONTRACTS §2): how the daemon asks a
//! connected app window to drive one of its pane views, and gets the answer.
//!
//! Every authenticated `/ws/events` socket registers here ([`Channel::register`])
//! and gets a bounded queue the socket handler writes out at once: a branch
//! of its main `select!` AND during its 250 ms throttle sleep, so a request
//! never waits for the next snapshot pass (the measured cost of the old push
//! path was ~1.24 s; this one is a few ms). The window answers on the same
//! socket with `browser_reply {rid, ok, result|error}`; [`Channel::reply`]
//! hands that to the waiting [`Channel::request`]. `browser_hello` tells us
//! what the window can do (the app engine), whether it is visible and
//! focused, and which browser tabs it holds.
//!
//! Bounded: a socket queue holds [`QUEUE`] frames, at most [`PENDING_CAP`]
//! requests wait at once, a hello lists at most [`HELLO_TABS_MAX`] tabs, and
//! every request has a deadline. A socket that closes fails its pending
//! requests at once with `Gone`, never by timeout.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::BrowserError;

/// One authenticated `/ws/events` socket.
pub(crate) type SocketId = u64;

/// Frames queued per socket before a send counts as "the window is not
/// keeping up" (requests are small; screenshots come back, never go out).
const QUEUE: usize = 64;
/// Requests in flight across every socket.
const PENDING_CAP: usize = 256;
/// Tabs a hello may list, and the longest tab id kept.
pub(crate) const HELLO_TABS_MAX: usize = 256;
pub(crate) const TAB_ID_MAX: usize = 128;
/// Largest reply accepted (a full-page screenshot is base64 PNG).
pub(crate) const REPLY_MAX: usize = 8 * 1024 * 1024;
/// A request's deadline unless the op says otherwise.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// What a window says about itself (`browser_hello`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Hello {
    /// The window can host agent views (the macOS app with pane views).
    pub(crate) app_engine: bool,
    pub(crate) visible: bool,
    pub(crate) focused: bool,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Browser tab ids this window holds with a live pane view.
    pub(crate) tabs: Vec<String>,
    /// What those tabs show (additive; older windows send none).
    pub(crate) pages: Vec<HelloPage>,
}

/// One tab's title and app-terms URL, as the window reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct HelloPage {
    pub(crate) tab: String,
    #[serde(default)]
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) url: String,
}

/// A reported page title, in chars, and URL, in bytes, at most.
pub(crate) const PAGE_TITLE_MAX: usize = 200;
pub(crate) const PAGE_URL_MAX: usize = 2048;

impl Hello {
    /// Clip what a client sent to the registry's bounds.
    pub(crate) fn bounded(mut self) -> Self {
        self.tabs.retain(|t| !t.is_empty() && t.len() <= TAB_ID_MAX);
        self.tabs.truncate(HELLO_TABS_MAX);
        self.pages
            .retain(|p| !p.tab.is_empty() && p.tab.len() <= TAB_ID_MAX);
        self.pages.truncate(HELLO_TABS_MAX);
        for page in &mut self.pages {
            if page.title.chars().count() > PAGE_TITLE_MAX {
                page.title = page.title.chars().take(PAGE_TITLE_MAX).collect();
            }
            if page.url.len() > PAGE_URL_MAX {
                page.url.clear();
            }
        }
        self
    }

    /// Preference for an agent's own page: visible and focused, then
    /// visible, then any.
    fn rank(&self) -> u8 {
        match (self.visible, self.focused) {
            (true, true) => 2,
            (true, false) => 1,
            _ => 0,
        }
    }
}

struct Socket {
    tx: mpsc::Sender<Arc<str>>,
    /// The workspace its `watch` names.
    workspace: Option<String>,
    hello: Option<Hello>,
}

struct Pending {
    socket: SocketId,
    tx: oneshot::Sender<Result<Value, BrowserError>>,
}

#[derive(Default)]
struct Inner {
    sockets: HashMap<SocketId, Socket>,
    pending: HashMap<u64, Pending>,
}

/// The daemon-wide registry (lives in `Browsers`).
#[derive(Default)]
pub(crate) struct Channel {
    inner: Mutex<Inner>,
    next_socket: AtomicU64,
    next_rid: AtomicU64,
}

/// Holds one socket's registration; dropping it (every exit path of the
/// socket handler) unregisters the socket and fails its pending requests.
pub(crate) struct Registration<'a> {
    channel: &'a Channel,
    pub(crate) id: SocketId,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.channel.unregister(self.id);
    }
}

/// Text for a request whose window went away.
pub(crate) const WINDOW_GONE: &str =
    "The Chimaera window holding this page closed or reconnected, so the page is gone.";

impl Channel {
    /// Register an authenticated socket: its id, the receiving end of its
    /// queue (the handler writes each frame out as it arrives), and the
    /// guard that unregisters it.
    pub(crate) fn register(&self) -> (Registration<'_>, mpsc::Receiver<Arc<str>>) {
        let id = self.next_socket.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = mpsc::channel(QUEUE);
        crate::lock(&self.inner).sockets.insert(
            id,
            Socket {
                tx,
                workspace: None,
                hello: None,
            },
        );
        (Registration { channel: self, id }, rx)
    }

    fn unregister(&self, id: SocketId) {
        let failed: Vec<Pending> = {
            let mut inner = crate::lock(&self.inner);
            inner.sockets.remove(&id);
            let rids: Vec<u64> = inner
                .pending
                .iter()
                .filter(|(_, p)| p.socket == id)
                .map(|(rid, _)| *rid)
                .collect();
            rids.iter()
                .filter_map(|rid| inner.pending.remove(rid))
                .collect()
        };
        for p in failed {
            let _ = p.tx.send(Err(BrowserError::Gone(WINDOW_GONE.to_string())));
        }
    }

    /// Authenticated sockets right now (`open_browser`'s "no window is
    /// connected" answer).
    pub(crate) fn socket_count(&self) -> usize {
        crate::lock(&self.inner).sockets.len()
    }

    pub(crate) fn alive(&self, id: SocketId) -> bool {
        crate::lock(&self.inner).sockets.contains_key(&id)
    }

    pub(crate) fn set_workspace(&self, id: SocketId, workspace: Option<String>) {
        if let Some(s) = crate::lock(&self.inner).sockets.get_mut(&id) {
            s.workspace = workspace;
        }
    }

    pub(crate) fn hello(&self, id: SocketId, hello: Hello) {
        if let Some(s) = crate::lock(&self.inner).sockets.get_mut(&id) {
            s.hello = Some(hello.bounded());
        }
    }

    /// Write a frame to every socket now (`browser_open`). A full queue
    /// drops it for that window: the frame is a "show this now", never owed.
    pub(crate) fn broadcast(&self, frame: Arc<str>) {
        let inner = crate::lock(&self.inner);
        for s in inner.sockets.values() {
            let _ = s.tx.try_send(frame.clone());
        }
    }

    /// The window an agent's own page goes to: an app-engine window on the
    /// session's workspace (any app-engine window when the session has
    /// none), visible and focused first, then visible, then any; the newest
    /// wins a tie.
    pub(crate) fn choose(&self, workspace: Option<&str>) -> Option<SocketId> {
        let inner = crate::lock(&self.inner);
        inner
            .sockets
            .iter()
            .filter_map(|(id, s)| {
                let hello = s.hello.as_ref().filter(|h| h.app_engine)?;
                if workspace.is_some() && s.workspace.as_deref() != workspace {
                    return None;
                }
                Some((hello.rank(), *id))
            })
            .max()
            .map(|(_, id)| id)
    }

    /// The tabs windows on `workspace` hold, each with what its window says
    /// it shows (title and URL empty when the window didn't say), deduped,
    /// in tab order.
    pub(crate) fn listed_pages(&self, workspace: &str) -> Vec<HelloPage> {
        let inner = crate::lock(&self.inner);
        let mut out: std::collections::BTreeMap<String, HelloPage> = Default::default();
        for s in inner.sockets.values() {
            if s.workspace.as_deref() != Some(workspace) {
                continue;
            }
            let Some(hello) = &s.hello else { continue };
            for tab in &hello.tabs {
                let info = hello.pages.iter().find(|p| &p.tab == tab);
                let entry = out.entry(tab.clone()).or_insert_with(|| HelloPage {
                    tab: tab.clone(),
                    ..HelloPage::default()
                });
                if let Some(info) = info {
                    if entry.title.is_empty() && entry.url.is_empty() {
                        entry.title = info.title.clone();
                        entry.url = info.url.clone();
                    }
                }
            }
        }
        out.into_values().collect()
    }

    /// Every tab some window lists (any workspace).
    pub(crate) fn listed_tabs(&self) -> std::collections::HashSet<String> {
        let inner = crate::lock(&self.inner);
        inner
            .sockets
            .values()
            .filter_map(|s| s.hello.as_ref())
            .flat_map(|h| h.tabs.iter().cloned())
            .collect()
    }

    /// The window holding browser tab `tab` with a live pane view, if any.
    pub(crate) fn socket_with_tab(&self, tab: &str) -> Option<SocketId> {
        let inner = crate::lock(&self.inner);
        inner
            .sockets
            .iter()
            .filter_map(|(id, s)| {
                let hello = s.hello.as_ref()?;
                hello
                    .tabs
                    .iter()
                    .any(|t| t == tab)
                    .then(|| (hello.rank(), *id))
            })
            .max()
            .map(|(_, id)| id)
    }

    /// Connected app-engine windows, and how many of them are on `workspace`
    /// (for the refusal that names the fix).
    pub(crate) fn app_windows(&self, workspace: Option<&str>) -> (usize, usize) {
        let inner = crate::lock(&self.inner);
        let app: Vec<&Socket> = inner
            .sockets
            .values()
            .filter(|s| s.hello.as_ref().is_some_and(|h| h.app_engine))
            .collect();
        let here = app
            .iter()
            .filter(|s| workspace.is_none() || s.workspace.as_deref() == workspace)
            .count();
        (app.len(), here)
    }

    /// Send one request to `socket` and wait (bounded by `timeout`) for its
    /// reply. `frame` builds the frame for the request id it is given.
    /// A window error (`ok: false`) comes back as `BrowserError::Engine`
    /// carrying the window's text.
    pub(crate) async fn request(
        &self,
        socket: SocketId,
        frame: impl FnOnce(u64) -> Value,
        timeout: Duration,
    ) -> Result<Value, BrowserError> {
        let rid = self.next_rid.fetch_add(1, Ordering::Relaxed) + 1;
        let text: Arc<str> = Arc::from(frame(rid).to_string());
        let (tx, rx) = oneshot::channel();
        {
            let mut inner = crate::lock(&self.inner);
            if inner.pending.len() >= PENDING_CAP {
                return Err(BrowserError::Engine(
                    "Too many browser requests are in flight; try again in a moment.".to_string(),
                ));
            }
            let Some(s) = inner.sockets.get(&socket) else {
                return Err(BrowserError::Gone(WINDOW_GONE.to_string()));
            };
            match s.tx.try_send(text) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => return Err(BrowserError::Engine(
                    "The Chimaera window is not keeping up with requests; try again in a moment."
                        .to_string(),
                )),
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Err(BrowserError::Gone(WINDOW_GONE.to_string()))
                }
            }
            inner.pending.insert(rid, Pending { socket, tx });
        }
        // Removes the entry on timeout and when the caller is cancelled.
        let _guard = PendingGuard { channel: self, rid };
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(BrowserError::Gone(WINDOW_GONE.to_string())),
            Err(_) => Err(BrowserError::Timeout(format!(
                "The Chimaera window did not answer within {} s.",
                timeout.as_secs()
            ))),
        }
    }

    /// A `browser_reply` from `socket`. Only the socket a request went to
    /// may answer it; anything else is ignored.
    pub(crate) fn reply(&self, socket: SocketId, rid: u64, outcome: Result<Value, String>) {
        let pending = {
            let mut inner = crate::lock(&self.inner);
            match inner.pending.get(&rid) {
                Some(p) if p.socket == socket => inner.pending.remove(&rid),
                _ => None,
            }
        };
        if let Some(p) = pending {
            let _ = p.tx.send(
                outcome.map_err(|e| BrowserError::Engine(super::clip_chars(&e, 2048).into_owned())),
            );
        }
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        crate::lock(&self.inner).pending.len()
    }
}

struct PendingGuard<'a> {
    channel: &'a Channel,
    rid: u64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        crate::lock(&self.channel.inner).pending.remove(&self.rid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hello(app: bool, visible: bool, focused: bool, tabs: &[&str]) -> Hello {
        Hello {
            app_engine: app,
            visible,
            focused,
            width: 1440,
            height: 900,
            tabs: tabs.iter().map(|t| t.to_string()).collect(),
            pages: Vec::new(),
        }
    }

    /// Answer the next frame on `rx` with `answer(frame)`.
    async fn answer_next(
        channel: &Channel,
        socket: SocketId,
        rx: &mut mpsc::Receiver<Arc<str>>,
        answer: impl FnOnce(&Value) -> Result<Value, String>,
    ) -> Value {
        let frame: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        let rid = frame["rid"].as_u64().unwrap();
        channel.reply(socket, rid, answer(&frame));
        frame
    }

    #[tokio::test]
    async fn replies_reach_their_request_by_rid() {
        let channel = Channel::default();
        let (reg, mut rx) = channel.register();
        let id = reg.id;
        let a = channel.request(id, |rid| json!({"rid": rid, "op": "a"}), DEFAULT_TIMEOUT);
        let b = channel.request(id, |rid| json!({"rid": rid, "op": "b"}), DEFAULT_TIMEOUT);
        let window = async {
            let fa: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
            let fb: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
            // Out of order, and a stray rid nobody asked for is ignored.
            channel.reply(id, 9999, Ok(json!("stray")));
            channel.reply(id, fb["rid"].as_u64().unwrap(), Ok(json!({"op": fb["op"]})));
            channel.reply(id, fa["rid"].as_u64().unwrap(), Err("no such view".into()));
        };
        let (ra, rb, ()) = tokio::join!(a, b, window);
        assert_eq!(rb.unwrap(), json!({"op": "b"}));
        assert_eq!(ra.unwrap_err(), BrowserError::Engine("no such view".into()));
        assert_eq!(channel.pending_len(), 0);
    }

    #[tokio::test]
    async fn only_the_asked_socket_may_answer() {
        let channel = Channel::default();
        let (a, mut rx_a) = channel.register();
        let (b, _rx_b) = channel.register();
        let (a_id, b_id) = (a.id, b.id);
        let req = channel.request(a_id, |rid| json!({"rid": rid}), Duration::from_millis(300));
        let window = async {
            let frame: Value = serde_json::from_str(&rx_a.recv().await.unwrap()).unwrap();
            channel.reply(b_id, frame["rid"].as_u64().unwrap(), Ok(json!("spoof")));
        };
        let (result, ()) = tokio::join!(req, window);
        assert!(
            matches!(result, Err(BrowserError::Timeout(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_request_times_out_and_leaves_nothing_behind() {
        let channel = Channel::default();
        let (reg, _rx) = channel.register();
        let result = channel
            .request(reg.id, |rid| json!({"rid": rid}), Duration::from_millis(50))
            .await;
        assert!(
            matches!(result, Err(BrowserError::Timeout(_))),
            "{result:?}"
        );
        assert_eq!(channel.pending_len(), 0);
    }

    #[tokio::test]
    async fn a_closed_socket_fails_its_requests_at_once() {
        let channel = Channel::default();
        let (reg, mut rx) = channel.register();
        let id = reg.id;
        let req = channel.request(id, |rid| json!({"rid": rid}), Duration::from_secs(30));
        let close = async {
            rx.recv().await.unwrap();
            drop(reg);
        };
        let started = std::time::Instant::now();
        let (result, ()) = tokio::join!(req, close);
        assert!(matches!(result, Err(BrowserError::Gone(_))), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        // And a request to it now is Gone without waiting.
        let again = channel
            .request(id, |rid| json!({"rid": rid}), Duration::from_secs(30))
            .await;
        assert!(matches!(again, Err(BrowserError::Gone(_))));
        assert_eq!(channel.socket_count(), 0);
    }

    #[tokio::test]
    async fn a_cancelled_request_frees_its_slot() {
        let channel = Channel::default();
        let (reg, _rx) = channel.register();
        let req = channel.request(reg.id, |rid| json!({"rid": rid}), Duration::from_secs(30));
        let _ = tokio::time::timeout(Duration::from_millis(20), req).await;
        assert_eq!(channel.pending_len(), 0);
    }

    #[test]
    fn window_choice_prefers_focused_then_visible_on_the_workspace() {
        let channel = Channel::default();
        let (hidden, _r1) = channel.register();
        let (visible, _r2) = channel.register();
        let (focused_elsewhere, _r3) = channel.register();
        let (web, _r4) = channel.register();
        for (reg, ws) in [
            (&hidden, "w1"),
            (&visible, "w1"),
            (&focused_elsewhere, "w2"),
            (&web, "w1"),
        ] {
            channel.set_workspace(reg.id, Some(ws.into()));
        }
        assert_eq!(channel.choose(Some("w1")), None, "no hello yet");
        channel.hello(hidden.id, hello(true, false, false, &[]));
        assert_eq!(channel.choose(Some("w1")), Some(hidden.id));
        channel.hello(visible.id, hello(true, true, false, &[]));
        channel.hello(focused_elsewhere.id, hello(true, true, true, &[]));
        // A browser tab (no app engine) never hosts an agent view, focused or not.
        channel.hello(web.id, hello(false, true, true, &[]));
        assert_eq!(channel.choose(Some("w1")), Some(visible.id));
        channel.hello(hidden.id, hello(true, true, true, &[]));
        assert_eq!(channel.choose(Some("w1")), Some(hidden.id));
        assert_eq!(channel.choose(Some("w2")), Some(focused_elsewhere.id));
        assert_eq!(channel.choose(Some("w9")), None);
        assert_eq!(channel.app_windows(Some("w9")), (3, 0));
        assert_eq!(channel.app_windows(Some("w1")), (3, 2));
    }

    #[test]
    fn tabs_resolve_to_the_window_holding_them_and_hellos_are_bounded() {
        let channel = Channel::default();
        let (a, _ra) = channel.register();
        let (b, _rb) = channel.register();
        channel.hello(a.id, hello(true, false, false, &["t-1", "t-2"]));
        channel.hello(b.id, hello(true, true, false, &["t-2"]));
        assert_eq!(channel.socket_with_tab("t-1"), Some(a.id));
        assert_eq!(channel.socket_with_tab("t-2"), Some(b.id), "visible wins");
        assert_eq!(channel.socket_with_tab("t-3"), None);

        let long = "x".repeat(TAB_ID_MAX + 1);
        let many: Vec<String> = (0..HELLO_TABS_MAX + 10)
            .map(|i| format!("t{i}"))
            .chain([long.clone(), String::new()])
            .collect();
        let bounded = Hello {
            tabs: many,
            ..Hello::default()
        }
        .bounded();
        assert_eq!(bounded.tabs.len(), HELLO_TABS_MAX);
        assert!(!bounded.tabs.contains(&long));

        let page = |tab: &str, title: String, url: String| HelloPage {
            tab: tab.into(),
            title,
            url,
        };
        let bounded = Hello {
            pages: (0..HELLO_TABS_MAX + 3)
                .map(|i| {
                    page(
                        &format!("t{i}"),
                        "x".repeat(PAGE_TITLE_MAX + 5),
                        "http://a/".into(),
                    )
                })
                .chain([
                    page("", "t".into(), "u".into()),
                    page(&long, "t".into(), "u".into()),
                ])
                .collect(),
            ..Hello::default()
        }
        .bounded();
        assert_eq!(bounded.pages.len(), HELLO_TABS_MAX);
        assert!(bounded
            .pages
            .iter()
            .all(|p| p.title.chars().count() == PAGE_TITLE_MAX));
        let long_url = Hello {
            pages: vec![page(
                "t1",
                "T".into(),
                format!("http://a/{}", "p".repeat(PAGE_URL_MAX)),
            )],
            ..Hello::default()
        }
        .bounded();
        assert_eq!(
            long_url.pages[0].url, "",
            "an over-long URL is dropped, not cut"
        );
        assert_eq!(long_url.pages[0].title, "T");
    }

    #[tokio::test]
    async fn broadcast_reaches_every_socket_and_a_full_queue_refuses() {
        let channel = Channel::default();
        let (a, mut ra) = channel.register();
        let (_b, mut rb) = channel.register();
        channel.broadcast(Arc::from("{\"type\":\"browser_open\"}"));
        assert_eq!(&*ra.recv().await.unwrap(), "{\"type\":\"browser_open\"}");
        assert_eq!(&*rb.recv().await.unwrap(), "{\"type\":\"browser_open\"}");

        for _ in 0..QUEUE {
            channel.broadcast(Arc::from("x"));
        }
        let result = channel
            .request(a.id, |rid| json!({"rid": rid}), DEFAULT_TIMEOUT)
            .await;
        assert!(
            matches!(&result, Err(BrowserError::Engine(e)) if e.contains("not keeping up")),
            "{result:?}"
        );
        // Draining frees it again.
        while ra.try_recv().is_ok() {}
        let id = a.id;
        let req = channel.request(
            id,
            |rid| json!({"rid": rid, "op": "state"}),
            DEFAULT_TIMEOUT,
        );
        let window = answer_next(&channel, id, &mut ra, |_| Ok(json!({})));
        let (result, frame) = tokio::join!(req, window);
        assert_eq!(result.unwrap(), json!({}));
        assert_eq!(frame["op"], "state");
    }
}
