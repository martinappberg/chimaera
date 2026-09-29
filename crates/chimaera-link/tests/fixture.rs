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
    // Exactly what the account service answers for a replayed token.
    assert_eq!(replay.status().as_u16(), 400);
    assert_eq!(
        replay.json::<ApiError>().await.unwrap().error,
        "invalid_grant"
    );
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
        payment_due: None,
        subscription_status: None,
        plans: None,
        returning_until: None,
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

#[tokio::test]
async fn baton_cas_offline_fork_and_mirror_fencing() {
    let fixture = Fixture::start().await;
    let first = fixture.client();
    let second = Client::new(
        &fixture.keeper.endpoint,
        Some(fixture.keeper.add_device("other-device").await.unwrap()),
    )
    .unwrap();
    let id = "w-handoff-test";
    assert_eq!(first.baton(id).await.unwrap().epoch, 0);
    let lease = first
        .acquire_baton(
            id,
            &AcquireBaton {
                holder_id: "fake-device".into(),
                expected_epoch: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(lease.epoch, 1);
    assert!(!lease.requires_fork);
    let held = HeldBaton {
        holder_id: "fake-device".into(),
        epoch: 1,
    };
    first.renew_baton(id, &held).await.unwrap();
    let policy = HandoffPolicy {
        holder_id: "fake-device".into(),
        epoch: 1,
        handoff_enabled: true,
        offline_takeover: true,
        has_agents: true,
    };
    first.set_handoff_policy(id, &policy).await.unwrap();
    assert!(second.set_handoff_policy(id, &policy).await.is_err());
    second.disable_handoff_policy(id).await.unwrap();
    assert!(
        first
            .mirror_credentials(&MirrorRequest {
                workspace_id: id.into(),
                epoch: None
            })
            .await
            .is_err(),
        "privacy revocation denies new read grants too"
    );
    first.set_handoff_policy(id, &policy).await.unwrap();
    assert!(
        first
            .mirror_credentials(&MirrorRequest {
                workspace_id: id.into(),
                epoch: None
            })
            .await
            .is_err(),
        "worker policy publication cannot re-enable mirroring"
    );
    second.enable_mirror(id).await.unwrap();
    let conflict = second
        .acquire_baton(
            id,
            &AcquireBaton {
                holder_id: "other-device".into(),
                expected_epoch: 1,
            },
        )
        .await
        .unwrap_err();
    // The service answers an occupied v1 baton with `stale_epoch`.
    assert_eq!(
        conflict.downcast_ref::<BatonConflict>().unwrap().error,
        "stale_epoch"
    );
    assert!(
        second.renew_baton(id, &held).await.is_err(),
        "holder id cannot impersonate another device"
    );
    let credential = first
        .mirror_credentials(&MirrorRequest {
            workspace_id: id.into(),
            epoch: Some(1),
        })
        .await
        .unwrap();
    assert!(!format!("{credential:?}").contains(&credential.password));
    let http = reqwest::Client::new();
    let write = || {
        http.post(format!("{}/_test/mirror-write", fixture.keeper.endpoint))
            .bearer_auth(fake::STATIC_TOKEN)
            .json(&serde_json::json!({"workspace_id":id,"password":credential.password}))
    };
    assert_eq!(write().send().await.unwrap().status().as_u16(), 204);
    first.release_baton(id, &held).await.unwrap();
    assert_eq!(write().send().await.unwrap().status().as_u16(), 403);
    let next = second
        .acquire_baton(
            id,
            &AcquireBaton {
                holder_id: "other-device".into(),
                expected_epoch: 1,
            },
        )
        .await
        .unwrap();
    assert_eq!(next.epoch, 2);
    assert!(!next.requires_fork);
    http.post(format!(
        "{}/_test/baton/{id}/expire",
        fixture.keeper.endpoint
    ))
    .bearer_auth(fake::STATIC_TOKEN)
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();
    let expired = second
        .renew_baton(
            id,
            &HeldBaton {
                holder_id: "other-device".into(),
                epoch: 2,
            },
        )
        .await
        .unwrap_err();
    // An expired lease cannot be renewed; the service says `stale_epoch`.
    assert_eq!(
        expired.downcast_ref::<BatonConflict>().unwrap().error,
        "stale_epoch"
    );
    assert_eq!(
        first.baton(id).await.unwrap().holder_id.as_deref(),
        Some("other-device")
    );
    let offline = first
        .acquire_baton(
            id,
            &AcquireBaton {
                holder_id: "fake-device".into(),
                expected_epoch: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(offline.epoch, 3);
    assert!(offline.requires_fork);
    let stale = second
        .release_baton(
            id,
            &HeldBaton {
                holder_id: "other-device".into(),
                epoch: 2,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        stale.downcast_ref::<BatonConflict>().unwrap().error,
        "stale_epoch"
    );
    let readonly = first
        .mirror_credentials(&MirrorRequest {
            workspace_id: id.into(),
            epoch: None,
        })
        .await
        .unwrap();
    assert!(readonly.read_only);
    assert_eq!(
        http.post(format!("{}/_test/mirror-write", fixture.keeper.endpoint))
            .bearer_auth(fake::STATIC_TOKEN)
            .json(&serde_json::json!({"workspace_id":id,"password":readonly.password}))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        403
    );
}

#[tokio::test]
async fn revoked_refresh_clears_client_and_publishes_signout() {
    let fixture = Fixture::start().await;
    let client = fixture.client();
    let mut watch = client.token_updates();
    fixture.client().sign_out_everywhere().await.unwrap();
    let error = client.me().await.unwrap_err();
    assert!(error.is::<AuthorizationRevoked>(), "{error:#}");
    tokio::time::timeout(Duration::from_secs(5), watch.changed())
        .await
        .expect("revocation must publish sign-out")
        .unwrap();
    assert!(watch.borrow().is_none());
    assert!(client.tokens().await.is_none());
}

#[tokio::test]
async fn delegation_is_scoped_replaced_and_revoked_with_device() {
    let fixture = Fixture::start().await;
    let client = fixture.client();
    let first = client.delegate_daemon().await.unwrap();
    assert_eq!(first.device_id, "fake-device");
    assert!(!format!("{first:?}").contains(&first.access_token));
    assert_eq!(
        client
            .renew_delegation(&first.access_token)
            .await
            .unwrap()
            .access_token,
        first.access_token
    );
    let http = reqwest::Client::new();
    for path in [
        "/v1/me",
        "/v1/devices",
        "/v1/sign-out-everywhere",
        "/v1/delegations",
    ] {
        let response = http
            .request(
                if path.contains("sign-out") || path == "/v1/delegations" {
                    reqwest::Method::POST
                } else {
                    reqwest::Method::GET
                },
                format!("{}{path}", fixture.keeper.endpoint),
            )
            .bearer_auth(&first.access_token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            401,
            "delegation must not access {path}"
        );
    }
    let lease: Baton = http
        .post(format!(
            "{}/v1/baton/w-delegated/acquire",
            fixture.keeper.endpoint
        ))
        .bearer_auth(&first.access_token)
        .json(&AcquireBaton {
            holder_id: first.device_id.clone(),
            expected_epoch: 0,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lease.holder_id.as_ref(), Some(&first.device_id));
    let second = client.delegate_daemon().await.unwrap();
    assert_ne!(first.access_token, second.access_token);
    assert!(client.renew_delegation(&first.access_token).await.is_err());
    client.sign_out_everywhere().await.unwrap();
    assert!(client.renew_delegation(&second.access_token).await.is_err());
}

#[tokio::test]
async fn executable_handoff_conformance() {
    let fixture = Fixture::start().await;
    let checks = conformance::handoff(&fixture.keeper.endpoint, fake::STATIC_TOKEN, true)
        .await
        .unwrap();
    assert_eq!(checks.len(), 3);
}

#[tokio::test]
async fn account_billing_and_cloud_status_use_device_authentication() {
    let fixture = Fixture::start().await;
    let client = fixture.client();
    let status = client.worker_status().await.unwrap();
    assert_eq!(status.state, WorkerState::Unavailable);
    assert_eq!(status.reason, Some(WorkerReason::ProvisioningDisabled));
    assert_eq!(status.phase, None);
    for plan in [Plan::Pro, Plan::Max] {
        for interval in [BillingInterval::Month, BillingInterval::Year] {
            assert!(client
                .billing_checkout(plan.clone(), interval)
                .await
                .unwrap()
                .url
                .starts_with("https://checkout.stripe.com/"));
        }
    }
    assert!(client
        .billing_checkout(Plan::None, BillingInterval::Month)
        .await
        .is_err());
    assert!(client
        .billing_portal()
        .await
        .unwrap()
        .url
        .starts_with("https://billing.stripe.com/"));
    let unsigned = Client::new(&fixture.keeper.endpoint, None).unwrap();
    assert!(unsigned.worker_status().await.is_err());
    assert!(unsigned.billing_portal().await.is_err());
    let delegation = client.delegate_daemon().await.unwrap();
    let response = reqwest::Client::new()
        .get(format!("{}/v1/worker/status", fixture.keeper.endpoint))
        .bearer_auth(&delegation.access_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
}
