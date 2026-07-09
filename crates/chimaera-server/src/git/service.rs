use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    /// (a save / an agent write), whose change we have already announced.
    fn invalidate(&self, ws_id: &str) {
        crate::lock(&self.hashes).remove(ws_id);
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

    pub(super) async fn status(&self, git: &Path, repo: &RepoInfo) -> anyhow::Result<StatusData> {
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
pub(crate) fn mark_path_dirty(state: &AppState, path: &str) {
    let expanded = expand_tilde(path);
    let target = Path::new(&expanded);
    let mut bumped = false;
    for ws in crate::lock(&state.workspaces).list() {
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
            let Ok(data) = state.git.status(&git.path, &repo).await else {
                continue;
            };
            let (_, changed) = state.git.publish(&ws_id, &data);
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
}
