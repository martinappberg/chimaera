//! Append-only native host trust. Caller holds its original Connect/account
//! commit admission and supplies a positively verified, explicitly approved key.
//! A failed post-append check refuses the grant; it never rolls back user trust.
use super::super::selection::SelectionFailure;
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

type Result<T> = std::result::Result<T, SelectionFailure>;
const MAX_FILE: usize = 1024 * 1024;

fn refused() -> SelectionFailure {
    SelectionFailure::UnsupportedConfiguration
}
fn name(value: &std::ffi::OsStr) -> Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| refused())
}
fn open_at(parent: i32, leaf: &CString, flags: i32, mode: u32) -> Result<OwnedFd> {
    let fd = unsafe { nix::libc::openat(parent, leaf.as_ptr(), flags, mode) };
    if fd < 0 {
        return Err(refused());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
fn directory(path: &Path) -> Result<OwnedFd> {
    if !path.is_absolute() || path.as_os_str().len() > 4096 {
        return Err(refused());
    }
    let flags =
        nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC;
    let mut dir = open_at(nix::libc::AT_FDCWD, &CString::new("/").unwrap(), flags, 0)?;
    let mut count = 0;
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(part) => {
                count += 1;
                if count > 64 {
                    return Err(refused());
                }
                dir = open_at(dir.as_raw_fd(), &name(part)?, flags, 0)?;
            }
            _ => return Err(refused()),
        }
    }
    Ok(dir)
}
fn identity(fd: &OwnedFd) -> Result<(u64, u64)> {
    let file = File::from(fd.try_clone().map_err(|_| refused())?);
    let stat = file.metadata().map_err(|_| refused())?;
    Ok((stat.dev(), stat.ino()))
}
fn owned_directory(fd: &OwnedFd) -> Result<()> {
    let mut stat = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
    if unsafe { nix::libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(refused());
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_uid != unsafe { nix::libc::geteuid() }
        || stat.st_mode & 0o022 != 0
        || stat.st_mode & nix::libc::S_IFMT != nix::libc::S_IFDIR
    {
        return Err(refused());
    }
    Ok(())
}
fn read(file: &mut File) -> Result<Vec<u8>> {
    let stat = file.metadata().map_err(|_| refused())?;
    if !stat.is_file()
        || stat.uid() != unsafe { nix::libc::geteuid() }
        || stat.mode() & 0o022 != 0
        || stat.len() as usize > MAX_FILE
    {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refused())?;
    if bytes.len() > MAX_FILE {
        return Err(refused());
    }
    Ok(bytes)
}
pub(super) fn candidate_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| refused())?;
    let bytes = read(&mut file)?;
    if bytes.is_empty() || bytes.len() > 32768 {
        return Err(refused());
    }
    Ok(bytes)
}

pub(super) struct Target {
    path: PathBuf,
    parent: OwnedFd,
    leaf: CString,
    original: Option<(u64, u64, Vec<u8>)>,
}
pub(super) enum Destination {
    Existing(Target),
    NewSsh {
        path: PathBuf,
        home: PathBuf,
        pinned: OwnedFd,
    },
}
impl Destination {
    pub(super) fn capture(path: PathBuf, home: &Path) -> Result<Self> {
        let ssh = home.join(".ssh");
        if path.parent() == Some(ssh.as_path()) {
            match std::fs::symlink_metadata(&ssh) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let pinned = directory(home)?;
                    owned_directory(&pinned)?;
                    return Ok(Self::NewSsh {
                        path,
                        home: home.into(),
                        pinned,
                    });
                }
                Err(_) => return Err(refused()),
                Ok(_) => {}
            }
        }
        Target::capture(path).map(Self::Existing)
    }
    pub(super) fn append(&self, entry: &[u8]) -> Result<()> {
        match self {
            Self::Existing(target) => target.append(entry),
            Self::NewSsh { path, home, pinned } => {
                owned_directory(pinned)?;
                if identity(&directory(home)?)? != identity(pinned)? {
                    return Err(refused());
                }
                let leaf = CString::new(".ssh").unwrap();
                // Enrollment happens only in the approved trust commit. An
                // independently created directory is never adopted on retry.
                if unsafe { nix::libc::mkdirat(pinned.as_raw_fd(), leaf.as_ptr(), 0o700) } != 0 {
                    return Err(refused());
                }
                if unsafe { nix::libc::fsync(pinned.as_raw_fd()) } != 0 {
                    return Err(SelectionFailure::Unavailable);
                }
                let ssh = open_at(
                    pinned.as_raw_fd(),
                    &leaf,
                    nix::libc::O_RDONLY
                        | nix::libc::O_DIRECTORY
                        | nix::libc::O_NOFOLLOW
                        | nix::libc::O_CLOEXEC,
                    0,
                )?;
                if identity(&directory(&home.join(".ssh"))?)? != identity(&ssh)? {
                    return Err(refused());
                }
                let target = Target::capture(path.clone())?;
                if identity(&target.parent)? != identity(&ssh)? {
                    return Err(refused());
                }
                target.append(entry)
            }
        }
    }
}
impl Target {
    /// Capture only an existing safe parent. Clean-Mac `.ssh` enrollment is
    /// performed separately after positive proof; absent arbitrary directories
    /// cannot be fabricated from a remote connection or a prompt string.
    pub(super) fn capture(path: PathBuf) -> Result<Self> {
        let parent_path = path.parent().ok_or_else(refused)?;
        let parent = directory(parent_path)?;
        owned_directory(&parent)?;
        let leaf = name(path.file_name().ok_or_else(refused)?)?;
        let fd = unsafe {
            nix::libc::openat(
                parent.as_raw_fd(),
                leaf.as_ptr(),
                nix::libc::O_RDONLY
                    | nix::libc::O_NONBLOCK
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_CLOEXEC,
            )
        };
        let original = if fd < 0 {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
                return Err(refused());
            }
            None
        } else {
            let mut file = unsafe { File::from_raw_fd(fd) };
            let bytes = read(&mut file)?;
            let stat = file.metadata().map_err(|_| refused())?;
            Some((stat.dev(), stat.ino(), bytes))
        };
        Ok(Self {
            path,
            parent,
            leaf,
            original,
        })
    }
    fn current_parent(&self) -> Result<()> {
        let path = self.path.parent().ok_or_else(refused)?;
        if identity(&directory(path)?)? != identity(&self.parent)? {
            return Err(refused());
        }
        owned_directory(&self.parent)
    }
    /// Exactly one append syscall preserves external writers' preceding data.
    /// No truncation or inode replacement is attempted on any error.
    pub(super) fn append(&self, entry: &[u8]) -> Result<()> {
        if entry.is_empty()
            || entry.len() > 32 * 1024
            || entry.last() != Some(&b'\n')
            || entry[..entry.len() - 1].contains(&b'\n')
            || entry.contains(&b'\r')
            || entry.contains(&0)
        {
            return Err(refused());
        }
        self.current_parent()?;
        let mut flags = nix::libc::O_RDWR
            | nix::libc::O_APPEND
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_CLOEXEC;
        if self.original.is_none() {
            flags |= nix::libc::O_CREAT | nix::libc::O_EXCL;
        }
        let fd = open_at(self.parent.as_raw_fd(), &self.leaf, flags, 0o600)?;
        let mut file = File::from(fd);
        let stat = file.metadata().map_err(|_| refused())?;
        let bytes = read(&mut file)?;
        match &self.original {
            Some((device, inode, original))
                if stat.dev() == *device && stat.ino() == *inode && bytes == *original => {}
            None if bytes.is_empty() => {}
            _ => return Err(refused()),
        }
        if bytes.len() + entry.len() + 1 > MAX_FILE {
            return Err(refused());
        }
        self.current_parent()?;
        let current = open_at(
            self.parent.as_raw_fd(),
            &self.leaf,
            nix::libc::O_RDONLY
                | nix::libc::O_NONBLOCK
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_CLOEXEC,
            0,
        )?;
        if identity(&current)? != (stat.dev(), stat.ino()) {
            return Err(refused());
        }
        // Prefixing a newline cannot merge an incomplete prior line with this
        // approved record. Even a short write preserves all original bytes.
        let mut append = Vec::with_capacity(entry.len() + 1);
        append.push(b'\n');
        append.extend_from_slice(entry);
        let written = file
            .write(&append)
            .map_err(|_| SelectionFailure::Unavailable)?;
        if written != append.len() {
            return Err(SelectionFailure::Unavailable);
        }
        file.sync_all().map_err(|_| SelectionFailure::Unavailable)?;
        if unsafe { nix::libc::fsync(self.parent.as_raw_fd()) } != 0 {
            return Err(SelectionFailure::Unavailable);
        }
        self.verify_committed((stat.dev(), stat.ino()))
    }
    fn verify_committed(&self, receipt: (u64, u64)) -> Result<()> {
        self.current_parent()?;
        let current = open_at(
            self.parent.as_raw_fd(),
            &self.leaf,
            nix::libc::O_RDONLY
                | nix::libc::O_NONBLOCK
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_CLOEXEC,
            0,
        )?;
        let current = File::from(current);
        let stat = current.metadata().map_err(|_| refused())?;
        if !stat.is_file()
            || stat.uid() != unsafe { nix::libc::geteuid() }
            || stat.mode() & 0o022 != 0
            || stat.len() > MAX_FILE as u64
            || (stat.dev(), stat.ino()) != receipt
        {
            return Err(refused());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, FileTypeExt, PermissionsExt};
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "cx-native-trust-store-{}",
                    &chimaera_core::generate_token()[..24]
                ));
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }
        fn file(&self, bytes: &[u8]) -> PathBuf {
            let path = self.0.join("known_hosts");
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    const ENTRY: &[u8] = b"synthetic.invalid ssh-ed25519 approved-fixture\n";
    #[test]
    fn append_preserves_unterminated_original_inode_and_refuses_replay() {
        let fixture = Fixture::new();
        let path = fixture.file(b"original unterminated");
        let before = std::fs::metadata(&path).unwrap();
        let target = Target::capture(path.clone()).unwrap();
        target.append(ENTRY).unwrap();
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            [b"original unterminated\n".as_slice(), ENTRY].concat()
        );
        assert!(target.append(ENTRY).is_err());
    }
    #[test]
    fn intervening_append_or_replacement_never_overwrites_external_data() {
        for replace in [false, true] {
            let fixture = Fixture::new();
            let path = fixture.file(b"original\n");
            let target = Target::capture(path.clone()).unwrap();
            if replace {
                std::fs::remove_file(&path).unwrap();
                fixture.file(b"external replacement\n");
            } else {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(b"external append\n")
                    .unwrap();
            }
            let external = std::fs::read(&path).unwrap();
            assert!(target.append(ENTRY).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), external);
        }
    }
    #[test]
    fn parent_swap_symlinks_fifo_and_writable_paths_refuse_without_damage() {
        let fixture = Fixture::new();
        let parent = fixture.0.join("parent");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = parent.join("known_hosts");
        let target = Target::capture(path.clone()).unwrap();
        let away = fixture.0.join("away");
        std::fs::rename(&parent, &away).unwrap();
        symlink(&away, &parent).unwrap();
        assert!(target.append(ENTRY).is_err());
        assert!(!away.join("known_hosts").exists());
        assert!(Target::capture(path).is_err());
        let file = fixture.file(b"unchanged");
        let link = fixture.0.join("link");
        symlink(&file, &link).unwrap();
        assert!(Target::capture(link).is_err());
        let fifo = fixture.0.join("fifo");
        let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { nix::libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(Target::capture(fifo).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(Target::capture(file.clone()).is_err());
        assert_eq!(std::fs::read(file).unwrap(), b"unchanged");
    }
    #[test]
    fn clean_home_enrollment_waits_for_commit_and_refuses_foreign_directory() {
        for concurrent in [false, true] {
            let fixture = Fixture::new();
            let path = fixture.0.join(".ssh/known_hosts");
            let destination = Destination::capture(path.clone(), &fixture.0).unwrap();
            assert!(!fixture.0.join(".ssh").exists());
            if concurrent {
                std::fs::create_dir(fixture.0.join(".ssh")).unwrap();
            }
            let result = destination.append(ENTRY);
            if concurrent {
                assert!(result.is_err());
                assert!(!path.exists());
            } else {
                result.unwrap();
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    [b"\n".as_slice(), ENTRY].concat()
                );
                assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o777, 0o600);
            }
        }
    }
    #[test]
    fn post_append_fifo_replacement_refuses_without_waiting_or_rolling_back() {
        let fixture = Fixture::new();
        let path = fixture.file(b"original\n");
        let target = Target::capture(path.clone()).unwrap();
        let stat = std::fs::metadata(&path).unwrap();
        target.append(ENTRY).unwrap();
        let away = fixture.0.join("approved-away");
        std::fs::rename(&path, &away).unwrap();
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { nix::libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            send.send(target.verify_committed((stat.dev(), stat.ino())).is_err())
                .unwrap();
        });
        assert!(receive
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap());
        worker.join().unwrap();
        assert_eq!(
            std::fs::read(&away).unwrap(),
            [b"original\n\n".as_slice(), ENTRY].concat()
        );
        assert!(std::fs::symlink_metadata(path)
            .unwrap()
            .file_type()
            .is_fifo());
    }
}
