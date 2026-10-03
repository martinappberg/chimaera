//! Personal account controls, distinct from project daemon/runtime authority.
mod ownership;
use super::{lock, Shell};
use chimaera_link::providers::{CatalogPage, Command, CommandResult, Error, ModeReply, Original};
use std::sync::{Arc, LazyLock};
use tauri::{AppHandle, Manager};
use tokio::sync::Semaphore;

static REQUESTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(16)));
static WRITERS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));
fn check_window(state: &Shell, label: &str) -> Result<(), String> {
    // Local account permissions remain separate from every remote daemon UI.
    // A local project window can express personal intent without a project token.
    if state
        .window_scope(label)
        .is_some_and(|scope| scope.alias.is_none() && !scope.navigation_pending && !scope.detached)
    {
        Ok(())
    } else {
        Err("providers_personal_window_required".into())
    }
}
fn current(state: &Shell, generation: u64) -> Result<(), String> {
    if state.pro.generation() == generation && lock(&state.pro.client).is_some() {
        Ok(())
    } else {
        Err(Error::ContextChanged.to_string())
    }
}
fn slot() -> Result<tokio::sync::OwnedSemaphorePermit, String> {
    REQUESTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::LimitReached.to_string())
}
#[tauri::command]
pub async fn pro_personal_provider_mode(
    state: tauri::State<'_, Shell>,
    window: tauri::WebviewWindow,
) -> Result<ModeReply, String> {
    check_window(&state, window.label())?;
    let _slot = slot()?;
    let (client, generation) = state
        .pro
        .client_now()
        .ok_or_else(|| Error::SignInRequired.to_string())?;
    let reply = client
        .personal_provider_mode()
        .await
        .map_err(|e| e.to_string())?;
    current(&state, generation)?;
    check_window(&state, window.label())?;
    Ok(reply)
}
#[tauri::command]
pub async fn pro_personal_provider_catalog(
    state: tauri::State<'_, Shell>,
    window: tauri::WebviewWindow,
) -> Result<CatalogPage, String> {
    check_window(&state, window.label())?;
    let _slot = slot()?;
    let (client, generation) = state
        .pro
        .client_now()
        .ok_or_else(|| Error::SignInRequired.to_string())?;
    let reply = client
        .personal_provider_catalog()
        .await
        .map_err(|e| e.to_string())?;
    current(&state, generation)?;
    check_window(&state, window.label())?;
    Ok(reply)
}
#[tauri::command]
pub async fn pro_personal_provider_command(
    app: AppHandle,
    state: tauri::State<'_, Shell>,
    window: tauri::WebviewWindow,
    original: Original,
    payload: String,
) -> Result<CommandResult, String> {
    let command = Command::decode_owned(payload).map_err(|e| e.to_string())?;
    command
        .validate_original(&original)
        .map_err(|e| e.to_string())?;
    check_window(&state, window.label())?;
    let request = slot()?;
    let writer = WRITERS
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::LimitReached.to_string())?;
    let label = window.label().to_owned();
    let generation = state.pro.generation();
    let operation = state.pro.operation.clone();
    // Never wrap this join in an observer deadline: the operation lock and
    // budgets follow Link's actual detached HTTP owner, including cancellation.
    ownership::owned(operation, request, Some(writer), async move {
        let state = app.state::<Shell>();
        current(&state, generation)?;
        check_window(&state, &label)?;
        let (client, _) = state
            .pro
            .client_now()
            .ok_or_else(|| Error::SignInRequired.to_string())?;
        let reply = client
            .personal_provider_command(original, command)
            .await
            .map_err(|e| e.to_string())?;
        current(&state, generation)?;
        Ok(reply)
    })
    .await
}
#[tauri::command]
pub async fn pro_personal_provider_operation(
    state: tauri::State<'_, Shell>,
    window: tauri::WebviewWindow,
    original: Original,
) -> Result<CommandResult, String> {
    original.validate().map_err(|e| e.to_string())?;
    check_window(&state, window.label())?;
    let _slot = slot()?;
    let (client, generation) = state
        .pro
        .client_now()
        .ok_or_else(|| Error::SignInRequired.to_string())?;
    let reply = client
        .personal_provider_operation(&original, &original.operation_id)
        .await
        .map_err(|e| e.to_string())?;
    current(&state, generation)?;
    check_window(&state, window.label())?;
    Ok(reply)
}
#[tauri::command]
pub async fn pro_personal_provider_open(
    app: AppHandle,
    state: tauri::State<'_, Shell>,
    window: tauri::WebviewWindow,
    original: Original,
) -> Result<(), String> {
    original.validate().map_err(|e| e.to_string())?;
    check_window(&state, window.label())?;
    let request = slot()?;
    let generation = state.pro.generation();
    let operation = state.pro.operation.clone();
    let label = window.label().to_owned();
    ownership::owned(operation, request, None, async move {
        let state = app.state::<Shell>();
        current(&state, generation)?;
        check_window(&state, &label)?;
        let (client, _) = state
            .pro
            .client_now()
            .ok_or_else(|| Error::SignInRequired.to_string())?;
        let reply = client
            .personal_provider_operation(&original, &original.operation_id)
            .await
            .map_err(|e| e.to_string())?;
        current(&state, generation)?;
        check_window(&state, &label)?;
        let action = reply
            .attempt
            .action
            .ok_or_else(|| Error::StateChanged.to_string())?;
        // Link validated the fresh exact parent's phase/provider/fixed HTTPS
        // origin. No URL from IPC is accepted, including remembered actions.
        let url = action.url().to_owned();
        tokio::task::spawn_blocking(move || open::that(url))
            .await
            .map_err(|_| Error::Unavailable.to_string())?
            .map_err(|_| Error::Unavailable.to_string())?;
        Ok(())
    })
    .await
}
