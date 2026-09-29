//! The privileged fixture: stand-in programs run as jobs, a stand-in tool
//! the host downloads and sets up, a long agent tool, and `job-finished`,
//! each reachable from the daemon's tests
//! (crates/chimaera-server/src/tests/plugin_jobs.rs). Never shipped.
//!
//! What it heard is recorded under the `events` state key (`"finished <id>
//! exit=<code> timed_out=<bool> out=<first line of stdout>"`), readable
//! through `query("events")`.

use chimaera_plugin_api::serde_json::{json, Value};
use chimaera_plugin_api::{host, platform, ui, Context, Event, Plugin, ToolDef, ToolResult};

struct Fixture;

fn record(cx: &Context, what: String) {
    let mut events: Vec<Value> = host::state_get(cx, "events")
        .ok()
        .flatten()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    events.push(json!(what));
    let over = events.len().saturating_sub(50);
    events.drain(..over);
    let _ = host::state_put(cx, "events", &json!(events));
}

/// The first line a job printed.
fn first_line(cx: &Context, id: &str) -> String {
    platform::output_read(cx, &format!(".jobs/{id}/stdout.log"), 0, 4096)
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .next()
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_else(|e| format!("(unread: {e})"))
}

fn start(cx: &Context, spec: Value) -> Result<Option<Value>, String> {
    let id = platform::job_start(cx, &spec)?;
    record(cx, format!("started {id}"));
    Ok(Some(ui::tree(ui::text(&id))))
}

impl Plugin for Fixture {
    fn tools() -> Vec<ToolDef> {
        vec![ToolDef::new(
            "build",
            "Run the fixture's stand-in build and answer with what it printed.",
            json!({"type": "object", "properties": {"slow": {"type": "boolean"}}}),
        )]
    }

    fn call_tool(cx: Context, name: &str, args: Value) -> ToolResult {
        if name != "build" {
            return ToolResult::error(format!("unknown tool {name}"));
        }
        let script = if args.get("slow").and_then(Value::as_bool) == Some(true) {
            "sleep 60; echo late"
        } else {
            "echo building; sleep 0.2; echo built"
        };
        match platform::job_start(
            &cx,
            &json!({"program": "sh", "args": ["-c", script], "priority": "agent", "label": "build"}),
        ) {
            Ok(id) => ToolResult::wait(id, "still building"),
            Err(e) => ToolResult::error(e),
        }
    }

    fn tool_resume(cx: Context, _name: &str, job: &str) -> ToolResult {
        let status = platform::job_status(&cx, job);
        ToolResult::text(format!(
            "exit {} · {}",
            status["exit"],
            first_line(&cx, job)
        ))
    }

    fn render(cx: Context, view: &str, _args: Value) -> Result<Value, String> {
        if view != "jobs" {
            return Err(format!("no such view {view}"));
        }
        let tool = platform::tool_state(&cx, "fixture-tool");
        Ok(ui::tree(ui::stack(vec![
            ui::heading("Jobs"),
            json!({"type": "keyvalue", "items": [
                {"key": "tool", "value": if tool["installed"].is_null() { "not installed" } else { "installed" }},
            ]}),
            ui::row(vec![
                ui::button("Echo", "echo", Value::Null),
                ui::button("Sleep past its limit", "sleep", Value::Null),
                ui::button("Run the tool", "tool", Value::Null),
            ]),
        ])))
    }

    fn on_action(
        cx: Context,
        _view: &str,
        action: &str,
        payload: Value,
    ) -> Result<Option<Value>, String> {
        let wall = payload.get("wall_s").and_then(Value::as_u64);
        match action {
            "echo" => start(
                &cx,
                json!({"program": "echo", "args": ["hello", "from", "a", "job"]}),
            ),
            "sleep" => start(
                &cx,
                // A child in the background too: the whole group must go.
                json!({"program": "sh", "args": ["-c", "sleep 30 & echo $! > child.pid; wait"],
                       "cwd": "output:", "wall_s": wall.unwrap_or(1)}),
            ),
            "slow" => start(
                &cx,
                json!({"program": "sleep", "args": ["30"], "wall_s": wall.unwrap_or(30)}),
            ),
            "tool" => start(
                &cx,
                json!({"program": "fixture-tool", "args": ["hello"], "prefer": "tool:fixture-tool"}),
            ),
            "env" => start(
                &cx,
                json!({"program": "sh", "args": ["-c", "echo \"$GREETING $TERM\""],
                       "env": payload.get("env").cloned().unwrap_or(json!({"GREETING": "hi"}))}),
            ),
            "spec" => start(&cx, payload),
            "cancel" => {
                let id = payload.get("id").and_then(Value::as_str).unwrap_or("");
                platform::job_cancel(&cx, id);
                Ok(None)
            }
            other => Err(format!("no such action {other}")),
        }
    }

    fn query(cx: Context, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "events" => Ok(host::state_get(&cx, "events")
                .ok()
                .flatten()
                .unwrap_or(json!([]))),
            "status" => {
                let id = args.get("id").and_then(Value::as_str).unwrap_or("");
                Ok(platform::job_status(&cx, id))
            }
            "tool" => Ok(platform::tool_state(&cx, "fixture-tool")),
            "roots" => {
                let r = platform::roots(&cx);
                Ok(json!({"workspace": r.workspace, "output": r.output}))
            }
            _ => Err(format!("no such query {name}")),
        }
    }

    fn on_event(cx: Context, event: Event) -> Option<String> {
        if let Event::JobFinished(end) = event {
            record(
                &cx,
                format!(
                    "finished {} exit={} timed_out={} out={}",
                    end.id,
                    end.exit.map_or("none".to_string(), |c| c.to_string()),
                    end.timed_out,
                    first_line(&cx, &end.id)
                ),
            );
        }
        None
    }
}

chimaera_plugin_api::export!(Fixture);
