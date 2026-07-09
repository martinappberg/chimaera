use std::path::Path;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

use super::parse::{status_json, RepoInfo, StatusData};
use super::resolve::{GitBinary, MIN_GIT};
use super::service::{configured_git, run_git, GitService, ProbeOutcome};

/// Cap on each side of a diff; larger files bail to "open the file instead".
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;

#[derive(Deserialize)]
pub(crate) struct StatusQuery {
    workspace_id: String,
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
    let repo = match state
        .git
        .discover(&git.path, &q.workspace_id, &ws.root)
        .await
    {
        ProbeOutcome::Repo(repo) => repo,
        // Not a repo, or git couldn't read one (dubious ownership, a wedged
        // filesystem). `repo_error` is the reason for the latter — the client
        // turns it into an actionable panel instead of a blank "no repo".
        other => {
            let epoch = state.git.epoch(&q.workspace_id);
            return Json(json!({
                "repo": false,
                "git_ok": true,
                "git": git.json(),
                "repo_error": other.error(),
                "workspace_id": q.workspace_id,
                "epoch": epoch,
            }))
            .into_response();
        }
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
        .into_repo()
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

pub(super) fn conflict(message: &str) -> Response {
    (StatusCode::CONFLICT, Json(json!({"error": message}))).into_response()
}

pub(super) fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response()
}

/// The mutation handlers' response when the resolved git can't run the service.
pub(super) fn git_too_old(git: &GitBinary) -> Response {
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
        .into_repo()
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
    fn repo_relative_rejects_escapes() {
        let top = Path::new("/repo");
        assert_eq!(
            repo_relative(top, "/repo/src/x.rs").as_deref(),
            Some("src/x.rs")
        );
        assert_eq!(repo_relative(top, "/other/x.rs"), None);
    }
}
