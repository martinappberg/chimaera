//! Signing out on a computer stands its projects down at the account first.
//!
//! Sign-out never stops a computer's own work, and nobody verifies a
//! signed-out computer's leases any more. Both are only safe once the account
//! will not continue that work elsewhere: a held project whose policy still
//! says `has_agents`/`offline_takeover` is taken over by the cloud about 75 s
//! later while this computer keeps running it (review R4 B2). So
//! `/pro/disconnect` publishes "nothing to continue" for every project this
//! computer holds ([`stand_down`]) before it drops the delegation. A project
//! the account did not acknowledge stays [`unreleased`]: its lease proof is
//! kept, its agents are fenced at that lease's deadline (fail closed), no
//! signed-out exemption applies to it, and a background [`retry`] keeps asking
//! until the account answers. The credential lives only in that task's memory.
use super::protocol::Configure;
use crate::{lock, AppState};
use serde_json::json;
use std::{sync::Arc, time::Duration};

/// How long sign-out waits for the stand-downs before answering; a project
/// still unanswered then is owed and retried in the background. Kept well
/// under the app's 15 s wait for `/pro/disconnect`.
const WAIT: Duration = Duration::from_secs(8);
/// Stand-downs sent at once.
const AT_ONCE: usize = 8;

/// What the account said to a stand-down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Answer {
    /// Acknowledged: nobody continues the project on this computer's behalf.
    Released,
    /// Refused for good (this computer no longer holds the lease, or its
    /// credential is refused): the project stays fenced until sign-in.
    Refused,
    /// No answer, or the account failing: asked again in the background.
    Owed,
}

pub(super) fn answer(response: &anyhow::Result<super::transport::Response>) -> Answer {
    let Ok(response) = response else {
        return Answer::Owed;
    };
    match response.status {
        200..=299 => Answer::Released,
        // A project kept off the cloud cannot be taken by anyone.
        403 if serde_json::from_slice::<serde_json::Value>(&response.body)
            .is_ok_and(|body| body["error"] == "mirror_disabled") =>
        {
            Answer::Released
        }
        408 | 429 | 500.. => Answer::Owed,
        _ => Answer::Refused,
    }
}

/// Publishes "nothing to continue" for one held project: no handoff, no
/// takeover after a lapse, no agents, so neither the lapse takeover nor a
/// browser's open resumes it on this computer's behalf.
async fn stand_down(config: &Configure, workspace: &str, epoch: u64) -> Answer {
    let response = super::engine::account(
        config,
        &format!("/v1/baton/{workspace}/policy"),
        "PUT",
        Some(&json!({
            "holder_id": config.delegation.device_id,
            "epoch": epoch,
            "handoff_enabled": false,
            "offline_takeover": false,
            "has_agents": false,
        })),
    )
    .await;
    answer(&response)
}

/// Stands every held project down, at most [`AT_ONCE`] at a time and for at
/// most [`WAIT`]; a project without an answer by then is owed.
pub(super) async fn stand_down_all(
    config: &Configure,
    held: &[(String, u64)],
) -> Vec<(String, u64, Answer)> {
    use futures::StreamExt;
    let answered = std::sync::Mutex::new(Vec::new());
    let _ = tokio::time::timeout(
        WAIT,
        futures::stream::iter(held.iter().cloned())
            .map(|(workspace, epoch)| async move {
                let answer = stand_down(config, &workspace, epoch).await;
                (workspace, epoch, answer)
            })
            .buffer_unordered(AT_ONCE)
            .for_each(|row| {
                lock(&answered).push(row);
                async {}
            }),
    )
    .await;
    let mut answered = answered.into_inner().unwrap_or_else(|e| e.into_inner());
    for (workspace, epoch) in held {
        if !answered.iter().any(|(id, _, _)| id == workspace) {
            answered.push((workspace.clone(), *epoch, Answer::Owed));
        }
    }
    answered
}

/// Signed out without the account acknowledging this project's stand-down.
pub(super) fn unreleased(state: &AppState, workspace: &str) -> bool {
    super::signed_out(state) && lock(&state.pro.unreleased).contains(workspace)
}
/// Signed out, and the account acknowledged the stand-down (or this computer
/// held no lease for the project): nobody continues it elsewhere, so the
/// computer's own work goes on without the account.
pub(super) fn released(state: &AppState, workspace: &str) -> bool {
    super::signed_out(state) && !lock(&state.pro.unreleased).contains(workspace)
}
/// A new configuration (sign-in) ends sign-out's bookkeeping: the lease loop
/// decides from here.
pub(super) fn forget(state: &AppState) {
    if let Some(task) = lock(&state.pro.stand_down).take() {
        task.abort();
    }
    lock(&state.pro.unreleased).clear();
}

/// Asks again, in the background, for the owed stand-downs, backing off to
/// half a minute, until each is answered or the user signs in again. A
/// release lets the project's agents run on (its kept proof goes) and resumes
/// what its fence stopped; a refusal leaves it fenced.
pub(super) fn retry(state: &Arc<AppState>, config: Configure, mut owed: Vec<(String, u64)>) {
    if let Some(task) = lock(&state.pro.stand_down).take() {
        task.abort();
    }
    if owed.is_empty() {
        return;
    }
    let owner = Arc::downgrade(state);
    let task = tokio::spawn(async move {
        let mut wait = Duration::from_secs(2);
        while !owed.is_empty() {
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(Duration::from_secs(30));
            let answers = stand_down_all(&config, &owed).await;
            let Some(state) = owner.upgrade() else {
                return;
            };
            if !super::signed_out(&state)
                || state.stopping.load(std::sync::atomic::Ordering::Acquire)
            {
                return;
            }
            let mut changed = Vec::new();
            for (workspace, epoch, answer) in answers {
                if answer == Answer::Owed {
                    continue;
                }
                owed.retain(|(id, _)| *id != workspace);
                if answer == Answer::Released {
                    lock(&state.pro.unreleased).remove(&workspace);
                    super::execution::drop_held(&state, &workspace);
                    changed.push((workspace, epoch));
                } else {
                    tracing::info!(
                        "The account refused to stand a signed-out project down; it stays paused here"
                    );
                }
            }
            if changed.is_empty() {
                continue;
            }
            if let Err(error) = super::persist(&state).await {
                tracing::warn!(%error, "Could not save a project's stand-down after sign-out");
            }
            for (workspace, epoch) in changed {
                resume(&state, &workspace, epoch).await;
            }
        }
    });
    *lock(&state.pro.stand_down) = Some(task);
}

/// The project is this computer's alone again: what a fence stopped at the
/// stood-down epoch resumes, as a signed-out computer's work does.
pub(super) async fn resume(state: &Arc<AppState>, workspace: &str, _epoch: u64) {
    if let Err(error) = crate::ledger::resume_deferred_workspace(state, workspace).await {
        tracing::warn!(%error, "Could not resume a project's sessions after sign-out");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, body: &str) -> anyhow::Result<super::super::transport::Response> {
        Ok(super::super::transport::Response {
            status,
            body: body.as_bytes().to_vec(),
            from_account: true,
        })
    }

    #[test]
    fn only_an_acknowledgment_releases_and_only_a_failure_is_asked_again() {
        assert_eq!(answer(&reply(204, "")), Answer::Released);
        assert_eq!(
            answer(&reply(403, r#"{"error":"mirror_disabled"}"#)),
            Answer::Released
        );
        assert_eq!(
            answer(&reply(403, r#"{"error":"forbidden"}"#)),
            Answer::Refused
        );
        assert_eq!(
            answer(&reply(409, r#"{"error":"stale_epoch"}"#)),
            Answer::Refused
        );
        assert_eq!(answer(&reply(401, "")), Answer::Refused);
        assert_eq!(answer(&reply(503, "")), Answer::Owed);
        assert_eq!(answer(&reply(408, "")), Answer::Owed);
        assert_eq!(answer(&Err(anyhow::anyhow!("offline"))), Answer::Owed);
    }

    /// Review R4 B2: signing out first tells the account there is nothing
    /// to continue for each held project. Acknowledged, the computer's work
    /// goes on unfenced; unanswered, the project stays fenced at its lease's
    /// deadline (also after a restart) until a background retry is answered.
    #[tokio::test]
    async fn signing_out_stands_held_projects_down_or_keeps_them_fenced() {
        use axum::{extract::Request, response::IntoResponse, routing::any, Router};
        use std::sync::atomic::{AtomicU16, Ordering};
        for acknowledged in [true, false] {
            let root = std::env::temp_dir().join(format!(
                "chimaera-sign-out-{}",
                chimaera_core::generate_token()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let state = Arc::new(AppState::new(
                "fixture".into(),
                "fixture".into(),
                4242,
                0,
                root.clone(),
                root.join("config"),
            ));
            crate::pro::install_execution_fixture(&state, "w-a", 2).unwrap();
            let status = Arc::new(AtomicU16::new(if acknowledged { 204 } else { 503 }));
            let bodies = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
            let (answer_with, seen) = (status.clone(), bodies.clone());
            let router = Router::new().fallback(any(move |request: Request| {
                let (status, seen) = (answer_with.clone(), seen.clone());
                async move {
                    let path = request.uri().path().to_owned();
                    let method = request.method().clone();
                    let body = axum::body::to_bytes(request.into_body(), 1 << 16)
                        .await
                        .unwrap();
                    if method != axum::http::Method::PUT || path != "/v1/baton/w-a/policy" {
                        return axum::http::StatusCode::NOT_FOUND.into_response();
                    }
                    lock(&seen).push(serde_json::from_slice(&body).unwrap());
                    axum::http::StatusCode::from_u16(status.load(Ordering::SeqCst))
                        .unwrap()
                        .into_response()
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            *lock(&state.pro.runtime) = Some(
                serde_json::from_value(json!({
                    "account_id":"a-fixture","role":"device","endpoint":endpoint,
                    "keeper_url":"","hours_exhausted":false,
                    "execution":{"version":1,"installation_id":"i-home",
                        "capability":super::super::execution::wire::ExecutionCapability::managed()},
                    "delegation":{"access_token":"synthetic","expires_at":"2099-01-01T00:00:00Z",
                        "scope":["baton","mirror"],"device_id":"d-home"}
                }))
                .unwrap(),
            );
            let answered =
                super::super::routes::disconnect(axum::extract::State(state.clone())).await;
            assert_eq!(answered.status(), axum::http::StatusCode::NO_CONTENT);
            let first = lock(&bodies)[0].clone();
            assert_eq!(
                first,
                json!({"holder_id":"d-home","epoch":2,"handoff_enabled":false,
                    "offline_takeover":false,"has_agents":false})
            );
            assert!(super::super::signed_out(&state));
            // Until its deadline the computer's agents keep running either way.
            assert!(crate::pro::may_execute(&state, "w-a"));
            if acknowledged {
                assert!(released(&state, "w-a"));
                assert!(!unreleased(&state, "w-a"));
                // No lease is kept, so nothing fences it later.
                assert!(super::super::execution::expired_lease_fixture(&state, "w-a").is_empty());
                assert!(crate::pro::may_execute(&state, "w-a"));
            } else {
                assert!(unreleased(&state, "w-a"));
                assert!(!released(&state, "w-a"));
                let saved = std::fs::read_to_string(state.pro.root.join("state.json")).unwrap();
                assert!(saved.contains(r#""unreleased":["w-a"]"#), "{saved}");
                // Fail closed: fenced at the kept lease's deadline.
                assert_eq!(
                    super::super::execution::expired_lease_fixture(&state, "w-a"),
                    vec!["w-a"]
                );
                assert!(!crate::pro::may_execute(&state, "w-a"));
                // The account comes back: the retry's acknowledgment releases it.
                status.store(204, Ordering::SeqCst);
                tokio::time::timeout(Duration::from_secs(20), async {
                    while unreleased(&state, "w-a") {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await
                .expect("the background retry releases the project");
                assert!(crate::pro::may_execute(&state, "w-a"));
                assert!(lock(&bodies).len() >= 2);
            }
            forget(&state);
            server.abort();
            let _ = server.await;
            drop(state);
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
