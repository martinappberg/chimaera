use anyhow::{bail, Result};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;
pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub(crate) fn endpoint(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("endpoint must not include credentials, query, or fragment");
    }
    if url.path() != "/" && !url.path().is_empty() {
        bail!("endpoint must be an origin");
    }
    match (url.scheme(), url.host_str()) {
        ("https", Some(_)) | ("http", Some("127.0.0.1")) => Ok(url),
        _ => bail!("TLS is required except on 127.0.0.1"),
    }
}
pub(crate) fn path(base: &Url, segments: &[&str]) -> Url {
    let mut url = base.clone();
    url.path_segments_mut()
        .expect("validated HTTP origin")
        .clear()
        .extend(segments);
    url
}
