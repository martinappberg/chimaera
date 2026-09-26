//! The Knowledge route and the mycelium plugin's two MCP tools over a fixed
//! mycelium workspace (`fixtures/living/`), pinned byte-for-byte BEFORE the
//! reader moves out of the daemon (docs/plugin-system-plan.md, P2): that port
//! must leave the daemon↔UI wire and what agents read unchanged.
//!
//! Re-bless deliberately (a real, reviewed change to the Knowledge wire):
//! `CHIMAERA_BLESS_KNOWLEDGE=1 cargo test -p chimaera-server knowledge`.

use super::support::*;
use crate::{lock, AppState};

const TREE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tests/fixtures/living");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tests/fixtures/knowledge");

/// Every copied file's mtime: the handoff's `written_ms` is serialized, so a
/// clock-dependent mtime would make the fixture unblessable.
const FIXED_MTIME_SECS: u64 = 1_789_000_000;

fn check(name: &str, actual: &str) {
    let path = std::path::Path::new(FIXTURES).join(name);
    if std::env::var_os("CHIMAERA_BLESS_KNOWLEDGE").is_some() {
        std::fs::create_dir_all(FIXTURES).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing fixture {name} — bless it first"));
    assert_eq!(
        actual, expected,
        "the Knowledge output changed ({name}); moving the reader must not change it"
    );
}

/// Recursive copy with every file's mtime pinned. `std::fs` only: the tree
/// is a handful of small regular files.
fn copy_tree(src: &std::path::Path, dst: &std::path::Path) {
    let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(FIXED_MTIME_SECS);
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&to).unwrap();
            copy_tree(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&to)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
        }
    }
}

/// A workspace holding the fixture tree, with the mycelium plugin switched
/// on through its route (which also re-detects the footprint).
async fn mycelium_workspace(state: &Arc<AppState>) -> String {
    let ws = make_workspace(state, "knowledge-fixture").await;
    let root = lock(&state.workspaces).get(&ws).unwrap().root;
    copy_tree(std::path::Path::new(TREE), &root);
    let (status, out) = request(
        state,
        Method::PUT,
        &format!("/api/v1/workspaces/{ws}/plugins/mycelium"),
        Some(serde_json::json!({"on": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    ws
}

#[tokio::test]
async fn mycelium_knowledge_route_is_unchanged() {
    let state = test_state();
    let ws = mycelium_workspace(&state).await;
    let (status, _, bytes) = request_bytes(
        &state,
        Method::GET,
        &format!("/api/v1/workspaces/{ws}/knowledge"),
        Some("test-token"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // The wire is exactly the compact form of this value, so the pretty
    // fixture below pins the response bytes, not just their meaning.
    assert_eq!(
        serde_json::to_vec(&body).unwrap(),
        bytes.as_ref(),
        "the Knowledge response is not canonical JSON"
    );

    // Nothing here is nondeterministic, so nothing is normalized: every
    // `path` is workspace-relative (the temp root never appears), no field
    // names the host, and the only mtime-derived field (`left_off.written_ms`)
    // reads FIXED_MTIME_SECS. The `claude memory` guidance entry would carry
    // an absolute path, but it exists only when ~/.claude/projects holds
    // notes for this exact (unique, per-process) temp root, which it can't.
    let text = serde_json::to_string_pretty(&body).unwrap() + "\n";
    // Guards so a bless can't capture a fixture that stopped testing the
    // reader: the provider answered, and the fenced example stayed content.
    assert_eq!(body["provider"], "mycelium", "{text}");
    assert_eq!(body["left_off"]["written_ms"], FIXED_MTIME_SECS * 1000);
    assert!(!text.contains("inside a fence"), "{text}");
    check("mycelium.json", &text);
}

#[tokio::test]
async fn mycelium_knowledge_tools_are_unchanged() {
    let state = test_state();
    let ws = mycelium_workspace(&state).await;
    let worker = inject_silent_agent(&state, "wk");
    lock(&state.session_workspaces).insert(worker.clone(), ws.clone());

    // Two words so the pin covers ranking (hits scoring 2 lead) as well as
    // the tie order among the rest.
    let (is_err, search) = mcp_tool_call(
        &state,
        &worker,
        "wk",
        "knowledge_search",
        serde_json::json!({"query": "batch qc"}),
    )
    .await;
    assert!(!is_err, "{search}");
    check("search.txt", &search);

    let (is_err, get) = mcp_tool_call(
        &state,
        &worker,
        "wk",
        "knowledge_get",
        serde_json::json!({"id": "F-001"}),
    )
    .await;
    assert!(!is_err, "{get}");
    check("get.txt", &get);

    state.sessions.kill(&worker).ok();
}
