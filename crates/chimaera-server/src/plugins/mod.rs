//! Workbench plugins: opt-in add-ons that say exactly what they add.
//! Design: docs/design/timeline-knowledge-plugins-plan.md §6; the plugin host:
//! docs/design/plugin-system-plan.md.
//!
//! A plugin is a small TOML manifest (data, never code) plus its behaviour:
//! a WASM component (`plugin.wasm`). Every plugin lives in its own
//! repository and is installed into `<data dir>/plugins/<id>/<version>/`
//! (`installed`) from a release, checksum-verified, or from a local
//! directory. This binary carries no plugin bytes: it embeds only
//! `plugins/plugins.lock`, the curated list of first-party plugins (each
//! with the release this daemon installs and that release's sha256s). A
//! plugin runs in `runtime` (the sandbox) and asks the daemon for everything
//! through `hostfns` (bounded). No plugin behaviour is daemon code: the
//! Knowledge provider (mycelium) answers `knowledge.rs` through its
//! `knowledge` export like any other plugin would.
//!
//! The catalog (`Catalog`, on `AppState`) is the installed copies; the
//! wire adds each first-party plugin of the lock that has none, as
//! `available` (never active, nothing to load). Reading an installed copy
//! checks its files against the `SHA256SUMS` kept beside them (and a
//! first-party copy at the pinned version against the lock): a mismatch
//! lists it with a fault and it never loads. Gates run before anything
//! loads: a manifest's `api` must be a WIT version this host serves and its
//! `requires.chimaera` must match this daemon — a plugin that fails one
//! stays listed, off, with the reason — as does a copy of a plugin whose
//! job moved into chimaera (`retired`), which nothing installs again. The
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
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

pub(crate) mod activity;
pub mod capabilities;
pub(crate) mod files;
pub(crate) mod hostfns;
pub(crate) mod installed;
pub(crate) mod jobs;
pub(crate) mod output;
pub(crate) mod pdata;
pub(crate) mod platform;
pub(crate) mod preview;
pub(crate) mod releases;
pub(crate) mod retired;
pub(crate) mod revoke;
pub(crate) mod runtime;
pub(crate) mod screens;
pub(crate) mod surfaces;
pub(crate) mod toolchain;
pub(crate) mod tools;
pub(crate) mod trust;

/// How long a workspace's detect result is trusted before a re-stat.
const DETECT_TTL: Duration = Duration::from_secs(30);

/// The newest WIT version (`chimaera:plugin@0.2.x`): what a new plugin targets.
pub(crate) const API: &str = "0.2";
/// Every WIT version this host serves. A manifest's `api` must be one of
/// them; a WIT bump adds a version here and keeps the old ones (0.1 through
/// its own bindings, `runtime::v1`).
pub(crate) const SERVED_APIS: &[&str] = &["0.1", API];

/// The test-only plugins (the host's fixture, and the first-party releases
/// the lock pins, which tests install by path): embedded by test builds
/// only, laid out by `scripts/build-plugins.sh`.
#[cfg(test)]
#[derive(rust_embed::RustEmbed)]
#[folder = "../../plugins/dist-test"]
pub(crate) struct DistTest;

/// `plugins/plugins.lock`: the first-party plugins, the release of each this
/// daemon installs, and that release's checksums. The only plugin data this
/// binary carries.
const LOCK: &str = include_str!("../../../../plugins/plugins.lock");

/// One `[[plugin]]` of the lock.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct Locked {
    pub(crate) id: String,
    /// What the card shows before any copy is installed.
    pub(crate) name: String,
    pub(crate) summary: String,
    /// The release Install fetches (`v<version>`).
    pub(crate) version: String,
    /// `owner/repo`: where it is released, and what an installed copy's
    /// `[release] github` must name to be first-party.
    pub(crate) repo: String,
    /// That release's `SHA256SUMS` entries, lowercase hex.
    pub(crate) sha256_wasm: String,
    pub(crate) sha256_toml: String,
    /// What the maintainers approved it to do: its tier (`sandboxed` or
    /// `privileged`) and capability digest (`capabilities`). A sandboxed
    /// update from its repository keeps the badge only while its digest is
    /// this one; a privileged plugin is verified only at the pin.
    pub(crate) tier: String,
    pub(crate) caps: String,
}

/// Parse and check a lock: ids, versions, repos and sha256s well-formed,
/// each id once; sorted by id.
pub(crate) fn parse_lock(text: &str) -> Result<Vec<Locked>, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Lock {
        #[serde(default)]
        plugin: Vec<Locked>,
    }
    let mut entries = toml::from_str::<Lock>(text)
        .map_err(|e| format!("plugins.lock: {e}"))?
        .plugin;
    for l in &entries {
        let id = &l.id;
        if !valid_id(id) {
            return Err(format!("plugins.lock: id {id:?} is not a plugin id"));
        }
        plugin_version(&l.version).map_err(|e| format!("plugins.lock: {id}: {e}"))?;
        if !valid_github(&l.repo) {
            return Err(format!(
                "plugins.lock: {id}: repo {:?} is not owner/repo",
                l.repo
            ));
        }
        if l.name.trim().is_empty() || l.summary.trim().is_empty() {
            return Err(format!("plugins.lock: {id}: name and summary are required"));
        }
        for sha in [&l.sha256_wasm, &l.sha256_toml, &l.caps] {
            if sha.len() != 64 || !sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                return Err(format!(
                    "plugins.lock: {id}: a sha256 is not 64 lowercase hex digits"
                ));
            }
        }
        if capabilities::Tier::parse(&l.tier).is_none() {
            return Err(format!(
                "plugins.lock: {id}: tier {:?} is not sandboxed or privileged",
                l.tier
            ));
        }
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(pair) = entries.windows(2).find(|p| p[0].id == p[1].id) {
        return Err(format!("plugins.lock names {} twice", pair[0].id));
    }
    Ok(entries)
}

/// The lock, parsed once. A lock that doesn't parse is a build bug (a test
/// fails on it); the daemon then lists no first-party plugin rather than
/// refusing to start.
static LOCKED: LazyLock<Vec<Locked>> = LazyLock::new(|| {
    parse_lock(LOCK).unwrap_or_else(|err| {
        tracing::error!(%err, "no first-party plugins: the embedded plugins.lock is invalid");
        Vec::new()
    })
});

/// Every first-party plugin, sorted by id.
pub(crate) fn lock_entries() -> &'static [Locked] {
    &LOCKED
}

/// The lock's entry for `id`, if it is a first-party plugin.
pub(crate) fn lock_entry(id: &str) -> Option<&'static Locked> {
    lock_entries().iter().find(|l| l.id == id)
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub(crate) id: String,
    pub(crate) name: String,
    /// The plugin's own version, `MAJOR.MINOR.PATCH` (`validate`).
    pub(crate) version: String,
    pub(crate) summary: String,
    /// A few plain sentences from the plugin's author: what it is and why a
    /// person would switch it on. The card shows it under the summary.
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) homepage: Option<String>,
    /// The WIT version the component targets (`"0.1"`); a gate.
    pub(crate) api: String,
    #[serde(default)]
    pub(crate) detect: Detect,
    /// `[access]`: what it may read through the host (`capabilities`).
    #[serde(default)]
    pub(crate) access: capabilities::AccessDecl,
    #[serde(default)]
    pub(crate) requires: Requires,
    #[serde(default)]
    pub(crate) recommends: Recommends,
    #[serde(default)]
    pub(crate) setup: Option<Setup>,
    #[serde(default)]
    pub(crate) provides: Provides,
    #[serde(default)]
    pub(crate) adds: Adds,
    /// Where newer versions are published (the release checker's source).
    #[serde(default)]
    pub(crate) release: Option<ReleaseSource>,
    /// `[[views]]` (0.2): the screens it draws, in the Chimaera format.
    #[serde(default)]
    pub(crate) views: Vec<platform::ViewDecl>,
    /// `[[files]]` (0.2): the file kinds it opens in one of its views.
    #[serde(default)]
    pub(crate) files: Vec<platform::FileKind>,
    /// `[[actions]]` (0.2): items it adds to matching files' menus.
    #[serde(default)]
    pub(crate) actions: Vec<platform::FileAction>,
    /// `[[settings]]` (0.2): its settings, drawn in Settings → Plugins.
    #[serde(default)]
    pub(crate) settings: Vec<platform::SettingDecl>,
    /// `[[programs]]` (0.2): the programs it may run as jobs (privileged).
    #[serde(default)]
    pub(crate) programs: Vec<platform::ProgramDecl>,
    /// `[[tools]]` (0.2): side programs the host downloads on a click
    /// (privileged).
    #[serde(default)]
    pub(crate) tools: Vec<platform::ToolDecl>,
    /// The behaviour — set by the catalog, never by the TOML.
    #[serde(skip)]
    pub(crate) wasm: Wasm,
    /// What it can do, derived once when the manifest is parsed
    /// (`parse_manifest`): the card's Can list, its tier and digest.
    #[serde(skip)]
    pub(crate) caps: capabilities::Caps,
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

/// What the catalog knows about a loaded copy beyond its manifest: where it
/// lives, what it goes back to, whether its bytes are the ones released,
/// and why it can't run, if it can't.
#[derive(Clone, Debug, Default)]
pub(crate) struct Origin {
    /// The installed copy's `current` version directory (`None`: a test
    /// build's catalog extra, which has no directory).
    pub(crate) path: Option<PathBuf>,
    /// The installed copy's `previous` version (Use previous).
    pub(crate) previous: Option<String>,
    /// Why this copy can't run here: a failed gate, or its files not
    /// matching the `SHA256SUMS` kept beside them. Listed, never active.
    pub(crate) fault: Option<String>,
    /// Where the checker and Update look for a newer version: its
    /// `[release] github`.
    pub(crate) release: Option<String>,
    /// A Chimaera plugin: its id is in the lock and its `[release] github`
    /// is the lock's repository (an update keeps it; a third-party plugin
    /// reusing a first-party id never has it).
    pub(crate) first_party: bool,
    /// Its files match the release's `SHA256SUMS` kept beside them — and,
    /// a first-party copy at the pinned version, the lock's sha256s too.
    pub(crate) verified: bool,
    /// Installed from a local directory (`POST /plugins/install {path}`):
    /// that directory.
    pub(crate) local_path: Option<PathBuf>,
    /// The repository a release install came from (the host-written
    /// `source-github` marker): where trust records say it came from.
    pub(crate) source_github: Option<String>,
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
    /// Stable agent identity → the agent-native plugin it needs.
    /// A genuine hard requirement (none today; a recommendation is
    /// `recommends`).
    #[serde(default)]
    pub(crate) agent_plugins: BTreeMap<String, AgentPluginReq>,
    /// A semver requirement on this daemon (`">=0.4.0"`, `"^0.4"`) for a
    /// plugin that needs a host import a later daemon added; a gate.
    #[serde(default)]
    pub(crate) chimaera: Option<String>,
    /// One plain sentence: what the required agent-side plugin is for (the
    /// card's "Agent-side plugin" box says it above the agents' rows).
    #[serde(default)]
    pub(crate) summary: Option<String>,
}

/// `[recommends.agent_plugins.<agent>]`: an agent-side plugin that makes
/// this plugin more useful to the agents the user runs, never needed for it
/// to work (mycelium's, which lets an agent record knowledge the Knowledge
/// reader then shows). The card offers it only for agents installed here.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Recommends {
    #[serde(default)]
    pub(crate) agent_plugins: BTreeMap<String, AgentPluginReq>,
    /// One plain sentence: what the recommended agent-side plugin is for.
    #[serde(default)]
    pub(crate) summary: Option<String>,
}

impl Manifest {
    /// Every agent-side plugin id this plugin names for `agent`, required
    /// and recommended: the hooks codex hook trust may write for it.
    pub(crate) fn agent_plugin_ids(&self, agent: &str) -> Vec<&str> {
        self.requires
            .agent_plugins
            .get(agent)
            .into_iter()
            .chain(self.recommends.agent_plugins.get(agent))
            .map(|req| req.id.as_str())
            .collect()
    }

    /// The agent-side plugin this plugin names for `agent` (with `id`, that
    /// one): a requirement first, else a recommendation (the install route
    /// serves both).
    fn agent_plugin_matching(&self, agent: &str, id: Option<&str>) -> Option<&AgentPluginReq> {
        self.requires
            .agent_plugins
            .get(agent)
            .into_iter()
            .chain(self.recommends.agent_plugins.get(agent))
            .find(|req| id.is_none_or(|id| req.id == id))
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentPluginReq {
    /// The agent's own plugin id, e.g. "mycelium@mycelium".
    pub(crate) id: String,
    /// What the agent's `marketplace add` takes (owner/repo or a URL).
    #[serde(default)]
    pub(crate) marketplace: String,
    /// Native install source for providers without the Claude/Codex marketplace
    /// pipeline: a local package or a source accepted by that provider.
    #[serde(default)]
    pub(crate) source: Option<String>,
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
    // 0.2 only (`validate`).
    FileSaved,
    FileChanged,
    JobFinished,
    SettingsChanged,
}

impl EventKind {
    /// Whether a 0.1 build could hear it (its WIT's `event` variant).
    fn in_v1(self) -> bool {
        matches!(
            self,
            EventKind::Hook
                | EventKind::SessionEnded
                | EventKind::SwitchedOn
                | EventKind::SwitchedOff
        )
    }
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
/// else; `install` and `preview` are routes' own segments (`/plugins/install`,
/// `/plugins/preview`).
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id != "install"
        && id != "preview"
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
    // Shown on every card, prompt and terminal: words, never control
    // characters (a terminal escape could redraw a trust prompt).
    for (what, text) in [("name", &m.name), ("summary", &m.summary)] {
        let empty_name = what == "name" && text.trim().is_empty();
        if empty_name || text.len() > 200 || text.chars().any(char::is_control) {
            return Err(format!(
                "the {what} is one line of at most 200 bytes, without control characters"
            ));
        }
    }
    let mut tools = BTreeSet::new();
    for tool in &m.provides.mcp_tools {
        // No dots: codex pre-approves a tool through a dotted config key
        // (`mcp_servers.chimaera.tools.<name>.approval_mode`), where a dot
        // would split the name — refused here rather than skipped there.
        if tool.is_empty()
            || tool.len() > 64
            || !tool
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(format!(
                "MCP tool {tool:?} must be 1–64 letters, digits, underscores or dashes"
            ));
        }
        if crate::mcp::is_core_tool(tool) {
            return Err(format!("MCP tool {tool:?} is reserved by chimaera"));
        }
        if !tools.insert(tool) {
            return Err(format!("MCP tool {tool:?} is listed twice"));
        }
    }
    // Detect paths are stat'ed under the workspace root: relative, plain
    // components only, so a manifest can't probe outside the workspace.
    for rel in &m.detect.any {
        let plain = !rel.is_empty()
            && Path::new(rel)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !plain {
            return Err(format!(
                "detect path {rel:?} must be relative to the workspace, without `..` or `.`"
            ));
        }
    }
    if m.api == "0.1" {
        if let Some(e) = m.provides.events.iter().find(|e| !e.in_v1()) {
            return Err(format!(
                "the event {e:?} needs api = \"0.2\" (a 0.1 component can't hear it)"
            ));
        }
        if !(m.views.is_empty()
            && m.files.is_empty()
            && m.actions.is_empty()
            && m.settings.is_empty()
            && m.programs.is_empty()
            && m.tools.is_empty())
        {
            return Err(
                "[[views]], [[files]], [[actions]], [[settings]], [[programs]] and \
                 [[tools]] need api = \"0.2\""
                    .into(),
            );
        }
    }
    platform::validate(m)?;
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
    if text.len() as u64 > installed::TOML_MAX {
        return Err("plugin.toml exceeds the 64 KiB manifest limit".into());
    }
    let mut m = toml::from_str::<Manifest>(text).map_err(|e| format!("plugin.toml: {e}"))?;
    validate(&m)?;
    m.caps = capabilities::Caps::of(&m);
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

/// A workspace-relative path of plain components (`hostfns`' rule).
pub(crate) fn hostfns_relative(path: &str) -> Result<PathBuf, String> {
    hostfns::relative(path)
}

/// The loadable plugins, one entry per id, sorted by id (the Extensions
/// tab's order, and the order active plugins' tools and instruction
/// paragraphs reach an agent in): every installed copy, and (test builds)
/// the catalog extras no installed copy shadows. Then the gates, on each —
/// after the fault reading the copy already found (its files not matching
/// its `SHA256SUMS`), which wins. A retired plugin's reason wins over both:
/// whatever else is wrong with the copy, the answer is to remove it.
pub(crate) fn resolve(
    extras: &[Arc<Manifest>],
    installed: &[installed::InstalledCopy],
    daemon: &str,
) -> Vec<Arc<Manifest>> {
    let mut all: Vec<Arc<Manifest>> = installed
        .iter()
        .map(|c| {
            let mut m = c.manifest.clone();
            m.origin.path = Some(c.dir.clone());
            m.origin.previous = c.previous.clone();
            m
        })
        .chain(
            extras
                .iter()
                .filter(|e| installed.iter().all(|c| c.manifest.id != e.id))
                .map(|e| (**e).clone()),
        )
        .map(|mut m| {
            m.origin.release = m.release.as_ref().map(|r| r.github.clone());
            let fault = match retired::of(&m.id) {
                Some(r) => Some(r.reason.to_string()),
                None => m.origin.fault.take().or_else(|| gate(&m, daemon)),
            };
            m.origin.fault = fault;
            Arc::new(m)
        })
        .collect();
    all.sort_by(|a, b| a.id.cmp(&b.id));
    all
}

/// The test build's catalog extras (none in a real build).
fn extras() -> Vec<Arc<Manifest>> {
    #[cfg(test)]
    return test_catalog::extra();
    #[cfg(not(test))]
    Vec::new()
}

/// The catalog every lookup reads (on `AppState`): the installed copies
/// under `<data dir>/plugins` (and a test build's extras). Reloaded
/// (`reload`, blocking — off the reactor) after an install, update,
/// rollback or remove, so a change takes effect without a restart. The
/// first-party plugins nothing is installed for are not in it: they have no
/// component to load, and the wire lists them as `available` (`listing`).
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

pub(crate) fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
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
        let extras = extras();
        let copies = read(&self.installed).clone();
        let daemon = read(&self.daemon).clone();
        let all = Arc::new(resolve(&extras, &copies, &daemon));
        *write(&self.merged) = (extras.len(), all);
    }

    /// Every loadable plugin, sorted by id.
    pub(crate) fn all(&self) -> Arc<Vec<Arc<Manifest>>> {
        {
            let merged = read(&self.merged);
            if merged.0 == extras().len() {
                return merged.1.clone();
            }
        }
        self.remerge();
        read(&self.merged).1.clone()
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<Manifest>> {
        self.all().iter().find(|m| m.id == id).cloned()
    }

    /// Every installed copy as found on disk.
    pub(crate) fn installed_copies(&self) -> Arc<Vec<installed::InstalledCopy>> {
        read(&self.installed).clone()
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

/// Why a plugin change or check was refused, as its route answers it
/// (`{"error": message}` with `status`; constructors in `releases`).
#[derive(Debug)]
pub(crate) struct Refusal {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
    /// Keys the answer carries beside `error` (`Refusal::with`).
    pub(crate) detail: Option<Value>,
}

/// The catalog every lookup reads: the loadable plugins.
pub(crate) fn catalog(state: &AppState) -> Arc<Vec<Arc<Manifest>>> {
    state.plugin_catalog.all()
}

pub(crate) fn manifest(state: &AppState, id: &str) -> Option<Arc<Manifest>> {
    state.plugin_catalog.get(id)
}

/// Why a change to `pid` needs an installed copy it doesn't have: a
/// first-party plugin that isn't installed (409), or no plugin at all (404).
pub(crate) fn not_installed(pid: &str) -> Refusal {
    match lock_entry(pid) {
        Some(l) => Refusal::conflict(format!("{} isn't installed — install it first", l.name)),
        None => Refusal::not_found("unknown plugin"),
    }
}

/// One row of the Extensions tab.
pub(crate) enum Entry {
    /// A loadable plugin (an installed copy).
    Loaded(Arc<Manifest>),
    /// A first-party plugin of the lock with no installed copy.
    Available(&'static Locked),
}

/// What the Extensions tab lists, sorted by id: every loadable plugin, and
/// each first-party plugin nothing is installed for.
pub(crate) fn listing(state: &AppState) -> Vec<Entry> {
    let loaded = catalog(state);
    let mut all: Vec<Entry> = loaded.iter().cloned().map(Entry::Loaded).collect();
    all.extend(
        lock_entries()
            .iter()
            .filter(|l| loaded.iter().all(|m| m.id != l.id))
            .map(Entry::Available),
    );
    all.sort_by(|a, b| a.id().cmp(b.id()));
    all
}

impl Entry {
    fn id(&self) -> &str {
        match self {
            Entry::Loaded(m) => &m.id,
            Entry::Available(l) => &l.id,
        }
    }
}

/// `id`'s entry on the wire: its loaded copy, else the first-party plugin
/// it names as `available`; `None` when neither (a removed third-party one).
pub(crate) fn entry_json(state: &AppState, id: &str) -> Option<Value> {
    match (manifest(state, id), lock_entry(id)) {
        (Some(m), _) => Some(manifest_json(state, &m)),
        (None, Some(l)) => Some(available_json(l)),
        (None, None) => None,
    }
}

/// Test builds only: plugins added to this test process's catalog as if
/// installed (every `Catalog` sees them; an installed copy of the same id
/// shadows one). They have no directory, so Remove and Use previous don't
/// apply to them.
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

    /// The 0.2 platform fixture (`plugins/test-platform`).
    pub(crate) fn platform() -> Arc<Manifest> {
        add(
            &dist_test_text("test-platform/plugin.toml"),
            dist_test_bytes("test-platform/plugin.wasm"),
        )
    }

    /// The privileged fixture (`plugins/test-privileged`): programs, a
    /// tool download, a long agent tool.
    pub(crate) fn privileged() -> Arc<Manifest> {
        add(
            &dist_test_text("test-privileged/plugin.toml"),
            dist_test_bytes("test-privileged/plugin.wasm"),
        )
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

    /// `plugins/dist-test/<dir>` on disk: what a test installs by path (the
    /// first-party releases the build script downloads there, the fixture).
    pub(crate) fn dist_test_dir(dir: &str) -> PathBuf {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/dist-test")
            .join(dir);
        assert!(
            path.join("plugin.wasm").is_file(),
            "{} — run scripts/build-plugins.sh",
            path.display()
        );
        path
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
        .filter(|m| {
            on.contains(&m.id)
                && found.contains(&m.id)
                && m.origin.fault.is_none()
                && trust::hold(state, m).is_none()
        })
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
/// plugin active there, plus agent communication's while it is on (empty —
/// and so no settings change at all, nor a codex terminal's MCP injection —
/// when neither).
pub(crate) async fn spawn_allow(state: &AppState, ws: &str) -> Vec<String> {
    // A build the host refused or faulted here offers nothing, so nothing of
    // it is pre-allowed.
    let mut tools: Vec<String> = active(state, ws)
        .await
        .iter()
        .filter(|m| state.plugin_runtime.fault(m, ws).is_none())
        .flat_map(|m| m.provides.mcp_tools.iter().cloned())
        .collect();
    // Agent communication while it is on: every agent here may see and
    // message the others without a prompt (the switch is the standing
    // permission; an ask-first Mastermind's sends are filtered back out by
    // its gate).
    if crate::comms::enabled(state) {
        tools.extend(crate::comms::TOOLS.iter().map(|t| t.to_string()));
    }
    tools
}

/// The workspace a session belongs to, if any.
pub(crate) fn workspace_of_session(state: &AppState, sid: &str) -> Option<String> {
    crate::lock(&state.session_workspaces).get(sid).cloned()
}

/// A loaded plugin on the wire (`GET /plugins`, `GET /workspaces/{id}/plugins`,
/// and the install/update/rollback/remove/check answers). The fields after
/// `detect` say what is running, where it came from and whether its bytes
/// are the released ones; the optional ones are present only when they hold
/// something.
pub(crate) fn manifest_json(state: &AppState, m: &Manifest) -> Value {
    let o = &m.origin;
    let mut v = manifest_fields(m);
    v["source"] = json!("installed");
    v["installed"] = json!(true);
    // The badge: Chimaera's own plugin, as the maintainers approved it (a
    // first-party update that grew its capabilities is the user's to trust).
    v["first_party"] =
        json!(o.first_party && trust::standing(state, m) == trust::Standing::Verified);
    trust::wire(state, m, &mut v);
    v["verified"] = json!(o.verified);
    v["sha256_wasm"] = json!(&*m.wasm.sha256);
    if let Some(path) = &o.path {
        v["path"] = json!(path);
    }
    if let Some(repo) = &o.release {
        v["repo"] = json!(repo);
    }
    if o.first_party {
        if let Some(l) = lock_entry(&m.id) {
            v["pinned_version"] = json!(l.version);
        }
    }
    if let Some(dir) = &o.local_path {
        v["local_path"] = json!(dir);
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
    if let Some(fault) = &o.fault {
        v["fault"] = json!(fault);
    }
    v
}

/// What a manifest itself says, on the wire: the author's words, what the
/// plugin adds and provides, its version and WIT version. An installed entry
/// (`manifest_json`) and a release described before install
/// (`preview::describe`) both start from it, so the two can't drift.
fn manifest_fields(m: &Manifest) -> Value {
    json!({
        "id": m.id,
        "name": m.name,
        "summary": m.summary,
        "description": text_or_null(m.description.as_deref()),
        "homepage": m.homepage,
        "adds": {"ui": m.adds.ui, "agents": m.adds.agents},
        "provides": {
            "knowledge": m.provides.knowledge,
            "mcp_tools": m.provides.mcp_tools,
            "views": m.provides.views,
        },
        "setup": m.setup.as_ref().map(|s| json!({"prompt": s.prompt})),
        "detect": m.detect.any,
        "requires_summary": text_or_null(m.requires.summary.as_deref()),
        "recommends_summary": text_or_null(m.recommends.summary.as_deref()),
        "version": m.version,
        "api": m.api,
        // What it can do, in the host's words (the card's Can list), its
        // tier and its capability digest (what a trust answer confirms).
        "tier": m.caps.tier().as_str(),
        "caps": m.caps.digest(),
        "can": m.caps.lines_json(),
        // The 0.2 tables: its screens, the file kinds it opens, its file
        // menu items, its settings, its programs and its tools.
        "platform": platform::wire(m),
    })
}

/// An author's optional prose on the wire: trimmed, and `null` when blank
/// (the card then shows nothing rather than an empty paragraph).
fn text_or_null(text: Option<&str>) -> Value {
    match text.map(str::trim) {
        Some(t) if !t.is_empty() => json!(t),
        _ => Value::Null,
    }
}

/// A first-party plugin with nothing installed, on the wire: the lock's
/// name, summary, pinned version and repository — what Install fetches —
/// and nothing a manifest would say (there is none on this host yet).
pub(crate) fn available_json(l: &Locked) -> Value {
    json!({
        "id": l.id,
        "name": l.name,
        "summary": l.summary,
        "description": null,
        "homepage": null,
        "adds": {"ui": [], "agents": []},
        "provides": {"knowledge": null, "mcp_tools": [], "views": []},
        "setup": null,
        "detect": [],
        "requires_summary": null,
        "recommends_summary": null,
        "version": l.version,
        "api": null,
        // The lock's record; the words come with the details (`preview`).
        "tier": l.tier,
        "caps": l.caps,
        "can": [],
        "standing": "verified",
        "hold": null,
        "source": "available",
        "installed": false,
        "first_party": true,
        "verified": false,
        "repo": l.repo,
        "pinned_version": l.version,
    })
}

pub(crate) fn not_found(what: &str) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": what}))).into_response()
}

/// GET /plugins — the catalog (what exists on this daemon, and the
/// first-party plugins it can install).
pub(crate) async fn list_plugins(State(state): State<Arc<AppState>>) -> Response {
    let plugins: Vec<Value> = listing(&state)
        .iter()
        .map(|e| match e {
            Entry::Loaded(m) => manifest_json(&state, m),
            Entry::Available(l) => available_json(l),
        })
        .collect();
    Json(json!({
        "schema": 1,
        "plugins": plugins,
        "policy": trust::policy(&state).json(),
    }))
    .into_response()
}

/// An agent-plugin map on the wire: `[{agent, id, marketplace}]`.
fn agent_plugins_json(map: &BTreeMap<String, AgentPluginReq>) -> Value {
    json!(map
        .iter()
        .map(|(agent, req)| json!({
            "agent": agent,
            "id": req.id,
            "marketplace": req.marketplace,
            "source": req.source,
            "installable": match crate::agents::AgentKind::parse(agent) {
                Some(crate::agents::AgentKind::Claude | crate::agents::AgentKind::Codex) => true,
                Some(crate::agents::AgentKind::Antigravity | crate::agents::AgentKind::Grok) => req.source.is_some(),
                _ => false,
            },
        }))
        .collect::<Vec<_>>())
}

/// GET /workspaces/{id}/plugins — per-plugin status in one workspace: on /
/// footprint detected / active. Agent-plugin state (asked of the agents
/// themselves) rides `GET /workspaces/{id}/agent-plugins`.
pub(crate) async fn workspace_plugins(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&id) else {
        return not_found("unknown workspace");
    };
    let on: BTreeSet<String> = workspace.plugins_on.iter().cloned().collect();
    let found = refresh_detect(&state, &id).await;
    let plugins: Vec<Value> = listing(&state)
        .iter()
        .map(|e| {
            let m = match e {
                Entry::Loaded(m) => m,
                // Nothing to switch on until it is installed; a switch kept
                // from before a Remove holds again after the next install.
                Entry::Available(l) => {
                    let mut v = available_json(l);
                    for key in ["on", "detected", "active"] {
                        v[key] = json!(false);
                    }
                    v["requires"] = json!([]);
                    v["recommends"] = json!([]);
                    return v;
                }
            };
            let mut v = manifest_json(&state, m);
            // A plugin that can't run is off whatever its switch says: the
            // switch is kept, and holds again once the fault (or the hold:
            // a block, the policy, missing trust) is gone.
            let is_on = on.contains(&m.id) && m.origin.fault.is_none() && v["hold"].is_null();
            let detected = found.contains(&m.id);
            v["on"] = json!(is_on);
            v["detected"] = json!(detected);
            v["active"] = json!(is_on && detected);
            // Additive, and only when there is one: why the plugin isn't
            // answering here (the card shows it). A catalog fault already
            // said why (`manifest_json`).
            if m.origin.fault.is_none() {
                if let Some(fault) = state.plugin_runtime.fault(m, &id) {
                    v["fault"] = json!(fault);
                }
            }
            v["requires"] = agent_plugins_json(&m.requires.agent_plugins);
            v["recommends"] = agent_plugins_json(&m.recommends.agent_plugins);
            v
        })
        .collect();
    Json(json!({
        "schema": 1,
        "workspace_id": id,
        "root": workspace.root,
        "plugins": plugins,
        "policy": trust::policy(&state).json(),
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
    match manifest(&state, &pid) {
        Some(m) if body.on => {
            if let Some(fault) = &m.origin.fault {
                // A retired plugin's fault is the whole answer already.
                if let Some(r) = retired::of(&m.id) {
                    return r.refusal().into_response();
                }
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error": format!("{} can't run on this daemon: {fault}", m.name)})),
                )
                    .into_response();
            }
            match trust::hold(&state, &m) {
                None => {}
                Some(trust::Hold::Untrusted) => {
                    let source = trust::source_of(&m);
                    return trust::needs_trust(&m, &source, &m.wasm.sha256, Some(&m), false)
                        .into_response();
                }
                Some(trust::Hold::Blocked(b)) => {
                    return Refusal::conflict(format!(
                        "Chimaera blocked {} {}: {}",
                        m.name, m.version, b.reason
                    ))
                    .with(json!({"blocked": revoke::block_json(&b)}))
                    .into_response()
                }
                Some(trust::Hold::Policy(why)) => {
                    return Refusal::new(
                        StatusCode::FORBIDDEN,
                        format!("{} can't run here: {why}", m.name),
                    )
                    .into_response()
                }
            }
        }
        Some(_) => {}
        // Nothing installed has nothing to run; switching it off is still
        // fine — it clears a switch kept from before a Remove, first-party
        // or not (a third-party one has no card left to do it from).
        None if body.on => return not_installed(&pid).into_response(),
        None if !valid_id(&pid) => return not_found("unknown plugin"),
        None => {}
    }
    let save_failed = |err: anyhow::Error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("could not save the workspace: {err}")})),
        )
            .into_response()
    };
    // Only a plugin that was on hears it is going (clearing a switch kept
    // from before never runs its code).
    let was_on = crate::lock(&state.workspaces)
        .get(&id)
        .is_some_and(|w| w.plugins_on.iter().any(|p| p == &pid));
    let staged = crate::lock(&state.workspaces).stage_plugin_on(&id, &pid, body.on);
    let crate::workspaces::PluginSwitch {
        workspace,
        snapshot,
        undo,
    } = match staged {
        Ok(Some(switch)) => switch,
        Ok(None) => return not_found("unknown workspace"),
        Err(err) => return save_failed(err),
    };
    // Written off the workspaces lock (every route takes it; the file may
    // be on NFS), and taken back if it can't be.
    let written = tokio::task::spawn_blocking(move || snapshot.write())
        .await
        .unwrap_or_else(|err| Err(anyhow::anyhow!("the write task failed: {err}")));
    if let Err(err) = written {
        crate::lock(&state.workspaces).undo_plugin_on(&id, &pid, undo);
        return save_failed(err);
    }
    // A plugin that asked to hear it is told it is going (on the instance
    // it had, so it can let go of what it kept), before it starts over.
    let heard = |kind| manifest(&state, &pid).filter(|m| m.provides.hears(kind));
    if !body.on && was_on {
        if let Some(m) = heard(EventKind::SwitchedOff) {
            state
                .plugin_runtime
                .on_event(&state, &m, &id, None, runtime::wit::Event::SwitchedOff)
                .await;
        }
    }
    if !body.on {
        // Its programs stop with it, and what it published here goes.
        jobs::cancel_where(&state, &pid, Some(&id));
        crate::lock(&state.plugin_platform.surfaces).forget_pair(&pid, &id);
    }
    // Either way the plugin starts over here: a fresh instance on next use,
    // and a fault cleared (switching off and on is how the user retries a
    // faulted plugin).
    state.plugin_runtime.reset(&pid, &id);
    crate::lock(&state.knowledge).forget_provider(&pid, Some(&id));
    // The switch is the moment agents' view changes: re-detect now so the
    // next connect answers from a fresh footprint.
    refresh_detect(&state, &id).await;
    if body.on {
        runtime::warm(&state, Some(pid.clone()), std::time::Duration::ZERO);
        if let Some(m) = heard(EventKind::SwitchedOn) {
            let (state, id) = (state.clone(), id.clone());
            // Off the request: the switch answers at once. Delivered only
            // where it is active (its footprint found).
            tokio::spawn(async move {
                if active(&state, &id).await.iter().any(|a| a.id == m.id) {
                    state
                        .plugin_runtime
                        .on_event(&state, &m, &id, None, runtime::wit::Event::SwitchedOn)
                        .await;
                }
            });
        }
    }
    state.changes.notify_waiters();
    Json(json!({"workspace_id": id, "plugins_on": workspace.plugins_on})).into_response()
}

/// Switch `pid` off in every workspace, durably — see
/// `WorkspaceStore::clear_plugin`. The write runs off the workspaces lock.
pub(crate) async fn clear_switches(state: &AppState, pid: &str) -> anyhow::Result<()> {
    let snapshot = crate::lock(&state.workspaces).clear_plugin(pid)?;
    let Some(snapshot) = snapshot else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || snapshot.write())
        .await
        .unwrap_or_else(|err| Err(anyhow::anyhow!("the write task failed: {err}")))?;
    tracing::info!(plugin = pid, "plugin switched off in every workspace");
    Ok(())
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

#[derive(Deserialize)]
pub(crate) struct InstallAgentBody {
    agent: String,
    /// Absent for older clients: preserve required-before-recommended selection.
    agent_plugin_id: Option<String>,
}

pub(crate) fn bad_request(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": msg.into()}))).into_response()
}

/// POST /workspaces/{id}/plugins/{pid}/install {agent, agent_plugin_id?} — install the agent
/// plugin this workbench plugin requires or recommends, with the AGENT's own
/// plugin manager, in a visible terminal the user watches (chimaera never
/// reimplements `claude plugin` / `codex plugin`). The session is theirs to
/// read and close; the probe cache is invalidated when the command finishes,
/// even while its terminal waits for the user to dismiss the result.
pub(crate) async fn install_requirement(
    State(state): State<Arc<AppState>>,
    AxPath((id, pid)): AxPath<(String, String)>,
    Json(body): Json<InstallAgentBody>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&id) else {
        return not_found("unknown workspace");
    };
    let Some(m) = manifest(&state, &pid) else {
        return not_found("unknown plugin");
    };
    let Some(req) = m.agent_plugin_matching(&body.agent, body.agent_plugin_id.as_deref()) else {
        return bad_request(format!(
            "{} names no matching {} plugin",
            m.name, body.agent
        ));
    };
    let Some(kind) = crate::agents::AgentKind::parse(&body.agent) else {
        return bad_request("unknown agent");
    };
    if matches!(
        kind,
        crate::agents::AgentKind::Antigravity | crate::agents::AgentKind::Grok
    ) {
        let Some(source) = req.source.clone() else {
            return bad_request("This plugin needs to declare an installation source for this agent. Manage it in the agent meanwhile.");
        };
        return crate::agent_probe::actions::run(
            State(state),
            AxPath(id),
            Json(crate::agent_probe::actions::Action {
                agent: body.agent,
                action: "install_plugin".into(),
                target: Some(source),
            }),
        )
        .await;
    }
    if !matches!(
        kind,
        crate::agents::AgentKind::Claude | crate::agents::AgentKind::Codex
    ) {
        return bad_request("Plugin installation is unavailable for this agent.");
    }
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
    let install_verb = if kind == crate::agents::AgentKind::Codex {
        "add"
    } else {
        "install"
    };
    let agent = kind.as_str();
    let session_id = crate::agents::fresh_session_id();
    let prepare_state = state.clone();
    let prepare_id = session_id.clone();
    let prepare_workspace = workspace.id.clone();
    // Keep completion out of the untrusted PTY stream. The runtime directory
    // is private and the random, exclusively created marker is one byte at most.
    let (completion, prelude) = match tokio::task::spawn_blocking(move || {
        let path = chimaera_core::runtime_dir().join(format!(
            "plugin-install-{}",
            chimaera_core::generate_token()
        ));
        std::fs::File::create_new(&path)?;
        // Agent-plugin managers need the same modules/PATH as agents in this
        // workspace. Runtime bootstrap installers have a separate contract.
        let startup = crate::environment::job_startup_blocking();
        let prelude = crate::environment::materialize_prelude(
            &prepare_state,
            &prepare_id,
            &prepare_workspace,
            None,
            startup.as_deref(),
        );
        Ok::<_, std::io::Error>((path, prelude))
    })
    .await
    {
        Ok(Ok(path)) => path,
        err => {
            tracing::warn!(?err, "could not prepare plugin install completion marker");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "could not prepare plugin installation"})),
            )
                .into_response();
        }
    };
    let env = crate::api::session_env(&state, &session_id, "dark", prelude.as_deref());
    let env_remove = crate::api::spawn_env_remove(&env);
    // The agent's plugin manager runs a bare `git`; the Git binary path
    // setting's goes first on its PATH, after the login shell and the
    // Environment prelude have run.
    let git_dir = crate::git::usable_git_dir(&state)
        .await
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
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
            // Values are arguments, never interpolated shell source. Plugin
            // names and executable paths can contain quotes or shell syntax.
            vec![
                "/bin/bash".to_string(),
                "-c".to_string(),
                include_str!("install-agent.sh").to_string(),
                "chimaera-plugin-install".to_string(),
                m.name.clone(),
                agent.to_string(),
                bin.to_string_lossy().into_owned(),
                req.marketplace.clone(),
                req.id.clone(),
                install_verb.to_string(),
                completion.to_string_lossy().into_owned(),
                git_dir,
            ],
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
                    if tokio::fs::metadata(&completion)
                        .await
                        .is_ok_and(|metadata| metadata.len() > 0)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                let _ = tokio::fs::remove_file(&completion).await;
                if let Some(prelude) = prelude {
                    let _ = tokio::fs::remove_file(prelude).await;
                }
                watch_state.probes.changed();
                watch_state.changes.notify_waiters();
            });
            tracing::info!(workspace = %id, plugin = %pid, agent, "plugin requirement install started");
            state.changes.notify_waiters();
            Json(json!({"session_id": info.id})).into_response()
        }
        Err(err) => {
            let _ = tokio::fs::remove_file(&completion).await;
            if let Some(prelude) = prelude {
                let _ = tokio::fs::remove_file(prelude).await;
            }
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
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
            started_by: crate::history::StartedBy::You,
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

    /// Each first-party release the lock pins, as the build script laid it
    /// out in `plugins/dist-test/<id>/` for the tests: its manifest parsed,
    /// and the raw bytes of both files.
    fn locked_releases() -> Vec<(&'static Locked, Manifest, Vec<u8>, Vec<u8>)> {
        lock_entries()
            .iter()
            .map(|l| {
                let toml = test_catalog::dist_test_bytes(&format!("{}/plugin.toml", l.id));
                let wasm = test_catalog::dist_test_bytes(&format!("{}/plugin.wasm", l.id));
                let m = parse_manifest(std::str::from_utf8(&toml).unwrap())
                    .unwrap_or_else(|e| panic!("{}: {e}", l.id));
                (l, m, toml, wasm)
            })
            .collect()
    }

    #[test]
    fn the_embedded_lock_parses_and_names_well_formed_releases() {
        let lock = parse_lock(LOCK).expect("plugins/plugins.lock parses");
        assert!(!lock.is_empty(), "plugins/plugins.lock names no plugin");
        assert_eq!(
            lock_entries().len(),
            lock.len(),
            "the daemon reads the same lock"
        );
        let ids: Vec<&str> = lock.iter().map(|l| l.id.as_str()).collect();
        assert!(
            ids.windows(2).all(|p| p[0] < p[1]),
            "sorted, each once: {ids:?}"
        );
        assert!(lock_entry("mycelium").is_some());
        assert!(
            lock_entry("test-fixture").is_none(),
            "the fixture is never first-party"
        );

        // What parse_lock refuses.
        let entry = |extra: &str| {
            format!(
                "[[plugin]]\nid = \"x\"\nname = \"X\"\nsummary = \"x\"\nversion = \"0.1.0\"\n\
                 repo = \"a/b\"\nsha256_wasm = \"{0}\"\nsha256_toml = \"{0}\"\n\
                 tier = \"sandboxed\"\ncaps = \"{0}\"\n{extra}",
                "a".repeat(64)
            )
        };
        assert!(parse_lock(&entry("")).is_ok());
        assert!(parse_lock(&entry("url = \"x\"\n")).is_err(), "unknown keys");
        assert!(
            parse_lock(&format!("{}{}", entry(""), entry(""))).is_err(),
            "twice"
        );
        for (from, to) in [
            ("id = \"x\"", "id = \"X\""),
            ("version = \"0.1.0\"", "version = \"0.1\""),
            ("repo = \"a/b\"", "repo = \"../b\""),
            ("summary = \"x\"", "summary = \" \""),
            ("tier = \"sandboxed\"", "tier = \"trusted\""),
        ] {
            assert!(
                parse_lock(&entry("").replacen(from, to, 1)).is_err(),
                "{to}"
            );
        }
        let upper = entry("").replacen(&"a".repeat(64), &"A".repeat(64), 1);
        assert!(
            parse_lock(&upper).is_err(),
            "lowercase hex, as SHA256SUMS writes it"
        );
    }

    #[test]
    fn every_locked_release_is_what_the_lock_says() {
        for (l, m, toml, wasm) in locked_releases() {
            let id = &l.id;
            assert_eq!(
                crate::fs::sha256_hex(&wasm),
                l.sha256_wasm,
                "{id}: plugin.wasm"
            );
            assert_eq!(
                crate::fs::sha256_hex(&toml),
                l.sha256_toml,
                "{id}: plugin.toml"
            );
            assert_eq!(m.id, *id);
            assert_eq!(
                m.version, l.version,
                "{id}: the lock pins its manifest's version"
            );
            assert_eq!(m.name, l.name, "{id}: the lock's name is its manifest's");
            assert_eq!(
                m.release.as_ref().map(|r| r.github.as_str()),
                Some(l.repo.as_str()),
                "{id}: it updates from the repository the lock pins"
            );
            assert!(SERVED_APIS.contains(&m.api.as_str()), "{id}: api {}", m.api);
            assert_eq!(gate(&m, "0.4.1"), None, "{id} runs on a released daemon");
            // What the maintainers approved it to do is what it can do:
            // `chimaera plugin caps` prints both for a release's manifest.
            assert_eq!(
                (m.caps.tier().as_str(), m.caps.digest().as_str()),
                (l.tier.as_str(), l.caps.as_str()),
                "{id}: the lock's tier and caps are the release's (chimaera plugin caps plugin.toml)"
            );
            assert!(!m.summary.is_empty());
            assert!(
                !m.adds.ui.is_empty() || !m.adds.agents.is_empty(),
                "{id} must say what it adds"
            );
            for rel in &m.detect.any {
                assert!(
                    !rel.starts_with('/') && !rel.contains(".."),
                    "{id}: detect paths are workspace-relative"
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
        for (l, m, _, _) in locked_releases() {
            let named = m
                .requires
                .agent_plugins
                .values()
                .chain(m.recommends.agent_plugins.values());
            for req in named {
                assert!(cli_safe(&req.id) && cli_safe(&req.marketplace), "{}", l.id);
            }
        }
    }

    #[test]
    fn native_plugin_sources_are_additive_and_unknown_agents_are_not_installable() {
        let m = demo(
            r#"version = "0.1.0"
api = "0.1"
[requires.agent_plugins.agy]
id = "kit"
source = "/tmp/antigravity-kit"
[requires.agent_plugins.grok]
id = "kit"
source = "https://example.test/kit.git"
[requires.agent_plugins.future]
id = "future-kit"
"#,
        )
        .unwrap();
        let rows = agent_plugins_json(&m.requires.agent_plugins);
        let rows = rows.as_array().unwrap();
        assert_eq!(
            rows.iter().find(|r| r["agent"] == "agy").unwrap()["installable"],
            true
        );
        assert_eq!(
            rows.iter().find(|r| r["agent"] == "grok").unwrap()["source"],
            "https://example.test/kit.git"
        );
        assert_eq!(
            rows.iter().find(|r| r["agent"] == "future").unwrap()["installable"],
            false
        );
    }

    #[test]
    fn requires_and_recommends_both_name_an_agent_plugin() {
        let m = demo(
            "version = \"0.1.0\"\napi = \"0.1\"\n\
             [requires.agent_plugins.codex]\nid = \"need@x\"\nmarketplace = \"x/need\"\n\
             [recommends.agent_plugins.claude]\nid = \"nice@x\"\nmarketplace = \"x/nice\"\n\
             [recommends.agent_plugins.codex]\nid = \"also@x\"\nmarketplace = \"x/also\"\n",
        )
        .unwrap();
        assert_eq!(
            m.agent_plugin_matching("codex", None).unwrap().id,
            "need@x",
            "a requirement wins"
        );
        assert_eq!(
            m.agent_plugin_matching("claude", None).unwrap().id,
            "nice@x"
        );
        assert!(m.agent_plugin_matching("agy", None).is_none());
        assert_eq!(m.agent_plugin_ids("codex"), ["need@x", "also@x"]);
        assert_eq!(m.agent_plugin_ids("claude"), ["nice@x"]);
        assert!(m.agent_plugin_ids("agy").is_empty());
        assert_eq!(
            m.agent_plugin_matching("codex", Some("also@x"))
                .unwrap()
                .marketplace,
            "x/also"
        );
        assert!(m.agent_plugin_matching("codex", Some("nice@x")).is_none());
        assert!(m
            .agent_plugin_matching("codex", Some("undeclared@x"))
            .is_none());
        assert!(
            demo("version = \"0.1.0\"\napi = \"0.1\"\n[recommends]\nchimaera = \">=1\"\n").is_err()
        );
    }

    /// Mycelium 0.1.2's shape: the author's description, and the sentence
    /// the card says about its recommended agent-side plugin.
    #[test]
    fn a_manifest_carries_its_authors_words_for_the_card() {
        let m = demo(
            "version = \"0.1.2\"\napi = \"0.1\"\n\
             description = \"Mycelium is the Arjun Raj lab's living-repository framework: \
             agents record what they find as they work.\"\n\
             [recommends]\nsummary = \"Mycelium's own agent plugin gives claude and codex the \
             skills that record findings, decisions and learnings as they work. Install it for \
             the agents you use; the Knowledge view reads .living/ either way.\"\n\
             [recommends.agent_plugins.claude]\nid = \"mycelium@mycelium\"\n\
             marketplace = \"arjunrajlaboratory/mycelium\"\n\
             [recommends.agent_plugins.codex]\nid = \"mycelium@mycelium\"\n\
             marketplace = \"arjunrajlaboratory/mycelium\"\n",
        )
        .unwrap();
        assert!(m
            .description
            .as_deref()
            .unwrap()
            .starts_with("Mycelium is the Arjun Raj lab's"));
        assert!(m
            .recommends
            .summary
            .as_deref()
            .unwrap()
            .ends_with("reads .living/ either way."));
        assert_eq!(m.recommends.agent_plugins.len(), 2);
        assert_eq!(m.requires.summary, None);

        let m = demo(
            "version = \"0.1.0\"\napi = \"0.1\"\n[requires]\nsummary = \"Needed.\"\n\
             [requires.agent_plugins.codex]\nid = \"need@x\"\nmarketplace = \"x/need\"\n",
        )
        .unwrap();
        assert_eq!(m.requires.summary.as_deref(), Some("Needed."));
        assert_eq!(m.description, None, "optional");

        // Blank prose never reaches the card as an empty paragraph.
        assert_eq!(text_or_null(Some("  ")), Value::Null);
        assert_eq!(text_or_null(None), Value::Null);
        assert_eq!(text_or_null(Some(" Hi. ")), json!("Hi."));
        assert!(
            demo("version = \"0.1.0\"\napi = \"0.1\"\n[recommends]\nsummaries = \"x\"\n").is_err(),
            "a typo is an error"
        );
    }

    #[test]
    fn unknown_manifest_fields_are_rejected() {
        let err = toml::from_str::<Manifest>(
            "id='x'\nname='x'\nversion='0.1.0'\nsummary='x'\napi='0.1'\n[provides]\nmcp_tool=['typo']",
        );
        assert!(err.is_err());
    }

    #[test]
    fn tool_names_cannot_claim_core_tools_or_permission_wildcards() {
        for name in [
            "document_guide",
            "notify",
            "run_in_terminal",
            "spawn_agent",
            "message_agent",
            "workspace_agents",
            "*",
            "echo(*)",
            "",
            "echo echo",
        ] {
            assert!(
                demo(&format!(
                    "version='0.1.0'\napi='0.1'\n[provides]\nmcp_tools=['{name}']"
                ))
                .is_err(),
                "{name}"
            );
        }
        assert!(
            demo("version='0.1.0'\napi='0.1'\n[provides]\nmcp_tools=['echo', 'echo']").is_err()
        );
        assert!(
            demo("version='0.1.0'\napi='0.1'\n[provides]\nmcp_tools=['my_plugin-echo-V2']").is_ok()
        );
        // A dot would split codex's dotted pre-approval key: refused here,
        // never skipped silently at spawn.
        assert!(
            demo("version='0.1.0'\napi='0.1'\n[provides]\nmcp_tools=['my_plugin.echo']").is_err()
        );
    }

    #[test]
    fn detect_paths_stay_inside_the_workspace() {
        for bad in ["../x", "/etc/passwd", "a/../../b", "./x", ""] {
            assert!(
                demo(&format!(
                    "version='0.1.0'\napi='0.1'\n[detect]\nany=['{bad}']"
                ))
                .is_err(),
                "{bad}"
            );
        }
        assert!(demo("version='0.1.0'\napi='0.1'\n[detect]\nany=['.living/INDEX.md']").is_ok());
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
        assert!(valid_id("mycelium") && valid_id("latex2"));
        for bad in [
            "", "-x", "X", "a/b", "a.b", "..", "install", "preview", "a b",
        ] {
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
        assert_eq!(gate(&with("0.2", None), "0.4.1"), None);
        let newer = gate(&with("0.9", None), "0.4.1").unwrap();
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

    fn copy(id: &str, version: &str, previous: Option<&str>) -> installed::InstalledCopy {
        let mut m = parse_manifest(&format!(
            "id = \"{id}\"\nname = \"Demo\"\nsummary = \"x\"\nversion = \"{version}\"\n\
             api = \"0.1\"\n[release]\ngithub = \"acme/{id}\"\n"
        ))
        .unwrap();
        m.origin.verified = true;
        installed::InstalledCopy {
            manifest: m,
            dir: PathBuf::from(format!("/p/{id}/{version}")),
            previous: previous.map(str::to_string),
        }
    }

    #[test]
    fn the_catalog_is_the_installed_copies_each_gated() {
        let extra = Arc::new(demo("version = \"9.0.0\"\napi = \"0.1\"\n").unwrap());
        let mut faulted = copy("zeta", "0.1.0", None);
        faulted.manifest.origin.fault = Some("its files do not match".into());
        faulted.manifest.api = "0.9".into();
        let all = resolve(
            &[extra],
            &[
                copy("demo", "0.3.2", Some("0.3.0")),
                copy("api-new", "1.0.0", None),
                faulted,
            ],
            "0.4.1",
        );
        let ids: Vec<&str> = all.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            ["api-new", "demo", "zeta"],
            "sorted; an installed copy shadows an extra"
        );
        let m = &all[1];
        assert_eq!(m.version, "0.3.2", "the installed copy, not the extra");
        assert_eq!(m.origin.previous.as_deref(), Some("0.3.0"));
        assert_eq!(m.origin.path, Some(PathBuf::from("/p/demo/0.3.2")));
        assert_eq!(m.origin.release.as_deref(), Some("acme/demo"));
        assert!(m.origin.verified, "what reading the copy found is kept");
        assert_eq!(m.origin.fault, None);
        assert_eq!(
            all[2].origin.fault.as_deref(),
            Some("its files do not match"),
            "a fault found reading the copy wins over the gates"
        );

        // A gate fails: listed, with why.
        let mut newer = copy("demo", "0.4.0", None);
        newer.manifest.api = "0.9".into();
        let m = &resolve(&[], &[newer], "0.4.1")[0];
        assert!(m
            .origin
            .fault
            .as_deref()
            .unwrap()
            .starts_with("needs a newer chimaera"));

        // A test extra alone: listed with no directory and no release.
        let extra = Arc::new(demo("version = \"1.0.0\"\napi = \"0.1\"\n").unwrap());
        let m = &resolve(&[extra], &[], "0.4.1")[0];
        assert_eq!(m.origin.path, None);
        assert_eq!(m.origin.release, None, "nothing names a release source");
    }

    #[test]
    fn detect_needs_a_real_footprint_and_ignores_symlinks() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-plugins-detect-{}-{}",
            std::process::id(),
            crate::timeline::now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let catalog: Vec<Arc<Manifest>> = locked_releases()
            .into_iter()
            .map(|(_, m, _, _)| Arc::new(m))
            .collect();
        let catalog = &catalog;
        let none = detect_blocking(&root, catalog);
        assert!(!none.contains("mycelium"));
        assert!(none.contains("latex"), "no footprint = always present");
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
