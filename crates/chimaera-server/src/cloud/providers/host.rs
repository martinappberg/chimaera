//! Fixed worker provider effect handles. The private controller owns policy and attempts.
use crate::{agents::AgentKind, AppState};
use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock, Weak,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkerProvider {
    Claude,
    Codex,
    GitHub,
}
impl WorkerProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::GitHub => "github",
        }
    }
    fn agent(self) -> Option<AgentKind> {
        match self {
            Self::Claude => Some(AgentKind::Claude),
            Self::Codex => Some(AgentKind::Codex),
            Self::GitHub => None,
        }
    }
}
pub struct ProviderDefinition {
    pub id: &'static str,
    pub provider: WorkerProvider,
    pub methods: &'static [&'static str],
    pub kind: Option<WorkerProvider>,
}
pub const PROVIDERS: &[ProviderDefinition] = &[
    ProviderDefinition {
        id: "claude",
        provider: WorkerProvider::Claude,
        methods: &["browser_code"],
        kind: Some(WorkerProvider::Claude),
    },
    ProviderDefinition {
        id: "codex",
        provider: WorkerProvider::Codex,
        methods: &["device_code"],
        kind: Some(WorkerProvider::Codex),
    },
    ProviderDefinition {
        id: "github",
        provider: WorkerProvider::GitHub,
        methods: &["device_code"],
        kind: None,
    },
];
pub fn definition(id: &str) -> Option<&'static ProviderDefinition> {
    PROVIDERS.iter().find(|p| p.id == id)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderState {
    Missing,
    NeedsSignIn,
    SignedIn,
    Unknown,
    Unavailable,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProviderStatus {
    pub id: String,
    pub label: String,
    pub category: String,
    pub installed: Option<bool>,
    pub state: ProviderState,
    pub reason: Option<String>,
    pub checked_at: Option<u64>,
    pub methods: Vec<String>,
    pub disconnect_supported: bool,
}
impl ProviderStatus {
    pub fn new(id: &str) -> Self {
        let def = definition(id);
        let catalog = chimaera_core::cloud_providers::provider_definition(id);
        Self {
            id: id.into(),
            label: catalog.map_or(id, |d| d.label.as_str()).into(),
            category: catalog.map_or("agent", |d| d.category.as_str()).into(),
            installed: None,
            state: ProviderState::Unknown,
            reason: None,
            checked_at: None,
            disconnect_supported: def.is_some() && matches!(id, "claude" | "codex" | "github"),
            methods: def.map_or_else(Vec::new, |d| {
                d.methods.iter().map(|m| (*m).into()).collect()
            }),
        }
    }
    pub fn failed(mut self, reason: &str) -> Self {
        self.reason = Some(reason.into());
        self
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Waiting,
    Verifying,
    Connected,
    Disconnected,
    Failed,
    Canceled,
    Expired,
}
impl Phase {
    pub fn pending(self) -> bool {
        matches!(self, Self::Preparing | Self::Waiting | Self::Verifying)
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Browser {
        url: String,
        input: &'static str,
    },
    DeviceCode {
        verification_url: String,
        user_code: String,
    },
    Terminal {
        workspace_id: String,
        session_id: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Connect,
    Disconnect,
}
#[derive(Clone, Debug, Serialize)]
pub struct Connection {
    pub id: String,
    pub provider_id: String,
    pub operation: Operation,
    pub phase: Phase,
    pub expires_at: u64,
    pub action: Option<Action>,
    pub error_code: Option<String>,
}

pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub trait WorkerProviders: Send + Sync {
    fn cached_observations(&self) -> Vec<ProviderStatus>;
    fn pending_disconnect(&self) -> Option<Connection>;
    fn readiness<'a>(
        &'a self,
        ids: &'a [String],
        fresh: bool,
    ) -> ProviderFuture<'a, Vec<ProviderStatus>>;
    fn start(
        &self,
        provider: WorkerProvider,
        operation: Operation,
    ) -> Result<Connection, &'static str>;
    fn observe(&self, id: &str) -> Option<Connection>;
    fn submit(&self, id: &str, code: String) -> Result<Connection, &'static str>;
    fn cancel<'a>(&'a self, id: &'a str) -> ProviderFuture<'a, Result<Connection, &'static str>>;
}
#[derive(Default)]
pub(crate) struct ProviderSlot(OnceLock<Option<Arc<dyn WorkerProviders>>>);
impl ProviderSlot {
    #[cfg(test)]
    pub(crate) fn initialized(&self) -> bool {
        self.0.get().is_some()
    }
    pub(super) fn resolve(
        &self,
        factory: impl FnOnce() -> Option<Arc<dyn WorkerProviders>>,
    ) -> Option<&Arc<dyn WorkerProviders>> {
        self.0.get_or_init(factory).as_ref()
    }
    pub(super) fn current(&self) -> Option<&Arc<dyn WorkerProviders>> {
        self.0.get().and_then(Option::as_ref)
    }
    pub(super) fn admit(&self, state: &Arc<AppState>) -> Option<&Arc<dyn WorkerProviders>> {
        // Internal handoff readiness also serves explicitly configured workers;
        // provider HTTP routes retain their separate environment admission.
        if !super::super::enabled() && !crate::pro::is_worker(state) {
            return None;
        }
        self.resolve(|| {
            state
                .daemon_extension
                .as_ref()
                .and_then(|runtime| runtime.worker_providers(ProviderHost::new(state)))
        })
    }
}
#[derive(Clone)]
pub struct ProviderHost {
    state: Weak<AppState>,
    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    fixture_bins: Option<Arc<std::sync::Mutex<std::collections::HashMap<WorkerProvider, PathBuf>>>>,
}
pub struct ProviderWork {
    _state: Arc<AppState>,
}
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
pub(crate) fn active() -> usize {
    ACTIVE.load(Ordering::Acquire)
}
pub struct ActivePermit {
    _private: (),
}
impl Drop for ActivePermit {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Clone)]
pub struct ProviderSession {
    state: Weak<AppState>,
    workspace_id: String,
    session_id: String,
}
impl ProviderSession {
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
    pub fn id(&self) -> &str {
        &self.session_id
    }
    pub fn alive(&self) -> bool {
        self.state
            .upgrade()
            .is_some_and(|s| s.sessions.get(&self.session_id).is_some_and(|s| s.alive))
    }
    pub fn kill(&self) {
        if let Some(state) = self.state.upgrade() {
            let _ = state.sessions.kill(&self.session_id);
        }
    }
}
impl ProviderHost {
    pub(crate) fn new(state: &Arc<AppState>) -> Self {
        Self {
            state: Arc::downgrade(state),
            #[cfg(all(unix, feature = "daemon-extension-fixture"))]
            fixture_bins: None,
        }
    }
    pub fn retain(&self) -> Result<ProviderWork, &'static str> {
        Ok(ProviderWork {
            _state: self.state.upgrade().ok_or("unavailable")?,
        })
    }
    pub fn active_permit(&self) -> ActivePermit {
        ACTIVE.fetch_add(1, Ordering::AcqRel);
        ActivePermit { _private: () }
    }
    pub fn fresh_connection_id(&self) -> String {
        crate::agents::fresh_session_id()
    }
    pub fn home(&self) -> PathBuf {
        self.state
            .upgrade()
            .map(|state| home(&state))
            .unwrap_or_default()
    }
    pub fn stopping(&self) -> bool {
        self.state
            .upgrade()
            .is_none_or(|s| s.stopping.load(Ordering::Acquire))
    }
    pub fn changed(&self) {
        if let Some(state) = self.state.upgrade() {
            state.changes.notify_waiters();
        }
    }
    pub async fn binary(
        &self,
        provider: WorkerProvider,
        fresh: bool,
    ) -> Result<ProviderExecutable, &'static str> {
        let state = self.state.upgrade().ok_or("unavailable")?;
        #[cfg(all(unix, feature = "daemon-extension-fixture"))]
        if let Some(bins) = &self.fixture_bins {
            if let Some(path) = crate::lock(bins).get(&provider).cloned() {
                return Ok(ProviderExecutable {
                    bin: path,
                    home: home(&state),
                });
            }
        }
        let bin = if let Some(kind) = provider.agent() {
            let missing = crate::lock(&state.agent_bins)
                .get(&kind)
                .is_some_and(|h| h.path.is_err());
            crate::launcher::detect(&state, kind, fresh || missing)
                .await
                .path
                .map_err(|_| "not_installed")?
        } else {
            let out = super::process::output(&mut super::process::command(
                std::path::Path::new("/bin/sh"),
                &["-c", "command -v gh"],
                &home(&state),
            ))
            .await?;
            if !out.success {
                return Err("not_installed");
            }
            let path = PathBuf::from(
                String::from_utf8(out.stdout)
                    .map_err(|_| "probe_failed")?
                    .trim(),
            );
            if !path.is_absolute() {
                return Err("probe_failed");
            }
            path
        };
        Ok(ProviderExecutable {
            bin,
            home: home(&state),
        })
    }
    pub async fn install_agent(
        &self,
        provider: WorkerProvider,
    ) -> Result<ProviderSession, &'static str> {
        let kind = provider.agent().ok_or("installation_unavailable")?;
        let state = self.state.upgrade().ok_or("unavailable")?;
        let root = home(&state).join("projects/.chimaera-setup");
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|_| "setup_unavailable")?;
        let owner = state.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            crate::lock(&owner.workspaces)
                .add_internal(root)
                .map_err(|_| "setup_unavailable")
        })
        .await
        .map_err(|_| "setup_unavailable")??;
        let response = crate::runtimes::install_agent(
            State(state.clone()),
            Path(kind.as_str().into()),
            Json(
                serde_json::from_value(serde_json::json!({"workspace_id":workspace.id}))
                    .map_err(|_| "setup_failed")?,
            ),
        )
        .await;
        if !response.status().is_success() {
            return Err("setup_failed");
        }
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .map_err(|_| "setup_failed")?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "setup_failed")?;
        let session_id = value["session_id"]
            .as_str()
            .filter(|s| s.len() <= 128)
            .ok_or("setup_failed")?
            .to_owned();
        Ok(ProviderSession {
            state: Arc::downgrade(&state),
            workspace_id: workspace.id,
            session_id,
        })
    }
}
fn home(state: &AppState) -> PathBuf {
    state
        .claude_settings_path
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub struct ProviderExecutable {
    bin: PathBuf,
    home: PathBuf,
}
pub struct GitHubUser(String);
pub enum ProviderInvocation {
    ClaudeStatus,
    ClaudeLogin,
    ClaudeLogout,
    CodexAppServer,
    GitHubStatus { active: bool },
    GitHubLoginHelp,
    GitHubLogin { skip_ssh: bool },
    GitHubSetupGit,
    GitHubLogout { user: GitHubUser },
}
impl ProviderInvocation {
    pub fn from_legacy_args(args: &[&str]) -> Option<Self> {
        Some(match args {
            ["auth", "status", "--json"] => Self::ClaudeStatus,
            ["auth", "login", "--claudeai"] => Self::ClaudeLogin,
            ["auth", "logout"] => Self::ClaudeLogout,
            ["app-server"] => Self::CodexAppServer,
            ["auth", "status", "--active", "--hostname", "github.com", "--json", "hosts"] => {
                Self::GitHubStatus { active: true }
            }
            ["auth", "status", "--hostname", "github.com", "--json", "hosts"] => {
                Self::GitHubStatus { active: false }
            }
            ["auth", "login", "--help"] => Self::GitHubLoginHelp,
            ["auth", "login", "--hostname", "github.com", "--git-protocol", "https", "--web"] => {
                Self::GitHubLogin { skip_ssh: false }
            }
            ["auth", "login", "--hostname", "github.com", "--git-protocol", "https", "--web", "--skip-ssh-key"] => {
                Self::GitHubLogin { skip_ssh: true }
            }
            ["auth", "setup-git", "--hostname", "github.com"] => Self::GitHubSetupGit,
            ["auth", "logout", "--hostname", "github.com", "--user", user]
                if !user.is_empty()
                    && user.len() <= 39
                    && !user.starts_with('-')
                    && user.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') =>
            {
                Self::GitHubLogout {
                    user: GitHubUser((*user).into()),
                }
            }
            _ => return None,
        })
    }
}
impl ProviderExecutable {
    pub fn command(&self, invocation: ProviderInvocation) -> tokio::process::Command {
        let mut args = match &invocation {
            ProviderInvocation::ClaudeStatus => vec!["auth", "status", "--json"],
            ProviderInvocation::ClaudeLogin => vec!["auth", "login", "--claudeai"],
            ProviderInvocation::ClaudeLogout => vec!["auth", "logout"],
            ProviderInvocation::CodexAppServer => vec!["app-server"],
            ProviderInvocation::GitHubStatus { active: true } => vec![
                "auth",
                "status",
                "--active",
                "--hostname",
                "github.com",
                "--json",
                "hosts",
            ],
            ProviderInvocation::GitHubStatus { active: false } => vec![
                "auth",
                "status",
                "--hostname",
                "github.com",
                "--json",
                "hosts",
            ],
            ProviderInvocation::GitHubLoginHelp => vec!["auth", "login", "--help"],
            ProviderInvocation::GitHubLogin { .. } => vec![
                "auth",
                "login",
                "--hostname",
                "github.com",
                "--git-protocol",
                "https",
                "--web",
            ],
            ProviderInvocation::GitHubSetupGit => {
                vec!["auth", "setup-git", "--hostname", "github.com"]
            }
            ProviderInvocation::GitHubLogout { user } => {
                vec![
                    "auth",
                    "logout",
                    "--hostname",
                    "github.com",
                    "--user",
                    user.0.as_str(),
                ]
            }
        };
        if matches!(
            &invocation,
            ProviderInvocation::GitHubLogin { skip_ssh: true }
        ) {
            args.push("--skip-ssh-key");
        }
        super::process::command(&self.bin, &args, &self.home)
    }
    #[cfg(all(unix, feature = "daemon-extension-fixture"))]
    pub fn fixture(bin: PathBuf, home: PathBuf) -> Self {
        Self { bin, home }
    }
}
#[cfg(all(unix, feature = "daemon-extension-fixture"))]
pub mod fixture;
