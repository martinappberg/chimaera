//! The host's test fixture: each tool pokes one limit of the plugin host
//! (crates/chimaera-server/src/tests/plugin_host.rs). Never shipped: the
//! build script lays it out under plugins/dist-test, which only the
//! daemon's test builds embed.

use chimaera_plugin_api::serde_json::{json, Value};
use chimaera_plugin_api::{host, Context, Plugin, ToolDef, ToolResult};

const TOOLS: [&str; 8] = [
    "echo", "loop", "allocate", "panic", "read", "state", "append", "recent",
];

struct Fixture;

impl Plugin for Fixture {
    fn tools() -> Vec<ToolDef> {
        TOOLS
            .iter()
            .map(|name| ToolDef::new(*name, "A test fixture tool.", json!({"type": "object"})))
            .collect()
    }

    fn call_tool(cx: Context, name: &str, args: Value) -> ToolResult {
        match name {
            // Its arguments back, plus the context the host handed it; an
            // `emit` argument goes out as a UI event first.
            "echo" => {
                if let Some(event) = args.get("emit") {
                    host::emit(&cx, event);
                }
                ToolResult::text(
                    json!({"args": args, "workspace": cx.workspace, "session": cx.session,
                        "mastermind": cx.mastermind})
                    .to_string(),
                )
            }
            // Never returns: the host's deadline must stop it.
            "loop" => {
                let mut x: u64 = 0;
                loop {
                    x = std::hint::black_box(x.wrapping_add(1));
                }
            }
            // {mb}: allocate and touch that much linear memory.
            "allocate" => {
                let mb = args.get("mb").and_then(Value::as_u64).unwrap_or(1) as usize;
                let mut bytes: Vec<u8> = Vec::with_capacity(mb << 20);
                bytes.resize(mb << 20, 1);
                std::hint::black_box(&bytes);
                ToolResult::text(format!("allocated {mb} MiB"))
            }
            "panic" => panic!("the test fixture panics on request"),
            // {path, cap?, mode?}: read (the byte count), "stat" or "list".
            "read" => {
                let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                let cap = args.get("cap").and_then(Value::as_u64).unwrap_or(1 << 20) as u32;
                match args.get("mode").and_then(Value::as_str) {
                    Some("stat") => match host::stat(&cx, path) {
                        Ok(s) => ToolResult::text(
                            json!({"size": s.size, "mtime_ms": s.mtime_ms, "is_dir": s.is_dir})
                                .to_string(),
                        ),
                        Err(err) => ToolResult::error(err),
                    },
                    Some("list") => match host::list(&cx, path, cap) {
                        Ok(entries) => ToolResult::text(
                            json!(entries
                                .iter()
                                .map(|e| json!({"name": e.name, "is_dir": e.is_dir,
                                    "is_symlink": e.is_symlink}))
                                .collect::<Vec<_>>())
                            .to_string(),
                        ),
                        Err(err) => ToolResult::error(err),
                    },
                    _ => match host::read(&cx, path, cap) {
                        Ok(bytes) => ToolResult::text(bytes.len().to_string()),
                        Err(err) => ToolResult::error(err),
                    },
                }
            }
            // {key, value} or {key, big: <bytes>}: put, then get it back.
            "state" => {
                let key = args.get("key").and_then(Value::as_str).unwrap_or("k");
                let value = match args.get("big").and_then(Value::as_u64) {
                    Some(n) => json!("x".repeat(n as usize)),
                    None => args.get("value").cloned().unwrap_or(Value::Null),
                };
                if let Err(err) = host::state_put(&cx, key, &value) {
                    return ToolResult::error(err);
                }
                match host::state_get(&cx, key) {
                    Ok(v) => ToolResult::text(v.map(|v| v.to_string()).unwrap_or_default()),
                    Err(err) => ToolResult::error(err),
                }
            }
            // {entry} or {text}: a Timeline append; the new seq.
            "append" => {
                let entry = args.get("entry").cloned().unwrap_or_else(|| {
                    json!({"kind": "note",
                        "text": args.get("text").and_then(Value::as_str).unwrap_or("a fixture note")})
                });
                match host::timeline_append(&cx, &entry) {
                    Ok(seq) => ToolResult::text(seq.to_string()),
                    Err(err) => ToolResult::error(err),
                }
            }
            // {kinds?, limit?}: the newest entries of those kinds.
            "recent" => {
                let kinds: Vec<&str> = args
                    .get("kinds")
                    .and_then(Value::as_array)
                    .map(|k| k.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_else(|| vec!["note"]);
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as u32;
                match host::timeline_recent(&cx, &kinds, limit) {
                    Ok(entries) => ToolResult::text(Value::Array(entries).to_string()),
                    Err(err) => ToolResult::error(err),
                }
            }
            other => ToolResult::error(format!("unknown tool {other}")),
        }
    }
}

chimaera_plugin_api::export!(Fixture);
