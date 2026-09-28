//! A one-use loopback return carries navigation intent, never entitlement.
use anyhow::{Context, Result};
use chimaera_link::DesktopBillingCallback;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Success,
    Canceled,
    Portal,
}

pub(super) struct Callback {
    pub outcome: Outcome,
    socket: TcpStream,
}
impl Callback {
    pub async fn finish(self) {
        let (title, message) = if self.outcome == Outcome::Canceled {
            (
                "Back in chimaera",
                "Checkout was canceled. You can close this tab and continue in the app.",
            )
        } else {
            ("Back in chimaera", "Chimaera is checking your account and will update automatically. You can close this tab.")
        };
        let body = include_str!("../../../../assets/sign-in.html")
            .replace("{{title}}", title)
            .replace("{message}", message)
            .replace("{{footer}}", "Secure account return");
        reply(self.socket, "200 OK", &body).await;
    }
}

async fn reply(mut socket: TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        socket.write_all(response.as_bytes()).await?;
        socket.shutdown().await
    })
    .await;
}

fn parse(request: &[u8], host: &str, nonce: &str, checkout: bool) -> Option<Outcome> {
    let text = std::str::from_utf8(request).ok()?;
    let (headers, rest) = text.split_once("\r\n\r\n")?;
    if !rest.is_empty() {
        return None;
    }
    let mut lines = headers.split("\r\n");
    let mut first = lines.next()?.split(' ');
    if first.next()? != "GET" {
        return None;
    }
    let target = first.next()?;
    if !matches!(first.next()?, "HTTP/1.1" | "HTTP/1.0") || first.next().is_some() {
        return None;
    }
    let mut seen_host = false;
    for line in lines {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("host") {
            if seen_host || value.trim() != host {
                return None;
            }
            seen_host = true;
        }
        if name.eq_ignore_ascii_case("transfer-encoding")
            || (name.eq_ignore_ascii_case("content-length") && value.trim() != "0")
        {
            return None;
        }
    }
    if !seen_host || !target.starts_with("/billing/callback?") {
        return None;
    }
    let url = url::Url::parse(&format!("http://{host}{target}")).ok()?;
    if url.fragment().is_some() {
        return None;
    }
    let pairs: Vec<_> = url.query_pairs().collect();
    if pairs.len() != 2 {
        return None;
    }
    let states: Vec<_> = pairs.iter().filter(|(key, _)| key == "state").collect();
    if states.len() != 1 || !same_nonce(states[0].1.as_bytes(), nonce.as_bytes()) {
        return None;
    }
    let outcomes: Vec<_> = pairs.iter().filter(|(key, _)| key == "outcome").collect();
    if outcomes.len() != 1 {
        return None;
    }
    match (checkout, outcomes[0].1.as_ref()) {
        (true, "success") => Some(Outcome::Success),
        (true, "canceled") => Some(Outcome::Canceled),
        (false, "portal") => Some(Outcome::Portal),
        _ => None,
    }
}
fn same_nonce(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

pub(super) async fn receive(
    listener: TcpListener,
    callback: &DesktopBillingCallback,
    checkout: bool,
) -> Result<Callback> {
    callback.validate()?;
    let expected = url::Url::parse(&callback.redirect_uri)?;
    let host = format!(
        "127.0.0.1:{}",
        expected.port().context("missing callback port")?
    );
    loop {
        let (mut socket, _) = listener.accept().await?;
        let request = tokio::time::timeout(Duration::from_secs(5), async {
            let mut request = Vec::with_capacity(1024);
            while request.len() < 8192 {
                let mut bytes = [0; 1024];
                let remaining = (8192 - request.len()).min(bytes.len());
                let n = socket.read(&mut bytes[..remaining]).await?;
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&bytes[..n]);
                if request.windows(4).any(|part| part == b"\r\n\r\n") {
                    return Ok::<_, std::io::Error>(Some(request));
                }
            }
            Ok(None)
        })
        .await;
        if let Some(outcome) = request
            .ok()
            .and_then(Result::ok)
            .flatten()
            .and_then(|request| parse(&request, &host, &callback.state, checkout))
        {
            return Ok(Callback { outcome, socket });
        }
        reply(
            socket,
            "400 Bad Request",
            "This return could not be verified. Return to chimaera to check your account.",
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(query: &str) -> Vec<u8> {
        format!("GET /billing/callback?{query} HTTP/1.1\r\nHost: 127.0.0.1:32100\r\n\r\n")
            .into_bytes()
    }
    #[test]
    fn strict_returns_reject_host_query_and_kind_confusion() {
        let nonce = "A".repeat(43);
        let query = format!("state={nonce}&outcome=success");
        assert_eq!(
            parse(&request(&query), "127.0.0.1:32100", &nonce, true),
            Some(Outcome::Success)
        );
        for query in [
            format!("{query}&state={nonce}"),
            format!("{query}&extra=1"),
            "state=wrong&outcome=success".to_string(),
            format!("state={nonce}&outcome=portal"),
            format!("{query}#fragment"),
        ] {
            assert!(parse(&request(&query), "127.0.0.1:32100", &nonce, true).is_none());
        }
        for value in [
            String::from_utf8(request(&query))
                .unwrap()
                .replace("127.0.0.1", "localhost"),
            String::from_utf8(request(&query))
                .unwrap()
                .replace("\r\n\r\n", "\r\nHost: 127.0.0.1:32100\r\n\r\n"),
            String::from_utf8(request(&query))
                .unwrap()
                .replace("GET ", "POST "),
            String::from_utf8(request(&query))
                .unwrap()
                .replace(" HTTP/1.1", " HTTP/1.2"),
        ] {
            assert!(parse(value.as_bytes(), "127.0.0.1:32100", &nonce, true).is_none());
        }
    }
    #[tokio::test]
    async fn an_incomplete_request_does_not_survive_its_attempt_deadline() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let callback = DesktopBillingCallback {
            redirect_uri: format!("http://{address}/billing/callback"),
            state: chimaera_link::Pkce::new().state,
        };
        let server = tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_millis(40),
                receive(listener, &callback, true),
            )
            .await
            .is_err()
        });
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket.write_all(b"GET /billing/callback?").await.unwrap();
        assert!(server.await.unwrap());
        assert!(TcpStream::connect(address).await.is_err());
    }

    #[tokio::test]
    async fn accepted_return_is_one_use_and_response_contains_no_nonce_or_entitlement() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let nonce = chimaera_link::Pkce::new().state;
        let callback = DesktopBillingCallback {
            redirect_uri: format!("http://{address}/billing/callback"),
            state: nonce.clone(),
        };
        let server = tokio::spawn(async move {
            receive(listener, &callback, true)
                .await
                .unwrap()
                .finish()
                .await;
        });
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket.write_all(format!("GET /billing/callback?state={nonce}&outcome=success HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes()).await.unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).await.unwrap();
        server.await.unwrap();
        assert!(response.contains("checking your account"));
        assert!(response.contains("Secure account return"));
        assert!(!response.contains("Secure desktop sign-in"));
        assert!(!response.contains("{{footer}}"));
        assert!(!response.contains(&nonce));
        assert!(!response.contains("plan is active"));
        assert!(TcpStream::connect(address).await.is_err());
    }
}
