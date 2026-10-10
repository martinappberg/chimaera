//! Atomic persistence for the daemon's small JSON state stores (view-state,
//! ledger, workspaces, recents, settings). Each writes its whole file at once
//! via a temp sibling + rename, so a crash mid-write never leaves a torn file.
//! Serialization (compact vs pretty) and any bookkeeping (a generation bump, a
//! `written_at` stamp) stay at the call site; this owns only the shared
//! create-dir → write-tmp → rename dance.

use std::path::Path;

use anyhow::Context;

/// Reserved siblings are incomplete daemon writes, never project snapshot data.
pub const PROJECT_STAGING_PREFIX: &str = ".chimaera-staging-";

/// Keep atomic renames on the destination filesystem without putting partial
/// bytes into a mirror. Preserve UTF-8 boundaries and the usual NAME_MAX ceiling.
pub fn project_temp_name(name: &std::ffi::OsStr) -> std::ffi::OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let nonce = &chimaera_core::generate_token()[..32];
    let budget = 255 - (PROJECT_STAGING_PREFIX.len() + 1 + nonce.len() + ".tmp".len());
    let bytes = name.as_bytes();
    let cut = utf8_cut(bytes, budget);
    let mut out = Vec::with_capacity(255);
    out.extend_from_slice(PROJECT_STAGING_PREFIX.as_bytes());
    out.extend_from_slice(&bytes[..cut]);
    out.push(b'.');
    out.extend_from_slice(nonce.as_bytes());
    out.extend_from_slice(b".tmp");
    std::ffi::OsString::from_vec(out)
}

/// The longest prefix of `bytes` within `budget` that does not split a UTF-8
/// sequence (names that are not UTF-8 are cut at the budget).
fn utf8_cut(bytes: &[u8], budget: usize) -> usize {
    let mut cut = bytes.len().min(budget);
    while cut > 0 && cut < bytes.len() && (bytes[cut] & 0b1100_0000) == 0b1000_0000 {
        cut -= 1;
    }
    cut
}

/// The hidden temp sibling for a write into a project. `staging` (a daemon
/// with the extension) reserves the [`PROJECT_STAGING_PREFIX`] names a mirror
/// skips; otherwise it is the plain `.{name}.{8 random}.tmp` sibling, shortened
/// at a UTF-8 boundary so the whole stays within NAME_MAX.
pub(crate) fn project_temp_sibling(staging: bool, name: &std::ffi::OsStr) -> std::ffi::OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    if staging {
        return project_temp_name(name);
    }
    let nonce = &chimaera_core::generate_token()[..8];
    let budget = 255 - (".".len() + ".".len() + nonce.len() + ".tmp".len());
    let bytes = name.as_bytes();
    let cut = utf8_cut(bytes, budget);
    let mut out = Vec::with_capacity(cut + 14);
    out.push(b'.');
    out.extend_from_slice(&bytes[..cut]);
    out.push(b'.');
    out.extend_from_slice(nonce.as_bytes());
    out.extend_from_slice(b".tmp");
    std::ffi::OsString::from_vec(out)
}

/// Whether a listing hides `name` as an incomplete staged write: only on a
/// daemon that stages (`project_temp_sibling`).
pub(crate) fn is_project_staging(staging: bool, name: &str) -> bool {
    staging && name.starts_with(PROJECT_STAGING_PREFIX)
}

/// Like [`atomic_write_json`], but the bytes and the rename reach stable
/// storage before returning (F_FULLFSYNC on macOS, where fsync alone may stay
/// in the drive cache). For state whose loss after a crash or power cut would
/// let two machines run the same work: Pro ownership and the session ledger's
/// Pro transfer writes, including public session imports. Ordinary preference
/// stores keep the cheaper write.
pub fn atomic_write_json_durable(path: &Path, contents: impl AsRef<[u8]>) -> anyhow::Result<()> {
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
/// Called under a store's writer gate when a newer snapshot already won. Sync
/// that current version rather than replaying the older durable caller's bytes.
pub(crate) fn sync_json_durable(path: &Path) -> anyhow::Result<()> {
    let (file, _) = crate::fs::open_regular(path)?;
    full_sync(&file)?;
    let directory = std::fs::File::open(path.parent().context("durable state needs a parent")?)?;
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

    /// A daemon without the extension writes the same hidden temp sibling
    /// it always did and hides nothing extra from listings.
    #[test]
    fn a_free_daemons_temp_files_keep_their_names() {
        let free = project_temp_sibling(false, std::ffi::OsStr::new("notes.md"));
        let free = free.to_str().unwrap();
        assert!(
            free.starts_with(".notes.md.") && free.ends_with(".tmp"),
            "{free}"
        );
        assert_eq!(free.len(), ".notes.md.".len() + 8 + ".tmp".len());
        assert!(!free.starts_with(PROJECT_STAGING_PREFIX));
        let staged = project_temp_sibling(true, std::ffi::OsStr::new("notes.md"));
        assert!(staged.to_str().unwrap().starts_with(PROJECT_STAGING_PREFIX));
        assert!(!is_project_staging(
            false,
            ".chimaera-staging-notes.md.x.tmp"
        ));
        assert!(is_project_staging(true, ".chimaera-staging-notes.md.x.tmp"));
        let long = "é".repeat(200);
        let name = project_temp_sibling(false, std::ffi::OsStr::new(&long));
        assert!(name.len() <= 255 && name.to_str().is_some());
    }
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
