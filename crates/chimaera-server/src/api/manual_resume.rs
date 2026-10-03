//! Explicit same-conversation restoration. The HTTP observer never owns the
//! child, filesystem work or admission quota after an effect has started.
use crate::{
    ledger::{self, LedgerEntry},
    AppState,
};
use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

fn refusal(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({"error":code}))).into_response()
}

fn failed(stage: &'static str) {
    // Fixed stage names aid fault-path tests without exposing native paths,
    // credentials or provider diagnostics in either logs or the HTTP reply.
    tracing::debug!(stage, "manual resume refused");
    #[cfg(test)]
    eprintln!("manual resume refused at {stage}");
}

pub(crate) async fn resume(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    #[cfg(not(unix))]
    {
        let _ = (state, id, request);
        refusal(StatusCode::CONFLICT, "manual_resume_unsupported")
    }
    #[cfg(unix)]
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let deferred = { crate::lock(&state.deferred_sessions).get(&id).cloned() };
        let entry = deferred.or_else(|| {
            ledger::snapshot(&state)
                .0
                .into_iter()
                .find(|entry| entry.id == id)
        });
        let Some(entry) = entry else {
            return refusal(StatusCode::NOT_FOUND, "manual_resume_missing");
        };
        if entry
            .manual_resume_reason
            .as_deref()
            .is_some_and(|reason| reason != "project_secrets_idle")
        {
            return refusal(StatusCode::CONFLICT, "manual_resume_unknown_reason");
        }
        let Some(agent) = &entry.agent else {
            return refusal(StatusCode::CONFLICT, "manual_resume_unqualified");
        };
        if agent.ui != chimaera_agent::model::SessionUi::Chat
            || ledger::manual::Receipt::for_entry(&entry).is_err()
        {
            return refusal(StatusCode::CONFLICT, "manual_resume_unqualified");
        }
        let Some(workspace) = crate::lock(&state.workspaces).get(&entry.workspace_id) else {
            return refusal(StatusCode::CONFLICT, "manual_resume_workspace_missing");
        };
        let Ok(dispatch) = crate::pro::mutation::Dispatch::capture(&state, &entry.workspace_id)
        else {
            return refusal(StatusCode::CONFLICT, "manual_resume_authority_changed");
        };
        static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
        let Ok(slot) = SLOTS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(64)))
            .clone()
            .try_acquire_owned()
        else {
            return refusal(StatusCode::TOO_MANY_REQUESTS, "manual_resume_busy");
        };
        match tokio::time::timeout(
            Duration::from_secs(2),
            axum::body::to_bytes(request.into_body(), 1),
        )
        .await
        {
            Ok(Ok(body)) if body.is_empty() => (),
            _ => return refusal(StatusCode::BAD_REQUEST, "manual_resume_body_not_empty"),
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _slot = slot;
            let outcome = owned(state, entry, workspace, dispatch, deadline).await;
            let _ = sender.send(outcome);
        });
        match tokio::time::timeout_at(deadline, receiver).await {
            Ok(Ok(Ok(row))) => Json(row).into_response(),
            Ok(Ok(Err(()))) => refusal(StatusCode::CONFLICT, "manual_resume_unavailable"),
            _ => refusal(StatusCode::CONFLICT, "manual_resume_pending"),
        }
    }
}

#[cfg(unix)]
async fn owned(
    state: Arc<AppState>,
    entry: LedgerEntry,
    workspace: crate::workspaces::Workspace,
    dispatch: crate::pro::mutation::Dispatch,
    deadline: tokio::time::Instant,
) -> Result<Value, ()> {
    let _configuration = tokio::time::timeout_at(
        deadline,
        crate::pro::manual_resume_configuration(&state).lock_owned(),
    )
    .await
    .map_err(|_| ())?;
    let turn = ledger::ResumeTurn::take(&state, &entry.id);
    let _turn = tokio::time::timeout_at(deadline, turn.wait())
        .await
        .map_err(|_| ())?;
    let _lifecycle =
        crate::chat::ChatSwitchGuard::acquire(&state, &entry.id, "manual-resume").ok_or(())?;
    let _mutation = dispatch.begin(&state).map_err(|_| ())?;
    let current = || -> Result<(), ()> {
        if tokio::time::Instant::now() >= deadline
            || state.stopping.load(std::sync::atomic::Ordering::Acquire)
            || crate::lock(&state.workspaces)
                .get(&workspace.id)
                .is_none_or(|now| now.root != workspace.root)
        {
            return Err(());
        }
        dispatch.check(&state).map_err(|_| ())
    };
    current()?;
    crate::pro::ensure_root(crate::pro::manual_resume_storage(&state))
        .await
        .map_err(|_| failed("storage_root"))?;
    let storage = state.clone();
    let mut receipts = tokio::task::spawn_blocking(move || ledger::manual::load(&storage))
        .await
        .map_err(|_| failed("receipt_worker"))?
        .map_err(|_| failed("receipt_read"))?;
    current()?;
    let deferred = crate::lock(&state.deferred_sessions)
        .get(&entry.id)
        .cloned();
    if deferred.is_none() {
        if !receipts.contains(&entry)
            || !tokio::time::timeout_at(
                deadline,
                state.chat.resumed_native_ready(
                    &entry.id,
                    entry.agent.as_ref().unwrap().resume.as_deref().unwrap(),
                ),
            )
            .await
            .map_err(|_| ())?
            .map_err(|_| ())?
        {
            return Err(());
        }
        return row(&state, &entry.id);
    }
    if deferred.as_ref() != Some(&entry)
        || entry.manual_resume_reason.as_deref() != Some("project_secrets_idle")
        || state.chat.get(&entry.id).is_some_and(|info| info.alive)
        || state.sessions.get(&entry.id).is_some_and(|info| info.alive)
        || state.chat.process_group(&entry.id).is_some()
    {
        return Err(());
    }
    receipts.retain(&state, &entry).map_err(|_| ())?;
    let storage = state.clone();
    let original = entry.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let native = crate::bundle::native_path(&storage, &original)?
            .ok_or_else(|| anyhow::anyhow!("manual native unavailable"))?;
        let (file, metadata) = crate::fs::open_regular(&native)?;
        anyhow::ensure!(
            metadata.len() > 0 && metadata.len() <= 100 * 1024 * 1024,
            "manual native unavailable"
        );
        file.sync_all()?;
        Ok(())
    })
    .await
    .map_err(|_| failed("native_worker"))?
    .map_err(|_| failed("native_sync"))?;
    current()?;
    // A positively reaped old registry row owns no process. Reusing its exact
    // ID opens the existing journal; remove() never removes journal bytes.
    state.chat.remove(&entry.id);
    // After spawn starts, every error is handled by this owned continuation.
    // It retains lifecycle/configuration/authority/quota until actual reaping.
    let spawned = dispatch
        .run(ledger::manual_resumption(
            entry.id.clone(),
            ledger::respawn(&state, &entry, workspace.clone()),
        ))
        .await;
    let mut pause = None;
    let result = async {
        spawned.map_err(|_| failed("spawn"))?;
        current()?;
        pause = Some(
            tokio::time::timeout_at(deadline, state.chat.pause_commands(&entry.id))
                .await
                .map_err(|_| ())?
                .map_err(|_| ())?,
        );
        let native = entry.agent.as_ref().unwrap().resume.as_deref().unwrap();
        loop {
            current()?;
            if tokio::time::timeout_at(deadline, state.chat.resumed_native_ready(&entry.id, native))
                .await
                .map_err(|_| ())?
                .map_err(|_| ())?
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::timeout_at(deadline, state.chat.sync_resumed_journal(&entry.id))
            .await
            .map_err(|_| ())?
            .map_err(|_| ())?;
        current()?;
        let storage = state.clone();
        tokio::task::spawn_blocking(move || ledger::manual::save(&storage, &receipts))
            .await
            .map_err(|_| ())?
            .map_err(|_| ())?;
        current()?;
        if crate::lock(&state.deferred_sessions).get(&entry.id) != Some(&entry) {
            return Err(());
        }
        crate::lock(&state.deferred_sessions).remove(&entry.id);
        durable(&state).await?;
        current()?;
        row(&state, &entry.id)
    }
    .await;
    if result.is_err() {
        crate::lock(&state.deferred_sessions).insert(entry.id.clone(), entry.clone());
        if let Some(pause) = &mut pause {
            pause.fence();
        }
        state.chat.fence(&entry.id);
        // A bound on the HTTP observer cannot become an untracked process.
        // Repeated exact fencing owns cleanup even if driver teardown stalls.
        while pause.as_ref().is_some_and(|pause| pause.cleanup_pending())
            || state.chat.process_group(&entry.id).is_some()
            || state.chat.get(&entry.id).is_some_and(|info| info.alive)
        {
            if let Some(pause) = &mut pause {
                pause.fence();
            }
            state.chat.fence(&entry.id);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Keep the input fence and actual owner until the retained manual row
        // is durable; a transient storage failure does not authorize a child.
        while durable(&state).await.is_err() {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    result
}

#[cfg(unix)]
async fn durable(state: &Arc<AppState>) -> Result<(), ()> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let (entries, links) = ledger::snapshot(&state);
        crate::lock(&state.ledger).write_maintenance_durable(&entries, &links)?;
        Ok(())
    })
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}
#[cfg(unix)]
fn row(state: &AppState, id: &str) -> Result<Value, ()> {
    crate::session_view::sessions_json(state)
        .into_iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
        .ok_or(())
}
