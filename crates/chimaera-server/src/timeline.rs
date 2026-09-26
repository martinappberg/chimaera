//! The per-workspace Timeline: what happened, written by the daemon from
//! signals it already receives (an agent finished a turn, a long command
//! failed, a Slurm job ended, an agent recorded knowledge) — never by an LLM.
//! Design: docs/timeline-knowledge-plugins-plan.md §4.
//!
//! Durable state is one append-only JSONL per workspace
//! (`<data_dir>/workspace/<ws>/timeline.jsonl`), size-capped by compaction
//! (the newest ~1 MiB survives once a file passes 2 MiB), with `seq` as the
//! first serialized key and a torn-tail-tolerant loader — the chat-journal
//! discipline at a fraction of the traffic (a handful of writes per hour).
//! Hot state is a bounded ring per workspace, loaded lazily on first touch.
//!
//! Ordering: seq is assigned and the line enqueued under ONE lock, and a
//! single writer thread drains the queue, so file order always equals seq
//! order. No fs work ever runs on the reactor: loads and paging reads go
//! through `spawn_blocking`, writes through the writer thread.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Entries kept in memory per workspace — weeks of normal traffic.
pub(crate) const RING_CAP: usize = 500;
/// Compaction threshold and what survives it.
const FILE_MAX: u64 = 2 * 1024 * 1024;
const COMPACT_KEEP: u64 = 1024 * 1024;
/// Bytes read from the file tail on first load (≥ what compaction keeps).
const LOAD_TAIL: u64 = 1024 * 1024;
/// A line longer than this is refused at write time (field caps keep real
/// entries far below it).
const LINE_MAX: usize = 16 * 1024;
/// Per-field text caps, applied at construction so nothing unbounded reaches
/// the file, the ring, a client, or a model context.
pub(crate) const TITLE_MAX: usize = 300;
pub(crate) const RESULT_MAX: usize = 400;
pub(crate) const TEXT_MAX: usize = 2 * 1024;
pub(crate) const FILES_MAX: usize = 10;
/// Page size bounds for the route and the MCP reader.
pub(crate) const PAGE_DEFAULT: usize = 100;
pub(crate) const PAGE_MAX: usize = 200;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Episode,
    Command,
    Job,
    Session,
    Knowledge,
    Note,
    /// A kind written by a newer daemon — kept, rendered generically.
    #[serde(other)]
    Unknown,
}

/// One Timeline entry. `seq` MUST stay the first field (the loader and any
/// future fast path read it first); every other field is optional and
/// additive — the wire is a stable public interface.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) seq: u64,
    pub(crate) ts: u64,
    pub(crate) kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ui: Option<String>,
    /// Fidelity of an episode: protocol (chat) › hooks (claude TUI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<String>,
    /// finished | interrupted | errored | exited | unknown
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) end: Option<String>,
    /// "mastermind" when the turn's prompt was relayed by the Mastermind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) start_ts: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence: Option<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command: Option<CommandInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) job: Option<JobInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) knowledge: Option<KnowledgeChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<Note>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub(crate) struct Evidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) files: Vec<String>,
    #[serde(default)]
    pub(crate) files_n: usize,
    #[serde(default)]
    pub(crate) tools: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recorded: Option<Recorded>,
}

/// What an episode wrote into the project's knowledge (attributed only when
/// unambiguous — see `knowledge`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Recorded {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) findings: Vec<String>,
    #[serde(default)]
    pub(crate) learnings: usize,
    #[serde(default)]
    pub(crate) decisions: usize,
    /// Fingerprints of the recorded entries (join key for "recorded by").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fps: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct CommandInfo {
    pub(crate) text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exit: Option<i32>,
    pub(crate) ms: u64,
    /// "user" | "agent"
    pub(crate) source: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct JobInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    /// The final state when known (COMPLETED, FAILED, …), else ENDED.
    pub(crate) state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) elapsed: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct KnowledgeChange {
    /// "status" (a finding's confidence moved) | "new" (a finding appeared)
    pub(crate) change: String,
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) from: Option<String>,
    pub(crate) to: String,
    pub(crate) claim: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Note {
    pub(crate) from_sid: String,
    pub(crate) from_name: String,
    /// A session id, "mastermind", or None (everyone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) to: Option<String>,
    pub(crate) text: String,
}

impl Entry {
    /// A blank entry of `kind`; `append` stamps seq/ts.
    pub(crate) fn new(kind: Kind) -> Self {
        Entry {
            seq: 0,
            ts: 0,
            kind,
            sid: None,
            name: None,
            agent: None,
            ui: None,
            tier: None,
            title: None,
            result: None,
            end: None,
            via: None,
            start_ts: None,
            ms: None,
            evidence: None,
            command: None,
            job: None,
            knowledge: None,
            note: None,
        }
    }
}

struct WsTimeline {
    seq: u64,
    ring: VecDeque<Arc<Entry>>,
}

enum WriteOp {
    Append { path: PathBuf, line: String },
    Remove { dir: PathBuf },
}

pub(crate) struct TimelineService {
    root: PathBuf,
    inner: Mutex<HashMap<String, WsTimeline>>,
    epochs: Mutex<HashMap<String, u64>>,
    writer: Mutex<Option<mpsc::Sender<WriteOp>>>,
}

impl TimelineService {
    pub(crate) fn new(root: PathBuf) -> Self {
        TimelineService {
            root,
            inner: Mutex::new(HashMap::new()),
            epochs: Mutex::new(HashMap::new()),
            writer: Mutex::new(None),
        }
    }

    fn dir(&self, ws: &str) -> PathBuf {
        self.root.join(sanitize(ws))
    }

    fn path(&self, ws: &str) -> PathBuf {
        self.dir(ws).join("timeline.jsonl")
    }

    /// Load a workspace's tail from disk once (off the reactor). Concurrent
    /// first touches may both load; the first insert wins.
    async fn ensure_loaded(&self, ws: &str) {
        if crate::lock(&self.inner).contains_key(ws) {
            return;
        }
        let path = self.path(ws);
        let loaded = tokio::task::spawn_blocking(move || load_tail(&path))
            .await
            .unwrap_or_else(|_| (0, VecDeque::new()));
        crate::lock(&self.inner)
            .entry(ws.to_string())
            .or_insert(WsTimeline {
                seq: loaded.0,
                ring: loaded.1,
            });
    }

    /// Append an entry: stamps `seq` + `ts` (unless the caller set a ts),
    /// pushes the ring, queues the durable write, bumps the workspace epoch.
    /// The caller wakes `/ws/events` (`state.changes.notify_waiters()`), so
    /// several appends from one event batch into one wake.
    pub(crate) async fn append(&self, ws: &str, mut entry: Entry) -> Arc<Entry> {
        self.ensure_loaded(ws).await;
        if entry.ts == 0 {
            entry.ts = now_ms();
        }
        let path = self.path(ws);
        let arc = {
            let mut inner = crate::lock(&self.inner);
            let tl = inner.entry(ws.to_string()).or_insert(WsTimeline {
                seq: 0,
                ring: VecDeque::new(),
            });
            tl.seq += 1;
            entry.seq = tl.seq;
            let line = serde_json::to_string(&entry).unwrap_or_default();
            let arc = Arc::new(entry);
            tl.ring.push_back(arc.clone());
            while tl.ring.len() > RING_CAP {
                tl.ring.pop_front();
            }
            // Enqueue under the same lock that assigned seq: file order ==
            // seq order even with concurrent appenders.
            if !line.is_empty() && line.len() <= LINE_MAX {
                self.send(WriteOp::Append { path, line });
            } else {
                tracing::warn!(ws, "timeline entry over the line cap; kept in memory only");
            }
            arc
        };
        *crate::lock(&self.epochs).entry(ws.to_string()).or_insert(0) += 1;
        arc
    }

    /// Newest-first page. `since` returns entries with seq > since (the
    /// incremental refetch); `before` pages older than a seq. Entries older
    /// than the ring come from the file (off the reactor).
    pub(crate) async fn page(
        &self,
        ws: &str,
        before: Option<u64>,
        since: Option<u64>,
        limit: usize,
    ) -> (Vec<Arc<Entry>>, bool) {
        self.ensure_loaded(ws).await;
        let limit = limit.clamp(1, PAGE_MAX);
        let (from_ring, ring_floor) = {
            let inner = crate::lock(&self.inner);
            let Some(tl) = inner.get(ws) else {
                return (Vec::new(), false);
            };
            let floor = tl.ring.front().map(|e| e.seq).unwrap_or(0);
            let picked: Vec<Arc<Entry>> = tl
                .ring
                .iter()
                .rev()
                .filter(|e| since.is_none_or(|s| e.seq > s))
                .filter(|e| before.is_none_or(|b| e.seq < b))
                .take(limit + 1)
                .cloned()
                .collect();
            (picked, floor)
        };
        if from_ring.len() > limit {
            let mut page = from_ring;
            page.truncate(limit);
            return (page, true);
        }
        // Ring exhausted: page older entries from the file when the caller
        // is paging backwards (a `since` refetch never needs the file).
        let wants_older = since.is_none() && ring_floor > 1;
        if !wants_older {
            let more = false;
            return (from_ring, more);
        }
        let need = limit + 1 - from_ring.len();
        let cutoff = from_ring
            .last()
            .map(|e| e.seq)
            .or(before)
            .unwrap_or(ring_floor)
            .min(ring_floor);
        let path = self.path(ws);
        let older = tokio::task::spawn_blocking(move || read_older(&path, cutoff, need))
            .await
            .unwrap_or_default();
        let mut page = from_ring;
        page.extend(older.into_iter().map(Arc::new));
        let more = page.len() > limit;
        page.truncate(limit);
        (page, more)
    }

    /// The newest entries (newest first), from memory only — the hot path
    /// for the Mastermind's reader and episode post-processing.
    pub(crate) async fn latest(&self, ws: &str, limit: usize) -> Vec<Arc<Entry>> {
        self.page(ws, None, None, limit).await.0
    }

    pub(crate) fn epoch(&self, ws: &str) -> u64 {
        crate::lock(&self.epochs).get(ws).copied().unwrap_or(0)
    }

    /// Every known workspace epoch, for the `/ws/events` timeline frame.
    pub(crate) fn epochs_snapshot(&self) -> HashMap<String, u64> {
        crate::lock(&self.epochs).clone()
    }

    /// Forget a deleted workspace: memory now, its directory via the writer
    /// (queued behind any pending appends, so nothing resurrects it).
    pub(crate) fn remove_workspace(&self, ws: &str) {
        crate::lock(&self.inner).remove(ws);
        crate::lock(&self.epochs).remove(ws);
        self.send(WriteOp::Remove { dir: self.dir(ws) });
    }

    fn send(&self, op: WriteOp) {
        let mut writer = crate::lock(&self.writer);
        if writer.is_none() {
            let (tx, rx) = mpsc::channel::<WriteOp>();
            let spawned = std::thread::Builder::new()
                .name("timeline-writer".into())
                .spawn(move || writer_loop(rx));
            if spawned.is_err() {
                tracing::error!("could not start the timeline writer thread");
                return;
            }
            *writer = Some(tx);
        }
        if let Some(tx) = writer.as_ref() {
            let _ = tx.send(op);
        }
    }
}

fn writer_loop(rx: mpsc::Receiver<WriteOp>) {
    while let Ok(op) = rx.recv() {
        match op {
            WriteOp::Append { path, line } => {
                if let Err(err) = append_line(&path, &line) {
                    tracing::warn!(%err, path = %path.display(), "timeline append failed");
                    continue;
                }
                if std::fs::metadata(&path).is_ok_and(|m| m.len() > FILE_MAX) {
                    if let Err(err) = compact(&path) {
                        tracing::warn!(%err, "timeline compaction failed");
                    }
                }
            }
            WriteOp::Remove { dir } => {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    // A torn tail (crash mid-write) must not swallow the next entry: start
    // on a fresh line when the file doesn't end with one.
    let len = file.metadata()?.len();
    if len > 0 {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::Start(len - 1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            file.write_all(b"\n")?;
        }
    }
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    file.write_all(buf.as_bytes())
}

/// Keep the newest `COMPACT_KEEP` bytes (cut at a line boundary): tmp write +
/// rename, so a crash mid-compaction leaves the old file intact.
fn compact(path: &Path) -> std::io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(COMPACT_KEEP);
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::with_capacity((len - start) as usize);
    file.take(COMPACT_KEEP).read_to_end(&mut tail)?;
    let body = if start > 0 {
        match tail.iter().position(|&b| b == b'\n') {
            Some(i) => &tail[i + 1..],
            None => &tail[..0],
        }
    } else {
        &tail[..]
    };
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// Read the file tail: the resumed seq and the newest `RING_CAP` entries in
/// seq order. Unparseable lines (a torn tail, a hand edit) are skipped.
fn load_tail(path: &Path) -> (u64, VecDeque<Arc<Entry>>) {
    let entries = read_tail_entries(path, LOAD_TAIL);
    let seq = entries.iter().map(|e| e.seq).max().unwrap_or(0);
    let mut ring: VecDeque<Arc<Entry>> = VecDeque::with_capacity(RING_CAP.min(entries.len()));
    let skip = entries.len().saturating_sub(RING_CAP);
    for entry in entries.into_iter().skip(skip) {
        ring.push_back(Arc::new(entry));
    }
    (seq, ring)
}

fn read_tail_entries(path: &Path, max_bytes: u64) -> Vec<Entry> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return Vec::new();
    };
    let start = len.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if file.take(max_bytes).read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    if start > 0 {
        // The first line is almost surely a partial one.
        lines.next();
    }
    let mut entries: Vec<Entry> = lines
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .collect();
    entries.sort_by_key(|e| e.seq);
    entries.dedup_by_key(|e| e.seq);
    entries
}

/// Entries with seq < `before`, newest first, at most `need` — for paging
/// past the in-memory ring. The whole capped file is at most `FILE_MAX`.
fn read_older(path: &Path, before: u64, need: usize) -> Vec<Entry> {
    let mut all = read_tail_entries(path, FILE_MAX);
    all.retain(|e| e.seq < before);
    all.reverse();
    all.truncate(need);
    all
}

/// Workspace ids are daemon-minted, but the path component is sanitized
/// anyway: nothing outside the timeline root can ever be named.
fn sanitize(ws: &str) -> String {
    let cleaned: String = ws
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(96)
        .collect();
    if cleaned.is_empty() {
        "_".to_string()
    } else {
        cleaned
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Truncate to `max` bytes on a char boundary, appending "…" when cut.
pub(crate) fn cap(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

/// The prefix `mcp::message_agent` puts on a Mastermind relay. Kept in sync
/// by a test in `mcp` (the relay must stay recognizable here).
pub(crate) const MASTERMIND_RELAY_PREFIX: &str = "[via the workspace Mastermind";

/// Split a prompt into (title, via): a Mastermind relay loses its
/// attribution line and is marked `via: "mastermind"`.
pub(crate) fn prompt_title(prompt: &str) -> (String, Option<String>) {
    let trimmed = prompt.trim();
    if let Some(rest) = trimmed.strip_prefix(MASTERMIND_RELAY_PREFIX) {
        let body = rest.split_once('\n').map(|(_, b)| b).unwrap_or("");
        return (
            cap(&one_line(body), TITLE_MAX),
            Some("mastermind".to_string()),
        );
    }
    (cap(&one_line(trimmed), TITLE_MAX), None)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first meaningful sentence of an agent's final message: the Timeline's
/// "result" line. Skips code, headings, list intros ("Here's what I did:")
/// and bare filler ("Done!"); strips inline markdown. None when nothing
/// informative remains — an honest blank beats a hollow line.
pub(crate) fn headline(text: &str) -> Option<String> {
    let mut in_fence = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || line.is_empty() || line.starts_with('#') || line.starts_with('|') {
            continue;
        }
        let plain = strip_inline_markdown(strip_block_marker(line));
        let plain = plain.trim();
        if plain.len() < 3 || plain.ends_with(':') || is_filler(plain) {
            continue;
        }
        return Some(cap(&first_sentence(plain), RESULT_MAX));
    }
    None
}

fn strip_block_marker(line: &str) -> &str {
    let line = line.trim_start_matches('>').trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return rest;
        }
    }
    // "1. " / "12) "
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let rest = &line[digits..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return rest;
        }
    }
    line
}

fn strip_inline_markdown(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '_' if chars.peek() == Some(&c) => {
                chars.next();
            }
            '`' => {}
            '[' => {
                // [text](url) → text
                let mut label = String::new();
                let mut closed = false;
                for n in chars.by_ref() {
                    if n == ']' {
                        closed = true;
                        break;
                    }
                    label.push(n);
                }
                out.push_str(&label);
                if closed && chars.peek() == Some(&'(') {
                    for n in chars.by_ref() {
                        if n == ')' {
                            break;
                        }
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn is_filler(line: &str) -> bool {
    let lower = line.to_lowercase();
    let bare = lower.trim_end_matches(['!', '.', ' ']);
    const FILLER: [&str; 12] = [
        "done",
        "all done",
        "perfect",
        "great",
        "sure",
        "okay",
        "ok",
        "got it",
        "excellent",
        "all set",
        "finished",
        "complete",
    ];
    FILLER.contains(&bare)
}

fn first_sentence(text: &str) -> String {
    // A sentence ends at ". ", "! " or "? " — but not inside the first 20
    // chars (abbreviations like "e.g. " would cut a result to nothing).
    let bytes = text.as_bytes();
    for i in 20..bytes.len().saturating_sub(1) {
        if matches!(bytes[i], b'.' | b'!' | b'?') && bytes[i + 1] == b' ' {
            return text[..=i].to_string();
        }
    }
    text.to_string()
}

#[derive(Deserialize)]
pub(crate) struct TimelineQuery {
    #[serde(default)]
    before: Option<u64>,
    #[serde(default)]
    since: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
}

/// GET /workspaces/{id}/timeline?before=&since=&limit= — newest first.
/// `{schema:1, epoch, entries, more}`; `more` says an older page exists.
pub(crate) async fn get_timeline(
    axum::extract::State(state): axum::extract::State<Arc<crate::AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<TimelineQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if crate::lock(&state.workspaces).get(&id).is_none() {
        return (
            axum::http::StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": format!("unknown workspace {id}")})),
        )
            .into_response();
    }
    let limit = query.limit.unwrap_or(PAGE_DEFAULT);
    let (entries, more) = state
        .timeline
        .page(&id, query.before, query.since, limit)
        .await;
    let entries: Vec<&Entry> = entries.iter().map(|e| e.as_ref()).collect();
    axum::Json(serde_json::json!({
        "schema": 1,
        "epoch": state.timeline.epoch(&id),
        "entries": entries,
        "more": more,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-timeline-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn episode(title: &str) -> Entry {
        let mut e = Entry::new(Kind::Episode);
        e.title = Some(title.to_string());
        e
    }

    async fn settle(svc: &TimelineService, ws: &str, want_lines: usize) {
        for _ in 0..200 {
            let n = std::fs::read_to_string(svc.path(ws))
                .map(|s| s.lines().count())
                .unwrap_or(0);
            if n >= want_lines {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("writer never flushed {want_lines} lines");
    }

    #[tokio::test]
    async fn appends_assign_monotonic_seq_and_survive_reload() {
        let root = temp_root("reload");
        let svc = TimelineService::new(root.clone());
        for i in 0..5 {
            svc.append("ws1", episode(&format!("turn {i}"))).await;
        }
        assert_eq!(svc.epoch("ws1"), 5);
        settle(&svc, "ws1", 5).await;
        let again = TimelineService::new(root.clone());
        let (page, more) = again.page("ws1", None, None, 3).await;
        assert!(more);
        let seqs: Vec<u64> = page.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![5, 4, 3]);
        let next = again.append("ws1", episode("after reload")).await;
        assert_eq!(next.seq, 6, "seq resumes from the file");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn since_and_before_page_correctly() {
        let root = temp_root("page");
        let svc = TimelineService::new(root.clone());
        for i in 0..10 {
            svc.append("w", episode(&format!("{i}"))).await;
        }
        let (since, _) = svc.page("w", None, Some(7), 50).await;
        assert_eq!(
            since.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![10, 9, 8]
        );
        let (before, more) = svc.page("w", Some(4), None, 50).await;
        assert_eq!(
            before.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
        assert!(!more);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn torn_tail_is_skipped_and_next_append_starts_a_fresh_line() {
        let root = temp_root("torn");
        let svc = TimelineService::new(root.clone());
        svc.append("w", episode("one")).await;
        settle(&svc, "w", 1).await;
        let path = svc.path("w");
        // Simulate a crash mid-write.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"{\"seq\":2,\"ts\":1,\"ki").unwrap();
        drop(f);
        let again = TimelineService::new(root.clone());
        let e = again.append("w", episode("two")).await;
        assert_eq!(e.seq, 2, "the torn line never counted");
        settle(&again, "w", 3).await;
        let third = TimelineService::new(root.clone());
        let (page, _) = third.page("w", None, None, 10).await;
        let titles: Vec<_> = page.iter().filter_map(|e| e.title.clone()).collect();
        assert_eq!(titles, vec!["two".to_string(), "one".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn compaction_keeps_the_newest_whole_lines() {
        let root = temp_root("compact");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("timeline.jsonl");
        let mut body = String::new();
        let mut seq = 0;
        while body.len() as u64 <= FILE_MAX + 4096 {
            seq += 1;
            let mut e = episode(&"x".repeat(200));
            e.seq = seq;
            body.push_str(&serde_json::to_string(&e).unwrap());
            body.push('\n');
        }
        std::fs::write(&path, &body).unwrap();
        compact(&path).unwrap();
        let kept = std::fs::read_to_string(&path).unwrap();
        assert!(kept.len() as u64 <= COMPACT_KEEP);
        let entries: Vec<Entry> = kept
            .lines()
            .map(|l| serde_json::from_str(l).expect("every kept line is whole"))
            .collect();
        assert_eq!(
            entries.last().unwrap().seq,
            seq,
            "the newest entry survives"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_kinds_and_fields_still_parse() {
        let e: Entry =
            serde_json::from_str(r#"{"seq":3,"ts":1,"kind":"hologram","future_field":1}"#).unwrap();
        assert_eq!(e.kind, Kind::Unknown);
    }

    #[test]
    fn serialization_puts_seq_first_and_omits_empty_fields() {
        let mut e = episode("t");
        e.seq = 9;
        e.ts = 1;
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.starts_with("{\"seq\":9,"), "{s}");
        assert!(!s.contains("result"));
    }

    #[test]
    fn headline_skips_filler_intros_code_and_markdown() {
        assert_eq!(headline("Done!\n\nPer-sample **MAD** thresholds now; three samples lose under 2%. More detail follows.").as_deref(),
            Some("Per-sample MAD thresholds now; three samples lose under 2%."));
        assert_eq!(
            headline("Here's what I did:\n- Fixed the `loader` API\n- Added tests").as_deref(),
            Some("Fixed the loader API")
        );
        assert_eq!(
            headline("```rust\nfn x() {}\n```\n## Summary\nThe [config](a.toml) was wrong.")
                .as_deref(),
            Some("The config was wrong.")
        );
        assert_eq!(headline("Done.\n\nAll set!"), None);
        assert_eq!(headline(""), None);
    }

    #[test]
    fn mastermind_relays_lose_their_attribution_line() {
        let (title, via) = prompt_title(
            "[via the workspace Mastermind — the coordinating agent the user appointed]\nre-run DE without S3",
        );
        assert_eq!(title, "re-run DE without S3");
        assert_eq!(via.as_deref(), Some("mastermind"));
        let (title, via) = prompt_title("  fix   the QC\nfilter ");
        assert_eq!(title, "fix the QC filter");
        assert!(via.is_none());
    }

    #[test]
    fn cap_cuts_on_a_char_boundary() {
        let s = "γδ".repeat(200);
        let c = cap(&s, 11);
        assert!(c.ends_with('…'));
        assert!(c.len() <= 11);
    }

    #[test]
    fn sanitize_never_escapes_the_root() {
        assert_eq!(sanitize("../../etc"), "______etc");
        assert_eq!(sanitize(""), "_");
    }
}
