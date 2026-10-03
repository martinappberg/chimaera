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

fn metadata(path: &str) -> String {
    let beta = if path.contains("count_tokens") {
        "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,token-counting-2024-11-01"
    } else {
        "claude-code-20250219,oauth-2025-04-20"
    };
    format!("Anthropic-Version: 2023-06-01\r\nAnthropic-Beta: {beta}\r\nUser-Agent: claude-cli/2.1.287 (external, cli)\r\n")
}
fn pinned(route: wire::ClaudeRoute) -> wire::ClaudeRequestHeaders {
    let beta = if route == wire::ClaudeRoute::CountTokens {
        "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,token-counting-2024-11-01"
    } else {
        "claude-code-20250219,oauth-2025-04-20"
    };
    wire::ClaudeRequestHeaders::from_http(
        "2023-06-01",
        beta,
        "claude-cli/2.1.287 (external, cli)",
        route,
    )
    .unwrap()
}
async fn http(frontend: &Frontend, path: &str, auth: &str, extra: &str) -> Vec<u8> {
    http_at(frontend, path, auth, &metadata(path), extra).await
}
async fn http_at(
    frontend: &Frontend,
    path: &str,
    auth: &str,
    headers: &str,
    extra: &str,
) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(3),async {
        let mut socket=TcpStream::connect(&frontend.address).await.unwrap();
        let request=format!("POST {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {auth}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n{}{extra}\r\n{{}}",frontend.address,headers);
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
            matches!(&req.command,wire::Command::ClaudeStreamPinned{route:r,content_length:2,headers} if *r==route && *headers==pinned(route))
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
        (
            "/v1/messages?beta=true",
            frontend.token(),
            "Anthropic-Beta: oauth-2025-04-20\r\n",
        ),
        (
            "/v1/messages?beta=true",
            frontend.token(),
            "Anthropic-Version: 2023-06-01\r\n",
        ),
        (
            "/v1/messages?beta=true",
            frontend.token(),
            "User-Agent: claude-cli/2.1.288 (external, cli)\r\n",
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
        wire::Command::ClaudeStreamPinned {
            route: wire::ClaudeRoute::Messages,
            headers: pinned(wire::ClaudeRoute::Messages),
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
        let header=format!("POST /v1/messages?beta=true HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n{}\r\n",frontend.address,frontend.token(),metadata("/v1/messages?beta=true"));
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
    assert!(matches!(
        Owner::for_child(
            &children[0],
            wire::Command::ClaudeStream {
                route: wire::ClaudeRoute::Messages,
                content_length: 2,
            },
            Instant::now() + Duration::from_secs(1)
        ),
        Err(wire::Error::Unsupported)
    ));
    let owner = Owner::for_child(
        &children[0],
        wire::Command::ClaudeStreamPinned {
            route: wire::ClaudeRoute::Messages,
            headers: pinned(wire::ClaudeRoute::Messages),
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

#[tokio::test]
async fn observed_messages_beta_sets_survive_the_http_to_unix_boundary() {
    for agent in [
        "claude-cli/2.1.287 (external, cli)",
        "claude-cli/2.1.287 (external, sdk-cli)",
    ] {
        for beta in [
        "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,redact-thinking-2026-02-12,thinking-token-count-2026-05-13,context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,per-turn-control-2026-07-01,mid-conversation-tool-changes-2026-07-01,effort-2025-11-24,structured-outputs-2025-12-15",
        "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,thinking-token-count-2026-05-13,context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,per-turn-control-2026-07-01,mid-conversation-tool-changes-2026-07-01,effort-2025-11-24,dangerous-tool-use-2026-09-03,thinking-display-updates-2026-08-18,afk-mode-2026-01-31,extended-cache-ttl-2025-04-11",
    ] {
        let fixture=Fixture::new(Duration::from_secs(1)); fixture.verified().await;
        let child=ChildLifetime::new(&fixture.state,Instant::now()+Duration::from_secs(3)).unwrap();
        let frontend=Arc::new(Frontend::start(child.clone()).await.unwrap()); let caller=frontend.clone();
        let headers=format!("Anthropic-Version: 2023-06-01\r\nAnthropic-Beta: {beta}\r\nUser-Agent: {agent}\r\n");
        let task=tokio::spawn(async move { http_at(&caller,"/v1/messages?beta=true",caller.token(),&headers,"").await });
        let(mut peer,req)=request(&fixture.listener).await;
        let wire::Command::ClaudeStreamPinned{headers,route,..}=&req.command else {panic!("missing pinned metadata")};
        assert!(*route==wire::ClaudeRoute::Messages); assert_eq!(headers.beta_header().unwrap(),beta);
        assert_eq!(headers.version.as_str(),"2023-06-01"); assert_eq!(headers.user_agent.as_str(),agent);
        uploaded(&mut peer,&req).await; response(&mut peer,&req,b"{}",false).await;
        assert!(task.await.unwrap().starts_with(b"HTTP/1.1 200"));
        Arc::try_unwrap(frontend).ok().unwrap().stop().await; drop(child);
        assert_eq!(provider_ready::active(&fixture.state),0); fixture.finish().await;
    }
    }
}
#[tokio::test]
async fn unknown_missing_or_changed_cli_metadata_never_claims_a_provider_stream() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let child =
        ChildLifetime::new(&fixture.state, Instant::now() + Duration::from_secs(3)).unwrap();
    let frontend = Frontend::start(child.clone()).await.unwrap();
    let good = metadata("/v1/messages?beta=true");
    for headers in [
        String::new(),
        good.replace("2023-06-01", "2024-01-01"),
        good.replace("oauth-2025-04-20", "oauth-2025-04-20,unknown-beta"),
        good.replace("2.1.287", "2.1.288"),
        good.replace("(external, cli)", "(external, sdk-cli, agent-sdk/1)"),
        good.replace("(external, cli)", "(external, sdk-cli, workload/cron)"),
        good.replace("(external, cli)", "(external, sdk)"),
        good.replace("oauth-2025-04-20", "oauth-2025-04-20,oauth-2025-04-20"),
    ] {
        let bytes = http_at(
            &frontend,
            "/v1/messages?beta=true",
            frontend.token(),
            &headers,
            "",
        )
        .await;
        assert!(bytes.starts_with(b"HTTP/1.1 400"));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), fixture.listener.accept())
                .await
                .is_err()
        );
        assert_eq!(provider_ready::active(&fixture.state), 1);
    }
    let count_headers = metadata("/v1/messages/count_tokens?beta=true")
        .replace("(external, cli)", "(external, sdk-cli)");
    let bytes = http_at(
        &frontend,
        "/v1/messages/count_tokens?beta=true",
        frontend.token(),
        &count_headers,
        "",
    )
    .await;
    assert!(bytes.starts_with(b"HTTP/1.1 400"));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), fixture.listener.accept())
            .await
            .is_err()
    );
    assert_eq!(provider_ready::active(&fixture.state), 1);
    frontend.stop().await;
    drop(child);
    assert_eq!(provider_ready::active(&fixture.state), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn diagnostic_reasons_separate_actual_raw_and_metadata_refusals_before_owner() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let child =
        ChildLifetime::new(&fixture.state, Instant::now() + Duration::from_secs(3)).unwrap();
    let trace = diagnostics::Trace::synthetic();
    let frontend = Frontend::start_diagnostic(child, Some(trace.clone()))
        .await
        .unwrap();
    let path = "/v1/messages?beta=true";
    assert!(
        http(&frontend, path, frontend.token(), "Content-Length: 2\r\n")
            .await
            .is_empty()
    );
    let response = http_at(
        &frontend,
        path,
        frontend.token(),
        &metadata(path).replace("2023-06-01", "synthetic-unsupported"),
        "",
    )
    .await;
    assert!(response.starts_with(b"HTTP/1.1 400"));
    assert_eq!(
        trace.reasons(),
        (
            diagnostics::Refusal::RawLengthDuplicate as u8 + 1,
            diagnostics::Refusal::RequestVersion as u8 + 1
        )
    );
    assert_eq!(trace.counts(), (2, 1, 1));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), fixture.listener.accept())
            .await
            .is_err()
    );
    frontend.stop().await;
    fixture.finish().await;
}

#[tokio::test]
async fn actual_request_metadata_diagnostics_never_accept_unknown_or_missing_profiles() {
    use diagnostics::Refusal;
    let path = "/v1/messages?beta=true";
    for (headers, expected) in [
        (String::new(), Refusal::RequestVersionHeader),
        (
            metadata(path).replace(
                "claude-cli/2.1.287 (external, cli)",
                "synthetic-unsupported",
            ),
            Refusal::RequestUserAgent,
        ),
        (
            metadata(path).replace(
                "claude-code-20250219,oauth-2025-04-20",
                "synthetic-unsupported",
            ),
            Refusal::RequestBeta,
        ),
    ] {
        let fixture = Fixture::new(Duration::from_secs(1));
        fixture.verified().await;
        let child =
            ChildLifetime::new(&fixture.state, Instant::now() + Duration::from_secs(3)).unwrap();
        let trace = diagnostics::Trace::synthetic();
        let frontend = Frontend::start_diagnostic(child, Some(trace.clone()))
            .await
            .unwrap();
        let response = http_at(&frontend, path, frontend.token(), &headers, "").await;
        assert!(response.starts_with(b"HTTP/1.1 400"));
        assert_eq!(trace.reasons(), (0, expected as u8 + 1));
        assert_eq!(trace.counts(), (1, 0, 1));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), fixture.listener.accept())
                .await
                .is_err()
        );
        frontend.stop().await;
        fixture.finish().await;
    }
}
