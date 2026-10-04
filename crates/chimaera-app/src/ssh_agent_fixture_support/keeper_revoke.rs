//! C2 observes an actual held Link stream across account revocation. Fixed
//! localhost fixture inputs only; no SSH, operation, terminal or route authority.
use chimaera_link::{Client, ClusterJobState, ClusterOperation, ClusterReply, Tokens};
use futures_util::{SinkExt, StreamExt};
use std::{future::Future, io::Write, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{timeout_at, Instant},
};
use tokio_tungstenite::tungstenite::{error::ProtocolError, Error, Message};
use zeroize::Zeroizing;

const HOST: &str = "fixture-host";
const BATCH: &str = "j-00000001";
const TOKEN: &str = "synthetic-route-device-token";
const HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n";

async fn bounded<T>(deadline: Instant, future: impl Future<Output = T>) -> Result<T, ()> {
    timeout_at(
        deadline.min(Instant::now() + Duration::from_secs(5)),
        future,
    )
    .await
    .map_err(|_| ())
}
fn transport_closed(error: &Error) -> bool {
    matches!(
        error,
        Error::ConnectionClosed
            | Error::AlreadyClosed
            | Error::Protocol(ProtocolError::ResetWithoutClosingHandshake)
    ) || matches!(error, Error::Io(error) if matches!(error.kind(),
            std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset))
}
fn held_head(bytes: &[u8]) -> bool {
    bytes == HEAD
}

pub(super) async fn run(endpoint: String, original_remaining_ms: u64) -> Result<(), ()> {
    let start = Instant::now();
    if original_remaining_ms == 0 || original_remaining_ms > 300_000 {
        return Err(());
    }
    let original = start + Duration::from_millis(original_remaining_ms);
    let end = original.min(start + Duration::from_secs(15));
    let close_end = original.min(start + Duration::from_secs(10));
    let url = url::Url::parse(&endpoint).map_err(|_| ())?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    let client = Client::new(
        &endpoint,
        Some(Tokens {
            access_token: TOKEN.into(),
            refresh_token: "synthetic-route-refresh-token".into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
        }),
    )
    .map_err(|_| ())?;
    let ClusterReply::Overview { overview } = bounded(
        close_end,
        client.cluster_operation(HOST, &ClusterOperation::Overview { refresh: false }),
    )
    .await?
    .map_err(|_| ())?
    else {
        return Err(());
    };
    if overview.state_unreadable
        || overview.degraded
        || overview.jobs.len() != 2
        || overview
            .jobs
            .iter()
            .filter(|job| job.id == BATCH && !job.attached)
            .count()
            != 1
        || overview
            .jobs
            .iter()
            .filter(|job| job.id == "j-00000002" && job.attached)
            .count()
            != 1
        || !overview.workspaces.is_empty()
        || overview
            .jobs
            .iter()
            .any(|job| job.state != ClusterJobState::Running || job.slurm_job_id.is_none())
    {
        return Err(());
    }
    let mut routes = overview
        .routes
        .iter()
        .filter(|route| route.job_id == BATCH && route.workspace_id.is_none());
    let route = routes.next().ok_or(())?;
    if routes.next().is_some()
        || route.daemon.token.is_empty()
        || route.daemon.token.len() > 4096
        || !route
            .daemon
            .token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(());
    }
    let mut socket = bounded(close_end, client.cluster_tcp(HOST, BATCH, None))
        .await?
        .map_err(|_| ())?;
    let request = Zeroizing::new(format!("GET /api/v1/fixture-revoke HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nConnection: keep-alive\r\n\r\n", route.daemon.token).into_bytes());
    bounded(
        close_end,
        socket.send(Message::Binary(request.to_vec().into())),
    )
    .await?
    .map_err(|_| ())?;
    let mut response = Zeroizing::new(Vec::with_capacity(HEAD.len()));
    while response.len() < HEAD.len() {
        match bounded(close_end, socket.next()).await? {
            Some(Ok(Message::Binary(bytes))) if bytes.len() <= HEAD.len() - response.len() => {
                response.extend_from_slice(&bytes)
            }
            Some(Ok(Message::Ping(bytes))) if bytes.len() <= 125 => {
                bounded(close_end, socket.send(Message::Pong(bytes)))
                    .await?
                    .map_err(|_| ())?;
            }
            Some(Ok(Message::Pong(bytes))) if bytes.len() <= 125 => (),
            _ => return Err(()),
        }
    }
    if !held_head(&response) {
        return Err(());
    }
    // The upstream fixture retains this exact authenticated socket for 15s.
    // Prove it has not already closed before publishing the revoke handoff.
    match timeout_at(
        close_end.min(Instant::now() + Duration::from_millis(200)),
        socket.next(),
    )
    .await
    {
        Err(_) => (),
        _ => return Err(()),
    }
    println!("C2_LINK_OPENED");
    std::io::stdout().flush().map_err(|_| ())?;
    loop {
        // No per-read reseed: timeout cannot count as transport closure.
        match timeout_at(close_end, socket.next()).await.map_err(|_| ())? {
            None | Some(Ok(Message::Close(_))) => break,
            Some(Err(error)) if transport_closed(&error) => break,
            Some(Ok(Message::Ping(bytes))) if bytes.len() <= 125 => {
                bounded(close_end, socket.send(Message::Pong(bytes)))
                    .await?
                    .map_err(|_| ())?;
            }
            Some(Ok(Message::Pong(bytes))) if bytes.len() <= 125 => (),
            _ => return Err(()),
        }
    }
    drop(socket); // Original transport ends before any post-revocation probes.
    for (path, upgrade) in [
        ("/v1/hosts/fixture-host/jobs/j-00000001/tcp", true),
        ("/v1/hosts/fixture-host/reconnect", false),
        ("/v1/hosts/fixture-host/ssh/auth/route-grants", false),
    ] {
        refused_request(url.port().ok_or(())?, path, upgrade, end).await?;
    }
    println!("C2_LINK_REVOKED");
    Ok(())
}

// Literal localhost only, no proxy/resolver/client background driver. This
// owns each actual TCP socket through bounded writes/header read and drop.
async fn refused_request(
    port: u16,
    path: &str,
    upgrade: bool,
    deadline: Instant,
) -> Result<(), ()> {
    let mut socket = bounded(
        deadline,
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)),
    )
    .await?
    .map_err(|_| ())?;
    let request = Zeroizing::new(if upgrade {
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: MDEyMzQ1Njc4OWFiY2RlZg==\r\n\r\n")
    } else {
        format!("POST {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}")
    });
    bounded(deadline, socket.write_all(request.as_bytes()))
        .await?
        .map_err(|_| ())?;
    let mut buffer = Zeroizing::new([0u8; 4096]);
    let mut length = 0;
    while length < buffer.len() {
        let n = bounded(deadline, socket.read(&mut buffer[length..]))
            .await?
            .map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        length += n;
        if buffer[..length]
            .windows(4)
            .any(|bytes| bytes == b"\r\n\r\n")
        {
            return if buffer[..length].starts_with(b"HTTP/1.1 401 Unauthorized\r\n") {
                Ok(())
            } else {
                Err(())
            };
        }
    }
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fresh_fixed_request_requires_actual_unauthorized_status() {
        for (status, refused) in [("401 Unauthorized", true), ("200 OK", false)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = Zeroizing::new([0u8; 4096]);
                let mut length = 0;
                loop {
                    let count = socket.read(&mut buffer[length..]).await.unwrap();
                    assert!(count > 0);
                    length += count;
                    if buffer[..length].ends_with(b"\r\n\r\n{}") {
                        break;
                    }
                    assert!(length < buffer.len());
                }
                assert!(buffer[..length]
                    .starts_with(b"POST /v1/hosts/fixture-host/reconnect HTTP/1.1\r\n"));
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });
            let deadline = Instant::now() + Duration::from_secs(1);
            assert_eq!(
                refused_request(port, "/v1/hosts/fixture-host/reconnect", false, deadline)
                    .await
                    .is_ok(),
                refused
            );
            timeout_at(deadline, server).await.unwrap().unwrap();
        }
    }
    #[test]
    fn held_response_and_transport_end_are_exact() {
        assert!(held_head(HEAD));
        assert!(!held_head(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ));
        assert!(!held_head(&[HEAD, b"x"].concat()));
        assert!(transport_closed(&Error::ConnectionClosed));
        assert!(transport_closed(&Error::Protocol(
            ProtocolError::ResetWithoutClosingHandshake
        )));
        assert!(!transport_closed(&Error::Io(
            std::io::ErrorKind::TimedOut.into()
        )));
        assert!(!transport_closed(&Error::Io(
            std::io::ErrorKind::WouldBlock.into()
        )));
        assert!(!transport_closed(&Error::Protocol(
            ProtocolError::InvalidOpcode(3)
        )));
    }
}
