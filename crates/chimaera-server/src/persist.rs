//! Atomic persistence for the daemon's small JSON state stores (view-state,
//! ledger, workspaces, recents, settings). Each writes its whole file at once
//! via a temp sibling + rename, so a crash mid-write never leaves a torn file.
//! Serialization (compact vs pretty) and any bookkeeping (a generation bump, a
//! `written_at` stamp) stay at the call site; this owns only the shared
//! create-dir → write-tmp → rename dance.

use std::path::Path;

use anyhow::Context;

/// Write `contents` to `path` atomically: ensure the parent dir exists, write
/// a `.json.tmp` sibling, then rename it over `path`. The stores all target
/// `*.json`, so the tmp name mirrors the historical `with_extension("json.tmp")`.
pub(crate) fn atomic_write_json(path: &Path, contents: impl AsRef<[u8]>) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, contents).with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename into {}", path.display()))?;
    Ok(())
}
