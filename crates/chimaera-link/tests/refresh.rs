#![cfg(feature = "fixtures")]
//! The refresh contract against the account service's real answers: 400
//! `invalid_grant` for revoked/expired/replayed tokens, transient failures
//! retried once with the same token, and keeper refusals never rotating a
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
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::net::TcpListener;

#[derive(Default)]
struct Account {
    endpoint: Mutex<String>,
    /// Scripted refresh statuses, consumed in order; then success.
    refresh_script: Mutex<Vec<u16>>,
    refresh_tokens_seen: Mutex<Vec<String>>,
    refreshes: AtomicUsize,
    /// Whether the account itself still accepts the current access token.
    account_accepts: std::sync::atomic::AtomicBool,
}

fn tokens(prefix: &str) -> Tokens {
    Tokens {
        access_token: format!("{prefix}-access"),
        refresh_token: format!("{prefix}-refresh"),
        token_type: "Bearer".into(),
        expires_in: 900,
    }
}

async fn refresh(State(state): State<Arc<Account>>, Json(body): Json<Value>) -> Response {
    state.refreshes.fetch_add(1, Ordering::SeqCst);
    state
        .refresh_tokens_seen
        .lock()
        .unwrap()
        .push(body["refresh_token"].as_str().unwrap_or_default().into());
    let scripted = {
        let mut script = state.refresh_script.lock().unwrap();
        (!script.is_empty()).then(|| script.remove(0))
    };
    match scripted {
        Some(400) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_grant"})),
        )
            .into_response(),
        Some(status) => StatusCode::from_u16(status).unwrap().into_response(),
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

async fn start(initial: Tokens) -> (Client, Arc<Account>, tokio::task::JoinHandle<()>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(Account::default());
    *state.endpoint.lock().unwrap() = endpoint.clone();
    let router = Router::new()
        .route("/v1/oauth/refresh", post(refresh))
        .route("/v1/me", get(me))
        .route("/v1/devices", get(devices))
        .route("/v1/events", get(events))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (
        Client::new(&endpoint, Some(initial)).unwrap(),
        state,
        server,
    )
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
async fn transient_refresh_failure_retries_once_with_the_same_token() {
    let (client, account, server) = start(tokens("before")).await;
    account.refresh_script.lock().unwrap().push(503);
    client.me().await.unwrap();
    assert_eq!(account.refreshes.load(Ordering::SeqCst), 2);
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
async fn persistent_outage_and_rate_limits_keep_the_session() {
    for script in [vec![503, 502], vec![429], vec![408, 500]] {
        let (client, account, server) = start(tokens("before")).await;
        let attempts = script.len();
        *account.refresh_script.lock().unwrap() = script;
        let error = client.me().await.unwrap_err();
        assert!(!error.is::<AuthorizationRevoked>(), "{error:#}");
        assert_eq!(
            client.tokens().await.unwrap().refresh_token,
            "before-refresh",
            "a transient failure never signs out"
        );
        assert_eq!(account.refreshes.load(Ordering::SeqCst), attempts);
        server.abort();
    }
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
