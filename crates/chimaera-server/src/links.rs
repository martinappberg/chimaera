//! Agent <-> terminal links: the user-granted edges behind linked terminals.
//!
//! One agent per terminal — a re-link moves the leash — while an agent may
//! hold many terminals (enforced by the map shape: terminal id is the key).
//! Links are the agent's whole access scope: its MCP tools see exactly the
//! terminals linked here, nothing else. They are granted by the user (UI
//! action or an `@term:` mention in the composer), never by the agent, and
//! live in memory only: a link dies with either session, and sessions die
//! with the daemon.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

/// The live link list as JSON, pruning edges whose sessions are gone.
/// Stable terminal-id order so /ws/events snapshot dedup doesn't flap.
pub(crate) fn links_json(state: &AppState) -> Vec<serde_json::Value> {
    let mut links = crate::lock(&state.links);
    links.retain(|terminal, agent| {
        state.sessions.get(terminal).is_some() && state.sessions.get(agent).is_some()
    });
    let mut entries: Vec<(&String, &String)> = links.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(terminal, agent)| json!({"terminal_id": terminal, "agent_id": agent}))
        .collect()
}

/// Terminal ids linked to `agent_id` (the agent's MCP access scope), pruned
/// of dead sessions, in stable order.
pub(crate) fn terminals_of(state: &AppState, agent_id: &str) -> Vec<String> {
    let mut links = crate::lock(&state.links);
    links.retain(|terminal, agent| {
        state.sessions.get(terminal).is_some() && state.sessions.get(agent).is_some()
    });
    let mut out: Vec<String> = links
        .iter()
        .filter(|(_, agent)| agent.as_str() == agent_id)
        .map(|(terminal, _)| terminal.clone())
        .collect();
    out.sort();
    out
}

/// Create or move a link, with full validation. Used by the REST handler
/// and by the `@term:` mention auto-linker. Returns the agent the terminal
/// was previously linked to, if it moved.
pub(crate) fn link(
    state: &AppState,
    terminal_id: &str,
    agent_id: &str,
) -> Result<Option<String>, (StatusCode, String)> {
    if state.sessions.get(agent_id).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown agent session {agent_id}"),
        ));
    }
    if state.sessions.get(terminal_id).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown terminal session {terminal_id}"),
        ));
    }
    let agents = crate::lock(&state.agents);
    if !agents.contains_key(agent_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{agent_id} is not an agent session"),
        ));
    }
    if agents.contains_key(terminal_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{terminal_id} is an agent session, not a terminal"),
        ));
    }
    drop(agents);

    let moved_from = {
        let mut links = crate::lock(&state.links);
        let old = links.insert(terminal_id.to_string(), agent_id.to_string());
        old.filter(|previous| previous != agent_id)
    };
    state.changes.notify_waiters();
    Ok(moved_from)
}

/// GET /api/v1/links
pub(crate) async fn list_links(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::Value::Array(links_json(&state)))
}

#[derive(Deserialize)]
pub(crate) struct PutLink {
    terminal_id: String,
    agent_id: String,
}

/// PUT /api/v1/links — link a terminal to an agent (re-link moves it).
pub(crate) async fn put_link(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PutLink>,
) -> Response {
    match link(&state, &body.terminal_id, &body.agent_id) {
        Ok(moved_from) => Json(json!({
            "terminal_id": body.terminal_id,
            "agent_id": body.agent_id,
            "moved_from": moved_from,
        }))
        .into_response(),
        Err((code, message)) => (code, Json(json!({"error": message}))).into_response(),
    }
}

/// DELETE /api/v1/links/{terminal_id} — unlink (idempotent).
pub(crate) async fn delete_link(
    State(state): State<Arc<AppState>>,
    Path(terminal_id): Path<String>,
) -> Response {
    let removed = crate::lock(&state.links).remove(&terminal_id).is_some();
    if removed {
        state.changes.notify_waiters();
    }
    StatusCode::NO_CONTENT.into_response()
}
