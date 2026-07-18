//! Filesystem endpoints: the folder picker (home + directories-only listing),
//! and the file service backing file tabs — full directory listings, ranged
//! raw reads, atomic single-file writes (lightweight editing), file management
//! (create/rename/delete behind the file-manager context menus), server-
//! rendered markdown, paged CSV/TSV tables (with a transparent gzip tier for
//! .gz/.bgz), and short-lived tickets that let iframes/img tags fetch bytes
//! without a bearer header.

use std::collections::HashMap;
use std::io::{BufRead, Read, Seek, SeekFrom};
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
/// Maximum bytes a paged table request scans from the start of a plain file.
/// CSV has no row index, so a hostile giant `offset_rows` would otherwise tie
/// up one blocking worker walking an arbitrarily large dataset.
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
/// Hard cap on entries returned by a single `fs/dirs` / `fs/list` listing.
/// The daemon runs on shared login nodes over NFS/Lustre where a scratch dir
/// can hold hundreds of thousands of entries; without a ceiling a single
/// listing balloons the response and the allocation. Past this the answer is
/// honestly `truncated`.
const MAX_DIR_ENTRIES: usize = 4000;
/// How long a raw-access ticket stays valid.
const TICKET_TTL: Duration = Duration::from_secs(600);
/// In-memory capability ceiling. A buggy or hostile bearer-authenticated
/// client must not grow the ticket map without bound during that TTL.
const MAX_TICKETS: usize = 4096;

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

/// Modification time as an opaque token for the `X-Mtime` header and the PUT
/// `expect_mtime` conflict check: nanoseconds since the unix epoch, so two
/// saves within the same second still compare as different on filesystems
/// with sub-second timestamps.
fn mtime_token(meta: &std::fs::Metadata) -> String {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or_else(|| "0".to_string(), |d| d.as_nanos().to_string())
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
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(err)) => bad_request(&err),
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("filesystem task failed: {join}")})),
        )
            .into_response(),
    }
}

/// Response-shaped companion to [`blocking_json`] for byte-range reads.
async fn blocking_response<F>(work: F) -> Response
where
    F: FnOnce() -> anyhow::Result<Response> + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
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
        if dirs.len() >= MAX_DIR_ENTRIES {
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
                if entries.len() >= MAX_DIR_ENTRIES {
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
        if entries.len() >= MAX_DIR_ENTRIES {
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
/// back by PUT's `expect_mtime`) headers. `limit` is capped at 2MB.
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
    let meta =
        std::fs::metadata(&path).with_context(|| format!("{}: failed to stat", path.display()))?;
    let mtime = mtime_token(&meta);

    let (mime, total, bytes, truncated) = if is_gzip_path(&path) {
        let (total, bytes, more) = read_gz_slice(&path, offset, limit)?;
        (gz_mime(&path), total, bytes, more)
    } else {
        let (total, bytes) = read_file_slice(&path, offset, limit)?;
        let truncated = offset.saturating_add(bytes.len() as u64) < total;
        let mime = mime_guess::from_path(&path).first_or_octet_stream();
        (mime, Some(total), bytes, truncated)
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
    if let Some(total) = total {
        response.headers_mut().insert(
            HeaderName::from_static("x-file-size"),
            // Always ASCII digits; from_str cannot fail on it.
            HeaderValue::from_str(&total.to_string()).unwrap_or(HeaderValue::from_static("0")),
        );
    }
    Ok(response)
}

/// Read up to `limit` bytes of the (canonical) file at `path` starting at
/// `offset`. Returns the total file size and the bytes.
fn read_file_slice(path: &Path, offset: u64, limit: u64) -> anyhow::Result<(u64, Vec<u8>)> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("{}: failed to open", path.display()))?;
    let total = file
        .metadata()
        .with_context(|| format!("{}: failed to stat", path.display()))?
        .len();
    let mut bytes = Vec::new();
    if offset < total {
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("{}: failed to seek", path.display()))?;
        file.take(limit)
            .read_to_end(&mut bytes)
            .with_context(|| format!("{}: failed to read", path.display()))?;
    }
    Ok((total, bytes))
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
}

/// Outcome of an attempted atomic write.
enum WriteOutcome {
    /// Written; carries the file's new `X-Mtime` token.
    Written(String),
    /// The on-disk mtime no longer matches `expect_mtime`.
    Conflict,
}

/// PUT /api/v1/fs/file?path=&expect_mtime= — write the raw request body to
/// the file, atomically (hidden sibling tmp + rename), creating it if its
/// parent directory exists. 204 on success with the new `X-Mtime` so the
/// editor can chain saves; 400 for directories/missing parents; 409
/// `{"error":"file changed on disk"}` when `expect_mtime` (the token from a
/// previous GET/PUT) no longer matches — the check is skipped when the param
/// is absent; 413 over 1MB (editing is for small text files).
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
        write_file_atomic(&query.path, &body, query.expect_mtime.as_deref())
    })
    .await;
    match result {
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("file-write task failed: {join}")})),
        )
            .into_response(),
        Ok(Ok(WriteOutcome::Written(mtime))) => {
            // A save is a git-relevant change: nudge the workspace(s) holding
            // this path so the tree/panel refetch without any polling.
            crate::git::mark_path_dirty(&state, &dirty_path).await;
            let mut response = StatusCode::NO_CONTENT.into_response();
            response.headers_mut().insert(
                HeaderName::from_static("x-mtime"),
                // The token is ASCII digits; from_str cannot fail on it.
                HeaderValue::from_str(&mtime).unwrap_or(HeaderValue::from_static("0")),
            );
            response
        }
        Ok(Ok(WriteOutcome::Conflict)) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "file changed on disk"})),
        )
            .into_response(),
        Ok(Err(err)) => bad_request(&err),
    }
}

/// Write `bytes` to the file at `raw` atomically: a hidden tmp sibling (never
/// visible in listings, even transiently) is written, given the original
/// file's permissions, then renamed over the target. Refuses directories and
/// paths whose parent directory does not exist.
fn write_file_atomic(
    raw: &str,
    bytes: &[u8],
    expect_mtime: Option<&str>,
) -> anyhow::Result<WriteOutcome> {
    let expanded = expand_tilde(raw)?;
    let (target, existing) = match std::fs::metadata(&expanded) {
        Ok(meta) if meta.is_dir() => {
            anyhow::bail!("{} is a directory", expanded.display());
        }
        Ok(meta) => {
            let path =
                std::fs::canonicalize(&expanded).with_context(|| expanded.display().to_string())?;
            (path, Some(meta))
        }
        // New file: the parent directory must already exist.
        Err(_) => {
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
    };

    if let Some(expect) = expect_mtime {
        // The client edited some version of the file; if the disk moved on
        // (or the file vanished) since that version, refuse to clobber it.
        if existing.as_ref().map(mtime_token).as_deref() != Some(expect) {
            return Ok(WriteOutcome::Conflict);
        }
    }

    let tmp = target.with_file_name(format!(
        ".{}.{}.tmp",
        target
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default(),
        &chimaera_core::generate_token()[..8]
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("failed to write {}", tmp.display()))?;
    if let Some(meta) = &existing {
        // Keep the original mode (e.g. an executable script stays executable).
        // Best-effort: a failure here still leaves a correct write.
        if let Err(err) = std::fs::set_permissions(&tmp, meta.permissions()) {
            tracing::warn!(path = %tmp.display(), %err, "failed to carry permissions onto tmp file");
        }
    }
    if let Err(err) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(
            anyhow::Error::new(err).context(format!("failed to rename into {}", target.display()))
        );
    }
    let meta = std::fs::metadata(&target)
        .with_context(|| format!("{}: failed to stat after write", target.display()))?;
    Ok(WriteOutcome::Written(mtime_token(&meta)))
}

#[derive(Deserialize)]
pub(crate) struct MarkdownQuery {
    path: String,
}

/// GET /api/v1/fs/markdown?path= — the file rendered as sanitized GFM HTML.
pub(crate) async fn markdown(Query(query): Query<MarkdownQuery>) -> Response {
    blocking_json(move || Ok(json!({"html": render_markdown(&query.path)?}))).await
}

/// Render the markdown file at `raw` with comrak (GFM extensions), then
/// sanitize with ammonia's defaults so raw HTML in the source cannot inject
/// scripts. Files over 4MB are rejected.
fn render_markdown(raw: &str) -> anyhow::Result<String> {
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
    let text = String::from_utf8_lossy(&bytes);

    let mut options = comrak::Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    // Let raw HTML through comrak; ammonia strips anything dangerous.
    options.render.r#unsafe = true;
    let html = comrak::markdown_to_html(&text, &options);
    Ok(ammonia::clean(&html))
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
}

/// GET /api/v1/fs/table?path=&offset_rows=0&limit_rows=200&delim=auto — a
/// page of a CSV/TSV file: header row as `columns`, then `limit_rows` data
/// rows starting at `offset_rows`. All cells are strings. `.gz`/`.bgz` files
/// (bioinformatics reality: `.tsv.gz` everywhere) page identically via
/// sequential decode, capped at [`MAX_GZ_DECOMPRESS`] decompressed bytes.
pub(crate) async fn table(Query(query): Query<TableQuery>) -> Response {
    let limit = query.limit_rows.unwrap_or(200).min(MAX_TABLE_ROWS);
    let delim = query.delim.as_deref().unwrap_or("auto");
    let delim = delim.to_string();
    blocking_json(move || read_table(&query.path, query.offset_rows, limit, &delim)).await
}

/// Parse one page of the delimited (possibly gzip-compressed) file at `raw`.
fn read_table(
    raw: &str,
    offset_rows: usize,
    limit_rows: usize,
    delim: &str,
) -> anyhow::Result<serde_json::Value> {
    let path = canonical_file(raw)?;
    let gz = is_gzip_path(&path);
    let delimiter = match delim {
        "auto" => sniff_delimiter(&path, gz)?,
        "," | "comma" => b',',
        "\t" | "tab" => b'\t',
        other => anyhow::bail!("unsupported delimiter {other:?} (want auto, comma, or tab)"),
    };

    let file = std::fs::File::open(&path)
        .with_context(|| format!("{}: failed to open", path.display()))?;
    let (columns, rows, truncated) = if gz {
        // Take caps the decode work so a gzip bomb cannot spin the daemon;
        // flate2's read decoders buffer their input internally.
        let decoder = MultiGzDecoder::new(file).take(MAX_GZ_DECOMPRESS);
        let (columns, rows, mut truncated, rest) =
            page_records(decoder, delimiter, offset_rows, limit_rows, &path)?;
        if rest.limit() == 0 {
            // Cap reached: rows past it are unreachable by sequential decode,
            // so the page is honestly "truncated" even though we saw EOF.
            truncated = true;
        }
        (columns, rows, truncated)
    } else {
        let (columns, rows, mut truncated, rest) = page_records(
            std::io::BufReader::new(file).take(MAX_TABLE_SCAN_BYTES),
            delimiter,
            offset_rows,
            limit_rows,
            &path,
        )?;
        if rest.limit() == 0 {
            truncated = true;
        }
        (columns, rows, truncated)
    };

    Ok(json!({
        "columns": columns,
        "rows": rows,
        "offset": offset_rows,
        "truncated": truncated,
    }))
}

/// One paged table read: header row, data rows, more-rows-remain, and the
/// reader handed back (gz callers inspect its remaining `Take` budget to
/// detect the decode cap).
type PagedRecords<R> = (Vec<String>, Vec<Vec<String>>, bool, R);

/// Page `limit_rows` records (after skipping `offset_rows`) out of a
/// delimited byte stream: header row first, then the page. Returns the
/// exhausted reader so gz callers can check whether the decode cap was hit.
fn page_records<R: Read>(
    input: R,
    delimiter: u8,
    offset_rows: usize,
    limit_rows: usize,
    path: &Path,
) -> anyhow::Result<PagedRecords<R>> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true)
        .from_reader(input);

    let lossy_cells = |record: &csv::ByteRecord| -> Vec<String> {
        record
            .iter()
            .map(|cell| String::from_utf8_lossy(cell).into_owned())
            .collect()
    };

    let columns = lossy_cells(
        reader
            .byte_headers()
            .with_context(|| format!("{}: failed to parse header row", path.display()))?,
    );

    let mut rows = Vec::with_capacity(limit_rows.min(256));
    let mut truncated = false;
    for (index, record) in reader.byte_records().enumerate() {
        let record = record.with_context(|| format!("{}: failed to parse row", path.display()))?;
        if index < offset_rows {
            continue;
        }
        if rows.len() == limit_rows {
            truncated = true;
            break;
        }
        rows.push(lossy_cells(&record));
    }

    Ok((columns, rows, truncated, reader.into_inner()))
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
/// (decoded) line: any tab means tab, otherwise comma.
fn sniff_delimiter(path: &Path, gz: bool) -> anyhow::Result<u8> {
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
    let mut first_line = Vec::new();
    if gz {
        std::io::BufReader::new(MultiGzDecoder::new(file))
            .take(64 * 1024)
            .read_until(b'\n', &mut first_line)
            .with_context(|| format!("{}: failed to decompress", path.display()))?;
    } else {
        std::io::BufReader::new(file)
            .take(64 * 1024)
            .read_until(b'\n', &mut first_line)
            .with_context(|| format!("{}: failed to read", path.display()))?;
    }
    Ok(if first_line.contains(&b'\t') {
        b'\t'
    } else {
        b','
    })
}

#[derive(Deserialize)]
pub(crate) struct ValidateRequest {
    candidates: Vec<String>,
    base: String,
    /// Additive (older clients omit it, older daemons ignore it): enables the
    /// bare-basename fallback below, scoped to this workspace's index.
    #[serde(default)]
    workspace_id: Option<String>,
}

/// A candidate eligible for the bare-basename fallback: a single path segment
/// (no `/`), not a dotfile / `~` form / flag-like token, shaped like
/// `name.ext` with a letter-led extension of at most 8 alphanumerics — the
/// same shape the terminal client's `BARE_EXT_RE` admits. Prose words
/// (`docs`, `license`) and version numbers (`1.2.3`) never qualify, so the
/// fallback cannot widen what the clients already treat as path-like.
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
        && (1..=8).contains(&ext.len())
        && ext.starts_with(|c: char| c.is_ascii_alphabetic())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
}

/// POST /api/v1/fs/validate {candidates, base, workspace_id?} — batched
/// existence check behind the terminal and chat link providers: only
/// path-like strings that resolve to something real get underlined. Each
/// candidate is resolved (absolute, or relative against the absolute `base`,
/// `~` expanded) and answered under `valid` as `{path, kind}` — the canonical
/// absolute path and whether it is a `file` or a `dir`. Misses are simply
/// absent. Candidates past [`MAX_VALIDATE_CANDIDATES`] are ignored: cheap and
/// batched by design.
///
/// Bare-basename fallback: an agent often mentions a file by basename alone
/// ("FIGURE_PLAN.md") when it lives in a subdirectory (`paper/FIGURE_PLAN.md`),
/// so direct-child resolution can never confirm it. When `workspace_id` is
/// given and a [`bare_basename`]-shaped candidate misses, the workspace's
/// quickopen index is consulted and the candidate resolves IFF exactly one
/// indexed file has that exact name — ambiguity (three `main.rs`) refuses,
/// and the hit is re-canonicalized so a file deleted since the walk stays a
/// miss (existence-verified links only, same as the direct path). Bounds: the
/// index is the quickopen walk — entry/depth/time-capped, ignore-respecting,
/// cached per workspace with a short TTL — fetched at most once per request
/// and only when a fallback-eligible candidate actually missed.
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
    let work = move || {
        let base = Path::new(&body.base);
        let mut valid = serde_json::Map::new();
        // Lazily fetched, at most once per request; inner None = unknown
        // workspace (fallback silently off — degrade, don't error).
        let mut index: Option<Option<Arc<Vec<crate::quickopen::IndexedFile>>>> = None;
        for candidate in body.candidates.iter().take(MAX_VALIDATE_CANDIDATES) {
            if candidate.is_empty() || valid.contains_key(candidate) {
                continue;
            }
            let Ok(expanded) = expand_tilde(candidate) else {
                continue;
            };
            let joined = if expanded.is_absolute() {
                expanded
            } else {
                base.join(expanded)
            };
            // canonicalize both resolves (symlinks, `..`) and checks existence;
            // anything unresolvable is a miss, never an error.
            if let Ok(resolved) = std::fs::canonicalize(&joined) {
                let kind = if resolved.is_dir() { "dir" } else { "file" };
                valid.insert(
                    candidate.clone(),
                    json!({"path": resolved.to_string_lossy(), "kind": kind}),
                );
                continue;
            }
            // Direct resolution missed — try the unique-basename fallback.
            let Some(workspace_id) = body.workspace_id.as_deref() else {
                continue;
            };
            if !bare_basename(candidate) {
                continue;
            }
            let files = index
                .get_or_insert_with(|| crate::quickopen::workspace_index(&state, workspace_id));
            let Some(files) = files.as_deref() else {
                continue;
            };
            let Some(path) = crate::quickopen::unique_file_named(files, candidate) else {
                continue;
            };
            // The index may be up to its TTL stale: re-canonicalize so only a
            // file that exists RIGHT NOW links (and the answer is canonical,
            // matching the direct path's contract).
            let Ok(resolved) = std::fs::canonicalize(path) else {
                continue;
            };
            if !resolved.is_file() {
                continue;
            }
            valid.insert(
                candidate.clone(),
                json!({"path": resolved.to_string_lossy(), "kind": "file"}),
            );
        }
        json!({"valid": valid})
    };
    match tokio::task::spawn_blocking(work).await {
        Ok(body) => Json(body).into_response(),
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("validate task failed: {join}")})),
        )
            .into_response(),
    }
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
