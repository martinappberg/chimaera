//! Workbench plugins: opt-in add-ons that say exactly what they add.
//! Design: docs/timeline-knowledge-plugins-plan.md §6; the plugin host:
//! docs/plugin-system-plan.md.
//!
//! A plugin is a small TOML manifest (data, never code) plus its behaviour:
//! a WASM component (`plugin.wasm`) built from `plugins/<crate>` by
//! `scripts/build-plugins.sh` and embedded from `plugins/dist`, like
//! `web-ui/dist`, or installed from a plugin's release into
//! `<data dir>/plugins/<id>/<version>/` (`installed`). It runs in `runtime`
//! (the sandbox) and asks the daemon for everything through `hostfns`
//! (bounded). No plugin behaviour is daemon code: the Knowledge provider
//! (mycelium) answers `knowledge.rs` through its `knowledge` export like any
//! other plugin would.
//!
//! The catalog (`Catalog`, on `AppState`) merges the embedded plugins with
//! the installed copies: the same id in both → the higher version loads and
//! the card names both; equal → the embedded copy; an older installed copy
//! is `stale`. Gates run before anything loads: a manifest's `api` must be a
//! WIT version this host serves and its `requires.chimaera` must match this
//! daemon — a plugin that fails one stays listed, off, with the reason. The
//! catalog reloads after every install, update, rollback and remove.
//!
//! State is minimal by design: a plugin is switched on per workspace
//! (`Workspace.plugins_on` — the Plugins page is per workspace, so is its
//! switch), and it is *active* there when it is on AND its `detect` paths are
//! present. Detection is cached per workspace with a short TTL; stat work
//! always runs off the reactor, and a stale entry is refreshed (awaited)
//! before any answer that decides what an agent sees — a session connecting
//! right after the switch flips must get the tools, not a cold-cache miss.
//!
//! The invariant this module exists to keep: with no plugin active, nothing
//! an agent sees changes (MCP tools/list, instructions, generated settings).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

pub(crate) mod hostfns;
pub(crate) mod installed;
pub(crate) mod releases;
pub(crate) mod runtime;
pub(crate) mod tools;

/// How long a workspace's detect result is trusted before a re-stat.
const DETECT_TTL: Duration = Duration::from_secs(30);

/// The WIT version (`chimaera:plugin@0.1.x`) first-party plugins target.
pub(crate) const API: &str = "0.1";
/// Every WIT version this host serves. A manifest's `api` must be one of
/// them; an additive WIT bump adds a version here and keeps the old ones.
pub(crate) const SERVED_APIS: &[&str] = &[API];

/// The first-party WASM plugins, `<id>/{plugin.toml,plugin.wasm}`, built by
/// `scripts/build-plugins.sh`. Release builds embed the folder; debug builds
/// read it from disk when the catalog loads (rust-embed's debug mode).
#[derive(RustEmbed)]
#[folder = "../../plugins/dist"]
struct Dist;

/// The test-only plugins (the host's fixture): embedded by test builds only.
#[cfg(test)]
#[derive(RustEmbed)]
#[folder = "../../plugins/dist-test"]
pub(crate) struct DistTest;

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub(crate) id: String,
    pub(crate) name: String,
    /// The plugin's own version, `MAJOR.MINOR.PATCH` (`validate`).
    pub(crate) version: String,
    pub(crate) summary: String,
    #[serde(default)]
    pub(crate) homepage: Option<String>,
    /// The WIT version the component targets (`"0.1"`); a gate.
    pub(crate) api: String,
    #[serde(default)]
    pub(crate) detect: Detect,
    #[serde(default)]
    pub(crate) requires: Requires,
    #[serde(default)]
    pub(crate) setup: Option<Setup>,
    #[serde(default)]
    pub(crate) provides: Provides,
    #[serde(default)]
    pub(crate) adds: Adds,
    /// Where newer versions are published (the release checker's source).
    #[serde(default)]
    pub(crate) release: Option<ReleaseSource>,
    /// The behaviour — set by the catalog, never by the TOML.
    #[serde(skip)]
    pub(crate) wasm: Wasm,
    /// Where this copy came from and what the catalog decided about it —
    /// set by the catalog, never by the TOML.
    #[serde(skip)]
    pub(crate) origin: Origin,
}

/// A plugin's component (`plugin.wasm`), which the runtime runs, and its
/// SHA-256: which build it is (the compile cache and the live instances are
/// keyed by it, so a moved `current` never runs the old code).
#[derive(Clone, Default)]
pub(crate) struct Wasm {
    pub(crate) bytes: Arc<Cow<'static, [u8]>>,
    pub(crate) sha256: Arc<str>,
}

impl Wasm {
    pub(crate) fn new(bytes: Cow<'static, [u8]>) -> Self {
        let sha256 = Arc::from(crate::fs::sha256_hex(&bytes));
        Wasm {
            bytes: Arc::new(bytes),
            sha256,
        }
    }
}

impl std::fmt::Debug for Wasm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Wasm({} bytes)", self.bytes.len())
    }
}

/// Where a catalog entry came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Source {
    /// Ships inside this chimaera binary (`plugins/dist`).
    #[default]
    Embedded,
    /// Installed from a release into `<data dir>/plugins/<id>/`.
    Installed,
}

impl Source {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Source::Embedded => "embedded",
            Source::Installed => "installed",
        }
    }
}

/// The catalog's decision about one plugin id, carried on the manifest that
/// loads (or would load) for it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Origin {
    pub(crate) source: Source,
    /// The installed copy's `current` version directory, when it has one.
    pub(crate) path: Option<PathBuf>,
    pub(crate) embedded_version: Option<String>,
    pub(crate) installed_version: Option<String>,
    /// The installed copy's `previous` version (Use previous).
    pub(crate) previous: Option<String>,
    /// An installed copy older than the embedded one (the embedded loads).
    pub(crate) stale: bool,
    /// Why this daemon can't run it (a failed gate): listed, never active.
    pub(crate) gate: Option<String>,
    /// The installed copy's release source — what the checker asks, even
    /// when the embedded copy is the one that loads.
    pub(crate) installed_release: Option<String>,
}

/// Workspace-relative paths whose presence makes the plugin active here.
/// Empty = always present (a plugin with no project footprint).
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Detect {
    #[serde(default)]
    pub(crate) any: Vec<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Requires {
    /// agent kind ("claude" | "codex") → the agent-native plugin it needs.
    #[serde(default)]
    pub(crate) agent_plugins: BTreeMap<String, AgentPluginReq>,
    /// A semver requirement on this daemon (`">=0.4.0"`, `"^0.4"`) for a
    /// plugin that needs a host import a later daemon added; a gate.
    #[serde(default)]
    pub(crate) chimaera: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentPluginReq {
    /// The agent's own plugin id, e.g. "mycelium@mycelium".
    pub(crate) id: String,
    /// What the agent's `marketplace add` takes (owner/repo or a URL).
    pub(crate) marketplace: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct Setup {
    /// The plugin's own documented setup prompt, sent to an agent session the
    /// user picks (their click, their billing).
    pub(crate) prompt: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Provides {
    /// The plugin is a Knowledge provider: its `knowledge` export fills the
    /// Knowledge view, and this name is the route's `provider` (`"mycelium"`).
    #[serde(default)]
    pub(crate) knowledge: Option<String>,
    /// Named MCP tools served only where the plugin is active.
    #[serde(default)]
    pub(crate) mcp_tools: Vec<String>,
    /// Named first-party UI modules (lazy-loaded by the web UI).
    #[serde(default)]
    pub(crate) views: Vec<String>,
    /// The events the host delivers to the plugin's `on-event`: nothing it
    /// did not declare, so a hook never instantiates a plugin that ignores it.
    #[serde(default)]
    pub(crate) events: Vec<EventKind>,
}

impl Provides {
    pub(crate) fn hears(&self, event: EventKind) -> bool {
        self.events.contains(&event)
    }
}

/// The `on-event` variants a manifest may declare (`provides.events`).
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum EventKind {
    Hook,
    SessionEnded,
    SwitchedOn,
    SwitchedOff,
}

/// The card's "Adds" lines, in words — mandatory honesty, not decoration.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Adds {
    #[serde(default)]
    pub(crate) ui: Vec<String>,
    #[serde(default)]
    pub(crate) agents: Vec<String>,
}

/// `[release]`: where a plugin's newer versions are published. The GitHub
/// releases API, tags `v<version>`, assets `plugin.wasm`, `plugin.toml` and
/// `SHA256SUMS`.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseSource {
    /// `owner/repo`.
    pub(crate) github: String,
}

/// A plugin id: lowercase ASCII letters, digits and dashes, starting with a
/// letter or digit. It names a directory and a URL segment, so nothing
/// else; `install` is the install route's own segment.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id != "install"
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A GitHub `owner/repo`: what the checker and the install route put in an
/// API URL, so nothing that could leave that path.
pub(crate) fn valid_github(slug: &str) -> bool {
    let mut parts = slug.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && p.len() <= 100
                && !p.starts_with('.')
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

/// A plugin version: plain `MAJOR.MINOR.PATCH` (no pre-release or build
/// part — the precedence rule and "strictly newer" compare these).
pub(crate) fn plugin_version(v: &str) -> Result<semver::Version, String> {
    match semver::Version::parse(v) {
        Ok(parsed) if parsed.pre.is_empty() && parsed.build.is_empty() => Ok(parsed),
        _ => Err(format!("version {v:?} is not MAJOR.MINOR.PATCH")),
    }
}

/// The manifest's own consistency, checked wherever one is read: a readable
/// id, version and release source. (The gates are separate: a manifest can
/// be well-formed and still not run on this daemon.)
pub(crate) fn validate(m: &Manifest) -> Result<(), String> {
    if !valid_id(&m.id) {
        return Err(format!(
            "plugin id {:?} must be lowercase letters, digits and dashes",
            m.id
        ));
    }
    plugin_version(&m.version)?;
    if let Some(release) = &m.release {
        if !valid_github(&release.github) {
            return Err(format!(
                "release.github {:?} is not owner/repo",
                release.github
            ));
        }
    }
    Ok(())
}

/// Parse and validate a manifest's text.
pub(crate) fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let m = toml::from_str::<Manifest>(text).map_err(|e| format!("plugin.toml: {e}"))?;
    validate(&m)?;
    Ok(m)
}

/// The compatibility gates: `api` must be a WIT version this host serves,
/// and `requires.chimaera` must match `daemon` (a dev build, the `0.0.1`
/// sentinel, matches every requirement — as the release check treats it).
/// `Some(why)` in the card's words when this daemon can't run `m`.
pub(crate) fn gate(m: &Manifest, daemon: &str) -> Option<String> {
    if !SERVED_APIS.contains(&m.api.as_str()) {
        let served = SERVED_APIS.join(", ");
        let wants = api_parts(&m.api);
        let newest = SERVED_APIS.iter().filter_map(|a| api_parts(a)).max();
        return Some(match (wants, newest) {
            (Some(w), Some(n)) if w > n => format!(
                "needs a newer chimaera: it targets plugin API {}, this daemon serves {served}",
                m.api
            ),
            (Some(_), _) => format!(
                "needs a newer plugin: it targets plugin API {}, this daemon serves {served}",
                m.api
            ),
            (None, _) => format!("its api {:?} is not a plugin API version", m.api),
        });
    }
    let req = m.requires.chimaera.as_deref()?;
    let Ok(parsed) = semver::VersionReq::parse(req) else {
        return Some(format!(
            "its requires.chimaera ({req}) is not a version requirement"
        ));
    };
    if chimaera_core::version_is_dev(daemon) {
        return None;
    }
    let Ok(version) = semver::Version::parse(daemon) else {
        return None;
    };
    (!parsed.matches(&version)).then(|| {
        format!(
            "needs chimaera {} (this is {daemon})",
            describe_req(&parsed)
        )
    })
}

/// `"0.1"` → (0, 1).
fn api_parts(api: &str) -> Option<(u64, u64)> {
    let (major, minor) = api.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// A requirement in the card's words: `>=0.4.0` reads "≥ 0.4.0".
fn describe_req(req: &semver::VersionReq) -> String {
    req.comparators
        .iter()
        .map(|c| {
            let text = c.to_string();
            match c.op {
                semver::Op::GreaterEq => format!("≥ {}", text.trim_start_matches(">=")),
                semver::Op::LessEq => format!("≤ {}", text.trim_start_matches("<=")),
                _ => text,
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every `<id>/plugin.toml` + `<id>/plugin.wasm` pair in an embedded dist
/// folder, by id. A pair that doesn't hold together (no component, an id
/// that isn't its folder's, a manifest that doesn't parse) is left out,
/// loudly: unreachable for a dist this build's script laid out.
fn load_dist<E: RustEmbed>() -> Vec<Arc<Manifest>> {
    let mut dirs: Vec<String> = E::iter()
        .filter_map(|path| path.strip_suffix("/plugin.toml").map(str::to_string))
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs.into_iter()
        .filter_map(|dir| {
            let text = E::get(&format!("{dir}/plugin.toml"))?;
            let mut m = match parse_manifest(&String::from_utf8_lossy(&text.data)) {
                Ok(m) => m,
                // Unreachable in a tested build (`every_manifest_parses`); a
                // broken manifest must not take the daemon down with it.
                Err(err) => {
                    tracing::error!(plugin = %dir, %err, "invalid embedded plugin manifest");
                    return None;
                }
            };
            let Some(wasm) = E::get(&format!("{dir}/plugin.wasm")) else {
                tracing::error!(plugin = %dir, "embedded plugin has no plugin.wasm");
                return None;
            };
            if m.id != dir {
                tracing::error!(
                    plugin = %dir,
                    id = %m.id,
                    "embedded plugin refused: its id must be its folder"
                );
                return None;
            }
            m.wasm = Wasm::new(wasm.data);
            Some(Arc::new(m))
        })
        .collect()
}

/// What this daemon ships: the embedded plugins, sorted by id (their folder
/// names). Loaded once per process.
static PRODUCTION: LazyLock<Vec<Arc<Manifest>>> = LazyLock::new(load_dist::<Dist>);

/// The shipped catalog — never the test fixtures, never an installed copy.
pub(crate) fn production_catalog() -> &'static [Arc<Manifest>] {
    &PRODUCTION
}

/// The embedded plugins: the shipped ones plus, in a test build, whatever
/// `test_catalog` added.
fn embedded() -> Vec<Arc<Manifest>> {
    #[allow(unused_mut)]
    let mut all: Vec<Arc<Manifest>> = production_catalog().to_vec();
    #[cfg(test)]
    all.extend(test_catalog::extra());
    all
}

/// Merge the embedded plugins with the installed copies, one entry per id,
/// sorted by id (the Plugins page's order, and the order active plugins'
/// tools and instruction paragraphs reach an agent in). The same id in
/// both: the higher version loads and both versions are named; equal
/// versions: the embedded copy; an older installed copy is `stale`. Then
/// the gates, on the copy that loads.
pub(crate) fn resolve(
    embedded: &[Arc<Manifest>],
    installed: &[installed::InstalledCopy],
    daemon: &str,
) -> Vec<Arc<Manifest>> {
    let version =
        |m: &Manifest| plugin_version(&m.version).unwrap_or(semver::Version::new(0, 0, 0));
    let mut ids: BTreeSet<&str> = embedded.iter().map(|m| m.id.as_str()).collect();
    ids.extend(installed.iter().map(|c| c.manifest.id.as_str()));
    ids.into_iter()
        .map(|id| {
            let e = embedded.iter().find(|m| m.id == id);
            let i = installed.iter().find(|c| c.manifest.id == id);
            let installed_wins = match (e, i) {
                (Some(e), Some(i)) => version(&i.manifest) > version(e),
                (None, Some(_)) => true,
                _ => false,
            };
            let mut chosen: Manifest = match (installed_wins, e, i) {
                (true, _, Some(i)) => i.manifest.clone(),
                (_, Some(e), _) => (**e).clone(),
                _ => unreachable!("every id came from one side"),
            };
            chosen.origin = Origin {
                source: if installed_wins {
                    Source::Installed
                } else {
                    Source::Embedded
                },
                path: i.map(|c| c.dir.clone()),
                embedded_version: e.map(|m| m.version.clone()),
                installed_version: i.map(|c| c.manifest.version.clone()),
                previous: i.and_then(|c| c.previous.clone()),
                stale: matches!((e, i), (Some(e), Some(i)) if version(&i.manifest) < version(e)),
                gate: gate(&chosen, daemon),
                installed_release: i
                    .and_then(|c| c.manifest.release.as_ref().map(|r| r.github.clone())),
            };
            Arc::new(chosen)
        })
        .collect()
}

/// The catalog every lookup reads (on `AppState`): the embedded plugins
/// merged with the installed copies under `<data dir>/plugins`. Reloaded
/// (`reload`, blocking — off the reactor) after an install, update,
/// rollback or remove, so a change takes effect without a restart.
pub(crate) struct Catalog {
    /// `<data dir>/plugins`.
    pub(crate) root: PathBuf,
    /// The daemon version the gates compare against (tests pin another).
    daemon: RwLock<String>,
    installed: RwLock<Arc<Vec<installed::InstalledCopy>>>,
    /// The merged catalog, and (test builds) how many `test_catalog` extras
    /// it was merged with — extras added later trigger a re-merge.
    merged: RwLock<(usize, Arc<Vec<Arc<Manifest>>>)>,
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Catalog {
    /// Load the catalog for `root` (blocking: reads the installed copies;
    /// called once at boot, where the daemon's other stores load too).
    pub(crate) fn load(root: PathBuf) -> Self {
        let catalog = Catalog {
            root,
            daemon: RwLock::new(chimaera_core::VERSION.to_string()),
            installed: RwLock::new(Arc::new(Vec::new())),
            merged: RwLock::new((usize::MAX, Arc::new(Vec::new()))),
        };
        catalog.reload();
        catalog
    }

    /// Re-read the installed copies and re-merge (blocking fs: callers on
    /// the reactor go through `spawn_blocking`).
    pub(crate) fn reload(&self) {
        let copies = Arc::new(installed::scan(&self.root));
        *write(&self.installed) = copies;
        self.remerge();
    }

    fn remerge(&self) {
        let embedded = embedded();
        let copies = read(&self.installed).clone();
        let daemon = read(&self.daemon).clone();
        let all = Arc::new(resolve(&embedded, &copies, &daemon));
        *write(&self.merged) = (extras_len(), all);
    }

    /// Every plugin, sorted by id.
    pub(crate) fn all(&self) -> Arc<Vec<Arc<Manifest>>> {
        {
            let merged = read(&self.merged);
            if merged.0 == extras_len() {
                return merged.1.clone();
            }
        }
        self.remerge();
        read(&self.merged).1.clone()
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<Manifest>> {
        self.all().iter().find(|m| m.id == id).cloned()
    }

    /// The installed copy of `id` as found on disk, if any.
    pub(crate) fn installed_copy(&self, id: &str) -> Option<installed::InstalledCopy> {
        read(&self.installed)
            .iter()
            .find(|c| c.manifest.id == id)
            .cloned()
    }

    /// The version the gates compare against.
    pub(crate) fn daemon_version(&self) -> String {
        read(&self.daemon).clone()
    }

    /// Tests only: gate against another daemon version (a test build is the
    /// `0.0.1` dev sentinel, which every requirement matches).
    #[cfg(test)]
    pub(crate) fn set_daemon_version_for_tests(&self, version: &str) {
        *write(&self.daemon) = version.to_string();
        self.remerge();
    }
}

#[cfg(test)]
fn extras_len() -> usize {
    test_catalog::extra().len()
}

#[cfg(not(test))]
fn extras_len() -> usize {
    0
}

/// Why a plugin change or check was refused, as its route answers it
/// (`{"error": message}` with `status`; constructors in `releases`).
#[derive(Debug)]
pub(crate) struct Refusal {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
}

/// The catalog every lookup reads.
pub(crate) fn catalog(state: &AppState) -> Arc<Vec<Arc<Manifest>>> {
    state.plugin_catalog.all()
}

pub(crate) fn manifest(state: &AppState, id: &str) -> Option<Arc<Manifest>> {
    state.plugin_catalog.get(id)
}

/// Test builds only: plugins added to this test process's catalog as if
/// embedded, beside the shipped ones (every `Catalog` sees them;
/// `production_catalog()` never does).
#[cfg(test)]
pub(crate) mod test_catalog {
    use super::*;

    static EXTRA: std::sync::Mutex<Vec<Arc<Manifest>>> = std::sync::Mutex::new(Vec::new());

    pub(crate) fn extra() -> Vec<Arc<Manifest>> {
        crate::lock(&EXTRA).clone()
    }

    /// Add a WASM plugin (its manifest text + component bytes); idempotent
    /// by id — the first registration wins.
    pub(crate) fn add(manifest: &str, wasm: Vec<u8>) -> Arc<Manifest> {
        let mut extra = crate::lock(&EXTRA);
        let mut m = parse_manifest(manifest).expect("test manifest parses");
        if let Some(existing) = extra.iter().find(|e| e.id == m.id) {
            return existing.clone();
        }
        m.wasm = Wasm::new(Cow::Owned(wasm));
        let m = Arc::new(m);
        extra.push(m.clone());
        m
    }

    /// The host's fixture plugin (`plugins/test-fixture`, built into
    /// `plugins/dist-test`), added to the catalog.
    pub(crate) fn fixture() -> Arc<Manifest> {
        add(&fixture_manifest(), fixture_wasm())
    }

    pub(crate) fn fixture_manifest() -> String {
        dist_test_text("test-fixture/plugin.toml")
    }

    pub(crate) fn fixture_wasm() -> Vec<u8> {
        dist_test_bytes("test-fixture/plugin.wasm")
    }

    /// A file the build script laid out in `plugins/dist-test`.
    pub(crate) fn dist_test_bytes(path: &str) -> Vec<u8> {
        DistTest::get(path)
            .unwrap_or_else(|| panic!("plugins/dist-test/{path} — run scripts/build-plugins.sh"))
            .data
            .into_owned()
    }

    pub(crate) fn dist_test_text(path: &str) -> String {
        String::from_utf8(dist_test_bytes(path)).unwrap()
    }
}

/// Per-workspace detect results: plugin ids whose footprint is present.
#[derive(Default)]
pub(crate) struct DetectCache {
    entries: HashMap<String, (Instant, BTreeSet<String>)>,
}

impl DetectCache {
    pub(crate) fn forget_workspace(&mut self, ws: &str) {
        self.entries.remove(ws);
    }

    /// The catalog changed (a plugin installed, updated, rolled back or
    /// removed): its detect paths may have too.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Stat every manifest's detect paths under `root` (blocking).
fn detect_blocking(root: &Path, catalog: &[Arc<Manifest>]) -> BTreeSet<String> {
    catalog
        .iter()
        .filter(|m| {
            m.detect.any.is_empty() || m.detect.any.iter().any(|rel| present_unlinked(root, rel))
        })
        .map(|m| m.id.clone())
        .collect()
}

/// `root/rel` exists and NO component of `rel` is a symlink — the plugin's
/// own tooling refuses symlinked state dirs, so neither do we (checking only
/// the last component would let a symlinked `.living/` count through its
/// children).
fn present_unlinked(root: &Path, rel: &str) -> bool {
    let mut path = root.to_path_buf();
    for part in Path::new(rel).components() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(md) if !md.is_symlink() => {}
            _ => return false,
        }
    }
    true
}

/// Re-stat a workspace's footprints (off the reactor) and cache the result.
pub(crate) async fn refresh_detect(state: &AppState, ws: &str) -> BTreeSet<String> {
    let Some(root) = crate::lock(&state.workspaces).get(ws).map(|w| w.root) else {
        return BTreeSet::new();
    };
    let catalog = catalog(state);
    let found = tokio::task::spawn_blocking(move || detect_blocking(&root, &catalog))
        .await
        .unwrap_or_default();
    crate::lock(&state.plugin_detect)
        .entries
        .insert(ws.to_string(), (Instant::now(), found.clone()));
    found
}

fn switched_on(state: &AppState, ws: &str) -> BTreeSet<String> {
    crate::lock(&state.workspaces)
        .get(ws)
        .map(|w| w.plugins_on.into_iter().collect())
        .unwrap_or_default()
}

/// Active plugins in a workspace: switched on AND footprint present AND
/// passing the gates. A stale detect is refreshed first (a few stats, off
/// the reactor) — callers are routes, MCP connects, spawns and
/// once-per-event paths, never a tight loop. With nothing switched on this
/// returns before any fs work.
pub(crate) async fn active(state: &AppState, ws: &str) -> Vec<Arc<Manifest>> {
    let on = switched_on(state, ws);
    if on.is_empty() {
        return Vec::new();
    }
    let cached = crate::lock(&state.plugin_detect)
        .entries
        .get(ws)
        .filter(|(at, _)| at.elapsed() < DETECT_TTL)
        .map(|(_, found)| found.clone());
    let found = match cached {
        Some(found) => found,
        None => refresh_detect(state, ws).await,
    };
    catalog(state)
        .iter()
        .filter(|m| on.contains(&m.id) && found.contains(&m.id) && m.origin.gate.is_none())
        .cloned()
        .collect()
}

/// Active plugins in the workspace of session `sid` (empty when the session
/// has no workspace).
pub(crate) async fn active_for_session(state: &AppState, sid: &str) -> Vec<Arc<Manifest>> {
    match workspace_of_session(state, sid) {
        Some(ws) => active(state, &ws).await,
        None => Vec::new(),
    }
}

/// MCP tools to pre-allow for a session spawned in `ws`: those of every
/// plugin active there, plus `tell_mastermind` when the workspace has a
/// Mastermind (empty — and so no settings change at all — when neither).
pub(crate) async fn spawn_allow(state: &AppState, ws: &str) -> Vec<String> {
    let mut tools: Vec<String> = active(state, ws)
        .await
        .iter()
        .flat_map(|m| m.provides.mcp_tools.iter().cloned())
        .collect();
    // A workspace with a Mastermind: its workers may message it without a
    // prompt (the Mastermind's own spawn carrying the entry is inert — the
    // tool is never offered to it).
    if crate::lock(&state.workspaces)
        .get(ws)
        .is_some_and(|w| w.mastermind.is_some())
    {
        tools.push("tell_mastermind".to_string());
    }
    tools
}

/// The workspace a session belongs to, if any.
pub(crate) fn workspace_of_session(state: &AppState, sid: &str) -> Option<String> {
    crate::lock(&state.session_workspaces).get(sid).cloned()
}

/// A catalog entry on the wire (`GET /plugins`, `GET /workspaces/{id}/plugins`,
/// and the install/update/rollback/remove/check answers). The fields after
/// `detect` say what is running and where it came from; the optional ones
/// are present only when they hold something.
pub(crate) fn manifest_json(state: &AppState, m: &Manifest) -> Value {
    let mut v = json!({
        "id": m.id,
        "name": m.name,
        "summary": m.summary,
        "homepage": m.homepage,
        "adds": {"ui": m.adds.ui, "agents": m.adds.agents},
        "provides": {
            "knowledge": m.provides.knowledge,
            "mcp_tools": m.provides.mcp_tools,
            "views": m.provides.views,
        },
        "setup": m.setup.as_ref().map(|s| json!({"prompt": s.prompt})),
        "detect": m.detect.any,
        "version": m.version,
        "api": m.api,
        "source": m.origin.source.as_str(),
        "stale": m.origin.stale,
    });
    let o = &m.origin;
    if let Some(path) = &o.path {
        v["path"] = json!(path);
    }
    if let Some(version) = &o.embedded_version {
        v["embedded_version"] = json!(version);
    }
    if let Some(version) = &o.installed_version {
        v["installed_version"] = json!(version);
    }
    if let Some(previous) = &o.previous {
        v["previous"] = json!(previous);
    }
    if let Some(update) = releases::offer_for(state, m) {
        v["update"] = json!({
            "version": update.version,
            "url": update.url,
            "checked_ms": update.checked_ms,
        });
    }
    if let Some(gate) = &o.gate {
        v["fault"] = json!(gate);
    }
    v
}

pub(crate) fn not_found(what: &str) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": what}))).into_response()
}

/// GET /plugins — the catalog (what exists on this daemon).
pub(crate) async fn list_plugins(State(state): State<Arc<AppState>>) -> Response {
    let plugins: Vec<Value> = catalog(&state)
        .iter()
        .map(|m| manifest_json(&state, m))
        .collect();
    Json(json!({"schema": 1, "plugins": plugins})).into_response()
}

/// GET /workspaces/{id}/plugins — per-plugin status in one workspace: on /
/// footprint detected / active. Agent-plugin requirement state (asked of the
/// agents themselves) rides `GET /workspaces/{id}/agent-plugins`.
pub(crate) async fn workspace_plugins(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&id) else {
        return not_found("unknown workspace");
    };
    let on: BTreeSet<String> = workspace.plugins_on.iter().cloned().collect();
    let found = refresh_detect(&state, &id).await;
    let plugins: Vec<Value> = catalog(&state)
        .iter()
        .map(|m| {
            let mut v = manifest_json(&state, m);
            // A plugin that fails a gate is off whatever its switch says:
            // the switch is kept, and holds again once the gate passes.
            let is_on = on.contains(&m.id) && m.origin.gate.is_none();
            let detected = found.contains(&m.id);
            v["on"] = json!(is_on);
            v["detected"] = json!(detected);
            v["active"] = json!(is_on && detected);
            // Additive, and only when there is one: why the plugin isn't
            // answering here (the card shows it). A failed gate already
            // said why (`manifest_json`).
            if m.origin.gate.is_none() {
                if let Some(fault) = state.plugin_runtime.fault(m, &id) {
                    v["fault"] = json!(fault);
                }
            }
            v["requires"] = json!(m
                .requires
                .agent_plugins
                .iter()
                .map(|(agent, req)| json!({
                    "agent": agent,
                    "id": req.id,
                    "marketplace": req.marketplace,
                }))
                .collect::<Vec<_>>());
            v
        })
        .collect();
    Json(json!({
        "schema": 1,
        "workspace_id": id,
        "root": workspace.root,
        "plugins": plugins,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct PutWorkspacePlugin {
    on: bool,
}

/// PUT /workspaces/{id}/plugins/{pid} {on} — switch a plugin on or off for
/// one workspace. Durable or refused: a toggle the next restart forgets
/// would silently change what agents see.
pub(crate) async fn put_workspace_plugin(
    State(state): State<Arc<AppState>>,
    AxPath((id, pid)): AxPath<(String, String)>,
    Json(body): Json<PutWorkspacePlugin>,
) -> Response {
    let Some(m) = manifest(&state, &pid) else {
        return not_found("unknown plugin");
    };
    if body.on {
        if let Some(gate) = &m.origin.gate {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": format!("{} can't run on this daemon: {gate}", m.name)})),
            )
                .into_response();
        }
    }
    let result = crate::lock(&state.workspaces).set_plugin_on(&id, &pid, body.on);
    match result {
        Ok(Some(workspace)) => {
            // Either way the plugin starts over here: a fresh instance on
            // next use, and a fault cleared (switching off and on is how
            // the user retries a faulted plugin).
            state.plugin_runtime.reset(&pid, &id);
            // The switch is the moment agents' view changes: re-detect now
            // so the next connect answers from a fresh footprint.
            refresh_detect(&state, &id).await;
            state.changes.notify_waiters();
            Json(json!({"workspace_id": id, "plugins_on": workspace.plugins_on})).into_response()
        }
        Ok(None) => not_found("unknown workspace"),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("could not save the workspace: {err}")})),
        )
            .into_response(),
    }
}
/// Plugin ids / marketplace sources we hand to an agent CLI: first-party
/// manifest strings, still charset-gated (and never flag-shaped) because they
/// land in a generated script.
fn cli_safe(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@/._:+-".contains(c))
}

#[derive(Deserialize)]
pub(crate) struct AgentBody {
    agent: String,
}

pub(crate) fn bad_request(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": msg.into()}))).into_response()
}

/// POST /workspaces/{id}/plugins/{pid}/install {agent} — install the agent
/// plugin this workbench plugin requires, with the AGENT's own plugin
/// manager, in a visible terminal the user watches (chimaera never
/// reimplements `claude plugin` / `codex plugin`). The session is theirs to
/// read and close; the probe cache is invalidated when it ends.
pub(crate) async fn install_requirement(
    State(state): State<Arc<AppState>>,
    AxPath((id, pid)): AxPath<(String, String)>,
    Json(body): Json<AgentBody>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&id) else {
        return not_found("unknown workspace");
    };
    let Some(m) = manifest(&state, &pid) else {
        return not_found("unknown plugin");
    };
    let Some(req) = m.requires.agent_plugins.get(&body.agent) else {
        return bad_request(format!("{} needs nothing from {}", m.name, body.agent));
    };
    let Some(kind) = crate::agents::AgentKind::parse(&body.agent) else {
        return bad_request("unknown agent");
    };
    if !cli_safe(&req.id) || !cli_safe(&req.marketplace) {
        return bad_request("manifest requirement is not CLI-safe");
    }
    let det = crate::launcher::detect(&state, kind, false).await;
    let bin = match det.path {
        Ok(bin) => bin,
        Err(err) => {
            return (StatusCode::CONFLICT, Json(json!({"error": err}))).into_response();
        }
    };
    let bin = crate::runtimes::sq(&bin.to_string_lossy());
    let install_verb = if kind == crate::agents::AgentKind::Codex {
        "add"
    } else {
        "install"
    };
    let agent = kind.as_str();
    let script = format!(
        "echo 'Installing {name} for {agent} with {agent}'\''s own plugin manager.'\n         echo\n         echo '$ {agent} plugin marketplace add {mkt}'\n         {bin} plugin marketplace add {mkt_q} || echo '(already added or unavailable — continuing)'\n         echo\n         echo '$ {agent} plugin {install_verb} {pid_s}'\n         {bin} plugin {install_verb} {pid_q}\n         status=$?\n         echo\n         if [ $status -eq 0 ]; then echo 'Done — you can close this terminal.'; \
         else echo \"Install failed (exit $status).\"; fi\n         exit $status\n",
        name = m.name.replace('\'', ""),
        mkt = req.marketplace,
        mkt_q = crate::runtimes::sq(&req.marketplace),
        pid_s = req.id,
        pid_q = crate::runtimes::sq(&req.id),
    );
    let session_id = crate::agents::fresh_session_id();
    let env = crate::api::session_env(&state, &session_id, "dark", None);
    let env_remove = crate::api::spawn_env_remove(&env);
    let opts = chimaera_pty::SpawnOpts {
        cwd: workspace.root.clone(),
        name: Some(format!("install {} for {agent}", m.id)),
        cols: 100,
        rows: 24,
        // Login-shell wrap, like every agent spawn and the probes that found
        // this CLI: an npm-installed agent (`#!/usr/bin/env node`) needs the
        // user's PATH, which an app-launched daemon's own env lacks.
        command: Some(crate::launcher::wrap_login_shell(
            &crate::launcher::login_shell(),
            vec!["/bin/bash".to_string(), "-c".to_string(), script],
        )),
        id: Some(session_id.clone()),
        env,
        env_remove,
        scrollback: crate::lock(&state.settings).scrollback_lines(),
    };
    match state.sessions.spawn(opts) {
        Ok(info) => {
            crate::lock(&state.session_workspaces).insert(info.id.clone(), workspace.id.clone());
            // When the install ends, the agents' answers changed.
            let watch_state = state.clone();
            let sid = info.id.clone();
            tokio::spawn(async move {
                while watch_state.sessions.get(&sid).is_some() {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                watch_state.probes.invalidate();
                watch_state.changes.notify_waiters();
            });
            tracing::info!(workspace = %id, plugin = %pid, agent, "plugin requirement install started");
            state.changes.notify_waiters();
            Json(json!({"session_id": info.id})).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

/// POST /workspaces/{id}/plugins/{pid}/setup {agent} — a new chat session of
/// the user's chosen agent, sent the plugin's own documented setup prompt.
/// The user's click, their billing; the plugin's own tooling does the work.
pub(crate) async fn setup_workspace(
    State(state): State<Arc<AppState>>,
    AxPath((id, pid)): AxPath<(String, String)>,
    Json(body): Json<AgentBody>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&id) else {
        return not_found("unknown workspace");
    };
    let Some(m) = manifest(&state, &pid) else {
        return not_found("unknown plugin");
    };
    let Some(setup) = &m.setup else {
        return bad_request(format!("{} has no setup step", m.name));
    };
    let Some(kind) = crate::agents::AgentKind::parse(&body.agent) else {
        return bad_request("unknown agent");
    };
    if !kind.chat_capable() {
        return bad_request(format!("no chat driver for {}", body.agent));
    }
    let theme = crate::lock(&state.settings)
        .map_cached()
        .get("appearance.theme")
        .and_then(|v| v.as_str())
        .filter(|t| *t == "light" || *t == "dark")
        .unwrap_or("dark")
        .to_string();
    let spawned = crate::chat::spawn_fresh_chat(
        &state,
        workspace,
        crate::chat::FreshChat {
            id: None,
            kind,
            model: None,
            name: Some(format!("{} setup", m.name)),
            title_hint: None,
            theme,
            prelude: None,
            mastermind: None,
            fork: None,
        },
    )
    .await;
    let row = match spawned {
        Ok(row) => row,
        Err(crate::chat::ChatSpawnFailure::AgentUnavailable(msg)) => {
            return (StatusCode::CONFLICT, Json(json!({"error": msg}))).into_response();
        }
        Err(crate::chat::ChatSpawnFailure::Internal(err)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("spawn failed: {err}")})),
            )
                .into_response();
        }
    };
    let Some(sid) = row["id"].as_str().map(str::to_owned) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "spawned session has no id"})),
        )
            .into_response();
    };
    let command = chimaera_agent::model::AgentCommand::Send {
        blocks: vec![chimaera_agent::model::ContentBlock::Text {
            text: setup.prompt.clone(),
        }],
    };
    if let Err(err) = state.chat.command(&sid, command).await {
        // The session exists; say what didn't happen rather than hiding it.
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"session_id": sid, "error": format!("setup prompt not delivered: {err}")})),
        )
            .into_response();
    }
    tracing::info!(workspace = %id, plugin = %pid, session = %sid, "plugin setup started");
    Json(json!({"session_id": sid})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_manifest_parses_and_ids_are_unique() {
        let embedded = Dist::iter().filter(|p| p.ends_with("/plugin.toml")).count();
        assert_eq!(
            production_catalog().len(),
            embedded,
            "a manifest failed to parse or its component was refused"
        );
        let ids: Vec<&str> = production_catalog().iter().map(|m| m.id.as_str()).collect();
        assert!(
            ids.windows(2).all(|pair| pair[0] < pair[1]),
            "the catalog is sorted by id, each once: {ids:?}"
        );
        for id in ["agent-notes", "mycelium"] {
            let m = production_catalog()
                .iter()
                .find(|m| m.id == id)
                .unwrap_or_else(|| panic!("{id} ships (run scripts/build-plugins.sh)"));
            assert!(!m.wasm.bytes.is_empty());
            assert_eq!(m.api, API);
            assert_eq!(gate(m, "0.4.1"), None, "{id} runs on a released daemon");
        }
        for m in production_catalog() {
            assert!(!m.summary.is_empty(), "{} needs a summary", m.id);
            assert_eq!(m.origin.source, Source::Embedded);
            assert!(
                !m.adds.ui.is_empty() || !m.adds.agents.is_empty(),
                "{} must say what it adds",
                m.id
            );
            for rel in &m.detect.any {
                assert!(
                    !rel.starts_with('/') && !rel.contains(".."),
                    "{}: detect paths are workspace-relative",
                    m.id
                );
            }
        }
    }

    #[test]
    fn cli_safe_rejects_flags_and_shell_metacharacters() {
        assert!(cli_safe("mycelium@mycelium"));
        assert!(cli_safe("arjunrajlaboratory/mycelium"));
        assert!(!cli_safe("--evil"));
        assert!(!cli_safe("a;rm -rf"));
        assert!(!cli_safe("$(x)"));
        assert!(!cli_safe(""));
        for m in production_catalog() {
            for req in m.requires.agent_plugins.values() {
                assert!(cli_safe(&req.id) && cli_safe(&req.marketplace), "{}", m.id);
            }
        }
    }

    #[test]
    fn the_test_fixture_never_ships() {
        test_catalog::fixture();
        assert!(embedded().iter().any(|m| m.id == "test-fixture"));
        assert!(production_catalog().iter().all(|m| m.id != "test-fixture"));
        assert!(Dist::iter().all(|p| !p.starts_with("test-")));
    }

    #[test]
    fn unknown_manifest_fields_are_rejected() {
        let err = toml::from_str::<Manifest>(
            "id='x'\nname='x'\nversion='0.1.0'\nsummary='x'\napi='0.1'\n[provides]\nmcp_tool=['typo']",
        );
        assert!(err.is_err());
    }

    const BASE: &str = "id = \"demo\"\nname = \"Demo\"\nsummary = \"x\"\n";

    fn demo(extra: &str) -> Result<Manifest, String> {
        parse_manifest(&format!("{BASE}{extra}"))
    }

    #[test]
    fn a_manifest_needs_a_semver_version_and_an_api() {
        assert!(demo("api = \"0.1\"\n").is_err(), "version is required");
        assert!(demo("version = \"0.1.0\"\n").is_err(), "api is required");
        for bad in ["0.1", "v0.1.0", "0.1.0-beta.1", "0.1.0+build", "x"] {
            let err = demo(&format!("version = \"{bad}\"\napi = \"0.1\"\n")).unwrap_err();
            assert!(err.contains("MAJOR.MINOR.PATCH"), "{bad}: {err}");
        }
        let m = demo(
            "version = \"1.2.3\"\napi = \"0.1\"\n[requires]\nchimaera = \">=0.4.0\"\n\
             [provides]\nevents = [\"hook\", \"session-ended\"]\n[release]\ngithub = \"acme/demo\"\n",
        )
        .unwrap();
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.requires.chimaera.as_deref(), Some(">=0.4.0"));
        assert!(m.provides.hears(EventKind::Hook) && m.provides.hears(EventKind::SessionEnded));
        assert!(!m.provides.hears(EventKind::SwitchedOn));
        assert_eq!(m.release.unwrap().github, "acme/demo");
        assert!(
            demo("version = \"0.1.0\"\napi = \"0.1\"\n[provides]\nevents = [\"tick\"]\n").is_err()
        );
        assert!(
            demo("version = \"0.1.0\"\napi = \"0.1\"\n[release]\ngithub = \"../x\"\n").is_err()
        );
        assert!(
            demo("version = \"0.1.0\"\napi = \"0.1\"\n[release]\nurl = \"https://x\"\n").is_err()
        );
    }

    #[test]
    fn ids_and_github_slugs_are_path_safe() {
        assert!(valid_id("agent-notes") && valid_id("latex2"));
        for bad in ["", "-x", "X", "a/b", "a.b", "..", "install", "a b"] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert!(valid_github("arjunrajlaboratory/mycelium") && valid_github("a-b/c_d.e"));
        for bad in [
            "a", "a/b/c", "../b", "a/..", "a/.git", "a/b?x", "a /b", "/b",
        ] {
            assert!(!valid_github(bad), "{bad}");
        }
    }

    #[test]
    fn the_gates_speak_in_the_cards_words() {
        let with = |api: &str, req: Option<&str>| {
            let req = req
                .map(|r| format!("[requires]\nchimaera = \"{r}\"\n"))
                .unwrap_or_default();
            demo(&format!("version = \"0.1.0\"\napi = \"{api}\"\n{req}")).unwrap()
        };
        assert_eq!(gate(&with("0.1", None), "0.4.1"), None);
        let newer = gate(&with("0.2", None), "0.4.1").unwrap();
        assert!(newer.starts_with("needs a newer chimaera"), "{newer}");
        let older = gate(&with("0.0", None), "0.4.1").unwrap();
        assert!(older.starts_with("needs a newer plugin"), "{older}");
        assert!(gate(&with("one", None), "0.4.1").is_some());

        assert_eq!(gate(&with("0.1", Some(">=0.4.0")), "0.4.1"), None);
        assert_eq!(
            gate(&with("0.1", Some(">=0.5.0")), "0.4.1").as_deref(),
            Some("needs chimaera ≥ 0.5.0 (this is 0.4.1)")
        );
        assert!(gate(&with("0.1", Some("^0.3")), "0.4.1")
            .unwrap()
            .contains("^0.3"));
        assert_eq!(
            gate(&with("0.1", Some(">=99.0.0")), "0.0.1"),
            None,
            "a dev build matches every requirement"
        );
        assert!(gate(&with("0.1", Some("soon")), "0.4.1")
            .unwrap()
            .contains("not a version requirement"));
    }

    fn copy(version: &str, previous: Option<&str>) -> installed::InstalledCopy {
        let mut m = demo(&format!(
            "version = \"{version}\"\napi = \"0.1\"\n[release]\ngithub = \"acme/demo\"\n"
        ))
        .unwrap();
        m.origin.source = Source::Installed;
        installed::InstalledCopy {
            manifest: m,
            dir: PathBuf::from(format!("/p/demo/{version}")),
            previous: previous.map(str::to_string),
        }
    }

    fn shipped(version: &str) -> Arc<Manifest> {
        Arc::new(demo(&format!("version = \"{version}\"\napi = \"0.1\"\n")).unwrap())
    }

    #[test]
    fn precedence_is_the_higher_version_and_never_silent() {
        // Installed higher: it loads, and both versions are named.
        let all = resolve(
            &[shipped("0.3.1")],
            &[copy("0.3.2", Some("0.3.0"))],
            "0.4.1",
        );
        let m = &all[0];
        assert_eq!(m.version, "0.3.2");
        assert_eq!(m.origin.source, Source::Installed);
        assert_eq!(m.origin.embedded_version.as_deref(), Some("0.3.1"));
        assert_eq!(m.origin.installed_version.as_deref(), Some("0.3.2"));
        assert_eq!(m.origin.previous.as_deref(), Some("0.3.0"));
        assert_eq!(m.origin.path, Some(PathBuf::from("/p/demo/0.3.2")));
        assert!(!m.origin.stale);
        assert_eq!(m.origin.installed_release.as_deref(), Some("acme/demo"));

        // Equal: the embedded copy loads; the installed one is still named.
        let m = &resolve(&[shipped("0.3.2")], &[copy("0.3.2", None)], "0.4.1")[0];
        assert_eq!(m.origin.source, Source::Embedded);
        assert!(!m.origin.stale);
        assert_eq!(m.origin.installed_version.as_deref(), Some("0.3.2"));

        // Installed older: the embedded loads and the copy is stale.
        let m = &resolve(&[shipped("0.3.2")], &[copy("0.3.1", None)], "0.4.1")[0];
        assert_eq!(m.origin.source, Source::Embedded);
        assert_eq!(m.version, "0.3.2");
        assert!(m.origin.stale);

        // Numeric, not lexicographic.
        let m = &resolve(&[shipped("0.9.0")], &[copy("0.10.0", None)], "0.4.1")[0];
        assert_eq!(m.origin.source, Source::Installed);

        // One side only.
        let m = &resolve(&[], &[copy("1.0.0", None)], "0.4.1")[0];
        assert_eq!(m.origin.source, Source::Installed);
        assert_eq!(m.origin.embedded_version, None);
        let m = &resolve(&[shipped("1.0.0")], &[], "0.4.1")[0];
        assert_eq!(m.origin.source, Source::Embedded);
        assert_eq!(m.origin.path, None);
    }

    #[test]
    fn detect_needs_a_real_footprint_and_ignores_symlinks() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-plugins-detect-{}-{}",
            std::process::id(),
            crate::timeline::now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let catalog = production_catalog();
        let none = detect_blocking(&root, catalog);
        assert!(!none.contains("mycelium"));
        assert!(
            none.contains("agent-notes"),
            "no footprint = always present"
        );
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("findings")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, root.join(".living")).unwrap();
        assert!(!detect_blocking(&root, catalog).contains("mycelium"));
        std::fs::write(root.join("MYCELIUM.md"), "# protocol").unwrap();
        assert!(detect_blocking(&root, catalog).contains("mycelium"));
        let _ = std::fs::remove_dir_all(root);
    }
}
