//! Ask the agents themselves: what plugins, skills and hooks does each agent
//! CLI have on this host? Chimaera never re-derives an agent's discovery
//! rules — it asks the agent (plan §6: "always from the agent").
//!
//! - claude: `claude plugin list --json` (a stable, documented JSON) and
//!   `claude plugin details <id>` (human text — only the totals are read,
//!   and an unparseable line is simply omitted).
//! - codex: a SHORT-LIVED `codex app-server` over stdio JSON-RPC
//!   (`skills/list`, `hooks/list`, `config/batchWrite`) — no model call, no
//!   thread. Wire facts live-probed 2026-09-25 on codex 0.153 (PROTOCOL.md).
//!
//! Login-node discipline: every child is login-shell wrapped (the same env
//! the TUI sees), time-boxed, output-capped, `kill_on_drop`; ONE probe runs
//! daemon-wide at a time (a semaphore — three windows on the Plugins tab
//! must not spawn three app-servers); answers are cached for a minute.
//!
//! Probes run under the user's Environment prelude, like sessions do — on
//! HPC hosts `module load` is how claude and node reach PATH at all. The
//! host-wide claude answer gets the host scope only; codex's per-directory
//! answer (and its trust write) gets host ⊕ workspace. All `plugin details`
//! calls share ONE login shell: the profile plus a prelude can take seconds
//! per shell on a busy login node, and the gate is held throughout.

use std::collections::{BTreeMap, HashMap};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::agents::AgentKind;
use crate::AppState;

pub(crate) mod actions;
pub(crate) mod connections;
mod extensions;

const CACHE_TTL: Duration = Duration::from_secs(60);
const CLI_TIMEOUT: Duration = Duration::from_secs(20);
const CLI_OUTPUT_CAP: usize = 4 * 1024 * 1024;
const RPC_TIMEOUT: Duration = Duration::from_secs(15);
/// One JSON-RPC response line (skills lists can be large, never unbounded).
const RPC_LINE_CAP: usize = 8 * 1024 * 1024;
/// Plugins whose `details` we ask claude for.
const DETAILS_MAX: usize = 12;
/// The whole `details` batch; sections finished by then still count.
const DETAILS_TIMEOUT: Duration = Duration::from_secs(45);
/// Every `claude plugin details` in one shell. Ids and the binary arrive
/// as positional arguments, never as shell source. Each marker starts its
/// own line (the leading newline) and carries the probe's random nonce, so
/// no text a plugin ships can open, close or re-attribute a section.
const DETAILS_SCRIPT: &str = r#"nonce=$1 bin=$2
shift 2
i=0
for id in "$@"; do
    printf '\n\036%s start %d\n' "$nonce" "$i"
    "$bin" plugin details "$id"
    status=$?
    printf '\n\036%s end %d %d\n' "$nonce" "$i" "$status"
    i=$((i + 1))
done
"#;
/// SKILL.md frontmatter read budget.
const SKILL_HEAD: u64 = 8 * 1024;
/// Skills listed per scanned directory.
const SKILLS_PER_DIR: usize = 200;
/// A plugin's hooks.json read for the "what it runs" line.
const HOOKS_JSON_MAX: u64 = 256 * 1024;

#[derive(Default)]
pub struct ProbeState {
    cache: Mutex<HashMap<String, (Instant, Value)>>,
    generation: AtomicU64,
    changed_epoch: AtomicU64,
    connection_auth: connections::AuthState,
}

/// Daemon-wide: at most one agent CLI probe in flight.
static GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

impl ProbeState {
    fn get(&self, key: &str) -> Option<Value> {
        crate::lock(&self.cache)
            .get(key)
            .filter(|(at, _)| at.elapsed() < CACHE_TTL)
            .map(|(_, v)| v.clone())
    }

    fn put(&self, key: &str, value: Value, generation: u64) {
        let mut cache = crate::lock(&self.cache);
        // An install may finish while a CLI probe is still in flight. Its
        // old result can answer that caller, but must not refill the cache.
        if self.generation.load(Ordering::Relaxed) != generation {
            return;
        }
        if cache.len() > 64 {
            cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
        }
        cache.insert(key.to_string(), (Instant::now(), value));
    }

    /// Forget everything (an install or trust write just changed the truth).
    pub(crate) fn invalidate(&self) {
        let mut cache = crate::lock(&self.cache);
        self.generation.fetch_add(1, Ordering::Relaxed);
        cache.clear();
    }

    /// A write changed agent plugins/hooks. Explicit refreshes invalidate
    /// only the cache: broadcasting those would cause a refetch loop.
    pub(crate) fn changed(&self) {
        self.invalidate();
        self.changed_epoch.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn changed_epoch(&self) -> u64 {
        self.changed_epoch.load(Ordering::Relaxed)
    }
}

/// One probe's Environment prelude file. It lives exactly as long as the
/// probe; removal is best effort and never on the reactor.
struct ProbePrelude(Option<PathBuf>);

impl ProbePrelude {
    /// `workspace` None = the host scope alone. A failed write is "no
    /// prelude", never a failed probe — the same degrade sessions get.
    async fn write(state: &Arc<AppState>, workspace: Option<&str>) -> Self {
        let state = Arc::clone(state);
        let workspace = workspace.map(str::to_string);
        let path = tokio::task::spawn_blocking(move || {
            crate::environment::materialize_probe_prelude(&state, workspace.as_deref())
        })
        .await
        .ok()
        .flatten();
        ProbePrelude(path)
    }

    fn path(&self) -> Option<&Path> {
        self.0.as_deref()
    }
}

impl Drop for ProbePrelude {
    fn drop(&mut self) {
        let Some(path) = self.0.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn_blocking(move || crate::environment::remove_prelude_path(&path));
            }
            Err(_) => crate::environment::remove_prelude_path(&path),
        }
    }
}

fn wrapped(bin: &Path, args: &[&str]) -> Vec<String> {
    let mut argv = vec![bin.to_string_lossy().into_owned()];
    argv.extend(args.iter().map(|a| a.to_string()));
    crate::launcher::wrap_login_shell(&crate::launcher::login_shell(), argv)
}

fn base_command(
    argv: &[String],
    cwd: Option<&Path>,
    prelude: Option<&Path>,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Every session spawn's env hygiene (`api::spawn_env_remove`): the
    // daemon's own launcher markers (a daemon started inside Claude Code)
    // must not make the probe a nested child session, and an inherited
    // prelude pair (a daemon started in a chimaera terminal) must neither
    // suppress this probe's prelude nor run another session's.
    let env: Vec<(String, String)> = prelude
        .map(|path| ("CHIMAERA_PRELUDE".to_string(), path.display().to_string()))
        .into_iter()
        .collect();
    for name in crate::api::spawn_env_remove(&env) {
        cmd.env_remove(name);
    }
    cmd.envs(env);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd
}

/// Run a CLI to completion: stdout capped (over the cap is an error, never a
/// silently truncated JSON), stderr tail kept for the error message. The CLI
/// runs in its own process group, killed whole once the login shell exits or
/// the probe is abandoned (see `process::probe_output`).
async fn run_bounded(
    argv: &[String],
    cwd: Option<&Path>,
    prelude: Option<&Path>,
) -> Result<String, String> {
    let mut child = crate::process::Child::spawn(&mut base_command(argv, cwd, prelude))
        .map_err(|_| format!("could not start {}", argv[0]))?;
    let mut stdout = child.take_stdout().ok_or("no stdout")?;
    let mut stderr = child.take_stderr().ok_or("no stderr")?;
    let work = async {
        // Both pipes at once: a CLI that fills stderr before closing stdout
        // would otherwise block on its write while we wait for stdout's EOF.
        // Past the cap stderr is drained unkept, so it can never block.
        let mut out = Vec::new();
        let mut err = Vec::new();
        let read_out = async {
            (&mut stdout)
                .take(CLI_OUTPUT_CAP as u64 + 1)
                .read_to_end(&mut out)
                .await
                .map_err(|e| e.to_string())
        };
        let read_err = async {
            let _ = (&mut stderr).take(8 * 1024).read_to_end(&mut err).await;
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        };
        // Waiting alongside the reads: the leader's exit kills its group,
        // so a helper holding a pipe open can't stall the reads to EOF.
        let (read, (), status) = tokio::join!(read_out, read_err, child.wait());
        read?;
        let status = status.map_err(|e| e.to_string())?;
        Ok::<_, String>((out, err, status))
    };
    let (out, err, status) = tokio::time::timeout(CLI_TIMEOUT, work)
        .await
        .map_err(|_| "timed out".to_string())??;
    if out.len() > CLI_OUTPUT_CAP {
        return Err("output over the size cap".to_string());
    }
    if !status.success() {
        let tail = String::from_utf8_lossy(&err);
        return Err(crate::timeline::cap(tail.trim(), 300));
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Kills a batch's whole process group when dropped: the shell AND the
/// call it has in flight, which `kill_on_drop` (the shell alone) misses — a
/// `claude` hung on a stalled NFS read would outlive the probe on a shared
/// login node. Dropped before the shell is reaped, so the group id can't
/// have been reused.
struct GroupKill(Option<nix::unistd::Pid>);

impl Drop for GroupKill {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL);
        }
    }
}

/// Run a batch under one overall deadline and return the stdout that
/// arrived before it (or before the cap) — partial by design: the caller
/// trusts only sections whose end marker made it in. Once reading stops
/// (the shell would block on a full pipe past the cap), or if the probe is
/// abandoned, the batch's process group is killed.
async fn run_until(argv: &[String], prelude: Option<&Path>, limit: Duration) -> String {
    let mut cmd = base_command(argv, None, prelude);
    // Each call's failure is in its end marker's status; nothing reads stderr.
    cmd.stderr(Stdio::null());
    // Its own group: the login shell execs into the batch shell, whose
    // children are the calls (see `GroupKill`).
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            tracing::debug!(%err, "agent probe batch did not start");
            return String::new();
        }
    };
    // Declared after `child`, so on an abandoned probe it drops first.
    let group = GroupKill(child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32)));
    let Some(mut stdout) = child.stdout.take() else {
        return String::new();
    };
    let deadline = tokio::time::Instant::now() + limit;
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    while out.len() < CLI_OUTPUT_CAP {
        // `read` is cancel-safe: the deadline never loses bytes already read.
        match tokio::time::timeout_at(deadline, stdout.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => {
                let keep = n.min(CLI_OUTPUT_CAP - out.len());
                out.extend_from_slice(&chunk[..keep]);
            }
            // EOF, a read error, or the deadline.
            _ => break,
        }
    }
    drop(group);
    let _ = child.start_kill();
    let _ = child.wait().await;
    String::from_utf8_lossy(&out).into_owned()
}

/// A short-lived codex app-server: initialize once, then requests.
struct CodexRpc {
    // Drop the process group before reaping the leader (PID reuse).
    group: GroupKill,
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

impl CodexRpc {
    async fn open(bin: &Path, cwd: &Path, prelude: Option<&Path>) -> Result<Self, String> {
        let argv = wrapped(bin, &["app-server"]);
        let mut cmd = base_command(&argv, Some(cwd), prelude);
        cmd.stdin(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("could not start codex app-server: {e}"))?;
        let group = GroupKill(child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32)));
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut rpc = CodexRpc {
            group,
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
        };
        rpc.request(
            "initialize",
            json!({
                "clientInfo": {"name": "chimaera", "title": "Chimaera",
                    "version": chimaera_core::VERSION},
                "capabilities": {"experimentalApi": true},
            }),
        )
        .await?;
        rpc.send(&json!({"method": "initialized"})).await?;
        Ok(rpc)
    }

    async fn send(&mut self, msg: &Value) -> Result<(), String> {
        let mut line = msg.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("codex app-server write failed: {e}"))
    }

    /// One request → its result (an `error` frame is an Err). Notifications
    /// and non-JSON lines (a chatty login profile) are skipped.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"id": id, "method": method, "params": params}))
            .await?;
        let read = async {
            for _ in 0..1024 {
                let Some(line) = read_rpc_line(&mut self.stdout)
                    .await
                    .map_err(|e| format!("codex app-server read failed: {e}"))?
                else {
                    return Err("codex app-server closed".to_string());
                };
                let Ok(msg) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                if msg.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(err) = msg.get("error") {
                    return Err(format!(
                        "codex {method}: {}",
                        err.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("error")
                    ));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            Err("too many codex notifications".to_string())
        };
        tokio::time::timeout(RPC_TIMEOUT, read)
            .await
            .map_err(|_| format!("codex {method} timed out"))?
    }

    async fn close(mut self) {
        drop(self.group);
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

/// Bound the allocation before reading an agent-controlled JSON-RPC frame.
async fn read_rpc_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let n = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map_or(chunk.len(), |i| i + 1);
        if line.len() + n > RPC_LINE_CAP {
            return Err(std::io::Error::other("codex answer over the size cap"));
        }
        let end = chunk[n - 1] == b'\n';
        line.extend_from_slice(&chunk[..n]);
        reader.consume(n);
        if end {
            return Ok(Some(line));
        }
    }
}

async fn bin_of(
    state: &AppState,
    kind: AgentKind,
) -> Result<(PathBuf, Option<String>, crate::runtime_retention::Usage), String> {
    let det = crate::launcher::detect(state, kind, false).await;
    match det.path {
        Ok(path) => {
            let (usage, mut bins) =
                crate::runtime_retention::acquire(state, Some(kind), vec![path])
                    .await
                    .map_err(|error| error.to_string())?;
            Ok((bins.remove(0), det.version, usage))
        }
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------- claude

/// `claude plugin details` totals: "Skills (10)", "Hooks (3)",
/// "Always-on:   ~1,086 tok". Human text — anything unparseable is None.
fn parse_claude_details(text: &str) -> (Option<u64>, Option<u64>, Option<u64>) {
    let count_after = |label: &str| {
        text.lines().find_map(|l| {
            let l = l.trim_start();
            let rest = l.strip_prefix(label)?.trim_start().strip_prefix('(')?;
            rest.split(')').next()?.trim().parse::<u64>().ok()
        })
    };
    let tokens = text.lines().find_map(|l| {
        let rest = l.trim_start().strip_prefix("Always-on:")?;
        let digits: String = rest
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit() || *c == ',')
            .filter(char::is_ascii_digit)
            .collect();
        digits.parse::<u64>().ok()
    });
    (count_after("Skills"), count_after("Hooks"), tokens)
}

/// [`DETAILS_SCRIPT`]'s output → each asked id's text, in ask order. Only
/// sections that carry this probe's nonce on both markers and ended with
/// status 0 count; anything else (the login profile's chatter, a forged
/// marker, a section cut off by the deadline) is None or plain text.
fn split_details(out: &str, nonce: &str, count: usize) -> Vec<Option<String>> {
    let start = format!("\u{1e}{nonce} start ");
    let end = format!("\u{1e}{nonce} end ");
    let mut sections = vec![None; count];
    let mut open: Option<(usize, String)> = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix(&start) {
            open = rest
                .parse::<usize>()
                .ok()
                .filter(|&i| i < count)
                .map(|i| (i, String::new()));
        } else if let Some(rest) = line.strip_prefix(&end) {
            let mut fields = rest.split(' ');
            let index = fields.next().and_then(|f| f.parse::<usize>().ok());
            let clean = fields.next() == Some("0") && fields.next().is_none();
            if let Some((i, text)) = open.take() {
                if index == Some(i) && clean {
                    sections[i] = Some(text);
                }
            }
        } else if let Some((_, text)) = open.as_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    sections
}

/// `claude plugin details` for every id (in order; None = no clean answer),
/// in ONE login shell under one deadline.
async fn claude_details(bin: &Path, ids: &[&str], prelude: Option<&Path>) -> Vec<Option<String>> {
    if ids.is_empty() {
        return Vec::new();
    }
    let nonce = chimaera_core::generate_token();
    let mut argv = vec![
        "/bin/bash".to_string(),
        "-c".to_string(),
        DETAILS_SCRIPT.to_string(),
        "chimaera-probe".to_string(),
        nonce.clone(),
        bin.to_string_lossy().into_owned(),
    ];
    argv.extend(ids.iter().map(|id| id.to_string()));
    let argv = crate::launcher::wrap_login_shell(&crate::launcher::login_shell(), argv);
    let out = run_until(&argv, prelude, DETAILS_TIMEOUT).await;
    split_details(&out, &nonce, ids.len())
}

async fn claude_state(state: &Arc<AppState>) -> Value {
    if let Some(hit) = state.probes.get("claude") {
        return hit;
    }
    let (bin, version, _usage) = match bin_of(state, AgentKind::Claude).await {
        Ok(found) => found,
        Err(err) => return json!({"agent": "claude", "available": false, "error": err}),
    };
    let _permit = GATE.acquire().await;
    // Whoever held the gate may have just filled the cache (two views asking
    // at once): don't rerun a dozen login-shell CLI calls for the same answer.
    if let Some(hit) = state.probes.get("claude") {
        return hit;
    }
    let generation = state.probes.generation.load(Ordering::Relaxed);
    // Host scope only: this one answer serves every workspace.
    let prelude = ProbePrelude::write(state, None).await;
    let listed = run_bounded(
        &wrapped(&bin, &["plugin", "list", "--json"]),
        None,
        prelude.path(),
    )
    .await;
    let plugins: Vec<Value> = match listed.and_then(|out| {
        serde_json::from_str::<Vec<Value>>(out.trim())
            .map_err(|e| format!("unexpected output: {e}"))
    }) {
        Ok(list) => list,
        Err(err) => {
            let v = json!({"agent": "claude", "available": true, "version": version,
                "plugins": [], "error": err});
            return v;
        }
    };
    let mut rows = Vec::new();
    // (row index, id) of every plugin whose details are asked for.
    let mut asked: Vec<(usize, String)> = Vec::new();
    for (i, p) in plugins.iter().enumerate() {
        let id = p
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        if i < DETAILS_MAX && crate::launcher::safe_arg(&id.replace(['@', '/'], "-")) {
            asked.push((rows.len(), id.clone()));
        }
        rows.push(json!({
            "id": id,
            "version": p.get("version"),
            "scope": p.get("scope"),
            "enabled": p.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            "install_path": p.get("installPath"),
        }));
    }
    let ids: Vec<&str> = asked.iter().map(|(_, id)| id.as_str()).collect();
    let details = claude_details(&bin, &ids, prelude.path()).await;
    for ((row, _), text) in asked.iter().zip(details) {
        let Some(text) = text else {
            continue;
        };
        let (skills, hooks, tokens) = parse_claude_details(&text);
        rows[*row]["skills_n"] = json!(skills);
        rows[*row]["hooks_n"] = json!(hooks);
        rows[*row]["always_on_tokens"] = json!(tokens);
    }
    let value = json!({"agent": "claude", "available": true, "version": version, "plugins": rows});
    state.probes.put("claude", value.clone(), generation);
    value
}

// ---------------------------------------------------------------- codex

/// The raw `skills/list` + `hooks/list` answers for one workspace's cwd.
async fn codex_raw(state: &Arc<AppState>, workspace_id: &str, root: &Path) -> Value {
    let key = format!("codex:{}", root.display());
    if let Some(hit) = state.probes.get(&key) {
        return hit;
    }
    let (bin, version, _usage) = match bin_of(state, AgentKind::Codex).await {
        Ok(found) => found,
        Err(err) => return json!({"available": false, "error": err}),
    };
    let _permit = GATE.acquire().await;
    if let Some(hit) = state.probes.get(&key) {
        return hit;
    }
    let generation = state.probes.generation.load(Ordering::Relaxed);
    // Host ⊕ workspace, as a codex session in this workspace would see.
    let prelude = ProbePrelude::write(state, Some(workspace_id)).await;
    let result = async {
        let mut rpc = CodexRpc::open(&bin, root, prelude.path()).await?;
        let cwd = root.to_string_lossy().into_owned();
        let skills = rpc
            .request("skills/list", json!({"cwds": [cwd], "forceReload": false}))
            .await;
        let hooks = rpc.request("hooks/list", json!({"cwds": [cwd]})).await;
        rpc.close().await;
        Ok::<_, String>((skills, hooks))
    }
    .await;
    let value = match result {
        Ok((skills, hooks)) => {
            let first = |v: &Result<Value, String>| {
                v.as_ref()
                    .ok()
                    .and_then(|r| r.get("data"))
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            let mut errors: Vec<String> = Vec::new();
            if let Err(e) = &skills {
                errors.push(e.clone());
            }
            if let Err(e) = &hooks {
                errors.push(e.clone());
            }
            json!({"available": true, "version": version,
                "skills": first(&skills), "hooks": first(&hooks), "errors": errors})
        }
        Err(err) => json!({"available": true, "version": version, "error": err}),
    };
    state.probes.put(&key, value.clone(), generation);
    value
}

/// snake_case hook event (from a codex hook key) → the hooks.json section
/// name (PascalCase).
fn pascal(snake: &str) -> String {
    snake
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// "What it runs", in a line: the script (+ its first argument) a hook
/// command invokes, found in the plugin's own hooks.json by the key's
/// `<event>:<group>:<hook>` suffix. Best-effort — None hides the line.
fn hook_command(source_path: &str, key: &str) -> Option<String> {
    let mut parts = key.rsplitn(4, ':');
    let hook_idx: usize = parts.next()?.parse().ok()?;
    let group_idx: usize = parts.next()?.parse().ok()?;
    let event = pascal(parts.next()?);
    let path = Path::new(source_path);
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > HOOKS_JSON_MAX {
        return None;
    }
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let command = doc
        .get("hooks")?
        .get(&event)?
        .get(group_idx)?
        .get("hooks")?
        .get(hook_idx)?
        .get("command")?
        .as_str()?;
    Some(command_summary(command))
}

fn command_summary(command: &str) -> String {
    let words: Vec<&str> = command
        .split(|c: char| c.is_whitespace() || c == ';')
        .map(|w| w.trim_matches(['"', '\'']))
        .filter(|w| !w.is_empty())
        .collect();
    let is_script = |w: &str| w.ends_with(".sh") || w.ends_with(".py");
    let base = |w: &'_ str| w.rsplit('/').next().unwrap_or(w).to_string();
    if let Some(i) = words.iter().position(|w| is_script(w)) {
        return match words.get(i + 1) {
            // A dispatcher handing off to the real handler: name the handler.
            Some(next) if is_script(next) => base(next),
            Some(arg) if arg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => {
                format!("{} {arg}", base(words[i]))
            }
            _ => base(words[i]),
        };
    }
    crate::timeline::cap(command, 80)
}

fn codex_hooks(raw: &Value) -> Vec<Value> {
    raw.get("hooks")
        .and_then(|h| h.get("hooks"))
        .and_then(Value::as_array)
        .map(|hooks| {
            hooks
                .iter()
                .map(|h| {
                    let key = h.get("key").and_then(Value::as_str).unwrap_or("");
                    let source = h.get("sourcePath").and_then(Value::as_str).unwrap_or("");
                    // Event names in the hooks.json vocabulary (codex reports
                    // camelCase — `postToolUse`; the section is `PostToolUse`),
                    // so both agents' hooks read the same.
                    let event = h.get("eventName").and_then(Value::as_str).map(|e| {
                        let mut c = e.chars();
                        c.next()
                            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                            .unwrap_or_default()
                    });
                    let mut row = json!({
                        "key": key,
                        "event": event,
                        "plugin_id": h.get("pluginId"),
                        "source": h.get("source"),
                        "trust": h.get("trustStatus"),
                        "hash": h.get("currentHash"),
                        "command": hook_command(source, key),
                    });
                    // No matcher = fires always; absent, never a JSON null.
                    if let Some(m) = h.get("matcher").and_then(Value::as_str) {
                        row["matcher"] = json!(m);
                    }
                    row
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn codex_state(state: &Arc<AppState>, workspace_id: &str, root: &Path) -> Value {
    let raw = codex_raw(state, workspace_id, root).await;
    if raw.get("available") == Some(&json!(false)) {
        return json!({"agent": "codex", "available": false, "error": raw.get("error")});
    }
    let hooks = {
        let raw = raw.clone();
        tokio::task::spawn_blocking(move || codex_hooks(&raw))
            .await
            .unwrap_or_default()
    };
    // Codex plugins, as codex itself attributes skills and hooks to them.
    let mut plugins: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    if let Some(skills) = raw
        .get("skills")
        .and_then(|s| s.get("skills"))
        .and_then(Value::as_array)
    {
        for skill in skills {
            if let Some(pid) = skill.get("pluginId").and_then(Value::as_str) {
                plugins.entry(pid.to_string()).or_default().0 += 1;
            }
        }
    }
    for hook in &hooks {
        if let Some(pid) = hook.get("plugin_id").and_then(Value::as_str) {
            plugins.entry(pid.to_string()).or_default().1 += 1;
        }
    }
    let plugins: Vec<Value> = plugins
        .into_iter()
        .map(|(id, (skills, hooks))| {
            json!({"id": id, "enabled": true, "skills_n": skills, "hooks_n": hooks})
        })
        .collect();
    json!({
        "agent": "codex",
        "available": true,
        "version": raw.get("version"),
        "plugins": plugins,
        "hooks": hooks,
        "error": raw.get("error"),
    })
}

// ---------------------------------------------------------------- routes

fn workspace_root(state: &AppState, id: &str) -> Option<PathBuf> {
    crate::lock(&state.workspaces).get(id).map(|w| w.root)
}

fn not_found() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::NOT_FOUND,
        axum::Json(json!({"error": "unknown workspace"})),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
pub(crate) struct ProbeQuery {
    #[serde(default)]
    refresh: bool,
}

async fn refresh_probes(state: &AppState) {
    state.probes.invalidate();
    // A transient version timeout must not disable inventory until a daemon
    // restart. Explicit retries reuse the resolved binary, preserving the
    // configured installation and avoiding expensive PATH probes for others.
    let missing: Vec<_> = crate::lock(&state.agent_bins)
        .iter()
        .filter(|(_, detection)| detection.version.is_none())
        .filter_map(|(kind, detection)| {
            detection
                .path
                .as_ref()
                .ok()
                .map(|path| (*kind, path.clone()))
        })
        .collect();
    for (kind, path) in missing {
        let version = crate::launcher::probe_version(&path).await;
        if let Some(current) = crate::lock(&state.agent_bins).get_mut(&kind) {
            if current.path.as_ref().ok() == Some(&path) && current.version.is_none() {
                current.version = version;
            }
        }
    }
}

/// GET /workspaces/{id}/agent-plugins — each agent's plugins (and codex's
/// hooks with their trust state), asked of the agents themselves.
pub(crate) async fn agent_plugins(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<ProbeQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(root) = workspace_root(&state, &id) else {
        return not_found();
    };
    if query.refresh {
        refresh_probes(&state).await;
    }
    let (claude, codex, agy, grok) = tokio::join!(
        claude_state(&state),
        codex_state(&state, &id, &root),
        extensions::inventory(&state, &id, &root, AgentKind::Antigravity),
        extensions::inventory(&state, &id, &root, AgentKind::Grok),
    );
    axum::Json(json!({
        "schema": 1,
        "host": state.hostname,
        "agents": [claude, codex, agy, grok],
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
pub(crate) struct TrustBody {
    hooks: Vec<TrustHook>,
}

#[derive(serde::Deserialize)]
pub(crate) struct TrustHook {
    key: String,
    hash: String,
}

/// POST /workspaces/{id}/plugins/{pid}/trust-hooks {hooks:[{key,hash}]} —
/// the user's click, written as codex's own trust record. Re-lists first and
/// writes ONLY hooks that belong to a codex plugin this plugin names (required
/// or recommended), are untrusted or modified, and still hash to exactly what
/// the user was shown.
pub(crate) async fn trust_hooks(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path((id, pid)): axum::extract::Path<(String, String)>,
    axum::Json(body): axum::Json<TrustBody>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let Some(root) = workspace_root(&state, &id) else {
        return not_found();
    };
    let codex_plugins: Vec<String> = crate::plugins::manifest(&state, &pid)
        .map(|m| {
            m.agent_plugin_ids("codex")
                .into_iter()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if codex_plugins.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "this plugin has no codex hooks to trust"})),
        )
            .into_response();
    }
    let (bin, _, _usage) = match bin_of(&state, AgentKind::Codex).await {
        Ok(found) => found,
        Err(err) => {
            return (StatusCode::CONFLICT, axum::Json(json!({"error": err}))).into_response()
        }
    };
    let _permit = GATE.acquire().await;
    let prelude = ProbePrelude::write(&state, Some(&id)).await;
    let result = async {
        let mut rpc = CodexRpc::open(&bin, &root, prelude.path()).await?;
        let cwd = root.to_string_lossy().into_owned();
        let listed = rpc.request("hooks/list", json!({"cwds": [cwd]})).await?;
        let current: Vec<Value> = listed
            .get("data")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|e| e.get("hooks"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut trusted = Vec::new();
        let mut skipped = Vec::new();
        let mut state_table = serde_json::Map::new();
        for want in &body.hooks {
            let Some(hook) = current
                .iter()
                .find(|h| h.get("key").and_then(Value::as_str) == Some(want.key.as_str()))
            else {
                skipped.push(json!({"key": want.key, "reason": "no longer listed"}));
                continue;
            };
            let belongs = hook
                .get("pluginId")
                .and_then(Value::as_str)
                .is_some_and(|id| codex_plugins.iter().any(|p| p == id));
            let status = hook
                .get("trustStatus")
                .and_then(Value::as_str)
                .unwrap_or("");
            let hash = hook
                .get("currentHash")
                .and_then(Value::as_str)
                .unwrap_or("");
            let reason = if !belongs {
                Some("not this plugin's hook")
            } else if !matches!(status, "untrusted" | "modified") {
                Some("already trusted or managed")
            } else if hash != want.hash {
                Some("changed since you reviewed it")
            } else {
                None
            };
            match reason {
                Some(reason) => skipped.push(json!({"key": want.key, "reason": reason})),
                None => {
                    state_table.insert(want.key.clone(), json!({"trusted_hash": hash}));
                    trusted.push(want.key.clone());
                }
            }
        }
        if !state_table.is_empty() {
            // `upsert` on the table MERGES (live-verified: other trust
            // records survive) — never `replace`, which would untrust
            // everything else.
            rpc.request(
                "config/batchWrite",
                json!({
                    "edits": [{"keyPath": "hooks.state", "mergeStrategy": "upsert",
                        "value": Value::Object(state_table)}],
                    "reloadUserConfig": true,
                }),
            )
            .await?;
        }
        rpc.close().await;
        Ok::<_, String>((trusted, skipped))
    }
    .await;
    drop(_permit);
    state.probes.changed();
    state.changes.notify_waiters();
    match result {
        Ok((trusted, skipped)) => {
            tracing::info!(workspace = %id, plugin = %pid, trusted = trusted.len(),
                "codex hooks trusted by the user");
            axum::Json(json!({"trusted": trusted, "skipped": skipped})).into_response()
        }
        Err(err) => (StatusCode::BAD_GATEWAY, axum::Json(json!({"error": err}))).into_response(),
    }
}

// ---------------------------------------------------------------- skills

/// `name:` / `description:` from a SKILL.md frontmatter (hand-parsed; the
/// single-line and quoted forms, plus a `>`/`|` folded description).
fn skill_frontmatter(text: &str) -> Option<(String, String)> {
    let body = text.strip_prefix("---")?;
    let end = body.find("\n---")?;
    let mut name = None;
    let mut description = String::new();
    let mut folding = false;
    for line in body[..end].lines() {
        if folding {
            if line.starts_with(' ') || line.starts_with('\t') {
                if !description.is_empty() {
                    description.push(' ');
                }
                description.push_str(line.trim());
                continue;
            }
            folding = false;
        }
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(unquote(v));
        } else if let Some(v) = line.strip_prefix("description:") {
            let v = v.trim();
            if v == ">" || v == "|" || v == ">-" || v == "|-" {
                folding = true;
            } else {
                description = unquote(v);
            }
        }
    }
    Some((name?, crate::timeline::cap(&description, 400)))
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(v)
        .replace("\\\"", "\"")
}

/// Every `<dir>/*/SKILL.md` (bounded reads).
fn scan_skills(dir: &Path) -> Vec<(String, String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten().take(SKILLS_PER_DIR) {
        let path = entry.path().join("SKILL.md");
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut head = String::new();
        if std::io::Read::take(file, SKILL_HEAD)
            .read_to_string(&mut head)
            .is_err()
        {
            continue;
        }
        if let Some((name, description)) = skill_frontmatter(&head) {
            out.push((name, description, path));
        }
    }
    out
}

#[derive(Default)]
struct SkillRow {
    description: String,
    source: &'static str,
    plugin: Option<String>,
    claude: Option<(String, Option<String>, Option<PathBuf>)>,
    codex: Option<(String, Option<String>, Option<PathBuf>)>,
}

/// GET /workspaces/{id}/skills — every skill each agent can use here.
pub(crate) async fn skills(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<ProbeQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(root) = workspace_root(&state, &id) else {
        return not_found();
    };
    if query.refresh {
        refresh_probes(&state).await;
    }
    let (claude, codex, agy, grok) = tokio::join!(
        claude_state(&state),
        codex_raw(&state, &id, &root),
        extensions::inventory(&state, &id, &root, AgentKind::Antigravity),
        extensions::inventory(&state, &id, &root, AgentKind::Grok),
    );
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let plugin_dirs: Vec<(String, PathBuf)> = claude
        .get("plugins")
        .and_then(Value::as_array)
        .map(|ps| {
            ps.iter()
                .filter(|p| p.get("enabled").and_then(Value::as_bool).unwrap_or(false))
                .filter_map(|p| {
                    let id = p.get("id")?.as_str()?;
                    let name = id.split('@').next()?.to_string();
                    Some((name, PathBuf::from(p.get("install_path")?.as_str()?)))
                })
                .collect()
        })
        .unwrap_or_default();
    let scan_root = root.clone();
    let scanned = tokio::task::spawn_blocking(move || {
        let mut found: Vec<(&'static str, Option<String>, String, String, PathBuf)> = Vec::new();
        for (n, d, p) in scan_skills(&scan_root.join(".claude/skills")) {
            found.push(("project", None, n, d, p));
        }
        if let Some(home) = &home {
            for (n, d, p) in scan_skills(&home.join(".claude/skills")) {
                found.push(("user", None, n, d, p));
            }
        }
        for (plugin, dir) in &plugin_dirs {
            for (n, d, p) in scan_skills(&dir.join("skills")) {
                found.push((
                    "plugin",
                    Some(plugin.clone()),
                    format!("{plugin}:{n}"),
                    d,
                    p,
                ));
            }
        }
        found
    })
    .await
    .unwrap_or_default();

    let mut rows: BTreeMap<(String, Option<PathBuf>), SkillRow> = BTreeMap::new();
    for (source, plugin, name, description, path) in scanned {
        let row = rows.entry((name.clone(), Some(path.clone()))).or_default();
        row.description = description;
        row.source = source;
        row.plugin = plugin;
        row.claude = Some(("available".into(), None, Some(path)));
    }
    // Built into claude: the catalog a live claude chat session reported at
    // its handshake (only source for skills with no file).
    let live_catalog = crate::chat::claude_catalog_in_workspace(&state, &id);
    let live = live_catalog.is_some();
    // `_`-prefixed entries are the CLI's internal commands, not skills.
    for (name, description) in live_catalog
        .unwrap_or_default()
        .into_iter()
        .filter(|(name, _)| !name.starts_with('_'))
    {
        let row = rows.entry((name.clone(), None)).or_default();
        if row.claude.is_none() {
            row.claude = Some(("available".into(), None, None));
            row.source = "builtin";
            row.description = description;
        }
    }
    let mut errors: Vec<Value> = Vec::new();
    if let Some(entry) = codex.get("skills").filter(|v| !v.is_null()) {
        for skill in entry
            .get("skills")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = skill.get("name").and_then(Value::as_str) else {
                continue;
            };
            let enabled = skill
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let scope = skill.get("scope").and_then(Value::as_str).unwrap_or("");
            let plugin_id = skill.get("pluginId").and_then(Value::as_str);
            let path = skill.get("path").and_then(Value::as_str).map(PathBuf::from);
            let row = rows.entry((name.to_string(), path)).or_default();
            if row.source.is_empty() {
                row.source = match (scope, plugin_id) {
                    (_, Some(_)) => "plugin",
                    ("repo", _) => "project",
                    ("user", _) => "user",
                    _ => "system",
                };
                row.plugin = plugin_id.map(|p| p.split('@').next().unwrap_or(p).to_string());
                row.description = crate::timeline::cap(
                    skill
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    400,
                );
            }
            row.codex = Some(if enabled {
                (
                    "available".into(),
                    None,
                    skill.get("path").and_then(Value::as_str).map(PathBuf::from),
                )
            } else {
                (
                    "off".into(),
                    Some("disabled in codex's config".into()),
                    skill.get("path").and_then(Value::as_str).map(PathBuf::from),
                )
            });
        }
        for err in entry
            .get("errors")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            errors.push(json!({"agent": "codex", "path": err.get("path"),
                "message": err.get("message")}));
        }
    }
    let claude_available = claude.get("available") == Some(&json!(true));
    let codex_available = codex.get("available") == Some(&json!(true));
    let mut skills: Vec<Value> = rows
        .into_iter()
        .map(|((name, _), row)| {
            let side = |slot: &Option<(String, Option<String>, Option<PathBuf>)>,
                        prefix: char,
                        available: bool,
                        agent: &str| {
                match slot {
                    Some((state, reason, _)) => json!({
                        "state": state,
                        "reason": reason,
                        "invoke": format!("{prefix}{name}"),
                    }),
                    None => json!({
                        "state": "absent",
                        "reason": if !available {
                            Some(format!("{agent} is not installed here"))
                        } else if row.source == "plugin" {
                            Some(format!("the plugin is not installed for {agent}"))
                        } else {
                            None
                        },
                    }),
                }
            };
            json!({
                "name": name,
                "description": row.description,
                "source": if row.source.is_empty() { "system" } else { row.source },
                "plugin": row.plugin,
                "paths": {
                    "claude": row.claude.as_ref().and_then(|c| c.2.clone()),
                    "codex": row.codex.as_ref().and_then(|c| c.2.clone()),
                },
                "agents": {
                    "claude": side(&row.claude, '/', claude_available, "claude"),
                    "codex": side(&row.codex, '$', codex_available, "codex"),
                },
            })
        })
        .collect();
    for inventory in [&agy, &grok] {
        extensions::append_skills(&mut skills, inventory);
        for key in ["error", "skills_error"] {
            if let Some(message) = inventory[key].as_str() {
                errors.push(json!({"agent":inventory["agent"],"message":message}));
            }
        }
    }
    for skill in &mut skills {
        use sha2::Digest;
        // A refreshed inventory can insert/remove earlier rows. Never transfer
        // an expanded row to another skill just because its position changed.
        let identity = json!({"name":skill["name"],"paths":skill["paths"]}).to_string();
        let digest = sha2::Sha256::digest(identity.as_bytes());
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        skill["id"] = json!(format!("skill-{hex}"));
    }
    axum::Json(json!({
        "schema": 1,
        "host": state.hostname,
        "agents": {
            "claude": {"available": claude_available, "version": claude.get("version"), "live": live},
            "codex": {"available": codex_available, "version": codex.get("version")},
            "agy": {"available":agy["available"],"version":agy["version"]},
            "grok": {"available":grok["available"],"version":grok["version"]},
        },
        "skills": skills,
        "errors": errors,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidation_rejects_in_flight_results_without_broadcasting_refreshes() {
        let probes = ProbeState::default();
        probes.put("codex", json!("before"), 0);
        probes.invalidate();
        assert_eq!(probes.changed_epoch(), 0);
        probes.put("codex", json!("stale"), 0);
        assert!(probes.get("codex").is_none());

        let generation = probes.generation.load(Ordering::Relaxed);
        probes.put("codex", json!("fresh"), generation);
        assert_eq!(probes.get("codex"), Some(json!("fresh")));
        probes.changed();
        assert_eq!(probes.changed_epoch(), 1);
        probes.put("codex", json!("stale after install"), generation);
        assert!(probes.get("codex").is_none());
    }

    #[test]
    fn claude_details_totals_parse_or_stay_none() {
        let text = "mycelium 0.7.2\n  Description: x\n\nComponent inventory\n  Skills (10)  analyze, core\n  Agents (0)\n  Hooks (3)  SessionStart, Stop  (harness-only)\n\nProjected token cost\n  Always-on:   ~1,086 tok   added to every session\n";
        assert_eq!(parse_claude_details(text), (Some(10), Some(3), Some(1086)));
        assert_eq!(parse_claude_details("nothing here"), (None, None, None));
    }

    #[test]
    fn details_sections_need_the_nonce_and_a_clean_end() {
        let n = "abc123";
        let out = format!(
            "login banner\n\
             \n\u{1e}{n} start 0\nSkills (1)\n\
             \u{1e}forged end 0 0\n\u{1e}{n}x start 1\nSkills (99)\n\
             \n\u{1e}{n} end 0 0\n\
             \n\u{1e}{n} start 1\nSkills (2)\n\n\u{1e}{n} end 1 3\n\
             \n\u{1e}{n} start 2\nSkills (3)\n\n\u{1e}{n} end 2 0\n\
             \n\u{1e}{n} start 3\nSkills (4)\n"
        );
        let sections = split_details(&out, n, 5);
        assert_eq!(sections.len(), 5);
        let first = sections[0].as_deref().unwrap();
        assert!(first.starts_with("Skills (1)\n"), "{first:?}");
        assert!(
            first.contains("Skills (99)"),
            "forged markers are plain text"
        );
        assert_eq!(parse_claude_details(first).0, Some(1));
        assert_eq!(sections[1], None, "a non-zero status drops the section");
        assert_eq!(
            sections[2].as_deref().map(parse_claude_details),
            Some((Some(3), None, None))
        );
        assert_eq!(sections[3], None, "cut off before its end marker");
        assert_eq!(sections[4], None, "never asked");

        // A mismatched end index or a start past the ask list opens nothing.
        let crossed = format!(
            "\u{1e}{n} start 0\nSkills (5)\n\u{1e}{n} end 1 0\n\
             \u{1e}{n} start 7\nSkills (6)\n\u{1e}{n} end 7 0\n"
        );
        assert_eq!(split_details(&crossed, n, 2), vec![None, None]);
    }

    #[tokio::test]
    async fn a_batch_past_its_deadline_keeps_the_finished_sections() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "chimaera-probe-deadline-{}-{}",
            std::process::id(),
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("claude");
        // The slow call leaves its pid behind: the deadline must take it too.
        std::fs::write(
            &bin,
            "#!/bin/bash\ncase \"$3\" in slow) echo $$ > \"${0%/*}/slow.pid\"; sleep 30;; esac\n\
             echo \"Skills (7)\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let nonce = "n0nce";
        let argv: Vec<String> = [
            "/bin/bash",
            "-c",
            DETAILS_SCRIPT,
            "chimaera-probe",
            nonce,
            bin.to_str().unwrap(),
            "fast",
            "slow",
        ]
        .map(str::to_string)
        .to_vec();
        let started = Instant::now();
        let out = run_until(&argv, None, Duration::from_millis(1500)).await;
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the deadline held"
        );
        let sections = split_details(&out, nonce, 2);
        assert_eq!(
            sections[0].as_deref().map(parse_claude_details),
            Some((Some(7), None, None))
        );
        assert_eq!(sections[1], None);
        let slow: i32 = std::fs::read_to_string(dir.join("slow.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let gone = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(slow), None).is_err()
        });
        assert!(gone, "the call in flight at the deadline was killed too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hook_commands_summarize_to_the_script_and_its_verb() {
        assert_eq!(
            command_summary(
                r#"if [ -n "${PLUGIN_ROOT:-}" ]; then "$PLUGIN_ROOT/hooks/mycelium-codex-dispatch.sh" health; fi"#
            ),
            "mycelium-codex-dispatch.sh health"
        );
        assert_eq!(
            command_summary(
                r#"if [ -n "${PLUGIN_ROOT:-}" ]; then "${PLUGIN_ROOT}/hooks/mycelium-codex-dispatch.sh" mycelium-stop-check.sh; fi"#
            ),
            "mycelium-stop-check.sh",
            "a dispatcher names its handler"
        );
        assert_eq!(command_summary("echo hi"), "echo hi");
        assert_eq!(pascal("post_tool_use"), "PostToolUse");
    }

    #[test]
    fn skill_frontmatter_handles_quotes_and_folding() {
        let md = "---\nname: develop\ndescription: \"Run it \\\"locally\\\"\"\n---\nbody";
        assert_eq!(
            skill_frontmatter(md),
            Some(("develop".into(), "Run it \"locally\"".into()))
        );
        let folded = "---\nname: x\ndescription: >\n  one\n  two\n---\n";
        assert_eq!(
            skill_frontmatter(folded),
            Some(("x".into(), "one two".into()))
        );
        assert_eq!(skill_frontmatter("no frontmatter"), None);
    }
}
