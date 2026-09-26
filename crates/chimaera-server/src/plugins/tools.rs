//! The MCP tools workbench plugins contribute. Named in each manifest's
//! `provides.mcp_tools`; served by `mcp.rs` ONLY to sessions whose workspace
//! has the plugin active (tools/list AND the call gate), with the plugin's
//! instruction paragraph appended at initialize.
//!
//! Generic: every plugin answers through its component's exports
//! (`runtime`), the definitions checked against its manifest there.

use std::sync::Arc;

use serde_json::{json, Value};

use super::Manifest;
use crate::AppState;

/// The plugin that owns an MCP tool name, if any: an `active` one first
/// (two plugins may name the same tool; the one switched on here answers),
/// else any in the catalog (for the "isn't switched on" refusal).
pub(crate) fn owner(
    state: &AppState,
    active: &[Arc<Manifest>],
    tool: &str,
) -> Option<Arc<Manifest>> {
    let owns = |m: &&Arc<Manifest>| m.provides.mcp_tools.iter().any(|t| t == tool);
    if let Some(m) = active.iter().find(owns) {
        return Some(m.clone());
    }
    super::catalog(state).iter().find(owns).cloned()
}

/// What the active `plugins` add to what an agent in `ws` is handed: their
/// instruction paragraphs and their tool definitions, in catalog order. A
/// plugin that can't answer (refused, faulted, trapping) adds nothing.
pub(crate) async fn offered(
    state: &Arc<AppState>,
    plugins: &[Arc<Manifest>],
    ws: &str,
) -> (Vec<String>, Vec<Value>) {
    let mut paragraphs = Vec::new();
    let mut tools = Vec::new();
    for m in plugins {
        match state.plugin_runtime.offer(state, m, ws).await {
            Ok(offer) => {
                paragraphs.extend(offer.instructions);
                tools.extend(offer.tools);
            }
            Err(err) => tracing::warn!(plugin = %m.id, %err, "plugin offers nothing"),
        }
    }
    (paragraphs, tools)
}

/// Dispatch a plugin tool (the caller already checked `m` is active in the
/// session's workspace).
pub(crate) async fn call(
    state: &Arc<AppState>,
    m: &Manifest,
    agent_id: &str,
    name: &str,
    args: &Value,
) -> Value {
    let Some(ws) = super::workspace_of_session(state, agent_id) else {
        return json!({
            "content": [{ "type": "text", "text": "this session has no workspace" }],
            "isError": true,
        });
    };
    state
        .plugin_runtime
        .call_tool(state, m, &ws, agent_id, name, args)
        .await
}
