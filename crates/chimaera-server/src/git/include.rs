//! `.worktreeinclude`: Claude Code's file, in gitignore syntax, naming
//! git-IGNORED files (a `.env`, local config) worth copying into a new
//! worktree. A file is copied when it is both ignored by git and matched by
//! the include patterns — the same rule Claude Code applies, so there is no
//! new convention to learn. Bounded ([`MAX_FILES`], [`MAX_BYTES`]); symlinks
//! are never followed or copied; nothing outside the source checkout is
//! read; nothing already present in the new worktree is overwritten.

use std::path::{Component, Path, PathBuf};

use super::service::run_git;

/// Most files one worktree create copies.
const MAX_FILES: usize = 200;
/// Most bytes one worktree create copies.
const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// The include file itself is small; a bigger one is not read.
const MAX_INCLUDE_FILE: u64 = 64 * 1024;
/// Directories walked inside one ignored directory the patterns reach into.
const MAX_WALK_DIRS: usize = 500;

/// What a copy did, reported on the create answer.
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct IncludeReport {
    pub(crate) copied: usize,
    pub(crate) bytes: u64,
    /// Matched files left behind because a bound was reached.
    pub(crate) capped: bool,
    /// The first few copied paths (repo-relative), for "Copied .env and 1 more".
    pub(crate) names: Vec<String>,
}

/// Names reported back per create.
const REPORTED_NAMES: usize = 3;

impl IncludeReport {
    pub(super) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "copied": self.copied,
            "bytes": self.bytes,
            "capped": self.capped,
            "names": self.names,
        })
    }
}

/// One gitignore-syntax pattern.
#[derive(Debug, Clone)]
struct Pattern {
    /// Glob segments (split on `/`).
    segments: Vec<String>,
    /// Anchored to the root (a leading or inner `/`); else matches at any
    /// depth by basename.
    anchored: bool,
    /// Trailing `/`: matches directories only.
    dir_only: bool,
    /// `!pattern`: re-includes.
    negated: bool,
}

/// The parsed include file.
#[derive(Debug, Default)]
pub(super) struct Patterns(Vec<Pattern>);

impl Patterns {
    pub(super) fn parse(text: &str) -> Patterns {
        let mut out = Vec::new();
        for raw in text.lines() {
            let line = raw.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (negated, body) = match line.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, line.strip_prefix('\\').unwrap_or(line)),
            };
            let dir_only = body.ends_with('/');
            let body = body.trim_end_matches('/');
            if body.is_empty() {
                continue;
            }
            let anchored = body.contains('/');
            let body = body.trim_start_matches('/');
            let segments: Vec<String> = body
                .split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if segments.is_empty() {
                continue;
            }
            out.push(Pattern {
                segments,
                anchored,
                dir_only,
                negated,
            });
        }
        Patterns(out)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Does the repo-relative `path` match (gitignore semantics: the last
    /// matching pattern wins, and a matched parent directory matches
    /// everything inside it)?
    pub(super) fn matches(&self, path: &str, is_dir: bool) -> bool {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        // Parents first: a directory match covers its contents.
        for end in 1..parts.len() {
            if self.decide(&parts[..end], true) == Some(true) {
                return true;
            }
        }
        self.decide(&parts, is_dir) == Some(true)
    }

    fn decide(&self, parts: &[&str], is_dir: bool) -> Option<bool> {
        let mut verdict = None;
        for p in &self.0 {
            if p.dir_only && !is_dir {
                continue;
            }
            let hit = if p.anchored {
                match_segments(&p.segments, parts)
            } else {
                parts
                    .last()
                    .is_some_and(|name| p.segments.len() == 1 && wildmatch(&p.segments[0], name))
            };
            if hit {
                verdict = Some(!p.negated);
            }
        }
        verdict
    }

    /// Could a pattern match something strictly inside the repo-relative
    /// directory `dir`? Only anchored patterns whose leading segments fit
    /// can — an unanchored basename pattern never sends the walk into an
    /// ignored tree (`node_modules/`), which is the bound that matters.
    fn reaches_into(&self, dir: &str) -> bool {
        let parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
        self.0.iter().any(|p| {
            !p.negated
                && p.anchored
                && p.segments.len() > parts.len()
                && p.segments
                    .iter()
                    .zip(parts.iter())
                    .all(|(seg, part)| seg == "**" || wildmatch(seg, part))
        })
    }
}

/// Match glob segments against path parts; `**` spans zero or more parts.
fn match_segments(segments: &[String], parts: &[&str]) -> bool {
    match segments.split_first() {
        None => parts.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=parts.len()).any(|skip| match_segments(rest, &parts[skip..]))
        }
        Some((first, rest)) => match parts.split_first() {
            Some((part, tail)) => wildmatch(first, part) && match_segments(rest, tail),
            None => false,
        },
    }
}

/// One path segment against one glob segment: `*`, `?`, `[...]` (with
/// `!`/`^` negation and ranges), `\` escapes. Never crosses a `/` (the
/// caller splits on it).
fn wildmatch(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    wild(&p, &t)
}

fn wild(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some(pi);
                    mark = ti;
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                '[' => {
                    if let Some((hit, next)) = class(p, pi, t[ti]) {
                        if hit {
                            pi = next;
                            ti += 1;
                            continue;
                        }
                    } else if t[ti] == '[' {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
                '\\' if pi + 1 < p.len() => {
                    if p[pi + 1] == t[ti] {
                        pi += 2;
                        ti += 1;
                        continue;
                    }
                }
                c => {
                    if c == t[ti] {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
            }
        }
        match star {
            Some(s) => {
                pi = s + 1;
                mark += 1;
                ti = mark;
            }
            None => return false,
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// A `[...]` class at `p[start]`: `(matched, index after ])`, or `None`
/// when unterminated (then `[` is literal).
fn class(p: &[char], start: usize, c: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negate = matches!(p.get(i), Some('!') | Some('^'));
    if negate {
        i += 1;
    }
    let mut hit = false;
    let mut first = true;
    while i < p.len() {
        if p[i] == ']' && !first {
            return Some((hit != negate, i + 1));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            if p[i] <= c && c <= p[i + 2] {
                hit = true;
            }
            i += 3;
        } else {
            if p[i] == c {
                hit = true;
            }
            i += 1;
        }
    }
    None
}

/// A repo-relative path git printed, safe to join under a checkout: no
/// absolute path, no `..`, nothing empty.
fn safe_relative(rel: &str) -> Option<PathBuf> {
    let path = Path::new(rel);
    if rel.is_empty() || path.is_absolute() {
        return None;
    }
    path.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then(|| path.to_path_buf())
}

/// Copy the files `.worktreeinclude` names from the checkout at `source`
/// into the new worktree at `dest`. No include file (or an empty one) is
/// the common case and costs one stat.
pub(super) async fn copy_included(
    git: &Path,
    procs: &tokio::sync::Semaphore,
    source: &Path,
    dest: &Path,
) -> IncludeReport {
    let include = source.join(".worktreeinclude");
    let text = {
        let include = include.clone();
        tokio::task::spawn_blocking(move || -> Option<String> {
            use std::io::Read;
            let meta = std::fs::symlink_metadata(&include).ok()?;
            if !meta.is_file() || meta.len() > MAX_INCLUDE_FILE {
                return None;
            }
            let mut text = String::new();
            std::fs::File::open(&include)
                .ok()?
                .take(MAX_INCLUDE_FILE)
                .read_to_string(&mut text)
                .ok()?;
            Some(text)
        })
        .await
        .ok()
        .flatten()
    };
    let Some(text) = text else {
        return IncludeReport::default();
    };
    let patterns = Patterns::parse(&text);
    if patterns.is_empty() {
        return IncludeReport::default();
    }
    // The ignored set, with wholly-ignored directories collapsed to one
    // `dir/` entry — git never descends into them, so this stays cheap on a
    // tree with a huge `node_modules/`.
    let out = match run_git(
        git,
        procs,
        source,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
        4 * 1024 * 1024,
    )
    .await
    {
        Ok(out) if out.success => out,
        _ => return IncludeReport::default(),
    };
    let entries: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let source = source.to_path_buf();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || copy_matching(&patterns, &entries, &source, &dest))
        .await
        .unwrap_or_default()
}

/// The blocking half: pick the matching ignored files and copy them.
fn copy_matching(
    patterns: &Patterns,
    entries: &[String],
    source: &Path,
    dest: &Path,
) -> IncludeReport {
    let mut report = IncludeReport::default();
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        if files.len() > MAX_FILES {
            break;
        }
        let is_dir = entry.ends_with('/');
        let rel_str = entry.trim_end_matches('/');
        let Some(rel) = safe_relative(rel_str) else {
            continue;
        };
        if !is_dir {
            if patterns.matches(rel_str, false) {
                files.push(rel);
            }
            continue;
        }
        let whole = patterns.matches(rel_str, true);
        if whole || patterns.reaches_into(rel_str) {
            walk_ignored_dir(patterns, source, &rel, whole, &mut files);
        }
    }
    for rel in files {
        if report.copied >= MAX_FILES {
            report.capped = true;
            break;
        }
        let from = source.join(&rel);
        let to = dest.join(&rel);
        // Symlinks are never copied (nor followed out of the checkout).
        let Ok(meta) = std::fs::symlink_metadata(&from) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if report.bytes + meta.len() > MAX_BYTES {
            report.capped = true;
            continue;
        }
        if std::fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        // Never write through a symlink: the new worktree is checked out at
        // its base, where a folder on the way may be a link pointing out.
        if !no_symlink_on_the_way(dest, &rel) {
            continue;
        }
        if let Some(parent) = to.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        match std::fs::copy(&from, &to) {
            Ok(n) => {
                report.copied += 1;
                report.bytes += n;
                if report.names.len() < REPORTED_NAMES {
                    report.names.push(rel.to_string_lossy().into_owned());
                }
            }
            Err(err) => {
                tracing::debug!(%err, file = %from.display(), ".worktreeinclude copy failed");
            }
        }
    }
    report
}

/// Whether every existing folder between `root` and `rel`'s parent is a
/// real directory (none is a symlink), so creating and writing there stays
/// inside `root`.
fn no_symlink_on_the_way(root: &Path, rel: &Path) -> bool {
    let Some(parent) = rel.parent() else {
        return true;
    };
    let mut cur = root.to_path_buf();
    for comp in parent.components() {
        cur.push(comp);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => return false,
            Ok(meta) if !meta.is_dir() => return false,
            Ok(_) => {}
            // Missing from here on: create_dir_all makes real folders.
            Err(_) => return true,
        }
    }
    true
}

/// Collect files under an ignored directory the patterns reach: all of it
/// when the directory itself matched, else just the matching files. Bounded
/// by [`MAX_WALK_DIRS`] and the file cap; symlinks never followed.
fn walk_ignored_dir(
    patterns: &Patterns,
    source: &Path,
    rel_dir: &Path,
    whole: bool,
    files: &mut Vec<PathBuf>,
) {
    let mut stack = vec![rel_dir.to_path_buf()];
    let mut visited = 0usize;
    while let Some(rel) = stack.pop() {
        visited += 1;
        if visited > MAX_WALK_DIRS || files.len() > MAX_FILES {
            return;
        }
        let Ok(read) = std::fs::read_dir(source.join(&rel)) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let child = rel.join(entry.file_name());
            let child_str = child.to_string_lossy();
            if file_type.is_dir() {
                if whole || patterns.reaches_into(&child_str) || patterns.matches(&child_str, true)
                {
                    stack.push(child);
                }
            } else if file_type.is_file() && (whole || patterns.matches(&child_str, false)) {
                files.push(child);
                if files.len() > MAX_FILES {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn never_writes_through_a_symlinked_folder() {
        let base = std::env::temp_dir().join(format!(
            "chimaera-include-link-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let dest = base.join("wt");
        let outside = base.join("outside");
        std::fs::create_dir_all(dest.join("real")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, dest.join("config")).unwrap();
        assert!(no_symlink_on_the_way(&dest, Path::new(".env")));
        assert!(no_symlink_on_the_way(&dest, Path::new("real/.env")));
        assert!(no_symlink_on_the_way(&dest, Path::new("new/dir/.env")));
        assert!(!no_symlink_on_the_way(
            &dest,
            Path::new("config/local.yaml")
        ));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn gitignore_style_matching() {
        let p = Patterns::parse(
            "# local secrets\n.env\n.env.*\n!.env.example\n/config/local.yaml\nsecrets/\ndata/**/*.key\n",
        );
        assert!(p.matches(".env", false));
        assert!(
            p.matches("packages/api/.env", false),
            "basename at any depth"
        );
        assert!(p.matches(".env.local", false));
        assert!(!p.matches(".env.example", false), "negation re-includes");
        assert!(p.matches("config/local.yaml", false));
        assert!(!p.matches("other/config/local.yaml", false), "anchored");
        assert!(p.matches("secrets", true));
        assert!(!p.matches("secrets", false), "dir-only pattern");
        assert!(p.matches("secrets/key.pem", false), "inside a matched dir");
        assert!(p.matches("data/a/b/x.key", false));
        assert!(p.matches("data/x.key", false), "** spans zero segments");
        assert!(!p.matches("README.md", false));
        assert!(p.reaches_into("config"));
        assert!(p.reaches_into("data"));
        assert!(!p.reaches_into("node_modules"));
    }

    #[test]
    fn wildmatch_classes_and_escapes() {
        assert!(wildmatch("*.log", "a.log"));
        assert!(!wildmatch("*.log", "a.txt"));
        assert!(wildmatch("file?.txt", "file1.txt"));
        assert!(wildmatch("[abc].md", "b.md"));
        assert!(!wildmatch("[!abc].md", "b.md"));
        assert!(wildmatch("[0-9]x", "7x"));
        assert!(wildmatch("\\*literal", "*literal"));
        assert!(!wildmatch("\\*literal", "xliteral"));
    }

    #[test]
    fn copies_only_matching_ignored_files_within_bounds() {
        let base = std::env::temp_dir().join(format!(
            "chimaera-include-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let src = base.join("src");
        let dst = base.join("dst");
        std::fs::create_dir_all(src.join("secrets/deep")).unwrap();
        std::fs::create_dir_all(src.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join(".env"), "A=1").unwrap();
        std::fs::write(src.join("secrets/deep/k.pem"), "k").unwrap();
        std::fs::write(src.join("node_modules/pkg/.env"), "no").unwrap();
        std::fs::write(src.join("build.log"), "no").unwrap();
        std::os::unix::fs::symlink("/etc/hosts", src.join(".env.link")).unwrap();
        let patterns = Patterns::parse(".env\n.env.link\nsecrets/\n");
        // What `ls-files --others --ignored --directory` would print.
        let entries: Vec<String> = [
            ".env",
            ".env.link",
            "secrets/",
            "node_modules/",
            "build.log",
            "../escape",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let report = copy_matching(&patterns, &entries, &src, &dst);
        assert_eq!(report.copied, 2, "{report:?}");
        assert!(dst.join(".env").exists());
        assert!(dst.join("secrets/deep/k.pem").exists());
        assert!(
            !dst.join("node_modules/pkg/.env").exists(),
            "never walked into"
        );
        assert!(!dst.join("build.log").exists(), "ignored but not included");
        assert!(
            std::fs::symlink_metadata(dst.join(".env.link")).is_err(),
            "symlinks never copied"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
