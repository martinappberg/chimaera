//! Agent <-> terminal links: the user-granted edges behind linked terminals,
//! and agent <-> browser-tab links: panes the user shares with an agent's
//! browser tools (`agent_browser`).
//!
//! One agent per terminal — a re-link moves the leash — while an agent may
//! hold many terminals (enforced by the map shape: terminal id is the key).
//! Links are the agent's whole access scope: its MCP tools see exactly the
//! terminals linked here, nothing else. They are granted by the user (UI
//! action or an `@term:` mention in the composer), never by the agent, and
//! live in memory only: a link dies with either session, and sessions die
//! with the daemon.
//!
//! Browser links work the same way, keyed by the window's browser tab id
//! (the daemon holds no tabs; a window lists the ones it holds in its
//! `browser_hello`). One agent per tab, many tabs per agent; each gets a
//! short page id the agent addresses it by (`p1`, `p2`, … per agent, stable
//! while linked). A link dies with its agent or when the user unlinks it
//! (the window does that when the tab closes).

use std::collections::HashMap;
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
        state.sessions.get(terminal).is_some() && crate::chat::session_alive(state, agent)
    });
    let mut entries: Vec<(&String, &String)> = links.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(terminal, agent)| link_json(terminal, agent))
        .collect()
}

/// One agent↔terminal edge as the sessions frame and `GET /links` carry it.
pub(crate) fn link_json(terminal_id: &str, agent_id: &str) -> serde_json::Value {
    json!({"terminal_id": terminal_id, "agent_id": agent_id})
}

/// Terminal ids linked to `agent_id` (the agent's MCP access scope), pruned
/// of dead sessions, in stable order.
pub(crate) fn terminals_of(state: &AppState, agent_id: &str) -> Vec<String> {
    let mut links = crate::lock(&state.links);
    links.retain(|terminal, agent| {
        state.sessions.get(terminal).is_some() && crate::chat::session_alive(state, agent)
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
    if !crate::chat::session_alive(state, agent_id) {
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

// ---- browser links -------------------------------------------------------

/// Browser links kept at once (each is a pane a person shared by hand).
const BROWSER_LINKS_CAP: usize = 64;

/// One shared browser tab: the agent it is linked to and the page id that
/// agent addresses it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserLink {
    pub(crate) agent_id: String,
    pub(crate) page: String,
}

/// A window's browser tab id: short, opaque, URL-safe.
pub(crate) fn valid_tab_id(tab: &str) -> bool {
    !tab.is_empty()
        && tab.len() <= crate::agent_browser::channel::TAB_ID_MAX
        && tab
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

fn prune_browser(state: &AppState, links: &mut HashMap<String, BrowserLink>) {
    links.retain(|_, link| crate::chat::session_alive(state, &link.agent_id));
}

/// The live browser links as the sessions frame carries them, in tab order.
pub(crate) fn browser_links_json(state: &AppState) -> Vec<serde_json::Value> {
    let mut links = crate::lock(&state.browser_links);
    prune_browser(state, &mut links);
    let mut entries: Vec<(&String, &BrowserLink)> = links.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(tab, link)| browser_link_json(tab, &link.agent_id, &link.page))
        .collect()
}

/// One agent↔browser-tab edge (sessions frame, `GET /links/browser`).
pub(crate) fn browser_link_json(tab: &str, agent_id: &str, page: &str) -> serde_json::Value {
    json!({"tab": tab, "agent_id": agent_id, "page": page})
}

/// The panes shared with `agent_id`: (tab, page id), in page order.
pub(crate) fn panes_of(state: &AppState, agent_id: &str) -> Vec<(String, String)> {
    let mut links = crate::lock(&state.browser_links);
    prune_browser(state, &mut links);
    let mut out: Vec<(String, String)> = links
        .iter()
        .filter(|(_, link)| link.agent_id == agent_id)
        .map(|(tab, link)| (tab.clone(), link.page.clone()))
        .collect();
    out.sort_by_key(|(_, page)| page_number(page));
    out
}

/// The link of `tab`, if it is shared (with any agent).
pub(crate) fn browser_link_of(state: &AppState, tab: &str) -> Option<BrowserLink> {
    let mut links = crate::lock(&state.browser_links);
    prune_browser(state, &mut links);
    links.get(tab).cloned()
}

fn page_number(page: &str) -> u32 {
    page.strip_prefix('p')
        .and_then(|n| n.parse().ok())
        .unwrap_or(u32::MAX)
}

/// Share browser tab `tab` with `agent_id` (a re-link moves it). Returns
/// the page id the agent addresses it by and the agent it moved from.
pub(crate) fn link_browser(
    state: &AppState,
    tab: &str,
    agent_id: &str,
) -> Result<(String, Option<String>), (StatusCode, String)> {
    if !valid_tab_id(tab) {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid browser tab id".to_string(),
        ));
    }
    if !crate::chat::session_alive(state, agent_id)
        || !crate::lock(&state.agents).contains_key(agent_id)
    {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown agent session {agent_id}"),
        ));
    }
    let (page, moved_from) = {
        let mut links = crate::lock(&state.browser_links);
        prune_browser(state, &mut links);
        match links.get(tab) {
            Some(link) if link.agent_id == agent_id => return Ok((link.page.clone(), None)),
            Some(_) => {}
            None if links.len() >= BROWSER_LINKS_CAP => {
                return Err((
                    StatusCode::CONFLICT,
                    format!("at most {BROWSER_LINKS_CAP} browser tabs can be linked at once"),
                ))
            }
            None => {}
        }
        let used: std::collections::HashSet<u32> = links
            .values()
            .filter(|l| l.agent_id == agent_id)
            .map(|l| page_number(&l.page))
            .collect();
        let n = (1..).find(|n| !used.contains(n)).unwrap_or(1);
        let page = format!("p{n}");
        let old = links.insert(
            tab.to_string(),
            BrowserLink {
                agent_id: agent_id.to_string(),
                page: page.clone(),
            },
        );
        (page, old.map(|l| l.agent_id))
    };
    state.browsers.forget_shared(tab);
    state.changes.notify_waiters();
    Ok((page, moved_from))
}

/// Unshare `tab` (idempotent). Returns whether a link was removed.
pub(crate) fn unlink_browser(state: &AppState, tab: &str) -> bool {
    let removed = crate::lock(&state.browser_links).remove(tab).is_some();
    if removed {
        state.browsers.forget_shared(tab);
        state.changes.notify_waiters();
    }
    removed
}

/// GET /api/v1/links/browser
pub(crate) async fn list_browser_links(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    Json(serde_json::Value::Array(browser_links_json(&state)))
}

#[derive(Deserialize)]
pub(crate) struct PutBrowserLink {
    tab: String,
    agent_id: String,
}

/// PUT /api/v1/links/browser — share a browser tab with an agent (a re-link
/// moves it).
pub(crate) async fn put_browser_link(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PutBrowserLink>,
) -> Response {
    match link_browser(&state, &body.tab, &body.agent_id) {
        Ok((page, moved_from)) => Json(json!({
            "tab": body.tab,
            "agent_id": body.agent_id,
            "page": page,
            "moved_from": moved_from,
        }))
        .into_response(),
        Err((code, message)) => (code, Json(json!({"error": message}))).into_response(),
    }
}

/// DELETE /api/v1/links/browser/{tab} — unshare (idempotent).
pub(crate) async fn delete_browser_link(
    State(state): State<Arc<AppState>>,
    Path(tab): Path<String>,
) -> Response {
    unlink_browser(&state, &tab);
    StatusCode::NO_CONTENT.into_response()
}
