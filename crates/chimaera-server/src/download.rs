//! GET /download/{ticket} — browser-native downloads. An anchor navigation
//! cannot send an Authorization header, so downloads reuse the /raw story: a
//! short-lived single-path ticket, minted via the bearer-authed POST
//! /api/v1/fs/ticket, authorizes each fetch (the route is mounted outside the
//! auth layer). Files stream as-is with an attachment disposition;
//! directories stream as a zip built on the fly — memory is bounded by the
//! duplex pipe, deflate window, and entry-capped traversal stack; nothing is
//! spooled to disk.

use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_zip::{
    AttributeCompatibility, Compression, ZipDateTime, ZipDateTimeBuilder, ZipEntryBuilder,
};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tokio::io::DuplexStream;
use tokio_util::io::ReaderStream;

use crate::AppState;

/// Hard ceiling on entries (files + directories) in one streamed zip. A
/// scratch dir on a login node can hold millions of files; past this the
/// stream is aborted (an honest failed download), never silently truncated
/// into an archive that looks complete.
const MAX_ZIP_ENTRIES: usize = 250_000;
/// Raw filesystem-name bytes retained while one directory is classified.
/// Combined with the entry cap (which bounds `OsString` allocation overhead),
/// this prevents one very wide directory from consuming the daemon's RSS.
const MAX_ZIP_DIRECTORY_NAME_BYTES: usize = 8 * 1024 * 1024;
/// Bytes owned by relative paths waiting on the descriptor-relative DFS stack.
/// Full prefixes are charged, so a deep, wide tree cannot amplify a small set
/// of component names into hundreds of megabytes of queued `PathBuf`s.
const MAX_ZIP_QUEUED_PATH_BYTES: usize = 8 * 1024 * 1024;

struct ZipEntryBudget {
    used: usize,
    limit: usize,
}

impl ZipEntryBudget {
    fn new(limit: usize) -> Self {
        Self { used: 0, limit }
    }

    fn ensure_available(&self) -> anyhow::Result<()> {
        if self.used >= self.limit {
            anyhow::bail!("more than {} entries — refusing to zip", self.limit);
        }
        Ok(())
    }

    fn reserve(&mut self) -> anyhow::Result<()> {
        self.ensure_available()?;
        self.used += 1;
        Ok(())
    }

    fn remaining(&self) -> usize {
        self.limit - self.used
    }
}

struct ZipPathBudget {
    used: usize,
    limit: usize,
}

impl ZipPathBudget {
    fn new(limit: usize) -> Self {
        Self { used: 0, limit }
    }

    fn reserve(&mut self, path: &FsPath) -> anyhow::Result<()> {
        let bytes = path.as_os_str().as_encoded_bytes().len();
        let Some(next) = self.used.checked_add(bytes) else {
            anyhow::bail!("zip traversal path budget overflow");
        };
        if next > self.limit {
            anyhow::bail!(
                "zip traversal needs more than {} bytes of queued paths",
                self.limit
            );
        }
        self.used = next;
        Ok(())
    }

    fn release(&mut self, path: &FsPath) {
        self.used -= path.as_os_str().as_encoded_bytes().len();
    }
}

fn charge_directory_name_bytes(
    retained: &mut usize,
    additional: usize,
    limit: usize,
) -> anyhow::Result<()> {
    let Some(next) = retained.checked_add(additional) else {
        anyhow::bail!("zip directory-name budget overflow");
    };
    if next > limit {
        anyhow::bail!("zip directory names exceed {limit} retained bytes");
    }
    *retained = next;
    Ok(())
}

/// Reserve an archive entry before retaining its path. Charging directories
/// at discovery keeps a very wide tree from filling the traversal stack
/// before the archive-entry ceiling is reached.
fn queue_zip_directory(
    stack: &mut Vec<PathBuf>,
    budget: &mut ZipEntryBudget,
    paths: &mut ZipPathBudget,
    path: PathBuf,
) -> anyhow::Result<()> {
    budget.ensure_available()?;
    paths.reserve(&path)?;
    // `ensure_available` above makes this infallible; keep `reserve` as the
    // single place that advances the entry counter.
    budget.reserve()?;
    stack.push(path);
    Ok(())
}

/// Capacity of the duplex pipe between the zip task and the response body.
/// This bounds buffered archive bytes; traversal paths have the separate
/// entry and byte ceilings above. Backpressure from a slow client suspends the
/// walk rather than buffering file contents.
const PIPE_CAPACITY: usize = 64 * 1024;

/// GET /download/{ticket} — the ticketed path as a browser download. A file
/// streams verbatim (Content-Length included, so the browser shows progress);
/// a directory streams as `<name>.zip`. Renaming or deleting the path after
/// minting simply 404s (the ticket is a per-path snapshot); a walk already in
/// flight finishes on whatever it can still read. Entry mtimes ride into the
/// zip as DOS datetimes in UTC (zip carries no timezone; without them every
/// extracted file would read as 1980, the DOS epoch).
pub(crate) async fn download(
    State(state): State<Arc<AppState>>,
    Path(ticket): Path<String>,
) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    let Some(path) = crate::lock(&state.tickets).lookup(&ticket) else {
        return not_found();
    };
    let Ok(target) = open_ticket_target(path.clone()).await else {
        tracing::warn!(path = %path.display(), "ticketed download path unstattable");
        return not_found();
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());

    if let OpenedTicketTarget::Directory(root_fd) = &target {
        let (writer, reader) = tokio::io::duplex(PIPE_CAPACITY);
        let root = path.clone();
        let root_fd = Arc::clone(root_fd);
        let utc_offset = local_utc_offset().await;
        tokio::spawn(async move {
            // A failure mid-walk drops the writer: the client sees a
            // truncated/failed download, never an incomplete archive
            // presented as success. A client disconnect closes the read
            // half, the next write errors, and the task exits.
            if let Err(err) = zip_dir(root.clone(), root_fd, writer, utc_offset).await {
                tracing::warn!(path = %root.display(), %err, "folder download aborted");
            }
        });
        return (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/zip"),
                ),
                (
                    header::CONTENT_DISPOSITION,
                    content_disposition(&format!("{name}.zip")),
                ),
            ],
            Body::from_stream(ReaderStream::new(reader)),
        )
            .into_response();
    }

    let OpenedTicketTarget::File(file, meta) = target else {
        unreachable!("directory target returned above");
    };
    let file = tokio::fs::File::from_std(file);
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime.essence_str())
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            ),
            (header::CONTENT_LENGTH, HeaderValue::from(meta.len())),
            (header::CONTENT_DISPOSITION, content_disposition(&name)),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response()
}

/// Walk `root` and stream it as a zip into `pipe`. Iterative (no recursion),
/// symlinks are never followed (loop/escape safety — the quickopen walker's
/// discipline), unreadable entries are skipped like `fs/list` skips them.
/// Every directory gets an explicit `name/` entry so empty ones survive
/// extraction; entry names are rooted at the folder's own name, so unzipping
/// yields a single top-level folder.
async fn zip_dir(
    root: PathBuf,
    root_fd: Arc<File>,
    pipe: DuplexStream,
    utc_offset: i64,
) -> anyhow::Result<()> {
    use tokio_util::compat::TokioAsyncReadCompatExt;

    let root_name = root
        .file_name()
        .map(safe_zip_component)
        .unwrap_or_else(|| "folder".to_string());
    let mut zip = async_zip::base::write::ZipFileWriter::with_tokio(pipe);
    let mut stack = Vec::new();
    let mut budget = ZipEntryBudget::new(MAX_ZIP_ENTRIES);
    let mut path_budget = ZipPathBudget::new(MAX_ZIP_QUEUED_PATH_BYTES);
    queue_zip_directory(&mut stack, &mut budget, &mut path_budget, PathBuf::new())?;

    while let Some(rel) = stack.pop() {
        path_budget.release(&rel);
        let Some((dir_meta, names)) =
            list_zip_directory(Arc::clone(&root_fd), rel.clone(), budget.remaining()).await?
        else {
            continue; // renamed/unreadable after discovery
        };
        let dir_entry = zip_entry_name(&root_name, &rel, true);
        zip.write_entry_whole(
            stamp(
                ZipEntryBuilder::new(dir_entry.into(), Compression::Stored),
                Some(&dir_meta),
                utc_offset,
                0o040_755, // drwxr-xr-x
            ),
            &[],
        )
        .await?;

        for entry_name in names {
            // Preserve the old fail-fast behavior: once full, do not keep
            // opening entries merely to discover whether they are readable.
            budget.ensure_available()?;
            let entry_rel = rel.join(entry_name);
            match open_zip_entry(Arc::clone(&root_fd), entry_rel.clone()).await? {
                Some(OpenedZipEntry::Directory) => {
                    queue_zip_directory(&mut stack, &mut budget, &mut path_budget, entry_rel)?;
                }
                Some(OpenedZipEntry::File(file, meta)) => {
                    budget.reserve()?;
                    let name = zip_entry_name(&root_name, &entry_rel, false);
                    let builder = stamp(
                        ZipEntryBuilder::new(name.into(), Compression::Deflate),
                        Some(&meta),
                        utc_offset,
                        0o100_644, // -rw-r--r--
                    );
                    let file = tokio::fs::File::from_std(file);
                    let mut entry_writer = zip.write_entry_stream(builder).await?;
                    futures::io::copy(&mut file.compat(), &mut entry_writer).await?;
                    entry_writer.close().await?;
                }
                None => {} // symlink, special file, vanished, or unreadable
            }
        }
    }
    zip.close().await?;
    Ok(())
}

enum OpenedZipEntry {
    Directory,
    File(File, Metadata),
}

enum OpenedTicketTarget {
    Directory(Arc<File>),
    File(File, Metadata),
}

/// Open and classify the ticket path once. Metadata and bytes now come from
/// the same no-follow descriptor: a file swapped to a symlink cannot redirect
/// a plain download, and a folder walk inherits this already-anchored root.
async fn open_ticket_target(path: PathBuf) -> anyhow::Result<OpenedTicketTarget> {
    tokio::task::spawn_blocking(move || {
        let file = open_nofollow(&path, Beneath::Any)?;
        let metadata = file.metadata()?;
        if metadata.is_dir() {
            Ok(OpenedTicketTarget::Directory(Arc::new(file)))
        } else if metadata.is_file() {
            Ok(OpenedTicketTarget::File(file, metadata))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ticket path is not a regular file or directory",
            ))
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("download open task failed: {e}"))?
    .map_err(Into::into)
}

/// List a directory opened beneath `root`. The raw-name cap is conservative:
/// symlinks and special files count while enumerating so an attacker cannot
/// use millions of skipped entries to defeat the archive's memory ceiling.
async fn list_zip_directory(
    root: Arc<File>,
    relative: PathBuf,
    remaining: usize,
) -> anyhow::Result<Option<(Metadata, Vec<OsString>)>> {
    tokio::task::spawn_blocking(move || {
        let directory = match open_beneath(&root, &relative, Beneath::Dir) {
            Ok(directory) => directory,
            Err(_) => return Ok(None),
        };
        let metadata = directory.metadata()?;
        let mut names = Vec::new();
        let mut retained_name_bytes = 0;
        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            let mut listing = match rustix::fs::Dir::read_from(&directory) {
                Ok(listing) => listing,
                Err(_) => return Ok(None),
            };
            while let Some(entry) = listing.read() {
                let Ok(entry) = entry else {
                    break; // unreadable remainder: preserve the usable prefix
                };
                let bytes = entry.file_name().to_bytes();
                if bytes == b"." || bytes == b".." {
                    continue;
                }
                if names.len() >= remaining {
                    anyhow::bail!("more than {MAX_ZIP_ENTRIES} entries — refusing to zip");
                }
                charge_directory_name_bytes(
                    &mut retained_name_bytes,
                    bytes.len(),
                    MAX_ZIP_DIRECTORY_NAME_BYTES,
                )?;
                names.push(OsStr::from_bytes(bytes).to_os_string());
            }
        }
        #[cfg(windows)]
        {
            let listing = match cap_dir(&directory).and_then(|dir| dir.entries()) {
                Ok(listing) => listing,
                Err(_) => return Ok(None),
            };
            for entry in listing {
                let Ok(entry) = entry else {
                    break; // unreadable remainder: preserve the usable prefix
                };
                let name = entry.file_name();
                if names.len() >= remaining {
                    anyhow::bail!("more than {MAX_ZIP_ENTRIES} entries — refusing to zip");
                }
                charge_directory_name_bytes(
                    &mut retained_name_bytes,
                    name.as_encoded_bytes().len(),
                    MAX_ZIP_DIRECTORY_NAME_BYTES,
                )?;
                names.push(name);
            }
        }
        Ok(Some((metadata, names)))
    })
    .await
    .map_err(|e| anyhow::anyhow!("zip directory task failed: {e}"))?
}

/// Safely classify and open one discovered name. O_NONBLOCK prevents a FIFO
/// or device planted during the walk from stalling a blocking worker; only
/// regular files and directories are retained.
async fn open_zip_entry(
    root: Arc<File>,
    relative: PathBuf,
) -> anyhow::Result<Option<OpenedZipEntry>> {
    tokio::task::spawn_blocking(move || {
        let file = match open_beneath(&root, &relative, Beneath::Any) {
            Ok(file) => file,
            Err(_) => return Ok(None),
        };
        let metadata = file.metadata()?;
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            Ok(Some(OpenedZipEntry::Directory))
        } else if file_type.is_file() {
            Ok(Some(OpenedZipEntry::File(file, metadata)))
        } else {
            Ok(None)
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("zip entry task failed: {e}"))?
}

/// What the last component of [`open_beneath`] / [`open_nofollow`] must be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Beneath {
    /// A directory.
    Dir,
    /// Anything, opened read-only (non-blocking on Unix, so a FIFO planted
    /// in the walk cannot park the worker); callers classify it by its
    /// metadata.
    Any,
}

#[cfg(unix)]
impl Beneath {
    fn flags(self) -> rustix::fs::OFlags {
        use rustix::fs::OFlags;
        match self {
            Beneath::Dir => OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Beneath::Any => OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        }
    }
}

/// Open `path` itself, refusing a link (symlink, or on Windows a junction
/// or any other name-surrogate reparse point) at its last component; the
/// components before it resolve as usual.
pub(crate) fn open_nofollow(path: &FsPath, kind: Beneath) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        Ok(File::from(rustix::fs::open(
            path,
            kind.flags(),
            rustix::fs::Mode::empty(),
        )?))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        // OPEN_REPARSE_POINT opens a link itself instead of its target, so
        // the metadata check below sees it; BACKUP_SEMANTICS lets a
        // directory open at all.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        refuse_links(file, kind)
    }
}

/// A directory opened by path, following links as `File::open` does: the
/// root a confined walk starts from. Windows needs backup semantics to open
/// a directory at all.
pub(crate) fn open_dir_path(path: &FsPath) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        File::open(path)
    }
    #[cfg(windows)]
    {
        cap_std::fs::Dir::open_ambient_dir(path, cap_std::ambient_authority())
            .map(cap_std::fs::Dir::into_std_file)
    }
}

/// `dir` (an open directory) as a cap-std directory: every path through it
/// resolves beneath it, and a link that would leave it is an error.
#[cfg(windows)]
pub(crate) fn cap_dir(dir: &File) -> std::io::Result<cap_std::fs::Dir> {
    Ok(cap_std::fs::Dir::from_std_file(dir.try_clone()?))
}

/// The last check of a Windows no-follow open: a handle opened on a link
/// itself is refused, and `Dir` must be a directory.
#[cfg(windows)]
fn refuse_links(file: File, kind: Beneath) -> std::io::Result<File> {
    let meta = file.metadata()?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "a symbolic link or junction (links are never followed here)",
        ));
    }
    if kind == Beneath::Dir && !meta.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "not a directory",
        ));
    }
    Ok(file)
}

/// Open `relative` by walking from an already-open root descriptor. Every
/// intermediate and final component is no-follow (`O_NOFOLLOW`; on Windows
/// a cap-std open that refuses junctions and other reparse points); no
/// path lookup is ever restarted from the process cwd after the root is
/// anchored.
pub(crate) fn open_beneath(root: &File, relative: &FsPath, kind: Beneath) -> std::io::Result<File> {
    if relative.as_os_str().is_empty() {
        return root.try_clone();
    }
    #[cfg(unix)]
    {
        let mut directory = root.try_clone()?;
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(non_normal());
            };
            let flags = if components.peek().is_none() {
                kind.flags()
            } else {
                Beneath::Dir.flags()
            };
            directory = File::from(
                rustix::fs::openat(&directory, name, flags, rustix::fs::Mode::empty())
                    .map_err(std::io::Error::from)?,
            );
        }
        Ok(directory)
    }
    #[cfg(windows)]
    {
        use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
        let mut directory = cap_dir(root)?;
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(non_normal());
            };
            if components.peek().is_some() {
                directory = directory.open_dir_nofollow(name)?;
                continue;
            }
            let file = match kind {
                Beneath::Dir => directory.open_dir_nofollow(name)?.into_std_file(),
                // A file open refuses a directory on Windows; retry as one,
                // keeping the first error for a name that is neither.
                Beneath::Any => {
                    let mut options = cap_std::fs::OpenOptions::new();
                    options.read(true).follow(FollowSymlinks::No);
                    match directory.open_with(name, &options) {
                        Ok(file) => file.into_std(),
                        Err(err) => match directory.open_dir_nofollow(name) {
                            Ok(sub) => sub.into_std_file(),
                            Err(_) => return Err(err),
                        },
                    }
                }
            };
            return refuse_links(file, kind);
        }
        Err(non_normal())
    }
}

fn non_normal() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "zip path contains a non-normal component",
    )
}

/// One filesystem name as a safe cross-platform ZIP path component. ZIP uses
/// `/` separators, but Windows extractors commonly also treat `\` as one;
/// replacing it (plus drive-colon/control characters) prevents a repository
/// filename such as `..\startup` or `C:` from becoming an extraction path.
fn safe_zip_component(name: &std::ffi::OsStr) -> String {
    name.to_string_lossy()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

fn zip_entry_name(root_name: &str, relative: &std::path::Path, directory: bool) -> String {
    let mut name = root_name.to_string();
    for component in relative.components() {
        if let std::path::Component::Normal(part) = component {
            name.push('/');
            name.push_str(&safe_zip_component(part));
        }
    }
    if directory {
        name.push('/');
    }
    name
}

/// The daemon host's current UTC offset in seconds, via `date +%z` (POSIX;
/// prints e.g. `-0700`). std exposes no timezone, so a subprocess it is —
/// one per folder download, noise next to streaming the archive. Falls back
/// to UTC (0) if `date` misbehaves.
#[cfg(unix)]
async fn local_utc_offset() -> i64 {
    match tokio::process::Command::new("date")
        .arg("+%z")
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            parse_utc_offset(String::from_utf8_lossy(&out.stdout).trim())
        }
        _ => 0,
    }
}

/// The daemon host's current UTC offset in seconds, from the system time
/// zone and whether daylight time is in effect now. UTC (0) if unknown.
#[cfg(windows)]
async fn local_utc_offset() -> i64 {
    use windows_sys::Win32::System::Time::{
        GetTimeZoneInformation, TIME_ZONE_ID_INVALID, TIME_ZONE_INFORMATION,
    };
    // SystemServices' names for these; not worth that feature for two values.
    const TIME_ZONE_ID_STANDARD: u32 = 1;
    const TIME_ZONE_ID_DAYLIGHT: u32 = 2;
    // SAFETY: TIME_ZONE_INFORMATION is plain data (integers and u16 arrays),
    // valid all-zero, and the call only writes into it.
    let mut tz: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    let id = unsafe { GetTimeZoneInformation(&mut tz) };
    // Bias is minutes west of UTC (UTC = local + bias).
    let bias = match id {
        TIME_ZONE_ID_INVALID => return 0,
        TIME_ZONE_ID_DAYLIGHT => tz.Bias + tz.DaylightBias,
        TIME_ZONE_ID_STANDARD => tz.Bias + tz.StandardBias,
        _ => tz.Bias,
    };
    -i64::from(bias) * 60
}

/// Parse a `[+-]HHMM` (or `[+-]HH:MM`) UTC offset into seconds; 0 on
/// anything unexpected.
#[cfg(any(unix, test))]
fn parse_utc_offset(s: &str) -> i64 {
    let (sign, rest) = match s.as_bytes().first() {
        Some(b'+') => (1, &s[1..]),
        Some(b'-') => (-1, &s[1..]),
        _ => return 0,
    };
    let digits = rest.replace(':', "");
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return 0;
    }
    let hours: i64 = digits[..2].parse().unwrap_or(0);
    let minutes: i64 = digits[2..].parse().unwrap_or(0);
    sign * (hours * 3600 + minutes * 60)
}

/// Stamp a zip entry with its file's mtime and unix mode. Left unstamped,
/// the writer's defaults extract as 1980-dated, mode-000 files (Info-ZIP
/// applies the zeroed attribute bits verbatim). The full st_mode goes in —
/// type bits like Info-ZIP writes them, so the executable bit survives on
/// scripts; `fallback` covers an unreadable metadata, and every entry on
/// Windows, which has no mode to carry.
fn stamp(
    builder: ZipEntryBuilder,
    meta: Option<&std::fs::Metadata>,
    utc_offset: i64,
    fallback: u32,
) -> ZipEntryBuilder {
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        meta.map(|m| m.permissions().mode()).unwrap_or(fallback)
    };
    #[cfg(windows)]
    let mode = fallback;
    let builder = builder
        .attribute_compatibility(AttributeCompatibility::Unix)
        .unix_permissions((mode & 0xFFFF) as u16);
    match meta.and_then(|m| m.modified().ok()) {
        Some(mtime) => builder.last_modification_date(zip_datetime(mtime, utc_offset)),
        None => builder,
    }
}

/// A file mtime as a zip DOS datetime in the daemon host's local time
/// (`utc_offset` seconds east of UTC): zip carries no timezone, and local
/// wall-clock is the format's convention, so extraction shows the time the
/// user saw on the file. One offset per archive, captured at download time —
/// entries last touched in a different DST phase skew by the DST hour (the
/// standard zip caveat), and a remote daemon stamps ITS host's local time.
/// DOS dates span 1980–2107 at 2-second granularity — out-of-range mtimes
/// clamp, and the builder itself masks the seconds' low bit.
fn zip_datetime(mtime: SystemTime, utc_offset: i64) -> ZipDateTime {
    let secs = mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        + utc_offset;
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    );
    let clamped = if year < 1980 {
        (1980, 1, 1, 0, 0, 0)
    } else if year > 2107 {
        (2107, 12, 31, 23, 59, 58)
    } else {
        (year, month, day, hour, minute, second)
    };
    ZipDateTimeBuilder::new()
        .year(clamped.0)
        .month(clamped.1)
        .day(clamped.2)
        .hour(clamped.3)
        .minute(clamped.4)
        .second(clamped.5)
        .build()
}

/// Proleptic-Gregorian civil date from days since 1970-01-01 — Howard
/// Hinnant's `civil_from_days`, the standard allocation-free algorithm
/// (keeps a date dependency out of the daemon). Shared with `history::usage`.
pub(crate) fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (
        (if month <= 2 { year + 1 } else { year }) as i32,
        month,
        day,
    )
}

/// RFC 6266 attachment disposition: a sanitized ASCII `filename` fallback
/// plus, when the name isn't pure ASCII, an RFC 5987 `filename*` carrying the
/// exact UTF-8 name percent-encoded.
fn content_disposition(name: &str) -> HeaderValue {
    let ascii: String = name
        .chars()
        .map(|c| match c {
            '"' | '\\' => '_',
            c if c.is_ascii_graphic() || c == ' ' => c,
            _ => '_',
        })
        .collect();
    let value = if name.is_ascii() {
        format!("attachment; filename=\"{ascii}\"")
    } else {
        format!(
            "attachment; filename=\"{ascii}\"; filename*=UTF-8''{}",
            pct_encode(name)
        )
    };
    // The fallback is ASCII-sanitized above, so this cannot fail; the
    // static value is a belt-and-braces default.
    HeaderValue::from_str(&value).unwrap_or(HeaderValue::from_static("attachment"))
}

/// Percent-encode everything outside RFC 5987 `attr-char` as UTF-8 bytes.
fn pct_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'!'
            | b'#'
            | b'$'
            | b'&'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_ascii_passthrough_and_escaping() {
        assert_eq!(
            content_disposition("plot.png").to_str().unwrap(),
            "attachment; filename=\"plot.png\""
        );
        // Quotes and backslashes can't appear in a quoted-string fallback.
        assert_eq!(
            content_disposition("a\"b\\c.txt").to_str().unwrap(),
            "attachment; filename=\"a_b_c.txt\""
        );
    }

    #[test]
    fn content_disposition_non_ascii_gets_rfc5987_name() {
        let value = content_disposition("å plot.png");
        let value = value.to_str().unwrap();
        assert!(value.contains("filename=\"_ plot.png\""), "{value}");
        assert!(
            value.contains("filename*=UTF-8''%C3%A5%20plot.png"),
            "{value}"
        );
    }

    #[test]
    fn zip_entry_names_cannot_encode_windows_traversal_or_drive_paths() {
        // On Unix `\` is a filename byte; on Windows it separates, and the
        // `..` it makes is dropped like any non-normal component.
        assert_eq!(
            zip_entry_name(
                &safe_zip_component(std::ffi::OsStr::new("C:")),
                std::path::Path::new(r"..\startup.cmd"),
                false,
            ),
            if cfg!(windows) {
                "C_/startup.cmd"
            } else {
                "C_/.._startup.cmd"
            }
        );
        assert_eq!(
            zip_entry_name("folder", std::path::Path::new("sub/file.txt"), false),
            "folder/sub/file.txt"
        );
    }

    #[test]
    fn directory_budget_is_charged_before_the_path_is_queued() {
        let mut stack = Vec::new();
        let mut budget = ZipEntryBudget::new(2);
        let mut paths = ZipPathBudget::new(usize::MAX);

        queue_zip_directory(&mut stack, &mut budget, &mut paths, PathBuf::from("root")).unwrap();
        queue_zip_directory(&mut stack, &mut budget, &mut paths, PathBuf::from("child")).unwrap();
        let err = queue_zip_directory(
            &mut stack,
            &mut budget,
            &mut paths,
            PathBuf::from("too-wide"),
        )
        .unwrap_err();

        assert_eq!(
            stack.len(),
            2,
            "a rejected directory must not grow the stack"
        );
        assert!(err.to_string().contains("more than 2 entries"), "{err}");
    }

    #[test]
    fn traversal_retained_bytes_are_bounded_before_allocation_is_queued() {
        let mut stack = Vec::new();
        let mut entries = ZipEntryBudget::new(10);
        let mut paths = ZipPathBudget::new(8);

        queue_zip_directory(&mut stack, &mut entries, &mut paths, PathBuf::from("1234")).unwrap();
        let err = queue_zip_directory(&mut stack, &mut entries, &mut paths, PathBuf::from("56789"))
            .unwrap_err();
        assert_eq!(stack, [PathBuf::from("1234")]);
        assert!(err.to_string().contains("8 bytes of queued paths"), "{err}");

        let mut retained_names = 4;
        let err = charge_directory_name_bytes(&mut retained_names, 5, 8).unwrap_err();
        assert_eq!(retained_names, 4, "a rejected name must not consume budget");
        assert!(err.to_string().contains("8 retained bytes"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_walk_cannot_escape_after_root_path_is_swapped() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "chimaera-zip-descriptor-{}-{nonce}",
            std::process::id()
        ));
        let root = base.join("root");
        let anchored = base.join("anchored");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("safe.txt"), "safe").unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();

        let root_fd = File::from(
            rustix::fs::open(
                &root,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .unwrap(),
        );
        std::fs::rename(&root, &anchored).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();

        assert!(open_beneath(&root_fd, FsPath::new("safe.txt"), Beneath::Any).is_ok());
        assert!(open_beneath(&root_fd, FsPath::new("secret.txt"), Beneath::Any).is_err());

        std::fs::remove_file(&root).unwrap();
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A junction is Windows' unprivileged directory link: one inside the
    /// root that points out is never walked through, nor opened itself.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_junction_never_leads_out_of_the_root() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "chimaera-zip-junction-{}-{nonce}",
            std::process::id()
        ));
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("sub/safe.txt"), "safe").unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        junction::create(&outside, root.join("out")).unwrap();
        assert!(
            root.join("out/secret.txt").exists(),
            "the junction resolves"
        );

        let root_fd = open_dir_path(&root).unwrap();
        let ok = open_beneath(&root_fd, FsPath::new("sub/safe.txt"), Beneath::Any).unwrap();
        assert!(ok.metadata().unwrap().is_file());
        let sub = open_beneath(&root_fd, FsPath::new("sub"), Beneath::Any).unwrap();
        assert!(
            sub.metadata().unwrap().is_dir(),
            "Any opens a directory too"
        );
        for (rel, kind) in [
            ("out/secret.txt", Beneath::Any),
            ("out", Beneath::Any),
            ("out", Beneath::Dir),
            ("sub/safe.txt", Beneath::Dir),
        ] {
            assert!(
                open_beneath(&root_fd, FsPath::new(rel), kind).is_err(),
                "{rel} as {kind:?}"
            );
        }
        assert!(open_nofollow(&root.join("out"), Beneath::Dir).is_err());
        assert!(open_nofollow(&root.join("sub"), Beneath::Dir).is_ok());

        // The Unix swap attack can't start here: Windows refuses to rename
        // a directory while a handle to it is open.
        let anchored = open_nofollow(&root.join("sub"), Beneath::Dir).unwrap();
        assert!(std::fs::rename(root.join("sub"), root.join("moved")).is_err());
        assert!(open_beneath(&anchored, FsPath::new("safe.txt"), Beneath::Any).is_ok());
        drop(anchored);

        // The zip walk lists the junction but never descends into it.
        let names =
            list_zip_directory(Arc::new(open_dir_path(&root).unwrap()), PathBuf::new(), 100)
                .await
                .unwrap()
                .unwrap()
                .1;
        assert!(names.iter().any(|n| n == "out"), "{names:?}");
        let entry = open_zip_entry(
            Arc::new(open_dir_path(&root).unwrap()),
            PathBuf::from("out"),
        )
        .await
        .unwrap();
        assert!(entry.is_none(), "a junction is skipped like a symlink");

        assert!(outside.join("secret.txt").exists());
        // Windows won't remove a folder something still holds open.
        drop((ok, sub, root_fd));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29)); // leap day
    }

    #[test]
    fn zip_datetime_carries_mtime_and_clamps_the_dos_floor() {
        // 2024-03-01T04:05:06Z.
        let mtime =
            UNIX_EPOCH + std::time::Duration::from_secs(19_783 * 86_400 + 4 * 3600 + 5 * 60 + 6);
        let dt = zip_datetime(mtime, 0);
        assert_eq!(
            (
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second()
            ),
            (2024, 3, 1, 4, 5, 6), // even second survives DOS 2s granularity
        );
        // A negative UTC offset shifts across midnight into the previous
        // local day (04:05:06Z at UTC-7 is 21:05:06 the day before).
        let dt = zip_datetime(mtime, -7 * 3600);
        assert_eq!(
            (dt.year(), dt.month(), dt.day(), dt.hour()),
            (2024, 2, 29, 21),
        );
        // Pre-1980 mtimes clamp to the DOS floor instead of panicking the
        // builder (its year field is `year - 1980` in a u16).
        let dt = zip_datetime(UNIX_EPOCH, 0);
        assert_eq!((dt.year(), dt.month(), dt.day()), (1980, 1, 1));
        // ... even when the offset would push the wall clock below 1970.
        let dt = zip_datetime(UNIX_EPOCH, -7 * 3600);
        assert_eq!((dt.year(), dt.month(), dt.day()), (1980, 1, 1));
    }

    #[test]
    fn parse_utc_offset_handles_common_forms() {
        assert_eq!(parse_utc_offset("-0700"), -7 * 3600);
        assert_eq!(parse_utc_offset("+0200"), 2 * 3600);
        assert_eq!(parse_utc_offset("+05:30"), 5 * 3600 + 30 * 60);
        assert_eq!(parse_utc_offset("+0000"), 0);
        // Garbage (an empty or word answer) falls back to UTC.
        assert_eq!(parse_utc_offset(""), 0);
        assert_eq!(parse_utc_offset("UTC"), 0);
        assert_eq!(parse_utc_offset("+07"), 0);
    }
}
