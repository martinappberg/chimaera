//! Revisions a client names (`base=`, `rev=`): validated before git sees
//! them and resolved to a full commit sha. The name part must pass
//! `check-ref-format --allow-onelevel` (so `main`, `origin/main`, `HEAD`,
//! `v1.2` and hex shas pass), optionally followed by `~N` / `^N` steps
//! (`HEAD~1`, `abc123^`); nothing flag-shaped, no whitespace, no ranges.
//! Then `rev-parse --verify --quiet <rev>^{commit}` resolves it — the one
//! form the log, show and diff routes run with.

use std::path::Path;

use tokio::sync::Semaphore;

use super::service::run_git;

/// The longest revision accepted.
const MAX_REV: usize = 256;

/// Split `rev` into its name and a `~N`/`^N` suffix, or `None` when the
/// shape is wrong before any process runs.
pub(super) fn split_rev(rev: &str) -> Option<(&str, &str)> {
    if rev.is_empty() || rev.len() > MAX_REV || rev.starts_with('-') {
        return None;
    }
    if rev.contains("@{")
        || rev
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == ':')
    {
        return None;
    }
    let cut = rev.find(['~', '^']).unwrap_or(rev.len());
    let (name, suffix) = rev.split_at(cut);
    if name.is_empty() {
        return None;
    }
    // The suffix: any run of `~` / `^`, each optionally followed by digits.
    let mut chars = suffix.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '~' && c != '^' {
            return None;
        }
        while chars.peek().is_some_and(|d| d.is_ascii_digit()) {
            chars.next();
        }
    }
    Some((name, suffix))
}

/// Resolve a client-named revision to a full commit sha in the repository
/// at `dir`, or `None` (malformed, not a ref git accepts, or no such
/// commit).
pub(super) async fn resolve_commit(
    git: &Path,
    procs: &Semaphore,
    dir: &Path,
    rev: &str,
) -> Option<String> {
    let (name, _) = split_rev(rev)?;
    let check = run_git(
        git,
        procs,
        dir,
        &["check-ref-format", "--allow-onelevel", name],
        1024,
    )
    .await
    .ok()?;
    if !check.success {
        return None;
    }
    let spec = format!("{rev}^{{commit}}");
    let out = run_git(
        git,
        procs,
        dir,
        &["rev-parse", "--verify", "--quiet", &spec],
        1024,
    )
    .await
    .ok()?;
    if !out.success {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    super::anchor::is_sha(&sha).then_some(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rev_shapes() {
        assert_eq!(split_rev("main"), Some(("main", "")));
        assert_eq!(split_rev("origin/main"), Some(("origin/main", "")));
        assert_eq!(split_rev("HEAD~2"), Some(("HEAD", "~2")));
        assert_eq!(split_rev("abc1234^"), Some(("abc1234", "^")));
        assert_eq!(split_rev("HEAD^2~3"), Some(("HEAD", "^2~3")));
        assert_eq!(split_rev("-p"), None, "flag-shaped");
        assert_eq!(split_rev("--output=/tmp/x"), None);
        assert_eq!(split_rev("a b"), None);
        assert_eq!(
            split_rev("main..feat"),
            Some(("main..feat", "")),
            "the name half is left to check-ref-format, which refuses `..`"
        );
        assert_eq!(split_rev("HEAD:secret"), None, "no blob paths");
        assert_eq!(split_rev("main@{1}"), None, "no reflog selectors");
        assert_eq!(split_rev("~1"), None);
        assert_eq!(split_rev("HEAD~x"), None);
        assert_eq!(split_rev(""), None);
    }
}
