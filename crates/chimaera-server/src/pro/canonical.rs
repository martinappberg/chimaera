//! Local conflict preservation is outside the mirrored project. Adopting the
//! canonical checkpoint must not destroy unpublished local edits or re-upload
//! those edits as new canonical project files.
use anyhow::{ensure, Context, Result};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};
const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_FILES: usize = 4096;
pub(super) struct Conflicts {
    root: PathBuf,
    bytes: u64,
    files: usize,
}
impl Conflicts {
    pub fn open(root: &Path) -> Result<Self> {
        let mut result = Self {
            root: root.to_owned(),
            bytes: 0,
            files: 0,
        };
        if !root.try_exists()? {
            return Ok(result);
        }
        let mut pending = vec![root.to_owned()];
        let mut paths = 0;
        while let Some(path) = pending.pop() {
            ensure!(
                !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "conflict storage contains symlink"
            );
            for entry in std::fs::read_dir(path)? {
                paths += 1;
                ensure!(
                    paths <= MAX_FILES * 2,
                    "local conflict storage exceeds limit"
                );
                let entry = entry?;
                let metadata = entry.metadata()?;
                ensure!(
                    !entry.file_type()?.is_symlink(),
                    "conflict storage contains symlink"
                );
                if metadata.is_dir() {
                    pending.push(entry.path());
                } else {
                    ensure!(metadata.is_file(), "invalid local conflict entry");
                    result.files += 1;
                    result.bytes = result
                        .bytes
                        .checked_add(metadata.len())
                        .context("local conflict size overflow")?;
                }
                ensure!(
                    result.files <= MAX_FILES && result.bytes <= MAX_BYTES,
                    "local conflict storage exceeds limit"
                );
            }
        }
        Ok(result)
    }
    pub fn preserve(&mut self, source: &Path, relative: &Path) -> Result<()> {
        ensure!(
            super::policy::allowed_path(relative),
            "unsafe conflict path"
        );
        let metadata = std::fs::symlink_metadata(source)?;
        ensure!(
            metadata.is_file() && metadata.len() <= super::policy::MAX_FILE_BYTES,
            "local conflict file exceeds limit"
        );
        ensure!(
            self.files < MAX_FILES
                && self
                    .bytes
                    .checked_add(metadata.len())
                    .is_some_and(|v| v <= MAX_BYTES),
            "local conflict storage exceeds limit"
        );
        let directory = self.root.join(chimaera_core::generate_token());
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o700))?;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let target = directory.join(relative);
        std::fs::create_dir_all(target.parent().context("invalid conflict destination")?)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(&target)?;
        let mut input = std::fs::File::open(source)?.take(super::policy::MAX_FILE_BYTES + 1);
        let copied = std::io::copy(&mut input, &mut output)?;
        ensure!(
            copied == metadata.len() && copied <= super::policy::MAX_FILE_BYTES,
            "local conflict changed while preserving it"
        );
        output.flush()?;
        output.sync_all()?;
        self.files += 1;
        self.bytes += copied;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserved_copies_never_replace_previous_conflicts_and_caps_fail_before_original_changes() {
        let root_dir = std::env::temp_dir().join(format!(
            "chimaera-local-conflicts-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root_dir).unwrap();
        let source = root_dir.join("source");
        let root = root_dir.join("conflicts");
        std::fs::write(&source, "first local edit").unwrap();
        let mut store = Conflicts::open(&root).unwrap();
        store.preserve(&source, Path::new("file")).unwrap();
        std::fs::write(&source, "second local edit").unwrap();
        store.preserve(&source, Path::new("file")).unwrap();
        let mut values = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path().join("file")).unwrap())
            .collect::<Vec<_>>();
        values.sort();
        assert_eq!(values, ["first local edit", "second local edit"]);
        store.bytes = MAX_BYTES;
        assert!(store.preserve(&source, Path::new("file")).is_err());
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            "second local edit"
        );
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 2);
        std::fs::remove_dir_all(root_dir).unwrap();
    }
}
