//! Filesystem endpoints: the folder picker (home + directories-only listing),
//! and the file service backing file tabs — full directory listings, ranged
//! raw reads, atomic single-file writes (lightweight editing), file management
//! (create/rename/delete behind the file-manager context menus), server-
//! rendered markdown, paged CSV/TSV tables (with a transparent gzip tier for
//! .gz/.bgz), and short-lived tickets that let iframes/img tags fetch bytes
//! without a bearer header.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::Context;
use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use flate2::read::{GzDecoder, MultiGzDecoder};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;

mod row_index;

/// Hard cap on a single `fs/file` read.
const MAX_FILE_CHUNK: u64 = 2 * 1024 * 1024;
/// Default `fs/file` read size (256KB).
const DEFAULT_FILE_CHUNK: u64 = 256 * 1024;
/// Hard cap on a `fs/file` PUT body — editing is for small text files;
/// anything bigger belongs in a real editor.
const MAX_WRITE_BYTES: usize = 1024 * 1024;
/// Hard cap on decompressed bytes consumed per gzip-backed request. Gzip has
/// no random access, so every read decodes sequentially from the start; this
/// bounds that work (and defuses gzip bombs) at the cost of an honest
/// "truncated" answer for very deep reads.
const MAX_GZ_DECOMPRESS: u64 = 64 * 1024 * 1024;
/// Maximum bytes one paged table request walks in a plain file (from the
/// nearest row-index checkpoint), so a hostile giant `offset_rows` cannot tie
/// up a blocking worker walking an arbitrarily large dataset in one go.
const MAX_TABLE_SCAN_BYTES: u64 = 64 * 1024 * 1024;
/// Largest markdown source `fs/markdown` will render.
const MAX_MARKDOWN_BYTES: u64 = 4 * 1024 * 1024;
/// Hard cap on `fs/table` rows per page.
const MAX_TABLE_ROWS: usize = 1000;
/// Largest spreadsheet `fs/xlsx` will parse, measured on the **on-disk
/// (zip-compressed) source**. Note this is NOT a hard RSS bound: calamine has no
/// lazy streaming, so it decompresses and materializes the whole sheet plus the
/// shared-strings table, and peak memory is a multiple of this figure (a highly
/// repetitive sheet compresses well and expands a lot). The cap keeps that
/// multiple bounded and the transient spike off the reactor (`spawn_blocking`);
/// an over-cap file gets an honest "too large" message. Typical result-table
/// spreadsheets are well under this; huge ones belong in a CSV export.
const MAX_XLSX_BYTES: u64 = 8 * 1024 * 1024;
/// Aggregate uncompressed ZIP payload accepted for xlsx/xlsm/ods previews.
/// A tiny highly-compressible workbook can otherwise expand far beyond the
/// source-size gate before calamine materializes its sheet and shared strings.
const MAX_XLSX_EXPANDED_BYTES: u64 = 64 * 1024 * 1024;
/// ZIP entry ceiling for the same preflight (bounds workbook structure work).
const MAX_XLSX_ENTRIES: usize = 4096;
/// Hard cap on candidates per `fs/validate` request (the UI batches one
/// request per visible-viewport scan).
const MAX_VALIDATE_CANDIDATES: usize = 50;
/// Longest `fs/validate` candidate considered: longer strings are not paths
/// an agent wrote, and each one costs a canonicalize per base.
const MAX_VALIDATE_CANDIDATE_BYTES: usize = 1024;
/// Most extra `bases` one `fs/validate` request may add after `base`.
const MAX_VALIDATE_BASES: usize = 8;
/// Most matches one ambiguous `fs/validate` candidate lists.
const MAX_AMBIGUOUS: usize = 5;
/// Most index matches existence-checked per `fs/validate` candidate.
const MAX_AMBIGUOUS_PROBES: usize = 8;
/// Wall-clock budget for one `fs/validate` request. With up to nine bases
/// and a diff-prefix retry, a 50-candidate batch is up to ~900 canonicalize
/// calls — seconds on a cold NFS mount — and link underlining is not worth
/// holding a limiter permit longer than this.
const VALIDATE_BUDGET: Duration = Duration::from_secs(5);
/// Hard cap on entries returned by a single `fs/dirs` / `fs/list` listing.
/// The daemon runs on shared login nodes over NFS/Lustre where a scratch dir
/// can hold hundreds of thousands of entries; without a ceiling a single
/// listing balloons the response and the allocation. Past this the answer is
/// honestly `truncated`.
pub(crate) const MAX_DIR_ENTRIES: usize = 1000;
/// How long a raw-access ticket stays valid.
const TICKET_TTL: Duration = Duration::from_secs(600);
/// In-memory capability ceiling. A buggy or hostile bearer-authenticated
/// client must not grow the ticket map without bound during that TTL.
const MAX_TICKETS: usize = 4096;
/// Shared ceiling for metadata-heavy and parser-heavy file requests. Tokio's
/// blocking pool can grow very large under a request burst; on NFS/Lustre that
/// turns one stalled mount into host-wide thread and syscall pressure. Queued
/// requests remain asynchronous, so terminals/chat/health stay responsive.
pub(crate) static FILESYSTEM_WORK: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

/// The daemon user's home directory (`$HOME`).
fn home_dir() -> anyhow::Result<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => Ok(PathBuf::from(home)),
        _ => anyhow::bail!("HOME is not set"),
    }
}

/// Expand a leading `~` to the user's home directory; other paths pass through.
fn expand_tilde(raw: &str) -> anyhow::Result<PathBuf> {
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }
    Ok(PathBuf::from(raw))
}

/// Expand `~` and canonicalize; the error carries the pre-canonical path so
/// "$path: No such file or directory" reads naturally.
fn canonical(raw: &str) -> anyhow::Result<PathBuf> {
    let expanded = expand_tilde(raw)?;
    std::fs::canonicalize(&expanded).with_context(|| expanded.display().to_string())
}

/// Resolve `raw` WITHOUT following a final symlink: `~` expanded, the parent
/// canonicalized (it must exist), the leaf name kept as given. This is the
/// resolution rename/delete need — `canonical()` would resolve a symlink to
/// its target, and deleting a symlink must never delete what it points at.
/// The leaf itself may or may not exist; callers check.
fn canonical_parent_join(raw: &str) -> anyhow::Result<PathBuf> {
    let expanded = expand_tilde(raw)?;
    let name = expanded
        .file_name()
        .map(|n| n.to_os_string())
        .with_context(|| format!("{} has no file name", expanded.display()))?;
    let parent = match expanded.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => anyhow::bail!("{} has no parent directory", expanded.display()),
    };
    let parent = std::fs::canonicalize(parent).with_context(|| parent.display().to_string())?;
    Ok(parent.join(name))
}

/// Canonicalize `raw` and require a regular file (not a directory).
fn canonical_file(raw: &str) -> anyhow::Result<PathBuf> {
    let path = canonical(raw)?;
    if !path.is_file() {
        anyhow::bail!("{} is not a file", path.display());
    }
    Ok(path)
}

/// True when the path names a gzip stream: `.gz`, or bgzip's `.bgz`. BGZF is
/// standard multi-member gzip, so sequential multi-member decode covers it
/// (block-level random access is a later wave).
fn is_gzip_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz") || ext.eq_ignore_ascii_case("bgz"))
}

/// The name a gzip file decompresses to, judged from its path:
/// `foo.tsv.gz` -> `foo.tsv`.
fn gz_inner_from_path(path: &Path) -> Option<String> {
    path.file_stem().map(|s| s.to_string_lossy().into_owned())
}

/// The stored FNAME of the first gzip member, if the compressor recorded one
/// (`gzip file.tsv` does; pipelines often don't).
fn gz_inner_from_header(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut decoder = GzDecoder::new(file);
    if decoder.header().is_none() {
        // Header parsing is lazy in flate2's read decoder; a short read
        // forces it. Errors just mean "no name to sniff".
        let mut probe = [0u8; 1];
        let _ = decoder.read(&mut probe);
    }
    let name = decoder.header()?.filename()?;
    std::str::from_utf8(name).ok().map(str::to_owned)
}

/// Content type for a gzip file, from the inner (decompressed) name: the path
/// minus its .gz/.bgz suffix, falling back to the member FNAME, else
/// octet-stream. `foo.tsv.gz` reads as a TSV, not as a gzip blob.
fn gz_mime(path: &Path) -> mime_guess::Mime {
    let guess = |name: String| mime_guess::from_path(Path::new(&name)).first();
    gz_inner_from_path(path)
        .and_then(guess)
        .or_else(|| gz_inner_from_header(path).and_then(guess))
        .unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM)
}

/// File-version fingerprint as an opaque decimal token for the `X-Mtime`
/// header and PUT `expect_mtime` conflict check. mtime remains the primary
/// signal, but length plus Unix inode/ctime identity catch a same-size rewrite
/// whose timestamp was preserved or rounded by a coarse shared filesystem.
///
/// Clients hold tokens across daemon restarts and upgrades, so the digest must
/// be stable across Rust releases: SHA-256 over fixed-width little-endian
/// fields, never `DefaultHasher` (whose algorithm std may change).
fn mtime_token(meta: &std::fs::Metadata) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    hasher.update(modified.to_le_bytes());
    hasher.update(meta.len().to_le_bytes());
    hasher.update([u8::from(meta.is_file()), u8::from(meta.is_dir())]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hasher.update(meta.dev().to_le_bytes());
        hasher.update(meta.ino().to_le_bytes());
        hasher.update(meta.ctime().to_le_bytes());
        hasher.update(meta.ctime_nsec().to_le_bytes());
    }
    let digest = hasher.finalize();
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(head).max(1).to_string()
}

/// Lowercase hex SHA-256 of `bytes` — the `X-Content-Hash` / `expect_hash`
/// version of a file's contents.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex_lower(&Sha256::digest(bytes))
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// A header value from a token/hash we minted (ASCII digits or hex).
fn ascii_header(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).unwrap_or(HeaderValue::from_static("0"))
}

/// 400 with a JSON error body.
fn bad_request(err: &anyhow::Error) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": format!("{err:#}")})),
    )
        .into_response()
}

/// GET /api/v1/fs/home
pub(crate) async fn home() -> Response {
    match home_dir() {
        Ok(path) => Json(json!({"path": path.to_string_lossy()})).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct DirsQuery {
    path: String,
    #[serde(default)]
    hidden: bool,
}

/// One subdirectory in a `fs/dirs` listing.
#[derive(Serialize)]
struct DirEntry {
    name: String,
    path: String,
}

/// GET /api/v1/fs/dirs?path=<path>&hidden=<bool>
pub(crate) async fn dirs(Query(query): Query<DirsQuery>) -> Response {
    blocking_json(move || list_dirs(&query.path, query.hidden)).await
}

/// Run JSON-producing filesystem/preview work on a blocking thread. NFS and
/// Lustre can stall even a metadata lookup, while gzip/markdown parsing is
/// CPU-heavy; neither belongs on a Tokio worker.
async fn blocking_json<F>(work: F) -> Response
where
    F: FnOnce() -> anyhow::Result<serde_json::Value> + Send + 'static,
{
    let permit = FILESYSTEM_WORK
        .acquire()
        .await
        .expect("filesystem work semaphore is never closed");
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(err)) => bad_request(&err),
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("filesystem task failed: {join}")})),
        )
            .into_response(),
    }
}

/// Response-shaped companion to [`blocking_json`] for byte-range reads (and
/// any other route whose blocking filesystem work must share the limiter).
pub(crate) async fn blocking_response<F>(work: F) -> Response
where
    F: FnOnce() -> anyhow::Result<Response> + Send + 'static,
{
    blocking_response_on(&FILESYSTEM_WORK, work).await
}

/// [`blocking_response`] under another limiter: a subsystem whose bursts
/// must never take the shared `FILESYSTEM_WORK` permits (the draft mirror).
pub(crate) async fn blocking_response_on<F>(
    limiter: &'static tokio::sync::Semaphore,
    work: F,
) -> Response
where
    F: FnOnce() -> anyhow::Result<Response> + Send + 'static,
{
    let permit = limiter
        .acquire()
        .await
        .expect("filesystem work semaphore is never closed");
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => bad_request(&err),
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("filesystem task failed: {join}")})),
        )
            .into_response(),
    }
}

/// Whether a directory entry is (or resolves to) a directory, preferring the
/// `readdir`-provided d_type so the common case costs no extra syscall. Only a
/// symlink — or a filesystem that reports an unknown type — falls back to a
/// `metadata()` stat, which follows the link so symlinks-to-dirs still count.
/// This is what defuses the per-entry stat storm on a big NFS/Lustre listing.
fn entry_is_dir(entry: &std::fs::DirEntry) -> bool {
    match entry.file_type() {
        Ok(ft) if ft.is_dir() => true,
        Ok(ft) if ft.is_file() => false,
        _ => std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()),
    }
}

/// Canonicalize `raw` (after tilde expansion) and list its subdirectories:
/// directories and symlinks resolving to directories only, dotted names
/// excluded unless `hidden`, sorted case-insensitively by name, capped at
/// [`MAX_DIR_ENTRIES`].
fn list_dirs(raw: &str, hidden: bool) -> anyhow::Result<serde_json::Value> {
    let path = canonical(raw)?;
    if !path.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }

    let entries = std::fs::read_dir(&path)
        .with_context(|| format!("{}: failed to read directory", path.display()))?;
    let mut dirs = Vec::new();
    let mut truncated = false;
    for entry in entries {
        // Unreadable entries are skipped silently.
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        if !entry_is_dir(&entry) {
            continue;
        }
        dirs.push(DirEntry {
            name,
            path: entry.path().to_string_lossy().into_owned(),
        });
        if dirs.len() > MAX_DIR_ENTRIES {
            dirs.pop();
            truncated = true;
            break;
        }
    }
    dirs.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(json!({
        "path": path.to_string_lossy(),
        "parent": path.parent().map(|p| p.to_string_lossy()),
        "dirs": dirs,
        "truncated": truncated,
    }))
}

/// One entry in a `fs/list` listing.
#[derive(Serialize)]
struct FsEntry {
    name: String,
    path: String,
    kind: &'static str,
    size: u64,
    /// Modification time as seconds since the unix epoch (0 if unavailable).
    mtime: u64,
    /// This entry is a symlink. Additive + skip-when-false: absent on old
    /// daemons, where the client reads it as a regular entry. `kind` still
    /// reflects the RESOLVED target (a symlink-to-dir is `"dir"`), so
    /// navigation is unchanged.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    symlink: bool,
    /// The raw link text (`readlink`), for the "→ target" hover. Present only
    /// on symlinks.
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    /// A symlink whose target does not resolve (dangling). Emitted as
    /// `kind: "file"` so the wire union stays "dir"|"file"; the client shows
    /// it distinctly and refuses to open it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    broken: bool,
}

/// GET /api/v1/fs/list?path=<path>&hidden=<bool> — full directory listing
/// (dirs and files) for the file tree.
pub(crate) async fn list(Query(query): Query<DirsQuery>) -> Response {
    blocking_json(move || list_entries(&query.path, query.hidden)).await
}

/// List all entries of a directory: dirs first then files, each group sorted
/// case-insensitively; dot entries excluded unless `hidden`; unreadable
/// entries (including broken symlinks) skipped; capped at [`MAX_DIR_ENTRIES`].
/// Unlike `list_dirs` this needs each entry's size + mtime, so it stats — the
/// cap is what bounds that on a huge directory.
fn list_entries(raw: &str, hidden: bool) -> anyhow::Result<serde_json::Value> {
    let path = canonical(raw)?;
    if !path.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }

    let read = std::fs::read_dir(&path)
        .with_context(|| format!("{}: failed to read directory", path.display()))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in read {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        let entry_path = entry.path();
        // d_type from readdir (no extra syscall on the common filesystems);
        // symlink-ness is the link itself, unlike the following metadata().
        let is_link = entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false);
        let target = is_link.then(|| {
            std::fs::read_link(&entry_path)
                .map(|t| t.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        // metadata() follows symlinks. A dangling symlink stats as an error:
        // rather than dropping it (invisible, unremovable from the UI), emit
        // it as a broken file entry — delete/rename act on the link itself.
        let Ok(meta) = std::fs::metadata(&entry_path) else {
            if is_link {
                let mtime = std::fs::symlink_metadata(&entry_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                entries.push(FsEntry {
                    name,
                    path: entry_path.to_string_lossy().into_owned(),
                    kind: "file",
                    size: 0,
                    mtime,
                    symlink: true,
                    target,
                    broken: true,
                });
                if entries.len() > MAX_DIR_ENTRIES {
                    entries.pop();
                    truncated = true;
                    break;
                }
            }
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        entries.push(FsEntry {
            name,
            path: entry_path.to_string_lossy().into_owned(),
            kind: if meta.is_dir() { "dir" } else { "file" },
            size: meta.len(),
            mtime,
            symlink: is_link,
            target,
            broken: false,
        });
        if entries.len() > MAX_DIR_ENTRIES {
            entries.pop();
            truncated = true;
            break;
        }
    }
    entries.sort_by(|a, b| {
        (a.kind != "dir")
            .cmp(&(b.kind != "dir"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(json!({
        "path": path.to_string_lossy(),
        "parent": path.parent().map(|p| p.to_string_lossy()),
        "entries": entries,
        "truncated": truncated,
    }))
}

#[derive(Deserialize)]
pub(crate) struct FileQuery {
    path: String,
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    limit: Option<u64>,
}

/// GET /api/v1/fs/file?path=&offset=0&limit=262144 — raw bytes of a slice of
/// the file, with `X-File-Size` (total size), `X-Truncated` (whether bytes
/// remain past this slice), and `X-Mtime` (opaque modification token, echoed
/// back by PUT's `expect_mtime`) headers. `limit` is capped at 2MB. When the
/// body is the WHOLE raw file (offset 0, nothing left past it, not a gzip
/// decode) and no write raced the read (the token re-checked after it, one
/// retry) it also carries `X-Content-Hash`: the lowercase hex SHA-256 of
/// exactly these bytes, echoed back by PUT's `expect_hash`.
///
/// `.gz`/`.bgz` paths are decompressed transparently: `offset`/`limit` then
/// address DECOMPRESSED bytes (sequential decode, capped), the Content-Type
/// comes from the inner name, and `X-File-Size` is only present once the
/// total decompressed size is known (i.e. this slice reached EOF).
pub(crate) async fn file(Query(query): Query<FileQuery>) -> Response {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_FILE_CHUNK)
        .min(MAX_FILE_CHUNK);
    blocking_response(move || read_file_response(&query.path, query.offset, limit)).await
}

/// Build the `fs/file` response for a plain or gzip-compressed file.
fn read_file_response(raw: &str, offset: u64, limit: u64) -> anyhow::Result<Response> {
    let path = canonical_file(raw)?;

    let (mime, total, bytes, truncated, mtime, hash) = if is_gzip_path(&path) {
        let meta = std::fs::metadata(&path)
            .with_context(|| format!("{}: failed to stat", path.display()))?;
        let (total, bytes, more) = read_gz_slice(&path, offset, limit)?;
        (gz_mime(&path), total, bytes, more, mtime_token(&meta), None)
    } else {
        let slice = read_file_slice(&path, offset, limit)?;
        let hash = slice.whole_file_hash(offset);
        let mime = mime_guess::from_path(&path).first_or_octet_stream();
        (
            mime,
            Some(slice.total),
            slice.bytes,
            !slice.eof,
            slice.mtime,
            hash,
        )
    };

    let mut response = (
        [
            (header::CONTENT_TYPE, mime.essence_str().to_string()),
            (
                HeaderName::from_static("x-truncated"),
                truncated.to_string(),
            ),
            (HeaderName::from_static("x-mtime"), mtime),
        ],
        bytes,
    )
        .into_response();
    let headers = response.headers_mut();
    if let Some(total) = total {
        headers.insert(
            HeaderName::from_static("x-file-size"),
            ascii_header(&total.to_string()),
        );
    }
    if let Some(hash) = hash {
        headers.insert(
            HeaderName::from_static("x-content-hash"),
            ascii_header(&hash),
        );
    }
    Ok(response)
}

/// One plain-file read: the bytes, whether they reach EOF, the file size, and
/// the version token of the descriptor they were read from.
struct FileSlice {
    bytes: Vec<u8>,
    /// Nothing remains past this slice (observed by reading, not by `stat`).
    eof: bool,
    total: u64,
    /// The token from BEFORE the read: if a write raced it, the client's
    /// watch sees the file move past this and reads again.
    mtime: String,
    /// The token was the same after the read: `bytes` are exactly the
    /// version `mtime` names.
    stable: bool,
}

impl FileSlice {
    /// `X-Content-Hash`, only for a body that IS the file (a client echoes it
    /// as `expect_hash`, and a slice's hash would never match the disk) and
    /// only when no write raced the read (a hash of one version must never
    /// travel with the token of another). Otherwise the client falls back to
    /// the token.
    fn whole_file_hash(&self, offset: u64) -> Option<String> {
        (offset == 0 && self.eof && self.stable).then(|| sha256_hex(&self.bytes))
    }
}

/// Read up to `limit` bytes of the (canonical) file at `path` starting at
/// `offset`. EOF is judged by reading one byte past the slice rather than by
/// the size `fstat` reported, so a file that grows or shrinks between the two
/// can never earn a whole-file hash for a partial body. The token comes from
/// an fstat before the read, so a second fstat after it checks that no write
/// landed in between; on a change the read is retried once (reopened: a
/// rename-replace is a new inode), and a second change leaves the slice
/// unstable (no content hash).
fn read_file_slice(path: &Path, offset: u64, limit: u64) -> anyhow::Result<FileSlice> {
    read_file_slice_racing(path, offset, limit, &mut || {})
}

/// [`read_file_slice`] with `racer` run between each attempt's first fstat
/// and its read — the window a concurrent writer can hit (tests use it).
fn read_file_slice_racing(
    path: &Path,
    offset: u64,
    limit: u64,
    racer: &mut dyn FnMut(),
) -> anyhow::Result<FileSlice> {
    let slice = read_file_slice_once(path, offset, limit, racer)?;
    if slice.stable {
        return Ok(slice);
    }
    read_file_slice_once(path, offset, limit, racer)
}

fn read_file_slice_once(
    path: &Path,
    offset: u64,
    limit: u64,
    racer: &mut dyn FnMut(),
) -> anyhow::Result<FileSlice> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("{}: failed to open", path.display()))?;
    let meta = file
        .metadata()
        .with_context(|| format!("{}: failed to stat", path.display()))?;
    racer();
    let stat_len = meta.len();
    let probe = limit.saturating_add(1);
    let mut bytes = Vec::with_capacity(
        usize::try_from(probe.min(stat_len.saturating_sub(offset) + 1)).unwrap_or(0),
    );
    // Past the end there is nothing to read (and a huge offset would only
    // make lseek fail): an empty, non-truncated slice, as always.
    if offset <= stat_len {
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("{}: failed to seek", path.display()))?;
        (&mut file)
            .take(probe)
            .read_to_end(&mut bytes)
            .with_context(|| format!("{}: failed to read", path.display()))?;
    }
    let eof = bytes.len() as u64 <= limit;
    if !eof {
        bytes.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    let read_end = offset.saturating_add(bytes.len() as u64);
    let total = if eof && !bytes.is_empty() {
        read_end
    } else if eof && offset == 0 {
        0
    } else if eof {
        // Past EOF: the read proves only that the file ends at or before `offset`.
        stat_len.min(offset)
    } else {
        stat_len.max(read_end.saturating_add(1))
    };
    let mtime = mtime_token(&meta);
    let after = file
        .metadata()
        .with_context(|| format!("{}: failed to stat", path.display()))?;
    Ok(FileSlice {
        bytes,
        eof,
        total,
        stable: mtime_token(&after) == mtime,
        mtime,
    })
}

/// Sequentially decode the gzip file at `path`, skipping `offset`
/// decompressed bytes and returning up to `limit` more. Multi-member streams
/// (bgzip/BGZF, concatenated gzips) decode transparently. Returns the total
/// decompressed size when this read hit EOF (`None` while unknown), the
/// bytes, and whether more decompressed bytes remain.
fn read_gz_slice(
    path: &Path,
    offset: u64,
    limit: u64,
) -> anyhow::Result<(Option<u64>, Vec<u8>, bool)> {
    if offset > MAX_GZ_DECOMPRESS {
        anyhow::bail!(
            "{}: offset {offset} is beyond the {MAX_GZ_DECOMPRESS}-byte sequential decode cap for compressed files",
            path.display()
        );
    }
    let file =
        std::fs::File::open(path).with_context(|| format!("{}: failed to open", path.display()))?;
    let ctx = || format!("{}: failed to decompress", path.display());
    // flate2's read decoders buffer their input internally.
    let mut decoder = MultiGzDecoder::new(file);
    let skipped =
        std::io::copy(&mut (&mut decoder).take(offset), &mut std::io::sink()).with_context(ctx)?;
    if skipped < offset {
        // Offset past decompressed EOF: empty slice, and now the total is known.
        return Ok((Some(skipped), Vec::new(), false));
    }
    let mut bytes = Vec::new();
    (&mut decoder)
        .take(limit)
        .read_to_end(&mut bytes)
        .with_context(ctx)?;
    // A full slice may sit exactly at EOF; probe one byte to find out.
    let mut probe = [0u8; 1];
    let more = bytes.len() as u64 == limit && decoder.read(&mut probe).with_context(ctx)? > 0;
    let total = if more {
        None
    } else {
        Some(offset + bytes.len() as u64)
    };
    Ok((total, bytes, more))
}

#[derive(Deserialize)]
pub(crate) struct PutFileQuery {
    path: String,
    #[serde(default)]
    expect_mtime: Option<String>,
    #[serde(default)]
    expect_hash: Option<String>,
}

/// Largest file a PUT precondition hashes. A client can only hold the
/// `X-Content-Hash` of a file it received whole in one `fs/file` read, so a
/// bigger file can never match `expect_hash`; this also bounds the work a
/// stale PUT against a huge file can cause.
const MAX_HASH_BYTES: u64 = MAX_FILE_CHUNK;

/// What the client says it edited. `expect_hash` wins over `expect_mtime`.
#[derive(Clone, Copy)]
enum Precondition<'a> {
    None,
    Mtime(&'a str),
    Hash(&'a str),
}

/// Outcome of an attempted write.
enum WriteOutcome {
    /// The body's bytes are on disk: written by this call (`wrote`), or
    /// already there (an idempotent retry after a lost reply). Carries the
    /// file's `X-Mtime` token and `X-Content-Hash`.
    Written {
        mtime: String,
        hash: String,
        wrote: bool,
    },
    /// The precondition failed. Carries the disk's current token and hash
    /// when they are known (absent for a missing file; no hash past
    /// [`MAX_HASH_BYTES`]).
    Conflict {
        mtime: Option<String>,
        hash: Option<String>,
    },
}

/// PUT /api/v1/fs/file?path=&expect_hash=&expect_mtime= — write the raw
/// request body to the file, creating it if its parent directory exists. 204
/// on success with the new `X-Mtime` and `X-Content-Hash` (of the bytes now on
/// disk) so the editor can chain saves; 400 for directories, non-regular
/// files, dangling symlinks and missing parents; 413 over 1MB (editing is for
/// small text files); 409 `{"error":"file changed on disk"}` when the
/// precondition fails, with `X-Mtime`/`X-Content-Hash` describing the current
/// disk state when known.
///
/// Preconditions (neither = unconditional overwrite):
/// - `expect_hash` (a previous `X-Content-Hash`): the file's CURRENT bytes are
///   hashed — opening forces fresh attributes on NFS, unlike a cached `stat`.
///   A mismatch whose disk bytes already equal the body answers success
///   without writing, so a retry after a lost reply is a no-op, not a 409
///   against our own write. A missing file is a 409.
/// - `expect_mtime` (a previous `X-Mtime`): the metadata token must match.
pub(crate) async fn put_file(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PutFileQuery>,
    body: Bytes,
) -> Response {
    if body.len() > MAX_WRITE_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "error": format!(
                    "file too large to save ({} bytes, limit {MAX_WRITE_BYTES})",
                    body.len()
                )
            })),
        )
            .into_response();
    }
    let dirty_path = query.path.clone();
    let result = tokio::task::spawn_blocking(move || {
        let expect_hash = query.expect_hash.map(|h| h.to_ascii_lowercase());
        let pre = match (expect_hash.as_deref(), query.expect_mtime.as_deref()) {
            (Some(hash), _) => Precondition::Hash(hash),
            (None, Some(mtime)) => Precondition::Mtime(mtime),
            (None, None) => Precondition::None,
        };
        write_file(&query.path, &body, pre)
    })
    .await;
    match result {
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("file-write task failed: {join}")})),
        )
            .into_response(),
        Ok(Ok(WriteOutcome::Written { mtime, hash, wrote })) => {
            if wrote {
                // A save is a git-relevant change: nudge the workspace(s)
                // holding this path so the tree/panel refetch without any
                // polling (and watching windows re-probe it promptly).
                crate::git::mark_path_dirty(&state, &dirty_path).await;
            }
            let mut response = StatusCode::NO_CONTENT.into_response();
            let headers = response.headers_mut();
            headers.insert(HeaderName::from_static("x-mtime"), ascii_header(&mtime));
            headers.insert(
                HeaderName::from_static("x-content-hash"),
                ascii_header(&hash),
            );
            response
        }
        Ok(Ok(WriteOutcome::Conflict { mtime, hash })) => {
            let mut response = (
                StatusCode::CONFLICT,
                Json(json!({"error": "file changed on disk"})),
            )
                .into_response();
            let headers = response.headers_mut();
            if let Some(mtime) = mtime {
                headers.insert(HeaderName::from_static("x-mtime"), ascii_header(&mtime));
            }
            if let Some(hash) = hash {
                headers.insert(
                    HeaderName::from_static("x-content-hash"),
                    ascii_header(&hash),
                );
            }
            response
        }
        Ok(Err(err)) => bad_request(&err),
    }
}

/// The disk's current version of an existing file: its token, and its content
/// hash when it is at most [`MAX_HASH_BYTES`]. Reads through `file`, so the
/// answer describes that descriptor's inode.
fn file_version(file: &mut std::fs::File) -> std::io::Result<(String, Option<String>)> {
    use sha2::{Digest, Sha256};
    let meta = file.metadata()?;
    let token = mtime_token(&meta);
    if meta.len() > MAX_HASH_BYTES {
        return Ok((token, None));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut seen = 0u64;
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        seen += n as u64;
        if seen > MAX_HASH_BYTES {
            // Grew past the cap mid-read: no hash, same as a big file.
            return Ok((token, None));
        }
        hasher.update(&buf[..n]);
    }
    Ok((token, Some(hex_lower(&hasher.finalize()))))
}

/// [`file_version`] by path; `None` when the file does not exist.
fn path_version(path: &Path) -> anyhow::Result<Option<(String, Option<String>)>> {
    match std::fs::File::open(path) {
        Ok(mut file) => Ok(Some(file_version(&mut file).with_context(|| {
            format!("{}: failed to read current contents", path.display())
        })?)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            Err(anyhow::Error::new(err).context(format!("{}: failed to open", path.display())))
        }
    }
}

/// Judge `pre` against the disk's current state (`None` = missing). `Ok(())`
/// means go ahead and write; `Err(outcome)` is the answer instead — a
/// conflict, or success without a write when the disk already holds exactly
/// the body (the idempotent-retry case, `expect_hash` only).
fn judge(
    pre: Precondition<'_>,
    current: Option<(String, Option<String>)>,
    body_hash: &str,
) -> Result<(), WriteOutcome> {
    match pre {
        Precondition::None => Ok(()),
        Precondition::Mtime(expect) => match current {
            Some((mtime, _)) if mtime == expect => Ok(()),
            Some((mtime, hash)) => Err(WriteOutcome::Conflict {
                mtime: Some(mtime),
                hash,
            }),
            None => Err(WriteOutcome::Conflict {
                mtime: None,
                hash: None,
            }),
        },
        Precondition::Hash(expect) => match current {
            Some((_, Some(hash))) if hash == expect => Ok(()),
            Some((mtime, Some(hash))) if hash == body_hash => Err(WriteOutcome::Written {
                mtime,
                hash,
                wrote: false,
            }),
            Some((mtime, hash)) => Err(WriteOutcome::Conflict {
                mtime: Some(mtime),
                hash,
            }),
            None => Err(WriteOutcome::Conflict {
                mtime: None,
                hash: None,
            }),
        },
    }
}

/// The disk version `pre` needs to be judged: a stat for `expect_mtime`, the
/// full hash only for `expect_hash`, nothing without a precondition.
fn version_for(
    pre: Precondition<'_>,
    path: &Path,
) -> anyhow::Result<Option<(String, Option<String>)>> {
    match pre {
        Precondition::None => Ok(None),
        Precondition::Mtime(_) => match std::fs::metadata(path) {
            Ok(meta) => Ok(Some((mtime_token(&meta), None))),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => {
                Err(anyhow::Error::new(err).context(format!("{}: failed to stat", path.display())))
            }
        },
        Precondition::Hash(_) => path_version(path),
    }
}

/// Removes its temp file on drop unless disarmed, so every early return and
/// `?` after the temp exists cleans up (a full disk must not leave a partial
/// hidden file behind).
struct TempFile(Option<PathBuf>);

impl TempFile {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Longest file name most filesystems accept (NAME_MAX).
const MAX_NAME_BYTES: usize = 255;

/// The hidden temp sibling's name: `.{name}.{8 random}.tmp`, with `name`
/// shortened (at a UTF-8 boundary when it is UTF-8) so the whole stays within
/// [`MAX_NAME_BYTES`] — a 250-byte file name must still be saveable.
fn temp_name(name: &std::ffi::OsStr) -> std::ffi::OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let nonce = &chimaera_core::generate_token()[..8];
    let budget = MAX_NAME_BYTES - (".".len() + ".".len() + nonce.len() + ".tmp".len());
    let bytes = name.as_bytes();
    let mut cut = bytes.len().min(budget);
    // Never split a multi-byte character: back off past continuation bytes.
    while cut > 0 && cut < bytes.len() && (bytes[cut] & 0b1100_0000) == 0b1000_0000 {
        cut -= 1;
    }
    let mut out = Vec::with_capacity(cut + 14);
    out.push(b'.');
    out.extend_from_slice(&bytes[..cut]);
    out.push(b'.');
    out.extend_from_slice(nonce.as_bytes());
    out.extend_from_slice(b".tmp");
    std::ffi::OsString::from_vec(out)
}

/// Give the temp file the target's owner and group where the kernel allows.
/// A non-root daemon can never give a file away, but may move it to any group
/// it belongs to — the shared project directory case — so a refused full
/// `fchown` retries with the group alone. Failures are expected and ignored.
fn carry_owner(file: &std::fs::File, target: &std::fs::Metadata) {
    use std::os::unix::fs::MetadataExt;
    let Ok(now) = file.metadata() else { return };
    if now.uid() == target.uid() && now.gid() == target.gid() {
        return;
    }
    if std::os::unix::fs::fchown(file, Some(target.uid()), Some(target.gid())).is_err()
        && now.gid() != target.gid()
    {
        let _ = std::os::unix::fs::fchown(file, None, Some(target.gid()));
    }
}

/// fsync a directory so a completed rename survives a crash. Best effort:
/// some filesystems (and FUSE/NFS mounts) refuse to open or sync directories.
fn sync_dir(dir: &Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

/// Write `bytes` to the file at `raw` under `pre`.
///
/// Normally atomic: a hidden temp sibling (never visible in listings, even
/// transiently) is created with the target's permission bits (never more
/// permissive, even before the exact `fchmod`), given its owner/group where
/// allowed, written, fsynced, and renamed over the target; then the directory
/// is fsynced. A live symlink is written THROUGH (its target is replaced; the
/// link stays). Refuses directories, non-regular files, dangling symlinks
/// (replacing the link with a regular file would silently detach it) and paths
/// whose parent directory does not exist.
///
/// A target with other hard links is rewritten in place instead (see
/// [`write_in_place`]).
fn write_file(raw: &str, bytes: &[u8], pre: Precondition<'_>) -> anyhow::Result<WriteOutcome> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let expanded = expand_tilde(raw)?;
    let body_hash = sha256_hex(bytes);
    let (target, existing) = match std::fs::metadata(&expanded) {
        Ok(meta) if meta.is_dir() => {
            anyhow::bail!("{} is a directory", expanded.display());
        }
        Ok(meta) if !meta.is_file() => {
            anyhow::bail!("{} is not a regular file", expanded.display());
        }
        Ok(meta) => {
            let path =
                std::fs::canonicalize(&expanded).with_context(|| expanded.display().to_string())?;
            (path, Some(meta))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(&expanded).is_ok_and(|m| m.file_type().is_symlink()) {
                anyhow::bail!(
                    "{} is a symlink to a missing file; refusing to replace the link with a regular file",
                    expanded.display()
                );
            }
            // New file: the parent directory must already exist.
            let name = expanded
                .file_name()
                .map(|n| n.to_os_string())
                .with_context(|| format!("{} has no file name", expanded.display()))?;
            let parent = match expanded.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => anyhow::bail!("{} has no parent directory", expanded.display()),
            };
            let parent =
                std::fs::canonicalize(parent).with_context(|| parent.display().to_string())?;
            if !parent.is_dir() {
                anyhow::bail!("{} is not a directory", parent.display());
            }
            (parent.join(name), None)
        }
        Err(err) => {
            return Err(
                anyhow::Error::new(err).context(format!("{}: failed to stat", expanded.display()))
            );
        }
    };

    // First check: refuse before doing any write work.
    if let Err(outcome) = judge(pre, version_for(pre, &target)?, &body_hash) {
        return Ok(outcome);
    }

    if existing.as_ref().is_some_and(|meta| meta.nlink() > 1) {
        return write_in_place(&target, bytes, pre, body_hash);
    }

    let parent = target
        .parent()
        .with_context(|| format!("{} has no parent directory", target.display()))?;
    let name = target
        .file_name()
        .with_context(|| format!("{} has no file name", target.display()))?;
    let tmp = parent.join(temp_name(name));
    let mode = existing
        .as_ref()
        .map_or(0o666, |meta| meta.permissions().mode() & 0o7777);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .with_context(|| format!("failed to create {}", tmp.display()))?;
    let mut guard = TempFile(Some(tmp.clone()));
    if let Some(meta) = &existing {
        // Owner first: a successful chown clears setuid/setgid bits, which
        // the exact chmod below then restores. The open's mode was filtered by
        // the umask; the chmod restores bits it dropped (e.g. group write on a
        // shared file). Best-effort: a failure still leaves a correct write.
        carry_owner(&file, meta);
        if let Err(err) = file.set_permissions(meta.permissions()) {
            tracing::warn!(path = %tmp.display(), %err, "failed to carry permissions onto tmp file");
        }
    }
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    drop(file);

    // Second check, as late as possible: another writer may have landed
    // while the temp was written and synced. What remains is the gap between
    // this read and rename(2) itself — POSIX has no compare-and-rename, and
    // NFS offers no lock every writer honours — so a write landing in those
    // microseconds is still replaced.
    if let Err(outcome) = judge(pre, version_for(pre, &target)?, &body_hash) {
        return Ok(outcome);
    }
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("failed to rename into {}", target.display()))?;
    guard.disarm();
    sync_dir(parent);

    let meta = std::fs::metadata(&target)
        .with_context(|| format!("{}: failed to stat after write", target.display()))?;
    Ok(WriteOutcome::Written {
        mtime: mtime_token(&meta),
        hash: body_hash,
        wrote: true,
    })
}

/// Rewrite a hard-linked file in place. Renaming a temp over one name would
/// split it from its other links (they would keep the old bytes), so the
/// inode itself is overwritten, which also keeps owner, mode and ACLs. The
/// cost is atomicity: a reader can see a torn file mid-write, and a crash or
/// I/O error mid-write leaves one. Bytes are written before the truncate so
/// the file is never transiently empty. The precondition is judged on the
/// same descriptor right before writing.
fn write_in_place(
    target: &Path,
    bytes: &[u8],
    pre: Precondition<'_>,
    body_hash: String,
) -> anyhow::Result<WriteOutcome> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(target)
        .with_context(|| format!("{}: failed to open for writing", target.display()))?;
    let current =
        match pre {
            Precondition::None => None,
            Precondition::Mtime(_) => Some((
                mtime_token(
                    &file
                        .metadata()
                        .with_context(|| format!("{}: failed to stat", target.display()))?,
                ),
                None,
            )),
            Precondition::Hash(_) => Some(file_version(&mut file).with_context(|| {
                format!("{}: failed to read current contents", target.display())
            })?),
        };
    if let Err(outcome) = judge(pre, current, &body_hash) {
        return Ok(outcome);
    }
    let ctx = || format!("failed to write {} in place", target.display());
    file.seek(SeekFrom::Start(0)).with_context(ctx)?;
    file.write_all(bytes).with_context(ctx)?;
    file.set_len(bytes.len() as u64).with_context(ctx)?;
    file.sync_all().with_context(ctx)?;
    let meta = file
        .metadata()
        .with_context(|| format!("{}: failed to stat after write", target.display()))?;
    Ok(WriteOutcome::Written {
        mtime: mtime_token(&meta),
        hash: body_hash,
        wrote: true,
    })
}

#[cfg(test)]
mod slice_tests {
    use super::*;

    fn temp_file(tag: &str, contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-slice-{tag}-{}-{}",
            std::process::id(),
            &chimaera_core::generate_token()[..8]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, contents).unwrap();
        file
    }

    /// A write landing between the token's fstat and the read is caught by
    /// the fstat after it; one retry reads a consistent version, whose hash
    /// and token then travel together.
    #[test]
    fn a_raced_read_retries_once_and_pairs_hash_with_token() {
        let file = temp_file("retry", "one\n");
        let mut calls = 0;
        let slice = read_file_slice_racing(&file, 0, 1024, &mut || {
            calls += 1;
            if calls == 1 {
                std::fs::write(&file, "two, longer\n").unwrap();
            }
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert!(slice.stable);
        assert_eq!(slice.bytes, b"two, longer\n");
        assert_eq!(slice.mtime, mtime_token(&std::fs::metadata(&file).unwrap()));
        assert_eq!(slice.whole_file_hash(0), Some(sha256_hex(b"two, longer\n")));
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    /// Raced on the retry too: the slice keeps the pre-read token (so the
    /// client's watch re-reads) and carries no content hash at all.
    #[test]
    fn a_read_raced_twice_omits_the_content_hash() {
        let file = temp_file("twice", "v0\n");
        let mut calls = 0;
        let slice = read_file_slice_racing(&file, 0, 1024, &mut || {
            calls += 1;
            std::fs::write(&file, "v".repeat(calls + 3)).unwrap();
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert!(!slice.stable);
        assert_eq!(slice.whole_file_hash(0), None);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn an_undisturbed_whole_read_is_hashed_once() {
        let file = temp_file("calm", "calm\n");
        let mut calls = 0;
        let slice = read_file_slice_racing(&file, 0, 1024, &mut || calls += 1).unwrap();
        assert_eq!(calls, 1);
        assert!(slice.stable && slice.eof);
        assert_eq!(slice.whole_file_hash(0), Some(sha256_hex(b"calm\n")));
        // A partial body is never hashed as the file.
        let part = read_file_slice(&file, 0, 2).unwrap();
        assert_eq!(part.whole_file_hash(0), None);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }
}

#[cfg(test)]
mod write_tests {
    use super::*;

    /// Every exit between creating the temp and renaming it drops the guard
    /// armed, so no failure after that point can leave a hidden temp behind.
    #[test]
    fn temp_guard_removes_unless_disarmed() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-temp-guard-{}-{}",
            std::process::id(),
            &chimaera_core::generate_token()[..8]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dropped = dir.join(".a.tmp");
        std::fs::write(&dropped, b"x").unwrap();
        drop(TempFile(Some(dropped.clone())));
        assert!(!dropped.exists());

        let kept = dir.join(".b.tmp");
        std::fs::write(&kept, b"x").unwrap();
        let mut guard = TempFile(Some(kept.clone()));
        guard.disarm();
        drop(guard);
        assert!(kept.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn temp_names_fit_name_max_without_splitting_characters() {
        use std::os::unix::ffi::OsStrExt;
        let short = temp_name(std::ffi::OsStr::new("notes.md"));
        let short = short.to_str().unwrap();
        assert!(
            short.starts_with(".notes.md.") && short.ends_with(".tmp"),
            "{short}"
        );
        assert_eq!(short.len(), ".notes.md.".len() + 8 + ".tmp".len());

        // 254 bytes of two-byte characters: cut to fit, on a boundary.
        let long = "é".repeat(127);
        let name = temp_name(std::ffi::OsStr::new(&long));
        assert!(name.as_bytes().len() <= MAX_NAME_BYTES);
        assert!(name.to_str().is_some(), "split a character: {name:?}");
    }
}

#[derive(Deserialize)]
pub(crate) struct MarkdownQuery {
    path: String,
}

/// GET /api/v1/fs/markdown?path= — `{html, frontmatter}`: the file rendered
/// as sanitized GFM HTML (see [`markdown_to_html`] for what it carries), and
/// the raw text of a leading YAML frontmatter block (see [`frontmatter`];
/// delimiter lines excluded) or null.
pub(crate) async fn markdown(Query(query): Query<MarkdownQuery>) -> Response {
    blocking_json(move || {
        let text = read_markdown(&query.path)?;
        Ok(json!({
            "html": sanitize_markdown(&markdown_to_html(&text)),
            "frontmatter": frontmatter(&text).map(|f| f.inner),
        }))
    })
    .await
}

/// Read the markdown file at `raw` as (lossy) UTF-8. Files over 4MB are
/// rejected.
fn read_markdown(raw: &str) -> anyhow::Result<String> {
    let path = canonical_file(raw)?;
    let size = std::fs::metadata(&path)
        .with_context(|| format!("{}: failed to stat", path.display()))?
        .len();
    if size > MAX_MARKDOWN_BYTES {
        anyhow::bail!(
            "{} is too large to render as markdown ({size} bytes, limit {MAX_MARKDOWN_BYTES})",
            path.display()
        );
    }
    let bytes =
        std::fs::read(&path).with_context(|| format!("{}: failed to read", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// A leading YAML frontmatter block.
struct Frontmatter<'a> {
    /// The text between the delimiter lines, without the line break that
    /// ends the last one (empty lines inside are kept).
    inner: &'a str,
    /// How many lines of the document the block occupies, delimiters
    /// included.
    lines: usize,
}

/// The leading `---` frontmatter block, when there is one. Two rules must
/// agree for a block to count:
///
/// - comrak's front-matter split (`strings::split_off_front_matter`,
///   reproduced exactly: an optional BOM, `---` alone on line 1, closed by
///   the first line that is exactly `---` — a `\r\n` closer is searched
///   first, as comrak does), because comrak is what removes it from the
///   render;
/// - the live editor's stricter shape (`mdLive.ts` `frontmatterEnd`): the
///   closer within the first 200 lines and at least one `key:` line inside,
///   so a document that merely opens with a thematic break keeps it.
///
/// [`markdown_to_html`] enables comrak's extension only for a block that
/// passes both, and [`promote_math_blocks_mapped`] skips exactly the same
/// lines.
fn frontmatter(text: &str) -> Option<Frontmatter<'_>> {
    const DELIM: &str = "---";
    const MAX_LINES: usize = 200;
    let s = text.strip_prefix('\u{feff}').unwrap_or(text);
    let after_open = s.strip_prefix(DELIM)?;
    let open_len = DELIM.len()
        + if after_open.starts_with('\n') {
            1
        } else if after_open.starts_with("\r\n") {
            2
        } else {
            return None;
        };
    let body = &s[open_len..];
    let close_nl = body
        .find("\n---\r\n")
        .or_else(|| body.find("\n---\n"))
        .or_else(|| body.find("\n---"))?;
    let after_close = &body[close_nl + 1 + DELIM.len()..];
    if !(after_close.is_empty() || after_close.starts_with('\n') || after_close.starts_with("\r\n"))
    {
        return None;
    }
    let inner = &body[..close_nl];
    // The opener, at least one inner line (comrak needs the `\n` before the
    // closer, so `---\n---` is no block), and the closer.
    let lines = 3 + inner.matches('\n').count();
    if lines > MAX_LINES {
        return None;
    }
    // The editor's `/^[A-Za-z0-9_-]+\s*:/`.
    let keyed = inner.lines().any(|line| {
        line.split_once(':').is_some_and(|(key, _)| {
            let key = key.trim_end_matches([' ', '\t']);
            !key.is_empty()
                && key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
    });
    keyed.then(|| Frontmatter {
        inner: inner.strip_suffix('\r').unwrap_or(inner),
        lines,
    })
}

/// Promote `$$` BLOCKS to comrak's ```math fence before parsing. comrak's
/// dollar math is inline-only — it pairs within one paragraph — so an
/// equation wrapped as `+ \left(1-w\right)…` on its second line is cut by
/// the bullet list that line starts. The fence keeps the interior raw,
/// exactly what the live editor's block parser does (`previews/mdMath.ts`
/// owns the grammar; its cases live in `previews/mathBlocks.fixture.json`):
///
/// - a line whose content (after ≤ 3 spaces and any `>` quote prefix) opens
///   with `$$` — not `$$$`, no closing `$$` later on it — opens a block only
///   when a CLOSER IS IN SIGHT: a later line containing `$$`, before the next
///   blank line, at the same quote depth. Prose that merely begins with `$$`
///   stays prose, and a slip costs at most a paragraph, never the document;
/// - the first such line closes it; text after its `$$` becomes the next
///   line of prose;
/// - the block keeps the opener's quote prefix and indentation, so one
///   inside a list item stays inside the item;
/// - fenced code, raw-HTML blocks (to their next blank line) and a leading
///   YAML frontmatter block pass through untouched;
/// - a lone `$$` line closing display math opened earlier in its paragraph
///   (an odd `$$` count so far) is left to inline pairing.
///
/// A line pass has no container model, and the fixture pins where that
/// shows: a `$$` block in a list item whose content column is 4+ (`10. `,
/// nested lists) reads as indented code here while the editor, which knows
/// the column, still renders it; a block opened on a marker line whose
/// later lines leave the item is promoted here while the editor ends it
/// unclosed; a lazy quote line resets the `$$` parity here; and an
/// unbalanced backtick hides the rest of its line from the parity count.
///
/// Promotion can add lines (text after an opening `$$`, or around a closing
/// one, gets a line of its own), so the pass also returns a [`LineMap`] from
/// its output's lines back to `text`'s, which keeps comrak's `data-sourcepos`
/// in the source file's numbering.
fn promote_math_blocks_mapped(text: &str) -> (Cow<'_, str>, LineMap) {
    let mut map = LineMap::default();
    if !text.contains("$$") {
        return (Cow::Borrowed(text), map);
    }
    fn body(line: &str) -> &str {
        line.trim_end_matches('\n').trim_end_matches('\r')
    }
    /// The blockquote prefix `( {0,3}> ?)*`: its depth and byte length.
    fn quote_prefix(s: &str) -> (usize, usize) {
        let b = s.as_bytes();
        let (mut depth, mut i) = (0, 0);
        loop {
            let mut j = i;
            while j < b.len() && b[j] == b' ' && j - i < 3 {
                j += 1;
            }
            if j < b.len() && b[j] == b'>' {
                j += 1;
                if j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                    j += 1;
                }
                depth += 1;
                i = j;
            } else {
                return (depth, i);
            }
        }
    }
    /// A fence opener (```/~~~): its char and length.
    fn fence(content: &str) -> Option<(u8, usize)> {
        let ch = *content.as_bytes().first()?;
        if ch != b'`' && ch != b'~' {
            return None;
        }
        let len = content.bytes().take_while(|&c| c == ch).count();
        (len >= 3).then_some((ch, len))
    }
    fn closes_fence(content: &str, ch: u8, len: usize) -> bool {
        let run = content.bytes().take_while(|&c| c == ch).count();
        run >= len && content[run..].trim().is_empty()
    }
    /// The text after `$$` when the line's content opens a block.
    fn opener(content: &str) -> Option<&str> {
        let rest = content.strip_prefix("$$")?;
        (!rest.starts_with('$') && !rest.contains("$$")).then_some(rest)
    }
    fn starts_html_block(content: &str) -> bool {
        content.starts_with('<')
            && content[1..]
                .bytes()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == b'/' || c == b'!' || c == b'?')
    }
    /// A list marker (`- `, `+ `, `* `, `1. `, `1) `): its byte length and
    /// the item's text after it.
    fn after_list_marker(content: &str) -> Option<(usize, &str)> {
        let b = content.as_bytes();
        let n = match b.first()? {
            b'-' | b'*' | b'+' => 1,
            c if c.is_ascii_digit() => {
                let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
                if !matches!(b.get(n), Some(b'.' | b')')) {
                    return None;
                }
                n + 1
            }
            _ => return None,
        };
        (b.get(n) == Some(&b' ')).then(|| (n + 1, &content[n + 1..]))
    }
    /// A heading, thematic break or table row: a block of its own, ending
    /// any paragraph without contributing to one.
    fn ends_paragraph(content: &str) -> bool {
        let b = content.as_bytes();
        match b.first() {
            Some(b'#') => b.get(1).is_none_or(|&c| c == b' ' || c == b'#'),
            Some(b'|') => true,
            Some(&c @ (b'-' | b'*' | b'_')) => {
                content.bytes().filter(|&x| x != b' ').all(|x| x == c)
                    && content.bytes().filter(|&x| x == c).count() >= 3
            }
            _ => false,
        }
    }
    /// `$$` delimiters in a line: pairs not part of a longer run (`$$$` is
    /// text) and outside backtick code spans — the editor's `countDollarPairs`,
    /// except that an unbalanced backtick hides the rest of the line here.
    fn dollar_pairs(content: &str) -> usize {
        let mut n = 0;
        let mut in_code = false;
        let b = content.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'`' => in_code = !in_code,
                b'$' if !in_code => {
                    let run = b[i..].iter().take_while(|&&c| c == b'$').count();
                    if run == 2 {
                        n += 1;
                    }
                    i += run;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        n
    }

    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::with_capacity(text.len() + 32);
    let mut i = 0;
    // Output lines before `counted` (a byte offset into `out`) are tallied in
    // `out_lines`; blocks settle the tally before mapping their own lines.
    let (mut out_lines, mut counted) = (0u32, 0usize);
    // A leading frontmatter block is metadata, never math — the same block
    // comrak and the editor set aside.
    if let Some(fm) = frontmatter(text) {
        i = fm.lines.min(lines.len());
        out.extend(lines[..i].iter().copied());
    }
    let mut in_fence: Option<(u8, usize)> = None;
    let mut in_html = false;
    let mut para_dollars = 0usize;
    let mut para_depth = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let b = body(line);
        let (depth, pe) = quote_prefix(b);
        let after_quote = &b[pe..];
        let content = after_quote.trim_start_matches([' ', '\t']);
        // Columns, the way CommonMark counts them: a tab is four.
        let indent = after_quote[..after_quote.len() - content.len()]
            .bytes()
            .map(|c| if c == b'\t' { 4 } else { 1 })
            .sum::<usize>();
        if let Some((ch, len)) = in_fence {
            if indent <= 3 && closes_fence(content, ch, len) {
                in_fence = None;
            }
            out.push_str(line);
            i += 1;
            continue;
        }
        if content.trim().is_empty() {
            in_html = false;
            para_dollars = 0;
            out.push_str(line);
            i += 1;
            continue;
        }
        if in_html {
            out.push_str(line);
            i += 1;
            continue;
        }
        if indent <= 3 {
            // A fence may sit on a list item's marker line (`- ```` … `  ````).
            if let Some(f) = fence(after_list_marker(content).map_or(content, |(_, rest)| rest)) {
                in_fence = Some(f);
                para_dollars = 0;
                out.push_str(line);
                i += 1;
                continue;
            }
            if starts_html_block(content) {
                in_html = true;
                para_dollars = 0;
                out.push_str(line);
                i += 1;
                continue;
            }
        }
        if depth != para_depth {
            para_dollars = 0;
            para_depth = depth;
        }
        if indent <= 3 && ends_paragraph(content) {
            para_dollars = 0;
            out.push_str(line);
            i += 1;
            continue;
        }
        // A list item's paragraph starts after its marker; a block opened on
        // the marker line keeps the marker and indents its body to the item.
        let (text, open_prefix, body_prefix) =
            match (indent <= 3).then(|| after_list_marker(content)).flatten() {
                Some((m, rest)) => {
                    para_dollars = 0;
                    (
                        rest,
                        b[..pe + indent + m].to_string(),
                        format!("{}{}", &b[..pe + indent], " ".repeat(m)),
                    )
                }
                None => (
                    content,
                    b[..pe + indent].to_string(),
                    b[..pe + indent].to_string(),
                ),
            };
        if indent <= 3 && para_dollars.is_multiple_of(2) {
            if let Some(rest) = opener(text) {
                // The closer in sight: before the next blank line, at this depth.
                let mut closer = None;
                for (j, l) in lines.iter().enumerate().skip(i + 1) {
                    let bj = body(l);
                    let (dj, pj) = quote_prefix(bj);
                    if bj[pj..].trim().is_empty() || dj < depth {
                        break;
                    }
                    if let Some(k) = bj[pj..].find("$$") {
                        closer = Some((j, pj + k));
                        break;
                    }
                }
                if let Some((j, k)) = closer {
                    // Every line before the opener was copied whole (with its
                    // newline), so the tally is exact here.
                    out_lines +=
                        u32::try_from(out[counted..].matches('\n').count()).unwrap_or(u32::MAX);
                    let (open_src, close_src) = (line_no(i), line_no(j));
                    out.push_str(&open_prefix);
                    out.push_str("```math\n");
                    map.next(&mut out_lines, open_src);
                    if !rest.trim().is_empty() {
                        out.push_str(&body_prefix);
                        out.push_str(rest);
                        out.push('\n');
                        map.next(&mut out_lines, open_src);
                    }
                    if j > i + 1 {
                        out.extend(lines[i + 1..j].iter().copied());
                        map.next(&mut out_lines, line_no(i + 1));
                        out_lines += line_no(j - 1) - line_no(i + 1);
                    }
                    let bj = body(lines[j]);
                    let (before, after) = (&bj[..k], &bj[k + 2..]);
                    if !before[quote_prefix(before).1..].trim().is_empty() {
                        out.push_str(before);
                        out.push('\n');
                        map.next(&mut out_lines, close_src);
                    }
                    out.push_str(&body_prefix);
                    out.push_str("```\n");
                    map.next(&mut out_lines, close_src);
                    if !after.trim().is_empty() {
                        out.push_str(&bj[..quote_prefix(bj).1]);
                        out.push_str(after.trim_start());
                        out.push('\n');
                        map.next(&mut out_lines, close_src);
                    }
                    // The next copied line resumes in step with its source.
                    map.at(out_lines + 1, line_no(j + 1));
                    counted = out.len();
                    i = j + 1;
                    para_dollars = 0;
                    continue;
                }
            }
        }
        para_dollars += dollar_pairs(text);
        out.push_str(line);
        i += 1;
    }
    (Cow::Owned(out), map)
}

/// [`promote_math_blocks_mapped`] without the map (the fixture's view).
#[cfg(test)]
fn promote_math_blocks(text: &str) -> Cow<'_, str> {
    promote_math_blocks_mapped(text).0
}

/// 1-based line number of 0-based line index `i`.
fn line_no(i: usize) -> u32 {
    u32::try_from(i + 1).unwrap_or(u32::MAX)
}

/// Output line -> source line for text rewritten by
/// [`promote_math_blocks_mapped`]. Stored as the few anchors where the two
/// stop moving in step (a split-off fence line repeats its source line), so
/// memory scales with promoted blocks, never with document length. Empty =
/// identity.
#[derive(Default)]
struct LineMap {
    /// `(output line, source line)`, both 1-based, output ascending.
    anchors: Vec<(u32, u32)>,
}

impl LineMap {
    /// The source line of output line `out` (1-based).
    fn source_line(&self, out: u32) -> u32 {
        match self.anchors.partition_point(|&(o, _)| o <= out) {
            0 => out,
            n => {
                let (o, src) = self.anchors[n - 1];
                src + (out - o)
            }
        }
    }

    /// Record that output line `out` comes from source line `src`, adding an
    /// anchor only where that breaks step with the lines before it.
    fn at(&mut self, out: u32, src: u32) {
        if self.source_line(out) != src {
            self.anchors.push((out, src));
        }
    }

    /// [`Self::at`] for the next output line, advancing the tally.
    fn next(&mut self, out_lines: &mut u32, src: u32) {
        *out_lines += 1;
        self.at(*out_lines, src);
    }

    fn is_identity(&self) -> bool {
        self.anchors.is_empty()
    }
}

/// What [`markdown_to_html`] hands comrak, for `doc_check`'s AST walk (the
/// checker must parse a document exactly as the reading view does): the
/// `$$`-promoted text, how many leading lines an accepted [`frontmatter`]
/// block spans (None = no block, so no front-matter option), and the line map.
pub(crate) struct MarkdownParseInput<'t> {
    pub(crate) text: Cow<'t, str>,
    pub(crate) frontmatter_lines: Option<usize>,
    lines: LineMap,
}

impl MarkdownParseInput<'_> {
    /// The source line of line `line` (1-based) of [`Self::text`].
    pub(crate) fn source_line(&self, line: usize) -> usize {
        self.lines.source_line(line as u32) as usize
    }
}

pub(crate) fn markdown_parse_input(text: &str) -> MarkdownParseInput<'_> {
    let frontmatter_lines = frontmatter(text).map(|f| f.lines);
    let (text, lines) = promote_math_blocks_mapped(text);
    MarkdownParseInput {
        text,
        frontmatter_lines,
        lines,
    }
}

/// Rewrite every `data-sourcepos="l:c-l:c"` in comrak's output from promoted
/// lines to source lines. Columns inside a promoted block's split-off lines
/// stay as comrak counted them (approximate there); lines are exact.
fn remap_sourcepos<'a>(html: &'a str, map: &LineMap) -> Cow<'a, str> {
    const ATTR: &str = "data-sourcepos=\"";
    if map.is_identity() || !html.contains(ATTR) {
        return Cow::Borrowed(html);
    }
    // `l:c-l:c` -> the four numbers, or None for anything else.
    fn parse(value: &str) -> Option<[u32; 4]> {
        let (start, end) = value.split_once('-')?;
        let (l1, c1) = start.split_once(':')?;
        let (l2, c2) = end.split_once(':')?;
        Some([
            l1.parse().ok()?,
            c1.parse().ok()?,
            l2.parse().ok()?,
            c2.parse().ok()?,
        ])
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find(ATTR) {
        out.push_str(&rest[..i + ATTR.len()]);
        rest = &rest[i + ATTR.len()..];
        let Some(end) = rest.find('"') else { break };
        match parse(&rest[..end]) {
            Some([l1, c1, l2, c2]) => {
                let (l1, l2) = (map.source_line(l1), map.source_line(l2));
                out.push_str(&format!("{l1}:{c1}-{l2}:{c2}"));
            }
            None => out.push_str(&rest[..end]),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// comrak renders a ```math fence — every promoted `$$` block, and a
/// hand-written GitHub one — as `<pre[ data-sourcepos="…"]><code
/// class="language-math" data-math-style="display">`. The client keys on the
/// SAME `<span data-math-style>` shape for every equation, so the fence chrome
/// is rewritten to it here, the source position moving to the `<p>`; the
/// literal inside is already HTML-escaped, so the closing tags cannot occur
/// within it.
fn math_fences_to_spans(html: &str) -> Cow<'_, str> {
    const PRE: &str = "<pre";
    const CODE: &str = "<code class=\"language-math\" data-math-style=\"display\">";
    const CLOSE: &str = "</code></pre>";
    if !html.contains(CODE) {
        return Cow::Borrowed(html);
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find(PRE) {
        let tail = &rest[i + PRE.len()..];
        let (pos, after_pre) = match tail
            .strip_prefix(" data-sourcepos=\"")
            .and_then(|t| t.split_once('"'))
        {
            Some((pos, after)) => (Some(pos), after),
            None => (None, tail),
        };
        let Some(body) = after_pre
            .strip_prefix('>')
            .and_then(|t| t.strip_prefix(CODE))
        else {
            out.push_str(&rest[..i + PRE.len()]);
            rest = tail;
            continue;
        };
        out.push_str(&rest[..i]);
        out.push_str("<p");
        if let Some(pos) = pos {
            out.push_str(" data-sourcepos=\"");
            out.push_str(pos);
            out.push('"');
        }
        out.push_str("><span data-math-style=\"display\">");
        match body.find(CLOSE) {
            Some(j) => {
                out.push_str(&body[..j]);
                out.push_str("</span></p>");
                rest = &body[j + CLOSE.len()..];
            }
            None => {
                out.push_str(body);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// comrak renders a task item's box as `<input type="checkbox"[
/// data-sourcepos][ class][ checked=""] disabled="" />`. The sanitizer never
/// allows `input` — a form control has no place in a rendered document — so
/// each checkbox becomes an inert `<span class="md-task" data-task="done|todo">`
/// first, keeping a `data-sourcepos` comrak put on it (a task in a table
/// cell). A raw-HTML checkbox in the source becomes the same span, which is
/// harmless; any other `<input>` is left for the sanitizer to drop.
fn tasks_to_spans(html: &str) -> Cow<'_, str> {
    const OPEN: &str = "<input";
    if !html.contains(OPEN) {
        return Cow::Borrowed(html);
    }
    /// The double-quoted value of `name` in `tag`, if present.
    fn attr<'t>(tag: &'t str, name: &str) -> Option<&'t str> {
        let marker = format!(" {name}=\"");
        let start = tag.find(&marker)? + marker.len();
        tag[start..].split_once('"').map(|(value, _)| value)
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find(OPEN) {
        out.push_str(&rest[..i]);
        let from_tag = &rest[i..];
        let tag = from_tag.find('>').map(|end| &from_tag[..=end]);
        let is_checkbox = tag.is_some_and(|tag| {
            matches!(
                tag.as_bytes().get(OPEN.len()),
                Some(b' ' | b'\t' | b'\n' | b'\r' | b'/')
            ) && attr(tag, "type").is_some_and(|t| t.eq_ignore_ascii_case("checkbox"))
        });
        let Some(tag) = tag.filter(|_| is_checkbox) else {
            out.push_str(OPEN);
            rest = &from_tag[OPEN.len()..];
            continue;
        };
        let done = tag
            .split(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
            .any(|token| token == "checked" || token.starts_with("checked="));
        out.push_str("<span class=\"md-task\" data-task=\"");
        out.push_str(if done { "done" } else { "todo" });
        out.push('"');
        if let Some(pos) = attr(tag, "data-sourcepos").filter(|v| {
            v.bytes()
                .all(|b| b.is_ascii_digit() || b == b':' || b == b'-')
        }) {
            out.push_str(" data-sourcepos=\"");
            out.push_str(pos);
            out.push('"');
        }
        out.push_str("></span>");
        rest = &from_tag[tag.len()..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// comrak with the GFM extensions the reading view promises, plus `$…$` /
/// `$$…$$` math — inline, promoted `$$` blocks and ```math fences alike —
/// emitted as `<span data-math-style>` LaTeX literals, never typeset here;
/// the client owns KaTeX. Raw HTML passes through for ammonia to judge.
///
/// Also: a leading [`frontmatter`] block is set aside (never a rule plus a
/// setext heading); GitHub alerts (`> [!NOTE]` …) render as
/// `div.markdown-alert.markdown-alert-<type>` with a `p.markdown-alert-title`;
/// headings get GitHub-slug ids (and an empty `a.anchor` link), which the
/// sanitizer namespaces as `user-content-<slug>` like footnote ids; task boxes
/// become [`tasks_to_spans`] spans; and every element carries
/// `data-sourcepos="line:col-line:col"` in the SOURCE file's numbering
/// (frontmatter and promoted `$$` blocks included).
fn markdown_to_html(text: &str) -> String {
    let with_frontmatter = frontmatter(text).is_some();
    let (promoted, lines) = promote_math_blocks_mapped(text);
    let mut options = comrak::Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.math_dollars = true;
    options.extension.alerts = true;
    // No prefix here: the sanitizer's `id_prefix` namespaces EVERY id —
    // headings, footnotes and raw HTML alike — as `user-content-…`, GitHub's
    // scheme, so a document can never clobber the app's own element ids.
    options.extension.header_id_prefix = Some(String::new());
    if with_frontmatter {
        // Only for a block `frontmatter` accepted, so a document that merely
        // opens with a thematic break keeps it.
        options.extension.front_matter_delimiter = Some("---".to_owned());
    }
    // Let raw HTML through comrak; ammonia strips anything dangerous.
    options.render.r#unsafe = true;
    options.render.sourcepos = true;
    options.render.tasklist_classes = true;
    let html = comrak::markdown_to_html(&promoted, &options);
    let html = remap_sourcepos(&html, &lines);
    let html = math_fences_to_spans(&html);
    tasks_to_spans(&html).into_owned()
}

/// Every class the rendered document may carry: exactly what comrak emits
/// for the features above, plus the task span. Anything else (a raw-HTML
/// `class`, a code block's `language-*`) is stripped.
const MARKDOWN_CLASSES: &[(&str, &[&str])] = &[
    (
        "div",
        &[
            "markdown-alert",
            "markdown-alert-note",
            "markdown-alert-tip",
            "markdown-alert-important",
            "markdown-alert-warning",
            "markdown-alert-caution",
        ],
    ),
    ("p", &["markdown-alert-title"]),
    ("span", &["md-task"]),
    ("a", &["anchor", "footnote-backref"]),
    ("sup", &["footnote-ref"]),
    ("section", &["footnotes"]),
    ("ul", &["contains-task-list"]),
    ("ol", &["contains-task-list"]),
    ("li", &["task-list-item"]),
];

/// ammonia's defaults, widened only for what [`markdown_to_html`] emits:
///
/// - `data-math-style` on `span`, the marker the client's typesetter keys on.
///   Its value is inert (a style name) and the span's text is LaTeX the
///   client renders with KaTeX trust off, so a hand-written `<span
///   data-math-style>` in a document can do no more than `$$` can;
/// - `data-task` on `span` (the task marker) and [`MARKDOWN_CLASSES`];
/// - `data-sourcepos` everywhere (inert coordinates);
/// - `section` (the footnotes container) and `id` where comrak puts one
///   (headings, footnote refs and definitions), every id prefixed
///   `user-content-` — so `href="#fn-1"` pairs with `id="user-content-fn-1"`
///   and the client maps `#x` to `user-content-x`.
///
/// Never `input`: task boxes arrive as spans.
fn sanitize_markdown(html: &str) -> String {
    let mut builder = ammonia::Builder::default();
    builder
        .add_tags(&["section"])
        .add_tag_attributes("span", &["data-math-style", "data-task"])
        .add_generic_attributes(&["data-sourcepos"])
        .id_prefix(Some("user-content-"));
    for tag in ["h1", "h2", "h3", "h4", "h5", "h6", "a", "li"] {
        builder.add_tag_attributes(tag, &["id"]);
    }
    for (tag, classes) in MARKDOWN_CLASSES {
        builder.add_allowed_classes(*tag, *classes);
    }
    builder.clean(html).to_string()
}

#[cfg(test)]
mod markdown_tests {
    use super::*;

    /// The shared `$$` block case list — its `about` documents the fields;
    /// Vitest reads the same file for the editor's half.
    const MATH_BLOCKS_FIXTURE: &str =
        include_str!("../../../web-ui/src/lib/previews/mathBlocks.fixture.json");

    /// Strict on purpose: a misspelled key would silently drop its pin.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MathBlockCase {
        note: String,
        input: String,
        editor: Vec<String>,
        server: Option<String>,
        reading: Vec<String>,
        diverges: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MathBlocksFixture {
        about: Vec<String>,
        cases: Vec<MathBlockCase>,
    }

    /// `style:literal` for every `<span data-math-style>` in rendered HTML —
    /// what `MarkdownView` typesets, in document order. The first `>` after
    /// the attribute ends the tag: html5ever escapes `>` inside values.
    fn reading_equations(html: &str) -> Vec<String> {
        const MARK: &str = "data-math-style=\"";
        let mut out = Vec::new();
        let mut rest = html;
        while let Some(i) = rest.find(MARK) {
            rest = &rest[i + MARK.len()..];
            let style_end = rest.find('"').expect("a closed attribute");
            let style = &rest[..style_end];
            rest = &rest[rest.find('>').expect("a closed tag") + 1..];
            let end = rest.find("</span>").expect("a closed span");
            out.push(format!("{style}:{}", &rest[..end]));
            rest = &rest[end..];
        }
        out
    }

    #[test]
    fn dollar_block_rules_mirror_the_live_parser() {
        let fixture: MathBlocksFixture =
            serde_json::from_str(MATH_BLOCKS_FIXTURE).expect("the fixture parses");
        assert!(!fixture.about.is_empty() && !fixture.cases.is_empty());
        for c in &fixture.cases {
            let want = c.server.as_deref().unwrap_or(&c.input);
            assert_eq!(
                promote_math_blocks(&c.input),
                want,
                "{}: {:?}",
                c.note,
                c.input
            );
            let html = sanitize_markdown(&markdown_to_html(&c.input));
            assert_eq!(reading_equations(&html), c.reading, "{}: {html}", c.note);
            if c.diverges.is_none() {
                // The fixture's own consistency: one CLOSED MathBlock in the
                // editor per fence this pass adds (an unclosed one is dumped
                // as `MathBlock(unclosed)` and adds none).
                let fences = |s: &str| s.matches("```math").count();
                let blocks = c
                    .editor
                    .iter()
                    .filter(|e| e.starts_with("MathBlock:") || e.contains("/MathBlock:"))
                    .count();
                assert_eq!(
                    blocks,
                    fences(want) - fences(&c.input),
                    "{}: one closed MathBlock per promoted fence",
                    c.note
                );
            }
        }
    }

    #[test]
    fn dollar_math_survives_sanitization_as_literals_for_the_client() {
        let html = sanitize_markdown(&markdown_to_html(
            "Inline $a<b$ here.\n\n$$\nx^2\n$$\n\n<script>alert(1)</script>\n",
        ));
        assert_eq!(
            reading_equations(&html),
            ["inline:a&lt;b", "display:x^2\n"],
            "{html}"
        );
        // A promoted `$$` block is handed over in the same `<p><span>` shape
        // as every other equation, its source lines on the `<p>`.
        assert!(
            html.contains(
                "<p data-sourcepos=\"3:1-5:3\"><span data-math-style=\"display\">x^2\n</span></p>"
            ),
            "{html}"
        );
        assert!(!html.contains("<script"), "{html}");
    }

    #[test]
    fn currency_dollars_stay_text() {
        let html = markdown_to_html("costs $5 and $10\n");
        assert!(!html.contains("data-math-style"), "{html}");
        assert!(html.contains("$5 and $10"), "{html}");
    }

    /// The markdown-table recipe (web-ui app.css, both surfaces) keys on the
    /// `align` attribute: comrak must write GFM `:-:` / `--:` as
    /// `align="center|right"` (never a style,
    /// which the sanitizer would strip), ammonia's defaults must let it
    /// through, and an unmarked column must carry no attribute at all.
    #[test]
    fn gfm_alignment_survives_sanitization_as_align_attributes() {
        let html = without_sourcepos(&sanitize_markdown(&markdown_to_html(
            "| a | b | c |\n|:-:|--:|---|\n| 1 | 2 | 3 |\n",
        )));
        for cell in [
            "<th align=\"center\">a</th>",
            "<th align=\"right\">b</th>",
            "<th>c</th>",
            "<td align=\"center\">1</td>",
            "<td align=\"right\">2</td>",
            "<td>3</td>",
        ] {
            assert!(html.contains(cell), "missing {cell} in {html}");
        }
    }

    /// Hand-written HTML reaches the sanitizer too (`render.unsafe`), and the
    /// recipe matches its `align` case-insensitively — so ammonia must
    /// pass a raw value through unnormalised, empty ones included.
    #[test]
    fn raw_html_alignment_passes_through_unnormalised() {
        let html = sanitize_markdown(&markdown_to_html(
            "<table><tr><td align=\"CENTER\">x</td><td align=\"\">y</td></tr></table>\n",
        ));
        assert!(html.contains("<td align=\"CENTER\">x</td>"), "{html}");
        assert!(html.contains("<td align=\"\">y</td>"), "{html}");
    }

    /// `html` minus every ` data-sourcepos="…"` — for assertions about other
    /// attributes.
    fn without_sourcepos(html: &str) -> String {
        const ATTR: &str = " data-sourcepos=\"";
        let mut out = String::with_capacity(html.len());
        let mut rest = html;
        while let Some(i) = rest.find(ATTR) {
            out.push_str(&rest[..i]);
            let after = &rest[i + ATTR.len()..];
            rest = &after[after.find('"').expect("a closed attribute") + 1..];
        }
        out.push_str(rest);
        out
    }

    fn render(text: &str) -> String {
        sanitize_markdown(&markdown_to_html(text))
    }

    /// A leading frontmatter block is metadata: no rule, no setext heading,
    /// its text answered raw — and every line after it keeps the SOURCE
    /// file's number (a heading on line 5 after a 3-line block says `5:1`).
    #[test]
    fn frontmatter_is_set_aside_and_later_lines_keep_their_numbers() {
        let text = "---\ntitle: A $$ title\n---\n\n# Five\n\n$$\nx\n$$\n";
        let html = render(text);
        assert!(!html.contains("<hr"), "{html}");
        assert!(!html.contains("<h2"), "{html}");
        assert!(!html.contains("title:"), "{html}");
        assert!(
            html.contains("<h1 id=\"user-content-five\" data-sourcepos=\"5:1-5:6\">"),
            "{html}"
        );
        // The frontmatter's `$$` is not math; the block after it is, on 7-9
        // (the end column is the promoted fence's, one past the `$$`).
        assert!(
            html.contains("<p data-sourcepos=\"7:1-9:3\"><span data-math-style=\"display\">x\n"),
            "{html}"
        );
        let fm = frontmatter(text).expect("a frontmatter block");
        assert_eq!(fm.inner, "title: A $$ title");
        assert_eq!(fm.lines, 3);

        // CRLF and a BOM are comrak's frontmatter too; inner lines keep CRLF.
        let crlf = "\u{feff}---\r\na: 1\r\nb: 2\r\n---\r\n# Five\r\n";
        assert_eq!(frontmatter(crlf).map(|f| f.inner), Some("a: 1\r\nb: 2"));
        let html = render(crlf);
        assert!(html.contains("data-sourcepos=\"5:1-5:6\""), "{html}");
        assert!(!html.contains("<hr"), "{html}");
    }

    /// Only the live editor's shape counts: a document that merely opens with
    /// a thematic break (no `key:` line), a closer past line 200, or a closer
    /// that is not exactly `---` keeps rendering as it always did.
    #[test]
    fn frontmatter_needs_the_editors_shape() {
        for text in [
            "---\n\nintro\n\n---\n# H\n",
            "---\nnot a key line\n---\n",
            "---\ntitle: x\n--- \n",
            "---\ntitle: x\n----\n",
            "--- \ntitle: x\n---\n",
        ] {
            assert!(frontmatter(text).is_none(), "{text:?}");
            assert!(render(text).contains("<hr"), "{text:?}");
        }
        let long = format!("---\ntitle: x\n{}---\n", "k: v\n".repeat(197));
        assert_eq!(frontmatter(&long).map(|f| f.lines), Some(200));
        let too_long = format!("---\ntitle: x\n{}---\n", "k: v\n".repeat(198));
        assert!(frontmatter(&too_long).is_none());
    }

    /// GitHub alerts: every type, matched case-insensitively, keep exactly
    /// comrak's classes through the sanitizer; a raw-HTML class is stripped.
    #[test]
    fn github_alerts_keep_their_classes() {
        let html = without_sourcepos(&render(concat!(
            "> [!NOTE]\n> n\n\n",
            "> [!tip]\n> t\n\n",
            "> [!Important]\n> i\n\n",
            "> [!WARNING]\n> w\n\n",
            "> [!CAUTION] Mind the gap\n> c\n\n",
            "<div class=\"evil markdown-alert\" onclick=\"x()\">raw</div>\n",
        )));
        for (kind, title) in [
            ("note", "Note"),
            ("tip", "Tip"),
            ("important", "Important"),
            ("warning", "Warning"),
            ("caution", "Mind the gap"),
        ] {
            let want = format!(
                "<div class=\"markdown-alert markdown-alert-{kind}\">\n<p class=\"markdown-alert-title\">{title}</p>"
            );
            assert!(html.contains(&want), "missing {want} in {html}");
        }
        assert!(
            html.contains("<div class=\"markdown-alert\">raw</div>"),
            "{html}"
        );
        assert!(
            !html.contains("evil") && !html.contains("onclick"),
            "{html}"
        );
    }

    /// Heading ids are GitHub slugs (deduplicated) under `user-content-`;
    /// the heading's own anchor link keeps the bare `#slug` the client maps.
    #[test]
    fn heading_ids_are_namespaced_github_slugs() {
        let html = without_sourcepos(&render(
            "# Hello, World!\n\n# Hello, World!\n\n## Ünï code\n",
        ));
        for want in [
            "<h1 id=\"user-content-hello-world\">Hello, World!<a href=\"#hello-world\" class=\"anchor\" rel=\"noopener noreferrer\"></a></h1>",
            "<h1 id=\"user-content-hello-world-1\">",
            "<h2 id=\"user-content-ünï-code\">",
        ] {
            assert!(html.contains(want), "missing {want} in {html}");
        }
        // Raw HTML ids are namespaced too: a document cannot clobber the app.
        let html = render("<p id=\"app\">x</p>\n");
        assert!(!html.contains("id=\"app\""), "{html}");
    }

    /// A footnote reference `#fn-1` pairs with `id="user-content-fn-1"`, the
    /// back-reference `#fnref-1` with `id="user-content-fnref-1"`.
    #[test]
    fn footnote_ids_pair_with_their_links() {
        let html = without_sourcepos(&render("Hi[^1].\n\n[^1]: A greeting.\n"));
        for want in [
            "<sup class=\"footnote-ref\"><a href=\"#fn-1\" id=\"user-content-fnref-1\"",
            "<section class=\"footnotes\">",
            "<li id=\"user-content-fn-1\">",
            "<a href=\"#fnref-1\" class=\"footnote-backref\"",
        ] {
            assert!(html.contains(want), "missing {want} in {html}");
        }
    }

    /// Task boxes arrive as inert spans; `input` never survives — neither
    /// comrak's nor a raw one (a raw checkbox becomes the same span).
    #[test]
    fn task_boxes_become_spans_and_inputs_never_survive() {
        let html = without_sourcepos(&render(concat!(
            "- [ ] todo\n- [x] done\n\n",
            "<input type=\"checkbox\" checked onclick=\"x()\">\n\n",
            "<input type=\"text\" value=\"v\">\n",
        )));
        for want in [
            "<ul class=\"contains-task-list\">",
            "<li class=\"task-list-item\"><span class=\"md-task\" data-task=\"todo\"></span> todo</li>",
            "<li class=\"task-list-item\"><span class=\"md-task\" data-task=\"done\"></span> done</li>",
        ] {
            assert!(html.contains(want), "missing {want} in {html}");
        }
        assert_eq!(html.matches("data-task=\"done\"").count(), 2, "{html}");
        assert!(!html.contains("<input"), "{html}");
        assert!(!html.contains("onclick"), "{html}");
    }

    /// Promotion splits text around `$$` onto lines of its own; positions
    /// after (and inside) the block still name the source lines.
    #[test]
    fn promoted_math_blocks_do_not_shift_source_lines() {
        let text = concat!(
            "Intro\n",     // 1
            "\n",          // 2
            "$$x^2\n",     // 3: opener with text after it
            "+ y\n",       // 4
            "z $$ tail\n", // 5: text before AND after the closer
            "\n",          // 6
            "> $$a\n",     // 7: inside a quote
            "> b $$\n",    // 8
            "\n",          // 9
            "# Ten\n",     // 10
        );
        let (promoted, map) = promote_math_blocks_mapped(text);
        assert!(
            promoted.lines().count() > text.lines().count(),
            "{promoted}"
        );
        let html = render(text);
        for want in [
            "<p data-sourcepos=\"1:1-1:5\">Intro</p>",
            "<p data-sourcepos=\"3:1-5:3\"><span data-math-style=\"display\">",
            "<p data-sourcepos=\"5:1-5:4\">tail</p>",
            "<blockquote data-sourcepos=\"7:1-8:",
            "<h1 id=\"user-content-ten\" data-sourcepos=\"10:1-10:5\">",
        ] {
            assert!(html.contains(want), "missing {want} in {html}");
        }
        // Every output line maps inside the source.
        let total = u32::try_from(text.lines().count()).unwrap();
        for out in 1..=u32::try_from(promoted.lines().count()).unwrap() {
            assert!((1..=total).contains(&map.source_line(out)), "line {out}");
        }
        // No `$$` at all: the identity map, nothing rewritten.
        assert!(promote_math_blocks_mapped("# a\n\nb\n").1.is_identity());
    }
}

#[derive(Deserialize)]
pub(crate) struct TableQuery {
    path: String,
    #[serde(default)]
    offset_rows: usize,
    #[serde(default)]
    limit_rows: Option<usize>,
    #[serde(default)]
    delim: Option<String>,
    /// Comma-separated line prefixes skipped wherever they appear: `##` for
    /// VCF meta lines, `@` for SAM headers, `#,track,browser` for BED.
    #[serde(default)]
    comment: Option<String>,
    /// `false`: there is no header row — every line is data, and the columns
    /// are `names` followed by `colN` for any further field.
    #[serde(default)]
    header: Option<bool>,
    /// Comma-separated column names for a header-less file.
    #[serde(default)]
    names: Option<String>,
    /// `false`: fields are never quoted. SAM qualities and VCF text can open a
    /// field with `"`, which CSV quoting would read as a quote that swallows
    /// lines until the next one.
    #[serde(default)]
    quote: Option<bool>,
}

const MAX_COMMENT_PREFIXES: usize = 4;
const MAX_COMMENT_PREFIX_BYTES: usize = 16;
const MAX_TABLE_NAMES: usize = 256;
const MAX_TABLE_NAME_BYTES: usize = 256;

/// The parse options of one `fs/table` read beyond paging.
struct TableOpts {
    delim: String,
    comments: Vec<Vec<u8>>,
    header: bool,
    names: Vec<String>,
    quoting: bool,
}

impl TableOpts {
    fn from_query(q: &TableQuery) -> anyhow::Result<Self> {
        let comments: Vec<Vec<u8>> = q
            .comment
            .as_deref()
            .unwrap_or("")
            .split(',')
            .filter(|p| !p.is_empty())
            .map(|p| p.as_bytes().to_vec())
            .collect();
        if comments.len() > MAX_COMMENT_PREFIXES
            || comments.iter().any(|p| p.len() > MAX_COMMENT_PREFIX_BYTES)
        {
            anyhow::bail!(
                "comment takes at most {MAX_COMMENT_PREFIXES} prefixes of \
                 {MAX_COMMENT_PREFIX_BYTES} bytes"
            );
        }
        let names: Vec<String> = q
            .names
            .as_deref()
            .filter(|n| !n.is_empty())
            .map(|n| n.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        if names.len() > MAX_TABLE_NAMES || names.iter().any(|n| n.len() > MAX_TABLE_NAME_BYTES) {
            anyhow::bail!(
                "names takes at most {MAX_TABLE_NAMES} names of {MAX_TABLE_NAME_BYTES} bytes"
            );
        }
        Ok(TableOpts {
            delim: q.delim.clone().unwrap_or_else(|| "auto".into()),
            comments,
            header: q.header.unwrap_or(true),
            names,
            quoting: q.quote.unwrap_or(true),
        })
    }

    fn is_comment(&self, record: &csv::ByteRecord) -> bool {
        !self.comments.is_empty()
            && record
                .get(0)
                .is_some_and(|first| self.comments.iter().any(|p| first.starts_with(p)))
    }

    /// Everything that changes which byte a row number lands on — the row
    /// index is only reusable under the same answer.
    fn index_key(&self, delimiter: u8) -> String {
        let comments: Vec<String> = self
            .comments
            .iter()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        format!(
            "{delimiter}|{}|{}|{}",
            self.quoting,
            self.header,
            comments.join("\u{1f}")
        )
    }
}

/// GET /api/v1/fs/table?path=&offset_rows=0&limit_rows=200&delim=auto — a
/// page of a CSV/TSV file: header row as `columns`, then `limit_rows` data
/// rows starting at `offset_rows`. All cells are strings. `.gz`/`.bgz` files
/// (bioinformatics reality: `.tsv.gz` everywhere) page identically via
/// sequential decode, capped at [`MAX_GZ_DECOMPRESS`] decompressed bytes.
///
/// Additive options: `comment` (skipped line prefixes), `header=false` with
/// optional `names`, and `quote=false` — together they read VCF, BED, GFF and
/// SAM. Plain files keep a sparse row index ([`row_index`]), so a deep page
/// seeks instead of re-parsing from byte 0; each request still walks at most
/// [`MAX_TABLE_SCAN_BYTES`], and one that runs out before `offset_rows`
/// answers `scan_limited` with how far it got (`scanned_to`) — asking again
/// resumes from there. The response also carries `total_rows` once a scan has
/// reached the end, else `est_rows` (a byte-rate estimate, plain files only).
pub(crate) async fn table(Query(query): Query<TableQuery>) -> Response {
    let limit = query.limit_rows.unwrap_or(200).min(MAX_TABLE_ROWS);
    let opts = match TableOpts::from_query(&query) {
        Ok(opts) => opts,
        Err(err) => return bad_request(&err),
    };
    blocking_json(move || {
        read_table(
            &query.path,
            query.offset_rows,
            limit,
            &opts,
            MAX_TABLE_SCAN_BYTES,
        )
    })
    .await
}

/// One page's scan: the rows, and where the walk stopped.
struct TableScan {
    rows: Vec<Vec<String>>,
    /// More rows remain past the page (or may: a budget or cap stopped us).
    truncated: bool,
    eof: bool,
    /// The scan budget ran out before reaching `offset_rows`.
    scan_limited: bool,
    /// The data-row number the walk stopped at.
    end_row: usize,
    /// Rows and bytes this request walked (for the row estimate).
    walked_rows: usize,
    walked_bytes: u64,
}

fn lossy_cells(record: &csv::ByteRecord) -> Vec<String> {
    record
        .iter()
        .map(|cell| String::from_utf8_lossy(cell).into_owned())
        .collect()
}

/// Read up to the header row: leading comment lines are skipped, and the
/// first other record is the header — unless the file has none.
fn read_header<R: Read>(
    reader: &mut csv::Reader<R>,
    opts: &TableOpts,
    path: &Path,
) -> anyhow::Result<Option<Vec<String>>> {
    if !opts.header {
        return Ok(None);
    }
    let mut record = csv::ByteRecord::new();
    loop {
        let more = reader
            .read_byte_record(&mut record)
            .with_context(|| format!("{}: failed to parse header row", path.display()))?;
        if !more {
            return Ok(Some(Vec::new()));
        }
        if !opts.is_comment(&record) {
            return Ok(Some(lossy_cells(&record)));
        }
    }
}

/// Where one page walk starts and what bounds it.
struct Walk<'a> {
    /// The data-row number the reader is positioned at.
    first_row: usize,
    offset_rows: usize,
    limit_rows: usize,
    /// Most bytes to walk; `None` when the reader carries its own cap.
    budget: Option<u64>,
    /// Learns every checkpoint the walk passes.
    index: Option<&'a mut row_index::RowIndex>,
}

/// Walk data rows, collecting `limit_rows` of them from `offset_rows`.
fn scan_page<R: Read>(
    reader: &mut csv::Reader<R>,
    opts: &TableOpts,
    walk: Walk<'_>,
    path: &Path,
) -> anyhow::Result<TableScan> {
    let Walk {
        first_row,
        offset_rows,
        limit_rows,
        budget,
        mut index,
    } = walk;
    let mut record = csv::ByteRecord::new();
    let mut rows = Vec::with_capacity(limit_rows.min(256));
    let start = reader.position().byte();
    let mut row = first_row;
    let mut truncated = false;
    let mut eof = false;
    let mut scan_limited = false;
    loop {
        let at = reader.position().byte();
        if budget.is_some_and(|b| at - start > b) {
            truncated = true;
            scan_limited = row < offset_rows;
            break;
        }
        let more = reader
            .read_byte_record(&mut record)
            .with_context(|| format!("{}: failed to parse row", path.display()))?;
        if !more {
            eof = true;
            break;
        }
        if opts.is_comment(&record) {
            continue;
        }
        if let Some(index) = index.as_deref_mut() {
            index.observe(row, at);
        }
        if row >= offset_rows {
            if rows.len() == limit_rows {
                truncated = true;
                break;
            }
            rows.push(lossy_cells(&record));
        }
        row += 1;
    }
    Ok(TableScan {
        rows,
        truncated,
        eof,
        scan_limited,
        end_row: row,
        walked_rows: row - first_row,
        walked_bytes: reader.position().byte() - start,
    })
}

/// Bytes per line in a 32 KB window ending at or after `at` (clamped to the
/// file), counting only the whole lines inside it; None when it holds none.
fn window_line_rate(file: &mut std::fs::File, at: u64, len: u64) -> Option<f64> {
    const WINDOW: u64 = 32 * 1024;
    let start = at.min(len.saturating_sub(WINDOW));
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity(WINDOW as usize);
    file.take(WINDOW).read_to_end(&mut buf).ok()?;
    // The window opens mid-line: measure from the first line break to the last.
    let first = buf.iter().position(|&b| b == b'\n')?;
    let last = buf.iter().rposition(|&b| b == b'\n')?;
    let lines = buf[first + 1..=last]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    (lines > 0).then(|| (last - first) as f64 / lines as f64)
}

/// Parse one page of the delimited (possibly gzip-compressed) file at `raw`,
/// walking at most `budget` bytes of a plain file.
fn read_table(
    raw: &str,
    offset_rows: usize,
    limit_rows: usize,
    opts: &TableOpts,
    budget: u64,
) -> anyhow::Result<serde_json::Value> {
    let path = canonical_file(raw)?;
    let gz = is_gzip_path(&path);
    let delimiter = match opts.delim.as_str() {
        "auto" => sniff_delimiter(&path, gz, &opts.comments)?,
        "," | "comma" => b',',
        "\t" | "tab" => b'\t',
        other => anyhow::bail!("unsupported delimiter {other:?} (want auto, comma, or tab)"),
    };

    let file = std::fs::File::open(&path)
        .with_context(|| format!("{}: failed to open", path.display()))?;
    let mut builder = csv::ReaderBuilder::new();
    // Headers are read by hand (comment lines may precede them), so the csv
    // reader treats every record as data.
    builder
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .quoting(opts.quoting);

    let (header, scan, total_rows, est_rows) = if gz {
        // Take caps the decode work so a gzip bomb cannot spin the daemon;
        // flate2's read decoders buffer their input internally. A gzip
        // stream cannot seek, so there is no index: every read decodes from
        // the start.
        let mut reader = builder.from_reader(MultiGzDecoder::new(file).take(MAX_GZ_DECOMPRESS));
        let header = read_header(&mut reader, opts, &path)?;
        let walk = Walk {
            first_row: 0,
            offset_rows,
            limit_rows,
            budget: None,
            index: None,
        };
        let mut scan = scan_page(&mut reader, opts, walk, &path)?;
        if reader.get_ref().limit() == 0 {
            // Cap reached: rows past it are unreachable by sequential decode,
            // so the page is honestly "truncated" even though we saw EOF.
            scan.truncated = true;
            scan.eof = false;
        }
        let total = scan.eof.then_some(scan.end_row);
        (header, scan, total, None)
    } else {
        let meta = file
            .metadata()
            .with_context(|| format!("{}: failed to stat", path.display()))?;
        let key = row_index::IndexKey {
            path: path.clone(),
            opts: opts.index_key(delimiter),
        };
        let version = mtime_token(&meta);
        let mut reader = builder.from_reader(std::io::BufReader::new(file));
        let header = read_header(&mut reader, opts, &path)?;
        let mut index = row_index::lookup(&key, &version)
            .unwrap_or_else(|| row_index::RowIndex::new(version, reader.position().byte()));
        let (start_row, start_byte) = index.seek_point(offset_rows);
        let mut pos = csv::Position::new();
        pos.set_byte(start_byte);
        reader
            .seek(pos)
            .with_context(|| format!("{}: failed to seek", path.display()))?;
        let walk = Walk {
            first_row: start_row,
            offset_rows,
            limit_rows,
            budget: Some(budget),
            index: Some(&mut index),
        };
        let scan = scan_page(&mut reader, opts, walk, &path)?;
        if scan.eof {
            index.total = Some(scan.end_row);
        }
        let total = index.total;
        row_index::store(key, index);
        let est = match total {
            Some(_) => None,
            None if scan.walked_rows > 0 && scan.walked_bytes > 0 => {
                // Rows often grow down a file (ids get longer), so the walked
                // rate alone overshoots from the top: average it with the
                // line rate at the middle and the end of what remains.
                let walked_to = start_byte + scan.walked_bytes;
                let mut file = reader.into_inner().into_inner();
                let rest = meta.len().saturating_sub(walked_to);
                let rates: Vec<f64> = [
                    Some(scan.walked_bytes as f64 / scan.walked_rows as f64),
                    window_line_rate(&mut file, walked_to + rest / 2, meta.len()),
                    window_line_rate(&mut file, meta.len(), meta.len()),
                ]
                .into_iter()
                .flatten()
                .collect();
                let per_row = rates.iter().sum::<f64>() / rates.len() as f64;
                Some(scan.end_row + (rest as f64 / per_row).round() as usize)
            }
            None => None,
        };
        (header, scan, total, est)
    };

    let columns = match header {
        Some(columns) => columns,
        None => {
            // Header-less: as wide as the widest row on the page, named from
            // `names` where given.
            let width = scan
                .rows
                .iter()
                .map(Vec::len)
                .max()
                .unwrap_or(opts.names.len());
            (0..width)
                .map(|i| {
                    opts.names
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| format!("col{}", i + 1))
                })
                .collect()
        }
    };

    Ok(json!({
        "columns": columns,
        "rows": scan.rows,
        "offset": offset_rows,
        "truncated": scan.truncated,
        "total_rows": total_rows,
        "est_rows": est_rows,
        "scan_limited": scan.scan_limited,
        "scanned_to": scan.end_row,
    }))
}
#[cfg(test)]
mod table_tests {
    use super::*;

    fn opts(delim: &str) -> TableOpts {
        TableOpts {
            delim: delim.into(),
            comments: Vec::new(),
            header: true,
            names: Vec::new(),
            quoting: true,
        }
    }

    /// A walk that runs out of budget before its target answers
    /// `scan_limited` with its progress, and the next ask resumes from the
    /// checkpoints the first one left instead of starting over.
    #[test]
    fn scan_budget_resumes_from_the_row_index() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-table-budget-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rows.tsv");
        let mut text = String::from("n\tsquare\n");
        for i in 0..20_000 {
            text.push_str(&format!("{i}\t{}\n", i * i));
        }
        std::fs::write(&path, &text).unwrap();
        let raw = path.to_string_lossy().into_owned();
        let o = opts("tab");

        // ~16 bytes a row: a 64 KB budget walks ~4k rows per ask.
        let budget = 64 * 1024;
        let first = read_table(&raw, 15_000, 5, &o, budget).unwrap();
        assert_eq!(first["scan_limited"], true);
        assert_eq!(first["truncated"], true);
        assert!(first["rows"].as_array().unwrap().is_empty());
        let mut reached = first["scanned_to"].as_u64().unwrap();
        assert!(reached > 3_000 && reached < 15_000, "{reached}");
        assert!(first["est_rows"].as_u64().unwrap() > 15_000);

        let mut page = first;
        for _ in 0..8 {
            page = read_table(&raw, 15_000, 5, &o, budget).unwrap();
            if page["scan_limited"] == false {
                break;
            }
            let next = page["scanned_to"].as_u64().unwrap();
            assert!(next > reached, "no progress: {next} <= {reached}");
            reached = next;
        }
        assert_eq!(page["scan_limited"], false);
        assert_eq!(page["rows"][0], serde_json::json!(["15000", "225000000"]));
        assert_eq!(page["rows"].as_array().unwrap().len(), 5);

        // Once indexed, a deep page is one short seek within the budget.
        let again = read_table(&raw, 14_990, 3, &o, budget).unwrap();
        assert_eq!(again["scan_limited"], false);
        assert_eq!(again["rows"][0][0], "14990");

        // Reading to the end records the total for every later page.
        let mut end = read_table(&raw, 19_998, 5, &o, budget).unwrap();
        for _ in 0..8 {
            if end["scan_limited"] == false {
                break;
            }
            end = read_table(&raw, 19_998, 5, &o, budget).unwrap();
        }
        assert_eq!(end["truncated"], false);
        assert_eq!(end["total_rows"], 20_000);
        let top = read_table(&raw, 0, 2, &o, budget).unwrap();
        assert_eq!(top["total_rows"], 20_000);
        assert!(top["est_rows"].is_null());

        // A rewrite changes the version: the old offsets are not trusted.
        let mut shorter = String::from("n\tsquare\n");
        for i in 0..100 {
            shorter.push_str(&format!("{i}\t-\n"));
        }
        std::fs::write(&path, shorter).unwrap();
        let fresh = read_table(&raw, 98, 5, &o, budget).unwrap();
        assert_eq!(fresh["rows"][0], serde_json::json!(["98", "-"]));
        assert_eq!(fresh["total_rows"], 100);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
#[derive(Deserialize)]
pub(crate) struct XlsxQuery {
    path: String,
    #[serde(default)]
    sheet: Option<String>,
    #[serde(default)]
    offset_rows: usize,
    #[serde(default)]
    limit_rows: Option<usize>,
}

/// GET /api/v1/fs/xlsx?path=&sheet=&offset_rows=0&limit_rows=200 — one page of a
/// spreadsheet sheet (xlsx/xls/xlsm/ods), shaped like `fs/table` (a header
/// `columns` row + string `rows`) PLUS the workbook's `sheets` list and the
/// resolved `sheet`, so the UI can offer a sheet picker and reuse the CSV grid.
/// The first row is the header (parity with the CSV viewer). Runs on a blocking
/// worker (calamine parses the whole file) after a source-size gate.
pub(crate) async fn xlsx(Query(query): Query<XlsxQuery>) -> Response {
    let limit = query.limit_rows.unwrap_or(200).min(MAX_TABLE_ROWS);
    let result = tokio::task::spawn_blocking(move || {
        read_xlsx(
            &query.path,
            query.sheet.as_deref(),
            query.offset_rows,
            limit,
        )
    })
    .await;
    match result {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(err)) => bad_request(&err),
        Err(_) => bad_request(&anyhow::anyhow!("spreadsheet read task panicked")),
    }
}

/// One xlsx cell → the string the grid shows. Empty cells become "" (not the
/// literal "Empty" that `Data`'s Display would print); everything else uses the
/// canonical Display (numbers, bools, dates, errors).
fn xlsx_cell(cell: &calamine::Data) -> String {
    match cell {
        calamine::Data::Empty => String::new(),
        calamine::Data::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Parse one page of a spreadsheet sheet into the `fs/table` JSON shape (plus
/// `sheets`/`sheet`). calamine has no lazy row iterator, so the whole sheet is
/// materialized once per request — the [`MAX_XLSX_BYTES`] gate keeps that
/// bounded, and the caller runs us off the reactor.
fn read_xlsx(
    raw: &str,
    sheet: Option<&str>,
    offset_rows: usize,
    limit_rows: usize,
) -> anyhow::Result<serde_json::Value> {
    use calamine::Reader;

    let path = canonical_file(raw)?;
    let size = std::fs::metadata(&path)
        .with_context(|| format!("{}: failed to stat", path.display()))?
        .len();
    if size > MAX_XLSX_BYTES {
        anyhow::bail!(
            "spreadsheet is {} MB — over the {} MB preview cap (export to CSV for larger data)",
            size / (1024 * 1024),
            MAX_XLSX_BYTES / (1024 * 1024),
        );
    }

    preflight_workbook_expansion(&path)?;

    let mut workbook = calamine::open_workbook_auto(&path)
        .with_context(|| format!("{}: not a readable spreadsheet", path.display()))?;
    let sheets: Vec<String> = workbook.sheet_names().to_vec();
    let sheet_name = match sheet {
        Some(s) if sheets.iter().any(|x| x == s) => s.to_string(),
        Some(s) => anyhow::bail!("no sheet named {s:?}"),
        None => sheets
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("spreadsheet has no sheets"))?,
    };
    let range = workbook
        .worksheet_range(&sheet_name)
        .with_context(|| format!("failed to read sheet {sheet_name:?}"))?;

    let mut row_iter = range.rows();
    // First row is the header, matching the CSV table viewer.
    let columns: Vec<String> = match row_iter.next() {
        Some(header) => header.iter().map(xlsx_cell).collect(),
        None => Vec::new(),
    };
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut truncated = false;
    for (index, row) in row_iter.enumerate() {
        if index < offset_rows {
            continue;
        }
        if rows.len() >= limit_rows {
            truncated = true;
            break;
        }
        rows.push(row.iter().map(xlsx_cell).collect());
    }

    Ok(json!({
        "sheets": sheets,
        "sheet": sheet_name,
        "columns": columns,
        "rows": rows,
        "offset": offset_rows,
        "truncated": truncated,
    }))
}

/// Budget ZIP-backed spreadsheet formats before calamine decompresses them.
/// Legacy `.xls` is not a ZIP container and is already bounded directly by
/// [`MAX_XLSX_BYTES`]. Central-directory sizes are cheap to inspect and give
/// us a hard expansion/entry ceiling for xlsx/xlsm/ods.
fn preflight_workbook_expansion(path: &Path) -> anyhow::Result<()> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(extension.as_deref(), Some("xlsx" | "xlsm" | "ods")) {
        return Ok(());
    }

    let file =
        std::fs::File::open(path).with_context(|| format!("{}: failed to open", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("{}: invalid spreadsheet ZIP", path.display()))?;
    if archive.len() > MAX_XLSX_ENTRIES {
        anyhow::bail!(
            "spreadsheet contains {} ZIP entries — over the {MAX_XLSX_ENTRIES}-entry preview cap",
            archive.len()
        );
    }
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index_raw(index)
            .with_context(|| format!("{}: invalid ZIP entry", path.display()))?;
        expanded = expanded.saturating_add(entry.size());
        if expanded > MAX_XLSX_EXPANDED_BYTES {
            anyhow::bail!(
                "spreadsheet expands past the {} MB preview cap (export to CSV for larger data)",
                MAX_XLSX_EXPANDED_BYTES / (1024 * 1024)
            );
        }
    }
    Ok(())
}

/// `.tsv` -> tab, `.csv` -> comma, judged from the end of a file name.
fn delimiter_from_name(name: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tsv") {
        Some(b'\t')
    } else if lower.ends_with(".csv") {
        Some(b',')
    } else {
        None
    }
}

/// Pick the delimiter: the effective file name decides by extension (for gz
/// that is the path minus its .gz/.bgz suffix — `foo.tsv.gz` -> tsv — then
/// the gzip member's stored FNAME); with no telling name, sniff the first
/// (decoded) line that is not a comment: any tab means tab, otherwise comma.
fn sniff_delimiter(path: &Path, gz: bool, comments: &[Vec<u8>]) -> anyhow::Result<u8> {
    let effective = if gz {
        gz_inner_from_path(path)
    } else {
        path.file_name().map(|n| n.to_string_lossy().into_owned())
    };
    if let Some(delim) = effective.as_deref().and_then(delimiter_from_name) {
        return Ok(delim);
    }
    if gz {
        if let Some(delim) = gz_inner_from_header(path)
            .as_deref()
            .and_then(delimiter_from_name)
        {
            return Ok(delim);
        }
    }
    let file =
        std::fs::File::open(path).with_context(|| format!("{}: failed to open", path.display()))?;
    let input: Box<dyn Read> = if gz {
        Box::new(MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    };
    // One 64 KB window for the whole sniff, comment lines included.
    let mut window = std::io::BufReader::new(input).take(64 * 1024);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = window
            .read_until(b'\n', &mut line)
            .with_context(|| format!("{}: failed to read", path.display()))?;
        if n == 0 || !comments.iter().any(|p| line.starts_with(p)) {
            break;
        }
    }
    Ok(if line.contains(&b'\t') { b'\t' } else { b',' })
}

#[derive(Deserialize)]
pub(crate) struct ValidateRequest {
    candidates: Vec<String>,
    base: String,
    /// Additive: more absolute directories to resolve relative candidates
    /// against, tried in order after `base` (see [`validate`]).
    #[serde(default)]
    bases: Option<Vec<String>>,
    /// Additive (older clients omit it, older daemons ignore it): enables the
    /// workspace-index fallbacks below, scoped to this workspace's index.
    #[serde(default)]
    workspace_id: Option<String>,
    /// Additive: only the exact join onto each base — no diff-prefix strip,
    /// no workspace-index fallback. A document's own link names one file; a
    /// broken `b/spec.md` must stay broken rather than open `spec.md`.
    #[serde(default)]
    strict: bool,
}

/// A candidate eligible for the bare-basename fallback: a single path segment
/// (no `/`), not a dotfile / `~` form / flag-like token, shaped like
/// `name.ext` with a letter-led extension of at most 16 alphanumerics (long
/// enough for `.safetensors`) — never wider than what the clients already
/// treat as path-like. Prose words (`docs`, `license`) and version numbers
/// (`1.2.3`) never qualify.
fn bare_basename(candidate: &str) -> bool {
    if candidate.contains('/') {
        return false;
    }
    if candidate.starts_with(['.', '~', '-']) {
        return false;
    }
    let Some((stem, ext)) = candidate.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && (1..=16).contains(&ext.len())
        && ext.starts_with(|c: char| c.is_ascii_alphabetic())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The part of a candidate the path-suffix fallback matches, if eligible: it
/// contains `/` and is not absolute, `~`-rooted or dot-relative (those name
/// one place exactly). A trailing `/` (a directory mention) is dropped.
fn suffix_candidate(candidate: &str) -> Option<&str> {
    if !candidate.contains('/') || candidate.starts_with(['/', '~', '.']) {
        return None;
    }
    let trimmed = candidate.trim_end_matches('/');
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Canonicalize (resolving symlinks and `..`, and proving existence) and
/// classify; anything unresolvable is `None`, never an error.
fn resolve_entry(path: &Path) -> Option<(PathBuf, &'static str)> {
    let resolved = std::fs::canonicalize(path).ok()?;
    let kind = if resolved.is_dir() { "dir" } else { "file" };
    Some((resolved, kind))
}

fn entry_json(path: &Path, kind: &str) -> serde_json::Value {
    json!({"path": path.to_string_lossy(), "kind": kind})
}

/// The direct rungs of the ladder: an absolute or `~` candidate as-is; else
/// joined onto each base in order; else (unless `strict`), for a git diff
/// prefix (`a/`, `b/`), the remainder joined onto each base. First hit wins.
fn resolve_direct(
    candidate: &str,
    bases: &[PathBuf],
    strict: bool,
) -> Option<(PathBuf, &'static str)> {
    let expanded = expand_tilde(candidate).ok()?;
    if expanded.is_absolute() {
        return resolve_entry(&expanded);
    }
    if let Some(hit) = bases
        .iter()
        .find_map(|base| resolve_entry(&base.join(&expanded)))
    {
        return Some(hit);
    }
    if strict {
        return None;
    }
    let rest = candidate
        .strip_prefix("a/")
        .or_else(|| candidate.strip_prefix("b/"))
        .filter(|rest| !rest.is_empty())?;
    bases
        .iter()
        .find_map(|base| resolve_entry(&base.join(rest)))
}

/// What the workspace index says about one candidate.
enum IndexAnswer {
    Unique(serde_json::Value),
    Ambiguous(Vec<serde_json::Value>),
    Miss,
}

/// Judge index matches: sort shortest path first then lexicographically,
/// re-canonicalize (the index is served stale, so only entries that exist
/// RIGHT NOW count, answered canonically like the direct rungs) until
/// [`MAX_AMBIGUOUS`] are confirmed, probing at most [`MAX_AMBIGUOUS_PROBES`].
/// One confirmed match among all probed is unique; one with matches left
/// unprobed is a miss (uniqueness unproven — the false-positive defense).
fn judge_index_matches(
    mut matches: Vec<&crate::quickopen::IndexedFile>,
    files_only: bool,
) -> IndexAnswer {
    matches.sort_by(|a, b| {
        a.path
            .len()
            .cmp(&b.path.len())
            .then_with(|| a.path.cmp(&b.path))
    });
    let mut found: Vec<(PathBuf, &'static str)> = Vec::new();
    for entry in matches.iter().take(MAX_AMBIGUOUS_PROBES) {
        let Some((resolved, kind)) = resolve_entry(Path::new(&entry.path)) else {
            continue;
        };
        if files_only && kind != "file" {
            continue;
        }
        if found.iter().any(|(path, _)| *path == resolved) {
            continue;
        }
        found.push((resolved, kind));
        if found.len() == MAX_AMBIGUOUS {
            break;
        }
    }
    match found.as_slice() {
        [] => IndexAnswer::Miss,
        [(path, kind)] if matches.len() <= MAX_AMBIGUOUS_PROBES => {
            IndexAnswer::Unique(entry_json(path, kind))
        }
        [_] => IndexAnswer::Miss,
        several => IndexAnswer::Ambiguous(
            several
                .iter()
                .map(|(path, kind)| entry_json(path, kind))
                .collect(),
        ),
    }
}

/// POST /api/v1/fs/validate {candidates, base, bases?, workspace_id?, strict?} —
/// batched existence check behind the terminal, chat and document link
/// providers: only path-like strings that resolve to something real get
/// underlined. Answers `{valid: {[cand]: {path, kind}}, ambiguous: {[cand]:
/// [{path, kind}]}}`: `path` is canonical and absolute, `kind` is `file` or
/// `dir`; misses are simply absent. Candidates past
/// [`MAX_VALIDATE_CANDIDATES`] or longer than
/// [`MAX_VALIDATE_CANDIDATE_BYTES`] are ignored: cheap and batched by design.
///
/// The ladder, per candidate, first hit wins:
/// 1. absolute or `~` → as-is;
/// 2. joined onto `base` (required, absolute), then onto each of `bases`
///    (absolute dirs only, deduped, at most [`MAX_VALIDATE_BASES`]) in order
///    — the cwd when the text was written, the document's folder, …;
/// 3. a git diff prefix (`a/`, `b/`) stripped, the rest joined onto each base;
/// 4. with `workspace_id`, the workspace's quickopen index: a
///    [`bare_basename`] matches files with exactly that name; a partial path
///    ([`suffix_candidate`]) matches entries whose workspace-relative path
///    equals it or ends with `/` + it (`figs/plot.png` for
///    `results/figs/plot.png`). One match → `valid`; several → `ambiguous`
///    (at most [`MAX_AMBIGUOUS`], shortest path first) for the client to
///    offer a choice, never an arbitrary pick.
///
/// `strict` (document links) stops after rung 2: a link a document spells
/// out must name exactly that file. Chat and terminal text stay lenient.
///
/// Bounds: the index is the quickopen walk — entry/depth/time-capped,
/// ignore-respecting (so `target/`, `work/` and symlinked trees are
/// invisible to rung 4), served stale-while-revalidating per workspace (up to
/// two minutes plus one walk behind the disk on a slow tree), fetched at most
/// once per request, only when a rung-4-eligible candidate reached it, and
/// never waited on (a cold index answers nothing this round). The whole
/// request runs under the shared filesystem limiter with a
/// [`VALIDATE_BUDGET`] wall-clock budget; candidates not reached in time are
/// misses this round (clients retry misses).
pub(crate) async fn validate(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ValidateRequest>,
) -> Response {
    if !Path::new(&body.base).is_absolute() {
        return bad_request(&anyhow::anyhow!(
            "base {:?} is not an absolute path",
            body.base
        ));
    }
    // Every resolution stats the disk and a fallback may walk a (bounded)
    // tree — NFS-slow work that must stay off the async reactor.
    blocking_json(move || {
        let deadline = Instant::now() + VALIDATE_BUDGET;
        let mut bases = vec![PathBuf::from(&body.base)];
        for extra in body.bases.iter().flatten() {
            if bases.len() > MAX_VALIDATE_BASES {
                break;
            }
            let extra = Path::new(extra);
            if extra.is_absolute() && !bases.iter().any(|b| b == extra) {
                bases.push(extra.to_path_buf());
            }
        }
        let mut valid = serde_json::Map::new();
        let mut ambiguous = serde_json::Map::new();
        // Lazily fetched, at most once per request; inner None = unknown
        // workspace or an index still being built (fallback off this round —
        // degrade, don't error).
        let mut index: Option<Option<Arc<Vec<crate::quickopen::IndexedFile>>>> = None;
        for candidate in body.candidates.iter().take(MAX_VALIDATE_CANDIDATES) {
            if candidate.is_empty()
                || candidate.len() > MAX_VALIDATE_CANDIDATE_BYTES
                || valid.contains_key(candidate)
                || ambiguous.contains_key(candidate)
            {
                continue;
            }
            if Instant::now() >= deadline {
                break;
            }
            if let Some((path, kind)) = resolve_direct(candidate, &bases, body.strict) {
                valid.insert(candidate.clone(), entry_json(&path, kind));
                continue;
            }
            if body.strict {
                continue;
            }
            let Some(workspace_id) = body.workspace_id.as_deref() else {
                continue;
            };
            let basename = bare_basename(candidate);
            let suffix = if basename {
                None
            } else {
                suffix_candidate(candidate)
            };
            if !basename && suffix.is_none() {
                continue;
            }
            let files = index.get_or_insert_with(|| {
                crate::quickopen::workspace_index_if_free(&state, workspace_id)
            });
            let Some(files) = files.as_deref() else {
                continue;
            };
            let answer = match suffix {
                None => judge_index_matches(crate::quickopen::files_named(files, candidate), true),
                Some(suffix) => {
                    judge_index_matches(crate::quickopen::entries_with_suffix(files, suffix), false)
                }
            };
            match answer {
                IndexAnswer::Unique(hit) => {
                    valid.insert(candidate.clone(), hit);
                }
                IndexAnswer::Ambiguous(hits) => {
                    ambiguous.insert(candidate.clone(), serde_json::Value::Array(hits));
                }
                IndexAnswer::Miss => {}
            }
        }
        Ok(json!({"valid": valid, "ambiguous": ambiguous}))
    })
    .await
}

#[derive(Deserialize)]
pub(crate) struct MkdirRequest {
    path: String,
}

/// POST /api/v1/fs/mkdir {path} — create a directory (with any missing
/// parents) and return its canonical path. The daemon runs as the user, so
/// this is scoped to their own filesystem permissions — the same trust model
/// as writing a file via PUT /fs/file. Idempotent: an already-existing
/// directory is a success. Backs the folder picker's "create folder" action,
/// so a workspace can be opened on a path that does not exist yet.
pub(crate) async fn mkdir(Json(body): Json<MkdirRequest>) -> Response {
    let work = move || -> anyhow::Result<serde_json::Value> {
        let expanded = expand_tilde(&body.path)?;
        if expanded.as_os_str().is_empty() {
            anyhow::bail!("empty path");
        }
        std::fs::create_dir_all(&expanded)
            .with_context(|| format!("{}: failed to create directory", expanded.display()))?;
        // Canonicalize what we just made so the caller opens the resolved path
        // (symlinks/`..` collapsed), matching create_workspace's own view.
        let path =
            std::fs::canonicalize(&expanded).with_context(|| expanded.display().to_string())?;
        if !path.is_dir() {
            anyhow::bail!("{} is not a directory", path.display());
        }
        Ok(json!({ "path": path.to_string_lossy() }))
    };
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(err)) => bad_request(&err),
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("mkdir task failed: {join}")})),
        )
            .into_response(),
    }
}

/// Outcome of a create/rename/delete mutation: done (with the response
/// body), or a name conflict the UI surfaces as an inline error (409).
enum MutateOutcome {
    Done(serde_json::Value),
    Conflict(String),
}

/// Run a blocking filesystem mutation and map its outcome onto the shared
/// response shape (200 Json / 409 conflict / 400 error). On success the
/// touched paths nudge the git watcher (same reason as `put_file`: the
/// tree/panel refetch without polling).
async fn run_mutation<F>(state: &AppState, work: F, dirty: &[&str]) -> Response
where
    F: FnOnce() -> anyhow::Result<MutateOutcome> + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(MutateOutcome::Done(body))) => {
            for path in dirty {
                crate::git::mark_path_dirty(state, path).await;
            }
            if body.is_null() {
                StatusCode::NO_CONTENT.into_response()
            } else {
                Json(body).into_response()
            }
        }
        Ok(Ok(MutateOutcome::Conflict(msg))) => {
            (StatusCode::CONFLICT, Json(json!({ "error": msg }))).into_response()
        }
        Ok(Err(err)) => bad_request(&err),
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("fs task failed: {join}")})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum CreateKind {
    File,
    Dir,
}

#[derive(Deserialize)]
pub(crate) struct CreateRequest {
    path: String,
    kind: CreateKind,
}

/// POST /api/v1/fs/create {path, kind:"file"|"dir"} — create an empty file or
/// directory, making any missing parent directories (the inline "new file"
/// input accepts nested `a/b/c.txt` names). Unlike `mkdir` this is an explicit
/// user "New File/Folder", so an already-existing target is a 409 conflict,
/// never a silent success. Returns the canonical created path. Same trust
/// model as PUT /fs/file: the daemon runs as the user.
pub(crate) async fn create(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateRequest>,
) -> Response {
    let raw = body.path.clone();
    let work = move || -> anyhow::Result<MutateOutcome> {
        let expanded = expand_tilde(&body.path)?;
        if expanded.as_os_str().is_empty() {
            anyhow::bail!("empty path");
        }
        if let Some(parent) = expanded.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("{}: failed to create parent", parent.display()))?;
        }
        let conflict = || MutateOutcome::Conflict(format!("{} already exists", expanded.display()));
        match body.kind {
            CreateKind::File => {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&expanded)
                {
                    Ok(_) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Ok(conflict());
                    }
                    Err(err) => {
                        return Err(anyhow::Error::new(err)
                            .context(format!("{}: failed to create file", expanded.display())));
                    }
                }
            }
            CreateKind::Dir => match std::fs::create_dir(&expanded) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Ok(conflict());
                }
                Err(err) => {
                    return Err(anyhow::Error::new(err).context(format!(
                        "{}: failed to create directory",
                        expanded.display()
                    )));
                }
            },
        }
        let path =
            std::fs::canonicalize(&expanded).with_context(|| expanded.display().to_string())?;
        Ok(MutateOutcome::Done(
            json!({ "path": path.to_string_lossy() }),
        ))
    };
    run_mutation(&state, work, &[&raw]).await
}

#[derive(Deserialize)]
pub(crate) struct RenameRequest {
    from: String,
    to: String,
}

/// POST /api/v1/fs/rename {from, to} — rename (or move) a file or directory.
/// `to` is a full path whose parent must already exist; an existing target is
/// a 409 (except a case-only rename of the same file on a case-insensitive
/// filesystem, which must go through). Symlinks are renamed as themselves,
/// never their targets. Cross-filesystem moves are refused with a friendly
/// error rather than silently degrading to copy+delete. Returns the canonical
/// new path.
pub(crate) async fn rename(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RenameRequest>,
) -> Response {
    let (raw_from, raw_to) = (body.from.clone(), body.to.clone());
    let work = move || -> anyhow::Result<MutateOutcome> {
        let from = canonical_parent_join(&body.from)?;
        if std::fs::symlink_metadata(&from).is_err() {
            anyhow::bail!("{}: No such file or directory", from.display());
        }
        let to = canonical_parent_join(&body.to)?;
        if std::fs::symlink_metadata(&to).is_ok() {
            // canonicalize sees through case-insensitive filesystems: when the
            // "existing" target is the source itself, this is a case-only
            // rename (foo.txt -> Foo.txt) and must proceed.
            let same = std::fs::canonicalize(&to)
                .is_ok_and(|resolved| std::fs::canonicalize(&from).is_ok_and(|f| f == resolved));
            if !same {
                return Ok(MutateOutcome::Conflict(format!(
                    "{} already exists",
                    to.display()
                )));
            }
        }
        match std::fs::rename(&from, &to) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => {
                anyhow::bail!(
                    "{} → {}: cannot move across filesystems — copy instead",
                    from.display(),
                    to.display()
                );
            }
            Err(err) => {
                return Err(anyhow::Error::new(err).context(format!(
                    "failed to rename {} to {}",
                    from.display(),
                    to.display()
                )));
            }
        }
        // Return the parent-resolved path as-is: canonicalizing now would
        // resolve a renamed symlink to its target.
        Ok(MutateOutcome::Done(json!({ "path": to.to_string_lossy() })))
    };
    run_mutation(&state, work, &[&raw_from, &raw_to]).await
}

#[derive(Deserialize)]
pub(crate) struct DeleteRequest {
    path: String,
}

/// POST /api/v1/fs/delete {path} — permanently delete a file, symlink (the
/// link itself, never its target), or directory (recursively). There is no
/// server-side trash; the UI fronts this with an explicit confirmation.
/// Refuses `/` (structurally: it has no file name) and the user's home
/// directory. 204 on success.
pub(crate) async fn delete(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DeleteRequest>,
) -> Response {
    let raw = body.path.clone();
    let work = move || -> anyhow::Result<MutateOutcome> {
        let target = canonical_parent_join(&body.path)?;
        let home = home_dir()
            .and_then(|h| std::fs::canonicalize(&h).with_context(|| h.display().to_string()));
        if home.is_ok_and(|h| h == target) {
            anyhow::bail!("refusing to delete your home directory");
        }
        let meta = std::fs::symlink_metadata(&target)
            .with_context(|| format!("{}: No such file or directory", target.display()))?;
        if meta.is_dir() {
            std::fs::remove_dir_all(&target)
                .with_context(|| format!("{}: failed to delete directory", target.display()))?;
        } else {
            // Regular files AND symlinks (remove_file unlinks the link itself).
            std::fs::remove_file(&target)
                .with_context(|| format!("{}: failed to delete", target.display()))?;
        }
        Ok(MutateOutcome::Done(serde_json::Value::Null))
    };
    run_mutation(&state, work, &[&raw]).await
}

/// Hard ceiling on entries a single copy/move walk may touch — the same
/// runaway backstop the zip builder uses, so a pathological tree aborts
/// loudly instead of pinning a blocking thread indefinitely.
const MAX_COPY_ENTRIES: usize = 250_000;

/// How a copy resolves a target that already exists.
#[derive(Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
enum OnConflict {
    /// 409 (the default) — the UI decides what to do.
    #[default]
    Fail,
    /// Auto-pick a free "name copy"/"name copy 2" sibling (macOS semantics).
    Unique,
}

/// A collision-free variant of `to`: `to` itself when free, else
/// `stem copy.ext`, `stem copy 2.ext`, … in `to`'s parent. Probed on disk.
fn unique_dest(to: &Path) -> PathBuf {
    if std::fs::symlink_metadata(to).is_err() {
        return to.to_path_buf();
    }
    let parent = to.parent().unwrap_or(Path::new("."));
    let stem = to
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Keep a compound extension whole (foo.tar.gz -> foo copy.tar.gz) by
    // splitting on the FIRST dot after the stem, not file_stem/extension.
    let full = to.file_name().map(|n| n.to_string_lossy().into_owned());
    let ext = full
        .as_deref()
        .and_then(|n| n.strip_prefix(&stem))
        .filter(|s| s.starts_with('.'))
        .map(str::to_string)
        .unwrap_or_default();
    for n in 1..10_000 {
        let name = if n == 1 {
            format!("{stem} copy{ext}")
        } else {
            format!("{stem} copy {n}{ext}")
        };
        let candidate = parent.join(name);
        if std::fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    to.to_path_buf() // give up after 10k — the copy then fails loudly
}

/// Recursively copy `src` to `dst` (which must not yet exist). Symlinks are
/// recreated as links, never followed (matching the download zip walk); files
/// stream through `std::io::copy` (bounded internal buffer — never `read` the
/// whole file into RAM); `entries` counts against [`MAX_COPY_ENTRIES`].
fn copy_recursive(src: &Path, dst: &Path, entries: &mut usize) -> anyhow::Result<()> {
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((from, to)) = stack.pop() {
        *entries += 1;
        if *entries > MAX_COPY_ENTRIES {
            anyhow::bail!("more than {MAX_COPY_ENTRIES} entries — refusing to copy");
        }
        let meta = std::fs::symlink_metadata(&from)
            .with_context(|| format!("{}: cannot read", from.display()))?;
        if meta.file_type().is_symlink() {
            let link = std::fs::read_link(&from)?;
            std::os::unix::fs::symlink(&link, &to)
                .with_context(|| format!("{}: failed to recreate symlink", to.display()))?;
        } else if meta.is_dir() {
            std::fs::create_dir(&to)
                .with_context(|| format!("{}: failed to create directory", to.display()))?;
            for entry in std::fs::read_dir(&from)
                .with_context(|| format!("{}: failed to read directory", from.display()))?
            {
                let entry = entry?;
                stack.push((entry.path(), to.join(entry.file_name())));
            }
        } else {
            let mut reader = std::fs::File::open(&from)
                .with_context(|| format!("{}: failed to open", from.display()))?;
            let mut writer = std::fs::File::create(&to)
                .with_context(|| format!("{}: failed to create", to.display()))?;
            std::io::copy(&mut reader, &mut writer)
                .with_context(|| format!("{} → {}: copy failed", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// `child` is `ancestor` itself or lies beneath it (both already canonical) —
/// the guard against copying/moving a directory into its own subtree.
fn is_within(child: &Path, ancestor: &Path) -> bool {
    child == ancestor || child.starts_with(ancestor)
}

#[derive(Deserialize)]
pub(crate) struct CopyRequest {
    from: String,
    to: String,
    #[serde(default)]
    on_conflict: OnConflict,
}

/// POST /api/v1/fs/copy {from, to, on_conflict?} — copy a file, symlink (as a
/// link), or directory (recursively) to `to`. `on_conflict:"unique"` picks a
/// free "name copy" sibling instead of 409-ing. Refuses copying a directory
/// into itself or a descendant. Returns the canonical new path.
pub(crate) async fn copy(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CopyRequest>,
) -> Response {
    let (raw_from, raw_to) = (body.from.clone(), body.to.clone());
    let work = move || -> anyhow::Result<MutateOutcome> {
        let from = canonical_parent_join(&body.from)?;
        if std::fs::symlink_metadata(&from).is_err() {
            anyhow::bail!("{}: No such file or directory", from.display());
        }
        let mut to = canonical_parent_join(&body.to)?;
        // Guard against copying a directory into its own subtree (the canonical
        // source vs the canonical destination PARENT — `to` itself doesn't
        // exist yet).
        if let (Ok(src_c), Some(parent)) = (std::fs::canonicalize(&from), to.parent()) {
            if let Ok(dst_parent_c) = std::fs::canonicalize(parent) {
                if is_within(&dst_parent_c, &src_c) {
                    anyhow::bail!("cannot copy a directory into itself");
                }
            }
        }
        if std::fs::symlink_metadata(&to).is_ok() {
            match body.on_conflict {
                OnConflict::Unique => to = unique_dest(&to),
                OnConflict::Fail => {
                    return Ok(MutateOutcome::Conflict(format!(
                        "{} already exists",
                        to.display()
                    )));
                }
            }
        }
        let mut entries = 0usize;
        if let Err(err) = copy_recursive(&from, &to, &mut entries) {
            // Leave no half-copy behind on failure.
            let _ = std::fs::remove_dir_all(&to).or_else(|_| std::fs::remove_file(&to));
            return Err(err);
        }
        Ok(MutateOutcome::Done(json!({ "path": to.to_string_lossy() })))
    };
    run_mutation(&state, work, &[&raw_from, &raw_to]).await
}

#[derive(Deserialize)]
pub(crate) struct MoveRequest {
    from: String,
    to: String,
}

/// POST /api/v1/fs/move {from, to} — move a file/symlink/directory. Tries a
/// plain rename; on a cross-filesystem boundary falls back to a guarded
/// recursive copy then deletes the source (only after the copy fully
/// succeeds). Refuses moving the home directory or a directory into itself.
/// 409 if `to` already exists. Returns the canonical new path.
pub(crate) async fn move_(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MoveRequest>,
) -> Response {
    let (raw_from, raw_to) = (body.from.clone(), body.to.clone());
    let work = move || -> anyhow::Result<MutateOutcome> {
        let from = canonical_parent_join(&body.from)?;
        if std::fs::symlink_metadata(&from).is_err() {
            anyhow::bail!("{}: No such file or directory", from.display());
        }
        let home = home_dir()
            .and_then(|h| std::fs::canonicalize(&h).with_context(|| h.display().to_string()));
        if home.is_ok_and(|h| std::fs::canonicalize(&from).is_ok_and(|f| f == h)) {
            anyhow::bail!("refusing to move your home directory");
        }
        let to = canonical_parent_join(&body.to)?;
        if let (Ok(src_c), Some(parent)) = (std::fs::canonicalize(&from), to.parent()) {
            if let Ok(dst_parent_c) = std::fs::canonicalize(parent) {
                if is_within(&dst_parent_c, &src_c) {
                    anyhow::bail!("cannot move a directory into itself");
                }
            }
        }
        if std::fs::symlink_metadata(&to).is_ok() {
            let same = std::fs::canonicalize(&to)
                .is_ok_and(|resolved| std::fs::canonicalize(&from).is_ok_and(|f| f == resolved));
            if !same {
                return Ok(MutateOutcome::Conflict(format!(
                    "{} already exists",
                    to.display()
                )));
            }
        }
        match std::fs::rename(&from, &to) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => {
                // Copy across the boundary, then unlink the source — but only
                // once the copy has fully succeeded (never lose data on a
                // partial copy).
                let mut entries = 0usize;
                if let Err(err) = copy_recursive(&from, &to, &mut entries) {
                    let _ = std::fs::remove_dir_all(&to).or_else(|_| std::fs::remove_file(&to));
                    return Err(err);
                }
                let src_meta = std::fs::symlink_metadata(&from)?;
                if src_meta.is_dir() {
                    std::fs::remove_dir_all(&from).with_context(|| {
                        format!("{}: copied but failed to remove", from.display())
                    })?;
                } else {
                    std::fs::remove_file(&from).with_context(|| {
                        format!("{}: copied but failed to remove", from.display())
                    })?;
                }
            }
            Err(err) => {
                return Err(anyhow::Error::new(err).context(format!(
                    "failed to move {} to {}",
                    from.display(),
                    to.display()
                )));
            }
        }
        Ok(MutateOutcome::Done(json!({ "path": to.to_string_lossy() })))
    };
    run_mutation(&state, work, &[&raw_from, &raw_to]).await
}

/// In-memory store of short-lived raw-access tickets. A ticket is bound to
/// one canonical file path and expires after [`TICKET_TTL`]; expired entries
/// are purged on every create/lookup.
#[derive(Default)]
pub(crate) struct TicketStore {
    tickets: HashMap<String, Ticket>,
}

struct Ticket {
    path: PathBuf,
    expires: Instant,
}

impl TicketStore {
    /// Mint a ticket for `path`, valid for `ttl`.
    fn create(&mut self, path: PathBuf, ttl: Duration) -> String {
        self.purge();
        if self.tickets.len() >= MAX_TICKETS {
            // Expiries preserve creation order for a common TTL. Evicting the
            // soonest-to-expire capability keeps the store bounded while
            // retaining the freshest previews/downloads.
            if let Some(oldest) = self
                .tickets
                .iter()
                .min_by_key(|(_, ticket)| ticket.expires)
                .map(|(key, _)| key.clone())
            {
                self.tickets.remove(&oldest);
            }
        }
        let ticket = format!("t-{}", &chimaera_core::generate_token()[..32]);
        self.tickets.insert(
            ticket.clone(),
            Ticket {
                path,
                expires: Instant::now() + ttl,
            },
        );
        ticket
    }

    /// The path bound to `ticket`, if it exists and has not expired.
    /// Shared with the download module — same store, same capability model.
    pub(crate) fn lookup(&mut self, ticket: &str) -> Option<PathBuf> {
        self.purge();
        self.tickets.get(ticket).map(|t| t.path.clone())
    }

    fn purge(&mut self) {
        let now = Instant::now();
        self.tickets.retain(|_, t| t.expires > now);
    }

    /// Force a ticket to be already expired (test hook for the expiry path).
    #[cfg(test)]
    pub(crate) fn expire(&mut self, ticket: &str) {
        if let Some(t) = self.tickets.get_mut(ticket) {
            t.expires = Instant::now() - Duration::from_secs(1);
        }
    }
}

#[cfg(test)]
mod ticket_store_tests {
    use super::*;

    #[test]
    fn ticket_store_evicts_oldest_at_hard_cap() {
        let mut store = TicketStore::default();
        let first = store.create(PathBuf::from("/first"), TICKET_TTL);
        for n in 1..=MAX_TICKETS {
            store.create(PathBuf::from(format!("/{n}")), TICKET_TTL);
        }
        assert_eq!(store.tickets.len(), MAX_TICKETS);
        assert!(
            store.lookup(&first).is_none(),
            "oldest ticket was not evicted"
        );
    }
}

#[derive(Deserialize)]
pub(crate) struct TicketRequest {
    path: String,
}

/// POST /api/v1/fs/ticket {path} — mint a 10-minute access ticket for a file
/// or directory, so iframes, img tags, and <a href> download navigations
/// (none of which can send Authorization headers) can fetch it via GET
/// /raw/{ticket} (files only) or GET /download/{ticket}. The bearer token
/// never appears in a URL. A ticket is a per-path snapshot: renaming the
/// path afterwards makes the fetch 404, deliberately.
pub(crate) async fn create_ticket(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TicketRequest>,
) -> Response {
    let path = match tokio::task::spawn_blocking(move || canonical(&body.path)).await {
        Ok(Ok(path)) => path,
        Ok(Err(err)) => return bad_request(&err),
        Err(join) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("ticket path task failed: {join}")})),
            )
                .into_response();
        }
    };
    let ticket = crate::lock(&state.tickets).create(path, TICKET_TTL);
    Json(json!({"ticket": ticket})).into_response()
}

/// Parse a single `Range: bytes=...` header value against a file of `total`
/// bytes into an inclusive (start, end) pair. `None` means "serve the whole
/// file" (no/unusable range — RFC 9110 lets a server ignore malformed or
/// multi-part ranges); `Some(Err(()))` means unsatisfiable (416).
fn parse_byte_range(value: &str, total: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = value.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None; // multipart ranges: not worth it, serve the whole file
    }
    let (start, end) = spec.split_once('-')?;
    let range = if start.is_empty() {
        // Suffix form: the last N bytes.
        let suffix: u64 = end.parse().ok()?;
        if suffix == 0 || total == 0 {
            return Some(Err(()));
        }
        (total.saturating_sub(suffix), total - 1)
    } else {
        let start: u64 = start.parse().ok()?;
        let end: u64 = if end.is_empty() {
            total.saturating_sub(1)
        } else {
            end.parse().ok()?
        };
        if start >= total || start > end {
            return Some(Err(()));
        }
        (start, end.min(total.saturating_sub(1)))
    };
    Some(Ok(range))
}

/// GET /raw/{ticket} — the ticketed file's bytes, no bearer auth (mounted
/// outside the /api auth layer). Content-Type comes from the extension. HTML
/// is confined with `Content-Security-Policy: sandbox allow-scripts` and no
/// referrer; SVG gets a script-less sandbox (scripts never run in <img>, but
/// direct navigation should not run them either). Single byte ranges are
/// honored (206/416; pdf.js fetches pages lazily this way). 404 on unknown
/// or expired tickets, and on files that vanished since the ticket was minted.
pub(crate) async fn raw(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(ticket): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    let Some(path) = crate::lock(&state.tickets).lookup(&ticket) else {
        return not_found();
    };
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "ticketed file unreadable");
            return not_found();
        }
    };
    let total = match file.metadata().await {
        // Tickets may now name directories (folder downloads); /raw itself
        // stays file-only — a dir ticket here is a 404, not a listing.
        Ok(meta) if meta.is_file() => meta.len(),
        Ok(_) => return not_found(),
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "ticketed file unstattable");
            return not_found();
        }
    };

    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| parse_byte_range(v, total));
    let (status, span) = match range {
        None => (StatusCode::OK, (0, total.saturating_sub(1))),
        Some(Ok(span)) => (StatusCode::PARTIAL_CONTENT, span),
        Some(Err(())) => {
            let mut response = (
                StatusCode::RANGE_NOT_SATISFIABLE,
                Json(json!({"error": "range not satisfiable"})),
            )
                .into_response();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{total}")) {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            return response;
        }
    };

    let (start, end) = span;
    let len = if total == 0 { 0 } else { end - start + 1 };
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    if let Err(err) = file.seek(SeekFrom::Start(start)).await {
        tracing::warn!(path = %path.display(), %err, "ticketed file read failed");
        return not_found();
    }

    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    // Stream the selected span. The previous `vec![0; len]` loaded an
    // un-ranged file (or attacker-chosen large range) wholly into daemon RSS.
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file.take(len)));
    let mut response = (
        status,
        [
            (header::CONTENT_TYPE, mime.essence_str().to_string()),
            (header::ACCEPT_RANGES, "bytes".to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
        ],
        body,
    )
        .into_response();
    if status == StatusCode::PARTIAL_CONTENT {
        if let Ok(value) = HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")) {
            response.headers_mut().insert(header::CONTENT_RANGE, value);
        }
    }
    let sandbox = match mime.essence_str() {
        "text/html" => Some(HeaderValue::from_static("sandbox allow-scripts")),
        "image/svg+xml" => Some(HeaderValue::from_static("sandbox")),
        _ => None,
    };
    if let Some(csp) = sandbox {
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_SECURITY_POLICY, csp);
        headers.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
    }
    response
}
