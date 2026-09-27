//! Additive account-side handoff contracts. See `HANDOFF.md`.
use serde::{Deserialize, Serialize};

pub const BATON_VERSION: u16 = 1;
pub const BATON_LEASE_SECONDS: u64 = 90;
pub const BATON_RENEW_SECONDS: u64 = 5;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baton {
    pub workspace_id: String,
    pub holder_id: Option<String>,
    pub epoch: u64,
    pub expires_at: Option<String>,
    pub server_now: String,
    pub requires_fork: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcquireBaton {
    pub holder_id: String,
    pub expected_epoch: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldBaton {
    pub holder_id: String,
    pub epoch: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatonConflict {
    pub error: String,
    #[serde(default)]
    pub baton: Option<Baton>,
}
impl std::fmt::Display for BatonConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "baton conflict: {}", self.error)
    }
}
impl std::error::Error for BatonConflict {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirrorRequest {
    pub workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct MirrorCredentials {
    pub workspace_id: String,
    pub repository_url: String,
    pub working_tree_url: String,
    pub username: String,
    pub password: String,
    pub expires_at: String,
    pub read_only: bool,
    pub storage_limit_bytes: u64,
    pub max_file_bytes: u64,
}
impl std::fmt::Debug for MirrorCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MirrorCredentials")
            .field("workspace_id", &self.workspace_id)
            .field("read_only", &self.read_only)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Delegation {
    pub access_token: String,
    pub expires_at: String,
    pub scope: Vec<String>,
    pub device_id: String,
}
impl std::fmt::Debug for Delegation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Delegation")
            .field("device_id", &self.device_id)
            .field("scope", &self.scope)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
