//! Per-window view state: opaque JSON blobs keyed by client-generated window
//! ids (layout tree, focus-mode flag, zoom state). Stored as a single JSON
//! object file, `view-state.json`, load-tolerant like `workspaces.json`.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::AppState;

/// Maximum stored blob size (the raw PUT body).
const MAX_STATE_BYTES: usize = 64 * 1024;
/// Maximum keys retained. Every browser tab ever opened mints its own window
/// key (and one more per workspace it visits), so an uncapped store grows
/// forever on a long-lived daemon — each entry up to 64 KiB, held in RSS and
/// rewritten on every layout change. Live windows re-PUT on every change,
/// which keeps them newest; the eviction only ever reaches tabs long gone.
const MAX_KEYS: usize = 128;
/// Read cap for `view-state.json`: `MAX_KEYS` blobs of `MAX_STATE_BYTES`
/// is 8 MiB, doubled because a blob is stored re-serialized, not as sent
/// (a float like `1e15` re-encodes longer), and for the keys and
/// separators. A file past it is corruption (or a legacy store from before
/// `MAX_KEYS`): it loads empty — windows fall back to their default layout —
/// and the next put replaces it.
pub(crate) const MAX_STATE_FILE_BYTES: usize = 2 * MAX_KEYS * MAX_STATE_BYTES;

type Blobs = Vec<(String, Arc<serde_json::Value>)>;

/// The store's file name, for its log lines.
const VIEW_STATE_FILE: &str = "view-state.json";

/// One bounded read of `view-state.json`: `Err` when it couldn't be read;
/// missing, oversized and corrupt are empty (logged).
fn read_state(
    path: &std::path::Path,
) -> Result<serde_json::Map<String, serde_json::Value>, std::io::Error> {
    Ok(
        match crate::persist::read_bounded(path, MAX_STATE_FILE_BYTES) {
            crate::persist::Bounded::Ok(contents) => {
                match serde_json::from_slice::<serde_json::Value>(&contents) {
                    Ok(serde_json::Value::Object(map)) => map,
                    Ok(_) => {
                        tracing::warn!(path = %path.display(), "view-state.json is not a JSON object; starting with empty view state");
                        serde_json::Map::new()
                    }
                    Err(err) => {
                        tracing::warn!(path = %path.display(), %err, "corrupt view-state.json; starting with empty view state");
                        serde_json::Map::new()
                    }
                }
            }
            crate::persist::Bounded::Missing => serde_json::Map::new(),
            crate::persist::Bounded::TooLarge => {
                tracing::warn!(path = %path.display(), "view-state.json exceeds {MAX_STATE_FILE_BYTES} bytes; starting with empty view state");
                serde_json::Map::new()
            }
            crate::persist::Bounded::Io(err) => return Err(err),
        },
    )
}

/// The persist side, behind the one lock every write takes.
#[derive(Default)]
struct Writer {
    /// The last put stamp flushed.
    last: u64,
    /// The file is there but can't be read: nothing is written over it.
    /// Each write retries the read first; once it reads, the hold lifts.
    hold: crate::persist::Hold,
    /// What a held file held once it read again, the keys this daemon had
    /// no value for: every write carries them until the store folds them
    /// into memory (its next put or get).
    recovered: Blobs,
}

/// In-memory key -> JSON value map backed by a single JSON object file
/// (save-on-change). Values are opaque to the server and shared behind an
/// `Arc`: the persist snapshot is 128 pointer bumps, not a deep copy of up
/// to 8 MiB of JSON trees on a reactor worker.
pub(crate) struct ViewStateStore {
    path: PathBuf,
    items: BTreeMap<String, Arc<serde_json::Value>>,
    /// Keys oldest-first by last write, for eviction. Load seeds it in the
    /// map's order — alphabetical (`serde_json::Map` is a BTreeMap here), so
    /// hex window ids sort ahead of the `ws_*` workspace fallbacks and a
    /// one-time over-cap trim reaches dead windows first; every PUT moves
    /// its key to the back, so the front is the least recently written.
    recency: VecDeque<String>,
    /// Serializes writers on the blocking pool so a slow NFS rename can never
    /// let an older snapshot land after a newer one (see [`Self::put`]), and
    /// owns the hold on an unreadable file.
    writer: Arc<Mutex<Writer>>,
    /// Monotonic per-put stamp; the writer records the last one it flushed.
    seq: u64,
}

impl ViewStateStore {
    /// Load the store from `path` (blocking; boot). A missing, oversized or
    /// corrupt file yields an empty store (with a warning for the latter
    /// two); an unreadable one an empty store whose writes are held until
    /// the file reads again (puts still serve this daemon's windows).
    pub(crate) fn load(path: PathBuf) -> Self {
        let mut writer = Writer::default();
        let loaded = read_state(&path).unwrap_or_else(|err| {
            writer.hold.note(&path, VIEW_STATE_FILE, err);
            serde_json::Map::new()
        });
        let items: BTreeMap<String, Arc<serde_json::Value>> = loaded
            .into_iter()
            .map(|(key, value)| (key, Arc::new(value)))
            .collect();
        // The file carries no write order, so a legacy over-cap store is
        // NOT trimmed here: its keys seed the queue alphabetically, and an
        // alphabetical trim could drop a live window's layout on the first
        // boot after an upgrade. It shrinks as puts arrive — each put moves
        // its key to the back and evicts one from the front — and every GET
        // refreshes its key too, so a window that merely reloads is safe.
        ViewStateStore {
            path,
            recency: items.keys().cloned().collect(),
            items,
            writer: Arc::new(Mutex::new(writer)),
            seq: 0,
        }
    }

    /// Fold in what a once-held file turned out to hold: keys this daemon
    /// has no value for, oldest in the eviction order.
    /// Runs on the reactor (from `get`/`put`), so it never waits on the
    /// writer: a write in progress holds that lock across its read and
    /// rename (slow on NFS). When the writer is busy this skips, and the
    /// next get or put absorbs — meanwhile every write carries the
    /// recovered keys itself.
    fn absorb(&mut self) {
        let mut writer = match self.writer.try_lock() {
            Ok(writer) => writer,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        let recovered = std::mem::take(&mut writer.recovered);
        drop(writer);
        for (key, value) in recovered.into_iter().rev() {
            if !self.items.contains_key(&key) {
                self.recency.push_front(key.clone());
                self.items.insert(key, value);
            }
        }
    }

    /// The `surfaces` lists (what each window shows) of every window
    /// currently showing workspace `ws`, WITHOUT touching recency (a status
    /// read must not keep a dead tab's blob alive). Keys are `<win>_<ws>`;
    /// the `ws_<ws>` mirror is skipped (it duplicates a window).
    pub(crate) fn surfaces_for(&self, ws: &str) -> Vec<(String, serde_json::Value)> {
        let suffix = format!("_{ws}");
        self.items
            .iter()
            .filter(|(k, _)| k.ends_with(&suffix) && !k.starts_with("ws_"))
            .filter_map(|(k, v)| {
                let surfaces = v.get("surfaces")?;
                surfaces
                    .is_array()
                    .then(|| (k.trim_end_matches(&suffix).to_string(), surfaces.clone()))
            })
            .take(8)
            .collect()
    }

    /// A read counts as use: a window that boots or switches workspace
    /// reads its blob, and that must keep it out of the eviction queue's
    /// front even when it never edits its layout afterwards.
    pub(crate) fn get(&mut self, key: &str) -> Option<Arc<serde_json::Value>> {
        self.absorb();
        let value = self.items.get(key).cloned()?;
        self.touch(key);
        Some(value)
    }

    fn touch(&mut self, key: &str) {
        self.recency.retain(|k| k != key);
        self.recency.push_back(key.to_string());
    }

    /// Store `value` under `key` and schedule the persist. The map is
    /// snapshotted under the caller's lock; serialization and the disk write
    /// both run on the blocking pool (`~/.chimaera` may be NFS, and this
    /// fires on a 500 ms debounce per divider drag, per window — never on
    /// the reactor). Returns the write task so a handler can await the
    /// outcome. While the file on disk can't be read the write is skipped
    /// (logged once) and answers `Ok`: view state is reconstructible, and
    /// the value is in memory for this daemon's windows.
    pub(crate) fn put(
        &mut self,
        key: String,
        value: serde_json::Value,
    ) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        self.absorb();
        self.items.insert(key.clone(), Arc::new(value));
        self.touch(&key);
        self.evict();
        self.seq += 1;
        let seq = self.seq;
        let path = self.path.clone();
        let writer = self.writer.clone();
        // The snapshot is the key list plus one Arc bump per value;
        // formatting up to MAX_KEYS × MAX_STATE_BYTES of JSON is CPU that
        // belongs on the pool, not on a reactor worker under the store lock.
        let mut items: Blobs = self
            .items
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        tokio::task::spawn_blocking(move || {
            // Writers queue on this lock; a snapshot older than the last one
            // flushed is dropped rather than rolling the file back.
            let mut w = crate::lock(&writer);
            if seq <= w.last {
                return Ok(());
            }
            if w.hold.is_held() {
                match read_state(&path) {
                    Ok(disk) => {
                        w.recovered = disk
                            .into_iter()
                            .filter(|(key, _)| !items.iter().any(|(k, _)| k == key))
                            .map(|(key, value)| (key, Arc::new(value)))
                            .collect();
                        w.hold.release(&path, VIEW_STATE_FILE);
                    }
                    Err(err) => {
                        w.hold.note(&path, VIEW_STATE_FILE, err);
                        return Ok(());
                    }
                }
            }
            for (key, value) in &w.recovered {
                if !items.iter().any(|(k, _)| k == key) {
                    items.push((key.clone(), value.clone()));
                }
            }
            let bytes = serialize_items(&items)?;
            crate::persist::atomic_write_json(&path, bytes)?;
            w.last = seq;
            Ok(())
        })
    }

    /// Drop least-recently-written keys past [`MAX_KEYS`].
    fn evict(&mut self) {
        while self.items.len() > MAX_KEYS {
            let Some(oldest) = self.recency.pop_front() else {
                break;
            };
            self.items.remove(&oldest);
        }
    }
}

/// The on-disk object, written without materializing a second tree: one
/// map entry per key, each value serialized straight from its `Arc`.
fn serialize_items(items: &[(String, Arc<serde_json::Value>)]) -> serde_json::Result<Vec<u8>> {
    use serde::ser::{SerializeMap, Serializer};
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::new(&mut out);
    let mut map = ser.serialize_map(Some(items.len()))?;
    for (key, value) in items {
        map.serialize_entry(key, &**value)?;
    }
    map.end()?;
    Ok(out)
}

/// Keys are client-generated window ids: `[A-Za-z0-9_-]{1,64}`.
fn valid_key(key: &str) -> bool {
    (1..=64).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn bad_key(key: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": format!("invalid view-state key {key:?} (want [A-Za-z0-9_-]{{1,64}})")})),
    )
        .into_response()
}

/// GET /api/v1/view-state/{key}
pub(crate) async fn get_view_state(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Response {
    if !valid_key(&key) {
        return bad_key(&key);
    }
    // Bound to its own statement: the guard drops before the blob is
    // serialized, so one window's 64 KiB read never holds every other
    // window's view-state traffic.
    let value = crate::lock(&state.view_state).get(&key);
    match value {
        Some(value) => Json(json!({"state": *value})).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response(),
    }
}

/// PUT /api/v1/view-state/{key} — store any JSON value up to 64KB; 204 on
/// success, including while the file on disk can't be read (the value is
/// then kept in memory only — view state is reconstructible, and refusing
/// would fail every window's layout save for the daemon's life).
pub(crate) async fn put_view_state(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    body: Bytes,
) -> Response {
    if !valid_key(&key) {
        return bad_key(&key);
    }
    if body.len() > MAX_STATE_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": format!("view state exceeds {MAX_STATE_BYTES} bytes")})),
        )
            .into_response();
    }
    let value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("invalid JSON: {err}")})),
            )
                .into_response();
        }
    };
    // Bound to its own statement: the guard drops before the write is awaited.
    let write = crate::lock(&state.view_state).put(key, value);
    match write.await {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(err)) => {
            tracing::error!(%err, "failed to persist view state");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
        Err(join) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("view-state write failed: {join}")})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `get` runs on the reactor: it must not wait while a write holds the
    /// writer lock (a slow NFS rename).
    #[test]
    fn get_never_waits_on_a_busy_writer() {
        let mut store = temp_store("busy-writer");
        store.items.insert("k".into(), Arc::new(json!(1)));
        let writer = store.writer.clone();
        let (locked_tx, locked) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _busy = crate::lock(&writer);
            locked_tx.send(()).unwrap();
            let _ = released.recv_timeout(std::time::Duration::from_secs(10));
        });
        locked.recv().unwrap();
        let (got_tx, got) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            got_tx.send(store.get("k").map(|v| (*v).clone())).unwrap();
        });
        let value = got.recv_timeout(std::time::Duration::from_secs(5));
        release.send(()).unwrap();
        holder.join().unwrap();
        reader.join().unwrap();
        assert_eq!(value.expect("get waited on the writer"), Some(json!(1)));
    }

    fn got(store: &mut ViewStateStore, key: &str) -> Option<serde_json::Value> {
        store.get(key).map(|value| (*value).clone())
    }

    fn temp_store(label: &str) -> ViewStateStore {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-view-state-{label}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        ViewStateStore::load(dir.join("view-state.json"))
    }

    /// The cap evicts least-recently-WRITTEN keys, a re-PUT counts as fresh,
    /// and the trimmed set is what reaches disk.
    #[tokio::test]
    async fn put_caps_keys_by_write_recency_and_persists_the_trimmed_set() {
        let mut store = temp_store("cap");
        for i in 0..MAX_KEYS {
            drop(store.put(format!("w-{i}"), json!(i)));
        }
        // Refresh the oldest key, then overflow by two: the two evicted keys
        // are the oldest UNREFRESHED ones.
        drop(store.put("w-0".to_string(), json!("kept")));
        drop(store.put("late-0".to_string(), json!(0)));
        let last = store.put("late-1".to_string(), json!(1));
        last.await.unwrap().unwrap();
        assert_eq!(store.items.len(), MAX_KEYS);
        assert_eq!(got(&mut store, "w-0"), Some(json!("kept")));
        assert_eq!(got(&mut store, "w-1"), None);
        assert_eq!(got(&mut store, "w-2"), None);
        assert_eq!(got(&mut store, "w-3"), Some(json!(3)));
        assert_eq!(got(&mut store, "late-1"), Some(json!(1)));

        let mut reloaded = ViewStateStore::load(store.path.clone());
        assert_eq!(reloaded.items.len(), MAX_KEYS);
        assert_eq!(got(&mut reloaded, "w-0"), Some(json!("kept")));
        assert_eq!(got(&mut reloaded, "w-1"), None);
        std::fs::remove_dir_all(store.path.parent().unwrap()).ok();
    }

    /// An over-cap file from an older daemon is kept whole on load (the file
    /// carries no write order, so trimming there could hit a live window)
    /// and shrinks as puts arrive; a GET counts as use.
    #[tokio::test]
    async fn load_keeps_an_oversized_store_until_puts_trim_it() {
        let store = temp_store("trim");
        let mut items = serde_json::Map::new();
        for i in 0..(MAX_KEYS + 20) {
            items.insert(format!("k-{i:03}"), json!(i));
        }
        crate::persist::atomic_write_json(&store.path, serde_json::to_vec(&items).unwrap())
            .unwrap();
        let mut reloaded = ViewStateStore::load(store.path.clone());
        assert_eq!(reloaded.items.len(), MAX_KEYS + 20);
        // Reading the alphabetically-first key marks it live.
        assert_eq!(got(&mut reloaded, "k-000"), Some(json!(0)));
        reloaded
            .put("fresh".to_string(), json!("x"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.items.len(), MAX_KEYS);
        assert_eq!(got(&mut reloaded, "k-000"), Some(json!(0)));
        assert_eq!(got(&mut reloaded, "fresh"), Some(json!("x")));
        assert_eq!(got(&mut reloaded, "k-001"), None);
        std::fs::remove_dir_all(store.path.parent().unwrap()).ok();
    }
}
