//! Rail "Recents": daemon-persisted, per-workspace history of ENDED agent
//! conversations (docs/design/README.md "The agent launcher" — Rail Recents). When an
//! agent session's PTY dies, its record retires here instead of vanishing,
//! so the conversation can still be found — and resumed where the CLI
//! supports it (`claude --resume`; codex `thread/resume` / `codex resume`).
//! A row starts fresh only when Chimaera did not capture a native handle.
//!
//! Live sessions never appear here: entries whose conversation is running
//! again (resumed in some session) are hidden at read time, not deleted —
//! they come back when that session ends.

use std::collections::HashMap;
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
    /// Native conversation handle (Claude session id / Codex thread id);
    /// None = Chimaera can only start a fresh session from this row.
    pub(crate) resume: Option<String>,
    /// Ancestor session ids this conversation absorbed across resume cycles
    /// (older claude CLIs forked a new id per resume, 2.1.283 keeps it; the
    /// ancestors' transcripts stay on disk). The merge must not resurrect them from the transcript scan —
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
    /// The transcript path claude's hooks last reported (claude only) — the
    /// file `--resume` will reopen, wherever claude filed it (an agent that
    /// entered a worktree mid-conversation moved it there). Store-only.
    pub(crate) transcript: Option<PathBuf>,
    /// The folder the session ran in when it was not the workspace root (a
    /// worktree it was started in), so reopening resumes it there while that
    /// folder exists. Store-only; the API exposes it as `worktree` when it is
    /// a Chimaera-managed worktree that still exists.
    pub(crate) cwd: Option<PathBuf>,
}

/// Ancestor-chain bound: transcripts on disk outlive everything, so a
/// dropped id would resurrect — but a conversation resumed hundreds of
/// times is not a real shape; this is a runaway guard, not a policy.
const SUPERSEDES_CAP: usize = 32;

impl RecentEntry {
    /// The identity an archive hides it under (see `recents_archive`).
    fn archive_key(&self) -> String {
        crate::recents_archive::key_of(self.resume.as_deref(), self.kind.as_str(), &self.title)
    }

    /// The wire shape (`supersedes`, `transcript` and `cwd` stay internal to
    /// the store file). `worktree` is additive: the managed worktree this
    /// conversation ran in, when it still exists (resolved by the caller).
    fn to_api_json(&self, worktree: Option<&PathBuf>) -> serde_json::Value {
        json!({
            "key": self.archive_key(),
            "kind": self.kind.as_str(),
            "title": self.title,
            "resume": self.resume,
            "last_active": self.last_active,
            "ui": self.ui,
            "worktree": worktree,
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
            "transcript": self.transcript,
            "cwd": self.cwd,
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
            // Additive and tolerant: older store entries have neither.
            transcript: value
                .get("transcript")
                .and_then(|t| t.as_str())
                .map(PathBuf::from),
            cwd: value.get("cwd").and_then(|c| c.as_str()).map(PathBuf::from),
        })
    }
}

/// Read cap for `recents.json`. The format bounds each workspace to
/// `CAP_PER_WORKSPACE` (20) rows of a title, two PATH_MAX paths and at most
/// `SUPERSEDES_CAP` ancestor ids — a few KiB per row at worst, ~100 KiB per
/// workspace — so 8 MiB holds far more workspaces than anyone keeps. A file
/// past it is corruption: it loads empty and the next push replaces it.
pub(crate) const MAX_RECENTS_BYTES: usize = 8 * 1024 * 1024;

type RecentsMap = HashMap<String, Vec<RecentEntry>>;

/// The store's file name, for its log lines.
const RECENTS_FILE: &str = "recents.json";

/// What one bounded read of `recents.json` found: `Err` when it couldn't be
/// read; missing, oversized and corrupt are all an empty map (logged).
fn read_recents(path: &std::path::Path) -> Result<RecentsMap, std::io::Error> {
    match crate::persist::read_bounded(path, MAX_RECENTS_BYTES) {
        crate::persist::Bounded::Ok(contents) => {
            match serde_json::from_slice::<serde_json::Value>(&contents) {
                Ok(serde_json::Value::Object(map)) => Ok(map
                    .into_iter()
                    .map(|(ws, list)| {
                        let entries = list
                            .as_array()
                            .map(|a| a.iter().filter_map(RecentEntry::from_json).collect())
                            .unwrap_or_default();
                        (ws, entries)
                    })
                    .collect()),
                Ok(_) | Err(_) => {
                    tracing::warn!(path = %path.display(), "corrupt recents.json; starting with empty recents");
                    Ok(HashMap::new())
                }
            }
        }
        crate::persist::Bounded::Missing => Ok(HashMap::new()),
        crate::persist::Bounded::TooLarge => {
            tracing::warn!(path = %path.display(), "recents.json exceeds {MAX_RECENTS_BYTES} bytes; starting with empty recents");
            Ok(HashMap::new())
        }
        crate::persist::Bounded::Io(err) => Err(err),
    }
}

/// workspace id -> ended conversations, newest first; backed by a single
/// JSON file (save-on-change), load-tolerant like the other stores.
pub(crate) struct RecentsStore {
    path: PathBuf,
    items: RecentsMap,
    /// The file is there but couldn't be read (permissions, a directory at
    /// the path, an I/O error): nothing is written over it. Each save
    /// retries the read; once it reads, what it holds is merged in and the
    /// hold lifts.
    hold: crate::persist::Hold,
}

impl RecentsStore {
    /// Load the store (blocking; boot). Missing, oversized or corrupt is an
    /// empty history (logged); unreadable is an empty history held from
    /// being saved over.
    pub(crate) fn load(path: PathBuf) -> Self {
        let mut hold = crate::persist::Hold::default();
        let items = read_recents(&path).unwrap_or_else(|err| {
            hold.note(&path, RECENTS_FILE, err);
            HashMap::new()
        });
        RecentsStore { path, items, hold }
    }

    /// A held store's retry (blocking, where the save runs): when the file
    /// reads now, merge it in — this daemon's rows first (they are newer),
    /// then the file's rows this daemon doesn't already carry, by key and by
    /// lineage — and lift the hold. Returns whether a save may go ahead.
    fn retry_held(&mut self) -> bool {
        if !self.hold.is_held() {
            return true;
        }
        let disk = match read_recents(&self.path) {
            Ok(disk) => disk,
            Err(err) => {
                self.hold.note(&self.path, RECENTS_FILE, err);
                return false;
            }
        };
        for (ws, rows) in disk {
            let list = self.items.entry(ws).or_default();
            for row in rows {
                let key = row.archive_key();
                let known = list.iter().any(|e| {
                    e.archive_key() == key
                        || row
                            .resume
                            .as_ref()
                            .is_some_and(|id| e.supersedes.contains(id))
                });
                if !known {
                    list.push(row);
                }
            }
            list.truncate(CAP_PER_WORKSPACE);
        }
        self.hold.release(&self.path, RECENTS_FILE);
        true
    }

    /// Record an ended conversation at the front of its workspace's list.
    /// The same conversation ending again (a resume that re-ended) updates
    /// its existing entry, and `supersedes` — the session id this one was
    /// resumed FROM (older claude CLIs forked a new id per resume) — replaces the
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

    /// The transcript paths recorded for conversation `id` (its own row, or
    /// a row whose lineage absorbed it), across workspaces.
    pub(crate) fn recorded_transcripts(&self, id: &str) -> Vec<PathBuf> {
        self.items
            .values()
            .flatten()
            .filter(|e| e.resume.as_deref() == Some(id) || e.supersedes.iter().any(|s| s == id))
            .filter_map(|e| e.transcript.clone())
            .collect()
    }

    /// Where conversation `id` last ran in this workspace, when that was not
    /// the root (see [`RecentEntry::cwd`]). Follows the lineage like
    /// [`Self::recorded_transcripts`]: an absorbed older id finds its row.
    pub(crate) fn cwd_of(&self, workspace_id: &str, id: &str) -> Option<PathBuf> {
        self.items
            .get(workspace_id)?
            .iter()
            .find(|e| e.resume.as_deref() == Some(id) || e.supersedes.iter().any(|s| s == id))
            .and_then(|e| e.cwd.clone())
    }

    /// resume id -> the ancestor ids that conversation absorbed.
    fn ancestors(&self, workspace_id: &str) -> HashMap<String, Vec<String>> {
        self.items
            .get(workspace_id)
            .into_iter()
            .flatten()
            .filter(|e| !e.supersedes.is_empty())
            .filter_map(|e| Some((e.resume.clone()?, e.supersedes.clone())))
            .collect()
    }

    /// Atomically persist the map (tmp file + rename) — never over a file
    /// that can't be read (the change stays in memory; the hold logs once).
    fn save(&mut self) -> anyhow::Result<()> {
        if !self.retry_held() {
            return Ok(());
        }
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

/// resume id -> ancestor ids, for a workspace's Recents (the archive hides a
/// conversation's whole chain).
pub(crate) fn ancestors(state: &AppState, workspace_id: &str) -> HashMap<String, Vec<String>> {
    crate::lock(&state.recents).ancestors(workspace_id)
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
    retire_with_resume(state, session_id, pinned, osc, ui, None);
}

/// Retire a session while carrying the structured driver's native handle.
/// `AgentRecord::resume_id` is transcript-derived (the TUI/Claude path), so a
/// Codex chat's thread id must cross from `ChatInfo` before its registry entry
/// is removed. Claude hints still pass the transcript-existence gate below.
pub(crate) fn retire_with_resume(
    state: &Arc<AppState>,
    session_id: &str,
    pinned: Option<&str>,
    osc: Option<&str>,
    ui: SessionUi,
    resume_hint: Option<String>,
) {
    retire_inner(state, session_id, pinned, osc, ui, resume_hint, true);
}

fn retire_inner(
    state: &Arc<AppState>,
    session_id: &str,
    pinned: Option<&str>,
    osc: Option<&str>,
    ui: SessionUi,
    resume_hint: Option<String>,
    remember: bool,
) {
    let Some(record) = crate::lock(&state.agents).remove(session_id) else {
        return;
    };
    let workspace_id = crate::lock(&state.session_workspaces).remove(session_id);
    // Where it ran, read before the recipe goes below: a chat's recipe, else
    // the PTY agent's spawn folder.
    let session_cwd = crate::lock(&state.chat_recipes)
        .get(session_id)
        .map(|r| r.workspace_root.clone())
        .or_else(|| record.spawn_cwd.clone());
    // Its history record closes here too — every session, Masterminds and
    // untitled boots included (Recents skips those below; history doesn't).
    crate::history::close(
        state,
        session_id,
        &record,
        pinned,
        osc,
        ui,
        resume_hint.as_deref(),
    );
    // The session's identity ends here, so its respawn recipe must too: a
    // chat toggled to the terminal keeps its ChatRecipe (the view-switch
    // exit paths deliberately preserve it for the successor), and the PTY
    // retire paths — the watcher, DELETE, close-all — never cleared it.
    crate::lock(&state.chat_recipes).remove(session_id);
    // Same identity-ends rule for what the session uploaded (screenshots,
    // desktop drops): uploads are session-lifetime state, and leaving them
    // would grow ~/.chimaera without bound on shared login nodes.
    crate::upload::prune_session_uploads(state, session_id);
    // And its prelude file (runtime tmp — night-scrub would get leftovers,
    // but tidy is tidy).
    crate::environment::remove_prelude_file(session_id);
    crate::agents::remove_fork_context(session_id);

    // A dead Mastermind must not stay bound (the dock would show a ghost),
    // and it never lands in Recents: it is the observer, not a roster
    // conversation — resuming it as a normal chat would leak MCP status
    // dumps into the launcher. The PUT/DELETE routes pre-remove the record
    // (making this retire a no-op); this arm covers self-exits.
    let was_mastermind = workspace_id
        .as_deref()
        .is_some_and(|ws| crate::lock(&state.workspaces).clear_mastermind_if(ws, session_id));

    let title = pinned
        .map(str::to_string)
        .unwrap_or_else(|| record.display_name(osc));
    // Claude sessions still on the bare fallback name never got a prompt —
    // empty boots, nothing a human could recognize in a list. Codex/gemini
    // have no title machinery yet, so their bare-name rows stay (dropping
    // them would keep those agents out of recents entirely).
    let skip = !remember
        || was_mastermind
        || (record.kind == AgentKind::Claude && title == record.kind.as_str());
    if let Some(workspace_id) = workspace_id.filter(|_| !skip) {
        // Non-Claude chat agents own their durable store and resume by id;
        // there is no Claude-style transcript path to validate. ChatInfo is
        // therefore the authority, with resumed_from as the fallback for a
        // resumed process that died before initialization completed.
        //
        // Claude only promises resumption a transcript can deliver: 2.1.204
        // interactive sessions persist NO transcript (verified 2026-07-07),
        // so an unverified id would mint a row whose click dies with "No
        // conversation found". The hint lets structured chat name its native
        // id; it still has to resolve to an actual transcript file.
        let workspace_root = crate::lock(&state.workspaces)
            .get(&workspace_id)
            .map(|w| w.root);
        let resume = if record.kind == AgentKind::Codex
            || (ui == SessionUi::Chat && record.kind != AgentKind::Claude)
        {
            resume_hint.or_else(|| record.resumed_from.clone())
        } else {
            // The shared lookup: the hook-reported transcript, the session's
            // own folder (a worktree it started in), then the root.
            let cwds: Vec<PathBuf> = session_cwd
                .iter()
                .chain(workspace_root.iter())
                .cloned()
                .collect();
            let recorded: Vec<PathBuf> = record.transcript_path.iter().cloned().collect();
            let candidates: Vec<String> =
                [resume_hint, record.resume_id(), record.resumed_from.clone()]
                    .into_iter()
                    .flatten()
                    .collect();
            let ids: Vec<&str> = candidates.iter().map(String::as_str).collect();
            // One lookup for all of the conversation's handles: the store is
            // listed at most once however many of them miss.
            crate::claude_store::find_first(&state.claude_projects_dir, &ids, &recorded, &cwds)
                .map(|(id, _)| id.to_string())
        };
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
            transcript: record.transcript_path.clone(),
            // Only a folder other than the root is worth remembering.
            cwd: session_cwd.filter(|c| workspace_root.as_ref() != Some(c)),
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

/// The linked worktree holding `dir` (the nearest folder with a `.git`
/// file), searched only inside `managed`. Blocking.
fn worktree_top(dir: &std::path::Path, managed: &std::path::Path) -> Option<PathBuf> {
    dir.ancestors()
        .take_while(|d| d.starts_with(managed) && *d != managed)
        .find(|d| d.join(".git").is_file())
        .map(std::path::Path::to_path_buf)
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
    // hooks report, or the ancestor id it was resumed from (older claude CLIs
    // forked a new session id per resume, and hooks may not have fired yet).
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
    // Archived conversations are hidden here (never deleted): by their key,
    // or any id of their resume chain.
    let archived = crate::lock(&state.recents_archive).hidden(&query.workspace_id);

    let store = state.claude_projects_dir.clone();
    let root = workspace.root.clone();
    let managed = state.worktrees_root_real.clone();
    // A row's worktree, for the UI's "remove it" offer: the top of the
    // managed worktree it ran in (also from a subfolder), while it exists.
    let cwds: Vec<Option<PathBuf>> = store_entries
        .iter()
        .map(|e| e.cwd.clone().filter(|c| c.starts_with(&managed)))
        .collect();
    // Transcripts can be tens of MB; scan them off the async runtime. The
    // root's own project dir plus its Claude worktrees' (`EnterWorktree`,
    // `.claude/worktrees/<name>`), where conversations from outside chimaera
    // that entered a worktree now live.
    let (scanned, worktrees) = tokio::task::spawn_blocking(move || {
        let mut dirs = vec![store.join(crate::launcher::encode_cwd(&root))];
        dirs.extend(crate::claude_store::claude_worktree_dirs(&store, &[root]));
        let worktrees: Vec<Option<PathBuf>> = cwds
            .into_iter()
            .map(|c| c.and_then(|c| worktree_top(&c, &managed)))
            .collect();
        (crate::launcher::scan_resumables(&dirs, &exclude), worktrees)
    })
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
        .zip(worktrees.iter().map(Some).chain(std::iter::repeat(None)))
        .filter(|(e, _)| e.resume.as_ref().is_none_or(|r| !live.contains(r)))
        .filter(|(e, _)| {
            !archived.contains(&e.archive_key())
                && !e.supersedes.iter().any(|s| archived.contains(s))
        })
        .map(|(e, wt)| e.to_api_json(wt.and_then(|w| w.as_ref())))
        .collect();
    for row in scanned {
        let Some(id) = row.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if seen.contains(id) || live.contains(id) || archived.contains(id) {
            continue;
        }
        seen.insert(id.to_string());
        merged.push(json!({
            "key": id,
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
#[cfg(test)]
mod tests {
    use super::*;

    fn entry(resume: &str, cwd: Option<&str>) -> RecentEntry {
        RecentEntry {
            kind: AgentKind::Claude,
            title: "t".into(),
            resume: Some(resume.into()),
            supersedes: Vec::new(),
            last_active: 1,
            ui: Some(SessionUi::Chat),
            transcript: None,
            cwd: cwd.map(PathBuf::from),
        }
    }

    /// Reopening by an id the conversation absorbed (an older claude forked
    /// one per resume) still finds where it ran, like its transcripts do.
    #[test]
    fn cwd_of_follows_the_resume_lineage() {
        let dir = std::env::temp_dir().join(format!("chimaera-recents-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = RecentsStore::load(dir.join("recents.json"));
        store.push("w", entry("old", Some("/wt/a")), None);
        store.push("w", entry("new", Some("/wt/a")), Some("old"));
        assert_eq!(store.cwd_of("w", "new"), Some(PathBuf::from("/wt/a")));
        assert_eq!(store.cwd_of("w", "old"), Some(PathBuf::from("/wt/a")));
        assert_eq!(store.cwd_of("w", "other"), None);
        assert_eq!(store.cwd_of("x", "new"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn worktree_top_walks_up_only_inside_the_managed_root() {
        let dir = std::env::temp_dir().join(format!("chimaera-recents-top-{}", std::process::id()));
        let managed = dir.join("worktrees");
        let top = managed
            .join("repo-1")
            .join("chimaera")
            .join("claude-abc123");
        let sub = top.join("src").join("deep");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(top.join(".git"), "gitdir: elsewhere\n").unwrap();
        assert_eq!(worktree_top(&sub, &managed), Some(top.clone()));
        assert_eq!(worktree_top(&top, &managed), Some(top.clone()));
        std::fs::remove_file(top.join(".git")).unwrap();
        assert_eq!(worktree_top(&sub, &managed), None, "gone: no worktree");
        std::fs::remove_dir_all(&dir).ok();
    }
}
