//! In-app SSH auth prompts. From a windowed app ssh has no tty to ask for a
//! password or a Duo passcode, so we point `SSH_ASKPASS` at ourselves: when
//! ssh (bringing up the ControlMaster — see `chimaera-remote`) needs input, it
//! runs this binary in `--askpass` mode with the prompt; that helper hands the
//! prompt to the running app over a unix socket, the app shows a modal, and
//! the typed answer flows back to ssh on stdout. Because every ssh call to a
//! host multiplexes one ControlMaster, the user is asked once per host, not
//! once per command.
//!
//! Keyboard-interactive prompts (Duo's "Passcode or option (1-3):") reach
//! askpass too, so the same modal — prompt text over a single input — covers
//! both password and 2FA. Host-key confirmation is not an askpass prompt and
//! is out of scope here (a first connect to an unknown host still needs the
//! key in `~/.ssh/known_hosts`).
//! Each child also frames its normalized host alias with the prompt, so the
//! native relay can target only that host's windows (plus local home).

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
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

/// Env var carrying the askpass socket path to the helper (and to ssh, which
/// passes its environment through to the askpass program it runs).
#[cfg(unix)]
const SOCK_ENV: &str = "CHIMAERA_ASKPASS_SOCK";
pub(crate) const SCOPE_FRAME: &str = "chimaera-askpass-scope-v1";

/// How long a prompt waits for the UI before giving up. A dropped window or
/// an ignored modal must not pin an ssh process open forever — on timeout we
/// return no answer and ssh fails cleanly.
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

/// Emit an askpass event only to shell-registered windows allowed to observe
/// this host. Tauri event subscriptions are directly available to daemon
/// pages, so app-wide broadcast plus a Svelte filter is not an auth boundary.
fn emit_scoped<T: Clone + Serialize>(
    app: &AppHandle,
    event: &str,
    payload: T,
    alias: Option<&str>,
) {
    let Some(shell) = app.try_state::<crate::shell::Shell>() else {
        return;
    };
    for label in shell.askpass_targets(alias) {
        let Some(window) = app.get_webview_window(&label) else {
            continue;
        };
        if let Err(error) = window.emit(event, payload.clone()) {
            tracing::warn!("askpass: could not emit {event} to {label}: {error}");
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
fn socket_path() -> PathBuf {
    let dir = socket_dir(&chimaera_core::runtime_dir());
    // The /tmp fallback dir is ours alone — same 0700 as the runtime dir it
    // stands in for (the socket carries ssh secrets). Best-effort like the
    // core dir resolvers: the bind reports the real failure.
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    if let Err(e) = builder.create(&dir) {
        tracing::warn!("failed to create askpass socket dir {}: {e}", dir.display());
    }
    dir.join(SOCK_LEAF)
}

#[cfg(unix)]
fn shim_path() -> PathBuf {
    chimaera_core::runtime_dir().join("askpass.sh")
}

/// Wire ssh/scp spawned from this process (and their ControlMaster children)
/// to prompt through the app: write the askpass shim, export the ssh env, and
/// start the socket listener. Called once at startup, before any connect.
#[cfg(unix)]
pub fn install(app: &AppHandle) -> Result<()> {
    let sock = socket_path();
    // A stale socket from a previous run would refuse the bind.
    std::fs::remove_file(&sock).ok();

    let exe = std::env::current_exe().context("resolve current executable")?;
    let shim = shim_path();
    // ssh runs `$SSH_ASKPASS "<prompt>"` with exactly one arg, so a tiny shim
    // re-invokes us in --askpass mode. `$@` forwards that single prompt arg.
    std::fs::write(&shim, format!("#!/bin/sh\nexec {exe:?} --askpass \"$@\"\n"))
        .with_context(|| format!("write {}", shim.display()))?;
    let mut perms = std::fs::metadata(&shim)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(&shim, perms)?;

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

/// Windows: a token-gated loopback TCP listener. ssh runs INSIDE WSL and
/// execs a distro-side wrapper script (installed by `wsl::wire_connect`,
/// which bakes this listener's port + token in); the wrapper pipes the
/// prompt to `chimaera.exe --askpass` over interop, and that helper connects
/// back here — Windows-side loopback, never WSL→Windows TCP (firewalled by
/// default under NAT). The token exists because loopback TCP lacks the unix
/// socket's 0700-dir confidentiality: without it any local process could
/// present a fake prompt and be handed a typed password.
#[cfg(windows)]
static RELAY: std::sync::OnceLock<(u16, String)> = std::sync::OnceLock::new();

/// The relay's `(port, token)` for the WSL wrapper script to embed.
#[cfg(windows)]
pub fn relay_endpoint() -> Option<(u16, String)> {
    RELAY.get().cloned()
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

#[cfg(windows)]
pub fn install(app: &AppHandle) -> Result<()> {
    let token = chimaera_core::generate_token();
    let std_listener =
        std::net::TcpListener::bind(("127.0.0.1", 0)).context("bind askpass relay")?;
    std_listener.set_nonblocking(true)?;
    let port = std_listener.local_addr()?.port();
    let _ = RELAY.set((port, token.clone()));

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
/// client's half-close) is the prompt; reply secret + newline. The read is
/// SIZE-CAPPED and TIMED: unlike the unix socket (gated by a 0700 dir), any
/// local process can connect to this loopback port — without bounds one
/// could stream gigabytes into this String or hold a task open forever.
/// The token protects secrecy; these bounds protect availability.
#[cfg(windows)]
async fn serve_one_tcp(app: AppHandle, mut stream: tokio::net::TcpStream, token: String) {
    /// Token line + a generous ceiling for any real ssh prompt.
    const MAX_REQUEST: u64 = 64 * 1024;
    const READ_TIMEOUT: Duration = Duration::from_secs(10);

    let mut input = String::new();
    let read = tokio::time::timeout(
        READ_TIMEOUT,
        (&mut stream).take(MAX_REQUEST).read_to_string(&mut input),
    )
    .await;
    match read {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            tracing::warn!("askpass: could not read relay request: {e}");
            return;
        }
        Err(_) => {
            tracing::warn!("askpass: relay request timed out");
            return;
        }
    }
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
    let _ = stream.write_all(answer.as_bytes()).await;
    let _ = stream.write_all(b"\n").await;
    let _ = stream.shutdown().await;
}

/// Serve one askpass request: read the prompt (the helper half-closes its
/// write side to mark the end), ask the UI, write the answer back.
#[cfg(unix)]
async fn serve_one(app: AppHandle, mut stream: UnixStream) {
    let mut request = String::new();
    if let Err(e) = stream.read_to_string(&mut request).await {
        tracing::warn!("askpass: could not read prompt: {e}");
        return;
    }
    let (alias, prompt) = split_prompt_request(request);
    let answer = resolve_prompt(&app, alias, prompt).await;
    // ssh reads the secret up to the first newline; terminate with exactly one.
    let _ = stream.write_all(answer.as_bytes()).await;
    let _ = stream.write_all(b"\n").await;
    let _ = stream.shutdown().await;
}

/// Register the prompt, ask the UI, wait out the timeout — the transport-
/// agnostic middle both the unix socket and the Windows TCP relay feed.
async fn resolve_prompt(app: &AppHandle, alias: Option<String>, prompt: String) -> String {
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
        Ok(Ok(Some(secret))) => secret,
        // Cancelled, timed out, or the app dropped the sender: no answer, so
        // ssh moves on and fails cleanly rather than hanging. Windows still
        // showing the prompt must drop it — there is no one left to receive
        // an answer.
        _ => {
            state.discard(id);
            emit_done(app, id, event.alias.as_deref());
            String::new()
        }
    }
}

/// `chimaera-app --askpass <prompt>`: the helper ssh execs. Relays the prompt
/// to the running app and prints the answer for ssh to read. A missing socket
/// or app yields an empty answer (ssh then fails cleanly, never hangs).
#[cfg(unix)]
pub fn run_helper() {
    let prompt = std::env::args()
        .skip_while(|a| a != "--askpass")
        .nth(1)
        .unwrap_or_default();
    let Some(sock) = std::env::var_os(SOCK_ENV) else {
        return;
    };
    let alias = std::env::var(chimaera_remote::ASKPASS_ALIAS_ENV)
        .ok()
        .filter(|alias| !alias.is_empty());
    let answer = ask(&sock, alias.as_deref(), &prompt).unwrap_or_default();
    print!("{answer}");
    std::io::stdout().flush().ok();
}

/// Windows `--askpass`: invoked THROUGH WSL interop by the distro-side
/// wrapper script. Everything arrives on stdin — line 1 `"<port> <token>"`,
/// then the versioned scope frame, alias, and prompt to EOF — never argv,
/// whose Linux→Windows marshaling for arbitrary prompt text is unverified.
/// Any failure prints nothing: ssh gets an empty answer and fails cleanly,
/// never hangs.
#[cfg(windows)]
pub fn run_helper() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let Some((head, prompt)) = input.split_once('\n') else {
        return;
    };
    let mut parts = head.split_whitespace();
    let (Some(port), Some(token)) = (
        parts.next().and_then(|p| p.parse::<u16>().ok()),
        parts.next(),
    ) else {
        return;
    };
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return;
    };
    if stream.write_all(token.as_bytes()).is_err()
        || stream.write_all(b"\n").is_err()
        || stream.write_all(prompt.as_bytes()).is_err()
        || stream.shutdown(Shutdown::Write).is_err()
    {
        return;
    }
    let mut answer = String::new();
    if stream.read_to_string(&mut answer).is_err() {
        return;
    }
    print!("{}", answer.strip_suffix('\n').unwrap_or(&answer));
    let _ = std::io::stdout().flush();
}

#[cfg(unix)]
fn ask(sock: &std::ffi::OsStr, alias: Option<&str>, prompt: &str) -> Result<String> {
    let mut stream = StdUnixStream::connect(sock).context("connect askpass socket")?;
    stream.write_all(SCOPE_FRAME.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.write_all(alias.unwrap_or_default().as_bytes())?;
    stream.write_all(b"\n")?;
    stream.write_all(prompt.as_bytes())?;
    // Half-close so the server's read-to-EOF returns the whole prompt.
    stream.shutdown(Shutdown::Write)?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer)?;
    // The server terminates the answer with a newline; hand ssh just the secret.
    Ok(answer.strip_suffix('\n').unwrap_or(&answer).to_string())
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

    /// The helper and server must agree on framing: the client half-closes to
    /// mark the prompt's end, the server replies with the secret + one newline
    /// which the client strips. This mirrors `serve_one`'s write framing.
    #[test]
    fn helper_round_trips_prompt_and_secret() {
        let sock =
            chimaera_core::runtime_dir().join(format!("askpass-test-{}.sock", std::process::id()));
        std::fs::remove_file(&sock).ok();
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut prompt = String::new();
            s.read_to_string(&mut prompt).unwrap(); // returns on the client's half-close
            assert_eq!(
                prompt,
                "chimaera-askpass-scope-v1\nSherlock\nuser@host's password:"
            );
            s.write_all(b"hunter2\n").unwrap(); // secret + newline, same as serve_one
        });

        let got = ask(sock.as_os_str(), Some("Sherlock"), "user@host's password:").unwrap();
        assert_eq!(got, "hunter2");
        server.join().unwrap();
        std::fs::remove_file(&sock).ok();
    }

    #[test]
    fn prompt_scope_round_trips_and_old_helpers_stay_unscoped() {
        let (alias, prompt) = split_prompt_request(
            "chimaera-askpass-scope-v1\nSherlock\nPasscode or option (1-3):\nDuo".into(),
        );
        assert_eq!(alias.as_deref(), Some("Sherlock"));
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
}
