//! Session anchors: where a session's repository stood at one moment —
//! `{repo, worktree, branch, head}` with the FULL HEAD sha — and the commits
//! made between two anchors. A small internal API over the service's bounded
//! runner (4 processes, 8 s, capped output): the session tracker captures an
//! anchor at session start, at claude's turn ends and at session end, and
//! the session record (Part 2 of docs/design/git-and-session-history-plan.md)
//! reads them. Read-only: nothing here changes a repository, and nothing
//! requires or prompts for commits — they only show up as HEAD moving.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::AppState;

use super::parse::{read_head_blocking, HeadRef, RepoInfo};
use super::service::{configured_git, probe_repo, run_git, ProbeOutcome};

/// At most this many commits are listed between two anchors (the plan's
/// bound: a session record keeps up to 50 shas).
pub(crate) const MAX_ANCHOR_COMMITS: usize = 50;

/// Where one checkout stood at one moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Anchor {
    /// The repository ([`RepoInfo::repo_path`]): the same for every worktree.
    pub(crate) repo: PathBuf,
    /// The checkout (main or linked worktree) the session was in.
    pub(crate) worktree: PathBuf,
    /// The branch checked out there; `None` when detached or unborn.
    pub(crate) branch: Option<String>,
    /// The full HEAD sha; `None` on an unborn branch (no commits yet).
    pub(crate) head: Option<String>,
    /// When it was captured (ms since the epoch).
    pub(crate) at_ms: u64,
}

impl Anchor {
    pub(crate) fn json(&self) -> serde_json::Value {
        json!({
            "repo": self.repo.to_string_lossy(),
            "worktree": self.worktree.to_string_lossy(),
            "branch": self.branch,
            "head": self.head,
            "at_ms": self.at_ms,
        })
    }
}

/// One commit between two anchors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AnchorCommit {
    pub(crate) sha: String,
    pub(crate) subject: String,
    /// Committer time, seconds since the epoch.
    pub(crate) time: i64,
}

/// The commits reachable from a later anchor but not an earlier one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Between {
    /// Newest first, at most [`MAX_ANCHOR_COMMITS`].
    pub(crate) commits: Vec<AnchorCommit>,
    /// More than [`MAX_ANCHOR_COMMITS`] exist.
    pub(crate) truncated: bool,
    /// The later HEAD does not descend from the earlier one on the same
    /// branch: history was rewritten (a rebase, a reset, an amend).
    pub(crate) rewritten: bool,
}

/// A full or abbreviated object name, hex only — every sha that reaches a
/// git argv here passed this, so nothing flag-shaped ever does.
pub(super) fn is_sha(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Capture the anchor of the checkout containing `dir`. `None` when `dir`
/// is not in a repository, git is missing or too old, or git failed.
pub(crate) async fn capture(state: &AppState, dir: &Path) -> Option<Anchor> {
    let git = state.git.resolve_git(configured_git(state)).await;
    if !git.adequate {
        return None;
    }
    let ProbeOutcome::Repo(repo) = probe_repo(&git.path, &state.git.procs, dir).await else {
        return None;
    };
    capture_in(state, &git.path, &repo).await
}

/// [`capture`] for an already-resolved checkout (the tracker caches the
/// probe per cwd): one `rev-parse` for the full sha, one file read for the
/// branch.
pub(super) async fn capture_in(state: &AppState, git: &Path, repo: &RepoInfo) -> Option<Anchor> {
    let head = run_git(
        git,
        &state.git.procs,
        &repo.toplevel,
        &["rev-parse", "--verify", "--quiet", "HEAD"],
        1024,
    )
    .await
    .ok()
    .filter(|out| out.success)
    .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    .filter(|sha| is_sha(sha));
    let git_dir = repo.git_dir.clone();
    let branch = match tokio::task::spawn_blocking(move || read_head_blocking(&git_dir)).await {
        Ok(HeadRef::Branch(name)) => Some(name),
        _ => None,
    };
    Some(Anchor {
        repo: repo.repo_path(),
        worktree: repo.toplevel.clone(),
        branch,
        head,
        at_ms: crate::session_view::now_ms(),
    })
}

/// The commits made between `old` and `new` (both full shas from anchors),
/// run in `dir` (any checkout of the repository — worktrees share one object
/// store). `None` when git could not answer. Equal shas answer empty.
pub(crate) async fn commits_between(
    state: &AppState,
    dir: &Path,
    old: &str,
    new: &str,
) -> Option<Between> {
    if !is_sha(old) || !is_sha(new) {
        return None;
    }
    if old == new {
        return Some(Between::default());
    }
    let git = state.git.resolve_git(configured_git(state)).await;
    if !git.adequate {
        return None;
    }
    // A HEAD that no longer descends from where it was means the history
    // under it was rewritten; the commits listed are then the new ones.
    let ancestor = run_git(
        &git.path,
        &state.git.procs,
        dir,
        &["merge-base", "--is-ancestor", old, new],
        1024,
    )
    .await
    .ok()?;
    let range = format!("{old}..{new}");
    let limit = format!("--max-count={}", MAX_ANCHOR_COMMITS + 1);
    let out = run_git(
        &git.path,
        &state.git.procs,
        dir,
        &[
            "log",
            "--no-color",
            "-z",
            &limit,
            "--format=%H%x1f%ct%x1f%s",
            &range,
        ],
        256 * 1024,
    )
    .await
    .ok()?;
    if !out.success {
        return None;
    }
    let mut commits = parse_anchor_log(&out.stdout);
    let truncated = commits.len() > MAX_ANCHOR_COMMITS;
    commits.truncate(MAX_ANCHOR_COMMITS);
    Some(Between {
        commits,
        truncated,
        rewritten: !ancestor.success,
    })
}

/// Parse `log -z --format=%H%x1f%ct%x1f%s`: NUL-separated records of three
/// unit-separated fields.
fn parse_anchor_log(bytes: &[u8]) -> Vec<AnchorCommit> {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter_map(|record| {
            let record = record.trim_start_matches('\n');
            let mut fields = record.splitn(3, '\u{1f}');
            let sha = fields.next()?.trim();
            if !is_sha(sha) {
                return None;
            }
            let time = fields.next()?.trim().parse().unwrap_or(0);
            let subject = fields.next().unwrap_or("").trim().to_string();
            Some(AnchorCommit {
                sha: sha.to_string(),
                subject,
                time,
            })
        })
        .collect()
}

impl AnchorCommit {
    pub(crate) fn json(&self) -> serde_json::Value {
        json!({"sha": self.sha, "subject": self.subject, "time": self.time})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_check_refuses_flags_and_names() {
        assert!(is_sha("0123abcd"));
        assert!(is_sha(&"a".repeat(40)));
        assert!(!is_sha("--all"));
        assert!(!is_sha("main"));
        assert!(!is_sha("abc"));
        assert!(!is_sha(&"a".repeat(65)));
    }

    #[test]
    fn parses_anchor_log_records() {
        let sha1 = "a".repeat(40);
        let sha2 = "b".repeat(40);
        let raw = format!(
            "{sha1}\u{1f}1700000000\u{1f}fix: one\0{sha2}\u{1f}1700000100\u{1f}two words\0"
        );
        let commits = parse_anchor_log(raw.as_bytes());
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sha, sha1);
        assert_eq!(commits[0].subject, "fix: one");
        assert_eq!(commits[1].time, 1700000100);
        // A garbage record is skipped, never guessed.
        assert!(parse_anchor_log(b"not a record\0").is_empty());
    }
}
