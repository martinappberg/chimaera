//! Embeds a build id — `<git-short-hash>[-dirty].<build-unix-secs>`, e.g.
//! `ff52221-dirty.1783438290` — so every binary can say which source it was
//! built from. The daemon self-update flow compares these across machines;
//! before build ids, every build called itself 0.0.1 and a 21-hour-old
//! daemon was indistinguishable from a fresh one (field find on a cluster).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let hash = git(&["rev-parse", "--short=7", "HEAD"]);
    // Empty porcelain output = clean tree; a failed probe counts as clean so
    // non-git builds (source tarballs) read `unknown`, not `unknown-dirty`.
    let dirty = hash.is_some() && git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!(
        "cargo:rustc-env=CHIMAERA_BUILD_ID={}{}.{}",
        hash.as_deref().unwrap_or("unknown"),
        if dirty { "-dirty" } else { "" },
        secs
    );

    // Re-embed when the checked-out commit moves. Dirty-flag freshness is
    // commit-granularity by nature — plain edits touch nothing under .git.
    // Outside a git checkout (a source tarball) there is no commit to follow.
    if git(&["rev-parse", "--git-dir"]).is_none() {
        return;
    }
    match head_files() {
        Some(files) => {
            for file in files {
                println!("cargo:rerun-if-changed={}", file.display());
            }
        }
        // No file reliably moves with the commit. Cargo counts a missing
        // watched path as changed, so watching one that never exists
        // re-derives the id on every build: slow, but never stale.
        None => {
            let out_dir = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
            let never = Path::new(&out_dir).join("never-exists");
            println!("cargo:rerun-if-changed={}", never.display());
        }
    }
}

/// The files git rewrites when HEAD comes to name another commit, or `None`
/// when none reliably is: reflogs off while the branch ref is packed, the
/// reftable backend, or a git too old for `--git-path`.
///
/// Each name resolves through `--git-path`, which knows where this checkout
/// keeps it — `HEAD` and `logs/HEAD` in a linked worktree's own git dir,
/// branch refs, their reflogs and `packed-refs` in the common dir — and only
/// files that exist are returned: cargo counts a missing watched path as
/// changed, which would rebuild this crate and every dependent on every run.
fn head_files() -> Option<Vec<PathBuf>> {
    let branch = git(&["symbolic-ref", "-q", "HEAD"]);
    let mut names = vec!["HEAD".to_owned(), "logs/HEAD".to_owned()];
    if let Some(branch) = &branch {
        names.extend([
            branch.clone(),
            format!("logs/{branch}"),
            "packed-refs".to_owned(),
        ]);
    }
    let mut args = vec!["rev-parse"];
    for name in &names {
        args.extend(["--git-path", name.as_str()]);
    }
    // Relative results are relative to our working directory, the package root.
    let cwd = env::current_dir().ok()?;
    let paths: Vec<PathBuf> = git(&args)?.lines().map(|l| cwd.join(l)).collect();
    // Any other count means this git didn't understand `--git-path`.
    if paths.len() != names.len() {
        return None;
    }
    let (head, head_log) = (&paths[0], &paths[1]);

    // HEAD is rewritten by a branch switch. Detached, it holds the commit id
    // itself, so it moves with every commit too — unless it is the reftable
    // backend's `ref:` placeholder, which never changes.
    let mut files = vec![head.clone()];
    let mut follows_commits =
        branch.is_none() && !fs::read_to_string(head).ok()?.starts_with("ref:");
    // A reflog gains a line with every commit, reset or checkout, and
    // outlives `git pack-refs`.
    let mut logs = vec![head_log];
    if let [loose, branch_log, packed] = &paths[2..] {
        logs.push(branch_log);
        if loose.exists() {
            files.push(loose.clone());
            follows_commits = true;
        } else if packed.exists() {
            // A packed ref's id lives here, though a commit writes a loose
            // ref instead, which only the reflogs announce.
            files.push(packed.clone());
        }
    }
    for log in logs.into_iter().filter(|l| l.exists()) {
        files.push(log.clone());
        follows_commits = true;
    }
    follows_commits.then_some(files)
}
