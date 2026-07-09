//! Filesystem endpoints: the folder picker (home + directories-only listing),
//! and the file service backing file tabs — full directory listings, ranged
//! raw reads, atomic single-file writes (lightweight editing), server-rendered
//! markdown, paged CSV/TSV tables (with a transparent gzip tier for .gz/.bgz),
//! and short-lived tickets that let iframes/img tags fetch bytes without a
//! bearer header.

use std::collections::HashMap;
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::Context;
use axum::body::Bytes;
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
/// Largest markdown source `fs/markdown` will render.
const MAX_MARKDOWN_BYTES: u64 = 4 * 1024 * 1024;
/// Hard cap on `fs/table` rows per page.
const MAX_TABLE_ROWS: usize = 1000;
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
    blocking_listing(move || list_dirs(&query.path, query.hidden)).await
}

/// Run a directory-listing on a blocking thread and shape the result — a slow
/// Lustre `read_dir` must never wedge a tokio worker.
async fn blocking_listing<F>(work: F) -> Response
where
    F: FnOnce() -> anyhow::Result<serde_json::Value> + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(err)) => bad_request(&err),
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("listing task failed: {join}")})),
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
}

/// GET /api/v1/fs/list?path=<path>&hidden=<bool> — full directory listing
/// (dirs and files) for the file tree.
pub(crate) async fn list(Query(query): Query<DirsQuery>) -> Response {
    blocking_listing(move || list_entries(&query.path, query.hidden)).await
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
        // metadata() follows symlinks; broken symlinks and unreadable entries
        // are skipped.
        let Ok(meta) = std::fs::metadata(&entry_path) else {
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
    match read_file_response(&query.path, query.offset, limit) {
        Ok(response) => response,
        Err(err) => bad_request(&err),
    }
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
    match write_file_atomic(&query.path, &body, query.expect_mtime.as_deref()) {
        Ok(WriteOutcome::Written(mtime)) => {
            // A save is a git-relevant change: nudge the workspace(s) holding
            // this path so the tree/panel refetch without any polling.
            crate::git::mark_path_dirty(&state, &query.path).await;
            let mut response = StatusCode::NO_CONTENT.into_response();
            response.headers_mut().insert(
                HeaderName::from_static("x-mtime"),
                // The token is ASCII digits; from_str cannot fail on it.
                HeaderValue::from_str(&mtime).unwrap_or(HeaderValue::from_static("0")),
            );
            response
        }
        Ok(WriteOutcome::Conflict) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "file changed on disk"})),
        )
            .into_response(),
        Err(err) => bad_request(&err),
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
    match render_markdown(&query.path) {
        Ok(html) => Json(json!({"html": html})).into_response(),
        Err(err) => bad_request(&err),
    }
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
    match read_table(&query.path, query.offset_rows, limit, delim) {
        Ok(body) => Json(body).into_response(),
        Err(err) => bad_request(&err),
    }
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
        let (columns, rows, truncated, _) = page_records(
            std::io::BufReader::new(file),
            delimiter,
            offset_rows,
            limit_rows,
            &path,
        )?;
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
}

/// POST /api/v1/fs/validate {candidates, base} — batched existence check
/// behind the terminal link provider: only path-like strings that resolve to
/// something real get underlined. Each candidate is resolved (absolute, or
/// relative against the absolute `base`, `~` expanded) and answered under
/// `valid` as `{path, kind}` — the canonical absolute path and whether it is
/// a `file` or a `dir`. Misses are simply absent. Candidates past
/// [`MAX_VALIDATE_CANDIDATES`] are ignored: cheap and batched by design.
pub(crate) async fn validate(Json(body): Json<ValidateRequest>) -> Response {
    let base = Path::new(&body.base);
    if !base.is_absolute() {
        return bad_request(&anyhow::anyhow!(
            "base {:?} is not an absolute path",
            body.base
        ));
    }
    let mut valid = serde_json::Map::new();
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
        let Ok(resolved) = std::fs::canonicalize(&joined) else {
            continue;
        };
        let kind = if resolved.is_dir() { "dir" } else { "file" };
        valid.insert(
            candidate.clone(),
            json!({"path": resolved.to_string_lossy(), "kind": kind}),
        );
    }
    Json(json!({"valid": valid})).into_response()
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
    fn lookup(&mut self, ticket: &str) -> Option<PathBuf> {
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

#[derive(Deserialize)]
pub(crate) struct TicketRequest {
    path: String,
}

/// POST /api/v1/fs/ticket {path} — mint a 10-minute raw-access ticket for a
/// file, so iframes and img tags (which cannot send Authorization headers)
/// can fetch it via GET /raw/{ticket}. The bearer token never appears in a
/// URL.
pub(crate) async fn create_ticket(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TicketRequest>,
) -> Response {
    let path = match canonical_file(&body.path) {
        Ok(path) => path,
        Err(err) => return bad_request(&err),
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
        Ok(meta) => meta.len(),
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
    let mut bytes = vec![0u8; len as usize];
    let read = async {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        file.seek(SeekFrom::Start(start)).await?;
        file.read_exact(&mut bytes).await
    };
    if let Err(err) = read.await {
        tracing::warn!(path = %path.display(), %err, "ticketed file read failed");
        return not_found();
    }

    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let mut response = (
        status,
        [
            (header::CONTENT_TYPE, mime.essence_str().to_string()),
            (header::ACCEPT_RANGES, "bytes".to_string()),
        ],
        bytes,
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
