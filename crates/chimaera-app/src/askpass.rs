//! In-app SSH auth prompts. From a windowed app ssh has no tty to ask for a
//! password or a Duo passcode, so we point `SSH_ASKPASS` at ourselves: when
//! ssh (bringing up the ControlMaster — see `chimaera-remote`) needs input, it
//! runs this binary in `--askpass` mode with the prompt; that helper hands the
//! prompt to the running app over a unix socket, the app shows a modal, and
//! the typed answer flows back to ssh on stdout. Because every ssh call to a
//! host multiplexes one ControlMaster, the user is asked once per host, not
//! once per command. Windows' OpenSSH has no ControlMaster, so there a
//! password or 2FA host asks once per ssh call.
//!
//! Transport: a unix socket in a private dir (unix); a token-gated loopback
//! TCP listener (Windows), where ssh runs this executable itself as
//! `SSH_ASKPASS` — no shell shim exists there to add `--askpass`, so the
//! relay's env var plus a single argument marks the helper role
//! ([`is_helper_invocation`]).
//!
//! Keyboard-interactive prompts (Duo's "Passcode or option (1-3):") reach
//! askpass too, so the same modal — prompt text over a single input — covers
//! both password and 2FA. Host-key confirmation is not an askpass prompt and
//! is out of scope here (chimaera-remote's ssh options decide whether an
//! unknown host's key is recorded; a changed key always stops the connect).
//! Each child also frames its normalized host alias with the prompt, so the
//! native relay can target only that host's windows (plus local home).
//!
//! Cancel means cancel. OpenSSH reads an askpass that fails as an empty answer
//! and asks again (three times per method, then the next method), so a cancel
//! cannot be conveyed by what the helper prints or how it exits. Instead the
//! helper exits [`CANCELLED_EXIT`] and its shim ends the ssh that asked; the
//! "authentication cancelled" line it leaves on stderr tells the connect flow
//! not to carry on into a fresh ssh (see `ASKPASS_CANCELLED`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::Shutdown;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::net::UnixStream as StdUnixStream;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

/// Env var carrying the askpass socket path to the helper (and to ssh, which
/// passes its environment through to the askpass program it runs).
#[cfg(unix)]
const SOCK_ENV: &str = "CHIMAERA_ASKPASS_SOCK";

/// Env var carrying the loopback relay's `"<port> <token>"` to the helper
/// (Windows). It also marks a single-argument run of this executable as the
/// helper, so only ssh — which inherits it from the app — ever sees it.
#[cfg(windows)]
const RELAY_ENV: &str = "CHIMAERA_ASKPASS_RELAY";

/// Every variable `install` (or a connect, for the alias) puts in this
/// process's environment for its OWN ssh children. The local daemon must not
/// inherit them: its terminals and agents would then prompt through the app's
/// modal (hanging with no Home open, and letting any agent raise a native
/// prompt labelled with a real host) — see `daemon::spawn_detached`.
#[cfg(unix)]
pub(crate) const APP_ASKPASS_ENV: &[&str] = &[
    "SSH_ASKPASS",
    "SSH_ASKPASS_REQUIRE",
    "CHIMAERA_ASKPASS_SOCK",
    chimaera_remote::ASKPASS_ALIAS_ENV,
];
#[cfg(windows)]
pub(crate) const APP_ASKPASS_ENV: &[&str] = &[
    "SSH_ASKPASS",
    "SSH_ASKPASS_REQUIRE",
    RELAY_ENV,
    chimaera_remote::ASKPASS_ALIAS_ENV,
];
pub(crate) const SCOPE_FRAME: &str = "chimaera-askpass-scope-v1";

/// Exit status of `--askpass` for a prompt that got no answer (cancelled,
/// timed out, or its window gone). The shim ends the asking ssh on it.
pub(crate) const CANCELLED_EXIT: i32 = 75;

/// The tail of the unix askpass shim: run right after the helper with its
/// status in `$?`. A cancel ends the process that exec'd the shim — ssh,
/// whose `$PPID` this is — because the only other thing a cancelled ssh does
/// is ask again. (Windows has no shim: the helper ends its ssh itself.)
#[cfg(unix)]
fn cancel_epilogue() -> String {
    format!(
        "rc=$?\n\
         [ \"$rc\" -eq {CANCELLED_EXIT} ] && kill -TERM \"$PPID\"\n\
         exit \"$rc\"\n"
    )
}

/// What the relay answers a prompt that got no answer (cancelled, timed out,
/// its window gone). A secret always ends in a newline and this never does, so
/// the two cannot be confused — and an empty or cut-off reply, a relay that
/// refused or died, stays [`Reply::Unanswered`]: ssh's old "no answer" rather
/// than the user's cancel.
const CANCEL_REPLY: &str = "cancelled";

/// What the helper makes of the relay's reply.
#[derive(Debug, PartialEq)]
enum Reply {
    Secret(String),
    Cancelled,
    Unanswered,
}

/// What the relay writes back: the secret and ONE newline — an empty secret
/// (Enter on a prompt) is still an answer — or [`CANCEL_REPLY`].
fn encode_reply(answer: Option<&str>) -> Vec<u8> {
    match answer {
        Some(secret) => format!("{secret}\n"),
        None => CANCEL_REPLY.to_string(),
    }
    .into_bytes()
}

/// A reply cut short before its newline is never a secret: a half-read
/// password is never handed to ssh.
fn decode_reply(reply: &str) -> Reply {
    if let Some(secret) = reply.strip_suffix('\n') {
        Reply::Secret(secret.to_string())
    } else if reply == CANCEL_REPLY {
        Reply::Cancelled
    } else {
        Reply::Unanswered
    }
}

/// The helper's cancel: leave the note the connect flow reads on stderr
/// (ssh's stderr is captured) and exit with the status the shim acts on. On
/// Windows there is no shim, so the helper ends the asking ssh itself.
fn cancelled() -> ! {
    eprintln!("{}", chimaera_remote::ASKPASS_CANCELLED);
    #[cfg(windows)]
    end_asking_ssh();
    std::process::exit(CANCELLED_EXIT)
}

/// Whether this process is the askpass helper: `--askpass` (the unix shim's
/// form), or on Windows the relay variable plus exactly one argument — how
/// ssh runs `SSH_ASKPASS` (`<program> <prompt>`). A relaunch of the app
/// (`restart` after an update) inherits the variable but never passes a lone
/// argument.
pub fn is_helper_invocation() -> bool {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.iter().any(|a| a == "--askpass") {
        return true;
    }
    #[cfg(windows)]
    if args.len() == 1 && std::env::var_os(RELAY_ENV).is_some() {
        return true;
    }
    false
}

/// The prompt ssh handed the helper: the argument after `--askpass`, else
/// the only argument (Windows, where ssh runs this executable directly).
fn helper_prompt(args: &[String]) -> String {
    match args.iter().position(|a| a == "--askpass") {
        Some(i) => args.get(i + 1).cloned().unwrap_or_default(),
        None => args.first().cloned().unwrap_or_default(),
    }
}

/// End the ssh that ran this helper (its parent, checked by image name so a
/// recycled pid is never hit). Best effort: if the parent is not ssh, the
/// cancel degrades to ssh's own "no answer" — it asks again.
#[cfg(windows)]
fn end_asking_ssh() {
    if let Some(pid) = parent_process()
        .filter(|(_, image)| image.eq_ignore_ascii_case("ssh.exe"))
        .map(|(pid, _)| pid)
    {
        chimaera_core::proc::kill_pid(pid);
    }
}

/// This process's parent: its pid and image name, from one process snapshot.
#[cfg(windows)]
fn parent_process() -> Option<(u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let me = std::process::id();
    // SAFETY: a snapshot handle checked against INVALID_HANDLE_VALUE, walked
    // with a correctly sized entry, and closed exactly once.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entries: Vec<(u32, u32, String)> = Vec::new();
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            entries.push((
                entry.th32ProcessID,
                entry.th32ParentProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..len]),
            ));
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        let parent = entries.iter().find(|(pid, _, _)| *pid == me)?.1;
        entries
            .into_iter()
            .find(|(pid, _, _)| *pid == parent)
            .map(|(pid, _, image)| (pid, image))
    }
}

/// Held by the tests that write a script and exec it. A fork in one test
/// thread while another still has its fresh script open for writing makes
/// that exec fail with ETXTBSY on Linux.
#[cfg(all(test, unix))]
pub(crate) static EXEC_LOCK: Mutex<()> = Mutex::new(());

/// How long a prompt waits for the UI before giving up. A dropped window or
/// an ignored modal must not pin an ssh process open forever — on timeout we
/// return no answer, which the helper turns into ending that ssh (a cancel).
const PROMPT_TIMEOUT: Duration = Duration::from_secs(180);

/// Prompts awaiting a UI answer, keyed by a per-request id. Managed as Tauri
/// state so the socket task and the `answer_askpass` command share it.
///
/// The prompt TEXT is kept alongside the answer channel because the
/// `ssh-askpass` emit is fire-and-forget: a window that mounts after the
/// emit (startup window restore kicks off remote connects before any webview
/// has loaded) would otherwise never learn a prompt exists, and ssh would
/// silently wait out the timeout — the "host stuck connecting with no
/// prompt" failure. Eligible windows fetch `pending_scoped()` on mount to
/// close that gap without exposing another host's challenge.
#[derive(Default)]
pub struct Askpass {
    pending: Mutex<HashMap<u64, PendingPrompt>>,
    seq: AtomicU64,
}

struct PendingPrompt {
    alias: Option<String>,
    prompt: String,
    tx: oneshot::Sender<Option<String>>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum AnswerResult {
    Answered(Option<String>),
    Missing,
    Forbidden,
}

/// `ssh-askpass` event payload: a prompt the UI must answer.
#[derive(Clone, Serialize)]
pub struct PromptEvent {
    id: u64,
    alias: Option<String>,
    prompt: String,
}

impl PromptEvent {
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }
}

impl Askpass {
    fn register(
        &self,
        alias: Option<String>,
        prompt: String,
        tx: oneshot::Sender<Option<String>>,
    ) -> u64 {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        lock(&self.pending).insert(id, PendingPrompt { alias, prompt, tx });
        id
    }

    fn discard(&self, id: u64) {
        lock(&self.pending).remove(&id);
    }

    /// Drop every prompt for `alias` (its connect was cancelled: the ssh that
    /// asked is gone). Dropping the answer channel ends each prompt's wait at
    /// once, which discards it and tells its windows it is done.
    pub(crate) fn discard_alias(&self, alias: &str) -> usize {
        let mut pending = lock(&self.pending);
        let before = pending.len();
        pending.retain(|_, prompt| prompt.alias.as_deref() != Some(alias));
        before - pending.len()
    }

    /// Resolve a prompt only when the shell-owned caller scope is allowed to
    /// see its alias. Authorization and removal share one lock, so a caller
    /// cannot race a scope check against another answer.
    pub fn answer_scoped(
        &self,
        id: u64,
        secret: Option<String>,
        window_scope: &crate::shell::WindowScope,
    ) -> AnswerResult {
        let mut pending = lock(&self.pending);
        let Some(prompt) = pending.get(&id) else {
            return AnswerResult::Missing;
        };
        if !window_scope.allows_askpass(prompt.alias.as_deref()) {
            return AnswerResult::Forbidden;
        }
        let prompt = pending.remove(&id).expect("prompt checked above");
        let alias = prompt.alias.clone();
        let _ = prompt.tx.send(secret);
        AnswerResult::Answered(alias)
    }

    /// Prompts this shell-owned caller scope may observe, oldest first (ssh
    /// asks sequentially, so order is answer order).
    pub fn pending_scoped(&self, window_scope: &crate::shell::WindowScope) -> Vec<PromptEvent> {
        let mut prompts: Vec<PromptEvent> = lock(&self.pending)
            .iter()
            .filter(|(_, prompt)| window_scope.allows_askpass(prompt.alias.as_deref()))
            .map(|(id, p)| PromptEvent {
                id: *id,
                alias: p.alias.clone(),
                prompt: p.prompt.clone(),
            })
            .collect();
        prompts.sort_by_key(|p| p.id);
        prompts
    }
}

/// Deliver an askpass event only to shell-registered windows allowed to
/// observe this host. Tauri event subscriptions are directly available to
/// daemon pages, and even `emit_to` reaches every page's catch-all listener,
/// so only per-window delivery (`shell::deliver_to`) is an auth boundary.
fn emit_scoped<T: Serialize>(app: &AppHandle, event: &str, payload: T, alias: Option<&str>) {
    let Some(shell) = app.try_state::<crate::shell::Shell>() else {
        return;
    };
    for label in shell.askpass_targets(alias) {
        if let Err(error) = crate::shell::deliver_to(app, &label, event, &payload) {
            tracing::warn!("askpass: could not deliver {event} to {label}: {error}");
        }
    }
}

pub(crate) fn emit_done(app: &AppHandle, id: u64, alias: Option<&str>) {
    emit_scoped(app, "ssh-askpass-done", id, alias);
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The leaf name of the askpass relay socket.
#[cfg(unix)]
const SOCK_LEAF: &str = "askpass.sock";

/// The askpass socket DIRECTORY for a given runtime dir: the runtime dir
/// itself normally, or a short `/tmp/chimaera-<home-hash>/run` when the full
/// socket path would overshoot the ~104-byte unix-socket (`sun_path`) limit —
/// a `CHIMAERA_HOME` deep in a worktree pushes `<home>/run/askpass.sock` past
/// it and the bind fails, so ssh auth dies with no prompt. The twin of
/// `control_dir` in chimaera-remote (same cap, same fallback shape, still
/// distinct per home so a dev app's relay never collides with the real
/// app's); keep the two in step. Pure so the length invariant is testable.
#[cfg(unix)]
fn socket_dir(runtime_dir: &Path) -> PathBuf {
    /// Headroom under the ~104-byte `sun_path` cap.
    const SUN_PATH_SAFE: usize = 100;

    if runtime_dir.as_os_str().len() + 1 + SOCK_LEAF.len() <= SUN_PATH_SAFE {
        return runtime_dir.to_path_buf();
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    runtime_dir.hash(&mut h);
    // Keep the `/run` tail so the socket shape (`…/run/askpass.sock`) is
    // stable per home.
    PathBuf::from(format!("/tmp/chimaera-{:08x}", h.finish() as u32)).join("run")
}

#[cfg(unix)]
fn socket_path() -> Result<PathBuf> {
    let runtime = chimaera_core::runtime_dir();
    let dir = socket_dir(&runtime);
    if dir == runtime {
        // The runtime dir itself: best-effort like the core dir resolvers,
        // the bind reports the real failure.
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        if let Err(e) = builder.create(&dir) {
            tracing::warn!("failed to create askpass socket dir {}: {e}", dir.display());
        }
    } else {
        ensure_private_fallback(&dir)?;
    }
    Ok(dir.join(SOCK_LEAF))
}

/// The `/tmp` fallback's name is predictable, so another local user could
/// pre-create it (or its parent) and later swap our socket for a relay that
/// reads every typed secret. Both levels must be real directories this user
/// owns with no group/other access; otherwise askpass stays off.
#[cfg(unix)]
fn ensure_private_fallback(dir: &Path) -> Result<()> {
    if let Some(parent) = dir.parent() {
        chimaera_core::ensure_private_dir(parent)
            .with_context(|| format!("askpass socket dir {}", parent.display()))?;
    }
    chimaera_core::ensure_private_dir(dir)
        .with_context(|| format!("askpass socket dir {}", dir.display()))
}

/// `SSH_ASKPASS` / `SSH_ASKPASS_REQUIRE` as they were before `install`, for
/// the local daemon's environment. Never the app's own shim (an app started
/// from a terminal whose daemon inherited it from an older app).
static USER_ASKPASS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();

/// `shim` is what the app sets `SSH_ASKPASS` to: the unix shim script, or
/// on Windows this executable itself.
fn remember_user_askpass(shim: &std::path::Path) {
    let current: Vec<(String, String)> = ["SSH_ASKPASS", "SSH_ASKPASS_REQUIRE"]
        .iter()
        .filter_map(|key| Some((key.to_string(), std::env::var(key).ok()?)))
        .collect();
    let _ = USER_ASKPASS.set(user_askpass(current, shim));
}

/// The user's askpass settings among `current`: none at all when
/// `SSH_ASKPASS` is unset or names the app's shim.
fn user_askpass(current: Vec<(String, String)>, shim: &std::path::Path) -> Vec<(String, String)> {
    let theirs = current
        .iter()
        .any(|(k, v)| k == "SSH_ASKPASS" && !v.is_empty() && std::path::Path::new(v) != shim);
    if theirs {
        current
    } else {
        Vec::new()
    }
}

/// What `remember_user_askpass` kept (empty before `install` or when none).
pub(crate) fn user_askpass_env() -> &'static [(String, String)] {
    USER_ASKPASS.get().map_or(&[], Vec::as_slice)
}

#[cfg(unix)]
fn shim_path() -> PathBuf {
    chimaera_core::runtime_dir().join("askpass.sh")
}

/// ssh runs `$SSH_ASKPASS "<prompt>"` with exactly one arg, so a tiny shim
/// re-invokes us in --askpass mode (`$@` forwards that single prompt arg) and
/// then acts on the helper's status (see [`cancel_epilogue`]). No `exec`: the
/// shim has to outlive the helper to end ssh after it.
#[cfg(unix)]
fn shim_script(exe: &Path) -> String {
    format!("#!/bin/sh\n{exe:?} --askpass \"$@\"\n{}", cancel_epilogue())
}

/// Wire ssh/scp spawned from this process (and their ControlMaster children)
/// to prompt through the app: write the askpass shim, export the ssh env, and
/// start the socket listener. Called once at startup, before any connect.
#[cfg(unix)]
pub fn install(app: &AppHandle) -> Result<()> {
    let sock = socket_path()?;
    // A stale socket from a previous run would refuse the bind.
    std::fs::remove_file(&sock).ok();

    let exe = std::env::current_exe().context("resolve current executable")?;
    let shim = shim_path();
    std::fs::write(&shim, shim_script(&exe))
        .with_context(|| format!("write {}", shim.display()))?;
    let mut perms = std::fs::metadata(&shim)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(&shim, perms)?;

    // The user's own askpass, if any, before ours replaces it: the daemon's
    // sessions get it back (`daemon::spawn_detached`).
    remember_user_askpass(&shim);
    // ssh children inherit these. REQUIRE=force makes ssh use askpass even
    // when a tty exists — the default only kicks in with no tty, and a GUI
    // app's environment is murky enough that we don't want to depend on it.
    std::env::set_var("SSH_ASKPASS", &shim);
    std::env::set_var("SSH_ASKPASS_REQUIRE", "force");
    std::env::set_var(SOCK_ENV, &sock);

    let std_listener = std::os::unix::net::UnixListener::bind(&sock)
        .with_context(|| format!("bind {}", sock.display()))?;
    std_listener.set_nonblocking(true)?;

    let app = app.clone();
    // `UnixListener::from_std` registers with the Tokio reactor, so it must
    // run inside the runtime — spawn first, convert there. `install` itself is
    // called from Tauri's synchronous setup, which has no reactor.
    tauri::async_runtime::spawn(async move {
        let listener = match UnixListener::from_std(std_listener) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("askpass listener unavailable: {e}");
                return;
            }
        };
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move { serve_one(app, stream).await });
                }
                Err(e) => {
                    tracing::warn!("askpass listener stopped: {e}");
                    break;
                }
            }
        }
    });
    tracing::info!("askpass ready on {}", sock.display());
    Ok(())
}

/// A version marker and per-child host context precede the arbitrary ssh
/// prompt. The marker matters during rolling updates: an older helper can send
/// a multi-line Duo prompt, and treating its first line as an alias would show
/// it in no remote window. Unknown/old framing remains deliberately unscoped.
fn split_prompt_request(input: String) -> (Option<String>, String) {
    let Some(scoped) = input
        .strip_prefix(SCOPE_FRAME)
        .and_then(|rest| rest.strip_prefix('\n'))
    else {
        return (None, input);
    };
    let Some((alias, prompt)) = scoped.split_once('\n') else {
        return (None, input);
    };
    (
        (!alias.is_empty()).then(|| alias.to_string()),
        prompt.to_string(),
    )
}

/// Windows: a token-gated loopback TCP listener, and this executable as
/// `SSH_ASKPASS` (Win32-OpenSSH runs it with the prompt as its one argument).
/// The token exists because loopback TCP lacks the unix socket's private-dir
/// confidentiality: without it any local process could present a fake prompt
/// and be handed a typed password. It reaches the helper through ssh's
/// inherited environment, never argv.
#[cfg(windows)]
pub fn install(app: &AppHandle) -> Result<()> {
    let token = chimaera_core::generate_token();
    let std_listener =
        std::net::TcpListener::bind(("127.0.0.1", 0)).context("bind askpass relay")?;
    std_listener.set_nonblocking(true)?;
    let port = std_listener.local_addr()?.port();

    let exe = std::env::current_exe().context("resolve current executable")?;
    // The user's own askpass, if any, before ours replaces it: the daemon's
    // sessions get it back (`daemon::spawn_detached`).
    remember_user_askpass(&exe);
    // REQUIRE=force: ssh would otherwise try the console first, and the app's
    // ssh children have none.
    std::env::set_var("SSH_ASKPASS", &exe);
    std::env::set_var("SSH_ASKPASS_REQUIRE", "force");
    std::env::set_var(RELAY_ENV, format!("{port} {token}"));

    let app = app.clone();
    // Same reactor rule as the unix listener: convert inside the runtime.
    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("askpass relay unavailable: {e}");
                return;
            }
        };
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let app = app.clone();
                    let token = token.clone();
                    tauri::async_runtime::spawn(async move {
                        serve_one_tcp(app, stream, token).await;
                    });
                }
                Err(e) => {
                    tracing::warn!("askpass relay stopped: {e}");
                    break;
                }
            }
        }
    });
    tracing::info!("askpass relay on 127.0.0.1:{port}");
    Ok(())
}

/// One relay request: first line is the auth token, the rest (to the
/// client's half-close) is the scoped prompt; reply as [`encode_reply`].
/// Unlike the unix socket, any local process can connect to this port, so
/// the token guards secrecy and the request bounds guard availability.
#[cfg(windows)]
async fn serve_one_tcp(app: AppHandle, mut stream: tokio::net::TcpStream, token: String) {
    let Some(input) = read_request(&mut stream, MAX_PROMPT_REQUEST, PROMPT_READ_TIMEOUT).await
    else {
        return;
    };
    let Some((client_token, request)) = input.split_once('\n') else {
        tracing::warn!("askpass: malformed relay request");
        return;
    };
    if client_token.trim() != token {
        tracing::warn!("askpass: relay request with a bad token refused");
        return;
    }
    let (alias, prompt) = split_prompt_request(request.to_string());
    let answer = resolve_prompt(&app, alias, prompt).await;
    let _ = stream.write_all(&encode_reply(answer.as_deref())).await;
    let _ = stream.shutdown().await;
}

/// A generous ceiling for any real ssh prompt request, and how long a client
/// may take to send it. Any same-user process can connect to the socket;
/// without bounds one could stream without end into a String or pin a task.
/// (The Windows token line fits well inside the same ceiling.)
const MAX_PROMPT_REQUEST: u64 = 64 * 1024;
const PROMPT_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Read one whole request (to the client's half-close), or `None` when it is
/// larger than `max` bytes, not UTF-8, or not finished within `timeout`.
async fn read_request<R: tokio::io::AsyncRead + Unpin>(
    stream: &mut R,
    max: u64,
    timeout: Duration,
) -> Option<String> {
    let mut request = String::new();
    // One byte past the cap tells "exactly max" from "too long".
    let read =
        tokio::time::timeout(timeout, stream.take(max + 1).read_to_string(&mut request)).await;
    match read {
        Ok(Ok(_)) if request.len() as u64 <= max => Some(request),
        Ok(Ok(_)) => {
            tracing::warn!("askpass: prompt request over {max} bytes refused");
            None
        }
        Ok(Err(e)) => {
            tracing::warn!("askpass: could not read prompt: {e}");
            None
        }
        Err(_) => {
            tracing::warn!("askpass: prompt request timed out");
            None
        }
    }
}

/// Serve one askpass request: read the prompt (the helper half-closes its
/// write side to mark the end), ask the UI, write the answer back.
#[cfg(unix)]
async fn serve_one(app: AppHandle, mut stream: UnixStream) {
    let Some(request) = read_request(&mut stream, MAX_PROMPT_REQUEST, PROMPT_READ_TIMEOUT).await
    else {
        return;
    };
    let (alias, prompt) = split_prompt_request(request);
    let answer = resolve_prompt(&app, alias, prompt).await;
    let _ = stream.write_all(&encode_reply(answer.as_deref())).await;
    let _ = stream.shutdown().await;
}

/// Register the prompt, ask the UI, wait out the timeout — the transport-
/// agnostic middle both the unix socket and the Windows TCP relay feed.
/// `None` = no answer was given.
async fn resolve_prompt(app: &AppHandle, alias: Option<String>, prompt: String) -> Option<String> {
    let state = app.state::<Askpass>();
    let (tx, rx) = oneshot::channel();
    let prompt = prompt.trim_end().to_string();
    let id = state.register(alias.clone(), prompt.clone(), tx);
    let event = PromptEvent { id, alias, prompt };
    // Emit only to matching windows that are ALREADY listening. Windows that
    // mount later find this prompt through the equally scoped list command;
    // zero targets at emit time is fine during startup restore.
    emit_scoped(app, "ssh-askpass", event.clone(), event.alias.as_deref());
    match tokio::time::timeout(PROMPT_TIMEOUT, rx).await {
        Ok(Ok(Some(secret))) => Some(secret),
        // Cancelled, timed out, or the app dropped the sender: no answer, so
        // the helper ends the waiting ssh rather than leave it hanging (or
        // asking again). Windows still showing the prompt must drop it —
        // there is no one left to receive an answer.
        _ => {
            state.discard(id);
            emit_done(app, id, event.alias.as_deref());
            None
        }
    }
}

/// `chimaera-app --askpass <prompt>`: the helper ssh execs. Relays the prompt
/// to the running app and prints the answer for ssh to read. A prompt that
/// gets no answer — cancelled or timed out — exits [`CANCELLED_EXIT`] for the
/// shim to end ssh; a missing socket or app yields an empty answer (ssh then
/// fails cleanly, never hangs).
#[cfg(unix)]
pub fn run_helper() {
    let prompt = helper_prompt(&std::env::args().skip(1).collect::<Vec<_>>());
    let Some(sock) = std::env::var_os(SOCK_ENV) else {
        return;
    };
    let alias = std::env::var(chimaera_remote::ASKPASS_ALIAS_ENV)
        .ok()
        .filter(|alias| !alias.is_empty());
    match ask(&sock, alias.as_deref(), &prompt) {
        Ok(Reply::Secret(secret)) => print!("{secret}"),
        Ok(Reply::Cancelled) => cancelled(),
        Ok(Reply::Unanswered) | Err(_) => {}
    }
    std::io::stdout().flush().ok();
}

/// Windows: the same helper over the loopback relay named by [`RELAY_ENV`].
/// Any failure prints nothing: ssh gets an empty answer and fails cleanly,
/// never hangs. A prompt that got no answer ends the asking ssh
/// ([`cancelled`]).
#[cfg(windows)]
pub fn run_helper() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let prompt = helper_prompt(&args);
    let Some(relay) = std::env::var(RELAY_ENV).ok() else {
        return;
    };
    let Some((port, token)) = relay
        .split_once(' ')
        .and_then(|(port, token)| Some((port.parse::<u16>().ok()?, token)))
    else {
        return;
    };
    let alias = std::env::var(chimaera_remote::ASKPASS_ALIAS_ENV)
        .ok()
        .filter(|alias| !alias.is_empty());
    match ask_tcp(port, token, alias.as_deref(), &prompt) {
        Ok(Reply::Secret(secret)) => print!("{secret}"),
        Ok(Reply::Cancelled) => cancelled(),
        Ok(Reply::Unanswered) | Err(_) => {}
    }
    std::io::stdout().flush().ok();
}

/// One loopback relay round trip: the token line, then the same framing as
/// the unix socket's.
#[cfg(windows)]
fn ask_tcp(port: u16, token: &str, alias: Option<&str>, prompt: &str) -> Result<Reply> {
    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).context("connect askpass relay")?;
    stream.write_all(token.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.write_all(&scoped_request(alias, prompt))?;
    stream.shutdown(Shutdown::Write)?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(decode_reply(&reply))
}

/// The scope frame, the alias and the prompt, one per line (the prompt may
/// span several).
fn scoped_request(alias: Option<&str>, prompt: &str) -> Vec<u8> {
    format!("{SCOPE_FRAME}\n{}\n{prompt}", alias.unwrap_or_default()).into_bytes()
}

/// One relay round trip: the prompt out, the relay's reply back.
#[cfg(unix)]
fn ask(sock: &std::ffi::OsStr, alias: Option<&str>, prompt: &str) -> Result<Reply> {
    let mut stream = StdUnixStream::connect(sock).context("connect askpass socket")?;
    stream.write_all(&scoped_request(alias, prompt))?;
    // Half-close so the server's read-to-EOF returns the whole prompt.
    stream.shutdown(Shutdown::Write)?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(decode_reply(&reply))
}

#[cfg(test)]
mod portable_tests {
    use super::*;

    #[test]
    fn the_prompt_is_the_argument_after_the_flag_or_the_only_one() {
        let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert_eq!(
            helper_prompt(&args(&["--askpass", "Password:"])),
            "Password:"
        );
        assert_eq!(helper_prompt(&args(&["Password: "])), "Password: ");
        assert_eq!(helper_prompt(&args(&["--askpass"])), "");
        assert_eq!(helper_prompt(&[]), "");
    }

    #[test]
    fn a_scoped_request_round_trips_through_the_relays_parser() {
        let request = String::from_utf8(scoped_request(Some("cluster"), "Passcode:\nDuo")).unwrap();
        let (alias, prompt) = split_prompt_request(request);
        assert_eq!(alias.as_deref(), Some("cluster"));
        assert_eq!(prompt, "Passcode:\nDuo");
        let (alias, _) =
            split_prompt_request(String::from_utf8(scoped_request(None, "Password:")).unwrap());
        assert_eq!(alias, None);
    }

    /// The Windows helper's framing against a stand-in relay: token line
    /// first, then the scoped prompt; the reply decodes as on unix.
    #[cfg(windows)]
    #[test]
    fn the_tcp_helper_sends_the_token_then_the_scoped_prompt() {
        for (reply, want) in [
            (
                encode_reply(Some("hunter2")),
                Reply::Secret("hunter2".into()),
            ),
            (encode_reply(None), Reply::Cancelled),
            (Vec::new(), Reply::Unanswered),
        ] {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let mut request = String::new();
                s.read_to_string(&mut request).unwrap();
                s.write_all(&reply).unwrap();
                request
            });
            let got = ask_tcp(port, "tok", Some("cluster"), "Password:").unwrap();
            assert_eq!(
                server.join().unwrap(),
                "tok\nchimaera-askpass-scope-v1\ncluster\nPassword:"
            );
            assert_eq!(got, want);
        }
    }

    /// The cancel path's parent lookup finds a real, different process.
    #[cfg(windows)]
    #[test]
    fn the_parent_process_is_found_by_snapshot() {
        let (pid, image) = parent_process().expect("a parent");
        assert_ne!(pid, std::process::id());
        assert!(!image.is_empty());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// The askpass socket must always bind: a short runtime dir is used
    /// as-is, while a deep isolated `CHIMAERA_HOME` (whose `<home>/run`
    /// overshoots the ~104-byte `sun_path` cap) falls back to a short `/tmp`
    /// dir keyed by the home — still ending in `/run`, still distinct per
    /// home so a dev app's relay never answers the real app's ssh. Mirrors
    /// `control_dir_stays_under_sun_path_for_a_deep_home` in chimaera-remote.
    #[test]
    fn socket_dir_stays_under_sun_path_for_a_deep_home() {
        // Normal home: unchanged — the runtime dir itself.
        let normal = socket_dir(Path::new("/Users/x/.chimaera-dev-app/worktree/run"));
        assert_eq!(normal, Path::new("/Users/x/.chimaera-dev-app/worktree/run"));

        // Deep isolated home (CHIMAERA_HOME inside a worktree) overshoots →
        // /tmp fallback that keeps the full socket path legal.
        let deep = socket_dir(Path::new(
            "/Users/martinkjellberg/dev/chimaera/.claude/worktrees/magical-colden-00b63c/.chimaera-dev/run",
        ));
        assert!(deep.starts_with("/tmp/"), "{}", deep.display());
        assert!(deep.ends_with("run"), "{}", deep.display());
        assert!(
            deep.as_os_str().len() + 1 + SOCK_LEAF.len() <= 104,
            "{}",
            deep.display()
        );
        // A different deep home resolves to a different socket dir.
        let other = socket_dir(Path::new(
            "/Users/martinkjellberg/dev/chimaera/.claude/worktrees/some-other-worktree-abcdef/.chimaera-dev/run",
        ));
        assert_ne!(other, deep);
    }

    /// Serve the bytes `reply` on a fresh socket and return what the helper's
    /// `ask` made of them, plus the prompt the server saw.
    fn ask_against(tag: &str, reply: Vec<u8>) -> (Reply, String) {
        // A private dir, not `runtime_dir()`: another test points CHIMAERA_HOME
        // at a temp dir and removes it again while the suite runs in parallel.
        let dir = std::env::temp_dir().join(format!("askpass-ask-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("s");
        std::fs::remove_file(&sock).ok();
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut prompt = String::new();
            s.read_to_string(&mut prompt).unwrap(); // returns on the client's half-close
            s.write_all(&reply).unwrap();
            prompt
        });

        let got = ask(sock.as_os_str(), Some("cluster"), "user@host's password:").unwrap();
        let prompt = server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
        (got, prompt)
    }

    /// The helper and server must agree on framing: the client half-closes to
    /// mark the prompt's end, the server replies with the secret + one newline
    /// which the client strips.
    #[test]
    fn helper_round_trips_prompt_and_secret() {
        let (got, prompt) = ask_against("secret", encode_reply(Some("hunter2")));
        assert_eq!(
            prompt,
            "chimaera-askpass-scope-v1\ncluster\nuser@host's password:"
        );
        assert_eq!(got, Reply::Secret("hunter2".into()));
    }

    /// Only an explicit cancel ends the ssh that asked. Enter on a prompt is
    /// an empty SECRET and stays an answer; a relay that refused or died
    /// replies nothing (or is cut off), which is no cancel and no password.
    #[test]
    fn only_an_explicit_reply_cancels() {
        assert_eq!(
            ask_against("cancel", encode_reply(None)).0,
            Reply::Cancelled
        );
        assert_eq!(
            ask_against("enter", encode_reply(Some(""))).0,
            Reply::Secret(String::new())
        );
        assert_eq!(ask_against("refused", Vec::new()).0, Reply::Unanswered);
        assert_eq!(decode_reply("hunt"), Reply::Unanswered);
        assert_eq!(decode_reply("cancel"), Reply::Unanswered);
    }

    /// Run a shim built around a fake helper that exits `helper_status`
    /// (printing `printed`), as a child of an `sh` that would go on to print
    /// "survived" — the stand-in for ssh. Returns that outer sh's stdout and
    /// how it ended.
    fn run_shim(
        tag: &str,
        helper_status: i32,
        printed: &str,
    ) -> (String, std::process::ExitStatus) {
        let _exec = lock(&EXEC_LOCK);
        let dir = std::env::temp_dir().join(format!("askpass-shim-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("helper");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\nprintf '{printed}'\nexit {helper_status}\n"),
        )
        .unwrap();
        let shim = dir.join("askpass.sh");
        std::fs::write(&shim, shim_script(&fake)).unwrap();
        for f in [&fake, &shim] {
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("'{}' 'a prompt'; echo survived", shim.display()))
            .output()
            .unwrap();
        std::fs::remove_dir_all(&dir).ok();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.status,
        )
    }

    /// THE bug: OpenSSH takes a failed askpass for an empty answer and asks
    /// again, so the shim ends the process that ran it when the helper reports
    /// a cancel — and only then.
    #[test]
    fn the_shim_ends_the_asking_process_on_a_cancel_only() {
        use std::os::unix::process::ExitStatusExt;

        let (out, status) = run_shim("cancel", CANCELLED_EXIT, "");
        assert_eq!(status.signal(), Some(15), "asker is sent SIGTERM");
        assert!(!out.contains("survived"), "{out}");

        // An answer passes through untouched, and the asker lives on.
        let (out, status) = run_shim("answer", 0, "hunter2");
        assert_eq!(out, "hunter2survived\n");
        assert!(status.success());

        // Any other helper failure keeps its old meaning: no answer, ssh
        // decides.
        let (out, status) = run_shim("failed", 1, "");
        assert_eq!(out, "survived\n");
        assert!(status.success());
    }

    #[test]
    fn prompt_scope_round_trips_and_old_helpers_stay_unscoped() {
        let (alias, prompt) = split_prompt_request(
            "chimaera-askpass-scope-v1\ncluster\nPasscode or option (1-3):\nDuo".into(),
        );
        assert_eq!(alias.as_deref(), Some("cluster"));
        assert_eq!(prompt, "Passcode or option (1-3):\nDuo");

        let (alias, prompt) = split_prompt_request("Passcode or option (1-3):\nDuo prompt".into());
        assert_eq!(alias, None);
        assert_eq!(prompt, "Passcode or option (1-3):\nDuo prompt");
    }

    #[test]
    fn pending_and_answer_enforce_the_shell_window_scope() {
        let askpass = Askpass::default();
        let (tx, rx) = oneshot::channel();
        let id = askpass.register(Some("remote-2".into()), "Password:".into(), tx);
        let remote_1 = crate::shell::WindowScope::new(
            Some("remote-1".into()),
            Some("workspace".into()),
            "remote-1-window".into(),
        );

        assert!(askpass.pending_scoped(&remote_1).is_empty());
        assert_eq!(
            askpass.answer_scoped(id, Some("wrong-window".into()), &remote_1),
            AnswerResult::Forbidden
        );
        let local_workspace = crate::shell::WindowScope::new(
            None,
            Some("local-workspace".into()),
            "local-window".into(),
        );
        assert!(askpass.pending_scoped(&local_workspace).is_empty());

        // Home-granted fallback is shell-owned: later workspace navigation
        // cannot hide an in-flight startup/first-connect prompt.
        let mut fallback = crate::shell::WindowScope::new(None, None, "fallback-window".into());
        fallback.ws = Some("opened-after-connect-started".into());
        assert_eq!(askpass.pending_scoped(&fallback).len(), 1);
        assert_eq!(
            askpass.answer_scoped(id, Some("secret".into()), &fallback),
            AnswerResult::Answered(Some("remote-2".into()))
        );
        assert_eq!(rx.blocking_recv().unwrap(), Some("secret".into()));
    }

    #[test]
    fn compute_prompts_require_an_explicit_login_host_scope() {
        let askpass = Askpass::default();
        let (tx, rx) = oneshot::channel();
        let id = askpass.register(Some("cluster".into()), "Password:".into(), tx);

        // This is a valid ordinary ssh alias, not proof that the window is a
        // compute view reached through `cluster`.
        let colliding_remote = crate::shell::WindowScope::new(
            Some("cluster#job123".into()),
            Some("workspace".into()),
            "ordinary-remote".into(),
        );
        assert!(askpass.pending_scoped(&colliding_remote).is_empty());
        assert_eq!(
            askpass.answer_scoped(id, Some("wrong-window".into()), &colliding_remote),
            AnswerResult::Forbidden
        );

        let compute = crate::shell::WindowScope::new_compute(
            "cluster#job123".into(),
            "cluster".into(),
            Some("workspace".into()),
            "compute-window".into(),
        );
        assert_eq!(askpass.pending_scoped(&compute).len(), 1);
        assert_eq!(
            askpass.answer_scoped(id, Some("secret".into()), &compute),
            AnswerResult::Answered(Some("cluster".into()))
        );
        assert_eq!(rx.blocking_recv().unwrap(), Some("secret".into()));
    }

    /// A cancelled connect drops exactly its own host's prompts, and each
    /// dropped prompt's waiter sees the sender go (so `resolve_prompt` ends
    /// it and dismisses it in its windows) instead of waiting out 180 s.
    #[test]
    fn discarding_an_alias_drops_only_that_hosts_prompts() {
        let askpass = Askpass::default();
        let (tx_a, rx_a) = oneshot::channel();
        let (tx_a2, rx_a2) = oneshot::channel();
        let (tx_b, mut rx_b) = oneshot::channel();
        let (tx_none, mut rx_none) = oneshot::channel();
        askpass.register(Some("host-a".into()), "Password:".into(), tx_a);
        askpass.register(Some("host-a".into()), "Passcode:".into(), tx_a2);
        askpass.register(Some("host-b".into()), "Password:".into(), tx_b);
        askpass.register(None, "Password:".into(), tx_none);

        assert_eq!(askpass.discard_alias("host-a"), 2);
        assert!(rx_a.blocking_recv().is_err(), "the waiter is released");
        assert!(rx_a2.blocking_recv().is_err());
        assert!(rx_b
            .try_recv()
            .is_err_and(|e| e == oneshot::error::TryRecvError::Empty));
        assert!(rx_none
            .try_recv()
            .is_err_and(|e| e == oneshot::error::TryRecvError::Empty));
        let home = crate::shell::WindowScope::new(None, None, "home".into());
        assert_eq!(askpass.pending_scoped(&home).len(), 2);
        assert_eq!(askpass.discard_alias("host-a"), 0);
    }

    /// Any same-user process can connect to the relay socket: a request past
    /// the cap or one that never finishes is dropped, never read without end.
    #[tokio::test]
    async fn a_prompt_request_is_bounded_in_size_and_time() {
        use tokio::io::AsyncWriteExt;

        let (mut client, mut server) = tokio::io::duplex(1024);
        let writer = tokio::spawn(async move {
            client.write_all(&[b'x'; 200]).await.unwrap();
            client.shutdown().await.unwrap();
        });
        assert_eq!(
            read_request(&mut server, 200, Duration::from_secs(5)).await,
            Some("x".repeat(200)),
            "exactly the cap is a request"
        );
        writer.await.unwrap();

        let (mut client, mut server) = tokio::io::duplex(1024);
        let writer = tokio::spawn(async move {
            // Past the cap; the reader stops after cap + 1 bytes.
            let _ = client.write_all(&[b'x'; 900]).await;
        });
        assert_eq!(
            read_request(&mut server, 200, Duration::from_secs(5)).await,
            None
        );
        writer.abort();

        // Opened and never finished: the deadline ends the read.
        let (_client, mut server) = tokio::io::duplex(1024);
        assert_eq!(
            read_request(&mut server, 200, Duration::from_millis(50)).await,
            None
        );
    }

    /// The daemon gets back the user's own askpass, never the app's shim.
    #[test]
    fn only_the_users_own_askpass_is_remembered_for_the_daemon() {
        let shim = Path::new("/home/u/.chimaera/run/askpass.sh");
        let pairs = |list: &[(&str, &str)]| -> Vec<(String, String)> {
            list.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let theirs = pairs(&[
            ("SSH_ASKPASS", "/usr/libexec/ssh-askpass"),
            ("SSH_ASKPASS_REQUIRE", "prefer"),
        ]);
        assert_eq!(user_askpass(theirs.clone(), shim), theirs);
        let ours = pairs(&[
            ("SSH_ASKPASS", "/home/u/.chimaera/run/askpass.sh"),
            ("SSH_ASKPASS_REQUIRE", "force"),
        ]);
        assert!(user_askpass(ours, shim).is_empty());
        assert!(user_askpass(pairs(&[("SSH_ASKPASS_REQUIRE", "force")]), shim).is_empty());
        assert!(user_askpass(Vec::new(), shim).is_empty());
    }

    /// The `/tmp` fallback is refused when another user could have planted
    /// it: a world-accessible dir at either level, or a symlink.
    #[test]
    fn the_tmp_fallback_dir_must_be_private_at_both_levels() {
        let root = std::env::temp_dir().join(format!("askpass-private-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();

        let fresh = root.join("fresh").join("run");
        ensure_private_fallback(&fresh).unwrap();
        for dir in [fresh.parent().unwrap(), fresh.as_path()] {
            let mode = std::fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
        }

        let planted_parent = root.join("planted");
        std::fs::create_dir(&planted_parent).unwrap();
        std::fs::set_permissions(&planted_parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(ensure_private_fallback(&planted_parent.join("run")).is_err());
        assert!(
            !planted_parent.join("run").exists(),
            "nothing created inside it"
        );

        let planted_leaf = root.join("leaf");
        std::fs::create_dir(&planted_leaf).unwrap();
        std::fs::create_dir(planted_leaf.join("run")).unwrap();
        std::fs::set_permissions(
            planted_leaf.join("run"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::set_permissions(&planted_leaf, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(ensure_private_fallback(&planted_leaf.join("run")).is_err());

        let linked = root.join("linked");
        std::os::unix::fs::symlink(&fresh, &linked).unwrap();
        assert!(ensure_private_fallback(&linked.join("run")).is_err());

        std::fs::set_permissions(&planted_parent, std::fs::Permissions::from_mode(0o700)).ok();
        std::fs::remove_dir_all(&root).ok();
    }
}
