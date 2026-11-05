//! "Open in PowerPoint": hand a deck the window shows to the real app.
//!
//! A local window's file opens in place. Another host's file goes on a
//! **round trip**: the shell downloads a working copy into its cache, opens
//! that, watches it, and uploads every save PowerPoint makes back to the host
//! through `PUT /api/v1/fs/file/stream` with the host's last known hash as
//! the precondition. A host that changed the file meanwhile answers 409, the
//! trip turns `conflict` and holds every later save until the person picks
//! one of the resolutions (`office_trip_resolve`): nothing is ever written
//! over the host's version without being asked.
//!
//! Rules that bite:
//! - **The host is the caller's own** (`caller_scope`), never an argument: a
//!   page can only act on its own host's files.
//! - **No macro-enabled deck is pulled from another machine** (`.pptm`): the
//!   remote allowlist is macro-free; only this machine's own files may be one.
//! - **Every upload but "save as a copy" carries `expect_hash`**, and the copy
//!   goes to a name the host reported free.
//! - Trips persist (≤ [`MAX_TRIPS`], tokenless, atomic) and resume at launch:
//!   a working copy saved while the app was closed uploads on resume, because
//!   `last_local_hash` is persisted with it.
//! - Every file and network operation runs on a blocking thread; every trip
//!   mutation that spans one holds `Office::io`, so the watcher and the
//!   person's choices never interleave.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use super::lock;

/// The event a trip's state rides (web `listenOfficeTrip`).
const EVENT: &str = "office-trip";
/// Trips kept at once (also the persisted file's cap).
const MAX_TRIPS: usize = 32;
/// The persisted trips file's read cap: 32 small records fit many times over.
const MAX_TRIPS_FILE_BYTES: u64 = 512 * 1024;
/// The daemon's own cap for a streamed file (`fs::MAX_PACKAGE_BYTES`).
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// How often a working copy's metadata is read.
const POLL: Duration = Duration::from_secs(2);
/// A zip's end-of-central-directory record is 22 bytes plus a comment of at
/// most 64 KiB, so it lies within this many bytes of the end.
const EOCD_SEARCH: u64 = 22 + 0xFFFF;
/// Earlier working copies a trip keeps on disk ("open theirs" never replaces
/// the file PowerPoint has open); older ones are deleted.
const MAX_RETIRED: usize = 8;
/// `<stem> (PowerPoint copy N).<ext>` names tried before giving up.
const MAX_COPY_NAMES: u32 = 50;
/// Working-copy directories tried per (host, path) before giving up.
const MAX_DIR_SLOTS: u32 = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const API_TIMEOUT: Duration = Duration::from_secs(20);

/// Decks the shell hands to an app from this machine.
const LOCAL_EXTENSIONS: &[&str] = &["pptx", "pptm", "ppsx", "potx"];
/// Decks it pulls from another host: macro-free only.
const REMOTE_EXTENSIONS: &[&str] = &["pptx", "ppsx", "potx"];

type TripKey = (String, String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TripState {
    Watching,
    Uploading,
    Conflict,
    Error,
    /// PowerPoint closed the deck with every save on the host: the trip is
    /// over (only ever sent, never stored).
    Done,
}

/// One round trip, as persisted. Holds no token: the endpoint is re-read
/// from the live tunnel for every request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Trip {
    alias: String,
    remote_path: String,
    local_path: PathBuf,
    /// The host's hash we last downloaded or uploaded: every upload's
    /// precondition.
    base_hash: String,
    /// The working copy's hash the host last received (or was downloaded).
    last_local_hash: String,
    uploaded_ms: Option<u64>,
    state: TripState,
    message: Option<String>,
    /// The host's hash a 409 reported: "overwrite"'s precondition.
    conflict_hash: Option<String>,
    /// Earlier working-copy directories, deleted with the trip.
    #[serde(default)]
    retired: Vec<PathBuf>,
}

/// The wire shape (web `OfficeTrip`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct TripView {
    path: String,
    state: TripState,
    uploaded_ms: Option<u64>,
    message: Option<String>,
}

/// `office_open`'s answer (web `OfficeOpened`).
#[derive(Serialize)]
pub(crate) struct Opened {
    app: String,
    trip: Option<TripView>,
}

impl Trip {
    fn new(alias: String, remote_path: String, local_path: PathBuf, hash: String) -> Self {
        Trip {
            alias,
            remote_path,
            local_path,
            base_hash: hash.clone(),
            last_local_hash: hash,
            uploaded_ms: None,
            state: TripState::Watching,
            message: None,
            conflict_hash: None,
            retired: Vec::new(),
        }
    }

    fn key(&self) -> TripKey {
        (self.alias.clone(), self.remote_path.clone())
    }

    fn view(&self) -> TripView {
        TripView {
            path: self.remote_path.clone(),
            state: self.state,
            uploaded_ms: self.uploaded_ms,
            message: self.message.clone(),
        }
    }

    fn name(&self) -> &str {
        basename(&self.remote_path)
    }
}

// --- pure parts ------------------------------------------------------------------

/// The lowercased extension of `path`'s file name, when it has a stem.
fn extension(path: &str) -> Option<String> {
    let (stem, ext) = basename(path).rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

fn basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Refuse a path this shell won't hand to PowerPoint. `remote`: the deck
/// comes from another host, so macro-enabled files are out.
fn check_deck(path: &str, remote: bool) -> Result<(), String> {
    if cfg!(windows) {
        return Err("not available on Windows".to_string());
    }
    if !path.starts_with('/') {
        return Err("not an absolute path".to_string());
    }
    if path.len() > 4096 || path.chars().any(char::is_control) {
        return Err("not a usable path".to_string());
    }
    let ext = extension(path).unwrap_or_default();
    if remote && ext == "pptm" {
        return Err(
            "a deck with macros isn't opened from another machine: download it instead".to_string(),
        );
    }
    let allowed = if remote {
        REMOTE_EXTENSIONS
    } else {
        LOCAL_EXTENSIONS
    };
    if allowed.contains(&ext.as_str()) {
        Ok(())
    } else {
        Err("only PowerPoint decks open in PowerPoint".to_string())
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(bytes))
}

/// A host alias as one directory name: `[A-Za-z0-9._-]`, never starting with
/// a dot (no `..`, nothing hidden), bounded; an alias that had to change
/// carries a hash of the original, so two aliases never share a directory.
fn alias_dir(alias: &str) -> String {
    let mut clean: String = alias
        .chars()
        .take(48)
        .enumerate()
        .map(|(i, c)| match c {
            '.' if i == 0 => '_',
            c if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') => c,
            _ => '_',
        })
        .collect();
    if clean.is_empty() || clean != alias {
        clean.push('-');
        clean.push_str(&sha256_hex(alias.as_bytes())[..8]);
    }
    clean
}

/// The `n`th working-copy directory for (host, path): the first is
/// `<alias>/<16 hex of sha256(path)>`, later ones get `.2`, `.3`, ….
fn dir_slot(root: &Path, alias: &str, remote_path: &str, n: u32) -> PathBuf {
    let hash = &sha256_hex(remote_path.as_bytes())[..16];
    let name = if n <= 1 {
        hash.to_string()
    } else {
        format!("{hash}.{n}")
    };
    root.join(alias_dir(alias)).join(name)
}

/// `<stem> (PowerPoint copy).<ext>` beside `remote_path`, then
/// `… (PowerPoint copy 2).<ext>`, ….
fn copy_path(remote_path: &str, n: u32) -> Option<String> {
    let (dir, name) = remote_path.rsplit_once('/')?;
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    let tag = if n <= 1 {
        "PowerPoint copy".to_string()
    } else {
        format!("PowerPoint copy {n}")
    };
    Some(format!("{dir}/{stem} ({tag}).{ext}"))
}

/// Whether `tail` (a file's last ≤ [`EOCD_SEARCH`] bytes) ends with a zip's
/// end-of-central-directory record whose comment runs exactly to the end:
/// the archive was written through. A signature inside compressed data does
/// not pass, since its comment length would not match.
fn zip_complete(tail: &[u8]) -> bool {
    const SIG: &[u8] = b"PK\x05\x06";
    if tail.len() < 22 {
        return false;
    }
    (0..=tail.len() - 22).rev().any(|at| {
        tail[at..at + 4] == *SIG && {
            let comment = u16::from_le_bytes([tail[at + 20], tail[at + 21]]) as usize;
            at + 22 + comment == tail.len()
        }
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A slow tunnel still finishes a 256 MiB transfer: a minute plus 128 KiB/s.
fn transfer_timeout(len: u64) -> Duration {
    Duration::from_secs(60 + len / (128 * 1024))
}

// --- the watcher's state machine (pure over `Disk` + `Remote`) ------------------

/// A working copy's identity on disk: a save (in place or temp + rename)
/// changes at least one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    len: u64,
    mtime_ns: u128,
    ino: u64,
}

pub(crate) trait Disk {
    fn stat(&self, path: &Path) -> Option<Fingerprint>;
    /// The file's SHA-256 when it is a complete zip, `None` while it is not
    /// (a save still being written); `Err` with a short human reason.
    fn complete_hash(&self, path: &Path) -> Result<Option<String>, String>;
    /// An app holds the file open: PowerPoint (like Word) keeps a `~$` owner
    /// file beside a document it has open. A working copy has a directory of
    /// its own, so any owner file there is this deck's.
    fn held_open(&self, path: &Path) -> bool;
}

pub(crate) enum Sent {
    Saved { hash: String },
    Conflict { hash: Option<String> },
}

pub(crate) enum Failure {
    /// No answer: try again on the next tick.
    Unreachable(String),
    /// The host answered no: try again on the next change.
    Refused(String),
}

impl Failure {
    fn message(self) -> String {
        match self {
            Failure::Unreachable(m) | Failure::Refused(m) => m,
        }
    }
}

pub(crate) trait Remote {
    /// Stream `local` to `remote_path`, under `expect` when given.
    fn upload(
        &self,
        remote_path: &str,
        expect: Option<&str>,
        local: &Path,
        local_hash: &str,
    ) -> Result<Sent, Failure>;
    fn exists(&self, remote_path: &str) -> Result<bool, Failure>;
}

/// What the watcher knows about one working copy between ticks (not
/// persisted: a resumed trip starts blank and so re-examines the file once).
#[derive(Clone, Debug, Default)]
pub(crate) struct Watch {
    /// The previous poll's fingerprint: a change must hold for two polls.
    seen: Option<Fingerprint>,
    /// The fingerprint last examined (hashed and acted on).
    settled: Option<Fingerprint>,
    /// The last attempt didn't reach the host: examine again next tick.
    retry: bool,
    /// The host's tunnel was down at the last tick.
    waiting: bool,
    /// The last hash taken, so a retry or a resolution doesn't re-read it.
    hashed: Option<(Fingerprint, String)>,
    /// PowerPoint has had the deck open (its owner file was seen), or the
    /// trip was resumed at launch (it was open when the trip began).
    app_seen: bool,
    /// Polls in a row with PowerPoint's owner file gone.
    closed_polls: u8,
}

/// Polls PowerPoint's owner file must stay gone before the trip ends (a save
/// can briefly replace the files beside it).
const CLOSED_POLLS: u8 = 3;

impl Watch {
    /// A trip resumed at launch: its deck was open in PowerPoint when the
    /// trip began, so the owner file going (or being gone) ends it.
    fn resumed() -> Self {
        Watch {
            app_seen: true,
            ..Watch::default()
        }
    }

    fn settled_at(fp: Option<Fingerprint>, hash: &str) -> Self {
        Watch {
            seen: fp,
            settled: fp,
            hashed: fp.map(|fp| (fp, hash.to_string())),
            ..Watch::default()
        }
    }
}

fn set_status(trip: &mut Trip, state: TripState, message: Option<String>) -> bool {
    if trip.state == state && trip.message == message {
        return false;
    }
    trip.state = state;
    trip.message = message;
    true
}

/// The working copy's hash now, reusing the last one taken for an unchanged
/// file.
fn current_hash(
    watch: &mut Watch,
    disk: &dyn Disk,
    local: &Path,
    fp: Fingerprint,
) -> Result<Option<String>, String> {
    if let Some((at, hash)) = &watch.hashed {
        if *at == fp {
            return Ok(Some(hash.clone()));
        }
    }
    let hash = disk.complete_hash(local)?;
    if let Some(hash) = &hash {
        watch.hashed = Some((fp, hash.clone()));
    }
    Ok(hash)
}

/// The settled working copy's hash for a resolution, or why there is none.
fn hash_for_resolution(trip: &Trip, watch: &mut Watch, disk: &dyn Disk) -> Result<String, String> {
    let fp = disk
        .stat(&trip.local_path)
        .ok_or_else(|| "PowerPoint's copy is gone".to_string())?;
    let hash = current_hash(watch, disk, &trip.local_path, fp)?
        .ok_or_else(|| "PowerPoint is still saving it: try again in a moment".to_string())?;
    watch.seen = Some(fp);
    watch.settled = Some(fp);
    Ok(hash)
}

fn saved(trip: &mut Trip, hash: String, now_ms: u64) {
    trip.base_hash.clone_from(&hash);
    trip.last_local_hash = hash;
    trip.uploaded_ms = Some(now_ms);
    trip.state = TripState::Watching;
    trip.message = None;
    trip.conflict_hash = None;
}

/// One watcher tick for one trip. `net` is `None` while the host's tunnel is
/// down. Returns whether the trip changed (persist + emit); `uploading` is
/// told just before an upload starts.
pub(crate) fn step(
    trip: &mut Trip,
    watch: &mut Watch,
    disk: &dyn Disk,
    net: Option<&dyn Remote>,
    now_ms: u64,
    uploading: &mut dyn FnMut(&Trip),
) -> bool {
    if disk.held_open(&trip.local_path) {
        watch.app_seen = true;
        watch.closed_polls = 0;
    } else if watch.app_seen {
        watch.closed_polls = watch.closed_polls.saturating_add(1);
    }
    // Held: only the person's choice moves a conflict on.
    if trip.state == TripState::Conflict {
        return false;
    }
    let Some(net) = net else {
        watch.waiting = true;
        let message = format!("waiting for {}", trip.alias);
        return set_status(trip, TripState::Error, Some(message));
    };
    let mut changed = false;
    if std::mem::take(&mut watch.waiting) {
        watch.retry = true;
        changed |= set_status(trip, TripState::Watching, None);
    }
    let Some(fp) = disk.stat(&trip.local_path) else {
        watch.seen = None;
        return changed;
    };
    if watch.seen != Some(fp) {
        watch.seen = Some(fp);
        return changed;
    }
    if watch.settled == Some(fp) && !watch.retry {
        return changed;
    }
    let hash = match current_hash(watch, disk, &trip.local_path, fp) {
        Ok(Some(hash)) => hash,
        // Mid-write: look again next tick.
        Ok(None) => return changed,
        Err(message) => {
            watch.settled = Some(fp);
            watch.retry = false;
            return set_status(trip, TripState::Error, Some(message)) || changed;
        }
    };
    watch.settled = Some(fp);
    watch.retry = false;
    if hash == trip.last_local_hash {
        return set_status(trip, TripState::Watching, None) || changed;
    }
    let expect = trip.base_hash.clone();
    trip.state = TripState::Uploading;
    trip.message = None;
    uploading(trip);
    match net.upload(&trip.remote_path, Some(&expect), &trip.local_path, &hash) {
        Ok(Sent::Saved { hash }) => saved(trip, hash, now_ms),
        Ok(Sent::Conflict { hash }) => {
            trip.state = TripState::Conflict;
            trip.message = None;
            trip.conflict_hash = hash;
        }
        Err(Failure::Unreachable(message)) => {
            trip.state = TripState::Error;
            trip.message = Some(message);
            watch.retry = true;
        }
        Err(Failure::Refused(message)) => {
            trip.state = TripState::Error;
            trip.message = Some(message);
        }
    }
    true
}

/// "overwrite": send the working copy over the host's changed file, under
/// the hash the 409 reported (so a third version still isn't overwritten).
pub(crate) fn overwrite(
    trip: &mut Trip,
    watch: &mut Watch,
    disk: &dyn Disk,
    net: &dyn Remote,
    now_ms: u64,
) -> Result<(), String> {
    if trip.state != TripState::Conflict {
        return Err("nothing to overwrite".to_string());
    }
    let Some(expect) = trip.conflict_hash.clone() else {
        return Err(format!(
            "{} is gone from {}: save PowerPoint's as a copy instead",
            trip.name(),
            trip.alias
        ));
    };
    let hash = hash_for_resolution(trip, watch, disk)?;
    match net.upload(&trip.remote_path, Some(&expect), &trip.local_path, &hash) {
        Ok(Sent::Saved { hash }) => {
            saved(trip, hash, now_ms);
            Ok(())
        }
        Ok(Sent::Conflict { hash }) => {
            trip.conflict_hash = hash;
            Err(format!(
                "{} changed again on {}: nothing was overwritten",
                trip.name(),
                trip.alias
            ))
        }
        Err(failure) => Err(failure.message()),
    }
}

/// "copy": send the working copy beside the host's file under the first
/// free copy name; the trip then targets the copy, so later saves never
/// touch the other version. Returns the copy's path.
pub(crate) fn save_copy(
    trip: &mut Trip,
    watch: &mut Watch,
    disk: &dyn Disk,
    net: &dyn Remote,
    now_ms: u64,
) -> Result<String, String> {
    let hash = hash_for_resolution(trip, watch, disk)?;
    let mut target = None;
    for n in 1..=MAX_COPY_NAMES {
        let candidate = copy_path(&trip.remote_path, n)
            .ok_or_else(|| "this file has no name to copy".to_string())?;
        if !net.exists(&candidate).map_err(Failure::message)? {
            target = Some(candidate);
            break;
        }
    }
    let target = target.ok_or_else(|| "too many copies beside it already".to_string())?;
    match net.upload(&target, None, &trip.local_path, &hash) {
        Ok(Sent::Saved { hash }) => {
            trip.remote_path.clone_from(&target);
            saved(trip, hash, now_ms);
            Ok(target)
        }
        Ok(Sent::Conflict { .. }) => Err(format!("{} refused the copy", trip.alias)),
        Err(failure) => Err(failure.message()),
    }
}

/// The trip is over: PowerPoint closed the deck (its owner file stayed gone
/// for a few polls after being seen) and the host has every save. A trip in
/// conflict, failing or mid-upload never ends this way — the person decides.
pub(crate) fn finished(trip: &Trip, watch: &mut Watch, disk: &dyn Disk) -> bool {
    trip.state == TripState::Watching
        && trip.message.is_none()
        && watch.app_seen
        && watch.closed_polls >= CLOSED_POLLS
        && !watch.retry
        && may_finish(trip, watch, disk).is_ok()
}

/// "done" is refused while the working copy holds what the host lacks.
pub(crate) fn may_finish(trip: &Trip, watch: &mut Watch, disk: &dyn Disk) -> Result<(), String> {
    let Some(fp) = disk.stat(&trip.local_path) else {
        return Ok(());
    };
    let unsent = match current_hash(watch, disk, &trip.local_path, fp)? {
        Some(hash) => hash != trip.last_local_hash,
        None => true,
    };
    if !unsent {
        return Ok(());
    }
    Err(if trip.state == TripState::Conflict {
        format!(
            "PowerPoint's copy has changes {} doesn't have: save it as a copy, open theirs, or overwrite",
            trip.alias
        )
    } else {
        format!("PowerPoint's last save hasn't reached {} yet", trip.alias)
    })
}

// --- persistence -------------------------------------------------------------------

/// The trips, backed by a small capped JSON file in the data dir.
pub(crate) struct TripStore {
    path: PathBuf,
    trips: Vec<Trip>,
}

impl TripStore {
    /// Load-tolerant: a missing, oversized or corrupt file is an empty set.
    /// A record whose working copy lies outside `root` is dropped: the
    /// watcher uploads whatever that path holds.
    pub(crate) fn load(path: PathBuf, root: &Path) -> Self {
        let inside = |p: &Path| {
            p.starts_with(root)
                && p != root
                && !p
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
        };
        let trips = match std::fs::metadata(&path) {
            Ok(meta) if meta.len() > MAX_TRIPS_FILE_BYTES => {
                tracing::warn!(path = %path.display(), "office trips file over its cap; ignoring it");
                Vec::new()
            }
            Ok(_) => std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Vec<Trip>>(&bytes).ok())
                .unwrap_or_else(|| {
                    tracing::warn!(path = %path.display(), "invalid office trips file; ignoring it");
                    Vec::new()
                }),
            Err(_) => Vec::new(),
        };
        let mut kept: Vec<Trip> = Vec::new();
        for mut trip in trips {
            if kept.len() >= MAX_TRIPS
                || !inside(&trip.local_path)
                || kept.iter().any(|t| t.key() == trip.key())
            {
                continue;
            }
            trip.retired.retain(|dir| inside(dir));
            // An upload the last run was in the middle of: the resumed
            // watcher decides afresh.
            if trip.state == TripState::Uploading {
                trip.state = TripState::Watching;
            }
            kept.push(trip);
        }
        TripStore { path, trips: kept }
    }

    fn get(&self, key: &TripKey) -> Option<&Trip> {
        self.trips.iter().find(|t| &t.key() == key)
    }

    /// Replace the trip at `old` (or add one), refusing past the cap.
    fn put(&mut self, old: &TripKey, trip: Trip) -> Result<(), String> {
        if let Some(at) = self.trips.iter().position(|t| &t.key() == old) {
            self.trips[at] = trip;
            return Ok(());
        }
        if self.trips.len() >= MAX_TRIPS {
            return Err(format!(
                "{MAX_TRIPS} decks are already open in PowerPoint: press done on one first"
            ));
        }
        self.trips.push(trip);
        Ok(())
    }

    fn remove(&mut self, key: &TripKey) -> Option<Trip> {
        let at = self.trips.iter().position(|t| &t.key() == key)?;
        Some(self.trips.remove(at))
    }

    fn contents(&self) -> Vec<u8> {
        serde_json::to_vec(&self.trips).expect("trips always serialize")
    }

    fn write(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        chimaera_core::atomic_write_private(path, contents)
    }
}

// --- the real disk and host --------------------------------------------------------

fn fingerprint(meta: &std::fs::Metadata) -> Fingerprint {
    #[cfg(unix)]
    let ino = std::os::unix::fs::MetadataExt::ino(meta);
    #[cfg(not(unix))]
    let ino = 0;
    Fingerprint {
        len: meta.len(),
        mtime_ns: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos()),
        ino,
    }
}

fn too_large() -> String {
    format!(
        "too large to send back (over {} MiB)",
        MAX_FILE_BYTES / (1024 * 1024)
    )
}

/// SHA-256 of a reader, through a fixed buffer.
fn hash_reader(reader: &mut impl Read) -> std::io::Result<(String, u64)> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex(&hasher.finalize()), total))
}

struct RealDisk;

impl Disk for RealDisk {
    fn stat(&self, path: &Path) -> Option<Fingerprint> {
        std::fs::metadata(path).ok().map(|meta| fingerprint(&meta))
    }

    fn held_open(&self, path: &Path) -> bool {
        let Some(dir) = path.parent() else {
            return false;
        };
        std::fs::read_dir(dir).is_ok_and(|entries| {
            entries
                .filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().starts_with("~$"))
        })
    }

    fn complete_hash(&self, path: &Path) -> Result<Option<String>, String> {
        let unreadable = |e: std::io::Error| format!("couldn't read PowerPoint's copy: {e}");
        // One descriptor for the check and the hash: a save renamed over the
        // path meanwhile is judged next tick, never half of each.
        let mut file = std::fs::File::open(path).map_err(unreadable)?;
        let len = file.metadata().map_err(unreadable)?.len();
        if len > MAX_FILE_BYTES {
            return Err(too_large());
        }
        let tail_len = len.min(EOCD_SEARCH);
        file.seek(SeekFrom::Start(len - tail_len))
            .map_err(unreadable)?;
        let mut tail = Vec::with_capacity(tail_len as usize);
        (&mut file)
            .take(tail_len)
            .read_to_end(&mut tail)
            .map_err(unreadable)?;
        if !zip_complete(&tail) {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(0)).map_err(unreadable)?;
        let (hash, read) =
            hash_reader(&mut (&mut file).take(MAX_FILE_BYTES + 1)).map_err(unreadable)?;
        if read != len {
            // Written to in place while it was read: next tick.
            return Ok(None);
        }
        Ok(Some(hash))
    }
}

/// A host's daemon through its live loopback endpoint (local port of the
/// tunnel + its bearer token, re-read for every tick and never stored).
struct Http {
    alias: String,
    port: u16,
    token: String,
}

/// A response's `X-Content-Hash`, when it is one.
fn content_hash(response: &ureq::http::Response<ureq::Body>) -> Option<String> {
    let value = response.headers().get("x-content-hash")?.to_str().ok()?;
    (value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

/// The daemon's `{"error": …}`, bounded, when it sent one.
fn server_message(response: &mut ureq::http::Response<ureq::Body>) -> Option<String> {
    let body: serde_json::Value = response
        .body_mut()
        .with_config()
        .limit(16 * 1024)
        .read_json()
        .ok()?;
    let message = body.get("error")?.as_str()?;
    Some(message.chars().take(200).collect())
}

impl Http {
    fn url(&self, route: &str) -> String {
        format!("http://127.0.0.1:{}{route}", self.port)
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }

    fn unreachable(&self) -> Failure {
        Failure::Unreachable(format!("couldn't reach {}: trying again", self.alias))
    }

    /// Download `remote_path` to `dest` (a fresh name in a fresh directory)
    /// through a ticket, streaming to a temp sibling under the size cap and
    /// hashing as it writes. Returns the hash of what arrived.
    fn download(&self, remote_path: &str, dest: &Path) -> Result<String, String> {
        let name = basename(remote_path);
        let mut minted = crate::http::agent()
            .post(&self.url("/api/v1/fs/ticket"))
            .header("Authorization", &self.bearer())
            .config()
            .http_status_as_error(false)
            .timeout_global(Some(API_TIMEOUT))
            .build()
            .send_json(serde_json::json!({ "path": remote_path }))
            .map_err(|_| self.unreachable().message())?;
        if minted.status().as_u16() != 200 {
            return Err(server_message(&mut minted)
                .unwrap_or_else(|| format!("{} couldn't send {name}", self.alias)));
        }
        let ticket: serde_json::Value = minted
            .body_mut()
            .with_config()
            .limit(16 * 1024)
            .read_json()
            .map_err(|_| format!("{} couldn't send {name}", self.alias))?;
        let ticket = ticket
            .get("ticket")
            .and_then(|t| t.as_str())
            .ok_or_else(|| format!("{} couldn't send {name}", self.alias))?;
        // `/download` is ticket-authorized; the bearer stays off it.
        let mut response = crate::http::agent()
            .get(&self.url(&format!("/download/{}", urlencoding::encode(ticket))))
            .config()
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(transfer_timeout(MAX_FILE_BYTES)))
            .build()
            .call()
            .map_err(|_| self.unreachable().message())?;
        match response.status().as_u16() {
            200 => {}
            404 => return Err(format!("{name} isn't on {} any more", self.alias)),
            _ => {
                return Err(server_message(&mut response)
                    .unwrap_or_else(|| format!("{} couldn't send {name}", self.alias)))
            }
        }
        // A file always streams with its length; a directory never does.
        let len = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| format!("{name} isn't a file"))?;
        if len > MAX_FILE_BYTES {
            return Err(format!(
                "{name} is too large to open (over {} MiB)",
                MAX_FILE_BYTES / (1024 * 1024)
            ));
        }
        let part = dest.with_file_name(format!(".{name}.part"));
        let failed = |e: std::io::Error| format!("couldn't save the copy: {e}");
        let result = (|| -> Result<String, String> {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&part)
                .map_err(failed)?;
            let mut out = HashingWriter::new(std::io::BufWriter::with_capacity(256 * 1024, file));
            // ureq's limit errors on the read that finds it spent, so a body
            // of exactly `len` needs one byte of slack to reach its EOF; the
            // count below still refuses anything but `len`.
            let mut body = response.body_mut().with_config().limit(len + 1).reader();
            let copied = std::io::copy(&mut body, &mut out)
                .map_err(|_| format!("the download from {} broke off", self.alias))?;
            if copied != len {
                return Err(format!("the download from {} broke off", self.alias));
            }
            let (writer, hash) = out.finish();
            let file = writer.into_inner().map_err(|e| failed(e.into_error()))?;
            file.sync_all().map_err(failed)?;
            drop(file);
            std::fs::rename(&part, dest).map_err(failed)?;
            Ok(hash)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        result
    }
}

impl Remote for Http {
    fn upload(
        &self,
        remote_path: &str,
        expect: Option<&str>,
        local: &Path,
        local_hash: &str,
    ) -> Result<Sent, Failure> {
        let file = std::fs::File::open(local)
            .map_err(|e| Failure::Refused(format!("couldn't read PowerPoint's copy: {e}")))?;
        let len = file
            .metadata()
            .map_err(|e| Failure::Refused(format!("couldn't read PowerPoint's copy: {e}")))?
            .len();
        if len > MAX_FILE_BYTES {
            return Err(Failure::Refused(too_large()));
        }
        let mut url = format!(
            "/api/v1/fs/file/stream?path={}",
            urlencoding::encode(remote_path)
        );
        if let Some(expect) = expect {
            url.push_str(&format!("&expect_hash={}", urlencoding::encode(expect)));
        }
        let mut response = crate::http::agent()
            .put(&self.url(&url))
            .header("Authorization", &self.bearer())
            .header("Content-Type", "application/octet-stream")
            .config()
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(transfer_timeout(len)))
            .build()
            .send(&file)
            .map_err(|_| self.unreachable())?;
        match response.status().as_u16() {
            200 | 204 => Ok(Sent::Saved {
                hash: content_hash(&response).unwrap_or_else(|| local_hash.to_string()),
            }),
            409 => Ok(Sent::Conflict {
                hash: content_hash(&response),
            }),
            413 => Err(Failure::Refused(format!(
                "too large to save on {} (over {} MiB)",
                self.alias,
                MAX_FILE_BYTES / (1024 * 1024)
            ))),
            status => Err(Failure::Refused(
                server_message(&mut response)
                    .unwrap_or_else(|| format!("{} refused the save ({status})", self.alias)),
            )),
        }
    }

    fn exists(&self, remote_path: &str) -> Result<bool, Failure> {
        let url = format!(
            "/api/v1/fs/file?path={}&offset=0&limit=1",
            urlencoding::encode(remote_path)
        );
        let mut response = crate::http::agent()
            .get(&self.url(&url))
            .header("Authorization", &self.bearer())
            .config()
            .http_status_as_error(false)
            .timeout_global(Some(API_TIMEOUT))
            .build()
            .call()
            .map_err(|_| self.unreachable())?;
        match response.status().as_u16() {
            200 => Ok(true),
            400 | 404 => Ok(false),
            status => Err(Failure::Refused(
                server_message(&mut response).unwrap_or_else(|| {
                    format!("{} couldn't check for a copy ({status})", self.alias)
                }),
            )),
        }
    }
}

/// A writer that hashes what it passes on.
struct HashingWriter<W> {
    inner: W,
    hasher: sha2::Sha256,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        use sha2::Digest;
        HashingWriter {
            inner,
            hasher: sha2::Sha256::new(),
        }
    }

    fn finish(self) -> (W, String) {
        use sha2::Digest;
        (self.inner, hex(&self.hasher.finalize()))
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Hand `path` to PowerPoint, else the system's app for it. Blocking.
/// Returns the app's name for the page.
#[cfg(target_os = "macos")]
fn open_in_app(path: &Path) -> Result<String, String> {
    let open = |args: &[&str]| {
        std::process::Command::new("/usr/bin/open")
            .args(args)
            .arg("--")
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    if open(&["-b", "com.microsoft.Powerpoint"]) {
        return Ok("Microsoft PowerPoint".to_string());
    }
    // PowerPoint isn't installed: whatever opens decks here (Keynote, …).
    if open(&[]) {
        return Ok("the default app".to_string());
    }
    Err("no app on this Mac opens PowerPoint decks".to_string())
}

#[cfg(not(target_os = "macos"))]
fn open_in_app(path: &Path) -> Result<String, String> {
    open::that_detached(path).map_err(|e| format!("no app opened it: {e}"))?;
    Ok("the default app".to_string())
}

// --- app state + glue ----------------------------------------------------------------

pub(crate) struct Office {
    /// Where working copies live: `<app cache>/office`.
    root: PathBuf,
    inner: Mutex<Inner>,
    /// Held across every trip operation that does file or network IO.
    io: tokio::sync::Mutex<()>,
}

struct Inner {
    store: TripStore,
    watch: HashMap<TripKey, Watch>,
    /// The watcher loop is alive (it exits when the last trip goes).
    running: bool,
}

impl Office {
    fn snapshot(&self, key: &TripKey) -> Option<(Trip, Watch)> {
        let inner = lock(&self.inner);
        let trip = inner.store.get(key)?.clone();
        let watch = inner.watch.get(key).cloned().unwrap_or_default();
        Some((trip, watch))
    }

    /// Store `trip` (keyed now by its own key, replacing the one at `old`).
    fn put(&self, app: &AppHandle, old: &TripKey, trip: Trip, watch: Watch) -> Result<(), String> {
        let mut inner = lock(&self.inner);
        inner.store.put(old, trip.clone())?;
        inner.watch.remove(old);
        inner.watch.insert(trip.key(), watch);
        if !inner.running {
            inner.running = true;
            tauri::async_runtime::spawn(watch_loop(app.clone()));
        }
        Ok(())
    }

    /// Write the trips file. Called with `io` held, so writes stay ordered.
    fn persist(&self) {
        let (path, contents) = {
            let inner = lock(&self.inner);
            (inner.store.path.clone(), inner.store.contents())
        };
        if let Err(error) = TripStore::write(&path, &contents) {
            tracing::warn!(%error, "could not save the PowerPoint round trips");
        }
    }
}

fn emit(app: &AppHandle, trip: &Trip) {
    // Remote paths go to that host's windows only; a local window could
    // hold a same-named file of its own.
    super::emit_host_scoped(app, EVENT, &trip.alias, trip.view(), None);
}

/// Load the persisted trips and resume watching them. Called once, when
/// startup first manages `Shell`.
pub(crate) fn start(app: &AppHandle) {
    // Working copies may hold saves the host hasn't received yet, so they
    // live in the data dir: macOS treats ~/Library/Caches as purgeable, and
    // an isolated dev app shares the release app's cache identifier.
    let root = chimaera_core::data_dir().join("office");
    let store = TripStore::load(chimaera_core::data_dir().join("office-trips.json"), &root);
    let resume = !store.trips.is_empty();
    let watch = store
        .trips
        .iter()
        .map(|t| (t.key(), Watch::resumed()))
        .collect();
    app.manage(Office {
        root,
        inner: Mutex::new(Inner {
            store,
            watch,
            running: resume,
        }),
        io: tokio::sync::Mutex::new(()),
    });
    if resume {
        tauri::async_runtime::spawn(watch_loop(app.clone()));
    }
}

/// The 2 s poll over every trip, alive only while a trip exists.
async fn watch_loop(app: AppHandle) {
    loop {
        tokio::time::sleep(POLL).await;
        let Some(office) = app.try_state::<Office>() else {
            return;
        };
        let _io = office.io.lock().await;
        let aliases: Vec<String> = {
            let mut inner = lock(&office.inner);
            if inner.store.trips.is_empty() {
                inner.running = false;
                return;
            }
            let mut aliases: Vec<String> =
                inner.store.trips.iter().map(|t| t.alias.clone()).collect();
            aliases.sort();
            aliases.dedup();
            aliases
        };
        let mut endpoints = HashMap::new();
        for alias in aliases {
            let endpoint = super::notices::endpoint(&app, &Some(alias.clone())).await;
            endpoints.insert(alias, endpoint);
        }
        let ticking = app.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || tick_all(&ticking, endpoints)).await;
    }
}

fn tick_all(app: &AppHandle, endpoints: HashMap<String, Option<(u16, String)>>) {
    let Some(office) = app.try_state::<Office>() else {
        return;
    };
    let keys: Vec<TripKey> = lock(&office.inner)
        .store
        .trips
        .iter()
        .map(Trip::key)
        .collect();
    let mut dirty = false;
    for key in keys {
        let Some((mut trip, mut watch)) = office.snapshot(&key) else {
            continue;
        };
        let net = endpoints
            .get(&key.0)
            .cloned()
            .flatten()
            .map(|(port, token)| Http {
                alias: key.0.clone(),
                port,
                token,
            });
        let changed = step(
            &mut trip,
            &mut watch,
            &RealDisk,
            net.as_ref().map(|n| n as &dyn Remote),
            now_ms(),
            &mut |t| emit(app, t),
        );
        if changed {
            emit(app, &trip);
            dirty = true;
        }
        if finished(&trip, &mut watch, &RealDisk) {
            remove_copies(&trip, &office.root);
            {
                let mut inner = lock(&office.inner);
                inner.store.remove(&key);
                inner.watch.remove(&key);
            }
            trip.state = TripState::Done;
            emit(app, &trip);
            dirty = true;
            continue;
        }
        let mut inner = lock(&office.inner);
        let _ = inner.store.put(&key, trip);
        inner.watch.insert(key, watch);
    }
    if dirty {
        office.persist();
    }
}

/// Delete a finished trip's working copies (only ever under the office root).
fn remove_copies(trip: &Trip, root: &Path) {
    for dir in trip
        .retired
        .iter()
        .map(PathBuf::as_path)
        .chain(trip.local_path.parent())
    {
        if dir.starts_with(root) && dir != root {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn office(app: &AppHandle) -> Result<tauri::State<'_, Office>, String> {
    app.try_state::<Office>()
        .ok_or_else(|| "the app is still starting".to_string())
}

async fn endpoint_for(app: &AppHandle, alias: &str) -> Result<Http, String> {
    super::notices::endpoint(app, &Some(alias.to_string()))
        .await
        .map(|(port, token)| Http {
            alias: alias.to_string(),
            port,
            token,
        })
        .ok_or_else(|| format!("{alias} isn't connected"))
}

/// Claim a fresh working-copy directory (never one PowerPoint may have open).
fn claim_dir(root: &Path, alias: &str, remote_path: &str) -> Result<PathBuf, String> {
    chimaera_core::ensure_private_dir(root)
        .map_err(|e| format!("couldn't make a place for the copy: {e}"))?;
    let failed = |e: std::io::Error| format!("couldn't make a place for the copy: {e}");
    std::fs::create_dir_all(root.join(alias_dir(alias))).map_err(failed)?;
    for n in 1..=MAX_DIR_SLOTS {
        let dir = dir_slot(root, alias, remote_path, n);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(failed(e)),
        }
    }
    Err("too many copies of this deck are open: press done on one".to_string())
}

/// Download the host's current `remote_path` into a fresh directory.
/// Returns the working copy, its hash and its fingerprint.
fn fetch(
    net: &Http,
    root: &Path,
    remote_path: &str,
) -> Result<(PathBuf, String, Option<Fingerprint>), String> {
    let dir = claim_dir(root, &net.alias, remote_path)?;
    let local = dir.join(basename(remote_path));
    match net.download(remote_path, &local) {
        Ok(hash) => {
            let fp = RealDisk.stat(&local);
            Ok((local, hash, fp))
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// Keep `dir` for deletion with the trip, deleting the oldest past the cap.
fn retire(trip: &mut Trip, root: &Path, dir: Option<&Path>) {
    if let Some(dir) = dir.filter(|d| d.starts_with(root) && *d != root) {
        trip.retired.push(dir.to_path_buf());
    }
    while trip.retired.len() > MAX_RETIRED {
        let oldest = trip.retired.remove(0);
        let _ = std::fs::remove_dir_all(oldest);
    }
}

/// Open `path` (on the caller's host) in PowerPoint: in place on this
/// machine, through a round trip from another host.
#[tauri::command]
pub(super) async fn office_open(
    webview: tauri::WebviewWindow,
    path: String,
) -> Result<Opened, String> {
    let app = webview.app_handle().clone();
    // Own-host: the host is the caller's own scope; `require_target` refuses
    // a window whose old document still runs mid-navigation. A local caller
    // gets the same hand-off as `reveal_in_file_manager` (its paths are this
    // machine's); a remote one only ever reaches its own host's files.
    let alias = super::caller_scope(&app, webview.label())?.alias;
    super::require_target(&app, webview.label(), alias.as_deref())?;
    let Some(alias) = alias else {
        check_deck(&path, false)?;
        let app_name = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
            let local = super::commands::local_file_path(&path)?;
            open_in_app(&local)
        })
        .await
        .map_err(|e| e.to_string())??;
        return Ok(Opened {
            app: app_name,
            trip: None,
        });
    };
    check_deck(&path, true)?;
    let office = office(&app)?;
    let _io = office.io.lock().await;
    let key = (alias.clone(), path.clone());
    let existing = office.snapshot(&key);
    let full = lock(&office.inner).store.trips.len() >= MAX_TRIPS;
    if existing.is_none() && full {
        return Err(format!(
            "{MAX_TRIPS} decks are already open in PowerPoint: press done on one first"
        ));
    }
    let net = endpoint_for(&app, &alias).await;
    let root = office.root.clone();
    let (trip, watch, app_name) =
        tauri::async_runtime::spawn_blocking(move || -> Result<(Trip, Watch, String), String> {
            if let Some((trip, watch)) = existing.clone() {
                if trip.local_path.is_file() {
                    let app_name = open_in_app(&trip.local_path)?;
                    return Ok((trip, watch, app_name));
                }
            }
            // New, or its working copy went missing: fetch the host's version.
            let net = net?;
            let (local, hash, fp) = fetch(&net, &root, &path)?;
            let mut trip = Trip::new(alias, path, local.clone(), hash.clone());
            if let Some((old, _)) = existing {
                trip.retired = old.retired;
                retire(&mut trip, &root, old.local_path.parent());
            }
            match open_in_app(&local) {
                Ok(app_name) => Ok((trip, Watch::settled_at(fp, &hash), app_name)),
                Err(e) => {
                    if let Some(dir) = local.parent() {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    Err(e)
                }
            }
        })
        .await
        .map_err(|e| e.to_string())??;
    office.put(&app, &key, trip.clone(), watch)?;
    office.persist();
    emit(&app, &trip);
    Ok(Opened {
        app: app_name,
        trip: Some(trip.view()),
    })
}

/// The caller's host's round trip for `path`, if one runs.
#[tauri::command]
pub(super) fn office_trip(
    webview: tauri::WebviewWindow,
    path: String,
) -> Result<Option<TripView>, String> {
    let app = webview.app_handle();
    // Own-host, as `office_open`: the caller's own scope, never an argument.
    let alias = super::caller_scope(app, webview.label())?.alias;
    super::require_target(app, webview.label(), alias.as_deref())?;
    let Some(alias) = alias else {
        return Ok(None);
    };
    let office = office(app)?;
    let view = lock(&office.inner)
        .store
        .get(&(alias, path))
        .map(Trip::view);
    Ok(view)
}

/// Settle the caller's host's trip for `path`: "overwrite", "copy" (answers
/// the copy's path), "theirs" or "done".
#[tauri::command]
pub(super) async fn office_trip_resolve(
    webview: tauri::WebviewWindow,
    path: String,
    choice: String,
) -> Result<Option<String>, String> {
    let app = webview.app_handle().clone();
    // Own-host, as `office_open`: the caller's own scope, never an argument.
    let alias = super::caller_scope(&app, webview.label())?.alias;
    super::require_target(&app, webview.label(), alias.as_deref())?;
    let alias = alias.ok_or_else(|| "nothing goes back and forth on this machine".to_string())?;
    if !matches!(choice.as_str(), "overwrite" | "copy" | "theirs" | "done") {
        return Err("not a choice".to_string());
    }
    let office = office(&app)?;
    let _io = office.io.lock().await;
    let key = (alias.clone(), path);
    let Some((mut trip, mut watch)) = office.snapshot(&key) else {
        return if choice == "done" {
            Ok(None)
        } else {
            Err("this deck isn't open in PowerPoint".to_string())
        };
    };
    let net = if choice == "done" {
        None
    } else {
        Some(endpoint_for(&app, &alias).await?)
    };
    let root = office.root.clone();
    let (outcome, trip, watch, forget) = tauri::async_runtime::spawn_blocking(move || {
        let now = now_ms();
        let outcome = match (choice.as_str(), net.as_ref()) {
            ("overwrite", Some(net)) => {
                overwrite(&mut trip, &mut watch, &RealDisk, net, now).map(|()| None)
            }
            ("copy", Some(net)) => save_copy(&mut trip, &mut watch, &RealDisk, net, now).map(Some),
            ("theirs", Some(net)) => {
                fetch(net, &root, &trip.remote_path).and_then(|(local, hash, fp)| {
                    let app_name = open_in_app(&local);
                    if let Err(e) = app_name {
                        if let Some(dir) = local.parent() {
                            let _ = std::fs::remove_dir_all(dir);
                        }
                        return Err(e);
                    }
                    let old = trip.local_path.clone();
                    retire(&mut trip, &root, old.parent());
                    trip.local_path = local;
                    trip.base_hash.clone_from(&hash);
                    trip.last_local_hash.clone_from(&hash);
                    trip.state = TripState::Watching;
                    trip.message = None;
                    trip.conflict_hash = None;
                    watch = Watch::settled_at(fp, &hash);
                    Ok(None)
                })
            }
            _ => may_finish(&trip, &mut watch, &RealDisk).map(|()| None),
        };
        let forget = choice == "done" && outcome.is_ok();
        if forget {
            remove_copies(&trip, &root);
        }
        (outcome, trip, watch, forget)
    })
    .await
    .map_err(|e| e.to_string())?;
    if forget {
        lock(&office.inner).store.remove(&key);
        lock(&office.inner).watch.remove(&key);
    } else {
        office.put(&app, &key, trip.clone(), watch)?;
        emit(&app, &trip);
    }
    office.persist();
    outcome
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;

    #[test]
    fn an_alias_becomes_one_safe_directory_name() {
        assert_eq!(alias_dir("host-a"), "host-a");
        assert_eq!(alias_dir("login.example_1"), "login.example_1");
        for alias in ["host-a#job1", "../etc", "a/b", ".hidden", "", "ü"] {
            let dir = alias_dir(alias);
            assert!(!dir.is_empty(), "{alias:?}");
            assert!(!dir.contains('/') && !dir.contains('\\'), "{dir}");
            assert!(!dir.starts_with('.'), "{dir}");
            assert!(
                dir.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
                "{dir}"
            );
        }
        assert!(alias_dir("host-a#job1").starts_with("host-a_job1-"));
        // Two aliases that clean to the same text stay apart.
        assert_ne!(alias_dir("a#b"), alias_dir("a_b"));
        assert_ne!(alias_dir("a#b"), alias_dir("a/b"));
        assert!(alias_dir(&"x".repeat(500)).len() < 64);

        let root = Path::new("/cache/office");
        let first = dir_slot(root, "host-a", "/p/deck.pptx", 1);
        assert_eq!(first.parent().unwrap(), root.join("host-a"));
        assert_eq!(first.file_name().unwrap().len(), 16);
        let second = dir_slot(root, "host-a", "/p/deck.pptx", 2);
        assert_eq!(
            second.file_name().unwrap().to_string_lossy(),
            format!("{}.2", first.file_name().unwrap().to_string_lossy())
        );
        assert_ne!(first, dir_slot(root, "host-a", "/p/other.pptx", 1));
    }

    #[test]
    fn local_decks_may_carry_macros_but_none_comes_from_another_machine() {
        if cfg!(windows) {
            assert!(check_deck("/d/a.pptx", false).is_err());
            return;
        }
        for ok in [
            "/d/a.pptx",
            "/d/A.PPTX",
            "/d/a.pptm",
            "/d/a.ppsx",
            "/d/b.potx",
        ] {
            assert_eq!(check_deck(ok, false), Ok(()), "{ok}");
        }
        for ok in ["/d/a.pptx", "/d/a.PpSx", "/d/a.potx"] {
            assert_eq!(check_deck(ok, true), Ok(()), "{ok}");
        }
        let macro_deck = check_deck("/d/a.PPTM", true).unwrap_err();
        assert!(macro_deck.contains("macros"), "{macro_deck}");
        for bad in [
            "relative/a.pptx",
            "/d/a.ppt",
            "/d/a.key",
            "/d/.pptx",
            "/d/pptx",
            "/d/a.pptx/",
            "/d/a\n.pptx",
        ] {
            assert!(check_deck(bad, false).is_err(), "{bad}");
            assert!(check_deck(bad, true).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_copy_goes_beside_the_original_under_a_counted_name() {
        assert_eq!(
            copy_path("/a/deck.pptx", 1).unwrap(),
            "/a/deck (PowerPoint copy).pptx"
        );
        assert_eq!(
            copy_path("/a/deck.pptx", 2).unwrap(),
            "/a/deck (PowerPoint copy 2).pptx"
        );
        assert_eq!(
            copy_path("/a/b/q3.v2.ppsx", 3).unwrap(),
            "/a/b/q3.v2 (PowerPoint copy 3).ppsx"
        );
        assert_eq!(
            copy_path("/deck.pptx", 1).unwrap(),
            "/deck (PowerPoint copy).pptx"
        );
        assert_eq!(copy_path("/a/.pptx", 1), None);
        assert_eq!(copy_path("deck.pptx", 1), None);
    }

    fn eocd(comment: &[u8]) -> Vec<u8> {
        let mut record = b"PK\x05\x06".to_vec();
        record.extend_from_slice(&[0u8; 16]);
        record.extend_from_slice(&(comment.len() as u16).to_le_bytes());
        record.extend_from_slice(comment);
        record
    }

    #[test]
    fn a_zip_is_complete_only_when_its_end_record_closes_the_file() {
        let mut file = b"PK\x03\x04 entries ...".to_vec();
        file.extend(eocd(b""));
        assert!(zip_complete(&file));
        let mut commented = b"data".to_vec();
        commented.extend(eocd(b"made by PowerPoint"));
        assert!(zip_complete(&commented));
        // Cut short, or bytes still being appended after the record.
        assert!(!zip_complete(&file[..file.len() - 1]));
        let mut trailing = file.clone();
        trailing.extend_from_slice(b"more");
        assert!(!zip_complete(&trailing));
        // A signature inside the data whose comment length doesn't fit.
        let mut stray = b"PK\x05\x06".to_vec();
        stray.extend_from_slice(&[0u8; 40]);
        assert!(!zip_complete(&stray));
        assert!(!zip_complete(b""));
        assert!(!zip_complete(b"PK\x05\x06"));
    }

    fn trip(n: usize, root: &Path) -> Trip {
        Trip::new(
            "host-a".into(),
            format!("/d/deck{n}.pptx"),
            root.join(format!("host-a/{n:016}/deck{n}.pptx")),
            "a".repeat(64),
        )
    }

    #[test]
    fn trips_round_trip_through_their_file_and_stay_capped() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-office-test-{}",
            &chimaera_core::generate_token()[..12]
        ));
        let root = dir.join("cache/office");
        let file = dir.join("office-trips.json");
        let mut store = TripStore::load(file.clone(), &root);
        assert!(store.trips.is_empty());
        let mut first = trip(0, &root);
        first.state = TripState::Uploading;
        first.conflict_hash = Some("b".repeat(64));
        first.retired = vec![root.join("host-a/old"), PathBuf::from("/elsewhere")];
        store.put(&first.key(), first.clone()).unwrap();
        for n in 1..MAX_TRIPS {
            let t = trip(n, &root);
            store.put(&t.key(), t).unwrap();
        }
        let over = trip(MAX_TRIPS, &root);
        assert!(store.put(&over.key(), over).is_err(), "the cap holds");
        // Replacing an existing trip is not an addition.
        let mut moved = trip(1, &root);
        moved.remote_path = "/d/deck1 (PowerPoint copy).pptx".into();
        store.put(&trip(1, &root).key(), moved.clone()).unwrap();
        TripStore::write(&file, &store.contents()).unwrap();

        let loaded = TripStore::load(file.clone(), &root);
        assert_eq!(loaded.trips.len(), MAX_TRIPS);
        let back = loaded.get(&first.key()).unwrap();
        assert_eq!(
            back.state,
            TripState::Watching,
            "an interrupted upload resumes as watching"
        );
        assert_eq!(back.conflict_hash, first.conflict_hash);
        assert_eq!(
            back.retired,
            vec![root.join("host-a/old")],
            "only dirs under the root"
        );
        assert!(loaded.get(&moved.key()).is_some());
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("token"), "{text}");

        // A record pointing outside the cache is dropped; an oversized file
        // is ignored whole.
        let mut stray = trip(0, &root);
        stray.local_path = PathBuf::from("/Users/someone/secret.pptx");
        let mut escape = trip(1, &root);
        escape.local_path = root.join("../../escape.pptx");
        TripStore::write(&file, &serde_json::to_vec(&vec![stray, escape]).unwrap()).unwrap();
        assert!(TripStore::load(file.clone(), &root).trips.is_empty());
        std::fs::write(&file, vec![b' '; MAX_TRIPS_FILE_BYTES as usize + 1]).unwrap();
        assert!(TripStore::load(file.clone(), &root).trips.is_empty());
        std::fs::write(&file, b"{not json").unwrap();
        assert!(TripStore::load(file, &root).trips.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    struct FakeDisk {
        fp: RefCell<Option<Fingerprint>>,
        hash: RefCell<Result<Option<String>, String>>,
        hashes: RefCell<usize>,
        open: RefCell<bool>,
    }

    impl FakeDisk {
        fn new() -> Self {
            FakeDisk {
                fp: RefCell::new(Some(fp(1))),
                hash: RefCell::new(Ok(Some(h('a')))),
                hashes: RefCell::new(0),
                open: RefCell::new(false),
            }
        }

        /// PowerPoint saved: new metadata, new contents.
        fn save(&self, n: u64, hash: char) {
            *self.fp.borrow_mut() = Some(fp(n));
            *self.hash.borrow_mut() = Ok(Some(h(hash)));
        }
    }

    impl Disk for FakeDisk {
        fn stat(&self, _: &Path) -> Option<Fingerprint> {
            *self.fp.borrow()
        }
        fn complete_hash(&self, _: &Path) -> Result<Option<String>, String> {
            *self.hashes.borrow_mut() += 1;
            self.hash.borrow().clone()
        }
        fn held_open(&self, _: &Path) -> bool {
            *self.open.borrow()
        }
    }

    #[derive(Default)]
    struct FakeHost {
        uploads: RefCell<Vec<(String, Option<String>)>>,
        answers: RefCell<VecDeque<Result<Sent, Failure>>>,
        existing: Vec<String>,
    }

    impl FakeHost {
        fn answer(&self, answer: Result<Sent, Failure>) {
            self.answers.borrow_mut().push_back(answer);
        }
    }

    impl Remote for FakeHost {
        fn upload(
            &self,
            remote_path: &str,
            expect: Option<&str>,
            _: &Path,
            local_hash: &str,
        ) -> Result<Sent, Failure> {
            self.uploads
                .borrow_mut()
                .push((remote_path.to_string(), expect.map(str::to_string)));
            self.answers
                .borrow_mut()
                .pop_front()
                .unwrap_or(Ok(Sent::Saved {
                    hash: local_hash.to_string(),
                }))
        }
        fn exists(&self, remote_path: &str) -> Result<bool, Failure> {
            Ok(self.existing.iter().any(|p| p == remote_path))
        }
    }

    fn fp(n: u64) -> Fingerprint {
        Fingerprint {
            len: 1000 + n,
            mtime_ns: n as u128,
            ino: 7,
        }
    }

    fn h(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn opened() -> (Trip, Watch) {
        let trip = Trip::new(
            "host-a".into(),
            "/d/deck.pptx".into(),
            PathBuf::from("/cache/office/host-a/x/deck.pptx"),
            h('a'),
        );
        (trip, Watch::settled_at(Some(fp(1)), &h('a')))
    }

    /// Tick with the host reachable; the states `uploading` saw.
    fn tick(
        trip: &mut Trip,
        watch: &mut Watch,
        disk: &FakeDisk,
        host: &FakeHost,
    ) -> (bool, Vec<TripState>) {
        let mut seen = Vec::new();
        let changed = step(trip, watch, disk, Some(host as &dyn Remote), 99, &mut |t| {
            seen.push(t.state)
        });
        (changed, seen)
    }

    #[test]
    fn a_save_uploads_once_it_holds_still_under_the_hosts_hash() {
        let (mut trip, mut watch) = opened();
        let disk = FakeDisk::new();
        let host = FakeHost::default();
        // Nothing changed since the download: no reads, no uploads.
        assert_eq!(tick(&mut trip, &mut watch, &disk, &host), (false, vec![]));
        assert_eq!(*disk.hashes.borrow(), 0);

        disk.save(2, 'b');
        // First sight of the change: wait for it to hold.
        assert_eq!(tick(&mut trip, &mut watch, &disk, &host), (false, vec![]));
        assert!(host.uploads.borrow().is_empty());
        // Still changing.
        disk.save(3, 'c');
        assert_eq!(tick(&mut trip, &mut watch, &disk, &host), (false, vec![]));
        // Stable across two polls: upload under the downloaded hash.
        host.answer(Ok(Sent::Saved { hash: h('c') }));
        let (changed, seen) = tick(&mut trip, &mut watch, &disk, &host);
        assert!(changed);
        assert_eq!(seen, vec![TripState::Uploading]);
        assert_eq!(
            *host.uploads.borrow(),
            vec![("/d/deck.pptx".to_string(), Some(h('a')))]
        );
        assert_eq!(trip.state, TripState::Watching);
        assert_eq!(trip.base_hash, h('c'));
        assert_eq!(trip.last_local_hash, h('c'));
        assert_eq!(trip.uploaded_ms, Some(99));
        // Quiet again.
        assert_eq!(tick(&mut trip, &mut watch, &disk, &host), (false, vec![]));
        assert_eq!(host.uploads.borrow().len(), 1);

        // A touch that leaves the bytes alone uploads nothing.
        disk.save(4, 'c');
        tick(&mut trip, &mut watch, &disk, &host);
        tick(&mut trip, &mut watch, &disk, &host);
        assert_eq!(host.uploads.borrow().len(), 1);

        // A half-written save waits for the archive to close.
        *disk.fp.borrow_mut() = Some(fp(5));
        *disk.hash.borrow_mut() = Ok(None);
        tick(&mut trip, &mut watch, &disk, &host);
        tick(&mut trip, &mut watch, &disk, &host);
        assert_eq!(host.uploads.borrow().len(), 1);
        *disk.hash.borrow_mut() = Ok(Some(h('d')));
        tick(&mut trip, &mut watch, &disk, &host);
        assert_eq!(host.uploads.borrow().len(), 2);
        assert_eq!(
            host.uploads.borrow()[1].1,
            Some(h('c')),
            "the next precondition is the upload's hash"
        );
    }

    #[test]
    fn a_conflict_holds_every_later_save_until_resolved() {
        let (mut trip, mut watch) = opened();
        let disk = FakeDisk::new();
        let host = FakeHost::default();
        disk.save(2, 'b');
        tick(&mut trip, &mut watch, &disk, &host);
        host.answer(Ok(Sent::Conflict { hash: Some(h('z')) }));
        assert!(tick(&mut trip, &mut watch, &disk, &host).0);
        assert_eq!(trip.state, TripState::Conflict);
        assert_eq!(trip.conflict_hash, Some(h('z')));
        assert_eq!(trip.base_hash, h('a'), "nothing moved the base");

        // Later saves stay here.
        for n in 3..8 {
            disk.save(n, 'c');
            assert_eq!(tick(&mut trip, &mut watch, &disk, &host), (false, vec![]));
        }
        assert_eq!(host.uploads.borrow().len(), 1);
        // A tunnel drop doesn't paper over it.
        assert!(!step(&mut trip, &mut watch, &disk, None, 1, &mut |_| {}));
        assert_eq!(trip.state, TripState::Conflict);

        // "done" is refused: the copy holds what the host lacks.
        let refused = may_finish(&trip, &mut watch, &disk).unwrap_err();
        assert!(refused.contains("as a copy"), "{refused}");

        // "overwrite" goes under the hash the 409 reported.
        host.answer(Ok(Sent::Conflict { hash: Some(h('y')) }));
        assert!(overwrite(&mut trip, &mut watch, &disk, &host, 5).is_err());
        assert_eq!(trip.state, TripState::Conflict);
        assert_eq!(trip.conflict_hash, Some(h('y')), "it changed again");
        overwrite(&mut trip, &mut watch, &disk, &host, 6).unwrap();
        assert_eq!(
            host.uploads.borrow()[1..],
            [
                ("/d/deck.pptx".to_string(), Some(h('z'))),
                ("/d/deck.pptx".to_string(), Some(h('y'))),
            ]
        );
        assert_eq!(trip.state, TripState::Watching);
        assert_eq!(
            (trip.base_hash.clone(), trip.uploaded_ms),
            (h('c'), Some(6))
        );
        assert_eq!(may_finish(&trip, &mut watch, &disk), Ok(()));
        // And the watcher doesn't send it a second time.
        tick(&mut trip, &mut watch, &disk, &host);
        tick(&mut trip, &mut watch, &disk, &host);
        assert_eq!(host.uploads.borrow().len(), 3);
    }

    #[test]
    fn overwrite_needs_the_hosts_hash_and_a_copy_takes_a_free_name() {
        let (mut trip, mut watch) = opened();
        let disk = FakeDisk::new();
        disk.save(2, 'b');
        let host = FakeHost {
            existing: vec!["/d/deck (PowerPoint copy).pptx".into()],
            ..FakeHost::default()
        };
        assert!(
            overwrite(&mut trip, &mut watch, &disk, &host, 1).is_err(),
            "no conflict"
        );
        // The host's file vanished: a 409 without a hash.
        trip.state = TripState::Conflict;
        trip.conflict_hash = None;
        let gone = overwrite(&mut trip, &mut watch, &disk, &host, 1).unwrap_err();
        assert!(gone.contains("gone"), "{gone}");
        assert!(host.uploads.borrow().is_empty());

        let copy = save_copy(&mut trip, &mut watch, &disk, &host, 7).unwrap();
        assert_eq!(copy, "/d/deck (PowerPoint copy 2).pptx");
        assert_eq!(*host.uploads.borrow(), vec![(copy.clone(), None)]);
        assert_eq!(trip.remote_path, copy, "later saves go to the copy");
        assert_eq!(trip.state, TripState::Watching);
        assert_eq!(trip.base_hash, h('b'));
        assert_eq!(trip.conflict_hash, None);
    }

    #[test]
    fn closing_the_deck_in_powerpoint_ends_a_trip_once_everything_is_sent() {
        let (mut trip, mut watch) = opened();
        let disk = FakeDisk::new();
        let host = FakeHost::default();
        // Not seen open yet (PowerPoint still starting): never ends.
        for _ in 0..5 {
            tick(&mut trip, &mut watch, &disk, &host);
        }
        assert!(!finished(&trip, &mut watch, &disk));
        // Open, then a save that isn't on the host yet, then closed.
        *disk.open.borrow_mut() = true;
        tick(&mut trip, &mut watch, &disk, &host);
        disk.save(2, 'b');
        *disk.open.borrow_mut() = false;
        tick(&mut trip, &mut watch, &disk, &host);
        assert!(
            !finished(&trip, &mut watch, &disk),
            "an unsent save holds it"
        );
        // The save goes up; after a few closed polls the trip is over.
        tick(&mut trip, &mut watch, &disk, &host);
        assert_eq!(host.uploads.borrow().len(), 1);
        for _ in 0..CLOSED_POLLS {
            tick(&mut trip, &mut watch, &disk, &host);
        }
        assert!(finished(&trip, &mut watch, &disk));
        // A conflict never ends on its own.
        trip.state = TripState::Conflict;
        assert!(!finished(&trip, &mut watch, &disk));
    }

    #[test]
    fn a_resumed_trip_ends_when_powerpoint_has_closed_it() {
        let (trip, _) = opened();
        let mut watch = Watch::resumed();
        let disk = FakeDisk::new();
        let host = FakeHost::default();
        let mut trip = trip;
        for _ in 0..=CLOSED_POLLS {
            tick(&mut trip, &mut watch, &disk, &host);
        }
        assert!(finished(&trip, &mut watch, &disk));
    }

    #[test]
    fn done_is_refused_while_a_save_hasnt_reached_the_host() {
        let (trip, mut watch) = opened();
        let disk = FakeDisk::new();
        assert_eq!(may_finish(&trip, &mut watch, &disk), Ok(()));
        disk.save(2, 'b');
        let refused = may_finish(&trip, &mut watch, &disk).unwrap_err();
        assert!(refused.contains("hasn't reached host-a"), "{refused}");
        // Mid-save counts as unsent.
        *disk.hash.borrow_mut() = Ok(None);
        *disk.fp.borrow_mut() = Some(fp(3));
        assert!(may_finish(&trip, &mut watch, &disk).is_err());
        // A working copy that's gone holds nothing.
        *disk.fp.borrow_mut() = None;
        assert_eq!(may_finish(&trip, &mut watch, &disk), Ok(()));
    }

    #[test]
    fn a_host_out_of_reach_waits_and_a_failed_send_retries() {
        let (mut trip, mut watch) = opened();
        let disk = FakeDisk::new();
        let host = FakeHost::default();
        disk.save(2, 'b');
        // Tunnel down: say so once, keep the change.
        assert!(step(&mut trip, &mut watch, &disk, None, 1, &mut |_| {}));
        assert_eq!(trip.state, TripState::Error);
        assert_eq!(trip.message.as_deref(), Some("waiting for host-a"));
        assert!(!step(&mut trip, &mut watch, &disk, None, 1, &mut |_| {}));
        // Back: watching, then the held change goes once it holds still.
        assert!(tick(&mut trip, &mut watch, &disk, &host).0);
        assert_eq!(trip.state, TripState::Watching);
        host.answer(Err(Failure::Unreachable("couldn't reach host-a".into())));
        tick(&mut trip, &mut watch, &disk, &host);
        assert_eq!(host.uploads.borrow().len(), 1);
        assert_eq!(trip.state, TripState::Error);
        // Transport failure: the next tick tries again without a new save
        // or a second read of the file.
        let reads = *disk.hashes.borrow();
        tick(&mut trip, &mut watch, &disk, &host);
        assert_eq!(host.uploads.borrow().len(), 2);
        assert_eq!(*disk.hashes.borrow(), reads);
        assert_eq!(trip.state, TripState::Watching);

        // A refusal waits for the next change instead.
        disk.save(3, 'c');
        tick(&mut trip, &mut watch, &disk, &host);
        host.answer(Err(Failure::Refused("no".into())));
        tick(&mut trip, &mut watch, &disk, &host);
        assert_eq!(trip.message.as_deref(), Some("no"));
        tick(&mut trip, &mut watch, &disk, &host);
        assert_eq!(host.uploads.borrow().len(), 3);
        disk.save(4, 'd');
        tick(&mut trip, &mut watch, &disk, &host);
        tick(&mut trip, &mut watch, &disk, &host);
        assert_eq!(host.uploads.borrow().len(), 4);
    }

    #[test]
    fn a_resumed_trip_sends_what_was_saved_while_the_app_was_closed() {
        let (mut trip, _) = opened();
        let mut watch = Watch::default();
        let disk = FakeDisk::new();
        let host = FakeHost::default();
        // Unchanged while closed: one read, nothing sent.
        tick(&mut trip, &mut watch, &disk, &host);
        tick(&mut trip, &mut watch, &disk, &host);
        assert!(host.uploads.borrow().is_empty());
        let (mut trip, _) = opened();
        let mut watch = Watch::default();
        disk.save(2, 'b');
        tick(&mut trip, &mut watch, &disk, &host);
        tick(&mut trip, &mut watch, &disk, &host);
        assert_eq!(host.uploads.borrow().len(), 1);
    }

    #[test]
    fn the_wire_shape_is_the_pages_office_trip() {
        let (mut trip, _) = opened();
        trip.message = Some("waiting for host-a".into());
        trip.state = TripState::Error;
        let value = serde_json::to_value(trip.view()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "path": "/d/deck.pptx",
                "state": "error",
                "uploaded_ms": null,
                "message": "waiting for host-a",
            })
        );
    }
}
