//! Git integration: status, diff, and worktree orchestration for a workspace's
//! repo.
//!
//! Shells out to system `git` and parses porcelain v2 (DESIGN.md "Git + Slurm":
//! gitoxide's diff gaps make a library a two-backend liability; shelling out is
//! adequate for read-mostly status/log/diff/show). Every invocation is bounded
//! because the daemon shares a login node (DESIGN.md resource budget): a hard
//! timeout that KILLS the child (a wedged NFS mount must never pin a thread), an
//! output-size cap, an entry-count cap, and a daemon-wide concurrency permit.
//!
//! Inspection is read-only and stores nothing: git state is reconstructible, so
//! status and diffs are recomputed on demand. The ONLY mutations are worktree
//! create/remove, and they are confined to the managed root
//! (`AppState::worktrees_root`) — chimaera never removes a checkout it did not
//! create, never one a live session is sitting in, and never one with
//! uncommitted work unless forced.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::AppState;

/// Kill any git child that outlives this. A hung status on a wedged network
/// filesystem must never pin a daemon task.
const GIT_TIMEOUT: Duration = Duration::from_secs(8);
/// Hard ceiling on a single git invocation's stdout. `status`/`diff` on a
/// pathological tree is truncated rather than buffered unbounded.
const MAX_STATUS_OUTPUT: usize = 8 * 1024 * 1024;
/// Cap on each side of a diff; larger files bail to "open the file instead".
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
/// Cap on status entries materialized; a 50k-change tree truncates honestly
/// instead of shipping a huge list the UI cannot use anyway.
const MAX_STATUS_ENTRIES: usize = 5000;
/// Daemon-wide ceiling on concurrent git child processes (bounds CPU on a
/// shared node — an accidental fan-out of statuses cannot saturate cores).
const MAX_CONCURRENT_GIT: usize = 4;
/// Backstop cadence: catches out-of-band changes (external editor, a `git`
/// command in a terminal) that fire none of the event-driven refresh triggers.
const BACKSTOP_INTERVAL: Duration = Duration::from_secs(12);
/// The oldest git this service can drive. Every core command uses a flag or
/// subcommand younger than this: `worktree` and `rev-parse --git-common-dir`
/// (2.5), `status --porcelain=v2` (2.11), and `--no-optional-locks` (2.15).
/// Below it we degrade to an honest "your git is too old" instead of parsing
/// garbage — a real hazard on HPC login nodes still shipping RHEL 7's 1.8.3.1.
const MIN_GIT: (u32, u32) = (2, 15);

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
    procs: Arc<Semaphore>,
}

/// A discovered repository for one workspace.
#[derive(Clone)]
pub(crate) struct RepoInfo {
    /// Working-tree root of THIS workspace's checkout. `--show-toplevel` gives
    /// the right directory whether the workspace opened the main checkout or a
    /// linked worktree (Chimaera itself is developed in a linked worktree).
    toplevel: PathBuf,
    /// `--git-common-dir`, shared by every worktree of the repo. The stable
    /// repo identity: it names the managed-worktree directory (see `repo_key`).
    common_dir: PathBuf,
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
    async fn resolve_git(&self, configured: Option<String>) -> Arc<GitBinary> {
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

    fn epoch(&self, ws_id: &str) -> u64 {
        crate::lock(&self.epochs).get(ws_id).copied().unwrap_or(0)
    }

    /// Snapshot of every known workspace epoch, for the `/ws/events` git frame.
    pub(crate) fn epochs_snapshot(&self) -> HashMap<String, u64> {
        crate::lock(&self.epochs).clone()
    }

    /// Bump a workspace's epoch (does not notify — the caller batches the wake).
    fn bump(&self, ws_id: &str) {
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
    fn publish(&self, ws_id: &str, data: &StatusData) -> (u64, bool) {
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
    async fn discover(&self, git: &Path, ws_id: &str, root: &Path) -> Option<RepoInfo> {
        if let Some(cached) = crate::lock(&self.repos).get(ws_id) {
            if cached.is_some() {
                return cached.clone();
            }
        }
        let info = probe_repo(git, &self.procs, root).await;
        crate::lock(&self.repos).insert(ws_id.to_string(), info.clone());
        info
    }

    async fn status(&self, git: &Path, repo: &RepoInfo) -> anyhow::Result<StatusData> {
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
    async fn worktrees(&self, git: &Path, repo: &RepoInfo) -> anyhow::Result<Vec<WorktreeInfo>> {
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

/// One entry of `git worktree list --porcelain`.
#[derive(Default)]
struct WorktreeInfo {
    path: PathBuf,
    /// Short HEAD sha.
    head: Option<String>,
    /// Short branch name (`refs/heads/x` -> `x`); `None` when detached.
    branch: Option<String>,
    detached: bool,
    bare: bool,
    locked: bool,
    prunable: bool,
}

/// Parse `git worktree list --porcelain`: blank-line-separated records of
/// `worktree <path>` followed by `HEAD`/`branch`/`detached`/`bare`/`locked`/
/// `prunable` attributes. (Line-oriented, not `-z`: a path containing a newline
/// is pathological and would only mis-split that one record.)
fn parse_worktrees(bytes: &[u8]) -> Vec<WorktreeInfo> {
    let text = String::from_utf8_lossy(bytes);
    let mut out: Vec<WorktreeInfo> = Vec::new();
    let mut current: Option<WorktreeInfo> = None;
    for line in text.lines() {
        if line.is_empty() {
            out.extend(current.take());
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            out.extend(current.take());
            current = Some(WorktreeInfo {
                path: PathBuf::from(path),
                ..Default::default()
            });
            continue;
        }
        let Some(w) = current.as_mut() else { continue };
        if let Some(sha) = line.strip_prefix("HEAD ") {
            w.head = Some(sha.trim().chars().take(7).collect());
        } else if let Some(reference) = line.strip_prefix("branch ") {
            w.branch = Some(
                reference
                    .trim()
                    .trim_start_matches("refs/heads/")
                    .to_string(),
            );
        } else if line == "detached" {
            w.detached = true;
        } else if line == "bare" {
            w.bare = true;
        } else if line == "locked" || line.starts_with("locked ") {
            w.locked = true;
        } else if line == "prunable" || line.starts_with("prunable ") {
            w.prunable = true;
        }
    }
    out.extend(current.take());
    out
}

/// Run `git rev-parse` to resolve the working-tree root and common git dir.
async fn probe_repo(git: &Path, procs: &Semaphore, root: &Path) -> Option<RepoInfo> {
    let out = run_git(
        git,
        procs,
        root,
        &["rev-parse", "--show-toplevel", "--git-common-dir"],
        8 * 1024,
    )
    .await
    .ok()?;
    if !out.success {
        return None; // not a repo (or git absent) — caller renders "no repo"
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let toplevel = PathBuf::from(lines.next()?.trim());
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
    Some(RepoInfo {
        toplevel,
        common_dir,
    })
}

/// How the git binary was found — surfaced so the UI can explain what it is
/// pointed at ("your login shell's git" vs "the path you set").
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitSource {
    /// The explicit `git.path` setting.
    Setting,
    /// `command -v git` in the user's login shell (picks up module loads).
    LoginShell,
    /// The daemon's own PATH (`git`) — the last resort.
    Path,
}

impl GitSource {
    fn as_str(self) -> &'static str {
        match self {
            GitSource::Setting => "setting",
            GitSource::LoginShell => "login-shell",
            GitSource::Path => "path",
        }
    }
}

/// The resolved git the whole service runs on: which binary, how it was found,
/// and whether it clears [`MIN_GIT`]. Daemon-global (one git for every
/// workspace), recomputed only when the `git.path` setting changes.
struct GitBinary {
    /// The binary to spawn (absolute when resolved, bare `git` as a last resort).
    path: PathBuf,
    source: GitSource,
    /// `git --version`'s raw line, e.g. "git version 2.39.1 (Apple Git-…)".
    /// `None` when the binary could not be run at all (missing / not executable).
    raw: Option<String>,
    /// Parsed (major, minor, patch), when the raw line was understood.
    parsed: Option<(u32, u32, u32)>,
    /// The parsed version clears [`MIN_GIT`] — the service can actually run.
    adequate: bool,
}

impl GitBinary {
    /// The parsed version as "MAJOR.MINOR.PATCH", for the diagnostic UI.
    fn version_str(&self) -> Option<String> {
        self.parsed.map(|(a, b, c)| format!("{a}.{b}.{c}"))
    }

    /// The diagnostic block carried on every git-status response, so the client
    /// can explain a too-old / missing git instead of rendering a blank repo.
    fn json(&self) -> serde_json::Value {
        json!({
            "ok": self.adequate,
            "path": self.path.to_string_lossy(),
            "source": self.source.as_str(),
            "version": self.version_str(),
            "raw": self.raw,
            "min": format!("{}.{}", MIN_GIT.0, MIN_GIT.1),
        })
    }
}

/// Resolve the git binary: an explicit path wins, then the login shell's git
/// (so `module load git` in a user's dotfiles is picked up with zero config),
/// then the daemon's PATH. Whatever is chosen is version-probed so the caller
/// can gate on [`MIN_GIT`].
async fn resolve_git_binary(configured: Option<String>) -> GitBinary {
    let (path, source) = match configured {
        Some(p) => (PathBuf::from(p), GitSource::Setting),
        None => match login_shell_git().await {
            Some(p) => (p, GitSource::LoginShell),
            None => (PathBuf::from("git"), GitSource::Path),
        },
    };
    let raw = probe_git_version(&path).await;
    let parsed = raw.as_deref().and_then(parse_git_version);
    let adequate = parsed.is_some_and(|(maj, min, _)| (maj, min) >= MIN_GIT);
    GitBinary {
        path,
        source,
        raw,
        parsed,
        adequate,
    }
}

/// `command -v git` through the user's login shell — the same trick the agent
/// launcher uses, because the daemon's own PATH on an HPC login node is the
/// stock `/usr/bin/git`, while the modern one lives behind `module load git`
/// that only a login shell (sourcing the user's profile) has applied.
async fn login_shell_git() -> Option<PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let output = tokio::process::Command::new(&shell)
        .arg("-lc")
        .arg("command -v git")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(5), output)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Login shells may print banners; the path is the last non-empty line.
    let path = String::from_utf8_lossy(&out.stdout)
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())?
        .to_string();
    path.starts_with('/').then(|| PathBuf::from(path))
}

/// First non-empty line of `<bin> --version`, or `None` if it cannot run.
async fn probe_git_version(bin: &Path) -> Option<String> {
    let output = tokio::process::Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(2), output)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// Parse a `git --version` line into (major, minor, patch). Lenient about the
/// trailing junk real builds carry ("2.39.GIT", "2.39.5 (Apple Git-154)"):
/// non-digits are stripped per component and a missing component reads as 0.
fn parse_git_version(raw: &str) -> Option<(u32, u32, u32)> {
    // "git version 2.39.1 …" — the version token is the third word.
    let token = raw.split_whitespace().nth(2)?;
    let mut parts = token
        .split('.')
        .map(|p| p.trim_matches(|c: char| !c.is_ascii_digit()));
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// The bounded output of one git invocation.
struct GitOutput {
    stdout: Vec<u8>,
    stderr: String,
    success: bool,
    /// stdout exceeded the cap and was truncated.
    truncated: bool,
}

/// Spawn `git <args>` in `dir`, bounded by a concurrency permit, an output cap,
/// and a kill-on-timeout. stdout and stderr are drained concurrently so a
/// chatty git cannot deadlock by filling the stderr pipe while we read stdout.
async fn run_git(
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

// ---- porcelain v2 parsing ---------------------------------------------------

/// Parsed `git status --porcelain=v2 --branch` output.
#[derive(Default)]
struct StatusData {
    branch: Option<String>,
    detached: bool,
    head: Option<String>,
    upstream: Option<String>,
    ahead: i64,
    behind: i64,
    entries: Vec<Entry>,
    truncated: bool,
}

/// One changed path.
#[derive(Default, Clone)]
struct Entry {
    rel: String,
    orig_rel: Option<String>,
    /// Index (staged) status code; `?` for untracked.
    x: char,
    /// Worktree (unstaged) status code; `?` for untracked.
    y: char,
    staged: bool,
    unstaged: bool,
    untracked: bool,
    conflicted: bool,
}

impl Entry {
    fn changed(rel: String, orig_rel: Option<String>, x: char, y: char) -> Self {
        Entry {
            rel,
            orig_rel,
            x,
            y,
            staged: x != '.',
            unstaged: y != '.',
            untracked: false,
            conflicted: false,
        }
    }
    fn untracked(rel: String) -> Self {
        Entry {
            rel,
            orig_rel: None,
            x: '?',
            y: '?',
            staged: false,
            unstaged: true,
            untracked: true,
            conflicted: false,
        }
    }
}

/// Parse the NUL-separated porcelain v2 stream. `-z` makes every record and
/// header NUL-terminated; a rename record's original path is a SEPARATE
/// following NUL field (the `\t` of the non-`-z` form).
fn parse_status(bytes: &[u8], output_truncated: bool) -> StatusData {
    let text = String::from_utf8_lossy(bytes);
    let tokens: Vec<&str> = text.split('\0').collect();
    let mut data = StatusData {
        truncated: output_truncated,
        ..Default::default()
    };
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        i += 1;
        if tok.is_empty() {
            continue;
        }
        match tok.as_bytes()[0] {
            b'#' => parse_header(tok, &mut data),
            b'1' => {
                if let Some(e) = parse_changed(tok, 6, None) {
                    data.entries.push(e);
                }
            }
            b'2' => {
                // Rename/copy: the original path is the next NUL field.
                let orig = tokens
                    .get(i)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                if tokens.get(i).is_some() {
                    i += 1;
                }
                if let Some(e) = parse_changed(tok, 7, orig) {
                    data.entries.push(e);
                }
            }
            b'u' => {
                if let Some(mut e) = parse_changed(tok, 8, None) {
                    // Unmerged: always a conflict needing resolution, regardless
                    // of the individual stage codes.
                    e.conflicted = true;
                    e.staged = false;
                    e.unstaged = true;
                    data.entries.push(e);
                }
            }
            b'?' => {
                if let Some(path) = tok.get(2..).filter(|p| !p.is_empty()) {
                    data.entries.push(Entry::untracked(path.to_string()));
                }
            }
            // `!` (ignored) never appears (`--ignored=no`); anything else is
            // skipped rather than guessed.
            _ => {}
        }
        if data.entries.len() >= MAX_STATUS_ENTRIES {
            data.truncated = true;
            break;
        }
    }
    data
}

/// Parse a `1`/`2`/`u` record. These share the shape `<T> <XY> <fields…> <path>`
/// where `skip_fields` counts the space-separated fields between `XY` and the
/// path (which is last and may itself contain spaces).
fn parse_changed(tok: &str, skip_fields: usize, orig_rel: Option<String>) -> Option<Entry> {
    let body = tok.get(2..)?; // drop the "<T> " prefix
    let mut it = body.splitn(skip_fields + 2, ' ');
    let xy = it.next()?;
    for _ in 0..skip_fields {
        it.next()?;
    }
    let path = it.next()?.to_string();
    let mut chars = xy.chars();
    let x = chars.next()?;
    let y = chars.next()?;
    Some(Entry::changed(path, orig_rel, x, y))
}

fn parse_header(tok: &str, data: &mut StatusData) {
    let rest = tok.trim_start_matches('#').trim();
    if let Some(v) = rest.strip_prefix("branch.head ") {
        if v == "(detached)" {
            data.detached = true;
        } else {
            data.branch = Some(v.to_string());
        }
    } else if let Some(v) = rest.strip_prefix("branch.upstream ") {
        data.upstream = Some(v.to_string());
    } else if let Some(v) = rest.strip_prefix("branch.oid ") {
        // "(initial)" marks an unborn branch (no commits yet).
        data.head = if v.trim() == "(initial)" {
            None
        } else {
            Some(v.trim().chars().take(7).collect())
        };
    } else if let Some(v) = rest.strip_prefix("branch.ab ") {
        for part in v.split_whitespace() {
            if let Some(n) = part.strip_prefix('+') {
                data.ahead = n.parse().unwrap_or(0);
            } else if let Some(n) = part.strip_prefix('-') {
                data.behind = n.parse().unwrap_or(0);
            }
        }
    }
}

/// A stable hash of the status, so the backstop poll only bumps the epoch on a
/// real change (not on every re-run).
fn hash_status(d: &StatusData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    d.branch.hash(&mut h);
    d.head.hash(&mut h);
    d.ahead.hash(&mut h);
    d.behind.hash(&mut h);
    d.upstream.hash(&mut h);
    for e in &d.entries {
        e.rel.hash(&mut h);
        e.orig_rel.hash(&mut h);
        e.x.hash(&mut h);
        e.y.hash(&mut h);
        e.untracked.hash(&mut h);
        e.conflicted.hash(&mut h);
    }
    h.finish()
}

fn status_json(ws_id: &str, epoch: u64, repo: &RepoInfo, d: &StatusData) -> serde_json::Value {
    let (mut staged, mut unstaged, mut untracked, mut conflicted) = (0u32, 0u32, 0u32, 0u32);
    let entries: Vec<serde_json::Value> = d
        .entries
        .iter()
        .map(|e| {
            if e.conflicted {
                conflicted += 1;
            } else if e.untracked {
                untracked += 1;
            } else {
                if e.staged {
                    staged += 1;
                }
                if e.unstaged {
                    unstaged += 1;
                }
            }
            json!({
                "path": repo.toplevel.join(&e.rel).to_string_lossy(),
                "rel": e.rel,
                "orig": e.orig_rel.as_ref().map(|o| repo.toplevel.join(o).to_string_lossy().into_owned()),
                "orig_rel": e.orig_rel,
                "x": e.x.to_string(),
                "y": e.y.to_string(),
                "staged": e.staged,
                "unstaged": e.unstaged,
                "untracked": e.untracked,
                "conflicted": e.conflicted,
            })
        })
        .collect();
    json!({
        "repo": true,
        "workspace_id": ws_id,
        "epoch": epoch,
        "branch": d.branch,
        "detached": d.detached,
        "head": d.head,
        "upstream": d.upstream,
        "ahead": d.ahead,
        "behind": d.behind,
        "entries": entries,
        "counts": {
            "staged": staged,
            "unstaged": unstaged,
            "untracked": untracked,
            "conflicted": conflicted,
            "total": d.entries.len(),
        },
        "truncated": d.truncated,
    })
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

// ---- refresh triggers -------------------------------------------------------

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
            let Some(repo) = state.git.discover(&git.path, &ws_id, &ws.root).await else {
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

// ---- HTTP handlers ----------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct StatusQuery {
    workspace_id: String,
}

/// The explicit `git.path` override, if the user set one.
fn configured_git(state: &AppState) -> Option<String> {
    crate::lock(&state.settings).git_path()
}

/// GET /api/v1/git/status?workspace_id= — the repo's status, or `{repo:false}`.
/// Every response carries a `git` diagnostic block and a `git_ok` flag; when
/// `git_ok` is false the resolved git is missing or too old (see [`MIN_GIT`])
/// and the client shows how to point chimaera at a modern git.
pub(crate) async fn status(
    State(state): State<Arc<AppState>>,
    Query(q): Query<StatusQuery>,
) -> Response {
    let Some(ws) = crate::lock(&state.workspaces).get(&q.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {}", q.workspace_id)})),
        )
            .into_response();
    };
    let git = state.git.resolve_git(configured_git(&state)).await;
    if !git.adequate {
        // The git binary itself can't drive the service — report that
        // distinctly from "not a repo" so the panel explains WHY and offers
        // the fix, instead of parsing an ancient git's output into "(unborn)".
        let epoch = state.git.epoch(&q.workspace_id);
        return Json(json!({
            "repo": false,
            "git_ok": false,
            "git": git.json(),
            "workspace_id": q.workspace_id,
            "epoch": epoch,
        }))
        .into_response();
    }
    let Some(repo) = state
        .git
        .discover(&git.path, &q.workspace_id, &ws.root)
        .await
    else {
        let epoch = state.git.epoch(&q.workspace_id);
        return Json(json!({
            "repo": false,
            "git_ok": true,
            "git": git.json(),
            "workspace_id": q.workspace_id,
            "epoch": epoch,
        }))
        .into_response();
    };
    match state.git.status(&git.path, &repo).await {
        Ok(data) => {
            // Publishing may discover an unannounced change (an external editor,
            // a terminal `git` command) and bump the epoch; read the epoch after,
            // so THIS response is already current and the caller won't refetch.
            let (epoch, bumped) = state.git.publish(&q.workspace_id, &data);
            if bumped {
                state.changes.notify_waiters();
            }
            let mut body = status_json(&q.workspace_id, epoch, &repo, &data);
            body["git_ok"] = json!(true);
            body["git"] = git.json();
            Json(body).into_response()
        }
        Err(err) => {
            tracing::warn!(%err, workspace = %q.workspace_id, "git status failed");
            // Degrade honestly: the repo exists, status is momentarily
            // unavailable. Same shape as success (plus `error`) so the client
            // never has to special-case missing fields.
            let epoch = state.git.epoch(&q.workspace_id);
            let mut body = status_json(&q.workspace_id, epoch, &repo, &StatusData::default());
            body["error"] = json!(err.to_string());
            body["git_ok"] = json!(true);
            body["git"] = git.json();
            Json(body).into_response()
        }
    }
}

/// GET /api/v1/git/worktrees?workspace_id= — every worktree of this repo, with
/// the branch each is on. The client maps its sessions into them by cwd, so
/// "which agent is on which branch" is derived, never stored.
pub(crate) async fn worktrees(
    State(state): State<Arc<AppState>>,
    Query(q): Query<StatusQuery>,
) -> Response {
    let Some(ws) = crate::lock(&state.workspaces).get(&q.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown workspace"})),
        )
            .into_response();
    };
    let git = state.git.resolve_git(configured_git(&state)).await;
    if !git.adequate {
        // Too old to list worktrees; the status endpoint carries the diagnostic.
        return Json(json!({"repo": false, "worktrees": []})).into_response();
    }
    let Some(repo) = state
        .git
        .discover(&git.path, &q.workspace_id, &ws.root)
        .await
    else {
        return Json(json!({"repo": false, "worktrees": []})).into_response();
    };
    match state.git.worktrees(&git.path, &repo).await {
        Ok(list) => {
            let managed_root = std::fs::canonicalize(&state.worktrees_root)
                .unwrap_or_else(|_| state.worktrees_root.clone());
            let items: Vec<serde_json::Value> = list
                .iter()
                .map(|w| {
                    json!({
                        "path": w.path.to_string_lossy(),
                        "branch": w.branch,
                        "head": w.head,
                        "detached": w.detached,
                        "bare": w.bare,
                        "locked": w.locked,
                        "prunable": w.prunable,
                        // The worktree this workspace actually has checked out.
                        "current": w.path == repo.toplevel,
                        // Created by chimaera under the managed root: the ONLY
                        // worktrees it will remove, so the UI shows the control
                        // exactly where the daemon would allow it.
                        "managed": w.path.starts_with(&managed_root),
                    })
                })
                .collect();
            Json(json!({"repo": true, "worktrees": items})).into_response()
        }
        Err(err) => {
            tracing::warn!(%err, workspace = %q.workspace_id, "git worktree list failed");
            Json(json!({"repo": true, "worktrees": [], "error": err.to_string()})).into_response()
        }
    }
}

// ---- worktree orchestration (the feature's ONLY mutations) ------------------

/// A stable directory name for a repo, shared by all of its worktrees:
/// `<repo-dir-name>-<hash of the common git dir>`. The hash disambiguates two
/// checkouts that happen to share a basename; the name keeps it human.
fn repo_key(repo: &RepoInfo) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    repo.common_dir.hash(&mut h);
    let name = repo
        .common_dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{safe}-{:08x}", h.finish() as u32)
}

/// Reject anything git would not accept as a branch name, and anything that
/// could be read as a flag. `check-ref-format --branch` already rules out `..`,
/// control characters, and trailing junk — the leading-`-` guard keeps the name
/// from being parsed as an option before git ever sees it.
async fn valid_branch(git: &Path, procs: &Semaphore, dir: &Path, branch: &str) -> bool {
    if branch.is_empty() || branch.len() > 200 || branch.starts_with('-') {
        return false;
    }
    match run_git(
        git,
        procs,
        dir,
        &["check-ref-format", "--branch", branch],
        4096,
    )
    .await
    {
        Ok(out) => out.success,
        Err(_) => false,
    }
}

/// Does `refs/heads/<branch>` already exist?
async fn branch_exists(git: &Path, procs: &Semaphore, dir: &Path, branch: &str) -> bool {
    let refname = format!("refs/heads/{branch}");
    match run_git(
        git,
        procs,
        dir,
        &["rev-parse", "--verify", "--quiet", &refname],
        4096,
    )
    .await
    {
        Ok(out) => out.success,
        Err(_) => false,
    }
}

fn conflict(message: &str) -> Response {
    (StatusCode::CONFLICT, Json(json!({"error": message}))).into_response()
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response()
}

/// The mutation handlers' response when the resolved git can't run the service.
fn git_too_old(git: &GitBinary) -> Response {
    let msg = match git.version_str() {
        Some(v) => format!(
            "git {v} is too old for this — chimaera needs git ≥ {}.{}. \
             Point it at a newer git in Settings (git.path).",
            MIN_GIT.0, MIN_GIT.1
        ),
        None => format!(
            "no runnable git at {} — set git.path to a git ≥ {}.{} in Settings.",
            git.path.display(),
            MIN_GIT.0,
            MIN_GIT.1
        ),
    };
    bad_request(&msg)
}

#[derive(Deserialize)]
pub(crate) struct CreateWorktree {
    workspace_id: String,
    /// Branch to check out. Created off `base` (or HEAD) when it does not exist.
    branch: String,
    /// Start point for a NEW branch; HEAD when omitted.
    #[serde(default)]
    base: Option<String>,
}

/// POST /api/v1/git/worktrees — create a worktree for `branch` under the managed
/// root and register it as a workspace, so the new branch is immediately a
/// window you can open (its own tree, status and diffs). Additive: it never
/// touches an existing checkout.
pub(crate) async fn create_worktree(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateWorktree>,
) -> Response {
    let Some(ws) = crate::lock(&state.workspaces).get(&body.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown workspace"})),
        )
            .into_response();
    };
    let git = state.git.resolve_git(configured_git(&state)).await;
    if !git.adequate {
        return git_too_old(&git);
    }
    let Some(repo) = state
        .git
        .discover(&git.path, &body.workspace_id, &ws.root)
        .await
    else {
        return bad_request("not a git repository");
    };
    let branch = body.branch.trim().to_string();
    if !valid_branch(&git.path, &state.git.procs, &repo.toplevel, &branch).await {
        return bad_request("invalid branch name");
    }
    if let Some(base) = body.base.as_deref() {
        if base.starts_with('-') || base.is_empty() {
            return bad_request("invalid base revision");
        }
    }

    // Managed location only. `branch` passed check-ref-format, so it carries no
    // `..` component; assert containment anyway — a path escape here would let a
    // later `remove` delete outside the managed root.
    let path = state.worktrees_root.join(repo_key(&repo)).join(&branch);
    if !path.starts_with(&state.worktrees_root) {
        return bad_request("branch name escapes the managed worktree root");
    }
    if path.exists()
        && std::fs::read_dir(&path)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true)
    {
        return conflict("a worktree for that branch already exists");
    }
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("failed to create {}: {err}", parent.display())})),
            )
                .into_response();
        }
    }

    let path_str = path.to_string_lossy().into_owned();
    // An existing branch is checked out as-is; a new one is created off `base`
    // (or HEAD). git itself refuses if the branch is checked out elsewhere.
    let exists = branch_exists(&git.path, &state.git.procs, &repo.toplevel, &branch).await;
    let mut args: Vec<&str> = vec!["worktree", "add"];
    if exists {
        args.push(&path_str);
        args.push(&branch);
    } else {
        args.push("-b");
        args.push(&branch);
        args.push(&path_str);
        if let Some(base) = body.base.as_deref() {
            args.push(base);
        }
    }
    let out = match run_git(
        &git.path,
        &state.git.procs,
        &repo.toplevel,
        &args,
        64 * 1024,
    )
    .await
    {
        Ok(out) => out,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };
    if !out.success {
        // git's own message is the most useful thing we can say here (branch
        // already checked out in another worktree, bad base, …).
        return conflict(&out.stderr);
    }

    // The new worktree is a folder: register it so it can be opened as a window.
    let canonical = std::fs::canonicalize(&path).unwrap_or(path);
    let workspace = match crate::lock(&state.workspaces).add(canonical.clone()) {
        Ok(workspace) => workspace,
        Err(err) => {
            tracing::warn!(%err, "worktree created but workspace registration failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };

    // The repo's worktree list changed: every window watching it refetches.
    state.git.bump(&body.workspace_id);
    state.changes.notify_waiters();

    Json(json!({
        "worktree": {"path": canonical.to_string_lossy(), "branch": branch},
        "workspace": {"id": workspace.id, "root": workspace.root, "name": workspace.name},
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct RemoveWorktree {
    workspace_id: String,
    /// Absolute path of the worktree to remove.
    path: String,
    /// Remove even with uncommitted changes.
    #[serde(default)]
    force: bool,
}

/// DELETE /api/v1/git/worktrees — remove a MANAGED worktree. Destructive, so it
/// is fenced four ways: it must live under the managed root (Chimaera never
/// deletes a checkout it did not create), it must not be the workspace you are
/// looking at, no live session may be sitting inside it, and it must be clean
/// unless `force`. The branch itself is left alone.
pub(crate) async fn remove_worktree(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RemoveWorktree>,
) -> Response {
    let Some(ws) = crate::lock(&state.workspaces).get(&body.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown workspace"})),
        )
            .into_response();
    };
    let git = state.git.resolve_git(configured_git(&state)).await;
    if !git.adequate {
        return git_too_old(&git);
    }
    let Some(repo) = state
        .git
        .discover(&git.path, &body.workspace_id, &ws.root)
        .await
    else {
        return bad_request("not a git repository");
    };
    let Ok(target) = std::fs::canonicalize(&body.path) else {
        return bad_request("no such worktree");
    };

    // Fence 1: only what we created.
    let managed = std::fs::canonicalize(&state.worktrees_root)
        .unwrap_or_else(|_| state.worktrees_root.clone());
    if !target.starts_with(&managed) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "chimaera only removes worktrees it created"})),
        )
            .into_response();
    }
    // Fence 2: never pull the floor out from the window asking.
    if target == repo.toplevel {
        return conflict("cannot remove the worktree this workspace is open on");
    }
    // Fence 3: a live session inside would lose its shell.
    let inside: Vec<String> = {
        let cwds = crate::lock(&state.current_cwds);
        state
            .sessions
            .list()
            .into_iter()
            .filter(|info| info.alive)
            .filter(|info| {
                let cwd = cwds
                    .get(&info.id)
                    .cloned()
                    .unwrap_or_else(|| info.cwd.clone());
                cwd.starts_with(&target)
            })
            .map(|info| info.name)
            .collect()
    };
    if !inside.is_empty() {
        return conflict(&format!(
            "{} live session(s) are inside that worktree: {}",
            inside.len(),
            inside.join(", ")
        ));
    }
    // Fence 4: uncommitted work is not ours to throw away.
    if !body.force {
        match run_git(
            &git.path,
            &state.git.procs,
            &target,
            &["--no-optional-locks", "status", "--porcelain"],
            MAX_STATUS_OUTPUT,
        )
        .await
        {
            Ok(out) if out.success && !out.stdout.is_empty() => {
                return conflict("worktree has uncommitted changes");
            }
            Ok(_) => {}
            Err(err) => return conflict(&err.to_string()),
        }
    }

    let target_str = target.to_string_lossy().into_owned();
    let mut args: Vec<&str> = vec!["worktree", "remove"];
    if body.force {
        args.push("--force");
    }
    args.push(&target_str);
    match run_git(
        &git.path,
        &state.git.procs,
        &repo.toplevel,
        &args,
        64 * 1024,
    )
    .await
    {
        Ok(out) if out.success => {}
        Ok(out) => return conflict(&out.stderr),
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    }

    // Drop the workspace registration that pointed at it (never the directory —
    // git already removed that).
    let stale: Vec<String> = crate::lock(&state.workspaces)
        .list()
        .into_iter()
        .filter(|w| w.root == target)
        .map(|w| w.id)
        .collect();
    for id in stale {
        if let Err(err) = crate::lock(&state.workspaces).remove(&id) {
            tracing::warn!(%err, %id, "failed to unregister removed worktree");
        }
    }

    state.git.bump(&body.workspace_id);
    state.changes.notify_waiters();
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(crate) struct DiffQuery {
    workspace_id: String,
    path: String,
    /// `unstaged` (default), `staged`, or `head`.
    #[serde(default)]
    mode: Option<String>,
}

/// GET /api/v1/git/diff?workspace_id=&path=&mode= — the two blob versions for a
/// side-by-side view. Returns full before/after text (the client's MergeView
/// computes the diff); binary and over-cap files bail with a flag.
pub(crate) async fn diff(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DiffQuery>,
) -> Response {
    let Some(ws) = crate::lock(&state.workspaces).get(&q.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown workspace"})),
        )
            .into_response();
    };
    let git = state.git.resolve_git(configured_git(&state)).await;
    if !git.adequate {
        return git_too_old(&git);
    }
    let Some(repo) = state
        .git
        .discover(&git.path, &q.workspace_id, &ws.root)
        .await
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "not a git repository"})),
        )
            .into_response();
    };
    let Some(rel) = repo_relative(&repo.toplevel, &q.path) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "path is not inside the repository"})),
        )
            .into_response();
    };

    let mode = q.mode.as_deref().unwrap_or("unstaged");
    let (a_spec, a_label, b_from_worktree, b_label) = match mode {
        "staged" => (Some(format!("HEAD:{rel}")), "HEAD", false, "staged"),
        "head" => (Some(format!("HEAD:{rel}")), "HEAD", true, "working tree"),
        // "unstaged" (default): index vs working tree.
        _ => (Some(format!(":{rel}")), "index", true, "working tree"),
    };
    let b_spec = if b_from_worktree {
        None
    } else {
        Some(format!(":{rel}"))
    };

    // Fetch both sides (a = base, b = target). A missing object is a valid
    // outcome: no HEAD blob = added; no worktree file = deleted.
    let a = match a_spec {
        Some(spec) => show_blob(&git.path, &state.git, &repo, &spec).await,
        None => Ok(None),
    };
    let b = match b_spec {
        Some(spec) => show_blob(&git.path, &state.git, &repo, &spec).await,
        None => read_worktree(&repo.toplevel.join(&rel)).await,
    };
    let (a, b) = match (a, b) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            return Json(json!({"error": e, "too_large": e == "too_large"})).into_response()
        }
    };

    // Either side binary or oversized → the UI offers "open the file".
    if a.as_deref().map(is_binary).unwrap_or(false) || b.as_deref().map(is_binary).unwrap_or(false)
    {
        return Json(json!({"path": q.path, "rel": rel, "mode": mode, "binary": true}))
            .into_response();
    }
    let to_text = |bytes: Option<Vec<u8>>| bytes.map(|b| String::from_utf8_lossy(&b).into_owned());
    let a_text = to_text(a);
    let b_text = to_text(b);
    Json(json!({
        "path": q.path,
        "rel": rel,
        "mode": mode,
        "binary": false,
        "too_large": false,
        "added": a_text.is_none(),
        "deleted": b_text.is_none(),
        "a": a_text.unwrap_or_default(),
        "b": b_text.unwrap_or_default(),
        "a_label": a_label,
        "b_label": b_label,
    }))
    .into_response()
}

/// `git show <spec>` → the blob bytes, `None` if the object does not exist, or
/// `Err("too_large")` past the cap.
async fn show_blob(
    git_bin: &Path,
    git: &GitService,
    repo: &RepoInfo,
    spec: &str,
) -> Result<Option<Vec<u8>>, String> {
    let out = run_git(
        git_bin,
        &git.procs,
        &repo.toplevel,
        &["show", spec],
        MAX_DIFF_BYTES,
    )
    .await
    .map_err(|e| e.to_string())?;
    if out.truncated {
        return Err("too_large".into());
    }
    // A non-zero exit means the path does not exist at that rev (added/deleted).
    Ok(out.success.then_some(out.stdout))
}

/// Read a working-tree file, `None` if absent, `Err("too_large")` past the cap.
async fn read_worktree(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.len() as usize > MAX_DIFF_BYTES => Err("too_large".into()),
        Ok(_) => match tokio::fs::read(path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) => Err(e.to_string()),
        },
        Err(_) => Ok(None), // deleted / never existed
    }
}

/// git's own heuristic: a NUL byte in the first 8000 bytes means binary.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

/// Repo-relative path for `abs`, or `None` if it escapes the repo.
///
/// `git rev-parse --show-toplevel` returns a symlink-RESOLVED path, so a client
/// path carrying an unresolved prefix (macOS `/tmp` -> `/private/tmp`) would
/// never match it lexically. Resolve the input the same way before comparing;
/// a deleted file has no canonical form, so fall back to resolving its parent
/// and re-attaching the file name, and finally to the raw path.
fn repo_relative(toplevel: &Path, abs: &str) -> Option<String> {
    let raw = Path::new(abs);
    let resolved = std::fs::canonicalize(raw).ok().or_else(|| {
        let parent = raw.parent()?;
        let name = raw.file_name()?;
        Some(std::fs::canonicalize(parent).ok()?.join(name))
    });
    let candidate = resolved.as_deref().unwrap_or(raw);
    let rel = candidate.strip_prefix(toplevel).ok()?;
    if rel
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(rel.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_versions_and_gates_on_min() {
        // The version token is the third word; trailing build junk is tolerated.
        assert_eq!(parse_git_version("git version 2.39.1"), Some((2, 39, 1)));
        assert_eq!(
            parse_git_version("git version 2.39.5 (Apple Git-154)"),
            Some((2, 39, 5))
        );
        assert_eq!(parse_git_version("git version 2.20.GIT"), Some((2, 20, 0)));
        assert_eq!(parse_git_version("git version 1.8.3.1"), Some((1, 8, 3)));
        assert_eq!(parse_git_version("not a version line"), None);

        // The gate that decides the "too old" panel: RHEL 7's 1.8.3.1 fails,
        // the 2.15 floor and anything above it passes.
        let adequate =
            |raw: &str| parse_git_version(raw).is_some_and(|(maj, min, _)| (maj, min) >= MIN_GIT);
        assert!(!adequate("git version 1.8.3.1"));
        assert!(!adequate("git version 2.14.9"));
        assert!(adequate("git version 2.15.0"));
        assert!(adequate("git version 2.39.1"));
    }

    #[test]
    fn parses_branch_header_and_mixed_entries() {
        // A realistic `--porcelain=v2 --branch -z` stream (NUL-separated).
        let stream = concat!(
            "# branch.oid 1234567890abcdef\0",
            "# branch.head main\0",
            "# branch.upstream origin/main\0",
            "# branch.ab +2 -1\0",
            "1 .M N... 100644 100644 100644 aaa bbb src/changed.rs\0",
            "1 M. N... 100644 100644 100644 ccc ddd src/staged.rs\0",
            "2 R. N... 100644 100644 100644 eee fff R100 new/name.rs\0old/name.rs\0",
            "u UU N... 100644 100644 100644 100644 g h i src/conflict.rs\0",
            "? untracked.txt\0",
        );
        let data = parse_status(stream.as_bytes(), false);
        assert_eq!(data.branch.as_deref(), Some("main"));
        assert_eq!(data.head.as_deref(), Some("1234567"));
        assert_eq!(data.upstream.as_deref(), Some("origin/main"));
        assert_eq!(data.ahead, 2);
        assert_eq!(data.behind, 1);
        assert_eq!(data.entries.len(), 5);

        let changed = &data.entries[0];
        assert_eq!(changed.rel, "src/changed.rs");
        assert!(changed.unstaged && !changed.staged);

        let staged = &data.entries[1];
        assert!(staged.staged && !staged.unstaged);

        let rename = &data.entries[2];
        assert_eq!(rename.rel, "new/name.rs");
        assert_eq!(rename.orig_rel.as_deref(), Some("old/name.rs"));

        let conflict = &data.entries[3];
        assert!(conflict.conflicted);

        let untracked = &data.entries[4];
        assert_eq!(untracked.rel, "untracked.txt");
        assert!(untracked.untracked);
    }

    #[test]
    fn detached_head_and_unborn_branch() {
        let detached = parse_status(b"# branch.head (detached)\0# branch.oid abcdef123\0", false);
        assert!(detached.detached);
        assert_eq!(detached.branch, None);
        assert_eq!(detached.head.as_deref(), Some("abcdef1"));

        let unborn = parse_status(b"# branch.oid (initial)\0# branch.head main\0", false);
        assert_eq!(unborn.head, None);
        assert_eq!(unborn.branch.as_deref(), Some("main"));
    }

    #[test]
    fn paths_with_spaces_survive() {
        let data = parse_status(b"1 .M N... 100644 100644 100644 a b src/a file.rs\0", false);
        assert_eq!(data.entries[0].rel, "src/a file.rs");
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

    #[test]
    fn parses_worktree_list() {
        let out = concat!(
            "worktree /repo\n",
            "HEAD 1234567890abcdef\n",
            "branch refs/heads/main\n",
            "\n",
            "worktree /repo/.claude/worktrees/feat\n",
            "HEAD abcdef1234567890\n",
            "branch refs/heads/claude/feat\n",
            "\n",
            "worktree /repo/detached\n",
            "HEAD 0badc0de0badc0de\n",
            "detached\n",
            "locked being rebased\n",
            "\n",
        );
        let list = parse_worktrees(out.as_bytes());
        assert_eq!(list.len(), 3);

        assert_eq!(list[0].path, PathBuf::from("/repo"));
        assert_eq!(list[0].branch.as_deref(), Some("main"));
        assert_eq!(list[0].head.as_deref(), Some("1234567"));
        assert!(!list[0].detached);

        // refs/heads/ is stripped, but a slash INSIDE the branch name survives.
        assert_eq!(list[1].branch.as_deref(), Some("claude/feat"));

        assert!(list[2].detached);
        assert_eq!(list[2].branch, None);
        assert!(list[2].locked);
    }

    #[test]
    fn repo_relative_rejects_escapes() {
        let top = Path::new("/repo");
        assert_eq!(
            repo_relative(top, "/repo/src/x.rs").as_deref(),
            Some("src/x.rs")
        );
        assert_eq!(repo_relative(top, "/other/x.rs"), None);
    }
}
