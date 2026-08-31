//! WebSocket bridges: /ws/sessions/{id} <-> chimaera_pty session, and the
//! /ws/events full-snapshot session bus.
//!
//! Browsers cannot set an Authorization header on a WebSocket, so the first
//! text frame must be `{"type":"auth","token":"..."}` (within 5 seconds).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
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
    },
    Resize {
        cols: u16,
        rows: u16,
    },
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
    let auth_dims = match authenticate(&mut socket, &state).await {
        Some(dims) => dims,
        None => {
            let _ = send_json(
                &mut socket,
                &json!({"type": "error", "message": "unauthorized"}),
            )
            .await;
            return;
        }
    };

    // Adopt the client's grid BEFORE attaching so the snapshot below is
    // rendered at the size the client will actually display it.
    if let Some((cols, rows)) = auth_dims {
        resize_off_reactor(&state, &id, cols, rows, "pre-attach resize").await;
    }

    let attach_res = match attach_off_reactor(&state, &id).await {
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
                let mut ready = match serde_json::to_value(&words.info) {
                    Ok(serde_json::Value::Object(map)) => map,
                    _ => serde_json::Map::new(),
                };
                ready.insert("type".to_string(), json!("ready"));
                ready.insert("cwd_current".to_string(), json!(words.info.cwd.clone()));
                if send_json(&mut socket, &serde_json::Value::Object(ready))
                    .await
                    .is_err()
                {
                    return;
                }
                if socket
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
    if send_json(&mut socket, &serde_json::Value::Object(ready))
        .await
        .is_err()
    {
        return;
    }

    // Snapshot as one binary frame, then enter the bridge loop. Adjacency is
    // a contract: after any reset-bearing frame (`ready` here, `resync` in
    // repaint) the NEXT binary frame is the complete snapshot — the client's
    // parked write-through path relies on nothing interleaving.
    let snapshot = Bytes::from(std::mem::take(&mut attachment.snapshot));
    if socket.send(Message::Binary(snapshot)).await.is_err() {
        return;
    }

    let mut output_open = true;
    let mut events_open = true;
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
    loop {
        tokio::select! {
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
            out = attachment.output.recv(), if output_open => match out {
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
                    let resized_to = match &event {
                        chimaera_pty::SessionEvent::Resized { cols, rows } => Some((*cols, *rows)),
                        _ => None,
                    };
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
                    if let Some(dims) = resized_to {
                        if client_dims != Some(dims) {
                            let at = tokio::time::Instant::now() + RESYNC_DEBOUNCE;
                            resync_sleep.as_mut().reset(at);
                            resync_at = Some(at);
                        }
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => events_open = false,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(bytes))) => {
                    for chunk in bytes.chunks(TERMINAL_INPUT_CHUNK) {
                        if attachment
                            .input
                            .send(Bytes::copy_from_slice(chunk))
                            .await
                            .is_err()
                        {
                            // Session is gone; flush the batched tail (its
                            // last words), tell the client, and hang up.
                            let _ = send_ordered_json(
                                &mut socket,
                                &mut batch,
                                &json!({"type": "exited", "status": null}),
                            )
                            .await;
                            return;
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
) -> Result<anyhow::Result<chimaera_pty::Attachment>, tokio::task::JoinError> {
    let state = Arc::clone(state);
    let id = id.to_string();
    tokio::task::spawn_blocking(move || state.sessions.attach(&id)).await
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
    let mut fresh = match attach_off_reactor(state, id).await {
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

async fn handle_chat(mut socket: WebSocket, id: String, state: Arc<AppState>) {
    let Some(last_seq) = chat_authenticate(&mut socket, &state).await else {
        let _ = send_json(
            &mut socket,
            &json!({"type": "error", "message": "unauthorized"}),
        )
        .await;
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
    loop {
        tokio::select! {
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
                    let switching = crate::lock(&state.chat_switching).get(&id).cloned();
                    let frame = match switching.as_deref() {
                        Some("term") => json!({"type": "degraded"}),
                        Some(_) => json!({"type": "error", "code": "unknown_session",
                                          "message": "session switching"}),
                        None if state.sessions.get(&id).is_some() => json!({"type": "degraded"}),
                        None => json!({"type": "exited",
                                       "status": state.chat.get(&id).and_then(|c| c.exit_status)}),
                    };
                    let _ = send_json(&mut socket, &frame).await;
                    return;
                }
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<chimaera_agent::model::AgentCommand>(&text) {
                        Ok(cmd) => {
                            if let Err(err) = cmd.validate_ingress() {
                                tracing::debug!(%id, %err, "chat command exceeds ingress budget");
                                // Reject only this command. The authenticated
                                // socket and agent remain healthy, so the UI
                                // can correct the payload and retry.
                                let _ = send_json(
                                    &mut socket,
                                    &json!({"type": "error", "code": "invalid_command",
                                            "message": err.to_string()}),
                                )
                                .await;
                                continue;
                            }
                            if let Err(err) = state.chat.command(&id, cmd).await {
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
                                    &json!({"type": "error", "code": code,
                                            "message": message}),
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
        let _ = send_json(
            &mut socket,
            &json!({"type": "error", "message": "unauthorized"}),
        )
        .await;
        return;
    }

    // Released on every exit path below (a leaked watcher would poll git forever).
    let mut watch = crate::git::WatchGuard::new(state.clone());
    // Per-client, bounded mounted-path monitor. Dropping the socket drops every
    // registration, so a closed window costs zero filesystem work.
    let mut fs_watch = crate::fs_watch::FsWatch::new();

    let mut last_sent: Option<Arc<String>> = None;
    let mut last_settings_gen: Option<u64> = None;
    let mut last_git: Option<String> = None;
    let mut last_update_epoch: Option<u64> = None;
    let mut last_recents_epoch: Option<u64> = None;
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

    loop {
        tokio::select! {
            _ = state.changes.notified() => {}
            _ = tokio::time::sleep(EVENTS_TICK) => {}
            msg = socket.recv() => match msg {
                // The only client frame on this bus: which workspace this window
                // shows + the exact mounted paths whose disk state it renders.
                Some(Ok(Message::Text(text))) => {
                    if let Ok(ClientMessage::Watch { workspace_id, files, dirs }) =
                        serde_json::from_str::<ClientMessage>(&text)
                    {
                        watch.set(workspace_id);
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
        let fs_changes = fs_watch.poll(false).await;
        if send_fs_changes(&mut socket, fs_changes).await.is_err() {
            return;
        }
        tokio::time::sleep(EVENTS_THROTTLE).await;
    }
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
    let mut frame = crate::lock(&state.update).to_json();
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

/// Send a `{"type":"git","epochs":{workspace_id:epoch}}` invalidate frame when
/// any workspace's git epoch moved. The status payload never rides this bus —
/// the client refetches `GET /git/status` for its active workspace
/// (invalidate-and-pull keeps big path lists off the daemon-wide firehose). The
/// map is ordered (BTreeMap) so an unchanged snapshot compares equal.
async fn send_git_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    last: &mut Option<String>,
) -> Result<(), axum::Error> {
    let epochs: std::collections::BTreeMap<String, u64> =
        state.git.epochs_snapshot().into_iter().collect();
    let frame = json!({"type": "git", "epochs": epochs}).to_string();
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
async fn authenticate(socket: &mut WebSocket, state: &AppState) -> Option<Option<(u16, u16)>> {
    match tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<ClientMessage>(&text) {
            Ok(ClientMessage::Auth { token, cols, rows }) if token == state.token => {
                Some(cols.zip(rows))
            }
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
        COMMAND_TEXT_TOTAL_MAX,
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
        }));
        let encoded = serde_json::to_vec(&AgentCommand::Send { blocks }).unwrap();
        assert!(encoded.len() <= MAX_CHAT_COMMAND_MESSAGE);
    }
}
