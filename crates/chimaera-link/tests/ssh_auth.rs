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
        let app = keeper.router();
        Self {
            keeper,
            task: tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
        }
    }
    fn client(&self) -> Client {
        Client::new(&self.keeper.endpoint, Some(fake::FakeKeeper::tokens())).unwrap()
    }
    async fn host(&self, alias: &str) -> Host {
        self.client()
            .register_ssh_auth_host(alias, target(), None)
            .await
            .unwrap()
    }
    async fn grant(&self, host: &str) -> SshAuthGrant {
        let caps = self.client().ssh_auth_capabilities().await.unwrap();
        self.client()
            .create_ssh_auth_grant(host, &request(caps.keeper_boot))
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn target() -> SshTarget {
    SshTarget {
        hostname: "login.example.invalid".into(),
        user: Some("person".into()),
        port: 22,
    }
}
fn request(boot: String) -> SshAuthGrantRequest {
    SshAuthGrantRequest {
        version: 1,
        keeper_boot: boot,
        destination: SshAuthDestination {
            hostname: "login.example.invalid".into(),
            user: "person".into(),
            port: 22,
        },
        host_keys: vec![SshAuthHostKey {
            key: "AQ==".into(),
            is_ca: false,
        }],
        user_keys: vec!["Ag==".into()],
    }
}
#[tokio::test]
async fn absent_capability_prevents_registration_and_first_connect_is_inert_until_bound_reconnect()
{
    let f = Fixture::start().await;
    let client = f.client();
    assert!(client
        .register_ssh_auth_host("cluster", target(), None)
        .await
        .unwrap_err()
        .is::<ServiceUnsupported>());
    assert!(client.hosts().await.unwrap().is_empty());
    f.keeper.set_ssh_auth_supported(true).await;
    assert_eq!(conformance::run_ssh_auth(&client).await.unwrap().len(), 1);
    let host = f.host("cluster").await;
    assert_eq!(host.status, HostStatus::Offline);
    assert!(host.daemon.is_none());
    assert_eq!(f.keeper.ssh_auth_reconnects().await, 0);
    let grant = f.grant(&host.id).await;
    assert_eq!(f.keeper.ssh_auth_reconnects().await, 0);
    assert!(
        client
            .reconnect_host_with_ssh_auth(&host.id, &grant)
            .await
            .is_err(),
        "grant needs a live device channel"
    );
    let _socket = client.ssh_auth_socket(&host.id, &grant).await.unwrap();
    client
        .reconnect_host_with_ssh_auth(&host.id, &grant)
        .await
        .unwrap();
    assert_eq!(f.keeper.ssh_auth_reconnects().await, 1);
    client
        .delete_ssh_auth_grant(&host.id, &grant)
        .await
        .unwrap();
    client
        .delete_ssh_auth_grant(&host.id, &grant)
        .await
        .unwrap();
    assert!(client
        .reconnect_host_with_ssh_auth(&host.id, &grant)
        .await
        .is_err());
    assert!(
        client
            .hosts()
            .await
            .unwrap()
            .iter()
            .any(|row| row.id == host.id),
        "auth failure/deletion never deletes saved host"
    );
}
#[tokio::test]
async fn selected_host_device_boot_and_live_destination_are_required() {
    let f = Fixture::start().await;
    f.keeper.set_ssh_auth_supported(true).await;
    let first = f.host("first").await;
    let second = f.host("second").await;
    let grant = f.grant(&first.id).await;
    let client = f.client();
    let other = Client::new(
        &f.keeper.endpoint,
        Some(f.keeper.tokens_for_device("second-device").await.unwrap()),
    )
    .unwrap();
    assert!(other.ssh_auth_socket(&first.id, &grant).await.is_err());
    assert!(client.ssh_auth_socket(&second.id, &grant).await.is_err());
    let _socket = client.ssh_auth_socket(&first.id, &grant).await.unwrap();
    assert!(
        client.ssh_auth_socket(&first.id, &grant).await.is_err(),
        "live channel replacement refused"
    );
    assert!(client
        .reconnect_host_with_ssh_auth(&second.id, &grant)
        .await
        .is_err());
    assert!(other
        .reconnect_host_with_ssh_auth(&first.id, &grant)
        .await
        .is_err());
    let changed = SshAuthDestination {
        hostname: "other.example.invalid".into(),
        user: "person".into(),
        port: 22,
    };
    f.keeper
        .set_ssh_auth_destination(&first.id, changed)
        .await
        .unwrap();
    assert!(client
        .reconnect_host_with_ssh_auth(&first.id, &grant)
        .await
        .is_err());
    f.keeper.set_ssh_auth_supported(true).await; // boot changes and all old grants end
    assert!(client
        .reconnect_host_with_ssh_auth(&first.id, &grant)
        .await
        .is_err());
    assert_eq!(f.keeper.ssh_auth_reconnects().await, 0);
}
#[tokio::test]
async fn admission_is_per_device_expiry_releases_capacity_and_header_is_reconnect_only() {
    let f = Fixture::start().await;
    f.keeper.set_ssh_auth_supported(true).await;
    let host = f.host("cluster").await;
    let client = f.client();
    let mut grants = Vec::new();
    for _ in 0..4 {
        grants.push(f.grant(&host.id).await);
    }
    let caps = client.ssh_auth_capabilities().await.unwrap();
    assert!(client
        .create_ssh_auth_grant(&host.id, &request(caps.keeper_boot.clone()))
        .await
        .is_err());
    f.keeper.expire_ssh_auth_grant(&grants[0].grant_id).await;
    assert!(client.ssh_auth_socket(&host.id, &grants[0]).await.is_err());
    let grant = f.grant(&host.id).await;
    for path in [
        "/v1/hosts",
        "/v1/hosts/irrelevant/cluster/operations",
        "/v1/ssh/auth/capabilities",
    ] {
        let response = reqwest::Client::new()
            .post(format!("{}{path}", f.keeper.endpoint))
            .bearer_auth(fake::STATIC_TOKEN)
            .header(SSH_AUTH_GRANT_HEADER, &grant.grant_id)
            .json(&serde_json::json!({"alias":"wrong","register_only":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    }
    assert_eq!(f.keeper.ssh_auth_reconnects().await, 0);
    assert_eq!(client.hosts().await.unwrap().len(), 1);
}
#[tokio::test]
async fn global_admission_and_browser_origin_are_refused_without_effects() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let f = Fixture::start().await;
    f.keeper.set_ssh_auth_supported(true).await;
    let host = f.host("cluster").await;
    let caps = f.client().ssh_auth_capabilities().await.unwrap();
    for device in 0..8 {
        let client = Client::new(
            &f.keeper.endpoint,
            Some(
                f.keeper
                    .tokens_for_device(&format!("device-{device}"))
                    .await
                    .unwrap(),
            ),
        )
        .unwrap();
        for _ in 0..4 {
            client
                .create_ssh_auth_grant(&host.id, &request(caps.keeper_boot.clone()))
                .await
                .unwrap();
        }
    }
    assert!(f
        .client()
        .create_ssh_auth_grant(&host.id, &request(caps.keeper_boot))
        .await
        .is_err());
    f.keeper.set_ssh_auth_supported(true).await;
    let grant = f.grant(&host.id).await;
    let url = format!(
        "{}/v1/hosts/{}/ssh/auth/grants/{}/ws",
        f.keeper.endpoint.replacen("http:", "ws:", 1),
        host.id,
        grant.grant_id
    );
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", fake::STATIC_TOKEN).parse().unwrap(),
    );
    req.headers_mut()
        .insert("origin", "http://127.0.0.1".parse().unwrap());
    assert!(tokio::time::timeout(
        Duration::from_secs(2),
        tokio_tungstenite::connect_async(req)
    )
    .await
    .unwrap()
    .is_err());
    assert_eq!(f.keeper.ssh_auth_reconnects().await, 0);
}

#[tokio::test]
async fn paused_upgrade_cannot_admit_reconnect_or_send_ready_after_signout() {
    let f = Fixture::start().await;
    f.keeper.set_ssh_auth_supported(true).await;
    let host = f.host("cluster").await;
    let grant = f.grant(&host.id).await;
    let entered = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    let release = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    f.keeper
        .pause_ssh_auth_upgrade(entered.clone(), release.clone())
        .await;
    let client = f.client();
    let id = host.id.clone();
    let g = grant.clone();
    let upgrade = tokio::spawn(async move { client.ssh_auth_socket(&id, &g).await });
    tokio::time::timeout(Duration::from_secs(2), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert!(f
        .client()
        .reconnect_host_with_ssh_auth(&host.id, &grant)
        .await
        .is_err());
    assert_eq!(f.keeper.ssh_auth_reconnects().await, 0);
    f.client().sign_out_everywhere().await.unwrap();
    release.add_permits(1);
    assert!(tokio::time::timeout(Duration::from_secs(2), upgrade)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert_eq!(f.keeper.ssh_auth_reconnects().await, 0);
}
