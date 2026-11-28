//! Reveal (API 0.4, `reveal`): a plugin opens one of its views, or a file
//! it claims, beside the agent session whose call it is answering, without
//! taking focus. Design: docs/design/rich-views-plan.md, "Reveal: an agent
//! opens a view next to itself".
//!
//! It is `browser_show`'s hand-over for plugin views: a `plugin_reveal`
//! frame goes to every connected window at once, as `open_browser`'s
//! `browser_open` does (`browser_open::present`), and each window decides
//! with the same rule (the one holding the session's pane, else a visible
//! window on its workspace) and places the tab beside the session's pane
//! without moving the person's focus (`web-ui` `browser/agentOpen.ts`).
//!
//! Only a session's own call may reveal; at most [`PER_MINUTE`] a minute per
//! session; each is written to the plugin's activity log.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::platform::Slot;
use super::Manifest;
use crate::AppState;

/// Reveals per session in any minute.
const PER_MINUTE: usize = 4;
const MINUTE: Duration = Duration::from_secs(60);
/// Sessions whose reveals are remembered; the least recent is forgotten.
const SESSIONS_CAP: usize = 64;
/// A reveal's `args`, as JSON.
pub(crate) const ARGS_MAX: usize = 4 * 1024;

/// Per-session reveal times (`Platform::reveals`).
#[derive(Default)]
pub(crate) struct Reveals {
    sends: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Reveals {
    /// Record a reveal if `session` is within its minute's budget.
    fn admit(&self, session: &str) -> Result<(), String> {
        let mut sends = crate::lock(&self.sends);
        sends.retain(|_, s| s.back().is_some_and(|at| at.elapsed() < MINUTE));
        if !sends.contains_key(session) && sends.len() >= SESSIONS_CAP {
            let stalest = sends
                .iter()
                .min_by_key(|(_, s)| s.back().copied())
                .map(|(id, _)| id.clone());
            if let Some(id) = stalest {
                sends.remove(&id);
            }
        }
        let times = sends.entry(session.to_string()).or_default();
        while times.front().is_some_and(|at| at.elapsed() >= MINUTE) {
            times.pop_front();
        }
        if times.len() >= PER_MINUTE {
            let wait = MINUTE.saturating_sub(times[0].elapsed()).as_secs().max(1);
            return Err(format!(
                "Not opened: this session already opened {PER_MINUTE} views in the last minute. \
                 Wait {wait}s, or keep using one that is open."
            ));
        }
        times.push_back(Instant::now());
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    #[serde(default)]
    view: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    args: Option<Value>,
}

/// What to open: a tab view, or a claimed file (in the view it opens in),
/// with the view's args.
#[derive(Debug, PartialEq)]
pub(crate) struct Target {
    pub(crate) view: String,
    pub(crate) title: String,
    pub(crate) file: Option<String>,
    pub(crate) args: Map<String, Value>,
}

/// `spec` checked against `m`.
pub(crate) fn parse(m: &Manifest, spec: &str) -> Result<Target, String> {
    if spec.len() > ARGS_MAX + 2048 {
        return Err(format!(
            "a reveal's args are at most {} KiB of JSON",
            ARGS_MAX >> 10
        ));
    }
    let spec: Spec = serde_json::from_str(spec).map_err(|e| format!("not a reveal spec: {e}"))?;
    let args = match spec.args {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(a)) => a,
        Some(_) => return Err("a reveal's args are a JSON object".into()),
    };
    if Value::Object(args.clone()).to_string().len() > ARGS_MAX {
        return Err(format!(
            "a reveal's args are at most {} KiB of JSON",
            ARGS_MAX >> 10
        ));
    }
    match (spec.view, spec.file) {
        (Some(view), None) => {
            let Some(decl) = m.views.iter().find(|v| v.id == view) else {
                return Err(format!("{} declares no view {view:?}", m.name));
            };
            if decl.slot != Slot::Tab {
                return Err(format!(
                    "{view} is a {} view: reveal opens a tab view, or a file in its view",
                    decl.slot.as_str()
                ));
            }
            Ok(Target {
                view,
                title: decl.title.clone(),
                file: None,
                args,
            })
        }
        (None, Some(file)) => {
            if file.is_empty() || super::hostfns_relative(&file).is_err() {
                return Err(format!("{file:?} is not a path in this workspace"));
            }
            let Some(kind) = super::platform::file_kind(m, &file) else {
                return Err(format!("{} doesn't open {file}", m.name));
            };
            let title = m
                .views
                .iter()
                .find(|v| v.id == kind.view)
                .map_or_else(|| kind.view.clone(), |v| v.title.clone());
            Ok(Target {
                view: kind.view.clone(),
                title,
                file: Some(file),
                args,
            })
        }
        _ => Err("a reveal names a view or a file (one of them)".into()),
    }
}

/// `reveal`: open `spec` beside `session`. What happened, in words.
pub(crate) async fn reveal(
    state: &Arc<AppState>,
    m: &Manifest,
    ws: &str,
    session: Option<&str>,
    spec: &str,
) -> Result<String, String> {
    let Some(session) = session.filter(|s| !s.is_empty()) else {
        return Err(
            "reveal opens a view beside the agent session whose tool call this is; this call \
             came from no agent's tool call"
                .into(),
        );
    };
    let target = parse(m, spec)?;
    // A reveal nobody could see costs nothing.
    if state.browsers.channel.socket_count() == 0 {
        return Err(format!(
            "Not opened: no Chimaera window is connected right now. Tell the user to open {} \
             themselves.",
            target.title
        ));
    }
    state.plugin_platform.reveals.admit(session)?;
    let frame = crate::ws::frames::plugin_reveal(
        session,
        ws,
        &m.id,
        &target.view,
        target.file.as_deref(),
        &target.args,
    );
    state
        .browsers
        .channel
        .broadcast(Arc::from(frame.to_string()));
    super::activity::record(
        state,
        &m.id,
        json!({
            "kind": "reveal",
            "workspace": ws,
            "session": session,
            "view": target.view,
            "file": target.file,
        }),
    )
    .await;
    let what = match &target.file {
        Some(file) => format!("{file} in {}", target.title),
        None => target.title.clone(),
    };
    Ok(format!(
        "Asked the windows on this workspace to open {what} beside the session, without taking \
         the user's focus. This says nothing about whether the user is looking."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_a_minute_per_session() {
        let r = Reveals::default();
        for _ in 0..PER_MINUTE {
            r.admit("s-1").unwrap();
        }
        let err = r.admit("s-1").unwrap_err();
        assert!(err.contains("4 views in the last minute"), "{err}");
        assert!(r.admit("s-2").is_ok(), "another session has its own");
    }
}
