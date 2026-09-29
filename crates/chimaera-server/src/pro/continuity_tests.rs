//! Loopback account fixtures for the ownership, flush and return paths. The
//! fake records every request; its Git endpoints refuse connections, so a
//! snapshot stops at its first network publication step.
use super::*;
use axum::{
    body::Bytes,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use std::{collections::HashMap, sync::Mutex, time::Duration as StdDuration};

pub(super) struct FakeAccount {
    pub endpoint: String,
    pub requests: Arc<Mutex<Vec<(String, String, serde_json::Value)>>>,
    pub baton: Arc<Mutex<serde_json::Value>>,
    /// The reply to acquire/renew: a status and body (a grant or a refusal).
    pub grant: Arc<Mutex<Option<(u16, serde_json::Value)>>>,
    pub delays: Arc<Mutex<HashMap<String, StdDuration>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for FakeAccount {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl FakeAccount {
    pub async fn start(baton: serde_json::Value) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let baton = Arc::new(Mutex::new(baton));
        let grant = Arc::new(Mutex::new(None));
        let delays = Arc::new(Mutex::new(HashMap::<String, StdDuration>::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (recorded, current, granted, delayed) = (
            requests.clone(),
            baton.clone(),
            grant.clone(),
            delays.clone(),
        );
        let router = Router::new().fallback(any(
            move |method: Method, uri: axum::http::Uri, body: Bytes| {
                let (recorded, current, granted, delayed) = (
                    recorded.clone(),
                    current.clone(),
                    granted.clone(),
                    delayed.clone(),
                );
                async move {
                    let path = uri.path().to_owned();
                    let body: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    lock(&recorded).push((method.to_string(), path.clone(), body.clone()));
                    let delay = lock(&delayed).get(&path).copied();
                    if let Some(delay) = delay {
                        tokio::time::sleep(delay).await;
                    }
                    respond(&method, &path, &body, &current, &granted)
                }
            },
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            endpoint,
            requests,
            baton,
            grant,
            delays,
            server,
        }
    }
    pub fn calls(&self, method: &str, path: &str) -> Vec<serde_json::Value> {
        lock(&self.requests)
            .iter()
            .filter(|(m, p, _)| m == method && p == path)
            .map(|(_, _, body)| body.clone())
            .collect()
    }
}
fn respond(
    method: &Method,
    path: &str,
    body: &serde_json::Value,
    baton: &Mutex<serde_json::Value>,
    grant: &Mutex<Option<(u16, serde_json::Value)>>,
) -> Response {
    let segments: Vec<_> = path.trim_start_matches('/').split('/').collect();
    match (method.as_str(), segments.as_slice()) {
        ("GET", [_, "baton", _]) => Json(lock(baton).clone()).into_response(),
        ("POST", [_, "baton", _, "acquire" | "renew"]) => match lock(grant).clone() {
            Some((200, grant)) => {
                *lock(baton) = grant.clone();
                Json(grant).into_response()
            }
            Some((status, body)) => (
                StatusCode::from_u16(status).unwrap_or(StatusCode::CONFLICT),
                Json(body),
            )
                .into_response(),
            None => StatusCode::CONFLICT.into_response(),
        },
        ("PUT", ["v1", "baton", _, "policy"]) => StatusCode::NO_CONTENT.into_response(),
        ("POST", [_, "mirror", "credentials"]) => Json(json!({
            "workspace_id": body["workspace_id"],
            // A refused connection fails Git at once; the global two-slot Git
            // budget is shared with every other test in this process.
            "repository_url": "http://127.0.0.1:9/git/repository.git",
            "working_tree_url": "http://127.0.0.1:9/git/working-tree.git",
            "username": "fixture",
            "password": "fixture-password",
            "read_only": body["epoch"].is_null(),
            "storage_limit_bytes": 64 * 1024 * 1024,
            "max_file_bytes": 8 * 1024 * 1024,
        }))
        .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

pub(super) fn temp(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "chimaera-continuity-{label}-{}",
        chimaera_core::generate_token()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}
pub(super) fn state(root: &Path) -> Arc<AppState> {
    Arc::new(AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.join("data"),
        root.join("home/.claude"),
    ))
}
pub(super) fn device(endpoint: &str) -> Configure {
    serde_json::from_value(json!({
        "account_id": "a-fixture",
        "role": "device",
        "endpoint": endpoint,
        "keeper_url": "",
        "hours_exhausted": false,
        "execution": {
            "version": 1,
            "installation_id": "i-home",
            "capability": execution::wire::ExecutionCapability::checkpoint_fork(),
        },
        "delegation": {
            "access_token": "synthetic",
            "expires_at": "2099-01-01T00:00:00Z",
            "scope": ["baton", "mirror"],
            "device_id": "d-home",
        },
    }))
    .unwrap()
}
pub(super) fn owned(
    workspace: &str,
    holder: &str,
    epoch: u64,
    lease: &str,
    sequence: u64,
) -> serde_json::Value {
    json!({
        "workspace_id": workspace,
        "holder_id": holder,
        "epoch": epoch,
        "requires_fork": false,
        "server_now": "2026-09-28T00:00:00Z",
        "expires_at": "2026-09-28T00:01:30Z",
        "continuity": {
            "version": 2,
            "mode": "checkpoint_fork_v1",
            "policy_revision": 1,
            "preferred_installation_id": "i-home",
        },
        "execution_capability": execution::wire::ExecutionCapability::checkpoint_fork(),
        "execution_lease": {"id": lease, "sequence": sequence},
    })
}
/// A registered project with one file, owned locally under a fresh v2 lease.
pub(super) fn project(
    state: &Arc<AppState>,
    root: &Path,
    config: &Configure,
    epoch: u64,
) -> crate::workspaces::Workspace {
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("notes.txt"), "laptop work\n").unwrap();
    let workspace = lock(&state.workspaces).add(project).unwrap();
    let grant: Baton =
        serde_json::from_value(owned(&workspace.id, "d-home", epoch, "lease-fixture", 1)).unwrap();
    execution::accept(state, config, &grant, 0, execution::RequestStart::now()).unwrap();
    lock(&state.pro.ownership).insert(workspace.id.clone(), Ownership::Local { epoch });
    workspace
}

#[tokio::test]
async fn negotiated_execution_publishes_handoff_eligibility_before_snapshot_bytes() {
    let root = temp("policy");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    for exhausted in [false, true] {
        let mut config = device(&account.endpoint);
        config.hours_exhausted = exhausted;
        let workspace = if exhausted {
            lock(&state.workspaces).list()[0].clone()
        } else {
            project(&state, &root, &config, 4)
        };
        *lock(&account.baton) = owned(&workspace.id, "d-home", 4, "lease-fixture", 1);
        // The fake has no Git service, so publication stops after eligibility.
        assert!(snapshot(&state, &config, &workspace.id, false)
            .await
            .is_err());
        let calls = account.calls("PUT", &format!("/v1/baton/{}/policy", workspace.id));
        assert_eq!(
            calls.last(),
            Some(&json!({
                "holder_id": "d-home",
                "epoch": 4,
                "handoff_enabled": !exhausted,
                "offline_takeover": !exhausted,
                "has_agents": false,
            })),
            "a v2-enrolled project must publish its automatic-continuation eligibility"
        );
    }
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

/// Send a request and hang up before the reply, as a relay that timed out,
/// once `midway` says the work is in progress.
async fn abandon(
    address: std::net::SocketAddr,
    path: &str,
    body: serde_json::Value,
    midway: impl Fn() -> bool,
) {
    use tokio::io::AsyncWriteExt;
    let body = body.to_string();
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    tokio::time::timeout(StdDuration::from_secs(30), async {
        while !midway() {
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("the work never started");
    drop(stream);
}
fn leftovers(project: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![project.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("stage-")
                || name.starts_with("hydrate-")
                || name.starts_with("index-")
                || name.ends_with(".lock")
            {
                found.push(name.clone());
            }
            if entry.file_type().unwrap().is_dir() && !name.starts_with("stage-") {
                pending.push(entry.path());
            }
        }
    }
    found
}

#[tokio::test]
async fn an_abandoned_flush_still_finishes_and_leaves_no_interrupted_git_state() {
    let root = temp("abandoned");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let config = device(&account.endpoint);
    let workspace = project(&state, &root, &config, 4);
    *lock(&account.baton) = owned(&workspace.id, "d-home", 4, "lease-fixture", 1);
    *lock(&state.pro.runtime) = Some(config.clone());
    // A previously SIGKILLed helper left locks, a temporary index and a copy.
    let mirror = state.pro.root.join(&workspace.id);
    mirror::initialize(&mirror.join("working-tree.git"))
        .await
        .unwrap();
    std::fs::write(mirror.join("working-tree.git/index-interrupted"), b"").unwrap();
    std::fs::create_dir_all(mirror.join("working-tree.git/refs/heads")).unwrap();
    std::fs::write(mirror.join("working-tree.git/refs/heads/main.lock"), b"").unwrap();
    std::fs::create_dir_all(mirror.join("stage-interrupted/tree")).unwrap();
    // The flush is still running when its caller gives up.
    lock(&account.delays).insert(
        format!("/v1/baton/{}/policy", workspace.id),
        StdDuration::from_secs(2),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = crate::app(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let policy = format!("/v1/baton/{}/policy", workspace.id);
    abandon(
        address,
        "/api/v1/pro/handoff",
        json!({"workspace_id": workspace.id, "expected_epoch": 4}),
        || !account.calls("PUT", &policy).is_empty(),
    )
    .await;
    // The caller left while agents were stopped and the flush was mid-way.
    assert!(matches!(
        lock(&state.pro.ownership).get(&workspace.id),
        Some(Ownership::Transferring { epoch: 4 })
    ));
    tokio::time::timeout(StdDuration::from_secs(60), async {
        while super::super::detached::running(&state) > 0
            || matches!(
                lock(&state.pro.ownership).get(&workspace.id),
                Some(Ownership::Transferring { .. })
            )
        {
            tokio::time::sleep(StdDuration::from_millis(50)).await;
        }
    })
    .await
    .expect("the abandoned flush must run to its end");
    // It reached the account after its caller left, then failed to publish
    // (the fixture has no Git service) and recovered instead of stranding.
    assert_eq!(
        account
            .calls("PUT", &format!("/v1/baton/{}/policy", workspace.id))
            .len(),
        1
    );
    assert!(
        crate::pro::may_write(&state, &workspace.id),
        "a failed flush returns the project to this computer"
    );
    assert_eq!(leftovers(&mirror), Vec::<String>::new());
    server.abort();
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

fn checkpoint(epoch: u64) -> serde_json::Value {
    json!({
        "id": "cp-fixture",
        "sequence": 1,
        "source_holder_id": "d-home",
        "source_epoch": epoch,
        "working_tree_oid": "a".repeat(40),
        "config_oid": "b".repeat(40),
        "handoff_oid": "c".repeat(40),
        "continuation": "idle",
    })
}

#[tokio::test]
async fn reacquiring_its_own_epoch_continues_local_work_without_install_or_fork() {
    for (label, holder, expires) in [
        ("released", None, None),
        ("lapsed", Some("d-home"), Some("2026-09-28T00:00:00Z")),
    ] {
        let root = temp(label);
        let state = state(&root);
        let account = FakeAccount::start(json!({})).await;
        let config = device(&account.endpoint);
        let workspace = project(&state, &root, &config, 4);
        // The daemon restarted (or woke from sleep) after this epoch.
        lock(&state.pro.ownership).insert(
            workspace.id.clone(),
            Ownership::AwaitingVerification { epoch: 4 },
        );
        let mut current = owned(&workspace.id, "d-home", 4, "lease-fixture", 1);
        current["holder_id"] = json!(holder);
        current["expires_at"] = json!(expires);
        current["server_now"] = json!("2026-09-28T00:05:00Z");
        current["checkpoint"] = checkpoint(4);
        *lock(&account.baton) = current;
        // The account treats a lapsed own lease as a takeover (requires_fork).
        let mut grant = owned(&workspace.id, "d-home", 5, "lease-next", 1);
        grant["requires_fork"] = json!(holder.is_some());
        grant["checkpoint"] = checkpoint(4);
        *lock(&account.grant) = Some((200, grant));
        reconcile(&state, &config, &workspace.id).await.unwrap();
        assert!(
            matches!(
                lock(&state.pro.ownership).get(&workspace.id),
                Some(Ownership::Local { epoch: 5 })
            ),
            "{label}"
        );
        assert!(
            account.calls("POST", "/v2/mirror/credentials").is_empty(),
            "{label}: no checkpoint is fetched or installed over newer local work"
        );
        assert!(!lock(&state.pro.preferences)[&workspace.id].execution_uncertain);
        assert!(!execution::recovery_context(&state, &workspace.id));
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn the_reconnect_grace_after_a_lapsed_lease_is_a_quiet_wait() {
    let root = temp("grace");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let config = device(&account.endpoint);
    let workspace = project(&state, &root, &config, 4);
    lock(&state.pro.ownership).insert(
        workspace.id.clone(),
        Ownership::AwaitingVerification { epoch: 4 },
    );
    let mut current = owned(&workspace.id, "d-home", 4, "lease-fixture", 1);
    current["server_now"] = json!("2026-09-28T00:01:40Z");
    current["checkpoint"] = checkpoint(4);
    *lock(&account.baton) = current.clone();
    *lock(&account.grant) = Some((409, json!({"error":"takeover_grace","baton":current})));
    reconcile(&state, &config, &workspace.id).await.unwrap();
    assert!(crate::pro::may_execute(&state, &workspace.id));
    assert!(lock(&state.pro.status)
        .get(&workspace.id)
        .is_none_or(|status| status.error.is_none()));
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_device_takes_back_a_lapsed_cloud_lease_but_waits_to_move_live_cloud_work() {
    let root = temp("lapsed-worker");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let mut config = device(&account.endpoint);
    config.keeper_url = account.endpoint.clone();
    let workspace = project(&state, &root, &config, 4);
    lock(&state.pro.ownership).insert(
        workspace.id.clone(),
        Ownership::Remote {
            epoch: 5,
            holder: "worker-a".into(),
        },
    );
    let mut cloud = owned(&workspace.id, "worker-a", 5, "lease-cloud", 3);
    cloud["checkpoint"] = checkpoint(5);
    // Live cloud work and a computer that just woke on battery: no move.
    *lock(&account.baton) = cloud.clone();
    lazy_handback(&state, &config).await.unwrap();
    assert!(account.calls("GET", "/v1/hosts").is_empty());
    assert!(account.calls("POST", "/v2/mirror/credentials").is_empty());
    // The cloud's lease lapsed: nothing runs there, so the project returns
    // without waiting for power or a keeper route.
    cloud["server_now"] = json!("2026-09-28T00:05:00Z");
    *lock(&account.baton) = cloud;
    lazy_handback(&state, &config).await.unwrap();
    assert!(
        !account.calls("POST", "/v2/mirror/credentials").is_empty(),
        "the return started from the last acknowledged checkpoint"
    );
    // The fixture cannot serve Git, so this attempt fails and backs off.
    assert!(lock(&state.pro.return_backoff).contains_key(&workspace.id));
    let attempts = account.calls("POST", "/v2/mirror/credentials").len();
    lazy_handback(&state, &config).await.unwrap();
    assert_eq!(
        account.calls("POST", "/v2/mirror/credentials").len(),
        attempts,
        "a failed return is retried with backoff, not every pass"
    );
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

async fn post(state: &Arc<AppState>, path: &str, body: &str) -> (StatusCode, serde_json::Value) {
    use tower::ServiceExt;
    let response = crate::app(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(path)
                .header("Authorization", "Bearer fixture")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

#[tokio::test]
async fn a_sleep_flush_preempts_the_periodic_pass_and_answers_within_its_deadline() {
    let root = temp("sleep");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let config = device(&account.endpoint);
    let workspace = project(&state, &root, &config, 4);
    *lock(&account.baton) = owned(&workspace.id, "d-home", 4, "lease-fixture", 1);
    *lock(&state.pro.runtime) = Some(config);
    // A periodic pass is holding the job reservation (a long push).
    let jobs = state.pro.jobs.clone();
    let pass = tokio::spawn(async move {
        let _guard = jobs.lock_owned().await;
        tokio::time::sleep(StdDuration::from_secs(3600)).await;
    });
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    *lock(&state.pro.mirror_task) = Some(pass);
    // Publishing is slower than the caller's deadline.
    lock(&account.delays).insert(
        format!("/v1/baton/{}/policy", workspace.id),
        StdDuration::from_secs(12),
    );
    let started = std::time::Instant::now();
    let (status, reply) = post(&state, "/api/v1/pro/sleep", r#"{"deadline_ms":9000}"#).await;
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::OK);
    assert!(
        elapsed < StdDuration::from_millis(9000),
        "answered within the caller's deadline: {elapsed:?}"
    );
    assert_eq!(reply["handoff"], false);
    assert_eq!(reply["reason"], "deadline");
    assert_eq!(reply["pending"], json!([workspace.id]));
    // The computer woke before the flush finished: nothing is released and
    // the flush still ends on its own, returning the project here.
    assert_eq!(
        post(&state, "/api/v1/pro/wake", "").await.0,
        StatusCode::NO_CONTENT
    );
    tokio::time::timeout(StdDuration::from_secs(60), async {
        while !lock(&state.pro.sleeping).is_empty()
            || matches!(
                lock(&state.pro.ownership).get(&workspace.id),
                Some(Ownership::Transferring { .. })
            )
        {
            tokio::time::sleep(StdDuration::from_millis(50)).await;
        }
    })
    .await
    .expect("the flush finishes after its caller's deadline");
    assert!(crate::pro::may_write(&state, &workspace.id));
    assert!(account
        .calls("POST", &format!("/v2/baton/{}/release", workspace.id))
        .is_empty());
    assert_eq!(
        leftovers(&state.pro.root.join(&workspace.id)),
        Vec::<String>::new()
    );
    // The old request shape (no body) still works.
    assert_eq!(
        post(&state, "/api/v1/pro/sleep", "").await.0,
        StatusCode::OK
    );
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

async fn delete(state: &Arc<AppState>, path: &str) -> StatusCode {
    use tower::ServiceExt;
    crate::app(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("DELETE")
                .uri(path)
                .header("Authorization", "Bearer fixture")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_drain_waits_for_running_work_then_refuses_new_transfers_until_released() {
    let root = temp("drain");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let mut config = device(&account.endpoint);
    config.role = Role::Worker;
    config.execution.as_mut().unwrap().installation_id = None;
    *lock(&state.pro.runtime) = Some(config);
    // A transfer still holds the job reservation: the deadline passes first,
    // and the drain releases itself rather than wedging the machine.
    let jobs = state.pro.jobs.clone();
    let running = tokio::spawn(async move {
        let _guard = jobs.lock_owned().await;
        tokio::time::sleep(StdDuration::from_millis(1500)).await;
    });
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    let (status, reply) = post(&state, "/api/v1/pro/drain", r#"{"deadline_ms":1000}"#).await;
    assert_eq!(
        (status, reply["error"].clone()),
        (StatusCode::CONFLICT, json!("transfer_busy"))
    );
    assert!(!super::super::drain::draining(&state));
    // With a longer deadline the drain waits for that work to finish.
    // (Git helper slots are process-wide, so other tests' helpers also count.)
    let (status, reply) = post(&state, "/api/v1/pro/drain", r#"{"deadline_ms":120000}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(reply["token"].is_string());
    assert!(running.is_finished());
    assert_eq!(
        super::super::project_operations(&state),
        0,
        "a completed drain does not count its own reservation"
    );
    // New transfers are refused while drained; nothing waits on them.
    for (path, body) in [
        (
            "/api/v1/pro/handoff",
            r#"{"workspace_id":"w-a","expected_epoch":3}"#,
        ),
        (
            "/api/v1/pro/hydrate",
            r#"{"workspace_id":"w-a","expected_epoch":3}"#,
        ),
    ] {
        let (status, reply) = post(&state, path, body).await;
        assert_eq!(
            (status, reply["error"].clone()),
            (StatusCode::CONFLICT, json!("draining")),
            "{path}"
        );
    }
    assert_eq!(
        delete(&state, "/api/v1/pro/drain").await,
        StatusCode::NO_CONTENT
    );
    assert!(!super::super::drain::draining(&state));
    assert!(
        state.pro.jobs.try_lock().is_ok(),
        "releasing the drain frees the reservation"
    );
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}
