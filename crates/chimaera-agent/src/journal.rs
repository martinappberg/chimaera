//! Per-session durable event journal + in-memory replay ring.
//!
//! One append-only, size-capped JSONL file per chat session under
//! `<data>/chat/` (the repo's durable-state rule: JSONL, capped, never
//! SQLite), plus a bounded in-memory ring that serves the common
//! attach-with-recent-last_seq case without touching disk. Sequence numbers
//! are assigned once, before fan-out, so the journal, the live broadcast,
//! and every client agree on them — this is the seq-replay contract from
//! DESIGN.md's transport section, realized for structured streams.
//!
//! Disk writes happen on a dedicated writer thread: `~/.chimaera` may be NFS
//! on an HPC login node, and a hung write must stall (backpressure) rather
//! than grow memory or block the async pump's executor.

use std::collections::VecDeque;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::AgentEvent;

/// Journal file cap; on overflow the file is compacted down to
/// [`JournalCaps::compact_target`] at a turn boundary.
const FILE_CAP: u64 = 4 * 1024 * 1024;
const COMPACT_TARGET: u64 = 2 * 1024 * 1024;
/// Ring bounds: the cheap replay path for reconnects.
const RING_MAX_ENTRIES: usize = 1024;
const RING_MAX_BYTES: usize = 1024 * 1024;
/// No single journal line may exceed this; oversized events are replaced
/// (they would blow the ring/replay budgets downstream).
const MAX_ENTRY_BYTES: usize = 256 * 1024;
/// Writer-queue depth. Small enough that pending-but-unwritten entries are
/// always still in the ring (entries ≤ queue cap < ring cap), so replay
/// never has a hole; deep enough to ride out normal fs latency.
const WRITE_QUEUE_DEPTH: usize = 256;

/// Directory budgets, enforced by [`prune_dir`] at daemon start.
pub const DIR_MAX_BYTES: u64 = 100 * 1024 * 1024;
pub const DIR_MAX_FILES: usize = 200;

#[derive(Clone, Copy, Debug)]
pub struct JournalCaps {
    pub file_cap: u64,
    pub compact_target: u64,
    pub ring_max_entries: usize,
    pub ring_max_bytes: usize,
}

impl Default for JournalCaps {
    fn default() -> Self {
        Self {
            file_cap: FILE_CAP,
            compact_target: COMPACT_TARGET,
            ring_max_entries: RING_MAX_ENTRIES,
            ring_max_bytes: RING_MAX_BYTES,
        }
    }
}

/// One journaled event: the unit of replay and of the live broadcast.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SeqEvent {
    pub seq: u64,
    pub ts: u64,
    pub ev: AgentEvent,
}

enum WriteOp {
    Line(Vec<u8>),
    /// Drain barrier: ack once everything before it hit the file.
    Sync(mpsc::SyncSender<()>),
}

struct RingState {
    ring: VecDeque<(Arc<SeqEvent>, usize)>,
    ring_bytes: usize,
    next_seq: u64,
}

pub struct Journal {
    path: PathBuf,
    state: Mutex<RingState>,
    tx: mpsc::SyncSender<WriteOp>,
    /// Highest seq the writer thread has durably appended; ring eviction
    /// never drops an entry the file doesn't have yet.
    written_seq: Arc<AtomicU64>,
    caps: JournalCaps,
}

impl Journal {
    pub fn open(dir: &Path, session_id: &str) -> Result<Self> {
        Self::open_with(dir, session_id, JournalCaps::default())
    }

    pub fn open_with(dir: &Path, session_id: &str, caps: JournalCaps) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let path = dir.join(format!("{session_id}.jsonl"));

        // Resume seq continuity across daemon restarts: the next seq picks
        // up after whatever the existing file ends with.
        let mut last_seq = 0u64;
        let mut size = 0u64;
        if let Ok(existing) = fs::read_to_string(&path) {
            size = existing.len() as u64;
            for line in existing.lines() {
                if let Ok(entry) = serde_json::from_str::<SeqEvent>(line) {
                    last_seq = last_seq.max(entry.seq);
                }
            }
        }

        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;

        let (tx, rx) = mpsc::sync_channel::<WriteOp>(WRITE_QUEUE_DEPTH);
        let written_seq = Arc::new(AtomicU64::new(last_seq));
        let writer = WriterThread {
            file,
            path: path.clone(),
            size,
            caps,
            written_seq: Arc::clone(&written_seq),
        };
        std::thread::Builder::new()
            .name(format!("journal-{session_id}"))
            .spawn(move || writer.run(rx))
            .context("spawn journal writer thread")?;

        Ok(Self {
            path,
            state: Mutex::new(RingState {
                ring: VecDeque::new(),
                ring_bytes: 0,
                next_seq: last_seq + 1,
            }),
            tx,
            written_seq,
            caps,
        })
    }

    /// Assign the next seq, journal the event, and return it for broadcast.
    /// Blocks only if the writer queue is full (fs stalled) — deliberate
    /// backpressure toward the agent instead of unbounded buffering.
    pub fn append(&self, ev: AgentEvent) -> Arc<SeqEvent> {
        let ts = now_ms();
        let mut state = self.state.lock().expect("journal state lock");
        let seq = state.next_seq;
        state.next_seq += 1;

        let mut entry = Arc::new(SeqEvent { seq, ts, ev });
        let mut line = serde_json::to_vec(&*entry).expect("AgentEvent serializes");
        if line.len() > MAX_ENTRY_BYTES {
            tracing::warn!(
                seq,
                bytes = line.len(),
                "journal entry exceeded size cap; replaced"
            );
            entry = Arc::new(SeqEvent {
                seq,
                ts,
                ev: AgentEvent::Error {
                    message: format!("event exceeded the {MAX_ENTRY_BYTES}-byte journal cap"),
                    fatal: false,
                },
            });
            line = serde_json::to_vec(&*entry).expect("Error event serializes");
        }
        line.push(b'\n');

        let line_len = line.len();
        state.ring.push_back((Arc::clone(&entry), line_len));
        state.ring_bytes += line_len;
        let written = self.written_seq.load(Ordering::Acquire);
        while (state.ring.len() > self.caps.ring_max_entries
            || state.ring_bytes > self.caps.ring_max_bytes)
            && state.ring.front().is_some_and(|(e, _)| e.seq <= written)
        {
            if let Some((_, len)) = state.ring.pop_front() {
                state.ring_bytes -= len;
            }
        }
        drop(state);

        // A send error means the writer thread died (disk gone); the session
        // keeps streaming from the ring rather than dying with the disk.
        if self.tx.send(WriteOp::Line(line)).is_err() {
            tracing::error!(path = %self.path.display(), "journal writer gone; entries now memory-only");
        }
        entry
    }

    pub fn last_seq(&self) -> u64 {
        self.state.lock().expect("journal state lock").next_seq - 1
    }

    /// Everything after `last_seq`, oldest first. Serves from the ring when
    /// it covers the gap; otherwise drains the writer and reads the file
    /// (bounded by the file cap). Blocking — callers on the async side wrap
    /// this in `spawn_blocking`.
    pub fn replay_from(&self, last_seq: u64) -> Result<Vec<Arc<SeqEvent>>> {
        {
            let state = self.state.lock().expect("journal state lock");
            let ring_covers = match state.ring.front() {
                Some((front, _)) => front.seq <= last_seq + 1,
                None => state.next_seq == last_seq + 1,
            };
            if ring_covers {
                return Ok(state
                    .ring
                    .iter()
                    .filter(|(e, _)| e.seq > last_seq)
                    .map(|(e, _)| Arc::clone(e))
                    .collect());
            }
        }

        // Ring can't serve: barrier-drain the writer so the file is current,
        // then merge file contents with anything appended meanwhile (which
        // is necessarily still in the ring).
        self.sync();
        let content = fs::read_to_string(&self.path)
            .with_context(|| format!("read {}", self.path.display()))?;
        let mut events: Vec<Arc<SeqEvent>> = Vec::new();
        for line in content.lines() {
            match serde_json::from_str::<SeqEvent>(line) {
                Ok(entry) if entry.seq > last_seq => events.push(Arc::new(entry)),
                Ok(_) => {}
                Err(err) => tracing::warn!(%err, "skipping corrupt journal line"),
            }
        }
        let file_max = events.last().map(|e| e.seq).unwrap_or(last_seq);
        let state = self.state.lock().expect("journal state lock");
        events.extend(
            state
                .ring
                .iter()
                .filter(|(e, _)| e.seq > file_max)
                .map(|(e, _)| Arc::clone(e)),
        );
        Ok(events)
    }

    /// Block until the writer thread has flushed everything queued so far.
    pub fn sync(&self) {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if self.tx.send(WriteOp::Sync(ack_tx)).is_ok() {
            let _ = ack_rx.recv();
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

struct WriterThread {
    file: fs::File,
    path: PathBuf,
    size: u64,
    caps: JournalCaps,
    written_seq: Arc<AtomicU64>,
}

impl WriterThread {
    fn run(mut self, rx: mpsc::Receiver<WriteOp>) {
        while let Ok(op) = rx.recv() {
            match op {
                WriteOp::Line(line) => {
                    let seq = parse_seq(&line);
                    if let Err(err) = self.file.write_all(&line) {
                        tracing::error!(%err, path = %self.path.display(), "journal write failed");
                        continue;
                    }
                    self.size += line.len() as u64;
                    if let Some(seq) = seq {
                        self.written_seq.store(seq, Ordering::Release);
                    }
                    if self.size > self.caps.file_cap {
                        if let Err(err) = self.compact() {
                            tracing::error!(%err, path = %self.path.display(), "journal compaction failed");
                        }
                    }
                }
                WriteOp::Sync(ack) => {
                    let _ = self.file.flush();
                    let _ = ack.send(());
                }
            }
        }
    }

    /// Rewrite the file keeping the newest ~compact_target bytes, cut at a
    /// turn boundary when one exists, with a `Truncated` head marker so
    /// replaying clients know history was dropped (the agent's own
    /// transcript remains the full source of truth).
    fn compact(&mut self) -> Result<()> {
        let content = fs::read_to_string(&self.path)?;
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() < 2 {
            return Ok(());
        }

        // Walk from the end until we've accumulated the target budget…
        let mut kept_bytes = 0u64;
        let mut cut = lines.len();
        while cut > 0 && kept_bytes < self.caps.compact_target {
            cut -= 1;
            kept_bytes += lines[cut].len() as u64 + 1;
        }
        // …then prefer the next turn boundary at or after the cut, so a
        // transcript never resumes mid-turn.
        let boundary = lines[cut..]
            .iter()
            .position(|l| l.contains("\"turn_started\""))
            .map(|off| cut + off);
        let cut = boundary.unwrap_or(cut).min(lines.len() - 1);
        if cut == 0 {
            return Ok(());
        }

        let first_kept_seq = serde_json::from_str::<SeqEvent>(lines[cut])
            .map(|e| e.seq)
            .unwrap_or(1);
        let marker = SeqEvent {
            seq: first_kept_seq.saturating_sub(1),
            ts: now_ms(),
            ev: AgentEvent::Truncated,
        };

        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut out = fs::File::create(&tmp)?;
            let mut buf = serde_json::to_vec(&marker)?;
            buf.push(b'\n');
            for line in &lines[cut..] {
                buf.extend_from_slice(line.as_bytes());
                buf.push(b'\n');
            }
            out.write_all(&buf)?;
            self.size = buf.len() as u64;
        }
        fs::rename(&tmp, &self.path)?;
        self.file = fs::OpenOptions::new().append(true).open(&self.path)?;
        Ok(())
    }
}

/// Cheap seq extraction from a serialized line (`{"seq":N,…`) — avoids a
/// full parse on the write hot path.
fn parse_seq(line: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(line).ok()?;
    let rest = s.strip_prefix("{\"seq\":")?;
    let end = rest.find([',', '}'])?;
    rest[..end].parse().ok()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Maps an agent's native session handle (claude session id / codex thread
/// id) to the chimaera session whose journal holds that conversation, so a
/// resume seeds chat history. Small, capped, atomically rewritten.
#[derive(Default)]
pub struct JournalIndex {
    path: PathBuf,
    entries: Mutex<Vec<IndexEntry>>,
}

#[derive(Clone, Serialize, Deserialize)]
struct IndexEntry {
    native_id: String,
    session_id: String,
    ts: u64,
}

const INDEX_MAX_ENTRIES: usize = 200;

impl JournalIndex {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("index.json");
        let entries = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    pub fn record(&self, native_id: &str, session_id: &str) {
        let mut entries = self.entries.lock().expect("index lock");
        entries.retain(|e| e.native_id != native_id);
        entries.push(IndexEntry {
            native_id: native_id.to_string(),
            session_id: session_id.to_string(),
            ts: now_ms(),
        });
        if entries.len() > INDEX_MAX_ENTRIES {
            let excess = entries.len() - INDEX_MAX_ENTRIES;
            entries.drain(..excess);
        }
        let snapshot = entries.clone();
        drop(entries);
        if let Err(err) = save_atomic(&self.path, &snapshot) {
            tracing::warn!(%err, "failed to save journal index");
        }
    }

    pub fn lookup(&self, native_id: &str) -> Option<String> {
        self.entries
            .lock()
            .expect("index lock")
            .iter()
            .rev()
            .find(|e| e.native_id == native_id)
            .map(|e| e.session_id.clone())
    }
}

fn save_atomic(path: &Path, entries: &[IndexEntry]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec(entries)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Append one event to a session's journal while NO live Journal owns the
/// file (the view toggle stamps ModeSwitch markers between the old process
/// stopping and the new one spawning). Seq continuity comes from the same
/// scan `open` does; the throwaway writer thread drains before return.
pub fn append_marker(dir: &Path, session_id: &str, ev: AgentEvent) -> Result<()> {
    let journal = Journal::open(dir, session_id)?;
    journal.append(ev);
    journal.sync();
    Ok(())
}

/// Enforce the chat-dir budget at daemon start: oldest-mtime journals go
/// first. No live-session exclusions needed — chat sessions die with the
/// daemon, so at startup every journal is history.
pub fn prune_dir(dir: &Path, max_bytes: u64, max_files: usize) -> Result<()> {
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl") {
            if let Ok(meta) = entry.metadata() {
                files.push((path, meta.len(), meta.modified().unwrap_or(UNIX_EPOCH)));
            }
        }
    }
    files.sort_by_key(|(_, _, mtime)| *mtime);

    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    let mut count = files.len();
    for (path, size, _) in &files {
        if total <= max_bytes && count <= max_files {
            break;
        }
        if fs::remove_file(path).is_ok() {
            total -= size;
            count -= 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Usage;

    fn msg(text: &str) -> AgentEvent {
        AgentEvent::MessageChunk {
            turn_id: "t1".into(),
            text: text.into(),
        }
    }

    #[test]
    fn seq_is_monotonic_and_replay_serves_from_ring() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path(), "s-test").unwrap();

        let first = journal.append(msg("one"));
        let second = journal.append(msg("two"));
        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);

        let all = journal.replay_from(0).unwrap();
        assert_eq!(all.len(), 2);
        let gap = journal.replay_from(1).unwrap();
        assert_eq!(gap.len(), 1);
        assert_eq!(gap[0].seq, 2);
        assert!(journal.replay_from(2).unwrap().is_empty());
    }

    #[test]
    fn replay_falls_back_to_file_when_ring_evicted() {
        let dir = tempfile::tempdir().unwrap();
        let caps = JournalCaps {
            ring_max_entries: 4,
            ring_max_bytes: 1024 * 1024,
            ..Default::default()
        };
        let journal = Journal::open_with(dir.path(), "s-test", caps).unwrap();
        for i in 0..20 {
            journal.append(msg(&format!("m{i}")));
        }
        // Ring only holds the newest few; a from-zero replay must still
        // return the full history via the file.
        let all = journal.replay_from(0).unwrap();
        assert_eq!(all.len(), 20);
        assert_eq!(all.first().unwrap().seq, 1);
        assert_eq!(all.last().unwrap().seq, 20);
        // And stays strictly ordered with no duplicates.
        for pair in all.windows(2) {
            assert_eq!(pair[1].seq, pair[0].seq + 1);
        }
    }

    #[test]
    fn reopen_resumes_seq_numbering() {
        let dir = tempfile::tempdir().unwrap();
        {
            let journal = Journal::open(dir.path(), "s-test").unwrap();
            journal.append(msg("one"));
            journal.append(msg("two"));
            journal.sync();
        }
        let journal = Journal::open(dir.path(), "s-test").unwrap();
        let next = journal.append(msg("three"));
        assert_eq!(next.seq, 3);
        let all = journal.replay_from(0).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn compaction_truncates_at_turn_boundary_with_marker() {
        let dir = tempfile::tempdir().unwrap();
        let caps = JournalCaps {
            file_cap: 8 * 1024,
            compact_target: 3 * 1024,
            ..Default::default()
        };
        let journal = Journal::open_with(dir.path(), "s-test", caps).unwrap();

        // Several turns of chunky events to push past the file cap.
        for turn in 0..8 {
            journal.append(AgentEvent::TurnStarted {
                turn_id: format!("turn{turn}"),
            });
            for _ in 0..4 {
                journal.append(msg(&"x".repeat(400)));
            }
            journal.append(AgentEvent::TurnCompleted {
                turn_id: format!("turn{turn}"),
                usage: Usage::default(),
            });
        }
        journal.sync();

        let content = fs::read_to_string(journal.path()).unwrap();
        let first: SeqEvent = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(first.ev, AgentEvent::Truncated, "head marker present");
        let second: SeqEvent = serde_json::from_str(content.lines().nth(1).unwrap()).unwrap();
        assert!(
            matches!(second.ev, AgentEvent::TurnStarted { .. }),
            "history resumes at a turn boundary, got {:?}",
            second.ev
        );
        assert_eq!(second.seq, first.seq + 1, "marker seq abuts kept history");
        assert!((content.len() as u64) < 8 * 1024, "file shrank below cap");
    }

    #[test]
    fn oversized_event_is_replaced_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path(), "s-test").unwrap();
        journal.append(msg(&"x".repeat(MAX_ENTRY_BYTES + 1)));
        let all = journal.replay_from(0).unwrap();
        assert_eq!(all.len(), 1);
        match &all[0].ev {
            AgentEvent::Error { message, fatal } => {
                assert!(message.contains("cap"));
                assert!(!fatal);
            }
            other => panic!("expected Error replacement, got {other:?}"),
        }
    }

    #[test]
    fn index_records_looks_up_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let index = JournalIndex::load(dir.path());
        index.record("native-a", "s-1");
        index.record("native-b", "s-2");
        index.record("native-a", "s-3"); // re-record moves, not duplicates
        assert_eq!(index.lookup("native-a").as_deref(), Some("s-3"));
        assert_eq!(index.lookup("native-b").as_deref(), Some("s-2"));
        assert_eq!(index.lookup("native-zzz"), None);

        // Persisted: a fresh load sees the same entries.
        let reloaded = JournalIndex::load(dir.path());
        assert_eq!(reloaded.lookup("native-a").as_deref(), Some("s-3"));

        for i in 0..(INDEX_MAX_ENTRIES + 50) {
            index.record(&format!("n{i}"), "s-x");
        }
        let entries = index.entries.lock().unwrap();
        assert!(entries.len() <= INDEX_MAX_ENTRIES);
    }

    #[test]
    fn prune_dir_removes_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..6 {
            let path = dir.path().join(format!("s-{i}.jsonl"));
            fs::write(&path, vec![b'x'; 1000]).unwrap();
            // Distinct mtimes, oldest = lowest index.
            let mtime = SystemTime::now() - std::time::Duration::from_secs(600 - i as u64 * 60);
            let file = fs::File::open(&path).unwrap();
            file.set_times(fs::FileTimes::new().set_modified(mtime))
                .unwrap();
        }

        prune_dir(dir.path(), 3500, 100).unwrap();
        let remaining: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        assert_eq!(remaining.len(), 3);
        assert!(remaining.contains(&"s-5.jsonl".to_string()), "newest kept");
        assert!(!remaining.contains(&"s-0.jsonl".to_string()), "oldest gone");

        prune_dir(dir.path(), u64::MAX, 1).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().flatten().count(), 1);
    }
}
