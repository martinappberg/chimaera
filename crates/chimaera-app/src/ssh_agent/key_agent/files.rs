//! Native-selected private files are pinned before any unlock prompt. Only a
//! bounded, unchanged snapshot reaches the task-private loader's stdin.
use super::super::selection::SelectionFailure;
use std::{
    ffi::{CString, OsStr},
    fs::{File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Component, Path, PathBuf},
};
use zeroize::Zeroizing;

const MAX_KEY: u64 = 1024 * 1024;
type Result<T> = std::result::Result<T, SelectionFailure>;
#[derive(PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    len: u64,
    mode: u32,
    uid: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Identity {
    fn new(stat: &Metadata) -> Self {
        Self {
            dev: stat.dev(),
            ino: stat.ino(),
            len: stat.len(),
            mode: stat.mode(),
            uid: stat.uid(),
            modified: (stat.mtime(), stat.mtime_nsec()),
            changed: (stat.ctime(), stat.ctime_nsec()),
        }
    }
}
pub(super) struct Captured {
    path: PathBuf,
    parent: File,
    name: CString,
    file: File,
    identity: Identity,
    limit: u64,
}
fn directory(path: &Path) -> Result<File> {
    if !path.is_absolute() || path.as_os_str().len() > 4096 {
        return Err(SelectionFailure::UnsupportedConfiguration);
    }
    let mut dir = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open("/")
        .map_err(|_| SelectionFailure::Unavailable)?;
    let mut count = 0;
    for part in path.components() {
        let part = match part {
            Component::RootDir => continue,
            Component::Normal(part) => part,
            _ => return Err(SelectionFailure::UnsupportedConfiguration),
        };
        count += 1;
        if count > 128 {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        let name = CString::new(part.as_bytes())
            .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
        let fd = unsafe {
            nix::libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                nix::libc::O_RDONLY
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                    SelectionFailure::NoKeys
                } else {
                    SelectionFailure::Unavailable
                },
            );
        }
        dir = unsafe { File::from_raw_fd(fd) };
    }
    Ok(dir)
}
impl Captured {
    pub(super) fn capture(path: PathBuf) -> Result<Option<Self>> {
        Self::capture_inner(path, true)
    }
    pub(super) fn certificate(path: PathBuf) -> Result<Option<Self>> {
        Self::capture_inner(path, false)
    }
    fn capture_inner(path: PathBuf, private: bool) -> Result<Option<Self>> {
        let parent_path = path
            .parent()
            .ok_or(SelectionFailure::UnsupportedConfiguration)?;
        let name = CString::new(
            path.file_name()
                .ok_or(SelectionFailure::UnsupportedConfiguration)?
                .as_bytes(),
        )
        .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
        let parent = match directory(parent_path) {
            Err(SelectionFailure::NoKeys) => return Ok(None),
            result => result?,
        };
        let fd = unsafe {
            nix::libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                nix::libc::O_RDONLY
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK
                    | nix::libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(SelectionFailure::Unavailable)
            };
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let stat = file.metadata().map_err(|_| SelectionFailure::Unavailable)?;
        let limit = if private { MAX_KEY } else { 32 * 1024 };
        let owner = unsafe { nix::libc::geteuid() };
        if !stat.is_file()
            || (stat.uid() != owner && (private || stat.uid() != 0))
            || stat.mode() & if private { 0o077 } else { 0o022 } != 0
            || stat.len() == 0
            || stat.len() > limit
        {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        let captured = Self {
            path,
            parent,
            name,
            file,
            identity: Identity::new(&stat),
            limit,
        };
        captured.check()?;
        Ok(Some(captured))
    }
    pub(super) fn label(&self) -> &OsStr {
        self.path.file_name().unwrap_or_default()
    }
    pub(super) fn check(&self) -> Result<()> {
        let current_parent = directory(self.path.parent().ok_or(SelectionFailure::Unavailable)?)?;
        let old_parent = self
            .parent
            .metadata()
            .map_err(|_| SelectionFailure::Unavailable)?;
        let new_parent = current_parent
            .metadata()
            .map_err(|_| SelectionFailure::Unavailable)?;
        if old_parent.dev() != new_parent.dev() || old_parent.ino() != new_parent.ino() {
            return Err(SelectionFailure::Unavailable);
        }
        let fd = unsafe {
            nix::libc::openat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                nix::libc::O_RDONLY
                    | nix::libc::O_NONBLOCK
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(SelectionFailure::Unavailable);
        }
        let entry = unsafe { File::from_raw_fd(fd) };
        let entry = entry
            .metadata()
            .map_err(|_| SelectionFailure::Unavailable)?;
        let stat = self
            .file
            .metadata()
            .map_err(|_| SelectionFailure::Unavailable)?;
        if !entry.is_file()
            || Identity::new(&entry) != self.identity
            || Identity::new(&stat) != self.identity
        {
            return Err(SelectionFailure::Unavailable);
        }
        Ok(())
    }
    pub(super) fn snapshot(&mut self) -> Result<Zeroizing<Vec<u8>>> {
        self.check()?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| SelectionFailure::Unavailable)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(self.limit as usize + 1));
        self.file
            .by_ref()
            .take(self.limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| SelectionFailure::Unavailable)?;
        if bytes.len() as u64 > self.limit || bytes.len() as u64 != self.identity.len {
            return Err(SelectionFailure::Unavailable);
        }
        self.check()?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            let root = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "cx-key-capture-{}",
                    &chimaera_core::generate_token()[..24]
                ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            Self(root)
        }
        fn key(&self) -> PathBuf {
            let path = self.0.join("selected");
            std::fs::write(&path, b"synthetic captured bytes").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            path
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn capture_bounds_modes_and_both_inode_and_in_place_changes() {
        let root = Root::new();
        let path = root.key();
        let mut captured = Captured::capture(path.clone()).unwrap().unwrap();
        assert_eq!(&*captured.snapshot().unwrap(), b"synthetic captured bytes");
        assert_eq!(captured.label(), "selected");
        assert!(Captured::capture(root.0.join("missing")).unwrap().is_none());
        assert!(Captured::capture(root.0.join("absent-parent/key"))
            .unwrap()
            .is_none());
        std::fs::write(&path, b"different selected bytes").unwrap();
        assert!(captured.snapshot().is_err());
        let path = root.key();
        let captured = Captured::capture(path.clone()).unwrap().unwrap();
        std::fs::rename(&path, root.0.join("original")).unwrap();
        root.key();
        assert!(captured.check().is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Captured::capture(path).is_err());
        let path = root.key();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_KEY + 1)
            .unwrap();
        assert!(Captured::capture(path).is_err());
    }
    #[test]
    fn capture_refuses_link_device_fifo_and_replaced_ancestor() {
        let root = Root::new();
        let path = root.key();
        let alias = root.0.join("alias");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(Captured::capture(alias).is_err());
        assert!(Captured::capture(PathBuf::from("/dev/null")).is_err());
        let fifo = root.0.join("fifo");
        let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { nix::libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(Captured::capture(fifo).is_err());
        let captured = Captured::capture(path).unwrap().unwrap();
        let displaced = root.0.with_extension("displaced");
        std::fs::rename(&root.0, &displaced).unwrap();
        std::os::unix::fs::symlink(&displaced, &root.0).unwrap();
        assert!(captured.check().is_err());
        std::fs::remove_file(&root.0).unwrap();
        std::fs::rename(displaced, &root.0).unwrap();
    }
}
