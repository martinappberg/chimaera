use super::*;
use crate::pro::protocol::Delegation;
use std::sync::Arc;
fn fixture() -> (Arc<AppState>, Configure, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "chimaera-execution-{}",
        chimaera_core::generate_token()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let state = Arc::new(AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.clone(),
        root.join("config"),
    ));
    let config = Configure {
        recovery: false,
        account_id: Some("a-fixture".into()),
        role: Role::Device,
        endpoint: "http://127.0.0.1:1".into(),
        keeper_url: String::new(),
        hours_exhausted: false,
        execution: Some(wire::ExecutionConfiguration {
            version: 1,
            installation_id: Some("i-home".into()),
            capability: wire::ExecutionCapability::managed(),
        }),
        delegation: Delegation {
            workspace: None,
            access_token: "synthetic".into(),
            expires_at: String::new(),
            scope: vec!["baton".into(), "mirror".into()],
            device_id: "d-home".into(),
        },
    };
    (state, config, root)
}
fn baton() -> Baton {
    serde_json::from_value(json!({"workspace_id":"w-a","holder_id":"d-home","epoch":2,"requires_fork":false,"server_now":"2026-09-28T00:00:00Z","expires_at":"2026-09-28T00:01:30Z",
    "continuity":{"version":2,"mode":"managed_v1","policy_revision":1,"preferred_installation_id":"i-home"},"execution_capability":{"version":1,"boundary":"managed_processes","expired_takeover":false},"execution_lease":{"id":"lease-a","sequence":1}})).unwrap()
}

#[tokio::test]
async fn strict_worker_polling_fences_released_work_until_hydration() {
    use axum::{http::StatusCode, response::IntoResponse, routing::any, Json, Router};
    use std::sync::atomic::AtomicUsize;
    let (state, mut config, root) = fixture();
    config.role = Role::Worker;
    config.delegation.device_id = "worker-fixture".into();
    let released = json!({
        "workspace_id":"w-a", "holder_id":null, "epoch":3, "requires_fork":false,
        "server_now":"2026-09-28T00:00:00Z", "expires_at":null,
        "continuity":{"version":2,"mode":"managed_v1","policy_revision":1,"preferred_installation_id":"i-home"},
        "execution_capability":{"version":1,"boundary":"managed_processes","expired_takeover":false}
    });
    let mutations = Arc::new(AtomicUsize::new(0));
    let counted = mutations.clone();
    let router = Router::new().fallback(any(move |request: axum::extract::Request| {
        let baton = released.clone();
        let counted = counted.clone();
        async move {
            if request.method() == axum::http::Method::GET
                && request.uri().path() == "/v2/baton/w-a"
            {
                Json(baton).into_response()
            } else {
                counted.fetch_add(1, Ordering::SeqCst);
                StatusCode::CONFLICT.into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    config.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 1 });
    super::super::engine::reconcile(&state, &config, "w-a")
        .await
        .unwrap();
    assert!(managed(&state, "w-a"));
    assert!(!checkpoint_mode(&state, "w-a"));
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
    assert!(matches!(
        lock(&state.pro.ownership).get("w-a"),
        Some(Ownership::Hydrating { epoch: 3 })
    ));
    assert!(!crate::pro::may_execute(&state, "w-a"));
    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn passive_observation_and_restart_never_install_execution_authority() {
    for strict in [false, true] {
        let (state, config, root) = fixture();
        if strict {
            worker_fixture(&state);
        }
        let grant = baton();
        observe(&state, &config, &grant).unwrap();
        lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 2 });
        // A passive read never creates a lease: a worker cannot execute, and
        // a device publishes nothing, but keeps its own work (laptop first).
        assert_eq!(crate::pro::may_execute(&state, "w-a"), !strict);
        assert!(!lease_valid(&state, "w-a"));
        assert!(!crate::pro::may_restore(&state, "w-a"));
        assert!(crate::pro::may_execute(&state, "w-free"));
        accept(&state, &config, &grant, 0, RequestStart::now()).unwrap();
        assert!(crate::pro::may_execute(&state, "w-a"));
        assert!(crate::pro::may_restore(&state, "w-a"));
        crate::pro::ensure_root(&state.pro.root).await.unwrap();
        crate::pro::persist(&state).await.unwrap();
        let restored = crate::pro::ProState::new(state.pro.root.clone());
        assert!(restored.execution.proofs.lock().unwrap().is_empty());
        assert!(lock(&restored.preferences)["w-a"].continuity.is_some());
        assert_eq!(restored.worker.load(Ordering::Acquire), strict);
        assert!(matches!(
            lock(&restored.ownership)["w-a"],
            Ownership::AwaitingVerification { epoch: 2 }
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn grant_replay_stale_generation_and_capability_downgrade_do_not_extend_deadline() {
    let (state, config, root) = fixture();
    let grant = baton();
    accept(&state, &config, &grant, 0, RequestStart::now()).unwrap();
    assert!(accept(&state, &config, &grant, 0, RequestStart::now()).is_err());
    let mut next = grant.clone();
    next.execution_lease.as_mut().unwrap().sequence = 2;
    assert!(accept(&state, &config, &next, 1, RequestStart::now()).is_err());
    next.execution_capability.as_mut().unwrap().expired_takeover = true;
    assert!(accept(&state, &config, &next, 0, RequestStart::now()).is_err());
    let mut legacy = grant.clone();
    legacy.continuity = None;
    legacy.execution_lease = None;
    legacy.execution_capability = None;
    assert!(observe(&state, &config, &legacy).is_err());
    assert_eq!(lock(&state.pro.execution.proofs)["w-a"].lease.sequence, 1);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn device_lease_expiry_stops_publication_but_never_local_work() {
    let (state, _config, root) = fixture();
    crate::pro::install_execution_fixture(&state, "w-a", 2).unwrap();
    lock(&state.pro.execution.proofs)
        .get_mut("w-a")
        .unwrap()
        .deadline = lease::Deadline::expired_fixture();
    assert!(expire(&state, 0).is_empty());
    assert!(crate::pro::may_execute(&state, "w-a"));
    assert!(!lease_valid(&state, "w-a"), "publication waits for renewal");
    assert!(!lock(&state.pro.preferences)["w-a"].execution_uncertain);
    // Forwarded viewers act only under a live lease.
    assert!(crate::pro::validate_execution_scope(&state, "w-a", 2).is_err());
    // A verified other owner is the one fence on a device.
    lock(&state.pro.ownership).insert(
        "w-a".into(),
        Ownership::Remote {
            epoch: 3,
            holder: "worker-a".into(),
        },
    );
    assert!(!crate::pro::may_execute(&state, "w-a"));
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn expiry_closes_ingress_and_marks_uncertain_without_a_network_round_trip() {
    let (state, _config, root) = fixture();
    worker_fixture(&state);
    crate::pro::install_execution_fixture(&state, "w-a", 2).unwrap();
    {
        let mut proofs = lock(&state.pro.execution.proofs);
        proofs.get_mut("w-a").unwrap().deadline = lease::Deadline::expired_fixture();
    }
    assert!(!crate::pro::may_execute(&state, "w-a"));
    assert_eq!(expire(&state, 0), vec!["w-a"]);
    assert!(lock(&state.pro.preferences)["w-a"].execution_uncertain);
    assert!(!resume_allowed(&state, "w-a"));
    assert!(crate::pro::validate_execution_scope(&state, "w-a", 2).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn only_preferred_installation_can_request_automatic_return() {
    let (state, mut config, root) = fixture();
    observe(&state, &config, &baton()).unwrap();
    assert!(preferred_here(&state, &config, "w-a"));
    config.execution.as_mut().unwrap().installation_id = Some("i-viewer".into());
    assert!(!preferred_here(&state, &config, "w-a"));
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn immutable_receipt_cannot_be_a_git_revision_expression() {
    let mut receipt = wire::Checkpoint {
        id: "cp-a".into(),
        sequence: 1,
        source_holder_id: "d-home".into(),
        source_epoch: 2,
        working_tree_oid: "a".repeat(40),
        config_oid: "b".repeat(40),
        handoff_oid: "c".repeat(40),
        continuation: wire::Continuation::Idle,
    };
    receipt::validate(&receipt).unwrap();
    assert_eq!(
        receipt::revision(Some(&receipt), "main").unwrap(),
        "a".repeat(40)
    );
    receipt.working_tree_oid = "HEAD:../private".into();
    assert!(receipt::validate(&receipt).is_err());
}

#[tokio::test]
async fn same_boot_crash_never_turns_empty_registry_into_stopped_evidence() {
    let (state, config, root) = fixture();
    accept(&state, &config, &baton(), 0, RequestStart::now()).unwrap();
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 2 });
    crate::pro::ensure_root(&state.pro.root).await.unwrap();
    prepare_launch(&state, "w-a").await.unwrap();
    let restored = State::restore(&state.pro.root, &lock(&state.pro.preferences));
    assert!(restored.unclean.contains("w-a"));
    let disk = crate::pro::ProState::new(state.pro.root.clone());
    assert!(disk.execution.unclean.contains("w-a"));
    std::fs::write(state.pro.root.join("state.json"), b"{}").unwrap();
    let damaged = crate::pro::ProState::new(state.pro.root.clone());
    assert!(lock(&damaged.execution.latched).contains("w-a"));
    assert!(damaged.execution.proofs.lock().unwrap().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[path = "runtime_tests.rs"]
mod runtime;

#[test]
fn canonical_recovery_has_a_distinct_exact_capability_and_keeps_expired_input_closed() {
    let (state, mut config, root) = fixture();
    worker_fixture(&state);
    let mut grant = baton();
    config.execution.as_mut().unwrap().capability = wire::ExecutionCapability::checkpoint_fork();
    grant.execution_capability = Some(wire::ExecutionCapability::checkpoint_fork());
    grant.continuity.as_mut().unwrap().mode = "checkpoint_fork_v1".into();
    accept(&state, &config, &grant, 0, RequestStart::now()).unwrap();
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 2 });
    assert!(crate::pro::may_execute(&state, "w-a"));
    lock(&state.pro.execution.proofs)
        .get_mut("w-a")
        .unwrap()
        .deadline = lease::Deadline::expired_fixture();
    expire(&state, 0);
    assert!(
        resume_allowed(&state, "w-a"),
        "recovery does not add a routine human review gate"
    );
    assert!(
        !crate::pro::may_execute(&state, "w-a"),
        "automatic recovery is not permission to use an expired grant"
    );
    assert!(recovery_context(&state, "w-a"));
    grant.epoch = 3;
    grant.execution_lease = Some(wire::ExecutionLease {
        id: "lease-new".into(),
        sequence: 1,
    });
    grant.requires_fork = true;
    accept(&state, &config, &grant, 0, RequestStart::now()).unwrap();
    assert!(
        !crate::pro::may_execute(&state, "w-a"),
        "old ownership cannot use a new proof"
    );
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 3 });
    assert!(crate::pro::may_execute(&state, "w-a"));
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn new_default_capability_never_changes_an_existing_strict_policy_renewal() {
    let (state, mut config, root) = fixture();
    observe(&state, &config, &baton()).unwrap();
    config.execution.as_mut().unwrap().capability = wire::ExecutionCapability::checkpoint_fork();
    assert_eq!(
        effective(&state, &config, "w-a")
            .unwrap()
            .execution
            .unwrap()
            .capability,
        wire::ExecutionCapability::managed()
    );
    let mut mismatched = baton();
    mismatched.execution_capability = Some(wire::ExecutionCapability::checkpoint_fork());
    assert!(observe(&state, &config, &mismatched).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
