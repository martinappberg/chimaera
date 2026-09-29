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

/// Managed worktrees checked for "merged" per worktree listing.
const MAX_MERGE_CHECKS: usize = 16;

/// Local branches listed (read-only: name, last commit date, upstream).
const MAX_BRANCHES: usize = 100;

#[derive(Deserialize)]
pub(crate) struct StatusQuery {
    workspace_id: String,
    /// One of the workspace's repositories (its top level); the one at or
    /// around the root when absent — the pre-several-repositories behavior.
    #[serde(default)]
    repo: Option<String>,
}

/// Linked worktrees are looked for in at most this many known repositories
/// when a `repo=` names none of them directly.
const MAX_WORKTREE_LOOKUPS: usize = 8;

/// The repository a request names. `repo` must be one the workspace knows —
/// its primary, one found below its root, or a worktree `git worktree list`
/// reports for one of those — never an arbitrary path (git never runs inside
/// a folder before it is validated). Absent, it is the primary, exactly as
/// before: `Ok(outcome)` of the root's probe.
pub(super) async fn pick_repo(
    state: &AppState,
    git: &Path,
    ws: &crate::workspaces::Workspace,
    repo: Option<&str>,
) -> Result<ProbeOutcome, Response> {
    let primary = state.git.discover(git, &ws.id, &ws.root).await;
    let Some(raw) = repo.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(primary);
    };
    let wanted = std::path::PathBuf::from(raw);
    let unknown = || bad_request("not one of this workspace's repositories");
    if !wanted.is_absolute() {
        return Err(unknown());
    }
    if let Some(p) = primary.repo() {
        if p.toplevel == wanted {
            return Ok(primary);
        }
    }
    if let Some(found) = state.git.found_repo(&ws.id, &wanted) {
        return Ok(ProbeOutcome::Repo(found));
    }
    // A worktree of a known repository (a branch row's "changes on this
    // branch"): only what git lists for a repository we already trust.
    let mut known: Vec<RepoInfo> = primary.repo().cloned().into_iter().collect();
    known.extend(
        state
            .git
            .known_toplevels(&ws.id)
            .into_iter()
            .filter_map(|t| state.git.found_repo(&ws.id, &t)),
    );
    for repo in known.iter().take(MAX_WORKTREE_LOOKUPS) {
        let Ok(list) = state.git.worktrees(git, repo).await else {
            continue;
        };
        if list.iter().any(|w| w.path == wanted) {
            if let ProbeOutcome::Repo(info) =
                super::service::probe_repo(git, &state.git.procs, &wanted).await
            {
                if info.toplevel == wanted && info.common_dir == repo.common_dir {
                    return Ok(ProbeOutcome::Repo(info));
                }
            }
        }
    }
    Err(unknown())
}

#[derive(Deserialize)]
pub(crate) struct ReposQuery {
    workspace_id: String,
    /// Probe again (the panel's refresh button).
    #[serde(default)]
    refresh: bool,
}

/// GET /api/v1/git/repos?workspace_id=&refresh= — the workspace's
/// repositories: the one at or around its root, then those below it (a
/// bounded two-level probe at open, file-tree listings, agents' folders,
/// submodules), at most 32. Each: top level, path in the workspace, kind
/// (`root` | `enclosing` | `nested` | `submodule`), the repository it sits
/// in, and its branch (read from `HEAD`, no process). Change counts come
/// from each repository's own `GET /git/status?repo=`.
pub(crate) async fn repos(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReposQuery>,
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
        return Json(json!({
            "workspace_id": q.workspace_id,
            "git_ok": false,
            "git": git.json(),
            "repos": [],
            "capped": false,
            "epoch": state.git.epoch(&q.workspace_id),
        }))
        .into_response();
    }
    let listing = super::repos::discover_all(&state, &git.path, &ws, q.refresh).await;
    let mut entries: Vec<(RepoInfo, super::repos::RepoKind)> = Vec::new();
    if let Some(p) = listing.primary.repo() {
        let kind = if p.toplevel == ws.root {
            super::repos::RepoKind::Root
        } else {
            super::repos::RepoKind::Enclosing
        };
        entries.push((p.clone(), kind));
    }
    entries.extend(listing.found.iter().map(|f| (f.info.clone(), f.kind)));
    let git_dirs: Vec<std::path::PathBuf> =
        entries.iter().map(|(r, _)| r.git_dir.clone()).collect();
    let heads = tokio::task::spawn_blocking(move || {
        git_dirs
            .iter()
            .map(|d| super::parse::read_head_blocking(d))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let tops: Vec<std::path::PathBuf> = entries.iter().map(|(r, _)| r.toplevel.clone()).collect();
    let items: Vec<serde_json::Value> = entries
        .iter()
        .enumerate()
        .map(|(i, (repo, kind))| {
            let (branch, detached, head) = match heads.get(i) {
                Some(super::parse::HeadRef::Branch(b)) => (Some(b.clone()), false, None),
                Some(super::parse::HeadRef::Detached(sha)) => {
                    (None, true, Some(sha.chars().take(7).collect::<String>()))
                }
                _ => (None, false, None),
            };
            // The repository this one sits in (the innermost other one
            // containing it), for the panel's nesting and submodule marks.
            let parent = tops
                .iter()
                .filter(|t| **t != repo.toplevel && repo.toplevel.starts_with(t))
                .max_by_key(|t| t.as_os_str().len())
                .map(|t| t.to_string_lossy().into_owned());
            let rel = match kind {
                super::repos::RepoKind::Enclosing => None,
                _ => repo.toplevel.strip_prefix(&ws.root).ok().map(|r| {
                    let r = r.to_string_lossy().into_owned();
                    if r.is_empty() {
                        ".".to_string()
                    } else {
                        r
                    }
                }),
            };
            json!({
                "path": repo.toplevel.to_string_lossy(),
                "rel": rel,
                "kind": kind.as_str(),
                "submodule": *kind == super::repos::RepoKind::Submodule,
                "parent": parent,
                "branch": branch,
                "detached": detached,
                "head": head,
                "epoch": state.git.repo_epoch(&q.workspace_id, &repo.toplevel),
            })
        })
        .collect();
    Json(json!({
        "workspace_id": q.workspace_id,
        "git_ok": true,
        "git": git.json(),
        "repos": items,
        "capped": listing.capped,
        "primary_error": listing.primary.error(),
        "epoch": state.git.epoch(&q.workspace_id),
    }))
    .into_response()
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
    let picked = match pick_repo(&state, &git.path, &ws, q.repo.as_deref()).await {
        Ok(outcome) => outcome,
        Err(refusal) => return refusal,
    };
    let repo = match picked {
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
    // Single-flighted per workspace: an epoch bump makes every watching
    // window call this handler at once, and they all share one status run
    // (see `GitService::status_shared`).
    match state
        .git
        .status_shared(&git.path, &q.workspace_id, &repo)
        .await
    {
        Ok(shared) => {
            let (epoch, bumped) = if shared.flushed {
                // A change was announced while this run was in flight: the
                // data may predate it. Serve it (it is what a direct run
                // would have returned) but don't re-seed it as the published
                // baseline — that would force a second bump and a second
                // full fan-out per announced change.
                (state.git.epoch(&q.workspace_id), false)
            } else {
                // Publishing may discover an unannounced change (an external
                // editor, a terminal `git` command) and bump the epoch; read
                // the epoch after, so THIS response is already current and
                // the caller won't refetch.
                state.git.publish(&q.workspace_id, &repo, &shared.data)
            };
            if bumped {
                state.changes.notify_waiters();
            }
            // Submodules the status marks become repositories of the
            // workspace (they may sit deeper than the open-time probe).
            let subs: Vec<String> = shared
                .data
                .entries
                .iter()
                .filter(|e| e.submodule)
                .map(|e| e.rel().to_string())
                .collect();
            if !subs.is_empty() {
                super::repos::note_submodules(&state, &ws, &repo.toplevel, subs).await;
            }
            let mut body = status_json(&q.workspace_id, epoch, &repo, &shared.data);
            body["git_ok"] = json!(true);
            body["git"] = git.json();
            body["repo_epoch"] = json!(state.git.repo_epoch(&q.workspace_id, &repo.toplevel));
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
            body["repo_epoch"] = json!(state.git.repo_epoch(&q.workspace_id, &repo.toplevel));
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
    let picked = match pick_repo(&state, &git.path, &ws, q.repo.as_deref()).await {
        Ok(outcome) => outcome,
        Err(refusal) => return refusal,
    };
    let Some(repo) = picked.into_repo() else {
        return Json(json!({"repo": false, "worktrees": []})).into_response();
    };
    match state.git.worktrees(&git.path, &repo).await {
        Ok(list) => {
            let managed_root = {
                let root = state.worktrees_root.clone();
                tokio::task::spawn_blocking(move || std::fs::canonicalize(&root).unwrap_or(root))
                    .await
                    .unwrap_or_else(|_| state.worktrees_root.clone())
            };
            // Against the main checkout's branch, per other worktree (a
            // bounded handful per refresh): how far ahead/behind it is, and
            // — for a managed one, the only kind chimaera removes — whether
            // the main branch already contains it ("merged").
            let base_ref = match list.first() {
                Some(main) => match (&main.branch, &main.sha) {
                    (Some(b), _) => Some(format!("refs/heads/{b}")),
                    (None, Some(sha)) => Some(sha.clone()),
                    _ => None,
                },
                None => None,
            };
            // The main checkout's HEAD is the tip of `base_ref`: with the
            // worktree's sha it keys the cache (two commits never change
            // their distance).
            let base_sha = list.first().and_then(|m| m.sha.clone());
            let mut vs_main: Vec<Option<(u64, u64)>> = vec![None; list.len()];
            for (i, w) in list.iter().enumerate().take(MAX_MERGE_CHECKS + 1).skip(1) {
                let (Some(base), Some(sha)) = (&base_ref, &w.sha) else {
                    continue;
                };
                if !super::anchor::is_sha(sha) {
                    continue;
                }
                let key = base_sha.clone().map(|b| (b, sha.clone()));
                if let Some(hit) = key
                    .as_ref()
                    .and_then(|k| crate::lock(&state.git.vs_main).get(k).copied())
                {
                    vs_main[i] = Some(hit);
                    continue;
                }
                let range = format!("{base}...{sha}");
                if let Ok(out) = run_git(
                    &git.path,
                    &state.git.procs,
                    &repo.toplevel,
                    &["rev-list", "--left-right", "--count", &range],
                    1024,
                )
                .await
                {
                    if out.success {
                        vs_main[i] = parse_left_right(&String::from_utf8_lossy(&out.stdout));
                        if let (Some(k), Some(counts)) = (key, vs_main[i]) {
                            let mut cache = crate::lock(&state.git.vs_main);
                            if cache.len() >= super::service::VS_MAIN_CAP {
                                cache.clear();
                            }
                            cache.insert(k, counts);
                        }
                    }
                }
            }
            // "merged" means the branch had commits of its own and the main
            // branch now holds them all. A brand-new branch (nothing on it
            // yet) is also "0 ahead", but it is not merged: it says nothing.
            let mut merged: Vec<Option<bool>> = Vec::with_capacity(list.len());
            for (w, counts) in list.iter().zip(&vs_main) {
                let managed = w.path.starts_with(&managed_root) && w.path != repo.toplevel;
                let value = match counts.filter(|_| managed) {
                    None => None,
                    Some((_, ahead)) if ahead > 0 => Some(false),
                    Some(_) => Some(
                        !branch_is_new(
                            &state,
                            &git.path,
                            &repo.toplevel,
                            w.branch.as_deref(),
                            w.sha.as_deref(),
                        )
                        .await,
                    ),
                };
                merged.push(value);
            }
            let items: Vec<serde_json::Value> = list
                .iter()
                .zip(merged)
                .zip(vs_main)
                .map(|((w, merged), counts)| {
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
                        // Additive: a managed worktree whose HEAD the main
                        // checkout's branch already contains (null = not asked).
                        "merged": merged,
                        // Additive: commits this worktree has that the main
                        // checkout's branch lacks, and the reverse (null for
                        // the main checkout itself, or not asked).
                        "ahead_of_main": counts.map(|(_, ahead)| ahead),
                        "behind_main": counts.map(|(behind, _)| behind),
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
    /// One of the workspace's repositories (see [`pick_repo`]).
    #[serde(default)]
    repo: Option<String>,
    /// A revision (branch, tag, sha, `HEAD~1`): validated with
    /// `check-ref-format` and resolved with `rev-parse --verify`. With the
    /// default mode it is the working tree against that revision; with
    /// `mode=commit` it is that commit against its first parent.
    #[serde(default)]
    rev: Option<String>,
    /// `mode=commit` on a renamed file: its path before the commit.
    #[serde(default)]
    orig: Option<String>,
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
    let picked = match pick_repo(&state, &git.path, &ws, q.repo.as_deref()).await {
        Ok(outcome) => outcome,
        Err(refusal) => return refusal,
    };
    // No `repo`: the innermost known repository holding the path (a file
    // in a nested repository diffs against ITS history), else the primary.
    let picked = match (q.repo.as_deref(), picked) {
        (None, outcome) => match state.git.innermost(&q.workspace_id, Path::new(&q.path)) {
            Some((inner, _)) => ProbeOutcome::Repo(inner),
            None => outcome,
        },
        (Some(_), outcome) => outcome,
    };
    let Some(repo) = picked.into_repo() else {
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
    let rev = q.rev.as_deref().map(str::trim).filter(|r| !r.is_empty());
    let (a_spec, a_label, b_spec, b_label): (Option<String>, String, Option<String>, String) =
        match rev {
            Some(rev) => {
                let Some(sha) =
                    super::rev::resolve_commit(&git.path, &state.git.procs, &repo.toplevel, rev)
                        .await
                else {
                    return bad_request(&format!("unknown revision {rev:?}"));
                };
                let short: String = sha.chars().take(7).collect();
                if mode == "commit" {
                    // The commit against its first parent (a root commit
                    // against nothing: the whole file is added).
                    let parent_spec = format!("{sha}^");
                    let parent = super::rev::resolve_commit(
                        &git.path,
                        &state.git.procs,
                        &repo.toplevel,
                        &parent_spec,
                    )
                    .await;
                    let before_rel = q
                        .orig
                        .as_deref()
                        .and_then(|o| repo_relative(&repo.toplevel, o))
                        .unwrap_or_else(|| rel.clone());
                    let a_label = parent
                        .as_ref()
                        .map(|p| p.chars().take(7).collect::<String>())
                        .unwrap_or_else(|| "nothing".to_string());
                    (
                        parent.map(|p| format!("{p}:{before_rel}")),
                        a_label,
                        Some(format!("{sha}:{rel}")),
                        short,
                    )
                } else {
                    // The working tree against the revision.
                    let label = if super::anchor::is_sha(rev) {
                        short
                    } else {
                        rev.to_string()
                    };
                    (
                        Some(format!("{sha}:{rel}")),
                        label,
                        None,
                        "working tree".to_string(),
                    )
                }
            }
            None => match mode {
                "staged" => (
                    Some(format!("HEAD:{rel}")),
                    "HEAD".into(),
                    Some(format!(":{rel}")),
                    "staged".into(),
                ),
                "head" => (
                    Some(format!("HEAD:{rel}")),
                    "HEAD".into(),
                    None,
                    "working tree".into(),
                ),
                // "unstaged" (default): index vs working tree.
                _ => (
                    Some(format!(":{rel}")),
                    "index".into(),
                    None,
                    "working tree".into(),
                ),
            },
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
pub(super) fn repo_relative(toplevel: &Path, abs: &str) -> Option<String> {
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
    fn parses_left_right_counts() {
        assert_eq!(parse_left_right("3\t2\n"), Some((3, 2)));
        assert_eq!(parse_left_right("0 0"), Some((0, 0)));
        assert_eq!(parse_left_right("garbage"), None);
    }

    #[test]
    fn parses_branch_list_with_tracking() {
        let raw = "main\u{1f}1700000000\u{1f}origin/main\u{1f}[ahead 2, behind 1]\u{1f}*\n\
                   feat/x\u{1f}1690000000\u{1f}\u{1f}\u{1f} \n\
                   old\u{1f}1600000000\u{1f}origin/old\u{1f}[gone]\u{1f} \n";
        let list = parse_branches(raw.as_bytes());
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].name, "main");
        assert!(list[0].current);
        assert_eq!((list[0].ahead, list[0].behind), (2, 1));
        assert_eq!(list[0].upstream.as_deref(), Some("origin/main"));
        assert_eq!(list[1].name, "feat/x");
        assert_eq!(list[1].upstream, None);
        assert!(!list[1].current);
        assert!(list[2].gone);
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

/// Parse `rev-list --left-right --count A...B` ("<left>\t<right>").
/// Whether `branch` has never moved since it was created — its reflog holds
/// only the creation entry — at `sha`. Cached by (branch, sha); an unknown
/// answer (no reflog, a failed run) reads as not new, the old behavior.
async fn branch_is_new(
    state: &AppState,
    git: &Path,
    dir: &Path,
    branch: Option<&str>,
    sha: Option<&str>,
) -> bool {
    let (Some(branch), Some(sha)) = (branch, sha) else {
        return false;
    };
    let key = (branch.to_string(), sha.to_string());
    if let Some(hit) = crate::lock(&state.git.fresh_branches).get(&key).copied() {
        return hit;
    }
    let refname = format!("refs/heads/{branch}");
    let answer = match run_git(
        git,
        &state.git.procs,
        dir,
        &["reflog", "show", "-n", "2", "--format=%H", &refname, "--"],
        4096,
    )
    .await
    {
        Ok(out) if out.success => {
            let entries: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            entries.len() == 1 && entries[0] == sha
        }
        _ => false,
    };
    let mut cache = crate::lock(&state.git.fresh_branches);
    if cache.len() >= super::service::VS_MAIN_CAP {
        cache.clear();
    }
    cache.insert(key, answer);
    answer
}

pub(super) fn parse_left_right(text: &str) -> Option<(u64, u64)> {
    let mut it = text.split_whitespace();
    let left = it.next()?.parse().ok()?;
    let right = it.next()?.parse().ok()?;
    Some((left, right))
}

#[derive(Deserialize)]
pub(crate) struct BranchesQuery {
    workspace_id: String,
    #[serde(default)]
    repo: Option<String>,
}

/// One local branch.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct BranchInfo {
    pub(super) name: String,
    /// Committer time of its tip, seconds since the epoch.
    pub(super) time: i64,
    pub(super) upstream: Option<String>,
    pub(super) ahead: u32,
    pub(super) behind: u32,
    /// Its upstream was deleted.
    pub(super) gone: bool,
    /// Checked out in the workspace's own checkout.
    pub(super) current: bool,
}

/// Parse `for-each-ref --format=%(refname:short)%1f%(committerdate:unix)%1f
/// %(upstream:short)%1f%(upstream:track)%1f%(HEAD)`, one branch per line.
pub(super) fn parse_branches(bytes: &[u8]) -> Vec<BranchInfo> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| {
            let mut f = line.split('\u{1f}');
            let name = f.next()?.trim();
            if name.is_empty() {
                return None;
            }
            let time = f.next().unwrap_or("").trim().parse().unwrap_or(0);
            let upstream = Some(f.next().unwrap_or("").trim())
                .filter(|u| !u.is_empty())
                .map(str::to_string);
            let track = f.next().unwrap_or("").trim();
            let current = f.next().unwrap_or("").trim() == "*";
            let mut ahead = 0;
            let mut behind = 0;
            for part in track.trim_matches(|c| c == '[' || c == ']').split(',') {
                let part = part.trim();
                if let Some(n) = part.strip_prefix("ahead ") {
                    ahead = n.trim().parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix("behind ") {
                    behind = n.trim().parse().unwrap_or(0);
                }
            }
            Some(BranchInfo {
                name: name.to_string(),
                time,
                upstream,
                ahead,
                behind,
                gone: track == "[gone]",
                current,
            })
        })
        .collect()
}

/// GET /api/v1/git/branches?workspace_id= — the repository's local
/// branches, most recently committed first (≤100): name, last commit date,
/// upstream and how far ahead/behind it. Read-only: there is no checkout.
pub(crate) async fn branches(
    State(state): State<Arc<AppState>>,
    Query(q): Query<BranchesQuery>,
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
        return Json(json!({"repo": false, "branches": []})).into_response();
    }
    let picked = match pick_repo(&state, &git.path, &ws, q.repo.as_deref()).await {
        Ok(outcome) => outcome,
        Err(refusal) => return refusal,
    };
    let Some(repo) = picked.into_repo() else {
        return Json(json!({"repo": false, "branches": []})).into_response();
    };
    let count = format!("--count={}", MAX_BRANCHES + 1);
    let out = match run_git(
        &git.path,
        &state.git.procs,
        &repo.toplevel,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            &count,
            "--format=%(refname:short)%1f%(committerdate:unix)%1f%(upstream:short)%1f%(upstream:track)%1f%(HEAD)",
            "refs/heads",
        ],
        512 * 1024,
    )
    .await
    {
        Ok(out) if out.success => out,
        Ok(out) => {
            return Json(json!({"repo": true, "branches": [], "error": out.stderr}))
                .into_response()
        }
        Err(err) => {
            return Json(json!({"repo": true, "branches": [], "error": err.to_string()}))
                .into_response()
        }
    };
    let mut list = parse_branches(&out.stdout);
    let truncated = list.len() > MAX_BRANCHES;
    list.truncate(MAX_BRANCHES);
    let items: Vec<serde_json::Value> = list
        .iter()
        .map(|b| {
            json!({
                "name": b.name,
                "time": b.time,
                "upstream": b.upstream,
                "ahead": b.ahead,
                "behind": b.behind,
                "gone": b.gone,
                "current": b.current,
            })
        })
        .collect();
    Json(json!({"repo": true, "branches": items, "truncated": truncated})).into_response()
}
