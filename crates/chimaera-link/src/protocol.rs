//! Serializable v0 account and keeper messages. See PROTOCOL.md for lifecycle rules.
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 0;
pub const MAX_DATA_FRAME: usize = 64 * 1024;
pub const MAX_CONTROL_FRAME: usize = 128 * 1024;
pub const MAX_IN_FLIGHT: usize = 16;
pub const MAX_STREAMS: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    None,
    Pro,
    Max,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    NoPlan,
    Unavailable,
    Preparing,
    Ready,
    Sleeping,
    Limited,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerReason {
    ProvisioningDisabled,
    BetaInviteRequired,
    HoursExhausted,
    StorageExhausted,
    SpendLimitReached,
    ProvisioningFailed,
}

/// Account-confirmed preparation stage, not an estimate or daemon readiness.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerPhase {
    Keeper,
    Worker,
    Connecting,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerStatus {
    pub state: WorkerState,
    pub reason: Option<WorkerReason>,
    /// Present only while preparing; older services omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<WorkerPhase>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BillingInterval {
    Month,
    Year,
}

/// A hosted subscription-change review, never an instruction to mutate billing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BillingPortalTarget {
    pub plan: Plan,
    pub interval: BillingInterval,
}
impl BillingPortalTarget {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.plan != Plan::None, "choose Pro or Max");
        Ok(())
    }
}

/// A single native billing attempt. The nonce is not an account credential, but
/// must not be logged or exposed to another window/attempt.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopBillingCallback {
    pub redirect_uri: String,
    pub state: String,
}
impl DesktopBillingCallback {
    pub fn validate(&self) -> anyhow::Result<()> {
        use base64::Engine;
        let url = url::Url::parse(&self.redirect_uri)?;
        let port = url.port().filter(|port| *port >= 1024);
        if port.is_none()
            || self.redirect_uri
                != format!("http://127.0.0.1:{}/billing/callback", port.unwrap_or(0))
            || self.state.len() != 43
            || base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&self.state)
                .map_or(true, |bytes| bytes.len() != 32)
        {
            anyhow::bail!("invalid desktop billing callback");
        }
        Ok(())
    }
}

// Hosted billing URLs are temporary capabilities; never include them in Debug.
#[derive(Clone, Serialize, Deserialize)]
pub struct BillingSession {
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Limits {
    pub cloud_hours: u64,
    pub storage_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub cloud_hours: f64,
    pub storage_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub account_id: String,
    pub email: String,
    pub plan: Plan,
    pub device_id: String,
    pub protocol: u32,
    pub keeper_url: String,
    pub limits: Limits,
    pub usage: Usage,
    pub hours_exhausted: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<String>,
    pub name: String,
    pub last_seen: String,
    #[serde(rename = "this")]
    pub current: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    Ssh,
    Device,
    Worker,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostStatus {
    Connected,
    Connecting,
    Prompting,
    Offline,
}
// Intentionally no Debug: the daemon token must never appear in logs.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Daemon {
    pub token: String,
    pub build: String,
    pub sessions: usize,
}
impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("token", &"[redacted]")
            .field("build", &self.build)
            .field("sessions", &self.sessions)
            .finish()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Host {
    pub id: String,
    pub alias: String,
    pub kind: HostKind,
    pub status: HostStatus,
    pub daemon: Option<Daemon>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddHost {
    pub alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshTarget>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshTarget {
    pub hostname: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
}
fn default_ssh_port() -> u16 {
    22
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: u64,
}
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub code: String,
    pub redirect_uri: String,
    pub code_verifier: String,
    pub device_name: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Host {
        host: Host,
    },
    HostRemoved {
        host_id: String,
    },
    Prompt {
        id: String,
        host_id: String,
        prompt: String,
        echo: bool,
    },
    PromptClosed {
        id: String,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventCommand {
    Answer { id: String, value: Option<String> },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServeCommand {
    Register { alias: String, daemon: Daemon },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServeEvent {
    Registered { host_id: String },
    Open { stream_id: String },
    Close { stream_id: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
