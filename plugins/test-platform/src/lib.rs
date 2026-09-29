//! The 0.2 platform fixture: every screen node, every platform import and
//! the new events, each reachable from the daemon's tests
//! (crates/chimaera-server/src/tests/plugin_platform.rs) through the views,
//! actions, file-action and query routes. Never shipped.
//!
//! What it heard is recorded under the `events` state key (the last 50,
//! `"<kind> <detail>"`), readable through `query("events")`; switching it
//! on or off is also kept durably (`switched`), so a test can read it after
//! the instance is gone.

use chimaera_plugin_api::serde_json::{json, Value};
use chimaera_plugin_api::{host, platform, ui, Context, Event, Plugin};

struct Fixture;

const EVENTS_KEPT: usize = 50;

fn record(cx: &Context, what: String) {
    let mut events: Vec<Value> = host::state_get(cx, "events")
        .ok()
        .flatten()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    events.push(json!(what));
    let over = events.len().saturating_sub(EVENTS_KEPT);
    events.drain(..over);
    let _ = host::state_put(cx, "events", &json!(events));
}

fn count(cx: &Context) -> u64 {
    host::state_get(cx, "count")
        .ok()
        .flatten()
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// Every node of `ui/1`, once, and two a client doesn't know.
fn board(cx: &Context) -> Value {
    let greeting = platform::setting(cx, "greeting").unwrap_or(json!("hello"));
    let loud = platform::setting(cx, "loud") == Some(json!(true));
    let mode = platform::setting(cx, "mode")
        .and_then(|m| m.as_str().map(str::to_string))
        .unwrap_or_else(|| "calm".into());
    let greeting = greeting.as_str().unwrap_or("hello");
    let greeting = if loud {
        greeting.to_uppercase()
    } else {
        greeting.to_string()
    };
    let kept = host::state_get(cx, "kept")
        .ok()
        .flatten()
        .unwrap_or(Value::Null);
    let roots = platform::roots(cx);
    let rows: Vec<Value> = (1..=3)
        .map(|i| json!({"title": format!("Row {i}"), "subtitle": "a list row", "badges": [{"text": "new", "tone": "accent"}],
            "actions": [ui::button("Open", "open-file", json!({"file": "notes.fixture"}))]}))
        .collect();
    ui::tree(json!({"type": "stack", "children": [
        {"type": "heading", "text": format!("{greeting} ({mode})"), "level": 1},
        {"type": "row", "gap": "small", "children": [
            {"type": "badge", "text": format!("count {}", count(cx)), "tone": "accent"},
            {"type": "badge", "text": format!("kept {kept}"), "tone": "neutral"},
            {"type": "icon", "name": "check", "label": "ok", "tone": "good"},
        ]},
        {"type": "text", "text": "Plain text in each tone.", "tone": "neutral"},
        {"type": "text", "text": "Good", "tone": "good", "size": "small"},
        {"type": "text", "text": "Warn", "tone": "warn", "emphasis": true},
        {"type": "text", "text": "Bad", "tone": "bad", "mono": true},
        {"type": "markdown", "text": "Some **markdown** with `code` and a [link](https://example.com)."},
        {"type": "code", "text": "fn main() {}\n", "language": "rust"},
        {"type": "keyvalue", "items": [
            {"key": "workspace", "value": roots.workspace},
            {"key": "output", "value": roots.output},
        ]},
        {"type": "progress", "value": 0.4, "label": "Building"},
        {"type": "progress", "label": "Waiting"},
        {"type": "callout", "title": "Heads up", "text": "A callout.", "tone": "warn"},
        {"type": "divider"},
        {"type": "section", "title": "Inputs", "children": [
            {"type": "row", "children": [
                ui::button("Count", "count", json!({"by": 1})),
                ui::button("Keep", "keep", json!({"value": "kept-value"})),
                ui::button("Publish", "publish", Value::Null),
                ui::button("Write output", "write", Value::Null),
                {"type": "button", "label": "Save to workspace", "action": "save-to-workspace",
                 "payload": {"from": "output:hello.txt", "to": "hello-saved.txt"}, "tone": "accent"},
            ]},
            {"type": "toggle", "label": "Loud", "name": "loud", "value": loud, "action": "toggle"},
            {"type": "select", "label": "Mode", "name": "mode", "value": mode,
             "options": [{"value": "calm", "label": "Calm"}, {"value": "busy", "label": "Busy"}], "action": "pick"},
            {"type": "form", "action": "submit", "submit": "Send", "children": [
                {"type": "textfield", "label": "Name", "name": "name", "placeholder": "Your name"},
                {"type": "textfield", "label": "Notes", "name": "notes", "multiline": true},
            ]},
        ]},
        {"type": "tabs", "tabs": [
            {"title": "List", "children": [{"type": "list", "items": rows,
                "more": {"query": "page", "args": {"offset": 3}}}]},
            {"title": "Table", "children": [{"type": "table",
                "columns": [{"key": "name", "title": "Name"}, {"key": "size", "title": "Size", "align": "end"}],
                "rows": [{"name": "main.tex", "size": "4 KB"}, {"name": "refs.bib", "size": "1 KB"}]}]},
        ]},
        {"type": "grid", "columns": 2, "children": [
            {"type": "card", "title": "A card", "children": [{"type": "text", "text": "In a grid."}]},
            {"type": "card", "children": [
                {"type": "file", "path": "notes.fixture", "label": "The notes"},
                {"type": "link", "text": "Open the notes at line 2", "file": "notes.fixture", "line": 2},
                {"type": "link", "text": "example.com", "href": "https://example.com"},
            ]},
        ]},
        {"type": "split", "ratio": 0.5, "children": [
            {"type": "editor", "path": "notes.fixture", "base": "head"},
            {"type": "pdf", "src": "output:out.pdf"},
        ]},
        {"type": "image", "src": "output:pic.png", "alt": "A picture the fixture wrote"},
        {"type": "diagnostics", "file": "notes.fixture"},
        {"type": "diff", "before": "the quick brown fox\n", "after": "the quick red fox\n", "mode": "prose"},
        {"type": "log", "src": "output:hello.txt"},
        {"type": "empty", "title": "Nothing here yet", "text": "An empty state.",
         "action": {"label": "Count", "action": "count"}},
        {"type": "sparkle-graph", "fallback": {"type": "text", "text": "fallback for a node from the future"}},
        {"type": "hologram", "children": [{"type": "text", "text": "a child of an unknown node"}]},
    ]}))
}

fn doc(cx: &Context, file: &str) -> Value {
    let text = host::read(cx, file, 64 * 1024)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_else(|e| e);
    ui::tree(ui::stack(vec![
        ui::heading(file),
        json!({"type": "code", "text": text}),
        json!({"type": "diagnostics", "file": file}),
    ]))
}

fn events(cx: &Context) -> Value {
    host::state_get(cx, "events")
        .ok()
        .flatten()
        .unwrap_or(json!([]))
}

impl Plugin for Fixture {
    fn render(cx: Context, view: &str, args: Value) -> Result<Value, String> {
        match view {
            "board" => Ok(board(&cx)),
            "panel" => Ok(ui::tree(ui::section(
                "Platform",
                vec![ui::text(&format!(
                    "{} events",
                    events(&cx).as_array().map_or(0, Vec::len)
                ))],
            ))),
            "doc" => {
                let file = args.get("file").and_then(Value::as_str).ok_or("no file")?;
                Ok(doc(&cx, file))
            }
            "chip" => Ok(ui::tree(
                json!({"type": "badge", "text": format!("{}", count(&cx)), "tone": "accent"}),
            )),
            "card" => Ok(ui::tree(ui::text("The fixture's card section."))),
            other => Err(format!("no such view {other}")),
        }
    }

    fn on_action(
        cx: Context,
        view: &str,
        action: &str,
        payload: Value,
    ) -> Result<Option<Value>, String> {
        match action {
            "count" => {
                let by = payload.get("by").and_then(Value::as_u64).unwrap_or(1);
                host::state_put(&cx, "count", &json!(count(&cx) + by))?;
                Ok(Some(board(&cx)))
            }
            "keep" => {
                let value = payload.get("value").cloned().unwrap_or(Value::Null);
                platform::state_keep(&cx, "kept", &value)?;
                Ok(Some(board(&cx)))
            }
            "publish" => {
                platform::publish(
                    &cx,
                    "diagnostics/1",
                    "notes",
                    &json!({"items": [
                        {"file": "notes.fixture", "severity": "error", "line": 2, "message": "Undefined thing", "source": "fixture"},
                        {"file": "notes.fixture", "severity": "warning", "line": 1, "message": "A warning"},
                    ]}),
                )?;
                platform::publish(
                    &cx,
                    "output/1",
                    "notes",
                    &json!({"source": "notes.fixture", "output": "output:hello.txt", "state": "ok",
                            "label": "Built", "finished_ms": host::now_ms(), "changed_pages": [1]}),
                )?;
                platform::publish(
                    &cx,
                    "references/1",
                    "notes",
                    &json!({"shapes": [{"kind": "fixture", "pattern": "FX-\\d+"}],
                            "ids": [{"id": "FX-1", "key": "fx-1", "kind": "fixture",
                                     "title": "The first fixture id",
                                     "span": {"path": "notes.fixture", "line": 1, "end_line": 2}},
                                    {"id": "FX-2", "key": "fx-2", "kind": "fixture",
                                     "title": "The board", "view": "board"}]}),
                )?;
                platform::invalidate(&cx, "board");
                Ok(None)
            }
            "write" => {
                platform::output_write(&cx, "hello.txt", b"hello from the output folder\n")?;
                let back = platform::output_read(&cx, "hello.txt", 6, 4)?;
                let listed = platform::output_list(&cx, "", 16)?;
                record(
                    &cx,
                    format!(
                        "wrote {} {}",
                        String::from_utf8_lossy(&back),
                        listed
                            .iter()
                            .map(|e| e.name.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                );
                Ok(Some(board(&cx)))
            }
            "watch" => {
                let paths: Vec<String> = payload
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| p.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                platform::watch(&cx, &paths)?;
                Ok(None)
            }
            "remove" => {
                platform::output_remove(&cx, "hello.txt")?;
                Ok(None)
            }
            "toggle" | "pick" | "submit" => {
                record(&cx, format!("{action} {payload}"));
                Ok(Some(board(&cx)))
            }
            "bad-tree" => Ok(Some(
                json!({"ui": "1", "root": {"type": "button", "action": "x"}}),
            )),
            "count-words" if view.is_empty() => {
                let file = payload
                    .get("file")
                    .and_then(Value::as_str)
                    .ok_or("no file")?;
                let text = host::read(&cx, file, 1 << 20)?;
                let words = String::from_utf8_lossy(&text).split_whitespace().count();
                Ok(Some(json!({"message": format!("{file}: {words} words"),
                               "open": {"view": "board"}})))
            }
            other => Err(format!("no such action {other}")),
        }
    }

    fn query(cx: Context, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "events" => Ok(events(&cx)),
            "page" => {
                let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
                let limit = platform::setting(&cx, "limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(3);
                let items: Vec<Value> = (offset..offset + limit)
                    .map(|i| json!({"title": format!("Row {}", i + 1)}))
                    .collect();
                Ok(
                    json!({"items": items, "more": {"query": "page", "args": {"offset": offset + limit}}}),
                )
            }
            "state" => Ok(json!({
                "kept": host::state_get(&cx, "kept").ok().flatten(),
                "switched": host::state_get(&cx, "switched").ok().flatten(),
                "settings": {
                    "greeting": platform::setting(&cx, "greeting"),
                    "loud": platform::setting(&cx, "loud"),
                    "mode": platform::setting(&cx, "mode"),
                    "limit": platform::setting(&cx, "limit"),
                    "main": platform::setting(&cx, "main"),
                    "undeclared": platform::setting(&cx, "undeclared"),
                },
            })),
            _ => Err(format!("no such query {name}")),
        }
    }

    fn on_event(cx: Context, event: Event) -> Option<String> {
        match event {
            Event::FileSaved(path) => {
                record(&cx, format!("saved {path}"));
                platform::invalidate(&cx, "board");
            }
            Event::FileChanged(path) => {
                record(&cx, format!("changed {path}"));
                platform::invalidate(&cx, "board");
            }
            Event::SettingsChanged(key) => record(&cx, format!("setting {key}")),
            Event::SwitchedOn => {
                let _ = platform::state_keep(&cx, "switched", &json!("on"));
            }
            Event::SwitchedOff => {
                let _ = platform::state_keep(&cx, "switched", &json!("off"));
            }
            _ => {}
        }
        None
    }
}

chimaera_plugin_api::export!(Fixture);
