//! The kill switch: a list of plugin builds chimaera blocks, everywhere.
//! Design: docs/plugin-platform-plan.md §2 ("The kill switch").
//!
//! - **Two copies of one file.** `plugins/revoked.json` is embedded in every
//!   build (a host that never reaches the network is covered by its next
//!   chimaera release); the same file on the repository's main branch is the
//!   live list, fetched with the plugin release checker (once after boot,
//!   then daily, under `update.autoCheck`) and kept on disk as the last good
//!   copy (`<data dir>/plugins/revoked.json`).
//! - **Signed.** A fetched list counts only with valid Ed25519 signatures
//!   (`revoked.sig` beside it) from at least `threshold` of the keys this
//!   binary embeds (`plugins/revocation-keys.txt`), so a compromised GitHub
//!   account alone can neither block nor unblock a plugin. With no key
//!   embedded, no fetched list counts (none is fetched): the embedded
//!   snapshot still does.
//! - **Never older.** Each list carries a `serial`, signed with it; a list
//!   older than the one in force is refused, so an old signed list can't be
//!   served again to lift a later block. The kept copy's signature is kept
//!   beside it and checked again at boot.
//! - **Entries** name an id and the versions or `plugin.wasm` sha256s they
//!   cover (neither: every version), a level and a reason. **Hard**: the
//!   build never loads, no override. **Soft**: switched off, and the user
//!   may switch it back on after reading why (`trust::allow_blocked`).
//! - **It acts at once**: a list that newly blocks a loaded build drops that
//!   plugin's instances (`apply`), not at the next restart.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Manifest;
use crate::AppState;

/// The snapshot this build carries.
const EMBEDDED: &str = include_str!("../../../../plugins/revoked.json");
/// The keys whose signatures a fetched list needs, and how many.
const KEYS: &str = include_str!("../../../../plugins/revocation-keys.txt");
/// A list's size cap, fetched or kept.
const LIST_MAX: usize = 256 << 10;
const ENTRIES_MAX: usize = 1024;
const REASON_MAX: usize = 300;
/// The last good fetched list, beside the installed plugins, and its
/// signatures.
const CACHE: &str = "revoked.json";
const CACHE_SIG: &str = "revoked.sig";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Level {
    Hard,
    Soft,
}

impl Level {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Level::Hard => "hard",
            Level::Soft => "soft",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) versions: Vec<String>,
    /// `plugin.wasm` sha256s, lowercase hex.
    #[serde(default)]
    pub(crate) sha256: Vec<String>,
    pub(crate) level: Level,
    pub(crate) reason: String,
}

impl Entry {
    fn covers(&self, id: &str, version: &str, sha256: &str) -> bool {
        self.id == id
            && ((self.versions.is_empty() && self.sha256.is_empty())
                || self.versions.iter().any(|v| v == version)
                || self.sha256.iter().any(|s| s == sha256))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct List {
    pub(crate) schema: u32,
    /// Raised with every published change; a host never takes a lower one.
    #[serde(default)]
    pub(crate) serial: u64,
    #[serde(default)]
    pub(crate) entries: Vec<Entry>,
}

/// Parse and check a list: its schema, each entry's id, and the caps.
pub(crate) fn parse(text: &str) -> Result<List, String> {
    if text.len() > LIST_MAX {
        return Err(format!("over its {} KiB cap", LIST_MAX >> 10));
    }
    let list: List = serde_json::from_str(text).map_err(|e| format!("the revocation list: {e}"))?;
    if list.schema != 1 {
        return Err(format!("unknown revocation list schema {}", list.schema));
    }
    if list.entries.len() > ENTRIES_MAX {
        return Err(format!("more than {ENTRIES_MAX} entries"));
    }
    for e in &list.entries {
        if !super::valid_id(&e.id) {
            return Err(format!("{:?} is not a plugin id", e.id));
        }
        if e.reason.trim().is_empty() || e.reason.len() > REASON_MAX {
            return Err(format!(
                "{}: a reason is one short sentence (≤ {REASON_MAX} bytes)",
                e.id
            ));
        }
        if e.sha256
            .iter()
            .any(|s| s.len() != 64 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        {
            return Err(format!("{}: a sha256 is not 64 lowercase hex digits", e.id));
        }
    }
    Ok(list)
}

/// The embedded keys and the number of distinct signatures a list needs:
/// `threshold <n>` and one hex public key per line; `#` comments.
fn keys() -> (Vec<ed25519_dalek::VerifyingKey>, usize) {
    let mut keys = Vec::new();
    let mut threshold = 1;
    for line in KEYS.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(n) = line.strip_prefix("threshold") {
            threshold = n.trim().parse().unwrap_or(usize::MAX).max(1);
            continue;
        }
        match hex32(line).and_then(|b| ed25519_dalek::VerifyingKey::from_bytes(&b).ok()) {
            Some(key) => keys.push(key),
            None => tracing::error!(
                line,
                "plugins/revocation-keys.txt: not an Ed25519 public key"
            ),
        }
    }
    (keys, threshold)
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    hex_bytes(s)?.try_into().ok()
}

/// Whether `sig` (one hex Ed25519 signature per line) carries valid
/// signatures of `list` from at least `threshold` distinct `keys`.
pub(crate) fn signed(
    list: &[u8],
    sig: &str,
    keys: &[ed25519_dalek::VerifyingKey],
    threshold: usize,
) -> bool {
    let mut signers = BTreeSet::new();
    for line in sig
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(16)
    {
        let Some(bytes) = hex_bytes(line).and_then(|b| <[u8; 64]>::try_from(b).ok()) else {
            continue;
        };
        let signature = ed25519_dalek::Signature::from_bytes(&bytes);
        for (i, key) in keys.iter().enumerate() {
            if key.verify_strict(list, &signature).is_ok() {
                signers.insert(i);
            }
        }
    }
    !keys.is_empty() && signers.len() >= threshold
}

/// A block on one build, as the card and the gate say it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Block {
    pub(crate) level: Level,
    pub(crate) reason: String,
}

/// The lists in force: the embedded snapshot and the last good fetched one.
#[derive(Debug, Default)]
pub(crate) struct Revocations {
    embedded: Vec<Entry>,
    fetched: Vec<Entry>,
    /// The newest serial in force (the embedded list's or the kept one's).
    serial: u64,
    /// When the live list was last fetched and accepted.
    pub(crate) fetched_ms: Option<u64>,
    /// Why the last fetch wasn't accepted (unreachable, unsigned).
    pub(crate) error: Option<String>,
}

impl Revocations {
    /// The embedded list and the kept copy under `root`, if its signatures
    /// still verify (blocking; boot).
    pub(crate) fn load(root: &Path) -> Self {
        let embedded = match parse(EMBEDDED) {
            Ok(list) => list,
            Err(err) => {
                tracing::error!(%err, "the embedded plugins/revoked.json is invalid");
                List {
                    schema: 1,
                    serial: 0,
                    entries: Vec::new(),
                }
            }
        };
        let (keys, threshold) = keys();
        let kept = std::fs::read_to_string(root.join(CACHE))
            .ok()
            .filter(|text| text.len() <= LIST_MAX)
            .filter(|text| {
                let sig = std::fs::read_to_string(root.join(CACHE_SIG)).unwrap_or_default();
                signed(text.as_bytes(), &sig, &keys, threshold)
            })
            .and_then(|text| parse(&text).ok())
            .filter(|l| l.serial >= embedded.serial);
        let serial = kept.as_ref().map_or(embedded.serial, |l| l.serial);
        Revocations {
            embedded: embedded.entries,
            fetched: kept.map(|l| l.entries).unwrap_or_default(),
            serial,
            fetched_ms: None,
            error: None,
        }
    }

    /// The block on `id` at `version` whose component hashes to `sha256`,
    /// if any: a hard entry wins over a soft one.
    pub(crate) fn block(&self, id: &str, version: &str, sha256: &str) -> Option<Block> {
        let mut found: Option<&Entry> = None;
        for e in self.embedded.iter().chain(&self.fetched) {
            if e.covers(id, version, sha256)
                && found.is_none_or(|f| f.level == Level::Soft && e.level == Level::Hard)
            {
                found = Some(e);
            }
        }
        found.map(|e| Block {
            level: e.level,
            reason: e.reason.clone(),
        })
    }

    /// Tests only: take `entries` as the fetched list.
    #[cfg(test)]
    pub(crate) fn set_fetched_for_tests(&mut self, entries: Vec<Entry>) {
        self.fetched = entries;
    }
}

/// Where the live list is published: the repository's main branch.
fn list_url() -> Option<String> {
    if let Ok(url) = std::env::var("CHIMAERA_PLUGIN_REVOCATIONS") {
        return Some(url);
    }
    let slug = chimaera_core::REPOSITORY.strip_prefix("https://github.com/")?;
    Some(format!(
        "https://raw.githubusercontent.com/{slug}/main/plugins/revoked.json"
    ))
}

/// Fetch the live list and its signatures, verify, keep, apply. A failure
/// keeps the list in force (and says why in `error`).
pub(crate) async fn refresh(state: &Arc<AppState>) {
    let Some(url) = list_url() else {
        return;
    };
    let (keys, threshold) = keys();
    if keys.is_empty() {
        super::write(&state.plugin_guard.revoked).error = Some(
            "this build embeds no revocation key, so only its own snapshot counts".to_string(),
        );
        return;
    }
    let result = async {
        let list = crate::agent_updates::curl(&url, &[])
            .await
            .map_err(|e| format!("could not fetch {url}: {e:#}"))?;
        let sig_url = match url.strip_suffix(".json") {
            Some(base) => format!("{base}.sig"),
            None => format!("{url}.sig"),
        };
        let sig = crate::agent_updates::curl(&sig_url, &[])
            .await
            .map_err(|e| format!("could not fetch its signatures: {e:#}"))?;
        let sig = String::from_utf8_lossy(&sig).into_owned();
        if !signed(&list, &sig, &keys, threshold) {
            return Err(
                "its signatures don't verify against the keys this chimaera trusts".to_string(),
            );
        }
        let text = String::from_utf8(list).map_err(|_| "the list is not UTF-8".to_string())?;
        let parsed = parse(&text)?;
        let in_force = super::read(&state.plugin_guard.revoked).serial;
        if parsed.serial < in_force {
            return Err(format!(
                "it is older than the list in force (serial {} < {in_force})",
                parsed.serial
            ));
        }
        Ok::<_, String>((text, sig, parsed))
    }
    .await;
    match result {
        Ok((text, sig, parsed)) => {
            let root = state.plugin_catalog.root.clone();
            let written = tokio::task::spawn_blocking(move || {
                // The signatures first: a list kept without them doesn't count.
                crate::persist::atomic_write_json(&root.join(CACHE_SIG), sig.as_bytes())?;
                crate::persist::atomic_write_json(&root.join(CACHE), text.as_bytes())
            })
            .await;
            if !matches!(written, Ok(Ok(()))) {
                tracing::warn!("the fetched plugin revocation list could not be kept on disk");
            }
            {
                let mut r = super::write(&state.plugin_guard.revoked);
                r.serial = parsed.serial;
                r.fetched = parsed.entries;
                r.fetched_ms = Some(crate::timeline::now_ms());
                r.error = None;
            }
            apply(state).await;
        }
        Err(err) => {
            tracing::debug!(%err, "plugin revocation list not refreshed");
            super::write(&state.plugin_guard.revoked).error = Some(err);
        }
    }
}

/// A list or the host policy changed: every loaded build it now holds
/// (blocked, or refused by policy) stops at once (its instances and offers
/// dropped, its jobs cancelled, a knowledge snapshot forgotten), and windows
/// are told.
pub(crate) async fn apply(state: &Arc<AppState>) {
    use super::trust::Hold;
    let held: Vec<(Arc<Manifest>, bool)> = super::catalog(state)
        .iter()
        .filter_map(|m| match super::trust::hold(state, m) {
            Some(Hold::Blocked(_)) => Some((m.clone(), true)),
            Some(Hold::Policy(_)) => Some((m.clone(), false)),
            // Withdrawing trust stops a plugin itself.
            Some(Hold::Untrusted) | None => None,
        })
        .collect();
    for (m, blocked) in &held {
        state.plugin_runtime.forget_plugin(&m.id);
        // Its programs stop too, wherever they run.
        state.plugin_platform.jobs.cancel_where(&m.id, None);
        crate::lock(&state.knowledge).forget_provider(&m.id, None);
        if *blocked {
            super::activity::record(
                state,
                &m.id,
                json!({"kind": "blocked", "version": m.version, "sha256": &*m.wasm.sha256}),
            )
            .await;
        }
    }
    state.changes.notify_waiters();
}

/// A block on the wire: `{level, reason}`.
pub(crate) fn block_json(b: &Block) -> Value {
    json!({"level": b.level.as_str(), "reason": b.reason})
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    fn key(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn the_embedded_files_parse() {
        parse(EMBEDDED).expect("plugins/revoked.json parses");
        let (keys, threshold) = keys();
        assert!(threshold >= 1);
        assert!(keys.len() < 64, "a handful of maintainers' keys");
    }

    #[test]
    fn a_list_counts_only_with_enough_signatures_from_embedded_keys() {
        let list = br#"{"schema":1,"entries":[]}"#;
        let (a, b, stranger) = (key(1), key(2), key(3));
        let keys = [a.verifying_key(), b.verifying_key()];
        let sig = |k: &ed25519_dalek::SigningKey| hex(&k.sign(list).to_bytes());
        assert!(signed(list, &sig(&a), &keys, 1));
        assert!(!signed(list, &sig(&a), &keys, 2), "one of two");
        assert!(signed(
            list,
            &format!("{}\n{}\n", sig(&a), sig(&b)),
            &keys,
            2
        ));
        assert!(
            !signed(list, &format!("{}\n{}\n", sig(&a), sig(&a)), &keys, 2),
            "the same key twice is one signer"
        );
        assert!(!signed(list, &sig(&stranger), &keys, 1));
        assert!(
            !signed(b"{\"schema\":1}", &sig(&a), &keys, 1),
            "another list"
        );
        assert!(!signed(list, &sig(&a), &[], 1), "no keys: nothing verifies");
        assert!(!signed(list, "zz", &keys, 1));
    }

    /// A signature made by `scripts/revocations.mjs` (Node's crypto) with a
    /// throwaway key verifies here: the two sides agree on the format.
    #[test]
    fn a_signature_from_the_signing_script_verifies() {
        let key = ed25519_dalek::VerifyingKey::from_bytes(
            &hex32("d9729f639429f9fa5cf7c548a03b9d3a56741193eaee0d5411c3d69c7d41f607").unwrap(),
        )
        .unwrap();
        let list = br#"{"schema":1,"entries":[]}"#;
        let sig = "29b240fa66b1696499c779bed398f61c59e42a0e96a59380c68a6c34939357e4a3c9d81a70053cb0b9398767203ebed984682625078fc55cbbc1f459af0e870d";
        assert!(signed(list, sig, &[key], 1));
        assert!(!signed(br#"{"schema":1,"entries":[ ]}"#, sig, &[key], 1));
    }

    #[test]
    fn entries_cover_versions_hashes_or_everything_and_hard_wins() {
        let list = parse(&format!(
            r#"{{"schema":1,"entries":[
                {{"id":"x","versions":["0.1.0"],"level":"soft","reason":"broken"}},
                {{"id":"x","sha256":["{}"],"level":"hard","reason":"steals keys"}},
                {{"id":"y","level":"hard","reason":"every version"}}
            ]}}"#,
            "a".repeat(64)
        ))
        .unwrap();
        let r = Revocations {
            embedded: list.entries,
            ..Default::default()
        };
        let sha = "a".repeat(64);
        assert_eq!(r.block("x", "0.1.0", "b").unwrap().level, Level::Soft);
        assert_eq!(r.block("x", "0.1.0", &sha).unwrap().level, Level::Hard);
        assert_eq!(r.block("x", "0.2.0", "b"), None);
        assert!(r.block("y", "9.9.9", "c").is_some());
        assert_eq!(r.block("z", "0.1.0", "b"), None);
    }

    #[test]
    fn a_kept_list_counts_only_with_its_signatures() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-revoke-{}-{}",
            std::process::id(),
            &chimaera_core::generate_token()[..8]
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(CACHE),
            r#"{"schema":1,"serial":9,"entries":[{"id":"x","level":"hard","reason":"r"}]}"#,
        )
        .unwrap();
        std::fs::write(root.join(CACHE_SIG), "00").unwrap();
        let r = Revocations::load(&root);
        assert_eq!(r.block("x", "0.1.0", "a"), None, "unsigned: not in force");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_list_carries_its_serial() {
        let list = parse(r#"{"schema":1,"serial":3,"entries":[]}"#).unwrap();
        assert_eq!(list.serial, 3);
        assert_eq!(parse(r#"{"schema":1,"entries":[]}"#).unwrap().serial, 0);
        assert!(
            parse(EMBEDDED).unwrap().serial >= 1,
            "the embedded list has one"
        );
    }

    #[test]
    fn a_bad_list_is_refused() {
        assert!(parse(r#"{"schema":2,"entries":[]}"#).is_err());
        assert!(
            parse(r#"{"schema":1,"entries":[{"id":"X","level":"hard","reason":"r"}]}"#).is_err()
        );
        assert!(
            parse(r#"{"schema":1,"entries":[{"id":"x","level":"hard","reason":" "}]}"#).is_err()
        );
        assert!(
            parse(r#"{"schema":1,"entries":[{"id":"x","level":"odd","reason":"r"}]}"#).is_err()
        );
        assert!(parse(r#"{"schema":1,"entries":[],"extra":1}"#).is_err());
    }
}
