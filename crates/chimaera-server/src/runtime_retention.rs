//! Keep the activated agent package; defer reclamation while a session can
//! still resolve companions from an older package. Locks live on the shared
//! filesystem, so workspace daemons on different cluster nodes cooperate.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;

use crate::agents::AgentKind;
use crate::AppState;

const ENTRY_LIMIT: usize = 1024;

fn open_lock(root: &Path, name: &str) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(name))
}

/// Plain terminals also protect installed packages: agents started by typing
/// into a shell must not lose companions during an update in another window.
/// Holding every kind for a plain shell is conservative and uses a fixed small
/// set of locks. Failed preparation/spawn drops the guards without a registry row.
pub(crate) struct Usage(Vec<(PathBuf, AgentKind, LeaseLock)>);

/// A held flock (usage lease, removal guard, or the install lock —
/// `runtimes::InstallLock` is this type). A fork inherits the same open file
/// description until exec, so closing our descriptor alone would leave the
/// lock held by that unrelated child; an explicit unlock ends it at once.
#[derive(Debug)]
pub(crate) struct LeaseLock(pub(crate) File);
impl Drop for LeaseLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            tracing::warn!(%error, "agent lock release deferred until descriptor close");
        }
    }
}

pub(crate) async fn acquire(
    state: &AppState,
    agent: Option<AgentKind>,
    mut executables: Vec<PathBuf>,
) -> anyhow::Result<(Usage, Vec<PathBuf>)> {
    let mut roots = vec![state.managed_root.clone()];
    roots.extend(state.legacy_managed_root.clone());
    tokio::task::spawn_blocking(move || {
        let mut locks = Vec::new();
        for root in roots {
            std::fs::create_dir_all(&root)?;
            let canonical_root = root.canonicalize()?;
            for kind in AgentKind::ALL {
                if agent.is_some_and(|agent| agent != kind) {
                    continue;
                }
                let file = open_lock(&root, &format!(".{}.use", kind.as_str()))
                    .with_context(|| format!("Cannot protect agent files in {}", root.display()))?;
                file.try_lock_shared()
                    .map_err(std::io::Error::from)
                    .context("Agent storage is being cleaned up. Try again in a moment")?;
                let file = LeaseLock(file);
                if agent.is_none() {
                    locks.push((root.clone(), kind, file));
                    continue;
                }
                // Pin argv to the same package whose lease we acquire. An
                // activation symlink may change before the child executes.
                for executable in &mut executables {
                    let resolved = match executable.canonicalize() {
                        Ok(path) => path,
                        Err(error) if executable.starts_with(&root) => return Err(error.into()),
                        Err(_) => continue,
                    };
                    let Ok(relative) = resolved.strip_prefix(canonical_root.join(kind.as_str()))
                    else {
                        continue;
                    };
                    let Some(Component::Normal(version)) = relative.components().next() else {
                        continue;
                    };
                    let Some(version) = version.to_str().filter(|version| valid_version(version))
                    else {
                        continue;
                    };
                    let lease = open_lock(&root, &format!(".{}-{version}.use", kind.as_str()))?;
                    lease.try_lock_shared().map_err(std::io::Error::from)?;
                    locks.push((root.clone(), kind, LeaseLock(lease)));
                    *executable = resolved;
                }
            }
        }
        Ok((Usage(locks), executables))
    })
    .await?
}

pub(crate) fn watch(state: Arc<AppState>, id: String, usage: Usage) {
    if usage.0.is_empty() {
        return;
    }
    tokio::spawn(async move {
        while state.sessions.get(&id).is_some()
            || state.chat.get(&id).is_some_and(|info| info.alive)
            || crate::lock(&state.chat_switching).contains_key(&id)
        {
            tokio::time::sleep(crate::agents::poll_interval()).await;
        }
        // Registry removal can precede the driver's polite shutdown + kill
        // grace. Keep companions through that teardown too.
        tokio::time::sleep(chimaera_agent::driver::KILL_GRACE + std::time::Duration::from_secs(2))
            .await;
        let roots: Vec<_> = usage
            .0
            .iter()
            .map(|(root, kind, _)| (root.clone(), *kind))
            .collect();
        drop(usage);
        if !state.stopping.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = tokio::task::spawn_blocking(move || {
                for (root, kind) in roots {
                    cleanup(&root, kind);
                }
            })
            .await;
        }
    });
}

pub(crate) fn boot(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut restored = state.restored.subscribe();
        if restored.wait_for(|done| *done).await.is_err() {
            return;
        }
        let root = state.managed_root.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for kind in AgentKind::ALL {
                cleanup(&root, kind);
            }
        })
        .await;
    });
}

fn cleanup(root: &Path, kind: AgentKind) {
    if !root.join(kind.as_str()).is_dir() {
        return;
    }
    // Same lock as install/update/remove: never sweep a package being staged
    // or activated, and never unlink the lock file itself.
    let result = (|| -> anyhow::Result<()> {
        let install = open_lock(root, &format!(".{}.lock", kind.as_str()))?;
        if install.try_lock().is_err() {
            return Ok(());
        }
        let _install = LeaseLock(install);
        prune_locked(root, kind)
    })();
    if let Err(error) = result {
        tracing::warn!(%error, agent = kind.as_str(), "agent storage cleanup deferred");
    }
}

/// Caller holds the install lock. Take the broad lock first to exclude new
/// sessions while checking every package lease, and retain all guards through
/// removal. None means a terminal, agent, or older daemon still needs the files.
pub(crate) fn lock_removal(root: &Path, kind: AgentKind) -> anyhow::Result<Option<Vec<LeaseLock>>> {
    let tree = root.join(kind.as_str());
    if !tree.exists() {
        return Ok(Some(Vec::new()));
    }
    let usage = open_lock(root, &format!(".{}.use", kind.as_str()))?;
    match usage.try_lock().map_err(std::io::Error::from) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let usage = LeaseLock(usage);
    if untracked_daemon(root)? {
        return Ok(None);
    }
    let mut locks = vec![usage];
    for entry in bounded_entries(&tree)? {
        let name = entry.file_name();
        let Some(version) = name.to_str().filter(|name| valid_version(name)) else {
            continue;
        };
        let lease = open_lock(root, &format!(".{}-{version}.use", kind.as_str()))?;
        match lease.try_lock().map_err(std::io::Error::from) {
            Ok(()) => locks.push(LeaseLock(lease)),
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(locks))
}

/// Caller holds the install lock. Plain terminals protect the whole tree;
/// agent sessions lease their exact packages, including companion executables.
pub(crate) fn prune_locked(root: &Path, kind: AgentKind) -> anyhow::Result<()> {
    let tree = root.join(kind.as_str());
    if !tree.is_dir() || std::fs::symlink_metadata(&tree)?.file_type().is_symlink() {
        return Ok(());
    }
    let usage = open_lock(root, &format!(".{}.use", kind.as_str()))?;
    match usage.try_lock().map_err(std::io::Error::from) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let _usage = LeaseLock(usage);
    if untracked_daemon(root)? {
        return Ok(());
    }
    let mut keep = vec![active_version(root, kind, kind.as_str())?];
    if kind == AgentKind::Antigravity && root.join("bin/agy-acp").exists() {
        keep.push(active_version(root, kind, "agy-acp")?);
    }
    for entry in bounded_entries(&tree)? {
        let name = entry.file_name();
        let Some(version) = name.to_str().filter(|name| valid_version(name)) else {
            continue;
        };
        // Never follow a version-directory symlink or delete an unfamiliar
        // directory. Staging trees are owned exclusively by the installer.
        if keep.iter().any(|current| current == version)
            || !entry.file_type()?.is_dir()
            || !entry.path().join("bin").join(kind.as_str()).is_file()
        {
            continue;
        }
        let lease = open_lock(root, &format!(".{}-{version}.use", kind.as_str()))?;
        match lease.try_lock().map_err(std::io::Error::from) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error.into()),
        }
        let _lease = LeaseLock(lease);
        std::fs::remove_dir_all(entry.path()).with_context(|| {
            format!("Could not remove unused {} {version}", kind.product_name())
        })?;
        std::fs::remove_file(root.join(format!(".{}-{version}.use", kind.as_str())))?;
        tracing::info!(
            agent = kind.as_str(),
            version,
            "removed superseded agent package"
        );
    }
    if kind == AgentKind::Antigravity {
        prune_agy_chat(root, &keep)?;
    }
    Ok(())
}

fn prune_agy_chat(root: &Path, versions: &[String]) -> anyhow::Result<()> {
    let active = match root.join("bin/agy-acp").canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for version in versions {
        let lease = open_lock(root, &format!(".agy-{version}.use"))?;
        match lease.try_lock().map_err(std::io::Error::from) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error.into()),
        }
        let _lease = LeaseLock(lease);
        let chat = root.join("agy").join(version).join("chat");
        if !chat.is_dir() || std::fs::symlink_metadata(&chat)?.file_type().is_symlink() {
            continue;
        }
        for entry in bounded_entries(&chat)? {
            if entry.file_type()?.is_dir()
                && entry.file_name().to_str().is_some_and(valid_version)
                && !active.starts_with(entry.path().canonicalize()?)
                && entry.path().join("agy_acp_server.par").is_file()
                && entry.path().join("localharness_external").is_file()
            {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
    }
    Ok(())
}

fn valid_version(name: &str) -> bool {
    name.len() <= 128
        && name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
}

fn active_version(root: &Path, kind: AgentKind, executable: &str) -> anyhow::Result<String> {
    let target = std::fs::read_link(root.join("bin").join(executable))?;
    let components: Vec<_> = target.components().collect();
    anyhow::ensure!(
        components.len() >= 4
            && components[0] == Component::ParentDir
            && components[1] == Component::Normal(kind.as_str().as_ref()),
        "unrecognized agent activation link"
    );
    let Component::Normal(version) = components[2] else {
        anyhow::bail!("invalid activated version")
    };
    let version = version
        .to_str()
        .filter(|s| valid_version(s))
        .context("invalid activated version")?;
    anyhow::ensure!(
        root.join("bin").join(executable).is_file(),
        "activated agent package is missing"
    );
    anyhow::ensure!(
        root.join("bin")
            .join(executable)
            .canonicalize()?
            .starts_with(root.join(kind.as_str()).join(version).canonicalize()?),
        "agent activation link leaves its version directory"
    );
    Ok(version.to_string())
}

fn bounded_entries(path: &Path) -> anyhow::Result<Vec<std::fs::DirEntry>> {
    let entries: Vec<_> = std::fs::read_dir(path)?
        .take(ENTRY_LIMIT + 1)
        .collect::<Result<_, _>>()?;
    anyhow::ensure!(
        entries.len() <= ENTRY_LIMIT,
        "too many entries to safely inspect agent storage"
    );
    Ok(entries)
}

fn untracked_manifest(path: &Path) -> anyhow::Result<bool> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(16 * 1024).read_to_end(&mut bytes)?;
    let manifest: chimaera_core::Manifest = serde_json::from_slice(&bytes)?;
    Ok(!manifest.runtime_leases && (!manifest.written_here() || manifest.is_alive()))
}

/// Older daemons cannot participate in usage locks. A foreign-node manifest
/// is deliberately treated as live; its pid is meaningless on this node.
/// Closed cluster workspaces remove their manifest. An unknown/old daemon
/// therefore delays cleanup until it closes or upgrades, never risks its work.
fn untracked_daemon(root: &Path) -> anyhow::Result<bool> {
    let data = root.parent().context("agent storage has no parent")?;
    if untracked_manifest(&data.join("manifest.json"))? {
        return Ok(true);
    }
    let workspaces = data.join("cluster/w");
    if !workspaces.exists() {
        return Ok(false);
    }
    for entry in bounded_entries(&workspaces)? {
        if untracked_manifest(&entry.path().join("data/manifest.json"))? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "chimaera-retention-{}",
                chimaera_core::generate_token()
            ));
            std::fs::create_dir_all(dir.join("agents/bin")).unwrap();
            Self(dir)
        }
        fn root(&self) -> PathBuf {
            self.0.join("agents")
        }
        fn package(&self, kind: &str, version: &str) -> PathBuf {
            let dir = self.root().join(kind).join(version);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            std::fs::write(dir.join("bin").join(kind), "executable fixture").unwrap();
            std::fs::write(dir.join("companion"), "needed by a running session").unwrap();
            dir
        }
        fn activate(&self, kind: &str, version: &str) {
            symlink(
                format!("../{kind}/{version}/bin/{kind}"),
                self.root().join("bin").join(kind),
            )
            .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn cleanup_keeps_activated_package_and_removes_all_superseded_versions() {
        let f = Fixture::new();
        let old = f.package("codex", "0.155.1");
        let older = f.package("codex", "0.144.1");
        let current = f.package("codex", "0.159.2");
        f.activate("codex", "0.159.2");
        cleanup(&f.root(), AgentKind::Codex);
        assert!(!old.exists());
        assert!(!older.exists());
        assert!(current.join("companion").exists());
    }

    #[tokio::test]
    async fn another_workspace_and_plain_shell_defer_cleanup_until_last_use_ends() {
        let f = Fixture::new();
        let old = f.package("codex", "0.155.1");
        f.package("codex", "0.159.2");
        f.activate("codex", "0.159.2");
        let mut state = AppState::new(
            "token".into(),
            "host".into(),
            1,
            0,
            f.0.join("workspace"),
            f.0.join("config"),
        );
        state.managed_root = f.root();
        let first = acquire(&state, Some(AgentKind::Codex), vec![old.join("bin/codex")])
            .await
            .unwrap()
            .0;
        let shell = acquire(&state, None, vec![]).await.unwrap().0;
        cleanup(&f.root(), AgentKind::Codex);
        assert!(old.join("companion").exists());
        drop(first);
        cleanup(&f.root(), AgentKind::Codex);
        assert!(old.exists());
        drop(shell);
        cleanup(&f.root(), AgentKind::Codex);
        assert!(!old.exists());
    }

    #[tokio::test]
    async fn current_agent_does_not_keep_unused_older_packages_and_argv_is_pinned() {
        let f = Fixture::new();
        let old = f.package("codex", "0.155.1");
        let current = f.package("codex", "0.159.2");
        f.activate("codex", "0.159.2");
        let mut state = AppState::new(
            "token".into(),
            "host".into(),
            1,
            0,
            f.0.join("workspace"),
            f.0.join("config"),
        );
        state.managed_root = f.root();
        let (usage, binaries) = acquire(
            &state,
            Some(AgentKind::Codex),
            vec![f.root().join("bin/codex")],
        )
        .await
        .unwrap();
        assert_eq!(
            binaries,
            vec![current.join("bin/codex").canonicalize().unwrap()]
        );
        cleanup(&f.root(), AgentKind::Codex);
        assert!(!old.exists());
        let next = f.package("codex", "0.160.0");
        std::fs::remove_file(f.root().join("bin/codex")).unwrap();
        f.activate("codex", "0.160.0");
        cleanup(&f.root(), AgentKind::Codex);
        assert!(current.join("companion").exists());
        drop(usage);
        cleanup(&f.root(), AgentKind::Codex);
        assert!(!current.exists());
        assert!(next.join("companion").exists());
    }

    #[test]
    fn installation_lock_blocks_cleanup() {
        let f = Fixture::new();
        let old = f.package("codex", "0.155.1");
        f.package("codex", "0.159.2");
        f.activate("codex", "0.159.2");
        let install = open_lock(&f.root(), ".codex.lock").unwrap();
        install.try_lock().unwrap();
        let install = LeaseLock(install);
        cleanup(&f.root(), AgentKind::Codex);
        assert!(old.exists());
        drop(install);
        cleanup(&f.root(), AgentKind::Codex);
        assert!(!old.exists());
    }

    #[test]
    fn old_daemon_on_another_node_prevents_cleanup_until_it_closes() {
        let f = Fixture::new();
        let old = f.package("codex", "0.155.1");
        f.package("codex", "0.159.2");
        f.activate("codex", "0.159.2");
        let manifest = f.0.join("cluster/w/w-12345678/data/manifest.json");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "hostname": "some-other-compute-node", "port": 1, "token": "test", "pid": 1,
                "version": "0.1.0", "started_at": 1
            }))
            .unwrap(),
        )
        .unwrap();
        cleanup(&f.root(), AgentKind::Codex);
        assert!(old.exists());
        assert!(lock_removal(&f.root(), AgentKind::Codex).unwrap().is_none());
        std::fs::remove_file(manifest).unwrap();
        cleanup(&f.root(), AgentKind::Codex);
        assert!(!old.exists());
        assert!(lock_removal(&f.root(), AgentKind::Codex).unwrap().is_some());
    }

    #[tokio::test]
    async fn original_usage_and_removal_unlock_before_inherited_descriptors_close() {
        let f = Fixture::new();
        let package = f.package("codex", "0.159.2");
        f.activate("codex", "0.159.2");
        let mut state = AppState::new(
            "token".into(),
            "host".into(),
            1,
            0,
            f.0.join("workspace"),
            f.0.join("config"),
        );
        state.managed_root = f.root();
        for agent in [None, Some(AgentKind::Codex)] {
            let executables = agent
                .map(|_| package.join("bin/codex"))
                .into_iter()
                .collect();
            let (usage, _) = acquire(&state, agent, executables).await.unwrap();
            // try_clone models a pre-exec fork's identical open description.
            let inherited: Vec<_> = usage
                .0
                .iter()
                .map(|(_, _, lease)| lease.0.try_clone().unwrap())
                .collect();
            assert!(!inherited.is_empty());
            assert!(lock_removal(&f.root(), AgentKind::Codex).unwrap().is_none());
            drop(usage);
            let removal = lock_removal(&f.root(), AgentKind::Codex).unwrap().unwrap();
            drop(inherited);
            // Closing old inherited files cannot unlock the successor owner.
            assert!(acquire(&state, None, vec![]).await.is_err());
            let inherited_removal: Vec<_> = removal
                .iter()
                .map(|lease| lease.0.try_clone().unwrap())
                .collect();
            drop(removal);
            let (successor, _) = acquire(&state, None, vec![]).await.unwrap();
            drop(inherited_removal);
            assert!(lock_removal(&f.root(), AgentKind::Codex).unwrap().is_none());
            drop(successor);
            assert!(lock_removal(&f.root(), AgentKind::Codex).unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn uninstall_keeps_both_roots_while_another_workspace_uses_either() {
        use axum::extract::{Path as UrlPath, State};
        use axum::http::StatusCode;

        let shared = Fixture::new();
        let legacy = Fixture::new();
        let packages: Vec<_> = [&shared, &legacy]
            .into_iter()
            .map(|f| {
                let package = f.package("codex", "0.159.2");
                std::fs::set_permissions(
                    package.join("bin/codex"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap();
                f.activate("codex", "0.159.2");
                package
            })
            .collect();
        let make_state = |name: &str| {
            let mut state = AppState::new(
                "token".into(),
                "host".into(),
                1,
                0,
                shared.0.join(name).join("data"),
                shared.0.join(name).join("config"),
            );
            state.managed_root = shared.root();
            state.legacy_managed_root = Some(legacy.root());
            state
        };
        let owner = make_state("owner");
        let remover = Arc::new(make_state("remover"));
        // Exact package leases from either root, then a plain shell's broad
        // leases. Every refusal must leave BOTH installations intact.
        for executable in packages
            .iter()
            .map(|p| Some(p.join("bin/codex")))
            .chain([None])
        {
            let agent = executable.as_ref().map(|_| AgentKind::Codex);
            let (usage, _) = acquire(&owner, agent, executable.into_iter().collect())
                .await
                .unwrap();
            let response =
                crate::runtimes::uninstall_agent(State(remover.clone()), UrlPath("codex".into()))
                    .await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            for package in &packages {
                assert!(package.join("companion").exists());
            }
            drop(usage);
        }
        // The broad removal guard also fences a new session while deletion
        // is in progress, closing the check-then-spawn race.
        let removal = lock_removal(&shared.root(), AgentKind::Codex)
            .unwrap()
            .unwrap();
        assert!(acquire(
            &owner,
            Some(AgentKind::Codex),
            vec![packages[0].join("bin/codex")]
        )
        .await
        .is_err());
        drop(removal);
        let response =
            crate::runtimes::uninstall_agent(State(remover), UrlPath("codex".into())).await;
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(status, StatusCode::OK, "uninstall response: {body:?}");
        for package in packages {
            assert!(!package.exists());
        }
    }

    #[test]
    fn missing_or_malformed_activation_never_deletes_packages() {
        let f = Fixture::new();
        let old = f.package("codex", "0.155.1");
        cleanup(&f.root(), AgentKind::Codex);
        assert!(old.exists());
        symlink("../codex/0.159.2/bin/codex", f.root().join("bin/codex")).unwrap();
        cleanup(&f.root(), AgentKind::Codex);
        assert!(old.exists());
    }

    #[test]
    fn cleanup_does_not_follow_version_symlinks_or_remove_unknown_directories() {
        let f = Fixture::new();
        f.package("codex", "0.159.2");
        f.activate("codex", "0.159.2");
        let outside = f.0.join("personal");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), "user data").unwrap();
        symlink(&outside, f.root().join("codex/0.1.0")).unwrap();
        let unknown = f.root().join("codex/notes");
        std::fs::create_dir_all(&unknown).unwrap();
        cleanup(&f.root(), AgentKind::Codex);
        assert!(outside.join("keep").exists());
        assert!(unknown.exists());
    }

    #[test]
    fn agy_retains_both_active_entrypoints_and_only_current_chat_package() {
        let f = Fixture::new();
        let old = f.package("agy", "1.0.0");
        let current = f.package("agy", "1.1.2");
        f.activate("agy", "1.1.2");
        for release in ["0.1.0-100", "0.1.0-200"] {
            let chat = current.join("chat").join(release);
            std::fs::create_dir_all(&chat).unwrap();
            std::fs::write(chat.join("agy_acp_server.par"), "server").unwrap();
            std::fs::write(chat.join("localharness_external"), "harness").unwrap();
        }
        symlink(
            "../agy/1.1.2/chat/0.1.0-200/agy_acp_server.par",
            f.root().join("bin/agy-acp"),
        )
        .unwrap();
        cleanup(&f.root(), AgentKind::Antigravity);
        assert!(!old.exists());
        assert!(!current.join("chat/0.1.0-100").exists());
        assert!(current
            .join("chat/0.1.0-200/localharness_external")
            .exists());
    }
}
