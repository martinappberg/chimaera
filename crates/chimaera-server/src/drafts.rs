//! Draft mirror: the daemon-side copy of unsaved editor text.
//!
//! The browser journals dirty buffers to IndexedDB, but IndexedDB is per
//! origin — a remote daemon reached through a new tunnel port is a new origin
//! with an empty store. So the client also mirrors each dirty buffer here,
//! and a window on any origin can offer to recover it.
//!
//! Storage is two small JSON files per draft under `<data dir>/drafts/`,
//! named by the first 32 hex chars of SHA-256(path): `<id>.json` holds the
//! draft (text included) and `<id>.meta.json` its listing metadata, so a
//! listing — which the client asks for on every editable file open — reads a
//! few hundred bytes per draft instead of up to 1 MiB. An update rewrites
//! both atomically (draft first, so a sidecar never advertises text that is
//! not there) instead of appending to a shared log; a delete removes both
//! (sidecar first). Every dimension is capped (text per draft, draft count,
//! total bytes of both files) with least-recently-updated eviction of the
//! pair, and a missing or corrupt file is skipped, never fatal. Drafts hold
//! unsaved user text, so the directory is 0700 and each file 0600 from
//! creation — `~/.chimaera` may sit on a shared login node. All filesystem
//! work runs off the reactor under the shared filesystem limiter.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;

/// Largest draft text accepted (UTF-8 bytes) — the editor's own save cap.
const MAX_DRAFT_TEXT_BYTES: usize = 1024 * 1024;
/// Most drafts kept; the least recently updated beyond this are evicted.
const MAX_DRAFTS: usize = 64;
/// Most bytes all draft files may occupy together.
const MAX_DRAFTS_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
/// Longest `path` key accepted (PATH_MAX).
const MAX_DRAFT_PATH_BYTES: usize = 4096;
/// Longest `base_hash` accepted (a hex SHA-256 is 64).
const MAX_BASE_HASH_BYTES: usize = 128;
/// Longest `writer` id accepted (a browser window's random id).
const MAX_WRITER_BYTES: usize = 128;
/// Request-body ceiling for `PUT /fs/drafts`. JSON escaping can grow a
/// 1 MiB text up to sixfold (`\u0000`), so the body limit sits above that
/// and the 1 MiB text cap is judged on the decoded text (413).
pub(crate) const MAX_DRAFT_BODY_BYTES: usize = 7 * 1024 * 1024;
/// A temp file left by a crash mid-write is swept once it is this old (a
/// live write finishes in well under a second).
const STALE_TEMP_AGE: Duration = Duration::from_secs(600);
/// Largest sidecar a listing reads: a real one is under 4.5 KiB (the path
/// and hash caps), so anything bigger is not ours.
const MAX_META_FILE_BYTES: u64 = 16 * 1024;

/// Serializes draft mutations (write + eviction, delete). Reads need no lock:
/// every write lands by atomic rename.
static DRAFTS_WRITE: Mutex<()> = Mutex::new(());

/// One draft file (`<id>.json`), self-contained for `GET /fs/draft`.
#[derive(Serialize, Deserialize)]
struct StoredDraft {
    path: String,
    base_hash: Option<String>,
    /// When the daemon stored it (its clock): listing order and the fallback
    /// recency of a draft written without `client_updated_ms`.
    updated_ms: u64,
    /// When the writer's text last changed, by the writer's clock (the PUT's
    /// `updated_ms`). The browser compares it with its own IndexedDB copy's
    /// time — like with like, whatever the skew between the two machines.
    #[serde(default)]
    client_updated_ms: Option<u64>,
    /// The browser window that wrote it (its random per-page id), so a
    /// window's clear removes only its own draft: two windows on one file
    /// share this one key. `None` from an older client.
    #[serde(default)]
    writer: Option<String>,
    /// UTF-8 length of `text`.
    bytes: u64,
    text: String,
}

/// A draft's listing sidecar (`<id>.meta.json`): what `GET /fs/drafts`
/// answers per draft, without the text.
#[derive(Serialize, Deserialize)]
struct DraftMeta {
    path: String,
    base_hash: Option<String>,
    updated_ms: u64,
    #[serde(default)]
    client_updated_ms: Option<u64>,
    #[serde(default)]
    writer: Option<String>,
    bytes: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The first 32 hex chars of SHA-256(path): both files' shared stem.
fn draft_id(path: &str) -> String {
    let mut digest = crate::fs::sha256_hex(path.as_bytes());
    digest.truncate(32);
    digest
}

/// `<root>/<id>.json`: the draft, text included.
fn draft_file(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.json"))
}

/// `<root>/<id>.meta.json`: the draft's listing sidecar.
fn meta_file(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.meta.json"))
}

/// The draft id of a file name this module wrote, and whether it is the
/// sidecar; `None` for temps and strays.
fn parse_draft_name(name: &str) -> Option<(&str, bool)> {
    let (stem, is_meta) = match name.strip_suffix(".meta.json") {
        Some(stem) => (stem, true),
        None => (name.strip_suffix(".json")?, false),
    };
    let ours = stem.len() == 32 && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    ours.then_some((stem, is_meta))
}

/// Remove `file`; already gone is fine.
fn remove_if_present(file: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(file) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => {
            Err(anyhow::Error::new(err).context(format!("failed to delete {}", file.display())))
        }
    }
}

fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into()}))).into_response()
}

/// Create the drafts directory owner-only (unsaved text may be sensitive).
fn ensure_root(root: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)
    {
        Ok(()) => Ok(()),
        Err(err) => {
            Err(anyhow::Error::new(err).context(format!("failed to create {}", root.display())))
        }
    }
}

/// Atomically replace `dest` with `contents`, created 0600: a random
/// `create_new` sibling (no two writers share a temp) renamed into place,
/// removed on any failure.
fn write_private(dest: &Path, contents: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dest.with_file_name(format!(
        ".{name}.{}.tmp",
        &chimaera_core::generate_token()[..16]
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, dest)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.with_context(|| format!("failed to write {}", dest.display()))
}

/// One draft's files on disk, as eviction sees them.
#[derive(Default)]
struct DraftFiles {
    /// The draft file's mtime (its last atomic replace); a lone sidecar's
    /// stands in for a draft file a crash lost.
    modified: Option<SystemTime>,
    /// Both files together.
    len: u64,
    draft: Option<PathBuf>,
    meta: Option<PathBuf>,
}

/// Enforce the count and byte caps over draft + sidecar pairs, newest kept,
/// `keep` (the id just written) never evicted. Recency is the draft file's
/// mtime, so enforcement needs one stat per file, never a parse. An evicted
/// pair goes sidecar first. Also sweeps temp files a crash left behind.
fn evict(root: &Path, keep: &str) {
    let Ok(read) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    let mut kept_len = 0u64;
    let mut drafts: HashMap<String, DraftFiles> = HashMap::new();
    for entry in read.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        let path = entry.path();
        if let Some((id, is_meta)) = parse_draft_name(name) {
            if id == keep {
                kept_len += meta.len();
                continue;
            }
            let files = drafts.entry(id.to_owned()).or_default();
            files.len += meta.len();
            if is_meta {
                files.meta = Some(path);
                files.modified.get_or_insert(modified);
            } else {
                files.draft = Some(path);
                files.modified = Some(modified);
            }
        } else if name.starts_with('.')
            && name.ends_with(".tmp")
            && now
                .duration_since(modified)
                .is_ok_and(|age| age >= STALE_TEMP_AGE)
        {
            let _ = std::fs::remove_file(&path);
        }
    }
    let mut drafts: Vec<DraftFiles> = drafts.into_values().collect();
    drafts.sort_by_key(|files| std::cmp::Reverse(files.modified));
    let mut count = 1usize;
    let mut total = kept_len;
    for files in drafts {
        if count < MAX_DRAFTS && total.saturating_add(files.len) <= MAX_DRAFTS_TOTAL_BYTES {
            count += 1;
            total += files.len;
        } else {
            for file in [files.meta, files.draft].into_iter().flatten() {
                let _ = std::fs::remove_file(&file);
            }
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct PutDraftRequest {
    path: String,
    #[serde(default)]
    base_hash: Option<String>,
    text: String,
    /// The writer's own time for this text (epoch ms, its clock). Taken as
    /// any JSON value: a malformed one is dropped, never a 422 that would
    /// lose the draft.
    #[serde(default)]
    updated_ms: Option<serde_json::Value>,
    #[serde(default)]
    writer: Option<String>,
}

/// A client epoch-ms value, when it is one (a finite, non-negative number
/// within JavaScript's safe integers).
fn client_ms(value: Option<&serde_json::Value>) -> Option<u64> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    value
        .and_then(serde_json::Value::as_f64)
        .filter(|v| (0.0..=MAX_SAFE_INTEGER).contains(v))
        .map(|v| v as u64)
}

/// PUT /api/v1/fs/drafts {path, base_hash: string|null, text, updated_ms?,
/// writer?} — store (or replace) the draft for `path` and its listing
/// sidecar; `updated_ms` (the writer's clock) comes back as
/// `client_updated_ms`, `writer` as itself. 204; 413 when `text` is over 1 MiB
/// (UTF-8 bytes); 400 for an empty or overlong `path` / `base_hash` / `writer`. Beyond 64 drafts or 16 MiB in total (both files
/// counted) the least recently updated drafts are evicted.
pub(crate) async fn put_draft(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PutDraftRequest>,
) -> Response {
    if body.text.len() > MAX_DRAFT_TEXT_BYTES {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "draft too large ({} bytes, limit {MAX_DRAFT_TEXT_BYTES})",
                body.text.len()
            ),
        );
    }
    if body.path.is_empty() || body.path.len() > MAX_DRAFT_PATH_BYTES {
        return json_error(StatusCode::BAD_REQUEST, "path must be 1..=4096 bytes");
    }
    if body
        .base_hash
        .as_ref()
        .is_some_and(|h| h.len() > MAX_BASE_HASH_BYTES)
    {
        return json_error(StatusCode::BAD_REQUEST, "base_hash is too long");
    }
    if body
        .writer
        .as_ref()
        .is_some_and(|w| w.len() > MAX_WRITER_BYTES)
    {
        return json_error(StatusCode::BAD_REQUEST, "writer is too long");
    }
    let root = state.drafts_root.clone();
    crate::fs::blocking_response(move || {
        let stored = StoredDraft {
            bytes: body.text.len() as u64,
            path: body.path,
            base_hash: body.base_hash,
            updated_ms: now_ms(),
            client_updated_ms: client_ms(body.updated_ms.as_ref()),
            writer: body.writer,
            text: body.text,
        };
        let contents = serde_json::to_vec(&stored).context("failed to encode draft")?;
        let meta = serde_json::to_vec(&DraftMeta {
            path: stored.path.clone(),
            base_hash: stored.base_hash.clone(),
            updated_ms: stored.updated_ms,
            client_updated_ms: stored.client_updated_ms,
            writer: stored.writer.clone(),
            bytes: stored.bytes,
        })
        .context("failed to encode draft metadata")?;
        let id = draft_id(&stored.path);
        let _writing = crate::lock(&DRAFTS_WRITE);
        ensure_root(&root)?;
        // The draft first: a sidecar must never advertise text that isn't there.
        write_private(&draft_file(&root, &id), &contents)?;
        write_private(&meta_file(&root, &id), &meta)?;
        evict(&root, &id);
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// Read one sidecar, bounded; `None` (logged) when it is unreadable, corrupt,
/// oversized, or names a path whose id is not `id`.
fn read_meta(file: &Path, id: &str) -> Option<DraftMeta> {
    let read = || -> anyhow::Result<DraftMeta> {
        let mut bytes = Vec::new();
        std::fs::File::open(file)?
            .take(MAX_META_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_META_FILE_BYTES,
            "sidecar too large"
        );
        let meta: DraftMeta = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(draft_id(&meta.path) == id, "sidecar names another path");
        Ok(meta)
    };
    read()
        .map_err(|err| tracing::debug!(path = %file.display(), %err, "skipping draft sidecar"))
        .ok()
}

/// GET /api/v1/fs/drafts — `{drafts: [{path, base_hash, updated_ms,
/// client_updated_ms, writer, bytes}]}` newest first (by `updated_ms`),
/// without text. Reads only the sidecars; a draft whose
/// sidecar is missing or corrupt is skipped.
pub(crate) async fn list_drafts(State(state): State<Arc<AppState>>) -> Response {
    let root = state.drafts_root.clone();
    crate::fs::blocking_response(move || {
        let mut drafts: Vec<DraftMeta> = Vec::new();
        let read = match std::fs::read_dir(&root) {
            Ok(read) => Some(read),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(
                    anyhow::Error::new(err).context(format!("failed to read {}", root.display()))
                );
            }
        };
        for entry in read.into_iter().flatten().flatten() {
            let name = entry.file_name();
            let Some((id, true)) = name.to_str().and_then(parse_draft_name) else {
                continue;
            };
            if let Some(meta) = read_meta(&entry.path(), id) {
                drafts.push(meta);
            }
        }
        drafts.sort_by(|a, b| {
            b.updated_ms
                .cmp(&a.updated_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        Ok(Json(json!({ "drafts": drafts })).into_response())
    })
    .await
}

#[derive(Deserialize)]
pub(crate) struct DraftQuery {
    path: String,
    /// DELETE only: remove the draft only when this window wrote it.
    #[serde(default)]
    writer: Option<String>,
}

/// GET /api/v1/fs/draft?path= — `{path, base_hash, text, updated_ms,
/// client_updated_ms, writer}`, or 404 when there is no (readable) draft for
/// exactly this path. `client_updated_ms` / `writer` are null for a draft
/// stored without them.
pub(crate) async fn get_draft(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DraftQuery>,
) -> Response {
    let root = state.drafts_root.clone();
    crate::fs::blocking_response(move || {
        let file = draft_file(&root, &draft_id(&query.path));
        let stored = match std::fs::read(&file) {
            Ok(bytes) => serde_json::from_slice::<StoredDraft>(&bytes).ok(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(
                    anyhow::Error::new(err).context(format!("failed to read {}", file.display()))
                );
            }
        };
        // The file name is a truncated hash: confirm the stored key, so a
        // collision (or a corrupt file) reads as "no draft".
        Ok(match stored {
            Some(draft) if draft.path == query.path => Json(json!({
                "path": draft.path,
                "base_hash": draft.base_hash,
                "text": draft.text,
                "updated_ms": draft.updated_ms,
                "client_updated_ms": draft.client_updated_ms,
                "writer": draft.writer,
            }))
            .into_response(),
            _ => json_error(StatusCode::NOT_FOUND, "no draft for this path"),
        })
    })
    .await
}

/// Who wrote the stored draft `id` for `path`: its sidecar's `writer`, else
/// (no readable sidecar) the draft file's. `None` when neither reads — then
/// there is nothing a writer could own.
fn stored_writer(root: &Path, id: &str, path: &str) -> Option<Option<String>> {
    if let Some(meta) = read_meta(&meta_file(root, id), id) {
        return Some(meta.writer);
    }
    let bytes = std::fs::read(draft_file(root, id)).ok()?;
    let draft: StoredDraft = serde_json::from_slice(&bytes).ok()?;
    (draft.path == path).then_some(draft.writer)
}

/// DELETE /api/v1/fs/draft?path=&writer= — drop the draft for `path` and its
/// sidecar (sidecar first). With `writer`, only a draft that window wrote
/// goes (another window's — or an older client's, which names none — stays).
/// 204 whether or not one existed or went.
pub(crate) async fn delete_draft(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DraftQuery>,
) -> Response {
    let root = state.drafts_root.clone();
    crate::fs::blocking_response(move || {
        let id = draft_id(&query.path);
        let _writing = crate::lock(&DRAFTS_WRITE);
        if let Some(writer) = &query.writer {
            let owned = stored_writer(&root, &id, &query.path)
                .is_none_or(|stored| stored.as_deref() == Some(writer.as_str()));
            if !owned {
                return Ok(StatusCode::NO_CONTENT.into_response());
            }
        }
        remove_if_present(&meta_file(&root, &id))?;
        remove_if_present(&draft_file(&root, &id))?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}
