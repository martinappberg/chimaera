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

/// An immutable project restriction, not a requested expansion of authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    pub workspace_id: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfigureAck {
    pub workspace_authority: u16,
    pub workspace: WorkspaceBinding,
    pub workspace_root: std::path::PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Delegation {
    pub access_token: String,
    pub expires_at: String,
    pub scope: Vec<String>,
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceBinding>,
}
impl std::fmt::Debug for Delegation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Delegation")
            .field("device_id", &self.device_id)
            .field("scope", &self.scope)
            .field("workspace", &self.workspace)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Automatic takeover is published only after a successful mirror snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffPolicy {
    pub holder_id: String,
    pub epoch: u64,
    pub handoff_enabled: bool,
    pub offline_takeover: bool,
    pub has_agents: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerWake {
    pub worker_id: Option<String>,
    pub state: String,
    pub keeper_url: String,
}

impl WorkspaceConfigureAck {
    /// Verify the distinct scoped endpoint's bounded response before allowing
    /// project work. Callers must not fall back to legacy configure on failure.
    pub fn decode(
        status: u16,
        body: &[u8],
        expected: &WorkspaceBinding,
        root: &std::path::Path,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            status == 200 && body.len() <= 16 * 1024,
            "workspace configuration not acknowledged"
        );
        let value: Self = serde_json::from_slice(body)
            .map_err(|_| anyhow::anyhow!("invalid workspace configuration acknowledgment"))?;
        anyhow::ensure!(
            value.confirms(expected, root),
            "workspace configuration acknowledgment mismatch"
        );
        Ok(value)
    }

    /// An old daemon's 204/missing field never confirms scoped acceptance.
    pub fn confirms(&self, expected: &WorkspaceBinding, root: &std::path::Path) -> bool {
        self.workspace_authority == 1 && &self.workspace == expected && self.workspace_root == root
    }
}
