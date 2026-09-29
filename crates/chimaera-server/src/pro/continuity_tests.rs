//! Loopback account fixtures for the ownership, flush and return paths. The
//! fake records every request; Git endpoints are deliberately absent, so a
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
        let (recorded, current, granted, delayed, origin) = (
            requests.clone(),
            baton.clone(),
            grant.clone(),
            delays.clone(),
            endpoint.clone(),
        );
        let router = Router::new().fallback(any(
            move |method: Method, uri: axum::http::Uri, body: Bytes| {
                let (recorded, current, granted, delayed, origin) = (
                    recorded.clone(),
                    current.clone(),
                    granted.clone(),
                    delayed.clone(),
                    origin.clone(),
                );
                async move {
                    let path = uri.path().to_owned();
                    let body: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    lock(&recorded).push((method.to_string(), path.clone(), body.clone()));
                    let delay = lock(&delayed).get(&path).copied();
                    if let Some(delay) = delay {
                        tokio::time::sleep(delay).await;
                    }
                    respond(&method, &path, &body, &current, &granted, &origin)
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
    grant: &Mutex<Option<serde_json::Value>>,
    origin: &str,
) -> Response {
    let segments: Vec<_> = path.trim_start_matches('/').split('/').collect();
    match (method.as_str(), segments.as_slice()) {
        ("GET", [_, "baton", _]) => Json(lock(baton).clone()).into_response(),
        ("POST", [_, "baton", _, "acquire" | "renew"]) => match lock(grant).clone() {
            Some(grant) => {
                *lock(baton) = grant.clone();
                Json(grant).into_response()
            }
            None => StatusCode::CONFLICT.into_response(),
        },
        ("PUT", ["v1", "baton", _, "policy"]) => StatusCode::NO_CONTENT.into_response(),
        ("POST", [_, "mirror", "credentials"]) => Json(json!({
            "workspace_id": body["workspace_id"],
            "repository_url": format!("{origin}/git/repository.git"),
            "working_tree_url": format!("{origin}/git/working-tree.git"),
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

/// Send a request and hang up before the reply, as a relay that timed out.
async fn abandon(address: std::net::SocketAddr, path: &str, body: serde_json::Value) {
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
    tokio::time::sleep(StdDuration::from_millis(300)).await;
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
    abandon(
        address,
        "/api/v1/pro/handoff",
        json!({"workspace_id": workspace.id, "expected_epoch": 4}),
    )
    .await;
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
