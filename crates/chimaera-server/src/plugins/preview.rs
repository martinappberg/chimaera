//! Seeing a plugin before installing it: what its release's `plugin.toml`
//! says — the author's description, what it adds, the agent-side plugin it
//! asks for — fetched on the user's click, checked, parsed, and never
//! written to disk. The Extensions card's expanded body and the repository
//! form's Preview read it.
//!
//! - `GET /plugins/{pid}/details`: an installed plugin answers its catalog
//!   entry. A first-party plugin with nothing installed answers its pinned
//!   release's `plugin.toml` — the direct download `pinned_release` names,
//!   the same one Install fetches — whose sha256 must be the lock's
//!   `sha256_toml` (422 otherwise). Cached per (id, version) for the
//!   daemon's lifetime: the lock is fixed per binary, so at most one entry
//!   per lock entry.
//! - `POST /plugins/preview {github}`: a repository's latest release, its
//!   `plugin.toml` checked against that release's `SHA256SUMS`. The lock's
//!   own repository answers its pinned release instead — what Install
//!   installs from it. Cached per (repo, tag), the last `PREVIEWS_MAX`.
//!
//! Both answer the installed entry's shape (`manifest_fields`) with
//! `source: "available"`, `installed: false`, `requires` / `recommends`,
//! `release_url` and — when the releases API gives the component's size —
//! `download: {wasm_bytes}`; a gate this daemon would refuse at install is
//! the entry's `fault`. Every fetch rides `releases`' fence (10 s, 1 MiB,
//! the manifest capped again at `TOML_MAX`); nothing here runs at boot or in
//! the checker's loop, and a failure is never cached.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::installed::{normalize_github, DOWNLOAD_MISMATCH, TOML_MAX};
use super::releases::{self, Release};
use super::{Locked, Manifest, Refusal};
use crate::AppState;

/// How many repositories' latest releases stay described.
const PREVIEWS_MAX: usize = 32;

/// What the card says when the release can't be read: its summary (from the
/// lock) is already on screen above this line.
pub(crate) const UNREACHABLE: &str =
    "couldn't reach github.com — the summary above is all we know for now";

/// Described releases (on `Releases`). Hot state: a restart asks again.
#[derive(Default)]
pub(crate) struct Previews {
    /// (id, version) → a first-party plugin's pinned release, described.
    pinned: Mutex<HashMap<(String, String), Value>>,
    /// (repo lowercased, version) → a repository's latest release,
    /// described; most recently used first.
    latest: Mutex<VecDeque<((String, String), Value)>>,
}

impl Previews {
    fn recall(&self, key: &(String, String)) -> Option<Value> {
        let mut latest = crate::lock(&self.latest);
        let at = latest.iter().position(|(k, _)| k == key)?;
        let entry = latest.remove(at)?;
        let value = entry.1.clone();
        latest.push_front(entry);
        Some(value)
    }

    fn remember(&self, key: (String, String), value: Value) {
        let mut latest = crate::lock(&self.latest);
        latest.retain(|(k, _)| *k != key);
        latest.push_front((key, value));
        latest.truncate(PREVIEWS_MAX);
    }
}

/// A release's manifest on the wire, before anything is installed: the
/// installed entry's manifest fields, plus where it comes from.
fn describe(
    state: &AppState,
    m: &Manifest,
    repo: &str,
    pin: Option<&Locked>,
    release: &Release,
) -> Value {
    let mut v = super::manifest_fields(m);
    v["requires"] = super::agent_plugins_json(&m.requires.agent_plugins);
    v["recommends"] = super::agent_plugins_json(&m.recommends.agent_plugins);
    v["source"] = json!("available");
    v["installed"] = json!(false);
    v["first_party"] = json!(pin.is_some());
    v["verified"] = json!(false);
    v["repo"] = json!(repo);
    v["release_url"] = json!(release.url);
    if let Some(l) = pin {
        v["pinned_version"] = json!(l.version);
    }
    if let Some(bytes) = release.wasm_size {
        v["download"] = json!({"wasm_bytes": bytes});
    }
    if let Some(why) = super::gate(m, &state.plugin_catalog.daemon_version()) {
        v["fault"] = json!(why);
    }
    v
}

/// A downloaded `plugin.toml`, read as install would read it.
fn parse(toml: &[u8]) -> Result<Manifest, Refusal> {
    if toml.len() as u64 > TOML_MAX {
        return Err(Refusal::invalid(
            "the release's plugin.toml is too large to be a plugin's",
        ));
    }
    let text = std::str::from_utf8(toml)
        .map_err(|_| Refusal::invalid("the release's plugin.toml is not UTF-8"))?;
    super::parse_manifest(text).map_err(|e| Refusal::invalid(format!("the release's {e}")))
}

/// `pid`'s details: its catalog entry when installed, else its pinned
/// release described.
pub(crate) async fn details(state: &AppState, pid: &str) -> Result<Value, Refusal> {
    if let Some(m) = super::manifest(state, pid) {
        let mut v = super::manifest_json(state, &m);
        v["requires"] = super::agent_plugins_json(&m.requires.agent_plugins);
        v["recommends"] = super::agent_plugins_json(&m.recommends.agent_plugins);
        return Ok(v);
    }
    match super::lock_entry(pid) {
        Some(l) => pinned(state, l).await,
        None => Err(Refusal::not_found("unknown plugin")),
    }
}

/// A first-party plugin's pinned release, described: its `plugin.toml` by
/// direct download, checked against the lock. The component's size is the
/// releases API's to give (the tag's asset list), asked alongside and never
/// waited on for anything else — without it the answer has no `download`.
async fn pinned(state: &AppState, l: &'static Locked) -> Result<Value, Refusal> {
    let key = (l.id.clone(), l.version.clone());
    let previews = &state.plugin_releases.previews;
    if let Some(v) = crate::lock(&previews.pinned).get(&key).cloned() {
        return Ok(v);
    }
    let mut release = releases::pinned_release(state, l);
    let (toml, listed) = tokio::join!(
        releases::fetch_small(&release.toml_url, "the release's plugin.toml"),
        releases::fetch_release(state, &l.repo, Some(&l.version)),
    );
    let toml = toml.map_err(|refusal| {
        tracing::info!(plugin = %l.id, version = %l.version, error = %refusal.message, "plugin details unreachable");
        Refusal::upstream(UNREACHABLE)
    })?;
    let got = crate::fs::sha256_hex(&toml);
    if got != l.sha256_toml {
        tracing::error!(
            plugin = %l.id,
            version = %l.version,
            pinned = %l.sha256_toml,
            %got,
            "plugin details refused: the release's plugin.toml is not the one plugins.lock pins"
        );
        return Err(Refusal::invalid(format!(
            "{}'s description on GitHub isn't the one chimaera approved",
            l.name
        )));
    }
    let m = parse(&toml)?;
    release.wasm_size = listed.ok().and_then(|r| r.wasm_size);
    let v = describe(state, &m, &l.repo, Some(l), &release);
    crate::lock(&previews.pinned).insert(key, v.clone());
    Ok(v)
}

/// A repository's latest release, described (the lock's repository: its
/// pinned release, which is what Install installs from it).
pub(crate) async fn preview(state: &AppState, typed: &str) -> Result<Value, Refusal> {
    let github = normalize_github(typed);
    if github.is_empty() {
        return Err(Refusal::bad_request("name a repository (owner/repo)"));
    }
    if let Some(l) = super::lock_entries()
        .iter()
        .find(|l| l.repo.eq_ignore_ascii_case(github))
    {
        return pinned(state, l).await;
    }
    let release = releases::fetch_release(state, github, None).await?;
    let key = (github.to_ascii_lowercase(), release.version.clone());
    let previews = &state.plugin_releases.previews;
    if let Some(v) = previews.recall(&key) {
        return Ok(v);
    }
    let unreachable = |refusal: Refusal| {
        tracing::info!(%github, version = %release.version, error = %refusal.message, "plugin preview: a release file unreadable");
        Refusal::upstream(format!(
            "couldn't download {github}'s release from github.com — try again later"
        ))
    };
    let sums = releases::fetch_small(&release.sums_url, "SHA256SUMS")
        .await
        .map_err(unreachable)?;
    let Some(want) = releases::parse_sums(&String::from_utf8_lossy(&sums))
        .get("plugin.toml")
        .cloned()
    else {
        return Err(Refusal::invalid(
            "that release doesn't list its plugin files, so it can't be checked or installed",
        ));
    };
    let toml = releases::fetch_small(&release.toml_url, "the release's plugin.toml")
        .await
        .map_err(unreachable)?;
    let got = crate::fs::sha256_hex(&toml);
    if got != want {
        tracing::error!(%github, version = %release.version, expected = %want, %got, "plugin preview refused: plugin.toml does not match the release's SHA256SUMS");
        return Err(Refusal::invalid(DOWNLOAD_MISMATCH));
    }
    let m = parse(&toml)?;
    if m.version != release.version {
        return Err(Refusal::invalid(format!(
            "it is tagged v{} but its plugin.toml says {}",
            release.version, m.version
        )));
    }
    let v = describe(state, &m, github, None, &release);
    previews.remember(key, v.clone());
    Ok(v)
}

/// GET /plugins/{pid}/details — everything the card shows, before install
/// too.
pub(crate) async fn details_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    match details(&state, &pid).await {
        Ok(v) => Json(v).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct PreviewBody {
    /// `owner/repo` (a `https://github.com/owner/repo` URL is read as one).
    #[serde(default)]
    github: Option<String>,
}

/// POST /plugins/preview {github} — a repository's plugin as its latest
/// release describes it. Nothing is written.
pub(crate) async fn preview_route(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PreviewBody>,
) -> Response {
    match preview(&state, body.github.as_deref().unwrap_or("")).await {
        Ok(v) => Json(v).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_previews_are_bounded_and_most_recent_first() {
        let previews = Previews::default();
        let key = |i: usize| (format!("acme/p{i}"), "0.1.0".to_string());
        for i in 0..PREVIEWS_MAX + 5 {
            previews.remember(key(i), json!(i));
        }
        assert_eq!(crate::lock(&previews.latest).len(), PREVIEWS_MAX);
        assert_eq!(previews.recall(&key(0)), None, "the oldest went");
        // A recall moves it to the front: it outlives the next insert.
        let oldest_kept = key(5);
        assert_eq!(previews.recall(&oldest_kept), Some(json!(5)));
        previews.remember(key(999), json!(999));
        assert_eq!(previews.recall(&oldest_kept), Some(json!(5)));
        assert_eq!(previews.recall(&key(6)), None);
        // The same key again replaces, never duplicates.
        previews.remember(key(999), json!("again"));
        assert_eq!(crate::lock(&previews.latest).len(), PREVIEWS_MAX);
        assert_eq!(previews.recall(&key(999)), Some(json!("again")));
    }
}
