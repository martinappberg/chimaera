//! WebSocket bridges: /ws/sessions/{id} <-> chimaera_pty session, and the
//! /ws/events full-snapshot session bus.
//!
//! Browsers cannot set an Authorization header on a WebSocket, so the first
//! text frame must be `{"type":"auth","token":"..."}` (within 5 seconds).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use crate::AppState;

const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
/// One interactive terminal message may contain a sizeable paste, but must
/// not be allowed to use tungstenite's much larger default allocation.
const MAX_TERMINAL_INPUT_MESSAGE: usize = 1024 * 1024;
/// Structured commands can contain four 2 MiB base64 images plus a 256 KiB
/// text block. Leave room for JSON escaping and field overhead, but reject a
/// giant frame in tungstenite before serde allocates the command tree.
const MAX_CHAT_COMMAND_MESSAGE: usize = 10 * 1024 * 1024;
/// The events socket accepts only tiny watch registrations. Cap the frame
/// before serde can allocate attacker-chosen path arrays.
const MAX_EVENTS_INPUT_MESSAGE: usize = 128 * 1024;
/// Queue terminal input in bounded pieces so the PTY channel's item capacity
/// also implies a byte capacity.
const TERMINAL_INPUT_CHUNK: usize = 64 * 1024;
/// Coalesce window for repaints triggered by *other* clients' resizes: an
/// interactive divider drag fires resizes in bursts, and every repaint is a
/// full-screen rewrite.
const RESYNC_DEBOUNCE: Duration = Duration::from_millis(120);
/// PTY output coalescing window. The first chunk after an idle gap is sent
/// immediately (leading edge — typing echo never waits); chunks arriving
/// within the window after a send accumulate into one frame. A repainting TUI
/// otherwise produces one ≤8 KiB WS frame per PTY read, dozens per second,
/// and every frame costs each attached client a wakeup + parse slice.
const OUTPUT_COALESCE_WINDOW: Duration = Duration::from_millis(8);
/// Byte ceiling for one coalesced output frame; a full batch flushes without
/// waiting out the window.
const OUTPUT_COALESCE_MAX_BYTES: usize = 32 * 1024;
/// Unpark catch-up ceiling, in ring chunks: a parked connection whose
/// backlog exceeds this repaints from the authoritative grid instead of
/// replaying the ring — a long replay ships megabytes of scrollback the
/// snapshot paints in kilobytes. 64 chunks ≈ ≤512 KiB worst case, aligned
/// with the client pool's own parked-buffer bound.
const UNPARK_REPLAY_MAX_CHUNKS: usize = 64;

/// Accumulates broadcast output chunks into one WS frame. A single-chunk
/// batch is sent as the original refcounted `Bytes` (zero-copy — the same
/// buffer is shared by every attached client); only multi-chunk batches
/// concatenate into a fresh per-client allocation.
struct OutputBatch {
    chunks: Vec<Bytes>,
    bytes: usize,
}

impl OutputBatch {
    fn new() -> Self {
        OutputBatch {
            chunks: Vec::new(),
            bytes: 0,
        }
    }

    fn push(&mut self, chunk: Bytes) {
        self.bytes = self.bytes.saturating_add(chunk.len());
        self.chunks.push(chunk);
    }

    fn is_full(&self) -> bool {
        self.bytes >= OUTPUT_COALESCE_MAX_BYTES
    }

    fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Drain the batch into one frame; `None` when empty.
    fn take_frame(&mut self) -> Option<Bytes> {
        let frame = match self.chunks.len() {
            0 => None,
            1 => self.chunks.pop(),
            _ => {
                let joined = self.chunks.concat();
                self.chunks.clear();
                Some(Bytes::from(joined))
            }
        };
        self.reset_storage();
        frame
    }

    /// Discard the batch (a resync's fresh snapshot supersedes it: batched
    /// bytes are already parsed into the server grid the snapshot renders).
    fn clear(&mut self) {
        self.chunks.clear();
        self.reset_storage();
    }

    fn reset_storage(&mut self) {
        self.bytes = 0;
        // A pathological drain of many tiny chunks must not pin its
        // high-water Vec capacity for the connection's whole life.
        self.chunks.shrink_to(32);
    }
}

/// Client -> server text frames.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Auth {
        token: String,
        /// The client's current grid, adopted before the snapshot is
        /// rendered. Without it a reconnect after a dropped resize replays
        /// a snapshot at stale dims into a differently-sized xterm — every
        /// soft-wrapped row then re-wraps at the wrong column.
        #[serde(default)]
        cols: Option<u16>,
        #[serde(default)]
        rows: Option<u16>,
        /// Session WS only: the client attaches parked (a hidden pooled
        /// terminal reconnecting). The server skips the snapshot render and
        /// withholds output until `unpark`; the client's grid dims are NOT
        /// adopted (a hidden window's stale dims must never reflow the grid
        /// out from under a visible one).
        #[serde(default)]
        parked: bool,
        #[serde(flatten)]
        scope: crate::workspace_scope::Fields,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Session WS only: the client parked this terminal (hidden pooled
    /// instance). Output forwarding stops; the session's bounded broadcast
    /// ring becomes the catch-up buffer. Events still flow.
    Park,
    /// Session WS only: the terminal is shown again. Forwarding resumes from
    /// the ring; a missed foreign resize, a never-sent snapshot, or a ring
    /// overflow (`Lagged`) each repaint from the authoritative grid instead.
    Unpark,
    /// `/ws/events` only: "this window is looking at workspace W" (null when it
    /// has none). Gates the git backstop poll — see `git::WatchGuard`.
    Watch {
        #[serde(default)]
        workspace_id: Option<String>,
        /// Mounted file previews and visibly-listed directories. Both arrays
        /// are additive: older clients omit them; the daemon independently
        /// caps count, path length, and aggregate bytes before retaining them.
        #[serde(default)]
        files: Vec<String>,
        #[serde(default)]
        dirs: Vec<String>,
        /// Additive: the repositories below the root this window watches
        /// (sections open, files mounted), by top level. Only these nested
        /// repositories ride the 12 s git backstop.
        #[serde(default)]
        git_repos: Vec<String>,
    },
}

/// The first authenticated frame fixes both project scope and account generation.
/// A later account with an equal workspace/epoch cannot revive this connection.
#[derive(Clone)]
struct SocketScope {
    scope: crate::workspace_scope::Scope,
    admission: crate::workspace_scope::Mutation,
}
impl std::ops::Deref for SocketScope {
    type Target = crate::workspace_scope::Scope;
    fn deref(&self) -> &Self::Target {
        &self.scope
    }
}
impl SocketScope {
    /// The first frame's scope. One this machine cannot admit yet only because
    /// it just thawed and its own renewal of exactly that epoch is still out
    /// waits for the renewal (bounded by the resume window) instead of being
    /// refused, so the socket that woke the machine is the one that gets in.
    /// `waking` tells a viewer that understands the frame why it is quiet. A
    /// refused or unanswered renewal, or any other scope, is refused at once.
    async fn admit(
        state: &AppState,
        fields: crate::workspace_scope::Fields,
        waking: Option<&mut WebSocket>,
    ) -> anyhow::Result<Option<Self>> {
        let Some(scope) = fields.scope()? else {
            return Ok(None);
        };
        let first = Self::bind(state, scope.clone());
        if first.is_ok() || !scope.renewing(state) {
            return first.map(Some);
        }
        if let Some(socket) = waking {
            // A viewer that already left needs no wait.
            send_json(socket, &json!({"type":"waking"})).await?;
        }
        if !scope.await_renewal(state).await {
            return first.map(Some);
        }
        Self::bind(state, scope).map(Some)
    }
    fn bind(state: &AppState, scope: crate::workspace_scope::Scope) -> anyhow::Result<Self> {
        let admission = crate::workspace_scope::Mutation::for_scope(state, scope.clone())?;
        Ok(Self { scope, admission })
    }
    fn validate(&self, state: &AppState) -> anyhow::Result<()> {
        self.admission.validate(state)
    }
    fn session(&self, state: &AppState, id: &str) -> anyhow::Result<()> {
        self.admission.session(state, id)?;
        self.validate(state)
    }
    fn begin_session(
        &self,
        state: &AppState,
        id: &str,
    ) -> anyhow::Result<crate::pro::mutation::Guard> {
        self.admission.session(state, id)?;
        self.admission.begin(state)
    }
}

async fn terminal_input(
    state: &Arc<AppState>,
    id: &str,
    input: &chimaera_pty::InputSender,
    bytes: Bytes,
    scope: Option<&SocketScope>,
) -> Result<(), chimaera_pty::ExecError> {
    if let Some(scope) = scope {
        let scope = scope.clone();
        let state = Arc::clone(state);
        let id = id.to_owned();
        input
            .send_authorized(bytes, move || {
                scope
                    .begin_session(&state, &id)
                    .map_err(|_| chimaera_pty::ExecError::Busy("project connection changed".into()))
            })
            .await
    } else {
        input
            .send(bytes)
            .await
            .map_err(|_| chimaera_pty::ExecError::SessionGone)
    }
}

async fn chat_command(
    state: &Arc<AppState>,
    id: &str,
    command: chimaera_agent::model::AgentCommand,
    scope: Option<&SocketScope>,
) -> anyhow::Result<()> {
    let Some(scope) = scope else {
        return state.chat.command(id, command).await;
    };
    let guard = scope.begin_session(state, id)?;
    let state = Arc::clone(state);
    let id = id.to_owned();
    // Keep admitted enqueue work owned if its viewer disconnects. The bounded
    // wait is cancellable before enqueue; an accepted item belongs to the old
    // driver, which must be fenced before replacement authority can activate.
    tokio::spawn(async move {
        let _guard = guard;
        tokio::time::timeout(Duration::from_secs(5), state.chat.command(&id, command)).await?
    })
    .await?
}

/// Why a session with no process here is not an exit. `Moved`: it continues on
/// another machine (`to` is where it is going, in the viewer's words;
/// `"other"` is another of the user's computers, sent as `to:"computer"` with
/// the additive `other:true`).
/// `Paused`: it stays here and resumes on its own — after this daemon restarts
/// (`restarting`), once its agent is signed in on this cloud machine
/// (`needs_provider`), while its transfer finishes opening it (`importing`), or
/// never here at all (`stays_on_computer`: a plain terminal that moved with its
/// project waits for the computer). Both frames are additive; older clients
/// ignore them and reconnect. Only Pro transfer and ownership paths ever
/// populate the registries this reads, so free users never see either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pause {
    Moved(&'static str),
    Paused {
        reason: &'static str,
        provider: Option<String>,
    },
}
impl Pause {
    pub(crate) fn frame(&self) -> serde_json::Value {
        match self {
            Pause::Moved("other") => json!({"type":"moved","to":"computer","other":true}),
            Pause::Moved(to) => json!({"type":"moved","to":to}),
            Pause::Paused { reason, provider } => {
                let mut frame = json!({"type":"paused","reason":reason});
                if let Some(provider) = provider {
                    frame["provider"] = json!(provider);
                }
                frame
            }
        }
    }
}

/// What [`classify_pause`] decides from, gathered from daemon state.
#[derive(Clone, Debug, Default)]
struct PauseFacts {
    /// This daemon is a cloud machine (its sessions move to "your computer").
    worker: bool,
    /// The project's work left this computer for another of the user's
    /// computers (acting there brought it there), not for the cloud.
    other: bool,
    /// A transfer holds this session's lifecycle right now: the source is
    /// exporting it, or the destination is opening it.
    transfer: bool,
    /// The session's process is known to this daemon life.
    known: bool,
    entry: Option<EntryFacts>,
}
#[derive(Clone, Debug, Default)]
struct EntryFacts {
    /// Waiting at boot for this daemon life's ownership proof (a restart or
    /// update), not moving anywhere.
    restarting: bool,
    /// Its project is arriving here (files installing, setup running): the
    /// entry left from the original move is about to be replaced.
    arriving: bool,
    /// Imported here by a transfer and not started yet.
    arrived: bool,
    /// A plain terminal that moved with its project; it only runs on a computer.
    moved_shell: bool,
    /// The agent CLI this session runs, when it is an agent.
    provider: Option<String>,
    /// That agent is not ready on this cloud machine.
    blocked: bool,
    /// This machine may run the project right now.
    writable: bool,
}

/// Decide what a viewer of a stopped session is told. Deliberately pure so
/// every transfer phase is covered by unit tests without Pro fixtures.
///
/// A session that stopped HERE (no import) moved away only while a transfer
/// exports it or when this machine may no longer run its project; one this
/// machine still owns is waiting out a restart check. A cloud machine cannot
/// tell "restart awaiting verification" from "returned to the computer" by
/// ownership alone (both refuse writes): a session that ran in this daemon
/// life was stopped by a hand-back, one that did not is restart-deferred.
/// [`ProState`](crate::pro) exposing the ownership phase would settle both
/// without that inference.
fn classify_pause(facts: &PauseFacts, ran_here: impl FnOnce() -> bool) -> Option<Pause> {
    let away = if facts.worker {
        "computer"
    } else if facts.other {
        "other"
    } else {
        "cloud"
    };
    let Some(entry) = &facts.entry else {
        // Opening a transfer before its entry is recorded: say so rather than
        // "unknown session", which a client gives up on after a few retries.
        return (facts.transfer && !facts.known).then_some(Pause::Paused {
            reason: "importing",
            provider: None,
        });
    };
    if entry.restarting {
        return Some(Pause::Paused {
            reason: "restarting",
            provider: None,
        });
    }
    if !facts.worker && entry.arriving && !entry.arrived {
        // Taking its work back: the old entry says so until the import lands.
        return Some(Pause::Moved("computer"));
    }
    if entry.arrived {
        return Some(if facts.worker && entry.moved_shell {
            Pause::Paused {
                reason: "stays_on_computer",
                provider: None,
            }
        } else if facts.worker && entry.blocked {
            Pause::Paused {
                reason: "needs_provider",
                provider: entry.provider.clone(),
            }
        } else if !facts.worker {
            // Work coming back to this computer.
            Pause::Moved("computer")
        } else {
            Pause::Paused {
                reason: "importing",
                provider: None,
            }
        });
    }
    if facts.transfer {
        return Some(Pause::Moved(away));
    }
    if entry.writable || (facts.worker && !ran_here()) {
        return Some(Pause::Paused {
            reason: "restarting",
            provider: None,
        });
    }
    Some(Pause::Moved(away))
}

fn entry_facts(state: &AppState, entry: &crate::ledger::LedgerEntry) -> EntryFacts {
    let provider = entry
        .agent
        .as_ref()
        .map(|agent| agent.kind.as_str().to_owned());
    let blocked = provider.as_deref().is_some_and(|provider| {
        crate::pro::workspace_provider_blocks(state, &entry.workspace_id)
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|block| block["id"] == provider))
    });
    EntryFacts {
        // Recorded ownership decides the words, never a guess from which
        // processes this daemon life happens to remember.
        restarting: crate::pro::restart_deferred(state, &entry.id),
        arriving: crate::pro::ownership_phase(state, &entry.workspace_id)
            == crate::pro::Phase::Arriving,
        arrived: entry.handoff.is_some(),
        moved_shell: entry.agent.is_none()
            && entry
                .handoff
                .as_ref()
                .is_some_and(|handoff| handoff.origin == crate::bundle::Origin::Moved),
        provider,
        blocked,
        writable: crate::pro::may_write(state, &entry.workspace_id),
    }
}

/// The pause state of session `id`, if it has no process here for a reason
/// that is not an exit.
pub(crate) fn pause_state(state: &AppState, id: &str) -> Option<Pause> {
    let entry = crate::lock(&state.deferred_sessions).get(id).cloned();
    pause_for(state, id, entry.as_ref())
}

/// [`pause_state`] for a deferred entry the caller already holds (a paused
/// session row).
pub(crate) fn pause_for(
    state: &AppState,
    id: &str,
    entry: Option<&crate::ledger::LedgerEntry>,
) -> Option<Pause> {
    let facts = PauseFacts {
        worker: crate::pro::is_worker(state),
        other: crate::lock(&state.session_workspaces)
            .get(id)
            .or(entry.map(|entry| &entry.workspace_id))
            .is_some_and(|workspace| crate::pro::other_computer(state, workspace)),
        transfer: crate::lock(&state.chat_switching)
            .get(id)
            .map(String::as_str)
            == Some("transfer"),
        known: state.chat.get(id).is_some() || state.sessions.get(id).is_some(),
        entry: entry.map(|entry| entry_facts(state, entry)),
    };
    classify_pause(&facts, || {
        state.chat.get(id).is_some()
            || state.sessions.get(id).is_some()
            || state.sessions.last_words(id).is_some()
    })
}

fn pause_frame(state: &AppState, id: &str) -> Option<serde_json::Value> {
    pause_state(state, id).map(|pause| pause.frame())
}

/// This computer's own user acted in session `id` (a chat command or typing
/// on this daemon's unscoped socket): see `pro::acted_here`.
fn acted_here(state: &AppState, id: &str) {
    let workspace = crate::lock(&state.session_workspaces).get(id).cloned();
    if let Some(workspace) = workspace {
        crate::pro::acted_here(state, &workspace);
    }
}

/// Delivers, once, input a viewer relay held while it brought the work to this
/// computer (`session_proxy`): the chat commands and typing the user sent
/// before the session ran here. Each goes through the same checks as input on
/// this daemon's own socket. `Err` when the session cannot take it (nothing
/// was delivered for that frame).
pub(crate) async fn deliver_held(
    state: &Arc<AppState>,
    id: &str,
    chat: bool,
    frame: axum::extract::ws::Message,
) -> anyhow::Result<()> {
    // The resumed session may need a moment to come up after its project
    // arrived; the relay is still holding the input meanwhile.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !(if chat {
        state.chat.get(id).is_some_and(|chat| chat.alive)
    } else {
        state.sessions.get(id).is_some_and(|session| session.alive)
    }) {
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "the session did not start here"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::ensure!(session_writable(state, id), "the project is not here");
    match (chat, frame) {
        (true, Message::Text(text)) => {
            let mut command = serde_json::from_str::<chimaera_agent::model::AgentCommand>(&text)?;
            command.validate_ingress()?;
            let saved = crate::upload::save_send_images(state, id, &mut command).await;
            let interaction = crate::activity::is_interaction(&command);
            if let Err(error) = chat_command(state, id, command, None).await {
                crate::upload::discard_saved_images(saved);
                return Err(error);
            }
            if interaction {
                crate::activity::record(state, id);
                acted_here(state, id);
            }
            Ok(())
        }
        (false, Message::Binary(bytes)) => {
            let attachment = state.sessions.attach_quiet(id)?;
            for chunk in bytes.chunks(TERMINAL_INPUT_CHUNK) {
                terminal_input(
                    state,
                    id,
                    &attachment.input,
                    Bytes::copy_from_slice(chunk),
                    None,
                )
                .await
                .map_err(|error| anyhow::anyhow!("terminal input failed: {error}"))?;
            }
            crate::activity::record(state, id);
            acted_here(state, id);
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The `type` of a chat command frame (`send`, `interrupt`, `permission`…),
/// for tagging a refusal with the command it answers: a client hands text
/// back to the composer only for a refused `send`. `None` for anything that
/// is not a small command tag.
pub(crate) fn command_kind(text: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Kind {
        #[serde(rename = "type")]
        kind: String,
    }
    let kind = serde_json::from_str::<Kind>(text).ok()?.kind;
    (kind.len() <= 32 && kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')).then_some(kind)
}

/// A chat refusal naming the command it answers (additive `command`).
fn command_refusal(mut answer: serde_json::Value, text: &str) -> serde_json::Value {
    if let Some(command) = command_kind(text) {
        answer["command"] = json!(command);
    }
    answer
}

/// Input this socket may not deliver. The additive `reason` lets a client say
/// why in its own words (`watching`: the viewer chose to watch; `elsewhere`:
/// the project runs on the other machine right now), and the additive
/// `owner` (`"cloud"` | `"computer"`, [`session_owner`]) where it runs, so the
/// words need no guess; `message` stays plain.
fn refusal(state: &AppState, id: &str, watching: bool) -> serde_json::Value {
    let owner = session_owner(state, id);
    if watching {
        json!({"type":"error","code":"read_only","reason":"watching","owner":owner,
               "message":"You're watching. Take control to type."})
    } else {
        let workspace = crate::lock(&state.session_workspaces)
            .get(id)
            .cloned()
            .unwrap_or_default();
        let place = if owner == "cloud" {
            "in the cloud"
        } else if crate::pro::other_computer(state, &workspace) {
            "on your other computer"
        } else {
            "on your computer"
        };
        json!({"type":"error","code":"read_only","reason":"elsewhere","owner":owner,
               "message":format!("This project is running {place} right now. That was not sent.")})
    }
}

/// Where the project of session `id` runs now: `"cloud"` or `"computer"`
/// (`pro::owner_kind`; a session with no project runs where this daemon is).
pub(crate) fn session_owner(state: &AppState, id: &str) -> &'static str {
    let workspace = crate::lock(&state.session_workspaces)
        .get(id)
        .cloned()
        .unwrap_or_default();
    crate::pro::owner_kind(state, &workspace)
}

pub(crate) fn session_writable(state: &AppState, id: &str) -> bool {
    let workspace = crate::lock(&state.session_workspaces).get(id).cloned();
    workspace.is_none_or(|workspace| crate::pro::may_execute(state, &workspace))
}

/// GET /ws/sessions/{id}
pub(crate) async fn session_ws(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    Query(options): Query<crate::session_proxy::SocketOptions>,
) -> Response {
    ws.max_message_size(MAX_TERMINAL_INPUT_MESSAGE)
        .max_frame_size(MAX_TERMINAL_INPUT_MESSAGE)
        .on_upgrade(move |socket| handle(socket, id, state, options))
}

async fn handle(
    mut socket: WebSocket,
    id: String,
    state: Arc<AppState>,
    options: crate::session_proxy::SocketOptions,
) {
    let auth = match authenticate(&mut socket, &state, true).await {
        Ok(auth) => auth,
        Err(denied) => {
            denied.answer(&mut socket).await;
            return;
        }
    };
    let scope = auth.scope.clone();
    if scope
        .as_ref()
        .is_some_and(|scope| scope.session(&state, &id).is_err())
    {
        scope_changed(&mut socket).await;
        return;
    }
    let remote_auth = json!({"type":"auth", "token":"", "cols":auth.dims.map(|d| d.0), "rows":auth.dims.map(|d| d.1), "parked":auth.parked});
    if scope.is_none()
        && crate::session_proxy::socket(&state, &id, "sessions", &options, remote_auth, &mut socket)
            .await
    {
        return;
    }
    let auth_dims = if auth.parked || options.read_only || !session_writable(&state, &id) {
        None
    } else {
        auth.dims
    };

    // Adopt the client's grid BEFORE attaching so the snapshot below is
    // rendered at the size the client will actually display it. A parked
    // attach adopts nothing: a hidden window's stale dims must never reflow
    // the grid out from under a visible one.
    if let Some((cols, rows)) = auth_dims {
        if !resize_off_reactor(&state, &id, cols, rows, "pre-attach resize", scope.as_ref()).await {
            scope_changed(&mut socket).await;
            return;
        }
    }

    let attach_res = match attach_off_reactor(&state, &id, auth.parked).await {
        Ok(res) => res,
        Err(err) => {
            // A panicked render task is an internal failure, not session
            // death: close (the client's reconnect retries) rather than
            // replaying last words for a session that may be alive.
            tracing::warn!(%id, %err, "attach render task failed");
            return;
        }
    };
    let mut attachment = match attach_res {
        // A stopped process still registered for a moment after a transfer
        // stop is not an exit to replay: the viewer follows the session.
        Ok(attachment) if !attachment.info.alive && pause_frame(&state, &id).is_some() => {
            if let Some(frame) = pause_frame(&state, &id) {
                let _ = send_json(&mut socket, &frame).await;
            }
            return;
        }
        Ok(attachment) => attachment,
        Err(err) => {
            // Paused for a transfer: not an exit, and its last screen is not
            // its last words. The viewer follows it to its new owner.
            if let Some(frame) = pause_frame(&state, &id) {
                let _ = send_json(&mut socket, &frame).await;
                return;
            }
            // A session that died before this client could attach (fast
            // agent failures — a missing API key kills codex in ~400ms)
            // still gets an honest pane: replay the final screen once,
            // then close as exited. Blank panes teach nothing.
            if let Some(words) = state.sessions.last_words(&id) {
                let mut ready = match serde_json::to_value(&words.info) {
                    Ok(serde_json::Value::Object(map)) => map,
                    _ => serde_json::Map::new(),
                };
                ready.insert("type".to_string(), json!("ready"));
                ready.insert("cwd_current".to_string(), json!(words.info.cwd.clone()));
                let mut ready = serde_json::Value::Object(ready);
                if let Some(alias) = scope
                    .as_ref()
                    .and_then(|scope| scope.alias(&state).ok().flatten())
                {
                    alias.session(&mut ready);
                }
                if send_json(&mut socket, &ready).await.is_err() {
                    return;
                }
                // A parked client discards any snapshot on this connection
                // (its buffer desynced at the parked ready) and replays last
                // words via the fresh visible attach its adopt makes — don't
                // render bytes it is guaranteed to drop.
                if !auth.parked
                    && socket
                        .send(Message::Binary(Bytes::from(words.snapshot)))
                        .await
                        .is_err()
                {
                    return;
                }
                let _ = send_json(
                    &mut socket,
                    &json!({"type": "exited", "status": words.info.exit_status}),
                )
                .await;
                return;
            }
            tracing::debug!(%id, %err, "ws attach failed");
            let _ = send_json(
                &mut socket,
                // Retryable: mid view-switch the id exists but its process
                // is being respawned; clients back off and re-attach.
                &json!({"type": "error", "code": "unknown_session",
                        "message": format!("unknown session {id}")}),
            )
            .await;
            return;
        }
    };

    // Ready frame: {"type":"ready", ...SessionInfo fields..., "cwd_current"}
    let mut ready = match serde_json::to_value(&attachment.info) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    ready.insert("type".to_string(), json!("ready"));
    // Same field as REST/events session JSON: the polled cwd (shell naming
    // watcher), falling back to the spawn cwd.
    let cwd_current = crate::lock(&state.current_cwds)
        .get(&id)
        .cloned()
        .unwrap_or_else(|| attachment.info.cwd.clone());
    ready.insert("cwd_current".to_string(), json!(cwd_current));
    let mut ready = serde_json::Value::Object(ready);
    if let Some(alias) = scope
        .as_ref()
        .and_then(|scope| scope.alias(&state).ok().flatten())
    {
        alias.session(&mut ready);
    }
    if send_json(&mut socket, &ready).await.is_err() {
        return;
    }

    // Snapshot as one binary frame, then enter the bridge loop. Adjacency is
    // a contract: after any reset-bearing frame (`ready` here, `resync` in
    // repaint) the NEXT binary frame is the complete snapshot — the client's
    // parked write-through path relies on nothing interleaving. A parked
    // attach sends none (attach_quiet rendered none): the first unpark
    // repaints instead.
    if !auth.parked {
        let snapshot = Bytes::from(std::mem::take(&mut attachment.snapshot));
        if socket.send(Message::Binary(snapshot)).await.is_err() {
            return;
        }
    }

    let mut output_open = true;
    let mut events_open = true;
    // Parked: output forwarding is off (the select arm below is disabled),
    // and the session's bounded broadcast ring holds the stream. The ring is
    // shared by every attachment, so N parked windows don't multiply it —
    // but a non-consuming receiver does keep the ring's rolling window of
    // recent chunks alive (bounded at OUTPUT_CHANNEL_CAPACITY × chunk size;
    // the same retention any slow attachment already imposes, freed as the
    // ring wraps).
    let mut parked = auth.parked;
    // The next unpark must repaint instead of resuming the ring: the client
    // has no grid yet (parked attach — no snapshot was sent), or the grid
    // reflowed while parked (foreign resize; a pending resync consumed by
    // park), so the ring's bytes cannot catch it up. True only while parked.
    let mut unpark_repaint = auth.parked;
    // Dims this connection itself asked for. Its xterm reflowed natively when
    // it resized, so a Resized event echoing these back needs no repaint —
    // resyncing the initiator is exactly the "terminal resets when I change
    // the font size" bug. Seeded from auth so the pre-attach adopt above
    // doesn't count as foreign.
    let mut client_dims: Option<(u16, u16)> = auth_dims;
    // Pending repaint for a *foreign* resize (another window attached to the
    // same session), debounced so drag bursts coalesce into one repaint.
    let mut resync_at: Option<tokio::time::Instant> = None;
    // Output coalescing (see OUTPUT_COALESCE_WINDOW): `flush_at` armed means
    // a frame just went out — chunks batch until the window elapses or the
    // batch fills; disarmed means the stream went idle and the next chunk is
    // sent immediately.
    let mut batch = OutputBatch::new();
    let mut flush_at: Option<tokio::time::Instant> = None;
    // One reusable timer per debounce, reset in place when (re)armed:
    // re-creating a Sleep every loop iteration would churn the timer wheel
    // on the output hot path. The `is_some()` guards keep an elapsed,
    // un-reset Sleep from being polled again.
    let resync_sleep = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(resync_sleep);
    let flush_sleep = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(flush_sleep);
    let mut scope_tick = tokio::time::interval(Duration::from_secs(1));
    scope_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = scope_tick.tick(), if scope.is_some() => {
                if scope.as_ref().is_some_and(|s| s.session(&state, &id).is_err()) { scope_changed(&mut socket).await; return; }
            },
            _ = &mut resync_sleep, if resync_at.is_some() => {
                resync_at = None;
                if !repaint(&mut socket, &id, &state, &mut attachment,
                            &mut batch, &mut flush_at, &mut output_open).await {
                    return;
                }
            },
            _ = &mut flush_sleep, if flush_at.is_some() => {
                if batch.is_empty() {
                    // The window elapsed idle: disarm so the next chunk leads.
                    flush_at = None;
                } else {
                    if !send_batch(&mut socket, &mut batch).await {
                        return;
                    }
                    // Still streaming: keep flushing at the window cadence.
                    let at = tokio::time::Instant::now() + OUTPUT_COALESCE_WINDOW;
                    flush_sleep.as_mut().reset(at);
                    flush_at = Some(at);
                }
            },
            out = attachment.output.recv(), if output_open && !parked => match out {
                Ok(bytes) => {
                    batch.push(bytes);
                    // Fold in whatever is already queued — batching without
                    // waiting (the wait, when any, is the flush timer's).
                    let mut drain_err: Option<TryRecvError> = None;
                    while !batch.is_full() {
                        match attachment.output.try_recv() {
                            Ok(more) => batch.push(more),
                            Err(TryRecvError::Empty) => break,
                            Err(err) => {
                                drain_err = Some(err);
                                break;
                            }
                        }
                    }
                    if matches!(drain_err, Some(TryRecvError::Lagged(_))) {
                        tracing::debug!(%id, "ws output lagged; resyncing");
                        resync_at = None;
                        if !repaint(&mut socket, &id, &state, &mut attachment,
                                    &mut batch, &mut flush_at, &mut output_open).await {
                            return;
                        }
                    } else {
                        let closed = matches!(drain_err, Some(TryRecvError::Closed));
                        if flush_at.is_none() || batch.is_full() || closed {
                            if !send_batch(&mut socket, &mut batch).await {
                                return;
                            }
                            flush_at = if closed {
                                None
                            } else {
                                let at = tokio::time::Instant::now() + OUTPUT_COALESCE_WINDOW;
                                flush_sleep.as_mut().reset(at);
                                Some(at)
                            };
                        }
                        if closed {
                            output_open = false;
                        }
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    tracing::debug!(%id, skipped, "ws output lagged; resyncing");
                    resync_at = None;
                    if !repaint(&mut socket, &id, &state, &mut attachment,
                                &mut batch, &mut flush_at, &mut output_open).await {
                        return;
                    }
                }
                Err(RecvError::Closed) => {
                    // The child died inside a window: the batched tail is its
                    // last words — flush before going quiet.
                    if !send_batch(&mut socket, &mut batch).await {
                        return;
                    }
                    flush_at = None;
                    output_open = false;
                }
            },
            event = attachment.events.recv(), if events_open => match event {
                Ok(event) => {
                    // Stopped for a transfer: say it moved (after its final
                    // output), not that it exited. A scoped viewer whose
                    // project connection changed hears that first: it must
                    // re-read where the project runs before following it.
                    if matches!(event, chimaera_pty::SessionEvent::Exited { .. }) {
                        if scope.as_ref().is_some_and(|s| s.session(&state, &id).is_err()) {
                            scope_changed(&mut socket).await;
                            return;
                        }
                        if let Some(frame) = pause_frame(&state, &id) {
                            let _ = send_ordered_json(&mut socket, &mut batch, &frame).await;
                            return;
                        }
                    }
                    let resized_to = match &event {
                        chimaera_pty::SessionEvent::Resized { cols, rows } => Some((*cols, *rows)),
                        _ => None,
                    };
                    // Exited while parked: do NOT drain the ring here. The
                    // Exited broadcast races the reader thread's final bytes
                    // (session.rs sends the event, then sleeps, then reaps),
                    // and a lagged/overflowing ring cannot promise a coherent
                    // escape stream anyway. The honest final screen comes
                    // from the last-words replay: the client latches its
                    // parked buffer desynced on an exit while parked, and
                    // adopt resyncs into a fresh attach that replays it.
                    match serde_json::to_value(&event) {
                        Ok(value) => {
                            // Ordered send: batched output first, so an event
                            // never overtakes the bytes it postdates.
                            if !send_ordered_json(&mut socket, &mut batch, &value).await {
                                return;
                            }
                        }
                        Err(err) => tracing::warn!(%id, %err, "failed to serialize session event"),
                    }
                    // A resize this connection did NOT request reflowed the
                    // server grid out from under the client's xterm; repaint
                    // from the authoritative grid (tmux redraw semantics).
                    // The initiator is skipped: its xterm already reflowed.
                    // Parked, the repaint is deferred to unpark instead —
                    // rendering a snapshot nobody displays is the waste this
                    // protocol exists to avoid.
                    if let Some(dims) = resized_to {
                        if client_dims != Some(dims) {
                            if parked {
                                unpark_repaint = true;
                            } else {
                                let at = tokio::time::Instant::now() + RESYNC_DEBOUNCE;
                                resync_sleep.as_mut().reset(at);
                                resync_at = Some(at);
                            }
                        }
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => events_open = false,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(bytes))) => {
                    if scope.as_ref().is_some_and(|s| s.session(&state, &id).is_err()) { scope_changed(&mut socket).await; return; }
                    if options.read_only || !session_writable(&state, &id) {
                        let _ = send_ordered_json(&mut socket, &mut batch, &refusal(&state, &id, options.read_only)).await;
                        continue;
                    }
                    let mut interacted = false;
                    for chunk in bytes.chunks(TERMINAL_INPUT_CHUNK) {
                        if !session_writable(&state, &id) || scope.as_ref().is_some_and(|s| s.session(&state, &id).is_err()) { scope_changed(&mut socket).await; return; }
                        if let Err(error) = terminal_input(&state, &id, &attachment.input, Bytes::copy_from_slice(chunk), scope.as_ref()).await {
                            if scope.as_ref().is_some_and(|s| s.session(&state, &id).is_err()) {
                                scope_changed(&mut socket).await;
                                return;
                            }
                            if matches!(error, chimaera_pty::ExecError::Busy(_)) {
                                let _ = send_ordered_json(&mut socket, &mut batch, &json!({"type":"error","code":"read_only","reason":"busy","owner":session_owner(&state, &id),"message":"Your project is busy. Wait a moment before typing again."})).await;
                                break;
                            }
                            // Session is gone; flush the batched tail (its
                            // last words), tell the client, and hang up.
                            let gone = pause_frame(&state, &id)
                                .unwrap_or_else(|| json!({"type": "exited", "status": null}));
                            let _ = send_ordered_json(&mut socket, &mut batch, &gone).await;
                            return;
                        }
                        if !interacted {
                            crate::activity::record(&state, &id);
                            // This computer's own user typed (not a forwarded
                            // viewer): the last actor keeps the work here.
                            if scope.is_none() { acted_here(&state, &id); }
                            interacted = true;
                        }
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    if scope.as_ref().is_some_and(|s| s.session(&state, &id).is_err()) { scope_changed(&mut socket).await; return; }
                    match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::Resize { cols, rows }) => {
                            if options.read_only || !session_writable(&state, &id) { continue; }
                            client_dims = Some((cols, rows));
                            // Flush output rendered at the old width before
                            // the grid reflows under it — the initiator is
                            // excluded from resync, so an inverted flush
                            // here would never be repaired.
                            if !send_batch(&mut socket, &mut batch).await {
                                return;
                            }
                            if !resize_off_reactor(&state, &id, cols, rows, "ws resize", scope.as_ref()).await {
                                scope_changed(&mut socket).await;
                                return;
                            }
                        }
                        Ok(ClientMessage::Park) => {
                            if !parked {
                                parked = true;
                                // Bytes already read from the ring must not
                                // be dropped — the client buffers them.
                                if !send_batch(&mut socket, &mut batch).await {
                                    return;
                                }
                                flush_at = None;
                                // A pending foreign-resize repaint defers to
                                // the unpark repaint.
                                if resync_at.take().is_some() {
                                    unpark_repaint = true;
                                }
                            }
                        }
                        Ok(ClientMessage::Unpark) => {
                            if parked {
                                parked = false;
                                // Repaint when the ring can't (or shouldn't)
                                // catch the client up: no grid yet / reflow
                                // while parked, or a backlog past the replay
                                // ceiling — one snapshot paints in KBs what
                                // a long replay ships in MBs. len() counts
                                // sent-minus-received (it can exceed the
                                // ring's capacity when lagged), which is
                                // exactly the "too much happened" signal.
                                if unpark_repaint
                                    || attachment.output.len() > UNPARK_REPLAY_MAX_CHUNKS
                                {
                                    unpark_repaint = false;
                                    if !repaint(&mut socket, &id, &state, &mut attachment,
                                                &mut batch, &mut flush_at, &mut output_open).await {
                                        return;
                                    }
                                }
                                // Otherwise the re-enabled output arm resumes
                                // from the ring; an overflow surfaces as
                                // Lagged there and repaints through the
                                // existing path.
                            }
                        }
                        // Ignore re-auth, the events-bus `watch` frame, and
                        // unknown message types.
                        Ok(ClientMessage::Auth { .. }) | Ok(ClientMessage::Watch { .. }) | Err(_) => {}
                    }
                }
                // Client went away: drop the attachment, the session lives on.
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {} // ping/pong are handled by axum
            },
        }
    }
}

/// `SessionManager::attach` renders the whole scrollback under the term lock
/// (~95 ms measured client-visible at the 10k-line default; the cap is 200k)
/// — blocking work that must never run on a reactor worker. Outer `Err` =
/// the render task itself failed (a panic), inner `Err` = unknown session.
async fn attach_off_reactor(
    state: &Arc<AppState>,
    id: &str,
    quiet: bool,
) -> Result<anyhow::Result<chimaera_pty::Attachment>, tokio::task::JoinError> {
    let state = Arc::clone(state);
    let id = id.to_string();
    tokio::task::spawn_blocking(move || {
        if quiet {
            // Parked attach: subscribe only, no snapshot render — the first
            // unpark repaints via a fresh full attach.
            state.sessions.attach_quiet(&id)
        } else {
            state.sessions.attach(&id)
        }
    })
    .await
}

/// `resize` winches the PTY and reflows the headless grid under the same
/// term lock the snapshot render holds — off the reactor for the same
/// reason. Failures are logged, not fatal (resizes are advisory; a dead or
/// unknown session simply ignores them).
async fn resize_off_reactor(
    state: &Arc<AppState>,
    id: &str,
    cols: u16,
    rows: u16,
    what: &str,
    scope: Option<&SocketScope>,
) -> bool {
    let state = Arc::clone(state);
    let task_id = id.to_string();
    let scope = scope.cloned();
    match tokio::task::spawn_blocking(move || {
        let _guard = scope
            .as_ref()
            .map(|scope| scope.begin_session(&state, &task_id))
            .transpose()?;
        state.sessions.resize(&task_id, cols, rows)
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(err)) if err.is::<crate::pro::mutation::Changed>() => return false,
        Ok(Err(err)) => tracing::debug!(%id, %err, "{what} failed"),
        Err(err) => tracing::warn!(%id, %err, "{what} task failed"),
    }
    true
}

/// Send the batched output as one frame, if any; false = socket gone.
async fn send_batch(socket: &mut WebSocket, batch: &mut OutputBatch) -> bool {
    match batch.take_frame() {
        Some(frame) => socket.send(Message::Binary(frame)).await.is_ok(),
        None => true,
    }
}

/// Send a JSON side-channel frame, flushing batched output FIRST: an event
/// must never overtake the bytes it postdates (`exited` before the final
/// output, `resized` before pre-reflow output — and the resize initiator is
/// excluded from resync, so an inverted flush there would never be
/// repaired). False = socket gone.
async fn send_ordered_json(
    socket: &mut WebSocket,
    batch: &mut OutputBatch,
    value: &serde_json::Value,
) -> bool {
    send_batch(socket, batch).await && send_json(socket, value).await.is_ok()
}

/// Repaint the client from the authoritative grid: fresh attach (off the
/// reactor), a dims-tagged resync frame (the client resizes BEFORE replaying
/// — a snapshot replayed at any other width re-wraps into garbage), then the
/// snapshot, sent adjacent to the resync frame (the client's parked
/// write-through path relies on the next binary frame after a reset being
/// the complete snapshot). Batched output is discarded first: those bytes
/// were already parsed into the grid this snapshot renders. The events
/// subscription is deliberately kept: swapping it could drop an Exited/Title
/// event broadcast during the swap; the output receiver IS swapped, so it is
/// re-opened even after a Closed. Returns false when the connection is done
/// — socket gone, or the re-attach failed (closing lets the client's normal
/// reconnect self-heal with a fresh snapshot instead of silently streaming
/// onto a stale grid whose resync trigger was already consumed).
async fn repaint(
    socket: &mut WebSocket,
    id: &str,
    state: &Arc<AppState>,
    attachment: &mut chimaera_pty::Attachment,
    batch: &mut OutputBatch,
    flush_at: &mut Option<tokio::time::Instant>,
    output_open: &mut bool,
) -> bool {
    batch.clear();
    *flush_at = None;
    let mut fresh = match attach_off_reactor(state, id, false).await {
        Ok(Ok(fresh)) => fresh,
        Ok(Err(err)) => {
            tracing::debug!(%id, %err, "resync attach failed; closing for a clean reconnect");
            return false;
        }
        Err(err) => {
            tracing::warn!(%id, %err, "resync render task failed; closing for a clean reconnect");
            return false;
        }
    };
    let frame = json!({
        "type": "resync",
        "cols": fresh.info.cols,
        "rows": fresh.info.rows,
    });
    if send_json(socket, &frame).await.is_err() {
        return false;
    }
    let snapshot = Bytes::from(std::mem::take(&mut fresh.snapshot));
    if socket.send(Message::Binary(snapshot)).await.is_err() {
        return false;
    }
    attachment.info = fresh.info;
    attachment.output = fresh.output;
    attachment.input = fresh.input;
    *output_open = true;
    true
}

/// GET /ws/chat/{id} — the structured chat bridge: JSON events out (seq-
/// numbered, gap-replayed from the journal), AgentCommands in. The chat
/// sibling of /ws/sessions/{id}; deliberately a separate endpoint — none of
/// the PTY channel's byte-pipe semantics (binary frames, dims, resync)
/// apply here.
pub(crate) async fn chat_ws(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
    Query(options): Query<crate::session_proxy::SocketOptions>,
) -> Response {
    ws.max_message_size(MAX_CHAT_COMMAND_MESSAGE)
        .max_frame_size(MAX_CHAT_COMMAND_MESSAGE)
        .on_upgrade(move |socket| handle_chat(socket, id, state, options))
}

/// Chat replay batch size: bounds per-frame size without flooding the socket
/// with one frame per event on a cold attach.
const CHAT_BATCH: usize = 128;
/// Byte budget per replay frame. Count-only batching admitted 128 maximum-size
/// journal entries into one ~32 MiB JSON frame, creating a large allocation
/// and long main-thread parse pause on cold reconnect. One entry may approach
/// the journal's 256 KiB cap; otherwise frames stay near this ceiling.
const CHAT_BATCH_BYTES: usize = 512 * 1024;

async fn handle_chat(
    mut socket: WebSocket,
    id: String,
    state: Arc<AppState>,
    options: crate::session_proxy::SocketOptions,
) {
    let (last_seq, scope) = match chat_authenticate(&mut socket, &state).await {
        Ok(auth) => auth,
        Err(denied) => {
            denied.answer(&mut socket).await;
            return;
        }
    };

    if scope
        .as_ref()
        .is_some_and(|s| s.session(&state, &id).is_err())
    {
        scope_changed(&mut socket).await;
        return;
    }
    if scope.is_none()
        && crate::session_proxy::socket(
            &state,
            &id,
            "chat",
            &options,
            json!({"type":"auth","token":"","last_seq":last_seq}),
            &mut socket,
        )
        .await
    {
        return;
    }

    // A conversation paused for a transfer continues elsewhere: never greet
    // the viewer with its stopped driver (`alive:false` reads as "exited").
    if !state.chat.get(&id).is_some_and(|chat| chat.alive) {
        if let Some(frame) = pause_frame(&state, &id) {
            let _ = send_json(&mut socket, &frame).await;
            return;
        }
    }
    // Replay may read the journal file — keep it off the reactor.
    let attachment = {
        let state = state.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || state.chat.attach(&id, last_seq)).await
    };
    let attachment = match attachment {
        Ok(Ok(attachment)) => attachment,
        _ => {
            let _ = send_json(
                &mut socket,
                // Retryable: mid view-switch the driver may not be up yet.
                &json!({"type": "error", "code": "unknown_session",
                        "message": format!("unknown chat session {id}")}),
            )
            .await;
            return;
        }
    };

    let ready = json!({
        "type": "ready",
        "session": attachment.info,
        // Tell every client the cursor the daemon actually honored. This can
        // differ from auth.last_seq after a server-side journal reset.
        "replay_from": attachment.replay_from,
        // The journal's highest seq now. A client whose own last_seq exceeds
        // this is stale (the journal was recreated and numbering restarted);
        // it hard-resets rather than silently dropping every replayed event.
        "head": attachment.head_seq,
    });
    if send_json(&mut socket, &ready).await.is_err() {
        return;
    }

    // `attach` may clamp a stale client cursor back to 0 when its journal was
    // recreated. Dedupe live events against that effective cursor: if replay
    // is empty, retaining the client's old (higher) seq would silently skip
    // every event the new journal ever emits.
    let mut sent_seq = attachment.replay_from;
    if !send_chat_batches(&mut socket, &attachment.replay, &mut sent_seq).await {
        return;
    }

    let mut live = attachment.live;
    let mut scope_tick = tokio::time::interval(Duration::from_secs(1));
    scope_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = scope_tick.tick(), if scope.is_some() => {
                if scope.as_ref().is_some_and(|s| s.session(&state, &id).is_err()) { scope_changed(&mut socket).await; return; }
            },
            event = live.recv() => match event {
                Ok(entry) => {
                    // The replay tail can overlap the subscription start.
                    if entry.seq <= sent_seq {
                        continue;
                    }
                    let frame = json!({"type": "ev", "seq": entry.seq, "ts": entry.ts, "ev": entry.ev});
                    if send_json(&mut socket, &frame).await.is_err() {
                        return;
                    }
                    sent_seq = entry.seq;
                }
                Err(RecvError::Lagged(skipped)) => {
                    // Slow client: re-replay the gap from the journal instead
                    // of buffering (same philosophy as the PTY resync).
                    tracing::debug!(%id, skipped, "chat ws lagged; replaying gap");
                    let replayed = {
                        let state = state.clone();
                        let id = id.clone();
                        let from = sent_seq;
                        tokio::task::spawn_blocking(move || state.chat.attach(&id, from)).await
                    };
                    match replayed {
                        Ok(Ok(fresh)) => {
                            live = fresh.live;
                            if !send_chat_batches(&mut socket, &fresh.replay, &mut sent_seq).await {
                                return;
                            }
                        }
                        _ => return,
                    }
                }
                Err(RecvError::Closed) => {
                    // Driver gone. Decide what to tell the client:
                    // - mid view-switch (chat_switching holds the id): the
                    //   respawn is in flight but not registered yet (journal
                    //   append + launcher::detect take up to ~2s on a cold
                    //   cache), so DON'T report "exited" — say "degraded" for a
                    //   term target, or a retryable frame for a chat target.
                    // - a PTY already under this id: it degraded/toggled to a
                    //   terminal.
                    // - otherwise: the session genuinely exited.
                    // - stopped for a transfer: it moved, it did not exit
                    //   (a scoped viewer whose connection changed hears that
                    //   first, to re-read where the project runs).
                    if scope.as_ref().is_some_and(|s| s.session(&state, &id).is_err()) {
                        scope_changed(&mut socket).await;
                        return;
                    }
                    let switching = crate::lock(&state.chat_switching).get(&id).cloned();
                    let frame = pause_frame(&state, &id).unwrap_or_else(|| match switching.as_deref() {
                        Some("term") => json!({"type": "degraded"}),
                        Some(_) => json!({"type": "error", "code": "unknown_session",
                                          "message": "session switching"}),
                        None if state.sessions.get(&id).is_some() => json!({"type": "degraded"}),
                        None => json!({"type": "exited",
                                       "status": state.chat.get(&id).and_then(|c| c.exit_status)}),
                    });
                    let _ = send_json(&mut socket, &frame).await;
                    return;
                }
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => {
                    if scope.as_ref().is_some_and(|s| s.session(&state, &id).is_err()) { scope_changed(&mut socket).await; return; }
                    match serde_json::from_str::<chimaera_agent::model::AgentCommand>(&text) {
                        Ok(mut cmd) => {
                            if options.read_only || !session_writable(&state, &id) {
                                let _ = send_json(&mut socket, &command_refusal(refusal(&state, &id, options.read_only), &text)).await;
                                continue;
                            }
                            if let Err(err) = cmd.validate_ingress() {
                                tracing::debug!(%id, %err, "chat command exceeds ingress budget");
                                // Reject only this command. The authenticated
                                // socket and agent remain healthy, so the UI
                                // can correct the payload and retry.
                                let _ = send_json(
                                    &mut socket,
                                    &command_refusal(json!({"type": "error", "code": "invalid_command",
                                            "message": err.to_string()}), &text),
                                )
                                .await;
                                continue;
                            }
                            // A send's images get a saved copy the echoed
                            // message can show after replay.
                            let saved = crate::upload::save_send_images(&state, &id, &mut cmd).await;
                            if !session_writable(&state, &id) || scope.as_ref().is_some_and(|s| s.session(&state, &id).is_err()) {
                                crate::upload::discard_saved_images(saved);
                                scope_changed(&mut socket).await;
                                return;
                            }
                            let interaction = crate::activity::is_interaction(&cmd);
                            if let Err(err) = chat_command(&state, &id, cmd, scope.as_ref()).await {
                                // The send never happened: neither do its copies.
                                crate::upload::discard_saved_images(saved);
                                if err.is::<crate::pro::mutation::Changed>() {
                                    scope_changed(&mut socket).await;
                                    return;
                                }
                                tracing::debug!(%id, %err, "chat command failed");
                                // code=command_failed: one refused command is
                                // NOT a dead socket — without the code the
                                // client treats this frame as fatal and stops
                                // reconnecting forever (additive field; old
                                // clients ignore unknown codes and keep their
                                // previous behavior).
                                let (code, message) = if err
                                    .downcast_ref::<chimaera_agent::CommandQueueFull>()
                                    .is_some()
                                {
                                    ("invalid_command", err.to_string())
                                } else {
                                    ("command_failed", "agent unavailable".to_string())
                                };
                                let _ = send_json(
                                    &mut socket,
                                    &command_refusal(json!({"type": "error", "code": code,
                                            "message": message}), &text),
                                )
                                .await;
                            } else if interaction {
                                crate::activity::record(&state, &id);
                                if scope.is_none() {
                                    acted_here(&state, &id);
                                }
                            }
                        }
                        Err(err) => {
                            tracing::debug!(%id, %err, "unparseable chat frame");
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

/// Ship replay entries in bounded batches, advancing `sent_seq`.
async fn send_chat_batches(
    socket: &mut WebSocket,
    replay: &[Arc<chimaera_agent::journal::SeqEvent>],
    sent_seq: &mut u64,
) -> bool {
    let mut start = 0;
    while start < replay.len() {
        let end = chat_batch_end(replay, start);
        let chunk = &replay[start..end];
        let events: Vec<serde_json::Value> = chunk
            .iter()
            .map(|e| json!({"seq": e.seq, "ts": e.ts, "ev": e.ev}))
            .collect();
        if send_json(socket, &json!({"type": "batch", "events": events}))
            .await
            .is_err()
        {
            return false;
        }
        if let Some(last) = chunk.last() {
            *sent_seq = last.seq;
        }
        start = end;
    }
    true
}

/// End index for the next replay batch, bounded by both entry count and
/// serialized bytes. Always admits at least one entry so a single large (but
/// journal-valid) event makes progress.
fn chat_batch_end(replay: &[Arc<chimaera_agent::journal::SeqEvent>], start: usize) -> usize {
    let mut bytes = 0usize;
    let mut end = start;
    let count_end = replay.len().min(start.saturating_add(CHAT_BATCH));
    while end < count_end {
        // SeqEvent is the same three fields the batch embeds. The surrounding
        // array/object punctuation is tiny; leave a small fixed allowance per
        // row so the target remains an honest upper bound in practice.
        let next = serde_json::to_vec(&*replay[end])
            .map(|v| v.len().saturating_add(2))
            .unwrap_or(CHAT_BATCH_BYTES);
        if end > start && bytes.saturating_add(next) > CHAT_BATCH_BYTES {
            break;
        }
        bytes = bytes.saturating_add(next);
        end += 1;
    }
    end.max(start.saturating_add(1).min(replay.len()))
}

/// First-frame auth for the chat channel: carries `last_seq` instead of grid
/// dims. `None` = rejected.
async fn chat_authenticate(
    socket: &mut WebSocket,
    state: &AppState,
) -> Result<(u64, Option<SocketScope>), Denied> {
    #[derive(Deserialize)]
    struct ChatAuth {
        #[serde(rename = "type")]
        kind: String,
        token: String,
        #[serde(default)]
        last_seq: u64,
        #[serde(flatten)]
        scope: crate::workspace_scope::Fields,
    }
    let Ok(Some(Ok(Message::Text(text)))) = tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await
    else {
        return Err(Denied::Token);
    };
    match serde_json::from_str::<ChatAuth>(&text) {
        Ok(auth) if auth.kind == "auth" && auth.token == state.token => Ok((
            auth.last_seq,
            SocketScope::admit(state, auth.scope, Some(socket))
                .await
                .map_err(|_| Denied::Scope)?,
        )),
        _ => Err(Denied::Token),
    }
}

/// GET /ws/events — the session bus. After first-frame auth the server sends
/// a full `{"type":"sessions","sessions":[...]}` snapshot immediately and
/// again (throttled to at most 4/s) whenever any session appears, disappears,
/// or changes state/title. Dead simple full-snapshot protocol; no diffs.
pub(crate) async fn events_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.max_message_size(MAX_EVENTS_INPUT_MESSAGE)
        .max_frame_size(MAX_EVENTS_INPUT_MESSAGE)
        .on_upgrade(move |socket| handle_events(socket, state))
}

/// The retryable project-scope refusal. Every caller returns right after, so
/// it closes the socket properly too: without a close frame the viewer (or
/// the gateway relaying it) saw a reset without a closing handshake.
async fn scope_changed(socket: &mut WebSocket) {
    let _ = send_json(socket, &json!({"type":"error","code":"workspace_scope_changed","message":"Your project connection changed. Reconnecting…"})).await;
    let _ = socket
        .send(Message::Close(Some(axum::extract::ws::CloseFrame {
            code: axum::extract::ws::close_code::AGAIN,
            reason: "workspace_scope_changed".into(),
        })))
        .await;
}

async fn scoped_events(mut socket: WebSocket, state: Arc<AppState>, scope: SocketScope) {
    if scope.validate(&state).is_err() {
        scope_changed(&mut socket).await;
        return;
    }
    state.wait_restored().await;
    let alias = match scope.alias(&state) {
        Ok(alias) => alias,
        Err(_) => return,
    };
    let mut watch = crate::git::WatchGuard::new(state.clone());
    watch.set(Some(scope.workspace_id.clone()));
    let mut files = crate::fs_watch::FsWatch::new();
    let mut last = String::new();
    let mut settings = None;
    let mut epochs_sent = std::collections::HashMap::new();
    let mut recents = None;
    let mut last_notice = state.notices.head();
    let mut tick = tokio::time::interval(EVENTS_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if scope.validate(&state).is_err() {
            scope_changed(&mut socket).await;
            return;
        }
        let frame = json!({"type":"sessions","sessions":crate::workspace_scope::sessions(&state,&scope),"links":crate::workspace_scope::links(&state,&scope)}).to_string();
        if frame != last {
            if socket
                .send(Message::Text(frame.clone().into()))
                .await
                .is_err()
            {
                return;
            }
            last = frame;
        }
        if send_settings_snapshot(&mut socket, &state, &mut settings)
            .await
            .is_err()
        {
            return;
        }
        for (kind, epochs) in [
            ("git", state.git.epochs_snapshot()),
            ("timeline", state.timeline.epochs_snapshot()),
        ] {
            let own: std::collections::BTreeMap<_, _> = epochs
                .into_iter()
                .filter(|(id, _)| id == &scope.workspace_id)
                .collect();
            let value = json!({"type":kind,"epochs":own});
            if epochs_sent.get(kind) != Some(&value) {
                if send_json(&mut socket, &value).await.is_err() {
                    return;
                }
                epochs_sent.insert(kind, value);
            }
        }
        if send_recents_snapshot(&mut socket, &state, &mut recents)
            .await
            .is_err()
        {
            return;
        }
        if let Some(frame) = crate::notices::frame_since(&state, &mut last_notice) {
            if let Ok(mut frame) = serde_json::from_str::<serde_json::Value>(&frame) {
                if let Some(rows) = frame["notices"].as_array_mut() {
                    rows.retain(|row| row["workspace_id"].as_str() == Some(&scope.workspace_id));
                    if !rows.is_empty() && send_json(&mut socket, &frame).await.is_err() {
                        return;
                    }
                }
            }
        }
        let mut changes = files.poll(false).await;
        if let Some(alias) = &alias {
            for paths in [
                &mut changes.files,
                &mut changes.removed,
                &mut changes.dirs,
                &mut changes.removed_dirs,
            ] {
                for path in paths {
                    *path = alias.output(path);
                }
            }
        }
        if send_fs_changes(&mut socket, changes).await.is_err() {
            return;
        }
        tokio::select! {
            _ = tick.tick() => {},
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(ClientMessage::Watch {workspace_id, files: mut wanted_files, mut dirs, git_repos: _}) = serde_json::from_str(&text) {
                        if workspace_id.as_deref().is_some_and(|id| id != scope.workspace_id) { return; }
                        if let Some(alias)=&alias { for path in wanted_files.iter_mut().chain(dirs.iter_mut()) { *path=alias.input(path); } }
                        // A path this viewer may not read is dropped, never a
                        // reason to close: its window watches that one itself.
                        let (wanted_files, dirs) = readable_watch(&state, &scope, wanted_files, dirs).await;
                        if scope.validate(&state).is_err() { scope_changed(&mut socket).await; return; }
                        files.set(wanted_files,dirs);
                        tokio::time::sleep(EVENTS_THROTTLE).await;
                    }
                },
                Some(Ok(Message::Ping(payload))) => { if socket.send(Message::Pong(payload)).await.is_err() { return; } },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {},
            },
        }
    }
}

/// The part of a scoped window's watch registration its viewer may read.
async fn readable_watch(
    state: &AppState,
    scope: &SocketScope,
    files: Vec<String>,
    dirs: Vec<String>,
) -> (Vec<String>, Vec<String>) {
    let split = files.len();
    let paths: Vec<String> = files.into_iter().chain(dirs).collect();
    let readable = scope
        .readable(state, paths.clone())
        .await
        .unwrap_or_default();
    let mut kept = (Vec::new(), Vec::new());
    for (index, path) in paths.into_iter().enumerate() {
        if readable.get(index) == Some(&true) {
            if index < split {
                kept.0.push(path);
            } else {
                kept.1.push(path);
            }
        }
    }
    kept
}

/// Minimum gap between snapshot frames (<= 4/s). Also the reuse window of
/// the shared sessions-snapshot cache (`session_view::EVENTS_SNAPSHOT_REUSE`
/// is defined AS this constant so the two can't drift apart).
pub(crate) const EVENTS_THROTTLE: Duration = Duration::from_millis(250);
/// Fallback poll: catches changes that never signal `changes` (e.g. a PTY
/// child exiting on its own).
const EVENTS_TICK: Duration = Duration::from_secs(1);

/// First retry after a project feed ends, doubling up to [`FEED_RETRY_MAX`].
const FEED_RETRY_MIN: Duration = Duration::from_secs(1);
const FEED_RETRY_MAX: Duration = Duration::from_secs(30);

/// How the local watchers must change for the watched project.
enum LocalWatch {
    /// The project runs elsewhere: its owner's feed replaces local watching
    /// of the project's own paths; the window's other paths stay watched here.
    Park(Vec<String>, Vec<String>),
    /// The project is local again: resume watching it here.
    Restore(String, Vec<String>, Vec<String>),
}

/// A window watching a project that currently runs on another machine gets
/// that project's session, file, Git and Timeline frames from its owner,
/// merged into this window's own loop; everything else (settings, recents,
/// notices, updates, plugins) stays this daemon's. When the owner changes or
/// sleeps the feed ends and restarts; the window's socket never closes for
/// it. A project with no route (every free user's) stays in local mode.
struct ProjectView {
    /// The window's watched project and its mounted/listed paths.
    watched: Option<(String, Vec<String>, Vec<String>)>,
    feed: Option<crate::session_proxy::Feed>,
    /// Local git/fs watching is parked because the project is routed.
    remote: bool,
    retry_at: Option<tokio::time::Instant>,
    backoff: Duration,
    git: Option<(String, u64)>,
    timeline: Option<(String, u64)>,
}
impl ProjectView {
    fn new() -> Self {
        Self {
            watched: None,
            feed: None,
            remote: false,
            retry_at: None,
            backoff: FEED_RETRY_MIN,
            git: None,
            timeline: None,
        }
    }
    fn stop_feed(&mut self) {
        self.feed = None;
        self.git = None;
        self.timeline = None;
        self.retry_at = None;
        self.backoff = FEED_RETRY_MIN;
    }
    /// A new watch registration from the window.
    fn watch(&mut self, workspace: Option<&str>, files: &[String], dirs: &[String]) {
        match (&self.feed, workspace) {
            (Some(feed), Some(workspace)) if feed.workspace == workspace => {
                feed.watch(files.to_vec(), dirs.to_vec());
            }
            (Some(_), _) => self.stop_feed(),
            (None, _) => {}
        }
        self.watched = workspace.map(|w| (w.to_owned(), files.to_vec(), dirs.to_vec()));
    }
    /// Re-decide local vs routed for the watched project; start (or retry) its
    /// feed while routed.
    fn reconcile(&mut self, state: &Arc<AppState>) -> Option<LocalWatch> {
        let Some((workspace, files, dirs)) = &self.watched else {
            self.stop_feed();
            self.remote = false;
            return None;
        };
        if !state.session_proxy.routed(workspace) {
            if !self.remote {
                return None;
            }
            let restore = LocalWatch::Restore(workspace.clone(), files.clone(), dirs.clone());
            self.remote = false;
            self.stop_feed();
            return Some(restore);
        }
        let parked = !std::mem::replace(&mut self.remote, true);
        let park = parked.then(|| {
            LocalWatch::Park(
                state.session_proxy.outside_project(workspace, files),
                state.session_proxy.outside_project(workspace, dirs),
            )
        });
        if self.feed.is_none()
            && self
                .retry_at
                .is_none_or(|at| tokio::time::Instant::now() >= at)
        {
            self.retry_at = None;
            self.feed = Some(crate::session_proxy::Feed::start(
                Arc::clone(state),
                workspace.clone(),
                files.clone(),
                dirs.clone(),
            ));
        }
        park
    }
    async fn next(&mut self) -> Option<crate::session_proxy::FeedFrame> {
        match self.feed.as_mut() {
            Some(feed) => feed.next().await,
            None => std::future::pending().await,
        }
    }
    /// The feed ended (owner change, sleeping owner, transient failure).
    fn ended(&mut self) {
        self.feed = None;
        self.retry_at = Some(tokio::time::Instant::now() + self.backoff);
        self.backoff = (self.backoff * 2).min(FEED_RETRY_MAX);
    }
}

async fn handle_events(mut socket: WebSocket, state: Arc<AppState>) {
    let auth = match authenticate(&mut socket, &state, false).await {
        Ok(auth) => auth,
        Err(denied) => {
            denied.answer(&mut socket).await;
            return;
        }
    };
    if let Some(scope) = auth.scope {
        scoped_events(socket, state, scope).await;
        return;
    }

    // Released on every exit path below (a leaked watcher would poll git forever).
    let mut watch = crate::git::WatchGuard::new(state.clone());
    // Per-client, bounded mounted-path monitor. Dropping the socket drops every
    // registration, so a closed window costs zero filesystem work.
    let mut fs_watch = crate::fs_watch::FsWatch::new();
    // Writes the daemon heard about, re-stated at once for watched paths.
    // Subscribed before the first snapshot so no write between the two is
    // missed (the poll would still catch it, just later).
    let mut touched = state.fs_touched.subscribe();
    let mut touched_open = true;
    let mut project = ProjectView::new();

    let mut last_sent: Option<Arc<String>> = None;
    let mut last_settings_gen: Option<u64> = None;
    let mut last_git: Option<String> = None;
    let mut last_update_epoch: Option<u64> = None;
    let mut last_recents_epoch: Option<u64> = None;
    let mut last_agent_plugins_epoch: Option<u64> = None;
    let mut last_timeline: Option<String> = None;
    // Notices start at the head: a (re)connecting window is told about what
    // happens from now on, never handed old alerts as new.
    let mut last_notice = state.notices.head();
    // Plugin `emit` frames, same rule: from now on, never a replay.
    let mut last_plugin_event = state.plugin_runtime.events_head();
    // A new window's FIRST settings frame gets one fresh disk read (off the
    // reactor): a hand-edit inside the watcher's poll window must not greet
    // a fresh window with stale settings. Steady-state sends stay cached.
    {
        let state = state.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = crate::lock(&state.settings).current();
        })
        .await;
    }
    if send_settings_snapshot(&mut socket, &state, &mut last_settings_gen)
        .await
        .is_err()
    {
        return;
    }
    // A window connecting during ledger resurrection must not receive a
    // half-restored roster as its first snapshot — it would prune the
    // still-respawning sessions' tabs out of its restored layout.
    state.wait_restored().await;
    if send_sessions_snapshot(&mut socket, &state, &mut last_sent)
        .await
        .is_err()
    {
        return;
    }
    if send_git_snapshot(&mut socket, &state, &mut last_git, None)
        .await
        .is_err()
    {
        return;
    }
    if send_update_snapshot(&mut socket, &state, &mut last_update_epoch)
        .await
        .is_err()
    {
        return;
    }
    if send_recents_snapshot(&mut socket, &state, &mut last_recents_epoch)
        .await
        .is_err()
    {
        return;
    }
    if send_timeline_snapshot(&mut socket, &state, &mut last_timeline, None)
        .await
        .is_err()
    {
        return;
    }

    if send_agent_plugins_snapshot(&mut socket, &state, &mut last_agent_plugins_epoch)
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            _ = state.changes.notified() => {}
            _ = tokio::time::sleep(EVENTS_TICK) => {}
            first = touched.recv(), if touched_open => {
                let mut paths: Vec<std::path::PathBuf> = Vec::new();
                match first {
                    Ok(batch) => paths.extend(batch.iter().cloned()),
                    // Skipped writes are the poll's to find; no fast path now.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        touched_open = false;
                    }
                }
                // Everything queued since coalesces into this one stat pass
                // (bounded by the channel's capacity).
                loop {
                    match touched.try_recv() {
                        Ok(batch) => paths.extend(batch.iter().cloned()),
                        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                        Err(_) => break,
                    }
                }
                let changes = fs_watch.poll_touched(&paths).await;
                if send_fs_changes(&mut socket, changes).await.is_err() {
                    return;
                }
                // Fall through to the regular sends: a steady stream of
                // writes must neither starve them (the tick restarts every
                // iteration) nor outrun the throttle below, which bounds
                // these passes to four a second per window.
            }
            // The watched project's frames from where it runs now.
            frame = project.next() => match frame {
                Some(crate::session_proxy::FeedFrame::Fs(value)) => {
                    project.backoff = FEED_RETRY_MIN;
                    if send_json(&mut socket, &value).await.is_err() {
                        return;
                    }
                }
                Some(crate::session_proxy::FeedFrame::Git(epoch)) => {
                    project.backoff = FEED_RETRY_MIN;
                    project.git = project.watched.as_ref().map(|(w, ..)| (w.clone(), epoch));
                }
                Some(crate::session_proxy::FeedFrame::Timeline(epoch)) => {
                    project.backoff = FEED_RETRY_MIN;
                    project.timeline = project.watched.as_ref().map(|(w, ..)| (w.clone(), epoch));
                }
                None => project.ended(),
            },
            msg = socket.recv() => match msg {
                // The only client frame on this bus: which workspace this window
                // shows + the exact mounted paths whose disk state it renders.
                Some(Ok(Message::Text(text))) => {
                    if let Ok(ClientMessage::Watch { workspace_id, files, dirs, git_repos }) =
                        serde_json::from_str::<ClientMessage>(&text)
                    {
                        project.watch(workspace_id.as_deref(), &files, &dirs);
                        let routed = workspace_id
                            .as_deref()
                            .is_some_and(|w| state.session_proxy.routed(w));
                        if routed {
                            // The owner's feed replaces local watching of a
                            // project that runs elsewhere; this computer's own
                            // files outside it (an upload, a note in the home
                            // folder) are still watched here.
                            let _ = project.reconcile(&state);
                            watch.set(None);
                            let workspace = workspace_id.as_deref().unwrap_or_default();
                            let outside_files = state.session_proxy.outside_project(workspace, &files);
                            let outside_dirs = state.session_proxy.outside_project(workspace, &dirs);
                            if fs_watch.set(outside_files, outside_dirs) {
                                let changes = fs_watch.poll(false).await;
                                if send_fs_changes(&mut socket, changes).await.is_err() {
                                    return;
                                }
                            }
                        } else {
                            project.stop_feed();
                            project.remote = false;
                            watch.set(workspace_id);
                            watch.set_repos(git_repos);
                            if fs_watch.set(files, dirs) {
                                // Establish new metadata baselines immediately when
                                // the two-second client-I/O ceiling allows it. New
                                // directory name baselines are separately batched.
                                let changes = fs_watch.poll(false).await;
                                if send_fs_changes(&mut socket, changes).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    continue;
                }
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => return,
            },
        }
        // A registration or an owner change may have moved the watched project
        // between this daemon and another machine.
        match project.reconcile(&state) {
            Some(LocalWatch::Park(files, dirs)) => {
                watch.set(None);
                fs_watch.set(files, dirs);
            }
            Some(LocalWatch::Restore(workspace, files, dirs)) => {
                watch.set(Some(workspace));
                fs_watch.set(files, dirs);
            }
            None => {}
        }
        if send_settings_snapshot(&mut socket, &state, &mut last_settings_gen)
            .await
            .is_err()
        {
            return;
        }
        if send_sessions_snapshot(&mut socket, &state, &mut last_sent)
            .await
            .is_err()
        {
            return;
        }
        if send_git_snapshot(&mut socket, &state, &mut last_git, project.git.as_ref())
            .await
            .is_err()
        {
            return;
        }
        if send_update_snapshot(&mut socket, &state, &mut last_update_epoch)
            .await
            .is_err()
        {
            return;
        }
        if send_recents_snapshot(&mut socket, &state, &mut last_recents_epoch)
            .await
            .is_err()
        {
            return;
        }
        if send_timeline_snapshot(
            &mut socket,
            &state,
            &mut last_timeline,
            project.timeline.as_ref(),
        )
        .await
        .is_err()
        {
            return;
        }
        if send_agent_plugins_snapshot(&mut socket, &state, &mut last_agent_plugins_epoch)
            .await
            .is_err()
        {
            return;
        }
        if let Some(frame) = crate::notices::frame_since(&state, &mut last_notice) {
            if socket.send(Message::Text(frame.into())).await.is_err() {
                return;
            }
        }
        // `{"type":"plugin", ...}` — additive; a client ignores types it
        // doesn't know.
        for frame in state
            .plugin_runtime
            .events_since(&mut last_plugin_event, watch.workspace())
        {
            if socket
                .send(Message::Text(frame.as_ref().into()))
                .await
                .is_err()
            {
                return;
            }
        }
        let fs_changes = fs_watch.poll(false).await;
        if send_fs_changes(&mut socket, fs_changes).await.is_err() {
            return;
        }
        tokio::time::sleep(EVENTS_THROTTLE).await;
    }
}

/// Installs and hook trust affect every workspace on this host. Push only
/// an invalidation; visible Extensions views pull the agents' own reports.
async fn send_agent_plugins_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    last_epoch: &mut Option<u64>,
) -> Result<(), axum::Error> {
    let epoch = state.probes.changed_epoch();
    if *last_epoch == Some(epoch) {
        return Ok(());
    }
    send_json(socket, &json!({"type": "agent_plugins", "epoch": epoch})).await?;
    *last_epoch = Some(epoch);
    Ok(())
}

/// Send a path-only filesystem invalidation. File contents/listings remain
/// pull-based; this tiny frame says exactly which mounted payloads are stale.
async fn send_fs_changes(
    socket: &mut WebSocket,
    changes: crate::fs_watch::FsChanges,
) -> Result<(), axum::Error> {
    if changes.is_empty() {
        return Ok(());
    }
    let frame = json!({
        "type": "fs",
        "files": changes.files,
        "removed": changes.removed,
        "dirs": changes.dirs,
        "removed_dirs": changes.removed_dirs,
    })
    .to_string();
    socket.send(Message::Text(frame.into())).await
}

/// Send a `{"type":"update", ...}` frame when the daemon's release knowledge
/// changed (see `update`). The payload is the same shape GET /api/v1/update
/// returns, so the client has one parser.
async fn send_update_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    last_epoch: &mut Option<u64>,
) -> Result<(), axum::Error> {
    let epoch = state
        .update_epoch
        .load(std::sync::atomic::Ordering::Relaxed);
    if *last_epoch == Some(epoch) {
        return Ok(());
    }
    let mut frame = crate::update::status_json(state);
    frame["type"] = serde_json::json!("update");
    socket.send(Message::Text(frame.to_string().into())).await?;
    *last_epoch = Some(epoch);
    Ok(())
}

/// Send a `{"type":"recents","epoch":N}` invalidate frame when any workspace's
/// recents changed. Like the git frame, the payload never rides the bus —
/// the client refetches GET /recents for its own workspace.
async fn send_recents_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    last_epoch: &mut Option<u64>,
) -> Result<(), axum::Error> {
    let epoch = state
        .recents_epoch
        .load(std::sync::atomic::Ordering::Relaxed);
    if *last_epoch == Some(epoch) {
        return Ok(());
    }
    let frame = json!({"type": "recents", "epoch": epoch}).to_string();
    socket.send(Message::Text(frame.into())).await?;
    *last_epoch = Some(epoch);
    Ok(())
}

/// Send a `{"type":"timeline","epochs":{workspace_id:epoch}}` invalidate
/// frame when any workspace's Timeline grew — the git frame's shape and
/// dedupe: entries never ride the bus; the client pulls its own workspace's
/// page (`GET /workspaces/{id}/timeline?since=`).
async fn send_timeline_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    last: &mut Option<String>,
    remote: Option<&(String, u64)>,
) -> Result<(), axum::Error> {
    let mut epochs: std::collections::BTreeMap<String, u64> =
        state.timeline.epochs_snapshot().into_iter().collect();
    // A project running elsewhere reports its owner's Timeline epoch.
    if let Some((workspace, epoch)) = remote {
        epochs.insert(workspace.clone(), *epoch);
    }
    let frame = json!({"type": "timeline", "epochs": epochs}).to_string();
    if last.as_deref() == Some(frame.as_str()) {
        return Ok(());
    }
    socket.send(Message::Text(frame.clone().into())).await?;
    *last = Some(frame);
    Ok(())
}

/// Send a `{"type":"settings","settings":{...}}` frame when the settings
/// content generation moved (PUT, or a hand-edit surfaced by the settings
/// watcher task). Deliberately reads the CACHED generation/map — no re-stat:
/// this runs per client on every events wake, and a blocking stat here under
/// the settings mutex would stall the reactor on an NFS hiccup. External
/// edits reach this path via `settings::watch_external_edits` (off-reactor
/// stat + reload + notify).
async fn send_settings_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    last_gen: &mut Option<u64>,
) -> Result<(), axum::Error> {
    let (generation, map) = {
        let store = crate::lock(&state.settings);
        let generation = store.generation_cached();
        if *last_gen == Some(generation) {
            return Ok(());
        }
        (generation, store.map_cached().clone())
    };
    let frame = json!({"type": "settings", "settings": map}).to_string();
    socket.send(Message::Text(frame.into())).await?;
    *last_gen = Some(generation);
    Ok(())
}

/// Send a `{"type":"git","epochs":{workspace_id:epoch},"repos":{workspace_id:
/// {toplevel:epoch}}}` invalidate frame when any git epoch moved. The status
/// payload never rides this bus — the client refetches `GET /git/status` for
/// its active workspace, and for each repository whose own epoch moved
/// (invalidate-and-pull keeps big path lists off the daemon-wide firehose).
/// `repos` is additive; the maps are ordered (BTreeMap) so an unchanged
/// snapshot compares equal.
async fn send_git_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    last: &mut Option<String>,
    remote: Option<&(String, u64)>,
) -> Result<(), axum::Error> {
    let mut epochs: std::collections::BTreeMap<String, u64> =
        state.git.epochs_snapshot().into_iter().collect();
    // A project running elsewhere reports its owner's Git epoch.
    if let Some((workspace, epoch)) = remote {
        epochs.insert(workspace.clone(), *epoch);
    }
    let repos: std::collections::BTreeMap<String, std::collections::BTreeMap<String, u64>> = state
        .git
        .repo_epochs_snapshot()
        .into_iter()
        .map(|(ws, m)| {
            (
                ws,
                m.into_iter()
                    .map(|(top, e)| (top.to_string_lossy().into_owned(), e))
                    .collect(),
            )
        })
        .collect();
    let frame = json!({"type": "git", "epochs": epochs, "repos": repos}).to_string();
    if last.as_deref() == Some(frame.as_str()) {
        return Ok(());
    }
    socket.send(Message::Text(frame.clone().into())).await?;
    *last = Some(frame);
    Ok(())
}

/// Send the current session snapshot if it differs from the last one sent.
/// The snapshot itself is built once per change generation and shared across
/// every connected client (`session_view::shared_sessions_snapshot`); only
/// the last-sent compare — and the send — stay per-client. The `Arc` identity
/// check makes the common no-change case free: an unchanged cache hands every
/// client the same allocation.
async fn send_sessions_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    last_sent: &mut Option<Arc<String>>,
) -> Result<(), axum::Error> {
    let snapshot = crate::session_view::shared_sessions_snapshot(state).await;
    if last_sent
        .as_ref()
        .is_some_and(|prev| Arc::ptr_eq(prev, &snapshot) || **prev == *snapshot)
    {
        return Ok(());
    }
    socket.send(Message::Text(snapshot.as_str().into())).await?;
    *last_sent = Some(snapshot);
    Ok(())
}

/// First-frame auth: text `{"type":"auth","token":...}` within 5 seconds.
/// `None` = rejected; `Some(dims)` = accepted, with the client grid when the
/// auth frame carried one.
/// The accepted auth frame's parameters: the client grid (if sent) and
/// whether the client attached parked.
struct AuthParams {
    dims: Option<(u16, u16)>,
    parked: bool,
    scope: Option<SocketScope>,
}

/// Why a first frame was refused. A wrong or missing token is final for the
/// client; a project scope the daemon cannot admit right now (an owner that
/// just woke and has not renewed yet, a changed epoch) is retryable, and the
/// UI already reconnects on `workspace_scope_changed`. Answering it
/// `unauthorized` made the first sockets after a cloud wake fail for good.
enum Denied {
    Token,
    Scope,
}

impl Denied {
    async fn answer(self, socket: &mut WebSocket) {
        match self {
            Denied::Token => {
                let _ =
                    send_json(socket, &json!({"type": "error", "message": "unauthorized"})).await;
            }
            Denied::Scope => scope_changed(socket).await,
        }
    }
}

/// `waking`: the socket's viewer understands the `waking` frame (terminals;
/// the events feed does not), sent while a thawed owner renews (see
/// `SocketScope::admit`).
async fn authenticate(
    socket: &mut WebSocket,
    state: &AppState,
    waking: bool,
) -> Result<AuthParams, Denied> {
    let Ok(Some(Ok(Message::Text(text)))) = tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await
    else {
        return Err(Denied::Token);
    };
    match serde_json::from_str::<ClientMessage>(&text) {
        Ok(ClientMessage::Auth {
            token,
            cols,
            rows,
            parked,
            scope,
        }) if token == state.token => Ok(AuthParams {
            dims: cols.zip(rows),
            parked,
            scope: SocketScope::admit(state, scope, waking.then_some(socket))
                .await
                .map_err(|_| Denied::Scope)?,
        }),
        _ => Err(Denied::Token),
    }
}

async fn send_json(socket: &mut WebSocket, value: &serde_json::Value) -> Result<(), axum::Error> {
    socket.send(Message::Text(value.to_string().into())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chimaera_agent::journal::SeqEvent;
    use chimaera_agent::model::{
        AgentCommand, AgentEvent, ContentBlock, COMMAND_IMAGES_MAX, COMMAND_IMAGE_BASE64_MAX,
        COMMAND_PATH_MAX, COMMAND_TEXT_TOTAL_MAX,
    };

    fn replay_entry(seq: u64, text: &str) -> Arc<SeqEvent> {
        Arc::new(SeqEvent {
            seq,
            ts: 0,
            ev: AgentEvent::MessageChunk {
                turn_id: "t".to_string(),
                text: text.to_string(),
            },
        })
    }

    #[test]
    fn a_refusal_names_the_command_it_answers() {
        assert_eq!(
            command_kind(r#"{"type":"send","blocks":[]}"#).as_deref(),
            Some("send")
        );
        assert_eq!(
            command_kind(r#"{"type":"interrupt"}"#).as_deref(),
            Some("interrupt")
        );
        for text in ["", "{}", r#"{"type":"Send"}"#, r#"{"type":7}"#, "not json"] {
            assert_eq!(command_kind(text), None, "{text}");
        }
        let refused = command_refusal(
            json!({"type":"error","code":"command_failed"}),
            r#"{"type":"permission","request_id":"r","option_id":"o"}"#,
        );
        assert_eq!(refused["command"], "permission");
    }

    fn paused(reason: &'static str) -> Option<Pause> {
        Some(Pause::Paused {
            reason,
            provider: None,
        })
    }

    #[test]
    fn work_that_left_for_another_computer_says_so() {
        // Acting on another of the user's computers brought the work there:
        // this computer's own views say it continues on the other computer
        // (older clients read `to:"computer"`), never "in the cloud".
        let left = |transfer, writable| PauseFacts {
            other: true,
            transfer,
            entry: Some(EntryFacts {
                writable,
                provider: Some("claude".into()),
                ..EntryFacts::default()
            }),
            ..PauseFacts::default()
        };
        for (transfer, writable) in [(true, true), (false, false)] {
            let pause = classify_pause(&left(transfer, writable), || true);
            assert_eq!(pause, Some(Pause::Moved("other")));
            assert_eq!(
                pause.unwrap().frame(),
                json!({"type":"moved","to":"computer","other":true})
            );
        }
        // A cloud machine's other owner is always the user's computer.
        let worker = PauseFacts {
            worker: true,
            ..left(true, true)
        };
        assert_eq!(
            classify_pause(&worker, || true),
            Some(Pause::Moved("computer"))
        );
    }

    #[test]
    fn only_a_real_transfer_says_moved_and_it_names_where_the_session_goes() {
        let stopped = |worker, writable| PauseFacts {
            worker,
            entry: Some(EntryFacts {
                writable,
                provider: Some("claude".into()),
                ..EntryFacts::default()
            }),
            ..PauseFacts::default()
        };
        // Exporting for a transfer: away from this machine.
        let exporting = |worker| PauseFacts {
            transfer: true,
            ..stopped(worker, true)
        };
        assert_eq!(
            classify_pause(&exporting(false), || true),
            Some(Pause::Moved("cloud"))
        );
        assert_eq!(
            classify_pause(&exporting(true), || true),
            Some(Pause::Moved("computer"))
        );
        // Stopped because another machine now runs the project.
        assert_eq!(
            classify_pause(&stopped(false, false), || true),
            Some(Pause::Moved("cloud"))
        );
        assert_eq!(
            classify_pause(&stopped(true, false), || true),
            Some(Pause::Moved("computer"))
        );
        // Waiting out a restart on the machine that owns the project: after
        // every Pro update, a computer's own chats are not "in the cloud".
        assert_eq!(
            classify_pause(&stopped(false, true), || false),
            paused("restarting")
        );
        // A cloud machine that restarted refuses writes until verified, yet
        // its sessions did not go anywhere.
        assert_eq!(
            classify_pause(&stopped(true, false), || false),
            paused("restarting")
        );
        // No entry and no transfer: an ordinary session, nothing to say.
        assert_eq!(classify_pause(&PauseFacts::default(), || true), None);
        // A transfer opening a session before its entry exists.
        let opening = PauseFacts {
            transfer: true,
            ..PauseFacts::default()
        };
        assert_eq!(classify_pause(&opening, || false), paused("importing"));
        let snapshot_of_a_live_session = PauseFacts {
            known: true,
            ..opening
        };
        assert_eq!(classify_pause(&snapshot_of_a_live_session, || true), None);
    }

    #[test]
    fn work_arriving_is_opening_or_waiting_never_moving_away() {
        let arrived = |worker, blocked, moved_shell| PauseFacts {
            worker,
            transfer: true,
            entry: Some(EntryFacts {
                arrived: true,
                blocked,
                moved_shell,
                provider: (!moved_shell).then(|| "codex".to_owned()),
                ..EntryFacts::default()
            }),
            ..PauseFacts::default()
        };
        assert_eq!(
            classify_pause(&arrived(true, false, false), || true),
            paused("importing")
        );
        assert_eq!(
            classify_pause(&arrived(true, true, false), || true),
            Some(Pause::Paused {
                reason: "needs_provider",
                provider: Some("codex".into())
            })
        );
        assert_eq!(
            classify_pause(&arrived(true, false, true), || true),
            paused("stays_on_computer")
        );
        // A computer receiving its work back.
        assert_eq!(
            classify_pause(&arrived(false, false, false), || true),
            Some(Pause::Moved("computer"))
        );
        let frame = Pause::Paused {
            reason: "needs_provider",
            provider: Some("claude".into()),
        }
        .frame();
        assert_eq!(
            frame,
            json!({"type":"paused","reason":"needs_provider","provider":"claude"})
        );
        assert_eq!(
            Pause::Moved("cloud").frame(),
            json!({"type":"moved","to":"cloud"})
        );
    }

    #[test]
    fn chat_replay_batch_is_bounded_by_count() {
        let replay: Vec<_> = (1..=CHAT_BATCH as u64 + 1)
            .map(|seq| replay_entry(seq, "x"))
            .collect();
        assert_eq!(chat_batch_end(&replay, 0), CHAT_BATCH);
        assert_eq!(chat_batch_end(&replay, CHAT_BATCH), CHAT_BATCH + 1);
    }

    #[test]
    fn chat_replay_batch_is_bounded_by_bytes_but_always_progresses() {
        let large = "x".repeat(CHAT_BATCH_BYTES / 2 + 1024);
        let replay = vec![replay_entry(1, &large), replay_entry(2, &large)];
        assert_eq!(chat_batch_end(&replay, 0), 1);
        assert_eq!(chat_batch_end(&replay, 1), 2);
    }

    #[test]
    fn output_batch_single_chunk_is_zero_copy() {
        let chunk = Bytes::from_static(b"echo hello");
        let mut batch = OutputBatch::new();
        batch.push(chunk.clone());
        let frame = batch.take_frame().expect("frame");
        // The refcounted buffer itself must ride through, not a copy: every
        // attached client shares the broadcast chunk's allocation.
        assert_eq!(frame.as_ptr(), chunk.as_ptr());
        assert!(batch.take_frame().is_none());
    }

    #[test]
    fn output_batch_concatenates_in_order() {
        let mut batch = OutputBatch::new();
        batch.push(Bytes::from_static(b"ab"));
        batch.push(Bytes::from_static(b"cd"));
        batch.push(Bytes::from_static(b"ef"));
        assert_eq!(batch.take_frame().expect("frame").as_ref(), b"abcdef");
        assert!(batch.take_frame().is_none());
        assert_eq!(batch.bytes, 0);
    }

    #[test]
    fn output_batch_full_at_byte_ceiling() {
        let mut batch = OutputBatch::new();
        batch.push(Bytes::from(vec![0u8; OUTPUT_COALESCE_MAX_BYTES - 1]));
        assert!(!batch.is_full());
        batch.push(Bytes::from_static(b"x"));
        assert!(batch.is_full());
        assert_eq!(
            batch.take_frame().expect("frame").len(),
            OUTPUT_COALESCE_MAX_BYTES
        );
        assert!(!batch.is_full());
    }

    #[test]
    fn output_batch_clear_discards_pending() {
        let mut batch = OutputBatch::new();
        batch.push(Bytes::from_static(b"stale"));
        batch.clear();
        assert!(batch.take_frame().is_none());
        assert_eq!(batch.bytes, 0);
    }

    #[test]
    fn maximum_valid_browser_command_fits_transport_envelope() {
        let mut blocks = vec![ContentBlock::Text {
            // NUL takes the widest common JSON escape (`\\u0000`), proving
            // the transport envelope covers payload caps, not just ASCII.
            text: "\0".repeat(COMMAND_TEXT_TOTAL_MAX),
        }];
        blocks.extend((0..COMMAND_IMAGES_MAX).map(|_| ContentBlock::Image {
            media_type: "image/png".to_string(),
            data: "x".repeat(COMMAND_IMAGE_BASE64_MAX),
            path: Some("p".repeat(COMMAND_PATH_MAX)),
        }));
        let encoded = serde_json::to_vec(&AgentCommand::Send { blocks }).unwrap();
        assert!(encoded.len() <= MAX_CHAT_COMMAND_MESSAGE);
    }
}
