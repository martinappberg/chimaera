//! Atomic persistence for the daemon's small JSON state stores (view-state,
//! ledger, workspaces, recents, settings). Each writes its whole file at once
//! via a temp sibling + rename, so a crash mid-write never leaves a torn file.
//! Serialization (compact vs pretty) and any bookkeeping (a generation bump, a
//! `written_at` stamp) stay at the call site; this owns only the shared
//! create-dir → write-tmp → rename dance, the bounded read every loader uses,
//! and [`DiskGate`], the per-store blocking-job gate.

use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Context;

/// Write `contents` to `path` atomically: ensure the parent dir exists, write
/// a `.json.tmp` sibling, then rename it over `path`. The stores all target
/// `*.json`, so the tmp name mirrors the historical `with_extension("json.tmp")`.
pub(crate) fn atomic_write_json(path: &Path, contents: impl AsRef<[u8]>) -> anyhow::Result<()> {
    atomic_write_json_modified(path, contents).map(|_| ())
}

/// [`atomic_write_json`], answering the written file's mtime. It is read from
/// the temp file's own descriptor before the rename, so an edit that lands
/// right after the rename can never be recorded as this write's mtime (a
/// store that recorded it would miss that edit forever).
pub(crate) fn atomic_write_json_modified(
    path: &Path,
    contents: impl AsRef<[u8]>,
) -> anyhow::Result<Option<SystemTime>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let modified = write_tmp(&tmp, contents.as_ref())
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename into {}", path.display()))?;
    Ok(modified)
}

fn write_tmp(tmp: &Path, contents: &[u8]) -> std::io::Result<Option<SystemTime>> {
    let mut file = std::fs::File::create(tmp)?;
    file.write_all(contents)?;
    Ok(file.metadata()?.modified().ok())
}

/// What [`read_bounded`] found.
#[derive(Debug)]
pub(crate) enum Bounded {
    /// No file at the path.
    Missing,
    /// More than the cap: the file was never read past `max_bytes + 1`.
    TooLarge,
    /// The whole file, at most `max_bytes`.
    Ok(Vec<u8>),
    /// Unreadable (permissions, a directory or FIFO at the path, an I/O
    /// error mid-read) — distinct from Missing so a store can keep what it
    /// last read instead of treating a hiccup as "the user cleared it".
    Io(std::io::Error),
}

/// Read a small state file without ever holding more than `max_bytes + 1`
/// bytes: the cap is enforced by the read itself (`Read::take`), never by a
/// stat alone, because a file can grow between a stat and the read. The open
/// is non-blocking and regular-files-only (`fs::open_regular`), so a FIFO
/// planted at the path can't park the blocking thread. Blocking: call it off
/// the reactor.
pub(crate) fn read_bounded(path: &Path, max_bytes: usize) -> Bounded {
    let (file, meta) = match crate::fs::open_regular(path) {
        Ok(opened) => opened,
        Err(err) if err.kind() == ErrorKind::NotFound => return Bounded::Missing,
        Err(err) => return Bounded::Io(err),
    };
    let limit = max_bytes as u64 + 1;
    // The stat only sizes the buffer; the take() below is the bound.
    let mut bytes = Vec::with_capacity(meta.len().min(limit) as usize);
    if let Err(err) = file.take(limit).read_to_end(&mut bytes) {
        return Bounded::Io(err);
    }
    if bytes.len() > max_bytes {
        return Bounded::TooLarge;
    }
    Bounded::Ok(bytes)
}

/// One disk job at a time for one store, on the blocking pool.
///
/// A single slot rather than a wider semaphore: a store's jobs must apply in
/// order anyway (a refresh must not land after a later write), and one slot
/// means a hung NFS mount pins at most one blocking thread per store while
/// every other caller waits asynchronously. The guard moves INTO the blocking
/// job, so a caller that goes away mid-job (a closed window drops the
/// handler) cannot release the slot while its job still runs — a started
/// blocking task can't be aborted, so that would let jobs pile up.
///
/// Lock order: this gate, then the store's `std::sync::Mutex`. Inside a job
/// the mutex is taken only briefly — never across the job's I/O, never
/// across an `.await`. (A store's synchronous getters, which bypass the
/// gate, still stat and read under the mutex.)
#[derive(Clone, Default)]
pub(crate) struct DiskGate(Arc<tokio::sync::Mutex<()>>);

impl DiskGate {
    /// Run `job` on the blocking pool once this store's earlier jobs are done.
    pub(crate) async fn run<T, F>(&self, job: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let slot = self.0.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            job()
        })
        .await
        .context("store disk job failed")
    }
}

/// A test's stand-in for slow or changing storage: a store calls it on the
/// blocking pool at a fixed point of a job (before a read, after a write).
/// Release builds carry no hook: the stores' `Option<Hook>` fields are then
/// `Option<()>`, and [`run_hook`] does nothing.
#[cfg(test)]
pub(crate) type Hook = Arc<dyn Fn() + Send + Sync>;
/// Clone, not Copy, so a store copies its hook the same way in both builds.
#[cfg(not(test))]
#[derive(Clone)]
pub(crate) struct Hook;

#[cfg(test)]
pub(crate) fn run_hook(hook: Option<&Hook>) {
    if let Some(hook) = hook {
        hook();
    }
}
#[cfg(not(test))]
pub(crate) fn run_hook(_: Option<&Hook>) {}

/// Why a store's file body wasn't produced.
pub(crate) enum Encode {
    /// Pretty-printed, the body would exceed the store's read cap.
    TooLarge,
    Failed(serde_json::Error),
}

/// The pretty-printed body of a small JSON state file, newline-terminated,
/// refused over `max_bytes`: a write must never produce a file the store's
/// own bounded read would then ignore.
pub(crate) fn encode_pretty<T: serde::Serialize>(
    value: &T,
    max_bytes: usize,
) -> Result<Vec<u8>, Encode> {
    let mut body = serde_json::to_vec_pretty(value).map_err(Encode::Failed)?;
    body.push(b'\n');
    if body.len() > max_bytes {
        return Err(Encode::TooLarge);
    }
    Ok(body)
}

/// One look at a capped store file (settings, env profiles), taken without
/// the store's mutex.
pub(crate) enum Look<T> {
    /// The mtime still matches what the store last read.
    Same,
    /// The file's content and the mtime stat'ed just before reading it —
    /// `T::default()` for a missing or oversized file, and whatever `parse`
    /// makes of a corrupt one (the stores' never-brick rule).
    Loaded(T, Option<SystemTime>),
    /// The stat or read failed (an NFS hiccup, a directory at the path): the
    /// store keeps its cache and records no mtime, so the next look retries.
    /// Reloading through a failing filesystem would hand every reader an
    /// empty store; a read-modify-write refuses rather than overwrite it.
    Unreadable,
}

/// Stat `path` and read it (bounded) only when its mtime moved from
/// `recorded`. Blocking. The recorded mtime is the stat BEFORE the read, so
/// an edit landing mid-read moves the mtime past it and the next look picks
/// it up instead of missing it. `name` is the file name for the log lines.
pub(crate) fn look<T: Default>(
    path: &Path,
    recorded: Option<SystemTime>,
    max_bytes: usize,
    name: &str,
    hook: Option<&Hook>,
    parse: impl FnOnce(&[u8]) -> T,
) -> Look<T> {
    let on_disk = match std::fs::metadata(path) {
        Ok(meta) => meta.modified().ok(),
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(err) => {
            tracing::debug!(path = %path.display(), %err, "{name} stat failed; keeping the cache");
            return Look::Unreadable;
        }
    };
    if on_disk == recorded {
        return Look::Same;
    }
    run_hook(hook);
    let data = match read_bounded(path, max_bytes) {
        Bounded::Missing => return Look::Loaded(T::default(), None),
        Bounded::TooLarge => {
            tracing::warn!(path = %path.display(), "{name} exceeds {max_bytes} bytes; ignoring");
            T::default()
        }
        Bounded::Io(err) => {
            tracing::warn!(path = %path.display(), %err, "failed to read {name}; keeping the cache");
            return Look::Unreadable;
        }
        Bounded::Ok(bytes) => parse(&bytes),
    };
    Look::Loaded(data, on_disk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_bounded_outcomes() {
        let dir = std::env::temp_dir().join(format!("chimaera-bounded-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("state.json");
        assert!(matches!(read_bounded(&file, 8), Bounded::Missing));
        std::fs::write(&file, b"12345678").unwrap();
        assert!(matches!(read_bounded(&file, 8), Bounded::Ok(b) if b == b"12345678"));
        std::fs::write(&file, b"123456789").unwrap();
        assert!(matches!(read_bounded(&file, 8), Bounded::TooLarge));
        assert!(matches!(read_bounded(&dir, 8), Bounded::Io(_)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
