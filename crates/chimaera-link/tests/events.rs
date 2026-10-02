#![cfg(feature = "fixtures")]
use chimaera_link::*;
use futures::{SinkExt, StreamExt};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::Notify};
use tokio_tungstenite::tungstenite::Message;

#[derive(Default)]
struct HandshakeGate {
    connections: AtomicUsize,
    blocked: Notify,
    release: Notify,
}
struct Fixture {
    keeper: fake::FakeKeeper,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn start(gate: Option<Arc<HandshakeGate>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let keeper = fake::FakeKeeper::new(format!("http://{}", listener.local_addr().unwrap()));
        let router = keeper.router().layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let gate = gate.clone();
                async move {
                    if let Some(gate) = gate {
                        if request.uri().path() == "/v1/events"
                            && gate.connections.fetch_add(1, Ordering::SeqCst) == 1
                        {
                            gate.blocked.notify_one();
                            tokio::time::timeout(Duration::from_secs(5), gate.release.notified())
                                .await
                                .unwrap();
                        }
                    }
                    next.run(request).await
                }
            },
        ));
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { keeper, task }
    }
    fn client(&self) -> Client {
        Client::new(&self.keeper.endpoint, Some(fake::FakeKeeper::tokens())).unwrap()
    }
    async fn hook(&self, suffix: &str, body: serde_json::Value) -> serde_json::Value {
        reqwest::Client::new()
            .post(format!("{}/_test/{suffix}", self.keeper.endpoint))
            .bearer_auth(fake::STATIC_TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap_or(serde_json::Value::Null)
    }
    async fn answers(&self) -> Vec<EventCommand> {
        reqwest::Client::new()
            .get(format!("{}/_test/answers", self.keeper.endpoint))
            .bearer_auth(fake::STATIC_TOKEN)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn prompt(connection: &mut EventConnection) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(Event::Prompt { id, .. })) = connection.events.recv().await {
                return id;
            }
        }
    })
    .await
    .unwrap()
}

async fn stale_answer_reconnect(gated: bool) {
    let gate = gated.then(|| Arc::new(HandshakeGate::default()));
    let fixture = Fixture::start(gate.clone()).await;
    let client = fixture.client();
    let host = client.add_host("auth-fixture").await.unwrap();
    let mut connection = client.events();
    // Wait for the first authenticated snapshot before publishing the prompt.
    tokio::time::timeout(Duration::from_secs(5), connection.events.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let wire = fixture.hook("prompt", serde_json::json!({"host_id":host.id,"prompt":"Synthetic fixture password?","echo":false})).await;
    let old = prompt(&mut connection).await;
    fixture.hook("drop-events", serde_json::Value::Null).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(connection.events.recv().await, Some(Err(_))) {}
    })
    .await
    .unwrap();
    if let Some(gate) = &gate {
        tokio::time::timeout(Duration::from_secs(5), gate.blocked.notified())
            .await
            .unwrap();
    } else {
        // The disconnect drain already ran; enqueue during reconnect backoff.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    connection
        .commands
        .send(EventCommand::Answer {
            id: old.clone(),
            value: Some("stale-synthetic-answer".into()),
        })
        .await
        .unwrap();
    if let Some(gate) = &gate {
        gate.release.notify_one();
    }
    let current = prompt(&mut connection).await;
    assert_ne!(
        old, current,
        "a replayed keeper prompt gets a new connection-local identifier"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        fixture.answers().await.is_empty(),
        "an old answer must not reach the keeper"
    );
    connection
        .answer(current.clone(), Some("current-synthetic-answer".into()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(connection.events.recv().await, Some(Ok(Event::PromptClosed { id })) if id == current) { break; }
        }
    }).await.unwrap();
    let answers = fixture.answers().await;
    assert_eq!(answers.len(), 1);
    let EventCommand::Answer { id, value } = &answers[0];
    assert_eq!(id, wire["id"].as_str().unwrap());
    assert!(value.as_deref() == Some("current-synthetic-answer"));
}
#[tokio::test]
async fn auth_answers_during_backoff_cannot_answer_a_reused_keeper_prompt() {
    stale_answer_reconnect(false).await;
}
#[tokio::test]
async fn auth_answers_during_socket_handshake_cannot_answer_a_reused_keeper_prompt() {
    stale_answer_reconnect(true).await;
}

async fn next_host(socket: &mut Socket) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text)))
                    if matches!(serde_json::from_str::<Event>(&text), Ok(Event::Host { .. })) =>
                {
                    break
                }
                Some(Ok(Message::Ping(data))) => socket.send(Message::Pong(data)).await.unwrap(),
                Some(Ok(Message::Pong(_))) => {}
                _ => panic!("device events socket ended before its host event"),
            }
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn replacing_one_devices_events_keeps_the_other_devices_stream_open() {
    let fixture = Fixture::start(None).await;
    let a = fixture.client();
    a.add_host("first-host").await.unwrap();
    let tokens = fixture.keeper.add_device("device-b").await.unwrap();
    let b = Client::new(&fixture.keeper.endpoint, Some(tokens.clone())).unwrap();
    let mut a_socket = a.open_socket(&["v1", "events"], true).await.unwrap();
    next_host(&mut a_socket).await;
    let mut b_socket = b.open_socket(&["v1", "events"], true).await.unwrap();
    next_host(&mut b_socket).await;
    // Rotation must retain B's identity rather than falling back to device A.
    let rotated: Tokens = reqwest::Client::new()
        .post(format!("{}/v1/oauth/refresh", fixture.keeper.endpoint))
        .json(&serde_json::json!({"refresh_token": tokens.refresh_token}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(rotated.access_token, tokens.access_token);
    let refreshed_b = Client::new(&fixture.keeper.endpoint, Some(rotated)).unwrap();
    let delegation = refreshed_b.delegate_daemon().await.unwrap();
    assert_eq!(delegation.device_id, "device-b");
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = format!(
        "{}/v1/events",
        fixture.keeper.endpoint.replace("http://", "ws://")
    )
    .into_client_request()
    .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", delegation.access_token)
            .parse()
            .unwrap(),
    );
    let (mut replacement, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    next_host(&mut replacement).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match b_socket.next().await {
                None | Some(Ok(Message::Close(_))) | Some(Err(_)) => break,
                Some(Ok(Message::Ping(data))) => {
                    let _ = b_socket.send(Message::Pong(data)).await;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    // Cross the former global-epoch heartbeat check, not only the upgrade.
    tokio::time::sleep(Duration::from_secs(21)).await;
    a.add_host("still-open").await.unwrap();
    next_host(&mut a_socket).await;
    next_host(&mut replacement).await;
}
