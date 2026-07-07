//! Rail "Recents": daemon-persisted, per-workspace history of ENDED agent
//! conversations (DESIGN.md "The agent launcher" — Rail Recents). When an
//! agent session's PTY dies, its record retires here instead of vanishing,
//! so the conversation can still be found — and resumed where the CLI
//! supports it (`claude --resume`; codex/gemini restart fresh until their
//! resume story is verified against real binaries).
//!
//! Live sessions never appear here: entries whose conversation is running
//! again (resumed in some session) are hidden at read time, not deleted —
//! they come back when that session ends.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::agents::AgentKind;
use crate::AppState;

/// Entries kept per workspace (the UI shows ~3 collapsed, the rest scroll).
const CAP_PER_WORKSPACE: usize = 20;

/// One ended agent conversation.
#[derive(Clone, Debug)]
pub(crate) struct RecentEntry {
    /// Which agent CLI ran it (drives the rail glyph).
    pub(crate) kind: AgentKind,
    pub(crate) title: String,
    /// Claude session id (`--resume <id>`); None = can only start fresh.
    pub(crate) resume: Option<String>,
    /// When the session ended, unix seconds.
    pub(crate) last_active: u64,
}

impl RecentEntry {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "kind": self.kind.as_str(),
            "title": self.title,
            "resume": self.resume,
            "last_active": self.last_active,
        })
    }

    fn from_json(value: &serde_json::Value) -> Option<RecentEntry> {
        Some(RecentEntry {
            kind: AgentKind::parse(value.get("kind")?.as_str()?)?,
            title: value.get("title")?.as_str()?.to_string(),
            resume: value
                .get("resume")
                .and_then(|r| r.as_str())
                .map(str::to_string),
            last_active: value.get("last_active")?.as_u64()?,
        })
    }
}

/// workspace id -> ended conversations, newest first; backed by a single
/// JSON file (save-on-change), load-tolerant like the other stores.
pub(crate) struct RecentsStore {
    path: PathBuf,
    items: HashMap<String, Vec<RecentEntry>>,
}

impl RecentsStore {
    pub(crate) fn load(path: PathBuf) -> Self {
        let items = match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
                Ok(serde_json::Value::Object(map)) => map
                    .into_iter()
                    .map(|(ws, list)| {
                        let entries = list
                            .as_array()
                            .map(|a| a.iter().filter_map(RecentEntry::from_json).collect())
                            .unwrap_or_default();
                        (ws, entries)
                    })
                    .collect(),
                Ok(_) | Err(_) => {
                    tracing::warn!(path = %path.display(), "corrupt recents.json; starting with empty recents");
                    HashMap::new()
                }
            },
            Err(err) if err.kind() == ErrorKind::NotFound => HashMap::new(),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "failed to read recents.json; starting with empty recents");
                HashMap::new()
            }
        };
        RecentsStore { path, items }
    }

    /// Record an ended conversation at the front of its workspace's list.
    /// The same conversation ending again (a resume that re-ended) updates
    /// its existing entry, and `supersedes` — the session id this one was
    /// resumed FROM (claude forks a new id per resume) — replaces the
    /// ancestor entry instead of duplicating the conversation. Handle-less
    /// duplicates collapse by (kind, title) so untitled codex/gemini rows
    /// never pile up. Persists on change.
    pub(crate) fn push(
        &mut self,
        workspace_id: &str,
        entry: RecentEntry,
        supersedes: Option<&str>,
    ) {
        let list = self.items.entry(workspace_id.to_string()).or_default();
        list.retain(|e| match (&entry.resume, &e.resume) {
            (_, Some(old)) if supersedes == Some(old.as_str()) => false,
            (Some(new), Some(old)) => new != old,
            (None, None) => !(e.kind == entry.kind && e.title == entry.title),
            _ => true,
        });
        list.insert(0, entry);
        list.truncate(CAP_PER_WORKSPACE);
        if let Err(err) = self.save() {
            tracing::error!(%err, "failed to persist recents");
        }
    }

    pub(crate) fn list(&self, workspace_id: &str) -> Vec<RecentEntry> {
        self.items.get(workspace_id).cloned().unwrap_or_default()
    }

    /// Atomically persist the map (tmp file + rename).
    fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let map: serde_json::Map<String, serde_json::Value> = self
            .items
            .iter()
            .map(|(ws, list)| {
                (
                    ws.clone(),
                    serde_json::Value::Array(list.iter().map(RecentEntry::to_json).collect()),
                )
            })
            .collect();
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&map)?)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to rename into {}", self.path.display()))?;
        Ok(())
    }
}

/// Retire a dead agent session: drop its record (and workspace mapping) and,
/// when the conversation is recognizable, remember it in the workspace's
/// recents. Called from the agent watch loop — the one place agent records
/// die. `pinned` is the user-renamed session name, `osc` the last OSC title
/// seen while the session was alive. Broadcasts a change when anything moved.
pub(crate) fn retire(
    state: &Arc<AppState>,
    session_id: &str,
    pinned: Option<&str>,
    osc: Option<&str>,
) {
    let Some(record) = crate::lock(&state.agents).remove(session_id) else {
        return;
    };
    let workspace_id = crate::lock(&state.session_workspaces).remove(session_id);

    let title = pinned
        .map(str::to_string)
        .unwrap_or_else(|| record.display_name(osc));
    // Claude sessions still on the bare fallback name never got a prompt —
    // empty boots, nothing a human could recognize in a list. Codex/gemini
    // have no title machinery yet, so their bare-name rows stay (dropping
    // them would keep those agents out of recents entirely).
    let skip = record.kind == AgentKind::Claude && title == record.kind.as_str();
    if let Some(workspace_id) = workspace_id.filter(|_| !skip) {
        let entry = RecentEntry {
            kind: record.kind,
            title,
            // Fall back to the resumed-from id when no hook ever reported a
            // transcript: the old conversation is still the resume target.
            resume: record.resume_id().or_else(|| record.resumed_from.clone()),
            last_active: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        crate::lock(&state.recents).push(&workspace_id, entry, record.resumed_from.as_deref());
    }
    state.changes.notify_waiters();
}

#[derive(Deserialize)]
pub(crate) struct RecentsQuery {
    workspace_id: String,
}

/// GET /api/v1/recents?workspace_id= — the workspace's ended agent
/// conversations, newest first, minus any whose conversation is live again.
pub(crate) async fn list_recents(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RecentsQuery>,
) -> Response {
    if crate::lock(&state.workspaces)
        .get(&query.workspace_id)
        .is_none()
    {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {}", query.workspace_id)})),
        )
            .into_response();
    }
    // A conversation is "live" through either identity: the transcript its
    // hooks report, or the ancestor id it was resumed from (claude forks a
    // new session id per resume, and hooks may not have fired yet).
    let live: std::collections::HashSet<String> = crate::lock(&state.agents)
        .values()
        .flat_map(|r| [r.resume_id(), r.resumed_from.clone()])
        .flatten()
        .collect();
    let entries: Vec<serde_json::Value> = crate::lock(&state.recents)
        .list(&query.workspace_id)
        .iter()
        .filter(|e| e.resume.as_ref().is_none_or(|r| !live.contains(r)))
        .map(RecentEntry::to_json)
        .collect();
    Json(serde_json::Value::Array(entries)).into_response()
}
