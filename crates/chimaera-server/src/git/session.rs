//! Sessions know their repository and branch.
//!
//! Every live session has a current folder: a shell's polled cwd, the `cwd`
//! every claude hook payload carries (an agent that `cd`s or enters a
//! worktree mid-session moves with it), else the folder it was spawned in.
//! The tracker resolves the checkout containing that folder (one
//! `rev-parse`, cached per folder) and reads its branch from the `HEAD`
//! file (no process). It recomputes only when a session's folder changes or
//! a git epoch moves — never on a timer: it wakes on the change bus, the
//! same edge the events socket rides.
//!
//! Alongside, per session (in memory, bounded): the anchor at start, the
//! latest anchor (claude turn ends), and the anchor at end — the small API
//! `GET /sessions/{id}/git` exposes and the session record reads.
//!
//! And the one mutation: while an agent runs inside a worktree chimaera
//! manages, that worktree is locked (`git worktree lock --reason "chimaera:
//! <session>"`) so other tools' clean-up sweeps leave it alone; the lock is
//! released when the last agent inside it ends. Locks another tool made are
//! never touched, and stale chimaera locks are reconciled at daemon start.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::AppState;

use super::anchor::{capture_in, commits_between, Anchor};
use super::parse::{read_head_blocking, HeadRef, RepoInfo};
use super::service::{configured_git, probe_repo, run_git, ProbeOutcome};

/// The lock reason prefix that marks a lock as chimaera's own. Anything
/// else is another tool's lock and is never touched.
const LOCK_PREFIX: &str = "chimaera:";

/// Recently ended sessions whose anchors stay readable (the route, and the
/// session record written when a session ends).
const ENDED_CAP: usize = 128;

/// Folder → probe cache bound: past it the cache is dropped wholesale (it
/// refills one `rev-parse` per distinct live folder).
const PROBE_CACHE_CAP: usize = 256;

/// The longest cwd accepted from a hook payload.
const MAX_CWD_BYTES: usize = 4096;

/// A floor between tracker passes: a burst of change-bus wakes (a chat
/// streaming, a roster churn) folds into one pass.
const PASS_GAP: Duration = Duration::from_millis(250);

/// Everything the tracker knows, shared with the session rows and the route.
#[derive(Default)]
pub(crate) struct SessionGits {
    /// The latest `cwd` from each agent session's claude hook payloads.
    hook_cwds: Mutex<HashMap<String, PathBuf>>,
    /// Live sessions the tracker has resolved.
    live: Mutex<HashMap<String, Tracked>>,
    /// Recently ended sessions' anchors, oldest first.
    ended: Mutex<VecDeque<(String, Ended)>>,
    /// Folder → checkout (`None` = not in a repository), so a folder costs
    /// one `rev-parse` however many sessions sit in it.
    probes: Mutex<HashMap<PathBuf, Option<RepoInfo>>>,
    /// Managed worktrees chimaera locked, and the session each names.
    locks: Mutex<HashMap<PathBuf, String>>,
}

#[derive(Clone)]
struct Tracked {
    /// The folder the checkout was resolved from.
    cwd: PathBuf,
    repo: Option<RepoInfo>,
    head: HeadRef,
    agent: bool,
    start: Option<Anchor>,
    /// The latest anchor captured after start (claude turn ends).
    latest: Option<Anchor>,
}

#[derive(Clone)]
struct Ended {
    start: Option<Anchor>,
    end: Option<Anchor>,
}

impl Tracked {
    /// The additive `git` field on a session row: `null` outside a repo.
    fn row(&self) -> serde_json::Value {
        let Some(repo) = &self.repo else {
            return serde_json::Value::Null;
        };
        let (branch, detached, head) = match &self.head {
            HeadRef::Branch(name) => (Some(name.as_str()), false, None),
            HeadRef::Detached(sha) => (None, true, Some(sha.chars().take(7).collect::<String>())),
            HeadRef::Unknown => (None, false, None),
        };
        json!({
            "repo": repo.repo_path().to_string_lossy(),
            "worktree": repo.toplevel.to_string_lossy(),
            "branch": branch,
            "detached": detached,
            "head": head,
        })
    }
}

/// What the session rows need from the tracker, per session id.
pub(crate) struct SessionRow {
    /// An agent's hook-reported folder (its `cwd_current`).
    pub(crate) hook_cwd: Option<PathBuf>,
    /// The `git` field.
    pub(crate) git: serde_json::Value,
}

impl SessionGits {
    /// Record the `cwd` a claude hook payload carried. Returns whether it
    /// moved (the caller announces the change). Only absolute, bounded
    /// paths are taken — the value is a directory git will run in.
    pub(crate) fn note_hook_cwd(&self, id: &str, cwd: &str) -> bool {
        if cwd.is_empty() || cwd.len() > MAX_CWD_BYTES || cwd.contains('\0') {
            return false;
        }
        let path = PathBuf::from(cwd);
        if !path.is_absolute() {
            return false;
        }
        let mut cwds = crate::lock(&self.hook_cwds);
        if cwds.get(id) == Some(&path) {
            return false;
        }
        cwds.insert(id.to_string(), path);
        true
    }

    /// Per-session facts for the session rows (built once per change
    /// generation, so a small clone).
    pub(crate) fn rows(&self) -> HashMap<String, SessionRow> {
        let hooks = crate::lock(&self.hook_cwds).clone();
        let live = crate::lock(&self.live);
        let mut out: HashMap<String, SessionRow> = HashMap::with_capacity(live.len());
        for (id, tracked) in live.iter() {
            out.insert(
                id.clone(),
                SessionRow {
                    hook_cwd: hooks.get(id).cloned(),
                    git: tracked.row(),
                },
            );
        }
        for (id, cwd) in hooks {
            out.entry(id).or_insert(SessionRow {
                hook_cwd: Some(cwd),
                git: serde_json::Value::Null,
            });
        }
        out
    }

    /// The checkout a live session is in, if the tracker resolved one.
    pub(crate) fn repo_of(&self, id: &str) -> Option<RepoInfo> {
        crate::lock(&self.live).get(id).and_then(|t| t.repo.clone())
    }

    fn cached_probe(&self, cwd: &Path) -> Option<Option<RepoInfo>> {
        crate::lock(&self.probes).get(cwd).cloned()
    }

    fn cache_probe(&self, cwd: PathBuf, repo: Option<RepoInfo>) {
        let mut probes = crate::lock(&self.probes);
        if probes.len() >= PROBE_CACHE_CAP {
            probes.clear();
        }
        probes.insert(cwd, repo);
    }

    /// A git epoch moved: a folder that was not a repository may be one now
    /// (`git init`, a clone), so negative answers are asked again.
    fn forget_negative_probes(&self) {
        crate::lock(&self.probes).retain(|_, repo| repo.is_some());
    }

    fn push_ended(&self, id: String, ended: Ended) {
        let mut list = crate::lock(&self.ended);
        list.retain(|(other, _)| *other != id);
        if list.len() >= ENDED_CAP {
            list.pop_front();
        }
        list.push_back((id, ended));
    }

    fn ended(&self, id: &str) -> Option<Ended> {
        crate::lock(&self.ended)
            .iter()
            .rev()
            .find(|(other, _)| other == id)
            .map(|(_, e)| e.clone())
    }
}

/// One live session as the tracker sees it this pass.
struct LiveSession {
    id: String,
    cwd: PathBuf,
    agent: bool,
}

/// Every live session and its current folder. Each registry lock is taken
/// and dropped on its own — never nested.
fn live_sessions(state: &AppState) -> (Vec<LiveSession>, BTreeSet<String>) {
    let ptys = state.sessions.list();
    let chats = state.chat.list();
    let agents: BTreeSet<String> = crate::lock(&state.agents).keys().cloned().collect();
    let polled = crate::lock(&state.current_cwds).clone();
    let hooks = crate::lock(&state.git.sessions.hook_cwds).clone();
    // Mid view-switch a session sits in neither registry for a moment; it
    // is still live (its record must not end and restart).
    let switching: BTreeSet<String> = crate::lock(&state.chat_switching).keys().cloned().collect();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for info in ptys.iter().filter(|i| i.alive) {
        let agent = agents.contains(&info.id);
        let cwd = if agent {
            hooks.get(&info.id).cloned()
        } else {
            polled.get(&info.id).cloned()
        }
        .unwrap_or_else(|| info.cwd.clone());
        seen.insert(info.id.clone());
        out.push(LiveSession {
            id: info.id.clone(),
            cwd,
            agent,
        });
    }
    for info in chats.iter().filter(|c| c.alive) {
        if !seen.insert(info.id.clone()) {
            continue;
        }
        let cwd = hooks
            .get(&info.id)
            .cloned()
            .unwrap_or_else(|| info.cwd.clone());
        out.push(LiveSession {
            id: info.id.clone(),
            cwd,
            agent: true,
        });
    }
    (out, switching)
}

/// The checkout containing `cwd`: the probe cache, else one `rev-parse`.
async fn resolve_cwd(state: &AppState, git: &Path, cwd: &Path) -> Option<RepoInfo> {
    if let Some(cached) = state.git.sessions.cached_probe(cwd) {
        return cached;
    }
    let repo = match probe_repo(git, &state.git.procs, cwd).await {
        ProbeOutcome::Repo(repo) => Some(repo),
        // A transient failure is not cached: the next pass asks again.
        ProbeOutcome::Error(_) => return None,
        ProbeOutcome::NotARepo => None,
    };
    state
        .git
        .sessions
        .cache_probe(cwd.to_path_buf(), repo.clone());
    repo
}

/// Read several `HEAD` files off the reactor in one blocking task.
async fn read_heads(git_dirs: Vec<PathBuf>) -> HashMap<PathBuf, HeadRef> {
    tokio::task::spawn_blocking(move || {
        git_dirs
            .into_iter()
            .map(|dir| {
                let head = read_head_blocking(&dir);
                (dir, head)
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// The tracker task: wakes on the change bus, reconciles, repeats.
pub(crate) async fn track_sessions(state: Arc<AppState>) {
    state.wait_restored().await;
    reconcile_locks(&state).await;
    let mut seen_generation: Option<u64> = None;
    let mut seen_epochs: HashMap<String, u64> = HashMap::new();
    loop {
        // Registered BEFORE the generation is read, so a change landing
        // between the read and the wait still wakes this loop.
        let notified = state.changes.subscribe();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let generation = state.changes.generation();
        if seen_generation == Some(generation) {
            notified.await;
            continue;
        }
        seen_generation = Some(generation);
        pass(&state, &mut seen_epochs).await;
        tokio::time::sleep(PASS_GAP).await;
    }
}

/// One reconcile pass. Git runs only for folders that changed (a probe)
/// and sessions that started or ended (an anchor); a moved epoch costs a
/// `HEAD` file read per checkout.
async fn pass(state: &Arc<AppState>, seen_epochs: &mut HashMap<String, u64>) {
    let git = state.git.resolve_git(configured_git(state)).await;
    let epochs = state.git.epochs_snapshot();
    let epochs_moved = *seen_epochs != epochs;
    if epochs_moved {
        state.git.sessions.forget_negative_probes();
        *seen_epochs = epochs;
    }
    let (live, switching) = live_sessions(state);
    // A hook cwd outlives nothing: drop the ones whose agent record is gone
    // (a session that ended before this tracker ever saw it).
    {
        let agents: BTreeSet<String> = crate::lock(&state.agents).keys().cloned().collect();
        crate::lock(&state.git.sessions.hook_cwds).retain(|id, _| agents.contains(id));
    }
    let previous: HashMap<String, Tracked> = crate::lock(&state.git.sessions.live).clone();
    let mut next: HashMap<String, Tracked> = HashMap::with_capacity(live.len());
    let mut started: Vec<String> = Vec::new();

    for session in &live {
        let prior = previous.get(&session.id);
        let mut tracked = match prior {
            Some(t) if t.cwd == session.cwd => t.clone(),
            _ => {
                let repo = if git.adequate {
                    resolve_cwd(state, &git.path, &session.cwd).await
                } else {
                    None
                };
                Tracked {
                    cwd: session.cwd.clone(),
                    repo,
                    head: HeadRef::Unknown,
                    agent: session.agent,
                    start: prior.and_then(|p| p.start.clone()),
                    latest: prior.and_then(|p| p.latest.clone()),
                }
            }
        };
        tracked.agent = session.agent;
        if tracked.repo.is_some() && tracked.start.is_none() {
            started.push(session.id.clone());
        }
        next.insert(session.id.clone(), tracked);
    }
    // A session mid view-switch keeps what it had.
    for id in &switching {
        if let Some(t) = previous.get(id) {
            next.entry(id.clone()).or_insert_with(|| t.clone());
        }
    }

    // Branches: re-read every checkout's HEAD when an epoch moved (a
    // checkout in a terminal, a commit), else only the newly resolved ones.
    let dirs: BTreeSet<PathBuf> = next
        .iter()
        .filter(|(id, t)| {
            epochs_moved
                || previous
                    .get(*id)
                    .is_none_or(|p| p.cwd != t.cwd || p.repo != t.repo)
        })
        .filter_map(|(_, t)| t.repo.as_ref().map(|r| r.git_dir.clone()))
        .collect();
    if !dirs.is_empty() {
        let heads = read_heads(dirs.into_iter().collect()).await;
        for tracked in next.values_mut() {
            if let Some(repo) = &tracked.repo {
                if let Some(head) = heads.get(&repo.git_dir) {
                    tracked.head = head.clone();
                }
            }
        }
    }

    // Start anchors for sessions that just landed in a repository.
    if git.adequate {
        for id in &started {
            let Some(repo) = next.get(id).and_then(|t| t.repo.clone()) else {
                continue;
            };
            let anchor = capture_in(state, &git.path, &repo).await;
            if let Some(t) = next.get_mut(id) {
                t.start = anchor;
            }
        }
    }

    // Ends: tracked before, gone now.
    let gone: Vec<(String, Tracked)> = previous
        .into_iter()
        .filter(|(id, _)| !next.contains_key(id))
        .collect();
    let row_changed = {
        let mut live_map = crate::lock(&state.git.sessions.live);
        let changed = live_map.len() != next.len()
            || next
                .iter()
                .any(|(id, t)| live_map.get(id).is_none_or(|p| p.row() != t.row()));
        *live_map = next.clone();
        changed
    };
    for (id, tracked) in gone {
        let end = match (&tracked.repo, git.adequate) {
            (Some(repo), true) => capture_in(state, &git.path, repo).await,
            _ => None,
        };
        state.git.sessions.push_ended(
            id.clone(),
            Ended {
                start: tracked.start.clone(),
                end: end.or(tracked.latest.clone()),
            },
        );
        crate::lock(&state.git.sessions.hook_cwds).remove(&id);
    }

    if git.adequate {
        sync_locks(state, &git.path, &next).await;
    }
    if row_changed {
        state.changes.notify_waiters();
    }
}

/// A claude turn ended: capture the session's latest anchor so an abrupt
/// end still has a recent "where it stood". Queued, so the hook answers
/// first.
pub(crate) fn session_turn_end(state: &Arc<AppState>, id: &str) {
    let Some(repo) = state.git.sessions.repo_of(id) else {
        return;
    };
    let state = state.clone();
    let id = id.to_string();
    tokio::spawn(async move {
        let git = state.git.resolve_git(configured_git(&state)).await;
        if !git.adequate {
            return;
        }
        let anchor = capture_in(&state, &git.path, &repo).await;
        if let Some(t) = crate::lock(&state.git.sessions.live).get_mut(&id) {
            if t.repo.as_ref() == Some(&repo) {
                t.latest = anchor;
            }
        }
    });
}

// ---- worktree locks ---------------------------------------------------------

/// The managed root, resolved the way git prints worktree paths.
async fn managed_root(state: &AppState) -> PathBuf {
    let root = state.worktrees_root.clone();
    tokio::task::spawn_blocking(move || std::fs::canonicalize(&root).unwrap_or(root))
        .await
        .unwrap_or_else(|_| state.worktrees_root.clone())
}

/// A linked worktree's lock reason (`<git_dir>/locked`), if it is locked.
fn lock_reason_blocking(git_dir: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(git_dir.join("locked")).ok()?;
    let mut buf = String::new();
    file.take(4096).read_to_string(&mut buf).ok()?;
    Some(buf.trim().to_string())
}

/// The session id a chimaera lock names, or `None` for another tool's lock.
fn chimaera_lock_session(reason: &str) -> Option<&str> {
    reason
        .strip_prefix(LOCK_PREFIX)
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// Lock or unlock one worktree. Failures are logged, never fatal: a lock is
/// a courtesy to other tools' clean-up sweeps, not a guarantee chimaera
/// depends on.
async fn set_lock(state: &AppState, git: &Path, worktree: &Path, session: Option<&str>) -> bool {
    let path = worktree.to_string_lossy().into_owned();
    let reason;
    let args: Vec<&str> = match session {
        Some(id) => {
            reason = format!("{LOCK_PREFIX} {id}");
            vec!["worktree", "lock", "--reason", &reason, &path]
        }
        None => vec!["worktree", "unlock", &path],
    };
    match run_git(git, &state.git.procs, worktree, &args, 4096).await {
        Ok(out) if out.success => true,
        Ok(out) => {
            tracing::debug!(worktree = %path, stderr = %out.stderr, "worktree lock change refused");
            false
        }
        Err(err) => {
            tracing::debug!(worktree = %path, %err, "worktree lock change failed");
            false
        }
    }
}

/// Keep each managed worktree locked exactly while an agent session runs
/// inside it, naming one of those sessions. Only locks chimaera made (or
/// finds unlocked) are changed.
async fn sync_locks(state: &AppState, git: &Path, live: &HashMap<String, Tracked>) {
    let managed = managed_root(state).await;
    // worktree → (its checkout, the agent sessions inside), sorted so the
    // named session is stable across passes.
    let mut wanted: BTreeMap<PathBuf, (RepoInfo, BTreeSet<String>)> = BTreeMap::new();
    for (id, t) in live {
        let Some(repo) = &t.repo else { continue };
        if !t.agent || !repo.toplevel.starts_with(&managed) || repo.git_dir == repo.common_dir {
            continue;
        }
        wanted
            .entry(repo.toplevel.clone())
            .or_insert_with(|| (repo.clone(), BTreeSet::new()))
            .1
            .insert(id.clone());
    }
    let held = crate::lock(&state.git.sessions.locks).clone();
    // Lock (or re-name) where agents are.
    for (worktree, (repo, ids)) in &wanted {
        if held.get(worktree).is_some_and(|named| ids.contains(named)) {
            continue;
        }
        let git_dir = repo.git_dir.clone();
        let reason = tokio::task::spawn_blocking(move || lock_reason_blocking(&git_dir))
            .await
            .ok()
            .flatten();
        let Some(first) = ids.iter().next() else {
            continue;
        };
        match reason.as_deref() {
            None => {
                if set_lock(state, git, worktree, Some(first)).await {
                    crate::lock(&state.git.sessions.locks).insert(worktree.clone(), first.clone());
                }
            }
            Some(reason) => match chimaera_lock_session(reason) {
                Some(named) if ids.contains(named) => {
                    crate::lock(&state.git.sessions.locks)
                        .insert(worktree.clone(), named.to_string());
                }
                // Ours, naming a session that has left: re-name it.
                Some(_) => {
                    set_lock(state, git, worktree, None).await;
                    if set_lock(state, git, worktree, Some(first)).await {
                        crate::lock(&state.git.sessions.locks)
                            .insert(worktree.clone(), first.clone());
                    }
                }
                // Another tool's lock: leave it exactly as it is.
                None => {}
            },
        }
    }
    // Unlock where the last agent left.
    for (worktree, _) in held {
        if wanted.contains_key(&worktree) {
            continue;
        }
        crate::lock(&state.git.sessions.locks).remove(&worktree);
        let Some(git_dir) = linked_git_dir(&worktree).await else {
            continue;
        };
        let reason = tokio::task::spawn_blocking(move || lock_reason_blocking(&git_dir))
            .await
            .ok()
            .flatten();
        if reason.as_deref().and_then(chimaera_lock_session).is_some() {
            set_lock(state, git, &worktree, None).await;
        }
    }
}

/// A linked worktree's git dir, from its `.git` file (`gitdir: <path>`).
async fn linked_git_dir(worktree: &Path) -> Option<PathBuf> {
    let worktree = worktree.to_path_buf();
    tokio::task::spawn_blocking(move || super::repos::gitdir_of_blocking(&worktree))
        .await
        .ok()
        .flatten()
}

/// At daemon start (after the ledger restored its sessions): release every
/// chimaera lock under the managed root whose session is not alive — the
/// daemon that made it went away without unlocking. Bounded walk; symlinks
/// never followed; other tools' locks untouched.
async fn reconcile_locks(state: &Arc<AppState>) {
    let git = state.git.resolve_git(configured_git(state)).await;
    if !git.adequate {
        return;
    }
    let managed = managed_root(state).await;
    let found = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        for worktree in super::repos::managed_worktrees_blocking(&managed) {
            let Some(git_dir) = super::repos::gitdir_of_blocking(&worktree) else {
                continue;
            };
            if let Some(reason) = lock_reason_blocking(&git_dir) {
                if let Some(id) = chimaera_lock_session(&reason) {
                    out.push((worktree, id.to_string()));
                }
            }
        }
        out
    })
    .await
    .unwrap_or_default();
    for (worktree, id) in found {
        if crate::chat::session_alive(state, &id) {
            continue;
        }
        tracing::info!(worktree = %worktree.display(), session = %id, "releasing a stale chimaera worktree lock");
        set_lock(state, &git.path, &worktree, None).await;
    }
}

// ---- route --------------------------------------------------------------------

/// GET /api/v1/sessions/{id}/git — where a session's repository stood when
/// it started, where it stands now (or stood when it ended), and the
/// commits in between (≤50, newest first). Nulls outside a repository.
pub(crate) async fn session_git(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let tracked = crate::lock(&state.git.sessions.live).get(&id).cloned();
    let live = crate::chat::session_alive(&state, &id);
    let (start, current, dir) = if let Some(t) = tracked {
        let git = state.git.resolve_git(configured_git(&state)).await;
        let current = match (&t.repo, git.adequate) {
            (Some(repo), true) => capture_in(&state, &git.path, repo).await,
            _ => None,
        };
        (t.start, current, t.repo.map(|r| r.toplevel))
    } else if let Some(ended) = state.git.sessions.ended(&id) {
        let dir = ended
            .end
            .as_ref()
            .or(ended.start.as_ref())
            .map(|a| a.worktree.clone());
        (ended.start, ended.end, dir)
    } else if live {
        // Alive but not reconciled yet (it just started): where it stands
        // now, from the folder it was started in.
        let current = match crate::chat::session_root(&state, &id) {
            Some(dir) => super::anchor::capture(&state, &dir).await,
            None => None,
        };
        (None, current, None)
    } else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown session {id}")})),
        )
            .into_response();
    };

    let mut body = json!({
        "session_id": id,
        "live": live,
        "start": start.as_ref().map(Anchor::json),
        "current": current.as_ref().map(Anchor::json),
        "commits": [],
        "truncated": false,
        "rewritten": false,
        "branch_changed": false,
        "repo_changed": false,
    });
    if let (Some(start), Some(current)) = (&start, &current) {
        if start.repo != current.repo {
            body["repo_changed"] = json!(true);
        } else {
            body["branch_changed"] = json!(start.branch != current.branch);
            if let (Some(old), Some(new), Some(dir)) = (&start.head, &current.head, &dir) {
                if let Some(between) = commits_between(&state, dir, old, new).await {
                    body["commits"] = between.commits.iter().map(|c| c.json()).collect();
                    body["truncated"] = json!(between.truncated);
                    // A branch switch is not a rewrite: only a same-branch
                    // HEAD that no longer descends is.
                    body["rewritten"] = json!(between.rewritten && start.branch == current.branch);
                }
            }
        }
    }
    Json(body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_cwds_take_only_absolute_bounded_paths() {
        let s = SessionGits::default();
        assert!(!s.note_hook_cwd("a", "relative/dir"));
        assert!(!s.note_hook_cwd("a", ""));
        assert!(!s.note_hook_cwd("a", &format!("/{}", "x".repeat(MAX_CWD_BYTES))));
        assert!(s.note_hook_cwd("a", "/repo"));
        // Unchanged is not a change.
        assert!(!s.note_hook_cwd("a", "/repo"));
        assert!(s.note_hook_cwd("a", "/repo/.claude/worktrees/x"));
        let rows = s.rows();
        assert_eq!(
            rows["a"].hook_cwd.as_deref(),
            Some(Path::new("/repo/.claude/worktrees/x"))
        );
        assert!(rows["a"].git.is_null());
    }

    #[test]
    fn lock_reasons_are_ours_only_with_the_prefix() {
        assert_eq!(chimaera_lock_session("chimaera: s-123"), Some("s-123"));
        assert_eq!(chimaera_lock_session("chimaera:"), None);
        assert_eq!(chimaera_lock_session("claude agent agent-a1b2"), None);
        assert_eq!(chimaera_lock_session(""), None);
    }

    #[test]
    fn a_session_row_names_repo_worktree_and_branch() {
        let repo = RepoInfo {
            toplevel: PathBuf::from("/r/.claude/worktrees/x"),
            common_dir: PathBuf::from("/r/.git"),
            git_dir: PathBuf::from("/r/.git/worktrees/x"),
        };
        let mut t = Tracked {
            cwd: PathBuf::from("/r/.claude/worktrees/x/src"),
            repo: Some(repo),
            head: HeadRef::Branch("feat/x".into()),
            agent: true,
            start: None,
            latest: None,
        };
        let row = t.row();
        assert_eq!(row["repo"], "/r");
        assert_eq!(row["worktree"], "/r/.claude/worktrees/x");
        assert_eq!(row["branch"], "feat/x");
        assert_eq!(row["detached"], false);
        t.head = HeadRef::Detached("0123456789abcdef".into());
        assert_eq!(t.row()["head"], "0123456");
        assert!(t.row()["branch"].is_null());
        t.repo = None;
        assert!(t.row().is_null());
    }
}
