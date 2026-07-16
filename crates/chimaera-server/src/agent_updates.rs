//! Agent release awareness: does a newer build of an agent CLI exist?
//!
//! The daemon-side twin of `update` (chimaera's own release reporter), for
//! the agent binaries it launches: a slow periodic check per agent against
//! the same official endpoints the curated install scripts in `runtimes`
//! already trust, cached in `AppState` and surfaced as `latest_version` /
//! `update_available` on the GET /api/v1/agents rows. Settings' re-check
//! runs the probes inline via `GET /agents?check=true`.
//!
//! The transport is a `curl` subprocess for the same reason as `update.rs`:
//! it is the one HTTP client every HPC site ships, trusts, and routes
//! through its proxies. Every call is bounded (10s, 1MB) so a wedged proxy
//! never piles up work in the daemon. Failures keep the previous answer —
//! an air-gapped cluster failing four probes every six hours is normal
//! life, not a warning.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;

use crate::agents::AgentKind;
use crate::AppState;

/// How often the loop re-checks. Same cadence reasoning as the daemon's own
/// release check: four rounds a day is far below any rate limit and fresh
/// enough for CLI release cadence.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// First check waits out daemon startup, staggered after `update`'s 60s so
/// daemons booting together don't burst both checks at once.
const INITIAL_DELAY: Duration = Duration::from_secs(90);

/// The newest known upstream release of one agent CLI.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentLatest {
    /// Bare version ("0.146.0") — prefix-stripped and charset-gated, safe
    /// for the wire and the UI.
    pub(crate) version: String,
    /// When the successful probe ran, unix seconds.
    pub(crate) checked_at: u64,
}

/// Periodic checker. Gated by the same `update.autoCheck` setting as the
/// daemon's own release check — one switch turns off all phone-home.
pub(crate) async fn run_checker(state: Arc<AppState>) {
    tokio::time::sleep(INITIAL_DELAY).await;
    loop {
        if crate::lock(&state.settings).update_auto_check() {
            check_all(&state).await;
        }
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

/// Probe every agent's latest release concurrently and store what landed.
/// A failed probe keeps the previous entry (stale beats absent); any change
/// wakes `/ws/events` subscribers so open surfaces can refetch.
pub(crate) async fn check_all(state: &Arc<AppState>) {
    let results = futures::future::join_all(
        AgentKind::ALL
            .into_iter()
            .map(|kind| async move { (kind, fetch_latest(kind).await) }),
    )
    .await;
    let now = crate::update::unix_now();
    let mut changed = false;
    {
        let mut cache = crate::lock(&state.agent_updates);
        for (kind, result) in results {
            match result {
                Ok(version) => {
                    let fresh = AgentLatest {
                        version,
                        checked_at: now,
                    };
                    if cache.get(&kind).map(|l| &l.version) != Some(&fresh.version) {
                        changed = true;
                    }
                    cache.insert(kind, fresh);
                }
                Err(err) => {
                    tracing::debug!(agent = kind.as_str(), %err, "agent release check failed");
                }
            }
        }
    }
    if changed {
        state.changes.notify_waiters();
    }
}

/// A snapshot of the cached latest-release map, for the /agents row builder
/// (one lock take for all four rows).
pub(crate) fn snapshot(state: &AppState) -> HashMap<AgentKind, AgentLatest> {
    crate::lock(&state.agent_updates).clone()
}

/// Whether `latest` is strictly newer than the installed binary's
/// `--version` line. Never guesses: an unparseable side (exotic or
/// pre-release version) never claims an update. Deliberately NOT
/// `release_is_newer` — its `0.0.1` dev-sentinel special case is about
/// chimaera's own build stamping, not agent versions.
pub(crate) fn update_available(current_line: Option<&str>, latest: &str) -> bool {
    let Some(current) = current_line.and_then(bare_version) else {
        return false;
    };
    match (
        chimaera_core::parse_version(current),
        chimaera_core::parse_version(latest),
    ) {
        (Some(cur), Some(new)) => new > cur,
        _ => false,
    }
}

/// The bare version number wherever the CLI buried it in its `--version`
/// line ("codex-cli 0.144.1", "2.1.197 (Claude Code)") — the first
/// digit-leading token, the same rule the UI's `versionNumber()` renders by.
pub(crate) fn bare_version(line: &str) -> Option<&str> {
    line.split_whitespace()
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()))
}

// --- the per-agent probes ------------------------------------------------

/// Fetch the latest released version of one agent, from the same official
/// endpoint its curated install script uses (see `runtimes`).
async fn fetch_latest(kind: AgentKind) -> anyhow::Result<String> {
    match kind {
        AgentKind::Claude => {
            let body = curl(
                &format!("{}/latest", crate::runtimes::CLAUDE_DOWNLOAD_BASE),
                &[],
            )
            .await?;
            parse_claude_latest(&body)
        }
        AgentKind::Codex => {
            let body = curl(
                "https://api.github.com/repos/openai/codex/releases/latest",
                &["Accept: application/vnd.github+json"],
            )
            .await?;
            parse_codex_release(&body)
        }
        AgentKind::Antigravity => {
            let body = curl(
                &format!(
                    "{}/manifests/{}.json",
                    crate::runtimes::AGY_MANIFEST_BASE,
                    agy_platform()
                ),
                &[],
            )
            .await?;
            parse_agy_manifest(&body)
        }
        // No managed install exists (node runtime, phase 2), but a personal
        // npm install still deserves the "newer exists" signal.
        AgentKind::Gemini => {
            let body = curl("https://registry.npmjs.org/@google/gemini-cli/latest", &[]).await?;
            parse_npm_latest(&body)
        }
    }
}

/// The antigravity manifest platform for this daemon's build target. The
/// released VERSION is platform-uniform, so the non-musl variant is fine
/// even where the install script would pick musl at runtime.
fn agy_platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin_arm64",
        ("macos", _) => "darwin_amd64",
        (_, "aarch64") => "linux_arm64",
        _ => "linux_amd64",
    }
}

/// One bounded fetch: 10s wall clock, 1MB body, kill_on_drop. Shared with
/// `update::fetch_latest` (the daemon's own release check) — one fence for
/// every phone-home the daemon makes.
pub(crate) async fn curl(url: &str, headers: &[&str]) -> anyhow::Result<Vec<u8>> {
    let mut cmd = tokio::process::Command::new("curl");
    cmd.args(["-fsSL", "-m", "10", "--max-filesize", "1048576"]);
    for header in headers {
        cmd.args(["-H", header]);
    }
    cmd.args([
        "-H",
        concat!("User-Agent: chimaera/", env!("CARGO_PKG_VERSION")),
        url,
    ]);
    let output = cmd
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to run curl")?;
    if !output.status.success() {
        anyhow::bail!(
            "curl exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

/// The gate every probed version passes before it is stored: digit-leading,
/// `[0-9.A-Za-z-]` only — the same charset the install scripts enforce.
/// These strings land on the wire and in UI copy; anything exotic is
/// refused rather than echoed.
fn checked_version(version: &str) -> anyhow::Result<String> {
    let version = version.trim();
    let valid = version.starts_with(|c: char| c.is_ascii_digit())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
    if !valid {
        anyhow::bail!("unexpected version string {version:?}");
    }
    Ok(version.to_string())
}

/// `downloads.claude.ai/claude-code-releases/latest` — the version, as
/// plain text (the same URL the install script reads).
fn parse_claude_latest(body: &[u8]) -> anyhow::Result<String> {
    checked_version(std::str::from_utf8(body).context("non-utf8 version body")?)
}

/// GitHub `releases/latest` for openai/codex — tags are `rust-v0.144.1`.
fn parse_codex_release(body: &[u8]) -> anyhow::Result<String> {
    let value: serde_json::Value = serde_json::from_slice(body).context("bad release JSON")?;
    let tag = value
        .get("tag_name")
        .and_then(|t| t.as_str())
        .context("release has no tag_name")?;
    let version = tag.strip_prefix("rust-v").unwrap_or(tag);
    checked_version(version.strip_prefix('v').unwrap_or(version))
}

/// The antigravity auto-updater manifest — `{"version": ..., "url": ...,
/// "sha512": ...}` (the same shape the install script parses with sed).
fn parse_agy_manifest(body: &[u8]) -> anyhow::Result<String> {
    let value: serde_json::Value = serde_json::from_slice(body).context("bad manifest JSON")?;
    let version = value
        .get("version")
        .and_then(|v| v.as_str())
        .context("manifest has no version")?;
    checked_version(version)
}

/// The npm registry's `/latest` dist-tag document — one version manifest.
fn parse_npm_latest(body: &[u8]) -> anyhow::Result<String> {
    let value: serde_json::Value = serde_json::from_slice(body).context("bad registry JSON")?;
    let version = value
        .get("version")
        .and_then(|v| v.as_str())
        .context("registry document has no version")?;
    checked_version(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_official_payload_shapes() {
        assert_eq!(parse_claude_latest(b"2.1.207\n").unwrap(), "2.1.207");
        assert!(parse_claude_latest(b"<html>proxy login</html>").is_err());
        assert!(parse_claude_latest(b"../../etc/passwd").is_err());

        let codex = br#"{"tag_name": "rust-v0.146.0", "html_url": "x"}"#;
        assert_eq!(parse_codex_release(codex).unwrap(), "0.146.0");
        assert!(parse_codex_release(b"{}").is_err());

        let agy = br#"{"version": "1.2.3", "url": "u", "sha512": "s"}"#;
        assert_eq!(parse_agy_manifest(agy).unwrap(), "1.2.3");

        let npm = br#"{"name": "@google/gemini-cli", "version": "0.9.0"}"#;
        assert_eq!(parse_npm_latest(npm).unwrap(), "0.9.0");
    }

    #[test]
    fn bare_version_finds_the_number_wherever_the_cli_buried_it() {
        assert_eq!(bare_version("2.1.197 (Claude Code)"), Some("2.1.197"));
        assert_eq!(bare_version("codex-cli 0.144.1"), Some("0.144.1"));
        assert_eq!(bare_version("0.9.0"), Some("0.9.0"));
        assert_eq!(bare_version("no digits here"), None);
    }

    #[test]
    fn update_available_never_guesses() {
        assert!(update_available(Some("codex-cli 0.144.1"), "0.146.0"));
        assert!(update_available(Some("2.1.197 (Claude Code)"), "2.1.207"));
        // Numeric compare, not lexicographic.
        assert!(update_available(Some("0.9.9"), "0.10.0"));
        // Same or older: no update.
        assert!(!update_available(Some("2.1.207 (Claude Code)"), "2.1.207"));
        assert!(!update_available(Some("2.2.0 (Claude Code)"), "2.1.207"));
        // Unparseable on either side: never claim one.
        assert!(!update_available(Some("built from source"), "1.0.0"));
        assert!(!update_available(Some("1.0.0"), "2.0.0-beta.1"));
        assert!(!update_available(None, "1.0.0"));
        // Agent versions get NO 0.0.1 dev-sentinel exemption (that rule is
        // chimaera's own build stamping, see release_is_newer).
        assert!(update_available(Some("0.0.1"), "0.1.0"));
    }
}
