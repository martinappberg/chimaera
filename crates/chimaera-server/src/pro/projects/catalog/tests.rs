use super::*;
use axum::{body::Body, extract::State, http::Request, routing::any, Router};
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Service {
    mode: AtomicUsize,
    requests: std::sync::Mutex<Vec<String>>,
}
async fn serve(State(service): State<Arc<Service>>, request: Request<Body>) -> Response {
    let path = request.uri().to_string();
    lock(&service.requests).push(format!("{} {path}", request.method()));
    let mode = service.mode.load(Ordering::Acquire);
    let reply = |value: serde_json::Value| Json(value).into_response();
    match path.as_str() {
        "/v2/capabilities" if mode == 4 => StatusCode::NOT_FOUND.into_response(),
        "/v2/capabilities" => reply(json!({"project_catalog":if mode == 6 {2} else {1}})),
        "/v2/projects" if mode == 1 => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        "/v2/projects" if mode == 5 => StatusCode::NOT_FOUND.into_response(),
        "/v2/projects" if mode == 7 => {
            reply(json!({"catalog_version":1,"projects":[],"next_cursor":null}))
        }
        "/v2/projects" if mode == 2 => reply(
            json!({"catalog_version":1,"projects":[{"workspace_id":"w-copy","name":"unsafe\nname","epoch":4,"checkpoint_id":"c-one"}],"next_cursor":null}),
        ),
        "/v2/projects" if mode == 3 => {
            reply(json!({"catalog_version":1,"projects":[],"next_cursor":"../bad"}))
        }
        "/v2/projects" if mode == 8 => reply(
            json!({"catalog_version":1,"projects":(0..128).map(|index|json!({"workspace_id":format!("w-{index:03}"),"name":"Published","epoch":4,"checkpoint_id":"c-one"})).collect::<Vec<_>>(),"next_cursor":"w-127"}),
        ),
        // Legacy metadata rows can consume a whole page without visible rows.
        "/v2/projects" => {
            reply(json!({"catalog_version":1,"projects":[],"next_cursor":"w-before"}))
        }
        "/v2/projects?after=w-before" => reply(
            json!({"catalog_version":1,"projects":[{"workspace_id":"w-copy","name":"A published project å","epoch":4,"checkpoint_id":"c-one"}],"next_cursor":null}),
        ),
        "/v1/hosts" => reply(json!([])),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn fixture() -> (
    Arc<AppState>,
    Arc<Service>,
    tokio::task::JoinHandle<()>,
    PathBuf,
) {
    let root = std::env::temp_dir().join(format!(
        "chimaera-project-catalog-{}",
        chimaera_core::generate_token()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let state = Arc::new(AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.clone(),
        root.join("config"),
    ));
    let service = Arc::new(Service::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    super::super::super::moves::device_fixture(&state, &origin);
    let app = Router::new()
        .fallback(any(serve))
        .with_state(service.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (state, service, task, root)
}

#[tokio::test]
async fn published_catalog_is_passive_and_transient_failure_preserves_destination() {
    let (state, service, task, root) = fixture().await;
    let chosen = root.join("chosen");
    std::fs::create_dir(&chosen).unwrap();
    let config = lock(&state.pro.runtime).clone().unwrap();
    let mut destination = super::super::reserve(&chosen, &[], &[]).unwrap();
    destination.account = account_scope(&config);
    lock(&state.pro.adoptions).insert("w-copy".into(), destination);
    let result = super::super::list(&state).await;
    assert!(result.error.is_none());
    assert_eq!(result.projects.len(), 1);
    assert_eq!(result.projects[0].name, "A published project å");
    assert!(result.projects[0].host_id.is_none());
    assert!(result.projects[0].destination_saved);
    assert!(result.projects[0].local_root.is_none());
    assert!(lock(&state.pro.ownership).is_empty());
    assert!(lock(&service.requests)
        .iter()
        .all(|request| request.starts_with("GET /v2/")));
    service.mode.store(1, Ordering::Release);
    lock(&state.pro.project_cache).checked_at = 0;
    let retained = super::super::list(&state).await;
    assert!(retained.error.is_some());
    assert_eq!(retained.projects[0].workspace_id, "w-copy");
    assert!(retained.projects[0].destination_saved);
    assert!(!retained.projects[0].available);
    assert_eq!(
        lock(&state.pro.adoptions).get("w-copy").unwrap().root,
        chosen
    );
    // Negotiated empty means no visible account projects, never stale worker fallback.
    service.mode.store(7, Ordering::Release);
    lock(&state.pro.project_cache).checked_at = 0;
    assert!(super::super::list(&state).await.projects.is_empty());
    assert!(lock(&service.requests)
        .iter()
        .all(|request| !request.contains("/v1/hosts")));
    task.abort();
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn older_services_fall_back_but_malformed_negotiated_pages_fail_closed() {
    let (state, service, task, root) = fixture().await;
    let config = lock(&state.pro.runtime).clone().unwrap();
    for mode in [4, 5, 6] {
        service.mode.store(mode, Ordering::Release);
        assert!(list(&state, &config).await.unwrap().is_none());
    }
    for mode in [2, 3] {
        service.mode.store(mode, Ordering::Release);
        assert!(list(&state, &config).await.is_err());
    }
    service.mode.store(8, Ordering::Release);
    lock(&service.requests).clear();
    assert_eq!(list(&state, &config).await.unwrap().unwrap().len(), 128);
    assert_eq!(lock(&service.requests).len(), 2);
    assert!(lock(&service.requests)
        .iter()
        .all(|request| request.starts_with("GET /v2/")));
    task.abort();
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn manifest_metadata_is_bounded_and_visibility_is_explicit() {
    for invalid in ["", "  ", "private\nname", &"å".repeat(257)] {
        assert!(metadata(invalid, true).is_none());
    }
    let valid = serde_json::to_value(metadata(&"å".repeat(256), true).unwrap()).unwrap();
    assert_eq!(valid["version"], 1);
    assert_eq!(valid["visible"], true);
    let internal = serde_json::to_value(metadata("Setup", false).unwrap()).unwrap();
    assert_eq!(internal["visible"], false);
}
