//! Rebuild only damaged derived shadow history; retain its entire prior store.
use super::{mirror, transport};
use anyhow::{ensure, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
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

pub(super) async fn baseline(shadow: &Path) -> Result<Option<PathBuf>> {
    let shadow = shadow.to_path_buf();
    tokio::task::spawn_blocking(move || {
        if directory(&shadow)? {
            return Ok(Some(shadow));
        }
        let quarantine = parent(&shadow)?.join(QUARANTINE);
        if !directory(&quarantine)? {
            return Ok(None);
        }
        let previous = quarantine.join("previous.git");
        ensure!(
            directory(&previous)?,
            "interrupted shadow recovery needs its preserved baseline"
        );
        Ok(Some(previous))
    })
    .await?
}

async fn fetch(target: &Path, source: &Path) -> Result<()> {
    transport::git_output(
        transport::git(target, None).await?,
        &[
            "fetch",
            "--no-tags",
            source.to_str().context("invalid shadow source")?,
            "+refs/heads/*:refs/heads/*",
        ],
        vec![],
    )
    .await?;
    Ok(())
}
async fn complete(repository: &Path) -> Result<()> {
    // fsck verifies object contents and links, including packed objects. Merely
    // resolving branch names or their top-level trees is not a complete check.
    transport::git_output(
        transport::git(repository, None).await?,
        &[
            "fsck",
            "--full",
            "--strict",
            "--no-reflogs",
            "--no-dangling",
        ],
        vec![],
    )
    .await?;
    for branch in ["main", "config", "handoff"] {
        transport::git_output(
            transport::git(repository, None).await?,
            &["cat-file", "-e", &format!("refs/heads/{branch}^{{commit}}")],
            vec![],
        )
        .await?;
    }
    Ok(())
}
async fn damaged_reference(repository: &Path) -> Result<bool> {
    for branch in ["main", "config", "handoff"] {
        let mut command = transport::git(repository, None).await?;
        command.args(["rev-parse", "--verify", &format!("refs/heads/{branch}")]);
        let reference = transport::run(command, vec![], Duration::from_secs(5), 128).await?;
        if !reference.success {
            continue;
        }
        let oid = std::str::from_utf8(&reference.stdout)?.trim();
        ensure!(
            oid.len() == 40 && oid.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid shadow object identity"
        );
        for kind in ["commit", "tree"] {
            let mut command = transport::git(repository, None).await?;
            command.args(["cat-file", "-e", &format!("{oid}^{{{kind}}}")]);
            let object = transport::run(command, vec![], Duration::from_secs(5), 4096).await?;
            if !object.success {
                ensure!(
                    object.object_damage(),
                    "shadow object could not be checked safely"
                );
                return Ok(true);
            }
        }
    }
    let mut command = transport::git(repository, None).await?;
    command.args([
        "fsck",
        "--full",
        "--strict",
        "--no-reflogs",
        "--no-dangling",
    ]);
    let closure = transport::run(
        command,
        vec![],
        Duration::from_secs(45),
        transport::PATH_CAP,
    )
    .await?;
    ensure!(
        closure.success || closure.object_damage(),
        "shadow object closure could not be checked safely"
    );
    Ok(!closure.success)
}

pub(super) struct Repair {
    shadow: PathBuf,
    previous: PathBuf,
    fresh: PathBuf,
    interrupted: bool,
}

pub(super) async fn prepare(
    shadow: &Path,
    incoming: &Path,
    cache: std::sync::Arc<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<Option<Repair>> {
    let old = baseline(shadow).await?;
    if old.as_deref().is_none_or(|old| old == shadow) {
        mirror::initialize(shadow).await?;
        let refreshed = match fetch(shadow, incoming).await {
            Ok(()) => complete(shadow).await,
            Err(error) => Err(error),
        };
        match refreshed {
            Ok(()) => return Ok(None),
            Err(error) if !damaged_reference(shadow).await? => return Err(error),
            Err(_) => {}
        }
    }
    let old = old.context("damaged shadow has no preserved baseline")?;
    ensure!(
        damaged_reference(&old).await?,
        "shadow replacement requires objective object damage"
    );
    complete(incoming).await?;
    let quarantine = parent(shadow)?.join(QUARANTINE);
    let previous = quarantine.join("previous.git");
    let interrupted = old == previous;
    let fresh = parent(shadow)?.join(REBUILD);
    let check_quarantine = quarantine.clone();
    let prepare_fresh = fresh.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        // The filesystem task outlives a canceled caller. Keep the exact cache
        // exclusion until canceled staging cleanup has actually completed.
        let _cache = cache;
        if !interrupted && directory(&check_quarantine)? {
            // An interruption before moving the old repository leaves only an
            // empty reservation. remove_dir cannot discard any saved content.
            fs::remove_dir(&check_quarantine)
                .context("a prior damaged shadow is already preserved")?;
            sync_directory(
                check_quarantine
                    .parent()
                    .context("quarantine parent unavailable")?,
            )?;
        }
        // This fixed, never-published rebuild directory contains only copied
        // canonical cache data. Reclaim a canceled preparation, never history.
        if directory(&prepare_fresh)? {
            fs::remove_dir_all(&prepare_fresh)?;
        }
        Ok(())
    })
    .await??;
    mirror::initialize(&fresh).await?;
    fetch(&fresh, incoming).await?;
    complete(&fresh).await?;
    Ok(Some(Repair {
        shadow: shadow.to_path_buf(),
        previous,
        fresh,
        interrupted,
    }))
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

impl Repair {
    /// Caller retains the owned cache/configuration guards in this blocking
    /// finalizer, even if the HTTP request is canceled. No history is deleted.
    #[cfg(test)]
    fn install(self) -> Result<()> {
        self.install_with(|source, destination| fs::rename(source, destination))
    }

    #[cfg(test)]
    fn install_with(self, publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>) -> Result<()> {
        self.install_checked(|| Ok(()), publish)
    }

    fn install_checked(
        self,
        current: impl FnOnce() -> Result<()>,
        publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    ) -> Result<()> {
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

pub(super) async fn install(
    repair: Repair,
    cache: std::sync::Arc<tokio::sync::OwnedMutexGuard<()>>,
    configuration: tokio::sync::OwnedMutexGuard<()>,
    current: impl FnOnce() -> Result<()> + Send + 'static,
) -> Result<std::sync::Arc<tokio::sync::OwnedMutexGuard<()>>> {
    let (result, cache) = tokio::task::spawn_blocking(move || {
        let _configuration = configuration;
        let result = repair.install_checked(current, |source, destination| {
            fs::rename(source, destination)
        });
        (result, cache)
    })
    .await?;
    result?;
    Ok(cache)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    async fn prepare(shadow: &Path, incoming: &Path) -> Result<Option<Repair>> {
        let guard = Arc::new(Arc::new(Mutex::new(())).lock_owned().await);
        super::prepare(shadow, incoming, guard).await
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "synthetic Git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
    async fn fixture() -> (PathBuf, PathBuf, PathBuf, String) {
        let root = std::env::temp_dir().join(format!(
            "chimaera-shadow-repair-{}",
            chimaera_core::generate_token()
        ));
        let incoming = root.join("incoming.git");
        let shadow = root.join("working-tree.git");
        let tree = root.join("tree");
        fs::create_dir_all(&tree).unwrap();
        mirror::initialize(&incoming).await.unwrap();
        for branch in ["main", "config", "handoff"] {
            fs::write(tree.join("fixture.txt"), branch).unwrap();
            mirror::commit_tree(&incoming, &tree, branch).await.unwrap();
        }
        mirror::initialize(&shadow).await.unwrap();
        fetch(&shadow, &incoming).await.unwrap();
        // Unique outgoing history matches the live failure; the incoming store
        // cannot refill this missing commit as an ordinary object fetch.
        fs::write(tree.join("fixture.txt"), "outgoing handoff evidence").unwrap();
        mirror::commit_tree(&shadow, &tree, "handoff")
            .await
            .unwrap();
        fs::write(
            root.join("unpublished.txt"),
            "unpublished local cache evidence",
        )
        .unwrap();
        let unique = git(
            &shadow,
            &[
                "hash-object",
                "-w",
                root.join("unpublished.txt").to_str().unwrap(),
            ],
        );
        git(
            &shadow,
            &["update-ref", "refs/preserved/unpublished", &unique],
        );
        (root, shadow, incoming, unique)
    }
    fn damage(shadow: &Path, missing: bool) -> (String, PathBuf) {
        let oid = git(shadow, &["rev-parse", "refs/heads/handoff"]);
        let object = shadow.join("objects").join(&oid[..2]).join(&oid[2..]);
        assert!(object.is_file());
        fs::remove_file(&object).unwrap();
        if !missing {
            fs::write(&object, []).unwrap();
        }
        (oid, object)
    }

    #[tokio::test]
    async fn damaged_shadow_preserves_unique_objects_and_rebuilds_complete_history() {
        for missing in [false, true] {
            let (root, shadow, incoming, unique) = fixture().await;
            let (damaged, _) = damage(&shadow, missing);
            assert!(
                fetch(&shadow, &incoming).await.is_err(),
                "ordinary fetch must reproduce the live failure"
            );
            let repair = prepare(&shadow, &incoming).await.unwrap().unwrap();
            repair.install().unwrap();
            complete(&shadow).await.unwrap();
            let previous = root.join(QUARANTINE).join("previous.git");
            assert_eq!(
                git(&previous, &["rev-parse", "refs/heads/handoff"]),
                damaged
            );
            assert_eq!(
                git(&previous, &["rev-parse", "refs/preserved/unpublished"]),
                unique
            );
            assert_eq!(
                git(&previous, &["cat-file", "blob", &unique]),
                "unpublished local cache evidence"
            );
            for branch in ["main", "config", "handoff"] {
                assert_eq!(
                    git(&shadow, &["rev-parse", &format!("refs/heads/{branch}")]),
                    git(&incoming, &["rev-parse", &format!("refs/heads/{branch}")])
                );
            }
            damage(&shadow, false);
            assert!(
                prepare(&shadow, &incoming).await.is_err(),
                "a second failure must not overwrite the preserved repository"
            );
            assert_eq!(
                git(&previous, &["cat-file", "blob", &unique]),
                "unpublished local cache evidence"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn incomplete_incoming_and_healthy_failures_never_replace_shadow() {
        let (root, shadow, incoming, unique) = fixture().await;
        let invalid_source = root.join("not-a-repository");
        fs::create_dir(&invalid_source).unwrap();
        assert!(prepare(&shadow, &invalid_source).await.is_err());
        assert!(!root.join(QUARANTINE).exists());
        assert_eq!(
            git(&shadow, &["rev-parse", "refs/preserved/unpublished"]),
            unique
        );
        damage(&incoming, false);
        damage(&shadow, true);
        assert!(prepare(&shadow, &incoming).await.is_err());
        assert!(!root.join(QUARANTINE).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn interrupted_swap_reuses_preserved_baseline_without_overwriting_it() {
        let (root, shadow, incoming, unique) = fixture().await;
        damage(&shadow, false);
        let repair = prepare(&shadow, &incoming).await.unwrap().unwrap();
        let quarantine = root.join(QUARANTINE);
        fs::create_dir(&quarantine).unwrap();
        fs::rename(&shadow, quarantine.join("previous.git")).unwrap();
        // Simulate an interruption after quarantine, before installation.
        drop(repair);
        assert_eq!(
            baseline(&shadow).await.unwrap(),
            Some(quarantine.join("previous.git"))
        );
        prepare(&shadow, &incoming)
            .await
            .unwrap()
            .unwrap()
            .install()
            .unwrap();
        complete(&shadow).await.unwrap();
        assert_eq!(
            git(
                &quarantine.join("previous.git"),
                &["cat-file", "blob", &unique]
            ),
            "unpublished local cache evidence"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cancellation_retains_cache_exclusion_until_blocking_swap_finishes() {
        let (root, shadow, incoming, unique) = fixture().await;
        damage(&shadow, false);
        let repair = prepare(&shadow, &incoming).await.unwrap().unwrap();
        let locks = super::super::ProState::new(root.join("control"));
        let cache = locks.cache("w-swap-a").unwrap();
        let original_mutex = Arc::as_ptr(&cache);
        let configuration = Arc::new(Mutex::new(()));
        let cache_guard = Arc::new(cache.clone().lock_owned().await);
        let configuration_guard = configuration.clone().lock_owned().await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let task = tokio::spawn(install(
            repair,
            cache_guard,
            configuration_guard,
            move || {
                entered_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(15)).unwrap();
                Ok(())
            },
        ));
        entered_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(cache);
        let cache = locks.cache("w-swap-a").unwrap();
        assert_eq!(
            Arc::as_ptr(&cache),
            original_mutex,
            "weak registry must retain the mutex held by the detached finalizer"
        );
        assert!(cache.try_lock().is_err());
        assert!(configuration.try_lock().is_err());
        let sibling = Arc::new(locks.cache("w-swap-b").unwrap().lock_owned().await);
        transport::cache_scope("w-swap-b", sibling, async {
            let sibling_path = root.join("sibling.git");
            mirror::initialize(&sibling_path).await.unwrap();
            fetch(&sibling_path, &incoming).await.unwrap();
            complete(&sibling_path).await.unwrap();
        })
        .await;
        resume_tx.send(()).unwrap();
        let _guard = tokio::time::timeout(Duration::from_secs(5), cache.lock())
            .await
            .unwrap();
        complete(&shadow).await.unwrap();
        assert_eq!(
            git(
                &root.join(QUARANTINE).join("previous.git"),
                &["cat-file", "blob", &unique]
            ),
            "unpublished local cache evidence"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn empty_quarantine_and_failed_publication_preserve_retryable_history() {
        let (root, shadow, incoming, unique) = fixture().await;
        damage(&shadow, false);
        fs::create_dir(root.join(QUARANTINE)).unwrap();
        let repair = prepare(&shadow, &incoming).await.unwrap().unwrap();
        assert!(repair
            .install_with(|_, _| Err(std::io::Error::other("synthetic rename failure")))
            .is_err());
        let previous = root.join(QUARANTINE).join("previous.git");
        assert!(!shadow.exists());
        assert_eq!(
            git(&previous, &["cat-file", "blob", &unique]),
            "unpublished local cache evidence"
        );
        assert!(mirror::initialize(&shadow).await.is_err());
        prepare(&shadow, &incoming)
            .await
            .unwrap()
            .unwrap()
            .install()
            .unwrap();
        complete(&shadow).await.unwrap();
        assert_eq!(
            git(&previous, &["cat-file", "blob", &unique]),
            "unpublished local cache evidence"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn changed_authority_preserves_original_cache_and_does_not_publish_rebuild() {
        let (root, shadow, incoming, unique) = fixture().await;
        damage(&shadow, false);
        let repair = prepare(&shadow, &incoming).await.unwrap().unwrap();
        assert!(repair
            .install_checked(
                || anyhow::bail!("synthetic owner or account change"),
                |_, _| panic!("changed authority must never publish")
            )
            .is_err());
        assert!(!root.join(QUARANTINE).exists());
        assert_eq!(
            git(&shadow, &["cat-file", "blob", &unique]),
            "unpublished local cache evidence"
        );
        assert!(root.join(REBUILD).is_dir());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn corrupt_blob_is_detected_even_when_ref_commit_and_tree_are_readable() {
        let (root, shadow, incoming, _) = fixture().await;
        let oid = git(&shadow, &["rev-parse", "refs/heads/handoff:fixture.txt"]);
        let object = shadow.join("objects").join(&oid[..2]).join(&oid[2..]);
        fs::remove_file(&object).unwrap();
        fs::write(&object, b"invalid synthetic compressed object").unwrap();
        assert!(damaged_reference(&shadow).await.unwrap());
        prepare(&shadow, &incoming)
            .await
            .unwrap()
            .unwrap()
            .install()
            .unwrap();
        complete(&shadow).await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
