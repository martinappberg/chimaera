//! WebSocket bridges: /ws/sessions/{id} <-> chimaera_pty session, and the
//! /ws/events full-snapshot session bus.
//!
//! Browsers cannot set an Authorization header on a WebSocket, so the first
//! text frame must be `{"type":"auth","token":"..."}` (within 5 seconds).
//!
//! A session socket ends with its session: after `exited` the daemon lingers
//! [`EXITED_LINGER`] for the final PTY bytes, then sends a normal Close and
//! waits (bounded) for the client's reply, so a finished terminal never pins
//! a connection until the client gets round to closing it.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use crate::AppState;

pub(crate) mod frames;

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
/// How long a session socket stays open after its `exited` frame (or after
/// the session otherwise vanished). The Exited event races the reader
/// thread's final PTY bytes — session.rs publishes it, then gives the reader
/// ~60 ms before reaping — so the bridge keeps forwarding for a beat before
/// it closes.
const EXITED_LINGER: Duration = Duration::from_secs(1);
/// Bound on the closing handshake: after the daemon's Close frame, wait for
/// the client's Close reply before dropping the connection. Tearing TCP down
/// while client frames sit unread answers with a RST, and a RST can discard
/// whatever the client has not read yet — the final screen and `exited`.
const CLOSE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
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

/// GET /ws/sessions/{id}
pub(crate) async fn session_ws(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.max_message_size(MAX_TERMINAL_INPUT_MESSAGE)
        .max_frame_size(MAX_TERMINAL_INPUT_MESSAGE)
        .on_upgrade(move |socket| handle(socket, id, state))
}

async fn handle(mut socket: WebSocket, id: String, state: Arc<AppState>) {
    let auth = match authenticate(&mut socket, &state).await {
        Some(auth) => auth,
        None => {
            let _ = send_json(&mut socket, &frames::error("unauthorized")).await;
            return;
        }
    };
    let auth_dims = if auth.parked { None } else { auth.dims };

    // Adopt the client's grid BEFORE attaching so the snapshot below is
    // rendered at the size the client will actually display it. A parked
    // attach adopts nothing: a hidden window's stale dims must never reflow
    // the grid out from under a visible one.
    if let Some((cols, rows)) = auth_dims {
        resize_off_reactor(&state, &id, cols, rows, "pre-attach resize").await;
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
        Ok(attachment) => attachment,
        Err(err) => {
            // A session that died before this client could attach (fast
            // agent failures — a missing API key kills codex in ~400ms)
            // still gets an honest pane: replay the final screen once,
            // then close as exited. Blank panes teach nothing.
            if let Some(words) = state.sessions.last_words(&id) {
                let ready = frames::pty_ready(&words.info, &words.info.cwd);
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
                if send_json(&mut socket, &frames::exited(words.info.exit_status))
                    .await
                    .is_ok()
                {
                    close_gracefully(&mut socket).await;
                }
                return;
            }
            tracing::debug!(%id, %err, "ws attach failed");
            let _ = send_json(
                &mut socket,
                // Retryable: mid view-switch the id exists but its process
                // is being respawned; clients back off and re-attach.
                &frames::error_coded("unknown_session", &format!("unknown session {id}")),
            )
            .await;
            return;
        }
    };

    // Ready frame: {"type":"ready", ...SessionInfo fields..., "cwd_current"},
    // `cwd_current` being the polled cwd (shell naming watcher), falling
    // back to the spawn cwd — the same field as REST/events session JSON.
    let cwd_current = crate::lock(&state.current_cwds)
        .get(&id)
        .cloned()
        .unwrap_or_else(|| attachment.info.cwd.clone());
    let ready = frames::pty_ready(&attachment.info, &cwd_current);
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
    // The session is over (exited, its channels closed, or its input gone):
    // the connection closes at this deadline (see EXITED_LINGER). Whether an
    // `exited` frame already went out decides if the close sends one first —
    // never twice.
    let mut close_at: Option<tokio::time::Instant> = None;
    let mut exited_sent = false;
    let mut input_open = true;
    let close_sleep = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(close_sleep);
    // Once the close is armed, nothing repaints: `repaint` re-attaches BY ID,
    // and an id respawned inside the linger (a view switch) would swap this
    // closing connection onto the live successor — which the close would
    // then hang up on. A foreign resize, a lag or an unpark in the linger is
    // left to the client's own recovery (a resync replays the last words).
    loop {
        tokio::select! {
            _ = &mut close_sleep, if close_at.is_some() => {
                let flushed = if exited_sent {
                    // The batch holds bytes that arrived after `exited` (the
                    // reader's final drain); they follow it, as they happened.
                    send_batch(&mut socket, &mut batch).await
                } else {
                    // Ordered send: the batched final bytes precede `exited`.
                    // The session vanished without its Exited event reaching
                    // this connection (input died first, or the event was
                    // lost to a lagged subscription): gone, status unknown.
                    send_ordered_json(&mut socket, &mut batch, &frames::exited(None)).await
                };
                if flushed {
                    close_gracefully(&mut socket).await;
                }
                return;
            },
            _ = &mut resync_sleep, if resync_at.is_some() && close_at.is_none() => {
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
                    if matches!(drain_err, Some(TryRecvError::Lagged(_))) && close_at.is_none() {
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
                Err(RecvError::Lagged(skipped)) if close_at.is_some() => {
                    tracing::debug!(%id, skipped, "ws output lagged while closing");
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
                    match frames::pty_event(&event) {
                        Ok(value) => {
                            // Ordered send: batched output first, so an event
                            // never overtakes the bytes it postdates.
                            if !send_ordered_json(&mut socket, &mut batch, &value).await {
                                return;
                            }
                            if matches!(event, chimaera_pty::SessionEvent::Exited { .. }) {
                                exited_sent = true;
                                arm_close(&mut close_at, close_sleep.as_mut());
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
                Err(RecvError::Closed) => {
                    // The session object is gone (events close only when it
                    // drops), whether or not its Exited reached us.
                    events_open = false;
                    arm_close(&mut close_at, close_sleep.as_mut());
                }
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(bytes))) => {
                    for chunk in bytes.chunks(TERMINAL_INPUT_CHUNK) {
                        if !input_open {
                            break;
                        }
                        if attachment
                            .input
                            .send(Bytes::copy_from_slice(chunk))
                            .await
                            .is_err()
                        {
                            // The PTY writer is gone: the session is ending.
                            // Keep forwarding its last words and its Exited
                            // (with the real status) through the linger.
                            input_open = false;
                            arm_close(&mut close_at, close_sleep.as_mut());
                        }
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::Resize { cols, rows }) => {
                            client_dims = Some((cols, rows));
                            // Flush output rendered at the old width before
                            // the grid reflows under it — the initiator is
                            // excluded from resync, so an inverted flush
                            // here would never be repaired.
                            if !send_batch(&mut socket, &mut batch).await {
                                return;
                            }
                            resize_off_reactor(&state, &id, cols, rows, "ws resize").await;
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
                            if parked && close_at.is_none() {
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
async fn resize_off_reactor(state: &Arc<AppState>, id: &str, cols: u16, rows: u16, what: &str) {
    let state = Arc::clone(state);
    let task_id = id.to_string();
    match tokio::task::spawn_blocking(move || state.sessions.resize(&task_id, cols, rows)).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::debug!(%id, %err, "{what} failed"),
        Err(err) => tracing::warn!(%id, %err, "{what} task failed"),
    }
}

/// Arm the session socket's close deadline ([`EXITED_LINGER`] from now),
/// unless it is already armed: the first end-of-session signal wins.
fn arm_close(
    close_at: &mut Option<tokio::time::Instant>,
    close_sleep: std::pin::Pin<&mut tokio::time::Sleep>,
) {
    if close_at.is_none() {
        let at = tokio::time::Instant::now() + EXITED_LINGER;
        close_sleep.reset(at);
        *close_at = Some(at);
    }
}

/// End the connection with a normal Close frame, then wait — at most
/// [`CLOSE_HANDSHAKE_TIMEOUT`] — for the client's Close reply, discarding
/// anything it sent meanwhile, so nothing unread is left behind when the
/// connection drops (see the constant).
async fn close_gracefully(socket: &mut WebSocket) {
    let frame = CloseFrame {
        code: close_code::NORMAL,
        reason: "".into(),
    };
    if socket.send(Message::Close(Some(frame))).await.is_err() {
        return;
    }
    let _ = tokio::time::timeout(CLOSE_HANDSHAKE_TIMEOUT, async {
        while let Some(Ok(msg)) = socket.recv().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
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
    let frame = frames::pty_resync(fresh.info.cols, fresh.info.rows);
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
) -> Response {
    ws.max_message_size(MAX_CHAT_COMMAND_MESSAGE)
        .max_frame_size(MAX_CHAT_COMMAND_MESSAGE)
        .on_upgrade(move |socket| handle_chat(socket, id, state))
}

/// Chat replay batch size: bounds per-frame size without flooding the socket
/// with one frame per event on a cold attach.
const CHAT_BATCH: usize = 128;
/// Byte budget per replay frame. Count-only batching admitted 128 maximum-size
/// journal entries into one ~32 MiB JSON frame, creating a large allocation
/// and long main-thread parse pause on cold reconnect. One entry may approach
/// the journal's 256 KiB cap; otherwise frames stay near this ceiling.
const CHAT_BATCH_BYTES: usize = 512 * 1024;

/// A browser view owns its native Mod attachment; closing the view must not
/// stop the conversation, but must release the window's callbacks and focus.
struct NativeUiAttachment {
    state: Arc<AppState>,
    session: String,
    client: String,
}

#[derive(Deserialize)]
struct ChatFrameKind<'a> {
    #[serde(rename = "type", borrow)]
    kind: &'a str,
}

impl Drop for NativeUiAttachment {
    fn drop(&mut self) {
        let state = self.state.clone();
        let session = self.session.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            state.chat.detach_native_ui(&session, &client).await;
        });
    }
}

async fn handle_chat(mut socket: WebSocket, id: String, state: Arc<AppState>) {
    let Some(last_seq) = chat_authenticate(&mut socket, &state).await else {
        let _ = send_json(&mut socket, &frames::error("unauthorized")).await;
        return;
    };

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
                &frames::error_coded("unknown_session", &format!("unknown chat session {id}")),
            )
            .await;
            return;
        }
    };

    // `replay_from` tells every client the cursor the daemon actually honored
    // (it can differ from auth.last_seq after a server-side journal reset);
    // `head` is the journal's highest seq now, so a client whose own
    // last_seq exceeds it knows the journal was recreated and hard-resets
    // rather than silently dropping every replayed event.
    let ready = frames::chat_ready(
        &attachment.info,
        attachment.replay_from,
        attachment.head_seq,
    );
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
    let mut native_ui = attachment.native_ui;
    let mut native_ui_open = true;
    let client_id = chimaera_core::generate_token();
    let _native_attachment = NativeUiAttachment {
        state: state.clone(),
        session: id.clone(),
        client: client_id.clone(),
    };
    loop {
        tokio::select! {
            event = native_ui.recv(), if native_ui_open => {
                let frame = match event {
                    Ok(event) if event.for_client(&client_id) => frames::native_ui(&event),
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => frames::native_ui_reset(),
                    Err(RecvError::Closed) => { native_ui_open = false; continue },
                };
                if send_json(&mut socket, &frame).await.is_err() { return }
            },
            event = live.recv() => match event {
                Ok(entry) => {
                    // The replay tail can overlap the subscription start.
                    if entry.seq <= sent_seq {
                        continue;
                    }
                    let frame = frames::chat_event(&entry);
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
                    let switching = crate::lock(&state.chat_switching).get(&id).cloned();
                    let frame = match switching.as_deref() {
                        Some("term") => frames::chat_degraded(),
                        Some("closed") => frames::exited(None),
                        Some(_) => frames::error_coded("unknown_session", "session switching"),
                        None if state.sessions.get(&id).is_some() => frames::chat_degraded(),
                        None => frames::exited(state.chat.get(&id).and_then(|c| c.exit_status)),
                    };
                    let _ = send_json(&mut socket, &frame).await;
                    return;
                }
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => {
                    if serde_json::from_str::<ChatFrameKind<'_>>(&text).is_ok_and(|f| f.kind == "native_ui") {
                        // Reject before allocating the UI request tree. Normal
                        // sends may carry images and have a separate budget.
                        if text.len() > chimaera_agent::native_ui::UI_REQUEST_BYTES + 1024 { return }
                        if let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text) {
                            let request_id = frame["request_id"].as_str().unwrap_or_default();
                            let result = chimaera_agent::native_ui::NativeUiCommand::new(&client_id, request_id, frame["request"].clone())
                                .and_then(|command| state.chat.native_ui(&id, command));
                            if let Err(error) = result {
                                let frame = frames::native_ui(&json!({"kind":"response","client_id":client_id,
                                    "request_id":request_id.chars().take(64).collect::<String>(),"error":error.to_string()}));
                                if send_json(&mut socket, &frame).await.is_err() { return }
                            }
                            continue;
                        }
                    }
                    match serde_json::from_str::<chimaera_agent::model::AgentCommand>(&text) {
                        Ok(mut cmd) => {
                            // Before the images are saved: a blank block never
                            // reaches an agent, and a send left with nothing is
                            // refused like an oversized one.
                            cmd.strip_empty_blocks();
                            if let Err(err) = cmd.validate_ingress() {
                                tracing::debug!(%id, %err, "chat command refused at ingress");
                                // Reject only this command. The authenticated
                                // socket and agent remain healthy, so the UI
                                // can correct the payload and retry.
                                let _ = send_json(
                                    &mut socket,
                                    &frames::error_coded("invalid_command", &err.to_string()),
                                )
                                .await;
                                continue;
                            }
                            // A send's images get a saved copy the echoed
                            // message can show after replay.
                            let saved = crate::upload::save_send_images(&state, &id, &mut cmd).await;
                            // The user's own prompt: agent communication's
                            // unattended wakes start over in this workspace.
                            let prompt = matches!(
                                cmd,
                                chimaera_agent::model::AgentCommand::Send { .. }
                                    | chimaera_agent::model::AgentCommand::SendAfterTurn { .. }
                                    | chimaera_agent::model::AgentCommand::SendNow { .. }
                            );
                            let sent = state.chat.command(&id, cmd).await;
                            if sent.is_ok() && prompt {
                                crate::comms::user_stepped_in(&state, &id);
                            }
                            if let Err(err) = sent {
                                // The send never happened: neither do its copies.
                                crate::upload::discard_saved_images(saved);
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
                                    &frames::error_coded(code, &message),
                                )
                                .await;
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
        if send_json(socket, &frames::chat_batch(chunk)).await.is_err() {
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
async fn chat_authenticate(socket: &mut WebSocket, state: &AppState) -> Option<u64> {
    #[derive(Deserialize)]
    struct ChatAuth {
        #[serde(rename = "type")]
        kind: String,
        token: String,
        #[serde(default)]
        last_seq: u64,
    }
    match tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<ChatAuth>(&text) {
            Ok(auth) if auth.kind == "auth" && auth.token == state.token => Some(auth.last_seq),
            _ => None,
        },
        _ => None,
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

/// Minimum gap between snapshot frames (<= 4/s). Also the reuse window of
/// the shared sessions-snapshot cache (`session_view::EVENTS_SNAPSHOT_REUSE`
/// is defined AS this constant so the two can't drift apart).
pub(crate) const EVENTS_THROTTLE: Duration = Duration::from_millis(250);
/// Fallback poll: catches changes that never signal `changes` (e.g. a PTY
/// child exiting on its own).
const EVENTS_TICK: Duration = Duration::from_secs(1);

async fn handle_events(mut socket: WebSocket, state: Arc<AppState>) {
    if authenticate(&mut socket, &state).await.is_none() {
        let _ = send_json(&mut socket, &frames::error("unauthorized")).await;
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

    let mut last_sent: Option<Arc<String>> = None;
    let mut last_settings_gen: Option<u64> = None;
    let mut last_git: Option<String> = None;
    let mut last_update_epoch: Option<u64> = None;
    let mut last_recents_epoch: Option<u64> = None;
    let mut last_agent_plugins_epoch: Option<u64> = None;
    let mut last_timeline: Option<String> = None;
    let mut last_comms: Option<String> = None;
    // Notices start at the head: a (re)connecting window is told about what
    // happens from now on, never handed old alerts as new.
    let mut last_notice = state.notices.head();
    // Plugin `emit` frames, same rule: from now on, never a replay.
    let mut last_plugin_event = state.plugin_runtime.events_head();
    // Agent `open_browser` frames, same rule: a window that was not here when
    // the agent asked never opens the pane later.
    let mut last_browser_open = state.browser_opens.head();
    // Counted for `open_browser`'s honest "no window is connected" answer;
    // dropped on every exit path below.
    let _consumer = state.browser_opens.consumer();
    // A new window's FIRST settings frame gets one fresh disk read (off the
    // reactor, through the store's gate): a hand-edit inside the watcher's
    // poll window must not greet a fresh window with stale settings.
    // Steady-state sends stay cached. Bounded: behind a hung read the frame
    // (and the sessions snapshot after it) must still come, so past one poll
    // period it goes out from the cache and the edit catches up on a later
    // poll's wake.
    let _ = tokio::time::timeout(
        crate::settings::SETTINGS_POLL,
        crate::settings::refresh(&state),
    )
    .await;
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
    if send_git_snapshot(&mut socket, &state, &mut last_git)
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
    if send_timeline_snapshot(&mut socket, &state, &mut last_timeline)
        .await
        .is_err()
    {
        return;
    }
    if send_comms_snapshot(&mut socket, &state, &mut last_comms)
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
            msg = socket.recv() => match msg {
                // The only client frame on this bus: which workspace this window
                // shows + the exact mounted paths whose disk state it renders.
                Some(Ok(Message::Text(text))) => {
                    if let Ok(ClientMessage::Watch { workspace_id, files, dirs, git_repos }) =
                        serde_json::from_str::<ClientMessage>(&text)
                    {
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
                    continue;
                }
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => return,
            },
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
        if send_git_snapshot(&mut socket, &state, &mut last_git)
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
        if send_timeline_snapshot(&mut socket, &state, &mut last_timeline)
            .await
            .is_err()
        {
            return;
        }
        if send_comms_snapshot(&mut socket, &state, &mut last_comms)
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
        // `{"type":"browser_open", ...}` — additive, like the plugin frames
        // below: a client ignores types it doesn't know.
        for frame in state.browser_opens.since(&mut last_browser_open) {
            if socket
                .send(Message::Text(frame.as_ref().into()))
                .await
                .is_err()
            {
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
    send_json(socket, &frames::agent_plugins(epoch)).await?;
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
    let frame = frames::fs(&changes).to_string();
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
    let frame = frames::update(crate::lock(&state.update).to_json());
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
    let frame = frames::recents(epoch).to_string();
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
) -> Result<(), axum::Error> {
    let epochs: std::collections::BTreeMap<String, u64> =
        state.timeline.epochs_snapshot().into_iter().collect();
    let frame = frames::timeline(&epochs).to_string();
    if last.as_deref() == Some(frame.as_str()) {
        return Ok(());
    }
    socket.send(Message::Text(frame.clone().into())).await?;
    *last = Some(frame);
    Ok(())
}

/// Send a `{"type":"comms","epochs":{workspace_id:epoch}}` invalidate frame
/// when any workspace's agent-communication state (unread counts, wake
/// requests) may have changed — the timeline frame's shape and dedupe; the
/// client pulls its own workspace's `GET /workspaces/{id}/comms`.
async fn send_comms_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    last: &mut Option<String>,
) -> Result<(), axum::Error> {
    let epochs: std::collections::BTreeMap<String, u64> =
        state.comms.epochs_snapshot().into_iter().collect();
    let frame = frames::comms(&epochs).to_string();
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
    let frame = frames::settings(&map).to_string();
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
) -> Result<(), axum::Error> {
    let epochs: std::collections::BTreeMap<String, u64> =
        state.git.epochs_snapshot().into_iter().collect();
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
    let frame = frames::git(&epochs, &repos).to_string();
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
}

async fn authenticate(socket: &mut WebSocket, state: &AppState) -> Option<AuthParams> {
    match tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<ClientMessage>(&text) {
            Ok(ClientMessage::Auth {
                token,
                cols,
                rows,
                parked,
            }) if token == state.token => Some(AuthParams {
                dims: cols.zip(rows),
                parked,
            }),
            _ => None,
        },
        _ => None,
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

    /// The `/ws/sessions/{id}` text frames → `pty-socket.json` (the binary
    /// snapshot and output frames carry no JSON and are not pinned).
    #[test]
    fn pty_socket_fixture() {
        use chimaera_pty::{SessionEvent, ShellPhase};
        use chimaera_wire_fixtures::{case, pin};
        use frames::samples;

        let live = samples::session_info("s-abc123", true);
        let dead = samples::session_info("s-dead", false);
        pin(
            "pty-socket",
            "crates/chimaera-server/src/ws.rs",
            "/ws/sessions/{id} text frames (ws::frames): ready, resync, the SessionEvent frames, exited, and the errors. Each ready/resync is followed by the snapshot as one binary frame.",
            vec![
                case(
                    "ready",
                    "attach accepted: the SessionInfo fields plus cwd_current (the polled cwd)",
                    frames::pty_ready(&live, std::path::Path::new("/home/u/proj/src")),
                ),
                case(
                    "ready_last_words",
                    "a session that died before the attach: its final screen replays, then `exited`",
                    frames::pty_ready(&dead, &dead.cwd),
                ),
                case("resync", "repaint at this grid; the next binary frame is the snapshot", frames::pty_resync(100, 40)),
                case(
                    "title",
                    "the terminal title changed",
                    frames::pty_event(&SessionEvent::Title { title: "vim lib.rs".into() }).unwrap(),
                ),
                case(
                    "resized",
                    "another client resized the grid",
                    frames::pty_event(&SessionEvent::Resized { cols: 100, rows: 40 }).unwrap(),
                ),
                case(
                    "shell_phase",
                    "OSC 133 shell phase (the client reads phase from session rows; this frame is informational)",
                    frames::pty_event(&SessionEvent::Shell { phase: ShellPhase::Running }).unwrap(),
                ),
                case(
                    "exited_event",
                    "the child exited, as the session's own event",
                    frames::pty_event(&SessionEvent::Exited { status: Some(0) }).unwrap(),
                ),
                case("exited", "the daemon's own exited frame after a last-words replay", frames::exited(Some(1))),
                case("exited_unknown", "the session is gone with no status", frames::exited(None)),
                case("error_unauthorized", "bad first frame: the socket closes", frames::error("unauthorized")),
                case(
                    "error_unknown_session",
                    "no such session (retryable mid view-switch)",
                    frames::error_coded("unknown_session", "unknown session s-abc123"),
                ),
            ],
        );
    }

    /// The `/ws/chat/{id}` frames → `chat-socket.json`.
    #[test]
    fn chat_socket_fixture() {
        use chimaera_agent::native_ui::NativeUiEvent;
        use chimaera_wire_fixtures::{case, pin};
        use frames::samples;

        let replay = vec![replay_entry(1, "hel"), replay_entry(2, "lo")];
        pin(
            "chat-socket",
            "crates/chimaera-server/src/ws.rs",
            "/ws/chat/{id} frames (ws::frames): ready (ChatInfo), batch replay, live ev, the end frames, the Mod UI frames, and the errors; `ev` payloads are chat-events.json's.",
            vec![
                case(
                    "ready",
                    "attach accepted, with every additive ChatInfo field set",
                    frames::chat_ready(&samples::chat_info("s-chat1", true), 0, 118),
                ),
                case(
                    "ready_plain",
                    "attach accepted for an idle chat, replaying from a client cursor",
                    frames::chat_ready(&samples::chat_info("s-chat2", false), 40, 41),
                ),
                case("batch", "a replay slice, in seq order", frames::chat_batch(&replay)),
                case("batch_empty", "an empty replay slice (nothing since the cursor)", frames::chat_batch(&[])),
                case("ev", "one live journal entry", frames::chat_event(&replay[0])),
                case("degraded", "the session moved to a terminal under the same id", frames::chat_degraded()),
                case("exited", "the driver exited 0", frames::exited(Some(0))),
                case("exited_unknown", "the driver is gone with no status", frames::exited(None)),
                case(
                    "native_ui_response",
                    "a Mod UI request answered for this client",
                    frames::native_ui(&NativeUiEvent::Response {
                        client_id: "c1".into(),
                        request_id: "r1".into(),
                        result: Some(serde_json::json!({"ok": true})),
                        error: None,
                    }),
                ),
                case(
                    "native_ui_notification",
                    "a Mod UI notification",
                    frames::native_ui(&NativeUiEvent::Notification {
                        payload: serde_json::json!({"method": "ui/notify", "params": {"text": "hi"}}),
                    }),
                ),
                case(
                    "native_ui_refused",
                    "a Mod UI request the daemon refused before the Mod saw it",
                    frames::native_ui(&serde_json::json!({"kind":"response","client_id":"c1",
                        "request_id":"r2","error":"request too large"})),
                ),
                case("native_ui_reset", "the Mod UI stream lagged: drop request state", frames::native_ui_reset()),
                case("error_unauthorized", "bad first frame: the socket closes", frames::error("unauthorized")),
                case(
                    "error_unknown_session",
                    "no such chat (retryable mid view-switch)",
                    frames::error_coded("unknown_session", "unknown chat session s-chat1"),
                ),
                case(
                    "error_invalid_command",
                    "one command refused at ingress; the socket stays up",
                    frames::error_coded("invalid_command", "text block exceeds 262144 byte/item limit (got 300000)"),
                ),
                case(
                    "error_command_failed",
                    "the agent could not take the command; the socket stays up",
                    frames::error_coded("command_failed", "agent unavailable"),
                ),
            ],
        );
    }

    /// The `/ws/events` bus frames → `events-bus.json`. The plugin `job`
    /// frame (`frames::plugin_job` over a running `plugins::jobs::Job`) is not
    /// pinned: it needs a live job; its envelope is `plugin_emit`'s.
    #[test]
    fn events_bus_fixture() {
        use crate::agent_state::{AgentKind, AgentRecord, AgentState};
        use crate::notices::{Notice, NoticeKind};
        use chimaera_wire_fixtures::{case, pin};
        use frames::samples;
        use std::collections::BTreeMap;

        let mut claude = AgentRecord::new("k".into(), AgentKind::Claude);
        claude.state = AgentState::Running;
        let shell = samples::session_info("s-shell1", true);
        let rows = vec![
            crate::session_view::finish_row(
                crate::session_view::pty_row(
                    &shell,
                    Some("w1".into()),
                    None,
                    Some("zsh"),
                    None,
                    None,
                    false,
                ),
                None,
                None,
            ),
            crate::session_view::finish_row(
                crate::chat::chat_session_json(
                    &samples::chat_info("s-chat1", true),
                    Some("w1".into()),
                    Some(&claude),
                    false,
                ),
                None,
                None,
            ),
        ];
        let links = vec![crate::links::link_json("s-shell1", "s-chat1")];
        let epochs: BTreeMap<String, u64> = BTreeMap::from([("w1".to_string(), 3)]);
        let repos: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::from([(
            "w1".to_string(),
            BTreeMap::from([("/home/u/proj".to_string(), 3)]),
        )]);
        let notice = Notice::fixture(NoticeKind::Permission);
        let notice_created = notice.created();
        let mut update = crate::update::UpdateStatus::default().to_json();
        // Stamped per build; pinned to constants so the fixture is stable.
        update["current"] = serde_json::json!("0.70.0");
        update["build"] = serde_json::json!("abc1234");
        update["dev"] = serde_json::json!(false);
        let mut emitted = serde_json::Map::new();
        emitted.insert("event".into(), serde_json::json!("compiled"));
        emitted.insert("pages".into(), serde_json::json!(12));

        pin(
            "events-bus",
            "crates/chimaera-server/src/ws.rs",
            "/ws/events frames (ws::frames): the sessions roster + links, settings, the invalidation frames, fs changes, update status, notices, browser_open, the plugin platform frames, and the error.",
            vec![
                case("sessions", "the full roster (a shell row, a chat row; session-rows.json has every row shape) and one link", frames::sessions(rows, links)),
                case("sessions_empty", "no sessions, no links", frames::sessions(vec![], vec![])),
                case(
                    "settings",
                    "the settings map (settings.json ground truth, any JSON)",
                    frames::settings(&serde_json::json!({"theme": "dark", "agents": {"claude": {"path": "/usr/local/bin/claude"}}})),
                ),
                case("git", "git epochs per workspace and per repository", frames::git(&epochs, &repos)),
                case("timeline", "timeline epochs per workspace", frames::timeline(&epochs)),
                case("comms", "agent-communication epochs per workspace", frames::comms(&epochs)),
                case("recents", "recents changed", frames::recents(3)),
                case("agent_plugins", "agent plugin installs/trust changed", frames::agent_plugins(2)),
                case(
                    "fs",
                    "mounted paths that changed on disk",
                    frames::fs(&crate::fs_watch::FsChanges {
                        files: vec!["/home/u/proj/README.md".into()],
                        removed: vec!["/home/u/proj/old.txt".into()],
                        dirs: vec!["/home/u/proj/src".into()],
                        removed_dirs: vec![],
                    }),
                ),
                case("update", "release knowledge (same body as GET /api/v1/update), nothing checked yet", frames::update(update)),
                case(
                    "notices",
                    "new notices since the client's mark (age_ms measured on the daemon's clock)",
                    frames::notices(&[std::sync::Arc::new(notice)], notice_created),
                ),
                case(
                    "browser_open",
                    "an agent asked for a browser pane",
                    frames::browser_open("s-chat1", Some("w1"), &"127.0.0.1", &5173u16, &"/docs"),
                ),
                case("plugin_view", "a plugin screen should re-render", frames::plugin_view("latex", "w1", "preview")),
                case("plugin_surface", "a plugin published a data surface key", frames::plugin_surface(&"latex", &"w1", &"figures", &"fig-1")),
                case("plugin_emit", "a plugin's own event, host keys last", frames::plugin_emit(emitted, "latex", "w1")),
                case("error_unauthorized", "bad first frame: the socket closes", frames::error("unauthorized")),
            ],
        );
    }
}
