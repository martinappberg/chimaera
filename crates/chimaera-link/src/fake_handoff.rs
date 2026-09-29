//! Bounded in-memory ownership fixture; production storage is deliberately absent.
use crate::{fake::FakeKeeper, *};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio::{sync::Mutex, time::Instant};

#[derive(Default)]
pub(crate) struct FixtureHandoff {
    state: Mutex<Data>,
}
#[derive(Default)]
struct Data {
    batons: HashMap<String, Lease>,
    devices: HashMap<String, String>,
    credentials: HashMap<String, Credential>,
    delegations: HashMap<String, Scoped>,
    policies: HashMap<String, HandoffPolicy>,
    mirror_disabled: HashSet<String>,
}
struct Scoped {
    device: String,
    deadline: Instant,
    parent_deadline: Instant,
}
struct Lease {
    baton: Baton,
    deadline: Instant,
}
struct Credential {
    workspace: String,
    holder: String,
    epoch: Option<u64>,
    deadline: Instant,
}
impl FixtureHandoff {
    pub async fn allows(&self, token: &str, path: &str) -> bool {
        let data = self.state.lock().await;
        data.delegations
            .get(token)
            .is_some_and(|d| d.deadline > Instant::now())
            && (path.starts_with("/v1/baton/")
                || path == "/v1/mirror/credentials"
                || path == "/v1/delegations/renew"
                || path == "/v1/events"
                || path == "/v1/hosts"
                || path.starts_with("/v1/hosts/")
                || path == "/v1/serve"
                || path.starts_with("/v1/serve/"))
    }
    pub async fn revoke_all(&self) {
        let mut data = self.state.lock().await;
        data.delegations.clear();
        data.credentials.clear();
    }
    pub async fn bind_device(&self, token: &str, device: &str) -> anyhow::Result<()> {
        let mut data = self.state.lock().await;
        anyhow::ensure!(data.devices.len() < 256, "fixture device limit");
        data.devices.insert(token.into(), device.into());
        Ok(())
    }
}
pub(crate) fn routes() -> Router<FakeKeeper> {
    Router::new()
        .route("/v1/delegations", post(delegate))
        .route("/v1/delegations/renew", post(renew_delegation))
        .route("/v1/baton/{id}", get(read))
        .route(
            "/v1/baton/{id}/policy",
            axum::routing::put(policy).delete(disable_policy),
        )
        .route("/v1/baton/{id}/enable-mirror", post(enable_mirror))
        .route("/v1/baton/{id}/acquire", post(acquire))
        .route("/v1/baton/{id}/renew", post(renew))
        .route("/v1/baton/{id}/release", post(release))
        .route("/v1/mirror/credentials", post(credentials))
        .route("/_test/baton/{id}/expire", post(expire))
        .route("/_test/mirror-write", post(mirror_write))
}
fn timestamp(seconds: i64) -> String {
    (time::OffsetDateTime::now_utc() + time::Duration::seconds(seconds))
        .format(&time::format_description::well_known::Rfc3339)
        .expect("valid timestamp")
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
}
fn holder(data: &Data, headers: &HeaderMap) -> String {
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    data.delegations
        .get(token)
        .map(|d| d.device.clone())
        .or_else(|| data.devices.get(token).cloned())
        .unwrap_or_else(|| "fake-device".into())
}
fn current(data: &Data, id: &str) -> Baton {
    let mut value = data
        .batons
        .get(id)
        .map(|l| l.baton.clone())
        .unwrap_or(Baton {
            workspace_id: id.into(),
            holder_id: None,
            epoch: 0,
            expires_at: None,
            server_now: timestamp(0),
            requires_fork: false,
        });
    value.server_now = timestamp(0);
    value
}
/// A caller naming a holder other than its own credential's: the service
/// answers `400 invalid_request`, not 403.
fn holder_mismatch() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: "invalid_request".into(),
        }),
    )
        .into_response()
}
/// v1 conflicts use exactly the service's vocabulary: `stale_epoch` for a
/// mismatched epoch, an occupied baton, a non-holder and an expired lease;
/// `mirror_commit_in_progress` during the publication fence.
fn conflict(error: &str, baton: Baton) -> Response {
    (
        StatusCode::CONFLICT,
        Json(BatonConflict {
            error: error.into(),
            baton: Some(baton),
        }),
    )
        .into_response()
}
async fn read(State(keeper): State<FakeKeeper>, Path(id): Path<String>) -> Response {
    if !valid_id(&id) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    Json(current(&*keeper.handoff().state.lock().await, &id)).into_response()
}
async fn acquire(
    State(keeper): State<FakeKeeper>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AcquireBaton>,
) -> Response {
    if !valid_id(&id) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut data = keeper.handoff().state.lock().await;
    if holder(&data, &headers) != request.holder_id {
        return holder_mismatch();
    }
    let mut baton = current(&data, &id);
    if baton.epoch != request.expected_epoch {
        return conflict("stale_epoch", baton);
    }
    let active = data
        .batons
        .get(&id)
        .is_some_and(|l| l.baton.holder_id.is_some() && l.deadline > Instant::now());
    if active {
        return if baton.holder_id.as_ref() == Some(&request.holder_id) {
            Json(baton).into_response()
        } else {
            conflict("stale_epoch", baton)
        };
    }
    if !data.batons.contains_key(&id) && data.batons.len() >= 128 {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Some(epoch) = baton.epoch.checked_add(1) else {
        return StatusCode::CONFLICT.into_response();
    };
    baton.epoch = epoch;
    baton.requires_fork = baton.holder_id.is_some();
    baton.holder_id = Some(request.holder_id);
    baton.expires_at = Some(timestamp(BATON_LEASE_SECONDS as i64));
    data.batons.insert(
        id,
        Lease {
            baton: baton.clone(),
            deadline: Instant::now() + Duration::from_secs(BATON_LEASE_SECONDS),
        },
    );
    Json(baton).into_response()
}
async fn renew(
    State(keeper): State<FakeKeeper>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<HeldBaton>,
) -> Response {
    update(&keeper, &id, &headers, request, false).await
}
async fn release(
    State(keeper): State<FakeKeeper>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<HeldBaton>,
) -> Response {
    update(&keeper, &id, &headers, request, true).await
}
async fn update(
    keeper: &FakeKeeper,
    id: &str,
    headers: &HeaderMap,
    request: HeldBaton,
    release: bool,
) -> Response {
    if !valid_id(id) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut data = keeper.handoff().state.lock().await;
    if holder(&data, headers) != request.holder_id {
        return holder_mismatch();
    }
    let baton = current(&data, id);
    if baton.epoch != request.epoch {
        return conflict("stale_epoch", baton);
    }
    if baton.holder_id.as_ref() != Some(&request.holder_id) {
        return conflict("stale_epoch", baton);
    }
    let lease = data.batons.get_mut(id).expect("holder has a lease");
    if lease.deadline <= Instant::now() {
        return conflict("stale_epoch", baton);
    }
    lease.baton.server_now = timestamp(0);
    if release {
        lease.baton.holder_id = None;
        lease.baton.expires_at = None;
        lease.baton.requires_fork = false;
    } else {
        lease.deadline = Instant::now() + Duration::from_secs(BATON_LEASE_SECONDS);
        lease.baton.expires_at = Some(timestamp(BATON_LEASE_SECONDS as i64));
    }
    Json(lease.baton.clone()).into_response()
}
async fn credentials(
    State(keeper): State<FakeKeeper>,
    headers: HeaderMap,
    Json(request): Json<MirrorRequest>,
) -> Response {
    if !valid_id(&request.workspace_id) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut data = keeper.handoff().state.lock().await;
    if data.mirror_disabled.contains(&request.workspace_id) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let holder = holder(&data, &headers);
    if let Some(epoch) = request.epoch {
        let baton = current(&data, &request.workspace_id);
        if !data.batons.get(&request.workspace_id).is_some_and(|l| {
            l.deadline > Instant::now()
                && l.baton.epoch == epoch
                && l.baton.holder_id.as_ref() == Some(&holder)
        }) {
            return conflict("stale_epoch", baton);
        }
    }
    data.credentials.retain(|_, c| c.deadline > Instant::now());
    if data.credentials.len() >= 256 {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let password = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        rand::random::<[u8; 32]>(),
    );
    data.credentials.insert(
        password.clone(),
        Credential {
            workspace: request.workspace_id.clone(),
            holder,
            epoch: request.epoch,
            deadline: Instant::now() + Duration::from_secs(900),
        },
    );
    Json(MirrorCredentials {
        repository_url: format!(
            "{}/git/{}/repository.git",
            keeper.endpoint, request.workspace_id
        ),
        working_tree_url: format!(
            "{}/git/{}/working-tree.git",
            keeper.endpoint, request.workspace_id
        ),
        workspace_id: request.workspace_id,
        username: "scoped".into(),
        password,
        expires_at: timestamp(900),
        read_only: request.epoch.is_none(),
        storage_limit_bytes: 4_000_000_000,
        max_file_bytes: 100_000_000,
    })
    .into_response()
}
async fn expire(State(keeper): State<FakeKeeper>, Path(id): Path<String>) -> Response {
    let mut data = keeper.handoff().state.lock().await;
    let Some(lease) = data.batons.get_mut(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    lease.deadline = Instant::now();
    lease.baton.expires_at = Some(timestamp(-1));
    StatusCode::NO_CONTENT.into_response()
}
#[derive(Deserialize)]
struct Write {
    password: String,
    workspace_id: String,
}
async fn mirror_write(State(keeper): State<FakeKeeper>, Json(request): Json<Write>) -> Response {
    let data = keeper.handoff().state.lock().await;
    let allowed = data
        .credentials
        .get(&request.password)
        .is_some_and(|credential| {
            credential.workspace == request.workspace_id
                && credential.deadline > Instant::now()
                && data.batons.get(&request.workspace_id).is_some_and(|lease| {
                    lease.deadline > Instant::now()
                        && lease.baton.holder_id.as_ref() == Some(&credential.holder)
                        && credential.epoch == Some(lease.baton.epoch)
                })
        });
    if allowed {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::FORBIDDEN
    }
    .into_response()
}

fn delegated_response(token: String, device: String, seconds: i64) -> Json<Delegation> {
    Json(Delegation {
        workspace: None,
        access_token: token,
        device_id: device,
        expires_at: timestamp(seconds),
        scope: vec!["baton".into(), "mirror".into(), "keeper".into()],
    })
}
async fn delegate(State(keeper): State<FakeKeeper>, headers: HeaderMap) -> Response {
    let mut data = keeper.handoff().state.lock().await;
    let device = holder(&data, &headers);
    data.delegations
        .retain(|_, d| d.device != device && d.deadline > Instant::now());
    if data.delegations.len() >= 256 {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let token = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        rand::random::<[u8; 32]>(),
    );
    data.delegations.insert(
        token.clone(),
        Scoped {
            device: device.clone(),
            deadline: Instant::now() + Duration::from_secs(86400),
            parent_deadline: Instant::now() + Duration::from_secs(86400),
        },
    );
    drop(data);
    // A replacement invalidates old live sockets too. The single-account
    // fixture reconnects all device streams; production targets the parent.
    keeper.invalidate_live_transports();
    delegated_response(token, device, 86400).into_response()
}
async fn renew_delegation(State(keeper): State<FakeKeeper>, headers: HeaderMap) -> Response {
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    let mut data = keeper.handoff().state.lock().await;
    let Some(delegation) = data.delegations.get_mut(token) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let now = Instant::now();
    if delegation.deadline <= now || delegation.parent_deadline <= now {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    delegation.deadline = (now + Duration::from_secs(86400)).min(delegation.parent_deadline);
    delegated_response(
        token.into(),
        delegation.device.clone(),
        delegation.deadline.duration_since(now).as_secs() as i64,
    )
    .into_response()
}

async fn policy(
    State(keeper): State<FakeKeeper>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(policy): Json<HandoffPolicy>,
) -> Response {
    let mut data = keeper.handoff().state.lock().await;
    let baton = current(&data, &id);
    if holder(&data, &headers) != policy.holder_id {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !data.batons.get(&id).is_some_and(|lease| {
        lease.deadline > Instant::now()
            && lease.baton.holder_id.as_deref() == Some(&policy.holder_id)
            && lease.baton.epoch == policy.epoch
    }) {
        return conflict("stale_epoch", baton);
    }
    data.policies.insert(id, policy);
    StatusCode::NO_CONTENT.into_response()
}
async fn disable_policy(
    State(keeper): State<FakeKeeper>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> StatusCode {
    let mut data = keeper.handoff().state.lock().await;
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    if data.delegations.contains_key(token) {
        return StatusCode::FORBIDDEN;
    }
    if !valid_id(&id) {
        return StatusCode::BAD_REQUEST;
    }
    if data.mirror_disabled.len() >= 128 && !data.mirror_disabled.contains(&id) {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    data.credentials.retain(|_, c| c.workspace != id);
    data.mirror_disabled.insert(id.clone());
    data.policies.remove(&id);
    StatusCode::NO_CONTENT
}

async fn enable_mirror(
    State(keeper): State<FakeKeeper>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> StatusCode {
    let mut data = keeper.handoff().state.lock().await;
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    if data.delegations.contains_key(token) {
        return StatusCode::FORBIDDEN;
    }
    if !valid_id(&id) {
        return StatusCode::BAD_REQUEST;
    }
    data.mirror_disabled.remove(&id);
    StatusCode::NO_CONTENT
}
