//! Personal controls stay on local account Home, outside project transports.
mod ownership;
use super::{lock, Shell};
use chimaera_link::{
    project_secrets::{self as wire, CatalogPage, Command, CommandResult, Error},
    Client,
};
use ownership::owned;
use std::sync::{Arc, LazyLock};
use tauri::{AppHandle, Manager};
use tokio::sync::Semaphore;

static REQUESTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(16)));
static WRITERS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));

fn home(scope: &crate::shell::WindowScope) -> bool {
    scope.home_hub
        && scope.alias.is_none()
        && scope.ws.is_none()
        && !scope.navigation_pending
        && !scope.detached
}
fn check_window(state: &Shell, label: &str) -> Result<(), String> {
    if state.window_scope(label).as_ref().is_some_and(home) {
        Ok(())
    } else {
        Err("project_secrets_account_home_required".into())
    }
}
fn changed() -> String {
    "project_secrets_context_changed".into()
}
fn current(state: &Shell, generation: u64) -> Result<(), String> {
    if state.pro.generation() == generation && lock(&state.pro.client).is_some() {
        Ok(())
    } else {
        Err(changed())
    }
}
async fn context(client: &Client, expected: Option<&str>) -> Result<String, String> {
    let context = client
        .personal_control_context()
        .await
        .map_err(|e| e.to_string())?
        .context;
    if expected.is_some_and(|expected| expected != context) {
        return Err(changed());
    }
    Ok(context)
}

#[tauri::command]
pub async fn pro_project_secrets_catalog(
    state: tauri::State<'_, Shell>,
    window: tauri::WebviewWindow,
    after: Option<String>,
) -> Result<CatalogPage, String> {
    check_window(&state, window.label())?;
    let _request = REQUESTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::LimitReached.to_string())?;
    let (client, generation) = state
        .pro
        .client_now()
        .ok_or_else(|| Error::SignInRequired.to_string())?;
    let binding = context(&client, None).await?;
    current(&state, generation)?;
    let catalog = client
        .project_secrets_page(after.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    context(&client, Some(&binding)).await?;
    current(&state, generation)?;
    check_window(&state, window.label())?;
    Ok(CatalogPage {
        version: 1,
        context: binding,
        catalog,
    })
}

#[tauri::command]
pub async fn pro_project_secret_command(
    app: AppHandle,
    state: tauri::State<'_, Shell>,
    window: tauri::WebviewWindow,
    context_tag: String,
    payload: String,
) -> Result<CommandResult, String> {
    let command = Command::decode_owned(payload).map_err(|e| e.to_string())?;
    check_window(&state, window.label())?;
    if !wire::context(&context_tag) {
        return Err(Error::InvalidRequest.to_string());
    }
    let request = REQUESTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::LimitReached.to_string())?;
    let writer = WRITERS
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::LimitReached.to_string())?;
    let label = window.label().to_owned();
    let expected_generation = state.pro.generation();
    let operation = state.pro.operation.clone();
    // Detach before the first await. The account lock follows the complete Link
    // owner, including its cancellation-safe one-shot send; observer loss
    // cannot expose replacement credentials while that effect is unsettled.
    owned(operation, request, writer, async move {
        let state = app.state::<Shell>();
        check_window(&state, &label)?;
        current(&state, expected_generation)?;
        let (client, generation) = state
            .pro
            .client_now()
            .ok_or_else(|| Error::SignInRequired.to_string())?;
        context(&client, Some(&context_tag)).await?;
        current(&state, generation)?;
        check_window(&state, &label)?;
        let receipt = client
            .project_secret_command(command)
            .await
            .map_err(|e| e.to_string())?;
        context(&client, Some(&context_tag)).await?;
        current(&state, generation)?;
        Ok(CommandResult {
            version: 1,
            context: context_tag,
            receipt,
        })
    })
    .await
}

#[tauri::command]
pub async fn pro_project_secret_operation(
    state: tauri::State<'_, Shell>,
    window: tauri::WebviewWindow,
    context_tag: String,
    operation_id: String,
) -> Result<CommandResult, String> {
    check_window(&state, window.label())?;
    let _request = REQUESTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::LimitReached.to_string())?;
    if !wire::context(&context_tag) {
        return Err(Error::InvalidRequest.to_string());
    }
    let (client, generation) = state
        .pro
        .client_now()
        .ok_or_else(|| Error::SignInRequired.to_string())?;
    context(&client, Some(&context_tag)).await?;
    current(&state, generation)?;
    let receipt = client
        .project_secret_operation(&operation_id)
        .await
        .map_err(|e| e.to_string())?;
    context(&client, Some(&context_tag)).await?;
    current(&state, generation)?;
    check_window(&state, window.label())?;
    Ok(CommandResult {
        version: 1,
        context: context_tag,
        receipt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::WindowScope;
    #[test]
    fn secret_controls_require_the_current_local_home_scope() {
        let mut scope = WindowScope::new(None, None, "home".into());
        assert!(home(&scope));
        scope.navigation_pending = true;
        assert!(!home(&scope));
        assert!(!home(&WindowScope::new(
            None,
            Some("project".into()),
            "workspace".into()
        )));
        assert!(!home(&WindowScope::new(
            Some("cluster".into()),
            None,
            "remote".into()
        )));
        assert!(!home(&WindowScope::new_detached(
            None,
            None,
            "detached".into()
        )));
    }
}
