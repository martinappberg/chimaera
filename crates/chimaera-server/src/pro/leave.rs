//! The app leaving this computer and coming back. While the native app is
//! here, work runs here; when it quits (`POST /pro/leave`) or the computer
//! sleeps (`/pro/sleep`), work moves to the cloud; when it comes back
//! (`/pro/wake`, or its first `/pro/power`), work comes home at the next
//! pause (`engine::lazy_handback`, after [`app_settled`]'s short guard).
//!
//! Leaving is decided here, so the loopback harness drives exactly what the
//! app drives. A project moves when one of its Claude or Codex conversations
//! is working or waiting on the user (a permission or a question: the user
//! will want to answer it from a browser), unless the account says that agent
//! is signed out in the cloud; it then carries every such conversation or
//! none (the private snapshot checks each can travel before anything stops,
//! [`ConversationStays`]). Idle projects stay held here with a current copy;
//! a browser that opens one afterwards asks the account, and this daemon
//! hands it over then ([`observed`], [`open_elsewhere`]).
//!
//! Never left running nowhere: a moved project's outcome stays `pending`
//! until the cloud says it runs the work (`moved`) or that it cannot
//! (`staying_here` with its reason); while the app is away this daemon then
//! takes the work back, as it does when the cloud has not taken it within
//! [`TAKE_BOUND`] ([`watch`], [`take_back`]).
//!
//! Every outcome is one durable entry per project (`moved`, `staying_here`
//! with a closed reason, or `pending`), shown in `/pro/status`, reported to
//! the account for the browser, and logged as one fixed-category line.
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

/// What a handover may spend after the app has gone. Generous: no OS is
/// about to freeze this computer, and a release the account confirms beats one
/// that has to lapse.
const BUDGET: Duration = Duration::from_secs(90);
/// One account call made by a leave or a watch; unknown past this.
const ACCOUNT_WAIT: Duration = Duration::from_secs(3);
/// How often a moved project's placement is read while its answer is awaited.
const WATCH_EVERY: Duration = Duration::from_secs(5);
/// The cloud has not taken a released project by then: the work comes back.
/// Covers starting a sleeping or new cloud machine and a lapsed lease's grace.
pub(super) const TAKE_BOUND: Duration = Duration::from_secs(240);
/// The cloud took the project but has not said it runs it by then: it comes
/// back. Covers the project's setup command (bounded at ten minutes there).
const CONFIRM_BOUND: Duration = Duration::from_secs(900);
/// A watch that never reaches the account gives up here (offline).
const WATCH_CAP: Duration = Duration::from_secs(1800);

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
    /// The cloud's storage allowance is full.
    CloudStorageFull,
    /// A working conversation is too large to travel.
    ConversationTooLarge,
    /// A working conversation could not be saved for the move.
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
            Self::CloudStorageFull => "cloud_storage_full",
            Self::ConversationTooLarge => "conversation_too_large",
            Self::ConversationNotSaved => "conversation_not_saved",
            Self::CloudUnavailable => "cloud_unavailable",
            Self::NotSyncedYet => "not_synced_yet",
            Self::Offline => "offline",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(value.to_owned())).ok()
    }
}

/// A project's last leave outcome (persisted in `state.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Outcome {
    pub state: Where,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    /// Unix ms.
    pub at: u64,
    /// What a move stopped here that the cloud does not continue: plain
    /// terminals (`terminal`) and agent kinds it does not run. The project's
    /// files moved, so nothing may keep writing them here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped: Vec<String>,
}

/// A quit handover refused before anything stopped: a conversation it exists
/// for cannot travel, so the whole project stays and keeps running here.
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

/// What leaving does with one project.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Plan {
    Move,
    /// Stays here, with a fresh copy in the cloud.
    Copy(Reason),
}

/// Agent kinds the cloud can continue.
pub(super) fn movable(kind: &str) -> bool {
    matches!(kind, "claude" | "codex")
}

/// The decision for one project: `active` are the agent kinds working or
/// waiting on the user there, `not_ready` the providers the account says are
/// not signed in on the cloud machine. Unknown readiness tries the move: the
/// cloud says so if it cannot run it, and the work then comes back.
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
        "timeout" => Reason::Offline,
        _ => Reason::CloudUnavailable,
    }
}

// --- the app here or away ---------------------------------------------------

/// The app arrived: it launched (`/pro/wake`, or its first `/pro/power` while
/// away) or the computer woke with it (`/pro/wake`, `woke`). A leave or sleep
/// handover still running stops before it parks or releases anything (the
/// sleep generation moves), and the last leave's outcomes are over: the work
/// comes home by the usual rules.
pub(super) fn app_arrived(state: &Arc<AppState>, woke: bool) {
    let was = state.pro.app_since.load(Ordering::Acquire);
    if woke || was == 0 {
        state
            .pro
            .app_since
            .store(super::now().max(1), Ordering::Release);
    }
    if was != 0 {
        return;
    }
    state.pro.sleep_generation.fetch_add(1, Ordering::AcqRel);
    let cleared: Vec<String> = lock(&state.pro.left).drain().map(|(id, _)| id).collect();
    if cleared.is_empty() {
        return;
    }
    state.changes.notify_waiters();
    let Some(config) = lock(&state.pro.runtime)
        .clone()
        .filter(|config| config.execution.is_some())
    else {
        return;
    };
    // The browser stops saying where the work went when the app left.
    tokio::spawn(async move {
        let sent = cleared.iter().map(|id| {
            let path = format!("/v2/workspaces/{id}/leave");
            let config = &config;
            async move {
                let _ = tokio::time::timeout(
                    ACCOUNT_WAIT,
                    engine::account(config, &path, "DELETE", None),
                )
                .await;
            }
        });
        futures::future::join_all(sent).await;
    });
}
/// The app is going: it quit, or the computer is about to sleep.
pub(super) fn app_away(state: &AppState) {
    state.pro.app_since.store(0, Ordering::Release);
}
pub(super) fn app_here(state: &AppState) -> bool {
    state.pro.app_since.load(Ordering::Acquire) != 0
}
/// The app has been here for the short guard before live cloud work moves
/// home (`engine::settle_seconds`).
pub(super) fn app_settled(state: &AppState) -> bool {
    let since = state.pro.app_since.load(Ordering::Acquire);
    since != 0 && super::now().saturating_sub(since) >= engine::settle_seconds()
}

/// The additive top-level `leaving` of `/pro/status`, for a personal
/// computer: whether this daemon can hand work to the cloud when the app
/// leaves. The app reads it when it attaches, so a daemon that cannot is
/// known then, not first at quit.
pub(super) fn readiness(state: &AppState, config: Option<&Configure>) -> serde_json::Value {
    match config {
        Some(config)
            if config.role == super::protocol::Role::Device
                && config.delegation.workspace.is_none() =>
        {
            if state.daemon_extension.is_some() {
                json!({"ready": true})
            } else {
                json!({"ready": false, "reason": "optional_runtime_unavailable"})
            }
        }
        _ => serde_json::Value::Null,
    }
}

// --- outcomes ---------------------------------------------------------------

pub(super) fn outcome(state: &AppState, workspace: &str) -> Option<Outcome> {
    lock(&state.pro.left).get(workspace).cloned()
}
fn pending(state: &AppState, workspace: &str) -> bool {
    outcome(state, workspace).is_some_and(|outcome| outcome.state == Where::Pending)
}

/// Records and logs one outcome, unless the account changed since the caller
/// started (`generation`); the caller persists.
fn record(
    state: &AppState,
    generation: u64,
    workspace: &str,
    place: Where,
    reason: Option<Reason>,
    stopped: Vec<String>,
) {
    if state.pro.generation.load(Ordering::Acquire) != generation {
        return;
    }
    // Projects no longer registered here say nothing any more (read before
    // `left` is taken: one lock at a time).
    let registered: std::collections::HashSet<String> = lock(&state.workspaces)
        .list()
        .into_iter()
        .map(|workspace| workspace.id)
        .collect();
    {
        let mut left = lock(&state.pro.left);
        left.retain(|id, _| registered.contains(id));
        if left.len() >= 128 && !left.contains_key(workspace) {
            return;
        }
        left.insert(
            workspace.to_owned(),
            Outcome {
                state: place,
                reason,
                at: crate::session_view::now_ms(),
                stopped,
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

async fn save(state: &AppState) {
    if let Err(error) = super::persist(state).await {
        tracing::warn!(%error, "Could not save where work went when the app left");
    }
}

/// Tells the account, so a browser can say where the work is. Best effort:
/// an unreachable account (offline) keeps the outcome here only.
async fn report(state: &AppState, config: &Configure, generation: u64, workspace: &str) {
    if state.pro.generation.load(Ordering::Acquire) != generation || config.execution.is_none() {
        return;
    }
    let Some(outcome) = outcome(state, workspace) else {
        return;
    };
    let body = json!({"state": outcome.state, "reason": outcome.reason});
    let path = format!("/v2/workspaces/{workspace}/leave");
    let sent = engine::account(config, &path, "PUT", Some(&body));
    match tokio::time::timeout(ACCOUNT_WAIT, sent).await {
        Ok(Ok(response)) if (200..300).contains(&response.status) => {}
        _ => tracing::info!(
            target: "chimaera_server::pro::leave",
            "leave outcome not reported to the account"
        ),
    }
}
async fn report_all(state: &AppState, config: &Configure, generation: u64, ids: &[String]) {
    futures::future::join_all(ids.iter().map(|id| report(state, config, generation, id))).await;
}

/// One of `set`'s entries, held by an owned task and removed when it ends,
/// however it ends (so no entry can outlive its task).
struct Claim {
    state: Arc<AppState>,
    watching: bool,
    ids: Vec<String>,
}
impl Claim {
    fn take(state: &Arc<AppState>, watching: bool, ids: Vec<String>) -> Self {
        let mut set = lock(if watching {
            &state.pro.watching
        } else {
            &state.pro.leaving
        });
        let ids = ids
            .into_iter()
            .filter(|id| set.insert(id.clone()))
            .collect();
        Self {
            state: state.clone(),
            watching,
            ids,
        }
    }
}
impl Drop for Claim {
    fn drop(&mut self) {
        let mut set = lock(if self.watching {
            &self.state.pro.watching
        } else {
            &self.state.pro.leaving
        });
        for id in &self.ids {
            set.remove(id);
        }
    }
}

/// The providers the account says are not signed in on the cloud machine
/// (`GET /v2/cloud/agents`, from that machine's own last check). Empty when
/// unknown: unknown is tried, never assumed, and the cloud says so if it
/// cannot run the work.
async fn not_ready(config: &Configure) -> Vec<String> {
    // Only an account with negotiated execution (continuity v2) answers.
    if config.execution.is_none() {
        return Vec::new();
    }
    let read = engine::account(config, "/v2/cloud/agents", "GET", None);
    let Ok(Ok(response)) = tokio::time::timeout(ACCOUNT_WAIT, read).await else {
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

/// What a move in `workspace` stops here that the cloud does not continue
/// (each kind once, at most eight): plain terminals and other agent kinds.
fn stays_behind(state: &AppState, workspace: &str) -> Vec<String> {
    let ids: Vec<String> = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, id)| id.as_str() == workspace)
        .map(|(id, _)| id.clone())
        .take(64)
        .collect();
    let mut kinds: Vec<String> = Vec::new();
    for id in ids {
        let agent = lock(&state.agents)
            .get(&id)
            .map(|record| record.kind.as_str().to_owned());
        let kind = if let Some(chat) = state.chat.get(&id) {
            if !chat.alive {
                continue;
            }
            agent.unwrap_or(chat.agent)
        } else if state.sessions.get(&id).is_some_and(|info| info.alive) {
            agent.unwrap_or_else(|| "terminal".into())
        } else {
            continue;
        };
        if !movable(&kind) && !kind.is_empty() && kind.len() <= 32 && !kinds.contains(&kind) {
            kinds.push(kind);
        }
        if kinds.len() >= 8 {
            break;
        }
    }
    kinds
}

// --- leaving ----------------------------------------------------------------

/// The app is leaving (it quit). Accepted at once: the app waits 1.5 s and a
/// client that goes away must not cancel anything, so the decision, every
/// account call and every write happen in one owned task. A wake (the app
/// coming back) at any point before a project parks keeps its work here.
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
    app_away(&state);
    let since = state.pro.sleep_generation.load(Ordering::Acquire);
    let generation = state.pro.generation.load(Ordering::Acquire);
    tokio::spawn(run(state.clone(), config, generation, since));
    (
        StatusCode::ACCEPTED,
        Json(json!({"leaving": [], "accepted": true})),
    )
        .into_response()
}

async fn run(state: Arc<AppState>, config: Configure, generation: u64, since: u64) {
    let current = |state: &AppState| {
        state.pro.generation.load(Ordering::Acquire) == generation
            && state.pro.sleep_generation.load(Ordering::Acquire) == since
    };
    let refusal = if config.hours_exhausted {
        Some(Reason::CloudTimeUsedUp)
    } else if super::drain::draining(&state) || super::delegation_lapsed(&state) {
        Some(Reason::CloudUnavailable)
    } else {
        None
    };
    // Every enrolled project with work going on, or held here and copyable.
    let mut candidates = Vec::new();
    let mut decided = Vec::new();
    // Listed first: `record` reads the registry too (a `for` keeps its
    // iterator expression's guard for the whole loop).
    let listed = lock(&state.workspaces).list();
    for workspace in listed.into_iter().take(128) {
        let id = workspace.id;
        let enrolled = lock(&state.pro.ownership).contains_key(&id);
        if !enrolled
            || !lock(&state.pro.authority).allows(&id)
            || super::parked(&state, &id)
            || lock(&state.pro.leaving).contains(&id)
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
            record(
                &state,
                generation,
                &id,
                Where::StayingHere,
                Some(Reason::NotSyncedYet),
                Vec::new(),
            );
            decided.push(id);
        }
    }
    let _claim = Claim::take(
        &state,
        false,
        candidates.iter().map(|(id, _)| id.clone()).collect(),
    );
    let mut moves = Vec::new();
    let mut copies = Vec::new();
    if let Some(reason) = refusal {
        for (id, active) in &candidates {
            if !active.is_empty() {
                record(
                    &state,
                    generation,
                    id,
                    Where::StayingHere,
                    Some(reason),
                    Vec::new(),
                );
                decided.push(id.clone());
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
        if !current(&state) {
            return;
        }
        for (id, active) in candidates {
            match plan(&active, &not_ready) {
                Plan::Move => {
                    let stopped = stays_behind(&state, &id);
                    record(&state, generation, &id, Where::Pending, None, stopped);
                    moves.push(id);
                }
                Plan::Copy(reason) => {
                    record(
                        &state,
                        generation,
                        &id,
                        Where::StayingHere,
                        Some(reason),
                        Vec::new(),
                    );
                    decided.push(id.clone());
                    copies.push(id);
                }
            }
        }
    }
    save(&state).await;
    move_projects(&state, &config, generation, moves, since, false).await;
    report_all(&state, &config, generation, &decided).await;
    copy(&state, &config, generation, &copies).await;
}

/// Moves `moves` (parked, as the quit handover always was), records how each
/// one ended and watches the cloud's answer for those that left.
async fn move_projects(
    state: &Arc<AppState>,
    config: &Configure,
    generation: u64,
    moves: Vec<String>,
    since: u64,
    opened: bool,
) {
    if moves.is_empty() {
        return;
    }
    let handover = routes::Handover {
        deadline: tokio::time::Instant::now() + BUDGET,
        park: true,
        opened,
        since: Some(since),
    };
    let results: HashMap<String, anyhow::Result<()>> =
        match routes::hand_over(state, config.clone(), handover, Some(moves.clone())).await {
            Ok((flushes, unavailable)) => {
                let mut results: HashMap<String, anyhow::Result<()>> =
                    flushes.await.unwrap_or_default().into_iter().collect();
                for id in unavailable {
                    results.insert(id, Err(anyhow::anyhow!("checkpoint pending")));
                }
                results
            }
            // The app came back before anything parked: its arrival already
            // cleared these outcomes, and the work never stopped here.
            Err("woke") => return,
            Err(_) => moves
                .iter()
                .map(|id| (id.clone(), Err(anyhow::anyhow!("transfer busy"))))
                .collect(),
        };
    let mut left = Vec::new();
    let mut stayed = Vec::new();
    for id in &moves {
        match results.get(id.as_str()) {
            // Parked: it left this computer. `pending` until the cloud says it
            // runs the work (or cannot); a wake meanwhile unparks it.
            Some(Ok(())) if super::parked(state, id) => left.push(id.clone()),
            // The app came back during the handover: the work is here.
            Some(Ok(())) => {}
            Some(Err(error)) => {
                record(
                    state,
                    generation,
                    id,
                    Where::StayingHere,
                    Some(reason_for(error)),
                    Vec::new(),
                );
                stayed.push(id.clone());
            }
            None => {
                record(
                    state,
                    generation,
                    id,
                    Where::StayingHere,
                    Some(Reason::CloudUnavailable),
                    Vec::new(),
                );
                stayed.push(id.clone());
            }
        }
    }
    save(state).await;
    report_all(state, config, generation, &stayed).await;
    for id in left {
        tokio::spawn(watch(state.clone(), config.clone(), generation, id));
    }
}

/// A current copy of each project that stays here, so the cloud has its
/// files; its agents keep running.
async fn copy(state: &Arc<AppState>, config: &Configure, generation: u64, copies: &[String]) {
    if copies.is_empty() {
        return;
    }
    let _jobs = state.pro.jobs.lock().await;
    for id in copies {
        if generation != state.pro.generation.load(Ordering::Acquire) || app_here(state) {
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

// --- sleep ------------------------------------------------------------------

/// The computer is about to sleep (`/pro/sleep`): the app is away and every
/// flushable project gets the same outcome a quit would give it, decided
/// without any account call (the OS deadline is tight). Returns the account
/// generation the outcomes belong to.
pub(super) fn sleeping(state: &AppState, refusal: Option<Reason>) -> u64 {
    app_away(state);
    let generation = state.pro.generation.load(Ordering::Acquire);
    // Listed first: `record` reads the registry too (a `for` keeps its
    // iterator expression's guard for the whole loop).
    let listed = lock(&state.workspaces).list();
    for workspace in listed.into_iter().take(128) {
        let id = workspace.id;
        if !routes::flushable(state, &id) || super::parked(state, &id) {
            continue;
        }
        let active = engine::leaving_agents(state, &id);
        match (refusal, plan(&active, &[])) {
            (Some(reason), Plan::Move) => record(
                state,
                generation,
                &id,
                Where::StayingHere,
                Some(reason),
                Vec::new(),
            ),
            (None, Plan::Move) => {
                let stopped = stays_behind(state, &id);
                record(state, generation, &id, Where::Pending, None, stopped);
            }
            (_, Plan::Copy(reason)) => record(
                state,
                generation,
                &id,
                Where::StayingHere,
                Some(reason),
                Vec::new(),
            ),
        }
    }
    generation
}
/// A sleep's flushes ended (those still running are resolved by the watch
/// when the computer is awake again): failures say why; then everything is
/// saved and, off the OS deadline, reported.
pub(super) async fn slept(
    state: &Arc<AppState>,
    generation: u64,
    results: &[(String, anyhow::Result<()>)],
) {
    for (id, result) in results {
        if let Err(error) = result {
            if pending(state, id) {
                record(
                    state,
                    generation,
                    id,
                    Where::StayingHere,
                    Some(reason_for(error)),
                    Vec::new(),
                );
            }
        }
    }
    save(state).await;
    let Some(config) = lock(&state.pro.runtime).clone() else {
        return;
    };
    let ids: Vec<String> = lock(&state.pro.left).keys().cloned().collect();
    let owner = state.clone();
    tokio::spawn(async move { report_all(&owner, &config, generation, &ids).await });
}

// --- after leaving: the account's answer -------------------------------------

/// Each lease tick's ownership read, for a personal computer: a browser asked
/// the cloud to run a project this computer holds while the app is away
/// (`open_in_cloud`), and a `pending` outcome nobody watches yet (a daemon
/// restart, a computer that slept) gets its watch.
pub(super) fn observed(state: &Arc<AppState>, config: &Configure, baton: &super::protocol::Baton) {
    if config.role != super::protocol::Role::Device
        || config.delegation.workspace.is_some()
        || state.daemon_extension.is_none()
    {
        return;
    }
    let workspace = baton.workspace_id.as_str();
    let generation = state.pro.generation.load(Ordering::Acquire);
    if pending(state, workspace)
        && !lock(&state.pro.leaving).contains(workspace)
        && !lock(&state.pro.watching).contains(workspace)
        && !lock(&state.pro.sleeping).contains(workspace)
    {
        tokio::spawn(watch(
            state.clone(),
            config.clone(),
            generation,
            workspace.to_owned(),
        ));
        return;
    }
    if !baton.open_in_cloud {
        return;
    }
    let refusal = if app_here(state) {
        Some("app_here")
    } else if baton.holder_id.as_deref() != Some(config.delegation.device_id.as_str())
        || super::owned_epoch(state, workspace) != Some(baton.epoch)
    {
        Some("not_held_here")
    } else {
        open_refusal(state, workspace)
    };
    if let Some(refusal) = refusal {
        tracing::debug!(
            target: "chimaera_server::pro::leave",
            refusal,
            "a browser's request to open a project in the cloud waits"
        );
    } else {
        let since = state.pro.sleep_generation.load(Ordering::Acquire);
        tokio::spawn(open_elsewhere(
            state.clone(),
            config.clone(),
            generation,
            since,
            workspace.to_owned(),
        ));
    }
}

/// A project held here may go to the cloud for a browser only when nothing
/// in it is working or waiting (that work would stop for a cloud the user
/// did not choose for it) and every session is at a pause. Otherwise why
/// not, as a fixed category.
fn open_refusal(state: &AppState, workspace: &str) -> Option<&'static str> {
    if super::parked(state, workspace)
        || pending(state, workspace)
        || lock(&state.pro.leaving).contains(workspace)
    {
        Some("leaving")
    } else if !routes::flushable(state, workspace)
        || !super::execution::lease_valid(state, workspace)
    {
        Some("not_flushable")
    } else if !engine::leaving_agents(state, workspace).is_empty() {
        Some("working")
    } else if !engine::at_pause(state, workspace) {
        Some("busy")
    } else {
        None
    }
}

/// A browser opened a project this computer holds while the app is away: it
/// is handed to the cloud like a quit's move (parked, watched), even with no
/// conversation in it, since the account wakes the cloud for that request.
async fn open_elsewhere(
    state: Arc<AppState>,
    config: Configure,
    generation: u64,
    since: u64,
    workspace: String,
) {
    // Checked again now (the tick that asked may be seconds old), then held.
    if open_refusal(&state, &workspace).is_some() {
        return;
    }
    let _claim = Claim::take(&state, false, vec![workspace.clone()]);
    if _claim.ids.is_empty() {
        return;
    }
    tracing::info!(
        target: "chimaera_server::pro::leave",
        "a browser opened a project held here while the app is away"
    );
    let stopped = stays_behind(&state, &workspace);
    record(
        &state,
        generation,
        &workspace,
        Where::Pending,
        None,
        stopped,
    );
    save(&state).await;
    move_projects(&state, &config, generation, vec![workspace], since, true).await;
}

/// What the cloud said about a project that left: the account's placement.
#[derive(Debug, Default, Deserialize)]
pub(super) struct Read {
    #[serde(default)]
    holder_id: Option<String>,
    #[serde(default)]
    route_host_id: Option<String>,
    #[serde(default)]
    leave: Option<ReadLeave>,
}
#[derive(Debug, Default, Deserialize)]
struct ReadLeave {
    state: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Wait,
    Moved,
    Back(Reason),
}

/// The decision for a project that left `waited` ago, from one read: the
/// cloud's own answer first (it runs the work, or cannot and why), then
/// another computer that took it, then the bounds.
pub(super) fn verdict(read: &Read, me: &str, waited: Duration) -> Verdict {
    match read.leave.as_ref().map(|leave| leave.state.as_str()) {
        Some("moved") => return Verdict::Moved,
        Some("staying_here") => {
            return Verdict::Back(
                read.leave
                    .as_ref()
                    .and_then(|leave| leave.reason.as_deref())
                    .and_then(Reason::parse)
                    .unwrap_or(Reason::CloudUnavailable),
            )
        }
        _ => {}
    }
    match read.holder_id.as_deref() {
        Some(holder) if holder != me => {
            if read
                .route_host_id
                .as_deref()
                .is_some_and(|route| route.starts_with("device-"))
            {
                Verdict::Moved
            } else if waited >= CONFIRM_BOUND {
                Verdict::Back(Reason::CloudUnavailable)
            } else {
                Verdict::Wait
            }
        }
        _ if waited >= TAKE_BOUND => Verdict::Back(Reason::CloudUnavailable),
        _ => Verdict::Wait,
    }
}

/// Waits for the cloud's answer about a project that left (or, after a
/// sleep, resolves it): `moved` once it runs the work; otherwise, while the
/// app is still away and the project parked, the work comes back here.
async fn watch(state: Arc<AppState>, config: Configure, generation: u64, workspace: String) {
    let claim = Claim::take(&state, true, vec![workspace.clone()]);
    if claim.ids.is_empty() {
        return;
    }
    let started = tokio::time::Instant::now();
    // A release the account confirmed made it `pending` there; a release
    // still lapsing has to say so first, or an older answer would be read.
    let mut said = !lock(&state.pro.release_pending).contains(&workspace);
    let me = config.delegation.device_id.clone();
    loop {
        tokio::time::sleep(WATCH_EVERY).await;
        if state.pro.generation.load(Ordering::Acquire) != generation
            || !pending(&state, &workspace)
        {
            return;
        }
        let waited = started.elapsed();
        if !said {
            let body = json!({"state": "pending"});
            let path = format!("/v2/workspaces/{workspace}/leave");
            said = matches!(
                tokio::time::timeout(ACCOUNT_WAIT, engine::account(&config, &path, "PUT", Some(&body))).await,
                Ok(Ok(response)) if (200..300).contains(&response.status)
            );
            if !said && waited < WATCH_CAP {
                continue;
            }
        }
        let path = format!("/v2/workspaces/{workspace}/placement");
        let read = tokio::time::timeout(ACCOUNT_WAIT, engine::account(&config, &path, "GET", None))
            .await
            .ok()
            .and_then(Result::ok)
            .filter(|response| response.status == 200)
            .and_then(|response| response.json::<Read>().ok());
        let decision = match read {
            Some(read) if said => verdict(&read, &me, waited),
            _ if waited >= WATCH_CAP => Verdict::Back(Reason::Offline),
            _ => Verdict::Wait,
        };
        match decision {
            Verdict::Wait => continue,
            Verdict::Moved => {
                let stopped = outcome(&state, &workspace)
                    .map(|outcome| outcome.stopped)
                    .unwrap_or_default();
                record(&state, generation, &workspace, Where::Moved, None, stopped);
                save(&state).await;
                return;
            }
            Verdict::Back(reason) => {
                if super::parked(&state, &workspace) && !app_here(&state) {
                    take_back(&state, &config, generation, &workspace, reason).await;
                } else {
                    record(
                        &state,
                        generation,
                        &workspace,
                        Where::StayingHere,
                        Some(reason),
                        Vec::new(),
                    );
                    save(&state).await;
                }
                return;
            }
        }
    }
}

/// The cloud did not take the work or cannot run it, and the app is away:
/// this computer takes it back now, as a wake would for this one project. A
/// release nobody took is re-acquired at its own epoch (no install, no fork:
/// the stopped conversations resume and continue their turn once); one the
/// cloud holds comes home at its next pause (`reclaim`, `lazy_handback`).
async fn take_back(
    state: &Arc<AppState>,
    config: &Configure,
    generation: u64,
    workspace: &str,
    reason: Reason,
) {
    if state.pro.generation.load(Ordering::Acquire) != generation {
        return;
    }
    tracing::info!(
        target: "chimaera_server::pro::leave",
        reason = reason.as_str(),
        "the cloud did not continue a project; it comes back here"
    );
    record(
        state,
        generation,
        workspace,
        Where::StayingHere,
        Some(reason),
        Vec::new(),
    );
    let resume_now = bring_back(state, workspace);
    save(state).await;
    state.pro.renew_now.notify_waiters();
    report(state, config, generation, workspace).await;
    if resume_now {
        resume_here(state, workspace);
    }
}

/// Starts bringing one project back to this computer, as a wake would for it
/// alone: unparked; a release nobody took is re-acquired at its own epoch
/// (`Transferring` → `AwaitingVerification`, no install, no fork), one the
/// cloud holds comes home at its next pause (`reclaim`, `lazy_handback`,
/// conflicts kept in both versions). Returns whether its stopped sessions
/// resume here at once (a release that never went out). The caller persists.
fn bring_back(state: &AppState, workspace: &str) -> bool {
    super::unpark(state, workspace);
    {
        let mut ownership = lock(&state.pro.ownership);
        match ownership.get(workspace).cloned() {
            Some(Ownership::Transferring { epoch }) => {
                ownership.insert(
                    workspace.to_owned(),
                    Ownership::AwaitingVerification { epoch },
                );
            }
            Some(Ownership::Remote { .. }) => {
                let mut reclaim = lock(&state.pro.reclaim);
                if reclaim.len() < 128 || reclaim.contains(workspace) {
                    reclaim.insert(workspace.to_owned());
                }
            }
            _ => {}
        }
    }
    // The next lease tick runs a return pass for it at once.
    state.pro.return_pass.store(0, Ordering::Release);
    lock(&state.pro.release_pending).remove(workspace)
}
fn resume_here(state: &Arc<AppState>, workspace: &str) {
    let owner = state.clone();
    let workspace = workspace.to_owned();
    tokio::spawn(async move {
        if let Err(error) = crate::ledger::resume_deferred_workspace(&owner, &workspace).await {
            tracing::warn!(%error, "Could not resume a project's sessions after bringing it back");
        }
    });
}

/// Whether "run here" applies to `workspace` on this computer (the additive
/// `run_here` of its `/pro/status` row): a personal computer with the
/// Runtime, the project registered and synced here, and its work in the
/// cloud or on its way there (parked, released, or held there).
pub(super) fn may_run_here(state: &AppState, config: Option<&Configure>, workspace: &str) -> bool {
    config.is_some_and(|config| {
        config.role == super::protocol::Role::Device && config.delegation.workspace.is_none()
    }) && state.daemon_extension.is_some()
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

/// `POST /pro/projects/{id}/here`: the user asked for one project to run on
/// this computer again ("Run here"), whatever the app's presence: it comes
/// back at its conversation's next pause through the usual return (exactly
/// once, no new turn, both versions kept where both changed). 202 when it is
/// on its way (`returning` on its row until it is here), 409 `not_elsewhere`
/// when there is nothing to bring back, 404 for an unknown project.
pub(crate) async fn run_here(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(workspace): axum::extract::Path<String>,
) -> Response {
    if !super::valid_id(&workspace) || lock(&state.workspaces).get(&workspace).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let config = lock(&state.pro.runtime).clone();
    if !may_run_here(&state, config.as_ref(), &workspace) {
        let returning = lock(&state.pro.reclaim).contains(&workspace);
        return if returning {
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
        target: "chimaera_server::pro::leave",
        "a project was asked back to this computer"
    );
    // The latest computer the user ran it on is the one it returns to.
    lock(&state.pro.opened_here).insert(workspace.clone());
    let resume_now = bring_back(&state, &workspace);
    // A Transferring project re-acquires through the lease loop, which only
    // `reclaim` holds to account; mark it returning either way.
    {
        let mut reclaim = lock(&state.pro.reclaim);
        if reclaim.len() < 128 || reclaim.contains(&workspace) {
            reclaim.insert(workspace.clone());
        }
    }
    save(&state).await;
    state.pro.renew_now.notify_waiters();
    if resume_now {
        resume_here(&state, &workspace);
    }
    (StatusCode::ACCEPTED, Json(json!({"returning": true}))).into_response()
}

/// A project taken back from the cloud is here again: the account hears its
/// outcome once more (it may have refused it while the cloud held it).
pub(super) async fn back_here(state: &AppState, config: &Configure, workspace: &str) {
    let generation = state.pro.generation.load(Ordering::Acquire);
    report(state, config, generation, workspace).await;
}

/// On the cloud machine, after a project arrived and its sessions resumed:
/// the account hears whether the cloud runs the work (`moved`) or cannot
/// (every conversation waits for an agent not signed in here), so a computer
/// waiting on that answer can take the work back. Best effort.
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
    let body = if blocked && !engine::live_agents(state, workspace) {
        json!({"state": "staying_here", "reason": Reason::AgentNotConnectedInCloud})
    } else {
        json!({"state": "moved"})
    };
    let path = format!("/v2/workspaces/{workspace}/leave");
    tokio::spawn(async move {
        let sent = engine::account(&config, &path, "PUT", Some(&body));
        if !matches!(
            tokio::time::timeout(Duration::from_secs(10), sent).await,
            Ok(Ok(response)) if (200..300).contains(&response.status)
        ) {
            tracing::info!(
                target: "chimaera_server::pro::leave",
                "the cloud's answer about an arrived project was not reported"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(kinds: &[&str]) -> Vec<String> {
        kinds.iter().map(|kind| kind.to_string()).collect()
    }

    fn fixture(name: &str) -> (Arc<AppState>, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "chimaera-leave-{name}-{}",
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
        (state, root)
    }

    #[tokio::test]
    async fn a_daemon_without_pro_answers_nothing_and_writes_nothing() {
        let (state, root) = fixture("free");
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
        // Wording alone never decides a conversation's fate any more.
        assert_eq!(reason("too large"), Reason::CloudUnavailable);
        assert_eq!(reason("workspace release failed"), Reason::CloudUnavailable);
        assert_eq!(
            reason("https://synthetic:secret@example.invalid"),
            Reason::CloudUnavailable
        );
        assert_eq!(
            Reason::parse("cloud_storage_full"),
            Some(Reason::CloudStorageFull)
        );
        assert_eq!(Reason::parse("something else"), None);
        assert_eq!(
            serde_json::to_value(Outcome {
                state: Where::StayingHere,
                reason: Some(Reason::AgentNotConnectedInCloud),
                at: 1,
                stopped: Vec::new(),
            })
            .unwrap(),
            json!({"state": "staying_here", "reason": "agent_not_connected_in_cloud", "at": 1})
        );
        assert_eq!(
            serde_json::to_value(Outcome {
                state: Where::Moved,
                reason: None,
                at: 2,
                stopped: kinds(&["terminal"]),
            })
            .unwrap(),
            json!({"state": "moved", "at": 2, "stopped": ["terminal"]})
        );
    }

    fn read(value: serde_json::Value) -> Read {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn moved_only_once_the_cloud_says_it_runs_the_work() {
        let short = Duration::from_secs(10);
        // Taken by the cloud machine, not confirmed yet: wait.
        let taken = read(
            json!({"holder_id":"m-1","route_host_id":"worker-m-1","leave":{"state":"pending"}}),
        );
        assert_eq!(verdict(&taken, "d-me", short), Verdict::Wait);
        let confirmed =
            read(json!({"holder_id":"m-1","route_host_id":"worker-m-1","leave":{"state":"moved"}}));
        assert_eq!(verdict(&confirmed, "d-me", short), Verdict::Moved);
        // Another of the user's computers took it.
        let computer = read(
            json!({"holder_id":"d-2","route_host_id":"device-d-2","leave":{"state":"pending"}}),
        );
        assert_eq!(verdict(&computer, "d-me", short), Verdict::Moved);
    }

    #[test]
    fn work_the_cloud_cannot_run_or_never_took_comes_back_with_its_reason() {
        let short = Duration::from_secs(10);
        let refused = read(
            json!({"holder_id":"m-1","route_host_id":"worker-m-1","leave":{"state":"staying_here","reason":"agent_not_connected_in_cloud"}}),
        );
        assert_eq!(
            verdict(&refused, "d-me", short),
            Verdict::Back(Reason::AgentNotConnectedInCloud)
        );
        let budget = read(
            json!({"holder_id":null,"leave":{"state":"staying_here","reason":"cloud_time_used_up"}}),
        );
        assert_eq!(
            verdict(&budget, "d-me", short),
            Verdict::Back(Reason::CloudTimeUsedUp)
        );
        // An unknown reason still brings it back.
        let unknown = read(json!({"leave":{"state":"staying_here","reason":"new_kind"}}));
        assert_eq!(
            verdict(&unknown, "d-me", short),
            Verdict::Back(Reason::CloudUnavailable)
        );
        // Released and nobody took it: within the bound wait, then back.
        let released = read(json!({"holder_id":null,"leave":{"state":"pending"}}));
        assert_eq!(verdict(&released, "d-me", short), Verdict::Wait);
        assert_eq!(
            verdict(&released, "d-me", TAKE_BOUND),
            Verdict::Back(Reason::CloudUnavailable)
        );
        // A lease of ours still lapsing counts the same.
        let lapsing = read(json!({"holder_id":"d-me","leave":{"state":"pending"}}));
        assert_eq!(
            verdict(&lapsing, "d-me", TAKE_BOUND),
            Verdict::Back(Reason::CloudUnavailable)
        );
        // Taken and never confirmed: back after the longer bound.
        let taken = read(json!({"holder_id":"m-1","leave":{"state":"pending"}}));
        assert_eq!(verdict(&taken, "d-me", TAKE_BOUND), Verdict::Wait);
        assert_eq!(
            verdict(&taken, "d-me", CONFIRM_BOUND),
            Verdict::Back(Reason::CloudUnavailable)
        );
    }

    #[tokio::test]
    async fn the_app_coming_back_cancels_a_leave_and_clears_its_outcomes() {
        let (state, root) = fixture("arrive");
        app_arrived(&state, false);
        assert!(app_here(&state));
        let before = state.pro.sleep_generation.load(Ordering::Acquire);
        app_away(&state);
        assert!(!app_here(&state) && !app_settled(&state));
        let since = state.pro.sleep_generation.load(Ordering::Acquire);
        lock(&state.pro.left).insert(
            "w-a".into(),
            Outcome {
                state: Where::Pending,
                reason: None,
                at: 1,
                stopped: Vec::new(),
            },
        );
        // A power report while away is the app attaching again.
        app_arrived(&state, false);
        assert!(app_here(&state));
        assert!(lock(&state.pro.left).is_empty());
        let after = state.pro.sleep_generation.load(Ordering::Acquire);
        assert_eq!(since, before);
        assert_ne!(after, since, "a leave that captured `since` cannot park");
        // While here, a second report changes nothing.
        app_arrived(&state, false);
        assert_eq!(state.pro.sleep_generation.load(Ordering::Acquire), after);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_claim_never_outlives_its_task() {
        let (state, root) = fixture("claim");
        let task = tokio::spawn({
            let state = state.clone();
            async move {
                let _claim = Claim::take(&state, false, vec!["w-a".into()]);
                std::future::pending::<()>().await;
            }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !lock(&state.pro.leaving).contains("w-a") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // A second leave skips the project while the first runs.
        assert!(Claim::take(&state, false, vec!["w-a".into()])
            .ids
            .is_empty());
        // Cancelled (the client went away, the daemon is stopping): released.
        task.abort();
        let _ = task.await;
        assert!(!lock(&state.pro.leaving).contains("w-a"));
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn sleep_records_each_projects_outcome_without_blocking() {
        let (state, root) = fixture("sleep");
        let folder = root.join("project");
        std::fs::create_dir_all(&folder).unwrap();
        let workspace = lock(&state.workspaces).add(folder).unwrap();
        lock(&state.pro.ownership).insert(workspace.id.clone(), Ownership::Local { epoch: 3 });
        app_arrived(&state, false);
        // The OS deadline is tight: deciding never waits on anything.
        let generation =
            tokio::time::timeout(Duration::from_secs(5), async { sleeping(&state, None) })
                .await
                .expect("sleep decided without blocking");
        assert!(
            !app_here(&state),
            "the app is away while the computer sleeps"
        );
        let outcome = outcome(&state, &workspace.id).unwrap();
        assert_eq!(
            (outcome.state, outcome.reason),
            (Where::StayingHere, Some(Reason::NothingRunning))
        );
        // A refusal is said for work that would have moved; nothing here works.
        assert_eq!(sleeping(&state, Some(Reason::CloudTimeUsedUp)), generation);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn one_project_comes_back_without_touching_the_others() {
        let (state, root) = fixture("here");
        let id = |name: &str| name.to_owned();
        lock(&state.pro.ownership).insert(id("w-released"), Ownership::Transferring { epoch: 4 });
        lock(&state.pro.ownership).insert(
            id("w-cloud"),
            Ownership::Remote {
                epoch: 5,
                holder: "m-1".into(),
            },
        );
        lock(&state.pro.ownership).insert(id("w-other"), Ownership::Transferring { epoch: 2 });
        for parked in ["w-released", "w-cloud", "w-other"] {
            lock(&state.pro.parked).insert(id(parked));
        }
        lock(&state.pro.release_pending).insert(id("w-released"));
        // A release that never went out resumes here at once, at its epoch.
        assert!(bring_back(&state, "w-released"));
        assert!(matches!(
            lock(&state.pro.ownership).get("w-released"),
            Some(Ownership::AwaitingVerification { epoch: 4 })
        ));
        // Work the cloud holds comes home at its next pause.
        assert!(!bring_back(&state, "w-cloud"));
        assert!(lock(&state.pro.reclaim).contains("w-cloud"));
        // Nothing else moved.
        assert!(lock(&state.pro.parked).contains("w-other"));
        assert!(matches!(
            lock(&state.pro.ownership).get("w-other"),
            Some(Ownership::Transferring { epoch: 2 })
        ));
        // Without the Runtime (a free daemon) the call never applies.
        assert!(!may_run_here(&state, None, "w-other"));
        let response = run_here(State(state.clone()), axum::extract::Path("w-none".into())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn status_says_when_this_daemon_cannot_hand_work_over() {
        let (state, root) = fixture("ready");
        let config: Configure = serde_json::from_value(json!({
            "account_id": "a-fixture", "endpoint": "http://127.0.0.1:9",
            "keeper_url": "", "role": "device",
            "delegation": {"access_token": "synthetic", "device_id": "d-home",
                "expires_at": "2099-01-01T00:00:00Z", "scope": ["baton", "mirror"]}
        }))
        .unwrap();
        assert_eq!(
            readiness(&state, Some(&config)),
            json!({"ready": false, "reason": "optional_runtime_unavailable"})
        );
        assert!(readiness(&state, None).is_null());
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
