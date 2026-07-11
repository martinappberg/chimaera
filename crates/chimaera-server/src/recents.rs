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

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use chimaera_agent::model::SessionUi;

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
    /// Ancestor session ids this conversation absorbed across resume cycles
    /// (claude forks a new id per resume; the ancestors' transcripts stay on
    /// disk). The merge must not resurrect them from the transcript scan —
    /// resuming an ancestor id would fork the conversation from its
    /// pre-resume state.
    pub(crate) supersedes: Vec<String>,
    /// When the session ended, unix seconds.
    pub(crate) last_active: u64,
    /// The surface the session last ran on (chat vs the real TUI), so
    /// reopening the row lands in the same mode. `None` for pre-`ui` store
    /// entries and transcript-scan rows (no known surface); the UI then falls
    /// back to the launcher's sticky default.
    pub(crate) ui: Option<SessionUi>,
}

/// Ancestor-chain bound: transcripts on disk outlive everything, so a
/// dropped id would resurrect — but a conversation resumed hundreds of
/// times is not a real shape; this is a runaway guard, not a policy.
const SUPERSEDES_CAP: usize = 32;

impl RecentEntry {
    /// The wire shape (`supersedes` stays internal to the store file).
    fn to_api_json(&self) -> serde_json::Value {
        json!({
            "kind": self.kind.as_str(),
            "title": self.title,
            "resume": self.resume,
            "last_active": self.last_active,
            "ui": self.ui,
        })
    }

    fn to_store_json(&self) -> serde_json::Value {
        json!({
            "kind": self.kind.as_str(),
            "title": self.title,
            "resume": self.resume,
            "supersedes": self.supersedes,
            "last_active": self.last_active,
            "ui": self.ui,
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
            supersedes: value
                .get("supersedes")
                .and_then(|s| s.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            last_active: value.get("last_active")?.as_u64()?,
            // Tolerant: missing/invalid → None, so pre-`ui` entries load.
            ui: value
                .get("ui")
                .and_then(|u| u.as_str())
                .and_then(|s| match s {
                    "chat" => Some(SessionUi::Chat),
                    "term" => Some(SessionUi::Term),
                    _ => None,
                }),
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
    /// ancestor entry instead of duplicating the conversation, inheriting
    /// its ancestor chain so the transcript scan can never resurrect any
    /// generation of it. Handle-less duplicates collapse by (kind, title)
    /// so untitled codex/gemini rows never pile up. Persists on change.
    pub(crate) fn push(
        &mut self,
        workspace_id: &str,
        mut entry: RecentEntry,
        supersedes: Option<&str>,
    ) {
        let list = self.items.entry(workspace_id.to_string()).or_default();
        if let Some(ancestor) = supersedes {
            if !entry.supersedes.iter().any(|s| s == ancestor) {
                entry.supersedes.push(ancestor.to_string());
            }
        }
        list.retain(|e| {
            let displaced = match (&entry.resume, &e.resume) {
                (_, Some(old)) if supersedes == Some(old.as_str()) => true,
                (Some(new), Some(old)) => new == old,
                (None, None) => e.kind == entry.kind && e.title == entry.title,
                _ => false,
            };
            if displaced {
                // The displaced entry's own ancestors move onto the new one.
                for s in &e.supersedes {
                    if !entry.supersedes.contains(s) {
                        entry.supersedes.push(s.clone());
                    }
                }
            }
            !displaced
        });
        entry.supersedes.truncate(SUPERSEDES_CAP);
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
        let map: serde_json::Map<String, serde_json::Value> = self
            .items
            .iter()
            .map(|(ws, list)| {
                (
                    ws.clone(),
                    serde_json::Value::Array(list.iter().map(RecentEntry::to_store_json).collect()),
                )
            })
            .collect();
        crate::persist::atomic_write_json(&self.path, serde_json::to_vec(&map)?)
    }
}

/// Retire a dead agent session: drop its record (and workspace mapping) and,
/// when the conversation is recognizable, remember it in the workspace's
/// recents. Called from the agent watch loop — the one place agent records
/// die. `pinned` is the user-renamed session name, `osc` the last OSC title
/// seen while the session was alive, `ui` the surface it last ran on (so the
/// recents row reopens in the same mode). Broadcasts a change when anything
/// moved.
pub(crate) fn retire(
    state: &Arc<AppState>,
    session_id: &str,
    pinned: Option<&str>,
    osc: Option<&str>,
    ui: SessionUi,
) {
    let Some(record) = crate::lock(&state.agents).remove(session_id) else {
        return;
    };
    let workspace_id = crate::lock(&state.session_workspaces).remove(session_id);
    // The session's identity ends here, so its respawn recipe must too: a
    // chat toggled to the terminal keeps its ChatRecipe (the view-switch
    // exit paths deliberately preserve it for the successor), and the PTY
    // retire paths — the watcher, DELETE, close-all — never cleared it.
    crate::lock(&state.chat_recipes).remove(session_id);
    // Same identity-ends rule for what the session uploaded (screenshots,
    // desktop drops): uploads are session-lifetime state, and leaving them
    // would grow ~/.chimaera without bound on shared login nodes.
    crate::upload::prune_session_uploads(state, session_id);

    let title = pinned
        .map(str::to_string)
        .unwrap_or_else(|| record.display_name(osc));
    // Claude sessions still on the bare fallback name never got a prompt —
    // empty boots, nothing a human could recognize in a list. Codex/gemini
    // have no title machinery yet, so their bare-name rows stay (dropping
    // them would keep those agents out of recents entirely).
    let skip = record.kind == AgentKind::Claude && title == record.kind.as_str();
    if let Some(workspace_id) = workspace_id.filter(|_| !skip) {
        // Only promise resumption a transcript can deliver: claude 2.1.204
        // interactive sessions persist NO transcript (verified 2026-07-07),
        // so an unverified id would mint a row whose click dies with "No
        // conversation found". The resumed-from ancestor is the fallback
        // target when the session's own transcript never materialized.
        let workspace_root = crate::lock(&state.workspaces)
            .get(&workspace_id)
            .map(|w| w.root);
        let resume =
            [record.resume_id(), record.resumed_from.clone()]
                .into_iter()
                .flatten()
                .find(|id| {
                    record.transcript_path.as_deref().is_some_and(|p| {
                        p.file_stem().is_some_and(|s| s == id.as_str()) && p.is_file()
                    }) || workspace_root.as_deref().is_some_and(|root| {
                        state
                            .claude_projects_dir
                            .join(crate::launcher::encode_cwd(root))
                            .join(id)
                            .with_extension("jsonl")
                            .is_file()
                    })
                });
        let entry = RecentEntry {
            kind: record.kind,
            title,
            resume,
            supersedes: Vec::new(), // push() records the ancestor chain
            last_active: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            ui: Some(ui),
        };
        crate::lock(&state.recents).push(&workspace_id, entry, record.resumed_from.as_deref());
        // The epoch rides /ws/events so the rail refetches exactly when a
        // row landed, instead of guessing at the watch loop's timing.
        state
            .recents_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    state.changes.notify_waiters();
}

#[derive(Deserialize)]
pub(crate) struct RecentsQuery {
    workspace_id: String,
}

/// GET /api/v1/recents?workspace_id= — the workspace's ended agent
/// conversations, newest first, minus any whose conversation is live again.
///
/// Two sources merge here: the daemon's own history (any agent kind, ended
/// under its watch) and the claude transcript store (conversations from
/// before chimaera, or from claude run outside it). The rail's Recents is
/// the ONE resume surface — the launcher popover has no list of its own.
pub(crate) async fn list_recents(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RecentsQuery>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&query.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {}", query.workspace_id)})),
        )
            .into_response();
    };
    // A conversation is "live" through either identity: the transcript its
    // hooks report, or the ancestor id it was resumed from (claude forks a
    // new session id per resume, and hooks may not have fired yet).
    let (live, exclude): (std::collections::HashSet<String>, Vec<PathBuf>) = {
        let agents = crate::lock(&state.agents);
        (
            agents
                .values()
                .flat_map(|r| [r.resume_id(), r.resumed_from.clone()])
                .flatten()
                .collect(),
            agents
                .values()
                .filter_map(|a| a.transcript_path.clone())
                .collect(),
        )
    };
    let store_entries = crate::lock(&state.recents).list(&query.workspace_id);

    let dir = state
        .claude_projects_dir
        .join(crate::launcher::encode_cwd(&workspace.root));
    // Transcripts can be tens of MB; scan them off the async runtime.
    let scanned =
        tokio::task::spawn_blocking(move || crate::launcher::scan_resumables(&dir, &exclude))
            .await
            .unwrap_or_default();

    // Daemon entries win on identity collisions: they know the agent kind
    // and the true end time, and carry non-claude history the store can't.
    // "Identity" spans a conversation's whole resume lineage — superseded
    // ancestor ids count as seen, or the scan would resurrect them (their
    // transcripts stay on disk) and a click would fork the conversation
    // from its pre-resume state.
    let mut seen: std::collections::HashSet<String> = store_entries
        .iter()
        .flat_map(|e| e.resume.iter().chain(e.supersedes.iter()))
        .cloned()
        .collect();
    let mut merged: Vec<serde_json::Value> = store_entries
        .iter()
        .filter(|e| e.resume.as_ref().is_none_or(|r| !live.contains(r)))
        .map(RecentEntry::to_api_json)
        .collect();
    for row in scanned {
        let Some(id) = row.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if seen.contains(id) || live.contains(id) {
            continue;
        }
        seen.insert(id.to_string());
        merged.push(json!({
            "kind": AgentKind::Claude.as_str(),
            "title": row.get("title").cloned().unwrap_or_default(),
            "resume": id,
            "last_active": row.get("mtime").cloned().unwrap_or(json!(0)),
            // Conversations from outside chimaera have no known surface.
            "ui": serde_json::Value::Null,
        }));
    }
    merged.sort_by_key(|e| {
        std::cmp::Reverse(e.get("last_active").and_then(|v| v.as_u64()).unwrap_or(0))
    });
    merged.truncate(CAP_PER_WORKSPACE);
    Json(serde_json::Value::Array(merged)).into_response()
}
