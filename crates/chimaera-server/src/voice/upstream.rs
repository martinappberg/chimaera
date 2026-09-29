//! The outbound half of dictation: one WebSocket to the speech service.
//!
//! TCP (or an `https_proxy` CONNECT tunnel — HPC sites route egress through
//! one, and claude itself honors the same variables), then rustls with the
//! host's trust roots, then the WebSocket handshake. Every step shares one
//! deadline so a wedged proxy or a silent firewall costs a bounded wait.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, Uri};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

pub(crate) type Stream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Connect + proxy + TLS + upgrade, together.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The service sends transcripts (small JSON); anything bigger is not it.
const MAX_INBOUND_MESSAGE: usize = 256 * 1024;
/// A proxy's CONNECT reply header block.
const MAX_PROXY_REPLY: usize = 16 * 1024;

/// Why an upgrade failed, in the terms the relay reports.
#[derive(Debug)]
pub(crate) enum ConnectError {
    /// The service answered the upgrade with an HTTP status (401/403 = the
    /// login was refused).
    Rejected(u16),
    /// Anything before an HTTP answer: DNS, TCP, proxy, TLS, timeout.
    Network(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Rejected(status) => write!(
                f,
                "the speech service refused the connection (HTTP {status})"
            ),
            ConnectError::Network(why) => write!(f, "could not reach the speech service: {why}"),
        }
    }
}

/// Open the WebSocket at `url` (ws:// or wss://) with `headers`.
pub(crate) async fn connect(
    url: &str,
    headers: &[(&'static str, String)],
) -> Result<Stream, ConnectError> {
    match tokio::time::timeout(CONNECT_TIMEOUT, connect_inner(url, headers)).await {
        Ok(result) => result,
        Err(_) => Err(ConnectError::Network("timed out".to_string())),
    }
}

async fn connect_inner(
    url: &str,
    headers: &[(&'static str, String)],
) -> Result<Stream, ConnectError> {
    let mut request = url
        .into_client_request()
        .map_err(|e| ConnectError::Network(format!("bad url: {e}")))?;
    for (name, value) in headers {
        let value = HeaderValue::from_str(value)
            .map_err(|_| ConnectError::Network(format!("header {name} is not valid")))?;
        request.headers_mut().insert(*name, value);
    }
    let uri = request.uri().clone();
    let (host, port, tls) = endpoint(&uri).map_err(|e| ConnectError::Network(e.to_string()))?;

    let tcp = match proxy_for(&host, tls) {
        Some(proxy) => tunnel(&proxy, &host, port).await,
        None => TcpStream::connect((host.as_str(), port))
            .await
            .with_context(|| format!("connect {host}:{port}")),
    }
    .map_err(|e| ConnectError::Network(format!("{e:#}")))?;
    let _ = tcp.set_nodelay(true);

    let connector = if tls {
        Connector::Rustls(
            tls_config()
                .await
                .map_err(|e| ConnectError::Network(format!("{e:#}")))?,
        )
    } else {
        Connector::Plain
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_INBOUND_MESSAGE))
        .max_frame_size(Some(MAX_INBOUND_MESSAGE));
    match tokio_tungstenite::client_async_tls_with_config(
        request,
        tcp,
        Some(config),
        Some(connector),
    )
    .await
    {
        Ok((stream, _response)) => Ok(stream),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(ConnectError::Rejected(response.status().as_u16()))
        }
        Err(e) => Err(ConnectError::Network(e.to_string())),
    }
}

fn endpoint(uri: &Uri) -> anyhow::Result<(String, u16, bool)> {
    let tls = match uri.scheme_str() {
        Some("wss") => true,
        Some("ws") => false,
        other => bail!("unsupported scheme {other:?}"),
    };
    let host = uri.host().context("url has no host")?;
    // `Uri::host` keeps an IPv6 literal's brackets; sockets and SNI want it bare.
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    Ok((host, port, tls))
}

/// The host's trust roots, loaded once (a file walk — kept off the reactor).
/// `SSL_CERT_FILE` / `SSL_CERT_DIR` are honored the way curl honors them.
async fn tls_config() -> anyhow::Result<Arc<rustls::ClientConfig>> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config.clone());
    }
    let config = tokio::task::spawn_blocking(|| -> anyhow::Result<Arc<rustls::ClientConfig>> {
        let found = rustls_native_certs::load_native_certs();
        let mut roots = rustls::RootCertStore::empty();
        let (added, _ignored) = roots.add_parsable_certificates(found.certs);
        if added == 0 {
            bail!("no trusted root certificates found on this host");
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("TLS setup")?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Arc::new(config))
    })
    .await
    .context("TLS setup")??;
    Ok(CONFIG.get_or_init(|| config).clone())
}

/// An `http://[user:pass@]host[:port]` forward proxy.
#[derive(Debug, PartialEq)]
struct Proxy {
    host: String,
    port: u16,
    /// `user:pass`, already percent-decoded.
    credentials: Option<String>,
}

/// The proxy the environment names for `host`, if any. `NO_PROXY` wins.
fn proxy_for(host: &str, tls: bool) -> Option<Proxy> {
    let var = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| std::env::var(n).ok().filter(|v| !v.trim().is_empty()))
    };
    let no_proxy = var(&["NO_PROXY", "no_proxy"]).unwrap_or_default();
    if bypasses(&no_proxy, host) {
        return None;
    }
    let raw = if tls {
        var(&["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"])
    } else {
        var(&["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"])
    }?;
    parse_proxy(&raw)
}

fn bypasses(no_proxy: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    no_proxy
        .split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .any(|entry| {
            let entry = entry.to_ascii_lowercase();
            let entry = entry.split(':').next().unwrap_or("");
            let suffix = entry.trim_start_matches("*.").trim_start_matches('.');
            entry == "*" || host == suffix || host.ends_with(&format!(".{suffix}"))
        })
}

fn parse_proxy(raw: &str) -> Option<Proxy> {
    let raw = raw.trim();
    // A bare `host:port` is conventionally an http proxy; an https:// or
    // socks:// one is a transport this relay doesn't speak.
    let rest = match raw.split_once("://") {
        Some(("http", rest)) => rest,
        Some(_) => return None,
        None => raw,
    };
    let authority = rest.split('/').next().unwrap_or("");
    let (credentials, hostport) = match authority.rsplit_once('@') {
        Some((userinfo, hostport)) => (Some(percent_decode(userinfo)), hostport),
        None => (None, authority),
    };
    let (host, port) = if let Some(bracketed) = hostport.strip_prefix('[') {
        let (host, after) = bracketed.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(port) => port.parse().ok()?,
            None => 80,
        };
        (host, port)
    } else {
        match hostport.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (hostport, 80),
        }
    };
    if host.is_empty() {
        return None;
    }
    Some(Proxy {
        host: host.to_string(),
        port,
        credentials,
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A CONNECT tunnel through `proxy` to `host:port`.
async fn tunnel(proxy: &Proxy, host: &str, port: u16) -> anyhow::Result<TcpStream> {
    let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port))
        .await
        .with_context(|| format!("connect proxy {}:{}", proxy.host, proxy.port))?;
    let target = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut head = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(credentials) = &proxy.credentials {
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        head.push_str(&format!("Proxy-Authorization: Basic {encoded}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .await
        .context("proxy CONNECT")?;

    // Read byte-by-byte to the blank line: anything past it belongs to the
    // TLS stream, so the reply must not be over-read.
    let mut reply = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !reply.ends_with(b"\r\n\r\n") {
        if reply.len() >= MAX_PROXY_REPLY {
            bail!("proxy reply too long");
        }
        let n = stream
            .read(&mut byte)
            .await
            .context("proxy CONNECT reply")?;
        if n == 0 {
            bail!("proxy closed the connection");
        }
        reply.push(byte[0]);
    }
    let status_line = String::from_utf8_lossy(&reply);
    let status_line = status_line.lines().next().unwrap_or("");
    let status = status_line.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        return Err(anyhow!("proxy refused the tunnel ({})", status_line.trim()));
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_urls() {
        assert_eq!(
            parse_proxy("http://proxy.example:3128"),
            Some(Proxy {
                host: "proxy.example".into(),
                port: 3128,
                credentials: None
            })
        );
        assert_eq!(
            parse_proxy("proxy.example:8080/"),
            Some(Proxy {
                host: "proxy.example".into(),
                port: 8080,
                credentials: None
            })
        );
        assert_eq!(
            parse_proxy("http://u%40x:p%3Aw@10.0.0.1:3128"),
            Some(Proxy {
                host: "10.0.0.1".into(),
                port: 3128,
                credentials: Some("u@x:p:w".into())
            })
        );
        assert_eq!(
            parse_proxy("http://proxy.example").map(|p| p.port),
            Some(80)
        );
        assert_eq!(parse_proxy("socks5://proxy.example:1080"), None);
        assert_eq!(parse_proxy("https://proxy.example:443"), None);
        assert_eq!(parse_proxy("http://"), None);
    }

    #[test]
    fn no_proxy_matching() {
        assert!(bypasses("localhost,.anthropic.com", "api.anthropic.com"));
        assert!(bypasses("anthropic.com", "api.anthropic.com"));
        assert!(bypasses("*.anthropic.com", "api.anthropic.com"));
        assert!(bypasses("*", "api.anthropic.com"));
        assert!(bypasses("API.anthropic.com:443", "api.anthropic.com"));
        assert!(!bypasses("", "api.anthropic.com"));
        assert!(!bypasses(
            "example.com, notanthropic.com",
            "api.anthropic.com"
        ));
        assert!(!bypasses("thropic.com", "api.anthropic.com"));
    }

    #[test]
    fn endpoints() {
        let uri: Uri = "wss://api.anthropic.com/api/ws/x?a=1".parse().unwrap();
        assert_eq!(
            endpoint(&uri).unwrap(),
            ("api.anthropic.com".into(), 443, true)
        );
        let uri: Uri = "ws://127.0.0.1:9123/x".parse().unwrap();
        assert_eq!(endpoint(&uri).unwrap(), ("127.0.0.1".into(), 9123, false));
        let uri: Uri = "ws://[::1]:9123/x".parse().unwrap();
        assert_eq!(endpoint(&uri).unwrap(), ("::1".into(), 9123, false));
    }
}
