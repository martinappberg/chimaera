//! History, read-only: a repository's log (optionally one file's, following
//! renames), one commit's message and files with line counts, and "Changes on
//! this branch" — everything since a branch left its base, uncommitted work
//! included. Every revision a client names is validated (`rev::`) before git
//! sees it; every answer is bounded. Chimaera shows history and never checks
//! out, reverts or resets.
//!
//! `GET /git/log?path=` (≤50) and `rev=` on `GET /git/diff` are the same routes
//! the plugin platform plan uses for the editor's change bars.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

use super::http::{bad_request, git_too_old, pick_repo, repo_relative, view_json};
use super::parse::RepoInfo;
use super::rev::resolve_commit;
use super::service::{configured_git, run_git};

/// Commits per log page.
pub(super) const MAX_LOG: usize = 50;
/// Files listed for one commit or one branch's changes.
const MAX_FILES: usize = 500;
/// A commit message is shown whole up to this; longer is cut.
const MAX_MESSAGE: usize = 16 * 1024;
/// git's well-known empty tree: the "parent" of a root commit.
pub(super) const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn not_found(message: &str) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": message}))).into_response()
}

/// Resolve the workspace, git and the repository a history request names.
async fn open_repo(
    state: &Arc<AppState>,
    ws_id: &str,
    repo: Option<&str>,
    scope: Option<&crate::workspace_scope::Scope>,
) -> Result<(std::path::PathBuf, RepoInfo), Response> {
    let Some(ws) = crate::lock(&state.workspaces).get(ws_id) else {
        return Err(not_found("unknown workspace"));
    };
    let git = state.git.resolve_git(configured_git(state)).await;
    if !git.adequate {
        return Err(git_too_old(&git));
    }
    let picked = pick_repo(state, &git.path, &ws, repo, scope).await?;
    let Some(info) = picked.into_repo() else {
        return Err(bad_request("not a git repository"));
    };
    Ok((git.path.clone(), info))
}

/// One commit of a log page.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct LogCommit {
    pub(super) sha: String,
    pub(super) parents: Vec<String>,
    pub(super) author: String,
    /// Author time, seconds since the epoch.
    pub(super) time: i64,
    pub(super) subject: String,
    /// The message after the subject, cut at [`MAX_BODY`].
    pub(super) body: String,
}

/// A history row's body (its hover) is cut here.
const MAX_BODY: usize = 1024;

/// The log format: sha, parents, author name, author time, subject, body —
/// unit-separated, one record per NUL.
const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%at%x1f%s%x1f%b";

pub(super) fn parse_log(bytes: &[u8]) -> Vec<LogCommit> {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter_map(|record| {
            let record = record.trim_start_matches('\n');
            if record.is_empty() {
                return None;
            }
            let mut f = record.splitn(6, '\u{1f}');
            let sha = f.next()?.trim().to_string();
            if !super::anchor::is_sha(&sha) {
                return None;
            }
            let parents = f
                .next()
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let author = f.next().unwrap_or("").to_string();
            let time = f.next().unwrap_or("0").trim().parse().unwrap_or(0);
            let subject = f.next().unwrap_or("").trim_end().to_string();
            let mut body = f.next().unwrap_or("").trim().to_string();
            if body.len() > MAX_BODY {
                let mut cut = MAX_BODY;
                while !body.is_char_boundary(cut) {
                    cut -= 1;
                }
                body.truncate(cut);
                body.push('…');
            }
            Some(LogCommit {
                sha,
                parents,
                author,
                time,
                subject,
                body,
            })
        })
        .collect()
}

#[derive(Deserialize)]
pub(crate) struct LogQuery {
    workspace_id: String,
    #[serde(default)]
    repo: Option<String>,
    /// One file's history (followed across renames).
    #[serde(default)]
    path: Option<String>,
    /// Start from this revision (a branch, a sha); HEAD when absent.
    #[serde(default)]
    rev: Option<String>,
    #[serde(default)]
    skip: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

/// GET /api/v1/git/log?workspace_id=&repo=&path=&rev=&skip=&limit= — at most
/// 50 commits a page, newest first: sha, parents, author, date, subject.
/// With `path`, one file's history, followed across renames. `has_more`
/// says another page exists. An unborn branch answers an empty page.
pub(crate) async fn log(
    State(state): State<Arc<AppState>>,
    scope: Option<Extension<crate::workspace_scope::Scope>>,
    Query(q): Query<LogQuery>,
) -> Response {
    let (git, repo) = match open_repo(
        &state,
        &q.workspace_id,
        q.repo.as_deref(),
        scope.as_ref().map(|s| &s.0),
    )
    .await
    {
        Ok(v) => v,
        Err(r) => return r,
    };
    let limit = q.limit.unwrap_or(MAX_LOG).clamp(1, MAX_LOG);
    let skip = q.skip.unwrap_or(0).min(1_000_000);
    let start = match q.rev.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(rev) => match resolve_commit(&git, &state.git.procs, &repo.toplevel, rev).await {
            Some(sha) => Some(sha),
            None => return bad_request(&format!("unknown revision {rev:?}")),
        },
        None => None,
    };
    let rel = match q.path.as_deref() {
        Some(p) => match repo_relative(&repo.toplevel, p) {
            Some(rel) if !rel.is_empty() => Some(rel),
            _ => return bad_request("path is not inside the repository"),
        },
        None => None,
    };
    let max = format!("--max-count={}", limit + 1);
    let skip_arg = format!("--skip={skip}");
    let mut args: Vec<&str> = vec!["log", "--no-color", "-z", LOG_FORMAT, &max, &skip_arg];
    if let Some(sha) = start.as_deref() {
        args.push(sha);
    } else {
        args.push("HEAD");
    }
    if let Some(rel) = rel.as_deref() {
        args.push("--follow");
        args.push("--");
        args.push(rel);
    }
    let out = match run_git(&git, &state.git.procs, &repo.toplevel, &args, 1024 * 1024).await {
        Ok(out) => out,
        Err(err) => return bad_request(&err.to_string()),
    };
    if !out.success {
        // No commits yet: an empty history, not an error.
        if out.stderr.contains("does not have any commits")
            || out.stderr.contains("unknown revision")
        {
            return view_json(
                &state,
                scope.as_ref().map(|s| &s.0),
                "/git/log",
                json!({
                    "toplevel": repo.toplevel.to_string_lossy(),
                    "commits": [],
                    "has_more": false,
                    "unborn": true,
                }),
            )
            .into_response();
        }
        return bad_request(&out.stderr);
    }
    let mut commits = parse_log(&out.stdout);
    let has_more = commits.len() > limit;
    commits.truncate(limit);
    let items: Vec<serde_json::Value> = commits
        .iter()
        .map(|c| {
            json!({
                "sha": c.sha,
                "parents": c.parents,
                "author": c.author,
                "time": c.time,
                "subject": c.subject,
                "body": c.body,
            })
        })
        .collect();
    view_json(
        &state,
        scope.as_ref().map(|s| &s.0),
        "/git/log",
        json!({
            "toplevel": repo.toplevel.to_string_lossy(),
            "path": rel,
            "commits": items,
            "has_more": has_more,
            "skip": skip,
        }),
    )
    .into_response()
}

/// One changed file between two trees.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ChangedFile {
    pub(super) rel: String,
    pub(super) orig_rel: Option<String>,
    /// A (added), M, D, R (renamed), C (copied), T (type change), U …
    pub(super) status: char,
    pub(super) added: Option<u64>,
    pub(super) removed: Option<u64>,
    pub(super) binary: bool,
}

/// Parse `diff -z --name-status` output: `S\0path\0` or `R100\0old\0new\0`.
pub(super) fn parse_name_status(bytes: &[u8]) -> Vec<ChangedFile> {
    let text = String::from_utf8_lossy(bytes);
    let tokens: Vec<&str> = text.split('\0').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let code = tokens[i];
        i += 1;
        let Some(status) = code.chars().next() else {
            continue;
        };
        if !status.is_ascii_uppercase() {
            continue;
        }
        if status == 'R' || status == 'C' {
            let (Some(old), Some(new)) = (tokens.get(i), tokens.get(i + 1)) else {
                break;
            };
            i += 2;
            out.push(ChangedFile {
                rel: new.to_string(),
                orig_rel: Some(old.to_string()),
                status,
                ..Default::default()
            });
        } else {
            let Some(path) = tokens.get(i) else { break };
            i += 1;
            out.push(ChangedFile {
                rel: path.to_string(),
                status,
                ..Default::default()
            });
        }
    }
    out
}

/// Parse `diff -z --numstat` into (path, added, removed) — `-`/`-` for a
/// binary file; a rename is `a\tr\t\0old\0new\0`.
pub(super) fn parse_numstat(bytes: &[u8]) -> Vec<(String, Option<u64>, Option<u64>)> {
    let text = String::from_utf8_lossy(bytes);
    let tokens: Vec<&str> = text.split('\0').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        i += 1;
        if tok.is_empty() {
            continue;
        }
        let mut f = tok.splitn(3, '\t');
        let (Some(a), Some(r), Some(path)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let path = if path.is_empty() {
            // Rename: the old and new paths follow as their own fields.
            let new = tokens.get(i + 1).copied().unwrap_or("");
            i += 2;
            new.to_string()
        } else {
            path.to_string()
        };
        out.push((path, a.parse().ok(), r.parse().ok()));
    }
    out
}

/// The files changed between `from` and `to` (a tree-ish, or `None` for the
/// working tree), with line counts. `-M` finds renames.
async fn changed_files(
    state: &AppState,
    git: &Path,
    repo: &RepoInfo,
    from: &str,
    to: Option<&str>,
) -> anyhow::Result<(Vec<ChangedFile>, bool)> {
    let mut status_args: Vec<&str> = vec!["diff", "--no-color", "-z", "-M", "--name-status", from];
    let mut num_args: Vec<&str> = vec!["diff", "--no-color", "-z", "-M", "--numstat", from];
    if let Some(to) = to {
        status_args.push(to);
        num_args.push(to);
    }
    let names = run_git(
        git,
        &state.git.procs,
        &repo.toplevel,
        &status_args,
        1024 * 1024,
    )
    .await?;
    if !names.success {
        anyhow::bail!("{}", names.stderr);
    }
    let nums = run_git(
        git,
        &state.git.procs,
        &repo.toplevel,
        &num_args,
        1024 * 1024,
    )
    .await?;
    let mut files = parse_name_status(&names.stdout);
    let truncated = files.len() > MAX_FILES || names.truncated;
    files.truncate(MAX_FILES);
    if nums.success {
        let counts: std::collections::HashMap<String, (Option<u64>, Option<u64>)> =
            parse_numstat(&nums.stdout)
                .into_iter()
                .map(|(p, a, r)| (p, (a, r)))
                .collect();
        for f in &mut files {
            if let Some((a, r)) = counts.get(&f.rel) {
                f.added = *a;
                f.removed = *r;
                f.binary = a.is_none() && r.is_none();
            }
        }
    }
    Ok((files, truncated))
}

fn files_json(repo: &RepoInfo, files: &[ChangedFile]) -> Vec<serde_json::Value> {
    files
        .iter()
        .map(|f| {
            json!({
                "path": repo.toplevel.join(&f.rel).to_string_lossy(),
                "rel": f.rel,
                "orig": f.orig_rel.as_ref().map(|o| repo.toplevel.join(o).to_string_lossy().into_owned()),
                "orig_rel": f.orig_rel,
                "status": f.status.to_string(),
                "added": f.added,
                "removed": f.removed,
                "binary": f.binary,
            })
        })
        .collect()
}

#[derive(Deserialize)]
pub(crate) struct ShowQuery {
    workspace_id: String,
    #[serde(default)]
    repo: Option<String>,
    rev: String,
}

/// GET /api/v1/git/show?workspace_id=&repo=&rev= — one commit: its message
/// (subject and body), author and date, parents, and its files (against its
/// first parent; a root commit against nothing) with added/removed line
/// counts. Each file's diff opens with `GET /git/diff?rev=<sha>&mode=commit`.
pub(crate) async fn show(
    State(state): State<Arc<AppState>>,
    scope: Option<Extension<crate::workspace_scope::Scope>>,
    Query(q): Query<ShowQuery>,
) -> Response {
    let (git, repo) = match open_repo(
        &state,
        &q.workspace_id,
        q.repo.as_deref(),
        scope.as_ref().map(|s| &s.0),
    )
    .await
    {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(sha) = resolve_commit(&git, &state.git.procs, &repo.toplevel, q.rev.trim()).await
    else {
        return bad_request(&format!("unknown revision {:?}", q.rev));
    };
    let meta = match run_git(
        &git,
        &state.git.procs,
        &repo.toplevel,
        &[
            "show",
            "--no-color",
            "--no-patch",
            "--format=%H%x1f%P%x1f%an%x1f%at%x1f%cn%x1f%ct%x1f%B",
            &sha,
        ],
        MAX_MESSAGE + 4096,
    )
    .await
    {
        Ok(out) if out.success => out,
        Ok(out) => return bad_request(&out.stderr),
        Err(err) => return bad_request(&err.to_string()),
    };
    let text = String::from_utf8_lossy(&meta.stdout).into_owned();
    let mut f = text.splitn(7, '\u{1f}');
    let full = f.next().unwrap_or("").trim().to_string();
    let parents: Vec<String> = f
        .next()
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let author = f.next().unwrap_or("").to_string();
    let time: i64 = f.next().unwrap_or("0").trim().parse().unwrap_or(0);
    let committer = f.next().unwrap_or("").to_string();
    let commit_time: i64 = f.next().unwrap_or("0").trim().parse().unwrap_or(0);
    let message = f.next().unwrap_or("").trim_end().to_string();
    let (subject, body) = match message.split_once('\n') {
        Some((s, b)) => (s.trim().to_string(), b.trim().to_string()),
        None => (message.trim().to_string(), String::new()),
    };
    let from = parents
        .first()
        .cloned()
        .unwrap_or_else(|| EMPTY_TREE.to_string());
    let (files, truncated) = match changed_files(&state, &git, &repo, &from, Some(&full)).await {
        Ok(v) => v,
        Err(err) => return bad_request(&err.to_string()),
    };
    view_json(
        &state,
        scope.as_ref().map(|s| &s.0),
        "/git/show",
        json!({
            "toplevel": repo.toplevel.to_string_lossy(),
            "sha": full,
            "parents": parents,
            "author": author,
            "time": time,
            "committer": committer,
            "commit_time": commit_time,
            "subject": subject,
            "body": body,
            "message_truncated": meta.truncated,
            "files": files_json(&repo, &files),
            "truncated": truncated,
        }),
    )
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct CompareQuery {
    workspace_id: String,
    /// The branch's checkout (a worktree of one of the workspace's
    /// repositories, or the repository itself).
    #[serde(default)]
    repo: Option<String>,
    /// What the branch left from; by default the main checkout's branch for a
    /// linked worktree, else the branch's upstream.
    #[serde(default)]
    base: Option<String>,
}

/// GET /api/v1/git/compare?workspace_id=&repo=&base= — "Changes on this
/// branch": everything since the branch left its base — the merge-base diff
/// plus uncommitted work (staged, unstaged and untracked) — as files with
/// line counts, and how many commits the branch has on top of the base. Each
/// file's diff opens with `GET /git/diff?rev=<merge_base>&repo=<checkout>`.
pub(crate) async fn compare(
    State(state): State<Arc<AppState>>,
    scope: Option<Extension<crate::workspace_scope::Scope>>,
    Query(q): Query<CompareQuery>,
) -> Response {
    let (git, repo) = match open_repo(
        &state,
        &q.workspace_id,
        q.repo.as_deref(),
        scope.as_ref().map(|s| &s.0),
    )
    .await
    {
        Ok(v) => v,
        Err(r) => return r,
    };
    let procs = &state.git.procs;
    let top = &repo.toplevel;
    // The base: asked for, else the main checkout's branch (a linked
    // worktree), else the upstream; a branch with neither has only its
    // uncommitted work to show.
    let requested = q.base.as_deref().map(str::trim).filter(|b| !b.is_empty());
    let (base_label, base_sha) = match requested {
        Some(base) => match resolve_commit(&git, procs, top, base).await {
            Some(sha) => (Some(base.to_string()), Some(sha)),
            None => return bad_request(&format!("unknown base {base:?}")),
        },
        None => {
            let main = if repo.git_dir != repo.common_dir {
                super::worktree::main_branch_ref(&state, &git, &repo).await
            } else {
                None
            };
            let upstream = match run_git(
                &git,
                procs,
                top,
                &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
                4096,
            )
            .await
            {
                Ok(out) if out.success => {
                    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
                }
                _ => None,
            };
            let label = main
                .map(|m| m.trim_start_matches("refs/heads/").to_string())
                .or(upstream);
            match label {
                Some(l) => {
                    let sha = resolve_commit(&git, procs, top, &l).await;
                    (Some(l), sha)
                }
                None => (None, None),
            }
        }
    };
    let head = resolve_commit(&git, procs, top, "HEAD").await;
    // Where the branch left its base.
    let merge_base = match (&base_sha, &head) {
        (Some(base), Some(head)) => {
            match run_git(&git, procs, top, &["merge-base", base, head], 1024).await {
                Ok(out) if out.success => {
                    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
                        .filter(|s| super::anchor::is_sha(s))
                }
                _ => None,
            }
        }
        _ => None,
    };
    // Commits on the branch since then.
    let ahead = match (&merge_base, &head) {
        (Some(mb), Some(head)) => {
            let range = format!("{mb}..{head}");
            match run_git(&git, procs, top, &["rev-list", "--count", &range], 1024).await {
                Ok(out) if out.success => String::from_utf8_lossy(&out.stdout).trim().parse().ok(),
                _ => None,
            }
        }
        _ => Some(0u64),
    };
    // The files: the working tree against the merge base (committed +
    // staged + unstaged), or against HEAD when there is no base.
    let from = merge_base
        .clone()
        .or_else(|| head.clone())
        .unwrap_or_else(|| EMPTY_TREE.to_string());
    let (mut files, mut truncated) = match changed_files(&state, &git, &repo, &from, None).await {
        Ok(v) => v,
        Err(err) => return bad_request(&err.to_string()),
    };
    // Untracked files are uncommitted work too.
    if let Ok(out) = run_git(
        &git,
        procs,
        top,
        &["ls-files", "-z", "--others", "--exclude-standard"],
        1024 * 1024,
    )
    .await
    {
        if out.success {
            for rel in String::from_utf8_lossy(&out.stdout)
                .split('\0')
                .filter(|s| !s.is_empty())
            {
                if files.len() >= MAX_FILES {
                    truncated = true;
                    break;
                }
                files.push(ChangedFile {
                    rel: rel.to_string(),
                    status: '?',
                    ..Default::default()
                });
            }
        }
    }
    view_json(
        &state,
        scope.as_ref().map(|s| &s.0),
        "/git/compare",
        json!({
            "toplevel": top.to_string_lossy(),
            "base": base_label,
            "merge_base": merge_base,
            "diff_from": from,
            "head": head,
            "ahead": ahead,
            "files": files_json(&repo, &files),
            "truncated": truncated,
        }),
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_log_records() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let raw = format!(
            "{a}\u{1f}{b}\u{1f}Ada Lovelace\u{1f}1700000000\u{1f}fix: rounding\u{1f}Why it was off.\n\0\n{b}\u{1f}\u{1f}Ada\u{1f}1690000000\u{1f}initial\u{1f}\0"
        );
        let log = parse_log(raw.as_bytes());
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].parents, vec![b.clone()]);
        assert_eq!(log[0].author, "Ada Lovelace");
        assert_eq!(log[0].subject, "fix: rounding");
        assert_eq!(log[0].body, "Why it was off.");
        assert!(log[1].parents.is_empty(), "a root commit");
    }

    #[test]
    fn parses_name_status_with_renames() {
        let raw = "M\0src/a.rs\0R087\0old/b.rs\0new/b.rs\0A\0c.txt\0D\0gone.md\0";
        let files = parse_name_status(raw.as_bytes());
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].status, 'M');
        assert_eq!(files[1].status, 'R');
        assert_eq!(files[1].rel, "new/b.rs");
        assert_eq!(files[1].orig_rel.as_deref(), Some("old/b.rs"));
        assert_eq!(files[3].status, 'D');
    }

    #[test]
    fn parses_numstat_including_binary_and_renames() {
        let raw = "3\t1\tsrc/a.rs\0-\t-\timg.png\0".to_string() + "2\t2\t\0old/b.rs\0new/b.rs\0";
        let rows = parse_numstat(raw.as_bytes());
        assert_eq!(rows[0], ("src/a.rs".to_string(), Some(3), Some(1)));
        assert_eq!(rows[1], ("img.png".to_string(), None, None));
        assert_eq!(rows[2], ("new/b.rs".to_string(), Some(2), Some(2)));
    }
}
