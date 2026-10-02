//! Session history: one lasting record per agent session (plan
//! `docs/design/git-and-session-history-plan.md` §7–§10). Who started it, what it
//! changed, what it cost, and where its transcript is — with or without git.
//!
//! Durable state is one append-only JSONL per workspace,
//! `<data_dir>/workspace/<ws>/sessions.jsonl`, beside the Timeline. A record
//! is two lines sharing a `rid`: an `open` line when the session starts and a
//! `close` line (the whole record) when it ends; a reader merges them, the
//! close winning. The Mastermind's actions and note deliveries append `act`
//! lines to the same file. Past `FILE_MAX` the file compacts: the oldest
//! closed records fold into one `month` totals line per month (sessions, and
//! cost and tokens per agent and model), so totals survive indefinitely at a
//! few hundred bytes a month while details stay for the last few thousand
//! sessions.
//!
//! Only open records live in memory (bounded by live sessions). Their state
//! is checkpointed to `<data_dir>/history-open.json` (small, rewritten
//! atomically, debounced) so a daemon that dies without a graceful stop still
//! closes them — with the last checkpointed usage — on the next boot, before
//! the ledger resurrects their sessions under new records (`started_by:
//! "restart"`). A graceful stop closes every open record itself.
//!
//! No fs on the reactor: every write goes through ONE writer thread (file
//! order == call order), every read through `spawn_blocking`.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agent_state::{AgentKind, AgentRecord};
use crate::AppState;
use chimaera_agent::model::{AgentEvent, SessionUi};

pub(crate) mod edits;
pub(crate) mod routes;
pub(crate) mod usage;

/// Compaction threshold, and the detail that survives one.
const FILE_MAX: u64 = 4 * 1024 * 1024;
const COMPACT_KEEP: u64 = 2 * 1024 * 1024;
/// A line longer than this is refused (field caps keep real records near
/// 1 KiB).
const LINE_MAX: usize = 8 * 1024;
const TITLE_MAX: usize = 200;
const PROMPT_MAX: usize = 300;
const PATH_MAX: usize = 200;
const TOP_FILES: usize = 10;
const MODELS_MAX: usize = 4;
const MODEL_MAX: usize = 80;
/// Per open record: file-touch times (the same-file warning), pairs already
/// told, distinct paths counted.
const TOUCHED_MAX: usize = 100;
const TOLD_MAX: usize = 256;
const DISTINCT_MAX: usize = 1000;
/// Conversation id -> the agent's last running totals, for the baseline a
/// resumed conversation subtracts.
const PRIORS_MAX: usize = 256;
const ACT_DETAIL_MAX: usize = 200;
/// At most this many same-file lines ride one hook answer.
const SAME_FILE_LINES_MAX: usize = 3;
/// Checkpoint cadence for open records whose usage moved.
const CHECKPOINT_EVERY: Duration = Duration::from_secs(30);
/// Acts kept through a compaction.
const ACTS_KEPT: usize = 500;
/// Recently closed records kept in memory, so the git tracker's late answer
/// (the commits run git after the session ended) can still amend them.
const RECENT_CLOSED_MAX: usize = 64;
/// Commits a record carries (sha + subject); `commits_n` keeps the count.
const RECORD_COMMITS_MAX: usize = 20;
const COMMIT_SUBJECT_MAX: usize = 100;

/// Who started a session. The wire form is a plain string: `you`,
/// `mastermind`, `restart`, or another session's id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartedBy {
    /// The user: the launcher, Recents, All sessions, a plugin's setup
    /// button, appointing a Mastermind.
    You,
    /// The workspace Mastermind's `spawn_agent`.
    Mastermind,
    /// Another session: a fork (branch) of that session's conversation.
    Session(String),
    /// Brought back by a daemon restart (the ledger's resurrection).
    Restart,
}

impl StartedBy {
    pub(crate) fn wire(&self) -> String {
        match self {
            StartedBy::You => "you".into(),
            StartedBy::Mastermind => "mastermind".into(),
            StartedBy::Restart => "restart".into(),
            StartedBy::Session(id) => id.clone(),
        }
    }
}

/// How a session ended.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Outcome {
    /// The agent quit, or the user closed it.
    Exited,
    /// The agent's process failed (a chat driver's protocol error or a
    /// non-zero exit) and did not recover.
    Crashed,
    /// The daemon ended the record, not the session: a daemon stop or
    /// restart (a resurrected session continues under a new record).
    Retired,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Files {
    /// Distinct files the session wrote (counted up to `DISTINCT_MAX`).
    pub(crate) n: usize,
    /// The most recently written, newest first, workspace-relative when
    /// under the root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) top: Vec<String>,
}

/// What a session used. `None` = the agent reports nothing for it (a codex
/// TUI has no telemetry; codex chats report tokens, not cost) — never zero.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Usage {
    /// Estimated at API prices (what the agent reports), not what a
    /// subscriber pays.
    pub(crate) cost_usd: Option<f64>,
    pub(crate) tokens_in: Option<u64>,
    pub(crate) tokens_out: Option<u64>,
    pub(crate) turns: Option<u32>,
}

/// The agent's own running totals at close — claude reports its cost as a
/// session total, codex its thread's tokens — kept so a later resume of the
/// same conversation can subtract what an earlier record already counted.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Totals {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tin: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tout: Option<f64>,
}

/// Where the conversation lives: `kind` is `chat` (chimaera's journal,
/// `journal` = its id), `claude` (claude's transcript, `path`), or the agent
/// name for a TUI whose store chimaera never reads (codex's rollouts).
/// `native` is the agent's own conversation id — the resume handle.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Transcript {
    pub(crate) kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) journal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) native: Option<String>,
}

/// One session record: the `open` line carries its opening fields, the
/// `close` line the whole record. Additive — the wire is a stable interface.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Record {
    /// Record id: `<session id>@<started ms>` (a resurrected session keeps
    /// its id, so the id alone is not unique across records).
    pub(crate) rid: String,
    pub(crate) id: String,
    pub(crate) agent: String,
    /// The surface it last ran on: `chat` | `term`.
    pub(crate) ui: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) first_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) models: Vec<String>,
    pub(crate) started_by: String,
    /// ms since the Unix epoch.
    pub(crate) started: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ended: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) outcome: Option<Outcome>,
    #[serde(default)]
    pub(crate) files: Files,
    #[serde(default)]
    pub(crate) usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) transcript: Option<Transcript>,
    /// Git anchors `{start, current, commits[]}` in a repository; null
    /// elsewhere (see [`git_field`]).
    #[serde(default)]
    pub(crate) git: Option<serde_json::Value>,
    /// This was its workspace's Mastermind.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) mastermind: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) totals: Option<Totals>,
}

/// A Mastermind action or a note delivery (`act` line).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Act {
    pub(crate) ts: u64,
    /// `you`, or the acting session's id.
    pub(crate) by: String,
    pub(crate) act: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

/// Folded totals for one month's compacted records (UTC month).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Month {
    pub(crate) month: String,
    pub(crate) sessions: u64,
    #[serde(default)]
    pub(crate) rows: Vec<MonthRow>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct MonthRow {
    pub(crate) agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    pub(crate) sessions: u64,
    /// Summed over the sessions whose cost is known (`cost_sessions`).
    pub(crate) cost_usd: f64,
    pub(crate) cost_sessions: u64,
    pub(crate) tokens_in: u64,
    pub(crate) tokens_out: u64,
    pub(crate) token_sessions: u64,
    /// Time the sessions ran (the sum of their durations), ms.
    #[serde(default)]
    pub(crate) duration_ms: u64,
}

/// One line of `sessions.jsonl`. `t` is serialized first.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "t", rename_all = "lowercase")]
pub(crate) enum Line {
    Open(Record),
    Close(Record),
    Act(Act),
    Month(Month),
    /// A line kind written by a newer daemon — skipped, kept on compaction.
    #[serde(other)]
    Unknown,
}

/// A running total the agent reports (claude's session cost, codex's thread
/// tokens, a claude TUI's statusline tokens), turned into what THIS record
/// spent: the sum of its increases. A drop means the counter restarted (a
/// new process after a view switch that did not restore it), so the new
/// value counts whole. The first value is the baseline when it was seen
/// before any turn (a TUI's statusline paints at start, restored or zero);
/// otherwise the conversation's previous total (`prior`) is subtracted when
/// the new value is at or above it — claude carries its total across a
/// resume.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Running {
    spent: f64,
    last: Option<f64>,
}

impl Running {
    pub(crate) fn see(&mut self, value: f64, pre_turn: bool, prior: Option<f64>) {
        if !value.is_finite() || value < 0.0 {
            return;
        }
        let delta = match self.last {
            None if pre_turn => 0.0,
            None => match prior {
                Some(p) if value >= p => value - p,
                _ => value,
            },
            Some(last) if value >= last => value - last,
            Some(_) => value,
        };
        self.spent += delta;
        self.last = Some(value);
    }

    pub(crate) fn value(&self) -> Option<f64> {
        self.last.map(|_| self.spent)
    }

    pub(crate) fn last(&self) -> Option<f64> {
        self.last
    }
}

/// An open record's in-memory state.
struct Live {
    ws: String,
    rec: Record,
    /// The native conversation this record continues (a resume or a
    /// restart), for the running-total baseline.
    conv: Option<String>,
    cost: Running,
    tin: Running,
    tout: Running,
    /// Claude chat reports tokens per turn: summed.
    tin_sum: u64,
    tout_sum: u64,
    tokens_summed: bool,
    turns: u32,
    /// Whether turns are observable for this record (not a hook-less TUI).
    turns_known: bool,
    crashed: bool,
    was_chat: bool,
    /// The agent's own conversation id (a chat's Init) and claude's
    /// transcript (its hooks), so a checkpointed record still says where
    /// the conversation lives when a boot has to close it.
    native: Option<String>,
    transcript_path: Option<String>,
    /// path -> last write (ms), newest last.
    touched: VecDeque<(String, u64)>,
    distinct: HashSet<u64>,
    told: HashSet<(String, String)>,
    orphan_seen: u8,
    /// The session's git story, once the git tracker handed it over
    /// ([`attach_git`]); an open record that closes later carries it.
    git: Option<serde_json::Value>,
}

impl Live {
    fn touch(&mut self, path: &str, now: u64) {
        if let Some(pos) = self.touched.iter().position(|(p, _)| p == path) {
            self.touched.remove(pos);
        }
        self.touched.push_back((path.to_string(), now));
        while self.touched.len() > TOUCHED_MAX {
            self.touched.pop_front();
        }
        if self.distinct.len() < DISTINCT_MAX {
            self.distinct.insert(hash_path(path));
        }
    }

    fn see_model(&mut self, model: &str) {
        let model = model.trim();
        if model.is_empty() {
            return;
        }
        let model: String = model.chars().take(MODEL_MAX).collect();
        if self.rec.models.last() == Some(&model) {
            return;
        }
        self.rec.models.retain(|m| m != &model);
        self.rec.models.push(model);
        while self.rec.models.len() > MODELS_MAX {
            self.rec.models.remove(0);
        }
    }

    fn usage(&self) -> Usage {
        let (tin, tout) = if self.tokens_summed {
            (Some(self.tin_sum), Some(self.tout_sum))
        } else {
            (
                self.tin.value().map(|v| v.round() as u64),
                self.tout.value().map(|v| v.round() as u64),
            )
        };
        Usage {
            cost_usd: self.cost.value().map(round_usd),
            tokens_in: tin,
            tokens_out: tout,
            turns: self.turns_known.then_some(self.turns),
        }
    }

    fn totals(&self) -> Option<Totals> {
        let t = Totals {
            cost: self.cost.last(),
            tin: self.tin.last(),
            tout: self.tout.last(),
        };
        (t != Totals::default()).then_some(t)
    }

    /// Where the conversation lives, from what the open record has seen.
    fn pointer(&self) -> Transcript {
        let native = self.native.clone().or_else(|| {
            self.transcript_path
                .as_deref()
                .and_then(|p| Path::new(p).file_stem())
                .map(|s| s.to_string_lossy().into_owned())
        });
        let native = native.or_else(|| self.conv.clone());
        if self.was_chat {
            Transcript {
                kind: "chat".into(),
                journal: Some(self.rec.id.clone()),
                path: self.transcript_path.clone(),
                native,
            }
        } else if self.rec.agent == AgentKind::Claude.as_str() {
            Transcript {
                kind: "claude".into(),
                journal: None,
                path: self.transcript_path.clone(),
                native,
            }
        } else {
            Transcript {
                kind: self.rec.agent.clone(),
                journal: None,
                path: None,
                native,
            }
        }
    }

    /// The record as it stands (the checkpoint and the list's live rows).
    fn snapshot(&self) -> Record {
        let mut rec = self.rec.clone();
        rec.transcript = Some(self.pointer());
        rec.usage = self.usage();
        rec.totals = self.totals();
        rec.files.n = self.distinct.len();
        rec.files.top = self
            .touched
            .iter()
            .rev()
            .take(TOP_FILES)
            .map(|(p, _)| cap(p, PATH_MAX))
            .collect();
        rec
    }
}

/// Dollars to a hundredth of a cent (the agents report sub-cent turns).
fn round_usd(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn hash_path(path: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

enum Op {
    Append { path: PathBuf, line: String },
    Index { body: String },
    Remove { dir: PathBuf },
    Flush(mpsc::Sender<()>),
}

/// The per-daemon half: open records, the writer, and small caches.
pub(crate) struct HistoryService {
    root: PathBuf,
    index_path: PathBuf,
    live: Mutex<HashMap<String, Live>>,
    removed: Mutex<HashSet<String>>,
    priors: Mutex<VecDeque<(String, Totals)>>,
    /// An open record's usage moved since the last checkpoint.
    dirty: AtomicBool,
    /// Bumped on every close and act (clients refetch on the recents nudge;
    /// tests wait on it).
    epoch: AtomicU64,
    writer: Mutex<Option<mpsc::Sender<Op>>>,
    started: AtomicBool,
    pub(crate) usage_cache: Mutex<usage::Cache>,
    /// (session id, workspace, closed record), newest last.
    recent_closed: Mutex<VecDeque<(String, String, Record)>>,
}

impl HistoryService {
    pub(crate) fn new(data_dir: &Path) -> Self {
        HistoryService {
            root: data_dir.join("workspace"),
            index_path: data_dir.join("history-open.json"),
            live: Mutex::new(HashMap::new()),
            removed: Mutex::new(HashSet::new()),
            priors: Mutex::new(VecDeque::new()),
            dirty: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            writer: Mutex::new(None),
            started: AtomicBool::new(false),
            usage_cache: Mutex::new(usage::Cache::default()),
            recent_closed: Mutex::new(VecDeque::new()),
        }
    }

    pub(crate) fn path(&self, ws: &str) -> PathBuf {
        self.root.join(sanitize(ws)).join("sessions.jsonl")
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    fn append(&self, ws: &str, line: &Line) {
        if crate::lock(&self.removed).contains(ws) {
            return;
        }
        let Ok(text) = serde_json::to_string(line) else {
            return;
        };
        if text.len() > LINE_MAX {
            tracing::warn!(ws, "session history line over the cap; dropped");
            return;
        }
        self.send(Op::Append {
            path: self.path(ws),
            line: text,
        });
    }

    fn send(&self, op: Op) {
        let mut writer = crate::lock(&self.writer);
        if writer.is_none() {
            let (tx, rx) = mpsc::channel::<Op>();
            let index_path = self.index_path.clone();
            let spawned = std::thread::Builder::new()
                .name("history-writer".into())
                .spawn(move || writer_loop(rx, index_path));
            if spawned.is_err() {
                tracing::error!("could not start the session history writer thread");
                return;
            }
            *writer = Some(tx);
        }
        if let Some(tx) = writer.as_ref() {
            let _ = tx.send(op);
        }
    }

    /// Block (off the reactor, or at shutdown) until every queued write hit
    /// the disk, at most `timeout`.
    pub(crate) fn flush(&self, timeout: Duration) {
        let (tx, rx) = mpsc::channel();
        self.send(Op::Flush(tx));
        let _ = rx.recv_timeout(timeout);
    }

    /// Rewrite the open-records checkpoint from memory.
    pub(crate) fn checkpoint(&self) {
        self.dirty.store(false, Ordering::Relaxed);
        let records: Vec<serde_json::Value> = crate::lock(&self.live)
            .values()
            .map(|l| {
                serde_json::json!({
                    "ws": l.ws,
                    "conv": l.conv,
                    "record": l.snapshot(),
                })
            })
            .collect();
        let body = serde_json::json!({
            "saved_at": now_ms(),
            "records": records,
        })
        .to_string();
        self.send(Op::Index { body });
    }

    fn prior(&self, conv: &str) -> Option<Totals> {
        crate::lock(&self.priors)
            .iter()
            .rev()
            .find(|(c, _)| c == conv)
            .map(|(_, t)| *t)
    }

    fn remember_prior(&self, conv: &str, totals: Totals) {
        let mut priors = crate::lock(&self.priors);
        priors.retain(|(c, _)| c != conv);
        priors.push_back((conv.to_string(), totals));
        while priors.len() > PRIORS_MAX {
            priors.pop_front();
        }
    }

    /// Forget a deleted workspace: its open records, then its file (behind
    /// any queued appends, so nothing resurrects it).
    pub(crate) fn remove_workspace(&self, ws: &str) {
        crate::lock(&self.removed).insert(ws.to_string());
        crate::lock(&self.live).retain(|_, l| l.ws != ws);
        crate::lock(&self.usage_cache).forget(ws);
        self.send(Op::Remove {
            dir: self.root.join(sanitize(ws)),
        });
        self.checkpoint();
    }

    /// Take an open record out of memory (it is about to be closed).
    fn take(&self, sid: &str) -> Option<Live> {
        crate::lock(&self.live).remove(sid)
    }

    /// Append the close line and remember the conversation's totals.
    fn finish(&self, live: Live, rec: Record) {
        if let (Some(conv), Some(totals)) = (
            rec.transcript.as_ref().and_then(|t| t.native.clone()),
            rec.totals,
        ) {
            self.remember_prior(&conv, totals);
        }
        self.append(&live.ws, &Line::Close(rec.clone()));
        {
            let mut recent = crate::lock(&self.recent_closed);
            recent.retain(|(sid, _, _)| *sid != rec.id);
            if recent.len() >= RECENT_CLOSED_MAX {
                recent.pop_front();
            }
            recent.push_back((rec.id.clone(), live.ws.clone(), rec));
        }
        crate::lock(&self.usage_cache).forget(&live.ws);
        self.epoch.fetch_add(1, Ordering::Relaxed);
        self.checkpoint();
    }
}

fn writer_loop(rx: mpsc::Receiver<Op>, index_path: PathBuf) {
    while let Ok(op) = rx.recv() {
        match op {
            Op::Append { path, line } => {
                if let Err(err) = append_line(&path, &line) {
                    tracing::warn!(%err, path = %path.display(), "session history append failed");
                    continue;
                }
                if std::fs::metadata(&path).is_ok_and(|m| m.len() > FILE_MAX) {
                    if let Err(err) = compact(&path, COMPACT_KEEP) {
                        tracing::warn!(%err, "session history compaction failed");
                    }
                }
            }
            Op::Index { body } => {
                if let Err(err) = crate::persist::atomic_write_json(&index_path, body) {
                    tracing::warn!(%err, "session history checkpoint failed");
                }
            }
            Op::Remove { dir } => {
                let _ = std::fs::remove_file(dir.join("sessions.jsonl"));
                // The Timeline removes the directory itself; ours may have
                // re-created it with a racing append — an empty one goes.
                let _ = std::fs::remove_dir(&dir);
            }
            Op::Flush(ack) => {
                let _ = ack.send(());
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
    // A torn tail (a crash mid-write) must not swallow the next record.
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

/// Read and parse a workspace's history file (bounded by `FILE_MAX` plus a
/// write or two). Unparseable lines — a torn tail, a hand edit — are skipped.
/// BLOCKING: call off the reactor.
pub(crate) fn read_lines(path: &Path) -> Vec<Line> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    // Never more than twice the cap, whatever a hand edit did to the file.
    if file.take(FILE_MAX * 2).read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    String::from_utf8_lossy(&buf)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Line>(l).ok())
        .collect()
}

/// Records merged by `rid` (the close line wins), in file order of their
/// first line; plus the acts and the month totals.
#[derive(Default)]
pub(crate) struct Parsed {
    pub(crate) records: Vec<Record>,
    pub(crate) acts: Vec<Act>,
    pub(crate) months: Vec<Month>,
}

pub(crate) fn merge(lines: Vec<Line>) -> Parsed {
    let mut out = Parsed::default();
    let mut at: HashMap<String, usize> = HashMap::new();
    for line in lines {
        match line {
            Line::Open(rec) => {
                if !at.contains_key(&rec.rid) {
                    at.insert(rec.rid.clone(), out.records.len());
                    out.records.push(rec);
                }
            }
            Line::Close(rec) => match at.get(&rec.rid) {
                Some(&i) => out.records[i] = rec,
                None => {
                    at.insert(rec.rid.clone(), out.records.len());
                    out.records.push(rec);
                }
            },
            Line::Act(act) => out.acts.push(act),
            Line::Month(m) => match out.months.iter_mut().find(|x| x.month == m.month) {
                Some(existing) => fold_month(existing, &m),
                None => out.months.push(m),
            },
            Line::Unknown => {}
        }
    }
    out
}

fn fold_month(into: &mut Month, from: &Month) {
    into.sessions += from.sessions;
    for row in &from.rows {
        match into
            .rows
            .iter_mut()
            .find(|r| r.agent == row.agent && r.model == row.model)
        {
            Some(r) => {
                r.sessions += row.sessions;
                r.cost_usd += row.cost_usd;
                r.cost_sessions += row.cost_sessions;
                r.tokens_in += row.tokens_in;
                r.tokens_out += row.tokens_out;
                r.token_sessions += row.token_sessions;
                r.duration_ms += row.duration_ms;
            }
            None => into.rows.push(row.clone()),
        }
    }
}

/// The model a record is counted under: the one it ran last.
pub(crate) fn primary_model(rec: &Record) -> Option<String> {
    rec.models.last().cloned()
}

/// Fold one closed record into its month's totals.
fn fold_record(months: &mut BTreeMap<String, Month>, rec: &Record) {
    let key = usage::month_of(rec.started, 0);
    let month = months.entry(key.clone()).or_insert_with(|| Month {
        month: key,
        sessions: 0,
        rows: Vec::new(),
    });
    month.sessions += 1;
    let model = primary_model(rec);
    let row = match month
        .rows
        .iter_mut()
        .position(|r| r.agent == rec.agent && r.model == model)
    {
        Some(i) => &mut month.rows[i],
        None => {
            month.rows.push(MonthRow {
                agent: rec.agent.clone(),
                model,
                ..MonthRow::default()
            });
            month.rows.last_mut().expect("just pushed")
        }
    };
    row.sessions += 1;
    row.duration_ms += rec.ended.map_or(0, |e| e.saturating_sub(rec.started));
    if let Some(c) = rec.usage.cost_usd {
        row.cost_usd += c;
        row.cost_sessions += 1;
    }
    if rec.usage.tokens_in.is_some() || rec.usage.tokens_out.is_some() {
        row.tokens_in += rec.usage.tokens_in.unwrap_or(0);
        row.tokens_out += rec.usage.tokens_out.unwrap_or(0);
        row.token_sessions += 1;
    }
}

/// Compact past the cap: keep the newest records (and every still-open one)
/// worth about `keep` bytes, fold the rest into month totals, keep the acts
/// no older than the oldest kept record (at most `ACTS_KEPT`). Temp file +
/// rename, so a crash mid-compaction leaves the old file intact.
fn compact(path: &Path, keep: u64) -> std::io::Result<()> {
    let mut buf = Vec::new();
    std::fs::File::open(path)?
        .take(FILE_MAX * 2)
        .read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    struct Entry<'a> {
        raw: &'a str,
        line: Line,
    }
    let entries: Vec<Entry> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|raw| {
            serde_json::from_str::<Line>(raw)
                .ok()
                .map(|line| Entry { raw, line })
        })
        .collect();
    // Per rid: its lines' byte cost, whether it is closed, the merged record,
    // and the index of its newest line (recency).
    struct Rid {
        bytes: u64,
        closed: bool,
        rec: Record,
        newest: usize,
    }
    let mut rids: HashMap<String, Rid> = HashMap::new();
    let mut months: BTreeMap<String, Month> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        match &e.line {
            Line::Open(rec) | Line::Close(rec) => {
                let closed = matches!(e.line, Line::Close(_));
                let slot = rids.entry(rec.rid.clone()).or_insert_with(|| Rid {
                    bytes: 0,
                    closed: false,
                    rec: rec.clone(),
                    newest: i,
                });
                slot.bytes += e.raw.len() as u64 + 1;
                slot.newest = i;
                if closed {
                    slot.closed = true;
                    slot.rec = rec.clone();
                }
            }
            Line::Month(m) => {
                let into = months.entry(m.month.clone()).or_insert_with(|| Month {
                    month: m.month.clone(),
                    ..Month::default()
                });
                fold_month(into, m);
            }
            Line::Act(_) | Line::Unknown => {}
        }
    }
    let mut order: Vec<(&String, &Rid)> = rids.iter().collect();
    order.sort_by_key(|(_, r)| std::cmp::Reverse(r.newest));
    let mut kept: HashSet<&str> = HashSet::new();
    let mut used = 0u64;
    let mut oldest_kept = u64::MAX;
    for (rid, r) in &order {
        if !r.closed || used + r.bytes <= keep {
            used += r.bytes;
            kept.insert(rid.as_str());
            oldest_kept = oldest_kept.min(r.rec.started);
        } else {
            fold_record(&mut months, &r.rec);
        }
    }
    // Acts as old as the oldest kept record stay (all of them when no
    // record is kept), the newest `ACTS_KEPT` at most.
    let act_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(&e.line, Line::Act(a) if oldest_kept == u64::MAX || a.ts >= oldest_kept)
        })
        .map(|(i, _)| i)
        .collect();
    let acts_kept: HashSet<usize> = act_indices.iter().rev().take(ACTS_KEPT).copied().collect();
    let mut out = String::with_capacity(used as usize + 64 * 1024);
    for m in months.values() {
        if let Ok(line) = serde_json::to_string(&Line::Month(m.clone())) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    for (i, e) in entries.iter().enumerate() {
        let keep_line = match &e.line {
            Line::Open(rec) | Line::Close(rec) => kept.contains(rec.rid.as_str()),
            Line::Act(_) => acts_kept.contains(&i),
            Line::Unknown => true,
            Line::Month(_) => false,
        };
        if keep_line {
            out.push_str(e.raw);
            out.push('\n');
        }
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, out.as_bytes())?;
    std::fs::rename(&tmp, path)
}

/// Workspace ids are daemon-minted; the path component is sanitized anyway
/// (the Timeline's rule, so both files share a directory).
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
    crate::timeline::now_ms()
}

fn cap(text: &str, max: usize) -> String {
    crate::timeline::cap(text, max)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A path as the record keeps it: workspace-relative under the root.
fn rel(path: &str, root: Option<&Path>) -> String {
    let p = Path::new(path);
    match root.and_then(|r| p.strip_prefix(r).ok()) {
        Some(r) if !r.as_os_str().is_empty() => r.to_string_lossy().into_owned(),
        _ => path.to_string(),
    }
}

// ---- the record's lifecycle ------------------------------------------------

/// Open a session's record. Called once per agent session, where its
/// lifetime watcher starts (`agents::spawn_agent_watch`) — every spawn path
/// goes through there, and a view switch or rewind does not (the record
/// spans the session, whatever surface it is on).
pub(crate) fn open(state: &Arc<AppState>, sid: &str, started_by: StartedBy) {
    let Some(ws) = crate::lock(&state.session_workspaces).get(sid).cloned() else {
        return;
    };
    let Some((kind, title, conv)) = crate::lock(&state.agents)
        .get(sid)
        .map(|r| (r.kind, r.display_name(None), r.resumed_from.clone()))
    else {
        return;
    };
    let chat = state.chat.contains(sid);
    let mastermind = crate::lock(&state.workspaces)
        .get(&ws)
        .and_then(|w| w.mastermind)
        .is_some_and(|m| m.session_id == sid);
    let now = now_ms();
    let rec = Record {
        rid: format!("{sid}@{now}"),
        id: sid.to_string(),
        agent: kind.as_str().to_string(),
        ui: if chat { "chat" } else { "term" }.to_string(),
        title: (title != kind.as_str()).then(|| cap(&one_line(&title), TITLE_MAX)),
        started_by: started_by.wire(),
        started: now,
        mastermind,
        ..Record::default()
    };
    // Hook-less TUIs never report a turn; everything else does.
    let turns_known = chat || kind == AgentKind::Claude;
    let live = Live {
        ws: ws.clone(),
        rec: rec.clone(),
        conv: conv.clone(),
        cost: Running::default(),
        tin: Running::default(),
        tout: Running::default(),
        tin_sum: 0,
        tout_sum: 0,
        tokens_summed: false,
        turns: 0,
        turns_known,
        crashed: false,
        was_chat: chat,
        native: None,
        transcript_path: None,
        touched: VecDeque::new(),
        distinct: HashSet::new(),
        told: HashSet::new(),
        orphan_seen: 0,
        git: None,
    };
    let history = &state.history;
    let replaced = crate::lock(&history.live).insert(sid.to_string(), live);
    if let Some(old) = replaced {
        // The same id opened twice without a close (a spawn path that never
        // retired its predecessor): end the old record honestly.
        let rec = close_from_live(state, &old, Outcome::Retired);
        history.finish(old, rec);
    }
    history.append(&ws, &Line::Open(rec));
    history.checkpoint();
    // A continued conversation needs its previous running totals; the map
    // holds recent ones, the file the rest (read once, off the reactor).
    if let Some(conv) = conv.filter(|c| history.prior(c).is_none()) {
        let state = state.clone();
        let path = history.path(&ws);
        tokio::spawn(async move {
            let found = tokio::task::spawn_blocking(move || {
                merge(read_lines(&path))
                    .records
                    .into_iter()
                    .rev()
                    .find(|r| {
                        r.transcript
                            .as_ref()
                            .and_then(|t| t.native.as_deref())
                            .is_some_and(|n| n == conv)
                            && r.totals.is_some()
                    })
                    .and_then(|r| {
                        let native = r.transcript.and_then(|t| t.native)?;
                        Some((native, r.totals?))
                    })
            })
            .await
            .ok()
            .flatten();
            if let Some((conv, totals)) = found {
                if state.history.prior(&conv).is_none() {
                    state.history.remember_prior(&conv, totals);
                }
            }
        });
    }
}

/// The git anchors a record carries the instant it closes: the tracker's
/// start and latest-or-end anchors, from memory (it runs no git). The
/// commits arrive a moment later through [`attach_git`].
pub(crate) fn git_field(state: &AppState, session_id: &str) -> Option<serde_json::Value> {
    state
        .git
        .sessions
        .anchors_json(session_id)
        .map(|anchors| compact_git(&anchors))
}

/// The git tracker's answer for a session that ended in a repository
/// (`GET /sessions/{id}/git`'s body): an open record keeps it for its close;
/// a record that already closed is rewritten with it (a later close line for
/// the same record replaces the earlier one).
pub(crate) fn attach_git(state: &AppState, sid: &str, body: &serde_json::Value) {
    let git = compact_git(body);
    if let Some(live) = crate::lock(&state.history.live).get_mut(sid) {
        live.git = Some(git);
        return;
    }
    let amended = {
        let mut recent = crate::lock(&state.history.recent_closed);
        recent
            .iter_mut()
            .rev()
            .find(|(id, _, _)| id == sid)
            .map(|(_, ws, rec)| {
                rec.git = Some(git);
                (ws.clone(), rec.clone())
            })
    };
    if let Some((ws, rec)) = amended {
        state.history.append(&ws, &Line::Close(rec));
        crate::lock(&state.history.usage_cache).forget(&ws);
        state.history.epoch.fetch_add(1, Ordering::Relaxed);
    }
}

/// The bounded shape a record keeps: where the session started and ended
/// (repo, worktree, branch, head) and at most [`RECORD_COMMITS_MAX`] commits
/// as short sha + subject, with the full count beside them.
fn compact_git(body: &serde_json::Value) -> serde_json::Value {
    let anchor = |a: &serde_json::Value| -> serde_json::Value {
        if a.is_null() {
            return serde_json::Value::Null;
        }
        let text = |k: &str| a.get(k).and_then(|v| v.as_str()).map(|s| cap(s, PATH_MAX));
        serde_json::json!({
            "repo": text("repo"),
            "worktree": text("worktree"),
            "branch": text("branch"),
            "head": text("head"),
        })
    };
    let mut out = serde_json::json!({
        "start": anchor(body.get("start").unwrap_or(&serde_json::Value::Null)),
        "end": anchor(body.get("current").unwrap_or(&serde_json::Value::Null)),
    });
    if let Some(commits) = body.get("commits").and_then(|c| c.as_array()) {
        let kept: Vec<serde_json::Value> = commits
            .iter()
            .take(RECORD_COMMITS_MAX)
            .map(|c| {
                let sha = c.get("sha").and_then(|v| v.as_str()).unwrap_or("");
                let subject = c.get("subject").and_then(|v| v.as_str()).unwrap_or("");
                serde_json::json!({
                    "sha": sha.chars().take(12).collect::<String>(),
                    "subject": cap(&one_line(subject), COMMIT_SUBJECT_MAX),
                })
            })
            .collect();
        out["commits"] = serde_json::Value::Array(kept);
        out["commits_n"] = serde_json::json!(commits.len());
        for flag in ["truncated", "rewritten", "branch_changed", "repo_changed"] {
            if body.get(flag).and_then(|v| v.as_bool()) == Some(true) {
                out[flag] = serde_json::json!(true);
            }
        }
    }
    out
}

/// Close a session's record where its identity ends (`recents::retire`).
/// `record` is the AgentRecord just removed; `native` the chat driver's own
/// conversation id when it had one.
pub(crate) fn close(
    state: &AppState,
    sid: &str,
    record: &AgentRecord,
    pinned: Option<&str>,
    osc: Option<&str>,
    ui: SessionUi,
    native: Option<&str>,
) {
    let Some(mut live) = state.history.take(sid) else {
        return;
    };
    let outcome = if live.crashed {
        Outcome::Crashed
    } else {
        Outcome::Exited
    };
    live.rec.ui = match ui {
        SessionUi::Chat => "chat",
        SessionUi::Term => "term",
    }
    .to_string();
    let rec = close_with_record(state, &live, record, pinned, osc, native, outcome);
    state.history.finish(live, rec);
}

/// Close a session's record by id, reading its AgentRecord in place (the
/// Mastermind's teardown removes the identity itself, so it closes first).
pub(crate) fn close_by_id(state: &AppState, sid: &str, outcome: Outcome) {
    let record = crate::lock(&state.agents).get(sid).cloned();
    let Some(live) = state.history.take(sid) else {
        return;
    };
    let native = state.chat.get(sid).and_then(|c| c.native_session_id);
    let rec = match &record {
        Some(r) => close_with_record(state, &live, r, None, None, native.as_deref(), outcome),
        None => close_from_live(state, &live, outcome),
    };
    state.history.finish(live, rec);
}

fn workspace_root(state: &AppState, ws: &str) -> Option<PathBuf> {
    crate::lock(&state.workspaces).get(ws).map(|w| w.root)
}

fn close_with_record(
    state: &AppState,
    live: &Live,
    record: &AgentRecord,
    pinned: Option<&str>,
    osc: Option<&str>,
    native: Option<&str>,
    outcome: Outcome,
) -> Record {
    let root = workspace_root(state, &live.ws);
    let mut rec = live.snapshot();
    rec.ended = Some(now_ms());
    rec.outcome = Some(outcome);
    let title = pinned
        .map(str::to_string)
        .unwrap_or_else(|| record.display_name(osc));
    rec.title = (title != record.kind.as_str()).then(|| cap(&one_line(&title), TITLE_MAX));
    rec.first_prompt = record
        .first_prompt
        .as_deref()
        .map(|p| crate::timeline::prompt_title(p).0)
        .filter(|p| !p.is_empty())
        .map(|p| cap(&p, PROMPT_MAX));
    // The authoritative file list is the record's own (hooks and protocol
    // both feed it); the distinct count is ours (the list caps at 100).
    rec.files.n = live.distinct.len().max(record.files_touched.len());
    rec.files.top = record
        .files_touched
        .iter()
        .rev()
        .take(TOP_FILES)
        .map(|p| cap(&rel(p, root.as_deref()), PATH_MAX))
        .collect();
    rec.transcript = Some(transcript_pointer(live, record, native));
    rec.git = live.git.clone().or_else(|| git_field(state, &live.rec.id));
    rec
}

/// A record closed without its AgentRecord (an orphan sweep, a boot close):
/// what the open record itself knows.
fn close_from_live(state: &AppState, live: &Live, outcome: Outcome) -> Record {
    let root = workspace_root(state, &live.ws);
    let mut rec = live.snapshot();
    rec.ended = Some(now_ms());
    rec.outcome = Some(outcome);
    rec.files.top = rec
        .files
        .top
        .iter()
        .map(|p| cap(&rel(p, root.as_deref()), PATH_MAX))
        .collect();
    rec.git = live.git.clone().or_else(|| git_field(state, &live.rec.id));
    rec
}

fn transcript_pointer(live: &Live, record: &AgentRecord, native: Option<&str>) -> Transcript {
    let native = native
        .map(str::to_string)
        .or_else(|| record.resume_id())
        .or_else(|| live.native.clone())
        .or_else(|| record.resumed_from.clone())
        .or_else(|| live.conv.clone());
    let path = record
        .transcript_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());
    if live.was_chat {
        Transcript {
            kind: "chat".into(),
            journal: Some(live.rec.id.clone()),
            path,
            native,
        }
    } else if record.kind == AgentKind::Claude {
        Transcript {
            kind: "claude".into(),
            journal: None,
            path,
            native,
        }
    } else {
        Transcript {
            kind: record.kind.as_str().into(),
            journal: None,
            path: None,
            native,
        }
    }
}

/// Close every open record at a graceful stop (outcome `retired`: the daemon
/// ended the record, and the ledger brings the session back under a new
/// one), then wait for the writes. Runs after the ledger's final flush.
pub(crate) fn close_all_for_exit(state: &AppState) {
    let sids: Vec<String> = crate::lock(&state.history.live).keys().cloned().collect();
    for sid in sids {
        close_by_id(state, &sid, Outcome::Retired);
    }
    state.history.checkpoint();
    state.history.flush(Duration::from_secs(3));
}

/// Close what a daemon that died without a graceful stop left open, from its
/// checkpoint: before the ledger resurrects those sessions under new
/// records. `ended` is the checkpoint time — the last moment known alive.
pub(crate) async fn boot_close(state: &Arc<AppState>) {
    let path = state.history.index_path.clone();
    let body = tokio::task::spawn_blocking(move || std::fs::read_to_string(&path).ok())
        .await
        .ok()
        .flatten();
    let Some(body) = body else {
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let saved_at = value.get("saved_at").and_then(|v| v.as_u64()).unwrap_or(0);
    let mut closed = 0usize;
    for entry in value
        .get("records")
        .and_then(|r| r.as_array())
        .into_iter()
        .flatten()
    {
        let Some(ws) = entry.get("ws").and_then(|w| w.as_str()) else {
            continue;
        };
        let Some(mut rec) = entry
            .get("record")
            .and_then(|r| serde_json::from_value::<Record>(r.clone()).ok())
        else {
            continue;
        };
        if rec.ended.is_some() {
            continue;
        }
        rec.ended = Some(saved_at.max(rec.started));
        rec.outcome = Some(Outcome::Retired);
        let root = workspace_root(state, ws);
        rec.files.top = rec
            .files
            .top
            .iter()
            .map(|p| cap(&rel(p, root.as_deref()), PATH_MAX))
            .collect();
        if let (Some(conv), Some(totals)) = (
            rec.transcript
                .as_ref()
                .and_then(|t| t.native.clone())
                .or_else(|| {
                    entry
                        .get("conv")
                        .and_then(|c| c.as_str())
                        .map(str::to_string)
                }),
            rec.totals,
        ) {
            state.history.remember_prior(&conv, totals);
        }
        state.history.append(ws, &Line::Close(rec));
        closed += 1;
    }
    if closed > 0 {
        tracing::info!(closed, "closed session records a stopped daemon left open");
        state.history.epoch.fetch_add(1, Ordering::Relaxed);
    }
    state.history.checkpoint();
}

/// The daemon's history task: close orphaned records (a session whose
/// identity vanished without a retire — two sweeps in a row, so a retire
/// mid-flight always wins) and checkpoint moved usage. Idle-cheap: one pass
/// over the open records every `CHECKPOINT_EVERY`.
pub(crate) fn spawn_task(state: Arc<AppState>) {
    if state.history.started.swap(true, Ordering::Relaxed) {
        return;
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(CHECKPOINT_EVERY).await;
            if state.stopping.load(Ordering::Relaxed) {
                return;
            }
            sweep(&state);
            if state.history.dirty.load(Ordering::Relaxed) {
                state.history.checkpoint();
            }
        }
    });
}

fn sweep(state: &AppState) {
    let sids: Vec<String> = crate::lock(&state.history.live).keys().cloned().collect();
    for sid in sids {
        let known = crate::lock(&state.agents).contains_key(&sid)
            || crate::chat::session_alive(state, &sid);
        let orphaned = {
            let mut live = crate::lock(&state.history.live);
            let Some(l) = live.get_mut(&sid) else {
                continue;
            };
            if known {
                l.orphan_seen = 0;
                false
            } else {
                l.orphan_seen = l.orphan_seen.saturating_add(1);
                l.orphan_seen >= 2
            }
        };
        if orphaned {
            let outcome = crate::lock(&state.history.live)
                .get(&sid)
                .map(|l| {
                    if l.crashed {
                        Outcome::Crashed
                    } else {
                        Outcome::Exited
                    }
                })
                .unwrap_or(Outcome::Exited);
            close_by_id(state, &sid, outcome);
        }
    }
}

// ---- what the record observes ----------------------------------------------

/// A claude TUI's statusline heartbeat (running totals). Chat sessions are
/// the protocol's to report.
pub(crate) fn observe_statusline(state: &AppState, sid: &str, payload: &serde_json::Value) {
    if state.chat.contains(sid) {
        return;
    }
    let cost = payload
        .get("cost")
        .and_then(|c| c.get("total_cost_usd"))
        .and_then(|v| v.as_f64());
    let window = payload.get("context_window");
    let tin = window
        .and_then(|w| w.get("total_input_tokens"))
        .and_then(|v| v.as_f64());
    let tout = window
        .and_then(|w| w.get("total_output_tokens"))
        .and_then(|v| v.as_f64());
    let model = payload.get("model").and_then(|m| {
        m.get("display_name")
            .or_else(|| m.get("id"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    });
    let history = &state.history;
    let mut live = crate::lock(&history.live);
    let Some(l) = live.get_mut(sid) else {
        return;
    };
    let pre_turn = l.turns == 0;
    let prior = l.conv.as_deref().and_then(|c| history.prior(c));
    if let Some(v) = cost {
        l.cost.see(v, pre_turn, prior.and_then(|p| p.cost));
    }
    if let Some(v) = tin {
        l.tin.see(v, pre_turn, prior.and_then(|p| p.tin));
    }
    if let Some(v) = tout {
        l.tout.see(v, pre_turn, prior.and_then(|p| p.tout));
    }
    if let Some(m) = model {
        l.see_model(&m);
    }
    l.crashed = false;
    history.dirty.store(true, Ordering::Relaxed);
}

/// A hook event (claude TUIs and claude chats both fire them). Turns count
/// from the TUI's prompts only — chat turns are the protocol's.
pub(crate) fn observe_hook(
    state: &AppState,
    sid: &str,
    event: &str,
    transcript_path: Option<&str>,
    touched: Option<&str>,
) {
    let is_chat = state.chat.contains(sid);
    let mut live = crate::lock(&state.history.live);
    let Some(l) = live.get_mut(sid) else {
        return;
    };
    l.crashed = false;
    if event == "UserPromptSubmit" && !is_chat {
        l.turns = l.turns.saturating_add(1);
        l.rec.ui = "term".into();
    }
    if let Some(path) = touched {
        l.touch(path, now_ms());
    }
    if let Some(path) = transcript_path.filter(|p| !p.is_empty() && p.len() <= 4096) {
        if l.transcript_path.as_deref() != Some(path) {
            l.transcript_path = Some(path.to_string());
        }
    }
}

/// A chat protocol event.
pub(crate) fn observe_chat(state: &AppState, sid: &str, ev: &AgentEvent) {
    use chimaera_agent::model::ToolKind;
    let history = &state.history;
    let mut live = crate::lock(&history.live);
    let Some(l) = live.get_mut(sid) else {
        return;
    };
    l.was_chat = true;
    l.rec.ui = "chat".into();
    match ev {
        AgentEvent::Init {
            model,
            native_session_id,
            ..
        } => {
            if let Some(m) = model {
                l.see_model(m);
            }
            if !native_session_id.is_empty() {
                l.native = Some(native_session_id.chars().take(128).collect());
            }
        }
        AgentEvent::ModelSwitched { to, .. } => l.see_model(to),
        AgentEvent::ToolCall {
            kind: ToolKind::Edit,
            locations,
            ..
        } => {
            let now = now_ms();
            for path in locations {
                l.touch(path, now);
            }
        }
        AgentEvent::TurnCompleted { usage, .. } => {
            l.turns = l.turns.saturating_add(1);
            let prior = l.conv.as_deref().and_then(|c| history.prior(c));
            if let Some(c) = usage.cost_usd {
                l.cost.see(c, false, prior.and_then(|p| p.cost));
            }
            if l.rec.agent == AgentKind::Codex.as_str() {
                // Codex reports its thread's running totals.
                if usage.input_tokens > 0 || usage.output_tokens > 0 {
                    l.tin
                        .see(usage.input_tokens as f64, false, prior.and_then(|p| p.tin));
                    l.tout.see(
                        usage.output_tokens as f64,
                        false,
                        prior.and_then(|p| p.tout),
                    );
                }
            } else if l.rec.agent == AgentKind::Claude.as_str() {
                // Claude's result carries the turn's own tokens.
                l.tokens_summed = true;
                l.tin_sum = l.tin_sum.saturating_add(usage.input_tokens);
                l.tout_sum = l.tout_sum.saturating_add(usage.output_tokens);
            }
            history.dirty.store(true, Ordering::Relaxed);
        }
        AgentEvent::TurnAborted { .. } => {
            l.turns = l.turns.saturating_add(1);
        }
        _ => {}
    }
    l.crashed = false;
}

/// A chat driver's exit: a protocol error, a failed handshake, or a
/// non-zero exit marks the record crashed until the session shows life
/// again (a degrade to the terminal, a resume) — any later event clears it.
pub(crate) fn observe_exit(state: &AppState, sid: &str, exit: &chimaera_agent::driver::DriverExit) {
    use chimaera_agent::driver::DriverExit;
    let failed = match exit {
        DriverExit::ProtocolError(_) | DriverExit::HandshakeFailed { .. } => true,
        DriverExit::Clean(Some(code)) => *code != 0,
        DriverExit::Clean(None) | DriverExit::Killed => false,
    };
    if failed {
        if let Some(l) = crate::lock(&state.history.live).get_mut(sid) {
            l.crashed = true;
        }
    }
}

// ---- the same-file warning -------------------------------------------------

/// Cap on pairs one same-file answer lists.
const SAME_FILE_PAIRS_MAX: usize = 200;

/// Every (session, other session, path, when the other wrote it) among this
/// workspace's LIVE sessions, newest first — both directions of each pair.
pub(crate) fn same_file_pairs(state: &AppState, ws: &str) -> Vec<(String, String, String, u64)> {
    let live = crate::lock(&state.history.live);
    let mine: Vec<(&String, &Live)> = live.iter().filter(|(_, l)| l.ws == ws).collect();
    let mut out = Vec::new();
    for (sid, me) in &mine {
        let paths: HashSet<&str> = me.touched.iter().map(|(p, _)| p.as_str()).collect();
        for (osid, other) in &mine {
            if sid == osid {
                continue;
            }
            for (path, at) in &other.touched {
                if paths.contains(path.as_str()) {
                    out.push(((*sid).clone(), (*osid).clone(), path.clone(), *at));
                }
            }
        }
    }
    out.sort_by_key(|(_, _, _, at)| std::cmp::Reverse(*at));
    out.truncate(SAME_FILE_PAIRS_MAX);
    out
}

/// Context lines for a hook answer: another LIVE session in this workspace
/// wrote a file this one also wrote. Once per (other session, file) per
/// receiver, at most `SAME_FILE_LINES_MAX` per answer; nothing locks, and
/// with no overlap the answer is untouched (empty).
pub(crate) fn same_file_lines(state: &AppState, sid: &str) -> Vec<String> {
    let now = now_ms();
    // Gather under the history lock only (a leaf); names resolve after.
    let (ws, found) = {
        let live = crate::lock(&state.history.live);
        let Some(me) = live.get(sid) else {
            return Vec::new();
        };
        if me.touched.is_empty() {
            return Vec::new();
        }
        let mine: HashSet<&str> = me.touched.iter().map(|(p, _)| p.as_str()).collect();
        let mut found: Vec<(String, String, u64)> = Vec::new();
        for (osid, other) in live.iter() {
            if osid == sid || other.ws != me.ws {
                continue;
            }
            for (path, at) in other.touched.iter().rev() {
                if mine.contains(path.as_str()) && !me.told.contains(&(osid.clone(), path.clone()))
                {
                    found.push((osid.clone(), path.clone(), *at));
                }
            }
        }
        found.sort_by_key(|(_, _, at)| std::cmp::Reverse(*at));
        found.truncate(SAME_FILE_LINES_MAX);
        (me.ws.clone(), found)
    };
    if found.is_empty() {
        return Vec::new();
    }
    let root = workspace_root(state, &ws);
    let mut lines = Vec::with_capacity(found.len());
    for (osid, path, at) in &found {
        let name = crate::session_view::display_name_now(state, osid)
            .map(|n| one_line(&n))
            .unwrap_or_else(|| osid.clone());
        let name = cap(&name, 80);
        let ago = crate::comms::age(now.saturating_sub(*at));
        lines.push(format!(
            "chimaera: session '{name}' ({osid}), also running in this workspace, edited {} {ago} ago.",
            rel(path, root.as_deref())
        ));
    }
    if let Some(me) = crate::lock(&state.history.live).get_mut(sid) {
        for (osid, path, _) in found {
            if me.told.len() >= TOLD_MAX {
                break;
            }
            me.told.insert((osid, path));
        }
    }
    lines
}

// ---- the audit trail -------------------------------------------------------

/// Append an `act` line: a Mastermind action or a note delivery.
pub(crate) fn act(
    state: &AppState,
    ws: &str,
    by: &str,
    act: &str,
    target: Option<&str>,
    detail: Option<&str>,
) {
    let line = Line::Act(Act {
        ts: now_ms(),
        by: by.to_string(),
        act: act.to_string(),
        target: target.map(str::to_string),
        detail: detail
            .map(|d| cap(&one_line(d), ACT_DETAIL_MAX))
            .filter(|d| !d.is_empty()),
    });
    state.history.append(ws, &line);
    state.history.epoch.fetch_add(1, Ordering::Relaxed);
}

/// The open records of a workspace (the list's live rows), newest first.
pub(crate) fn live_records(state: &AppState, ws: &str) -> Vec<Record> {
    let mut out: Vec<Record> = crate::lock(&state.history.live)
        .values()
        .filter(|l| l.ws == ws)
        .map(Live::snapshot)
        .collect();
    let root = workspace_root(state, ws);
    for rec in &mut out {
        rec.files.top = rec
            .files
            .top
            .iter()
            .map(|p| rel(p, root.as_deref()))
            .collect();
        // A live row wears the name the rail shows (a pinned TUI name lives
        // on its PTY, not the record) and the prompt so far.
        if let Some(name) = crate::session_view::display_name_now(state, &rec.id) {
            if name != rec.agent {
                rec.title = Some(cap(&one_line(&name), TITLE_MAX));
            }
        }
        if rec.first_prompt.is_none() {
            rec.first_prompt = crate::lock(&state.agents)
                .get(&rec.id)
                .and_then(|r| r.first_prompt.as_deref())
                .map(|p| crate::timeline::prompt_title(p).0)
                .filter(|p| !p.is_empty())
                .map(|p| cap(&p, PROMPT_MAX));
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.started));
    out
}

#[cfg(test)]
mod tests;
