use std::path::{Path, PathBuf};
use std::sync::Arc;

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
use super::service::{configured_git, run_git, MAX_STATUS_OUTPUT};

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

    // Managed location only. `branch` passed check-ref-format, so it carries no
    // `..` component; assert containment anyway — a path escape here would let a
    // later `remove` delete outside the managed root.
    let path = state.worktrees_root.join(repo_key(repo)).join(branch);
    if !path.starts_with(&state.worktrees_root) {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "branch name escapes the managed worktree root",
        ));
    }
    // Filesystem checks off the reactor (the managed root may be on NFS).
    let prepared = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || -> Result<(), Refusal> {
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
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    refuse(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("failed to create {}: {err}", parent.display()),
                    )
                })?;
            }
            Ok(())
        })
        .await
        .map_err(|err| refuse(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?
    };
    prepared?;

    let path_str = path.to_string_lossy().into_owned();
    // An existing branch is checked out as-is; a new one is created off `base`
    // (or HEAD). git itself refuses if the branch is checked out elsewhere.
    let exists = branch_exists(git, procs, &repo.toplevel, branch).await;
    let mut args: Vec<&str> = vec!["worktree", "add"];
    if exists {
        args.push(&path_str);
        args.push(branch);
    } else {
        args.push("-b");
        args.push(branch);
        args.push(&path_str);
        if let Some(base) = base {
            args.push(base);
        }
    }
    let out = run_git(git, procs, &repo.toplevel, &args, 64 * 1024)
        .await
        .map_err(|err| refuse(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    if !out.success {
        // git's own message is the most useful thing we can say here (branch
        // already checked out in another worktree, bad base, …).
        return Err(refuse(StatusCode::CONFLICT, out.stderr));
    }

    // A worktree is a dimension of the workspace it was made from, never a
    // workspace of its own: nothing is registered, so no window moves and the
    // home screen gains no entry. Opening it as its own window stays possible
    // like any folder (POST /workspaces), and a removal still drops such a
    // registration (see `remove_worktree`).
    let canonical = {
        let raw = path.clone();
        tokio::task::spawn_blocking(move || std::fs::canonicalize(&raw).unwrap_or(raw))
            .await
            .unwrap_or_else(|_| path.clone())
    };

    // Ignored files the repo asks to carry over (`.env` and the like).
    let included = copy_included(git, procs, &repo.toplevel, &canonical).await;

    // The repo's worktree list changed: every window watching it refetches.
    // Invalidate too — the refetch this bump triggers must recompute, not be
    // served a pre-op result still inside the share's reuse window.
    state.git.bump(ws_id);
    state.git.invalidate(ws_id);
    state.changes.notify_waiters();

    Ok(Created {
        path: canonical,
        branch: branch.to_string(),
        included,
        reused: false,
    })
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
            "worktree": {"path": created.path.to_string_lossy(), "branch": created.branch},
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
                std::fs::canonicalize(&raw).ok(),
                std::fs::canonicalize(&root).unwrap_or(root),
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
        if let Err(err) = crate::lock(&state.workspaces).remove(&id) {
            tracing::warn!(%err, %id, "failed to unregister removed worktree");
        }
        // The unregistered workspace's git state and quick-open index go
        // with it (see `delete_workspace` — same eviction).
        state.git.forget_workspace(&id);
        crate::lock(&state.quickopen).forget_workspace(&id);
        state.timeline.remove_workspace(&id);
        crate::lock(&state.plugin_detect).forget_workspace(&id);
        state.plugin_runtime.forget_workspace(&id);
        crate::lock(&state.plugin_state).forget_workspace(&id);
        crate::plugins::platform::forget_workspace(&state, &id);
        crate::lock(&state.knowledge).forget_workspace(&id);
        state.history.remove_workspace(&id);
        if crate::lock(&state.recents_archive).forget_workspace(&id) {
            crate::recents_archive::persist(&state).await;
        }
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
        std::fs::canonicalize(&requested)
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
