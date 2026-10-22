use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Semaphore;

use crate::AppState;

use super::http::{bad_request, conflict, git_too_old};
use super::include::{copy_included, IncludeReport};
use super::parse::RepoInfo;
use super::service::{configured_git, run_git, run_git_within, MAX_STATUS_OUTPUT};

/// A branch name as a Windows directory path: git allows names the Windows
/// filesystem refuses (`aux`, `con.txt`, `a:b`, a trailing dot), so each
/// `/`-separated component is made creatable. Applied only on Windows, where
/// the folder never needs to read back as the branch name (git records the
/// worktree's branch itself).
fn windows_safe_dir(branch: &str) -> String {
    const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let is_reserved = |stem: &str| {
        let upper = stem.trim_end().to_ascii_uppercase();
        RESERVED.contains(&upper.as_str())
            || (upper.len() == 4
                && (upper.starts_with("COM") || upper.starts_with("LPT"))
                && matches!(upper.as_bytes()[3], b'1'..=b'9'))
    };
    branch
        .split('/')
        .map(|part| {
            let mut part: String = part
                .chars()
                .map(|c| {
                    if c.is_control() || "<>:\"|?*".contains(c) {
                        '-'
                    } else {
                        c
                    }
                })
                .collect();
            while part.ends_with('.') || part.ends_with(' ') {
                part.pop();
                part.push('-');
            }
            // The device name applies to the stem: `aux.txt` is as reserved as `aux`.
            if is_reserved(part.split('.').next().unwrap_or("")) {
                part.insert(0, '_');
            }
            part
        })
        .collect::<Vec<_>>()
        .join("/")
}

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

/// A created (or, for [`ensure_branch_worktree`], reused) worktree.
#[derive(Clone)]
pub(crate) struct Created {
    pub(crate) path: PathBuf,
    pub(crate) branch: String,
    pub(crate) included: IncludeReport,
    pub(crate) reused: bool,
}

type Refusal = (StatusCode, String);

fn refuse(status: StatusCode, message: impl Into<String>) -> Refusal {
    (status, message.into())
}

/// How long a worktree checkout may take. It writes every tracked file, so
/// it grows with the repository — a few seconds for tens of thousands of
/// files on a laptop, more on a network filesystem — far past the budget
/// git reads get, but still bounded.
#[cfg(not(test))]
const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(120);
/// Short in tests, so a checkout cut off at its deadline can be driven.
#[cfg(test)]
const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(3);

/// Parallel checkout workers (git 2.32+; an older git ignores the key).
/// Writing the files is most of a worktree's creation, and a few workers
/// halve it; the cap matches the daemon's ceiling on concurrent git
/// processes, so a shared node's cores stay shared.
fn checkout_workers() -> String {
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(4);
    format!("checkout.workers={workers}")
}

/// Create a worktree for `branch` under the managed root (off `base`, or
/// HEAD, when the branch is new) and copy what `.worktreeinclude` names. It
/// stays a dimension of the workspace `ws_id` names (whose git epoch
/// announces the change); nothing is registered. Additive: it never touches
/// an existing checkout.
async fn create_in(
    state: &Arc<AppState>,
    ws_id: &str,
    git: &Path,
    repo: &RepoInfo,
    branch: &str,
    base: Option<&str>,
) -> Result<Created, Refusal> {
    let procs = &state.git.procs;
    if !valid_branch(git, procs, &repo.toplevel, branch).await {
        return Err(refuse(StatusCode::BAD_REQUEST, "invalid branch name"));
    }
    if let Some(base) = base {
        if super::rev::resolve_commit(git, procs, &repo.toplevel, base)
            .await
            .is_none()
        {
            return Err(refuse(
                StatusCode::BAD_REQUEST,
                format!("unknown base revision {base:?}"),
            ));
        }
    }
    let path = prepare_path(state, repo, branch).await?;
    // An existing branch is checked out as-is; a new one is created off
    // `base` (or HEAD). git itself refuses if the branch is checked out
    // elsewhere.
    let exists = branch_exists(git, procs, &repo.toplevel, branch).await;
    check_out(state, ws_id, git, repo, branch, base, exists, &path).await
}

/// The managed folder for `branch`: refused when something is already
/// there, its parent made, and spelled through the parent's real path —
/// what `canonicalize` gives once git has made the folder, known before it
/// exists, so a chat can start on it while the checkout runs.
async fn prepare_path(
    state: &Arc<AppState>,
    repo: &RepoInfo,
    branch: &str,
) -> Result<PathBuf, Refusal> {
    // Managed location only. `branch` passed check-ref-format (or was
    // minted here), so it carries no `..` component; assert containment
    // anyway — a path escape here would let a later `remove` delete outside
    // the managed root.
    let leaf = if cfg!(windows) {
        windows_safe_dir(branch)
    } else {
        branch.to_string()
    };
    let path = state.worktrees_root.join(repo_key(repo)).join(leaf);
    if !path.starts_with(&state.worktrees_root) {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "branch name escapes the managed worktree root",
        ));
    }
    // Filesystem checks off the reactor (the managed root may be on NFS).
    tokio::task::spawn_blocking(move || -> Result<PathBuf, Refusal> {
        if path.exists()
            && std::fs::read_dir(&path)
                .map(|mut d| d.next().is_some())
                .unwrap_or(true)
        {
            return Err(refuse(
                StatusCode::CONFLICT,
                "a worktree for that branch already exists",
            ));
        }
        let (Some(parent), Some(leaf)) = (path.parent(), path.file_name()) else {
            return Ok(path);
        };
        std::fs::create_dir_all(parent).map_err(|err| {
            refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to create {}: {err}", parent.display()),
            )
        })?;
        let real =
            chimaera_core::paths::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        Ok(real.join(leaf))
    })
    .await
    .map_err(|err| refuse(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?
}

/// `git worktree add` into a prepared folder, then the `.worktreeinclude`
/// copy and the epoch bump. A new branch is made first, on its own: a call
/// racing for the same name fails there, before touching anything, and a
/// branch this call made is provably its own to drop again. A failed
/// checkout leaves nothing of this call's: git removes what a failed `add`
/// made; one killed past its deadline can't, so its half-written folder and
/// registration go here.
#[allow(clippy::too_many_arguments)]
async fn check_out(
    state: &Arc<AppState>,
    ws_id: &str,
    git: &Path,
    repo: &RepoInfo,
    branch: &str,
    base: Option<&str>,
    exists: bool,
    path: &Path,
) -> Result<Created, Refusal> {
    let procs = &state.git.procs;
    if !exists {
        let mut args = vec!["branch", branch];
        if let Some(base) = base {
            args.push(base);
        }
        let made = run_git(git, procs, &repo.toplevel, &args, 4096)
            .await
            .map_err(|err| refuse(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
        if !made.success {
            return Err(refuse(StatusCode::CONFLICT, made.stderr));
        }
    }
    let path_str = path.to_string_lossy().into_owned();
    let workers = checkout_workers();
    let added = run_git_within(
        git,
        procs,
        &repo.toplevel,
        &["-c", &workers, "worktree", "add", &path_str, branch],
        64 * 1024,
        CHECKOUT_TIMEOUT,
    )
    .await;
    let failed = match added {
        Ok(out) if out.success => None,
        // git's own message is the most useful thing we can say here (branch
        // already checked out in another worktree, …).
        Ok(out) => Some((refuse(StatusCode::CONFLICT, out.stderr), false)),
        Err(err) => Some((
            refuse(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            true,
        )),
    };
    if let Some((refusal, killed)) = failed {
        undo_failed_add(
            git,
            procs,
            &repo.toplevel,
            killed.then_some(path),
            (!exists).then_some(branch),
        )
        .await;
        state.git.bump(ws_id);
        state.git.invalidate(ws_id);
        state.changes.notify_waiters();
        return Err(refusal);
    }

    // A worktree is a dimension of the workspace it was made from, never a
    // workspace of its own: nothing is registered, so no window moves and the
    // home screen gains no entry. Opening it as its own window stays possible
    // like any folder (POST /workspaces), and a removal still drops such a
    // registration (see `remove_worktree`).

    // Ignored files the repo asks to carry over (`.env` and the like).
    let included = copy_included(git, procs, &repo.toplevel, path).await;

    // The repo's worktree list changed: every window watching it refetches.
    // Invalidate too — the refetch this bump triggers must recompute, not be
    // served a pre-op result still inside the share's reuse window.
    state.git.bump(ws_id);
    state.git.invalidate(ws_id);
    state.changes.notify_waiters();

    Ok(Created {
        path: path.to_path_buf(),
        branch: branch.to_string(),
        included,
        reused: false,
    })
}

/// Undo a failed checkout's own leftovers. `killed_at`: the folder of an
/// `add` killed past its deadline — git keeps it registered and locked as
/// "initializing", so only a double `--force` drops the registration — this
/// call's alone (it was empty or missing, under the managed root, and this
/// call's git was writing it). `made_branch`: the branch this call made.
async fn undo_failed_add(
    git: &Path,
    procs: &Semaphore,
    toplevel: &Path,
    killed_at: Option<&Path>,
    made_branch: Option<&str>,
) {
    if let Some(path) = killed_at {
        let path_str = path.to_string_lossy().into_owned();
        let _ = run_git(
            git,
            procs,
            toplevel,
            &["worktree", "remove", "--force", "--force", &path_str],
            4096,
        )
        .await;
        let leftover = path.to_path_buf();
        let _ = tokio::task::spawn_blocking(move || {
            if leftover.exists() {
                if let Err(err) = std::fs::remove_dir_all(&leftover) {
                    tracing::warn!(path = %leftover.display(), %err,
                        "could not remove a cut-off worktree checkout");
                }
            }
        })
        .await;
    }
    if let Some(branch) = made_branch {
        // `-d`, never `-D`: made by this call at its base, it holds nothing
        // of its own, and `-d` refuses one another checkout now uses.
        match run_git(git, procs, toplevel, &["branch", "-d", branch], 4096).await {
            Ok(out) if out.success => {}
            Ok(out) => tracing::warn!(%branch, stderr = %out.stderr,
                "kept the branch of a failed worktree checkout"),
            Err(err) => tracing::warn!(%branch, %err,
                "kept the branch of a failed worktree checkout"),
        }
    }
}

/// The worktree that has `branch` checked out in `workspace`'s repository,
/// creating a managed one (off `base`) when none does — the Mastermind's
/// `spawn_agent {branch}`. An existing checkout of the branch, managed or
/// not, is reused as-is.
pub(crate) async fn ensure_branch_worktree(
    state: &Arc<AppState>,
    workspace: &crate::workspaces::Workspace,
    branch: &str,
    base: Option<&str>,
) -> Result<Created, Refusal> {
    let git = state.git.resolve_git(configured_git(state)).await;
    if !git.adequate {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "git is missing or too old for worktrees",
        ));
    }
    let Some(repo) = state
        .git
        .discover(&git.path, &workspace.id, &workspace.root)
        .await
        .into_repo()
    else {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "this workspace is not a git repository",
        ));
    };
    let branch = branch.trim();
    if let Ok(list) = state.git.worktrees(&git.path, &repo).await {
        if let Some(existing) = list.iter().find(|w| w.branch.as_deref() == Some(branch)) {
            return Ok(Created {
                path: existing.path.clone(),
                branch: branch.to_string(),
                included: IncludeReport::default(),
                reused: true,
            });
        }
    }
    create_in(state, &workspace.id, &git.path, &repo, branch, base).await
}

/// Where a new agent session's own worktree goes: a fresh branch
/// `chimaera/<agent>-<6 hex>` off the workspace repository's current HEAD,
/// checked out at `<managed root>/<repo>-<hash>/chimaera/<agent>-<6 hex>`
/// (the managed root is `~/.chimaera/worktrees`). Outside the repository on
/// purpose: the main checkout's status stays clean, nothing is written into
/// a shared repository, and the removal fences only ever act under the
/// managed root. It is an ordinary registered worktree, locked
/// (`chimaera: <session>`) while an agent works inside, so `git worktree
/// list`-based clean-up (Claude Code's, `scripts/worktree-gc`) sees it as
/// in use; once unlocked it is judged like any other worktree.
pub(crate) async fn create_session_worktree(
    state: &Arc<AppState>,
    workspace: &crate::workspaces::Workspace,
    agent: &str,
) -> Result<Created, Refusal> {
    plan_session_worktree(state, workspace, agent)
        .await?
        .check_out(state)
        .await
}

/// A new agent session's worktree, decided but not yet checked out: the
/// branch is free and its folder ready, so a chat can open on them while
/// [`PlannedWorktree::check_out`] writes the files.
pub(crate) struct PlannedWorktree {
    ws_id: String,
    git: PathBuf,
    repo: RepoInfo,
    pub(crate) branch: String,
    pub(crate) path: PathBuf,
}

impl PlannedWorktree {
    /// The repository's top level, which the new worktree copies at HEAD.
    pub(crate) fn source(&self) -> &Path {
        &self.repo.toplevel
    }

    pub(crate) async fn check_out(self, state: &Arc<AppState>) -> Result<Created, Refusal> {
        check_out(
            state,
            &self.ws_id,
            &self.git,
            &self.repo,
            &self.branch,
            None,
            false,
            &self.path,
        )
        .await
    }
}

/// The quick half of [`create_session_worktree`]: a free branch name and its
/// prepared folder (one `rev-parse` per name tried; git's location and the
/// repository come from caches).
pub(crate) async fn plan_session_worktree(
    state: &Arc<AppState>,
    workspace: &crate::workspaces::Workspace,
    agent: &str,
) -> Result<PlannedWorktree, Refusal> {
    let git = state.git.resolve_git(configured_git(state)).await;
    if !git.adequate {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "git is missing or too old for worktrees",
        ));
    }
    let Some(repo) = state
        .git
        .discover(&git.path, &workspace.id, &workspace.root)
        .await
        .into_repo()
    else {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "this workspace is not a git repository",
        ));
    };
    let agent: String = agent
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(24)
        .collect::<String>()
        .to_ascii_lowercase();
    let agent = if agent.is_empty() {
        "agent".into()
    } else {
        agent
    };
    // A clash (an existing branch of that name) just draws again; never
    // check out somebody's existing branch for a "new worktree". The name is
    // valid by construction, so it skips `check-ref-format`.
    for _ in 0..4 {
        let branch = format!("chimaera/{agent}-{}", six_hex());
        if branch_exists(&git.path, &state.git.procs, &repo.toplevel, &branch).await {
            continue;
        }
        let path = prepare_path(state, &repo, &branch).await?;
        return Ok(PlannedWorktree {
            ws_id: workspace.id.clone(),
            git: git.path.clone(),
            repo,
            branch,
            path,
        });
    }
    Err(refuse(
        StatusCode::CONFLICT,
        "could not pick a free branch name for the worktree",
    ))
}

/// Six random hex digits naming a session worktree. `generate_token` is 64
/// lowercase hex characters drawn from 32 random bytes, so any six of them
/// are six uniformly random hex digits — always exactly six.
fn six_hex() -> String {
    chimaera_core::generate_token()[..6].to_string()
}

/// Undo [`create_session_worktree`] for a spawn that then failed: the
/// worktree is new, clean and empty, so a plain `worktree remove` (never
/// `--force`) takes it, and the branch minted for it goes with `branch -d`
/// (which refuses a branch holding commits of its own). A reused checkout
/// is never touched. Failures are logged, never escalated.
pub(crate) async fn discard_session_worktree(
    state: &Arc<AppState>,
    workspace: &crate::workspaces::Workspace,
    created: &Created,
) {
    if created.reused {
        return;
    }
    let git = state.git.resolve_git(configured_git(state)).await;
    let Some(repo) = state
        .git
        .discover(&git.path, &workspace.id, &workspace.root)
        .await
        .into_repo()
    else {
        return;
    };
    let path = chimaera_core::paths::wire_path(&created.path);
    let procs = &state.git.procs;
    match run_git(
        &git.path,
        procs,
        &repo.toplevel,
        &["worktree", "remove", &path],
        64 * 1024,
    )
    .await
    {
        Ok(out) if out.success => {}
        Ok(out) => {
            tracing::warn!(%path, stderr = %out.stderr, "could not discard an unused session worktree");
            return;
        }
        Err(err) => {
            tracing::warn!(%path, %err, "could not discard an unused session worktree");
            return;
        }
    }
    // The name passed check-ref-format and never starts with '-'.
    match run_git(
        &git.path,
        procs,
        &repo.toplevel,
        &["branch", "-d", &created.branch],
        4096,
    )
    .await
    {
        Ok(out) if out.success => {}
        Ok(out) => tracing::warn!(branch = %created.branch, stderr = %out.stderr,
            "kept the branch of a discarded session worktree"),
        Err(err) => tracing::warn!(branch = %created.branch, %err,
            "kept the branch of a discarded session worktree"),
    }
    state.git.bump(&workspace.id);
    state.git.invalidate(&workspace.id);
    state.changes.notify_waiters();
}

/// The Chimaera-managed worktree a session started in — the worktree's top
/// level, also when the session started in a subfolder of it. Read from
/// what the session tracker already resolved (the spawn folder's probe, else
/// the session's own checkout when the spawn folder lies inside it), so it
/// does no filesystem access and is safe on every session snapshot; `None`
/// until the tracker has looked, and outside the managed root.
pub(crate) fn managed_worktree_of(state: &AppState, id: &str, cwd: &Path) -> Option<PathBuf> {
    let managed = |p: &Path| {
        p.starts_with(&state.worktrees_root_real) || p.starts_with(&state.worktrees_root)
    };
    if !managed(cwd) {
        return None;
    }
    let top = state.git.sessions.toplevel_of(cwd).or_else(|| {
        state
            .git
            .sessions
            .repo_of(id)
            .map(|r| r.toplevel)
            .filter(|t| cwd.starts_with(t))
    })?;
    managed(&top).then_some(top)
}

/// Unregister workspace `id` and drop everything keyed by it (a worktree
/// opened as its own window, once that worktree is gone). Never touches a
/// folder.
pub(crate) async fn unregister_workspace(state: &Arc<AppState>, id: &str) {
    if let Err(err) = crate::lock(&state.workspaces).remove(id) {
        tracing::warn!(%err, %id, "failed to unregister removed worktree");
    }
    // Its git state and quick-open index go with it (see `delete_workspace`
    // — same eviction).
    state.git.forget_workspace(id);
    crate::lock(&state.quickopen).forget_workspace(id);
    state.timeline.remove_workspace(id);
    state.comms.forget_workspace(id);
    crate::lock(&state.plugin_detect).forget_workspace(id);
    state.plugin_runtime.forget_workspace(id);
    crate::lock(&state.plugin_state).forget_workspace(id);
    crate::plugins::platform::forget_workspace(state, id);
    crate::lock(&state.knowledge).forget_workspace(id);
    state.history.remove_workspace(id);
    if crate::lock(&state.recents_archive).forget_workspace(id) {
        crate::recents_archive::persist(state).await;
    }
}

/// Drop registrations of managed worktrees whose folder is gone (removed
/// outside chimaera — `git worktree remove`, a clean-up script — after one
/// was opened in its own window). Only workspaces under the managed root are
/// checked, so the usual list costs no filesystem access; only a definite
/// "not found" counts (a slow or failing mount keeps the row).
pub(crate) async fn forget_missing_managed_workspaces(state: &Arc<AppState>) {
    let candidates: Vec<(String, PathBuf)> = crate::lock(&state.workspaces)
        .list()
        .into_iter()
        .filter(|w| {
            w.root.starts_with(&state.worktrees_root_real)
                || w.root.starts_with(&state.worktrees_root)
        })
        .map(|w| (w.id, w.root))
        .collect();
    if candidates.is_empty() {
        return;
    }
    let gone: Vec<String> = tokio::task::spawn_blocking(move || {
        candidates
            .into_iter()
            .filter(|(_, root)| {
                matches!(std::fs::metadata(root), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
            })
            .map(|(id, _)| id)
            .collect()
    })
    .await
    .unwrap_or_default();
    for id in gone {
        unregister_workspace(state, &id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::windows_safe_dir;

    #[test]
    fn windows_dir_names_avoid_reserved_devices_and_bad_characters() {
        assert_eq!(windows_safe_dir("feature/login"), "feature/login");
        assert_eq!(windows_safe_dir("aux"), "_aux");
        assert_eq!(windows_safe_dir("Con.txt"), "_Con.txt");
        assert_eq!(windows_safe_dir("fix/com1"), "fix/_com1");
        assert_eq!(windows_safe_dir("lpt9.x.y"), "_lpt9.x.y");
        assert_eq!(windows_safe_dir("com0"), "com0");
        assert_eq!(windows_safe_dir("auxiliary"), "auxiliary");
        assert_eq!(windows_safe_dir("a:b|c?d*e\"f<g>"), "a-b-c-d-e-f-g-");
        assert_eq!(windows_safe_dir("x\u{1}y"), "x-y");
        assert_eq!(windows_safe_dir("name."), "name-");
        assert_eq!(windows_safe_dir("dir./leaf "), "dir-/leaf-");
    }

    #[test]
    fn session_worktree_suffix_is_six_hex_digits() {
        for _ in 0..64 {
            let s = super::six_hex();
            assert_eq!(s.len(), 6, "{s}");
            assert!(
                s.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{s}"
            );
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct CreateWorktree {
    workspace_id: String,
    /// Branch to check out. Created off `base` (or HEAD) when it does not exist.
    branch: String,
    /// Start point for a NEW branch; HEAD when omitted.
    #[serde(default)]
    base: Option<String>,
    /// Which of the workspace's repositories (its top level); the one at or
    /// around the root when absent.
    #[serde(default)]
    repo: Option<String>,
}

/// POST /api/v1/git/worktrees — create a worktree for `branch` under the managed
/// root. It stays part of the workspace it was made from (a Branches row, a
/// place an agent can start in); the window never moves and no workspace is
/// registered, so `workspace` in the answer is always null (kept for the
/// wire's shape). Additive: it never touches an existing checkout. The
/// answer's additive `included` reports what `.worktreeinclude` copied.
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
    let picked = match super::http::pick_repo(&state, &git.path, &ws, body.repo.as_deref()).await {
        Ok(outcome) => outcome,
        Err(refusal) => return refusal,
    };
    let Some(repo) = picked.into_repo() else {
        return bad_request("not a git repository");
    };
    let base = body
        .base
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty());
    match create_in(
        &state,
        &body.workspace_id,
        &git.path,
        &repo,
        body.branch.trim(),
        base,
    )
    .await
    {
        Ok(created) => Json(json!({
            "worktree": {"path": chimaera_core::paths::wire_path(&created.path), "branch": created.branch},
            "workspace": null,
            "included": created.included.json(),
        }))
        .into_response(),
        Err((status, message)) => (status, Json(json!({"error": message}))).into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct RemoveWorktree {
    workspace_id: String,
    /// Absolute path of the worktree to remove.
    path: String,
    /// Remove even with uncommitted changes.
    #[serde(default)]
    force: bool,
    /// Which of the workspace's repositories the worktree belongs to.
    #[serde(default)]
    repo: Option<String>,
}

/// Does the checkout at `target` hold commits that exist nowhere else — on
/// no upstream and in no branch it merged into? `base_ref` is what "merged"
/// means here (the main checkout's branch). Errors read as "yes": a remove
/// that can't prove it is safe does not happen without `force`.
async fn has_unshared_commits(
    git: &Path,
    procs: &Semaphore,
    target: &Path,
    base_ref: Option<&str>,
) -> bool {
    let upstream = run_git(
        git,
        procs,
        target,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        4096,
    )
    .await;
    if matches!(&upstream, Ok(out) if out.success) {
        return match run_git(
            git,
            procs,
            target,
            &["rev-list", "--count", "@{u}..HEAD"],
            4096,
        )
        .await
        {
            Ok(out) if out.success => String::from_utf8_lossy(&out.stdout).trim() != "0",
            _ => true,
        };
    }
    let Some(base) = base_ref else {
        return true;
    };
    match run_git(
        git,
        procs,
        target,
        &["merge-base", "--is-ancestor", "HEAD", base],
        4096,
    )
    .await
    {
        Ok(out) => !out.success,
        Err(_) => true,
    }
}

/// DELETE /api/v1/git/worktrees — remove a MANAGED worktree. Destructive, so it
/// is fenced five ways: it must live under the managed root (Chimaera never
/// deletes a checkout it did not create), it must not be the workspace you are
/// looking at, no live session may be sitting inside it, it must be clean,
/// and its branch must not hold commits that are neither pushed nor merged —
/// the last two unless `force`. The branch itself is left alone.
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
    let picked = match super::http::pick_repo(&state, &git.path, &ws, body.repo.as_deref()).await {
        Ok(outcome) => outcome,
        Err(refusal) => return refusal,
    };
    let Some(repo) = picked.into_repo() else {
        return bad_request("not a git repository");
    };
    let (target, managed) = {
        let raw = PathBuf::from(&body.path);
        let root = state.worktrees_root.clone();
        match tokio::task::spawn_blocking(move || {
            (
                chimaera_core::paths::canonicalize(&raw).ok(),
                chimaera_core::paths::canonicalize(&root).unwrap_or(root),
            )
        })
        .await
        {
            Ok((Some(target), managed)) => (target, managed),
            _ => return bad_request("no such worktree"),
        }
    };

    // Fence 1: only what we created.
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
    // Every surface counts: a shell's polled cwd, a PTY's spawn cwd, a
    // chat's cwd, and the folder an agent's hooks last reported.
    let inside: Vec<String> = {
        let hooks = state.git.sessions.rows();
        let mut inside: std::collections::BTreeMap<String, String> = Default::default();
        {
            let cwds = crate::lock(&state.current_cwds);
            for info in state.sessions.list().into_iter().filter(|i| i.alive) {
                let cwd = cwds
                    .get(&info.id)
                    .cloned()
                    .unwrap_or_else(|| info.cwd.clone());
                let hook_inside = hooks
                    .get(&info.id)
                    .and_then(|r| r.hook_cwd.as_ref())
                    .is_some_and(|c| c.starts_with(&target));
                if cwd.starts_with(&target) || hook_inside {
                    inside.insert(info.id.clone(), info.name.clone());
                }
            }
        }
        for chat in state.chat.list().into_iter().filter(|c| c.alive) {
            let hook_inside = hooks
                .get(&chat.id)
                .and_then(|r| r.hook_cwd.as_ref())
                .is_some_and(|c| c.starts_with(&target));
            if chat.cwd.starts_with(&target) || hook_inside {
                inside.insert(chat.id.clone(), chat.agent.clone());
            }
        }
        inside.into_values().collect()
    };
    if !inside.is_empty() {
        return conflict(&format!(
            "{} live session(s) are inside that worktree: {}",
            inside.len(),
            inside.join(", ")
        ));
    }
    if !body.force {
        // Fence 4: uncommitted work is not ours to throw away.
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
        // Fence 5: commits nobody else has. The branch survives a remove, but
        // a worktree is where people look for work — don't make it vanish
        // while that work exists nowhere else.
        let base_ref = main_branch_ref(&state, &git.path, &repo).await;
        if has_unshared_commits(&git.path, &state.git.procs, &target, base_ref.as_deref()).await {
            return conflict(
                "this branch has commits that are neither pushed nor merged — \
                 remove it from a terminal if you mean to",
            );
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
        unregister_workspace(&state, &id).await;
    }

    // Bump + invalidate, same pairing as the add path: the triggered
    // refetch must never reuse a pre-removal shared result.
    state.git.bump(&body.workspace_id);
    state.git.invalidate(&body.workspace_id);
    state.changes.notify_waiters();
    StatusCode::NO_CONTENT.into_response()
}

/// What "merged" means for this repository's branches: the branch the main
/// checkout has out (`refs/heads/<name>`), else its detached commit.
pub(super) async fn main_branch_ref(
    state: &AppState,
    git: &Path,
    repo: &RepoInfo,
) -> Option<String> {
    let list = state.git.worktrees(git, repo).await.ok()?;
    let main = list.first()?;
    match (&main.branch, &main.sha) {
        (Some(branch), _) => Some(format!("refs/heads/{branch}")),
        (None, Some(sha)) => Some(sha.clone()),
        _ => None,
    }
}

/// Validate a session's requested starting folder: it must be a directory
/// inside the workspace's root or inside one of its repository's worktrees
/// (never an arbitrary path). Returns the canonical folder.
pub(crate) async fn allowed_session_cwd(
    state: &AppState,
    workspace: &crate::workspaces::Workspace,
    raw: &str,
) -> Result<PathBuf, String> {
    let requested = PathBuf::from(raw);
    if raw.is_empty() || raw.len() > 4096 || !requested.is_absolute() {
        return Err("cwd must be an absolute path".to_string());
    }
    let canonical = tokio::task::spawn_blocking(move || {
        chimaera_core::paths::canonicalize(&requested)
            .ok()
            .filter(|p| p.is_dir())
    })
    .await
    .ok()
    .flatten()
    .ok_or_else(|| format!("no such folder {raw:?}"))?;
    if canonical.starts_with(&workspace.root) {
        return Ok(canonical);
    }
    let git = state.git.resolve_git(configured_git(state)).await;
    if git.adequate {
        // The worktrees of the workspace's repositories (the primary first;
        // a bounded handful of the ones below the root).
        let mut repos: Vec<RepoInfo> = state
            .git
            .discover(&git.path, &workspace.id, &workspace.root)
            .await
            .into_repo()
            .into_iter()
            .collect();
        repos.extend(
            state
                .git
                .known_toplevels(&workspace.id)
                .into_iter()
                .filter_map(|t| state.git.found_repo(&workspace.id, &t)),
        );
        for repo in repos.iter().take(8) {
            if let Ok(list) = state.git.worktrees(&git.path, repo).await {
                if list.iter().any(|w| canonical.starts_with(&w.path)) {
                    return Ok(canonical);
                }
            }
        }
    }
    Err("cwd must be inside the workspace or one of its worktrees".to_string())
}
