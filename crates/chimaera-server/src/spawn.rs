//! The one spawn path for chimaera-owned sessions.
//!
//! `POST /api/v1/sessions` and boot resurrection (`ledger`) must produce
//! byte-identical sessions — same env injection, same hook wiring, same
//! login-shell wrap — or resurrected sessions would drift from freshly
//! spawned ones in exactly the ways that are hardest to notice (stale hook
//! ports, missing shims, un-themed TUIs). The HTTP handler owns request
//! validation; everything from "the request is valid" onward lives here.

use std::path::PathBuf;
use std::sync::Arc;

use crate::agents::AgentKind;
use crate::workspaces::Workspace;
use crate::AppState;

/// What to run in the session.
pub(crate) enum SpawnKind {
    /// The user's interactive shell, with shell integration when available.
    Shell,
    /// An agent TUI. `resume` is a claude conversation id (`--resume <id>`);
    /// callers guarantee it is only set for [`AgentKind::Claude`].
    Agent {
        kind: AgentKind,
        model: Option<String>,
        resume: Option<String>,
    },
}

/// A validated spawn request.
pub(crate) struct SpawnSpec {
    pub(crate) workspace: Workspace,
    /// Session id to (re)use. `None` mints a fresh one; resurrection passes
    /// the dead session's id so every layout tab referencing it rebinds.
    pub(crate) id: Option<String>,
    /// Pins the display name (`SessionInfo::renamed`); resurrection carries
    /// a user rename across the restart this way.
    pub(crate) name: Option<String>,
    /// Working directory override; `None` spawns at the workspace root.
    /// Resurrection passes the last polled cwd so shells come back where
    /// they were, not where they started.
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) cols: Option<u16>,
    pub(crate) rows: Option<u16>,
    /// "light" | "dark" (validated by the caller).
    pub(crate) theme: String,
    /// Provisional display title for agent sessions (resurrection carries
    /// the dead session's title so the rail row stays recognizable until
    /// the agent re-titles itself). Ignored for shells.
    pub(crate) title_hint: Option<String>,
    /// Launch-scope prelude text (concatenated after the host + workspace
    /// preludes — see `environment`). Not persisted in the ledger, so
    /// resurrection passes None: a resurrected session re-runs the durable
    /// scopes only.
    pub(crate) prelude: Option<String>,
    pub(crate) kind: SpawnKind,
}

/// Why a spawn could not happen.
pub(crate) enum SpawnFailure {
    /// The agent binary is missing/broken; the message is the detection
    /// error shown to the user (HTTP 409).
    AgentUnavailable(String),
    /// Everything else (HTTP 500).
    Internal(anyhow::Error),
}

/// Spawn a session per `spec` and register all its server-side state.
/// Returns the same session JSON `GET /sessions` would list it with.
pub(crate) async fn spawn_session(
    state: &Arc<AppState>,
    spec: SpawnSpec,
) -> Result<serde_json::Value, SpawnFailure> {
    let workspace = spec.workspace;
    // Every session gets a pre-picked id: it rides in the spawn env as
    // CHIMAERA_SESSION (shells too — typed agents need their session
    // context) and, for claude, in the hook URL.
    let id = spec.id.unwrap_or_else(crate::agents::fresh_session_id);
    let cwd = spec.cwd.unwrap_or_else(|| workspace.root.clone());
    // The user's environment prelude (host ⊕ workspace ⊕ launch), written
    // per session and sourced once by the shell rc / agent wrapper. Runs
    // per real spawn only — reconnects reattach to the live PTY.
    let prelude =
        crate::environment::materialize_prelude(state, &id, &workspace.id, spec.prelude.as_deref());
    let env = crate::api::session_env(state, &id, &spec.theme, prelude.as_deref());
    let env_remove = crate::api::spawn_env_remove(&env);
    let mut opts = chimaera_pty::SpawnOpts {
        cwd,
        name: spec.name,
        cols: spec.cols.map_or(80, |c| c.clamp(20, 500)),
        rows: spec.rows.map_or(24, |r| r.clamp(5, 200)),
        command: None,
        id: Some(id.clone()),
        env,
        env_remove,
        // settings.json ground truth; applies to sessions spawned from now on.
        scrollback: crate::lock(&state.settings).scrollback_lines(),
    };

    let mut spawned_agent = None;
    match &spec.kind {
        // Plain shells get shell integration injected (OSC 133 journal
        // marks); a failure to materialize the scripts degrades to a plain
        // spawn. Its env lands ON TOP of the session env (shims PATH,
        // CHIMAERA_*) — the two use disjoint variable sets, so nothing is
        // clobbered.
        SpawnKind::Shell => match chimaera_core::shellint::shell_launch() {
            Ok(launch) => {
                opts.command = Some(launch.argv);
                opts.env.extend(launch.env);
            }
            Err(err) => {
                tracing::warn!(%err, "shell integration unavailable; spawning plain shell");
            }
        },
        // Agent sessions: resolve the agent binary (cached, via the login
        // shell; user install first, managed fallback), and — for claude —
        // generate the per-session settings file that wires its hooks to
        // this daemon and carries the scheme theme.
        SpawnKind::Agent {
            kind: agent_kind,
            model,
            resume,
        } => {
            let agent_kind = *agent_kind;
            let bin = match crate::launcher::detect(state, agent_kind, false).await.path {
                Ok(path) => path,
                Err(msg) => return Err(SpawnFailure::AgentUnavailable(msg)),
            };
            let key = crate::agents::fresh_agent_key();
            // Hook injection is claude-only: other agents have no hook system
            // to wire, so their sessions stay honestly "unknown". The scheme
            // theme rides in the same settings file — unless the user's own
            // settings already set one (respect the explicit choice).
            let settings = if agent_kind == AgentKind::Claude {
                let (theme_set, user_statusline) = crate::runtimes::claude_settings_gates(
                    &state.claude_settings_path,
                    &workspace.root,
                )
                .await;
                let settings_theme = (!theme_set).then_some(spec.theme.as_str());
                // PTY TUI spawns are never the Mastermind (a chat-only role),
                // so no permissions block rides these settings.
                match crate::agents::write_settings(
                    &id,
                    &key,
                    state.port,
                    settings_theme,
                    user_statusline.as_ref(),
                    None,
                ) {
                    Ok(path) => Some(path),
                    Err(err) => {
                        tracing::error!(%err, "failed to write agent settings");
                        return Err(SpawnFailure::Internal(err));
                    }
                }
            } else {
                None
            };
            // Codex themes via `-c tui.theme=` (config-file override, verified
            // against codex 0.142.5); skipped when the user's own config.toml
            // picks a theme.
            let codex_theme = (agent_kind == AgentKind::Codex
                && !crate::runtimes::codex_user_theme_set(&state.codex_config_path))
            .then(|| crate::runtimes::codex_theme_name(&spec.theme));
            // Claude also carries the linked-terminals MCP config (per-session
            // endpoint + key); other agents' MCP integrations come later.
            let mcp_config = if agent_kind == AgentKind::Claude {
                match crate::agents::write_mcp_config(&id, &key, state.port) {
                    Ok(path) => Some(path),
                    Err(err) => {
                        tracing::error!(%err, "failed to write agent mcp config");
                        return Err(SpawnFailure::Internal(err));
                    }
                }
            } else {
                None
            };
            // Codex resume is a subcommand (`codex resume <thread>`), not a
            // flag. Fresh Codex and every Claude/Gemini spawn keep the normal
            // builder; the dedicated resume builder pins Codex's argv order.
            let mut argv = if agent_kind == AgentKind::Codex && resume.is_some() {
                crate::launcher::build_agent_resume_command(
                    agent_kind,
                    &bin,
                    settings.as_deref(),
                    model.as_deref(),
                    resume.as_deref(),
                    None,
                    None,
                    codex_theme,
                )
            } else {
                crate::launcher::build_agent_command(
                    agent_kind,
                    &bin,
                    settings.as_deref(),
                    model.as_deref(),
                    resume.as_deref(),
                    codex_theme,
                )
            };
            if let Some(mcp) = &mcp_config {
                argv.push("--mcp-config".to_string());
                argv.push(mcp.to_string_lossy().into_owned());
            }
            // Login-shell wrap: agents must see the user's terminal environment
            // (exported API keys, nvm PATHs) — the daemon's own env never
            // sourced their profile.
            opts.command = Some(crate::launcher::wrap_login_shell(
                &crate::launcher::login_shell(),
                argv,
            ));
            // Register the record before spawning so no hook can beat it in.
            let mut record = crate::agents::AgentRecord::new(key, agent_kind);
            // Claude forks a new session id on --resume; remember the ancestor
            // so recents can hide (and later supersede) the old conversation.
            record.resumed_from = resume.clone();
            // A carried-over title slots in as the provisional first-prompt
            // name: it loses to any real title the agent produces, exactly
            // like a first prompt would.
            record.first_prompt = spec.title_hint.clone();
            crate::lock(&state.agents).insert(id.clone(), record);
            spawned_agent = Some(agent_kind);
        }
    }

    match state.sessions.spawn(opts) {
        Ok(info) => {
            crate::lock(&state.session_workspaces).insert(info.id.clone(), workspace.id.clone());
            // Remember the spawn theme: resurrection re-themes the session's
            // successor with it (there is no other durable record of it).
            crate::lock(&state.session_themes).insert(info.id.clone(), spec.theme.clone());
            let mut polled = None;
            if spawned_agent.is_some() {
                crate::agents::spawn_agent_watch(state.clone(), info.id.clone());
            } else {
                // Prime the display name (a fresh shell sits at the root, so
                // it is the shell itself) and start the naming watcher.
                let shell = crate::naming::default_shell_name();
                crate::lock(&state.display_names).insert(info.id.clone(), shell.clone());
                polled = Some(shell);
                crate::naming::spawn_shell_watch(state.clone(), info.id.clone());
            }
            state.changes.notify_waiters();
            let agent = crate::lock(&state.agents).get(&info.id).cloned();
            Ok(crate::session_view::session_json(
                &info,
                Some(workspace.id),
                agent.as_ref(),
                polled.as_deref(),
                None,  // fresh session: cwd_current is the spawn cwd
                None,  // no exec in flight
                false, // a fresh PTY spawn is never a bound Mastermind
            ))
        }
        Err(err) => {
            crate::lock(&state.agents).remove(&id);
            tracing::error!(%err, "failed to spawn session");
            Err(SpawnFailure::Internal(err))
        }
    }
}
