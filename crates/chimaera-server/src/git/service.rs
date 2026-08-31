use std::collections::HashMap;
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

/// The read-only git service: discovery cache, per-workspace nudge epochs, and
/// the concurrency permit shared by every invocation.
pub(crate) struct GitService {
    /// workspace id -> discovered repo (`None` = not a repo). A repo root is
    /// stable so a `Some` is cached for the daemon's life; a `None` is re-probed
    /// on demand, so `git init` in an already-open workspace eventually surfaces.
    repos: Mutex<HashMap<String, Option<RepoInfo>>>,
    /// workspace id -> nudge epoch, bumped whenever that workspace's git state
    /// may have changed. Surfaced on `/ws/events` so the client refetches; the
    /// payload never rides the firehose (invalidate-and-pull).
    epochs: Mutex<HashMap<String, u64>>,
    /// workspace id -> how many connected clients are LOOKING at it (registered
    /// over `/ws/events`, released on disconnect). This gates the backstop poll.
    ///
    /// Deliberately not "was pulled recently": pulls only happen when something
    /// changed, so a recency window decays to zero on a quiet repo and the
    /// backstop would stop watching exactly when it is needed.
    watchers: Mutex<HashMap<String, usize>>,
    /// workspace id -> hash of the last computed status, so the backstop only
    /// bumps the epoch when something actually changed.
    hashes: Mutex<HashMap<String, u64>>,
    /// The resolved git binary, cached keyed by the `git.path` setting so an
    /// edit re-resolves. Resolution runs a login shell (to pick up a
    /// module-loaded git in the user's dotfiles), so it must not happen per
    /// invocation — every git call reads the cached path.
    resolved_git: Mutex<Option<(Option<String>, Arc<GitBinary>)>>,
    /// Bounds concurrent `git` processes across the whole daemon.
    pub(super) procs: Arc<Semaphore>,
    /// Per-workspace single-flight + short reuse for status runs.
    status_share: StatusShare,
}

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

    /// Invalidate `ws_id`'s share: a change was announced, so the next
    /// caller must recompute and any in-flight run must not be shared or
    /// published. Dropping the outcome also unpins its parsed payload.
    fn flush(&self, ws_id: &str) {
        let slot = crate::lock(&self.slots).get(ws_id).cloned();
        if let Some(slot) = slot {
            let mut inner = crate::lock(&slot.inner);
            inner.flushes += 1;
            inner.outcome = None;
        }
    }

    /// Drop `ws_id`'s slot entirely (the workspace is gone).
    fn evict(&self, ws_id: &str) {
        crate::lock(&self.slots).remove(ws_id);
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
            epochs: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            hashes: Mutex::new(HashMap::new()),
            resolved_git: Mutex::new(None),
            procs: Arc::new(Semaphore::new(MAX_CONCURRENT_GIT)),
            status_share: StatusShare::new(),
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

    /// Forget the published hash: the next computed status is accepted as the new
    /// baseline WITHOUT a second epoch bump. Paired with an event-driven bump
    /// (a save / an agent write), whose change we have already announced. Also
    /// flushes the single-flight share — the refetch this announcement
    /// triggers must recompute, never reuse a pre-change result.
    pub(super) fn invalidate(&self, ws_id: &str) {
        crate::lock(&self.hashes).remove(ws_id);
        self.status_share.flush(ws_id);
    }

    /// Drop everything held for a deleted workspace: the discovery cache,
    /// its epoch, the published-status hash, and — the part that actually
    /// weighs something — the status share's slot with its parsed payload.
    /// `watchers` is left alone: it is refcounted by connected clients and
    /// their guards release it (a stale entry there is a usize, and the
    /// backstop skips unknown workspace ids anyway).
    pub(crate) fn forget_workspace(&self, ws_id: &str) {
        crate::lock(&self.repos).remove(ws_id);
        crate::lock(&self.epochs).remove(ws_id);
        crate::lock(&self.hashes).remove(ws_id);
        self.status_share.evict(ws_id);
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
    pub(super) fn publish(&self, ws_id: &str, data: &StatusData) -> (u64, bool) {
        let hash = hash_status(data);
        let bumped = match crate::lock(&self.hashes).insert(ws_id.to_string(), hash) {
            Some(previous) => previous != hash,
            None => false,
        };
        if bumped {
            self.bump(ws_id);
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

    /// Workspaces at least one connected client is currently looking at.
    fn watched(&self) -> Vec<String> {
        crate::lock(&self.watchers).keys().cloned().collect()
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
        self.status_share
            .get_or_run(ws_id, STATUS_REUSE, || self.status_uncached(git, repo))
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
        let slot = self.status_share.slot(ws_id);
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
    fn repo(&self) -> Option<&RepoInfo> {
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

/// Run `git rev-parse` to resolve the working-tree root and common git dir.
async fn probe_repo(git: &Path, procs: &Semaphore, root: &Path) -> ProbeOutcome {
    let out = match run_git(
        git,
        procs,
        root,
        &["rev-parse", "--show-toplevel", "--git-common-dir"],
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
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    // A success with no toplevel line is pathological; treat as non-repo.
    let Some(toplevel) = lines.next().map(str::trim).filter(|l| !l.is_empty()) else {
        return ProbeOutcome::NotARepo;
    };
    let toplevel = PathBuf::from(toplevel);
    // `--git-common-dir` prints relative to the CWD we ran in unless it is an
    // absolute path into the main checkout (the linked-worktree case).
    let common = lines.next().map(str::trim).unwrap_or("");
    let common_dir = match common {
        "" => toplevel.join(".git"),
        c => {
            let p = PathBuf::from(c);
            if p.is_absolute() {
                p
            } else {
                root.join(p)
            }
        }
    };
    ProbeOutcome::Repo(RepoInfo {
        toplevel,
        common_dir,
    })
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

/// One `/ws/events` connection's "I am looking at workspace W" registration.
/// A guard because that socket has many exit paths (auth failure, send error,
/// client close) and a leaked watcher would poll git forever.
pub(crate) struct WatchGuard {
    state: Arc<AppState>,
    ws: Option<String>,
}

impl WatchGuard {
    pub(crate) fn new(state: Arc<AppState>) -> Self {
        WatchGuard { state, ws: None }
    }

    /// Point this connection at `ws` (or nothing), releasing any previous one.
    pub(crate) fn set(&mut self, ws: Option<String>) {
        if self.ws == ws {
            return;
        }
        if let Some(previous) = self.ws.take() {
            self.state.git.unwatch(&previous);
        }
        if let Some(next) = ws {
            self.state.git.watch(&next);
            self.ws = Some(next);
        }
    }
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
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
    // Snapshot the list and drop the guard before the loop — a `std::sync`
    // guard must never be live across an `.await` (this fn is async now).
    let workspaces = crate::lock(&state.workspaces).list();
    let mut bumped = false;
    for ws in workspaces {
        // Component-wise prefix (so `/repo` never matches `/repo2`).
        if target.starts_with(&ws.root) {
            state.git.bump(&ws.id);
            // We just announced this change; drop the published baseline so the
            // pull it triggers adopts the new status without bumping again.
            state.git.invalidate(&ws.id);
            bumped = true;
        }
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
        for ws_id in watched {
            let Some(ws) = crate::lock(&state.workspaces).get(&ws_id) else {
                continue;
            };
            let Some(repo) = state
                .git
                .discover(&git.path, &ws_id, &ws.root)
                .await
                .into_repo()
            else {
                continue;
            };
            let Ok(shared) = state.git.status_shared(&git.path, &ws_id, &repo).await else {
                continue;
            };
            if shared.flushed {
                // Invalidated mid-run: the announced change's own fan-out
                // publishes the post-change status — don't re-seed pre-change
                // data as the baseline.
                continue;
            }
            let (_, changed) = state.git.publish(&ws_id, &shared.data);
            bumped |= changed;
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
    #[test]
    fn publish_announces_each_unannounced_change_exactly_once() {
        let svc = GitService::new();
        let clean = StatusData::default();
        let dirty = StatusData {
            entries: vec![Entry::untracked("new.txt".to_string())],
            ..Default::default()
        };

        // First observation establishes the baseline silently.
        assert_eq!(svc.publish("w", &clean), (0, false));

        // An unannounced change (external editor / terminal git) bumps once...
        assert_eq!(svc.publish("w", &dirty), (1, true));
        // ...and re-publishing the same status does not bump again.
        assert_eq!(svc.publish("w", &dirty), (1, false));

        // An event-driven bump (a save) announces, then invalidates the baseline;
        // the pull it triggers adopts the new status WITHOUT a second bump.
        svc.bump("w");
        svc.invalidate("w");
        assert_eq!(svc.publish("w", &clean), (2, false));
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
        for expected in [1u64, 2] {
            svc.status_share
                .get_or_run("w", reuse, || async {
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

        let runs = AtomicU64::new(0);
        let reuse = Duration::from_secs(60);
        state
            .git
            .status_share
            .get_or_run(&ws.id, reuse, || async {
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
            .get_or_run(&ws.id, reuse, || async {
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
            .get_or_run("w", Duration::from_secs(60), || async {
                Ok(StatusData::default())
            })
            .await
            .unwrap();
        svc.bump("w");
        assert_eq!(crate::lock(&svc.status_share.slots).len(), 1);
        svc.forget_workspace("w");
        assert!(crate::lock(&svc.status_share.slots).is_empty());
        assert_eq!(svc.epoch("w"), 0);
    }

    fn now_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }
}
