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
pub enum SpawnKind {
    /// The user's interactive shell, with shell integration when available.
    Shell,
    /// An agent TUI. `resume` is a Claude conversation id or Codex thread id.
    Agent {
        kind: AgentKind,
        model: Option<String>,
        resume: Option<String>,
    },
}

/// A validated spawn request.
pub struct SpawnSpec {
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
    pub(crate) fork_head: bool,
    pub(crate) native_cwd: Option<PathBuf>,
    /// Who started it, for an agent session's history record (shells keep
    /// none).
    pub(crate) started_by: crate::history::StartedBy,
}

/// Why a spawn could not happen.
pub enum SpawnFailure {
    /// The agent binary is missing/broken; the message is the detection
    /// error shown to the user (HTTP 409).
    AgentUnavailable(String),
    /// Everything else (HTTP 500).
    Internal(anyhow::Error),
}

/// Spawn a session per `spec` and register all its server-side state.
/// Returns the same session JSON `GET /sessions` would list it with.
pub async fn spawn_session(
    state: &Arc<AppState>,
    spec: SpawnSpec,
) -> Result<serde_json::Value, SpawnFailure> {
    let workspace = spec.workspace;
    let shell = matches!(spec.kind, SpawnKind::Shell);
    let need = if shell {
        crate::policy::Need::Shell
    } else {
        crate::policy::Need::Execute
    };
    let allowed = |state: &AppState, workspace: &str| state.policy().allows(state, workspace, need);
    if !allowed(state, &workspace.id) {
        return Err(SpawnFailure::Internal(anyhow::anyhow!(
            "workspace owned elsewhere"
        )));
    }
    // Every session gets a pre-picked id: it rides in the spawn env as
    // CHIMAERA_SESSION (shells too — typed agents need their session
    // context) and, for claude, in the hook URL.
    let id = spec.id.unwrap_or_else(crate::agents::fresh_session_id);
    let cwd = spec.cwd.unwrap_or_else(|| workspace.root.clone());
    let usage;
    // The user's environment prelude (startup ⊕ host ⊕ workspace ⊕ launch),
    // written per session and sourced once by the shell rc / agent wrapper.
    // Runs per real spawn only — reconnects reattach to the live PTY.
    let startup = crate::environment::job_startup().await;
    let prelude = crate::environment::materialize_prelude(
        state,
        &id,
        &workspace.id,
        spec.prelude.as_deref(),
        startup.as_deref(),
    );
    let env = crate::api::session_env(state, &id, &spec.theme, prelude.as_deref());
    let env_remove = crate::api::spawn_env_remove(&env);
    let mut opts = chimaera_pty::SpawnOpts {
        cwd,
        name: spec.name,
        cols: spec
            .cols
            .map_or(80, |c| c.clamp(20, chimaera_pty::MAX_TERMINAL_COLS)),
        rows: spec
            .rows
            .map_or(24, |r| r.clamp(5, chimaera_pty::MAX_TERMINAL_ROWS)),
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
        SpawnKind::Shell => {
            usage = crate::runtime_retention::acquire(state, None, vec![])
                .await
                .map_err(SpawnFailure::Internal)?
                .0;
            match chimaera_core::shellint::shell_launch() {
                Ok(launch) => {
                    opts.command = Some(launch.argv);
                    opts.env.extend(launch.env);
                }
                Err(err) => {
                    tracing::warn!(%err, "shell integration unavailable; spawning plain shell");
                }
            }
        }
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
            let (guard, mut binaries) =
                crate::runtime_retention::acquire(state, Some(agent_kind), vec![bin])
                    .await
                    .map_err(SpawnFailure::Internal)?;
            usage = guard;
            let bin = binaries.remove(0);
            let key = crate::agents::fresh_agent_key();
            // Claude's hooks drive attention state. Codex's notify below
            // captures identity only; its attention stays "unknown". The scheme
            // theme rides in the same settings file — unless the user's own
            // settings already set one (respect the explicit choice).
            let settings = if agent_kind == AgentKind::Claude {
                let (theme_set, user_statusline) = crate::runtimes::claude_settings_gates(
                    &state.claude_settings_path,
                    &workspace.root,
                )
                .await;
                let settings_theme = (!theme_set).then_some(spec.theme.as_str());
                let plugin_tools = crate::plugins::spawn_allow(state, &workspace.id).await;
                // PTY TUI spawns are never the Mastermind (a chat-only role);
                // only active plugins' tools may ride a permissions block.
                match crate::agents::write_settings(
                    &id,
                    &key,
                    state.port,
                    settings_theme,
                    user_statusline.as_ref(),
                    None,
                    &plugin_tools,
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
            // Codex TUIs reach the chimaera endpoint while agent
            // communication is on (default) or a plugin with tools is active
            // here (`spawn_allow`); with neither, the argv and env stay
            // exactly what they were.
            // A workspace policy may give a project's terminal agents the
            // daemon's tools too (`LaunchContext::tools`); otherwise that
            // opt-in boundary stands.
            let codex_plugin_tools = if agent_kind == AgentKind::Codex {
                crate::plugins::spawn_allow(state, &workspace.id).await
            } else {
                Vec::new()
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
                    None,
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
            if spec.fork_head {
                if resume.is_none() {
                    return Err(SpawnFailure::Internal(anyhow::anyhow!(
                        "native fork requires resume"
                    )));
                }
                if let Err(error) = crate::launcher::fork_native_head(agent_kind, &mut argv) {
                    return Err(SpawnFailure::Internal(error));
                }
            }
            if let Some(mcp) = &mcp_config {
                argv.push("--mcp-config".to_string());
                argv.push(mcp.to_string_lossy().into_owned());
            }
            let mut codex_identity = false;
            if agent_kind == AgentKind::Codex {
                let notify = state
                    .policy()
                    .codex_notify_args(state, &workspace.id, &id, &key)
                    .await;
                // The rollout identity only serves the notify shim; without
                // it a Codex TUI carries no transcript.
                codex_identity = !notify.is_empty();
                argv.extend(notify);
            }
            if !codex_plugin_tools.is_empty()
                || (agent_kind == AgentKind::Codex
                    && state.policy().launch_context(state, &workspace.id).tools)
            {
                // Pre-approved: the prompt-free tools every session gets
                // (`notify`) plus the active plugins' own.
                let approve: Vec<String> = crate::mcp::ALWAYS_ALLOWED_TOOLS
                    .iter()
                    .map(|t| t.to_string())
                    .chain(codex_plugin_tools)
                    .collect();
                // `-c` trails `resume <thread>` too, like the theme override.
                argv.extend(crate::launcher::codex_tui_mcp_args(
                    &crate::agents::mcp_url_bare(&id, state.port),
                    &approve,
                ));
                // The key rides the env, never world-readable argv; env is
                // applied after env_remove, so nothing strips it.
                opts.env
                    .push((crate::launcher::CODEX_MCP_KEY_ENV.to_string(), key.clone()));
            }
            if agent_kind == AgentKind::Claude && state.policy().updates_managed(state) {
                // Agents that come with the machine's image are updated with
                // it: claude's own updater could only fail there (the image
                // prefix is not the daemon user's to write) and say so mid-turn.
                opts.env
                    .push(("DISABLE_AUTOUPDATER".to_string(), "1".to_string()));
            }
            let transferred = crate::lock(&state.deferred_sessions).get(&id).cloned();
            if let Some(entry) = transferred.filter(|entry| entry.workspace_id == workspace.id) {
                // A positional prompt starts a billed turn: only a terminal
                // agent whose turn was cut off by the move gets one.
                let pickup = entry.handoff.is_some().then(|| {
                    tui_pickup(
                        entry.agent.as_ref().and_then(|a| a.carryover.as_ref()),
                        state.policy().launch_context(state, &workspace.id).recovery,
                    )
                });
                if let Some(context) = pickup.flatten() {
                    crate::launcher::append_transfer_prompt(&mut argv, &context);
                }
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
            // Older claude CLIs fork a new session id on --resume (2.1.283
            // keeps it); remember the ancestor so recents can hide (and later
            // supersede) the old conversation either way.
            record.resumed_from = resume.clone();
            record.native_cwd = spec.native_cwd.clone();
            if codex_identity {
                if let Some(thread) = resume.clone() {
                    if let Some(home) = state
                        .codex_config_path
                        .parent()
                        .map(std::path::Path::to_path_buf)
                    {
                        let cwd = record
                            .native_cwd_for(&thread)
                            .unwrap_or_else(|| opts.cwd.clone());
                        let sought = thread.clone();
                        let path = tokio::task::spawn_blocking(move || {
                            crate::codex_rollout::find_rollout(&home, &sought, &cwd)
                        })
                        .await
                        .ok()
                        .flatten();
                        if let Some(path) = path {
                            record.codex_thread_id = Some(thread);
                            record.transcript_path = Some(path);
                        }
                    }
                }
            }
            // A carried-over title slots in as the provisional first-prompt
            // name: it loses to any real title the agent produces, exactly
            // like a first prompt would.
            record.first_prompt = spec.title_hint.clone();
            crate::lock(&state.agents).insert(id.clone(), record);
            spawned_agent = Some(agent_kind);
        }
    }

    if !allowed(state, &workspace.id) {
        return Err(SpawnFailure::Internal(anyhow::anyhow!(
            "project execution authority changed during launch"
        )));
    }
    // Shells share the short launch/registration gate with agents.
    let kind = if spawned_agent.is_some() {
        crate::policy::LaunchKind::Agent
    } else {
        crate::policy::LaunchKind::Shell
    };
    let (launch, _reservation) = state
        .policy()
        .admit_launch(state, &workspace.id, kind)
        .await
        .map_err(SpawnFailure::Internal)?;
    let managed = launch.managed();
    state
        .policy()
        .launch_env(state, &workspace.id, &mut opts.env, &mut opts.env_remove)
        .await
        .map_err(SpawnFailure::Internal)?;
    let native = match &spec.kind {
        SpawnKind::Agent { resume, .. } => resume.as_deref(),
        SpawnKind::Shell => None,
    };
    if let SpawnKind::Agent { kind, .. } = &spec.kind {
        crate::ledger::check_manual_native(state, Some(&id), *kind, native)
            .map_err(SpawnFailure::Internal)?;
    }
    let import_admission = state
        .policy()
        .hold_session(state, &workspace.id, &id, native, false)
        .map_err(SpawnFailure::Internal)?;
    let spawned = if managed {
        state.sessions.spawn_managed(opts)
    } else {
        state.sessions.spawn(opts)
    };
    drop(import_admission);
    match spawned {
        Ok(info) => {
            crate::runtime_retention::watch(state.clone(), info.id.clone(), usage);
            crate::lock(&state.session_workspaces).insert(info.id.clone(), workspace.id.clone());
            if !allowed(state, &workspace.id) {
                let _ = state.sessions.kill(&info.id);
                return Err(SpawnFailure::Internal(anyhow::anyhow!(
                    "project execution authority changed during launch"
                )));
            }
            launch.registered(info.id.clone());
            // Remember the spawn theme: resurrection re-themes the session's
            // successor with it (there is no other durable record of it).
            crate::lock(&state.session_themes).insert(info.id.clone(), spec.theme.clone());
            let mut polled = None;
            if spawned_agent.is_some() {
                crate::agents::spawn_agent_watch(
                    state.clone(),
                    info.id.clone(),
                    spec.started_by.clone(),
                );
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

/// The one message a moved terminal agent starts with, shown in its terminal
/// as the user's own line: only when a turn was cut off by the move (idle
/// conversations resume silently), in plain words that name no machines.
/// After an abrupt loss (`recovery`) it also asks the agent to check what
/// already happened before repeating anything.
pub(crate) fn tui_pickup(
    carry: Option<&chimaera_agent::Carryover>,
    recovery: bool,
) -> Option<String> {
    let carry = carry.filter(|carry| carry.turn_in_flight || carry.interrupted_work())?;
    let mut text = String::from(
        "Continuing here. The previous run stopped while this task was underway; please continue it.",
    );
    if recovery {
        text.push_str(
            " Some of that work may have happened after the last saved point: check the project and any external effects before repeating a step.",
        );
    }
    if !carry.background.is_empty() {
        text.push_str(
            " Background tasks that were running have stopped; restart the ones still needed.",
        );
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_cut_off_turn_gets_a_neutral_pickup_prompt() {
        let idle = chimaera_agent::Carryover::default();
        assert_eq!(
            tui_pickup(None, false),
            None,
            "unknown means no billed turn"
        );
        assert_eq!(tui_pickup(None, true), None);
        assert_eq!(tui_pickup(Some(&idle), true), None);
        let busy = chimaera_agent::Carryover {
            turn_in_flight: true,
            ..Default::default()
        };
        for recovery in [false, true] {
            let text = tui_pickup(Some(&busy), recovery).unwrap();
            assert!(text.starts_with("Continuing here."));
            for word in ["host", "transfer", "laptop", "cloud", "Chimaera"] {
                assert!(!text.contains(word), "{word}: {text}");
            }
        }
    }
}
