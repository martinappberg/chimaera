use super::*;
use crate::pro::{engine, protocol::Configure, Ownership};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    routing::any,
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tower::ServiceExt;

struct Fixture {
    root: PathBuf,
    state: Arc<AppState>,
    project: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        Self::with_runtime(None)
    }
    fn with_runtime(runtime: Option<Arc<dyn crate::daemon_extension::Runtime>>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "chimaera-authority-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(root.join("project")).unwrap();
        let root = root.canonicalize().unwrap();
        let mut state = AppState::new(
            "fixture-token".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        );
        state.daemon_extension = runtime;
        let state = Arc::new(state);
        state.stopping.store(true, Ordering::Release);
        Self {
            project: root.join("project"),
            root,
            state,
        }
    }
    fn body(&self) -> Value {
        json!({"account_id":"account-a","role":"worker","endpoint":"http://127.0.0.1:9","keeper_url":"","workspace_root":self.project,
        "delegation":{"access_token":"synthetic-delegation","expires_at":"2099-01-01T00:00:00Z","scope":["baton","mirror"],"device_id":"worker-a","workspace":{"workspace_id":"w-a","revision":7}}})
    }
    async fn bind(&self) -> Configure {
        let (status, ack) = request(
            &self.state,
            Method::POST,
            "/api/v1/pro/configure/workspace",
            Some(self.body()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            ack,
            json!({"workspace_authority":1,"workspace":{"workspace_id":"w-a","revision":7},"workspace_root":self.project})
        );
        serde_json::from_value(self.body()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn request(
    state: &Arc<AppState>,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::app(state.clone()).oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("Authorization", "Bearer fixture-token")
                .header("Content-Type", "application/json")
                .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
                .unwrap(),
        ),
    )
    .await
    .expect("scoped request must complete within deadline")
    .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

#[tokio::test]
async fn distinct_route_rejects_legacy_downgrade_and_requires_worker_binding() {
    let fixture = Fixture::new();
    let (status, _) = request(
        &fixture.state,
        Method::POST,
        "/api/v1/pro/configure",
        Some(fixture.body()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!fixture.state.pro.root.exists());
    for (key, value) in [
        ("role", json!("device")),
        ("keeper_url", json!("http://127.0.0.1:9")),
        ("account_id", Value::Null),
    ] {
        let mut body = fixture.body();
        body[key] = value;
        assert_eq!(
            request(
                &fixture.state,
                Method::POST,
                "/api/v1/pro/configure/workspace",
                Some(body)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    for value in [
        Value::Null,
        json!({"workspace_id":"w-a","revision":0}),
        json!({"workspace_id":"../w-b","revision":1}),
    ] {
        let mut body = fixture.body();
        body["delegation"]["workspace"] = value;
        assert_eq!(
            request(
                &fixture.state,
                Method::POST,
                "/api/v1/pro/configure/workspace",
                Some(body)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut body = fixture.body();
    body["delegation"]["scope"] = json!(["baton", "mirror", "keeper"]);
    assert_eq!(
        request(
            &fixture.state,
            Method::POST,
            "/api/v1/pro/configure/workspace",
            Some(body)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut body = fixture.body();
    body["delegation"]
        .as_object_mut()
        .unwrap()
        .remove("workspace");
    assert_eq!(
        request(
            &fixture.state,
            Method::POST,
            "/api/v1/pro/configure",
            Some(body)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn binding_latches_across_disconnect_restart_and_corrupt_marker() {
    let fixture = Fixture::new();
    let config = fixture.bind().await;
    let (_, active_status) = request(&fixture.state, Method::GET, "/api/v1/pro/status", None).await;
    assert_eq!(active_status["configured"], true);
    assert_eq!(
        active_status["workspace_configuration"]["workspace"]["workspace_id"],
        "w-a"
    );
    let marker = fixture.state.pro.root.join("workspace-authority.json");
    assert!(!std::fs::read_to_string(&marker)
        .unwrap()
        .contains("synthetic-delegation"));
    assert_eq!(
        request(
            &fixture.state,
            Method::DELETE,
            "/api/v1/pro/configure",
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(!fixture.state.pro.configured.load(Ordering::Acquire));
    assert!(lock(&fixture.state.pro.authority).restricted());
    assert!(!crate::pro::may_write(&fixture.state, "w-a"));
    let (_, status) = request(&fixture.state, Method::GET, "/api/v1/pro/status", None).await;
    assert_eq!(
        status["workspace_configuration"]["workspace"]["revision"],
        7
    );
    let mut broad = fixture.body();
    broad["delegation"]
        .as_object_mut()
        .unwrap()
        .remove("workspace");
    assert_eq!(
        request(
            &fixture.state,
            Method::POST,
            "/api/v1/pro/configure",
            Some(broad)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for (key, value) in [("revision", json!(8)), ("workspace_id", json!("w-b"))] {
        let mut body = fixture.body();
        body["delegation"]["workspace"][key] = value;
        assert_eq!(
            request(
                &fixture.state,
                Method::POST,
                "/api/v1/pro/configure/workspace",
                Some(body)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let restored = Authority::load(&fixture.state.pro.root);
    assert!(restored.allows("w-a"));
    assert!(!restored.allows("w-b"));
    *lock(&fixture.state.pro.authority) = restored;
    assert!(prepare(&fixture.state, &config, fixture.project.clone())
        .await
        .is_ok());
    std::fs::write(&marker, b"{}").unwrap();
    let corrupt = Authority::load(&fixture.state.pro.root);
    assert!(corrupt.restricted());
    assert!(!corrupt.allows("w-a"));
}

#[tokio::test]
async fn foreign_workspace_fails_before_network_files_or_local_mutation() {
    let fixture = Fixture::new();
    let mut config = fixture.bind().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(any(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            })),
        )
        .await
        .unwrap();
    });
    // A separately accepted endpoint is required; mutation of the config itself
    // is not permission to replace the accepted account origin.
    config.endpoint = endpoint;
    assert!(config_matches(&fixture.state, &config, "w-a").is_err());
    for (path, method, body) in [
        ("/v1/baton/w-b", "GET", None),
        (
            "/v1/baton/w-b/acquire",
            "POST",
            Some(json!({"holder_id":"worker-a","expected_epoch":0})),
        ),
        (
            "/v1/mirror/credentials",
            "POST",
            Some(json!({"workspace_id":"w-b","expected_epoch":0})),
        ),
        ("/v1/worker/wake", "POST", None),
        ("/v1/baton/w-a/policy", "DELETE", None),
        ("/v1/hosts", "GET", None),
    ] {
        assert!(engine::account(&config, path, method, body.as_ref())
            .await
            .is_err());
    }
    let cache = fixture.root.join("forbidden-cache");
    let cache_guard = Arc::new(fixture.state.pro.cache("w-b").unwrap().lock_owned().await);
    assert!(engine::fetch_snapshot(
        &fixture.state,
        &config,
        "w-b",
        &cache,
        cache_guard,
        fixture.state.pro.generation.load(Ordering::Acquire),
    )
    .await
    .is_err());
    assert!(!cache.exists());
    for (path, method, body) in [
        (
            "/api/v1/pro/hydrate",
            Method::POST,
            json!({"workspace_id":"w-b","expected_epoch":0,"destination_root":fixture.root.join("forbidden")}),
        ),
        (
            "/api/v1/pro/handoff",
            Method::POST,
            json!({"workspace_id":"w-b","expected_epoch":0}),
        ),
        (
            "/api/v1/pro/privacy",
            Method::PUT,
            json!({"workspace_id":"w-b","never_mirror":true}),
        ),
    ] {
        assert_eq!(
            request(&fixture.state, method, path, Some(body)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert!(!fixture.root.join("forbidden").exists());
    assert!(!lock(&fixture.state.pro.preferences).contains_key("w-b"));
    assert!(!lock(&fixture.state.pro.ownership).contains_key("w-b"));
    assert!(engine::snapshot(&fixture.state, &config, "w-b", false)
        .await
        .is_err());
    assert!(
        engine::hydrate(&fixture.state, &config, "w-b", 0, false, None)
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // Prove the same transport fixture is reachable for the one allowed id.
    assert_eq!(
        engine::account(&config, "/v1/baton/w-a", "GET", None)
            .await
            .unwrap()
            .status,
        204
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn registered_root_is_fixed_and_replacement_is_not_adopted() {
    let fixture = Fixture::new();
    let config = fixture.bind().await;
    assert_eq!(
        destination(&fixture.state, &config, "w-a", None)
            .await
            .unwrap(),
        Some(fixture.project.clone())
    );
    let other = fixture.root.join("other");
    assert!(destination(&fixture.state, &config, "w-a", Some(&other))
        .await
        .is_err());
    assert!(!other.exists());
    assert!(registered_root(&fixture.state, "w-a", &other).is_err());
    std::fs::rename(&fixture.project, fixture.root.join("old-project")).unwrap();
    std::fs::create_dir(&fixture.project).unwrap();
    assert!(destination(&fixture.state, &config, "w-a", None)
        .await
        .is_err());
    assert!(prepare(&fixture.state, &config, fixture.project.clone())
        .await
        .is_err());
    assert!(
        engine::hydrate(&fixture.state, &config, "w-a", 0, false, None)
            .await
            .is_err()
    );
    assert!(!fixture.state.pro.root.join("w-a").exists());
}

#[tokio::test]
async fn foreign_registered_workspace_remains_fenced() {
    let fixture = Fixture::new();
    fixture.bind().await;
    let foreign = lock(&fixture.state.workspaces)
        .add(fixture.root.join("foreign"))
        .unwrap();
    lock(&fixture.state.session_workspaces).insert("s-other".into(), foreign.id.clone());
    lock(&fixture.state.pro.ownership).insert(foreign.id.clone(), Ownership::Local { epoch: 2 });
    assert!(!crate::pro::may_write(&fixture.state, &foreign.id));
    assert!(!crate::pro::may_import(&fixture.state, &foreign.id, 2));
    assert!(!engine::eligible(&fixture.state, &foreign));
    assert!(!crate::pro::workspace_in_scope(&fixture.state, &foreign.id));
    // Placement is the system's decision: there is no pin route to try.
    let (code, _) = request(
        &fixture.state,
        Method::PUT,
        "/api/v1/pro/keep-running",
        Some(json!({"session_id":"s-other","keep_running":true})),
    )
    .await;
    assert!(code.is_client_error(), "{code}");
    let (_, status) = request(&fixture.state, Method::GET, "/api/v1/pro/status", None).await;
    assert_eq!(status["workspaces"], json!([]));
    // Agent sign-in is the providers' bounded connection jobs (one credential
    // writer each); the legacy login-terminal route is gone.
    let (code, body) = request(
        &fixture.state,
        Method::POST,
        "/api/v1/pro/cloud/onboard",
        Some(json!({"agent":"claude"})),
    )
    .await;
    assert!(code.is_client_error(), "{code}");
    assert!(!body.to_string().contains("cloud setup"), "{body}");
}

#[test]
fn renewal_never_widens_or_rebinds_authority() {
    let fixture = Fixture::new();
    let config: Configure = serde_json::from_value(fixture.body()).unwrap();
    let old = config.delegation;
    let mut next = old.clone();
    next.access_token = "synthetic-rotated".into();
    assert!(renewal(&old, &next).is_ok());
    for binding in [
        None,
        Some(WorkspaceBinding {
            workspace_id: "w-a".into(),
            revision: 8,
        }),
        Some(WorkspaceBinding {
            workspace_id: "w-b".into(),
            revision: 7,
        }),
    ] {
        let mut value = next.clone();
        value.workspace = binding;
        assert!(renewal(&old, &value).is_err());
    }
    let mut value = next.clone();
    value.device_id = "worker-b".into();
    assert!(renewal(&old, &value).is_err());
    let mut value = next.clone();
    value.scope.push("keeper".into());
    assert!(renewal(&old, &value).is_err());
    let mut value = next.clone();
    value.access_token.clear();
    assert!(renewal(&old, &value).is_err());
    let mut legacy = old.clone();
    legacy.workspace = None;
    assert!(renewal(&legacy, &old).is_err());
    assert!(renewal(&legacy, &legacy).is_ok());
}

#[tokio::test]
async fn rejected_or_stale_renewal_does_not_count_as_a_refresh() {
    let fixture = Fixture::new();
    let config = fixture.bind().await;
    let generation = fixture.state.pro.generation.load(Ordering::Acquire);
    let previous = config.delegation;
    let mut next = previous.clone();
    next.access_token = "synthetic-rotated".into();
    next.workspace = None;
    assert!(!install_renewal(
        &fixture.state,
        generation,
        &previous,
        next
    ));
    assert_eq!(
        lock(&fixture.state.pro.runtime)
            .as_ref()
            .unwrap()
            .delegation
            .access_token,
        previous.access_token
    );
    let mut next = previous.clone();
    next.access_token = "synthetic-rotated".into();
    assert!(!install_renewal(
        &fixture.state,
        generation + 1,
        &previous,
        next.clone()
    ));
    assert_eq!(
        lock(&fixture.state.pro.runtime)
            .as_ref()
            .unwrap()
            .delegation
            .access_token,
        previous.access_token
    );
    assert!(install_renewal(&fixture.state, generation, &previous, next));
    assert_eq!(
        lock(&fixture.state.pro.runtime)
            .as_ref()
            .unwrap()
            .delegation
            .access_token,
        "synthetic-rotated"
    );
}

#[tokio::test]
async fn supervised_revision_comparison_preserves_every_other_latched_identity() {
    let fixture = Fixture::new();
    fixture.bind().await;
    let state = Arc::new(AppState::new(
        "fixture-token".into(),
        "fixture".into(),
        4242,
        0,
        fixture.root.clone(),
        fixture.root.join("config"),
    ));
    state.stopping.store(true, Ordering::Release);
    let Authority::Bound(previous) = Authority::load(&state.pro.root) else {
        panic!("saved authority missing");
    };
    let mut incoming = previous.clone();
    incoming.workspace.revision += 1;
    let receipt = serde_json::from_value(json!({"version":1,"workspace_id":"w-a","account_id":"account-a",
        "root_identity":{"device":previous.identity.0,"inode":previous.identity.1},"registration_revision":8,
        "launch_generation":1,"previous_generation":0,"os_boot_id":crate::pro::execution::supervisor::fixture_boot(&state).unwrap()})).unwrap();
    crate::pro::execution::supervisor::stage(&state, Some(receipt));
    revision_advance(&state, &previous, &incoming).unwrap();
    for mismatch in 0..7 {
        let mut changed = incoming.clone();
        match mismatch {
            0 => changed.account_id = "other".into(),
            1 => changed.endpoint = "http://127.0.0.1:10".into(),
            2 => changed.workspace.workspace_id = "w-other".into(),
            3 => changed.root = fixture.root.join("other"),
            4 => changed.identity.0 += 1,
            5 => changed.identity.1 += 1,
            6 => changed.workspace.revision = 7,
            _ => unreachable!(),
        }
        assert!(
            revision_advance(&state, &previous, &changed).is_err(),
            "mismatch {mismatch}"
        );
    }
    assert!(crate::pro::execution::supervisor::pending(&state));
    assert!(crate::pro::execution::supervisor::ack(&state).is_none());
    assert_eq!(
        lock(&state.pro.authority)
            .acknowledgment()
            .unwrap()
            .workspace
            .revision,
        7
    );
}

// A finite admission probe stops before Git or installation. The actual composed
// companion journey remains the materialization/runtime acceptance gate.
#[tokio::test]
async fn initial_hydrate_retains_transfer_owner_after_read_admission() {
    use crate::daemon_extension::{self, transfer, Runtime};
    struct Probe {
        calls: Arc<AtomicUsize>,
    }
    impl Runtime for Probe {
        fn coordinate(
            &self,
            _owner: daemon_extension::CoordinatorOwner,
        ) -> daemon_extension::RuntimeFuture {
            Box::pin(async {})
        }
        fn transfer<'a>(
            &'a self,
            host: Arc<transfer::TransferHost>,
            operation: transfer::TransferRequest<'a>,
        ) -> transfer::TransferFuture<'a> {
            Box::pin(async move {
                host.current()?;
                match operation {
                    transfer::TransferRequest::Initialize { path, format } => {
                        anyhow::ensure!(
                            path == host.cache().join("incoming.git") && format == "sha1",
                            "wrong initial repository"
                        );
                        self.calls.fetch_add(1, Ordering::SeqCst);
                        anyhow::bail!("fixture_initial_repository_admitted")
                    }
                    _ => anyhow::bail!("unexpected initial transfer operation"),
                }
            })
        }
    }
    for changed in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let fixture = Fixture::with_runtime(Some(Arc::new(Probe {
            calls: calls.clone(),
        })));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let owner = fixture.state.clone();
        let url = endpoint.clone();
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = reads.clone();
        let peer = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/mirror/credentials", axum::routing::post(move || {
                let owner = owner.clone();
                let url = url.clone();
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    if changed {
                        owner.pro.generation.fetch_add(1, Ordering::AcqRel);
                    }
                    axum::Json(json!({"workspace_id":"w-a","repository_url":url,"working_tree_url":url,
                        "username":"fixture","password":"synthetic","read_only":true,
                        "storage_limit_bytes":1024,"max_file_bytes":1024}))
                }
            }))).await.unwrap();
        });
        let mut body = fixture.body();
        body["endpoint"] = endpoint.into();
        assert_eq!(
            request(
                &fixture.state,
                Method::POST,
                "/api/v1/pro/configure/workspace",
                Some(body)
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(crate::lock(&fixture.state.workspaces).get("w-a").is_none());
        let (status, reply) = request(
            &fixture.state,
            Method::POST,
            "/api/v1/pro/hydrate",
            Some(json!({
                "workspace_id":"w-a","expected_epoch":0,"destination_root":fixture.project,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        if changed {
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(reply["code"], "account_changed");
        } else {
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(reply["error"], "fixture_initial_repository_admitted");
        }
        assert!(!fixture.state.pro.root.join("w-a/incoming.git").exists());
        assert!(crate::lock(&fixture.state.workspaces).get("w-a").is_none());
        peer.abort();
        let _ = peer.await;
    }
}
