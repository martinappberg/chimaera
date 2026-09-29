#![cfg(feature = "fixtures")]
//! The refresh contract against the account service's real answers: 400
//! `invalid_grant` for revoked/expired/replayed tokens (a rotated token is
//! consumed on receipt), transient failures that keep the session without
//! presenting the token again at once, and keeper refusals never rotating a
//! token the account still accepts.
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chimaera_link::{AuthorizationRevoked, Client, Tokens};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[derive(Default)]
struct Account {
    endpoint: Mutex<String>,
    /// Scripted refresh statuses, consumed in order; then success.
    refresh_script: Mutex<Vec<u16>>,
    refresh_tokens_seen: Mutex<Vec<String>>,
    /// Refresh tokens already rotated; presenting one again is theft.
    consumed: Mutex<HashSet<String>>,
    refreshes: AtomicUsize,
    /// Whether the account itself still accepts the current access token.
    account_accepts: std::sync::atomic::AtomicBool,
    /// What the keeper's `/v1/hosts` answers (see `hosts`).
    keeper_status: std::sync::atomic::AtomicU16,
}

fn tokens(prefix: &str) -> Tokens {
    Tokens {
        access_token: format!("{prefix}-access"),
        refresh_token: format!("{prefix}-refresh"),
        token_type: "Bearer".into(),
        expires_in: 900,
    }
}

fn invalid_grant() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":"invalid_grant"})),
    )
        .into_response()
}

async fn refresh(State(state): State<Arc<Account>>, Json(body): Json<Value>) -> Response {
    state.refreshes.fetch_add(1, Ordering::SeqCst);
    let presented = body["refresh_token"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    state
        .refresh_tokens_seen
        .lock()
        .unwrap()
        .push(presented.clone());
    let scripted = {
        let mut script = state.refresh_script.lock().unwrap();
        (!script.is_empty()).then(|| script.remove(0))
    };
    match scripted {
        Some(400) => invalid_grant(),
        Some(status) => StatusCode::from_u16(status).unwrap().into_response(),
        // Rotation commits on receipt, before the reply is written.
        None if !state.consumed.lock().unwrap().insert(presented) => invalid_grant(),
        None => Json(tokens("after")).into_response(),
    }
}

async fn me(State(state): State<Arc<Account>>, headers: HeaderMap) -> Response {
    if headers
        .get("authorization")
        .is_none_or(|value| value != "Bearer after-access")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(
        json!({"account_id":"fixture","email":"fixture@example.invalid","plan":"pro",
        "device_id":"d-fixture","protocol":0,"keeper_url":*state.endpoint.lock().unwrap(),
        "limits":{"cloud_hours":1,"storage_bytes":1},"usage":{"cloud_hours":0,"storage_bytes":0},
        "hours_exhausted":false}),
    )
    .into_response()
}

async fn devices(State(state): State<Arc<Account>>) -> Response {
    if state.account_accepts.load(Ordering::SeqCst) {
        Json(json!([])).into_response()
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// The keeper always refuses the upgrade, as a keeper does during an account
/// outage or with a stale revocation cache.
async fn events() -> StatusCode {
    StatusCode::UNAUTHORIZED
}

/// A keeper HTTP route answering what its account lets it: 503
/// `account_unavailable` while the account is down, 401 when it refuses.
async fn hosts(State(state): State<Arc<Account>>) -> Response {
    match state.keeper_status.load(Ordering::SeqCst) {
        503 => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"account_unavailable"})),
        )
            .into_response(),
        401 => StatusCode::UNAUTHORIZED.into_response(),
        _ => Json(json!([])).into_response(),
    }
}

async fn start(initial: Tokens) -> (Client, Arc<Account>, tokio::task::JoinHandle<()>) {
    let (account, state, server) = serve_account().await;
    (
        Client::new(&format!("http://{account}"), Some(initial)).unwrap(),
        state,
        server,
    )
}

async fn serve_account() -> (SocketAddr, Arc<Account>, tokio::task::JoinHandle<()>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = format!("http://{address}");
    let state = Arc::new(Account::default());
    *state.endpoint.lock().unwrap() = endpoint.clone();
    let router = Router::new()
        .route("/v1/oauth/refresh", post(refresh))
        .route("/v1/me", get(me))
        .route("/v1/devices", get(devices))
        .route("/v1/events", get(events))
        .route("/v1/hosts", get(hosts))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (address, state, server)
}

/// Relays to the account, but the first refresh loses its reply after the
/// account committed the rotation: the connection drops once the answer
/// arrives, exactly like a timeout or a network change mid-request.
async fn lossy_proxy(upstream: SocketAddr) -> (String, tokio::task::JoinHandle<()>) {
    const REFRESH: &[u8] = b"POST /v1/oauth/refresh";
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let lost = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(async move {
        while let Ok((mut device, _)) = listener.accept().await {
            let lost = lost.clone();
            tokio::spawn(async move {
                let Ok(mut account) = TcpStream::connect(upstream).await else {
                    return;
                };
                let (mut up, mut down) = ([0_u8; 16 * 1024], [0_u8; 16 * 1024]);
                let mut lose = false;
                loop {
                    tokio::select! {
                        read = device.read(&mut up) => {
                            let Ok(n @ 1..) = read else { return };
                            if up[..n].windows(REFRESH.len()).any(|w| w == REFRESH)
                                && !lost.swap(true, Ordering::SeqCst)
                            {
                                lose = true;
                            }
                            if account.write_all(&up[..n]).await.is_err() {
                                return;
                            }
                        }
                        read = account.read(&mut down) => {
                            let Ok(n @ 1..) = read else { return };
                            if lose {
                                return;
                            }
                            if device.write_all(&down[..n]).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    (endpoint, task)
}

#[tokio::test]
async fn invalid_grant_clears_credentials_and_publishes_sign_out() {
    let (client, account, server) = start(tokens("before")).await;
    account.refresh_script.lock().unwrap().push(400);
    let mut updates = client.token_updates();
    let error = client.me().await.unwrap_err();
    assert!(error.is::<AuthorizationRevoked>(), "{error:#}");
    tokio::time::timeout(Duration::from_secs(5), updates.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(updates.borrow().is_none());
    assert!(client.tokens().await.is_none());
    // A revoked token is never presented again: every later call fails
    // locally instead of bumping the account's session epoch once more.
    assert!(client.me().await.is_err());
    assert_eq!(account.refreshes.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn an_answered_failure_keeps_the_session_for_a_later_refresh() {
    let (client, account, server) = start(tokens("before")).await;
    account.refresh_script.lock().unwrap().push(503);
    let error = client.me().await.unwrap_err();
    assert!(!error.is::<AuthorizationRevoked>(), "{error:#}");
    // The account answered, so it may have rotated: no immediate replay.
    assert_eq!(account.refreshes.load(Ordering::SeqCst), 1);
    client.me().await.unwrap();
    assert_eq!(
        *account.refresh_tokens_seen.lock().unwrap(),
        vec!["before-refresh".to_string(), "before-refresh".to_string()]
    );
    assert_eq!(
        client.tokens().await.unwrap().refresh_token,
        "after-refresh"
    );
    server.abort();
}

#[tokio::test]
async fn outages_deploys_and_rate_limits_keep_the_session() {
    for status in [500, 502, 503, 504, 404, 408, 429] {
        let (client, account, server) = start(tokens("before")).await;
        account.refresh_script.lock().unwrap().push(status);
        let error = client.me().await.unwrap_err();
        assert!(!error.is::<AuthorizationRevoked>(), "{status}: {error:#}");
        assert_eq!(
            client.tokens().await.unwrap().refresh_token,
            "before-refresh",
            "{status} never signs out"
        );
        assert_eq!(account.refreshes.load(Ordering::SeqCst), 1, "{status}");
        server.abort();
    }
}

#[tokio::test]
async fn a_refresh_whose_reply_is_lost_is_not_replayed() {
    let (upstream, account, server) = serve_account().await;
    let (endpoint, proxy) = lossy_proxy(upstream).await;
    let client = Client::new(&endpoint, Some(tokens("before"))).unwrap();
    let updates = client.token_updates();
    let error = client.me().await.unwrap_err();
    assert!(!error.is::<AuthorizationRevoked>(), "{error:#}");
    // The account rotated on receipt; presenting the old token again would
    // read as theft and revoke this device.
    assert_eq!(account.refreshes.load(Ordering::SeqCst), 1);
    assert!(account.consumed.lock().unwrap().contains("before-refresh"));
    assert_eq!(
        client.tokens().await.unwrap().refresh_token,
        "before-refresh",
        "an unknown outcome is not a sign-out"
    );
    assert!(!updates.has_changed().unwrap());
    proxy.abort();
    server.abort();
}

#[tokio::test]
async fn keeper_refusal_rotates_only_when_the_account_rejects_the_token() {
    let (client, account, server) = start(tokens("after")).await;
    client.me().await.unwrap();
    account.account_accepts.store(true, Ordering::SeqCst);
    assert!(client.open_socket(&["v1", "events"], true).await.is_err());
    assert_eq!(
        account.refreshes.load(Ordering::SeqCst),
        0,
        "the account still accepts this token; the keeper's refusal is its own"
    );
    assert_eq!(client.tokens().await.unwrap().access_token, "after-access");
    account.account_accepts.store(false, Ordering::SeqCst);
    assert!(client.open_socket(&["v1", "events"], true).await.is_err());
    assert_eq!(account.refreshes.load(Ordering::SeqCst), 1);
    server.abort();
}

/// During an account outage the keeper answers 503 `account_unavailable`:
/// transient, never a refresh or a sign-out. A keeper 401 on an HTTP route
/// rotates only when the account itself rejects the token, as for sockets.
#[tokio::test]
async fn keeper_account_outage_is_transient_on_http_routes() {
    let (client, account, server) = start(tokens("after")).await;
    client.me().await.unwrap();
    account.keeper_status.store(503, Ordering::SeqCst);
    let error = client.hosts().await.unwrap_err();
    assert!(!error.is::<AuthorizationRevoked>(), "{error:#}");
    assert_eq!(account.refreshes.load(Ordering::SeqCst), 0);
    assert_eq!(client.tokens().await.unwrap().access_token, "after-access");
    account.keeper_status.store(401, Ordering::SeqCst);
    account.account_accepts.store(true, Ordering::SeqCst);
    assert!(client.hosts().await.is_err());
    assert_eq!(
        account.refreshes.load(Ordering::SeqCst),
        0,
        "the account still accepts this token; the keeper's refusal is its own"
    );
    assert_eq!(client.tokens().await.unwrap().access_token, "after-access");
    account.keeper_status.store(200, Ordering::SeqCst);
    assert!(client.hosts().await.unwrap().is_empty());
    server.abort();
}
