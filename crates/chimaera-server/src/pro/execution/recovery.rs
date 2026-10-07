//! Recovery credentials never enter Configure or grant execution permission.
use super::{
    watchdog,
    wire::{
        ExecutionCapability, ExecutionConfiguration, ExecutionRecoveryAck, ExecutionRecoveryRequest,
    },
};
use crate::{
    lock,
    pro::{
        engine,
        protocol::{Configure, Delegation, Role},
        transport, Ownership,
    },
    AppState,
};
use anyhow::{ensure, Context, Result};
use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Json,
};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

pub(crate) async fn recover(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ExecutionRecoveryRequest>,
) -> Response {
    let result = tokio::time::timeout(Duration::from_secs(19 * 60), run(&state, request)).await;
    match result {
        Ok(Ok(ack)) => Json(ack).into_response(),
        Ok(Err(error)) => crate::pro::routes::failure(error),
        Err(_) => {
            crate::pro::routes::failure(anyhow::anyhow!("project recovery is still unconfirmed"))
        }
    }
}
async fn run(
    state: &Arc<AppState>,
    mut request: ExecutionRecoveryRequest,
) -> Result<ExecutionRecoveryAck> {
    request.endpoint = transport::endpoint(&request.endpoint)?;
    let grant = &request.recovery;
    ensure!(
        crate::pro::valid_id(&grant.workspace_id)
            && crate::pro::valid_id(&grant.holder_id)
            && grant.epoch > 0
            && grant.access_token.len() <= 8192
            && !grant.access_token.is_empty()
            && !grant.access_token.chars().any(char::is_control)
            && grant.scope.len() == 2
            && grant.scope.iter().any(|s| s == "mirror")
            && grant.scope.iter().any(|s| s == "release"),
        "invalid recovery authority"
    );
    crate::pro::authority::workspace(state, &grant.workspace_id)?;
    let _job = state
        .pro()
        .jobs
        .try_lock()
        .map_err(|_| anyhow::anyhow!("project transfer is active"))?;
    let generation;
    {
        let _configuration = state.pro().configuration.lock().await;
        generation = state.pro().generation.load(Ordering::Acquire);
        if let Some(runtime) = lock(&state.pro().runtime).as_ref() {
            ensure!(
                runtime.account_id.as_deref() == Some(&request.account_id)
                    && runtime.endpoint == request.endpoint,
                "account changed before recovery"
            );
        }
        let mut preferences = lock(&state.pro().preferences);
        let preference = preferences
            .get_mut(&grant.workspace_id)
            .context("managed project recovery identity unavailable")?;
        let identity = preference
            .execution_identity
            .as_ref()
            .context("managed project recovery identity unavailable")?;
        ensure!(
            preference.continuity.is_some()
                && identity.endpoint == request.endpoint
                && identity.account_id == request.account_id
                && identity.installation_id.as_deref() == Some(&request.installation_id)
                && identity.holder_id == grant.holder_id
                && identity.epoch == grant.epoch,
            "project recovery authority mismatch"
        );
        ensure!(
            lock(&state.workspaces).get(&grant.workspace_id).is_some(),
            "recovery requires existing local project"
        );
        preference.recovery_pending = true;
        preference.execution_uncertain = true;
    }
    // Close every new launch first, then signal. Cancellation leaves the durable
    // pending fence in place; it never re-enables the old execution lease.
    crate::pro::persist(state).await?;
    watchdog::stop(state, std::slice::from_ref(&grant.workspace_id)).await?;
    ensure!(
        generation == state.pro().generation.load(Ordering::Acquire),
        "account changed during recovery"
    );
    crate::pro::transition::set_ownership(
        state,
        &grant.workspace_id,
        Some(Ownership::Local { epoch: grant.epoch }),
        "recovery",
    );
    let config = Configure {
        recovery: true,
        account_id: Some(request.account_id),
        role: Role::Device,
        endpoint: request.endpoint,
        keeper_url: String::new(),
        hours_exhausted: false,
        alias: None,
        execution: Some(ExecutionConfiguration {
            version: 1,
            installation_id: Some(request.installation_id),
            capability: ExecutionCapability::managed(),
        }),
        delegation: Delegation {
            access_token: grant.access_token.clone(),
            expires_at: grant.expires_at.clone(),
            scope: grant.scope.clone(),
            device_id: grant.holder_id.clone(),
            workspace: None,
        },
    };
    engine::snapshot(state, &config, &grant.workspace_id, true).await?;
    ensure!(
        generation == state.pro().generation.load(Ordering::Acquire),
        "account changed during recovery"
    );
    crate::pro::transition::set_ownership(
        state,
        &grant.workspace_id,
        Some(Ownership::AwaitingVerification { epoch: grant.epoch }),
        "recovery",
    );
    lock(&state.pro().preferences)
        .get_mut(&grant.workspace_id)
        .context("recovery state unavailable")?
        .recovery_pending = false;
    crate::pro::persist(state).await?;
    Ok(ExecutionRecoveryAck {
        execution_recovery: 1,
        workspace_id: grant.workspace_id.clone(),
        holder_id: grant.holder_id.clone(),
        epoch: grant.epoch,
        released: true,
    })
}
