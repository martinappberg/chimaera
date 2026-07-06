//! Quick-open file index: GET /api/v1/fs/quickopen fuzzy-matches a
//! workspace's files by name/path for the Cmd+P palette. A fresh walk is
//! cheap at workspace scale; a short-TTL cache keeps a fast typer from
//! hammering the disk on every keystroke.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

/// Directory names never worth indexing: VCS internals, package/build
/// output, virtualenvs, and pipeline scratch (`.snakemake`, nextflow's
/// `work/`). Matched by name at any depth; everything else (including other
/// dotfiles — `.gitignore` is a real quick-open target) is indexed.
const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "__pycache__",
    ".venv",
    "venv",
    ".snakemake",
    "work",
];
/// Walk guard: stop indexing past this many files (results are partial).
const MAX_INDEX_FILES: usize = 100_000;
/// How long a walk result stays fresh.
const CACHE_TTL: Duration = Duration::from_secs(5);
/// Default and maximum result counts.
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

/// One indexed file. The lowercase copies are precomputed once per walk so
/// matching stays allocation-free per keystroke.
pub(crate) struct IndexedFile {
    path: String,
    rel: String,
    name: String,
    mtime: u64,
    rel_lower: String,
    name_lower: String,
}

/// Per-workspace walk cache with a [`CACHE_TTL`] freshness window.
#[derive(Default)]
pub(crate) struct QuickOpenCache {
    walks: HashMap<String, (Instant, Arc<Vec<IndexedFile>>)>,
}

impl QuickOpenCache {
    fn get_fresh(&self, workspace_id: &str) -> Option<Arc<Vec<IndexedFile>>> {
        match self.walks.get(workspace_id) {
            Some((built, files)) if built.elapsed() < CACHE_TTL => Some(files.clone()),
            _ => None,
        }
    }

    fn insert(&mut self, workspace_id: String, files: Arc<Vec<IndexedFile>>) {
        self.walks.insert(workspace_id, (Instant::now(), files));
    }
}

#[derive(Deserialize)]
pub(crate) struct QuickOpenQuery {
    workspace_id: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

/// GET /api/v1/fs/quickopen?workspace_id=&q=&limit=50 —
/// `{"entries":[{"path","rel","name","mtime"}]}`, ranked name-prefix >
/// name-substring > path-subsequence (case-insensitive), newest mtime
/// breaking ties. An empty query returns the most recently modified files.
pub(crate) async fn quickopen(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QuickOpenQuery>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&query.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {}", query.workspace_id)})),
        )
            .into_response();
    };

    // Walk outside the cache lock: two racing queries may both walk, which
    // is fine at this scale — never hold a lock across disk I/O. (The lookup
    // is bound to its own statement so the guard drops before the re-lock.)
    let cached = crate::lock(&state.quickopen).get_fresh(&workspace.id);
    let files = match cached {
        Some(files) => files,
        None => {
            let files = Arc::new(walk(&workspace.root));
            crate::lock(&state.quickopen).insert(workspace.id.clone(), files.clone());
            files
        }
    };

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    Json(json!({"entries": search(&files, &query.q, limit)})).into_response()
}

/// Walk `root` collecting regular files, skipping [`IGNORED_DIRS`] and all
/// symlinks (never followed: loop safety), stopping at [`MAX_INDEX_FILES`].
/// Unreadable entries are skipped silently, matching `fs/list`.
fn walk(root: &Path) -> Vec<IndexedFile> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read {
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if file_type.is_dir() {
                if !IGNORED_DIRS.contains(&name.as_str()) {
                    stack.push(entry.path());
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if files.len() >= MAX_INDEX_FILES {
                tracing::warn!(
                    root = %root.display(),
                    cap = MAX_INDEX_FILES,
                    "quickopen index hit the file-count guard; results are partial"
                );
                return files;
            }
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            files.push(IndexedFile {
                rel_lower: rel.to_lowercase(),
                name_lower: name.to_lowercase(),
                path: path.to_string_lossy().into_owned(),
                rel,
                name,
                mtime,
            });
        }
    }
    files
}

/// Rank, sort, and serialize the matching entries.
fn search(files: &[IndexedFile], q: &str, limit: usize) -> Vec<serde_json::Value> {
    let q = q.trim().to_lowercase();
    let mut hits: Vec<(u8, &IndexedFile)> = files
        .iter()
        .filter_map(|f| rank(f, &q).map(|r| (r, f)))
        .collect();
    hits.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.mtime.cmp(&a.1.mtime))
            .then_with(|| a.1.rel.cmp(&b.1.rel))
    });
    hits.into_iter()
        .take(limit)
        .map(|(_, f)| json!({"path": f.path, "rel": f.rel, "name": f.name, "mtime": f.mtime}))
        .collect()
}

/// Match tier: name-prefix (0) beats name-substring (1) beats
/// path-subsequence (2); `None` filters the file out. An empty query matches
/// everything equally, so the mtime tiebreaker surfaces recent files.
fn rank(file: &IndexedFile, q: &str) -> Option<u8> {
    if q.is_empty() {
        Some(3)
    } else if file.name_lower.starts_with(q) {
        Some(0)
    } else if file.name_lower.contains(q) {
        Some(1)
    } else if is_subsequence(q, &file.rel_lower) {
        Some(2)
    } else {
        None
    }
}

/// True when `needle`'s chars appear in order (not necessarily adjacent)
/// within `haystack` — the classic fuzzy-palette match.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut haystack = haystack.chars();
    needle.chars().all(|n| haystack.by_ref().any(|h| h == n))
}
