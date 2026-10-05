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

/// On the cloud machine, tells the account what this readiness check found
/// for the agents it can run, so a computer whose app quits can keep work the
/// cloud could not continue (`GET /v2/cloud/agents`). Only definite answers
/// are reported; an unknown or unavailable check says nothing. Best effort and
/// off the caller's path.
pub(crate) fn report_cloud_agents(state: &AppState, statuses: &[ProviderStatus]) {
    if !super::is_worker(state) {
        return;
    }
    // The account answers this only with negotiated execution (continuity v2).
    let Some(config) = lock(&state.pro.runtime)
        .clone()
        .filter(|config| config.execution.is_some())
    else {
        return;
    };
    let agents: Vec<serde_json::Value> = statuses
        .iter()
        .filter(|status| matches!(status.id.as_str(), "claude" | "codex"))
        .filter_map(|status| {
            let signed_in = match status.state {
                ProviderState::SignedIn => status.installed == Some(true),
                ProviderState::NeedsSignIn | ProviderState::Missing => false,
                ProviderState::Unknown | ProviderState::Unavailable => return None,
            };
            Some(serde_json::json!({"id": status.id, "signed_in": signed_in}))
        })
        .take(2)
        .collect();
    if agents.is_empty() {
        return;
    }
    // One write per change, not per readiness call (the provider list polls).
    static SENT: std::sync::Mutex<Option<Vec<serde_json::Value>>> = std::sync::Mutex::new(None);
    {
        let mut sent = lock(&SENT);
        if sent.as_ref() == Some(&agents) {
            return;
        }
        *sent = Some(agents.clone());
    }
    tokio::spawn(async move {
        let body = serde_json::json!({ "agents": agents });
        let sent = super::engine::account(&config, "/v2/cloud/agents", "PUT", Some(&body));
        if !matches!(
            tokio::time::timeout(Duration::from_secs(10), sent).await,
            Ok(Ok(response)) if (200..300).contains(&response.status)
        ) {
            tracing::info!("cloud agent sign-ins not reported to the account");
            // Sent again on the next check instead of never.
            *lock(&SENT) = None;
        }
    });
}

/// Records which providers still hold sessions back. A blocked provider never
/// holds the project or other sessions: those resume, and these wait as
/// paused rows naming their provider.
pub(super) fn record(state: &AppState, workspace: &str, blocked: Vec<BlockedProvider>) {
    let mut statuses = lock(&state.pro.status);
    let status = statuses.entry(workspace.into()).or_default();
    let changed = status.blocked_providers.len() != blocked.len();
    status.blocked_providers = blocked;
    if status.blocked_providers.is_empty() {
        if status.error.as_deref() == Some("cloud_provider_not_ready") {
            status.error = None;
            status.error_code = None;
        }
    } else {
        status.error = Some("cloud_provider_not_ready".into());
        status.error_code = Some("cloud_provider_not_ready");
    }
    drop(statuses);
    if changed {
        state.changes.notify_waiters();
    }
}

/// Whether a deferred session waits for a provider that is not ready yet.
pub(super) fn waits_for_provider(
    entry: &crate::ledger::LedgerEntry,
    blocked: &[BlockedProvider],
) -> bool {
    entry
        .agent
        .as_ref()
        .is_some_and(|agent| blocked.iter().any(|block| block.id == agent.kind.as_str()))
}

/// The provider a paused row waits for, for the session list (additive
/// `blocked_provider`), so the page can name what to connect.
pub(crate) fn blocking_provider(
    state: &AppState,
    entry: &crate::ledger::LedgerEntry,
) -> Option<String> {
    let statuses = lock(&state.pro.status);
    let blocked = &statuses.get(&entry.workspace_id)?.blocked_providers;
    waits_for_provider(entry, blocked)
        .then(|| {
            entry
                .agent
                .as_ref()
                .map(|agent| agent.kind.as_str().to_owned())
        })
        .flatten()
}

/// After a provider signed in: check again (fresh) and resume the sessions
/// whose provider is ready. Errs with the providers still missing.
pub(super) async fn resume_ready(state: &Arc<AppState>, workspace: &str) -> anyhow::Result<()> {
    let blocked = check(state, workspace, true).await;
    record(state, workspace, blocked.clone());
    super::persist(state).await?;
    crate::ledger::resume_deferred_filtered(state, workspace, |entry| {
        !waits_for_provider(entry, &blocked)
    })
    .await?;
    if blocked.is_empty() {
        Ok(())
    } else {
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
            // A project waits in setup, or runs here with some sessions
            // still waiting for their provider.
            let (Ownership::SettingUp { epoch } | Ownership::Local { epoch }) =
                ownership.get(&workspace.id)?
            else {
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
            Some(Ownership::SettingUp { .. } | Ownership::Local { .. })
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
