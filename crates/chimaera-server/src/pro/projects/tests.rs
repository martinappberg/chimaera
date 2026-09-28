use super::*;
use axum::{body::Body, http::Request, routing::any, Router};
use std::sync::{
    atomic::{AtomicU64, AtomicUsize},
    Mutex,
};

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
    epoch: AtomicU64,
    holder_after_next_read: Mutex<Option<Option<String>>>,
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
    let baton = || json!({"workspace_id":"w-cloud","holder_id":lock(&f.holder).clone(),"epoch":f.epoch.load(Ordering::SeqCst),"expires_at":"2099-01-01T00:00:00Z","server_now":"2026-01-01T00:00:00Z","requires_fork":false});
    match path {
        "/v1/hosts" => Json(json!([{"id":"worker-1","alias":"Cloud","kind":"worker","status":"connected"}])).into_response(),
        "/v1/hosts/worker-1/http/api/v1/workspaces" => Json(json!([{"id":"w-cloud","name":"Cloud project"},{"id":"w-setup","name":"Setup","cloud_internal":true}])).into_response(),
        "/v1/hosts/worker-1/http/api/v1/pro/handoff" => { f.handoffs.fetch_add(1,Ordering::Relaxed);*lock(&f.holder)=None;StatusCode::NO_CONTENT.into_response() },
        "/v1/baton/w-cloud" => {let reply=baton();if let Some(next)=lock(&f.holder_after_next_read).take(){*lock(&f.holder)=next;}Json(reply).into_response()},
        "/v1/baton/w-cloud/renew" => Json(baton()).into_response(),
        "/v1/baton/w-cloud/acquire" => {let bytes=axum::body::to_bytes(request.into_body(),4096).await.unwrap();let body:serde_json::Value=serde_json::from_slice(&bytes).unwrap();let requested=body["holder_id"].as_str().unwrap();if body["expected_epoch"].as_u64()!=Some(f.epoch.load(Ordering::SeqCst)) || lock(&f.holder).as_deref().is_some_and(|holder|holder!=requested){return StatusCode::CONFLICT.into_response();}f.acquires.fetch_add(1,Ordering::Relaxed);*lock(&f.holder)=Some(requested.into());Json(baton()).into_response() },
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
        epoch: AtomicU64::new(3),
        holder_after_next_read: Mutex::new(None),
        root: root.clone(),
        origin: origin.clone(),
        requests: Mutex::new(Vec::new()),
        handoffs: AtomicUsize::new(0),
        acquires: AtomicUsize::new(0),
        holder: Mutex::new(Some("1".into())),
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
    *lock(&fixture.holder) = Some("1".into());
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
    *lock(&fixture.holder) = Some("1".into());
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

#[test]
fn lazy_return_preserves_finished_conversation_through_real_worker_route() {
    // Agent launch helpers read process-global home paths. Keep this synthetic
    // CLI and its imported conversation isolated from real provider settings.
    const CHILD: &str = "CHIMAERA_HANDBACK_IDENTITY_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = temp();
        std::fs::create_dir(root.join("home")).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "pro::projects::tests::lazy_return_preserves_finished_conversation_through_real_worker_route", "--nocapture"])
            .env(CHILD, "1")
            .env("CHIMAERA_HOME", root.join("app"))
            .env("HOME", root.join("home"))
            .env("SHELL", "/bin/sh")
            .output().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        use std::os::unix::fs::PermissionsExt;
        const SESSION: &str = "s-finished";
        const NATIVE: &str = "11111111-1111-4111-8111-111111111111";
        let root = PathBuf::from(std::env::var_os("CHIMAERA_HOME").unwrap()).parent().unwrap().to_path_buf();
        let fixture_root = root.clone();
        tokio::task::spawn_blocking(move || repositories(&fixture_root)).await.unwrap();
        let source = root.join("cloud-project");
        std::fs::create_dir(&source).unwrap();
        let cloud = state(&root.join("cloud-daemon"));
        let workspace = crate::workspaces::Workspace { id: "w-cloud".into(), root: source.clone(), name: "Cloud project".into(), last_opened_at: super::super::now(), mastermind: None, plugins_on: Vec::new(), cloud_internal: false };
        lock(&cloud.workspaces).import_exact(workspace.clone()).unwrap();
        let native = cloud.claude_projects_dir.join(crate::launcher::encode_cwd(&source)).join(format!("{NATIVE}.jsonl"));
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        let native_bytes = format!("{{\"sessionId\":\"{NATIVE}\",\"type\":\"assistant\",\"message\":{{\"content\":\"Finished the requested work.\"}}}}\n");
        std::fs::write(&native, &native_bytes).unwrap();
        let journal = format!("{}\n", serde_json::to_string(&chimaera_agent::journal::SeqEvent { seq: 17, ts: 123, ev: chimaera_agent::model::AgentEvent::Notice { text: "Completed conversation remains here".into() } }).unwrap());
        std::fs::create_dir_all(cloud.chat.journal_dir()).unwrap();
        std::fs::write(cloud.chat.journal_dir().join(format!("{SESSION}.jsonl")), &journal).unwrap();
        crate::ledger::defer(&cloud, crate::ledger::LedgerEntry {
            id: SESSION.into(), workspace_id: "w-cloud".into(), cwd: source, suspended: true, handoff: None, pinned_name: Some("Finished task".into()), cols: 80, rows: 24, theme: "dark".into(), created_at: 123,
            agent: Some(crate::ledger::LedgerAgent { kind: crate::agents::AgentKind::Claude, resume: Some(NATIVE.into()), transcript: None, native_cwd: None, title: "Finished task".into(), ui: chimaera_agent::model::SessionUi::Chat, model: None, carryover: Some(chimaera_agent::Carryover::default()) }),
        }).unwrap();
        let archive = crate::bundle::export(cloud.clone(), SESSION, crate::bundle::ExportMode::Snapshot).await.unwrap();
        let repository = root.join("source");
        std::fs::create_dir(repository.join("bundles")).unwrap();
        std::fs::rename(archive, repository.join(format!("bundles/{SESSION}.zip"))).unwrap();
        let manifest_path = repository.join("manifest.json");
        let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["sessions"] = json!([{"id": SESSION, "archive": format!("bundles/{SESSION}.zip")}]);
        std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let update = root.clone();
        tokio::task::spawn_blocking(move || {
            git(&update.join("source"), &["add", "."]);
            git(&update.join("source"), &["commit", "-qm", "finished conversation"]);
            git(&update.join("working-tree.git"), &["fetch", "--quiet", update.join("source").to_str().unwrap(), "+refs/heads/handoff:refs/heads/handoff"]);
            git(&update.join("working-tree.git"), &["update-server-info"]);
        }).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let fixture = Arc::new(Fixture { epoch: AtomicU64::new(3), holder_after_next_read: Mutex::new(None), root: root.clone(), origin: origin.clone(), requests: Mutex::new(Vec::new()), handoffs: AtomicUsize::new(0), acquires: AtomicUsize::new(0), holder: Mutex::new(Some("1".into())), invalidate_on_repository_fetch: Mutex::new(None) });
        let router = Router::new().fallback(any(respond)).with_state(fixture.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let laptop = state(&root.join("laptop"));
        configure(&laptop, &origin);
        let destination = root.join("original-project");
        std::fs::create_dir(&destination).unwrap();
        lock(&laptop.workspaces).import_exact(crate::workspaces::Workspace { root: destination.clone(), ..workspace }).unwrap();
        lock(&laptop.pro.ownership).insert("w-cloud".into(), super::super::Ownership::Remote { epoch: 3, holder: "1".into() });
        laptop.pro.power_suitable.store(true, Ordering::Release);
        laptop.pro.awake_since.store(super::super::now().saturating_sub(301), Ordering::Release);
        let script = root.join("claude-fixture");
        let inputs = root.join("received.jsonl");
        std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"init\",\"response\":{{\"commands\":[]}}}}}}'\nprintf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"{NATIVE}\",\"model\":\"fixture-model\",\"permissionMode\":\"default\"}}'\ncat > '{}'\n", inputs.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        lock(&laptop.agent_bins).insert(crate::agents::AgentKind::Claude, crate::launcher::AgentDetection { path: Ok(script), version: Some("2.1.283".into()), managed: false, explicit: true, mtime: None });
        let config = lock(&laptop.pro.runtime).clone().unwrap();
        engine::lazy_handback(&laptop, &config).await.unwrap();
        assert_eq!(fixture.handoffs.load(Ordering::Relaxed), 1, "raw baton holder must resolve to the full keeper route ID");
        assert_eq!(fixture.acquires.load(Ordering::Relaxed), 1);
        assert_eq!(super::super::owned_epoch(&laptop, "w-cloud"), Some(3));
        assert!(lock(&fixture.requests).iter().any(|path| path == "POST /v1/hosts/worker-1/http/api/v1/pro/handoff"));
        assert_eq!(std::fs::read_to_string(destination.join("project.txt")).unwrap(), "cloud source\n");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !laptop.chat.get(SESSION).is_some_and(|chat| chat.alive && chat.native_session_id.as_deref() == Some(NATIVE)) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert_eq!(lock(&laptop.session_workspaces).get(SESSION).map(String::as_str), Some("w-cloud"));
        assert_eq!(lock(&laptop.agents).get(SESSION).unwrap().state, crate::agent_state::AgentState::Finished);
        assert!(std::fs::read_to_string(laptop.chat.journal_dir().join(format!("{SESSION}.jsonl"))).unwrap().contains("Completed conversation remains here"));
        assert_eq!(std::fs::read_to_string(laptop.claude_projects_dir.join(crate::launcher::encode_cwd(&destination)).join(format!("{NATIVE}.jsonl"))).unwrap(), native_bytes);
        let recorded = std::fs::read_to_string(&inputs).unwrap_or_default();
        assert!(!recorded.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok()).any(|frame| frame["type"] == "user"), "finished conversation must not receive a new model turn");
        laptop.stopping.store(true, Ordering::Relaxed);
        laptop.chat.kill(SESSION);
        server.abort();
        let _ = server.await;
    });
}

#[test]
fn worker_poll_cannot_skip_roundtrip_hydration_or_diverge_snapshot_ancestry() {
    const CHILD: &str = "CHIMAERA_WORKER_ANCESTRY_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = temp();
        std::fs::create_dir(root.join("home")).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "pro::projects::tests::worker_poll_cannot_skip_roundtrip_hydration_or_diverge_snapshot_ancestry", "--nocapture"])
            .env(CHILD, "1").env("CHIMAERA_HOME", root.join("app"))
            .env("HOME", root.join("home")).env("SHELL", "/bin/sh")
            .output().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        use super::super::{mirror, Ownership};
        let root = PathBuf::from(std::env::var_os("CHIMAERA_HOME").unwrap())
            .parent()
            .unwrap()
            .to_path_buf();
        let fixture_root = root.clone();
        tokio::task::spawn_blocking(move || repositories(&fixture_root))
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let fixture = Arc::new(Fixture {
            epoch: AtomicU64::new(1),
            holder_after_next_read: Mutex::new(None),
            root: root.clone(),
            origin: origin.clone(),
            requests: Mutex::new(Vec::new()),
            handoffs: AtomicUsize::new(0),
            acquires: AtomicUsize::new(0),
            holder: Mutex::new(None),
            invalidate_on_repository_fetch: Mutex::new(None),
        });
        let app = Router::new()
            .fallback(any(respond))
            .with_state(fixture.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let make = |name: &str, role: Role| {
            let s = state(&root.join(name));
            configure(&s, &origin);
            let project = root.join(format!("{name}-project"));
            std::fs::create_dir(&project).unwrap();
            lock(&s.workspaces)
                .import_exact(crate::workspaces::Workspace {
                    id: "w-cloud".into(),
                    root: project,
                    name: "Fixture".into(),
                    last_opened_at: 1,
                    mastermind: None,
                    plugins_on: vec![],
                    cloud_internal: false,
                })
                .unwrap();
            let mut config = lock(&s.pro.runtime).clone().unwrap();
            config.role = role;
            config.delegation.device_id = name.into();
            *lock(&s.pro.runtime) = Some(config.clone());
            (s, config)
        };
        let (worker, worker_config) = make("worker-1", Role::Worker);
        let (device, device_config) = make("device-1", Role::Device);
        let remote = root.join("working-tree.git");
        let shadow = |s: &AppState| s.pro.root.join("w-cloud/working-tree.git");
        async fn publish(root: &Path, shadow: &Path, remote: &Path, label: &str, epoch: u64) {
            for branch in ["config", "handoff"] {
                let tree = root.join(format!("stage-{label}-{branch}"));
                std::fs::create_dir(&tree).unwrap();
                git(
                    shadow,
                    &[
                        "--work-tree",
                        tree.to_str().unwrap(),
                        "checkout",
                        &format!("refs/heads/{branch}"),
                        "--",
                        ".",
                    ],
                );
                if branch == "config" {
                    std::fs::create_dir_all(tree.join(".claude")).unwrap();
                    std::fs::write(
                        tree.join(".claude/settings.json"),
                        serde_json::to_vec(&json!({"theme":label})).unwrap(),
                    )
                    .unwrap();
                } else {
                    let path = tree.join("manifest.json");
                    let mut manifest: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    manifest["epoch"] = epoch.into();
                    std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
                    // A changing journal payload makes handoff ancestry independent
                    // of the working-tree branch, whose project files stay equal.
                    std::fs::write(
                        tree.join("synthetic-journal.jsonl"),
                        format!("{{\"completed\":\"{label}\"}}\n"),
                    )
                    .unwrap();
                }
                mirror::commit_tree(shadow, &tree, branch).await.unwrap();
            }
            git(
                shadow,
                &[
                    "push",
                    "--atomic",
                    remote.to_str().unwrap(),
                    "refs/heads/main",
                    "refs/heads/config",
                    "refs/heads/handoff",
                ],
            );
            git(remote, &["update-server-info"]);
        }
        engine::hydrate(&worker, &worker_config, "w-cloud", 1, false, None)
            .await
            .unwrap();
        publish(&root, &shadow(&worker), &remote, "worker-first", 1).await;
        *lock(&fixture.holder) = None;
        fixture.epoch.store(2, Ordering::SeqCst);
        lock(&device.pro.ownership).insert(
            "w-cloud".into(),
            Ownership::Remote {
                epoch: 1,
                holder: "worker-1".into(),
            },
        );
        engine::hydrate(&device, &device_config, "w-cloud", 2, false, None)
            .await
            .unwrap();
        publish(&root, &shadow(&device), &remote, "device-second", 2).await;
        let expected: Vec<_> = ["config", "handoff"]
            .into_iter()
            .map(|branch| {
                (
                    branch,
                    git(&remote, &["rev-parse", &format!("refs/heads/{branch}")]),
                )
            })
            .collect();
        *lock(&fixture.holder) = None;
        fixture.epoch.store(3, Ordering::SeqCst);
        // The sleeping worker missed the intermediate device ownership entirely.
        assert!(matches!(
            lock(&worker.pro.ownership).get("w-cloud"),
            Some(Ownership::Local { epoch: 1 })
        ));
        {
            let _job = worker.pro.jobs.lock().await;
            tokio::time::timeout(
                Duration::from_secs(2),
                engine::reconcile(&worker, &worker_config, "w-cloud"),
            )
            .await
            .expect("snapshot recovery must not deadlock on its own job lock")
            .unwrap();
            assert_eq!(super::super::owned_epoch(&worker, "w-cloud"), Some(1));
            assert_eq!(fixture.acquires.load(Ordering::SeqCst), 2);
        }
        engine::reconcile(&worker, &worker_config, "w-cloud")
            .await
            .unwrap();
        assert_eq!(
            fixture.acquires.load(Ordering::SeqCst),
            2,
            "worker lease polling must not acquire ahead of hydration"
        );
        assert_ne!(super::super::owned_epoch(&worker, "w-cloud"), Some(3));
        engine::hydrate(&worker, &worker_config, "w-cloud", 3, false, None)
            .await
            .unwrap();
        assert_eq!(fixture.acquires.load(Ordering::SeqCst), 3);
        for (branch, oid) in &expected {
            assert_eq!(
                git(
                    &shadow(&worker),
                    &["rev-parse", &format!("refs/heads/{branch}")]
                ),
                *oid,
                "outgoing shadow must start from the newly hydrated remote head"
            );
        }
        assert_eq!(
            git(
                &shadow(&worker),
                &["show", "refs/heads/handoff:synthetic-journal.jsonl"]
            ),
            "{\"completed\":\"device-second\"}"
        );
        publish(&root, &shadow(&worker), &remote, "worker-third", 3).await;
        for (branch, oid) in expected {
            git(
                &remote,
                &[
                    "merge-base",
                    "--is-ancestor",
                    &oid,
                    &format!("refs/heads/{branch}"),
                ],
            );
        }
        assert_eq!(super::super::owned_epoch(&worker, "w-cloud"), Some(3));
        let requests = lock(&fixture.requests).len();
        engine::hydrate(&worker, &worker_config, "w-cloud", 3, false, None)
            .await
            .unwrap();
        assert_eq!(fixture.acquires.load(Ordering::SeqCst), 3);
        assert!(lock(&fixture.requests)[requests..]
            .iter()
            .all(|path| path == "GET /v1/baton/w-cloud" || path == "POST /v1/baton/w-cloud/renew"));
        // Equal cached epochs are not proof of ownership after a clean release.
        // Exercise the normal route's job reservation around that retry.
        *lock(&fixture.holder) = None;
        {
            let _job = worker.pro.jobs.lock().await;
            engine::hydrate(&worker, &worker_config, "w-cloud", 3, false, None)
                .await
                .unwrap();
        }
        assert_eq!(fixture.acquires.load(Ordering::SeqCst), 4);
        assert_eq!(
            git(
                &shadow(&worker),
                &["show", "refs/heads/handoff:synthetic-journal.jsonl"]
            ),
            "{\"completed\":\"worker-third\"}"
        );
        // Ownership can change between the shortcut's read and reconciliation.
        // Neither an unowned busy-job no-op nor observing another holder is a
        // successful retained grant, even if cached Local was unchanged.
        *lock(&fixture.holder_after_next_read) = Some(None);
        {
            let _job = worker.pro.jobs.lock().await;
            engine::hydrate(&worker, &worker_config, "w-cloud", 3, false, None)
                .await
                .unwrap();
        }
        assert_eq!(fixture.acquires.load(Ordering::SeqCst), 5);
        *lock(&fixture.holder_after_next_read) = Some(Some("device-2".into()));
        {
            let _job = worker.pro.jobs.lock().await;
            assert!(engine::hydrate(&worker, &worker_config, "w-cloud", 3, false, None)
                .await
                .is_err());
        }
        assert_eq!(fixture.acquires.load(Ordering::SeqCst), 5);
        assert!(matches!(lock(&worker.pro.ownership).get("w-cloud"), Some(Ownership::Remote { holder, epoch:3 }) if holder == "device-2"));
        assert!(worker.chat.list().is_empty());
        server.abort();
        let _ = server.await;
    });
}
