//! Per-project adoption. The webview supplies an identity, never a filesystem path.
use super::{daemon_request, Shell};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::{oneshot, Semaphore};

static OPEN_PROJECT: Semaphore = Semaphore::const_new(1);

/// Fixed codes for a project that did not open; the page maps each to a
/// sentence (`CloudProjects.svelte`). Anything unexpected is `FAILED`.
pub(in crate::shell) mod open_code {
    /// A saved copy or transfer is currently busy; requires another explicit action.
    pub const BUSY: &str = "project_busy";
    pub const FOLDER_NOT_EMPTY: &str = "project_folder_not_empty";
    pub const FOLDER_MISSING: &str = "project_folder_missing";
    /// Inside another project or Git repository.
    pub const FOLDER_NESTED: &str = "project_folder_nested";
    pub const ACCOUNT_CHANGED: &str = "account_changed";
    pub const ALREADY_OPENING: &str = "project_already_opening";
    pub const UNAVAILABLE: &str = "project_unavailable";
    /// The plan ended and the time to bring its cloud work home has passed.
    pub const RETURN_WINDOW_ENDED: &str = "return_window_ended";
    pub const FAILED: &str = "project_open_failed";
    pub const UPDATE_REQUIRED: &str = "project_copy_update_required";
    pub const CHECKPOINT_PENDING: &str = "project_checkpoint_pending";

    pub(super) fn of(error: &anyhow::Error) -> &'static str {
        let text = error.to_string();
        [
            BUSY,
            FOLDER_NOT_EMPTY,
            FOLDER_MISSING,
            FOLDER_NESTED,
            ACCOUNT_CHANGED,
            ALREADY_OPENING,
            UNAVAILABLE,
            RETURN_WINDOW_ENDED,
            UPDATE_REQUIRED,
            CHECKPOINT_PENDING,
        ]
        .into_iter()
        .find(|code| text == *code)
        .unwrap_or(FAILED)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct CloudProject {
    workspace_id: String,
    name: String,
    #[serde(default)]
    host_id: Option<String>,
    #[serde(default)]
    host_alias: Option<String>,
    local_root: Option<PathBuf>,
    #[serde(default)]
    destination_saved: bool,
    available: bool,
    error: Option<String>,
}
#[derive(Deserialize, Serialize)]
pub struct CloudProjectOpen {
    workspace_id: String,
    root: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_copy: Option<serde_json::Value>,
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
        .set_title(format!("Choose where to save {}", project.name))
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

/// The daemon needs an empty folder for the project itself, but picking a
/// parent such as ~/Projects is the natural gesture. An empty choice is used
/// as is; otherwise a new `<chosen>/<name>` folder is made (or an existing
/// empty one reused), with a numbered suffix if that name is taken.
/// Returns the destination and whether this call created it.
fn destination(chosen: &Path, name: &str) -> Result<(PathBuf, bool)> {
    if std::fs::read_dir(chosen)?.next().is_none() {
        return Ok((chosen.to_path_buf(), false));
    }
    let base = folder_name(name);
    for attempt in 1..=32 {
        let candidate = chosen.join(if attempt == 1 {
            base.clone()
        } else {
            format!("{base} {attempt}")
        });
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok((candidate, true)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(&candidate)?;
                if metadata.is_dir() && std::fs::read_dir(&candidate)?.next().is_none() {
                    return Ok((candidate, false));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!(open_code::FOLDER_NOT_EMPTY)
}

/// A folder name from the project's display name: no separators, control
/// characters or leading dots, bounded, never empty.
fn folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let bounded: String = cleaned
        .trim()
        .trim_start_matches('.')
        .trim()
        .chars()
        .take(80)
        .collect();
    if bounded.trim().is_empty() {
        "Project".into()
    } else {
        bounded.trim_end().into()
    }
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
            json!({"copy_version":1,"workspace_id":workspace_id,"expected_account_id":account,"expected_endpoint":endpoint}),
        )
    } else {
        selected.map(|root| json!({"copy_version":1,"workspace_id":workspace_id,"destination_root":root,"expected_account_id":account,"expected_endpoint":endpoint}))
    }
}
#[tauri::command]
pub async fn pro_copy_project(
    app: AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Shell>,
    workspace_id: String,
) -> Result<Option<CloudProjectOpen>, String> {
    async {
        let _operation = OPEN_PROJECT
            .try_acquire()
            .context(open_code::ALREADY_OPENING)?;
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
            .context(open_code::UNAVAILABLE)?;
        ensure!(
            generation == state.pro.generation(),
            open_code::ACCOUNT_CHANGED
        );
        let local = super::lock(&state.local).clone();
        let target = crate::wsl::CopyTarget::capture(&local)?;
        let saved = project.destination_saved || project.local_root.is_some();
        let chosen = if saved {
            None
        } else {
            choose(&app, &window, &project).await?
        };
        let selected = match chosen {
            Some(chosen) => {
                target.check(&super::lock(&state.local))?;
                target.validate_chosen(&chosen)?;
                let name = project.name.clone();
                let (path, _) =
                    tokio::task::spawn_blocking(move || destination(&chosen, &name)).await??;
                target.check(&super::lock(&state.local))?;
                Some(target.destination(path).await?)
            }
            None => None,
        };
        let Some(body) = import_body(&workspace_id, saved, selected, &account, &endpoint) else {
            return Ok(None);
        };
        // Signing out or switching accounts while a picker is open cannot import
        // a project under the replacement account's daemon delegation.
        ensure!(
            generation == state.pro.generation() && state.pro.client().await.is_some(),
            open_code::ACCOUNT_CHANGED
        );
        target.check(&super::lock(&state.local))?;
        // Even a failed request may have enrolled this inode durably. Keep the
        // destination for an explicit retry; an HTTP error cannot prove it is
        // unbound, and an empty replacement folder would no longer be ours.
        let opened = super::local_daemon_request(
            local,
            "POST",
            "/pro/projects/copy",
            Some(body),
            _operation,
        )
        .await?;
        target.check(&super::lock(&state.local))?;
        let imported = copy_ack(opened, &workspace_id)?;
        ensure!(
            generation == state.pro.generation(),
            open_code::ACCOUNT_CHANGED
        );
        let _ = app.emit("pro-changed", ());
        Ok(Some(imported))
    }
    .await
    .map_err(|error: anyhow::Error| open_code::of(&error).to_owned())
}

/// Older pages retain their command name but still receive safe copy behavior.
#[tauri::command]
pub async fn pro_open_cloud_project(
    app: AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Shell>,
    workspace_id: String,
) -> Result<Option<CloudProjectOpen>, String> {
    pro_copy_project(app, window, state, workspace_id).await
}

/// A successful status code cannot stand in for negotiated copy semantics.
fn copy_ack(value: serde_json::Value, workspace: &str) -> Result<CloudProjectOpen> {
    ensure!(
        value["copy_version"] == 1
            && matches!(value["state"].as_str(), Some("local_copy" | "owned_local")),
        open_code::UPDATE_REQUIRED
    );
    let copied = value["state"] == "local_copy";
    let owner_epoch = value["local_copy"]["owner_epoch"].clone();
    ensure!(
        owner_epoch.is_null()
            || owner_epoch
                .as_u64()
                .is_some_and(|epoch| epoch <= 9_007_199_254_740_991),
        open_code::UPDATE_REQUIRED
    );
    let mut result: CloudProjectOpen =
        serde_json::from_value(value).context(open_code::UPDATE_REQUIRED)?;
    // Project-role chrome derives only from the exact ACK, not an additive
    // role field an older daemon might supply without copy semantics.
    result.local_copy = copied.then(|| json!({"state":"ready","ready":true}));
    if copied && !owner_epoch.is_null() {
        result.local_copy.as_mut().unwrap()["owner_epoch"] = owner_epoch;
    }
    ensure!(
        result.workspace_id == workspace
            && crate::wsl::valid_daemon_root(&result.root)
            && !result.name.is_empty()
            && result.name.len() <= 1024,
        open_code::UPDATE_REQUIRED
    );
    Ok(result)
}

#[tauri::command]
pub async fn pro_take_over_project(
    app: AppHandle,
    state: tauri::State<'_, Shell>,
    workspace_id: String,
    expected_epoch: u64,
) -> Result<(), String> {
    async {
        let _operation = OPEN_PROJECT.try_acquire().context(open_code::ALREADY_OPENING)?;
        ensure!(!workspace_id.is_empty() && workspace_id.len() <= 128
            && workspace_id.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)), open_code::FAILED);
        ensure!(state.pro.client().await.is_some(), open_code::ACCOUNT_CHANGED);
        let (generation, account, endpoint) = {
            let _account_change = state.pro.operation.lock().await;
            let account = super::lock(&state.pro.account).as_ref().map(|account| account.account_id.clone()).context(open_code::ACCOUNT_CHANGED)?;
            let endpoint = state.pro.endpoint.clone().context(open_code::UNAVAILABLE)?;
            (state.pro.generation(), account, endpoint)
        };
        let answer = super::daemon_request_guarded(&state, "POST", "/pro/projects/takeover", Some(json!({"workspace_id":workspace_id,"expected_epoch":expected_epoch,"expected_account_id":account,"expected_endpoint":endpoint})), _operation).await?;
        ensure!(answer["workspace_id"] == workspace_id && answer["state"] == "owned_local", open_code::UPDATE_REQUIRED);
        ensure!(generation == state.pro.generation(), open_code::ACCOUNT_CHANGED);
        let _ = app.emit("pro-changed", ());
        Ok(())
    }.await.map_err(|error:anyhow::Error| open_code::of(&error).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_copy_keeps_admission_until_the_http_request_finishes() {
        use std::io::{Read, Write};
        use std::sync::Arc;
        use std::time::Duration;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local = crate::daemon::LocalDaemon {
            port: listener.local_addr().unwrap().port(),
            token: "synthetic-local-token".into(),
            build: None,
            outdated: false,
            live_sessions: None,
        };
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = tokio::task::spawn_blocking(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 4096);
            }
            assert!(request.starts_with(b"POST /api/v1/pro/projects/copy HTTP/1.1\r\n"));
            let mut body = [0; 4];
            stream.read_exact(&mut body).unwrap();
            assert_eq!(&body, b"null");
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let admission = Arc::new(Semaphore::new(1));
        let permit = admission.clone().try_acquire_owned().unwrap();
        let command = tokio::spawn(async move {
            super::super::local_daemon_request(local, "POST", "/pro/projects/copy", None, permit)
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), entered_rx)
            .await
            .unwrap()
            .unwrap();
        command.abort();
        assert!(command.await.unwrap_err().is_cancelled());
        assert!(admission.try_acquire().is_err());
        release_tx.send(()).unwrap();
        let _next = tokio::time::timeout(Duration::from_secs(5), admission.acquire())
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
    }

    #[test]
    fn a_parent_folder_gets_a_new_project_folder_and_an_empty_one_is_used() {
        let base = std::env::temp_dir().join(format!(
            "chimaera-picker-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&base).unwrap();
        // An empty choice is the project folder itself.
        assert_eq!(destination(&base, "Thesis").unwrap(), (base.clone(), false));
        std::fs::write(base.join("notes.txt"), "existing").unwrap();
        let (first, created) = destination(&base, "Thesis").unwrap();
        assert_eq!((first.clone(), created), (base.join("Thesis"), true));
        // The same name again: the empty folder is reused, not duplicated.
        assert_eq!(
            destination(&base, "Thesis").unwrap(),
            (first.clone(), false)
        );
        std::fs::write(first.join("draft.md"), "work").unwrap();
        assert_eq!(
            destination(&base, "Thesis").unwrap(),
            (base.join("Thesis 2"), true)
        );
        assert_eq!(
            destination(&base, "../escape").unwrap().0,
            base.join("-escape")
        );
        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(folder_name("  .hidden/name\n "), "hidden-name-");
        assert_eq!(folder_name("..."), "Project");
        assert_eq!(folder_name(&"x".repeat(200)).len(), 80);
    }

    #[test]
    fn only_fixed_open_codes_reach_the_page() {
        assert_eq!(
            open_code::of(&anyhow::anyhow!(open_code::BUSY)),
            open_code::BUSY
        );
        assert_eq!(
            open_code::of(&anyhow::anyhow!("Permission denied (os error 13)")),
            open_code::FAILED
        );
        assert_eq!(
            open_code::of(&anyhow::anyhow!("x").context(open_code::ALREADY_OPENING)),
            open_code::ALREADY_OPENING
        );
        for (detail, code) in [
            (
                "The cloud project is busy; wait for a pause and try again",
                open_code::BUSY,
            ),
            (
                "destination folder is not empty",
                open_code::FOLDER_NOT_EMPTY,
            ),
            ("saved folder is missing", open_code::FOLDER_MISSING),
            (
                "folder is inside a Git repository",
                open_code::FOLDER_NESTED,
            ),
            (
                "Account changed; open the project again",
                open_code::ACCOUNT_CHANGED,
            ),
            ("private path=/sensitive", open_code::FAILED),
        ] {
            assert_eq!(super::super::project_failure(detail), code, "{detail}");
        }
    }

    #[test]
    fn a_daemon_error_code_decides_the_open_failure_and_prose_only_an_old_daemon() {
        use super::super::open_failure;
        let failure = |body: serde_json::Value| open_failure(&serde_json::to_vec(&body).unwrap());
        for (daemon, page) in [
            ("folder_not_empty", open_code::FOLDER_NOT_EMPTY),
            ("folder_missing", open_code::FOLDER_MISSING),
            ("folder_moved", open_code::FOLDER_MISSING),
            ("folder_nested", open_code::FOLDER_NESTED),
            ("busy", open_code::BUSY),
            ("account_changed", open_code::ACCOUNT_CHANGED),
            ("unavailable", open_code::UNAVAILABLE),
            ("return_window_ended", open_code::RETURN_WINDOW_ENDED),
            // Reasons without a sentence of their own read as the generic line.
            ("privacy", open_code::FAILED),
            ("owned_elsewhere", open_code::FAILED),
            ("not_a_project", open_code::FAILED),
            ("a_code_from_a_newer_daemon", open_code::FAILED),
        ] {
            assert_eq!(failure(json!({"error_code": daemon})), page, "{daemon}");
        }
        // The code wins; prose that would classify differently is not consulted.
        assert_eq!(
            failure(json!({"error":"destination folder is not empty","error_code":"privacy"})),
            open_code::FAILED
        );
        // A daemon that predates the code is classified by its sentence.
        assert_eq!(
            failure(json!({"error":"destination folder is not empty","code":"other"})),
            open_code::FOLDER_NOT_EMPTY
        );
        assert_eq!(open_failure(b""), open_code::FAILED);
        assert_eq!(open_failure(b"<html>bad gateway</html>"), open_code::FAILED);
    }

    #[test]
    fn opening_requires_the_exact_copy_acknowledgment() {
        // A Windows native shell receives POSIX paths from its WSL daemon.
        let root = "/home/user/chimaera-copy-acknowledgment";
        let good = json!({"copy_version":1,"state":"local_copy","workspace_id":"w-one","root":root,"name":"One"});
        assert_eq!(
            copy_ack(good.clone(), "w-one").unwrap().local_copy,
            Some(json!({"state":"ready","ready":true}))
        );
        let mut owned = good.clone();
        owned["state"] = json!("owned_local");
        assert!(copy_ack(owned, "w-one").unwrap().local_copy.is_none());
        let mut released = good.clone();
        released["local_copy"] = json!({"state":"ready","ready":true,"owner_epoch":7});
        assert_eq!(
            copy_ack(released.clone(), "w-one").unwrap().local_copy,
            Some(json!({"state":"ready","ready":true,"owner_epoch":7}))
        );
        for epoch in [
            json!(-1),
            json!(1.5),
            json!("7"),
            json!(9_007_199_254_740_992_u64),
        ] {
            released["local_copy"]["owner_epoch"] = epoch;
            assert!(copy_ack(released.clone(), "w-one").is_err());
        }
        for (key, value) in [
            ("copy_version", json!(2)),
            ("state", json!("hydrating")),
            ("workspace_id", json!("w-other")),
            ("root", json!("relative")),
            ("root", json!(r"C:\Users\user\project")),
            ("root", json!("/home/user/../other")),
            ("root", json!("/home/user/\0other")),
            ("root", json!(format!("/{}", "x".repeat(4096)))),
        ] {
            let mut bad = good.clone();
            bad[key] = value;
            assert!(copy_ack(bad, "w-one").is_err());
        }
        let mut missing_ack = good;
        missing_ack.as_object_mut().unwrap().remove("copy_version");
        assert!(copy_ack(missing_ack, "w-one").is_err());
    }
    #[test]
    fn a_passive_catalog_project_needs_no_inferred_host_identity() {
        let project: CloudProject = serde_json::from_value(json!({"workspace_id":"w-one","name":"One","local_root":null,"destination_saved":true,"available":true,"error":null})).unwrap();
        assert!(project.host_id.is_none() && project.host_alias.is_none());
        // A pending saved binding is still reused, even with no ready registry root.
        assert!(import_body(
            &project.workspace_id,
            project.destination_saved,
            None,
            "account",
            "https://example.invalid"
        )
        .is_some());
    }
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
                json!({"copy_version":1,"workspace_id":"w-cloud","expected_account_id":"account","expected_endpoint":"https://example.invalid"})
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
                json!({"copy_version":1,"workspace_id":"w-cloud","destination_root":"/chosen/project","expected_account_id":"account","expected_endpoint":"https://example.invalid"})
            )
        );
    }
}
