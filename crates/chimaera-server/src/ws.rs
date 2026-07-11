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
use tokio::sync::broadcast::error::RecvError;

use crate::AppState;

const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
/// Coalesce window for repaints triggered by *other* clients' resizes: an
/// interactive divider drag fires resizes in bursts, and every repaint is a
/// full-screen rewrite.
const RESYNC_DEBOUNCE: Duration = Duration::from_millis(120);

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
    },
}

/// GET /ws/sessions/{id}
pub(crate) async fn session_ws(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| handle(socket, id, state))
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
        if let Err(err) = state.sessions.resize(&id, cols, rows) {
            tracing::debug!(%id, %err, "pre-attach resize failed");
        }
    }

    let mut attachment = match state.sessions.attach(&id) {
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

    // Snapshot as one binary frame, then enter the bridge loop.
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
    loop {
        tokio::select! {
            _ = async move {
                match resync_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                resync_at = None;
                if !resync(&mut socket, &id, &state, &mut attachment).await {
                    return;
                }
            },
            out = attachment.output.recv(), if output_open => match out {
                Ok(bytes) => {
                    if socket.send(Message::Binary(bytes)).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    tracing::debug!(%id, skipped, "ws output lagged; resyncing");
                    resync_at = None;
                    if !resync(&mut socket, &id, &state, &mut attachment).await {
                        return;
                    }
                }
                Err(RecvError::Closed) => output_open = false,
            },
            event = attachment.events.recv(), if events_open => match event {
                Ok(event) => {
                    let resized_to = match &event {
                        chimaera_pty::SessionEvent::Resized { cols, rows } => Some((*cols, *rows)),
                        _ => None,
                    };
                    match serde_json::to_value(&event) {
                        Ok(value) => {
                            if send_json(&mut socket, &value).await.is_err() {
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
                            resync_at = Some(tokio::time::Instant::now() + RESYNC_DEBOUNCE);
                        }
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => events_open = false,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(bytes))) => {
                    if attachment.input.send(bytes).await.is_err() {
                        // Session is gone; tell the client and hang up.
                        let _ = send_json(
                            &mut socket,
                            &json!({"type": "exited", "status": null}),
                        )
                        .await;
                        return;
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::Resize { cols, rows }) => {
                            client_dims = Some((cols, rows));
                            if let Err(err) = state.sessions.resize(&id, cols, rows) {
                                tracing::debug!(%id, %err, "ws resize failed");
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

/// Repaint the client from the authoritative grid: fresh attach, a
/// dims-tagged resync frame (the client resizes BEFORE replaying — a snapshot
/// replayed at any other width re-wraps into garbage), then the snapshot.
/// The events subscription is deliberately kept: swapping it could drop an
/// Exited/Title event broadcast during the swap. Returns false when the
/// socket is gone.
async fn resync(
    socket: &mut WebSocket,
    id: &str,
    state: &AppState,
    attachment: &mut chimaera_pty::Attachment,
) -> bool {
    match state.sessions.attach(id) {
        Ok(mut fresh) => {
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
            true
        }
        Err(err) => {
            // Session gone mid-resync; the kept events channel delivers the
            // Exited that explains it.
            tracing::debug!(%id, %err, "resync attach failed");
            true
        }
    }
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
    ws.on_upgrade(move |socket| handle_chat(socket, id, state))
}

/// Chat replay batch size: bounds per-frame size without flooding the socket
/// with one frame per event on a cold attach.
const CHAT_BATCH: usize = 128;

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
        "replay_from": last_seq,
        // The journal's highest seq now. A client whose own last_seq exceeds
        // this is stale (the journal was recreated and numbering restarted);
        // it hard-resets rather than silently dropping every replayed event.
        "head": attachment.head_seq,
    });
    if send_json(&mut socket, &ready).await.is_err() {
        return;
    }

    let mut sent_seq = last_seq;
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
                            if let Err(err) = state.chat.command(&id, cmd).await {
                                tracing::debug!(%id, %err, "chat command failed");
                                // code=command_failed: one refused command is
                                // NOT a dead socket — without the code the
                                // client treats this frame as fatal and stops
                                // reconnecting forever (additive field; old
                                // clients ignore unknown codes and keep their
                                // previous behavior).
                                let _ = send_json(
                                    &mut socket,
                                    &json!({"type": "error", "code": "command_failed",
                                            "message": "agent unavailable"}),
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
    for chunk in replay.chunks(CHAT_BATCH) {
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
    }
    true
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
    ws.on_upgrade(move |socket| handle_events(socket, state))
}

/// Minimum gap between snapshot frames (<= 4/s).
const EVENTS_THROTTLE: Duration = Duration::from_millis(250);
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

    let mut last_sent: Option<String> = None;
    let mut last_settings_gen: Option<u64> = None;
    let mut last_git: Option<String> = None;
    let mut last_update_epoch: Option<u64> = None;
    let mut last_recents_epoch: Option<u64> = None;
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
                // is showing, so the git backstop knows what to poll.
                Some(Ok(Message::Text(text))) => {
                    if let Ok(ClientMessage::Watch { workspace_id }) =
                        serde_json::from_str::<ClientMessage>(&text)
                    {
                        watch.set(workspace_id);
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
        tokio::time::sleep(EVENTS_THROTTLE).await;
    }
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
/// content generation moved (PUT or hand-edit; the store re-stats the file).
async fn send_settings_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    last_gen: &mut Option<u64>,
) -> Result<(), axum::Error> {
    let (generation, map) = {
        let mut store = crate::lock(&state.settings);
        let generation = store.generation();
        if *last_gen == Some(generation) {
            return Ok(());
        }
        (generation, store.current().clone())
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
async fn send_sessions_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    last_sent: &mut Option<String>,
) -> Result<(), axum::Error> {
    let snapshot = json!({
        "type": "sessions",
        "sessions": crate::session_view::sessions_json(state),
        "links": crate::links::links_json(state),
    })
    .to_string();
    if last_sent.as_deref() == Some(snapshot.as_str()) {
        return Ok(());
    }
    socket.send(Message::Text(snapshot.clone().into())).await?;
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
