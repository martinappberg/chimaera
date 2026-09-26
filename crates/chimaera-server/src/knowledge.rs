//! Knowledge: a read-only view over what agents record, through providers
//! (mycelium today) plus the guidance and memory files agents already keep.
//! Chimaera never writes knowledge (plan decision 3).

use std::sync::Arc;

use crate::timeline::Recorded;
use crate::AppState;

/// What the workspace's knowledge provider gained since the last check,
/// attributed to the episode that just ended in `sid` — only when that
/// attribution is unambiguous (see the provider notes). None until a
/// structured provider is active.
pub(crate) async fn recorded_since_last_check(
    _state: &Arc<AppState>,
    _ws: &str,
    _sid: &str,
) -> Option<Recorded> {
    None
}

fn tool_error(t: String) -> serde_json::Value {
    serde_json::json!({ "content": [{ "type": "text", "text": t }], "isError": true })
}

/// knowledge_search {query, limit?} — lands with the mycelium reader.
pub(crate) async fn tool_search(
    _state: &Arc<AppState>,
    _sid: &str,
    _args: &serde_json::Value,
) -> serde_json::Value {
    tool_error("knowledge search is not available yet".into())
}

/// knowledge_get {id} — lands with the mycelium reader.
pub(crate) async fn tool_get(
    _state: &Arc<AppState>,
    _sid: &str,
    _args: &serde_json::Value,
) -> serde_json::Value {
    tool_error("knowledge reading is not available yet".into())
}
