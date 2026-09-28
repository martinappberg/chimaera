//! Explicit v2 managed-execution contract. Unsupported takeover is never inferred
//! from a version label or from a successful legacy configure response.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionCapability {
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
    pub fn supported(&self) -> bool {
        self == &Self::managed()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfiguration {
    pub version: u16,
    pub installation_id: Option<String>,
    pub capability: ExecutionCapability,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfigureAck {
    pub execution_authority: u16,
    pub execution: ExecutionConfiguration,
    pub workspace_configuration: Option<crate::WorkspaceConfigureAck>,
}
impl ExecutionConfigureAck {
    pub fn decode(
        status: u16,
        body: &[u8],
        expected: &ExecutionConfiguration,
        workspace: Option<&crate::WorkspaceConfigureAck>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            status == 200 && body.len() <= 16 * 1024,
            "execution authority not acknowledged"
        );
        let value: Self = serde_json::from_slice(body)
            .map_err(|_| anyhow::anyhow!("invalid execution authority acknowledgment"))?;
        anyhow::ensure!(
            value.execution_authority == 1
                && value.execution == *expected
                && expected.version == 1
                && expected.capability.supported()
                && value.workspace_configuration.as_ref() == workspace,
            "execution authority acknowledgment mismatch"
        );
        Ok(value)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Continuity {
    pub version: u16,
    pub mode: String,
    pub policy_revision: u64,
    pub preferred_installation_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLease {
    pub id: String,
    pub sequence: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Continuation {
    Idle,
    Interrupted,
    #[default]
    Uncertain,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
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
pub struct ExecutionRecoveryGrant {
    pub access_token: String,
    pub expires_at: String,
    pub scope: Vec<String>,
    pub workspace_id: String,
    pub holder_id: String,
    pub epoch: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRecoveryRequest {
    pub endpoint: String,
    pub account_id: String,
    pub installation_id: String,
    pub recovery: ExecutionRecoveryGrant,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRecoveryAck {
    pub execution_recovery: u16,
    pub workspace_id: String,
    pub holder_id: String,
    pub epoch: u64,
    pub released: bool,
}
impl ExecutionRecoveryAck {
    pub fn decode(
        status: u16,
        body: &[u8],
        grant: &ExecutionRecoveryGrant,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            status == 200 && body.len() <= 16 * 1024,
            "execution recovery not acknowledged"
        );
        let value: Self = serde_json::from_slice(body)
            .map_err(|_| anyhow::anyhow!("invalid execution recovery acknowledgment"))?;
        anyhow::ensure!(
            value.execution_recovery == 1
                && value.released
                && value.workspace_id == grant.workspace_id
                && value.holder_id == grant.holder_id
                && value.epoch == grant.epoch,
            "execution recovery acknowledgment mismatch"
        );
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn execution_ack_requires_exact_negotiation_and_has_no_legacy_success_path() {
        let execution = ExecutionConfiguration {
            version: 1,
            installation_id: Some("i-fixture".into()),
            capability: ExecutionCapability::managed(),
        };
        let ack = ExecutionConfigureAck {
            execution_authority: 1,
            execution: execution.clone(),
            workspace_configuration: None,
        };
        let body = serde_json::to_vec(&ack).unwrap();
        assert!(ExecutionConfigureAck::decode(200, &body, &execution, None).is_ok());
        for status in [204, 404, 401, 500] {
            assert!(ExecutionConfigureAck::decode(status, &body, &execution, None).is_err());
        }
        for body in [b"{}".as_slice(), b"<html>legacy fallback</html>", b""] {
            assert!(ExecutionConfigureAck::decode(200, body, &execution, None).is_err());
        }
        let mut widened = execution.clone();
        widened.capability.expired_takeover = true;
        assert!(!widened.capability.supported());
        assert!(ExecutionConfigureAck::decode(200, &body, &widened, None).is_err());
        let mut other = execution.clone();
        other.installation_id = Some("i-other".into());
        assert!(ExecutionConfigureAck::decode(200, &body, &other, None).is_err());
        assert!(ExecutionConfigureAck::decode(200, &vec![b' '; 16385], &execution, None).is_err());
    }
    #[test]
    fn recovery_completion_is_exact_and_never_inferred_from_no_content() {
        let grant = ExecutionRecoveryGrant {
            access_token: "synthetic".into(),
            expires_at: "2026-09-28T00:05:00Z".into(),
            scope: vec!["mirror".into(), "release".into()],
            workspace_id: "w-a".into(),
            holder_id: "d-old".into(),
            epoch: 7,
        };
        let ack = ExecutionRecoveryAck {
            execution_recovery: 1,
            workspace_id: grant.workspace_id.clone(),
            holder_id: grant.holder_id.clone(),
            epoch: grant.epoch,
            released: true,
        };
        let body = serde_json::to_vec(&ack).unwrap();
        assert!(ExecutionRecoveryAck::decode(200, &body, &grant).is_ok());
        assert!(ExecutionRecoveryAck::decode(204, b"", &grant).is_err());
        let mut changed = ack;
        changed.epoch += 1;
        assert!(
            ExecutionRecoveryAck::decode(200, &serde_json::to_vec(&changed).unwrap(), &grant)
                .is_err()
        );
    }
}
