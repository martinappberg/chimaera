use super::{home, now, process, readiness, ProviderDefinition, ProviderState};
use crate::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    path::Path as FsPath,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

#[path = "claude.rs"]
mod claude;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
pub(crate) fn active() -> usize {
    ACTIVE.load(Ordering::Acquire)
}
struct Busy;
impl Drop for Busy {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Preparing,
    Waiting,
    Verifying,
    Connected,
    Disconnected,
    Failed,
    Canceled,
    Expired,
}
impl Phase {
    fn pending(self) -> bool {
        matches!(self, Self::Preparing | Self::Waiting | Self::Verifying)
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Action {
    Browser {
        url: String,
        input: &'static str,
    },
    DeviceCode {
        verification_url: String,
        user_code: String,
    },
    Terminal {
        workspace_id: String,
        session_id: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Operation {
    Connect,
    Disconnect,
}
#[derive(Clone, Debug, Serialize)]
pub(super) struct Connection {
    id: String,
    provider_id: String,
    operation: Operation,
    phase: Phase,
    expires_at: u64,
    action: Option<Action>,
    error_code: Option<String>,
}
pub(super) struct Attempt {
    value: Mutex<Connection>,
    cancel: watch::Sender<bool>,
    finished: AtomicBool,
    session: Mutex<Option<String>>,
    pub(super) process: Mutex<Option<u32>>,
    input: Mutex<Option<mpsc::Sender<String>>>,
}
impl Attempt {
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    pub async fn wait_finished(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(4), async {
            while !self.finished() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
    }
    pub fn snapshot(&self) -> Connection {
        crate::lock(&self.value).clone()
    }
    pub fn submit(&self, code: String) -> Result<(), &'static str> {
        if code.is_empty()
            || code.len() > 4096
            || code.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err("invalid_authorization_code");
        }
        let mut value = crate::lock(&self.value);
        // Claude's page shows `code#state`; its CLI splits on `#` and, given
        // only one half, prints an error nobody sees and keeps waiting. Refuse
        // it here while the attempt is still waiting for the whole code.
        if value.provider_id == "claude" && !claude::complete_code(&code) {
            return Err("authorization_code_incomplete");
        }
        if value.phase != Phase::Waiting
            || value.expires_at <= now()
            || !matches!(
                value.action,
                Some(Action::Browser {
                    input: "authorization_code",
                    ..
                })
            )
        {
            return Err("connection_not_waiting");
        }
        let input = crate::lock(&self.input);
        input
            .as_ref()
            .ok_or("connection_not_waiting")?
            .try_send(code)
            .map_err(|_| "connection_not_waiting")?;
        // The submitted code is never part of connection state or diagnostics.
        value.phase = Phase::Verifying;
        value.action = None;
        Ok(())
    }
    pub fn cancel(&self) {
        let mut value = crate::lock(&self.value);
        if value.phase.pending() && value.operation == Operation::Connect {
            value.phase = Phase::Canceled;
            value.action = None;
            let _ = self.cancel.send(true);
        }
    }
    pub(super) fn update(&self, phase: Phase, action: Option<Action>, error: Option<&str>) {
        let mut value = crate::lock(&self.value);
        // A late CLI completion must never resurrect a canceled attempt.
        if !value.phase.pending() {
            return;
        }
        value.phase = phase;
        value.action = action;
        value.error_code = error.map(str::to_owned);
    }
}
pub(super) fn start(
    state: Arc<AppState>,
    def: &'static ProviderDefinition,
) -> Result<Connection, &'static str> {
    start_operation(state, def, Operation::Connect)
}
pub(super) fn start_disconnect(
    state: Arc<AppState>,
    def: &'static ProviderDefinition,
) -> Result<Connection, &'static str> {
    start_operation(state, def, Operation::Disconnect)
}
pub(super) fn disconnecting(state: &AppState, id: &str) -> bool {
    crate::lock(&state.cloud_providers.connections)
        .values()
        .any(|attempt| {
            let value = attempt.snapshot();
            !attempt.finished()
                && value.provider_id == id
                && value.operation == Operation::Disconnect
        })
}
pub(super) fn pending_disconnect(state: &AppState) -> Option<Connection> {
    crate::lock(&state.cloud_providers.connections)
        .values()
        .filter(|attempt| !attempt.finished())
        .map(|attempt| attempt.snapshot())
        .filter(|value| value.operation == Operation::Disconnect)
        .min_by(|a, b| (a.expires_at, &a.id).cmp(&(b.expires_at, &b.id)))
}
fn start_operation(
    state: Arc<AppState>,
    def: &'static ProviderDefinition,
    operation: Operation,
) -> Result<Connection, &'static str> {
    let mut connections = crate::lock(&state.cloud_providers.connections);
    connections.retain(|_, a| !a.finished() || a.snapshot().expires_at.saturating_add(300) > now());
    if let Some(a) = connections.values().find(|a| {
        let c = a.snapshot();
        c.provider_id == def.id && !a.finished()
    }) {
        let snapshot = a.snapshot();
        return if snapshot.operation == operation {
            Ok(snapshot)
        } else {
            Err("provider_busy")
        };
    }
    while connections.len() >= 24 {
        let oldest = connections
            .iter()
            .filter(|(_, a)| a.finished())
            .min_by_key(|(_, a)| a.snapshot().expires_at)
            .map(|(id, _)| id.clone());
        let Some(oldest) = oldest else {
            break;
        };
        connections.remove(&oldest);
    }
    let (cancel, mut canceled) = watch::channel(false);
    let value = Connection {
        id: crate::agents::fresh_session_id(),
        provider_id: def.id.into(),
        operation,
        phase: Phase::Preparing,
        expires_at: now()
            + if operation == Operation::Connect {
                900
            } else {
                60
            },
        action: None,
        error_code: None,
    };
    let attempt = Arc::new(Attempt {
        value: Mutex::new(value.clone()),
        cancel,
        finished: AtomicBool::new(false),
        session: Mutex::new(None),
        process: Mutex::new(None),
        input: Mutex::new(None),
    });
    connections.insert(value.id.clone(), attempt.clone());
    drop(connections);
    state
        .cloud_providers
        .auth_epoch
        .fetch_add(1, Ordering::AcqRel);
    crate::lock(&state.cloud_providers.cache).remove(def.id);
    ACTIVE.fetch_add(1, Ordering::AcqRel);
    tokio::spawn(async move {
        let _busy = Busy;
        let work = async {
            match operation {
                Operation::Connect => run(&state, def, &attempt).await,
                Operation::Disconnect => super::disconnect::run(&state, def, &attempt).await,
            }
        };
        let stopped = async {
            loop {
                if state.stopping.load(Ordering::Acquire) {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        };
        let result = tokio::select! {
            result=work=>result,
            _=canceled.changed()=>Err("canceled"),
            _=tokio::time::sleep(Duration::from_secs(if operation == Operation::Connect { 900 } else { 60 }))=>Err("expired"),
            _=stopped=>Err("unavailable"),
        };
        // The run future has been dropped here, so its RPC process group and
        // session guards have been stopped. PTY termination includes a short
        // SIGHUP→SIGKILL grace; keep the provider reserved until it is reaped.
        let session = crate::lock(&attempt.session).clone();
        let process = *crate::lock(&attempt.process);
        if let Some(id) = &session {
            let _ = state.sessions.kill(id);
        }
        let clean = tokio::time::timeout(Duration::from_secs(3), async {
            while session
                .as_ref()
                .is_some_and(|id| state.sessions.get(id).is_some_and(|s| s.alive))
                || process.is_some_and(process::group_alive)
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok();
        crate::lock(&attempt.input).take();
        state
            .cloud_providers
            .auth_epoch
            .fetch_add(1, Ordering::AcqRel);
        crate::lock(&state.cloud_providers.cache).remove(def.id);
        match result {
            Ok(()) => attempt.update(
                if operation == Operation::Connect {
                    Phase::Connected
                } else {
                    Phase::Disconnected
                },
                None,
                None,
            ),
            Err("canceled") => attempt.update(Phase::Canceled, None, None),
            Err("expired") => attempt.update(Phase::Expired, None, Some("expired")),
            Err(reason) => attempt.update(Phase::Failed, None, Some(reason)),
        }
        if clean {
            attempt.finished.store(true, Ordering::Release);
            state.changes.notify_waiters();
            return;
        }
        // Never launch a replacement credential writer while cleanup is
        // uncertain; keep checking (bounded) and release the provider as soon
        // as the old process group is really gone, instead of until restart.
        {
            let mut value = crate::lock(&attempt.value);
            value.error_code = Some("cleanup_failed".into());
            value.action = None;
        }
        state.changes.notify_waiters();
        for _ in 0..120 {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if state.stopping.load(Ordering::Acquire) {
                return;
            }
            let gone = !session
                .as_ref()
                .is_some_and(|id| state.sessions.get(id).is_some_and(|s| s.alive))
                && !process.is_some_and(process::group_alive);
            if gone {
                attempt.finished.store(true, Ordering::Release);
                state.changes.notify_waiters();
                return;
            }
        }
    });
    Ok(value)
}
struct SessionGuard {
    state: Arc<AppState>,
    id: String,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        let _ = self.state.sessions.kill(&self.id);
    }
}
async fn workspace(state: &Arc<AppState>) -> Result<crate::workspaces::Workspace, &'static str> {
    let root = home(state).join("projects/.chimaera-setup");
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|_| "setup_unavailable")?;
    let owner = state.clone();
    tokio::task::spawn_blocking(move || {
        crate::lock(&owner.workspaces)
            .add_internal(root)
            .map_err(|_| "setup_unavailable")
    })
    .await
    .map_err(|_| "setup_unavailable")?
}
async fn response_id(response: axum::response::Response) -> Result<String, &'static str> {
    if !response.status().is_success() {
        return Err("setup_failed");
    }
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .map_err(|_| "setup_failed")?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "setup_failed")?;
    value["session_id"]
        .as_str()
        .filter(|id| id.len() <= 128)
        .map(str::to_owned)
        .ok_or("setup_failed")
}
async fn install(
    state: &Arc<AppState>,
    def: &ProviderDefinition,
    attempt: &Attempt,
) -> Result<(), &'static str> {
    let Some(kind) = def.kind else {
        return Err("installation_unavailable");
    };
    let ws = workspace(state).await?;
    let response = crate::runtimes::install_agent(
        State(state.clone()),
        Path(kind.as_str().into()),
        Json(serde_json::from_value(json!({"workspace_id":ws.id})).map_err(|_| "setup_failed")?),
    )
    .await;
    let id = response_id(response).await?;
    *crate::lock(&attempt.session) = Some(id.clone());
    let _session = SessionGuard {
        state: state.clone(),
        id: id.clone(),
    };
    attempt.update(
        Phase::Preparing,
        Some(Action::Terminal {
            workspace_id: ws.id,
            session_id: id.clone(),
        }),
        None,
    );
    while state.sessions.get(&id).is_some_and(|s| s.alive) {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    super::binary(state, def, true)
        .await
        .map_err(|_| "installation_failed")?;
    Ok(())
}
async fn terminal(
    state: &Arc<AppState>,
    def: &ProviderDefinition,
    bin: &FsPath,
    attempt: &Attempt,
) -> Result<(), &'static str> {
    let ws = workspace(state).await?;
    let args: Vec<String> = if def.id == "claude" {
        vec![
            bin.to_string_lossy().into_owned(),
            "auth".into(),
            "login".into(),
            "--claudeai".into(),
        ]
    } else {
        // Fixed GitHub-only command. A successful login installs Git's credential
        // helper; no token is ever requested or printed by Chimaera.
        vec!["/bin/sh".into(),"-c".into(),format!("{} auth login --hostname github.com --git-protocol https --web && {} auth setup-git",super::super::quote(&bin.to_string_lossy()),super::super::quote(&bin.to_string_lossy()))]
    };
    let id = crate::agents::fresh_session_id();
    let env = crate::api::session_env(state, &id, "dark", None);
    let env_remove = crate::api::spawn_env_remove(&env);
    let opts = chimaera_pty::SpawnOpts {
        cwd: ws.root,
        name: Some(format!(
            "Connect {}",
            chimaera_core::cloud_providers::provider_definition(def.id)
                .map_or(def.id, |p| p.label.as_str())
        )),
        cols: 90,
        rows: 26,
        command: Some(crate::launcher::wrap_login_shell(
            &crate::launcher::login_shell(),
            args,
        )),
        id: Some(id.clone()),
        env,
        env_remove,
        scrollback: crate::lock(&state.settings).scrollback_lines(),
    };
    state
        .sessions
        .spawn(opts)
        .map_err(|_| "terminal_unavailable")?;
    crate::lock(&state.session_workspaces).insert(id.clone(), ws.id.clone());
    crate::activity::record(state, &id);
    state.changes.notify_waiters();
    *crate::lock(&attempt.session) = Some(id.clone());
    let _session = SessionGuard {
        state: state.clone(),
        id: id.clone(),
    };
    attempt.update(
        Phase::Waiting,
        Some(Action::Terminal {
            workspace_id: ws.id,
            session_id: id.clone(),
        }),
        None,
    );
    while state.sessions.get(&id).is_some_and(|s| s.alive) {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}
fn device_action(value: &Value) -> Result<(String, Action), &'static str> {
    if value["type"] != "chatgptDeviceCode" {
        return Err("unsupported_login");
    }
    let login = value["loginId"]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
        .ok_or("invalid_login_response")?;
    let url = value["verificationUrl"]
        .as_str()
        .filter(|s| s.len() <= 4096)
        .ok_or("invalid_login_response")?;
    let uri = url
        .parse::<axum::http::Uri>()
        .map_err(|_| "invalid_login_response")?;
    // Shared provider-origin catalog is also enforced by the native opener.
    // The device flow needs no local callback or credential-bearing redirects.
    if uri.scheme_str() != Some("https")
        || !chimaera_core::cloud_providers::provider_auth_origins("codex")
            .iter()
            .any(|origin| {
                let authority = uri.authority().map_or("", |a| a.as_str());
                origin
                    == &format!(
                        "https://{}",
                        authority.strip_suffix(":443").unwrap_or(authority)
                    )
            })
        || url.contains('#')
    {
        return Err("invalid_login_response");
    }
    let code = value["userCode"]
        .as_str()
        .filter(|s| {
            (4..=32).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
        .ok_or("invalid_login_response")?;
    Ok((
        login.into(),
        Action::DeviceCode {
            verification_url: url.into(),
            user_code: code.into(),
        },
    ))
}
async fn codex(state: &Arc<AppState>, bin: &FsPath, attempt: &Attempt) -> Result<(), &'static str> {
    let mut rpc = process::Rpc::open(bin, &home(state)).await?;
    *crate::lock(&attempt.process) = rpc.process_id();
    let response = rpc
        .request("account/login/start", json!({"type":"chatgptDeviceCode"}))
        .await
        .map_err(|_| "device_login_unavailable")?;
    let (login, action) = device_action(&response)?;
    attempt.update(Phase::Waiting, Some(action), None);
    for _ in 0..1024 {
        let msg = rpc.next().await?;
        if msg["method"] == "account/login/completed"
            && msg["params"]["loginId"].as_str() == Some(&login)
        {
            return if msg["params"]["success"] == true {
                Ok(())
            } else {
                Err("sign_in_failed")
            };
        }
    }
    Err("output_limit")
}
async fn run(
    state: &Arc<AppState>,
    def: &'static ProviderDefinition,
    attempt: &Attempt,
) -> Result<(), &'static str> {
    let ids = vec![def.id.to_owned()];
    if readiness(state, &ids, true)
        .await
        .first()
        .is_some_and(|s| s.state == ProviderState::SignedIn)
    {
        return Ok(());
    }
    if matches!(super::binary(state, def, false).await, Err("not_installed")) {
        install(state, def, attempt).await?;
    }
    let bin = super::binary(state, def, false).await?;
    if def.id == "claude" {
        claude::login(state, &bin, attempt).await?;
    } else if def.id == "codex" {
        codex(state, &bin, attempt).await?;
    } else {
        terminal(state, def, &bin, attempt).await?;
    }
    attempt.update(Phase::Verifying, None, None);
    let status = readiness(state, &ids, true).await;
    if status
        .first()
        .is_some_and(|s| s.state == ProviderState::SignedIn)
    {
        Ok(())
    } else {
        Err("sign_in_not_confirmed")
    }
}

#[cfg(test)]
#[path = "connect_tests.rs"]
mod tests;
