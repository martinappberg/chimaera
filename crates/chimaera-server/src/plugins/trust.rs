//! Trust: who approved what a plugin can do, and whether it may run here.
//! Design: docs/plugin-platform-plan.md §2 ("Trust and verification").
//!
//! - **Standing.** A build is *verified* when the lock covers it (the
//!   release chimaera pins, byte for byte, or a sandboxed first-party update
//!   whose capability digest is the one the lock recorded), *trusted* when
//!   a trust record covers it (the user approved exactly that capability
//!   digest from that source), and *untrusted* otherwise: it stays installed
//!   and off until the user trusts what it can do.
//! - **Trust records** live in `<data dir>/plugins/trust.json` (small,
//!   capped, rewritten atomically): `{id, source, caps, version, granted_ms,
//!   how}`. A record covers a digest, not a version: an update that asks for
//!   nothing new needs no new answer. The first daemon to read no file
//!   writes one that trusts every copy already installed (they were the
//!   user's own installs, from before chimaera asked); a file that doesn't
//!   parse trusts nothing (fail closed) until the user trusts again.
//! - **Admission** (`admit`): an install, update or rollback runs only a
//!   build that is verified, trusted, a subset of the covered build it
//!   replaces, or confirmed by the caller with the digest the user was shown
//!   (`trust`). Anything else is refused with `409` and the capabilities to
//!   show (`needs_trust`), so the window or the CLI can ask and try again.
//! - **Policy** (`policy`): the host setting `plugins.allowUnverified` and a
//!   machine-wide `/etc/chimaera/policy.json` (`allowUnverified`,
//!   `allowPrivileged`, `blocked`), which can only tighten. A file that
//!   doesn't parse fails closed: no unverified and no privileged plugin.
//! - **Holds** (`hold`): why a loaded plugin may not run here — blocked
//!   (the kill switch or the policy's list), refused by policy, or untrusted.
//!   `active` never returns a held plugin.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::capabilities::Tier;
use super::revoke::{self, Block, Level};
use super::{Manifest, Refusal};
use crate::AppState;

const TRUST_FILE: &str = "trust.json";
const RECORDS_MAX: usize = 512;
/// The machine-wide policy (an admin's; users can't loosen it).
const POLICY_FILE: &str = "/etc/chimaera/policy.json";
const POLICY_MAX: u64 = 64 << 10;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Record {
    pub(crate) id: String,
    /// `github:<owner/repo>` (lowercase) or `path:<absolute dir>`.
    pub(crate) source: String,
    /// The capability digest the user approved.
    pub(crate) caps: String,
    /// The version they approved it at (for the record; it covers others).
    pub(crate) version: String,
    pub(crate) granted_ms: u64,
    /// `prompt` (the user's answer), `subset` (asked for no more than a
    /// covered build it replaced), `grandfathered` (installed before
    /// chimaera asked).
    pub(crate) how: String,
}

/// A soft-blocked build the user switched back on anyway.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct AllowedBlock {
    id: String,
    sha256: String,
    granted_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TrustFile {
    schema: u32,
    #[serde(default)]
    records: Vec<Record>,
    /// id → the version whose growth the user skipped (its update isn't
    /// offered again).
    #[serde(default)]
    skipped: BTreeMap<String, String>,
    #[serde(default)]
    allowed_blocks: Vec<AllowedBlock>,
}

#[derive(Debug, Default)]
struct TrustStore {
    file: TrustFile,
    /// Why the file on disk couldn't be read: nothing it said is trusted.
    broken: Option<String>,
}

/// `allowPrivileged`: which plugins that run programs a host allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Privileged {
    All,
    Verified,
    None,
}

impl Privileged {
    fn as_str(self) -> &'static str {
        match self {
            Privileged::All => "all",
            Privileged::Verified => "verified",
            Privileged::None => "none",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PolicyPlugins {
    #[serde(default)]
    allow_unverified: Option<bool>,
    #[serde(default)]
    allow_privileged: Option<Privileged>,
    #[serde(default)]
    blocked: Vec<String>,
}

/// What the machine-wide file says (`Default`: there is none).
#[derive(Debug, Clone, Default)]
struct FilePolicy {
    present: bool,
    plugins: PolicyPlugins,
    /// It didn't parse: fail closed.
    error: Option<String>,
}

fn load_policy_file(path: &Path) -> FilePolicy {
    #[derive(Deserialize)]
    struct Top {
        #[serde(default)]
        plugins: Option<PolicyPlugins>,
    }
    let text = match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FilePolicy::default(),
        Ok(md) if md.len() > POLICY_MAX => Err(format!("over its {} KiB cap", POLICY_MAX >> 10)),
        _ => std::fs::read_to_string(path).map_err(|e| e.to_string()),
    };
    let parsed = text.and_then(|t| serde_json::from_str::<Top>(&t).map_err(|e| e.to_string()));
    match parsed {
        Ok(top) => FilePolicy {
            present: true,
            plugins: top.plugins.unwrap_or_default(),
            error: None,
        },
        Err(err) => {
            tracing::error!(path = %path.display(), %err, "plugin policy file unreadable: unverified and privileged plugins are refused");
            FilePolicy {
                present: true,
                plugins: PolicyPlugins::default(),
                error: Some(format!("{}: {err}", path.display())),
            }
        }
    }
}

/// The policy in force: the host setting and the machine-wide file.
#[derive(Debug, Clone)]
pub(crate) struct Policy {
    pub(crate) allow_unverified: bool,
    pub(crate) allow_privileged: Privileged,
    pub(crate) blocked: BTreeSet<String>,
    /// A machine-wide file is in force.
    pub(crate) managed: bool,
    /// The machine-wide file didn't parse (the policy failed closed).
    pub(crate) error: Option<String>,
}

impl Policy {
    pub(crate) fn json(&self) -> Value {
        json!({
            "allow_unverified": self.allow_unverified,
            "allow_privileged": self.allow_privileged.as_str(),
            "managed": self.managed,
            "error": self.error,
        })
    }
}

/// The trust records, the policy file and the kill switch (on `AppState`).
pub(crate) struct Guard {
    path: PathBuf,
    store: Mutex<TrustStore>,
    /// Trust writes, one at a time, each writing the store as it is then.
    writing: tokio::sync::Mutex<()>,
    policy_path: RwLock<PathBuf>,
    policy_file: RwLock<FilePolicy>,
    pub(crate) revoked: RwLock<revoke::Revocations>,
}

impl Guard {
    /// Read the trust records (writing the first file when there is none),
    /// the policy file and the revocation lists (blocking; boot).
    pub(crate) fn load(catalog: &super::Catalog) -> Guard {
        let path = catalog.root.join(TRUST_FILE);
        let store = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<TrustFile>(&text) {
                Ok(file) if file.schema == 1 => TrustStore { file, broken: None },
                Ok(file) => TrustStore {
                    file: TrustFile::default(),
                    broken: Some(format!("unknown schema {}", file.schema)),
                },
                Err(err) => TrustStore {
                    file: TrustFile::default(),
                    broken: Some(err.to_string()),
                },
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let store = TrustStore {
                    file: grandfathered(catalog),
                    broken: None,
                };
                if let Err(err) = write_file(&path, &store.file) {
                    tracing::warn!(%err, "plugin trust records not written");
                }
                store
            }
            Err(err) => TrustStore {
                file: TrustFile::default(),
                broken: Some(err.to_string()),
            },
        };
        if let Some(err) = &store.broken {
            tracing::error!(path = %path.display(), %err, "plugin trust records unreadable: no third-party plugin runs until trusted again");
        }
        let policy_path = std::env::var_os("CHIMAERA_POLICY_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(POLICY_FILE));
        Guard {
            path,
            store: Mutex::new(store),
            writing: tokio::sync::Mutex::new(()),
            policy_file: RwLock::new(load_policy_file(&policy_path)),
            policy_path: RwLock::new(policy_path),
            revoked: RwLock::new(revoke::Revocations::load(&catalog.root)),
        }
    }

    fn trusts(&self, id: &str, source: &str, caps: &str) -> bool {
        crate::lock(&self.store)
            .file
            .records
            .iter()
            .any(|r| r.id == id && r.source == source && r.caps == caps)
    }

    pub(crate) fn skipped(&self, id: &str) -> Option<String> {
        crate::lock(&self.store).file.skipped.get(id).cloned()
    }

    fn block_allowed(&self, id: &str, sha256: &str) -> bool {
        crate::lock(&self.store)
            .file
            .allowed_blocks
            .iter()
            .any(|a| a.id == id && a.sha256 == sha256)
    }

    /// Re-read the machine-wide policy file (blocking).
    pub(crate) fn reload_policy(&self) {
        let path = super::read(&self.policy_path).clone();
        *super::write(&self.policy_file) = load_policy_file(&path);
    }

    /// Tests only: read the policy from `path` instead.
    #[cfg(test)]
    pub(crate) fn set_policy_path_for_tests(&self, path: PathBuf) {
        *super::write(&self.policy_path) = path;
        self.reload_policy();
    }
}

fn write_file(path: &Path, file: &TrustFile) -> anyhow::Result<()> {
    let text = serde_json::to_vec_pretty(file)?;
    crate::persist::atomic_write_json(path, text)
}

/// The first trust file: every copy already installed that the lock
/// doesn't cover, trusted as it is — the user installed each of them.
fn grandfathered(catalog: &super::Catalog) -> TrustFile {
    let now = crate::timeline::now_ms();
    let records = catalog
        .installed_copies()
        .iter()
        .filter(|c| !c.manifest.origin.first_party)
        .map(|c| Record {
            id: c.manifest.id.clone(),
            source: source_of(&c.manifest),
            caps: c.manifest.caps.digest(),
            version: c.manifest.version.clone(),
            granted_ms: now,
            how: "grandfathered".into(),
        })
        .collect();
    TrustFile {
        schema: 1,
        records,
        ..Default::default()
    }
}

/// Write the store as it is now (off the reactor, one write at a time).
async fn persist(state: &AppState) -> Result<(), Refusal> {
    let _one = state.plugin_guard.writing.lock().await;
    let (path, file) = {
        let store = crate::lock(&state.plugin_guard.store);
        (state.plugin_guard.path.clone(), store.file.clone())
    };
    tokio::task::spawn_blocking(move || write_file(&path, &file))
        .await
        .map_err(|e| Refusal::internal(format!("the trust write failed: {e}")))?
        .map_err(|e| Refusal::internal(format!("could not save the trust records: {e:#}")))?;
    crate::lock(&state.plugin_guard.store).broken = None;
    Ok(())
}

fn now_record(m: &Manifest, source: String, how: &str) -> Record {
    Record {
        id: m.id.clone(),
        source,
        caps: m.caps.digest(),
        version: m.version.clone(),
        granted_ms: crate::timeline::now_ms(),
        how: how.into(),
    }
}

/// Add `record` (replacing the same id + source), keeping the newest
/// `RECORDS_MAX`.
fn insert(state: &AppState, record: Record) {
    let mut store = crate::lock(&state.plugin_guard.store);
    let records = &mut store.file.records;
    records.retain(|r| !(r.id == record.id && r.source == record.source && r.caps == record.caps));
    records.push(record);
    if records.len() > RECORDS_MAX {
        records.sort_by_key(|r| std::cmp::Reverse(r.granted_ms));
        records.truncate(RECORDS_MAX);
    }
    store.file.schema = 1;
}

/// Where an installed copy came from, as a trust record names it.
pub(crate) fn source_of(m: &Manifest) -> String {
    if let Some(dir) = &m.origin.local_path {
        return format!("path:{}", dir.display());
    }
    match m
        .origin
        .source_github
        .as_deref()
        .or(m.release.as_ref().map(|r| r.github.as_str()))
    {
        Some(repo) => format!("github:{}", repo.to_ascii_lowercase()),
        None => "unknown".into(),
    }
}

/// A source as the trust prompt shows it.
fn source_words(source: &str) -> String {
    match source.split_once(':') {
        Some(("github", repo)) => format!("github.com/{repo}"),
        Some(("path", dir)) => format!("a local build in {dir}"),
        _ => "an unknown source".into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Standing {
    Verified,
    Trusted,
    Untrusted,
}

impl Standing {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Standing::Verified => "verified",
            Standing::Trusted => "trusted",
            Standing::Untrusted => "untrusted",
        }
    }
}

/// The lock covers `m`'s capabilities: its entry is sandboxed and records
/// exactly this digest (a privileged plugin is covered only at the pin).
fn lock_covers(m: &Manifest) -> bool {
    super::lock_entry(&m.id).is_some_and(|l| {
        l.tier == Tier::Sandboxed.as_str()
            && m.caps.tier() == Tier::Sandboxed
            && m.caps.digest() == l.caps
    })
}

/// How a loaded build stands (see the module header).
pub(crate) fn standing(state: &AppState, m: &Manifest) -> Standing {
    // A test build's catalog extras aren't installed copies: the test put
    // them there.
    #[cfg(test)]
    if m.origin.path.is_none() {
        return Standing::Trusted;
    }
    if m.origin.first_party {
        let pinned =
            super::lock_entry(&m.id).is_some_and(|l| l.version == m.version && m.origin.verified);
        if pinned || lock_covers(m) {
            return Standing::Verified;
        }
    }
    if state
        .plugin_guard
        .trusts(&m.id, &source_of(m), &m.caps.digest())
    {
        return Standing::Trusted;
    }
    Standing::Untrusted
}

/// The policy in force (the host setting read from the settings cache: no
/// disk read on the reactor).
pub(crate) fn policy(state: &AppState) -> Policy {
    let setting = crate::lock(&state.settings)
        .map_cached()
        .get("plugins.allowUnverified")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let file = super::read(&state.plugin_guard.policy_file).clone();
    let closed = file.error.is_some();
    Policy {
        allow_unverified: setting && !closed && file.plugins.allow_unverified.unwrap_or(true),
        allow_privileged: if closed {
            Privileged::None
        } else {
            file.plugins.allow_privileged.unwrap_or(Privileged::All)
        },
        blocked: file.plugins.blocked.iter().cloned().collect(),
        managed: file.present,
        error: file.error,
    }
}

/// The block on `m` in force: the kill switch's (a soft one the user
/// allowed doesn't count) or the host policy's list.
pub(crate) fn block(state: &AppState, m: &Manifest) -> Option<Block> {
    let listed = super::read(&state.plugin_guard.revoked).block(&m.id, &m.version, &m.wasm.sha256);
    if let Some(b) = listed {
        if b.level == Level::Hard || !state.plugin_guard.block_allowed(&m.id, &m.wasm.sha256) {
            return Some(b);
        }
    }
    policy(state).blocked.contains(&m.id).then(|| Block {
        level: Level::Hard,
        reason: "this host's policy blocks it".into(),
    })
}

/// Why a loaded plugin may not run here.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Hold {
    Blocked(Block),
    Policy(String),
    Untrusted,
}

/// What a policy refuses of a build in `standing` at `tier`, in words.
fn refused_by(policy: &Policy, standing: Standing, tier: Tier) -> Option<String> {
    if tier == Tier::Privileged {
        match policy.allow_privileged {
            Privileged::None => {
                return Some("this host allows no plugin that runs programs".into())
            }
            Privileged::Verified if standing != Standing::Verified => {
                return Some("this host allows only verified plugins that run programs".into())
            }
            _ => {}
        }
    }
    (standing != Standing::Verified && !policy.allow_unverified)
        .then(|| "this host only allows verified plugins".into())
}

pub(crate) fn hold(state: &AppState, m: &Manifest) -> Option<Hold> {
    if let Some(b) = block(state, m) {
        return Some(Hold::Blocked(b));
    }
    let standing = standing(state, m);
    if let Some(why) = refused_by(&policy(state), standing, m.caps.tier()) {
        return Some(Hold::Policy(why));
    }
    (standing == Standing::Untrusted).then_some(Hold::Untrusted)
}

/// The card's trust fields: `tier`, `caps` and `can` (from the manifest,
/// `manifest_fields`), plus `standing` and, when the plugin is held, `hold`
/// (`{kind: blocked|policy|untrusted, reason?, level?}`).
pub(crate) fn wire(state: &AppState, m: &Manifest, v: &mut Value) {
    v["standing"] = json!(standing(state, m).as_str());
    // A copy that can't run at all says why once (its `fault`), not also
    // that it waits for trust it couldn't use.
    let held = if m.origin.fault.is_some() {
        None
    } else {
        hold(state, m)
    };
    v["hold"] = match held {
        None => Value::Null,
        Some(Hold::Blocked(b)) => {
            json!({"kind": "blocked", "level": b.level.as_str(), "reason": b.reason})
        }
        Some(Hold::Policy(why)) => json!({"kind": "policy", "reason": why}),
        Some(Hold::Untrusted) => json!({"kind": "untrusted"}),
    };
    if let Some(skipped) = state.plugin_guard.skipped(&m.id) {
        v["skipped_version"] = json!(skipped);
    }
}

/// A build on its way in: an install, an update or a rollback.
pub(crate) struct Incoming<'a> {
    pub(crate) m: &'a Manifest,
    /// Where it comes from (`source_of`'s words).
    pub(crate) source: String,
    pub(crate) wasm_sha256: &'a str,
    /// The release the lock pins, its bytes checked against the lock.
    pub(crate) pinned: bool,
}

/// How an admitted build is covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admitted {
    Verified,
    /// Trusted, and how (`Record::how`): a new record to write, or an
    /// existing one (`None`).
    Trusted(Option<&'static str>),
}

/// Whether `incoming` may be installed here, as `token` (the digest the
/// user was shown and confirmed, if they were asked) allows. See the
/// module header; the refusal carries what to show (`needs_trust`).
pub(crate) fn admit(
    state: &AppState,
    incoming: &Incoming<'_>,
    token: Option<&str>,
) -> Result<Admitted, Refusal> {
    let m = incoming.m;
    if let Some(b) =
        super::read(&state.plugin_guard.revoked).block(&m.id, &m.version, incoming.wasm_sha256)
    {
        return Err(Refusal::invalid(format!(
            "Chimaera blocked {} {}: {}",
            m.name, m.version, b.reason
        ))
        .with(json!({"blocked": revoke::block_json(&b)})));
    }
    let policy = policy(state);
    if policy.blocked.contains(&m.id) {
        return Err(Refusal::new(
            StatusCode::FORBIDDEN,
            format!("this host's policy blocks {}", m.name),
        ));
    }
    let digest = m.caps.digest();
    let first_party_source = super::lock_entry(&m.id)
        .is_some_and(|l| incoming.source == format!("github:{}", l.repo.to_ascii_lowercase()));
    let verified = incoming.pinned || (first_party_source && lock_covers(m));
    let standing = if verified {
        Standing::Verified
    } else {
        Standing::Trusted
    };
    if let Some(why) = refused_by(&policy, standing, m.caps.tier()) {
        return Err(Refusal::new(
            StatusCode::FORBIDDEN,
            format!("{} can't be installed: {why}", m.name),
        )
        .with(json!({"policy": policy.json()})));
    }
    if verified {
        return Ok(Admitted::Verified);
    }
    if state.plugin_guard.trusts(&m.id, &incoming.source, &digest) {
        return Ok(Admitted::Trusted(None));
    }
    // The build it replaces, if that one was covered and came from the same
    // place: asking for no more than it is no new question. Except where
    // only the lock vouched for a privileged build: the lock covers its pin
    // alone, so a later release asks (privileged updates come through the
    // lock, plan §17).
    let running = super::manifest(state, &m.id)
        .filter(|r| source_of(r) == incoming.source && standing_ok(state, r));
    let lock_only = |r: &Manifest| {
        m.caps.tier() == Tier::Privileged
            && crate::plugins::trust::standing(state, r) == Standing::Verified
    };
    if running
        .as_ref()
        .is_some_and(|r| r.caps.covers(&m.caps) && !lock_only(r))
    {
        return Ok(Admitted::Trusted(Some("subset")));
    }
    if token == Some(digest.as_str()) {
        return Ok(Admitted::Trusted(Some("prompt")));
    }
    Err(needs_trust(
        m,
        &incoming.source,
        incoming.wasm_sha256,
        running.as_deref(),
        token.is_some(),
    ))
}

/// A covered build that isn't blocked (its capabilities can vouch for a
/// smaller update).
fn standing_ok(state: &AppState, m: &Manifest) -> bool {
    standing(state, m) != Standing::Untrusted && block(state, m).is_none()
}

/// The 409 a trust prompt answers: what `m` from `source` can do, what it
/// asks for beyond `running` (the build it would replace), and the digest
/// to confirm with.
pub(crate) fn needs_trust(
    m: &Manifest,
    source: &str,
    wasm_sha256: &str,
    running: Option<&Manifest>,
    stale_token: bool,
) -> Refusal {
    let grown = running.map(|r| m.caps.beyond(&r.caps));
    let message = if stale_token {
        format!(
            "what {} {} can do changed since you looked — review it again",
            m.name, m.version
        )
    } else if let (Some(r), Some(g)) = (running, grown.as_ref()) {
        if g.is_empty() {
            format!("{} {} needs your trust to run", m.name, m.version)
        } else {
            format!(
                "{} {} would do more than {}: allow it to update",
                m.name, m.version, r.version
            )
        }
    } else {
        format!(
            "{} {} isn't verified by the Chimaera maintainers: trust what it can do to install it",
            m.name, m.version
        )
    };
    let tier = m.caps.tier();
    Refusal::conflict(message).with(json!({"trust": {
        "id": m.id,
        "name": m.name,
        "version": m.version,
        "source": source_words(source),
        "tier": tier.as_str(),
        "caps": m.caps.digest(),
        "can": m.caps.lines_json(),
        "grown": grown.map(|g| g.lines_json()),
        "from_version": running.map(|r| r.version.clone()),
        "sha256": wasm_sha256,
        // A privileged plugin is confirmed by typing its name.
        "confirm": (tier == Tier::Privileged).then(|| m.name.clone()),
    }}))
}

/// After an admitted install or update: the trust record it earned, and the
/// activity line. The skip of a version the user has now taken is dropped.
pub(crate) async fn after_admit(
    state: &AppState,
    m: &Manifest,
    source: &str,
    admitted: Admitted,
) -> Result<(), Refusal> {
    if let Admitted::Trusted(Some(how)) = admitted {
        insert(state, now_record(m, source.to_string(), how));
        crate::lock(&state.plugin_guard.store)
            .file
            .skipped
            .remove(&m.id);
        persist(state).await?;
        super::activity::record(
            state,
            &m.id,
            json!({"kind": "trust", "version": m.version, "caps": m.caps.digest(), "how": how}),
        )
        .await;
    }
    Ok(())
}

#[derive(Deserialize)]
pub(crate) struct TrustBody {
    /// The capability digest the user was shown and confirmed (`trust`,
    /// as the install and update routes name it, is read the same).
    #[serde(default, alias = "trust")]
    caps: Option<String>,
    /// Switch a soft-blocked build back on (its sha256 is the one running).
    #[serde(default)]
    allow_block: bool,
}

/// POST /plugins/{pid}/trust {caps} — trust what the installed build can
/// do (its card's Trust), or {allow_block: true} — run a soft-blocked build
/// anyway. Either takes effect at once; windows are told.
pub(crate) async fn trust_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
    Json(body): Json<TrustBody>,
) -> Response {
    let Some(m) = super::manifest(&state, &pid) else {
        return super::not_installed(&pid).into_response();
    };
    if body.allow_block {
        let listed =
            super::read(&state.plugin_guard.revoked).block(&m.id, &m.version, &m.wasm.sha256);
        match listed {
            Some(b) if b.level == Level::Soft => {
                {
                    // One allowance per build: asking again only renews it.
                    let mut store = crate::lock(&state.plugin_guard.store);
                    let allowed = &mut store.file.allowed_blocks;
                    allowed.retain(|a| !(a.id == m.id && a.sha256 == *m.wasm.sha256));
                    allowed.push(AllowedBlock {
                        id: m.id.clone(),
                        sha256: m.wasm.sha256.to_string(),
                        granted_ms: crate::timeline::now_ms(),
                    });
                }
                if let Err(r) = persist(&state).await {
                    return r.into_response();
                }
                super::activity::record(
                    &state,
                    &m.id,
                    json!({"kind": "allow-block", "version": m.version, "reason": b.reason}),
                )
                .await;
            }
            Some(_) => {
                return Refusal::conflict(format!(
                    "Chimaera blocked {} {} for good: update or remove it",
                    m.name, m.version
                ))
                .into_response()
            }
            None => return Refusal::conflict(format!("{} isn't blocked", m.name)).into_response(),
        }
    } else {
        let digest = m.caps.digest();
        if body.caps.as_deref() != Some(digest.as_str()) {
            let source = source_of(&m);
            return needs_trust(&m, &source, &m.wasm.sha256, Some(&m), body.caps.is_some())
                .into_response();
        }
        let refused = refused_by(&policy(&state), Standing::Trusted, m.caps.tier());
        if let Some(why) = refused {
            return Refusal::new(
                StatusCode::FORBIDDEN,
                format!("{} can't run here: {why}", m.name),
            )
            .into_response();
        }
        insert(&state, now_record(&m, source_of(&m), "prompt"));
        if let Err(r) = persist(&state).await {
            return r.into_response();
        }
        super::activity::record(
            &state,
            &m.id,
            json!({"kind": "trust", "version": m.version, "caps": digest, "how": "prompt"}),
        )
        .await;
    }
    state.plugin_runtime.forget_plugin(&m.id);
    state.changes.notify_waiters();
    Json(json!({"id": pid, "plugin": super::entry_json(&state, &pid)})).into_response()
}

/// DELETE /plugins/{pid}/trust — withdraw every trust record for `pid`
/// (and any soft block allowed): a plugin the lock doesn't cover goes off
/// in every workspace at once, its instances dropped.
pub(crate) async fn untrust_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    if !super::valid_id(&pid) {
        return super::not_found("unknown plugin");
    }
    {
        let mut store = crate::lock(&state.plugin_guard.store);
        store.file.records.retain(|r| r.id != pid);
        store.file.allowed_blocks.retain(|a| a.id != pid);
    }
    if let Err(r) = persist(&state).await {
        return r.into_response();
    }
    state.plugin_runtime.forget_plugin(&pid);
    // Its programs stop with its trust, wherever they run, and what it
    // published goes.
    super::jobs::cancel_where(&state, &pid, None);
    crate::lock(&state.plugin_platform.surfaces).forget_plugin(&pid);
    crate::lock(&state.knowledge).forget_provider(&pid, None);
    super::activity::record(&state, &pid, json!({"kind": "untrust"})).await;
    state.changes.notify_waiters();
    Json(json!({"id": pid, "plugin": super::entry_json(&state, &pid)})).into_response()
}

#[derive(Deserialize)]
pub(crate) struct SkipBody {
    version: String,
}

/// POST /plugins/{pid}/skip {version} — Skip this version: an update that
/// asks for more is not offered again at that version (the build that
/// runs keeps running).
pub(crate) async fn skip_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
    Json(body): Json<SkipBody>,
) -> Response {
    let Some(m) = super::manifest(&state, &pid) else {
        return super::not_installed(&pid).into_response();
    };
    if super::plugin_version(&body.version).is_err() {
        return super::bad_request("version is MAJOR.MINOR.PATCH");
    }
    crate::lock(&state.plugin_guard.store)
        .file
        .skipped
        .insert(m.id.clone(), body.version.clone());
    if let Err(r) = persist(&state).await {
        return r.into_response();
    }
    super::activity::record(
        &state,
        &m.id,
        json!({"kind": "skip", "version": body.version}),
    )
    .await;
    state.changes.notify_waiters();
    Json(json!({"id": pid, "plugin": super::entry_json(&state, &pid)})).into_response()
}

/// A Remove: the user's answers about `id` go with it (a later install of
/// the id is a new question). A failed write is logged; the records in
/// memory are gone either way.
pub(crate) async fn forget(state: &AppState, id: &str) {
    {
        let mut store = crate::lock(&state.plugin_guard.store);
        store.file.records.retain(|r| r.id != id);
        store.file.allowed_blocks.retain(|a| a.id != id);
        store.file.skipped.remove(id);
    }
    if let Err(r) = persist(state).await {
        tracing::warn!(plugin = id, error = %r.message, "trust records not updated after a remove");
    }
}

/// Tests only: the records in force.
#[cfg(test)]
pub(crate) fn records_for_tests(state: &AppState) -> Vec<Record> {
    crate::lock(&state.plugin_guard.store).file.records.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_policy_file_tightens_and_fails_closed() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-policy-{}-{}",
            std::process::id(),
            crate::timeline::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("policy.json");
        assert!(!load_policy_file(&path).present, "none: nothing tightened");
        std::fs::write(
            &path,
            r#"{"plugins": {"allowUnverified": false, "allowPrivileged": "verified", "blocked": ["x"]}}"#,
        )
        .unwrap();
        let p = load_policy_file(&path);
        assert!(p.present && p.error.is_none());
        assert_eq!(p.plugins.allow_unverified, Some(false));
        assert_eq!(p.plugins.allow_privileged, Some(Privileged::Verified));
        std::fs::write(&path, r#"{"plugins": {"allowUnverified": fals"#).unwrap();
        assert!(load_policy_file(&path).error.is_some(), "a syntax error");
        std::fs::write(&path, r#"{"plugins": {"allowEverything": true}}"#).unwrap();
        assert!(load_policy_file(&path).error.is_some(), "an unknown key");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn what_a_policy_refuses() {
        let open = Policy {
            allow_unverified: true,
            allow_privileged: Privileged::All,
            blocked: BTreeSet::new(),
            managed: false,
            error: None,
        };
        assert_eq!(refused_by(&open, Standing::Trusted, Tier::Privileged), None);
        let verified_only = Policy {
            allow_unverified: false,
            ..open.clone()
        };
        assert!(refused_by(&verified_only, Standing::Trusted, Tier::Sandboxed).is_some());
        assert_eq!(
            refused_by(&verified_only, Standing::Verified, Tier::Sandboxed),
            None
        );
        let no_programs = Policy {
            allow_privileged: Privileged::None,
            ..open.clone()
        };
        assert!(refused_by(&no_programs, Standing::Verified, Tier::Privileged).is_some());
        let verified_programs = Policy {
            allow_privileged: Privileged::Verified,
            ..open
        };
        assert!(refused_by(&verified_programs, Standing::Trusted, Tier::Privileged).is_some());
        assert_eq!(
            refused_by(&verified_programs, Standing::Verified, Tier::Privileged),
            None
        );
    }

    #[test]
    fn sources_read_as_the_prompt_says_them() {
        assert_eq!(source_words("github:acme/x"), "github.com/acme/x");
        assert_eq!(
            source_words("path:/home/me/x"),
            "a local build in /home/me/x"
        );
    }
}
