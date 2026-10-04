//! macOS needs a named Mach-O image: a captured FD is not executable there.
//! This stage contains only verified captured bytes, never a selected command.
use super::transport::CompanionCleanupUnknown;
use anyhow::{ensure, Result};
use rustix::fs::{self as at, AtFlags, Mode, OFlags};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::PathBuf,
    sync::Arc,
};
const CAP: u64 = 128 * 1024 * 1024;
const LEAF: &str = "helper";
fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
}
fn same_number(value: impl TryInto<u64>, expected: u64) -> bool {
    value.try_into().ok() == Some(expected)
}
fn identity(m: &fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
pub(super) struct Stage {
    parent: fs::File,
    directory: fs::File,
    file: fs::File,
    name: String,
    path: PathBuf,
}
impl Stage {
    pub(super) fn copy(source: &fs::File) -> Result<Arc<Self>> {
        let original = source.metadata()?;
        ensure!(
            original.is_file() && original.len() <= CAP,
            "transfer executable exceeds limit"
        );
        let parent_path = std::env::temp_dir().canonicalize()?;
        let parent = fs::OpenOptions::new()
            .read(true)
            .custom_flags(directory_flags().bits() as i32)
            .open(&parent_path)?;
        let name = format!("chimaera-pro-transfer-{}", chimaera_core::generate_token());
        at::mkdirat(&parent, &name, Mode::RWXU)?;
        // Once mkdir succeeded, ambiguity retains that named directory rather
        // than guessing a cleanup identity from a later pathname observation.
        let directory = fs::File::from(
            at::openat(&parent, &name, directory_flags(), Mode::empty())
                .map_err(|_| CompanionCleanupUnknown)?,
        );
        let file = match at::openat(
            &directory,
            LEAF,
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK,
            Mode::from_raw_mode(0o500),
        ) {
            Ok(fd) => fs::File::from(fd),
            Err(error) => {
                let captured = directory.metadata().map_err(|_| CompanionCleanupUnknown)?;
                let current = at::statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|_| CompanionCleanupUnknown)?;
                ensure!(
                    same_number(current.st_dev, captured.dev())
                        && same_number(current.st_ino, captured.ino()),
                    CompanionCleanupUnknown
                );
                at::unlinkat(&parent, &name, AtFlags::REMOVEDIR)
                    .map_err(|_| CompanionCleanupUnknown)?;
                parent.sync_all().map_err(|_| CompanionCleanupUnknown)?;
                return Err(error.into());
            }
        };
        let mut stage = Arc::new(Self {
            path: parent_path.join(&name).join(LEAF),
            parent,
            directory,
            file,
            name,
        });
        let result = (|| -> Result<()> {
            let mut source = source.try_clone()?;
            source.seek(SeekFrom::Start(0))?;
            let mut target = stage.file.try_clone()?;
            let copied = std::io::copy(&mut source.by_ref().take(CAP + 1), &mut target)?;
            ensure!(
                copied == original.len()
                    && copied <= CAP
                    && identity(&source.metadata()?) == identity(&original),
                "transfer executable changed during capture"
            );
            stage.file.sync_all()?;
            stage.directory.sync_all()?;
            stage.parent.sync_all()?;
            // Executing a named file while any retained description can write
            // it can fail ETXTBSY. Reopen the same pinned leaf read-only, verify
            // its inode, then close every writer before publishing the stage.
            let readonly = fs::File::from(at::openat(
                &stage.directory,
                LEAF,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )?);
            let read_meta = readonly.metadata()?;
            let written = stage.file.metadata()?;
            ensure!(
                read_meta.dev() == written.dev() && read_meta.ino() == written.ino(),
                "transfer executable stage changed"
            );
            drop(target);
            Arc::get_mut(&mut stage)
                .expect("unpublished unique stage")
                .file = readonly;
            stage.verify()?;
            Ok(())
        })();
        if let Err(error) = result {
            stage.cleanup().map_err(|_| CompanionCleanupUnknown)?;
            return Err(error);
        }
        Ok(stage)
    }
    pub(super) fn path(&self) -> &std::path::Path {
        &self.path
    }
    pub(super) fn verify(&self) -> Result<()> {
        let file = self.file.metadata()?;
        let named = at::statat(&self.directory, LEAF, AtFlags::SYMLINK_NOFOLLOW)?;
        let directory = self.directory.metadata()?;
        let current = at::statat(&self.parent, &self.name, AtFlags::SYMLINK_NOFOLLOW)?;
        ensure!(
            file.is_file()
                && file.nlink() == 1
                && file.mode() & 0o7777 == 0o500
                && file.uid() == unsafe { nix::libc::geteuid() }
                && same_number(named.st_dev, file.dev())
                && same_number(named.st_ino, file.ino())
                && same_number(named.st_size, file.len())
                && at::FileType::from_raw_mode(named.st_mode) == at::FileType::RegularFile,
            "transfer executable stage changed"
        );
        ensure!(
            directory.is_dir()
                && directory.mode() & 0o7777 == 0o700
                && directory.uid() == file.uid()
                && same_number(current.st_dev, directory.dev())
                && same_number(current.st_ino, directory.ino())
                && at::FileType::from_raw_mode(current.st_mode) == at::FileType::Directory,
            "transfer executable directory changed"
        );
        Ok(())
    }
    pub(super) fn configure(self: &Arc<Self>, command: &mut tokio::process::Command) {
        let stage = self.clone();
        let name = std::ffi::CString::new(self.name.as_bytes()).expect("generated stage name");
        // pre_exec uses only async-signal-safe fstat/fstatat over pre-owned FDs.
        unsafe {
            command.pre_exec(move || {
                let mut file: nix::libc::stat = std::mem::zeroed();
                let mut named: nix::libc::stat = std::mem::zeroed();
                let mut directory: nix::libc::stat = std::mem::zeroed();
                let mut parent_entry: nix::libc::stat = std::mem::zeroed();
                if nix::libc::fstat(stage.directory.as_raw_fd(), &mut directory) != 0
                    || nix::libc::fstatat(
                        stage.parent.as_raw_fd(),
                        name.as_ptr(),
                        &mut parent_entry,
                        nix::libc::AT_SYMLINK_NOFOLLOW,
                    ) != 0
                    || directory.st_dev != parent_entry.st_dev
                    || directory.st_ino != parent_entry.st_ino
                    || directory.st_mode & 0o7777 != 0o700
                    || nix::libc::fstat(stage.file.as_raw_fd(), &mut file) != 0
                    || nix::libc::fstatat(
                        stage.directory.as_raw_fd(),
                        c"helper".as_ptr(),
                        &mut named,
                        nix::libc::AT_SYMLINK_NOFOLLOW,
                    ) != 0
                    || file.st_dev != named.st_dev
                    || file.st_ino != named.st_ino
                    || file.st_size != named.st_size
                    || named.st_nlink != 1
                    || named.st_mode & 0o7777 != 0o500
                {
                    return Err(std::io::Error::from_raw_os_error(nix::libc::ESTALE));
                }
                Ok(())
            });
        }
    }
    pub(super) fn cleanup(&self) -> Result<()> {
        self.verify()?;
        at::unlinkat(&self.directory, LEAF, AtFlags::empty())?;
        self.directory.sync_all()?;
        // remove_dir is exclusive and cannot discard an unexpected sibling.
        at::unlinkat(&self.parent, &self.name, AtFlags::REMOVEDIR)?;
        self.parent.sync_all()?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_byte_stage_ignores_replaced_source_path_and_cleans_exact_owned_inodes() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-image-{}",
            chimaera_core::generate_token()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("source");
        fs::write(&path, b"original owned bytes").unwrap();
        let source = fs::File::open(&path).unwrap();
        fs::rename(&path, root.join("old")).unwrap();
        fs::write(&path, b"successor must not run").unwrap();
        let stage = Stage::copy(&source).unwrap();
        assert_eq!(fs::read(stage.path()).unwrap(), b"original owned bytes");
        let staged = stage.path().parent().unwrap().to_owned();
        stage.cleanup().unwrap();
        assert!(!staged.exists());
        assert_eq!(fs::read(path).unwrap(), b"successor must not run");
        fs::remove_dir_all(root).unwrap();
    }
    fn script(root: &std::path::Path, bytes: &[u8]) -> fs::File {
        use std::os::unix::fs::PermissionsExt;
        let path = root.join("source");
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        fs::File::open(path).unwrap()
    }
    #[tokio::test]
    async fn named_captured_image_executes_original_bytes_and_cleans_on_malformed_reply() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-run-{}",
            chimaera_core::generate_token()
        ));
        fs::create_dir(&root).unwrap();
        let source = script(&root, b"#!/bin/sh\nprintf 'original-fixed-output'\n");
        fs::rename(root.join("source"), root.join("old")).unwrap();
        drop(script(&root, b"#!/bin/sh\nexit 71\n"));
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let prepare = Box::new(move |_| {
            let stage = Stage::copy(&source)?;
            let _ = sender.send(stage.path().parent().unwrap().to_owned());
            let mut command = tokio::process::Command::new(stage.path());
            command.env_clear();
            stage.configure(&mut command);
            Ok(super::super::transport::PreparedCompanion {
                file: None,
                command,
                cleanup: Some(Box::new(move || stage.cleanup())),
            })
        });
        let output = super::super::transport::run_companion(
            tokio::process::Command::new("/unused-original-owner"),
            Vec::new(),
            std::time::Duration::from_secs(2),
            128,
            prepare,
        )
        .await
        .unwrap();
        assert!(output.success);
        assert_eq!(output.stdout, b"original-fixed-output");
        assert!(
            serde_json::from_slice::<super::super::config_wire::Reply>(&output.stdout).is_err()
        );
        assert!(!receiver.await.unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn aborted_observer_keeps_stage_and_cache_until_same_retained_cleanup_completes() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-held-{}",
            chimaera_core::generate_token()
        ));
        fs::create_dir(&root).unwrap();
        let marker = root.join("started");
        let bytes = format!(
            "#!/bin/sh\nprintf ready > '{}'\nexec /bin/sleep 30\n",
            marker.display()
        );
        let source = script(&root, bytes.as_bytes());
        let (published, stage_path) = tokio::sync::oneshot::channel();
        let (cleaning, cleanup_started) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let prepare = Box::new(move |_| {
            let stage = Stage::copy(&source)?;
            let _ = published.send(stage.path().parent().unwrap().to_owned());
            let mut command = tokio::process::Command::new(stage.path());
            command.env_clear();
            stage.configure(&mut command);
            Ok(super::super::transport::PreparedCompanion {
                file: None,
                command,
                cleanup: Some(Box::new(move || {
                    let _ = cleaning.send(());
                    blocked.recv_timeout(std::time::Duration::from_secs(2))?;
                    stage.cleanup()
                })),
            })
        });
        let mutex = Arc::new(tokio::sync::Mutex::new(()));
        let guard = Arc::new(mutex.clone().lock_owned().await);
        let workspace = format!("transfer-stage-test-{}", chimaera_core::generate_token());
        let task = tokio::spawn(async move {
            super::super::transport::cache_scope(
                &workspace,
                guard,
                super::super::transport::run_companion(
                    tokio::process::Command::new("/unused-original-owner"),
                    Vec::new(),
                    std::time::Duration::from_secs(10),
                    128,
                    prepare,
                ),
            )
            .await
        });
        let path = tokio::time::timeout(std::time::Duration::from_secs(2), stage_path)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !tokio::fs::try_exists(&marker).await.unwrap() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        tokio::time::timeout(std::time::Duration::from_secs(2), cleanup_started)
            .await
            .unwrap()
            .unwrap();
        assert!(path.exists());
        assert!(mutex.try_lock().is_err());
        release.send(()).unwrap();
        let settled = tokio::time::timeout(std::time::Duration::from_secs(3), mutex.lock())
            .await
            .unwrap();
        assert!(!path.exists());
        drop(settled);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unknown_stage_replacement_refuses_cleanup_and_keeps_recovery_evidence() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-unknown-{}",
            chimaera_core::generate_token()
        ));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        fs::write(&source, b"original").unwrap();
        let stage = Stage::copy(&fs::File::open(&source).unwrap()).unwrap();
        let path = stage.path().to_owned();
        fs::rename(&path, path.with_file_name("saved")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(stage.cleanup().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        let stage_dir = path.parent().unwrap().to_owned();
        drop(stage);
        fs::remove_dir_all(stage_dir).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
