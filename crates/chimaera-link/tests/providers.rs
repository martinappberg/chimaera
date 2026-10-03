#![cfg(feature = "fixtures")]
use axum::{
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chimaera_link::{fake, providers::*, Client};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
const PARENT: &str = "12345678-1234-1234-1234-123456789abc";
const CHILD: &str = "12345678-1234-1234-1234-123456789abd";
const ATTEMPT: &str = "12345678-1234-1234-1234-123456789abe";
fn catalog() -> Value {
    json!({"providers_control":{"version":1,"account_id":"fixture","holder_id":"worker-one","process_boot":PARENT,"registration_generation":1,"worker_credential_digest":"a".repeat(64)},"connections":[{"provider":"claude","state":"disconnected","generation":2,"revision":0},{"provider":"codex","state":"disconnected","generation":0,"revision":0},{"provider":"github","state":"disconnected","generation":0,"revision":0}]})
}
fn attempt() -> Value {
    json!({"id":ATTEMPT,"provider_id":"claude","operation":"connect","phase":"waiting","expires_at":2000000000_u64,"action":{"type":"browser","url":"https://claude.com/oauth/authorize","input":"authorization_code"},"error_code":null,"control_version":1,"connection_generation":2,"credential_revision":0,"registration_generation":1})
}
fn original() -> Original {
    Original {
        context: "b".repeat(64),
        operation_id: PARENT.into(),
        provider: Provider::Claude,
        operation: Operation::Connect,
        expected_connection_generation: 2,
        registration: serde_json::from_value(catalog()["providers_control"].clone()).unwrap(),
        attempt_id: None,
    }
}
fn command() -> Command {
    Command {
        version: 1,
        operation_id: PARENT.into(),
        provider: Provider::Claude,
        expected_connection_generation: 2,
        command: Action::Connect {},
    }
}
#[test]
fn closed_capability_and_identity_validation_refuses_partial_or_rounded_authority() {
    let mut value = catalog();
    let parsed: Catalog = serde_json::from_value(value.clone()).unwrap();
    assert!(parsed.validate().is_ok());
    value["connections"][0]["generation"] = json!(COUNTER_MAX + 1);
    assert_eq!(
        serde_json::from_value::<Catalog>(value)
            .unwrap()
            .validate()
            .err(),
        Some(Error::Unsupported)
    );
    let mut value = catalog();
    value["connections"][2]["provider"] = json!("codex");
    assert_eq!(
        serde_json::from_value::<Catalog>(value)
            .unwrap()
            .validate()
            .err(),
        Some(Error::Unsupported)
    );
    let mut value = attempt();
    value.as_object_mut().unwrap().remove("error_code");
    assert!(serde_json::from_value::<Attempt>(value).is_err());
    let mut value = attempt();
    value["action"]["url"] = json!("https://attacker.invalid/authorize");
    assert_eq!(
        serde_json::from_value::<Attempt>(value)
            .unwrap()
            .validate(&original())
            .err(),
        Some(Error::Unconfirmed)
    );
}
#[test]
fn child_commands_never_reuse_parent_and_code_errors_are_fixed() {
    let mut parent = original();
    parent.attempt_id = Some(ATTEMPT.into());
    let mut child = Command {
        version: 1,
        operation_id: CHILD.into(),
        provider: Provider::Claude,
        expected_connection_generation: 2,
        command: Action::Submit {
            attempt_id: ATTEMPT.into(),
            submission_nonce: CHILD.into(),
            code: Code::new("synthetic-code".into()).unwrap(),
        },
    };
    assert!(child.validate_original(&parent).is_ok());
    child.operation_id = PARENT.into();
    assert_eq!(
        child.validate_original(&parent).err(),
        Some(Error::InvalidRequest)
    );
    let error = Command::decode_owned(json!({"version":1,"operation_id":CHILD,"provider":"claude","expected_connection_generation":2,"command":{"type":"connect","code":"synthetic-code"}}).to_string()).err().unwrap();
    assert_eq!(error, Error::InvalidRequest);
    assert!(!error.to_string().contains("synthetic"));
}
#[derive(Default)]
struct Control {
    legacy: AtomicBool,
    missing: AtomicBool,
    changed: AtomicBool,
    hold: AtomicBool,
    reject: AtomicBool,
    sent: AtomicUsize,
    status: AtomicUsize,
    release: tokio::sync::Notify,
    catalogs: AtomicUsize,
    modes: AtomicUsize,
    pause_final: AtomicBool,
    final_entered: tokio::sync::Notify,
    final_release: tokio::sync::Notify,
    exchange_replied: tokio::sync::Notify,
}
async fn mode(State(s): State<Arc<Control>>) -> axum::response::Response {
    if s.modes.fetch_add(1, Ordering::AcqRel) == 1 && s.pause_final.load(Ordering::Acquire) {
        s.final_entered.notify_one();
        s.final_release.notified().await;
    }
    if s.missing.load(Ordering::Acquire) {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    Json(json!({"version":1,"context": if s.changed.load(Ordering::Acquire) { "c".repeat(64) } else { "b".repeat(64) },"mode": if s.legacy.load(Ordering::Acquire) { "legacy" } else { "personal" }})).into_response()
}
async fn send(
    State(s): State<Arc<Control>>,
    headers: axum::http::HeaderMap,
    Json(value): Json<Value>,
) -> axum::response::Response {
    assert_eq!(
        headers.get("authorization").unwrap(),
        format!("Bearer {}", fake::STATIC_TOKEN).as_str()
    );
    assert!(value["operation_id"] == PARENT || value["operation_id"] == CHILD);
    s.sent.fetch_add(1, Ordering::AcqRel);
    if s.hold.load(Ordering::Acquire) {
        s.release.notified().await;
    }
    if s.reject.load(Ordering::Acquire) {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    axum::http::StatusCode::BAD_GATEWAY.into_response()
}
async fn status(State(s): State<Arc<Control>>) -> Json<Value> {
    s.status.fetch_add(1, Ordering::AcqRel);
    Json(attempt())
}
struct Fixture {
    endpoint: String,
    client: Client,
    state: Arc<Control>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let fake = fake::FakeKeeper::new(endpoint.clone());
        let state = Arc::new(Control::default());
        let routes = Router::new()
            .route("/v1/personal/providers/mode", get(mode))
            .route(
                "/v1/personal/providers",
                get(|State(s): State<Arc<Control>>| async move {
                    s.catalogs.fetch_add(1, Ordering::AcqRel);
                    Json(catalog())
                }),
            )
            .route("/v1/personal/providers/commands", post(send))
            .route("/v1/personal/providers/operations/{operation}", get(status))
            .with_state(state.clone());
        async fn exchange_seen(
            State(s): State<Arc<Control>>,
            request: axum::extract::Request,
            next: axum::middleware::Next,
        ) -> axum::response::Response {
            let exchange = request.uri().path() == "/v1/oauth/token";
            let response = next.run(request).await;
            if exchange {
                s.exchange_replied.notify_one();
            }
            response
        }
        let app = fake
            .router()
            .merge(routes)
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                exchange_seen,
            ));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            endpoint: endpoint.clone(),
            client: Client::new(&endpoint, Some(fake::FakeKeeper::tokens())).unwrap(),
            state,
            task,
        }
    }
}
#[tokio::test]
async fn mode_context_and_registration_refuse_before_any_effect() {
    let f = Fixture::start().await;
    f.state.legacy.store(true, Ordering::Release);
    assert_eq!(
        f.client
            .personal_provider_command(original(), command())
            .await
            .err(),
        Some(Error::Unsupported)
    );
    f.state.legacy.store(false, Ordering::Release);
    f.state.missing.store(true, Ordering::Release);
    assert_eq!(
        f.client
            .personal_provider_command(original(), command())
            .await
            .err(),
        Some(Error::Unsupported)
    );
    f.state.missing.store(false, Ordering::Release);
    f.state.changed.store(true, Ordering::Release);
    assert_eq!(
        f.client
            .personal_provider_command(original(), command())
            .await
            .err(),
        Some(Error::ContextChanged)
    );
    f.state.changed.store(false, Ordering::Release);
    let mut wrong = original();
    wrong.registration.holder_id = "worker-two".into();
    assert_eq!(
        f.client
            .personal_provider_command(wrong, command())
            .await
            .err(),
        Some(Error::StateChanged)
    );
    assert_eq!(f.state.sent.load(Ordering::Acquire), 0);
}
#[tokio::test]
async fn final_exact_bearer_owner_serializes_legitimate_account_replacement() {
    let f = Fixture::start().await;
    let pkce = chimaera_link::Pkce::new();
    let redirect = "http://127.0.0.1:49152/callback";
    let html = reqwest::Client::new()
        .get(pkce.authorization_url(&f.endpoint, redirect).unwrap())
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
    f.state.pause_final.store(true, Ordering::Release);
    let client = f.client.clone();
    let command_task = tokio::spawn(async move {
        client
            .personal_provider_command(original(), command())
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.state.final_entered.notified(),
    )
    .await
    .unwrap();
    let client = f.client.clone();
    let replacement = tokio::spawn(async move {
        client
            .exchange_code(chimaera_link::TokenRequest {
                grant_type: "authorization_code".into(),
                code,
                redirect_uri: redirect.into(),
                code_verifier: pkce.verifier,
                device_name: "Synthetic replacement".into(),
            })
            .await
    });
    // The actual account exchange has produced a new credential reply, but
    // publishing it must wait behind the existing final-check/send owner.
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.state.exchange_replied.notified(),
    )
    .await
    .unwrap();
    tokio::task::yield_now().await;
    assert!(!replacement.is_finished());
    assert_eq!(f.state.sent.load(Ordering::Acquire), 0);
    f.state.final_release.notify_one();
    assert_eq!(command_task.await.unwrap().err(), Some(Error::Unconfirmed));
    let tokens = tokio::time::timeout(std::time::Duration::from_secs(5), replacement)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_ne!(tokens.access_token, fake::STATIC_TOKEN);
    assert_eq!(f.state.sent.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn rejected_or_lost_child_reply_never_replays_and_parent_poll_recovers() {
    let f = Fixture::start().await;
    let mut parent = original();
    parent.attempt_id = Some(ATTEMPT.into());
    let child = Command {
        version: 1,
        operation_id: CHILD.into(),
        provider: Provider::Claude,
        expected_connection_generation: 2,
        command: Action::Submit {
            attempt_id: ATTEMPT.into(),
            submission_nonce: CHILD.into(),
            code: Code::new("synthetic-code".into()).unwrap(),
        },
    };
    f.state.reject.store(true, Ordering::Release);
    assert_eq!(
        f.client
            .personal_provider_command(parent.clone(), child)
            .await
            .err(),
        Some(Error::SignInRequired)
    );
    assert_eq!(f.state.sent.load(Ordering::Acquire), 1);
    let reply = f
        .client
        .personal_provider_operation(&parent, PARENT)
        .await
        .unwrap();
    assert_eq!(reply.operation_id, PARENT);
    assert_eq!(reply.attempt.id, ATTEMPT);
    assert_eq!(f.state.sent.load(Ordering::Acquire), 1);
    assert_eq!(
        f.client
            .personal_provider_operation(&parent, CHILD)
            .await
            .err(),
        Some(Error::InvalidRequest)
    );
    // A syntactically unusable accepted reply is also uncertain, without retry.
    let f = Fixture::start().await;
    assert_eq!(
        f.client
            .personal_provider_command(original(), command())
            .await
            .err(),
        Some(Error::Unconfirmed)
    );
    assert_eq!(f.state.sent.load(Ordering::Acquire), 1);
    assert_eq!(
        f.client
            .personal_provider_operation(&original(), PARENT)
            .await
            .unwrap()
            .attempt
            .id,
        ATTEMPT
    );
    assert_eq!(f.state.sent.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn canceled_observers_retain_all_four_writers_through_http_completion() {
    let f = Fixture::start().await;
    f.state.hold.store(true, Ordering::Release);
    for _ in 0..4 {
        let mut observer = Box::pin(f.client.personal_provider_command(original(), command()));
        assert!(futures::poll!(observer.as_mut()).is_pending());
        drop(observer);
    }
    assert_eq!(
        f.client
            .personal_provider_command(original(), command())
            .await
            .err(),
        Some(Error::LimitReached)
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while f.state.sent.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.state.hold.store(false, Ordering::Release);
    f.state.release.notify_waiters();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if f.client
                .personal_provider_command(original(), command())
                .await
                .err()
                != Some(Error::LimitReached)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
