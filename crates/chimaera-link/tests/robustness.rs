#![cfg(feature = "fixtures")]
//! Contract evolution against loopback services: additive fields and values
//! are tolerated, an unsupported service is a typed error, and one message
//! the client cannot act on never drops a whole reverse connection.
use axum::{
    extract::{
        ws::{Message as AxumMessage, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chimaera_link::*;
use futures::StreamExt;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::net::TcpListener;

fn tokens() -> Tokens {
    Tokens {
        access_token: "synthetic-access".into(),
        refresh_token: "synthetic-refresh".into(),
        token_type: "Bearer".into(),
        expires_in: 900,
    }
}

async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (endpoint, task)
}

fn account(endpoint: &str, plan: &str, protocol: u32) -> Value {
    json!({"account_id":"a","email":"a@example.invalid","plan":plan,"device_id":"d-fixture",
        "protocol":protocol,"keeper_url":endpoint,"limits":{"cloud_hours":1,"storage_bytes":1},
        "usage":{"cloud_hours":0,"storage_bytes":0},"hours_exhausted":false,
        "introduced_later":{"any":"shape"}})
}

#[tokio::test]
async fn an_unsupported_service_is_a_typed_error_not_a_retry() {
    let endpoint = Arc::new(Mutex::new(String::new()));
    let me_endpoint = endpoint.clone();
    let router = Router::new()
        .route("/v2/capabilities", get(|| async { StatusCode::NOT_FOUND }))
        .route(
            "/v2/workspaces/{id}/placement",
            get(|| async { StatusCode::NOT_FOUND }),
        )
        .route(
            "/v1/me",
            get(move || {
                let endpoint = me_endpoint.lock().unwrap().clone();
                async move { Json(account(&endpoint, "pro", 1)) }
            }),
        );
    let (origin, task) = serve(router).await;
    *endpoint.lock().unwrap() = origin.clone();
    let client = Client::new(&origin, Some(tokens())).unwrap();
    for error in [
        client.execution_capabilities().await.unwrap_err(),
        client.workspace_placement("w-a").await.unwrap_err(),
        client.me().await.unwrap_err(),
    ] {
        assert!(error.is::<ServiceUnsupported>(), "{error:#}");
    }
    task.abort();
}

#[tokio::test]
async fn additive_fields_and_unknown_values_never_fail_a_service_response() {
    let endpoint = Arc::new(Mutex::new(String::new()));
    let me_endpoint = endpoint.clone();
    let default = Arc::new(Mutex::new(
        json!({"version":2,"boundary":"canonical_checkpoint","expired_takeover":true,"added":"later"}),
    ));
    let advertised = default.clone();
    let router = Router::new()
        .route(
            "/v2/capabilities",
            get(move || {
                let default = advertised.lock().unwrap().clone();
                async move {
                    Json(json!({"execution_authority":2,"execution_capability":default,
                        "supported_execution_capabilities":[
                            {"version":1,"boundary":"managed_processes","expired_takeover":false},
                            {"version":2,"boundary":"canonical_checkpoint","expired_takeover":true},
                            {"version":3,"boundary":"future","expired_takeover":true,"mode":"new"}],
                        "failover_grace_seconds":30,"installation_binding":1,
                        "workspace_placement":2,"checkpoint_receipts":1,"new_capability":7}))
                }
            }),
        )
        .route(
            "/v1/me",
            get(move || {
                let endpoint = me_endpoint.lock().unwrap().clone();
                async move { Json(account(&endpoint, "team", 0)) }
            }),
        )
        .route(
            "/v1/worker/status",
            get(|| async {
                Json(json!({"state":"hibernating","reason":"new_reason","phase":"warming","eta":3}))
            }),
        )
        .route(
            "/v1/hosts",
            get(|| async {
                Json(json!([
                    {"id":"h-a","alias":"cluster","kind":"ssh","status":"sleeping","daemon":null,"error":null,"region":"x"},
                    {"id":"h-b","alias":"watch","kind":"phone","status":"connected","daemon":null,"error":null}
                ]))
            }),
        )
        .route(
            "/v2/workspaces/{id}/placement",
            get(|Path(id): Path<String>| async move {
                Json(json!({"workspace_id":id,"holder_id":"m-cloud","route_host_id":null,"epoch":3,
                    "policy_revision":1,"availability":"archived","preferred_installation_id":null,
                    "checkpoint_id":null,"server_now":"2026-09-28T19:00:00Z","expires_at":null,"wake":"soon"}))
            }),
        );
    let (origin, task) = serve(router).await;
    *endpoint.lock().unwrap() = origin.clone();
    let client = Client::new(&origin, Some(tokens())).unwrap();

    let capabilities = client.execution_capabilities().await.unwrap();
    assert_eq!(
        capabilities.selected(),
        Some(ExecutionCapability::checkpoint_fork())
    );
    // A default this client does not implement: pick what both support.
    *default.lock().unwrap() = json!({"version":3,"boundary":"future","expired_takeover":true});
    assert_eq!(
        client.execution_capabilities().await.unwrap().selected(),
        Some(ExecutionCapability::checkpoint_fork())
    );

    assert_eq!(client.me().await.unwrap().plan, Plan::Unknown);
    let status = client.worker_status().await.unwrap();
    assert_eq!(status.state, WorkerState::Unknown);
    assert_eq!(status.reason, Some(WorkerReason::Unknown));
    assert_eq!(status.phase, Some(WorkerPhase::Unknown));
    let hosts = client.hosts().await.unwrap();
    assert_eq!(
        hosts.len(),
        1,
        "a host kind this client cannot use is dropped"
    );
    assert_eq!(hosts[0].status, HostStatus::Unknown);
    let placement = client.workspace_placement("w-a").await.unwrap();
    assert_eq!(placement.availability, PlacementAvailability::Unknown);
    assert!(placement.route_host_id.is_none());
    task.abort();
}

#[derive(Default)]
struct Keeper {
    controls: AtomicUsize,
    opened: Mutex<Vec<String>>,
}

async fn control(State(keeper): State<Arc<Keeper>>, ws: WebSocketUpgrade) -> Response {
    keeper.controls.fetch_add(1, Ordering::SeqCst);
    ws.on_upgrade(|mut socket| async move {
        let Some(Ok(AxumMessage::Text(_register))) = socket.next().await else {
            return;
        };
        for frame in [
            json!({"type":"registered","host_id":"device-d-fixture"}),
            json!({"type":"introduced_later","detail":1}),
            json!({"type":"open"}),
            json!({"type":"open","stream_id":"s-one"}),
            json!({"type":"open","stream_id":"s-one"}),
            json!({"type":"open","stream_id":"s-two"}),
        ] {
            socket
                .send(AxumMessage::Text(frame.to_string().into()))
                .await
                .unwrap();
        }
        // Hold the control connection open like a live keeper.
        while let Some(Ok(message)) = socket.next().await {
            if let AxumMessage::Ping(data) = message {
                let _ = socket.send(AxumMessage::Pong(data)).await;
            }
        }
    })
}

async fn reverse(
    State(keeper): State<Arc<Keeper>>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    keeper.opened.lock().unwrap().push(id);
    ws.on_upgrade(|socket| async move {
        let _socket = socket;
        tokio::time::sleep(Duration::from_secs(30)).await;
    })
}

#[tokio::test]
async fn one_message_the_client_cannot_act_on_never_drops_reverse_serve() {
    let daemon = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let daemon_port = daemon.local_addr().unwrap().port();
    let daemon_task = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = daemon.accept().await {
            held.push(stream);
        }
    });
    let keeper = Arc::new(Keeper::default());
    let endpoint = Arc::new(Mutex::new(String::new()));
    let me_endpoint = endpoint.clone();
    let router = Router::new()
        .route(
            "/v1/me",
            get(move || {
                let endpoint = me_endpoint.lock().unwrap().clone();
                async move { Json(account(&endpoint, "pro", 0)) }
            }),
        )
        .route("/v1/serve", get(control))
        .route("/v1/serve/{id}", get(reverse))
        .with_state(keeper.clone());
    let (origin, task) = serve(router).await;
    *endpoint.lock().unwrap() = origin.clone();
    let client = Client::new(&origin, Some(tokens())).unwrap();
    let serving = Serve::start(
        client,
        daemon_port,
        "fixture".into(),
        Daemon {
            token: "synthetic-daemon".into(),
            build: "fixture".into(),
            sessions: 0,
        },
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while keeper.opened.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("streams after an unknown message still open");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut opened = keeper.opened.lock().unwrap().clone();
    opened.sort();
    assert_eq!(opened, ["s-one", "s-two"], "a duplicate id opens once");
    assert_eq!(
        keeper.controls.load(Ordering::SeqCst),
        1,
        "the control connection was never dropped and re-registered"
    );
    serving.close();
    task.abort();
    daemon_task.abort();
}

#[tokio::test]
async fn a_slow_events_consumer_is_resynchronized_not_abandoned() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let keeper = fake::FakeKeeper::new(format!("http://{}", listener.local_addr().unwrap()));
    let router = keeper.router();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = Client::new(&keeper.endpoint, Some(fake::FakeKeeper::tokens())).unwrap();
    // More hosts than the 64-event queue holds, so the snapshot blocks.
    for index in 0..80 {
        client.add_host(&format!("host-{index}")).await.unwrap();
    }
    let mut events = client.events();
    // Stall past both of the link's 10 s delivery deadlines (the queued
    // events, then its error report): the old loop exited for good here.
    tokio::time::sleep(Duration::from_secs(21)).await;
    let mut received = 0;
    while received <= 64 {
        match tokio::time::timeout(Duration::from_secs(10), events.events.recv()).await {
            Ok(Some(_)) => received += 1,
            Ok(None) => panic!("the events task ended after {received} events"),
            Err(_) => panic!("no resynchronization after {received} events"),
        }
    }
    task.abort();
}
