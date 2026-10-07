//! Authenticated fixed provider routes; optional private policy owns its one cache and attempts.
pub(crate) mod host;
pub(crate) mod process;
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
pub(crate) use host::{definition, ProviderSlot, ProviderState, ProviderStatus};
use host::{Operation, WorkerProviders, PROVIDERS};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
pub(crate) async fn bounded_process_output(
    command: &mut tokio::process::Command,
) -> Result<(bool, Vec<u8>), &'static str> {
    let out = process::output(command).await?;
    Ok((out.success, out.stdout))
}
pub(crate) fn active_operations() -> usize {
    host::active()
}
fn controller(state: &Arc<AppState>) -> Option<&Arc<dyn WorkerProviders>> {
    state.pro().cloud_providers.admit(state)
}
pub(crate) fn cached_observations(state: &AppState) -> Vec<ProviderStatus> {
    state.pro().cloud_providers.current().map_or_else(
        || {
            PROVIDERS
                .iter()
                .map(|p| ProviderStatus::new(p.id))
                .collect()
        },
        |p| p.cached_observations(),
    )
}
pub(crate) async fn readiness(
    state: &Arc<AppState>,
    ids: &[String],
    fresh: bool,
) -> Vec<ProviderStatus> {
    if ids.len() > 16 || ids.iter().any(|id| id.len() > 64) {
        return vec![ProviderStatus::new("unsupported-provider").failed("provider_limit")];
    }
    if let Some(owner) = controller(state) {
        owner.readiness(ids, fresh).await
    } else {
        ids.iter()
            .map(|id| ProviderStatus::new(id).failed("unavailable"))
            .collect()
    }
}
fn unavailable() -> Response {
    Json(json!({"available":false,"providers":[],"handoffs":[],"connection":null})).into_response()
}
pub(crate) async fn list(State(state): State<Arc<AppState>>) -> Response {
    if !super::enabled() {
        return unavailable();
    }
    if controller(&state).is_none() {
        return unavailable();
    }
    let ids = PROVIDERS
        .iter()
        .map(|d| d.id.to_string())
        .collect::<Vec<_>>();
    let mut result = json!({"available":true,"providers":readiness(&state,&ids,false).await,"handoffs":crate::pro::cloud_provider_blocks(&state)});
    // Reopening the UI or losing the mutation's HTTP response must not strand a
    // running logout with no job ID. This is an observation, never a new action.
    if let Some(connection) = state
        .pro()
        .cloud_providers
        .current()
        .and_then(|p| p.pending_disconnect())
    {
        result["connection"] = json!(connection);
    }
    Json(result).into_response()
}
pub(crate) async fn start(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    if !super::enabled() {
        return unavailable();
    }
    let Some(def) = definition(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"unsupported_provider"})),
        )
            .into_response();
    };
    let Some(owner) = controller(&state) else {
        return unavailable();
    };
    match owner.start(def.provider, Operation::Connect) {
        Ok(connection) => Json(json!({"available":true,"connection":connection})).into_response(),
        Err(error) => (StatusCode::CONFLICT, Json(json!({"error":error}))).into_response(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Disconnect {
    acknowledge_cloud_work: bool,
}
pub(crate) async fn disconnect(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(input): Json<Disconnect>,
) -> Response {
    if !super::enabled() {
        return unavailable();
    }
    if !input.acknowledge_cloud_work {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"disconnect_confirmation_required"})),
        )
            .into_response();
    }
    let Some(def) = definition(&id).filter(|def| matches!(def.id, "claude" | "codex" | "github"))
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"unsupported_provider"})),
        )
            .into_response();
    };
    let Some(owner) = controller(&state) else {
        return unavailable();
    };
    match owner.start(def.provider, Operation::Disconnect) {
        Ok(connection) => Json(json!({"available":true,"connection":connection})).into_response(),
        Err(error) => (StatusCode::CONFLICT, Json(json!({"error":error}))).into_response(),
    }
}
pub(crate) async fn get(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    connection_response(&state, &id, false).await
}
#[derive(serde::Deserialize)]
pub(crate) struct Input {
    code: String,
}
pub(crate) async fn submit(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(input): Json<Input>,
) -> Response {
    if !super::enabled() {
        return unavailable();
    }
    let Some(owner) = state.pro().cloud_providers.current() else {
        return unavailable();
    };
    match owner.submit(&id, input.code) {
        Ok(connection) => Json(json!({"available":true,"connection":connection})).into_response(),
        Err("connection_expired") => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"connection_expired"})),
        )
            .into_response(),
        Err(code) => (StatusCode::CONFLICT, Json(json!({"error":code}))).into_response(),
    }
}
pub(crate) async fn cancel(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    connection_response(&state, &id, true).await
}
async fn connection_response(state: &Arc<AppState>, id: &str, cancel: bool) -> Response {
    if !super::enabled() {
        return unavailable();
    }
    let Some(owner) = state.pro().cloud_providers.current() else {
        return unavailable();
    };
    let connection = if cancel {
        owner.cancel(id).await.ok()
    } else {
        owner.observe(id)
    };
    match connection {
        Some(connection) => Json(json!({"available":true,"connection":connection})).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"connection_expired"})),
        )
            .into_response(),
    }
}
