//! The web views fixture (API 0.3): a `web` node on its Web board, the page
//! that node shows (`web/view.html`, in the bundle `web_bundle()` returns),
//! and a library it lends agents' pages (`web/testlib/`). Each piece is
//! reachable from the daemon's tests
//! (crates/chimaera-server/src/tests/plugin_web.rs) and by hand in the
//! isolated preview. Never shipped.

use chimaera_plugin_api::serde_json::{json, Value};
use chimaera_plugin_api::{host, Context, Plugin};

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

/// The board: a heading and the web node, whose `data` carries the pings
/// so far (a new `key` delivers it to the running page without a reload).
fn board(cx: &Context) -> Value {
    let n = pings(cx);
    json!({"ui": "1", "fill": true, "root": {"type": "stack", "children": [
        {"type": "text", "text": format!("The page has pinged {n} times."), "tone": "neutral"},
        {"type": "web", "entry": "view.html", "id": "fixture", "label": "Web fixture page",
         "data": {"greeting": "hello from the plugin", "pings": n, "file": "sample.csv"},
         "key": n,
         "fallback": {"type": "callout", "text": "This chimaera cannot draw web views."}}
    ]}})
}

impl Plugin for Fixture {
    fn render(cx: Context, view: &str, _args: Value) -> Result<Value, String> {
        match view {
            "board" => Ok(board(&cx)),
            other => Err(format!("no such view {other}")),
        }
    }

    fn on_action(cx: Context, view: &str, action: &str, _payload: Value) -> Result<Option<Value>, String> {
        match action {
            "ping" => {
                let n = pings(&cx) + 1;
                host::state_put(&cx, "pings", &json!(n))?;
                Ok(Some(board(&cx)))
            }
            other => Err(format!("{view}: no such action {other}")),
        }
    }

    fn web_bundle() -> Vec<u8> {
        BUNDLE.to_vec()
    }
}

chimaera_plugin_api::export!(Fixture);
