//! The workspace-admission seam is inert on a daemon without an extension:
//! no Pro state is read or written, no Pro route is served, nothing Pro adds
//! appears on `/health`, `/workspaces` or the session rows, and only the
//! durable fence an earlier composed daemon left is honoured.
use super::support::*;
use crate::*;

/// A free daemon over `data`, exactly as `lifecycle` installs it.
fn free_state(data: PathBuf) -> Arc<AppState> {
    let state = test_state_with_data_dir(0, data.clone());
    state.use_inert_policy(crate::policy::fence::Fence::load(&data));
    state
}

fn untouched(state: &AppState, data: &std::path::Path) {
    assert!(!state.pro.initialized(), "Pro state was read");
    assert!(
        !state.bundle_imports.initialized(),
        "import record was read"
    );
    assert!(!data.join("pro").exists(), "Pro state was written");
    assert!(!data.join("bundles").exists(), "import record was written");
    assert!(!state.session_proxy.polling(), "a placement poll started");
}

#[tokio::test]
async fn a_free_daemon_serves_the_free_product_and_nothing_else() {
    let data = test_dir("free-seam");
    let state = free_state(data.clone());

    let (status, health) = request(&state, Method::GET, "/api/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    let mut keys: Vec<_> = health.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["build", "hostname", "name", "pid", "uptime_secs", "version"],
        "{health}"
    );

    let root = std::fs::canonicalize(test_dir("free-seam-project")).unwrap();
    let (status, workspace) = request(
        &state,
        Method::POST,
        "/api/v1/workspaces",
        Some(serde_json::json!({"root": root.to_string_lossy()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{workspace}");
    let id = workspace["id"].as_str().unwrap().to_owned();
    assert!(!root.join(".chimaera-workspace").exists());
    let (status, _) = request(
        &state,
        Method::POST,
        &format!("/api/v1/workspaces/{id}/open"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, listed) = request(&state, Method::GET, "/api/v1/workspaces", None).await;
    let listed = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == id.as_str())
        .unwrap()
        .clone();
    assert!(listed.get("local_copy").is_none(), "{listed}");

    let (status, session) = request(
        &state,
        Method::POST,
        "/api/v1/sessions",
        Some(serde_json::json!({"workspace_id": id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let sid = session["id"].as_str().unwrap().to_owned();
    let (_, rows) = request(&state, Method::GET, "/api/v1/sessions", None).await;
    for row in rows.as_array().unwrap() {
        for pro_field in ["last_input_ms", "placement", "pause", "blocked_provider"] {
            assert!(row.get(pro_field).is_none(), "{pro_field} on {row}");
        }
    }
    request(
        &state,
        Method::DELETE,
        &format!("/api/v1/sessions/{sid}"),
        None,
    )
    .await;

    for route in [
        "/api/v1/pro/status",
        "/api/v1/pro/projects",
        "/api/v1/pro/cloud",
    ] {
        let (status, _) = request(&state, Method::GET, route, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}");
    }
    let (status, _) = request(
        &state,
        Method::POST,
        "/api/v1/pro/configure",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    untouched(&state, &data);
    std::fs::remove_dir_all(root).ok();
    std::fs::remove_dir_all(data).ok();
}

#[tokio::test]
async fn a_free_daemon_admits_and_dispatches_without_pro_state() {
    let data = test_dir("free-seam-admission");
    let state = free_state(data.clone());
    let policy = state.policy();
    for need in [
        crate::policy::Need::Execute,
        crate::policy::Need::Shell,
        crate::policy::Need::Restore,
    ] {
        assert!(policy.allows(&state, "w-free", need));
    }
    assert!(policy
        .reserve(&state, "w-free", crate::policy::LaunchKind::Agent)
        .unwrap()
        .is_none());
    let (launch, reservation) = policy
        .admit_launch(&state, "w-free", crate::policy::LaunchKind::Agent)
        .await
        .unwrap();
    assert!(!launch.managed() && reservation.is_none());
    let admission = policy.capture(&state, "w-free").unwrap();
    assert!(admission.begin(&state).unwrap().is_none());
    let installer = admission.installer(&state, "w-free").await.unwrap();
    installer.finish().await.unwrap();
    assert!(policy
        .hold_session(&state, "w-free", "s-free", Some("native"), false)
        .is_ok());
    let mut env = vec![("KEEP".to_string(), "1".to_string())];
    let mut remove = vec!["DROP".to_string()];
    policy
        .launch_env(&state, "w-free", &mut env, &mut remove)
        .await
        .unwrap();
    assert_eq!(env.len(), 1);
    assert_eq!(remove.len(), 1);
    assert_eq!(
        policy.launch_context(&state, "w-free"),
        crate::policy::LaunchContext::default()
    );
    assert!(!policy.updates_managed(&state));
    assert!(!policy.active(&state));
    assert!(policy.session_pause(&state, "s-free", None).is_none());
    assert!(policy.owner(&state, "w-free").is_none());
    crate::activity::record(&state, "s-free");
    crate::activity::touch(&state);
    assert_eq!(crate::activity::last_change(&state), None);
    untouched(&state, &data);
    std::fs::remove_dir_all(data).ok();
}

fn write(data: &std::path::Path, path: &str, bytes: &[u8]) {
    let path = data.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[tokio::test]
async fn a_free_daemon_keeps_the_fence_an_earlier_composed_daemon_left() {
    let data = test_dir("free-seam-fence");
    write(
        &data,
        "pro/state.json",
        br#"{"ownership":{
            "w-cloud":{"state":"remote","epoch":3,"holder":"h"},
            "w-local":{"state":"local","epoch":2},
            "w-leaving":{"state":"transferring","epoch":4},
            "w-parked":{"state":"transferring","epoch":5}},
          "parked":["w-parked"],
          "legacy_pending":["w-pending"],
          "preferences":{"w-copy":{"copy":{"x":1}},"w-bound":{"account":"a"}}}"#,
    );
    write(
        &data,
        "pro/copy-authority.json",
        br#"{"version":1,"workspaces":["w-readonly"]}"#,
    );
    let fence = crate::policy::fence::Fence::load(&data);
    for fenced in ["w-cloud", "w-parked", "w-pending", "w-copy", "w-readonly"] {
        assert!(fence.fenced(fenced), "{fenced}");
    }
    for free in ["w-local", "w-leaving", "w-bound", "w-never"] {
        assert!(!fence.fenced(free), "{free}");
    }
    let state = free_state(data.clone());
    let policy = state.policy();
    assert!(!policy.allows(&state, "w-cloud", crate::policy::Need::Shell));
    assert!(policy.capture(&state, "w-cloud").is_err());
    assert!(policy
        .reserve(&state, "w-cloud", crate::policy::LaunchKind::Shell)
        .is_err());
    assert!(policy.allows(&state, "w-local", crate::policy::Need::Execute));
    // The fence was read, never Pro's live state.
    assert!(!state.pro.initialized());
    std::fs::remove_dir_all(data).ok();
}

#[test]
fn an_unreadable_fence_record_blocks_nothing() {
    let damaged = test_dir("free-seam-damaged");
    write(&damaged, "pro/state.json", b"\x00 not json {");
    write(&damaged, "pro/copy-authority.json", b"[");
    assert_eq!(crate::policy::fence::Fence::load(&damaged).len(), 0);
    // An I/O error (here: a directory where the file belongs) is not a fence.
    let unreadable = test_dir("free-seam-unreadable");
    std::fs::create_dir_all(unreadable.join("pro/state.json")).unwrap();
    assert_eq!(crate::policy::fence::Fence::load(&unreadable).len(), 0);
    let oversized = test_dir("free-seam-oversized");
    write(&oversized, "pro/state.json", &vec![b' '; 1024 * 1024 + 1]);
    assert_eq!(crate::policy::fence::Fence::load(&oversized).len(), 0);
    for dir in [damaged, unreadable, oversized] {
        std::fs::remove_dir_all(dir).ok();
    }
}
