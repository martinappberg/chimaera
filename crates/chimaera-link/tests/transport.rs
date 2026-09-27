use chimaera_link::*;
use futures::{SinkExt, StreamExt};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::io::{duplex, AsyncWriteExt};
use tokio_tungstenite::{
    tungstenite::{protocol::Role, Message},
    WebSocketStream,
};

#[test]
fn endpoints_reject_downgrade_and_credential_tricks() {
    for endpoint in [
        "http://example.com",
        "http://localhost:80",
        "http://127.0.0.2",
        "https://user:password@example.com",
        "https://example.com/prefix",
        "https://example.com?token=x",
        "file:///tmp/socket",
    ] {
        assert!(Client::new(endpoint, None).is_err(), "{endpoint}");
    }
    assert!(Client::new("https://example.com", None).is_ok());
    assert!(Client::new("http://127.0.0.1:49152", None).is_ok());
}
#[test]
fn pkce_matches_rfc7636_vector_and_rejects_state_ambiguity() {
    assert_eq!(
        Pkce::challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let pkce = Pkce::new();
    assert_eq!(pkce.verifier.len(), 43);
    let mut callback = url::Url::parse("http://127.0.0.1:49152/callback").unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &pkce.state)
        .append_pair("code", "a-code");
    assert_eq!(pkce.callback_code(&callback).unwrap(), "a-code");
    callback.query_pairs_mut().append_pair("state", &pkce.state);
    assert!(pkce.callback_code(&callback).is_err());
    assert!(pkce
        .authorization_url("https://example.com", "http://attacker.invalid/callback")
        .is_err());
}
#[test]
fn secrets_are_redacted_from_debug() {
    let tokens = Tokens {
        access_token: "access-secret".into(),
        refresh_token: "refresh-secret".into(),
        token_type: "Bearer".into(),
        expires_in: 60,
    };
    let daemon = Daemon {
        token: "daemon-secret".into(),
        build: "build".into(),
        sessions: 1,
    };
    assert!(!format!("{tokens:?}{daemon:?}").contains("secret"));
}
#[tokio::test]
async fn bridge_drains_before_eof_and_documents_no_half_close() {
    let (mut source, tcp) = duplex(4096);
    let (a, b) = duplex(4096);
    let ws = WebSocketStream::from_raw_socket(a, Role::Client, Some(websocket_config(false))).await;
    let mut peer =
        WebSocketStream::from_raw_socket(b, Role::Server, Some(websocket_config(false))).await;
    let task = tokio::spawn(bridge(tcp, ws));
    source.write_all(b"before eof").await.unwrap();
    source.shutdown().await.unwrap();
    let mut bytes = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(2), peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Message::Binary(data) => bytes.extend_from_slice(&data),
            Message::Close(_) => break,
            _ => {}
        }
    }
    assert_eq!(bytes, b"before eof");
    task.await.unwrap().unwrap();
}
#[tokio::test]
async fn stalled_reader_backpressures_writer_then_resumes_without_loss() {
    let (mut source, tcp) = duplex(1024);
    let (a, b) = duplex(1024);
    let ws = WebSocketStream::from_raw_socket(a, Role::Client, Some(websocket_config(false))).await;
    let mut peer =
        WebSocketStream::from_raw_socket(b, Role::Server, Some(websocket_config(false))).await;
    let bridge_task = tokio::spawn(bridge(tcp, ws));
    let written = Arc::new(AtomicUsize::new(0));
    let progress = written.clone();
    let writer = tokio::spawn(async move {
        for _ in 0..8192 {
            source.write_all(&[0x5a; 1024]).await.unwrap();
            progress.fetch_add(1024, Ordering::SeqCst);
        }
        source.shutdown().await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let before = written.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        before,
        written.load(Ordering::SeqCst),
        "writer must stall when bounded queue fills"
    );
    assert!(before <= (MAX_IN_FLIGHT + 3) * MAX_DATA_FRAME);
    let mut received = 0;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(message)) = peer.next().await {
            match message {
                Message::Binary(data) => {
                    assert!(data.iter().all(|b| *b == 0x5a));
                    received += data.len();
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(received, 8 * 1024 * 1024);
    writer.await.unwrap();
    bridge_task.await.unwrap().unwrap();
}
#[tokio::test]
async fn data_plane_rejects_text_and_oversize_messages() {
    for message in [
        Message::Text("not bytes".into()),
        Message::Binary(vec![0; MAX_DATA_FRAME + 1].into()),
    ] {
        let (_source, tcp) = duplex(1024);
        let (a, b) = duplex(2 * MAX_DATA_FRAME);
        let ws =
            WebSocketStream::from_raw_socket(a, Role::Server, Some(websocket_config(false))).await;
        let mut peer = WebSocketStream::from_raw_socket(b, Role::Client, None).await;
        let task = tokio::spawn(bridge(tcp, ws));
        peer.send(message).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_err());
    }
}
