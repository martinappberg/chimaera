//! The notice feed: discrete "a person may want to know this now" events —
//! an agent finished its turn, needs a permission or an answer, stopped on an
//! error or a usage limit, or explicitly asked to notify the user (the MCP
//! `notify` tool). Consumers turn them into OS notifications: the native
//! shell long-polls [`get_notices`] once per daemon it has open (one alert
//! per notice however many windows it has, covering workspaces with no
//! window open), and browser tabs receive `notices` frames on `/ws/events`.
//!
//! **One detector, every surface.** Agent state is written from three places
//! (claude hooks, chat protocol events, the transcript watcher — claude chats
//! even get two of them), so edges are NOT emitted at the write sites: a
//! single watcher ([`run`]) diffs `AgentRecord.state` on every change wake and
//! sees each real transition exactly once, whichever writer caused it. The
//! descriptive words ride the record (`notice_note` / `reply_draft`, stashed
//! by the writers) and are consumed here at the edge.
//!
//! **Settle before speaking.** An edge becomes a notice only after it has
//! held for [`SETTLE`]: an auto-approved permission, or a user who replies
//! the instant a turn ends, never flashes a notification, and a turn-end the
//! agent immediately follows with "waiting on you" arrives as one notice.
//!
//! **Not only this computer's sessions.** Two more sources feed the same
//! ring: a Pro return that kept both versions of changed files
//! ([`push_kept_both`], a project event, not a session's), and a project that
//! runs on another machine — its owner's notices about that project's
//! conversations arrive on a window's events feed and are relayed here
//! ([`relay`], de-duplicated, so they reach the OS once like local ones).
//!
//! Bounded by construction: a small ring of recent notices ([`RING_CAP`]),
//! replayed to a reconnecting consumer only while fresh ([`REPLAY_MAX_AGE`]);
//! a fresh consumer (new boot, first poll) starts at the head, so opening
//! the app never replays history as new alerts. Presentation (sound, focus
//! suppression, the Dock badge) is the consumer's business; which KINDS
//! exist at all is decided here, from the `notifications.*` settings.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent_state::{AgentState, NoticeNote};
use crate::AppState;

/// How long an edge must hold before it becomes a notice.
const SETTLE: Duration = Duration::from_millis(800);
/// Recent notices kept for consumers catching up after a short gap.
const RING_CAP: usize = 64;
/// A reconnecting consumer is only told about notices this recent — after a
/// laptop sleep, a burst of hour-old alerts is noise; the in-app marks and
/// the badge already carry what is still true.
const REPLAY_MAX_AGE: Duration = Duration::from_secs(10 * 60);
/// Pause after a change wake before re-diffing (see [`run`]).
const WAKE_THROTTLE: Duration = Duration::from_millis(150);
/// Longest a `GET /notices` long-poll may hold the request open.
const MAX_WAIT: Duration = Duration::from_secs(30);
/// An agent's own `notify` within this window replaces the automatic
/// turn-end notice for the same session: "ping me when done" should ping
/// once, with the agent's words.
const AGENT_NOTICE_SUPERSEDES: Duration = Duration::from_secs(120);
/// Agent `notify` rate limits, per session: at most one per
/// [`AGENT_MIN_GAP`], and [`AGENT_HOURLY_CAP`] per rolling hour.
const AGENT_MIN_GAP: Duration = Duration::from_secs(5);
const AGENT_HOURLY_CAP: usize = 20;
const HOUR: Duration = Duration::from_secs(60 * 60);
/// Caps on agent-authored text (the tool validates, this bounds the ring).
pub(crate) const AGENT_TITLE_MAX: usize = 80;
/// Relayed notices remembered for de-duplication (every window watching a
/// routed project runs its own feed, so each owner notice arrives once per
/// window). Entries also expire after [`REPLAY_MAX_AGE`]: an owner never
/// replays older notices to a feed (a feed starts at its head).
const RELAY_SEEN_CAP: usize = 256;
/// Bounds on words this feed did not write itself (another daemon's, a
/// project's name, a kept file's name) before they enter the ring.
const WORDS_MAX: usize = 480;
const NAME_MAX: usize = 200;
/// Kept copies a `kept_both` notice names in its body; the full (bounded)
/// list rides the additive `kept.paths`.
const KEPT_NAMED: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoticeKind {
    /// The turn ended and the agent handed back the floor.
    Done,
    /// The turn ended with the agent explicitly waiting on the user.
    Input,
    /// A tool permission (or plan approval) is blocking the turn.
    Permission,
    /// A structured question is blocking the turn.
    Question,
    /// The turn or the session failed.
    Error,
    /// A usage limit is blocking requests.
    RateLimited,
    /// The agent called the `notify` tool.
    Agent,
    /// A Pro return kept both versions of files changed on both machines
    /// (the user's own beside the incoming one), or of a Git branch. A
    /// project event: `session_id` is a per-project key, not a session.
    KeptBoth,
}

impl NoticeKind {
    /// The settings key that switches this kind on or off.
    fn setting(self) -> &'static str {
        match self {
            NoticeKind::Done => "notifications.turnFinished",
            NoticeKind::Agent => "notifications.agentMessages",
            NoticeKind::Input
            | NoticeKind::Permission
            | NoticeKind::Question
            | NoticeKind::Error
            | NoticeKind::RateLimited
            | NoticeKind::KeptBoth => "notifications.needsYou",
        }
    }

    /// The state phrase a notification leads with.
    fn phrase(self) -> &'static str {
        match self {
            NoticeKind::Done => "Finished",
            NoticeKind::Input => "Waiting for you",
            NoticeKind::Permission => "Needs permission",
            NoticeKind::Question => "Has a question",
            NoticeKind::Error => "Stopped on an error",
            NoticeKind::RateLimited => "Hit a usage limit",
            NoticeKind::Agent => "Message",
            NoticeKind::KeptBoth => "Kept both versions",
        }
    }

    /// The owner's wire name, for the kinds a routed project relays: a turn
    /// that ended (finished, or waiting for the user), a permission or a
    /// question blocking it, and the agent's own `notify` — which the owner
    /// lets replace its turn-end notice, so relaying `done` without it would
    /// lose "ping me when it's done". Errors and usage limits stay with the
    /// owner: a cloud machine stopping its agents for a hand-back must not
    /// read as news here.
    fn relayed(kind: &str) -> Option<Self> {
        match kind {
            "done" => Some(NoticeKind::Done),
            "input" => Some(NoticeKind::Input),
            "permission" => Some(NoticeKind::Permission),
            "question" => Some(NoticeKind::Question),
            "agent" => Some(NoticeKind::Agent),
            _ => None,
        }
    }

    /// Whether the agent is blocked on the user's decision (consumers may
    /// bounce the Dock for these). A finished turn — even one that ends
    /// "waiting for your input" — is news, not a blocker: only a permission
    /// or a question stops the agent until the user answers.
    fn blocking(self) -> bool {
        matches!(self, NoticeKind::Permission | NoticeKind::Question)
    }
}

/// One notice. Titles are composed here, once, so every consumer words the
/// same event the same way; the structured fields let a consumer route a
/// click (session + workspace) and re-compose for its platform.
#[derive(Clone, Debug)]
pub(crate) struct Notice {
    pub(crate) id: u64,
    pub(crate) kind: NoticeKind,
    pub(crate) session_id: String,
    pub(crate) workspace_id: Option<String>,
    pub(crate) workspace: Option<String>,
    pub(crate) agent: Option<String>,
    /// The session's display name (what the rail calls it).
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) body: String,
    pub(crate) at_ms: u64,
    /// `kept_both` only: what the return kept (the additive `kept` field).
    pub(crate) kept: Option<Kept>,
    created: Instant,
}

/// What a return kept in both versions: `files` counted, `paths` naming up
/// to 32 of the kept copies (project-relative, as the mirror row's
/// `kept_paths`), and the other machine's diverged Git branches kept beside
/// the user's (`<branch>@cloud-<commit>`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Kept {
    pub(crate) files: usize,
    pub(crate) paths: Vec<String>,
    pub(crate) branches: Vec<String>,
}

impl Notice {
    /// Wire shape. `age_ms` is measured on THIS daemon's clock, so a
    /// consumer on another machine can judge freshness without clock sync.
    pub(crate) fn to_json(&self, now: Instant) -> Value {
        let mut value = json!({
            "id": self.id,
            "kind": self.kind,
            "blocking": self.kind.blocking(),
            "session_id": self.session_id,
            "workspace_id": self.workspace_id,
            "workspace": self.workspace,
            "agent": self.agent,
            "name": self.name,
            "title": self.title,
            "subtitle": self.subtitle,
            "body": self.body,
            "at_ms": self.at_ms,
            "age_ms": now.saturating_duration_since(self.created).as_millis() as u64,
        });
        if let Some(kept) = &self.kept {
            value["kept"] = json!(kept);
        }
        value
    }
}

/// The feed store. `boot` distinguishes this daemon process from its
/// predecessor (ids restart at 1 after a restart, and the port + token
/// survive a handoff, so a consumer needs something that doesn't).
pub(crate) struct Notices {
    boot: String,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    head: u64,
    ring: VecDeque<Arc<Notice>>,
    /// Per-session agent `notify` send times within the last hour.
    agent_sends: HashMap<String, VecDeque<Instant>>,
    /// Relayed notices already taken (see [`relay`]), oldest first.
    relayed: VecDeque<(RelayKey, Instant)>,
}

/// One owner notice's identity: its session, its kind, and the owner's
/// timestamp for it (a turn's own). Two windows' feeds deliver the same key.
type RelayKey = (String, NoticeKind, u64);

impl Notices {
    pub(crate) fn new() -> Self {
        Notices {
            boot: chimaera_core::generate_token()[..12].to_string(),
            inner: Mutex::new(Inner::default()),
        }
    }

    pub(crate) fn boot(&self) -> &str {
        &self.boot
    }

    pub(crate) fn head(&self) -> u64 {
        crate::lock(&self.inner).head
    }

    fn push(&self, mut notice: Notice) -> Arc<Notice> {
        let mut inner = crate::lock(&self.inner);
        inner.head += 1;
        notice.id = inner.head;
        let notice = Arc::new(notice);
        inner.ring.push_back(Arc::clone(&notice));
        while inner.ring.len() > RING_CAP {
            inner.ring.pop_front();
        }
        notice
    }

    /// Notices after `after` that are still fresh enough to replay.
    pub(crate) fn since(&self, after: u64) -> Vec<Arc<Notice>> {
        let inner = crate::lock(&self.inner);
        inner
            .ring
            .iter()
            .filter(|n| n.id > after && n.created.elapsed() <= REPLAY_MAX_AGE)
            .cloned()
            .collect()
    }

    /// Whether a relayed notice is new here; remembers it if so. Bounded
    /// both by count and by age.
    fn first_relay(&self, key: RelayKey) -> bool {
        let mut inner = crate::lock(&self.inner);
        while inner
            .relayed
            .front()
            .is_some_and(|(_, at)| at.elapsed() > REPLAY_MAX_AGE)
        {
            inner.relayed.pop_front();
        }
        if inner.relayed.iter().any(|(seen, _)| *seen == key) {
            return false;
        }
        if inner.relayed.len() >= RELAY_SEEN_CAP {
            inner.relayed.pop_front();
        }
        inner.relayed.push_back((key, Instant::now()));
        true
    }

    /// When this session last had an agent-sent notice, if within `window`.
    fn agent_sent_within(&self, session_id: &str, window: Duration) -> bool {
        crate::lock(&self.inner)
            .agent_sends
            .get(session_id)
            .and_then(|sends| sends.back())
            .is_some_and(|at| at.elapsed() <= window)
    }

    /// Record an agent send if the session is within its limits.
    fn admit_agent_send(&self, session_id: &str) -> Result<(), String> {
        let mut inner = crate::lock(&self.inner);
        let sends = inner.agent_sends.entry(session_id.to_string()).or_default();
        while sends.front().is_some_and(|at| at.elapsed() > HOUR) {
            sends.pop_front();
        }
        if let Some(last) = sends.back() {
            let gap = last.elapsed();
            if gap < AGENT_MIN_GAP {
                let wait = (AGENT_MIN_GAP - gap).as_secs().max(1);
                return Err(format!(
                    "Not sent: this session notified the user moments ago. Wait {wait}s, \
                     or fold this into one message."
                ));
            }
        }
        if sends.len() >= AGENT_HOURLY_CAP {
            return Err(format!(
                "Not sent: this session already sent {AGENT_HOURLY_CAP} notifications in \
                 the last hour. The user is still notified automatically when your turn \
                 ends or you need them."
            ));
        }
        sends.push_back(Instant::now());
        Ok(())
    }
}

/// A notification kind is on unless the user switched it off. Reads the
/// CACHED settings map — the watcher runs on the reactor, and hand-edits are
/// folded in off-reactor by `settings::watch_external_edits`.
fn kind_enabled(state: &AppState, kind: NoticeKind) -> bool {
    setting_bool(state, kind.setting(), true)
}

fn setting_bool(state: &AppState, key: &str, default: bool) -> bool {
    crate::lock(&state.settings)
        .map_cached()
        .get(key)
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

/// Who a notice is about: the names a person recognizes plus the routing
/// ids. Locks are taken one at a time in the documented order (sessions →
/// session_workspaces → agents, then workspaces alone), never nested.
struct Subject {
    name: String,
    workspace_id: Option<String>,
    workspace: Option<String>,
    agent: Option<String>,
    mastermind: bool,
}

fn describe(state: &AppState, session_id: &str) -> Option<Subject> {
    let pty = state.sessions.get(session_id);
    let workspace_id = crate::lock(&state.session_workspaces)
        .get(session_id)
        .cloned();
    let (name, agent) = {
        let agents = crate::lock(&state.agents);
        let record = agents.get(session_id)?;
        let name = match &pty {
            Some(info) if info.renamed => info.name.clone(),
            Some(info) => record.display_name(info.title.as_deref()),
            None => record.display_name(None),
        };
        (name, Some(record.kind.as_str().to_string()))
    };
    let workspace = workspace_id
        .as_deref()
        .and_then(|id| crate::lock(&state.workspaces).get(id));
    Some(Subject {
        name,
        mastermind: workspace
            .as_ref()
            .and_then(|w| w.mastermind.as_ref())
            .is_some_and(|m| m.session_id == session_id),
        workspace: workspace.map(|w| w.name),
        workspace_id,
        agent,
    })
}

/// Compose and store a state-edge notice.
fn emit_edge(
    state: &AppState,
    session_id: &str,
    kind: NoticeKind,
    note: Option<NoticeNote>,
) -> Option<Arc<Notice>> {
    let subject = describe(state, session_id)?;
    // The Mastermind is the observer, not the observed (its dock owns its
    // attention) — the same rule every roster surface applies.
    if subject.mastermind {
        return None;
    }
    let phrase = kind.phrase();
    let subtitle = match &subject.workspace {
        Some(ws) => format!("{phrase} · {ws}"),
        None => phrase.to_string(),
    };
    let body = note.map(|n| n.text).unwrap_or_default();
    Some(state.notices.push(Notice {
        id: 0,
        kind,
        session_id: session_id.to_string(),
        workspace_id: subject.workspace_id,
        workspace: subject.workspace,
        agent: subject.agent,
        title: subject.name.clone(),
        name: subject.name,
        subtitle,
        body,
        at_ms: crate::session_view::now_ms(),
        kept: None,
        created: Instant::now(),
    }))
}

/// The MCP `notify` tool's entry point: validate, rate-limit, store. The
/// `Ok` text is what the agent sees.
pub(crate) fn push_agent_notice(
    state: &AppState,
    session_id: &str,
    title: Option<&str>,
    message: &str,
) -> Result<String, String> {
    let message = crate::agent_state::notice_line(message);
    if message.is_empty() {
        return Err("`message` must be a non-empty string.".to_string());
    }
    if !kind_enabled(state, NoticeKind::Agent) {
        return Err(
            "Not sent: the user has turned off agent notifications in Chimaera's settings."
                .to_string(),
        );
    }
    let subject = describe(state, session_id).ok_or("unknown session")?;
    state.notices.admit_agent_send(session_id)?;
    let title = title
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(AGENT_TITLE_MAX).collect::<String>());
    // The agent's headline leads when given; the session then names the
    // sender in the subtitle so the user still knows who is talking.
    let (title, subtitle) = match title {
        Some(title) => {
            let from = match &subject.workspace {
                Some(ws) => format!("{} · {ws}", subject.name),
                None => subject.name.clone(),
            };
            (title, from)
        }
        None => (
            subject.name.clone(),
            subject.workspace.clone().unwrap_or_default(),
        ),
    };
    state.notices.push(Notice {
        id: 0,
        kind: NoticeKind::Agent,
        session_id: session_id.to_string(),
        workspace_id: subject.workspace_id,
        workspace: subject.workspace,
        agent: subject.agent,
        name: subject.name,
        title,
        subtitle,
        body: message,
        at_ms: crate::session_view::now_ms(),
        kept: None,
        created: Instant::now(),
    });
    state.changes.notify_waiters();
    Ok("Notification sent.".to_string())
}

/// A Pro return kept both versions of something (see `pro::report_return`):
/// one notice for the whole return, never one per file. `paths` are the kept
/// copies (`<name>.mine-<yyyymmdd-hhmm>`, project-relative, up to 32 of
/// `files`); `branches` the other machine's diverged branches kept beside the
/// user's. Words say what happened and where the user's version is; the
/// structured `kept` lets the UI list or open them.
pub(crate) fn push_kept_both(
    state: &AppState,
    workspace_id: &str,
    files: usize,
    paths: &[std::path::PathBuf],
    branches: &[String],
) -> Option<Arc<Notice>> {
    if (files == 0 && branches.is_empty()) || !kind_enabled(state, NoticeKind::KeptBoth) {
        return None;
    }
    let project = crate::lock(&state.workspaces)
        .get(workspace_id)
        .map(|w| w.name)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "your project".to_string());
    let project = clip(&project, NAME_MAX);
    let paths: Vec<String> = paths
        .iter()
        .take(32)
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let branches: Vec<String> = branches.iter().take(32).cloned().collect();
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let what = match (files, branches.len()) {
        (0, b) => plural(b, "branch", "branches"),
        (f, 0) => plural(f, "file", "files"),
        (f, b) => format!(
            "{} and {}",
            plural(f, "file", "files"),
            plural(b, "branch", "branches")
        ),
    };
    // Named by the kept copy's own file name: the one the user will find in
    // the file tree right beside the incoming version.
    let named = |items: Vec<&str>, total: usize| {
        let mut list = items.join(", ");
        if total > items.len() {
            list.push_str(&format!(" and {} more", total - items.len()));
        }
        list
    };
    let mut body = Vec::new();
    if files > 0 {
        let names: Vec<&str> = paths
            .iter()
            .take(KEPT_NAMED)
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(p)
            })
            .collect();
        let list = named(names, files);
        body.push(if files == 1 {
            format!("Your version is saved beside it ({list}).")
        } else {
            format!("Your versions are saved beside them ({list}).")
        });
    }
    if !branches.is_empty() {
        let list = named(
            branches
                .iter()
                .take(KEPT_NAMED)
                .map(String::as_str)
                .collect(),
            branches.len(),
        );
        body.push(if branches.len() == 1 {
            format!("The other machine's version is kept as branch {list}.")
        } else {
            format!("The other machine's versions are kept as branches {list}.")
        });
    }
    let notice = state.notices.push(Notice {
        id: 0,
        kind: NoticeKind::KeptBoth,
        // A per-project key where consumers expect a session id: a newer
        // return's notice replaces this project's older one, never another
        // project's; a click routes by `workspace_id`.
        session_id: format!("kept-both-{workspace_id}"),
        workspace_id: Some(workspace_id.to_string()),
        workspace: Some(project.clone()),
        agent: None,
        name: project.clone(),
        title: format!("Kept both versions of {what}"),
        subtitle: project,
        body: clip(&body.join(" "), WORDS_MAX),
        at_ms: crate::session_view::now_ms(),
        kept: Some(Kept {
            files,
            paths,
            branches,
        }),
        created: Instant::now(),
    });
    state.changes.notify_waiters();
    Some(notice)
}

/// A notice the owner of a routed project raised about one of that
/// project's conversations, as a window's events feed received it
/// (`session_proxy::Feed`). Taken into this daemon's own feed — so the
/// native app and browser tabs alert about it like a local one — when:
///
/// - it is a relayed kind (a turn that ended; a permission or a question;
///   the agent's own message), switched on in THIS computer's settings;
/// - it names this project and a session that does not run here (a live
///   local session notifies through this daemon's own watcher; relaying the
///   owner's word too would alert twice);
/// - it is fresh and new: every window watching the project runs its own
///   feed, so the same notice arrives once per window, and only the first
///   (by session, kind and the owner's timestamp) is kept.
///
/// Words are the owner's, bounded; the subtitle is recomposed with this
/// computer's project name. Returns whether it was taken.
pub(crate) fn relay(state: &AppState, workspace_id: &str, row: &Value) -> bool {
    let Some(kind) = row["kind"].as_str().and_then(NoticeKind::relayed) else {
        return false;
    };
    let (Some(session_id), Some(at_ms)) = (row["session_id"].as_str(), row["at_ms"].as_u64())
    else {
        return false;
    };
    let age = Duration::from_millis(row["age_ms"].as_u64().unwrap_or(0));
    if session_id.is_empty()
        || session_id.len() > 128
        || row["workspace_id"].as_str() != Some(workspace_id)
        || age > REPLAY_MAX_AGE
        || !kind_enabled(state, kind)
    {
        return false;
    }
    let local = state.sessions.get(session_id).is_some_and(|s| s.alive)
        || state.chat.get(session_id).is_some_and(|c| c.alive);
    if local
        || !state
            .notices
            .first_relay((session_id.to_string(), kind, at_ms))
    {
        return false;
    }
    let text = |key: &str, max: usize| clip(row[key].as_str().unwrap_or_default(), max);
    let workspace = crate::lock(&state.workspaces)
        .get(workspace_id)
        .map(|w| w.name)
        .or_else(|| row["workspace"].as_str().map(str::to_string))
        .map(|name| clip(&name, NAME_MAX));
    let name = text("name", NAME_MAX);
    let title = Some(text("title", NAME_MAX))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| name.clone());
    let phrase = kind.phrase();
    state.notices.push(Notice {
        id: 0,
        kind,
        session_id: session_id.to_string(),
        workspace_id: Some(workspace_id.to_string()),
        subtitle: match &workspace {
            Some(ws) => format!("{phrase} · {ws}"),
            None => phrase.to_string(),
        },
        workspace,
        agent: row["agent"].as_str().map(|a| clip(a, 32)),
        name,
        title,
        body: text("body", WORDS_MAX),
        // The owner's own time for it (what the de-duplication keys on);
        // its age as the owner measured it carries over.
        at_ms,
        kept: None,
        created: Instant::now().checked_sub(age).unwrap_or_else(Instant::now),
    });
    state.changes.notify_waiters();
    true
}

/// `text` without control characters, at most `max` chars.
fn clip(text: &str, max: usize) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// What an observed state change means for the feed.
#[derive(Debug, PartialEq, Eq)]
enum Edge {
    /// Schedule a notice of this kind (after the settle).
    Notice(NoticeKind),
    /// A pending turn-end notice becomes "waiting for you" (the agent's
    /// post-turn verdict lands just after its turn end).
    Upgrade,
    /// The agent moved on — drop whatever was pending.
    Cancel,
    None,
}

fn classify(prev: AgentState, next: AgentState) -> Edge {
    use AgentState::*;
    match (prev, next) {
        (_, Running) => Edge::Cancel,
        (Running, Finished) => Edge::Notice(NoticeKind::Done),
        (Running, IdlePrompt) => Edge::Notice(NoticeKind::Input),
        // A claude TUI's idle_prompt hook re-announces the SAME turn end a
        // minute later — news only while the turn-end notice is pending.
        (Finished, IdlePrompt) => Edge::Upgrade,
        (_, NeedsPermission) => Edge::Notice(NoticeKind::Permission),
        // Failures come before the "resolved" rule below: a session that
        // crashes or hits a limit while waiting on the user must still say
        // so — it is no longer waiting, and nobody acted.
        (_, Errored) => Edge::Notice(NoticeKind::Error),
        (_, RateLimited) => Edge::Notice(NoticeKind::RateLimited),
        // A block resolved without the agent resuming (denied, interrupted)
        // — the user acted, so they already know.
        (NeedsPermission, _) => Edge::Cancel,
        _ => Edge::None,
    }
}

/// Whether a pending notice of `kind` still describes the record's state
/// when its settle elapses.
fn still_true(kind: NoticeKind, state: AgentState) -> bool {
    match kind {
        NoticeKind::Done => state == AgentState::Finished,
        NoticeKind::Input => state == AgentState::IdlePrompt,
        NoticeKind::Permission | NoticeKind::Question => state == AgentState::NeedsPermission,
        NoticeKind::Error => state == AgentState::Errored,
        NoticeKind::RateLimited => state == AgentState::RateLimited,
        NoticeKind::Agent | NoticeKind::KeptBoth => false,
    }
}

struct Pending {
    kind: NoticeKind,
    due: Instant,
}

/// The edge detector. Runs for the daemon's lifetime; wakes on every change
/// (plus a 1s backstop — `notify_waiters` only reaches registered waiters,
/// and a level diff catches up on whatever a missed wake hid).
pub(crate) async fn run(state: Arc<AppState>) {
    let mut seen: HashMap<String, AgentState> = HashMap::new();
    let mut pending: HashMap<String, Pending> = HashMap::new();
    loop {
        let wake = state.changes.notified();
        tokio::pin!(wake);
        // Register before reading state (Notify only wakes registered
        // waiters); the first poll is what registers `ChangeBus::notified`.
        let woke = futures::poll!(wake.as_mut()).is_ready();
        observe(&state, &mut seen, &mut pending);
        if fire_due(&state, &mut pending) {
            state.changes.notify_waiters();
        }
        let next = pending
            .values()
            .map(|p| p.due.saturating_duration_since(Instant::now()))
            .min()
            .unwrap_or(Duration::from_secs(1))
            .min(Duration::from_secs(1));
        if woke {
            continue;
        }
        tokio::select! {
            _ = &mut wake => {
                // Chat sessions wake the bus on every streamed chunk; a short
                // breather turns that storm into a few level diffs (edges are
                // level-diffed, so nothing is lost — the settle dwarfs this).
                tokio::time::sleep(WAKE_THROTTLE).await;
            }
            _ = tokio::time::sleep(next) => {}
        }
    }
}

fn observe(
    state: &AppState,
    seen: &mut HashMap<String, AgentState>,
    pending: &mut HashMap<String, Pending>,
) {
    let mut agents = crate::lock(&state.agents);
    let now = Instant::now();
    for (id, record) in agents.iter_mut() {
        let corrected = record.state_corrected.take();
        // First sight (a new session, or every session at boot) is a
        // baseline, not an edge: a resurrected session must not announce
        // the state it was restored into.
        let Some(prev) = seen.get_mut(id) else {
            seen.insert(id.clone(), record.state);
            continue;
        };
        // So is a correction of a stale reading (a resumed idle chat read
        // Running from its SessionStart hook until its Init): no turn ended.
        // Only while the record still reads the corrected state — a change
        // since (a failed process, a permission asked by a queued turn) is
        // an edge as usual.
        if corrected == Some(record.state) {
            *prev = record.state;
            pending.remove(id);
            continue;
        }
        if *prev == record.state {
            continue;
        }
        let prev = std::mem::replace(prev, record.state);
        // Back to work: whatever the last edge was about (a permission
        // answered inside the settle, an idle-prompt message) is history,
        // and must not become the words of the NEXT notice.
        if record.state == AgentState::Running {
            record.notice_note = None;
        }
        match classify(prev, record.state) {
            Edge::Notice(kind) => {
                pending.insert(
                    id.clone(),
                    Pending {
                        kind,
                        due: now + SETTLE,
                    },
                );
            }
            Edge::Upgrade => {
                if let Some(p) = pending.get_mut(id) {
                    if p.kind == NoticeKind::Done {
                        p.kind = NoticeKind::Input;
                    }
                }
            }
            Edge::Cancel => {
                pending.remove(id);
            }
            Edge::None => {}
        }
    }
    if seen.len() != agents.len() {
        seen.retain(|id, _| agents.contains_key(id));
        pending.retain(|id, _| agents.contains_key(id));
        drop(agents);
        // A gone session's rate-limit history goes with it.
        let mut inner = crate::lock(&state.notices.inner);
        inner.agent_sends.retain(|id, _| seen.contains_key(id));
    }
}

/// Emit every pending notice whose settle has elapsed and whose edge still
/// holds. Returns whether anything was emitted.
fn fire_due(state: &AppState, pending: &mut HashMap<String, Pending>) -> bool {
    let now = Instant::now();
    let due: Vec<(String, NoticeKind)> = pending
        .iter()
        .filter(|(_, p)| p.due <= now)
        .map(|(id, p)| (id.clone(), p.kind))
        .collect();
    let mut emitted = false;
    for (id, kind) in due {
        pending.remove(&id);
        // Validate + consume the note under one agents lock, then describe
        // (which takes its own locks) after dropping it.
        let resolved = {
            let mut agents = crate::lock(&state.agents);
            let Some(record) = agents.get_mut(&id) else {
                continue;
            };
            if !still_true(kind, record.state) {
                continue;
            }
            // "Done" isn't done while subagents are still on the wire (the
            // hooks tier's straggler case) — the same hold the unread marks
            // apply.
            if kind == NoticeKind::Done && !record.subagents.is_empty() {
                continue;
            }
            let note = record.notice_note.take();
            let kind = match (&note, kind) {
                (Some(n), NoticeKind::Permission) if n.question => NoticeKind::Question,
                _ => kind,
            };
            (kind, note)
        };
        let (kind, note) = resolved;
        if !kind_enabled(state, kind) {
            continue;
        }
        if matches!(kind, NoticeKind::Done | NoticeKind::Input)
            && state
                .notices
                .agent_sent_within(&id, AGENT_NOTICE_SUPERSEDES)
        {
            continue;
        }
        emitted |= emit_edge(state, &id, kind, note).is_some();
    }
    emitted
}

/// One row of the attention set: a live session blocked on the user's
/// approval — a permission or a question (the UI's `needsApproval`, the one
/// state every count shows; finished / waiting-for-input sessions are
/// unread news, not a number on the Dock).
#[derive(Serialize, Hash)]
struct Attention {
    id: String,
    workspace_id: Option<String>,
    state: &'static str,
}

fn attention(state: &AppState) -> Vec<Attention> {
    // Runs on every long-poll wake (each streamed chat chunk), and the set
    // is almost always empty: find the few blocked records first, and only
    // then pay for liveness and workspace lookups — one lock at a time.
    let mut ids: Vec<String> = crate::lock(&state.agents)
        .iter()
        .filter(|(_, r)| r.state == AgentState::NeedsPermission)
        .map(|(id, _)| id.clone())
        .collect();
    // A project running on another machine: its conversations blocked on an
    // approval count too, as the in-app count already does from the same
    // rows. It also keeps a relayed permission alert up until it is answered
    // (on either machine) — consumers take back a blocking alert whose
    // session leaves this set.
    let routed = state.policy().routed_decisions(state);
    if ids.is_empty() && routed.is_empty() {
        return Vec::new();
    }
    ids.retain(|id| {
        state
            .sessions
            .get(id)
            .map(|s| s.alive)
            .or_else(|| state.chat.get(id).map(|c| c.alive))
            .unwrap_or(false)
    });
    let masterminds = crate::lock(&state.workspaces).mastermind_bindings();
    let workspaces = crate::lock(&state.session_workspaces);
    let mut rows: Vec<Attention> = ids
        .into_iter()
        .map(|id| {
            let workspace_id = workspaces.get(&id).cloned();
            (id, workspace_id)
        })
        .chain(
            routed
                .into_iter()
                .map(|(id, workspace_id)| (id, Some(workspace_id))),
        )
        .filter_map(|(id, workspace_id)| {
            let mastermind = workspace_id
                .as_ref()
                .and_then(|ws| masterminds.get(ws))
                .is_some_and(|sid| *sid == id);
            (!mastermind).then(|| Attention {
                id,
                workspace_id,
                state: AgentState::NeedsPermission.as_str(),
            })
        })
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows.dedup_by(|a, b| a.id == b.id);
    rows
}

fn attention_hash(rows: &[Attention]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    rows.hash(&mut hasher);
    hasher.finish()
}

#[derive(Deserialize)]
pub(crate) struct NoticesQuery {
    /// The last notice id this consumer has seen (0 = none).
    #[serde(default)]
    after: u64,
    /// The boot id that `after` belongs to; a mismatch (or none) starts the
    /// consumer at the head instead of replaying.
    #[serde(default)]
    boot: Option<String>,
    /// The attention hash this consumer last saw; the poll also returns when
    /// the needs-you set changes.
    #[serde(default)]
    attn: Option<u64>,
    /// Seconds to hold the request open waiting for news (capped).
    #[serde(default)]
    wait: u64,
}

/// GET /api/v1/notices — the native shell's long-poll. Returns at once when
/// there are notices after `after` (same `boot`), the attention set differs
/// from `attn`, or the consumer is fresh; otherwise holds up to `wait`
/// seconds for one of those. Bearer-authed like every API route.
///
/// Response: `{boot, head, notices:[…], attention:{hash, sessions:[…]},
/// prefs:{sound, while_focused, dock_badge}}`.
pub(crate) async fn get_notices(
    State(state): State<Arc<AppState>>,
    Query(q): Query<NoticesQuery>,
) -> Json<Value> {
    let deadline = Instant::now() + Duration::from_secs(q.wait).min(MAX_WAIT);
    let fresh = q.boot.as_deref() != Some(state.notices.boot());
    loop {
        let wake = state.changes.notified();
        tokio::pin!(wake);
        // Register before reading state (Notify only wakes registered
        // waiters); the first poll is what registers `ChangeBus::notified`.
        let woke = futures::poll!(wake.as_mut()).is_ready();
        let notices = if fresh {
            Vec::new()
        } else {
            state.notices.since(q.after)
        };
        let rows = attention(&state);
        let hash = attention_hash(&rows);
        // A daemon stopping must not wait out a held poll: graceful shutdown
        // drains in-flight requests.
        let stopping = state.stopping.load(Ordering::Relaxed);
        if fresh
            || stopping
            || !notices.is_empty()
            || q.attn != Some(hash)
            || Instant::now() >= deadline
        {
            let now = Instant::now();
            return Json(json!({
                "boot": state.notices.boot(),
                "head": state.notices.head(),
                "notices": notices.iter().map(|n| n.to_json(now)).collect::<Vec<_>>(),
                "attention": { "hash": hash, "sessions": rows },
                "prefs": {
                    "sound": setting_bool(&state, "notifications.sound", true),
                    "while_focused": setting_bool(&state, "notifications.whileFocused", true),
                    "dock_badge": setting_bool(&state, "notifications.dockBadge", true),
                },
            }));
        }
        if woke {
            continue;
        }
        tokio::select! {
            _ = &mut wake => {
                tokio::time::sleep(WAKE_THROTTLE).await;
            }
            _ = tokio::time::sleep_until(deadline.into()) => {}
        }
    }
}

/// The `/ws/events` half: notices newer than `last` for this client, plus
/// the new high-water mark. A client starts at the head when it connects
/// (see `ws::handle_events`), so a reload never replays old alerts.
pub(crate) fn frame_since(state: &AppState, last: &mut u64) -> Option<String> {
    let notices = state.notices.since(*last);
    let newest = notices.iter().map(|n| n.id).max()?;
    *last = newest;
    let now = Instant::now();
    Some(
        json!({
            "type": "notices",
            "notices": notices.iter().map(|n| n.to_json(now)).collect::<Vec<_>>(),
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_end_edges_classify() {
        use AgentState::*;
        assert_eq!(classify(Running, Finished), Edge::Notice(NoticeKind::Done));
        assert_eq!(
            classify(Running, IdlePrompt),
            Edge::Notice(NoticeKind::Input)
        );
        assert_eq!(classify(Finished, IdlePrompt), Edge::Upgrade);
        assert_eq!(classify(Finished, Running), Edge::Cancel);
        assert_eq!(classify(NeedsPermission, Running), Edge::Cancel);
    }

    #[test]
    fn blocking_edges_classify() {
        use AgentState::*;
        assert_eq!(
            classify(Running, NeedsPermission),
            Edge::Notice(NoticeKind::Permission)
        );
        assert_eq!(
            classify(Unknown, NeedsPermission),
            Edge::Notice(NoticeKind::Permission)
        );
        // Denied / interrupted: the user acted, nothing to announce.
        assert_eq!(classify(NeedsPermission, Finished), Edge::Cancel);
        assert_eq!(classify(Running, Errored), Edge::Notice(NoticeKind::Error));
        assert_eq!(
            classify(Running, RateLimited),
            Edge::Notice(NoticeKind::RateLimited)
        );
        // No news: a session settling from its spawn state.
        assert_eq!(classify(Unknown, Finished), Edge::None);
        // Failing while blocked on the user is news — nobody acted.
        assert_eq!(
            classify(NeedsPermission, Errored),
            Edge::Notice(NoticeKind::Error)
        );
        assert_eq!(
            classify(NeedsPermission, RateLimited),
            Edge::Notice(NoticeKind::RateLimited)
        );
    }

    /// A resumed idle chat reads Running from its SessionStart hook until
    /// its Init corrects it to Finished. That correction is a baseline: no
    /// turn ended, so it never becomes a "finished" notice (review R4 S2).
    /// A real turn end still does.
    #[test]
    fn a_corrected_reading_is_a_baseline_not_a_finished_notice() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-notices-corrected-{}",
            chimaera_core::generate_token()
        ));
        let state = AppState::new(
            "test-token".into(),
            "test-host".into(),
            std::process::id(),
            0,
            dir.join("data"),
            dir.join("config"),
        );
        let mut record = crate::agent_state::AgentRecord::new(
            "key".into(),
            crate::agent_state::AgentKind::Claude,
        );
        record.state = AgentState::Running;
        crate::lock(&state.agents).insert("s-resumed".into(), record);
        let (mut seen, mut pending) = (HashMap::new(), HashMap::new());
        observe(&state, &mut seen, &mut pending);
        {
            let mut agents = crate::lock(&state.agents);
            let record = agents.get_mut("s-resumed").unwrap();
            record.state = AgentState::Finished;
            record.state_corrected = Some(AgentState::Finished);
        }
        observe(&state, &mut seen, &mut pending);
        assert!(pending.is_empty(), "a correction is not a turn end");
        assert_eq!(seen["s-resumed"], AgentState::Finished);
        assert!(crate::lock(&state.agents)["s-resumed"]
            .state_corrected
            .is_none());
        // An ordinary turn end afterwards is still news.
        crate::lock(&state.agents)
            .get_mut("s-resumed")
            .unwrap()
            .state = AgentState::Running;
        observe(&state, &mut seen, &mut pending);
        crate::lock(&state.agents)
            .get_mut("s-resumed")
            .unwrap()
            .state = AgentState::Finished;
        observe(&state, &mut seen, &mut pending);
        assert_eq!(pending["s-resumed"].kind, NoticeKind::Done);
        // A correction the record has since moved past is no baseline: the
        // process failed right after its Init, and that is news.
        crate::lock(&state.agents)
            .get_mut("s-resumed")
            .unwrap()
            .state = AgentState::Running;
        observe(&state, &mut seen, &mut pending);
        {
            let mut agents = crate::lock(&state.agents);
            let record = agents.get_mut("s-resumed").unwrap();
            record.state = AgentState::Errored;
            record.state_corrected = Some(AgentState::Finished);
        }
        observe(&state, &mut seen, &mut pending);
        assert_eq!(pending["s-resumed"].kind, NoticeKind::Error);
        assert!(crate::lock(&state.agents)["s-resumed"]
            .state_corrected
            .is_none());
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pending_notice_must_still_hold() {
        assert!(still_true(NoticeKind::Done, AgentState::Finished));
        assert!(!still_true(NoticeKind::Done, AgentState::Running));
        assert!(still_true(
            NoticeKind::Question,
            AgentState::NeedsPermission
        ));
        assert!(!still_true(NoticeKind::Permission, AgentState::Running));
    }

    #[test]
    fn ring_is_bounded_and_ids_advance() {
        let notices = Notices::new();
        for _ in 0..(RING_CAP + 10) {
            notices.push(test_notice());
        }
        assert_eq!(notices.head(), (RING_CAP + 10) as u64);
        let all = notices.since(0);
        assert_eq!(all.len(), RING_CAP);
        assert_eq!(all.first().map(|n| n.id), Some(11));
        assert_eq!(notices.since(notices.head()).len(), 0);
    }

    #[test]
    fn agent_sends_are_rate_limited() {
        let notices = Notices::new();
        assert!(notices.admit_agent_send("s-1").is_ok());
        // Immediately again: inside the minimum gap.
        assert!(notices.admit_agent_send("s-1").is_err());
        // Another session has its own budget.
        assert!(notices.admit_agent_send("s-2").is_ok());
        assert!(notices.agent_sent_within("s-1", AGENT_NOTICE_SUPERSEDES));
        assert!(!notices.agent_sent_within("s-3", AGENT_NOTICE_SUPERSEDES));
    }

    #[test]
    fn hourly_cap_holds() {
        let notices = Notices::new();
        {
            let mut inner = crate::lock(&notices.inner);
            let old = Instant::now() - AGENT_MIN_GAP * 2;
            inner.agent_sends.insert(
                "s-1".into(),
                std::iter::repeat_n(old, AGENT_HOURLY_CAP).collect(),
            );
        }
        assert!(notices.admit_agent_send("s-1").is_err());
    }

    fn test_notice() -> Notice {
        Notice {
            id: 0,
            kind: NoticeKind::Done,
            session_id: "s-1".into(),
            workspace_id: None,
            workspace: None,
            agent: None,
            name: "n".into(),
            title: "t".into(),
            subtitle: "s".into(),
            body: "b".into(),
            at_ms: 0,
            kept: None,
            created: Instant::now(),
        }
    }
}
