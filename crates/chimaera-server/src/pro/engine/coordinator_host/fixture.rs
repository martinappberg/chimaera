//! Explicit nondefault test host. No private dependency or production setter.
use crate::{daemon_extension::Runtime, lock, AppState};
use anyhow::ensure;
use axum::{extract::State, http::StatusCode, response::Response, Json};
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
};

pub struct Reply {
    pub status: StatusCode,
    pub body: serde_json::Value,
}
async fn reply(response: Response) -> anyhow::Result<Reply> {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
    Ok(Reply {
        status,
        body: if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        },
    })
}

/// Caller owns the disposable root; it is never removed by this handle.
/// A failed close retains that root and the original work for diagnosis.
#[derive(Clone, Copy)]
pub enum Layout {
    Standard,
    Continuity,
    Projects,
    AgentContexts,
}

pub struct Harness {
    pub(in crate::pro::engine) state: Arc<AppState>,
    root: PathBuf,
    transfer_key: String,
}
impl Harness {
    pub async fn new(
        root: PathBuf,
        factory: Option<fn() -> Arc<dyn Runtime>>,
    ) -> anyhow::Result<Self> {
        Self::new_layout(root, Layout::Standard, factory).await
    }
    pub async fn new_layout(
        root: PathBuf,
        layout: Layout,
        factory: Option<fn() -> Arc<dyn Runtime>>,
    ) -> anyhow::Result<Self> {
        Self::new_captured(root, layout, None, factory, None).await
    }
    /// Original project fixture data directory within one caller-owned anchor.
    pub async fn new_projects(
        root: PathBuf,
        data: PathBuf,
        factory: Option<fn() -> Arc<dyn Runtime>>,
    ) -> anyhow::Result<Self> {
        Self::new_captured(root, Layout::Projects, Some(data), factory, None).await
    }
    /// Captured trusted composition before AppState publication; no setter.
    pub async fn new_with_runtime(
        root: PathBuf,
        runtime: Arc<dyn Runtime>,
    ) -> anyhow::Result<Self> {
        Self::new_captured(root, Layout::Standard, None, None, Some(runtime)).await
    }
    async fn new_captured(
        root: PathBuf,
        layout: Layout,
        data: Option<PathBuf>,
        factory: Option<fn() -> Arc<dyn Runtime>>,
        runtime: Option<Arc<dyn Runtime>>,
    ) -> anyhow::Result<Self> {
        let (root, state) = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            ensure!(
                root.is_absolute() && root.as_os_str().len() <= 4096,
                "fixture root must be bounded and absolute"
            );
            // Resolve this caller's owned anchor once, never process HOME.
            let root = root.canonicalize()?;
            let metadata = std::fs::symlink_metadata(&root)?;
            ensure!(
                metadata.is_dir()
                    && metadata.uid() == rustix::process::geteuid().as_raw()
                    && metadata.mode() & 0o777 == 0o700
                    && root.parent().is_some(),
                "fixture root must be an owned private directory"
            );
            for suffix in ["data", "config"] {
                let path = root.join(suffix);
                match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                let metadata = std::fs::symlink_metadata(&path)?;
                ensure!(
                    metadata.is_dir()
                        && metadata.uid() == rustix::process::geteuid().as_raw()
                        && metadata.mode() & 0o777 == 0o700,
                    "fixture child is untrusted"
                );
            }
            let project_data = match data {
                Some(data) => {
                    ensure!(
                        data == root || data.parent() == Some(root.as_path()),
                        "fixture data outside original anchor"
                    );
                    if data != root {
                        std::fs::DirBuilder::new()
                            .mode(0o700)
                            .create(&data)
                            .or_else(|error| {
                                if error.kind() == std::io::ErrorKind::AlreadyExists {
                                    Ok(())
                                } else {
                                    Err(error)
                                }
                            })?;
                    }
                    let metadata = std::fs::symlink_metadata(&data)?;
                    ensure!(
                        metadata.is_dir()
                            && metadata.uid() == rustix::process::geteuid().as_raw()
                            && metadata.mode() & 0o777 == 0o700,
                        "fixture data child untrusted"
                    );
                    data
                }
                None => root.clone(),
            };
            let mut state = AppState::new(
                match layout {
                    Layout::Standard => chimaera_core::generate_token(),
                    Layout::Projects => "local-test".into(),
                    Layout::AgentContexts => "test-token".into(),
                    _ => "fixture".into(),
                },
                match layout {
                    Layout::Standard => "fixture-device".into(),
                    Layout::AgentContexts => "testhost".into(),
                    _ => "fixture".into(),
                },
                match layout {
                    Layout::Standard => std::process::id(),
                    _ => 4242,
                },
                0,
                match layout {
                    Layout::Projects | Layout::AgentContexts => project_data.clone(),
                    _ => root.join("data"),
                },
                match layout {
                    Layout::Continuity => root.join("home/.claude"),
                    Layout::Projects => project_data.join("config"),
                    _ => root.join("config"),
                },
            );
            if matches!(layout, Layout::AgentContexts) {
                let provider_home = root.join("provider-home");
                state.claude_projects_dir = provider_home.join(".claude/projects");
                state.claude_settings_path = provider_home.join(".claude/settings.json");
                state.codex_config_path = provider_home.join(".codex/config.toml");
                state.managed_root = root.join("managed-agents");
                state.legacy_managed_root = None;
            }
            Ok::<_, anyhow::Error>((root, state))
        })
        .await??;
        if let Some(runtime) = runtime.or_else(|| factory.map(|factory| factory())) {
            state.pro().set_runtime(runtime);
        }
        Ok(Self {
            state: Arc::new(state),
            root,
            transfer_key: format!(
                "fixture-transfer-{}",
                &chimaera_core::generate_token()[..16]
            ),
        })
    }
    pub(in crate::pro::engine) fn fixture_root(&self) -> &std::path::Path {
        &self.root
    }
    /// Original filesystem/process owners for the relocated repository tests.
    /// This fixture alone admits sibling staging paths within its owned root.
    pub async fn transfer_owner(
        &self,
    ) -> anyhow::Result<Arc<crate::pro::transfer_host::TransferHost>> {
        let guard = Arc::new(
            self.state
                .pro()
                .cache(&self.transfer_key)?
                .lock_owned()
                .await,
        );
        crate::pro::transfer_host::TransferHost::capture_fixture(
            self.state.clone(),
            self.root.clone(),
            self.transfer_key.clone(),
            guard,
        )
        .await
    }
    /// Exact original cache registry, available only in this nondefault fixture.
    pub fn cache(&self, workspace: &str) -> anyhow::Result<Arc<tokio::sync::Mutex<()>>> {
        ensure!(
            crate::pro::valid_id(workspace),
            "invalid fixture cache identity"
        );
        self.state.pro().cache(workspace)
    }
    pub fn configuration(&self) -> Arc<tokio::sync::Mutex<()>> {
        self.state.pro().configuration.clone()
    }
    pub async fn transfer_owner_with_cache(
        &self,
        workspace: &str,
        guard: Arc<tokio::sync::OwnedMutexGuard<()>>,
    ) -> anyhow::Result<Arc<crate::pro::transfer_host::TransferHost>> {
        let actual = self.cache(workspace)?;
        ensure!(
            Arc::ptr_eq(tokio::sync::OwnedMutexGuard::mutex(&guard), &actual),
            "fixture cache guard is not the original registry owner"
        );
        crate::pro::transfer_host::TransferHost::capture_fixture(
            self.state.clone(),
            self.root.clone(),
            workspace.to_owned(),
            guard,
        )
        .await
    }
    pub async fn settle_transfer(&self) -> anyhow::Result<()> {
        let _original = self
            .state
            .pro()
            .cache(&self.transfer_key)?
            .lock_owned()
            .await;
        crate::pro::transport::cache_quiescent(&self.transfer_key)
    }
    pub async fn configure(&self, request: serde_json::Value) -> anyhow::Result<Reply> {
        ensure!(
            serde_json::to_vec(&request)?.len() <= 64 * 1024,
            "fixture configuration exceeds bound"
        );
        let config = serde_json::from_value(request)?;
        reply(crate::pro::configure(State(self.state.clone()), Json(config)).await).await
    }
    pub async fn status(&self) -> serde_json::Value {
        crate::pro::status(State(self.state.clone())).await.0
    }
    pub async fn disconnect(&self) -> anyhow::Result<Reply> {
        reply(crate::pro::disconnect(State(self.state.clone())).await).await
    }
    pub async fn register_workspace(&self, root: PathBuf) -> anyhow::Result<Reply> {
        let fixture_root = self.root.clone();
        let canonical = tokio::task::spawn_blocking(move || {
            let root = root.canonicalize()?;
            ensure!(
                root.starts_with(&fixture_root) && root != fixture_root,
                "fixture project is outside its owned root"
            );
            Ok::<_, anyhow::Error>(root)
        })
        .await??;
        let request = serde_json::from_value(serde_json::json!({"root":canonical}))?;
        reply(crate::api::create_workspace(State(self.state.clone()), Json(request)).await).await
    }
    /// Positive original task and transport drain precedes successful close.
    /// This five-second fixture cleanup bound never renews an operation budget.
    pub async fn close(&self) -> anyhow::Result<()> {
        self.state.stopping.store(true, Ordering::Release);
        {
            let _configuration = self.state.pro().configuration.lock().await;
            crate::pro::routes::stop_tasks(&self.state).await?;
        }
        let result = crate::pro::drain(
            State(self.state.clone()),
            axum::body::Bytes::from_static(b"{\"deadline_ms\":5000}"),
        )
        .await;
        ensure!(result.status().is_success(), "fixture work has not drained");
        crate::pro::shutdown(&self.state).await;
        // Actual registered processes must also have settled. Merely closing
        // the policy observer cannot certify their resource cleanup.
        ensure!(lock(&self.state.agents).is_empty(), "fixture agents remain");
        ensure!(
            self.state.sessions.list().is_empty(),
            "fixture sessions remain"
        );
        Ok(())
    }
}
