//! Whether this computer can be reached, and the daemon's own reverse link.
//!
//! One fact decides where work runs: whoever holds a project's lease runs it,
//! and this computer holds it while its daemon can reach the account. The
//! lease loop's own calls are that fact ([`answered`], [`unreachable`]); the
//! guard for bringing cloud work home is that they have succeeded without a
//! gap for a few seconds ([`settled`]), the only rule against bouncing.
//!
//! The same daemon keeps this computer reachable from the user's other
//! devices, so quitting the app changes nothing: [`start`] dials the keeper's
//! reverse-serve socket (`chimaera-link` PROTOCOL.md "Reverse serve") with the
//! daemon's own delegation and bridges each stream the keeper opens to this
//! daemon's loopback port. It runs only for a personal computer configured
//! for an active Pro account with the Runtime composed in; a free daemon never
//! dials. The delegation stays in memory (never logged, never on disk); remote
//! viewers still need this daemon's own bearer, registered with the keeper.
use crate::{lock, AppState};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

/// Seconds the account must have answered without a gap before live cloud
/// work comes home: three renewals, so one lucky request after a wake pulls
/// nothing home only to lose it again. Short on purpose: a return is cheap
/// (this computer has the files), and losing it again costs the cloud the
/// full lease-plus-grace path, so a flapping computer cannot bounce work
/// fast. A development build (the loopback harness) may
/// change it with `CHIMAERA_PRO_SETTLE_SECS` (at most five minutes); release
/// builds ignore the variable.
pub(super) fn guard_seconds() -> u64 {
    static GUARD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *GUARD.get_or_init(|| {
        guard_override(
            chimaera_core::is_dev_build(),
            std::env::var("CHIMAERA_PRO_SETTLE_SECS").ok().as_deref(),
        )
    })
}
fn guard_override(dev: bool, value: Option<&str>) -> u64 {
    const GUARD: u64 = 15;
    value
        .filter(|_| dev)
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(GUARD, |seconds| seconds.min(300))
}

/// A server error this recent means the account is up but failing: a computer
/// then keeps running its own work past its lease (`execution::expire`), since
/// nobody else can acquire through a failing account either.
const ERRORING_FOR: u64 = 30;

/// The lease loop got an HTTP answer. Reachable means a success or the
/// account's conflict (409, a quiet wait); a refused credential is not. A
/// server error says the account is up but failing (nobody can acquire
/// through it either, so a computer keeps its own work, `execution::expire`)
/// only when it carries the account's own marker (`from_account`): a 5xx
/// without it is a proxy, captive portal or edge in between, which proves
/// nothing and counts as unreachable. Successes do not need the marker: the
/// account is reached over TLS, and a curl too old to report a header
/// (before 7.84) must still see its successes.
pub(super) fn answered(state: &AppState, status: u16, from_account: bool) {
    let now = super::now();
    if status >= 500 && from_account {
        state.pro.erroring_at.store(now, Ordering::Release);
        state.pro.reachable_since.store(0, Ordering::Release);
    } else if (200..300).contains(&status) || status == 409 {
        let _ = state.pro.reachable_since.compare_exchange(
            0,
            now.max(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    } else {
        unreachable(state);
    }
}
/// The account could not be reached, or this computer slept or froze: the
/// guard starts over.
pub(super) fn unreachable(state: &AppState) {
    state.pro.reachable_since.store(0, Ordering::Release);
}
/// Reached without a gap for the guard.
pub(super) fn settled(state: &AppState) -> bool {
    let since = state.pro.reachable_since.load(Ordering::Acquire);
    since != 0 && super::now().saturating_sub(since) >= guard_seconds()
}
/// The account answered with a server error within the last half minute.
pub(super) fn erroring(state: &AppState) -> bool {
    let at = state.pro.erroring_at.load(Ordering::Acquire);
    at != 0 && super::now().saturating_sub(at) <= ERRORING_FOR
}

/// Streams the keeper may open at once, and the largest message either side
/// may send (the keeper's own limits).
const MAX_STREAMS: usize = 128;
const MAX_DATA_FRAME: usize = 64 * 1024;
const MAX_CONTROL_FRAME: usize = 128 * 1024;
/// Bytes read from this daemon and not yet written to the keeper, across all
/// streams of the link (KiB): a slow keeper holds the readers back instead of
/// queueing up to 16 frames per stream on each of 128 streams (review R3 S3).
const QUEUED_KIB: usize = 8 * 1024;
const PING_EVERY: Duration = Duration::from_secs(20);
const PONG_WITHIN: Duration = Duration::from_secs(60);

/// What the link needs from the configuration, or why it does not run.
#[derive(Clone)]
struct Target {
    url: String,
    bearer: String,
    alias: String,
}
fn target(state: &AppState) -> Option<Target> {
    if state.daemon_extension.is_none()
        || !state.pro.configured.load(Ordering::Acquire)
        || super::execution::worker(state)
    {
        return None;
    }
    let config = lock(&state.pro.runtime).clone()?;
    if config.role != super::protocol::Role::Device
        || config.delegation.workspace.is_some()
        || !config
            .delegation
            .scope
            .iter()
            .any(|scope| scope == "keeper")
        || config.keeper_url.is_empty()
    {
        return None;
    }
    let base = config.keeper_url.trim_end_matches('/');
    let url = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}/v1/serve")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}/v1/serve")
    } else {
        return None;
    };
    let alias = config
        .alias
        .as_deref()
        .map(|alias| {
            alias
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
        })
        .filter(|alias| !alias.is_empty() && alias.len() <= 256)
        .unwrap_or_else(|| "Computer".to_owned());
    Some(Target {
        url,
        bearer: config.delegation.access_token,
        alias,
    })
}

/// Starts (or restarts) the reverse link for the current configuration;
/// nothing runs when the configuration is not a personal Pro computer.
pub(super) fn start(state: &Arc<AppState>) {
    stop(state);
    if target(state).is_none() {
        return;
    }
    let generation = state.pro.generation.load(Ordering::Acquire);
    let weak = Arc::downgrade(state);
    let task = tokio::spawn(async move { run(weak, generation).await });
    *lock(&state.pro.link) = Some(task);
}
/// Ends the reverse link and every stream it carries (sign-out, a new
/// configuration, shutdown).
pub(super) fn stop(state: &AppState) {
    if let Some(task) = lock(&state.pro.link).take() {
        task.abort();
    }
}
/// Whether the reverse link task is running (tests and status).
#[cfg(test)]
pub(super) fn running(state: &AppState) -> bool {
    lock(&state.pro.link)
        .as_ref()
        .is_some_and(|task| !task.is_finished())
}

fn current(state: &AppState, generation: u64) -> bool {
    !state.stopping.load(Ordering::Acquire)
        && state.pro.generation.load(Ordering::Acquire) == generation
}

async fn run(weak: std::sync::Weak<AppState>, generation: u64) {
    let mut attempts: u32 = 0;
    loop {
        let Some(state) = weak.upgrade() else {
            return;
        };
        // A refused or expired delegation can do nothing; the app mints a new
        // one and configures again, which starts a new link.
        if !current(&state, generation) || super::delegation_lapsed(&state) {
            return;
        }
        // Read each time: a renewed or re-minted delegation is used at once.
        let Some(target) = target(&state) else {
            return;
        };
        let port = state.port;
        let token = state.token.clone();
        drop(state);
        let started = tokio::time::Instant::now();
        let outcome = serve(&weak, generation, &target, port, token).await;
        if started.elapsed() >= Duration::from_secs(20) {
            attempts = 0;
        }
        // Refused: the delegation was revoked, replaced or signed out
        // everywhere. Nothing is dialed again until a new configuration
        // starts a new link (review R3 S2).
        if matches!(outcome, Ended::Refused) {
            tracing::info!(
                target: "chimaera_server::pro::reach",
                "the keeper refused this computer's link; it stays closed until set up again"
            );
            return;
        }
        let wait = backoff(attempts);
        attempts = attempts.saturating_add(1);
        tracing::info!(
            target: "chimaera_server::pro::reach",
            "the link that keeps this computer reachable ended; reconnecting"
        );
        tokio::time::sleep(wait).await;
    }
}

enum Ended {
    Refused,
    Lost,
}

/// Jittered exponential backoff, half a second doubling to ten.
fn backoff(attempt: u32) -> Duration {
    let ceiling = Duration::from_millis(500)
        .saturating_mul(1 << attempt.min(5))
        .min(Duration::from_secs(10));
    let jitter = u64::from(rand_u16()) % (ceiling.as_millis() as u64 / 2 + 1);
    ceiling / 2 + Duration::from_millis(jitter)
}
fn rand_u16() -> u16 {
    let token = chimaera_core::generate_token();
    u16::from_str_radix(&token[..4.min(token.len())], 16).unwrap_or(0)
}

/// Live sessions here, for the keeper's host list (a count only).
fn sessions(state: &AppState) -> usize {
    state.sessions.list().len() + state.chat.list().len()
}

async fn serve(
    weak: &std::sync::Weak<AppState>,
    generation: u64,
    target: &Target,
    port: u16,
    token: String,
) -> Ended {
    let headers = [("Authorization", format!("Bearer {}", target.bearer))];
    let mut socket = match crate::voice::upstream::connect(&target.url, &headers).await {
        Ok(socket) => socket,
        Err(crate::voice::upstream::ConnectError::Rejected(401 | 403)) => return Ended::Refused,
        Err(_) => return Ended::Lost,
    };
    let count = weak.upgrade().map(|state| sessions(&state)).unwrap_or(0);
    let register = json!({
        "type": "register",
        "alias": target.alias,
        "daemon": {"token": token, "build": chimaera_core::BUILD_ID, "sessions": count},
    });
    if socket
        .send(Message::Text(register.to_string().into()))
        .await
        .is_err()
    {
        return Ended::Lost;
    }
    drop(token);
    let mut streams: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    let queued = Arc::new(tokio::sync::Semaphore::new(QUEUED_KIB));
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
    let mut last_pong = tokio::time::Instant::now();
    let ended = loop {
        streams.retain(|_, task| !task.is_finished());
        tokio::select! {
            _ = ping.tick() => {
                if last_pong.elapsed() >= PONG_WITHIN
                    || !weak.upgrade().is_some_and(|state| current(&state, generation))
                {
                    break Ended::Lost;
                }
                if tokio::time::timeout(Duration::from_secs(10), socket.send(Message::Ping(Vec::new().into())))
                    .await
                    .map_or(true, |sent| sent.is_err())
                {
                    break Ended::Lost;
                }
            }
            message = socket.next() => {
                let Some(Ok(message)) = message else { break Ended::Lost };
                match message {
                    Message::Text(text) if text.len() <= MAX_CONTROL_FRAME => {
                        let Ok(event) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                        let id = event["stream_id"].as_str().filter(|id| valid_stream(id));
                        match (event["type"].as_str(), id) {
                            (Some("open"), Some(id)) if !streams.contains_key(id) && streams.len() < MAX_STREAMS => {
                                let url = format!("{}/{id}", target.url);
                                let bearer = target.bearer.clone();
                                let queued = queued.clone();
                                streams.insert(id.to_owned(), tokio::spawn(async move {
                                    let _ = stream(&url, &bearer, port, queued).await;
                                }));
                            }
                            (Some("close"), Some(id)) => {
                                if let Some(task) = streams.remove(id) {
                                    task.abort();
                                }
                            }
                            // `registered` and anything newer: nothing to do.
                            _ => {}
                        }
                    }
                    Message::Ping(data) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break Ended::Lost;
                        }
                    }
                    Message::Pong(_) => last_pong = tokio::time::Instant::now(),
                    _ => break Ended::Lost,
                }
            }
        }
    };
    for (_, task) in streams {
        task.abort();
    }
    ended
}

fn valid_stream(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// One stream the keeper opened: a WebSocket of raw bytes bridged to this
/// daemon's loopback port. End of either side ends both; at most 16 frames
/// of 64 KiB are read ahead, so a slow side holds the other back.
async fn stream(
    url: &str,
    bearer: &str,
    port: u16,
    queued: Arc<tokio::sync::Semaphore>,
) -> anyhow::Result<()> {
    let headers = [("Authorization", format!("Bearer {bearer}"))];
    let socket = crate::voice::upstream::connect(url, &headers)
        .await
        .map_err(|_| anyhow::anyhow!("stream refused"))?;
    let tcp = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await??;
    let (mut tcp_rx, mut tcp_tx) = tcp.into_split();
    let (mut ws_tx, mut ws_rx) = socket.split();
    type Queued = (Message, Option<tokio::sync::OwnedSemaphorePermit>);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Queued>(16);
    let outbound = {
        let tx = tx.clone();
        async move {
            let mut buffer = vec![0; MAX_DATA_FRAME];
            loop {
                // Room in the link-wide budget first, then read at most that.
                let permit = queued
                    .clone()
                    .acquire_many_owned((MAX_DATA_FRAME / 1024) as u32)
                    .await?;
                let n = tcp_rx.read(&mut buffer).await?;
                if n == 0 {
                    let _ = tx.send((Message::Close(None), None)).await;
                    return anyhow::Ok(());
                }
                if tx
                    .send((Message::Binary(buffer[..n].to_vec().into()), Some(permit)))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
        }
    };
    let closing = tx.clone();
    let inbound = async move {
        while let Some(message) = ws_rx.next().await {
            match message? {
                Message::Binary(data) if data.len() <= MAX_DATA_FRAME => {
                    tcp_tx.write_all(&data).await?
                }
                Message::Ping(data) => {
                    if tx.send((Message::Pong(data), None)).await.is_err() {
                        break;
                    }
                }
                Message::Pong(_) => {}
                Message::Close(_) => break,
                _ => anyhow::bail!("invalid data-plane frame"),
            }
        }
        let _ = tcp_tx.shutdown().await;
        anyhow::Ok(())
    };
    let writer = async move {
        while let Some((message, permit)) = rx.recv().await {
            let closing = matches!(message, Message::Close(_));
            tokio::time::timeout(Duration::from_secs(60), ws_tx.send(message)).await??;
            drop(permit);
            if closing {
                break;
            }
        }
        anyhow::Ok(())
    };
    // The daemon's reply and its close are queued for the writer: the stream
    // ends once the writer has sent them, or when the keeper's side ends.
    // Ending as soon as the reading side finished dropped a reply this daemon
    // had already read off its own port (a `Connection: close` request's
    // whole answer).
    let sending = async move {
        let reading = async move {
            let read = outbound.await;
            // A failed read sent no close of its own: end the stream anyway.
            if read.is_err() {
                let _ = closing.send((Message::Close(None), None)).await;
            }
            read
        };
        let (read, written) = tokio::join!(reading, writer);
        read.and(written)
    };
    tokio::select! {
        result = sending => result,
        result = inbound => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_return_guard_is_fifteen_seconds_and_fixed_in_release_builds() {
        assert_eq!(guard_override(false, Some("5")), 15);
        assert_eq!(guard_override(true, None), 15);
        assert_eq!(guard_override(true, Some("5")), 5);
        assert_eq!(guard_override(true, Some("9000")), 300);
        assert_eq!(guard_override(true, Some("soon")), 15);
    }

    /// A daemon without Pro (or not a personal Pro computer) never dials.
    #[tokio::test]
    async fn a_free_daemon_never_dials_and_has_no_link() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-reach-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        start(&state);
        assert!(!running(&state));
        assert!(target(&state).is_none());
        // Configured, but no Runtime composed in: still nothing.
        *lock(&state.pro.runtime) = Some(
            serde_json::from_value(serde_json::json!({
                "role":"device","endpoint":"https://account.example","keeper_url":"https://keeper.example",
                "delegation":{"access_token":"synthetic","expires_at":"2099-01-01T00:00:00Z",
                "scope":["baton","mirror","keeper"],"device_id":"d-home"}
            }))
            .unwrap(),
        );
        state.pro.configured.store(true, Ordering::Release);
        start(&state);
        assert!(!running(&state));
        assert!(!settled(&state));
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reachable_means_answered_without_a_gap() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-reach-gap-{}",
            chimaera_core::generate_token()
        ));
        let state = AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        );
        answered(&state, 200, true);
        let since = state.pro.reachable_since.load(Ordering::Acquire);
        assert!(since > 0);
        answered(&state, 409, true);
        assert_eq!(state.pro.reachable_since.load(Ordering::Acquire), since);
        // A proxy's or captive portal's 5xx (no account marker) proves
        // nothing: not erroring, not reachable.
        answered(&state, 502, false);
        assert_eq!(state.pro.reachable_since.load(Ordering::Acquire), 0);
        assert!(!erroring(&state));
        // A refused credential is not reachable either.
        answered(&state, 200, true);
        answered(&state, 401, true);
        assert_eq!(state.pro.reachable_since.load(Ordering::Acquire), 0);
        // A success needs no marker (an old curl cannot report one).
        answered(&state, 200, false);
        assert!(state.pro.reachable_since.load(Ordering::Acquire) > 0);
        answered(&state, 503, true);
        assert_eq!(state.pro.reachable_since.load(Ordering::Acquire), 0);
        assert!(erroring(&state));
        answered(&state, 200, true);
        unreachable(&state);
        assert_eq!(state.pro.reachable_since.load(Ordering::Acquire), 0);
        state.pro.reachable_since.store(1, Ordering::Release);
        assert!(settled(&state));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn backoff_stays_within_ten_seconds() {
        for attempt in 0..40 {
            let wait = backoff(attempt);
            assert!(wait >= Duration::from_millis(250) && wait <= Duration::from_secs(10));
        }
    }

    #[test]
    fn only_plain_stream_ids_are_dialed() {
        assert!(valid_stream("a1B2"));
        assert!(!valid_stream(""));
        assert!(!valid_stream("../x"));
        assert!(!valid_stream(&"a".repeat(129)));
    }

    /// A reply the daemon writes and closes at once (a `Connection: close`
    /// answer) reaches the keeper whole before the stream ends.
    #[tokio::test]
    async fn a_reply_the_daemon_closes_after_still_reaches_the_keeper() {
        const REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok";
        let daemon = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = daemon.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = daemon.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            socket.write_all(REPLY).await.unwrap();
        });
        let keeper = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/serve/s-1", keeper.local_addr().unwrap());
        let received = tokio::spawn(async move {
            let (socket, _) = keeper.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            socket
                .send(Message::Binary(b"GET / HTTP/1.1\r\n\r\n".to_vec().into()))
                .await
                .unwrap();
            let mut bytes = Vec::new();
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Binary(data) => bytes.extend_from_slice(&data),
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            bytes
        });
        let queued = Arc::new(tokio::sync::Semaphore::new(QUEUED_KIB));
        tokio::time::timeout(
            Duration::from_secs(10),
            stream(&url, "synthetic", port, queued.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        let bytes = tokio::time::timeout(Duration::from_secs(10), received)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bytes, REPLY);
        // Every byte read was released from the link-wide budget.
        assert_eq!(queued.available_permits(), QUEUED_KIB);
    }

    struct Composed;
    impl crate::daemon_extension::Runtime for Composed {
        fn coordinate(
            &self,
            _owner: crate::daemon_extension::CoordinatorOwner,
        ) -> crate::daemon_extension::RuntimeFuture {
            Box::pin(async {})
        }
    }

    /// A configured personal Pro computer with the Runtime, whose keeper is a
    /// local fake at `port`.
    fn configured(name: &str, port: u16) -> (Arc<AppState>, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "chimaera-reach-{name}-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut state = AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        );
        state.daemon_extension = Some(Arc::new(Composed));
        let state = Arc::new(state);
        *lock(&state.pro.runtime) = Some(
            serde_json::from_value(serde_json::json!({
                "role":"device","endpoint":"http://127.0.0.1:1",
                "keeper_url": format!("http://127.0.0.1:{port}"),
                "delegation":{"access_token":"synthetic","expires_at":"2099-01-01T00:00:00Z",
                "scope":["baton","mirror","keeper"],"device_id":"d-home"}
            }))
            .unwrap(),
        );
        state.pro.configured.store(true, Ordering::Release);
        (state, root)
    }

    async fn ended(state: &AppState) -> bool {
        for _ in 0..100 {
            if !running(state) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// Review R3 S2: a keeper that refuses the delegation (revoked, signed out
    /// everywhere) ends the link; it is not dialed again until set up anew.
    #[tokio::test]
    async fn a_refused_delegation_ends_the_link_for_good() {
        let keeper = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = keeper.local_addr().unwrap().port();
        let dials = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = dials.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = keeper.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });
        let (state, root) = configured("refused", port);
        start(&state);
        assert!(
            ended(&state).await,
            "the link kept dialing a refusing keeper"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(dials.load(Ordering::SeqCst), 1);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Signing out (`DELETE /pro/configure`) ends a running link at once.
    #[tokio::test]
    async fn signing_out_ends_the_link() {
        // A keeper that accepts and then says nothing keeps the link open.
        let keeper = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = keeper.local_addr().unwrap().port();
        let held = Arc::new(std::sync::Mutex::new(Vec::new()));
        let holder = held.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = keeper.accept().await {
                lock(&holder).push(socket);
            }
        });
        let (state, root) = configured("sign-out", port);
        start(&state);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(running(&state));
        let response = super::super::routes::disconnect(axum::extract::State(state.clone())).await;
        assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
        assert!(ended(&state).await);
        assert!(target(&state).is_none());
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
