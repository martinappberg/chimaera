//! Quick-open file index: GET /api/v1/fs/quickopen fuzzy-matches a
//! workspace's files by name/path for the Cmd+P palette. The same cached
//! index backs the `/fs/validate` bare-basename fallback (see
//! [`workspace_index_if_free`] / [`unique_file_named`]), so link validation
//! never adds a second walker.
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
//!   The validator never parks behind an in-flight walk either: it takes the
//!   index if one exists, walks only when nothing else is walking, and
//!   otherwise skips (a link earns its underline on the next repaint).
//! - **Freshness scales with cost.** A complete walk is trusted for
//!   [`FRESH_PER_WALK_COST`] × its own duration (floored at [`CACHE_TTL`],
//!   capped at [`CACHE_TTL_MAX`]): a 40 ms local walk refreshes every 5 s, a
//!   2 s NFS crawl every 20 s. A walk cut by a guard is trusted only for the
//!   floor — an incomplete index must be retried soon, not believed longest.
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
/// honest partial results when a guard trips: entry count (a scratch dir
/// with hundreds of thousands of files must not balloon the index), depth
/// (runaway generated/looping trees; real workspaces stay well under this),
/// and wall time (a cold NFS walk must not wedge the request that triggered
/// it). The time guard is checked per directory, so one wedged `read_dir`
/// on a dead mount is the one thing it cannot bound.
const MAX_INDEX_FILES: usize = 100_000;
pub(crate) const MAX_INDEX_DEPTH: usize = 32;
const WALK_TIME_CAP: Duration = Duration::from_secs(3);
/// The shortest a walk result stays fresh (a fast typer on a local tree),
/// and the whole window a partial walk gets.
const CACHE_TTL: Duration = Duration::from_secs(5);
/// Freshness multiplier on a complete walk's own duration — an expensive
/// tree is re-crawled proportionally less often.
const FRESH_PER_WALK_COST: u32 = 10;
/// The longest a walk result is trusted, however slow the walk was.
const CACHE_TTL_MAX: Duration = Duration::from_secs(120);
/// A workspace index nobody has queried for this long is dropped on the
/// next query for any workspace: a 100k-entry index is tens of MB, and the
/// daemon's RSS budget is ~150 MB. Eviction is lazy — a fully idle daemon
/// keeps its last index until someone asks again — and a workspace that is
/// deleted is dropped at once (`forget_workspace`).
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

    /// Drop indexes nobody has queried for [`IDLE_EVICT`]. A slot with a
    /// walk in flight is kept whatever its age: dropping it would let the
    /// next query mint a fresh slot — and a fresh walk lock — and crawl the
    /// same tree alongside the orphan, the fan-out the single-flight exists
    /// to prevent.
    fn evict_idle(&mut self, now: Instant) {
        self.slots.retain(|_, slot| {
            // Only contention means a walk is in flight; a poisoned lock is
            // a walk that already died.
            let walking = matches!(
                slot.walk.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            );
            let st = crate::lock(&slot.state);
            walking || st.refreshing || now.duration_since(st.last_used) < IDLE_EVICT
        });
    }

    /// A deleted workspace's index goes with it — its id can never be
    /// queried again, so the idle sweep would otherwise be its only reclaim.
    pub(crate) fn forget_workspace(&mut self, workspace_id: &str) {
        self.slots.remove(workspace_id);
    }

    /// Drop every index: the ignore list changed, and an index built under
    /// the old one must not be served stale for its whole freshness window.
    pub(crate) fn clear(&mut self) {
        self.slots.clear();
    }
}

/// One workspace's index plus its single-flight state.
#[derive(Default)]
struct Slot {
    /// Held for the whole duration of a walk — the single-flight. A cold
    /// palette query with nothing to serve parks here (it is on a blocking
    /// thread) and finds the index filled when the lock frees; the link
    /// validator never parks (see [`workspace_index_if_free`]).
    walk: Mutex<()>,
    state: Mutex<SlotState>,
}

/// A walk result and how long it is trusted.
struct Index {
    files: Arc<Vec<IndexedFile>>,
    built: Instant,
    fresh_for: Duration,
    /// The walk tripped a guard. Logged at warn once per streak — a
    /// permanently-partial NFS tree must not flood the log on every refresh.
    partial: bool,
}

struct SlotState {
    index: Option<Index>,
    /// A background re-walk is in flight (stale-while-revalidate). Set by the
    /// query that kicks it, cleared by [`RefreshGuard`] however the walk ends.
    refreshing: bool,
    last_used: Instant,
}

impl Default for SlotState {
    fn default() -> Self {
        SlotState {
            index: None,
            refreshing: false,
            last_used: Instant::now(),
        }
    }
}

/// Clears the slot's `refreshing` flag when a background walk ends — by any
/// path, including a panic or the task being dropped before it ever ran
/// (runtime shutdown) — so a refresh that never happened can't wedge the
/// slot into "stale forever". Constructed on the spawning side for exactly
/// that reason: the closure owning it may never execute.
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

/// The (possibly stale) file index for a workspace, for the palette: `None`
/// for an unknown workspace. BLOCKING: call from `spawn_blocking`. A
/// workspace with an index answers at once (a stale one kicks a background
/// refresh); a workspace with none walks inline, single-flighted — a
/// concurrent cold caller parks for that walk instead of starting its own.
pub(crate) fn workspace_index(
    state: &Arc<AppState>,
    workspace_id: &str,
) -> Option<Arc<Vec<IndexedFile>>> {
    lookup(state, workspace_id, true)
}

/// [`workspace_index`] for the link validator, which fires on every terminal
/// repaint and chat message from inside its own `spawn_blocking`: it must
/// never park a pool thread behind someone else's walk. Same answers, except
/// that a workspace whose only walk is already in flight yields `None` —
/// the validator degrades to "no link this round" and the next repaint finds
/// the index built.
pub(crate) fn workspace_index_if_free(
    state: &Arc<AppState>,
    workspace_id: &str,
) -> Option<Arc<Vec<IndexedFile>>> {
    lookup(state, workspace_id, false)
}

fn lookup(state: &Arc<AppState>, workspace_id: &str, wait: bool) -> Option<Arc<Vec<IndexedFile>>> {
    let workspace = crate::lock(&state.workspaces).get(workspace_id)?;
    let now = Instant::now();
    let slot = {
        let mut cache = crate::lock(&state.quickopen);
        cache.evict_idle(now);
        cache.slot(&workspace.id)
    };

    // Warm path: serve what is there; a stale index kicks one refresh. The
    // flag is raised under the state lock; the guard that lowers it is built
    // after the lock is released (its Drop takes that lock — a spawn that
    // panicked with the guard in hand would self-deadlock) and before the
    // task is spawned, so no interleaving leaves the flag stuck.
    let warm = {
        let mut st = crate::lock(&slot.state);
        st.last_used = now;
        let stale = st
            .index
            .as_ref()
            .is_some_and(|index| now.duration_since(index.built) >= index.fresh_for);
        // The kick is decided under the lock so two stale readers can't both
        // claim it; the flag is theirs to clear via the guard from here on.
        let kick = stale && !st.refreshing;
        if kick {
            st.refreshing = true;
        }
        st.index.as_ref().map(|index| (index.files.clone(), kick))
    };
    if let Some((files, kick)) = warm {
        if kick {
            let done = RefreshGuard(slot.clone());
            spawn_refresh(state, done, slot.clone(), workspace.root.clone());
        }
        return Some(files);
    }

    // Cold path: walk now. Taking `walk` parks behind any in-flight walk for
    // this workspace; re-check afterwards because that walk may have filled
    // the index for us. A poisoned lock (a walk that panicked) is still a
    // usable lock — the state it guards is rebuilt by the next walk.
    let _walking = if wait {
        crate::lock(&slot.walk)
    } else {
        match slot.walk.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        }
    };
    if let Some(index) = &crate::lock(&slot.state).index {
        return Some(index.files.clone());
    }
    let ignore = crate::lock(&state.settings).quickopen_ignore_dirs();
    Some(build(&slot, &workspace.root, ignore.as_deref()))
}

/// Re-walk `root` for `slot` on the blocking pool. `done` clears the slot's
/// `refreshing` flag when the task ends — or when it is dropped unrun.
fn spawn_refresh(state: &Arc<AppState>, done: RefreshGuard, slot: Arc<Slot>, root: PathBuf) {
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        let _done = done;
        let _walking = crate::lock(&slot.walk);
        // settings.json ground truth: user-tuned ignore list, else the
        // built-in default (read at walk time; a settings PUT that changes
        // the list also drops every slot, see `QuickOpenCache::clear`).
        let ignore = crate::lock(&state.settings).quickopen_ignore_dirs();
        build(&slot, &root, ignore.as_deref());
    });
}

/// Test hook: age a workspace's index past its freshness window so the next
/// query serves it stale and kicks a refresh.
#[cfg(test)]
pub(crate) fn age_index(state: &Arc<AppState>, workspace_id: &str) {
    let slot = crate::lock(&state.quickopen).slot(workspace_id);
    let mut st = crate::lock(&slot.state);
    if let Some(index) = &mut st.index {
        index.built = Instant::now() - CACHE_TTL_MAX - Duration::from_secs(1);
    }
}

/// Walk `root` and install the result as `slot`'s index, sizing its
/// freshness window from the walk's own cost. Caller holds `slot.walk`.
fn build(slot: &Slot, root: &Path, ignore: Option<&[String]>) -> Arc<Vec<IndexedFile>> {
    let started = Instant::now();
    let walk = walk_outcome(root, ignore);
    let took = started.elapsed();
    let mut st = crate::lock(&slot.state);
    let previous = st.index.as_ref();
    // A walk that produced nothing and tripped a guard — an unreadable root
    // (a mount that blipped, a permission change), or a deadline that fired
    // before the first entry — says nothing about the tree. Keep serving the
    // last index and retry on the short window instead of blanking the
    // palette and every bare-basename link.
    if walk.partial && walk.files.is_empty() {
        if let Some(index) = previous.filter(|index| !index.files.is_empty()) {
            let files = index.files.clone();
            tracing::warn!(root = %root.display(), ?took,
                "quickopen walk returned nothing (root unreadable?); keeping the previous index");
            st.index = Some(Index {
                files: files.clone(),
                built: Instant::now(),
                fresh_for: CACHE_TTL,
                partial: true,
            });
            return files;
        }
    }
    if walk.partial {
        if previous.is_some_and(|index| index.partial) {
            tracing::debug!(root = %root.display(), ?took, entries = walk.files.len(),
                "quickopen index still partial (a walk guard tripped)");
        } else {
            tracing::warn!(root = %root.display(), ?took, entries = walk.files.len(),
                "quickopen index hit a walk guard; results are partial until a full walk completes");
        }
    }
    let files = Arc::new(walk.files);
    // An incomplete index is retried on the floor: the bare-basename
    // fallback's "exactly one match" defense only holds against a complete
    // walk, so the wrong-file window must stay as short as it always was.
    let fresh_for = if walk.partial {
        CACHE_TTL
    } else {
        (took * FRESH_PER_WALK_COST).clamp(CACHE_TTL, CACHE_TTL_MAX)
    };
    st.index = Some(Index {
        files: files.clone(),
        built: Instant::now(),
        fresh_for,
        partial: walk.partial,
    });
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

/// A walk's entries plus whether a guard (or an unreadable root) cut it
/// short.
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
/// without building 100k-file trees. Unreadable entries below the root are
/// skipped silently, matching `fs/list`; an unreadable root is reported as
/// partial, never as an empty tree.
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
            if depth == 0 {
                return Walk {
                    files,
                    partial: true,
                };
            }
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

/// Rank the matching entries and serialize the top `limit`. Order: match
/// tier, then newest mtime, then relative path — a strict total order (paths
/// are unique per walk), so selecting the top `limit` before sorting them
/// yields exactly the prefix a full sort would; the empty query matches the
/// whole index and must not sort 100k entries per keystroke.
fn search(files: &[IndexedFile], q: &str, limit: usize, dirs: bool) -> Vec<serde_json::Value> {
    let q = q.trim().to_lowercase();
    let mut hits: Vec<(u8, &IndexedFile)> = files
        .iter()
        .filter(|f| dirs || !f.is_dir)
        .filter_map(|f| rank(f, &q).map(|r| (r, f)))
        .collect();
    let cmp = |a: &(u8, &IndexedFile), b: &(u8, &IndexedFile)| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.mtime.cmp(&a.1.mtime))
            .then_with(|| a.1.rel.cmp(&b.1.rel))
    };
    if hits.len() > limit {
        hits.select_nth_unstable_by(limit, cmp);
        hits.truncate(limit);
    }
    hits.sort_unstable_by(cmp);
    hits.into_iter()
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
