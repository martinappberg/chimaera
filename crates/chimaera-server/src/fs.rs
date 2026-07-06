//! Filesystem browsing endpoints backing the folder picker: the daemon
//! user's home and a directories-only listing of an arbitrary path.

use std::path::PathBuf;

use anyhow::Context;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

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
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

/// Canonicalize `raw` (after tilde expansion) and list its subdirectories:
/// directories and symlinks resolving to directories only, dotted names
/// excluded unless `hidden`, sorted case-insensitively by name.
fn list_dirs(raw: &str, hidden: bool) -> anyhow::Result<serde_json::Value> {
    let expanded = expand_tilde(raw)?;
    let path = std::fs::canonicalize(&expanded)
        .with_context(|| format!("{}: no such directory", expanded.display()))?;
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
