//! Durable session ledger + boot resurrection.
//!
//! PTY sessions are daemon-owned child processes: any daemon restart —
//! update, crash, `chimaera kill` — necessarily ends them. What makes that
//! survivable is this module: a small `sessions.json` under the data dir,
//! continuously reconciled from live state, that records each session's
//! *semantic* identity (workspace, cwd, agent kind, claude conversation id,
//! pinned name) rather than its process. On boot the daemon resurrects from
//! it: shells respawn at their last cwd, claude agents respawn with
//! `--resume`, and non-resumable agents retire honestly into Recents instead
//! of vanishing (DESIGN.md: "cold-restart → re-attach every session via
//! --resume with preserved cwd").
//!
//! Session ids are preserved across the restart. That single property is
//! what lets every persisted layout tab, linked-terminal edge, and open
//! window rebind without any client-side migration.
//!
//! The ledger deliberately stores no argv: resurrection rebuilds commands
//! through the same spawn path as POST /sessions, so hook URLs, shims, and
//! themes always match the *new* daemon, never the dead one.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde_json::json;

use crate::agents::AgentKind;
use crate::AppState;

/// One live session, as much of it as can be rebuilt after a restart.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LedgerEntry {
    pub(crate) id: String,
    pub(crate) workspace_id: String,
    /// Last polled cwd (shells) or spawn cwd (agents).
    pub(crate) cwd: PathBuf,
    /// User-pinned display name (`SessionInfo::renamed`), when set.
    pub(crate) pinned_name: Option<String>,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    /// The scheme the session was themed for at spawn.
    pub(crate) theme: String,
    /// `None` = plain shell.
    pub(crate) agent: Option<LedgerAgent>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LedgerAgent {
    pub(crate) kind: AgentKind,
    /// Claude conversation id (`--resume <id>`), when known. A recorded id
    /// is a CLAIM, not a promise: claude 2.1.204 interactive sessions do
    /// not persist their transcripts (verified 2026-07-07 — print mode
    /// does; 2.1.19x did), and `--resume` against a conversation with no
    /// transcript dies with "No conversation found". Restore verifies the
    /// transcript on disk before resuming.
    pub(crate) resume: Option<String>,
    /// The transcript path claude's hooks reported, when any — the exact
    /// file `--resume` needs to exist.
    pub(crate) transcript: Option<PathBuf>,
    /// Current display title — carried onto the successor session (or a
    /// Recents row) so the conversation stays recognizable either way.
    pub(crate) title: String,
}

/// What the previous daemon left behind.
#[derive(Debug, Default)]
pub(crate) struct BootLedger {
    pub(crate) sessions: Vec<LedgerEntry>,
    /// terminal session id -> agent session id (linked terminals).
    pub(crate) links: HashMap<String, String>,
    /// Unix seconds of the last reconcile — the honest `last_active` for
    /// entries that retire into Recents at boot.
    pub(crate) written_at: u64,
}

impl LedgerEntry {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "id": self.id,
            "workspace_id": self.workspace_id,
            "cwd": self.cwd,
            "pinned_name": self.pinned_name,
            "cols": self.cols,
            "rows": self.rows,
            "theme": self.theme,
            "agent": self.agent.as_ref().map(|a| json!({
                "kind": a.kind.as_str(),
                "resume": a.resume,
                "transcript": a.transcript,
                "title": a.title,
            })),
        })
    }

    fn from_json(value: &serde_json::Value) -> Option<LedgerEntry> {
        let agent = match value.get("agent") {
            None | Some(serde_json::Value::Null) => None,
            Some(a) => Some(LedgerAgent {
                kind: AgentKind::parse(a.get("kind")?.as_str()?)?,
                resume: a.get("resume").and_then(|r| r.as_str()).map(str::to_string),
                transcript: a
                    .get("transcript")
                    .and_then(|t| t.as_str())
                    .map(PathBuf::from),
                title: a.get("title")?.as_str()?.to_string(),
            }),
        };
        Some(LedgerEntry {
            id: value.get("id")?.as_str()?.to_string(),
            workspace_id: value.get("workspace_id")?.as_str()?.to_string(),
            cwd: PathBuf::from(value.get("cwd")?.as_str()?),
            pinned_name: value
                .get("pinned_name")
                .and_then(|n| n.as_str())
                .map(str::to_string),
            cols: value.get("cols")?.as_u64()? as u16,
            rows: value.get("rows")?.as_u64()? as u16,
            theme: value
                .get("theme")
                .and_then(|t| t.as_str())
                .unwrap_or("dark")
                .to_string(),
            agent,
        })
    }
}

/// The on-disk ledger. One writer (the reconcile loop); writes are atomic
/// (tmp + rename) and skipped when nothing changed, so an idle daemon costs
/// zero I/O.
pub(crate) struct LedgerStore {
    path: PathBuf,
    /// Serialized body of the last write, for change detection.
    last_written: Option<String>,
}

impl LedgerStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        LedgerStore {
            path,
            last_written: None,
        }
    }

    /// Read what the previous daemon left. Load-tolerant like every store:
    /// a missing or corrupt file is an empty ledger, never an error.
    pub(crate) fn load_boot(&self) -> BootLedger {
        let contents = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(err) if err.kind() == ErrorKind::NotFound => return BootLedger::default(),
            Err(err) => {
                tracing::warn!(path = %self.path.display(), %err, "failed to read session ledger");
                return BootLedger::default();
            }
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
            tracing::warn!(path = %self.path.display(), "corrupt session ledger; starting empty");
            return BootLedger::default();
        };
        let sessions = value
            .get("sessions")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter_map(LedgerEntry::from_json).collect())
            .unwrap_or_default();
        let links = value
            .get("links")
            .and_then(|l| l.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        BootLedger {
            sessions,
            links,
            written_at: value
                .get("written_at")
                .and_then(|w| w.as_u64())
                .unwrap_or(0),
        }
    }

    /// Persist the snapshot if it differs from the last write. `written_at`
    /// is excluded from the comparison (it would make every snapshot
    /// "changed") and stamped only when a real write happens.
    pub(crate) fn write_if_changed(
        &mut self,
        entries: &[LedgerEntry],
        links: &HashMap<String, String>,
    ) {
        // BTreeMap orders the links so an unchanged snapshot compares equal.
        let links: std::collections::BTreeMap<&String, &String> = links.iter().collect();
        let body = json!({
            "sessions": entries.iter().map(LedgerEntry::to_json).collect::<Vec<_>>(),
            "links": links,
        })
        .to_string();
        if self.last_written.as_deref() == Some(body.as_str()) {
            return;
        }
        if let Err(err) = self.save(&body) {
            tracing::error!(%err, "failed to persist session ledger");
            return;
        }
        self.last_written = Some(body);
    }

    fn save(&self, body: &str) -> anyhow::Result<()> {
        let full = {
            let mut value: serde_json::Value = serde_json::from_str(body)?;
            value["v"] = json!(1);
            value["written_at"] = json!(unix_now());
            value.to_string()
        };
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, full).with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to rename into {}", self.path.display()))?;
        Ok(())
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Reconcile tick: the fallback when no change notification fires.
const RECONCILE_TICK: Duration = Duration::from_secs(5);
/// Minimum gap between writes: cwd polls and title updates churn, and the
/// ledger lives on a (possibly NFS) home directory.
const WRITE_DEBOUNCE: Duration = Duration::from_secs(2);

/// Own the ledger for the daemon's lifetime: consume what the previous
/// daemon left (resurrect / retire), then keep `sessions.json` reconciled
/// with live truth until shutdown.
pub(crate) async fn run(state: Arc<AppState>) {
    // Boot data must be read (and acted on) before the reconcile loop's
    // first write replaces it with the current — initially empty — truth.
    let boot = crate::lock(&state.ledger).load_boot();
    restore(&state, boot).await;
    // Serving started concurrently; sessions snapshots held back by
    // `wait_restored` may flow now that the roster is whole.
    state.restored.send_replace(true);
    loop {
        tokio::select! {
            _ = state.changes.notified() => {}
            _ = tokio::time::sleep(RECONCILE_TICK) => {}
        }
        let (entries, links) = snapshot(&state);
        crate::lock(&state.ledger).write_if_changed(&entries, &links);
        tokio::time::sleep(WRITE_DEBOUNCE).await;
    }
}

/// Derive the ledger from live state. Only chimaera-spawned user sessions
/// qualify: install sessions (runtimes) are transient by design, and a
/// session without a workspace mapping could not be respawned anywhere.
/// Also called once at graceful shutdown for the final flush.
pub(crate) fn snapshot(state: &AppState) -> (Vec<LedgerEntry>, HashMap<String, String>) {
    let infos = state.sessions.list();
    let live_ids: std::collections::HashSet<&str> = infos.iter().map(|i| i.id.as_str()).collect();
    // The theme map has no other reaper: prune it to live sessions here.
    crate::lock(&state.session_themes).retain(|id, _| live_ids.contains(id.as_str()));

    let install_ids: std::collections::HashSet<String> = crate::lock(&state.installs)
        .values()
        .map(|(id, _)| id.clone())
        .collect();
    let workspaces = crate::lock(&state.session_workspaces);
    let agents = crate::lock(&state.agents);
    let cwds = crate::lock(&state.current_cwds);
    let themes = crate::lock(&state.session_themes);

    let entries = infos
        .iter()
        .filter(|info| info.alive && !install_ids.contains(&info.id))
        .filter_map(|info| {
            let workspace_id = workspaces.get(&info.id)?.clone();
            let agent = agents.get(&info.id).map(|record| LedgerAgent {
                kind: record.kind,
                resume: record.resume_id().or_else(|| record.resumed_from.clone()),
                transcript: record.transcript_path.clone(),
                title: record.display_name(info.title.as_deref()),
            });
            Some(LedgerEntry {
                id: info.id.clone(),
                workspace_id,
                cwd: cwds
                    .get(&info.id)
                    .cloned()
                    .unwrap_or_else(|| info.cwd.clone()),
                pinned_name: info.renamed.then(|| info.name.clone()),
                cols: info.cols,
                rows: info.rows,
                theme: themes
                    .get(&info.id)
                    .cloned()
                    .unwrap_or_else(|| "dark".into()),
                agent,
            })
        })
        .collect();

    let links = crate::lock(&state.links)
        .iter()
        .filter(|(t, a)| live_ids.contains(t.as_str()) && live_ids.contains(a.as_str()))
        .map(|(t, a)| (t.clone(), a.clone()))
        .collect();
    (entries, links)
}

/// Act on what the previous daemon left: resurrect what can come back
/// faithfully, retire the rest into Recents. Never silent — the log says
/// what happened to every entry.
pub(crate) async fn restore(state: &Arc<AppState>, boot: BootLedger) {
    if boot.sessions.is_empty() {
        return;
    }
    // `daemon.restoreSessions` off = retire agents into Recents (their
    // conversations must still be findable) and let shells go.
    let restore = crate::lock(&state.settings).restore_sessions();
    let mut respawned = 0usize;
    let mut retired = 0usize;
    for entry in &boot.sessions {
        let workspace = crate::lock(&state.workspaces).get(&entry.workspace_id);
        let plan = plan_restore(entry, restore, workspace.is_some());
        match plan {
            RestorePlan::Respawn => {
                let workspace = workspace.expect("plan requires a workspace");
                match respawn(state, entry, workspace).await {
                    Ok(()) => respawned += 1,
                    Err(err) => {
                        tracing::warn!(session = %entry.id, %err, "resurrection failed");
                        if retire_to_recents(state, entry, boot.written_at) {
                            retired += 1;
                        }
                    }
                }
            }
            RestorePlan::Retire => {
                if retire_to_recents(state, entry, boot.written_at) {
                    retired += 1;
                }
            }
            RestorePlan::Drop => {}
        }
    }
    // Linked-terminal edges survive only when both endpoints did.
    {
        let live: std::collections::HashSet<String> =
            state.sessions.list().into_iter().map(|i| i.id).collect();
        let mut links = crate::lock(&state.links);
        for (terminal, agent) in &boot.links {
            if live.contains(terminal) && live.contains(agent) {
                links.insert(terminal.clone(), agent.clone());
            }
        }
    }
    if retired > 0 {
        state
            .recents_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    if respawned > 0 || retired > 0 {
        tracing::info!(
            respawned,
            retired,
            of = boot.sessions.len(),
            "restored sessions from the ledger"
        );
        state.changes.notify_waiters();
    }
}

#[derive(Debug, PartialEq)]
enum RestorePlan {
    Respawn,
    Retire,
    Drop,
}

/// Pure restore policy, split out for tests: shells and claude respawn
/// (claude resumes its conversation; a claude that never got a transcript
/// has nothing to lose and boots fresh). Other agents have no *verified*
/// resume mechanism — respawning a fresh TUI while pretending it is the
/// same conversation would be a lie, so they retire into Recents instead.
fn plan_restore(entry: &LedgerEntry, restore_enabled: bool, workspace_exists: bool) -> RestorePlan {
    match &entry.agent {
        None if restore_enabled && workspace_exists => RestorePlan::Respawn,
        None => RestorePlan::Drop,
        Some(agent) => {
            if restore_enabled && workspace_exists && agent.kind == AgentKind::Claude {
                RestorePlan::Respawn
            } else {
                RestorePlan::Retire
            }
        }
    }
}

/// The resume id to actually pass, verified against disk: the recorded
/// transcript path, or the conventional store location for this cwd. A
/// conversation whose transcript does not exist gets `None` — resuming it
/// would only produce an instantly-dead "No conversation found" pane.
/// (Claude 2.1.204 interactive sessions persist no transcript, so this is
/// a normal case, not a corruption case.)
fn resolve_resume(claude_projects_dir: &std::path::Path, entry: &LedgerEntry) -> Option<String> {
    let agent = entry.agent.as_ref()?;
    let id = agent.resume.as_deref()?;
    let recorded = agent
        .transcript
        .as_deref()
        .is_some_and(|path| path.is_file());
    let derived = claude_projects_dir
        .join(crate::launcher::encode_cwd(&entry.cwd))
        .join(id)
        .with_extension("jsonl")
        .is_file();
    (recorded || derived).then(|| id.to_string())
}

async fn respawn(
    state: &Arc<AppState>,
    entry: &LedgerEntry,
    workspace: crate::workspaces::Workspace,
) -> anyhow::Result<()> {
    // A cwd deleted while the daemon was down falls back to the workspace
    // root — a shell somewhere beats no shell.
    let cwd = if entry.cwd.is_dir() {
        entry.cwd.clone()
    } else {
        workspace.root.clone()
    };
    let mut title_hint = None;
    let kind = match &entry.agent {
        None => crate::spawn::SpawnKind::Shell,
        Some(agent) => {
            let resume = resolve_resume(&state.claude_projects_dir, entry);
            if resume.is_none() && agent.resume.is_some() {
                tracing::info!(session = %entry.id,
                    "conversation transcript is gone; respawning fresh instead of --resume");
            }
            // Whether resuming or starting over, the successor keeps the old
            // display title until claude re-titles it — the rail row must
            // not reset to a bare "claude".
            title_hint = Some(agent.title.clone());
            crate::spawn::SpawnKind::Agent {
                kind: agent.kind,
                model: None,
                resume,
            }
        }
    };
    let spec = crate::spawn::SpawnSpec {
        workspace,
        id: Some(entry.id.clone()),
        name: entry.pinned_name.clone(),
        cwd: Some(cwd),
        cols: Some(entry.cols),
        rows: Some(entry.rows),
        theme: entry.theme.clone(),
        title_hint,
        kind,
    };
    match crate::spawn::spawn_session(state, spec).await {
        Ok(_) => Ok(()),
        Err(crate::spawn::SpawnFailure::AgentUnavailable(msg)) => anyhow::bail!("{msg}"),
        Err(crate::spawn::SpawnFailure::Internal(err)) => Err(err),
    }
}

/// Remember a non-resurrectable agent conversation in its workspace's
/// Recents, honoring `retire()`'s rules (untitled claude boots carry
/// nothing recognizable and are skipped). Returns whether a row landed.
fn retire_to_recents(state: &Arc<AppState>, entry: &LedgerEntry, last_active: u64) -> bool {
    let Some(agent) = &entry.agent else {
        return false; // shells have no conversation to remember
    };
    let title = entry
        .pinned_name
        .clone()
        .unwrap_or_else(|| agent.title.clone());
    if agent.kind == AgentKind::Claude && title == agent.kind.as_str() {
        return false;
    }
    let recent = crate::recents::RecentEntry {
        kind: agent.kind,
        title,
        // Only promise resumption the transcript can actually deliver; a
        // row without `resume` honestly starts fresh (existing UI rule).
        resume: resolve_resume(&state.claude_projects_dir, entry),
        supersedes: Vec::new(),
        last_active: if last_active > 0 {
            last_active
        } else {
            unix_now()
        },
    };
    crate::lock(&state.recents).push(&entry.workspace_id, recent, None);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_entry() -> LedgerEntry {
        LedgerEntry {
            id: "s-1".into(),
            workspace_id: "w1".into(),
            cwd: PathBuf::from("/tmp"),
            pinned_name: None,
            cols: 120,
            rows: 40,
            theme: "dark".into(),
            agent: None,
        }
    }

    fn agent_entry(kind: AgentKind, resume: Option<&str>) -> LedgerEntry {
        LedgerEntry {
            agent: Some(LedgerAgent {
                kind,
                resume: resume.map(str::to_string),
                transcript: None,
                title: "fix the tests".into(),
            }),
            ..shell_entry()
        }
    }

    /// `--resume` is only passed when the conversation's transcript exists —
    /// at the hook-recorded path or the conventional store location. Claude
    /// 2.1.204 interactive sessions write no transcript, so a missing file
    /// is the NORMAL case, and resuming it would spawn an instantly-dead
    /// "No conversation found" pane.
    #[test]
    fn resume_requires_a_transcript_on_disk() {
        let dir = std::env::temp_dir().join(format!("chimaera-resume-test-{}", std::process::id()));
        let store = dir.join("projects");
        let cwd = dir.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();

        let mut entry = agent_entry(AgentKind::Claude, Some("conv-1"));
        entry.cwd = cwd.clone();

        // No transcript anywhere: no resume.
        assert_eq!(resolve_resume(&store, &entry), None);

        // The conventional store location for this cwd counts.
        let derived_dir = store.join(crate::launcher::encode_cwd(&cwd));
        std::fs::create_dir_all(&derived_dir).unwrap();
        std::fs::write(derived_dir.join("conv-1.jsonl"), "{}\n").unwrap();
        assert_eq!(resolve_resume(&store, &entry), Some("conv-1".to_string()));
        std::fs::remove_file(derived_dir.join("conv-1.jsonl")).unwrap();

        // The hook-recorded path counts wherever it points.
        let recorded = dir.join("elsewhere-conv-1.jsonl");
        std::fs::write(&recorded, "{}\n").unwrap();
        entry.agent.as_mut().unwrap().transcript = Some(recorded);
        assert_eq!(resolve_resume(&store, &entry), Some("conv-1".to_string()));

        // Shells and resume-less agents never resolve.
        assert_eq!(resolve_resume(&store, &shell_entry()), None);
        assert_eq!(
            resolve_resume(&store, &agent_entry(AgentKind::Claude, None)),
            None
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn restore_policy() {
        // Shells and claude respawn; other agents retire (no verified resume).
        assert_eq!(
            plan_restore(&shell_entry(), true, true),
            RestorePlan::Respawn
        );
        assert_eq!(
            plan_restore(&agent_entry(AgentKind::Claude, Some("abc")), true, true),
            RestorePlan::Respawn
        );
        // Claude without a transcript still respawns — nothing to lose.
        assert_eq!(
            plan_restore(&agent_entry(AgentKind::Claude, None), true, true),
            RestorePlan::Respawn
        );
        assert_eq!(
            plan_restore(&agent_entry(AgentKind::Codex, None), true, true),
            RestorePlan::Retire
        );
        // Restore disabled: conversations must still be findable, shells go.
        assert_eq!(plan_restore(&shell_entry(), false, true), RestorePlan::Drop);
        assert_eq!(
            plan_restore(&agent_entry(AgentKind::Claude, Some("abc")), false, true),
            RestorePlan::Retire
        );
        // Workspace unregistered while the daemon was down: nowhere to spawn,
        // but the conversation is still worth remembering.
        assert_eq!(plan_restore(&shell_entry(), true, false), RestorePlan::Drop);
        assert_eq!(
            plan_restore(&agent_entry(AgentKind::Claude, Some("abc")), true, false),
            RestorePlan::Retire
        );
    }

    #[test]
    fn ledger_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("chimaera-ledger-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.json");
        let mut store = LedgerStore::new(path.clone());

        let entries = vec![
            shell_entry(),
            LedgerEntry {
                pinned_name: Some("data wrangling".into()),
                ..agent_entry(AgentKind::Claude, Some("conv-1"))
            },
        ];
        let links: HashMap<String, String> = [("s-1".to_string(), "s-2".to_string())].into();
        store.write_if_changed(&entries, &links);

        let boot = LedgerStore::new(path.clone()).load_boot();
        assert_eq!(boot.sessions, entries);
        assert_eq!(boot.links.get("s-1").map(String::as_str), Some("s-2"));
        assert!(boot.written_at > 0);

        // Unchanged snapshots skip the write (mtime stays put).
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        store.write_if_changed(&entries, &links);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);

        // A missing or corrupt file is an empty ledger, never an error.
        std::fs::write(&path, "not json").unwrap();
        assert!(LedgerStore::new(path.clone())
            .load_boot()
            .sessions
            .is_empty());
        std::fs::remove_file(&path).unwrap();
        assert!(LedgerStore::new(path).load_boot().sessions.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }
}
