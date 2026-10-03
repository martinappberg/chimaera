//! Actual HTTP/Unix peers, synthetic protection only; not official CLI/Broker.
use super::super::{
    provider_client::tests::request,
    provider_ready::{self, tests::Fixture},
};
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UnixStream},
};

async fn http(frontend: &Frontend, path: &str, auth: &str, extra: &str) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(3),async {
        let mut socket=TcpStream::connect(&frontend.address).await.unwrap();
        let request=format!("POST {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {auth}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n{extra}\r\n{{}}",frontend.address);
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut bytes=Vec::with_capacity(16385);
        socket.take(16385).read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.len()<=16384);
        bytes
    }).await.unwrap()
}
async fn frame(socket: &mut UnixStream, kind: u8, body: &[u8]) {
    let mut header = [kind, 0, 0, 0, 0];
    header[1..].copy_from_slice(&(body.len() as u32).to_be_bytes());
    socket.write_all(&header).await.unwrap();
    socket.write_all(body).await.unwrap();
}
async fn uploaded(socket: &mut UnixStream, req: &wire::Request) {
    let mut body = Vec::new();
    loop {
        let mut header = [0; 5];
        socket.read_exact(&mut header).await.unwrap();
        let header = wire::FrameHeader::decode(&header).unwrap();
        let mut bytes = vec![0; header.length];
        socket.read_exact(&mut bytes).await.unwrap();
        match header.kind {
            wire::FrameKind::RequestData => body.extend_from_slice(&bytes),
            wire::FrameKind::RequestEnd => {
                let end: wire::StreamEnd = serde_json::from_slice(&bytes).unwrap();
                end.validate(req, body.len() as u64).unwrap();
                assert_eq!(body, b"{}");
                return;
            }
            _ => panic!("unexpected synthetic upload frame"),
        }
    }
}
async fn response(socket: &mut UnixStream, req: &wire::Request, body: &[u8], wrong_end: bool) {
    let response = wire::Response {
        version: 1,
        binding: req.binding.clone(),
        request_id: req.request_id.clone(),
        result: wire::Reply::ClaudeHead {
            head: wire::ClaudeHead {
                status: 200,
                headers: wire::Headers {
                    content_type: wire::ContentType::Json,
                    retry_after_seconds: None,
                },
            },
        },
    };
    frame(socket, 3, &wire::encode_control(&response).unwrap()).await;
    frame(socket, 4, body).await;
    let end = wire::StreamEnd {
        version: 1,
        binding: req.binding.clone(),
        request_id: req.request_id.clone(),
        bytes: body.len() as u64 + u64::from(wrong_end),
    };
    frame(socket, 5, &wire::encode_control(&end).unwrap()).await;
    socket.shutdown().await.unwrap();
}
#[tokio::test]
async fn messages_and_count_tokens_preserve_body_and_exact_correlated_end() {
    for (path, route) in [
        ("/v1/messages?beta=true", wire::ClaudeRoute::Messages),
        (
            "/v1/messages/count_tokens?beta=true",
            wire::ClaudeRoute::CountTokens,
        ),
    ] {
        let fixture = Fixture::new(Duration::from_secs(1));
        fixture.verified().await;
        let child =
            ChildLifetime::new(&fixture.state, Instant::now() + Duration::from_secs(3)).unwrap();
        let frontend = Arc::new(Frontend::start(child.clone()).await.unwrap());
        let caller = frontend.clone();
        let task = tokio::spawn(async move { http(&caller, path, caller.token(), "").await });
        let (mut peer, req) = request(&fixture.listener).await;
        assert!(
            matches!(req.command,wire::Command::ClaudeStream{route:r,content_length:2} if r==route)
        );
        uploaded(&mut peer, &req).await;
        response(&mut peer, &req, b"{\"synthetic\":true}", false).await;
        let bytes = task.await.unwrap();
        assert!(bytes.starts_with(b"HTTP/1.1 200"));
        assert!(bytes
            .windows(18)
            .any(|part| part == b"{\"synthetic\":true}"));
        Arc::try_unwrap(frontend).ok().unwrap().stop().await;
        drop(child);
        assert_eq!(provider_ready::active(&fixture.state), 0);
        fixture.finish().await;
    }
}
#[tokio::test]
async fn malformed_http_never_contacts_provider() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let child =
        ChildLifetime::new(&fixture.state, Instant::now() + Duration::from_secs(3)).unwrap();
    let frontend = Frontend::start(child.clone()).await.unwrap();
    for (path, auth, extra) in [
        ("/v1/messages?beta=true", "wrong", ""),
        ("/v1/messages?other=true", frontend.token(), ""),
        (
            "http://attacker.invalid/v1/messages?beta=true",
            frontend.token(),
            "",
        ),
        (
            "/v1/messages?beta=true",
            frontend.token(),
            "Authorization: Bearer other\r\n",
        ),
        (
            "/v1/messages?beta=true",
            frontend.token(),
            "Content-Length: 2\r\n",
        ),
        (
            "/v1/messages?beta=true",
            frontend.token(),
            "Transfer-Encoding: chunked\r\n",
        ),
    ] {
        let bytes = http(&frontend, path, auth, extra).await;
        assert!(!bytes.starts_with(b"HTTP/1.1 200"));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), fixture.listener.accept())
                .await
                .is_err()
        );
    }
    frontend.stop().await;
    drop(child);
    fixture.finish().await;
}
#[tokio::test]
async fn uncertain_after_head_refuses_sdk_retry_and_settles_actual_connections() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let child =
        ChildLifetime::new(&fixture.state, Instant::now() + Duration::from_secs(3)).unwrap();
    let frontend = Arc::new(Frontend::start(child.clone()).await.unwrap());
    let caller = frontend.clone();
    let task =
        tokio::spawn(
            async move { http(&caller, "/v1/messages?beta=true", caller.token(), "").await },
        );
    let (mut peer, req) = request(&fixture.listener).await;
    uploaded(&mut peer, &req).await;
    response(&mut peer, &req, b"{\"partial\":true}", true).await;
    let _ = task.await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while child.current().is_ok() {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    assert!(Owner::for_child(
        &child,
        wire::Command::ClaudeStream {
            route: wire::ClaudeRoute::Messages,
            content_length: 2
        },
        Instant::now() + Duration::from_secs(1)
    )
    .is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), fixture.listener.accept())
            .await
            .is_err()
    );
    Arc::try_unwrap(frontend).ok().unwrap().stop().await;
    drop(child);
    assert_eq!(provider_ready::active(&fixture.state), 0);
    fixture.finish().await;
}
#[tokio::test]
async fn lost_http_observer_and_original_child_deadline_close_owned_streams() {
    for cancel in [true, false] {
        let fixture = Fixture::new(Duration::from_secs(1));
        fixture.verified().await;
        let child = ChildLifetime::new(
            &fixture.state,
            Instant::now() + Duration::from_millis(if cancel { 1500 } else { 150 }),
        )
        .unwrap();
        let frontend = Arc::new(Frontend::start(child.clone()).await.unwrap());
        let caller = frontend.clone();
        let task = tokio::spawn(async move {
            http(&caller, "/v1/messages?beta=true", caller.token(), "").await
        });
        let (mut peer, req) = request(&fixture.listener).await;
        uploaded(&mut peer, &req).await;
        assert_eq!(provider_ready::active(&fixture.state), 2);
        if cancel {
            task.abort();
        }
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while provider_ready::active(&fixture.state) > 1 {
                tokio::task::yield_now().await
            }
        })
        .await
        .unwrap();
        Arc::try_unwrap(frontend).ok().unwrap().stop().await;
        drop(child);
        assert_eq!(provider_ready::active(&fixture.state), 0);
        fixture.finish().await;
    }
}
#[tokio::test]
async fn per_request_and_upload_deadlines_precede_the_original_child_budget() {
    for stall_upload in [false, true] {
        let fixture = Fixture::new(Duration::from_secs(1));
        fixture.verified().await;
        let child =
            ChildLifetime::new(&fixture.state, Instant::now() + Duration::from_secs(3)).unwrap();
        let frontend = Frontend::start_at(
            child.clone(),
            Duration::from_millis(if stall_upload { 1500 } else { 400 }),
            Duration::from_millis(if stall_upload { 400 } else { 1500 }),
        )
        .await
        .unwrap();
        let mut client = TcpStream::connect(&frontend.address).await.unwrap();
        let header=format!("POST /v1/messages?beta=true HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n",frontend.address,frontend.token());
        client.write_all(header.as_bytes()).await.unwrap();
        if !stall_upload {
            client.write_all(b"{}").await.unwrap();
        }
        let (mut peer, req) = request(&fixture.listener).await;
        if !stall_upload {
            uploaded(&mut peer, &req).await;
        }
        let mut output = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(1),
            client.take(4096).read_to_end(&mut output),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(Instant::now() < child.deadline());
        assert!(!output.starts_with(b"HTTP/1.1 200"));
        frontend.stop().await;
        drop(child);
        assert_eq!(provider_ready::active(&fixture.state), 0);
        fixture.finish().await;
    }
}
#[tokio::test]
async fn child_budget_is_separate_from_expired_ready_and_stream_quota() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            ChildLifetime::new(&fixture.state, Instant::now() + Duration::from_secs(1)).unwrap(),
        );
    }
    assert!(matches!(
        ChildLifetime::new(&fixture.state, Instant::now() + Duration::from_secs(1)),
        Err(wire::Error::LimitReached)
    ));
    let owner = Owner::for_child(
        &children[0],
        wire::Command::ClaudeStream {
            route: wire::ClaudeRoute::Messages,
            content_length: 2,
        },
        Instant::now() + Duration::from_secs(600),
    )
    .unwrap();
    assert!(owner.current().is_ok());
    assert_eq!(provider_ready::active(&fixture.state), 5);
    children[0].cancel();
    assert!(owner.current().is_err());
    drop(owner);
    drop(children);
    assert_eq!(provider_ready::active(&fixture.state), 0);
    fixture.finish().await;
}
