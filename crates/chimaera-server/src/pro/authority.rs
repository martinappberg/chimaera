//! Consumer-side workspace restriction. Remote services remain authoritative;
//! this does not replace namespace isolation or scope arbitrary daemon APIs.
use super::protocol::{Configure, Delegation, Role, WorkspaceBinding, WorkspaceConfigureAck};
use crate::{lock, AppState};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Accepted {
    pub workspace: WorkspaceBinding,
    account_id: String,
    endpoint: String,
    pub root: PathBuf,
    identity: (u64, u64),
}
#[derive(Clone, Default)]
pub(super) enum Authority {
    #[default]
    Unbound,
    Bound(Accepted),
    Invalid,
}
impl Authority {
    pub fn load(root: &Path) -> Self {
        let path = root.join("workspace-authority.json");
        let value = (|| -> Result<Option<Accepted>> {
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            ensure!(metadata.is_file(), "invalid workspace authority record");
            let mut file = std::fs::File::open(&path)?;
            let mut bytes = Vec::new();
            file.by_ref().take(16 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 16 * 1024,
                "workspace authority record exceeds limit"
            );
            let value: Accepted = serde_json::from_slice(&bytes)?;
            ensure!(
                valid_binding(&value.workspace)
                    && super::valid_id(&value.account_id)
                    && value.root.is_absolute(),
                "invalid workspace authority record"
            );
            Ok(Some(value))
        })();
        match value {
            Ok(Some(value)) => Self::Bound(value),
            Ok(None) => Self::Unbound,
            Err(_) => Self::Invalid,
        }
    }
    pub fn allows(&self, workspace: &str) -> bool {
        match self {
            Self::Unbound => true,
            Self::Bound(value) => value.workspace.workspace_id == workspace,
            Self::Invalid => false,
        }
    }
    pub fn restricted(&self) -> bool {
        !matches!(self, Self::Unbound)
    }
    pub fn acknowledgment(&self) -> Option<WorkspaceConfigureAck> {
        match self {
            Self::Bound(value) => Some(value.ack()),
            _ => None,
        }
    }
}
impl Accepted {
    pub fn ack(&self) -> WorkspaceConfigureAck {
        WorkspaceConfigureAck {
            workspace_authority: 1,
            workspace: self.workspace.clone(),
            workspace_root: self.root.clone(),
        }
    }
}
fn identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        (0, 0)
    }
}
pub(super) fn valid_binding(value: &WorkspaceBinding) -> bool {
    super::valid_id(&value.workspace_id) && value.revision > 0
}
fn worker_scopes(scopes: &[String]) -> bool {
    scopes.len() == 2
        && scopes.iter().any(|scope| scope == "baton")
        && scopes.iter().any(|scope| scope == "mirror")
}
pub(super) fn validate_scoped(config: &Configure) -> Result<()> {
    ensure!(
        cfg!(unix),
        "workspace directory authority is unsupported on this platform"
    );
    let binding = config
        .delegation
        .workspace
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("workspace binding required"))?;
    ensure!(
        valid_binding(binding)
            && config.role == Role::Worker
            && worker_scopes(&config.delegation.scope),
        "invalid workspace authority"
    );
    ensure!(
        config.keeper_url.is_empty() && config.account_id.as_deref().is_some_and(super::valid_id),
        "workspace authority requires account identity and no keeper access"
    );
    Ok(())
}
pub(super) fn renewal(previous: &Delegation, next: &Delegation) -> Result<()> {
    ensure!(
        previous.workspace == next.workspace && previous.device_id == next.device_id,
        "delegation renewal changed authority"
    );
    ensure!(
        !next.access_token.is_empty()
            && next.access_token.len() <= 8192
            && !next.access_token.chars().any(char::is_control),
        "invalid renewed delegation"
    );
    if next.workspace.is_some() {
        ensure!(
            worker_scopes(&next.scope),
            "delegation renewal changed authority"
        );
    }
    Ok(())
}
/// Only a validated grant installed into the current generation counts as a
/// renewal. A rejected response leaves both the token and refresh deadline alone.
pub(super) fn install_renewal(
    state: &AppState,
    generation: u64,
    previous: &Delegation,
    next: Delegation,
) -> bool {
    if renewal(previous, &next).is_err() {
        return false;
    }
    let mut runtime = lock(&state.pro.runtime);
    let Some(runtime) = runtime.as_mut() else {
        return false;
    };
    if generation
        != state
            .pro
            .generation
            .load(std::sync::atomic::Ordering::Acquire)
        || renewal(&runtime.delegation, &next).is_err()
    {
        return false;
    }
    runtime.delegation = next;
    true
}

pub(super) fn config_workspace(config: &Configure, workspace: &str) -> Result<()> {
    ensure!(super::valid_id(workspace), "invalid workspace identity");
    ensure!(
        config
            .delegation
            .workspace
            .as_ref()
            .is_none_or(|binding| binding.workspace_id == workspace),
        "workspace authority denied"
    );
    Ok(())
}
pub(super) fn workspace(state: &AppState, workspace: &str) -> Result<()> {
    ensure!(
        super::valid_id(workspace) && lock(&state.pro.authority).allows(workspace),
        "workspace authority denied"
    );
    Ok(())
}
pub(super) fn config_matches(
    state: &AppState,
    config: &Configure,
    workspace_id: &str,
) -> Result<()> {
    config_workspace(config, workspace_id)?;
    workspace(state, workspace_id)?;
    match &*lock(&state.pro.authority) {
        Authority::Bound(value) => ensure!(
            config.delegation.workspace.as_ref() == Some(&value.workspace)
                && config.account_id.as_ref() == Some(&value.account_id)
                && config.endpoint == value.endpoint,
            "workspace authority changed"
        ),
        Authority::Unbound => ensure!(
            config.delegation.workspace.is_none(),
            "workspace authority not accepted"
        ),
        Authority::Invalid => anyhow::bail!("workspace authority record needs recovery"),
    }
    Ok(())
}
/// Bound grants may only reach their exact baton/mirror endpoints and renewal.
/// In particular they never enter keeper discovery, wake or account operations.
pub(super) fn account_request(
    config: &Configure,
    path: &str,
    method: &str,
    body: Option<&serde_json::Value>,
) -> Result<()> {
    let Some(binding) = &config.delegation.workspace else {
        return Ok(());
    };
    validate_scoped(config)?;
    let baton = format!("/v1/baton/{}", binding.workspace_id);
    let allowed = (path == "/v1/delegations/renew" && method == "POST")
        || (path == baton && method == "GET")
        || (["acquire", "renew", "release"]
            .iter()
            .any(|action| path == format!("{baton}/{action}"))
            && method == "POST")
        || (path == format!("{baton}/policy") && method == "PUT")
        || (path == "/v1/mirror/credentials"
            && method == "POST"
            && body.and_then(|body| body["workspace_id"].as_str()) == Some(&binding.workspace_id));
    ensure!(allowed, "workspace authority denied");
    Ok(())
}
pub(super) async fn prepare(
    state: &AppState,
    config: &Configure,
    root: PathBuf,
) -> Result<Accepted> {
    validate_scoped(config)?;
    ensure!(
        root.is_absolute()
            && !root
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir)),
        "invalid workspace root"
    );
    // Reject changed authority before even probing a caller-supplied path.
    match &*lock(&state.pro.authority) {
        Authority::Bound(previous) => ensure!(
            config.delegation.workspace.as_ref() == Some(&previous.workspace)
                && config.account_id.as_ref() == Some(&previous.account_id)
                && config.endpoint == previous.endpoint
                && root == previous.root,
            "workspace authority cannot be changed"
        ),
        Authority::Invalid => anyhow::bail!("workspace authority record needs recovery"),
        Authority::Unbound => ensure!(
            lock(&state.pro.runtime).is_none(),
            "workspace authority needs a fresh daemon"
        ),
    }
    for workspace in lock(&state.workspaces).list() {
        ensure!(
            config
                .delegation
                .workspace
                .as_ref()
                .is_some_and(|value| value.workspace_id == workspace.id)
                && workspace.root == root,
            "workspace authority needs a dedicated daemon"
        );
    }
    let (root, metadata) = tokio::task::spawn_blocking(move || -> Result<_> {
        let root = std::fs::canonicalize(root)?;
        let metadata = std::fs::metadata(&root)?;
        ensure!(metadata.is_dir(), "workspace root is not a directory");
        Ok((root, metadata))
    })
    .await??;
    let value = Accepted {
        workspace: config.delegation.workspace.clone().unwrap(),
        account_id: config.account_id.clone().unwrap(),
        endpoint: config.endpoint.clone(),
        root,
        identity: identity(&metadata),
    };
    for workspace in lock(&state.workspaces).list() {
        ensure!(
            workspace.id == value.workspace.workspace_id && workspace.root == value.root,
            "workspace authority needs a dedicated daemon"
        );
    }
    match &*lock(&state.pro.authority) {
        Authority::Bound(previous) => {
            ensure!(previous == &value, "workspace authority cannot be changed")
        }
        Authority::Invalid => anyhow::bail!("workspace authority record needs recovery"),
        Authority::Unbound => ensure!(
            lock(&state.pro.runtime).is_none(),
            "workspace authority needs a fresh daemon"
        ),
    }
    Ok(value)
}
pub(super) async fn save(state: &AppState, value: &Accepted) -> Result<()> {
    let path = state.pro.root.join("workspace-authority.json");
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= 16 * 1024,
        "workspace authority record exceeds limit"
    );
    tokio::task::spawn_blocking(move || crate::persist::atomic_write_json(&path, bytes)).await??;
    Ok(())
}
pub(super) async fn destination(
    state: &AppState,
    config: &Configure,
    workspace_id: &str,
    requested: Option<&Path>,
) -> Result<Option<PathBuf>> {
    config_matches(state, config, workspace_id)?;
    let value = match &*lock(&state.pro.authority) {
        Authority::Bound(value) => Some(value.clone()),
        _ => None,
    };
    let Some(value) = value else {
        return Ok(requested.map(Path::to_path_buf));
    };
    ensure!(
        requested.is_none_or(|path| path == value.root),
        "workspace destination is fixed"
    );
    let metadata = tokio::fs::symlink_metadata(&value.root).await?;
    ensure!(
        metadata.is_dir() && identity(&metadata) == value.identity,
        "registered workspace root changed"
    );
    Ok(Some(value.root))
}

/// Pure registry check used before scheduling work; the async boundary also
/// verifies the pinned directory identity before a transfer touches that root.
pub(super) fn registered_root(state: &AppState, workspace_id: &str, root: &Path) -> Result<()> {
    workspace(state, workspace_id)?;
    if let Authority::Bound(value) = &*lock(&state.pro.authority) {
        ensure!(root == value.root, "workspace destination is fixed");
    }
    Ok(())
}

pub(super) fn session(state: &AppState, session_id: &str) -> Result<()> {
    if !lock(&state.pro.authority).restricted() {
        return Ok(());
    }
    let workspace_id = lock(&state.session_workspaces)
        .get(session_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("workspace authority denied"))?;
    workspace(state, &workspace_id)
}

#[cfg(all(test, unix))]
#[path = "authority_tests.rs"]
mod tests;
