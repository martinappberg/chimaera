//! Worker-only setup through the same authenticated daemon routes on every
//! client: machine facts and cloning a project. Agent sign-in is `providers`
//! (bounded connection jobs with one credential writer per provider).
pub(crate) mod providers;
use crate::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::io::AsyncReadExt;
static OPERATIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
pub(crate) fn active_operations() -> usize {
    1 - OPERATIONS.available_permits() + providers::active_operations()
}
pub(crate) fn enabled() -> bool {
    std::env::var("CHIMAERA_WORKER").as_deref() == Ok("1")
}
pub(crate) fn is_onboarding_workspace(workspace: &crate::workspaces::Workspace) -> bool {
    // Agent install terminals belong to worker setup, never to a project
    // that should be mirrored or offered for a local copy.
    workspace.cloud_internal
        || enabled()
            && std::env::var_os("HOME").is_some_and(|home| {
                workspace.root == PathBuf::from(home).join("projects/.chimaera-setup")
            })
}
fn unavailable() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error":"cloud setup is available on a cloud machine"})),
    )
        .into_response()
}
fn error(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error":message}))).into_response()
}
pub(crate) async fn info(State(state): State<Arc<AppState>>) -> Response {
    if !enabled() {
        return Json(json!({"available":false})).into_response();
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let public_key = async {
        let file = tokio::fs::File::open(home.as_ref()?.join(".ssh/id_ed25519.pub"))
            .await
            .ok()?;
        let mut bytes = Vec::new();
        file.take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() > 16 * 1024 {
            return None;
        }
        let text = String::from_utf8(bytes).ok()?;
        text.trim()
            .starts_with("ssh-ed25519 ")
            .then(|| text.trim().to_owned())
    }
    .await;
    let claude = crate::launcher::detect(&state, crate::agents::AgentKind::Claude, false)
        .await
        .path
        .is_ok();
    let codex = crate::launcher::detect(&state, crate::agents::AgentKind::Codex, false)
        .await
        .path
        .is_ok();
    Json(json!({"available":true,"home":home,"ssh_public_key":public_key,"claude_installed":claude,"codex_installed":codex})).into_response()
}
#[derive(Deserialize)]
pub(crate) struct Project {
    url: String,
    name: Option<String>,
}
fn repository(input: &str) -> Option<axum::http::Uri> {
    let url = input.parse::<axum::http::Uri>().ok()?;
    if input.len() > 4096
        || url.scheme_str() != Some("https")
        || url.host().is_none()
        || url.authority().is_some_and(|a| a.as_str().contains('@'))
        || url.query().is_some()
        || input.contains('#')
        || url.path() == "/"
    {
        return None;
    }
    Some(url)
}
fn project_name(input: &str) -> bool {
    !input.is_empty()
        && input.len() <= 80
        && !input.starts_with('.')
        && input
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

struct CloneChild {
    child: tokio::process::Child,
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
}
impl CloneChild {
    fn spawn(command: &mut tokio::process::Command) -> std::io::Result<Self> {
        #[cfg(unix)]
        command.process_group(0);
        let child = command.kill_on_drop(true).spawn()?;
        #[cfg(unix)]
        let group = child
            .id()
            .and_then(|id| rustix::process::Pid::from_raw(id as i32));
        Ok(Self {
            child,
            #[cfg(unix)]
            group,
        })
    }
    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(unix)]
        if let Some(group) = self.group {
            use rustix::process::{waitid, WaitId, WaitIdOptions};
            let mut changed =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
            loop {
                match waitid(
                    WaitId::Pid(group),
                    WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                ) {
                    Ok(Some(_)) => break,
                    Ok(None) => {}
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(error) => return Err(error.into()),
                }
                tokio::select! { _ = changed.recv() => {}, _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
            }
            // Signal while the leader is still waitable, then clear its
            // identity before reaping can allow the PID to be reused.
            self.stop_group();
        }
        self.child.wait().await
    }
    fn stop_group(&mut self) {
        #[cfg(unix)]
        if let Some(group) = self.group.take() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
}
impl Drop for CloneChild {
    fn drop(&mut self) {
        // Git forks remote helpers and index-pack; they must not keep writing
        // after cancellation releases the operation permit or removes staging.
        self.stop_group();
    }
}
async fn clone_status(command: &mut tokio::process::Command, limit: Duration) -> bool {
    let Ok(mut child) = CloneChild::spawn(command) else {
        return false;
    };
    let result = tokio::time::timeout(limit, child.wait()).await;
    child.stop_group();
    if !matches!(result, Ok(Ok(_))) {
        let _ = child.child.kill().await;
    }
    matches!(result, Ok(Ok(status)) if status.success())
}
pub(crate) async fn project(
    State(state): State<Arc<AppState>>,
    Json(input): Json<Project>,
) -> Response {
    if !enabled() {
        return unavailable();
    }
    let Ok(operation) = OPERATIONS.try_acquire() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"another cloud project is still cloning"})),
        )
            .into_response();
    };
    let Some(url) = repository(&input.url) else {
        return error("use an HTTPS repository URL without embedded credentials");
    };
    let name = input.name.unwrap_or_else(|| {
        url.path()
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("project")
            .trim_end_matches(".git")
            .into()
    });
    if !project_name(&name) {
        return error(
            "use a project name containing letters, numbers, dots, dashes or underscores",
        );
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return error("home directory is unavailable");
    };
    let operation = start_clone(
        state,
        home.join("projects"),
        name,
        url.to_string(),
        PathBuf::from("git"),
        Duration::from_secs(300),
        operation,
    );
    operation
        .await
        .unwrap_or_else(|_| error("clone operation could not finish"))
}
fn publish_clone(staging: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    // No check/rename gap and no plain-rename fallback: a newly arrived file
    // or empty directory belongs to its creator, even on clone completion.
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        staging,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(Into::into)
}
fn start_clone(
    state: Arc<AppState>,
    projects: PathBuf,
    name: String,
    url: String,
    git: PathBuf,
    limit: Duration,
    operation: tokio::sync::SemaphorePermit<'static>,
) -> tokio::task::JoinHandle<Response> {
    tokio::spawn(async move {
        // The detached operation owns the child, stage, publication and
        // registration through caller cancellation, including failure cleanup.
        let _operation = operation;
        clone_project(state, projects, name, url, git, limit).await
    })
}
async fn clone_project(
    state: Arc<AppState>,
    projects: PathBuf,
    name: String,
    url: String,
    git: PathBuf,
    limit: Duration,
) -> Response {
    if tokio::fs::create_dir_all(&projects).await.is_err() {
        return error("could not create projects directory");
    }
    let destination = projects.join(&name);
    if tokio::fs::try_exists(&destination).await.unwrap_or(true) {
        return error("a project with that name already exists");
    }
    // Stage on the same filesystem and publish only a complete clone. Git
    // receives argv directly; no URL or project name becomes shell code.
    let staging = projects.join(format!(".clone-{}", crate::agents::fresh_session_id()));
    if tokio::fs::create_dir(&staging).await.is_err() {
        return error("could not reserve clone directory");
    }
    let mut command = tokio::process::Command::new(git);
    command
        .args([
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "clone",
            "--",
        ])
        .arg(&url)
        .arg(&staging)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let cloned = clone_status(&mut command, limit).await;
    if !cloned {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return error("clone failed; connect GitHub or check the repository URL");
    }
    let from = staging.clone();
    let to = destination.clone();
    if !matches!(
        tokio::task::spawn_blocking(move || publish_clone(&from, &to)).await,
        Ok(Ok(()))
    ) {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return error("project name became unavailable");
    }
    let owner = state.clone();
    let root = destination.clone();
    match tokio::task::spawn_blocking(move || crate::lock(&owner.workspaces).add(root)).await {
        Ok(Ok(workspace)) => {
            state.changes.notify_waiters();
            Json(json!({"workspace_id":workspace.id,"root":destination})).into_response()
        }
        _ => error("project cloned but could not be registered"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn canceled_clone_stops_its_forked_writer() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "(printf ready; sleep 0.2; printf escaped) & wait"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = CloneChild::spawn(&mut command).unwrap();
        let mut output = child.child.stdout.take().unwrap();
        let mut ready = [0; 5];
        tokio::time::timeout(Duration::from_secs(2), output.read_exact(&mut ready))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&ready, b"ready");
        drop(child);
        let mut remainder = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), output.read_to_end(&mut remainder))
            .await
            .unwrap()
            .unwrap();
        assert!(
            remainder.is_empty(),
            "forked clone writer survived cancellation"
        );
    }
    fn clone_fixture() -> (Arc<AppState>, PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!(
            "chimaera-owned-clone-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(base.join("projects")).unwrap();
        let base = base.canonicalize().unwrap();
        let git = base.join("fake-git");
        std::fs::write(&git,"#!/bin/sh\nfor stage do :; done\nprintf cloned > \"$stage/payload\"\nprintf ready > \"$stage/../ready\"\nwhile [ ! -f \"$stage/../release\" ]; do /bin/sleep 0.02; done\n").unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            base.join("data"),
            base.join("config"),
        ));
        (state, base, git)
    }
    async fn wait_file(path: &std::path::Path) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn cancelled_cloud_clone_retains_admission_until_timeout_cleanup() {
        static ADMISSION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
        let (state, base, git) = clone_fixture();
        let owner = state.clone();
        let projects = base.join("projects");
        let permit = ADMISSION.acquire().await.unwrap();
        let caller = tokio::spawn(async move {
            start_clone(
                owner,
                projects,
                "picked".into(),
                "https://example.test/repo".into(),
                git,
                Duration::from_millis(250),
                permit,
            )
            .await
        });
        wait_file(&base.join("projects/ready")).await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(
            ADMISSION.try_acquire().is_err(),
            "cancelled observer released live clone admission"
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(permit) = ADMISSION.try_acquire() {
                    drop(permit);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!base.join("projects/picked").exists());
        assert!(!std::fs::read_dir(base.join("projects"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".clone-")));
        state
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
        std::fs::remove_dir_all(base).unwrap();
    }
    #[tokio::test]
    async fn completed_cloud_clone_preserves_an_arriving_empty_destination() {
        use std::os::unix::fs::MetadataExt;
        static ADMISSION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
        let (state, base, git) = clone_fixture();
        let worker = start_clone(
            state.clone(),
            base.join("projects"),
            "picked".into(),
            "https://example.test/repo".into(),
            git,
            Duration::from_secs(3),
            ADMISSION.acquire().await.unwrap(),
        );
        wait_file(&base.join("projects/ready")).await;
        let destination = base.join("projects/picked");
        std::fs::create_dir(&destination).unwrap();
        let inode = destination.metadata().unwrap().ino();
        std::fs::write(base.join("projects/release"), "").unwrap();
        assert_eq!(worker.await.unwrap().status(), StatusCode::BAD_REQUEST);
        assert_eq!(destination.metadata().unwrap().ino(), inode);
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        assert!(!std::fs::read_dir(base.join("projects"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".clone-")));
        // Exercise the actual publication primitive after its prior empty
        // name observation: even an empty newly created directory survives.
        let stage = base.join("projects/stage");
        std::fs::create_dir(&stage).unwrap();
        std::fs::write(stage.join("payload"), "cloned").unwrap();
        assert!(publish_clone(&stage, &destination).is_err());
        assert!(stage.join("payload").is_file());
        assert_eq!(destination.metadata().unwrap().ino(), inode);
        state
            .stopping
            .store(true, std::sync::atomic::Ordering::Release);
        std::fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn cloud_clone_cannot_accept_credentials_or_shell_protocols() {
        assert!(repository("https://github.com/example/project.git").is_some());
        for url in [
            "https://token@example.test/repo",
            "file:///etc/passwd",
            "ext::sh -c id",
            "https://example.test/repo?token=secret",
        ] {
            assert!(repository(url).is_none());
        }
        assert!(project_name("my-project"));
        assert!(!project_name("../escape"));
        assert!(!project_name("repo;id"));
    }
}
