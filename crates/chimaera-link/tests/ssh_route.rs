#![cfg(feature = "fixtures")]
//! Wire/correlation fixture only: no SSH processes, keys, or capability rollout.
use axum::{
    extract::{ws::WebSocketUpgrade, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chimaera_link::*;
use futures::StreamExt;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Data {
    base: String,
    mode: usize,
    writes: AtomicUsize,
    reconnects: AtomicUsize,
}
struct Fixture {
    data: Arc<Data>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn start(mode: usize) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let data = Arc::new(Data {
            base: format!("http://{}", listener.local_addr().unwrap()),
            mode,
            writes: AtomicUsize::new(0),
            reconnects: AtomicUsize::new(0),
        });
        let router = Router::new()
            .route("/v1/me", get(me))
            .route("/v1/ssh/auth/capabilities", get(capabilities))
            .route("/v1/hosts", post(register))
            .route("/v1/hosts/host/ssh/auth/route-grants", post(grant))
            .route("/v1/hosts/host/ssh/auth/route-grants/grant/ws", get(socket))
            .route(
                "/v1/hosts/host/ssh/auth/route-grants/grant",
                axum::routing::delete(delete_grant),
            )
            .route("/v1/hosts/host/reconnect", post(reconnect))
            .with_state(data.clone());
        Self {
            data,
            task: tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
        }
    }
    fn client(&self) -> Client {
        Client::new(
            &self.data.base,
            Some(Tokens {
                access_token: "fixture".into(),
                refresh_token: "refresh".into(),
                token_type: "Bearer".into(),
                expires_in: 3600,
            }),
        )
        .unwrap()
    }
}
fn authed(headers: &HeaderMap) {
    assert_eq!(headers.get("authorization").unwrap(), "Bearer fixture");
}
async fn me(State(data): State<Arc<Data>>, headers: HeaderMap) -> Json<Value> {
    authed(&headers);
    Json(
        json!({"account_id":"account","email":"fixture@example.invalid","plan":"pro","device_id":"device","protocol":0,"keeper_url":data.base,"limits":{"cloud_hours":1,"storage_bytes":1},"usage":{"cloud_hours":0,"storage_bytes":0},"hours_exhausted":false}),
    )
}
async fn capabilities(State(data): State<Arc<Data>>, headers: HeaderMap) -> Json<Value> {
    authed(&headers);
    let mut value =
        json!({"version":1,"hostbound_v1":true,"register_only_v1":true,"keeper_boot":"boot"});
    if data.mode != 0 {
        value["proxyjump_v1"] = json!(true);
    }
    if data.mode >= 9 {
        value["route_policy_v1"] = json!(true);
    }
    Json(value)
}
async fn register(
    State(data): State<Arc<Data>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    authed(&headers);
    data.writes.fetch_add(1, Ordering::SeqCst);
    assert_eq!(body["register_only"], true);
    assert_eq!(body["ssh_route"]["jumps"].as_array().unwrap().len(), 1);
    let mut value = json!({"id":"host","alias":body["alias"],"kind":"ssh","status":"offline","daemon":null,"error":null,"ssh":body["ssh"],"ssh_route":body["ssh_route"]});
    match data.mode {
        2 => {
            value.as_object_mut().unwrap().remove("ssh_route");
        }
        3 => {
            value["ssh_route"]["jumps"][0]["port"] = json!(23);
        }
        _ => {}
    }
    Json(value)
}
async fn grant(
    State(data): State<Arc<Data>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    authed(&headers);
    data.writes.fetch_add(1, Ordering::SeqCst);
    if data.mode == 8 {
        return StatusCode::NOT_FOUND.into_response();
    }
    let modes: Vec<Value> = body["legs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|leg| leg["mode"].clone())
        .collect();
    let mut value = json!({"version":1,"grant_id":"grant","expires_in":180,"destination":body["destination"],"route":body["route"],"modes":modes});
    if data.mode >= 9 && data.mode != 10 {
        value["policies"] = Value::Array(
            body["legs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|leg| leg["policy"].clone())
                .collect(),
        );
    }
    if data.mode == 11 {
        value["policies"][1]["methods"] = json!(["publickey"]);
    }
    match data.mode {
        4 => {
            value["modes"] = json!(["key", "interactive"]);
        }
        5 => {
            value["destination"]["user"] = json!("other");
        }
        _ => {}
    }
    Json(value).into_response()
}
async fn socket(
    State(data): State<Arc<Data>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    authed(&headers);
    ws.on_upgrade(move |mut socket| async move {
        let ready = json!({"type":"ready","version":1,"grant_id":"grant","keeper_boot":if data.mode == 6 {"other"} else {"boot"},"legs":if data.mode == 7 {1} else {2}});
        socket.send(axum::extract::ws::Message::Text(ready.to_string().into())).await.unwrap();
        while let Some(Ok(_)) = socket.next().await {}
    })
}
async fn delete_grant(headers: HeaderMap) -> StatusCode {
    authed(&headers);
    StatusCode::NO_CONTENT
}
async fn reconnect(State(data): State<Arc<Data>>, headers: HeaderMap) -> StatusCode {
    authed(&headers);
    assert_eq!(headers.get(SSH_AUTH_ROUTE_GRANT_HEADER).unwrap(), "grant");
    assert!(headers.get(SSH_AUTH_GRANT_HEADER).is_none());
    data.reconnects.fetch_add(1, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}
fn request() -> SshRouteGrantRequest {
    let jump = SshAuthDestination {
        hostname: "jump.example.invalid".into(),
        user: "visitor".into(),
        port: 2222,
    };
    let destination = SshAuthDestination {
        hostname: "target.example.invalid".into(),
        user: "person".into(),
        port: 22,
    };
    SshRouteGrantRequest {
        version: 1,
        keeper_boot: "boot".into(),
        destination: destination.clone(),
        route: SshRoute {
            version: 1,
            jumps: vec![jump.clone()],
        },
        legs: vec![
            SshRouteAuthLeg {
                policy: None,
                destination: jump,
                mode: SshRouteMode::Interactive,
                host_keys: vec![SshAuthHostKey {
                    key: "AQ==".into(),
                    is_ca: false,
                }],
                user_keys: vec![],
            },
            SshRouteAuthLeg {
                policy: None,
                destination,
                mode: SshRouteMode::Key,
                host_keys: vec![SshAuthHostKey {
                    key: "Ag==".into(),
                    is_ca: false,
                }],
                user_keys: vec!["Aw==".into()],
            },
        ],
    }
}
fn target(request: &SshRouteGrantRequest) -> SshTarget {
    SshTarget {
        hostname: request.destination.hostname.clone(),
        user: Some(request.destination.user.clone()),
        port: request.destination.port,
    }
}
fn with_policy(mut request: SshRouteGrantRequest) -> SshRouteGrantRequest {
    for leg in &mut request.legs {
        leg.policy = Some(SshRoutePolicy {
            version: 1,
            methods: if leg.mode == SshRouteMode::Key {
                vec![SshRouteMethod::Publickey, SshRouteMethod::Password]
            } else {
                vec![SshRouteMethod::KeyboardInteractive]
            },
            host_key_algorithms: vec!["ssh-ed25519".into()],
            ca_signature_algorithms: vec!["ssh-ed25519".into()],
            pubkey_accepted_algorithms: vec!["ssh-ed25519".into()],
            kex_algorithms: vec!["curve25519-sha256".into()],
            ciphers: vec!["chacha20-poly1305@openssh.com".into()],
            macs: vec!["hmac-sha2-256-etm@openssh.com".into()],
        });
    }
    request
}
#[tokio::test]
async fn policy_capability_precedes_effects_and_exact_ack_precedes_reconnect() {
    for mode in [0, 1] {
        let f = Fixture::start(mode).await;
        let request = with_policy(request());
        let client = f.client();
        assert!(client
            .register_ssh_policy_route_host(
                "cluster",
                target(&request),
                request.route.clone(),
                None
            )
            .await
            .err()
            .unwrap()
            .is::<ServiceUnsupported>());
        assert!(client
            .create_ssh_route_grant("host", &request)
            .await
            .err()
            .unwrap()
            .is::<ServiceUnsupported>());
        assert_eq!(f.data.writes.load(Ordering::SeqCst), 0);
    }
    for mode in [9, 10, 11] {
        let f = Fixture::start(mode).await;
        let request = with_policy(request());
        let client = f.client();
        let result = client.create_ssh_route_grant("host", &request).await;
        if mode == 9 {
            let grant = result.unwrap();
            assert!(grant.matches(&request));
            let _socket = client
                .ssh_route_socket("host", &grant, "boot")
                .await
                .unwrap();
            client
                .reconnect_host_with_ssh_route("host", &grant, "boot")
                .await
                .unwrap();
            assert_eq!(f.data.reconnects.load(Ordering::SeqCst), 1);
        } else {
            assert!(result.is_err());
            assert_eq!(f.data.reconnects.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn old_service_refuses_route_before_registration_or_grant_effects() {
    let f = Fixture::start(0).await;
    let request = request();
    let client = f.client();
    assert!(client
        .register_ssh_route_host("cluster", target(&request), request.route.clone(), None)
        .await
        .err()
        .unwrap()
        .is::<ServiceUnsupported>());
    assert!(client
        .create_ssh_route_grant("host", &request)
        .await
        .err()
        .unwrap()
        .is::<ServiceUnsupported>());
    assert_eq!(f.data.writes.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn registration_and_grant_require_positive_exact_route_acknowledgments() {
    for mode in [2, 3] {
        let f = Fixture::start(mode).await;
        let request = request();
        assert!(f
            .client()
            .register_ssh_route_host("cluster", target(&request), request.route, None)
            .await
            .is_err());
        assert_eq!(f.data.writes.load(Ordering::SeqCst), 1);
    }
    for mode in [4, 5, 8] {
        let f = Fixture::start(mode).await;
        let error = f
            .client()
            .create_ssh_route_grant("host", &request())
            .await
            .err()
            .unwrap();
        if mode == 8 {
            assert!(error.is::<ServiceUnsupported>());
        }
        assert_eq!(f.data.writes.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn ready_is_server_first_exact_and_reconnect_header_is_route_only() {
    for mode in [1, 6, 7] {
        let f = Fixture::start(mode).await;
        let client = f.client();
        let request = request();
        let host = client
            .register_ssh_route_host("cluster", target(&request), request.route.clone(), None)
            .await
            .unwrap();
        assert_eq!(host.host.id, "host");
        let grant = client
            .create_ssh_route_grant("host", &request)
            .await
            .unwrap();
        let socket = client.ssh_route_socket("host", &grant, "boot").await;
        if mode == 1 {
            let _socket = socket.unwrap();
            client
                .reconnect_host_with_ssh_route("host", &grant, "boot")
                .await
                .unwrap();
            assert_eq!(f.data.reconnects.load(Ordering::SeqCst), 1);
            client.delete_ssh_route_grant("host", &grant).await.unwrap();
        } else {
            assert!(socket.is_err());
            assert_eq!(f.data.reconnects.load(Ordering::SeqCst), 0);
        }
        assert!(client
            .reconnect_host_with_ssh_route("host", &grant, "other")
            .await
            .is_err());
    }
}
