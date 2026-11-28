//! Calls from a plugin to its own page (API 0.4, `web-call`): the plugin
//! names a `web` node of one of its views and a method; the page answers
//! through `chimaera.handle(method, fn)` (`chimaera-core` `page_api.js`).
//! Design: docs/design/rich-views-plan.md, "Calls from a plugin, or an
//! agent, to its page".
//!
//! Who answers: the window showing that node on screen (its `browser_hello`
//! lists its shown web nodes, `agent_browser::channel`), most recently
//! focused first, asked over the window request channel (`web_call`); when
//! no window shows it, or the plugin asked `fresh`, the headless host
//! (`web_headless`). `web-call` returns the call's id at once; the plugin
//! answers its tool with `wait` on it, and `call_tool` holds the agent's
//! call until the answer comes (`tool_wait`), then asks `tool-resume`,
//! where the plugin reads it (`web-call-result`).
//!
//! Bounded: [`IN_FLIGHT_PER_PLUGIN`] / [`IN_FLIGHT`] calls wait at once,
//! each at most [`CALL_TIMEOUT`]; an answer is at most [`ANSWER_MAX`] of
//! JSON; a finished call is kept [`KEEP`] (at most [`DONE_PER_PLUGIN`] per
//! plugin) for the plugin to read, then forgotten.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::watch;

use super::platform::Slot;
use super::Manifest;
use crate::AppState;

/// How long a call may take, window or headless, launch included.
pub(crate) const CALL_TIMEOUT: Duration = super::jobs::TOOL_WAIT;
/// A call's arguments and a page's answer, as JSON.
pub(crate) const ARGS_MAX: usize = 64 * 1024;
pub(crate) const ANSWER_MAX: usize = 64 * 1024;
/// Calls waiting at once, per plugin and daemon-wide.
const IN_FLIGHT_PER_PLUGIN: usize = 8;
const IN_FLIGHT: usize = 32;
/// A finished call's answer is kept this long, at most this many a plugin.
const KEEP: Duration = Duration::from_secs(120);
const DONE_PER_PLUGIN: usize = 64;
/// A method's name and a node id's, in bytes.
const METHOD_MAX: usize = 64;
pub(crate) const NODE_ID_MAX: usize = 128;
/// What a call's id starts with (a job's is `j-`).
const PREFIX: &str = "wc-";

/// A call's outcome: none while it waits.
type Outcome = Option<Result<Value, String>>;

struct Entry {
    plugin: String,
    ws: String,
    rx: watch::Receiver<Outcome>,
    started: Instant,
    /// When it finished (it is kept `KEEP` from then).
    done: Option<Instant>,
}

/// The daemon's calls (`Platform::web_calls`).
#[derive(Default)]
pub(crate) struct WebCalls {
    calls: Mutex<HashMap<String, Entry>>,
}

/// What a plugin asks: `web-call`'s spec.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Spec {
    pub(crate) view: String,
    pub(crate) id: String,
    pub(crate) method: String,
    #[serde(default)]
    pub(crate) args: Value,
    #[serde(default)]
    pub(crate) file: Option<String>,
    #[serde(default)]
    pub(crate) fresh: bool,
}

/// Whether `id` names a page call (rather than a job).
pub(crate) fn is_call(id: &str) -> bool {
    id.starts_with(PREFIX)
}

/// `spec` checked against `m`: the view is one of its own, the node id and
/// method are short names, the arguments fit.
pub(crate) fn parse(m: &Manifest, spec: &str) -> Result<Spec, String> {
    if spec.len() > ARGS_MAX + 4096 {
        return Err(format!(
            "a call's arguments are at most {} KiB of JSON",
            ARGS_MAX >> 10
        ));
    }
    let spec: Spec = serde_json::from_str(spec).map_err(|e| format!("not a web call spec: {e}"))?;
    let Some(view) = m.views.iter().find(|v| v.id == spec.view) else {
        return Err(format!("{} declares no view {:?}", m.name, spec.view));
    };
    if m.web.as_ref().is_none_or(|w| w.pages.is_empty()) {
        return Err(format!("{} declares no web pages ([web] pages)", m.name));
    }
    if spec.id.is_empty() || spec.id.len() > NODE_ID_MAX {
        return Err(format!(
            "a call names its web node's id (1 to {NODE_ID_MAX} bytes)"
        ));
    }
    let name_ok = |s: &str| {
        !s.is_empty()
            && s.len() <= METHOD_MAX
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
    };
    if !name_ok(&spec.method) {
        return Err(format!(
            "a method is 1 to {METHOD_MAX} letters, digits or _ - . :"
        ));
    }
    if spec.args.to_string().len() > ARGS_MAX {
        return Err(format!(
            "a call's arguments are at most {} KiB of JSON",
            ARGS_MAX >> 10
        ));
    }
    match (&spec.file, view.slot) {
        (Some(file), _) => {
            if file.is_empty() || super::hostfns_relative(file).is_err() {
                return Err(format!("{file:?} is not a path in this workspace"));
            }
            if view.slot == Slot::File
                && super::platform::file_kind(m, file).is_none_or(|k| k.view != view.id)
            {
                return Err(format!("{} does not open {file} in {}", m.name, view.id));
            }
        }
        (None, Slot::File) if spec.fresh => {
            return Err(format!(
                "{} is a file view: a fresh call names its file",
                view.id
            ))
        }
        _ => {}
    }
    Ok(spec)
}

impl WebCalls {
    /// Forget calls finished longer than `KEEP` ago, and a plugin's oldest
    /// past `DONE_PER_PLUGIN`.
    fn prune(calls: &mut HashMap<String, Entry>) {
        calls.retain(|_, e| e.done.is_none_or(|at| at.elapsed() < KEEP));
        let mut done: HashMap<&str, Vec<(Instant, &str)>> = HashMap::new();
        for (id, e) in calls.iter() {
            if let Some(at) = e.done {
                done.entry(&e.plugin).or_default().push((at, id));
            }
        }
        let over: Vec<String> = done
            .into_values()
            .filter(|v| v.len() > DONE_PER_PLUGIN)
            .flat_map(|mut v| {
                v.sort();
                let n = v.len() - DONE_PER_PLUGIN;
                v.into_iter()
                    .take(n)
                    .map(|(_, id)| id.to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        for id in over {
            calls.remove(&id);
        }
    }

    /// Record a new call for `plugin` in `ws`: its id and the sender that
    /// finishes it, or why it can't wait (too many already do).
    fn admit(&self, plugin: &str, ws: &str) -> Result<(String, watch::Sender<Outcome>), String> {
        let mut calls = crate::lock(&self.calls);
        Self::prune(&mut calls);
        let waiting = calls.values().filter(|e| e.done.is_none());
        let (mine, all) = waiting.fold((0, 0), |(m, a), e| {
            (m + usize::from(e.plugin == plugin), a + 1)
        });
        if mine >= IN_FLIGHT_PER_PLUGIN {
            return Err(format!(
                "{IN_FLIGHT_PER_PLUGIN} calls to this plugin's pages are already waiting"
            ));
        }
        if all >= IN_FLIGHT {
            return Err("too many calls to plugin pages are waiting; try again in a moment".into());
        }
        let id = format!("{PREFIX}{}", &chimaera_core::generate_token()[..16]);
        let (tx, rx) = watch::channel(None);
        calls.insert(
            id.clone(),
            Entry {
                plugin: plugin.to_string(),
                ws: ws.to_string(),
                rx,
                started: Instant::now(),
                done: None,
            },
        );
        Ok((id, tx))
    }

    fn finish(&self, id: &str, tx: &watch::Sender<Outcome>, outcome: Result<Value, String>) {
        let _ = tx.send(Some(outcome));
        if let Some(e) = crate::lock(&self.calls).get_mut(id) {
            e.done = Some(Instant::now());
        }
    }

    /// The call's receiver, if it is `plugin`'s in `ws`.
    fn receiver(&self, plugin: &str, ws: &str, id: &str) -> Option<watch::Receiver<Outcome>> {
        let mut calls = crate::lock(&self.calls);
        Self::prune(&mut calls);
        calls
            .get(id)
            .filter(|e| e.plugin == plugin && e.ws == ws)
            .map(|e| e.rx.clone())
    }

    /// `web-call-result`: the answer as JSON text, none while it waits, or
    /// why it failed.
    pub(crate) fn result(
        &self,
        plugin: &str,
        ws: &str,
        id: &str,
    ) -> Result<Option<String>, String> {
        let Some(rx) = self.receiver(plugin, ws, id) else {
            return Err(format!(
                "no call {id} here (a finished call is kept {} minutes)",
                KEEP.as_secs() / 60
            ));
        };
        let outcome = rx.borrow().clone();
        match outcome {
            None => Ok(None),
            Some(Ok(v)) => Ok(Some(v.to_string())),
            Some(Err(why)) => Err(why),
        }
    }

    /// Wait (at most `timeout`) for `plugin`'s call to finish; whether it did.
    pub(crate) async fn wait(&self, plugin: &str, id: &str, timeout: Duration) -> bool {
        let rx = {
            let calls = crate::lock(&self.calls);
            calls
                .get(id)
                .filter(|e| e.plugin == plugin)
                .map(|e| e.rx.clone())
        };
        let Some(mut rx) = rx else {
            return false;
        };
        tokio::time::timeout(timeout, rx.wait_for(|o| o.is_some()))
            .await
            .is_ok_and(|r| r.is_ok())
    }

    /// Calls still waiting (tests, diagnostics).
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        crate::lock(&self.calls)
            .values()
            .filter(|e| e.done.is_none())
            .count()
    }

    /// How long the oldest waiting call has waited (diagnostics).
    #[allow(dead_code)]
    pub(crate) fn oldest(&self) -> Option<Duration> {
        crate::lock(&self.calls)
            .values()
            .filter(|e| e.done.is_none())
            .map(|e| e.started.elapsed())
            .max()
    }
}

/// `web-call`: check, record, and dispatch in the background; the call's id.
pub(crate) fn start(
    state: &Arc<AppState>,
    m: &Arc<Manifest>,
    ws: &str,
    spec: &str,
) -> Result<String, String> {
    let spec = parse(m, spec)?;
    let calls = &state.plugin_platform.web_calls;
    let (id, tx) = calls.admit(&m.id, ws)?;
    let (state, m, ws, call) = (state.clone(), m.clone(), ws.to_string(), id.clone());
    tokio::spawn(async move {
        let outcome =
            match tokio::time::timeout(CALL_TIMEOUT, dispatch(&state, &m, &ws, &call, &spec)).await
            {
                Ok(outcome) => outcome.and_then(bounded),
                Err(_) => Err(format!(
                    "the page didn't answer {:?} within {} s",
                    spec.method,
                    CALL_TIMEOUT.as_secs()
                )),
            };
        if let Err(why) = &outcome {
            tracing::debug!(plugin = %m.id, call = %call, %why, "web call failed");
        }
        state.plugin_platform.web_calls.finish(&call, &tx, outcome);
    });
    Ok(id)
}

/// An answer within `ANSWER_MAX`.
fn bounded(v: Value) -> Result<Value, String> {
    let len = v.to_string().len();
    if len > ANSWER_MAX {
        return Err(format!(
            "the page's answer is {} KiB, over the {} KiB a call may return (save a picture or \
             a table with chimaera.save and answer its output: path)",
            len >> 10,
            ANSWER_MAX >> 10
        ));
    }
    Ok(v)
}

/// The window showing the node, else the headless host.
async fn dispatch(
    state: &Arc<AppState>,
    m: &Arc<Manifest>,
    ws: &str,
    call: &str,
    spec: &Spec,
) -> Result<Value, String> {
    if !spec.fresh {
        let node = WebViewRef {
            plugin: &m.id,
            view: &spec.view,
            id: &spec.id,
            file: spec.file.as_deref(),
        };
        if let Some(socket) = state.browsers.channel.socket_with_web_view(ws, &node) {
            match window_call(state, socket, &m.id, call, spec).await {
                Ok(v) => return Ok(v),
                // The window went away mid-call: a headless page answers.
                Err(crate::agent_browser::BrowserError::Gone(_)) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    super::web_headless::call(state, m, ws, call, spec).await
}

/// One web node a window says it shows.
pub(crate) struct WebViewRef<'a> {
    pub(crate) plugin: &'a str,
    pub(crate) view: &'a str,
    pub(crate) id: &'a str,
    /// The plugin's file (none: any file the view shows).
    pub(crate) file: Option<&'a str>,
}

/// Ask `socket`'s window: its node runs the page's handler.
async fn window_call(
    state: &AppState,
    socket: crate::agent_browser::channel::SocketId,
    plugin: &str,
    call: &str,
    spec: &Spec,
) -> Result<Value, crate::agent_browser::BrowserError> {
    let fields = json!({
        "plugin": plugin,
        "node_view": spec.view,
        "node": spec.id,
        "file": spec.file,
        "call": call,
        "method": spec.method,
        "args": spec.args,
    });
    let fields = match fields {
        Value::Object(m) => m,
        _ => unreachable!("an object"),
    };
    let out = state
        .browsers
        .channel
        .request(
            socket,
            |rid| crate::ws::frames::browser_request(rid, "web_call", None, None, fields),
            CALL_TIMEOUT,
        )
        .await?;
    Ok(out.get("value").cloned().unwrap_or(Value::Null))
}

/// `tool_wait` for a call: whether it finished within the hold.
pub(crate) async fn tool_wait(state: &AppState, plugin: &str, call: &str) -> bool {
    state
        .plugin_platform
        .web_calls
        .wait(plugin, call, super::jobs::TOOL_WAIT)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn calls_are_capped_scoped_and_kept_for_their_plugin() {
        let calls = WebCalls::default();
        let mut senders = Vec::new();
        for _ in 0..IN_FLIGHT_PER_PLUGIN {
            senders.push(calls.admit("p", "w").unwrap());
        }
        let err = calls.admit("p", "w").unwrap_err();
        assert!(err.contains("already waiting"), "{err}");
        // Another plugin still may.
        let (other, other_tx) = calls.admit("q", "w").unwrap();
        assert_eq!(calls.waiting(), IN_FLIGHT_PER_PLUGIN + 1);
        let (id, tx) = senders.pop().unwrap();
        assert!(is_call(&id));
        assert_eq!(calls.result("p", "w", &id), Ok(None));
        calls.finish(&id, &tx, Ok(json!({"region": "chr17"})));
        assert!(calls.wait("p", &id, Duration::from_millis(10)).await);
        assert_eq!(
            calls.result("p", "w", &id),
            Ok(Some(r#"{"region":"chr17"}"#.to_string()))
        );
        // Another plugin or workspace can't read it.
        assert!(calls.result("q", "w", &id).is_err());
        assert!(calls.result("p", "elsewhere", &id).is_err());
        calls.finish(&other, &other_tx, Err("no handler".into()));
        assert_eq!(calls.result("q", "w", &other), Err("no handler".into()));
        // A slot freed: one more may wait.
        assert!(calls.admit("p", "w").is_ok());
        assert!(
            !calls
                .wait("p", "wc-unknown", Duration::from_millis(10))
                .await
        );
    }

    #[test]
    fn an_answer_over_the_cap_is_refused_in_words() {
        assert!(bounded(json!({"a": 1})).is_ok());
        let big = json!({"rows": "x".repeat(ANSWER_MAX)});
        let err = bounded(big).unwrap_err();
        assert!(err.contains("chimaera.save"), "{err}");
    }
}
