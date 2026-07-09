use std::path::Path;
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
        .into_repo()
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
        .into_repo()
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
