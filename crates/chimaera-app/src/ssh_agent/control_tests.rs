use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::watch;
use tokio_tungstenite::{
    tungstenite::{protocol::Role, Message},
    WebSocketStream,
};

type Socket = WebSocketStream<tokio::io::DuplexStream>;
async fn sockets() -> (Socket, Socket) {
    let (a, b) = tokio::io::duplex(256 * 1024);
    tokio::join!(
        WebSocketStream::from_raw_socket(
            a,
            Role::Client,
            Some(chimaera_link::websocket_config(true))
        ),
        WebSocketStream::from_raw_socket(
            b,
            Role::Server,
            Some(chimaera_link::websocket_config(true))
        )
    )
}
async fn send(socket: &mut Socket, request: SshAuthRequest) {
    socket
        .send(Message::Text(
            serde_json::to_string(&request).unwrap().into(),
        ))
        .await
        .unwrap();
}
async fn receive(socket: &mut Socket) -> SshAuthReply {
    let message = tokio::time::timeout(Duration::from_secs(1), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    SshAuthReply::from_frame(message.into_text().unwrap().as_bytes()).unwrap()
}
async fn started(calls: &AtomicUsize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn grant_socket_runs_verified_bind_and_signature_without_replay() {
    let (native, mut keeper) = sockets().await;
    let (cancel, rx) = watch::channel(false);
    let mock = Mock::new();
    let calls = mock.calls.clone();
    let drops = mock.drops.clone();
    let task = tokio::spawn(control::run(verifier(mock), native, rx));
    send(&mut keeper, bind_request("a", 1, valid_bind(b"socket"))).await;
    assert!(matches!(
        receive(&mut keeper).await,
        SshAuthReply::Bound { .. }
    ));
    send(&mut keeper, sign_request("a", 2, valid_sign(b"socket"))).await;
    assert!(matches!(
        receive(&mut keeper).await,
        SshAuthReply::Signature { .. }
    ));
    send(&mut keeper, sign_request("a", 2, valid_sign(b"socket"))).await;
    assert!(matches!(task.await.unwrap(), Err(Failure::InvalidRequest)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    drop(cancel);
}

#[tokio::test]
async fn lost_socket_or_account_owner_cancels_pending_touch_and_drops_agent() {
    for action in 0..3 {
        let (native, mut keeper) = sockets().await;
        let (cancel, rx) = watch::channel(false);
        let mut mock = Mock::new();
        mock.stall_sign = true;
        let calls = mock.calls.clone();
        let drops = mock.drops.clone();
        let task = tokio::spawn(control::run(verifier(mock), native, rx));
        send(&mut keeper, bind_request("a", 1, valid_bind(b"pending"))).await;
        assert!(matches!(
            receive(&mut keeper).await,
            SshAuthReply::Bound { .. }
        ));
        send(&mut keeper, sign_request("a", 2, valid_sign(b"pending"))).await;
        started(&calls).await;
        match action {
            0 => drop(keeper),
            1 => {
                cancel.send_replace(true);
            }
            _ => drop(cancel),
        }
        let outcome = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            outcome,
            Err(Failure::Unavailable | Failure::Revoked)
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn grant_deadline_cancels_idle_socket_without_incoming_message() {
    let (native, _keeper) = sockets().await;
    let (_cancel, rx) = watch::channel(false);
    let v = GrantVerifier::new(
        &selected(),
        Instant::now() + Duration::from_millis(20),
        Mock::new(),
    )
    .ok()
    .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), control::run(v, native, rx))
            .await
            .unwrap(),
        Err(Failure::Expired)
    ));
}

#[tokio::test]
async fn malformed_or_second_ready_never_reaches_local_agent() {
    for message in [
        Message::Binary(vec![1].into()),
        Message::Text("{\"type\":\"ready\"}".into()),
        Message::Text(" ".repeat(chimaera_link::SSH_AUTH_FRAME_MAX + 1).into()),
    ] {
        let (native, mut keeper) = sockets().await;
        let (_cancel, rx) = watch::channel(false);
        let mock = Mock::new();
        let calls = mock.calls.clone();
        let task = tokio::spawn(control::run(verifier(mock), native, rx));
        let _ = keeper.send(message).await;
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn flooding_pending_touch_closes_grant_instead_of_hiding_socket_loss() {
    let (native, mut keeper) = sockets().await;
    let (_cancel, rx) = watch::channel(false);
    let mut mock = Mock::new();
    mock.stall_sign = true;
    let calls = mock.calls.clone();
    let drops = mock.drops.clone();
    let task = tokio::spawn(control::run(verifier(mock), native, rx));
    send(&mut keeper, bind_request("a", 1, valid_bind(b"flood"))).await;
    assert!(matches!(
        receive(&mut keeper).await,
        SshAuthReply::Bound { .. }
    ));
    send(&mut keeper, sign_request("a", 2, valid_sign(b"flood"))).await;
    started(&calls).await;
    for _ in 0..17 {
        let _ = keeper.send(Message::Ping(vec![].into())).await;
    }
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap(),
        Err(Failure::InvalidRequest)
    ));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_or_replayed_frame_cancels_touch_before_a_signature_can_escape() {
    for packet in [
        "{\"type\":\"ready\"}".to_string(),
        "{\"type\":\"unknown\"}".to_string(),
        serde_json::to_string(&sign_request("a", 2, valid_sign(b"invalid-pending"))).unwrap(),
    ] {
        let (native, mut keeper) = sockets().await;
        let (_cancel, rx) = watch::channel(false);
        let mut mock = Mock::new();
        mock.stall_sign = true;
        let calls = mock.calls.clone();
        let drops = mock.drops.clone();
        let task = tokio::spawn(control::run(verifier(mock), native, rx));
        send(
            &mut keeper,
            bind_request("a", 1, valid_bind(b"invalid-pending")),
        )
        .await;
        assert!(matches!(
            receive(&mut keeper).await,
            SshAuthReply::Bound { .. }
        ));
        send(
            &mut keeper,
            sign_request("a", 2, valid_sign(b"invalid-pending")),
        )
        .await;
        started(&calls).await;
        keeper.send(Message::Text(packet.into())).await.unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap(),
            Err(Failure::InvalidRequest)
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(!matches!(keeper.next().await, Some(Ok(Message::Text(_)))));
    }
}
