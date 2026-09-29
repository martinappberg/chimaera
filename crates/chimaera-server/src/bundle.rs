//! Versioned session archives. Only named members are imported; native CLI
//! credentials, settings, hooks, and process argv are never archive inputs.
use crate::{ledger::LedgerEntry, workspaces::Workspace, AppState};
use anyhow::{bail, Context, Result};
use axum::{
    body::Body,
    extract::{Path as RoutePath, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::AsyncWriteExt;

#[path = "bundle_empty.rs"]
mod empty;

pub(crate) const MAX_ARCHIVE: u64 = 100_000_000;
const MAX_NATIVE: u64 = 90_000_000;
const MAX_JOURNAL: u64 = 4 * 1024 * 1024;
const MAX_METADATA: u64 = 256 * 1024;
#[cfg(test)]
pub(crate) static TEST_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static OPERATIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportMode {
    Snapshot,
    Stop,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Origin {
    Moved,
    Home,
}
impl Origin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Moved => "moved",
            Self::Home => "home",
        }
    }
}
#[derive(Clone, Deserialize)]
pub(crate) struct ImportOptions {
    #[serde(default)]
    pub fork: bool,
    pub origin: Origin,
    pub epoch: u64,
    #[serde(default)]
    pub defer_start: bool,
    #[serde(default)]
    pub destination_root: Option<PathBuf>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HandoffResume {
    pub fork: bool,
    pub origin: Origin,
    pub epoch: u64,
}
#[derive(Serialize)]
pub(crate) struct ImportedSession {
    pub id: String,
    pub workspace_id: String,
    pub paused: bool,
}
#[derive(Serialize, Deserialize)]
struct Member {
    bytes: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    workspace: Workspace,
    session: Value,
    source_host: String,
    source_os: String,
    stopped: bool,
    #[serde(default)]
    links: BTreeMap<String, String>,
    members: BTreeMap<String, Member>,
}
fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
fn root(state: &AppState) -> PathBuf {
    state
        .chat
        .journal_dir()
        .parent()
        .expect("journal data parent")
        .join("bundles")
}
fn temp_path(state: &AppState) -> PathBuf {
    root(state)
        .join("tmp")
        .join(format!("{}.zip", chimaera_core::generate_token()))
}
fn private_create(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(path.parent().context("file parent")?)?;
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}
fn digest_file(path: &Path, cap: u64) -> Result<Member> {
    let (mut file, metadata) = crate::fs::open_regular(path)?;
    if metadata.len() > cap {
        bail!("bundle member exceeds size limit");
    }
    let mut digest = Sha256::new();
    let mut bytes = 0;
    let mut block = [0; 64 * 1024];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > cap {
            bail!("bundle member grew beyond size limit");
        }
        digest.update(&block[..n]);
    }
    Ok(Member {
        bytes,
        sha256: hex(digest.finalize()),
    })
}
fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>> {
    let (file, metadata) = crate::fs::open_regular(path)?;
    if metadata.len() > cap {
        bail!("bundle member exceeds size limit");
    }
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        bail!("bundle member grew beyond size limit");
    }
    Ok(bytes)
}
fn native_path(state: &AppState, entry: &LedgerEntry) -> Result<Option<PathBuf>> {
    let Some(agent) = &entry.agent else {
        return Ok(None);
    };
    let id = agent
        .resume
        .as_deref()
        .context("native conversation is not ready to export")?;
    if !crate::codex_notify::valid_thread_id(id) {
        bail!("invalid native conversation ID");
    }
    match agent.kind {
        crate::agents::AgentKind::Claude => {
            let expected = state
                .claude_projects_dir
                .join(crate::launcher::encode_cwd(&entry.cwd))
                .join(format!("{id}.jsonl"));
            if expected.is_file() {
                Ok(Some(expected))
            } else {
                bail!("Claude transcript is unavailable")
            }
        }
        crate::agents::AgentKind::Codex => Ok(Some(
            crate::codex_notify::find_rollout(
                state.codex_config_path.parent().context("Codex home")?,
                id,
                agent.native_cwd.as_deref().unwrap_or(&entry.cwd),
            )
            .context("Codex rollout is unavailable")?,
        )),
        _ => bail!("this agent does not support portable session archives"),
    }
}
/// A Pro transfer's ledger writes must survive a power cut (losing one could
/// let two machines run the same work); the generic bundle routes keep the
/// ordinary atomic write.
async fn flush_ledger(state: &Arc<AppState>, durable: bool) -> Result<()> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        let (entries, links) = crate::ledger::snapshot(&state);
        let mut ledger = crate::lock(&state.ledger);
        if durable {
            ledger.write_durable(&entries, &links)
        } else {
            ledger.write_checked(&entries, &links)
        }
    })
    .await?
}
/// Temporary archives a previous daemon life left behind. Only files older
/// than an hour go: a transfer started by this life may be writing one.
pub(crate) async fn sweep_temporary(state: &Arc<AppState>) {
    let directory = root(state).join("tmp");
    let _ = tokio::task::spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.filter_map(std::result::Result::ok).take(4096) {
            let stale = entry
                .metadata()
                .ok()
                .filter(|metadata| metadata.is_file())
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(3600));
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    })
    .await;
}

/// The caller owns and must unlink the returned temporary archive.
pub(crate) async fn export(state: Arc<AppState>, id: &str, mode: ExportMode) -> Result<PathBuf> {
    export_inner(state, id, mode, false, false)
        .await?
        .context("conversation is empty")
}

/// [`export`] for a Pro transfer: its ledger writes are durable.
pub(crate) async fn export_durable(
    state: Arc<AppState>,
    id: &str,
    mode: ExportMode,
) -> Result<PathBuf> {
    export_inner(state, id, mode, false, true)
        .await?
        .context("conversation is empty")
}

/// A proven unstarted chat has no portable conversation yet. Automatic mirrors
/// preserve it on the source (durably suspended for clean handoff); explicit
/// single-session export remains strict.
pub(crate) async fn export_for_mirror(
    state: Arc<AppState>,
    id: &str,
    mode: ExportMode,
) -> Result<Option<PathBuf>> {
    export_inner(state, id, mode, true, true).await
}

async fn export_inner(
    state: Arc<AppState>,
    id: &str,
    mode: ExportMode,
    skip_unstarted: bool,
    durable: bool,
) -> Result<Option<PathBuf>> {
    let _permit = OPERATIONS.try_acquire().context("bundle operation limit")?;
    if !valid_id(id) {
        bail!("invalid session ID");
    }
    let _guard = crate::chat::ChatSwitchGuard::acquire(&state, id, "transfer")
        .context("session lifecycle operation already in progress")?;
    let mut entry = crate::ledger::snapshot(&state)
        .0
        .into_iter()
        .find(|e| e.id == id)
        .context("unknown session")?;
    entry.suspended = false;
    entry.handoff = None;
    let workspace = crate::lock(&state.workspaces)
        .get(&entry.workspace_id)
        .context("unknown workspace")?;
    // Reject a missing native handle before stopping anything. A snapshot may
    // race an append; the checked copy below then asks the caller to retry.
    let mut paused = if skip_unstarted && mode == ExportMode::Stop && state.chat.get(id).is_some() {
        Some(
            tokio::time::timeout(Duration::from_secs(3), state.chat.pause_commands(id))
                .await
                .context("session input did not pause before transfer deadline")??,
        )
    } else {
        None
    };
    let checked = state.clone();
    let probe = entry.clone();
    let native = tokio::task::spawn_blocking(move || match native_path(&checked, &probe) {
        Ok(_) => Ok(true),
        Err(_) if skip_unstarted && empty::unstarted(&checked, &probe) => Ok(false),
        Err(error) => Err(error),
    })
    .await??;
    if mode == ExportMode::Stop && entry.agent.is_some() {
        let mut suspended = entry.clone();
        suspended.suspended = true;
        crate::ledger::defer(&state, suspended)?;
        flush_ledger(&state, durable).await?;
        if let Some(pause) = paused.take() {
            pause.commit_kill();
        } else if state.chat.get(id).is_some() {
            state.chat.kill(id);
        } else {
            // Agent CLIs use TERM to flush native state and clean detached work.
            // The ordinary terminal close remains the bounded fallback.
            if let Some(pid) = state.sessions.get(id).and_then(|s| s.pid) {
                let mut child = tokio::process::Command::new("/bin/kill")
                    .args(["-TERM", &pid.to_string()])
                    .kill_on_drop(true)
                    .spawn()?;
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                let _ = tokio::time::timeout(Duration::from_secs(3), async {
                    while state.sessions.get(id).is_some_and(|s| s.alive) {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await;
            }
            state.sessions.kill(id)?;
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while state.chat.get(id).is_some_and(|s| s.alive)
                || state.sessions.get(id).is_some_and(|s| s.alive)
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("agent did not stop before bundle deadline")?;
        // attach drains the journal writer's pending tail before removal.
        let drain = state.clone();
        let session = id.to_string();
        let _ = tokio::task::spawn_blocking(move || drain.chat.attach(&session, 0)).await;
        state.chat.remove(id);
        flush_ledger(&state, durable).await?;
    }
    if !native {
        return Ok(None);
    }
    let path = temp_path(&state);
    let destination = path.clone();
    let result = tokio::task::spawn_blocking(move || {
        write_archive(&state, &entry, workspace, mode, &destination)
    })
    .await?;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error);
    }
    Ok(Some(path))
}
fn write_archive(
    state: &AppState,
    entry: &LedgerEntry,
    workspace: Workspace,
    mode: ExportMode,
    path: &Path,
) -> Result<()> {
    let mut memory = BTreeMap::<String, Vec<u8>>::new();
    let journal = state.chat.journal_dir().join(format!("{}.jsonl", entry.id));
    if journal.is_file() {
        let mut bytes = read_capped(&journal, MAX_JOURNAL)?;
        // A snapshot sees only complete append-only records, never a torn tail.
        if let Some(end) = bytes.iter().rposition(|b| *b == b'\n') {
            bytes.truncate(end + 1);
        } else {
            bytes.clear();
        }
        validate_journal(&bytes)?;
        memory.insert("journal.jsonl".into(), bytes);
    }
    if let Some(native) = entry.agent.as_ref().and_then(|a| a.resume.as_deref()) {
        let settings = state.chat.index().settings(native);
        memory.insert(
            "index.json".into(),
            serde_json::to_vec(
                &json!({"model":settings.model,"effort":settings.effort,"mode":settings.mode}),
            )?,
        );
    }
    if let Some(view) = crate::lock(&state.view_state).get(&format!("ws_{}", entry.workspace_id)) {
        let bytes = serde_json::to_vec(&*view)?;
        if bytes.len() <= 64 * 1024 {
            memory.insert("view.json".into(), bytes);
        }
    }
    let native = native_path(state, entry)?;
    let mut members = BTreeMap::new();
    for (name, bytes) in &memory {
        members.insert(
            name.clone(),
            Member {
                bytes: bytes.len() as u64,
                sha256: hex(Sha256::digest(bytes)),
            },
        );
    }
    if let Some(native) = &native {
        members.insert("native.jsonl".into(), digest_file(native, MAX_NATIVE)?);
    }
    let session = entry.to_json();
    let manifest = Manifest {
        version: 1,
        workspace,
        session,
        source_host: hostname::get()
            .unwrap_or_default()
            .to_string_lossy()
            .chars()
            .take(256)
            .collect(),
        source_os: std::env::consts::OS.into(),
        stopped: mode == ExportMode::Stop && entry.agent.is_some(),
        links: crate::lock(&state.links)
            .iter()
            .filter(|(terminal, agent)| terminal.as_str() == entry.id || agent.as_str() == entry.id)
            .take(128)
            .map(|(terminal, agent)| (terminal.clone(), agent.clone()))
            .collect(),
        members,
    };
    let metadata = serde_json::to_vec(&manifest)?;
    if metadata.len() as u64 > MAX_METADATA {
        bail!("bundle metadata exceeds limit");
    }
    let mut zip = zip::ZipWriter::new(private_create(path)?);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o600);
    zip.start_file("manifest.json", options)?;
    zip.write_all(&metadata)?;
    for (name, bytes) in memory {
        zip.start_file(name, options)?;
        zip.write_all(&bytes)?;
    }
    if let Some(native) = native {
        zip.start_file("native.jsonl", options)?;
        let (mut file, _) = crate::fs::open_regular(&native)?;
        let mut digest = Sha256::new();
        let mut count = 0;
        let mut block = [0; 64 * 1024];
        loop {
            let n = file.read(&mut block)?;
            if n == 0 {
                break;
            }
            count += n as u64;
            if count > MAX_NATIVE {
                bail!("native transcript grew beyond limit");
            }
            digest.update(&block[..n]);
            zip.write_all(&block[..n])?;
        }
        let expected = &manifest.members["native.jsonl"];
        if hex(digest.finalize()) != expected.sha256 || expected.bytes != count {
            bail!("native transcript changed during snapshot; retry");
        }
    }
    let file = zip.finish()?;
    file.sync_all()?;
    if file.metadata()?.len() > MAX_ARCHIVE {
        bail!("archive exceeds limit");
    }
    Ok(())
}
fn validate_journal(bytes: &[u8]) -> Result<()> {
    let mut previous = 0;
    for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        if line.len() > 256 * 1024 {
            bail!("journal record exceeds limit");
        }
        let entry: chimaera_agent::journal::SeqEvent = serde_json::from_slice(line)?;
        if entry.seq <= previous {
            bail!("journal sequence is not increasing");
        }
        previous = entry.seq;
    }
    Ok(())
}
struct Opened {
    manifest: Manifest,
    entry: LedgerEntry,
    zip: zip::ZipArchive<File>,
    digest: String,
}
fn open_archive(path: &Path) -> Result<Opened> {
    let digest = digest_file(path, MAX_ARCHIVE)?.sha256;
    let (file, _) = crate::fs::open_regular(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    if zip.len() > 5 || zip.is_empty() {
        bail!("unexpected bundle members");
    }
    let mut names = std::collections::HashSet::new();
    for i in 0..zip.len() {
        let file = zip.by_index(i)?;
        if !matches!(
            file.name(),
            "manifest.json" | "native.jsonl" | "journal.jsonl" | "index.json" | "view.json"
        ) || !names.insert(file.name().to_owned())
            || file.compression() != zip::CompressionMethod::Stored
            || file.is_symlink()
            || file.is_dir()
        {
            bail!("invalid bundle member");
        }
        let cap = if file.name() == "native.jsonl" {
            MAX_NATIVE
        } else if file.name() == "journal.jsonl" {
            MAX_JOURNAL
        } else {
            MAX_METADATA
        };
        if file.size() > cap {
            bail!("bundle member exceeds limit");
        }
    }
    let manifest: Manifest =
        serde_json::from_reader(zip.by_name("manifest.json")?.take(MAX_METADATA + 1))?;
    if manifest.version != 1
        || manifest.members.len() + 1 != names.len()
        || manifest.links.len() > 128
        || manifest
            .links
            .iter()
            .any(|(terminal, agent)| !valid_id(terminal) || !valid_id(agent))
    {
        bail!("unsupported or inconsistent bundle manifest");
    }
    let mut entry =
        LedgerEntry::from_json(&manifest.session).context("invalid session identity")?;
    entry.suspended = true;
    if !valid_id(&entry.id)
        || !valid_id(&entry.workspace_id)
        || entry.workspace_id != manifest.workspace.id
        || !entry.cwd.is_absolute()
        || !manifest.workspace.root.is_absolute()
        || entry
            .cwd
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        || !entry.cwd.starts_with(&manifest.workspace.root)
    {
        bail!("invalid workspace or session identity");
    }
    for (name, expected) in &manifest.members {
        let mut file = zip.by_name(name)?;
        if file.size() != expected.bytes {
            bail!("bundle member length mismatch");
        }
        let mut digest = Sha256::new();
        let mut block = [0; 64 * 1024];
        loop {
            let n = file.read(&mut block)?;
            if n == 0 {
                break;
            }
            digest.update(&block[..n]);
        }
        if hex(digest.finalize()) != expected.sha256 {
            bail!("bundle member checksum mismatch");
        }
    }
    if entry.agent.is_some() && !names.contains("native.jsonl") {
        bail!("agent bundle requires native transcript");
    }
    if let Some(agent) = &entry.agent {
        if agent.native_cwd.as_ref().is_some_and(|cwd| {
            !cwd.is_absolute()
                || cwd.to_string_lossy().len() > 4096
                || cwd
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
        }) {
            bail!("invalid native cwd provenance");
        }
        let native = agent
            .resume
            .as_deref()
            .context("missing native conversation ID")?;
        if !crate::codex_notify::valid_thread_id(native) {
            bail!("invalid native conversation ID");
        }
        if agent.kind == crate::agents::AgentKind::Codex {
            use std::io::BufRead;
            let mut first = String::new();
            std::io::BufReader::new(zip.by_name("native.jsonl")?.take(MAX_METADATA + 1))
                .read_line(&mut first)?;
            if first.len() as u64 > MAX_METADATA {
                bail!("native header exceeds limit");
            }
            let header: Value = serde_json::from_str(&first)?;
            if header["type"] != "session_meta"
                || header["payload"]["id"].as_str() != Some(native)
                || header["payload"]["cwd"].as_str()
                    != agent.native_cwd.as_deref().unwrap_or(&entry.cwd).to_str()
            {
                bail!("native rollout identity does not match bundle");
            }
        }
    }
    if manifest.members.contains_key("journal.jsonl") {
        let mut bytes = Vec::new();
        zip.by_name("journal.jsonl")?.read_to_end(&mut bytes)?;
        validate_journal(&bytes)?;
    }
    Ok(Opened {
        manifest,
        entry,
        zip,
        digest,
    })
}
fn install_member(opened: &mut Opened, name: &str, path: &Path) -> Result<()> {
    let mut input = opened.zip.by_name(name)?;
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_file()) {
        bail!("bundle destination is not a regular file");
    }
    let temp = path.with_extension(format!("bundle-{}", &chimaera_core::generate_token()[..16]));
    let result = (|| {
        let mut output = private_create(&temp)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
fn native_destination(state: &AppState, entry: &LedgerEntry) -> Result<Option<PathBuf>> {
    let Some(agent) = &entry.agent else {
        return Ok(None);
    };
    let id = agent
        .resume
        .as_deref()
        .context("missing native conversation")?;
    if !crate::codex_notify::valid_thread_id(id) {
        bail!("invalid native conversation ID");
    }
    match agent.kind {
        crate::agents::AgentKind::Claude => Ok(Some(
            state
                .claude_projects_dir
                .join(crate::launcher::encode_cwd(&entry.cwd))
                .join(format!("{id}.jsonl")),
        )),
        crate::agents::AgentKind::Codex => {
            let home = state.codex_config_path.parent().context("Codex home")?;
            // Exact existing path wins; the bounded dated directory fallback
            // also works without a private Codex metadata database.
            Ok(Some(
                crate::codex_notify::find_rollout(
                    home,
                    id,
                    agent.native_cwd.as_deref().unwrap_or(&entry.cwd),
                )
                .unwrap_or_else(|| {
                    home.join("sessions/2000/01/01")
                        .join(format!("rollout-2000-01-01T00-00-00-{id}.jsonl"))
                }),
            ))
        }
        _ => bail!("unsupported native agent"),
    }
}
/// An ordinary (non-durable) import; the route streams into `import_inner`.
#[cfg(test)]
pub(crate) async fn import(
    state: Arc<AppState>,
    path: &Path,
    options: ImportOptions,
) -> Result<ImportedSession> {
    let _permit = OPERATIONS.try_acquire().context("bundle operation limit")?;
    import_inner(state, path, options, false).await
}
/// [`import`] for a Pro transfer: its ledger writes are durable.
pub(crate) async fn import_durable(
    state: Arc<AppState>,
    path: &Path,
    options: ImportOptions,
) -> Result<ImportedSession> {
    let _permit = OPERATIONS.try_acquire().context("bundle operation limit")?;
    import_inner(state, path, options, true).await
}
async fn import_inner(
    state: Arc<AppState>,
    path: &Path,
    options: ImportOptions,
    durable: bool,
) -> Result<ImportedSession> {
    let path = path.to_owned();
    let mut opened = tokio::task::spawn_blocking(move || open_archive(&path)).await??;
    if let Some(destination) = options.destination_root.as_ref() {
        let destination = destination.clone();
        let old_root = opened.manifest.workspace.root.clone();
        let old_cwd = opened.entry.cwd.clone();
        let (root, cwd) = tokio::task::spawn_blocking(move || -> Result<(PathBuf, PathBuf)> {
            let root = std::fs::canonicalize(&destination)
                .context("destination root must already exist")?;
            if !destination.is_absolute() || root != destination || !root.is_dir() {
                bail!("destination root must be a canonical directory");
            }
            let relative = old_cwd
                .strip_prefix(&old_root)
                .context("session cwd escapes original root")?;
            let cwd = std::fs::canonicalize(root.join(relative))
                .context("destination cwd must already exist")?;
            if !cwd.starts_with(&root) || !cwd.is_dir() {
                bail!("destination cwd escapes project root");
            }
            Ok((root, cwd))
        })
        .await??;
        if let Some(agent) = &mut opened.entry.agent {
            if agent.native_cwd.is_none() && cwd != opened.entry.cwd {
                agent.native_cwd = Some(opened.entry.cwd.clone());
            }
        }
        opened.entry.cwd = cwd;
        opened.manifest.workspace.root = root;
    }
    let id = opened.entry.id.clone();
    let workspace_id = opened.entry.workspace_id.clone();
    let _guard = crate::chat::ChatSwitchGuard::acquire(&state, &id, "transfer")
        .context("session lifecycle operation already in progress")?;
    if !crate::pro::may_import(&state, &workspace_id, options.epoch) {
        bail!("workspace owned elsewhere");
    }
    if crate::pro::owned_epoch(&state, &workspace_id).is_some_and(|epoch| epoch != options.epoch) {
        bail!("stale bundle ownership epoch");
    }
    let live = state.chat.get(&id).is_some_and(|s| s.alive)
        || state.sessions.get(&id).is_some_and(|s| s.alive);
    if live {
        let receipt = root(&state).join("imports").join(format!("{id}.json"));
        let receipt =
            tokio::task::spawn_blocking(move || read_capped(&receipt, MAX_METADATA)).await?;
        if receipt
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|value| {
                value["epoch"].as_u64() == Some(options.epoch)
                    && value["sha256"].as_str() == Some(&opened.digest)
                    && value["root"].as_str() == opened.manifest.workspace.root.to_str()
            })
        {
            return Ok(ImportedSession {
                id,
                workspace_id,
                paused: false,
            });
        }
        bail!("session already running; stop it before importing");
    }
    if let Some(native) = opened
        .entry
        .agent
        .as_ref()
        .and_then(|a| a.resume.as_deref())
    {
        if state
            .chat
            .list()
            .iter()
            .any(|session| session.alive && session.native_session_id.as_deref() == Some(native))
        {
            bail!("native conversation is active in another session");
        }
        let live_ids: std::collections::HashSet<_> = state
            .sessions
            .list()
            .into_iter()
            .filter(|session| session.alive)
            .map(|session| session.id)
            .collect();
        if crate::lock(&state.agents).iter().any(|(id, record)| {
            live_ids.contains(id)
                && record
                    .resume_id()
                    .or_else(|| record.resumed_from.clone())
                    .as_deref()
                    == Some(native)
        }) {
            bail!("native conversation is active in another session");
        }
    }
    for (terminal, agent) in &opened.manifest.links {
        if terminal != &id && agent != &id {
            bail!("bundle link does not belong to its session");
        }
        for endpoint in [terminal, agent] {
            let known = crate::lock(&state.session_workspaces)
                .get(endpoint)
                .cloned()
                .or_else(|| {
                    crate::lock(&state.deferred_sessions)
                        .get(endpoint)
                        .map(|entry| entry.workspace_id.clone())
                });
            if known.is_some_and(|known| known != workspace_id) {
                bail!("bundle link crosses workspace boundaries");
            }
        }
    }
    let paused =
        options.defer_start || (opened.entry.agent.is_none() && options.origin == Origin::Moved);
    opened.entry.handoff = Some(HandoffResume {
        fork: options.fork,
        origin: options.origin,
        epoch: options.epoch,
    });
    let archive_digest = opened.digest.clone();
    let linked = opened.manifest.links.clone();
    let setup = state.clone();
    let workspace = opened.manifest.workspace.clone();
    let (entry, view) =
        tokio::task::spawn_blocking(move || -> Result<(LedgerEntry, Option<Value>)> {
            let canonical = std::fs::canonicalize(&workspace.root)
                .context("workspace root must exist at its original path before import")?;
            if canonical != workspace.root || !opened.entry.cwd.is_dir() {
                bail!("bundle paths must exist without remapping");
            }
            crate::lock(&setup.workspaces).import_exact(workspace)?;
            if let Some(native) = native_destination(&setup, &opened.entry)? {
                install_member(&mut opened, "native.jsonl", &native)?;
                if opened
                    .entry
                    .agent
                    .as_ref()
                    .is_some_and(|a| a.kind == crate::agents::AgentKind::Codex)
                    && !crate::codex_notify::verify_rollout(
                        &native,
                        opened
                            .entry
                            .agent
                            .as_ref()
                            .unwrap()
                            .resume
                            .as_deref()
                            .unwrap(),
                        opened
                            .entry
                            .agent
                            .as_ref()
                            .unwrap()
                            .native_cwd
                            .as_deref()
                            .unwrap_or(&opened.entry.cwd),
                    )
                {
                    bail!("native rollout identity does not match bundle");
                }
                opened.entry.agent.as_mut().unwrap().transcript = Some(native);
            }
            if opened.manifest.members.contains_key("journal.jsonl") {
                let mut bytes = Vec::new();
                opened
                    .zip
                    .by_name("journal.jsonl")?
                    .read_to_end(&mut bytes)?;
                validate_journal(&bytes)?;
                let journal = setup
                    .chat
                    .journal_dir()
                    .join(format!("{}.jsonl", opened.entry.id));
                install_member(&mut opened, "journal.jsonl", &journal)?;
            }
            if let Some(native) = opened
                .entry
                .agent
                .as_ref()
                .and_then(|a| a.resume.as_deref())
            {
                let settings: Value = if opened.manifest.members.contains_key("index.json") {
                    serde_json::from_reader(opened.zip.by_name("index.json")?)?
                } else {
                    json!({})
                };
                setup
                    .chat
                    .index()
                    .record_settings(native, &opened.entry.id, |target| {
                        target.model = settings["model"].as_str().map(str::to_owned);
                        target.effort = settings["effort"].as_str().map(str::to_owned);
                        target.mode = settings["mode"].as_str().map(str::to_owned);
                    });
            }
            let view = if opened.manifest.members.contains_key("view.json") {
                let bytes = opened.zip.by_name("view.json")?.size();
                if bytes > 64 * 1024 {
                    bail!("view state exceeds limit");
                }
                Some(serde_json::from_reader(opened.zip.by_name("view.json")?)?)
            } else {
                None
            };
            Ok((opened.entry, view))
        })
        .await??;
    if let Some(view) = view {
        let key = format!("ws_{workspace_id}");
        let task = {
            let mut store = crate::lock(&state.view_state);
            if store.get(&key).is_none() {
                Some(store.put(key, view))
            } else {
                None
            }
        };
        if let Some(task) = task {
            task.await??;
        }
    }
    crate::ledger::defer(&state, entry.clone())?;
    {
        let mut links = crate::lock(&state.links);
        for (terminal, agent) in linked {
            links.insert(terminal, agent);
        }
    }
    flush_ledger(&state, durable).await?;
    if !paused {
        let workspace = crate::lock(&state.workspaces)
            .get(&workspace_id)
            .context("workspace disappeared")?;
        state.chat.remove(&id);
        crate::ledger::respawn_transfer(
            &state,
            &entry,
            workspace,
            options.fork,
            Some(options.origin.as_str()),
        )
        .await?;
        crate::lock(&state.deferred_sessions).remove(&id);
        state.session_proxy.clear_workspace(&workspace_id);
        flush_ledger(&state, durable).await?;
    }
    let imported_root = crate::lock(&state.workspaces)
        .get(&workspace_id)
        .context("workspace disappeared")?
        .root;
    let receipt = root(&state).join("imports").join(format!("{id}.json"));
    tokio::task::spawn_blocking(move || {
        crate::persist::atomic_write_json(
            &receipt,
            serde_json::to_vec(
                &json!({"epoch":options.epoch,"sha256":archive_digest,"root":imported_root}),
            )?,
        )
    })
    .await??;
    state.changes.notify_waiters();
    Ok(ImportedSession {
        id,
        workspace_id,
        paused,
    })
}
#[derive(Deserialize)]
pub(crate) struct ExportRequest {
    stop: bool,
}
fn failure(error: anyhow::Error) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"error":error.to_string()})),
    )
        .into_response()
}
async fn archive_response(path: PathBuf) -> Result<Response> {
    let file = tokio::fs::File::open(&path).await?;
    let len = file.metadata().await?.len();
    tokio::fs::remove_file(path).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response())
}
pub(crate) async fn snapshot_route(
    State(state): State<Arc<AppState>>,
    RoutePath(id): RoutePath<String>,
) -> Response {
    match export(state, &id, ExportMode::Snapshot).await {
        Ok(path) => archive_response(path).await.unwrap_or_else(failure),
        Err(error) => failure(error),
    }
}
pub(crate) async fn export_route(
    State(state): State<Arc<AppState>>,
    RoutePath(id): RoutePath<String>,
    Json(body): Json<ExportRequest>,
) -> Response {
    let mode = if body.stop {
        ExportMode::Stop
    } else {
        ExportMode::Snapshot
    };
    // Through a keeper this route moves work between machines (the durable
    // ledger then matters); for everyone else it is an ordinary export.
    let exported = if crate::pro::configured(&state) {
        export_durable(state, &id, mode).await
    } else {
        export(state, &id, mode).await
    };
    match exported {
        Ok(path) => archive_response(path).await.unwrap_or_else(failure),
        Err(error) => failure(error),
    }
}
pub(crate) async fn import_route(
    State(state): State<Arc<AppState>>,
    Query(options): Query<ImportOptions>,
    body: Body,
) -> Response {
    let path = temp_path(&state);
    let result: Result<ImportedSession> = async {
        let _permit = OPERATIONS.try_acquire().context("bundle operation limit")?;
        let create = path.clone();
        let file = tokio::task::spawn_blocking(move || private_create(&create)).await??;
        let mut file = tokio::fs::File::from_std(file);
        let mut stream = body.into_data_stream();
        let mut bytes = 0;
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(30), stream.next())
            .await
            .context("bundle upload stalled")?
        {
            let chunk = chunk?;
            bytes += chunk.len() as u64;
            if bytes > MAX_ARCHIVE {
                bail!("archive exceeds limit");
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        drop(file);
        let durable = crate::pro::configured(&state);
        import_inner(state, &path, options, durable).await
    }
    .await;
    let _ = tokio::fs::remove_file(path).await;
    match result {
        Ok(imported) => Json(imported).into_response(),
        Err(error) => failure(error),
    }
}

/// Suspended identities stay in the roster so window tabs survive a transfer
/// or an ownership check after restart, without claiming an agent is running.
/// `label` names the row when it has no pinned name; where it is shown
/// decides the words (see `pro::paused_label`).
pub(crate) fn paused_row(entry: &LedgerEntry, label: &str) -> Value {
    let agent = entry.agent.as_ref();
    let name = entry
        .pinned_name
        .as_deref()
        .or_else(|| {
            agent
                .map(|a| a.title.as_str())
                .filter(|t| !t.trim().is_empty())
        })
        .unwrap_or(label);
    json!({"id":entry.id,"workspace_id":entry.workspace_id,"cwd":entry.cwd,"cwd_current":entry.cwd,"name":name,"display_name":name,"renamed":entry.pinned_name.is_some(),"cols":entry.cols,"rows":entry.rows,"created_at":entry.created_at,"alive":false,"exit_status":null,"title":null,"pid":null,"phase":"unknown","exec_stage":null,"kind":if agent.is_some(){"agent"}else{"shell"},"agent_kind":agent.map(|a|a.kind.as_str()),"agent_state":"finished","agent_title":agent.map(|a|a.title.as_str()),"ui":agent.map_or("term",|a|if a.ui==chimaera_agent::model::SessionUi::Chat{"chat"}else{"term"}),"chat_capable":agent.is_some(),"placement":"here","placement_available":false,"suspended":true,"last_input_ms":null})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "chimaera-bundle-test-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::canonicalize(path).unwrap()
    }
    #[test]
    fn archive_preserves_native_and_journal_bytes_and_rejects_extra_paths() {
        let directory = dir();
        let data = directory.join("data");
        let mut state = AppState::new(
            "fixture".into(),
            "fixture".into(),
            0,
            0,
            data.clone(),
            directory.join("config"),
        );
        state.claude_projects_dir = directory.join("claude/projects");
        let workspace = crate::lock(&state.workspaces)
            .add(directory.clone())
            .unwrap();
        let native = "11111111-1111-4111-8111-111111111111";
        let entry = LedgerEntry {
            id: "s-bundle".into(),
            suspended: false,
            handoff: None,
            workspace_id: workspace.id.clone(),
            cwd: directory.clone(),
            pinned_name: Some("preserved title".into()),
            cols: 90,
            rows: 30,
            theme: "light".into(),
            created_at: 123,
            agent: Some(crate::ledger::LedgerAgent {
                kind: crate::agents::AgentKind::Claude,
                resume: Some(native.into()),
                transcript: None,
                native_cwd: None,
                title: "native task".into(),
                ui: chimaera_agent::model::SessionUi::Chat,
                model: Some("fixture-model".into()),
                carryover: None,
            }),
        };
        let native_path = native_destination(&state, &entry).unwrap().unwrap();
        let native_bytes=format!("{{\"sessionId\":\"{native}\",\"type\":\"user\",\"message\":{{\"content\":\"preserved\"}}}}\n");
        private_create(&native_path)
            .unwrap()
            .write_all(native_bytes.as_bytes())
            .unwrap();
        let journal = format!(
            "{}\n",
            serde_json::to_string(&chimaera_agent::journal::SeqEvent {
                seq: 17,
                ts: 123,
                ev: chimaera_agent::model::AgentEvent::Notice {
                    text: "journal stays at sequence 17".into()
                }
            })
            .unwrap()
        );
        let journal_path = state.chat.journal_dir().join("s-bundle.jsonl");
        private_create(&journal_path)
            .unwrap()
            .write_all(journal.as_bytes())
            .unwrap();
        let path = directory.join("valid.zip");
        write_archive(&state, &entry, workspace, ExportMode::Snapshot, &path).unwrap();
        let mut opened = open_archive(&path).unwrap();
        assert_eq!(opened.entry.id, entry.id);
        assert_eq!(opened.entry.cwd, entry.cwd);
        for (name, expected) in [
            ("native.jsonl", native_bytes.as_bytes()),
            ("journal.jsonl", journal.as_bytes()),
        ] {
            let mut bytes = Vec::new();
            opened
                .zip
                .by_name(name)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes, expected);
        }
        let mut writer = zip::ZipWriter::new_append(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap(),
        )
        .unwrap();
        writer
            .start_file(
                "../../auth.json",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(b"{}").unwrap();
        writer.finish().unwrap();
        assert!(
            open_archive(&path).is_err(),
            "unexpected members are never extracted"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn journal_rejects_duplicate_sequence_and_oversized_records() {
        let event = serde_json::to_string(&chimaera_agent::journal::SeqEvent {
            seq: 4,
            ts: 1,
            ev: chimaera_agent::model::AgentEvent::Notice {
                text: "fixture".into(),
            },
        })
        .unwrap();
        assert!(validate_journal(format!("{event}\n{event}\n").as_bytes()).is_err());
        assert!(validate_journal(&vec![b'a'; 256 * 1024 + 1]).is_err());
    }
}
