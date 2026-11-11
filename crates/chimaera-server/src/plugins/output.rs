//! Output folders: where a plugin keeps what it makes (a built PDF, a log,
//! a snapshot to diff against), one per (plugin, workspace), outside the
//! repository. Design: docs/design/plugin-platform-plan.md §7.
//!
//! - `<cache dir>/plugins/<id>/<workspace>/` (`chimaera_core::cache_dir`:
//!   `$XDG_CACHE_HOME/chimaera`, survives restarts, never night-scrubbed).
//! - The plugin addresses it as `output:<path>`: every path is relative,
//!   walked from the folder's own descriptor with `O_NOFOLLOW` on each
//!   component (a program the plugin runs could leave a link there; on
//!   Windows a cap-std walk that refuses junctions and symlinks alike), so
//!   nothing it names leaves the folder.
//! - Reads start at an offset and stop at 8 MiB a call (a large log
//!   streams); the plugin's own writes are ≤ 8 MiB a file, written to a
//!   temporary name and renamed into place.
//! - A quota per plugin across its workspaces (1 GiB): past it, the least
//!   recently changed top-level entries go. Usage is measured by a bounded
//!   walk, at most once a minute per plugin unless a Clear reset it; in
//!   between, every write the plugin makes itself (a file, a large
//!   surface) is charged against that figure before it lands (`charge`),
//!   and one that would pass the quota measures again, evicts to make room,
//!   or is refused.
//! - The UI reads output files through the ordinary file routes by their
//!   absolute path (`GET …/plugins/{pid}/output` answers the folder), so the
//!   PDF viewer, the log view and `/raw` tickets work unchanged.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use axum::extract::{Path as AxPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde_json::json;

use crate::AppState;

/// A read's ceiling, whatever `cap` the plugin asks for.
pub(crate) const READ_MAX: usize = 8 << 20;
/// A file the plugin writes itself.
pub(crate) const WRITE_MAX: usize = 8 << 20;
const LIST_MAX: usize = 4096;
/// A plugin's output folders together.
pub(crate) const QUOTA: u64 = 1 << 30;
/// The walk that measures usage stops here (entries).
const WALK_MAX: usize = 200_000;
const USAGE_TTL: Duration = Duration::from_secs(60);

/// Measured usage per plugin: bytes, and when.
#[derive(Default)]
pub(crate) struct Usage {
    by_plugin: HashMap<String, (u64, Instant)>,
}

/// The root every plugin's output folders live under.
pub(crate) fn root_for(data_dir: &Path) -> PathBuf {
    #[cfg(test)]
    return data_dir.join("cache").join("plugins");
    #[cfg(not(test))]
    {
        let _ = data_dir;
        chimaera_core::cache_dir().join("plugins")
    }
}

/// `(plugin, workspace)`'s folder (not created).
pub(crate) fn folder(root: &Path, plugin: &str, ws: &str) -> PathBuf {
    root.join(plugin).join(crate::timeline::sanitize(ws))
}

/// `output:<path>` or a bare path, as a relative path of plain components.
pub(crate) fn relative(path: &str) -> Result<PathBuf, String> {
    let path = path.strip_prefix("output:").unwrap_or(path);
    let mut out = PathBuf::new();
    for part in Path::new(path).components() {
        match part {
            Component::Normal(name) => out.push(name),
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "output:{path}: paths stay inside the output folder (no `..`, not absolute)"
                ))
            }
        }
    }
    Ok(out)
}

fn io(rel: &Path, err: impl std::fmt::Display) -> String {
    format!("output:{}: {err}", rel.display())
}

/// The folder itself, created (0700) if missing.
fn open_root(dir: &Path) -> Result<File, String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .map_err(|e| format!("the output folder: {e}"))?;
    crate::download::open_dir_path(dir).map_err(|e| format!("the output folder: {e}"))
}

#[cfg(unix)]
fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// Bytes of `rel` from `offset`, at most `cap` (blocking).
pub(crate) fn read(dir: &Path, rel: &Path, offset: u64, cap: usize) -> Result<Vec<u8>, String> {
    let root = open_root(dir)?;
    let mut file = crate::download::open_beneath(&root, rel, crate::download::Beneath::Any)
        .map_err(|e| io(rel, e))?;
    let meta = file.metadata().map_err(|e| io(rel, e))?;
    if !meta.is_file() {
        return Err(io(rel, "not a regular file"));
    }
    let cap = cap.min(READ_MAX);
    file.seek(SeekFrom::Start(offset)).map_err(|e| io(rel, e))?;
    let mut bytes = Vec::with_capacity(cap.min(meta.len().saturating_sub(offset) as usize));
    file.take(cap as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| io(rel, e))?;
    Ok(bytes)
}

/// A folder's entries, at most `cap`, sorted (blocking).
#[cfg(unix)]
pub(crate) fn list(
    dir: &Path,
    rel: &Path,
    cap: usize,
) -> Result<Vec<super::runtime::wit::Entry>, String> {
    let root = open_root(dir)?;
    let folder = crate::download::open_beneath(&root, rel, crate::download::Beneath::Dir)
        .map_err(|e| io(rel, e))?;
    let entries = rustix::fs::Dir::read_from(&folder).map_err(|e| io(rel, e))?;
    let mut out = Vec::new();
    for entry in entries {
        if out.len() >= cap.min(LIST_MAX) {
            break;
        }
        let entry = entry.map_err(|e| io(rel, e))?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let kind = match entry.file_type() {
            FileType::Unknown => rustix::fs::statat(&folder, name, AtFlags::SYMLINK_NOFOLLOW)
                .map(|st| FileType::from_raw_mode(st.st_mode))
                .unwrap_or(FileType::Unknown),
            known => known,
        };
        out.push(super::runtime::wit::Entry {
            name: name.to_string_lossy().into_owned(),
            is_dir: kind == FileType::Directory,
            is_symlink: kind == FileType::Symlink,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Keep the `keep` newest entries of folder `rel` (by modification time)
/// and remove the rest, never following a link (blocking). A folder that
/// isn't there yet is fine.
#[cfg(unix)]
pub(crate) fn prune_oldest(dir: &Path, rel: &Path, keep: usize) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let root = open_root(dir)?;
    let folder = match crate::download::open_beneath(&root, rel, crate::download::Beneath::Dir) {
        Ok(folder) => folder,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(rel, e)),
    };
    let mut entries = Vec::new();
    for entry in rustix::fs::Dir::read_from(&folder).map_err(|e| io(rel, e))? {
        if entries.len() >= LIST_MAX {
            break;
        }
        let entry = entry.map_err(|e| io(rel, e))?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let name = std::ffi::OsStr::from_bytes(name).to_os_string();
        if let Ok(st) = rustix::fs::statat(&folder, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
            entries.push((st.st_mtime, name));
        }
    }
    if entries.len() <= keep {
        return Ok(());
    }
    entries.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    let mut left = WALK_MAX;
    for (_, name) in entries.into_iter().skip(keep) {
        remove_entry(&folder, &name, &mut left).map_err(|e| io(rel, e))?;
    }
    Ok(())
}

/// The parent folder of `rel` beneath `root`, made on the way (never
/// through a link, private: the output folder's), and `rel`'s last
/// component.
#[cfg(unix)]
fn parent_of(root: &File, rel: &Path) -> Result<(File, std::ffi::OsString), String> {
    parent_made(root, rel, 0o700)
}

/// `parent_of`, making missing folders with `mode` (the umask applies).
#[cfg(unix)]
fn parent_made(
    root: &File,
    rel: &Path,
    mode: rustix::fs::RawMode,
) -> Result<(File, std::ffi::OsString), String> {
    let mut parts: Vec<&std::ffi::OsStr> = rel
        .components()
        .map(|c| match c {
            Component::Normal(n) => Ok(n),
            _ => Err(io(rel, "not a plain relative path")),
        })
        .collect::<Result<_, _>>()?;
    let name = parts
        .pop()
        .ok_or_else(|| io(rel, "names the folder itself"))?
        .to_os_string();
    let mut dir = root.try_clone().map_err(|e| io(rel, e))?;
    for part in parts {
        match rustix::fs::mkdirat(&dir, part, Mode::from_raw_mode(mode)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(io(rel, e)),
        }
        dir = File::from(
            rustix::fs::openat(&dir, part, dir_flags(), Mode::empty()).map_err(|e| io(rel, e))?,
        );
    }
    Ok((dir, name))
}

/// Make folder `rel` (and those on its path) beneath the output folder,
/// never through a link (blocking).
#[cfg(unix)]
pub(crate) fn make_dir(dir: &Path, rel: &Path) -> Result<(), String> {
    let root = open_root(dir)?;
    if rel.as_os_str().is_empty() {
        return Ok(());
    }
    let (parent, name) = parent_of(&root, rel)?;
    match rustix::fs::mkdirat(&parent, name.as_os_str(), Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(e) => return Err(io(rel, e)),
    }
    // It must be a folder, not a link someone left there.
    rustix::fs::openat(&parent, name.as_os_str(), dir_flags(), Mode::empty())
        .map(drop)
        .map_err(|e| io(rel, e))
}

/// Create (or truncate) file `rel` for writing, never through a link
/// (blocking): a job's log.
#[cfg(unix)]
pub(crate) fn create_file(dir: &Path, rel: &Path) -> Result<File, String> {
    let root = open_root(dir)?;
    let (parent, name) = parent_of(&root, rel)?;
    let fd = rustix::fs::openat(
        &parent,
        name.as_os_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| io(rel, e))?;
    Ok(File::from(fd))
}

/// A file of `len` bytes is one the plugin may write itself.
pub(crate) fn fits(rel: &Path, len: usize) -> Result<(), String> {
    if len > WRITE_MAX {
        return Err(io(
            rel,
            format!(
                "a file the plugin writes is at most {} MiB",
                WRITE_MAX >> 20
            ),
        ));
    }
    Ok(())
}

/// Write `bytes` to `rel` (a temporary name, then a rename: a reader never
/// sees half a file) (blocking).
#[cfg(unix)]
pub(crate) fn write(dir: &Path, rel: &Path, bytes: &[u8]) -> Result<(), String> {
    fits(rel, bytes.len())?;
    let root = open_root(dir)?;
    let (parent, name) = parent_of(&root, rel)?;
    let tmp = format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        &chimaera_core::generate_token()[..8]
    );
    let fd = rustix::fs::openat(
        &parent,
        tmp.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| io(rel, e))?;
    let mut file = File::from(fd);
    let written = file.write_all(bytes).and_then(|()| file.flush());
    drop(file);
    if let Err(err) = written {
        let _ = rustix::fs::unlinkat(&parent, tmp.as_str(), AtFlags::empty());
        return Err(io(rel, err));
    }
    // A folder where the file goes is refused by rename (EISDIR); a link
    // there is replaced, never written through.
    rustix::fs::renameat(&parent, tmp.as_str(), &parent, name.as_os_str()).map_err(|e| {
        let _ = rustix::fs::unlinkat(&parent, tmp.as_str(), AtFlags::empty());
        io(rel, e)
    })
}

/// Copy output file `rel` to `dst_rel` beneath the workspace root `dst`
/// (**Save to workspace**, the user's click): never through a link on
/// either side, never over an existing file unless `replace`; its bytes.
#[cfg(unix)]
pub(crate) fn copy_out(
    dir: &Path,
    rel: &Path,
    dst: &Path,
    dst_rel: &Path,
    replace: bool,
) -> Result<u64, String> {
    let root = open_root(dir)?;
    let mut src = crate::download::open_beneath(&root, rel, crate::download::Beneath::Any)
        .map_err(|e| io(rel, e))?;
    if !src.metadata().map_err(|e| io(rel, e))?.is_file() {
        return Err(io(rel, "not a regular file"));
    }
    let dst_root = File::open(dst).map_err(|e| format!("the workspace root: {e}"))?;
    // Folders made in the workspace are ordinary ones (collaborators on a
    // shared project read them), not the output folder's private ones.
    let (parent, name) = parent_made(&dst_root, dst_rel, 0o755)?;
    let shown = || dst_rel.display().to_string();
    match rustix::fs::statat(&parent, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile => {
            return Err(format!("{}: something other than a file is there", shown()))
        }
        Ok(_) if !replace => return Err(format!("{} already exists", shown())),
        _ => {}
    }
    let tmp = format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        &chimaera_core::generate_token()[..8]
    );
    let fd = rustix::fs::openat(
        &parent,
        tmp.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o644),
    )
    .map_err(|e| format!("{}: {e}", shown()))?;
    let mut out = File::from(fd);
    let copied = std::io::copy(&mut (&mut src).take(QUOTA), &mut out).and_then(|n| {
        out.flush()?;
        Ok(n)
    });
    drop(out);
    let n = match copied {
        Ok(n) => n,
        Err(err) => {
            let _ = rustix::fs::unlinkat(&parent, tmp.as_str(), AtFlags::empty());
            return Err(format!("{}: {err}", shown()));
        }
    };
    rustix::fs::renameat(&parent, tmp.as_str(), &parent, name.as_os_str()).map_err(|e| {
        let _ = rustix::fs::unlinkat(&parent, tmp.as_str(), AtFlags::empty());
        format!("{}: {e}", shown())
    })?;
    Ok(n)
}

/// Whether any component of `rel` is hidden (starts with a dot).
fn hidden(rel: &Path) -> bool {
    rel.components()
        .any(|c| c.as_os_str().as_encoded_bytes().first() == Some(&b'.'))
}

/// Remove `rel`, a file or a whole folder, never following a link
/// (blocking). The folder itself: `rel` empty.
pub(crate) fn remove(dir: &Path, rel: &Path) -> Result<(), String> {
    if !dir.exists() {
        return Ok(());
    }
    let root = open_root(dir)?;
    #[cfg(windows)]
    let root = crate::download::cap_dir(&root).map_err(|e| io(rel, e))?;
    if rel.as_os_str().is_empty() {
        let mut left = WALK_MAX;
        return empty_folder(&root, &mut left).map_err(|e| io(rel, e));
    }
    let (parent, name) = parent_of(&root, rel)?;
    remove_entry(&parent, &name, &mut WALK_MAX.clone()).map_err(|e| io(rel, e))
}

#[cfg(unix)]
fn remove_entry(parent: &File, name: &std::ffi::OsStr, left: &mut usize) -> std::io::Result<()> {
    let st = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if FileType::from_raw_mode(st.st_mode) == FileType::Directory {
        let dir = File::from(rustix::fs::openat(
            parent,
            name,
            dir_flags(),
            Mode::empty(),
        )?);
        empty_folder(&dir, left)?;
        rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?;
    } else {
        rustix::fs::unlinkat(parent, name, AtFlags::empty())?;
    }
    Ok(())
}

#[cfg(unix)]
fn empty_folder(dir: &File, left: &mut usize) -> std::io::Result<()> {
    let names: Vec<std::ffi::OsString> = rustix::fs::Dir::read_from(dir)?
        .filter_map(Result::ok)
        .map(|e| std::ffi::OsString::from(e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n != "." && n != "..")
        .collect();
    for name in names {
        if *left == 0 {
            return Err(std::io::Error::other("too many entries to remove"));
        }
        *left -= 1;
        remove_entry(dir, &name, left)?;
    }
    Ok(())
}

/// The Windows twins of the descriptor-relative functions above: the same
/// contracts on cap-std directories, whose every open is beneath the folder
/// and refuses a junction or symlink (`open_dir_nofollow`,
/// `FollowSymlinks::No`). Modes are not set: files inherit the folder's
/// ACL, which is the user's own under the profile.
#[cfg(windows)]
mod win {
    use super::*;
    use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
    use cap_std::fs::{Dir, OpenOptions};
    use std::ffi::{OsStr, OsString};

    fn entry(
        name: OsString,
        ty: Option<cap_std::fs::FileType>,
    ) -> super::super::runtime::wit::Entry {
        super::super::runtime::wit::Entry {
            name: name.to_string_lossy().into_owned(),
            is_dir: ty.is_some_and(|t| t.is_dir()),
            is_symlink: ty.is_some_and(|t| t.is_symlink()),
        }
    }

    pub(crate) fn list(
        dir: &Path,
        rel: &Path,
        cap: usize,
    ) -> Result<Vec<super::super::runtime::wit::Entry>, String> {
        let root = open_root(dir)?;
        let folder = crate::download::open_beneath(&root, rel, crate::download::Beneath::Dir)
            .and_then(|f| crate::download::cap_dir(&f))
            .map_err(|e| io(rel, e))?;
        let mut out = Vec::new();
        for e in folder.entries().map_err(|e| io(rel, e))? {
            if out.len() >= cap.min(LIST_MAX) {
                break;
            }
            let e = e.map_err(|e| io(rel, e))?;
            out.push(entry(e.file_name(), e.file_type().ok()));
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub(crate) fn prune_oldest(dir: &Path, rel: &Path, keep: usize) -> Result<(), String> {
        let root = open_root(dir)?;
        let folder = match crate::download::open_beneath(&root, rel, crate::download::Beneath::Dir)
        {
            Ok(folder) => crate::download::cap_dir(&folder).map_err(|e| io(rel, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io(rel, e)),
        };
        let mut entries = Vec::new();
        for e in folder.entries().map_err(|e| io(rel, e))? {
            if entries.len() >= LIST_MAX {
                break;
            }
            let e = e.map_err(|e| io(rel, e))?;
            let name = e.file_name();
            // `symlink_metadata`: a link's own time, never its target's.
            if let Ok(mtime) = folder.symlink_metadata(&name).and_then(|m| m.modified()) {
                entries.push((mtime.into_std(), name));
            }
        }
        if entries.len() <= keep {
            return Ok(());
        }
        entries.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
        let mut left = WALK_MAX;
        for (_, name) in entries.into_iter().skip(keep) {
            remove_entry(&folder, &name, &mut left).map_err(|e| io(rel, e))?;
        }
        Ok(())
    }

    /// The parent folder of `rel` beneath `root`, made on the way (never
    /// through a link), and `rel`'s last component.
    pub(super) fn parent_of(root: &Dir, rel: &Path) -> Result<(Dir, OsString), String> {
        let mut parts: Vec<&OsStr> = rel
            .components()
            .map(|c| match c {
                Component::Normal(n) => Ok(n),
                _ => Err(io(rel, "not a plain relative path")),
            })
            .collect::<Result<_, _>>()?;
        let name = parts
            .pop()
            .ok_or_else(|| io(rel, "names the folder itself"))?
            .to_os_string();
        let mut dir = root.try_clone().map_err(|e| io(rel, e))?;
        for part in parts {
            match dir.create_dir(part) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io(rel, e)),
            }
            dir = dir.open_dir_nofollow(part).map_err(|e| io(rel, e))?;
        }
        Ok((dir, name))
    }

    fn root_dir(dir: &Path) -> Result<Dir, String> {
        crate::download::cap_dir(&open_root(dir)?).map_err(|e| format!("the output folder: {e}"))
    }

    pub(crate) fn make_dir(dir: &Path, rel: &Path) -> Result<(), String> {
        let root = root_dir(dir)?;
        if rel.as_os_str().is_empty() {
            return Ok(());
        }
        let (parent, name) = parent_of(&root, rel)?;
        match parent.create_dir(&name) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(rel, e)),
        }
        // It must be a folder, not a link someone left there.
        parent
            .open_dir_nofollow(&name)
            .map(drop)
            .map_err(|e| io(rel, e))
    }

    fn create_options(new: bool) -> OpenOptions {
        let mut options = OpenOptions::new();
        options.write(true).follow(FollowSymlinks::No);
        if new {
            options.create_new(true);
        } else {
            options.create(true).truncate(true);
        }
        options
    }

    pub(crate) fn create_file(dir: &Path, rel: &Path) -> Result<File, String> {
        let root = root_dir(dir)?;
        let (parent, name) = parent_of(&root, rel)?;
        parent
            .open_with(&name, &create_options(false))
            .map(cap_std::fs::File::into_std)
            .map_err(|e| io(rel, e))
    }

    /// Write `bytes` beside `name` under a temporary name and rename it into
    /// place; `shown` names the target in errors.
    fn write_renamed(
        parent: &Dir,
        name: &OsStr,
        from: &mut dyn Read,
        shown: &dyn Fn(&dyn std::fmt::Display) -> String,
    ) -> Result<u64, String> {
        let tmp = format!(
            ".{}.{}.tmp",
            name.to_string_lossy(),
            &chimaera_core::generate_token()[..8]
        );
        let mut file = parent
            .open_with(&tmp, &create_options(true))
            .map(cap_std::fs::File::into_std)
            .map_err(|e| shown(&e))?;
        let written = std::io::copy(from, &mut file).and_then(|n| {
            file.flush()?;
            Ok(n)
        });
        drop(file);
        let n = match written {
            Ok(n) => n,
            Err(err) => {
                let _ = parent.remove_file(&tmp);
                return Err(shown(&err));
            }
        };
        // A folder where the file goes is refused by the rename; a link
        // there is replaced or refused, never written through.
        parent.rename(&tmp, parent, name).map_err(|e| {
            let _ = parent.remove_file(&tmp);
            shown(&e)
        })?;
        Ok(n)
    }

    pub(crate) fn write(dir: &Path, rel: &Path, bytes: &[u8]) -> Result<(), String> {
        fits(rel, bytes.len())?;
        let root = root_dir(dir)?;
        let (parent, name) = parent_of(&root, rel)?;
        write_renamed(
            &parent,
            &name,
            &mut &bytes[..],
            &|e: &dyn std::fmt::Display| io(rel, e),
        )
        .map(drop)
    }

    pub(crate) fn copy_out(
        dir: &Path,
        rel: &Path,
        dst: &Path,
        dst_rel: &Path,
        replace: bool,
    ) -> Result<u64, String> {
        let root = open_root(dir)?;
        let mut src = crate::download::open_beneath(&root, rel, crate::download::Beneath::Any)
            .map_err(|e| io(rel, e))?;
        if !src.metadata().map_err(|e| io(rel, e))?.is_file() {
            return Err(io(rel, "not a regular file"));
        }
        let dst_root = crate::download::open_dir_path(dst)
            .and_then(|f| crate::download::cap_dir(&f))
            .map_err(|e| format!("the workspace root: {e}"))?;
        let (parent, name) = parent_of(&dst_root, dst_rel)?;
        let shown = |e: &dyn std::fmt::Display| format!("{}: {e}", dst_rel.display());
        match parent.symlink_metadata(&name) {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
                return Err(shown(&"something other than a file is there"))
            }
            Ok(_) if !replace => return Err(format!("{} already exists", dst_rel.display())),
            _ => {}
        }
        write_renamed(&parent, &name, &mut (&mut src).take(QUOTA), &shown)
    }

    pub(super) fn remove_entry(
        parent: &Dir,
        name: &OsStr,
        left: &mut usize,
    ) -> std::io::Result<()> {
        let meta = match parent.symlink_metadata(name) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            // The link itself (a junction is a directory entry), never what
            // it points at.
            return parent.remove_file_or_symlink(name);
        }
        if meta.is_dir() {
            let dir = parent.open_dir_nofollow(name)?;
            empty_folder(&dir, left)?;
            drop(dir);
            parent.remove_dir(name)
        } else {
            parent.remove_file(name)
        }
    }

    pub(super) fn empty_folder(dir: &Dir, left: &mut usize) -> std::io::Result<()> {
        let names: Vec<OsString> = dir
            .entries()?
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        for name in names {
            if *left == 0 {
                return Err(std::io::Error::other("too many entries to remove"));
            }
            *left -= 1;
            remove_entry(dir, &name, left)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
pub(crate) use win::{copy_out, create_file, list, make_dir, prune_oldest, write};
#[cfg(windows)]
use win::{empty_folder, parent_of, remove_entry};

/// Bytes under `dir`, walking at most `WALK_MAX` entries, never through a
/// link (blocking); also each top-level entry of each workspace folder, for
/// eviction: (path, bytes, newest mtime).
fn measure(dir: &Path) -> (u64, Vec<(PathBuf, u64, SystemTime)>) {
    let mut total = 0;
    let mut tops = Vec::new();
    let mut left = WALK_MAX;
    let Ok(workspaces) = std::fs::read_dir(dir) else {
        return (0, tops);
    };
    for ws in workspaces.flatten() {
        let Ok(entries) = std::fs::read_dir(ws.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let (bytes, newest) = walk(&entry.path(), &mut left);
            total += bytes;
            tops.push((entry.path(), bytes, newest));
        }
    }
    (total, tops)
}

fn walk(path: &Path, left: &mut usize) -> (u64, SystemTime) {
    if *left == 0 {
        return (0, SystemTime::UNIX_EPOCH);
    }
    *left -= 1;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return (0, SystemTime::UNIX_EPOCH);
    };
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    if !meta.is_dir() {
        return (meta.len(), mtime);
    }
    let mut bytes = 0;
    let mut newest = mtime;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let (b, m) = walk(&entry.path(), left);
            bytes += b;
            newest = newest.max(m);
        }
    }
    (bytes, newest)
}

impl Usage {
    /// `plugin`'s usage, measured again when older than `USAGE_TTL`; past
    /// `quota`, its oldest top-level entries are removed until it fits
    /// (blocking).
    pub(crate) fn enforce(root: &Path, plugin: &str, quota: u64) -> u64 {
        let dir = root.join(plugin);
        let (mut total, mut tops) = measure(&dir);
        if total > quota {
            tops.sort_by_key(|(_, _, newest)| *newest);
            for (path, bytes, _) in tops {
                if total <= quota {
                    break;
                }
                let removed = if path.is_dir() && !path.is_symlink() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                match removed {
                    Ok(()) => {
                        total = total.saturating_sub(bytes);
                        tracing::info!(%plugin, path = %path.display(), "plugin output evicted (over its quota)");
                    }
                    Err(err) => tracing::warn!(%plugin, %err, "plugin output eviction failed"),
                }
            }
        }
        total
    }

    /// The last measured usage if fresh.
    fn fresh(&self, plugin: &str) -> Option<u64> {
        self.by_plugin
            .get(plugin)
            .filter(|(_, at)| at.elapsed() < USAGE_TTL)
            .map(|(b, _)| *b)
    }

    /// Count `n` bytes about to be written against the fresh figure: true
    /// when they fit under `quota` (and are now counted). A missing or stale
    /// figure, or one they would push past `quota`, is false: the caller
    /// measures again.
    fn charge(&mut self, plugin: &str, n: u64, quota: u64) -> bool {
        match self.by_plugin.get_mut(plugin) {
            Some((bytes, at)) if at.elapsed() < USAGE_TTL && bytes.saturating_add(n) <= quota => {
                *bytes += n;
                true
            }
            _ => false,
        }
    }

    fn record(&mut self, plugin: &str, bytes: u64) {
        self.by_plugin
            .insert(plugin.to_string(), (bytes, Instant::now()));
    }

    fn forget(&mut self, plugin: &str) {
        self.by_plugin.remove(plugin);
    }
}

/// Measure (and enforce the quota on) `plugin`'s output unless measured
/// within the last minute; its bytes.
pub(crate) async fn usage(state: &AppState, plugin: &str, force: bool) -> u64 {
    if !force {
        if let Some(bytes) = crate::lock(&state.plugin_platform.usage).fresh(plugin) {
            return bytes;
        }
    }
    let root = state.plugin_platform.output_root.clone();
    let id = plugin.to_string();
    let bytes = tokio::task::spawn_blocking(move || Usage::enforce(&root, &id, QUOTA))
        .await
        .unwrap_or(0);
    crate::lock(&state.plugin_platform.usage).record(plugin, bytes);
    bytes
}

/// Charge `n` bytes `plugin` is about to write against its quota: within
/// the fresh figure they just count; otherwise it is measured again, its
/// oldest entries evicted to leave room for them, and refused when even
/// that leaves no room. Without this a plugin could write 8 MiB a call for
/// a whole minute between measures.
pub(crate) async fn charge(state: &AppState, plugin: &str, n: u64) -> Result<(), String> {
    charge_within(state, plugin, n, QUOTA).await
}

pub(crate) async fn charge_within(
    state: &AppState,
    plugin: &str,
    n: u64,
    quota: u64,
) -> Result<(), String> {
    if crate::lock(&state.plugin_platform.usage).charge(plugin, n, quota) {
        return Ok(());
    }
    // Never evict everything for a write that could not fit anyway.
    if n > quota {
        return Err(format!(
            "{n} bytes is more than this plugin's output quota ({} MiB)",
            quota >> 20
        ));
    }
    let root = state.plugin_platform.output_root.clone();
    let id = plugin.to_string();
    let room = quota.saturating_sub(n);
    let measured = tokio::task::spawn_blocking(move || Usage::enforce(&root, &id, room))
        .await
        .map_err(|e| format!("measuring the output folder failed: {e}"))?;
    let mut usage = crate::lock(&state.plugin_platform.usage);
    usage.record(plugin, measured);
    if usage.charge(plugin, n, quota) {
        Ok(())
    } else {
        Err(format!(
            "this plugin's output folders are full ({} of {} MiB)",
            measured >> 20,
            quota >> 20
        ))
    }
}

/// A removed plugin: every output folder it had (blocking).
pub(crate) fn forget_plugin(state: &AppState, plugin: &str) {
    crate::lock(&state.plugin_platform.usage).forget(plugin);
    let dir = state.plugin_platform.output_root.join(plugin);
    if dir.exists() && !dir.is_symlink() {
        if let Err(err) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(%plugin, %err, "plugin output not removed");
        }
    }
}

/// A deleted workspace: every plugin's output folder for it (blocking).
pub(crate) fn forget_workspace(root: &Path, ws: &str) {
    let Ok(plugins) = std::fs::read_dir(root) else {
        return;
    };
    let name = crate::timeline::sanitize(ws);
    for plugin in plugins.flatten() {
        let dir = plugin.path().join(&name);
        if dir.exists() && !dir.is_symlink() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// `GET /plugins/{pid}/output`: the plugin's use of its output folders
/// (Settings → Plugins shows it beside **Clear**).
pub(crate) async fn usage_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    if super::manifest(&state, &pid).is_none() {
        return super::not_found(&format!("plugin {pid}"));
    }
    let bytes = usage(&state, &pid, false).await;
    Json(json!({"bytes": bytes, "quota": QUOTA})).into_response()
}

/// `DELETE /plugins/{pid}/output`: Clear — every output folder it has.
pub(crate) async fn clear_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    if !super::valid_id(&pid) {
        return super::not_found(&format!("plugin {pid}"));
    }
    let root = state.plugin_platform.output_root.join(&pid);
    let cleared = tokio::task::spawn_blocking(move || remove(&root, Path::new(""))).await;
    crate::lock(&state.plugin_platform.usage).forget(&pid);
    match cleared {
        Ok(Ok(())) => Json(json!({"bytes": 0, "quota": QUOTA})).into_response(),
        Ok(Err(err)) => super::bad_request(err),
        Err(err) => super::bad_request(format!("clearing failed: {err}")),
    }
}

/// `GET /workspaces/{id}/plugins/{pid}/output`: the folder's absolute
/// path, so the UI resolves `output:<path>` for its file views.
pub(crate) async fn folder_route(
    State(state): State<Arc<AppState>>,
    AxPath((ws, pid)): AxPath<(String, String)>,
) -> Response {
    if crate::lock(&state.workspaces).get(&ws).is_none() {
        return super::not_found(&format!("workspace {ws}"));
    }
    if super::manifest(&state, &pid).is_none() {
        return super::not_found(&format!("plugin {pid}"));
    }
    let dir = folder(&state.plugin_platform.output_root, &pid, &ws);
    let made = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || open_root(&dir).map(drop)).await
    };
    if let Ok(Err(err)) = made {
        return super::bad_request(err);
    }
    Json(json!({"root": chimaera_core::paths::wire_path(&dir)})).into_response()
}

#[derive(serde::Deserialize)]
pub(crate) struct SaveBody {
    /// `output:<path>`.
    from: String,
    /// Workspace-relative.
    to: String,
    #[serde(default)]
    replace: bool,
}

/// `POST /workspaces/{id}/plugins/{pid}/output/save {from, to, replace?}`:
/// **Save to workspace** — the user's click copies an output file into the
/// workspace (a plugin can't write there itself).
pub(crate) async fn save_route(
    State(state): State<Arc<AppState>>,
    AxPath((ws, pid)): AxPath<(String, String)>,
    Json(body): Json<SaveBody>,
) -> Response {
    let Some(root) = crate::lock(&state.workspaces).get(&ws).map(|w| w.root) else {
        return super::not_found(&format!("workspace {ws}"));
    };
    if super::manifest(&state, &pid).is_none() {
        return super::not_found(&format!("plugin {pid}"));
    }
    let (rel, dst_rel) = match (relative(&body.from), super::hostfns_relative(&body.to)) {
        (Ok(a), Ok(b)) if !b.as_os_str().is_empty() => (a, b),
        (Err(e), _) | (_, Err(e)) => return super::bad_request(e),
        _ => return super::bad_request("name the file to save to"),
    };
    // The plugin names where its button saves: never a hidden path, where a
    // file does more than sit there (`.envrc`, `.git/hooks`, `.github/`).
    if hidden(&dst_rel) {
        return super::bad_request(format!(
            "{}: a plugin saves only to visible paths in the workspace",
            body.to
        ));
    }
    let dir = folder(&state.plugin_platform.output_root, &pid, &ws);
    let target = root.join(&dst_rel);
    let copied = {
        let root = root.clone();
        let dst_rel = dst_rel.clone();
        tokio::task::spawn_blocking(move || copy_out(&dir, &rel, &root, &dst_rel, body.replace))
            .await
    };
    match copied {
        Ok(Ok(bytes)) => {
            let target = chimaera_core::paths::wire_path(&target);
            crate::git::mark_path_dirty(&state, &target).await;
            Json(json!({"path": target, "bytes": bytes})).into_response()
        }
        Ok(Err(err)) => super::bad_request(err),
        Err(err) => super::bad_request(format!("saving failed: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-output-test-{}-{label}-{}",
            std::process::id(),
            &chimaera_core::generate_token()[..8]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn pruning_keeps_the_newest_and_never_follows_a_link() {
        let dir = temp("prune");
        let outside = temp("prune-outside");
        std::fs::write(outside.join("keep.txt"), b"x").unwrap();
        let jobs = dir.join(".jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        // The oldest of all: a link out, which goes as a link.
        std::os::unix::fs::symlink(&outside, jobs.join("j-link")).unwrap();
        let now = SystemTime::now();
        for (i, name) in ["j-a", "j-b", "j-c"].iter().enumerate() {
            let f = jobs.join(name);
            std::fs::create_dir(&f).unwrap();
            std::fs::write(f.join("stdout.log"), b"log").unwrap();
            File::open(&f)
                .unwrap()
                .set_modified(now + Duration::from_secs(10 + i as u64 * 10))
                .unwrap();
        }
        prune_oldest(&dir, Path::new(".jobs"), 2).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&jobs)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["j-b", "j-c"]);
        assert!(outside.join("keep.txt").exists());
        // Nothing there yet is fine.
        prune_oldest(&dir, Path::new("none"), 2).unwrap();
    }

    #[test]
    fn a_save_goes_only_to_visible_paths() {
        assert!(hidden(Path::new(".envrc")));
        assert!(hidden(Path::new(".github/workflows/x.yml")));
        assert!(hidden(Path::new("docs/.git/hooks/pre-commit")));
        assert!(!hidden(Path::new("thesis/main.pdf")));
    }

    #[cfg(unix)]
    #[test]
    fn a_plugin_stays_in_its_folder() {
        let dir = temp("output");
        write(&dir, Path::new("a/b.txt"), b"hello").unwrap();
        assert_eq!(read(&dir, Path::new("a/b.txt"), 1, 3).unwrap(), b"ell");
        assert!(relative("../x").is_err());
        assert!(relative("/etc/passwd").is_err());
        assert_eq!(relative("output:a/b").unwrap(), PathBuf::from("a/b"));
        // A link someone left there is never followed.
        std::os::unix::fs::symlink("/etc", dir.join("out")).unwrap();
        assert!(read(&dir, Path::new("out/hostname"), 0, 10).is_err());
        assert!(write(&dir, Path::new("out/x"), b"x").is_err());
        // Writing over the link replaces the link, not what it points to.
        write(&dir, Path::new("out"), b"file now").unwrap();
        assert!(!dir.join("out").is_symlink());
        let listed = list(&dir, Path::new(""), 10).unwrap();
        assert_eq!(listed.len(), 2);
        remove(&dir, Path::new("a")).unwrap();
        assert!(!dir.join("a").exists());
        assert!(write(&dir, Path::new("big"), &vec![0; WRITE_MAX + 1]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_quota_evicts_the_oldest_first() {
        let root = temp("quota");
        let ws = folder(&root, "p", "w-1");
        write(&ws, Path::new("old/a"), &[0; 600]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        write(&ws, Path::new("new"), &[0; 600]).unwrap();
        let old = std::fs::File::options()
            .write(true)
            .open(ws.join("old/a"))
            .unwrap();
        old.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        std::fs::File::open(ws.join("old"))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        assert_eq!(Usage::enforce(&root, "p", 1000), 600);
        assert!(!ws.join("old").exists());
        assert!(ws.join("new").exists());
    }

    /// Windows: a junction someone left in the folder is never read,
    /// listed into, written or removed through, and pruning removes the
    /// junction itself, never what it points at.
    #[cfg(windows)]
    #[test]
    fn a_plugin_stays_in_its_folder_past_a_junction() {
        let dir = temp("output-win");
        let outside = temp("output-win-outside");
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        write(&dir, Path::new("a/b.txt"), b"hello").unwrap();
        assert_eq!(read(&dir, Path::new("a/b.txt"), 1, 3).unwrap(), b"ell");
        write(&dir, Path::new("a/b.txt"), b"again").unwrap();
        assert_eq!(read(&dir, Path::new("a/b.txt"), 0, 10).unwrap(), b"again");
        junction::create(&outside, dir.join("out")).unwrap();
        assert!(read(&dir, Path::new("out/secret.txt"), 0, 10).is_err());
        assert!(read(&dir, Path::new("out"), 0, 10).is_err());
        assert!(list(&dir, Path::new("out"), 10).is_err());
        assert!(write(&dir, Path::new("out/x"), b"x").is_err());
        assert!(create_file(&dir, Path::new("out/log")).is_err());
        assert!(create_file(&dir, Path::new("out")).is_err());
        assert!(make_dir(&dir, Path::new("out/sub")).is_err());
        assert!(make_dir(&dir, Path::new("out")).is_err());
        let listed = list(&dir, Path::new(""), 10).unwrap();
        let out = listed.iter().find(|e| e.name == "out").unwrap();
        assert!(out.is_symlink && !out.is_dir);
        // Save to workspace: never through a junction on the far side.
        let ws = temp("output-win-ws");
        junction::create(&outside, ws.join("docs")).unwrap();
        assert!(copy_out(
            &dir,
            Path::new("a/b.txt"),
            &ws,
            Path::new("docs/b.txt"),
            true
        )
        .is_err());
        assert_eq!(
            copy_out(
                &dir,
                Path::new("a/b.txt"),
                &ws,
                Path::new("saved/b.txt"),
                false
            )
            .unwrap(),
            5
        );
        assert!(copy_out(
            &dir,
            Path::new("a/b.txt"),
            &ws,
            Path::new("saved/b.txt"),
            false
        )
        .is_err());
        remove(&dir, Path::new("out")).unwrap();
        assert!(!dir.join("out").exists());
        junction::create(&outside, dir.join("jobs-link")).unwrap();
        prune_oldest(&dir, Path::new(""), 0).unwrap();
        remove(&dir, Path::new("")).unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert!(outside.join("secret.txt").exists());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    }

    #[test]
    fn a_write_is_charged_against_the_fresh_figure_until_it_would_pass_the_quota() {
        let mut usage = Usage::default();
        // Nothing measured yet: the caller measures first.
        assert!(!usage.charge("p", 10, 1000));
        usage.record("p", 600);
        assert!(usage.charge("p", 300, 1000));
        assert_eq!(usage.fresh("p"), Some(900));
        // Past the quota: not counted, measured again.
        assert!(!usage.charge("p", 200, 1000));
        assert_eq!(usage.fresh("p"), Some(900));
        assert!(usage.charge("p", 100, 1000));
        // A stale figure is never trusted.
        usage
            .by_plugin
            .insert("q".into(), (0, Instant::now() - USAGE_TTL));
        assert!(!usage.charge("q", 1, 1000));
    }
}
