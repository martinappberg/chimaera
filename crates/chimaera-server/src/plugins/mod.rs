//! Workbench plugins: opt-in add-ons that say exactly what they add.
//! Design: docs/timeline-knowledge-plugins-plan.md §6.
//!
//! A plugin is a small TOML manifest (data, never code) plus, for
//! first-party plugins, daemon code behind NAMED capabilities
//! (`provides.knowledge = "mycelium"` → `crate::mycelium`). Manifests are
//! embedded in the binary and version with it; there is no dynamic loading
//! and no third-party code path.
//!
//! State is minimal by design: a plugin is switched on per workspace
//! (`Workspace.plugins_on` — the Plugins page is per workspace, so is its
//! switch), and it is *active* there when it is on AND its `detect` paths are
//! present. Detection is cached per workspace with a short TTL; stat work
//! always runs off the reactor, and a stale entry is refreshed (awaited)
//! before any answer that decides what an agent sees — a session connecting
//! right after the switch flips must get the tools, not a cold-cache miss.
//!
//! The invariant this module exists to keep: with no plugin active, nothing
//! an agent sees changes (MCP tools/list, instructions, generated settings).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

pub(crate) mod tools;

/// How long a workspace's detect result is trusted before a re-stat.
const DETECT_TTL: Duration = Duration::from_secs(30);

/// The first-party manifests. Adding a plugin = a manifest here + the named
/// capabilities it provides (see docs/agent-guides/plugins.md).
const MANIFESTS: [&str; 2] = [
    include_str!("manifests/mycelium.toml"),
    include_str!("manifests/agent-notes.toml"),
];

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) summary: String,
    #[serde(default)]
    pub(crate) homepage: Option<String>,
    #[serde(default)]
    pub(crate) detect: Detect,
    #[serde(default)]
    pub(crate) requires: Requires,
    #[serde(default)]
    pub(crate) setup: Option<Setup>,
    #[serde(default)]
    pub(crate) provides: Provides,
    #[serde(default)]
    pub(crate) adds: Adds,
}

/// Workspace-relative paths whose presence makes the plugin active here.
/// Empty = always present (a plugin with no project footprint).
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Detect {
    #[serde(default)]
    pub(crate) any: Vec<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Requires {
    /// agent kind ("claude" | "codex") → the agent-native plugin it needs.
    #[serde(default)]
    pub(crate) agent_plugins: BTreeMap<String, AgentPluginReq>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentPluginReq {
    /// The agent's own plugin id, e.g. "mycelium@mycelium".
    pub(crate) id: String,
    /// What the agent's `marketplace add` takes (owner/repo or a URL).
    pub(crate) marketplace: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct Setup {
    /// The plugin's own documented setup prompt, sent to an agent session the
    /// user picks (their click, their billing).
    pub(crate) prompt: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Provides {
    /// A named knowledge reader (`"mycelium"`).
    #[serde(default)]
    pub(crate) knowledge: Option<String>,
    /// Named MCP tools served only where the plugin is active.
    #[serde(default)]
    pub(crate) mcp_tools: Vec<String>,
    /// Named first-party UI modules (lazy-loaded by the web UI).
    #[serde(default)]
    pub(crate) views: Vec<String>,
}

/// The card's "Adds" lines, in words — mandatory honesty, not decoration.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Adds {
    #[serde(default)]
    pub(crate) ui: Vec<String>,
    #[serde(default)]
    pub(crate) agents: Vec<String>,
}

static CATALOG: LazyLock<Vec<Manifest>> = LazyLock::new(|| {
    MANIFESTS
        .iter()
        .filter_map(|text| match toml::from_str::<Manifest>(text) {
            Ok(manifest) => Some(manifest),
            // Unreachable in a tested build (`every_manifest_parses`); a
            // broken manifest must not take the daemon down with it.
            Err(err) => {
                tracing::error!(%err, "invalid embedded plugin manifest");
                None
            }
        })
        .collect()
});

pub(crate) fn catalog() -> &'static [Manifest] {
    &CATALOG
}

pub(crate) fn manifest(id: &str) -> Option<&'static Manifest> {
    catalog().iter().find(|m| m.id == id)
}

/// Per-workspace detect results: plugin ids whose footprint is present.
#[derive(Default)]
pub(crate) struct DetectCache {
    entries: HashMap<String, (Instant, BTreeSet<String>)>,
}

impl DetectCache {
    pub(crate) fn forget_workspace(&mut self, ws: &str) {
        self.entries.remove(ws);
    }
}

/// Stat every manifest's detect paths under `root` (blocking).
fn detect_blocking(root: &Path) -> BTreeSet<String> {
    catalog()
        .iter()
        .filter(|m| {
            m.detect.any.is_empty() || m.detect.any.iter().any(|rel| present_unlinked(root, rel))
        })
        .map(|m| m.id.clone())
        .collect()
}

/// `root/rel` exists and NO component of `rel` is a symlink — the plugin's
/// own tooling refuses symlinked state dirs, so neither do we (checking only
/// the last component would let a symlinked `.living/` count through its
/// children).
fn present_unlinked(root: &Path, rel: &str) -> bool {
    let mut path = root.to_path_buf();
    for part in Path::new(rel).components() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(md) if !md.is_symlink() => {}
            _ => return false,
        }
    }
    true
}

/// Re-stat a workspace's footprints (off the reactor) and cache the result.
pub(crate) async fn refresh_detect(state: &AppState, ws: &str) -> BTreeSet<String> {
    let Some(root) = crate::lock(&state.workspaces).get(ws).map(|w| w.root) else {
        return BTreeSet::new();
    };
    let found = tokio::task::spawn_blocking(move || detect_blocking(&root))
        .await
        .unwrap_or_default();
    crate::lock(&state.plugin_detect)
        .entries
        .insert(ws.to_string(), (Instant::now(), found.clone()));
    found
}

fn switched_on(state: &AppState, ws: &str) -> BTreeSet<String> {
    crate::lock(&state.workspaces)
        .get(ws)
        .map(|w| w.plugins_on.into_iter().collect())
        .unwrap_or_default()
}

/// Active plugins in a workspace: switched on AND footprint present. A stale
/// detect is refreshed first (a few stats, off the reactor) — callers are
/// routes, MCP connects, spawns and once-per-event paths, never a tight loop.
/// With nothing switched on this returns before any fs work.
pub(crate) async fn active(state: &AppState, ws: &str) -> Vec<&'static Manifest> {
    let on = switched_on(state, ws);
    if on.is_empty() {
        return Vec::new();
    }
    let cached = crate::lock(&state.plugin_detect)
        .entries
        .get(ws)
        .filter(|(at, _)| at.elapsed() < DETECT_TTL)
        .map(|(_, found)| found.clone());
    let found = match cached {
        Some(found) => found,
        None => refresh_detect(state, ws).await,
    };
    catalog()
        .iter()
        .filter(|m| on.contains(&m.id) && found.contains(&m.id))
        .collect()
}

/// Active plugins in the workspace of session `sid` (empty when the session
/// has no workspace).
pub(crate) async fn active_for_session(state: &AppState, sid: &str) -> Vec<&'static Manifest> {
    match workspace_of_session(state, sid) {
        Some(ws) => active(state, &ws).await,
        None => Vec::new(),
    }
}

/// MCP tools to pre-allow for a session spawned in `ws`: those of every
/// plugin active there, plus `tell_mastermind` when the workspace has a
/// Mastermind (empty — and so no settings change at all — when neither).
pub(crate) async fn spawn_allow(state: &AppState, ws: &str) -> Vec<String> {
    let mut tools: Vec<String> = active(state, ws)
        .await
        .iter()
        .flat_map(|m| m.provides.mcp_tools.iter().cloned())
        .collect();
    // A workspace with a Mastermind: its workers may message it without a
    // prompt (the Mastermind's own spawn carrying the entry is inert — the
    // tool is never offered to it).
    if crate::lock(&state.workspaces)
        .get(ws)
        .is_some_and(|w| w.mastermind.is_some())
    {
        tools.push("tell_mastermind".to_string());
    }
    tools
}

/// The workspace a session belongs to, if any.
pub(crate) fn workspace_of_session(state: &AppState, sid: &str) -> Option<String> {
    crate::lock(&state.session_workspaces).get(sid).cloned()
}

fn manifest_json(m: &Manifest) -> Value {
    json!({
        "id": m.id,
        "name": m.name,
        "summary": m.summary,
        "homepage": m.homepage,
        "adds": {"ui": m.adds.ui, "agents": m.adds.agents},
        "provides": {
            "knowledge": m.provides.knowledge,
            "mcp_tools": m.provides.mcp_tools,
            "views": m.provides.views,
        },
        "setup": m.setup.as_ref().map(|s| json!({"prompt": s.prompt})),
        "detect": m.detect.any,
    })
}

fn not_found(what: &str) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": what}))).into_response()
}

/// GET /plugins — the catalog (what exists on this daemon).
pub(crate) async fn list_plugins() -> Response {
    let plugins: Vec<Value> = catalog().iter().map(manifest_json).collect();
    Json(json!({"schema": 1, "plugins": plugins})).into_response()
}

/// GET /workspaces/{id}/plugins — per-plugin status in one workspace: on /
/// footprint detected / active. Agent-plugin requirement state (asked of the
/// agents themselves) rides `GET /workspaces/{id}/agent-plugins`.
pub(crate) async fn workspace_plugins(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&id) else {
        return not_found("unknown workspace");
    };
    let on: BTreeSet<String> = workspace.plugins_on.iter().cloned().collect();
    let found = refresh_detect(&state, &id).await;
    let plugins: Vec<Value> = catalog()
        .iter()
        .map(|m| {
            let mut v = manifest_json(m);
            let is_on = on.contains(&m.id);
            let detected = found.contains(&m.id);
            v["on"] = json!(is_on);
            v["detected"] = json!(detected);
            v["active"] = json!(is_on && detected);
            v["requires"] = json!(m
                .requires
                .agent_plugins
                .iter()
                .map(|(agent, req)| json!({
                    "agent": agent,
                    "id": req.id,
                    "marketplace": req.marketplace,
                }))
                .collect::<Vec<_>>());
            v
        })
        .collect();
    Json(json!({
        "schema": 1,
        "workspace_id": id,
        "root": workspace.root,
        "plugins": plugins,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct PutWorkspacePlugin {
    on: bool,
}

/// PUT /workspaces/{id}/plugins/{pid} {on} — switch a plugin on or off for
/// one workspace. Durable or refused: a toggle the next restart forgets
/// would silently change what agents see.
pub(crate) async fn put_workspace_plugin(
    State(state): State<Arc<AppState>>,
    AxPath((id, pid)): AxPath<(String, String)>,
    Json(body): Json<PutWorkspacePlugin>,
) -> Response {
    if manifest(&pid).is_none() {
        return not_found("unknown plugin");
    }
    let result = crate::lock(&state.workspaces).set_plugin_on(&id, &pid, body.on);
    match result {
        Ok(Some(workspace)) => {
            // The switch is the moment agents' view changes: re-detect now
            // so the next connect answers from a fresh footprint.
            refresh_detect(&state, &id).await;
            state.changes.notify_waiters();
            Json(json!({"workspace_id": id, "plugins_on": workspace.plugins_on})).into_response()
        }
        Ok(None) => not_found("unknown workspace"),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("could not save the workspace: {err}")})),
        )
            .into_response(),
    }
}
/// Plugin ids / marketplace sources we hand to an agent CLI: first-party
/// manifest strings, still charset-gated (and never flag-shaped) because they
/// land in a generated script.
fn cli_safe(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@/._:+-".contains(c))
}

#[derive(Deserialize)]
pub(crate) struct AgentBody {
    agent: String,
}

fn bad_request(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": msg.into()}))).into_response()
}

/// POST /workspaces/{id}/plugins/{pid}/install {agent} — install the agent
/// plugin this workbench plugin requires, with the AGENT's own plugin
/// manager, in a visible terminal the user watches (chimaera never
/// reimplements `claude plugin` / `codex plugin`). The session is theirs to
/// read and close; the probe cache is invalidated when it ends.
pub(crate) async fn install_requirement(
    State(state): State<Arc<AppState>>,
    AxPath((id, pid)): AxPath<(String, String)>,
    Json(body): Json<AgentBody>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&id) else {
        return not_found("unknown workspace");
    };
    let Some(m) = manifest(&pid) else {
        return not_found("unknown plugin");
    };
    let Some(req) = m.requires.agent_plugins.get(&body.agent) else {
        return bad_request(format!("{} needs nothing from {}", m.name, body.agent));
    };
    let Some(kind) = crate::agents::AgentKind::parse(&body.agent) else {
        return bad_request("unknown agent");
    };
    if !cli_safe(&req.id) || !cli_safe(&req.marketplace) {
        return bad_request("manifest requirement is not CLI-safe");
    }
    let det = crate::launcher::detect(&state, kind, false).await;
    let bin = match det.path {
        Ok(bin) => bin,
        Err(err) => {
            return (StatusCode::CONFLICT, Json(json!({"error": err}))).into_response();
        }
    };
    let bin = crate::runtimes::sq(&bin.to_string_lossy());
    let install_verb = if kind == crate::agents::AgentKind::Codex {
        "add"
    } else {
        "install"
    };
    let agent = kind.as_str();
    let script = format!(
        "echo 'Installing {name} for {agent} with {agent}'\''s own plugin manager.'\n         echo\n         echo '$ {agent} plugin marketplace add {mkt}'\n         {bin} plugin marketplace add {mkt_q} || echo '(already added or unavailable — continuing)'\n         echo\n         echo '$ {agent} plugin {install_verb} {pid_s}'\n         {bin} plugin {install_verb} {pid_q}\n         status=$?\n         echo\n         if [ $status -eq 0 ]; then echo 'Done — you can close this terminal.'; \
         else echo \"Install failed (exit $status).\"; fi\n         exit $status\n",
        name = m.name.replace('\'', ""),
        mkt = req.marketplace,
        mkt_q = crate::runtimes::sq(&req.marketplace),
        pid_s = req.id,
        pid_q = crate::runtimes::sq(&req.id),
    );
    let session_id = crate::agents::fresh_session_id();
    let env = crate::api::session_env(&state, &session_id, "dark", None);
    let env_remove = crate::api::spawn_env_remove(&env);
    let opts = chimaera_pty::SpawnOpts {
        cwd: workspace.root.clone(),
        name: Some(format!("install {} for {agent}", m.id)),
        cols: 100,
        rows: 24,
        // Login-shell wrap, like every agent spawn and the probes that found
        // this CLI: an npm-installed agent (`#!/usr/bin/env node`) needs the
        // user's PATH, which an app-launched daemon's own env lacks.
        command: Some(crate::launcher::wrap_login_shell(
            &crate::launcher::login_shell(),
            vec!["/bin/bash".to_string(), "-c".to_string(), script],
        )),
        id: Some(session_id.clone()),
        env,
        env_remove,
        scrollback: crate::lock(&state.settings).scrollback_lines(),
    };
    match state.sessions.spawn(opts) {
        Ok(info) => {
            crate::lock(&state.session_workspaces).insert(info.id.clone(), workspace.id.clone());
            // When the install ends, the agents' answers changed.
            let watch_state = state.clone();
            let sid = info.id.clone();
            tokio::spawn(async move {
                while watch_state.sessions.get(&sid).is_some() {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                watch_state.probes.invalidate();
                watch_state.changes.notify_waiters();
            });
            tracing::info!(workspace = %id, plugin = %pid, agent, "plugin requirement install started");
            state.changes.notify_waiters();
            Json(json!({"session_id": info.id})).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

/// POST /workspaces/{id}/plugins/{pid}/setup {agent} — a new chat session of
/// the user's chosen agent, sent the plugin's own documented setup prompt.
/// The user's click, their billing; the plugin's own tooling does the work.
pub(crate) async fn setup_workspace(
    State(state): State<Arc<AppState>>,
    AxPath((id, pid)): AxPath<(String, String)>,
    Json(body): Json<AgentBody>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&id) else {
        return not_found("unknown workspace");
    };
    let Some(m) = manifest(&pid) else {
        return not_found("unknown plugin");
    };
    let Some(setup) = &m.setup else {
        return bad_request(format!("{} has no setup step", m.name));
    };
    let Some(kind) = crate::agents::AgentKind::parse(&body.agent) else {
        return bad_request("unknown agent");
    };
    if !kind.chat_capable() {
        return bad_request(format!("no chat driver for {}", body.agent));
    }
    let theme = crate::lock(&state.settings)
        .map_cached()
        .get("appearance.theme")
        .and_then(|v| v.as_str())
        .filter(|t| *t == "light" || *t == "dark")
        .unwrap_or("dark")
        .to_string();
    let spawned = crate::chat::spawn_fresh_chat(
        &state,
        workspace,
        crate::chat::FreshChat {
            id: None,
            kind,
            model: None,
            name: Some(format!("{} setup", m.name)),
            title_hint: None,
            theme,
            prelude: None,
            mastermind: None,
            fork: None,
        },
    )
    .await;
    let row = match spawned {
        Ok(row) => row,
        Err(crate::chat::ChatSpawnFailure::AgentUnavailable(msg)) => {
            return (StatusCode::CONFLICT, Json(json!({"error": msg}))).into_response();
        }
        Err(crate::chat::ChatSpawnFailure::Internal(err)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("spawn failed: {err}")})),
            )
                .into_response();
        }
    };
    let Some(sid) = row["id"].as_str().map(str::to_owned) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "spawned session has no id"})),
        )
            .into_response();
    };
    let command = chimaera_agent::model::AgentCommand::Send {
        blocks: vec![chimaera_agent::model::ContentBlock::Text {
            text: setup.prompt.clone(),
        }],
    };
    if let Err(err) = state.chat.command(&sid, command).await {
        // The session exists; say what didn't happen rather than hiding it.
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"session_id": sid, "error": format!("setup prompt not delivered: {err}")})),
        )
            .into_response();
    }
    tracing::info!(workspace = %id, plugin = %pid, session = %sid, "plugin setup started");
    Json(json!({"session_id": sid})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_manifest_parses_and_ids_are_unique() {
        assert_eq!(
            catalog().len(),
            MANIFESTS.len(),
            "a manifest failed to parse"
        );
        let ids: BTreeSet<&str> = catalog().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids.len(), catalog().len());
        for m in catalog() {
            assert!(!m.summary.is_empty(), "{} needs a summary", m.id);
            assert!(
                !m.adds.ui.is_empty() || !m.adds.agents.is_empty(),
                "{} must say what it adds",
                m.id
            );
            for rel in &m.detect.any {
                assert!(
                    !rel.starts_with('/') && !rel.contains(".."),
                    "{}: detect paths are workspace-relative",
                    m.id
                );
            }
        }
    }

    #[test]
    fn cli_safe_rejects_flags_and_shell_metacharacters() {
        assert!(cli_safe("mycelium@mycelium"));
        assert!(cli_safe("arjunrajlaboratory/mycelium"));
        assert!(!cli_safe("--evil"));
        assert!(!cli_safe("a;rm -rf"));
        assert!(!cli_safe("$(x)"));
        assert!(!cli_safe(""));
        for m in catalog() {
            for req in m.requires.agent_plugins.values() {
                assert!(cli_safe(&req.id) && cli_safe(&req.marketplace), "{}", m.id);
            }
        }
    }

    #[test]
    fn unknown_manifest_fields_are_rejected() {
        let err = toml::from_str::<Manifest>(
            "id='x'\nname='x'\nsummary='x'\n[provides]\nmcp_tool=['typo']",
        );
        assert!(err.is_err());
    }

    #[test]
    fn detect_needs_a_real_footprint_and_ignores_symlinks() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-plugins-detect-{}-{}",
            std::process::id(),
            crate::timeline::now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let none = detect_blocking(&root);
        assert!(!none.contains("mycelium"));
        assert!(
            none.contains("agent-notes"),
            "no footprint = always present"
        );
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("findings")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, root.join(".living")).unwrap();
        assert!(!detect_blocking(&root).contains("mycelium"));
        std::fs::write(root.join("MYCELIUM.md"), "# protocol").unwrap();
        assert!(detect_blocking(&root).contains("mycelium"));
        let _ = std::fs::remove_dir_all(root);
    }
}
