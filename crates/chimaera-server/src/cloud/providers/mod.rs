//! Worker provider readiness and explicit, short-lived authentication jobs.
//! IDs are catalog keys, never executable names supplied by a client.
#[cfg(feature = "provider-authority-prototype")]
pub mod authority;
mod connect;
#[cfg(feature = "provider-authority-prototype")]
mod control_login;
mod disconnect;
#[cfg(feature = "provider-authority-prototype")]
mod login_home;
mod process;
/// Bounded output for probes using their own environment, with the same
/// process-group cleanup as provider authentication subprocesses.
pub(crate) async fn bounded_process_output(
    command: &mut tokio::process::Command,
) -> Result<(bool, Vec<u8>), &'static str> {
    let output = process::output(command).await?;
    Ok((output.success, output.stdout))
}
#[cfg(test)]
mod tests;
pub(crate) fn active_operations() -> usize {
    connect::active()
}
use crate::{agents::AgentKind, AppState};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) struct ProviderDefinition {
    pub id: &'static str,
    pub methods: &'static [&'static str],
    pub(super) kind: Option<AgentKind>,
}
pub(crate) const PROVIDERS: &[ProviderDefinition] = &[
    ProviderDefinition {
        id: "claude",
        methods: &["browser_code"],
        kind: Some(AgentKind::Claude),
    },
    ProviderDefinition {
        id: "codex",
        methods: &["device_code"],
        kind: Some(AgentKind::Codex),
    },
    ProviderDefinition {
        id: "github",
        methods: &["device_code"],
        kind: None,
    },
];
pub(crate) fn definition(id: &str) -> Option<&'static ProviderDefinition> {
    PROVIDERS.iter().find(|p| p.id == id)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProviderState {
    Missing,
    NeedsSignIn,
    SignedIn,
    Unknown,
    Unavailable,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProviderStatus {
    pub id: String,
    pub label: String,
    pub category: String,
    pub installed: Option<bool>,
    pub state: ProviderState,
    pub reason: Option<String>,
    pub checked_at: Option<u64>,
    pub methods: Vec<String>,
    pub disconnect_supported: bool,
}
impl ProviderStatus {
    fn new(id: &str) -> Self {
        let def = definition(id);
        let catalog = chimaera_core::cloud_providers::provider_definition(id);
        Self {
            id: id.into(),
            label: catalog.map_or(id, |d| d.label.as_str()).into(),
            category: catalog.map_or("agent", |d| d.category.as_str()).into(),
            installed: None,
            state: ProviderState::Unknown,
            reason: None,
            checked_at: None,
            disconnect_supported: def.is_some() && disconnect::supported(id),
            methods: def.map_or_else(Vec::new, |d| {
                d.methods.iter().map(|m| (*m).into()).collect()
            }),
        }
    }
    fn failed(mut self, reason: &str) -> Self {
        self.reason = Some(reason.into());
        self
    }
}
#[derive(Default)]
pub(crate) struct Providers {
    cache: Mutex<HashMap<String, (Instant, ProviderStatus)>>,
    auth_epoch: AtomicU64,
    probe: tokio::sync::Mutex<()>,
    connections: Mutex<HashMap<String, Arc<connect::Attempt>>>,
}
pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(super) fn home(state: &AppState) -> PathBuf {
    state
        .claude_settings_path
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"))
}
pub(super) async fn binary(
    state: &AppState,
    def: &ProviderDefinition,
    fresh: bool,
) -> Result<PathBuf, &'static str> {
    if let Some(kind) = def.kind {
        // A previous missing binary must not remain missing forever after a
        // user installs it outside Chimaera. Successful detections already
        // stat-check their executable; only negative cache hits need refresh.
        let missing = crate::lock(&state.agent_bins)
            .get(&kind)
            .is_some_and(|hit| hit.path.is_err());
        return crate::launcher::detect(state, kind, fresh || missing)
            .await
            .path
            .map_err(|_| "not_installed");
    }
    // GitHub's CLI is found on the login shell's PATH, which a test cannot
    // point at a fake; tests register one per fixture home instead.
    #[cfg(test)]
    if let Some(bin) = tests::github_cli(state) {
        return Ok(bin);
    }
    let mut cmd = process::command(
        std::path::Path::new("/bin/sh"),
        &["-c", "command -v gh"],
        &home(state),
    );
    let output = process::output(&mut cmd).await?;
    if !output.success {
        return Err("not_installed");
    }
    let path = String::from_utf8(output.stdout).map_err(|_| "probe_failed")?;
    let path = PathBuf::from(path.trim());
    if !path.is_absolute() {
        return Err("probe_failed");
    }
    Ok(path)
}
fn auth_status(id: &str, output: &process::Output) -> Result<bool, &'static str> {
    match id {
        "claude" => {
            let value: Value =
                serde_json::from_slice(&output.stdout).map_err(|_| "invalid_status")?;
            match (value["loggedIn"].as_bool(), output.code) {
                (Some(true), Some(0)) => Ok(true),
                (Some(false), Some(1 | 0)) => Ok(false),
                _ => Err("invalid_status"),
            }
        }
        "github" => {
            if !output.success {
                return Err("invalid_status");
            }
            let value: Value =
                serde_json::from_slice(&output.stdout).map_err(|_| "invalid_status")?;
            let all = value["hosts"].as_object().ok_or("invalid_status")?;
            let Some(hosts) = all.get("github.com") else {
                return Ok(false);
            };
            let hosts = hosts.as_array().ok_or("invalid_status")?;
            if hosts.is_empty() {
                return Ok(false);
            }
            if hosts.iter().any(|h| {
                h["active"].as_bool() == Some(true) && h["state"].as_str() == Some("success")
            }) {
                Ok(true)
            } else {
                // Failed network/token verification does not establish logout.
                Err("invalid_status")
            }
        }
        _ => Err("unsupported_provider"),
    }
}
async fn probe(state: &Arc<AppState>, id: &str) -> ProviderStatus {
    let mut status = ProviderStatus::new(id);
    status.checked_at = Some(now());
    let Some(def) = definition(id) else {
        status.state = ProviderState::Unavailable;
        return status.failed("unsupported_provider");
    };
    let bin = match binary(state, def, false).await {
        Ok(bin) => bin,
        Err("not_installed") => {
            status.installed = Some(false);
            status.state = ProviderState::Missing;
            return status;
        }
        Err(reason) => return status.failed(reason),
    };
    status.installed = Some(true);
    let auth = if id == "codex" {
        match process::Rpc::open(&bin, &home(state)).await {
            Ok(mut rpc) => match rpc
                .request("account/read", json!({"refreshToken":false}))
                .await
            {
                Ok(value) => match value.get("account") {
                    Some(Value::Null) if value["requiresOpenaiAuth"].as_bool() == Some(true) => {
                        Ok(false)
                    }
                    Some(account)
                        if matches!(account["type"].as_str(), Some("chatgpt" | "apiKey")) =>
                    {
                        Ok(true)
                    }
                    _ => Err("external_auth_unverified"),
                },
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        }
    } else {
        let args: &[&str] = if id == "claude" {
            &["auth", "status", "--json"]
        } else {
            &[
                "auth",
                "status",
                "--active",
                "--hostname",
                "github.com",
                "--json",
                "hosts",
            ]
        };
        match process::output(&mut process::command(&bin, args, &home(state))).await {
            Ok(output) => auth_status(id, &output),
            Err(e) => Err(e),
        }
    };
    match auth {
        Ok(true) => status.state = ProviderState::SignedIn,
        Ok(false) => status.state = ProviderState::NeedsSignIn,
        Err(e) => status.reason = Some(e.into()),
    }
    status
}
/// Prompt context must never turn a read into authentication or a CLI probe.
/// Expired, absent, concurrently changed and disconnecting states are unknown.
pub(crate) fn cached_observations(state: &AppState) -> Vec<ProviderStatus> {
    let epoch = state.cloud_providers.auth_epoch.load(Ordering::Acquire);
    let now = Instant::now();
    let cache = crate::lock(&state.cloud_providers.cache);
    let mut result: Vec<_> = PROVIDERS
        .iter()
        .map(|definition| {
            let mut observation = ProviderStatus::new(definition.id);
            if let Some((_, status)) = cache
                .get(definition.id)
                .filter(|(at, _)| *at <= now && now.duration_since(*at) < Duration::from_secs(30))
            {
                observation.installed = status.installed;
                observation.state =
                    if status.state == ProviderState::SignedIn && status.installed != Some(true) {
                        ProviderState::Unknown
                    } else {
                        status.state
                    };
                observation.checked_at = status.checked_at;
            }
            observation
        })
        .collect();
    drop(cache);
    for observation in &mut result {
        if state.cloud_providers.auth_epoch.load(Ordering::Acquire) != epoch
            || connect::disconnecting(state, &observation.id)
        {
            *observation = ProviderStatus::new(&observation.id);
        }
    }
    result
}

/// Shared by onboarding and staged handoff. This NEVER installs or signs in.
/// Unknown future IDs fail closed; they are not treated as shell sessions.
pub(crate) async fn readiness(
    state: &Arc<AppState>,
    ids: &[String],
    fresh: bool,
) -> Vec<ProviderStatus> {
    // Internal callers use session-provider IDs, never arbitrary executable names.
    // A malformed/oversized request must still fail closed, not become an empty
    // list that a caller could mistake for "all requirements satisfied".
    if ids.len() > 16 || ids.iter().any(|id| id.len() > 64) {
        return vec![ProviderStatus::new("unsupported-provider").failed("provider_limit")];
    }
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    let mut result = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    for id in ids {
        if connect::disconnecting(state, &id) {
            result.push(ProviderStatus::new(&id).failed("disconnecting"));
            continue;
        }
        let epoch = state.cloud_providers.auth_epoch.load(Ordering::Acquire);
        let requested = Instant::now();
        let Ok(_single) =
            tokio::time::timeout_at(deadline, state.cloud_providers.probe.lock()).await
        else {
            result.push(ProviderStatus::new(&id).failed("probe_timeout"));
            continue;
        };
        let cached = crate::lock(&state.cloud_providers.cache)
            .get(&id)
            .filter(|(at, _)| {
                at.elapsed() < Duration::from_secs(30) && (!fresh || *at >= requested)
            })
            .map(|(_, v)| v.clone());
        if let Some(cached) = cached {
            if state.cloud_providers.auth_epoch.load(Ordering::Acquire) == epoch
                && !connect::disconnecting(state, &id)
            {
                result.push(cached);
            } else {
                result.push(ProviderStatus::new(&id).failed("connection_changed"));
            }
            continue;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let status =
            tokio::time::timeout(remaining.min(Duration::from_secs(12)), probe(state, &id))
                .await
                .unwrap_or_else(|_| ProviderStatus::new(&id).failed("probe_timeout"));
        if state.cloud_providers.auth_epoch.load(Ordering::Acquire) != epoch
            || connect::disconnecting(state, &id)
        {
            result.push(ProviderStatus::new(&id).failed("connection_changed"));
            continue;
        }
        // Only catalog keys are cached: unknown IDs cannot grow daemon state.
        if definition(&id).is_some() {
            crate::lock(&state.cloud_providers.cache).insert(id, (Instant::now(), status.clone()));
        }
        result.push(status);
    }
    result
}
fn unavailable() -> Response {
    Json(json!({"available":false,"providers":[],"handoffs":[],"connection":null})).into_response()
}
pub(crate) async fn list(State(state): State<Arc<AppState>>) -> Response {
    if !super::enabled() {
        return unavailable();
    }
    let ids = PROVIDERS
        .iter()
        .map(|d| d.id.to_string())
        .collect::<Vec<_>>();
    let mut result = json!({"available":true,"providers":readiness(&state,&ids,false).await,"handoffs":crate::pro::cloud_provider_blocks(&state)});
    // Reopening the UI or losing the mutation's HTTP response must not strand a
    // running logout with no job ID. This is an observation, never a new action.
    if let Some(connection) = connect::pending_disconnect(&state) {
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
    match connect::start(state, def) {
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
    let Some(def) = definition(&id).filter(|def| disconnect::supported(def.id)) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"unsupported_provider"})),
        )
            .into_response();
    };
    match connect::start_disconnect(state, def) {
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
    let attempt = crate::lock(&state.cloud_providers.connections)
        .get(&id)
        .cloned();
    let Some(attempt) = attempt else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"connection_expired"})),
        )
            .into_response();
    };
    if let Err(code) = attempt.submit(input.code) {
        return (StatusCode::CONFLICT, Json(json!({"error":code}))).into_response();
    }
    Json(json!({"available":true,"connection":attempt.snapshot()})).into_response()
}
pub(crate) async fn cancel(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    connection_response(&state, &id, true).await
}
async fn connection_response(state: &AppState, id: &str, cancel: bool) -> Response {
    if !super::enabled() {
        return unavailable();
    }
    let attempt = crate::lock(&state.cloud_providers.connections)
        .get(id)
        .cloned();
    let Some(attempt) = attempt else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"connection_expired"})),
        )
            .into_response();
    };
    if cancel {
        attempt.cancel();
        attempt.wait_finished().await;
    }
    Json(json!({"available":true,"connection":attempt.snapshot()})).into_response()
}
