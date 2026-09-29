//! The 0.2 plugin platform, proven with its fixture (`plugins/test-platform`,
//! built into `plugins/dist-test` by `scripts/build-plugins.sh`): screens
//! and their check, actions and file actions, the query route, declared
//! settings, durable state across a restart, output folders and Save to
//! workspace, data surfaces, file events (a save, a known write, the
//! sweep), the watch set, switched-on/off, and invalidation. A 0.1 plugin
//! keeps loading beside it (`plugin_host.rs`, the first-party releases in
//! `plugins.rs`).

use std::time::Duration;

use serde_json::{json, Value};

use super::support::*;
use crate::{lock, AppState};

const PID: &str = "test-platform";

async fn switch(state: &Arc<AppState>, ws: &str, on: bool) {
    let (status, body) = request(
        state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}"),
        Some(json!({"on": on})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A workspace with the platform fixture switched on.
async fn platform_workspace(label: &str) -> (Arc<AppState>, String) {
    crate::plugins::test_catalog::platform();
    let state = test_state();
    let ws = make_workspace(&state, label).await;
    switch(&state, &ws, true).await;
    (state, ws)
}

fn root_of(state: &Arc<AppState>, ws: &str) -> PathBuf {
    lock(&state.workspaces).get(ws).unwrap().root
}

async fn render(state: &Arc<AppState>, ws: &str, view: &str) -> Value {
    let (status, body) = request(
        state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/views/{view}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn act(state: &Arc<AppState>, ws: &str, action: &str, payload: Value) -> Value {
    let (status, body) = request(
        state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/views/board/actions"),
        Some(json!({"action": action, "payload": payload})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn query(state: &Arc<AppState>, ws: &str, name: &str) -> Value {
    let (status, body) = request(
        state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/query/{name}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["data"].clone()
}

/// The first node of `kind` in a tree, depth first.
fn find<'a>(node: &'a Value, kind: &str) -> Option<&'a Value> {
    if node["type"] == kind {
        return Some(node);
    }
    let kids = node["children"].as_array().into_iter().flatten();
    let tabs = node["tabs"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|t| t["children"].as_array().into_iter().flatten());
    kids.chain(tabs).find_map(|c| find(c, kind))
}

fn heading(tree: &Value) -> String {
    find(&tree["root"], "heading").unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Wait (≤ 5 s) until the fixture's recorded events satisfy `ok`.
async fn events_until(
    state: &Arc<AppState>,
    ws: &str,
    ok: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    let mut last = Vec::new();
    for _ in 0..50 {
        last = query(state, ws, "events")
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap().to_string())
            .collect();
        if ok(&last) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("events never matched: {last:?}");
}

#[tokio::test]
async fn a_0_2_plugin_draws_every_node_and_acts() {
    let (state, ws) = platform_workspace("platform-draw").await;
    let board = render(&state, &ws, "board").await;
    assert_eq!(board["title"], "Platform board");
    assert_eq!(board["slot"], "tab");
    let tree = &board["tree"];
    assert_eq!(tree["ui"], "1");
    assert_eq!(heading(tree), "hello (calm)");
    for kind in [
        "stack",
        "row",
        "grid",
        "split",
        "tabs",
        "section",
        "card",
        "divider",
        "text",
        "heading",
        "markdown",
        "code",
        "keyvalue",
        "badge",
        "icon",
        "progress",
        "empty",
        "callout",
        "list",
        "table",
        "file",
        "link",
        "image",
        "button",
        "toggle",
        "select",
        "textfield",
        "form",
        "editor",
        "pdf",
        "diagnostics",
        "diff",
        "log",
    ] {
        assert!(find(&tree["root"], kind).is_some(), "no {kind} node");
    }
    // The roots it asked for: the workspace and its output folder.
    let kv = find(&tree["root"], "keyvalue").unwrap();
    assert_eq!(
        kv["items"][0]["value"],
        root_of(&state, &ws).display().to_string()
    );

    // An action answers the new tree.
    let after = act(&state, &ws, "count", json!({"by": 2})).await;
    let badge = find(&after["tree"]["root"], "badge").unwrap();
    assert_eq!(badge["text"], "count 2");
    // A form's fields ride beside the payload.
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/views/board/actions"),
        Some(json!({"action": "submit", "payload": null, "form": {"name": "Ada"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = query(&state, &ws, "events").await;
    assert!(
        events.to_string().contains("Ada"),
        "the form reached the plugin: {events}"
    );

    // A tree the check refuses is not drawn, and says what to fix.
    let bad = act(&state, &ws, "bad-tree", Value::Null).await;
    assert!(bad["tree"].is_null());
    assert!(
        bad["error"].as_str().unwrap().contains("could not draw"),
        "{bad}"
    );
    assert!(
        bad["problems"][0]
            .as_str()
            .unwrap()
            .starts_with("root.label"),
        "{bad}"
    );
    // Built-in actions are the UI's, never the plugin's.
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/views/board/actions"),
        Some(json!({"action": "save-to-workspace"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Every slot draws; an undeclared view doesn't exist; a file view needs
    // a file it claims.
    for view in ["panel", "chip", "card"] {
        assert!(render(&state, &ws, view).await["tree"]["root"].is_object());
    }
    let (status, _) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/views/nope"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    std::fs::write(
        root_of(&state, &ws).join("notes.fixture"),
        "one two\nthree\n",
    )
    .unwrap();
    let (status, doc) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/views/doc?file=notes.fixture"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert!(doc["tree"].to_string().contains("one two"), "{doc}");
    let (status, _) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/views/doc?file=notes.md"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Switched off, none of it answers.
    switch(&state, &ws, false).await;
    let (status, _) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/views/board"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn file_actions_and_queries_reach_the_plugin() {
    let (state, ws) = platform_workspace("platform-actions").await;
    std::fs::write(root_of(&state, &ws).join("README.md"), "a b c d\n").unwrap();
    let (status, answer) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/file-actions/count-words"),
        Some(json!({"file": "README.md"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["message"], "README.md: 4 words");
    assert_eq!(answer["open"]["view"], "board");
    // Not a file the action matches.
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/file-actions/count-words"),
        Some(json!({"file": "notes.txt"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // A long list's next page, through the query route.
    let (status, page) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/query/page?args=%7B%22offset%22%3A3%7D"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["data"]["items"][0]["title"], "Row 4");
    assert_eq!(page["data"]["items"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn settings_are_declared_checked_and_heard() {
    let (state, ws) = platform_workspace("platform-settings").await;
    let (status, listed) = request(
        &state,
        Method::GET,
        &format!("/api/v1/plugins/{PID}/settings?workspace={ws}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let keys: Vec<&str> = listed["settings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["greeting", "loud", "mode", "limit", "main"]);
    assert_eq!(listed["settings"][0]["value"], "hello");
    assert_eq!(listed["settings"][0]["set"], false);

    let put = |key: &str, value: Value| {
        let state = state.clone();
        let ws = ws.clone();
        let key = key.to_string();
        async move {
            request(
                &state,
                Method::PUT,
                &format!("/api/v1/plugins/{PID}/settings"),
                Some(json!({"key": key, "value": value, "workspace": ws})),
            )
            .await
        }
    };
    assert_eq!(put("greeting", json!("hi")).await.0, StatusCode::OK);
    assert_eq!(put("mode", json!("busy")).await.0, StatusCode::OK);
    let board = render(&state, &ws, "board").await;
    assert_eq!(heading(&board["tree"]), "hi (busy)");
    // Checked against the declaration.
    assert_eq!(put("limit", json!(99)).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(put("mode", json!("wild")).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        put("main", json!("../etc")).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(put("undeclared", json!(1)).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(put("limit", json!(5)).await.0, StatusCode::OK);
    let values = query(&state, &ws, "state").await;
    assert_eq!(values["settings"]["limit"], 5);
    assert!(values["settings"]["undeclared"].is_null());
    // A reset goes back to the default.
    assert_eq!(put("greeting", Value::Null).await.0, StatusCode::OK);
    assert_eq!(
        query(&state, &ws, "state").await["settings"]["greeting"],
        "hello"
    );
    // The plugin heard each change.
    events_until(&state, &ws, |e| {
        e.iter().any(|x| x == "setting greeting") && e.iter().any(|x| x == "setting limit")
    })
    .await;
    // A host setting holds in every workspace; a workspace one only here.
    let other = make_workspace(&state, "platform-settings-2").await;
    switch(&state, &other, true).await;
    let there = query(&state, &other, "state").await;
    assert_eq!(there["settings"]["mode"], "busy");
    assert_eq!(there["settings"]["limit"], 3);
}

#[tokio::test]
async fn durable_state_and_switches_survive_a_restart() {
    crate::plugins::test_catalog::platform();
    let data = test_dir("platform-restart");
    let state = test_state_with_data_dir(0, data.clone());
    let ws = make_workspace(&state, "platform-restart-ws").await;
    switch(&state, &ws, true).await;
    act(&state, &ws, "keep", json!({"value": "kept-value"})).await;
    act(&state, &ws, "count", json!({"by": 1})).await;
    // switched-on is delivered off the request.
    let mut on = Value::Null;
    for _ in 0..50 {
        on = query(&state, &ws, "state").await["switched"].clone();
        if on == "on" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(on, "on");
    drop(state);

    let state = test_state_with_data_dir(0, data.clone());
    let back = query(&state, &ws, "state").await;
    assert_eq!(back["kept"], "kept-value", "{back}");
    // In-memory state starts over.
    let board = render(&state, &ws, "board").await;
    assert_eq!(
        find(&board["tree"]["root"], "badge").unwrap()["text"],
        "count 0"
    );

    // switched-off reaches the plugin before its instance goes.
    switch(&state, &ws, false).await;
    let kept = std::fs::read_to_string(
        data.join("plugins/.data/test-platform")
            .join(format!("{ws}.json")),
    )
    .unwrap();
    assert!(kept.contains(r#""switched":"\"off\"""#), "{kept}");
}

#[tokio::test]
async fn output_folders_stay_confined_and_save_only_on_a_click() {
    let (state, ws) = platform_workspace("platform-output").await;
    act(&state, &ws, "write", Value::Null).await;
    let events = events_until(&state, &ws, |e| e.iter().any(|x| x.starts_with("wrote"))).await;
    assert!(
        events.contains(&"wrote from hello.txt".to_string()),
        "{events:?}"
    );
    let (status, folder) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/output"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let root = PathBuf::from(folder["root"].as_str().unwrap());
    assert!(root.join("hello.txt").is_file());
    assert!(
        !root.starts_with(root_of(&state, &ws)),
        "outside the repository"
    );

    // Save to workspace: the user's click, never over a file unasked.
    let save = |replace: bool| {
        let state = state.clone();
        let ws = ws.clone();
        async move {
            request(
                &state,
                Method::POST,
                &format!("/api/v1/workspaces/{ws}/plugins/{PID}/output/save"),
                Some(
                    json!({"from": "output:hello.txt", "to": "out/hello.txt", "replace": replace}),
                ),
            )
            .await
        }
    };
    let (status, saved) = save(false).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        std::fs::read_to_string(root_of(&state, &ws).join("out/hello.txt")).unwrap(),
        "hello from the output folder\n"
    );
    assert_eq!(save(false).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(save(true).await.0, StatusCode::OK);
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{ws}/plugins/{PID}/output/save"),
        Some(json!({"from": "output:../../x", "to": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Use and Clear.
    let (_, used) = request(
        &state,
        Method::GET,
        &format!("/api/v1/plugins/{PID}/output"),
        None,
    )
    .await;
    assert!(used["bytes"].as_u64().unwrap() > 0, "{used}");
    let (status, cleared) = request(
        &state,
        Method::DELETE,
        &format!("/api/v1/plugins/{PID}/output"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["bytes"], 0);
    assert!(!root.join("hello.txt").exists());
}

#[tokio::test]
async fn surfaces_are_checked_scoped_and_announced() {
    let (state, ws) = platform_workspace("platform-surfaces").await;
    let other = make_workspace(&state, "platform-surfaces-2").await;
    switch(&state, &other, true).await;
    let mut mark = state.plugin_runtime.events_head();
    act(&state, &ws, "publish", Value::Null).await;
    let (status, diags) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/surfaces/diagnostics/1?file=notes.fixture"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = &diags["items"][0];
    assert_eq!(items["plugin"], PID);
    assert_eq!(items["data"]["items"].as_array().unwrap().len(), 2);
    let (_, output) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/surfaces/output/1?file=notes.fixture"),
        None,
    )
    .await;
    assert_eq!(output["items"][0]["data"]["state"], "ok");
    // Ids it answers for: what the client's reference registry links.
    let (_, refs) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/surfaces/references/1"),
        None,
    )
    .await;
    assert_eq!(refs["items"][0]["plugin"], PID);
    assert_eq!(refs["items"][0]["data"]["shapes"][0]["pattern"], "FX-\\d+");
    assert_eq!(refs["items"][0]["data"]["ids"][1]["view"], "board");
    // Another file, another workspace: nothing.
    let (_, none) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/surfaces/diagnostics/1?file=main.tex"),
        None,
    )
    .await;
    assert_eq!(none["items"], json!([]));
    let (_, none) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{other}/surfaces/diagnostics/1"),
        None,
    )
    .await;
    assert_eq!(none["items"], json!([]));
    let (status, _) = request(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/surfaces/colors/1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Windows on this workspace were told (and asked to re-render the
    // board); a window on the other one wasn't.
    let frames: Vec<Value> = state
        .plugin_runtime
        .events_since(&mut mark.clone(), Some(&ws))
        .iter()
        .map(|f| serde_json::from_str(f).unwrap())
        .collect();
    assert!(frames
        .iter()
        .any(|f| f["type"] == "surface" && f["surface"] == "diagnostics/1"));
    assert!(frames
        .iter()
        .any(|f| f["type"] == "view" && f["view"] == "board"));
    assert!(state
        .plugin_runtime
        .events_since(&mut mark, Some(&other))
        .is_empty());
}

#[tokio::test]
async fn invalidations_coalesce_to_four_a_second() {
    let (state, ws) = platform_workspace("platform-invalidate").await;
    let m = crate::plugins::manifest(&state, PID).unwrap();
    let mut mark = state.plugin_runtime.events_head();
    for _ in 0..20 {
        crate::plugins::screens::invalidate(&state, &m, &ws, "board");
    }
    // An undeclared view is ignored.
    crate::plugins::screens::invalidate(&state, &m, &ws, "nope");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let frames = state.plugin_runtime.events_since(&mut mark, Some(&ws));
    assert_eq!(frames.len(), 2, "one now, one for the burst: {frames:?}");
}

#[tokio::test]
async fn file_events_are_debounced_and_reach_only_claimed_or_watched_files() {
    let (state, ws) = platform_workspace("platform-files").await;
    let root = root_of(&state, &ws);
    let notes = root.join("notes.fixture");
    std::fs::write(&notes, "v0\n").unwrap();
    // Three quick saves from the editor: one `file-saved`.
    for i in 1..=3 {
        let (status, _, _) = put_raw(
            &state,
            &format!("/api/v1/fs/file?path={}", notes.display()),
            format!("v{i}\n").into_bytes(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let events = events_until(&state, &ws, |e| {
        e.iter().any(|x| x == "saved notes.fixture")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let events_after = query(&state, &ws, "events").await;
    assert_eq!(
        events_after
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e.as_str().unwrap().contains("notes.fixture"))
            .count(),
        1,
        "{events:?} / {events_after}"
    );
    // A known write (an agent's, a file operation) to a claimed file:
    // `file-changed`.
    std::fs::write(&notes, "agent\n").unwrap();
    crate::git::mark_path_dirty(&state, &notes.to_string_lossy()).await;
    events_until(&state, &ws, |e| {
        e.iter().any(|x| x == "changed notes.fixture")
    })
    .await;
    // A file it neither claims nor watches: nothing.
    std::fs::write(root.join("other.txt"), "x").unwrap();
    crate::git::mark_path_dirty(&state, &root.join("other.txt").to_string_lossy()).await;
    // Watched: heard. And the sweep sees a write no one announced, while a
    // view is open.
    act(&state, &ws, "watch", json!({"paths": ["watched.txt"]})).await;
    std::fs::write(root.join("watched.txt"), "one").unwrap();
    crate::git::mark_path_dirty(&state, &root.join("watched.txt").to_string_lossy()).await;
    events_until(&state, &ws, |e| {
        e.iter().any(|x| x == "changed watched.txt")
    })
    .await;
    render(&state, &ws, "board").await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    std::fs::write(root.join("watched.txt"), "two, longer").unwrap();
    let events = events_until(&state, &ws, |e| {
        e.iter().filter(|x| *x == "changed watched.txt").count() >= 2
    })
    .await;
    assert!(
        !events.iter().any(|x| x.contains("other.txt")),
        "{events:?}"
    );
}
