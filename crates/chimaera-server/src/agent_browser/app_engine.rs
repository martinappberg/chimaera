//! The app engine: pages that are pane views in a connected macOS app window
//! (CONTRACTS §2, §3). Each [`Action`] becomes one `browser_request` on that
//! window's `/ws/events` socket (`channel`); the window forwards it to the
//! shell (`native_view_show` / `_drive` / `_nav` / `_close`) and replies.
//!
//! Pages load through the daemon's own proxy: an allowlisted target is
//! minted (`proxy::mint`) and the view loads `/proxy/{id}{path}` on the
//! window's daemon origin, like a pane. Every URL reported back is mapped to
//! the agent's terms (`http://host:port/path`, [`map_href`]).
//!
//! The shell's "shown" reply says nothing about the load, so navigation waits
//! here: before it, the agent's isolated world gets a mark (`__chimaeraNav`); the load
//! is done when a document without that mark reports `complete` (polled by
//! eval). A same-document step (a fragment change, `pushState` history) keeps
//! the mark, so that case is judged by the URL instead.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::channel::{SocketId, DEFAULT_TIMEOUT};
use super::{Action, ActionOut, BrowserError, HistoryOp, RingsOut, TargetUrl, World};
use crate::AppState;

/// Where a request goes: an agent's own view (`view`), or a pane the user
/// shared (`tab`; the window resolves the tab's own pane view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Addr {
    pub(crate) socket: SocketId,
    pub(crate) view: Option<String>,
    pub(crate) tab: Option<String>,
}

/// How long a navigation may take to reach `complete`.
const LOAD_TIMEOUT: Duration = Duration::from_secs(15);
/// A history step that loads no new document settles by URL; past this it
/// reports whatever the page shows.
const HISTORY_TIMEOUT: Duration = Duration::from_secs(8);
/// Between load polls (each is one isolated-world eval, a few ms).
const LOAD_POLL: Duration = Duration::from_millis(150);
/// One load poll's own deadline: a page mid-commit may not answer.
const POLL_EVAL_TIMEOUT: Duration = Duration::from_secs(3);

static NAV_MARK: AtomicU64 = AtomicU64::new(1);

/// The shell's answer when the page's JavaScript didn't run in time.
const SHELL_BUSY: &str = "the page didn't answer in time";

/// Act on one page. `Open` creates the view (or re-points it) at an
/// allowlisted target; `Open`, `Navigate` and `History` resolve to
/// `Value({"url","title","status"})` once the load settles (≤ 15 s), the
/// same shape the Chromium engine returns.
pub(crate) async fn act(
    state: &AppState,
    addr: &Addr,
    action: Action,
) -> Result<ActionOut, BrowserError> {
    match action {
        Action::Open { url, width, height } => {
            let path = proxied_path(state, &url).await?;
            let before = mark(state, addr).await;
            let fields = obj(json!({"url": path, "width": width, "height": height}));
            request(state, addr, "open", fields, DEFAULT_TIMEOUT).await?;
            let facts = wait_load(state, addr, before, Some(&path), false).await?;
            Ok(ActionOut::Value(facts))
        }
        Action::Navigate { url } => {
            let path = proxied_path(state, &url).await?;
            let before = mark(state, addr).await;
            // In the page itself, so a shared pane navigates in place; the
            // assign runs after the reply (a navigation would cut it off).
            let body = "setTimeout(() => location.assign(args.url), 0); return true;";
            eval(
                state,
                addr,
                World::Isolated,
                body,
                json!({"url": path}),
                5_000,
            )
            .await?;
            let facts = wait_load(state, addr, before, Some(&path), false).await?;
            Ok(ActionOut::Value(facts))
        }
        Action::History { op } => {
            let before = mark(state, addr).await;
            let name = match op {
                HistoryOp::Back => "back",
                HistoryOp::Forward => "forward",
                HistoryOp::Reload => "reload",
                HistoryOp::Stop => "stop",
            };
            request(
                state,
                addr,
                "nav",
                obj(json!({"action": name})),
                DEFAULT_TIMEOUT,
            )
            .await?;
            if op == HistoryOp::Stop {
                return Ok(ActionOut::Value(facts_now(state, addr).await?));
            }
            let facts = wait_load(state, addr, before, None, op != HistoryOp::Reload).await?;
            Ok(ActionOut::Value(facts))
        }
        Action::Eval {
            world,
            body,
            args,
            timeout_ms,
        } => {
            let value = eval(state, addr, world, &body, args, timeout_ms).await?;
            Ok(ActionOut::Value(value))
        }
        Action::Click {
            x,
            y,
            clicks,
            modifiers,
        } => {
            let drive =
                json!({"type": "click", "x": x, "y": y, "clicks": clicks, "modifiers": modifiers});
            drive_action(state, addr, drive, DEFAULT_TIMEOUT).await?;
            Ok(ActionOut::Done)
        }
        Action::Move { x, y } => {
            drive_action(
                state,
                addr,
                json!({"type": "move", "x": x, "y": y}),
                DEFAULT_TIMEOUT,
            )
            .await?;
            Ok(ActionOut::Done)
        }
        Action::Drag { from, to, steps } => {
            let drive = json!({"type": "drag", "from": [from.0, from.1], "to": [to.0, to.1], "steps": steps});
            let budget = DEFAULT_TIMEOUT + Duration::from_millis(u64::from(steps) * 20);
            drive_action(state, addr, drive, budget).await?;
            Ok(ActionOut::Done)
        }
        Action::Key { key, modifiers } => {
            let drive = json!({"type": "key", "key": key, "modifiers": modifiers});
            drive_action(state, addr, drive, DEFAULT_TIMEOUT).await?;
            Ok(ActionOut::Done)
        }
        Action::Text { text, per_key } => {
            // Paced keys are ~12 ms apart in the shell.
            let budget = if per_key {
                DEFAULT_TIMEOUT + Duration::from_millis(text.chars().count() as u64 * 20)
            } else {
                DEFAULT_TIMEOUT
            };
            let drive = json!({"type": "text", "text": text, "per_key": per_key});
            drive_action(state, addr, drive, budget).await?;
            Ok(ActionOut::Done)
        }
        Action::Scroll { x, y, dx, dy } => {
            let drive = json!({"type": "scroll", "x": x, "y": y, "dx": dx, "dy": dy});
            drive_action(state, addr, drive, DEFAULT_TIMEOUT).await?;
            Ok(ActionOut::Done)
        }
        Action::Screenshot {
            clip,
            full_page,
            max_width,
            format,
            quality,
        } => {
            let drive = json!({
                "type": "screenshot",
                "clip": clip,
                "full_page": full_page,
                "max_width": max_width,
                "format": format,
                "quality": quality,
            });
            let out = drive_action(state, addr, drive, Duration::from_secs(20)).await?;
            // The image's base64 rides the key its `format` names.
            let key = out["format"].as_str().unwrap_or("png");
            let b64 = out[key].as_str().unwrap_or_default().to_string();
            if b64.is_empty() {
                return Err(BrowserError::Engine(
                    "The window answered the screenshot without an image.".to_string(),
                ));
            }
            Ok(ActionOut::Png {
                b64,
                width: out["width"].as_u64().unwrap_or(0) as u32,
                height: out["height"].as_u64().unwrap_or(0) as u32,
            })
        }
        Action::Rings {
            console_since,
            network_since,
        } => {
            let drive = json!({"type": "rings", "console_since": console_since, "network_since": network_since});
            let out = drive_action(state, addr, drive, DEFAULT_TIMEOUT).await?;
            let rings: RingsOut = serde_json::from_value(out).map_err(|e| {
                BrowserError::Engine(format!(
                    "The window's console/network record was unreadable: {e}"
                ))
            })?;
            Ok(ActionOut::Rings(rings))
        }
        Action::Close => {
            request(state, addr, "close", Map::new(), DEFAULT_TIMEOUT).await?;
            Ok(ActionOut::Done)
        }
    }
}

/// One `drive` action the [`Action`] enum has no variant for (`instrument`,
/// `cursor`): the shell's own result.
pub(crate) async fn drive_raw(
    state: &AppState,
    addr: &Addr,
    action: Value,
) -> Result<Value, BrowserError> {
    drive_action(state, addr, action, DEFAULT_TIMEOUT).await
}

/// Hand the agent's own view to the user as a pane beside the agent: the
/// window adopts the live view into a new browser tab (no reload). Returns
/// the new tab's id.
pub(crate) async fn handover(
    state: &AppState,
    addr: &Addr,
    target: &TargetUrl,
    agent: &str,
) -> Result<String, BrowserError> {
    let fields = obj(json!({
        "host": target.host,
        "port": target.port,
        "path": target.path,
        "agent": agent,
    }));
    let out = request(state, addr, "handover", fields, DEFAULT_TIMEOUT).await?;
    match out["tab"].as_str() {
        Some(tab) if !tab.is_empty() && tab.len() <= super::channel::TAB_ID_MAX => {
            Ok(tab.to_string())
        }
        _ => Err(BrowserError::Engine(
            "The window opened no pane for the page.".to_string(),
        )),
    }
}

/// The pane marker for a shared or handed-over pane (no native call).
pub(crate) async fn set_state(
    state: &AppState,
    socket: SocketId,
    tab: &str,
    driving: bool,
    paused: bool,
) -> Result<(), BrowserError> {
    let addr = Addr {
        socket,
        view: None,
        tab: Some(tab.to_string()),
    };
    let fields = obj(json!({"driving": driving, "paused": paused}));
    request(state, &addr, "state", fields, Duration::from_secs(5))
        .await
        .map(|_| ())
}

/// A page URL in the agent's terms: `/proxy/{id}/rest` on the window's
/// origin becomes `http://host:port/rest` for the id's pinned target; a path
/// an app reached outside the prefix (the proxy's rescue of absolute paths)
/// is mapped onto `fallback`, the page's last known target. Anything else
/// (`about:blank`, `data:`) is returned as is.
pub(crate) fn map_href(state: &AppState, href: &str, fallback: Option<&TargetUrl>) -> String {
    let Some((_, path)) = split_href(href) else {
        return href.to_string();
    };
    if let Some((id, rest)) = proxy_path(path) {
        if let Some((host, port)) = crate::proxy::target_of(state, id) {
            return TargetUrl {
                host,
                port,
                path: rest,
            }
            .url();
        }
    }
    match fallback {
        Some(t) => TargetUrl {
            host: t.host.clone(),
            port: t.port,
            path: path.to_string(),
        }
        .url(),
        None => href.to_string(),
    }
}

/// Free text (a console message, a stack trace) with every proxied URL in
/// it put in the agent's terms: `scheme://authority/proxy/{id}` becomes the
/// id's `http://host:port`. Unknown ids are left alone.
pub(crate) fn map_text(state: &AppState, text: &str) -> String {
    const MARK: &str = "/proxy/";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(MARK) {
        let before = &rest[..at];
        // The URL's start: the nearest `http://` / `https://` in the same
        // whitespace-free run.
        let run_start = before
            .rfind(|c: char| c.is_whitespace() || c == '(' || c == '"' || c == '\'')
            .map_or(0, |i| i + 1);
        let run = &before[run_start..];
        let scheme = run.rfind("http://").or_else(|| run.rfind("https://"));
        let after = &rest[at + MARK.len()..];
        let id_len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(after.len());
        let id = &after[..id_len];
        match (scheme, crate::proxy::target_of(state, id)) {
            (Some(s), Some((host, port))) if !id.is_empty() => {
                out.push_str(&before[..run_start + s]);
                out.push_str(&format!("http://{host}:{port}"));
                rest = &after[id_len..];
            }
            _ => {
                out.push_str(&rest[..at + MARK.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The target a `/proxy/{id}` URL is pinned to, if `href` is one.
pub(crate) fn target_in(state: &AppState, href: &str) -> Option<TargetUrl> {
    let (_, path) = split_href(href)?;
    let (id, rest) = proxy_path(path)?;
    let (host, port) = crate::proxy::target_of(state, id)?;
    Some(TargetUrl {
        host,
        port,
        path: rest,
    })
}

/// `scheme://authority` and the path (+ query + fragment) of an http(s) URL.
fn split_href(href: &str) -> Option<(&str, &str)> {
    let rest = href
        .strip_prefix("http://")
        .or_else(|| href.strip_prefix("https://"))?;
    let at = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin_len = href.len() - rest.len() + at;
    let path = &href[origin_len..];
    Some((
        &href[..origin_len],
        if path.is_empty() { "/" } else { path },
    ))
}

/// `/proxy/{id}{rest}` → (`id`, `rest` starting with `/`).
fn proxy_path(path: &str) -> Option<(&str, String)> {
    let after = path.strip_prefix("/proxy/")?;
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let (id, rest) = after.split_at(end);
    if !id.starts_with("p-") {
        return None;
    }
    let rest = match rest.chars().next() {
        None => "/".to_string(),
        Some('/') => rest.to_string(),
        Some(_) => format!("/{rest}"),
    };
    Some((id, rest))
}

/// Mint the target's proxy id: the path the view loads on its daemon origin.
async fn proxied_path(state: &AppState, url: &TargetUrl) -> Result<String, BrowserError> {
    match crate::proxy::mint(state, &url.host, url.port).await {
        Ok(id) => Ok(format!("/proxy/{id}{}", url.path)),
        Err(refusal) => Err(BrowserError::Refused(crate::browser_open::refusal_text(
            &refusal,
            "Not loaded",
            &url.host,
            url.port,
            &url.url(),
        ))),
    }
}

/// What the page's document looked like before a navigation: the mark it
/// now carries and its URL (both `None` for a view with no document yet).
struct Before {
    mark: Option<u64>,
    href: Option<String>,
}

async fn mark(state: &AppState, addr: &Addr) -> Before {
    let n = NAV_MARK.fetch_add(1, Ordering::Relaxed);
    let body = "globalThis.__chimaeraNav = args.n; return location.href;";
    match eval(state, addr, World::Isolated, body, json!({"n": n}), 2_000).await {
        Ok(href) => Before {
            mark: Some(n),
            href: href.as_str().map(str::to_owned),
        },
        Err(_) => Before {
            mark: None,
            href: None,
        },
    }
}

const FACTS_BODY: &str = "\
let s = null;\n\
try { const e = performance.getEntriesByType('navigation')[0]; s = (e && e.responseStatus) || null; } catch (_) {}\n\
return {m: globalThis.__chimaeraNav ?? null, r: document.readyState, u: location.href, t: document.title, s};";

async fn facts_now(state: &AppState, addr: &Addr) -> Result<Value, BrowserError> {
    let f = eval(state, addr, World::Isolated, FACTS_BODY, json!({}), 3_000).await?;
    Ok(facts_value(&f))
}

fn facts_value(f: &Value) -> Value {
    json!({"url": f["u"], "title": f["t"], "status": f["s"]})
}

/// Wait for the navigation that followed `before` to settle (see the module
/// header). `expected` is the proxied path a navigation asked for;
/// `history` lets a step that loads no new document settle by URL.
async fn wait_load(
    state: &AppState,
    addr: &Addr,
    before: Before,
    expected: Option<&str>,
    history: bool,
) -> Result<Value, BrowserError> {
    // A fragment-only change of the current URL never loads a new document.
    let same_document = match (expected, before.href.as_deref()) {
        (Some(want), Some(href)) => split_href(href).is_some_and(|(_, cur)| {
            let strip = |p: &str| p.split('#').next().unwrap_or("").to_string();
            want.contains('#') && strip(cur) == strip(want) && cur != want
        }),
        _ => false,
    };
    let started = Instant::now();
    let deadline = if history {
        HISTORY_TIMEOUT
    } else {
        LOAD_TIMEOUT
    };
    let mut last_err: Option<BrowserError> = None;
    let mut last_facts: Option<Value> = None;
    loop {
        tokio::time::sleep(LOAD_POLL).await;
        let poll = eval(
            state,
            addr,
            World::Isolated,
            FACTS_BODY,
            json!({}),
            POLL_EVAL_TIMEOUT.as_millis() as u64,
        )
        .await;
        match poll {
            Ok(f) => {
                let complete = f["r"] == "complete" && f["u"] != "about:blank";
                let new_document = f["m"].as_u64() != before.mark || before.mark.is_none();
                let href = f["u"].as_str().unwrap_or_default();
                let moved = before.href.as_deref().is_some_and(|b| b != href);
                let arrived = expected
                    .is_some_and(|want| split_href(href).is_some_and(|(_, cur)| cur == want));
                if complete
                    && (new_document
                        || (same_document && arrived)
                        || (history && moved && started.elapsed() > Duration::from_millis(600)))
                {
                    return Ok(facts_value(&f));
                }
                last_facts = Some(f);
            }
            Err(e @ BrowserError::Gone(_)) => return Err(e),
            Err(e) => last_err = Some(e),
        }
        if started.elapsed() >= deadline {
            if let Some(f) = last_facts {
                if history {
                    // Nothing to go back to, or a step the page handled itself.
                    return Ok(facts_value(&f));
                }
                // The page answers but hasn't settled: report what it shows,
                // like the Chromium engine (`loading: true`).
                let mut v = facts_value(&f);
                v["loading"] = Value::Bool(true);
                return Ok(v);
            }
            let why = last_err
                .map(|e| format!(" (last answer: {e})"))
                .unwrap_or_default();
            return Err(BrowserError::Timeout(format!(
                "The page did not finish loading within {} s{why}. It may still be \
                 loading: browser_snapshot shows what it has so far.",
                deadline.as_secs()
            )));
        }
    }
}

/// An isolated/page-world eval: the body's return value, or the page's
/// error. The isolated world is the shell's agent content world, never the
/// pane bridge's (an agent there could forge the bridge's reports).
async fn eval(
    state: &AppState,
    addr: &Addr,
    world: World,
    body: &str,
    args: Value,
    timeout_ms: u64,
) -> Result<Value, BrowserError> {
    let drive = json!({
        "type": "eval",
        "body": body,
        "args": args,
        "world": match world { World::Isolated => "isolated", World::Page => "page" },
        "timeout_ms": timeout_ms,
    });
    let budget = Duration::from_millis(timeout_ms) + Duration::from_secs(5);
    match drive_action(state, addr, drive, budget).await {
        Ok(out) => Ok(out.get("value").cloned().unwrap_or(Value::Null)),
        // The shell's own refusal reads as it is.
        Err(BrowserError::Engine(e)) if e.starts_with(SHELL_BUSY) => Err(BrowserError::Timeout(e)),
        // Otherwise the window's error for an eval is the script's own.
        Err(BrowserError::Engine(e)) => Err(BrowserError::Page(format!("The script failed: {e}"))),
        Err(e) => Err(e),
    }
}

async fn drive_action(
    state: &AppState,
    addr: &Addr,
    action: Value,
    timeout: Duration,
) -> Result<Value, BrowserError> {
    request(
        state,
        addr,
        "drive",
        obj(json!({"action": action})),
        timeout,
    )
    .await
}

async fn request(
    state: &AppState,
    addr: &Addr,
    op: &str,
    fields: Map<String, Value>,
    timeout: Duration,
) -> Result<Value, BrowserError> {
    state
        .browsers
        .channel
        .request(
            addr.socket,
            |rid| {
                crate::ws::frames::browser_request(
                    rid,
                    op,
                    addr.view.as_deref(),
                    addr.tab.as_deref(),
                    fields,
                )
            },
            timeout,
        )
        .await
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hrefs_split_into_origin_and_path() {
        assert_eq!(
            split_href("http://127.0.0.1:5000/proxy/p-ab/lab?x=1#c"),
            Some(("http://127.0.0.1:5000", "/proxy/p-ab/lab?x=1#c"))
        );
        assert_eq!(
            split_href("http://localhost:5000"),
            Some(("http://localhost:5000", "/"))
        );
        assert_eq!(
            split_href("http://[::1]:5000?q"),
            Some(("http://[::1]:5000", "?q"))
        );
        assert_eq!(split_href("about:blank"), None);
        assert_eq!(split_href("data:text/html,x"), None);
    }

    #[test]
    fn proxy_paths_yield_the_id_and_the_rest() {
        assert_eq!(
            proxy_path("/proxy/p-ab12/lab/tree?token=x#y"),
            Some(("p-ab12", "/lab/tree?token=x#y".to_string()))
        );
        assert_eq!(
            proxy_path("/proxy/p-ab12"),
            Some(("p-ab12", "/".to_string()))
        );
        assert_eq!(
            proxy_path("/proxy/p-ab12?x"),
            Some(("p-ab12", "/?x".to_string()))
        );
        assert_eq!(proxy_path("/proxy/other/x"), None);
        assert_eq!(proxy_path("/lab"), None);
    }
}
