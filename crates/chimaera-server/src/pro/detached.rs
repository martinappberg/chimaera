//! Long transfers outlive the HTTP request that started them. A caller that
//! gives up (the keeper's 90 s relay, a supervisor timeout, a sleep deadline)
//! must never cancel Git halfway through a write or strand a Transferring or
//! Hydrating fence with its agents stopped. Each operation is an owned task
//! keyed by (kind, workspace, epoch): a repeated request joins the running
//! task. A release cannot be repeated once done, so its success is remembered
//! briefly and a retry after a lost reply is answered, not refused; hydration
//! re-verifies itself on every request. A failure is forgotten at once so a
//! retry starts fresh with whatever changed (a prepared root, a cache).
use crate::{lock, AppState};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use futures::future::{BoxFuture, FutureExt, Shared};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const LIMIT: usize = 128;
const RETAIN_SUCCESS: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub(super) struct Outcome {
    status: StatusCode,
    body: Option<serde_json::Value>,
}
impl Outcome {
    pub fn done() -> Self {
        Self {
            status: StatusCode::NO_CONTENT,
            body: None,
        }
    }
    pub fn refused(status: StatusCode, body: Option<serde_json::Value>) -> Self {
        Self { status, body }
    }
    fn succeeded(&self) -> bool {
        self.status.is_success()
    }
    /// The operation completed (a handover released its project).
    pub fn ok(&self) -> bool {
        self.succeeded()
    }
}
impl IntoResponse for Outcome {
    fn into_response(self) -> Response {
        match self.body {
            Some(body) => (self.status, Json(body)).into_response(),
            None => self.status.into_response(),
        }
    }
}

type Key = (&'static str, String, u64);
struct Entry {
    task: Shared<BoxFuture<'static, Outcome>>,
    started: Instant,
    done: Arc<AtomicBool>,
}
#[derive(Default)]
pub(super) struct Operations {
    entries: Mutex<HashMap<Key, Entry>>,
}

/// Join a running (or, with `retain`, recently succeeded) operation, or run
/// `precheck` and, if it admits the request, start `work` as an owned task.
/// Awaiting the result is optional: dropping this future never cancels it.
pub(super) async fn run<W>(
    state: &Arc<AppState>,
    (kind, retain): (&'static str, bool),
    workspace: &str,
    epoch: u64,
    precheck: impl FnOnce() -> Option<Outcome>,
    work: impl FnOnce() -> W,
) -> Outcome
where
    W: std::future::Future<Output = Outcome> + Send + 'static,
{
    let key: Key = (kind, workspace.to_owned(), epoch);
    let task = {
        let mut entries = lock(&state.pro.operations.entries);
        entries.retain(|_, entry| {
            !entry.done.load(Ordering::Acquire) || entry.started.elapsed() < RETAIN_SUCCESS
        });
        if let Some(entry) = entries.get(&key) {
            entry.task.clone()
        } else {
            if let Some(refusal) = precheck() {
                return refusal;
            }
            if entries.len() >= LIMIT {
                return Outcome::refused(
                    StatusCode::SERVICE_UNAVAILABLE,
                    Some(serde_json::json!({"error":"transfer_capacity"})),
                );
            }
            let owner = Arc::downgrade(state);
            let forget = key.clone();
            let done = Arc::new(AtomicBool::new(false));
            let finished = done.clone();
            let work = work();
            // The owned task records its own completion: nobody has to be
            // awaiting it for a failure to be forgotten or a success retained.
            let handle = tokio::spawn(async move {
                let outcome = work.await;
                finished.store(true, Ordering::Release);
                if !(retain && outcome.succeeded()) {
                    if let Some(state) = owner.upgrade() {
                        lock(&state.pro.operations.entries).remove(&forget);
                    }
                }
                outcome
            });
            let task = async move {
                handle.await.unwrap_or_else(|_| {
                    Outcome::refused(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Some(serde_json::json!({"error":"transfer_interrupted"})),
                    )
                })
            }
            .boxed()
            .shared();
            entries.insert(
                key,
                Entry {
                    task: task.clone(),
                    started: Instant::now(),
                    done,
                },
            );
            task
        }
    };
    task.await
}

/// Transfers still running, for the idle/drain accounting.
pub(super) fn running(state: &AppState) -> usize {
    lock(&state.pro.operations.entries)
        .values()
        .filter(|entry| !entry.done.load(Ordering::Acquire))
        .count()
}
