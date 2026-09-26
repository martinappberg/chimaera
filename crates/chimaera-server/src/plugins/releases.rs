//! Plugin releases: where a newer version of an installed plugin comes from,
//! and whether one exists. Design: docs/plugin-system-plan.md ("Versions and
//! updates").
//!
//! An installed plugin's manifest may name `[release] github = "owner/repo"`:
//! the GitHub releases API, tags `v<version>`, assets `plugin.wasm`,
//! `plugin.toml` and `SHA256SUMS`. The checker asks each such source for its
//! latest release, reads that release's `plugin.toml` (small) and offers the
//! version only when it is strictly newer than what runs here AND passes
//! this daemon's gates. It never downloads a component on its own: that is
//! the user's click (`installed`). Cadence: the daemon's own release checker
//! (`update::run_checker`) runs `check_all` once after boot and then daily;
//! `POST /plugins/{pid}/check` asks at once. Offers live in memory.
//!
//! Every fetch here is `agent_updates::curl` (10 s, 1 MiB): the fence every
//! phone-home the daemon makes shares. `CHIMAERA_PLUGIN_RELEASES_API`
//! replaces the API base (`{base}/{owner}/{repo}/releases/latest`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::{Manifest, Refusal};
use crate::AppState;

/// How often an installed plugin's source is asked, at most (Check now
/// aside): plugins release on their own cadence, a day is fresh enough.
pub(crate) const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// The GitHub API's repos base; `CHIMAERA_PLUGIN_RELEASES_API` replaces it.
const GITHUB_API: &str = "https://api.github.com/repos";
const ACCEPT_GITHUB: &str = "Accept: application/vnd.github+json";

/// The daemon's plugin-release knowledge (on `AppState`). Hot state.
#[derive(Default)]
pub(crate) struct Releases {
    /// plugin id → the newer, gate-passing release a check found.
    offers: Mutex<HashMap<String, Offer>>,
    /// Tests only: the API base (the env knob is process-wide, and tests
    /// run in parallel).
    api_override: Mutex<Option<String>>,
    /// One install, update, rollback or remove at a time, daemon-wide: they
    /// move links under one directory and reload one catalog.
    pub(crate) changing: tokio::sync::Mutex<()>,
}

/// A newer release a check found (the card's Update chip).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Offer {
    pub(crate) version: String,
    /// The release's page, for humans.
    pub(crate) url: String,
    pub(crate) checked_ms: u64,
}

/// One release, as the releases API describes it.
#[derive(Debug)]
pub(crate) struct Release {
    /// The tag without its `v`.
    pub(crate) version: String,
    pub(crate) url: String,
    pub(crate) wasm_url: String,
    pub(crate) toml_url: String,
    pub(crate) sums_url: String,
}

impl Releases {
    /// Tests only: point this daemon's plugin release fetches at `base`.
    #[cfg(test)]
    pub(crate) fn set_api_for_tests(&self, base: &str) {
        *crate::lock(&self.api_override) = Some(base.to_string());
    }

    fn api_base(&self) -> String {
        if let Some(base) = crate::lock(&self.api_override).clone() {
            return base;
        }
        std::env::var("CHIMAERA_PLUGIN_RELEASES_API").unwrap_or_else(|_| GITHUB_API.to_string())
    }

    /// Drop what is known about `id` (it was removed).
    pub(crate) fn forget(&self, id: &str) {
        crate::lock(&self.offers).remove(id);
    }
}

/// The offer for `m`, if a check found a release newer than the version
/// that runs now (an install since then may have caught up with it).
pub(crate) fn offer_for(state: &AppState, m: &Manifest) -> Option<Offer> {
    let offer = crate::lock(&state.plugin_releases.offers)
        .get(&m.id)
        .cloned()?;
    newer(&offer.version, &m.version).then_some(offer)
}

/// `candidate` is strictly newer than `running` (both plain semver; an
/// unreadable side never claims an update).
pub(crate) fn newer(candidate: &str, running: &str) -> bool {
    match (
        super::plugin_version(candidate),
        super::plugin_version(running),
    ) {
        (Ok(c), Ok(r)) => c > r,
        _ => false,
    }
}

/// Ask `github`'s releases API for its latest release, or the one tagged
/// `v{version}`.
pub(crate) async fn fetch_release(
    state: &AppState,
    github: &str,
    version: Option<&str>,
) -> Result<Release, Refusal> {
    if !super::valid_github(github) {
        return Err(Refusal::bad_request(format!(
            "{github:?} is not a GitHub owner/repo"
        )));
    }
    let base = state.plugin_releases.api_base();
    let base = base.trim_end_matches('/');
    let url = match version {
        None => format!("{base}/{github}/releases/latest"),
        Some(v) => {
            super::plugin_version(v).map_err(Refusal::bad_request)?;
            format!("{base}/{github}/releases/tags/v{v}")
        }
    };
    let body = crate::agent_updates::curl(&url, &[ACCEPT_GITHUB])
        .await
        .map_err(|e| Refusal::upstream(format!("could not read {github}'s releases: {e:#}")))?;
    let release = parse_release(&body, github).map_err(Refusal::upstream)?;
    if let Some(v) = version {
        if release.version != v {
            return Err(Refusal::upstream(format!(
                "{github} answered v{} for v{v}",
                release.version
            )));
        }
    }
    Ok(release)
}

/// Parse a releases-API payload: the tag (`v<MAJOR.MINOR.PATCH>`), the page,
/// and the three assets by name. Asset URLs must be http(s): they are
/// handed to curl.
pub(crate) fn parse_release(body: &[u8], github: &str) -> Result<Release, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "the release is not JSON".to_string())?;
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("the release has no tag_name")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    super::plugin_version(version)
        .map_err(|_| format!("the release tag {tag:?} is not v<MAJOR.MINOR.PATCH>"))?;
    let url = value
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|u| web_url(u))
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://github.com/{github}/releases/tag/{tag}"));
    let assets = value
        .get("assets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let asset = |name: &str| -> Result<String, String> {
        assets
            .iter()
            .find(|a| a.get("name").and_then(Value::as_str) == Some(name))
            .and_then(|a| a.get("browser_download_url").and_then(Value::as_str))
            .filter(|u| web_url(u))
            .map(str::to_string)
            .ok_or_else(|| format!("the {tag} release has no {name} asset"))
    };
    Ok(Release {
        version: version.to_string(),
        url,
        wasm_url: asset("plugin.wasm")?,
        toml_url: asset("plugin.toml")?,
        sums_url: asset("SHA256SUMS")?,
    })
}

fn web_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// A small release asset (a manifest, the checksum list) into memory,
/// through the shared 10 s / 1 MiB fence.
pub(crate) async fn fetch_small(url: &str, what: &str) -> Result<Vec<u8>, Refusal> {
    crate::agent_updates::curl(url, &[])
        .await
        .map_err(|e| Refusal::upstream(format!("could not download {what}: {e:#}")))
}

/// `SHA256SUMS` (the `sha256sum` output format: `<hex>  <name>`, or
/// `<hex> *<name>` for binary mode) → name → lowercase hex.
pub(crate) fn parse_sums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (hex, name) = line.trim().split_once(char::is_whitespace)?;
            let name = name.trim_start().trim_start_matches('*');
            let name = name.strip_prefix("./").unwrap_or(name);
            let hex = hex.to_ascii_lowercase();
            (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| (name.trim().to_string(), hex))
        })
        .collect()
}

/// Ask `id`'s release source whether a newer version exists, and remember
/// the answer. `Ok(None)`: nothing newer that this daemon can run.
pub(crate) async fn check(state: &Arc<AppState>, id: &str) -> Result<Option<Offer>, Refusal> {
    let Some(m) = super::manifest(state, id) else {
        return Err(Refusal::not_found("unknown plugin"));
    };
    let Some(github) = m.origin.installed_release.clone() else {
        return Err(Refusal::conflict(if m.origin.installed_version.is_none() {
            format!(
                "{} ships with chimaera — it updates with chimaera itself",
                m.name
            )
        } else {
            format!(
                "{} names no release source ([release] in its plugin.toml)",
                m.name
            )
        }));
    };
    let release = fetch_release(state, &github, None).await?;
    let offer = if newer(&release.version, &m.version) {
        let text = fetch_small(&release.toml_url, "the release's plugin.toml").await?;
        match compatible(state, id, &release, &text) {
            Ok(()) => Some(Offer {
                version: release.version.clone(),
                url: release.url.clone(),
                checked_ms: crate::timeline::now_ms(),
            }),
            Err(why) => {
                tracing::info!(plugin = %id, version = %release.version, %why, "newer plugin release not offered");
                None
            }
        }
    } else {
        None
    };
    let changed = {
        let mut offers = crate::lock(&state.plugin_releases.offers);
        let before = offers.get(id).map(|o| o.version.clone());
        match &offer {
            Some(o) => {
                offers.insert(id.to_string(), o.clone());
            }
            None => {
                offers.remove(id);
            }
        }
        before != offer.as_ref().map(|o| o.version.clone())
    };
    if changed {
        state.changes.notify_waiters();
    }
    Ok(offer)
}

/// Whether a release's `plugin.toml` describes a version of `id` this
/// daemon can run: the same id, the tag's version, the gates.
pub(crate) fn compatible(
    state: &AppState,
    id: &str,
    release: &Release,
    toml: &[u8],
) -> Result<(), String> {
    let text = std::str::from_utf8(toml).map_err(|_| "its plugin.toml is not UTF-8".to_string())?;
    let m = super::parse_manifest(text)?;
    if m.id != id {
        return Err(format!("its plugin.toml is for {:?}, not {id:?}", m.id));
    }
    if m.version != release.version {
        return Err(format!(
            "it is tagged v{} but its plugin.toml says {}",
            release.version, m.version
        ));
    }
    match super::gate(&m, &state.plugin_catalog.daemon_version()) {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// Every installed plugin with a release source, asked once (sequentially:
/// a handful of small requests a day). Failures are debug-logged — an
/// air-gapped cluster failing a check is normal life.
pub(crate) async fn check_all(state: &Arc<AppState>) {
    let ids: Vec<String> = super::catalog(state)
        .iter()
        .filter(|m| m.origin.installed_release.is_some())
        .map(|m| m.id.clone())
        .collect();
    for id in ids {
        if let Err(err) = check(state, &id).await {
            tracing::debug!(plugin = %id, error = %err.message, "plugin release check failed");
        }
    }
}

/// POST /plugins/{pid}/check — Check now: ask the plugin's release source
/// at once. Answers the offer (or null) and the plugin's catalog entry.
pub(crate) async fn check_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    match check(&state, &pid).await {
        Ok(offer) => {
            let plugin = super::manifest(&state, &pid).map(|m| super::manifest_json(&state, &m));
            Json(json!({
                "id": pid,
                "update": offer.map(|o| json!({
                    "version": o.version,
                    "url": o.url,
                    "checked_ms": o.checked_ms,
                })),
                "plugin": plugin,
            }))
            .into_response()
        }
        Err(refusal) => refusal.into_response(),
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}

impl Refusal {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Refusal {
            status,
            message: message.into(),
        }
    }
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
    /// The release is readable but not installable (checksum, manifest,
    /// gate).
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, message)
    }
    /// The release source could not be reached or read.
    pub(crate) fn upstream(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, message)
    }
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_release_and_its_three_assets() {
        let body = br#"{
            "tag_name": "v0.3.2",
            "html_url": "https://github.com/acme/demo/releases/tag/v0.3.2",
            "assets": [
                {"name": "plugin.wasm", "browser_download_url": "https://x/plugin.wasm"},
                {"name": "plugin.toml", "browser_download_url": "https://x/plugin.toml"},
                {"name": "SHA256SUMS", "browser_download_url": "https://x/SHA256SUMS"}
            ]
        }"#;
        let r = parse_release(body, "acme/demo").unwrap();
        assert_eq!(r.version, "0.3.2");
        assert!(r.url.ends_with("/tag/v0.3.2"));
        assert_eq!(r.wasm_url, "https://x/plugin.wasm");
        assert_eq!(r.sums_url, "https://x/SHA256SUMS");

        let no_sums = br#"{"tag_name": "v0.3.2", "assets": [
            {"name": "plugin.wasm", "browser_download_url": "https://x/w"},
            {"name": "plugin.toml", "browser_download_url": "https://x/t"}]}"#;
        assert!(parse_release(no_sums, "acme/demo")
            .unwrap_err()
            .contains("SHA256SUMS"));
        let local = br#"{"tag_name": "v1.0.0", "assets": [
            {"name": "plugin.wasm", "browser_download_url": "file:///etc/passwd"},
            {"name": "plugin.toml", "browser_download_url": "https://x/t"},
            {"name": "SHA256SUMS", "browser_download_url": "https://x/s"}]}"#;
        assert!(parse_release(local, "acme/demo").is_err(), "http(s) only");
        assert!(parse_release(br#"{"tag_name": "nightly"}"#, "acme/demo").is_err());
        assert!(parse_release(b"<html>", "acme/demo").is_err());
    }

    #[test]
    fn sums_read_the_sha256sum_format() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = parse_sums(&format!(
            "{a}  plugin.wasm\n{b} *plugin.toml\nnot a line\n{a}  ./other\n"
        ));
        assert_eq!(sums.get("plugin.wasm"), Some(&a));
        assert_eq!(sums.get("plugin.toml"), Some(&"b".repeat(64)));
        assert_eq!(sums.get("other"), Some(&a));
        assert_eq!(sums.len(), 3);
    }

    #[test]
    fn only_a_strictly_newer_release_is_newer() {
        assert!(newer("0.3.2", "0.3.1"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(!newer("0.3.1", "0.3.1"));
        assert!(!newer("0.3.0", "0.3.1"));
        assert!(!newer("0.4.0-beta.1", "0.3.1"), "never guess");
        assert!(!newer("x", "0.3.1"));
    }
}
