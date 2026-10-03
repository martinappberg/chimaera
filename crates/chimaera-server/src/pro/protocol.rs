//! The daemon consumes the documented wire without linking the TLS app crate.
use serde::{Deserialize, Serialize};

/// An immutable project restriction, not a requested expansion of authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceBinding {
    pub workspace_id: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceConfigureAck {
    pub workspace_authority: u16,
    pub workspace: WorkspaceBinding,
    pub workspace_root: std::path::PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Delegation {
    pub access_token: String,
    pub expires_at: String,
    pub scope: Vec<String>,
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceBinding>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct Configure {
    /// Internal-only recovery transport; HTTP Configure cannot enable it.
    #[serde(skip)]
    pub recovery: bool,
    #[serde(default)]
    pub execution: Option<super::execution::wire::ExecutionConfiguration>,
    #[serde(default)]
    pub account_id: Option<String>,
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
    pub continuity: Option<super::execution::wire::Continuity>,
    #[serde(default)]
    pub execution_capability: Option<super::execution::wire::ExecutionCapability>,
    #[serde(default)]
    pub execution_lease: Option<super::execution::wire::ExecutionLease>,
    #[serde(default)]
    pub checkpoint: Option<super::execution::wire::Checkpoint>,
    #[serde(default)]
    pub mirror_disabled: bool,
    pub workspace_id: String,
    pub holder_id: Option<String>,
    pub epoch: u64,
    pub expires_at: Option<String>,
    pub server_now: String,
    pub requires_fork: bool,
    /// Additive: the account asks the work to move to this holder, because
    /// the user acted on that computer (or a phone acted while the cloud
    /// slept and this computer can take it). The holder yields at its next
    /// pause; the named computer takes it. Absent from older accounts.
    #[serde(default)]
    pub move_to: Option<String>,
    /// Additive: when the account recorded that request, by its own clock.
    #[serde(default)]
    pub move_requested_at: Option<String>,
    /// Additive: `computer` (the user acted on another computer) or `phone`
    /// (a phone acted while the cloud slept).
    #[serde(default)]
    pub move_reason: Option<String>,
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
    #[serde(default)]
    pub alias: String,
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

#[derive(Deserialize)]
pub(crate) struct WorkspaceConfigure {
    #[serde(flatten)]
    pub config: Configure,
    pub workspace_root: std::path::PathBuf,
}

/// Keeper transport addresses workers by a prefixed host ID; account ownership
/// uses the raw registered worker ID. Never apply this to device/SSH rows.
pub(super) fn worker_holder_id(host_id: &str) -> Option<&str> {
    if !super::valid_id(host_id) {
        return None;
    }
    host_id
        .strip_prefix("worker-")
        .filter(|holder| super::valid_id(holder))
}
impl Host {
    pub(super) fn worker_holder(&self) -> Option<&str> {
        (self.kind == "worker")
            .then(|| worker_holder_id(&self.id))
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_identity_translation_is_typed_exact_and_bounded() {
        let mut host: Host = serde_json::from_value(
            serde_json::json!({"id":"worker-873107b04056d8","kind":"worker","status":"connected"}),
        )
        .unwrap();
        assert_eq!(host.worker_holder(), Some("873107b04056d8"));
        for kind in ["device", "ssh", "future"] {
            host.kind = kind.into();
            assert!(host.worker_holder().is_none());
        }
        for id in [
            "",
            "worker-",
            "873107b04056d8",
            "prefix-worker-873107b04056d8",
            "worker-873107b04056d8/other",
        ] {
            assert!(worker_holder_id(id).is_none());
        }
        assert!(worker_holder_id(&format!("worker-{}", "x".repeat(128))).is_none());
        assert_ne!(
            worker_holder_id("worker-worker-873107b04056d8"),
            Some("873107b04056d8")
        );
    }
}
