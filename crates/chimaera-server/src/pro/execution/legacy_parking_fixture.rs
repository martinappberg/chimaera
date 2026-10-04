use crate::{lock, AppState};
use chimaera_core::project_secret_idle::RootIdentity;
use chimaera_core::project_secret_idle::{Binding, Prepare};
use serde_json::json;
use std::sync::Arc;
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
                "capability":crate::pro::execution::wire::ExecutionCapability::managed()},
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
        crate::pro::execution::supervisor::stage(&state, Some(receipt));
        crate::pro::execution::supervisor::apply(&state, Some(&accepted))
            .await
            .unwrap();
        *lock(&state.pro.authority) = crate::pro::authority::Authority::Bound(accepted);
        *lock(&state.pro.runtime) = Some(config);
        state.pro.configured.store(true, Ordering::Release);
        // Synthetic execution authority uses the same grant validator as other
        // execution fixtures; this is not an account or process-idle proof.
        crate::pro::execution::install_fixture(&state, "w-a", 4).unwrap();
        Self {
            state,
            root,
            binding,
        }
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
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
