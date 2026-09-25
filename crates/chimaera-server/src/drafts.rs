//! Draft mirror: the daemon-side copy of unsaved editor text.
//!
//! The browser journals dirty buffers to IndexedDB, but IndexedDB is per
//! origin — a remote daemon reached through a new tunnel port is a new origin
//! with an empty store. So the client also mirrors each dirty buffer here,
//! and a window on any origin can offer to recover it.
//!
//! Storage is one small JSON file per draft under `<data dir>/drafts/`, named
//! by the first 32 hex chars of SHA-256(path): an update rewrites one file
//! atomically instead of appending to a shared log. Every dimension is
//! capped (text per draft, draft count, total bytes) with
//! least-recently-updated eviction, and a corrupt file is skipped, never
//! fatal. Drafts hold unsaved user text, so the directory is 0700 and each
//! file 0600 from creation — `~/.chimaera` may sit on a shared login node.
//! All filesystem work runs off the reactor under the shared filesystem
//! limiter.

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
/// Request-body ceiling for `PUT /fs/drafts`. JSON escaping can grow a
/// 1 MiB text up to sixfold (`\u0000`), so the body limit sits above that
/// and the 1 MiB text cap is judged on the decoded text (413).
pub(crate) const MAX_DRAFT_BODY_BYTES: usize = 7 * 1024 * 1024;
/// A temp file left by a crash mid-write is swept once it is this old (a
/// live write finishes in well under a second).
const STALE_TEMP_AGE: Duration = Duration::from_secs(600);

/// Serializes draft mutations (write + eviction, delete). Reads need no lock:
/// every write lands by atomic rename.
static DRAFTS_WRITE: Mutex<()> = Mutex::new(());

/// One draft file.
#[derive(Serialize, Deserialize)]
struct StoredDraft {
    path: String,
    base_hash: Option<String>,
    updated_ms: u64,
    /// UTF-8 length of `text`, stored so a listing never decodes the text.
    bytes: u64,
    text: String,
}

/// A draft file minus its text: serde skips the unknown `text` field without
/// allocating it, so listing 64 drafts never materializes 16 MiB.
#[derive(Serialize, Deserialize)]
struct DraftMeta {
    path: String,
    base_hash: Option<String>,
    updated_ms: u64,
    bytes: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `<root>/<first 32 hex of SHA-256(path)>.json`.
fn draft_file(root: &Path, path: &str) -> PathBuf {
    let digest = crate::fs::sha256_hex(path.as_bytes());
    root.join(format!("{}.json", &digest[..32]))
}

/// A file name this module wrote as a draft (never a temp or a stray).
fn is_draft_name(name: &str) -> bool {
    name.strip_suffix(".json").is_some_and(|stem| {
        stem.len() == 32 && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
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

/// Enforce the count and byte caps, newest kept, `keep` (the draft just
/// written) never evicted. Recency is the file's mtime — the time of its last
/// atomic replace — so enforcement needs one stat per draft, never a parse.
/// Also sweeps temp files a crash left behind.
fn evict(root: &Path, keep: &Path) {
    let Ok(read) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    let mut drafts: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        let path = entry.path();
        if is_draft_name(name) {
            if path != keep {
                drafts.push((modified, meta.len(), path));
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
    drafts.sort_by_key(|draft| std::cmp::Reverse(draft.0));
    let mut count = 1usize;
    let mut total = std::fs::metadata(keep).map_or(0, |m| m.len());
    for (_, len, path) in drafts {
        if count < MAX_DRAFTS && total.saturating_add(len) <= MAX_DRAFTS_TOTAL_BYTES {
            count += 1;
            total += len;
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct PutDraftRequest {
    path: String,
    #[serde(default)]
    base_hash: Option<String>,
    text: String,
}

/// PUT /api/v1/fs/drafts {path, base_hash: string|null, text} — store (or
/// replace) the draft for `path`. 204; 413 when `text` is over 1 MiB (UTF-8
/// bytes); 400 for an empty or overlong `path` / `base_hash`. Beyond 64
/// drafts or 16 MiB in total the least recently updated drafts are evicted.
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
    let root = state.drafts_root.clone();
    crate::fs::blocking_response(move || {
        let stored = StoredDraft {
            bytes: body.text.len() as u64,
            path: body.path,
            base_hash: body.base_hash,
            updated_ms: now_ms(),
            text: body.text,
        };
        let contents = serde_json::to_vec(&stored).context("failed to encode draft")?;
        let dest = draft_file(&root, &stored.path);
        let _writing = crate::lock(&DRAFTS_WRITE);
        ensure_root(&root)?;
        write_private(&dest, &contents)?;
        evict(&root, &dest);
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// GET /api/v1/fs/drafts — `{drafts: [{path, base_hash, updated_ms, bytes}]}`
/// newest first, without text. Unreadable or corrupt draft files are skipped.
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
            if !name.to_str().is_some_and(is_draft_name) {
                continue;
            }
            let path = entry.path();
            match std::fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| Ok(serde_json::from_slice::<DraftMeta>(&bytes)?))
            {
                Ok(meta) => drafts.push(meta),
                Err(err) => {
                    tracing::debug!(path = %path.display(), %err, "skipping unreadable draft");
                }
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
}

/// GET /api/v1/fs/draft?path= — `{path, base_hash, text, updated_ms}`, or 404
/// when there is no (readable) draft for exactly this path.
pub(crate) async fn get_draft(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DraftQuery>,
) -> Response {
    let root = state.drafts_root.clone();
    crate::fs::blocking_response(move || {
        let file = draft_file(&root, &query.path);
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
            }))
            .into_response(),
            _ => json_error(StatusCode::NOT_FOUND, "no draft for this path"),
        })
    })
    .await
}

/// DELETE /api/v1/fs/draft?path= — drop the draft for `path`. 204 whether or
/// not one existed.
pub(crate) async fn delete_draft(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DraftQuery>,
) -> Response {
    let root = state.drafts_root.clone();
    crate::fs::blocking_response(move || {
        let file = draft_file(&root, &query.path);
        let _writing = crate::lock(&DRAFTS_WRITE);
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(
                    anyhow::Error::new(err).context(format!("failed to delete {}", file.display()))
                );
            }
        }
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}
