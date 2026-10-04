//! Exact derived-cache publication remains with the original public owner.
use super::{
    transfer_dispatch::{self, TransferReply, TransferRequest},
    transfer_host::TransferHost,
};
use anyhow::{ensure, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
const QUARANTINE: &str = "working-tree.quarantine";
const REBUILD: &str = "working-tree.rebuild.git";
fn directory(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "invalid shadow recovery directory"
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
fn parent(shadow: &Path) -> Result<&Path> {
    shadow.parent().context("shadow parent unavailable")
}

/// Non-Clone prepared publication bound to one original host/cache/generation.
/// Only the host can construct its fixed derived-cache paths.
pub struct PreparedShadow {
    owner: Arc<TransferHost>,
    parent: fs::File,
    shadow: PathBuf,
    previous: PathBuf,
    fresh: PathBuf,
    interrupted: bool,
}
impl PreparedShadow {
    pub(crate) fn new(
        owner: Arc<TransferHost>,
        shadow: PathBuf,
        directory: fs::File,
        interrupted: bool,
    ) -> Self {
        let parent = shadow.parent().expect("validated shadow parent");
        Self {
            previous: parent.join(QUARANTINE).join("previous.git"),
            fresh: parent.join(REBUILD),
            owner,
            parent: directory,
            shadow,
            interrupted,
        }
    }
    fn same_cache(&self, cache: &Arc<tokio::sync::OwnedMutexGuard<()>>) -> Result<()> {
        self.owner.same_cache(cache)
    }
    fn install_checked(
        self,
        current: impl FnOnce() -> Result<()>,
        publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    ) -> Result<()> {
        self.owner.filesystem_current()?;
        let parent = parent(&self.shadow)?;
        let quarantine = self
            .previous
            .parent()
            .context("quarantine parent unavailable")?;
        ensure!(directory(&self.fresh)?, "prepared shadow is missing");
        sync_rebuild(&self.fresh)?;
        // Preserve pre-durability cache evidence as bytes, including corrupted
        // objects; syncing it does not make it a trusted canonical snapshot.
        sync_rebuild(if self.interrupted {
            &self.previous
        } else {
            &self.shadow
        })?;
        current()?;
        use std::os::unix::fs::MetadataExt;
        let original = self.parent.metadata()?;
        let named = self.owner.directory(parent)?.metadata()?;
        ensure!(
            original.dev() == named.dev() && original.ino() == named.ino(),
            "Original shadow parent changed before publication"
        );
        if self.interrupted {
            ensure!(
                !directory(&self.shadow)? && directory(&self.previous)?,
                "interrupted shadow recovery changed"
            );
        } else {
            ensure!(
                directory(&self.shadow)? && !directory(quarantine)?,
                "shadow quarantine already exists"
            );
            fs::create_dir(quarantine)?;
            sync_directory(parent)?;
            fs::rename(&self.shadow, &self.previous)?;
            sync_directory(quarantine)?;
            sync_directory(parent)?;
        }
        // If this rename fails or the process stops here, previous.git remains
        // the verified local baseline. A later fenced hydrate rebuilds afresh.
        publish(&self.fresh, &self.shadow)?;
        sync_directory(parent)?;
        Ok(())
    }
}
#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    anyhow::bail!("shadow recovery requires durable directory replacement on this platform")
}

fn sync_rebuild(root: &Path) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut directories = Vec::new();
    let mut count = 0;
    while let Some(path) = pending.pop() {
        count += 1;
        ensure!(
            count <= super::policy::MAX_PATHS,
            "shadow recovery exceeds filesystem limit"
        );
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "shadow recovery contains a symlink"
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                ensure!(
                    count + pending.len() <= super::policy::MAX_PATHS,
                    "shadow recovery exceeds filesystem limit"
                );
                pending.push(entry?.path());
            }
            directories.push(path);
        } else {
            ensure!(
                metadata.is_file(),
                "shadow recovery contains a special file"
            );
            fs::File::open(path)?.sync_all()?;
        }
    }
    for directory in directories.into_iter().rev() {
        sync_directory(&directory)?;
    }
    Ok(())
}

pub(super) async fn prepare(
    shadow: &Path,
    incoming: &Path,
    cache: Arc<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<Option<PreparedShadow>> {
    let original = transfer_dispatch::owner()?;
    original.host.same_cache(&cache)?;
    match transfer_dispatch::call(TransferRequest::PrepareShadow { shadow, incoming }).await? {
        TransferReply::ShadowPrepared(repair) => Ok(repair),
        _ => anyhow::bail!("optional transfer runtime returned an invalid result"),
    }
}
pub(super) async fn install(
    repair: PreparedShadow,
    cache: std::sync::Arc<tokio::sync::OwnedMutexGuard<()>>,
    configuration: tokio::sync::OwnedMutexGuard<()>,
    current: impl FnOnce() -> Result<()> + Send + 'static,
) -> Result<std::sync::Arc<tokio::sync::OwnedMutexGuard<()>>> {
    let (result, cache) = tokio::task::spawn_blocking(move || {
        let _configuration = configuration;
        let result = repair.same_cache(&cache).and_then(|()| {
            repair.install_checked(current, |source, destination| {
                fs::rename(source, destination)
            })
        });
        (result, cache)
    })
    .await?;
    result?;
    Ok(cache)
}

#[cfg(all(unix, feature = "daemon-extension-fixture"))]
impl PreparedShadow {
    pub fn fixture_install(self) -> Result<()> {
        self.install_checked(
            || Ok(()),
            |source, destination| fs::rename(source, destination),
        )
    }
    pub fn fixture_install_with(
        self,
        publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    ) -> Result<()> {
        self.install_checked(|| Ok(()), publish)
    }
    pub fn fixture_install_checked(
        self,
        current: impl FnOnce() -> Result<()>,
        publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    ) -> Result<()> {
        self.install_checked(current, publish)
    }
}
#[cfg(all(unix, feature = "daemon-extension-fixture"))]
pub async fn fixture_install(
    repair: PreparedShadow,
    cache: Arc<tokio::sync::OwnedMutexGuard<()>>,
    configuration: tokio::sync::OwnedMutexGuard<()>,
    current: impl FnOnce() -> Result<()> + Send + 'static,
) -> Result<Arc<tokio::sync::OwnedMutexGuard<()>>> {
    install(repair, cache, configuration, current).await
}
