//! Quick-open file index: GET /api/v1/fs/quickopen fuzzy-matches a
//! workspace's files by name/path for the Cmd+P palette. A fresh walk is
//! cheap at workspace scale; a short-TTL cache keeps a fast typer from
//! hammering the disk on every keystroke. The same cached index backs the
//! `/fs/validate` bare-basename fallback (see [`workspace_index`] /
//! [`unique_file_named`]), so link validation never adds a second walker.

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
/// Walk guards. The daemon lives on shared HPC login nodes where a workspace
/// can sit on NFS/Lustre, so every walk is bounded three ways and yields
/// honest partial results (cached like a full walk) when a guard trips:
/// entry count (a scratch dir with hundreds of thousands of files must not
/// balloon the index), depth (runaway generated/looping trees; real
/// workspaces stay well under this), and wall time (a cold NFS walk must not
/// wedge the request that triggered it).
const MAX_INDEX_FILES: usize = 100_000;
pub(crate) const MAX_INDEX_DEPTH: usize = 32;
const WALK_TIME_CAP: Duration = Duration::from_secs(3);
/// How long a walk result stays fresh.
const CACHE_TTL: Duration = Duration::from_secs(5);
/// Default and maximum result counts.
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

/// One indexed entry (file or directory). The lowercase copies are
/// precomputed once per walk so matching stays allocation-free per keystroke.
pub(crate) struct IndexedFile {
    pub(crate) path: String,
    pub(crate) rel: String,
    pub(crate) name: String,
    pub(crate) mtime: u64,
    pub(crate) rel_lower: String,
    pub(crate) name_lower: String,
    pub(crate) is_dir: bool,
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
    /// Also return directories (`"kind":"dir"`). Off by default so the
    /// Cmd+P palette stays a file finder; the chat composer's @-mentions
    /// opt in (folders are taggable in the agent CLIs).
    #[serde(default)]
    dirs: bool,
}

/// GET /api/v1/fs/quickopen?workspace_id=&q=&limit=50&dirs=false —
/// `{"entries":[{"path","rel","name","mtime","kind"}]}`, ranked name-prefix >
/// name-substring > path-subsequence (case-insensitive), newest mtime
/// breaking ties. An empty query returns the most recently modified files.
pub(crate) async fn quickopen(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QuickOpenQuery>,
) -> Response {
    let Some(files) = workspace_index(&state, &query.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {}", query.workspace_id)})),
        )
            .into_response();
    };

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    Json(json!({"entries": search(&files, &query.q, limit, query.dirs)})).into_response()
}

/// The (possibly cached) file index for a workspace — the shared machinery
/// behind GET /fs/quickopen and the `/fs/validate` bare-basename fallback.
/// `None` for an unknown workspace. BLOCKING (a cache miss walks the tree):
/// call from `spawn_blocking` or a handler that accepts the bounded inline
/// walk. Cost ceiling: at most one [`walk`] per workspace per [`CACHE_TTL`],
/// each bounded by the walk guards above — shared-login-node safe.
pub(crate) fn workspace_index(
    state: &Arc<AppState>,
    workspace_id: &str,
) -> Option<Arc<Vec<IndexedFile>>> {
    let workspace = crate::lock(&state.workspaces).get(workspace_id)?;

    // Walk outside the cache lock: two racing queries may both walk, which
    // is fine at this scale — never hold a lock across disk I/O. (The lookup
    // is bound to its own statement so the guard drops before the re-lock.)
    let cached = crate::lock(&state.quickopen).get_fresh(&workspace.id);
    Some(match cached {
        Some(files) => files,
        None => {
            // settings.json ground truth: user-tuned ignore list, else the
            // built-in default (the short cache TTL picks up changes fast).
            let ignore = crate::lock(&state.settings).quickopen_ignore_dirs();
            let files = Arc::new(walk(&workspace.root, ignore.as_deref()));
            crate::lock(&state.quickopen).insert(workspace.id.clone(), files.clone());
            files
        }
    })
}

/// The absolute path of the single FILE named `name` (exact, case-sensitive)
/// in the index — `None` when absent OR ambiguous. Refusing on ambiguity is
/// the false-positive defense for bare-basename links: `main.rs` mentioned in
/// a multi-crate repo must not underline and open an arbitrary one.
pub(crate) fn unique_file_named<'a>(files: &'a [IndexedFile], name: &str) -> Option<&'a str> {
    let mut found: Option<&str> = None;
    for file in files.iter().filter(|f| !f.is_dir && f.name == name) {
        if found.is_some() {
            return None;
        }
        found = Some(&file.path);
    }
    found
}

/// Walk `root` collecting regular files and directories, skipping the
/// ignored dirs (`ignore` override from settings, else [`IGNORED_DIRS`]) and
/// all symlinks (never followed: loop safety), bounded by [`MAX_INDEX_FILES`]
/// / [`MAX_INDEX_DEPTH`] / [`WALK_TIME_CAP`]. Unreadable entries are skipped
/// silently, matching `fs/list`.
fn walk(root: &Path, ignore: Option<&[String]>) -> Vec<IndexedFile> {
    walk_bounded(
        root,
        ignore,
        MAX_INDEX_FILES,
        MAX_INDEX_DEPTH,
        Instant::now() + WALK_TIME_CAP,
    )
}

/// [`walk`] with explicit bounds, so tests can exercise the guards without
/// building 100k-file trees.
pub(crate) fn walk_bounded(
    root: &Path,
    ignore: Option<&[String]>,
    max_files: usize,
    max_depth: usize,
    deadline: Instant,
) -> Vec<IndexedFile> {
    let ignored = |name: &str| match ignore {
        Some(list) => list.iter().any(|d| d == name),
        None => IGNORED_DIRS.contains(&name),
    };
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        // Time guard checked per directory (not per entry): cheap, and a
        // single directory read is the smallest unit worth interrupting.
        if Instant::now() >= deadline {
            tracing::warn!(
                root = %root.display(),
                "quickopen index hit the time guard; results are partial"
            );
            return files;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read {
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = file_type.is_dir();
            if is_dir {
                if ignored(&name) {
                    continue;
                }
                if depth + 1 < max_depth {
                    stack.push((entry.path(), depth + 1));
                }
            } else if !file_type.is_file() {
                continue;
            }
            if files.len() >= max_files {
                tracing::warn!(
                    root = %root.display(),
                    cap = max_files,
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
                is_dir,
            });
        }
    }
    files
}

/// Rank, sort, and serialize the matching entries.
fn search(files: &[IndexedFile], q: &str, limit: usize, dirs: bool) -> Vec<serde_json::Value> {
    let q = q.trim().to_lowercase();
    let mut hits: Vec<(u8, &IndexedFile)> = files
        .iter()
        .filter(|f| dirs || !f.is_dir)
        .filter_map(|f| rank(f, &q).map(|r| (r, f)))
        .collect();
    hits.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.mtime.cmp(&a.1.mtime))
            .then_with(|| a.1.rel.cmp(&b.1.rel))
    });
    hits.into_iter()
        .take(limit)
        .map(|(_, f)| {
            json!({
                "path": f.path,
                "rel": f.rel,
                "name": f.name,
                "mtime": f.mtime,
                "kind": if f.is_dir { "dir" } else { "file" },
            })
        })
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
