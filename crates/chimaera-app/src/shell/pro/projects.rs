//! Per-project adoption. The webview supplies an identity, never a filesystem path.
use super::{daemon_request, Shell};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::{oneshot, Semaphore};

static OPEN_PROJECT: Semaphore = Semaphore::const_new(1);

#[derive(Clone, Deserialize, Serialize)]
pub struct CloudProject {
    workspace_id: String,
    name: String,
    host_id: String,
    host_alias: String,
    local_root: Option<PathBuf>,
    available: bool,
    error: Option<String>,
}
#[derive(Deserialize, Serialize)]
pub struct CloudProjectOpen {
    workspace_id: String,
    root: PathBuf,
    name: String,
}
#[derive(Deserialize)]
struct Listing {
    projects: Vec<CloudProject>,
    error: Option<String>,
}
async fn list(state: &Shell) -> Result<Vec<CloudProject>> {
    let result: Listing =
        serde_json::from_value(daemon_request(state, "GET", "/pro/projects", None).await?)?;
    ensure!(
        result.projects.len() <= 128,
        "Cloud project list exceeds limit"
    );
    // The daemon's listing error is diagnostic text, not UI copy.
    anyhow::ensure!(
        !result.projects.is_empty() || result.error.is_none(),
        "Cloud projects are unavailable right now."
    );
    Ok(result.projects)
}
#[tauri::command]
pub async fn pro_cloud_projects(
    state: tauri::State<'_, Shell>,
) -> Result<Vec<CloudProject>, String> {
    let generation = state.pro.generation();
    if state.pro.client().await.is_none() {
        return Ok(Vec::new());
    }
    let rows = list(&state)
        .await
        .map_err(|_| "Cloud projects are unavailable right now.")?;
    if generation != state.pro.generation() {
        return Ok(Vec::new());
    }
    Ok(rows)
}

async fn choose(
    app: &AppHandle,
    window: &tauri::WebviewWindow,
    project: &CloudProject,
) -> Result<Option<PathBuf>> {
    let (send, receive) = oneshot::channel();
    app.dialog()
        .file()
        .set_parent(window)
        .set_title(format!("Save {} in this folder", project.name))
        .set_can_create_directories(true)
        .pick_folder(move |folder| {
            let _ = send.send(folder);
        });
    let Some(folder) = receive
        .await
        .context("The folder picker closed unexpectedly")?
    else {
        return Ok(None);
    };
    Ok(Some(folder.into_path().context("Choose a local folder")?))
}

/// Kept separate from native UI so cancellation is tested at the import boundary.
fn import_body(
    workspace_id: &str,
    saved: bool,
    selected: Option<PathBuf>,
    account: &str,
    endpoint: &str,
) -> Option<serde_json::Value> {
    if saved {
        Some(
            json!({"workspace_id":workspace_id,"expected_account_id":account,"expected_endpoint":endpoint}),
        )
    } else {
        selected.map(|root| json!({"workspace_id":workspace_id,"destination_root":root,"expected_account_id":account,"expected_endpoint":endpoint}))
    }
}
#[tauri::command]
pub async fn pro_open_cloud_project(
    app: AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Shell>,
    workspace_id: String,
) -> Result<Option<CloudProjectOpen>, String> {
    async {
        let _operation = OPEN_PROJECT
            .try_acquire()
            .context("A project is already being opened")?;
        ensure!(
            !workspace_id.is_empty()
                && workspace_id.len() <= 128
                && workspace_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)),
            "Invalid project identity"
        );
        ensure!(state.pro.client().await.is_some(), "Sign in first");
        let (generation, account, endpoint) = {
            let _account_change = state.pro.operation.lock().await;
            let account = super::lock(&state.pro.account)
                .as_ref()
                .map(|account| account.account_id.clone())
                .context("Sign in first")?;
            let endpoint = state
                .pro
                .endpoint
                .clone()
                .context("Pro endpoint is unavailable")?;
            (state.pro.generation(), account, endpoint)
        };
        let project = list(&state)
            .await?
            .into_iter()
            .find(|row| row.workspace_id == workspace_id)
            .context("This cloud project is unavailable; refresh the list and try again")?;
        ensure!(
            generation == state.pro.generation(),
            "Account changed; open the project again"
        );
        let saved = project.local_root.is_some();
        let selected = if saved {
            None
        } else {
            choose(&app, &window, &project).await?
        };
        let Some(body) = import_body(&workspace_id, saved, selected, &account, &endpoint) else {
            return Ok(None);
        };
        // Signing out or switching accounts while a picker is open cannot import
        // a project under the replacement account's daemon delegation.
        ensure!(
            generation == state.pro.generation() && state.pro.client().await.is_some(),
            "Account changed; open the project again"
        );
        let imported = serde_json::from_value(
            daemon_request(&state, "POST", "/pro/projects/open", Some(body)).await?,
        )?;
        ensure!(
            generation == state.pro.generation(),
            "Account changed while the project was opening"
        );
        let _ = app.emit("pro-changed", ());
        Ok(Some(imported))
    }
    .await
    .map_err(|error: anyhow::Error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancel_never_produces_an_import_request() {
        assert!(
            import_body("w-cloud", false, None, "account", "https://example.invalid").is_none()
        );
    }
    #[test]
    fn existing_mapping_never_replaces_its_path() {
        assert_eq!(
            import_body(
                "w-cloud",
                true,
                Some(PathBuf::from("/another")),
                "account",
                "https://example.invalid"
            ),
            Some(
                json!({"workspace_id":"w-cloud","expected_account_id":"account","expected_endpoint":"https://example.invalid"})
            )
        );
        assert_eq!(
            import_body(
                "w-cloud",
                false,
                Some(PathBuf::from("/chosen/project")),
                "account",
                "https://example.invalid"
            ),
            Some(
                json!({"workspace_id":"w-cloud","destination_root":"/chosen/project","expected_account_id":"account","expected_endpoint":"https://example.invalid"})
            )
        );
    }
}
