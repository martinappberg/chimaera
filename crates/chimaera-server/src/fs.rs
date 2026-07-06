//! Filesystem endpoints: the folder picker (home + directories-only listing),
//! and the file service backing file tabs — full directory listings, ranged
//! raw reads, server-rendered markdown, paged CSV/TSV tables, and short-lived
//! tickets that let iframes/img tags fetch bytes without a bearer header.

use std::collections::HashMap;
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::Context;
use axum::extract::{Query, State};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;

/// Hard cap on a single `fs/file` read.
const MAX_FILE_CHUNK: u64 = 2 * 1024 * 1024;
/// Default `fs/file` read size (256KB).
const DEFAULT_FILE_CHUNK: u64 = 256 * 1024;
/// Largest markdown source `fs/markdown` will render.
const MAX_MARKDOWN_BYTES: u64 = 4 * 1024 * 1024;
/// Hard cap on `fs/table` rows per page.
const MAX_TABLE_ROWS: usize = 1000;
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
    match list_dirs(&query.path, query.hidden) {
        Ok(body) => Json(body).into_response(),
        Err(err) => bad_request(&err),
    }
}

/// Canonicalize `raw` (after tilde expansion) and list its subdirectories:
/// directories and symlinks resolving to directories only, dotted names
/// excluded unless `hidden`, sorted case-insensitively by name.
fn list_dirs(raw: &str, hidden: bool) -> anyhow::Result<serde_json::Value> {
    let path = canonical(raw)?;
    if !path.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }

    let entries = std::fs::read_dir(&path)
        .with_context(|| format!("{}: failed to read directory", path.display()))?;
    let mut dirs = Vec::new();
    for entry in entries {
        // Unreadable entries are skipped silently.
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        let entry_path = entry.path();
        // metadata() follows symlinks, so a symlink to a directory counts;
        // files, broken symlinks, and unreadable entries are skipped.
        if std::fs::metadata(&entry_path).is_ok_and(|meta| meta.is_dir()) {
            dirs.push(DirEntry {
                name,
                path: entry_path.to_string_lossy().into_owned(),
            });
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
    match list_entries(&query.path, query.hidden) {
        Ok(body) => Json(body).into_response(),
        Err(err) => bad_request(&err),
    }
}

/// List all entries of a directory: dirs first then files, each group sorted
/// case-insensitively; dot entries excluded unless `hidden`; unreadable
/// entries (including broken symlinks) skipped.
fn list_entries(raw: &str, hidden: bool) -> anyhow::Result<serde_json::Value> {
    let path = canonical(raw)?;
    if !path.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }

    let read = std::fs::read_dir(&path)
        .with_context(|| format!("{}: failed to read directory", path.display()))?;
    let mut entries = Vec::new();
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
/// the file, with `X-File-Size` (total size) and `X-Truncated` (whether bytes
/// remain past this slice) headers. `limit` is capped at 2MB.
pub(crate) async fn file(Query(query): Query<FileQuery>) -> Response {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_FILE_CHUNK)
        .min(MAX_FILE_CHUNK);
    match read_file_slice(&query.path, query.offset, limit) {
        Ok((path, total, bytes)) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            let truncated = query.offset.saturating_add(bytes.len() as u64) < total;
            (
                [
                    (header::CONTENT_TYPE, mime.essence_str().to_string()),
                    (HeaderName::from_static("x-file-size"), total.to_string()),
                    (
                        HeaderName::from_static("x-truncated"),
                        truncated.to_string(),
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        Err(err) => bad_request(&err),
    }
}

/// Read up to `limit` bytes of the file at `raw` starting at `offset`.
/// Returns the canonical path, the total file size, and the bytes.
fn read_file_slice(raw: &str, offset: u64, limit: u64) -> anyhow::Result<(PathBuf, u64, Vec<u8>)> {
    let path = canonical_file(raw)?;
    let mut file = std::fs::File::open(&path)
        .with_context(|| format!("{}: failed to open", path.display()))?;
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
    Ok((path, total, bytes))
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
/// rows starting at `offset_rows`. All cells are strings.
pub(crate) async fn table(Query(query): Query<TableQuery>) -> Response {
    let limit = query.limit_rows.unwrap_or(200).min(MAX_TABLE_ROWS);
    let delim = query.delim.as_deref().unwrap_or("auto");
    match read_table(&query.path, query.offset_rows, limit, delim) {
        Ok(body) => Json(body).into_response(),
        Err(err) => bad_request(&err),
    }
}

/// Parse one page of the delimited file at `raw`.
fn read_table(
    raw: &str,
    offset_rows: usize,
    limit_rows: usize,
    delim: &str,
) -> anyhow::Result<serde_json::Value> {
    let path = canonical_file(raw)?;
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"))
    {
        anyhow::bail!(
            "{} is gzip-compressed; compressed tables are not supported yet",
            path.display()
        );
    }
    let delimiter = match delim {
        "auto" => sniff_delimiter(&path)?,
        "," | "comma" => b',',
        "\t" | "tab" => b'\t',
        other => anyhow::bail!("unsupported delimiter {other:?} (want auto, comma, or tab)"),
    };

    let file = std::fs::File::open(&path)
        .with_context(|| format!("{}: failed to open", path.display()))?;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true)
        .from_reader(std::io::BufReader::new(file));

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

    Ok(json!({
        "columns": columns,
        "rows": rows,
        "offset": offset_rows,
        "truncated": truncated,
    }))
}

/// Pick the delimiter from the extension (.tsv -> tab, .csv -> comma), or
/// sniff the first line: any tab means tab, otherwise comma.
fn sniff_delimiter(path: &Path) -> anyhow::Result<u8> {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("tsv") => return Ok(b'\t'),
        Some(ext) if ext.eq_ignore_ascii_case("csv") => return Ok(b','),
        _ => {}
    }
    let file =
        std::fs::File::open(path).with_context(|| format!("{}: failed to open", path.display()))?;
    let mut first_line = Vec::new();
    std::io::BufReader::new(file)
        .take(64 * 1024)
        .read_until(b'\n', &mut first_line)
        .with_context(|| format!("{}: failed to read", path.display()))?;
    Ok(if first_line.contains(&b'\t') {
        b'\t'
    } else {
        b','
    })
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

/// GET /raw/{ticket} — the ticketed file's bytes, no bearer auth (mounted
/// outside the /api auth layer). Content-Type comes from the extension. HTML
/// is confined with `Content-Security-Policy: sandbox allow-scripts` and no
/// referrer; SVG gets a script-less sandbox (scripts never run in <img>, but
/// direct navigation should not run them either). 404 on unknown or expired
/// tickets, and on files that vanished since the ticket was minted.
pub(crate) async fn raw(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(ticket): axum::extract::Path<String>,
) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    let Some(path) = crate::lock(&state.tickets).lookup(&ticket) else {
        return not_found();
    };
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "ticketed file unreadable");
            return not_found();
        }
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let mut response = (
        [(header::CONTENT_TYPE, mime.essence_str().to_string())],
        bytes,
    )
        .into_response();
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
