//! Programs a plugin runs, as jobs the host owns (docs/plugin-platform-plan.md
//! §6). Only a program the manifest declares (`[[programs]]`) can run, by
//! name: the host resolves it on the PATH the user's terminals get (the
//! login shell plus the environment prelude, captured once per prelude),
//! else in the plugin's own tools (`toolchain`). A plugin never passes a
//! path to a binary, and there is no shell between it and the program.
//!
//! What the host enforces, whoever asked (a person or an agent's tool):
//!
//! - **A queue:** at most `RUNNING_MAX` jobs daemon-wide, one per plugin,
//!   `WAITING_MAX` waiting per plugin, taken by priority (`user`, `agent`,
//!   `background`) then age.
//! - **Time:** `wall_s` (60 s unless the job says, at most 600 s), then
//!   SIGTERM to the job's process group and SIGKILL 5 s later.
//! - **Limits** set in the job's own process before the program starts, by
//!   a fixed POSIX `sh` preamble (no daemon memory is touched, no `unsafe`):
//!   `ulimit -v` (4 GiB of address space), `-t` (CPU: the wall time plus
//!   slack), `-f` (256 MiB per written file), then `nice -n 10` and, where
//!   there is one, `ionice -c 3`. A limit the kernel refuses (macOS and
//!   `-v`) is skipped, never a failed job.
//! - **Output:** stdout and stderr to `output:.jobs/<id>/{stdout,stderr}.log`,
//!   `LOG_MAX` each (the rest is drained and dropped); never into daemon
//!   memory. stdin is closed unless the spec gives it (≤ 4 MiB).
//! - **Environment:** the captured login environment minus the daemon's own
//!   variables, plus the spec's `env` — never `PATH`, `HOME`, `SHELL`,
//!   `LD_*`, `DYLD_*` or `CHIMAERA_*`, which the host owns.
//!
//! When a job ends the plugin hears `job-finished` (with the 30 s budget
//! `knowledge` has), a `job` frame tells the workspace's windows, and an
//! agent tool waiting on it (`tool-result.wait`) is answered.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::runtime::wit;
use super::{EventKind, Manifest};
use crate::AppState;

const RUNNING_MAX: usize = 2;
const WAITING_MAX: usize = 8;
const WALL_DEFAULT: u64 = 60;
const WALL_MAX: u64 = 600;
const TERM_GRACE: Duration = Duration::from_secs(5);
/// Address space, in KiB (`ulimit -v`).
const AS_KB: u64 = 4 << 20;
/// A written file, in 512-byte blocks (`ulimit -f`, POSIX units): 256 MiB.
const FSIZE_BLOCKS: u64 = (256 << 20) / 512;
pub(crate) const LOG_MAX: u64 = 16 << 20;
const STDIN_MAX: usize = 4 << 20;
/// Input all waiting jobs hold together, daemon-wide: a queue full of
/// 4 MiB inputs across plugins must not grow the daemon past its budget.
const STDIN_WAITING_MAX: usize = 16 << 20;
const ENV_MAX: usize = 64;
const ARGS_MAX: usize = 256;
const ARG_LEN_MAX: usize = 16 << 10;
/// Finished jobs kept for `job-status` (the newest).
const FINISHED_KEPT: usize = 128;
/// Jobs whose logs an output folder keeps (the newest).
const JOB_LOGS_KEPT: usize = 32;
/// Arguments an activity entry keeps (the rest are the log's detail).
const ACTIVITY_ARGS: usize = 4;
/// How long the captured login environment is trusted before it is
/// captured again (a profile edit, a new module).
const ENV_TTL: Duration = Duration::from_secs(10 * 60);
const ENV_CAPTURE_TIMEOUT: Duration = Duration::from_secs(20);
/// How long an agent's tool call waits on its job before answering
/// "still running" (under the agents' MCP timeouts).
pub(crate) const TOOL_WAIT: Duration = Duration::from_secs(45);

/// Variables the host owns: a job's `env` never sets them, in any case
/// (zsh ties `path` to `PATH`).
fn reserved(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(
        name.as_str(),
        "PATH" | "HOME" | "SHELL" | "USER" | "LOGNAME" | "IFS" | "ENV" | "BASH_ENV"
    ) || name.starts_with("LD_")
        || name.starts_with("DYLD_")
        || name.starts_with("CHIMAERA_")
}

/// A portable variable name, either case: TeX's own settings are lowercase
/// (`max_print_line`, `openout_any`) and kpathsea reads them from the
/// environment.
fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Priority {
    User,
    Agent,
    Background,
}

/// A job's spec, as `job-start` receives it.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    stdin: Option<String>,
    #[serde(default, alias = "wall-s")]
    wall_s: Option<u64>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    /// `tool:<id>`: this plugin's own copy even when the user has one.
    #[serde(default)]
    prefer: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum State {
    Queued,
    Running,
    Done,
}

/// One job, shared by the queue, its runner and whoever waits on it.
pub(crate) struct Job {
    pub(crate) id: String,
    pub(crate) plugin: String,
    pub(crate) workspace: String,
    program: String,
    /// `path` or `tool:<id>`: which copy of the program it runs.
    from: String,
    label: String,
    priority: Priority,
    seq: u64,
    status: Mutex<Status>,
    /// Flips to true when the job ends.
    done: tokio::sync::watch::Sender<bool>,
    cancel: tokio::sync::Notify,
    launch: Mutex<Option<Launch>>,
    /// Its `stdin`'s size, held until it starts.
    stdin_bytes: usize,
    /// Its process group once it runs (0 before): what `kill_all` stops.
    group: std::sync::atomic::AtomicI32,
}

#[derive(Clone)]
struct Status {
    state: State,
    exit: Option<i32>,
    timed_out: bool,
    cancelled: bool,
    error: Option<String>,
    queued_ms: u64,
    started_ms: Option<u64>,
    finished_ms: Option<u64>,
    started: Option<Instant>,
    duration_ms: u64,
}

/// What the runner needs to start the process.
struct Launch {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    stdin: Option<Vec<u8>>,
    wall: Duration,
    output: PathBuf,
}

impl Job {
    fn status(&self) -> Status {
        crate::lock(&self.status).clone()
    }

    /// `job-status`'s answer.
    pub(crate) fn json(&self) -> Value {
        let s = self.status();
        json!({
            "id": self.id,
            "state": match s.state {
                State::Queued => "queued",
                State::Running => "running",
                State::Done => "done",
            },
            "program": self.program,
            // Which copy runs it: the user's (`path`) or a tool of the
            // plugin's (`tool:<id>`), e.g. whether a missing TeX package
            // may be installed into it.
            "from": self.from,
            "label": self.label,
            "exit": s.exit,
            "timed_out": s.timed_out,
            "cancelled": s.cancelled,
            "error": s.error,
            "queued_ms": s.queued_ms,
            "started_ms": s.started_ms,
            "finished_ms": s.finished_ms,
            "duration_ms": s.duration_ms,
            "stdout": format!("output:.jobs/{}/stdout.log", self.id),
            "stderr": format!("output:.jobs/{}/stderr.log", self.id),
        })
    }

    pub(crate) fn is_done(&self) -> bool {
        self.status().state == State::Done
    }
}

/// The captured login environment, per prelude text.
struct Captured {
    env: Arc<Vec<(String, String)>>,
    at: Instant,
}

/// The host's jobs (on `Platform`).
#[derive(Default)]
pub(crate) struct Jobs {
    inner: Mutex<Inner>,
    envs: tokio::sync::Mutex<HashMap<String, Captured>>,
}

#[derive(Default)]
struct Inner {
    seq: u64,
    running: usize,
    waiting: Vec<Arc<Job>>,
    live: HashMap<String, Arc<Job>>,
    finished: VecDeque<Arc<Job>>,
}

impl Jobs {
    pub(crate) fn get(&self, id: &str) -> Option<Arc<Job>> {
        let inner = crate::lock(&self.inner);
        inner
            .live
            .get(id)
            .cloned()
            .or_else(|| inner.finished.iter().find(|j| j.id == id).cloned())
    }

    /// Every running job's process group gets SIGKILL now (the daemon is
    /// stopping: `kill_on_drop` would reach only each group's leader, and
    /// what it started would run on unowned).
    pub(crate) fn kill_all(&self) {
        let groups: Vec<i32> = crate::lock(&self.inner)
            .live
            .values()
            .map(|j| j.group.load(std::sync::atomic::Ordering::Relaxed))
            .filter(|g| *g > 0)
            .collect();
        for g in groups {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(g),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }

    /// Record a job's end and move it to the finished ring.
    fn finish(
        &self,
        job: &Arc<Job>,
        exit: Option<i32>,
        timed_out: bool,
        cancelled: bool,
        error: Option<String>,
    ) {
        {
            let mut s = crate::lock(&job.status);
            s.state = State::Done;
            s.exit = exit;
            s.timed_out = timed_out;
            s.cancelled = cancelled;
            s.error = error;
            s.finished_ms = Some(crate::timeline::now_ms());
            s.duration_ms = s.started.map_or(0, |t| t.elapsed().as_millis() as u64);
        }
        let mut inner = crate::lock(&self.inner);
        inner.live.remove(&job.id);
        inner.finished.push_back(job.clone());
        while inner.finished.len() > FINISHED_KEPT {
            inner.finished.pop_front();
        }
        drop(inner);
        // `send` drops the value when no one has subscribed yet, and a job
        // can end before its agent's call starts waiting on it.
        job.done.send_replace(true);
    }
}

/// Stop one job (queued: it never starts, and ends like any other job —
/// its frame, an activity line, the plugin's `job-finished` where it is
/// still on; running: its process group gets SIGTERM, then SIGKILL).
pub(crate) fn cancel(state: &Arc<AppState>, id: &str) {
    let jobs = &state.plugin_platform.jobs;
    let Some(job) = crate::lock(&jobs.inner).live.get(id).cloned() else {
        return;
    };
    let queued = {
        let mut inner = crate::lock(&jobs.inner);
        match inner.waiting.iter().position(|w| w.id == id) {
            Some(at) => {
                inner.waiting.remove(at);
                true
            }
            None => false,
        }
    };
    if !queued {
        job.cancel.notify_one();
        return;
    }
    jobs.finish(
        &job,
        None,
        false,
        true,
        Some("cancelled before it started".into()),
    );
    let state = state.clone();
    tokio::spawn(async move {
        let entry = json!({
            "kind": "job",
            "job": job.id,
            "workspace": job.workspace,
            "program": job.program,
            "cancelled": true,
            "started": false,
        });
        super::activity::record(&state, &job.plugin, entry).await;
        after(&state, &job).await;
    });
}

/// Stop every job of `plugin` (a hard block, a remove), or only those in
/// `workspace` (switched off there).
pub(crate) fn cancel_where(state: &Arc<AppState>, plugin: &str, workspace: Option<&str>) {
    let ids: Vec<String> = crate::lock(&state.plugin_platform.jobs.inner)
        .live
        .values()
        .filter(|j| j.plugin == plugin && workspace.is_none_or(|w| j.workspace == w))
        .map(|j| j.id.clone())
        .collect();
    for id in ids {
        cancel(state, &id);
    }
}

/// Wait (at most `limit`) for `id` to end; whether it did.
pub(crate) async fn wait(state: &AppState, plugin: &str, id: &str, limit: Duration) -> bool {
    let Some(job) = state.plugin_platform.jobs.get(id) else {
        return false;
    };
    if job.plugin != plugin {
        return false;
    }
    let mut rx = job.done.subscribe();
    if *rx.borrow() {
        return true;
    }
    let ended = tokio::time::timeout(limit, rx.wait_for(|d| *d)).await;
    ended.is_ok()
}

fn frame(state: &AppState, job: &Job) {
    let mut v = job.json();
    v["type"] = json!("job");
    v["plugin"] = json!(job.plugin);
    v["workspace"] = json!(job.workspace);
    state
        .plugin_runtime
        .push_event(&job.workspace, v.to_string());
    state.changes.notify_waiters();
}

/// The login environment for `ws` (the user's shell, `-l`, and the
/// prelude), captured once per prelude text and `ENV_TTL`.
async fn login_env(
    state: &Arc<AppState>,
    ws: Option<&str>,
) -> Result<Arc<Vec<(String, String)>>, String> {
    // The cluster job's startup commands are a scope like any other: in the
    // key, so an edit there re-captures too.
    let startup = crate::environment::job_startup().await;
    let prelude_text = {
        let mut preludes = crate::lock(&state.env_preludes);
        match ws {
            Some(ws) => preludes.current().effective(startup.as_deref(), ws, None),
            None => preludes.current().host_only(startup.as_deref()),
        }
    };
    let key = format!("{}\u{0}{prelude_text}", crate::launcher::login_shell());
    let mut envs = state.plugin_platform.jobs.envs.lock().await;
    if let Some(c) = envs.get(&key).filter(|c| c.at.elapsed() < ENV_TTL) {
        return Ok(c.env.clone());
    }
    let env = Arc::new(capture_env(state, ws).await?);
    envs.retain(|_, c| c.at.elapsed() < ENV_TTL);
    envs.insert(
        key,
        Captured {
            env: env.clone(),
            at: Instant::now(),
        },
    );
    Ok(env)
}

async fn capture_env(
    state: &Arc<AppState>,
    ws: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let prelude = {
        let state = state.clone();
        let ws = ws.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            crate::environment::materialize_probe_prelude(&state, ws.as_deref())
        })
        .await
        .ok()
        .flatten()
    };
    let argv = crate::launcher::wrap_login_shell(
        &crate::launcher::login_shell(),
        vec!["env".into(), "-0".into()],
    );
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .process_group(0);
    let env: Vec<(String, String)> = prelude
        .as_ref()
        .map(|p| ("CHIMAERA_PRELUDE".to_string(), p.display().to_string()))
        .into_iter()
        .collect();
    for name in crate::api::spawn_env_remove(&env) {
        cmd.env_remove(name);
    }
    cmd.envs(env);
    let out = async {
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("the login shell did not start: {e}"))?;
        let mut stdout = child.stdout.take().ok_or("no stdout")?;
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(1 << 20)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        let _ = child.wait().await;
        Ok::<_, String>(bytes)
    };
    let result = tokio::time::timeout(ENV_CAPTURE_TIMEOUT, out).await;
    if let Some(p) = prelude {
        tokio::task::spawn_blocking(move || crate::environment::remove_prelude_path(&p));
    }
    let bytes = result.map_err(|_| "the login shell took too long to start".to_string())??;
    let text = String::from_utf8_lossy(&bytes);
    // `env -0`; a shell whose `env` has no `-0` printed lines instead.
    let sep = if text.contains('\0') { '\0' } else { '\n' };
    // The daemon's own markers never reach a job (the spawn hygiene).
    let launcher = crate::api::launcher_context_env();
    let env: Vec<(String, String)> = text
        .split(sep)
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, _)| {
            !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !k.starts_with("CHIMAERA_")
                && !launcher.iter().any(|n| n == k)
        })
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if env.is_empty() {
        return Err("the login shell printed no environment".into());
    }
    Ok(env)
}

/// `path`'s absolute entries. A job starts in the workspace, so a relative
/// one (`.`, an empty entry, `bin`) would find a project's file first — for
/// the preamble's `nice`, and for every program the program starts.
fn absolute_path(path: &str) -> String {
    let kept: Vec<&str> = path.split(':').filter(|d| d.starts_with('/')).collect();
    if kept.is_empty() {
        "/usr/local/bin:/usr/bin:/bin".to_string()
    } else {
        kept.join(":")
    }
}

/// `name` in the first absolute `path` folder holding an executable file.
/// A relative entry (`.`, `bin`) is skipped: the job starts in the
/// workspace, so it would run whatever file of that name the project has.
fn on_path(name: &str, path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.split(':')
        .filter(|d| d.starts_with('/'))
        .find_map(|dir| {
            let candidate = Path::new(dir).join(name);
            let meta = std::fs::metadata(&candidate).ok()?;
            (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(candidate)
        })
}

/// Where `program` runs from: the user's copy on the login PATH, unless
/// `prefer` names one of the plugin's tools; else the plugin's installed
/// tool that provides it. With that tool's id and the folder to put first
/// on the job's PATH.
async fn resolve(
    state: &Arc<AppState>,
    m: &Manifest,
    program: &str,
    prefer: Option<&str>,
    path: &str,
) -> Result<(PathBuf, Option<(String, PathBuf)>), String> {
    let providers: Vec<&super::platform::ToolDecl> = m
        .tools
        .iter()
        .filter(|t| t.programs.iter().any(|p| p == program))
        .collect();
    if let Some(tool) = prefer.and_then(|p| p.strip_prefix("tool:")) {
        if !providers.iter().any(|t| t.id == tool) {
            return Err(format!("{} provides no {program} to prefer", tool));
        }
        let bin = super::toolchain::installed_bin(state, &m.id, tool)
            .await
            .ok_or_else(|| format!("{tool} is not installed: install it first"))?;
        let exe = bin.join(program);
        return Ok((exe, Some((tool.to_string(), bin))));
    }
    let name = program.to_string();
    let path = path.to_string();
    let found = tokio::task::spawn_blocking(move || on_path(&name, &path))
        .await
        .ok()
        .flatten();
    if let Some(exe) = found {
        return Ok((exe, None));
    }
    for t in providers {
        if let Some(bin) = super::toolchain::installed_bin(state, &m.id, &t.id).await {
            let exe = bin.join(program);
            if tokio::fs::metadata(&exe).await.is_ok() {
                return Ok((exe, Some((t.id.clone(), bin))));
            }
        }
    }
    Err(format!(
        "{program} was not found on this host's PATH{}",
        if m.tools.is_empty() {
            String::new()
        } else {
            " and none of this plugin's tools that provide it is installed".to_string()
        }
    ))
}

/// `job-start` for `m` in `ws`: checked, queued, maybe started.
pub(crate) async fn start(
    state: &Arc<AppState>,
    m: &Arc<Manifest>,
    ws: &str,
    spec: &str,
) -> Result<String, String> {
    if spec.len() > STDIN_MAX + (1 << 20) {
        return Err("a job spec is at most 5 MiB".into());
    }
    let spec: Spec = serde_json::from_str(spec).map_err(|e| format!("the job spec: {e}"))?;
    if !m.programs.iter().any(|p| p.name == spec.program) {
        return Err(format!(
            "{} is not one of {}'s declared programs ([[programs]])",
            spec.program, m.name
        ));
    }
    if spec.args.len() > ARGS_MAX
        || spec
            .args
            .iter()
            .any(|a| a.len() > ARG_LEN_MAX || a.contains('\0'))
    {
        return Err(format!(
            "at most {ARGS_MAX} arguments of at most 16 KiB, without NUL"
        ));
    }
    if spec.env.len() > ENV_MAX {
        return Err(format!("at most {ENV_MAX} environment variables"));
    }
    for (k, v) in &spec.env {
        if !valid_env_name(k) {
            return Err(format!("{k:?} is not an environment variable name"));
        }
        if reserved(k) {
            return Err(format!("{k} is the host's to set"));
        }
        if v.len() > 4096 || v.contains('\0') {
            return Err(format!("{k}: a value is at most 4 KiB, without NUL"));
        }
    }
    let stdin = spec.stdin.map(String::into_bytes);
    if stdin.as_ref().is_some_and(|s| s.len() > STDIN_MAX) {
        return Err("stdin is at most 4 MiB".into());
    }
    let priority = match spec.priority.as_deref() {
        None | Some("user") => Priority::User,
        Some("agent") => Priority::Agent,
        Some("background") => Priority::Background,
        Some(other) => return Err(format!("priority {other:?} is user, agent or background")),
    };
    let wall = Duration::from_secs(spec.wall_s.unwrap_or(WALL_DEFAULT).clamp(1, WALL_MAX));
    let label = crate::timeline::cap(spec.label.as_deref().unwrap_or(&spec.program), 200);

    let root = crate::lock(&state.workspaces)
        .get(ws)
        .map(|w| w.root)
        .ok_or("this workspace is gone")?;
    let output = super::output::folder(&state.plugin_platform.output_root, &m.id, ws);
    let cwd = match spec.cwd.as_deref() {
        None | Some("") => root.clone(),
        Some(c) if c.starts_with("output:") => {
            let rel = super::output::relative(c)?;
            let dir = output.clone();
            let made = rel.clone();
            tokio::task::spawn_blocking(move || super::output::make_dir(&dir, &made))
                .await
                .map_err(|e| e.to_string())??;
            output.join(rel)
        }
        Some(c) => {
            let rel = super::hostfns_relative(c)?;
            let root = root.clone();
            let c = c.to_string();
            tokio::task::spawn_blocking(move || {
                let dir =
                    std::fs::canonicalize(root.join(&rel)).map_err(|e| format!("{c}: {e}"))?;
                let base = std::fs::canonicalize(&root).map_err(|e| e.to_string())?;
                if !dir.starts_with(&base) || !dir.is_dir() {
                    return Err(format!("{c}: not a folder inside the workspace"));
                }
                Ok(dir)
            })
            .await
            .map_err(|e| e.to_string())??
        }
    };

    let env = login_env(state, Some(ws)).await?;
    let path = absolute_path(
        env.iter()
            .find(|(k, _)| k == "PATH")
            .map_or("", |(_, v)| v.as_str()),
    );
    let (program, tool_bin) =
        resolve(state, m, &spec.program, spec.prefer.as_deref(), &path).await?;
    let mut job_env: Vec<(String, String)> =
        env.iter().filter(|(k, _)| k != "PATH").cloned().collect();
    let job_path = match &tool_bin {
        Some((_, bin)) => format!("{}:{path}", bin.display()),
        None => path,
    };
    let from = tool_bin
        .as_ref()
        .map_or_else(|| "path".to_string(), |(id, _)| format!("tool:{id}"));
    job_env.push(("PATH".into(), job_path));
    job_env.push(("TERM".into(), "dumb".into()));
    for (k, v) in spec.env {
        job_env.retain(|(x, _)| *x != k);
        job_env.push((k, v));
    }

    let (done, _) = tokio::sync::watch::channel(false);
    let jobs = &state.plugin_platform.jobs;
    let job = {
        let mut inner = crate::lock(&jobs.inner);
        let waiting = inner.waiting.iter().filter(|j| j.plugin == m.id).count();
        if waiting >= WAITING_MAX {
            return Err(format!(
                "{} already has {WAITING_MAX} jobs waiting; try again when one has run",
                m.name
            ));
        }
        let stdin_bytes = stdin.as_ref().map_or(0, Vec::len);
        let held: usize = inner.waiting.iter().map(|j| j.stdin_bytes).sum();
        if held + stdin_bytes > STDIN_WAITING_MAX {
            return Err(
                "too much input is waiting to run; try again when a job has started".into(),
            );
        }
        inner.seq += 1;
        let job = Arc::new(Job {
            id: format!("j-{}", &chimaera_core::generate_token()[..10]),
            plugin: m.id.clone(),
            workspace: ws.to_string(),
            program: spec.program.clone(),
            from,
            label,
            priority,
            seq: inner.seq,
            status: Mutex::new(Status {
                state: State::Queued,
                exit: None,
                timed_out: false,
                cancelled: false,
                error: None,
                queued_ms: crate::timeline::now_ms(),
                started_ms: None,
                finished_ms: None,
                started: None,
                duration_ms: 0,
            }),
            done,
            cancel: tokio::sync::Notify::new(),
            launch: Mutex::new(Some(Launch {
                program,
                args: spec.args,
                cwd,
                env: job_env,
                stdin,
                wall,
                output,
            })),
            stdin_bytes,
            group: std::sync::atomic::AtomicI32::new(0),
        });
        inner.live.insert(job.id.clone(), job.clone());
        inner.waiting.push(job.clone());
        job
    };
    tracing::info!(plugin = %m.id, workspace = ws, job = %job.id, program = %job.program, "plugin job queued");
    frame(state, &job);
    dispatch(state);
    Ok(job.id.clone())
}

/// Start what the limits allow: the best waiting job whose plugin has
/// nothing running, while fewer than `RUNNING_MAX` run.
fn dispatch(state: &Arc<AppState>) {
    loop {
        let next = {
            let mut inner = crate::lock(&state.plugin_platform.jobs.inner);
            if inner.running >= RUNNING_MAX {
                return;
            }
            let busy: Vec<String> = inner
                .live
                .values()
                .filter(|j| j.status().state == State::Running)
                .map(|j| j.plugin.clone())
                .collect();
            let best = inner
                .waiting
                .iter()
                .enumerate()
                .filter(|(_, j)| !busy.contains(&j.plugin))
                .min_by_key(|(_, j)| (j.priority, j.seq))
                .map(|(i, _)| i);
            let Some(at) = best else {
                return;
            };
            let job = inner.waiting.remove(at);
            inner.running += 1;
            {
                let mut s = crate::lock(&job.status);
                s.state = State::Running;
                s.started = Some(Instant::now());
                s.started_ms = Some(crate::timeline::now_ms());
            }
            job
        };
        // A plugin blocked, untrusted or refused since its job queued never
        // starts it.
        let held = super::manifest(state, &next.plugin)
            .is_none_or(|m| m.origin.fault.is_some() || super::trust::hold(state, &m).is_some());
        if held {
            let jobs = &state.plugin_platform.jobs;
            crate::lock(&jobs.inner).running -= 1;
            jobs.finish(
                &next,
                None,
                false,
                true,
                Some("its plugin can't run here now".into()),
            );
            frame(state, &next);
            continue;
        }
        let state = state.clone();
        tokio::spawn(async move {
            run(&state, &next).await;
            crate::lock(&state.plugin_platform.jobs.inner).running -= 1;
            dispatch(&state);
            after(&state, &next).await;
        });
    }
}

/// The limits' preamble: POSIX `sh`, fixed text, the program and its
/// arguments passed through as `"$0" "$@"` (nothing interpolated).
fn preamble(wall: Duration) -> String {
    let cpu = wall.as_secs() + 30;
    format!(
        "ulimit -v {AS_KB} 2>/dev/null; ulimit -t {cpu} 2>/dev/null; ulimit -f {FSIZE_BLOCKS} 2>/dev/null\n\
         N=''; command -v nice >/dev/null 2>&1 && N='nice -n 10'\n\
         if command -v ionice >/dev/null 2>&1; then exec ionice -c 3 -t $N \"$0\" \"$@\"; fi\n\
         exec $N \"$0\" \"$@\""
    )
}

/// Run one job to its end (or its limit, or a cancel).
async fn run(state: &Arc<AppState>, job: &Arc<Job>) {
    let jobs = &state.plugin_platform.jobs;
    let Some(launch) = crate::lock(&job.launch).take() else {
        jobs.finish(job, None, false, false, Some("nothing to run".into()));
        return;
    };
    frame(state, job);
    let logs = {
        let dir = launch.output.clone();
        let id = job.id.clone();
        tokio::task::spawn_blocking(move || {
            // The newest jobs' logs stay (a plugin reads its last build's);
            // older ones go, so a build on every save doesn't fill the disk.
            if let Err(err) = super::output::prune_oldest(&dir, Path::new(".jobs"), JOB_LOGS_KEPT) {
                tracing::debug!(%err, "old job logs not pruned");
            }
            let rel = PathBuf::from(".jobs").join(&id);
            super::output::make_dir(&dir, &rel)?;
            let out = super::output::create_file(&dir, &rel.join("stdout.log"))?;
            let err = super::output::create_file(&dir, &rel.join("stderr.log"))?;
            Ok::<_, String>((out, err))
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r)
    };
    let (out_file, err_file) = match logs {
        Ok(files) => files,
        Err(err) => {
            jobs.finish(
                job,
                None,
                false,
                false,
                Some(format!("its log files: {err}")),
            );
            return;
        }
    };
    // The activity log's line (plan §2): the program, its first arguments
    // (clipped), where it ran — then how it ended.
    let mut entry = json!({
        "kind": "job",
        "job": job.id,
        "workspace": job.workspace,
        "program": job.program,
        "args": launch.args.iter().take(ACTIVITY_ARGS).map(|a| super::runtime::clip(a, 120)).collect::<Vec<_>>(),
        "cwd": launch.cwd.to_string_lossy(),
    });
    let outcome = run_process(
        launch,
        Some(&job.cancel),
        Some(&job.group),
        out_file,
        err_file,
    )
    .await;
    tracing::info!(
        plugin = %job.plugin,
        job = %job.id,
        exit = ?outcome.exit,
        timed_out = outcome.timed_out,
        cancelled = outcome.cancelled,
        "plugin job ended"
    );
    jobs.finish(
        job,
        outcome.exit,
        outcome.timed_out,
        outcome.cancelled,
        outcome.error,
    );
    let s = job.status();
    entry["exit"] = json!(s.exit);
    entry["timed_out"] = json!(s.timed_out);
    entry["cancelled"] = json!(s.cancelled);
    entry["duration_ms"] = json!(s.duration_ms);
    super::activity::record(state, &job.plugin, entry).await;
    // What the job wrote counts against the plugin's output quota.
    super::output::usage(state, &job.plugin, false).await;
}

/// How a process ended.
struct Outcome {
    exit: Option<i32>,
    timed_out: bool,
    cancelled: bool,
    error: Option<String>,
}

/// Start `launch` under the limits, its output into the two files, and wait
/// for its end, its wall time, or `cancel`.
async fn run_process(
    launch: Launch,
    cancel: Option<&tokio::sync::Notify>,
    group_slot: Option<&std::sync::atomic::AtomicI32>,
    out_file: std::fs::File,
    err_file: std::fs::File,
) -> Outcome {
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(preamble(launch.wall))
        .arg(&launch.program)
        .args(&launch.args)
        .current_dir(&launch.cwd)
        .env_clear()
        .envs(launch.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(if launch.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return Outcome {
                exit: None,
                timed_out: false,
                cancelled: false,
                error: Some(format!("{} did not start: {err}", launch.program.display())),
            }
        }
    };
    let group = child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32));
    if let (Some(slot), Some(g)) = (group_slot, group) {
        slot.store(g.as_raw(), std::sync::atomic::Ordering::Relaxed);
    }
    if let (Some(bytes), Some(mut stdin)) = (launch.stdin, child.stdin.take()) {
        tokio::spawn(async move {
            let _ = stdin.write_all(&bytes).await;
        });
    }
    let out_task = copy_capped(child.stdout.take(), out_file);
    let err_task = copy_capped(child.stderr.take(), err_file);
    let signal = |sig: nix::sys::signal::Signal| {
        if let Some(g) = group {
            let _ = nix::sys::signal::killpg(g, sig);
        }
    };
    let never = tokio::sync::Notify::new();
    let cancel = cancel.unwrap_or(&never);
    let mut timed_out = false;
    let mut cancelled = false;
    let status = tokio::select! {
        s = child.wait() => s.ok(),
        () = tokio::time::sleep(launch.wall) => { timed_out = true; None }
        () = cancel.notified() => { cancelled = true; None }
    };
    let status = match status {
        Some(s) => Some(s),
        None => {
            signal(nix::sys::signal::Signal::SIGTERM);
            match tokio::time::timeout(TERM_GRACE, child.wait()).await {
                Ok(s) => s.ok(),
                Err(_) => {
                    signal(nix::sys::signal::Signal::SIGKILL);
                    child.wait().await.ok()
                }
            }
        }
    };
    // Whatever the program left behind in its group goes too.
    signal(nix::sys::signal::Signal::SIGKILL);
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        let _ = out_task.await;
        let _ = err_task.await;
    })
    .await;
    Outcome {
        exit: status.and_then(|s| s.code()),
        timed_out,
        cancelled,
        error: None,
    }
}

/// Copy a pipe into `file`, keeping `LOG_MAX` bytes and draining the rest
/// (a program never blocks on a full pipe).
fn copy_capped<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    pipe: Option<R>,
    file: std::fs::File,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(mut pipe) = pipe else { return };
        let mut file = tokio::fs::File::from_std(file);
        let mut buf = vec![0u8; 64 << 10];
        let mut kept: u64 = 0;
        loop {
            match pipe.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = LOG_MAX.saturating_sub(kept) as usize;
                    if room > 0 {
                        let take = n.min(room);
                        if file.write_all(&buf[..take]).await.is_err() {
                            kept = LOG_MAX;
                        } else {
                            kept += take as u64;
                        }
                    }
                }
            }
        }
        let _ = file.flush().await;
    })
}

/// A tool's setup step (`toolchain::install`): its own program, in its
/// folder, with the host's login environment and the tool first on PATH;
/// the log beside it. Not queued: the user's click waits on it.
pub(crate) async fn run_setup(
    state: &Arc<AppState>,
    m: &Manifest,
    bin: &Path,
    cwd: &Path,
    program: &str,
    args: &[String],
) -> Result<(), String> {
    let env = login_env(state, None).await?;
    let path = absolute_path(
        env.iter()
            .find(|(k, _)| k == "PATH")
            .map_or("", |(_, v)| v.as_str()),
    );
    let mut job_env: Vec<(String, String)> =
        env.iter().filter(|(k, _)| k != "PATH").cloned().collect();
    job_env.push(("PATH".into(), format!("{}:{path}", bin.display())));
    job_env.push(("TERM".into(), "dumb".into()));
    let exe = bin.join(program);
    if tokio::fs::metadata(&exe).await.is_err() {
        return Err(format!("{program} is not in the tool's folder"));
    }
    let (out, err) = {
        let cwd = cwd.to_path_buf();
        let program = program.to_string();
        tokio::task::spawn_blocking(move || {
            let open = |suffix: &str| {
                std::fs::File::options()
                    .create(true)
                    .append(true)
                    .open(cwd.join(format!(".chimaera-setup-{program}.{suffix}")))
            };
            Ok::<_, std::io::Error>((open("out")?, open("err")?))
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?
    };
    let launch = Launch {
        program: exe,
        args: args.to_vec(),
        cwd: cwd.to_path_buf(),
        env: job_env,
        stdin: None,
        wall: Duration::from_secs(WALL_MAX),
        output: cwd.to_path_buf(),
    };
    tracing::info!(plugin = %m.id, %program, "plugin tool setup step");
    let outcome = run_process(launch, None, None, out, err).await;
    match (outcome.error, outcome.exit, outcome.timed_out) {
        (Some(err), _, _) => Err(err),
        (None, _, true) => Err(format!("{program} ran past {WALL_MAX} s")),
        (None, Some(0), false) => Ok(()),
        (None, code, false) => Err(format!(
            "{program} exited {}",
            code.map_or("on a signal".to_string(), |c| c.to_string())
        )),
    }
}

/// A job ended: its frame, and the plugin's `job-finished` if it declared it
/// and is still on in the job's workspace (a job stopped by a switch-off
/// must not start the plugin's next one).
async fn after(state: &Arc<AppState>, job: &Arc<Job>) {
    frame(state, job);
    let Some(m) = super::manifest(state, &job.plugin) else {
        return;
    };
    if !m.provides.hears(EventKind::JobFinished) {
        return;
    }
    let on = crate::lock(&state.workspaces)
        .get(&job.workspace)
        .is_some_and(|w| w.plugins_on.iter().any(|p| p == &job.plugin));
    if !on {
        return;
    }
    let s = job.status();
    let end = wit::JobEnd {
        id: job.id.clone(),
        exit: s.exit,
        timed_out: s.timed_out,
        duration_ms: s.duration_ms,
    };
    state
        .plugin_runtime
        .on_event(
            state,
            &m,
            &job.workspace,
            None,
            wit::Event::JobFinished(end),
        )
        .await;
}

/// A tool that answered `wait`: hold the agent's call until the job ends
/// (at most `TOOL_WAIT`), whether it did.
pub(crate) async fn tool_wait(state: &AppState, plugin: &str, job: &str) -> bool {
    wait(state, plugin, job, TOOL_WAIT).await
}

/// `GET /workspaces/{id}/jobs/{job}`: a job's state and where its output
/// is (`output:.jobs/<id>/…` in its plugin's output folder).
pub(crate) async fn status_route(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path((ws, id)): axum::extract::Path<(String, String)>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match state.plugin_platform.jobs.get(&id) {
        Some(job) if job.workspace == ws => {
            let mut v = job.json();
            v["plugin"] = json!(job.plugin);
            axum::Json(v).into_response()
        }
        _ => super::not_found(&format!("job {id}")),
    }
}

/// `DELETE /workspaces/{id}/jobs/{job}`: the user's Stop.
pub(crate) async fn cancel_route(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path((ws, id)): axum::extract::Path<(String, String)>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let jobs = &state.plugin_platform.jobs;
    match jobs.get(&id) {
        Some(job) if job.workspace == ws => {
            cancel(&state, &id);
            axum::Json(json!({"id": id, "cancelled": true})).into_response()
        }
        _ => super::not_found(&format!("job {id}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_names_and_the_hosts_own() {
        assert!(valid_env_name("TEXINPUTS"));
        assert!(valid_env_name("max_print_line"));
        assert!(!valid_env_name("1X"));
        assert!(!valid_env_name("A-B"));
        for owned in [
            "PATH",
            "HOME",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "CHIMAERA_X",
            "path",
            "Ld_Preload",
        ] {
            assert!(reserved(owned), "{owned}");
        }
        assert!(!reserved("TEXINPUTS"));
        assert!(!reserved("max_print_line"));
    }

    #[test]
    fn a_relative_path_entry_is_never_searched() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-onpath-{}-{}",
            std::process::id(),
            &chimaera_core::generate_token()[..8]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("prog");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let abs = dir.to_string_lossy().into_owned();
        assert_eq!(on_path("prog", &abs), Some(exe));
        // The same folder named relatively (never chdir in a test: they
        // share the process) is not searched.
        let cwd = std::env::current_dir().unwrap();
        let up = cwd.components().count();
        let relative = format!("{}{}", "../".repeat(up), abs.trim_start_matches('/'));
        assert!(Path::new(&cwd).join(&relative).join("prog").is_file());
        assert_eq!(on_path("prog", &format!(".:{relative}")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_jobs_path_keeps_only_absolute_entries() {
        assert_eq!(
            absolute_path(".:/opt/tex/bin::bin:/usr/bin:"),
            "/opt/tex/bin:/usr/bin"
        );
        assert_eq!(absolute_path(".:"), "/usr/local/bin:/usr/bin:/bin");
    }

    #[test]
    fn the_preamble_never_interpolates_the_program() {
        let p = preamble(Duration::from_secs(60));
        assert!(p.contains("ulimit -t 90"));
        assert!(p.contains(r#""$0" "$@""#));
        assert!(!p.contains("{"));
    }
}
