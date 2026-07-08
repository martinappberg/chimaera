//! Git integration: read-only status + diff for a workspace's repo.
//!
//! Shells out to system `git` and parses porcelain v2 (DESIGN.md "Git + Slurm":
//! gitoxide's diff gaps make a library a two-backend liability; shelling out is
//! adequate for read-mostly status/log/diff/show). Every invocation is bounded
//! because the daemon shares a login node (DESIGN.md resource budget): a hard
//! timeout that KILLS the child (a wedged NFS mount must never pin a thread), an
//! output-size cap, an entry-count cap, and a daemon-wide concurrency permit.
//! Nothing here is persisted — git state is reconstructible, so it is recomputed
//! on demand and never written under `~/.chimaera`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
/// A workspace is "watched" (eligible for the backstop poll) if its status was
/// pulled within this window. Idle workspaces cost nothing.
const WATCH_TTL: Duration = Duration::from_secs(60);
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
    /// workspace id -> last time a client pulled status (backstop gate).
    watching: Mutex<HashMap<String, Instant>>,
    /// workspace id -> hash of the last computed status, so the backstop only
    /// bumps the epoch when something actually changed.
    hashes: Mutex<HashMap<String, u64>>,
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
    /// `--git-common-dir`, shared by every worktree of the repo. The grouping
    /// key for the worktree dimension (P2) and a stable repo identity.
    #[allow(dead_code)] // consumed by the worktree dimension (P2); kept in P1.
    common_dir: PathBuf,
}

impl GitService {
    pub(crate) fn new() -> Self {
        GitService {
            repos: Mutex::new(HashMap::new()),
            epochs: Mutex::new(HashMap::new()),
            watching: Mutex::new(HashMap::new()),
            hashes: Mutex::new(HashMap::new()),
            procs: Arc::new(Semaphore::new(MAX_CONCURRENT_GIT)),
        }
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

    fn mark_watching(&self, ws_id: &str) {
        crate::lock(&self.watching).insert(ws_id.to_string(), Instant::now());
    }

    /// Discover the repo for `ws_id` rooted at `root`, caching the result.
    async fn discover(&self, ws_id: &str, root: &Path) -> Option<RepoInfo> {
        if let Some(cached) = crate::lock(&self.repos).get(ws_id) {
            if cached.is_some() {
                return cached.clone();
            }
        }
        let info = probe_repo(&self.procs, root).await;
        crate::lock(&self.repos).insert(ws_id.to_string(), info.clone());
        info
    }

    async fn status(&self, repo: &RepoInfo) -> anyhow::Result<StatusData> {
        // `--no-optional-locks` is load-bearing: refreshing the index for status
        // must never contend on the index lock with a `git commit` the user or an
        // agent runs in a terminal (slow/shared FS makes that contention real).
        let out = run_git(
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
}

/// Run `git rev-parse` to resolve the working-tree root and common git dir.
async fn probe_repo(procs: &Semaphore, root: &Path) -> Option<RepoInfo> {
    let out = run_git(
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
    procs: &Semaphore,
    dir: &Path,
    args: &[&str],
    stdout_cap: usize,
) -> anyhow::Result<GitOutput> {
    let _permit = procs
        .acquire()
        .await
        .expect("git semaphore is never closed");
    let mut child = Command::new("git")
        .current_dir(dir)
        .args(args)
        // Belt-and-suspenders with `--no-optional-locks`, and never block on a
        // credential/terminal prompt — this is a headless read.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn git (is it installed?): {e}"))?;

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

/// Backstop poll: for each recently-watched workspace, recompute status and bump
/// its epoch only when the status actually changed. Catches out-of-band edits
/// (external editor, a `git` command in a terminal) that fire no event trigger.
/// Idle workspaces are skipped entirely, so this costs nothing at rest.
pub(crate) async fn backstop_poll(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(BACKSTOP_INTERVAL).await;
        let watched: Vec<String> = {
            let now = Instant::now();
            crate::lock(&state.git.watching)
                .iter()
                .filter(|(_, seen)| now.duration_since(**seen) < WATCH_TTL)
                .map(|(id, _)| id.clone())
                .collect()
        };
        let mut bumped = false;
        for ws_id in watched {
            let Some(ws) = crate::lock(&state.workspaces).get(&ws_id) else {
                continue;
            };
            let Some(repo) = state.git.discover(&ws_id, &ws.root).await else {
                continue;
            };
            let Ok(data) = state.git.status(&repo).await else {
                continue;
            };
            let hash = hash_status(&data);
            let changed = crate::lock(&state.git.hashes).get(&ws_id) != Some(&hash);
            if changed {
                crate::lock(&state.git.hashes).insert(ws_id.clone(), hash);
                state.git.bump(&ws_id);
                bumped = true;
            }
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

/// GET /api/v1/git/status?workspace_id= — the repo's status, or `{repo:false}`.
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
    state.git.mark_watching(&q.workspace_id);
    let epoch = state.git.epoch(&q.workspace_id);
    let Some(repo) = state.git.discover(&q.workspace_id, &ws.root).await else {
        return Json(json!({"repo": false, "workspace_id": q.workspace_id, "epoch": epoch}))
            .into_response();
    };
    match state.git.status(&repo).await {
        Ok(data) => {
            crate::lock(&state.git.hashes).insert(q.workspace_id.clone(), hash_status(&data));
            Json(status_json(&q.workspace_id, epoch, &repo, &data)).into_response()
        }
        Err(err) => {
            tracing::warn!(%err, workspace = %q.workspace_id, "git status failed");
            // Degrade honestly: the repo exists, status is momentarily
            // unavailable. Same shape as success (plus `error`) so the client
            // never has to special-case missing fields.
            let mut body = status_json(&q.workspace_id, epoch, &repo, &StatusData::default());
            body["error"] = json!(err.to_string());
            Json(body).into_response()
        }
    }
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
    let Some(repo) = state.git.discover(&q.workspace_id, &ws.root).await else {
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
        Some(spec) => show_blob(&state.git, &repo, &spec).await,
        None => Ok(None),
    };
    let b = match b_spec {
        Some(spec) => show_blob(&state.git, &repo, &spec).await,
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
    git: &GitService,
    repo: &RepoInfo,
    spec: &str,
) -> Result<Option<Vec<u8>>, String> {
    let out = run_git(&git.procs, &repo.toplevel, &["show", spec], MAX_DIFF_BYTES)
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
