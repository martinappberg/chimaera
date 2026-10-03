use crate::{MAX_DATA_FRAME, MAX_IN_FLIGHT};
use anyhow::{bail, Result};
use futures::{Sink, SinkExt, Stream, StreamExt};
use std::{fmt::Display, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, watch},
    time::{timeout, Instant},
};
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message};

pub fn websocket_config(control: bool) -> WebSocketConfig {
    let size = if control {
        crate::MAX_CONTROL_FRAME
    } else {
        MAX_DATA_FRAME
    };
    WebSocketConfig::default()
        .read_buffer_size(MAX_DATA_FRAME)
        .write_buffer_size(0)
        .max_write_buffer_size((MAX_IN_FLIGHT + 1) * size)
        .max_message_size(Some(size))
        .max_frame_size(Some(size))
}

/// Bidirectional byte bridge. EOF in either direction closes the entire stream;
/// WebSocket v0 deliberately has no TCP half-close representation.
///
/// A 16-frame queue bounds data read ahead even when the peer stops reading.
/// Generic messages permit server adapters (e.g. axum) without linking a server
/// framework into the app. Text/data fragments over 64 KiB are rejected.
pub async fn bridge<T, W, E>(tcp: T, ws: W) -> Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
    W: Stream<Item = std::result::Result<Message, E>> + Sink<Message, Error = E> + Unpin,
    E: Display,
{
    let (mut tcp_rx, mut tcp_tx) = tokio::io::split(tcp);
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (tx, mut rx) = mpsc::channel(MAX_IN_FLIGHT);
    let (pong_tx, pong_rx) = watch::channel(Instant::now());
    let outbound = async {
        let mut buffer = vec![0; MAX_DATA_FRAME];
        loop {
            let n = tcp_rx.read(&mut buffer).await?;
            let message = if n == 0 {
                Message::Close(None)
            } else {
                Message::Binary(buffer[..n].to_vec().into())
            };
            if tx.send(message).await.is_err() {
                return Ok(());
            }
            if n == 0 {
                return std::future::pending::<Result<()>>().await;
            }
        }
    };
    let incoming = async {
        while let Some(message) = ws_rx.next().await {
            match message.map_err(|e| anyhow::anyhow!("websocket read: {e}"))? {
                Message::Binary(data) if data.len() <= MAX_DATA_FRAME => {
                    tcp_tx.write_all(&data).await?
                }
                Message::Binary(_) | Message::Text(_) => bail!("invalid data-plane frame"),
                Message::Ping(data) => {
                    if tx.send(Message::Pong(data)).await.is_err() {
                        break;
                    }
                }
                Message::Pong(_) => {
                    let _ = pong_tx.send(Instant::now());
                }
                Message::Close(_) => break,
                Message::Frame(_) => bail!("unexpected raw websocket frame"),
            }
        }
        tcp_tx.shutdown().await?;
        Ok(())
    };
    let writer = async {
        let mut ping = tokio::time::interval_at(
            Instant::now() + Duration::from_secs(20),
            Duration::from_secs(20),
        );
        loop {
            let message = tokio::select! {
                message = rx.recv() => match message { Some(message) => message, None => return Ok(()) },
                _ = ping.tick() => {
                    if pong_rx.borrow().elapsed() >= Duration::from_secs(60) { bail!("websocket pong timeout"); }
                    Message::Ping(Vec::new().into())
                }
            };
            let closed = matches!(message, Message::Close(_));
            timeout(Duration::from_secs(60), ws_tx.send(message))
                .await?
                .map_err(|e| anyhow::anyhow!("websocket write: {e}"))?;
            if closed {
                return Ok(());
            }
        }
    };
    tokio::select! { result = outbound => result, result = incoming => result, result = writer => result }
}
