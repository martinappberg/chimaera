use super::*;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use std::{
    io::Write,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    sync::Arc,
};
use tower::ServiceExt;

fn private_pipe() -> (OwnedFd, OwnedFd) {
    let (reader, writer) = std::io::pipe().unwrap();
    (reader.into(), writer.into())
}

struct Fixture {
    root: PathBuf,
    project: PathBuf,
}

#[cfg(target_os = "linux")]
#[test]
fn ordinary_startup_keeps_cleanup_metadata_without_process_hardening_or_duplicate_stage() {
    let f = Fixture::new();
    let state = f.state();
    let mut absent = f.receipt(&state, 1, 0);
    absent.maintenance_control = Some(super::super::maintenance_startup::Control {
        version: 1,
        fd: i32::MAX,
        channel_nonce: "A".repeat(43),
    });
    assert!(own_startup(absent).is_err());
    let dumpable = unsafe { nix::libc::prctl(nix::libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
    let startup = own_startup(f.receipt(&state, 1, 0)).unwrap();
    assert!(startup.maintenance.is_none());
    assert!(startup.receipt.maintenance_control.is_none());
    stage_startup(&state, Some(startup)).unwrap();
    assert!(lock(&state.pro.execution.maintenance_pending).is_none());
    assert!(stage_startup(&state, Some(own_startup(f.receipt(&state, 2, 1)).unwrap())).is_err());
    assert_eq!(
        lock(&state.pro.execution.supervisor_pending)
            .as_ref()
            .unwrap()
            .launch_generation,
        1
    );
    assert_eq!(
        unsafe { nix::libc::prctl(nix::libc::PR_GET_DUMPABLE, 0, 0, 0, 0) },
        dumpable
    );
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "chimaera-cleanup-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(root.join("project")).unwrap();
        let root = root.canonicalize().unwrap();
        Self {
            project: root.join("project"),
            root,
        }
    }
    fn state(&self) -> Arc<AppState> {
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            self.root.clone(),
            self.root.join("config"),
        ));
        state.stopping.store(true, Ordering::Release);
        state
    }
    fn config(&self) -> Value {
        json!({"account_id":"a-fixture","role":"worker","endpoint":"http://127.0.0.1:9","keeper_url":"","workspace_root":self.project,"execution":{"version":1,"installation_id":null,"capability":wire::ExecutionCapability::managed()},"delegation":{"access_token":"synthetic","expires_at":"2099-01-01T00:00:00Z","scope":["baton","mirror"],"device_id":"worker-fixture","workspace":{"workspace_id":"w-a","revision":7}}})
    }
    fn receipt(&self, state: &AppState, generation: u64, previous: u64) -> CleanupReceipt {
        let meta = std::fs::metadata(&self.project).unwrap();
        decode(&serde_json::to_vec(&json!({"version":1,"workspace_id":"w-a","account_id":"a-fixture","root_identity":{"device":meta.dev(),"inode":meta.ino()},"registration_revision":7,"launch_generation":generation,"previous_generation":previous,"os_boot_id":state.pro.execution.boot.clone().unwrap()})).unwrap()).unwrap()
    }
    async fn previously_bound(&self) -> Arc<AppState> {
        let state = self.state();
        crate::pro::ensure_root(&state.pro.root).await.unwrap();
        let config: Configure = serde_json::from_value(self.config()).unwrap();
        let accepted = crate::pro::authority::prepare(&state, &config, self.project.clone())
            .await
            .unwrap();
        crate::pro::authority::save(&state, &accepted)
            .await
            .unwrap();
        lock(&state.pro.preferences)
            .entry("w-a".into())
            .or_default()
            .supervisor_generation = Some(1);
        crate::pro::persist(&state).await.unwrap();
        drop(state);
        self.state()
    }
    fn advance(&self, state: &AppState) -> (Value, CleanupReceipt) {
        let mut config = self.config();
        config["delegation"]["workspace"]["revision"] = json!(8);
        let mut receipt = self.receipt(state, 2, 1);
        receipt.registration_revision = 8;
        (config, receipt)
    }
    async fn dirty(&self) -> Arc<AppState> {
        let state = self.state();
        crate::pro::install_execution_fixture(&state, "w-a", 2).unwrap();
        crate::pro::ensure_root(&state.pro.root).await.unwrap();
        prepare_launch(&state, "w-a").await.unwrap();
        drop(state);
        let restored = self.state();
        assert!(lock(&restored.pro.execution.unclean).contains_key("w-a"));
        restored
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn request(state: &Arc<AppState>, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        crate::app(state.clone()).oneshot(
            Request::builder()
                .method(if body.is_some() { "POST" } else { "GET" })
                .uri(path)
                .header("Authorization", "Bearer fixture")
                .header("Content-Type", "application/json")
                .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
#[tokio::test]
async fn exact_cleanup_is_persisted_before_ack_but_never_grants_execution() {
    let fixture = Fixture::new();
    let state = fixture.dirty().await;
    let receipt = fixture.receipt(&state, 2, 1);
    stage(&state, Some(receipt.clone()));
    assert!(!crate::pro::may_execute(&state, "w-a"));
    assert!(!crate::pro::may_execute(&state, "w-unmanaged"));
    assert!(!crate::pro::may_write(&state, "w-a"));
    assert!(request(&state, "/api/v1/health", None).await.1["supervisor_cleanup"].is_null());
    let (status, _) = request(
        &state,
        "/api/v1/pro/configure/execution",
        Some(fixture.config()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, health) = request(&state, "/api/v1/health", None).await;
    assert_eq!(
        health["supervisor_cleanup"],
        json!({"execution_cleanup":1,"workspace_id":"w-a","registration_revision":7,"launch_generation":2})
    );
    assert!(quiescent(&state, "w-a"));
    assert!(!crate::pro::may_execute(&state, "w-a"));
    assert!(lock(&state.pro.execution.proofs).is_empty());
    let saved: Value =
        serde_json::from_slice(&std::fs::read(state.pro.root.join("state.json")).unwrap()).unwrap();
    assert_eq!(saved["preferences"]["w-a"]["supervisor_generation"], 2);
    assert_eq!(saved["preferences"]["w-a"]["execution_active"], false);
    drop(state);
    let restarted = fixture.state();
    stage(&restarted, Some(receipt));
    assert_eq!(
        request(
            &restarted,
            "/api/v1/pro/configure/execution",
            Some(fixture.config())
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(ack(&restarted).is_none());
    assert!(!crate::pro::may_execute(&restarted, "w-a"));
    stage(&restarted, Some(fixture.receipt(&restarted, 3, 2)));
    assert_eq!(
        request(
            &restarted,
            "/api/v1/pro/configure/execution",
            Some(fixture.config())
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!crate::pro::may_execute(&restarted, "w-a"));
}
#[tokio::test]
async fn wrong_binding_boot_or_generation_preserves_crash_fence_and_pending_receipt() {
    let fixture = Fixture::new();
    let state = fixture.dirty().await;
    lock(&state.pro.preferences)
        .get_mut("w-a")
        .unwrap()
        .supervisor_generation = Some(2);
    for mismatch in 0..8 {
        let mut receipt = fixture.receipt(&state, 3, 2);
        match mismatch {
            0 => receipt.account_id = "a-other".into(),
            1 => receipt.workspace_id = "w-other".into(),
            2 => receipt.root_identity.device += 1,
            3 => receipt.root_identity.inode += 1,
            4 => receipt.registration_revision += 1,
            5 => receipt.os_boot_id = "00000000-0000-0000-0000-000000000000".into(),
            6 => receipt.launch_generation = 2,
            7 => receipt.previous_generation = 1,
            _ => unreachable!(),
        };
        stage(&state, Some(receipt));
        assert_eq!(
            request(
                &state,
                "/api/v1/pro/configure/execution",
                Some(fixture.config())
            )
            .await
            .0,
            StatusCode::BAD_REQUEST,
            "mismatch {mismatch}"
        );
        assert!(pending(&state));
        assert!(ack(&state).is_none());
        assert!(!quiescent(&state, "w-a"));
        assert!(lock(&state.pro.preferences)["w-a"].execution_active);
    }
}
#[tokio::test]
async fn broad_configuration_and_failed_persistence_cannot_clear_a_pending_gate() {
    let fixture = Fixture::new();
    let state = fixture.dirty().await;
    stage(&state, Some(fixture.receipt(&state, 2, 1)));
    let mut broad = fixture.config();
    broad.as_object_mut().unwrap().remove("workspace_root");
    broad["delegation"]
        .as_object_mut()
        .unwrap()
        .remove("workspace");
    assert_eq!(
        request(&state, "/api/v1/pro/configure/execution", Some(broad))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut legacy = fixture.config();
    legacy.as_object_mut().unwrap().remove("execution");
    assert_eq!(
        request(&state, "/api/v1/pro/configure/workspace", Some(legacy))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let path = state.pro.root.join("state.json");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        request(
            &state,
            "/api/v1/pro/configure/execution",
            Some(fixture.config())
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(pending(&state));
    assert!(ack(&state).is_none());
    assert!(!quiescent(&state, "w-a"));
    assert!(lock(&state.pro.preferences)["w-a"].execution_active);
    assert!(lock(&state.pro.preferences)["w-a"]
        .supervisor_generation
        .is_none());
}
#[tokio::test]
async fn active_authority_cannot_be_replaced_by_cleanup_evidence() {
    let fixture = Fixture::new();
    let state = fixture.state();
    crate::pro::install_execution_fixture(&state, "w-a", 2).unwrap();
    stage(&state, Some(fixture.receipt(&state, 2, 1)));
    assert_eq!(
        request(
            &state,
            "/api/v1/pro/configure/execution",
            Some(fixture.config())
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(pending(&state));
    assert!(ack(&state).is_none());
}
#[test]
fn parser_and_channel_reject_malformed_oversize_non_pipe_and_missing_eof() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let bytes = serde_json::to_vec(&fixture.receipt(&state, 2, 1)).unwrap();
    let (reader, writer) = private_pipe();
    let fd = reader.as_raw_fd();
    let mut writer = std::fs::File::from(writer);
    writer.write_all(&bytes).unwrap();
    drop(writer);
    assert_eq!(read_pipe(reader).unwrap().launch_generation, 2);
    assert_eq!(unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) }, -1);
    let (reader, writer) = private_pipe();
    let mut writer = std::fs::File::from(writer);
    writer.write_all(&vec![b' '; 4097]).unwrap();
    drop(writer);
    assert!(read_pipe(reader).is_err());
    let file = std::fs::File::open(Path::new("/dev/null")).unwrap();
    let fd: OwnedFd = file.into();
    assert!(read_pipe(fd).is_err());
    assert!(decode(b"{}").is_err());
    let (reader, writer) = private_pipe();
    let mut writer = std::fs::File::from(writer);
    writer.write_all(&bytes).unwrap();
    let began = std::time::Instant::now();
    assert!(read_pipe(reader).is_err());
    assert!(began.elapsed() >= Duration::from_secs(3));
    assert!(began.elapsed() < Duration::from_secs(5));
    drop(writer);
}

#[test]
fn provider_descriptor_requires_feature_and_cannot_alias_the_idle_channel() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let mut value = serde_json::to_value(fixture.receipt(&state, 2, 1)).unwrap();
    value["provider_runtime"] = json!({"version":1,"fd":70});
    #[cfg(not(feature = "provider-authority-prototype"))]
    assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
    #[cfg(feature = "provider-authority-prototype")]
    {
        assert!(decode(&serde_json::to_vec(&value).unwrap()).is_ok());
        value["maintenance_control"] = json!({"version":1,"fd":70,"channel_nonce":"A".repeat(43)});
        assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
        value["maintenance_control"]["fd"] = json!(71);
        assert!(decode(&serde_json::to_vec(&value).unwrap()).is_ok());
        for fd in [0, 2, 256, -1] {
            value["provider_runtime"]["fd"] = json!(fd);
            assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        value["provider_runtime"] = json!({"version":1,"fd":70,"capability":"A".repeat(43)});
        assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}

#[tokio::test]
async fn first_supervised_launch_still_requires_a_negotiated_account_lease() {
    let fixture = Fixture::new();
    let state = fixture.state();
    stage(&state, Some(fixture.receipt(&state, 1, 0)));
    assert_eq!(
        request(
            &state,
            "/api/v1/pro/configure/execution",
            Some(fixture.config())
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(ack(&state).is_some());
    assert!(!crate::pro::may_execute(&state, "w-a"));
    assert!(lock(&state.pro.execution.proofs).is_empty());
    let mut config: Configure = serde_json::from_value(fixture.config()).unwrap();
    config.execution = None;
    let grant:Baton=serde_json::from_value(json!({"workspace_id":"w-a","holder_id":"worker-fixture","epoch":1,"requires_fork":false,"server_now":"2026-09-28T00:00:00Z","expires_at":"2026-09-28T00:01:30Z"})).unwrap();
    assert!(accept(
        &state,
        &config,
        &grant,
        state.pro.generation.load(Ordering::Acquire),
        RequestStart::now()
    )
    .is_err());
}

#[tokio::test]
async fn cleanup_clears_only_launch_evidence_and_preserves_missing_policy_fence() {
    let fixture = Fixture::new();
    let state = fixture.dirty().await;
    {
        let mut preferences = lock(&state.pro.preferences);
        let row = preferences.get_mut("w-a").unwrap();
        row.execution_groups_overflow = true;
        row.execution_launch_pending = true;
        row.continuity = None;
    }
    lock(&state.pro.execution.uncertain).insert("w-a".into());
    stage(&state, Some(fixture.receipt(&state, 2, 1)));
    assert_eq!(
        request(
            &state,
            "/api/v1/pro/configure/execution",
            Some(fixture.config())
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(ack(&state).is_some());
    assert!(uncertain(&state, "w-a"));
    assert!(!unclean(&state, "w-a"));
    assert!(!crate::pro::may_execute(&state, "w-a"));
    let preference = lock(&state.pro.preferences)["w-a"].clone();
    assert!(
        !preference.execution_active
            && !preference.execution_launch_pending
            && !preference.execution_groups_overflow
    );
    assert!(preference.execution_groups.is_empty() && preference.execution_starts.is_empty());
    let saved: Value =
        serde_json::from_slice(&std::fs::read(state.pro.root.join("state.json")).unwrap()).unwrap();
    assert_eq!(saved["worker"], true);
    assert_eq!(saved["preferences"]["w-a"]["supervisor_generation"], 2);
}

#[tokio::test]
async fn unreadable_state_cannot_be_repaired_by_process_cleanup() {
    let fixture = Fixture::new();
    let state = fixture.state();
    crate::pro::ensure_root(&state.pro.root).await.unwrap();
    std::fs::write(state.pro.root.join("state.json"), b"not a persisted state").unwrap();
    drop(state);
    let state = fixture.state();
    stage(&state, Some(fixture.receipt(&state, 2, 1)));
    assert_eq!(
        request(
            &state,
            "/api/v1/pro/configure/execution",
            Some(fixture.config())
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(pending(&state) && ack(&state).is_none());
    assert!(!crate::pro::may_execute(&state, "w-a"));
}

#[tokio::test]
async fn fresh_supervised_restart_advances_only_revision_and_persists_before_ack() {
    let fixture = Fixture::new();
    let state = fixture.previously_bound().await;
    let (config, receipt) = fixture.advance(&state);
    stage(&state, Some(receipt));
    let (status, value) = request(&state, "/api/v1/pro/configure/execution", Some(config)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["workspace_configuration"]["workspace"]["revision"], 8);
    assert_eq!(ack(&state).unwrap().registration_revision, 8);
    assert_eq!(ack(&state).unwrap().launch_generation, 2);
    assert!(!crate::pro::may_execute(&state, "w-a"));
    let authority: Value = serde_json::from_slice(
        &std::fs::read(state.pro.root.join("workspace-authority.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(authority["workspace"]["revision"], 8);
    assert_eq!(authority["account_id"], "a-fixture");
    drop(state);
    let restored = fixture.state();
    assert_eq!(
        lock(&restored.pro.authority)
            .acknowledgment()
            .unwrap()
            .workspace
            .revision,
        8
    );
    assert_eq!(
        lock(&restored.pro.preferences)["w-a"].supervisor_generation,
        Some(2)
    );
}

#[tokio::test]
async fn revision_advance_rejects_missing_replayed_wrong_or_ordinary_configuration_receipts() {
    let fixture = Fixture::new();
    let state = fixture.previously_bound().await;
    let (config, receipt) = fixture.advance(&state);
    assert_eq!(
        request(
            &state,
            "/api/v1/pro/configure/execution",
            Some(config.clone())
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for mismatch in 0..7 {
        let mut wrong = receipt.clone();
        match mismatch {
            0 => wrong.registration_revision = 7,
            1 => wrong.launch_generation = 1,
            2 => wrong.previous_generation = 0,
            3 => wrong.account_id = "a-other".into(),
            4 => wrong.root_identity.inode += 1,
            5 => wrong.workspace_id = "w-other".into(),
            6 => wrong.os_boot_id = "00000000-0000-0000-0000-000000000000".into(),
            _ => unreachable!(),
        }
        stage(&state, Some(wrong));
        assert_eq!(
            request(
                &state,
                "/api/v1/pro/configure/execution",
                Some(config.clone())
            )
            .await
            .0,
            StatusCode::BAD_REQUEST,
            "mismatch {mismatch}"
        );
        assert!(pending(&state));
        assert!(ack(&state).is_none());
        assert_eq!(
            lock(&state.pro.authority)
                .acknowledgment()
                .unwrap()
                .workspace
                .revision,
            7
        );
        assert_eq!(
            lock(&state.pro.preferences)["w-a"].supervisor_generation,
            Some(1)
        );
    }
    stage(&state, Some(receipt));
    let mut legacy = config.clone();
    legacy.as_object_mut().unwrap().remove("execution");
    assert_eq!(
        request(&state, "/api/v1/pro/configure/workspace", Some(legacy))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert!(pending(&state));
    for revision in [6, 7] {
        let mut body = config.clone();
        body["delegation"]["workspace"]["revision"] = json!(revision);
        assert_eq!(
            request(&state, "/api/v1/pro/configure/execution", Some(body))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn revision_advance_rejects_changed_identity_and_replaced_root_before_persistence() {
    let fixture = Fixture::new();
    let state = fixture.previously_bound().await;
    let (config, receipt) = fixture.advance(&state);
    stage(&state, Some(receipt.clone()));
    for mismatch in 0..4 {
        let mut body = config.clone();
        match mismatch {
            0 => body["account_id"] = json!("a-other"),
            1 => body["delegation"]["workspace"]["workspace_id"] = json!("w-other"),
            2 => body["endpoint"] = json!("http://127.0.0.1:10"),
            3 => body["workspace_root"] = json!(fixture.root.join("outside")),
            _ => unreachable!(),
        }
        assert_eq!(
            request(&state, "/api/v1/pro/configure/execution", Some(body))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert!(pending(&state));
    }
    assert!(!fixture.root.join("outside").exists());
    std::fs::rename(&fixture.project, fixture.root.join("previous-project")).unwrap();
    std::fs::create_dir(&fixture.project).unwrap();
    let mut replacement = receipt;
    let metadata = std::fs::metadata(&fixture.project).unwrap();
    replacement.root_identity.device = metadata.dev();
    replacement.root_identity.inode = metadata.ino();
    stage(&state, Some(replacement));
    assert_eq!(
        request(&state, "/api/v1/pro/configure/execution", Some(config))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        lock(&state.pro.preferences)["w-a"].supervisor_generation,
        Some(1)
    );
}

#[tokio::test]
async fn revision_advance_rejects_live_unmapped_pty_and_rechecks_before_apply() {
    let fixture = Fixture::new();
    let state = fixture.previously_bound().await;
    let (config, receipt) = fixture.advance(&state);
    stage(&state, Some(receipt));
    let config: Configure = serde_json::from_value(config).unwrap();
    let accepted = crate::pro::authority::prepare(&state, &config, fixture.project.clone())
        .await
        .unwrap();
    let session = state
        .sessions
        .spawn(chimaera_pty::SpawnOpts {
            cwd: fixture.project.clone(),
            name: Some("fixture live process".into()),
            cols: 80,
            rows: 24,
            command: Some(vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()]),
            id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            scrollback: None,
        })
        .unwrap();
    assert!(
        crate::pro::authority::prepare(&state, &config, fixture.project.clone())
            .await
            .is_err()
    );
    assert!(apply(&state, Some(&accepted)).await.is_err());
    assert!(pending(&state));
    assert!(ack(&state).is_none());
    assert_eq!(
        lock(&state.pro.preferences)["w-a"].supervisor_generation,
        Some(1)
    );
    state.sessions.kill(&session.id).unwrap();
}
