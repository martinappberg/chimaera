//! Whether this computer can be reached, and the daemon's own reverse link.
//!
//! One fact decides where work runs: whoever holds a project's lease runs it,
//! and this computer holds it while its daemon can reach the account. The
//! lease loop's own calls are that fact ([`answered`], [`unreachable`]); the
//! guard for bringing cloud work home is that they have succeeded without a
//! gap for a minute ([`settled`]), the only rule against bouncing.
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
/// work comes home: a lid opened for a moment or a network blip pulls nothing
/// home only to lose it again. A development build (the loopback harness) may
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
    const GUARD: u64 = 60;
    value
        .filter(|_| dev)
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(GUARD, |seconds| seconds.min(300))
}

/// A server error this recent means the account is up but failing: a computer
/// then keeps running its own work past its lease (`execution::expire`), since
/// nobody else can acquire through a failing account either.
const ERRORING_FOR: u64 = 30;

/// The lease loop got an HTTP answer from the account.
pub(super) fn answered(state: &AppState, status: u16) {
    let now = super::now();
    if status >= 500 {
        state.pro.erroring_at.store(now, Ordering::Release);
        state.pro.reachable_since.store(0, Ordering::Release);
    } else {
        let _ = state.pro.reachable_since.compare_exchange(
            0,
            now.max(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
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
        if !current(&state, generation) {
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
        let wait = match outcome {
            // Refused: the delegation was revoked or replaced. The account
            // side closes every route it opened; wait for a new configuration
            // rather than hammering the keeper.
            Ended::Refused => Duration::from_secs(60),
            Ended::Lost => backoff(attempts),
        };
        attempts = attempts.saturating_add(1);
        tracing::info!(
            target: "chimaera_server::pro::reach",
            refused = matches!(outcome, Ended::Refused),
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
                                streams.insert(id.to_owned(), tokio::spawn(async move {
                                    let _ = stream(&url, &bearer, port).await;
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
async fn stream(url: &str, bearer: &str, port: u16) -> anyhow::Result<()> {
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
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(16);
    let outbound = {
        let tx = tx.clone();
        async move {
            let mut buffer = vec![0; MAX_DATA_FRAME];
            loop {
                let n = tcp_rx.read(&mut buffer).await?;
                if n == 0 {
                    let _ = tx.send(Message::Close(None)).await;
                    return anyhow::Ok(());
                }
                if tx
                    .send(Message::Binary(buffer[..n].to_vec().into()))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
        }
    };
    let inbound = async move {
        while let Some(message) = ws_rx.next().await {
            match message? {
                Message::Binary(data) if data.len() <= MAX_DATA_FRAME => {
                    tcp_tx.write_all(&data).await?
                }
                Message::Ping(data) => {
                    if tx.send(Message::Pong(data)).await.is_err() {
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
        while let Some(message) = rx.recv().await {
            let closing = matches!(message, Message::Close(_));
            tokio::time::timeout(Duration::from_secs(60), ws_tx.send(message)).await??;
            if closing {
                break;
            }
        }
        anyhow::Ok(())
    };
    tokio::select! {
        result = outbound => result,
        result = inbound => result,
        result = writer => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_return_guard_is_a_minute_and_fixed_in_release_builds() {
        assert_eq!(guard_override(false, Some("5")), 60);
        assert_eq!(guard_override(true, None), 60);
        assert_eq!(guard_override(true, Some("5")), 5);
        assert_eq!(guard_override(true, Some("9000")), 300);
        assert_eq!(guard_override(true, Some("soon")), 60);
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
        answered(&state, 200);
        let since = state.pro.reachable_since.load(Ordering::Acquire);
        assert!(since > 0);
        answered(&state, 409);
        assert_eq!(state.pro.reachable_since.load(Ordering::Acquire), since);
        answered(&state, 503);
        assert_eq!(state.pro.reachable_since.load(Ordering::Acquire), 0);
        assert!(erroring(&state));
        answered(&state, 200);
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
}
