//! Passive logical-workspace placement. Viewing never requests execution.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementAvailability {
    Owned,
    Unowned,
    Expired,
    PrivacyDisabled,
    /// The owner (a cloud machine) is suspended but keeps ownership: still
    /// routable. Passive reads never wake it; a send or a permission answer
    /// carries wake intent and does. Its lease reads expired by design.
    Suspended,
    /// A newer availability; never routable by this client.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct WorkspacePlacement {
    pub workspace_id: String,
    pub holder_id: Option<String>,
    pub route_host_id: Option<String>,
    pub epoch: u64,
    pub policy_revision: u64,
    pub availability: PlacementAvailability,
    pub preferred_installation_id: Option<String>,
    pub checkpoint_id: Option<String>,
    pub server_now: String,
    pub expires_at: Option<String>,
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}
impl WorkspacePlacement {
    /// The account's answer for a project it has no ownership record for
    /// (404 `workspace_not_found`): nobody executes it, epoch 0.
    pub(crate) fn unowned(workspace: &str) -> Self {
        Self {
            workspace_id: workspace.into(),
            holder_id: None,
            route_host_id: None,
            epoch: 0,
            policy_revision: 0,
            availability: PlacementAvailability::Unowned,
            preferred_installation_id: None,
            checkpoint_id: None,
            server_now: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            expires_at: None,
        }
    }
    /// Whether the account routes this project to its owner: an owned lease,
    /// or an owner that is suspended but keeps ownership.
    pub fn routable(&self) -> bool {
        matches!(
            self.availability,
            PlacementAvailability::Owned | PlacementAvailability::Suspended
        )
    }
    pub fn validate(&self, workspace: &str) -> Result<()> {
        ensure!(
            valid_id(workspace) && self.workspace_id == workspace,
            "workspace placement identity mismatch"
        );
        for id in [
            self.holder_id.as_deref(),
            self.route_host_id.as_deref(),
            self.preferred_installation_id.as_deref(),
            self.checkpoint_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            ensure!(valid_id(id), "invalid workspace placement identity");
        }
        ensure!(
            !self.server_now.is_empty() && self.server_now.len() <= 64,
            "invalid workspace placement clock"
        );
        let now = time::OffsetDateTime::parse(
            &self.server_now,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| anyhow::anyhow!("invalid workspace placement clock"))?;
        if self.availability == PlacementAvailability::Owned {
            let holder = self
                .holder_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("workspace owner missing"))?;
            // A live lease whose holder has no route (a revoked device, a
            // removed cloud machine) is a real state: owned but unreachable
            // until the lease lapses. Callers treat it as not routable now.
            if let Some(route) = self.route_host_id.as_deref() {
                ensure!(
                    route.strip_prefix("worker-") == Some(holder)
                        || route.strip_prefix("device-") == Some(holder),
                    "workspace route does not match owner"
                );
            }
            let expiry = self
                .expires_at
                .as_deref()
                .filter(|value| value.len() <= 64)
                .context("workspace lease missing")?;
            let expiry =
                time::OffsetDateTime::parse(expiry, &time::format_description::well_known::Rfc3339)
                    .map_err(|_| anyhow::anyhow!("invalid workspace lease clock"))?;
            ensure!(expiry > now, "workspace lease expired");
            ensure!(
                self.epoch > 0
                    && self
                        .expires_at
                        .as_ref()
                        .is_some_and(|value| !value.is_empty() && value.len() <= 64),
                "workspace lease missing"
            );
        } else if self.availability == PlacementAvailability::Suspended {
            // A paused owner: its lease lapsed by design, its route stays.
            let holder = self
                .holder_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("workspace owner missing"))?;
            if let Some(route) = self.route_host_id.as_deref() {
                ensure!(
                    route.strip_prefix("worker-") == Some(holder)
                        || route.strip_prefix("device-") == Some(holder),
                    "workspace route does not match owner"
                );
            }
            ensure!(self.epoch > 0, "workspace lease missing");
        } else {
            ensure!(
                self.route_host_id.is_none(),
                "inactive workspace must not have an execution route"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct InstallationIdentity {
    pub installation_id: String,
    pub installation_proof: String,
}
impl std::fmt::Debug for InstallationIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallationIdentity")
            .field("installation_id", &self.installation_id)
            .finish_non_exhaustive()
    }
}
impl InstallationIdentity {
    pub fn generate() -> Self {
        use base64::Engine;
        let proof = rand::random::<[u8; 32]>();
        let id = rand::random::<[u8; 16]>();
        Self {
            installation_id: format!(
                "i-{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id)
            ),
            installation_proof: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(proof),
        }
    }
    pub fn validate(&self) -> Result<()> {
        use base64::Engine;
        ensure!(
            valid_id(&self.installation_id) && self.installation_id.starts_with("i-"),
            "invalid installation identity"
        );
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&self.installation_proof)
            .map_err(|_| anyhow::anyhow!("invalid installation proof"))?;
        ensure!(
            bytes.len() == 32
                && base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes)
                    == self.installation_proof,
            "invalid installation proof"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct InstallationBinding {
    pub installation_id: String,
    pub device_id: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct WorkspaceHome {
    pub workspace_id: String,
    pub preferred_installation_id: String,
    pub policy_revision: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owned() -> WorkspacePlacement {
        serde_json::from_value(serde_json::json!({"workspace_id":"w-project","holder_id":"d-home","route_host_id":"device-d-home","epoch":4,"policy_revision":2,"availability":"owned","preferred_installation_id":"i-home","checkpoint_id":"c-one","server_now":"2026-09-28T19:00:00Z","expires_at":"2026-09-28T19:01:30Z"})).unwrap()
    }
    #[test]
    fn placement_binds_exact_workspace_and_typed_owner() {
        let mut value = owned();
        value.validate("w-project").unwrap();
        assert!(value.validate("w-other").is_err());
        value.route_host_id = Some("worker-wrong".into());
        assert!(value.validate("w-project").is_err());
        value.route_host_id = Some("prefix-device-d-home".into());
        assert!(value.validate("w-project").is_err());
        value.route_host_id = Some("worker-d-home".into());
        value.validate("w-project").unwrap();
        // Owned by a holder the account can no longer route to.
        value.route_host_id = None;
        value.validate("w-project").unwrap();
        value.route_host_id = Some("worker-d-home".into());
        value.availability = PlacementAvailability::Expired;
        assert!(value.validate("w-project").is_err());
        value.route_host_id = None;
        value.validate("w-project").unwrap();
    }
    /// A suspended owner keeps its route although its lease reads expired.
    #[test]
    fn a_suspended_owner_stays_routable_with_a_lapsed_lease() {
        let value: WorkspacePlacement = serde_json::from_value(serde_json::json!({"workspace_id":"w-project","holder_id":"cloud-1","route_host_id":"worker-cloud-1","epoch":4,"policy_revision":2,"availability":"suspended","preferred_installation_id":"i-home","checkpoint_id":"c-one","server_now":"2026-09-28T19:30:00Z","expires_at":"2026-09-28T19:01:30Z"})).unwrap();
        assert_eq!(value.availability, PlacementAvailability::Suspended);
        value.validate("w-project").unwrap();
        assert!(value.routable());
        let mut wrong = value.clone();
        wrong.route_host_id = Some("worker-other".into());
        assert!(wrong.validate("w-project").is_err());
        let mut ownerless = value.clone();
        ownerless.holder_id = None;
        assert!(ownerless.validate("w-project").is_err());
        let mut newer = value;
        newer.availability = PlacementAvailability::Unknown;
        assert!(!newer.routable());
    }
    #[test]
    fn installation_proof_is_canonical_bounded_and_not_debuggable() {
        let value = InstallationIdentity::generate();
        value.validate().unwrap();
        assert!(!format!("{value:?}").contains(&value.installation_proof));
        let mut invalid = value.clone();
        invalid.installation_proof.push('=');
        assert!(invalid.validate().is_err());
        invalid.installation_proof = "AA".into();
        assert!(invalid.validate().is_err());
        invalid.installation_id = "i-../other".into();
        assert!(invalid.validate().is_err());
    }
}

/// A capability as the service advertises it. Decoded leniently (a service
/// may add fields), then compared exactly against what this client
/// implements; the daemon acknowledgment itself stays exact.
#[derive(Clone, Debug, Deserialize)]
pub struct AdvertisedCapability {
    pub version: u16,
    pub boundary: String,
    pub expired_takeover: bool,
}
impl AdvertisedCapability {
    fn exact(&self) -> Option<crate::ExecutionCapability> {
        let capability = crate::ExecutionCapability {
            version: self.version,
            boundary: self.boundary.clone(),
            expired_takeover: self.expired_takeover,
        };
        capability.supported().then_some(capability)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExecutionCapabilities {
    pub execution_authority: u16,
    pub execution_capability: AdvertisedCapability,
    /// Every capability the service accepts; older services omit it.
    #[serde(default)]
    pub supported_execution_capabilities: Vec<AdvertisedCapability>,
    pub installation_binding: u16,
    pub workspace_placement: u16,
    pub checkpoint_receipts: u16,
}
impl ExecutionCapabilities {
    /// The service's default when this client implements it; otherwise this
    /// client's preferred capability among those the service also accepts
    /// (automatic recovery first). `None`: nothing in common.
    pub fn selected(&self) -> Option<crate::ExecutionCapability> {
        self.execution_capability.exact().or_else(|| {
            let offered: Vec<_> = self
                .supported_execution_capabilities
                .iter()
                .take(16)
                .filter_map(AdvertisedCapability::exact)
                .collect();
            [
                crate::ExecutionCapability::checkpoint_fork(),
                crate::ExecutionCapability::managed(),
            ]
            .into_iter()
            .find(|preferred| offered.contains(preferred))
        })
    }
    pub fn supported(&self) -> bool {
        self.execution_authority == 2
            && self.selected().is_some()
            && self.installation_binding == 1
            && self.workspace_placement == 2
            && self.checkpoint_receipts == 1
    }
}
#[derive(Debug)]
pub struct CleanReleaseRequired;
impl std::fmt::Display for CleanReleaseRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("clean_release_required")
    }
}
impl std::error::Error for CleanReleaseRequired {}
