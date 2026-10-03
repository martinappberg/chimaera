use super::*;
use chimaera_core::project_secret_idle::RootIdentity;
use serde_json::json;
use std::{os::unix::fs::MetadataExt, path::PathBuf, sync::atomic::Ordering};

pub(in crate::pro::execution) struct Fixture {
    pub(in crate::pro::execution) state: Arc<AppState>,
    root: PathBuf,
    pub(in crate::pro::execution) binding: Binding,
}
impl Fixture {
    pub(in crate::pro::execution) async fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "chimaera-maintenance-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(root.join("project")).unwrap();
        let root = root.canonicalize().unwrap();
        let project = root.join("project");
        let mut state = AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        );
        // The admission fixture supplies a synthetic boot on non-Linux hosts;
        // production still refuses launch binding without real boot evidence.
        if state.pro.execution.boot.is_none() {
            state.pro.execution.boot = Some("44444444-4444-4444-8444-444444444444".into());
        }
        let state = Arc::new(state);
        state.stopping.store(true, Ordering::Release);
        crate::pro::ensure_root(&state.pro.root).await.unwrap();
        let config: crate::pro::protocol::Configure = serde_json::from_value(json!({
            "account_id":"a-fixture", "role":"worker", "endpoint":"http://127.0.0.1:9",
            "keeper_url":"", "workspace_root":project,
            "execution":{"version":1,"installation_id":null,
                "capability":super::super::wire::ExecutionCapability::managed()},
            "delegation":{"access_token":"synthetic", "expires_at":"2099-01-01T00:00:00Z",
                "scope":["baton","mirror"], "device_id":"worker-fixture",
                "workspace":{"workspace_id":"w-a","revision":7}}
        }))
        .unwrap();
        let accepted = crate::pro::authority::prepare(&state, &config, project.clone())
            .await
            .unwrap();
        let metadata = std::fs::metadata(&project).unwrap();
        let binding = Binding {
            account_id: "a-fixture".into(),
            workspace_id: "w-a".into(),
            root_identity: RootIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            registration_revision: 7,
            launch_generation: 2,
            os_boot_id: state.pro.execution.boot.clone().unwrap(),
        };
        let receipt = serde_json::from_value(json!({
            "version":1,"workspace_id":binding.workspace_id,"account_id":binding.account_id,
            "root_identity":binding.root_identity,"registration_revision":7,
            "launch_generation":2,"previous_generation":0,"os_boot_id":binding.os_boot_id
        }))
        .unwrap();
        super::super::supervisor::stage(&state, Some(receipt));
        super::super::supervisor::apply(&state, Some(&accepted))
            .await
            .unwrap();
        *lock(&state.pro.authority) = crate::pro::authority::Authority::Bound(accepted);
        *lock(&state.pro.runtime) = Some(config);
        state.pro.configured.store(true, Ordering::Release);
        // Synthetic execution authority uses the same grant validator as other
        // execution fixtures; this is not an account or process-idle proof.
        super::super::install_fixture(&state, "w-a", 4).unwrap();
        Self {
            state,
            root,
            binding,
        }
    }
    fn launch(&self) -> Launch {
        Launch::capture(&self.state, &self.binding).unwrap()
    }
    pub(in crate::pro::execution) fn prepare(&self) -> Prepare {
        Prepare {
            version: 1,
            request_id: 1,
            binding: self.binding.clone(),
            attempt_id: "11111111-1111-4111-8111-111111111111".into(),
            operation_id: "22222222-2222-4222-8222-222222222222".into(),
            pending_id: "33333333-3333-4333-8333-333333333333".into(),
            expected_applied_revision: 0,
            expires_in_ms: 30_000,
        }
    }
    fn begin(&self) -> Owner {
        Owner::begin(
            &self.state,
            &self.launch(),
            &self.prepare(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn maintenance_and_final_mutation_admission_serialize_in_both_orders() {
    let fixture = Fixture::new().await;
    assert!(crate::pro::may_execute(&fixture.state, "w-a"));
    let guard = mutation::begin(&fixture.state, "w-a", 4, 0).unwrap();
    assert!(Owner::begin(
        &fixture.state,
        &fixture.launch(),
        &fixture.prepare(),
        Instant::now() + Duration::from_secs(30)
    )
    .is_err());
    assert!(!closed(&fixture.state, "w-a"));
    drop(guard);
    let owner = fixture.begin();
    assert!(!crate::pro::may_write(&fixture.state, "w-a"));
    assert!(!crate::pro::may_execute(&fixture.state, "w-a"));
    assert!(!mutation::idle(&fixture.state, "w-a"));
    assert!(!super::super::quiescent(&fixture.state, "w-a"));
    assert!(mutation::begin(&fixture.state, "w-a", 4, 0).is_err());
    assert!(mutation::begin_launch(&fixture.state, "w-a").is_err());
    assert!(mutation::begin_import(&fixture.state, "w-a", 4, 0)
        .await
        .is_err());
    assert!(Owner::begin(
        &fixture.state,
        &fixture.launch(),
        &fixture.prepare(),
        Instant::now() + Duration::from_secs(30)
    )
    .is_err());
    owner.abort_unstarted().unwrap();
    assert!(!closed(&fixture.state, "w-a"));
    assert!(crate::pro::may_execute(&fixture.state, "w-a"));
    assert!(mutation::begin_launch(&fixture.state, "w-a").is_ok());
}

#[tokio::test]
async fn launch_binding_generation_and_original_deadline_cannot_be_rebound() {
    let fixture = Fixture::new().await;
    for binding in [
        Binding {
            account_id: "a-other".into(),
            ..fixture.binding.clone()
        },
        Binding {
            registration_revision: 8,
            ..fixture.binding.clone()
        },
        Binding {
            launch_generation: 3,
            ..fixture.binding.clone()
        },
        Binding {
            root_identity: RootIdentity {
                device: fixture.binding.root_identity.device,
                inode: fixture.binding.root_identity.inode + 1,
            },
            ..fixture.binding.clone()
        },
        Binding {
            os_boot_id: "ffffffff-ffff-ffff-ffff-ffffffffffff".into(),
            ..fixture.binding.clone()
        },
    ] {
        assert!(Launch::capture(&fixture.state, &binding).is_err());
    }
    let launch = fixture.launch();
    assert!(Owner::begin(&fixture.state, &launch, &fixture.prepare(), Instant::now()).is_err());
    assert!(mutation::idle(&fixture.state, "w-a"));
    let deadline = Instant::now() + Duration::from_millis(100);
    let owner = Owner::begin(&fixture.state, &launch, &fixture.prepare(), deadline).unwrap();
    assert!(lock(&fixture.state.pro.execution.commits.0).maintenance["w-a"].deadline <= deadline);
    fixture.state.pro.generation.fetch_add(1, Ordering::AcqRel);
    assert!(owner.parking_started().is_err());
    owner.abort_unstarted().unwrap();
    assert!(Owner::begin(
        &fixture.state,
        &launch,
        &fixture.prepare(),
        Instant::now() + Duration::from_secs(30)
    )
    .is_err());
    assert!(mutation::idle(&fixture.state, "w-a"));
}

#[tokio::test]
async fn loss_after_possible_effect_keeps_counted_recovery_fence() {
    let fixture = Fixture::new().await;
    let owner = fixture.begin();
    owner.parking_started().unwrap();
    assert!(owner.abort_unstarted().is_err());
    assert!(closed(&fixture.state, "w-a"));
    assert!(!mutation::idle(&fixture.state, "w-a"));
    assert!(
        lock(&fixture.state.pro.execution.commits.0).maintenance["w-a"].phase
            == Phase::RecoveryRequired
    );
    assert!(!crate::pro::may_execute(&fixture.state, "w-a"));
    let untouched = Fixture::new().await;
    drop(untouched.begin());
    assert!(closed(&untouched.state, "w-a"));
    assert!(
        lock(&untouched.state.pro.execution.commits.0).maintenance["w-a"].phase
            == Phase::RecoveryRequired
    );
}

#[tokio::test]
async fn expired_attempt_cannot_start_effects_and_owned_work_outlives_observer() {
    let fixture = Fixture::new().await;
    let owner = fixture.begin();
    lock(&fixture.state.pro.execution.commits.0)
        .maintenance
        .get_mut("w-a")
        .unwrap()
        .deadline = Instant::now();
    assert!(owner.parking_started().is_err());
    owner.abort_unstarted().unwrap();
    let owner = fixture.begin();
    let (release, waiting) = tokio::sync::oneshot::channel();
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let worker = tokio::spawn(async move {
        waiting.await.unwrap();
        owner.abort_unstarted().unwrap();
        // A disappeared observer does not cancel the actual owner or rollback.
        let _ = send.send(());
    });
    let observer = tokio::spawn(async move {
        let _ = receive.await;
    });
    observer.abort();
    let _ = observer.await;
    assert!(closed(&fixture.state, "w-a"));
    assert!(!worker.is_finished());
    release.send(()).unwrap();
    worker.await.unwrap();
    assert!(!closed(&fixture.state, "w-a"));
    assert!(mutation::idle(&fixture.state, "w-a"));
}

#[tokio::test]
async fn competing_final_mutation_and_maintenance_have_one_admission_winner() {
    let fixture = Fixture::new().await;
    for _ in 0..16 {
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let launch = fixture.launch();
        let prepare = fixture.prepare();
        let maintenance_state = fixture.state.clone();
        let maintenance_barrier = barrier.clone();
        let maintenance = std::thread::spawn(move || {
            maintenance_barrier.wait();
            Owner::begin(
                &maintenance_state,
                &launch,
                &prepare,
                Instant::now() + Duration::from_secs(30),
            )
        });
        let mutation_state = fixture.state.clone();
        let mutation_barrier = barrier.clone();
        let mutation = std::thread::spawn(move || {
            mutation_barrier.wait();
            mutation::begin(&mutation_state, "w-a", 4, 0)
        });
        barrier.wait();
        let maintenance = maintenance.join().unwrap();
        let mutation = mutation.join().unwrap();
        assert_ne!(maintenance.is_ok(), mutation.is_ok());
        if let Ok(owner) = maintenance {
            owner.abort_unstarted().unwrap();
        }
        drop(mutation);
        assert!(mutation::idle(&fixture.state, "w-a"));
        assert!(!closed(&fixture.state, "w-a"));
    }
}

#[test]
fn ordinary_unconfigured_local_work_keeps_its_existing_admission() {
    let root = std::env::temp_dir().join(format!(
        "chimaera-maintenance-free-{}",
        chimaera_core::generate_token()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let state = AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.clone(),
        root.join("config"),
    );
    assert!(!closed(&state, "w-free"));
    assert!(crate::pro::may_write(&state, "w-free"));
    assert!(crate::pro::may_execute(&state, "w-free"));
    assert!(mutation::begin_launch(&state, "w-free").unwrap().is_none());
    assert!(mutation::idle(&state, "w-free"));
    std::fs::remove_dir_all(root).unwrap();
}
