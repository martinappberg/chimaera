//! Where a Claude conversation's transcript lives — the ONE lookup every
//! resume check goes through (chat resurrection, the view switch, journal
//! seeding, the ledger's PTY resume, Recents, history's reopen check, and the
//! resume catalog).
//!
//! Claude files a conversation under the project directory of the folder it
//! is working in (`~/.claude/projects/<encode_cwd(cwd)>/<id>.jsonl`). That
//! is NOT always the workspace root:
//!
//! - a session Chimaera started in a worktree (`CreateSession.worktree`/`cwd`,
//!   the Mastermind's `spawn_agent {branch}`) files under the worktree's own
//!   directory — the session's real cwd, which the ledger and Recents carry;
//! - an agent that calls `EnterWorktree` mid-conversation moves its
//!   transcript to `<root>/.claude/worktrees/<name>`'s directory. Claude
//!   reports the file it writes on every hook (`transcript_path`); the ledger
//!   and Recents record that path and it wins over any derived location.
//!
//! What it must find is checked in that order: the recorded path (when its
//! stem is the id and it sits inside the store), the project dir of each
//! known cwd, then — for conversations that predate the recorded path — the
//! project dirs of the cwds' own Claude worktrees (`<cwd>/.claude/worktrees/*`,
//! encoded `<cwd-key>--claude-worktrees-*`). The prefix includes
//! `--claude-worktrees-` because the encoding folds `/` into `-`: a bare
//! `<cwd-key>` prefix would match a sibling repository such as `proj-pro`.
//! That last step is one bounded `read_dir` of the store, never a recursive
//! walk, and only an exact `<id>.jsonl` counts.
//!
//! Every function here touches the filesystem: call it off the reactor
//! (`spawn_blocking`, or [`find`]).
//!
//! Claude itself resumes by id from any folder of the same repository, even
//! after the worktree the conversation was filed under is gone (verified
//! with claude 2.1.300: `--resume <id>` from the repository root, no
//! `--fork-session`, keeps appending to the worktree directory's file), so
//! only Chimaera's own existence checks needed to know where to look.

use std::path::{Path, PathBuf};

use crate::launcher::encode_cwd;

/// Store entries one worktree-directory scan reads before giving up — the
/// store holds one directory per folder claude ever ran in.
const SCAN_ENTRIES_MAX: usize = 20_000;
/// Worktree project directories one lookup considers.
const WORKTREE_DIRS_MAX: usize = 64;

/// The part of an encoded project-dir name that marks Claude's own
/// worktrees of the folder before it (`<cwd>/.claude/worktrees/<name>`).
const CLAUDE_WORKTREES: &str = "--claude-worktrees-";

/// A native id that can only name a file directly inside one project dir.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id != "." && id != ".." && !id.contains(['/', '\\', '\0'])
}

/// The conventional transcript location for `id` when claude ran in `cwd`.
pub(crate) fn derived(store: &Path, cwd: &Path, id: &str) -> PathBuf {
    store.join(encode_cwd(cwd)).join(format!("{id}.jsonl"))
}

/// A recorded transcript path names `id` — a hook's claim is only trusted
/// for the conversation it is about. It need not sit under the default
/// store: a user's `CLAUDE_CONFIG_DIR` moves the whole store, and then the
/// recorded path is the only one that is right.
fn recorded_ok(path: &Path, id: &str) -> bool {
    path.file_stem().is_some_and(|s| s == id)
        && path.extension().is_some_and(|e| e == "jsonl")
        && path.is_absolute()
        && !path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
}

/// The project directories of `cwds`' own Claude worktrees, from one bounded
/// pass over the store. Missing store = none.
pub(crate) fn claude_worktree_dirs(store: &Path, cwds: &[PathBuf]) -> Vec<PathBuf> {
    let prefixes: Vec<String> = cwds
        .iter()
        .map(|c| format!("{}{CLAUDE_WORKTREES}", encode_cwd(c)))
        .collect();
    if prefixes.is_empty() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(store) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.take(SCAN_ENTRIES_MAX).flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if prefixes
            .iter()
            .any(|p| name.len() > p.len() && name.starts_with(p.as_str()))
        {
            out.push(entry.path());
            if out.len() >= WORKTREE_DIRS_MAX {
                break;
            }
        }
    }
    out
}

/// Find `id`'s transcript. `recorded` are paths claude reported (hooks, kept
/// in the ledger/Recents/history); `cwds` are the folders the conversation
/// is known to have run in, most specific first (the session's own cwd, then
/// the workspace root). Blocking.
pub(crate) fn find_blocking(
    store: &Path,
    id: &str,
    recorded: &[PathBuf],
    cwds: &[PathBuf],
) -> Option<PathBuf> {
    if let Some(path) = find_known(store, id, recorded, cwds) {
        return Some(path);
    }
    if !valid_id(id) {
        return None;
    }
    let file = format!("{id}.jsonl");
    claude_worktree_dirs(store, cwds)
        .into_iter()
        .map(|dir| dir.join(&file))
        .find(|p| p.is_file())
}

/// [`find_blocking`] without the worktree-directory scan: a stat or a few,
/// for per-row checks that pair it with one [`worktree_index`].
pub(crate) fn find_known(
    store: &Path,
    id: &str,
    recorded: &[PathBuf],
    cwds: &[PathBuf],
) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    if let Some(path) = recorded.iter().find(|p| recorded_ok(p, id) && p.is_file()) {
        return Some(path.clone());
    }
    let mut seen: Vec<&Path> = Vec::with_capacity(cwds.len());
    for cwd in cwds {
        if cwd.as_os_str().is_empty() || seen.contains(&cwd.as_path()) {
            continue;
        }
        seen.push(cwd);
        let path = derived(store, cwd, id);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// [`find_blocking`] off the reactor.
pub(crate) async fn find(
    store: PathBuf,
    id: String,
    recorded: Vec<PathBuf>,
    cwds: Vec<PathBuf>,
) -> Option<PathBuf> {
    tokio::task::spawn_blocking(move || find_blocking(&store, &id, &recorded, &cwds))
        .await
        .ok()
        .flatten()
}

/// The transcript paths Chimaera has seen claude report for `id`: a live
/// agent record's (hooks) and its Recents rows'. No disk access.
pub(crate) fn recorded_for(state: &crate::AppState, id: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = crate::lock(&state.agents)
        .values()
        .filter_map(|r| r.transcript_path.clone())
        .filter(|p| p.file_stem().is_some_and(|s| s == id))
        .collect();
    out.extend(crate::lock(&state.recents).recorded_transcripts(id));
    out
}

/// Every id with a transcript in one of the workspace roots' Claude worktree
/// directories, mapped to its file: a one-shot index for checks that would
/// otherwise scan per row (history's reopen column). Bounded like the scan.
pub(crate) fn worktree_index(
    store: &Path,
    cwds: &[PathBuf],
) -> std::collections::HashMap<String, PathBuf> {
    const FILES_MAX: usize = 5_000;
    let mut index = std::collections::HashMap::new();
    for dir in claude_worktree_dirs(store, cwds) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.take(FILES_MAX).flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "jsonl") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    index.entry(stem.to_string()).or_insert(path);
                }
            }
            if index.len() >= FILES_MAX {
                return index;
            }
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-claude-store-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(dir: &Path, id: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{id}.jsonl"));
        std::fs::write(&path, "{}\n").unwrap();
        path
    }

    #[test]
    fn finds_the_session_cwd_before_the_root() {
        let base = scratch("cwd");
        let store = base.join("projects");
        let root = PathBuf::from("/w/proj");
        let wt = PathBuf::from("/home/u/.chimaera/worktrees/proj-1/chimaera/claude-ab12cd");
        let at_wt = put(&store.join(encode_cwd(&wt)), "c1");
        assert_eq!(
            find_blocking(&store, "c1", &[], &[wt.clone(), root.clone()]),
            Some(at_wt)
        );
        // Only the root known: the managed worktree dir is not guessed.
        assert_eq!(
            find_blocking(&store, "c1", &[], std::slice::from_ref(&root)),
            None
        );
        let at_root = put(&store.join(encode_cwd(&root)), "c2");
        assert_eq!(find_blocking(&store, "c2", &[], &[wt, root]), Some(at_root));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_recorded_path_wins_but_only_for_its_own_id() {
        let base = scratch("rec");
        let store = base.join("projects");
        let root = PathBuf::from("/w/proj");
        let elsewhere = put(&store.join("-somewhere-else"), "c1");
        assert_eq!(
            find_blocking(
                &store,
                "c1",
                std::slice::from_ref(&elsewhere),
                std::slice::from_ref(&root)
            ),
            Some(elsewhere.clone())
        );
        // Another id's path proves nothing about this one.
        assert_eq!(
            find_blocking(
                &store,
                "c9",
                std::slice::from_ref(&elsewhere),
                std::slice::from_ref(&root)
            ),
            None
        );
        // A relative claim: ignored.
        let outside = PathBuf::from("loose/c3.jsonl");
        assert_eq!(find_blocking(&store, "c3", &[outside], &[root]), None);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn claude_worktree_dirs_of_the_root_only() {
        let base = scratch("wt");
        let store = base.join("projects");
        let root = PathBuf::from("/w/proj");
        let entered = PathBuf::from("/w/proj/.claude/worktrees/fix-it");
        let file = put(&store.join(encode_cwd(&entered)), "c1");
        // A sibling repository whose name extends the root's: never matched.
        let sibling = PathBuf::from("/w/proj-pro/.claude/worktrees/fix-it");
        put(&store.join(encode_cwd(&sibling)), "c2");
        assert_eq!(
            find_blocking(&store, "c1", &[], std::slice::from_ref(&root)),
            Some(file)
        );
        assert_eq!(
            find_blocking(&store, "c2", &[], std::slice::from_ref(&root)),
            None
        );
        let index = worktree_index(&store, &[root]);
        assert!(index.contains_key("c1") && !index.contains_key("c2"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn ids_that_could_leave_a_directory_never_resolve() {
        let base = scratch("ids");
        let store = base.join("projects");
        for bad in ["", ".", "..", "../x", "a/b", "a\\b"] {
            assert!(!valid_id(bad), "{bad:?}");
            assert_eq!(
                find_blocking(&store, bad, &[], &[PathBuf::from("/w")]),
                None
            );
        }
        std::fs::remove_dir_all(&base).ok();
    }
}
