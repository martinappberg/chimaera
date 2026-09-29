//! Explicit v2 managed-execution contract. Unsupported takeover is never inferred
//! from a version label or from a successful legacy configure response.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionCapability {
    pub version: u16,
    pub boundary: String,
    pub expired_takeover: bool,
}
impl ExecutionCapability {
    pub fn managed() -> Self {
        Self {
            version: 1,
            boundary: "managed_processes".into(),
            expired_takeover: false,
        }
    }
    /// Automatic recovery owns canonical checkpoints, not arbitrary physical
    /// process side effects on a disconnected host.
    pub fn checkpoint_fork() -> Self {
        Self {
            version: 2,
            boundary: "canonical_checkpoint".into(),
            expired_takeover: true,
        }
    }
    pub fn supported(&self) -> bool {
        self == &Self::managed() || self == &Self::checkpoint_fork()
    }
    pub fn policy_mode(&self) -> Option<&'static str> {
        if self == &Self::managed() {
            Some("managed_v1")
        } else if self == &Self::checkpoint_fork() {
            Some("checkpoint_fork_v1")
        } else {
            None
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionConfiguration {
    pub version: u16,
    pub installation_id: Option<String>,
    pub capability: ExecutionCapability,
}
// Account responses below accept additive fields, matching the link crate
// (PROTOCOL.md): a newer service must not break an older daemon. The
// capability above and the local configure/recovery/identity records stay
// exact: an unknown field there means a different contract, not a newer one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Continuity {
    pub version: u16,
    pub mode: String,
    pub policy_revision: u64,
    pub preferred_installation_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExecutionLease {
    pub id: String,
    pub sequence: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Continuation {
    Idle,
    Interrupted,
    /// Unknown evidence is uncertain, never a blind replay.
    #[default]
    #[serde(other)]
    Uncertain,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub id: String,
    pub sequence: u64,
    pub source_holder_id: String,
    pub source_epoch: u64,
    pub working_tree_oid: String,
    pub config_oid: String,
    pub handoff_oid: String,
    pub continuation: Continuation,
}

/// A short-lived one-use recovery credential. It can only publish a stopped
/// snapshot and release one old grant; it is never a daemon Configure token.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionRecoveryGrant {
    pub access_token: String,
    pub expires_at: String,
    pub scope: Vec<String>,
    pub workspace_id: String,
    pub holder_id: String,
    pub epoch: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionRecoveryRequest {
    pub endpoint: String,
    pub account_id: String,
    pub installation_id: String,
    pub recovery: ExecutionRecoveryGrant,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionRecoveryAck {
    pub execution_recovery: u16,
    pub workspace_id: String,
    pub holder_id: String,
    pub epoch: u64,
    pub released: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::pro) struct Identity {
    pub endpoint: String,
    pub account_id: String,
    pub installation_id: Option<String>,
    pub holder_id: String,
    pub epoch: u64,
}
