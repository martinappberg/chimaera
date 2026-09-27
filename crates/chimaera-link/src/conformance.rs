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
        next_event(
            &mut events,
            |event| matches!(event, Event::Prompt { id: actual, .. } if actual == &id),
        )
        .await?;
        events
            .answer(id.clone(), Some("fixture-answer".into()))
            .await?;
        next_event(
            &mut events,
            |event| matches!(event, Event::PromptClosed { id: actual } if actual == &id),
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
