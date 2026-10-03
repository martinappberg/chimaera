#![cfg(feature = "fixtures")]
use axum::{
    extract::State as AxumState,
    routing::{get, post},
    Json as Reply, Router,
};
use chimaera_link::{fake, project_secrets::*, Client};
use serde_json::{json, Value as Json};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
const OP: &str = "12345678-1234-1234-1234-123456789abc";
fn command(action: &str) -> Json {
    let mut value = json!({"version":1,"operation_id":OP,"workspace_id":"w-one","expected_revision":2,"expected_pending":null,"action":action});
    if action == "set" {
        value["name"] = json!("DATABASE_URL");
        value["value"] = json!("synthetic-custom-value");
    }
    value
}
fn catalog() -> Json {
    json!({"version":1,"project_secrets":1,"name_policy":{"max_name_bytes":128,"max_value_bytes":8192,"max_names":32,"reserved_names":["HOME"],"reserved_prefixes":["GIT_"]},"projects":[{"workspace_id":"w-one","revision":2,"applied_names":[],"pending":null,"state":"ready"}],"next":null})
}
fn receipt() -> Json {
    json!({"version":1,"operation_id":OP,"workspace_id":"w-one","base_revision":2,"result_revision":null,"names":["DATABASE_URL"],"outcome":"queued"})
}

#[test]
fn commands_require_closed_fields_and_explicit_pending_and_bound_values() {
    for action in ["apply", "cancel", "remove"] {
        let mut request = command(action);
        request["expected_pending"] = json!("12345678-1234-1234-1234-123456789abd");
        if action == "remove" {
            request["name"] = json!("DATABASE_URL");
        }
        assert!(serde_json::from_value::<Command>(request.clone()).is_ok());
        request["hidden_field"] = json!(true);
        assert!(serde_json::from_value::<Command>(request.clone()).is_err());
        request.as_object_mut().unwrap().remove("hidden_field");
        request.as_object_mut().unwrap().remove("expected_pending");
        assert!(serde_json::from_value::<Command>(request).is_err());
    }
    for value in [String::new(), "x".repeat(8193), "x\0y".into()] {
        let mut request = command("set");
        request["value"] = json!(value);
        assert!(serde_json::from_value::<Command>(request).is_err());
    }
}
#[test]
fn fresh_catalog_conflicts_reserved_names_and_changed_receipts_refuse() {
    let page: Catalog = serde_json::from_value(catalog()).unwrap();
    let request: Command = serde_json::from_value(command("set")).unwrap();
    let names = request.validate_catalog(&page).unwrap();
    let ack: Receipt = serde_json::from_value(receipt()).unwrap();
    assert!(request.validate_receipt(&ack, &names).is_ok());
    let mut changed = page.clone();
    changed.projects[0].revision = 3;
    assert_eq!(
        request.validate_catalog(&changed).err(),
        Some(Error::StateChanged)
    );
    let mut reserved = command("set");
    reserved["name"] = json!("HOME");
    assert_eq!(
        serde_json::from_value::<Command>(reserved)
            .unwrap()
            .validate_catalog(&page)
            .err(),
        Some(Error::InvalidRequest)
    );
    let mut wrong = ack.clone();
    wrong.names = vec!["UNSEEN".into()];
    assert_eq!(
        request.validate_receipt(&wrong, &names),
        Err(Error::Unconfirmed)
    );
    let mut wrong = ack;
    wrong.workspace_id = "w-other".into();
    assert_eq!(
        request.validate_receipt(&wrong, &names),
        Err(Error::Unconfirmed)
    );
}

#[derive(Default)]
struct Control {
    supported: AtomicBool,
    sent: AtomicUsize,
    hold: AtomicBool,
    release: tokio::sync::Notify,
    context_mode: AtomicUsize,
}
async fn context_reply(AxumState(state): AxumState<Arc<Control>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    match state.context_mode.load(Ordering::Acquire) {
        1 => Reply(json!({"version":1,"context":"a".repeat(64)})).into_response(),
        2 => {
            Reply(json!({"version":1,"context":"a".repeat(64),"token":"synthetic-never-returned"}))
                .into_response()
        }
        3 => Reply(json!({"version":1,"context":"a".repeat(63)})).into_response(),
        _ => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}
async fn list(AxumState(state): AxumState<Arc<Control>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !state.supported.load(Ordering::Acquire) {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    Reply(catalog()).into_response()
}
async fn submit(
    AxumState(state): AxumState<Arc<Control>>,
    Reply(body): Reply<Json>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    assert_eq!(body["value"], "synthetic-custom-value");
    state.sent.fetch_add(1, Ordering::AcqRel);
    if state.hold.load(Ordering::Acquire) {
        state.release.notified().await;
    }
    // An accepted request with an unusable reply is never safe to resend.
    (axum::http::StatusCode::BAD_GATEWAY, "lost reply").into_response()
}
struct Fixture {
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
        let extra = Router::new()
            .route("/v1/personal/control-context", get(context_reply))
            .route("/v1/personal/project-secrets", get(list))
            .route("/v1/personal/project-secrets/commands", post(submit))
            .route(
                "/v1/personal/project-secrets/operations/{operation}",
                get(|| async { Reply(receipt()) }),
            )
            .with_state(state.clone());
        let app = fake.router().merge(extra);
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            client: Client::new(&endpoint, Some(fake::FakeKeeper::tokens())).unwrap(),
            state,
            task,
        }
    }
}
#[tokio::test]
async fn additive_account_context_is_closed_and_never_authorizes_a_command() {
    let f = Fixture::start().await;
    assert_eq!(
        f.client.personal_control_context().await.err(),
        Some(Error::Unsupported)
    );
    f.state.context_mode.store(1, Ordering::Release);
    let context = f.client.personal_control_context().await.unwrap();
    assert_eq!(context.version, 1);
    assert_eq!(context.context, "a".repeat(64));
    assert_eq!(f.state.sent.load(Ordering::Acquire), 0);
    f.state.context_mode.store(2, Ordering::Release);
    let rejected = f.client.personal_control_context().await.err().unwrap();
    assert_eq!(rejected, Error::Unconfirmed);
    assert!(!rejected.to_string().contains("synthetic-never-returned"));
    f.state.context_mode.store(3, Ordering::Release);
    assert_eq!(
        f.client.personal_control_context().await.err(),
        Some(Error::Unsupported)
    );
    assert_eq!(f.state.sent.load(Ordering::Acquire), 0);
}
#[tokio::test]
async fn one_page_stays_canonical_and_invalid_or_reversed_cursors_refuse() {
    let f = Fixture::start().await;
    f.state.supported.store(true, Ordering::Release);
    let page = f.client.project_secrets_page(None).await.unwrap();
    assert!(page.projects.len() <= 64);
    assert_eq!(page.projects[0].workspace_id, "w-one");
    assert_eq!(
        f.client.project_secrets_page(Some("w-one")).await.err(),
        Some(Error::Unsupported)
    );
    assert_eq!(
        f.client.project_secrets_page(Some("../other")).await.err(),
        Some(Error::InvalidRequest)
    );
    assert_eq!(f.state.sent.load(Ordering::Acquire), 0);
}
#[test]
fn ipc_owned_parser_has_fixed_errors_and_preserves_the_closed_command() {
    assert!(Command::decode_owned(command("set").to_string()).is_ok());
    let mut invalid = command("set");
    invalid["synthetic-secret-field"] = json!("synthetic-value");
    let error = Command::decode_owned(invalid.to_string()).err().unwrap();
    assert_eq!(error, Error::InvalidRequest);
    assert!(!error.to_string().contains("synthetic"));
    assert_eq!(
        Command::decode_owned(" ".repeat(COMMAND_MAX + 1)).err(),
        Some(Error::InvalidRequest)
    );
}
#[tokio::test]
async fn capability_is_fresh_and_lost_reply_never_resubmits_the_value() {
    let f = Fixture::start().await;
    assert_eq!(
        f.client
            .project_secret_command(serde_json::from_value(command("set")).unwrap())
            .await
            .err(),
        Some(Error::Unsupported)
    );
    assert_eq!(f.state.sent.load(Ordering::Acquire), 0);
    f.state.supported.store(true, Ordering::Release);
    assert_eq!(
        f.client
            .project_secret_command(serde_json::from_value(command("set")).unwrap())
            .await
            .err(),
        Some(Error::Unconfirmed)
    );
    assert_eq!(f.state.sent.load(Ordering::Acquire), 1);
    let ack = f.client.project_secret_operation(OP).await.unwrap();
    assert_eq!(ack.operation_id, OP);
    assert_eq!(f.state.sent.load(Ordering::Acquire), 1);
    f.state.supported.store(false, Ordering::Release);
    assert_eq!(
        f.client
            .project_secret_command(serde_json::from_value(command("set")).unwrap())
            .await
            .err(),
        Some(Error::Unsupported)
    );
    assert_eq!(f.state.sent.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn canceled_observers_do_not_release_four_actual_writer_owners() {
    let f = Fixture::start().await;
    f.state.supported.store(true, Ordering::Release);
    f.state.hold.store(true, Ordering::Release);
    for n in 1..=4 {
        let client = f.client.clone();
        let observer = tokio::spawn(async move {
            client
                .project_secret_command(serde_json::from_value(command("set")).unwrap())
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while f.state.sent.load(Ordering::Acquire) < n {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        observer.abort();
        let _ = observer.await;
    }
    assert_eq!(
        f.client
            .project_secret_command(serde_json::from_value(command("set")).unwrap())
            .await
            .err(),
        Some(Error::LimitReached)
    );
    f.state.hold.store(false, Ordering::Release);
    f.state.release.notify_waiters();
    // A subsequent request eventually admits only after an actual prior owner
    // completed. It still sends once and its lost reply stays unconfirmed.
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let result = f
                .client
                .project_secret_command(serde_json::from_value(command("set")).unwrap())
                .await;
            if result.err() == Some(Error::Unconfirmed) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.state.sent.load(Ordering::Acquire), 5);
}
