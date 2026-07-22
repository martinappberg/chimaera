//! Browser-pane reverse proxy: mint policy, the streaming data plane, the
//! absolute-path rescue, and the WebSocket tunnel — driven against real
//! in-test TCP targets (the proxy dials real sockets, so oneshot works).

use super::support::*;
use crate::*;

use axum::http::HeaderMap;

/// A tiny raw HTTP/1.1 target: echoes the request line + interesting headers
/// as JSON (marker "echo-target"), and answers /redirect with a 302 to an
/// absolute path — the shape Jupyter's login/lab redirects have.
async fn spawn_echo_target() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                    if buf.len() > 64 * 1024 {
                        return;
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let mut lines = head.lines();
                let reqline = lines.next().unwrap_or_default().to_string();
                let mut headers = std::collections::HashMap::new();
                for l in lines {
                    if let Some((k, v)) = l.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let path = reqline.split_whitespace().nth(1).unwrap_or("").to_string();
                let resp = if path.starts_with("/redirect") {
                    "HTTP/1.1 302 Found\r\nLocation: /lab?next=1\r\nContent-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                        .to_string()
                } else {
                    let body = serde_json::json!({
                        "marker": "echo-target",
                        "path": path,
                        "host": headers.get("host"),
                        "origin": headers.get("origin"),
                        "referer": headers.get("referer"),
                        "cookie": headers.get("cookie"),
                        "guard": headers.get("x-chimaera-proxied"),
                    })
                    .to_string();
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    addr
}

async fn mint(state: &Arc<AppState>, host: &str, port: u16) -> String {
    let (status, body) = request(
        state,
        Method::POST,
        "/api/v1/proxy",
        Some(serde_json::json!({"host": host, "port": port})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["id"].as_str().unwrap().to_string()
}

/// Oneshot GET with arbitrary extra headers, returning the raw response.
async fn get_with_headers(
    state: &Arc<AppState>,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, bytes::Bytes) {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let res = app(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes)
}

fn body_json(bytes: &bytes::Bytes) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn mint_requires_bearer_and_validates_targets() {
    let state = test_state_with_port(9700);

    // No bearer → 401 (the middleware, like every /api/v1 route).
    let res = app(state.clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/proxy")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"host":"127.0.0.1","port":8888}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Garbage hosts are refused before any classification.
    for bad in ["user@host", "host x", "-oProxyCommand=x", ""] {
        let (status, _) = request(
            &state,
            Method::POST,
            "/api/v1/proxy",
            Some(serde_json::json!({"host": bad, "port": 8888})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "host {bad:?}");
    }

    // The daemon's own port is not a proxy target (same-origin loop).
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/proxy",
        Some(serde_json::json!({"host": "localhost", "port": 9700})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A host that is neither loopback, this host, nor a compute node needs
    // explicit confirmation…
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/proxy",
        Some(serde_json::json!({"host": "elsewhere.example.org", "port": 8888})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "confirm_required");

    // …and is minted once the user confirmed.
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/proxy",
        Some(serde_json::json!({"host": "elsewhere.example.org", "port": 8888, "confirm": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["id"].as_str().unwrap().starts_with("p-"));

    // The daemon's own hostname qualifies without confirmation.
    let (status, body) = request(
        &state,
        Method::POST,
        "/api/v1/proxy",
        Some(serde_json::json!({"host": "testhost", "port": 8888})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn mint_is_idempotent_per_target() {
    let state = test_state();
    let a = mint(&state, "127.0.0.1", 18888).await;
    let b = mint(&state, "127.0.0.1", 18888).await;
    let c = mint(&state, "127.0.0.1", 18889).await;
    assert_eq!(a, b, "same target, same session");
    assert_ne!(a, c, "different port, different session");
}

#[tokio::test]
async fn proxied_request_streams_back_with_headers_rewritten() {
    let state = test_state();
    let target = spawn_echo_target().await;
    let id = mint(&state, "127.0.0.1", target.port()).await;

    let (status, headers, bytes) = get_with_headers(
        &state,
        &format!("/proxy/{id}/hello/world?x=1&y=2"),
        &[
            ("origin", "http://127.0.0.1:9700"),
            ("referer", &format!("http://127.0.0.1:9700/proxy/{id}/lab")),
            ("cookie", &format!("chimaera_proxy={id}; _xsrf=keepme")),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let echo = body_json(&bytes);
    assert_eq!(echo["marker"], "echo-target");
    assert_eq!(
        echo["path"], "/hello/world?x=1&y=2",
        "prefix stripped, query kept"
    );
    assert_eq!(
        echo["host"],
        format!("127.0.0.1:{}", target.port()),
        "Host is the target's own authority"
    );
    assert_eq!(
        echo["origin"],
        format!("http://127.0.0.1:{}", target.port()),
        "Origin moves to the target authority (Jupyter WS origin checks)"
    );
    assert_eq!(
        echo["referer"],
        format!("http://127.0.0.1:{}/lab", target.port()),
        "Referer is de-prefixed and re-homed; the proxy id never leaks upstream"
    );
    assert_eq!(
        echo["cookie"], "_xsrf=keepme",
        "our rescue cookie is stripped"
    );
    assert_eq!(echo["guard"], "1", "loop-guard header travels upstream");
    assert_eq!(
        header_str(&headers, "referrer-policy"),
        "same-origin",
        "injected when the app sets none"
    );
}

#[tokio::test]
async fn redirects_rewrite_under_the_prefix_and_documents_claim_the_cookie() {
    let state = test_state();
    let target = spawn_echo_target().await;
    let id = mint(&state, "127.0.0.1", target.port()).await;

    let (status, headers, _) = get_with_headers(
        &state,
        &format!("/proxy/{id}/redirect"),
        &[("sec-fetch-dest", "iframe"), ("accept", "text/html")],
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(
        header_str(&headers, "location"),
        format!("/proxy/{id}/lab?next=1"),
        "absolute-path Location comes back under the prefix"
    );
    let cookie = header_str(&headers, "set-cookie");
    assert!(
        cookie.starts_with(&format!("chimaera_proxy={id};")),
        "document navigation claims the rescue cookie: {cookie}"
    );
    assert!(cookie.contains("HttpOnly"), "{cookie}");

    // A subresource fetch does NOT claim the cookie.
    let (_, headers, _) =
        get_with_headers(&state, &format!("/proxy/{id}/static/main.js"), &[]).await;
    assert!(headers.get("set-cookie").is_none());
}

#[tokio::test]
async fn bare_proxy_id_redirects_to_slash_form() {
    let state = test_state();
    let target = spawn_echo_target().await;
    let id = mint(&state, "127.0.0.1", target.port()).await;
    let (status, headers, _) =
        get_with_headers(&state, &format!("/proxy/{id}?token=abc"), &[]).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        header_str(&headers, "location"),
        format!("/proxy/{id}/?token=abc"),
        "query (Jupyter's ?token=) survives the slash redirect"
    );
}

#[tokio::test]
async fn fallback_rescues_absolute_paths_via_cookie_and_referer() {
    let state = test_state();
    let target = spawn_echo_target().await;
    let id = mint(&state, "127.0.0.1", target.port()).await;

    // Cookie rescue (what a WebSocket handshake or redirected navigation has).
    let (status, _, bytes) = get_with_headers(
        &state,
        "/static/lab/main.js?v=1",
        &[("cookie", &format!("chimaera_proxy={id}"))],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let echo = body_json(&bytes);
    assert_eq!(echo["marker"], "echo-target");
    assert_eq!(
        echo["path"], "/static/lab/main.js?v=1",
        "original path, unprefixed"
    );

    // Referer rescue (what a page's subresource/XHR requests carry).
    let (status, _, bytes) = get_with_headers(
        &state,
        "/api/kernels?1",
        &[("referer", &format!("http://x/proxy/{id}/lab"))],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_json(&bytes)["path"], "/api/kernels?1");

    // A dead id rescues nothing.
    let (_, _, bytes) = get_with_headers(
        &state,
        "/static/x.js",
        &[(
            "cookie",
            "chimaera_proxy=p-deadbeefdeadbeefdeadbeefdeadbeef",
        )],
    )
    .await;
    assert_ne!(body_json(&bytes)["marker"], "echo-target");
}

#[tokio::test]
async fn workbench_surface_is_never_rescued() {
    let state = test_state();
    let target = spawn_echo_target().await;
    let id = mint(&state, "127.0.0.1", target.port()).await;
    let cookie = format!("chimaera_proxy={id}");

    // "/" is the app shell, cookie or not.
    let (_, _, bytes) = get_with_headers(&state, "/", &[("cookie", &cookie)]).await;
    assert_ne!(
        body_json(&bytes)["marker"],
        "echo-target",
        "/ must stay chimaera"
    );

    // A top-level document navigation is the workbench, not a pane iframe.
    let (_, _, bytes) = get_with_headers(
        &state,
        "/anything",
        &[("cookie", &cookie), ("sec-fetch-dest", "document")],
    )
    .await;
    assert_ne!(
        body_json(&bytes)["marker"],
        "echo-target",
        "top-level navigations must stay chimaera"
    );

    // Reserved daemon namespaces are never proxied.
    let (status, _, bytes) = get_with_headers(&state, "/api/v1/nope", &[("cookie", &cookie)]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_ne!(body_json(&bytes)["marker"], "echo-target");
    let (_, _, bytes) = get_with_headers(&state, "/ws/nope", &[("cookie", &cookie)]).await;
    assert_ne!(body_json(&bytes)["marker"], "echo-target");
}

#[tokio::test]
async fn unknown_revoked_and_unreachable_sessions_answer_honestly() {
    let state = test_state();

    // Unknown id → the expired page.
    let (status, headers, _) = get_with_headers(&state, "/proxy/p-nosuchsession/x", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header_str(&headers, "x-chimaera-proxy"), "expired");

    // Unreachable target → the 502 page (mint a port nothing listens on).
    let port = {
        let sock = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        sock.local_addr().unwrap().port()
    };
    let id = mint(&state, "127.0.0.1", port).await;
    let (status, headers, _) = get_with_headers(&state, &format!("/proxy/{id}/x"), &[]).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(header_str(&headers, "x-chimaera-proxy"), "unreachable");

    // Revoked → expired.
    let (status, _) = request(&state, Method::DELETE, &format!("/api/v1/proxy/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, headers, _) = get_with_headers(&state, &format!("/proxy/{id}/x"), &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header_str(&headers, "x-chimaera-proxy"), "expired");
}

#[tokio::test]
async fn loop_guard_refuses_re_proxied_requests() {
    let state = test_state();
    let target = spawn_echo_target().await;
    let id = mint(&state, "127.0.0.1", target.port()).await;
    let (status, headers, _) = get_with_headers(
        &state,
        &format!("/proxy/{id}/x"),
        &[("x-chimaera-proxied", "1")],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(header_str(&headers, "x-chimaera-proxy"), "loop");
}

#[tokio::test]
async fn websocket_echo_through_the_tunnel() {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let state = test_state();

    // A real WS echo target.
    let ws_target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr = ws_target.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((sock, _)) = ws_target.accept().await {
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else {
                    return;
                };
                use futures::StreamExt;
                while let Some(Ok(msg)) = ws.next().await {
                    if (msg.is_text() || msg.is_binary()) && ws.send(msg).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    let id = mint(&state, "127.0.0.1", ws_addr.port()).await;

    // The daemon on a real listener (upgrades need a live hyper server).
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/proxy/{id}/channels"))
            .await
            .expect("ws connect through the proxy");
    socket
        .send(WsMessage::text("kernel-bytes-through-the-tunnel"))
        .await
        .unwrap();
    match next_ws_frame(&mut socket).await {
        WsMessage::Text(text) => assert_eq!(text.as_str(), "kernel-bytes-through-the-tunnel"),
        other => panic!("expected echoed text frame, got {other:?}"),
    }
}
