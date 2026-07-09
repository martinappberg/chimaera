//! The agent launcher's server surface (DESIGN.md "The agent launcher"):
//!
//! - `GET /api/v1/agents` — the known-agent catalog (Claude Code, Codex,
//!   Gemini CLI) joined with what this host actually has: installed
//!   (login-shell `command -v`, cached for the daemon's lifetime,
//!   `?refresh=true` bypasses after an install), version (`--version`,
//!   2s budget, first line), the curated model list, and the install hint
//!   the UI pre-types — never executes — into a fresh terminal.
//! - `GET /api/v1/agents/claude/sessions?workspace_id=` — resumable Claude
//!   sessions for the workspace's cwd, read from the same
//!   `~/.claude/projects` JSONL store the naming pipeline tails.
//!
//! `POST /api/v1/sessions` consumes the catalog too (agent/model/resume);
//! that stays in `api.rs`, with argv assembly here so it is unit-testable.
//!
//! Detection resolves each binary through the user's *login* shell.
//! Field-tested: `claude` is not on the non-interactive ssh PATH on HPC
//! login nodes, so a plain `which` from the daemon's environment is not
//! enough.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::agents::AgentKind;
use crate::AppState;

/// Curated per-agent model list: `(id, label)` pairs, default first. The id
/// is passed verbatim as `--model <id>` (all three CLIs take the flag).
pub(crate) fn models(kind: AgentKind) -> &'static [(&'static str, &'static str)] {
    match kind {
        // Claude Code resolves the opus/sonnet/haiku aliases to the latest
        // snapshots itself, so this list never goes stale.
        AgentKind::Claude => &[("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")],
        // Source: the user config codex-cli 0.142.5 writes on this machine
        // (`model = "gpt-5.5"`). The app-server likely exposes a model list
        // (the official extension shows one) — adopt it when the codex
        // driver grows a models probe.
        AgentKind::Codex => &[("gpt-5.5", "GPT-5.5")],
        // agy picks its own model; no curated list until its integration.
        AgentKind::Antigravity => &[],
        // Static list (gemini is not installed here to probe): the models
        // the gemini-cli README documents for interactive use.
        AgentKind::Gemini => &[
            ("gemini-2.5-pro", "Gemini 2.5 Pro"),
            ("gemini-2.5-flash", "Gemini 2.5 Flash"),
        ],
    }
}

/// Install command for a missing agent — pre-typed (never executed) into a
/// fresh terminal session; the user reviews and presses Enter.
pub(crate) fn install_command(kind: AgentKind) -> &'static str {
    match kind {
        // The official installer script (claude.ai/install.sh), not npm:
        // it needs no node on the host, which matters on HPC login nodes.
        AgentKind::Claude => "curl -fsSL https://claude.ai/install.sh | bash",
        AgentKind::Codex => "npm install -g @openai/codex",
        // Single static binary, no node — same shape as claude's installer
        // (matters on HPC login nodes).
        AgentKind::Antigravity => "curl -fsSL https://antigravity.google/cli/install.sh | bash",
        AgentKind::Gemini => "npm install -g @google/gemini-cli",
    }
}

/// Docs URL shown next to the install action.
pub(crate) fn docs_url(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "https://docs.claude.com/en/docs/claude-code/setup",
        AgentKind::Codex => "https://developers.openai.com/codex/cli",
        AgentKind::Antigravity => "https://antigravity.google/docs",
        AgentKind::Gemini => "https://github.com/google-gemini/gemini-cli",
    }
}

/// An installed build the launcher should offer to UPDATE rather than run
/// blind: the npm-era TypeScript codex (0.1.x) predates `codex login` and
/// only knows `OPENAI_API_KEY` auth — found in the field when a spawn died
/// in ~400ms with "Missing OpenAI API key" on a host with no key anywhere.
pub(crate) fn is_outdated(kind: AgentKind, version: Option<&str>) -> bool {
    kind == AgentKind::Codex && version.is_some_and(|v| v.starts_with("0.1."))
}

/// One cached detection result: where the binary lives (or why it could not
/// be found), the first line of its `--version` output, and whether the
/// resolved binary is a chimaera-managed install (lives under
/// `~/.chimaera/agents`).
#[derive(Clone, Debug)]
pub(crate) struct AgentDetection {
    pub(crate) path: Result<PathBuf, String>,
    pub(crate) version: Option<String>,
    pub(crate) managed: bool,
    /// The resolved binary is the user's explicit `agents.<kind>.path` setting
    /// (a runnable one) — surfaced so the launcher can label provenance as the
    /// path you set, distinct from "yours" (PATH) or "chimaera" (managed).
    pub(crate) explicit: bool,
}

/// Detect one agent binary, served from the daemon-lifetime cache unless
/// `refresh` bypasses it (the launcher refreshes after an install). The
/// user's own PATH install wins; the managed bin dir is the fallback when
/// the login shell misses — so spawns prefer the user's binary and pick up
/// a managed install the moment it lands.
pub(crate) async fn detect(state: &AppState, kind: AgentKind, refresh: bool) -> AgentDetection {
    if !refresh {
        if let Some(hit) = crate::lock(&state.agent_bins).get(&kind) {
            return hit.clone();
        }
    }
    let explicit = crate::lock(&state.settings).agent_path(kind);
    let path = resolve_bin(
        kind,
        &crate::runtimes::managed_bin_dir(&state.managed_root),
        explicit.clone(),
    )
    .await;
    let version = match &path {
        Ok(bin) => probe_version(bin).await,
        Err(_) => None,
    };
    let managed = path
        .as_ref()
        .is_ok_and(|p| crate::runtimes::is_managed(p, &state.managed_root));
    // The explicit path was set AND it's what resolved (resolve_bin returns it
    // verbatim only when runnable; a typo falls through to normal resolution).
    let used_explicit = explicit.as_deref().is_some_and(|e| {
        path.as_ref()
            .is_ok_and(|p| p.as_os_str() == std::ffi::OsStr::new(e))
    });
    let detection = AgentDetection {
        path,
        version,
        managed,
        explicit: used_explicit,
    };
    crate::lock(&state.agent_bins).insert(kind, detection.clone());
    detection
}

/// The managed install of `bin` under the managed bin dir, if present and
/// executable — detection's fallback when the login shell misses.
pub(crate) fn managed_fallback(bin: &str, managed_bin: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = managed_bin.join(bin);
    let meta = std::fs::metadata(&path).ok()?;
    (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(path)
}

/// Whether `path` is a runnable regular file (follows symlinks).
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Resolve an agent binary: an explicit `agents.<kind>.path` wins when runnable,
/// then the user's login shell (`command -v`), then the managed bin dir when the
/// shell misses (managed installs are deliberately NOT on the user's PATH —
/// dotfiles stay theirs).
async fn resolve_bin(
    kind: AgentKind,
    managed_bin: &Path,
    explicit: Option<String>,
) -> Result<PathBuf, String> {
    // An explicit path the user set is authoritative when it's actually
    // runnable. A stale/typo'd one falls through to normal resolution rather
    // than hard-failing every spawn — the launcher surfaces the setting either
    // way, so a mistake is visible without bricking the agent.
    if let Some(p) = explicit.as_deref() {
        let path = PathBuf::from(p);
        if is_executable_file(&path) {
            return Ok(path);
        }
    }
    let bin = kind.as_str();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let output = tokio::process::Command::new(&shell)
        .arg("-lc")
        .arg(format!("command -v {bin}"))
        .output()
        .await;
    let not_found = || {
        format!(
            "{bin} not found via `{shell} -lc 'command -v {bin}'`; install {name} \
             (`{cmd}`, see {url}) or make `{bin}` resolvable from your login shell",
            name = kind.product_name(),
            cmd = install_command(kind),
            url = docs_url(kind),
        )
    };
    let user = match output {
        Ok(out) if out.status.success() => {
            // Login shells may print banners; the path is the last non-empty line.
            let stdout = String::from_utf8_lossy(&out.stdout);
            let path = stdout
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            if !path.starts_with('/') {
                Err(not_found())
            } else {
                let path = PathBuf::from(path);
                if kind == AgentKind::Antigravity && agy_is_ide_shim(&path) {
                    // The Antigravity IDE ships an `agy` symlink to its own
                    // app launcher (like VS Code's `code`): it opens the GUI
                    // and exits 0 silently — NOT the CLI. Field-found:
                    // spawning it made a pane that just said "[exited]".
                    Err(format!(
                        "the `agy` on your PATH is the Antigravity IDE's app launcher, \
                         not the Antigravity CLI; install the CLI (`{cmd}`, see {url})",
                        cmd = install_command(kind),
                        url = docs_url(kind),
                    ))
                } else {
                    Ok(path)
                }
            }
        }
        Ok(_) => Err(not_found()),
        Err(err) => Err(format!("failed to run login shell {shell}: {err}")),
    };
    // The user's own install wins; a managed install fills the miss.
    match user {
        Ok(path) => Ok(path),
        Err(msg) => managed_fallback(bin, managed_bin).ok_or(msg),
    }
}

/// True when a resolved `agy` is the Antigravity IDE's app launcher rather
/// than the standalone CLI. Two platform-spanning signals on the
/// canonicalized target: the macOS bundle (`…/Antigravity.app/…`), and the
/// launcher binary's own name — the IDE ships `antigravity` (VS Code's
/// `code` pattern; users symlink it to `agy` by hand on Linux), while the
/// real CLI binary is `agy` itself.
fn agy_is_ide_shim(path: &Path) -> bool {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    target
        .components()
        .any(|c| c.as_os_str().to_string_lossy() == "Antigravity.app")
        || target.file_name().is_some_and(|f| f == "antigravity")
}

/// Version-probe budget: node-backed CLIs take ~1s to boot; anything past
/// 2s is "no version" rather than a stalled launcher.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// First non-empty line of `<bin> --version`, or `None` on any failure.
async fn probe_version(bin: &Path) -> Option<String> {
    let output = tokio::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(VERSION_TIMEOUT, output)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

#[derive(Deserialize)]
pub(crate) struct AgentsQuery {
    #[serde(default)]
    refresh: bool,
}

/// GET /api/v1/agents[?refresh=true] — the launcher's agent rows.
pub(crate) async fn list_agents(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AgentsQuery>,
) -> Json<serde_json::Value> {
    // Detect concurrently: three login shells plus version probes on a cold
    // cache would otherwise serialize into a visible popover delay.
    let detections = futures::future::join_all(AgentKind::ALL.into_iter().map(|kind| {
        let state = state.clone();
        async move { (kind, detect(&state, kind, query.refresh).await) }
    }))
    .await;

    let rows = detections
        .into_iter()
        .map(|(kind, detection)| {
            let mut row = serde_json::Map::new();
            row.insert("id".into(), json!(kind.as_str()));
            row.insert("name".into(), json!(kind.product_name()));
            row.insert("installed".into(), json!(detection.path.is_ok()));
            if let Ok(path) = &detection.path {
                row.insert("path".into(), json!(path));
            }
            if let Some(version) = &detection.version {
                row.insert("version".into(), json!(version));
            }
            if detection.path.is_ok() && is_outdated(kind, detection.version.as_deref()) {
                // Installed but too old to run usefully: the UI offers the
                // install command as an UPDATE instead of spawning blind.
                row.insert("outdated".into(), json!(true));
            }
            if detection.managed {
                // The resolved binary is a chimaera-managed install under
                // ~/.chimaera/agents (the user has none of their own).
                row.insert("managed".into(), json!(true));
            }
            if detection.explicit {
                // Resolved from the user's explicit agents.<id>.path setting.
                row.insert("explicit".into(), json!(true));
            }
            // Whether POST /agents/{id}/install has a curated managed
            // install for this agent (gemini needs a node runtime: phase 2).
            row.insert(
                "managed_install".into(),
                json!(crate::runtimes::install_script(kind, &state.managed_root).is_some()),
            );
            let models: Vec<_> = models(kind)
                .iter()
                .map(|(id, label)| json!({"id": id, "label": label}))
                .collect();
            row.insert("models".into(), json!(models));
            // Whether this agent can spawn as a structured chat session
            // (claude stream-json / codex app-server drivers).
            row.insert(
                "chat_capable".into(),
                json!(
                    kind.chat_capable()
                        && detection.path.is_ok()
                        && !is_outdated(kind, detection.version.as_deref())
                ),
            );
            row.insert(
                "install".into(),
                json!({"command": install_command(kind), "url": docs_url(kind)}),
            );
            serde_json::Value::Object(row)
        })
        .collect();
    Json(serde_json::Value::Array(rows))
}

/// Charset guard for model/resume ids that land in argv. Argv cannot
/// shell-inject, but a flag-shaped value ("--dangerously-…") or control
/// bytes have no business in either field.
pub(crate) fn safe_arg(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Argv for an agent session. Claude gets the hook-injecting `--settings`
/// (which also carries the scheme theme) plus `--model`/`--resume` when
/// given; codex gets `-c tui.theme=<name>` when a theme should be injected
/// (verified against codex 0.142.5 — see `runtimes`); gemini/agy spawn
/// their TUIs plain — no hooks, `--model` when given, never `--resume`
/// (claude-only, enforced by the create handler).
pub(crate) fn build_agent_command(
    kind: AgentKind,
    bin: &Path,
    settings: Option<&Path>,
    model: Option<&str>,
    resume: Option<&str>,
    codex_theme: Option<&str>,
) -> Vec<String> {
    debug_assert!(
        resume.is_none() || kind == AgentKind::Claude,
        "resume is claude-only"
    );
    debug_assert!(
        settings.is_none() || kind == AgentKind::Claude,
        "hook settings are claude-only"
    );
    debug_assert!(
        codex_theme.is_none() || kind == AgentKind::Codex,
        "tui.theme is codex-only"
    );
    let mut cmd = vec![bin.to_string_lossy().into_owned()];
    if let Some(settings) = settings {
        cmd.push("--settings".to_string());
        cmd.push(settings.to_string_lossy().into_owned());
    }
    if let Some(theme) = codex_theme {
        cmd.push("-c".to_string());
        cmd.push(format!("tui.theme={theme}"));
    }
    if let Some(model) = model {
        cmd.push("--model".to_string());
        cmd.push(model.to_string());
    }
    if let Some(resume) = resume {
        cmd.push("--resume".to_string());
        cmd.push(resume.to_string());
    }
    cmd
}

/// Argv for a structured chat session (claude stream-json driver): the
/// protocol flags come from `chimaera_agent::claude::chat_args` (live-
/// verified against the pinned CLI version there), plus the same per-session
/// `--settings`/`--mcp-config` files the TUI spawn uses — hooks and linked
/// terminals work identically in both surfaces. `session_uuid` pins the
/// native session id at spawn (`--session-id`); resumes leave it `None`
/// because claude forks a fresh id on `--resume`.
pub(crate) fn build_chat_command(
    bin: &Path,
    settings: &Path,
    mcp_config: &Path,
    model: Option<&str>,
    resume: Option<&str>,
    session_uuid: Option<&str>,
    fork_at: Option<&str>,
) -> Vec<String> {
    debug_assert!(
        resume.is_none() || session_uuid.is_none(),
        "resume forks a new native id; pinning one is contradictory"
    );
    let mut cmd = vec![bin.to_string_lossy().into_owned()];
    cmd.extend(chimaera_agent::claude::chat_args(model, resume));
    cmd.push("--settings".to_string());
    cmd.push(settings.to_string_lossy().into_owned());
    cmd.push("--mcp-config".to_string());
    cmd.push(mcp_config.to_string_lossy().into_owned());
    if let Some(uuid) = session_uuid {
        cmd.push("--session-id".to_string());
        cmd.push(uuid.to_string());
    }
    // Checkpoint rewind: resume the transcript as a fork truncated at the
    // given message uuid (rides with --resume; the fork gets a new id).
    if let Some(at) = fork_at {
        cmd.push("--fork-session".to_string());
        cmd.push("--resume-session-at".to_string());
        cmd.push(at.to_string());
    }
    cmd
}

/// Wrap an agent argv in the user's login shell so the TUI gets the same
/// environment as a hand-opened terminal: nvm PATHs, exported API keys.
/// Found in the field — codex reads `OPENAI_API_KEY` from the environment
/// (typically exported in ~/.zshrc) and exits within ~400ms without it,
/// because the daemon's own environment never sourced the user's profile.
/// `exec` keeps the agent as the PTY's direct child (signals, exit status,
/// and foreground polling all see the real process). The `"$0" "$@"` form
/// passes the argv through untouched — nothing is re-quoted or interpolated
/// into the script.
///
/// The script re-prepends the shim dir (`$CHIMAERA_SHIMS`, set in the spawn
/// env) before exec'ing: login-shell init reorders the injected PATH —
/// measured on this machine, macOS path_helper plus the user's profile
/// demoted the shim dir from front to position 16 — so the front slot is
/// reclaimed after the profile has run. A duplicate entry further back is
/// harmless.
pub(crate) fn wrap_login_shell(shell: &str, argv: Vec<String>) -> Vec<String> {
    let script = if shell.rsplit('/').next() == Some("fish") {
        // fish has no `$0`/`$@`; -c puts trailing args in $argv instead.
        r#"test -n "$CHIMAERA_SHIMS"; and set -gx PATH $CHIMAERA_SHIMS $PATH
exec $argv"#
            .to_string()
    } else {
        r#"if [ -n "${CHIMAERA_SHIMS:-}" ]; then PATH="$CHIMAERA_SHIMS:$PATH"; export PATH; fi
exec "$0" "$@""#
            .to_string()
    };
    let mut cmd = vec![shell.to_string(), "-lc".to_string(), script];
    cmd.extend(argv);
    cmd
}

/// The user's login shell (the daemon runs as the user, so `$SHELL` is it).
pub(crate) fn login_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

// --- resumable sessions ------------------------------------------------------

/// Claude's project-store directory name for a cwd: every non-alphanumeric
/// character replaced by '-'. Verified against the real store on this
/// machine: `/Users/x/dev/chimaera` -> `-Users-x-dev-chimaera`, and a
/// `.claude-worktrees` component encodes as `--claude-worktrees` (the dot
/// becomes a dash too).
pub(crate) fn encode_cwd(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Cap on the resume list (the UI adds search past ~8 entries).
const RESUMABLE_CAP: usize = 20;

#[derive(Deserialize)]
pub(crate) struct ResumablesQuery {
    workspace_id: String,
}

/// GET /api/v1/agents/claude/sessions?workspace_id= — resumable Claude
/// sessions for the workspace's cwd. Newest first, capped. Transcripts
/// already attached to a live agent session are excluded (offering to
/// resume a session that is open in another pane is a foot-gun).
pub(crate) async fn claude_resumables(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ResumablesQuery>,
) -> Response {
    let Some(workspace) = crate::lock(&state.workspaces).get(&query.workspace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown workspace {}", query.workspace_id)})),
        )
            .into_response();
    };
    let dir = state.claude_projects_dir.join(encode_cwd(&workspace.root));
    let exclude: Vec<PathBuf> = crate::lock(&state.agents)
        .values()
        .filter_map(|a| a.transcript_path.clone())
        .collect();
    // Transcripts can be tens of MB; scan them off the async runtime.
    let list = tokio::task::spawn_blocking(move || scan_resumables(&dir, &exclude))
        .await
        .unwrap_or_default();
    Json(serde_json::Value::Array(list)).into_response()
}

/// Resumable sessions under one project-store dir: `*.jsonl` files, newest
/// mtime first, summarized to id/title/mtime/approx_messages, capped. A
/// missing dir (workspace never used with claude) is an empty list, not an
/// error. Transcripts with nothing user-visible to title them (warmups,
/// empty boots) are skipped rather than listed as "untitled".
/// (Shared with GET /recents, which merges this history into the rail.)
pub(crate) fn scan_resumables(dir: &Path, exclude: &[PathBuf]) -> Vec<serde_json::Value> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                return None;
            }
            if exclude.contains(&path) {
                return None;
            }
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((path, meta.modified().ok()?))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.1));

    let mut out = Vec::new();
    for (path, mtime) in files {
        if out.len() >= RESUMABLE_CAP {
            break;
        }
        let summary = summarize_transcript(&path);
        let Some(title) = summary.title() else {
            continue;
        };
        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mtime = mtime
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        out.push(json!({
            "id": id,
            "title": title,
            "mtime": mtime,
            "approx_messages": summary.approx_messages,
        }));
    }
    out
}

/// What one transcript scan yields: the title-chain inputs and an
/// approximate message count.
struct TranscriptSummary {
    custom_title: Option<String>,
    ai_title: Option<String>,
    first_prompt: Option<String>,
    approx_messages: u64,
}

impl TranscriptSummary {
    /// Same precedence as live agent naming: latest customTitle > latest
    /// aiTitle > first user prompt (truncated). `None` = nothing
    /// user-visible in the transcript.
    fn title(&self) -> Option<String> {
        self.custom_title
            .clone()
            .or_else(|| self.ai_title.clone())
            .or_else(|| {
                self.first_prompt
                    .as_deref()
                    .map(crate::agents::truncate_prompt)
            })
    }
}

/// One pass over a transcript. Cheap substring pre-filters keep full JSON
/// parsing off the (potentially huge) assistant lines: only title records
/// and user records until the first prompt is found are parsed. The message
/// count is approximate by design (raw `"type":"user"/"assistant"` line
/// matches — the launcher shows it as a size hint, nothing more).
fn summarize_transcript(path: &Path) -> TranscriptSummary {
    use std::io::BufRead;

    let mut summary = TranscriptSummary {
        custom_title: None,
        ai_title: None,
        first_prompt: None,
        approx_messages: 0,
    };
    // Title records reuse the live tail-poll semantics exactly (string sets,
    // null clears, latest wins) via a scratch AgentRecord.
    let mut titles = crate::agents::AgentRecord::new(String::new(), AgentKind::Claude);

    let Ok(file) = std::fs::File::open(path) else {
        return summary;
    };
    let mut reader = std::io::BufReader::new(file);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        let is_user = line.contains(r#""type":"user""#);
        if is_user || line.contains(r#""type":"assistant""#) {
            summary.approx_messages += 1;
        }
        if summary.first_prompt.is_none() && is_user {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                summary.first_prompt = first_prompt_text(&value);
            }
        }
        if line.contains(r#""customTitle""#) || line.contains(r#""type":"ai-title""#) {
            crate::agents::apply_title_line(&line, &mut titles);
        }
    }
    summary.custom_title = titles.custom_title;
    summary.ai_title = titles.ai_title;
    summary
}

/// The human prompt text of a transcript record, if it is a real typed user
/// message: type "user", not a sidechain, not injected meta, content either
/// a plain string or text blocks. Command invocations (`<command-name>…`)
/// and Claude Code's injected "Caveat:" preamble are not prompts.
fn first_prompt_text(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(|t| t.as_str()) != Some("user") {
        return None;
    }
    if value.get("isSidechain").and_then(|s| s.as_bool()) == Some(true)
        || value.get("isMeta").and_then(|m| m.as_bool()) == Some(true)
    {
        return None;
    }
    let content = value.get("message")?.get("content")?;
    let text = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    block.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let text = text.trim();
    if text.is_empty() || text.starts_with('<') || text.starts_with("Caveat:") {
        return None;
    }
    Some(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The IDE launcher masquerading as `agy` must be refused on every
    /// platform: via the macOS bundle path AND via the launcher binary's
    /// own name (Linux hand-symlink shape). The real CLI passes.
    #[test]
    fn agy_shim_detection_spans_platforms() {
        let dir = std::env::temp_dir().join(format!("chimaera-agy-shim-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Antigravity.app/Contents/bin")).unwrap();
        std::fs::create_dir_all(dir.join("plain")).unwrap();

        // macOS: agy -> …/Antigravity.app/Contents/bin/antigravity
        let mac_target = dir.join("Antigravity.app/Contents/bin/antigravity");
        std::fs::write(&mac_target, "").unwrap();
        let mac_link = dir.join("agy-mac");
        std::os::unix::fs::symlink(&mac_target, &mac_link).unwrap();
        assert!(agy_is_ide_shim(&mac_link));

        // Linux hand-symlink: agy -> /usr/bin/antigravity (no .app anywhere)
        let linux_target = dir.join("plain/antigravity");
        std::fs::write(&linux_target, "").unwrap();
        let linux_link = dir.join("agy-linux");
        std::os::unix::fs::symlink(&linux_target, &linux_link).unwrap();
        assert!(agy_is_ide_shim(&linux_link));

        // The real CLI: a binary actually named agy.
        let real = dir.join("plain/agy");
        std::fs::write(&real, "").unwrap();
        assert!(!agy_is_ide_shim(&real));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn encode_cwd_matches_the_real_store() {
        // Verified against ~/.claude/projects on this machine.
        assert_eq!(
            encode_cwd(Path::new("/Users/martinkjellberg/dev/chimaera")),
            "-Users-martinkjellberg-dev-chimaera"
        );
        // Dots and underscores are non-alphanumeric too: a hidden
        // `.claude-worktrees` component doubles the dash (observed).
        assert_eq!(
            encode_cwd(Path::new(
                "/Users/x/dev/chimaera/.claude-worktrees/elastic-margulis-62efae"
            )),
            "-Users-x-dev-chimaera--claude-worktrees-elastic-margulis-62efae"
        );
        assert_eq!(encode_cwd(Path::new("/tmp/my_proj.v2")), "-tmp-my-proj-v2");
    }

    #[test]
    fn safe_arg_rejects_flags_and_junk() {
        assert!(safe_arg("opus"));
        assert!(safe_arg("gemini-2.5-pro"));
        assert!(safe_arg("5e0d64b2-abcd-abcd-abcd-000000000000"));
        assert!(!safe_arg(""));
        assert!(!safe_arg("--dangerously-skip-permissions"));
        assert!(!safe_arg("a b"));
        assert!(!safe_arg("a;b"));
        assert!(!safe_arg("a/b"));
    }

    #[test]
    fn build_agent_command_claude_full() {
        let cmd = build_agent_command(
            AgentKind::Claude,
            Path::new("/usr/local/bin/claude"),
            Some(Path::new("/run/agents/s-1-settings.json")),
            Some("opus"),
            Some("5e0d64b2-abcd-abcd-abcd-000000000000"),
            None,
        );
        assert_eq!(
            cmd,
            [
                "/usr/local/bin/claude",
                "--settings",
                "/run/agents/s-1-settings.json",
                "--model",
                "opus",
                "--resume",
                "5e0d64b2-abcd-abcd-abcd-000000000000",
            ]
        );
    }

    #[test]
    fn build_agent_command_claude_bare_keeps_hooks_only() {
        let cmd = build_agent_command(
            AgentKind::Claude,
            Path::new("/usr/local/bin/claude"),
            Some(Path::new("/run/s.json")),
            None,
            None,
            None,
        );
        assert_eq!(cmd, ["/usr/local/bin/claude", "--settings", "/run/s.json"]);
    }

    #[test]
    fn build_agent_command_codex_and_gemini_are_plain_tuis() {
        // No settings injection for non-claude agents; --model passes through.
        let cmd = build_agent_command(
            AgentKind::Codex,
            Path::new("/opt/bin/codex"),
            None,
            Some("o4-mini"),
            None,
            None,
        );
        assert_eq!(cmd, ["/opt/bin/codex", "--model", "o4-mini"]);
        let cmd = build_agent_command(
            AgentKind::Gemini,
            Path::new("/opt/bin/gemini"),
            None,
            None,
            None,
            None,
        );
        assert_eq!(cmd, ["/opt/bin/gemini"]);
    }

    /// Codex themes through `-c tui.theme=<name>` — the config override
    /// documented by codex 0.142.5's own config schema; the names are the
    /// per-scheme defaults codex itself would pick.
    #[test]
    fn build_agent_command_codex_injects_scheme_theme() {
        let cmd = build_agent_command(
            AgentKind::Codex,
            Path::new("/opt/bin/codex"),
            None,
            Some("o4-mini"),
            None,
            Some("catppuccin-mocha"),
        );
        assert_eq!(
            cmd,
            [
                "/opt/bin/codex",
                "-c",
                "tui.theme=catppuccin-mocha",
                "--model",
                "o4-mini",
            ]
        );
    }

    #[test]
    fn wrap_login_shell_passes_argv_untouched() {
        // sh-family: `$0`/`$@` carry the argv — nothing is interpolated
        // into the script, so paths with spaces or quotes survive as-is.
        // The script also reclaims the shim dir's front PATH slot after
        // profile init (macOS path_helper demotes the injected order —
        // measured on this machine).
        let cmd = wrap_login_shell(
            "/bin/zsh",
            vec![
                "/Users/x/My Tools/codex".to_string(),
                "--model".to_string(),
                "o4-mini".to_string(),
            ],
        );
        assert_eq!(cmd[..2], ["/bin/zsh", "-lc"]);
        let script = &cmd[2];
        assert!(
            script.contains(r#"PATH="$CHIMAERA_SHIMS:$PATH""#),
            "{script}"
        );
        assert!(script.ends_with(r#"exec "$0" "$@""#), "{script}");
        assert_eq!(cmd[3..], ["/Users/x/My Tools/codex", "--model", "o4-mini"]);

        // fish has no `$0`/`$@`; trailing -c args land in $argv instead.
        let cmd = wrap_login_shell(
            "/opt/homebrew/bin/fish",
            vec!["/usr/bin/claude".to_string()],
        );
        assert_eq!(cmd[..2], ["/opt/homebrew/bin/fish", "-lc"]);
        let script = &cmd[2];
        assert!(
            script.contains("set -gx PATH $CHIMAERA_SHIMS $PATH"),
            "{script}"
        );
        assert!(script.ends_with("exec $argv"), "{script}");
        assert_eq!(cmd[3..], ["/usr/bin/claude"]);
    }

    #[test]
    fn first_prompt_text_takes_typed_prompts_only() {
        // Shaped like real transcript records (claude 2.1.196).
        let real = json!({
            "parentUuid": null, "isSidechain": false, "type": "user",
            "message": {"role": "user", "content": "fix the qc pipeline"},
            "cwd": "/w", "sessionId": "x",
        });
        assert_eq!(
            first_prompt_text(&real).as_deref(),
            Some("fix the qc pipeline")
        );

        // Text blocks are joined.
        let blocks = json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "text", "text": "part one"},
                {"type": "text", "text": "part two"},
            ]},
        });
        assert_eq!(
            first_prompt_text(&blocks).as_deref(),
            Some("part one part two")
        );

        // Sidechains, meta, tool results, command XML, caveats, non-user
        // records: none of these are prompts.
        let sidechain = json!({
            "type": "user", "isSidechain": true,
            "message": {"role": "user", "content": "subagent task"},
        });
        assert_eq!(first_prompt_text(&sidechain), None);
        let meta = json!({
            "type": "user", "isMeta": true,
            "message": {"role": "user", "content": "injected context"},
        });
        assert_eq!(first_prompt_text(&meta), None);
        let tool_result = json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "ok"},
            ]},
        });
        assert_eq!(first_prompt_text(&tool_result), None);
        let command = json!({
            "type": "user",
            "message": {"role": "user", "content": "<command-name>/clear</command-name>"},
        });
        assert_eq!(first_prompt_text(&command), None);
        let caveat = json!({
            "type": "user",
            "message": {"role": "user", "content": "Caveat: the messages below were generated"},
        });
        assert_eq!(first_prompt_text(&caveat), None);
        assert_eq!(first_prompt_text(&json!({"type": "assistant"})), None);
    }

    #[test]
    fn summarize_transcript_resolves_title_chain_and_counts() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-launcher-test-{}-{}",
            std::process::id(),
            &chimaera_core::generate_token()[..8]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");

        // Meta lines don't count; the first *typed* prompt wins; a later
        // ai-title outranks it; the latest custom-title outranks everything.
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"file-history-snapshot","messageId":"m"}"#,
                "\n",
                r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"sidechain"}}"#,
                "\n",
                r#"{"type":"user","message":{"role":"user","content":"the real prompt"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"AI name","sessionId":"x"}"#,
                "\n",
                r#"{"type":"custom-title","customTitle":"Pinned name","sessionId":"x"}"#,
                "\n",
            ),
        )
        .unwrap();

        let summary = summarize_transcript(&path);
        assert_eq!(summary.first_prompt.as_deref(), Some("the real prompt"));
        assert_eq!(summary.ai_title.as_deref(), Some("AI name"));
        assert_eq!(summary.custom_title.as_deref(), Some("Pinned name"));
        assert_eq!(summary.title().as_deref(), Some("Pinned name"));
        // 2 user lines (the sidechain counts, approximately) + 1 assistant.
        assert_eq!(summary.approx_messages, 3);

        // Clearing the custom title falls back to the ai title...
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, b"{\"customTitle\":null}\n").unwrap();
        assert_eq!(
            summarize_transcript(&path).title().as_deref(),
            Some("AI name")
        );

        // ...and a transcript with nothing user-visible has no title.
        let empty = dir.join("empty.jsonl");
        std::fs::write(&empty, "{\"type\":\"mode\",\"mode\":\"normal\"}\n").unwrap();
        assert_eq!(summarize_transcript(&empty).title(), None);
        assert_eq!(summarize_transcript(&empty).approx_messages, 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn curated_lists_cover_every_agent() {
        for kind in AgentKind::ALL {
            // agy has no curated model list (empty is fine); install and
            // docs must exist for every catalog agent.
            assert!(!install_command(kind).is_empty());
            assert!(docs_url(kind).starts_with("https://"));
        }
        // Outdated detection: npm-era codex (0.1.x, pre-`codex login`)
        // gets the update affordance; modern builds and other agents don't.
        assert!(is_outdated(AgentKind::Codex, Some("0.1.2504161551")));
        assert!(!is_outdated(AgentKind::Codex, Some("0.52.0")));
        assert!(!is_outdated(AgentKind::Codex, None));
        assert!(!is_outdated(AgentKind::Claude, Some("0.1.99")));
        // The spec'd claude aliases and labels, exactly.
        assert_eq!(
            models(AgentKind::Claude),
            [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")]
        );
        assert!(install_command(AgentKind::Claude).starts_with("curl "));
        assert!(install_command(AgentKind::Codex).contains("npm install -g @openai/codex"));
        assert!(install_command(AgentKind::Gemini).contains("npm install -g @google/gemini-cli"));
    }
}
