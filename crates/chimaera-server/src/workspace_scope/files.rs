//! Filesystem authority remains attached to directory descriptors after proof.
use rustix::fs::OFlags;
use std::{
    fs::File,
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug)]
pub(super) struct Root {
    path: PathBuf,
    directory: File,
    identity: (u64, u64),
}
impl Root {
    pub(super) fn pin(path: PathBuf) -> io::Result<Arc<Self>> {
        let directory = literal_directory(&path)?;
        let metadata = directory.metadata()?;
        let identity = (metadata.dev(), metadata.ino());
        Ok(Arc::new(Self {
            path,
            directory,
            identity,
        }))
    }
    fn check(&self) -> io::Result<()> {
        let current = literal_directory(&self.path)?.metadata()?;
        let pinned = self.directory.metadata()?;
        if current.dev() != pinned.dev() || current.ino() != pinned.ino() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
    pub(super) fn resolve(self: &Arc<Self>, path: &Path) -> io::Result<Resolved> {
        self.check()?;
        // Resolve intentional internal links once. Later effects walk only
        // the resolved relative components, never the original link again.
        let canonical = path.canonicalize()?;
        let relative = canonical
            .strip_prefix(&self.path)
            .map_err(|_| io::ErrorKind::PermissionDenied)?
            .to_owned();
        self.check()?;
        Ok(Resolved {
            root: self.clone(),
            canonical,
            relative,
            authority: None,
        })
    }
}
fn literal_directory(path: &Path) -> io::Result<File> {
    let anchor = File::open("/")?;
    crate::download::open_beneath(
        &anchor,
        path.strip_prefix("/")
            .map_err(|_| io::ErrorKind::InvalidInput)?,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
    )
}
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct TicketIdentity {
    workspace: String,
    epoch: u64,
    generation: u64,
    registered_root: PathBuf,
    captured_root: (u64, u64),
}
#[derive(Clone)]
pub(crate) struct Resolved {
    root: Arc<Root>,
    pub(crate) canonical: PathBuf,
    relative: PathBuf,
    authority: Option<Authority>,
}
impl Resolved {
    /// In-memory identity for exact scoped ticket renewal; no filesystem work
    /// is allowed while the shared ticket store is locked.
    pub(crate) fn ticket_identity(&self) -> Option<TicketIdentity> {
        let authority = self.authority.as_ref()?;
        Some(TicketIdentity {
            workspace: authority.admission.scope.workspace_id.clone(),
            epoch: authority.admission.scope.epoch,
            generation: authority.admission.generation(),
            registered_root: authority.root.clone(),
            captured_root: self.root.identity,
        })
    }

    pub(crate) fn open(&self, directory: bool) -> io::Result<File> {
        let file = self.open_any()?;
        if !(if directory {
            file.metadata()?.is_dir()
        } else {
            file.metadata()?.is_file()
        }) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(file)
    }
    pub(crate) fn open_any(&self) -> io::Result<File> {
        if let Some(authority) = &self.authority {
            authority.check()?;
        }
        self.root.check()?;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let file = crate::download::open_beneath(&self.root.directory, &self.relative, flags)?;
        self.root.check()?;
        if let Some(authority) = &self.authority {
            authority.check()?;
        }
        let metadata = file.metadata()?;
        if !(metadata.is_dir() || metadata.is_file()) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(file)
    }
    pub(crate) fn asset(&self, relative: &Path) -> io::Result<File> {
        let directory = self
            .relative
            .parent()
            .ok_or(io::ErrorKind::PermissionDenied)?;
        let mut next = self.clone();
        next.relative = directory.join(relative);
        next.canonical = self.root.path.join(&next.relative);
        next.open(false)
    }
    pub(crate) fn names(&self, limit: usize) -> io::Result<Vec<std::ffi::OsString>> {
        use std::os::unix::ffi::OsStrExt;
        let directory = self.open(true)?;
        let mut listing = rustix::fs::Dir::read_from(&directory)?;
        let mut names = Vec::new();
        while let Some(entry) = listing.read() {
            let entry = entry?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            names.push(std::ffi::OsStr::from_bytes(bytes).to_os_string());
            if names.len() >= limit {
                break;
            }
        }
        Ok(names)
    }
}

#[derive(Clone)]
struct Authority {
    state: std::sync::Weak<crate::AppState>,
    admission: super::Mutation,
    root: PathBuf,
}
impl Authority {
    fn check(&self) -> io::Result<()> {
        let state = self.state.upgrade().ok_or_else(|| {
            io::Error::new(io::ErrorKind::PermissionDenied, crate::policy::Changed)
        })?;
        if self.admission.validate(&state).is_err()
            || crate::lock(&state.workspaces)
                .get(&self.admission.scope.workspace_id)
                .is_none_or(|workspace| workspace.root != self.root)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                crate::policy::Changed,
            ));
        }
        Ok(())
    }
}
/// Captured once at scoped HTTP admission; ordinary daemon requests omit it.
#[derive(Clone)]
pub struct Context {
    project: Arc<Root>,
    uploads: Option<Arc<Root>>,
    authority: Authority,
}
impl Context {
    pub(super) fn pin(
        state: &Arc<crate::AppState>,
        admission: super::Mutation,
    ) -> io::Result<Self> {
        let root = crate::lock(&state.workspaces)
            .get(&admission.scope.workspace_id)
            .ok_or(io::ErrorKind::PermissionDenied)?
            .root;
        let authority = Authority {
            state: Arc::downgrade(state),
            admission,
            root: root.clone(),
        };
        authority.check()?;
        let project = Root::pin(root)?;
        authority.check()?;
        Ok(Self {
            project,
            // The configured daemon-state parent may use an OS alias (/var on
            // macOS). Resolve that trusted state root at admission, then keep
            // all upload/session descendants literal and descriptor-bound.
            uploads: state.uploads_root.parent().and_then(|parent| {
                let canonical = parent.canonicalize().ok()?;
                let root = Root::pin(canonical).ok()?;
                let configured = File::open(parent).ok()?.metadata().ok()?;
                let pinned = root.directory.metadata().ok()?;
                (configured.dev() == pinned.dev() && configured.ino() == pinned.ino())
                    .then_some(root)
            }),
            authority,
        })
    }
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        let state = self
            .authority
            .state
            .upgrade()
            .ok_or(crate::policy::Changed)?;
        self.authority.admission.validate(&state)
    }
    pub(crate) fn key(&self, raw: &str) -> io::Result<()> {
        self.authority.check()?;
        self.project.check()?;
        let path = crate::fs::expand_tilde(raw).map_err(|_| io::ErrorKind::InvalidInput)?;
        if path.exists() {
            self.project.resolve(&path)?.open_any()?;
            return Ok(());
        }
        let mut parent = path.as_path();
        let mut depth = 0;
        while !parent.exists() {
            if parent
                .components()
                .any(|part| part == std::path::Component::ParentDir)
                || depth >= 128
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            parent = parent.parent().ok_or(io::ErrorKind::PermissionDenied)?;
            depth += 1;
        }
        self.project.resolve(parent)?.open_any()?;
        self.authority.check()
    }
    pub(crate) fn entry(&self, raw: &str, follow: bool, create_parents: bool) -> io::Result<Entry> {
        self.authority.check()?;
        let mut path = crate::fs::expand_tilde(raw).map_err(|_| io::ErrorKind::InvalidInput)?;
        if follow && path.exists() {
            path = self.project.resolve(&path)?.canonical;
        }
        let name = path
            .file_name()
            .ok_or(io::ErrorKind::PermissionDenied)?
            .to_owned();
        let mut parent_path = path
            .parent()
            .ok_or(io::ErrorKind::PermissionDenied)?
            .to_owned();
        let mut missing = Vec::new();
        if create_parents {
            while !parent_path.exists() {
                let part = parent_path
                    .file_name()
                    .ok_or(io::ErrorKind::PermissionDenied)?
                    .to_owned();
                if part == "." || part == ".." || missing.len() >= 128 {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                missing.push(part);
                parent_path = parent_path
                    .parent()
                    .ok_or(io::ErrorKind::PermissionDenied)?
                    .to_owned();
            }
        }
        let mut parent = self.project.resolve(&parent_path)?;
        parent.authority = Some(self.authority.clone());
        for part in missing.into_iter().rev() {
            let directory = parent.open(true)?;
            self.authority.check()?;
            match rustix::fs::mkdirat(&directory, &part, rustix::fs::Mode::from_raw_mode(0o777)) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
            parent.relative.push(&part);
            parent.canonical.push(&part);
            parent.open(true)?;
        }
        let directory = parent.open(true)?;
        let canonical = parent.canonical.join(&name);
        Ok(Entry {
            parent,
            directory,
            name,
            canonical,
        })
    }

    pub(crate) fn read(&self, raw: &str) -> io::Result<Resolved> {
        self.authority.check()?;
        self.project.check()?;
        let path = crate::fs::expand_tilde(raw).map_err(|_| io::ErrorKind::InvalidInput)?;
        let mut resolved = match self.project.resolve(&path) {
            Ok(resolved) => resolved,
            Err(error) => {
                // A project's own pasted images are separately bounded roots.
                // Their parent components are literal/no-follow too.
                let canonical = path.canonicalize()?;
                let uploads = self.uploads.as_ref().ok_or(error)?;
                let relative = canonical
                    .strip_prefix(uploads.path.join("uploads"))
                    .map_err(|_| io::ErrorKind::PermissionDenied)?;
                let session = relative
                    .components()
                    .next()
                    .and_then(|part| match part {
                        std::path::Component::Normal(name) => name.to_str(),
                        _ => None,
                    })
                    .ok_or(io::ErrorKind::PermissionDenied)?;
                let state = self
                    .authority
                    .state
                    .upgrade()
                    .ok_or(io::ErrorKind::PermissionDenied)?;
                let belongs = crate::lock(&state.session_workspaces).get(session)
                    == Some(&self.authority.admission.scope.workspace_id);
                if !belongs {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                // No re-canonicalized child root can authorize a replaced
                // uploads/session directory outside the captured state root.
                let resolved = uploads.resolve(&canonical)?;
                let session_root = PathBuf::from("uploads").join(session);
                if !resolved.relative.starts_with(&session_root) {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                resolved
            }
        };
        resolved.authority = Some(self.authority.clone());
        Ok(resolved)
    }
}

/// A leaf operation owns its verified parent descriptor. Names are never
/// reopened through an absolute path after authority has been established.
pub(crate) struct Entry {
    parent: Resolved,
    pub(crate) directory: File,
    pub(crate) name: std::ffi::OsString,
    pub(crate) canonical: PathBuf,
}
impl Entry {
    pub(crate) fn current(&self) -> io::Result<()> {
        self.parent
            .authority
            .as_ref()
            .map_or(Ok(()), Authority::check)
    }
    pub(crate) fn check(&self) -> io::Result<()> {
        let current = self.parent.open(true)?.metadata()?;
        let pinned = self.directory.metadata()?;
        if current.dev() != pinned.dev() || current.ino() != pinned.ino() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
    pub(crate) fn open(&self, flags: OFlags) -> io::Result<File> {
        self.check()?;
        let file = File::from(rustix::fs::openat(
            &self.directory,
            &self.name,
            flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            rustix::fs::Mode::from_raw_mode(0o666),
        )?);
        let metadata = file.metadata()?;
        let created = rustix::fs::fstat(&file)?;
        let checked = self.check().and_then(|()| {
            if metadata.is_file() {
                Ok(())
            } else {
                Err(io::ErrorKind::PermissionDenied.into())
            }
        });
        if let Err(error) = checked {
            // A new exclusive temporary belongs to this operation even if
            // authority changes before the post-open check. Remove only our
            // inode through the already held parent, never a replacement.
            if flags.contains(OFlags::CREATE | OFlags::EXCL)
                && rustix::fs::statat(
                    &self.directory,
                    &self.name,
                    rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                )
                .is_ok_and(|stat| stat.st_ino == created.st_ino && stat.st_dev == created.st_dev)
            {
                let _ =
                    rustix::fs::unlinkat(&self.directory, &self.name, rustix::fs::AtFlags::empty());
            }
            return Err(error);
        }
        Ok(file)
    }
    pub(crate) fn directory(&self) -> io::Result<File> {
        self.check()?;
        let file = File::from(rustix::fs::openat(
            &self.directory,
            &self.name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?);
        self.check()?;
        Ok(file)
    }
    pub(crate) fn child(&self, directory: &File, name: &std::ffi::OsStr) -> io::Result<Self> {
        if Path::new(name).components().count() != 1 || name == "." || name == ".." {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mut parent = self.parent.clone();
        parent.relative.push(&self.name);
        parent.canonical.push(&self.name);
        Ok(Self {
            canonical: parent.canonical.join(name),
            parent,
            directory: directory.try_clone()?,
            name: name.to_owned(),
        })
    }
    pub(crate) fn stat(&self) -> io::Result<Option<rustix::fs::Stat>> {
        self.check()?;
        match rustix::fs::statat(
            &self.directory,
            &self.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => Ok(Some(stat)),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, os::unix::fs::symlink};
    fn fixture() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "chimaera-scoped-files-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(base.join("project/sub")).unwrap();
        std::fs::create_dir(base.join("private")).unwrap();
        std::fs::write(base.join("project/sub/file"), b"project").unwrap();
        std::fs::write(base.join("private/file"), b"private").unwrap();
        base.canonicalize().unwrap()
    }
    #[test]
    fn proven_file_and_parent_replacements_cannot_redirect_the_read() {
        let base = fixture();
        let root = Root::pin(base.join("project")).unwrap();
        let path = base.join("project/sub/file");
        let proved = root.resolve(&path).unwrap();
        assert_eq!(proved.canonical, path);
        std::fs::remove_file(&path).unwrap();
        symlink(base.join("private/file"), &path).unwrap();
        assert!(proved.open(false).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(base.join("project/sub"), base.join("old-sub")).unwrap();
        symlink(base.join("private"), base.join("project/sub")).unwrap();
        assert!(proved.open(false).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn internal_symlinks_are_resolved_once_and_root_replacement_refuses() {
        let base = fixture();
        symlink("sub/file", base.join("project/link")).unwrap();
        let root = Root::pin(base.join("project")).unwrap();
        let proved = root.resolve(&base.join("project/link")).unwrap();
        std::fs::remove_file(base.join("project/link")).unwrap();
        symlink(base.join("private/file"), base.join("project/link")).unwrap();
        let mut bytes = Vec::new();
        proved.open(false).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"project");
        std::fs::rename(base.join("project"), base.join("old-project")).unwrap();
        std::fs::create_dir(base.join("project")).unwrap();
        assert!(proved.open(false).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }
}
