use std::path::PathBuf;

use serde_json::json;

/// Cap on status entries materialized; a 50k-change tree truncates honestly
/// instead of shipping a huge list the UI cannot use anyway.
const MAX_STATUS_ENTRIES: usize = 5000;

/// A discovered repository for one workspace.
#[derive(Clone)]
pub(crate) struct RepoInfo {
    /// Working-tree root of THIS workspace's checkout. `--show-toplevel` gives
    /// the right directory whether the workspace opened the main checkout or a
    /// linked worktree (Chimaera itself is developed in a linked worktree).
    pub(super) toplevel: PathBuf,
    /// `--git-common-dir`, shared by every worktree of the repo. The stable
    /// repo identity: it names the managed-worktree directory (see `repo_key`).
    pub(super) common_dir: PathBuf,
}

/// One entry of `git worktree list --porcelain`.
#[derive(Default)]
pub(super) struct WorktreeInfo {
    pub(super) path: PathBuf,
    /// Short HEAD sha.
    pub(super) head: Option<String>,
    /// Short branch name (`refs/heads/x` -> `x`); `None` when detached.
    pub(super) branch: Option<String>,
    pub(super) detached: bool,
    pub(super) bare: bool,
    pub(super) locked: bool,
    pub(super) prunable: bool,
}

/// Parse `git worktree list --porcelain`: blank-line-separated records of
/// `worktree <path>` followed by `HEAD`/`branch`/`detached`/`bare`/`locked`/
/// `prunable` attributes. (Line-oriented, not `-z`: a path containing a newline
/// is pathological and would only mis-split that one record.)
pub(super) fn parse_worktrees(bytes: &[u8]) -> Vec<WorktreeInfo> {
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

/// Parsed `git status --porcelain=v2 --branch` output.
#[derive(Default)]
pub(super) struct StatusData {
    pub(super) branch: Option<String>,
    pub(super) detached: bool,
    pub(super) head: Option<String>,
    pub(super) upstream: Option<String>,
    pub(super) ahead: i64,
    pub(super) behind: i64,
    pub(super) entries: Vec<Entry>,
    pub(super) truncated: bool,
}

/// One changed path.
#[derive(Default, Clone)]
pub(super) struct Entry {
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
    pub(super) fn untracked(rel: String) -> Self {
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
pub(super) fn parse_status(bytes: &[u8], output_truncated: bool) -> StatusData {
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
pub(super) fn hash_status(d: &StatusData) -> u64 {
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

pub(super) fn status_json(
    ws_id: &str,
    epoch: u64,
    repo: &RepoInfo,
    d: &StatusData,
) -> serde_json::Value {
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
}
