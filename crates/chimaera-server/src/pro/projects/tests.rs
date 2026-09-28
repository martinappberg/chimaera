use super::*;
use axum::{body::Body, http::Request, routing::any, Router};
use std::sync::{atomic::AtomicUsize, Mutex};

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
    assert!(reserve(&chosen, &[], &[]).is_err());
    assert!(verify(&destination, true).is_err());
    std::fs::remove_file(chosen.join("personal.txt")).unwrap();
    std::fs::create_dir(root.join(".git")).unwrap();
    assert!(reserve(&chosen, &[], &[]).is_err());
    std::fs::remove_dir(root.join(".git")).unwrap();
    std::fs::rename(&chosen, root.join("moved")).unwrap();
    assert!(verify(&destination, false).is_err());
    std::fs::create_dir(&chosen).unwrap();
    assert!(verify(&destination, false).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&chosen, root.join("alias")).unwrap();
        assert!(reserve(&root.join("alias"), &[], &[]).is_err());
    }
    assert!(reserve(&root.join("missing"), &[], &[]).is_err());
    assert!(!root.join("missing").exists());
    std::fs::remove_dir_all(root).unwrap();
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Adoption fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Adoption fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn repositories(root: &Path) -> String {
    let repo = root.join("source");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(repo.join("project.txt"), "cloud source\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "project"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    git(
        root,
        &[
            "clone",
            "--quiet",
            "--bare",
            repo.to_str().unwrap(),
            "repository.git",
        ],
    );
    git(&root.join("repository.git"), &["update-server-info"]);
    git(&repo, &["checkout", "--quiet", "--orphan", "config"]);
    git(&repo, &["rm", "-qrf", "."]);
    std::fs::write(repo.join("missing-environment.json"), "[]").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "config"]);
    git(&repo, &["checkout", "--quiet", "--orphan", "handoff"]);
    git(&repo, &["rm", "-qrf", "."]);
    std::fs::write(repo.join("manifest.json"),serde_json::to_vec(&json!({"version":1,"workspace_id":"w-cloud","root":"/cloud/source","name":"Cloud project","epoch":3,"clean":true,"branch":"refs/heads/main","profile":{},"sessions":[]})).unwrap()).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "handoff"]);
    git(
        root,
        &[
            "clone",
            "--quiet",
            "--bare",
            repo.to_str().unwrap(),
            "working-tree.git",
        ],
    );
    git(&root.join("working-tree.git"), &["update-server-info"]);
    head
}
struct Fixture {
    root: PathBuf,
    origin: String,
    requests: Mutex<Vec<String>>,
    handoffs: AtomicUsize,
    acquires: AtomicUsize,
    holder: Mutex<Option<String>>,
    invalidate_on_repository_fetch: Mutex<Option<Arc<AppState>>>,
}
async fn respond(State(f): State<Arc<Fixture>>, request: Request<Body>) -> Response {
    let path = request.uri().path();
    if path.starts_with("/repository.git/") {
        if let Some(state) = lock(&f.invalidate_on_repository_fetch).take() {
            state.pro.generation.fetch_add(1, Ordering::AcqRel);
        }
    }
    lock(&f.requests).push(format!("{} {path}", request.method()));
    let baton = || json!({"workspace_id":"w-cloud","holder_id":lock(&f.holder).clone(),"epoch":3,"expires_at":"2099-01-01T00:00:00Z","server_now":"2026-01-01T00:00:00Z","requires_fork":false});
    match path {
        "/v1/hosts" => Json(json!([{"id":"worker-1","alias":"Cloud","kind":"worker","status":"connected"}])).into_response(),
        "/v1/hosts/worker-1/http/api/v1/workspaces" => Json(json!([{"id":"w-cloud","name":"Cloud project"},{"id":"w-setup","name":"Setup","cloud_internal":true}])).into_response(),
        "/v1/hosts/worker-1/http/api/v1/pro/handoff" => { f.handoffs.fetch_add(1,Ordering::Relaxed);*lock(&f.holder)=None;StatusCode::NO_CONTENT.into_response() },
        "/v1/baton/w-cloud" | "/v1/baton/w-cloud/renew" => Json(baton()).into_response(),
        "/v1/baton/w-cloud/acquire" => { f.acquires.fetch_add(1,Ordering::Relaxed);*lock(&f.holder)=Some("device-1".into());Json(baton()).into_response() },
        "/v1/mirror/credentials" => Json(json!({"workspace_id":"w-cloud","repository_url":format!("{}/repository.git",f.origin),"working_tree_url":format!("{}/working-tree.git",f.origin),"username":"fixture","password":"synthetic-fixture","read_only":true,"storage_limit_bytes":10485760,"max_file_bytes":1000000})).into_response(),
        _ if path.starts_with("/repository.git/") || path.starts_with("/working-tree.git/") => {
            if path.contains("..") { return StatusCode::BAD_REQUEST.into_response(); }
            match tokio::fs::read(f.root.join(path.trim_start_matches('/'))).await {
                Ok(bytes) => ([("content-type","application/octet-stream")],bytes).into_response(),
                Err(_) => StatusCode::NOT_FOUND.into_response(),
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
fn configure(state: &AppState, origin: &str) {
    *lock(&state.pro.runtime) = Some(Configure {
        account_id: Some("account-fixture".into()),
        role: Role::Device,
        endpoint: origin.into(),
        keeper_url: origin.into(),
        hours_exhausted: false,
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
async fn passive_discovery_then_explicit_real_git_adoption_preserves_roots_and_retries() {
    let root = temp();
    let fixture_root = root.clone();
    let head = tokio::task::spawn_blocking(move || repositories(&fixture_root))
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let fixture = Arc::new(Fixture {
        root: root.clone(),
        origin: origin.clone(),
        requests: Mutex::new(Vec::new()),
        handoffs: AtomicUsize::new(0),
        acquires: AtomicUsize::new(0),
        holder: Mutex::new(Some("worker-1".into())),
        invalidate_on_repository_fetch: Mutex::new(None),
    });
    let router = Router::new()
        .fallback(any(respond))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let laptop = state(&root.join("laptop"));
    configure(&laptop, &origin);
    *lock(&laptop.pro.projects_root) = Some(root.join("legacy-global-folder"));
    let listing = list(&laptop).await;
    assert_eq!(listing.projects.len(), 1);
    assert!(listing.projects[0].local_root.is_none());
    assert!(lock(&laptop.workspaces).list().is_empty());
    assert!(!root.join("legacy-global-folder").exists());
    assert!(lock(&fixture.requests)
        .iter()
        .all(|request| request.starts_with("GET ")));
    assert!(lock(&laptop.pro.adoptions).is_empty());
    let chosen = root.join("chosen");
    std::fs::create_dir(&chosen).unwrap();
    std::fs::write(chosen.join("personal.txt"), "untouched").unwrap();
    assert!(open(
        &laptop,
        Open {
            expected_account_id: "account-fixture".into(),
            expected_endpoint: origin.clone(),
            workspace_id: "w-cloud".into(),
            destination_root: Some(chosen.clone())
        }
    )
    .await
    .is_err());
    assert_eq!(fixture.handoffs.load(Ordering::Relaxed), 0);
    assert_eq!(
        std::fs::read_to_string(chosen.join("personal.txt")).unwrap(),
        "untouched"
    );
    std::fs::remove_file(chosen.join("personal.txt")).unwrap();
    let opened = open(
        &laptop,
        Open {
            expected_account_id: "account-fixture".into(),
            expected_endpoint: origin.clone(),
            workspace_id: "w-cloud".into(),
            destination_root: Some(chosen.clone()),
        },
    )
    .await
    .unwrap();
    assert_eq!(opened["workspace_id"], "w-cloud");
    assert_eq!(opened["root"], chosen.to_string_lossy().as_ref());
    assert_eq!(
        std::fs::read_to_string(chosen.join("project.txt")).unwrap(),
        "cloud source\n"
    );
    assert_eq!(git(&chosen, &["rev-parse", "HEAD"]), head);
    assert_eq!(fixture.handoffs.load(Ordering::Relaxed), 1);
    std::fs::write(chosen.join("project.txt"), "new local work\n").unwrap();
    let repeated = open(
        &laptop,
        Open {
            expected_account_id: "account-fixture".into(),
            expected_endpoint: origin.clone(),
            workspace_id: "w-cloud".into(),
            destination_root: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(opened, repeated);
    assert_eq!(fixture.handoffs.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.acquires.load(Ordering::Relaxed), 1);
    assert_eq!(
        std::fs::read_to_string(chosen.join("project.txt")).unwrap(),
        "new local work\n"
    );
    lock(&laptop.pro.runtime).as_mut().unwrap().account_id = Some("different-account".into());
    assert!(local_root(&laptop, "w-cloud").is_none());
    assert!(!account_matches(&laptop, "w-cloud"));
    assert!(open(
        &laptop,
        Open {
            expected_account_id: "account-fixture".into(),
            expected_endpoint: origin.clone(),
            workspace_id: "w-cloud".into(),
            destination_root: Some(chosen.clone())
        }
    )
    .await
    .is_err());
    assert_eq!(
        std::fs::read_to_string(chosen.join("project.txt")).unwrap(),
        "new local work\n"
    );
    assert_eq!(fixture.handoffs.load(Ordering::Relaxed), 1);
    configure(&laptop, &origin);
    let restarted = state(&root.join("laptop"));
    configure(&restarted, &origin);
    open(
        &restarted,
        Open {
            expected_account_id: "account-fixture".into(),
            expected_endpoint: origin.clone(),
            workspace_id: "w-cloud".into(),
            destination_root: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(chosen.join("project.txt")).unwrap(),
        "new local work\n"
    );
    assert_eq!(fixture.acquires.load(Ordering::Relaxed), 1);
    std::fs::rename(&chosen, root.join("moved")).unwrap();
    assert!(open(
        &restarted,
        Open {
            expected_account_id: "account-fixture".into(),
            expected_endpoint: origin.clone(),
            workspace_id: "w-cloud".into(),
            destination_root: None
        }
    )
    .await
    .is_err());
    assert!(!chosen.exists());
    assert_eq!(fixture.handoffs.load(Ordering::Relaxed), 1);
    // A second device's explicit choice records a different local root for the
    // same stable workspace identity, without moving the first device's files.
    *lock(&fixture.holder) = Some("worker-1".into());
    let second = state(&root.join("second-laptop"));
    configure(&second, &origin);
    let second_root = root.join("second-choice");
    std::fs::create_dir(&second_root).unwrap();
    let second_open = open(
        &second,
        Open {
            expected_account_id: "account-fixture".into(),
            expected_endpoint: origin.clone(),
            workspace_id: "w-cloud".into(),
            destination_root: Some(second_root.clone()),
        },
    )
    .await
    .unwrap();
    assert_eq!(second_open["workspace_id"], "w-cloud");
    assert_eq!(second_open["root"], second_root.to_string_lossy().as_ref());
    assert_eq!(
        std::fs::read_to_string(root.join("moved/project.txt")).unwrap(),
        "new local work\n"
    );
    assert_eq!(fixture.handoffs.load(Ordering::Relaxed), 2);
    drop(second);
    // Signing out during the repository network transfer must be observed
    // before git init/ref adoption or any working file installation starts.
    *lock(&fixture.holder) = Some("worker-1".into());
    let canceled = state(&root.join("canceled-device"));
    configure(&canceled, &origin);
    let cancel_root = root.join("cancel-target");
    std::fs::create_dir(&cancel_root).unwrap();
    *lock(&fixture.invalidate_on_repository_fetch) = Some(canceled.clone());
    assert!(open(
        &canceled,
        Open {
            expected_account_id: "account-fixture".into(),
            expected_endpoint: origin.clone(),
            workspace_id: "w-cloud".into(),
            destination_root: Some(cancel_root.clone())
        }
    )
    .await
    .is_err());
    assert!(std::fs::read_dir(&cancel_root).unwrap().next().is_none());
    assert!(!super::super::may_write(&canceled, "w-cloud"));
    assert!(lock(&canceled.workspaces).get("w-cloud").is_none());
    drop(canceled);
    server.abort();
    let _ = server.await;
    drop(restarted);
    drop(laptop);
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
    };
    std::fs::create_dir_all(root.join("daemon/pro")).unwrap();
    lock(&old.workspaces)
        .import_exact(workspace.clone())
        .unwrap();
    std::fs::write(root.join("daemon/pro/state.json"),serde_json::to_vec(&json!({"ownership":{"w-cloud":{"state":"remote","holder":"worker-1","epoch":3}},"preferences":{},"import_roots":{"w-cloud":target},"projects_root":root.join("legacy-global")})).unwrap()).unwrap();
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
    let result = open(
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

#[cfg(unix)]
#[tokio::test]
async fn scoped_worker_hydrates_real_git_only_into_registered_root() {
    let root = temp();
    let fixture_root = root.clone();
    let head = tokio::task::spawn_blocking(move || repositories(&fixture_root))
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let fixture = Arc::new(Fixture {
        root: root.clone(),
        origin: origin.clone(),
        requests: Mutex::new(Vec::new()),
        handoffs: AtomicUsize::new(0),
        acquires: AtomicUsize::new(0),
        holder: Mutex::new(None),
        invalidate_on_repository_fetch: Mutex::new(None),
    });
    let router = Router::new()
        .fallback(any(respond))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let worker = state(&root.join("scoped-daemon"));
    worker.stopping.store(true, Ordering::Release);
    let registered = root.join("registered-project");
    std::fs::create_dir(&registered).unwrap();
    let body = json!({"account_id":"account-fixture","role":"worker","endpoint":origin,"keeper_url":"","workspace_root":registered,
        "delegation":{"access_token":"synthetic","expires_at":"2099-01-01T00:00:00Z","scope":["baton","mirror"],"device_id":"device-1","workspace":{"workspace_id":"w-cloud","revision":2}}});
    let configured = super::super::routes::configure_workspace(
        State(worker.clone()),
        Json(serde_json::from_value(body).unwrap()),
    )
    .await;
    assert_eq!(configured.status(), StatusCode::OK);
    let config = lock(&worker.pro.runtime).clone().unwrap();
    engine::hydrate(&worker, &config, "w-cloud", 3, false, None)
        .await
        .unwrap();
    assert_eq!(
        lock(&worker.workspaces).get("w-cloud").unwrap().root,
        registered
    );
    assert_eq!(
        std::fs::read_to_string(registered.join("project.txt")).unwrap(),
        "cloud source\n"
    );
    assert_eq!(git(&registered, &["rev-parse", "HEAD"]), head);
    assert_eq!(fixture.handoffs.load(Ordering::Relaxed), 0);
    assert!(lock(&fixture.requests)
        .iter()
        .all(|path| !path.contains("/v1/hosts")));
    assert!(super::super::may_write(&worker, "w-cloud"));
    assert!(!super::super::may_write(&worker, "w-other"));
    server.abort();
    let _ = server.await;
    drop(worker);
    std::fs::remove_dir_all(root).unwrap();
}
