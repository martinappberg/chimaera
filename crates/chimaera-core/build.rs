//! Embeds a build id — `<git-short-hash>[-dirty].<build-unix-secs>`, e.g.
//! `ff52221-dirty.1783438290` — so every binary can say which source it was
//! built from. The daemon self-update flow compares these across machines;
//! before build ids, every build called itself 0.0.1 and a 21-hour-old
//! daemon was indistinguishable from a fresh one (field find on cluster).

use std::process::Command;

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

    // Re-embed when the checked-out commit moves: HEAD for branch switches,
    // the branch ref file for new commits. Dirty-flag freshness is
    // commit-granularity by nature — plain edits touch nothing under .git.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={git_dir}/{head_ref}");
        }
    }
}
