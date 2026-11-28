//! The headless host: a plugin's page answering its plugin's call when no
//! window shows it (or the call asked `fresh`). Design:
//! docs/design/rich-views-plan.md, "Calls from a plugin, or an agent, to its
//! page".
//!
//! It runs on the agent browser's Chromium engine
//! (`agent_browser::chromium`): the view is rendered as a window would
//! render it, its `web` node found by id, a grant minted (`web::mint`), and
//! the page opened render-only at `/web/{grant}/{entry}?api=1` with the
//! theme a window last used here. The daemon is the other end of the
//! bridge: it hands the page the host's word (`data`, `visibility`, a
//! `call`) as `chimaera:host` events and reads its `web_answer` from a
//! `chimaera:page` event, both in the page's own world through `Eval`. What
//! else the page says (`act`, `status`, `reference`) goes nowhere: a
//! headless page's region is not the person's.
//!
//! Bounded: at most [`PAGES_MAX`] headless pages (the least recently used
//! is closed first; they share the engine's page limit with agents' own
//! pages), each closed after [`IDLE`] unused. Never on a cluster login node,
//! nor where the daemon can't tell whether it is one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::web_calls::Spec;
use super::Manifest;
use crate::agent_browser::{Action, ActionOut, TargetUrl, World};
use crate::AppState;

/// Headless pages at once.
const PAGES_MAX: usize = 2;
/// A headless page unused this long is closed (the reaper's minute).
const IDLE: Duration = Duration::from_secs(120);
/// The page's viewport.
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
/// A window's theme blob kept per workspace, and how many workspaces.
const THEME_MAX: usize = 8 * 1024;
const THEMES_MAX: usize = 64;

struct Page {
    /// The engine's page key.
    key: String,
    /// What it shows: the entry, the build, and its data (as sent).
    entry: String,
    sha: Arc<str>,
    data: String,
    used: Instant,
}

/// The headless host's state (`Platform::web_headless`).
#[derive(Default)]
pub(crate) struct Headless {
    pages: Mutex<HashMap<String, Page>>,
    /// The `chimaera-theme` a window last minted a page with, per workspace.
    themes: Mutex<HashMap<String, String>>,
    /// One page opened at a time.
    opening: tokio::sync::Mutex<()>,
}

impl Headless {
    /// Keep the theme a window shows its pages in (`web::mint_route`).
    pub(crate) fn keep_theme(&self, ws: &str, theme: &str) {
        let ok = !theme.is_empty()
            && theme.len() <= THEME_MAX
            && theme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'=');
        if !ok {
            return;
        }
        let mut themes = crate::lock(&self.themes);
        if !themes.contains_key(ws) && themes.len() >= THEMES_MAX {
            themes.clear();
        }
        themes.insert(ws.to_string(), theme.to_string());
    }

    fn theme(&self, ws: &str) -> Option<String> {
        crate::lock(&self.themes).get(ws).cloned()
    }
}

/// What a plugin's call reads when no window shows its view and the daemon
/// may not start a browser here.
pub(crate) fn login_node_text(view: &str) -> String {
    format!(
        "No Chimaera window shows {view}, and this daemon runs on a cluster login node, where \
         Chimaera never starts a browser. Open the view in a window, or open the workspace as a \
         cluster job."
    )
}

/// Answer `spec` from a headless page.
pub(crate) async fn call(
    state: &Arc<AppState>,
    m: &Arc<Manifest>,
    ws: &str,
    call: &str,
    spec: &Spec,
) -> Result<Value, String> {
    let decl = m
        .views
        .iter()
        .find(|v| v.id == spec.view)
        .ok_or_else(|| format!("{} declares no view {:?}", m.name, spec.view))?;
    let title = format!("{} ({})", decl.title, m.name);
    if state.compute.login_node_unknown() {
        let _ = state.compute.detection().await;
    }
    if state.compute.is_login_node() || state.compute.login_node_unknown() {
        return Err(login_node_text(&title));
    }
    let (entry, data) = render_node(state, m, ws, spec, decl.slot.as_str()).await?;
    let key = page_key(&m.id, ws, &spec.view, &spec.id, spec.file.as_deref());
    let engine = &state.browsers.chromium;
    let unable = |e: crate::agent_browser::BrowserError| {
        format!("No Chimaera window shows {title}, and the headless browser couldn't answer: {e}")
    };
    {
        let _one = state.plugin_platform.web_headless.opening.lock().await;
        let path = super::web::mint(state, m, ws, &entry).await?;
        let fresh = {
            let pages = crate::lock(&state.plugin_platform.web_headless.pages);
            pages
                .get(&key)
                .is_none_or(|p| p.entry != entry || p.sha != m.wasm.sha256)
        };
        if fresh {
            close_over(state, PAGES_MAX - 1, &key).await;
            let mut query = String::from("?api=1");
            if let Some(theme) = state.plugin_platform.web_headless.theme(ws) {
                query.push_str("&chimaera-theme=");
                query.push_str(&theme);
            }
            let url = TargetUrl {
                host: "127.0.0.1".into(),
                port: state.port,
                path: format!("{path}{query}"),
            };
            let opened = engine
                .act(
                    state,
                    Some(ws),
                    &key,
                    Action::Open {
                        url,
                        width: WIDTH,
                        height: HEIGHT,
                    },
                )
                .await
                .map_err(unable)?;
            if let ActionOut::Value(v) = &opened {
                let status = v["status"].as_u64().unwrap_or(200);
                if status >= 400 {
                    return Err(format!(
                        "No Chimaera window shows {title}, and the headless page didn't load \
                         (HTTP {status})."
                    ));
                }
            }
            crate::lock(&state.plugin_platform.web_headless.pages).insert(
                key.clone(),
                Page {
                    key: key.clone(),
                    entry: entry.clone(),
                    sha: m.wasm.sha256.clone(),
                    data: String::new(),
                    used: Instant::now(),
                },
            );
        }
        // The node's data, when it changed (a new page has none yet), and
        // the page shown: the host's word, as a window gives it.
        let sent = data.to_string();
        let stale = {
            let mut pages = crate::lock(&state.plugin_platform.web_headless.pages);
            match pages.get_mut(&key) {
                Some(p) => {
                    p.used = Instant::now();
                    let stale = p.data != sent;
                    p.data = sent.clone();
                    stale
                }
                None => true,
            }
        };
        if stale {
            let host = engine
                .act(
                    state,
                    Some(ws),
                    &key,
                    Action::Eval {
                        world: World::Page,
                        body: HOST_SCRIPT.to_string(),
                        args: json!({"data": data}),
                        timeout_ms: 5_000,
                    },
                )
                .await;
            if let Err(e) = host {
                forget(state, &key);
                return Err(unable(e));
            }
        }
    }
    let ms = super::web_calls::CALL_TIMEOUT.as_millis() as u64 - 2_000;
    let answer = engine
        .act(
            state,
            Some(ws),
            &key,
            Action::Eval {
                world: World::Page,
                body: CALL_SCRIPT.to_string(),
                args: json!({"id": call, "method": spec.method, "params": spec.args, "ms": ms}),
                timeout_ms: ms + 1_000,
            },
        )
        .await;
    if let Some(p) = crate::lock(&state.plugin_platform.web_headless.pages).get_mut(&key) {
        p.used = Instant::now();
    }
    match answer {
        Ok(ActionOut::Value(msg)) => answer_of(&msg),
        Ok(_) => Err("the headless page gave no answer".into()),
        Err(e) => {
            if matches!(e, crate::agent_browser::BrowserError::Gone(_)) {
                forget(state, &key);
            }
            Err(unable(e))
        }
    }
}

/// The page's `web_answer` as the call's outcome.
fn answer_of(msg: &Value) -> Result<Value, String> {
    if msg["ok"] == json!(true) {
        Ok(msg.get("value").cloned().unwrap_or(Value::Null))
    } else {
        Err(msg["error"]
            .as_str()
            .unwrap_or("the page failed without saying why")
            .chars()
            .take(2048)
            .collect())
    }
}

/// The view's tree, as a window gets it, and the `web` node `spec.id` in
/// it: its entry and data.
async fn render_node(
    state: &Arc<AppState>,
    m: &Arc<Manifest>,
    ws: &str,
    spec: &Spec,
    slot: &str,
) -> Result<(String, Value), String> {
    let args = json!({"file": spec.file, "width": "wide", "slot": slot, "args": {}});
    let text = state
        .plugin_runtime
        .render(state, m, ws, &spec.view, &args.to_string())
        .await
        .map_err(|e| format!("{} couldn't draw {}: {e}", m.name, spec.view))?;
    let tree = super::screens::check_tree(&text).map_err(|problems| {
        format!(
            "{} sent a screen chimaera could not draw: {}",
            m.name,
            problems.join("; ")
        )
    })?;
    let node = find_web_node(&tree, &spec.id).ok_or_else(|| {
        format!(
            "{}'s view {} has no web node {:?} to ask",
            m.name, spec.view, spec.id
        )
    })?;
    let entry = node["entry"].as_str().unwrap_or_default().to_string();
    Ok((entry, node.get("data").cloned().unwrap_or(Value::Null)))
}

/// The `web` node with `id` in a checked tree (depth-first).
pub(crate) fn find_web_node<'a>(tree: &'a Value, id: &str) -> Option<&'a Value> {
    match tree {
        Value::Object(o) => {
            if o.get("type").and_then(Value::as_str) == Some("web")
                && o.get("id").and_then(Value::as_str) == Some(id)
            {
                return Some(tree);
            }
            o.values().find_map(|v| find_web_node(v, id))
        }
        Value::Array(a) => a.iter().find_map(|v| find_web_node(v, id)),
        _ => None,
    }
}

/// The engine's key for one node's page.
fn page_key(plugin: &str, ws: &str, view: &str, id: &str, file: Option<&str>) -> String {
    let what = format!("{plugin}\n{ws}\n{view}\n{id}\n{}", file.unwrap_or(""));
    format!("webview:{}", &super::web::fnv(what.as_bytes()))
}

/// Close the least recently used pages until at most `keep` are open
/// besides `except`.
async fn close_over(state: &Arc<AppState>, keep: usize, except: &str) {
    let victims: Vec<String> = {
        let pages = crate::lock(&state.plugin_platform.web_headless.pages);
        let mut others: Vec<(&Instant, &String)> = pages
            .values()
            .filter(|p| p.key != except)
            .map(|p| (&p.used, &p.key))
            .collect();
        others.sort();
        let n = others.len().saturating_sub(keep);
        others.into_iter().take(n).map(|(_, k)| k.clone()).collect()
    };
    for key in victims {
        close(state, &key).await;
    }
}

async fn close(state: &Arc<AppState>, key: &str) {
    forget(state, key);
    let _ = state
        .browsers
        .chromium
        .act(state, None, key, Action::Close)
        .await;
}

fn forget(state: &AppState, key: &str) {
    crate::lock(&state.plugin_platform.web_headless.pages).remove(key);
}

/// Close headless pages unused for `IDLE` (the agent browser's reaper,
/// every minute).
pub(crate) async fn reap(state: &Arc<AppState>) {
    let idle: Vec<String> = crate::lock(&state.plugin_platform.web_headless.pages)
        .values()
        .filter(|p| p.used.elapsed() >= IDLE)
        .map(|p| p.key.clone())
        .collect();
    for key in idle {
        close(state, &key).await;
    }
}

/// The host's word to a fresh page, as a window gives it: its data, and
/// that it is shown.
const HOST_SCRIPT: &str = r#"
const say = (cmd) => document.dispatchEvent(new CustomEvent("chimaera:host", { detail: JSON.stringify(cmd) }));
say({ op: "data", data });
say({ op: "visibility", visible: true });
return true;
"#;

/// One call: the page's `web_answer` for `id`, or a time-out in words.
const CALL_SCRIPT: &str = r#"
return await new Promise((resolve) => {
  let timer = null;
  const on = (e) => {
    if (typeof e.detail !== "string") return;
    let msg;
    try { msg = JSON.parse(e.detail); } catch { return; }
    if (msg === null || msg.kind !== "web_answer" || msg.id !== id) return;
    done(msg);
  };
  const done = (msg) => {
    document.removeEventListener("chimaera:page", on);
    clearTimeout(timer);
    resolve(msg);
  };
  document.addEventListener("chimaera:page", on);
  timer = setTimeout(() => done({ ok: false, error: `the page didn't answer ${method} in time` }), ms);
  if (typeof globalThis.chimaera !== "object") {
    done({ ok: false, error: "the page has no chimaera API (it is not a plugin page)" });
    return;
  }
  document.dispatchEvent(new CustomEvent("chimaera:host", {
    detail: JSON.stringify({ op: "call", id, method, args: params }),
  }));
});
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_node_is_found_by_id_anywhere_in_the_tree() {
        let tree = json!({"ui": "1", "root": {"type": "stack", "children": [
            {"type": "web", "id": "a", "entry": "a.html"},
            {"type": "card", "children": [{"type": "web", "id": "genome", "entry": "g.html", "data": {"x": 1}}]},
        ]}});
        assert_eq!(find_web_node(&tree, "genome").unwrap()["entry"], "g.html");
        assert!(find_web_node(&tree, "none").is_none());
    }

    #[test]
    fn answers_read_plainly() {
        assert_eq!(
            answer_of(&json!({"ok": true, "value": {"n": 2}})),
            Ok(json!({"n": 2}))
        );
        assert_eq!(answer_of(&json!({"ok": true})), Ok(Value::Null));
        assert_eq!(
            answer_of(&json!({"ok": false, "error": "no handler for look"})),
            Err("no handler for look".into())
        );
    }

    #[test]
    fn a_theme_is_kept_only_when_it_is_a_theme_blob() {
        let h = Headless::default();
        h.keep_theme("w", "eyJtIjoiZGFyayJ9");
        assert_eq!(h.theme("w").as_deref(), Some("eyJtIjoiZGFyayJ9"));
        h.keep_theme("w", "x&evil=1");
        assert_eq!(h.theme("w").as_deref(), Some("eyJtIjoiZGFyayJ9"));
    }

    #[test]
    fn the_login_node_refusal_says_what_to_do() {
        let text = login_node_text("Genome view (GenomeSpy)");
        assert!(text.contains("cluster login node"), "{text}");
        assert!(text.contains("Open the view in a window"), "{text}");
    }
}
