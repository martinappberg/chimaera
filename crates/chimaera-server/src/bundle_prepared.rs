//! A return prepares immutable file intents before touching the destination.
//! Shared registries are merged idempotently after the files land, while the
//! return remains fenced; they are never replaced with an old whole-store copy.
use super::*;
use crate::pro::install;
use std::os::unix::fs::PermissionsExt;

const MAX_PREPARATION: u64 = 512 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIntent {
    target: PathBuf,
    root: PathBuf,
    relative: PathBuf,
    before: Option<String>,
    after: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: u32,
    digest: String,
    epoch: u64,
    fork: bool,
    origin: Origin,
    workspace: Workspace,
    entry: Value,
    settings: Value,
    view: Option<Value>,
    links: BTreeMap<String, String>,
    send_state: Option<Vec<u8>>,
    files: Vec<FileIntent>,
    #[serde(default)]
    public: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sealed {
    sha256: String,
    payload: String,
}

pub(crate) struct PreparedImport {
    pub(crate) writes: Vec<install::Write>,
    state: Arc<AppState>,
    metadata: Metadata,
    generation: u64,
    #[cfg(test)]
    pause: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
    // No mode switch, restart or competing import may invalidate the prepared
    // transcript while its file transaction and metadata merge are in progress.
    _guard: crate::chat::ChatSwitchGuard,
}

fn admissible(state: &Arc<AppState>, entry: &LedgerEntry, epoch: u64) -> Result<()> {
    if !crate::pro::may_import(state, &entry.workspace_id, epoch)
        || crate::pro::owned_epoch(state, &entry.workspace_id).is_some_and(|owned| owned != epoch)
    {
        bail!("workspace ownership changed during bundle preparation");
    }
    if state.chat.get(&entry.id).is_some_and(|s| s.alive)
        || state.sessions.get(&entry.id).is_some_and(|s| s.alive)
    {
        bail!("session already running; stop it before importing");
    }
    if let Some(native) = entry
        .agent
        .as_ref()
        .and_then(|agent| agent.resume.as_deref())
    {
        if state.chat.list().iter().any(|s| {
            s.alive
                && (s.native_session_id.as_deref() == Some(native)
                    || crate::lock(&state.chat_recipes)
                        .get(&s.id)
                        .is_some_and(|recipe| recipe.resume.as_deref() == Some(native)))
        }) {
            bail!("native conversation is active in another session");
        }
        let live: std::collections::HashSet<_> = state
            .sessions
            .list()
            .into_iter()
            .filter(|s| s.alive)
            .map(|s| s.id)
            .collect();
        if crate::lock(&state.agents).iter().any(|(id, record)| {
            live.contains(id)
                && record
                    .resume_id()
                    .or_else(|| record.resumed_from.clone())
                    .as_deref()
                    == Some(native)
        }) {
            bail!("native conversation is active in another session");
        }
    }
    let deferred = crate::lock(&state.deferred_sessions);
    if !deferred.contains_key(&entry.id) && deferred.len() >= 512 {
        bail!("suspended session limit reached");
    }
    Ok(())
}

fn validate_links(
    state: &AppState,
    entry: &LedgerEntry,
    links: &BTreeMap<String, String>,
) -> Result<()> {
    for (terminal, agent) in links {
        if terminal != &entry.id && agent != &entry.id {
            bail!("bundle link does not belong to its session");
        }
        for endpoint in [terminal, agent] {
            let known = crate::lock(&state.session_workspaces)
                .get(endpoint)
                .cloned()
                .or_else(|| {
                    crate::lock(&state.deferred_sessions)
                        .get(endpoint)
                        .map(|e| e.workspace_id.clone())
                });
            if known.is_some_and(|known| known != entry.workspace_id) {
                bail!("bundle link crosses workspace boundaries");
            }
        }
    }
    Ok(())
}

/// Anchor the file to its nearest existing canonical directory. Missing
/// subdirectories will be created by the descriptor-relative transaction.
fn anchor(target: &Path) -> Result<(PathBuf, PathBuf)> {
    if !target.is_absolute()
        || target
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("invalid bundle destination");
    }
    let mut parent = target.parent().context("bundle destination parent")?;
    loop {
        match std::fs::symlink_metadata(parent) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    bail!("bundle destination traverses a symlink");
                }
                // Configured roots may contain a platform alias (/var on
                // macOS). Pin its resolved directory and recheck at finalization.
                return Ok((
                    std::fs::canonicalize(parent)?,
                    target.strip_prefix(parent)?.to_owned(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                parent = parent
                    .parent()
                    .context("bundle destination has no existing parent")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn capture_before(root: &Path, relative: &Path, path: &Path) -> Result<bool> {
    use rustix::fs::OFlags;
    let filesystem = File::open("/")?;
    let root = crate::download::open_beneath(
        &filesystem,
        root.strip_prefix("/")?,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
    )?;
    let input = match crate::download::open_beneath(
        &root,
        relative,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
    ) {
        Ok(opened) => opened,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let metadata = input.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_NATIVE {
        bail!("existing bundle file exceeds preservation limit");
    }
    let mut output = private_create(path)?;
    let count = std::io::copy(&mut input.take(MAX_NATIVE + 1), &mut output)?;
    if count != metadata.len() || count > MAX_NATIVE {
        bail!("bundle destination changed during capture");
    }
    output.set_permissions(std::fs::Permissions::from_mode(
        metadata.permissions().mode() & 0o777,
    ))?;
    output.sync_all()?;
    Ok(true)
}

fn stage_file(
    stage: &Path,
    files: &mut Vec<FileIntent>,
    target: &Path,
    input: &mut dyn Read,
    cap: u64,
) -> Result<()> {
    let (root, relative) = anchor(target)?;
    let index = files.len();
    let before = format!("{index}.before");
    let before = capture_before(&root, &relative, &stage.join(&before))?.then_some(before);
    let after = format!("{index}.after");
    let mut output = private_create(&stage.join(&after))?;
    if std::io::copy(&mut input.take(cap + 1), &mut output)? > cap {
        bail!("bundle member exceeds limit");
    }
    output.sync_all()?;
    files.push(FileIntent {
        target: target.to_owned(),
        root,
        relative,
        before,
        after,
    });
    Ok(())
}

fn member_json(opened: &mut Opened, name: &str, cap: u64) -> Result<Option<Value>> {
    if !opened.manifest.members.contains_key(name) {
        return Ok(None);
    }
    let member = opened.zip.by_name(name)?;
    if member.size() > cap {
        bail!("bundle metadata exceeds limit");
    }
    Ok(Some(serde_json::from_reader(member.take(cap + 1))?))
}

fn prepare_files(
    state: &AppState,
    mut opened: Opened,
    options: ImportOptions,
    stage: &Path,
    public: bool,
) -> Result<Metadata> {
    std::fs::create_dir_all(stage)?;
    std::fs::set_permissions(stage, std::fs::Permissions::from_mode(0o700))?;
    // Retry uses the original evidence. Recapturing 'before' after a partial
    // installation could legitimize an intervening edit as our own original.
    let record = stage.join("metadata.json");
    match read_capped(&record, MAX_PREPARATION) {
        Ok(bytes) => {
            let sealed: Sealed = serde_json::from_slice(&bytes)?;
            if hex(Sha256::digest(sealed.payload.as_bytes())) != sealed.sha256 {
                bail!("prepared bundle metadata checksum mismatch");
            }
            let metadata: Metadata = serde_json::from_str(&sealed.payload)?;
            let entry =
                LedgerEntry::from_json(&metadata.entry).context("invalid prepared session")?;
            if metadata.version != 1
                || metadata.digest != opened.digest
                || metadata.epoch != options.epoch
                || metadata.fork != options.fork
                || metadata.origin != options.origin
                || entry.id != opened.entry.id
                || metadata.workspace.id != opened.manifest.workspace.id
                || entry.workspace_id != metadata.workspace.id
                || !entry.cwd.starts_with(&metadata.workspace.root)
                || entry
                    .cwd
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                || options
                    .destination_root
                    .as_ref()
                    .is_some_and(|r| r != &metadata.workspace.root)
                || metadata.public != public
                || metadata.files.len() > 3
            {
                bail!("prepared bundle binding mismatch");
            }
            return Ok(metadata);
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error),
    }
    // There is no complete preparation yet. These are our own incomplete
    // bounded copies, not destination data; an existing transaction requires
    // metadata.json at the engine boundary and can never enter this branch.
    for index in 0..3 {
        for suffix in ["before", "after"] {
            match std::fs::remove_file(stage.join(format!("{index}.{suffix}"))) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    if let Some(destination) = options.destination_root.as_ref() {
        if std::fs::canonicalize(destination)? != *destination || !destination.is_dir() {
            bail!("destination root must be a canonical directory");
        }
        let relative = opened
            .entry
            .cwd
            .strip_prefix(&opened.manifest.workspace.root)?;
        // Native transcript directories encode the cwd text. Drop lexical
        // trailing separators even when this subdirectory has not landed yet.
        let cwd: PathBuf = destination.join(relative).components().collect();
        anchor(&cwd.join(".bundle-path-check"))?;
        if let Some(agent) = &mut opened.entry.agent {
            if agent.native_cwd.is_none() && cwd != opened.entry.cwd {
                agent.native_cwd = Some(opened.entry.cwd.clone());
            }
        }
        opened.entry.cwd = cwd;
        opened.manifest.workspace.root = destination.clone();
    }
    let mut files = Vec::new();
    let settings =
        member_json(&mut opened, "index.json", MAX_METADATA)?.unwrap_or_else(|| json!({}));
    let view = member_json(&mut opened, "view.json", 64 * 1024)?;
    let send_state = if opened.manifest.members.contains_key("send-state.json") {
        let mut bytes = Vec::new();
        opened
            .zip
            .by_name("send-state.json")?
            .read_to_end(&mut bytes)?;
        Some(chimaera_agent::journal::merge_send_state(
            state.chat.journal_dir(),
            &opened.entry.id,
            &bytes,
        )?)
    } else if opened.entry.agent.is_some() {
        // A legacy sender lacks this member. Preserve the destination's own
        // durable evidence and legacy echoes before its journal is replaced.
        Some(chimaera_agent::journal::export_send_state(
            state.chat.journal_dir(),
            &opened.entry.id,
        )?)
    } else {
        None
    };
    if let Some(native) = native_destination(state, &opened.entry)? {
        stage_file(
            stage,
            &mut files,
            &native,
            &mut opened.zip.by_name("native.jsonl")?,
            MAX_NATIVE,
        )?;
        opened.entry.agent.as_mut().unwrap().transcript = Some(native);
    }
    if opened.manifest.members.contains_key("journal.jsonl") {
        let target = state
            .chat
            .journal_dir()
            .join(format!("{}.jsonl", opened.entry.id));
        stage_file(
            stage,
            &mut files,
            &target,
            &mut opened.zip.by_name("journal.jsonl")?,
            MAX_JOURNAL,
        )?;
    }
    if public {
        let project = &opened.manifest.workspace;
        if crate::workspaces::identity::read(&project.root).is_some_and(|m| m.id != project.id) {
            bail!("destination carries a different workspace identity");
        }
        let marker = crate::workspaces::identity::marker_path(&project.root);
        let mut bytes = serde_json::to_vec(&crate::workspaces::identity::Marker {
            id: project.id.clone(),
            written_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        })?;
        bytes.push(b'\n');
        stage_file(stage, &mut files, &marker, &mut bytes.as_slice(), 4096)?;
    }
    opened.entry.handoff = Some(HandoffResume {
        fork: options.fork,
        origin: options.origin,
        epoch: options.epoch,
    });
    let metadata = Metadata {
        version: 1,
        digest: opened.digest,
        epoch: options.epoch,
        fork: options.fork,
        origin: options.origin,
        workspace: opened.manifest.workspace,
        entry: opened.entry.to_json(),
        settings,
        view,
        links: opened.manifest.links,
        send_state,
        files,
        public,
    };
    let payload = serde_json::to_string(&metadata)?;
    let bytes = serde_json::to_vec(&Sealed {
        sha256: hex(Sha256::digest(payload.as_bytes())),
        payload,
    })?;
    if bytes.len() as u64 > MAX_PREPARATION {
        bail!("bundle preparation exceeds limit");
    }
    crate::persist::atomic_write_json_durable(&record, bytes)?;
    Ok(metadata)
}

pub(crate) async fn prepare_import(
    state: Arc<AppState>,
    path: &Path,
    options: ImportOptions,
    staging: &Path,
) -> Result<PreparedImport> {
    let generation = crate::pro::mutation::generation(&state);
    if !options.defer_start {
        bail!("prepared imports must remain deferred");
    }
    let _permit = OPERATIONS.try_acquire().context("bundle transfer busy")?;
    let path = path.to_owned();
    let opened = tokio::task::spawn_blocking(move || open_archive(&path)).await??;
    prepare_opened(state, opened, options, staging, generation, None, false).await
}

async fn prepare_opened(
    state: Arc<AppState>,
    opened: Opened,
    options: ImportOptions,
    staging: &Path,
    generation: u64,
    guard: Option<crate::chat::ChatSwitchGuard>,
    public: bool,
) -> Result<PreparedImport> {
    let guard = match guard {
        Some(guard) => guard,
        None => crate::chat::ChatSwitchGuard::acquire(&state, &opened.entry.id, "transfer")
            .context("session lifecycle operation already in progress")?,
    };
    admissible(&state, &opened.entry, options.epoch)?;
    validate_links(&state, &opened.entry, &opened.manifest.links)?;
    let setup = state.clone();
    let stage = staging.to_owned();
    let metadata =
        tokio::task::spawn_blocking(move || prepare_files(&setup, opened, options, &stage, public))
            .await??;
    if crate::lock(&state.workspaces)
        .list()
        .iter()
        .any(|workspace| {
            (workspace.id == metadata.workspace.id || workspace.root == metadata.workspace.root)
                && (workspace.id != metadata.workspace.id
                    || workspace.root != metadata.workspace.root)
        })
    {
        bail!("workspace identity conflicts with an existing workspace");
    }
    let mut writes = Vec::new();
    for file in &metadata.files {
        let valid_name = |name: &str| {
            name.len() <= 32
                && name
                    .bytes()
                    .all(|c| c.is_ascii_digit() || b".beforeaft".contains(&c))
        };
        if !valid_name(&file.after) || file.before.as_ref().is_some_and(|n| !valid_name(n)) {
            bail!("invalid prepared file reference");
        }
        writes.push(install::Write {
            root: file.root.clone(),
            relative: file.relative.clone(),
            before: file.before.as_ref().map(|p| staging.join(p)),
            after: Some(staging.join(&file.after)),
        });
    }
    Ok(PreparedImport {
        writes,
        state,
        metadata,
        generation,
        #[cfg(test)]
        pause: None,
        _guard: guard,
    })
}

impl PreparedImport {
    #[cfg(test)]
    pub(crate) fn hold_finalization(
        &mut self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (resume, held) = tokio::sync::oneshot::channel();
        self.pause = Some((entered, held));
        (waiting, resume)
    }
    /// Merge shared state while the return remains fenced. All actions are
    /// repeatable after a crash; no agent/terminal is started here.
    pub(crate) async fn finalize(self) -> Result<ImportedSession> {
        let admission = crate::pro::mutation::begin_import(
            &self.state,
            &self.metadata.workspace.id,
            self.metadata.epoch,
            self.generation,
        )
        .await?;
        // A canceled caller must not release account/epoch or session guards
        // while an owned blocking metadata write still has access to stores.
        tokio::spawn(async move {
            let (imported, _, _guard) = self.finalize_admitted(Arc::new(admission), true).await?;
            Ok::<_, anyhow::Error>(imported)
        })
        .await?
    }

    async fn finalize_admitted(
        self,
        admission: Arc<crate::pro::mutation::ImportGuard>,
        receipt: bool,
    ) -> Result<(ImportedSession, LedgerEntry, crate::chat::ChatSwitchGuard)> {
        let _session_guard = self._guard;
        #[cfg(test)]
        if let Some((entered, held)) = self.pause {
            let _ = entered.send(());
            let _ = held.await;
        }
        let mut entry =
            LedgerEntry::from_json(&self.metadata.entry).context("invalid prepared session")?;
        admission.check(&self.state)?;
        admissible(&self.state, &entry, self.metadata.epoch)?;
        validate_links(&self.state, &entry, &self.metadata.links)?;
        let state = self.state.clone();
        let metadata = self.metadata;
        let blocking_admission = admission.clone();
        let (entry, view, links, digest, epoch, workspace_root) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                for file in &metadata.files {
                    if std::fs::canonicalize(&file.target)? != file.root.join(&file.relative) {
                        bail!("bundle destination changed during installation");
                    }
                }
                if std::fs::canonicalize(&metadata.workspace.root)? != metadata.workspace.root {
                    bail!("workspace root changed during installation");
                }
                if entry.agent.is_none() {
                    entry.cwd = clamp_into(&metadata.workspace.root, &entry.cwd);
                }
                if std::fs::canonicalize(&entry.cwd)? != entry.cwd || !entry.cwd.is_dir() {
                    bail!("imported session cwd is unavailable");
                }
                let root = metadata.workspace.root.clone();
                blocking_admission.check(&state)?;
                crate::lock(&state.workspaces).import_exact_durable(metadata.workspace)?;
                if let Some(bytes) = metadata.send_state {
                    blocking_admission.check(&state)?;
                    chimaera_agent::journal::import_send_state(
                        state.chat.journal_dir(),
                        &entry.id,
                        &bytes,
                    )?;
                }
                if let Some(native) = entry.agent.as_ref().and_then(|a| a.resume.as_deref()) {
                    blocking_admission.check(&state)?;
                    state
                        .chat
                        .index()
                        .record_settings_checked(native, &entry.id, |target| {
                            target.model = metadata.settings["model"].as_str().map(str::to_owned);
                            target.effort = metadata.settings["effort"].as_str().map(str::to_owned);
                            target.mode = metadata.settings["mode"].as_str().map(str::to_owned);
                        })?;
                }
                Ok((
                    entry,
                    metadata.view,
                    metadata.links,
                    metadata.digest,
                    metadata.epoch,
                    root,
                ))
            })
            .await??;
        if let Some(view) = view {
            admission.check(&self.state)?;
            let key = format!("ws_{}", entry.workspace_id);
            let task = {
                let mut store = crate::lock(&self.state.view_state);
                let current = store
                    .get(&key)
                    .map(|value| value.as_ref().clone())
                    .unwrap_or(view);
                Some(store.put_durable(key, current))
            };
            if let Some(task) = task {
                task.await??;
            }
        }
        admission.check(&self.state)?;
        crate::ledger::defer(&self.state, entry.clone())?;
        crate::lock(&self.state.links).extend(links);
        flush_ledger(&self.state, true).await?;
        admission.check(&self.state)?;
        if receipt {
            let receipt = root(&self.state)
                .join("imports")
                .join(format!("{}.json", entry.id));
            let receipt_state = self.state.clone();
            tokio::task::spawn_blocking(move || {
                admission.check(&receipt_state)?;
                crate::persist::atomic_write_json_durable(
                    &receipt,
                    serde_json::to_vec(
                        &json!({"epoch":epoch,"sha256":digest,"root":workspace_root}),
                    )?,
                )
            })
            .await??;
        }
        self.state.changes.notify_waiters();
        Ok((
            ImportedSession {
                id: entry.id.clone(),
                workspace_id: entry.workspace_id.clone(),
                paused: true,
            },
            entry,
            _session_guard,
        ))
    }
}

#[cfg(test)]
type TestPause = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);
#[cfg(test)]
type TestPauses = std::collections::HashMap<(usize, String, bool), TestPause>;
#[cfg(test)]
static PUBLIC_PAUSES: std::sync::LazyLock<std::sync::Mutex<TestPauses>> =
    std::sync::LazyLock::new(Default::default);
#[cfg(test)]
pub(crate) fn hold_public(
    state: &Arc<AppState>,
    id: &str,
    resume: bool,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    crate::lock(&PUBLIC_PAUSES).insert(
        (Arc::as_ptr(state) as usize, id.into(), resume),
        (entered, held),
    );
    (waiting, release)
}
#[cfg(test)]
fn public_pause(state: &Arc<AppState>, id: &str, resume: bool) -> Option<TestPause> {
    crate::lock(&PUBLIC_PAUSES).remove(&(Arc::as_ptr(state) as usize, id.into(), resume))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicReceipt {
    version: u32,
    pending: pending::Pending,
}

pub(super) async fn import_public(
    state: Arc<AppState>,
    path: PathBuf,
    options: ImportOptions,
    generation: u64,
    permit: tokio::sync::SemaphorePermit<'static>,
) -> Result<ImportedSession> {
    tokio::spawn(async move {
        let _permit = permit;
        let mut opened = tokio::task::spawn_blocking(move || open_archive(&path)).await??;
        normalize_destination(&mut opened, &options).await?;
        let guard = crate::chat::ChatSwitchGuard::acquire(&state, &opened.entry.id, "transfer")
            .context("session lifecycle operation already in progress")?;
        // Cancellation before this owned task starts has no effects. After it starts,
        // neither filesystem workers nor metadata writes outlive their admission.
        let admission = Arc::new(
            crate::pro::mutation::begin_import(
                &state,
                &opened.entry.workspace_id,
                options.epoch,
                generation,
            )
            .await?,
        );
        let pending = pending::Pending {
            binding: crate::pro::bundle_install_binding(
                &state,
                &opened.entry.workspace_id,
                options.epoch,
                opened.digest.clone(),
            )?,
            id: opened.entry.id.clone(),
            destination: opened.manifest.workspace.root.clone(),
            native: opened.entry.agent.as_ref().and_then(|a| a.resume.clone()),
            fork: options.fork,
            origin: options.origin,
            defer_start: options.defer_start,
        };
        let recovery_state = state.clone();
        let recovery_id = pending.id.clone();
        let result: Result<ImportedSession> = async {
            let data = root(&state)
                .parent()
                .context("bundle data parent")?
                .to_owned();
            let bundles = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
                Ok(std::fs::canonicalize(data)?.join("bundles"))
            })
            .await??;
            let recovering = state.bundle_imports.matching(&pending)?;
            let receipt_path = bundles
                .clone()
                .join("imports")
                .join(format!("{}.json", pending.id));
            let read_path = receipt_path.clone();
            let expected = pending.clone();
            let committed = tokio::task::spawn_blocking(move || -> Result<(bool, bool)> {
                let bytes = match read_capped(&read_path, MAX_METADATA) {
                    Ok(bytes) => bytes,
                    Err(error)
                        if error
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                    {
                        return Ok((false, false))
                    }
                    Err(error) => return Err(error),
                };
                let receipt = serde_json::from_slice::<PublicReceipt>(&bytes);
                let full = receipt.is_ok_and(|r| r.version == 2 && r.pending == expected);
                let legacy: Value = serde_json::from_slice(&bytes)?;
                let old = legacy.get("version").is_none()
                    && legacy["epoch"].as_u64() == Some(expected.binding.epoch)
                    && legacy["sha256"].as_str() == expected.binding.receipt.as_deref()
                    && legacy["root"].as_str() == expected.destination.to_str();
                Ok((full, old))
            })
            .await??;
            let live = state.chat.get(&pending.id).is_some_and(|s| s.alive)
                || state.sessions.get(&pending.id).is_some_and(|s| s.alive);
            if committed.0 || (committed.1 && live && !recovering) {
                // The committed receipt proves metadata and files landed. A dead
                // actor is not permission to overwrite its newer canonical history.
                let mut entry = crate::lock(&state.deferred_sessions)
                    .get(&pending.id)
                    .cloned()
                    .unwrap_or(opened.entry);
                entry.handoff = Some(HandoffResume {
                    fork: options.fork,
                    origin: options.origin,
                    epoch: options.epoch,
                });
                if entry.workspace_id != pending.binding.workspace
                    || !entry.cwd.starts_with(&pending.destination)
                {
                    bail!("committed bundle session binding changed");
                }
                let native_state = state.clone();
                entry = tokio::task::spawn_blocking(move || -> Result<LedgerEntry> {
                    if let Some(native) = native_destination(&native_state, &entry)? {
                        entry.agent.as_mut().unwrap().transcript = Some(native);
                    }
                    Ok(entry)
                })
                .await??;
                let live = state.chat.get(&entry.id).is_some_and(|s| s.alive)
                    || state.sessions.get(&entry.id).is_some_and(|s| s.alive);
                if !live {
                    admission.check(&state)?;
                    let restore_state = state.clone();
                    let restore_workspace = opened.manifest.workspace.clone();
                    let check = admission.clone();
                    tokio::task::spawn_blocking(move || {
                        check.check(&restore_state)?;
                        crate::lock(&restore_state.workspaces)
                            .import_exact_durable(restore_workspace)
                    })
                    .await??;
                    crate::ledger::defer(&state, entry.clone())?;
                    flush_ledger(&state, true).await?;
                }
                if recovering {
                    let finish_state = state.clone();
                    let finish_pending = pending.clone();
                    let check = admission.clone();
                    tokio::task::spawn_blocking(move || {
                        check.check(&finish_state)?;
                        finish_state.bundle_imports.finish(&finish_pending)
                    })
                    .await??;
                }
                return finish_public(
                    state,
                    admission,
                    guard,
                    entry,
                    opened.manifest.workspace,
                    options,
                )
                .await;
            }
            admissible(&state, &opened.entry, options.epoch)?;
            let directory = bundles.join("imports/prepared").join(&pending.id);
            let stage = directory.join("stage");
            let transaction_path = directory.join("transaction");
            let probe = transaction_path.clone();
            let record = stage.join("metadata.json");
            tokio::task::spawn_blocking(move || -> Result<()> {
                if probe.try_exists()? && !record.try_exists()? {
                    bail!("unfinished bundle installation has lost its recovery metadata");
                }
                Ok(())
            })
            .await??;
            let workspace = opened.manifest.workspace.clone();
            let mut deferred = options.clone();
            deferred.defer_start = true;
            let mut prepared = prepare_opened(
                state.clone(),
                opened,
                deferred,
                &stage,
                generation,
                Some(guard),
                true,
            )
            .await?;
            let install_state = state.clone();
            let marker = pending.clone();
            let check = admission.clone();
            let writes = std::mem::take(&mut prepared.writes);
            let apply_stage = stage.clone();
            let prepared_entry = prepared.metadata.entry.clone();
            let transaction =
                tokio::task::spawn_blocking(move || -> Result<install::Transaction> {
                    check.check(&install_state)?;
                    install_state.bundle_imports.begin(marker.clone())?;
                    // Publication serialized against final free/local spawn registration.
                    // Recheck after enrollment so an earlier admitted launch cannot hide.
                    let entry = LedgerEntry::from_json(&prepared_entry)
                        .context("invalid prepared session")?;
                    admissible(&install_state, &entry, marker.binding.epoch)?;
                    install::stage_budget(&apply_stage)?;
                    let mut transaction =
                        match install::Transaction::open(&transaction_path, &marker.binding)? {
                            Some(transaction) => transaction,
                            None => install::Transaction::prepare(
                                &transaction_path,
                                marker.binding,
                                writes,
                                200_000_000,
                            )?,
                        };
                    transaction.apply(&|| check.check(&install_state))?;
                    Ok(transaction)
                })
                .await??;
            #[cfg(test)]
            {
                prepared.pause = public_pause(&state, &pending.id, false);
            }
            let (imported, entry, guard) =
                prepared.finalize_admitted(admission.clone(), false).await?;
            let final_state = state.clone();
            let marker = pending.clone();
            let check = admission.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let mut transaction = transaction;
                transaction.commit(&|| check.check(&final_state))?;
                check.check(&final_state)?;
                install::write_state(
                    &receipt_path,
                    &serde_json::to_vec(&PublicReceipt {
                        version: 2,
                        pending: marker.clone(),
                    })?,
                )?;
                final_state.bundle_imports.finish(&marker)?;
                // Cleanup never authorizes another write. Edited recovery artifacts
                // remain retained; the committed receipt still prevents replay.
                if transaction.cleanup().is_ok() {
                    std::fs::remove_dir_all(&directory).ok();
                }
                Ok(())
            })
            .await??;
            let _ = imported;
            finish_public(state, admission, guard, entry, workspace, options).await
        }
        .await;
        match result {
            Err(_)
                if recovery_state
                    .bundle_imports
                    .check_session(&recovery_id, None)
                    .is_err() =>
            {
                Err(pending::PendingError.into())
            }
            other => other,
        }
    })
    .await?
}

async fn finish_public(
    state: Arc<AppState>,
    admission: Arc<crate::pro::mutation::ImportGuard>,
    _guard: crate::chat::ChatSwitchGuard,
    entry: LedgerEntry,
    workspace: Workspace,
    options: ImportOptions,
) -> Result<ImportedSession> {
    let live = state.chat.get(&entry.id).is_some_and(|s| s.alive)
        || state.sessions.get(&entry.id).is_some_and(|s| s.alive);
    let paused = options.defer_start || (entry.agent.is_none() && options.origin == Origin::Moved);
    if !live && !paused {
        admission.check(&state)?;
        let captured = crate::pro::mutation::Dispatch::capture(&state, &workspace.id)?;
        let admission = Arc::try_unwrap(admission)
            .map_err(|_| anyhow::anyhow!("bundle admission still in use"))?;
        let commit = admission.into_resume();
        crate::pro::mutation::resume_import(commit, captured, async {
            #[cfg(test)]
            if let Some((entered, held)) = public_pause(&state, &entry.id, true) {
                let _ = entered.send(());
                let _ = held.await;
            }
            state.chat.remove(&entry.id);
            crate::ledger::respawn_transfer(
                &state,
                &entry,
                workspace,
                options.fork,
                Some(options.origin.as_str()),
            )
            .await?;
            crate::lock(&state.deferred_sessions).remove(&entry.id);
            state.session_proxy.clear_workspace(&entry.workspace_id);
            flush_ledger(&state, true).await
        })
        .await?;
    }
    state.changes.notify_waiters();
    Ok(ImportedSession {
        id: entry.id,
        workspace_id: entry.workspace_id,
        paused: !live && paused,
    })
}
