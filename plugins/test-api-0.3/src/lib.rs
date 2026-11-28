//! The frozen 0.3 world's fixture: a tool, a view and two host calls, built
//! against plugins/api-0.3 (the SDK as it was at 0.3.0), so the daemon's
//! tests run a 0.3 component through its own bindings. Never shipped.

use chimaera_plugin_api::serde_json::{json, Value};
use chimaera_plugin_api::{host, ui, Context, Plugin, ToolDef, ToolResult};

struct Fixture;

impl Plugin for Fixture {
    fn tools() -> Vec<ToolDef> {
        vec![ToolDef::new(
            "echo03",
            "Echo `text`, with the bytes of `path` from `offset` (host.read-at), or answer \
             `wait` on a job that doesn't exist (`wait`).",
            json!({"type": "object", "properties": {
                "text": {"type": "string"}, "path": {"type": "string"},
                "offset": {"type": "number"}, "wait": {"type": "boolean"}
            }}),
        )]
    }

    fn call_tool(cx: Context, name: &str, args: Value) -> ToolResult {
        if name != "echo03" {
            return ToolResult::error(format!("unknown tool {name}"));
        }
        if args.get("wait").and_then(Value::as_bool) == Some(true) {
            return ToolResult::wait("j-none", "still waiting");
        }
        let text = args.get("text").and_then(Value::as_str).unwrap_or("");
        let _ = host::state_put(&cx, "last", &json!(text));
        let read = match args.get("path").and_then(Value::as_str) {
            Some(path) => {
                let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
                match host::read_at(&cx, path, offset, 16) {
                    Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Err(e) => return ToolResult::error(e),
                }
            }
            None => String::new(),
        };
        ToolResult::text(format!("0.3 says {text}{read}"))
    }

    fn render(cx: Context, view: &str, _args: Value) -> Result<Value, String> {
        if view != "board" {
            return Err(format!("no such view {view}"));
        }
        let last = host::state_get(&cx, "last")
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        Ok(ui::tree(ui::stack(vec![ui::text(&format!(
            "last: {last}"
        ))])))
    }
}

chimaera_plugin_api::export!(Fixture);
