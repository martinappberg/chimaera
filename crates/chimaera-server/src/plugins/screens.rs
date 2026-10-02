//! Screens in the Chimaera format (`ui/1`): the routes that render a
//! plugin's view and deliver its actions, the check every tree passes
//! before a window sees it, and invalidation. Design:
//! docs/design/plugin-platform-plan.md §3; every node and prop:
//! docs/agent-guides/plugins.md ("Screens").
//!
//! - **Checked on arrival.** A tree is at most `TREE_MAX` bytes and
//!   `NODES_MAX` nodes; every node is an object with a `type`; the nodes
//!   this version knows carry their required props (a label on every
//!   input, alt text on every image), and actions are names the plugin
//!   may use. A tree that fails is not drawn: the window says so, and the
//!   daemon log gets each problem with its JSON path.
//! - **Unknown nodes are allowed** (a newer plugin, an older chimaera): the
//!   UI draws their `fallback`, else a quiet placeholder with the children.
//! - **Invalidation coalesces**: at most `RENDERS_PER_SECOND` `view` frames
//!   a second per (plugin, workspace, view); a burst ends in one frame.
//! - **Views mark activity**: a render records that the plugin's view is
//!   open in the workspace (and which file a file view shows), which is
//!   what the watch sweep (`files`) keys on.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::platform::{valid_name, Slot};
use super::Manifest;
use crate::AppState;

pub(crate) const TREE_MAX: usize = 256 << 10;
const NODES_MAX: usize = 5_000;
const DEPTH_MAX: usize = 64;
/// Rows a `list` or `table` carries in a tree; more page through `query`.
const ROWS_MAX: usize = 200;
const PROBLEMS_SHOWN: usize = 20;
const RENDERS_PER_SECOND: u32 = 4;
/// A query's answer.
const QUERY_MAX: usize = 1 << 20;
/// A view counts as open this long after its last render.
pub(crate) const OPEN_FOR: Duration = Duration::from_secs(10 * 60);

/// Actions the UI (or the host) carries out itself: a plugin names them on
/// a button, never declares or handles them.
pub(crate) const BUILTIN_ACTIONS: &[&str] = &[
    "save-to-workspace",
    "open-file",
    "open-view",
    "open-url",
    "copy",
    "ask-agent",
    "install-tool",
];

/// The props each known node needs, and their JSON kind.
fn required(node: &str) -> Option<&'static [(&'static str, Kind)]> {
    use Kind::*;
    Some(match node {
        "stack" | "row" | "grid" | "card" => &[],
        "split" => &[("children", Array)],
        "tabs" => &[("tabs", Array)],
        "section" => &[("title", Str)],
        "divider" => &[],
        "text" | "heading" | "markdown" | "code" | "badge" | "callout" | "status" => {
            &[("text", Str)]
        }
        "keyvalue" => &[("items", Array)],
        "icon" => &[("name", Str)],
        "progress" => &[],
        "empty" => &[("title", Str)],
        "list" => &[("items", Array)],
        "table" => &[("columns", Array), ("rows", Array)],
        "file" => &[("path", Str)],
        "link" => &[("text", Str)],
        "image" => &[("src", Str), ("alt", Str)],
        "button" => &[("label", Str), ("action", Str)],
        "toggle" => &[("label", Str), ("name", Str)],
        "select" => &[("label", Str), ("name", Str), ("options", Array)],
        "segmented" => &[("name", Str), ("options", Array)],
        "textfield" => &[("label", Str), ("name", Str)],
        "form" => &[("action", Str)],
        "editor" => &[("path", Str)],
        "pdf" => &[("src", Str)],
        "diagnostics" => &[],
        "diff" => &[],
        "log" => &[("src", Str)],
        _ => return None,
    })
}

#[derive(Clone, Copy)]
enum Kind {
    Str,
    Array,
}

struct Walk {
    nodes: usize,
    problems: Vec<String>,
}

impl Walk {
    fn problem(&mut self, at: &str, what: impl std::fmt::Display) {
        if self.problems.len() < PROBLEMS_SHOWN {
            self.problems.push(format!("{at}: {what}"));
        }
    }

    fn action(&mut self, at: &str, action: Option<&Value>) {
        match action.and_then(Value::as_str) {
            Some(a) if valid_name(a) => {}
            Some(a) => self.problem(at, format!("action {a:?} is not a name")),
            None => {}
        }
    }

    fn node(&mut self, v: &Value, at: &str, depth: usize) {
        self.nodes += 1;
        if self.nodes > NODES_MAX {
            if self.nodes == NODES_MAX + 1 {
                self.problem(at, format!("more than {NODES_MAX} nodes"));
            }
            return;
        }
        if depth > DEPTH_MAX {
            self.problem(at, format!("nested deeper than {DEPTH_MAX}"));
            return;
        }
        let Some(fields) = v.as_object() else {
            self.problem(at, "a node is an object");
            return;
        };
        let Some(kind) = fields.get("type").and_then(Value::as_str) else {
            self.problem(at, "a node has a `type`");
            return;
        };
        if let Some(props) = required(kind) {
            for (prop, want) in props {
                let ok = match (fields.get(*prop), want) {
                    (Some(Value::String(s)), Kind::Str) => !s.trim().is_empty(),
                    (Some(Value::Array(_)), Kind::Array) => true,
                    _ => false,
                };
                if !ok {
                    let what = match want {
                        Kind::Str => "text",
                        Kind::Array => "a list",
                    };
                    self.problem(
                        &format!("{at}.{prop}"),
                        format!("a {kind} needs {prop} ({what})"),
                    );
                }
            }
        }
        match kind {
            "button" | "form" => self.action(&format!("{at}.action"), fields.get("action")),
            "toggle" | "select" | "segmented" => {
                self.action(&format!("{at}.action"), fields.get("action"))
            }
            "list" | "table" => {
                let rows = if kind == "list" { "items" } else { "rows" };
                if fields
                    .get(rows)
                    .and_then(Value::as_array)
                    .is_some_and(|r| r.len() > ROWS_MAX)
                {
                    self.problem(
                        &format!("{at}.{rows}"),
                        format!("at most {ROWS_MAX} rows; more page through `more`"),
                    );
                }
            }
            _ => {}
        }
        if let Some(children) = fields.get("children") {
            match children.as_array() {
                Some(list) => {
                    for (i, c) in list.iter().enumerate() {
                        self.node(c, &format!("{at}.children[{i}]"), depth + 1);
                    }
                }
                None => self.problem(&format!("{at}.children"), "children is a list of nodes"),
            }
        }
        if kind == "tabs" {
            for (i, tab) in fields
                .get("tabs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let tat = format!("{at}.tabs[{i}]");
                if tab
                    .get("title")
                    .and_then(Value::as_str)
                    .is_none_or(|t| t.trim().is_empty())
                {
                    self.problem(&tat, "a tab has a title");
                }
                for (j, c) in tab
                    .get("children")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    self.node(c, &format!("{tat}.children[{j}]"), depth + 1);
                }
            }
        }
        if kind == "list" {
            for (i, item) in fields
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let iat = format!("{at}.items[{i}]");
                if item.get("title").and_then(Value::as_str).is_none() {
                    self.problem(&iat, "a list item has a title");
                }
                for (j, a) in item
                    .get("actions")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    self.node(a, &format!("{iat}.actions[{j}]"), depth + 1);
                }
            }
        }
        // A callout's own buttons (a notice with its fix beside it).
        if kind == "callout" {
            for (j, a) in fields
                .get("actions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                self.node(a, &format!("{at}.actions[{j}]"), depth + 1);
            }
        }
        if let Some(fallback) = fields.get("fallback") {
            if fallback.as_str() != Some("drop") {
                self.node(fallback, &format!("{at}.fallback"), depth + 1);
            }
        }
    }
}

/// `text`, a plugin's tree: checked, and parsed. The error names what to
/// fix (each problem with its JSON path).
pub(crate) fn check_tree(text: &str) -> Result<Value, Vec<String>> {
    if text.len() > TREE_MAX {
        return Err(vec![format!(
            "the tree is {} KiB, over the {} KiB a screen may be",
            text.len() >> 10,
            TREE_MAX >> 10
        )]);
    }
    let tree: Value = serde_json::from_str(text).map_err(|e| vec![format!("not JSON ({e})")])?;
    let Some(ui) = tree.get("ui").and_then(Value::as_str) else {
        return Err(vec!["a screen is {\"ui\": \"1\", \"root\": <node>}".into()]);
    };
    if ui.split('.').next() != Some("1") {
        return Err(vec![format!(
            "ui {ui:?}: this chimaera draws version 1 screens"
        )]);
    }
    let mut walk = Walk {
        nodes: 0,
        problems: Vec::new(),
    };
    match tree.get("root") {
        Some(root) => walk.node(root, "root", 0),
        None => walk.problem("root", "a screen has a root node"),
    }
    if walk.problems.is_empty() {
        Ok(tree)
    } else {
        Err(walk.problems)
    }
}

/// Invalidation and view activity (on `Platform`).
#[derive(Default)]
pub(crate) struct Screens {
    /// (plugin, workspace, view) → when its last `view` frame went, and
    /// whether one is waiting.
    sent: HashMap<(String, String, String), (Instant, bool)>,
    /// (plugin, workspace) → when one of its views last rendered there,
    /// and the files its file views showed (with when).
    open: HashMap<(String, String), (Instant, HashMap<String, Instant>)>,
}

impl Screens {
    fn mark_open(&mut self, plugin: &str, ws: &str, file: Option<&str>) {
        let now = Instant::now();
        let entry = self
            .open
            .entry((plugin.to_string(), ws.to_string()))
            .or_insert_with(|| (now, HashMap::new()));
        entry.0 = now;
        if let Some(file) = file {
            entry.1.insert(file.to_string(), now);
        }
        // Bounded: what went quiet goes.
        entry.1.retain(|_, at| at.elapsed() < OPEN_FOR);
        if entry.1.len() > 64 {
            let oldest = entry
                .1
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(f, _)| f.clone());
            if let Some(oldest) = oldest {
                entry.1.remove(&oldest);
            }
        }
        self.open.retain(|_, (at, _)| at.elapsed() < OPEN_FOR);
    }

    /// The (plugin, workspace) pairs with a view open, and the files their
    /// file views show.
    pub(crate) fn open_now(&mut self) -> Vec<((String, String), Vec<String>)> {
        self.open.retain(|_, (at, _)| at.elapsed() < OPEN_FOR);
        self.open
            .iter()
            .map(|(k, (_, files))| (k.clone(), files.keys().cloned().collect()))
            .collect()
    }

    pub(crate) fn forget_plugin(&mut self, plugin: &str) {
        self.sent.retain(|(p, _, _), _| p != plugin);
        self.open.retain(|(p, _), _| p != plugin);
    }

    pub(crate) fn forget_workspace(&mut self, ws: &str) {
        self.sent.retain(|(_, w, _), _| w != ws);
        self.open.retain(|(_, w), _| w != ws);
    }
}

fn view_frame(plugin: &str, ws: &str, view: &str) -> String {
    json!({"type": "view", "plugin": plugin, "workspace": ws, "view": view}).to_string()
}

/// `invalidate(view)`: windows showing it render it again, at most
/// `RENDERS_PER_SECOND` times a second (a burst ends in one frame).
pub(crate) fn invalidate(state: &Arc<AppState>, m: &Manifest, ws: &str, view: &str) {
    if !m.views.iter().any(|v| v.id == view) {
        return;
    }
    let gap = Duration::from_millis(1000 / u64::from(RENDERS_PER_SECOND));
    let key = (m.id.clone(), ws.to_string(), view.to_string());
    let wait = {
        let mut screens = crate::lock(&state.plugin_platform.screens);
        let entry = screens
            .sent
            .entry(key.clone())
            .or_insert((Instant::now() - gap, false));
        if entry.1 {
            return;
        }
        let since = entry.0.elapsed();
        if since >= gap {
            entry.0 = Instant::now();
            None
        } else {
            entry.1 = true;
            Some(gap - since)
        }
    };
    let Some(wait) = wait else {
        state
            .plugin_runtime
            .push_event(ws, view_frame(&m.id, ws, view));
        state.changes.notify_waiters();
        return;
    };
    let state = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(wait).await;
        if let Some(entry) = crate::lock(&state.plugin_platform.screens)
            .sent
            .get_mut(&key)
        {
            *entry = (Instant::now(), false);
        }
        state
            .plugin_runtime
            .push_event(&key.1, view_frame(&key.0, &key.1, &key.2));
        state.changes.notify_waiters();
    });
}

/// The plugin, active in `ws`, or the response saying why not.
async fn active_plugin(state: &AppState, ws: &str, pid: &str) -> Result<Arc<Manifest>, Response> {
    if crate::lock(&state.workspaces).get(ws).is_none() {
        return Err(super::not_found(&format!("workspace {ws}")));
    }
    let Some(m) = super::manifest(state, pid) else {
        return Err(super::not_found(&format!("plugin {pid}")));
    };
    if !super::active(state, ws).await.iter().any(|a| a.id == pid) {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error": format!("{} is not on in this workspace", m.name)})),
        )
            .into_response());
    }
    Ok(m)
}

/// The window's answer when a plugin's screen can't be drawn.
fn cannot_draw(m: &Manifest, view: &str, why: Vec<String>) -> Response {
    tracing::warn!(plugin = %m.id, %view, problems = ?why, "plugin screen refused");
    Json(json!({
        "error": format!("{} sent a screen chimaera could not draw", m.name),
        "problems": why,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct RenderQuery {
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    width: Option<String>,
}

/// `GET /workspaces/{id}/plugins/{pid}/views/{view}?file=&width=`: the
/// view's tree, checked — `{"view", "title", "slot", "tree"}`, or
/// `{"error", "problems"}` when the plugin failed or sent a bad tree.
pub(crate) async fn render_route(
    State(state): State<Arc<AppState>>,
    AxPath((ws, pid, view)): AxPath<(String, String, String)>,
    Query(q): Query<RenderQuery>,
) -> Response {
    let m = match active_plugin(&state, &ws, &pid).await {
        Ok(m) => m,
        Err(r) => return r,
    };
    let Some(decl) = m.views.iter().find(|v| v.id == view) else {
        return super::not_found(&format!("view {view} of {}", m.name));
    };
    // The file a view shows is the plugin's to read and the sweep's to
    // stat: a workspace path, never one that climbs out.
    if let Some(file) = q.file.as_deref() {
        if file.is_empty() || super::hostfns_relative(file).is_err() {
            return super::bad_request(format!("{file:?} is not a path in this workspace"));
        }
    }
    if decl.slot == Slot::File {
        let Some(file) = q.file.as_deref() else {
            return super::bad_request("a file view needs ?file=");
        };
        if super::platform::file_kind(&m, file).is_none_or(|k| k.view != view) {
            return super::bad_request(format!("{} does not open {file} in {view}", m.name));
        }
    }
    let width = match q.width.as_deref() {
        Some("narrow") => "narrow",
        _ => "wide",
    };
    let args = json!({"file": q.file, "width": width, "slot": decl.slot.as_str()});
    crate::lock(&state.plugin_platform.screens).mark_open(&m.id, &ws, q.file.as_deref());
    // The watch sweep runs while a view is open: start it if it sleeps.
    state.plugin_platform.files_wake.notify_one();
    let text = match state
        .plugin_runtime
        .render(&state, &m, &ws, &view, &args.to_string())
        .await
    {
        Ok(text) => text,
        Err(err) => return cannot_draw(&m, &view, vec![err]),
    };
    match check_tree(&text) {
        Ok(tree) => Json(json!({
            "view": view,
            "title": decl.title,
            "slot": decl.slot.as_str(),
            "tree": tree,
        }))
        .into_response(),
        Err(problems) => cannot_draw(&m, &view, problems),
    }
}

#[derive(Deserialize)]
pub(crate) struct ActionBody {
    action: String,
    #[serde(default)]
    payload: Value,
    /// A form's fields, by name.
    #[serde(default)]
    form: Option<Map<String, Value>>,
    /// A toggle's or select's new value.
    #[serde(default)]
    value: Option<Value>,
}

/// What `on-action` receives: the node's payload as is, or — with a
/// form's fields or an input's value — an object carrying them (`form`,
/// `value`) beside the payload's own keys (a payload that isn't an object
/// rides as `payload`).
fn action_payload(body: &ActionBody) -> Value {
    if body.form.is_none() && body.value.is_none() {
        return body.payload.clone();
    }
    let mut out = match &body.payload {
        Value::Object(fields) => fields.clone(),
        Value::Null => Map::new(),
        other => {
            let mut m = Map::new();
            m.insert("payload".into(), other.clone());
            m
        }
    };
    if let Some(form) = &body.form {
        out.insert("form".into(), Value::Object(form.clone()));
    }
    if let Some(value) = &body.value {
        out.insert("value".into(), value.clone());
    }
    Value::Object(out)
}

/// `POST /workspaces/{id}/plugins/{pid}/views/{view}/actions {action,
/// payload, form?, value?}`: the plugin's `on-action`; its new tree
/// (checked), or `null` to keep what the view shows.
pub(crate) async fn action_route(
    State(state): State<Arc<AppState>>,
    AxPath((ws, pid, view)): AxPath<(String, String, String)>,
    Json(body): Json<ActionBody>,
) -> Response {
    let m = match active_plugin(&state, &ws, &pid).await {
        Ok(m) => m,
        Err(r) => return r,
    };
    if !m.views.iter().any(|v| v.id == view) {
        return super::not_found(&format!("view {view} of {}", m.name));
    }
    if !valid_name(&body.action) || BUILTIN_ACTIONS.contains(&body.action.as_str()) {
        return super::bad_request(format!("{:?} is not the plugin's action", body.action));
    }
    let payload = action_payload(&body);
    if payload.to_string().len() > 64 << 10 {
        return super::bad_request("an action's payload is at most 64 KiB");
    }
    let text = match state
        .plugin_runtime
        .on_action(&state, &m, &ws, &view, &body.action, &payload.to_string())
        .await
    {
        Ok(text) => text,
        Err(err) => return cannot_draw(&m, &view, vec![err]),
    };
    if text.trim() == "null" {
        return Json(json!({"tree": null})).into_response();
    }
    match check_tree(&text) {
        Ok(tree) => Json(json!({"tree": tree})).into_response(),
        Err(problems) => cannot_draw(&m, &view, problems),
    }
}

#[derive(Deserialize)]
pub(crate) struct FileActionBody {
    file: String,
}

/// `POST /workspaces/{id}/plugins/{pid}/file-actions/{action} {file}`: a
/// file menu item (`[[actions]]`); the plugin's `on-action("", action,
/// {"file"})`. Its answer: `{"message"?, "open"?: {"view", "file"?}}`.
pub(crate) async fn file_action_route(
    State(state): State<Arc<AppState>>,
    AxPath((ws, pid, action)): AxPath<(String, String, String)>,
    Json(body): Json<FileActionBody>,
) -> Response {
    let m = match active_plugin(&state, &ws, &pid).await {
        Ok(m) => m,
        Err(r) => return r,
    };
    let Some(decl) = m.actions.iter().find(|a| a.action == action) else {
        return super::not_found(&format!("action {action} of {}", m.name));
    };
    if !decl
        .patterns
        .iter()
        .any(|p| super::platform::matches(p, &body.file))
    {
        return super::bad_request(format!("{} is not for {}", decl.label, body.file));
    }
    let payload = json!({"file": body.file});
    let text = match state
        .plugin_runtime
        .on_action(&state, &m, &ws, "", &action, &payload.to_string())
        .await
    {
        Ok(text) => text,
        Err(err) => return super::bad_request(err),
    };
    let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let message = answer
        .get("message")
        .and_then(Value::as_str)
        .map(|m| crate::timeline::cap(m, 400));
    let open = answer.get("open").filter(|o| {
        o.get("view")
            .and_then(Value::as_str)
            .is_some_and(|v| m.views.iter().any(|d| d.id == v))
    });
    Json(json!({"message": message, "open": open})).into_response()
}

#[derive(Deserialize)]
pub(crate) struct QueryArgs {
    #[serde(default)]
    args: Option<String>,
}

/// `GET /workspaces/{id}/plugins/{pid}/query/{name}?args=<json>`: a read
/// the UI makes (a long list's next page): `{"data": …}`.
pub(crate) async fn query_route(
    State(state): State<Arc<AppState>>,
    AxPath((ws, pid, name)): AxPath<(String, String, String)>,
    Query(q): Query<QueryArgs>,
) -> Response {
    let m = match active_plugin(&state, &ws, &pid).await {
        Ok(m) => m,
        Err(r) => return r,
    };
    if !valid_name(&name) {
        return super::bad_request(format!("{name:?} is not a query name"));
    }
    let args = q.args.unwrap_or_else(|| "{}".into());
    if args.len() > 16 << 10 || serde_json::from_str::<Value>(&args).is_err() {
        return super::bad_request("args is JSON of at most 16 KiB");
    }
    match state
        .plugin_runtime
        .query(&state, &m, &ws, &name, &args)
        .await
    {
        Ok(text) if text.len() > QUERY_MAX => {
            super::bad_request(format!("{}'s answer is over 1 MiB", m.name))
        }
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(data) => Json(json!({"data": data})).into_response(),
            Err(e) => super::bad_request(format!("{}'s answer is not JSON ({e})", m.name)),
        },
        Err(err) => super::bad_request(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trees_are_checked_with_paths() {
        let good = json!({"ui": "1", "root": {"type": "stack", "children": [
            {"type": "heading", "text": "Build"},
            {"type": "button", "label": "Build", "action": "build"},
            {"type": "shiny-new-node", "fallback": {"type": "text", "text": "old"}},
            {"type": "tabs", "tabs": [{"title": "Log", "children": [{"type": "log", "src": "output:build.log"}]}]},
            {"type": "status", "state": "busy", "text": "Building"},
            {"type": "segmented", "name": "layout", "value": "split", "action": "layout",
             "options": [{"value": "split", "label": "Split"}, {"value": "pdf", "label": "PDF"}]},
            {"type": "callout", "text": "siunitx is missing", "actions": [
                {"type": "button", "label": "Install", "action": "install-package"}]},
        ]}});
        check_tree(&good.to_string()).unwrap();
        // A callout's buttons are checked like any other.
        let bad_action = json!({"ui": "1", "root": {"type": "callout", "text": "x", "actions": [
            {"type": "button", "label": "Go"}]}});
        let problems = check_tree(&bad_action.to_string()).unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("root.actions[0].action")),
            "{problems:?}"
        );

        let bad = json!({"ui": "1", "root": {"type": "stack", "children": [
            {"type": "button", "action": "go"},
            {"type": "image", "src": "output:a.png"},
            "not a node",
            {"type": "button", "label": "x", "action": "no spaces"},
        ]}});
        let problems = check_tree(&bad.to_string()).unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("root.children[0].label")),
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("root.children[1].alt")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.starts_with("root.children[2]")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("not a name")),
            "{problems:?}"
        );

        assert!(check_tree(r#"{"ui": "2", "root": {"type": "text", "text": "x"}}"#).is_err());
        let many: Vec<Value> = (0..NODES_MAX + 1)
            .map(|_| json!({"type": "divider"}))
            .collect();
        let huge = json!({"ui": "1", "root": {"type": "stack", "children": many}});
        assert!(check_tree(&huge.to_string()).is_err());
        let rows: Vec<Value> = (0..ROWS_MAX + 1)
            .map(|i| json!({"title": i.to_string()}))
            .collect();
        let long = json!({"ui": "1", "root": {"type": "list", "items": rows}});
        assert!(check_tree(&long.to_string()).is_err());
    }

    #[test]
    fn payloads_carry_form_values() {
        let body = |payload: Value, form: Option<Value>, value: Option<Value>| ActionBody {
            action: "a".into(),
            payload,
            form: form.map(|f| f.as_object().unwrap().clone()),
            value,
        };
        assert_eq!(
            action_payload(&body(json!({"k": 1}), None, None)),
            json!({"k": 1})
        );
        assert_eq!(
            action_payload(&body(json!({"k": 1}), Some(json!({"name": "x"})), None)),
            json!({"k": 1, "form": {"name": "x"}})
        );
        assert_eq!(
            action_payload(&body(json!(3), None, Some(json!(true)))),
            json!({"payload": 3, "value": true})
        );
    }
}
