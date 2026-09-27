//! The daemon consumes the documented wire without linking the TLS app crate.
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Delegation {
    pub access_token: String,
    pub expires_at: String,
    pub scope: Vec<String>,
    pub device_id: String,
}

#[derive(Clone, Deserialize)]
pub(crate) struct Configure {
    #[serde(default)]
    pub role: Role,
    pub endpoint: String,
    pub keeper_url: String,
    pub delegation: Delegation,
    #[serde(default)]
    pub hours_exhausted: bool,
}

#[derive(Clone, Deserialize)]
pub(super) struct Baton {
    #[serde(default)]
    pub mirror_disabled: bool,
    pub workspace_id: String,
    pub holder_id: Option<String>,
    pub epoch: u64,
    pub expires_at: Option<String>,
    pub server_now: String,
    pub requires_fork: bool,
}

#[derive(Clone, Deserialize)]
pub(super) struct MirrorCredentials {
    pub workspace_id: String,
    pub repository_url: String,
    pub working_tree_url: String,
    pub username: String,
    pub password: String,
    pub read_only: bool,
    pub storage_limit_bytes: u64,
    pub max_file_bytes: u64,
}

#[derive(Clone, Deserialize)]
pub(super) struct Host {
    pub id: String,
    pub kind: String,
    pub status: String,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Role {
    #[default]
    Device,
    Worker,
}
