//! `/ws/voice` end to end against a stand-in speech service.

use std::sync::{Arc, Mutex};

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::support::*;
use crate::*;

/// The stand-in is one global (`voice::SERVICE_FOR_TESTS`): these tests take
/// turns.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What the stand-in saw.
#[derive(Default)]
struct Seen {
    uri: String,
    x_app: Option<String>,
    keyterms: Option<String>,
    authorization: Option<String>,
    first_text: Option<String>,
    audio_bytes: usize,
    close_stream: bool,
}

/// A speech service that answers the way claude's client expects: an
/// utterance while audio flows, and the last words after `CloseStream`.
// tungstenite's handshake callback returns its own large error response.
#[allow(clippy::result_large_err)]
async fn stand_in(seen: Arc<Mutex<Seen>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let record = seen.clone();
        let mut ws =
            tokio_tungstenite::accept_hdr_async(tcp, move |req: &Request, res: Response| {
                let mut s = record.lock().unwrap();
                s.uri = req.uri().to_string();
                let header = |n: &str| {
                    req.headers()
                        .get(n)
                        .map(|v| v.to_str().unwrap().to_string())
                };
                s.x_app = header("x-app");
                s.keyterms = header("x-config-keyterms");
                s.authorization = header("authorization");
                Ok(res)
            })
            .await
            .unwrap();
        let mut spoke = false;
        while let Some(Ok(message)) = ws.next().await {
            match message {
                WsMessage::Text(text) => {
                    {
                        let mut s = seen.lock().unwrap();
                        s.first_text.get_or_insert_with(|| text.to_string());
                    }
                    if text.contains("CloseStream") {
                        seen.lock().unwrap().close_stream = true;
                        for frame in [
                            r#"{"type":"TranscriptText","data":"last bit"}"#,
                            r#"{"type":"TranscriptEndpoint"}"#,
                        ] {
                            ws.send(WsMessage::text(frame)).await.unwrap();
                        }
                    }
                }
                WsMessage::Binary(audio) => {
                    seen.lock().unwrap().audio_bytes += audio.len();
                    if !spoke {
                        spoke = true;
                        for frame in [
                            r#"{"type":"TranscriptInterim","data":"hello"}"#,
                            r#"{"type":"TranscriptText","data":"hello world"}"#,
                            r#"{"type":"TranscriptEndpoint"}"#,
                        ] {
                            ws.send(WsMessage::text(frame)).await.unwrap();
                        }
                    }
                }
                WsMessage::Close(_) => break,
                _ => {}
            }
        }
    });
    format!("ws://{addr}")
}

async fn daemon() -> (String, tokio::task::JoinHandle<()>) {
    let state = test_state();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("ws://{addr}/ws/voice"), server)
}

async fn frame<S>(socket: &mut S) -> serde_json::Value
where
    S: futures::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        if let WsMessage::Text(text) = next_ws_frame(socket).await {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

#[tokio::test]
async fn voice_relays_audio_and_transcripts() {
    let _turn = SERIAL.lock().await;
    let seen = Arc::new(Mutex::new(Seen::default()));
    *voice::SERVICE_FOR_TESTS.lock().unwrap() = Some(stand_in(seen.clone()).await);
    let (url, server) = daemon().await;

    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let send = |v: serde_json::Value| WsMessage::text(v.to_string());
    socket
        .send(send(
            serde_json::json!({"type": "auth", "token": "test-token"}),
        ))
        .await
        .unwrap();
    socket
        .send(send(serde_json::json!({
            "type": "start",
            "language": "sv-SE",
            "keyterms": ["chimaera", "Claude, Codex", "chimaera"],
        })))
        .await
        .unwrap();
    // Audio sent before the service is connected is buffered, not lost.
    socket
        .send(WsMessage::binary(vec![0u8; 3200]))
        .await
        .unwrap();

    assert_eq!(frame(&mut socket).await["type"], "ready");
    assert_eq!(
        frame(&mut socket).await,
        serde_json::json!({"type": "interim", "text": "hello"})
    );
    assert_eq!(
        frame(&mut socket).await,
        serde_json::json!({"type": "interim", "text": "hello world"})
    );
    assert_eq!(
        frame(&mut socket).await,
        serde_json::json!({"type": "final", "text": "hello world"})
    );
    socket
        .send(WsMessage::binary(vec![0u8; 3200]))
        .await
        .unwrap();
    socket
        .send(send(serde_json::json!({"type": "finalize"})))
        .await
        .unwrap();
    assert_eq!(
        frame(&mut socket).await,
        serde_json::json!({"type": "interim", "text": "last bit"})
    );
    assert_eq!(
        frame(&mut socket).await,
        serde_json::json!({"type": "final", "text": "last bit"})
    );
    assert_eq!(
        frame(&mut socket).await,
        serde_json::json!({"type": "done", "reason": "finished"})
    );

    {
        let s = seen.lock().unwrap();
        assert!(
            s.uri.contains("/api/ws/speech_to_text/voice_stream?"),
            "{}",
            s.uri
        );
        assert!(
            s.uri
                .contains("encoding=linear16&sample_rate=16000&channels=1"),
            "{}",
            s.uri
        );
        assert!(s.uri.contains("language=sv&"), "{}", s.uri);
        assert_eq!(s.x_app.as_deref(), Some("cli"));
        assert_eq!(s.keyterms.as_deref(), Some("chimaera,Claude Codex"));
        // A stand-in never gets the login.
        assert_eq!(s.authorization, None);
        assert_eq!(s.first_text.as_deref(), Some(r#"{"type":"KeepAlive"}"#));
        assert_eq!(s.audio_bytes, 6400);
        assert!(s.close_stream);
    }
    *voice::SERVICE_FOR_TESTS.lock().unwrap() = None;
    server.abort();
}

#[tokio::test]
async fn voice_cancel_discards() {
    let _turn = SERIAL.lock().await;
    let seen = Arc::new(Mutex::new(Seen::default()));
    *voice::SERVICE_FOR_TESTS.lock().unwrap() = Some(stand_in(seen.clone()).await);
    let (url, server) = daemon().await;

    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let send = |v: serde_json::Value| WsMessage::text(v.to_string());
    socket
        .send(send(
            serde_json::json!({"type": "auth", "token": "test-token"}),
        ))
        .await
        .unwrap();
    socket
        .send(send(serde_json::json!({"type": "start"})))
        .await
        .unwrap();
    assert_eq!(frame(&mut socket).await["type"], "ready");
    socket
        .send(send(serde_json::json!({"type": "cancel"})))
        .await
        .unwrap();
    assert_eq!(
        frame(&mut socket).await,
        serde_json::json!({"type": "done", "reason": "cancelled"})
    );
    assert!(seen.lock().unwrap().uri.contains("language=en&"));
    *voice::SERVICE_FOR_TESTS.lock().unwrap() = None;
    server.abort();
}

/// A service that answers every upgrade with `status`; counts the attempts.
async fn refusing(status: &'static str, attempts: Arc<Mutex<usize>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                return;
            };
            *attempts.lock().unwrap() += 1;
            let mut buf = [0u8; 4096];
            let _ = tcp.read(&mut buf).await;
            let reply = format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\n\r\n");
            let _ = tcp.write_all(reply.as_bytes()).await;
        }
    });
    format!("ws://{addr}")
}

#[tokio::test]
async fn voice_bad_request_is_refused_not_retried() {
    let _turn = SERIAL.lock().await;
    let attempts = Arc::new(Mutex::new(0));
    *voice::SERVICE_FOR_TESTS.lock().unwrap() =
        Some(refusing("400 Bad Request", attempts.clone()).await);
    let (url, server) = daemon().await;

    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let send = |v: serde_json::Value| WsMessage::text(v.to_string());
    socket
        .send(send(
            serde_json::json!({"type": "auth", "token": "test-token"}),
        ))
        .await
        .unwrap();
    socket
        .send(send(serde_json::json!({"type": "start"})))
        .await
        .unwrap();
    let error = frame(&mut socket).await;
    assert_eq!(error["code"], "refused");
    assert_eq!(frame(&mut socket).await["type"], "done");
    assert_eq!(*attempts.lock().unwrap(), 1);
    *voice::SERVICE_FOR_TESTS.lock().unwrap() = None;
    server.abort();
}

#[tokio::test]
async fn voice_refused_login_is_reported() {
    let _turn = SERIAL.lock().await;
    // A service that refuses the upgrade the way it refuses a bad login.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = tcp.read(&mut buf).await;
            let _ = tcp
                .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    });
    *voice::SERVICE_FOR_TESTS.lock().unwrap() = Some(format!("ws://{addr}"));
    let (url, server) = daemon().await;

    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let send = |v: serde_json::Value| WsMessage::text(v.to_string());
    socket
        .send(send(
            serde_json::json!({"type": "auth", "token": "test-token"}),
        ))
        .await
        .unwrap();
    socket
        .send(send(serde_json::json!({"type": "start"})))
        .await
        .unwrap();
    let error = frame(&mut socket).await;
    assert_eq!(error["type"], "error");
    assert_eq!(error["code"], "auth");
    assert_eq!(
        frame(&mut socket).await,
        serde_json::json!({"type": "done", "reason": "error"})
    );
    *voice::SERVICE_FOR_TESTS.lock().unwrap() = None;
    server.abort();
}

#[tokio::test]
async fn voice_requires_auth() {
    let (url, server) = daemon().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    socket
        .send(WsMessage::text(
            serde_json::json!({"type": "auth", "token": "wrong"}).to_string(),
        ))
        .await
        .unwrap();
    let error = frame(&mut socket).await;
    assert_eq!(error["code"], "unauthorized");
    server.abort();
}
