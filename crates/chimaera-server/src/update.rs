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

/// What the daemon currently knows about updates. A failed check keeps the
/// last good answer (`latest`, `succeeded_at`) and records why it failed, so
/// "couldn't check" never reads as "up to date".
#[derive(Debug, Default)]
pub(crate) struct UpdateStatus {
    /// The most recent attempt, successful or not.
    pub(crate) checked_at: Option<u64>,
    /// The most recent attempt that got an answer from the releases feed.
    pub(crate) succeeded_at: Option<u64>,
    pub(crate) latest: Option<Release>,
    /// Why the most recent attempt failed; cleared by the next success.
    pub(crate) error: Option<String>,
}

impl UpdateStatus {
    pub(crate) fn available(&self) -> bool {
        self.latest
            .as_ref()
            .is_some_and(|r| chimaera_core::release_is_newer(&current_version(), &r.version))
    }

    /// One word for "is there an update?", so every surface answers it the
    /// same way. A known newer release outranks a later failed check (the
    /// release did not stop existing); otherwise the last attempt decides.
    fn state(&self) -> &'static str {
        if self.available() {
            "available"
        } else if self.error.is_some() {
            "failed"
        } else if self.latest.is_some() {
            "current"
        } else {
            "unchecked"
        }
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        let current = current_version();
        json!({
            "current": current,
            "build": chimaera_core::BUILD_ID,
            "dev": chimaera_core::version_is_dev(&current),
            "state": self.state(),
            "checked_at": self.checked_at,
            "succeeded_at": self.succeeded_at,
            "error": self.error,
            "interval_secs": CHECK_INTERVAL.as_secs(),
            "available": self.available(),
            "latest": self.latest.as_ref().map(|r| json!({
                "version": r.version,
                "url": r.url,
                "published_at": r.published_at,
            })),
        })
    }
}

/// The version updates are compared against (and reported as `current`).
/// `CHIMAERA_UPDATE_CURRENT` exists so a dev build (whose real version is the
/// never-outdated `0.0.1` sentinel) can exercise the full popup flow against
/// a fixture; it has no production meaning.
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

/// Fetch and store, then move the update epoch so `/ws/events` subscribers
/// see the fresh `checked_at` (four frames a day; the UI's "checked 2h ago"
/// must not go stale). Failures are logged at debug — an air-gapped cluster
/// failing an update check four times a day is normal life, not a warning —
/// and reported in the status, where the user asking can see why.
///
/// Checks are serialized: a "check now" landing while another check runs
/// waits for it and reuses its answer instead of fetching twice.
pub(crate) async fn check_now(state: &Arc<AppState>) {
    // Every finished check moves the epoch (below, before the lock drops),
    // so a moved epoch after the wait means a check completed after we asked.
    let seen = state.update_epoch.load(Ordering::Relaxed);
    let _one_at_a_time = state.update_check.lock().await;
    if state.update_epoch.load(Ordering::Relaxed) != seen {
        return;
    }
    let result = fetch_latest().await;
    let now = unix_now();
    let mut status = crate::lock(&state.update);
    status.checked_at = Some(now);
    match result {
        Ok(release) => {
            status.latest = Some(release);
            status.succeeded_at = Some(now);
            status.error = None;
        }
        Err(err) => {
            tracing::debug!(err = %format!("{err:#}"), "release check failed");
            status.error = Some(describe_failure(&err));
        }
    }
    drop(status);
    state.update_epoch.fetch_add(1, Ordering::Relaxed);
    state.changes.notify_waiters();
}

/// A failed check, in words a user can act on. curl's own diagnosis ("Could
/// not resolve host: api.github.com") is already that, minus its prefix; an
/// HTTP 403/429 from GitHub is its unauthenticated rate limit, which a busy
/// login node's shared address hits — worth naming, since it clears itself.
fn describe_failure(err: &anyhow::Error) -> String {
    let raw = format!("{err:#}");
    if raw.starts_with("failed to run curl") {
        return "curl is not available on this host".to_string();
    }
    let msg = raw
        .strip_prefix("curl: (")
        .and_then(|rest| rest.split_once(") "))
        .map_or(raw.as_str(), |(_, m)| m);
    if msg.ends_with("error: 403") || msg.ends_with("error: 429") {
        return "GitHub is rate-limiting this address; the next check will retry".to_string();
    }
    // Wire-bounded: a pathological proxy page must not ride every frame.
    msg.chars().take(200).collect()
}

async fn fetch_latest() -> anyhow::Result<Release> {
    let url = releases_api_url().context("no releases endpoint for this build")?;
    // The shared bounded fetch (10s, 1MB, kill_on_drop) — one fence for
    // every phone-home the daemon makes.
    let body = crate::agent_updates::curl(&url, &["Accept: application/vnd.github+json"]).await?;
    parse_release(&body)
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
/// first (bounded by curl's own timeout) and returns the fresh truth — the
/// UI's "check now". An explicit ask runs even with `update.autoCheck` off
/// or on a dev build: the setting governs the daemon phoning home on its own.
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

    fn release(version: &str) -> Release {
        Release {
            version: version.into(),
            url: "https://example.test/rel".into(),
            published_at: None,
        }
    }

    #[test]
    fn status_json_shape() {
        let status = UpdateStatus {
            checked_at: Some(1_000),
            succeeded_at: Some(1_000),
            latest: Some(release("99.0.0")),
            error: None,
        };
        let json = status.to_json();
        assert_eq!(json["current"], chimaera_core::VERSION);
        assert_eq!(json["latest"]["version"], "99.0.0");
        assert_eq!(json["succeeded_at"], 1_000);
        assert_eq!(json["interval_secs"], 6 * 60 * 60);
        // The workspace dev sentinel is never "outdated" (release_is_newer),
        // so a dev daemon reports available: false even against 99.0.0 — and
        // says it is a dev build rather than posing as up to date.
        let dev = chimaera_core::VERSION == "0.0.1";
        assert_eq!(json["available"], !dev);
        assert_eq!(json["dev"], dev);
        assert_eq!(json["state"], if dev { "current" } else { "available" });
        // No check yet = empty status, honestly null.
        let empty = UpdateStatus::default().to_json();
        assert_eq!(empty["latest"], serde_json::Value::Null);
        assert_eq!(empty["available"], false);
        assert_eq!(empty["state"], "unchecked");
        assert_eq!(empty["error"], serde_json::Value::Null);
    }

    #[test]
    fn a_failed_check_never_reads_as_up_to_date() {
        // Never succeeded: failed, not current.
        let never = UpdateStatus {
            checked_at: Some(2_000),
            error: Some("Could not resolve host: api.github.com".into()),
            ..UpdateStatus::default()
        };
        assert_eq!(never.state(), "failed");
        assert_eq!(
            never.to_json()["error"],
            "Could not resolve host: api.github.com"
        );

        // Succeeded before, failed since: still failed (the last good answer
        // rides along with its own timestamp), unless it knew of a release —
        // a failed re-check doesn't un-publish it.
        let stale = UpdateStatus {
            checked_at: Some(3_000),
            succeeded_at: Some(1_000),
            latest: Some(release("0.0.1")),
            error: Some("timed out".into()),
        };
        assert_eq!(stale.state(), "failed");
        assert_eq!(stale.to_json()["succeeded_at"], 1_000);
    }

    #[test]
    fn failures_read_as_plain_words() {
        let say = |raw: &str| describe_failure(&anyhow::anyhow!("{raw}"));
        assert_eq!(
            say("curl: (6) Could not resolve host: api.github.com"),
            "Could not resolve host: api.github.com"
        );
        assert_eq!(
            say("curl: (22) The requested URL returned error: 403"),
            "GitHub is rate-limiting this address; the next check will retry"
        );
        assert_eq!(
            say("failed to run curl: No such file or directory (os error 2)"),
            "curl is not available on this host"
        );
        assert_eq!(say("bad release JSON"), "bad release JSON");
        assert_eq!(say(&"x".repeat(500)).len(), 200);
    }
}
