//! Worker-only setup through the same authenticated daemon routes on every
//! client. Agent sign-in runs the vendor CLI itself in an ordinary terminal.
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
    1 - OPERATIONS.available_permits()
}
pub(crate) fn enabled() -> bool {
    std::env::var("CHIMAERA_WORKER").as_deref() == Ok("1")
}
pub(crate) fn is_onboarding_workspace(workspace: &crate::workspaces::Workspace) -> bool {
    // Provider login terminals belong to worker setup, never to a project
    // that should be mirrored or offered for a local copy.
    enabled()
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
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
pub(crate) struct Onboard {
    agent: String,
}
pub(crate) async fn onboard(
    State(state): State<Arc<AppState>>,
    Json(input): Json<Onboard>,
) -> Response {
    if !enabled() {
        return unavailable();
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return error("home directory is unavailable");
    };
    let (name, script) = match input.agent.as_str() {
        "claude" | "codex" => {
            let kind = if input.agent == "claude" {
                crate::agents::AgentKind::Claude
            } else {
                crate::agents::AgentKind::Codex
            };
            let detection = crate::launcher::detect(&state, kind, false).await;
            let (install, path) = match detection.path {
                Ok(path) => (String::new(), path),
                Err(_) => (
                    crate::runtimes::install_script(kind, &state.managed_root).unwrap_or_default(),
                    state.managed_root.join("bin").join(kind.as_str()),
                ),
            };
            let arguments = if input.agent == "codex" {
                " login --device-auth"
            } else {
                ""
            };
            (
                format!("Sign in to {}", kind.product_name()),
                format!(
                    "{install}\nexec {}{arguments}",
                    quote(&path.to_string_lossy())
                ),
            )
        }
        "github" => (
            "Connect GitHub".into(),
            "gh auth login --hostname github.com --git-protocol https --web && gh auth setup-git"
                .into(),
        ),
        _ => return error("choose claude, codex or github"),
    };
    let setup = home.join("projects/.chimaera-setup");
    if tokio::fs::create_dir_all(&setup).await.is_err() {
        return error("could not prepare cloud setup directory");
    }
    let workspace = match crate::lock(&state.workspaces).add(setup) {
        Ok(w) => w,
        Err(_) => return error("could not register cloud setup workspace"),
    };
    // Multiple presses focus the existing login instead of issuing duplicate
    // native login requests or racing a managed installation.
    if let Some(session) = state.sessions.list().into_iter().find(|s| {
        s.alive
            && s.name == name
            && crate::lock(&state.session_workspaces).get(&s.id) == Some(&workspace.id)
    }) {
        return Json(json!({"workspace_id":workspace.id,"session_id":session.id})).into_response();
    }
    let body = serde_json::from_value(
        json!({"workspace_id":workspace.id,"kind":"shell","name":name,"prelude":script}),
    )
    .expect("static create-session fields");
    let response = crate::api::create_session(State(state.clone()), Json(body)).await;
    if !response.status().is_success() {
        return response;
    }
    let Ok(bytes) = axum::body::to_bytes(response.into_body(), 64 * 1024).await else {
        return error("cloud setup response unavailable");
    };
    let Ok(session) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return error("cloud setup response unavailable");
    };
    if let Some(id) = session["id"].as_str() {
        crate::activity::record(&state, id);
    }
    Json(json!({"workspace_id":workspace.id,"session_id":session["id"]})).into_response()
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
    let result = tokio::time::timeout(limit, child.child.wait()).await;
    child.stop_group();
    if result.is_err() {
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
    let Ok(_operation) = OPERATIONS.try_acquire() else {
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
    let projects = home.join("projects");
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
    let mut command = tokio::process::Command::new("git");
    command
        .args([
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "clone",
            "--",
        ])
        .arg(url.to_string())
        .arg(&staging)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let cloned = clone_status(&mut command, Duration::from_secs(300)).await;
    if !cloned {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return error("clone failed; connect GitHub or check the repository URL");
    }
    if tokio::fs::try_exists(&destination).await.unwrap_or(true)
        || tokio::fs::rename(&staging, &destination).await.is_err()
    {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return error("project name became unavailable");
    }
    match crate::lock(&state.workspaces).add(PathBuf::from(&destination)) {
        Ok(workspace) => {
            state.changes.notify_waiters();
            Json(json!({"workspace_id":workspace.id,"root":destination})).into_response()
        }
        Err(_) => error("project cloned but could not be registered"),
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
