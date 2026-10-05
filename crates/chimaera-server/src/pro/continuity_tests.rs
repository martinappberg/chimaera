//! Original free/admission fixtures and shared loopback account peer.
//! Paid continuity journeys live in the actual private runtime suite.
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

pub(in crate::pro) struct FakeAccount {
    pub endpoint: String,
    pub requests: Arc<Mutex<Vec<(String, String, serde_json::Value)>>>,
    /// Scripted replies by (method, path), consulted before the defaults
    /// (placement reads, keeper host lists and relayed worker requests).
    pub canned: Arc<Mutex<Canned>>,
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
            canned,
            server,
        }
    }
    pub fn script(&self, method: &str, path: &str, status: u16, body: serde_json::Value) {
        lock(&self.canned).insert((method.into(), path.into()), (status, body));
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

#[tokio::test]
async fn hidden_login_terminal_workspace_is_never_a_mirror_candidate() {
    let root = temp("hidden-login-workspace");
    let state = state(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let visible = lock(&state.workspaces).add(project.clone()).unwrap();
    assert!(eligible(&state, &visible));
    let hidden = lock(&state.workspaces).add_hidden(project).unwrap();
    assert!(!eligible(&state, &hidden));
    std::fs::remove_dir_all(root).unwrap();
}

pub(in crate::pro) fn device(endpoint: &str) -> Configure {
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

fn shell_entry(id: &str, workspace: &crate::workspaces::Workspace) -> crate::ledger::LedgerEntry {
    crate::ledger::LedgerEntry {
        id: id.into(),
        suspended: false,
        manual_resume_reason: None,
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
        ("return_window_ended", "return_window_ended"),
    ] {
        assert_eq!(error_code(&anyhow::anyhow!(text)), code, "{text}");
    }
}

/// Once a plan has ended and the time to bring cloud work home has passed, the
/// account answers 403 `return_window_ended`. It reads as itself (the page says
/// so plainly); any other 403 stays a plain response for its caller to judge.
#[tokio::test]
async fn an_ended_return_window_reads_as_itself_not_a_bare_refusal() {
    let fake = FakeAccount::start(json!({})).await;
    let config = device(&fake.endpoint);
    fake.script(
        "GET",
        "/v2/baton/w-ended",
        403,
        json!({"error":"return_window_ended"}),
    );
    fake.script(
        "GET",
        "/v2/baton/w-other",
        403,
        json!({"error":"mirror_disabled"}),
    );
    let error = account(&config, "/v2/baton/w-ended", "GET", None)
        .await
        .err()
        .unwrap();
    assert_eq!(
        super::super::routes::error_code(&error),
        "return_window_ended"
    );
    assert_eq!(
        super::super::projects::open_error_code(&error),
        "return_window_ended"
    );
    let other = account(&config, "/v2/baton/w-other", "GET", None)
        .await
        .unwrap();
    assert_eq!(other.status, 403);
}

/// The request that wakes a suspended cloud machine can arrive before the
/// watchdog's first tick after the thaw. It is admitted while the machine
/// renews its own paused lease, not refused as an unauthorized connection; a
/// lease that lapsed while the watchdog kept ticking is still refused.
#[tokio::test]
async fn the_request_that_wakes_a_cloud_machine_is_admitted_before_its_watchdog_ticks() {
    let root = temp("thaw-admit");
    let state = state(&root);
    let account = FakeAccount::start(json!({})).await;
    let mut config = device(&account.endpoint);
    config.role = Role::Worker;
    config.execution.as_mut().unwrap().installation_id = None;
    let workspace = project(&state, &root, &config, 4);
    *lock(&state.pro.runtime) = Some(config);
    execution::thawed_fixture(&state, &workspace.id);
    execution::ticked(&state, std::time::Instant::now());
    assert!(
        !crate::pro::may_execute(&state, &workspace.id),
        "a lapse the watchdog saw happen is not a freeze"
    );
    execution::thawed_fixture(&state, &workspace.id);
    assert!(crate::pro::may_execute(&state, &workspace.id));
    assert!(execution::resuming(&state, &workspace.id));
    // Noticing the thaw wakes the lease loop then and there (the watchdog
    // does the same on its own tick): the renewal a viewer is waiting on
    // starts at once, not at the loop's next five-second tick.
    tokio::time::timeout(StdDuration::from_millis(50), state.pro.renew_now.notified())
        .await
        .expect("the thaw woke the lease loop");
    let generation = state.pro.generation.load(Ordering::Acquire);
    assert!(
        execution::expire(&state, generation).is_empty(),
        "nothing is fenced while the renewal is out"
    );
    drop(account);
    drop(state);
    let _ = std::fs::remove_dir_all(root);
}

/// The short guard before live cloud work moves home to an open app is fixed
/// in release builds; a development build may change it within five minutes.
#[test]
fn the_settle_guard_is_short_and_fixed_in_release_builds() {
    assert_eq!(super::settle_override(false, Some("5")), 20);
    assert_eq!(super::settle_override(true, None), 20);
    assert_eq!(super::settle_override(true, Some("5")), 5);
    assert_eq!(super::settle_override(true, Some("120")), 120);
    assert_eq!(super::settle_override(true, Some("9000")), 300);
    assert_eq!(super::settle_override(true, Some("soon")), 20);
}
