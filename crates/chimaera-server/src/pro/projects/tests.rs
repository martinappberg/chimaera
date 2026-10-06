use super::*;
use axum::{body::Body, http::Request, routing::any, Router};
use std::sync::Mutex;

fn temp() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "chimaera-project-adoption-{}",
        chimaera_core::generate_token()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}
fn state(root: &Path) -> Arc<AppState> {
    Arc::new(AppState::new(
        "local-test".into(),
        "fixture".into(),
        4242,
        0,
        root.into(),
        root.join("config"),
    ))
}
#[test]
fn destination_rejects_nonempty_git_nested_symlink_and_replaced_folders() {
    let root = temp();
    let chosen = root.join("chosen");
    std::fs::create_dir(&chosen).unwrap();
    let destination = reserve(&chosen, &[], &[]).unwrap();
    std::fs::write(chosen.join("personal.txt"), "keep").unwrap();
    // Each refusal names its stable code (`error_code` on the route's failure).
    let code = |error: Option<anyhow::Error>| open_error_code(&error.unwrap());
    assert_eq!(code(reserve(&chosen, &[], &[]).err()), "folder_not_empty");
    assert_eq!(code(verify(&destination, true).err()), "folder_not_empty");
    std::fs::remove_file(chosen.join("personal.txt")).unwrap();
    std::fs::create_dir(root.join(".git")).unwrap();
    assert_eq!(code(reserve(&chosen, &[], &[]).err()), "folder_nested");
    std::fs::remove_dir(root.join(".git")).unwrap();
    assert_eq!(
        code(reserve(&chosen, &[], std::slice::from_ref(&chosen)).err()),
        "folder_nested"
    );
    std::fs::rename(&chosen, root.join("moved")).unwrap();
    assert_eq!(code(verify(&destination, false).err()), "folder_missing");
    std::fs::create_dir(&chosen).unwrap();
    assert_eq!(code(verify(&destination, false).err()), "folder_moved");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&chosen, root.join("alias")).unwrap();
        assert_eq!(
            code(reserve(&root.join("alias"), &[], &[]).err()),
            "folder_unusable"
        );
    }
    assert_eq!(
        code(reserve(&root.join("missing"), &[], &[]).err()),
        "folder_missing"
    );
    assert!(!root.join("missing").exists());
    std::fs::remove_dir_all(root).unwrap();
}

fn configure(state: &AppState, origin: &str) {
    *lock(&state.pro.runtime) = Some(Configure {
        recovery: false,
        execution: None,
        account_id: Some("account-fixture".into()),
        role: Role::Device,
        endpoint: origin.into(),
        keeper_url: origin.into(),
        hours_exhausted: false,
        alias: None,
        delegation: super::super::protocol::Delegation {
            workspace: None,
            access_token: "synthetic".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            scope: vec!["baton".into(), "mirror".into()],
            device_id: "device-1".into(),
        },
    });
}
#[tokio::test]
async fn copy_requires_a_negotiated_checkpoint_without_legacy_fallback() {
    let root = temp();
    let chosen = root.join("chosen");
    std::fs::create_dir(&chosen).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(any(move |request: Request<Body>| {
        let seen = seen.clone();
        async move {
            lock(&seen).push(request.uri().path().to_owned());
            Json(json!({"workspace_id":"w-cloud","holder_id":"worker-1","epoch":3,"expires_at":"2099-01-01T00:00:00Z","server_now":"2026-01-01T00:00:00Z","requires_fork":false,"checkpoint":null})).into_response()
        }
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let laptop = state(&root.join("daemon"));
    configure(&laptop, &origin);
    let error = copy(
        &laptop,
        Open {
            expected_account_id: "account-fixture".into(),
            expected_endpoint: origin,
            workspace_id: "w-cloud".into(),
            destination_root: Some(chosen.clone()),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(open_error_code(&error), "checkpoint_pending");
    assert_eq!(&*lock(&requests), &["/v2/baton/w-cloud"]);
    assert!(lock(&laptop.pro.adoptions).is_empty());
    assert!(lock(&laptop.workspaces).list().is_empty());
    assert!(std::fs::read_dir(&chosen).unwrap().next().is_none());
    server.abort();
    drop(laptop);
    std::fs::remove_dir_all(root).unwrap();
}

/// A failed open answers with its message and, beside it, the stable code the
/// native app maps to its own words (additive: `error` and `code` are as before).
#[tokio::test]
async fn a_failed_open_carries_a_stable_error_code_beside_its_message() {
    let root = temp();
    let laptop = state(&root.join("daemon"));
    let failure = |workspace_id: &'static str| {
        let laptop = laptop.clone();
        async move {
            let response = copy_project(
                State(laptop),
                Json(CopyRequest {
                    copy_version: 1,
                    project: Open {
                        expected_account_id: "account-fixture".into(),
                        expected_endpoint: "http://127.0.0.1:1".into(),
                        workspace_id: workspace_id.into(),
                        destination_root: None,
                    },
                }),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        }
    };
    let invalid = failure("not a project id").await;
    assert_eq!(invalid["error_code"], "not_a_project");
    assert_eq!(invalid["error"], "Invalid project identity");
    assert!(invalid["code"].is_string(), "the diagnostic category stays");
    let signed_out = failure("w-cloud").await;
    assert_eq!(signed_out["error_code"], "signed_out");
    assert_eq!(signed_out["error"], "Sign in to open a cloud project");
    // Failures raised by the transfer engine fall back to its category.
    assert_eq!(
        open_error_code(&anyhow::anyhow!("Account changed during project transfer")),
        "account_changed"
    );
    assert_eq!(
        open_error_code(&anyhow::anyhow!("ownership changed before release")),
        "owned_elsewhere"
    );
    assert_eq!(
        open_error_code(&anyhow::anyhow!(
            "previous managed processes are still stopping"
        )),
        "busy"
    );
    assert_eq!(
        open_error_code(&anyhow::anyhow!("mirror Git operation failed")),
        "failed"
    );
    // A code survives added context.
    assert_eq!(
        open_error_code(&refuse("privacy", "kept on its device").context("while opening")),
        "privacy"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn legacy_pending_registration_is_fenced_until_explicit_folder_choice() {
    let root = temp();
    let target = root.join("old-auto-import");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("partial.txt"), "preserve partial import").unwrap();
    let old = state(&root.join("daemon"));
    let workspace = crate::workspaces::Workspace {
        id: "w-cloud".into(),
        root: target.clone(),
        name: "Cloud project".into(),
        last_opened_at: super::super::now(),
        mastermind: None,
        plugins_on: vec![],
        cloud_internal: false,
        hidden: false,
    };
    std::fs::create_dir_all(root.join("daemon/pro")).unwrap();
    lock(&old.workspaces)
        .import_exact(workspace.clone())
        .unwrap();
    std::fs::write(root.join("daemon/pro/state.json"),serde_json::to_vec(&json!({"ownership":{"w-cloud":{"state":"remote","holder":"1","epoch":3}},"preferences":{},"import_roots":{"w-cloud":target},"projects_root":root.join("legacy-global")})).unwrap()).unwrap();
    drop(old);
    let restored = state(&root.join("daemon"));
    assert!(adoption_pending(&restored, "w-cloud"));
    assert!(!super::super::may_write(&restored, "w-cloud"));
    assert!(!engine::eligible(&restored, &workspace));
    assert!(local_root(&restored, "w-cloud").is_none());
    super::super::persist(&restored).await.unwrap();
    let again = state(&root.join("daemon"));
    assert!(adoption_pending(&again, "w-cloud"));
    assert!(local_root(&again, "w-cloud").is_none());
    assert_eq!(
        std::fs::read_to_string(target.join("partial.txt")).unwrap(),
        "preserve partial import"
    );
    drop(again);
    drop(restored);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn configuration_snapshot_waits_for_account_transition() {
    let root = temp();
    let state = state(&root);
    configure(&state, "http://127.0.0.1:1");
    let gate = state.pro.configuration.lock().await;
    state.pro.generation.store(8, Ordering::Release);
    let owner = state.clone();
    let pending = tokio::spawn(async move { configuration(&owner).await });
    tokio::task::yield_now().await;
    assert!(!pending.is_finished());
    lock(&state.pro.runtime).as_mut().unwrap().account_id = Some("replacement".into());
    drop(gate);
    let (config, generation) = pending.await.unwrap();
    assert_eq!(generation, 8);
    assert_eq!(config.unwrap().account_id.as_deref(), Some("replacement"));
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn original_laptop_root_is_bound_to_its_account_before_automatic_return() {
    let root = temp();
    let project = root.join("original");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("keep.txt"), "local work").unwrap();
    let state = state(&root.join("daemon"));
    configure(&state, "http://127.0.0.1:1");
    std::fs::create_dir_all(root.join("daemon")).unwrap();
    let workspace = crate::workspaces::Workspace {
        id: "w-cloud".into(),
        root: project.clone(),
        name: "Existing laptop project".into(),
        last_opened_at: super::super::now(),
        mastermind: None,
        plugins_on: vec![],
        cloud_internal: false,
        hidden: false,
    };
    lock(&state.workspaces)
        .import_exact(workspace.clone())
        .unwrap();
    assert!(local_root(&state, "w-cloud").is_none());
    let config = lock(&state.pro.runtime).clone().unwrap();
    bind_workspace_account(&state, &config, "w-cloud").unwrap();
    assert_eq!(local_root(&state, "w-cloud"), Some(project.clone()));
    super::super::persist(&state).await.unwrap();
    lock(&state.pro.runtime).as_mut().unwrap().account_id = Some("other-account".into());
    assert!(!account_matches(&state, "w-cloud"));
    assert!(!engine::eligible(&state, &workspace));
    assert!(local_root(&state, "w-cloud").is_none());
    lock(&state.pro.project_cache).checked_at = super::super::now();
    let result = copy(
        &state,
        Open {
            workspace_id: "w-cloud".into(),
            destination_root: Some(project.clone()),
            expected_account_id: "other-account".into(),
            expected_endpoint: "http://127.0.0.1:1".into(),
        },
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(project.join("keep.txt")).unwrap(),
        "local work"
    );
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cache_wait_cannot_admit_an_old_account_configuration() {
    let root = temp();
    let owner = state(&root);
    configure(&owner, "http://127.0.0.1:1");
    let config = lock(&owner.pro.runtime).clone().unwrap();
    for snapshot in [false, true] {
        let cache = owner.pro.cache("w-generation").unwrap();
        let guard = cache.lock_owned().await;
        let future = async {
            if snapshot {
                engine::snapshot(&owner, &config, "w-generation", false).await
            } else {
                engine::hydrate(&owner, &config, "w-generation", 1, false, None).await
            }
        };
        tokio::pin!(future);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut future)
            .await
            .is_err());
        owner.pro.generation.fetch_add(1, Ordering::AcqRel);
        drop(guard);
        assert_eq!(
            future.await.unwrap_err().to_string(),
            "Account changed while waiting for project cache"
        );
        assert!(!owner.pro.root.join("w-generation").exists());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn copying_a_project_this_computer_owns_is_inert() {
    let root = temp();
    let laptop = state(&root.join("daemon"));
    configure(&laptop, "http://127.0.0.1:1");
    let chosen = root.join("chosen");
    std::fs::create_dir(&chosen).unwrap();
    std::fs::write(chosen.join("keep.txt"), "owned local work").unwrap();
    let workspace = crate::workspaces::Workspace {
        id: "w-cloud".into(),
        root: chosen.clone(),
        name: "Local owner".into(),
        last_opened_at: 0,
        mastermind: None,
        plugins_on: vec![],
        cloud_internal: false,
        hidden: false,
    };
    lock(&laptop.workspaces).import_exact(workspace).unwrap();
    let config = lock(&laptop.pro.runtime).clone().unwrap();
    bind_workspace_account(&laptop, &config, "w-cloud").unwrap();
    lock(&laptop.pro.ownership).insert(
        "w-cloud".into(),
        super::super::Ownership::Local { epoch: 9 },
    );
    lock(&laptop.pro.project_cache).checked_at = super::super::now();
    let request = || Open {
        workspace_id: "w-cloud".into(),
        destination_root: None,
        expected_account_id: "account-fixture".into(),
        expected_endpoint: "http://127.0.0.1:1".into(),
    };
    let ack = copy(&laptop, request()).await.unwrap();
    assert_eq!(ack["state"], "owned_local");
    assert!(super::super::may_execute(&laptop, "w-cloud"));
    assert!(lock(&laptop.pro.preferences)
        .get("w-cloud")
        .unwrap()
        .copy
        .is_none());
    assert!(!laptop.pro.root.join("copy-authority.json").exists());
    assert_eq!(
        std::fs::read_to_string(chosen.join("keep.txt")).unwrap(),
        "owned local work"
    );
    drop(laptop);
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn refused_takeover_start_retires_its_saved_intent_and_keeps_copy_reopenable() {
    for limited in [false, true] {
        let root = temp();
        let laptop = state(&root.join("daemon"));
        configure(&laptop, "http://127.0.0.1:1");
        lock(&laptop.pro.runtime).as_mut().unwrap().execution=Some(serde_json::from_value(json!({"version":1,"installation_id":"i-fixture","capability":super::super::execution::wire::ExecutionCapability::checkpoint_fork()})).unwrap());
        let chosen = root.join("chosen");
        std::fs::create_dir(&chosen).unwrap();
        let mut destination = reserve(&chosen, &[], &[]).unwrap();
        destination.account = account_scope(&lock(&laptop.pro.runtime).clone().unwrap());
        destination.started = true;
        destination.complete = true;
        lock(&laptop.pro.adoptions).insert("w-cloud".into(), destination);
        lock(&laptop.workspaces)
            .import_exact(crate::workspaces::Workspace {
                id: "w-cloud".into(),
                root: chosen,
                name: "Copy".into(),
                last_opened_at: 0,
                mastermind: None,
                plugins_on: vec![],
                cloud_internal: false,
                hidden: false,
            })
            .unwrap();
        lock(&laptop.pro.preferences)
            .entry("w-cloud".into())
            .or_default()
            .copy = Some(super::super::project_copy::CopyState {
            checkpoint: None,
            pending: None,
            ready: true,
            takeover_requested: false,
            takeover_request: None,
            owner_epoch: Some(3),
        });
        if limited {
            super::super::moves::fill_requests_fixture(&laptop);
        } else {
            lock(&laptop.pro.parked).insert("w-cloud".into());
        }
        assert!(takeover(
            &laptop,
            TakeoverRequest {
                workspace_id: "w-cloud".into(),
                expected_account_id: "account-fixture".into(),
                expected_endpoint: "http://127.0.0.1:1".into(),
                expected_epoch: 3
            }
        )
        .await
        .is_err());
        let copy = lock(&laptop.pro.preferences)
            .get("w-cloud")
            .unwrap()
            .copy
            .clone()
            .unwrap();
        assert!(copy.ready);
        assert!(!copy.takeover_requested);
        assert!(copy.takeover_request.is_none());
        assert_eq!(
            super::super::project_copy::view(&laptop, "w-cloud").unwrap()["state"],
            "ready"
        );
        drop(laptop);
        std::fs::remove_dir_all(root).unwrap();
    }
}
