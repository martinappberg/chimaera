//! Output folders: where a plugin keeps what it makes (a built PDF, a log,
//! a snapshot to diff against), one per (plugin, workspace), outside the
//! repository. Design: docs/plugin-platform-plan.md §7.
//!
//! - `<cache dir>/plugins/<id>/<workspace>/` (`chimaera_core::cache_dir`:
//!   `$XDG_CACHE_HOME/chimaera`, survives restarts, never night-scrubbed).
//! - The plugin addresses it as `output:<path>`: every path is relative,
//!   walked from the folder's own descriptor with `O_NOFOLLOW` on each
//!   component (a program the plugin runs could leave a link there), so
//!   nothing it names leaves the folder.
//! - Reads start at an offset and stop at 8 MiB a call (a large log
//!   streams); the plugin's own writes are ≤ 8 MiB a file, written to a
//!   temporary name and renamed into place.
//! - A quota per plugin across its workspaces (1 GiB): past it, the least
//!   recently changed top-level entries go. Usage is measured by a bounded
//!   walk, at most once a minute per plugin unless a Clear reset it.
//! - The UI reads output files through the ordinary file routes by their
//!   absolute path (`GET …/plugins/{pid}/output` answers the folder), so the
//!   PDF viewer, the log view and `/raw` tickets work unchanged.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use axum::extract::{Path as AxPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde_json::json;

use crate::AppState;

/// A read's ceiling, whatever `cap` the plugin asks for.
pub(crate) const READ_MAX: usize = 8 << 20;
/// A file the plugin writes itself.
pub(crate) const WRITE_MAX: usize = 8 << 20;
const LIST_MAX: usize = 4096;
/// A plugin's output folders together.
pub(crate) const QUOTA: u64 = 1 << 30;
/// The walk that measures usage stops here (entries).
const WALK_MAX: usize = 200_000;
const USAGE_TTL: Duration = Duration::from_secs(60);

/// Measured usage per plugin: bytes, and when.
#[derive(Default)]
pub(crate) struct Usage {
    by_plugin: HashMap<String, (u64, Instant)>,
}

/// The root every plugin's output folders live under.
pub(crate) fn root_for(data_dir: &Path) -> PathBuf {
    #[cfg(test)]
    return data_dir.join("cache").join("plugins");
    #[cfg(not(test))]
    {
        let _ = data_dir;
        chimaera_core::cache_dir().join("plugins")
    }
}

/// `(plugin, workspace)`'s folder (not created).
pub(crate) fn folder(root: &Path, plugin: &str, ws: &str) -> PathBuf {
    root.join(plugin).join(crate::timeline::sanitize(ws))
}

/// `output:<path>` or a bare path, as a relative path of plain components.
pub(crate) fn relative(path: &str) -> Result<PathBuf, String> {
    let path = path.strip_prefix("output:").unwrap_or(path);
    let mut out = PathBuf::new();
    for part in Path::new(path).components() {
        match part {
            Component::Normal(name) => out.push(name),
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "output:{path}: paths stay inside the output folder (no `..`, not absolute)"
                ))
            }
        }
    }
    Ok(out)
}

fn io(rel: &Path, err: impl std::fmt::Display) -> String {
    format!("output:{}: {err}", rel.display())
}

/// The folder itself, created (0700) if missing.
fn open_root(dir: &Path) -> Result<File, String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .map_err(|e| format!("the output folder: {e}"))?;
    File::open(dir).map_err(|e| format!("the output folder: {e}"))
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// Bytes of `rel` from `offset`, at most `cap` (blocking).
pub(crate) fn read(dir: &Path, rel: &Path, offset: u64, cap: usize) -> Result<Vec<u8>, String> {
    let root = open_root(dir)?;
    let mut file = crate::download::open_beneath(
        &root,
        rel,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
    )
    .map_err(|e| io(rel, e))?;
    let meta = file.metadata().map_err(|e| io(rel, e))?;
    if !meta.is_file() {
        return Err(io(rel, "not a regular file"));
    }
    let cap = cap.min(READ_MAX);
    file.seek(SeekFrom::Start(offset)).map_err(|e| io(rel, e))?;
    let mut bytes = Vec::with_capacity(cap.min(meta.len().saturating_sub(offset) as usize));
    file.take(cap as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| io(rel, e))?;
    Ok(bytes)
}

/// A folder's entries, at most `cap`, sorted (blocking).
pub(crate) fn list(
    dir: &Path,
    rel: &Path,
    cap: usize,
) -> Result<Vec<super::runtime::wit::Entry>, String> {
    let root = open_root(dir)?;
    let folder = crate::download::open_beneath(&root, rel, dir_flags()).map_err(|e| io(rel, e))?;
    let entries = rustix::fs::Dir::read_from(&folder).map_err(|e| io(rel, e))?;
    let mut out = Vec::new();
    for entry in entries {
        if out.len() >= cap.min(LIST_MAX) {
            break;
        }
        let entry = entry.map_err(|e| io(rel, e))?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let kind = match entry.file_type() {
            FileType::Unknown => rustix::fs::statat(&folder, name, AtFlags::SYMLINK_NOFOLLOW)
                .map(|st| FileType::from_raw_mode(st.st_mode))
                .unwrap_or(FileType::Unknown),
            known => known,
        };
        out.push(super::runtime::wit::Entry {
            name: name.to_string_lossy().into_owned(),
            is_dir: kind == FileType::Directory,
            is_symlink: kind == FileType::Symlink,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Keep the `keep` newest entries of folder `rel` (by modification time)
/// and remove the rest, never following a link (blocking). A folder that
/// isn't there yet is fine.
pub(crate) fn prune_oldest(dir: &Path, rel: &Path, keep: usize) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let root = open_root(dir)?;
    let folder = match crate::download::open_beneath(&root, rel, dir_flags()) {
        Ok(folder) => folder,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(rel, e)),
    };
    let mut entries = Vec::new();
    for entry in rustix::fs::Dir::read_from(&folder).map_err(|e| io(rel, e))? {
        if entries.len() >= LIST_MAX {
            break;
        }
        let entry = entry.map_err(|e| io(rel, e))?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let name = std::ffi::OsStr::from_bytes(name).to_os_string();
        if let Ok(st) = rustix::fs::statat(&folder, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
            entries.push((st.st_mtime, name));
        }
    }
    if entries.len() <= keep {
        return Ok(());
    }
    entries.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    let mut left = WALK_MAX;
    for (_, name) in entries.into_iter().skip(keep) {
        remove_entry(&folder, &name, &mut left).map_err(|e| io(rel, e))?;
    }
    Ok(())
}

/// The parent folder of `rel` beneath `root`, made on the way (never
/// through a link, private: the output folder's), and `rel`'s last
/// component.
fn parent_of(root: &File, rel: &Path) -> Result<(File, std::ffi::OsString), String> {
    parent_made(root, rel, 0o700)
}

/// `parent_of`, making missing folders with `mode` (the umask applies).
fn parent_made(
    root: &File,
    rel: &Path,
    mode: rustix::fs::RawMode,
) -> Result<(File, std::ffi::OsString), String> {
    let mut parts: Vec<&std::ffi::OsStr> = rel
        .components()
        .map(|c| match c {
            Component::Normal(n) => Ok(n),
            _ => Err(io(rel, "not a plain relative path")),
        })
        .collect::<Result<_, _>>()?;
    let name = parts
        .pop()
        .ok_or_else(|| io(rel, "names the folder itself"))?
        .to_os_string();
    let mut dir = root.try_clone().map_err(|e| io(rel, e))?;
    for part in parts {
        match rustix::fs::mkdirat(&dir, part, Mode::from_raw_mode(mode)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(io(rel, e)),
        }
        dir = File::from(
            rustix::fs::openat(&dir, part, dir_flags(), Mode::empty()).map_err(|e| io(rel, e))?,
        );
    }
    Ok((dir, name))
}

/// Make folder `rel` (and those on its path) beneath the output folder,
/// never through a link (blocking).
pub(crate) fn make_dir(dir: &Path, rel: &Path) -> Result<(), String> {
    let root = open_root(dir)?;
    if rel.as_os_str().is_empty() {
        return Ok(());
    }
    let (parent, name) = parent_of(&root, rel)?;
    match rustix::fs::mkdirat(&parent, name.as_os_str(), Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(e) => return Err(io(rel, e)),
    }
    // It must be a folder, not a link someone left there.
    rustix::fs::openat(&parent, name.as_os_str(), dir_flags(), Mode::empty())
        .map(drop)
        .map_err(|e| io(rel, e))
}

/// Create (or truncate) file `rel` for writing, never through a link
/// (blocking): a job's log.
pub(crate) fn create_file(dir: &Path, rel: &Path) -> Result<File, String> {
    let root = open_root(dir)?;
    let (parent, name) = parent_of(&root, rel)?;
    let fd = rustix::fs::openat(
        &parent,
        name.as_os_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| io(rel, e))?;
    Ok(File::from(fd))
}

/// Write `bytes` to `rel` (a temporary name, then a rename: a reader never
/// sees half a file) (blocking).
pub(crate) fn write(dir: &Path, rel: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > WRITE_MAX {
        return Err(io(
            rel,
            format!(
                "a file the plugin writes is at most {} MiB",
                WRITE_MAX >> 20
            ),
        ));
    }
    let root = open_root(dir)?;
    let (parent, name) = parent_of(&root, rel)?;
    let tmp = format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        &chimaera_core::generate_token()[..8]
    );
    let fd = rustix::fs::openat(
        &parent,
        tmp.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| io(rel, e))?;
    let mut file = File::from(fd);
    let written = file.write_all(bytes).and_then(|()| file.flush());
    drop(file);
    if let Err(err) = written {
        let _ = rustix::fs::unlinkat(&parent, tmp.as_str(), AtFlags::empty());
        return Err(io(rel, err));
    }
    // A folder where the file goes is refused by rename (EISDIR); a link
    // there is replaced, never written through.
    rustix::fs::renameat(&parent, tmp.as_str(), &parent, name.as_os_str()).map_err(|e| {
        let _ = rustix::fs::unlinkat(&parent, tmp.as_str(), AtFlags::empty());
        io(rel, e)
    })
}

/// Copy output file `rel` to `dst_rel` beneath the workspace root `dst`
/// (**Save to workspace**, the user's click): never through a link on
/// either side, never over an existing file unless `replace`; its bytes.
pub(crate) fn copy_out(
    dir: &Path,
    rel: &Path,
    dst: &Path,
    dst_rel: &Path,
    replace: bool,
) -> Result<u64, String> {
    let root = open_root(dir)?;
    let mut src = crate::download::open_beneath(
        &root,
        rel,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
    )
    .map_err(|e| io(rel, e))?;
    if !src.metadata().map_err(|e| io(rel, e))?.is_file() {
        return Err(io(rel, "not a regular file"));
    }
    let dst_root = File::open(dst).map_err(|e| format!("the workspace root: {e}"))?;
    // Folders made in the workspace are ordinary ones (collaborators on a
    // shared project read them), not the output folder's private ones.
    let (parent, name) = parent_made(&dst_root, dst_rel, 0o755)?;
    let shown = || dst_rel.display().to_string();
    match rustix::fs::statat(&parent, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile => {
            return Err(format!("{}: something other than a file is there", shown()))
        }
        Ok(_) if !replace => return Err(format!("{} already exists", shown())),
        _ => {}
    }
    let tmp = format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        &chimaera_core::generate_token()[..8]
    );
    let fd = rustix::fs::openat(
        &parent,
        tmp.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o644),
    )
    .map_err(|e| format!("{}: {e}", shown()))?;
    let mut out = File::from(fd);
    let copied = std::io::copy(&mut (&mut src).take(QUOTA), &mut out).and_then(|n| {
        out.flush()?;
        Ok(n)
    });
    drop(out);
    let n = match copied {
        Ok(n) => n,
        Err(err) => {
            let _ = rustix::fs::unlinkat(&parent, tmp.as_str(), AtFlags::empty());
            return Err(format!("{}: {err}", shown()));
        }
    };
    rustix::fs::renameat(&parent, tmp.as_str(), &parent, name.as_os_str()).map_err(|e| {
        let _ = rustix::fs::unlinkat(&parent, tmp.as_str(), AtFlags::empty());
        format!("{}: {e}", shown())
    })?;
    Ok(n)
}

/// Whether any component of `rel` is hidden (starts with a dot).
fn hidden(rel: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    rel.components()
        .any(|c| c.as_os_str().as_bytes().first() == Some(&b'.'))
}

/// Remove `rel`, a file or a whole folder, never following a link
/// (blocking). The folder itself: `rel` empty.
pub(crate) fn remove(dir: &Path, rel: &Path) -> Result<(), String> {
    if !dir.exists() {
        return Ok(());
    }
    let root = open_root(dir)?;
    if rel.as_os_str().is_empty() {
        let mut left = WALK_MAX;
        return empty_folder(&root, &mut left).map_err(|e| io(rel, e));
    }
    let (parent, name) = parent_of(&root, rel)?;
    remove_entry(&parent, &name, &mut WALK_MAX.clone()).map_err(|e| io(rel, e))
}

fn remove_entry(parent: &File, name: &std::ffi::OsStr, left: &mut usize) -> std::io::Result<()> {
    let st = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if FileType::from_raw_mode(st.st_mode) == FileType::Directory {
        let dir = File::from(rustix::fs::openat(
            parent,
            name,
            dir_flags(),
            Mode::empty(),
        )?);
        empty_folder(&dir, left)?;
        rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?;
    } else {
        rustix::fs::unlinkat(parent, name, AtFlags::empty())?;
    }
    Ok(())
}

fn empty_folder(dir: &File, left: &mut usize) -> std::io::Result<()> {
    let names: Vec<std::ffi::OsString> = rustix::fs::Dir::read_from(dir)?
        .filter_map(Result::ok)
        .map(|e| std::ffi::OsString::from(e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n != "." && n != "..")
        .collect();
    for name in names {
        if *left == 0 {
            return Err(std::io::Error::other("too many entries to remove"));
        }
        *left -= 1;
        remove_entry(dir, &name, left)?;
    }
    Ok(())
}

/// Bytes under `dir`, walking at most `WALK_MAX` entries, never through a
/// link (blocking); also each top-level entry of each workspace folder, for
/// eviction: (path, bytes, newest mtime).
fn measure(dir: &Path) -> (u64, Vec<(PathBuf, u64, SystemTime)>) {
    let mut total = 0;
    let mut tops = Vec::new();
    let mut left = WALK_MAX;
    let Ok(workspaces) = std::fs::read_dir(dir) else {
        return (0, tops);
    };
    for ws in workspaces.flatten() {
        let Ok(entries) = std::fs::read_dir(ws.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let (bytes, newest) = walk(&entry.path(), &mut left);
            total += bytes;
            tops.push((entry.path(), bytes, newest));
        }
    }
    (total, tops)
}

fn walk(path: &Path, left: &mut usize) -> (u64, SystemTime) {
    if *left == 0 {
        return (0, SystemTime::UNIX_EPOCH);
    }
    *left -= 1;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return (0, SystemTime::UNIX_EPOCH);
    };
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    if !meta.is_dir() {
        return (meta.len(), mtime);
    }
    let mut bytes = 0;
    let mut newest = mtime;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let (b, m) = walk(&entry.path(), left);
            bytes += b;
            newest = newest.max(m);
        }
    }
    (bytes, newest)
}

impl Usage {
    /// `plugin`'s usage, measured again when older than `USAGE_TTL`; past
    /// `quota`, its oldest top-level entries are removed until it fits
    /// (blocking).
    pub(crate) fn enforce(root: &Path, plugin: &str, quota: u64) -> u64 {
        let dir = root.join(plugin);
        let (mut total, mut tops) = measure(&dir);
        if total > quota {
            tops.sort_by_key(|(_, _, newest)| *newest);
            for (path, bytes, _) in tops {
                if total <= quota {
                    break;
                }
                let removed = if path.is_dir() && !path.is_symlink() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                match removed {
                    Ok(()) => {
                        total = total.saturating_sub(bytes);
                        tracing::info!(%plugin, path = %path.display(), "plugin output evicted (over its quota)");
                    }
                    Err(err) => tracing::warn!(%plugin, %err, "plugin output eviction failed"),
                }
            }
        }
        total
    }

    /// The last measured usage if fresh.
    fn fresh(&self, plugin: &str) -> Option<u64> {
        self.by_plugin
            .get(plugin)
            .filter(|(_, at)| at.elapsed() < USAGE_TTL)
            .map(|(b, _)| *b)
    }

    fn record(&mut self, plugin: &str, bytes: u64) {
        self.by_plugin
            .insert(plugin.to_string(), (bytes, Instant::now()));
    }

    fn forget(&mut self, plugin: &str) {
        self.by_plugin.remove(plugin);
    }
}

/// Measure (and enforce the quota on) `plugin`'s output unless measured
/// within the last minute; its bytes.
pub(crate) async fn usage(state: &AppState, plugin: &str, force: bool) -> u64 {
    if !force {
        if let Some(bytes) = crate::lock(&state.plugin_platform.usage).fresh(plugin) {
            return bytes;
        }
    }
    let root = state.plugin_platform.output_root.clone();
    let id = plugin.to_string();
    let bytes = tokio::task::spawn_blocking(move || Usage::enforce(&root, &id, QUOTA))
        .await
        .unwrap_or(0);
    crate::lock(&state.plugin_platform.usage).record(plugin, bytes);
    bytes
}

/// A removed plugin: every output folder it had (blocking).
pub(crate) fn forget_plugin(state: &AppState, plugin: &str) {
    crate::lock(&state.plugin_platform.usage).forget(plugin);
    let dir = state.plugin_platform.output_root.join(plugin);
    if dir.exists() && !dir.is_symlink() {
        if let Err(err) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(%plugin, %err, "plugin output not removed");
        }
    }
}

/// A deleted workspace: every plugin's output folder for it (blocking).
pub(crate) fn forget_workspace(root: &Path, ws: &str) {
    let Ok(plugins) = std::fs::read_dir(root) else {
        return;
    };
    let name = crate::timeline::sanitize(ws);
    for plugin in plugins.flatten() {
        let dir = plugin.path().join(&name);
        if dir.exists() && !dir.is_symlink() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// `GET /plugins/{pid}/output`: the plugin's use of its output folders
/// (Settings → Plugins shows it beside **Clear**).
pub(crate) async fn usage_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    if super::manifest(&state, &pid).is_none() {
        return super::not_found(&format!("plugin {pid}"));
    }
    let bytes = usage(&state, &pid, false).await;
    Json(json!({"bytes": bytes, "quota": QUOTA})).into_response()
}

/// `DELETE /plugins/{pid}/output`: Clear — every output folder it has.
pub(crate) async fn clear_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
) -> Response {
    if !super::valid_id(&pid) {
        return super::not_found(&format!("plugin {pid}"));
    }
    let root = state.plugin_platform.output_root.join(&pid);
    let cleared = tokio::task::spawn_blocking(move || remove(&root, Path::new(""))).await;
    crate::lock(&state.plugin_platform.usage).forget(&pid);
    match cleared {
        Ok(Ok(())) => Json(json!({"bytes": 0, "quota": QUOTA})).into_response(),
        Ok(Err(err)) => super::bad_request(err),
        Err(err) => super::bad_request(format!("clearing failed: {err}")),
    }
}

/// `GET /workspaces/{id}/plugins/{pid}/output`: the folder's absolute
/// path, so the UI resolves `output:<path>` for its file views.
pub(crate) async fn folder_route(
    State(state): State<Arc<AppState>>,
    AxPath((ws, pid)): AxPath<(String, String)>,
) -> Response {
    if crate::lock(&state.workspaces).get(&ws).is_none() {
        return super::not_found(&format!("workspace {ws}"));
    }
    if super::manifest(&state, &pid).is_none() {
        return super::not_found(&format!("plugin {pid}"));
    }
    let dir = folder(&state.plugin_platform.output_root, &pid, &ws);
    let made = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || open_root(&dir).map(drop)).await
    };
    if let Ok(Err(err)) = made {
        return super::bad_request(err);
    }
    Json(json!({"root": dir.display().to_string()})).into_response()
}

#[derive(serde::Deserialize)]
pub(crate) struct SaveBody {
    /// `output:<path>`.
    from: String,
    /// Workspace-relative.
    to: String,
    #[serde(default)]
    replace: bool,
}

/// `POST /workspaces/{id}/plugins/{pid}/output/save {from, to, replace?}`:
/// **Save to workspace** — the user's click copies an output file into the
/// workspace (a plugin can't write there itself).
pub(crate) async fn save_route(
    State(state): State<Arc<AppState>>,
    AxPath((ws, pid)): AxPath<(String, String)>,
    Json(body): Json<SaveBody>,
) -> Response {
    let Some(root) = crate::lock(&state.workspaces).get(&ws).map(|w| w.root) else {
        return super::not_found(&format!("workspace {ws}"));
    };
    if super::manifest(&state, &pid).is_none() {
        return super::not_found(&format!("plugin {pid}"));
    }
    let (rel, dst_rel) = match (relative(&body.from), super::hostfns_relative(&body.to)) {
        (Ok(a), Ok(b)) if !b.as_os_str().is_empty() => (a, b),
        (Err(e), _) | (_, Err(e)) => return super::bad_request(e),
        _ => return super::bad_request("name the file to save to"),
    };
    // The plugin names where its button saves: never a hidden path, where a
    // file does more than sit there (`.envrc`, `.git/hooks`, `.github/`).
    if hidden(&dst_rel) {
        return super::bad_request(format!(
            "{}: a plugin saves only to visible paths in the workspace",
            body.to
        ));
    }
    let dir = folder(&state.plugin_platform.output_root, &pid, &ws);
    let target = root.join(&dst_rel);
    let copied = {
        let root = root.clone();
        let dst_rel = dst_rel.clone();
        tokio::task::spawn_blocking(move || copy_out(&dir, &rel, &root, &dst_rel, body.replace))
            .await
    };
    match copied {
        Ok(Ok(bytes)) => {
            let target = target.display().to_string();
            crate::git::mark_path_dirty(&state, &target).await;
            Json(json!({"path": target, "bytes": bytes})).into_response()
        }
        Ok(Err(err)) => super::bad_request(err),
        Err(err) => super::bad_request(format!("saving failed: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-output-test-{}-{label}-{}",
            std::process::id(),
            &chimaera_core::generate_token()[..8]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pruning_keeps_the_newest_and_never_follows_a_link() {
        let dir = temp("prune");
        let outside = temp("prune-outside");
        std::fs::write(outside.join("keep.txt"), b"x").unwrap();
        let jobs = dir.join(".jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        // The oldest of all: a link out, which goes as a link.
        std::os::unix::fs::symlink(&outside, jobs.join("j-link")).unwrap();
        let now = SystemTime::now();
        for (i, name) in ["j-a", "j-b", "j-c"].iter().enumerate() {
            let f = jobs.join(name);
            std::fs::create_dir(&f).unwrap();
            std::fs::write(f.join("stdout.log"), b"log").unwrap();
            File::open(&f)
                .unwrap()
                .set_modified(now + Duration::from_secs(10 + i as u64 * 10))
                .unwrap();
        }
        prune_oldest(&dir, Path::new(".jobs"), 2).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&jobs)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["j-b", "j-c"]);
        assert!(outside.join("keep.txt").exists());
        // Nothing there yet is fine.
        prune_oldest(&dir, Path::new("none"), 2).unwrap();
    }

    #[test]
    fn a_save_goes_only_to_visible_paths() {
        assert!(hidden(Path::new(".envrc")));
        assert!(hidden(Path::new(".github/workflows/x.yml")));
        assert!(hidden(Path::new("docs/.git/hooks/pre-commit")));
        assert!(!hidden(Path::new("thesis/main.pdf")));
    }

    #[test]
    fn a_plugin_stays_in_its_folder() {
        let dir = temp("output");
        write(&dir, Path::new("a/b.txt"), b"hello").unwrap();
        assert_eq!(read(&dir, Path::new("a/b.txt"), 1, 3).unwrap(), b"ell");
        assert!(relative("../x").is_err());
        assert!(relative("/etc/passwd").is_err());
        assert_eq!(relative("output:a/b").unwrap(), PathBuf::from("a/b"));
        // A link someone left there is never followed.
        std::os::unix::fs::symlink("/etc", dir.join("out")).unwrap();
        assert!(read(&dir, Path::new("out/hostname"), 0, 10).is_err());
        assert!(write(&dir, Path::new("out/x"), b"x").is_err());
        // Writing over the link replaces the link, not what it points to.
        write(&dir, Path::new("out"), b"file now").unwrap();
        assert!(!dir.join("out").is_symlink());
        let listed = list(&dir, Path::new(""), 10).unwrap();
        assert_eq!(listed.len(), 2);
        remove(&dir, Path::new("a")).unwrap();
        assert!(!dir.join("a").exists());
        assert!(write(&dir, Path::new("big"), &vec![0; WRITE_MAX + 1]).is_err());
    }

    #[test]
    fn the_quota_evicts_the_oldest_first() {
        let root = temp("quota");
        let ws = folder(&root, "p", "w-1");
        write(&ws, Path::new("old/a"), &[0; 600]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        write(&ws, Path::new("new"), &[0; 600]).unwrap();
        let old = std::fs::File::options()
            .write(true)
            .open(ws.join("old/a"))
            .unwrap();
        old.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        std::fs::File::open(ws.join("old"))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        assert_eq!(Usage::enforce(&root, "p", 1000), 600);
        assert!(!ws.join("old").exists());
        assert!(ws.join("new").exists());
    }
}
