//! Moving a copy the user discards to the Trash rather than deleting it (the
//! kept-version review's "Use the cloud's"). The copy leaves its folder by a
//! rename through that folder's descriptor, never by path: the `O_NOFOLLOW`
//! walk that found it (`kept::open_dir`) decides what moves, and a rename
//! never follows the name it moves. Nothing is copied, and a rename cannot
//! cross drives, so a copy goes to a Trash on its own drive: the home Trash
//! (`~/.Trash` on macOS, the freedesktop.org home trash elsewhere), or, for a
//! folder on another drive, that drive's own when it already has one
//! (`.Trashes/<uid>` on macOS, `.Trash/<uid>` or `.Trash-<uid>` elsewhere; a
//! Trash is never started at the top of a shared or network drive). With none
//! the copy is deleted, and the caller says so.
//!
//! Nothing here lists a Trash: macOS keeps `~/.Trash` unreadable without Full
//! Disk Access, but a rename into it by name works. A name already in a Trash
//! is never replaced; the copy takes "<name> 2", "<name> 3", … instead.
use rustix::{
    fs::{AtFlags, RenameFlags, CWD},
    io::Errno,
};
use std::{
    fmt::Write as _,
    fs::File,
    io::Write as _,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

/// Names tried before a Trash counts as unable to take the copy.
const ATTEMPTS: u32 = 100;
/// Room under the 255-byte file-name limit for `.trashinfo` and a " <n>".
const MAX_NAME: usize = 240;

/// Where a discarded copy went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Discarded {
    /// Into a Trash, where it can be taken back.
    Trash,
    /// Deleted: no Trash on its drive could take it.
    Deleted,
}

/// This user's home Trash: `~/.Trash` on macOS; elsewhere the freedesktop.org
/// home trash, `$XDG_DATA_HOME/Trash` (by default `~/.local/share/Trash`).
/// `None` without an absolute home.
pub(super) fn home() -> Option<PathBuf> {
    let absolute = |value: std::ffi::OsString| {
        let path = PathBuf::from(value);
        path.is_absolute().then_some(path)
    };
    let home = std::env::var_os("HOME").and_then(absolute)?;
    if cfg!(target_os = "macos") {
        return Some(home.join(".Trash"));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .and_then(absolute)
        .unwrap_or_else(|| home.join(".local/share"));
    Some(data.join("Trash"))
}

/// Whether a copy discarded in the project folder `root` would go to a Trash
/// (`false`: it would be deleted). `home` is [`home`], or a test's fixture;
/// without one there is no Trash at all.
pub(super) fn available(root: &Path, home: Option<&Path>) -> bool {
    std::fs::metadata(root).is_ok_and(|meta| !bins(root, home, meta.dev()).is_empty())
}

/// Move `name` out of `dir` (a folder inside the project folder `root`) into
/// a Trash on its drive, or delete it when none can take it. `origin` is
/// where it was, which a freedesktop.org Trash records so it can be put back.
pub(super) fn discard(
    dir: &File,
    name: &str,
    origin: &Path,
    root: &Path,
    home: Option<&Path>,
) -> std::io::Result<Discarded> {
    let device = dir.metadata()?.dev();
    for bin in bins(root, home, device) {
        match into(dir, name, origin, &bin) {
            Ok(()) => return Ok(Discarded::Trash),
            // Another drive after all (a bind mount), or a Trash that cannot
            // take it: try the next one.
            Err(error) => tracing::debug!(%error, "a Trash could not take a discarded copy"),
        }
    }
    rustix::fs::unlinkat(dir, name, AtFlags::empty())?;
    Ok(Discarded::Deleted)
}

/// A Trash a copy can go to.
struct Bin {
    path: PathBuf,
    /// This user's own folder inside a drive's shared Trash (`.Trashes/<uid>`,
    /// `.Trash/<uid>`), made on first use; it must be a real folder of this
    /// user's.
    personal: bool,
}

fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// A real folder (not a link) that belongs to this user.
fn own_folder(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir() && meta.uid() == uid())
}

/// The Trashes, in order, that could take a file on `device` inside the
/// project folder `root`. Nothing is created here.
fn bins(root: &Path, home: Option<&Path>, device: u64) -> Vec<Bin> {
    let Some(home) = home else {
        return Vec::new();
    };
    let mut bins = Vec::new();
    // The home Trash, when it is on the same drive. macOS keeps its own; the
    // freedesktop.org one is made on first use, so its nearest existing
    // folder decides the drive.
    let existing = home
        .ancestors()
        .find_map(|dir| Some((dir, std::fs::metadata(dir).ok()?)));
    if let Some((dir, meta)) = existing {
        if meta.is_dir() && meta.dev() == device && (dir == home || !cfg!(target_os = "macos")) {
            bins.push(Bin {
                path: home.to_path_buf(),
                personal: false,
            });
        }
    }
    let Some(top) = top(root, device) else {
        return bins;
    };
    let uid = uid();
    if cfg!(target_os = "macos") {
        if std::fs::symlink_metadata(top.join(".Trashes")).is_ok_and(|meta| meta.is_dir()) {
            bins.push(Bin {
                path: top.join(".Trashes").join(uid.to_string()),
                personal: true,
            });
        }
    } else {
        // An administrator's shared `.Trash` counts only as the specification
        // allows: a real folder with the sticky bit.
        let shared = top.join(".Trash");
        if std::fs::symlink_metadata(&shared)
            .is_ok_and(|meta| meta.is_dir() && meta.mode() & 0o1000 != 0)
        {
            bins.push(Bin {
                path: shared.join(uid.to_string()),
                personal: true,
            });
        }
        let own = top.join(format!(".Trash-{uid}"));
        if own_folder(&own) {
            bins.push(Bin {
                path: own,
                personal: false,
            });
        }
    }
    bins
}

/// The top folder of the drive `root` is on (the highest one on `device`).
fn top(root: &Path, device: u64) -> Option<PathBuf> {
    let same = |dir: &Path| std::fs::metadata(dir).is_ok_and(|meta| meta.dev() == device);
    if !same(root) {
        return None;
    }
    let mut top = root;
    while let Some(parent) = top.parent().filter(|parent| same(parent)) {
        top = parent;
    }
    Some(top.to_path_buf())
}

fn into(dir: &File, name: &str, origin: &Path, bin: &Bin) -> std::io::Result<()> {
    if bin.personal {
        if let Err(error) = std::fs::DirBuilder::new().mode(0o700).create(&bin.path) {
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        if !own_folder(&bin.path) {
            return Err(std::io::Error::other("not this user's Trash"));
        }
    }
    if cfg!(target_os = "macos") {
        for attempt in 0..ATTEMPTS {
            match rename_new(dir, name, &bin.path.join(candidate(name, attempt))) {
                Err(Errno::EXIST) => continue,
                other => return other.map_err(Into::into),
            }
        }
        return Err(std::io::Error::other("no free name in the Trash"));
    }
    // freedesktop.org: the file in `files/`, where it came from in
    // `info/<name>.trashinfo`, written first; creating it reserves the name.
    let files = bin.path.join("files");
    let info = bin.path.join("info");
    for folder in [&files, &info] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(folder)?;
    }
    let record = trashinfo(origin);
    for attempt in 0..ATTEMPTS {
        let taken = candidate(name, attempt);
        let info_path = info.join(format!("{taken}.trashinfo"));
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&info_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let moved = file
            .write_all(record.as_bytes())
            .and_then(|()| file.sync_all())
            .and_then(|()| rename_new(dir, name, &files.join(&taken)).map_err(Into::into));
        match moved {
            Ok(()) => return Ok(()),
            Err(error) => {
                let _ = std::fs::remove_file(&info_path);
                if error.raw_os_error() != Some(Errno::EXIST.raw_os_error()) {
                    return Err(error);
                }
            }
        }
    }
    Err(std::io::Error::other("no free name in the Trash"))
}

/// Rename `name` in `dir` to `target`, never replacing anything there.
fn rename_new(dir: &File, name: &str, target: &Path) -> rustix::io::Result<()> {
    match rustix::fs::renameat_with(dir, name, CWD, target, RenameFlags::NOREPLACE) {
        // A drive without an exclusive rename (NFS, some network shares): the
        // name was reserved or found free just before; check once more.
        Err(error) if unsupported(error) => {
            if std::fs::symlink_metadata(target).is_ok() {
                return Err(Errno::EXIST);
            }
            rustix::fs::renameat(dir, name, CWD, target)
        }
        other => other,
    }
}

fn unsupported(error: Errno) -> bool {
    error == Errno::INVAL
        || error == Errno::NOTSUP
        || error == Errno::OPNOTSUPP
        || error == Errno::NOSYS
}

/// The name a copy takes in the Trash: its own, then "<name> 2", "<name> 3",
/// …, shortened when needed to stay under the file-name limit.
fn candidate(name: &str, attempt: u32) -> String {
    let suffix = if attempt == 0 {
        String::new()
    } else {
        format!(" {}", attempt + 1)
    };
    let mut cut = name.len().min(MAX_NAME - suffix.len());
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{suffix}", &name[..cut])
}

/// A freedesktop.org `.trashinfo` record: the original path, URL-escaped,
/// and when it was discarded (local time).
fn trashinfo(origin: &Path) -> String {
    let mut path = String::new();
    for byte in origin.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(byte) {
            path.push(char::from(*byte));
        } else {
            let _ = write!(path, "%{byte:02X}");
        }
    }
    let date = super::canonical::local_now().map_or_else(
        || "1970-01-01T00:00:00".to_owned(),
        |tm| {
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            )
        },
    );
    format!("[Trash Info]\nPath={path}\nDeletionDate={date}\n")
}

/// A home Trash under `home_parent`, as this platform keeps it: macOS's must
/// already exist, the freedesktop.org one is made on first use.
#[cfg(test)]
pub(super) fn fixture_home_trash(home_parent: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        let trash = home_parent.join("home/.Trash");
        std::fs::create_dir_all(&trash).unwrap();
        trash
    } else {
        home_parent.join("home/.local/share/Trash")
    }
}

/// Where a copy named `name` sits in the Trash at `trash`.
#[cfg(test)]
pub(super) fn in_trash(trash: &Path, name: &str) -> PathBuf {
    if cfg!(target_os = "macos") {
        trash.join(name)
    } else {
        trash.join("files").join(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Folder(PathBuf);
    impl Folder {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "chimaera-trash-{}",
                chimaera_core::generate_token()
            ));
            std::fs::create_dir_all(path.join("project/sub")).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_discarded_copy_goes_to_the_home_trash_without_replacing_anything() {
        let folder = Folder::new();
        let root = folder.0.join("project");
        let trash = fixture_home_trash(&folder.0);
        let name = "notes.md.mine-20260929-1412";
        let dir = File::open(root.join("sub")).unwrap();
        let origin = root.join("sub").join(name);
        for body in ["first", "second"] {
            std::fs::write(&origin, body).unwrap();
            assert_eq!(
                discard(&dir, name, &origin, &root, Some(&trash)).unwrap(),
                Discarded::Trash
            );
            assert!(!origin.exists());
        }
        assert!(available(&root, Some(&trash)));
        assert_eq!(
            std::fs::read_to_string(in_trash(&trash, name)).unwrap(),
            "first"
        );
        assert_eq!(
            std::fs::read_to_string(in_trash(&trash, &format!("{name} 2"))).unwrap(),
            "second"
        );
        if !cfg!(target_os = "macos") {
            let info =
                std::fs::read_to_string(trash.join("info").join(format!("{name}.trashinfo")))
                    .unwrap();
            assert!(info.starts_with("[Trash Info]\nPath="), "{info}");
            assert!(
                info.contains(&format!("Path={}\n", origin.display())),
                "{info}"
            );
            assert!(info.contains("\nDeletionDate=20"), "{info}");
        }
    }

    #[test]
    fn without_a_trash_the_copy_is_deleted_and_says_so() {
        let folder = Folder::new();
        let root = folder.0.join("project");
        let dir = File::open(&root).unwrap();
        let name = "a.mine-20260929-1412";
        std::fs::write(root.join(name), "mine").unwrap();
        assert!(!available(&root, None));
        assert_eq!(
            discard(&dir, name, &root.join(name), &root, None).unwrap(),
            Discarded::Deleted
        );
        assert!(!root.join(name).exists());
        // A copy that is already gone is an error, as before.
        assert!(discard(&dir, name, &root.join(name), &root, None).is_err());
    }

    #[test]
    fn names_fit_and_paths_are_escaped() {
        assert_eq!(candidate("a.mine-20260929-1412", 0), "a.mine-20260929-1412");
        assert_eq!(
            candidate("a.mine-20260929-1412", 1),
            "a.mine-20260929-1412 2"
        );
        let long = "é".repeat(200);
        for attempt in [0, 1, 99] {
            let name = candidate(&long, attempt);
            assert!(name.len() <= MAX_NAME, "{}", name.len());
        }
        let record = trashinfo(Path::new("/home/me/My Project/ünï.txt"));
        assert!(
            record.contains("Path=/home/me/My%20Project/%C3%BCn%C3%AF.txt\n"),
            "{record}"
        );
    }
}
