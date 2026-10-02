//! What a plugin keeps across restarts: its durable state (`state-keep`)
//! and its settings' values. Design: docs/design/plugin-platform-plan.md §7
//! ("Durable state") and §9.
//!
//! Small capped JSON, rewritten atomically (never SQLite, never edited in
//! place), under `<data dir>/plugins/.data/<id>/`:
//!
//! - `<workspace>.json`: `{"kept": {key: json text}, "settings": {key: value}}`
//!   — the durable keys (within the 64 KiB of state per plugin and
//!   workspace) and the workspace-scoped settings;
//! - `host.json`: `{"settings": {key: value}}` — the host-scoped settings.
//!
//! Read once per (plugin, scope) and cached; every write goes through one
//! async lock, off the reactor.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Path as AxPath, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::platform::{SettingDecl, SettingScope};
use super::Manifest;
use crate::AppState;

/// `<data dir>/plugins/<DIR>`: not a plugin id (ids start with a letter or
/// digit), so the installed-copies scan never reads it.
const DIR: &str = ".data";
/// A file's cap: the state's 64 KiB, settings, and JSON's overhead.
const FILE_MAX: usize = 160 << 10;
/// The host-scoped file's name (a workspace id never has a dot).
const HOST: &str = "host";

#[derive(Serialize, Deserialize, Default, Clone)]
struct Doc {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    kept: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    settings: BTreeMap<String, Value>,
}

/// The cache and the one-writer lock (on `AppState` via `Platform`).
#[derive(Default)]
pub(crate) struct PluginData {
    /// (plugin, workspace id or `HOST`) → its file, as read or last written.
    docs: Mutex<HashMap<(String, String), Doc>>,
    writing: tokio::sync::Mutex<()>,
}

fn path(root: &Path, plugin: &str, scope: &str) -> PathBuf {
    root.join(DIR)
        .join(plugin)
        .join(format!("{}.json", crate::timeline::sanitize(scope)))
}

fn read_doc(file: &Path) -> Doc {
    match std::fs::read(file) {
        Ok(bytes) if bytes.len() <= FILE_MAX => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            tracing::warn!(path = %file.display(), %err, "plugin data unreadable; starting it over");
            Doc::default()
        }),
        Ok(_) => {
            tracing::warn!(path = %file.display(), "plugin data over its cap; starting it over");
            Doc::default()
        }
        Err(_) => Doc::default(),
    }
}

/// `(plugin, scope)`'s file, read on first use.
async fn doc(state: &AppState, plugin: &str, scope: &str) -> Doc {
    let key = (plugin.to_string(), scope.to_string());
    if let Some(doc) = crate::lock(&state.plugin_platform.data.docs).get(&key) {
        return doc.clone();
    }
    let file = path(&state.plugin_catalog.root, plugin, scope);
    let doc = tokio::task::spawn_blocking(move || read_doc(&file))
        .await
        .unwrap_or_default();
    crate::lock(&state.plugin_platform.data.docs)
        .entry(key)
        .or_insert(doc)
        .clone()
}

/// Change `(plugin, scope)`'s file with `edit` and write it (one writer at
/// a time, off the reactor). Refused past `FILE_MAX`, leaving it as it was.
async fn update(
    state: &AppState,
    plugin: &str,
    scope: &str,
    edit: impl FnOnce(&mut Doc),
) -> Result<(), String> {
    let _one = state.plugin_platform.data.writing.lock().await;
    let mut next = doc(state, plugin, scope).await;
    edit(&mut next);
    let text = serde_json::to_string(&next).map_err(|e| e.to_string())?;
    if text.len() > FILE_MAX {
        return Err(format!(
            "the plugin's kept data is capped at {} KiB",
            FILE_MAX >> 10
        ));
    }
    let file = path(&state.plugin_catalog.root, plugin, scope);
    tokio::task::spawn_blocking(move || {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::persist::atomic_write_json(&file, text)
    })
    .await
    .map_err(|e| format!("saving failed: {e}"))?
    .map_err(|e| format!("saving failed: {e:#}"))?;
    crate::lock(&state.plugin_platform.data.docs)
        .insert((plugin.to_string(), scope.to_string()), next);
    Ok(())
}

/// The durable keys `plugin` kept in `ws`.
pub(crate) async fn kept(state: &AppState, plugin: &str, ws: &str) -> BTreeMap<String, String> {
    doc(state, plugin, ws).await.kept
}

/// Replace the durable keys `plugin` keeps in `ws`.
pub(crate) async fn save_kept(
    state: &AppState,
    plugin: &str,
    ws: &str,
    kept: BTreeMap<String, String>,
) -> Result<(), String> {
    update(state, plugin, ws, |d| d.kept = kept).await
}

fn scope_of(decl: &SettingDecl, ws: &str) -> String {
    match decl.scope {
        SettingScope::Host => HOST.to_string(),
        SettingScope::Workspace => ws.to_string(),
    }
}

/// A declared setting's value for `ws`: the user's, else its default.
pub(crate) async fn setting(state: &AppState, m: &Manifest, ws: &str, key: &str) -> Option<Value> {
    let decl = m.settings.iter().find(|s| s.key == key)?;
    let set = doc(state, &m.id, &scope_of(decl, ws))
        .await
        .settings
        .get(key)
        .cloned();
    // A value a newer declaration no longer allows reads as the default.
    Some(
        set.and_then(|v| decl.check(&v).ok())
            .unwrap_or_else(|| decl.default.clone()),
    )
}

/// Set (or, with `None`, reset) a setting for `ws`; the workspaces whose
/// value changed (every one, for a host setting).
async fn set_setting(
    state: &AppState,
    m: &Manifest,
    ws: Option<&str>,
    key: &str,
    value: Option<Value>,
) -> Result<SettingScope, String> {
    let decl = m
        .settings
        .iter()
        .find(|s| s.key == key)
        .ok_or_else(|| format!("{} has no setting {key}", m.name))?;
    let value = value.map(|v| decl.check(&v)).transpose()?;
    let scope = match (decl.scope, ws) {
        (SettingScope::Host, _) => HOST.to_string(),
        (SettingScope::Workspace, Some(ws)) => ws.to_string(),
        (SettingScope::Workspace, None) => {
            return Err(format!("{key} is set per workspace: name one"))
        }
    };
    update(state, &m.id, &scope, |d| match value {
        Some(v) => {
            d.settings.insert(key.to_string(), v);
        }
        None => {
            d.settings.remove(key);
        }
    })
    .await?;
    Ok(decl.scope)
}

/// A removed plugin: everything it kept, everywhere (blocking).
pub(crate) fn forget_plugin(state: &AppState, plugin: &str) {
    crate::lock(&state.plugin_platform.data.docs).retain(|(p, _), _| p != plugin);
    let dir = state.plugin_catalog.root.join(DIR).join(plugin);
    if dir.exists() && !dir.is_symlink() {
        if let Err(err) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(%plugin, %err, "plugin data not removed");
        }
    }
}

/// A deleted workspace: every plugin's file for it (blocking).
pub(crate) fn forget_workspace(state: &AppState, ws: &str) {
    crate::lock(&state.plugin_platform.data.docs).retain(|(_, w), _| w != ws);
    let Ok(plugins) = std::fs::read_dir(state.plugin_catalog.root.join(DIR)) else {
        return;
    };
    let name = format!("{}.json", crate::timeline::sanitize(ws));
    for plugin in plugins.flatten() {
        let _ = std::fs::remove_file(plugin.path().join(&name));
    }
}

#[derive(Deserialize)]
pub(crate) struct SettingsQuery {
    #[serde(default)]
    workspace: Option<String>,
}

/// `GET /plugins/{pid}/settings?workspace=`: its declared settings with
/// their values (for that workspace), for Settings → Plugins.
pub(crate) async fn get_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
    Query(q): Query<SettingsQuery>,
) -> Response {
    let Some(m) = super::manifest(&state, &pid) else {
        return super::not_found(&format!("plugin {pid}"));
    };
    let ws = q.workspace.unwrap_or_default();
    let host = doc(&state, &m.id, HOST).await;
    let here = if ws.is_empty() {
        Doc::default()
    } else {
        doc(&state, &m.id, &ws).await
    };
    let settings: Vec<Value> = m
        .settings
        .iter()
        .map(|decl| {
            let set = match decl.scope {
                SettingScope::Host => host.settings.get(&decl.key),
                SettingScope::Workspace => here.settings.get(&decl.key),
            }
            .filter(|v| decl.check(v).is_ok());
            let mut v = decl.json();
            v["value"] = set.cloned().unwrap_or_else(|| decl.default.clone());
            v["set"] = json!(set.is_some());
            v
        })
        .collect();
    Json(json!({"plugin": m.id, "settings": settings})).into_response()
}

#[derive(Deserialize)]
pub(crate) struct PutSetting {
    key: String,
    /// `null` resets it to the default.
    #[serde(default)]
    value: Value,
    #[serde(default)]
    workspace: Option<String>,
}

/// `PUT /plugins/{pid}/settings {key, value, workspace?}`: set one (null
/// resets it); the plugin hears `settings-changed` where it is active.
pub(crate) async fn put_route(
    State(state): State<Arc<AppState>>,
    AxPath(pid): AxPath<String>,
    Json(body): Json<PutSetting>,
) -> Response {
    let Some(m) = super::manifest(&state, &pid) else {
        return super::not_found(&format!("plugin {pid}"));
    };
    if let Some(ws) = &body.workspace {
        if crate::lock(&state.workspaces).get(ws).is_none() {
            return super::not_found(&format!("workspace {ws}"));
        }
    }
    let value = (!body.value.is_null()).then_some(body.value);
    let scope = match set_setting(&state, &m, body.workspace.as_deref(), &body.key, value).await {
        Ok(scope) => scope,
        Err(err) => return super::bad_request(err),
    };
    let targets: Vec<String> = match (scope, &body.workspace) {
        (SettingScope::Workspace, Some(ws)) => vec![ws.clone()],
        _ => crate::lock(&state.workspaces)
            .list()
            .into_iter()
            .map(|w| w.id)
            .collect(),
    };
    super::files::settings_changed(&state, &m, targets, &body.key);
    get_route(
        State(state),
        AxPath(pid),
        Query(SettingsQuery {
            workspace: body.workspace,
        }),
    )
    .await
}
