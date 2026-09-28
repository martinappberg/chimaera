#![cfg(feature = "fixtures")]
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use chimaera_link::{Client, Tokens};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::Notify};

#[derive(Default)]
struct Rotation {
    started: Notify,
    release: Notify,
    count: AtomicUsize,
}
async fn refresh(State(state): State<Arc<Rotation>>, Json(body): Json<Value>) -> Json<Tokens> {
    assert_eq!(body["refresh_token"], "before-refresh");
    assert_eq!(state.count.fetch_add(1, Ordering::SeqCst), 0);
    state.started.notify_one();
    state.release.notified().await;
    Json(tokens("after"))
}
async fn me(headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
    if headers
        .get("authorization")
        .is_none_or(|value| value != "Bearer after-access")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(
        json!({"account_id":"fixture","email":"fixture@example.invalid","plan":"none",
        "device_id":"fixture","protocol":0,"keeper_url":"",
        "limits":{"cloud_hours":0,"storage_bytes":0},"usage":{"cloud_hours":0,"storage_bytes":0},"hours_exhausted":false}),
    ))
}
fn tokens(prefix: &str) -> Tokens {
    Tokens {
        access_token: format!("{prefix}-access"),
        refresh_token: format!("{prefix}-refresh"),
        token_type: "Bearer".into(),
        expires_in: 3600,
    }
}
struct Fixture {
    client: Client,
    rotation: Arc<Rotation>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let rotation = Arc::new(Rotation::default());
        let router = Router::new()
            .route("/v1/me", get(me))
            .route("/v1/oauth/refresh", post(refresh))
            .with_state(rotation.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            client: Client::new(&endpoint, Some(tokens("before"))).unwrap(),
            rotation,
            server,
        }
    }
    async fn cancel_during_rotation(&self) {
        let client = self.client.clone();
        let caller = tokio::spawn(async move { client.me().await });
        tokio::time::timeout(Duration::from_secs(5), self.rotation.started.notified())
            .await
            .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::test]
async fn canceling_account_read_preserves_one_use_rotation_and_waiters_reuse_it() {
    let fixture = Fixture::start().await;
    let mut updates = fixture.client.token_updates();
    fixture.cancel_during_rotation().await;
    let pending: Vec<_> = (0..8)
        .map(|_| {
            let client = fixture.client.clone();
            tokio::spawn(async move { client.me().await })
        })
        .collect();
    fixture.rotation.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), updates.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        updates.borrow_and_update().as_ref().unwrap().refresh_token,
        "after-refresh"
    );
    for request in pending {
        tokio::time::timeout(Duration::from_secs(5), request)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    assert_eq!(fixture.rotation.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn clearing_credentials_waits_for_rotation_and_cannot_resurrect_signin() {
    let fixture = Fixture::start().await;
    fixture.cancel_during_rotation().await;
    let client = fixture.client.clone();
    let mut clear = tokio::spawn(async move { client.clear_tokens().await });
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut clear)
        .await
        .is_err());
    fixture.rotation.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), clear)
        .await
        .unwrap()
        .unwrap();
    assert!(fixture.client.tokens().await.is_none());
    assert!(fixture.client.token_updates().borrow().is_none());
    assert!(fixture.client.me().await.is_err());
    assert_eq!(fixture.rotation.count.load(Ordering::SeqCst), 1);
}
