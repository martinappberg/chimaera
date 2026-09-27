#![cfg(feature = "fixtures")]
use chimaera_link::*;
use std::time::Duration;
use tokio::net::TcpListener;
struct Fixture {
    keeper: fake::FakeKeeper,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let keeper = fake::FakeKeeper::new(format!("http://{}", listener.local_addr().unwrap()));
        let router = keeper.router();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { keeper, task }
    }
    fn client(&self) -> Client {
        Client::new(&self.keeper.endpoint, Some(fake::FakeKeeper::tokens())).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn executable_conformance_against_real_loopback_sockets() {
    let fixture = Fixture::start().await;
    fixture.client().add_host("fixture-offline").await.unwrap();
    let checks = conformance::run(&fixture.keeper.endpoint, fake::STATIC_TOKEN, None, true)
        .await
        .unwrap();
    assert!(checks.len() >= 7, "{checks:?}");
}
#[tokio::test]
async fn oauth_enforces_pkce_single_use_and_refresh_rotation() {
    let fixture = Fixture::start().await;
    let pkce = Pkce::new();
    let redirect = "http://127.0.0.1:49152/callback";
    let http = reqwest::Client::new();
    let html = http
        .get(
            pkce.authorization_url(&fixture.keeper.endpoint, redirect)
                .unwrap(),
        )
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let href = html
        .split("href=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .replace("&amp;", "&");
    let code = pkce
        .callback_code(&url::Url::parse(&href).unwrap())
        .unwrap();
    let request = TokenRequest {
        grant_type: "authorization_code".into(),
        code,
        redirect_uri: redirect.into(),
        code_verifier: pkce.verifier,
        device_name: "Fixture".into(),
    };
    let client = Client::new(&fixture.keeper.endpoint, None).unwrap();
    let old = client.exchange_code(request.clone()).await.unwrap();
    assert!(
        client.exchange_code(request).await.is_err(),
        "code must be single-use"
    );
    let mut changes = client.token_updates();
    http.post(format!("{}/_test/expire-access", fixture.keeper.endpoint))
        .bearer_auth(&old.access_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let (a, b) = tokio::join!(client.me(), client.devices());
    a.unwrap();
    b.unwrap();
    changes.changed().await.unwrap();
    let new = changes.borrow_and_update().clone().unwrap();
    assert_ne!(old.refresh_token, new.refresh_token);
    let replay = http
        .post(format!("{}/v1/oauth/refresh", fixture.keeper.endpoint))
        .json(&RefreshRequest {
            refresh_token: old.refresh_token,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status().as_u16(), 401);
}
#[tokio::test]
async fn wrong_pkce_cannot_exchange_code() {
    let fixture = Fixture::start().await;
    let pkce = Pkce::new();
    let redirect = "http://127.0.0.1:49152/callback";
    let html = reqwest::get(
        pkce.authorization_url(&fixture.keeper.endpoint, redirect)
            .unwrap(),
    )
    .await
    .unwrap()
    .text()
    .await
    .unwrap();
    let href = html
        .split("href=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .replace("&amp;", "&");
    let code = pkce
        .callback_code(&url::Url::parse(&href).unwrap())
        .unwrap();
    let request = TokenRequest {
        grant_type: "authorization_code".into(),
        code,
        redirect_uri: redirect.into(),
        code_verifier: "wrong".repeat(20),
        device_name: "Fixture".into(),
    };
    assert!(fixture.client().exchange_code(request).await.is_err());
}
#[tokio::test]
async fn revocation_invalidates_access_refresh_and_live_streams() {
    let fixture = Fixture::start().await;
    let client = fixture.client();
    client.add_host("offline").await.unwrap();
    let mut events = client.events();
    let first = tokio::time::timeout(Duration::from_secs(3), events.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(first, Ok(Event::Host { .. })));
    let other = fixture.client();
    other.sign_out_everywhere().await.unwrap();
    assert!(client.me().await.is_err());
    let closed = tokio::time::timeout(Duration::from_secs(3), events.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(closed.is_err());
}
#[tokio::test]
async fn reverse_nonce_is_single_use_and_not_guessable() {
    let fixture = Fixture::start().await;
    let client = fixture.client();
    assert!(client
        .open_socket(&["v1", "serve", "unknown"], false)
        .await
        .is_err());
}

#[tokio::test]
async fn reverse_capability_is_consumed_once_and_control_drop_closes_stream() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let fixture = Fixture::start().await;
    let client = fixture.client();
    let mut control = client.open_socket(&["v1", "serve"], true).await.unwrap();
    control
        .send(Message::Text(
            serde_json::to_string(&ServeCommand::Register {
                alias: "test-device".into(),
                daemon: Daemon {
                    token: "daemon".into(),
                    build: "test".into(),
                    sessions: 0,
                },
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let host_id = loop {
        if let Some(Ok(Message::Text(text))) = control.next().await {
            if let ServeEvent::Registered { host_id } = serde_json::from_str(&text).unwrap() {
                break host_id;
            }
        }
    };
    let mut visitor = client.tcp(&host_id).await.unwrap();
    let stream_id = loop {
        match control.next().await {
            Some(Ok(Message::Text(text))) => {
                if let ServeEvent::Open { stream_id } = serde_json::from_str(&text).unwrap() {
                    break stream_id;
                }
            }
            Some(Ok(Message::Ping(bytes))) => control.send(Message::Pong(bytes)).await.unwrap(),
            _ => {}
        }
    };
    assert!(stream_id.len() >= 43);
    let _reverse = client
        .open_socket(&["v1", "serve", &stream_id], false)
        .await
        .unwrap();
    assert!(client
        .open_socket(&["v1", "serve", &stream_id], false)
        .await
        .is_err());
    control.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match visitor.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unassigned_keeper_does_not_block_account_and_devices() {
    use axum::{routing::get, Json, Router};
    let account = Account {
        account_id: "new-account".into(),
        email: "new@example.invalid".into(),
        plan: Plan::None,
        device_id: "new-device".into(),
        protocol: 0,
        keeper_url: String::new(),
        limits: Limits {
            cloud_hours: 0,
            storage_bytes: 0,
        },
        usage: Usage {
            cloud_hours: 0.0,
            storage_bytes: 0,
        },
        hours_exhausted: false,
    };
    let router = Router::new()
        .route(
            "/v1/me",
            get(move || {
                let account = account.clone();
                async { Json(account) }
            }),
        )
        .route("/v1/devices", get(|| async { Json(Vec::<Device>::new()) }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        Some(fake::FakeKeeper::tokens()),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    assert_eq!(client.me().await.unwrap().plan, Plan::None);
    assert!(client.devices().await.unwrap().is_empty());
    let error = client.hosts().await.unwrap_err().to_string();
    assert!(error.contains("no keeper is assigned"));
    task.abort();
}
