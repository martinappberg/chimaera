//! Descriptor-backed reads for a project viewer. Resolution permits internal
//! symlinks; the subsequent component walk never follows a replacement symlink.
use rustix::fs::OFlags;
use std::{
    fs::File,
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(crate) struct Confined {
    pub(super) root: PathBuf,
    directory: File,
}
impl Confined {
    pub(super) fn new(root: &Path) -> io::Result<Self> {
        let expected = std::fs::symlink_metadata(root)?;
        if !expected.is_dir() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        // Workspace roots are registered canonically. Resolving them again
        // would let a replaced ancestor nominate a different project inode.
        let root = root.to_owned();
        let anchor = File::open("/")?;
        let directory = crate::download::open_beneath(
            &anchor,
            root.strip_prefix("/")
                .map_err(|_| io::ErrorKind::InvalidInput)?,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        )?;
        let pinned = directory.metadata()?;
        if expected.dev() != pinned.dev() || expected.ino() != pinned.ino() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let confined = Self { root, directory };
        confined.check_root()?;
        Ok(confined)
    }
    pub(super) fn check_root(&self) -> io::Result<()> {
        let pinned = self.directory.metadata()?;
        let current = std::fs::symlink_metadata(&self.root)?;
        if !current.is_dir() || pinned.dev() != current.dev() || pinned.ino() != current.ino() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
    pub(super) fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        self.check_root()?;
        let resolved = std::fs::canonicalize(path)?;
        if !resolved.starts_with(&self.root) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(resolved)
    }
    fn open_resolved(&self, resolved: &Path, directory: bool) -> io::Result<File> {
        self.check_root()?;
        let relative = resolved
            .strip_prefix(&self.root)
            .map_err(|_| io::ErrorKind::PermissionDenied)?;
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        if directory {
            flags |= OFlags::DIRECTORY;
        }
        let file = crate::download::open_beneath(&self.directory, relative, flags)?;
        self.check_root()?;
        Ok(file)
    }
    pub(super) fn open(&self, path: &Path, directory: bool) -> io::Result<File> {
        self.open_resolved(&self.resolve(path)?, directory)
    }
    pub(super) fn read(&self, path: &Path, cap: u64) -> io::Result<Option<Vec<u8>>> {
        let file = self.open(path, false)?;
        super::read_opened(file, cap)
    }
    pub(super) fn listing(&self, path: &Path) -> io::Result<super::Listing> {
        let directory = self.open(path, true)?;
        let mut entries = rustix::fs::Dir::read_from(&directory)?;
        let mut names = std::collections::HashSet::new();
        let mut complete = true;
        let mut count = 0;
        while let Some(entry) = entries.read() {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if count >= super::MAX_LISTING_ENTRIES {
                complete = false;
                break;
            }
            count += 1;
            if let Ok(name) = std::str::from_utf8(name) {
                names.insert(name.to_owned());
            }
        }
        self.check_root()?;
        Ok(super::Listing { names, complete })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "chimaera-confined-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(root.join("project/sub")).unwrap();
        std::fs::write(root.join("project/sub/target.md"), "# Public\n").unwrap();
        std::fs::write(root.join("private.md"), "# Synthetic private heading\n").unwrap();
        std::fs::canonicalize(root).unwrap()
    }
    #[test]
    fn replacement_file_and_parent_symlinks_never_follow_outside() {
        let root = fixture();
        let confined = Confined::new(&root.join("project")).unwrap();
        let file = root.join("project/sub/target.md");
        let resolved = confined.resolve(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        symlink(root.join("private.md"), &file).unwrap();
        assert!(confined.open_resolved(&resolved, false).is_err());
        std::fs::remove_file(&file).unwrap();
        std::fs::rename(root.join("project/sub"), root.join("old-sub")).unwrap();
        symlink(&root, root.join("project/sub")).unwrap();
        assert!(confined.open_resolved(&resolved, false).is_err());
        assert!(confined.listing(&root.join("project/sub")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn preexisting_ancestor_symlink_cannot_nominate_a_different_root() {
        let root = fixture();
        let registered = root.join("enrolled/project");
        std::fs::create_dir_all(&registered).unwrap();
        let registered = std::fs::canonicalize(registered).unwrap();
        std::fs::rename(root.join("enrolled"), root.join("old-enrolled")).unwrap();
        symlink(&root, root.join("enrolled")).unwrap();
        assert!(
            Confined::new(&registered).is_err(),
            "a stored canonical root must never be resolved through a replaced ancestor"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn internal_symlinks_still_resolve_but_replaced_roots_refuse() {
        let root = fixture();
        symlink("sub/target.md", root.join("project/link.md")).unwrap();
        let confined = Confined::new(&root.join("project")).unwrap();
        assert_eq!(
            confined
                .read(&root.join("project/link.md"), 100)
                .unwrap()
                .unwrap(),
            b"# Public\n"
        );
        std::fs::rename(root.join("project"), root.join("old-project")).unwrap();
        std::fs::create_dir(root.join("project")).unwrap();
        std::fs::write(root.join("project/link.md"), "# Other\n").unwrap();
        assert!(confined.read(&root.join("project/link.md"), 100).is_err());
        std::fs::remove_dir_all(root.join("project")).unwrap();
        symlink(&root, root.join("project")).unwrap();
        assert!(Confined::new(&root.join("project")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
