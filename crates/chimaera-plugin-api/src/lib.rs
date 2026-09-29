//! The Rust side of the `chimaera:plugin` WIT world (`wit/chimaera.wit`,
//! package `chimaera:plugin@0.2.0`): what a Chimaera plugin implements
//! ([`Plugin`]), what it may ask the host ([`host`], and the platform's
//! [`platform`]). Design: `docs/plugin-system-plan.md` (the host) and
//! `docs/plugin-platform-plan.md` (screens, surfaces, files, settings,
//! output folders, programs). A 0.1 plugin moves to 0.2 by bumping this
//! dependency and its manifest's `api`: every new export has a default.
//!
//! A plugin is a `cdylib` crate built for `wasm32-wasip2`:
//!
//! ```ignore
//! use chimaera_plugin_api::{Context, Plugin, ToolDef, ToolResult};
//!
//! struct Hello;
//!
//! impl Plugin for Hello {
//!     fn tools() -> Vec<ToolDef> {
//!         vec![ToolDef::new("hello", "Say hello.", serde_json::json!({"type": "object"}))]
//!     }
//!     fn call_tool(_cx: Context, name: &str, _args: serde_json::Value) -> ToolResult {
//!         match name {
//!             "hello" => ToolResult::text("hello"),
//!             other => ToolResult::error(format!("unknown tool {other}")),
//!         }
//!     }
//! }
//!
//! chimaera_plugin_api::export!(Hello);
//! ```
//!
//! Built natively (tests, clippy) every host import is a stub that aborts the
//! process, so a native test must never reach a [`host`] or [`platform`]
//! call: keep pure logic in functions that take data.

// The generated guest bindings. `pub_export_macro` + `with_types_in $crate`
// (see `export!`) let a plugin crate export through this crate's bindings.
wit_bindgen::generate!({
    world: "chimaera-plugin",
    path: "wit",
    pub_export_macro: true,
    export_macro_name: "__export_chimaera_plugin",
    default_bindings_module: "chimaera_plugin_api",
});

pub use serde;
pub use serde_json;
use serde_json::Value;

pub use chimaera::plugin::types::{
    Context, Entry, Event, Hook, JobEnd, Level, Session, Snapshot, Stat, ToolDef, ToolResult,
};

use exports::chimaera::plugin::plugin::Guest;
use exports::chimaera::plugin::screens::Guest as ScreensGuest;

/// What a plugin provides. Every function has a default, so a plugin
/// implements only what it offers; the set is the WIT world's complete
/// export list (adding one is a breaking WIT change).
pub trait Plugin {
    /// The MCP tools offered where the plugin is on. The names must equal
    /// the manifest's `provides.mcp_tools` (the host refuses the plugin
    /// otherwise).
    fn tools() -> Vec<ToolDef> {
        Vec::new()
    }

    /// The paragraph appended to the agents' MCP instructions where on.
    fn instructions() -> Option<String> {
        None
    }

    /// One tool call. `args` is the call's JSON arguments. A tool that
    /// started a job answers [`ToolResult::wait`]; the host then calls
    /// [`Plugin::tool_resume`] once the job ended.
    fn call_tool(cx: Context, name: &str, args: Value) -> ToolResult {
        let _ = (cx, args);
        ToolResult::error(format!("unknown tool {name}"))
    }

    /// The Knowledge snapshot, or `Ok(None)` when `known` (the stamp the host
    /// holds) is still current.
    fn knowledge(cx: Context, known: Option<Value>) -> Result<Option<Snapshot>, String> {
        let _ = (cx, known);
        Ok(None)
    }

    /// A read the UI makes (and a long list's next page).
    fn query(cx: Context, name: &str, args: Value) -> Result<Value, String> {
        let _ = (cx, name, args);
        Err("no such query".to_string())
    }

    /// Something happened. For a `hook` event, one returned line joins the
    /// agent's hook context.
    fn on_event(cx: Context, event: Event) -> Option<String> {
        let _ = (cx, event);
        None
    }

    /// A view's tree in the Chimaera format: `{"ui": "1", "root": <node>}`
    /// (see [`ui`]). `args`: `{"file": <path>}` for a file view, `"width"`
    /// (`narrow` or `wide`).
    fn render(cx: Context, view: &str, args: Value) -> Result<Value, String> {
        let _ = (cx, args);
        Err(format!("no such view {view}"))
    }

    /// A node's action (a button, a form's submit, a file action): the
    /// view's new tree, or `None` to keep what it shows.
    fn on_action(
        cx: Context,
        view: &str,
        action: &str,
        payload: Value,
    ) -> Result<Option<Value>, String> {
        let _ = (cx, view, payload);
        Err(format!("no such action {action}"))
    }

    /// The final answer of a tool that answered [`ToolResult::wait`], once
    /// `job` ended.
    fn tool_resume(cx: Context, name: &str, job: &str) -> ToolResult {
        let _ = (cx, job);
        ToolResult::error(format!("{name} has no answer to resume"))
    }
}

/// Wire a type implementing [`Plugin`] to the component's exports. Invoke
/// once, at the crate root of a plugin: `chimaera_plugin_api::export!(MyPlugin);`
#[macro_export]
macro_rules! export {
    ($ty:ident) => {
        $crate::__export_chimaera_plugin!($ty with_types_in $crate);
    };
}

/// The JSON-text exports are parsed here, once, so a plugin sees values.
impl<T: Plugin> Guest for T {
    fn tools() -> Vec<ToolDef> {
        T::tools()
    }

    fn instructions() -> Option<String> {
        T::instructions()
    }

    fn call_tool(cx: Context, name: String, args: String) -> ToolResult {
        match serde_json::from_str(&args) {
            Ok(args) => T::call_tool(cx, &name, args),
            Err(err) => ToolResult::error(format!("{name}: the arguments are not JSON ({err})")),
        }
    }

    fn knowledge(cx: Context, known: Option<String>) -> Result<Option<Snapshot>, String> {
        // A stamp that no longer parses is simply not current.
        let known = known.and_then(|k| serde_json::from_str(&k).ok());
        T::knowledge(cx, known)
    }

    fn query(cx: Context, name: String, args: String) -> Result<String, String> {
        let args = serde_json::from_str(&args).map_err(|e| format!("arguments: {e}"))?;
        T::query(cx, &name, args).map(|v| v.to_string())
    }

    fn on_event(cx: Context, event: Event) -> Option<String> {
        T::on_event(cx, event)
    }
}

impl<T: Plugin> ScreensGuest for T {
    fn render(cx: Context, view: String, args: String) -> Result<String, String> {
        let args = serde_json::from_str(&args).unwrap_or(Value::Null);
        T::render(cx, &view, args).map(|v| v.to_string())
    }

    fn on_action(
        cx: Context,
        view: String,
        action: String,
        payload: String,
    ) -> Result<String, String> {
        let payload = serde_json::from_str(&payload).unwrap_or(Value::Null);
        T::on_action(cx, &view, &action, payload).map(|v| v.unwrap_or(Value::Null).to_string())
    }

    fn tool_resume(cx: Context, name: String, job: String) -> ToolResult {
        T::tool_resume(cx, &name, &job)
    }
}

impl ToolDef {
    /// A tool definition; `input_schema` is the tool's JSON Schema.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        ToolDef {
            name: name.into(),
            description: description.into(),
            input_schema: input_schema.to_string(),
        }
    }
}

impl ToolResult {
    /// A successful result the model reads as text.
    pub fn text(text: impl Into<String>) -> Self {
        ToolResult {
            text: text.into(),
            is_error: false,
            wait: None,
        }
    }

    /// A failed call; the model reads the text.
    pub fn error(text: impl Into<String>) -> Self {
        ToolResult {
            text: text.into(),
            is_error: true,
            wait: None,
        }
    }

    /// The answer waits for `job` (one [`platform::job_start`] returned):
    /// the host holds the agent's call until the job ends, then asks
    /// [`Plugin::tool_resume`]. `text` is what the agent reads if the job
    /// outlasts the hold ("still building").
    pub fn wait(job: impl Into<String>, text: impl Into<String>) -> Self {
        ToolResult {
            text: text.into(),
            is_error: false,
            wait: Some(job.into()),
        }
    }
}

impl Snapshot {
    /// A Knowledge snapshot: `stamp` changes when the source files do;
    /// `data` is the fixed shape the Knowledge view reads.
    pub fn new(stamp: &Value, data: &Value) -> Self {
        Snapshot {
            stamp: stamp.to_string(),
            data: data.to_string(),
        }
    }
}

/// What a plugin may ask the host (as in 0.1). Every call is bounded by the
/// host; fallible calls return the host's reason as text. The manifest's
/// `[access]` decides which reads answer.
///
/// The host serves each call for the workspace and session it made the
/// call for; the `cx` argument is the one the plugin was handed.
pub mod host {
    use super::chimaera::plugin::host as raw;
    use super::{Context, Entry, Level, Session, Stat};
    use serde_json::Value;

    /// A workspace file's bytes, at most `cap` (the host clamps to 8 MiB).
    /// `path` is workspace-relative; no component may be a symlink.
    pub fn read(cx: &Context, path: &str, cap: u32) -> Result<Vec<u8>, String> {
        raw::read(cx, path, cap)
    }

    /// Size, mtime and kind of a workspace path (never through a symlink).
    pub fn stat(cx: &Context, path: &str) -> Result<Stat, String> {
        raw::stat(cx, path)
    }

    /// A workspace directory's entries, at most `cap` (clamped to 4,096).
    pub fn list(cx: &Context, path: &str, cap: u32) -> Result<Vec<Entry>, String> {
        raw::list(cx, path, cap)
    }

    /// A value this plugin stored in this workspace, if any (in memory or
    /// kept durably with [`super::platform::state_keep`]).
    pub fn state_get(cx: &Context, key: &str) -> Result<Option<Value>, String> {
        raw::state_get(cx, key)
            .map(|text| serde_json::from_str(&text).map_err(|e| format!("state {key}: {e}")))
            .transpose()
    }

    /// Store a value for this plugin in this workspace (64 KiB per plugin per
    /// workspace, in memory, gone on a daemon restart). `null` removes it.
    pub fn state_put(cx: &Context, key: &str, value: &Value) -> Result<(), String> {
        raw::state_put(cx, key, &value.to_string())
    }

    /// The workspace's sessions.
    pub fn sessions(cx: &Context) -> Vec<Session> {
        raw::sessions(cx)
    }

    /// Append an entry to the workspace Timeline; the new entry's seq. The
    /// host accepts `{"kind":"note","to":…,"text":…}` from a session's call
    /// and rate-caps appends per session.
    pub fn timeline_append(cx: &Context, entry: &Value) -> Result<u64, String> {
        raw::timeline_append(cx, &entry.to_string())
    }

    /// The newest Timeline entries of `kinds`, newest first, at most `limit`.
    pub fn timeline_recent(cx: &Context, kinds: &[&str], limit: u32) -> Result<Vec<Value>, String> {
        let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
        raw::timeline_recent(cx, &kinds, limit)
            .iter()
            .map(|text| serde_json::from_str(text).map_err(|e| format!("timeline entry: {e}")))
            .collect()
    }

    /// A frame on the UI's event bus for windows showing this workspace:
    /// `{"type":"plugin","plugin":<id>,"workspace":<id>, ...event}`. `event`
    /// must be a JSON object.
    pub fn emit(cx: &Context, event: &Value) {
        raw::emit(cx, &event.to_string())
    }

    /// Wall-clock milliseconds since the Unix epoch.
    pub fn now_ms() -> u64 {
        raw::now_ms()
    }

    /// A line in the daemon's log, tagged with the plugin.
    pub fn log(level: Level, message: &str) {
        raw::log(level, message)
    }
}

/// What 0.2 adds for a plugin to ask: its output folder, data surfaces,
/// screens' invalidation, the watch set, declared settings, durable state,
/// the roots, and programs run as jobs. Bounded by the host like [`host`].
pub mod platform {
    use super::chimaera::plugin::platform as raw;
    use super::{Context, Entry};
    use serde_json::Value;

    /// Bytes of a file in the plugin's output folder for this workspace,
    /// from `offset`, at most `cap` (≤ 8 MiB a call).
    pub fn output_read(cx: &Context, path: &str, offset: u64, cap: u32) -> Result<Vec<u8>, String> {
        raw::output_read(cx, path, offset, cap)
    }

    /// A folder of the output folder, at most `cap` entries.
    pub fn output_list(cx: &Context, path: &str, cap: u32) -> Result<Vec<Entry>, String> {
        raw::output_list(cx, path, cap)
    }

    /// Write a file into the output folder (≤ 8 MiB; folders on its path
    /// are made).
    pub fn output_write(cx: &Context, path: &str, bytes: &[u8]) -> Result<(), String> {
        raw::output_write(cx, path, bytes)
    }

    /// Remove a file or folder of the output folder.
    pub fn output_remove(cx: &Context, path: &str) -> Result<(), String> {
        raw::output_remove(cx, path)
    }

    /// Publish a data surface (`diagnostics/1`, `output/1`, `sourcemap/1`,
    /// `knowledge/1`) under `key`.
    pub fn publish(cx: &Context, surface: &str, key: &str, data: &Value) -> Result<(), String> {
        raw::publish(cx, surface, key, &data.to_string())
    }

    /// Remove a published surface.
    pub fn unpublish(cx: &Context, surface: &str, key: &str) -> Result<(), String> {
        raw::publish(cx, surface, key, "null")
    }

    /// Tell windows showing `view` to render it again.
    pub fn invalidate(cx: &Context, view: &str) {
        raw::invalidate(cx, view)
    }

    /// The workspace paths (≤ 256) whose changes arrive as `file-changed`;
    /// replaces the previous set.
    pub fn watch(cx: &Context, paths: &[String]) -> Result<(), String> {
        raw::watch(cx, paths)
    }

    /// A declared setting's value: the user's, else its default.
    pub fn setting(cx: &Context, key: &str) -> Option<Value> {
        raw::setting_get(cx, key).and_then(|text| serde_json::from_str(&text).ok())
    }

    /// Store `value` durably under `key` (survives a restart); `null`
    /// removes it. Read it back with [`super::host::state_get`].
    pub fn state_keep(cx: &Context, key: &str, value: &Value) -> Result<(), String> {
        raw::state_keep(cx, key, &value.to_string())
    }

    /// The absolute workspace root and output folder.
    pub struct Roots {
        pub workspace: String,
        pub output: String,
    }

    pub fn roots(cx: &Context) -> Roots {
        let v: Value = serde_json::from_str(&raw::roots(cx)).unwrap_or(Value::Null);
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        Roots {
            workspace: s("workspace"),
            output: s("output"),
        }
    }

    /// Run a declared program as a job: `spec` is
    /// `{"program", "args", "cwd"?, "env"?, "stdin"?, "wall_s"?, "label"?, "priority"?}`.
    /// The job's id; `job-finished` arrives when it ends.
    pub fn job_start(cx: &Context, spec: &Value) -> Result<String, String> {
        raw::job_start(cx, &spec.to_string())
    }

    /// `{"state": "queued"|"running"|"done", "exit"?, "timed_out"?, ...}`.
    pub fn job_status(cx: &Context, id: &str) -> Value {
        serde_json::from_str(&raw::job_status(cx, id)).unwrap_or(Value::Null)
    }

    pub fn job_cancel(cx: &Context, id: &str) {
        raw::job_cancel(cx, id)
    }

    /// A declared side program: `{"installed", "version", "found_on_path", ...}`.
    pub fn tool_state(cx: &Context, tool: &str) -> Value {
        serde_json::from_str(&raw::tool_state(cx, tool)).unwrap_or(Value::Null)
    }
}

/// Screens in the Chimaera format (`ui/1`): small helpers for the JSON tree
/// a view returns. Every node is `{"type": …, props…}`; see
/// `docs/agent-guides/plugins.md` ("Screens") for every node and prop.
pub mod ui {
    use serde_json::{json, Value};

    /// A view's tree.
    pub fn tree(root: Value) -> Value {
        json!({"ui": "1", "root": root})
    }

    pub fn stack(children: Vec<Value>) -> Value {
        json!({"type": "stack", "children": children})
    }

    pub fn row(children: Vec<Value>) -> Value {
        json!({"type": "row", "children": children})
    }

    pub fn section(title: &str, children: Vec<Value>) -> Value {
        json!({"type": "section", "title": title, "children": children})
    }

    pub fn text(text: &str) -> Value {
        json!({"type": "text", "text": text})
    }

    pub fn heading(text: &str) -> Value {
        json!({"type": "heading", "text": text})
    }

    /// A button whose click reaches `on_action(view, action, payload)`.
    pub fn button(label: &str, action: &str, payload: Value) -> Value {
        json!({"type": "button", "label": label, "action": action, "payload": payload})
    }

    pub fn callout(text: &str, tone: &str) -> Value {
        json!({"type": "callout", "text": text, "tone": tone})
    }
}
