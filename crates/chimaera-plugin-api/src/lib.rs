//! The Rust side of the `chimaera:plugin` WIT world (`wit/chimaera.wit`):
//! what a Chimaera plugin implements ([`Plugin`]) and what it may ask the
//! host ([`host`]). Design: `docs/plugin-system-plan.md`.
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
//! process, so a native test must never reach a [`host`] call: keep pure
//! logic in functions that take data.

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
    Context, Entry, Event, Hook, Level, Session, Snapshot, Stat, ToolDef, ToolResult,
};

use exports::chimaera::plugin::plugin::Guest;

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

    /// One tool call. `args` is the call's JSON arguments.
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

    /// A read the UI makes.
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
        }
    }

    /// A failed call; the model reads the text.
    pub fn error(text: impl Into<String>) -> Self {
        ToolResult {
            text: text.into(),
            is_error: true,
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

/// What a plugin may ask the host. Every call is bounded by the host (the
/// limits are in the plan); fallible calls return the host's reason as text.
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

    /// A value this plugin stored in this workspace, if any.
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

    /// A frame on the UI's event bus: `{"type":"plugin","plugin":<id>,
    /// "workspace":<id>, ...event}`. `event` must be a JSON object.
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
