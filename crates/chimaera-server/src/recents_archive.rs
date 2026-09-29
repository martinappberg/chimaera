//! Archived Recents: conversations the user hid from the rail's Recents
//! ("Archive", "Archive all"). Archiving only HIDES — it never deletes a
//! transcript, a chat journal or a session record, and never touches
//! claude's or codex's own files. The conversations stay in All sessions
//! under "Archived", where "Unarchive" brings them back.
//!
//! Keyed by the identity Recents already uses: the native conversation id
//! (claude session id / codex thread id), or `~kind:title` for a handle-less
//! row. A conversation's ancestor ids (`supersedes`: older claude CLIs forked
//! a new id per resume) are archived with it, so it can't reappear under an
//! id of its own chain. Small capped JSON per workspace
//! (`<data_dir>/recents-archive.json`, ≤ `CAP_PER_WORKSPACE` entries, the
//! oldest dropped), rewritten atomically off the reactor; removed with the
//! workspace. Every change bumps the recents epoch, so every window's rail
//! and All sessions refetch.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;

/// Archived conversations kept per workspace; past it the oldest go (they
/// simply show in Recents again, if they are still among its 20).
pub(crate) const CAP_PER_WORKSPACE: usize = 2000;
const KEY_MAX: usize = 256;
const TITLE_MAX: usize = 300;
const BATCH_MAX: usize = 500;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Archived {
    /// The conversation's identity in Recents.
    pub(crate) key: String,
    pub(crate) kind: String,
    pub(crate) title: String,
    /// Its native id when it has one (what a resume would pass).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) resume: Option<String>,
    /// The surface it last ran on (`chat` | `term`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ui: Option<String>,
    /// When it was archived, unix seconds.
    pub(crate) at: u64,
    /// Ancestor ids of the same conversation, hidden with it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) supersedes: Vec<String>,
}

/// The identity a Recents row is archived under.
pub(crate) fn key_of(resume: Option<&str>, kind: &str, title: &str) -> String {
    match resume {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => format!("~{kind}:{title}"),
    }
}

pub(crate) struct ArchiveStore {
    path: PathBuf,
    /// workspace id -> archived, oldest first.
    items: HashMap<String, Vec<Archived>>,
}

impl ArchiveStore {
    /// Load-tolerant like every store: missing or corrupt is empty.
    pub(crate) fn load(path: PathBuf) -> Self {
        let items = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, Vec<Archived>>>(&s).ok())
            .unwrap_or_default();
        ArchiveStore { path, items }
    }

    /// Every id a workspace's archive hides: the keys and their ancestors.
    pub(crate) fn hidden(&self, ws: &str) -> HashSet<String> {
        self.items
            .get(ws)
            .into_iter()
            .flatten()
            .flat_map(|a| std::iter::once(a.key.clone()).chain(a.supersedes.iter().cloned()))
            .collect()
    }

    /// Newest first.
    pub(crate) fn list(&self, ws: &str) -> Vec<Archived> {
        let mut out = self.items.get(ws).cloned().unwrap_or_default();
        out.reverse();
        out
    }

    /// Archive (re-archiving moves an entry to the newest end). Returns the
    /// keys now archived.
    pub(crate) fn add(&mut self, ws: &str, entries: Vec<Archived>) -> Vec<String> {
        let list = self.items.entry(ws.to_string()).or_default();
        let mut added = Vec::with_capacity(entries.len());
        for entry in entries {
            list.retain(|a| a.key != entry.key);
            added.push(entry.key.clone());
            list.push(entry);
        }
        if list.len() > CAP_PER_WORKSPACE {
            let drop = list.len() - CAP_PER_WORKSPACE;
            list.drain(..drop);
        }
        added
    }

    /// Unarchive by key; returns how many came back.
    pub(crate) fn remove(&mut self, ws: &str, keys: &HashSet<String>) -> usize {
        let Some(list) = self.items.get_mut(ws) else {
            return 0;
        };
        let before = list.len();
        list.retain(|a| !keys.contains(&a.key));
        let removed = before - list.len();
        if list.is_empty() {
            self.items.remove(ws);
        }
        removed
    }

    pub(crate) fn forget_workspace(&mut self, ws: &str) -> bool {
        self.items.remove(ws).is_some()
    }

    fn snapshot(&self) -> (PathBuf, Vec<u8>) {
        (
            self.path.clone(),
            serde_json::to_vec(&self.items).unwrap_or_else(|_| b"{}".to_vec()),
        )
    }
}

/// Write the store (atomic, off the reactor). Writes serialize on
/// `recents_archive_write`, and each takes its snapshot under it, so the
/// last change is the one on disk.
pub(crate) async fn persist(state: &Arc<AppState>) {
    let _write = state.recents_archive_write.lock().await;
    let (path, body) = crate::lock(&state.recents_archive).snapshot();
    let written =
        tokio::task::spawn_blocking(move || crate::persist::atomic_write_json(&path, body)).await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::warn!(%err, "failed to persist archived recents"),
        Err(err) => tracing::warn!(%err, "archived recents write task failed"),
    }
}

fn changed(state: &AppState) {
    state
        .recents_epoch
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state.changes.notify_waiters();
}

fn bad(msg: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response()
}

fn unknown_workspace(state: &AppState, ws: &str) -> Option<Response> {
    crate::lock(&state.workspaces).get(ws).is_none().then(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {ws}")})),
        )
            .into_response()
    })
}

#[derive(Deserialize)]
pub(crate) struct ArchiveEntry {
    key: String,
    kind: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    resume: Option<String>,
    #[serde(default)]
    ui: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ArchiveBody {
    workspace_id: String,
    entries: Vec<ArchiveEntry>,
}

/// POST /recents/archive {workspace_id, entries: [{key, kind, title, resume?,
/// ui?}]} — hide these Recents rows (one, or "Archive all": every row the
/// rail lists). Their ancestor ids ride along from the Recents store.
pub(crate) async fn archive(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ArchiveBody>,
) -> Response {
    if let Some(resp) = unknown_workspace(&state, &body.workspace_id) {
        return resp;
    }
    if body.entries.is_empty() || body.entries.len() > BATCH_MAX {
        return bad("entries must hold 1 to 500 conversations");
    }
    if body
        .entries
        .iter()
        .any(|e| e.key.is_empty() || e.key.len() > KEY_MAX || e.kind.len() > 32)
    {
        return bad("invalid conversation key");
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ancestors = crate::recents::ancestors(&state, &body.workspace_id);
    let entries: Vec<Archived> = body
        .entries
        .into_iter()
        .map(|e| Archived {
            supersedes: e
                .resume
                .as_deref()
                .and_then(|r| ancestors.get(r).cloned())
                .unwrap_or_default(),
            title: crate::timeline::cap(&e.title, TITLE_MAX),
            resume: e.resume.filter(|r| !r.is_empty() && r.len() <= KEY_MAX),
            ui: e.ui.filter(|u| u == "chat" || u == "term"),
            key: e.key,
            kind: e.kind,
            at: now,
        })
        .collect();
    let archived = crate::lock(&state.recents_archive).add(&body.workspace_id, entries);
    persist(&state).await;
    changed(&state);
    Json(json!({"archived": archived})).into_response()
}

#[derive(Deserialize)]
pub(crate) struct UnarchiveBody {
    workspace_id: String,
    keys: Vec<String>,
}

/// POST /recents/unarchive {workspace_id, keys} — bring them back (the
/// Archived filter's action, and "Archive all"'s Undo).
pub(crate) async fn unarchive(
    State(state): State<Arc<AppState>>,
    Json(body): Json<UnarchiveBody>,
) -> Response {
    if let Some(resp) = unknown_workspace(&state, &body.workspace_id) {
        return resp;
    }
    if body.keys.len() > BATCH_MAX {
        return bad("at most 500 keys at a time");
    }
    let keys: HashSet<String> = body.keys.into_iter().collect();
    let n = crate::lock(&state.recents_archive).remove(&body.workspace_id, &keys);
    if n > 0 {
        persist(&state).await;
        changed(&state);
    }
    Json(json!({"unarchived": n})).into_response()
}

#[derive(Deserialize)]
pub(crate) struct ArchivedQuery {
    workspace_id: String,
}

/// GET /recents/archived?workspace_id= — the workspace's archived
/// conversations, newest first (All sessions' "Archived" filter).
pub(crate) async fn list_archived(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ArchivedQuery>,
) -> Response {
    if let Some(resp) = unknown_workspace(&state, &query.workspace_id) {
        return resp;
    }
    let archived = crate::lock(&state.recents_archive).list(&query.workspace_id);
    Json(json!({"archived": archived})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str) -> Archived {
        Archived {
            key: key.into(),
            kind: "claude".into(),
            title: key.into(),
            resume: Some(key.into()),
            ui: None,
            at: 1,
            supersedes: vec![],
        }
    }

    #[test]
    fn keys_fall_back_for_handle_less_rows() {
        assert_eq!(key_of(Some("abc"), "claude", "t"), "abc");
        assert_eq!(key_of(None, "codex", "port it"), "~codex:port it");
        assert_eq!(key_of(Some(""), "codex", "x"), "~codex:x");
    }

    #[test]
    fn the_cap_drops_the_oldest_and_rearchiving_moves_to_newest() {
        let dir = std::env::temp_dir().join(format!("chimaera-archive-{}", std::process::id()));
        let mut store = ArchiveStore::load(dir.join("a.json"));
        let many: Vec<Archived> = (0..CAP_PER_WORKSPACE + 5)
            .map(|i| entry(&format!("k{i}")))
            .collect();
        store.add("w", many);
        let hidden = store.hidden("w");
        assert_eq!(hidden.len(), CAP_PER_WORKSPACE);
        assert!(!hidden.contains("k0"), "the oldest went");
        assert!(hidden.contains(&format!("k{}", CAP_PER_WORKSPACE + 4)));
        store.add("w", vec![entry("k10")]);
        assert_eq!(store.list("w")[0].key, "k10", "newest first");
        let mut with_chain = entry("new-id");
        with_chain.supersedes = vec!["old-id".into()];
        store.add("w", vec![with_chain]);
        assert!(store.hidden("w").contains("old-id"), "ancestors hide too");
        let gone: HashSet<String> = ["new-id".to_string()].into();
        assert_eq!(store.remove("w", &gone), 1);
        assert!(!store.hidden("w").contains("old-id"));
        assert!(store.forget_workspace("w"));
        assert!(store.hidden("w").is_empty());
    }
}
