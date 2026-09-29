//! File-level repository facts that need no git process: where a
//! checkout's git dir is (a `.git` directory, or a `.git` file naming it),
//! and the worktrees under chimaera's managed root. Every function here is
//! blocking filesystem work — callers run it off the reactor — and bounded.

use std::io::Read;
use std::path::{Path, PathBuf};

/// A `.git` file is one short line; anything longer is not one.
const GIT_FILE_CAP: u64 = 4096;

/// Directories visited looking for managed worktrees at daemon start.
const MANAGED_WALK_DIRS: usize = 2000;

/// How deep branch names nest under a repo's managed directory
/// (`feat/x/y` is three levels) before the walk stops looking.
const MANAGED_WALK_DEPTH: usize = 8;

/// The git dir of the checkout at `dir`: `<dir>/.git` when it is a
/// directory, else the path a `.git` file names (`gitdir: <path>`, relative
/// to `dir` when not absolute). Symlinks are not followed.
pub(super) fn gitdir_of_blocking(dir: &Path) -> Option<PathBuf> {
    let dotgit = dir.join(".git");
    let meta = std::fs::symlink_metadata(&dotgit).ok()?;
    if meta.is_dir() {
        return Some(dotgit);
    }
    if !meta.is_file() {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(&dotgit)
        .ok()?
        .take(GIT_FILE_CAP)
        .read_to_string(&mut text)
        .ok()?;
    parse_git_file(&text, dir)
}

/// Parse a `.git` file's `gitdir: <path>` line.
pub(super) fn parse_git_file(text: &str, dir: &Path) -> Option<PathBuf> {
    let line = text.lines().next()?.trim();
    let target = line.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let path = PathBuf::from(target);
    Some(if path.is_absolute() {
        path
    } else {
        super::service::normalize(&dir.join(path))
    })
}

/// Every checkout under the managed root (`<root>/<repo-key>/<branch…>`):
/// directories holding a `.git` FILE (linked worktrees). Bounded by
/// [`MANAGED_WALK_DIRS`] and [`MANAGED_WALK_DEPTH`]; never descends into a
/// checkout or through a symlink.
pub(super) fn managed_worktrees_blocking(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        visited += 1;
        if visited > MANAGED_WALK_DIRS {
            break;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let path = entry.path();
            let is_checkout = std::fs::symlink_metadata(path.join(".git"))
                .map(|m| m.is_file())
                .unwrap_or(false);
            if is_checkout {
                out.push(path);
            } else if depth + 1 < MANAGED_WALK_DEPTH {
                stack.push((path, depth + 1));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_files_absolute_and_relative() {
        let dir = Path::new("/work/proj/sub");
        assert_eq!(
            parse_git_file("gitdir: /repo/.git/worktrees/x\n", dir),
            Some(PathBuf::from("/repo/.git/worktrees/x"))
        );
        // Submodules write a relative gitdir.
        assert_eq!(
            parse_git_file("gitdir: ../.git/modules/sub\n", dir),
            Some(PathBuf::from("/work/proj/.git/modules/sub"))
        );
        assert_eq!(parse_git_file("not a git file", dir), None);
        assert_eq!(parse_git_file("gitdir:   ", dir), None);
    }

    #[test]
    fn finds_managed_checkouts_without_descending_into_them() {
        let base = std::env::temp_dir().join(format!(
            "chimaera-managed-walk-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let one = base.join("repo-1234").join("feat").join("x");
        let two = base.join("repo-1234").join("main-work");
        let inner = one.join("nested");
        for dir in [&one, &two, &inner] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(one.join(".git"), "gitdir: /r/.git/worktrees/x\n").unwrap();
        std::fs::write(two.join(".git"), "gitdir: /r/.git/worktrees/m\n").unwrap();
        std::fs::write(inner.join(".git"), "gitdir: /r/.git/worktrees/n\n").unwrap();
        let mut found = managed_worktrees_blocking(&base);
        found.sort();
        assert_eq!(found, vec![one.clone(), two.clone()]);
        assert_eq!(
            gitdir_of_blocking(&one),
            Some(PathBuf::from("/r/.git/worktrees/x"))
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
