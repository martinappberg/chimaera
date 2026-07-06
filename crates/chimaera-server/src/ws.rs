//! WebSocket bridge: /ws/sessions/{id} <-> chimaera_pty session.
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

/// Client -> server text frames.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Auth { token: String },
    Resize { cols: u16, rows: u16 },
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
    if !authenticate(&mut socket, &state).await {
        let _ = send_json(
            &mut socket,
            &json!({"type": "error", "message": "unauthorized"}),
        )
        .await;
        return;
    }

    let mut attachment = match state.sessions.attach(&id) {
        Ok(attachment) => attachment,
        Err(err) => {
            tracing::debug!(%id, %err, "ws attach failed");
            let _ = send_json(
                &mut socket,
                &json!({"type": "error", "message": format!("unknown session {id}")}),
            )
            .await;
            return;
        }
    };

    // Ready frame: {"type":"ready", ...SessionInfo fields...}
    let mut ready = match serde_json::to_value(&attachment.info) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    ready.insert("type".to_string(), json!("ready"));
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
    loop {
        tokio::select! {
            out = attachment.output.recv(), if output_open => match out {
                Ok(bytes) => {
                    if socket.send(Message::Binary(bytes)).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    tracing::debug!(%id, skipped, "ws output lagged; resyncing");
                    match state.sessions.attach(&id) {
                        Ok(mut fresh) => {
                            if send_json(&mut socket, &json!({"type": "resync"})).await.is_err() {
                                return;
                            }
                            let snapshot = Bytes::from(std::mem::take(&mut fresh.snapshot));
                            if socket.send(Message::Binary(snapshot)).await.is_err() {
                                return;
                            }
                            attachment = fresh;
                        }
                        Err(err) => {
                            tracing::debug!(%id, %err, "ws resync attach failed");
                            output_open = false;
                        }
                    }
                }
                Err(RecvError::Closed) => output_open = false,
            },
            event = attachment.events.recv(), if events_open => match event {
                Ok(event) => match serde_json::to_value(&event) {
                    Ok(value) => {
                        if send_json(&mut socket, &value).await.is_err() {
                            return;
                        }
                    }
                    Err(err) => tracing::warn!(%id, %err, "failed to serialize session event"),
                },
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

/// First-frame auth: text `{"type":"auth","token":...}` within 5 seconds.
async fn authenticate(socket: &mut WebSocket, state: &AppState) -> bool {
    match tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            matches!(
                serde_json::from_str::<ClientMessage>(&text),
                Ok(ClientMessage::Auth { token }) if token == state.token
            )
        }
        _ => false,
    }
}

async fn send_json(socket: &mut WebSocket, value: &serde_json::Value) -> Result<(), axum::Error> {
    socket.send(Message::Text(value.to_string().into())).await
}
