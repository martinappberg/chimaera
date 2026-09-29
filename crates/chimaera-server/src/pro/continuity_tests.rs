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

type Canned = HashMap<(String, String), (u16, serde_json::Value)>;

pub(super) struct FakeAccount {
    pub endpoint: String,
    pub requests: Arc<Mutex<Vec<(String, String, serde_json::Value)>>>,
    pub baton: Arc<Mutex<serde_json::Value>>,
    /// The reply to acquire/renew: a status and body (a grant or a refusal).
    pub grant: Arc<Mutex<Option<(u16, serde_json::Value)>>>,
    pub delays: Arc<Mutex<HashMap<String, StdDuration>>>,
    /// Scripted replies by (method, path), consulted before the defaults
    /// (placement reads, keeper host lists and relayed worker requests).
    pub canned: Arc<Mutex<Canned>>,
    /// Paths of requests that carried `X-Chimaera-Wake: interaction`.
    pub wakes: Arc<Mutex<Vec<String>>>,
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
        let canned = Arc::new(Mutex::new(HashMap::new()));
        let wakes = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (recorded, current, granted, delayed, scripted, woken) = (
            requests.clone(),
            baton.clone(),
            grant.clone(),
            delays.clone(),
            canned.clone(),
            wakes.clone(),
        );
        let router = Router::new().fallback(any(
            move |method: Method,
                  uri: axum::http::Uri,
                  headers: axum::http::HeaderMap,
                  body: Bytes| {
                let (recorded, current, granted, delayed, scripted, woken) = (
                    recorded.clone(),
                    current.clone(),
                    granted.clone(),
                    delayed.clone(),
                    scripted.clone(),
                    woken.clone(),
                );
                async move {
                    let path = uri.path().to_owned();
                    let body: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    lock(&recorded).push((method.to_string(), path.clone(), body.clone()));
                    if headers
                        .get("x-chimaera-wake")
                        .is_some_and(|value| value == "interaction")
                    {
                        lock(&woken).push(path.clone());
                    }
                    let delay = lock(&delayed).get(&path).copied();
                    if let Some(delay) = delay {
                        tokio::time::sleep(delay).await;
                    }
                    let reply = lock(&scripted)
                        .get(&(method.to_string(), path.clone()))
                        .cloned();
                    if let Some((status, body)) = reply {
                        return (
                            StatusCode::from_u16(status).unwrap_or(StatusCode::CONFLICT),
                            Json(body),
                        )
                            .into_response();
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
            canned,
            wakes,
            server,
        }
    }
    pub fn script(&self, method: &str, path: &str, status: u16, body: serde_json::Value) {
        lock(&self.canned).insert((method.into(), path.into()), (status, body));
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

/// A cloud machine that resumes after its lease deadline renews its own epoch
/// first (the account kept it as a paused owner): no fence while the renewal is
/// out, no checkpoint install over its own newer work, and a fence only when
/// the account refuses.
#[tokio::test]
async fn a_resumed_cloud_machine_renews_its_own_epoch_before_any_fence() {
    for refused in [false, true] {
        let root = temp(if refused { "resume-refused" } else { "resume" });
        let state = state(&root);
        let account = FakeAccount::start(json!({})).await;
        let mut config = device(&account.endpoint);
        config.role = Role::Worker;
        config.execution.as_mut().unwrap().installation_id = None;
        let workspace = project(&state, &root, &config, 4);
        *lock(&state.pro.runtime) = Some(config.clone());
        let mut paused = owned(&workspace.id, "d-home", 4, "lease-fixture", 1);
        paused["server_now"] = json!("2026-09-28T00:30:00Z");
        paused["checkpoint"] = checkpoint(4);
        *lock(&account.baton) = paused;
        *lock(&account.grant) = Some(if refused {
            (409, json!({"error":"stale_epoch"}))
        } else {
            let mut grant = owned(&workspace.id, "d-home", 4, "lease-fixture", 2);
            grant["checkpoint"] = checkpoint(4);
            (200, grant)
        });
        assert!(
            execution::resumed_fixture(&state, &workspace.id).is_empty(),
            "nothing is fenced before the renewal answers"
        );
        assert!(
            crate::pro::may_execute(&state, &workspace.id),
            "the input that woke the machine is admitted"
        );
        let result = reconcile(&state, &config, &workspace.id).await;
        assert!(account
            .calls("POST", &format!("/v2/baton/{}/acquire", workspace.id))
            .is_empty());
        assert_eq!(
            account
                .calls("POST", &format!("/v2/baton/{}/renew", workspace.id))
                .len(),
            1
        );
        assert!(
            account.calls("POST", "/v2/mirror/credentials").is_empty(),
            "no checkpoint is installed over the machine's own work"
        );
        let generation = state.pro.generation.load(Ordering::Acquire);
        if refused {
            assert!(result.is_err());
            assert_eq!(
                execution::expire(&state, generation),
                vec![workspace.id.clone()]
            );
            assert!(!crate::pro::may_execute(&state, &workspace.id));
        } else {
            result.unwrap();
            assert!(execution::expire(&state, generation).is_empty());
            assert!(execution::lease_valid(&state, &workspace.id));
            assert!(matches!(
                lock(&state.pro.ownership).get(&workspace.id),
                Some(Ownership::Local { epoch: 4 })
            ));
        }
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

/// A cloud machine asleep with ownership reads as an expired lease, but the
/// account refuses anyone else's acquire (409 `held`). The computer must wake
/// it and ask for the work back (once it is settled), never try to take it.
#[tokio::test]
async fn a_suspended_cloud_owner_is_woken_and_asked_never_taken_over() {
    let root = temp("suspended-worker");
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
    cloud["server_now"] = json!("2026-09-28T00:30:00Z");
    *lock(&account.baton) = cloud;
    account.script(
        "GET",
        &format!("/v2/workspaces/{}/placement", workspace.id),
        200,
        json!({"workspace_id":workspace.id,"holder_id":"worker-a","route_host_id":"worker-worker-a",
               "epoch":5,"policy_revision":1,"availability":"suspended","preferred_installation_id":"i-home",
               "checkpoint_id":"cp-fixture","server_now":"2026-09-28T00:30:00Z","expires_at":"2026-09-28T00:01:30Z"}),
    );
    account.script(
        "GET",
        "/v1/hosts",
        200,
        json!([{"id":"worker-worker-a","kind":"worker","status":"connected","alias":"Cloud"}]),
    );
    let handoff = "/v1/hosts/worker-worker-a/http/api/v1/pro/handoff";
    // The woken machine is still mid-turn this time.
    account.script("POST", handoff, 409, json!({"error":"workspace_busy"}));
    // Just woke on battery: nothing is woken, nothing is taken.
    lazy_handback(&state, &config).await.unwrap();
    assert!(account.calls("POST", handoff).is_empty());
    assert!(account.calls("POST", "/v2/mirror/credentials").is_empty());
    assert!(account
        .calls("POST", &format!("/v2/baton/{}/acquire", workspace.id))
        .is_empty());
    // Settled on power: wake the machine and ask it for the work.
    state.pro.power_suitable.store(true, Ordering::Release);
    state.pro.awake_since.store(0, Ordering::Release);
    lazy_handback(&state, &config).await.unwrap();
    assert_eq!(
        account.calls("POST", handoff),
        vec![json!({"workspace_id": workspace.id, "expected_epoch": 5})]
    );
    assert_eq!(*lock(&account.wakes), vec![handoff.to_string()]);
    assert!(
        account.calls("POST", "/v2/mirror/credentials").is_empty(),
        "a held project is never fetched or acquired from under its owner"
    );
    assert!(lock(&state.pro.return_backoff).get(&workspace.id).is_none());
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

/// While the account is down the keeper answers 503 `account_unavailable`:
/// a return in progress waits quietly (no error for the user, no backoff).
#[tokio::test]
async fn an_account_outage_behind_the_keeper_is_a_quiet_wait() {
    let root = temp("keeper-outage");
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
    *lock(&account.baton) = owned(&workspace.id, "worker-a", 5, "lease-cloud", 3);
    state.pro.power_suitable.store(true, Ordering::Release);
    state.pro.awake_since.store(0, Ordering::Release);
    let outage = json!({"error":"account_unavailable"});
    let handoff = "/v1/hosts/worker-worker-a/http/api/v1/pro/handoff";
    account.script("GET", "/v1/hosts", 503, outage.clone());
    lazy_handback(&state, &config).await.unwrap();
    account.script(
        "GET",
        "/v1/hosts",
        200,
        json!([{"id":"worker-worker-a","kind":"worker","status":"connected","alias":"Cloud"}]),
    );
    account.script("POST", handoff, 503, outage);
    lazy_handback(&state, &config).await.unwrap();
    assert_eq!(account.calls("POST", handoff).len(), 1);
    assert!(lock(&state.pro.return_backoff).get(&workspace.id).is_none());
    assert!(lock(&state.pro.status)
        .get(&workspace.id)
        .is_none_or(|status| status.error.is_none()));
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

async fn status(state: &Arc<AppState>) -> serde_json::Value {
    use tower::ServiceExt;
    let response = crate::app(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/v1/pro/status")
                .header("Authorization", "Bearer fixture")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// A delegation the account refuses, or one that expired unrenewed, makes
/// `/pro/status` say the daemon is not configured, so the native app mints a
/// fresh one; a successful renewal clears it.
#[tokio::test]
async fn a_refused_or_lapsed_delegation_asks_for_setup_again() {
    let root = temp("delegation");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let mut config = device(&account.endpoint);
    *lock(&state.pro.runtime) = Some(config.clone());
    state.pro.configured.store(true, Ordering::Release);
    let generation = state.pro.generation.load(Ordering::Acquire);
    let reply = status(&state).await;
    assert_eq!(
        (reply["configured"].clone(), reply["renewal_failed"].clone()),
        (json!(true), json!(false))
    );
    account.script(
        "POST",
        "/v1/delegations/renew",
        401,
        json!({"error":"unauthorized"}),
    );
    assert!(!renew_delegation(&state, &config, generation).await);
    let reply = status(&state).await;
    assert_eq!(
        (reply["configured"].clone(), reply["renewal_failed"].clone()),
        (json!(false), json!(true))
    );
    account.script(
        "POST",
        "/v1/delegations/renew",
        200,
        json!({"access_token":"renewed","expires_at":"2099-02-01T00:00:00Z","scope":["baton","mirror"],"device_id":"d-home"}),
    );
    assert!(renew_delegation(&state, &config, generation).await);
    assert_eq!(status(&state).await["configured"], true);
    // Expired without a renewal: the same answer.
    config.delegation.expires_at = "2000-01-01T00:00:00Z".into();
    *lock(&state.pro.runtime) = Some(config);
    let reply = status(&state).await;
    assert_eq!(
        (reply["configured"].clone(), reply["renewal_failed"].clone()),
        (json!(false), json!(true))
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
    assert!(
        !lock(&state.pro.sleeping).is_empty() && crate::pro::may_write(&state, &workspace.id),
        "the project is this computer's again at once, while its flush still runs"
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

/// A sleep flush that could not hand the project over never renews or resumes
/// inside the sleep window (that would restart agents seconds before sleep and
/// keep the lease from lapsing); the wake returns the project and the sessions
/// the flush stopped to this computer at once, without the account.
#[tokio::test]
async fn an_unreleased_sleep_flush_waits_for_the_wake_and_resumes_locally() {
    let root = temp("sleep-pending");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let config = device(&account.endpoint);
    let workspace = project(&state, &root, &config, 4);
    *lock(&account.baton) = owned(&workspace.id, "d-home", 4, "lease-fixture", 1);
    *lock(&state.pro.runtime) = Some(config.clone());
    // The fixture has no Git service: publication fails inside the window.
    // (Git helper slots are process-wide, so under a parallel test run the
    // flush may outlive the reply; wait for it to end either way.)
    let (status, _) = post(&state, "/api/v1/pro/sleep", r#"{"deadline_ms":20000}"#).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::timeout(StdDuration::from_secs(120), async {
        while !lock(&state.pro.sleeping).is_empty() {
            tokio::time::sleep(StdDuration::from_millis(50)).await;
        }
    })
    .await
    .expect("the flush ends");
    assert!(lock(&state.pro.release_pending).contains(&workspace.id));
    let renewals = |account: &FakeAccount| {
        account
            .calls("POST", &format!("/v2/baton/{}/renew", workspace.id))
            .len()
            + account
                .calls("POST", &format!("/v2/baton/{}/acquire", workspace.id))
                .len()
    };
    assert_eq!(
        renewals(&account),
        0,
        "nothing renewed inside the sleep window"
    );
    assert!(matches!(
        lock(&state.pro.ownership).get(&workspace.id),
        Some(Ownership::Transferring { epoch: 4 })
    ));
    let reads = lock(&account.requests).len();
    reconcile(&state, &config, &workspace.id).await.unwrap();
    assert_eq!(
        lock(&account.requests).len(),
        reads,
        "the lease loop leaves a stopped-for-sleep project alone"
    );
    // A session the flush stopped (here a terminal, to need no agent CLI).
    let stopped = "s-stopped-for-sleep";
    crate::ledger::defer(
        &state,
        crate::ledger::LedgerEntry {
            id: stopped.into(),
            suspended: true,
            handoff: None,
            workspace_id: workspace.id.clone(),
            cwd: workspace.root.clone(),
            pinned_name: None,
            cols: 80,
            rows: 24,
            theme: "dark".into(),
            created_at: 0,
            agent: None,
        },
    )
    .unwrap();
    // The computer wakes with the account unreachable: the project and its
    // stopped session come back here anyway.
    lock(&account.delays).insert(
        format!("/v2/baton/{}", workspace.id),
        StdDuration::from_secs(60),
    );
    assert_eq!(
        post(&state, "/api/v1/pro/wake", "").await.0,
        StatusCode::NO_CONTENT
    );
    assert!(crate::pro::may_write(&state, &workspace.id));
    tokio::time::timeout(StdDuration::from_secs(10), async {
        while !state.sessions.get(stopped).is_some_and(|s| s.alive) {
            tokio::time::sleep(StdDuration::from_millis(50)).await;
        }
    })
    .await
    .expect("the stopped session resumed locally");
    assert_eq!(renewals(&account), 0);
    state.sessions.kill(stopped).ok();
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

fn shell_entry(id: &str, workspace: &crate::workspaces::Workspace) -> crate::ledger::LedgerEntry {
    crate::ledger::LedgerEntry {
        id: id.into(),
        suspended: false,
        handoff: None,
        workspace_id: workspace.id.clone(),
        cwd: workspace.root.clone(),
        pinned_name: None,
        cols: 80,
        rows: 24,
        theme: "dark".into(),
        created_at: 0,
        agent: None,
    }
}

/// A checkpoint install is fenced from the moment it is scheduled (before
/// hydrate's own fence), and the unverified boot fallback never resumes a
/// stale turn in a project the account already answered for.
#[tokio::test]
async fn a_scheduled_install_fences_at_once_and_the_boot_fallback_leaves_it_alone() {
    let root = temp("install-fence");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let config = device(&account.endpoint);
    let workspace = project(&state, &root, &config, 4);
    *lock(&state.pro.runtime) = Some(config.clone());
    // Restarted; meanwhile the cloud worked on and released a newer
    // checkpoint, which this computer must install.
    lock(&state.pro.ownership).insert(
        workspace.id.clone(),
        Ownership::AwaitingVerification { epoch: 4 },
    );
    let mut cloud = owned(&workspace.id, "worker-a", 5, "lease-cloud", 3);
    cloud["holder_id"] = json!(null);
    cloud["expires_at"] = json!(null);
    cloud["checkpoint"] = checkpoint(5);
    *lock(&account.baton) = cloud;
    lock(&account.delays).insert("/v2/mirror/credentials".into(), StdDuration::from_secs(3));
    assert!(crate::pro::may_write(&state, &workspace.id));
    reconcile(&state, &config, &workspace.id).await.unwrap();
    assert!(
        !crate::pro::may_write(&state, &workspace.id),
        "fenced from the moment the install is scheduled"
    );
    // A turn a previous daemon left behind waits for the verified path.
    let stale = "s-stale-boot-turn";
    let mut entry = shell_entry(stale, &workspace);
    entry.suspended = true;
    crate::ledger::defer(&state, entry).unwrap();
    crate::pro::defer_boot_session(&state, stale);
    crate::pro::resume_unverified(&state).await;
    assert!(
        state.sessions.get(stale).is_none(),
        "the fallback leaves an answered project alone"
    );
    // The install fails here (no Git service) and lifts its early fence.
    tokio::time::timeout(StdDuration::from_secs(60), async {
        while lock(&state.pro.installing).contains(&workspace.id) {
            tokio::time::sleep(StdDuration::from_millis(50)).await;
        }
    })
    .await
    .expect("the install ends and lifts its early fence");
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

/// A plain shell is never managed: it comes back at boot even while the
/// project's agents wait for this life's ownership proof.
#[tokio::test]
async fn plain_shells_come_back_at_boot_while_agents_wait() {
    let root = temp("boot-shell");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let config = device(&account.endpoint);
    let workspace = project(&state, &root, &config, 4);
    lock(&state.pro.ownership).insert(
        workspace.id.clone(),
        Ownership::AwaitingVerification { epoch: 4 },
    );
    assert!(!crate::pro::may_restore(&state, &workspace.id));
    let shell = "s-boot-shell";
    let agent = "s-boot-agent";
    let mut waiting = shell_entry(agent, &workspace);
    waiting.agent = Some(crate::ledger::LedgerAgent {
        kind: crate::agents::AgentKind::Claude,
        resume: None,
        transcript: None,
        native_cwd: None,
        title: "claude".into(),
        ui: chimaera_agent::model::SessionUi::Term,
        model: None,
        carryover: None,
    });
    crate::ledger::restore(
        &state,
        crate::ledger::BootLedger {
            sessions: vec![shell_entry(shell, &workspace), waiting],
            ..Default::default()
        },
    )
    .await;
    assert!(state.sessions.get(shell).is_some_and(|s| s.alive));
    assert!(lock(&state.deferred_sessions).contains_key(agent));
    assert!(!lock(&state.deferred_sessions).contains_key(shell));
    state.sessions.kill(shell).ok();
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

/// One conversation that cannot be saved yet (a fresh terminal agent with no
/// transcript) never fails the project's copy: the files still go.
#[tokio::test]
async fn an_unsaveable_conversation_never_fails_the_project_copy() {
    let root = temp("unsaveable");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let config = device(&account.endpoint);
    let workspace = project(&state, &root, &config, 4);
    *lock(&account.baton) = owned(&workspace.id, "d-home", 4, "lease-fixture", 1);
    let agent = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: workspace.root.clone(),
            name: None,
            cols: 80,
            rows: 24,
            command: Some(vec!["/bin/sleep".into(), "30".into()]),
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
    lock(&state.session_workspaces).insert(agent.id.clone(), workspace.id.clone());
    // The fixture has no Git service: the copy fails only when it publishes.
    let error = snapshot(&state, &config, &workspace.id, false)
        .await
        .unwrap_err();
    assert_ne!(
        super::super::routes::error_code(&error),
        "conversation_not_saved",
        "{error:#}"
    );
    assert!(state.sessions.get(&agent.id).is_some_and(|s| s.alive));
    state.sessions.kill(&agent.id).ok();
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

/// Signing out while this computer's own return is unfinished must not leave
/// the project fenced: no account is left to finish it.
#[tokio::test]
async fn sign_out_never_keeps_this_computers_own_return_fenced() {
    let root = temp("signout-return");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let config = device(&account.endpoint);
    let workspace = project(&state, &root, &config, 4);
    *lock(&state.pro.runtime) = Some(config);
    lock(&state.pro.ownership).insert(workspace.id.clone(), Ownership::Hydrating { epoch: 5 });
    assert!(!crate::pro::may_write(&state, &workspace.id));
    assert_eq!(
        delete(&state, "/api/v1/pro/configure").await,
        StatusCode::NO_CONTENT
    );
    assert!(crate::pro::may_write(&state, &workspace.id));
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
    // A transfer still holds the job reservation (held until released, not
    // for a guessed time): the deadline passes first, and the drain releases
    // itself rather than wedging the machine.
    let jobs = state.pro.jobs.clone();
    let (held, holding) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(async move {
        let _guard = jobs.lock_owned().await;
        let _ = held.send(());
        let _ = released.await;
    });
    holding.await.unwrap();
    let (status, reply) = post(&state, "/api/v1/pro/drain", r#"{"deadline_ms":1000}"#).await;
    assert_eq!(
        (status, reply["error"].clone()),
        (StatusCode::CONFLICT, json!("transfer_busy"))
    );
    assert!(!super::super::drain::draining(&state));
    // Two drain requests at once (a retried supervisor call) share one drain
    // and one token once that work finishes.
    let (first, second) = {
        let (a, b) = (state.clone(), state.clone());
        (
            tokio::spawn(async move {
                post(&a, "/api/v1/pro/drain", r#"{"deadline_ms":120000}"#).await
            }),
            tokio::spawn(async move {
                post(&b, "/api/v1/pro/drain", r#"{"deadline_ms":120000}"#).await
            }),
        )
    };
    tokio::task::yield_now().await;
    release.send(()).unwrap();
    let (first, second) = (first.await.unwrap(), second.await.unwrap());
    assert_eq!((first.0, second.0), (StatusCode::OK, StatusCode::OK));
    let token = first.1["token"].as_str().unwrap().to_owned();
    assert_eq!(second.1["token"], token.as_str());
    // The supervisor's contract: nonempty, at most 256 chars, no control chars.
    assert!(!token.is_empty() && token.len() <= 256 && !token.chars().any(char::is_control));
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

    // A transfer admitted just before a drain, still waiting for the job
    // reservation, refuses itself once the drain takes it (it would otherwise
    // hold the drain open until its deadline).
    // (This test's runtime is single-threaded, so the order below is exact:
    // a spawned task runs until it waits whenever this one yields.)
    let holder = state.pro.jobs.clone().lock_owned().await;
    let draining = {
        let state = state.clone();
        tokio::spawn(
            async move { post(&state, "/api/v1/pro/drain", r#"{"deadline_ms":5000}"#).await },
        )
    };
    // Once the drain holds its gate it is queued for the reservation.
    while state.pro.drain_gate.try_lock().is_ok() {
        tokio::task::yield_now().await;
    }
    let waiting = {
        let state = state.clone();
        tokio::spawn(async move { super::super::drain::reserve(&state).await.is_some() })
    };
    tokio::task::yield_now().await;
    drop(holder);
    let admitted = tokio::time::timeout(StdDuration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    assert!(!admitted, "the waiting transfer refused itself");
    let (status, _) = draining.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        delete(&state, "/api/v1/pro/drain").await,
        StatusCode::NO_CONTENT
    );
    // A drain request whose caller gives up does not leave the daemon
    // draining: only a completed drain stays in place.
    let cache = state.pro.cache("w-busy").unwrap();
    let abandoned = {
        let state = state.clone();
        tokio::spawn(
            async move { post(&state, "/api/v1/pro/drain", r#"{"deadline_ms":60000}"#).await },
        )
    };
    tokio::time::timeout(StdDuration::from_secs(5), async {
        while !super::super::drain::draining(&state) {
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    abandoned.abort();
    let _ = abandoned.await;
    assert!(!super::super::drain::draining(&state));
    drop(cache);
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn transfer_failures_carry_stable_codes_not_just_words() {
    use super::super::routes::error_code;
    for (text, code) in [
        ("Account changed during project transfer", "account_changed"),
        (
            "execution authority expired before publication",
            "ownership_unverified",
        ),
        (
            "Project ownership changed during return",
            "ownership_changed",
        ),
        (
            "a durable project checkpoint is not available yet",
            "checkpoint_pending",
        ),
        (
            "previous managed processes are still stopping",
            "previous_processes_running",
        ),
        ("mirror Git operation failed", "git"),
    ] {
        assert_eq!(error_code(&anyhow::anyhow!(text)), code, "{text}");
    }
}
