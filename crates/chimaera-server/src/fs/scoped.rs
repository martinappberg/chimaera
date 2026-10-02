//! Descriptor-bound variants of selected-project filesystem effects.
use super::*;
use crate::workspace_scope::files::{Context as Files, Entry};
use rustix::fs::{AtFlags, Mode, OFlags};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn version(
    entry: &Entry,
    pre: Precondition<'_>,
) -> anyhow::Result<Option<(String, Option<String>)>> {
    if entry.stat()?.is_none() {
        return Ok(None);
    }
    let mut file = entry.open(OFlags::RDONLY)?;
    match pre {
        Precondition::Hash(_) => Ok(Some(file_version(&mut file)?)),
        _ => Ok(Some((mtime_token(&file.metadata()?), None))),
    }
}
struct Temporary<'a> {
    entry: &'a Entry,
    name: std::ffi::OsString,
    inode: u64,
}
impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        // Cleanup can only unlink the exact file we created, even if another
        // writer replaced its temporary name while the operation failed.
        if rustix::fs::statat(&self.entry.directory, &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|stat| stat.st_ino == self.inode)
        {
            let _ = rustix::fs::unlinkat(&self.entry.directory, &self.name, AtFlags::empty());
        }
    }
}
pub(super) fn write(
    scope: &Files,
    raw: &str,
    bytes: &[u8],
    pre: Precondition<'_>,
    commit: impl FnOnce() -> anyhow::Result<Option<crate::pro::mutation::Guard>>,
) -> anyhow::Result<WriteOutcome> {
    let entry = scope.entry(raw, true, false)?;
    let hash = sha256_hex(bytes);
    if let Err(outcome) = judge(pre, version(&entry, pre)?, &hash) {
        return Ok(outcome);
    }
    let _commit = commit()?;
    let existing = if entry.stat()?.is_some() {
        Some(entry.open(OFlags::RDWR)?)
    } else {
        None
    };
    let meta = existing.as_ref().map(|file| file.metadata()).transpose()?;
    if meta.as_ref().is_some_and(|meta| meta.nlink() > 1) {
        let mut file = existing.expect("existing hard link");
        if let Err(outcome) = judge(pre, Some(file_version(&mut file)?), &hash) {
            return Ok(outcome);
        }
        entry.check()?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(bytes)?;
        file.set_len(bytes.len() as u64)?;
        file.sync_all()?;
        return Ok(WriteOutcome::Written {
            mtime: mtime_token(&file.metadata()?),
            hash,
            wrote: true,
        });
    }
    let name = crate::persist::project_temp_name(&entry.name);
    entry.check()?;
    let mut file = std::fs::File::from(rustix::fs::openat(
        &entry.directory,
        &name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(
            meta.as_ref()
                .map_or(0o666, |meta| meta.permissions().mode() & 0o7777) as _,
        ),
    )?);
    let temporary = Temporary {
        entry: &entry,
        name: name.clone(),
        inode: file.metadata()?.ino(),
    };
    if let Some(meta) = &meta {
        carry_owner(&file, meta);
        file.set_permissions(meta.permissions())?;
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    if let Err(outcome) = judge(pre, version(&entry, pre)?, &hash) {
        return Ok(outcome);
    }
    entry.check()?;
    rustix::fs::renameat(&entry.directory, &name, &entry.directory, &entry.name)?;
    entry.directory.sync_all()?;
    drop(temporary);
    Ok(WriteOutcome::Written {
        mtime: mtime_token(&file.metadata()?),
        hash,
        wrote: true,
    })
}
pub(super) fn create(scope: &Files, raw: &str, directory: bool) -> anyhow::Result<MutateOutcome> {
    let entry = scope.entry(raw, false, true)?;
    entry.check()?;
    let result = if directory {
        rustix::fs::mkdirat(&entry.directory, &entry.name, Mode::from_raw_mode(0o777))
    } else {
        rustix::fs::openat(
            &entry.directory,
            &entry.name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o666),
        )
        .map(drop)
    };
    match result {
        Ok(()) => Ok(MutateOutcome::Done(
            json!({"path": entry.canonical.to_string_lossy()}),
        )),
        Err(rustix::io::Errno::EXIST) => Ok(MutateOutcome::Conflict(format!(
            "{} already exists",
            entry.canonical.display()
        ))),
        Err(error) => Err(error.into()),
    }
}
pub(super) fn rename(scope: &Files, from: &str, to: &str) -> anyhow::Result<MutateOutcome> {
    rename_checked(scope, from, to, || {})
}
pub(super) fn rename_checked(
    scope: &Files,
    from: &str,
    to: &str,
    before_publish: impl FnOnce(),
) -> anyhow::Result<MutateOutcome> {
    let from = scope.entry(from, false, false)?;
    let to = scope.entry(to, false, false)?;
    let original = from.stat()?.context("source no longer exists")?;
    let mut case_only = false;
    if let Some(target) = to.stat()? {
        if target.st_dev != original.st_dev || target.st_ino != original.st_ino {
            return Ok(MutateOutcome::Conflict(format!(
                "{} already exists",
                to.canonical.display()
            )));
        }
        case_only = from.canonical.parent() == to.canonical.parent()
            && from.name != to.name
            && from.name.to_string_lossy().to_lowercase()
                == to.name.to_string_lossy().to_lowercase();
        if case_only {
            use std::os::unix::ffi::OsStrExt;
            // Distinct case-sensitive hard links also compare as one inode.
            // If the destination has its own literal directory entry, POSIX
            // rename is a no-op; only an alias of the source needs the hop.
            let mut listing = rustix::fs::Dir::read_from(&to.directory)?;
            let mut count = 0;
            while let Some(entry) = listing.read() {
                count += 1;
                anyhow::ensure!(
                    count <= MAX_COPY_ENTRIES,
                    "case-only rename directory exceeds inspection budget"
                );
                if entry?.file_name().to_bytes() == to.name.as_bytes() {
                    case_only = false;
                    break;
                }
            }
        }
        if !case_only {
            // POSIX rename of two existing hard links to the same inode is a
            // no-op; retain both names rather than introducing a delete.
            return Ok(MutateOutcome::Done(
                json!({"path":to.canonical.to_string_lossy()}),
            ));
        }
    }
    before_publish();
    from.check()?;
    to.check()?;
    let flags = rustix::fs::RenameFlags::NOREPLACE;
    let result = if case_only {
        // Case-insensitive filesystems already expose the destination as the
        // source inode. An exclusive temporary hop makes its final name free
        // without letting plain rename overwrite a concurrent arrival.
        let temporary = crate::persist::project_temp_name(&from.name);
        rustix::fs::renameat_with(
            &from.directory,
            &from.name,
            &from.directory,
            &temporary,
            flags,
        )?;
        let publish = (|| -> anyhow::Result<()> {
            from.check()?;
            to.check()?;
            let now = rustix::fs::statat(&from.directory, &temporary, AtFlags::SYMLINK_NOFOLLOW)?;
            anyhow::ensure!(
                now.st_dev == original.st_dev && now.st_ino == original.st_ino,
                "source changed before rename"
            );
            rustix::fs::renameat_with(&from.directory, &temporary, &to.directory, &to.name, flags)?;
            Ok(())
        })();
        if let Err(error) = publish {
            // Refuse rollback over another writer. The staged original remains
            // recoverable and its exact retained path is returned on failure.
            if rustix::fs::renameat_with(
                &from.directory,
                &temporary,
                &from.directory,
                &from.name,
                flags,
            )
            .is_err()
            {
                anyhow::bail!(
                    "rename refused; original retained at {}: {error}",
                    from.canonical
                        .parent()
                        .expect("parent")
                        .join(&temporary)
                        .display()
                );
            }
            return Err(error);
        }
        Ok(())
    } else {
        rustix::fs::renameat_with(&from.directory, &from.name, &to.directory, &to.name, flags)
    };
    match result {
        Ok(()) => Ok(MutateOutcome::Done(
            json!({"path":to.canonical.to_string_lossy()}),
        )),
        Err(rustix::io::Errno::EXIST) => Ok(MutateOutcome::Conflict(format!(
            "{} already exists",
            to.canonical.display()
        ))),
        Err(error) => Err(error.into()),
    }
}

fn charge(entries: &mut usize, depth: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        *entries < MAX_COPY_ENTRIES && depth < 128,
        "filesystem walk exceeds its entry/depth budget"
    );
    *entries += 1;
    Ok(())
}
fn remove(entry: &Entry, entries: &mut usize, depth: usize) -> anyhow::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    charge(entries, depth)?;
    let stat = entry.stat()?.context("source no longer exists")?;
    let directory = rustix::fs::FileType::from_raw_mode(stat.st_mode).is_dir();
    if directory {
        let handle = entry.directory()?;
        let mut listing = rustix::fs::Dir::read_from(&handle)?;
        while let Some(child) = listing.read() {
            let child = child?;
            let name = child.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            remove(
                &entry.child(&handle, std::ffi::OsStr::from_bytes(name))?,
                entries,
                depth + 1,
            )?;
        }
    }
    entry.check()?;
    // Refuse deleting a replacement that arrived while a directory drained.
    anyhow::ensure!(
        entry
            .stat()?
            .is_some_and(|now| now.st_dev == stat.st_dev && now.st_ino == stat.st_ino),
        "entry changed while deleting"
    );
    rustix::fs::unlinkat(
        &entry.directory,
        &entry.name,
        if directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        },
    )?;
    Ok(())
}
pub(super) fn delete(scope: &Files, raw: &str) -> anyhow::Result<MutateOutcome> {
    let entry = scope.entry(raw, false, false)?;
    remove(&entry, &mut 0, 0)?;
    Ok(MutateOutcome::Done(serde_json::Value::Null))
}
fn copy_entry(
    source: &Entry,
    target: &Entry,
    entries: &mut usize,
    depth: usize,
) -> anyhow::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    charge(entries, depth)?;
    let stat = source.stat()?.context("source no longer exists")?;
    let kind = rustix::fs::FileType::from_raw_mode(stat.st_mode);
    source.check()?;
    target.check()?;
    if kind.is_symlink() {
        let link = rustix::fs::readlinkat(&source.directory, &source.name, Vec::new())?;
        target.check()?;
        rustix::fs::symlinkat(&link, &target.directory, &target.name)?;
    } else if kind.is_dir() {
        let directory = source.directory()?;
        target.check()?;
        rustix::fs::mkdirat(
            &target.directory,
            &target.name,
            Mode::from_raw_mode((stat.st_mode & 0o777) as _),
        )?;
        let destination = target.directory()?;
        let mut listing = rustix::fs::Dir::read_from(&directory)?;
        while let Some(child) = listing.read() {
            let child = child?;
            let name = child.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            let name = std::ffi::OsStr::from_bytes(name);
            copy_entry(
                &source.child(&directory, name)?,
                &target.child(&destination, name)?,
                entries,
                depth + 1,
            )?;
        }
    } else if kind.is_file() {
        let mut input = source.open(OFlags::RDONLY)?;
        let mut output = target.open(OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            source.current()?;
            target.current()?;
            output.write_all(&buffer[..count])?;
        }
        output.set_permissions(input.metadata()?.permissions())?;
    } else {
        anyhow::bail!("source is not a regular file, symlink or directory");
    }
    Ok(())
}
pub(super) fn copy(
    scope: &Files,
    from: &str,
    to: &str,
    unique: bool,
) -> anyhow::Result<MutateOutcome> {
    let source = scope.entry(from, false, false)?;
    let mut destination = scope.entry(to, false, false)?;
    if destination.canonical.starts_with(&source.canonical) {
        anyhow::bail!("cannot copy a directory into itself");
    }
    if destination.stat()?.is_some() {
        if !unique {
            return Ok(MutateOutcome::Conflict(format!(
                "{} already exists",
                destination.canonical.display()
            )));
        }
        let stem = destination
            .canonical
            .file_stem()
            .context("missing file name")?
            .to_string_lossy()
            .into_owned();
        let extension = destination
            .canonical
            .extension()
            .map(|extension| format!(".{}", extension.to_string_lossy()))
            .unwrap_or_default();
        let parent = destination
            .canonical
            .parent()
            .context("missing parent")?
            .to_owned();
        let mut found = false;
        for index in 1..=1000 {
            let number = if index == 1 {
                String::new()
            } else {
                format!(" {index}")
            };
            let path = parent.join(format!("{stem} copy{number}{extension}"));
            destination = scope.entry(&path.to_string_lossy(), false, false)?;
            if destination.stat()?.is_none() {
                found = true;
                break;
            }
        }
        anyhow::ensure!(found, "too many conflicting copy names");
    }
    copy_entry(&source, &destination, &mut 0, 0)?;
    Ok(MutateOutcome::Done(
        json!({"path": destination.canonical.to_string_lossy()}),
    ))
}
pub(super) fn mkdir(scope: &Files, raw: &str) -> anyhow::Result<MutateOutcome> {
    if let Ok(bound) = scope.read(raw) {
        if bound.open(true).is_ok() {
            return Ok(MutateOutcome::Done(
                json!({"path":bound.canonical.to_string_lossy()}),
            ));
        }
    }
    let entry = scope.entry(raw, false, true)?;
    if let Some(stat) = entry.stat()? {
        anyhow::ensure!(
            rustix::fs::FileType::from_raw_mode(stat.st_mode).is_dir(),
            "destination is not a directory"
        );
    } else {
        entry.check()?;
        rustix::fs::mkdirat(&entry.directory, &entry.name, Mode::from_raw_mode(0o777))?;
    }
    entry.directory()?;
    Ok(MutateOutcome::Done(
        json!({"path":entry.canonical.to_string_lossy()}),
    ))
}
fn tree_fingerprint(
    entry: &Entry,
    entries: &mut usize,
    depth: usize,
    digest: &mut [u8; 32],
) -> anyhow::Result<()> {
    use sha2::Digest;
    use std::os::unix::ffi::OsStrExt;
    charge(entries, depth)?;
    let stat = entry.stat()?.context("source no longer exists")?;
    let metadata = |stat: &rustix::fs::Stat| {
        format!(
            "{:?}",
            (
                stat.st_dev,
                stat.st_ino,
                stat.st_mode,
                stat.st_nlink,
                stat.st_size,
                stat.st_mtime,
                stat.st_mtime_nsec,
                stat.st_ctime,
                stat.st_ctime_nsec
            )
        )
    };
    let before = metadata(&stat);
    let mut hash = sha2::Sha256::new();
    hash.update(entry.canonical.as_os_str().as_bytes());
    hash.update([0]);
    hash.update(&before);
    let kind = rustix::fs::FileType::from_raw_mode(stat.st_mode);
    if kind.is_file() {
        let mut file = entry.open(OFlags::RDONLY)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            entry.current()?;
            hash.update(&buffer[..count]);
        }
    } else if kind.is_symlink() {
        hash.update(rustix::fs::readlinkat(&entry.directory, &entry.name, Vec::new())?.to_bytes());
    } else if kind.is_dir() {
        let directory = entry.directory()?;
        let mut listing = rustix::fs::Dir::read_from(&directory)?;
        while let Some(child) = listing.read() {
            let child = child?;
            let name = child.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            tree_fingerprint(
                &entry.child(&directory, std::ffi::OsStr::from_bytes(name))?,
                entries,
                depth + 1,
                digest,
            )?;
        }
    }
    anyhow::ensure!(
        entry.stat()?.as_ref().map(&metadata).as_deref() == Some(before.as_str()),
        "source changed while checking move"
    );
    // Each unique path contributes once. Enumeration order is irrelevant and
    // retained memory is constant, including on large/NFS directory trees.
    for (slot, byte) in digest.iter_mut().zip(hash.finalize()) {
        *slot ^= byte;
    }
    Ok(())
}
pub(super) fn cross_device_move(
    scope: &Files,
    from: &str,
    to: &str,
    after_copy: impl FnOnce(),
) -> anyhow::Result<MutateOutcome> {
    let source = scope.entry(from, false, false)?;
    let mut before = [0; 32];
    tree_fingerprint(&source, &mut 0, 0, &mut before)?;
    let result = copy(scope, from, to, false)?;
    if matches!(result, MutateOutcome::Done(_)) {
        after_copy();
        let mut after = [0; 32];
        tree_fingerprint(&source, &mut 0, 0, &mut after)?;
        anyhow::ensure!(
            before == after,
            "source changed during move; both copies retained"
        );
        // As with ordinary filesystem deletion, a writer racing the final
        // check/unlink is outside POSIX atomicity. Changes during the copy
        // window, including nested same-inode edits, are refused here.
        remove(&source, &mut 0, 0)?;
    }
    Ok(result)
}
pub(super) fn move_entry(scope: &Files, from: &str, to: &str) -> anyhow::Result<MutateOutcome> {
    match rename(scope, from, to) {
        Err(error)
            if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::XDEV) =>
        {
            cross_device_move(scope, from, to, || {})
        }
        result => result,
    }
}
