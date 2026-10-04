//! Host-owned snapshot confinement and cleanup; optional paid mirror policy.
use super::{policy, transfer_dispatch as dispatch};
use anyhow::{bail, ensure, Result};
use dispatch::{TransferReply as Reply, TransferRequest as Request};
use rustix::fs::OFlags;
use serde::Serialize;
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
#[derive(Clone, Default, Serialize)]
pub struct Report {
    pub files: usize,
    pub bytes: u64,
    pub excluded: usize,
    pub too_large: usize,
    /// Paths present in the project that this snapshot left out. A receiver
    /// deletes nothing on their account; `None` once the list would exceed
    /// its bound, which makes every absence ambiguous.
    #[serde(skip)]
    pub left_out: Option<Vec<PathBuf>>,
}
const LEFT_OUT_LIMIT: usize = 4096;
impl Report {
    fn leave_out(&mut self, path: &Path) {
        if let Some(list) = self.left_out.as_mut() {
            if list.len() >= LEFT_OUT_LIMIT {
                self.left_out = None;
            } else {
                list.push(path.to_path_buf());
            }
        }
    }
}

const REPOSITORIES: [&str; 4] = [
    "working-tree.git",
    "repository.git",
    "incoming.git",
    "incoming-repository.git",
];

/// Leftovers of an interrupted helper or transfer (a killed daemon, a
/// SIGKILLed Git child): ref and index locks, temporary indexes and packs,
/// and staging copies. Callers hold the project's cache guard with no helper
/// running, so nothing live can own them; Git refuses to proceed past them.
pub(super) fn clear_interrupted(project: &Path) -> Result<()> {
    let entries = match fs::read_dir(project) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries.take(4096) {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (name.starts_with("stage-") || name.starts_with("hydrate-"))
            && entry.file_type()?.is_dir()
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    for repository in REPOSITORIES.map(|name| project.join(name)) {
        let Ok(entries) = fs::read_dir(&repository) else {
            continue;
        };
        for entry in entries.take(4096) {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type()?.is_file()
                && (name.starts_with("index-") || name.ends_with(".lock"))
            {
                fs::remove_file(entry.path())?;
            }
        }
        if let Ok(entries) = fs::read_dir(repository.join("objects/pack")) {
            for entry in entries.take(4096) {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with("tmp_")
                    && entry.file_type()?.is_file()
                {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        let mut pending = vec![repository.join("refs")];
        let mut seen = 0;
        while let Some(directory) = pending.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries {
                seen += 1;
                ensure!(seen <= 16_384, "mirror references exceed limit");
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    pending.push(entry.path());
                } else if kind.is_file() && entry.file_name().to_string_lossy().ends_with(".lock") {
                    fs::remove_file(entry.path())?;
                }
            }
        }
    }
    Ok(())
}

pub fn copy_tree(
    root: &Path,
    destination: &Path,
    (paths, ignored): (Vec<PathBuf>, Vec<PathBuf>),
    budget: u64,
    max_file: u64,
) -> Result<Report> {
    let directory = pin_project(root)?;
    copy_tree_pinned(
        root,
        &directory,
        destination,
        (paths, ignored),
        budget,
        max_file,
    )
}

fn pin_project(root: &Path) -> Result<fs::File> {
    let expected = fs::symlink_metadata(root)?;
    ensure!(expected.is_dir(), "project folder changed during mirror");
    // Registered workspace roots are canonical already. Resolving again would
    // accept an ancestor replaced with a symlink before this copy started.
    let directory = crate::download::open_beneath(
        &fs::File::open("/")?,
        root.strip_prefix("/")?,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
    )?;
    let pinned = directory.metadata()?;
    ensure!(
        expected.dev() == pinned.dev() && expected.ino() == pinned.ino(),
        "project folder changed during mirror"
    );
    Ok(directory)
}

fn check_project(root: &Path, directory: &fs::File) -> Result<()> {
    let current = fs::symlink_metadata(root)?;
    let pinned = directory.metadata()?;
    ensure!(
        current.is_dir() && current.dev() == pinned.dev() && current.ino() == pinned.ino(),
        "project folder changed during mirror"
    );
    Ok(())
}

fn copy_tree_pinned(
    root: &Path,
    directory: &fs::File,
    destination: &Path,
    (paths, ignored): (Vec<PathBuf>, Vec<PathBuf>),
    budget: u64,
    max_file: u64,
) -> Result<Report> {
    check_project(root, directory)?;
    fs::create_dir_all(destination)?;
    let mut report = Report {
        left_out: Some(Vec::new()),
        ..Report::default()
    };
    for relative in &ignored {
        report.leave_out(relative);
    }
    for relative in paths {
        if !policy::allowed_path(&relative) {
            report.excluded += 1;
            report.leave_out(&relative);
            continue;
        }
        // Open every component relative to the pinned project descriptor.
        // Checking ancestor paths before a full-path open leaves a race where
        // a replacement symlink can export an unrelated private directory.
        let mut input = match crate::download::open_beneath(
            directory,
            &relative,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        ) {
            Ok(file) => file,
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(nix::libc::ELOOP | nix::libc::ENOTDIR)
                ) =>
            {
                report.excluded += 1;
                report.leave_out(&relative);
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let metadata = input.metadata()?;
        if !metadata.is_file() {
            report.leave_out(&relative);
            continue;
        }
        if metadata.len() > max_file.min(policy::MAX_FILE_BYTES) {
            report.too_large += 1;
            report.leave_out(&relative);
            continue;
        }
        ensure!(
            report.bytes.saturating_add(metadata.len()) <= budget,
            "workspace exceeds mirror storage quota"
        );
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&target)?;
        let mut chunk = [0u8; 65536];
        let mut tail = Vec::new();
        let mut length = 0u64;
        let mut secret = false;
        loop {
            let count = input.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            length += count as u64;
            ensure!(
                length <= max_file.min(policy::MAX_FILE_BYTES)
                    && report.bytes.saturating_add(length) <= budget,
                "project grew beyond mirror limits"
            );
            tail.extend_from_slice(&chunk[..count]);
            if policy::contains_credential(&tail) {
                secret = true;
                break;
            }
            output.write_all(&chunk[..count])?;
            if tail.len() > 128 {
                tail.drain(..tail.len() - 128);
            }
        }
        drop(output);
        if secret {
            fs::remove_file(target)?;
            report.excluded += 1;
            report.leave_out(&relative);
            continue;
        }
        fs::set_permissions(&target, metadata.permissions())?;
        report.bytes += length;
        report.files += 1;
    }
    check_project(root, directory)?;
    Ok(report)
}

pub(super) async fn initialize(path: &Path) -> Result<()> {
    initialize_format(path, "sha1").await
}
pub(super) async fn initialize_format(path: &Path, format: &str) -> Result<()> {
    dispatch::call(Request::Initialize { path, format })
        .await?
        .unit()
}
pub(super) async fn validate_tree(
    repository: &Path,
    revision: &str,
    budget: u64,
    max_file: u64,
) -> Result<()> {
    validate_tree_bytes(repository, revision, budget, max_file)
        .await
        .map(|_| ())
}
pub(super) async fn validate_tree_bytes(
    repository: &Path,
    revision: &str,
    budget: u64,
    max_file: u64,
) -> Result<u64> {
    match dispatch::call(Request::ValidateTree {
        repository,
        revision,
        budget,
        max_file,
    })
    .await?
    {
        Reply::Bytes(value) => Ok(value),
        _ => bail!("optional transfer runtime returned an invalid result"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_snapshot_refuses_replaced_roots_and_never_follows_parent_or_file_links() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "chimaera-mirror-confined-{}",
            chimaera_core::generate_token()
        ));
        let project = root.join("project");
        let private = root.join("private");
        fs::create_dir_all(project.join("sub")).unwrap();
        fs::create_dir_all(&private).unwrap();
        let project = project.canonicalize().unwrap();
        fs::write(project.join("sub/file.txt"), "project bytes").unwrap();
        fs::write(private.join("file.txt"), "unrelated private bytes").unwrap();
        let directory = pin_project(&project).unwrap();
        let paths = || (vec![PathBuf::from("sub/file.txt")], Vec::new());
        fs::rename(project.join("sub"), project.join("old-sub")).unwrap();
        symlink(&private, project.join("sub")).unwrap();
        let stage = root.join("parent-link");
        let report = copy_tree_pinned(&project, &directory, &stage, paths(), 1000, 1000).unwrap();
        assert_eq!(report.files, 0);
        assert_eq!(
            report.left_out.unwrap(),
            vec![PathBuf::from("sub/file.txt")]
        );
        assert!(!stage.join("sub/file.txt").exists());
        fs::remove_file(project.join("sub")).unwrap();
        fs::rename(project.join("old-sub"), project.join("sub")).unwrap();
        fs::remove_file(project.join("sub/file.txt")).unwrap();
        symlink(private.join("file.txt"), project.join("sub/file.txt")).unwrap();
        let stage = root.join("file-link");
        let report = copy_tree_pinned(&project, &directory, &stage, paths(), 1000, 1000).unwrap();
        assert_eq!(report.files, 0);
        assert!(!stage.join("sub/file.txt").exists());
        fs::rename(&project, root.join("original-project")).unwrap();
        fs::create_dir_all(project.join("sub")).unwrap();
        fs::write(project.join("sub/file.txt"), "replacement project bytes").unwrap();
        let stage = root.join("replacement-root");
        assert!(copy_tree_pinned(&project, &directory, &stage, paths(), 1000, 1000).is_err());
        assert!(!stage.exists());
        fs::remove_dir_all(&project).unwrap();
        symlink(&private, &project).unwrap();
        assert!(pin_project(&project).is_err());
        // A preexisting ancestor link must not nominate a different root.
        fs::create_dir(private.join("nested")).unwrap();
        assert!(pin_project(&project.join("nested")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
