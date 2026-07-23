//! The semantic edit journal — the write half of the bidirectional loop.
//!
//! `describe` tells an agent where everything *is*; the journal tells it what
//! just *changed*, by whom, in named-object terms. One append-only JSONL file
//! per board under `.chimaera/board/journal/`, read back cheaply with
//! `chimaera board journal <board> --since N` — no full-file diff, no
//! guessing.
//!
//! Three disciplines shape everything here:
//!
//! - **Seq-first, no wall clock.** Every event is `{"seq": N, "actor": …,
//!   "event": …, …}` with seq strictly increasing from 1. There are
//!   deliberately no timestamps — the board format's no-churn/no-nonce rule
//!   extends to its surround, and determinism keeps every test and every
//!   replay honest. Order *is* the time.
//! - **Gaps are legal.** The size cap compacts by dropping the oldest events
//!   while preserving the survivors' seq numbers, so a reader must treat seq
//!   as ordered-and-unique, never dense. `--since N` still means exactly
//!   "everything after N".
//! - **Coalescing is the caller's job, helped by the API.** A 900 ms drag is
//!   one `move` with from/to, not 60 frames: callers record the *final*
//!   position of a gesture, and [`Journal::append_batch`] additionally
//!   collapses consecutive same-actor same-object moves within one batch.
//!
//! The journal is keyed by the board's workspace-relative *path*
//! ([`journal_path`]): renaming the board — or a directory above it — mints a
//! new key and orphans the old history. That is accepted for now; the plan's
//! rename-follow (re-keying on the same event that rewrites tab paths) is a
//! daemon-level concern layered on top, not something a path-derived key can
//! solve here.

use std::fmt;
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Compaction trigger: past this, the file is rewritten down to
/// [`COMPACT_TARGET_BYTES`].
pub const CAP_BYTES: u64 = 512 * 1024;

/// What survives a compaction: the newest events that fit here.
pub const COMPACT_TARGET_BYTES: u64 = 256 * 1024;

/// Who did it. The coarse three-way split the UI, CLI and daemon share; a
/// finer per-agent identity can widen this later without re-keying history
/// (unknown actors deserialize as a parse failure today, which readers skip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    Human,
    Agent,
    Daemon,
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Actor::Human => "human",
            Actor::Agent => "agent",
            Actor::Daemon => "daemon",
        })
    }
}

/// One journal entry. Serializes seq-first:
/// `{"seq":12,"actor":"human","event":"move","object":"callout",…}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Assigned by the journal on append — a caller-supplied value is
    /// overwritten, so build events with [`Event::new`] and let the journal
    /// number them.
    pub seq: u64,
    pub actor: Actor,
    #[serde(flatten)]
    pub kind: EventKind,
}

impl Event {
    /// An event awaiting its seq (stamped by [`Journal::append`]).
    pub fn new(actor: Actor, kind: EventKind) -> Self {
        Event {
            seq: 0,
            actor,
            kind,
        }
    }

    /// The human-readable one-liner the CLI prints:
    /// `#12 human moved callout [616, 208] → [520, 360]`.
    pub fn render(&self) -> String {
        let pt = |p: [f64; 2]| format!("[{}, {}]", p[0], p[1]);
        let body = match &self.kind {
            EventKind::Move { object, from, to } => {
                format!("moved {object} {} → {}", pt(*from), pt(*to))
            }
            EventKind::Resize { object, from, to } => {
                format!("resized {object} {} → {}", pt(*from), pt(*to))
            }
            EventKind::ObjectAdded { object, kind, page } => {
                format!("added {kind} {object} on {page}")
            }
            EventKind::ObjectRemoved { object, kind, page } => {
                format!("removed {kind} {object} from {page}")
            }
            EventKind::TextEdited { object } => format!("edited text of {object}"),
            EventKind::Restyle { object, changed } => {
                let fields = changed
                    .iter()
                    .map(|(path, value)| format!("{path} → {value}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("restyled {object} ({fields})")
            }
            EventKind::PageAdded { page } => format!("added page {page}"),
            EventKind::PageRemoved { page } => format!("removed page {page}"),
            EventKind::PageReordered { page, from, to } => {
                format!("reordered page {page} {from} → {to}")
            }
            EventKind::PageMerged { page, into } => format!("merged page {page} into {into}"),
            EventKind::PageSplit { page, into } => {
                format!("split page {page} into {}", into.join(", "))
            }
            EventKind::ObjectMovedToPage { object, from, to } => {
                format!("moved {object} from page {from} to page {to}")
            }
            EventKind::IntentChanged { page, kind } => {
                format!("set intent of page {page} to {kind}")
            }
            EventKind::BriefChanged => "edited the brief".to_string(),
            EventKind::Shown => "showed this board".to_string(),
            EventKind::Comment {
                page,
                object,
                at,
                pin,
                text,
            } => match (object, at) {
                (Some(object), _) => format!("commented {pin} on {object} on {page}: {text}"),
                (None, Some(p)) => format!("commented {pin} on {page} at {}: {text}", pt(*p)),
                (None, None) => format!("commented {pin} on {page}: {text}"),
            },
            EventKind::CommentResolved { pin } => format!("resolved comment {pin}"),
        };
        format!("#{} {} {}", self.seq, self.actor, body)
    }
}

/// The event vocabulary, v1.
///
/// Object-scoped events carry positions in the same points the file uses;
/// content events (`text-edited`, `brief-changed`) carry *no* content — the
/// board file has it, and duplicating it here would bloat the journal and
/// invite drift. The structural ops are §6.3b of the board plan verbatim
/// (`page-reordered`, `page-merged`, `page-split`, `object-moved-to-page`,
/// `intent-changed`, `brief-changed`), spelled in this journal's kebab-case:
/// they exist so "cut to 8 slides" leaves a trace an agent can read instead
/// of inferring a restructure from a full-file diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum EventKind {
    /// `from`/`to` are the object's `at` in points.
    Move {
        object: String,
        from: [f64; 2],
        to: [f64; 2],
    },
    /// `from`/`to` are the object's `size` in points.
    Resize {
        object: String,
        from: [f64; 2],
        to: [f64; 2],
    },
    ObjectAdded {
        object: String,
        kind: String,
        page: String,
    },
    ObjectRemoved {
        object: String,
        kind: String,
        page: String,
    },
    TextEdited {
        object: String,
    },
    /// Sparse configuration change (the plan §6.3's `restyle`): dot-paths
    /// over the object's canonical JSON with the values the file now holds
    /// (`{"x.title": "Time (s)"}`; null = the field was cleared). The one
    /// object event that names *fields* — a reader sees what about the object
    /// changed without diffing the file. `BTreeMap` for deterministic key
    /// order, like every serialized map in the format.
    Restyle {
        object: String,
        changed: std::collections::BTreeMap<String, serde_json::Value>,
    },
    PageAdded {
        page: String,
    },
    PageRemoved {
        page: String,
    },
    /// `from`/`to` are 0-based page indices.
    PageReordered {
        page: String,
        from: usize,
        to: usize,
    },
    PageMerged {
        page: String,
        into: String,
    },
    PageSplit {
        page: String,
        into: Vec<String>,
    },
    ObjectMovedToPage {
        object: String,
        from: String,
        to: String,
    },
    IntentChanged {
        page: String,
        kind: String,
    },
    BriefChanged,
    /// The board was (re-)emitted by `board show` — the surfacing signal a
    /// ShownCard consumer keys on (board plan §10). Content-free like the
    /// other content events: the shown board file beside this journal is the
    /// card; a re-show with the same id appends another `shown`, which is how
    /// an in-place card update announces itself.
    Shown,
    /// A comment pin (board plan §6.4): page-scoped, optionally bound to an
    /// object, optionally at a stored canvas point, with its text. The one
    /// deliberate exception to "content events carry no content" — pins live
    /// ONLY in the journal, never in the board file (whose diffability must
    /// not be polluted by conversation), so the journal is the sole carrier
    /// of the words. Compaction preserves an unresolved pin regardless of
    /// age (see [`Journal::append_batch`]'s compaction).
    Comment {
        page: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        object: Option<String>,
        /// The pin's canvas point — where a point pin renders, and where an
        /// object-bound pin falls back if its object is later removed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<[f64; 2]>,
        pin: String,
        text: String,
    },
    /// Resolves the pin (`comment-resolved` — the plan's `comment.resolved`
    /// in this journal's kebab-case). Once resolved, the pair may compact
    /// away like any other event.
    CommentResolved {
        pin: String,
    },
}

/// Where a board's journal lives:
/// `<workspace>/.chimaera/board/journal/<stem>-<hash8>.jsonl`.
///
/// The id is the board file's stem plus the first 8 hex of the sha256 of its
/// workspace-relative path — readable in a directory listing (`fig2-…`),
/// collision-free across same-named boards in different directories, and
/// **path-derived**: renaming the board or any directory above it re-keys the
/// journal. Callers must pass the same canonical form of `board_path` every
/// time (the CLI and the daemon both canonicalize first) or they will derive
/// different keys for one board.
pub fn journal_path(workspace: &Path, board_path: &Path) -> PathBuf {
    let rel = board_path.strip_prefix(workspace).unwrap_or(board_path);
    let rel = rel.to_string_lossy().replace('\\', "/");
    let mut h = Sha256::new();
    h.update(rel.as_bytes());
    let digest = h.finalize();
    let mut hash = String::with_capacity(8);
    for b in &digest[..4] {
        use std::fmt::Write as _;
        let _ = write!(hash, "{b:02x}");
    }
    let name = board_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("board");
    let stem = match name.to_ascii_lowercase().strip_suffix(".board.json") {
        Some(lower_stem) if !lower_stem.is_empty() => &name[..lower_stem.len()],
        _ => name,
    };
    crate::board_dir(workspace)
        .join("journal")
        .join(format!("{stem}-{hash}.jsonl"))
}

/// The append handle. Holds no open file — every append reopens by path, so
/// a compaction's temp+rename (which replaces the inode) can never strand a
/// writer on the unlinked file.
pub struct Journal {
    path: PathBuf,
    next_seq: u64,
    /// Unparseable lines seen on open — a torn tail, a truncated write.
    /// Surfaced, never fatal: the journal is an audit trail, not truth.
    warnings: usize,
    /// The file ends without a newline (a crash-torn tail). The next append
    /// writes a leading `\n` so the torn fragment stays its own (skipped)
    /// line instead of corrupting the new event.
    needs_newline: bool,
    cap_bytes: u64,
    compact_target: u64,
}

impl Journal {
    /// Open (or start) the journal at `path`, scanning it line by line for
    /// the last seq. Corrupt lines anywhere are skipped and counted — the
    /// next seq continues from the last *parseable* event.
    pub fn open(path: &Path) -> Result<Journal> {
        let mut last_seq = 0u64;
        let mut warnings = 0usize;
        let mut needs_newline = false;
        match std::fs::File::open(path) {
            Ok(file) => {
                let mut reader = std::io::BufReader::new(file);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => {
                            needs_newline = !line.ends_with('\n');
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<Event>(trimmed) {
                                Ok(ev) => last_seq = last_seq.max(ev.seq),
                                Err(_) => warnings += 1,
                            }
                        }
                        // A byte-level read error (non-UTF-8 tail): count it
                        // and stop — nothing past it is trustworthy.
                        Err(_) => {
                            warnings += 1;
                            needs_newline = true;
                            break;
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
        }
        Ok(Journal {
            path: path.to_path_buf(),
            next_seq: last_seq + 1,
            warnings,
            needs_newline,
            cap_bytes: CAP_BYTES,
            compact_target: COMPACT_TARGET_BYTES,
        })
    }

    /// Corrupt lines seen when this journal was opened.
    pub fn warnings(&self) -> usize {
        self.warnings
    }

    /// Append one event, stamping its seq (any caller-supplied seq is
    /// overwritten). Returns the assigned seq.
    pub fn append(&mut self, event: Event) -> Result<u64> {
        let seqs = self.append_batch(vec![event])?;
        Ok(seqs[0])
    }

    /// Append a batch, collapsing consecutive `move` events for the same
    /// object by the same actor (first `from`, last `to`) — one gesture, one
    /// event, even when the caller buffered intermediate positions. Returns
    /// the assigned seqs, in order.
    pub fn append_batch(&mut self, events: Vec<Event>) -> Result<Vec<u64>> {
        let mut batch: Vec<Event> = Vec::with_capacity(events.len());
        for e in events {
            if let (Some(prev), EventKind::Move { object, to, .. }) = (batch.last_mut(), &e.kind) {
                if prev.actor == e.actor {
                    if let EventKind::Move {
                        object: prev_object,
                        to: prev_to,
                        ..
                    } = &mut prev.kind
                    {
                        if prev_object == object {
                            *prev_to = *to;
                            continue;
                        }
                    }
                }
            }
            batch.push(e);
        }
        if batch.is_empty() {
            return Ok(Vec::new());
        }

        let mut out = String::new();
        if std::mem::take(&mut self.needs_newline) {
            out.push('\n');
        }
        let mut seqs = Vec::with_capacity(batch.len());
        for mut event in batch {
            event.seq = self.next_seq;
            self.next_seq += 1;
            seqs.push(event.seq);
            out.push_str(&serde_json::to_string(&event).context("serializing a journal event")?);
            out.push('\n');
        }

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        file.write_all(out.as_bytes())
            .with_context(|| format!("appending to {}", self.path.display()))?;
        drop(file);

        if std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0) > self.cap_bytes {
            self.compact()?;
        }
        Ok(seqs)
    }

    /// Convenience for the most common gesture.
    pub fn record_move(
        &mut self,
        actor: Actor,
        object: &str,
        from: [f64; 2],
        to: [f64; 2],
    ) -> Result<u64> {
        self.append(Event::new(
            actor,
            EventKind::Move {
                object: object.to_string(),
                from,
                to,
            },
        ))
    }

    /// Rewrite the file keeping the newest events that fit the compaction
    /// target, with their seq numbers intact — the resulting leading gap is
    /// legal and expected. Unresolved comment pins additionally survive
    /// regardless of age (see inline). Atomic (temp + rename), and corrupt
    /// lines do not survive it. Memory stays bounded: the file only ever
    /// exceeds the cap by one append batch, so reading it whole here is
    /// capped too.
    fn compact(&mut self) -> Result<()> {
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("opening {}", self.path.display())),
        };
        let mut lines: Vec<(String, Event)> = Vec::new();
        let mut reader = std::io::BufReader::new(file);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        if let Ok(ev) = serde_json::from_str::<Event>(trimmed) {
                            lines.push((trimmed.to_string(), ev));
                        }
                    }
                }
                Err(_) => break,
            }
        }

        // Walk from the newest, keeping whole lines while they fit; always
        // keep at least the newest event even if it alone busts the target.
        let mut budget = self.compact_target as usize;
        let mut keep_from = lines.len();
        while keep_from > 0 {
            let cost = lines[keep_from - 1].0.len() + 1;
            if cost > budget && keep_from < lines.len() {
                break;
            }
            budget = budget.saturating_sub(cost);
            keep_from -= 1;
        }

        // Unresolved comment pins in the dropped prefix survive anyway: pins
        // live only in the journal (board plan §6.3/§6.4), so losing one at a
        // cap boundary would silently erase an open annotation — a spooky,
        // hard-to-reproduce bug the plan calls fatal. Judged newest→oldest
        // against LATER resolutions only, so a re-used pin id (comment →
        // resolved → comment again) keeps its fresh incarnation. A resolved
        // pair compacts away like anything else. Preserved pins may push the
        // result past the compaction target; the file shrinks again once
        // they resolve.
        let mut resolved_later: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut preserved = vec![false; keep_from];
        for i in (0..lines.len()).rev() {
            match &lines[i].1.kind {
                EventKind::CommentResolved { pin } => {
                    resolved_later.insert(pin.as_str());
                }
                EventKind::Comment { pin, .. }
                    if i < keep_from && !resolved_later.contains(pin.as_str()) =>
                {
                    preserved[i] = true;
                }
                _ => {}
            }
        }

        let mut out = String::new();
        for (i, (l, _)) in lines.iter().enumerate() {
            if i >= keep_from || preserved[i] {
                out.push_str(l);
                out.push('\n');
            }
        }
        crate::write_atomic(&self.path, out.as_bytes())?;
        self.needs_newline = false;
        Ok(())
    }
}

/// Read every event with `seq > since_seq`, streaming line by line. A missing
/// journal is an empty one; unparseable lines are skipped.
pub fn read_since(path: &Path, since_seq: u64) -> Result<Vec<Event>> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    };
    let mut events = Vec::new();
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(ev) = serde_json::from_str::<Event>(trimmed) {
                    if ev.seq > since_seq {
                        events.push(ev);
                    }
                }
            }
            Err(_) => break,
        }
    }
    Ok(events)
}

/// The highest seq in the journal, or 0 when there is none (missing file
/// included).
pub fn latest_seq(path: &Path) -> Result<u64> {
    Ok(summary(path).map(|(_, latest)| latest).unwrap_or(0))
}

/// `(event count, latest seq)` for an existing, non-empty journal — what
/// `describe` prints as `journal: N events · latest seq M`. `None` when the
/// journal is missing or holds no parseable events.
pub fn summary(path: &Path) -> Option<(u64, u64)> {
    let file = std::fs::File::open(path).ok()?;
    let mut count = 0u64;
    let mut latest = 0u64;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(ev) = serde_json::from_str::<Event>(trimmed) {
                    count += 1;
                    latest = latest.max(ev.seq);
                }
            }
            Err(_) => break,
        }
    }
    if count == 0 {
        None
    } else {
        Some((count, latest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_journal(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-board-journal-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("test.jsonl")
    }

    fn mv(object: &str, from: [f64; 2], to: [f64; 2]) -> Event {
        Event::new(
            Actor::Human,
            EventKind::Move {
                object: object.to_string(),
                from,
                to,
            },
        )
    }

    #[test]
    fn append_and_read_round_trip_seq_first() {
        let path = tmp_journal("round-trip");
        let mut j = Journal::open(&path).unwrap();
        let s1 = j
            .record_move(Actor::Human, "callout", [616.0, 208.0], [520.0, 360.0])
            .unwrap();
        let s2 = j
            .append(Event::new(
                Actor::Agent,
                EventKind::Resize {
                    object: "bench-chart".to_string(),
                    from: [420.0, 320.0],
                    to: [460.0, 350.0],
                },
            ))
            .unwrap();
        let s3 = j
            .append(Event::new(
                Actor::Daemon,
                EventKind::IntentChanged {
                    page: "bench".to_string(),
                    kind: "claim-evidence".to_string(),
                },
            ))
            .unwrap();
        assert_eq!((s1, s2, s3), (1, 2, 3));

        // The wire shape is pinned: seq first, then actor, then the event
        // tag — and no timestamp anywhere.
        let raw = std::fs::read_to_string(&path).unwrap();
        let first = raw.lines().next().unwrap();
        assert!(
            first.starts_with(r#"{"seq":1,"actor":"human","event":"move","object":"callout""#),
            "{first}"
        );
        assert!(!raw.contains("\"ts\""), "no wall clock in the journal");

        let events = read_since(&path, 0).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].render(),
            "#1 human moved callout [616, 208] → [520, 360]"
        );
        assert_eq!(
            events[1].render(),
            "#2 agent resized bench-chart [420, 320] → [460, 350]"
        );
        assert_eq!(
            events[2].render(),
            "#3 daemon set intent of page bench to claim-evidence"
        );

        // --since N means strictly after N.
        let newer = read_since(&path, 2).unwrap();
        assert_eq!(newer.len(), 1);
        assert_eq!(newer[0].seq, 3);
        assert_eq!(latest_seq(&path).unwrap(), 3);
        assert_eq!(summary(&path), Some((3, 3)));
    }

    #[test]
    fn seq_continues_across_reopen() {
        let path = tmp_journal("reopen");
        let mut j = Journal::open(&path).unwrap();
        j.append(mv("a", [0.0, 0.0], [8.0, 8.0])).unwrap();
        j.append(mv("b", [0.0, 0.0], [16.0, 8.0])).unwrap();
        drop(j);
        let mut j = Journal::open(&path).unwrap();
        assert_eq!(j.warnings(), 0);
        let seq = j.append(mv("c", [0.0, 0.0], [24.0, 8.0])).unwrap();
        assert_eq!(seq, 3, "next seq is last + 1 across reopen");
    }

    #[test]
    fn corrupt_tail_is_tolerated_and_isolated() {
        let path = tmp_journal("corrupt-tail");
        let mut j = Journal::open(&path).unwrap();
        j.append(mv("a", [0.0, 0.0], [8.0, 8.0])).unwrap();
        // A crash-torn tail: half an event, no trailing newline.
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(br#"{"seq":2,"actor":"hum"#).unwrap();
        }
        let mut j = Journal::open(&path).unwrap();
        assert_eq!(j.warnings(), 1, "the torn line is counted, not fatal");
        let seq = j.append(mv("b", [8.0, 8.0], [16.0, 8.0])).unwrap();
        assert_eq!(seq, 2, "seq continues from the last parseable event");
        // The torn fragment stayed its own line; readers skip it.
        let events = read_since(&path, 0).unwrap();
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [1, 2]);
    }

    #[test]
    fn batch_coalesces_consecutive_moves_of_one_object() {
        let path = tmp_journal("coalesce");
        let mut j = Journal::open(&path).unwrap();
        let seqs = j
            .append_batch(vec![
                mv("callout", [616.0, 208.0], [600.0, 220.0]),
                mv("callout", [600.0, 220.0], [560.0, 300.0]),
                mv("callout", [560.0, 300.0], [520.0, 360.0]),
                mv("other", [0.0, 0.0], [8.0, 8.0]),
            ])
            .unwrap();
        assert_eq!(seqs, [1, 2], "three drags of one object are one event");
        let events = read_since(&path, 0).unwrap();
        assert_eq!(
            events[0].render(),
            "#1 human moved callout [616, 208] → [520, 360]",
            "first from, last to"
        );
        assert_eq!(events[1].render(), "#2 human moved other [0, 0] → [8, 8]");

        // A different actor between them breaks the run: no cross-actor merge.
        let mut agent_move = mv("callout", [1.0, 1.0], [2.0, 2.0]);
        agent_move.actor = Actor::Agent;
        let seqs = j
            .append_batch(vec![
                mv("callout", [0.0, 0.0], [1.0, 1.0]),
                agent_move,
                mv("callout", [2.0, 2.0], [3.0, 3.0]),
            ])
            .unwrap();
        assert_eq!(seqs, [3, 4, 5]);
    }

    #[test]
    fn size_cap_keeps_the_newest_events_and_their_seqs() {
        let path = tmp_journal("cap");
        let mut j = Journal::open(&path).unwrap();
        // Shrink the caps so the test stays fast; the mechanism is identical.
        j.cap_bytes = 8 * 1024;
        j.compact_target = 4 * 1024;
        for i in 0..200u64 {
            j.append(mv(&format!("obj-{i}"), [0.0, 0.0], [i as f64, i as f64]))
                .unwrap();
        }
        let len = std::fs::metadata(&path).unwrap().len();
        assert!(
            len <= 8 * 1024,
            "compaction kept the file under the cap: {len}"
        );

        let events = read_since(&path, 0).unwrap();
        assert!(events.len() < 200, "the oldest events were dropped");
        assert_eq!(
            events.last().unwrap().seq,
            200,
            "the newest event always survives"
        );
        let first = events.first().unwrap().seq;
        assert!(first > 1, "a leading seq gap is legal and expected");
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        let expect: Vec<u64> = (first..=200).collect();
        assert_eq!(
            seqs, expect,
            "survivors keep their original, contiguous seqs"
        );

        // Appending after a compaction continues the numbering.
        drop(j);
        let mut j = Journal::open(&path).unwrap();
        assert_eq!(j.append(mv("tail", [0.0, 0.0], [8.0, 8.0])).unwrap(), 201);
    }

    #[test]
    fn journal_path_is_stable_and_collision_free() {
        let ws = Path::new("/work/repo");
        let a = journal_path(ws, Path::new("/work/repo/figures/fig2.board.json"));
        let b = journal_path(ws, Path::new("/work/repo/figures/fig2.board.json"));
        assert_eq!(a, b, "same board, same key");
        assert!(a.starts_with("/work/repo/.chimaera/board/journal"), "{a:?}");
        let name = a.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("fig2-"),
            "stem survives in the key: {name}"
        );
        assert!(name.ends_with(".jsonl"), "{name}");
        assert_eq!(name.len(), "fig2-".len() + 8 + ".jsonl".len());

        // Same stem, different directory → different key.
        let c = journal_path(ws, Path::new("/work/repo/talks/fig2.board.json"));
        assert_ne!(a, c);
        // The key is path-derived: moving the board re-keys the journal.
        let d = journal_path(ws, Path::new("/work/repo/figures/v2/fig2.board.json"));
        assert_ne!(a, d);
    }

    #[test]
    fn shown_event_round_trips() {
        let path = tmp_journal("shown");
        let mut j = Journal::open(&path).unwrap();
        let seq = j
            .append(Event::new(Actor::Agent, EventKind::Shown))
            .unwrap();
        assert_eq!(seq, 1);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            raw.trim(),
            r#"{"seq":1,"actor":"agent","event":"shown"}"#,
            "content-free, seq-first, no timestamp"
        );
        let events = read_since(&path, 0).unwrap();
        assert_eq!(events[0].render(), "#1 agent showed this board");
    }

    fn pin(id: &str, object: Option<&str>, at: Option<[f64; 2]>, text: &str) -> Event {
        Event::new(
            Actor::Human,
            EventKind::Comment {
                page: "bench".to_string(),
                object: object.map(str::to_string),
                at,
                pin: id.to_string(),
                text: text.to_string(),
            },
        )
    }

    fn resolve(id: &str) -> Event {
        Event::new(
            Actor::Human,
            EventKind::CommentResolved {
                pin: id.to_string(),
            },
        )
    }

    #[test]
    fn comment_pins_round_trip_and_render() {
        let path = tmp_journal("comment");
        let mut j = Journal::open(&path).unwrap();
        j.append(pin(
            "c1",
            Some("callout"),
            None,
            "say the median, not the best case",
        ))
        .unwrap();
        j.append(pin("c2", None, Some([320.0, 96.0]), "this section drags"))
            .unwrap();
        j.append(resolve("c1")).unwrap();

        // The wire shape is the plan's §6.3 line, minus its `ts` (no wall
        // clock in this journal) and with `event` as the tag key: absent
        // object/at fields are omitted, never null.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut it = raw.lines();
        assert_eq!(
            it.next().unwrap(),
            r#"{"seq":1,"actor":"human","event":"comment","page":"bench","object":"callout","pin":"c1","text":"say the median, not the best case"}"#
        );
        assert_eq!(
            it.next().unwrap(),
            r#"{"seq":2,"actor":"human","event":"comment","page":"bench","at":[320.0,96.0],"pin":"c2","text":"this section drags"}"#
        );
        assert_eq!(
            it.next().unwrap(),
            r#"{"seq":3,"actor":"human","event":"comment-resolved","pin":"c1"}"#
        );

        let events = read_since(&path, 0).unwrap();
        assert_eq!(
            events[0].render(),
            "#1 human commented c1 on callout on bench: say the median, not the best case"
        );
        assert_eq!(
            events[1].render(),
            "#2 human commented c2 on bench at [320, 96]: this section drags"
        );
        assert_eq!(events[2].render(), "#3 human resolved comment c1");
    }

    #[test]
    fn compaction_preserves_unresolved_comment_pins() {
        let path = tmp_journal("pin-cap");
        let mut j = Journal::open(&path).unwrap();
        j.cap_bytes = 8 * 1024;
        j.compact_target = 4 * 1024;

        // An old unresolved pin, then enough moves to compact several times.
        j.append(pin("c1", Some("callout"), None, "tighten this"))
            .unwrap();
        for i in 0..200u64 {
            j.append(mv(&format!("obj-{i}"), [0.0, 0.0], [i as f64, i as f64]))
                .unwrap();
        }
        let events = read_since(&path, 0).unwrap();
        assert!(events.len() < 201, "the oldest moves were dropped");
        let first_move = events
            .iter()
            .find(|e| matches!(e.kind, EventKind::Move { .. }))
            .unwrap();
        assert!(
            first_move.seq > 2,
            "moves right after the pin were compacted away"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::Comment { pin, .. } if pin == "c1")),
            "the unresolved pin survived every compaction"
        );

        // Resolving frees the pair: after the next compaction cycles, the
        // old comment no longer haunts the file.
        j.append(resolve("c1")).unwrap();
        for i in 0..200u64 {
            j.append(mv(&format!("post-{i}"), [0.0, 0.0], [i as f64, i as f64]))
                .unwrap();
        }
        let events = read_since(&path, 0).unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::Comment { pin, .. } if pin == "c1")),
            "a resolved pin's comment compacts away"
        );

        // A re-used pin id after its resolution is a FRESH pin: only later
        // resolutions count against a comment.
        j.append(pin("c1", None, Some([8.0, 8.0]), "second thoughts"))
            .unwrap();
        for i in 0..200u64 {
            j.append(mv(&format!("again-{i}"), [0.0, 0.0], [i as f64, i as f64]))
                .unwrap();
        }
        let events = read_since(&path, 0).unwrap();
        let kept: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::Comment { pin, .. } if pin == "c1"))
            .collect();
        assert_eq!(kept.len(), 1, "the re-pinned incarnation survives");
        assert!(
            matches!(&kept[0].kind, EventKind::Comment { text, .. } if text == "second thoughts")
        );
    }

    #[test]
    fn unknown_event_kinds_are_skipped_not_fatal() {
        let path = tmp_journal("unknown-event");
        std::fs::write(
            &path,
            concat!(
                r#"{"seq":1,"actor":"human","event":"move","object":"a","from":[0,0],"to":[8,8]}"#,
                "\n",
                r#"{"seq":2,"actor":"human","event":"from-the-future","object":"a"}"#,
                "\n",
                r#"{"seq":3,"actor":"human","event":"page-added","page":"p2"}"#,
                "\n"
            ),
        )
        .unwrap();
        let events = read_since(&path, 0).unwrap();
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [1, 3], "an unknown event is skipped, the rest read");
        // The writer still continues past it.
        let mut j = Journal::open(&path).unwrap();
        assert_eq!(j.warnings(), 1);
        assert_eq!(j.append(mv("b", [0.0, 0.0], [8.0, 8.0])).unwrap(), 4);
    }
}
