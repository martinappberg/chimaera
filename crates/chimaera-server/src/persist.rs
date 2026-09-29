//! Atomic persistence for the daemon's small JSON state stores (view-state,
//! ledger, workspaces, recents, settings). Each writes its whole file at once
//! via a temp sibling + rename, so a crash mid-write never leaves a torn file.
//! Serialization (compact vs pretty) and any bookkeeping (a generation bump, a
//! `written_at` stamp) stay at the call site; this owns only the shared
//! create-dir → write-tmp → rename dance.

use std::path::Path;

use anyhow::Context;

/// Reserved siblings are incomplete daemon writes, never project snapshot data.
pub(crate) const PROJECT_STAGING_PREFIX: &str = ".chimaera-staging-";

/// Keep atomic renames on the destination filesystem without putting partial
/// bytes into a mirror. Preserve UTF-8 boundaries and the usual NAME_MAX ceiling.
pub(crate) fn project_temp_name(name: &std::ffi::OsStr) -> std::ffi::OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let nonce = &chimaera_core::generate_token()[..32];
    let budget = 255 - (PROJECT_STAGING_PREFIX.len() + 1 + nonce.len() + ".tmp".len());
    let bytes = name.as_bytes();
    let mut cut = bytes.len().min(budget);
    while cut > 0 && cut < bytes.len() && (bytes[cut] & 0b1100_0000) == 0b1000_0000 {
        cut -= 1;
    }
    let mut out = Vec::with_capacity(255);
    out.extend_from_slice(PROJECT_STAGING_PREFIX.as_bytes());
    out.extend_from_slice(&bytes[..cut]);
    out.push(b'.');
    out.extend_from_slice(nonce.as_bytes());
    out.extend_from_slice(b".tmp");
    std::ffi::OsString::from_vec(out)
}

/// Like [`atomic_write_json`], but the bytes and the rename reach stable
/// storage before returning (F_FULLFSYNC on macOS, where fsync alone may stay
/// in the drive cache). For state whose loss after a crash or power cut would
/// let two machines run the same work: Pro ownership and the session ledger's
/// Pro transfer writes. Ordinary preference stores and the generic bundle
/// routes keep the cheaper write.
pub(crate) fn atomic_write_json_durable(
    path: &Path,
    contents: impl AsRef<[u8]>,
) -> anyhow::Result<()> {
    use std::io::Write;
    let parent = path.parent().context("durable state needs a parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    file.write_all(contents.as_ref())?;
    full_sync(&file)?;
    drop(file);
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename into {}", path.display()))?;
    // The new bytes are in place. A filesystem that cannot sync a directory
    // (EINVAL/ENOTSUP on some network and FUSE mounts) does not make this
    // write a failure; any other error does.
    let directory = std::fs::File::open(parent)?;
    match full_sync(&directory) {
        Err(error) if !directory_sync_unsupported(&error) => Err(error),
        _ => Ok(()),
    }
}
fn directory_sync_unsupported(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::raw_os_error)
        .is_some_and(|code| {
            code == nix::libc::EINVAL || code == nix::libc::ENOTSUP || code == nix::libc::EOPNOTSUPP
        })
}
fn full_sync(file: &std::fs::File) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: a valid open descriptor; F_FULLFSYNC takes no argument.
        if unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_FULLFSYNC) } == 0 {
            return Ok(());
        }
        // Some filesystems (network, FAT) refuse F_FULLFSYNC; fall back.
    }
    file.sync_all()?;
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;
    /// After a successful rename, a filesystem that cannot sync a directory
    /// does not turn the write into a failure; a real I/O error still does.
    #[test]
    fn an_unsupported_directory_sync_is_not_a_failed_write() {
        for (code, unsupported) in [
            (nix::libc::EINVAL, true),
            (nix::libc::ENOTSUP, true),
            (nix::libc::EIO, false),
        ] {
            let error = anyhow::Error::from(std::io::Error::from_raw_os_error(code));
            assert_eq!(directory_sync_unsupported(&error), unsupported, "{code}");
        }
        let root = std::env::temp_dir().join(format!(
            "chimaera-durable-{}",
            chimaera_core::generate_token()
        ));
        atomic_write_json_durable(&root.join("state.json"), b"{}").unwrap();
        assert_eq!(std::fs::read(root.join("state.json")).unwrap(), b"{}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
