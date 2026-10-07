//! The public half of a fenced suspension. A cloud machine is idle only when
//! nothing here is still writing: sampling "no job right now" raced a push or
//! a finalizer that started a moment later and was then frozen mid-write.
//! The supervisor asks the daemon to drain first: new transfers are refused
//! (409 `draining`), the periodic pass does not start, and the call returns
//! once the job reservation, every transfer task, every project cache and
//! every Git helper slot is free and state is flushed to disk. The drain holds
//! until DELETE, or lapses on its own a while after the machine resumes.
use super::{detached, transport};
use crate::{lock, AppState};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

/// Wall-clock bound: a suspended machine that resumes without a cancel (the
/// supervisor restarted) returns to normal work by itself.
const LAPSE: Duration = Duration::from_secs(15 * 60);
const DEFAULT_DEADLINE_MS: u64 = 60_000;

pub(super) struct Drain {
    token: String,
    since: SystemTime,
    _jobs: tokio::sync::OwnedMutexGuard<()>,
}

#[derive(Default, Deserialize)]
struct Request {
    #[serde(default)]
    deadline_ms: Option<u64>,
}

/// Whether new transfer work must be refused right now.
pub(super) fn draining(state: &AppState) -> bool {
    let mut drain = lock(&state.pro().drain);
    if drain.as_ref().is_some_and(|drain| {
        SystemTime::now()
            .duration_since(drain.since)
            .is_ok_and(|age| age > LAPSE)
    }) {
        *drain = None;
    }
    drain.is_some()
}
pub(super) fn refusal() -> detached::Outcome {
    detached::Outcome::refused(StatusCode::CONFLICT, Some(json!({"error":"draining"})))
}

fn quiet(state: &AppState) -> bool {
    detached::running(state) == 0
        && lock(&state.pro().sleeping).is_empty()
        && lock(&state.pro().caches)
            .values()
            .all(|cache| cache.strong_count() == 0)
        && transport::helpers_idle()
}

/// `POST /api/v1/pro/drain {deadline_ms?}` → 200 `{token}` once quiet, or 409
/// `{error:"transfer_busy"}` (drain released) when the deadline passes first.
pub(crate) async fn start(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    let request: Request = if body.iter().all(u8::is_ascii_whitespace) {
        Request::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(request) => request,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    };
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(
            request
                .deadline_ms
                .unwrap_or(DEFAULT_DEADLINE_MS)
                .clamp(1_000, 600_000),
        );
    // One drain at a time: a second request waits for the first and then
    // shares its token (or starts afresh if the first gave up).
    let Ok(_gate) = tokio::time::timeout_at(deadline, state.pro().drain_gate.lock()).await else {
        return busy();
    };
    if draining(&state) {
        let token = lock(&state.pro().drain)
            .as_ref()
            .map(|drain| drain.token.clone());
        return Json(json!({"token":token})).into_response();
    }
    // The periodic pass and every transfer hold this reservation; taking it
    // both waits for them and keeps new ones from starting.
    let jobs = match tokio::time::timeout_at(deadline, state.pro().jobs.clone().lock_owned()).await
    {
        Ok(jobs) => jobs,
        Err(_) => return busy(),
    };
    let token = chimaera_core::generate_token()[..24].to_owned();
    *lock(&state.pro().drain) = Some(Drain {
        token: token.clone(),
        since: SystemTime::now(),
        _jobs: jobs,
    });
    // A transfer admitted just before this drain, still waiting for the
    // reservation, now refuses itself instead of holding the drain open.
    state.pro().drain_started.notify_waiters();
    // A caller that gives up (its request dropped) must not leave this
    // daemon draining: only a completed drain stays in place.
    let mut pending = Pending {
        state: &state,
        armed: true,
    };
    let settle = || {
        tokio::time::timeout_at(deadline, async {
            while !quiet(&state) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    };
    if settle().await.is_err() {
        return busy();
    }
    if let Err(error) = super::persist(&state).await {
        return super::routes::failure(error);
    }
    // Everything this daemon wrote reaches the disk before a snapshot of the
    // machine is taken; the supervisor still syncs the volume itself, so a
    // slow sync is not waited for past the caller's deadline.
    let _ = tokio::time::timeout_at(deadline, tokio::task::spawn_blocking(nix::unistd::sync)).await;
    pending.armed = false;
    Json(json!({"token":token})).into_response()
}

struct Pending<'a> {
    state: &'a AppState,
    armed: bool,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if self.armed {
            *lock(&self.state.pro().drain) = None;
            self.state.changes.notify_waiters();
        }
    }
}

/// Takes the job reservation for a transfer, unless a drain starts first:
/// then the transfer refuses itself (`None`) rather than wait behind the drain
/// that is waiting for it.
pub(super) async fn reserve(state: &AppState) -> Option<tokio::sync::MutexGuard<'_, ()>> {
    let started = state.pro().drain_started.notified();
    tokio::pin!(started);
    started.as_mut().enable();
    if draining(state) {
        return None;
    }
    tokio::select! {
        guard = state.pro().jobs.lock() => Some(guard),
        () = started => None,
    }
}
fn busy() -> Response {
    (StatusCode::CONFLICT, Json(json!({"error":"transfer_busy"}))).into_response()
}

/// `DELETE /api/v1/pro/drain` → 204: normal work resumes.
pub(crate) async fn cancel(State(state): State<Arc<AppState>>) -> Response {
    *lock(&state.pro().drain) = None;
    state.changes.notify_waiters();
    StatusCode::NO_CONTENT.into_response()
}
