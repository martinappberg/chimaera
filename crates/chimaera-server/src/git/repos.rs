//! A workspace's repositories, and the file-level facts that need no git
//! process.
//!
//! A workspace's repositories are the one at or around its root (the
//! service's `discover`) plus those found below it — never by walking the
//! tree. Four cheap sources feed the set: a two-level `.git` probe at open
//! (children and grandchildren of the root, skipping the Quick Open ignore
//! list, at most [`PROBE_CHECKS`] checks, off the reactor, once per open and
//! on the panel's refresh); file-tree listings that include a `.git` entry;
//! an agent's folder landing in a repository nobody knew (one `rev-parse`,
//! by the session tracker); and submodules (`.gitmodules` of a known
//! repository, and porcelain-v2's `S` marks). At most [`MAX_REPOS`] per
//! workspace; past that the set says `capped`. A path belongs to the
//! innermost repository containing it.
//!
//! The blocking functions here are filesystem work — callers run them off
//! the reactor — and each is bounded.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::parse::RepoInfo;

/// Repositories one workspace lists, its own included.
pub(crate) const MAX_REPOS: usize = 32;

/// `.git` checks the open-time probe makes (children + grandchildren).
pub(super) const PROBE_CHECKS: usize = 2000;

/// Entries read from one directory during the probe: a folder of thousands
/// of files costs at most this much, whatever it holds.
const PROBE_ENTRIES_PER_DIR: usize = 5000;

/// `.gitmodules` is small; a bigger one is not read.
const GITMODULES_CAP: u64 = 64 * 1024;

/// How a repository relates to the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RepoKind {
    /// The workspace root is the repository's top level.
    Root,
    /// The workspace root is inside the repository.
    Enclosing,
    /// A repository below the root (a cloned tool, a project in a folder).
    Nested,
    /// A submodule of another repository.
    Submodule,
}

impl RepoKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RepoKind::Root => "root",
            RepoKind::Enclosing => "enclosing",
            RepoKind::Nested => "nested",
            RepoKind::Submodule => "submodule",
        }
    }
}

/// How a repository below the root was found. A refresh re-probes; what
/// the tree, agents and submodule marks found stays while its `.git` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    Probe,
    Tree,
    Agent,
    Submodule,
}

/// A repository found below a workspace's root.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    pub(crate) info: RepoInfo,
    pub(crate) kind: RepoKind,
    pub(super) source: Source,
}

/// One workspace's repositories below its root (the primary lives in the
/// service's discovery cache).
#[derive(Default)]
pub(super) struct RepoSet {
    /// The open-time probe ran (a refresh runs it again).
    pub(super) probed: bool,
    /// By top level, so the listing is path-ordered.
    pub(super) found: BTreeMap<PathBuf, Found>,
    /// More were seen than [`MAX_REPOS`] allows.
    pub(super) capped: bool,
}

/// The checkout at `dir` (it holds a `.git`), from files alone: its git
/// dir, and the common dir a linked worktree's `commondir` names.
pub(super) fn checkout_at_blocking(dir: &Path) -> Option<RepoInfo> {
    let git_dir = gitdir_of_blocking(dir)?;
    let common_dir = read_small(&git_dir.join("commondir"), GIT_FILE_CAP)
        .and_then(|text| {
            let line = text.lines().next()?.trim().to_string();
            (!line.is_empty()).then_some(line)
        })
        .map(|rel| {
            let p = PathBuf::from(rel);
            if p.is_absolute() {
                p
            } else {
                super::service::normalize(&git_dir.join(p))
            }
        })
        .unwrap_or_else(|| git_dir.clone());
    Some(RepoInfo {
        toplevel: dir.to_path_buf(),
        common_dir,
        git_dir,
    })
}

/// A submodule's git dir lives in its superproject's `.git/modules/`.
pub(super) fn is_submodule(info: &RepoInfo) -> bool {
    let parts: Vec<_> = info.git_dir.components().collect();
    parts
        .windows(2)
        .any(|w| w[0].as_os_str() == ".git" && w[1].as_os_str() == "modules")
}

fn read_small(path: &Path, cap: u64) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(cap)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

fn has_dotgit(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).is_ok()
}

/// The open-time probe: which children and grandchildren of `root` hold a
/// `.git`. Skips ignored names and symlinks; stops after [`PROBE_CHECKS`]
/// checks (the second value says it was cut short).
pub(super) fn probe_two_levels_blocking(root: &Path, ignore: &[String]) -> (Vec<PathBuf>, bool) {
    let mut out = Vec::new();
    let mut checks = 0usize;
    let dirs_of = |dir: &Path| -> Vec<PathBuf> {
        let Ok(read) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = read
            .flatten()
            .take(PROBE_ENTRIES_PER_DIR)
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name != ".git" && !ignore.iter().any(|i| *i == name)
            })
            .map(|e| e.path())
            .collect();
        dirs.sort();
        dirs
    };
    for child in dirs_of(root) {
        checks += 1;
        if checks > PROBE_CHECKS {
            return (out, true);
        }
        if has_dotgit(&child) {
            out.push(child.clone());
        }
        for grandchild in dirs_of(&child) {
            checks += 1;
            if checks > PROBE_CHECKS {
                return (out, true);
            }
            if has_dotgit(&grandchild) {
                out.push(grandchild);
            }
        }
    }
    (out, false)
}

/// The submodule paths a checkout's `.gitmodules` names (repo-relative,
/// plain components only), at most [`MAX_REPOS`].
pub(super) fn gitmodules_paths_blocking(toplevel: &Path) -> Vec<PathBuf> {
    let Some(text) = read_small(&toplevel.join(".gitmodules"), GITMODULES_CAP) else {
        return Vec::new();
    };
    parse_gitmodules(&text)
}

pub(super) fn parse_gitmodules(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            if key.trim() != "path" {
                return None;
            }
            let rel = PathBuf::from(value.trim().trim_matches('"'));
            let plain = !rel.as_os_str().is_empty()
                && rel
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)));
            plain.then_some(rel)
        })
        .take(MAX_REPOS)
        .collect()
}

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

// ---- discovery (async; the blocking halves above run off the reactor) -----

/// A workspace's repositories, primary first, then the ones below its root
/// in path order.
pub(super) struct Listing {
    /// The probe of the root: a repository, not one, or git couldn't read it.
    pub(super) primary: super::service::ProbeOutcome,
    pub(super) found: Vec<Found>,
    pub(super) capped: bool,
}

/// Discover (once per open; again when `refresh`) and list a workspace's
/// repositories. A change to the set bumps the workspace's git epoch.
pub(super) async fn discover_all(
    state: &std::sync::Arc<crate::AppState>,
    git: &Path,
    ws: &crate::workspaces::Workspace,
    refresh: bool,
) -> Listing {
    let primary = state.git.discover(git, &ws.id, &ws.root).await;
    let probed = crate::lock(&state.git.repo_sets)
        .get(&ws.id)
        .is_some_and(|set| set.probed);
    if refresh || !probed {
        let root = ws.root.clone();
        let primary_top = primary.repo().map(|r| r.toplevel.clone());
        let kept: Vec<Found> = crate::lock(&state.git.repo_sets)
            .get(&ws.id)
            .map(|set| {
                set.found
                    .values()
                    .filter(|f| f.source != Source::Probe)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let ignore = {
            let state = state.clone();
            tokio::task::spawn_blocking(move || {
                crate::lock(&state.settings)
                    .quickopen_ignore_dirs()
                    .unwrap_or_else(|| {
                        crate::quickopen::IGNORED_DIRS
                            .iter()
                            .map(|s| s.to_string())
                            .collect()
                    })
            })
            .await
            .unwrap_or_default()
        };
        let scanned = tokio::task::spawn_blocking(move || {
            let mut out: Vec<Found> = Vec::new();
            let (dirs, _cut) = probe_two_levels_blocking(&root, &ignore);
            for dir in dirs {
                if let Some(info) = checkout_at_blocking(&dir) {
                    let kind = if is_submodule(&info) {
                        RepoKind::Submodule
                    } else {
                        RepoKind::Nested
                    };
                    out.push(Found {
                        info,
                        kind,
                        source: Source::Probe,
                    });
                }
            }
            // Submodules the root repository declares, at any depth.
            let mut tops: Vec<PathBuf> = primary_top.into_iter().collect();
            tops.extend(out.iter().map(|f| f.info.toplevel.clone()));
            for top in tops {
                for rel in gitmodules_paths_blocking(&top) {
                    let dir = top.join(rel);
                    if dir.starts_with(&root) && has_dotgit(&dir) {
                        if let Some(info) = checkout_at_blocking(&dir) {
                            out.push(Found {
                                info,
                                kind: RepoKind::Submodule,
                                source: Source::Probe,
                            });
                        }
                    }
                }
            }
            // What the tree and agents found stays while it is still there.
            for f in kept {
                if has_dotgit(&f.info.toplevel) {
                    out.push(f);
                }
            }
            out
        })
        .await
        .unwrap_or_default();
        {
            let mut sets = crate::lock(&state.git.repo_sets);
            let set = sets.entry(ws.id.clone()).or_default();
            set.found.clear();
            set.capped = false;
            set.probed = true;
        }
        for found in scanned {
            state.git.add_found(&ws.id, &ws.root, found);
        }
        state.git.bump(&ws.id);
        state.changes.notify_waiters();
    }
    let sets = crate::lock(&state.git.repo_sets);
    let (found, capped) = sets
        .get(&ws.id)
        .map(|set| (set.found.values().cloned().collect(), set.capped))
        .unwrap_or_default();
    Listing {
        primary,
        found,
        capped,
    }
}

/// A file-tree listing showed a `.git` inside `dir`: if a workspace holds
/// `dir` and doesn't know it yet, it is a repository now (free — the
/// listing already happened).
pub(crate) async fn note_listed_dir(state: &crate::AppState, dir: &Path) {
    let dir = dir.to_path_buf();
    let workspaces: Vec<crate::workspaces::Workspace> = crate::lock(&state.workspaces)
        .list()
        .into_iter()
        .filter(|w| dir.starts_with(&w.root) && dir != w.root)
        .filter(|w| !state.git.known_toplevels(&w.id).contains(&dir))
        .collect();
    if workspaces.is_empty() {
        return;
    }
    let probe_dir = dir.clone();
    let Ok(Some(info)) =
        tokio::task::spawn_blocking(move || checkout_at_blocking(&probe_dir)).await
    else {
        return;
    };
    note_found(state, &workspaces, info, Source::Tree);
}

/// A session's folder resolved (one `rev-parse`) to a repository its
/// workspace didn't know: add it.
pub(super) fn note_agent_repo(state: &crate::AppState, ws_id: &str, info: &RepoInfo) {
    let Some(ws) = crate::lock(&state.workspaces).get(ws_id) else {
        return;
    };
    if info.toplevel == ws.root
        || !info.toplevel.starts_with(&ws.root)
        || state.git.known_toplevels(ws_id).contains(&info.toplevel)
    {
        return;
    }
    note_found(state, &[ws], info.clone(), Source::Agent);
}

/// Porcelain v2 marked `rels` of `outer` as submodules.
pub(super) async fn note_submodules(
    state: &crate::AppState,
    ws: &crate::workspaces::Workspace,
    outer: &Path,
    rels: Vec<String>,
) {
    let known = state.git.known_toplevels(&ws.id);
    let dirs: Vec<PathBuf> = rels
        .iter()
        .map(|r| outer.join(r.trim_end_matches('/')))
        .filter(|d| d.starts_with(&ws.root) && !known.contains(d))
        .take(MAX_REPOS)
        .collect();
    if dirs.is_empty() {
        return;
    }
    let infos = tokio::task::spawn_blocking(move || {
        dirs.iter()
            .filter_map(|d| checkout_at_blocking(d))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    for info in infos {
        note_found(state, std::slice::from_ref(ws), info, Source::Submodule);
    }
}

fn note_found(
    state: &crate::AppState,
    workspaces: &[crate::workspaces::Workspace],
    info: RepoInfo,
    source: Source,
) {
    let kind = if source == Source::Submodule || is_submodule(&info) {
        RepoKind::Submodule
    } else {
        RepoKind::Nested
    };
    let mut changed = false;
    for ws in workspaces {
        if state.git.add_found(
            &ws.id,
            &ws.root,
            Found {
                info: info.clone(),
                kind,
                source,
            },
        ) {
            state.git.bump(&ws.id);
            changed = true;
        }
    }
    if changed {
        state.changes.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gitmodules_paths_safely() {
        let text = "[submodule \"vendor/tool\"]\n\tpath = vendor/tool\n\turl = https://x/y.git\n\
                    [submodule \"bad\"]\n\tpath = ../escape\n[submodule \"abs\"]\n\tpath = /etc\n";
        assert_eq!(parse_gitmodules(text), vec![PathBuf::from("vendor/tool")]);
    }

    #[test]
    fn two_level_probe_finds_repos_and_skips_ignored_and_deep() {
        let base = std::env::temp_dir().join(format!(
            "chimaera-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        for dir in [
            "pipeline/.git",
            "tools/cloned/.git",
            "node_modules/pkg/.git",
            "a/b/c/.git",
        ] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        std::fs::create_dir_all(base.join("analysis")).unwrap();
        std::fs::write(
            base.join("analysis/.git"),
            "gitdir: ../pipeline/.git/worktrees/an\n",
        )
        .unwrap();
        let ignore: Vec<String> = vec!["node_modules".into()];
        let (mut found, cut) = probe_two_levels_blocking(&base, &ignore);
        found.sort();
        assert!(!cut);
        assert_eq!(
            found,
            vec![
                base.join("analysis"),
                base.join("pipeline"),
                base.join("tools/cloned"),
            ],
            "ignored and three-deep are not probed"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn submodule_git_dirs_are_recognized() {
        let sub = RepoInfo {
            toplevel: PathBuf::from("/w/outer/vendor/tool"),
            common_dir: PathBuf::from("/w/outer/.git/modules/tool"),
            git_dir: PathBuf::from("/w/outer/.git/modules/tool"),
        };
        assert!(is_submodule(&sub));
        let linked = RepoInfo {
            toplevel: PathBuf::from("/w/wt"),
            common_dir: PathBuf::from("/w/outer/.git"),
            git_dir: PathBuf::from("/w/outer/.git/worktrees/wt"),
        };
        assert!(!is_submodule(&linked));
    }

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
