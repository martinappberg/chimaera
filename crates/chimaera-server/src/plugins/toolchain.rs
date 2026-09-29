//! Side programs a plugin installs (`[[tools]]`, docs/plugin-platform-plan.md
//! §8): declared downloads the host fetches on the user's click, never
//! install scripts.
//!
//! - **The download** is the artifact the manifest names for this host,
//!   over HTTPS only, from its fixed URL: its sha256 is checked while it
//!   streams, and it stops past its declared size (never more than 2 GiB).
//!   The manifest pins the sha256 and the lock pins the manifest, so a
//!   verified plugin's downloads are verified by the same review.
//! - **The unpacker is trusted core** (a bad archive is how a download path
//!   becomes an escape). It extracts into a fresh, empty folder, creating
//!   every entry relative to that folder's descriptor and never following a
//!   link on the way; refuses absolute paths, `..`, hard links, devices and
//!   FIFOs; allows a symbolic link only when it is relative and stays inside
//!   the folder, and never writes through one (`O_EXCL | O_NOFOLLOW`); and
//!   stops at 4 GiB unpacked or 200,000 entries.
//! - **Nothing outside the folder changes**: no PATH edit, no rc file. A
//!   tool's `bin` joins the PATH of that plugin's jobs only (`jobs`).
//! - **Layout**: `<data dir>/tools/<plugin>/<tool>/<version>/` behind an
//!   atomic `current` link; two versions kept. `setup` steps run once after
//!   unpacking, as jobs of the tool's own programs, before `current` moves.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rustix::fs::{FileType, Mode, OFlags};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use super::platform::{ArtifactDecl, ToolDecl};
use super::Manifest;
use crate::AppState;

/// An unpacked tool's ceiling: bytes and entries.
pub(crate) const UNPACKED_MAX: u64 = 4 << 30;
pub(crate) const ENTRIES_MAX: usize = 200_000;
const VERSIONS_KEPT: usize = 2;
const DOWNLOAD_TIMEOUT_S: u64 = 3600;
/// The small record beside an unpacked tool.
const RECORD: &str = ".chimaera-tool.json";

/// (plugin, tool) pairs installing now: one at a time each, and how far
/// each has got (the card and a screen show it while it runs).
#[derive(Default)]
pub(crate) struct Installs {
    busy: Mutex<HashSet<(String, String)>>,
    progress: Mutex<HashMap<(String, String), Progress>>,
}

/// An install's stage (`downloading`, `unpacking`, `setting up`) and, while
/// downloading, the bytes so far of the declared size.
#[derive(Clone, Copy)]
struct Progress {
    stage: &'static str,
    done: u64,
    total: u64,
}

impl Installs {
    fn set(&self, key: &(String, String), stage: &'static str, done: u64, total: u64) {
        crate::lock(&self.progress).insert(key.clone(), Progress { stage, done, total });
    }
}

/// Tests only: `https://example.invalid/…` downloads go to a local server.
#[cfg(test)]
static DOWNLOAD_OVERRIDE: Mutex<Option<(String, String)>> = Mutex::new(None);

#[cfg(test)]
pub(crate) fn set_downloads_for_tests(from: &str, to: &str) {
    *crate::lock(&DOWNLOAD_OVERRIDE) = Some((from.to_string(), to.to_string()));
}

fn download_url(url: &str) -> (String, bool) {
    #[cfg(test)]
    if let Some((from, to)) = crate::lock(&DOWNLOAD_OVERRIDE).clone() {
        if let Some(rest) = url.strip_prefix(&from) {
            return (format!("{to}{rest}"), true);
        }
    }
    (url.to_string(), false)
}

/// The host setting that moves plugins' tools out of the data dir: an HPC
/// home is often a small quota (a TeX Live is ~500 MB), `$SCRATCH` or a
/// group folder is not.
const TOOLS_DIR_SETTING: &str = "plugins.toolsDir";
/// What an install leaves free on the tools' filesystem, at least.
const FREE_AFTER_MIN: u64 = 1 << 30;

/// Where plugins' tools live: the `plugins.toolsDir` setting when it names
/// a folder (`tools_dir_setting`), else `<data dir>/tools`. Tools already
/// installed elsewhere stay there, unused, until installed again.
pub(crate) fn tools_root(state: &AppState) -> PathBuf {
    match tools_dir_setting(state) {
        Some(Ok(dir)) => dir,
        _ => {
            let plugins = &state.plugin_catalog.root;
            plugins.parent().unwrap_or(plugins).join("tools")
        }
    }
}

/// The `plugins.toolsDir` setting: None when unset; else the folder (`~` and
/// `$NAME` / `${NAME}` from the daemon's environment), or why it isn't one.
fn tools_dir_setting(state: &AppState) -> Option<Result<PathBuf, String>> {
    let raw = crate::lock(&state.settings)
        .map_cached()
        .get(TOOLS_DIR_SETTING)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)?;
    Some(expand_dir(&raw, |name| std::env::var(name).ok()))
}

/// `raw` as an absolute folder: a leading `~`, and `$NAME` or `${NAME}`
/// anywhere, expanded with `var`.
fn expand_dir(raw: &str, var: impl Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
    let mut out = String::new();
    let mut rest = raw;
    if rest == "~" || rest.starts_with("~/") {
        out.push_str(&var("HOME").ok_or("HOME isn't set, so ~ can't be expanded")?);
        rest = &rest[1..];
    }
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        let (name, after) = match rest.strip_prefix('{') {
            Some(inner) => {
                let end = inner.find('}').ok_or("a `${` without its `}`")?;
                (&inner[..end], &inner[end + 1..])
            }
            None => {
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                (&rest[..end], &rest[end..])
            }
        };
        if name.is_empty() {
            return Err("a `$` without a variable name".into());
        }
        out.push_str(&var(name).ok_or_else(|| {
            format!("${name} isn't set in the daemon's environment; write the folder out")
        })?);
        rest = after;
    }
    out.push_str(rest);
    let dir = PathBuf::from(out);
    if !dir.is_absolute() {
        return Err(format!("{} isn't an absolute folder", dir.display()));
    }
    Ok(dir)
}

/// Bytes free to this user on the filesystem holding `dir` (blocking).
fn free_bytes(dir: &Path) -> Option<u64> {
    let st = rustix::fs::statvfs(dir).ok()?;
    Some(st.f_bavail.saturating_mul(st.f_frsize))
}

fn tool_dir(state: &AppState, plugin: &str, tool: &str) -> PathBuf {
    tools_root(state).join(plugin).join(tool)
}

/// What the record beside an unpacked version says.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Record {
    version: String,
    platform: String,
    sha256: String,
    bytes: u64,
    bin: Option<String>,
    installed_ms: u64,
}

fn read_record(version_dir: &Path) -> Option<Record> {
    let path = version_dir.join(RECORD);
    // The tool's own setup programs can write in its folder: a record that
    // became a link, or names a `bin` outside, isn't one this host wrote.
    if !std::fs::symlink_metadata(&path).ok()?.is_file() {
        return None;
    }
    // Capped as it is read: the tool's own setup could have grown it.
    let mut text = Vec::new();
    File::open(&path)
        .ok()?
        .take((64 << 10) + 1)
        .read_to_end(&mut text)
        .ok()?;
    if text.len() > 64 << 10 {
        return None;
    }
    let record: Record = serde_json::from_slice(&text).ok()?;
    if let Some(bin) = &record.bin {
        if !Path::new(bin)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            return None;
        }
    }
    Some(record)
}

/// The installed version's folder and record (blocking).
fn current(dir: &Path) -> Option<(PathBuf, Record)> {
    let target = std::fs::read_link(dir.join("current")).ok()?;
    let name = target.file_name()?.to_str()?.to_string();
    if name.starts_with('.') || name.contains('/') {
        return None;
    }
    let version_dir = dir.join(name);
    let record = read_record(&version_dir)?;
    Some((version_dir, record))
}

/// The folder a plugin's installed tool keeps its programs in.
pub(crate) async fn installed_bin(state: &AppState, plugin: &str, tool: &str) -> Option<PathBuf> {
    let dir = tool_dir(state, plugin, tool);
    tokio::task::spawn_blocking(move || {
        let (version_dir, record) = current(&dir)?;
        Some(match &record.bin {
            Some(bin) => version_dir.join(bin),
            None => version_dir,
        })
    })
    .await
    .ok()
    .flatten()
}

/// `tool-state` and the card's Tools section: declared, installed (which
/// version, how big, when), and what this host could download.
pub(crate) async fn tool_json(state: &AppState, m: &Manifest, tool: &ToolDecl) -> Value {
    let dir = tool_dir(state, &m.id, &tool.id);
    let installed = tokio::task::spawn_blocking(move || current(&dir))
        .await
        .ok()
        .flatten();
    let key = (m.id.clone(), tool.id.clone());
    let busy = crate::lock(&state.plugin_platform.installs.busy).contains(&key);
    let progress = crate::lock(&state.plugin_platform.installs.progress)
        .get(&key)
        .copied();
    json!({
        "tool": tool.id,
        "declared": true,
        "name": tool.name,
        "version": tool.version,
        "programs": tool.programs,
        "installing": busy,
        "progress": progress.map(|p| json!({"stage": p.stage, "done": p.done, "total": p.total})),
        "installed": installed.as_ref().map(|(_, r)| json!({
            "version": r.version,
            "bytes": r.bytes,
            "installed_ms": r.installed_ms,
        })),
        "current": installed.as_ref().is_some_and(|(_, r)| r.version == tool.version),
        "download": tool.artifact().map(|a| json!({
            "host": super::platform::https_host(&a.url),
            "size": a.size,
        })),
    })
}

// --- the unpacker -------------------------------------------------------------

/// A path inside an archive as plain components: absolute, `..`, and empty
/// paths refused.
fn entry_path(raw: &[u8]) -> Result<PathBuf, String> {
    let text = std::str::from_utf8(raw).map_err(|_| "an entry's name is not UTF-8".to_string())?;
    if text.starts_with('/') {
        return Err(format!("{text}: an absolute path"));
    }
    let mut out = PathBuf::new();
    for part in Path::new(text).components() {
        match part {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            Component::ParentDir => return Err(format!("{text}: `..` leaves the folder")),
            _ => return Err(format!("{text}: not a relative path")),
        }
    }
    if out.as_os_str().is_empty() {
        return Err("an entry with no name".into());
    }
    Ok(out)
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// The folder `rel` names beneath `root`, each component made if missing,
/// none ever a link.
fn dir_beneath(root: &File, rel: &Path) -> Result<File, String> {
    let mut dir = root.try_clone().map_err(|e| e.to_string())?;
    for part in rel.components() {
        let Component::Normal(name) = part else {
            return Err(format!("{}: not a plain path", rel.display()));
        };
        match rustix::fs::mkdirat(&dir, name, Mode::from_raw_mode(0o755)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(format!("{}: {e}", rel.display())),
        }
        dir = File::from(
            rustix::fs::openat(&dir, name, dir_flags(), Mode::empty())
                .map_err(|e| format!("{}: {e} (a link where a folder must be)", rel.display()))?,
        );
    }
    Ok(dir)
}

/// The parent folder of `rel` beneath `root`, and its last name.
fn parent_beneath(root: &File, rel: &Path) -> Result<(File, std::ffi::OsString), String> {
    let name = rel
        .file_name()
        .ok_or_else(|| format!("{}: no name", rel.display()))?
        .to_os_string();
    let parent = rel.parent().unwrap_or(Path::new(""));
    Ok((dir_beneath(root, parent)?, name))
}

/// Whether link `target`, placed at `at` (relative to the folder), stays
/// inside the folder, read lexically.
fn link_stays_inside(at: &Path, target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') {
        return false;
    }
    let mut depth: i64 = at.parent().map_or(0, |p| p.components().count() as i64);
    for part in Path::new(target).components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Counts what has been unpacked against the limits.
struct Budget {
    bytes: u64,
    entries: usize,
    max_bytes: u64,
    max_entries: usize,
    /// Every link made, checked once the whole tree is there.
    links: Vec<PathBuf>,
}

impl Budget {
    fn entry(&mut self) -> Result<(), String> {
        self.entries += 1;
        if self.entries > self.max_entries {
            return Err(format!("more than {} entries", self.max_entries));
        }
        Ok(())
    }
}

/// Write one regular file (never over an entry already there, never
/// through a link) with `mode`'s execute bits.
fn write_entry(
    root: &File,
    rel: &Path,
    mode: u32,
    from: &mut dyn Read,
    budget: &mut Budget,
) -> Result<(), String> {
    let (parent, name) = parent_beneath(root, rel)?;
    let perms = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
    let fd = rustix::fs::openat(
        &parent,
        name.as_os_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(perms),
    )
    .map_err(|e| format!("{}: {e}", rel.display()))?;
    let mut file = File::from(fd);
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = from
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", rel.display()))?;
        if n == 0 {
            break;
        }
        budget.bytes += n as u64;
        if budget.bytes > budget.max_bytes {
            return Err(format!("more than {} MiB unpacked", budget.max_bytes >> 20));
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", rel.display()))?;
    }
    Ok(())
}

fn link_entry(root: &File, rel: &Path, target: &str, budget: &mut Budget) -> Result<(), String> {
    if !link_stays_inside(rel, target) {
        return Err(format!(
            "{}: a link to {target:?}, which leaves the folder",
            rel.display()
        ));
    }
    let (parent, name) = parent_beneath(root, rel)?;
    rustix::fs::symlinkat(target, &parent, name.as_os_str())
        .map_err(|e| format!("{}: {e}", rel.display()))?;
    budget.links.push(rel.to_path_buf());
    Ok(())
}

/// Whether the link at `rel` resolves inside `root` once every link on the
/// way is followed (blocking; the tree is the finished, private unpack). The
/// lexical check at creation can't see a chain: `d/e -> ..` is inside, and
/// so, read alone, is `f -> d/e/../..`, which lands above the folder. A
/// missing name is walked as written; more than 40 hops is refused.
fn resolves_inside(root: &Path, rel: &Path) -> bool {
    use std::collections::VecDeque;
    use std::ffi::OsString;
    let mut at: Vec<OsString> = Vec::new();
    let mut pending: VecDeque<OsString> = rel
        .components()
        .map(|c| c.as_os_str().to_os_string())
        .collect();
    let mut hops = 0;
    while let Some(part) = pending.pop_front() {
        if part == ".." {
            if at.pop().is_none() {
                return false;
            }
            continue;
        }
        if part == "." || part.is_empty() {
            continue;
        }
        at.push(part);
        let here: PathBuf = at.iter().fold(root.to_path_buf(), |p, c| p.join(c));
        let Ok(meta) = std::fs::symlink_metadata(&here) else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        hops += 1;
        if hops > 40 {
            return false;
        }
        let Ok(target) = std::fs::read_link(&here) else {
            return false;
        };
        if target.is_absolute() {
            return false;
        }
        at.pop();
        for c in target.components().rev() {
            pending.push_front(c.as_os_str().to_os_string());
        }
    }
    true
}

fn unpack_tar(reader: impl Read, root: &File, budget: &mut Budget) -> Result<(), String> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|e| format!("not a tar archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("a broken entry: {e}"))?;
        budget.entry()?;
        let rel = entry_path(&entry.path_bytes())?;
        let kind = entry.header().entry_type();
        match kind {
            tar::EntryType::Directory => {
                dir_beneath(root, &rel)?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mode = entry.header().mode().unwrap_or(0o644);
                write_entry(root, &rel, mode, &mut entry, budget)?;
            }
            tar::EntryType::Symlink => {
                let target = entry
                    .link_name_bytes()
                    .map(|t| String::from_utf8_lossy(&t).into_owned())
                    .unwrap_or_default();
                link_entry(root, &rel, &target, budget)?;
            }
            tar::EntryType::Link => {
                return Err(format!("{}: a hard link (refused)", rel.display()));
            }
            tar::EntryType::XGlobalHeader | tar::EntryType::XHeader => {}
            other => {
                return Err(format!(
                    "{}: a {other:?} entry (only files, folders and links inside are unpacked)",
                    rel.display()
                ));
            }
        }
    }
    Ok(())
}

fn unpack_zip(file: File, root: &File, budget: &mut Budget) -> Result<(), String> {
    let mut zip =
        zip::ZipArchive::new(BufReader::new(file)).map_err(|e| format!("not a zip: {e}"))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("a broken entry: {e}"))?;
        budget.entry()?;
        let name = entry.name_raw().to_vec();
        let rel = entry_path(&name)?;
        let mode = entry.unix_mode().unwrap_or(0o644);
        // zip keeps 16 bits of Unix mode; rustix's `RawMode` is u16 on
        // macOS and u32 on Linux.
        let ftype = FileType::from_raw_mode(mode as rustix::fs::RawMode);
        if entry.is_dir() {
            dir_beneath(root, &rel)?;
        } else if ftype == FileType::Symlink {
            let mut target = String::new();
            (&mut entry)
                .take(4096)
                .read_to_string(&mut target)
                .map_err(|e| e.to_string())?;
            link_entry(root, &rel, &target, budget)?;
        } else if ftype == FileType::RegularFile
            || ftype == FileType::Unknown
            || mode & 0o170000 == 0
        {
            write_entry(root, &rel, mode, &mut entry, budget)?;
        } else {
            return Err(format!("{}: not a file, folder or link", rel.display()));
        }
    }
    Ok(())
}

/// Unpack `archive` (`kind`: tar, tar.gz, tar.xz, zip, none) into `dest`,
/// which must not exist yet (blocking). Its bytes unpacked.
pub(crate) fn unpack(
    archive: &Path,
    kind: &str,
    dest: &Path,
    name: &str,
    max_bytes: u64,
    max_entries: usize,
) -> Result<u64, String> {
    std::fs::create_dir(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let root = File::open(dest).map_err(|e| e.to_string())?;
    let mut budget = Budget {
        bytes: 0,
        entries: 0,
        max_bytes,
        max_entries,
        links: Vec::new(),
    };
    let file = File::open(archive).map_err(|e| e.to_string())?;
    match kind {
        "tar" => unpack_tar(BufReader::new(file), &root, &mut budget)?,
        "tar.gz" => unpack_tar(
            flate2::read::MultiGzDecoder::new(BufReader::new(file)),
            &root,
            &mut budget,
        )?,
        "tar.xz" => {
            // The host's `xz` streams the tar into our own unpacker (a
            // login node always has one); only where there is none does
            // lzma-rs decode it, and lzma-rs holds a whole xz block in
            // memory (TinyTeX's is one 410 MiB block), so only small ones.
            if let Some(result) = unpack_tar_xz_streamed(archive, &root, &mut budget) {
                result?;
                return finish_links(dest, budget);
            }
            let size = std::fs::metadata(archive).map_err(|e| e.to_string())?.len();
            if size > XZ_IN_MEMORY_MAX {
                return Err(format!(
                    "a .tar.xz of {} MiB needs the xz program on this host to unpack",
                    size >> 20
                ));
            }
            // lzma-rs decodes to a writer: a capped temporary tar beside the
            // archive, then read as a tar.
            let tar_path = archive.with_extension("tar.tmp");
            {
                let out = File::create(&tar_path).map_err(|e| e.to_string())?;
                let mut capped = CappedWriter {
                    inner: std::io::BufWriter::new(out),
                    left: max_bytes + (max_bytes / 8) + (1 << 20),
                };
                let decoded = lzma_rs::xz_decompress(&mut BufReader::new(file), &mut capped);
                if let Err(err) = decoded {
                    let _ = std::fs::remove_file(&tar_path);
                    return Err(format!("not an xz archive (or over the cap): {err}"));
                }
                capped.inner.flush().map_err(|e| e.to_string())?;
            }
            let result = File::open(&tar_path)
                .map_err(|e| e.to_string())
                .and_then(|t| unpack_tar(BufReader::new(t), &root, &mut budget));
            let _ = std::fs::remove_file(&tar_path);
            result?;
        }
        "zip" => unpack_zip(file, &root, &mut budget)?,
        "none" => {
            budget.entry()?;
            let rel = entry_path(name.as_bytes())?;
            write_entry(&root, &rel, 0o755, &mut BufReader::new(file), &mut budget)?;
        }
        other => return Err(format!("unpack {other:?} is not supported")),
    }
    finish_links(dest, budget)
}

/// The links, checked once the whole tree is there; the bytes unpacked.
fn finish_links(dest: &Path, budget: Budget) -> Result<u64, String> {
    for link in &budget.links {
        if !resolves_inside(dest, link) {
            return Err(format!(
                "{}: a link that leaves the folder through another link",
                link.display()
            ));
        }
    }
    Ok(budget.bytes)
}

/// A `.tar.xz` lzma-rs may decode (it holds a whole xz block in memory).
const XZ_IN_MEMORY_MAX: u64 = 32 << 20;

/// What may follow a tar's end inside its xz stream (its zero padding is a
/// few KiB) before the rest is left unread.
const XZ_TAIL_MAX: u64 = 64 << 20;

/// Unpack `archive` through the host's `xz -dc`, streamed (blocking); None
/// when this host has no `xz`. xz only decompresses: the entries are read
/// by `unpack_tar`, with every check it makes. A tar reader stops quietly at
/// an early end, so xz must also end well — else a stream cut short (xz
/// killed, out of memory) would install half a tool.
fn unpack_tar_xz_streamed(
    archive: &Path,
    root: &File,
    budget: &mut Budget,
) -> Option<Result<(), String>> {
    let mut child = std::process::Command::new("xz")
        .args(["-dc", "--"])
        .arg(archive)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut out = BufReader::new(child.stdout.take()?);
    let result = unpack_tar(&mut out, root, budget).and_then(|()| {
        let tail = std::io::copy(&mut (&mut out).take(XZ_TAIL_MAX + 1), &mut std::io::sink())
            .map_err(|e| format!("reading from xz: {e}"))?;
        if tail > XZ_TAIL_MAX {
            return Err("more than the tar inside the .tar.xz".into());
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("xz could not decompress it ({status})"))
        }
    });
    // A refusal leaves xz with more to write: stop it and reap it.
    let _ = child.kill();
    let _ = child.wait();
    Some(result)
}

struct CappedWriter<W: Write> {
    inner: W,
    left: u64,
}

impl<W: Write> Write for CappedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.left {
            return Err(std::io::Error::other("over the unpacked cap"));
        }
        self.left -= buf.len() as u64;
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// --- the download -------------------------------------------------------------

/// Fetch `a` into `dest`, hashing as it streams; refuse a size or sha256
/// that isn't the declared one.
async fn download(
    a: &ArtifactDecl,
    dest: &Path,
    on_bytes: &(dyn Fn(u64) + Send + Sync),
) -> Result<u64, String> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;
    let (url, local) = download_url(&a.url);
    let cap = a.size.min(super::platform::DOWNLOAD_MAX);
    let mut cmd = tokio::process::Command::new("curl");
    cmd.args(["-fsSL", "-S", "-m", &DOWNLOAD_TIMEOUT_S.to_string()])
        .args(["--max-filesize", &cap.to_string()]);
    if !local {
        cmd.args(["--proto", "=https", "--proto-redir", "=https"]);
    }
    cmd.args([
        "-H",
        concat!("User-Agent: chimaera/", env!("CARGO_PKG_VERSION")),
        &url,
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("curl did not start: {e}"))?;
    let mut out = child.stdout.take().ok_or("curl has no stdout")?;
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 << 10];
    let mut total: u64 = 0;
    loop {
        let n = out.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        on_bytes(total);
        if total > cap {
            return Err(format!(
                "the download is larger than the {} bytes its plugin declared",
                a.size
            ));
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).await.map_err(|e| e.to_string())?;
    }
    file.sync_all().await.map_err(|e| e.to_string())?;
    let output = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "the download failed: {}",
            stderr.lines().last().unwrap_or("curl failed").trim()
        ));
    }
    let got: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !got.eq_ignore_ascii_case(&a.sha256) {
        tracing::warn!(url = %a.url, expected = %a.sha256, %got, "plugin tool download refused: sha256 mismatch");
        return Err(
            "the download is not the file its plugin names (its checksum differs), so it wasn't \
             installed"
                .into(),
        );
    }
    Ok(total)
}

/// Point `current` at `version` atomically, and keep only the newest two
/// versions (blocking).
fn activate(dir: &Path, version: &str) -> Result<(), String> {
    let tmp = dir.join(format!(
        ".current-{}",
        &chimaera_core::generate_token()[..8]
    ));
    std::os::unix::fs::symlink(version, &tmp).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("current")).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })?;
    let mut versions: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && name != "current" && name != version
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    versions.sort_by_key(|v| std::cmp::Reverse(v.0));
    for (_, old) in versions.into_iter().skip(VERSIONS_KEPT - 1) {
        let _ = std::fs::remove_dir_all(old);
    }
    Ok(())
}

/// Install `tool` of `m`: download, check, unpack, set up, activate.
pub(crate) async fn install(
    state: &Arc<AppState>,
    m: &Arc<Manifest>,
    tool: &ToolDecl,
) -> Result<Value, String> {
    let Some(artifact) = tool.artifact() else {
        return Err(format!(
            "{} has no download for this host ({})",
            tool.name,
            super::platform::this_platform().unwrap_or("an unknown platform")
        ));
    };
    let key = (m.id.clone(), tool.id.clone());
    if !crate::lock(&state.plugin_platform.installs.busy).insert(key.clone()) {
        return Err(format!("{} is already installing", tool.name));
    }
    let result = install_inner(state, m, tool, artifact).await;
    crate::lock(&state.plugin_platform.installs.busy).remove(&key);
    crate::lock(&state.plugin_platform.installs.progress).remove(&key);
    // What was fetched, from where, and the digest it was held to.
    let mut entry = json!({
        "kind": "tool-install",
        "tool": tool.id,
        "version": tool.version,
        "url": artifact.url,
        "sha256": artifact.sha256,
        "size": artifact.size,
    });
    if let Err(err) = &result {
        entry["kind"] = json!("tool-install-failed");
        entry["error"] = json!(err);
    }
    super::activity::record(state, &m.id, entry).await;
    result
}

async fn install_inner(
    state: &Arc<AppState>,
    m: &Arc<Manifest>,
    tool: &ToolDecl,
    artifact: &ArtifactDecl,
) -> Result<Value, String> {
    if let Some(Err(why)) = tools_dir_setting(state) {
        return Err(format!(
            "the Plugin Tools Folder setting ({TOOLS_DIR_SETTING}): {why}"
        ));
    }
    let dir = tool_dir(state, &m.id, &tool.id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    // Room for the download and what it unpacks to (an archive is some
    // times its size), with a margin left: a small home quota fills before
    // the install ends, and a full home breaks everything else too.
    let need = artifact
        .size
        .saturating_mul(if artifact.unpack == "none" { 1 } else { 4 });
    let free = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || free_bytes(&dir))
            .await
            .ok()
            .flatten()
    };
    if let Some(free) = free {
        if free < need.saturating_add(FREE_AFTER_MIN) {
            let _ = tokio::fs::remove_dir(&dir).await;
            return Err(format!(
                "{} needs about {} MB where plugin tools go ({}), with 1 GB to spare, and \
                 {} MB is free there. Choose a folder with room in Settings → Extensions → \
                 Plugin Tools Folder (on a cluster, $SCRATCH or a group folder).",
                tool.name,
                need >> 20,
                tools_root(state).display(),
                free >> 20
            ));
        }
    }
    // An install that never finished left its files here (a 255 MB
    // download, a half-unpacked tree); this one holds the tool's slot.
    {
        let dir = dir.clone();
        let _ = tokio::task::spawn_blocking(move || sweep_leftovers(&dir)).await;
    }
    let nonce = &chimaera_core::generate_token()[..10];
    let archive = dir.join(format!(".download-{nonce}"));
    let staging = dir.join(format!(".unpack-{nonce}"));
    let cleanup = |paths: Vec<PathBuf>| async move {
        for p in paths {
            let _ = tokio::fs::remove_dir_all(&p).await;
            let _ = tokio::fs::remove_file(&p).await;
        }
    };
    tracing::info!(plugin = %m.id, tool = %tool.id, url = %artifact.url, "plugin tool download started");
    let key = (m.id.clone(), tool.id.clone());
    let installs = &state.plugin_platform.installs;
    installs.set(&key, "downloading", 0, artifact.size);
    let on_bytes = |done: u64| installs.set(&key, "downloading", done, artifact.size);
    let bytes = match download(artifact, &archive, &on_bytes).await {
        Ok(b) => b,
        Err(err) => {
            cleanup(vec![archive]).await;
            return Err(err);
        }
    };
    // `unpack = "none"`: the file is the program, so it takes the program's
    // name (a release names it `jq-linux-amd64`; jobs ask for `jq`).
    let name = tool.programs.first().unwrap_or(&tool.id).clone();
    installs.set(&key, "unpacking", 0, 0);
    let unpacked = {
        let (archive, staging, kind) = (archive.clone(), staging.clone(), artifact.unpack.clone());
        tokio::task::spawn_blocking(move || {
            unpack(&archive, &kind, &staging, &name, UNPACKED_MAX, ENTRIES_MAX)
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r)
    };
    cleanup(vec![archive]).await;
    let unpacked = match unpacked {
        Ok(n) => n,
        Err(err) => {
            cleanup(vec![staging]).await;
            return Err(format!("{} could not be unpacked: {err}", tool.name));
        }
    };
    let version_dir = dir.join(&tool.version);
    // A version there already (a reinstall) is set aside, then replaced.
    let aside = dir.join(format!(".old-{nonce}"));
    let _ = tokio::fs::rename(&version_dir, &aside).await;
    if let Err(err) = tokio::fs::rename(&staging, &version_dir).await {
        let _ = tokio::fs::rename(&aside, &version_dir).await;
        cleanup(vec![staging]).await;
        return Err(err.to_string());
    }
    let record = Record {
        version: tool.version.clone(),
        platform: artifact.platform.clone(),
        sha256: artifact.sha256.clone(),
        bytes: unpacked,
        bin: artifact.bin.clone(),
        installed_ms: crate::timeline::now_ms(),
    };
    let text = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
    // An archive entry of the record's name is replaced, never written
    // through (it could be a link).
    let record_path = version_dir.join(RECORD);
    let _ = tokio::fs::remove_file(&record_path).await;
    let written = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&record_path)
        .await;
    let written = match written {
        Ok(mut f) => {
            use tokio::io::AsyncWriteExt;
            f.write_all(&text).await
        }
        Err(e) => Err(e),
    };
    if let Err(err) = written {
        restore(&version_dir, &aside).await;
        return Err(format!("{}: {err}", tool.name));
    }
    let bin = match &artifact.bin {
        Some(b) => version_dir.join(b),
        None => version_dir.clone(),
    };
    if !tool.setup.is_empty() {
        installs.set(&key, "setting up", 0, 0);
    }
    for step in &tool.setup {
        if let Err(err) =
            super::jobs::run_setup(state, m, &bin, &version_dir, &step.program, &step.args).await
        {
            restore(&version_dir, &aside).await;
            return Err(format!(
                "{} was unpacked but its setup failed: {err}",
                tool.name
            ));
        }
    }
    cleanup(vec![aside]).await;
    {
        let (dir, version) = (dir.clone(), tool.version.clone());
        tokio::task::spawn_blocking(move || activate(&dir, &version))
            .await
            .map_err(|e| e.to_string())??;
    }
    tracing::info!(plugin = %m.id, tool = %tool.id, version = %tool.version, bytes, unpacked, "plugin tool installed");
    Ok(json!({"tool": tool.id, "version": tool.version, "downloaded": bytes, "bytes": unpacked}))
}

/// A failed install: the new copy goes and the one it replaced (a
/// reinstall's) comes back, so `current` never names a missing folder.
async fn restore(version_dir: &Path, aside: &Path) {
    let _ = tokio::fs::remove_dir_all(version_dir).await;
    let _ = tokio::fs::rename(aside, version_dir).await;
}

/// Whether `name` is what an install leaves in a tool's folder while it
/// runs: the download, the unpack, a reinstall's set-aside copy, the xz
/// fallback's temporary tar (`install_inner`'s names).
fn install_leftover(name: &str) -> bool {
    [".download-", ".unpack-", ".old-"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// Remove what an install that never finished (the daemon stopped, the
/// host rebooted) left in `dir`, a tool's folder — never a link, never a
/// version or `current` (blocking). The caller holds the tool's install
/// slot, or runs before any install can start.
fn sweep_leftovers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        if !install_leftover(&name.to_string_lossy()) {
            continue;
        }
        let path = e.path();
        let removed = match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(_) => continue,
        };
        if let Err(err) = removed {
            tracing::warn!(path = %path.display(), %err, "plugin tool leftover not removed");
        }
    }
}

/// At boot: every tool folder's leftovers, each swept holding its tool's
/// install slot (one that is installing is left alone) (blocking).
pub(crate) fn sweep_all_leftovers(state: &AppState) {
    let Ok(plugins) = std::fs::read_dir(tools_root(state)) else {
        return;
    };
    let installs = &state.plugin_platform.installs;
    for p in plugins.flatten() {
        if !p.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(tools) = std::fs::read_dir(p.path()) else {
            continue;
        };
        for t in tools.flatten() {
            if !t.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let key = (
                p.file_name().to_string_lossy().into_owned(),
                t.file_name().to_string_lossy().into_owned(),
            );
            if !crate::lock(&installs.busy).insert(key.clone()) {
                continue;
            }
            sweep_leftovers(&t.path());
            crate::lock(&installs.busy).remove(&key);
        }
    }
}

/// A removed plugin: all of its tools (blocking).
pub(crate) fn forget_plugin(state: &AppState, plugin: &str) {
    let dir = tools_root(state).join(plugin);
    if dir.exists() && !dir.is_symlink() {
        if let Err(err) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(%plugin, %err, "plugin tools not removed");
        }
    }
}

fn find<'a>(m: &'a Manifest, tool: &str) -> Option<&'a ToolDecl> {
    m.tools.iter().find(|t| t.id == tool)
}

/// `GET /plugins/{pid}/tools`: the card's Tools section.
pub(crate) async fn list_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    let Some(m) = super::manifest(&state, &pid) else {
        return super::not_found(&format!("plugin {pid}"));
    };
    let mut tools = Vec::new();
    for t in &m.tools {
        tools.push(tool_json(&state, &m, t).await);
    }
    Json(json!({"plugin": m.id, "tools": tools})).into_response()
}

/// `POST /plugins/{pid}/tools/{tool}/install`: the user's click (Install,
/// Update): download, check, unpack, set up. Held plugins can't.
pub(crate) async fn install_route(
    State(state): State<Arc<AppState>>,
    AxPath((pid, tool)): AxPath<(String, String)>,
) -> Response {
    let Some(m) = super::manifest(&state, &pid) else {
        return super::not_found(&format!("plugin {pid}"));
    };
    if let Some(fault) = &m.origin.fault {
        return (StatusCode::CONFLICT, Json(json!({"error": fault}))).into_response();
    }
    if super::trust::hold(&state, &m).is_some() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": format!("{} can't run here until it is trusted and allowed", m.name)})),
        )
            .into_response();
    }
    let Some(decl) = find(&m, &tool).cloned() else {
        return super::not_found(&format!("tool {tool} of {}", m.name));
    };
    // Detached: a window that goes away mid-download doesn't strand it.
    let state2 = state.clone();
    let handle = tokio::spawn(async move { install(&state2, &m, &decl).await });
    match handle.await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(err)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": err})),
        )
            .into_response(),
        Err(err) => super::bad_request(format!("the install failed: {err}")),
    }
}

/// `DELETE /plugins/{pid}/tools/{tool}`: Remove — every version of it.
pub(crate) async fn remove_route(
    State(state): State<Arc<AppState>>,
    AxPath((pid, tool)): AxPath<(String, String)>,
) -> Response {
    if !super::valid_id(&pid) || !super::valid_id(&tool) {
        return super::not_found("unknown tool");
    }
    if crate::lock(&state.plugin_platform.installs.busy).contains(&(pid.clone(), tool.clone())) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "it is installing now"})),
        )
            .into_response();
    }
    let dir = tool_dir(&state, &pid, &tool);
    let removed = tokio::task::spawn_blocking(move || {
        if dir.exists() && !dir.is_symlink() {
            std::fs::remove_dir_all(&dir).map(|()| true)
        } else {
            Ok(false)
        }
    })
    .await;
    match removed {
        Ok(Ok(removed)) => {
            super::activity::record(&state, &pid, json!({"kind": "tool-remove", "tool": tool}))
                .await;
            Json(json!({"tool": tool, "removed": removed})).into_response()
        }
        Ok(Err(err)) => super::bad_request(err.to_string()),
        Err(err) => super::bad_request(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-unpack-{}-{label}-{}",
            std::process::id(),
            &chimaera_core::generate_token()[..8]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A tar whose entries are written raw (the builder refuses `..` and
    /// absolute names, which is the point here).
    fn tar_of(entries: &[(&str, tar::EntryType, &[u8], &str)]) -> Vec<u8> {
        let mut out = tar::Builder::new(Vec::new());
        for (name, kind, body, link) in entries {
            let mut h = tar::Header::new_old();
            {
                let raw = h.as_old_mut();
                raw.name[..name.len()].copy_from_slice(name.as_bytes());
                raw.linkname[..link.len()].copy_from_slice(link.as_bytes());
            }
            h.set_entry_type(*kind);
            h.set_mode(0o755);
            h.set_size(body.len() as u64);
            h.set_cksum();
            out.append(&h, *body).unwrap();
        }
        out.into_inner().unwrap()
    }

    fn unpack_bytes(bytes: &[u8], label: &str) -> Result<(PathBuf, u64), String> {
        let dir = temp(label);
        let archive = dir.join("a.tar");
        std::fs::write(&archive, bytes).unwrap();
        let dest = dir.join("out");
        unpack(&archive, "tar", &dest, "x", 1 << 20, 100).map(|n| (dest, n))
    }

    #[test]
    fn a_good_archive_unpacks_with_its_inside_links() {
        let bytes = tar_of(&[
            ("tool/", tar::EntryType::Directory, b"", ""),
            ("tool/bin/", tar::EntryType::Directory, b"", ""),
            (
                "tool/bin/run",
                tar::EntryType::Regular,
                b"#!/bin/sh\necho hi\n",
                "",
            ),
            ("tool/run-link", tar::EntryType::Symlink, b"", "bin/run"),
        ]);
        let (dest, n) = unpack_bytes(&bytes, "good").unwrap();
        assert_eq!(n, 18);
        assert!(dest.join("tool/bin/run").is_file());
        assert_eq!(
            std::fs::read_link(dest.join("tool/run-link")).unwrap(),
            PathBuf::from("bin/run")
        );
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            std::fs::metadata(dest.join("tool/bin/run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }

    /// `bytes` through the host's `xz` (None where it has none).
    fn xz_of(bytes: &[u8]) -> Option<Vec<u8>> {
        use std::io::Write as _;
        let mut child = std::process::Command::new("xz")
            .args(["-c", "-T1"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        child.stdin.take()?.write_all(bytes).ok()?;
        let out = child.wait_with_output().ok()?;
        out.status.success().then_some(out.stdout)
    }

    fn unpack_xz(bytes: &[u8], label: &str) -> Result<PathBuf, String> {
        let dir = temp(label);
        let archive = dir.join("a.tar.xz");
        std::fs::write(&archive, bytes).unwrap();
        let dest = dir.join("out");
        unpack(&archive, "tar.xz", &dest, "x", 1 << 20, 100).map(|_| dest)
    }

    #[test]
    fn a_tar_xz_streams_through_xz_with_every_check_and_a_cut_one_is_refused() {
        let tar = tar_of(&[
            ("tool/", tar::EntryType::Directory, b"", ""),
            ("tool/run", tar::EntryType::Regular, b"#!/bin/sh\n", ""),
        ]);
        let Some(xz) = xz_of(&tar) else {
            eprintln!("no xz on this host: the streamed path is untested here");
            return;
        };
        let dest = unpack_xz(&xz, "xz-good").unwrap();
        assert_eq!(
            std::fs::read(dest.join("tool/run")).unwrap(),
            b"#!/bin/sh\n"
        );
        // The entries are still ours to check.
        let evil = xz_of(&tar_of(&[("../evil", tar::EntryType::Regular, b"x", "")])).unwrap();
        assert!(unpack_xz(&evil, "xz-evil").unwrap_err().contains("`..`"));
        // A stream cut short installs nothing, even where the tar reader
        // would have stopped quietly.
        let cut = &xz[..xz.len() - 16];
        assert!(unpack_xz(cut, "xz-cut").is_err());
    }

    #[test]
    fn a_tools_folder_setting_expands_home_and_variables() {
        let var = |name: &str| match name {
            "HOME" => Some("/home/me".to_string()),
            "SCRATCH" => Some("/scratch/users/me".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_dir("~/tools", var).unwrap(),
            PathBuf::from("/home/me/tools")
        );
        assert_eq!(
            expand_dir("$SCRATCH/chimaera-tools", var).unwrap(),
            PathBuf::from("/scratch/users/me/chimaera-tools")
        );
        assert_eq!(
            expand_dir("${SCRATCH}/t", var).unwrap(),
            PathBuf::from("/scratch/users/me/t")
        );
        assert_eq!(
            expand_dir("/opt/tools", var).unwrap(),
            PathBuf::from("/opt/tools")
        );
        assert!(expand_dir("$GROUP_HOME/t", var)
            .unwrap_err()
            .contains("GROUP_HOME"));
        assert!(expand_dir("tools", var).unwrap_err().contains("absolute"));
        assert!(expand_dir("${SCRATCH/t", var).is_err());
    }

    #[test]
    fn an_unfinished_installs_leftovers_go_and_nothing_else() {
        let dir = temp("leftovers");
        let outside = temp("leftovers-outside");
        std::fs::write(outside.join("keep"), b"x").unwrap();
        std::fs::write(dir.join(".download-abc"), b"x").unwrap();
        std::fs::write(dir.join(".download-abc.tar.tmp"), b"x").unwrap();
        std::fs::create_dir_all(dir.join(".unpack-abc/a")).unwrap();
        std::fs::create_dir_all(dir.join(".old-abc")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join(".old-link")).unwrap();
        std::fs::create_dir_all(dir.join("1.0.0/bin")).unwrap();
        std::os::unix::fs::symlink("1.0.0", dir.join("current")).unwrap();
        sweep_leftovers(&dir);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["1.0.0", "current"]);
        assert!(
            outside.join("keep").exists(),
            "a link is removed, never followed"
        );
    }

    #[test]
    fn the_hostile_archive_corpus_is_refused() {
        let outside = temp("outside");
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            (
                "dotdot",
                tar_of(&[("../evil", tar::EntryType::Regular, b"x", "")]),
                "`..`",
            ),
            (
                "absolute",
                tar_of(&[("/tmp/evil", tar::EntryType::Regular, b"x", "")]),
                "absolute",
            ),
            (
                "hardlink",
                tar_of(&[("a", tar::EntryType::Link, b"", "/etc/passwd")]),
                "hard link",
            ),
            (
                "link-out",
                tar_of(&[("a", tar::EntryType::Symlink, b"", "../../etc")]),
                "leaves the folder",
            ),
            (
                "link-abs",
                tar_of(&[("a", tar::EntryType::Symlink, b"", "/etc")]),
                "leaves the folder",
            ),
            (
                "through-link",
                tar_of(&[
                    ("sub/", tar::EntryType::Directory, b"", ""),
                    ("link", tar::EntryType::Symlink, b"", "sub"),
                    ("link/file", tar::EntryType::Regular, b"x", ""),
                ]),
                "a link where a folder must be",
            ),
            (
                "over-a-link",
                tar_of(&[
                    ("f", tar::EntryType::Symlink, b"", "g"),
                    ("f", tar::EntryType::Regular, b"x", ""),
                ]),
                "exists",
            ),
            (
                // Each link inside when read alone; together, above the folder.
                "link-chain",
                tar_of(&[
                    ("d/", tar::EntryType::Directory, b"", ""),
                    ("d/e", tar::EntryType::Symlink, b"", ".."),
                    ("f", tar::EntryType::Symlink, b"", "d/e/../x"),
                ]),
                "through another link",
            ),
            (
                "link-loop",
                tar_of(&[
                    ("a", tar::EntryType::Symlink, b"", "b"),
                    ("b", tar::EntryType::Symlink, b"", "a"),
                ]),
                "through another link",
            ),
            (
                "device",
                tar_of(&[("dev", tar::EntryType::Char, b"", "")]),
                "Char",
            ),
            (
                "fifo",
                tar_of(&[("p", tar::EntryType::Fifo, b"", "")]),
                "Fifo",
            ),
        ];
        for (label, bytes, why) in cases {
            let err = unpack_bytes(&bytes, label).unwrap_err();
            assert!(err.contains(why), "{label}: {err}");
        }
        // Nothing landed outside.
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        // The limits.
        let many: Vec<(String, tar::EntryType, &[u8], &str)> = (0..101)
            .map(|i| (format!("f{i}"), tar::EntryType::Regular, &b"x"[..], ""))
            .collect();
        let refs: Vec<(&str, tar::EntryType, &[u8], &str)> = many
            .iter()
            .map(|(n, k, b, l)| (n.as_str(), *k, *b, *l))
            .collect();
        assert!(unpack_bytes(&tar_of(&refs), "many")
            .unwrap_err()
            .contains("entries"));
        let big = vec![0u8; (1 << 20) + 1];
        assert!(unpack_bytes(
            &tar_of(&[("big", tar::EntryType::Regular, &big, "")]),
            "big"
        )
        .unwrap_err()
        .contains("unpacked"));
    }

    #[test]
    fn links_are_read_lexically() {
        assert!(link_stays_inside(Path::new("a/b/link"), "../c"));
        assert!(link_stays_inside(Path::new("a/link"), "../c"));
        assert!(!link_stays_inside(Path::new("link"), "../c"));
        assert!(!link_stays_inside(Path::new("a/link"), "../../c"));
        assert!(!link_stays_inside(Path::new("a/link"), "/c"));
    }
}
