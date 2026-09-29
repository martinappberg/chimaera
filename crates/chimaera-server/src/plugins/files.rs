//! File events for plugins: `file-saved` (the editor saved a file a
//! plugin claims), `file-changed` (a claimed or watched file changed on
//! disk), and the plugins' watch sets. Also where `settings-changed` and
//! `switched-on` / `switched-off` are delivered. Design:
//! docs/plugin-platform-plan.md §5.
//!
//! - **Where changes come from.** Every write the daemon knows of already
//!   funnels through `git::mark_path_dirty` (saves, file operations,
//!   uploads, claude's hooks, chat edits): it calls [`touched`], which only
//!   queues. A save marks its path first ([`mark_saved`]) so the same
//!   write reads as `file-saved`. Anything else — a program, a `git
//!   checkout` — is seen by the sweep.
//! - **One worker** (started with the router) drains the queue, matches
//!   each change to the active plugins that declared the event and claim
//!   the file (`[[files]]`) or watch it, and debounces per (plugin,
//!   workspace, file): a burst is one event, delivered once the file's
//!   `debounce_ms` passed without another change. `file-saved` wins over
//!   `file-changed` inside one burst.
//! - **The sweep** stats each plugin's watch set (≤ 256 paths) and the
//!   files its file views show, every 5 s — only while one of its views
//!   rendered in that workspace within the last 10 minutes; otherwise not
//!   at all. The first sighting of a path records it silently.
//! - Bounded: at most `QUEUE_MAX` queued changes and `PENDING_MAX` pending
//!   deliveries (more are dropped, and logged once per burst).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::runtime::wit;
use super::{EventKind, Manifest};
use crate::AppState;

const QUEUE_MAX: usize = 1024;
const PENDING_MAX: usize = 1024;
pub(crate) const WATCH_MAX: usize = 256;
#[cfg(not(test))]
const SWEEP_EVERY: Duration = Duration::from_secs(5);
#[cfg(test)]
const SWEEP_EVERY: Duration = Duration::from_millis(300);
/// A save's mark lives this long (the write and its `mark_path_dirty`
/// happen within one request).
const SAVED_MARK: Duration = Duration::from_secs(5);

/// What the worker knows about a watched path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seen {
    /// Not stat'ed yet (or a known write just changed it): the next sweep
    /// records it without an event.
    Unknown,
    Missing,
    At(u64, u64),
}

#[derive(Default)]
pub(crate) struct Files {
    /// Changes waiting for the worker: (workspace, relative path, saved).
    queue: VecDeque<(String, String, bool)>,
    /// Paths a save just wrote (as the save named them), with when.
    saved: HashMap<String, Instant>,
    /// (plugin, workspace, file) → when to deliver, and whether a save is
    /// among the changes.
    pending: HashMap<(String, String, String), (Instant, bool)>,
    /// (plugin, workspace) → its watch set and what the sweep last saw.
    watches: HashMap<(String, String), HashMap<String, Seen>>,
    /// Viewed files the sweep saw (plugin, workspace) → path → stat.
    viewed: HashMap<(String, String), HashMap<String, Seen>>,
    last_sweep: Option<Instant>,
    dropped: bool,
}

impl Files {
    pub(crate) fn forget_plugin(&mut self, plugin: &str) {
        self.pending.retain(|(p, _, _), _| p != plugin);
        self.watches.retain(|(p, _), _| p != plugin);
        self.viewed.retain(|(p, _), _| p != plugin);
    }

    pub(crate) fn forget_workspace(&mut self, ws: &str) {
        self.queue.retain(|(w, _, _)| w != ws);
        self.pending.retain(|(_, w, _), _| w != ws);
        self.watches.retain(|(_, w), _| w != ws);
        self.viewed.retain(|(_, w), _| w != ws);
    }
}

/// Whether any plugin in the catalog hears file events at all (the common
/// case, none: a save then costs nothing here).
fn anyone_listens(state: &AppState) -> bool {
    state
        .plugin_catalog
        .all()
        .iter()
        .any(|m| m.provides.hears(EventKind::FileSaved) || m.provides.hears(EventKind::FileChanged))
}

/// The editor is about to save `path` (as the save request names it).
pub(crate) fn mark_saved(state: &AppState, path: &str) {
    if !anyone_listens(state) {
        return;
    }
    let mut files = crate::lock(&state.plugin_platform.files);
    files.saved.retain(|_, at| at.elapsed() < SAVED_MARK);
    if files.saved.len() < 256 {
        files.saved.insert(path.to_string(), Instant::now());
    }
}

/// A write the daemon knows of changed `rel` in `ws` (`written`: the path
/// as the writer named it). Only queues; the worker does the rest.
pub(crate) fn touched(state: &AppState, written: &str, ws: &str, rel: &str) {
    if rel.is_empty() || !anyone_listens(state) {
        return;
    }
    {
        let mut files = crate::lock(&state.plugin_platform.files);
        let saved = files
            .saved
            .remove(written)
            .is_some_and(|at| at.elapsed() < SAVED_MARK);
        if files.queue.len() >= QUEUE_MAX {
            if !files.dropped {
                files.dropped = true;
                tracing::warn!("plugin file events dropped: the queue is full");
            }
            return;
        }
        files.dropped = false;
        files
            .queue
            .push_back((ws.to_string(), rel.to_string(), saved));
    }
    state.plugin_platform.files_wake.notify_one();
}

/// `watch(paths)`: `plugin`'s watch set in `ws`, replacing the last one.
pub(crate) fn set_watch(
    state: &AppState,
    plugin: &str,
    ws: &str,
    paths: Vec<String>,
) -> Result<(), String> {
    if paths.len() > WATCH_MAX {
        return Err(format!("a watch set is at most {WATCH_MAX} paths"));
    }
    for p in &paths {
        let plain = !p.is_empty()
            && p.len() <= 1024
            && std::path::Path::new(p)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !plain {
            return Err(format!(
                "{p}: a watched path is workspace-relative, without `..`"
            ));
        }
    }
    let key = (plugin.to_string(), ws.to_string());
    let mut files = crate::lock(&state.plugin_platform.files);
    if paths.is_empty() {
        files.watches.remove(&key);
        return Ok(());
    }
    let old = files.watches.remove(&key).unwrap_or_default();
    let set = paths
        .into_iter()
        .map(|p| {
            let seen = old.get(&p).copied().unwrap_or(Seen::Unknown);
            (p, seen)
        })
        .collect();
    files.watches.insert(key, set);
    drop(files);
    state.plugin_platform.files_wake.notify_one();
    Ok(())
}

/// The worker: started once per state (tests build several routers over
/// one), idle on a `Notify` while nothing is queued, pending or open, and
/// gone with the state (it holds it only for a pass).
pub(crate) fn spawn_worker(state: Arc<AppState>) {
    if state.plugin_platform.worker.swap(true, Ordering::SeqCst) {
        return;
    }
    let weak = Arc::downgrade(&state);
    drop(state);
    tokio::spawn(async move {
        loop {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let wake = state.plugin_platform.files_wake.clone();
            let next = step(&state).await;
            drop(state);
            match next {
                Some(at) => {
                    tokio::select! {
                        () = wake.notified() => {}
                        () = tokio::time::sleep_until(at.into()) => {}
                    }
                }
                None => wake.notified().await,
            }
        }
    });
}

/// One pass: take the queue, deliver what is due, sweep if it is time.
/// When to run again (None: only when woken).
async fn step(state: &Arc<AppState>) -> Option<Instant> {
    let queued: Vec<(String, String, bool)> = crate::lock(&state.plugin_platform.files)
        .queue
        .drain(..)
        .collect();
    for (ws, rel, saved) in queued {
        route(state, &ws, &rel, saved).await;
    }
    let due: Vec<((String, String, String), bool)> = {
        let mut files = crate::lock(&state.plugin_platform.files);
        let now = Instant::now();
        let due: Vec<_> = files
            .pending
            .iter()
            .filter(|(_, (at, _))| *at <= now)
            .map(|(k, (_, saved))| (k.clone(), *saved))
            .collect();
        for (k, _) in &due {
            files.pending.remove(k);
        }
        due
    };
    for ((plugin, ws, rel), saved) in due {
        let Some(m) = super::manifest(state, &plugin) else {
            continue;
        };
        let event = if saved && m.provides.hears(EventKind::FileSaved) {
            wit::Event::FileSaved(rel)
        } else if m.provides.hears(EventKind::FileChanged) {
            wit::Event::FileChanged(rel)
        } else {
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            if super::active(&state, &ws)
                .await
                .iter()
                .any(|a| a.id == m.id)
            {
                state
                    .plugin_runtime
                    .on_event(&state, &m, &ws, None, event)
                    .await;
            }
        });
    }
    let open = crate::lock(&state.plugin_platform.screens).open_now();
    let sweep_due = !open.is_empty() && {
        let files = crate::lock(&state.plugin_platform.files);
        files
            .last_sweep
            .is_none_or(|at| at.elapsed() >= SWEEP_EVERY)
    };
    if sweep_due {
        sweep(state, open.clone()).await;
    }
    let files = crate::lock(&state.plugin_platform.files);
    let next_delivery = files.pending.values().map(|(at, _)| *at).min();
    let next_sweep = (!open.is_empty()).then(|| {
        files
            .last_sweep
            .map_or_else(Instant::now, |at| at + SWEEP_EVERY)
    });
    match (next_delivery, next_sweep) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The active plugins in `ws` that hear a change to `rel`: those that
/// declared the event and claim the file, or watch it.
async fn route(state: &Arc<AppState>, ws: &str, rel: &str, saved: bool) {
    let active = super::active(state, ws).await;
    let mut files = crate::lock(&state.plugin_platform.files);
    // A known write: the sweep must not report it again.
    let Files {
        watches, viewed, ..
    } = &mut *files;
    for ((_, w), set) in watches.iter_mut().chain(viewed.iter_mut()) {
        if w == ws {
            if let Some(seen) = set.get_mut(rel) {
                *seen = Seen::Unknown;
            }
        }
    }
    for m in active {
        let hears = m.provides.hears(EventKind::FileChanged)
            || (saved && m.provides.hears(EventKind::FileSaved));
        if !hears {
            continue;
        }
        let kind = super::platform::file_kind(&m, rel);
        let watched = files
            .watches
            .get(&(m.id.clone(), ws.to_string()))
            .is_some_and(|set| set.contains_key(rel));
        if kind.is_none() && !watched {
            continue;
        }
        queue_delivery(
            &mut files,
            &m,
            ws,
            rel,
            saved,
            kind.and_then(|k| k.debounce_ms),
        );
    }
}

fn queue_delivery(
    files: &mut Files,
    m: &Manifest,
    ws: &str,
    rel: &str,
    saved: bool,
    debounce_ms: Option<u64>,
) {
    let debounce =
        Duration::from_millis(debounce_ms.unwrap_or(super::platform::DEBOUNCE_DEFAULT_MS));
    let key = (m.id.clone(), ws.to_string(), rel.to_string());
    let at = Instant::now() + debounce;
    let room = files.pending.len() < PENDING_MAX;
    match files.pending.get_mut(&key) {
        Some(entry) => {
            entry.0 = at;
            entry.1 |= saved;
        }
        None if room => {
            files.pending.insert(key, (at, saved));
        }
        None => tracing::warn!(plugin = %m.id, "plugin file event dropped: too many pending"),
    }
}

/// One (plugin, workspace) to sweep: its root and its paths (`true`: in
/// the watch set; `false`: a file its file view shows).
type SweepPair = (String, String, PathBuf, Vec<(String, bool)>);

/// Stat every watched and viewed path of the open (plugin, workspace)
/// pairs; queue a change for each that moved since the last sweep.
async fn sweep(state: &Arc<AppState>, open: Vec<((String, String), Vec<String>)>) {
    let mut work: Vec<SweepPair> = Vec::new();
    {
        let mut files = crate::lock(&state.plugin_platform.files);
        files.last_sweep = Some(Instant::now());
        let workspaces = crate::lock(&state.workspaces);
        for ((plugin, ws), viewed) in &open {
            let Some(root) = workspaces.get(ws).map(|w| w.root) else {
                continue;
            };
            let mut paths: Vec<(String, bool)> = files
                .watches
                .get(&(plugin.clone(), ws.clone()))
                .map(|s| s.keys().map(|p| (p.clone(), true)).collect())
                .unwrap_or_default();
            let entry = files
                .viewed
                .entry((plugin.clone(), ws.clone()))
                .or_default();
            entry.retain(|p, _| viewed.contains(p));
            for p in viewed {
                entry.entry(p.clone()).or_insert(Seen::Unknown);
                paths.push((p.clone(), false));
            }
            if !paths.is_empty() {
                work.push((plugin.clone(), ws.clone(), root, paths));
            }
        }
    }
    if work.is_empty() {
        return;
    }
    let Ok(permit) = crate::fs::FILESYSTEM_WORK.acquire().await else {
        return;
    };
    let stats = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work.into_iter()
            .map(|(plugin, ws, root, paths)| {
                let seen: Vec<(String, bool, Seen)> = paths
                    .into_iter()
                    .map(|(p, watched)| {
                        let at = match std::fs::symlink_metadata(root.join(&p)) {
                            Ok(meta) => Seen::At(
                                meta.modified()
                                    .ok()
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map_or(0, |d| d.as_millis() as u64),
                                meta.len(),
                            ),
                            Err(_) => Seen::Missing,
                        };
                        (p, watched, at)
                    })
                    .collect();
                (plugin, ws, seen)
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let mut changed: HashSet<(String, String)> = HashSet::new();
    {
        let mut files = crate::lock(&state.plugin_platform.files);
        for (plugin, ws, seen) in stats {
            let key = (plugin, ws.clone());
            for (p, watched, now) in seen {
                let map = if watched {
                    files.watches.get_mut(&key)
                } else {
                    files.viewed.get_mut(&key)
                };
                let Some(map) = map else { continue };
                let Some(before) = map.get_mut(&p) else {
                    continue;
                };
                let moved = *before != Seen::Unknown && *before != now;
                *before = now;
                if moved {
                    changed.insert((ws.clone(), p));
                }
            }
        }
        for (ws, p) in &changed {
            if files.queue.len() < QUEUE_MAX {
                files.queue.push_back((ws.clone(), p.clone(), false));
            }
        }
    }
    if !changed.is_empty() {
        state.plugin_platform.files_wake.notify_one();
    }
}

/// `settings-changed(key)`, to `m` in each of `workspaces` where it is
/// active and declared the event.
pub(crate) fn settings_changed(
    state: &Arc<AppState>,
    m: &Arc<Manifest>,
    workspaces: Vec<String>,
    key: &str,
) {
    if !m.provides.hears(EventKind::SettingsChanged) {
        return;
    }
    let state = state.clone();
    let m = m.clone();
    let key = key.to_string();
    tokio::spawn(async move {
        for ws in workspaces {
            if super::active(&state, &ws)
                .await
                .iter()
                .any(|a| a.id == m.id)
            {
                state
                    .plugin_runtime
                    .on_event(
                        &state,
                        &m,
                        &ws,
                        None,
                        wit::Event::SettingsChanged(key.clone()),
                    )
                    .await;
            }
        }
    });
}
