//! Cloud handoff uses each deferred agent's provider, never a generic login flag.
use super::Ownership;
use crate::{
    cloud::providers::{ProviderState, ProviderStatus},
    lock, AppState,
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct BlockedProvider {
    pub id: String,
    pub state: ProviderState,
    pub reason: Option<String>,
}

#[derive(Debug)]
pub(super) struct Blocked(pub Vec<BlockedProvider>);
impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cloud_provider_not_ready")
    }
}
impl std::error::Error for Blocked {}

pub(super) fn restored(mut blocks: Vec<BlockedProvider>) -> Vec<BlockedProvider> {
    blocks.truncate(16);
    blocks.retain(|block| super::valid_id(&block.id) && block.state != ProviderState::SignedIn);
    for block in &mut blocks {
        block.reason = block.reason.take().filter(|reason| {
            reason.len() <= 64 && reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        });
    }
    blocks
}

pub(super) fn required(state: &AppState, workspace: &str) -> Vec<String> {
    let mut ids: Vec<_> = lock(&state.deferred_sessions)
        .values()
        .filter(|entry| entry.workspace_id == workspace)
        .filter_map(|entry| entry.agent.as_ref().map(|agent| agent.kind.as_str().into()))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

pub(super) fn blocked(required: &[String], statuses: &[ProviderStatus]) -> Vec<BlockedProvider> {
    required
        .iter()
        .filter_map(|id| {
            if crate::cloud::providers::definition(id).is_none() {
                return Some(BlockedProvider {
                    id: id.clone(),
                    state: ProviderState::Unknown,
                    reason: Some("unsupported_provider".into()),
                });
            }
            let status = statuses.iter().find(|status| status.id == *id);
            if status.is_some_and(|status| {
                status.state == ProviderState::SignedIn && status.installed == Some(true)
            }) {
                return None;
            }
            Some(BlockedProvider {
                id: id.clone(),
                state: status.map_or(ProviderState::Unknown, |status| {
                    if status.state == ProviderState::SignedIn {
                        ProviderState::Unknown
                    } else {
                        status.state
                    }
                }),
                reason: status
                    .and_then(|status| status.reason.clone())
                    .or_else(|| Some("readiness_unconfirmed".into())),
            })
        })
        .collect()
}

pub(super) async fn check(
    state: &Arc<AppState>,
    workspace: &str,
    fresh: bool,
) -> Vec<BlockedProvider> {
    if !super::is_worker(state) {
        return Vec::new();
    }
    let ids = required(state, workspace);
    if ids.is_empty() {
        return Vec::new();
    }
    match tokio::time::timeout(
        Duration::from_secs(35),
        crate::cloud::providers::readiness(state, &ids, fresh),
    )
    .await
    {
        Ok(statuses) => blocked(&ids, &statuses),
        Err(_) => ids
            .into_iter()
            .map(|id| BlockedProvider {
                id,
                state: ProviderState::Unknown,
                reason: Some("probe_timeout".into()),
            })
            .collect(),
    }
}

pub(super) fn record(
    state: &AppState,
    workspace: &str,
    blocked: Vec<BlockedProvider>,
) -> anyhow::Result<()> {
    let mut statuses = lock(&state.pro.status);
    let status = statuses.entry(workspace.into()).or_default();
    status.blocked_providers = blocked.clone();
    if blocked.is_empty() {
        if status.error.as_deref() == Some("cloud_provider_not_ready") {
            status.error = None;
            status.error_code = None;
        }
        Ok(())
    } else {
        status.error = Some("cloud_provider_not_ready".into());
        status.error_code = Some("cloud_provider_not_ready");
        state.changes.notify_waiters();
        Err(Blocked(blocked).into())
    }
}

/// The authenticated provider page may offer retry only for a staged cloud
/// workspace. No cached UI state can grant ownership or launch its agents.
pub(crate) fn cloud_provider_blocks(state: &AppState) -> Vec<serde_json::Value> {
    if !super::is_worker(state) {
        return Vec::new();
    }
    let ownership = lock(&state.pro.ownership).clone();
    let statuses = lock(&state.pro.status).clone();
    lock(&state.workspaces)
        .list()
        .into_iter()
        .filter_map(|workspace| {
            let Ownership::SettingUp { epoch } = ownership.get(&workspace.id)? else {
                return None;
            };
            let status = statuses.get(&workspace.id)?;
            (!status.blocked_providers.is_empty()).then(|| {
                serde_json::json!({
                    "workspace_id": workspace.id,
                    "name": workspace.name,
                    "expected_epoch": epoch,
                    "blocked_providers": status.blocked_providers,
                })
            })
        })
        .take(128)
        .collect()
}

/// Session-scoped MCP context reads recorded attention only, without a probe.
pub(crate) fn workspace_provider_blocks(state: &AppState, workspace: &str) -> serde_json::Value {
    if !super::is_worker(state)
        || !matches!(
            lock(&state.pro.ownership).get(workspace),
            Some(Ownership::SettingUp { .. })
        )
    {
        return serde_json::json!([]);
    }
    let statuses = lock(&state.pro.status);
    serde_json::json!(statuses
        .get(workspace)
        .map(|status| status.blocked_providers.iter().take(16).collect::<Vec<_>>())
        .unwrap_or_default())
}
