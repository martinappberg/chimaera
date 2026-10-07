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
pub struct AgentLatest {
    /// Bare version ("0.146.0") — prefix-stripped and charset-gated, safe
    /// for the wire and the UI.
    pub(crate) version: String,
    /// When the most recent attempt ran, unix seconds.
    pub(crate) checked_at: u64,
    pub(crate) error: Option<String>,
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
///
/// The account's cloud never asks: its agents come with its image and are
/// updated with it, so there is nothing a release would be offered for.
pub(crate) async fn check_all(state: &Arc<AppState>) {
    if state.policy().updates_managed(state) {
        return;
    }
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
            let fresh = checked_result(cache.get(&kind), result, now);
            changed |= cache.get(&kind) != Some(&fresh);
            cache.insert(kind, fresh);
        }
    }
    if changed {
        state.changes.notify_waiters();
    }
}

// Keep a known release after a failed re-check, but never represent that
// stale knowledge as a successful check. Bound network diagnostics on the wire.
fn checked_result(
    previous: Option<&AgentLatest>,
    result: anyhow::Result<String>,
    now: u64,
) -> AgentLatest {
    match result {
        Ok(version) => AgentLatest {
            version,
            checked_at: now,
            error: None,
        },
        Err(error) => AgentLatest {
            version: previous.map(|p| p.version.clone()).unwrap_or_default(),
            checked_at: now,
            error: Some(error.to_string().chars().take(512).collect()),
        },
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
        // The official registry carries the same release as the standalone CLI.
        AgentKind::Grok => {
            let body = curl("https://registry.npmjs.org/@xai-official/grok/latest", &[]).await?;
            parse_npm_latest(&body)
        }
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

/// The one `curl` invocation every phone-home shares: fail on HTTP errors,
/// follow redirects (release assets live behind one), a wall clock and a
/// size cap, chimaera's User-Agent.
fn curl_command(
    url: &str,
    headers: &[&str],
    max_bytes: u64,
    timeout_secs: u64,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("curl");
    // -S keeps curl's own one-line diagnosis on stderr under -s: it is the
    // part of a failed check a user can act on ("Could not resolve host").
    cmd.args(["-fsSL", "-S", "-m", &timeout_secs.to_string()]);
    cmd.args(["--max-filesize", &max_bytes.to_string()]);
    for header in headers {
        cmd.args(["-H", header]);
    }
    cmd.args([
        "-H",
        concat!("User-Agent: chimaera/", env!("CARGO_PKG_VERSION")),
        url,
    ]);
    cmd.kill_on_drop(true);
    cmd
}

/// One bounded fetch: 10s wall clock, 1MB body, kill_on_drop. Shared with
/// `update::fetch_latest` (the daemon's own release check) and the plugin
/// release checker — one fence for every phone-home the daemon makes.
pub(crate) async fn curl(url: &str, headers: &[&str]) -> anyhow::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    const CAP: u64 = 1 << 20;
    let mut child = curl_command(url, headers, CAP, 10)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to run curl")?;
    // Older curl versions don't enforce max-filesize for chunked bodies.
    // Bound the pipe before allocating, just as curl_file counts its bytes.
    let mut body = Vec::new();
    child
        .stdout
        .take()
        .context("curl has no stdout")?
        .take(CAP + 1)
        .read_to_end(&mut body)
        .await
        .context("reading from curl")?;
    if body.len() as u64 > CAP {
        anyhow::bail!("the download is larger than its {CAP}-byte cap");
    }
    let output = child.wait_with_output().await.context("waiting for curl")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        match stderr.lines().rev().map(str::trim).find(|l| !l.is_empty()) {
            Some(line) => anyhow::bail!("{line}"),
            None => anyhow::bail!("curl exited {}", output.status),
        }
    }
    Ok(body)
}

/// One bounded download into `dest` (created or truncated): at most
/// `max_bytes` — curl's own cap, and counted here too, since an old curl
/// only enforces it when the server announces a length — within
/// `timeout_secs`, kill_on_drop. The bytes stream to disk, never to memory;
/// the caller owns `dest`, including removing it after a failure.
pub(crate) async fn curl_file(
    url: &str,
    headers: &[&str],
    dest: &std::path::Path,
    max_bytes: u64,
    timeout_secs: u64,
) -> anyhow::Result<u64> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut cmd = curl_command(url, headers, max_bytes, timeout_secs);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().context("failed to run curl")?;
    let mut out = child.stdout.take().context("curl has no stdout")?;
    let mut file = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("could not create {}", dest.display()))?;
    let mut buf = vec![0u8; 64 << 10];
    let mut total: u64 = 0;
    loop {
        let n = out.read(&mut buf).await.context("reading from curl")?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max_bytes {
            // Dropping the child kills curl.
            anyhow::bail!("the download is larger than its {max_bytes}-byte cap");
        }
        file.write_all(&buf[..n])
            .await
            .context("writing the download")?;
    }
    file.sync_all().await.context("flushing the download")?;
    let output = child.wait_with_output().await.context("waiting for curl")?;
    if !output.status.success() {
        anyhow::bail!(
            "curl exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(total)
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
    fn failed_rechecks_keep_the_release_but_report_failure() {
        let good = checked_result(None, Ok("1.2.3".into()), 10);
        let failed = checked_result(Some(&good), Err(anyhow::anyhow!("offline")), 20);
        assert_eq!(failed.version, "1.2.3");
        assert_eq!(failed.checked_at, 20);
        assert_eq!(failed.error.as_deref(), Some("offline"));
        let recovered = checked_result(Some(&failed), Ok("1.2.3".into()), 30);
        assert_eq!(recovered.error, None);
        let unknown = checked_result(None, Err(anyhow::anyhow!("offline")), 40);
        assert!(unknown.version.is_empty());
        assert!(unknown.error.is_some());
    }

    #[tokio::test]
    async fn downloads_without_a_content_length_still_have_a_body_cap() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let serving = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            // The client may close as soon as it reads past the cap.
            let _ = socket.write_all(&vec![b'x'; (1 << 20) + 1]).await;
        });
        assert!(curl(&url, &[]).await.is_err());
        serving.await.unwrap();
    }

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
