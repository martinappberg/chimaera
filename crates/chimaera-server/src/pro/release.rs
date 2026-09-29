//! A successful mirror publication retains its account fence for ten seconds.
//! Only that explicit conflict may delay release; other failures retain ownership.
use super::{account, Baton, Configure};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
#[cfg(test)]
use serde_json::json;
use std::time::Duration;

#[derive(Deserialize)]
struct Conflict {
    error: String,
    baton: Baton,
}

pub(super) async fn after_publication(
    config: &Configure,
    workspace: &str,
    epoch: u64,
    budget: Duration,
    current: impl Fn() -> bool,
) -> Result<()> {
    tokio::time::timeout(budget.min(Duration::from_secs(15)), async {
        loop {
            ensure!(
                current(),
                "Account or project ownership changed before release"
            );
            let mut body = super::super::execution::body(config, epoch, false);
            let path = if config.recovery {
                body["workspace_id"] = workspace.into();
                body.as_object_mut().unwrap().remove("execution_capability");
                "/v2/recovery/release".into()
            } else {
                super::super::execution::path(config, workspace, "release")
            };
            let response = account(config, &path, "POST", Some(&body)).await?;
            if (200..300).contains(&response.status) {
                return Ok(());
            }
            ensure!(response.status == 409, "workspace release failed");
            if config.recovery {
                let value: serde_json::Value = serde_json::from_slice(&response.body)
                    .context("invalid recovery release response")?;
                ensure!(
                    value["error"] == "mirror_commit_in_progress",
                    "recovery release failed"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            let conflict: Conflict = serde_json::from_slice(&response.body)
                .context("invalid workspace release response")?;
            let baton = conflict.baton;
            // Compare server timestamps, not this device's potentially skewed clock.
            let now = utc_order(&baton.server_now);
            let expires = baton.expires_at.as_deref().and_then(utc_order);
            ensure!(
                conflict.error == "mirror_commit_in_progress"
                    && baton.workspace_id == workspace
                    && baton.epoch == epoch
                    && baton.holder_id.as_deref() == Some(&config.delegation.device_id)
                    && matches!((now, expires), (Some(now), Some(expires)) if expires > now),
                "workspace ownership changed before release"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .context("Project synchronization is finishing; please try again shortly")?
}

// Account timestamps are RFC3339 UTC. Restrict accepted offsets and normalize
// optional fractional seconds before ordering; never compare against device time.
fn utc_order(value: &str) -> Option<(&str, u32)> {
    let utc = value
        .strip_suffix('Z')
        .or_else(|| value.strip_suffix("+00:00"))?;
    let (seconds, fraction) = utc.split_once('.').unwrap_or((utc, ""));
    let bytes = seconds.as_bytes();
    if bytes.len() != 19
        || ![4, 7, 10, 13, 16]
            .into_iter()
            .zip(b"--T::")
            .all(|(i, separator)| bytes[i] == *separator)
        || !bytes
            .iter()
            .enumerate()
            .all(|(i, b)| [4, 7, 10, 13, 16].contains(&i) || b.is_ascii_digit())
        || fraction.len() > 9
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let field = |start, end| seconds.get(start..end)?.parse::<u32>().ok();
    if !(1..=12).contains(&field(5, 7)?)
        || !(1..=31).contains(&field(8, 10)?)
        || field(11, 13)? > 23
        || field(14, 16)? > 59
        || field(17, 19)? > 59
    {
        return None;
    }
    let nanos = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u32>().ok()? * 10u32.pow(9 - fraction.len() as u32)
    };
    Some((seconds, nanos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pro::protocol::{Delegation, Role};
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
        Json, Router,
    };
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    struct Fixture {
        calls: AtomicUsize,
        error: &'static str,
        epoch: u64,
        live: bool,
        current: AtomicBool,
        invalidate: bool,
    }
    async fn release(
        State(f): State<Arc<Fixture>>,
        headers: HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        assert_eq!(headers["authorization"], "Bearer fixture");
        assert_eq!(body, json!({"holder_id":"device","epoch":4}));
        let call = f.calls.fetch_add(1, Ordering::SeqCst);
        if f.invalidate {
            f.current.store(false, Ordering::SeqCst);
        }
        if call >= 2 {
            return (StatusCode::OK, Json(json!({})));
        }
        let now = "2026-09-28T00:00:00Z";
        (
            StatusCode::CONFLICT,
            Json(json!({"error":f.error,"baton":{
                "workspace_id":"w-fixture","holder_id":"device","epoch":f.epoch,
                "server_now":now,"expires_at":if f.live {"2026-09-28T00:01:30Z"} else {"2026-09-27T23:59:59Z"},
                "requires_fork":false
            }})),
        )
    }
    async fn run(error: &'static str, epoch: u64, live: bool, invalidate: bool) -> (bool, usize) {
        let fixture = Arc::new(Fixture {
            calls: AtomicUsize::new(0),
            error,
            epoch,
            live,
            current: AtomicBool::new(true),
            invalidate,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/baton/w-fixture/release", post(release))
            .with_state(fixture.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = Configure {
            recovery: false,
            execution: None,
            account_id: None,
            role: Role::Device,
            endpoint,
            keeper_url: String::new(),
            hours_exhausted: false,
            delegation: Delegation {
                workspace: None,
                access_token: "fixture".into(),
                expires_at: String::new(),
                scope: vec![],
                device_id: "device".into(),
            },
        };
        let result = after_publication(&config, "w-fixture", 4, Duration::from_secs(15), || {
            fixture.current.load(Ordering::SeqCst)
        })
        .await;
        task.abort();
        (result.is_ok(), fixture.calls.load(Ordering::SeqCst))
    }
    #[test]
    fn utc_timestamp_order_normalizes_fraction_and_rejects_other_offsets() {
        assert_eq!(
            utc_order("2026-09-28T01:02:03Z"),
            utc_order("2026-09-28T01:02:03.000+00:00")
        );
        assert!(utc_order("2026-09-28T01:02:03.9Z") < utc_order("2026-09-28T01:02:04Z"));
        assert!(utc_order("2026-09-28T01:02:03.1Z") > utc_order("2026-09-28T01:02:03Z"));
        assert!(utc_order("2026-09-28T01:02:03+01:00").is_none());
        assert!(utc_order("malformed").is_none());
    }
    #[tokio::test]
    async fn publication_fence_waits_without_republishing_or_changing_epoch() {
        assert_eq!(
            run("mirror_commit_in_progress", 4, true, false).await,
            (true, 3)
        );
    }
    #[tokio::test]
    async fn unrelated_conflict_changed_epoch_or_expired_lease_never_retries() {
        assert_eq!(run("stale_epoch", 4, true, false).await, (false, 1));
        assert_eq!(
            run("mirror_commit_in_progress", 5, true, false).await,
            (false, 1)
        );
        assert_eq!(
            run("mirror_commit_in_progress", 4, false, false).await,
            (false, 1)
        );
    }
    #[tokio::test]
    async fn account_change_during_publication_wait_prevents_another_request() {
        assert_eq!(
            run("mirror_commit_in_progress", 4, true, true).await,
            (false, 1)
        );
    }
    #[tokio::test]
    async fn changed_account_never_recovers_using_the_previous_delegation() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-release-account-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = Arc::new(crate::AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let server_state = state.clone();
        let server_calls = calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route("/v1/baton/w-fixture", axum::routing::get(move || {
            let state = server_state.clone(); let calls = server_calls.clone(); async move {
                calls.fetch_add(1, Ordering::SeqCst);
                state.pro.generation.fetch_add(1, Ordering::SeqCst);
                Json(json!({"workspace_id":"w-fixture","holder_id":"device","epoch":4,"server_now":"2026-09-28T00:00:00Z","expires_at":"2026-09-28T00:01:30Z","requires_fork":false}))
            }
        }));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = Configure {
            recovery: false,
            execution: None,
            account_id: None,
            role: Role::Device,
            endpoint,
            keeper_url: String::new(),
            hours_exhausted: false,
            delegation: Delegation {
                workspace: None,
                access_token: "fixture".into(),
                expires_at: String::new(),
                scope: vec![],
                device_id: "device".into(),
            },
        };
        let generation = state.pro.generation.load(Ordering::SeqCst);
        assert!(
            super::super::reconcile_generation(&state, &config, "w-fixture", generation)
                .await
                .is_err()
        );
        assert!(
            crate::lock(&state.pro.ownership).is_empty(),
            "old reply cannot restore local ownership"
        );
        assert!(
            super::super::reconcile_generation(&state, &config, "w-fixture", generation)
                .await
                .is_err()
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a stale recovery never contacts the old account again"
        );
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
}
