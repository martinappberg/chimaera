//! Loopback keeper and synthetic public keys only; no personal agent or host.
use super::*;
use crate::ssh_agent::{lifecycle::Registry, selection};
use chimaera_link::{fake::FakeKeeper, Host, SshTarget};
use std::{path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Semaphore,
};

struct Fixture {
    keeper: FakeKeeper,
    server: tokio::task::JoinHandle<()>,
    directory: PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let keeper = FakeKeeper::new(format!("http://{}", listener.local_addr().unwrap()));
        keeper.set_ssh_auth_supported(true).await;
        let router = keeper.router();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let directory =
            std::env::temp_dir().join(format!("cc-{}", &chimaera_core::generate_token()[..16]));
        std::fs::create_dir(&directory).unwrap();
        Self {
            keeper,
            server,
            directory,
        }
    }
    fn client(&self) -> Client {
        Client::new(&self.keeper.endpoint, Some(FakeKeeper::tokens())).unwrap()
    }
    async fn selection(&self) -> (Host, Selection) {
        let client = self.client();
        let host = client
            .register_ssh_auth_host(
                "cluster",
                SshTarget {
                    hostname: "hpc.example.invalid".into(),
                    user: Some("alice".into()),
                    port: 22,
                },
                None,
            )
            .await
            .unwrap();
        let public = ssh_key::PrivateKey::new(
            ssh_key::private::KeypairData::Ed25519(ssh_key::private::Ed25519Keypair::from_seed(
                &[7; 32],
            )),
            "synthetic",
        )
        .unwrap()
        .public_key()
        .clone();
        let known = self.directory.join("known_hosts");
        std::fs::write(
            &known,
            format!("hpc.example.invalid {}\n", public.to_openssh().unwrap()),
        )
        .unwrap();
        let socket = self.directory.join("agent");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let agent = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            assert_eq!(socket.read_u32().await.unwrap(), 1);
            assert_eq!(socket.read_u8().await.unwrap(), 11);
            let mut response = vec![12];
            response.extend_from_slice(&1u32.to_be_bytes());
            let blob = public.to_bytes().unwrap();
            response.extend_from_slice(&(blob.len() as u32).to_be_bytes());
            response.extend_from_slice(&blob);
            response.extend_from_slice(&0u32.to_be_bytes());
            socket.write_u32(response.len() as u32).await.unwrap();
            socket.write_all(&response).await.unwrap();
        });
        let config=format!("hostname hpc.example.invalid\nuser alice\nport 22\npubkeyauthentication true\nidentitiesonly no\nhostkeyalgorithms ssh-ed25519\npubkeyacceptedalgorithms ssh-ed25519\ncasignaturealgorithms ssh-ed25519\nidentityagent {}\nuserknownhostsfile {}\nglobalknownhostsfile none\n",socket.display(),known.display());
        let boot = client.ssh_auth_capabilities().await.unwrap().keeper_boot;
        let selection = selection::from_native_config(&config, &self.directory, None, boot)
            .await
            .unwrap();
        agent.await.unwrap();
        (host, selection)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn ready_precedes_reconnect_and_completing_authentication_preserves_host() {
    let fixture = Fixture::new().await;
    let (host, selection) = fixture.selection().await;
    let registry = Registry::default();
    let client = fixture.client();
    let result = authenticate(
        &client,
        &host.id,
        selection,
        registry.admit(0).ok().unwrap(),
        || async {
            assert_eq!(fixture.keeper.ssh_auth_reconnects().await, 1);
            Ok(17)
        },
    )
    .await;
    assert_eq!(result.ok().unwrap(), 17);
    assert_eq!(fixture.keeper.ssh_auth_reconnects().await, 1);
    assert!(client
        .hosts()
        .await
        .unwrap()
        .iter()
        .any(|item| item.id == host.id));
}

#[tokio::test]
async fn account_change_cancels_paused_upgrade_before_reconnect() {
    let fixture = Fixture::new().await;
    let (host, selection) = fixture.selection().await;
    let registry = Registry::default();
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    fixture
        .keeper
        .pause_ssh_auth_upgrade(entered.clone(), release.clone())
        .await;
    let client = fixture.client();
    let id = host.id.clone();
    let attempt = registry.admit(0).ok().unwrap();
    let task = tokio::spawn(async move {
        authenticate(&client, &id, selection, attempt, || async {
            panic!("Reconnect must not happen");
            #[allow(unreachable_code)]
            Ok(())
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    registry.advance(1);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap(),
        Err(Failure::Revoked)
    ));
    release.add_permits(1);
    assert_eq!(fixture.keeper.ssh_auth_reconnects().await, 0);
    assert!(fixture
        .client()
        .hosts()
        .await
        .unwrap()
        .iter()
        .any(|item| item.id == host.id));
}

#[tokio::test]
async fn account_change_cancels_pending_connect_without_replaying_or_removing_host() {
    let fixture = Fixture::new().await;
    let (host, selection) = fixture.selection().await;
    let registry = Registry::default();
    let entered = Arc::new(Semaphore::new(0));
    let waiting = entered.clone();
    let client = fixture.client();
    let id = host.id.clone();
    let attempt = registry.admit(0).ok().unwrap();
    let task = tokio::spawn(async move {
        authenticate(&client, &id, selection, attempt, || async {
            waiting.add_permits(1);
            std::future::pending::<Result<(), Failure>>().await
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    registry.advance(1);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap(),
        Err(Failure::Revoked)
    ));
    assert_eq!(fixture.keeper.ssh_auth_reconnects().await, 1);
    assert!(fixture
        .client()
        .hosts()
        .await
        .unwrap()
        .iter()
        .any(|item| item.id == host.id));
}

#[tokio::test]
async fn route_cleanup_survives_observer_abort_and_retains_shared_budget_until_http_settles() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let keeper = FakeKeeper::new(format!("http://{}", listener.local_addr().unwrap()));
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let waiting = entered.clone();
    let settled = release.clone();
    let router = keeper.router().route(
        "/v1/hosts/host/ssh/auth/route-grants/grant",
        axum::routing::delete(move |headers: axum::http::HeaderMap| {
            let waiting = waiting.clone();
            let settled = settled.clone();
            async move {
                assert_eq!(
                    headers.get("authorization").unwrap(),
                    "Bearer fake-keeper-local-token"
                );
                waiting.add_permits(1);
                settled.acquire().await.unwrap().forget();
                axum::http::StatusCode::NO_CONTENT
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = Client::new(&keeper.endpoint, Some(FakeKeeper::tokens())).unwrap();
    let registry = Registry::default();
    let mut tasks = Vec::new();
    let held = Arc::new(Semaphore::new(0));
    for _ in 0..4 {
        let lease = RouteLease {
            prompt_owner: None,
            client: client.clone(),
            host: "host".into(),
            grant: chimaera_link::SshRouteGrant {
                policies: None,
                version: 1,
                grant_id: "grant".into(),
                expires_in: 180,
                destination: chimaera_link::SshAuthDestination {
                    hostname: "synthetic.example.invalid".into(),
                    user: "synthetic".into(),
                    port: 22,
                },
                route: chimaera_link::SshRoute {
                    version: 1,
                    jumps: Vec::new(),
                },
                modes: vec![chimaera_link::SshRouteMode::Key],
            },
            attempt: Some(registry.admit(0).ok().unwrap()),
        };
        let held = held.clone();
        tasks.push(tokio::spawn(async move {
            let _lease = lease;
            held.add_permits(1);
            std::future::pending::<()>().await;
        }));
    }
    tokio::time::timeout(Duration::from_secs(2), held.acquire_many(4))
        .await
        .unwrap()
        .unwrap()
        .forget();
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
    tokio::time::timeout(Duration::from_secs(2), entered.acquire_many(4))
        .await
        .unwrap()
        .unwrap()
        .forget();
    registry.advance(1);
    assert!(matches!(registry.admit(1), Err(Failure::Unavailable)));
    release.add_permits(4);
    let attempt = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(attempt) = registry.admit(1) {
                return attempt;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!*attempt.cancellation().borrow());
    server.abort();
}
