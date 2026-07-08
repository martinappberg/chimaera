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
                &json!({"type": "error", "message": format!("unknown session {id}")}),
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
                        // Ignore re-auth and unknown message types.
                        Ok(ClientMessage::Auth { .. }) | Err(_) => {}
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

    let mut last_sent: Option<String> = None;
    let mut last_settings_gen: Option<u64> = None;
    let mut last_git: Option<String> = None;
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

    loop {
        tokio::select! {
            _ = state.changes.notified() => {}
            _ = tokio::time::sleep(EVENTS_TICK) => {}
            msg = socket.recv() => match msg {
                Some(Ok(_)) => continue, // client frames carry nothing here
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
        tokio::time::sleep(EVENTS_THROTTLE).await;
    }
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
        "sessions": crate::api::sessions_json(state),
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
