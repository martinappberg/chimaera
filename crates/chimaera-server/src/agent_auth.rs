//! Sign in to an agent CLI from the app. Two halves, one flow for every
//! runtime: a bounded **status probe** (each CLI's own status command, cached
//! on the agent catalog), and a bounded, cancellable **sign-in operation**
//! that runs the CLI's own headless login under the daemon (the URL and
//! device code it prints are captured for the UI, a pasted code is fed back
//! on stdin; a CLI that would open a browser itself is told not to, or gets a
//! no-op `open` on its PATH). Chimaera never touches credentials: the CLI owns the login and its
//! store, and success is only ever what the CLI's status command reports.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{Path as AxPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, watch},
};

use crate::{agents::AgentKind, lock, AppState};

/// A login's whole output stays private to the daemon; this bounds it.
const OUTPUT_LIMIT: usize = 64 * 1024;
/// A browser login that nobody finishes is abandoned, like the connector
/// sign-ins.
const DEADLINE: Duration = Duration::from_secs(10 * 60);
/// A status command that takes longer than this is unknown, not a verdict
/// (node-backed CLIs boot in ~1 s; `agy models` asks the network).
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a probed verdict is served from the cache. A login can expire or
/// be revoked outside chimaera, so the launcher's next open re-asks after
/// this; a sign-in, a chat startup failure or `?check=true` re-ask at once.
const STATUS_TTL: Duration = Duration::from_secs(10 * 60);
const URL_LIMIT: usize = 8192;

/// How this agent signs in.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum How {
    /// The CLI's headless login under the daemon: it prints a URL (and for
    /// device-code flows a code) and waits; the UI opens the URL.
    Browser,
}

/// The per-runtime truth table (docs/features/agents.md, "Sign in").
pub(crate) struct Recipe {
    pub(crate) how: How,
    /// What the login runs, in the user's words (shown in Settings).
    pub(crate) command: &'static str,
    /// Claude's browser page shows a code the user pastes back.
    pub(crate) accepts_code: bool,
    /// Signing in while signed in means something (a switch of account):
    /// true for a CLI whose login replaces the stored one; Antigravity's
    /// print mode just finds the existing login and reports it.
    pub(crate) relogin: bool,
}

pub(crate) fn recipe(kind: AgentKind) -> Option<Recipe> {
    Some(match kind {
        AgentKind::Claude => Recipe {
            how: How::Browser,
            command: "claude auth login",
            accepts_code: true,
            relogin: true,
        },
        AgentKind::Codex => Recipe {
            how: How::Browser,
            command: "codex login --device-auth",
            accepts_code: false,
            relogin: true,
        },
        AgentKind::Grok => Recipe {
            how: How::Browser,
            command: "grok login --device-auth",
            accepts_code: false,
            relogin: true,
        },
        AgentKind::Cursor => Recipe {
            how: How::Browser,
            command: "cursor-agent login",
            accepts_code: false,
            relogin: true,
        },
        // No login command, but print mode signs in headlessly: a native
        // zero-turn command (`/skills`) triggers the OAuth URL and a 60 s wait
        // for Google's hosted callback (probed 1.3.5). No pasted code: print
        // mode reads a piped stdin as its prompt and abandons the login at
        // once, so the login gets no stdin and only the callback completes it.
        AgentKind::Antigravity => Recipe {
            how: How::Browser,
            command: "agy -p /skills",
            accepts_code: false,
            relogin: false,
        },
        AgentKind::Gemini => return None,
    })
}

/// The headless login's argv tail and environment. Browser opening is the
/// app's job (its Open button works on a remote host too), so every CLI that
/// would open one itself is told not to: claude honors `BROWSER`, Cursor
/// `NO_OPEN_BROWSER`, and the device-code flows never open one.
struct Headless {
    args: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
    /// The CLI opens a browser itself and has no switch not to (Antigravity
    /// ignores `BROWSER` and `NO_BROWSER`): a no-op `open` / `xdg-open` goes
    /// first on its PATH, so only the app opens the page.
    no_browser: bool,
}

fn login_launch(kind: AgentKind) -> Option<Headless> {
    Some(match kind {
        AgentKind::Claude => Headless {
            args: &["auth", "login"],
            env: &[("BROWSER", "true")],
            no_browser: false,
        },
        AgentKind::Codex | AgentKind::Grok => Headless {
            args: &["login", "--device-auth"],
            env: &[],
            no_browser: false,
        },
        AgentKind::Cursor => Headless {
            args: &["login"],
            env: &[("NO_OPEN_BROWSER", "1")],
            no_browser: false,
        },
        AgentKind::Antigravity => Headless {
            args: &[
                "-p",
                "/skills",
                "--output-format",
                "json",
                "--print-timeout",
                "15s",
            ],
            env: &[],
            no_browser: true,
        },
        AgentKind::Gemini => return None,
    })
}

// ---------------------------------------------------------------- status --

#[derive(Clone)]
struct Status {
    signed_in: Option<bool>,
    checked_at: Instant,
    /// The executable the verdict is about: a changed install re-probes.
    bin: PathBuf,
}

#[derive(Default)]
pub(crate) struct AuthState {
    status: Mutex<HashMap<AgentKind, Status>>,
    /// One probe per agent at a time; later callers wait and read the cache.
    probing: Mutex<HashMap<AgentKind, Arc<tokio::sync::Mutex<()>>>>,
    operations: Mutex<HashMap<AgentKind, Arc<Operation>>>,
}

fn probe_lock(state: &AppState, kind: AgentKind) -> Arc<tokio::sync::Mutex<()>> {
    lock(&state.agent_auth.probing)
        .entry(kind)
        .or_default()
        .clone()
}

/// The status command's verdict from its exit and output, per CLI.
fn parse_probe(kind: AgentKind, success: bool, stdout: &str, stderr: &str) -> Option<bool> {
    let text = strip_controls(&format!("{stdout}\n{stderr}"));
    let lower = text.to_ascii_lowercase();
    match kind {
        // `claude auth status --json` → {"loggedIn": bool, …}
        AgentKind::Claude => serde_json::from_str::<serde_json::Value>(stdout.trim())
            .ok()
            .and_then(|v| v.get("loggedIn")?.as_bool()),
        // `codex login status` → "Logged in using ChatGPT" (0) / "Not logged in" (1)
        AgentKind::Codex => {
            if lower.contains("not logged in") {
                Some(false)
            } else if success && lower.contains("logged in") {
                Some(true)
            } else {
                None
            }
        }
        // `cursor-agent status --format json` → {"isAuthenticated": bool, …}
        AgentKind::Cursor => serde_json::from_str::<serde_json::Value>(stdout.trim())
            .ok()
            .and_then(|v| v.get("isAuthenticated")?.as_bool()),
        // `grok models` → "You are logged in with grok.com." / "You are not authenticated."
        AgentKind::Grok => {
            if lower.contains("not authenticated") {
                Some(false)
            } else if lower.contains("logged in") {
                Some(true)
            } else {
                None
            }
        }
        // `agy models` lists the account's models; without a login it can't.
        // A failure that says nothing about signing in (offline) is unknown,
        // never "signed out".
        AgentKind::Antigravity => {
            if success && text.lines().any(|l| l.contains('\t')) {
                Some(true)
            } else if !success
                && (lower.contains("please sign in") || lower.contains("not logged in"))
            {
                // Its own signed-out words only: an offline or quota failure
                // is unknown, never "signed out".
                Some(false)
            } else {
                None
            }
        }
        AgentKind::Gemini => None,
    }
}

fn probe_argv(kind: AgentKind) -> Option<&'static [&'static str]> {
    Some(match kind {
        AgentKind::Claude => &["auth", "status", "--json"],
        AgentKind::Codex => &["login", "status"],
        AgentKind::Cursor => &["status", "--format", "json"],
        AgentKind::Grok | AgentKind::Antigravity => &["models"],
        AgentKind::Gemini => return None,
    })
}

/// Ask the CLI itself whether it is signed in, the way a chat would run
/// it: through the user's login shell with the host prelude (`CODEX_HOME`,
/// `CLAUDE_CONFIG_DIR`, a proxy, an nvm node or a module load all come from
/// there), in its own process group, output capped. `None` = unknown (no
/// status command, no answer in time, or an unreadable one).
async fn run_probe(state: &Arc<AppState>, kind: AgentKind, bin: &Path) -> Option<bool> {
    let argv = probe_argv(kind)?;
    let prelude = crate::agent_probe::ProbePrelude::write(state, None).await;
    let mut cmd = crate::agent_probe::base_command(
        &crate::agent_probe::wrapped(bin, argv),
        None,
        prelude.path(),
    );
    cmd.process_group(0);
    let mut child = cmd.spawn().ok()?;
    let pid = nix::unistd::Pid::from_raw(child.id()? as i32);
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let work = async {
        let (out, err) = tokio::join!(read_capped(stdout), read_capped(stderr));
        let status = child.wait().await.ok()?;
        Some((status.success(), out, err))
    };
    match tokio::time::timeout(PROBE_TIMEOUT, work).await {
        Ok(Some((success, out, err))) => parse_probe(kind, success, &out, &err),
        Ok(None) => None,
        Err(_) => {
            // The leader is unreaped: the group id is still its own.
            let _ = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL);
            let _ = child.wait().await;
            None
        }
    }
}

/// Read a pipe to its end, keeping the first `OUTPUT_LIMIT` bytes and
/// draining the rest so the child never blocks on a full pipe.
async fn read_capped(mut pipe: impl AsyncRead + Unpin) -> String {
    let mut kept = Vec::new();
    let mut buf = [0; 4096];
    while let Ok(n) = pipe.read(&mut buf).await {
        if n == 0 {
            break;
        }
        if kept.len() < OUTPUT_LIMIT {
            let room = OUTPUT_LIMIT - kept.len();
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
    }
    String::from_utf8_lossy(&kept).into_owned()
}

fn cached(state: &AppState, kind: AgentKind, bin: &Path) -> Option<Option<bool>> {
    lock(&state.agent_auth.status)
        .get(&kind)
        .filter(|s| s.bin == bin && s.checked_at.elapsed() < STATUS_TTL)
        .map(|s| s.signed_in)
}

/// The verdict for `bin`, answered at once: the cached one while it lives;
/// without one, `None` now and a probe in the background (the catalog
/// refetches on the change bus), so a launcher paint never waits on five
/// CLIs. Only an explicit ask (`fresh`: Settings' Check, `?check=true`, a
/// sign-in's end, a chat's startup failure) probes inline. One probe per
/// agent at a time either way.
pub(crate) async fn status(
    state: &Arc<AppState>,
    kind: AgentKind,
    bin: &Path,
    fresh: bool,
) -> Option<bool> {
    recipe(kind)?;
    if !fresh {
        if let Some(verdict) = cached(state, kind, bin) {
            return verdict;
        }
        let probing = probe_lock(state, kind);
        if let Ok(guard) = probing.clone().try_lock_owned() {
            let state = state.clone();
            let bin = bin.to_path_buf();
            tokio::spawn(async move {
                let _guard = guard;
                probe_and_store(&state, kind, &bin).await;
                state.changes.notify_waiters();
            });
        }
        return None;
    }
    let probing = probe_lock(state, kind);
    let _guard = probing.lock().await;
    // A waiter behind a fresh probe reads what it found.
    if lock(&state.agent_auth.status)
        .get(&kind)
        .is_some_and(|s| s.bin == bin && s.checked_at.elapsed() < Duration::from_secs(2))
    {
        if let Some(verdict) = cached(state, kind, bin) {
            return verdict;
        }
    }
    probe_and_store(state, kind, bin).await
}

async fn probe_and_store(state: &Arc<AppState>, kind: AgentKind, bin: &Path) -> Option<bool> {
    let signed_in = run_probe(state, kind, bin).await;
    lock(&state.agent_auth.status).insert(
        kind,
        Status {
            signed_in,
            checked_at: Instant::now(),
            bin: bin.to_path_buf(),
        },
    );
    signed_in
}

/// A verdict as if just probed — tests plant what a CLI would have said.
#[cfg(test)]
pub(crate) fn preset_verdict(
    state: &AppState,
    kind: AgentKind,
    bin: &Path,
    signed_in: Option<bool>,
) {
    lock(&state.agent_auth.status).insert(
        kind,
        Status {
            signed_in,
            checked_at: Instant::now(),
            bin: bin.to_path_buf(),
        },
    );
}

/// The cached verdict, never a probe (a spawn must not wait on one).
pub(crate) fn cached_verdict(state: &AppState, kind: AgentKind) -> Option<bool> {
    lock(&state.agent_auth.status)
        .get(&kind)
        .filter(|s| s.checked_at.elapsed() < STATUS_TTL)
        .and_then(|s| s.signed_in)
}

/// Whether a sign-in for `kind` is running (an install must not run beside it).
pub(crate) fn running(state: &AppState, kind: AgentKind) -> bool {
    snapshot(state, kind).is_some_and(|p| p.running())
}

/// A chat failed to start: re-ask this agent's status so the launcher row
/// and Settings card say "not signed in" when that is why. Bounded by the
/// probe timeout; the catalog refreshes on the change bus.
pub(crate) fn recheck(state: Arc<AppState>, kind: AgentKind) {
    tokio::spawn(async move {
        let detection = crate::launcher::detect(&state, kind, false).await;
        if let Ok(bin) = detection.path {
            status(&state, kind, &bin, true).await;
            state.changes.notify_waiters();
        }
    });
}

/// The catalog row's additive `sign_in` descriptor.
pub(crate) fn descriptor(state: &AppState, kind: AgentKind) -> Option<serde_json::Value> {
    let recipe = recipe(kind)?;
    Some(json!({
        "how": recipe.how,
        "command": recipe.command,
        "accepts_code": recipe.accepts_code,
        "relogin": recipe.relogin,
        "operation": summary(state, kind),
    }))
}

// ------------------------------------------------------------- operation --

#[derive(Clone, Copy, PartialEq, Serialize, Debug)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Serialize)]
pub(crate) struct Progress {
    id: String,
    how: How,
    phase: Phase,
    workspace_id: String,
    started_at: u64,
    /// The page to sign in on, once the CLI printed it.
    url: Option<String>,
    /// A device code to confirm on that page (Codex, Grok).
    code: Option<String>,
    /// The page will show a code to paste back (Claude).
    accepts_code: bool,
    message: String,
}
impl Progress {
    fn running(&self) -> bool {
        matches!(self.phase, Phase::Running | Phase::Cancelling)
    }
}

pub(crate) struct Operation {
    progress: Mutex<Progress>,
    cancel: watch::Sender<bool>,
    /// A pasted code, for a login that reads one on stdin.
    input: mpsc::Sender<String>,
    /// Private: the login's own output, for a failure hint. Never on the wire.
    output: Mutex<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    workspace_id: String,
    request_id: String,
    /// The chat this sign-in was asked from (`/login`): relaunched in place
    /// when the login lands, like a chat that waited on it.
    #[serde(default)]
    session_id: Option<String>,
    /// Accepted for compatibility; the headless flows have no theme.
    #[serde(default)]
    #[allow(dead_code)]
    theme: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct StatusQuery {
    #[serde(default)]
    check: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Code {
    code: String,
}

fn kind(id: &str) -> Option<AgentKind> {
    AgentKind::parse(id).filter(|k| AgentKind::ALL.contains(k))
}
fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}
fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
fn snapshot(state: &AppState, kind: AgentKind) -> Option<Progress> {
    lock(&state.agent_auth.operations)
        .get(&kind)
        .map(|op| lock(&op.progress).clone())
}
pub(crate) fn summary(state: &AppState, kind: AgentKind) -> Option<serde_json::Value> {
    snapshot(state, kind).map(|p| json!({"id": p.id, "running": p.running(), "phase": p.phase}))
}

/// GET /api/v1/agents/{id}/sign-in[?check=true] — the status and the
/// current operation. `check` asks the CLI again.
pub(crate) async fn get(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
    Query(query): Query<StatusQuery>,
) -> Response {
    let Some(kind) = kind(&id) else {
        return error(StatusCode::NOT_FOUND, "Unknown agent.");
    };
    let Some(recipe) = recipe(kind) else {
        return error(StatusCode::NOT_FOUND, "This agent has no sign-in.");
    };
    let detection = crate::launcher::detect(&state, kind, false).await;
    let signed_in = match &detection.path {
        Ok(bin) => status(&state, kind, bin, query.check).await,
        Err(_) => None,
    };
    no_store(
        Json(json!({
            "installed": detection.path.is_ok(),
            "signed_in": signed_in,
            "how": recipe.how,
            "command": recipe.command,
            "accepts_code": recipe.accepts_code,
            "relogin": recipe.relogin,
            "operation": snapshot(&state, kind),
        }))
        .into_response(),
    )
}

/// POST /api/v1/agents/{id}/sign-in — start (or join) this agent's sign-in.
pub(crate) async fn start(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
    Json(body): Json<Request>,
) -> Response {
    let Some(kind) = kind(&id) else {
        return error(StatusCode::NOT_FOUND, "Unknown agent.");
    };
    let Some(recipe) = recipe(kind) else {
        return error(StatusCode::NOT_FOUND, "This agent has no sign-in.");
    };
    if body.request_id.is_empty()
        || body.request_id.len() > 128
        || !body
            .request_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return error(StatusCode::BAD_REQUEST, "Invalid sign-in request ID.");
    }
    // A repeated POST after a lost response observes the same operation; a
    // second window joins the running one instead of starting a competitor.
    if let Some(p) = snapshot(&state, kind).filter(|p| p.running() || p.id == body.request_id) {
        return no_store(Json(p).into_response());
    }
    let Some(workspace) = lock(&state.workspaces).get(&body.workspace_id) else {
        return error(
            StatusCode::NOT_FOUND,
            "Open a workspace on this host first.",
        );
    };
    // Never beside an install or update of the same agent: the executable
    // may be swapped under the login.
    if crate::agent_setup::summary(&state, kind).is_some_and(|s| s["running"] == true) {
        return error(
            StatusCode::CONFLICT,
            "Finish installing or updating this agent first.",
        );
    }
    if let Some(session) = body.session_id.as_deref() {
        if let Some(record) = lock(&state.agents).get_mut(session) {
            if record.kind == kind {
                record.signed_out_at_start = true;
            }
        }
    }
    let detection = crate::launcher::detect(&state, kind, false).await;
    let bin = match detection.path {
        Ok(bin) => bin,
        Err(_) => {
            return error(
                StatusCode::CONFLICT,
                &format!("{} is not installed on this host.", kind.product_name()),
            )
        }
    };
    let launch = {
        let headless = login_launch(kind).expect("a browser recipe has a launch");
        let mut argv = vec![bin.to_string_lossy().into_owned()];
        argv.extend(headless.args.iter().map(|a| a.to_string()));
        Launch::Browser {
            argv,
            env: headless
                .env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            stdin: recipe.accepts_code,
            cwd: workspace.root.clone(),
            no_browser: headless.no_browser,
        }
    };
    no_store(start_operation(state, kind, body, launch).await)
}

/// What a sign-in runs (one shape today; tests script it).
pub(crate) enum Launch {
    Browser {
        argv: Vec<String>,
        env: Vec<(String, String)>,
        /// Keep stdin open for a pasted code.
        stdin: bool,
        cwd: PathBuf,
        /// Put a no-op `open` / `xdg-open` first on the login's PATH.
        no_browser: bool,
    },
}

async fn start_operation(
    state: Arc<AppState>,
    kind: AgentKind,
    body: Request,
    launch: Launch,
) -> Response {
    let recipe = recipe(kind).expect("checked by the caller");
    let (cancel, cancelled) = watch::channel(false);
    let (input, codes) = mpsc::channel(1);
    let progress = Progress {
        id: body.request_id,
        how: recipe.how,
        phase: Phase::Running,
        workspace_id: body.workspace_id,
        started_at: crate::timeline::now_ms(),
        url: None,
        code: None,
        accepts_code: recipe.accepts_code,
        message: "Starting the sign-in…".into(),
    };
    let operation = Arc::new(Operation {
        progress: Mutex::new(progress.clone()),
        cancel,
        input,
        output: Mutex::new(String::new()),
    });
    {
        let mut operations = lock(&state.agent_auth.operations);
        // Re-check under the lock: two first clicks must not both start.
        if let Some(existing) = operations.get(&kind) {
            let p = lock(&existing.progress).clone();
            if p.running() || p.id == progress.id {
                return Json(p).into_response();
            }
        }
        operations.insert(kind, operation.clone());
    }
    tokio::spawn(async move {
        let (phase, message) = match launch {
            Launch::Browser {
                argv,
                env,
                stdin,
                cwd,
                no_browser,
            } => {
                run_browser(
                    &state,
                    operation.clone(),
                    kind,
                    Launch::Browser {
                        argv,
                        env,
                        stdin,
                        cwd,
                        no_browser,
                    },
                    cancelled,
                    codes,
                )
                .await
            }
        };
        finish(&operation, phase, &message);
        // The verdict the flow ended on is the one the catalog serves.
        state.changes.notify_waiters();
        // The chats that waited on this login start again, in place.
        if phase == Phase::Succeeded {
            crate::chat::relaunch_after_sign_in(state.clone(), kind).await;
        }
    });
    Json(progress).into_response()
}

/// DELETE /api/v1/agents/{id}/sign-in/{operation} — stop this sign-in.
pub(crate) async fn cancel(
    State(state): State<Arc<AppState>>,
    AxPath((id, operation)): AxPath<(String, String)>,
) -> Response {
    let Some(kind) = kind(&id) else {
        return error(StatusCode::NOT_FOUND, "Unknown agent.");
    };
    let Some(op) = lock(&state.agent_auth.operations).get(&kind).cloned() else {
        return error(
            StatusCode::NOT_FOUND,
            "This sign-in is no longer available.",
        );
    };
    let mut p = lock(&op.progress);
    // An old card must never cancel a newer attempt.
    if p.id != operation {
        return error(
            StatusCode::CONFLICT,
            "A newer sign-in has started. Refresh its status.",
        );
    }
    if p.running() {
        p.phase = Phase::Cancelling;
        p.message = "Stopping the sign-in…".into();
        op.cancel.send_replace(true);
    }
    no_store(Json(p.clone()).into_response())
}

/// POST /api/v1/agents/{id}/sign-in/{operation}/code — the code the sign-in
/// page showed, for a login that reads one (Claude).
pub(crate) async fn code(
    State(state): State<Arc<AppState>>,
    AxPath((id, operation)): AxPath<(String, String)>,
    Json(body): Json<Code>,
) -> Response {
    let Some(kind) = kind(&id) else {
        return error(StatusCode::NOT_FOUND, "Unknown agent.");
    };
    let Some(op) = lock(&state.agent_auth.operations).get(&kind).cloned() else {
        return error(
            StatusCode::NOT_FOUND,
            "This sign-in is no longer available.",
        );
    };
    let code = body.code.trim();
    if code.is_empty()
        || code.len() > 512
        || code.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return error(
            StatusCode::BAD_REQUEST,
            "Paste the code exactly as the page shows it.",
        );
    }
    {
        let p = lock(&op.progress);
        if p.id != operation {
            return error(
                StatusCode::CONFLICT,
                "A newer sign-in has started. Refresh its status.",
            );
        }
        if !p.accepts_code || !p.running() {
            return error(
                StatusCode::CONFLICT,
                "This sign-in isn't waiting for a code.",
            );
        }
    }
    if op.input.try_send(code.to_string()).is_err() {
        return error(StatusCode::CONFLICT, "A code is already being checked.");
    }
    let mut p = lock(&op.progress);
    p.message = "Checking the code…".into();
    no_store(Json(p.clone()).into_response())
}

// --------------------------------------------------------------- running --

struct Group(nix::unistd::Pid);
impl Drop for Group {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(self.0, nix::sys::signal::Signal::SIGKILL);
    }
}

fn append(op: &Operation, bytes: &[u8]) {
    let mut out = lock(&op.output);
    out.push_str(&String::from_utf8_lossy(bytes));
    if out.len() > OUTPUT_LIMIT {
        let mut cut = out.len() - OUTPUT_LIMIT;
        while !out.is_char_boundary(cut) {
            cut += 1;
        }
        out.drain(..cut);
    }
}

/// Read the login's URL and code out of what it has printed so far. Only
/// text up to the last whitespace counts: a URL split across pipe reads must
/// never reach the UI incomplete.
fn harvest(op: &Operation, kind: AgentKind) {
    let text = strip_controls(&lock(&op.output));
    let complete = text
        .rfind(char::is_whitespace)
        .map_or("", |end| &text[..end]);
    let mut p = lock(&op.progress);
    if p.url.is_none() {
        if let Some(url) = login_url(complete, kind) {
            p.url = Some(url);
            p.message = "Finish signing in in your browser.".into();
        }
    }
    if p.code.is_none() {
        p.code = device_code(complete);
    }
}

async fn drain(mut pipe: impl AsyncRead + Unpin, op: Arc<Operation>, kind: AgentKind) {
    let mut buf = [0; 4096];
    while let Ok(n) = pipe.read(&mut buf).await {
        if n == 0 {
            break;
        }
        append(&op, &buf[..n]);
        harvest(&op, kind);
    }
}

async fn run_browser(
    state: &Arc<AppState>,
    op: Arc<Operation>,
    kind: AgentKind,
    launch: Launch,
    cancel: watch::Receiver<bool>,
    mut codes: mpsc::Receiver<String>,
) -> (Phase, String) {
    let Launch::Browser {
        argv,
        env: extra_env,
        stdin,
        cwd,
        no_browser,
    } = launch;
    if *cancel.borrow() {
        return (Phase::Cancelled, "Sign-in cancelled.".into());
    }
    // The way a chat runs the CLI: the user's login shell, the host and
    // workspace prelude, the session env — so the login writes to the store
    // the chats read.
    let (id, workspace) = {
        let p = lock(&op.progress);
        (p.id.clone(), p.workspace_id.clone())
    };
    let prelude = crate::agent_probe::ProbePrelude::write(state, Some(&workspace)).await;
    let mut env = crate::api::session_env(state, &id, "dark", prelude.path());
    if no_browser {
        match no_browser_dir().await {
            Ok(dir) => env = with_path_prefix(env, &dir),
            Err(err) => {
                return (
                    Phase::Failed,
                    format!("Couldn't prepare the sign-in: {err}"),
                )
            }
        }
    }
    let remove = crate::api::spawn_env_remove(&env);
    let argv = crate::launcher::wrap_login_shell(&crate::launcher::login_shell(), argv);
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .envs(env)
        .envs(extra_env)
        .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    for name in remove {
        cmd.env_remove(name);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => return (Phase::Failed, format!("Couldn't start the sign-in: {err}")),
    };
    let Some(pid) = child.id() else {
        return (Phase::Failed, "The sign-in ended before it started.".into());
    };
    let group = Group(nix::unistd::Pid::from_raw(pid as i32));
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let mut stdin = child.stdin.take();
    lock(&op.progress).message = "Waiting for the sign-in page…".into();
    // The pipes are read on their own: the verdict is the exit status plus
    // the status command, never pipe EOF — a helper the login left behind
    // holding stdout must not turn a success into a ten-minute timeout.
    let reads = tokio::spawn({
        let op = op.clone();
        async move {
            tokio::join!(drain(stdout, op.clone(), kind), drain(stderr, op, kind));
        }
    });
    let mut cancel_wait = cancel.clone();
    let stopped = async move {
        if !*cancel_wait.borrow() {
            let _ = cancel_wait.changed().await;
        }
    };
    tokio::pin!(stopped);
    let deadline = tokio::time::sleep(DEADLINE);
    tokio::pin!(deadline);
    let exit = loop {
        tokio::select! {
            result = child.wait() => break Some(result),
            _ = &mut stopped => break None,
            _ = &mut deadline => break None,
            Some(code) = codes.recv(), if stdin.is_some() => {
                // Claude's prompt reads a line.
                let Some(pipe) = stdin.as_mut() else { continue };
                let write = async {
                    pipe.write_all(code.as_bytes()).await?;
                    pipe.write_all(b"\n").await?;
                    pipe.flush().await
                };
                if write.await.is_err() {
                    lock(&op.progress).message = "The sign-in stopped accepting input.".into();
                }
            }
        }
    };
    match exit {
        Some(result) => {
            // The leader is reaped: never signal a possibly recycled group id.
            std::mem::forget(group);
            // Whatever the pipes still hold lands in the output for the
            // failure tail; a holder that stays open is not waited for.
            let _ = tokio::time::timeout(Duration::from_millis(300), reads).await;
            let code = result.ok().and_then(|s| s.code());
            if code == Some(0) {
                verify(state, &op, kind).await
            } else {
                let tail = failure_tail(&lock(&op.output));
                (
                    Phase::Failed,
                    if tail.is_empty() {
                        "The sign-in didn't complete. Try again.".to_string()
                    } else {
                        format!("The sign-in didn't complete. {tail}")
                    },
                )
            }
        }
        None => {
            let cancelled = *cancel.borrow();
            let _ = nix::sys::signal::killpg(group.0, nix::sys::signal::Signal::SIGTERM);
            let _ = tokio::time::timeout(Duration::from_secs(2), reads).await;
            drop(group);
            let _ = child.wait().await;
            if cancelled {
                (Phase::Cancelled, "Sign-in cancelled.".into())
            } else {
                (
                    Phase::Failed,
                    "The sign-in wasn't finished within ten minutes. Try again.".into(),
                )
            }
        }
    }
}

/// A folder whose `open` and `xdg-open` do nothing, for a login that would
/// open a browser itself (the app opens the page). Under the runtime dir;
/// rewritten when missing.
async fn no_browser_dir() -> std::io::Result<PathBuf> {
    let dir = chimaera_core::runtime_dir().join("no-browser");
    tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&dir)?;
        for name in ["open", "xdg-open"] {
            let path = dir.join(name);
            if !path.exists() {
                std::fs::write(&path, "#!/bin/sh\nexit 0\n")?;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        Ok(dir)
    })
    .await
    .map_err(|_| std::io::Error::other("the no-browser folder could not be prepared"))?
}

/// `dir` first on the environment's PATH (the session PATH, else the
/// daemon's).
fn with_path_prefix(mut env: Vec<(String, String)>, dir: &Path) -> Vec<(String, String)> {
    let prefix = dir.display().to_string();
    if let Some(entry) = env.iter_mut().find(|(k, _)| k == "PATH") {
        entry.1 = format!("{prefix}:{}", entry.1);
    } else {
        let inherited = std::env::var("PATH").unwrap_or_default();
        env.push(("PATH".into(), format!("{prefix}:{inherited}")));
    }
    env
}

/// The login exited cleanly: only the CLI's own status command says whether
/// that was a sign-in.
async fn verify(state: &Arc<AppState>, op: &Operation, kind: AgentKind) -> (Phase, String) {
    lock(&op.progress).message = "Checking the sign-in…".into();
    let bin = match crate::launcher::detect(state, kind, false).await.path {
        Ok(bin) => bin,
        Err(why) => return (Phase::Failed, why),
    };
    match status(state, kind, &bin, true).await {
        Some(true) => (Phase::Succeeded, "Signed in.".into()),
        Some(false) => (
            Phase::Failed,
            format!(
                "{} still reports no sign-in. Try again.",
                kind.product_name()
            ),
        ),
        None => (
            Phase::Failed,
            format!(
                "{} didn't confirm the sign-in. Check it in a terminal, then start the chat again.",
                kind.product_name()
            ),
        ),
    }
}

fn finish(op: &Operation, phase: Phase, message: &str) {
    let mut p = lock(&op.progress);
    p.phase = phase;
    p.message = message.into();
    if !p.running() {
        // A finished page is stale; a code is single-use.
        p.url = None;
        p.code = None;
    }
}

// ------------------------------------------------------------------ text --

/// Terminal output minus its controls: ANSI CSI/OSC sequences and other
/// control characters, newlines and tabs kept.
pub(crate) fn strip_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: parameters/intermediates, then one final byte.
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: until BEL or ST (ESC \).
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            }
            continue;
        }
        if !c.is_control() || matches!(c, '\n' | '\t') {
            out.push(c);
        }
    }
    out
}

fn safe_url(url: &str) -> bool {
    if url.len() > URL_LIMIT || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Ok(uri) = url.parse::<axum::http::Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("https") && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
}

/// Every https URL in `text`, in order.
fn urls(text: &str) -> Vec<String> {
    text.match_indices("https://")
        .filter_map(|(start, _)| {
            let rest = &text[start..];
            let end = rest
                .find(|c: char| {
                    c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>')
                })
                .unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ')', ']']);
            safe_url(url).then(|| url.to_string())
        })
        .collect()
}

/// The hosts each CLI's sign-in page lives on (probed 2026-10-14).
fn login_hosts(kind: AgentKind) -> &'static [&'static str] {
    match kind {
        AgentKind::Claude => &["claude.com", "claude.ai", "anthropic.com"],
        AgentKind::Codex => &["auth.openai.com", "openai.com"],
        AgentKind::Grok => &["accounts.x.ai", "x.ai", "grok.com"],
        AgentKind::Cursor => &["cursor.com", "cursor.sh"],
        AgentKind::Antigravity => &["accounts.google.com", "google.com"],
        AgentKind::Gemini => &[],
    }
}

/// The sign-in page in `text`: a URL on the vendor's login host, else the
/// LAST URL printed — an update banner's link must not win over the login's.
pub(crate) fn login_url(text: &str, kind: AgentKind) -> Option<String> {
    let found = urls(text);
    found
        .iter()
        .find(|url| {
            url.parse::<axum::http::Uri>()
                .ok()
                .and_then(|u| u.host().map(str::to_string))
                .is_some_and(|host| {
                    login_hosts(kind)
                        .iter()
                        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
                })
        })
        .or(found.last())
        .cloned()
}

/// The first https URL in `text` (tests of the URL grammar).
#[cfg(test)]
pub(crate) fn first_url(text: &str) -> Option<String> {
    text.match_indices("https://").find_map(|(start, _)| {
        let rest = &text[start..];
        let end = rest
            .find(|c: char| {
                c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>')
            })
            .unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ',', ')', ']']);
        safe_url(url).then(|| url.to_string())
    })
}

/// A device code on a line of its own: `XLVC-SL7SV` (Codex), `58DZ-24CY`
/// (Grok). Groups of uppercase letters and digits joined by one dash.
pub(crate) fn device_code(text: &str) -> Option<String> {
    text.lines().map(str::trim).find_map(|line| {
        let (a, b) = line.split_once('-')?;
        let group = |s: &str| {
            (4..=8).contains(&s.len())
                && s.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        };
        (group(a) && group(b)).then(|| line.to_string())
    })
}

/// The last line or two of a failed login, plain, for the card's message.
fn failure_tail(output: &str) -> String {
    let text = strip_controls(output);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let tail = lines
        .iter()
        .rev()
        .take(2)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let whole = tail.chars().count();
    let mut tail: String = tail.chars().take(300).collect();
    if whole > 300 {
        tail.push('…');
    }
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vendor_s_login_page_wins_over_a_banner_s_link_else_the_last_url() {
        let out = "A new version is available: https://github.com/vendor/cli/releases\nOpen this link: https://auth.openai.com/codex/device\n";
        assert_eq!(
            login_url(out, AgentKind::Codex).as_deref(),
            Some("https://auth.openai.com/codex/device")
        );
        let out = "see https://docs.example.test/help\nthen https://login.example.test/start\n";
        assert_eq!(
            login_url(out, AgentKind::Cursor).as_deref(),
            Some("https://login.example.test/start")
        );
        assert_eq!(login_url("nothing here", AgentKind::Grok), None);
    }

    #[test]
    fn antigravity_s_print_mode_login_shows_its_page_and_asks_for_the_code() {
        let out = "Authentication required. Please visit the URL to log in:\n  https://accounts.google.com/o/oauth2/auth?access_type=offline&client_id=x&state=y\n\nWaiting for authentication (timeout 60s)...\nOr, paste the authorization code here and press Enter:\n";
        assert_eq!(
            first_url(out).as_deref(),
            Some(
                "https://accounts.google.com/o/oauth2/auth?access_type=offline&client_id=x&state=y"
            )
        );
        assert_eq!(device_code(out), None);
    }

    #[test]
    fn urls_and_codes_come_out_of_each_login_s_output() {
        let cursor = "Starting login process...\nOpen a browser and navigate to this link: https://cursor.com/loginDeepControl?challenge=abc&uuid=1&mode=login&redirectTarget=cli\n";
        assert_eq!(
            first_url(cursor).as_deref(),
            Some("https://cursor.com/loginDeepControl?challenge=abc&uuid=1&mode=login&redirectTarget=cli")
        );
        assert_eq!(device_code(cursor), None);

        let codex = strip_controls("Welcome to Codex [v\x1b[90m0.162.1\x1b[0m]\n1. Open this link in your browser\n   \x1b[94mhttps://auth.openai.com/codex/device\x1b[0m\n\n2. Enter this one-time code \x1b[90m(expires in 15 minutes)\x1b[0m\n   \x1b[94mXLVC-SL7SV\x1b[0m\n");
        assert_eq!(
            first_url(&codex).as_deref(),
            Some("https://auth.openai.com/codex/device")
        );
        assert_eq!(device_code(&codex).as_deref(), Some("XLVC-SL7SV"));

        let grok = "To sign in, open this URL in your browser:\n\n  https://accounts.x.ai/oauth2/device?user_code=58DZ-24CY\n\nConfirm this code in your browser:\n\n  58DZ-24CY\n";
        assert_eq!(
            first_url(grok).as_deref(),
            Some("https://accounts.x.ai/oauth2/device?user_code=58DZ-24CY")
        );
        assert_eq!(device_code(grok).as_deref(), Some("58DZ-24CY"));

        let claude = "Opening browser to sign in…\nIf the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&client_id=x&state=y\nPaste code here if prompted > ";
        assert_eq!(
            first_url(claude).as_deref(),
            Some("https://claude.com/cai/oauth/authorize?code=true&client_id=x&state=y")
        );
        assert_eq!(device_code(claude), None);

        // Never an http page, a credentialed authority, or a hyperlink envelope.
        assert_eq!(first_url("go to http://example.test/login"), None);
        assert_eq!(first_url("https://user:pw@example.test/"), None);
        assert_eq!(
            first_url(&strip_controls(
                "\x1b]8;;https://a.test/x\x07https://a.test/x\x1b]8;;\x07 done"
            ))
            .as_deref(),
            Some("https://a.test/x")
        );
    }

    #[test]
    fn each_cli_s_status_answer_reads_as_a_verdict_or_unknown() {
        assert_eq!(
            parse_probe(
                AgentKind::Claude,
                true,
                r#"{"loggedIn": true, "email": "x"}"#,
                ""
            ),
            Some(true)
        );
        assert_eq!(
            parse_probe(AgentKind::Claude, false, r#"{"loggedIn": false}"#, ""),
            Some(false)
        );
        assert_eq!(parse_probe(AgentKind::Claude, false, "", "boom"), None);
        assert_eq!(
            parse_probe(AgentKind::Codex, true, "Logged in using ChatGPT\n", ""),
            Some(true)
        );
        assert_eq!(
            parse_probe(AgentKind::Codex, false, "Not logged in\n", ""),
            Some(false)
        );
        assert_eq!(parse_probe(AgentKind::Codex, false, "", "panicked"), None);
        assert_eq!(
            parse_probe(
                AgentKind::Cursor,
                true,
                r#"{"status":"unauthenticated","isAuthenticated":false}"#,
                ""
            ),
            Some(false)
        );
        assert_eq!(
            parse_probe(AgentKind::Cursor, true, r#"{"isAuthenticated":true}"#, ""),
            Some(true)
        );
        assert_eq!(
            parse_probe(
                AgentKind::Grok,
                true,
                "You are logged in with grok.com.\n\nDefault model: grok-4.7\n",
                ""
            ),
            Some(true)
        );
        assert_eq!(
            parse_probe(
                AgentKind::Grok,
                true,
                "You are not authenticated.\n\nDefault model: grok-4.6\n",
                ""
            ),
            Some(false)
        );
        assert_eq!(
            parse_probe(
                AgentKind::Antigravity,
                true,
                "Fetching available models...\ngemini-3.8-flash-high\tGemini 3.8 Flash (High)\n",
                ""
            ),
            Some(true)
        );
        assert_eq!(
            parse_probe(
                AgentKind::Antigravity,
                false,
                "",
                "Please sign in to continue"
            ),
            Some(false)
        );
        // 1.3.4 signed out: "Please sign in", exit 1.
        assert_eq!(
            parse_probe(
                AgentKind::Antigravity,
                false,
                "Fetching available models...\nError: Please sign in to view available models. Launch the CLI without arguments to sign in.\n",
                ""
            ),
            Some(false)
        );
        // Offline is unknown, never "signed out".
        assert_eq!(
            parse_probe(
                AgentKind::Antigravity,
                false,
                "",
                "dial tcp: network unreachable"
            ),
            None
        );
    }

    #[test]
    fn every_catalog_agent_has_a_recipe_and_the_table_is_what_the_docs_say() {
        for kind in AgentKind::ALL {
            let recipe = recipe(kind).expect("a sign-in recipe");
            assert_eq!(recipe.how, How::Browser);
            assert!(login_launch(kind).is_some(), "{kind:?} launches headless");
        }
        assert_eq!(recipe(AgentKind::Antigravity).unwrap().how, How::Browser);
        assert!(recipe(AgentKind::Claude).unwrap().accepts_code);
        assert!(!recipe(AgentKind::Antigravity).unwrap().accepts_code);
        assert!(!recipe(AgentKind::Codex).unwrap().accepts_code);
        assert!(login_launch(AgentKind::Antigravity).unwrap().no_browser);
        assert!(!login_launch(AgentKind::Cursor).unwrap().no_browser);
        let env = with_path_prefix(
            vec![("PATH".into(), "/usr/bin".into())],
            Path::new("/x/no-browser"),
        );
        assert_eq!(env[0].1, "/x/no-browser:/usr/bin");
        assert_eq!(with_path_prefix(vec![], Path::new("/x"))[0].0, "PATH");
    }

    #[test]
    fn a_failure_tail_is_short_and_plain() {
        let tail =
            failure_tail("\x1b[31merror:\x1b[0m something went wrong\n\nSee https://x.test\n");
        assert_eq!(tail, "error: something went wrong See https://x.test");
        assert!(failure_tail(&"x".repeat(1000)).chars().count() <= 301);
    }

    fn state() -> Arc<AppState> {
        let root =
            std::env::temp_dir().join(format!("chimaera-auth-{}", chimaera_core::generate_token()));
        Arc::new(AppState::new(
            "token".into(),
            "test".into(),
            1,
            0,
            root.join("data"),
            root.join("config"),
        ))
    }
    fn request(id: &str) -> Request {
        Request {
            workspace_id: "test".into(),
            request_id: id.into(),
            session_id: None,
            theme: None,
        }
    }
    fn script(body: &str, stdin: bool) -> Launch {
        Launch::Browser {
            argv: vec!["/bin/bash".into(), "-c".into(), body.into()],
            env: vec![],
            stdin,
            cwd: "/tmp".into(),
            no_browser: false,
        }
    }
    async fn wait_for(
        state: &AppState,
        kind: AgentKind,
        done: impl Fn(&Progress) -> bool,
    ) -> Progress {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(p) = snapshot(state, kind).filter(&done) {
                    return p;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the operation settles")
    }

    #[tokio::test]
    async fn a_browser_login_shows_its_page_and_code_takes_a_pasted_code_and_never_claims_success_itself(
    ) {
        let state = state();
        // The verdict after a clean exit is the CLI's status command, here a
        // stand-in that answers nothing readable (never the host's real codex).
        let stand_in = std::env::temp_dir().join(format!(
            "chimaera-auth-codex-{}",
            chimaera_core::generate_token()
        ));
        std::fs::write(&stand_in, "#!/bin/sh\nexit 0\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        lock(&state.agent_bins).insert(
            AgentKind::Codex,
            crate::launcher::AgentDetection {
                path: Ok(stand_in),
                version: None,
                managed: false,
                explicit: false,
                mtime: None,
            },
        );
        // Prints a URL and a device code, then waits for the pasted code.
        start_operation(
            state.clone(),
            AgentKind::Codex,
            request("one"),
            script("echo 'open https://auth.example.test/device now'; echo '  ABCD-EFGH1  '; read code; echo \"got $code\"; exit 0", true),
        )
        .await;
        let p = wait_for(&state, AgentKind::Codex, |p| {
            p.url.is_some() && p.code.is_some()
        })
        .await;
        assert_eq!(p.url.as_deref(), Some("https://auth.example.test/device"));
        assert_eq!(p.code.as_deref(), Some("ABCD-EFGH1"));
        assert!(p.running());
        // A second click joins; a stale cancel is refused.
        start_operation(
            state.clone(),
            AgentKind::Codex,
            request("two"),
            script("echo WRONG", false),
        )
        .await;
        assert_eq!(snapshot(&state, AgentKind::Codex).unwrap().id, "one");
        let stale = cancel(State(state.clone()), AxPath(("codex".into(), "two".into()))).await;
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        // The code reaches the login's stdin.
        let fed = code(
            State(state.clone()),
            AxPath(("codex".into(), "one".into())),
            Json(Code {
                code: " PASTED-1 ".into(),
            }),
        )
        .await;
        assert_eq!(
            fed.status(),
            StatusCode::CONFLICT,
            "codex never reads a code"
        );
        let op = lock(&state.agent_auth.operations)
            .get(&AgentKind::Codex)
            .cloned()
            .unwrap();
        lock(&op.progress).accepts_code = true;
        let fed = code(
            State(state.clone()),
            AxPath(("codex".into(), "one".into())),
            Json(Code {
                code: " PASTED-1 ".into(),
            }),
        )
        .await;
        assert_eq!(fed.status(), StatusCode::OK);
        let p = wait_for(&state, AgentKind::Codex, |p| !p.running()).await;
        // A clean exit is verified by the CLI's status command — absent here,
        // so the flow refuses to call it a sign-in.
        assert_eq!(p.phase, Phase::Failed);
        assert!(p.message.contains("didn't confirm"), "{}", p.message);
        assert!(
            p.url.is_none() && p.code.is_none(),
            "a finished page is stale"
        );
        assert!(lock(&op.output).contains("got PASTED-1"));
    }

    #[tokio::test]
    async fn a_failed_login_carries_a_short_tail_and_cancel_stops_a_waiting_one() {
        let state = state();
        start_operation(
            state.clone(),
            AgentKind::Cursor,
            request("one"),
            script(
                "echo 'Error: could not reach the sign-in service' >&2; exit 3",
                false,
            ),
        )
        .await;
        let p = wait_for(&state, AgentKind::Cursor, |p| !p.running()).await;
        assert_eq!(p.phase, Phase::Failed);
        assert!(p.message.contains("could not reach"), "{}", p.message);

        start_operation(
            state.clone(),
            AgentKind::Cursor,
            request("two"),
            script("trap 'exit 0' TERM; echo waiting; sleep 120", false),
        )
        .await;
        wait_for(&state, AgentKind::Cursor, |p| p.running() && p.id == "two").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel(
            State(state.clone()),
            AxPath(("cursor".into(), "two".into())),
        )
        .await;
        let p = wait_for(&state, AgentKind::Cursor, |p| !p.running()).await;
        assert_eq!(p.phase, Phase::Cancelled);
        assert!(
            state.sessions.list().is_empty(),
            "a browser login is never a session"
        );
    }
}
