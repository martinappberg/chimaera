//! Installed plugins: `<data dir>/plugins/<id>/<version>/{plugin.toml,plugin.wasm,SHA256SUMS}`,
//! a `current` link naming the version that loads and a `previous` link
//! naming the one Use previous goes back to. Written only by a visible
//! install or update (the user's click, or `chimaera plugin add|update`).
//! Design: docs/plugin-system-plan.md ("Versions and updates").
//!
//! - **Three ways in, one layout.** A first-party plugin installs the
//!   release the lock pins (`POST /plugins/{pid}/install`), fetched by
//!   direct download and checked against the release's `SHA256SUMS` AND the
//!   lock; any other plugin installs from its GitHub release (the latest, or
//!   a tag) against its `SHA256SUMS`; a local build is copied from a
//!   directory (`{path}`), checked against the `SHA256SUMS` there when it has
//!   one, and marked with a `local-path` file. The release's `SHA256SUMS` is
//!   kept beside the two files.
//! - **Releases** (`install`): `SHA256SUMS` and `plugin.toml` come first
//!   (small, into memory), and the manifest is checked (its id, the tag's
//!   version, the gates) before the component is fetched; `plugin.wasm`
//!   streams to a temp dir under the plugin's directory (`WASM_MAX`) and is
//!   verified; only then is the temp dir renamed to `<version>/` and
//!   `current` swapped. Any failure leaves the old version current and
//!   removes the temp dir.
//! - **Every read re-checks** (`load_version`, from the catalog's reload):
//!   both files are hashed against the `SHA256SUMS` beside them — a match is
//!   `verified`, none (a copy installed before it was kept, or a local build
//!   without one) is not, a mismatch is a fault and the copy never loads
//!   until the same version is installed again (the card's Reinstall).
//! - **Links move atomically**: a symlink under a fresh name, then rename(2)
//!   over the old one — `runtimes.rs`'s idiom for the managed agent CLIs.
//!   An update never touches the version in use; the version it replaced
//!   becomes `previous`, and older ones are removed (two versions on disk at
//!   most). Use previous swaps `current` and `previous`, so it is reversible.
//! - **Every change reloads the catalog** and drops the plugin's instances
//!   (a running session sees the new tools on its next `tools/list`), and
//!   logs one audit line. One change at a time daemon-wide
//!   (`Releases::changing`); all filesystem work on the blocking pool.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{Path as AxPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use super::releases::{self, Release};
use super::{Locked, Manifest, Refusal, Wasm};
use crate::AppState;

/// A component's size cap, on download and on load.
pub(crate) const WASM_MAX: u64 = 16 << 20;
/// A manifest's size cap on load and on preview (downloads ride curl's
/// 1 MiB fence).
pub(crate) const TOML_MAX: u64 = 64 << 10;
/// The kept `SHA256SUMS`: whatever the release published, which the
/// download's own 1 MiB fence already bounded.
const SUMS_MAX: u64 = 1 << 20;
const SUMS: &str = "SHA256SUMS";
/// A local install's marker: the directory it was copied from.
const LOCAL_PATH: &str = "local-path";
/// Written by release installs only, never copied from a local build.
const SOURCE_GITHUB: &str = "source-github";
/// Why a copy whose files don't match its checksums never loads — the
/// card's words (the log line says which file and which list).
pub(crate) const SUMS_MISMATCH: &str =
    "the downloaded files don't match what the release published — reinstall it";
/// A download that doesn't hash to what its release lists (the refusal's
/// words; the log line carries both hashes).
pub(crate) const DOWNLOAD_MISMATCH: &str =
    "the downloaded files don't match what the release published — try again";
/// A component download's wall clock: 16 MiB through a slow site proxy.
const DOWNLOAD_SECS: u64 = 120;
const CURRENT: &str = "current";
const PREVIOUS: &str = "previous";
/// Staged downloads and set-aside trees. Dot-names: `scan` never reads them.
const TEMP_PREFIX: &str = ".tmp-";

/// One plugin's installed copy, as found on disk: the `current` version's
/// manifest (with its component), where it lives, and what `previous`
/// names.
#[derive(Clone, Debug)]
pub(crate) struct InstalledCopy {
    pub(crate) manifest: Manifest,
    /// `<root>/<id>/<version>`.
    pub(crate) dir: PathBuf,
    pub(crate) previous: Option<String>,
}

/// Every installed copy under `root`, sorted by id (blocking). A copy that
/// doesn't hold together (an unreadable manifest, an id or version that
/// isn't its directory's, a component over the cap) is left out, loudly.
pub(crate) fn scan(root: &Path) -> Vec<InstalledCopy> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(root = %root.display(), %err, "installed plugins unreadable");
            return Vec::new();
        }
    };
    let mut copies: Vec<InstalledCopy> = entries
        .flatten()
        .filter_map(|entry| {
            let id = entry.file_name().to_str()?.to_string();
            if !super::valid_id(&id) {
                return None;
            }
            match load_current(root, &id) {
                Ok(copy) => copy,
                Err(err) => {
                    tracing::warn!(plugin = %id, %err, "installed plugin skipped");
                    None
                }
            }
        })
        .collect();
    copies.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    copies
}

/// `<root>/<id>`'s `current` copy; `None` when nothing is current (a
/// directory with no link, e.g. mid-remove).
fn load_current(root: &Path, id: &str) -> Result<Option<InstalledCopy>, String> {
    let dir = root.join(id);
    let Some(version) = link(&dir, CURRENT)? else {
        return Ok(None);
    };
    let manifest = load_version(&dir, id, &version)?;
    let previous = link(&dir, PREVIOUS)
        .ok()
        .flatten()
        .filter(|p| *p != version && dir.join(p).join("plugin.toml").is_file());
    Ok(Some(InstalledCopy {
        manifest,
        dir: dir.join(&version),
        previous,
    }))
}

/// Read `<dir>/<version>/`: its manifest must be `id`'s at `version`, and
/// its component within the cap; then what the catalog knows about the
/// bytes (`check_files`) and whether it was a local install (blocking).
pub(crate) fn load_version(dir: &Path, id: &str, version: &str) -> Result<Manifest, String> {
    let vdir = dir.join(version);
    let toml = read_capped(&vdir.join("plugin.toml"), TOML_MAX)?;
    let text = std::str::from_utf8(&toml).map_err(|_| "plugin.toml is not UTF-8".to_string())?;
    let mut m = super::parse_manifest(text)?;
    if m.id != id {
        return Err(format!("its plugin.toml is for {:?}", m.id));
    }
    if m.version != version {
        return Err(format!(
            "its plugin.toml says version {}, its directory {version}",
            m.version
        ));
    }
    let wasm = read_capped(&vdir.join("plugin.wasm"), WASM_MAX)?;
    m.wasm = Wasm::new(Cow::Owned(wasm));
    let sums = read_optional(&vdir.join(SUMS), SUMS_MAX)?;
    let source = read_optional(&vdir.join(SOURCE_GITHUB), 512)?;
    let source = source.as_deref().and_then(|s| std::str::from_utf8(s).ok());
    check_files(
        &mut m,
        &crate::fs::sha256_hex(&toml),
        sums.as_deref(),
        source,
    );
    m.origin.local_path = read_optional(&vdir.join(LOCAL_PATH), 4 << 10)?
        .map(|bytes| PathBuf::from(String::from_utf8_lossy(&bytes).trim()))
        .filter(|p| p.is_absolute());
    Ok(m)
}

/// Decide `first_party`, `verified` and the checksum fault for a copy whose
/// component `m.wasm` holds and whose `plugin.toml` hashes to `toml_sha256`,
/// against the `SHA256SUMS` kept beside them (`None`: there is none).
fn check_files(m: &mut Manifest, toml_sha256: &str, sums: Option<&[u8]>, source: Option<&str>) {
    let lock = super::lock_entry(&m.id);
    let first_party = lock.is_some_and(|l| {
        m.release
            .as_ref()
            .is_some_and(|r| r.github.eq_ignore_ascii_case(&l.repo))
            && (source.is_some_and(|s| s.eq_ignore_ascii_case(&l.repo))
                || (m.version == l.version
                    && *m.wasm.sha256 == l.sha256_wasm
                    && toml_sha256 == l.sha256_toml))
    });
    let mut verified = false;
    if let Some(sums) = sums {
        let sums = releases::parse_sums(&String::from_utf8_lossy(sums));
        let listed = |name: &str| sums.get(name).map(String::as_str);
        if listed("plugin.wasm") == Some(&*m.wasm.sha256)
            && listed("plugin.toml") == Some(toml_sha256)
        {
            verified = true;
        } else {
            tracing::error!(
                plugin = %m.id,
                version = %m.version,
                "installed plugin refused: its files do not match the SHA256SUMS kept beside them"
            );
            m.origin.fault = Some(SUMS_MISMATCH.to_string());
        }
    }
    // At the version the lock pins, a first-party copy is the pinned
    // release only if it is byte for byte what the lock names (a local
    // build of that version is not, and says so by not being verified).
    if let Some(l) = lock.filter(|l| {
        m.release
            .as_ref()
            .is_some_and(|r| r.github.eq_ignore_ascii_case(&l.repo))
    }) {
        if l.version == m.version
            && (l.sha256_wasm != *m.wasm.sha256 || l.sha256_toml != toml_sha256)
        {
            verified = false;
        }
    }
    m.origin.first_party = first_party;
    m.origin.verified = verified;
}

/// `read_capped`, with a missing file as `None`.
fn read_optional(path: &Path, cap: u64) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => read_capped(path, cap).map(Some),
    }
}

/// A regular file's bytes, refused past `cap`.
fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = std::fs::File::open(path).map_err(|e| format!("{name}: {e}"))?;
    let meta = file.metadata().map_err(|e| format!("{name}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{name} is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{name}: {e}"))?;
    if bytes.len() as u64 > cap {
        return Err(format!("{name} is over its {cap}-byte cap"));
    }
    Ok(bytes)
}

/// The version a link in `dir` names; `None` when there is no link. A link
/// naming anything but a plain version directory is refused.
fn link(dir: &Path, name: &str) -> Result<Option<String>, String> {
    match std::fs::read_link(dir.join(name)) {
        Ok(target) => {
            let target = target.to_string_lossy().into_owned();
            super::plugin_version(&target)
                .map_err(|_| format!("{name} points at {target:?}, not a version directory"))?;
            Ok(Some(target))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("{name}: {err}")),
    }
}

/// Point `dir/name` at `version` atomically: a symlink under a fresh name,
/// then rename(2) over the old link. The target is relative, so a moved
/// data dir keeps working.
fn swap_link(dir: &Path, name: &str, version: &str) -> std::io::Result<()> {
    let staged = dir.join(format!(".{name}.new"));
    match std::fs::remove_file(&staged) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    std::os::unix::fs::symlink(version, &staged)?;
    std::fs::rename(&staged, dir.join(name))
}

/// A name no other staging in this process (or a previous one) uses.
fn nonce() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        crate::timeline::now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn io(what: &str) -> impl Fn(std::io::Error) -> Refusal + '_ {
    move |err| Refusal::internal(format!("{what}: {err}"))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Refusal> + Send + 'static,
) -> Result<T, Refusal> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Refusal::internal(format!("the plugin task failed: {e}")))?
}

/// `<dir>` made, leftovers of an interrupted change cleared (the caller
/// holds the change lock, so none is in flight), and a fresh temp dir in it.
fn stage(dir: &Path) -> Result<PathBuf, Refusal> {
    std::fs::create_dir_all(dir).map_err(io("could not create the plugin's directory"))?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let tmp = dir.join(format!("{TEMP_PREFIX}{}", nonce()));
    std::fs::create_dir(&tmp).map_err(io("could not create a temp directory"))?;
    Ok(tmp)
}

/// The streaming SHA-256 of a file, lowercase hex.
fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Move the verified `staged` tree to `<dir>/<version>` and make it current;
/// the version it replaces becomes `previous`, older ones go. Returns that
/// replaced version.
fn activate(dir: &Path, staged: &Path, version: &str) -> Result<Option<String>, Refusal> {
    let current = link(dir, CURRENT).map_err(Refusal::internal)?;
    let dest = dir.join(version);
    if dest.exists() {
        // A version kept from before (the previous one, reinstalled): the
        // fresh, verified download replaces it.
        let aside = dir.join(format!("{TEMP_PREFIX}old-{}", nonce()));
        std::fs::rename(&dest, &aside).map_err(io("could not set the old copy aside"))?;
        let _ = std::fs::remove_dir_all(&aside);
    }
    std::fs::rename(staged, &dest).map_err(io("could not move the download into place"))?;
    let replaced = current.filter(|c| c != version);
    // `previous` first: an interruption between the two swaps leaves the
    // old version current.
    if let Some(old) = &replaced {
        swap_link(dir, PREVIOUS, old).map_err(io("could not update the previous link"))?;
    }
    swap_link(dir, CURRENT, version).map_err(io("could not update the current link"))?;
    prune(dir, version, replaced.as_deref());
    Ok(replaced)
}

/// Remove version directories that are neither current nor previous
/// (best effort: a leftover is only disk, never loaded).
fn prune(dir: &Path, current: &str, previous: Option<&str>) {
    let previous = previous
        .map(str::to_string)
        .or_else(|| link(dir, PREVIOUS).ok().flatten());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if !is_dir || name.starts_with('.') || name == current || Some(&name) == previous.as_ref() {
            continue;
        }
        if let Err(err) = std::fs::remove_dir_all(entry.path()) {
            tracing::warn!(dir = %entry.path().display(), %err, "old plugin version not removed");
        }
    }
}

/// After any change: the catalog re-read, the plugin's instances and
/// compiled offers dropped (the next call runs the version now current),
/// footprints re-detected, windows told.
async fn after_change(state: &Arc<AppState>, id: &str) {
    let reloading = state.clone();
    if let Err(err) = tokio::task::spawn_blocking(move || reloading.plugin_catalog.reload()).await {
        tracing::error!(%err, "plugin catalog reload failed");
    }
    state.plugin_runtime.forget_plugin(id);
    crate::lock(&state.plugin_detect).clear();
    state.changes.notify_waiters();
}

/// What an install or update did.
struct Installed {
    id: String,
    version: String,
    replaced: Option<String>,
    wasm_sha256: String,
    toml_sha256: String,
}

/// Where a release install's files come from.
enum Fetch<'a> {
    /// `github`'s latest release, or the one tagged `v{version}`, as the
    /// releases API describes it.
    Release {
        github: &'a str,
        version: Option<&'a str>,
    },
    /// A first-party plugin's pinned release: direct downloads, and every
    /// checksum compared with the lock's too.
    Pinned(&'static Locked),
}

/// Install a release — `github`'s (the latest, or `version`), or a
/// first-party plugin's pinned one — or, with `update_of`, update that
/// plugin to its latest release. One path: see the module header.
async fn install(
    state: &Arc<AppState>,
    from: Fetch<'_>,
    update_of: Option<&Manifest>,
) -> Result<Installed, Refusal> {
    let _change = state.plugin_releases.changing.lock().await;
    let (release, pin, source) = match from {
        Fetch::Release { github, version } => (
            releases::fetch_release(state, github, version).await?,
            None,
            github.to_string(),
        ),
        Fetch::Pinned(l) => (releases::pinned_release(state, l), Some(l), l.repo.clone()),
    };
    if let Some(running) = update_of {
        if !releases::newer(&release.version, &running.version) {
            return Err(Refusal::conflict(format!(
                "{} {} is up to date (the latest release is {})",
                running.name, running.version, release.version
            )));
        }
    }
    let sums_file = releases::fetch_small(&release.sums_url, "SHA256SUMS").await?;
    let sums = releases::parse_sums(&String::from_utf8_lossy(&sums_file));
    let (Some(want_wasm), Some(want_toml)) = (
        sums.get("plugin.wasm").cloned(),
        sums.get("plugin.toml").cloned(),
    ) else {
        tracing::warn!(%source, version = %release.version, "plugin release refused: its SHA256SUMS does not list plugin.wasm and plugin.toml");
        return Err(Refusal::invalid(
            "that release doesn't list its plugin files, so it can't be checked or installed",
        ));
    };
    if let Some(l) = pin {
        // The release's own list must already be what the lock names:
        // nothing is downloaded for a release chimaera doesn't pin.
        for (file, want, pinned) in [
            ("plugin.wasm", &want_wasm, &l.sha256_wasm),
            ("plugin.toml", &want_toml, &l.sha256_toml),
        ] {
            if want != pinned {
                tracing::error!(
                    plugin = %l.id,
                    version = %l.version,
                    file,
                    pinned = %pinned,
                    listed = %want,
                    "plugin release refused: its SHA256SUMS does not match what chimaera pins (plugins.lock)"
                );
                return Err(Refusal::invalid(format!(
                    "{} {} on GitHub isn't the release chimaera approved, so it wasn't installed",
                    l.name, l.version
                )));
            }
        }
    }
    let toml = releases::fetch_small(&release.toml_url, "the release's plugin.toml").await?;
    let got_toml = crate::fs::sha256_hex(&toml);
    if got_toml != want_toml {
        tracing::error!(%source, version = %release.version, expected = %want_toml, got = %got_toml, "plugin download refused: plugin.toml does not match the release's SHA256SUMS");
        return Err(Refusal::invalid(DOWNLOAD_MISMATCH));
    }
    let text = std::str::from_utf8(&toml)
        .map_err(|_| Refusal::invalid("the release's plugin.toml is not UTF-8"))?;
    let m =
        super::parse_manifest(text).map_err(|e| Refusal::invalid(format!("the release's {e}")))?;
    let id = m.id.clone();
    if !m
        .release
        .as_ref()
        .is_some_and(|r| r.github.eq_ignore_ascii_case(&source))
    {
        return Err(Refusal::invalid(format!(
            "{source}'s plugin.toml must name that repository in release.github"
        )));
    }
    if let Some(copy) = state.plugin_catalog.installed_copy(&id) {
        if !copy
            .manifest
            .release
            .as_ref()
            .is_some_and(|r| r.github.eq_ignore_ascii_case(&source))
        {
            return Err(Refusal::conflict(format!(
                "{} is already installed from another source — remove it before changing publishers",
                m.name
            )));
        }
    }
    if let Some(running) = update_of {
        if running.id != id {
            return Err(Refusal::invalid(format!(
                "{source}'s release is the plugin {id:?}, not {:?}",
                running.id
            )));
        }
    }
    if let Some(l) = pin {
        let names = m.release.as_ref().map(|r| r.github.as_str());
        if id != l.id || names != Some(l.repo.as_str()) {
            return Err(Refusal::invalid(format!(
                "{} {}: its plugin.toml is {id:?} released from {}, not what chimaera pins \
                 ({:?} from {})",
                l.id,
                l.version,
                names.unwrap_or("nowhere"),
                l.id,
                l.repo
            )));
        }
    }
    releases::compatible(state, &id, &release, &toml).map_err(|why| {
        Refusal::invalid(format!(
            "{} {} can't be installed: {why}",
            m.name, release.version
        ))
    })?;
    // The same version again is refused — unless that copy's files no longer
    // match its release (the card's Reinstall): the fresh, verified download
    // replaces it in place (`activate`), keeping `previous`.
    if state
        .plugin_catalog
        .installed_copy(&id)
        .is_some_and(|c| c.manifest.version == release.version && c.manifest.origin.fault.is_none())
    {
        return Err(Refusal::conflict(format!(
            "{} {} is already installed",
            m.name, release.version
        )));
    }
    let dir = state.plugin_catalog.root.join(&id);
    let staging = dir.clone();
    let tmp = blocking(move || stage(&staging)).await?;
    let fetched =
        fetch_and_activate(&release, &dir, &tmp, &want_wasm, toml, sums_file, &source).await;
    if fetched.is_err() {
        let tmp = tmp.clone();
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(tmp)).await;
    }
    let replaced = fetched?;
    after_change(state, &id).await;
    tracing::info!(
        plugin = %id,
        version = %release.version,
        replaced = ?replaced,
        source = %source,
        pinned = pin.is_some(),
        sha256 = %want_wasm,
        "plugin {}",
        if update_of.is_some() { "updated" } else { "installed" }
    );
    Ok(Installed {
        id,
        version: release.version,
        replaced,
        wasm_sha256: want_wasm,
        toml_sha256: want_toml,
    })
}

/// The component into the temp dir, verified, the manifest and the
/// release's `SHA256SUMS` beside it, then the whole version made current.
async fn fetch_and_activate(
    release: &Release,
    dir: &Path,
    tmp: &Path,
    want_wasm: &str,
    toml: Vec<u8>,
    sums: Vec<u8>,
    source: &str,
) -> Result<Option<String>, Refusal> {
    let wasm = tmp.join("plugin.wasm");
    crate::agent_updates::curl_file(&release.wasm_url, &[], &wasm, WASM_MAX, DOWNLOAD_SECS)
        .await
        .map_err(|e| Refusal::upstream(format!("could not download plugin.wasm: {e:#}")))?;
    let (dir, tmp, want, version) = (
        dir.to_path_buf(),
        tmp.to_path_buf(),
        want_wasm.to_string(),
        release.version.clone(),
    );
    let source = source.to_owned();
    blocking(move || {
        let got = sha256_file(&wasm).map_err(io("could not read the download"))?;
        if got != want {
            tracing::error!(%version, expected = %want, %got, "plugin download refused: plugin.wasm does not match the release's SHA256SUMS");
            return Err(Refusal::invalid(DOWNLOAD_MISMATCH));
        }
        std::fs::write(tmp.join("plugin.toml"), &toml)
            .map_err(io("could not write plugin.toml"))?;
        std::fs::write(tmp.join(SUMS), &sums).map_err(io("could not write SHA256SUMS"))?;
        std::fs::write(tmp.join(SOURCE_GITHUB), source).map_err(io("could not write source-github"))?;
        activate(&dir, &tmp, &version)
    })
    .await
}

/// Install a local build: `plugin.toml`, `plugin.wasm` and, when there is
/// one, `SHA256SUMS` copied from `src` (an absolute directory) into
/// `<id>/<version>/`, with a `local-path` naming `src`. The same version
/// installed again replaces the copy (the loop of developing a plugin); a
/// `SHA256SUMS` there must match both files.
async fn install_path(state: &Arc<AppState>, src: PathBuf) -> Result<Installed, Refusal> {
    if !src.is_absolute() {
        return Err(Refusal::bad_request(format!(
            "{} is not an absolute path",
            src.display()
        )));
    }
    let _change = state.plugin_releases.changing.lock().await;
    let root = state.plugin_catalog.root.clone();
    let daemon = state.plugin_catalog.daemon_version();
    let (done, from) = blocking(move || {
        let src = src
            .canonicalize()
            .map_err(|e| Refusal::bad_request(format!("{}: {e}", src.display())))?;
        if !src.is_dir() {
            return Err(Refusal::bad_request(format!(
                "{} is not a directory",
                src.display()
            )));
        }
        let invalid = |e: String| Refusal::invalid(format!("{}: {e}", src.display()));
        let toml = read_capped(&src.join("plugin.toml"), TOML_MAX).map_err(invalid)?;
        let wasm = read_capped(&src.join("plugin.wasm"), WASM_MAX).map_err(invalid)?;
        let sums = read_optional(&src.join(SUMS), SUMS_MAX).map_err(invalid)?;
        let text = std::str::from_utf8(&toml)
            .map_err(|_| Refusal::invalid("its plugin.toml is not UTF-8"))?;
        let m = super::parse_manifest(text).map_err(|e| Refusal::invalid(format!("its {e}")))?;
        if let Some(why) = super::gate(&m, &daemon) {
            return Err(Refusal::invalid(format!(
                "{} {} can't be installed: {why}",
                m.name, m.version
            )));
        }
        let (wasm_sha256, toml_sha256) =
            (crate::fs::sha256_hex(&wasm), crate::fs::sha256_hex(&toml));
        if let Some(sums) = &sums {
            let sums = releases::parse_sums(&String::from_utf8_lossy(sums));
            for (file, got) in [("plugin.wasm", &wasm_sha256), ("plugin.toml", &toml_sha256)] {
                match sums.get(file) {
                    Some(want) if want == got => {}
                    Some(want) => {
                        return Err(Refusal::invalid(format!(
                            "{file} does not match the SHA256SUMS beside it (expected {want}, got {got})"
                        )))
                    }
                    None => {
                        return Err(Refusal::invalid(format!(
                            "the SHA256SUMS beside it does not list {file}"
                        )))
                    }
                }
            }
        }
        let dir = root.join(&m.id);
        let tmp = stage(&dir)?;
        let written = (|| {
            std::fs::write(tmp.join("plugin.toml"), &toml)
                .map_err(io("could not write plugin.toml"))?;
            std::fs::write(tmp.join("plugin.wasm"), &wasm)
                .map_err(io("could not write plugin.wasm"))?;
            if let Some(sums) = &sums {
                std::fs::write(tmp.join(SUMS), sums).map_err(io("could not write SHA256SUMS"))?;
            }
            std::fs::write(tmp.join(LOCAL_PATH), src.to_string_lossy().as_bytes())
                .map_err(io("could not write local-path"))?;
            activate(&dir, &tmp, &m.version)
        })();
        let replaced = match written {
            Ok(replaced) => replaced,
            Err(refusal) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(refusal);
            }
        };
        let done = Installed {
            id: m.id,
            version: m.version,
            replaced,
            wasm_sha256,
            toml_sha256,
        };
        Ok((done, src))
    })
    .await?;
    after_change(state, &done.id).await;
    tracing::info!(
        plugin = %done.id,
        version = %done.version,
        replaced = ?done.replaced,
        from = %from.display(),
        sha256 = %done.wasm_sha256,
        "plugin installed from a local directory"
    );
    Ok(done)
}

#[derive(Deserialize)]
pub(crate) struct InstallBody {
    /// `owner/repo` (a `https://github.com/owner/repo` URL is read as one).
    #[serde(default)]
    github: Option<String>,
    /// A release version; the latest when absent.
    #[serde(default)]
    version: Option<String>,
    /// Instead of a release: an absolute directory holding a local build.
    #[serde(default)]
    path: Option<PathBuf>,
}

/// What was typed for a repository, as the install route and Preview read
/// it: `owner/repo`, or its `https://github.com/owner/repo` URL (trimmed;
/// `valid_github` judges the rest).
pub(crate) fn normalize_github(typed: &str) -> &str {
    let typed = typed.trim();
    typed
        .strip_prefix("https://github.com/")
        .unwrap_or(typed)
        .trim_end_matches('/')
}

/// POST /plugins/install {github, version?} | {path} — install a plugin
/// from its GitHub release, or a local build from a directory (the user's
/// click, or `chimaera plugin add`). A first-party plugin's repository
/// installs the release the lock pins (unless another `version` is asked
/// for).
pub(crate) async fn install_route(
    State(state): State<Arc<AppState>>,
    Json(body): Json<InstallBody>,
) -> Response {
    let result = match (body.github.as_deref(), body.path) {
        (None, Some(path)) => install_path(&state, path).await,
        (Some(github), None) => {
            let github = normalize_github(github);
            let version = body
                .version
                .as_deref()
                .map(|v| v.trim().trim_start_matches('v'));
            let pinned = super::lock_entries()
                .iter()
                .find(|l| l.repo.eq_ignore_ascii_case(github))
                .filter(|l| version.is_none_or(|v| v == l.version));
            let from = match pinned {
                Some(l) => Fetch::Pinned(l),
                None => Fetch::Release { github, version },
            };
            install(&state, from, None).await
        }
        _ => Err(Refusal::bad_request(
            "name a repository (github) or a local directory (path)",
        )),
    };
    installed_reply(&state, result)
}

/// POST /plugins/{pid}/install — install a first-party plugin: the release
/// `plugins/plugins.lock` pins for it, checked against that release's
/// `SHA256SUMS` and the lock.
pub(crate) async fn pinned_install_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    let Some(l) = super::lock_entry(&pid) else {
        return Refusal::not_found(format!(
            "{pid:?} is not a plugin chimaera pins — install it from its repository"
        ))
        .into_response();
    };
    installed_reply(&state, install(&state, Fetch::Pinned(l), None).await)
}

/// POST /plugins/{pid}/update — install the plugin's latest release, when
/// it is newer than what runs and this daemon can run it.
pub(crate) async fn update_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    let Some(m) = super::manifest(&state, &pid) else {
        return super::not_installed(&pid).into_response();
    };
    let Some(github) = m.origin.release.clone() else {
        return releases::no_source(&m).into_response();
    };
    let from = Fetch::Release {
        github: &github,
        version: None,
    };
    installed_reply(&state, install(&state, from, Some(&m)).await)
}

fn installed_reply(state: &AppState, result: Result<Installed, Refusal>) -> Response {
    match result {
        Ok(done) => Json(json!({
            "id": done.id,
            "version": done.version,
            "previous": done.replaced,
            "sha256": {"plugin.wasm": done.wasm_sha256, "plugin.toml": done.toml_sha256},
            "plugin": super::entry_json(state, &done.id),
        }))
        .into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// POST /plugins/{pid}/rollback — Use previous: `current` back to the
/// previous version, which keeps the newer one as `previous` (so this is
/// reversible). Refused when that version can't run on this daemon.
pub(crate) async fn rollback_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    match rollback(&state, &pid).await {
        Ok((version, previous)) => Json(json!({
            "id": pid,
            "version": version,
            "previous": previous,
            "plugin": super::entry_json(&state, &pid),
        }))
        .into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

async fn rollback(state: &Arc<AppState>, pid: &str) -> Result<(String, String), Refusal> {
    let _change = state.plugin_releases.changing.lock().await;
    let Some(copy) = state.plugin_catalog.installed_copy(pid) else {
        return Err(super::not_installed(pid));
    };
    let Some(previous) = copy.previous.clone() else {
        return Err(Refusal::conflict(format!(
            "{} has no previous version to go back to",
            copy.manifest.name
        )));
    };
    let current = copy.manifest.version.clone();
    let dir = state.plugin_catalog.root.join(pid);
    let daemon = state.plugin_catalog.daemon_version();
    let (id, back_to, now) = (pid.to_string(), previous.clone(), current.clone());
    blocking(move || {
        let m = load_version(&dir, &id, &back_to)
            .map_err(|e| Refusal::invalid(format!("{id} {back_to} can't be loaded: {e}")))?;
        if let Some(why) = m.origin.fault.clone().or_else(|| super::gate(&m, &daemon)) {
            return Err(Refusal::invalid(format!(
                "{} {back_to} can't run on this daemon: {why}",
                m.name
            )));
        }
        swap_link(&dir, CURRENT, &back_to).map_err(io("could not update the current link"))?;
        swap_link(&dir, PREVIOUS, &now).map_err(io("could not update the previous link"))?;
        Ok(())
    })
    .await?;
    after_change(state, pid).await;
    tracing::info!(plugin = %pid, version = %previous, from = %current, "plugin rolled back");
    Ok((previous, current))
}

/// DELETE /plugins/{pid} — Remove: the installed copy's whole directory
/// (every version). A first-party plugin is listed as available again.
pub(crate) async fn remove_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    match remove(&state, &pid).await {
        Ok(()) => Json(json!({
            "id": pid,
            "removed": true,
            "plugin": super::entry_json(&state, &pid),
        }))
        .into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

async fn remove(state: &Arc<AppState>, pid: &str) -> Result<(), Refusal> {
    if !super::valid_id(pid) {
        return Err(Refusal::not_found("unknown plugin"));
    }
    let _change = state.plugin_releases.changing.lock().await;
    let root = state.plugin_catalog.root.clone();
    let dir = root.join(pid);
    let aside = root.join(format!("{TEMP_PREFIX}removed-{pid}-{}", nonce()));
    let removed = blocking(move || {
        if !dir.is_dir() {
            return Ok(false);
        }
        // Set aside first, so a half-deleted tree is never what `scan` finds.
        std::fs::rename(&dir, &aside).map_err(io("could not remove the plugin"))?;
        if let Err(err) = std::fs::remove_dir_all(&aside) {
            tracing::warn!(dir = %aside.display(), %err, "removed plugin's files not all deleted");
        }
        Ok(true)
    })
    .await?;
    if !removed {
        return Err(super::not_installed(pid));
    }
    state.plugin_releases.forget(pid);
    after_change(state, pid).await;
    tracing::info!(plugin = %pid, "plugin removed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-installed-{label}-{}-{}",
            std::process::id(),
            nonce()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest(id: &str, version: &str) -> String {
        format!(
            "id = \"{id}\"\nname = \"Demo\"\nversion = \"{version}\"\nsummary = \"x\"\napi = \"0.1\"\n"
        )
    }

    fn plant(root: &Path, id: &str, version: &str) {
        let v = root.join(id).join(version);
        std::fs::create_dir_all(&v).unwrap();
        std::fs::write(v.join("plugin.toml"), manifest(id, version)).unwrap();
        std::fs::write(v.join("plugin.wasm"), b"\0asm").unwrap();
    }

    #[test]
    fn scan_reads_current_and_previous_and_skips_what_does_not_hold() {
        let root = dir("scan");
        plant(&root, "demo", "0.1.0");
        plant(&root, "demo", "0.2.0");
        swap_link(&root.join("demo"), CURRENT, "0.2.0").unwrap();
        swap_link(&root.join("demo"), PREVIOUS, "0.1.0").unwrap();
        // No current link: nothing installed.
        plant(&root, "bare", "1.0.0");
        // A manifest for another id.
        plant(&root, "liar", "1.0.0");
        std::fs::write(
            root.join("liar/1.0.0/plugin.toml"),
            manifest("someone-else", "1.0.0"),
        )
        .unwrap();
        swap_link(&root.join("liar"), CURRENT, "1.0.0").unwrap();
        // A link that names no version.
        std::fs::create_dir_all(root.join("odd")).unwrap();
        std::os::unix::fs::symlink("../demo/0.2.0", root.join("odd/current")).unwrap();
        // Hidden and staging names are never read.
        std::fs::create_dir_all(root.join(".tmp-x")).unwrap();

        let copies = scan(&root);
        assert_eq!(copies.len(), 1, "{copies:?}");
        let c = &copies[0];
        assert_eq!(c.manifest.id, "demo");
        assert_eq!(c.manifest.version, "0.2.0");
        assert_eq!(c.previous.as_deref(), Some("0.1.0"));
        assert_eq!(c.dir, root.join("demo/0.2.0"));
        assert_eq!(&**c.manifest.wasm.bytes, b"\0asm");
        assert!(!c.manifest.origin.verified, "no SHA256SUMS beside it");
        assert_eq!(c.manifest.origin.fault, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn activation_swaps_current_keeps_one_previous_and_prunes_the_rest() {
        let root = dir("activate");
        let d = root.join("demo");
        for (i, version) in ["0.1.0", "0.2.0", "0.3.0"].into_iter().enumerate() {
            let tmp = stage(&d).unwrap();
            std::fs::write(tmp.join("plugin.toml"), manifest("demo", version)).unwrap();
            std::fs::write(tmp.join("plugin.wasm"), b"\0asm").unwrap();
            let replaced = activate(&d, &tmp, version).unwrap();
            assert_eq!(replaced.is_some(), i > 0);
        }
        assert_eq!(link(&d, CURRENT).unwrap().as_deref(), Some("0.3.0"));
        assert_eq!(link(&d, PREVIOUS).unwrap().as_deref(), Some("0.2.0"));
        assert!(!d.join("0.1.0").exists(), "older versions are pruned");
        let left: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(TEMP_PREFIX))
            .collect();
        assert!(left.is_empty(), "no staging left: {left:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    fn sums(wasm: &[u8], toml: &str) -> String {
        format!(
            "{}  plugin.wasm\n{}  plugin.toml\n",
            crate::fs::sha256_hex(wasm),
            crate::fs::sha256_hex(toml.as_bytes())
        )
    }

    #[test]
    fn a_read_checks_the_files_against_the_sums_kept_beside_them() {
        let root = dir("sums");
        let load = |root: &Path| load_version(&root.join("demo"), "demo", "0.1.0").unwrap();
        plant(&root, "demo", "0.1.0");
        let v = root.join("demo/0.1.0");
        let m = load(&root);
        assert!(
            !m.origin.verified && m.origin.fault.is_none(),
            "none kept: unverified"
        );
        assert!(!m.origin.first_party, "not in the lock");

        std::fs::write(v.join(SUMS), sums(b"\0asm", &manifest("demo", "0.1.0"))).unwrap();
        let m = load(&root);
        assert!(m.origin.verified, "{:?}", m.origin);
        assert_eq!(m.origin.fault, None);
        assert_eq!(m.origin.local_path, None);

        // A byte changed after the install: listed with the fault.
        std::fs::write(v.join("plugin.wasm"), b"\0asn").unwrap();
        let m = load(&root);
        assert!(!m.origin.verified);
        assert_eq!(m.origin.fault.as_deref(), Some(SUMS_MISMATCH));
        // A list that doesn't name both files is a mismatch too.
        std::fs::write(v.join("plugin.wasm"), b"\0asm").unwrap();
        std::fs::write(v.join(SUMS), "").unwrap();
        assert_eq!(load(&root).origin.fault.as_deref(), Some(SUMS_MISMATCH));

        std::fs::remove_file(v.join(SUMS)).unwrap();
        std::fs::write(v.join(LOCAL_PATH), "/home/me/demo\n").unwrap();
        assert_eq!(
            load(&root).origin.local_path,
            Some(PathBuf::from("/home/me/demo"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn first_party_requires_the_lock_bytes_or_the_recorded_release_source() {
        let l = crate::plugins::lock_entry("agent-notes").unwrap();
        let text = |version: &str, github: &str| {
            format!(
                "id = \"agent-notes\"\nname = \"Agent notes\"\nversion = \"{version}\"\n\
                 summary = \"x\"\napi = \"0.1\"\n[release]\ngithub = \"{github}\"\n"
            )
        };
        let checked = |toml: &str, sums_of: Option<&str>, source: Option<&str>| {
            let mut m = crate::plugins::parse_manifest(toml).unwrap();
            m.wasm = Wasm::new(Cow::Borrowed(b"\0asm"));
            let kept = sums_of.map(|t| sums(b"\0asm", t));
            check_files(
                &mut m,
                &crate::fs::sha256_hex(toml.as_bytes()),
                kept.as_deref().map(str::as_bytes),
                source,
            );
            m.origin
        };
        // Released from the lock's repository, past the pin: first-party and
        // verified by its own SHA256SUMS.
        let newer = text("9.0.0", &l.repo);
        let o = checked(&newer, Some(&newer), Some(&l.repo));
        assert!(o.first_party && o.verified, "{o:?}");
        // The same id from another repository never is.
        let other = text("9.0.0", "acme/agent-notes");
        let o = checked(&other, Some(&other), Some("acme/agent-notes"));
        assert!(!o.first_party && o.verified, "{o:?}");
        // At the pinned version, bytes that aren't the lock's are not the
        // pinned release: unverified, no fault (a local build of it).
        let pinned = text(&l.version, &l.repo);
        let o = checked(&pinned, Some(&pinned), None);
        assert!(!o.first_party && !o.verified && o.fault.is_none(), "{o:?}");
        let o = checked(&newer, Some(&newer), None);
        assert!(
            !o.first_party && o.verified,
            "a local build cannot grant itself the badge: {o:?}"
        );
    }

    #[test]
    fn a_capped_read_refuses_what_is_over() {
        let root = dir("cap");
        std::fs::write(root.join("f"), vec![0u8; 10]).unwrap();
        assert_eq!(read_capped(&root.join("f"), 10).unwrap().len(), 10);
        assert!(read_capped(&root.join("f"), 9).unwrap_err().contains("cap"));
        let _ = std::fs::remove_dir_all(root);
    }
}
