use super::*;
use crate::pro::protocol::Delegation;
use std::os::unix::process::CommandExt;
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
        alias: None,
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

/// A newer account may add fields to what it sends; the daemon keeps working
/// (unknown evidence of a turn is uncertain, never a blind replay). The
/// capability identity stays exact: an unknown field there is a different one.
#[test]
fn service_responses_accept_additive_fields() {
    let mut value = json!({"workspace_id":"w-a","holder_id":"d-home","epoch":2,"requires_fork":false,"server_now":"2026-09-28T00:00:00Z","expires_at":"2026-09-28T00:01:30Z",
    "continuity":{"version":2,"mode":"managed_v1","policy_revision":1,"preferred_installation_id":"i-home","future":true},"execution_capability":{"version":1,"boundary":"managed_processes","expired_takeover":false},"execution_lease":{"id":"lease-a","sequence":1,"issued_by":"future"},
    "checkpoint":{"id":"cp-a","sequence":1,"source_holder_id":"d-home","source_epoch":2,"working_tree_oid":"a","config_oid":"b","handoff_oid":"c","continuation":"paused_on_question","signed":"future"},
    "placement":{"availability":"suspended"}});
    let baton: Baton = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(baton.execution_lease.unwrap().sequence, 1);
    assert_eq!(
        baton.checkpoint.unwrap().continuation,
        wire::Continuation::Uncertain
    );
    value["execution_capability"]["future"] = json!(true);
    assert!(serde_json::from_value::<Baton>(value).is_err());
}

#[tokio::test]
async fn strict_worker_polling_refuses_missing_runtime_without_granting_execution() {
    use axum::{http::StatusCode, response::IntoResponse, routing::any, Json, Router};
    use std::sync::atomic::AtomicUsize;
    let (state, mut config, root) = fixture();
    config.role = Role::Worker;
    config.delegation.device_id = "worker-fixture".into();
    config.execution.as_mut().unwrap().installation_id = None;
    validate_configuration(&config).unwrap();
    worker_fixture(&state);
    let released = json!({
        "workspace_id":"w-a", "holder_id":null, "epoch":3, "requires_fork":false,
        "server_now":"2026-09-28T00:00:00Z", "expires_at":null,
        "continuity":{"version":2,"mode":"managed_v1","policy_revision":1,"preferred_installation_id":"i-home"},
        "execution_capability":{"version":1,"boundary":"managed_processes","expired_takeover":false}
    });
    // A passive, normally validated enrollment is metadata, not a lease.
    // The absent Runtime cannot install a proof or thaw this strict worker.
    observe(
        &state,
        &config,
        &serde_json::from_value(released.clone()).unwrap(),
    )
    .unwrap();
    assert!(managed(&state, "w-a"));
    assert!(!lease_valid(&state, "w-a"));
    assert!(!crate::pro::may_execute(&state, "w-a"));
    assert!(crate::pro::may_execute(&state, "w-free"));
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
    let error = super::super::engine::reconcile(&state, &config, "w-a")
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "optional_runtime_unavailable");
    assert!(!lease_valid(&state, "w-a"));
    assert!(crate::pro::may_execute(&state, "w-free"));
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
    assert!(matches!(
        lock(&state.pro.ownership).get("w-a"),
        Some(Ownership::Local { epoch: 1 })
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
/// A computer whose lease lapsed stops its own agents, like a cloud machine,
/// so the cloud continues exactly once, whatever the failure: the account
/// answering with its own server errors on every renewal fences at the
/// deadline too (review R4 B1), and so does a proxy's.
#[test]
fn device_lease_expiry_fences_its_own_agents_whatever_the_account_answered() {
    for from_account in [true, false] {
        let (state, _config, root) = fixture();
        crate::pro::install_execution_fixture(&state, "w-a", 2).unwrap();
        // Every renewal of this lease got a 503 (with or without the
        // account's marker); nothing moved the deadline.
        for _ in 0..3 {
            crate::pro::reach::answered(&state, 503, from_account);
        }
        assert!(expire(&state, 0).is_empty(), "the deadline has not passed");
        lock(&state.pro.execution.proofs)
            .get_mut("w-a")
            .unwrap()
            .deadline = lease::Deadline::expired_fixture();
        crate::pro::reach::answered(&state, 503, from_account);
        assert_eq!(expire(&state, 0), vec!["w-a"], "marked={from_account}");
        assert!(!crate::pro::may_execute(&state, "w-a"));
        assert!(crate::pro::validate_execution_scope(&state, "w-a", 2).is_err());
        // Its own sessions stay resumable once it holds the project again.
        assert!(!lock(&state.pro.preferences)["w-a"].execution_uncertain);
        assert!(resume_allowed(&state, "w-a"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
/// A computer that thaws before anyone could have taken its project renews
/// first and keeps running; one that thaws later is fenced at once.
#[test]
fn a_thawed_computer_renews_first_only_while_nobody_could_have_taken_over() {
    let (state, _config, root) = fixture();
    crate::pro::install_execution_fixture(&state, "w-a", 2).unwrap();
    // The fixture's lease was granted just now: a thaw right after its local
    // deadline is still inside the account's lease plus grace.
    {
        let mut proofs = lock(&state.pro.execution.proofs);
        let proof = proofs.get_mut("w-a").unwrap();
        proof.deadline = lease::Deadline::lapsed_recently_fixture();
    }
    assert!(resumed(&state, 0));
    assert!(resuming(&state, "w-a"));
    assert!(expire(&state, 0).is_empty(), "renewal first");
    assert!(crate::pro::may_execute(&state, "w-a"));
    // A deadline long past (anyone may hold the project now): no window.
    {
        let mut proofs = lock(&state.pro.execution.proofs);
        let proof = proofs.get_mut("w-a").unwrap();
        proof.renew_until = None;
        proof.deadline = lease::Deadline::expired_fixture();
    }
    assert!(!resumed(&state, 0));
    assert_eq!(expire(&state, 0), vec!["w-a"]);
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
    // No recorded process group: a worker cannot prove anything.
    let restored = State::restore(&state.pro.root, &lock(&state.pro.preferences), true, false);
    assert!(lock(&restored.unclean).contains_key("w-a"));
    // A device can still work (D1), but incomplete durable launch evidence
    // cannot prove that every old process group stopped for publication.
    let restored = State::restore(&state.pro.root, &lock(&state.pro.preferences), false, false);
    assert!(lock(&restored.unclean).contains_key("w-a"));
    // A recorded group that is still alive fences both until it exits.
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let mut preferences = lock(&state.pro.preferences).clone();
    preferences.get_mut("w-a").unwrap().execution_launch_pending = false;
    let started = restart::leader_start(child.id() as i32).unwrap();
    preferences.get_mut("w-a").unwrap().execution_groups = vec![child.id()];
    preferences.get_mut("w-a").unwrap().execution_starts = vec![started];
    for worker in [false, true] {
        let restored = State::restore(&state.pro.root, &preferences, worker, false);
        assert_eq!(lock(&restored.unclean)["w-a"], vec![(child.id(), started)]);
    }
    // The same id with another start time is a reused group, not old work.
    preferences.get_mut("w-a").unwrap().execution_starts = vec![started + 1];
    let restored = State::restore(&state.pro.root, &preferences, false, false);
    assert!(!lock(&restored.unclean).contains_key("w-a"));
    lock(&state.pro.execution.unclean).insert("w-a".into(), vec![(child.id(), started)]);
    reprobe(&state);
    assert!(!quiescent(&state, "w-a"), "a live old group blocks handoff");
    child.kill().unwrap();
    child.wait().unwrap();
    reprobe(&state);
    assert!(
        quiescent(&state, "w-a"),
        "an exited group releases its fence"
    );
    std::fs::write(state.pro.root.join("state.json"), b"{}").unwrap();
    let damaged = crate::pro::ProState::new(state.pro.root.clone());
    assert!(lock(&damaged.execution.latched).contains("w-a"));
    assert!(damaged.execution.proofs.lock().unwrap().is_empty());
    // Unreadable ownership state fails closed and keeps the damaged copy.
    assert!(lock(&damaged.execution.uncertain).contains("w-a"));
    assert!(state.pro.root.join("state.json.damaged").exists());
    std::fs::remove_dir_all(root).unwrap();
}

/// A lost or partial enrollment record makes only the projects it could have
/// covered uncertain (they stop publishing until the account confirms their
/// policy again); every other project keeps its ordinary behavior.
#[test]
fn lost_enrollment_records_make_only_those_projects_uncertain() {
    let restart = |root: &std::path::Path| {
        Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.to_path_buf(),
            root.join("config"),
        ))
    };
    let (state, config, root) = fixture();
    observe(&state, &config, &baton()).unwrap();
    // The latch names w-a, but its policy record is gone; w-b never enrolled.
    std::fs::create_dir_all(&state.pro.root).unwrap();
    std::fs::write(
        state.pro.root.join("execution-authority.json"),
        br#"{"version":1,"workspaces":["w-a"]}"#,
    )
    .unwrap();
    std::fs::write(
        state.pro.root.join("state.json"),
        br#"{"ownership":{},"preferences":{"w-b":{}}}"#,
    )
    .unwrap();
    let restarted = restart(&root);
    assert!(managed(&restarted, "w-a"));
    assert!(
        !managed(&restarted, "w-b"),
        "an unrelated project is unaffected"
    );
    // An authoritative read that carries w-a's policy resolves it.
    observe(&restarted, &config, &baton()).unwrap();
    assert!(managed(&restarted, "w-a"));
    assert!(lease_valid(&restarted, "w-b"));
    drop(restarted);

    // Nothing readable at all: a computer never makes an unenrolled project
    // managed (the account refuses an enrolled one's downgrade itself), and
    // an uncertain project still launches agents there.
    std::fs::write(state.pro.root.join("state.json"), b"not json").unwrap();
    std::fs::write(state.pro.root.join("execution-authority.json"), b"not json").unwrap();
    std::fs::create_dir_all(state.pro.root.join("w-mirrored")).unwrap();
    let restarted = restart(&root);
    assert!(!managed(&restarted, "w-mirrored"));
    assert!(lease_valid(&restarted, "w-never-mirrored"));
    lock(&restarted.pro.execution.uncertain).insert("w-new".into());
    lock(&restarted.pro.ownership).insert("w-new".into(), Ownership::Local { epoch: 1 });
    assert!(crate::pro::may_execute(&restarted, "w-new"));
    drop(restarted);
    // A cloud machine cannot tell which of its projects were enrolled: each
    // one with local mirror data waits for the account.
    let empty = HashMap::new();
    let worker = State::restore(&state.pro.root, &empty, true, true);
    assert!(lock(&worker.uncertain).contains("w-mirrored"));
    assert!(!lock(&worker.uncertain).contains("w-never-mirrored"));
    // A read error (here: the path is a directory) is not damage: the file
    // is not set aside and no unenrolled project becomes managed.
    let _ = std::fs::remove_file(state.pro.root.join("state.json"));
    let _ = std::fs::remove_file(state.pro.root.join("state.json.damaged"));
    std::fs::create_dir_all(state.pro.root.join("state.json")).unwrap();
    let unreadable = crate::pro::ProState::new(state.pro.root.clone());
    assert!(!state.pro.root.join("state.json.damaged").exists());
    assert!(!lock(&unreadable.execution.uncertain).contains("w-mirrored"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn device_shaped_grant_cannot_bypass_a_persisted_workers_restart_fence() {
    for evidence in ["unclean", "uncertain"] {
        let (state, config, root) = fixture();
        // A later request can call itself a device; the installation's
        // persisted worker marker remains the execution boundary.
        worker_fixture(&state);
        if evidence == "unclean" {
            lock(&state.pro.execution.unclean).insert("w-a".into(), Vec::new());
        } else {
            lock(&state.pro.execution.uncertain).insert("w-a".into());
        }
        let error = accept(&state, &config, &baton(), 0, RequestStart::now()).unwrap_err();
        assert!(error.to_string().contains("previous managed processes"));
        assert!(lock(&state.pro.execution.proofs).is_empty());
        state.pro.worker.store(false, Ordering::Release);
        accept(&state, &config, &baton(), 0, RequestStart::now()).unwrap();
        assert!(lock(&state.pro.execution.proofs).contains_key("w-a"));
        std::fs::remove_dir_all(root).unwrap();
    }
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
/// The account refused a legacy release as enrolled: the project is latched, so
/// the next pass routes it through the newer path (once an authoritative read
/// has restored its policy) and never silently back through the legacy one.
#[test]
fn a_project_the_account_requires_on_the_newer_path_is_latched_for_it() {
    let (state, config, root) = fixture();
    assert!(effective(&state, &config, "w-a")
        .unwrap()
        .execution
        .is_none());
    assert_eq!(
        path(
            &effective(&state, &config, "w-a").unwrap(),
            "w-a",
            "release"
        ),
        "/v1/baton/w-a/release"
    );
    assert!(require_v2(&state, "w-a"), "newly latched");
    assert!(!require_v2(&state, "w-a"), "a repeat is a no-op");
    assert!(managed(&state, "w-a"));
    assert!(!require_v2(&state, "not a workspace id"));
    // No policy yet: it waits for the account's answer instead of falling back.
    assert!(effective(&state, &config, "w-a").is_err());
    observe(&state, &config, &baton()).unwrap();
    assert_eq!(
        path(
            &effective(&state, &config, "w-a").unwrap(),
            "w-a",
            "release"
        ),
        "/v2/baton/w-a/release"
    );
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

#[tokio::test]
async fn graceful_same_boot_restart_does_not_fence_but_a_crash_probes_survivors() {
    let (state, config, root) = fixture();
    worker_fixture(&state);
    accept(&state, &config, &baton(), 0, RequestStart::now()).unwrap();
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 2 });
    crate::pro::ensure_root(&state.pro.root).await.unwrap();
    let intent = prepare_launch(&state, "w-a").await.unwrap().unwrap();
    let agent = state
        .sessions
        .spawn_managed(chimaera_pty::SpawnOpts {
            cwd: root.clone(),
            name: None,
            cols: 80,
            rows: 24,
            command: Some(vec!["/bin/sh".into()]),
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .unwrap();
    lock(&state.agents).insert(
        agent.id.clone(),
        crate::agent_state::AgentRecord::new("k".into(), crate::agent_state::AgentKind::Claude),
    );
    lock(&state.session_workspaces).insert(agent.id.clone(), "w-a".into());
    intent.registered(agent.id.clone());
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let saved = std::fs::read(state.pro.root.join("state.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
            if saved.as_ref().is_some_and(|value| {
                value["preferences"]["w-a"]["execution_launch_pending"] != true
                    && value["preferences"]["w-a"]["execution_groups"]
                        == serde_json::json!([agent.pid.unwrap()])
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // A crash here leaves a live recorded group: a successor must wait for it.
    let crashed = crate::pro::ProState::new(state.pro.root.clone());
    assert_eq!(
        lock(&crashed.execution.unclean)["w-a"]
            .iter()
            .map(|(group, _)| *group)
            .collect::<Vec<_>>(),
        vec![agent.pid.unwrap()]
    );
    // A graceful stop proves the agents exited and clears the evidence.
    shutdown(&state).await.unwrap();
    assert!(!state.sessions.get(&agent.id).is_some_and(|s| s.alive));
    let restarted = crate::pro::ProState::new(state.pro.root.clone());
    assert!(lock(&restarted.execution.unclean).is_empty());
    assert!(!lock(&restarted.preferences)["w-a"].execution_active);
    std::fs::remove_dir_all(root).unwrap();
}

/// Review R3 B4: switching "Keep on this computer only" on takes the project
/// out of the lease loop; its agents are never fenced for a lease nobody
/// renews any more.
#[test]
fn keeping_a_project_on_this_computer_never_fences_its_agents() {
    let (state, _config, root) = fixture();
    crate::pro::install_execution_fixture(&state, "w-a", 2).unwrap();
    lock(&state.pro.preferences)
        .entry("w-a".into())
        .or_default()
        .never_mirror = true;
    lock(&state.pro.execution.proofs)
        .get_mut("w-a")
        .unwrap()
        .deadline = lease::Deadline::expired_fixture();
    assert!(expire(&state, 0).is_empty());
    assert!(!fenced(&state, "w-a"));
    assert!(crate::pro::may_execute(&state, "w-a"));
    // Its sessions restore after a restart without a lease.
    assert!(restorable(&state, "w-a"));
    // Switching it back puts it in the loop again; the next renewal installs
    // a fresh proof.
    lock(&state.pro.preferences)
        .get_mut("w-a")
        .unwrap()
        .never_mirror = false;
    assert!(expire(&state, 0).is_empty());
    // A switch the account has not acknowledged yet keeps the lease: the
    // cloud could still take the project, so a lapse still fences.
    crate::pro::install_execution_fixture(&state, "w-a", 3).unwrap();
    {
        let mut preferences = lock(&state.pro.preferences);
        let preference = preferences.get_mut("w-a").unwrap();
        preference.never_mirror = true;
        preference.privacy_pending = true;
    }
    lock(&state.pro.execution.proofs)
        .get_mut("w-a")
        .unwrap()
        .deadline = lease::Deadline::expired_fixture();
    assert_eq!(expire(&state, 0), vec!["w-a"]);
    std::fs::remove_dir_all(root).unwrap();
}

/// Review R4 S1: what a lapse fence preserved carries the epoch it was fenced
/// at, persisted with the ledger. It resumes only while this computer holds
/// that epoch or one it re-acquired straight from it; once a grant shows
/// another machine held the project in between, it leaves for Recents, even
/// after a restart lost every in-memory record of the fence.
#[tokio::test]
async fn a_fenced_conversation_resumes_only_in_its_own_epoch() {
    let (state, config, root) = fixture();
    let grant = |epoch: u64| -> Baton {
        serde_json::from_value(json!({"workspace_id":"w-a","holder_id":"d-home","epoch":epoch,
            "requires_fork":false,"server_now":"2026-09-28T00:00:00Z","expires_at":"2026-09-28T00:01:30Z",
            "continuity":{"version":2,"mode":"managed_v1","policy_revision":1,"preferred_installation_id":"i-home"},
            "execution_capability":{"version":1,"boundary":"managed_processes","expired_takeover":false},
            "execution_lease":{"id":format!("lease-{epoch}"),"sequence":1}}))
        .unwrap()
    };
    accept(&state, &config, &grant(2), 0, RequestStart::now()).unwrap();
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 2 });
    let entry = |id: &str, fence: Option<u64>, handoff: bool| crate::ledger::LedgerEntry {
        id: id.into(),
        suspended: true,
        manual_resume_reason: None,
        fence_epoch: fence,
        handoff: handoff.then_some(crate::bundle::HandoffResume {
            fork: false,
            origin: crate::bundle::Origin::Home,
            epoch: 4,
        }),
        workspace_id: "w-a".into(),
        cwd: root.clone(),
        pinned_name: None,
        cols: 80,
        rows: 24,
        theme: "dark".into(),
        created_at: 0,
        agent: None,
    };
    // Survives a restart: the ledger keeps the fence epoch (and only then).
    let stale = entry("s-stale", Some(2), false);
    let saved = stale.to_json();
    assert_eq!(saved["fence_epoch"], 2);
    assert!(entry("s-other", None, false)
        .to_json()
        .get("fence_epoch")
        .is_none());
    let restored = crate::ledger::LedgerEntry::from_json(&saved).unwrap();
    assert_eq!(restored.fence_epoch, Some(2));
    {
        let mut deferred = lock(&state.deferred_sessions);
        deferred.insert("s-stale".into(), restored);
        deferred.insert("s-imported".into(), entry("s-imported", None, true));
        deferred.insert("s-other".into(), entry("s-other", None, false));
    }
    let current = |id: &str| {
        let entry = lock(&state.deferred_sessions).get(id).cloned().unwrap();
        crate::pro::fence_current(&state, &entry)
    };
    assert!(current("s-stale"), "still this computer's epoch");
    // Its own lapsed epoch re-acquired, nobody in between: still its own.
    lock(&state.pro.execution.proofs)
        .get_mut("w-a")
        .unwrap()
        .stopped = true;
    lock(&state.pro.execution.proofs).remove("w-a");
    accept(&state, &config, &grant(3), 0, RequestStart::now()).unwrap();
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 3 });
    assert_eq!(
        lock(&state.deferred_sessions)["s-stale"].fence_epoch,
        Some(3)
    );
    assert!(current("s-stale"));
    // Lapsed again; the cloud held epoch 4 and gave it back at 5. While
    // that return is still installing, nothing is settled yet: its own copy
    // may bring the conversation back (`finish_hydration` settles after).
    lock(&state.pro.execution.proofs).remove("w-a");
    lock(&state.pro.ownership).insert("w-a".into(), Ownership::Hydrating { epoch: 5 });
    accept(&state, &config, &grant(5), 0, RequestStart::now()).unwrap();
    assert!(lock(&state.deferred_sessions).contains_key("s-stale"));
    super::super::settle_fenced_here(&state, "w-a", Some(5));
    let deferred = lock(&state.deferred_sessions);
    assert!(!deferred.contains_key("s-stale"), "ran elsewhere: settled");
    assert!(deferred.contains_key("s-imported"));
    assert!(deferred.contains_key("s-other"), "not fenced: left alone");
    drop(deferred);
    assert_eq!(lock(&state.pro.settled).len(), 1, "on its way to Recents");
    // A fenced entry whose epoch this computer does not hold never resumes.
    lock(&state.deferred_sessions).insert("s-late".into(), entry("s-late", Some(4), false));
    assert!(!current("s-late"));
    std::fs::remove_dir_all(root).unwrap();
}

/// Review R4 S5: a wake checks the deadlines at once. A computer whose lease
/// lapsed past anyone's takeover while it slept is fenced by the wake itself,
/// not a tick later; one still inside that window renews first.
#[tokio::test]
async fn a_wake_fences_a_long_lapsed_lease_at_once() {
    let (state, _config, root) = fixture();
    crate::pro::install_execution_fixture(&state, "w-a", 2).unwrap();
    assert_eq!(
        watchdog::tick(&state),
        Duration::from_millis(100),
        "a held lease"
    );
    lock(&state.pro.execution.proofs)
        .get_mut("w-a")
        .unwrap()
        .deadline = lease::Deadline::lapsed_recently_fixture();
    crate::pro::routes::woke(&state).await;
    assert!(!fenced(&state, "w-a"), "inside the window: renewal first");
    assert!(resuming(&state, "w-a"));
    {
        let mut proofs = lock(&state.pro.execution.proofs);
        let proof = proofs.get_mut("w-a").unwrap();
        proof.renew_until = None;
        proof.deadline = lease::Deadline::expired_fixture();
    }
    crate::pro::routes::woke(&state).await;
    assert!(fenced(&state, "w-a"), "fenced by the wake itself");
    assert!(!crate::pro::may_execute(&state, "w-a"));
    lock(&state.pro.execution.proofs).clear();
    assert_eq!(
        watchdog::tick(&state),
        Duration::from_secs(1),
        "nothing held"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// A lapse fence keeps only what it stops: a conversation a clean hand-over
/// already stopped and recorded keeps its own record (its hand-off, no fence
/// epoch), so seeing the project held elsewhere never sends it to Recents
/// (the full loopback run's step 11).
#[tokio::test]
async fn a_lapse_never_marks_a_conversation_a_hand_over_already_kept() {
    let (state, _config, root) = fixture();
    crate::pro::install_execution_fixture(&state, "w-a", 2).unwrap();
    let handed = crate::ledger::LedgerEntry {
        id: "s-handed".into(),
        suspended: true,
        manual_resume_reason: None,
        fence_epoch: None,
        handoff: Some(crate::bundle::HandoffResume {
            fork: false,
            origin: crate::bundle::Origin::Home,
            epoch: 2,
        }),
        workspace_id: "w-a".into(),
        cwd: root.clone(),
        pinned_name: None,
        cols: 80,
        rows: 24,
        theme: "dark".into(),
        created_at: 0,
        agent: None,
    };
    lock(&state.deferred_sessions).insert("s-handed".into(), handed.clone());
    watchdog::preserve(&state, &["w-a".to_string()]);
    assert_eq!(lock(&state.deferred_sessions)["s-handed"], handed);
    crate::pro::install_remote_owner_fixture(&state, "w-a", 3);
    super::super::settle_fenced_here(&state, "w-a", None);
    assert!(lock(&state.deferred_sessions).contains_key("s-handed"));
    std::fs::remove_dir_all(root).unwrap();
}
