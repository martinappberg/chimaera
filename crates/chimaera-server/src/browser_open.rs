//! Agent-opened browser panes: the MCP `open_browser` tool's daemon half.
//!
//! An agent that has just started a web app asks for it to be shown; the
//! daemon validates the address, applies the proxy's own mint allowlist
//! ([`crate::proxy::check_target`] — the same function, never a copy), and
//! pushes a `browser_open` frame on `/ws/events`. The windows decide whether
//! to act (the one holding the session's tab, else a visible window on the
//! same workspace) and open the pane through the ordinary browser-pane path,
//! which mints its proxy ticket on mount as it always has. The tool mints
//! nothing.
//!
//! Deliberately one-way: nothing about the page ever flows back to the
//! agent. And deliberately unqueued: a frame reaches only the windows
//! connected when it is pushed (a reconnecting window starts at the head,
//! like notices), so a pane never pops open minutes after the call. The tool
//! says so honestly when no window is connected at all.
//!
//! Bounded: a small frame ring, per-session rate limits over a capped map,
//! and a consumer count maintained by the `/ws/events` handler.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::proxy::{self, Refusal};
use crate::AppState;

/// Frames kept for windows mid-pass (each window drains them within one
/// events-loop pass; the ring only bridges that gap).
const RING_CAP: usize = 16;
/// A frame older than this is dropped instead of delivered: a window whose
/// socket stalled must not open a pane long after the agent moved on.
const FRAME_MAX_AGE: Duration = Duration::from_secs(10);
/// Per-session limits: [`BURST_CAP`] opens in any [`BURST_WINDOW`], and
/// [`HOURLY_CAP`] per hour. A burst, not a minimum gap: an agent presenting a
/// frontend and its dashboard in one step makes two calls back to back, and
/// agents tend to treat a tool error as final. An app is still opened once,
/// not on every reload.
const BURST_CAP: usize = 3;
const BURST_WINDOW: Duration = Duration::from_secs(10);
const HOURLY_CAP: usize = 12;
const HOUR: Duration = Duration::from_secs(60 * 60);
/// Sessions whose send history is kept; the least recent is forgotten first.
const SESSIONS_CAP: usize = 64;
/// Longest URL the tool accepts (Jupyter's `?token=` URLs are ~100 chars).
const URL_MAX: usize = 2048;

/// Where a pane should point: what a `BrowserTab` stores.
#[derive(Debug, PartialEq)]
pub(crate) struct Target {
    pub(crate) host: String,
    pub(crate) port: u16,
    /// Path + query + fragment, always starting with `/`.
    pub(crate) path: String,
}

impl Target {
    /// The address as a person would type it (for tool results).
    pub(crate) fn url(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("http://{host}:{}{}", self.port, self.path)
    }
}

/// Parse an agent-supplied URL with the terminal link rules
/// (`web-ui/src/lib/shared/urlOpen.ts::proxyableUrl`): `http` only, no
/// userinfo, an explicit port unless the host is loopback (then 80), and
/// path + query + fragment kept. The UI normalizes the path the way the
/// browser's URL parser does before it is used, exactly as for a click.
pub(crate) fn parse_url(raw: &str) -> Result<Target, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("`url` must be a non-empty http:// address.".to_string());
    }
    if raw.len() > URL_MAX {
        return Err(format!("`url` is longer than {URL_MAX} characters."));
    }
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("`url` must not contain spaces or control characters.".to_string());
    }
    let scheme_end = raw.find("://").unwrap_or(0);
    let scheme = raw[..scheme_end].to_ascii_lowercase();
    if scheme == "https" {
        return Err(
            "Only plain http:// apps open in a browser pane (the pane speaks \
                    clear-text HTTP to the app). Give the user the URL to open themselves."
                .to_string(),
        );
    }
    if scheme != "http" {
        return Err(format!(
            "`url` must be an http:// address like http://localhost:5173/, got {raw:?}."
        ));
    }
    let rest = &raw[scheme_end + 3..];
    let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(split);
    if authority.contains('@') {
        return Err("`url` must not carry a user name or password.".to_string());
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(inner) => {
            let (host, after) = inner
                .split_once(']')
                .ok_or_else(|| format!("{raw:?} has an unclosed IPv6 bracket."))?;
            let port = match after {
                "" => None,
                p => Some(
                    p.strip_prefix(':')
                        .ok_or_else(|| format!("{raw:?} is not a valid address."))?,
                ),
            };
            (host, port)
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    // An IP literal in its canonical spelling, so the UI's same-target
    // dedupe sees one address, not two spellings of it.
    let host = match host.parse::<IpAddr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => host.to_ascii_lowercase(),
    };
    if host.is_empty() {
        return Err(format!("{raw:?} has no host."));
    }
    let port = match port.filter(|p| !p.is_empty()) {
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 && p.bytes().all(|b| b.is_ascii_digit()) => n,
            _ => return Err(format!("{p:?} is not a valid port.")),
        },
        None if proxy::is_loopback_host(&host) => 80,
        None => {
            return Err(format!(
                "Give the port explicitly (http://{host}:PORT/…): only loopback \
                 addresses default to port 80."
            ))
        }
    };
    let path = match tail.chars().next() {
        None => "/".to_string(),
        Some('/') => tail.to_string(),
        Some(_) => format!("/{tail}"),
    };
    Ok(Target { host, port, path })
}

/// The agent-open feed: the frame ring, the rate-limit history, and how many
/// windows are listening.
#[derive(Default)]
pub struct BrowserOpens {
    inner: Mutex<Inner>,
    /// Authenticated `/ws/events` sockets right now (see [`ConsumerGuard`]).
    consumers: AtomicUsize,
}

#[derive(Default)]
struct Inner {
    next: u64,
    ring: VecDeque<(u64, Instant, Arc<str>)>,
    /// Per-session open times within the last hour.
    sends: HashMap<String, VecDeque<Instant>>,
}

/// Counts one `/ws/events` consumer for as long as it lives — dropped on
/// every exit path of the socket handler.
pub(crate) struct ConsumerGuard<'a>(&'a BrowserOpens);

impl Drop for ConsumerGuard<'_> {
    fn drop(&mut self) {
        self.0.consumers.fetch_sub(1, Ordering::Relaxed);
    }
}

impl BrowserOpens {
    pub(crate) fn consumer(&self) -> ConsumerGuard<'_> {
        self.consumers.fetch_add(1, Ordering::Relaxed);
        ConsumerGuard(self)
    }

    pub(crate) fn consumers(&self) -> usize {
        self.consumers.load(Ordering::Relaxed)
    }

    /// The newest frame id: a (re)connecting window starts here.
    pub(crate) fn head(&self) -> u64 {
        crate::lock(&self.inner).next
    }

    /// Fresh frames newer than `last`, advancing the window's mark.
    pub(crate) fn since(&self, last: &mut u64) -> Vec<Arc<str>> {
        let inner = crate::lock(&self.inner);
        let frames = inner
            .ring
            .iter()
            .filter(|(id, at, _)| *id > *last && at.elapsed() <= FRAME_MAX_AGE)
            .map(|(_, _, frame)| frame.clone())
            .collect();
        *last = inner.next;
        frames
    }

    fn push(&self, frame: String) {
        let mut inner = crate::lock(&self.inner);
        inner.next += 1;
        let id = inner.next;
        inner.ring.push_back((id, Instant::now(), Arc::from(frame)));
        while inner.ring.len() > RING_CAP {
            inner.ring.pop_front();
        }
    }

    /// Record an open if the session is within its limits.
    fn admit(&self, session_id: &str) -> Result<(), String> {
        let mut inner = crate::lock(&self.inner);
        inner
            .sends
            .retain(|_, sends| sends.back().is_some_and(|at| at.elapsed() <= HOUR));
        if !inner.sends.contains_key(session_id) && inner.sends.len() >= SESSIONS_CAP {
            let stalest = inner
                .sends
                .iter()
                .min_by_key(|(_, sends)| sends.back().copied())
                .map(|(id, _)| id.clone());
            if let Some(id) = stalest {
                inner.sends.remove(&id);
            }
        }
        let sends = inner.sends.entry(session_id.to_string()).or_default();
        while sends.front().is_some_and(|at| at.elapsed() > HOUR) {
            sends.pop_front();
        }
        // The oldest open inside the window decides when a slot frees up.
        let recent = sends
            .iter()
            .rev()
            .take_while(|at| at.elapsed() < BURST_WINDOW)
            .count();
        if recent >= BURST_CAP {
            let oldest = sends[sends.len() - recent].elapsed();
            // Saturating: `oldest` is read again after the count above, and
            // may have crossed the window since (a plain `-` would panic).
            let wait = BURST_WINDOW
                .saturating_sub(oldest)
                .as_secs_f64()
                .ceil()
                .max(1.0) as u64;
            return Err(format!(
                "Not opened: this session already opened {BURST_CAP} browser panes in \
                 the last {}s. Wait {wait}s before opening another; one call per app \
                 is enough.",
                BURST_WINDOW.as_secs()
            ));
        }
        if sends.len() >= HOURLY_CAP {
            return Err(format!(
                "Not opened: this session already opened {HOURLY_CAP} browser panes in \
                 the last hour. Give the user the URL instead."
            ));
        }
        sends.push_back(Instant::now());
        Ok(())
    }
}

/// The `open_browser` tool: validate, apply the mint allowlist, rate-limit,
/// push. `Ok` and `Err` are both text the agent reads (`Err` as a tool
/// error).
pub(crate) async fn open(state: &AppState, session_id: &str, url: &str) -> Result<String, String> {
    let target = parse_url(url)?;
    let shown = target.url();
    let host = proxy::normalize_host(&target.host);
    match proxy::check_target(state, &host, target.port).await {
        Ok(_) => {}
        Err(Refusal::Invalid) => {
            return Err(format!(
                "Not opened: {:?} is not a valid host name.",
                target.host
            ));
        }
        Err(Refusal::ConfirmRequired) => {
            return Err(format!(
                "Not opened: {host} is not this host, loopback, or a node of one of the \
                 user's running Slurm jobs, so Chimaera opens it only after the user \
                 confirms it themselves. Give the user the URL ({shown}) to open."
            ));
        }
        Err(Refusal::Daemon) => {
            return Err(format!(
                "Not opened: port {} on this host is Chimaera itself.",
                target.port
            ));
        }
    }
    // Checked before the budget is spent: a call nobody could see costs
    // nothing, and the answer tells the agent to hand the URL over instead.
    if state.browser_opens.consumers() == 0 {
        return Ok(format!(
            "Not opened: no Chimaera window is connected right now. Tell the user the \
             URL ({shown}) so they can open it themselves."
        ));
    }
    state.browser_opens.admit(session_id)?;
    let workspace_id = crate::lock(&state.session_workspaces)
        .get(session_id)
        .cloned();
    state.browser_opens.push(
        json!({
            "type": "browser_open",
            "session_id": session_id,
            "workspace_id": workspace_id,
            "host": host,
            "port": target.port,
            "path": target.path,
        })
        .to_string(),
    );
    state.changes.notify_waiters();
    Ok(format!(
        "Requested a browser pane for {shown}: a window showing this session, and any \
         other visible window on this workspace, opens it beside your session. This says nothing about whether the page loads or works, nor that \
         the user is looking; nothing from the page comes back to you. Call again only \
         for a different address, not after the app reloads."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(host: &str, port: u16, path: &str) -> Target {
        Target {
            host: host.into(),
            port,
            path: path.into(),
        }
    }

    #[test]
    fn urls_parse_like_terminal_links() {
        assert_eq!(
            parse_url("http://localhost:5173/app?x=1"),
            Ok(t("localhost", 5173, "/app?x=1"))
        );
        assert_eq!(
            parse_url("  HTTP://LocalHost:8888/lab?token=abc#cell "),
            Ok(t("localhost", 8888, "/lab?token=abc#cell"))
        );
        // Loopback alone defaults to port 80; an empty path is the root.
        assert_eq!(parse_url("http://127.0.0.1"), Ok(t("127.0.0.1", 80, "/")));
        assert_eq!(
            parse_url("http://localhost:/x"),
            Ok(t("localhost", 80, "/x"))
        );
        assert_eq!(parse_url("http://[::1]:8501"), Ok(t("::1", 8501, "/")));
        assert_eq!(
            parse_url("http://[0:0:0:0:0:0:0:1]:8501?q"),
            Ok(t("::1", 8501, "/?q"))
        );
        assert_eq!(
            parse_url("http://node-014:8888/tree"),
            Ok(t("node-014", 8888, "/tree"))
        );
        assert_eq!(
            parse_url("http://localhost:4173#/runs"),
            Ok(t("localhost", 4173, "/#/runs"))
        );
    }

    #[test]
    fn urls_that_terminal_links_refuse_are_refused() {
        for bad in [
            "",
            "localhost:5173",
            "ftp://localhost:21/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://user:pw@localhost:1/",
            "http://localhost:0/",
            "http://localhost:70000/",
            "http://localhost:12ab/",
            "http://localhost:+80/",
            "http://example.org/",
            "http://[::1/",
            "http://:8080/",
            "http://local host:1/",
            "http://localhost:1/a\nb",
        ] {
            assert!(parse_url(bad).is_err(), "{bad:?} should be refused");
        }
        let err = parse_url("https://localhost:8443/").unwrap_err();
        assert!(err.contains("plain http"), "{err}");
        let err = parse_url("http://node-014/").unwrap_err();
        assert!(err.contains("port explicitly"), "{err}");
        assert!(parse_url(&format!("http://localhost:1/{}", "a".repeat(URL_MAX))).is_err());
    }

    #[test]
    fn opens_are_rate_limited_per_session() {
        let opens = BrowserOpens::default();
        // A small burst passes (two apps shown in one step), the next waits.
        for _ in 0..BURST_CAP {
            assert!(opens.admit("s-1").is_ok());
        }
        let err = opens.admit("s-1").unwrap_err();
        assert!(err.contains("in the last 10s"), "{err}");
        assert!(err.contains("Wait 10s"), "{err}");
        assert!(
            opens.admit("s-2").is_ok(),
            "another session has its own budget"
        );

        // Once the burst's oldest open ages out of the window, a slot frees.
        {
            let mut inner = crate::lock(&opens.inner);
            let sends = inner.sends.get_mut("s-1").unwrap();
            sends[0] = Instant::now() - BURST_WINDOW - Duration::from_millis(1);
        }
        assert!(opens.admit("s-1").is_ok());
        assert!(opens.admit("s-1").is_err());

        let old = Instant::now() - BURST_WINDOW * 2;
        crate::lock(&opens.inner)
            .sends
            .insert("s-3".into(), std::iter::repeat_n(old, HOURLY_CAP).collect());
        let err = opens.admit("s-3").unwrap_err();
        assert!(err.contains("last hour"), "{err}");
    }

    #[test]
    fn rate_limit_history_is_bounded() {
        let opens = BrowserOpens::default();
        for i in 0..(SESSIONS_CAP + 20) {
            assert!(opens.admit(&format!("s-{i}")).is_ok());
        }
        assert_eq!(crate::lock(&opens.inner).sends.len(), SESSIONS_CAP);
    }

    #[test]
    fn frames_reach_only_windows_connected_at_push() {
        let opens = BrowserOpens::default();
        for i in 0..(RING_CAP + 5) {
            opens.push(format!("f{i}"));
        }
        assert_eq!(crate::lock(&opens.inner).ring.len(), RING_CAP);
        // A window connecting now starts at the head: nothing replays.
        let mut late = opens.head();
        assert!(opens.since(&mut late).is_empty());
        opens.push("next".into());
        assert_eq!(opens.since(&mut late), vec![Arc::<str>::from("next")]);
        assert!(opens.since(&mut late).is_empty());

        // A stale frame is never delivered.
        let mut mark = opens.head();
        crate::lock(&opens.inner).ring.push_back((
            mark + 1,
            Instant::now() - FRAME_MAX_AGE * 2,
            Arc::from("old"),
        ));
        crate::lock(&opens.inner).next = mark + 1;
        assert!(opens.since(&mut mark).is_empty());
    }

    #[test]
    fn consumers_count_live_guards() {
        let opens = BrowserOpens::default();
        assert_eq!(opens.consumers(), 0);
        let a = opens.consumer();
        let b = opens.consumer();
        assert_eq!(opens.consumers(), 2);
        drop(a);
        drop(b);
        assert_eq!(opens.consumers(), 0);
    }
}
