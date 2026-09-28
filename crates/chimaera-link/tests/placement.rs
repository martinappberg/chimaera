#![cfg(feature = "fixtures")]
use axum::{
    extract::State,
    http::{Method, StatusCode},
    response::IntoResponse,
    Json, Router,
};
use chimaera_link::{Client, InstallationIdentity, Tokens};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
#[derive(Default)]
struct Fixture {
    calls: Mutex<Vec<(Method, String)>>,
    phase: AtomicUsize,
}
async fn handle(
    State(state): State<Arc<Fixture>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    assert_eq!(
        request.headers()["authorization"],
        "Bearer synthetic-access"
    );
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    state
        .calls
        .lock()
        .unwrap()
        .push((method.clone(), path.clone()));
    let phase = state.phase.load(Ordering::SeqCst);
    if path == "/v2/capabilities" {
        if phase == 9 {
            return StatusCode::NOT_FOUND.into_response();
        }
        return Json(json!({"execution_authority":2,"execution_capability":{"version":1,"boundary":"managed_processes","expired_takeover":false},"installation_binding":1,"workspace_placement":2,"checkpoint_receipts":1})).into_response();
    }
    if path == "/v2/workspaces/w-one/placement" {
        assert_eq!(method, Method::GET);
        return Json(json!({"workspace_id":"w-one","holder_id":"d-home","route_host_id":"device-d-home","epoch":4,"policy_revision":1,"availability":"owned","preferred_installation_id":"i-home","checkpoint_id":"cp-one","server_now":"2026-09-28T19:00:00Z","expires_at":"2026-09-28T19:01:30Z"})).into_response();
    }
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(request.into_body(), 16384)
            .await
            .unwrap(),
    )
    .unwrap();
    if path == "/v2/installations/bind" {
        assert_eq!(method, Method::POST);
        if phase == 1 {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"clean_release_required"})),
            )
                .into_response();
        }
        return Json(json!({"installation_id":if phase==2 {json!("i-other")} else {body["installation_id"].clone()},"device_id":"d-new"})).into_response();
    }
    if path.ends_with("/recovery") {
        assert_eq!(body["workspace_id"], "w-one");
        assert_eq!(body["expected_epoch"], 4);
        return Json(json!({"access_token":"synthetic-recovery","expires_at":"2099-01-01T00:00:00Z","scope":if phase==3 {json!(["mirror","release","proxy"])} else {json!(["mirror","release"])},"workspace_id":"w-one","holder_id":"d-home","epoch":4})).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
#[tokio::test]
async fn passive_placement_and_exact_installation_acknowledgments_never_infer_authority() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let state = Arc::new(Fixture::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(handle).with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = Client::new(
        &endpoint,
        Some(Tokens {
            access_token: "synthetic-access".into(),
            refresh_token: "synthetic-refresh".into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
        }),
    )
    .unwrap();
    client.execution_capabilities().await.unwrap();
    assert_eq!(
        client
            .workspace_placement("w-one")
            .await
            .unwrap()
            .route_host_id
            .as_deref(),
        Some("device-d-home")
    );
    assert!(state
        .calls
        .lock()
        .unwrap()
        .iter()
        .all(|(method, _)| *method == Method::GET));
    let identity = InstallationIdentity::generate();
    state.phase.store(1, Ordering::SeqCst);
    assert!(client
        .bind_installation(&identity)
        .await
        .unwrap_err()
        .is::<chimaera_link::CleanReleaseRequired>());
    state.phase.store(2, Ordering::SeqCst);
    assert!(client.bind_installation(&identity).await.is_err());
    state.phase.store(0, Ordering::SeqCst);
    assert_eq!(
        client.bind_installation(&identity).await.unwrap().device_id,
        "d-new"
    );
    let grant = client
        .installation_recovery(&identity, "w-one", "d-home", 4)
        .await
        .unwrap();
    assert_eq!(grant.workspace_id, "w-one");
    state.phase.store(3, Ordering::SeqCst);
    assert!(client
        .installation_recovery(&identity, "w-one", "d-home", 4)
        .await
        .is_err());
    state.phase.store(9, Ordering::SeqCst);
    assert!(client.execution_capabilities().await.is_err());
    assert_eq!(
        state.calls.lock().unwrap().len(),
        8,
        "no fallback, acquire or automatic retry"
    );
    task.abort();
}
