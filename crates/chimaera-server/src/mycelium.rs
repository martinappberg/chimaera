//! Read-only reader for a workspace's mycelium project knowledge — the
//! structured provider behind the Knowledge view
//! (docs/timeline-knowledge-plugins-plan.md §5, §6.3).
//!
//! - **Read-only, always.** Agents write knowledge through mycelium's skills
//!   and hooks; Chimaera only reads it. Nothing here creates, locks, or
//!   rewrites a file, and `.mycelium/locks` is never touched.
//! - **mycelium 0.7.2 is the format.** Findings (`.living/findings/<topic>.md`:
//!   `## F-NNN: claim`, `**Status:**`, an `### Evidence Ledger` table,
//!   `### Open Questions`), decisions and learnings (`### [YYYY-MM-DD] Title`
//!   at column 1 + `**Field**:` lines), `todo/TODO_REGISTRY.md`, and the
//!   `.mycelium/last-session.md` handoff (either schema mycelium's
//!   `finalize_handoff.py` accepts). Parsing matches what mycelium writes and
//!   is lenient beyond it: malformed input degrades to fewer items plus a
//!   `warnings` line. It never errors and never panics.
//! - **Fence-aware.** Headings and fields inside ``` / ~~~ fences (the
//!   CommonMark rules of mycelium's `markdown_fences.py`) or HTML comments are
//!   content, not entries. mycelium's own `collect_entries` is not
//!   fence-aware; an example entry in a code block must not become knowledge.
//! - **Bounded.** A file over [`MAX_FILE_BYTES`] is skipped unread, and every
//!   collection and text field has a cap (constants below). Symlinks are never
//!   followed, so the reader cannot leave the workspace.
//! - **Blocking.** Plain `std::fs`: callers run [`read`] and [`stamp`] under
//!   `spawn_blocking`, never on the reactor. [`stamp`] is the metadata-only
//!   check that lets a caller skip a re-parse when nothing changed.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;

/// A knowledge file over this size is skipped unread. mycelium's logs are
/// append-only prose (2 MiB is years of entries), so a bigger file is a
/// pasted dump, and parsing it would spend the login node's RSS budget.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Bytes read across all files by one [`read`]: 200 topic files at the
/// per-file cap would otherwise be 400 MiB of NFS I/O for one view. Topic
/// files are read last, so they are what a spent budget drops.
const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
/// Topic files read from `.living/findings/`, first by slug.
const MAX_TOPIC_FILES: usize = 200;
/// Entry / `F-` headings examined per file. Only [`MAX_ENTRIES`] are ever
/// materialized, but a hostile file of bare headings would otherwise hold
/// hundreds of thousands of spans at once. Logs keep the LAST ones (they
/// append); topic files the first.
const MAX_SCANNED_ENTRIES: usize = 5000;
/// Directory entries examined per listing, so a runaway directory can't turn
/// a cheap [`stamp`] into a crawl.
const MAX_DIR_ENTRIES: usize = 4096;
/// Items kept per kind (decisions, learnings, findings, todos, questions).
/// Decisions and learnings keep the newest.
const MAX_ENTRIES: usize = 400;
/// Evidence rows kept per finding: the newest, since ledgers append.
const MAX_LEDGER_ROWS: usize = 50;
/// Ceiling for every text field, "…" included; cuts land on a char boundary.
const MAX_TEXT_BYTES: usize = 2 * 1024;
const MAX_TAGS: usize = 20;
const MAX_QUESTIONS_PER_FINDING: usize = 20;
/// Items kept per list (handoff blockers / next steps, a decision's
/// alternatives).
const MAX_LIST_ITEMS: usize = 50;
/// `.mycelium/run/<host>/<session>/` directories examined for a fallback
/// handoff. mycelium deletes a run dir's handoff once Stop accepts it, so
/// only in-flight or abandoned sessions remain.
const MAX_RUN_DIRS: usize = 256;
const MAX_WARNINGS: usize = 50;

const LIVING: &str = ".living";
const PROTOCOL_FILE: &str = "MYCELIUM.md";
const STATUS_BEGIN: &str = "<!-- BEGIN MYCELIUM LIFECYCLE STATUS -->";
const STATUS_END: &str = "<!-- END MYCELIUM LIFECYCLE STATUS -->";

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

/// Everything the Knowledge view shows from mycelium. A missing source is an
/// empty section, never an error.
#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Knowledge {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) left_off: Option<LeftOff>,
    /// By slug.
    pub(crate) topics: Vec<Topic>,
    /// Newest first; undated last.
    pub(crate) decisions: Vec<Decision>,
    /// Newest first; undated last.
    pub(crate) learnings: Vec<Learning>,
    /// Registry order.
    pub(crate) todos: Vec<Todo>,
    /// Every finding's open questions, deduplicated.
    pub(crate) questions: Vec<OpenQuestion>,
    pub(crate) counts: Counts,
    /// Human-readable notes: skipped files, legacy formats, caps hit.
    pub(crate) warnings: Vec<String>,
}

/// The session handoff ("Where we left off").
#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct LeftOff {
    pub(crate) worked_on: String,
    pub(crate) decisions: String,
    pub(crate) blockers: Vec<String>,
    pub(crate) current: String,
    pub(crate) next: Vec<String>,
    /// The handoff file's mtime, ms since the epoch.
    pub(crate) written_ms: u64,
    /// Workspace-relative.
    pub(crate) path: String,
    /// Set only when no accepted handoff exists and this one came from an
    /// in-flight `.mycelium/run/<host>/<session-id>/` — the agent's own id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) host: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Topic {
    /// The file stem.
    pub(crate) slug: String,
    pub(crate) description: String,
    /// Workspace-relative.
    pub(crate) path: String,
    /// By numeric id.
    pub(crate) findings: Vec<Finding>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Finding {
    /// As written, e.g. "F-003".
    pub(crate) id: String,
    pub(crate) claim: String,
    /// preliminary | supported | robust | contradicted | unknown — mycelium
    /// derives it from the ledger; it is read here, never computed.
    pub(crate) status: String,
    pub(crate) implications: String,
    pub(crate) tags: Vec<String>,
    pub(crate) ledger: Vec<LedgerRow>,
    pub(crate) questions: Vec<String>,
    /// 1-based line of the `F-` heading, for "open in file".
    pub(crate) line: u32,
    /// Newest ledger date, else the topic's `last_updated`.
    pub(crate) updated: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct LedgerRow {
    pub(crate) date: String,
    pub(crate) run: String,
    pub(crate) dataset: String,
    pub(crate) project: String,
    pub(crate) result: String,
    /// supports | contradicts | refines | unknown
    pub(crate) direction: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Decision {
    /// [`fingerprint`] — mycelium's `D-N` ids are positional and renumber.
    pub(crate) fp: String,
    pub(crate) date: String,
    pub(crate) title: String,
    pub(crate) context: String,
    pub(crate) decision: String,
    pub(crate) alternatives: Vec<String>,
    pub(crate) rationale: String,
    pub(crate) consequences: String,
    pub(crate) tags: Vec<String>,
    pub(crate) line: u32,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Learning {
    /// [`fingerprint`] — mycelium's `L-N` ids are positional and renumber.
    pub(crate) fp: String,
    pub(crate) date: String,
    pub(crate) title: String,
    /// gotcha | edge-case | insight | failure | tip | other
    pub(crate) category: String,
    pub(crate) what: String,
    pub(crate) why: String,
    pub(crate) resolution: String,
    pub(crate) tags: Vec<String>,
    pub(crate) line: u32,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Todo {
    pub(crate) item: String,
    pub(crate) priority: String,
    pub(crate) status: String,
    pub(crate) category: String,
    pub(crate) date: String,
    pub(crate) author: String,
    /// The item's writeup, workspace-relative (the registry links it relative
    /// to `todo/`); verbatim when absolute, a URL, or escaping the workspace.
    pub(crate) file: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct OpenQuestion {
    pub(crate) text: String,
    /// The F-id that raised it.
    pub(crate) finding: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Counts {
    pub(crate) findings: u32,
    pub(crate) decisions: u32,
    pub(crate) learnings: u32,
    /// Todos not complete / wont-do, plus open questions.
    pub(crate) open: u32,
}

/// What [`read`] would read, by metadata: equal stamps mean a cached
/// [`Knowledge`] is still current.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stamp {
    /// `(workspace-relative path, mtime_ms, len)` per file.
    files: Vec<(String, u64, u64)>,
    /// Paths refused as symlinks: one appearing or vanishing changes the
    /// warnings even when no readable file did.
    refused: Vec<String>,
}

impl Stamp {
    /// mtime (ms) of one stamped file, by workspace-relative path — the
    /// Timeline attributes a new entry to a turn only when its file changed
    /// after that turn started.
    pub(crate) fn mtime_of(&self, rel: &str) -> Option<u64> {
        self.files
            .iter()
            .find(|(path, _, _)| path == rel)
            .map(|(_, mtime, _)| *mtime)
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Metadata of every file [`read`] would open — no contents read.
pub(crate) fn stamp(root: &Path) -> Stamp {
    let plan = plan(root);
    Stamp {
        files: plan
            .sources
            .iter()
            .map(|s| (s.rel.clone(), mtime_ms(&s.meta), s.meta.len()))
            .collect(),
        refused: plan.refused,
    }
}

/// Parse everything mycelium keeps for `root`. Outside a mycelium workspace
/// (see [`detect`]) nothing is read — a stray `todo/` is not knowledge.
pub(crate) fn read(root: &Path) -> Knowledge {
    let Plan {
        sources, mut notes, ..
    } = plan(root);
    let mut knowledge = Knowledge::default();
    let mut topics = Vec::new();
    let mut spent = 0u64;
    let mut over_budget = 0usize;
    // Topic sources arrive in slug order, so spending this budget as they
    // parse keeps exactly the first findings by (slug, id).
    let mut finding_budget = MAX_ENTRIES;
    let mut findings_seen = 0usize;
    let mut topics_unread = 0usize;
    for source in &sources {
        if matches!(source.kind, SourceKind::Topic { .. }) && finding_budget == 0 {
            topics_unread += 1;
            continue;
        }
        // An oversized file is read_source's to report, not the budget's.
        let len = source.meta.len();
        if len <= MAX_FILE_BYTES && spent + len > MAX_TOTAL_BYTES {
            over_budget += 1;
            continue;
        }
        let Some(text) = read_source(source, &mut notes) else {
            continue;
        };
        spent += text.len() as u64;
        let rel = source.rel.as_str();
        match &source.kind {
            SourceKind::Handoff { session_id, host } => {
                knowledge.left_off = parse_handoff(&text, source, &mut notes).map(|mut left| {
                    left.session_id.clone_from(session_id);
                    left.host.clone_from(host);
                    left
                });
            }
            SourceKind::Topic { slug } => {
                let (topic, found) = parse_topic(&text, rel, slug, finding_budget, &mut notes);
                findings_seen += found;
                if let Some(topic) = topic {
                    finding_budget -= topic.findings.len();
                    topics.push(topic);
                }
            }
            SourceKind::Decisions => knowledge.decisions = parse_decisions(&text, rel, &mut notes),
            SourceKind::Learnings => knowledge.learnings = parse_learnings(&text, rel, &mut notes),
            SourceKind::TodoRegistry => {
                knowledge.todos = parse_todos(&text, rel, false, &mut notes)
            }
            SourceKind::TodoLegacy => knowledge.todos = parse_todos(&text, rel, true, &mut notes),
        }
    }
    if over_budget > 0 {
        notes.push(format!(
            "{over_budget} knowledge files not read: the {} per-read budget was spent",
            mib(MAX_TOTAL_BYTES)
        ));
    }
    let shown: usize = topics.iter().map(|t| t.findings.len()).sum();
    if findings_seen > shown || topics_unread > 0 {
        let unread = if topics_unread > 0 {
            format!(" ({topics_unread} more topic files not read)")
        } else {
            String::new()
        };
        notes.push(format!(
            "findings: showing the first {shown} of {findings_seen}{unread}"
        ));
    }
    topics.sort_by(|a, b| a.slug.cmp(&b.slug));

    let mut seen = HashSet::new();
    let mut dropped = 0usize;
    for finding in topics.iter().flat_map(|t| &t.findings) {
        for question in &finding.questions {
            if !seen.insert(collapse_ws(&question.to_lowercase())) {
                continue;
            }
            if knowledge.questions.len() == MAX_ENTRIES {
                dropped += 1;
                continue;
            }
            knowledge.questions.push(OpenQuestion {
                text: question.clone(),
                finding: finding.id.clone(),
            });
        }
    }
    if dropped > 0 {
        notes.push(format!(
            "open questions: showing the first {MAX_ENTRIES} of {}",
            MAX_ENTRIES + dropped
        ));
    }

    let open_todos = knowledge
        .todos
        .iter()
        .filter(|t| !is_closed(&t.status))
        .count();
    knowledge.counts = Counts {
        findings: count(topics.iter().map(|t| t.findings.len()).sum()),
        decisions: count(knowledge.decisions.len()),
        learnings: count(knowledge.learnings.len()),
        open: count(open_todos + knowledge.questions.len()),
    };
    knowledge.topics = topics;
    knowledge.warnings = notes.finish();
    knowledge
}

/// A stable id for a decision or learning: FNV-1a 64 over the normalized
/// (lowercased, whitespace-collapsed) `kind|date|title`, as 12 hex chars
/// behind the kind's initial — `L-d407379e8c29`. Deliberately not std's
/// `DefaultHasher`, whose output may change between Rust releases: these ids
/// outlive the process (Timeline links, UI state).
pub(crate) fn fingerprint(kind: &str, date: &str, title: &str) -> String {
    let norm = |s: &str| collapse_ws(&s.to_lowercase());
    let key = format!("{}|{}|{}", norm(kind), norm(date), norm(title));
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let initial = kind
        .trim()
        .chars()
        .next()
        .map_or('X', |c| c.to_ascii_uppercase());
    format!("{initial}-{:012x}", hash >> 16)
}

// ---------------------------------------------------------------------------
// Discovery: which files a read would open
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum SourceKind {
    Handoff {
        session_id: Option<String>,
        host: Option<String>,
    },
    Topic {
        slug: String,
    },
    Decisions,
    Learnings,
    TodoRegistry,
    TodoLegacy,
}

#[derive(Debug)]
struct Source {
    kind: SourceKind,
    /// Workspace-relative, `/`-joined.
    rel: String,
    abs: PathBuf,
    /// From `symlink_metadata` — the file itself, never a link target.
    meta: Metadata,
}

/// The file set shared by [`stamp`] and [`read`], so the two can never
/// disagree about what a read covers.
#[derive(Default)]
struct Plan {
    sources: Vec<Source>,
    refused: Vec<String>,
    notes: Notes,
}

impl Plan {
    /// Whether `rel` is a real directory; a symlink is refused, not followed.
    fn dir(&mut self, root: &Path, rel: &str) -> bool {
        match std::fs::symlink_metadata(root.join(rel)) {
            Ok(meta) if meta.file_type().is_symlink() => {
                self.refuse(rel);
                false
            }
            Ok(meta) => meta.is_dir(),
            Err(_) => false,
        }
    }

    /// A regular file at `rel`, not yet queued.
    fn probe(&mut self, root: &Path, rel: &str, kind: SourceKind) -> Option<Source> {
        let abs = root.join(rel);
        let meta = std::fs::symlink_metadata(&abs).ok()?;
        if meta.file_type().is_symlink() {
            self.refuse(rel);
            return None;
        }
        meta.is_file().then(|| Source {
            kind,
            rel: rel.to_owned(),
            abs,
            meta,
        })
    }

    fn file(&mut self, root: &Path, rel: &str, kind: SourceKind) -> bool {
        let found = self.probe(root, rel, kind);
        let queued = found.is_some();
        self.sources.extend(found);
        queued
    }

    fn refuse(&mut self, rel: &str) {
        self.refused.push(rel.to_owned());
        self.notes.push(format!(
            "{rel} is a symlink; not followed (Knowledge reads only real files inside the workspace)"
        ));
    }

    /// Sorted UTF-8 names in `rel`, at most [`MAX_DIR_ENTRIES`] examined.
    fn list(&mut self, root: &Path, rel: &str) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(root.join(rel)) else {
            return Vec::new();
        };
        let mut entries = entries.flatten();
        let mut names: Vec<String> = entries
            .by_ref()
            .take(MAX_DIR_ENTRIES)
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        if entries.next().is_some() {
            self.notes.push(format!(
                "{rel}: over {MAX_DIR_ENTRIES} entries; only the first {MAX_DIR_ENTRIES} were considered"
            ));
        }
        names.sort();
        names
    }
}

fn plan(root: &Path) -> Plan {
    let mut plan = Plan::default();
    let living = plan.dir(root, LIVING);
    if !living && !is_regular_file(&root.join(PROTOCOL_FILE)) {
        return plan;
    }
    // Topic files go last: they are the many-file source, so they are what
    // a spent MAX_TOTAL_BYTES budget should drop.
    plan_handoff(root, &mut plan);
    if living {
        plan.file(root, ".living/decisions.md", SourceKind::Decisions);
        plan.file(root, ".living/learnings.md", SourceKind::Learnings);
    }
    if plan.dir(root, "todo") && !plan.file(root, "todo/TODO_REGISTRY.md", SourceKind::TodoRegistry)
    {
        plan.file(root, "todo/TODOLIST.md", SourceKind::TodoLegacy);
    }
    if living {
        plan_topics(root, &mut plan);
    }
    plan
}

/// The accepted shared handoff, else the newest in-flight run handoff
/// (`.mycelium/run/<host>/<session-id>/last-session.md`).
fn plan_handoff(root: &Path, plan: &mut Plan) {
    if !plan.dir(root, ".mycelium") {
        return;
    }
    let shared = SourceKind::Handoff {
        session_id: None,
        host: None,
    };
    if plan.file(root, ".mycelium/last-session.md", shared) || !plan.dir(root, ".mycelium/run") {
        return;
    }
    let mut best: Option<Source> = None;
    let mut examined = 0usize;
    'hosts: for host in plan.list(root, ".mycelium/run") {
        let host_rel = format!(".mycelium/run/{host}");
        if !is_run_component(&host) || !plan.dir(root, &host_rel) {
            continue;
        }
        for session in plan.list(root, &host_rel) {
            examined += 1;
            if examined > MAX_RUN_DIRS {
                plan.notes.push(format!(
                    ".mycelium/run: over {MAX_RUN_DIRS} session directories; the rest were not searched for a handoff"
                ));
                break 'hosts;
            }
            let dir_rel = format!("{host_rel}/{session}");
            if !is_run_component(&session) || !plan.dir(root, &dir_rel) {
                continue;
            }
            let kind = SourceKind::Handoff {
                session_id: Some(session.clone()),
                host: Some(host.clone()),
            };
            let Some(found) = plan.probe(root, &format!("{dir_rel}/last-session.md"), kind) else {
                continue;
            };
            let newer = best
                .as_ref()
                .is_none_or(|b| (mtime_ms(&found.meta), &found.rel) > (mtime_ms(&b.meta), &b.rel));
            if newer {
                best = Some(found);
            }
        }
    }
    plan.sources.extend(best);
}

fn plan_topics(root: &Path, plan: &mut Plan) {
    const DIR: &str = ".living/findings";
    if !plan.dir(root, DIR) {
        return;
    }
    let mut names: Vec<String> = plan
        .list(root, DIR)
        .into_iter()
        .filter(|n| {
            let lower = n.to_ascii_lowercase();
            lower.ends_with(".md")
                && !n.starts_with('.')
                && lower != "index.md"
                && lower != "findings_registry.md"
        })
        .collect();
    // By slug, not file name ("a-b.md" sorts before "a.md"), so the finding
    // budget in `read` is spent in the order the view shows.
    names.sort_by(|a, b| a[..a.len() - 3].cmp(&b[..b.len() - 3]));
    if names.len() > MAX_TOPIC_FILES {
        plan.notes.push(format!(
            "{DIR}: {} topic files; read the first {MAX_TOPIC_FILES} by name",
            names.len()
        ));
        names.truncate(MAX_TOPIC_FILES);
    }
    for name in names {
        let slug = name[..name.len() - 3].to_owned();
        plan.file(root, &format!("{DIR}/{name}"), SourceKind::Topic { slug });
    }
}

/// mycelium's own session-id rule (`[A-Za-z0-9._-]+`, ≤200, not `.`/`..`);
/// anything else in `run/` isn't a run directory mycelium made.
fn is_run_component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

fn mtime_ms(meta: &Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Bounded read of a planned file. Refuses anything that stopped being the
/// regular file the plan saw — a swap to a symlink between the `lstat` and
/// the `open` would otherwise be followed.
fn read_source(source: &Source, notes: &mut Notes) -> Option<String> {
    let rel = &source.rel;
    if source.meta.len() > MAX_FILE_BYTES {
        notes.push(format!(
            "{rel}: skipped; {} is over the {} read cap",
            mib(source.meta.len()),
            mib(MAX_FILE_BYTES)
        ));
        return None;
    }
    let file = match File::open(&source.abs) {
        Ok(file) => file,
        Err(err) => {
            notes.push(format!("{rel}: unreadable ({err})"));
            return None;
        }
    };
    if !file
        .metadata()
        .is_ok_and(|m| m.is_file() && same_file(&m, &source.meta))
    {
        notes.push(format!("{rel}: changed while being read; skipped"));
        return None;
    }
    let mut buf = Vec::with_capacity(usize::try_from(source.meta.len()).unwrap_or(0));
    if let Err(err) = file.take(MAX_FILE_BYTES + 1).read_to_end(&mut buf) {
        notes.push(format!("{rel}: unreadable ({err})"));
        return None;
    }
    if buf.len() as u64 > MAX_FILE_BYTES {
        notes.push(format!(
            "{rel}: skipped; grew past the {} read cap",
            mib(MAX_FILE_BYTES)
        ));
        return None;
    }
    let mut text = match String::from_utf8(buf) {
        Ok(text) => text,
        Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
    };
    if text.starts_with('\u{feff}') {
        text.drain(..'\u{feff}'.len_utf8());
    }
    Some(text)
}

#[cfg(unix)]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    (a.dev(), a.ino()) == (b.dev(), b.ino())
}

#[cfg(not(unix))]
fn same_file(_a: &Metadata, _b: &Metadata) -> bool {
    true
}

/// Rounded up, so a file a few bytes past the cap never reads as "2.0 MiB".
fn mib(bytes: u64) -> String {
    let tenths = (bytes as f64 / (1024.0 * 1024.0) * 10.0).ceil() / 10.0;
    if tenths.fract() == 0.0 {
        format!("{tenths:.0} MiB")
    } else {
        format!("{tenths:.1} MiB")
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Warnings, capped so a pathological tree can't grow the list without bound.
#[derive(Default)]
struct Notes {
    list: Vec<String>,
    dropped: usize,
}

impl Notes {
    fn push(&mut self, note: String) {
        if self.list.len() < MAX_WARNINGS {
            self.list.push(cap_text(note));
        } else {
            self.dropped += 1;
        }
    }

    fn finish(mut self) -> Vec<String> {
        if self.dropped > 0 {
            self.list
                .push(format!("…and {} more warnings", self.dropped));
        }
        self.list
    }
}

// ---------------------------------------------------------------------------
// Markdown line model: fences + HTML comments
// ---------------------------------------------------------------------------

/// How a line reads once code fences and HTML comments are accounted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    Text,
    /// Inside (or opening/closing) a fenced code block.
    Code,
    /// Inside a multi-line HTML comment, or YAML frontmatter.
    Comment,
}

struct Doc<'a> {
    lines: Vec<&'a str>,
    marks: Vec<Mark>,
    /// 0-based line of a fence left open at end of input.
    unclosed_fence: Option<usize>,
    /// 0-based line of an HTML comment left open at end of input.
    unclosed_comment: Option<usize>,
}

impl<'a> Doc<'a> {
    fn new(text: &'a str) -> Self {
        Self::with_prefix_hidden(text, 0)
    }

    /// Lines before `hidden` (frontmatter) are marked [`Mark::Comment`].
    fn with_prefix_hidden(text: &'a str, hidden: usize) -> Self {
        // Split on '\n' only, like mycelium's `split_log_lines`: line numbers
        // must agree with the files an agent (or "open in file") sees.
        let lines: Vec<&str> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect();
        let mut marks = Vec::with_capacity(lines.len());
        let mut fence: Option<Fence> = None;
        let mut unclosed_fence = None;
        let mut unclosed_comment = None;
        for (i, line) in lines.iter().enumerate() {
            if i < hidden {
                marks.push(Mark::Comment);
                continue;
            }
            if let Some(open) = &fence {
                marks.push(Mark::Code);
                if closes(line, open) {
                    fence = None;
                    unclosed_fence = None;
                }
                continue;
            }
            if unclosed_comment.is_some() {
                marks.push(Mark::Comment);
                if line.contains("-->") {
                    unclosed_comment = None;
                }
                continue;
            }
            if let Some(open) = opening_fence(line) {
                fence = Some(open);
                unclosed_fence = Some(i);
                marks.push(Mark::Code);
                continue;
            }
            // An odd count of backticks before it puts the `<!--` inside a
            // code span: a learning *about* HTML comments must not swallow
            // every entry after it.
            let opens_comment = line.rfind("<!--").is_some_and(|at| {
                !line[at + 4..].contains("-->") && line[..at].matches('`').count() % 2 == 0
            });
            if opens_comment {
                unclosed_comment = Some(i);
                // A line that is only the comment's start carries no content;
                // text before a mid-line opener still does.
                if line.trim_start().starts_with("<!--") {
                    marks.push(Mark::Comment);
                    continue;
                }
            }
            marks.push(Mark::Text);
        }
        Self {
            lines,
            marks,
            unclosed_fence,
            unclosed_comment,
        }
    }

    /// Frontmatter (`---` … `---` opening the file) as `key: value` pairs,
    /// with those lines hidden from the markdown scan: a YAML `# comment`
    /// must not read as a heading.
    fn with_frontmatter(text: &'a str) -> (Self, Vec<(String, String)>) {
        let (pairs, hidden) = frontmatter(text);
        (Self::with_prefix_hidden(text, hidden), pairs)
    }

    fn text(&self, i: usize) -> Option<&'a str> {
        (self.marks.get(i) == Some(&Mark::Text)).then(|| self.lines[i])
    }

    /// Whatever followed an unclosed fence or comment was never examined:
    /// say so rather than present a silently short list.
    fn note_unclosed(&self, rel: &str, notes: &mut Notes) {
        let unclosed = [
            ("code fence", self.unclosed_fence),
            ("HTML comment", self.unclosed_comment),
        ];
        for (what, at) in unclosed {
            if let Some(at) = at {
                notes.push(format!(
                    "{rel}: unclosed {what} at line {}; nothing after it was read as knowledge",
                    at + 1
                ));
            }
        }
    }
}

/// An open fenced code block (mycelium's `markdown_fences.Fence`).
struct Fence {
    marker: u8,
    len: usize,
    /// Visual column of the marker; bounds a matching closer.
    column: usize,
    /// Blockquote depth of the opener; a closer must match it exactly.
    quotes: usize,
}

/// The fence `line` opens: up to three spaces, then any run of list-item or
/// blockquote markers, then ≥3 backticks or tildes — a port of
/// `markdown_fences._FENCE_OPEN_RE`. A backtick fence's info string may not
/// contain a backtick (CommonMark 4.5).
fn opening_fence(line: &str) -> Option<Fence> {
    let b = line.as_bytes();
    let mut i = b.iter().take(3).take_while(|&&c| c == b' ').count();
    let spaces_after = |at: usize| {
        b.get(at..).map_or(0, |rest| {
            rest.iter()
                .take_while(|&&c| matches!(c, b' ' | b'\t'))
                .count()
        })
    };
    loop {
        match b.get(i) {
            Some(b'-' | b'+' | b'*') => {
                let ws = spaces_after(i + 1);
                if ws == 0 {
                    return None;
                }
                i += 1 + ws;
            }
            Some(b'>') => {
                i += 1;
                if matches!(b.get(i), Some(b' ' | b'\t')) {
                    i += 1;
                }
            }
            Some(c) if c.is_ascii_digit() => {
                let digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
                if digits > 9 || !matches!(b.get(i + digits), Some(b'.' | b')')) {
                    return None;
                }
                let ws = spaces_after(i + digits + 1);
                if ws == 0 {
                    return None;
                }
                i += digits + 1 + ws;
            }
            _ => break,
        }
    }
    let marker = *b.get(i)?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let len = b[i..].iter().take_while(|&&c| c == marker).count();
    if len < 3 || (marker == b'`' && line[i + len..].contains('`')) {
        return None;
    }
    let prefix = &line[..i];
    Some(Fence {
        marker,
        len,
        column: visual_column(prefix),
        quotes: prefix.matches('>').count(),
    })
}

/// Whether `line` closes `fence` (`markdown_fences.closes`): same character,
/// a run at least as long, nothing after it, the same blockquote depth, and
/// at most three columns past the opener.
fn closes(line: &str, fence: &Fence) -> bool {
    let b = line.as_bytes();
    let i = b
        .iter()
        .take_while(|&&c| matches!(c, b' ' | b'\t' | b'>'))
        .count();
    let len = b[i..].iter().take_while(|&&c| c == fence.marker).count();
    let prefix = &line[..i];
    len >= 3
        && len >= fence.len
        && line[i + len..].trim().is_empty()
        && prefix.matches('>').count() == fence.quotes
        && visual_column(prefix) <= fence.column + 3
}

/// Column where `prefix` ends, tabs expanded to Markdown's 4-column stops.
fn visual_column(prefix: &str) -> usize {
    prefix.chars().fold(0, |col, c| {
        if c == '\t' {
            col + 4 - col % 4
        } else {
            col + 1
        }
    })
}

/// A column-1 ATX heading as `(level, text)`. mycelium's parsers match
/// literal column-1 prefixes, so an indented heading is content here too.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim();
    // An optional closing run of '#' (CommonMark), but "C#" keeps its '#'.
    let unclosed = text.trim_end_matches('#');
    if unclosed.len() != text.len() && (unclosed.is_empty() || unclosed.ends_with([' ', '\t'])) {
        return Some((level, unclosed.trim_end()));
    }
    Some((level, text))
}

fn is_thematic_break(line: &str) -> bool {
    if line.len() - line.trim_start().len() > 3 {
        return false;
    }
    let mut chars = line.chars().filter(|c| !c.is_whitespace());
    let Some(first) = chars.next() else {
        return false;
    };
    let mut n = 1;
    for c in chars {
        if c != first {
            return false;
        }
        n += 1;
    }
    matches!(first, '-' | '*' | '_') && n >= 3
}

/// Leading `---` block: `key: value` pairs and the line count it spans (0
/// when there is none). Only flat scalars and folded/literal blocks — the
/// shapes mycelium's topic template writes; no YAML crate for five keys.
fn frontmatter(text: &str) -> (Vec<(String, String)>, usize) {
    let lines: Vec<&str> = text.split('\n').take(200).collect();
    let Some(start) = lines.iter().position(|l| !l.trim().is_empty()) else {
        return (Vec::new(), 0);
    };
    if lines[start].trim() != "---" {
        return (Vec::new(), 0);
    }
    let Some(end) = lines[start + 1..]
        .iter()
        .position(|l| matches!(l.trim(), "---" | "..."))
        .map(|at| start + 1 + at)
    else {
        return (Vec::new(), 0);
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut block: Option<usize> = None;
    for &line in &lines[start + 1..end] {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(at) = block {
            if line.starts_with([' ', '\t']) {
                let value = &mut pairs[at].1;
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(line.trim());
                continue;
            }
            block = None;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty()
            || line.starts_with([' ', '\t', '#'])
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            continue;
        }
        let value = value.trim();
        if matches!(value, "" | ">" | "|" | ">-" | "|-" | ">+" | "|+") {
            block = Some(pairs.len());
            pairs.push((key.to_ascii_lowercase(), String::new()));
            continue;
        }
        pairs.push((key.to_ascii_lowercase(), unquote(value).to_owned()));
    }
    (pairs, end + 1)
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

/// Cut to [`MAX_TEXT_BYTES`] (the "…" included) on a char boundary.
fn cap_text(mut s: String) -> String {
    if s.len() <= MAX_TEXT_BYTES {
        return s;
    }
    let mut end = MAX_TEXT_BYTES - '…'.len_utf8();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push('…');
    s
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Joined lines, trimmed, with each run of blank lines collapsed to one.
fn tidy<S: AsRef<str>>(lines: &[S]) -> String {
    let mut out = String::new();
    let mut gap = false;
    for line in lines {
        let line = line.as_ref().trim_end();
        if line.trim().is_empty() {
            gap = !out.is_empty();
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if gap {
                out.push('\n');
            }
        }
        gap = false;
        out.push_str(line);
    }
    out.trim_start().to_owned()
}

fn strip_inline_comments(line: &str) -> Cow<'_, str> {
    if !line.contains("<!--") {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        match rest[open + 4..].find("-->") {
            Some(close) => rest = &rest[open + 4 + close + 3..],
            // Continues on later lines, which the Doc marks as Comment.
            None => rest = "",
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// `[text](target)` → `text` (images too), plus the first link's target.
fn strip_links(s: &str) -> (String, Option<String>) {
    let mut out = String::with_capacity(s.len());
    let mut target = None;
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find("](").map(|at| open + at) else {
            break;
        };
        let Some(end) = rest[close + 2..].find(')').map(|at| close + 2 + at) else {
            break;
        };
        let before = &rest[..open];
        out.push_str(before.strip_suffix('!').unwrap_or(before));
        out.push_str(&rest[open + 1..close]);
        if target.is_none() {
            // `(path "title")`: the path is the first token.
            let link = rest[close + 2..end].split_whitespace().next().unwrap_or("");
            target = Some(link.trim_matches(['<', '>']).to_owned());
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    (out, target)
}

fn unbold(s: &str) -> &str {
    let t = s.trim();
    match t.strip_prefix("**").and_then(|t| t.strip_suffix("**")) {
        Some(inner) if !inner.trim().is_empty() => inner.trim(),
        _ => t,
    }
}

fn is_iso_date(b: &[u8]) -> bool {
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// A heading's leading date and title — a port of mycelium's
/// `split_entry_date_and_title`. Only a LEADING `[YYYY-MM-DD]` (or bare date)
/// is metadata; a date later in the title belongs to the title.
fn split_date_title(heading_text: &str) -> (&str, &str) {
    let text = unbold(heading_text);
    let (date, rest) = leading_date(text);
    let Some(date) = date else {
        return ("", text);
    };
    let title =
        rest.trim_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '-' | '–' | '—'));
    (date, if title.is_empty() { text } else { title })
}

fn leading_date(text: &str) -> (Option<&str>, &str) {
    let s = text.trim_start();
    let s = s.strip_prefix('[').unwrap_or(s);
    match s.as_bytes().get(..10) {
        Some(head) if is_iso_date(head) => {
            let rest = &s[10..];
            (Some(&s[..10]), rest.strip_prefix(']').unwrap_or(rest))
        }
        _ => (None, text),
    }
}

/// Filler that says nothing: empty/"None"/"TBD" answers, a template's
/// `[prompt]` or `{prompt}`, and the lines mycelium writes as scaffolding —
/// the five-section template's prompts and the Stop hook's deterministic
/// fallback handoff (`mycelium-stop-check.sh`).
fn is_placeholder(s: &str) -> bool {
    const EMPTY: &[&str] = &[
        "none",
        "none yet",
        "none so far",
        "no blockers",
        "nothing",
        "nothing yet",
        "n/a",
        "na",
        "tbd",
        "todo",
        "-",
        "—",
        "–",
        "…",
        "yyyy-mm-dd",
    ];
    const STUBS: &[&str] = &[
        "[decision]:",
        "[resolved/unresolved]:",
        "branch: x | tests: n passing",
        "completed the session work recorded in the finalized session log",
        "see `.living/decisions.md` for decisions recorded during this session",
        "see the finalized session log and `.living/learnings.md` for recorded issues",
        "review the finalized session log and continue from the current branch state",
    ];
    let t = s.trim().trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '_' | '*' | '`' | '(' | ')' | '.' | '!')
    });
    if t.is_empty()
        || (t.starts_with('[') && t.ends_with(']') && !t.contains("]("))
        || (t.starts_with('{') && t.ends_with('}'))
    {
        return true;
    }
    let lower = t.to_lowercase();
    EMPTY.contains(&lower.as_str()) || STUBS.iter().any(|stub| lower.starts_with(stub))
}

fn first_word(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .find(|w| !w.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn normalize_status(value: &str) -> String {
    let word = first_word(value);
    match word.as_str() {
        "preliminary" | "supported" | "robust" | "contradicted" => word,
        _ => "unknown".to_owned(),
    }
}

fn normalize_category(value: &str) -> String {
    // An unfilled template choice list ("[gotcha|edge-case|…]") is no answer.
    if value.contains('|') {
        return "other".to_owned();
    }
    let category = match first_word(value).as_str() {
        "gotcha" | "gotchas" => "gotcha",
        "edge-case" | "edge-cases" | "edge" | "edgecase" => "edge-case",
        "insight" | "insights" => "insight",
        "failure" | "failures" | "fail" | "failed" => "failure",
        "tip" | "tips" => "tip",
        _ => "other",
    };
    category.to_owned()
}

fn normalize_direction(value: &str) -> String {
    let lower = value.to_lowercase();
    let direction = lower
        .split(|c: char| !c.is_alphanumeric())
        .find_map(|w| {
            if w.starts_with("support") {
                Some("supports")
            } else if w.starts_with("contradict") {
                Some("contradicts")
            } else if w.starts_with("refin") {
                Some("refines")
            } else {
                None
            }
        })
        .unwrap_or("unknown");
    direction.to_owned()
}

/// mycelium's closed todo statuses (`complete`, `wont-do`) and their obvious
/// spellings.
fn is_closed(status: &str) -> bool {
    let norm: String = status
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '\'' | '’' | '`'))
        .map(|c| if c == ' ' || c == '_' { '-' } else { c })
        .collect();
    matches!(
        norm.trim_matches('-'),
        "complete"
            | "completed"
            | "done"
            | "closed"
            | "wont-do"
            | "wontdo"
            | "wont-fix"
            | "wontfix"
            | "cancelled"
            | "canceled"
            | "dropped"
    )
}

/// Tags as `[a, b]`, `a, b`, or `#a #b`; deduplicated case-insensitively.
fn parse_tags(raw: &str, tags: &mut Vec<String>) {
    let raw = raw.trim();
    if raw.starts_with('{') && raw.ends_with('}') {
        return;
    }
    let raw = raw
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .unwrap_or(raw);
    for piece in raw.split(',') {
        let piece = piece.trim();
        let words: Vec<&str> = piece.split_whitespace().collect();
        if words.len() > 1 && words.iter().all(|w| w.starts_with('#')) {
            for word in words {
                push_tag(word, tags);
            }
        } else {
            push_tag(piece, tags);
        }
    }
}

fn push_tag(raw: &str, tags: &mut Vec<String>) {
    let tag = raw
        .trim()
        .trim_matches(['[', ']', '`', '"', '\''])
        .trim_start_matches('#')
        .trim();
    if tag.is_empty() || tags.len() >= MAX_TAGS || tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    {
        return;
    }
    tags.push(cap_text(tag.to_owned()));
}

// ---------------------------------------------------------------------------
// Lists and tables
// ---------------------------------------------------------------------------

struct ListItem {
    text: String,
    /// A checked task box or a fully struck-through item: resolved.
    done: bool,
}

/// A list marker (`-`, `*`, `+`, `1.`, `1)`) and the text after it.
fn list_marker(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let after = match *b.first()? {
        b'-' | b'*' | b'+' => 1,
        c if c.is_ascii_digit() => {
            let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
            if digits > 9 || !matches!(b.get(digits), Some(b'.' | b')')) {
                return None;
            }
            digits + 1
        }
        _ => return None,
    };
    match b.get(after) {
        None => Some(""),
        Some(b' ' | b'\t') => Some(s[after..].trim_start()),
        _ => None,
    }
}

/// Top-level list items with their continuation lines (indented or lazy)
/// and nested items folded in.
fn list_items<'s>(lines: impl IntoIterator<Item = &'s str>) -> Vec<ListItem> {
    let mut items: Vec<ListItem> = Vec::new();
    let mut base: Option<usize> = None;
    let mut open = false;
    let mut gap = false;
    for line in lines {
        let body = line.trim_start();
        if body.is_empty() {
            gap = true;
            continue;
        }
        let indent = line.len() - body.len();
        let marker = if is_thematic_break(line) {
            None
        } else {
            list_marker(body)
        };
        if let Some(rest) = marker {
            let base = *base.get_or_insert(indent);
            if indent <= base + 1 {
                items.push(ListItem {
                    text: rest.to_owned(),
                    done: false,
                });
                open = true;
                gap = false;
                continue;
            }
        }
        match items.last_mut() {
            Some(last) if open && (indent > 0 || !gap) => {
                last.text.push(' ');
                last.text.push_str(marker.unwrap_or(body));
                gap = false;
            }
            _ => open = false,
        }
    }
    for item in &mut items {
        let text = collapse_ws(&item.text);
        let (text, done) = match text
            .strip_prefix("[x] ")
            .or_else(|| text.strip_prefix("[X] "))
        {
            Some(rest) => (rest.to_owned(), true),
            None => match text.strip_prefix("[ ] ") {
                Some(rest) => (rest.to_owned(), false),
                None => {
                    let struck = text.len() > 4 && text.starts_with("~~") && text.ends_with("~~");
                    (text, struck)
                }
            },
        };
        item.text = text;
        item.done = done;
    }
    items
}

/// Kept (not done, not filler) item texts, capped.
fn live_items(items: Vec<ListItem>, cap: usize) -> Vec<String> {
    items
        .into_iter()
        .filter(|item| !item.done && !is_placeholder(&item.text))
        .take(cap)
        .map(|item| cap_text(item.text))
        .collect()
}

/// A `|`-delimited table row's cells (`\|` escapes a pipe); the trailing
/// pipe is optional.
fn table_cells(line: &str) -> Option<Vec<String>> {
    let inner = line.trim().strip_prefix('|')?;
    let inner = match inner.strip_suffix('|') {
        Some(stripped) if !stripped.ends_with('\\') => stripped,
        _ => inner,
    };
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => cells.push(std::mem::take(&mut cell).trim().to_owned()),
            _ => cell.push(c),
        }
    }
    cells.push(cell.trim().to_owned());
    Some(cells)
}

fn is_separator(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().any(|c| c.contains('-'))
        && cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')))
}

/// A table cell as plain text: links unwrapped, backticks and a lone
/// dash/em-dash ("nothing here") dropped.
fn cell_text(cell: &str) -> String {
    let (text, _) = strip_links(cell);
    let text = text.trim().trim_matches('`').trim();
    if matches!(text, "-" | "—" | "–") {
        String::new()
    } else {
        cap_text(text.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Bold fields
// ---------------------------------------------------------------------------

const DECISION_FIELDS: &[&str] = &[
    "context",
    "decision",
    "alternatives considered",
    "rationale",
    "consequences",
    "tags",
];
/// Includes the fields mycelium's learning template adds that we don't show
/// (`mitigation_type`, …) — as known labels they end the field before them
/// even in their plain `label: value` form.
const LEARNING_FIELDS: &[&str] = &[
    "category",
    "what happened",
    "why it matters",
    "resolution",
    "tags",
    "mitigation type",
    "structural mitigation candidate",
    "source",
];
const FINDING_FIELDS: &[&str] = &["status", "claim", "implications", "tags"];

fn field_label(raw: &str) -> String {
    collapse_ws(&raw.replace('_', " ").to_lowercase())
}

/// A field-opening line: `**Label**: value` or `**Label:** value` with any
/// label, plus — for the kind's known labels only — `- **Label**: value` and
/// plain `Label: value` (mycelium's own tag reader accepts `Tags: x`). A
/// blockquote prefix is tolerated, as mycelium's is.
fn field_line<'l>(line: &'l str, known: &[&str]) -> Option<(String, &'l str)> {
    let s = line.trim_start_matches(|c: char| c == '>' || c.is_whitespace());
    let (s, bulleted) = match s.strip_prefix(['-', '*', '+']) {
        Some(rest) if rest.starts_with([' ', '\t']) => (rest.trim_start(), true),
        _ => (s, false),
    };
    if let Some(inner) = s.strip_prefix("**") {
        let close = inner.find("**")?;
        let raw = &inner[..close];
        let after = &inner[close + 2..];
        let (label, value) = match raw.strip_suffix(':') {
            Some(label) => (label, after),
            None => (raw, after.trim_start().strip_prefix(':')?),
        };
        let label = label.trim();
        if label.is_empty() || label.len() > 48 || label.contains('*') {
            return None;
        }
        let label = field_label(label);
        if bulleted && !known.contains(&label.as_str()) {
            return None;
        }
        return Some((label, value.trim()));
    }
    if bulleted {
        return None;
    }
    let (label, value) = s.split_once(':')?;
    let label = field_label(label);
    known
        .contains(&label.as_str())
        .then(|| (label, value.trim()))
}

/// An entry's bold fields, each with its value lines: the text after the
/// label plus every following line up to the next field, heading, or
/// thematic break. Fenced lines stay in the value (a snippet under "What
/// happened"); commented lines don't.
struct Fields<'a> {
    list: Vec<(String, Vec<Cow<'a, str>>)>,
}

impl<'a> Fields<'a> {
    fn parse(doc: &Doc<'a>, start: usize, end: usize, known: &[&str]) -> Self {
        let mut list: Vec<(String, Vec<Cow<'a, str>>)> = Vec::new();
        let mut current = false;
        for i in start..end.min(doc.lines.len()) {
            let line = doc.lines[i];
            match doc.marks[i] {
                Mark::Comment => continue,
                Mark::Code => {
                    if let (true, Some(field)) = (current, list.last_mut()) {
                        field.1.push(Cow::Borrowed(line));
                    }
                    continue;
                }
                Mark::Text => {}
            }
            if heading(line).is_some() || is_thematic_break(line) {
                current = false;
                continue;
            }
            if let Some((label, value)) = field_line(line, known) {
                list.push((label, vec![strip_inline_comments(value)]));
                current = true;
                continue;
            }
            if let (true, Some(field)) = (current, list.last_mut()) {
                let stripped = strip_inline_comments(line);
                if stripped.trim().is_empty() && !line.trim().is_empty() {
                    continue;
                }
                field.1.push(stripped);
            }
        }
        Self { list }
    }

    /// The first field, in file order, named any of `names` (a label and its
    /// aliases). A repeated field keeps its first value, as mycelium's tag
    /// reader does.
    fn get(&self, names: &[&str]) -> Option<&[Cow<'a, str>]> {
        self.list
            .iter()
            .find(|(label, _)| names.contains(&label.as_str()))
            .map(|(_, lines)| lines.as_slice())
    }

    fn text(&self, names: &[&str]) -> String {
        self.get(names)
            .map_or_else(String::new, |lines| cap_text(tidy(lines)))
    }

    /// A list-valued field (Alternatives considered): an inline value, the
    /// `- ` items below it, or failing both its paragraphs.
    fn items(&self, names: &[&str]) -> Vec<String> {
        let Some((first, rest)) = self.get(names).and_then(|lines| lines.split_first()) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        let inline = first.trim();
        if !inline.is_empty() {
            items.push(ListItem {
                text: inline.to_owned(),
                done: false,
            });
        }
        let listed = list_items(rest.iter().map(|l| l.as_ref()));
        if listed.is_empty() && inline.is_empty() {
            items.extend(
                paragraphs(rest)
                    .into_iter()
                    .map(|text| ListItem { text, done: false }),
            );
        }
        items.extend(listed);
        live_items(items, MAX_LIST_ITEMS)
    }

    fn tags(&self) -> Vec<String> {
        let mut tags = Vec::new();
        let Some((first, rest)) = self.get(&["tags"]).and_then(|lines| lines.split_first()) else {
            return tags;
        };
        if first.trim().is_empty() {
            // `**Tags**:` over a bullet list.
            for item in list_items(rest.iter().map(|l| l.as_ref())) {
                parse_tags(&item.text, &mut tags);
            }
        } else {
            // Only the label's own line: whatever follows (a `source:` note,
            // a stray paragraph) is not tags.
            parse_tags(first, &mut tags);
        }
        tags
    }
}

fn paragraphs<S: AsRef<str>>(lines: &[S]) -> Vec<String> {
    tidy(lines)
        .split("\n\n")
        .map(collapse_ws)
        .filter(|p| !p.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Decisions and learnings
// ---------------------------------------------------------------------------

struct EntrySpan<'a> {
    /// 0-based heading line.
    start: usize,
    /// Exclusive.
    end: usize,
    date: &'a str,
    title: &'a str,
}

struct Spans<'a> {
    /// The last [`MAX_SCANNED_ENTRIES`], in file order.
    kept: VecDeque<EntrySpan<'a>>,
    total: usize,
    mislevelled: usize,
}

impl<'a> Spans<'a> {
    fn push(&mut self, span: EntrySpan<'a>) {
        self.total += 1;
        if self.kept.len() == MAX_SCANNED_ENTRIES {
            self.kept.pop_front();
        }
        self.kept.push_back(span);
    }
}

/// Entry spans of a decisions/learnings log. `### ` at column 1 opens an
/// entry, as in every mycelium parser; a DATED `## ` heading is a
/// mislevelled legacy entry (pre-0.6 decision template) — accepted and
/// counted, exactly the headings mycelium's validator flags and its
/// migration raises. Any other `#`/`##` heading is structure: it closes the
/// open entry and is not one.
fn entry_spans<'a>(doc: &Doc<'a>) -> Spans<'a> {
    let mut spans = Spans {
        kept: VecDeque::new(),
        total: 0,
        mislevelled: 0,
    };
    let mut open: Option<EntrySpan> = None;
    for i in 0..doc.lines.len() {
        let Some((level, text)) = doc.text(i).and_then(heading) else {
            continue;
        };
        let entry = match level {
            3 => true,
            2 if leading_date(text).0.is_some() => {
                spans.mislevelled += 1;
                true
            }
            1 | 2 => false,
            _ => continue,
        };
        if let Some(mut span) = open.take() {
            span.end = i;
            spans.push(span);
        }
        if entry {
            let (date, title) = split_date_title(text);
            open = Some(EntrySpan {
                start: i,
                end: 0,
                date,
                title,
            });
        }
    }
    if let Some(mut span) = open {
        span.end = doc.lines.len();
        spans.push(span);
    }
    spans
}

/// Fingerprinted spans, newest first (undated last, then later-in-file
/// first — append order is the only recency signal an undated entry has),
/// capped to the newest [`MAX_ENTRIES`].
fn select_entries<'a>(
    doc: &Doc<'a>,
    rel: &str,
    kind: &str,
    notes: &mut Notes,
) -> Vec<(EntrySpan<'a>, String)> {
    let Spans {
        kept: spans,
        total,
        mislevelled,
    } = entry_spans(doc);
    if total > spans.len() {
        notes.push(format!(
            "{rel}: {total} entry headings; only the last {} were considered",
            spans.len()
        ));
    }
    if mislevelled > 0 {
        let verb = if mislevelled == 1 {
            "entry uses"
        } else {
            "entries use"
        };
        notes.push(format!(
            "{rel}: {mislevelled} {verb} ## headings (mycelium 0.7 expects ###)"
        ));
    }
    // A verbatim duplicate (same date and title) gets `~N` in file order, so
    // an id stays unique within a read — the UI keys lists by it.
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut entries: Vec<(usize, EntrySpan, String)> = spans
        .into_iter()
        .enumerate()
        .map(|(order, span)| {
            let base = fingerprint(kind, span.date, span.title);
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            let fp = if *n == 1 { base } else { format!("{base}~{n}") };
            (order, span, fp)
        })
        .collect();
    entries.sort_by(|a, b| b.1.date.cmp(a.1.date).then(b.0.cmp(&a.0)));
    if entries.len() > MAX_ENTRIES {
        notes.push(format!(
            "{rel}: showing the newest {MAX_ENTRIES} of {} entries",
            entries.len()
        ));
        entries.truncate(MAX_ENTRIES);
    }
    entries
        .into_iter()
        .map(|(_, span, fp)| (span, fp))
        .collect()
}

fn line_no(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

fn parse_decisions(text: &str, rel: &str, notes: &mut Notes) -> Vec<Decision> {
    let doc = Doc::new(text);
    doc.note_unclosed(rel, notes);
    select_entries(&doc, rel, "decision", notes)
        .into_iter()
        .map(|(span, fp)| {
            let fields = Fields::parse(&doc, span.start + 1, span.end, DECISION_FIELDS);
            Decision {
                fp,
                date: span.date.to_owned(),
                title: cap_text(span.title.to_owned()),
                context: fields.text(&["context"]),
                decision: fields.text(&["decision", "decision made"]),
                alternatives: fields.items(&[
                    "alternatives considered",
                    "alternatives",
                    "options considered",
                ]),
                rationale: fields.text(&["rationale"]),
                consequences: fields.text(&["consequences"]),
                tags: fields.tags(),
                line: line_no(span.start),
            }
        })
        .collect()
}

fn parse_learnings(text: &str, rel: &str, notes: &mut Notes) -> Vec<Learning> {
    let doc = Doc::new(text);
    doc.note_unclosed(rel, notes);
    select_entries(&doc, rel, "learning", notes)
        .into_iter()
        .map(|(span, fp)| {
            let fields = Fields::parse(&doc, span.start + 1, span.end, LEARNING_FIELDS);
            let category = fields
                .get(&["category"])
                .and_then(|lines| lines.first())
                .map_or_else(|| "other".to_owned(), |v| normalize_category(v));
            Learning {
                fp,
                date: span.date.to_owned(),
                title: cap_text(span.title.to_owned()),
                category,
                what: fields.text(&["what happened", "what"]),
                why: fields.text(&["why it matters", "why"]),
                resolution: fields.text(&["resolution", "fix"]),
                tags: fields.tags(),
                line: line_no(span.start),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

/// `F-NNN` at the start of a heading: `(id, numeric id, claim text)`.
fn finding_id(text: &str) -> Option<(String, u64, &str)> {
    let t = text.trim().trim_start_matches('*');
    let rest = t.strip_prefix("F-")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let is_sep =
        |c: char| c.is_whitespace() || matches!(c, ':' | '.' | '-' | '–' | '—' | ')' | '*');
    let after = &rest[digits..];
    if !(after.is_empty() || after.starts_with(is_sep)) {
        return None;
    }
    let num = rest[..digits].parse().unwrap_or(u64::MAX);
    let claim = after
        .trim_start_matches(is_sep)
        .trim_end_matches('*')
        .trim();
    Some((format!("F-{}", &rest[..digits]), num, claim))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Subsection {
    Other,
    Ledger,
    Questions,
}

fn subsection(text: &str) -> Subsection {
    let lower = text.to_lowercase();
    if lower.starts_with("evidence") {
        Subsection::Ledger
    } else if lower.starts_with("open question") {
        Subsection::Questions
    } else {
        Subsection::Other
    }
}

/// A topic file's findings — at most `budget` of them, the lowest ids — and
/// how many `F-` headings the file has in all (for the cap warning). `None`
/// for a file with no findings (or none left in the budget).
fn parse_topic(
    text: &str,
    rel: &str,
    slug: &str,
    budget: usize,
    notes: &mut Notes,
) -> (Option<Topic>, usize) {
    let (doc, front) = Doc::with_frontmatter(text);
    doc.note_unclosed(rel, notes);
    let meta = |key: &str| {
        front
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !is_placeholder(v))
    };

    struct Head<'t> {
        line: usize,
        /// The next `F-` heading's line (or EOF): the finding can't pass it.
        limit: usize,
        level: usize,
        id: String,
        num: u64,
        claim: &'t str,
    }
    let mut heads: Vec<Head> = Vec::new();
    let mut found = 0usize;
    let mut title: Option<&str> = None;
    for i in 0..doc.lines.len() {
        let Some((level, text)) = doc.text(i).and_then(heading) else {
            continue;
        };
        if level == 1 && title.is_none() {
            title = Some(text);
        }
        let (2 | 3, Some((id, num, claim))) = (level, finding_id(text)) else {
            continue;
        };
        found += 1;
        if let Some(last) = heads.last_mut().filter(|h| h.limit == doc.lines.len()) {
            last.limit = i;
        }
        if heads.len() < MAX_SCANNED_ENTRIES {
            heads.push(Head {
                line: i,
                limit: doc.lines.len(),
                level,
                id,
                num,
                claim,
            });
        }
    }
    if found > MAX_SCANNED_ENTRIES {
        notes.push(format!(
            "{rel}: {found} findings; only the first {MAX_SCANNED_ENTRIES} were considered"
        ));
    }
    // Stable: a duplicated id keeps file order.
    heads.sort_by_key(|h| h.num);
    heads.truncate(budget);
    if heads.is_empty() {
        return (None, found);
    }

    let last_updated = meta("last_updated")
        .filter(|v| v.as_bytes().get(..10).is_some_and(is_iso_date))
        .map(|v| v[..10].to_owned());
    let findings = heads
        .iter()
        .map(|head| {
            // A `## F-` finding runs to the next `#`/`##` heading; a `### F-`
            // one also stops at a `###` that isn't one of its subsections.
            let end = (head.line + 1..head.limit)
                .find(|&i| {
                    doc.text(i).and_then(heading).is_some_and(|(level, text)| {
                        level <= 2
                            || (head.level == 3
                                && level == 3
                                && subsection(text) == Subsection::Other)
                    })
                })
                .unwrap_or(head.limit);
            let mut finding = parse_finding(
                &doc,
                head.line,
                end,
                &head.id,
                head.claim,
                last_updated.as_deref(),
                notes,
            );
            finding.line = line_no(head.line);
            finding
        })
        .collect();

    let description = meta("description")
        .or_else(|| meta("topic"))
        .or(title.filter(|t| !is_placeholder(t)))
        .unwrap_or("");
    let topic = Topic {
        slug: cap_text(slug.to_owned()),
        description: cap_text(description.to_owned()),
        path: rel.to_owned(),
        findings,
    };
    (Some(topic), found)
}

fn parse_finding(
    doc: &Doc,
    head: usize,
    end: usize,
    id: &str,
    heading_claim: &str,
    last_updated: Option<&str>,
    notes: &mut Notes,
) -> Finding {
    let fields = Fields::parse(doc, head + 1, end, FINDING_FIELDS);
    let mut ledger_lines = Vec::new();
    let mut question_lines = Vec::new();
    let mut section = Subsection::Other;
    for i in head + 1..end {
        let Some(line) = doc.text(i) else {
            continue;
        };
        if let Some((_, text)) = heading(line) {
            section = subsection(text);
        } else if is_thematic_break(line) {
            section = Subsection::Other;
        } else if section == Subsection::Ledger {
            ledger_lines.push(line);
        } else if section == Subsection::Questions {
            question_lines.push(line);
        }
    }

    let ledger = parse_ledger(&ledger_lines);
    let updated = ledger
        .newest
        .or_else(|| last_updated.map(str::to_owned))
        .unwrap_or_default();
    if ledger.total > ledger.rows.len() {
        notes.push(format!(
            "{id}: showing the newest {} of {} evidence rows",
            ledger.rows.len(),
            ledger.total
        ));
    }

    let open: Vec<ListItem> = list_items(question_lines)
        .into_iter()
        .filter(|q| !q.done && !is_placeholder(&q.text))
        .collect();
    if open.len() > MAX_QUESTIONS_PER_FINDING {
        notes.push(format!(
            "{id}: showing the first {MAX_QUESTIONS_PER_FINDING} of {} open questions",
            open.len()
        ));
    }
    let questions = live_items(open, MAX_QUESTIONS_PER_FINDING);

    let claim = if is_placeholder(heading_claim) {
        fields.text(&["claim"])
    } else {
        cap_text(heading_claim.to_owned())
    };
    let status = fields
        .get(&["status"])
        .and_then(|lines| lines.first())
        .map_or_else(|| "unknown".to_owned(), |v| normalize_status(v));
    Finding {
        id: id.to_owned(),
        claim,
        status,
        implications: fields.text(&["implications"]),
        tags: fields.tags(),
        ledger: ledger.rows.into(),
        questions,
        line: 0,
        updated,
    }
}

/// Column indexes for date, run, dataset, project, result, direction.
type LedgerColumns = [Option<usize>; 6];

/// mycelium's column order, for a table with no recognizable header.
const LEDGER_POSITIONAL: LedgerColumns = [Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)];

fn ledger_columns(header: &[String]) -> Option<LedgerColumns> {
    let mut cols: LedgerColumns = [None; 6];
    for (i, cell) in header.iter().enumerate() {
        let name = cell.to_lowercase();
        let slot = if name.contains("date") {
            0
        } else if name.contains("run") || name.contains("session") {
            1
        } else if name.contains("dataset") || name == "data" {
            2
        } else if name.contains("project") {
            3
        } else if name.contains("result") || name.contains("observ") {
            4
        } else if name.contains("direction") {
            5
        } else {
            continue;
        };
        cols[slot].get_or_insert(i);
    }
    cols.iter().any(Option::is_some).then_some(cols)
}

struct Ledger {
    /// The newest [`MAX_LEDGER_ROWS`] (ledgers append), in file order.
    rows: VecDeque<LedgerRow>,
    total: usize,
    /// Newest ISO date across ALL rows, kept or not.
    newest: Option<String>,
}

/// An Evidence Ledger table, streamed: only the kept rows are ever built.
/// Columns are found by header name; mycelium's order is the fallback.
fn parse_ledger(lines: &[&str]) -> Ledger {
    let table: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.trim_start().starts_with('|'))
        .collect();
    let mut ledger = Ledger {
        rows: VecDeque::new(),
        total: 0,
        newest: None,
    };
    let mut cols = LEDGER_POSITIONAL;
    for (k, line) in table.iter().enumerate() {
        let Some(row) = table_cells(line) else {
            continue;
        };
        if is_separator(&row) {
            continue;
        }
        let header = table
            .get(k + 1)
            .and_then(|next| table_cells(next))
            .is_some_and(|next| is_separator(&next));
        if header {
            cols = ledger_columns(&row).unwrap_or(LEDGER_POSITIONAL);
            continue;
        }
        let cell = |slot: usize| {
            cols[slot]
                .and_then(|i| row.get(i))
                .map_or_else(String::new, |c| cell_text(c))
        };
        let date = cell(0);
        // mycelium's template row (`| YYYY-MM-DD | {session-id} | …`).
        if date.eq_ignore_ascii_case("yyyy-mm-dd") || date.starts_with('{') {
            continue;
        }
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        ledger.total += 1;
        if let Some(day) = date.get(..10).filter(|d| is_iso_date(d.as_bytes())) {
            if ledger.newest.as_deref().is_none_or(|newest| day > newest) {
                ledger.newest = Some(day.to_owned());
            }
        }
        let direction = cols[5]
            .and_then(|i| row.get(i))
            .map_or_else(|| "unknown".to_owned(), |c| normalize_direction(c));
        if ledger.rows.len() == MAX_LEDGER_ROWS {
            ledger.rows.pop_front();
        }
        ledger.rows.push_back(LedgerRow {
            run: cell(1),
            dataset: cell(2),
            project: cell(3),
            result: cell(4),
            date,
            direction,
        });
    }
    ledger
}

// ---------------------------------------------------------------------------
// Handoff
// ---------------------------------------------------------------------------

/// Section index for a handoff heading, across both schemas
/// `finalize_handoff.py` accepts (the five-section one and the
/// Current State / What Was Done / Key Decisions / Next Steps / Relevant
/// Files one). "Relevant Files" has no slot.
fn handoff_section(text: &str) -> Option<usize> {
    let name = collapse_ws(&text.to_lowercase().replace('&', "and"));
    match name.trim_end_matches(':') {
        "what was worked on" | "what was done" | "what we worked on" => Some(0),
        "key decisions made" | "key decisions" | "decisions made" => Some(1),
        "blockers and surprises" | "blockers" => Some(2),
        "current state" => Some(3),
        "next steps" => Some(4),
        _ => None,
    }
}

/// mycelium's `finalize_handoff._STALE_STOP_LINE`, word-level: lifecycle
/// chatter ("Stop hook finalization pending", "attempting a natural stop")
/// that the finalizer strips on acceptance. An in-flight run handoff hasn't
/// been through it yet.
fn is_stale_stop_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    // Only the FIRST lead can matter (any later one's tail is a suffix of
    // its tail), which keeps this linear on a hostile one-line file.
    let lead = words.windows(2).position(|pair| {
        matches!(pair, ["natural", "stop"])
            || matches!(pair, ["stop", "hook" | "finalization" | "attempt"])
    });
    let lifecycle = lead.is_some_and(|at| {
        let tail = &words[at + 2..];
        tail.iter().enumerate().any(|(j, w)| {
            matches!(*w, "pending" | "remain" | "remains" | "attempt")
                || (*w == "not" && tail.get(j + 1) == Some(&"yet"))
        })
    });
    lifecycle
        || words
            .iter()
            .position(|w| matches!(*w, "attempt" | "attempting"))
            .is_some_and(|at| words[at + 1..].contains(&"stop"))
}

/// The handoff minus mycelium's lifecycle-status block and stale Stop lines —
/// the body `finalize_handoff.clean_handoff_body` would publish.
fn clean_handoff(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let closed = text.contains(STATUS_END);
    let mut in_status = false;
    for line in text.split('\n') {
        if closed && !in_status && line.contains(STATUS_BEGIN) {
            in_status = true;
        }
        if in_status {
            in_status = !line.contains(STATUS_END);
            continue;
        }
        if is_stale_stop_line(line) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn parse_handoff(text: &str, source: &Source, notes: &mut Notes) -> Option<LeftOff> {
    let cleaned = clean_handoff(text);
    let doc = Doc::new(&cleaned);
    doc.note_unclosed(&source.rel, notes);
    // A known section heading opens its slot (a repeat appends to it); any
    // other heading at or above its level closes it; deeper ones are content.
    let mut sections: [Vec<&str>; 5] = Default::default();
    let mut current: Option<(usize, usize)> = None;
    for (i, &line) in doc.lines.iter().enumerate() {
        match doc.marks[i] {
            Mark::Comment => continue,
            Mark::Code => {}
            Mark::Text => {
                if let Some((level, text)) = heading(line) {
                    if let Some(slot) = handoff_section(text) {
                        current = Some((slot, level));
                        continue;
                    }
                    if current.is_some_and(|(_, open)| level <= open) {
                        current = None;
                    }
                }
            }
        }
        if let Some((slot, _)) = current {
            sections[slot].push(line);
        }
    }

    // Prose sections keep their markdown, minus filler lines ("- None").
    let prose = |lines: &[&str]| {
        let kept: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| {
                let t = l.trim();
                t.is_empty() || !is_placeholder(list_marker(t).unwrap_or(t))
            })
            .collect();
        cap_text(tidy(&kept))
    };
    // List sections are items; a section written as prose yields its
    // paragraphs instead.
    let list = |lines: &[&str]| {
        let mut items = list_items(lines.iter().copied());
        if items.is_empty() {
            items = paragraphs(lines)
                .into_iter()
                .map(|text| ListItem { text, done: false })
                .collect();
        }
        live_items(items, MAX_LIST_ITEMS)
    };
    let left = LeftOff {
        worked_on: prose(&sections[0]),
        decisions: prose(&sections[1]),
        blockers: list(&sections[2]),
        current: prose(&sections[3]),
        next: list(&sections[4]),
        written_ms: mtime_ms(&source.meta),
        path: source.rel.clone(),
        session_id: None,
        host: None,
    };
    let empty = left.worked_on.is_empty()
        && left.decisions.is_empty()
        && left.blockers.is_empty()
        && left.current.is_empty()
        && left.next.is_empty();
    if empty {
        notes.push(format!("{}: no handoff sections with content", source.rel));
        return None;
    }
    Some(left)
}

// ---------------------------------------------------------------------------
// Todos
// ---------------------------------------------------------------------------

/// A registry link target, made workspace-relative (the registry links
/// relative to `todo/`). Absolute paths, URLs, and targets that would climb
/// out of the workspace stay verbatim.
fn todo_path(target: &str) -> String {
    let target = target.trim();
    let path = target.split(['#', '?']).next().unwrap_or("");
    if path.is_empty() {
        return String::new();
    }
    if path.contains("://") || path.starts_with(['/', '~', '\\']) {
        return cap_text(target.to_owned());
    }
    let mut parts = vec!["todo"];
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return cap_text(target.to_owned());
                }
            }
            part => parts.push(part),
        }
    }
    cap_text(parts.join("/"))
}

/// Registry column indexes for item, priority, status, category, date,
/// author, file.
type TodoColumns = [Option<usize>; 7];

fn todo_columns(header: &[String]) -> Option<TodoColumns> {
    const NAMES: [&str; 7] = [
        "item", "priority", "status", "category", "date", "author", "file",
    ];
    let mut cols: TodoColumns = [None; 7];
    for (i, cell) in header.iter().enumerate() {
        let name = cell.trim_matches(['*', '`', ' ']).to_lowercase();
        if let Some(slot) = NAMES.iter().position(|n| *n == name) {
            cols[slot].get_or_insert(i);
        }
    }
    // Item plus Status or Priority: mycelium's Status/Priority key tables
    // ("Status | Meaning") have no Item column and must not match.
    (cols[0].is_some() && (cols[1].is_some() || cols[2].is_some())).then_some(cols)
}

/// `todo/TODO_REGISTRY.md` — the `Item | Priority | Status | …` table,
/// found by its header (mycelium's full template puts Status/Priority key
/// tables above it). Rows are read to the next heading, NOT to the
/// `<!-- Add new entries above this line -->` marker: mycelium 0.7.2's
/// `upsert_table_row.py` appends new rows at the end of the file, below it.
/// The legacy `todo/TODOLIST.md` had no schema; its list items are todos.
fn parse_todos(text: &str, rel: &str, legacy: bool, notes: &mut Notes) -> Vec<Todo> {
    let doc = Doc::new(text);
    doc.note_unclosed(rel, notes);
    if legacy {
        notes.push(format!(
            "{rel} is mycelium's legacy todo list; 0.7 keeps todos in todo/TODO_REGISTRY.md"
        ));
    }
    let row_at = |i: usize| doc.text(i).and_then(table_cells);
    let is_header = |i: usize| row_at(i + 1).is_some_and(|next| is_separator(&next));

    let header = (0..doc.lines.len()).find_map(|i| {
        let cells = row_at(i)?;
        if !is_header(i) {
            return None;
        }
        todo_columns(&cells).map(|cols| (i, cols))
    });
    let Some((header_at, cols)) = header else {
        if legacy {
            return legacy_todos(&doc, rel, notes);
        }
        notes.push(format!("{rel}: no Item | Priority | Status table found"));
        return Vec::new();
    };

    let mut todos = Vec::new();
    let mut total = 0usize;
    for i in header_at + 2..doc.lines.len() {
        let Some(line) = doc.text(i) else {
            continue;
        };
        if heading(line).is_some() {
            break;
        }
        let Some(cells) = table_cells(line) else {
            continue;
        };
        if is_separator(&cells) {
            continue;
        }
        if is_header(i) {
            // A repeated registry header is skipped; any other table ends it.
            if todo_columns(&cells).is_some() {
                continue;
            }
            break;
        }
        let Some(todo) = todo_row(&cells, &cols) else {
            continue;
        };
        total += 1;
        if todos.len() < MAX_ENTRIES {
            todos.push(todo);
        }
    }
    if total > MAX_ENTRIES {
        notes.push(format!(
            "{rel}: showing the first {MAX_ENTRIES} of {total} todos"
        ));
    }
    todos
}

fn todo_row(cells: &[String], cols: &TodoColumns) -> Option<Todo> {
    let raw = |slot: usize| {
        cols[slot]
            .and_then(|i| cells.get(i))
            .map_or("", String::as_str)
    };
    let (_, item_link) = strip_links(raw(0));
    let item = cell_text(raw(0));
    if item.is_empty() {
        return None;
    }
    let (_, file_link) = strip_links(raw(6));
    let file_text = cell_text(raw(6));
    let file = file_link
        .or((!file_text.is_empty()).then_some(file_text))
        .or(item_link)
        .map_or_else(String::new, |target| todo_path(&target));
    Some(Todo {
        item,
        priority: cell_text(raw(1)).to_lowercase(),
        status: cell_text(raw(2)).to_lowercase(),
        category: cell_text(raw(3)),
        date: cell_text(raw(4)),
        author: cell_text(raw(5)),
        file,
    })
}

fn legacy_todos(doc: &Doc, rel: &str, notes: &mut Notes) -> Vec<Todo> {
    let lines = (0..doc.lines.len()).filter_map(|i| doc.text(i));
    let items = list_items(lines);
    if items.len() > MAX_ENTRIES {
        notes.push(format!(
            "{rel}: showing the first {MAX_ENTRIES} of {} todos",
            items.len()
        ));
    }
    items
        .into_iter()
        .filter(|item| !is_placeholder(&item.text))
        .take(MAX_ENTRIES)
        .map(|item| {
            let (text, link) = strip_links(&item.text);
            Todo {
                item: cap_text(text.trim().to_owned()),
                status: if item.done { "complete" } else { "open" }.to_owned(),
                file: link.map_or_else(String::new, |target| todo_path(&target)),
                ..Todo::default()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    /// A throwaway workspace root, removed on drop.
    struct Fixture(PathBuf);

    impl Fixture {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "chimaera-mycelium-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn root(&self) -> &Path {
            &self.0
        }

        fn write(&self, rel: &str, body: &str) -> &Self {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
            self
        }

        fn set_mtime(&self, rel: &str, secs_ago: u64) {
            let when = SystemTime::now() - Duration::from_secs(secs_ago);
            File::options()
                .write(true)
                .open(self.0.join(rel))
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const BATCH_EFFECTS: &str = r#"---
topic: batch-effects
description: How sequencing batch shifts expression estimates across runs.
created: 2026-08-01
last_updated: 2026-09-10
status: active
---

# Batch Effects

## F-003: Batch 2 inflates the mitochondrial fraction
**Status:** supported
**Claim:** Libraries sequenced in batch 2 show a ~4% higher mitochondrial read fraction than batch 1 after QC.
**Implications:** Regress out batch before comparing mito-based QC across runs.
**Tags:** qc, batch, mitochondria

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|------|-------------|---------|---------|--------|-----------|
| 2026-08-14 | run-17 | pbmc-10k | atlas | +4.1% mito in batch 2 | supports |
| 2026-09-10 | [run-22](../log/2026-09-10-001-qc.md) | pbmc-20k | atlas | +3.8% mito in batch 2 | supports |

### Open Questions
- Does the shift persist after ambient RNA removal?
"#;

    const QC_THRESHOLDS: &str = r#"---
topic: qc-thresholds
description: "Per-sample QC cutoffs that hold across tissues."
last_updated: 2026-08-20
---

# QC thresholds

## F-005: MAD-based cutoffs beat fixed thresholds
**Status:** supported
**Claim:** Three-MAD cutoffs keep more good cells than fixed 500-gene floors.
**Implications:** Use MAD cutoffs per sample.
**Tags:** [qc, thresholds]

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-08-18 | run-19 | kidney | atlas | 3 MAD retained 12% more cells | supports |
| 2026-08-20 | run-20 | lung | atlas | same pattern | supports |

---

## F-004: Doublet rate scales with loading density
**Status:** preliminary
**Claim:** Doublets rise roughly linearly with cells loaded per lane.
**Tags:** #doublets #qc

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-08-19 | run-19 | kidney | atlas | 8% doublets at 20k loading | supports |

### Open Questions
- Is the scaling linear above 20k cells?
- {Gap in evidence that needs more data}
"#;

    const EXHAUSTION: &str = r#"---
topic: exhaustion
description: T-cell exhaustion signatures in tumour infiltrates.
---

# Exhaustion

## F-001: TOX marks terminal exhaustion
**Status:** robust
**Claim:** A TOX+ PD1-high cluster appears in every tumour cohort.
**Tags:** tcell, exhaustion

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-07-02 | run-3 | melanoma | tumour | TOX+ PD1hi cluster | supports |
| 2026-07-20 | run-8 | nsclc | tumour | same cluster | supports |
| 2026-08-05 | run-12 | crc | colon | only in MSI-high | refines |

## F-002: TCF7 predicts response
**Status:** contradicted
**Claim:** TCF7+ CD8 fraction predicts checkpoint response.

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-07-05 | run-4 | melanoma | tumour | TCF7+ enriched in responders | supports |
| 2026-08-30 | run-15 | nsclc | tumour | no association | contradicts |

### Open Questions
- Is the melanoma association a cohort artefact?
"#;

    const DECISIONS: &str = r#"# Decision Log

Append-only log of non-obvious decisions and their rationale.

**Entry template:** copy from `skills/core/templates/decision-log-entry.md` (includes Context, Decision, Alternatives considered, Rationale, Consequences, Tags fields).

### [2026-08-02] Use scran size factors over CPM

**Context**: Library sizes vary ten-fold across the two sequencing batches.

**Decision**: Normalise with scran pooled size factors.

**Alternatives considered**:
- CPM — ignores composition bias
- SCTransform — too slow on 200k cells

**Rationale**: scran corrects composition bias and scales to the atlas.

**Consequences**: Normalisation now needs a quick clustering pass first.

**Tags**: [normalisation, scran]

### [2026-09-12] Drop sample S14 from the atlas

**Context**: S14 failed QC in both batches.

**Decision**: Exclude S14 from all downstream analyses.

**Alternatives considered**:
- Keep S14 with a batch covariate — it still dominated PC1

**Rationale**: 38% mitochondrial reads; no rescue possible.

**Consequences**: The cohort is n=23.

**Tags**: qc, exclusion

### [2026-08-20] Pin scanpy to 1.10

**Context**: 1.11 changed the default HVG flavour.

**Decision**: Pin scanpy==1.10.3 in the environment.

**Alternatives considered**: none

**Rationale**: Results must stay comparable with the July runs.

**Consequences**: Revisit before publication.

**Tags**: #environment #scanpy
"#;

    const LEARNINGS: &str = r#"# Learnings

Append-only log of gotchas, surprises, and insights.

### [2026-08-02] Scanpy drops genes with zero counts

**Category**: gotcha

**What happened**: `sc.pp.filter_genes` silently removed 1,204 genes before HVG selection.

**Why it matters**: Marker panels lose genes without any warning.

**Resolution**: Filter after subsetting the marker panel.

**Tags**: [scanpy, filtering]

**mitigation_type**: ambient-awareness

<!-- mitigation_type guidance:
  structural       — A test, assertion, type constraint, frozenset, or schema
                     validation has SHIPPED in the codebase.
-->

**structural_mitigation_candidate**: assert the panel survives filtering in test_markers.py

source: atlas

### [2026-08-04] Slurm array OOM at 64G

**Category**: failure

**What happened**: The doublet step ran out of memory on the 200k-cell object.
It died building the kNN graph.

**Why it matters**: Re-runs cost a day of queue time.

**Resolution**: Request 128G for arrays above 150k cells.

**Tags**: slurm, memory

### [2026-08-11] Empty droplets in lane 3

**Category**: edge-case

**What happened**: Lane 3 had 40% empty droplets that passed the UMI floor.

**Why it matters**: They cluster together and look like a novel population.

**Resolution**: Run EmptyDrops per lane.

**Tags**: [droplets]

### [2026-08-15] Ambient RNA dominates low-count cells

**Category**: insight

**What happened**: Below 800 UMIs the profile matches the soup.

**Why it matters**: Low-count clusters are mostly ambient.

**Resolution**: SoupX before clustering.

**Tags**: ambient

### [2026-08-19] Cache the kNN graph

**Category**: tip

**What happened**: Rebuilding the graph took 40 minutes per run.

**Why it matters**: Parameter sweeps were dominated by it.

**Resolution**: Persist `obsp` between sweeps.

**Tags**: performance
"#;

    const TODO_REGISTRY: &str = r#"# TODO Registry

All future work items for this project are tracked here.

## Status Key

| Status | Meaning |
|--------|---------|
| `open` | Not yet started |
| `blocked` | Waiting on something external |

## Registry

| Item | Priority | Status | Category | Date | Author | File |
|------|----------|--------|----------|------|--------|------|
| Compare with public datasets | idea | open | validation | 2026-08-01 | Martin | [compare-public-data.md](compare-public-data.md) |
| Re-run QC with MAD cutoffs | high | in-progress | analysis | 2026-08-21 | Martin | [rerun-qc.md](rerun-qc.md) |
| Get batch 3 FASTQs | critical | `blocked` | data | 2026-09-02 | Martin | — |

<!-- Add new entries above this line -->
| Write methods section | medium | complete | writing | 2026-09-15 | Martin | [methods.md](methods.md) |
"#;

    const LAST_SESSION: &str = r#"<!-- BEGIN MYCELIUM LIFECYCLE STATUS -->
Lifecycle status: accepted by Stop at 2026-09-15T18:02:11Z.
<!-- END MYCELIUM LIFECYCLE STATUS -->

SESSION RESUME — Last session (2026-09-15 17:40):

## What was worked on
- Re-ran QC on batches 1-2 with MAD cutoffs
- Drafted the methods section

## Key decisions made
- Dropped S14 (see .living/decisions.md for full context)

## Blockers & surprises
- Batch 3 FASTQs still not delivered
- None

## Current state
- Branch: `qc-mad` | Tests: 42 passing

## Next steps
1. Request batch 3 from the sequencing core
2. Regress out batch before mito QC
- [x] Push the branch
"#;

    fn project() -> Fixture {
        let fx = Fixture::new("project");
        fx.write("MYCELIUM.md", "# Mycelium\n")
            .write(".living/findings/batch-effects.md", BATCH_EFFECTS)
            .write(".living/findings/qc-thresholds.md", QC_THRESHOLDS)
            .write(".living/findings/exhaustion.md", EXHAUSTION)
            .write(
                ".living/findings/FINDINGS_REGISTRY.md",
                "# Findings Registry\n\n## F-999: not a topic\n",
            )
            .write(".living/findings/INDEX.md", "## F-998: not a topic\n")
            .write(".living/decisions.md", DECISIONS)
            .write(".living/learnings.md", LEARNINGS)
            .write("todo/TODO_REGISTRY.md", TODO_REGISTRY)
            .write(".mycelium/last-session.md", LAST_SESSION);
        fx
    }

    #[test]
    fn a_realistic_project_reads_every_section() {
        let fx = project();
        let k = read(fx.root());
        assert!(k.warnings.is_empty(), "{:?}", k.warnings);

        // Topics by slug; the registry/index files are not topics.
        let slugs: Vec<&str> = k.topics.iter().map(|t| t.slug.as_str()).collect();
        assert_eq!(slugs, ["batch-effects", "exhaustion", "qc-thresholds"]);
        let batch = &k.topics[0];
        assert_eq!(batch.path, ".living/findings/batch-effects.md");
        assert_eq!(
            batch.description,
            "How sequencing batch shifts expression estimates across runs."
        );
        let f3 = &batch.findings[0];
        assert_eq!(f3.id, "F-003");
        assert_eq!(f3.claim, "Batch 2 inflates the mitochondrial fraction");
        assert_eq!(f3.status, "supported");
        assert_eq!(f3.tags, ["qc", "batch", "mitochondria"]);
        assert_eq!(f3.ledger.len(), 2);
        assert!(f3.ledger.iter().all(|r| r.direction == "supports"));
        assert_eq!(f3.ledger[1].run, "run-22", "links unwrap to their text");
        assert_eq!(f3.ledger[1].dataset, "pbmc-20k");
        assert_eq!(f3.updated, "2026-09-10");
        assert_eq!(
            f3.questions,
            ["Does the shift persist after ambient RNA removal?"]
        );
        assert_eq!(f3.line, 11);
        assert!(f3.implications.starts_with("Regress out batch"));

        let exhaustion = &k.topics[1];
        let statuses: Vec<(&str, &str)> = exhaustion
            .findings
            .iter()
            .map(|f| (f.id.as_str(), f.status.as_str()))
            .collect();
        assert_eq!(statuses, [("F-001", "robust"), ("F-002", "contradicted")]);
        assert_eq!(exhaustion.findings[0].ledger[2].direction, "refines");
        assert_eq!(exhaustion.findings[1].ledger[1].direction, "contradicts");
        assert_eq!(exhaustion.findings[1].updated, "2026-08-30");

        // Numeric order, not file order; the `{…}` template question drops.
        let qc = &k.topics[2];
        assert_eq!(
            qc.description,
            "Per-sample QC cutoffs that hold across tissues."
        );
        let ids: Vec<&str> = qc.findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["F-004", "F-005"]);
        assert_eq!(qc.findings[0].status, "preliminary");
        assert_eq!(qc.findings[0].tags, ["doublets", "qc"]);
        assert_eq!(qc.findings[0].implications, "");
        assert_eq!(
            qc.findings[0].questions,
            ["Is the scaling linear above 20k cells?"]
        );
        assert_eq!(qc.findings[1].tags, ["qc", "thresholds"]);
        assert_eq!(qc.findings[1].status, "supported");

        // Decisions newest first.
        let titles: Vec<&str> = k.decisions.iter().map(|d| d.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Drop sample S14 from the atlas",
                "Pin scanpy to 1.10",
                "Use scran size factors over CPM"
            ]
        );
        let scran = &k.decisions[2];
        assert_eq!(scran.date, "2026-08-02");
        assert_eq!(scran.line, 7);
        assert_eq!(
            scran.context,
            "Library sizes vary ten-fold across the two sequencing batches."
        );
        assert_eq!(scran.decision, "Normalise with scran pooled size factors.");
        assert_eq!(
            scran.alternatives,
            [
                "CPM — ignores composition bias",
                "SCTransform — too slow on 200k cells"
            ]
        );
        assert_eq!(
            scran.rationale,
            "scran corrects composition bias and scales to the atlas."
        );
        assert_eq!(
            scran.consequences,
            "Normalisation now needs a quick clustering pass first."
        );
        assert_eq!(scran.tags, ["normalisation", "scran"]);
        assert_eq!(
            scran.fp,
            fingerprint("decision", "2026-08-02", "Use scran size factors over CPM")
        );
        assert!(
            k.decisions[1].alternatives.is_empty(),
            "\"none\" is no alternative"
        );
        assert_eq!(k.decisions[1].tags, ["environment", "scanpy"]);
        assert_eq!(k.decisions[0].tags, ["qc", "exclusion"]);

        // Learnings newest first; template extras don't leak into fields.
        let categories: Vec<&str> = k.learnings.iter().map(|l| l.category.as_str()).collect();
        assert_eq!(
            categories,
            ["tip", "insight", "edge-case", "failure", "gotcha"]
        );
        let gotcha = &k.learnings[4];
        assert_eq!(gotcha.fp, "L-d407379e8c29");
        assert_eq!(
            gotcha.what,
            "`sc.pp.filter_genes` silently removed 1,204 genes before HVG selection."
        );
        assert_eq!(gotcha.why, "Marker panels lose genes without any warning.");
        assert_eq!(
            gotcha.resolution,
            "Filter after subsetting the marker panel."
        );
        assert_eq!(gotcha.tags, ["scanpy", "filtering"]);
        assert_eq!(
            k.learnings[3].what,
            "The doublet step ran out of memory on the 200k-cell object.\nIt died building the kNN graph."
        );
        assert_eq!(k.learnings[3].tags, ["slurm", "memory"]);

        // Todos: the registry table only (not the Status Key), including the
        // row mycelium's upsert appended below the marker.
        let todos: Vec<(&str, &str, &str)> = k
            .todos
            .iter()
            .map(|t| (t.item.as_str(), t.status.as_str(), t.file.as_str()))
            .collect();
        assert_eq!(
            todos,
            [
                (
                    "Compare with public datasets",
                    "open",
                    "todo/compare-public-data.md"
                ),
                (
                    "Re-run QC with MAD cutoffs",
                    "in-progress",
                    "todo/rerun-qc.md"
                ),
                ("Get batch 3 FASTQs", "blocked", ""),
                ("Write methods section", "complete", "todo/methods.md"),
            ]
        );
        assert_eq!(k.todos[0].priority, "idea");
        assert_eq!(k.todos[0].category, "validation");
        assert_eq!(k.todos[0].date, "2026-08-01");
        assert_eq!(k.todos[0].author, "Martin");

        // Open questions, flattened in topic order.
        let questions: Vec<(&str, &str)> = k
            .questions
            .iter()
            .map(|q| (q.finding.as_str(), q.text.as_str()))
            .collect();
        assert_eq!(
            questions,
            [
                ("F-003", "Does the shift persist after ambient RNA removal?"),
                ("F-002", "Is the melanoma association a cohort artefact?"),
                ("F-004", "Is the scaling linear above 20k cells?"),
            ]
        );

        let left = k.left_off.as_ref().expect("handoff");
        assert_eq!(left.path, ".mycelium/last-session.md");
        assert_eq!(
            left.worked_on,
            "- Re-ran QC on batches 1-2 with MAD cutoffs\n- Drafted the methods section"
        );
        assert_eq!(
            left.decisions,
            "- Dropped S14 (see .living/decisions.md for full context)"
        );
        assert_eq!(left.blockers, ["Batch 3 FASTQs still not delivered"]);
        assert_eq!(left.current, "- Branch: `qc-mad` | Tests: 42 passing");
        assert_eq!(
            left.next,
            [
                "Request batch 3 from the sequencing core",
                "Regress out batch before mito QC"
            ]
        );
        assert!(left.written_ms > 0);
        assert_eq!(left.session_id, None);

        assert_eq!(k.counts.findings, 5);
        assert_eq!(k.counts.decisions, 3);
        assert_eq!(k.counts.learnings, 5);
        // open + in-progress + blocked todos, plus three open questions.
        assert_eq!(k.counts.open, 6);
    }

    #[test]
    fn the_wire_shape_omits_absent_options() {
        let fx = project();
        let json = serde_json::to_value(read(fx.root())).unwrap();
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for key in [
            "left_off",
            "topics",
            "decisions",
            "learnings",
            "todos",
            "questions",
            "counts",
            "warnings",
        ] {
            assert!(keys.contains(&key), "missing {key}");
        }
        let left = json["left_off"].as_object().unwrap();
        assert!(!left.contains_key("session_id") && !left.contains_key("host"));
        assert_eq!(json["counts"]["open"], 6);
        assert_eq!(
            json["topics"][0]["findings"][0]["ledger"][0]["direction"],
            "supports"
        );

        let bare = Fixture::new("bare");
        let json = serde_json::to_value(read(bare.root())).unwrap();
        assert!(json.get("left_off").is_none());
        assert_eq!(json["decisions"], serde_json::json!([]));
    }

    #[test]
    fn fenced_and_commented_pseudo_entries_are_not_knowledge() {
        let fx = Fixture::new("fences");
        fx.write(
            ".living/decisions.md",
            r#"# Decisions

### [2026-05-01] Real decision

**Decision**: keep it

```markdown
### [2026-05-02] Example inside a fence
**Tags**: [nope]
```

**Tags**: [real]

~~~~
~~~
### [2026-05-03] A shorter closer doesn't close a longer fence
~~~~

- ```
### [2026-05-04] Fenced under a list marker
```

<!--
### [2026-05-05] Commented out
-->

### [2026-05-06] Second real decision
"#,
        );
        let k = read(fx.root());
        let titles: Vec<&str> = k.decisions.iter().map(|d| d.title.as_str()).collect();
        assert_eq!(titles, ["Second real decision", "Real decision"]);
        assert_eq!(k.decisions[1].tags, ["real"]);
        assert!(k.decisions[1].decision.starts_with("keep it"));
        assert!(k.warnings.is_empty(), "{:?}", k.warnings);
    }

    #[test]
    fn an_unclosed_fence_hides_the_rest_and_says_so() {
        let fx = Fixture::new("unclosed");
        fx.write(
            ".living/decisions.md",
            "### [2026-05-01] Before\n**Decision**: a\n\n```\n### [2026-05-02] After an unclosed fence\n",
        );
        let k = read(fx.root());
        assert_eq!(k.decisions.len(), 1);
        assert_eq!(k.decisions[0].title, "Before");
        assert!(
            k.warnings
                .iter()
                .any(|w| w.contains("unclosed code fence at line 4")),
            "{:?}",
            k.warnings
        );
    }

    #[test]
    fn dated_level_two_headings_are_legacy_entries_with_a_warning() {
        let fx = Fixture::new("legacy-level");
        fx.write(
            ".living/decisions.md",
            "# Decision Log\n\n## [2026-03-01] Legacy bracketed\n**Decision**: a\n\n\
             ## 2026-03-02 Legacy bare date\n**Decision**: b\n\n## Archive\n\n\
             Notes that are not an entry.\n\n### [2026-03-03] Current format\n**Decision**: c\n",
        );
        let k = read(fx.root());
        let got: Vec<(&str, &str, &str)> = k
            .decisions
            .iter()
            .map(|d| (d.date.as_str(), d.title.as_str(), d.decision.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("2026-03-03", "Current format", "c"),
                ("2026-03-02", "Legacy bare date", "b"),
                ("2026-03-01", "Legacy bracketed", "a"),
            ]
        );
        assert_eq!(
            k.warnings,
            [".living/decisions.md: 2 entries use ## headings (mycelium 0.7 expects ###)"]
        );
    }

    #[test]
    fn undated_headings_sort_last_and_keep_dates_inside_the_title() {
        let fx = Fixture::new("undated");
        fx.write(
            ".living/learnings.md",
            "### Untitled lesson\n**Category**: tip\n**What happened**: x\n\n\
             ### Cohort 2026-01-01 to 2026-02-01 mislabeled\n**Category**: gotcha\n\n\
             ### [2026-02-10] Dated one\n**Category**: insight\n",
        );
        let k = read(fx.root());
        let got: Vec<(&str, &str)> = k
            .learnings
            .iter()
            .map(|l| (l.date.as_str(), l.title.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("2026-02-10", "Dated one"),
                ("", "Cohort 2026-01-01 to 2026-02-01 mislabeled"),
                ("", "Untitled lesson"),
            ]
        );
        assert_eq!(k.learnings[2].category, "tip");
        assert_eq!(k.learnings[2].what, "x");
        assert_eq!(
            k.learnings[2].fp,
            fingerprint("learning", "", "Untitled lesson")
        );
    }

    #[test]
    fn colon_inside_the_bold_and_plain_labels_parse() {
        let fx = Fixture::new("variants");
        fx.write(
            ".living/learnings.md",
            "### [2026-06-01] Colon inside the bold\n\
             **Category:** Edge case\n\
             **What happened:** First line\ncontinues here\n\n\n\nand a second paragraph.\n\
             **Why it matters:** because\n\
             - **Resolution**: bulleted known label\n\
             Tags: plain, list\n",
        );
        let k = read(fx.root());
        let l = &k.learnings[0];
        assert_eq!(l.category, "edge-case");
        assert_eq!(
            l.what,
            "First line\ncontinues here\n\nand a second paragraph."
        );
        assert_eq!(l.why, "because");
        assert_eq!(l.resolution, "bulleted known label");
        assert_eq!(l.tags, ["plain", "list"]);
    }

    #[test]
    fn an_oversized_file_is_skipped_with_a_warning() {
        let fx = Fixture::new("oversized");
        let mut huge = String::from("### [2026-01-01] Buried\n**Category**: tip\n");
        huge.push_str(&"x".repeat(MAX_FILE_BYTES as usize));
        fx.write(".living/learnings.md", &huge).write(
            ".living/decisions.md",
            "### [2026-01-02] Small\n**Decision**: ok\n",
        );
        let k = read(fx.root());
        assert!(k.learnings.is_empty());
        assert_eq!(k.decisions.len(), 1);
        assert_eq!(k.warnings.len(), 1);
        assert!(
            k.warnings[0]
                .starts_with(".living/learnings.md: skipped; 2.1 MiB is over the 2 MiB read cap"),
            "{:?}",
            k.warnings
        );
        // Still stamped: growing or shrinking it must invalidate a cache.
        assert!(stamp(fx.root())
            .files
            .iter()
            .any(|(rel, ..)| rel == ".living/learnings.md"));
    }

    #[test]
    fn a_workspace_without_mycelium_reads_nothing() {
        let fx = Fixture::new("none");
        // A stray registry is not knowledge without `.living` / MYCELIUM.md.
        fx.write("todo/TODO_REGISTRY.md", TODO_REGISTRY);
        let k = read(fx.root());
        assert!(k.left_off.is_none());
        assert!(k.topics.is_empty() && k.decisions.is_empty() && k.learnings.is_empty());
        assert!(k.todos.is_empty() && k.questions.is_empty() && k.warnings.is_empty());
        assert_eq!(k.counts.open, 0);
        assert_eq!(stamp(fx.root()), Stamp::default());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;
        let outside = Fixture::new("outside");
        outside.write(".living/decisions.md", DECISIONS);
        outside.write("topic.md", BATCH_EFFECTS);
        let fx = Fixture::new("symlinked");
        symlink(outside.root().join(".living"), fx.root().join(".living")).unwrap();

        let k = read(fx.root());
        assert!(k.decisions.is_empty());
        assert_eq!(k.warnings.len(), 1);
        assert!(k.warnings[0].starts_with(".living is a symlink; not followed"));

        // With MYCELIUM.md it is a mycelium workspace — the link still isn't
        // followed, and neither is a linked topic file inside a real .living.
        fx.write("MYCELIUM.md", "# Mycelium\n");
        assert!(read(fx.root()).decisions.is_empty());

        let real = Fixture::new("real-with-link");
        real.write(".living/findings/exhaustion.md", EXHAUSTION);
        symlink(
            outside.root().join("topic.md"),
            real.root().join(".living/findings/linked.md"),
        )
        .unwrap();
        let k = read(real.root());
        let slugs: Vec<&str> = k.topics.iter().map(|t| t.slug.as_str()).collect();
        assert_eq!(slugs, ["exhaustion"]);
        assert!(k.warnings[0].starts_with(".living/findings/linked.md is a symlink"));
        assert_eq!(stamp(real.root()).refused, [".living/findings/linked.md"]);
    }

    #[test]
    fn fingerprints_are_stable_and_normalized() {
        // FNV-1a 64 reference values, computed independently (Python).
        assert_eq!(
            fingerprint(
                "learning",
                "2026-08-02",
                "Scanpy drops genes with zero counts"
            ),
            "L-d407379e8c29"
        );
        assert_eq!(fingerprint("decision", "", "Untitled"), "D-9c35de877e4d");
        assert_eq!(
            fingerprint(
                "Learning",
                " 2026-08-02 ",
                "SCANPY  drops\tgenes with zero counts\n"
            ),
            "L-d407379e8c29"
        );
        let title = "Scanpy drops genes with zero counts";
        assert_ne!(
            fingerprint("decision", "2026-08-02", title)[2..],
            fingerprint("learning", "2026-08-02", title)[2..]
        );
        assert_ne!(
            fingerprint("learning", "2026-08-03", title),
            "L-d407379e8c29"
        );
    }

    #[test]
    fn verbatim_duplicate_entries_get_distinct_ids() {
        let fx = Fixture::new("dupes");
        fx.write(
            ".living/learnings.md",
            "### [2026-01-01] Same\n**What happened**: first\n\n\
             ### [2026-01-01] Same\n**What happened**: second\n",
        );
        let k = read(fx.root());
        let base = fingerprint("learning", "2026-01-01", "Same");
        // Newest (later in file) first; the first-written keeps the bare id.
        assert_eq!(k.learnings[0].what, "second");
        assert_eq!(k.learnings[0].fp, format!("{base}~2"));
        assert_eq!(k.learnings[1].fp, base);
    }

    #[test]
    fn caps_cut_text_on_char_boundaries() {
        let capped = cap_text("é".repeat(2000));
        assert!(capped.len() <= MAX_TEXT_BYTES);
        assert!(capped.ends_with('…'));
        assert!(capped.trim_end_matches('…').chars().all(|c| c == 'é'));
        assert_eq!(cap_text("short".to_owned()), "short");

        let fx = Fixture::new("caps");
        let tags: Vec<String> = (0..30).map(|i| format!("t{i}")).collect();
        fx.write(
            ".living/decisions.md",
            &format!(
                "### [2026-01-01] Long\n**Context**: {}\n**Tags**: {}\n",
                "日".repeat(1000),
                tags.join(", ")
            ),
        );
        let d = &read(fx.root()).decisions[0];
        assert!(d.context.len() <= MAX_TEXT_BYTES && d.context.ends_with('…'));
        assert!(d.context.trim_end_matches('…').chars().all(|c| c == '日'));
        assert_eq!(d.tags.len(), MAX_TAGS);
        assert_eq!(d.tags[19], "t19");
    }

    #[test]
    fn stamp_changes_when_a_file_changes_or_appears() {
        let fx = project();
        let first = stamp(fx.root());
        assert_eq!(first, stamp(fx.root()));
        assert!(first
            .files
            .iter()
            .any(|(rel, ..)| rel == ".mycelium/last-session.md"));

        let path = fx.root().join(".living/learnings.md");
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str("\n### [2026-09-20] Appended\n**Category**: tip\n");
        std::fs::write(&path, body).unwrap();
        let appended = stamp(fx.root());
        assert_ne!(first, appended);
        assert_eq!(read(fx.root()).learnings[0].title, "Appended");

        fx.write(
            ".living/findings/new-topic.md",
            "## F-010: New\n**Status:** preliminary\n",
        );
        assert_ne!(appended, stamp(fx.root()));
    }

    #[test]
    fn an_in_flight_run_handoff_is_the_fallback() {
        let fx = Fixture::new("run-handoff");
        let body = "## What was worked on\n- {}\n\n\
                    ## Current state\n- Stop hook finalization pending\n- Loader fixed\n";
        fx.write(".living/learnings.md", "")
            .write(
                ".mycelium/run/claude/sess-a/last-session.md",
                &body.replace("{}", "older session"),
            )
            .write(
                ".mycelium/run/codex/sess-b/last-session.md",
                &body.replace("{}", "newer session"),
            )
            .write(
                ".mycelium/run/codex/bad name/last-session.md",
                &body.replace("{}", "not a mycelium run dir"),
            );
        fx.set_mtime(".mycelium/run/claude/sess-a/last-session.md", 600);
        fx.set_mtime(".mycelium/run/codex/sess-b/last-session.md", 60);
        fx.set_mtime(".mycelium/run/codex/bad name/last-session.md", 0);

        let left = read(fx.root()).left_off.expect("run handoff");
        assert_eq!(left.worked_on, "- newer session");
        assert_eq!(left.session_id.as_deref(), Some("sess-b"));
        assert_eq!(left.host.as_deref(), Some("codex"));
        assert_eq!(left.path, ".mycelium/run/codex/sess-b/last-session.md");
        // mycelium's finalizer strips lifecycle chatter; so do we.
        assert_eq!(left.current, "- Loader fixed");

        // An accepted shared handoff always wins.
        fx.write(".mycelium/last-session.md", LAST_SESSION);
        let left = read(fx.root()).left_off.unwrap();
        assert_eq!(left.path, ".mycelium/last-session.md");
        assert_eq!(left.session_id, None);
    }

    #[test]
    fn the_alternate_handoff_schema_and_fallback_filler() {
        let fx = Fixture::new("alt-handoff");
        fx.write(".living/decisions.md", "").write(
            ".mycelium/last-session.md",
            "## Current State\nParser half done.\n\n\
             ## What Was Done\n\
             - Completed the session work recorded in the finalized session log.\n\
             - Fixed the loader\n\n\
             ## Key Decisions\n\
             - See `.living/decisions.md` for decisions recorded during this session.\n\n\
             ## Next Steps\nShip the loader fix, then rerun QC.\n\n\
             ## Relevant Files\n- src/loader.py\n",
        );
        let left = read(fx.root()).left_off.unwrap();
        assert_eq!(left.current, "Parser half done.");
        assert_eq!(left.worked_on, "- Fixed the loader");
        assert_eq!(left.decisions, "");
        assert!(left.blockers.is_empty());
        assert_eq!(left.next, ["Ship the loader fix, then rerun QC."]);

        // Nothing but filler is no handoff at all.
        fx.write(
            ".mycelium/last-session.md",
            "## Next steps\n\
             - Review the finalized session log and continue from the current branch state.\n",
        );
        let k = read(fx.root());
        assert!(k.left_off.is_none());
        assert!(k.warnings[0].contains("no handoff sections with content"));
    }

    #[test]
    fn the_legacy_todolist_is_read_with_a_warning() {
        let fx = Fixture::new("todolist");
        fx.write("MYCELIUM.md", "").write(
            "todo/TODOLIST.md",
            "# Todo List\n\n## Items\n\n\
             <!-- Add todo items below. Link to detailed writeups as needed. -->\n\
             - [ ] Compare with GTEx ([notes](gtex.md))\n- [x] Draft figure 2\n- Plain item\n",
        );
        let k = read(fx.root());
        let got: Vec<(&str, &str, &str)> = k
            .todos
            .iter()
            .map(|t| (t.item.as_str(), t.status.as_str(), t.file.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("Compare with GTEx (notes)", "open", "todo/gtex.md"),
                ("Draft figure 2", "complete", ""),
                ("Plain item", "open", ""),
            ]
        );
        assert_eq!(k.counts.open, 2);
        assert!(k.warnings[0].contains("legacy todo list"));
    }

    #[test]
    fn the_ledger_tolerates_reordered_and_missing_columns() {
        let rows = parse_ledger(&[
            "| Direction | Date | Result |",
            "|---|---|---|",
            "| Refines the claim | 2026-01-02 | narrower |",
            "| supports | 2026-01-03 |",
            "| contradicts |",
        ])
        .rows;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].direction, "refines");
        assert_eq!(rows[0].date, "2026-01-02");
        assert_eq!(rows[0].result, "narrower");
        assert_eq!(rows[0].run, "");
        assert_eq!(rows[1].direction, "supports");
        assert_eq!(rows[1].result, "");
        assert_eq!(
            (rows[2].date.as_str(), rows[2].direction.as_str()),
            ("", "contradicts")
        );
        // Positional fallback without a header; the template row drops.
        let rows = parse_ledger(&[
            "| 2026-02-01 | r1 | d | p | ok | contradicts |",
            "| YYYY-MM-DD | x |",
        ])
        .rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run, "r1");
        assert_eq!(rows[0].direction, "contradicts");
    }

    #[test]
    fn a_long_ledger_keeps_the_newest_rows_but_dates_from_all() {
        let lines: Vec<String> = (1..=60)
            .map(|n| {
                let date = if n == 3 { "2027-06-01" } else { "2026-01-01" };
                format!("| {date} | r{n} | d | p | ok | supports |")
            })
            .collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let ledger = parse_ledger(&lines);
        assert_eq!(ledger.total, 60);
        assert_eq!(ledger.rows.len(), MAX_LEDGER_ROWS);
        assert_eq!(ledger.rows[0].run, "r11");
        assert_eq!(ledger.rows[MAX_LEDGER_ROWS - 1].run, "r60");
        // The newest date lives on a row that was not kept.
        assert_eq!(ledger.newest.as_deref(), Some("2027-06-01"));
    }

    #[test]
    fn the_fence_port_matches_markdown_fences() {
        let four = opening_fence("````").unwrap();
        assert!(!closes("```", &four), "a shorter run is content");
        assert!(closes("````", &four) && closes("`````  ", &four));
        assert!(!closes("~~~~", &four), "the other fence character");
        assert!(!closes("```` trailing", &four));

        assert!(
            opening_fence("    ```").is_none(),
            "four spaces is indented code"
        );
        assert!(opening_fence("\t```").is_none(), "a tab reaches column 4");
        assert!(opening_fence("```rust").is_some());
        assert!(opening_fence("``` a`b").is_none(), "backtick info string");
        assert!(opening_fence("~~~ a`b").is_some());
        assert!(opening_fence("``").is_none());

        let quoted = opening_fence("> ```").unwrap();
        assert!(!closes("```", &quoted) && closes("> ```", &quoted));

        let listed = opening_fence("1. ~~~").unwrap();
        assert_eq!(listed.column, 3);
        assert!(closes("~~~", &listed) && closes("      ~~~", &listed));
        assert!(
            !closes("       ~~~", &listed),
            "past the three-column slack"
        );
        assert!(!closes("\t\t~~~", &listed));
    }

    #[test]
    fn todo_links_resolve_inside_the_workspace() {
        assert_eq!(todo_path("item.md"), "todo/item.md");
        assert_eq!(todo_path("./sub/item.md#notes"), "todo/sub/item.md");
        assert_eq!(todo_path("../analysis/qc/QC.md"), "analysis/qc/QC.md");
        assert_eq!(todo_path("../../etc/passwd"), "../../etc/passwd");
        assert_eq!(todo_path("https://example.org/x"), "https://example.org/x");
        assert_eq!(todo_path("/abs/path.md"), "/abs/path.md");
        assert_eq!(
            strip_links("see ![fig](f.png) and [a](b.md)"),
            ("see fig and a".to_owned(), Some("f.png".to_owned()))
        );
    }

    #[test]
    fn an_unclosed_comment_warns_but_a_code_span_opener_is_text() {
        let fx = Fixture::new("comments");
        fx.write(
            ".living/learnings.md",
            "### [2026-01-01] About `<!--` in markdown\n**What happened**: see title\n\n\
             ### [2026-01-02] Still visible\n**What happened**: yes\n\n\
             <!-- a note that never closes\n### [2026-01-03] Hidden\n",
        );
        let k = read(fx.root());
        let titles: Vec<&str> = k.learnings.iter().map(|l| l.title.as_str()).collect();
        assert_eq!(titles, ["Still visible", "About `<!--` in markdown"]);
        assert_eq!(
            k.warnings,
            [".living/learnings.md: unclosed HTML comment at line 7; nothing after it was read as knowledge"]
        );
    }

    #[test]
    fn the_finding_cap_spends_topics_in_slug_order_and_skips_the_rest() {
        let topic = |first: usize, n: usize| {
            (first..first + n)
                .map(|id| format!("## F-{id:03}: claim {id}\n**Status:** supported\n\n"))
                .collect::<String>()
        };
        let fx = Fixture::new("finding-cap");
        fx.write(".living/findings/b.md", &topic(301, 300))
            .write(".living/findings/a.md", &topic(1, 300))
            .write(".living/findings/c.md", &topic(601, 10));
        let k = read(fx.root());
        let per_topic: Vec<(&str, usize)> = k
            .topics
            .iter()
            .map(|t| (t.slug.as_str(), t.findings.len()))
            .collect();
        assert_eq!(per_topic, [("a", 300), ("b", 100)]);
        assert_eq!(k.topics[1].findings[99].id, "F-400");
        assert_eq!(k.counts.findings, 400);
        assert_eq!(
            k.warnings,
            ["findings: showing the first 400 of 600 (1 more topic files not read)"]
        );
    }

    #[test]
    fn a_log_of_bare_headings_keeps_only_the_newest() {
        let log: String = (0..MAX_SCANNED_ENTRIES + 3)
            .map(|i| format!("### [2026-01-01] E{i}\n"))
            .collect();
        let fx = Fixture::new("scan-cap");
        fx.write(".living/decisions.md", &log);
        let k = read(fx.root());
        assert_eq!(k.decisions.len(), MAX_ENTRIES);
        // Same date: later in the file is newer.
        assert_eq!(
            k.decisions[0].title,
            format!("E{}", MAX_SCANNED_ENTRIES + 2)
        );
        assert_eq!(
            k.warnings,
            [
                format!(
                    ".living/decisions.md: {} entry headings; only the last {MAX_SCANNED_ENTRIES} were considered",
                    MAX_SCANNED_ENTRIES + 3
                ),
                format!(
                    ".living/decisions.md: showing the newest {MAX_ENTRIES} of {MAX_SCANNED_ENTRIES} entries"
                ),
            ]
        );
    }

    #[test]
    fn the_per_read_byte_budget_drops_topic_files_first() {
        let fx = Fixture::new("byte-budget");
        let filler = "x".repeat(1900 * 1024);
        fx.write("todo/TODO_REGISTRY.md", TODO_REGISTRY);
        for n in 0..9 {
            fx.write(
                &format!(".living/findings/t{n}.md"),
                &format!("## F-{n}: claim\n**Status:** robust\n\n{filler}\n"),
            );
        }
        let k = read(fx.root());
        // 8 × ~1.9 MiB fit in 16 MiB; the ninth doesn't. Todos, planned
        // before topics, are never the casualty.
        assert_eq!(k.topics.len(), 8);
        assert_eq!(k.todos.len(), 4);
        assert_eq!(
            k.warnings,
            ["1 knowledge files not read: the 16 MiB per-read budget was spent"]
        );
    }

    #[test]
    fn stale_stop_chatter_matches_the_finalizer() {
        assert!(is_stale_stop_line("- Stop hook finalization pending"));
        assert!(is_stale_stop_line("Natural stop not yet reached"));
        assert!(is_stale_stop_line("attempting a natural stop"));
        assert!(!is_stale_stop_line(
            "- Stop the Slurm array before rerunning"
        ));
        assert!(!is_stale_stop_line("Pending: batch 3 FASTQs"));
        // Linear on a hostile line: many leads, no tail.
        assert!(!is_stale_stop_line(&"stop hook ".repeat(200_000)));
    }
}
