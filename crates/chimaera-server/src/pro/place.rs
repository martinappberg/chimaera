//! Where a synced project's work runs, and the user's two choices about it.
//!
//! Whoever holds a project's lease runs it, and this computer holds it while
//! its daemon can reach the account (`reach`). Nothing here depends on the
//! app being open. The user may still choose, per project: "Run in the
//! cloud" ([`run_in_cloud`]: handed over now and kept there, `parked`, until
//! "Run here") and "Run here" ([`run_here`]: brought back at its
//! conversation's next pause). When the cloud cannot run a project it was
//! given, the account says why (`Baton::reason`): the plain reason shows on
//! the status row, and a project this computer parked comes back by the
//! ordinary return ([`observed`]).
use super::{engine, protocol::Configure, routes, Ownership};
use crate::{lock, AppState};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

/// What a "Run in the cloud" handover may spend: nothing is about to freeze
/// this computer, and a release the account confirms beats one that lapses.
const BUDGET: Duration = Duration::from_secs(90);

/// Why work is not where it would be. A closed set of plain categories: never
/// error text or identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Reason {
    /// The agent is not signed in on the cloud machine.
    AgentNotConnectedInCloud,
    CloudTimeUsedUp,
    /// The cloud's storage allowance is full.
    CloudStorageFull,
    CloudUnavailable,
    /// A working conversation is too large to travel.
    ConversationTooLarge,
    /// A working conversation could not be saved for the move.
    ConversationNotSaved,
    /// This computer does not hold the project's current copy yet.
    NotSyncedYet,
}

impl Reason {
    fn parse(value: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(value.to_owned())).ok()
    }
}

/// A "Run in the cloud" refused before anything stopped: a conversation it
/// carries cannot travel, so the whole project stays and keeps running here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationStays {
    TooLarge,
    NotSaved,
}
impl std::fmt::Display for ConversationStays {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "a working conversation is too large for the cloud copy",
            Self::NotSaved => "a working conversation could not be saved for the move",
        })
    }
}
impl std::error::Error for ConversationStays {}

/// Agent kinds the cloud can continue.
pub(super) fn movable(kind: &str) -> bool {
    matches!(kind, "claude" | "codex")
}

/// Why a handover that was tried did not happen: the snapshot's typed refusal
/// first, then the transfer's fixed categories.
pub(super) fn reason_for(error: &anyhow::Error) -> Reason {
    if let Some(stays) = error.downcast_ref::<ConversationStays>() {
        return match stays {
            ConversationStays::TooLarge => Reason::ConversationTooLarge,
            ConversationStays::NotSaved => Reason::ConversationNotSaved,
        };
    }
    match routes::error_code(error) {
        "quota" => Reason::CloudStorageFull,
        "conversation_not_saved" => Reason::ConversationNotSaved,
        "cloud_provider_not_ready" => Reason::AgentNotConnectedInCloud,
        "checkpoint_pending" => Reason::NotSyncedYet,
        _ => Reason::CloudUnavailable,
    }
}

/// The plain reason a project's work is not where it would be, for its status
/// row: the account's (the cloud could not run it) or this computer's own (a
/// "Run in the cloud" that could not start). Hot state, bounded.
#[derive(Default)]
pub(super) struct Reasons(std::sync::Mutex<std::collections::HashMap<String, (Reason, bool)>>);
impl Reasons {
    fn set(&self, workspace: &str, reason: Reason, from_account: bool) {
        let mut reasons = lock(&self.0);
        if reasons.len() < 128 || reasons.contains_key(workspace) {
            reasons.insert(workspace.to_owned(), (reason, from_account));
        }
    }
    pub(super) fn get(&self, workspace: &str) -> Option<Reason> {
        lock(&self.0).get(workspace).map(|(reason, _)| *reason)
    }
    pub(super) fn clear(&self) {
        lock(&self.0).clear();
    }
}

/// One ownership read on a personal computer: the account's reason is kept
/// for the status row, and a project this computer parked (the user chose the
/// cloud) that the cloud cannot run is unparked, so it comes back by the
/// ordinary return at its next pause. Nothing else waits for an answer.
pub(super) fn observed(state: &Arc<AppState>, config: &Configure, baton: &super::protocol::Baton) {
    if config.role != super::protocol::Role::Device || config.delegation.workspace.is_some() {
        return;
    }
    let workspace = &baton.workspace_id;
    {
        let mut holders = lock(&state.pro.holders);
        match baton.holder_kind.as_deref() {
            Some(kind @ ("computer" | "cloud"))
                if holders.len() < 128 || holders.contains_key(workspace) =>
            {
                // A display name only: bounded and printable, never a path.
                let name = baton
                    .holder_name
                    .as_deref()
                    .filter(|name| name.len() <= 128 && !name.chars().any(char::is_control))
                    .map(str::to_owned);
                holders.insert(workspace.clone(), (kind.to_owned(), name));
            }
            _ => {
                holders.remove(workspace);
            }
        }
    }
    let reason = baton.reason.as_deref().and_then(Reason::parse);
    match reason {
        Some(reason) => state.pro.reasons.set(workspace, reason, true),
        None => {
            lock(&state.pro.reasons.0).retain(|id, (_, account)| id != workspace || !*account);
        }
    }
    let elsewhere = baton
        .holder_id
        .as_deref()
        .is_some_and(|holder| holder != config.delegation.device_id);
    if reason.is_some()
        && super::parked(state, workspace)
        && (elsewhere || baton.holder_id.is_none())
    {
        super::unpark(state, workspace);
        tracing::info!(
            target: "chimaera_server::pro::place",
            "the cloud cannot run a project this computer handed it; it comes back"
        );
        let owner = state.clone();
        tokio::spawn(async move {
            if let Err(error) = super::persist(&owner).await {
                tracing::warn!(%error, "Could not save that a project comes back here");
            }
        });
    }
}

/// Where a project's work runs, for its status row: `here`, `cloud`,
/// `computer` (another of the user's computers), or null when the project is
/// not synced. Local evidence only; the holder's kind comes from the last
/// ownership read.
pub(super) fn place(state: &AppState, workspace: &str) -> serde_json::Value {
    let ownership = lock(&state.pro.ownership).get(workspace).cloned();
    match ownership {
        None => serde_json::Value::Null,
        Some(Ownership::Remote { .. }) => match lock(&state.pro.holders).get(workspace) {
            Some((kind, name)) if kind == "computer" => {
                json!({"where": "computer", "computer": name})
            }
            _ => json!({"where": "cloud"}),
        },
        Some(_) => json!({"where": "here"}),
    }
}

/// Starts bringing one project back to this computer: unparked; a release
/// nobody took is re-acquired at its own epoch (`Transferring` →
/// `AwaitingVerification`, no install, no fork), one the cloud holds comes
/// home at its next pause (`reclaim`, `lazy_handback`, conflicts kept in both
/// versions). Returns whether its stopped sessions resume here at once (a
/// release that never went out). The caller persists.
fn bring_back(state: &AppState, workspace: &str) -> bool {
    super::unpark(state, workspace);
    {
        let mut ownership = lock(&state.pro.ownership);
        if let Some(Ownership::Transferring { epoch }) = ownership.get(workspace).cloned() {
            ownership.insert(
                workspace.to_owned(),
                Ownership::AwaitingVerification { epoch },
            );
        }
    }
    let mut reclaim = lock(&state.pro.reclaim);
    if reclaim.len() < 128 || reclaim.contains(workspace) {
        reclaim.insert(workspace.to_owned());
    }
    drop(reclaim);
    // The next lease tick runs a return pass for it at once.
    state.pro.return_pass.store(0, Ordering::Release);
    lock(&state.pro.release_pending).remove(workspace)
}

fn personal(config: Option<&Configure>) -> bool {
    config.is_some_and(|config| {
        config.role == super::protocol::Role::Device && config.delegation.workspace.is_none()
    })
}

/// Whether "Run here" applies to `workspace` on this computer: a personal
/// computer with the Runtime, the project registered and synced here, and its
/// work in the cloud or on its way there.
pub(super) fn may_run_here(state: &AppState, config: Option<&Configure>, workspace: &str) -> bool {
    personal(config)
        && state.daemon_extension.is_some()
        && lock(&state.workspaces).get(workspace).is_some()
        && lock(&state.pro.authority).allows(workspace)
        && !super::project_copy::copy_only(state, workspace)
        && !lock(&state.pro.preferences)
            .get(workspace)
            .is_some_and(|p| p.never_mirror)
        && !lock(&state.pro.reclaim).contains(workspace)
        && matches!(
            lock(&state.pro.ownership).get(workspace),
            Some(Ownership::Remote { .. } | Ownership::Transferring { .. })
        )
        && !lock(&state.pro.sleeping).contains(workspace)
}

/// `POST /pro/projects/{id}/here` ("Run here"): the project comes back at its
/// conversation's next pause through the usual return (exactly once, no new
/// turn, both versions kept where both changed). 202 when it is on its way,
/// 409 `not_elsewhere` when there is nothing to bring back, 404 for an
/// unknown project.
pub(crate) async fn run_here(
    State(state): State<Arc<AppState>>,
    Path(workspace): Path<String>,
) -> Response {
    if !super::valid_id(&workspace) || lock(&state.workspaces).get(&workspace).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let config = lock(&state.pro.runtime).clone();
    if !may_run_here(&state, config.as_ref(), &workspace) {
        return if lock(&state.pro.reclaim).contains(&workspace) {
            (StatusCode::ACCEPTED, Json(json!({"returning": true}))).into_response()
        } else {
            (
                StatusCode::CONFLICT,
                Json(json!({"error": "not_elsewhere"})),
            )
                .into_response()
        };
    }
    tracing::info!(
        target: "chimaera_server::pro::place",
        "a project was asked back to this computer"
    );
    // The latest computer the user ran it on is the one it returns to.
    lock(&state.pro.opened_here).insert(workspace.clone());
    lock(&state.pro.reasons.0).remove(&workspace);
    let resume_now = bring_back(&state, &workspace);
    if let Err(error) = super::persist(&state).await {
        tracing::warn!(%error, "Could not save that a project comes back here");
    }
    state.pro.renew_now.notify_waiters();
    if resume_now {
        let owner = state.clone();
        tokio::spawn(async move {
            if let Err(error) = crate::ledger::resume_deferred_workspace(&owner, &workspace).await {
                tracing::warn!(%error, "Could not resume a project's sessions after bringing it back");
            }
        });
    }
    (StatusCode::ACCEPTED, Json(json!({"returning": true}))).into_response()
}

/// Whether "Run in the cloud" applies to `workspace` now, by the handover's
/// own conditions: a configured personal computer with the Runtime and cloud
/// time left, no drain, the project held here under a valid lease and not
/// already handed over. Local only: nothing asks the account.
pub(super) fn may_run_in_cloud(
    state: &AppState,
    config: Option<&Configure>,
    workspace: &str,
) -> bool {
    personal(config)
        && config.is_some_and(|config| !config.hours_exhausted)
        && state.daemon_extension.is_some()
        && state.pro.configured.load(Ordering::Acquire)
        && !super::delegation_lapsed(state)
        && !super::execution::worker(state)
        && !super::drain::draining(state)
        && routes::flushable(state, workspace)
        && super::execution::lease_valid(state, workspace)
        && !super::parked(state, workspace)
        && !lock(&state.pro.sleeping).contains(workspace)
}

/// `POST /pro/projects/{id}/cloud` ("Run in the cloud"): the project is handed
/// over now and stays in the cloud (parked: this computer neither renews nor
/// takes it back) until "Run here" or until the cloud says it cannot run it.
/// Its working or waiting conversations travel together or not at all. 202
/// when the handover started (it runs as an owned task; the row's `place`
/// and `reason` say how it ended), 409 `cloud_time_used_up` or `not_here`,
/// 404 for an unknown project.
pub(crate) async fn run_in_cloud(
    State(state): State<Arc<AppState>>,
    Path(workspace): Path<String>,
) -> Response {
    if !super::valid_id(&workspace) || lock(&state.workspaces).get(&workspace).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let config = lock(&state.pro.runtime).clone();
    if personal(config.as_ref()) && config.as_ref().is_some_and(|config| config.hours_exhausted) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "cloud_time_used_up"})),
        )
            .into_response();
    }
    if !may_run_in_cloud(&state, config.as_ref(), &workspace) {
        return (StatusCode::CONFLICT, Json(json!({"error": "not_here"}))).into_response();
    }
    let Some(config) = config else {
        return (StatusCode::CONFLICT, Json(json!({"error": "not_here"}))).into_response();
    };
    // Claimed before answering, so a second request (or a sleep flush) sees
    // the project already on its way and nothing hands it over twice.
    if !lock(&state.pro.sleeping).insert(workspace.clone()) {
        return (StatusCode::CONFLICT, Json(json!({"error": "not_here"}))).into_response();
    }
    tracing::info!(
        target: "chimaera_server::pro::place",
        "a project was asked to run in the cloud"
    );
    lock(&state.pro.reasons.0).remove(&workspace);
    let handover = routes::Handover {
        deadline: tokio::time::Instant::now() + BUDGET,
        park: true,
    };
    let owner = state.clone();
    // Owned: the caller going away never cancels a handover halfway.
    tokio::spawn(async move {
        let started =
            routes::hand_over(&owner, config, handover, Some(vec![workspace.clone()])).await;
        let failure = match started {
            // `transfer_busy`: another handover held the jobs past the budget.
            Err(_) => Some(Reason::CloudUnavailable),
            Ok((flushes, unavailable)) => {
                if unavailable.contains(&workspace) {
                    Some(Reason::CloudUnavailable)
                } else {
                    flushes
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .find(|(id, _)| *id == workspace)
                        .and_then(|(_, result)| result.err())
                        .map(|error| reason_for(&error))
                }
            }
        };
        lock(&owner.pro.sleeping).remove(&workspace);
        if let Some(reason) = failure {
            owner.pro.reasons.set(&workspace, reason, false);
            tracing::info!(
                target: "chimaera_server::pro::place",
                reason = ?reason,
                "a project asked to run in the cloud stays on this computer"
            );
        }
    });
    (StatusCode::ACCEPTED, Json(json!({"moving": true}))).into_response()
}

/// On the cloud machine, after a project arrived and its sessions resumed:
/// the account hears whether the cloud runs the work (no reason) or cannot
/// because every conversation waits for an agent not signed in here, so a
/// browser can say why and a computer that parked it takes it back. Retried a
/// few times, since a lost answer would leave the work running nowhere.
pub(super) fn arrived(state: &AppState, workspace: &str, blocked: bool) {
    if !super::is_worker(state) {
        return;
    }
    let Some(config) = lock(&state.pro.runtime)
        .clone()
        .filter(|config| config.execution.is_some())
    else {
        return;
    };
    let reason = (blocked && !engine::live_agents(state, workspace))
        .then_some("agent_not_connected_in_cloud");
    let body = json!({ "reason": reason });
    let path = format!("/v2/workspaces/{workspace}/reason");
    tokio::spawn(async move {
        for attempt in 0..4u32 {
            let sent = engine::account(&config, &path, "PUT", Some(&body));
            if matches!(
                tokio::time::timeout(Duration::from_secs(10), sent).await,
                Ok(Ok(response)) if (200..300).contains(&response.status)
            ) {
                return;
            }
            tokio::time::sleep(Duration::from_secs(2u64 << attempt)).await;
        }
        tracing::info!(
            target: "chimaera_server::pro::place",
            "the cloud's answer about an arrived project was not reported"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> (Arc<AppState>, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "chimaera-place-{name}-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = Arc::new(crate::daemon_extension::with_inert_for_tests(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        )));
        (state, root)
    }

    struct Composed;
    impl crate::daemon_extension::Runtime for Composed {
        fn coordinate(
            &self,
            _owner: crate::daemon_extension::CoordinatorOwner,
        ) -> crate::daemon_extension::RuntimeFuture {
            Box::pin(async {})
        }
    }

    /// Review R3 S1: "Run in the cloud" claims its project before answering,
    /// so a second request (or a sleep flush) never hands it over twice, and
    /// only a wake (never a sleep or a handover) advances the generation a
    /// flush checks.
    #[tokio::test]
    async fn run_in_the_cloud_claims_its_project_before_answering() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-place-claim-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut state = crate::daemon_extension::with_inert_for_tests(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        state.daemon_extension = Some(Arc::new(Composed));
        let state = Arc::new(state);
        let workspace = lock(&state.workspaces).add(root.clone()).unwrap().id;
        crate::pro::install_execution_fixture(&state, &workspace, 3).unwrap();
        *lock(&state.pro.runtime) = Some(device());
        state.pro.configured.store(true, Ordering::Release);
        let generation = state.pro.sleep_generation.load(Ordering::Acquire);
        // Nothing yields between the two requests: the first's task has not
        // run, so only its synchronous claim can refuse the second.
        let first = run_in_cloud(State(state.clone()), Path(workspace.clone())).await;
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let second = run_in_cloud(State(state.clone()), Path(workspace.clone())).await;
        assert_eq!(second.status(), StatusCode::CONFLICT);
        // The handover ends (here: it cannot reach the cloud) and releases
        // its claim.
        for _ in 0..200 {
            if lock(&state.pro.sleeping).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(lock(&state.pro.sleeping).is_empty());
        // A sleep starting while a project is claimed leaves it to that
        // handover and advances nothing.
        lock(&state.pro.sleeping).insert(workspace.clone());
        let handover = routes::Handover {
            deadline: tokio::time::Instant::now() + Duration::from_secs(5),
            park: false,
        };
        let (flushes, unavailable) = routes::hand_over(&state, device(), handover, None)
            .await
            .unwrap();
        assert!(unavailable.is_empty());
        let flushed = flushes.await.unwrap();
        assert!(
            flushed.iter().all(|(id, _)| *id != workspace),
            "{flushed:?}"
        );
        assert_eq!(
            state.pro.sleep_generation.load(Ordering::Acquire),
            generation
        );
        drop(state);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_daemon_without_pro_has_no_choices_and_writes_nothing() {
        let (state, root) = fixture("free");
        let workspace = lock(&state.workspaces).add(root.clone()).unwrap().id;
        for response in [
            run_here(State(state.clone()), Path(workspace.clone())).await,
            run_in_cloud(State(state.clone()), Path(workspace.clone())).await,
        ] {
            assert_eq!(response.status(), StatusCode::CONFLICT);
        }
        assert_eq!(
            run_in_cloud(State(state.clone()), Path("w-unknown".into()))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert!(lock(&state.pro.parked).is_empty());
        assert!(lock(&state.pro.reclaim).is_empty());
        assert_eq!(place(&state, &workspace), serde_json::Value::Null);
        assert!(!state.pro.root.join("state.json").exists());
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failures_become_plain_reasons_from_their_type_without_their_text() {
        let typed = |stays| reason_for(&anyhow::Error::new(stays).context("snapshot failed"));
        assert_eq!(
            typed(ConversationStays::TooLarge),
            Reason::ConversationTooLarge
        );
        assert_eq!(
            typed(ConversationStays::NotSaved),
            Reason::ConversationNotSaved
        );
        let reason = |text: &str| reason_for(&anyhow::anyhow!(text.to_owned()));
        assert_eq!(reason("too large"), Reason::CloudUnavailable);
        assert_eq!(
            reason("https://synthetic:secret@example.invalid"),
            Reason::CloudUnavailable
        );
        assert_eq!(
            Reason::parse("cloud_storage_full"),
            Some(Reason::CloudStorageFull)
        );
        assert_eq!(Reason::parse("nothing_running"), None);
        assert_eq!(
            serde_json::to_value(Reason::AgentNotConnectedInCloud).unwrap(),
            json!("agent_not_connected_in_cloud")
        );
    }

    async fn call(state: &Arc<AppState>, method: &str, path: &str) -> StatusCode {
        use tower::ServiceExt;
        crate::app(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .method(method)
                    .uri(path)
                    .header("Authorization", format!("Bearer {}", state.token))
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from("{\"suitable\":true}"))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// Review R2 M1: a lid close cancelled its own handover because the app's
    /// periodic power report counted as "the app arrived". Nothing the app
    /// sends any more can move the sleep generation: the quit, wake and power
    /// routes are gone, and reading status changes nothing.
    #[tokio::test]
    async fn nothing_the_app_sends_can_cancel_a_sleep_handover() {
        let (state, root) = fixture("app");
        let before = state.pro.sleep_generation.load(Ordering::Acquire);
        for (method, path) in [
            ("PUT", "/api/v1/pro/power"),
            ("POST", "/api/v1/pro/wake"),
            ("POST", "/api/v1/pro/leave"),
        ] {
            let status = call(&state, method, path).await;
            assert!(
                status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path} answered {status}"
            );
        }
        assert_eq!(
            call(&state, "GET", "/api/v1/pro/status").await,
            StatusCode::OK
        );
        assert_eq!(state.pro.sleep_generation.load(Ordering::Acquire), before);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn device() -> Configure {
        serde_json::from_value(json!({
            "account_id":"a-fixture","role":"device","endpoint":"http://127.0.0.1:1",
            "keeper_url":"","delegation":{"access_token":"synthetic","expires_at":"2099-01-01T00:00:00Z",
            "scope":["baton","mirror","keeper"],"device_id":"d-home"}
        }))
        .unwrap()
    }
    fn read(value: serde_json::Value) -> super::super::protocol::Baton {
        serde_json::from_value(value).unwrap()
    }

    /// Review R2 M2: a browser opening a project made the computer hand it
    /// over, stopping a running terminal job. An ownership read now only
    /// records where the work is and why; it never stops, parks or hands
    /// anything over (an old account's `open_in_cloud` is ignored).
    #[tokio::test]
    async fn opening_a_project_elsewhere_never_stops_work_here() {
        let (state, root) = fixture("open");
        let config = device();
        lock(&state.pro.ownership).insert("w-a".into(), Ownership::Local { epoch: 3 });
        let generation = state.pro.sleep_generation.load(Ordering::Acquire);
        observed(
            &state,
            &config,
            &read(
                json!({"workspace_id":"w-a","holder_id":"d-home","epoch":3,"server_now":"2026-10-05T00:00:00Z",
                "requires_fork":false,"open_in_cloud":true,"holder_kind":"computer","holder_name":"Studio"}),
            ),
        );
        assert!(matches!(
            lock(&state.pro.ownership).get("w-a"),
            Some(Ownership::Local { epoch: 3 })
        ));
        assert!(lock(&state.pro.sleeping).is_empty());
        assert!(lock(&state.pro.parked).is_empty());
        assert_eq!(
            state.pro.sleep_generation.load(Ordering::Acquire),
            generation
        );
        assert_eq!(place(&state, "w-a"), json!({"where": "here"}));
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The cloud saying it cannot run a project the user sent there brings it
    /// back by the ordinary return; elsewhere the row names the place.
    #[tokio::test]
    async fn the_cloud_refusing_a_parked_project_unparks_it_and_says_why() {
        let (state, root) = fixture("refused");
        let config = device();
        lock(&state.pro.ownership).insert(
            "w-a".into(),
            Ownership::Remote {
                epoch: 4,
                holder: "m-cloud".into(),
            },
        );
        lock(&state.pro.parked).insert("w-a".into());
        observed(
            &state,
            &config,
            &read(
                json!({"workspace_id":"w-a","holder_id":"m-cloud","epoch":4,"server_now":"2026-10-05T00:00:00Z",
                "requires_fork":false,"holder_kind":"cloud","reason":"agent_not_connected_in_cloud"}),
            ),
        );
        assert!(lock(&state.pro.parked).is_empty());
        assert_eq!(
            state.pro.reasons.get("w-a"),
            Some(Reason::AgentNotConnectedInCloud)
        );
        assert_eq!(place(&state, "w-a"), json!({"where": "cloud"}));
        // Another computer holds it: named, and the reason clears.
        observed(
            &state,
            &config,
            &read(
                json!({"workspace_id":"w-a","holder_id":"d-other","epoch":5,"server_now":"2026-10-05T00:00:00Z",
                "requires_fork":false,"holder_kind":"computer","holder_name":"Studio"}),
            ),
        );
        assert_eq!(state.pro.reasons.get("w-a"), None);
        assert_eq!(
            place(&state, "w-a"),
            json!({"where": "computer", "computer": "Studio"})
        );
        drop(state);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = std::fs::remove_dir_all(root);
    }

    /// Review R2 M3: the computer resumed work the cloud was running. Waking
    /// now resumes nothing on sight: a project whose sleep handover did not
    /// finish waits for the lease loop to verify who holds it.
    #[tokio::test]
    async fn waking_never_resumes_work_the_cloud_may_be_running() {
        let (state, root) = fixture("woke");
        crate::pro::install_execution_fixture(&state, "w-a", 3).unwrap();
        crate::pro::lapse_execution_fixture(&state, "w-a");
        lock(&state.pro.ownership).insert("w-a".into(), Ownership::Transferring { epoch: 3 });
        lock(&state.pro.release_pending).insert("w-a".into());
        state.pro.reachable_since.store(1, Ordering::Release);
        let generation = state.pro.sleep_generation.load(Ordering::Acquire);
        routes::woke(&state).await;
        assert!(matches!(
            lock(&state.pro.ownership).get("w-a"),
            Some(Ownership::AwaitingVerification { epoch: 3 })
        ));
        assert!(lock(&state.pro.release_pending).is_empty());
        assert!(state.pro.sleep_generation.load(Ordering::Acquire) > generation);
        // The guard for bringing work home starts over.
        assert!(!super::super::reach::settled(&state));
        // Review R3 B3: no agent starts or resumes before the lease loop
        // verified the project is still this computer's; terminals still work.
        assert!(!crate::pro::may_execute(&state, "w-a"));
        assert!(crate::pro::may_run_shell(&state, "w-a"));
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Review R3 B2: signing out is remembered across a restart, and only
    /// that (not a configuration that has not arrived yet) lets interrupted
    /// sessions resume without the account; signing in again clears it.
    #[tokio::test]
    async fn signing_out_is_remembered_across_a_restart() {
        let (state, root) = fixture("signed-out");
        assert!(!state.pro.signed_out.load(Ordering::Acquire));
        assert_eq!(
            call(&state, "DELETE", "/api/v1/pro/configure").await,
            StatusCode::NO_CONTENT
        );
        drop(state);
        let restarted = Arc::new(crate::daemon_extension::with_inert_for_tests(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        )));
        assert!(restarted.pro.signed_out.load(Ordering::Acquire));
        drop(restarted);
        std::fs::remove_dir_all(root).unwrap();
    }
}
