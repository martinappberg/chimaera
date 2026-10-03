//! A return's durable file intents. Originals survive until installation commits;
//! an exact retry rolls forward, while edits made after an intent was prepared
//! leave the project fenced and its recovery data intact.
use anyhow::{ensure, Context, Result};
use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write as _},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_JOURNAL: u64 = 16 * 1024 * 1024;
const MAX_PROGRESS: u64 = 8 * 1024 * 1024;

fn sync_file(file: &File) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: a live descriptor; F_FULLFSYNC takes no argument.
        if unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_FULLFSYNC) } == 0 {
            return Ok(());
        }
    }
    file.sync_all()?;
    Ok(())
}
fn sync_dir(file: &File) -> Result<()> {
    match sync_file(file) {
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error)
                .is_some_and(|code| {
                    code == nix::libc::EINVAL
                        || code == nix::libc::ENOTSUP
                        || code == nix::libc::EOPNOTSUPP
                }) =>
        {
            Ok(())
        }
        result => result,
    }
}
/// A synced child is not durably enrolled until its parent's directory entry
/// is synced too. Include ancestors: a workspace's cache directory may itself
/// have been created during this first return.
fn sync_enrollment(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        sync_dir(&directory(ancestor)?)?;
    }
    Ok(())
}

fn space(directory: &File, bytes: u64) -> Result<()> {
    let available = rustix::fs::fstatvfs(directory)?;
    ensure!(
        available.f_bavail.saturating_mul(available.f_frsize)
            >= bytes.saturating_add(16 * 1024 * 1024),
        "not enough disk space for recoverable installation"
    );
    Ok(())
}
pub(crate) fn stage_budget(root: &Path) -> Result<()> {
    let (files, directories) = paths(root)?;
    let root_dir = directory(root)?;
    let mut total = 0u64;
    for relative in files {
        let dir = parent(&root_dir, &relative, false)?;
        let file = plain(&dir, relative.file_name().unwrap())?
            .context("installation stage disappeared")?;
        total = total
            .checked_add(file.metadata()?.len())
            .context("installation stage size overflow")?;
        ensure!(
            total <= 4 * MAX_BYTES,
            "installation stage exceeds recovery storage limit"
        );
        sync_file(&file)?;
    }
    // Session metadata references the immutable archives here. Make the stage
    // durable before the journal can authorize the first destination mutation.
    for relative in directories.iter().rev() {
        sync_dir(&crate::download::open_beneath(
            &root_dir,
            relative,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        )?)?;
    }
    sync_enrollment(root)?;
    space(&root_dir, 0)
}

/// Durable bounded state beside the installation journal or its admission
/// marker. Pin every parent; an existing temporary symlink is never followed.
pub(crate) fn write_state(path: &Path, bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() as u64 <= MAX_JOURNAL,
        "durable installation state exceeds limit"
    );
    let parent = path.parent().context("installation state has no parent")?;
    ensure!(path.is_absolute(), "installation state must be absolute");
    let filesystem = File::open("/")?;
    let dir = parent_checked(&filesystem, path.strip_prefix("/")?, true, &|| Ok(()))?;
    sync_enrollment(parent)?;
    let name = path.file_name().context("installation state has no name")?;
    let temporary = format!("state-{}.tmp", chimaera_core::generate_token());
    let result = (|| {
        let mut file = File::from(rustix::fs::openat(
            &dir,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?);
        file.write_all(bytes)?;
        sync_file(&file)?;
        rustix::fs::renameat(&dir, temporary.as_str(), &dir, name)?;
        sync_dir(&dir)
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(&dir, temporary.as_str(), AtFlags::empty());
    }
    result
}

/// A staged file change. `before` is the version observed while planning, not a
/// fresh reading taken just before overwriting a possibly newer user edit.
pub(crate) struct Write {
    pub root: PathBuf,
    pub relative: PathBuf,
    pub before: Option<PathBuf>,
    pub after: Option<PathBuf>,
}

#[cfg(test)]
mod tests;

/// Bounded staging copy; every source file is opened beneath the original
/// descriptor. A racing symlink in any component refuses the preparation.
pub(super) fn snapshot(
    root: &Path,
    destination: &Path,
    include: &dyn Fn(&Path) -> bool,
    budget: u64,
) -> Result<()> {
    let root_dir = directory(root)?;
    let mut pending = vec![PathBuf::new()];
    let mut count = 0usize;
    let mut remaining = budget.min(MAX_BYTES);
    fs::create_dir_all(destination)?;
    while let Some(relative_dir) = pending.pop() {
        // Enumeration supplies names only; reads are anchored to root_dir.
        let _checked = crate::download::open_beneath(
            &root_dir,
            &relative_dir,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        )?;
        for entry in fs::read_dir(root.join(&relative_dir))? {
            let entry = entry?;
            count += 1;
            ensure!(
                count <= super::policy::MAX_PATHS,
                "installation staging exceeds path limit"
            );
            let relative = relative_dir.join(entry.file_name());
            if !include(&relative) {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                ensure!(
                    relative.components().count() <= 64,
                    "installation path is too deep"
                );
                fs::create_dir_all(destination.join(&relative))?;
                pending.push(relative);
            } else if kind.is_file() {
                let dir = parent(&root_dir, &relative, false)?;
                let mut input = plain(&dir, relative.file_name().unwrap())?
                    .context("staging source disappeared")?;
                let meta = input.metadata()?;
                ensure!(
                    meta.len() <= super::policy::MAX_FILE_BYTES && meta.len() <= remaining,
                    "installation staging exceeds storage limit"
                );
                let target = destination.join(&relative);
                space(&directory(destination)?, meta.len())?;
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                let bytes = std::io::copy(
                    &mut Read::by_ref(&mut input).take(super::policy::MAX_FILE_BYTES + 1),
                    &mut output,
                )?;
                ensure!(
                    bytes == meta.len() && bytes <= remaining,
                    "staging source changed during capture"
                );
                remaining -= bytes;
                output.set_permissions(fs::Permissions::from_mode(
                    meta.permissions().mode() & 0o777,
                ))?;
            }
        }
    }
    Ok(())
}
fn paths(root: &Path) -> Result<(BTreeSet<PathBuf>, BTreeSet<PathBuf>)> {
    let mut pending = vec![PathBuf::new()];
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut count = 0usize;
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(root.join(&dir))? {
            let entry = entry?;
            count += 1;
            ensure!(
                count <= super::policy::MAX_PATHS,
                "installation staging has too many paths"
            );
            let relative = dir.join(entry.file_name());
            let kind = entry.file_type()?;
            ensure!(
                !kind.is_symlink(),
                "installation staging contains a symlink"
            );
            if kind.is_dir() {
                directories.insert(relative.clone());
                pending.push(relative);
            } else {
                ensure!(kind.is_file(), "installation staging contains a non-file");
                files.insert(relative);
            }
        }
    }
    Ok((files, directories))
}
pub(super) fn changes(root: &Path, before: &Path, after: &Path) -> Result<Vec<Write>> {
    changes_after_walk(root, before, after, &|| Ok(()))
}
fn changes_after_walk(
    root: &Path,
    before: &Path,
    after: &Path,
    after_walk: &dyn Fn() -> Result<()>,
) -> Result<Vec<Write>> {
    let (before_files, before_dirs) = paths(before)?;
    let (after_files, after_dirs) = paths(after)?;
    after_walk()?;
    let before_root = directory(before)?;
    let after_root = directory(after)?;
    let mut all = before_files.clone();
    all.extend(after_files.iter().cloned());
    let mut writes = Vec::new();
    for relative in after_dirs.difference(&before_dirs) {
        writes.push(Write {
            root: root.to_path_buf(),
            relative: relative.clone(),
            before: None,
            after: Some(after.join(relative)),
        });
    }
    for relative in all {
        let old = before.join(&relative);
        let new = after.join(&relative);
        // Enumeration records presence. A removed/replaced file must refuse,
        // not turn a known before/after image into a different write intent.
        let open = |root: &File| -> Result<File> {
            let directory = parent(root, &relative, false)?;
            plain(
                &directory,
                relative.file_name().context("invalid staged path")?,
            )?
            .context("staging image disappeared")
        };
        let mut old_file = before_files
            .contains(&relative)
            .then(|| open(&before_root))
            .transpose()?;
        let mut new_file = after_files
            .contains(&relative)
            .then(|| open(&after_root))
            .transpose()?;
        let old = old_file.as_ref().map(|_| old);
        let new = new_file.as_ref().map(|_| new);
        if let (Some(old), Some(new)) = (&mut old_file, &mut new_file) {
            if digest(old)? == digest(new)? {
                continue;
            }
        }
        writes.push(Write {
            root: root.to_path_buf(),
            relative,
            before: old,
            after: new,
        });
    }
    Ok(writes)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Binding {
    pub endpoint: String,
    pub account: Option<String>,
    pub workspace: String,
    pub epoch: u64,
    pub receipt: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Version {
    sha256: String,
    bytes: u64,
    mode: u32,
    blob: String,
}
#[derive(Clone, Deserialize, Serialize)]
struct Intent {
    root: PathBuf,
    device: u64,
    inode: u64,
    relative: PathBuf,
    before: Option<Version>,
    after: Option<Version>,
    aside: String,
    applied: bool,
    #[serde(default)]
    directory_mode: Option<u32>,
    #[serde(default)]
    ready: bool,
}
#[derive(Deserialize, Serialize)]
struct Journal {
    version: u32,
    binding: Binding,
    committed: bool,
    intents: Vec<Intent>,
    #[serde(default)]
    git_roots: Vec<PathBuf>,
}
pub(crate) struct Transaction {
    directory: PathBuf,
    journal: Journal,
    reservations: Vec<Reservation>,
}

fn relative(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "unsafe installation path"
    );
    Ok(())
}
/// Open an absolute path without following any component, including the root.
pub(crate) fn directory(path: &Path) -> Result<File> {
    ensure!(path.is_absolute(), "installation root must be absolute");
    let base = File::open("/")?;
    let path = path.strip_prefix("/")?;
    Ok(crate::download::open_beneath(
        &base,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
    )?)
}
fn parent(root: &File, relative: &Path, create: bool) -> Result<File> {
    parent_checked(root, relative, create, &|| Ok(()))
}
fn parent_checked(
    root: &File,
    relative: &Path,
    create: bool,
    current: &dyn Fn() -> Result<()>,
) -> Result<File> {
    let mut dir = root.try_clone()?;
    for part in relative
        .parent()
        .context("installation path has no parent")?
        .components()
    {
        let Component::Normal(name) = part else {
            anyhow::bail!("unsafe installation parent")
        };
        if create {
            current()?;
            match rustix::fs::mkdirat(&dir, name, Mode::from_bits_truncate(0o700)) {
                Ok(()) => sync_dir(&dir)?,
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
        }
        dir = File::from(rustix::fs::openat(
            &dir,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
    }
    Ok(dir)
}
fn plain(dir: &File, name: &std::ffi::OsStr) -> Result<Option<File>> {
    match rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => {
            let file = File::from(fd);
            ensure!(
                file.metadata()?.is_file(),
                "installation target is not a regular file"
            );
            Ok(Some(file))
        }
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn digest(file: &mut File) -> Result<(String, u64, u32)> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= super::policy::MAX_FILE_BYTES,
        "installation file exceeds limit"
    );
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut length = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length += count as u64;
        ensure!(
            length <= super::policy::MAX_FILE_BYTES,
            "installation file grew beyond limit"
        );
        hash.update(&buffer[..count]);
    }
    ensure!(
        length == metadata.len(),
        "installation file changed during inspection"
    );
    Ok((
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        length,
        metadata.permissions().mode() & 0o777,
    ))
}
fn matches(dir: &File, name: &std::ffi::OsStr, wanted: &Option<Version>) -> Result<bool> {
    match (plain(dir, name)?, wanted) {
        (None, None) => Ok(true),
        (Some(mut file), Some(version)) => {
            let (hash, length, mode) = digest(&mut file)?;
            Ok(hash == version.sha256 && length == version.bytes && mode == version.mode)
        }
        _ => Ok(false),
    }
}
fn save_blob(directory: &Path, path: &Path, name: String, remaining: &mut u64) -> Result<Version> {
    let dir = self::directory(path.parent().context("staged file has no parent")?)?;
    let mut input = plain(&dir, path.file_name().context("staged file has no name")?)?
        .context("staged file disappeared")?;
    let meta = input.metadata()?;
    ensure!(
        meta.len() <= super::policy::MAX_FILE_BYTES && meta.len() <= *remaining,
        "installation recovery storage exceeds limit"
    );
    let blob = directory.join(&name);
    space(&self::directory(directory)?, meta.len())?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&blob)?;
    output.set_permissions(fs::Permissions::from_mode(0o600))?;
    let copied = std::io::copy(
        &mut Read::by_ref(&mut input).take(super::policy::MAX_FILE_BYTES + 1),
        &mut output,
    )?;
    ensure!(
        copied == meta.len() && copied <= *remaining,
        "staged file changed during capture"
    );
    sync_file(&output)?;
    let mut saved = plain(&self::directory(directory)?, std::ffi::OsStr::new(&name))?
        .context("installation blob disappeared")?;
    let (sha256, bytes, _) = digest(&mut saved)?;
    *remaining -= bytes;
    Ok(Version {
        sha256,
        bytes,
        mode: meta.permissions().mode() & 0o777,
        blob: name,
    })
}
struct Reservation {
    directory: File,
    name: String,
    device: u64,
    inode: u64,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(Some(file)) = plain(&self.directory, std::ffi::OsStr::new(&self.name)) {
            if let Ok(meta) = file.metadata() {
                if meta.dev() == self.device && meta.ino() == self.inode {
                    let _ =
                        rustix::fs::unlinkat(&self.directory, self.name.as_str(), AtFlags::empty());
                    let _ = sync_dir(&self.directory);
                }
            }
        }
    }
}
// Config overlays are planned before native session replacement. If both
// address the same file, session import wins, but only if both saw the same
// original. Compare absolute targets so different descriptor roots cannot
// conceal an overlap.
fn coalesce(writes: Vec<Write>) -> Result<Vec<Write>> {
    fn staged(path: &Option<PathBuf>) -> Result<Option<(String, u64, u32)>> {
        path.as_ref()
            .map(|path| {
                let dir = directory(path.parent().context("staged file has no parent")?)?;
                let mut file = plain(&dir, path.file_name().context("staged file has no name")?)?
                    .context("staged file disappeared")?;
                digest(&mut file)
            })
            .transpose()
    }
    let mut targets: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut result: Vec<Write> = Vec::new();
    for write in writes {
        relative(&write.relative)?;
        let _root = directory(&write.root)?;
        let target = write.root.join(&write.relative);
        if let Some(&index) = targets.get(&target) {
            let prior = &mut result[index];
            let directory_after = |path: &Option<PathBuf>| -> Result<bool> {
                match path {
                    Some(path) => Ok(fs::symlink_metadata(path)?.is_dir()),
                    None => Ok(false),
                }
            };
            let old_dir = directory_after(&prior.after)?;
            let new_dir = directory_after(&write.after)?;
            ensure!(
                old_dir == new_dir,
                "overlapping file and directory installation targets"
            );
            ensure!(
                staged(&prior.before)? == staged(&write.before)?,
                "overlapping installation targets observed different originals"
            );
            prior.after = write.after;
        } else {
            targets.insert(target, result.len());
            result.push(write);
        }
    }
    Ok(result)
}
fn valid_roots(roots: &[PathBuf]) -> Result<()> {
    ensure!(roots.len() <= 2, "installation has too many Git roots");
    let mut unique = BTreeSet::new();
    for root in roots {
        ensure!(
            root.is_absolute()
                && root.as_os_str().len() <= 4096
                && root.components().count() <= 64
                && root
                    .components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
                && unique.insert(root),
            "invalid or duplicate installation Git root"
        );
    }
    Ok(())
}
impl Transaction {
    pub(super) fn cleanup_committed(
        directory: &Path,
        endpoint: &str,
        account: Option<&str>,
        workspace: &str,
        epoch: u64,
    ) -> Result<()> {
        let path = directory.join("journal.json");
        if !path.try_exists()? {
            return Ok(());
        }
        let dir = self::directory(directory)?;
        let file = plain(&dir, std::ffi::OsStr::new("journal.json"))?
            .context("installation journal disappeared")?;
        ensure!(
            file.metadata()?.len() <= MAX_JOURNAL,
            "installation journal exceeds limit"
        );
        let journal: Journal = serde_json::from_reader(file.take(MAX_JOURNAL + 1))?;
        let binding = journal.binding;
        ensure!(
            binding.endpoint == endpoint
                && binding.account.as_deref() == account
                && binding.workspace == workspace
                && binding.epoch == epoch,
            "unfinished installation belongs to another account or epoch"
        );
        let transaction =
            Self::open(directory, &binding)?.context("installation journal disappeared")?;
        ensure!(
            transaction.committed(),
            "installation must commit before setup"
        );
        transaction.cleanup()
    }
    pub(crate) fn open(directory: &Path, binding: &Binding) -> Result<Option<Self>> {
        let path = directory.join("journal.json");
        if !path.try_exists()? {
            return Ok(None);
        }
        let dir = self::directory(directory)?;
        let file = plain(&dir, std::ffi::OsStr::new("journal.json"))?
            .context("installation journal disappeared")?;
        ensure!(
            file.metadata()?.len() <= MAX_JOURNAL,
            "installation journal exceeds limit"
        );
        let mut journal: Journal = serde_json::from_reader(file.take(MAX_JOURNAL + 1))?;
        ensure!(
            journal.version == 1 && journal.binding == *binding,
            "unfinished installation belongs to another account or checkpoint"
        );
        valid_roots(&journal.git_roots)?;
        Self::replay(&dir, &mut journal)?;
        ensure!(
            journal.intents.len() <= super::policy::MAX_PATHS,
            "installation journal has too many paths"
        );
        ensure!(
            !journal.committed || journal.intents.iter().all(|intent| intent.applied),
            "invalid installation commit state"
        );
        for (index, intent) in journal.intents.iter().enumerate() {
            relative(&intent.relative)?;
            ensure!(
                intent
                    .aside
                    .strip_prefix(&format!(
                        "{}return-",
                        crate::persist::PROJECT_STAGING_PREFIX
                    ))
                    .is_some_and(|tail| tail.split_once('-').is_some_and(
                        |(nonce, position)| nonce.len() == 16
                            && nonce.bytes().all(|b| b.is_ascii_hexdigit())
                            && position == index.to_string()
                    )),
                "unsafe installation recovery name"
            );
            for (suffix, version) in [("before", &intent.before), ("after", &intent.after)] {
                if let Some(version) = version {
                    ensure!(
                        version.blob == format!("{index}.{suffix}")
                            && version.bytes <= super::policy::MAX_FILE_BYTES
                            && version.mode <= 0o777
                            && version.sha256.len() == 64
                            && version.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                        "unsafe installation blob"
                    );
                }
            }
        }
        Ok(Some(Self {
            directory: directory.to_path_buf(),
            journal,
            reservations: Vec::new(),
        }))
    }
    pub(crate) fn prepare(
        directory: &Path,
        binding: Binding,
        writes: Vec<Write>,
        budget: u64,
    ) -> Result<Self> {
        ensure!(
            writes.len() <= super::policy::MAX_PATHS,
            "installation has too many paths"
        );
        ensure!(
            !directory.try_exists()?,
            "unfinished installation must be recovered first"
        );
        let writes = coalesce(writes)?;
        fs::create_dir(directory)?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        sync_enrollment(directory)?;
        let mut remaining = budget.min(MAX_BYTES);
        let mut keys = BTreeSet::new();
        let nonce = chimaera_core::generate_token();
        let mut intents = Vec::new();
        let prepared = (|| -> Result<()> {
            for (index, write) in writes.into_iter().enumerate() {
                relative(&write.relative)?;
                ensure!(
                    keys.insert((write.root.clone(), write.relative.clone())),
                    "duplicate installation target"
                );
                let root = self::directory(&write.root)?;
                let meta = root.metadata()?;
                let directory_mode = write
                    .after
                    .as_ref()
                    .map(|path| -> Result<Option<u32>> {
                        let metadata = fs::symlink_metadata(path)?;
                        ensure!(
                            !metadata.is_symlink(),
                            "installation stage contains a symlink"
                        );
                        if metadata.is_dir() {
                            Ok(Some(
                                self::directory(path)?.metadata()?.permissions().mode() & 0o777,
                            ))
                        } else {
                            Ok(None)
                        }
                    })
                    .transpose()?
                    .flatten();
                let before = write
                    .before
                    .as_ref()
                    .map(|path| {
                        save_blob(directory, path, format!("{index}.before"), &mut remaining)
                    })
                    .transpose()?;
                let after = if directory_mode.is_some() {
                    None
                } else {
                    write
                        .after
                        .as_ref()
                        .map(|path| {
                            save_blob(directory, path, format!("{index}.after"), &mut remaining)
                        })
                        .transpose()?
                };
                // Preparation must not silently adopt an edit made after staging.
                match parent(&root, &write.relative, false) {
                    Ok(dir) if directory_mode.is_some() => {
                        match rustix::fs::openat(
                            &dir,
                            write.relative.file_name().unwrap(),
                            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                            Mode::empty(),
                        ) {
                            Ok(_) | Err(rustix::io::Errno::NOENT) => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    Ok(dir) => ensure!(
                        matches(&dir, write.relative.file_name().unwrap(), &before)?,
                        "project changed while preparing installation"
                    ),
                    Err(error)
                        if error.downcast_ref::<rustix::io::Errno>()
                            == Some(&rustix::io::Errno::NOENT)
                            && before.is_none() => {}
                    Err(error) => return Err(error),
                }
                intents.push(Intent {
                    root: write.root,
                    device: meta.dev(),
                    inode: meta.ino(),
                    relative: write.relative,
                    before,
                    after,
                    aside: format!(
                        "{}return-{}-{index}",
                        crate::persist::PROJECT_STAGING_PREFIX,
                        &nonce[..16]
                    ),
                    applied: false,
                    directory_mode,
                    ready: false,
                });
            }
            Ok(())
        })();
        if let Err(error) = prepared {
            let _ = fs::remove_dir_all(directory);
            return Err(error);
        }
        let transaction = Self {
            directory: directory.to_path_buf(),
            journal: Journal {
                version: 1,
                binding,
                committed: false,
                intents,
                git_roots: Vec::new(),
            },
            reservations: Vec::new(),
        };
        if let Err(error) = transaction.persist() {
            let _ = fs::remove_dir_all(directory);
            return Err(error);
        }
        Ok(transaction)
    }
    fn persist(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&self.journal)?;
        ensure!(
            bytes.len() as u64 <= MAX_JOURNAL,
            "installation journal exceeds limit"
        );
        write_state(&self.directory.join("journal.json"), &bytes)
    }
    /// Reserve the original Git stores through file and metadata installation.
    /// A crash leaves recognizable locks; retries reclaim only this journal's
    /// exact marker, never a Git process's existing reservation.
    pub(super) fn reserve_git(
        &mut self,
        roots: Vec<PathBuf>,
        current: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        valid_roots(&roots)?;
        valid_roots(&self.journal.git_roots)?;
        if !self.reservations.is_empty() {
            return Ok(());
        }
        if self.journal.git_roots.is_empty() && !roots.is_empty() {
            self.journal.git_roots = roots;
            self.persist()?;
        }
        let marker = serde_json::to_vec(&(
            self.directory.clone(),
            &self.journal.binding,
            self.journal.intents.first().map(|intent| &intent.aside),
        ))?;
        let mut locks = BTreeSet::new();
        for root in &self.journal.git_roots {
            let dir = directory(root)?;
            for name in ["HEAD", "index", "config", "config.worktree", "packed-refs"] {
                locks.insert((root.clone(), PathBuf::from(format!("{name}.lock"))));
            }
            // Even an unchanged current branch needs a reservation: update-ref
            // can move it without taking the index lock.
            let head_path = root.join("HEAD");
            let original_head = self
                .journal
                .intents
                .iter()
                .find(|intent| intent.root.join(&intent.relative) == head_path)
                .and_then(|intent| intent.before.as_ref());
            let mut head = if let Some(version) = original_head {
                plain(
                    &directory(&self.directory)?,
                    std::ffi::OsStr::new(&version.blob),
                )?
            } else {
                plain(&dir, std::ffi::OsStr::new("HEAD"))?
            };
            if let Some(head) = &mut head {
                ensure!(head.metadata()?.len() <= 4096, "Git HEAD exceeds limit");
                let mut text = String::new();
                head.take(4097).read_to_string(&mut text)?;
                if let Some(reference) = text.trim().strip_prefix("ref: ") {
                    let reference = Path::new(reference);
                    relative(reference)?;
                    ensure!(reference.starts_with("refs"), "invalid Git HEAD reference");
                    let mut lock = reference.as_os_str().to_os_string();
                    lock.push(".lock");
                    // Linked HEAD points into the common store. Reserve the
                    // name there too; private ref locks alone cannot block it.
                    for common in &self.journal.git_roots {
                        locks.insert((common.clone(), PathBuf::from(&lock)));
                    }
                }
            }
            // A ref update is serialized by its named lock; packed-refs alone
            // does not block loose ref writes.
            for intent in &self.journal.intents {
                let absolute = intent.root.join(&intent.relative);
                if let Ok(path) = absolute.strip_prefix(root) {
                    if path.starts_with("refs") && intent.directory_mode.is_none() {
                        let mut lock = path.as_os_str().to_os_string();
                        lock.push(".lock");
                        locks.insert((root.clone(), PathBuf::from(lock)));
                    }
                }
            }
            let _ = dir;
        }
        let mut acquired = Vec::new();
        for (root, path) in locks {
            let root = directory(&root)?;
            let dir = parent_checked(&root, &path, true, current)?;
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let file = if let Some(mut file) = plain(&dir, std::ffi::OsStr::new(&name))? {
                if self.journal.committed && file.metadata()?.len() != marker.len() as u64 {
                    continue;
                }
                ensure!(
                    file.metadata()?.len() == marker.len() as u64,
                    "Git repository is busy"
                );
                let mut existing = Vec::new();
                Read::by_ref(&mut file)
                    .take(marker.len() as u64 + 1)
                    .read_to_end(&mut existing)?;
                if self.journal.committed && existing != marker {
                    continue;
                }
                ensure!(existing == marker, "Git repository is busy");
                file
            } else {
                if self.journal.committed {
                    continue;
                }
                // Publish a complete, synced marker atomically. A crash must
                // never leave an empty live lock indistinguishable from Git's.
                let lock_tag = Sha256::digest([marker.as_slice(), name.as_bytes()].concat())
                    .iter()
                    .take(8)
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                let temporary =
                    format!("{}lock-{lock_tag}", crate::persist::PROJECT_STAGING_PREFIX);
                if let Some(previous) = plain(&dir, std::ffi::OsStr::new(&temporary))? {
                    ensure!(
                        previous.metadata()?.len() <= marker.len() as u64,
                        "Git recovery lock changed"
                    );
                    let mut bytes = Vec::new();
                    previous
                        .take(marker.len() as u64 + 1)
                        .read_to_end(&mut bytes)?;
                    ensure!(marker.starts_with(&bytes), "Git recovery lock changed");
                    rustix::fs::unlinkat(&dir, temporary.as_str(), AtFlags::empty())?;
                }
                current()?;
                let mut file = File::from(rustix::fs::openat(
                    &dir,
                    temporary.as_str(),
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_bits_truncate(0o600),
                )?);
                file.write_all(&marker)?;
                sync_file(&file)?;
                sync_dir(&dir)?;
                current()?;
                rustix::fs::renameat_with(
                    &dir,
                    temporary.as_str(),
                    &dir,
                    name.as_str(),
                    RenameFlags::NOREPLACE,
                )?;
                plain(&dir, std::ffi::OsStr::new(&name))?.context("Git lock disappeared")?
            };
            let meta = file.metadata()?;
            let reservation = Reservation {
                directory: dir,
                name,
                device: meta.dev(),
                inode: meta.ino(),
            };
            sync_dir(&reservation.directory)?;
            acquired.push(reservation);
        }
        self.reservations = acquired;
        Ok(())
    }
    fn verify_installed(&self) -> Result<()> {
        for intent in &self.journal.intents {
            let root = directory(&intent.root)?;
            let meta = root.metadata()?;
            ensure!(
                meta.dev() == intent.device && meta.ino() == intent.inode,
                "installation folder was replaced"
            );
            let dir = parent(&root, &intent.relative, false)?;
            if intent.directory_mode.is_some() {
                let _checked = rustix::fs::openat(
                    &dir,
                    intent.relative.file_name().unwrap(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
            } else {
                ensure!(
                    matches(&dir, intent.relative.file_name().unwrap(), &intent.after)?,
                    "file edited after installation; recovery retained"
                );
            }
        }
        Ok(())
    }
    fn replay(dir: &File, journal: &mut Journal) -> Result<()> {
        let Some(file) = plain(dir, std::ffi::OsStr::new("progress.jsonl"))? else {
            return Ok(());
        };
        ensure!(
            file.metadata()?.len() <= MAX_PROGRESS,
            "installation progress exceeds limit"
        );
        let mut bytes = Vec::new();
        file.take(MAX_PROGRESS + 1).read_to_end(&mut bytes)?;
        let end = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        for line in bytes[..end]
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let (index, phase): (usize, u8) = serde_json::from_slice(line)?;
            match phase {
                1 if index < journal.intents.len() => journal.intents[index].ready = true,
                2 if index < journal.intents.len() => journal.intents[index].applied = true,
                3 if index == journal.intents.len()
                    && journal.intents.iter().all(|intent| intent.applied) =>
                {
                    journal.committed = true
                }
                _ => anyhow::bail!("invalid installation progress"),
            }
        }
        if end != bytes.len() {
            // An interrupted append is never authority for a mutation. Remove
            // only that incomplete tail before appending the next durable step.
            let file = File::from(rustix::fs::openat(
                dir,
                "progress.jsonl",
                OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
            ensure!(file.metadata()?.is_file(), "invalid installation progress");
            file.set_len(end as u64)?;
            sync_file(&file)?;
        }
        Ok(())
    }
    fn progress(&mut self, index: usize, phase: u8) -> Result<()> {
        if (phase == 1 && self.journal.intents[index].ready)
            || (phase == 2 && self.journal.intents[index].applied)
            || (phase == 3 && self.journal.committed)
        {
            return Ok(());
        }
        let dir = directory(&self.directory)?;
        let mut file = File::from(rustix::fs::openat(
            &dir,
            "progress.jsonl",
            OFlags::WRONLY
                | OFlags::APPEND
                | OFlags::CREATE
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?);
        let mut bytes = serde_json::to_vec(&(index, phase))?;
        bytes.push(b'\n');
        ensure!(
            file.metadata()?.is_file()
                && file.metadata()?.len() + bytes.len() as u64 <= MAX_PROGRESS,
            "installation progress exceeds limit"
        );
        file.write_all(&bytes)?;
        sync_file(&file)?;
        sync_dir(&dir)?;
        match phase {
            1 => self.journal.intents[index].ready = true,
            2 => self.journal.intents[index].applied = true,
            3 => self.journal.committed = true,
            _ => unreachable!(),
        }
        Ok(())
    }
    pub(crate) fn committed(&self) -> bool {
        self.journal.committed
    }
    pub(crate) fn apply(&mut self, current: &dyn Fn() -> Result<()>) -> Result<()> {
        if self.journal.committed {
            return Ok(());
        }
        current()?;
        for guard in &self.journal.intents {
            if guard.directory_mode.is_some() || !same_version(&guard.before, &guard.after) {
                continue;
            }
            let root = self::directory(&guard.root)?;
            let meta = root.metadata()?;
            ensure!(
                meta.dev() == guard.device && meta.ino() == guard.inode,
                "installation folder was replaced"
            );
            let dir = parent(&root, &guard.relative, false)?;
            ensure!(
                matches(&dir, guard.relative.file_name().unwrap(), &guard.before)?,
                "checkout changed during installation; recovery retained"
            );
        }
        for index in 0..self.journal.intents.len() {
            current()?;
            let intent = self.journal.intents[index].clone();
            let root = self::directory(&intent.root)?;
            let meta = root.metadata()?;
            ensure!(
                meta.dev() == intent.device && meta.ino() == intent.inode,
                "installation folder was replaced"
            );
            let dir = parent_checked(&root, &intent.relative, true, current)?;
            let name = intent.relative.file_name().unwrap();
            if let Some(mode) = intent.directory_mode {
                current()?;
                match rustix::fs::mkdirat(&dir, name, Mode::from_bits_truncate(mode as _)) {
                    Ok(()) => {}
                    Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
                let _checked = rustix::fs::openat(
                    &dir,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                sync_dir(&dir)?;
                self.progress(index, 2)?;
                continue;
            }
            if matches(&dir, name, &intent.after)? {
                // A crash may have occurred after the rename, before this bit.
                self.progress(index, 2)?;
                continue;
            }
            ensure!(
                !intent.applied,
                "file edited after installation; recovery retained"
            );
            let temporary = format!("{}.new", intent.aside);
            if intent.ready {
                ensure!(
                    plain(&dir, std::ffi::OsStr::new(&temporary))?.is_some(),
                    "installed file was removed after replacement; recovery retained"
                );
            }
            let aside = std::ffi::OsStr::new(&intent.aside);
            if !matches(&dir, name, &None)? {
                ensure!(
                    matches(&dir, name, &intent.before)?,
                    "file edited during installation; recovery retained"
                );
                // NOREPLACE and descriptor-relative names avoid both symlink
                // substitution and overwriting a name created concurrently.
                current()?;
                rustix::fs::renameat_with(&dir, name, &dir, aside, RenameFlags::NOREPLACE)?;
                sync_dir(&dir)?;
            }
            if intent.before.is_some() {
                ensure!(
                    matches(&dir, aside, &intent.before)?,
                    "file changed during replacement; original retained"
                );
            }
            if let Some(after) = &intent.after {
                let temp = format!("{}.new", intent.aside);
                // A crash can leave a complete or partial private temp. The
                // immutable after blob is the authority, so retry replaces it.
                match rustix::fs::unlinkat(&dir, temp.as_str(), AtFlags::empty()) {
                    Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                    Err(error) => return Err(error.into()),
                }
                let mut output = File::from(rustix::fs::openat(
                    &dir,
                    temp.as_str(),
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_bits_truncate(after.mode as _),
                )?);
                let mut input = plain(
                    &directory(&self.directory)?,
                    std::ffi::OsStr::new(&after.blob),
                )?
                .context("installation blob disappeared")?;
                ensure!(
                    digest(&mut input)? == (after.sha256.clone(), after.bytes, 0o600),
                    "installation blob changed"
                );
                use std::io::Seek;
                input.rewind()?;
                let copied = std::io::copy(&mut input, &mut output)?;
                ensure!(copied == after.bytes, "installation blob changed");
                output.set_permissions(fs::Permissions::from_mode(after.mode))?;
                output.flush()?;
                sync_file(&output)?;
                sync_dir(&dir)?;
                self.progress(index, 1)?;
                current()?;
                rustix::fs::renameat_with(&dir, temp.as_str(), &dir, name, RenameFlags::NOREPLACE)?;
            }
            sync_dir(&dir)?;
            self.progress(index, 2)?;
        }
        Ok(())
    }
    /// Commit before profile commands or agent admission. Retained backups are
    /// deleted only after this durable bit; no external command is rolled back.
    pub(crate) fn commit(&mut self, current: &dyn Fn() -> Result<()>) -> Result<()> {
        ensure!(
            self.journal.intents.iter().all(|intent| intent.applied),
            "installation is incomplete"
        );
        if !self.journal.committed {
            self.verify_installed()?;
        }
        current()?;
        self.progress(self.journal.intents.len(), 3)
    }
    pub(crate) fn cleanup(mut self) -> Result<()> {
        ensure!(
            self.journal.committed,
            "cannot discard unfinished installation"
        );
        self.reserve_git(Vec::new(), &|| Ok(()))?;
        for intent in &self.journal.intents {
            if intent.directory_mode.is_some() {
                continue;
            }
            let root = self::directory(&intent.root)?;
            let meta = root.metadata()?;
            ensure!(
                meta.dev() == intent.device && meta.ino() == intent.inode,
                "installation folder was replaced"
            );
            let dir = parent(&root, &intent.relative, false)?;
            let aside = std::ffi::OsStr::new(&intent.aside);
            // A user can have edited the displaced inode through an open fd.
            // Retain it rather than deleting an unrecorded version.
            if plain(&dir, aside)?.is_some() {
                ensure!(
                    matches(&dir, aside, &intent.before)?,
                    "original changed after replacement; recovery retained"
                );
                rustix::fs::unlinkat(&dir, aside, AtFlags::empty())?;
                sync_dir(&dir)?;
            }
        }
        self.reservations.clear();
        fs::remove_dir_all(&self.directory)?;
        sync_enrollment(
            self.directory
                .parent()
                .context("installation journal has no parent")?,
        )?;
        Ok(())
    }
}
fn same_version(before: &Option<Version>, after: &Option<Version>) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(before), Some(after)) => {
            before.sha256 == after.sha256
                && before.bytes == after.bytes
                && before.mode == after.mode
        }
        _ => false,
    }
}
