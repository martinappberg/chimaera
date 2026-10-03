//! Portable staging separates HEAD, index and working bytes. Only validated
//! entries/blobs travel; stat data and executable/machine-local extensions do not.
use super::{policy, project_git, transport};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_INDEX: usize = 16 * 1024 * 1024;
const INDEX_PATH: &str = "git/index.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::pro) struct Descriptor {
    pub version: u8,
    pub index: String,
}
impl Descriptor {
    fn new() -> Self {
        Self {
            version: 1,
            index: INDEX_PATH.into(),
        }
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && self.index == INDEX_PATH,
            "unsupported portable Git staging"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Format {
    Sha1,
    Sha256,
}
impl Format {
    fn width(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }
    fn hash(self, bytes: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha1 => sha1::Sha1::digest(bytes).to_vec(),
            Self::Sha256 => sha2::Sha256::digest(bytes).to_vec(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    mode: u32,
    stage: u8,
    oid: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    intent: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::pro) struct Index {
    version: u8,
    format: Format,
    entries: Vec<Entry>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn unhex(value: &str, format: Format) -> Result<Vec<u8>> {
    ensure!(
        value.len() == format.width() * 2
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid staged Git object"
    );
    (0..value.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&value[at..at + 2], 16).map_err(Into::into))
        .collect()
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && policy::allowed_path(Path::new(path))
        && !policy::contains_credential(path.as_bytes())
}
impl Index {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && self.entries.len() <= policy::MAX_PATHS,
            "portable Git index exceeds limit"
        );
        let mut previous: Option<&Entry> = None;
        let mut paths = BTreeSet::new();
        for entry in &self.entries {
            ensure!(valid_path(&entry.path), "unsafe staged Git path");
            ensure!(
                matches!(entry.mode, 0o100644 | 0o100755 | 0o120000 | 0o160000)
                    && entry.stage <= 3
                    && (!entry.intent || entry.stage == 0),
                "unsupported staged Git entry"
            );
            unhex(&entry.oid, self.format)?;
            if let Some(old) = previous {
                ensure!(
                    (&old.path, old.stage) < (&entry.path, entry.stage),
                    "unsorted or duplicate Git index entry"
                );
                ensure!(
                    old.path != entry.path || (old.stage != 0 && entry.stage != 0),
                    "mixed merged and unmerged Git entry"
                );
            }
            let mut parent = Path::new(&entry.path).parent();
            while let Some(path) = parent {
                ensure!(!paths.contains(path), "overlapping staged Git paths");
                parent = path.parent();
            }
            paths.insert(PathBuf::from(&entry.path));
            previous = Some(entry);
        }
        Ok(())
    }
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at + 4)
            .context("truncated Git index")?
            .try_into()?,
    ))
}
fn parse(bytes: &[u8], format: Format) -> Result<Index> {
    ensure!(
        bytes.len() <= MAX_INDEX && bytes.len() >= 12 + format.width(),
        "invalid Git index size"
    );
    let end = bytes.len() - format.width();
    ensure!(
        &bytes[..4] == b"DIRC" && format.hash(&bytes[..end]) == bytes[end..],
        "invalid Git index checksum"
    );
    let version = u32_at(bytes, 4)?;
    ensure!(matches!(version, 2 | 3), "unsupported Git index version");
    let count = u32_at(bytes, 8)? as usize;
    ensure!(count <= policy::MAX_PATHS, "Git index entry limit exceeded");
    let mut at = 12;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let start = at;
        let mode = u32_at(bytes, at + 24)?;
        at += 40;
        let oid = hex(bytes
            .get(at..at + format.width())
            .filter(|_| at + format.width() <= end)
            .context("truncated Git index")?);
        at += format.width();
        let flags = u16::from_be_bytes(
            bytes
                .get(at..at + 2)
                .context("truncated Git flags")?
                .try_into()?,
        );
        at += 2;
        let mut intent = false;
        if flags & 0x4000 != 0 {
            ensure!(version == 3, "invalid extended Git index flags");
            let extended = u16::from_be_bytes(
                bytes
                    .get(at..at + 2)
                    .context("truncated extended Git flags")?
                    .try_into()?,
            );
            at += 2;
            ensure!(
                extended & !0x2000 == 0,
                "sparse or unknown Git index flags are unsupported"
            );
            intent = extended & 0x2000 != 0;
        }
        let length = bytes
            .get(at..end)
            .context("truncated Git index path")?
            .iter()
            .position(|b| *b == 0)
            .context("unterminated Git index path")?;
        ensure!(
            length <= 4096 && usize::from(flags & 0xfff) == length.min(0xfff),
            "invalid Git index path length"
        );
        let path = std::str::from_utf8(&bytes[at..at + length])
            .context("non-UTF-8 Git paths cannot be copied")?
            .to_owned();
        at += length;
        let padding = 8 - (at - start) % 8;
        ensure!(
            bytes
                .get(at..at + padding)
                .is_some_and(|padding| padding.iter().all(|b| *b == 0))
                && at + padding <= end,
            "invalid Git index padding"
        );
        at += padding;
        entries.push(Entry {
            path,
            mode,
            stage: ((flags >> 12) & 3) as u8,
            oid,
            intent,
        });
    }
    while at < end {
        let signature = bytes
            .get(at..at + 4)
            .context("truncated Git index extension")?;
        ensure!(
            signature[0].is_ascii_uppercase(),
            "mandatory Git index extension is unsupported"
        );
        let size = u32_at(bytes, at + 4)? as usize;
        at = at
            .checked_add(8)
            .and_then(|at| at.checked_add(size))
            .context("Git index extension overflow")?;
        ensure!(at <= end, "truncated Git index extension");
    }
    let index = Index {
        version: 1,
        format,
        entries,
    };
    index.validate()?;
    Ok(index)
}

fn encode(index: &Index) -> Result<Vec<u8>> {
    index.validate()?;
    let version = if index.entries.iter().any(|entry| entry.intent) {
        3u32
    } else {
        2
    };
    let mut bytes = b"DIRC".to_vec();
    bytes.extend_from_slice(&version.to_be_bytes());
    bytes.extend_from_slice(&(index.entries.len() as u32).to_be_bytes());
    for entry in &index.entries {
        let start = bytes.len();
        bytes.extend_from_slice(&[0u8; 24]);
        bytes.extend_from_slice(&entry.mode.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 12]);
        bytes.extend_from_slice(&unhex(&entry.oid, index.format)?);
        let flags = ((entry.stage as u16) << 12)
            | (entry.path.len().min(0xfff) as u16)
            | if entry.intent { 0x4000 } else { 0 };
        bytes.extend_from_slice(&flags.to_be_bytes());
        if entry.intent {
            bytes.extend_from_slice(&0x2000u16.to_be_bytes());
        }
        bytes.extend_from_slice(entry.path.as_bytes());
        bytes.resize(bytes.len() + 8 - (bytes.len() - start) % 8, 0);
        ensure!(
            bytes.len() <= MAX_INDEX - index.format.width(),
            "portable Git index exceeds limit"
        );
    }
    bytes.extend_from_slice(&index.format.hash(&bytes));
    Ok(bytes)
}

fn read_private(root: &Path, relative: &Path, cap: u64) -> Result<Vec<u8>> {
    let directory = super::super::install::directory(root)?;
    let mut file = crate::download::open_beneath(
        &directory,
        relative,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
    )?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= cap,
        "staged Git file exceeds limit"
    );
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(cap + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= cap, "staged Git file exceeds limit");
    Ok(bytes)
}
async fn format(root: &Path) -> Result<Format> {
    let bytes = transport::git_output(
        project_git(root).await?,
        &["rev-parse", "--show-object-format"],
        vec![],
    )
    .await?;
    match bytes.as_slice() {
        b"sha1\n" => Ok(Format::Sha1),
        b"sha256\n" => Ok(Format::Sha256),
        _ => bail!("unsupported Git object format"),
    }
}

pub(in crate::pro) async fn local_index(root: &Path, temporary: &Path) -> Result<Index> {
    let format = format(root).await?;
    let bytes = transport::git_output(
        project_git(root).await?,
        &["rev-parse", "--absolute-git-dir"],
        vec![],
    )
    .await?;
    let directory = PathBuf::from(std::str::from_utf8(&bytes)?.trim_end_matches('\n'));
    let source = directory.clone();
    let bytes = tokio::task::spawn_blocking(move || -> Result<_> {
        match read_private(&source, Path::new("index"), MAX_INDEX as u64) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    })
    .await??;
    let Some(bytes) = bytes else {
        return Ok(Index {
            version: 1,
            format,
            entries: vec![],
        });
    };
    ensure!(
        bytes.len() >= 12 + format.width() && &bytes[..4] == b"DIRC",
        "invalid source Git index"
    );
    let content = bytes.len() - format.width();
    ensure!(
        format.hash(&bytes[..content]) == bytes[content..],
        "invalid source Git index checksum"
    );
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(temporary).await?;
    use tokio::io::AsyncWriteExt;
    file.write_all(&bytes).await?;
    file.sync_all().await?;
    drop(file);
    let mut command = project_git(root).await?;
    command.env("GIT_INDEX_FILE", temporary).args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
    ]);
    let result = transport::git_output(
        command,
        &["update-index", "--no-split-index", "--index-version=2"],
        vec![],
    )
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(temporary).await;
        return Err(error.context("Git staging cannot be normalized safely"));
    }
    let parent = temporary
        .parent()
        .context("private index parent unavailable")?
        .to_owned();
    let relative = PathBuf::from(
        temporary
            .file_name()
            .context("private index name unavailable")?,
    );
    let result = tokio::task::spawn_blocking(move || {
        parse(&read_private(&parent, &relative, MAX_INDEX as u64)?, format)
    })
    .await?;
    tokio::fs::remove_file(temporary).await?;
    result
}

pub(in crate::pro) async fn capture(
    root: &Path,
    artifact: &Path,
    budget: u64,
    max_file: u64,
) -> Result<(Descriptor, u64)> {
    let git = artifact.join("git");
    tokio::fs::create_dir_all(git.join("blobs")).await?;
    let index = local_index(root, &git.join("capture.index")).await?;
    let bytes = serde_json::to_vec(&index)?;
    ensure!(
        bytes.len() <= MAX_INDEX && bytes.len() as u64 <= budget,
        "portable Git staging exceeds storage limit"
    );
    let mut used = bytes.len() as u64;
    let mut objects = BTreeSet::new();
    for entry in &index.entries {
        if entry.mode == 0o160000 || !objects.insert(entry.oid.clone()) {
            continue;
        }
        let mut command = project_git(root).await?;
        command.args(["--no-replace-objects", "cat-file", "blob", &entry.oid]);
        let cap = max_file.min(policy::MAX_FILE_BYTES).min(budget - used);
        let length = transport::run_file(
            command,
            git.join("blobs").join(&entry.oid),
            Duration::from_secs(45),
            cap,
        )
        .await?;
        let (artifact, oid) = (artifact.to_owned(), entry.oid.clone());
        tokio::task::spawn_blocking(move || verify_blob(&artifact, &oid, index.format, None, cap))
            .await??;
        used = used
            .checked_add(length)
            .context("Git staging storage overflow")?;
    }
    tokio::fs::write(artifact.join(INDEX_PATH), bytes).await?;
    Ok((Descriptor::new(), used))
}

async fn load(artifact: &Path, descriptor: &Descriptor) -> Result<Index> {
    descriptor.validate()?;
    let artifact = artifact.to_owned();
    let index = tokio::task::spawn_blocking(move || -> Result<Index> {
        Ok(serde_json::from_slice(&read_private(
            &artifact,
            Path::new(INDEX_PATH),
            MAX_INDEX as u64,
        )?)?)
    })
    .await??;
    index.validate()?;
    Ok(index)
}

pub(in crate::pro) async fn require_service_format(
    artifact: &Path,
    descriptor: &Descriptor,
) -> Result<()> {
    ensure!(
        load(artifact, descriptor).await?.format == Format::Sha1,
        "Cloud Git transfer currently requires a SHA-1 repository; SHA-256 service support is unavailable"
    );
    Ok(())
}
fn merge(
    local: &Index,
    baseline: Option<&Index>,
    incoming: &Index,
) -> Result<(Index, Vec<String>)> {
    ensure!(
        local.format == incoming.format && baseline.is_none_or(|base| base.format == local.format),
        "Git staging object format changed"
    );
    let group = |index: &Index| {
        let mut paths: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        for entry in &index.entries {
            paths
                .entry(entry.path.clone())
                .or_default()
                .push(entry.clone());
        }
        paths
    };
    let local_paths = group(local);
    let incoming_paths = group(incoming);
    let baseline_paths = baseline.map(group);
    let paths: BTreeSet<_> = local_paths
        .keys()
        .chain(incoming_paths.keys())
        .chain(baseline_paths.iter().flat_map(|base| base.keys()))
        .cloned()
        .collect();
    let mut entries = Vec::new();
    let mut conflicts = Vec::new();
    for path in paths {
        let ours = local_paths.get(&path);
        let theirs = incoming_paths.get(&path);
        let base = baseline_paths.as_ref().and_then(|base| base.get(&path));
        let chosen = if ours == theirs || (baseline.is_some() && theirs == base) {
            ours
        } else if baseline.is_some() && ours == base {
            theirs
        } else {
            conflicts.push(path);
            ours
        };
        if let Some(chosen) = chosen {
            entries.extend(chosen.iter().cloned());
        }
    }
    let selected = Index {
        version: 1,
        format: local.format,
        entries: entries.clone(),
    };
    let selected_paths = group(&selected);
    let mut overlapping = BTreeSet::new();
    for path in selected_paths.keys() {
        let mut parent = Path::new(path).parent();
        while let Some(prefix) = parent {
            if let Some(name) = prefix
                .to_str()
                .filter(|name| selected_paths.contains_key(*name))
            {
                overlapping.insert(name.to_owned());
                overlapping.insert(path.clone());
            }
            parent = prefix.parent();
        }
    }
    if !overlapping.is_empty() {
        entries.retain(|entry| !overlapping.contains(&entry.path));
        for path in &overlapping {
            if let Some(ours) = local_paths.get(path) {
                entries.extend(ours.iter().cloned());
            }
        }
        conflicts.extend(overlapping);
        entries.sort_by(|a, b| (&a.path, a.stage).cmp(&(&b.path, b.stage)));
        conflicts.sort();
        conflicts.dedup();
    }
    let index = Index {
        version: 1,
        format: local.format,
        entries,
    };
    index.validate()?;
    Ok((index, conflicts))
}

/// Reads/exported bytes use an anchored no-follow descriptor. Git only sees our
/// verified private copy, not an imported path whose inode can change beneath it.
fn verify_blob(
    artifact: &Path,
    oid: &str,
    format: Format,
    destination: Option<&Path>,
    cap: u64,
) -> Result<u64> {
    let directory = super::super::install::directory(artifact)?;
    let mut input = crate::download::open_beneath(
        &directory,
        &Path::new("git/blobs").join(oid),
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
    )?;
    let metadata = input.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= cap,
        "staged Git blob exceeds storage limit"
    );
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = destination
        .map(|destination| options.open(destination))
        .transpose()?;
    let mut sha1 = sha1::Sha1::new();
    let mut sha256 = sha2::Sha256::new();
    let header = format!("blob {}\0", metadata.len());
    sha1.update(header.as_bytes());
    sha256.update(header.as_bytes());
    let mut buffer = [0u8; 16 * 1024];
    let mut overlap = Vec::with_capacity(128 + buffer.len());
    let mut length = 0u64;
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length = length
            .checked_add(count as u64)
            .context("staged Git blob size overflow")?;
        ensure!(
            length <= metadata.len() && length <= cap,
            "staged Git blob changed or exceeds storage limit"
        );
        overlap.extend_from_slice(&buffer[..count]);
        ensure!(
            !policy::contains_credential(&overlap),
            "staged Git blob contains credentials"
        );
        sha1.update(&buffer[..count]);
        sha256.update(&buffer[..count]);
        if let Some(output) = &mut output {
            output.write_all(&buffer[..count])?;
        }
        let discard = overlap.len().saturating_sub(128);
        overlap.drain(..discard);
    }
    ensure!(length == metadata.len(), "staged Git blob changed");
    let digest = match format {
        Format::Sha1 => hex(&sha1.finalize()),
        Format::Sha256 => hex(&sha256.finalize()),
    };
    ensure!(digest == oid, "staged Git blob object identity mismatch");
    if let Some(output) = &mut output {
        output.sync_all()?;
    }
    Ok(length)
}

/// Only a private checkout is mutated. Its index and object changes subsequently
/// join repository::prepare_receive's existing recoverable file transaction.
pub(in crate::pro) struct Receiving<'a> {
    pub handoff: &'a Path,
    pub descriptor: &'a Descriptor,
    pub baseline: Option<(&'a Path, &'a Descriptor)>,
    pub budget: u64,
    pub max_file: u64,
}
pub(in crate::pro) async fn receive(
    checkout: &Path,
    local: Option<&Index>,
    transfer: Receiving<'_>,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<super::StagingStatus> {
    let Receiving {
        handoff,
        descriptor,
        baseline,
        budget,
        max_file,
    } = transfer;
    let incoming = load(handoff, descriptor).await?;
    ensure!(
        format(checkout).await? == incoming.format,
        "Git staging object format changed"
    );
    let baseline = match baseline {
        Some((path, descriptor)) => Some(load(path, descriptor).await?),
        None => None,
    };
    let metadata = tokio::fs::metadata(handoff.join(INDEX_PATH)).await?.len();
    ensure!(metadata <= budget, "Git staging exceeds storage limit");
    let mut used = metadata;
    let mut objects = BTreeSet::new();
    for entry in &incoming.entries {
        if entry.mode == 0o160000 || !objects.insert(entry.oid.clone()) {
            continue;
        }
        check()?;
        let temporary = checkout
            .join(".git")
            .join(format!("index-blob-{}", chimaera_core::generate_token()));
        let (artifact, oid, destination) =
            (handoff.to_owned(), entry.oid.clone(), temporary.clone());
        let cap = max_file.min(policy::MAX_FILE_BYTES).min(budget - used);
        let length = tokio::task::spawn_blocking(move || {
            let result = verify_blob(&artifact, &oid, incoming.format, Some(&destination), cap);
            if result.is_err() {
                let _ = fs::remove_file(&destination);
            }
            result
        })
        .await??;
        used = used
            .checked_add(length)
            .context("Git staging size overflow")?;
        check()?;
        let result = transport::git_output(
            project_git(checkout).await?,
            &[
                "--no-replace-objects",
                "hash-object",
                "--no-filters",
                "-w",
                "--",
                temporary.to_str().context("invalid private blob path")?,
            ],
            vec![],
        )
        .await;
        tokio::fs::remove_file(temporary).await?;
        ensure!(
            result? == format!("{}\n", entry.oid).as_bytes(),
            "staged Git object import mismatch"
        );
    }
    let (merged, conflicts) = match local {
        Some(local) => merge(local, baseline.as_ref(), &incoming)?,
        None => (incoming.clone(), Vec::new()),
    };
    let merged_bytes = encode(&merged)?;
    let status = if !conflicts.is_empty() {
        check()?;
        let token = chimaera_core::generate_token();
        let relative = format!("chimaera-staging/{token}");
        let recovery = checkout.join(".git").join(&relative);
        let incoming_bytes = encode(&incoming)?;
        let local = local.context("local staging unavailable")?;
        let local_bytes = encode(local)?;
        let conflict_bytes = serde_json::to_vec(&conflicts)?;
        // An index file does not make its unselected blobs reachable to Git GC.
        // Pin all versions under a private tree, never an outbound mirror ref.
        let oids: BTreeSet<_> = incoming
            .entries
            .iter()
            .chain(&local.entries)
            .filter(|entry| entry.mode != 0o160000)
            .map(|entry| entry.oid.as_str())
            .collect();
        let mut tree = Vec::new();
        for (number, oid) in oids.iter().enumerate() {
            write!(tree, "100644 blob {oid}\t{number:06}\0")?;
        }
        ensure!(
            tree.len() <= MAX_INDEX,
            "Git staging recovery exceeds metadata limit"
        );
        let recovery_bytes = incoming_bytes
            .len()
            .checked_add(local_bytes.len())
            .and_then(|n| n.checked_add(conflict_bytes.len()))
            .and_then(|n| n.checked_add(tree.len()))
            .context("Git staging recovery size overflow")?;
        ensure!(
            used.checked_add(recovery_bytes as u64)
                .is_some_and(|n| n <= budget),
            "Git staging recovery exceeds storage limit"
        );
        let tree_oid =
            transport::git_output(project_git(checkout).await?, &["mktree", "-z"], tree).await?;
        let tree_oid = std::str::from_utf8(&tree_oid)?.trim();
        let reference = format!("refs/chimaera/staging/{token}");
        check()?;
        transport::git_output(
            project_git(checkout).await?,
            &["update-ref", &reference, tree_oid],
            vec![],
        )
        .await?;
        tokio::fs::create_dir_all(&recovery).await?;
        tokio::fs::write(recovery.join("incoming.index"), incoming_bytes).await?;
        tokio::fs::write(recovery.join("local.index"), local_bytes).await?;
        tokio::fs::write(recovery.join("conflicts.json"), conflict_bytes).await?;
        tokio::fs::write(recovery.join("objects.ref"), format!("{reference}\n")).await?;
        let mut bytes = 0;
        let paths = conflicts
            .iter()
            .take(16)
            .take_while(|path| {
                bytes += path.len();
                bytes <= 16 * 1024
            })
            .cloned()
            .collect();
        super::StagingStatus::Conflicts {
            paths,
            total: conflicts.len(),
            recovery: relative,
        }
    } else {
        super::StagingStatus::Synced
    };
    check()?;
    tokio::fs::write(checkout.join(".git/index"), merged_bytes).await?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "chimaera-index-{}",
                chimaera_core::generate_token()
            ));
            fs::create_dir(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }
        fn repo(&self, name: &str, sha256: bool) -> PathBuf {
            let root = self.0.join(name);
            fs::create_dir(&root).unwrap();
            let args = if sha256 {
                vec!["init", "--quiet", "--object-format=sha256"]
            } else {
                vec!["init", "--quiet"]
            };
            git(&root, &args);
            root
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[cfg(unix)]
    #[test]
    fn fifo_index_and_blob_refuse_before_waiting_for_a_writer() {
        const CHILD_ROOT: &str = "CHIMAERA_TEST_STAGING_FIFO_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            assert!(root.is_absolute());
            let oid = "a".repeat(40);
            fs::create_dir_all(root.join("git/blobs")).unwrap();
            for relative in [PathBuf::from("index"), Path::new("git/blobs").join(&oid)] {
                nix::unistd::mkfifo(
                    &root.join(relative),
                    nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
                )
                .unwrap();
            }
            assert!(read_private(&root, Path::new("index"), 512).is_err());
            assert!(verify_blob(&root, &oid, Format::Sha1, None, 512).is_err());
            fs::write(root.join("fifo-refusal-checked"), b"both refused").unwrap();
            return;
        }
        let fixture = Fixture::new();
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let test = format!(
            "{}::fifo_index_and_blob_refuse_before_waiting_for_a_writer",
            module_path!().split_once("::").unwrap().1
        );
        let mut child = Child(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &test])
                .env(CHILD_ROOT, &fixture.0)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "staging FIFO child failed");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "staging must refuse a FIFO without any writer"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // An old blocking implementation is killed and reaped by Child on the
        // deadline assertion; no rescue writer or stranded test thread exists.
        assert_eq!(
            fs::read(fixture.0.join("fifo-refusal-checked")).unwrap(),
            b"both refused"
        );
    }
    fn git(root: &Path, args: &[&str]) -> Vec<u8> {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "fixture Git command failed: {args:?}"
        );
        output.stdout
    }
    async fn receive_fresh(root: &Path, artifact: &Path, descriptor: &Descriptor) {
        receive(
            root,
            None,
            Receiving {
                handoff: artifact,
                descriptor,
                baseline: None,
                budget: 4 * 1024 * 1024,
                max_file: 1024 * 1024,
            },
            &|| Ok(()),
        )
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn distinct_head_index_worktree_and_staged_only_modes_binary_delete_intent() {
        for sha256 in [false, true] {
            let fixture = Fixture::new();
            let source = fixture.repo("source", sha256);
            fs::write(source.join("partial"), b"HEAD").unwrap();
            fs::write(source.join("deleted"), b"base").unwrap();
            git(&source, &["add", "."]);
            git(&source, &["commit", "-qm", "base"]);
            fs::write(source.join("partial"), b"INDEX").unwrap();
            fs::write(source.join("only"), b"staged only").unwrap();
            fs::write(source.join("binary"), [0, 255, 128, 0]).unwrap();
            fs::write(source.join("exec"), b"#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::{symlink, PermissionsExt};
                fs::set_permissions(source.join("exec"), fs::Permissions::from_mode(0o755))
                    .unwrap();
                symlink("partial", source.join("link")).unwrap();
            }
            git(&source, &["add", "."]);
            git(&source, &["rm", "--quiet", "deleted"]);
            fs::write(source.join("partial"), b"WORKTREE").unwrap();
            fs::remove_file(source.join("only")).unwrap();
            fs::write(source.join("intent"), b"not staged").unwrap();
            git(&source, &["add", "-N", "intent"]);
            let before = fs::read(source.join(".git/index")).unwrap();
            let artifact = fixture.0.join("artifact");
            let (descriptor, _) = capture(&source, &artifact, 4 * 1024 * 1024, 1024 * 1024)
                .await
                .unwrap();
            assert_eq!(fs::read(source.join(".git/index")).unwrap(), before);
            assert_eq!(git(&source, &["show", "HEAD:partial"]), b"HEAD");
            assert_eq!(fs::read(source.join("partial")).unwrap(), b"WORKTREE");
            let destination = fixture.repo("destination", sha256);
            git(
                &destination,
                &["fetch", "--quiet", source.to_str().unwrap(), "HEAD"],
            );
            let head = git(&source, &["rev-parse", "HEAD"]);
            git(
                &destination,
                &[
                    "update-ref",
                    "HEAD",
                    std::str::from_utf8(&head).unwrap().trim(),
                ],
            );
            fs::write(destination.join("partial"), b"WORKTREE").unwrap();
            fs::write(destination.join("intent"), b"not staged").unwrap();
            receive_fresh(&destination, &artifact, &descriptor).await;
            assert_eq!(git(&destination, &["show", "HEAD:partial"]), b"HEAD");
            assert_eq!(git(&destination, &["show", ":partial"]), b"INDEX");
            assert_eq!(fs::read(destination.join("partial")).unwrap(), b"WORKTREE");
            assert_eq!(git(&destination, &["show", ":only"]), b"staged only");
            assert!(!destination.join("only").exists());
            assert_eq!(git(&destination, &["show", ":binary"]), [0, 255, 128, 0]);
            let index = load(&artifact, &descriptor).await.unwrap();
            assert!(index
                .entries
                .iter()
                .any(|entry| entry.path == "intent" && entry.intent));
            assert!(index.entries.iter().all(|entry| entry.path != "deleted"));
            #[cfg(unix)]
            {
                assert!(index
                    .entries
                    .iter()
                    .any(|entry| entry.path == "exec" && entry.mode == 0o100755));
                assert!(index
                    .entries
                    .iter()
                    .any(|entry| entry.path == "link" && entry.mode == 0o120000));
            }
            assert_eq!(
                parse(
                    &fs::read(destination.join(".git/index")).unwrap(),
                    index.format
                )
                .unwrap(),
                index
            );
            assert!(!git(&destination, &["diff", "--cached", "--name-only"])
                .split(|b| *b == b'\n')
                .any(|path| path == b"intent"));
        }
    }
    #[tokio::test]
    async fn split_linked_index_and_unmerged_entries_preserve_source() {
        let fixture = Fixture::new();
        let main = fixture.repo("main", false);
        fs::write(main.join("file"), b"base").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-qm", "base"]);
        let linked = fixture.0.join("linked");
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "linked",
                linked.to_str().unwrap(),
            ],
        );
        fs::write(linked.join("file"), b"staged").unwrap();
        git(&linked, &["add", "file"]);
        git(&linked, &["update-index", "--split-index"]);
        let bytes = git(
            &linked,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        );
        let path = PathBuf::from(std::str::from_utf8(&bytes).unwrap().trim());
        let before = fs::read(&path).unwrap();
        let artifact = fixture.0.join("split");
        let (descriptor, _) = capture(&linked, &artifact, 1024 * 1024, 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
        let destination = fixture.repo("destination", false);
        receive_fresh(&destination, &artifact, &descriptor).await;
        assert_eq!(git(&destination, &["show", ":file"]), b"staged");
        let oid = git(&linked, &["rev-parse", ":file"]);
        let oid = std::str::from_utf8(&oid).unwrap().trim().to_string();
        let index = Index {
            version: 1,
            format: Format::Sha1,
            entries: (1..=3)
                .map(|stage| Entry {
                    path: "conflict".into(),
                    mode: 0o100644,
                    stage,
                    oid: oid.clone(),
                    intent: false,
                })
                .collect(),
        };
        fs::write(&path, encode(&index).unwrap()).unwrap();
        let artifact = fixture.0.join("unmerged");
        let (descriptor, _) = capture(&linked, &artifact, 1024 * 1024, 1024 * 1024)
            .await
            .unwrap();
        receive_fresh(&destination, &artifact, &descriptor).await;
        assert_eq!(
            git(&destination, &["ls-files", "--unmerged"])
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count(),
            3
        );
        assert_eq!(load(&artifact, &descriptor).await.unwrap(), index);
    }
    #[tokio::test]
    async fn immutable_baseline_merges_independent_entries_and_keeps_both_conflicting_indexes() {
        let fixture = Fixture::new();
        let source = fixture.repo("source", false);
        for path in ["ours", "theirs", "conflict"] {
            fs::write(source.join(path), b"base").unwrap();
        }
        git(&source, &["add", "."]);
        let baseline = fixture.0.join("baseline");
        let (base_descriptor, _) = capture(&source, &baseline, 1024 * 1024, 1024 * 1024)
            .await
            .unwrap();
        let destination = fixture.repo("destination", false);
        receive_fresh(&destination, &baseline, &base_descriptor).await;
        fs::write(destination.join("ours"), b"local only").unwrap();
        fs::write(destination.join("conflict"), b"local conflict").unwrap();
        git(&destination, &["add", "ours", "conflict"]);
        let local = local_index(&destination, &fixture.0.join("local.index"))
            .await
            .unwrap();
        fs::write(source.join("theirs"), b"incoming only").unwrap();
        fs::write(source.join("conflict"), b"incoming conflict").unwrap();
        git(&source, &["add", "theirs", "conflict"]);
        let artifact = fixture.0.join("incoming");
        let (descriptor, _) = capture(&source, &artifact, 1024 * 1024, 1024 * 1024)
            .await
            .unwrap();
        let status = receive(
            &destination,
            Some(&local),
            Receiving {
                handoff: &artifact,
                descriptor: &descriptor,
                baseline: Some((&baseline, &base_descriptor)),
                budget: 1024 * 1024,
                max_file: 1024 * 1024,
            },
            &|| Ok(()),
        )
        .await
        .unwrap();
        assert!(
            matches!(status, super::super::StagingStatus::Conflicts {paths,..} if paths == ["conflict"])
        );
        assert_eq!(git(&destination, &["show", ":ours"]), b"local only");
        assert_eq!(git(&destination, &["show", ":theirs"]), b"incoming only");
        assert_eq!(git(&destination, &["show", ":conflict"]), b"local conflict");
        let recovery = fs::read_dir(destination.join(".git/chimaera-staging"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            parse(
                &fs::read(recovery.join("local.index")).unwrap(),
                Format::Sha1
            )
            .unwrap(),
            local
        );
        assert_eq!(
            parse(
                &fs::read(recovery.join("incoming.index")).unwrap(),
                Format::Sha1
            )
            .unwrap(),
            load(&artifact, &descriptor).await.unwrap()
        );
        // The incoming conflict blob is absent from the installed index and all
        // ordinary commit trees. The recovery ref must preserve it across GC.
        git(&destination, &["gc", "--prune=now"]);
        let saved = parse(
            &fs::read(recovery.join("incoming.index")).unwrap(),
            Format::Sha1,
        )
        .unwrap();
        for entry in saved
            .entries
            .iter()
            .chain(&local.entries)
            .filter(|entry| entry.mode != 0o160000)
        {
            assert!(!git(&destination, &["cat-file", "blob", &entry.oid]).is_empty());
        }
        let reference = fs::read_to_string(recovery.join("objects.ref")).unwrap();
        assert!(reference.starts_with("refs/chimaera/staging/"));
        assert!(!git(&destination, &["ls-tree", reference.trim()]).is_empty());
    }
    #[tokio::test]
    async fn credential_paths_blobs_quota_tampering_and_stale_admission_fail_closed() {
        for (path, bytes, budget, max_file) in [
            (".env", b"ordinary".as_slice(), 1024 * 1024, 1024 * 1024),
            (
                "file",
                b"-----BEGIN PRIVATE KEY-----".as_slice(),
                1024 * 1024,
                1024 * 1024,
            ),
            ("file", b"ordinary".as_slice(), 1, 1024 * 1024),
            ("file", b"ordinary".as_slice(), 1024 * 1024, 2),
        ] {
            let fixture = Fixture::new();
            let source = fixture.repo("source", false);
            fs::write(source.join(path), bytes).unwrap();
            git(&source, &["add", "."]);
            let before = fs::read(source.join(".git/index")).unwrap();
            let artifact = fixture.0.join("artifact");
            assert!(capture(&source, &artifact, budget, max_file).await.is_err());
            assert_eq!(fs::read(source.join(".git/index")).unwrap(), before);
            assert!(!artifact.join(INDEX_PATH).exists());
        }
        let fixture = Fixture::new();
        let source = fixture.repo("source", false);
        fs::write(source.join("file"), b"stage").unwrap();
        git(&source, &["add", "."]);
        let artifact = fixture.0.join("artifact");
        let (descriptor, _) = capture(&source, &artifact, 1024 * 1024, 1024 * 1024)
            .await
            .unwrap();
        let destination = fixture.repo("destination", false);
        fs::write(destination.join("local"), b"local").unwrap();
        git(&destination, &["add", "."]);
        let before = fs::read(destination.join(".git/index")).unwrap();
        let transfer = || Receiving {
            handoff: &artifact,
            descriptor: &descriptor,
            baseline: None,
            budget: 1024 * 1024,
            max_file: 1024 * 1024,
        };
        assert!(
            receive(&destination, None, transfer(), &|| bail!("account changed"))
                .await
                .is_err()
        );
        assert_eq!(fs::read(destination.join(".git/index")).unwrap(), before);
        let index = load(&artifact, &descriptor).await.unwrap();
        fs::write(
            artifact.join("git/blobs").join(&index.entries[0].oid),
            b"tampered",
        )
        .unwrap();
        assert!(receive(&destination, None, transfer(), &|| Ok(()))
            .await
            .is_err());
        assert_eq!(fs::read(destination.join(".git/index")).unwrap(), before);
    }
    #[test]
    fn checksum_extensions_hostile_paths_sparse_and_duplicates_are_checked() {
        let index = Index {
            version: 1,
            format: Format::Sha1,
            entries: vec![Entry {
                path: "file".into(),
                mode: 0o100644,
                stage: 0,
                oid: "a".repeat(40),
                intent: false,
            }],
        };
        let bytes = encode(&index).unwrap();
        assert_eq!(parse(&bytes, Format::Sha1).unwrap(), index);
        let mut corrupt = bytes.clone();
        corrupt[15] ^= 1;
        assert!(parse(&corrupt, Format::Sha1).is_err());
        for (signature, accepted) in [
            (b"link", false),
            (b"sdir", false),
            (b"xxxx", false),
            (b"TREE", true),
            (b"REUC", true),
            (b"UNTR", true),
            (b"FSMN", true),
            (b"EOIE", true),
            (b"IEOT", true),
        ] {
            let mut extended = bytes[..bytes.len() - 20].to_vec();
            extended.extend_from_slice(signature);
            extended.extend_from_slice(&0u32.to_be_bytes());
            extended.extend_from_slice(&Format::Sha1.hash(&extended));
            let parsed = parse(&extended, Format::Sha1);
            assert_eq!(parsed.is_ok(), accepted);
            if let Ok(parsed) = parsed {
                assert_eq!(encode(&parsed).unwrap(), bytes);
            }
        }
        for path in [
            "../escape",
            "/escape",
            ".git/file",
            "a//b",
            "a/./b",
            "a/../b",
            "a\\b",
            ".ssh/key",
            "file\0rest",
        ] {
            let mut hostile = index.clone();
            hostile.entries[0].path = path.into();
            assert!(hostile.validate().is_err());
        }
        let mut sparse = index.clone();
        sparse.entries[0].mode = 0o040000;
        assert!(sparse.validate().is_err());
        let mut duplicate = index.clone();
        duplicate.entries.push(duplicate.entries[0].clone());
        assert!(duplicate.validate().is_err());
        let mut unknown = Descriptor::new();
        unknown.index = "../index.json".into();
        assert!(unknown.validate().is_err());
    }
}
