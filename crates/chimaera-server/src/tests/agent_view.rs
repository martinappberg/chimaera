//! "Core never changes what agents see" (timeline-knowledge-plugins plan
//! §10), pinned. Everything a WORKER agent is handed — the MCP tool list,
//! the initialize instructions (plain and supervised), the generated claude
//! `--settings`, the codex chat argv — is compared byte-for-byte against
//! fixtures captured BEFORE the Timeline/Knowledge/Plugins work touched
//! `mcp.rs`. Only a workspace with a plugin switched on may differ, and only
//! by exactly what that plugin's card says it adds.
//!
//! Re-bless deliberately (a real, reviewed change to the worker view):
//! `CHIMAERA_BLESS_AGENT_VIEW=1 cargo test -p chimaera-server agent_view`.

use super::support::*;
use crate::{lock, AppState};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tests/fixtures/agent_view");

fn check(name: &str, actual: &str) {
    let path = std::path::Path::new(FIXTURES).join(name);
    if std::env::var_os("CHIMAERA_BLESS_AGENT_VIEW").is_some() {
        std::fs::create_dir_all(FIXTURES).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing fixture {name} — bless it first"));
    assert_eq!(
        actual, expected,
        "the worker agent view changed ({name}); core must never change what agents see"
    );
}

async fn rpc(state: &Arc<AppState>, sid: &str, key: &str, method: &str) -> serde_json::Value {
    let (status, out) = mcp_post(
        state,
        sid,
        key,
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method,
            "params": {"protocolVersion": "2025-06-18"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    out["result"].clone()
}

fn pretty(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).unwrap() + "\n"
}

#[tokio::test]
async fn worker_mcp_view_is_unchanged() {
    let state = test_state();
    let ws = make_workspace(&state, "agent-view").await;
    let worker = inject_agent(&state, "wk");
    lock(&state.session_workspaces).insert(worker.clone(), ws.clone());

    check(
        "worker_tools.json",
        &pretty(&rpc(&state, &worker, "wk", "tools/list").await),
    );
    let init = rpc(&state, &worker, "wk", "initialize").await;
    check(
        "worker_instructions.txt",
        init["instructions"].as_str().unwrap(),
    );

    // Supervised: the same worker once its workspace has a Mastermind.
    let mm = inject_agent(&state, "mmk");
    lock(&state.session_workspaces).insert(mm.clone(), ws.clone());
    lock(&state.workspaces)
        .set_mastermind(
            &ws,
            Some(crate::workspaces::MastermindCfg {
                session_id: mm.clone(),
                mode: crate::workspaces::MastermindMode::Ask,
                agent: "claude".to_string(),
            }),
        )
        .unwrap();
    check(
        "supervised_tools.json",
        &pretty(&rpc(&state, &worker, "wk", "tools/list").await),
    );
    let init = rpc(&state, &worker, "wk", "initialize").await;
    check(
        "supervised_instructions.txt",
        init["instructions"].as_str().unwrap(),
    );

    // A plugin switched on WITHOUT its footprint present changes nothing.
    lock(&state.workspaces)
        .set_plugin_on(&ws, "mycelium", true)
        .unwrap();
    check(
        "supervised_tools.json",
        &pretty(&rpc(&state, &worker, "wk", "tools/list").await),
    );

    state.sessions.kill(&worker).ok();
    state.sessions.kill(&mm).ok();
}

#[test]
fn worker_generated_settings_and_codex_argv_are_unchanged() {
    let path = crate::agents::write_settings("s-agentview", "KEY", 4242, Some("dark"), None, None)
        .expect("write settings");
    let body = std::fs::read_to_string(&path).unwrap();
    // Normalize the per-host bits (the runtime dir) so the fixture is portable.
    let runtime = chimaera_core::runtime_dir();
    let body = body.replace(&runtime.to_string_lossy().to_string(), "<RUNTIME>");
    check("worker_settings.json", &body);
    let _ = std::fs::remove_file(path);

    let argv = crate::launcher::build_codex_chat_command(
        std::path::Path::new("/bin/codex"),
        Some("http://127.0.0.1:4242/api/v1/mcp/s-agentview"),
        None,
    );
    check("codex_argv.json", &pretty(&serde_json::json!(argv)));
}
