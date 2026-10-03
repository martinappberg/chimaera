//! Actual Configure + Unix peer; synthetic protection/access, no vendor or Broker.
use super::super::provider_ready::tests::Fixture;
use super::*;
use std::time::Duration;

pub(in crate::pro::execution) async fn request(
    listener: &tokio::net::UnixListener,
) -> (UnixStream, wire::Request) {
    tokio::time::timeout(Duration::from_secs(2), async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut header = [0; 5];
        stream.read_exact(&mut header).await.unwrap();
        let header = wire::FrameHeader::decode(&header).unwrap();
        assert_eq!(header.kind, wire::FrameKind::RequestBegin);
        let mut bytes = Zeroizing::new(vec![0; wire::CONTROL_MAX]);
        stream
            .read_exact(&mut bytes[..header.length])
            .await
            .unwrap();
        let request = wire::Request::decode(&bytes[..header.length]).unwrap();
        (stream, request)
    })
    .await
    .unwrap()
}
pub(in crate::pro::execution) async fn reply(
    stream: &mut UnixStream,
    request: &wire::Request,
    token: &str,
    trailing: bool,
) {
    let reply = wire::Response {
        version: 1,
        binding: request.binding.clone(),
        request_id: request.request_id.clone(),
        result: wire::Reply::GithubAccess {
            access: wire::GithubAccess {
                access_token: wire::AccessToken::new(token.into()).unwrap(),
                username: "x-access-token".into(),
                connection_generation: 3,
                credential_revision: 7,
                expires_at: None,
            },
        },
    };
    let bytes = wire::encode_control(&reply).unwrap();
    let mut header = [3, 0, 0, 0, 0];
    header[1..].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    stream.write_all(&header).await.unwrap();
    stream.write_all(&bytes).await.unwrap();
    if trailing {
        stream.write_all(&[0]).await.unwrap();
    }
    stream.shutdown().await.unwrap();
}
#[tokio::test]
async fn exact_reply_and_eof_return_access_without_opening_execution() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let owner = Owner::new(
        &fixture.state,
        wire::Command::GithubGhAccess {},
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    let task = tokio::spawn(async move { owner.github().await });
    let (mut peer, request) = request(&fixture.listener).await;
    reply(&mut peer, &request, "synthetic-A", false).await;
    let access = task.await.unwrap().unwrap();
    assert_eq!(access.access_token.expose(), "synthetic-A");
    assert!(!crate::pro::may_execute(&fixture.state, "w-a"));
    assert!(!crate::pro::may_restore(&fixture.state, "w-a"));
    fixture.finish().await;
}
#[tokio::test]
async fn late_access_after_configuration_retirement_is_not_delivered() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let owner = Owner::new(
        &fixture.state,
        wire::Command::GithubGhAccess {},
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    let task = tokio::spawn(async move { owner.github().await });
    let (_peer, _) = request(&fixture.listener).await;
    assert_eq!(provider_ready::active(&fixture.state), 1);
    provider_ready::retire(&fixture.state);
    assert!(matches!(
        task.await.unwrap(),
        Err(wire::Error::StateChanged)
    ));
    assert_eq!(provider_ready::active(&fixture.state), 0);
    fixture.finish().await;
}
#[tokio::test]
async fn trailing_frame_wrong_binding_and_unsupported_never_retry() {
    for fault in 0..5 {
        let fixture = Fixture::new(Duration::from_secs(1));
        fixture.verified().await;
        let owner = Owner::new(
            &fixture.state,
            wire::Command::GithubGhAccess {},
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        let task = tokio::spawn(async move { owner.github().await });
        let (mut peer, mut req) = request(&fixture.listener).await;
        if fault == 1 {
            req.binding.launch_generation += 1;
        }
        if fault == 4 {
            req.request_id = "00000000-0000-4000-8000-000000000099".into();
        }
        if fault < 2 || fault == 4 {
            reply(&mut peer, &req, "synthetic-A", fault == 0).await;
        } else if fault == 3 {
            peer.write_all(&[3, 0, 0]).await.unwrap();
            peer.shutdown().await.unwrap();
        } else {
            let bytes = wire::encode_control(&wire::Refusal {
                version: 1,
                binding: req.binding.clone(),
                request_id: req.request_id.clone(),
                error: wire::Error::Unsupported,
            })
            .unwrap();
            let mut header = [6, 0, 0, 0, 0];
            header[1..].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
            peer.write_all(&header).await.unwrap();
            peer.write_all(&bytes).await.unwrap();
            peer.shutdown().await.unwrap();
        }
        assert!(task.await.unwrap().is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), fixture.listener.accept())
                .await
                .is_err()
        );
        fixture.finish().await;
    }
}
#[tokio::test]
async fn stalled_eof_expires_and_project_limit_has_no_queue() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let mut owners = Vec::new();
    for _ in 0..wire::STREAMS_PROJECT {
        owners.push(
            Owner::new(
                &fixture.state,
                wire::Command::GithubGhAccess {},
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap(),
        );
    }
    assert!(matches!(
        Owner::new(
            &fixture.state,
            wire::Command::GithubGhAccess {},
            Instant::now() + Duration::from_secs(1)
        ),
        Err(wire::Error::LimitReached)
    ));
    drop(owners);
    let owner = Owner::new(
        &fixture.state,
        wire::Command::GithubGhAccess {},
        Instant::now() + Duration::from_millis(100),
    )
    .unwrap();
    let task = tokio::spawn(async move { owner.github().await });
    let (mut peer, req) = request(&fixture.listener).await;
    // Send a complete response, but retain its writer: no positive EOF receipt.
    let response = wire::Response {
        version: 1,
        binding: req.binding,
        request_id: req.request_id,
        result: wire::Reply::GithubAccess {
            access: wire::GithubAccess {
                access_token: wire::AccessToken::new("synthetic-A".into()).unwrap(),
                username: "x-access-token".into(),
                connection_generation: 1,
                credential_revision: 1,
                expires_at: None,
            },
        },
    };
    let bytes = wire::encode_control(&response).unwrap();
    let mut header = [3, 0, 0, 0, 0];
    header[1..].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    peer.write_all(&header).await.unwrap();
    peer.write_all(&bytes).await.unwrap();
    assert!(task.await.unwrap().is_err());
    fixture.finish().await;
}
