//! The web views fixture (API 0.4): a `web` node on its Web board and in
//! its `.webfix` file view, the page that node shows (`web/view.html`, in
//! the bundle `web_bundle()` returns), a library it lends agents' pages
//! (`web/testlib/`), and 0.4's calls and reveal: `web_ask` asks the page
//! (`platform::web_call`, answered through `wait_call` and `tool_resume`),
//! `web_show` opens the board or a file beside the agent
//! (`platform::reveal`), and `TW-<n>` in chat is a chip that opens the
//! board at that item (`references/1` with a shape view). Each piece is
//! reachable from the daemon's tests
//! (crates/chimaera-server/src/tests/plugin_web.rs) and by hand in the
//! isolated preview. Never shipped.

use chimaera_plugin_api::serde_json::{json, Value};
use chimaera_plugin_api::{host, platform, Context, Event, Plugin, ToolDef, ToolResult};

struct Fixture;

/// The bundle, packed from `web/` by `build.rs`.
const BUNDLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web.tar"));

fn pings(cx: &Context) -> u64 {
    host::state_get(cx, "pings")
        .ok()
        .flatten()
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// The view's args (`render`'s `args.args`): what a reveal or a chip
/// opened it with.
fn item(args: &Value) -> Option<String> {
    args.get("args")
        .and_then(|a| a.get("item"))
        .and_then(Value::as_str)
        .map(String::from)
}

/// A screen of the web node alone, whose `data` carries the pings so far
/// and the item it was opened at (a new `key` delivers it to the running
/// page without a reload).
fn board(cx: &Context, item: Option<String>, file: Option<&str>) -> Value {
    let n = pings(cx);
    json!({"ui": "1", "fill": true, "root": {"type": "stack", "children": [
        {"type": "web", "entry": "view.html", "id": "fixture", "label": "Web fixture page",
         "data": {"greeting": "hello from the plugin", "pings": n, "file": file.unwrap_or("sample.csv"), "item": item},
         "key": n,
         "fallback": {"type": "callout", "text": "This chimaera cannot draw web views."}}
    ]}})
}

/// The ids it answers for: any `TW-<n>` opens the board at it.
fn publish_references(cx: &Context) {
    let _ = platform::publish(
        cx,
        "references/1",
        "items",
        &json!({
            "shapes": [{"kind": "item", "pattern": "TW-[0-9]+",
                        "view": {"view": "board", "args_from_match": {"item": "<match>"}}}],
            "ids": [],
        }),
    );
}

impl Plugin for Fixture {
    fn tools() -> Vec<ToolDef> {
        vec![
            ToolDef::new(
                "web_ask",
                "Ask the Web board's page: method ping (what it shows), slow (answers after ms), \
                 big (an answer over the limit), fail (throws) or late (a handler it registers \
                 after 2 s). fresh: true asks a headless page even when a window shows it.",
                json!({"type": "object", "properties": {
                    "method": {"type": "string"},
                    "ms": {"type": "number"},
                    "fresh": {"type": "boolean"},
                    "file": {"type": "string"}
                }}),
            ),
            ToolDef::new(
                "web_show",
                "Open the Web board (at an item, e.g. TW-7), or a .webfix file, beside you.",
                json!({"type": "object", "properties": {
                    "item": {"type": "string"},
                    "file": {"type": "string"}
                }}),
            ),
        ]
    }

    fn call_tool(cx: Context, name: &str, args: Value) -> ToolResult {
        match name {
            "web_ask" => {
                let method = args.get("method").and_then(Value::as_str).unwrap_or("ping");
                let file = args.get("file").and_then(Value::as_str).map(String::from);
                let call = platform::WebCall {
                    view: if file.is_some() { "doc" } else { "board" }.into(),
                    id: "fixture".into(),
                    method: method.into(),
                    args: json!({"ms": args.get("ms").cloned().unwrap_or(json!(0))}),
                    file,
                    fresh: args.get("fresh").and_then(Value::as_bool) == Some(true),
                };
                match platform::web_call_with(&cx, &call) {
                    Ok(id) => ToolResult::wait_call(id, "the page is still answering"),
                    Err(e) => ToolResult::error(e),
                }
            }
            "web_show" => {
                let spec = match args.get("file").and_then(Value::as_str) {
                    Some(file) => json!({"file": file}),
                    None => json!({"view": "board", "args": {"item": args.get("item")}}),
                };
                match platform::reveal(&cx, &spec) {
                    Ok(text) => ToolResult::text(text),
                    Err(e) => ToolResult::error(e),
                }
            }
            other => ToolResult::error(format!("unknown tool {other}")),
        }
    }

    fn tool_resume(cx: Context, _name: &str, call: &str) -> ToolResult {
        match platform::web_call_result(&cx, call) {
            Ok(Some(value)) => ToolResult::text(format!("the page answered: {value}")),
            Ok(None) => ToolResult::text("the page is still answering"),
            Err(e) => ToolResult::error(format!("the page couldn't answer: {e}")),
        }
    }

    fn on_event(cx: Context, event: Event) -> Option<String> {
        if let Event::SwitchedOn = event {
            publish_references(&cx);
        }
        None
    }

    fn render(cx: Context, view: &str, args: Value) -> Result<Value, String> {
        // Published here too: a plugin switched on before this build.
        publish_references(&cx);
        match view {
            "board" => Ok(board(&cx, item(&args), None)),
            "doc" => {
                let file = args.get("file").and_then(Value::as_str);
                Ok(board(&cx, item(&args), file))
            }
            other => Err(format!("no such view {other}")),
        }
    }

    fn on_action(
        cx: Context,
        view: &str,
        action: &str,
        _payload: Value,
    ) -> Result<Option<Value>, String> {
        match action {
            "ping" => {
                let n = pings(&cx) + 1;
                host::state_put(&cx, "pings", &json!(n))?;
                Ok(Some(board(&cx, None, None)))
            }
            other => Err(format!("{view}: no such action {other}")),
        }
    }

    /// `read-at {path, offset, cap}`: 0.3's `host::read_at`, for the tests.
    fn query(cx: Context, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "read-at" => {
                let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
                let cap = args.get("cap").and_then(Value::as_u64).unwrap_or(64) as u32;
                let bytes = host::read_at(&cx, path, offset, cap)?;
                Ok(json!({"text": String::from_utf8_lossy(&bytes)}))
            }
            other => Err(format!("no such query {other}")),
        }
    }

    fn web_bundle() -> Vec<u8> {
        BUNDLE.to_vec()
    }
}

chimaera_plugin_api::export!(Fixture);
