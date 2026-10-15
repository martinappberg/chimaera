//! Sign in to an agent CLI from the app. Two halves, one flow for every
//! runtime: a bounded **status probe** (each CLI's own status command, cached
//! on the agent catalog), and a bounded, cancellable **sign-in operation**
//! that runs the CLI's own login under the daemon — headless where the CLI
//! offers it (the URL and device code it prints are captured for the UI, a
//! pasted code is fed back on stdin) and in a managed terminal where it does
//! not. Chimaera never touches credentials: the CLI owns the login and its
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
/// A managed-terminal sign-in re-probes this often while the terminal lives.
const TERMINAL_POLL: Duration = Duration::from_secs(15);
const URL_LIMIT: usize = 8192;

/// How this agent signs in.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum How {
    /// The CLI's headless login under the daemon: it prints a URL (and for
    /// device-code flows a code) and waits; the UI opens the URL.
    Browser,
    /// The CLI only signs in from its own interactive UI: a managed terminal
    /// runs it and the daemon watches for the login to land.
    Terminal,
}

/// The per-runtime truth table (docs/features/agents.md, "Sign in").
pub(crate) struct Recipe {
    pub(crate) how: How,
    /// What the login runs, in the user's words (shown in Settings).
    pub(crate) command: &'static str,
    /// Claude's browser page shows a code the user pastes back.
    pub(crate) accepts_code: bool,
}

pub(crate) fn recipe(kind: AgentKind) -> Option<Recipe> {
    Some(match kind {
        AgentKind::Claude => Recipe {
            how: How::Browser,
            command: "claude auth login",
            accepts_code: true,
        },
        AgentKind::Codex => Recipe {
            how: How::Browser,
            command: "codex login --device-auth",
            accepts_code: false,
        },
        AgentKind::Grok => Recipe {
            how: How::Browser,
            command: "grok login --device-auth",
            accepts_code: false,
        },
        AgentKind::Cursor => Recipe {
            how: How::Browser,
            command: "cursor-agent login",
            accepts_code: false,
        },
        AgentKind::Antigravity => Recipe {
            how: How::Terminal,
            command: "agy",
            accepts_code: false,
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
}

fn login_launch(kind: AgentKind) -> Option<Headless> {
    Some(match kind {
        AgentKind::Claude => Headless {
            args: &["auth", "login"],
            env: &[("BROWSER", "true")],
        },
        AgentKind::Codex | AgentKind::Grok => Headless {
            args: &["login", "--device-auth"],
            env: &[],
        },
        AgentKind::Cursor => Headless {
            args: &["login"],
            env: &[("NO_OPEN_BROWSER", "1")],
        },
        AgentKind::Antigravity | AgentKind::Gemini => return None,
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
                && [
                    "log in",
                    "login",
                    "sign in",
                    "authenticat",
                    "credential",
                    "oauth",
                ]
                .iter()
                .any(|needle| lower.contains(needle))
            {
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

/// Ask the CLI itself whether it is signed in. `None` = unknown (no status
/// command, it did not answer in time, or an unreadable answer).
async fn run_probe(kind: AgentKind, bin: &Path) -> Option<bool> {
    let argv = probe_argv(kind)?;
    let output = tokio::process::Command::new(bin)
        .args(argv)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(PROBE_TIMEOUT, output)
        .await
        .ok()?
        .ok()?;
    let cap = |bytes: &[u8]| {
        String::from_utf8_lossy(&bytes[..bytes.len().min(OUTPUT_LIMIT)]).into_owned()
    };
    parse_probe(
        kind,
        out.status.success(),
        &cap(&out.stdout),
        &cap(&out.stderr),
    )
}

/// The cached verdict for `bin`, probing when the cache has none, is stale,
/// is about another executable, or `fresh` is asked. Concurrent callers for
/// one agent share a single probe.
pub(crate) async fn status(
    state: &AppState,
    kind: AgentKind,
    bin: &Path,
    fresh: bool,
) -> Option<bool> {
    recipe(kind)?;
    let cached = |state: &AppState| {
        lock(&state.agent_auth.status)
            .get(&kind)
            .filter(|s| s.bin == bin && s.checked_at.elapsed() < STATUS_TTL)
            .map(|s| s.signed_in)
    };
    if !fresh {
        if let Some(verdict) = cached(state) {
            return verdict;
        }
    }
    let probing = probe_lock(state, kind);
    let _guard = probing.lock().await;
    // A waiter behind a fresh probe reads what it found.
    if !fresh
        || lock(&state.agent_auth.status)
            .get(&kind)
            .is_some_and(|s| s.bin == bin && s.checked_at.elapsed() < Duration::from_secs(2))
    {
        if let Some(verdict) = cached(state) {
            return verdict;
        }
    }
    let signed_in = run_probe(kind, bin).await;
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
    /// The managed terminal running the CLI (the terminal flow).
    terminal_session: Option<String>,
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
    #[serde(default)]
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
    let theme = match body.theme.as_deref() {
        Some("light") => "light",
        _ => "dark",
    };
    let launch = match recipe.how {
        How::Browser => {
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
            }
        }
        How::Terminal => Launch::Terminal {
            workspace,
            theme: theme.to_string(),
        },
    };
    no_store(start_operation(state, kind, body, launch).await)
}

/// What a sign-in runs.
pub(crate) enum Launch {
    Browser {
        argv: Vec<String>,
        env: Vec<(String, String)>,
        /// Keep stdin open for a pasted code.
        stdin: bool,
        cwd: PathBuf,
    },
    Terminal {
        workspace: crate::workspaces::Workspace,
        theme: String,
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
        terminal_session: None,
        message: match recipe.how {
            How::Browser => "Starting the sign-in…".into(),
            How::Terminal => "Opening a terminal…".into(),
        },
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
            } => {
                run_browser(
                    &state, &operation, kind, argv, env, stdin, cwd, cancelled, codes,
                )
                .await
            }
            Launch::Terminal { workspace, theme } => {
                run_terminal(&state, &operation, kind, workspace, theme, cancelled).await
            }
        };
        finish(&operation, phase, &message);
        // The verdict the flow ended on is the one the catalog serves.
        state.changes.notify_waiters();
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
fn harvest(op: &Operation) {
    let text = strip_controls(&lock(&op.output));
    let complete = text
        .rfind(char::is_whitespace)
        .map_or("", |end| &text[..end]);
    let mut p = lock(&op.progress);
    if p.url.is_none() {
        if let Some(url) = first_url(complete) {
            p.url = Some(url);
            p.message = "Finish signing in in your browser.".into();
        }
    }
    if p.code.is_none() {
        p.code = device_code(complete);
    }
}

async fn drain(mut pipe: impl AsyncRead + Unpin, op: &Operation) {
    let mut buf = [0; 4096];
    while let Ok(n) = pipe.read(&mut buf).await {
        if n == 0 {
            break;
        }
        append(op, &buf[..n]);
        harvest(op);
    }
}

#[allow(clippy::too_many_arguments)] // one call site; a struct would wrap the launch again
async fn run_browser(
    state: &Arc<AppState>,
    op: &Operation,
    kind: AgentKind,
    argv: Vec<String>,
    extra_env: Vec<(String, String)>,
    stdin: bool,
    cwd: PathBuf,
    cancel: watch::Receiver<bool>,
    mut codes: mpsc::Receiver<String>,
) -> (Phase, String) {
    if *cancel.borrow() {
        return (Phase::Cancelled, "Sign-in cancelled.".into());
    }
    let env = crate::api::session_env(state, &lock(&op.progress).id, "dark", None);
    let remove = crate::api::spawn_env_remove(&env);
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
    let group = Group(nix::unistd::Pid::from_raw(child.id().unwrap_or(0) as i32));
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let mut stdin = child.stdin.take();
    lock(&op.progress).message = "Waiting for the sign-in page…".into();
    let reads = async {
        tokio::join!(drain(stdout, op), drain(stderr, op));
    };
    tokio::pin!(reads);
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
            // Keep the leader unreaped while its pipes are open so a cancel
            // cannot signal a recycled process-group id.
            result = async { (&mut reads).await; child.wait().await } => break Some(result),
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
            std::mem::forget(group);
            let code = result.ok().and_then(|s| s.code());
            if code == Some(0) {
                verify(state, op, kind, &PathBuf::from(&argv[0])).await
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
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut reads).await;
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

/// The login exited cleanly: only the CLI's own status command says whether
/// that was a sign-in.
async fn verify(
    state: &Arc<AppState>,
    op: &Operation,
    kind: AgentKind,
    bin: &Path,
) -> (Phase, String) {
    lock(&op.progress).message = "Checking the sign-in…".into();
    match status(state, kind, bin, true).await {
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

/// The terminal flow: the CLI's own UI in a managed terminal, watched until
/// its status command reports a login, the terminal closes, or ten minutes.
async fn run_terminal(
    state: &Arc<AppState>,
    op: &Operation,
    kind: AgentKind,
    workspace: crate::workspaces::Workspace,
    theme: String,
    mut cancel: watch::Receiver<bool>,
) -> (Phase, String) {
    let spec = crate::spawn::SpawnSpec {
        workspace,
        id: None,
        name: Some(format!("sign in to {}", kind.product_name())),
        cwd: None,
        cols: None,
        rows: None,
        theme,
        title_hint: None,
        prelude: None,
        kind: crate::spawn::SpawnKind::Agent {
            kind,
            model: None,
            resume: None,
        },
        started_by: crate::history::StartedBy::You,
    };
    let session = match crate::spawn::spawn_session(state, spec).await {
        Ok(json) => json["id"].as_str().unwrap_or_default().to_string(),
        Err(crate::spawn::SpawnFailure::AgentUnavailable(why)) => return (Phase::Failed, why),
        Err(crate::spawn::SpawnFailure::Internal(err)) => {
            return (Phase::Failed, format!("Couldn't open a terminal: {err}"))
        }
    };
    {
        let mut p = lock(&op.progress);
        p.terminal_session = Some(session.clone());
        p.message = format!(
            "Sign in to {} in the terminal that opened.",
            kind.product_name()
        );
    }
    state.changes.notify_waiters();
    let bin = match crate::launcher::detect(state, kind, false).await.path {
        Ok(bin) => bin,
        Err(why) => return (Phase::Failed, why),
    };
    let started = Instant::now();
    loop {
        let alive = state.sessions.get(&session).is_some();
        if let Some(true) = status(state, kind, &bin, true).await {
            return (Phase::Succeeded, "Signed in.".into());
        }
        if !alive {
            return (
                Phase::Failed,
                format!(
                    "The terminal closed before {} reported a sign-in. Try again.",
                    kind.product_name()
                ),
            );
        }
        if started.elapsed() >= DEADLINE {
            return (
                Phase::Failed,
                "Still not signed in after ten minutes. Finish in the terminal, then check again."
                    .into(),
            );
        }
        tokio::select! {
            _ = tokio::time::sleep(TERMINAL_POLL) => {}
            _ = cancel.changed() => {
                let _ = state.sessions.kill(&session);
                return (Phase::Cancelled, "Sign-in cancelled.".into());
            }
        }
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

/// The first https URL in `text` — the sign-in page every login prints.
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
    let mut tail: String = tail.chars().take(300).collect();
    if tail.len() < tail.capacity() && lines.join(" ").chars().count() > 300 {
        tail.push('…');
    }
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

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
            match recipe.how {
                How::Browser => assert!(login_launch(kind).is_some(), "{kind:?} launches headless"),
                How::Terminal => {
                    assert!(login_launch(kind).is_none(), "{kind:?} signs in in its TUI")
                }
            }
        }
        assert_eq!(recipe(AgentKind::Antigravity).unwrap().how, How::Terminal);
        assert!(recipe(AgentKind::Claude).unwrap().accepts_code);
        assert!(!recipe(AgentKind::Codex).unwrap().accepts_code);
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
            theme: None,
        }
    }
    fn script(body: &str, stdin: bool) -> Launch {
        Launch::Browser {
            argv: vec!["/bin/bash".into(), "-c".into(), body.into()],
            env: vec![],
            stdin,
            cwd: "/tmp".into(),
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
