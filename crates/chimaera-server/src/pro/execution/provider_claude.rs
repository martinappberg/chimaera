//! Private loopback Claude frontend. No daemon route, upstream URL or OAuth
//! credential is exposed; each request owns its fixed Unix stream through EOF.
use super::provider_client::{ChildLifetime, Owner};
use axum::{
    body::Body,
    http::{Request, Response},
};
use chimaera_core::provider_runtime as wire;
use http_body_util::BodyExt;
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    convert::Infallible,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, Semaphore},
    task::{JoinHandle, JoinSet},
};
use zeroize::Zeroizing;

const HEADERS: usize = 16 * 1024;
const HEADER_TIME: Duration = Duration::from_secs(5);
const UPLOAD_TIME: Duration = Duration::from_secs(30);
const REQUEST_TIME: Duration = Duration::from_secs(600);

/// Not Clone/Debug. Only a trusted fixed child receives this frontend token.
pub(super) struct Frontend {
    address: String,
    token: Arc<Zeroizing<String>>,
    child: Arc<ChildLifetime>,
    task: Option<JoinHandle<()>>,
}
impl Frontend {
    pub(super) async fn start(child: Arc<ChildLifetime>) -> Result<Self, wire::Error> {
        Self::start_at(child, REQUEST_TIME, UPLOAD_TIME).await
    }
    async fn start_at(
        child: Arc<ChildLifetime>,
        request_time: Duration,
        upload_time: Duration,
    ) -> Result<Self, wire::Error> {
        child.current()?;
        let listener = child
            .wait(TcpListener::bind("127.0.0.1:0"))
            .await?
            .map_err(|_| wire::Error::Unavailable)?;
        let address = listener
            .local_addr()
            .map_err(|_| wire::Error::Unavailable)?
            .to_string();
        let token = Arc::new(Zeroizing::new(chimaera_core::generate_token()));
        let state = Arc::new(FrontendState {
            child: child.clone(),
            token: token.clone(),
            host: address.clone(),
            broken: AtomicBool::new(false),
            request_time,
            upload_time,
            connections: Arc::new(Semaphore::new(16)),
        });
        child.current()?;
        let task = tokio::spawn(listen(listener, state));
        Ok(Self {
            address,
            token,
            child,
            task: Some(task),
        })
    }
    pub(super) fn url(&self) -> String {
        format!("http://{}", self.address)
    }
    pub(super) fn token(&self) -> &str {
        self.token.as_str()
    }
    pub(super) async fn stop(mut self) {
        self.child.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for Frontend {
    fn drop(&mut self) {
        // JoinHandle drop detaches the retained listener continuation; it still
        // owns all connections until closure, instead of aborting their cleanup.
        self.child.cancel();
    }
}
struct FrontendState {
    child: Arc<ChildLifetime>,
    token: Arc<Zeroizing<String>>,
    host: String,
    broken: AtomicBool,
    request_time: Duration,
    upload_time: Duration,
    connections: Arc<Semaphore>,
}
async fn listen(listener: TcpListener, state: Arc<FrontendState>) {
    let mut tasks = JoinSet::new();
    let mut cancelled = state.child.cancellation();
    loop {
        tokio::select! {
            biased;
            _ = cancelled.wait_for(|v| *v) => break,
            _ = tokio::time::sleep_until(state.child.deadline().into()) => break,
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = listener.accept() => {
                let Ok((socket, _)) = accepted else { break; };
                let received=Instant::now();
                let Ok(permit) = state.connections.clone().try_acquire_owned() else { drop(socket); continue; };
                let state = state.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let Ok(socket)=header_socket(&state.child,socket,received).await else {return;};
                    // The last owner cannot settle before the HTTP socket closes.
                    let retained = Arc::new(Mutex::new(None::<Arc<Owner>>));
                    let cell = retained.clone();
                    let handler = state.clone();
                    let service = service_fn(move |request| handle(request, handler.clone(), cell.clone()));
                    let mut builder = http1::Builder::new();
                    builder.keep_alive(false).max_headers(32).max_buf_size(HEADERS)
                        .timer(TokioTimer::new()).header_read_timeout(HEADER_TIME);
                    let connection = builder.serve_connection(TokioIo::new(socket), service);
                    let _ = state.child.wait(connection).await;
                    if let Some(owner) = crate::lock(&retained).take() { drop(owner.observer()); }
                });
            }
        }
    }
    drop(listener);
    state.child.cancel();
    while tasks.join_next().await.is_some() {}
}
/// Hyper legitimately coalesces equal Content-Length headers. This closed
/// adapter rejects the raw duplicate before that normalization or any authority
/// effect. Prefix bytes are capped/zeroizing and replayed once into its parser.
struct PrefixSocket {
    socket: TcpStream,
    prefix: Zeroizing<Vec<u8>>,
    offset: usize,
}
impl AsyncRead for PrefixSocket {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.offset < self.prefix.len() {
            let n = buffer.remaining().min(self.prefix.len() - self.offset);
            buffer.put_slice(&self.prefix[self.offset..self.offset + n]);
            self.offset += n;
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut self.socket).poll_read(cx, buffer)
        }
    }
}
impl AsyncWrite for PrefixSocket {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.socket).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}
async fn header_socket(
    child: &ChildLifetime,
    mut socket: TcpStream,
    received: Instant,
) -> Result<PrefixSocket, wire::Error> {
    let work = async {
        if Instant::now() >= received + HEADER_TIME {
            return Err(wire::Error::Unavailable);
        }
        let mut bytes = Zeroizing::new(vec![0; HEADERS]);
        let mut n = 0;
        loop {
            let count = child
                .wait(socket.read(&mut bytes[n..]))
                .await?
                .map_err(|_| wire::Error::Unavailable)?;
            if count == 0 {
                return Err(wire::Error::InvalidRequest);
            }
            n += count;
            if let Some(end) = bytes[..n].windows(4).position(|w| w == b"\r\n\r\n") {
                let header =
                    std::str::from_utf8(&bytes[..end]).map_err(|_| wire::Error::InvalidRequest)?;
                let mut length = 0;
                let mut auth = 0;
                let mut host = 0;
                let mut content = 0;
                let mut count = 0;
                for line in header.split("\r\n").skip(1) {
                    count += 1;
                    let (name, _) = line.split_once(':').ok_or(wire::Error::InvalidRequest)?;
                    if name.eq_ignore_ascii_case("content-length") {
                        length += 1;
                    }
                    if name.eq_ignore_ascii_case("authorization") {
                        auth += 1;
                    }
                    if name.eq_ignore_ascii_case("host") {
                        host += 1;
                    }
                    if name.eq_ignore_ascii_case("content-type") {
                        content += 1;
                    }
                    if name.eq_ignore_ascii_case("transfer-encoding")
                        || name.eq_ignore_ascii_case("origin")
                        || name.eq_ignore_ascii_case("x-api-key")
                    {
                        return Err(wire::Error::InvalidRequest);
                    }
                }
                if count > 32 || length != 1 || auth != 1 || host != 1 || content != 1 {
                    return Err(wire::Error::InvalidRequest);
                }
                if Instant::now() >= received + HEADER_TIME {
                    return Err(wire::Error::Unavailable);
                }
                bytes.truncate(n);
                return Ok(PrefixSocket {
                    socket,
                    prefix: bytes,
                    offset: 0,
                });
            }
            if n == HEADERS {
                return Err(wire::Error::LimitReached);
            }
        }
    };
    tokio::time::timeout_at((received + HEADER_TIME).into(), work)
        .await
        .map_err(|_| wire::Error::Unavailable)?
}
fn authorized(
    request: &Request<Incoming>,
    state: &FrontendState,
) -> Result<(wire::ClaudeRoute, u64), wire::Error> {
    if state.broken.load(Ordering::Acquire) {
        return Err(wire::Error::StateChanged);
    }
    state.child.current()?;
    let one = |name: &str| -> Result<&str, wire::Error> {
        let all = request.headers().get_all(name);
        if all.iter().count() != 1 {
            return Err(wire::Error::InvalidRequest);
        }
        all.iter()
            .next()
            .and_then(|v| v.to_str().ok())
            .ok_or(wire::Error::InvalidRequest)
    };
    if request.method() != "POST"
        || request.uri().scheme().is_some()
        || request.uri().authority().is_some()
        || one("host")? != state.host
        || request.headers().contains_key("transfer-encoding")
        || request.headers().contains_key("origin")
        || request.headers().contains_key("x-api-key")
        || request.headers().len() > 32
        || request
            .headers()
            .iter()
            .map(|(k, v)| k.as_str().len() + v.as_bytes().len() + 4)
            .sum::<usize>()
            > HEADERS
        || one("content-type")? != "application/json"
    {
        return Err(wire::Error::InvalidRequest);
    }
    let auth = one("authorization")?
        .strip_prefix("Bearer ")
        .ok_or(wire::Error::InvalidRequest)?;
    // Fixed-size opaque frontend equality; no canonical provider token is here.
    if auth.len() != state.token.len()
        || auth
            .bytes()
            .zip(state.token.bytes())
            .fold(0u8, |d, (a, b)| d | (a ^ b))
            != 0
    {
        return Err(wire::Error::InvalidRequest);
    }
    let route = match request.uri().path_and_query().map(|p| p.as_str()) {
        Some("/v1/messages?beta=true") => wire::ClaudeRoute::Messages,
        Some("/v1/messages/count_tokens?beta=true") => wire::ClaudeRoute::CountTokens,
        _ => return Err(wire::Error::InvalidRequest),
    };
    let length = one("content-length")?;
    if length.is_empty() || !length.bytes().all(|b| b.is_ascii_digit()) {
        return Err(wire::Error::InvalidRequest);
    }
    let length = length.parse().map_err(|_| wire::Error::InvalidRequest)?;
    wire::BodyCount::new(length)?;
    Ok((route, length))
}
fn refusal(error: wire::Error) -> Response<Body> {
    let status = match error {
        wire::Error::InvalidRequest => 400,
        wire::Error::NeedsSignIn => 401,
        wire::Error::LimitReached => 429,
        wire::Error::Inactive | wire::Error::Unsupported | wire::Error::StateChanged => 503,
        wire::Error::Unavailable => 502,
    };
    let body =
        wire::ClaudeErrorBody::for_status(status).and_then(|body| wire::encode_control(&body));
    let bytes = body
        .map(|bytes| bytes::Bytes::from_owner(bytes))
        .unwrap_or_default();
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("Connection", "close")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}
async fn handle(
    request: Request<Incoming>,
    state: Arc<FrontendState>,
    retained: Arc<Mutex<Option<Arc<Owner>>>>,
) -> Result<Response<Body>, Infallible> {
    let (route, length) = match authorized(&request, &state) {
        Ok(v) => v,
        Err(e) => return Ok(refusal(e)),
    };
    let started = Instant::now();
    let owner = match Owner::for_child(
        &state.child,
        wire::Command::ClaudeStream {
            route,
            content_length: length,
        },
        started + state.request_time,
    ) {
        Ok(owner) => Arc::new(owner),
        Err(e) => return Ok(refusal(e)),
    };
    *crate::lock(&retained) = Some(owner.clone());
    let observer = owner.observer();
    let (head_tx, head_rx) = oneshot::channel();
    let (body_tx, body_rx) = mpsc::channel::<Result<bytes::Bytes, wire::Error>>(1);
    let producer = owner.clone();
    let producer_state = state.clone();
    tokio::spawn(async move {
        let result = exchange(
            &producer,
            request.into_body(),
            length,
            started + producer_state.upload_time,
            head_tx,
            &body_tx,
        )
        .await;
        if let Err(error) = result {
            // Latch before publishing failure so SDK retries cause no new effects.
            producer_state.broken.store(true, Ordering::Release);
            let _ = body_tx.try_send(Err(error));
            producer_state.child.cancel();
        }
        drop(body_tx);
        drop(producer);
    });
    let head = match owner.wait(head_rx).await {
        Ok(Ok(Ok(head))) => head,
        Ok(Ok(Err(error))) => return Ok(refusal(error)),
        _ => return Ok(refusal(wire::Error::Unavailable)),
    };
    let stream = futures::stream::unfold(
        (body_rx, owner, observer),
        |(mut body, owner, observer)| async move {
            body.recv()
                .await
                .map(|frame| (frame, (body, owner, observer)))
        },
    );
    let content_type = match head.headers.content_type {
        wire::ContentType::Json => "application/json",
        wire::ContentType::EventStream => "text/event-stream",
    };
    let mut response = Response::builder()
        .status(head.status)
        .header("Content-Type", content_type)
        .header("Connection", "close");
    if let Some(retry) = head.headers.retry_after_seconds {
        response = response.header("Retry-After", retry.to_string());
    }
    Ok(response
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| refusal(wire::Error::Unavailable)))
}
async fn write_frame<W: AsyncWrite + Unpin>(
    owner: &Owner,
    writer: &mut W,
    kind: u8,
    bytes: &[u8],
) -> Result<(), wire::Error> {
    let mut header = [kind, 0, 0, 0, 0];
    header[1..].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    wire::FrameHeader::decode(&header)?.check_payload(bytes)?;
    owner
        .wait(writer.write_all(&header))
        .await?
        .map_err(|_| wire::Error::Unavailable)?;
    owner
        .wait(writer.write_all(bytes))
        .await?
        .map_err(|_| wire::Error::Unavailable)
}
async fn read_frame<R: AsyncRead + Unpin>(
    owner: &Owner,
    reader: &mut R,
    bytes: &mut [u8],
) -> Result<wire::FrameHeader, wire::Error> {
    let mut header = [0; 5];
    owner
        .wait(reader.read_exact(&mut header))
        .await?
        .map_err(|_| wire::Error::Unavailable)?;
    let header = wire::FrameHeader::decode(&header)?;
    if header.length > bytes.len() {
        return Err(wire::Error::InvalidRequest);
    }
    owner
        .wait(reader.read_exact(&mut bytes[..header.length]))
        .await?
        .map_err(|_| wire::Error::Unavailable)?;
    Ok(header)
}
async fn exchange(
    owner: &Owner,
    mut incoming: Incoming,
    declared: u64,
    upload_deadline: Instant,
    head: oneshot::Sender<Result<wire::ClaudeHead, wire::Error>>,
    output: &mpsc::Sender<Result<bytes::Bytes, wire::Error>>,
) -> Result<(), wire::Error> {
    if Instant::now() >= upload_deadline {
        return Err(wire::Error::Unavailable);
    }
    let request = owner.claim()?;
    let mut socket = owner.connect().await?;
    if Instant::now() >= upload_deadline {
        return Err(wire::Error::Unavailable);
    }
    write_frame(owner, &mut socket, 0, &wire::encode_control(request)?).await?;
    let (mut reader, mut writer) = socket.split();
    let upload = async {
        let mut count = wire::BodyCount::new(declared)?;
        let work = async {
            while let Some(frame) = owner.wait(incoming.frame()).await? {
                if Instant::now() >= upload_deadline {
                    return Err(wire::Error::Unavailable);
                }
                let frame = frame.map_err(|_| wire::Error::Unavailable)?;
                let data = frame.into_data().map_err(|_| wire::Error::InvalidRequest)?;
                for chunk in data.chunks(wire::DATA_MAX) {
                    if Instant::now() >= upload_deadline {
                        return Err(wire::Error::Unavailable);
                    }
                    count.add(chunk.len())?;
                    write_frame(owner, &mut writer, 1, chunk).await?;
                }
            }
            let bytes = count.complete()?;
            if Instant::now() >= upload_deadline {
                return Err(wire::Error::Unavailable);
            }
            let end = wire::StreamEnd {
                version: 1,
                binding: request.binding.clone(),
                request_id: request.request_id.clone(),
                bytes,
            };
            write_frame(owner, &mut writer, 2, &wire::encode_control(&end)?).await?;
            owner
                .wait(writer.shutdown())
                .await?
                .map_err(|_| wire::Error::Unavailable)
        };
        tokio::time::timeout_at(upload_deadline.into(), work)
            .await
            .map_err(|_| wire::Error::Unavailable)?
    };
    let download = async {
        let mut bytes = Zeroizing::new(vec![0; wire::CONTROL_MAX]);
        let frame = read_frame(owner, &mut reader, &mut bytes).await?;
        if frame.kind == wire::FrameKind::Error {
            let refusal: wire::Refusal = serde_json::from_slice(&bytes[..frame.length])
                .map_err(|_| wire::Error::InvalidRequest)?;
            refusal.validate(request)?;
            let _ = head.send(Err(refusal.error));
            return Err(refusal.error);
        }
        if frame.kind != wire::FrameKind::ResponseBegin {
            return Err(wire::Error::InvalidRequest);
        }
        let response = wire::Response::decode(&bytes[..frame.length], request)?;
        let wire::Reply::ClaudeHead { head: response } = response.result else {
            return Err(wire::Error::InvalidRequest);
        };
        let status = response.status;
        let mut delayed = Some((head, response));
        if status == 200 {
            let (head, response) = delayed.take().ok_or(wire::Error::StateChanged)?;
            head.send(Ok(response))
                .map_err(|_| wire::Error::StateChanged)?;
        }
        let mut total = 0u64;
        let mut error_body = Zeroizing::new(Vec::with_capacity(1024));
        loop {
            let frame = read_frame(owner, &mut reader, &mut bytes).await?;
            match frame.kind {
                wire::FrameKind::ResponseData => {
                    total = total
                        .checked_add(frame.length as u64)
                        .filter(|v| *v <= wire::BODY_MAX)
                        .ok_or(wire::Error::LimitReached)?;
                    if status == 200 {
                        let mut part = Zeroizing::new(Vec::with_capacity(wire::DATA_MAX));
                        part.extend_from_slice(&bytes[..frame.length]);
                        owner
                            .wait(output.send(Ok(bytes::Bytes::from_owner(part))))
                            .await?
                            .map_err(|_| wire::Error::StateChanged)?;
                    } else {
                        if error_body.len() + frame.length > 1024 {
                            return Err(wire::Error::InvalidRequest);
                        }
                        error_body.extend_from_slice(&bytes[..frame.length]);
                    }
                }
                wire::FrameKind::ResponseEnd => {
                    let end: wire::StreamEnd = serde_json::from_slice(&bytes[..frame.length])
                        .map_err(|_| wire::Error::InvalidRequest)?;
                    end.validate(request, total)?;
                    if owner
                        .wait(reader.read(&mut [0]))
                        .await?
                        .map_err(|_| wire::Error::Unavailable)?
                        != 0
                    {
                        return Err(wire::Error::InvalidRequest);
                    }
                    if let Some((head, response)) = delayed.take() {
                        let body: wire::ClaudeErrorBody = serde_json::from_slice(&error_body)
                            .map_err(|_| wire::Error::InvalidRequest)?;
                        body.validate(status)?;
                        let body =
                            wire::encode_control(&wire::ClaudeErrorBody::for_status(status)?)?;
                        head.send(Ok(response))
                            .map_err(|_| wire::Error::StateChanged)?;
                        owner
                            .wait(output.send(Ok(bytes::Bytes::from_owner(body))))
                            .await?
                            .map_err(|_| wire::Error::StateChanged)?;
                    }
                    return owner.current();
                }
                _ => return Err(wire::Error::InvalidRequest),
            }
        }
    };
    tokio::select! {
        biased;
        _=output.closed()=>Err(wire::Error::StateChanged),
        result=async { tokio::try_join!(upload,download).map(|_|()) }=>result,
    }
}

#[cfg(test)]
#[path = "provider_claude_tests.rs"]
mod tests;
