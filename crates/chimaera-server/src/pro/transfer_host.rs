//! Original transfer transport ownership for trusted compiled policy.
//! This is not a sandbox or a renderer command interface. The executable,
//! environment base, credential source and root admission are host-owned.
use crate::{
    lock,
    pro::{self, protocol::MirrorCredentials, transport},
    AppState,
};
use anyhow::{ensure, Context, Result};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::process::{Child, Command};

/// A source or cache anchor admitted by the original host operation. A path
/// below it is descriptive; replacement of its named anchor never retargets it.
#[derive(Clone)]
struct Root {
    path: PathBuf,
    directory: Arc<File>,
}
impl Root {
    fn capture(path: PathBuf) -> Result<Self> {
        let directory = pro::install::directory(&path)?;
        Ok(Self {
            path,
            directory: Arc::new(directory),
        })
    }
    fn capture_cache(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute()
                && path.as_os_str().len() <= 4096
                && path.file_name() == Some(OsStr::new("pro")),
            "invalid original Pro cache anchor"
        );
        // A legitimate home/data alias is resolved once. The fixed Pro-cache
        // suffix and all later descendants remain strict no-follow paths.
        let data = path
            .parent()
            .context("Pro cache parent absent")?
            .canonicalize()?;
        Self::cache_beneath(Self::capture(data)?)
    }
    fn cache_beneath(data: Self) -> Result<Self> {
        use rustix::fs::{mkdirat, openat, Mode, OFlags};
        data.current()?;
        match mkdirat(&*data.directory, "pro", Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
        let directory = File::from(openat(
            &*data.directory,
            "pro",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let cache = Self {
            path: data.path.join("pro"),
            directory: Arc::new(directory),
        };
        // Creation and capture use the original data descriptor. Replacement
        // of either named directory refuses before this owner is published.
        data.current()?;
        cache.current()?;
        Ok(cache)
    }
    fn current(&self) -> Result<()> {
        let original = self.directory.metadata()?;
        let named = pro::install::directory(&self.path)?.metadata()?;
        ensure!(
            original.dev() == named.dev() && original.ino() == named.ino(),
            "Transfer root changed during the original operation"
        );
        Ok(())
    }
    fn contains(&self, path: &Path) -> bool {
        path.strip_prefix(&self.path).is_ok_and(|relative| {
            relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
        })
    }
}

fn captured_cache_path(description: &Path, captured: &Path, path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute() && path.as_os_str().len() <= 4096,
        "invalid transfer path"
    );
    match path.strip_prefix(description) {
        Ok(relative) => {
            ensure!(
                relative
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
                "invalid transfer cache path"
            );
            let path = captured.join(relative);
            ensure!(
                path.as_os_str().len() <= 4096,
                "captured transfer path exceeds bound"
            );
            Ok(path)
        }
        Err(_) => Ok(path.to_owned()),
    }
}

/// Same generation, source and cache namespace as the original operation.
/// Construction stays in the host; private policy cannot select another home,
/// project, configuration, program, token or process quota.
pub struct TransferHost {
    state: Arc<AppState>,
    workspace: String,
    generation: u64,
    source: std::sync::Mutex<Option<Root>>,
    git_roots: std::sync::Mutex<Vec<Root>>,
    cache: Root,
    cache_description: PathBuf,
    project_cache: PathBuf,
    _cache_guard: Arc<tokio::sync::OwnedMutexGuard<()>>,
}
impl TransferHost {
    pub(crate) async fn capture(
        state: Arc<AppState>,
        workspace: &str,
        source: Option<&Path>,
        cache_guard: Arc<tokio::sync::OwnedMutexGuard<()>>,
        generation: u64,
    ) -> Result<Arc<Self>> {
        let workspace = workspace.to_owned();
        let source = source.map(Path::to_path_buf);
        // The original cache owner remains in this worker even if the observer
        // is cancelled while a shared filesystem is resolving these anchors.
        tokio::task::spawn_blocking(move || {
            ensure!(pro::valid_id(&workspace), "invalid transfer workspace");
            ensure!(
                generation == state.pro().generation.load(Ordering::Acquire),
                "Account changed during transfer capture"
            );
            transport::cache_quiescent(&workspace)?;
            let source = source.map(Root::capture).transpose()?;
            let cache_description = state.pro().root.clone();
            let cache = Root::capture_cache(&cache_description)?;
            let project_cache = cache.path.join(&workspace);
            let owner = Arc::new(Self {
                state,
                workspace,
                generation,
                source: std::sync::Mutex::new(source),
                git_roots: std::sync::Mutex::new(Vec::new()),
                cache,
                cache_description,
                project_cache,
                _cache_guard: cache_guard,
            });
            owner.current()?;
            Ok(owner)
        })
        .await?
    }

    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    pub(crate) async fn capture_fixture(
        state: Arc<AppState>,
        root: PathBuf,
        workspace: String,
        cache_guard: Arc<tokio::sync::OwnedMutexGuard<()>>,
    ) -> Result<Arc<Self>> {
        tokio::task::spawn_blocking(move || {
            let source = Root::capture(root.clone())?;
            let generation = state.pro().generation.load(Ordering::Acquire);
            let host = Arc::new(Self {
                state,
                workspace,
                generation,
                source: std::sync::Mutex::new(Some(source.clone())),
                git_roots: std::sync::Mutex::new(Vec::new()),
                cache: source,
                cache_description: root.clone(),
                project_cache: root,
                _cache_guard: cache_guard,
            });
            host.roots_current()?;
            Ok(host)
        })
        .await?
    }
    pub(crate) fn same_cache(&self, cache: &Arc<tokio::sync::OwnedMutexGuard<()>>) -> Result<()> {
        ensure!(
            Arc::ptr_eq(&self._cache_guard, cache),
            "Shadow recovery cache owner changed"
        );
        Ok(())
    }
    /// Only the original fixed derived shadow can be prepared for publication.
    pub async fn prepare_shadow_install(
        self: &Arc<Self>,
        shadow: &Path,
        interrupted: bool,
    ) -> Result<pro::shadow_cache::PreparedShadow> {
        self.current()?;
        let shadow = self.captured_path(shadow)?;
        ensure!(
            shadow == self.project_cache.join("working-tree.git"),
            "invalid original shadow path"
        );
        let owner = self.clone();
        tokio::task::spawn_blocking(move || {
            owner.roots_current()?;
            let parent =
                owner.directory(shadow.parent().context("original shadow parent absent")?)?;
            Ok(pro::shadow_cache::PreparedShadow::new(
                owner,
                shadow,
                parent,
                interrupted,
            ))
        })
        .await?
    }
    /// Only the original public destination admission may bind this once. A
    /// legacy hydration can learn it from its admitted checkpoint; private
    /// repository policy cannot choose or replace it.
    pub(crate) async fn bind_source(self: &Arc<Self>, path: &Path) -> Result<()> {
        let host = self.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            host.roots_current()?;
            let root = Root::capture(path)?;
            let mut source = lock(&host.source);
            if let Some(original) = source.as_ref() {
                ensure!(original.path == root.path, "Transfer source changed");
                original.current()?;
            } else {
                *source = Some(root);
            }
            host.current()
        })
        .await?
    }
    pub(crate) fn belongs_to(&self, pro: &pro::ProState, workspace: &str) -> bool {
        self.workspace == workspace && std::ptr::eq(pro, self.state.pro())
    }
    pub fn cache(&self) -> &Path {
        &self.project_cache
    }
    /// Descriptive cache paths from the original caller are translated to the
    /// already pinned anchor. This performs no filesystem lookup or new root
    /// admission; later home-alias changes cannot select another cache.
    pub fn captured_path(&self, path: &Path) -> Result<PathBuf> {
        captured_cache_path(&self.cache_description, &self.cache.path, path)
    }
    pub fn current(&self) -> Result<()> {
        ensure!(
            self.generation == self.state.pro().generation.load(Ordering::Acquire),
            "Account changed during the original transfer"
        );
        transport::cache_quiescent(&self.workspace)?;
        Ok(())
    }
    // Filesystem admission is called only by a retained blocking worker.
    pub fn cleanup_uncertain(&self) {
        transport::uncertain(Some(&self.workspace));
    }
    pub fn filesystem_current(&self) -> Result<()> {
        if let Some(source) = lock(&self.source).as_ref() {
            source.current()?;
        }
        self.cache.current()?;
        for root in lock(&self.git_roots).iter() {
            root.current()?;
        }
        Ok(())
    }
    fn roots_current(&self) -> Result<()> {
        self.current()?;
        self.filesystem_current()
    }
    pub async fn prepare_directory(self: &Arc<Self>, path: &Path) -> Result<()> {
        let host = self.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            host.staging(&path)?;
            std::fs::create_dir_all(&path)?;
            host.current()
        })
        .await?
    }
    fn path(&self, path: &Path) -> Result<()> {
        self.roots_current()?;
        ensure!(
            path.as_os_str().len() <= 4096
                && (lock(&self.source)
                    .as_ref()
                    .is_some_and(|root| root.contains(path))
                    || lock(&self.git_roots).iter().any(|root| root.contains(path))
                    || (self.cache.contains(path) && path.starts_with(&self.project_cache))),
            "Git directory is outside the original transfer roots"
        );
        // Check every existing component beneath the original anchor. No parent
        // symlink, alternate cwd or replacement subtree becomes a new admission.
        pro::install::directory(path)?;
        Ok(())
    }
    fn git_environment(&self, key: &OsStr, path: &Path) -> Result<()> {
        if key == OsStr::new("GIT_INDEX_FILE") {
            self.path(path.parent().context("invalid transfer index path")?)?;
            ensure!(path.file_name().is_some(), "invalid transfer index path");
            return Ok(());
        }
        if key == OsStr::new("GIT_DIR") {
            match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    ensure!(
                        path.file_name() == Some(OsStr::new(".git")),
                        "invalid new transfer Git directory"
                    );
                    return self.path(path.parent().context("invalid transfer Git directory")?);
                }
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
        }
        self.path(path)
    }
    /// Original Git layout queries, at the original repository-admission
    /// cutpoint. Linked-worktree directories are pinned from these fixed
    /// queries, never accepted as a path supplied by a client or private policy.
    pub async fn git_layout(self: &Arc<Self>, source: &Path) -> Result<Option<GitLayout>> {
        self.current()?;
        let mut command = self.git(source).await?;
        command.args(["rev-parse", "--absolute-git-dir"]);
        let output = command
            .run(vec![], Duration::from_secs(15), transport::JSON_CAP)
            .await?;
        if !output.success() {
            return Ok(None);
        }
        let actual = PathBuf::from(String::from_utf8_lossy(output.bytes()).trim());
        let mut command = self.git(source).await?;
        command.args(["rev-parse", "--git-common-dir"]);
        let common = command
            .run(vec![], Duration::from_secs(15), transport::JSON_CAP)
            .await?;
        ensure!(common.success(), "Git common directory unavailable");
        let common = source.join(String::from_utf8_lossy(common.bytes()).trim());
        let host = self.clone();
        tokio::task::spawn_blocking(move || {
            host.roots_current()?;
            let actual = std::fs::canonicalize(actual)?;
            let common = std::fs::canonicalize(common)?;
            let roots = vec![
                Root::capture(actual.clone())?,
                Root::capture(common.clone())?,
            ];
            let beneath_original = |path: &Path| {
                lock(&host.source)
                    .as_ref()
                    .is_some_and(|root| root.contains(path))
                    || (host.cache.contains(path) && path.starts_with(&host.project_cache))
            };
            // Ordinary repositories/checkouts already lie beneath a captured
            // root. Only linked-worktree metadata outside it needs another
            // original descriptor; later queries cannot replace that binding.
            if !beneath_original(&actual) || !beneath_original(&common) {
                let mut captured = lock(&host.git_roots);
                if captured.is_empty() {
                    *captured = roots;
                } else {
                    ensure!(
                        captured[0].path == actual && captured[1].path == common,
                        "Git layout changed during transfer"
                    );
                }
            }
            host.current()?;
            Ok(Some(GitLayout { actual, common }))
        })
        .await?
    }

    fn staging(&self, path: &Path) -> Result<()> {
        self.roots_current()?;
        ensure!(
            path.starts_with(&self.project_cache) && self.cache.contains(path),
            "Staging path is outside the original project cache"
        );
        for ancestor in path.ancestors() {
            if ancestor == self.cache.path {
                break;
            }
            match std::fs::symlink_metadata(ancestor) {
                Ok(_) => {
                    pro::install::directory(ancestor)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    pub fn directory(&self, path: &Path) -> Result<File> {
        self.path(path)?;
        pro::install::directory(path)
    }
    /// Fixed read-only descriptor traversal used by portable metadata policy.
    /// Callers run this in the same retained blocking filesystem operation.
    pub fn read_file(&self, root: &Path, relative: &Path) -> Result<File> {
        let directory = self.directory(root)?;
        crate::download::open_beneath(
            &directory,
            relative,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
        )
        .map_err(Into::into)
    }
    pub fn open_regular(&self, path: &Path) -> Result<(File, std::fs::Metadata)> {
        self.path(path.parent().context("invalid transfer file path")?)?;
        crate::fs::open_regular(path).map_err(Into::into)
    }
    pub async fn child_permit(&self) -> Result<tokio::sync::SemaphorePermit<'static>> {
        self.current()?;
        transport::child_permit().await
    }
    pub fn snapshot(
        &self,
        source: &Path,
        destination: &Path,
        include: &dyn Fn(&Path) -> bool,
        budget: u64,
    ) -> Result<()> {
        self.path(source)?;
        self.staging(destination)?;
        pro::install::snapshot(source, destination, include, budget)
    }
    pub fn changes(
        &self,
        root: &Path,
        before: &Path,
        after: &Path,
    ) -> Result<Vec<pro::install::Write>> {
        self.path(root)?;
        self.path(before)?;
        self.path(after)?;
        pro::install::changes(root, before, after)
    }
    async fn capture_directory(self: &Arc<Self>, path: &Path) -> Result<Root> {
        let host = self.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            host.path(&path)?;
            Root::capture(path)
        })
        .await?
    }
    pub async fn git(self: &Arc<Self>, path: &Path) -> Result<Git> {
        // Capture before the original asynchronous program discovery. The
        // retained preparation revalidates this same descriptor after queuing.
        let directory = self.capture_directory(path).await?;
        Ok(Git {
            host: self.clone(),
            directory,
            command: transport::git(path, None).await?,
            invalid_environment: false,
            spawn_prepared: false,
        })
    }
    pub(in crate::pro) fn grant(
        self: &Arc<Self>,
        credentials: MirrorCredentials,
    ) -> Result<MirrorGrant> {
        ensure!(
            credentials.workspace_id == self.workspace,
            "mirror grant workspace changed"
        );
        Ok(MirrorGrant {
            host: self.clone(),
            credentials,
        })
    }
    /// Read at the original post-shadow-baseline cutpoint, from this same
    /// project's preferences; no cache or configuration is selected anew.
    pub fn published_tree(&self) -> Option<String> {
        lock(&self.state.pro().preferences)
            .get(&self.workspace)
            .and_then(|preference| preference.published_tree.clone())
    }
    pub fn observe_plain_folder(&self, repository: bool) -> bool {
        let mut plain = lock(&self.state.pro().plain_folders);
        if repository {
            plain.remove(&self.workspace);
            false
        } else {
            plain.insert(self.workspace.clone())
        }
    }
}

/// Descriptive paths from the original fixed Git metadata reads. The actual
/// descriptor admissions remain private in TransferHost.
pub struct GitLayout {
    pub actual: PathBuf,
    pub common: PathBuf,
}

/// The original mirror grant stays opaque in the public host. Private policy
/// can perform only a fixed mirror fetch/publication with its captured scope.
pub struct MirrorGrant {
    host: Arc<TransferHost>,
    credentials: MirrorCredentials,
}
#[derive(Clone, Copy)]
pub enum MirrorRepository {
    WorkingTree,
    Repository,
}
impl MirrorGrant {
    pub fn storage_limit_bytes(&self) -> u64 {
        self.credentials.storage_limit_bytes
    }
    pub fn max_file_bytes(&self) -> u64 {
        self.credentials.max_file_bytes
    }
    pub fn read_only(&self) -> bool {
        self.credentials.read_only
    }
    pub async fn fetch(
        &self,
        path: &Path,
        repository: MirrorRepository,
        prune: bool,
        refs: &[&str],
    ) -> Result<()> {
        self.network(path, repository, false, false, prune, refs)
            .await
    }
    pub async fn push(
        &self,
        path: &Path,
        repository: MirrorRepository,
        mirror: bool,
        refs: &[&str],
    ) -> Result<()> {
        ensure!(
            !self.credentials.read_only,
            "mirror credential is read-only"
        );
        self.network(path, repository, true, mirror, false, refs)
            .await
    }
    async fn network(
        &self,
        path: &Path,
        repository: MirrorRepository,
        push: bool,
        mirror: bool,
        prune: bool,
        refs: &[&str],
    ) -> Result<()> {
        let directory = self.host.capture_directory(path).await?;
        let check = self.check(directory);
        ensure!(
            !self.credentials.username.contains(['\n', '\r', '\0'])
                && !self.credentials.password.contains(['\n', '\r', '\0']),
            "invalid mirror credentials"
        );
        let url = transport::endpoint(match repository {
            MirrorRepository::WorkingTree => &self.credentials.working_tree_url,
            MirrorRepository::Repository => &self.credentials.repository_url,
        })?;
        let mut args = if push {
            vec![
                "push",
                if mirror { "--mirror" } else { "--atomic" },
                url.as_str(),
            ]
        } else if prune {
            vec!["fetch", "--prune", "--no-tags", url.as_str()]
        } else {
            vec!["fetch", "--no-tags", url.as_str()]
        };
        args.extend_from_slice(refs);
        let command = transport::git(
            path,
            Some((&self.credentials.username, &self.credentials.password)),
        )
        .await?;
        transport::cache_scope(
            &self.host.workspace,
            self.host._cache_guard.clone(),
            transport::git_output_checked(command, &args, vec![], check),
        )
        .await?;
        Ok(())
    }
    fn check(&self, directory: Root) -> transport::GitCheck {
        let host = self.host.clone();
        Arc::new(move || {
            directory.current()?;
            host.path(&directory.path)
        })
    }
}

/// A fixed Git program. Existing private argv policy is retained exactly; no
/// duplicate verb parser is introduced. Only its original finite environment
/// additions can be changed, and cwd/program/credentials cannot be replaced.
pub struct Git {
    host: Arc<TransferHost>,
    directory: Root,
    command: Command,
    invalid_environment: bool,
    spawn_prepared: bool,
}
impl Git {
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.spawn_prepared = false;
        self.command.args(args);
        self
    }
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.spawn_prepared = false;
        let key = key.as_ref();
        if matches!(
            key.to_str(),
            Some(
                "GIT_DIR"
                    | "GIT_WORK_TREE"
                    | "GIT_INDEX_FILE"
                    | "GIT_AUTHOR_NAME"
                    | "GIT_AUTHOR_EMAIL"
                    | "GIT_COMMITTER_NAME"
                    | "GIT_COMMITTER_EMAIL"
            )
        ) {
            self.command.env(key, value);
        } else {
            self.invalid_environment = true;
        }
        self
    }
    pub fn stdin(&mut self, value: Stdio) -> &mut Self {
        self.spawn_prepared = false;
        self.command.stdin(value);
        self
    }
    pub fn stdout(&mut self, value: Stdio) -> &mut Self {
        self.spawn_prepared = false;
        self.command.stdout(value);
        self
    }
    pub fn stderr(&mut self, value: Stdio) -> &mut Self {
        self.spawn_prepared = false;
        self.command.stderr(value);
        self
    }
    pub fn kill_on_drop(&mut self, value: bool) -> &mut Self {
        self.spawn_prepared = false;
        self.command.kill_on_drop(value);
        self
    }
    fn check(&self) -> Result<()> {
        ensure!(
            !self.invalid_environment,
            "invalid transfer Git environment"
        );
        self.host.current()
    }
    fn admission(&self) -> transport::GitCheck {
        let host = self.host.clone();
        let directory = self.directory.clone();
        let paths: Vec<(OsString, OsString)> = self
            .command
            .as_std()
            .get_envs()
            .filter_map(|(key, value)| {
                matches!(
                    key.to_str(),
                    Some("GIT_DIR" | "GIT_WORK_TREE" | "GIT_INDEX_FILE")
                )
                .then(|| value.map(|value| (key.to_os_string(), value.to_os_string())))
                .flatten()
            })
            .collect();
        Arc::new(move || {
            directory.current()?;
            host.path(&directory.path)?;
            for (key, path) in &paths {
                host.git_environment(key, Path::new(path))?;
            }
            Ok(())
        })
    }

    pub async fn run(self, input: Vec<u8>, timeout: Duration, cap: usize) -> Result<GitOutput> {
        self.check()?;
        let check = self.admission();
        let output = transport::cache_scope(
            &self.host.workspace,
            self.host._cache_guard.clone(),
            transport::run_checked(self.command, input, timeout, cap, check),
        )
        .await?;
        Ok(output.into())
    }
    pub async fn output(self, args: &[&str], input: Vec<u8>) -> Result<Vec<u8>> {
        self.check()?;
        let check = self.admission();
        transport::cache_scope(
            &self.host.workspace,
            self.host._cache_guard.clone(),
            transport::git_output_checked(self.command, args, input, check),
        )
        .await
    }
    pub async fn file(self, destination: PathBuf, timeout: Duration, cap: u64) -> Result<u64> {
        self.check()?;
        let parent = destination.parent().context("invalid staged blob path")?;
        ensure!(
            parent.starts_with(&self.host.project_cache) && self.host.cache.contains(parent),
            "Staged Git output is outside the original project cache"
        );
        let parent = self.host.capture_directory(parent).await?;
        let leaf = destination
            .file_name()
            .context("invalid staged blob path")?
            .to_os_string();
        let directory = parent.directory.clone();
        let output_length = Arc::new(AtomicU64::new(0));
        let written = output_length.clone();
        let output = Box::new(move || {
            use rustix::fs::{AtFlags, Mode, OFlags};
            parent.current()?;
            let file = rustix::fs::openat(
                &*directory,
                &leaf,
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?;
            let cleanup = Box::new(move || {
                match rustix::fs::unlinkat(&*directory, &leaf, AtFlags::empty()) {
                    Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
                    Err(error) => Err(error.into()),
                }
            });
            Ok(transport::PreparedFile {
                file: file.into(),
                destination,
                cap,
                length: written,
                cleanup,
            })
        });
        let check = self.admission();
        transport::cache_scope(
            &self.host.workspace,
            self.host._cache_guard.clone(),
            transport::run_file_checked(self.command, timeout, check, output, output_length),
        )
        .await
    }

    /// Prepare while the original ref-transaction still owns its reservations;
    /// cancellation before spawn creates no child. No command mutation or await
    /// is allowed between the later spawn and original transaction installation.
    pub async fn prepare_spawn(
        &mut self,
        permit: Arc<tokio::sync::SemaphorePermit<'static>>,
    ) -> Result<()> {
        self.check()?;
        let check = self.admission();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            check()
        })
        .await??;
        self.check()?;
        self.spawn_prepared = true;
        Ok(())
    }
    pub fn spawn(&mut self, _permit: &tokio::sync::SemaphorePermit<'static>) -> Result<Child> {
        self.check()?;
        ensure!(
            self.spawn_prepared,
            "Git transaction admission was not prepared"
        );
        self.spawn_prepared = false;
        Ok(self.command.spawn()?)
    }
}
pub struct GitOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    damaged: bool,
}
impl From<transport::Output> for GitOutput {
    fn from(output: transport::Output) -> Self {
        let damaged = output.object_damage();
        Self {
            success: output.success,
            stdout: output.stdout,
            damaged,
        }
    }
}
impl GitOutput {
    pub fn success(&self) -> bool {
        self.success
    }
    pub fn bytes(&self) -> &[u8] {
        &self.stdout
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.stdout
    }
    pub fn object_damage(&self) -> bool {
        self.damaged
    }
}

#[cfg(all(unix, feature = "daemon-extension-fixture"))]
pub mod fixture;

#[cfg(test)]
mod cache_anchor_tests {
    use super::*;
    #[test]
    fn original_cache_alias_is_resolved_once_and_suffixes_remain_no_follow() {
        use std::os::unix::fs::{symlink, DirBuilderExt};
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-anchor-{}",
            chimaera_core::generate_token()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let root = root.canonicalize().unwrap();
        let original = root.join("original");
        let successor = root.join("successor");
        for data in [&original, &successor] {
            std::fs::create_dir_all(data.join("pro/w-fixture/git")).unwrap();
        }
        std::fs::write(original.join("pro/w-fixture/git/proof"), b"original").unwrap();
        std::fs::write(successor.join("pro/w-fixture/git/proof"), b"successor").unwrap();
        let alias = root.join("home-alias");
        symlink(&original, &alias).unwrap();
        let description = alias.join("pro");
        let captured = Root::capture_cache(&description).unwrap();
        std::fs::remove_file(&alias).unwrap();
        symlink(&successor, &alias).unwrap();
        captured.current().unwrap();
        let path = captured_cache_path(
            &description,
            &captured.path,
            &description.join("w-fixture/git"),
        )
        .unwrap();
        assert_eq!(path, original.join("pro/w-fixture/git"));
        assert_eq!(std::fs::read(path.join("proof")).unwrap(), b"original");
        assert!(Root::capture(description.clone()).is_err());
        symlink(
            successor.join("pro/w-fixture/git"),
            original.join("pro/linked"),
        )
        .unwrap();
        let linked =
            captured_cache_path(&description, &captured.path, &description.join("linked")).unwrap();
        assert!(pro::install::directory(&linked).is_err());
        assert!(
            captured_cache_path(&description, &captured.path, &description.join("../escape"))
                .is_err()
        );
        std::fs::rename(original.join("pro"), original.join("old-pro")).unwrap();
        std::fs::create_dir(original.join("pro")).unwrap();
        assert!(captured.current().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn first_transfer_creates_only_fixed_cache_under_existing_data() {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-first-{}",
            chimaera_core::generate_token()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let root = root.canonicalize().unwrap();
        let data = root.join("data");
        let source = root.join("project");
        assert!(Root::capture_cache(&data.join("pro")).is_err());
        assert!(!data.exists());
        std::fs::create_dir(&data).unwrap();
        std::fs::create_dir(&source).unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            data.clone(),
            root.join("config"),
        ));
        let cache = Arc::new(state.pro().cache("w-first").unwrap().lock_owned().await);
        assert!(!data.join("pro").exists());
        let owner = TransferHost::capture(state, "w-first", Some(&source), cache, 0)
            .await
            .unwrap();
        assert!(data.join("pro").is_dir());
        assert!(!owner.cache().exists());
        owner.prepare_directory(owner.cache()).await.unwrap();
        assert_eq!(owner.cache(), data.join("pro/w-first"));
        assert!(owner.cache().is_dir());
        owner.filesystem_current().unwrap();
        drop(owner);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn replaced_data_anchor_cannot_receive_first_cache_directory() {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-data-replaced-{}",
            chimaera_core::generate_token()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let root = root.canonicalize().unwrap();
        let data = root.join("data");
        std::fs::create_dir(&data).unwrap();
        let captured = Root::capture(data.clone()).unwrap();
        let original = root.join("original-data");
        std::fs::rename(&data, &original).unwrap();
        std::fs::create_dir(&data).unwrap();
        assert!(Root::cache_beneath(captured).is_err());
        assert!(!data.join("pro").exists());
        assert!(!original.join("pro").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn fixed_cache_leaf_cannot_be_an_alias() {
        use std::os::unix::fs::{symlink, DirBuilderExt};
        let root = std::env::temp_dir().join(format!(
            "chimaera-transfer-leaf-{}",
            chimaera_core::generate_token()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::create_dir(root.join("destination")).unwrap();
        symlink(root.join("destination"), root.join("pro")).unwrap();
        assert!(Root::capture_cache(&root.join("pro")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
