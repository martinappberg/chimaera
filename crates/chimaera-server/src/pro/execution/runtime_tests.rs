use super::*;
use axum::{extract::State as WebState, http::HeaderMap, routing::get, Json, Router};
use std::sync::atomic::AtomicUsize;

fn receipt_fixture() -> wire::Checkpoint {
    wire::Checkpoint {
        id: "cp-fixture".into(),
        sequence: 2,
        source_holder_id: "d-home".into(),
        source_epoch: 2,
        working_tree_oid: "a".repeat(40),
        config_oid: "b".repeat(40),
        handoff_oid: "c".repeat(40),
        continuation: wire::Continuation::Idle,
    }
}

#[tokio::test]
async fn cold_boot_proof_clears_only_orphan_process_uncertainty_not_execution_authority() {
    let (state, config, root) = fixture();
    accept(&state, &config, &baton(), 0, RequestStart::now()).unwrap();
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 2 });
    crate::pro::ensure_root(&state.pro.root).await.unwrap();
    prepare_launch(&state, "w-a").await.unwrap();
    let mut saved = lock(&state.pro.preferences).clone();
    saved.get_mut("w-a").unwrap().execution_boot = Some("different-synthetic-kernel-boot".into());
    let restored = State::restore(&state.pro.root, &saved, true, false);
    if restored.boot.is_some() {
        assert!(!lock(&restored.unclean).contains_key("w-a"));
    } else {
        assert!(
            lock(&restored.unclean).contains_key("w-a"),
            "unknown boot must fail closed on a worker"
        );
    }
    assert!(restored.proofs.lock().unwrap().is_empty());
    assert!(restored.latched.lock().unwrap().contains("w-a"));
    saved.get_mut("w-a").unwrap().execution_boot = None;
    assert!(
        lock(&State::restore(&state.pro.root, &saved, true, false).unclean).contains_key("w-a")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn real_http_negotiation_status_and_restart_cannot_downgrade_enrolled_work() {
    let (state, config, root) = fixture();
    // Its `/pro/*` routes exist only with the extension composed.
    let state = std::sync::Arc::new(crate::daemon_extension::with_inert_for_tests(
        std::sync::Arc::into_inner(state).unwrap(),
    ));
    state.stopping.store(true, Ordering::Release);
    std::fs::create_dir_all(root.join("project")).unwrap();
    lock(&state.workspaces)
        .import_exact(crate::workspaces::Workspace {
            id: "w-a".into(),
            root: root.join("project"),
            name: "Synthetic project".into(),
            last_opened_at: 0,
            mastermind: None,
            plugins_on: vec![],
            cloud_internal: false,
            hidden: false,
        })
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let body = json!({"account_id":config.account_id,"role":"device","endpoint":config.endpoint,
        "keeper_url":"","execution":config.execution,"delegation":{"access_token":"synthetic","expires_at":"2099-01-01T00:00:00Z","scope":["baton","mirror"],"device_id":"d-home"}});
    let reply = crate::pro::transport::request(
        &endpoint,
        "/api/v1/pro/configure/execution",
        "POST",
        "wrong",
        Some(&body),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, 401);
    for path in ["/api/v1/pro/configure", "/api/v1/pro/configure/workspace"] {
        let mut request = body.clone();
        request["workspace_root"] = json!(root.join("project"));
        let reply =
            crate::pro::transport::request(&endpoint, path, "POST", "fixture", Some(&request))
                .await
                .unwrap();
        assert_eq!(reply.status, 400);
    }
    let reply = crate::pro::transport::request(
        &endpoint,
        "/api/v1/pro/configure/execution",
        "POST",
        "fixture",
        Some(&body),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, 200);
    let ack: Value = reply.json().unwrap();
    assert_eq!(
        ack,
        json!({"execution_authority":1,"execution":config.execution,"workspace_configuration":null})
    );
    assert!(!ack.to_string().contains("synthetic"));
    crate::pro::install_execution_fixture(&state, "w-a", 2).unwrap();
    crate::pro::persist(&state).await.unwrap();
    let reply =
        crate::pro::transport::request(&endpoint, "/api/v1/pro/status", "GET", "fixture", None)
            .await
            .unwrap();
    let status: Value = reply.json().unwrap();
    assert_eq!(status["workspaces"][0]["execution_allowed"], true);
    assert_eq!(status["workspaces"][0]["continuity"]["version"], 2);
    server.abort();
    let restored = Arc::new(crate::daemon_extension::with_inert_for_tests(
        AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ),
    ));
    restored.stopping.store(true, Ordering::Release);
    // A restarted device keeps its terminals, but its agents (new or
    // previous) wait for this life's verified ownership: the cloud may hold
    // the project by now.
    assert!(!crate::pro::may_execute(&restored, "w-a"));
    assert!(crate::pro::may_run_shell(&restored, "w-a"));
    assert!(!crate::pro::may_restore(&restored, "w-a"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::app(restored.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut legacy = body.clone();
    legacy.as_object_mut().unwrap().remove("execution");
    let reply = crate::pro::transport::request(
        &endpoint,
        "/api/v1/pro/configure",
        "POST",
        "fixture",
        Some(&legacy),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, 400);
    let reply = crate::pro::transport::request(
        &endpoint,
        "/api/v1/pro/configure/execution",
        "POST",
        "fixture",
        Some(&body),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, 200);
    assert!(
        !crate::pro::may_restore(&restored, "w-a"),
        "configure does not verify ownership"
    );
    let reply =
        crate::pro::transport::request(&endpoint, "/api/v1/pro/status", "GET", "fixture", None)
            .await
            .unwrap();
    assert_eq!(reply.status, 200);
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}

struct ReceiptServer {
    calls: AtomicUsize,
    recovery: bool,
}
async fn publication_reply(
    WebState(f): WebState<Arc<ReceiptServer>>,
    headers: HeaderMap,
) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer synthetic");
    let calls = f.calls.fetch_add(1, Ordering::SeqCst);
    let mut receipt = receipt_fixture();
    if calls == 0 {
        receipt.working_tree_oid = "d".repeat(40);
    }
    if f.recovery {
        Json(json!({"checkpoint":receipt}))
    } else {
        let mut reply = json!({"workspace_id":"w-a","holder_id":"d-home","epoch":2,"requires_fork":false,
            "server_now":"2026-09-28T00:00:00Z","expires_at":"2026-09-28T00:01:30Z"});
        reply["checkpoint"] = json!(receipt);
        Json(reply)
    }
}
#[tokio::test]
async fn acknowledgment_requires_exact_publication_and_recovery_never_uses_normal_routes() {
    for recovery in [false, true] {
        let (_, mut config, root) = fixture();
        config.recovery = recovery;
        let fixture = Arc::new(ReceiptServer {
            calls: AtomicUsize::new(0),
            recovery,
        });
        let route = if recovery {
            "/v2/recovery/checkpoint"
        } else {
            "/v2/baton/w-a"
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        config.endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route(route, get(publication_reply).post(publication_reply))
            .with_state(fixture.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let expected = receipt_fixture();
        let result = receipt::published(
            &config,
            "w-a",
            2,
            [
                &expected.working_tree_oid,
                &expected.config_oid,
                &expected.handoff_oid,
            ],
            wire::Continuation::Idle,
        )
        .await
        .unwrap();
        assert_eq!(result, expected);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        if recovery {
            for (path, method) in [
                ("/v2/baton/w-a", "GET"),
                ("/v2/baton/w-a/renew", "POST"),
                ("/v1/delegations/renew", "POST"),
            ] {
                assert!(crate::pro::engine::account(&config, path, method, None)
                    .await
                    .is_err());
            }
            assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        }
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn git(root: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "fixture git failed");
    String::from_utf8(output.stdout).unwrap().trim().into()
}
#[tokio::test]
async fn real_git_receipt_pins_old_objects_even_when_moving_heads_are_newer() {
    let (_, _, root) = fixture();
    let mut checkpoint = receipt_fixture();
    git(&root, &["init", "-q", "--initial-branch=main"]);
    let mut revisions = Vec::new();
    for value in [
        "saved tree",
        "saved settings",
        "saved conversation",
        "newer head",
    ] {
        std::fs::write(root.join("payload"), value).unwrap();
        git(&root, &["add", "payload"]);
        git(&root, &["commit", "-qm", "fixture"]);
        revisions.push(git(&root, &["rev-parse", "HEAD"]));
    }
    checkpoint.working_tree_oid = revisions[0].clone();
    checkpoint.config_oid = revisions[1].clone();
    checkpoint.handoff_oid = revisions[2].clone();
    for branch in ["main", "config", "handoff"] {
        git(
            &root,
            &["update-ref", &format!("refs/heads/{branch}"), &revisions[3]],
        );
    }
    receipt::pin(&root, &checkpoint).await.unwrap();
    assert_eq!(git(&root, &["show", "main:payload"]), "saved tree");
    assert_eq!(git(&root, &["show", "config:payload"]), "saved settings");
    assert_eq!(
        git(&root, &["show", "handoff:payload"]),
        "saved conversation"
    );
    checkpoint.working_tree_oid = "0".repeat(40);
    assert!(receipt::pin(&root, &checkpoint).await.is_err());
    assert_eq!(git(&root, &["show", "main:payload"]), "saved tree");
    std::fs::remove_dir_all(root).unwrap();
}
