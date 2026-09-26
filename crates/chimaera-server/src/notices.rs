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
            | NoticeKind::RateLimited => "notifications.needsYou",
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
    created: Instant,
}

impl Notice {
    /// Wire shape. `age_ms` is measured on THIS daemon's clock, so a
    /// consumer on another machine can judge freshness without clock sync.
    pub(crate) fn to_json(&self, now: Instant) -> Value {
        json!({
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
        })
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
}

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
        created: Instant::now(),
    });
    state.changes.notify_waiters();
    Ok("Notification sent.".to_string())
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
        NoticeKind::Agent => false,
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
        // First sight (a new session, or every session at boot) is a
        // baseline, not an edge: a resurrected session must not announce
        // the state it was restored into.
        let Some(prev) = seen.get_mut(id) else {
            seen.insert(id.clone(), record.state);
            continue;
        };
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
    if ids.is_empty() {
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
        .filter_map(|id| {
            let workspace_id = workspaces.get(&id).cloned();
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
            created: Instant::now(),
        }
    }
}
