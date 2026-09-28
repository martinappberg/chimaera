//! Waking an idle worker can replace its expired ownership epoch. Coordinate
//! that transition within one return attempt, without replaying ambiguous work.
use super::*;

pub(super) async fn prepare(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    host: &super::super::protocol::Host,
    holder: &str,
    initial_epoch: u64,
) -> Result<Option<u64>> {
    ensure!(
        host.worker_holder() == Some(holder),
        "Project connection changed"
    );
    let generation = state.pro.generation.load(Ordering::Acquire);
    let current = || {
        generation == state.pro.generation.load(Ordering::Acquire)
            && super::super::projects::account_matches(state, workspace)
            && !lock(&state.pro.preferences)
                .get(workspace)
                .is_some_and(|p| p.never_mirror)
    };
    tokio::time::timeout(Duration::from_secs(105), async {
        let mut seen_epoch = initial_epoch;
        let mut ambiguous = None;
        loop {
            ensure!(current(), "Account or project changed during return");
            let baton: Baton = account(config, &format!("/v1/baton/{workspace}"), "GET", None)
                .await
                .context("Could not check where your work is running")?
                .json()
                .context("Could not confirm where your work is running")?;
            ensure!(current(), "Account or project changed during return");
            ensure!(
                baton.workspace_id == workspace
                    && !baton.mirror_disabled
                    && baton.epoch >= seen_epoch,
                "Project ownership changed during return"
            );
            if baton.holder_id.is_none() {
                ensure!(
                    baton.epoch == seen_epoch,
                    "Project ownership changed during return"
                );
                return Ok(Some(baton.epoch));
            }
            if baton.holder_id.as_deref() != Some(holder) {
                return Ok(None);
            }
            seen_epoch = baton.epoch;
            // Persist the verified source epoch before requesting its release.
            // After a lost reply/restart, this exact unowned epoch can be hydrated.
            {
                let _configuration = state.pro.configuration.lock().await;
                ensure!(current(), "Account or project changed during return");
                lock(&state.pro.ownership).insert(
                    workspace.into(),
                    Ownership::Remote {
                        epoch: seen_epoch,
                        holder: holder.into(),
                    },
                );
                super::super::persist(state).await?;
            }
            ensure!(current(), "Account or project changed during return");
            if ambiguous != Some(seen_epoch) {
                let response = transport::request(
                    &config.keeper_url,
                    &format!("/v1/hosts/{}/http/api/v1/pro/handoff", host.id),
                    "POST",
                    &config.delegation.access_token,
                    Some(&json!({"workspace_id":workspace,"expected_epoch":seen_epoch})),
                )
                .await;
                ensure!(current(), "Account or project changed during return");
                match response {
                    Ok(response) if (200..300).contains(&response.status) => {
                        ambiguous = Some(seen_epoch);
                    }
                    Ok(response) if response.status == 409 => {
                        if serde_json::from_slice::<serde_json::Value>(&response.body)
                            .ok()
                            .is_some_and(|v| v["error"] == "workspace_busy")
                        {
                            return Ok(None);
                        }
                        // The rejected request did not move work. Refresh ownership
                        // immediately, before the resumed worker becomes idle again.
                    }
                    Ok(response)
                        if response.status == 503
                            && serde_json::from_slice::<serde_json::Value>(&response.body)
                                .ok()
                                .is_some_and(|v| v["error"] == "worker_asleep") => {}
                    Ok(response) if matches!(response.status, 502 | 504) => {
                        ambiguous = Some(seen_epoch);
                    }
                    Ok(_) => anyhow::bail!("Could not reconnect your work for return"),
                    Err(_) => {
                        // A lost response may hide a completed release. Read the
                        // authority until it resolves; never repeat that epoch's POST.
                        ambiguous = Some(seen_epoch);
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .context("Your work is taking longer to reconnect; Chimaera will try again")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::StatusCode,
        routing::{get, post},
        Json, Router,
    };
    use std::sync::{atomic::AtomicU64, Mutex};
    struct Fixture {
        epoch: AtomicU64,
        holder: Mutex<Option<String>>,
        posts: Mutex<Vec<u64>>,
        mode: &'static str,
        state: Arc<AppState>,
    }
    async fn read(State(f): State<Arc<Fixture>>) -> Json<serde_json::Value> {
        Json(
            json!({"workspace_id":"w-test","holder_id":lock(&f.holder).clone(),"epoch":f.epoch.load(Ordering::SeqCst),"expires_at":"2099-01-01T00:00:00Z","server_now":"2026-09-28T00:00:00Z","requires_fork":false}),
        )
    }
    async fn transfer(
        State(f): State<Arc<Fixture>>,
        Json(body): Json<serde_json::Value>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        let epoch = body["expected_epoch"].as_u64().unwrap();
        assert_eq!(body["workspace_id"], "w-test");
        lock(&f.posts).push(epoch);
        if f.mode == "busy" {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"workspace_busy"})),
            );
        }
        if f.mode == "changed" {
            *lock(&f.holder) = Some("another-device".into());
            f.epoch.store(4, Ordering::SeqCst);
            return (StatusCode::CONFLICT, Json(json!({})));
        }
        if f.mode == "canceled" {
            f.state.pro.generation.fetch_add(1, Ordering::SeqCst);
            return (StatusCode::CONFLICT, Json(json!({})));
        }
        if f.mode == "wake" && epoch == 3 {
            f.epoch.store(4, Ordering::SeqCst);
            return (StatusCode::CONFLICT, Json(json!({})));
        }
        *lock(&f.holder) = None;
        (
            if f.mode == "lost" {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::OK
            },
            Json(json!({})),
        )
    }
    async fn run(mode: &'static str) -> (Result<Option<u64>>, Vec<u64>) {
        let root = std::env::temp_dir().join(format!(
            "chimaera-handback-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = Arc::new(AppState::new(
            "local".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        lock(&state.pro.ownership).insert(
            "w-test".into(),
            Ownership::Remote {
                epoch: 3,
                holder: "worker".into(),
            },
        );
        let fixture = Arc::new(Fixture {
            epoch: AtomicU64::new(3),
            holder: Mutex::new(Some("worker".into())),
            posts: Mutex::new(Vec::new()),
            mode,
            state: state.clone(),
        });
        if mode == "released" {
            *lock(&fixture.holder) = None;
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/baton/w-test", get(read))
            .route(
                "/v1/hosts/worker-worker/http/api/v1/pro/handoff",
                post(transfer),
            )
            .with_state(fixture.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = Configure {
            account_id: None,
            role: Role::Device,
            endpoint: endpoint.clone(),
            keeper_url: endpoint,
            delegation: super::super::super::protocol::Delegation {
                access_token: "fixture".into(),
                expires_at: String::new(),
                scope: vec![],
                device_id: "home".into(),
            },
            hours_exhausted: false,
        };
        *lock(&state.pro.runtime) = Some(config.clone());
        let host = super::super::super::protocol::Host {
            id: "worker-worker".into(),
            kind: "worker".into(),
            alias: "Cloud".into(),
            status: "connected".into(),
        };
        let result = if mode == "released" {
            super::super::reconcile(&state, &config, "w-test")
                .await
                .map(|_| {
                    assert!(
                        matches!(
                            lock(&state.pro.ownership).get("w-test"),
                            Some(Ownership::Remote { .. })
                        ),
                        "lease polling must not resume stale local history"
                    );
                    None
                })
        } else {
            prepare(&state, &config, "w-test", &host, "worker", 3).await
        };
        let posts = lock(&fixture.posts).clone();
        server.abort();
        drop(fixture);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
        (result, posts)
    }
    #[tokio::test]
    async fn wake_epoch_change_retries_without_waiting_for_another_mirror_pass() {
        let (result, posts) = run("wake").await;
        assert_eq!(result.unwrap(), Some(4));
        assert_eq!(posts, [3, 4]);
    }
    #[tokio::test]
    async fn lost_release_reply_is_recovered_without_repeating_the_request() {
        let (result, posts) = run("lost").await;
        assert_eq!(result.unwrap(), Some(3));
        assert_eq!(posts, [3]);
    }
    #[tokio::test]
    async fn busy_other_owner_and_account_replacement_do_not_retry_work() {
        for mode in ["busy", "changed", "canceled"] {
            let (result, posts) = run(mode).await;
            if mode == "canceled" {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), None);
            }
            assert_eq!(posts, [3]);
        }
    }
    #[tokio::test]
    async fn remote_release_cannot_resume_the_old_local_history() {
        let (result, posts) = run("released").await;
        assert!(result.is_ok());
        assert!(posts.is_empty());
    }
}
