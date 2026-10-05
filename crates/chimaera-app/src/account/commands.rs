//! Thin native IPC adapters. Stable names and arguments; absent owners have no side effects.
use super::types;
use tauri::Manager;
#[tauri::command]
pub async fn pro_status(app: tauri::AppHandle) -> Result<types::account::Status, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => owner.pro_status(app).await,
        None => Ok(types::account::Status::absent()),
    }
}
#[tauri::command]
pub async fn pro_refresh_account(
    app: tauri::AppHandle,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_refresh_account(app, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_sign_in(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    screen_hint: Option<types::auth::ScreenHint>,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_sign_in(app, window, screen_hint, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_take_return(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    expected_account_lifetime: Option<String>,
) -> Result<bool, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_take_return(app, window, expected_account_lifetime)
                .await
        }
        None => Ok(false),
    }
}
#[tauri::command]
pub async fn pro_cancel_sign_in(
    app: tauri::AppHandle,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_cancel_sign_in(app, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_sign_out(
    app: tauri::AppHandle,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => owner.pro_sign_out(app, expected_account_lifetime).await,
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_sign_out_everywhere(
    app: tauri::AppHandle,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_sign_out_everywhere(app, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_hosts(
    app: tauri::AppHandle,
    expected_account_lifetime: Option<String>,
) -> Result<Vec<types::account::KeptHost>, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => owner.pro_hosts(app, expected_account_lifetime).await,
        None => Ok(Vec::new()),
    }
}
#[tauri::command]
pub async fn pro_set_host_kept(
    app: tauri::AppHandle,
    alias: String,
    kept: bool,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_set_host_kept(app, alias, kept, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_devices(
    app: tauri::AppHandle,
    expected_account_lifetime: Option<String>,
) -> Result<Vec<chimaera_link::Device>, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => owner.pro_devices(app, expected_account_lifetime).await,
        None => Ok(Vec::new()),
    }
}
#[tauri::command]
pub async fn pro_revoke_device(
    app: tauri::AppHandle,
    device_id: String,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_revoke_device(app, device_id, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_mirror_status(
    app: tauri::AppHandle,
    expected_account_lifetime: Option<String>,
) -> Result<serde_json::Value, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_mirror_status(app, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_set_never_mirror(
    app: tauri::AppHandle,
    workspace_id: String,
    never_mirror: bool,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_set_never_mirror(app, workspace_id, never_mirror, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_billing_checkout(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    plan: chimaera_link::Plan,
    interval: chimaera_link::BillingInterval,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_billing_checkout(app, window, plan, interval, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_billing_portal(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    target: Option<chimaera_link::BillingPortalTarget>,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_billing_portal(app, window, target, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_cancel_billing(
    app: tauri::AppHandle,
    attempt_id: Option<u64>,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_cancel_billing(app, attempt_id, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_personal_provider_mode(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    expected_account_lifetime: Option<String>,
) -> Result<chimaera_link::providers::ModeReply, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_personal_provider_mode(app, window, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_personal_provider_catalog(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    expected_account_lifetime: Option<String>,
) -> Result<chimaera_link::providers::CatalogPage, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_personal_provider_catalog(app, window, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_personal_provider_command(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    original: chimaera_link::providers::Original,
    payload: String,
    expected_account_lifetime: Option<String>,
) -> Result<chimaera_link::providers::CommandResult, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_personal_provider_command(
                    app,
                    window,
                    original,
                    payload,
                    expected_account_lifetime,
                )
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_personal_provider_operation(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    original: chimaera_link::providers::Original,
    expected_account_lifetime: Option<String>,
) -> Result<chimaera_link::providers::CommandResult, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_personal_provider_operation(app, window, original, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_personal_provider_open(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    original: chimaera_link::providers::Original,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_personal_provider_open(app, window, original, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_project_secrets_catalog(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    after: Option<String>,
    expected_account_lifetime: Option<String>,
) -> Result<chimaera_link::project_secrets::CatalogPage, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_project_secrets_catalog(app, window, after, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_project_secret_command(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    context_tag: String,
    payload: String,
    expected_account_lifetime: Option<String>,
) -> Result<chimaera_link::project_secrets::CommandResult, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_project_secret_command(
                    app,
                    window,
                    context_tag,
                    payload,
                    expected_account_lifetime,
                )
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_project_secret_operation(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    context_tag: String,
    operation_id: String,
    expected_account_lifetime: Option<String>,
) -> Result<chimaera_link::project_secrets::CommandResult, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_project_secret_operation(
                    app,
                    window,
                    context_tag,
                    operation_id,
                    expected_account_lifetime,
                )
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_cloud_projects(
    app: tauri::AppHandle,
    expected_account_lifetime: Option<String>,
) -> Result<Vec<types::projects::CloudProject>, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_cloud_projects(app, expected_account_lifetime)
                .await
        }
        None => Ok(Vec::new()),
    }
}
#[tauri::command]
pub async fn pro_copy_project(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    workspace_id: String,
    expected_account_lifetime: Option<String>,
) -> Result<Option<types::projects::CloudProjectOpen>, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_copy_project(app, window, workspace_id, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_open_cloud_project(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    workspace_id: String,
    expected_account_lifetime: Option<String>,
) -> Result<Option<types::projects::CloudProjectOpen>, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_open_cloud_project(app, window, workspace_id, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_take_over_project(
    app: tauri::AppHandle,
    workspace_id: String,
    expected_epoch: u64,
    expected_account_lifetime: Option<String>,
) -> Result<(), String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_take_over_project(app, workspace_id, expected_epoch, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_cloud_status(
    app: tauri::AppHandle,
    expected_account_lifetime: Option<String>,
) -> Result<types::cloud::CloudStatus, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => owner.pro_cloud_status(app, expected_account_lifetime).await,
        None => Err(super::ABSENT.into()),
    }
}
#[tauri::command]
pub async fn pro_cloud_request(
    app: tauri::AppHandle,
    request: types::cloud::Request,
    expected_account_lifetime: Option<String>,
) -> Result<serde_json::Value, String> {
    let owner = app.state::<crate::shell::Shell>().pro.owner().cloned();
    match owner {
        Some(owner) => {
            owner
                .pro_cloud_request(app, request, expected_account_lifetime)
                .await
        }
        None => Err(super::ABSENT.into()),
    }
}
