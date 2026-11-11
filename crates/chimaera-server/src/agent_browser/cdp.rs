//! A minimal Chrome DevTools Protocol client over `--remote-debugging-pipe`.
//!
//! The browser reads commands on its fd 3 and writes responses and events on
//! its fd 4, each message one JSON object terminated by a NUL byte. There is
//! no port: nothing else on the host can reach the browser.
//!
//! One reader task routes responses to their callers by `id` and hands
//! events to a synchronous callback (it must not block: it runs on the
//! reader task). One writer task owns the write half. When the pipe closes
//! (the browser exited or was killed) every pending call fails with
//! [`CdpError::Gone`] at once and the close callback runs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::state::lock;

/// One message from the browser. A full-page screenshot is the biggest
/// thing we ask for (capped at 8000 CSS px tall), well under this.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;
/// Commands queued for the writer. Callers wait (under their timeout) when
/// it is full; fire-and-forget commands are dropped instead.
const WRITE_QUEUE: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum CdpError {
    /// The pipe closed: the browser is gone.
    Gone(String),
    Timeout,
    /// The browser answered with an error.
    Protocol {
        code: i64,
        message: String,
    },
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CdpError::Gone(why) => write!(f, "the browser is gone ({why})"),
            CdpError::Timeout => f.write_str("the browser did not answer in time"),
            CdpError::Protocol { message, .. } => f.write_str(message),
        }
    }
}

/// An event: `session` is the flattened target session it belongs to, or
/// `None` for the browser itself.
#[derive(Debug)]
pub(super) struct Event {
    pub(super) method: String,
    pub(super) params: Value,
    pub(super) session: Option<String>,
}

type Reply = oneshot::Sender<Result<Value, CdpError>>;

pub(super) struct Conn {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Reply>>,
    tx: mpsc::Sender<Vec<u8>>,
    closed: AtomicBool,
    close_reason: Mutex<Option<String>>,
}

impl Conn {
    /// Starts the reader and writer tasks over the browser's pipe ends.
    /// `on_event` runs on the reader task for every event; `on_close` once,
    /// after pending calls have been failed.
    pub(super) fn start<R, W>(
        reader: R,
        writer: W,
        on_event: impl FnMut(Event) + Send + 'static,
        on_close: impl FnOnce(String) + Send + 'static,
    ) -> Arc<Conn>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, rx) = mpsc::channel(WRITE_QUEUE);
        let conn = Arc::new(Conn {
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            tx,
            closed: AtomicBool::new(false),
            close_reason: Mutex::new(None),
        });
        tokio::spawn(write_loop(writer, rx, Arc::downgrade(&conn)));
        tokio::spawn(read_loop(reader, Arc::clone(&conn), on_event, on_close));
        conn
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn gone(&self) -> CdpError {
        CdpError::Gone(
            lock(&self.close_reason)
                .clone()
                .unwrap_or_else(|| "pipe closed".to_string()),
        )
    }

    /// Sends one command and waits for its answer (at most `timeout`).
    pub(super) async fn call(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Value, CdpError> {
        if self.is_closed() {
            return Err(self.gone());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        lock(&self.pending).insert(id, reply);
        // Re-check after registering: a close between the first check and
        // the insert has already drained `pending` and would leave this
        // call waiting for its whole timeout.
        if self.is_closed() {
            lock(&self.pending).remove(&id);
            return Err(self.gone());
        }
        let bytes = encode(id, method, params, session);
        let outcome = tokio::time::timeout(timeout, async {
            if self.tx.send(bytes).await.is_err() {
                return Err(self.gone());
            }
            answer.await.unwrap_or_else(|_| Err(self.gone()))
        })
        .await;
        match outcome {
            Ok(result) => result,
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(CdpError::Timeout)
            }
        }
    }

    /// Sends a command without waiting; its answer is discarded. Used from
    /// the event path (dialogs), which must never wait.
    pub(super) fn fire(&self, method: &str, params: Value, session: Option<&str>) {
        if self.is_closed() {
            return;
        }
        // An id nobody registered: the reader drops its answer.
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if self
            .tx
            .try_send(encode(id, method, params, session))
            .is_err()
        {
            tracing::debug!("cdp: write queue full, dropped {method}");
        }
    }

    fn close(&self, reason: String) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        *lock(&self.close_reason) = Some(reason.clone());
        let drained: Vec<Reply> = lock(&self.pending).drain().map(|(_, r)| r).collect();
        for reply in drained {
            let _ = reply.send(Err(CdpError::Gone(reason.clone())));
        }
    }
}

fn encode(id: u64, method: &str, params: Value, session: Option<&str>) -> Vec<u8> {
    let mut msg = json!({"id": id, "method": method, "params": params});
    if let Some(s) = session {
        msg["sessionId"] = Value::String(s.to_string());
    }
    let mut bytes = serde_json::to_vec(&msg).unwrap_or_default();
    bytes.push(0);
    bytes
}

async fn write_loop<W>(mut writer: W, mut rx: mpsc::Receiver<Vec<u8>>, conn: std::sync::Weak<Conn>)
where
    W: tokio::io::AsyncWrite + Unpin,
{
    while let Some(bytes) = rx.recv().await {
        if let Err(e) = writer.write_all(&bytes).await {
            if let Some(conn) = conn.upgrade() {
                conn.close(format!("write failed: {e}"));
            }
            return;
        }
    }
}

async fn read_loop<R>(
    mut reader: R,
    conn: Arc<Conn>,
    mut on_event: impl FnMut(Event),
    on_close: impl FnOnce(String),
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    // Set while skipping an oversized message up to its terminator.
    let mut discarding = false;
    let reason = loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) => break "the browser closed its pipe".to_string(),
            Ok(n) => n,
            Err(e) => break format!("pipe read failed: {e}"),
        };
        let mut data = &chunk[..n];
        while let Some(pos) = data.iter().position(|b| *b == 0) {
            if discarding {
                discarding = false;
            } else {
                buf.extend_from_slice(&data[..pos]);
                dispatch(&conn, &buf, &mut on_event);
            }
            buf.clear();
            data = &data[pos + 1..];
        }
        if !discarding {
            buf.extend_from_slice(data);
            if buf.len() > MAX_MESSAGE {
                // The answer's caller times out; the stream stays in sync.
                tracing::warn!("cdp: dropped a message over {MAX_MESSAGE} bytes");
                buf = Vec::with_capacity(64 * 1024);
                discarding = true;
            }
        }
        // Don't keep a screenshot-sized allocation around between messages.
        if buf.is_empty() && buf.capacity() > 1024 * 1024 {
            buf = Vec::with_capacity(64 * 1024);
        }
    };
    conn.close(reason.clone());
    on_close(reason);
}

fn dispatch(conn: &Conn, raw: &[u8], on_event: &mut impl FnMut(Event)) {
    let Ok(mut msg) = serde_json::from_slice::<Value>(raw) else {
        tracing::debug!("cdp: unparseable message ({} bytes)", raw.len());
        return;
    };
    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
        let Some(reply) = lock(&conn.pending).remove(&id) else {
            return;
        };
        let result = match msg.get_mut("error") {
            Some(err) => Err(CdpError::Protocol {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string(),
            }),
            None => Ok(msg
                .get_mut("result")
                .map(Value::take)
                .unwrap_or(Value::Null)),
        };
        let _ = reply.send(result);
        return;
    }
    let Some(method) = msg
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let session = msg
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let params = msg
        .get_mut("params")
        .map(Value::take)
        .unwrap_or(Value::Null);
    on_event(Event {
        method,
        params,
        session,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    /// A fake browser on the other end of two in-memory pipes.
    struct Peer {
        rx: tokio::io::DuplexStream,
        tx: tokio::io::DuplexStream,
    }

    impl Peer {
        async fn next(&mut self) -> Value {
            let mut out = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                self.rx.read_exact(&mut byte).await.unwrap();
                if byte[0] == 0 {
                    return serde_json::from_slice(&out).unwrap();
                }
                out.push(byte[0]);
            }
        }
        async fn send(&mut self, v: Value) {
            let mut bytes = serde_json::to_vec(&v).unwrap();
            bytes.push(0);
            self.tx.write_all(&bytes).await.unwrap();
        }
    }

    fn pair() -> (
        Arc<Conn>,
        Peer,
        mpsc::UnboundedReceiver<Event>,
        oneshot::Receiver<String>,
    ) {
        let (our_w, peer_r) = duplex(1 << 20);
        let (peer_w, our_r) = duplex(1 << 20);
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (close_tx, close_rx) = oneshot::channel();
        let conn = Conn::start(
            our_r,
            our_w,
            move |e| {
                let _ = ev_tx.send(e);
            },
            move |why| {
                let _ = close_tx.send(why);
            },
        );
        (
            conn,
            Peer {
                rx: peer_r,
                tx: peer_w,
            },
            ev_rx,
            close_rx,
        )
    }

    #[tokio::test]
    async fn routes_answers_errors_and_events() {
        let (conn, mut peer, mut events, _close) = pair();
        let c = Arc::clone(&conn);
        let call = tokio::spawn(async move {
            c.call(
                "Page.navigate",
                json!({"url": "about:blank"}),
                Some("S1"),
                Duration::from_secs(5),
            )
            .await
        });
        let msg = peer.next().await;
        assert_eq!(msg["method"], "Page.navigate");
        assert_eq!(msg["sessionId"], "S1");
        assert_eq!(msg["params"]["url"], "about:blank");
        // An event arrives before the answer; two messages in one write.
        let id = msg["id"].clone();
        let mut bytes = serde_json::to_vec(&json!({"method": "Page.loadEventFired", "params": {"timestamp": 1}, "sessionId": "S1"})).unwrap();
        bytes.push(0);
        bytes.extend(serde_json::to_vec(&json!({"id": id, "result": {"frameId": "F"}})).unwrap());
        bytes.push(0);
        peer.tx.write_all(&bytes).await.unwrap();
        assert_eq!(call.await.unwrap().unwrap(), json!({"frameId": "F"}));
        let ev = events.recv().await.unwrap();
        assert_eq!(ev.method, "Page.loadEventFired");
        assert_eq!(ev.session.as_deref(), Some("S1"));

        let c = Arc::clone(&conn);
        let call = tokio::spawn(async move {
            c.call("Bogus.method", json!({}), None, Duration::from_secs(5))
                .await
        });
        let msg = peer.next().await;
        assert!(msg.get("sessionId").is_none());
        peer.send(json!({"id": msg["id"], "error": {"code": -32601, "message": "'Bogus.method' wasn't found"}}))
            .await;
        assert_eq!(
            call.await.unwrap(),
            Err(CdpError::Protocol {
                code: -32601,
                message: "'Bogus.method' wasn't found".into()
            })
        );
    }

    #[tokio::test]
    async fn times_out_and_forgets_the_call() {
        let (conn, mut peer, _events, _close) = pair();
        let r = conn
            .call("Page.enable", json!({}), None, Duration::from_millis(50))
            .await;
        assert_eq!(r, Err(CdpError::Timeout));
        let msg = peer.next().await;
        assert!(lock(&conn.pending).is_empty());
        // A late answer for a forgotten call is dropped quietly.
        peer.send(json!({"id": msg["id"], "result": {}})).await;
        conn.fire("Page.enable", json!({}), None);
        assert_eq!(peer.next().await["method"], "Page.enable");
    }

    #[tokio::test]
    async fn a_closed_pipe_fails_pending_calls_at_once() {
        let (conn, mut peer, _events, close) = pair();
        let c = Arc::clone(&conn);
        let call = tokio::spawn(async move {
            c.call("Page.enable", json!({}), None, Duration::from_secs(30))
                .await
        });
        let _ = peer.next().await;
        drop(peer);
        let started = std::time::Instant::now();
        assert!(matches!(call.await.unwrap(), Err(CdpError::Gone(_))));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(close.await.is_ok());
        assert!(conn.is_closed());
        assert!(matches!(
            conn.call("Page.enable", json!({}), None, Duration::from_secs(1))
                .await,
            Err(CdpError::Gone(_))
        ));
    }
}
