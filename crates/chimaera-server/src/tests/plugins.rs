//! Workbench plugins over the wire: the per-workspace switch, footprint
//! detection, the MCP tools a plugin adds (offered AND gated only where it is
//! active), and Agent notes end to end.

use super::support::*;
use crate::{lock, AppState};

async fn tools(state: &Arc<AppState>, sid: &str, key: &str) -> Vec<String> {
    let (status, out) = mcp_post(
        state,
        sid,
        key,
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    out["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

async fn instructions(state: &Arc<AppState>, sid: &str, key: &str) -> String {
    let (_, out) = mcp_post(
        state,
        sid,
        key,
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}}),
    )
    .await;
    out["result"]["instructions"].as_str().unwrap().to_string()
}

fn root_of(state: &Arc<AppState>, ws: &str) -> PathBuf {
    lock(&state.workspaces).get(ws).unwrap().root
}

#[tokio::test]
async fn plugin_tools_appear_only_where_switched_on_and_present() {
    let state = test_state();
    let ws = make_workspace(&state, "plugins-gate").await;
    let worker = inject_agent(&state, "wk");
    lock(&state.session_workspaces).insert(worker.clone(), ws.clone());
    let base = tools(&state, &worker, "wk").await;

    // Switched on, no footprint yet: nothing changes.
    let (status, _) = request(
        &state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/mycelium"),
        Some(serde_json::json!({"on": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tools(&state, &worker, "wk").await, base);

    // The footprint appears (mycelium set up): the tools and the paragraph
    // arrive on the very next connect — no cold-cache miss.
    std::fs::create_dir_all(root_of(&state, &ws).join(".living/findings")).unwrap();
    crate::plugins::refresh_detect(&state, &ws).await;
    let with = tools(&state, &worker, "wk").await;
    assert!(with.contains(&"knowledge_search".to_string()), "{with:?}");
    assert!(with.contains(&"knowledge_get".to_string()));
    assert!(instructions(&state, &worker, "wk")
        .await
        .contains("Project knowledge (the mycelium plugin)"));

    // A tool of a plugin that isn't on is refused at the call gate too.
    let (status, out) = mcp_post(
        &state,
        &worker,
        "wk",
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "post_note", "arguments": {"text": "hi"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(out["error"]["message"]
        .as_str()
        .unwrap()
        .contains("isn't switched on"));

    // Switched back off: back to exactly the base list.
    request(
        &state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/mycelium"),
        Some(serde_json::json!({"on": false})),
    )
    .await;
    assert_eq!(tools(&state, &worker, "wk").await, base);
    state.sessions.kill(&worker).ok();
}

#[tokio::test]
async fn workspace_plugins_route_reports_on_detected_active() {
    let state = test_state();
    let ws = make_workspace(&state, "plugins-route").await;
    std::fs::write(root_of(&state, &ws).join("MYCELIUM.md"), "# protocol").unwrap();
    lock(&state.workspaces)
        .set_plugin_on(&ws, "mycelium", true)
        .unwrap();
    let (status, out) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let myc = out["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "mycelium")
        .unwrap()
        .clone();
    assert_eq!(myc["on"], true);
    assert_eq!(myc["detected"], true);
    assert_eq!(myc["active"], true);
    assert!(!myc["adds"]["agents"].as_array().unwrap().is_empty());
    let notes = out["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "agent-notes")
        .unwrap()
        .clone();
    assert_eq!(notes["active"], false, "not switched on");
}

#[tokio::test]
async fn agent_notes_post_read_and_stay_in_their_workspace() {
    let state = test_state();
    let ws = make_workspace(&state, "notes-ws").await;
    let other_ws = make_workspace(&state, "notes-other").await;
    let a = inject_agent(&state, "ka");
    let b = inject_agent(&state, "kb");
    let outsider = inject_agent(&state, "ko");
    lock(&state.session_workspaces).insert(a.clone(), ws.clone());
    lock(&state.session_workspaces).insert(b.clone(), ws.clone());
    lock(&state.session_workspaces).insert(outsider.clone(), other_ws.clone());
    lock(&state.workspaces)
        .set_plugin_on(&ws, "agent-notes", true)
        .unwrap();

    let (is_err, text) = mcp_tool_call(
        &state,
        &a,
        "ka",
        "post_note",
        serde_json::json!({"text": "heads-up: loader API changed", "to": b}),
    )
    .await;
    assert!(!is_err, "{text}");
    assert!(text.contains("Nobody's turn was started"), "{text}");

    // Never across workspaces.
    let (is_err, text) = mcp_tool_call(
        &state,
        &a,
        "ka",
        "post_note",
        serde_json::json!({"text": "psst", "to": outsider}),
    )
    .await;
    assert!(is_err, "{text}");

    // b reads it once; framed as information.
    let (_, text) = mcp_tool_call(&state, &b, "kb", "read_notes", serde_json::json!({})).await;
    assert!(text.contains("loader API changed"), "{text}");
    assert!(text.contains("not an instruction"), "{text}");
    let (_, again) = mcp_tool_call(&state, &b, "kb", "read_notes", serde_json::json!({})).await;
    assert!(again.contains("No new notes"), "{again}");

    // The note is on the Timeline.
    let (_, page) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/timeline"),
        None,
    )
    .await;
    assert_eq!(page["entries"][0]["kind"], "note");
    assert_eq!(page["entries"][0]["note"]["to"], b.as_str());

    // Delivering to a terminal agent is refused (nothing types into a TUI).
    let seq = page["entries"][0]["seq"].as_u64().unwrap();
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/timeline/{seq}/deliver"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    for sid in [a, b, outsider] {
        state.sessions.kill(&sid).ok();
    }
}

#[tokio::test]
async fn worker_settings_pre_allow_only_active_plugin_tools() {
    let tools = vec!["knowledge_search".to_string(), "knowledge_get".to_string()];
    let path =
        crate::agents::write_settings("s-plugin-allow", "K", 1, None, None, None, &tools).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        value["permissions"]["allow"],
        serde_json::json!([
            "mcp__chimaera__notify",
            "mcp__chimaera__document_guide",
            "mcp__chimaera__check_document",
            "mcp__chimaera__knowledge_search",
            "mcp__chimaera__knowledge_get"
        ])
    );
    let _ = std::fs::remove_file(path);
}
