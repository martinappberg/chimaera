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

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

/// Env var carrying the askpass socket path to the helper (and to ssh, which
/// passes its environment through to the askpass program it runs).
const SOCK_ENV: &str = "CHIMAERA_ASKPASS_SOCK";

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
/// prompt" failure. The UI fetches `pending()` on mount to close that gap.
#[derive(Default)]
pub struct Askpass {
    pending: Mutex<HashMap<u64, PendingPrompt>>,
    seq: AtomicU64,
}

struct PendingPrompt {
    prompt: String,
    tx: oneshot::Sender<Option<String>>,
}

/// `ssh-askpass` event payload: a prompt the UI must answer.
#[derive(Clone, Serialize)]
pub struct PromptEvent {
    id: u64,
    prompt: String,
}

impl Askpass {
    fn register(&self, prompt: String, tx: oneshot::Sender<Option<String>>) -> u64 {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        lock(&self.pending).insert(id, PendingPrompt { prompt, tx });
        id
    }

    fn discard(&self, id: u64) {
        lock(&self.pending).remove(&id);
    }

    /// Resolve prompt `id` with the user's answer (`None` = cancelled).
    /// Returns whether the prompt was still pending — the caller broadcasts
    /// `ssh-askpass-done` so every OTHER window showing it dismisses too.
    pub fn answer(&self, id: u64, secret: Option<String>) -> bool {
        match lock(&self.pending).remove(&id) {
            Some(p) => {
                let _ = p.tx.send(secret);
                true
            }
            None => false,
        }
    }

    /// The prompts still awaiting an answer, oldest first (ssh asks
    /// sequentially, so order is answer order).
    pub fn pending(&self) -> Vec<PromptEvent> {
        let mut prompts: Vec<PromptEvent> = lock(&self.pending)
            .iter()
            .map(|(id, p)| PromptEvent {
                id: *id,
                prompt: p.prompt.clone(),
            })
            .collect();
        prompts.sort_by_key(|p| p.id);
        prompts
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn socket_path() -> PathBuf {
    chimaera_core::runtime_dir().join("askpass.sock")
}

fn shim_path() -> PathBuf {
    chimaera_core::runtime_dir().join("askpass.sh")
}

/// Wire ssh/scp spawned from this process (and their ControlMaster children)
/// to prompt through the app: write the askpass shim, export the ssh env, and
/// start the socket listener. Called once at startup, before any connect.
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

/// Serve one askpass request: read the prompt (the helper half-closes its
/// write side to mark the end), ask the UI, write the answer back.
async fn serve_one(app: AppHandle, mut stream: UnixStream) {
    let mut prompt = String::new();
    if let Err(e) = stream.read_to_string(&mut prompt).await {
        tracing::warn!("askpass: could not read prompt: {e}");
        return;
    }

    let state = app.state::<Askpass>();
    let (tx, rx) = oneshot::channel();
    let prompt = prompt.trim_end().to_string();
    let id = state.register(prompt.clone(), tx);
    let event = PromptEvent { id, prompt };
    // The emit reaches only windows that are ALREADY listening; windows that
    // mount later find this prompt via `pending()` (`list_askpass`). Zero
    // windows at emit time is therefore fine — not an error.
    if app.emit("ssh-askpass", event).is_err() {
        state.discard(id);
        return;
    }

    let answer = match tokio::time::timeout(PROMPT_TIMEOUT, rx).await {
        Ok(Ok(Some(secret))) => secret,
        // Cancelled, timed out, or the app dropped the sender: no answer, so
        // ssh moves on and fails cleanly rather than hanging. Windows still
        // showing the prompt must drop it — there is no one left to receive
        // an answer.
        _ => {
            state.discard(id);
            let _ = app.emit("ssh-askpass-done", id);
            String::new()
        }
    };
    // ssh reads the secret up to the first newline; terminate with exactly one.
    let _ = stream.write_all(answer.as_bytes()).await;
    let _ = stream.write_all(b"\n").await;
    let _ = stream.shutdown().await;
}

/// `chimaera-app --askpass <prompt>`: the helper ssh execs. Relays the prompt
/// to the running app and prints the answer for ssh to read. A missing socket
/// or app yields an empty answer (ssh then fails cleanly, never hangs).
pub fn run_helper() {
    let prompt = std::env::args()
        .skip_while(|a| a != "--askpass")
        .nth(1)
        .unwrap_or_default();
    let Some(sock) = std::env::var_os(SOCK_ENV) else {
        return;
    };
    let answer = ask(&sock, &prompt).unwrap_or_default();
    print!("{answer}");
    std::io::stdout().flush().ok();
}

fn ask(sock: &std::ffi::OsStr, prompt: &str) -> Result<String> {
    let mut stream = StdUnixStream::connect(sock).context("connect askpass socket")?;
    stream.write_all(prompt.as_bytes())?;
    // Half-close so the server's read-to-EOF returns the whole prompt.
    stream.shutdown(Shutdown::Write)?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer)?;
    // The server terminates the answer with a newline; hand ssh just the secret.
    Ok(answer.strip_suffix('\n').unwrap_or(&answer).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

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
            assert_eq!(prompt, "user@host's password:");
            s.write_all(b"hunter2\n").unwrap(); // secret + newline, same as serve_one
        });

        let got = ask(sock.as_os_str(), "user@host's password:").unwrap();
        assert_eq!(got, "hunter2");
        server.join().unwrap();
        std::fs::remove_file(&sock).ok();
    }
}
