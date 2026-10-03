//! The same executable checks run against a loopback fixture and a deployed
//! keeper. Destructive test hooks are opt-in and never used against production.
use crate::*;
use anyhow::{bail, ensure, Context, Result};
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_tungstenite::tungstenite::Message;

/// Read-only cluster checks. This deliberately submits no scheduler jobs.
pub async fn run_cluster(client: &Client, host_id: &str) -> Result<Vec<String>> {
    let caps = client.cluster_capabilities().await?;
    ensure!(caps.control_supported(), "cluster control is unsupported");
    let mut report = vec!["cluster capability version and flags verified".into()];
    let reply = client
        .cluster_operation(host_id, &ClusterOperation::Overview { refresh: false })
        .await?;
    ensure!(
        matches!(reply, ClusterReply::Overview { .. }),
        "cluster overview missing"
    );
    report.push("bounded passive overview and protected route identities verified".into());
    let reply = client
        .cluster_operation(host_id, &ClusterOperation::Facts { refresh: false })
        .await?;
    ensure!(
        matches!(reply, ClusterReply::Facts { .. }),
        "cluster facts missing"
    );
    report.push("cached typed cluster facts decoded".into());
    Ok(report)
}

/// Read-only negotiation, not proof that native cryptographic signing works.
pub async fn run_ssh_auth(client: &Client) -> Result<Vec<String>> {
    let caps = client.ssh_auth_capabilities().await?;
    ensure!(
        caps.registration_supported(),
        "inert SSH registration unsupported"
    );
    Ok(vec!["SSH auth version, hostbound and inert registration flags verified; no grant, SSH or signature requested".into()])
}

pub async fn run(
    endpoint: &str,
    token: &str,
    host_id: Option<&str>,
    test_hooks: bool,
) -> Result<Vec<String>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = Client::new(
        endpoint,
        Some(Tokens {
            access_token: token.into(),
            refresh_token: "conformance-no-refresh".into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
        }),
    )?;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()?;
    let base = crate::transport::endpoint(endpoint)?;
    let mut report = Vec::new();
    let unauth = http
        .get(crate::transport::path(&base, &["v1", "me"]))
        .send()
        .await?;
    ensure!(
        unauth.status().as_u16() == 401,
        "account must reject unauthenticated requests"
    );
    report.push("unauthenticated account request rejected".into());
    let me = client.me().await?;
    ensure!(
        me.protocol == PROTOCOL_VERSION,
        "protocol negotiation failed"
    );
    report.push("account version, keeper URL, limits and usage decoded".into());
    let keeper = crate::transport::endpoint(&me.keeper_url)?;
    let unauth = http
        .get(crate::transport::path(&keeper, &["v1", "hosts"]))
        .send()
        .await?;
    ensure!(
        unauth.status().as_u16() == 401,
        "keeper must reject unauthenticated requests"
    );
    let mut unauth_ws = crate::transport::path(&keeper, &["v1", "events"]);
    unauth_ws
        .set_scheme(if keeper.scheme() == "https" {
            "wss"
        } else {
            "ws"
        })
        .map_err(|_| anyhow::anyhow!("invalid websocket URL"))?;
    let rejected = tokio_tungstenite::connect_async(unauth_ws.as_str()).await;
    ensure!(
        matches!(rejected, Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status().as_u16() == 401),
        "unauthenticated websocket upgrade must return 401"
    );
    report.push("unauthenticated keeper REST and WebSocket rejected".into());
    let hosts = client.hosts().await?;
    let devices = client.devices().await?;
    ensure!(
        devices.iter().any(|d| d.current && d.id == me.device_id),
        "current device missing"
    );
    report.push("host directory and current device decoded".into());
    let mut events = client.events();
    if !hosts.is_empty() {
        next_event(&mut events, |event| matches!(event, Event::Host { .. })).await?;
        report.push("events delivers a host snapshot on connect".into());
    }
    if let Some(host_id) = host_id {
        let host = hosts
            .iter()
            .find(|h| h.id == host_id)
            .context("requested host is absent")?;
        if host.cluster.is_some() {
            report.extend(run_cluster(&client, host_id).await?);
        } else {
            let daemon = host
                .daemon
                .as_ref()
                .context("requested host has no daemon manifest")?;
            let tunnel = LinkTunnel::bind(client.clone(), host_id.into()).await?;
            let response = http
                .get(format!(
                    "http://127.0.0.1:{}/api/v1/health",
                    tunnel.local_port
                ))
                .bearer_auth(&daemon.token)
                .send()
                .await?;
            ensure!(
                response.status().is_success(),
                "daemon health failed through TCP bridge"
            );
            report.push("real daemon health through loopback TCP bridge".into());
        }
    }
    // A local echo endpoint makes reverse serve verifiable without an agent or
    // a daemon installation on the machine running the suite.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let echo = tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await?;
        let mut data = [0; 4096];
        loop {
            let n = tcp.read(&mut data).await?;
            if n == 0 {
                break;
            }
            tcp.write_all(&data[..n]).await?;
        }
        Ok::<_, std::io::Error>(())
    });
    let serve = Serve::start(
        client.clone(),
        port,
        "Link conformance".into(),
        Daemon {
            token: "conformance-daemon-token".into(),
            build: "conformance".into(),
            sessions: 0,
        },
    );
    let reverse_host = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(host) = client.hosts().await?.into_iter().find(|h| {
                h.kind == HostKind::Device
                    && h.alias == "Link conformance"
                    && h.status == HostStatus::Connected
            }) {
                return Ok::<_, anyhow::Error>(host);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;
    let mut socket = client.tcp(&reverse_host.id).await?;
    let payload = b"link-conformance\0binary\xff";
    socket
        .send(Message::Binary(payload.to_vec().into()))
        .await?;
    let echoed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Binary(data))) => return Ok(data),
                Some(Ok(Message::Ping(data))) => socket.send(Message::Pong(data)).await?,
                _ => bail!("reverse stream closed before echo"),
            }
        }
    })
    .await??;
    ensure!(echoed.as_ref() == payload, "reverse bytes changed");
    socket.close(None).await?;
    serve.close();
    echo.abort();
    report.push("reverse registration, stream authorization and binary round trip".into());
    if test_hooks {
        let host = hosts.first().context("test hooks need at least one host")?;
        let prompt: serde_json::Value = http.post(crate::transport::path(&keeper, &["_test", "prompt"]))
            .bearer_auth(token).json(&serde_json::json!({"host_id":host.id,"prompt":"Conformance password?","echo":false})).send().await?.error_for_status()?.json().await?;
        let id = prompt["id"]
            .as_str()
            .context("prompt id absent")?
            .to_string();
        let received = next_event(
            &mut events,
            |event| matches!(event, Event::Prompt { host_id, .. } if host_id == &host.id),
        )
        .await?;
        let Event::Prompt { id: local_id, .. } = received else {
            unreachable!()
        };
        events
            .answer(local_id.clone(), Some("fixture-answer".into()))
            .await?;
        next_event(
            &mut events,
            |event| matches!(event, Event::PromptClosed { id: actual } if actual == &local_id),
        )
        .await?;
        let answers: Vec<EventCommand> = http
            .get(crate::transport::path(&keeper, &["_test", "answers"]))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure!(answers.iter().any(|answer| matches!(answer, EventCommand::Answer { id: actual, value: Some(value) } if actual == &id && value == "fixture-answer")), "answer was not relayed");
        report.push("prompt, answer and prompt_closed round trip".into());
        http.post(crate::transport::path(&keeper, &["_test", "drop-events"]))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?;
        next_event(&mut events, |event| matches!(event, Event::Host { .. })).await?;
        report.push("events reconnect and snapshot recovery".into());
    }
    Ok(report)
}
async fn next_event(
    connection: &mut EventConnection,
    predicate: impl Fn(&Event) -> bool,
) -> Result<Event> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match connection.events.recv().await {
                Some(Ok(event)) if predicate(&event) => return Ok(event),
                Some(_) => {}
                None => bail!("events stopped"),
            }
        }
    })
    .await?
}

/// Account-side extension checks. Uses a fresh workspace id and leaves its
/// baton released. Test-only expiry/mirror authorization hooks are opt-in.
pub async fn handoff(endpoint: &str, token: &str, test_hooks: bool) -> Result<Vec<String>> {
    let client = Client::new(
        endpoint,
        Some(Tokens {
            access_token: token.into(),
            refresh_token: "conformance-no-refresh".into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
        }),
    )?;
    let me = client.me().await?;
    let workspace = format!("w-conformance-{:016x}", rand::random::<u64>());
    let initial = client.baton(&workspace).await?;
    ensure!(
        initial.epoch == 0 && initial.holder_id.is_none(),
        "initial baton is occupied"
    );
    let request = AcquireBaton {
        holder_id: me.device_id.clone(),
        expected_epoch: 0,
    };
    let acquired = client.acquire_baton(&workspace, &request).await?;
    ensure!(
        acquired.epoch == 1 && !acquired.requires_fork,
        "initial acquisition shape"
    );
    let stale = client
        .acquire_baton(&workspace, &request)
        .await
        .expect_err("stale CAS must fail");
    ensure!(
        stale
            .downcast_ref::<BatonConflict>()
            .is_some_and(|c| c.error == "stale_epoch"),
        "CAS conflict body absent"
    );
    let held = HeldBaton {
        holder_id: me.device_id.clone(),
        epoch: 1,
    };
    client.renew_baton(&workspace, &held).await?;
    let credentials = client
        .mirror_credentials(&MirrorRequest {
            workspace_id: workspace.clone(),
            epoch: Some(1),
        })
        .await?;
    let released = client.release_baton(&workspace, &held).await?;
    ensure!(
        released.holder_id.is_none() && released.epoch == 1,
        "release changed epoch"
    );
    let reacquired = client
        .acquire_baton(
            &workspace,
            &AcquireBaton {
                holder_id: me.device_id.clone(),
                expected_epoch: 1,
            },
        )
        .await?;
    ensure!(
        reacquired.epoch == 2 && !reacquired.requires_fork,
        "clean transfer must not fork"
    );
    let mut epoch = 2;
    let mut report = vec![
        "baton acquisition, typed CAS conflict, renewal and clean release".into(),
        "scoped write credentials decoded without embedded URL secrets".into(),
    ];
    if test_hooks {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        let base = crate::transport::endpoint(endpoint)?;
        let rejected = http
            .post(crate::transport::path(&base, &["_test", "mirror-write"]))
            .bearer_auth(token)
            .json(&serde_json::json!({"workspace_id":workspace,"password":credentials.password}))
            .send()
            .await?;
        ensure!(
            rejected.status().as_u16() == 403,
            "old mirror credential survived fencing"
        );
        http.post(crate::transport::path(
            &base,
            &["_test", "baton", &workspace, "expire"],
        ))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?;
        let offline = client
            .acquire_baton(
                &workspace,
                &AcquireBaton {
                    holder_id: me.device_id.clone(),
                    expected_epoch: 2,
                },
            )
            .await?;
        ensure!(
            offline.epoch == 3 && offline.requires_fork,
            "offline takeover must fork"
        );
        epoch = 3;
        report.push("expired lease takeover requires fork and old mirror writes are fenced".into());
    }
    client
        .release_baton(
            &workspace,
            &HeldBaton {
                holder_id: me.device_id,
                epoch,
            },
        )
        .await?;
    Ok(report)
}
