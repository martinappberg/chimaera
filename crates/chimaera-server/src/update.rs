//! Release awareness: does a newer chimaera exist?
//!
//! The daemon checks its own GitHub releases a few times a day and exposes
//! the answer at `GET /api/v1/update` (+ an `update` frame on `/ws/events`
//! when it changes), so every attached window — app or plain browser —
//! learns about updates from the daemon it is already talking to. *Applying*
//! an update stays with the clients that can do it (the app's signed
//! updater, `chimaera connect --update-daemon`); the daemon only reports.
//!
//! The transport is a `curl` subprocess, deliberately: it is the one HTTP
//! client every HPC site already ships, trusts, and routes through its
//! proxies — the same reasoning as `chimaera-remote`'s release fetch. Every
//! call is bounded (10s, 1MB) so a wedged proxy can never pile up work in
//! the daemon.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

/// How often the daemon re-checks. Four calls a day per daemon is far below
/// any rate limit and fresh enough for release cadence.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// First check waits out daemon startup (resurrection, first attaches) and
/// staggers daemons that boot together (login nodes after maintenance).
const INITIAL_DELAY: Duration = Duration::from_secs(60);

/// The newest published release, as fetched.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Release {
    /// Version without the tag's `v` prefix.
    pub(crate) version: String,
    /// Release page for humans (the toast's "release notes" link).
    pub(crate) url: String,
    pub(crate) published_at: Option<String>,
}

/// What the daemon currently knows about updates.
#[derive(Debug, Default)]
pub(crate) struct UpdateStatus {
    pub(crate) checked_at: Option<u64>,
    pub(crate) latest: Option<Release>,
}

impl UpdateStatus {
    pub(crate) fn available(&self) -> bool {
        self.latest
            .as_ref()
            .is_some_and(|r| chimaera_core::release_is_newer(&current_version(), &r.version))
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        json!({
            "current": chimaera_core::VERSION,
            "build": chimaera_core::BUILD_ID,
            "checked_at": self.checked_at,
            "available": self.available(),
            "latest": self.latest.as_ref().map(|r| json!({
                "version": r.version,
                "url": r.url,
                "published_at": r.published_at,
            })),
        })
    }
}

/// The version updates are compared against. `CHIMAERA_UPDATE_CURRENT`
/// exists so a dev build (whose real version is the never-outdated `0.0.1`
/// sentinel) can exercise the full popup flow against a fixture; it has no
/// production meaning.
fn current_version() -> String {
    std::env::var("CHIMAERA_UPDATE_CURRENT").unwrap_or_else(|_| chimaera_core::VERSION.to_string())
}

/// The releases endpoint. `CHIMAERA_RELEASES_API` overrides for tests and
/// dev verification (curl accepts file:// URLs, so a fixture on disk works).
fn releases_api_url() -> Option<String> {
    if let Ok(url) = std::env::var("CHIMAERA_RELEASES_API") {
        return Some(url);
    }
    let slug = chimaera_core::REPOSITORY.strip_prefix("https://github.com/")?;
    Some(format!(
        "https://api.github.com/repos/{}/releases/latest",
        slug.trim_end_matches('/')
    ))
}

/// Periodic checker. Dev builds stay silent (and off the network) unless
/// the endpoint is explicitly overridden; users can turn checking off with
/// the `update.autoCheck` setting.
pub(crate) async fn run_checker(state: Arc<AppState>) {
    if chimaera_core::VERSION == "0.0.1" && std::env::var("CHIMAERA_RELEASES_API").is_err() {
        return;
    }
    tokio::time::sleep(INITIAL_DELAY).await;
    loop {
        if crate::lock(&state.settings).update_auto_check() {
            check_now(&state).await;
        }
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

/// Fetch and store; on any change the update epoch moves and `/ws/events`
/// subscribers wake. Failures are logged at debug — an air-gapped cluster
/// failing an update check four times a day is normal life, not a warning.
pub(crate) async fn check_now(state: &Arc<AppState>) {
    match fetch_latest().await {
        Ok(release) => {
            let mut status = crate::lock(&state.update);
            let changed = status.latest.as_ref() != Some(&release);
            status.latest = Some(release);
            status.checked_at = Some(unix_now());
            drop(status);
            if changed {
                state.update_epoch.fetch_add(1, Ordering::Relaxed);
                state.changes.notify_waiters();
            }
        }
        Err(err) => {
            tracing::debug!(%err, "release check failed");
            crate::lock(&state.update).checked_at = Some(unix_now());
        }
    }
}

async fn fetch_latest() -> anyhow::Result<Release> {
    let url = releases_api_url().context("no releases endpoint for this build")?;
    let output = tokio::process::Command::new("curl")
        .args([
            "-fsSL",
            "-m",
            "10",
            "--max-filesize",
            "1048576",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            concat!("User-Agent: chimaera/", env!("CARGO_PKG_VERSION")),
            &url,
        ])
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
    parse_release(&output.stdout)
}

/// Parse a GitHub `releases/latest` payload.
fn parse_release(body: &[u8]) -> anyhow::Result<Release> {
    let value: serde_json::Value = serde_json::from_slice(body).context("bad release JSON")?;
    let tag = value
        .get("tag_name")
        .and_then(|t| t.as_str())
        .context("release has no tag_name")?;
    Ok(Release {
        version: tag.strip_prefix('v').unwrap_or(tag).to_string(),
        url: value
            .get("html_url")
            .and_then(|u| u.as_str())
            .unwrap_or(chimaera_core::REPOSITORY)
            .to_string(),
        published_at: value
            .get("published_at")
            .and_then(|p| p.as_str())
            .map(str::to_string),
    })
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Deserialize)]
pub(crate) struct UpdateQuery {
    #[serde(default)]
    refresh: Option<bool>,
}

/// GET /api/v1/update — the cached answer, instantly; `?refresh=true` checks
/// first (bounded by curl's own timeout) and returns the fresh truth.
pub(crate) async fn get_update(
    State(state): State<Arc<AppState>>,
    Query(query): Query<UpdateQuery>,
) -> Json<serde_json::Value> {
    if query.refresh == Some(true) {
        check_now(&state).await;
    }
    Json(crate::lock(&state.update).to_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_release_shape() {
        let body = br#"{
            "tag_name": "v0.6.0",
            "html_url": "https://github.com/martinappberg/chimaera/releases/tag/v0.6.0",
            "published_at": "2026-07-01T12:00:00Z",
            "assets": []
        }"#;
        let release = parse_release(body).unwrap();
        assert_eq!(release.version, "0.6.0");
        assert!(release.url.ends_with("/tag/v0.6.0"));
        assert_eq!(
            release.published_at.as_deref(),
            Some("2026-07-01T12:00:00Z")
        );

        assert!(parse_release(b"{}").is_err(), "tag_name is required");
        assert!(parse_release(b"not json").is_err());
    }

    #[test]
    fn status_json_shape() {
        let status = UpdateStatus {
            checked_at: Some(1_000),
            latest: Some(Release {
                version: "99.0.0".into(),
                url: "https://example.test/rel".into(),
                published_at: None,
            }),
        };
        let json = status.to_json();
        assert_eq!(json["current"], chimaera_core::VERSION);
        assert_eq!(json["latest"]["version"], "99.0.0");
        // The workspace dev sentinel is never "outdated" (release_is_newer),
        // so a dev daemon reports available: false even against 99.0.0.
        assert_eq!(json["available"], chimaera_core::VERSION != "0.0.1");
        // No check yet = empty status, honestly null.
        let empty = UpdateStatus::default().to_json();
        assert_eq!(empty["latest"], serde_json::Value::Null);
        assert_eq!(empty["available"], false);
    }
}
