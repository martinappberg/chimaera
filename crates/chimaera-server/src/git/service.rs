use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::AppState;

#[cfg(test)]
use super::parse::Entry;
use super::parse::{
    hash_status, parse_status, parse_worktrees, RepoInfo, StatusData, WorktreeInfo,
};
use super::resolve::{resolve_git_binary, GitBinary};

/// Kill any git child that outlives this. A hung status on a wedged network
/// filesystem must never pin a daemon task.
const GIT_TIMEOUT: Duration = Duration::from_secs(8);

/// Hard ceiling on a single git invocation's stdout. `status`/`diff` on a
/// pathological tree is truncated rather than buffered unbounded.
pub(super) const MAX_STATUS_OUTPUT: usize = 8 * 1024 * 1024;

/// Daemon-wide ceiling on concurrent git child processes (bounds CPU on a
/// shared node — an accidental fan-out of statuses cannot saturate cores).
const MAX_CONCURRENT_GIT: usize = 4;

/// Backstop cadence: catches out-of-band changes (external editor, a `git`
/// command in a terminal) that fire none of the event-driven refresh triggers.
const BACKSTOP_INTERVAL: Duration = Duration::from_secs(12);

/// How long one workspace's COMPLETED status keeps serving callers that
/// arrive after it finished. Callers that were already queued when a run
/// completes take its result regardless of this window (the single-flight
/// join — see [`StatusShare`]); the window only extends sharing to the
/// stragglers of the same fan-out. Event-driven invalidations (`invalidate`)
/// flush the share, so a refetch after a save never reuses a pre-save
/// result. Out-of-band git ops (a `git commit` in a terminal) fire no
/// invalidation, so the HTTP path accepts ≤ this much staleness there — the
/// MCP path does not (see [`GitService::status_fresh`]).
const STATUS_REUSE: Duration = Duration::from_secs(1);

/// The status-share / published-hash key of one repository in one
/// workspace. The primary repository uses its own top level too, so a
/// request naming it and one naming nothing share one slot.
pub(super) fn repo_slot(ws_id: &str, toplevel: &Path) -> String {
    format!("{ws_id}\u{0}{}", toplevel.display())
}

fn slot_prefix(ws_id: &str) -> String {
    format!("{ws_id}\u{0}")
}

/// The read-only git service: discovery cache, per-workspace nudge epochs, and
/// the concurrency permit shared by every invocation.
pub(crate) struct GitService {
    /// workspace id -> discovered repo (`None` = not a repo). A repo root is
    /// stable so a `Some` is cached for the daemon's life; a `None` is re-probed
    /// on demand, so `git init` in an already-open workspace eventually surfaces.
    repos: Mutex<HashMap<String, Option<RepoInfo>>>,
    /// workspace id -> the repositories found BELOW its root (see `repos`).
    pub(super) repo_sets: Mutex<HashMap<String, super::repos::RepoSet>>,
    /// workspace id -> nudge epoch, bumped whenever that workspace's git state
    /// may have changed. Surfaced on `/ws/events` so the client refetches; the
    /// payload never rides the firehose (invalidate-and-pull).
    epochs: Mutex<HashMap<String, u64>>,
    /// workspace id -> repository top level -> its own epoch, bumped with the
    /// workspace's when that repository changed: a window with several
    /// repositories refetches only the ones that moved.
    repo_epochs: Mutex<HashMap<String, BTreeMap<PathBuf, u64>>>,
    /// (workspace id, repository top level) -> how many windows have that
    /// repository's section open or a file inside it mounted. Nested
    /// repositories are backstop-polled only while watched this way.
    repo_watchers: Mutex<HashMap<(String, PathBuf), usize>>,
    /// workspace id -> how many connected clients are LOOKING at it (registered
    /// over `/ws/events`, released on disconnect). This gates the backstop poll.
    ///
    /// Deliberately not "was pulled recently": pulls only happen when something
    /// changed, so a recency window decays to zero on a quiet repo and the
    /// backstop would stop watching exactly when it is needed.
    watchers: Mutex<HashMap<String, usize>>,
    /// repository slot ([`repo_slot`]) -> hash of the last computed status,
    /// so the backstop only bumps the epoch when something actually changed.
    hashes: Mutex<HashMap<String, u64>>,
    /// The resolved git binary, cached keyed by the `git.path` setting so an
    /// edit re-resolves. Resolution runs a login shell (to pick up a
    /// module-loaded git in the user's dotfiles), so it must not happen per
    /// invocation — every git call reads the cached path.
    resolved_git: Mutex<Option<(Option<String>, Arc<GitBinary>)>>,
    /// Bounds concurrent `git` processes across the whole daemon.
    pub(super) procs: Arc<Semaphore>,
    /// Per-repository single-flight + short reuse for status runs, keyed by
    /// [`repo_slot`].
    status_share: StatusShare,
    /// Which repository and branch each live session is in, its anchors,
    /// and the managed-worktree locks held for agents (see `session`).
    pub(crate) sessions: super::session::SessionGits,
    /// (base sha, worktree sha) -> (behind, ahead): the Branches rows' counts
    /// against the main checkout's branch. Two commits never change their
    /// distance, so a refresh re-runs `rev-list` only when one of them moved.
    pub(super) vs_main: Mutex<HashMap<(String, String), (u64, u64)>>,
    /// (branch, sha) -> the branch never moved since it was created (a
    /// brand-new branch is not "merged", whatever its distance says).
    pub(super) fresh_branches: Mutex<HashMap<(String, String), bool>>,
}

/// Entries the ahead/behind cache keeps before it starts over.
pub(super) const VS_MAIN_CAP: usize = 256;

/// A status result from the share: the data, plus whether the run that
/// produced it was invalidated mid-flight. A flushed result is a valid
/// RESPONSE (it is what a direct run would have returned) but must not be
/// re-seeded as the published epoch baseline — the announced change's own
/// fan-out publishes the post-change status, and publishing pre-change data
/// here would force a second bump and a second full fan-out.
pub(super) struct SharedStatus {
    pub(super) data: Arc<StatusData>,
    pub(super) flushed: bool,
}

/// Single-flight for `git status`, per workspace. Concurrent callers queue
/// on one async run lock; the leader runs, and every caller that was already
/// WAITING when the run completes takes that run's outcome unconditionally —
/// success or failure — unless a flush invalidated it (classic single-flight:
/// you get the result of the run you waited on, so one epoch bump's fan-out
/// costs one `git status` no matter how long the run takes). The
/// [`STATUS_REUSE`] window applies only to callers arriving AFTER a run
/// completed, and never resurrects an error. Event-driven invalidations
/// ([`StatusShare::flush`]) drop the cached outcome — payload included, so a
/// dead result never stays pinned — and mark any in-flight run so its data
/// is served but neither shared nor published. Slots are evicted with their
/// workspace ([`GitService::forget_workspace`]); until then each pins at
/// most one parsed status.
struct StatusShare {
    slots: Mutex<HashMap<String, Arc<StatusSlot>>>,
}

#[derive(Default)]
struct StatusSlot {
    /// Serializes underlying runs for one workspace — the join point.
    /// Async, so queued callers yield instead of parking reactor workers;
    /// `run_git`'s semaphore + timeout still bound the process underneath.
    run_lock: tokio::sync::Mutex<()>,
    /// Sync-guarded bookkeeping. Every access is short and never held
    /// across an `.await`, which is what lets `flush` run from sync
    /// contexts while a leader is mid-run.
    inner: Mutex<SlotInner>,
}

#[derive(Default)]
struct SlotInner {
    /// Bumped by [`StatusShare::flush`]. A run that started before the
    /// current value must not be shared or published.
    flushes: u64,
    /// Completed-run counter. A caller snapshots it BEFORE queueing on
    /// `run_lock`; if it moved by the time the caller holds the lock, the
    /// caller waited out an in-flight run and joins its outcome.
    runs: u64,
    /// The last completed, un-flushed run's outcome. `flush` drops it —
    /// invalidation and payload release in one move.
    outcome: Option<RunOutcome>,
}

struct RunOutcome {
    /// When the run STARTED — the honest freshness bound for late arrivals
    /// (a slow run's result is already `run duration` old when it lands).
    started: Instant,
    /// Errors are kept ONLY so joiners of the failed run share the failure
    /// instead of serially eating their own timeout on a wedged repo; a
    /// caller arriving after the failure never reuses it.
    result: Result<Arc<StatusData>, String>,
}

impl StatusShare {
    fn new() -> Self {
        StatusShare {
            slots: Mutex::new(HashMap::new()),
        }
    }

    fn slot(&self, ws_id: &str) -> Arc<StatusSlot> {
        crate::lock(&self.slots)
            .entry(ws_id.to_string())
            .or_default()
            .clone()
    }

    /// Invalidate `key`'s share: a change was announced, so the next
    /// caller must recompute and any in-flight run must not be shared or
    /// published. Dropping the outcome also unpins its parsed payload.
    fn flush(&self, key: &str) {
        let slot = crate::lock(&self.slots).get(key).cloned();
        if let Some(slot) = slot {
            Self::flush_slot(&slot);
        }
    }

    fn flush_slot(slot: &StatusSlot) {
        let mut inner = crate::lock(&slot.inner);
        inner.flushes += 1;
        inner.outcome = None;
    }

    /// [`Self::flush`] every slot whose key starts with `prefix` (all of one
    /// workspace's repositories).
    fn flush_prefix(&self, prefix: &str) {
        let slots: Vec<Arc<StatusSlot>> = crate::lock(&self.slots)
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(_, v)| v.clone())
            .collect();
        for slot in slots {
            Self::flush_slot(&slot);
        }
    }

    /// Drop every slot whose key starts with `prefix` (the workspace is gone).
    fn evict_prefix(&self, prefix: &str) {
        crate::lock(&self.slots).retain(|k, _| !k.starts_with(prefix));
    }

    /// The single-flight entry: join the run this caller waited out, reuse
    /// a fresh completed result, or lead a new run.
    async fn get_or_run<F, Fut>(
        &self,
        ws_id: &str,
        reuse: Duration,
        run: F,
    ) -> anyhow::Result<SharedStatus>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<StatusData>>,
    {
        let slot = self.slot(ws_id);
        let arrived_runs = crate::lock(&slot.inner).runs;
        let _guard = slot.run_lock.lock().await;
        {
            let inner = crate::lock(&slot.inner);
            if let Some(outcome) = inner.outcome.as_ref() {
                // `joined`: a run completed while this caller queued — take
                // its outcome unconditionally (a flush would have dropped
                // it). Otherwise the caller arrived after completion, and
                // only a still-fresh SUCCESS is reusable.
                let joined = inner.runs != arrived_runs;
                match &outcome.result {
                    Ok(data) if joined || outcome.started.elapsed() < reuse => {
                        return Ok(SharedStatus {
                            data: data.clone(),
                            flushed: false,
                        });
                    }
                    Err(msg) if joined => anyhow::bail!("{msg}"),
                    _ => {}
                }
            }
        }
        Self::lead(&slot, run).await
    }

    /// Run under an already-held `run_lock`: execute, record the outcome for
    /// joiners and late arrivals, and report whether a flush landed mid-run.
    async fn lead<F, Fut>(slot: &StatusSlot, run: F) -> anyhow::Result<SharedStatus>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<StatusData>>,
    {
        let flushes_at_start = crate::lock(&slot.inner).flushes;
        let started = Instant::now();
        let result = run().await.map(Arc::new);
        let mut inner = crate::lock(&slot.inner);
        inner.runs += 1;
        let flushed = inner.flushes != flushes_at_start;
        inner.outcome = if flushed {
            // This run may predate the announced change: joiners re-run
            // (the first becomes the next leader) instead of sharing it.
            None
        } else {
            Some(RunOutcome {
                started,
                result: result
                    .as_ref()
                    .map(Arc::clone)
                    .map_err(|e| format!("{e:#}")),
            })
        };
        drop(inner);
        result.map(|data| SharedStatus { data, flushed })
    }
}

impl GitService {
    pub(crate) fn new() -> Self {
        GitService {
            repos: Mutex::new(HashMap::new()),
            repo_sets: Mutex::new(HashMap::new()),
            epochs: Mutex::new(HashMap::new()),
            repo_epochs: Mutex::new(HashMap::new()),
            repo_watchers: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            hashes: Mutex::new(HashMap::new()),
            resolved_git: Mutex::new(None),
            procs: Arc::new(Semaphore::new(MAX_CONCURRENT_GIT)),
            status_share: StatusShare::new(),
            sessions: Default::default(),
            vs_main: Mutex::new(HashMap::new()),
            fresh_branches: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve the git binary to use, honoring an explicit `git.path` setting
    /// (`configured`) and otherwise the user's login-shell git, then the daemon
    /// PATH. Cached and keyed by `configured`, so changing the setting (or
    /// clearing it) re-resolves on the next call and nothing else does.
    pub(super) async fn resolve_git(&self, configured: Option<String>) -> Arc<GitBinary> {
        {
            let cache = crate::lock(&self.resolved_git);
            if let Some((key, bin)) = cache.as_ref() {
                if *key == configured {
                    return bin.clone();
                }
            }
        }
        let bin = Arc::new(resolve_git_binary(configured.clone()).await);
        *crate::lock(&self.resolved_git) = Some((configured, bin.clone()));
        bin
    }

    pub(super) fn epoch(&self, ws_id: &str) -> u64 {
        crate::lock(&self.epochs).get(ws_id).copied().unwrap_or(0)
    }

    /// Snapshot of every known workspace epoch, for the `/ws/events` git frame.
    pub(crate) fn epochs_snapshot(&self) -> HashMap<String, u64> {
        crate::lock(&self.epochs).clone()
    }

    /// Bump a workspace's epoch (does not notify — the caller batches the wake).
    pub(super) fn bump(&self, ws_id: &str) {
        let mut epochs = crate::lock(&self.epochs);
        *epochs.entry(ws_id.to_string()).or_insert(0) += 1;
    }

    /// Bump one repository's epoch, and its workspace's with it (the
    /// workspace epoch is what single-repository windows follow).
    pub(super) fn bump_repo(&self, ws_id: &str, toplevel: &Path) {
        self.bump(ws_id);
        let mut epochs = crate::lock(&self.repo_epochs);
        *epochs
            .entry(ws_id.to_string())
            .or_default()
            .entry(toplevel.to_path_buf())
            .or_insert(0) += 1;
    }

    /// One repository's epoch (0 until it first moves).
    pub(super) fn repo_epoch(&self, ws_id: &str, toplevel: &Path) -> u64 {
        crate::lock(&self.repo_epochs)
            .get(ws_id)
            .and_then(|m| m.get(toplevel))
            .copied()
            .unwrap_or(0)
    }

    /// Every repository epoch, for the `/ws/events` git frame's `repos`.
    pub(crate) fn repo_epochs_snapshot(&self) -> HashMap<String, BTreeMap<PathBuf, u64>> {
        crate::lock(&self.repo_epochs).clone()
    }

    /// Forget the published hash: the next computed status is accepted as the new
    /// baseline WITHOUT a second epoch bump. Paired with an event-driven bump
    /// (a save / an agent write), whose change we have already announced. Also
    /// flushes the single-flight share — the refetch this announcement
    /// triggers must recompute, never reuse a pre-change result. Covers every
    /// repository of the workspace; [`Self::invalidate_repo`] covers one.
    pub(super) fn invalidate(&self, ws_id: &str) {
        let prefix = slot_prefix(ws_id);
        crate::lock(&self.hashes).retain(|k, _| !k.starts_with(&prefix));
        self.status_share.flush_prefix(&prefix);
    }

    /// [`Self::invalidate`] for one repository.
    pub(super) fn invalidate_repo(&self, ws_id: &str, toplevel: &Path) {
        let key = repo_slot(ws_id, toplevel);
        crate::lock(&self.hashes).remove(&key);
        self.status_share.flush(&key);
    }

    /// Drop everything held for a deleted workspace: the discovery caches,
    /// its epochs, the published-status hashes, and — the part that actually
    /// weighs something — the status share's slots with their parsed
    /// payloads. `watchers` are left alone: they are refcounted by connected
    /// clients and their guards release them (a stale entry there is a
    /// usize, and the backstop skips unknown workspace ids anyway).
    pub(crate) fn forget_workspace(&self, ws_id: &str) {
        let prefix = slot_prefix(ws_id);
        crate::lock(&self.repos).remove(ws_id);
        crate::lock(&self.repo_sets).remove(ws_id);
        crate::lock(&self.epochs).remove(ws_id);
        crate::lock(&self.repo_epochs).remove(ws_id);
        crate::lock(&self.hashes).retain(|k, _| !k.starts_with(&prefix));
        self.status_share.evict_prefix(&prefix);
    }

    /// Record a freshly computed status as the published baseline.
    ///
    /// If it differs from the previously published one, the world moved without
    /// an event trigger (an external editor, a `git` command in a terminal, or a
    /// change absorbed between polls) — so bump the epoch and let EVERY client
    /// refetch. Whoever computed it reports the post-bump epoch, so the caller's
    /// own client is already current and does not refetch. A first observation
    /// establishes the baseline silently: there is nothing to invalidate yet.
    ///
    /// This ownership matters: if a plain pull could overwrite the baseline
    /// without announcing, one client's fetch would hide the change from every
    /// other client and from the backstop.
    pub(super) fn publish(&self, ws_id: &str, repo: &RepoInfo, data: &StatusData) -> (u64, bool) {
        let hash = hash_status(data);
        let key = repo_slot(ws_id, &repo.toplevel);
        let bumped = match crate::lock(&self.hashes).insert(key, hash) {
            Some(previous) => previous != hash,
            None => false,
        };
        if bumped {
            self.bump_repo(ws_id, &repo.toplevel);
        }
        (self.epoch(ws_id), bumped)
    }

    fn watch(&self, ws_id: &str) {
        *crate::lock(&self.watchers)
            .entry(ws_id.to_string())
            .or_insert(0) += 1;
    }

    fn unwatch(&self, ws_id: &str) {
        let mut watchers = crate::lock(&self.watchers);
        if let Some(count) = watchers.get_mut(ws_id) {
            *count -= 1;
            if *count == 0 {
                watchers.remove(ws_id);
            }
        }
    }

    fn watch_repo(&self, ws_id: &str, toplevel: &Path) {
        *crate::lock(&self.repo_watchers)
            .entry((ws_id.to_string(), toplevel.to_path_buf()))
            .or_insert(0) += 1;
    }

    fn unwatch_repo(&self, ws_id: &str, toplevel: &Path) {
        let mut watchers = crate::lock(&self.repo_watchers);
        let key = (ws_id.to_string(), toplevel.to_path_buf());
        if let Some(count) = watchers.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                watchers.remove(&key);
            }
        }
    }

    /// Workspaces at least one connected client is currently looking at,
    /// each with the nested repositories some window watches.
    fn watched(&self) -> Vec<(String, Vec<PathBuf>)> {
        let workspaces: Vec<String> = crate::lock(&self.watchers).keys().cloned().collect();
        let repos = crate::lock(&self.repo_watchers);
        workspaces
            .into_iter()
            .map(|ws| {
                let tops = repos
                    .keys()
                    .filter(|(w, _)| *w == ws)
                    .map(|(_, t)| t.clone())
                    .collect();
                (ws, tops)
            })
            .collect()
    }

    /// Discover the repo for `ws_id` rooted at `root`, caching the result.
    ///
    /// Only a real repo is cached (its root is stable for the daemon's life). A
    /// non-repo OR a transient probe error is re-probed on demand — so a
    /// `git init`, a fixed permission, or an added `safe.directory` surfaces
    /// without a restart. The full [`ProbeOutcome`] is returned so the status
    /// handler can tell "not a repo" from "git couldn't read it" (dubious
    /// ownership, a wedged filesystem) and explain the latter.
    pub(super) async fn discover(&self, git: &Path, ws_id: &str, root: &Path) -> ProbeOutcome {
        if let Some(Some(cached)) = crate::lock(&self.repos).get(ws_id) {
            return ProbeOutcome::Repo(cached.clone());
        }
        let outcome = probe_repo(git, &self.procs, root).await;
        crate::lock(&self.repos).insert(ws_id.to_string(), outcome.repo().cloned());
        outcome
    }

    /// The cached primary repository of a workspace, if discovered.
    pub(super) fn primary(&self, ws_id: &str) -> Option<RepoInfo> {
        crate::lock(&self.repos).get(ws_id).cloned().flatten()
    }

    /// A repository found below a workspace's root, by top level.
    pub(super) fn found_repo(&self, ws_id: &str, toplevel: &Path) -> Option<RepoInfo> {
        crate::lock(&self.repo_sets)
            .get(ws_id)
            .and_then(|set| set.found.get(toplevel))
            .map(|f| f.info.clone())
    }

    /// Every known repository top level of a workspace (primary included).
    pub(super) fn known_toplevels(&self, ws_id: &str) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self
            .primary(ws_id)
            .map(|r| r.toplevel)
            .into_iter()
            .collect();
        if let Some(set) = crate::lock(&self.repo_sets).get(ws_id) {
            out.extend(set.found.keys().cloned());
        }
        out
    }

    /// The innermost known repository containing `path` in a workspace, and
    /// — when it is a submodule — its superproject's top level (whose
    /// status marks the submodule, so it refreshes too).
    pub(super) fn innermost(
        &self,
        ws_id: &str,
        path: &Path,
    ) -> Option<(RepoInfo, Option<PathBuf>)> {
        let primary = self.primary(ws_id);
        let sets = crate::lock(&self.repo_sets);
        let found: Vec<&super::repos::Found> = sets
            .get(ws_id)
            .map(|set| set.found.values().collect())
            .unwrap_or_default();
        let mut best: Option<(RepoInfo, bool)> = primary
            .clone()
            .filter(|p| path.starts_with(&p.toplevel))
            .map(|p| (p, false));
        for f in found {
            if path.starts_with(&f.info.toplevel)
                && best.as_ref().is_none_or(|(b, _)| {
                    f.info.toplevel.as_os_str().len() > b.toplevel.as_os_str().len()
                })
            {
                best = Some((f.info.clone(), f.kind == super::repos::RepoKind::Submodule));
            }
        }
        let (repo, submodule) = best?;
        let parent = if submodule {
            // The next repository out from the submodule.
            let outer = repo.toplevel.parent().map(Path::to_path_buf);
            let mut parent: Option<PathBuf> = primary
                .filter(|p| outer.as_ref().is_some_and(|o| o.starts_with(&p.toplevel)))
                .map(|p| p.toplevel);
            if let Some(set) = sets.get(ws_id) {
                for top in set.found.keys() {
                    if *top != repo.toplevel
                        && outer.as_ref().is_some_and(|o| o.starts_with(top))
                        && parent
                            .as_ref()
                            .is_none_or(|p| top.as_os_str().len() > p.as_os_str().len())
                    {
                        parent = Some(top.clone());
                    }
                }
            }
            parent
        } else {
            None
        };
        Some((repo, parent))
    }

    /// Add a repository found below a workspace's root. Returns whether the
    /// set changed (the caller announces it). Refuses past
    /// [`super::repos::MAX_REPOS`] (marking the set capped), the primary
    /// itself, a linked worktree of an already-known repository (a worktree
    /// is a dimension of its repository, never a peer), and anything outside
    /// the workspace root.
    pub(super) fn add_found(&self, ws_id: &str, root: &Path, found: super::repos::Found) -> bool {
        if !found.info.toplevel.starts_with(root) {
            return false;
        }
        let primary = self.primary(ws_id);
        if primary.as_ref().is_some_and(|p| {
            p.toplevel == found.info.toplevel || p.common_dir == found.info.common_dir
        }) {
            return false;
        }
        let mut sets = crate::lock(&self.repo_sets);
        let set = sets.entry(ws_id.to_string()).or_default();
        if set.found.contains_key(&found.info.toplevel)
            || set
                .found
                .values()
                .any(|f| f.info.common_dir == found.info.common_dir)
        {
            return false;
        }
        let limit = super::repos::MAX_REPOS - usize::from(primary.is_some());
        if set.found.len() >= limit {
            set.capped = true;
            return false;
        }
        set.found.insert(found.info.toplevel.clone(), found);
        true
    }

    /// The status entry for the repeat-caller pull paths (the HTTP handler
    /// and the backstop poll): single-flighted per workspace — callers that
    /// waited out a run join its result, late arrivals reuse it for
    /// [`STATUS_REUSE`] — so an epoch bump's fan-out of refetches costs one
    /// `git status`, not one per window. Event-driven invalidations flush
    /// the share (see [`GitService::invalidate`]); check the returned
    /// [`SharedStatus::flushed`] before publishing.
    pub(super) async fn status_shared(
        &self,
        git: &Path,
        ws_id: &str,
        repo: &RepoInfo,
    ) -> anyhow::Result<SharedStatus> {
        let key = repo_slot(ws_id, &repo.toplevel);
        self.status_share
            .get_or_run(&key, STATUS_REUSE, || self.status_uncached(git, repo))
            .await
    }

    /// A guaranteed-fresh status for the MCP tier: agents make decisions on
    /// this wire, and an out-of-band `git commit` in a terminal fires no
    /// invalidation — so even a ≤[`STATUS_REUSE`]-stale answer is wrong
    /// there. Serialized on the workspace's run lock (never a concurrent
    /// duplicate of an HTTP-triggered run, and its outcome is shared with
    /// queued callers), but it always runs — never reuses.
    pub(super) async fn status_fresh(
        &self,
        git: &Path,
        ws_id: &str,
        repo: &RepoInfo,
    ) -> anyhow::Result<Arc<StatusData>> {
        let slot = self.status_share.slot(&repo_slot(ws_id, &repo.toplevel));
        let _guard = slot.run_lock.lock().await;
        StatusShare::lead(&slot, || self.status_uncached(git, repo))
            .await
            .map(|shared| shared.data)
    }

    /// One raw `git status` run. WARNING: do not call this from a request
    /// path — it bypasses the per-workspace single-flight. Go through
    /// [`Self::status_shared`] (pull paths) or [`Self::status_fresh`]
    /// (freshness-critical paths) instead.
    async fn status_uncached(&self, git: &Path, repo: &RepoInfo) -> anyhow::Result<StatusData> {
        // `--no-optional-locks` is load-bearing: refreshing the index for status
        // must never contend on the index lock with a `git commit` the user or an
        // agent runs in a terminal (slow/shared FS makes that contention real).
        let out = run_git(
            git,
            &self.procs,
            &repo.toplevel,
            &[
                "--no-optional-locks",
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=all",
            ],
            MAX_STATUS_OUTPUT,
        )
        .await?;
        if !out.success {
            anyhow::bail!("git status failed: {}", out.stderr);
        }
        Ok(parse_status(&out.stdout, out.truncated))
    }

    /// Every worktree of this repo (the main checkout plus linked ones). Cheap
    /// and rarely-changing, so it is computed on demand rather than cached.
    pub(super) async fn worktrees(
        &self,
        git: &Path,
        repo: &RepoInfo,
    ) -> anyhow::Result<Vec<WorktreeInfo>> {
        let out = run_git(
            git,
            &self.procs,
            &repo.toplevel,
            &["--no-optional-locks", "worktree", "list", "--porcelain"],
            1024 * 1024,
        )
        .await?;
        if !out.success {
            anyhow::bail!("git worktree list failed: {}", out.stderr);
        }
        Ok(parse_worktrees(&out.stdout))
    }
}

/// The result of probing a directory for a git repository.
pub(super) enum ProbeOutcome {
    /// A real repository.
    Repo(RepoInfo),
    /// git ran and cleanly reported this is not a work tree — the ordinary
    /// "open a non-repo folder" case.
    NotARepo,
    /// git could not answer: it errored (dubious ownership on shared storage,
    /// a permission problem) or timed out on a wedged filesystem. Carries the
    /// reason so the UI can explain it — a real repo must never silently read
    /// as "not a git repository".
    Error(String),
}

impl ProbeOutcome {
    pub(super) fn repo(&self) -> Option<&RepoInfo> {
        match self {
            ProbeOutcome::Repo(r) => Some(r),
            _ => None,
        }
    }

    pub(super) fn into_repo(self) -> Option<RepoInfo> {
        match self {
            ProbeOutcome::Repo(r) => Some(r),
            _ => None,
        }
    }

    /// The failure reason, for the status JSON (`None` for a real repo or a
    /// genuine non-repo — both are unremarkable).
    pub(super) fn error(&self) -> Option<&str> {
        match self {
            ProbeOutcome::Error(msg) => Some(msg),
            _ => None,
        }
    }
}

/// Classify a `git rev-parse` that exited non-zero: git prints "not a git
/// repository" only for the genuine no-repo case, so anything else on stderr
/// (dubious ownership, permission denied) is a real error the user must see.
/// An empty stderr is treated as the ordinary non-repo rather than nagging.
fn classify_probe_failure(stderr: &str) -> ProbeOutcome {
    let stderr = stderr.trim();
    if stderr.is_empty() || stderr.contains("not a git repository") {
        ProbeOutcome::NotARepo
    } else {
        ProbeOutcome::Error(stderr.to_string())
    }
}

/// Run `git rev-parse` to resolve the working-tree root, this checkout's git
/// dir, and the common git dir. Public to the module: the session tracker
/// probes an agent's cwd the same way the workspace probe does.
pub(super) async fn probe_repo(git: &Path, procs: &Semaphore, root: &Path) -> ProbeOutcome {
    let out = match run_git(
        git,
        procs,
        root,
        &[
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
        8 * 1024,
    )
    .await
    {
        Ok(out) => out,
        // Spawn failure or the kill-on-timeout: not "no repo" — we couldn't
        // even ask. Surface it (e.g. "git timed out after 8s" on a wedged NFS).
        Err(err) => return ProbeOutcome::Error(err.to_string()),
    };
    if !out.success {
        return classify_probe_failure(&out.stderr);
    }
    match parse_probe(&String::from_utf8_lossy(&out.stdout), root) {
        Some(repo) => ProbeOutcome::Repo(repo),
        None => ProbeOutcome::NotARepo,
    }
}

/// Parse `rev-parse --show-toplevel --absolute-git-dir --git-common-dir`
/// run in `ran_in`. A success with no toplevel line is pathological (a bare
/// repo, or run inside a `.git` dir): treated as no work tree.
pub(super) fn parse_probe(text: &str, ran_in: &Path) -> Option<RepoInfo> {
    let mut lines = text.lines().map(str::trim);
    let toplevel = PathBuf::from(lines.next().filter(|l| !l.is_empty())?);
    let git_dir = lines
        .next()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| toplevel.join(".git"));
    // `--git-common-dir` prints relative to the CWD we ran in unless it is an
    // absolute path into the main checkout (the linked-worktree case).
    let common_dir = match lines.next().unwrap_or("") {
        "" => git_dir.clone(),
        c => {
            let p = PathBuf::from(c);
            if p.is_absolute() {
                p
            } else {
                normalize(&ran_in.join(p))
            }
        }
    };
    Some(RepoInfo {
        toplevel,
        common_dir,
        git_dir,
    })
}

/// Lexically fold `.`/`..` components (no filesystem access): a relative
/// `--git-common-dir` such as `../../.git` joined onto the probe dir must
/// compare equal to the same dir reached another way.
pub(super) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The bounded output of one git invocation.
pub(super) struct GitOutput {
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: String,
    pub(super) success: bool,
    /// stdout exceeded the cap and was truncated.
    pub(super) truncated: bool,
}

/// Spawn `git <args>` in `dir`, bounded by a concurrency permit, an output cap,
/// and a kill-on-timeout. stdout and stderr are drained concurrently so a
/// chatty git cannot deadlock by filling the stderr pipe while we read stdout.
pub(super) async fn run_git(
    git: &Path,
    procs: &Semaphore,
    dir: &Path,
    args: &[&str],
    stdout_cap: usize,
) -> anyhow::Result<GitOutput> {
    let _permit = procs
        .acquire()
        .await
        .expect("git semaphore is never closed");
    let mut child = Command::new(git)
        .current_dir(dir)
        .args(args)
        // Belt-and-suspenders with `--no-optional-locks`, and never block on a
        // credential/terminal prompt — this is a headless read. The rest of the
        // environment is inherited untouched, so git reads the user's own
        // ~/.gitconfig, credentials, and SSH setup — never a config we impose.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to spawn git at {} (is it installed?): {e}",
                git.display()
            )
        })?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    // `async move` takes ownership of `child`; on timeout the future is dropped,
    // dropping `child`, and `kill_on_drop` reaps the process.
    let fut = async move {
        let (out, err) = tokio::join!(
            read_capped(stdout, stdout_cap),
            read_capped(stderr, 64 * 1024),
        );
        let (stdout_bytes, truncated) = out?;
        let (stderr_bytes, _) = err?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((stdout_bytes, stderr_bytes, status, truncated))
    };

    match tokio::time::timeout(GIT_TIMEOUT, fut).await {
        Ok(Ok((stdout, stderr, status, truncated))) => Ok(GitOutput {
            stdout,
            stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
            success: status.success(),
            truncated,
        }),
        Ok(Err(e)) => Err(anyhow::Error::from(e).context("git io error")),
        Err(_) => anyhow::bail!("git timed out after {}s", GIT_TIMEOUT.as_secs()),
    }
}

/// Read at most `cap` bytes, reporting whether more was available (truncated).
async fn read_capped<R: AsyncRead + Unpin>(
    reader: R,
    cap: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    // Read one past the cap so we can distinguish "exactly cap" from "more".
    reader.take(cap as u64 + 1).read_to_end(&mut buf).await?;
    let truncated = buf.len() > cap;
    buf.truncate(cap);
    Ok((buf, truncated))
}

/// One `/ws/events` connection's "I am looking at workspace W" registration,
/// plus the nested repositories it watches (sections open, files mounted).
/// A guard because that socket has many exit paths (auth failure, send error,
/// client close) and a leaked watcher would poll git forever.
pub(crate) struct WatchGuard {
    state: Arc<AppState>,
    ws: Option<String>,
    repos: Vec<PathBuf>,
}

impl WatchGuard {
    pub(crate) fn new(state: Arc<AppState>) -> Self {
        WatchGuard {
            state,
            ws: None,
            repos: Vec::new(),
        }
    }

    /// The workspace this connection shows, if any.
    pub(crate) fn workspace(&self) -> Option<&str> {
        self.ws.as_deref()
    }

    /// Point this connection at `ws` (or nothing), releasing any previous one.
    pub(crate) fn set(&mut self, ws: Option<String>) {
        if self.ws == ws {
            return;
        }
        self.release_repos();
        if let Some(previous) = self.ws.take() {
            self.state.git.unwatch(&previous);
        }
        if let Some(next) = ws {
            self.state.git.watch(&next);
            self.ws = Some(next);
        }
    }

    /// The nested repositories this window watches, by top level. Only the
    /// workspace's known repositories count (never an arbitrary path), at
    /// most [`super::repos::MAX_REPOS`].
    pub(crate) fn set_repos(&mut self, tops: Vec<String>) {
        let Some(ws) = self.ws.clone() else {
            self.release_repos();
            return;
        };
        let known = self.state.git.known_toplevels(&ws);
        let mut next: Vec<PathBuf> = tops
            .into_iter()
            .map(PathBuf::from)
            .filter(|t| known.contains(t))
            .take(super::repos::MAX_REPOS)
            .collect();
        next.sort();
        next.dedup();
        if next == self.repos {
            return;
        }
        self.release_repos();
        for top in &next {
            self.state.git.watch_repo(&ws, top);
        }
        self.repos = next;
    }

    fn release_repos(&mut self) {
        if let Some(ws) = self.ws.as_deref() {
            for top in self.repos.drain(..) {
                self.state.git.unwatch_repo(ws, &top);
            }
        }
        self.repos.clear();
    }
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.release_repos();
        if let Some(ws) = self.ws.take() {
            self.state.git.unwatch(&ws);
        }
    }
}

/// Bump the epoch of every workspace whose root contains `path`, then wake the
/// events bus. Called from the file-save and agent-write paths — the moment a
/// tracked path changes, the client is nudged to refetch (zero polling).
pub(crate) async fn mark_path_dirty(state: &AppState, path: &str) {
    let expanded = expand_tilde(path);
    // Canonicalize before the prefix check. Workspace roots are stored
    // canonical (the create handler canonicalizes), so a `path` carrying `..`,
    // a relative segment, or a symlinked ancestor would fail `starts_with` and
    // a genuine in-workspace change would be silently not announced. Off the
    // reactor (blocking fs); fall back to the raw path when canonicalize fails
    // (a just-deleted file) — that preserves the prior behavior for that case.
    let target = {
        let expanded = expanded.clone();
        tokio::task::spawn_blocking(move || std::fs::canonicalize(&expanded))
            .await
            .ok()
            .and_then(Result::ok)
    }
    .unwrap_or_else(|| std::path::PathBuf::from(&expanded));
    // Tell watching windows first, whether or not a workspace holds the path
    // (a file opened from outside every workspace still refreshes). Both
    // spellings go out: a client watches the path it opened, which may be
    // the symlinked one. No receivers is not an error.
    let written = std::path::PathBuf::from(&expanded);
    let touched: crate::fs_watch::Touched = if written == target {
        Arc::from(vec![target.clone()])
    } else {
        Arc::from(vec![written, target.clone()])
    };
    let _ = state.fs_touched.send(touched);
    // Snapshot the list and drop the guard before the loop — a `std::sync`
    // guard must never be live across an `.await` (this fn is async now).
    let workspaces = crate::lock(&state.workspaces).list();
    let mut bumped = false;
    for ws in workspaces {
        // Component-wise prefix (so `/repo` never matches `/repo2`).
        if !target.starts_with(&ws.root) {
            continue;
        }
        if let Ok(rel) = target.strip_prefix(&ws.root) {
            crate::plugins::files::touched(state, &expanded, &ws.id, &rel.to_string_lossy());
        }
        // Only the repository containing the path refreshes (and a
        // submodule's superproject, whose status marks it). We just
        // announced the change, so drop the published baseline: the pull it
        // triggers adopts the new status without bumping again.
        match state.git.innermost(&ws.id, &target) {
            Some((repo, parent)) => {
                state.git.bump_repo(&ws.id, &repo.toplevel);
                state.git.invalidate_repo(&ws.id, &repo.toplevel);
                if let Some(parent) = parent {
                    state.git.bump_repo(&ws.id, &parent);
                    state.git.invalidate_repo(&ws.id, &parent);
                }
            }
            None => {
                state.git.bump(&ws.id);
                state.git.invalidate(&ws.id);
            }
        }
        bumped = true;
    }
    if bumped {
        state.changes.notify_waiters();
    }
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

/// Backstop poll: for each workspace a connected client is looking at, recompute
/// status and bump its epoch only when it actually changed. Catches out-of-band
/// edits (external editor, a `git` command in a terminal) that fire no event
/// trigger. With no window open, `watched()` is empty and this costs nothing.
pub(crate) async fn backstop_poll(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(BACKSTOP_INTERVAL).await;
        let watched = state.git.watched();
        if watched.is_empty() {
            continue;
        }
        let git = state.git.resolve_git(configured_git(&state)).await;
        if !git.adequate {
            continue;
        }
        let mut bumped = false;
        for (ws_id, nested) in watched {
            let Some(ws) = crate::lock(&state.workspaces).get(&ws_id) else {
                continue;
            };
            // The primary repository (as ever), plus the nested ones a
            // window has open — never every repository of the workspace.
            let mut repos: Vec<RepoInfo> = state
                .git
                .discover(&git.path, &ws_id, &ws.root)
                .await
                .into_repo()
                .into_iter()
                .collect();
            for top in nested {
                if let Some(found) = state.git.found_repo(&ws_id, &top) {
                    if !repos.iter().any(|r| r.toplevel == found.toplevel) {
                        repos.push(found);
                    }
                }
            }
            for repo in repos {
                let Ok(shared) = state.git.status_shared(&git.path, &ws_id, &repo).await else {
                    continue;
                };
                if shared.flushed {
                    // Invalidated mid-run: the announced change's own fan-out
                    // publishes the post-change status — don't re-seed
                    // pre-change data as the baseline.
                    continue;
                }
                let (_, changed) = state.git.publish(&ws_id, &repo, &shared.data);
                bumped |= changed;
            }
        }
        if bumped {
            state.changes.notify_waiters();
        }
    }
}

/// The explicit `git.path` override, if the user set one.
pub(super) fn configured_git(state: &AppState) -> Option<String> {
    crate::lock(&state.settings).git_path()
}

/// The directory of the git the `git.path` setting names, when it is
/// absolute and clears the version gate — for commands chimaera runs on the
/// user's behalf that call a bare `git` themselves (`claude plugin
/// marketplace add` clones with `--shallow-submodules`). Putting it first on
/// their PATH makes the setting reach them too; on a host whose stock git is
/// RHEL 7's 1.8.3 that is the difference between working and not. Only the
/// setting: a login-shell git is already on those commands' (login-shell)
/// PATH, and forcing its directory — often `/usr/bin` — first would shadow
/// the user's own python, node and the rest.
pub(crate) async fn usable_git_dir(state: &AppState) -> Option<PathBuf> {
    let configured = configured_git(state)?;
    let git = state.git.resolve_git(Some(configured)).await;
    if !git.adequate || !git.path.is_absolute() {
        return None;
    }
    git.path.parent().map(Path::to_path_buf)
}

/// Dirty-path cap on [`git_facts`]: the MCP answer is a digest, not the
/// status panel.
pub(crate) const GIT_FACTS_DIRTY_CAP: usize = 100;

/// Compact repo facts for the workspace MCP (`workspace_status` /
/// `list_changed_files`): branch, ahead/behind, and dirty paths
/// (workspace-relative, capped). `None` on ANY failure — missing/old git,
/// not a repo, a wedged status — so the MCP answer degrades to null instead
/// of erroring. Bounded by the same timeout/semaphore fences as every other
/// git call; read-only (never publishes, so no epoch side effects).
pub(crate) struct GitFacts {
    pub(crate) branch: Option<String>,
    pub(crate) ahead: i64,
    pub(crate) behind: i64,
    pub(crate) dirty: Vec<String>,
    /// More dirty paths exist than the cap allows.
    pub(crate) dirty_truncated: bool,
}

pub(crate) async fn git_facts(state: &AppState, ws_id: &str, root: &Path) -> Option<GitFacts> {
    let git = state.git.resolve_git(configured_git(state)).await;
    if !git.adequate {
        return None;
    }
    let repo = state
        .git
        .discover(&git.path, ws_id, root)
        .await
        .into_repo()?;
    // Fresh, never shared-stale: an agent that just ran `git commit` in a
    // terminal (no invalidation fires) must not be answered with the
    // pre-commit dirty list it would use to make decisions.
    let data = state.git.status_fresh(&git.path, ws_id, &repo).await.ok()?;
    let dirty: Vec<String> = data
        .rel_paths()
        .take(GIT_FACTS_DIRTY_CAP)
        .map(str::to_string)
        .collect();
    Some(GitFacts {
        branch: data.branch.clone(),
        ahead: data.ahead,
        behind: data.behind,
        dirty_truncated: data.entries.len() > GIT_FACTS_DIRTY_CAP || data.truncated,
        dirty,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real repo must never silently read as "not a git repository": only
    /// git's own "not a git repository" (and an empty stderr) is the ordinary
    /// non-repo; dubious ownership and permission failures are surfaced errors.
    #[test]
    fn classify_probe_failure_distinguishes_no_repo_from_error() {
        assert!(matches!(
            classify_probe_failure(
                "fatal: not a git repository (or any of the parent directories): .git"
            ),
            ProbeOutcome::NotARepo
        ));
        assert!(matches!(
            classify_probe_failure("  "),
            ProbeOutcome::NotARepo
        ));

        // The HPC-shared-storage case: git refuses a repo it considers unsafe.
        let dubious = "fatal: detected dubious ownership in repository at '/oak/x/repo'";
        match classify_probe_failure(dubious) {
            ProbeOutcome::Error(msg) => assert!(msg.contains("dubious ownership")),
            other => panic!("expected Error, got {:?}", other.error()),
        }

        assert!(matches!(
            classify_probe_failure("fatal: Could not read from remote repository"),
            ProbeOutcome::Error(_)
        ));
    }

    /// The baseline-ownership invariant. A pull must ANNOUNCE any change it
    /// discovers (otherwise one client's fetch hides it from every other client
    /// and from the backstop), but must not double-announce a change an event
    /// trigger already published.
    fn repo_at(top: &str) -> RepoInfo {
        RepoInfo {
            toplevel: PathBuf::from(top),
            common_dir: PathBuf::from(top).join(".git"),
            git_dir: PathBuf::from(top).join(".git"),
        }
    }

    #[test]
    fn publish_announces_each_unannounced_change_exactly_once() {
        let svc = GitService::new();
        let repo = repo_at("/w");
        let clean = StatusData::default();
        let dirty = StatusData {
            entries: vec![Entry::untracked("new.txt".to_string())],
            ..Default::default()
        };

        // First observation establishes the baseline silently.
        assert_eq!(svc.publish("w", &repo, &clean), (0, false));

        // An unannounced change (external editor / terminal git) bumps once...
        assert_eq!(svc.publish("w", &repo, &dirty), (1, true));
        // ...and re-publishing the same status does not bump again.
        assert_eq!(svc.publish("w", &repo, &dirty), (1, false));

        // An event-driven bump (a save) announces, then invalidates the baseline;
        // the pull it triggers adopts the new status WITHOUT a second bump.
        svc.bump("w");
        svc.invalidate("w");
        assert_eq!(svc.publish("w", &repo, &clean), (2, false));
        // The repository's own epoch moved with the workspace's.
        assert_eq!(svc.repo_epoch("w", &repo.toplevel), 1);
    }

    use std::sync::atomic::{AtomicU64, Ordering};

    /// The single-flight join: concurrent statuses for one workspace run the
    /// underlying git once, and every caller shares the SAME result (Arc
    /// identity — a follower must not build its own copy).
    #[tokio::test]
    async fn status_share_joins_concurrent_callers() {
        let share = StatusShare::new();
        let runs = AtomicU64::new(0);
        let reuse = Duration::from_secs(60);
        let (a, b) = tokio::join!(
            share.get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                // Yield mid-run so the second caller demonstrably queues on
                // the in-flight computation rather than racing past it.
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok(StatusData::default())
            }),
            share.get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            }),
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&a.data, &b.data));
        assert!(!a.flushed && !b.flushed);
    }

    /// THE production case: a run that outlives the reuse window. Callers
    /// that were already queued when it completes must JOIN its result — one
    /// underlying run for the whole fan-out — not each find the entry
    /// "expired on arrival" and lead their own serial run (N windows × run
    /// duration wall time, worse than no share at all). The TTL applies only
    /// to callers arriving after completion: the trailing call here re-runs.
    #[tokio::test]
    async fn status_share_waiters_join_a_run_that_outlives_reuse() {
        let share = StatusShare::new();
        let runs = AtomicU64::new(0);
        // Every completed entry is expired on arrival under a zero window.
        let reuse = Duration::ZERO;
        let (a, b, c) = tokio::join!(
            share.get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok(StatusData::default())
            }),
            share.get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            }),
            share.get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            }),
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "queued callers must join the slow run, not serialize behind it"
        );
        let (a, b, c) = (a.unwrap(), b.unwrap(), c.unwrap());
        assert!(Arc::ptr_eq(&a.data, &b.data));
        assert!(Arc::ptr_eq(&a.data, &c.data));

        // A caller arriving AFTER completion is a late arrival: the zero
        // window has expired the entry, so it leads a fresh run.
        share
            .get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            })
            .await
            .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    /// Waiters share a failed run's failure instead of serially eating their
    /// own timeout on a wedged repo; a caller arriving after the failure
    /// never reuses it and runs fresh.
    #[tokio::test]
    async fn status_share_waiters_share_a_failed_run() {
        let share = StatusShare::new();
        let runs = AtomicU64::new(0);
        let reuse = Duration::from_secs(60);
        let (a, b) = tokio::join!(
            share.get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                anyhow::bail!("git timed out after 8s")
            }),
            share.get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            }),
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the waiter ran its own status"
        );
        assert!(a.is_err());
        match b {
            Err(err) => assert!(err.to_string().contains("git timed out")),
            Ok(_) => panic!("the waiter must share the failed run's error"),
        }

        // Errors are never TTL-cached: the next arrival runs fresh.
        let c = share
            .get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            })
            .await;
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert!(c.is_ok());
    }

    /// `flush` (an event-driven invalidation) must force the next caller to
    /// recompute — a refetch triggered by a save may never reuse a pre-save
    /// result, however fresh.
    #[tokio::test]
    async fn status_share_flush_forces_recompute() {
        let share = StatusShare::new();
        let runs = AtomicU64::new(0);
        let reuse = Duration::from_secs(60);
        for expected in [1u64, 1, 2] {
            if expected == 2 {
                share.flush("w");
            }
            share
                .get_or_run("w", reuse, || async {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(StatusData::default())
                })
                .await
                .unwrap();
            // Pass 2 (fresh + un-flushed) reuses; the flush before pass 3
            // forces the recompute.
            assert_eq!(runs.load(Ordering::SeqCst), expected);
        }
        // The flush dropped the pinned payload immediately, not just marked it.
        share.flush("w");
        let slot = share.slot("w");
        assert!(crate::lock(&slot.inner).outcome.is_none());
    }

    /// A flush landing WHILE a run is in flight means that run's result may
    /// predate the announced change: it is returned to its own caller marked
    /// `flushed` (so the caller skips publish) and never installed as the
    /// shared result.
    #[tokio::test]
    async fn status_share_discards_result_flushed_mid_run() {
        let share = StatusShare::new();
        let runs = AtomicU64::new(0);
        let reuse = Duration::from_secs(60);
        let first = share
            .get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                share.flush("w"); // the change announcement, mid-run
                Ok(StatusData::default())
            })
            .await
            .unwrap();
        assert!(first.flushed, "the caller must know not to publish this");
        let second = share
            .get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            })
            .await
            .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2, "stale result was shared");
        assert!(!second.flushed);
    }

    /// The reuse window is a ceiling for LATE arrivals: with it at zero,
    /// strictly sequential callers each recompute.
    #[tokio::test]
    async fn status_share_expires_past_the_reuse_window() {
        let share = StatusShare::new();
        let runs = AtomicU64::new(0);
        for _ in 0..2 {
            share
                .get_or_run("w", Duration::ZERO, || async {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(StatusData::default())
                })
                .await
                .unwrap();
        }
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    /// The freshness-critical path (`status_fresh` / `StatusShare::lead`
    /// under the slot lock) always runs — even over a perfectly fresh cached
    /// result — and its outcome is installed for later shared callers.
    #[tokio::test]
    async fn status_fresh_semantics_always_run_then_share() {
        let share = StatusShare::new();
        let runs = AtomicU64::new(0);
        let reuse = Duration::from_secs(60);
        share
            .get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            })
            .await
            .unwrap();
        // The MCP-style fresh read: runs despite the fresh cache.
        let fresh = {
            let slot = share.slot("w");
            let _guard = slot.run_lock.lock().await;
            StatusShare::lead(&slot, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            })
            .await
            .unwrap()
        };
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        // ...and a shared caller right after reuses ITS result.
        let shared = share
            .get_or_run("w", reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            })
            .await
            .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert!(Arc::ptr_eq(&fresh.data, &shared.data));
    }

    /// The service-level wiring: `invalidate` (what `mark_path_dirty` and the
    /// worktree add/remove handlers call) flushes the share.
    #[tokio::test]
    async fn invalidate_flushes_the_status_share() {
        let svc = GitService::new();
        let runs = AtomicU64::new(0);
        let reuse = Duration::from_secs(60);
        let key = repo_slot("w", Path::new("/w"));
        for expected in [1u64, 2] {
            svc.status_share
                .get_or_run(&key, reuse, || async {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(StatusData::default())
                })
                .await
                .unwrap();
            assert_eq!(runs.load(Ordering::SeqCst), expected);
            svc.invalidate("w");
        }
    }

    /// End-to-end through the announcement path: a save inside a registered
    /// workspace (`mark_path_dirty`) bumps the epoch AND flushes the share,
    /// so the refetch it triggers recomputes instead of reusing pre-save data.
    #[tokio::test]
    async fn mark_path_dirty_flushes_the_share_end_to_end() {
        let base = std::env::temp_dir().join(format!(
            "chimaera-test-git-e2e-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        let root = base.join("ws");
        std::fs::create_dir_all(&root).unwrap();
        // Workspace roots are stored canonical (the create handler
        // canonicalizes) — match that, or the macOS /var -> /private/var
        // symlink defeats mark_path_dirty's prefix check.
        let root = std::fs::canonicalize(&root).unwrap();
        let state = crate::AppState::new(
            "t".into(),
            "h".into(),
            1,
            0,
            base.join("data"),
            base.join("config"),
        );
        let ws = crate::lock(&state.workspaces).add(root.clone()).unwrap();
        let key = repo_slot(&ws.id, &root);

        let runs = AtomicU64::new(0);
        let reuse = Duration::from_secs(60);
        state
            .git
            .status_share
            .get_or_run(&key, reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            })
            .await
            .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        // The save announcement: epoch bumped, share flushed → recompute.
        let saved = root.join("file.txt");
        std::fs::write(&saved, "x").unwrap();
        mark_path_dirty(&state, saved.to_str().unwrap()).await;
        assert_eq!(state.git.epoch(&ws.id), 1);
        state
            .git
            .status_share
            .get_or_run(&key, reuse, || async {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(StatusData::default())
            })
            .await
            .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2, "pre-save result was reused");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Deleting a workspace evicts its share slot (and the parsed status it
    /// pins) along with the discovery/epoch/hash entries.
    #[tokio::test]
    async fn forget_workspace_evicts_the_share_slot() {
        let svc = GitService::new();
        svc.status_share
            .get_or_run(
                &repo_slot("w", Path::new("/w")),
                Duration::from_secs(60),
                || async { Ok(StatusData::default()) },
            )
            .await
            .unwrap();
        svc.bump("w");
        // Another workspace whose id merely starts the same survives.
        svc.status_share
            .get_or_run(
                &repo_slot("w2", Path::new("/w2")),
                Duration::from_secs(60),
                || async { Ok(StatusData::default()) },
            )
            .await
            .unwrap();
        assert_eq!(crate::lock(&svc.status_share.slots).len(), 2);
        svc.forget_workspace("w");
        assert_eq!(crate::lock(&svc.status_share.slots).len(), 1);
        assert_eq!(svc.epoch("w"), 0);
    }

    /// Several repositories: a change inside one invalidates that one only
    /// (the others keep serving their shared status), and the innermost
    /// repository wins a path.
    #[tokio::test]
    async fn repositories_invalidate_and_resolve_independently() {
        use super::super::repos::{Found, RepoKind, Source};
        let svc = GitService::new();
        crate::lock(&svc.repos).insert("w".into(), Some(repo_at("/proj")));
        let nested = Found {
            info: repo_at("/proj/tools/cloned"),
            kind: RepoKind::Nested,
            source: Source::Probe,
        };
        assert!(svc.add_found("w", Path::new("/proj"), nested.clone()));
        assert!(
            !svc.add_found("w", Path::new("/proj"), nested),
            "no duplicates"
        );
        // A linked worktree of the primary is a dimension of it, not a peer.
        let linked = Found {
            info: RepoInfo {
                toplevel: PathBuf::from("/proj/wt"),
                common_dir: PathBuf::from("/proj/.git"),
                git_dir: PathBuf::from("/proj/.git/worktrees/wt"),
            },
            kind: RepoKind::Nested,
            source: Source::Tree,
        };
        assert!(!svc.add_found("w", Path::new("/proj"), linked));
        // Outside the root: never.
        let outside = Found {
            info: repo_at("/elsewhere"),
            kind: RepoKind::Nested,
            source: Source::Agent,
        };
        assert!(!svc.add_found("w", Path::new("/proj"), outside));

        let (inner, parent) = svc
            .innermost("w", Path::new("/proj/tools/cloned/src/x.rs"))
            .unwrap();
        assert_eq!(inner.toplevel, PathBuf::from("/proj/tools/cloned"));
        assert_eq!(parent, None, "a nested clone is not a submodule");
        let (outer, _) = svc
            .innermost("w", Path::new("/proj/tools/other.txt"))
            .unwrap();
        assert_eq!(outer.toplevel, PathBuf::from("/proj"));

        let runs = AtomicU64::new(0);
        let reuse = Duration::from_secs(60);
        for top in ["/proj", "/proj/tools/cloned"] {
            svc.status_share
                .get_or_run(&repo_slot("w", Path::new(top)), reuse, || async {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(StatusData::default())
                })
                .await
                .unwrap();
        }
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        svc.invalidate_repo("w", Path::new("/proj/tools/cloned"));
        for top in ["/proj", "/proj/tools/cloned"] {
            svc.status_share
                .get_or_run(&repo_slot("w", Path::new(top)), reuse, || async {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(StatusData::default())
                })
                .await
                .unwrap();
        }
        assert_eq!(
            runs.load(Ordering::SeqCst),
            3,
            "only the invalidated repository re-ran"
        );
    }

    /// The cap: at most 32 repositories per workspace, the primary included;
    /// past it the set says so.
    #[test]
    fn repository_sets_are_capped() {
        use super::super::repos::{Found, RepoKind, Source, MAX_REPOS};
        let svc = GitService::new();
        crate::lock(&svc.repos).insert("w".into(), Some(repo_at("/p")));
        let mut added = 0;
        for i in 0..40 {
            if svc.add_found(
                "w",
                Path::new("/p"),
                Found {
                    info: repo_at(&format!("/p/r{i}")),
                    kind: RepoKind::Nested,
                    source: Source::Probe,
                },
            ) {
                added += 1;
            }
        }
        assert_eq!(added, MAX_REPOS - 1);
        assert!(crate::lock(&svc.repo_sets)["w"].capped);
        assert_eq!(svc.known_toplevels("w").len(), MAX_REPOS);
    }

    fn now_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }
}
