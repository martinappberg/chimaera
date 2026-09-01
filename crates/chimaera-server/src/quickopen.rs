//! Quick-open file index: GET /api/v1/fs/quickopen fuzzy-matches a
//! workspace's files by name/path for the Cmd+P palette. The same cached
//! index backs the `/fs/validate` bare-basename fallback (see
//! [`workspace_index`] / [`unique_file_named`]), so link validation never
//! adds a second walker.
//!
//! The index is a bounded walk of the workspace root, and on a login node
//! that root is routinely a 20k-entry NFS tree that takes seconds to crawl.
//! Three rules keep that cost off the user and off the reactor:
//!
//! - **Serve stale, refresh behind.** Once a workspace has an index, every
//!   query answers from it immediately; a stale index kicks ONE background
//!   re-walk. Nobody waits on the disk except the very first query.
//! - **Single-flight.** One walk per workspace at a time — a burst of
//!   `/fs/validate` calls (every terminal repaint, every chat message) or a
//!   typing burst in the palette can never fan out into parallel crawls of
//!   the same tree (measured live: five concurrent 3 s walks per burst).
//! - **Freshness scales with cost.** A walk is trusted for
//!   [`FRESH_PER_WALK_COST`] × its own duration (floored at [`CACHE_TTL`],
//!   capped at [`CACHE_TTL_MAX`]): a 40 ms local walk refreshes every 5 s, a
//!   3 s NFS crawl every 30 s.
//!
//! Callers that walk block on the filesystem, so every entry point runs
//! under `spawn_blocking` — never inline in an async handler.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
/// The shortest a walk result stays fresh (a fast typer on a local tree).
const CACHE_TTL: Duration = Duration::from_secs(5);
/// Freshness multiplier on the walk's own duration — an expensive tree is
/// re-crawled proportionally less often.
const FRESH_PER_WALK_COST: u32 = 10;
/// The longest a walk result is trusted, however slow the walk was.
const CACHE_TTL_MAX: Duration = Duration::from_secs(120);
/// A workspace index nobody has queried for this long is dropped: a
/// 100k-entry index is tens of MB, and the daemon's RSS budget is ~150 MB.
const IDLE_EVICT: Duration = Duration::from_secs(600);
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

/// Per-workspace index slots. The map lock is held only to look up or
/// evict a slot — never across a walk.
#[derive(Default)]
pub(crate) struct QuickOpenCache {
    slots: HashMap<String, Arc<Slot>>,
}

impl QuickOpenCache {
    fn slot(&mut self, workspace_id: &str) -> Arc<Slot> {
        self.slots
            .entry(workspace_id.to_string())
            .or_insert_with(|| Arc::new(Slot::default()))
            .clone()
    }

    /// Drop indexes nobody has queried for [`IDLE_EVICT`]. A slot mid-walk
    /// is safe to drop from the map: the walker holds its own `Arc` and its
    /// result simply goes unused.
    fn evict_idle(&mut self, now: Instant) {
        self.slots
            .retain(|_, slot| now.duration_since(crate::lock(&slot.state).last_used) < IDLE_EVICT);
    }
}

/// One workspace's index plus its single-flight state.
#[derive(Default)]
struct Slot {
    /// Held for the whole duration of a walk — the single-flight. A cold
    /// caller with nothing to serve parks here (it is on a blocking thread)
    /// and finds the index filled when the lock frees.
    walk: Mutex<()>,
    state: Mutex<SlotState>,
}

struct SlotState {
    files: Option<Arc<Vec<IndexedFile>>>,
    built: Option<Instant>,
    fresh_for: Duration,
    /// A background re-walk is in flight (stale-while-revalidate).
    refreshing: bool,
    /// The last walk tripped a guard. Logged at warn once per streak — a
    /// permanently-partial NFS tree must not flood the log on every refresh.
    partial: bool,
    last_used: Instant,
}

impl Default for SlotState {
    fn default() -> Self {
        SlotState {
            files: None,
            built: None,
            fresh_for: CACHE_TTL,
            refreshing: false,
            partial: false,
            last_used: Instant::now(),
        }
    }
}

/// Clears the slot's `refreshing` flag when a background walk ends — by any
/// path, including a panic — so a failed refresh can never wedge the slot
/// into "stale forever".
struct RefreshGuard(Arc<Slot>);

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        crate::lock(&self.0.state).refreshing = false;
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
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let workspace_id = query.workspace_id.clone();
    // A cold workspace walks the tree (seconds on NFS): off the reactor, or
    // one palette keystroke stalls every terminal pump on that worker.
    let searched = tokio::task::spawn_blocking(move || {
        workspace_index(&state, &query.workspace_id)
            .map(|files| search(&files, &query.q, limit, query.dirs))
    })
    .await;
    match searched {
        Ok(Some(entries)) => Json(json!({"entries": entries})).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {workspace_id}")})),
        )
            .into_response(),
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("quickopen task failed: {join}")})),
        )
            .into_response(),
    }
}

/// The (possibly stale) file index for a workspace — the shared machinery
/// behind GET /fs/quickopen and the `/fs/validate` bare-basename fallback.
/// `None` for an unknown workspace.
///
/// BLOCKING: call from `spawn_blocking`. Only a workspace with no index yet
/// walks inline (single-flighted — a concurrent cold caller waits for that
/// walk instead of starting its own); a workspace with an index answers at
/// once and, when the index is past its freshness window, refreshes it in
/// the background. Cost ceiling: at most one walk per workspace in flight,
/// each bounded by the walk guards, re-run no more than once per freshness
/// window — shared-login-node safe.
pub(crate) fn workspace_index(
    state: &Arc<AppState>,
    workspace_id: &str,
) -> Option<Arc<Vec<IndexedFile>>> {
    let workspace = crate::lock(&state.workspaces).get(workspace_id)?;
    let now = Instant::now();
    let slot = {
        let mut cache = crate::lock(&state.quickopen);
        cache.evict_idle(now);
        cache.slot(&workspace.id)
    };

    // Warm path: serve what is there; a stale index kicks one refresh.
    {
        let mut st = crate::lock(&slot.state);
        st.last_used = now;
        if let Some(files) = st.files.clone() {
            let stale = st
                .built
                .is_none_or(|built| now.duration_since(built) >= st.fresh_for);
            if stale && !st.refreshing {
                st.refreshing = true;
                spawn_refresh(state, slot.clone(), workspace.root.clone());
            }
            return Some(files);
        }
    }

    // Cold path: walk now. Taking `walk` parks behind any in-flight walk
    // for this workspace; re-check afterwards because that walk may have
    // filled the index for us.
    let _walking = crate::lock(&slot.walk);
    if let Some(files) = crate::lock(&slot.state).files.clone() {
        return Some(files);
    }
    let ignore = crate::lock(&state.settings).quickopen_ignore_dirs();
    Some(build(&slot, &workspace.root, ignore.as_deref()))
}

/// Re-walk `root` for `slot` off the calling thread. Inside the daemon this
/// rides the blocking pool; with no runtime (unit tests) it runs inline so
/// the semantics stay observable.
fn spawn_refresh(state: &Arc<AppState>, slot: Arc<Slot>, root: PathBuf) {
    let state = state.clone();
    let run = move || {
        let _done = RefreshGuard(slot.clone());
        let _walking = crate::lock(&slot.walk);
        // settings.json ground truth: user-tuned ignore list, else the
        // built-in default (read at walk time so an edit lands next refresh).
        let ignore = crate::lock(&state.settings).quickopen_ignore_dirs();
        build(&slot, &root, ignore.as_deref());
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(run);
        }
        Err(_) => run(),
    }
}

/// Walk `root` and install the result as `slot`'s index, sizing its
/// freshness window from the walk's own cost. Caller holds `slot.walk`.
fn build(slot: &Slot, root: &Path, ignore: Option<&[String]>) -> Arc<Vec<IndexedFile>> {
    let started = Instant::now();
    let walk = walk_outcome(root, ignore);
    let took = started.elapsed();
    let files = Arc::new(walk.files);
    let mut st = crate::lock(&slot.state);
    if walk.partial {
        if st.partial {
            tracing::debug!(root = %root.display(), ?took, entries = files.len(),
                "quickopen index still partial (a walk guard tripped)");
        } else {
            tracing::warn!(root = %root.display(), ?took, entries = files.len(),
                "quickopen index hit a walk guard; results are partial until a full walk completes");
        }
    }
    st.partial = walk.partial;
    st.files = Some(files.clone());
    st.built = Some(Instant::now());
    st.fresh_for = (took * FRESH_PER_WALK_COST).clamp(CACHE_TTL, CACHE_TTL_MAX);
    files
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

/// A walk's entries plus whether a guard cut it short.
pub(crate) struct Walk {
    pub(crate) files: Vec<IndexedFile>,
    pub(crate) partial: bool,
}

/// Walk `root` with the production bounds (see the guard constants).
fn walk_outcome(root: &Path, ignore: Option<&[String]>) -> Walk {
    walk_bounded(
        root,
        ignore,
        MAX_INDEX_FILES,
        MAX_INDEX_DEPTH,
        Instant::now() + WALK_TIME_CAP,
    )
}

/// Walk `root` collecting regular files and directories, skipping the
/// ignored dirs (`ignore` override from settings, else [`IGNORED_DIRS`]) and
/// all symlinks (never followed: loop safety), bounded by `max_files` /
/// `max_depth` / `deadline` — explicit so tests can exercise the guards
/// without building 100k-file trees. Unreadable entries are skipped
/// silently, matching `fs/list`.
pub(crate) fn walk_bounded(
    root: &Path,
    ignore: Option<&[String]>,
    max_files: usize,
    max_depth: usize,
    deadline: Instant,
) -> Walk {
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
            return Walk {
                files,
                partial: true,
            };
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
                return Walk {
                    files,
                    partial: true,
                };
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
    Walk {
        files,
        partial: false,
    }
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
