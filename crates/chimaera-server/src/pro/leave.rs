//! Leaving this computer: the app quit. `POST /pro/leave` is the one thing the
//! native app sends (whenever Pro is active, with no question asked); which
//! work moves to the cloud is decided here, so the loopback harness drives
//! exactly what the app drives.
//!
//! A project moves when one of its Claude or Codex conversations is working
//! or waiting on the user (a permission or a question: the user will want to
//! answer it from a browser). A project whose conversations are all idle, or
//! whose working agents cannot run in the cloud, stays on this computer and
//! gets a current copy. Every enrolled project with something to say gets one
//! durable outcome (`moved`, `staying_here` with a closed reason, or `pending`
//! while its handover runs), shown in `/pro/status`, reported to the account
//! for the browser, and logged as one fixed-category line.
//!
//! The reply comes at once: the daemon outlives the app and finishes the
//! handover by itself within [`BUDGET`].
use super::{engine, protocol::Configure, routes, Ownership};
use crate::{lock, AppState};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

/// What the handover may spend after the app has gone. Generous: no OS is
/// about to freeze this computer, and a release the account confirms beats one
/// that has to lapse.
const BUDGET: Duration = Duration::from_secs(90);
/// The account's answer about the cloud's agent sign-ins; unknown past this.
const READINESS_WAIT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Where {
    Moved,
    StayingHere,
    Pending,
}

/// Why work stayed on this computer. A closed set of plain categories: never
/// error text or identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Reason {
    /// No conversation was working or waiting; a current copy went up.
    NothingRunning,
    /// The working agents are kinds the cloud does not run.
    AgentKindStaysHere,
    /// The agent is not signed in on the cloud machine.
    AgentNotConnectedInCloud,
    CloudTimeUsedUp,
    ConversationTooLarge,
    /// No conversation could be saved for the move (none has a transcript yet).
    ConversationNotSaved,
    CloudUnavailable,
    /// This computer does not hold the project's current copy yet.
    NotSyncedYet,
    Offline,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Self::NothingRunning => "nothing_running",
            Self::AgentKindStaysHere => "agent_kind_stays_here",
            Self::AgentNotConnectedInCloud => "agent_not_connected_in_cloud",
            Self::CloudTimeUsedUp => "cloud_time_used_up",
            Self::ConversationTooLarge => "conversation_too_large",
            Self::ConversationNotSaved => "conversation_not_saved",
            Self::CloudUnavailable => "cloud_unavailable",
            Self::NotSyncedYet => "not_synced_yet",
            Self::Offline => "offline",
        }
    }
}

/// A project's last leave outcome (persisted in `state.json`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Outcome {
    pub state: Where,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    /// Unix ms.
    pub at: u64,
}

/// What leaving does with one project.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Plan {
    Move,
    /// Stays here, with a fresh copy in the cloud.
    Copy(Reason),
}

/// Agent kinds the cloud can continue.
fn movable(kind: &str) -> bool {
    matches!(kind, "claude" | "codex")
}

/// The decision for one project: `active` are the agent kinds working or
/// waiting on the user there, `not_ready` the providers the account says are
/// not signed in on the cloud machine. Unknown readiness tries the move.
pub(super) fn plan(active: &[String], not_ready: &[String]) -> Plan {
    if active.is_empty() {
        return Plan::Copy(Reason::NothingRunning);
    }
    let mut movable = active.iter().filter(|kind| movable(kind)).peekable();
    if movable.peek().is_none() {
        return Plan::Copy(Reason::AgentKindStaysHere);
    }
    if movable.all(|kind| not_ready.contains(kind)) {
        return Plan::Copy(Reason::AgentNotConnectedInCloud);
    }
    Plan::Move
}

/// Why a handover that was tried did not happen, from its fixed categories.
pub(super) fn reason_for(error: &anyhow::Error) -> Reason {
    let text = error.to_string();
    if text.contains("too large") {
        return Reason::ConversationTooLarge;
    }
    match routes::error_code(error) {
        "quota" => Reason::ConversationTooLarge,
        "conversation_not_saved" => Reason::ConversationNotSaved,
        "cloud_provider_not_ready" => Reason::AgentNotConnectedInCloud,
        "checkpoint_pending" => Reason::NotSyncedYet,
        "timeout" => Reason::Offline,
        _ if text.contains("no conversation could move") => Reason::ConversationNotSaved,
        _ => Reason::CloudUnavailable,
    }
}

pub(super) fn outcome(state: &AppState, workspace: &str) -> Option<Outcome> {
    lock(&state.pro.left).get(workspace).copied()
}

/// Records and logs one outcome; the caller persists.
fn record(state: &AppState, workspace: &str, place: Where, reason: Option<Reason>) {
    {
        let mut left = lock(&state.pro.left);
        if left.len() >= 128 && !left.contains_key(workspace) {
            return;
        }
        left.insert(
            workspace.to_owned(),
            Outcome {
                state: place,
                reason,
                at: crate::session_view::now_ms(),
            },
        );
    }
    // One line per decision, written even in builds that log little else.
    tracing::info!(
        target: "chimaera_server::pro::leave",
        outcome = ?place,
        reason = reason.map(Reason::as_str),
        "leave decision"
    );
    state.changes.notify_waiters();
}

/// The providers the account says are not signed in on the cloud machine
/// (`GET /v2/cloud/agents`, from that machine's own last check). Empty when
/// unknown: unknown is tried, never assumed.
async fn not_ready(config: &Configure) -> Vec<String> {
    // Only an account with negotiated execution (continuity v2) answers.
    if config.execution.is_none() {
        return Vec::new();
    }
    let read = engine::account(config, "/v2/cloud/agents", "GET", None);
    let Ok(Ok(response)) = tokio::time::timeout(READINESS_WAIT, read).await else {
        return Vec::new();
    };
    if response.status != 200 {
        return Vec::new();
    }
    let Ok(body) = response.json::<serde_json::Value>() else {
        return Vec::new();
    };
    body["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .take(16)
        .filter(|row| row["signed_in"] == false)
        .filter_map(|row| row["id"].as_str())
        .filter(|id| movable(id))
        .map(str::to_owned)
        .collect()
}

/// Tells the account, so a browser can say where the work is. Best effort:
/// an unreachable account (offline) keeps the outcome here only.
async fn report(state: &AppState, config: &Configure, workspace: &str) {
    let Some(outcome) = outcome(state, workspace).filter(|_| config.execution.is_some()) else {
        return;
    };
    let body = json!({"state": outcome.state, "reason": outcome.reason});
    let path = format!("/v2/workspaces/{workspace}/leave");
    let sent = engine::account(config, &path, "PUT", Some(&body));
    match tokio::time::timeout(READINESS_WAIT, sent).await {
        Ok(Ok(response)) if (200..300).contains(&response.status) => {}
        _ => tracing::info!(
            target: "chimaera_server::pro::leave",
            "leave outcome not reported to the account"
        ),
    }
}

/// The app is leaving (it quit). Replies at once with each project's
/// decision; the handovers run on as an owned task.
pub(crate) async fn leave(State(state): State<Arc<AppState>>) -> Response {
    let Some(config) = lock(&state.pro.runtime).clone() else {
        return StatusCode::NO_CONTENT.into_response();
    };
    if config.role != super::protocol::Role::Device
        || config.delegation.workspace.is_some()
        || super::is_worker(&state)
    {
        return StatusCode::NO_CONTENT.into_response();
    }
    if state.daemon_extension.is_none() {
        tracing::info!(
            target: "chimaera_server::pro::leave",
            "leave refused: this daemon cannot hand work to the cloud"
        );
        return Json(json!({"leaving": [], "reason": "optional_runtime_unavailable"}))
            .into_response();
    }
    let refusal = if config.hours_exhausted {
        Some(Reason::CloudTimeUsedUp)
    } else if super::drain::draining(&state) || super::delegation_lapsed(&state) {
        Some(Reason::CloudUnavailable)
    } else {
        None
    };
    // Every enrolled project with work going on, or held here and copyable.
    let mut candidates = Vec::new();
    for workspace in lock(&state.workspaces).list().into_iter().take(128) {
        let id = workspace.id;
        let enrolled = lock(&state.pro.ownership).contains_key(&id);
        // A move still running from an earlier leave keeps its own outcome.
        let moving = outcome(&state, &id).is_some_and(|o| o.state == Where::Pending);
        if !enrolled
            || moving
            || !lock(&state.pro.authority).allows(&id)
            || super::parked(&state, &id)
        {
            continue;
        }
        let active = engine::leaving_agents(&state, &id);
        if routes::flushable(&state, &id) {
            candidates.push((id, active));
        } else if !active.is_empty()
            && !lock(&state.pro.preferences)
                .get(&id)
                .is_some_and(|p| p.never_mirror)
            && !matches!(
                lock(&state.pro.ownership).get(&id),
                Some(Ownership::Remote { .. })
            )
        {
            record(&state, &id, Where::StayingHere, Some(Reason::NotSyncedYet));
        }
    }
    let mut moves = Vec::new();
    let mut copies = Vec::new();
    if let Some(reason) = refusal {
        for (id, active) in &candidates {
            if !active.is_empty() {
                record(&state, id, Where::StayingHere, Some(reason));
            }
        }
    } else {
        let any_movable = candidates
            .iter()
            .any(|(_, active)| active.iter().any(|kind| movable(kind)));
        let not_ready = if any_movable {
            not_ready(&config).await
        } else {
            Vec::new()
        };
        for (id, active) in candidates {
            match plan(&active, &not_ready) {
                Plan::Move => {
                    record(&state, &id, Where::Pending, None);
                    moves.push(id);
                }
                Plan::Copy(reason) => {
                    record(&state, &id, Where::StayingHere, Some(reason));
                    copies.push(id);
                }
            }
        }
    }
    if let Err(error) = super::persist(&state).await {
        tracing::warn!(%error, "Could not save where work went when the app quit");
    }
    let decided: Vec<String> = lock(&state.pro.left).keys().cloned().collect();
    let reply = json!({"leaving": decided.iter().filter_map(|id| {
        let outcome = outcome(&state, id)?;
        Some(json!({"workspace_id": id, "state": outcome.state, "reason": outcome.reason}))
    }).collect::<Vec<_>>()});
    let owner = state.clone();
    tokio::spawn(async move {
        for id in moves.iter().chain(&copies) {
            report(&owner, &config, id).await;
        }
        hand_over(&owner, &config, moves).await;
        copy(&owner, &config, &copies).await;
    });
    Json(reply).into_response()
}

/// Moves `moves` (parked, as the quit handover always was) and records how
/// each one ended.
async fn hand_over(state: &Arc<AppState>, config: &Configure, moves: Vec<String>) {
    if moves.is_empty() {
        return;
    }
    let deadline = tokio::time::Instant::now() + BUDGET;
    let results =
        match routes::hand_over(state, config.clone(), deadline, true, Some(moves.clone())).await {
            Ok((flushes, unavailable)) => {
                let mut results: HashMap<String, anyhow::Result<()>> =
                    flushes.await.unwrap_or_default().into_iter().collect();
                for id in unavailable {
                    results.insert(id, Err(anyhow::anyhow!("checkpoint pending")));
                }
                results
            }
            Err(_) => moves
                .iter()
                .map(|id| (id.clone(), Err(anyhow::anyhow!("transfer busy"))))
                .collect(),
        };
    for id in &moves {
        let id = id.as_str();
        match results.get(id) {
            // Moved only when its handover kept the project parked: the
            // private snapshot refuses a quit handover that carries no
            // conversation, and a wake meanwhile unparks it.
            Some(Ok(())) if super::parked(state, id) => record(state, id, Where::Moved, None),
            // The app came back during the handover: the work is here.
            Some(Ok(())) => record(state, id, Where::StayingHere, None),
            Some(Err(error)) => record(state, id, Where::StayingHere, Some(reason_for(error))),
            None => record(
                state,
                id,
                Where::StayingHere,
                Some(Reason::CloudUnavailable),
            ),
        }
    }
    if let Err(error) = super::persist(state).await {
        tracing::warn!(%error, "Could not save where work went when the app quit");
    }
    for id in &moves {
        report(state, config, id).await;
    }
}

/// A current copy of each project that stays here, so the cloud has its
/// files; its agents keep running.
async fn copy(state: &Arc<AppState>, config: &Configure, copies: &[String]) {
    if copies.is_empty() {
        return;
    }
    let _jobs = state.pro.jobs.lock().await;
    let generation = state.pro.generation.load(Ordering::Acquire);
    for id in copies {
        if generation != state.pro.generation.load(Ordering::Acquire) {
            return;
        }
        if !routes::flushable(state, id) || !super::execution::lease_valid(state, id) {
            continue;
        }
        if let Err(error) = engine::snapshot(state, config, id, false).await {
            tracing::info!(
                target: "chimaera_server::pro::leave",
                category = engine::failure_code(&error),
                "the copy of a project that stays here failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(kinds: &[&str]) -> Vec<String> {
        kinds.iter().map(|kind| kind.to_string()).collect()
    }

    #[tokio::test]
    async fn a_daemon_without_pro_answers_nothing_and_writes_nothing() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-leave-free-{}",
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
        let response = leave(State(state.clone())).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(lock(&state.pro.left).is_empty());
        assert!(!state.pro.root.join("state.json").exists());
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn working_or_waiting_claude_and_codex_move() {
        assert_eq!(plan(&kinds(&["claude"]), &[]), Plan::Move);
        assert_eq!(plan(&kinds(&["codex", "gemini"]), &[]), Plan::Move);
    }

    #[test]
    fn idle_projects_and_other_agents_stay_with_a_copy() {
        assert_eq!(plan(&[], &[]), Plan::Copy(Reason::NothingRunning));
        assert_eq!(
            plan(&kinds(&["gemini", "agy"]), &[]),
            Plan::Copy(Reason::AgentKindStaysHere)
        );
    }

    #[test]
    fn a_provider_known_signed_out_in_the_cloud_keeps_its_work_here() {
        assert_eq!(
            plan(&kinds(&["claude"]), &kinds(&["claude"])),
            Plan::Copy(Reason::AgentNotConnectedInCloud)
        );
        // One agent the cloud can run is enough to move the project.
        assert_eq!(
            plan(&kinds(&["claude", "codex"]), &kinds(&["claude"])),
            Plan::Move
        );
        // Unknown readiness is tried, never assumed.
        assert_eq!(plan(&kinds(&["codex"]), &kinds(&["claude"])), Plan::Move);
    }

    #[test]
    fn failures_become_plain_reasons_without_their_text() {
        let reason = |text: &str| reason_for(&anyhow::anyhow!(text.to_owned()));
        assert_eq!(
            reason("no conversation could move: too large for the cloud copy"),
            Reason::ConversationTooLarge
        );
        assert_eq!(
            reason("no conversation could move"),
            Reason::ConversationNotSaved
        );
        assert_eq!(reason("workspace release failed"), Reason::CloudUnavailable);
        assert_eq!(
            reason("https://synthetic:secret@example.invalid"),
            Reason::CloudUnavailable
        );
        assert_eq!(
            serde_json::to_value(Outcome {
                state: Where::StayingHere,
                reason: Some(Reason::AgentNotConnectedInCloud),
                at: 1
            })
            .unwrap(),
            json!({"state": "staying_here", "reason": "agent_not_connected_in_cloud", "at": 1})
        );
        assert_eq!(
            serde_json::to_value(Outcome {
                state: Where::Moved,
                reason: None,
                at: 2
            })
            .unwrap(),
            json!({"state": "moved", "at": 2})
        );
    }
}
