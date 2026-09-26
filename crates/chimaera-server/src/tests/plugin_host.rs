//! The plugin host's limits, proven with the fixture plugin
//! (`plugins/test-fixture`, built into `plugins/dist-test` by
//! `scripts/build-plugins.sh`): the call deadline, the memory cap, a
//! panicking plugin, the fs confinement, the state cap, the Timeline's kind
//! allowlist and rate cap, the fault counter, and the manifest/tools check.
//! Each test runs through the real MCP endpoint, as an agent would.

use std::time::{Duration, Instant};

use super::support::*;
use crate::{lock, AppState};

/// A workspace with the fixture switched on and one agent session in it.
async fn fixture_workspace(label: &str, key: &str) -> (Arc<AppState>, String, String) {
    crate::plugins::test_catalog::fixture();
    let state = test_state();
    let ws = make_workspace(&state, label).await;
    let sid = inject_agent(&state, key);
    lock(&state.session_workspaces).insert(sid.clone(), ws.clone());
    lock(&state.workspaces)
        .set_plugin_on(&ws, "test-fixture", true)
        .unwrap();
    (state, ws, sid)
}

fn root_of(state: &Arc<AppState>, ws: &str) -> PathBuf {
    lock(&state.workspaces).get(ws).unwrap().root
}

#[tokio::test]
async fn the_first_call_compiles_instantiates_and_answers_with_the_hosts_context() {
    let (state, ws, sid) = fixture_workspace("host-first", "k1").await;
    let started = Instant::now();
    let (is_err, text) = mcp_tool_call(
        &state,
        &sid,
        "k1",
        "echo",
        serde_json::json!({"hello": "world"}),
    )
    .await;
    // The compile is once per process (tests share it), so this is an upper
    // bound on the cold path only when this test runs first.
    eprintln!(
        "first fixture call (compile + instantiate + call): {:?}",
        started.elapsed()
    );
    assert!(!is_err, "{text}");
    let echoed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(echoed["args"]["hello"], "world");
    assert_eq!(echoed["workspace"], ws.as_str());
    assert_eq!(echoed["session"], sid.as_str());
    assert_eq!(echoed["mastermind"], false);

    // Its tools are offered here, and only here.
    let (_, list) = mcp_post(
        &state,
        &sid,
        "k1",
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for tool in [
        "echo", "loop", "allocate", "panic", "read", "state", "append", "recent",
    ] {
        assert!(names.contains(&tool), "{tool} missing from {names:?}");
    }
    let started = Instant::now();
    mcp_tool_call(&state, &sid, "k1", "echo", serde_json::json!({})).await;
    eprintln!("warm fixture call through MCP: {:?}", started.elapsed());
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn a_runaway_loop_is_stopped_at_its_deadline_and_the_plugin_recovers() {
    let (state, _ws, sid) = fixture_workspace("host-deadline", "k2").await;
    state
        .plugin_runtime
        .set_budget_for_tests(Duration::from_millis(500));
    // Warm up so the measured call is the loop alone, not the compile.
    mcp_tool_call(&state, &sid, "k2", "echo", serde_json::json!({})).await;
    let started = Instant::now();
    let (is_err, text) = mcp_tool_call(&state, &sid, "k2", "loop", serde_json::json!({})).await;
    let took = started.elapsed();
    assert!(is_err, "{text}");
    assert!(text.contains("budget"), "{text}");
    assert!(
        took >= Duration::from_millis(450) && took < Duration::from_secs(3),
        "stopped after {took:?}"
    );
    let (is_err, text) = mcp_tool_call(&state, &sid, "k2", "echo", serde_json::json!({})).await;
    assert!(!is_err, "a fresh instance answers: {text}");
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn linear_memory_is_capped_per_instance() {
    let (state, _ws, sid) = fixture_workspace("host-memory", "k3").await;
    let (is_err, text) = mcp_tool_call(
        &state,
        &sid,
        "k3",
        "allocate",
        serde_json::json!({"mb": 200}),
    )
    .await;
    assert!(is_err, "200 MiB must not fit under 64 MiB: {text}");
    let (is_err, text) = mcp_tool_call(
        &state,
        &sid,
        "k3",
        "allocate",
        serde_json::json!({"mb": 16}),
    )
    .await;
    assert!(!is_err, "{text}");
    assert_eq!(text, "allocated 16 MiB");
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn a_panicking_plugin_is_an_error_and_the_next_call_works() {
    let (state, _ws, sid) = fixture_workspace("host-panic", "k4").await;
    let (is_err, text) = mcp_tool_call(&state, &sid, "k4", "panic", serde_json::json!({})).await;
    assert!(is_err, "{text}");
    assert!(
        text.contains("the test fixture panics on request"),
        "the guest's panic message rides the error: {text}"
    );
    let (is_err, text) = mcp_tool_call(&state, &sid, "k4", "echo", serde_json::json!({})).await;
    assert!(!is_err, "{text}");
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn reads_stay_in_the_workspace_refuse_symlinks_and_stop_at_the_cap() {
    let (state, ws, sid) = fixture_workspace("host-read", "k5").await;
    let root = root_of(&state, &ws);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/a.txt"), "x".repeat(100)).unwrap();
    let outside = test_dir("host-read-outside");
    std::fs::write(outside.join("secret.txt"), "secret").unwrap();
    std::os::unix::fs::symlink(root.join("sub/a.txt"), root.join("link.txt")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
    std::os::unix::fs::symlink(root.join("sub"), root.join("linkdir")).unwrap();

    let read = |args: serde_json::Value| {
        let state = state.clone();
        let sid = sid.clone();
        async move { mcp_tool_call(&state, &sid, "k5", "read", args).await }
    };
    assert_eq!(
        read(serde_json::json!({"path": "sub/a.txt"})).await,
        (false, "100".to_string())
    );
    assert_eq!(
        read(serde_json::json!({"path": "./sub/a.txt", "cap": 10})).await,
        (false, "10".to_string()),
        "a read stops at its cap"
    );
    for path in ["link.txt", "out/secret.txt", "linkdir/a.txt"] {
        let (is_err, text) = read(serde_json::json!({"path": path})).await;
        assert!(is_err && text.contains("symlink"), "{path}: {text}");
    }
    let (is_err, text) = read(serde_json::json!({"path": "../host-read-outside/secret.txt"})).await;
    assert!(is_err && text.contains("`..` is refused"), "{text}");
    let (is_err, text) = read(serde_json::json!({"path": outside.join("secret.txt")})).await;
    assert!(is_err && text.contains("absolute"), "{text}");
    let (is_err, text) = read(serde_json::json!({"path": "sub"})).await;
    assert!(is_err && text.contains("not a regular file"), "{text}");

    let (is_err, text) = read(serde_json::json!({"path": "sub/a.txt", "mode": "stat"})).await;
    assert!(!is_err, "{text}");
    let stat: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(stat["size"], 100);
    assert_eq!(stat["is_dir"], false);
    assert!(stat["mtime_ms"].as_u64().unwrap() > 0);
    let (is_err, text) = read(serde_json::json!({"path": "link.txt", "mode": "stat"})).await;
    assert!(is_err && text.contains("symlink"), "{text}");

    let (is_err, text) = read(serde_json::json!({"path": "", "mode": "list"})).await;
    assert!(!is_err, "{text}");
    let entries: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["link.txt", "linkdir", "out", "sub"]);
    assert_eq!(entries[0]["is_symlink"], true);
    assert_eq!(entries[3]["is_dir"], true);
    let (_, text) = read(serde_json::json!({"path": "", "mode": "list", "cap": 2})).await;
    let capped: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(capped.len(), 2, "a listing stops at its cap");
    let (is_err, text) = read(serde_json::json!({"path": "linkdir", "mode": "list"})).await;
    assert!(is_err && text.contains("symlink"), "{text}");
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn plugin_state_is_capped_per_workspace() {
    let (state, _ws, sid) = fixture_workspace("host-state", "k6").await;
    let (is_err, text) = mcp_tool_call(
        &state,
        &sid,
        "k6",
        "state",
        serde_json::json!({"key": "k", "value": {"a": 1}}),
    )
    .await;
    assert!(!is_err, "{text}");
    assert_eq!(text, r#"{"a":1}"#);
    let (is_err, text) = mcp_tool_call(
        &state,
        &sid,
        "k6",
        "state",
        serde_json::json!({"key": "big", "big": 70_000}),
    )
    .await;
    assert!(is_err, "{text}");
    assert!(text.contains("capped at 64 KiB"), "{text}");
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn timeline_appends_are_notes_only_rate_capped_and_readable() {
    let (state, ws, sid) = fixture_workspace("host-timeline", "k7").await;
    let (is_err, text) = mcp_tool_call(
        &state,
        &sid,
        "k7",
        "append",
        serde_json::json!({"entry": {"kind": "episode", "text": "a forged turn"}}),
    )
    .await;
    assert!(is_err && text.contains("`note` entries only"), "{text}");
    let (is_err, text) = mcp_tool_call(
        &state,
        &sid,
        "k7",
        "append",
        serde_json::json!({"entry": {"kind": "note", "text": "x", "sid": "someone-else"}}),
    )
    .await;
    assert!(is_err && text.contains("not sid"), "{text}");
    for i in 0..10 {
        let (is_err, text) = mcp_tool_call(
            &state,
            &sid,
            "k7",
            "append",
            serde_json::json!({"text": format!("note {i}")}),
        )
        .await;
        assert!(!is_err, "append {i}: {text}");
    }
    let (is_err, text) = mcp_tool_call(
        &state,
        &sid,
        "k7",
        "append",
        serde_json::json!({"text": "one too many"}),
    )
    .await;
    assert!(
        is_err && text.contains("too many notes this minute"),
        "{text}"
    );

    let (is_err, text) = mcp_tool_call(
        &state,
        &sid,
        "k7",
        "recent",
        serde_json::json!({"kinds": ["note"], "limit": 3}),
    )
    .await;
    assert!(!is_err, "{text}");
    let recent: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(recent.len(), 3);
    assert_eq!(recent[0]["note"]["text"], "note 9", "newest first");
    assert_eq!(
        recent[0]["note"]["from_sid"],
        sid.as_str(),
        "the host names the poster"
    );
    assert_eq!(recent[0]["sid"], sid.as_str());

    // The appends are the Timeline's own entries.
    let (_, page) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/timeline"),
        None,
    )
    .await;
    assert_eq!(page["entries"][0]["kind"], "note");
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn five_traps_in_a_minute_fault_the_plugin_until_it_is_switched_off_and_on() {
    let (state, ws, sid) = fixture_workspace("host-faults", "k8").await;
    for _ in 0..5 {
        let (is_err, _) = mcp_tool_call(&state, &sid, "k8", "panic", serde_json::json!({})).await;
        assert!(is_err);
    }
    let (is_err, text) = mcp_tool_call(&state, &sid, "k8", "echo", serde_json::json!({})).await;
    assert!(is_err && text.contains("faulted"), "{text}");
    let (_, out) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins"),
        None,
    )
    .await;
    let card = out["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "test-fixture")
        .unwrap()
        .clone();
    assert!(
        card["fault"]
            .as_str()
            .unwrap()
            .contains("5 failures within a minute"),
        "{card}"
    );
    for on in [false, true] {
        let (status, _) = request(
            &state,
            Method::PUT,
            &format!("/api/v1/workspaces/{ws}/plugins/test-fixture"),
            Some(serde_json::json!({"on": on})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (is_err, text) = mcp_tool_call(&state, &sid, "k8", "echo", serde_json::json!({})).await;
    assert!(!is_err, "switched off and on, it runs again: {text}");
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn a_component_whose_tools_differ_from_its_manifest_is_refused() {
    let manifest = "id = \"test-mismatch\"\nname = \"Mismatch\"\nversion = \"0.1.0\"\nsummary = \"x\"\napi = \"0.1\"\n\
                    [provides]\nmcp_tools = [\"mismatch_tool\"]\n[adds]\nagents = [\"x\"]\n";
    crate::plugins::test_catalog::add(manifest, crate::plugins::test_catalog::fixture_wasm());
    let state = test_state();
    let ws = make_workspace(&state, "host-mismatch").await;
    let sid = inject_agent(&state, "k9");
    lock(&state.session_workspaces).insert(sid.clone(), ws.clone());
    lock(&state.workspaces)
        .set_plugin_on(&ws, "test-mismatch", true)
        .unwrap();
    let (_, list) = mcp_post(
        &state,
        &sid,
        "k9",
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"mismatch_tool") && !names.contains(&"echo"),
        "a refused plugin offers nothing: {names:?}"
    );
    let (is_err, text) =
        mcp_tool_call(&state, &sid, "k9", "mismatch_tool", serde_json::json!({})).await;
    assert!(
        is_err && text.contains("its manifest names [mismatch_tool]"),
        "{text}"
    );
    let (_, out) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins"),
        None,
    )
    .await;
    let card = out["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "test-mismatch")
        .unwrap()
        .clone();
    assert!(
        card["fault"].as_str().unwrap().contains("refused"),
        "{card}"
    );
    state.sessions.kill(&sid).ok();
}

#[tokio::test]
async fn an_emitted_event_is_a_plugin_frame_for_the_ui() {
    let (state, ws, sid) = fixture_workspace("host-emit", "k10").await;
    let mut mark = state.plugin_runtime.events_head();
    mcp_tool_call(
        &state,
        &sid,
        "k10",
        "echo",
        serde_json::json!({"emit": {"type": "sneaky", "built": 3}}),
    )
    .await;
    let frames = state.plugin_runtime.events_since(&mut mark);
    assert_eq!(frames.len(), 1);
    let frame: serde_json::Value = serde_json::from_str(&frames[0]).unwrap();
    assert_eq!(frame["type"], "plugin", "the host's keys win");
    assert_eq!(frame["plugin"], "test-fixture");
    assert_eq!(frame["workspace"], ws.as_str());
    assert_eq!(frame["built"], 3);
    assert!(state.plugin_runtime.events_since(&mut mark).is_empty());
    state.sessions.kill(&sid).ok();
}

#[test]
fn the_fixture_is_never_in_the_shipped_catalog() {
    crate::plugins::test_catalog::fixture();
    assert!(crate::plugins::production_catalog()
        .iter()
        .all(|m| m.id != "test-fixture"));
}
