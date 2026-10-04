//! Real original daemon state for explicit, nondefault synthetic provider fixtures.
use super::*;
use std::{collections::HashMap, sync::Mutex};

pub struct Harness {
    state: Arc<AppState>,
    root: PathBuf,
    bins: Arc<Mutex<HashMap<WorkerProvider, PathBuf>>>,
}
impl Harness {
    pub fn new(label: &str) -> Self {
        assert!(
            label.len() <= 64
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        );
        let root = std::env::temp_dir().join(format!(
            "chimaera-provider-{label}-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut state = AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.join("data"),
            root.join("config"),
        );
        state.claude_settings_path = root.join(".claude/settings.json");
        Self {
            state: Arc::new(state),
            root,
            bins: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    pub fn owner(
        &self,
        factory: impl FnOnce(ProviderHost) -> Option<Arc<dyn WorkerProviders>>,
    ) -> Option<Arc<dyn WorkerProviders>> {
        self.state
            .cloud_providers
            .resolve(|| factory(self.host()))
            .cloned()
    }
    pub fn cached(&self) -> Vec<ProviderStatus> {
        super::super::cached_observations(&self.state)
    }
    pub fn initialized(&self) -> bool {
        self.state.cloud_providers.0.get().is_some()
    }
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }
    pub fn host(&self) -> ProviderHost {
        ProviderHost {
            state: Arc::downgrade(&self.state),
            fixture_bins: Some(self.bins.clone()),
        }
    }
}
impl ProviderHost {
    pub fn fixture_preset(&self, provider: WorkerProvider, path: PathBuf) {
        self.retain().expect("original fixture owner");
        // The explicit fixture is the only path-injection boundary; production
        // resolution always uses the original daemon launcher/login shell.
        assert!(path.is_absolute());
        crate::lock(self.fixture_bins.as_ref().expect("fixture host")).insert(provider, path);
    }
    pub fn fixture_executable(&self, path: PathBuf) -> ProviderExecutable {
        ProviderExecutable::fixture(path, self.home())
    }
    pub fn fixture_sessions_empty(&self) -> bool {
        self.state
            .upgrade()
            .expect("fixture owner")
            .sessions
            .list()
            .is_empty()
    }
    pub fn fixture_chats_empty(&self) -> bool {
        self.state
            .upgrade()
            .expect("fixture owner")
            .chat
            .list()
            .is_empty()
    }
    pub fn fixture_workspaces_empty(&self) -> bool {
        crate::lock(&self.state.upgrade().expect("fixture owner").workspaces)
            .list()
            .is_empty()
    }
}
