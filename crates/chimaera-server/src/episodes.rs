//! Episodes: an agent's pieces of work, folded into the Timeline.
//!
//! One episode per TURN — the store stays append-only; readers group a
//! session's consecutive turns. Three fidelity tiers, worn honestly:
//! - **protocol** (chat sessions): the journal's own events, folded by
//!   [`ChatEpisodes`] on the chat signal task — prompt, final prose, files
//!   actually written, turn end.
//! - **hooks** (claude TUI): UserPromptSubmit → PostToolUse → Stop, folded by
//!   [`TuiEpisodes`] in `agents::ingest` (PTY sessions only — claude chat
//!   sessions fire the same `--settings` hooks, and counting them twice would
//!   double every chat turn).
//!
//! Hook-less TUIs (codex/gemini terminals) produce no episodes: the daemon
//! never sees their turns.
//!
//! Anatomy (plan §4): the headline is the user's own prompt; the result is
//! the first meaningful sentence of the agent's final message; the evidence
//! is files · duration · tools · end state. Nothing here calls a model.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chimaera_agent::model::{AgentEvent, ToolKind, ToolStatus, UserMessageState};

use crate::timeline::{self, Entry, Evidence, Kind};
use crate::AppState;

/// Bytes of final prose kept per turn — the headline needs the start.
const TEXT_KEEP: usize = 8 * 1024;
/// Queued-but-unsent prompts remembered per session (a backstop).
const QUEUED_MAX: usize = 16;

/// A finished turn, before workspace/session context is attached.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Draft {
    pub(crate) prompt: Option<String>,
    pub(crate) text: String,
    pub(crate) files: Vec<String>,
    pub(crate) tools: usize,
    pub(crate) start_ts: u64,
    pub(crate) end_ts: u64,
    /// finished | interrupted | errored | exited | unknown
    pub(crate) end: &'static str,
    pub(crate) duration_ms: Option<u64>,
}

#[derive(Default)]
struct TurnAcc {
    open: bool,
    prompts: Vec<String>,
    start_ts: u64,
    text: String,
    /// A tool ran since prose started: the next prose begins the final
    /// segment (narration before tool calls is not the answer).
    text_stale: bool,
    files: Vec<String>,
    announced: HashMap<String, Vec<String>>,
    tools: usize,
}

#[derive(Default)]
struct SessionAcc {
    turn: TurnAcc,
    /// Prompts for the NEXT turn (codex announces TurnStarted after the
    /// message; claude queues mid-turn sends until the turn ends).
    next_prompts: Vec<String>,
    /// id → text of queued messages, promoted when `UserMessageUpdate{Sent}`.
    queued: HashMap<String, String>,
}

/// Folds chat protocol events into per-turn drafts. Owned by the chat signal
/// task (single consumer, no lock). The signal channel sheds events under
/// backpressure, so the fold is defensive: a TurnStarted over an open turn
/// closes the old one as `unknown` rather than merging two turns.
#[derive(Default)]
pub(crate) struct ChatEpisodes {
    sessions: HashMap<String, SessionAcc>,
}

impl ChatEpisodes {
    /// Fold one event; returns a draft when a turn closed.
    pub(crate) fn observe(&mut self, sid: &str, ts: u64, ev: &AgentEvent) -> Option<Draft> {
        let acc = self.sessions.entry(sid.to_string()).or_default();
        match ev {
            AgentEvent::UserMessage {
                text, id, queued, ..
            } => {
                if *queued {
                    if let Some(id) = id {
                        if acc.queued.len() >= QUEUED_MAX {
                            acc.queued.clear();
                        }
                        acc.queued.insert(id.clone(), text.clone());
                    }
                } else if acc.turn.open {
                    // Anything said INSIDE a running turn is feedback (a
                    // permission-deny reason, a codex steer) — part of this
                    // ask, never a new headline.
                } else {
                    acc.next_prompts.push(text.clone());
                }
                None
            }
            AgentEvent::UserMessageUpdate { id, state } => {
                if let Some(text) = acc.queued.remove(id) {
                    if matches!(state, UserMessageState::Sent) {
                        acc.next_prompts.push(text);
                    }
                }
                None
            }
            AgentEvent::TurnStarted { .. } => {
                let closed = acc
                    .turn
                    .open
                    .then(|| close(&mut acc.turn, ts, "unknown", None));
                acc.turn = TurnAcc {
                    open: true,
                    prompts: std::mem::take(&mut acc.next_prompts),
                    start_ts: ts,
                    ..TurnAcc::default()
                };
                closed.flatten()
            }
            AgentEvent::MessageChunk { .. } | AgentEvent::ToolCall { .. } if !acc.turn.open => {
                // The signal channel sheds under backpressure: a lost
                // TurnStarted must not lose the turn. Open it implicitly and
                // fold this event into it.
                acc.turn = TurnAcc {
                    open: true,
                    prompts: std::mem::take(&mut acc.next_prompts),
                    start_ts: ts,
                    ..TurnAcc::default()
                };
                self.observe(sid, ts, ev)
            }
            AgentEvent::MessageChunk { text, .. } => {
                if acc.turn.text_stale {
                    acc.turn.text.clear();
                    acc.turn.text_stale = false;
                }
                if acc.turn.text.len() < TEXT_KEEP {
                    acc.turn.text.push_str(text);
                }
                None
            }
            AgentEvent::MessagesSuperseded
            | AgentEvent::ModelSwitched {
                retract_current_turn: true,
                ..
            } => {
                acc.turn.text.clear();
                None
            }
            AgentEvent::ToolCall {
                id,
                kind,
                locations,
                status,
                ..
            } => {
                acc.turn.tools += 1;
                acc.turn.text_stale = true;
                if *kind == ToolKind::Edit {
                    match status {
                        ToolStatus::Completed => add_files(&mut acc.turn.files, locations),
                        ToolStatus::Pending | ToolStatus::InProgress => {
                            acc.turn.announced.insert(id.clone(), locations.clone());
                        }
                        ToolStatus::Failed => {}
                    }
                }
                None
            }
            AgentEvent::ToolCallUpdate { id, status, .. } if acc.turn.open => {
                match status {
                    ToolStatus::Completed => {
                        if let Some(paths) = acc.turn.announced.remove(id) {
                            add_files(&mut acc.turn.files, &paths);
                        }
                    }
                    ToolStatus::Failed => {
                        acc.turn.announced.remove(id);
                    }
                    _ => {}
                }
                None
            }
            AgentEvent::TurnCompleted { usage, .. } if acc.turn.open => close(
                &mut acc.turn,
                ts,
                "finished",
                (usage.duration_ms > 0).then_some(usage.duration_ms),
            ),
            AgentEvent::TurnAborted { interrupted, .. } if acc.turn.open => {
                let end = if *interrupted {
                    "interrupted"
                } else {
                    "errored"
                };
                close(&mut acc.turn, ts, end, None)
            }
            _ => None,
        }
    }

    /// Drop a session's state without emitting (a deliberate view switch
    /// kills the process mid-turn; that is not history).
    pub(crate) fn forget(&mut self, sid: &str) {
        self.sessions.remove(sid);
    }

    /// The session ended: close an open turn as `exited` and forget it.
    pub(crate) fn flush(&mut self, sid: &str, ts: u64) -> Option<Draft> {
        let mut acc = self.sessions.remove(sid)?;
        if acc.turn.open {
            close(&mut acc.turn, ts, "exited", None)
        } else {
            None
        }
    }
}

fn add_files(files: &mut Vec<String>, paths: &[String]) {
    for p in paths {
        if !files.contains(p) {
            files.push(p.clone());
        }
    }
}

/// Close the turn. A turn with no prompt, no prose, and no tools (a
/// no-op wake, a /compact) is not history.
fn close(turn: &mut TurnAcc, ts: u64, end: &'static str, duration: Option<u64>) -> Option<Draft> {
    let taken = std::mem::take(turn);
    let prompt = (!taken.prompts.is_empty()).then(|| taken.prompts.join("\n"));
    if prompt.is_none() && taken.text.trim().is_empty() && taken.tools == 0 {
        return None;
    }
    Some(Draft {
        prompt,
        text: taken.text,
        files: taken.files,
        tools: taken.tools,
        start_ts: taken.start_ts,
        end_ts: ts,
        end,
        duration_ms: duration,
    })
}

/// Hook-driven turns for claude TUIs (PTY sessions only).
#[derive(Default)]
pub(crate) struct TuiEpisodes {
    open: HashMap<String, TurnAcc>,
}

impl TuiEpisodes {
    pub(crate) fn prompt(&mut self, sid: &str, ts: u64, prompt: &str) -> Option<Draft> {
        // A new prompt while one is open: the Stop was missed.
        let closed = self
            .open
            .remove(sid)
            .and_then(|mut t| close(&mut t, ts, "unknown", None));
        self.open.insert(
            sid.to_string(),
            TurnAcc {
                open: true,
                prompts: vec![prompt.to_string()],
                start_ts: ts,
                ..TurnAcc::default()
            },
        );
        closed
    }

    pub(crate) fn tool(&mut self, sid: &str, file: Option<&str>) {
        if let Some(turn) = self.open.get_mut(sid) {
            turn.tools += 1;
            if let Some(file) = file {
                add_files(&mut turn.files, &[file.to_string()]);
            }
        }
    }

    pub(crate) fn stop(
        &mut self,
        sid: &str,
        ts: u64,
        end: &'static str,
        final_text: Option<&str>,
    ) -> Option<Draft> {
        let mut turn = self.open.remove(sid)?;
        if let Some(text) = final_text {
            turn.text = text.chars().take(TEXT_KEEP).collect();
        }
        close(&mut turn, ts, end, None)
    }

    pub(crate) fn forget(&mut self, sid: &str) {
        self.open.remove(sid);
    }
}

/// Attach session + workspace context to a draft and append it. The
/// Mastermind's own turns are not project history (the observer, not the
/// observed). Knowledge written during the turn is attributed here.
pub(crate) async fn record(state: &Arc<AppState>, sid: &str, draft: Draft, tier: &'static str) {
    if crate::mcp::mastermind_of(state, sid) {
        return;
    }
    let Some(ws) = crate::plugins::workspace_of_session(state, sid) else {
        return;
    };
    let root = crate::lock(&state.workspaces).get(&ws).map(|w| w.root);
    let (agent, ui) = session_kind(state, sid);
    let name = crate::session_view::display_name_now(state, sid);

    let mut entry = Entry::new(Kind::Episode);
    entry.sid = Some(sid.to_string());
    entry.name = name;
    entry.agent = agent;
    entry.ui = Some(ui.to_string());
    entry.tier = Some(tier.to_string());
    if let Some(prompt) = &draft.prompt {
        let (title, via) = timeline::prompt_title(prompt);
        entry.title = (!title.is_empty()).then_some(title);
        entry.via = via;
    }
    entry.result = timeline::headline(&draft.text);
    entry.end = Some(draft.end.to_string());
    entry.start_ts = (draft.start_ts > 0).then_some(draft.start_ts);
    entry.ts = draft.end_ts;
    entry.ms = draft
        .duration_ms
        .or_else(|| (draft.start_ts > 0).then(|| draft.end_ts.saturating_sub(draft.start_ts)));
    let files_n = draft.files.len();
    let files: Vec<String> = draft
        .files
        .iter()
        .take(timeline::FILES_MAX)
        .map(|f| relative_to(root.as_deref(), f))
        .collect();
    let mut evidence = Evidence {
        files,
        files_n,
        tools: draft.tools,
        recorded: None,
    };
    evidence.recorded =
        crate::knowledge::recorded_since_last_check(state, &ws, sid, entry.start_ts).await;
    entry.evidence = Some(evidence);
    state.timeline.append(&ws, entry).await;
    state.changes.notify_waiters();
}

/// A chat session that died on its own with an error is history ("claude-2
/// crashed"); a clean exit, a deliberate kill, or a handshake failure (which
/// degrades to a terminal and says so there) is not.
pub(crate) async fn record_exit(
    state: &Arc<AppState>,
    sid: &str,
    exit: &chimaera_agent::driver::DriverExit,
) {
    use chimaera_agent::driver::DriverExit;
    let detail = match exit {
        DriverExit::Clean(Some(code)) if *code != 0 => format!("exited with code {code}"),
        DriverExit::ProtocolError(err) => {
            format!("stopped responding: {}", timeline::cap(err, 200))
        }
        _ => return,
    };
    if crate::mcp::mastermind_of(state, sid) {
        return;
    }
    let Some(ws) = crate::plugins::workspace_of_session(state, sid) else {
        return;
    };
    let (agent, ui) = session_kind(state, sid);
    let mut entry = Entry::new(Kind::Session);
    entry.sid = Some(sid.to_string());
    entry.name = crate::session_view::display_name_now(state, sid);
    entry.agent = agent;
    entry.ui = Some(ui.to_string());
    entry.end = Some("errored".to_string());
    entry.title = Some(detail);
    state.timeline.append(&ws, entry).await;
    state.changes.notify_waiters();
}

/// Commands worth remembering: a failure that ran ≥10 s, or anything that
/// ran ≥2 min. A Ctrl-C (exit 130) is the user's own stop, not news.
const COMMAND_FAIL_MIN_MS: u64 = 10_000;
const COMMAND_LONG_MIN_MS: u64 = 120_000;
/// Command-line head persisted to the Timeline (after redaction).
const COMMAND_HEAD: usize = 120;

/// Programs whose "duration" is a person sitting in them, not work.
const INTERACTIVE: [&str; 22] = [
    "ssh", "mosh", "vim", "vi", "nvim", "nano", "emacs", "less", "more", "man", "top", "htop",
    "btop", "tmux", "screen", "watch", "claude", "codex", "gemini", "jupyter", "tail", "sudo",
];
/// Interpreters/shells: interactive only when started bare (a REPL).
const REPLS: [&str; 10] = [
    "python", "python3", "ipython", "R", "julia", "node", "bash", "zsh", "fish", "sh",
];

pub(crate) fn command_is_notable(meta: &chimaera_pty::CommandMeta) -> bool {
    let ms = meta.ended_at_ms.saturating_sub(meta.started_at_ms);
    let Some(text) = meta.command.as_deref() else {
        return false;
    };
    if is_interactive(text) {
        return false;
    }
    match meta.exit_code {
        Some(130) => false,
        Some(0) | None => ms >= COMMAND_LONG_MIN_MS,
        Some(_) => ms >= COMMAND_FAIL_MIN_MS,
    }
}

fn is_interactive(text: &str) -> bool {
    let mut words = text
        .split_whitespace()
        // Leading VAR=value assignments.
        .skip_while(|w| w.contains('=') && !w.starts_with('-'));
    let Some(first) = words.next() else {
        return true;
    };
    let base = first.rsplit('/').next().unwrap_or(first);
    let rest: Vec<&str> = words.collect();
    if INTERACTIVE.contains(&base) {
        return true;
    }
    if REPLS.contains(&base) && rest.is_empty() {
        return true;
    }
    // An interactive allocation (`srun --pty bash`, `salloc`) is a session.
    base == "salloc" || (base == "srun" && rest.contains(&"--pty"))
}

/// `scheme://user:pass@host` → `scheme://user:•••@host`; a bare
/// `scheme://TOKEN@host` masks the whole userinfo.
fn mask_url_userinfo(tok: &str) -> Option<String> {
    let start = tok.find("://")? + 3;
    let rest = &tok[start..];
    let at = rest[..rest.find('/').unwrap_or(rest.len())].rfind('@')?;
    let masked = match rest[..at].split_once(':') {
        Some((user, _)) => format!("{user}:•••"),
        None => "•••".to_string(),
    };
    Some(format!("{}{masked}{}", &tok[..start], &rest[at..]))
}

/// `user:password` (curl `-u`, `--user=`) → `user:•••`; a bare user stays.
fn mask_user_pass(value: &str) -> String {
    match value.split_once(':') {
        Some((user, _)) => format!("{user}:•••"),
        None => value.to_string(),
    }
}

/// Mask values that look like secrets before a command line is persisted or
/// shown to a model: `KEY=value` where the key names a credential, the
/// argument after a credential flag (`--token x`, `-p x`), the password half
/// of `-u`/`--user user:pass`, URL userinfo, mysql's glued `-pSECRET`, and
/// anything after "Bearer"/"Authorization:".
pub(crate) fn redact_command(text: &str) -> String {
    const SENSITIVE: [&str; 10] = [
        "token",
        "secret",
        "passw",
        "pwd",
        "apikey",
        "api_key",
        "api-key",
        "auth",
        "credential",
        "private",
    ];
    let sensitive = |w: &str| {
        let lower = w.to_lowercase();
        SENSITIVE.iter().any(|s| lower.contains(s))
    };
    let bare = |w: &str| w.trim_matches(['\'', '"']).to_lowercase();
    #[derive(PartialEq)]
    enum Mask {
        No,
        Next,
        /// After "Authorization:": a scheme word (Bearer/Basic/Token) is
        /// kept and the credential after it masked.
        AfterAuth,
        /// After `-u`/`--user`: only a `user:pass` value is secret.
        UserPass,
    }
    let program = text
        .split_whitespace()
        .next()
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_lowercase())
        .unwrap_or_default();
    let glued_password = program.starts_with("mysql") || program.starts_with("mariadb");
    let mut out: Vec<String> = Vec::new();
    let mut mask = Mask::No;
    for tok in text.split_whitespace() {
        match mask {
            Mask::UserPass => {
                out.push(mask_user_pass(tok));
                mask = Mask::No;
                continue;
            }
            Mask::AfterAuth if matches!(bare(tok).as_str(), "bearer" | "basic" | "token") => {
                out.push(tok.to_string());
                mask = Mask::Next;
                continue;
            }
            Mask::AfterAuth | Mask::Next => {
                out.push("•••".to_string());
                mask = Mask::No;
                continue;
            }
            Mask::No => {}
        }
        if let Some((key, value)) = tok.split_once('=') {
            if sensitive(key) {
                out.push(format!("{key}=•••"));
                continue;
            }
            if key == "--user" {
                out.push(format!("{key}={}", mask_user_pass(value)));
                continue;
            }
        }
        if let Some(masked) = mask_url_userinfo(tok) {
            out.push(masked);
            continue;
        }
        if glued_password && tok.len() > 2 && tok.starts_with("-p") {
            out.push("-p•••".to_string());
            continue;
        }
        let word = bare(tok);
        out.push(tok.to_string());
        if word.starts_with("authorization") {
            mask = Mask::AfterAuth;
        } else if word == "bearer" || (tok.starts_with('-') && (sensitive(tok) || tok == "-p")) {
            mask = Mask::Next;
        } else if tok == "-u" || tok == "--user" {
            mask = Mask::UserPass;
        }
    }
    out.join(" ")
}

/// A notable command in a workspace terminal becomes a Timeline entry.
pub(crate) async fn record_command(
    state: &Arc<AppState>,
    sid: &str,
    meta: &chimaera_pty::CommandMeta,
) {
    if !command_is_notable(meta) {
        return;
    }
    let Some(ws) = crate::plugins::workspace_of_session(state, sid) else {
        return;
    };
    let text = timeline::cap(
        &redact_command(meta.command.as_deref().unwrap_or_default()),
        COMMAND_HEAD,
    );
    let mut entry = Entry::new(Kind::Command);
    entry.sid = Some(sid.to_string());
    entry.name = crate::session_view::display_name_now(state, sid);
    entry.ts = meta.ended_at_ms;
    entry.command = Some(timeline::CommandInfo {
        text,
        exit: meta.exit_code,
        ms: meta.ended_at_ms.saturating_sub(meta.started_at_ms),
        source: match meta.source {
            chimaera_pty::CommandSource::Agent => "agent".to_string(),
            chimaera_pty::CommandSource::User => "user".to_string(),
        },
    });
    state.timeline.append(&ws, entry).await;
    state.changes.notify_waiters();
}

/// How often the job task looks — and the floor between the queue refreshes
/// it triggers (scheduler-polite; the UI's own visible poll is separate).
const JOBS_TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// The workspace whose root contains `workdir` (longest root wins).
fn workspace_for_dir(state: &AppState, workdir: &str) -> Option<String> {
    workspace_root_for_dir(state, workdir).map(|(id, _)| id)
}

fn workspace_root_for_dir(state: &AppState, workdir: &str) -> Option<(String, PathBuf)> {
    if workdir.is_empty() {
        return None;
    }
    let dir = Path::new(workdir);
    crate::lock(&state.workspaces)
        .list()
        .into_iter()
        .filter(|w| dir.starts_with(&w.root))
        .max_by_key(|w| w.root.as_os_str().len())
        .map(|w| (w.id, w.root))
}

/// Whether a live job may keep the background poll alive: only one owned by
/// a project workspace. A root at `$HOME` (or above it) claims every job the
/// user runs, which would turn the poll into a standing once-a-minute squeue
/// for as long as anything is queued; its ended jobs are still recorded
/// whenever a viewer's own refresh notices them.
fn keeps_poll_alive(state: &AppState, workdir: &str, home: Option<&Path>) -> bool {
    workspace_root_for_dir(state, workdir).is_some_and(|(_, root)| is_project_root(&root, home))
}

fn is_project_root(root: &Path, home: Option<&Path>) -> bool {
    home.is_none_or(|home| !home.starts_with(root))
}

/// Finished Slurm jobs → Timeline entries. One tick a minute: drain what
/// the compute service noticed, and — only while a job attributed to some
/// workspace is still live — refresh the queue so an unattended run still
/// reports its end. A laptop (no scheduler, no cache) does no work at all.
pub(crate) fn spawn_jobs_task(state: Arc<AppState>) {
    if state
        .timeline_jobs_started
        .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(JOBS_TICK).await;
            let live_attributed = state.compute.peek().is_some_and(|(at, snap)| {
                at.elapsed() >= JOBS_TICK
                    && snap.jobs.iter().any(|j| {
                        crate::compute::is_live_state(&j.state)
                            && keeps_poll_alive(&state, &j.workdir, home.as_deref())
                    })
            });
            if live_attributed {
                state.compute.snapshot(false).await;
            }
            for end in state.compute.drain_ended() {
                let Some(ws) = workspace_for_dir(&state, &end.job.workdir) else {
                    continue;
                };
                let mut entry = Entry::new(Kind::Job);
                entry.job = Some(timeline::JobInfo {
                    id: end.job.id.clone(),
                    name: timeline::cap(&end.job.name, 120),
                    state: end.state.clone(),
                    elapsed: (!end.job.elapsed.is_empty()).then(|| end.job.elapsed.clone()),
                });
                state.timeline.append(&ws, entry).await;
                state.changes.notify_waiters();
            }
        }
    });
}

fn session_kind(state: &AppState, sid: &str) -> (Option<String>, &'static str) {
    let agent = crate::lock(&state.agents)
        .get(sid)
        .map(|r| r.kind.as_str().to_string());
    let ui = if state.chat.get(sid).is_some() {
        "chat"
    } else {
        "term"
    };
    (agent, ui)
}

fn relative_to(root: Option<&Path>, path: &str) -> String {
    root.and_then(|r| Path::new(path).strip_prefix(r).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chimaera_agent::model::Usage;

    fn user(text: &str, id: Option<&str>, queued: bool) -> AgentEvent {
        AgentEvent::UserMessage {
            text: text.into(),
            attachments: 0,
            id: id.map(Into::into),
            queued,
            origin: None,
        }
    }
    fn started() -> AgentEvent {
        AgentEvent::TurnStarted {
            turn_id: "t".into(),
        }
    }
    fn chunk(text: &str) -> AgentEvent {
        AgentEvent::MessageChunk {
            turn_id: "t".into(),
            text: text.into(),
        }
    }
    fn edit(id: &str, path: &str, status: ToolStatus) -> AgentEvent {
        AgentEvent::ToolCall {
            id: id.into(),
            kind: ToolKind::Edit,
            title: "Edit".into(),
            locations: vec![path.into()],
            status,
            cross_turn: false,
            command: None,
        }
    }
    fn done(id: &str, status: ToolStatus) -> AgentEvent {
        AgentEvent::ToolCallUpdate {
            id: id.into(),
            status,
            content: None,
        }
    }
    fn completed() -> AgentEvent {
        AgentEvent::TurnCompleted {
            turn_id: "t".into(),
            usage: Usage::default(),
        }
    }

    #[test]
    fn a_claude_turn_keeps_the_prompt_final_prose_and_written_files_only() {
        let mut eps = ChatEpisodes::default();
        let s = "s1";
        assert!(eps
            .observe(s, 1, &user("fix QC", Some("u1"), false))
            .is_none());
        eps.observe(s, 2, &started());
        eps.observe(s, 3, &chunk("Let me look at the filter."));
        eps.observe(s, 4, &edit("e1", "/w/qc.R", ToolStatus::InProgress));
        eps.observe(s, 5, &edit("e2", "/w/denied.R", ToolStatus::InProgress));
        eps.observe(s, 6, &done("e1", ToolStatus::Completed));
        eps.observe(s, 7, &done("e2", ToolStatus::Failed));
        eps.observe(s, 8, &chunk("Done! Per-sample MAD thresholds now."));
        let d = eps.observe(s, 9, &completed()).expect("closed");
        assert_eq!(d.prompt.as_deref(), Some("fix QC"));
        assert_eq!(d.text, "Done! Per-sample MAD thresholds now.");
        assert_eq!(d.files, vec!["/w/qc.R".to_string()]);
        assert_eq!(d.tools, 2);
        assert_eq!((d.start_ts, d.end_ts, d.end), (2, 9, "finished"));
    }

    #[test]
    fn codex_message_before_a_late_turn_start_still_titles_the_turn() {
        let mut eps = ChatEpisodes::default();
        eps.observe("c", 1, &user("re-run DE", Some("m"), false));
        eps.observe("c", 5, &started());
        eps.observe("c", 6, &chunk("Re-ran it."));
        let d = eps.observe("c", 7, &completed()).unwrap();
        assert_eq!(d.prompt.as_deref(), Some("re-run DE"));
    }

    #[test]
    fn queued_follow_ups_title_the_next_turn_and_feedback_is_ignored() {
        let mut eps = ChatEpisodes::default();
        let s = "q";
        eps.observe(s, 1, &user("first", Some("a"), false));
        eps.observe(s, 2, &started());
        eps.observe(s, 3, &user("deny feedback", None, false));
        eps.observe(s, 4, &user("second", Some("b"), true));
        eps.observe(s, 5, &user("third", Some("c"), true));
        eps.observe(
            s,
            6,
            &AgentEvent::UserMessageUpdate {
                id: "c".into(),
                state: UserMessageState::Cancelled,
            },
        );
        let first = eps.observe(s, 7, &completed()).unwrap();
        assert_eq!(first.prompt.as_deref(), Some("first"));
        eps.observe(
            s,
            8,
            &AgentEvent::UserMessageUpdate {
                id: "b".into(),
                state: UserMessageState::Sent,
            },
        );
        eps.observe(s, 9, &started());
        eps.observe(s, 10, &chunk("ok"));
        let second = eps.observe(s, 11, &completed()).unwrap();
        assert_eq!(second.prompt.as_deref(), Some("second"));
    }

    #[test]
    fn a_lost_turn_end_closes_as_unknown_instead_of_merging() {
        let mut eps = ChatEpisodes::default();
        eps.observe("x", 1, &user("one", Some("1"), false));
        eps.observe("x", 2, &started());
        eps.observe("x", 3, &chunk("partial"));
        eps.observe("x", 4, &user("two", Some("2"), false));
        let lost = eps.observe("x", 5, &started()).expect("old turn closed");
        assert_eq!((lost.prompt.as_deref(), lost.end), (Some("one"), "unknown"));
    }

    #[test]
    fn superseded_prose_is_dropped_and_empty_turns_are_not_history() {
        let mut eps = ChatEpisodes::default();
        eps.observe("r", 1, &started());
        eps.observe("r", 2, &chunk("withdrawn"));
        eps.observe("r", 3, &AgentEvent::MessagesSuperseded);
        assert!(eps.observe("r", 4, &completed()).is_none(), "nothing left");
        eps.observe("r", 5, &started());
        eps.observe("r", 6, &chunk("kept"));
        assert!(eps.flush("r", 7).is_some_and(|d| d.end == "exited"));
    }

    #[test]
    fn interrupted_and_failed_turns_say_so() {
        let mut eps = ChatEpisodes::default();
        eps.observe("i", 1, &user("go", Some("g"), false));
        eps.observe("i", 2, &started());
        let d = eps
            .observe(
                "i",
                3,
                &AgentEvent::TurnAborted {
                    turn_id: "t".into(),
                    reason: "stop".into(),
                    interrupted: true,
                },
            )
            .unwrap();
        assert_eq!(d.end, "interrupted");
    }

    #[test]
    fn tui_hooks_fold_prompt_files_and_final_text() {
        let mut tui = TuiEpisodes::default();
        assert!(tui.prompt("p", 1, "draft legends").is_none());
        tui.tool("p", Some("/w/fig2.md"));
        tui.tool("p", None);
        let d = tui
            .stop("p", 9, "finished", Some("Drafted all six legends."))
            .unwrap();
        assert_eq!(d.prompt.as_deref(), Some("draft legends"));
        assert_eq!(d.files, vec!["/w/fig2.md".to_string()]);
        assert_eq!(d.tools, 2);
        assert!(
            tui.stop("p", 10, "finished", None).is_none(),
            "no open turn"
        );
    }

    fn meta(cmd: &str, exit: Option<i32>, secs: u64) -> chimaera_pty::CommandMeta {
        chimaera_pty::CommandMeta {
            seq: 1,
            command: Some(cmd.into()),
            source: chimaera_pty::CommandSource::User,
            exit_code: exit,
            started_at_ms: 1_000,
            ended_at_ms: 1_000 + secs * 1000,
        }
    }

    #[test]
    fn only_notable_commands_become_history() {
        assert!(command_is_notable(&meta(
            "snakemake -j 32 de_all",
            Some(1),
            7980
        )));
        assert!(
            !command_is_notable(&meta("make", Some(2), 3)),
            "quick failure"
        );
        assert!(
            command_is_notable(&meta("Rscript de.R", Some(0), 300)),
            "long success"
        );
        assert!(!command_is_notable(&meta("ls", Some(0), 1)));
        assert!(
            !command_is_notable(&meta("sleep 999", Some(130), 999)),
            "Ctrl-C"
        );
        assert!(!command_is_notable(&meta("vim notes.md", Some(0), 900)));
        assert!(!command_is_notable(&meta("ssh sherlock", Some(255), 3600)));
        assert!(
            !command_is_notable(&meta("python3", Some(0), 600)),
            "a REPL"
        );
        assert!(command_is_notable(&meta("python3 train.py", Some(0), 600)));
        assert!(!command_is_notable(&meta("srun --pty bash", Some(0), 3600)));
        assert!(!command_is_notable(&meta("FOO=1 htop", Some(0), 600)));
    }

    #[test]
    fn secrets_are_masked_before_persisting() {
        assert_eq!(
            redact_command("export GITHUB_TOKEN=ghp_abc123 && make"),
            "export GITHUB_TOKEN=••• && make"
        );
        assert_eq!(
            redact_command("curl -H 'Authorization: Bearer xyz' https://api"),
            "curl -H 'Authorization: Bearer ••• https://api"
        );
        assert_eq!(
            redact_command("curl -H \"Authorization: s3cr3t\" x"),
            "curl -H \"Authorization: ••• x"
        );
        assert_eq!(
            redact_command("mysql -u me -p hunter2"),
            "mysql -u me -p •••"
        );
        assert_eq!(
            redact_command("tool --api-key=k1 run"),
            "tool --api-key=••• run"
        );
        assert_eq!(redact_command("snakemake -j 32"), "snakemake -j 32");
        assert_eq!(
            redact_command("curl -u me:hunter2 https://x"),
            "curl -u me:••• https://x"
        );
        assert_eq!(
            redact_command("curl --user=me:hunter2 x"),
            "curl --user=me:••• x"
        );
        assert_eq!(redact_command("squeue -u martin"), "squeue -u martin");
        assert_eq!(redact_command("mysql -phunter2 db"), "mysql -p••• db");
        assert_eq!(redact_command("mkdir -pv out"), "mkdir -pv out");
        assert_eq!(
            redact_command("git clone https://me:tok@github.com/o/r"),
            "git clone https://me:•••@github.com/o/r"
        );
        assert_eq!(
            redact_command("git push https://ghp_abc@github.com/o/r"),
            "git push https://•••@github.com/o/r"
        );
        assert_eq!(
            redact_command("open https://example.com/a@b"),
            "open https://example.com/a@b"
        );
    }

    #[test]
    fn relative_paths_are_workspace_relative_when_inside() {
        assert_eq!(relative_to(Some(Path::new("/w")), "/w/a/b.rs"), "a/b.rs");
        assert_eq!(
            relative_to(Some(Path::new("/w")), "/elsewhere/c"),
            "/elsewhere/c"
        );
    }

    #[test]
    fn only_project_roots_keep_the_job_poll_alive() {
        let home = Some(Path::new("/home/u"));
        assert!(is_project_root(Path::new("/home/u/proj"), home));
        assert!(is_project_root(Path::new("/scratch/u/proj"), home));
        assert!(!is_project_root(Path::new("/home/u"), home));
        assert!(!is_project_root(Path::new("/home"), home));
        assert!(!is_project_root(Path::new("/"), home));
        assert!(is_project_root(Path::new("/home/u"), None));
    }
}
