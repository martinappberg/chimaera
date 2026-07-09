use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::json;

/// The oldest git this service can drive. Every core command uses a flag or
/// subcommand younger than this: `worktree` and `rev-parse --git-common-dir`
/// (2.5), `status --porcelain=v2` (2.11), and `--no-optional-locks` (2.15).
/// Below it we degrade to an honest "your git is too old" instead of parsing
/// garbage — a real hazard on HPC login nodes still shipping RHEL 7's 1.8.3.1.
pub(super) const MIN_GIT: (u32, u32) = (2, 15);

/// How the git binary was found — surfaced so the UI can explain what it is
/// pointed at ("your login shell's git" vs "the path you set").
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitSource {
    /// The explicit `git.path` setting.
    Setting,
    /// `command -v git` in the user's login shell (picks up module loads).
    LoginShell,
    /// The daemon's own PATH (`git`) — the last resort.
    Path,
}

impl GitSource {
    fn as_str(self) -> &'static str {
        match self {
            GitSource::Setting => "setting",
            GitSource::LoginShell => "login-shell",
            GitSource::Path => "path",
        }
    }
}

/// The resolved git the whole service runs on: which binary, how it was found,
/// and whether it clears [`MIN_GIT`]. Daemon-global (one git for every
/// workspace), recomputed only when the `git.path` setting changes.
pub(super) struct GitBinary {
    /// The binary to spawn (absolute when resolved, bare `git` as a last resort).
    pub(super) path: PathBuf,
    source: GitSource,
    /// `git --version`'s raw line, e.g. "git version 2.39.1 (Apple Git-…)".
    /// `None` when the binary could not be run at all (missing / not executable).
    raw: Option<String>,
    /// Parsed (major, minor, patch), when the raw line was understood.
    parsed: Option<(u32, u32, u32)>,
    /// The parsed version clears [`MIN_GIT`] — the service can actually run.
    pub(super) adequate: bool,
}

impl GitBinary {
    /// The parsed version as "MAJOR.MINOR.PATCH", for the diagnostic UI.
    pub(super) fn version_str(&self) -> Option<String> {
        self.parsed.map(|(a, b, c)| format!("{a}.{b}.{c}"))
    }

    /// The diagnostic block carried on every git-status response, so the client
    /// can explain a too-old / missing git instead of rendering a blank repo.
    pub(super) fn json(&self) -> serde_json::Value {
        json!({
            "ok": self.adequate,
            "path": self.path.to_string_lossy(),
            "source": self.source.as_str(),
            "version": self.version_str(),
            "raw": self.raw,
            "min": format!("{}.{}", MIN_GIT.0, MIN_GIT.1),
        })
    }
}

/// Resolve the git binary: an explicit path wins, then the login shell's git
/// (so `module load git` in a user's dotfiles is picked up with zero config),
/// then the daemon's PATH. Whatever is chosen is version-probed so the caller
/// can gate on [`MIN_GIT`].
pub(super) async fn resolve_git_binary(configured: Option<String>) -> GitBinary {
    let (path, source) = match configured {
        Some(p) => (PathBuf::from(p), GitSource::Setting),
        None => match login_shell_git().await {
            Some(p) => (p, GitSource::LoginShell),
            None => (PathBuf::from("git"), GitSource::Path),
        },
    };
    let raw = probe_git_version(&path).await;
    let parsed = raw.as_deref().and_then(parse_git_version);
    let adequate = parsed.is_some_and(|(maj, min, _)| (maj, min) >= MIN_GIT);
    GitBinary {
        path,
        source,
        raw,
        parsed,
        adequate,
    }
}

/// `command -v git` through the user's interactive login shell — the same
/// trick the agent launcher uses, because the daemon's own PATH on an HPC
/// login node is the stock `/usr/bin/git`, while the modern one lives behind
/// `module load git` that only the user's profile (often the interactive
/// `.bashrc`/`.zshrc`) has applied. `-ilc` and the passwd-backed
/// [`chimaera_core::login_shell`] mirror the launcher's resolution so a
/// GUI-launched daemon resolves git the same way a terminal would.
async fn login_shell_git() -> Option<PathBuf> {
    let shell = chimaera_core::login_shell();
    let output = tokio::process::Command::new(&shell)
        .arg("-ilc")
        .arg("command -v git")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(5), output)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Login shells may print banners; the path is the last non-empty line.
    let path = String::from_utf8_lossy(&out.stdout)
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())?
        .to_string();
    path.starts_with('/').then(|| PathBuf::from(path))
}

/// First non-empty line of `<bin> --version`, or `None` if it cannot run.
async fn probe_git_version(bin: &Path) -> Option<String> {
    let output = tokio::process::Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(2), output)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// Parse a `git --version` line into (major, minor, patch). Lenient about the
/// trailing junk real builds carry ("2.39.GIT", "2.39.5 (Apple Git-154)"):
/// non-digits are stripped per component and a missing component reads as 0.
fn parse_git_version(raw: &str) -> Option<(u32, u32, u32)> {
    // "git version 2.39.1 …" — the version token is the third word.
    let token = raw.split_whitespace().nth(2)?;
    let mut parts = token
        .split('.')
        .map(|p| p.trim_matches(|c: char| !c.is_ascii_digit()));
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_versions_and_gates_on_min() {
        // The version token is the third word; trailing build junk is tolerated.
        assert_eq!(parse_git_version("git version 2.39.1"), Some((2, 39, 1)));
        assert_eq!(
            parse_git_version("git version 2.39.5 (Apple Git-154)"),
            Some((2, 39, 5))
        );
        assert_eq!(parse_git_version("git version 2.20.GIT"), Some((2, 20, 0)));
        assert_eq!(parse_git_version("git version 1.8.3.1"), Some((1, 8, 3)));
        assert_eq!(parse_git_version("not a version line"), None);

        // The gate that decides the "too old" panel: RHEL 7's 1.8.3.1 fails,
        // the 2.15 floor and anything above it passes.
        let adequate =
            |raw: &str| parse_git_version(raw).is_some_and(|(maj, min, _)| (maj, min) >= MIN_GIT);
        assert!(!adequate("git version 1.8.3.1"));
        assert!(!adequate("git version 2.14.9"));
        assert!(adequate("git version 2.15.0"));
        assert!(adequate("git version 2.39.1"));
    }
}
