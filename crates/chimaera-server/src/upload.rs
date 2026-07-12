//! Session-scoped file uploads: the landing pad for OS-desktop drops and
//! pasted screenshots. The body STREAMS to disk chunk-by-chunk (the daemon
//! runs on shared login nodes — a whole file must never sit in RAM), capped
//! per file and per session, into `uploads_root/<session-id>/`. The route is
//! session-scoped so the daemon derives the destination itself and can prune
//! everything a session left behind when it ends.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::AppState;

/// Hard cap on one uploaded file. Uploads exist for screenshots and small
/// files dropped from the desktop; datasets belong on the filesystem via the
/// shell, not in an HTTP body.
pub(crate) const MAX_UPLOAD_BYTES: u64 = 32 * 1024 * 1024;
/// Hard cap on a session's whole uploads dir — the bounded-state rule: a
/// session cannot grow `~/.chimaera` without limit by receiving drops.
pub(crate) const MAX_SESSION_UPLOAD_BYTES: u64 = 256 * 1024 * 1024;
/// Hard cap on the file COUNT in a session's uploads dir. Bounds inode use and
/// keeps the per-upload `dir_usage` rescan cheap (it walks every entry), so a
/// flood of tiny drops can't turn each new upload into an ever-slower scan.
pub(crate) const MAX_SESSION_UPLOAD_FILES: usize = 256;

#[derive(Deserialize)]
pub(crate) struct UploadQuery {
    /// Suggested filename; sanitized to a strict basename server-side.
    #[serde(default)]
    name: Option<String>,
}

/// A safe basename for the uploaded file: no separators (path traversal on a
/// shared login node), no control bytes (it gets typed into shells/prompts),
/// no dot-dirs, bounded length. `None` rejects the name outright.
fn sanitize_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    if name.is_empty() || name.len() > 200 || name == "." || name == ".." {
        return None;
    }
    if name.contains(['/', '\\']) || name.chars().any(char::is_control) {
        return None;
    }
    Some(name.to_string())
}

/// Current (bytes, file-count) in a session's uploads dir (flat — uploads are
/// never nested). Missing dir reads as (0, 0). One scan feeds both caps.
async fn dir_usage(dir: &Path) -> (u64, usize) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return (0, 0);
    };
    let mut total = 0u64;
    let mut count = 0usize;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Ok(meta) = entry.metadata().await {
            if meta.is_file() {
                total += meta.len();
                count += 1;
            }
        }
    }
    (total, count)
}

/// POST /api/v1/sessions/{id}/upload?name= — stream the raw request body into
/// the session's uploads dir and answer `{path, name, size}` with the
/// absolute path on THIS host (for remote sessions the request already rode
/// the tunnel to the daemon that owns the session, so the path is valid where
/// the session runs). 404 for unknown sessions, 400 for bad names, 413 past
/// the per-file or per-session cap. Bearer-authed like every REST route.
pub(crate) async fn upload(
    State(state): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<UploadQuery>,
    body: Body,
) -> Response {
    // Existence check doubles as path-safety: only a registered session id
    // (a generated token, never attacker-shaped) becomes a directory name.
    let known = state.sessions.get(&id).is_some()
        || state.chat.get(&id).is_some()
        || crate::lock(&state.agents).contains_key(&id);
    if !known {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown session {id}")})),
        )
            .into_response();
    }

    let Some(name) = sanitize_name(query.name.as_deref().unwrap_or("upload.bin")) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid file name"})),
        )
            .into_response();
    };

    let dir = state.uploads_root.join(&id);
    if let Err(err) = tokio::fs::create_dir_all(&dir).await {
        return internal(&dir, "failed to create uploads dir", &err.into());
    }
    let (existing, existing_count) = dir_usage(&dir).await;
    if existing_count >= MAX_SESSION_UPLOAD_FILES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "error": format!(
                    "session upload file limit ({MAX_SESSION_UPLOAD_FILES} files) reached"
                )
            })),
        )
            .into_response();
    }

    // Stream to a hidden tmp sibling, then rename — a partial upload is never
    // visible under its final name (an agent could read it mid-write).
    let token = &chimaera_core::generate_token()[..8];
    let tmp = dir.join(format!(".{name}.{token}.tmp"));
    let mut file = match tokio::fs::File::create(&tmp).await {
        Ok(file) => file,
        Err(err) => return internal(&tmp, "failed to open upload file", &err.into()),
    };
    let mut written: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                drop(file);
                let _ = tokio::fs::remove_file(&tmp).await;
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("upload stream failed: {err}")})),
                )
                    .into_response();
            }
        };
        written += chunk.len() as u64;
        if written > MAX_UPLOAD_BYTES || existing + written > MAX_SESSION_UPLOAD_BYTES {
            drop(file);
            let _ = tokio::fs::remove_file(&tmp).await;
            let limit = if written > MAX_UPLOAD_BYTES {
                format!("file limit {MAX_UPLOAD_BYTES} bytes")
            } else {
                format!("session upload limit {MAX_SESSION_UPLOAD_BYTES} bytes")
            };
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"error": format!("upload too large ({limit})")})),
            )
                .into_response();
        }
        if let Err(err) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(&tmp).await;
            return internal(&tmp, "failed to write upload", &err.into());
        }
    }
    if let Err(err) = file.flush().await {
        drop(file);
        let _ = tokio::fs::remove_file(&tmp).await;
        return internal(&tmp, "failed to flush upload", &err.into());
    }
    drop(file);

    // Keep the dropped file's own name when free; a taken name gets a short
    // random prefix instead of clobbering. The exists→rename window is racy
    // only against the same user re-dropping the same name in the same
    // instant — an accepted, self-inflicted overwrite.
    let mut target = dir.join(&name);
    let mut final_name = name.clone();
    if tokio::fs::try_exists(&target).await.unwrap_or(false) {
        final_name = format!("{token}-{name}");
        target = dir.join(&final_name);
    }
    if let Err(err) = tokio::fs::rename(&tmp, &target).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return internal(&target, "failed to finalize upload", &err.into());
    }

    Json(json!({
        "path": target.to_string_lossy(),
        "name": final_name,
        "size": written,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct DirUploadQuery {
    /// Absolute destination directory on the daemon's filesystem.
    dir: String,
    /// Suggested filename; sanitized to a strict basename server-side.
    #[serde(default)]
    name: Option<String>,
}

/// POST /api/v1/fs/upload?dir=<abs>&name= — stream the raw request body into a
/// user-chosen directory (an OS-desktop drop onto a Finder pane or the FILES
/// tree). Unlike the session route this lands the file on the user's own
/// filesystem — same trust model as PUT /fs/file (the daemon runs as the
/// user), so there are no per-dir byte/count caps, only the per-file cap that
/// bounds one gesture. Collisions get a " copy" sibling rather than clobbering.
/// Answers `{path, name, size}`. Bearer-authed; 400 for a bad name or a
/// non-directory `dir`, 413 past the per-file cap.
pub(crate) async fn upload_to_dir(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DirUploadQuery>,
    body: Body,
) -> Response {
    let Some(name) = sanitize_name(query.name.as_deref().unwrap_or("upload.bin")) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid file name"})),
        )
            .into_response();
    };
    let dir = match tokio::fs::canonicalize(&query.dir).await {
        Ok(dir) if dir.is_dir() => dir,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("{} is not a directory", query.dir)})),
            )
                .into_response();
        }
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("{}: {err}", query.dir)})),
            )
                .into_response();
        }
    };

    // Hidden tmp sibling then rename — a partial upload never appears under its
    // final name.
    let token = &chimaera_core::generate_token()[..8];
    let tmp = dir.join(format!(".{name}.{token}.tmp"));
    let mut file = match tokio::fs::File::create(&tmp).await {
        Ok(file) => file,
        Err(err) => return internal(&tmp, "failed to open upload file", &err.into()),
    };
    let mut written: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                drop(file);
                let _ = tokio::fs::remove_file(&tmp).await;
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("upload stream failed: {err}")})),
                )
                    .into_response();
            }
        };
        written += chunk.len() as u64;
        if written > MAX_UPLOAD_BYTES {
            drop(file);
            let _ = tokio::fs::remove_file(&tmp).await;
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({
                    "error": format!("upload too large (file limit {MAX_UPLOAD_BYTES} bytes)")
                })),
            )
                .into_response();
        }
        if let Err(err) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(&tmp).await;
            return internal(&tmp, "failed to write upload", &err.into());
        }
    }
    if let Err(err) = file.flush().await {
        drop(file);
        let _ = tokio::fs::remove_file(&tmp).await;
        return internal(&tmp, "failed to flush upload", &err.into());
    }
    drop(file);

    // Never clobber an existing file: a taken name gets a " copy" sibling.
    let mut target = dir.join(&name);
    let mut final_name = name.clone();
    if tokio::fs::try_exists(&target).await.unwrap_or(false) {
        let (stem, ext) = match name.split_once('.') {
            Some((s, e)) => (s.to_string(), format!(".{e}")),
            None => (name.clone(), String::new()),
        };
        for n in 1..10_000 {
            let candidate = if n == 1 {
                format!("{stem} copy{ext}")
            } else {
                format!("{stem} copy {n}{ext}")
            };
            if !tokio::fs::try_exists(dir.join(&candidate))
                .await
                .unwrap_or(true)
            {
                final_name = candidate;
                break;
            }
        }
        target = dir.join(&final_name);
    }
    if let Err(err) = tokio::fs::rename(&tmp, &target).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return internal(&target, "failed to finalize upload", &err.into());
    }
    // Nudge the git watcher so the tree/panel refetch without polling (same
    // reason the fs mutations do).
    crate::git::mark_path_dirty(&state, &query.dir).await;

    Json(json!({
        "path": target.to_string_lossy(),
        "name": final_name,
        "size": written,
    }))
    .into_response()
}

fn internal(path: &Path, what: &str, err: &anyhow::Error) -> Response {
    tracing::warn!(path = %path.display(), %err, "{what}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": format!("{what}: {err}")})),
    )
        .into_response()
}

/// Remove everything a session uploaded. Best-effort and detached: deleting
/// a big dir on NFS can stall, and no caller (DELETE, retire, close-all)
/// should block on it.
pub(crate) fn prune_session_uploads(state: &Arc<AppState>, session_id: &str) {
    prune_dir(state.uploads_root.join(session_id));
}

/// Remove EVERY session's uploads — the close-all / shutdown companion of
/// `prune_session_uploads` (kill_all never touches `recents::retire`, so
/// per-session hooks would miss plain shells).
pub(crate) fn prune_all_uploads(state: &Arc<AppState>) {
    prune_dir(state.uploads_root.clone());
}

fn prune_dir(dir: PathBuf) {
    tokio::task::spawn_blocking(move || {
        if let Err(err) = std::fs::remove_dir_all(&dir) {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(dir = %dir.display(), %err, "failed to prune uploads");
            }
        }
    });
}

/// Boot-time sweep: after ledger restore settles, remove upload dirs whose
/// session no longer exists in any registry — crash leftovers and sessions
/// that died while no daemon was watching. Detached so it never delays boot.
pub(crate) fn spawn_boot_prune(state: Arc<AppState>) {
    tokio::spawn(async move {
        state.wait_restored().await;
        let root = state.uploads_root.clone();
        let Ok(mut entries) = tokio::fs::read_dir(&root).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let Some(id) = name.to_str() else { continue };
            let live = state.sessions.get(id).is_some()
                || state.chat.get(id).is_some()
                || crate::lock(&state.agents).contains_key(id);
            if !live {
                prune_dir(entry.path());
            }
        }
    });
}
