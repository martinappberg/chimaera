//! Bounded, client-scoped filesystem change detection for mounted file views.
//!
//! A recursive watcher is the wrong primitive for Chimaera: workspace roots can
//! be enormous, and inotify/FSEvents do not reliably report remote writes on
//! NFS/Lustre. Instead each `/ws/events` client registers only the file paths it
//! has mounted and the directories it is visibly listing. We stat those exact
//! paths every two seconds and occasionally hash directory entry names as a
//! metadata-cache backstop. All filesystem work runs off the async reactor and
//! every dimension is capped.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

const FAST_POLL_INTERVAL: Duration = Duration::from_secs(2);
const LISTING_POLL_INTERVAL: Duration = Duration::from_secs(12);
/// Newly registered directories get a bounded initial name/type baseline. A
/// registration burst must not turn 64 large directories into one hot scan.
const MAX_BASELINE_DIRS_PER_POLL: usize = 4;
pub(crate) const MAX_WATCH_FILES: usize = 64;
pub(crate) const MAX_WATCH_DIRS: usize = 64;
const MAX_WATCH_PATH_BYTES: usize = 4096;
const MAX_WATCH_TOTAL_BYTES: usize = 64 * 1024;
/// Mirrors `fs::MAX_DIR_ENTRIES`: the monitor must never walk farther than the
/// listing surface it invalidates.
const MAX_LISTING_ENTRIES: usize = crate::fs::MAX_DIR_ENTRIES;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FsChanges {
    pub(crate) files: Vec<String>,
    pub(crate) removed: Vec<String>,
    pub(crate) dirs: Vec<String>,
    pub(crate) removed_dirs: Vec<String>,
}

impl FsChanges {
    pub(crate) fn is_empty(&self) -> bool {
        self.files.is_empty()
            && self.removed.is_empty()
            && self.dirs.is_empty()
            && self.removed_dirs.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WatchPath {
    wire: String,
    disk: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataFingerprint {
    Missing,
    Present {
        modified_ns: u128,
        len: u64,
        kind: u8,
        identity: u128,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ListingFingerprint {
    count: usize,
    xor: u64,
    sum: u64,
    product_sum: u64,
    truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirFingerprint {
    metadata: MetadataFingerprint,
    listing: Option<ListingFingerprint>,
}

/// Outer `None`: not scanned. Inner `None`: scan failed transiently.
type ListingObservation = Option<Option<ListingFingerprint>>;

struct Observations {
    files: Vec<(String, Option<MetadataFingerprint>)>,
    dirs: Vec<(String, Option<MetadataFingerprint>, ListingObservation)>,
}

/// One events socket's watched-path state. Nothing survives disconnect.
pub(crate) struct FsWatch {
    files: Vec<WatchPath>,
    dirs: Vec<WatchPath>,
    file_state: HashMap<String, MetadataFingerprint>,
    dir_state: HashMap<String, DirFingerprint>,
    pending_listing_dirs: HashSet<String>,
    last_fast_poll: Option<Instant>,
    last_listing_poll: Option<Instant>,
    last_baseline_poll: Option<Instant>,
}

impl FsWatch {
    pub(crate) fn new() -> Self {
        Self {
            files: Vec::new(),
            dirs: Vec::new(),
            file_state: HashMap::new(),
            dir_state: HashMap::new(),
            pending_listing_dirs: HashSet::new(),
            last_fast_poll: None,
            last_listing_poll: None,
            last_baseline_poll: None,
        }
    }

    /// Replace this connection's registrations. Unchanged baselines survive so
    /// opening one more tab does not make every existing view refresh.
    pub(crate) fn set(&mut self, files: Vec<String>, dirs: Vec<String>) -> bool {
        let mut total_bytes = 0usize;
        let next_files = bounded_paths(
            files,
            MAX_WATCH_FILES,
            &mut total_bytes,
            MAX_WATCH_TOTAL_BYTES,
        );
        let next_dirs = bounded_paths(
            dirs,
            MAX_WATCH_DIRS,
            &mut total_bytes,
            MAX_WATCH_TOTAL_BYTES,
        );
        if self.files == next_files && self.dirs == next_dirs {
            return false;
        }

        let file_names: HashSet<&str> = next_files.iter().map(|p| p.wire.as_str()).collect();
        let dir_names: HashSet<&str> = next_dirs.iter().map(|p| p.wire.as_str()).collect();
        self.file_state
            .retain(|path, _| file_names.contains(path.as_str()));
        self.dir_state
            .retain(|path, _| dir_names.contains(path.as_str()));
        self.pending_listing_dirs
            .retain(|path| dir_names.contains(path.as_str()));
        for path in &next_dirs {
            if !self.dir_state.contains_key(&path.wire) {
                self.pending_listing_dirs.insert(path.wire.clone());
            }
        }
        self.files = next_files;
        self.dirs = next_dirs;
        true
    }

    /// Poll when due. `force_metadata` exists for tests; production registration
    /// updates remain on the two-second ceiling so a client cannot create a hot
    /// stat/read_dir loop by alternating watch frames.
    pub(crate) async fn poll(&mut self, force_metadata: bool) -> FsChanges {
        let now = Instant::now();
        if !force_metadata
            && self
                .last_fast_poll
                .is_some_and(|last| now.duration_since(last) < FAST_POLL_INTERVAL)
        {
            return FsChanges::default();
        }
        self.last_fast_poll = Some(now);

        // Full backstop scans stay on their 12-second cadence. Newly mounted
        // dirs are baselined in small two-second batches; file-only watch churn
        // therefore performs no directory walk at all.
        let periodic_listing = self
            .last_listing_poll
            .is_some_and(|last| now.duration_since(last) >= LISTING_POLL_INTERVAL);
        if self.last_listing_poll.is_none() {
            self.last_listing_poll = Some(now);
        }
        let baseline_listing = !self.pending_listing_dirs.is_empty()
            && self
                .last_baseline_poll
                .is_none_or(|last| now.duration_since(last) >= FAST_POLL_INTERVAL);
        let listing_dirs: HashSet<String> = if periodic_listing {
            self.last_listing_poll = Some(now);
            self.pending_listing_dirs.clear();
            self.dirs.iter().map(|path| path.wire.clone()).collect()
        } else if baseline_listing {
            self.last_baseline_poll = Some(now);
            let targets: Vec<String> = self
                .dirs
                .iter()
                .filter(|path| self.pending_listing_dirs.contains(&path.wire))
                .take(MAX_BASELINE_DIRS_PER_POLL)
                .map(|path| path.wire.clone())
                .collect();
            for path in &targets {
                self.pending_listing_dirs.remove(path);
            }
            targets.into_iter().collect()
        } else {
            HashSet::new()
        };

        let files = self.files.clone();
        let dirs = self.dirs.clone();
        let observed =
            match tokio::task::spawn_blocking(move || observe(files, dirs, listing_dirs)).await {
                Ok(value) => value,
                Err(join) => {
                    tracing::debug!(%join, "filesystem watch task failed");
                    return FsChanges::default();
                }
            };

        self.apply(observed)
    }

    fn apply(&mut self, observed: Observations) -> FsChanges {
        let mut changes = FsChanges::default();
        for (path, next) in observed.files {
            // Permission/transient I/O errors are not deletions. Preserve the
            // last good baseline and try again on the next tick.
            let Some(next) = next else { continue };
            let changed = self.file_state.get(&path) != Some(&next);
            self.file_state.insert(path.clone(), next);
            if changed {
                match next {
                    MetadataFingerprint::Missing => changes.removed.push(path),
                    MetadataFingerprint::Present { .. } => changes.files.push(path),
                }
            }
        }

        for (path, metadata, listing_observation) in observed.dirs {
            let Some(metadata) = metadata else { continue };
            let previous = self.dir_state.get(&path).copied();
            let mut changed = previous.is_none_or(|old| old.metadata != metadata);
            let next_listing = if let Some(listing) = listing_observation {
                if let (Some(old), Some(next)) = (previous.and_then(|p| p.listing), listing) {
                    changed |= old != next;
                }
                listing.or_else(|| previous.and_then(|p| p.listing))
            } else if previous.is_some_and(|old| old.metadata != metadata) {
                // A fast metadata change already announced the directory. Drop
                // the old name hash so the next slow scan adopts a fresh
                // baseline instead of announcing the same mutation twice.
                None
            } else {
                previous.and_then(|p| p.listing)
            };
            self.dir_state.insert(
                path.clone(),
                DirFingerprint {
                    metadata,
                    listing: next_listing,
                },
            );
            if changed {
                match metadata {
                    MetadataFingerprint::Missing => changes.removed_dirs.push(path),
                    MetadataFingerprint::Present { kind: 2, .. } => changes.dirs.push(path),
                    // A watched directory replaced by a file is gone as a
                    // listing target, even though metadata(path) succeeds.
                    MetadataFingerprint::Present { .. } => changes.removed_dirs.push(path),
                }
            }
        }
        changes
    }
}

fn bounded_paths(
    paths: Vec<String>,
    count_cap: usize,
    total_bytes: &mut usize,
    total_cap: usize,
) -> Vec<WatchPath> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for wire in paths {
        if out.len() >= count_cap
            || wire.is_empty()
            || wire.len() > MAX_WATCH_PATH_BYTES
            || !seen.insert(wire.clone())
        {
            continue;
        }
        let Some(next_total) = total_bytes.checked_add(wire.len()) else {
            break;
        };
        if next_total > total_cap {
            break;
        }
        *total_bytes = next_total;
        out.push(WatchPath {
            disk: expand_tilde(&wire),
            wire,
        });
    }
    out
}

fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(raw)
}

fn observe(
    files: Vec<WatchPath>,
    dirs: Vec<WatchPath>,
    listing_dirs: HashSet<String>,
) -> Observations {
    Observations {
        files: files
            .into_iter()
            .map(|path| (path.wire, metadata_fingerprint(&path.disk)))
            .collect(),
        dirs: dirs
            .into_iter()
            .map(|path| {
                let metadata = metadata_fingerprint(&path.disk);
                let listing = listing_dirs
                    .contains(&path.wire)
                    .then(|| listing_fingerprint(&path.disk));
                (path.wire, metadata, listing)
            })
            .collect(),
    }
}

/// `None` means unreadable/transient (do not turn it into a deletion).
fn metadata_fingerprint(path: &Path) -> Option<MetadataFingerprint> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let kind = if meta.is_file() {
                1
            } else if meta.is_dir() {
                2
            } else {
                3
            };
            let modified_ns = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_nanos());
            Some(MetadataFingerprint::Present {
                modified_ns,
                len: meta.len(),
                kind,
                identity: metadata_identity(&meta),
            })
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Some(MetadataFingerprint::Missing)
        }
        Err(_) => None,
    }
}

#[cfg(unix)]
fn metadata_identity(meta: &std::fs::Metadata) -> u128 {
    use std::os::unix::fs::MetadataExt;
    // ctime catches in-place rewrites whose mtime was deliberately restored;
    // dev+ino catches atomic replacement. This is process-local comparison,
    // not a persisted/wire identifier, so a compact mixing function is enough.
    let inode = ((meta.dev() as u128) << 64) | meta.ino() as u128;
    let ctime = ((meta.ctime() as i128 as u128) << 32) ^ meta.ctime_nsec() as i128 as u128;
    inode.rotate_left(29) ^ ctime
}

#[cfg(not(unix))]
fn metadata_identity(_meta: &std::fs::Metadata) -> u128 {
    0
}

/// Order-independent, bounded signature of directory entry names + d_type.
/// This catches remote entry changes even when an NFS metadata cache leaves the
/// directory mtime unchanged. It never fetches per-entry size or mtime.
fn listing_fingerprint(path: &Path) -> Option<ListingFingerprint> {
    let read = std::fs::read_dir(path).ok()?;
    let mut count = 0usize;
    let mut xor = 0u64;
    let mut sum = 0u64;
    let mut product_sum = 0u64;
    let mut truncated = false;
    for entry in read {
        let Ok(entry) = entry else { continue };
        if count >= MAX_LISTING_ENTRIES {
            truncated = true;
            break;
        }
        let kind = entry.file_type().map_or(0, |ft| {
            if ft.is_file() {
                1
            } else if ft.is_dir() {
                2
            } else if ft.is_symlink() {
                3
            } else {
                4
            }
        });
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        entry.file_name().hash(&mut hasher);
        kind.hash(&mut hasher);
        let hash = hasher.finish();
        xor ^= hash;
        sum = sum.wrapping_add(hash);
        product_sum = product_sum.wrapping_add(hash.wrapping_mul(hash | 1));
        count += 1;
    }
    Some(ListingFingerprint {
        count,
        xor,
        sum,
        product_sum,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "chimaera-fs-watch-{label}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn mounted_files_and_visible_dirs_report_repeated_external_changes() {
        let root = TempDir::new("changes");
        let file = root.0.join("already-dirty.txt");
        std::fs::write(&file, b"first").unwrap();

        let mut watch = FsWatch::new();
        watch.set(
            vec![file.to_string_lossy().into_owned()],
            vec![root.0.to_string_lossy().into_owned()],
        );
        let initial = watch.poll(true).await;
        assert_eq!(initial.files, [file.to_string_lossy()]);
        assert_eq!(initial.dirs, [root.0.to_string_lossy()]);

        // The path was already changed before this second write. Unlike git
        // porcelain status, the metadata fingerprint still moves.
        std::fs::write(&file, b"second-and-longer").unwrap();
        let child = root.0.join("new-ignored-output.bin");
        std::fs::write(&child, b"x").unwrap();
        let changed = watch.poll(true).await;
        assert_eq!(changed.files, [file.to_string_lossy()]);
        assert_eq!(changed.dirs, [root.0.to_string_lossy()]);

        std::fs::remove_file(&file).unwrap();
        let removed = watch.poll(true).await;
        assert_eq!(removed.removed, [file.to_string_lossy()]);
    }

    #[tokio::test]
    async fn registration_is_deduplicated_and_hard_capped() {
        let mut watch = FsWatch::new();
        let files = (0..MAX_WATCH_FILES + 20)
            .flat_map(|i| [format!("/tmp/f{i}"), format!("/tmp/f{i}")])
            .collect();
        let dirs = (0..MAX_WATCH_DIRS + 20)
            .map(|i| format!("/tmp/d{i}"))
            .collect();
        watch.set(files, dirs);
        assert_eq!(watch.files.len(), MAX_WATCH_FILES);
        assert_eq!(watch.dirs.len(), MAX_WATCH_DIRS);
    }

    #[tokio::test]
    async fn registration_churn_does_not_rescan_every_directory() {
        let dirs: Vec<String> = (0..10)
            .map(|i| format!("/tmp/chimaera-watch-d{i}"))
            .collect();
        let mut watch = FsWatch::new();
        watch.set(Vec::new(), dirs.clone());

        // One immediate poll consumes only the bounded baseline batch.
        let _ = watch.poll(true).await;
        assert_eq!(
            watch.pending_listing_dirs.len(),
            10 - MAX_BASELINE_DIRS_PER_POLL
        );
        let last_full_scan = watch.last_listing_poll;

        // Adding only a file within the same interval may stat metadata for the
        // test, but it must not consume or rescan directory baselines.
        watch.set(vec!["/tmp/chimaera-watch-file".into()], dirs);
        let _ = watch.poll(true).await;
        assert_eq!(
            watch.pending_listing_dirs.len(),
            10 - MAX_BASELINE_DIRS_PER_POLL
        );
        assert_eq!(watch.last_listing_poll, last_full_scan);
    }
}
