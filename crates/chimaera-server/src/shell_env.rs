//! The environment of the user's interactive login shell, read once and laid
//! over every agent the daemon starts.
//!
//! An agent runs through `$SHELL -lc` (`launcher::wrap_login_shell`): the
//! login profile is read, the interactive rc (`~/.zshrc`, `~/.bashrc`) is
//! not — and the rc is where most exports live. The spawn itself can't turn
//! interactive: a chat agent speaks NDJSON on stdout and an rc may print, an
//! rc may be slow or `exec` something, and `-i` without a terminal complains
//! about job control. So one interactive login shell is asked for its
//! environment apart from every spawn, in its own process group, under a
//! deadline and a size cap, and what its startup files set is added to the
//! spawn's environment ([`overlay`]). A capture that fails changes nothing:
//! agents start as they did before it existed.
//!
//! The capture lives as long as the startup files it read: each spawn stats
//! them and a changed one is read again, so an export added to `~/.zshrc`
//! reaches the next agent. A file they source can't be seen that way; the
//! explicit re-checks ([`invalidate`]) and a daemon restart cover it. It has
//! no maximum age on purpose: reading again on a timer would re-run whatever
//! else the rc does (a keychain prompt, an agent it starts) with nobody
//! asking. The price is that a value the rc computes at startup, such as a
//! short-lived token, is as old as the capture.
//!
//! One capture serves the whole process: it is a fact about this process's
//! environment and the user's files, and the probe commands that need it
//! (`agent_probe::base_command`) are built without an `AppState`.
//!
//! **Values here are the user's secrets** (API keys live in rc files). They
//! are held in memory only and never logged: [`ShellEnv`]'s `Debug` prints
//! counts, and every log line names the shell and a reason, nothing else.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use futures::FutureExt;

/// The interactive login shell's PATH, handed to the login wrap to restore
/// once the profile has run (see [`ShellEnv::overlay`]). The wrap unsets it,
/// so no agent ever sees it.
pub(crate) const SHELL_PATH_ENV: &str = "CHIMAERA_SHELL_PATH";

/// How long a spawn waits for a capture before starting without it.
const WAIT: Duration = Duration::from_secs(6);
/// How long the shell itself gets. Longer than a spawn waits: a slow rc on a
/// loaded host still lands for the next agent instead of failing every time.
const BUDGET: Duration = Duration::from_secs(30);
/// Startup chatter plus the environment. Past it the capture is dropped.
const OUTPUT_CAP: usize = 512 * 1024;
/// A shell that couldn't answer is asked again after this long — or at once
/// when a startup file changes or the user re-checks.
const RETRY_FAILED: Duration = Duration::from_secs(600);

/// What the startup files set, relative to the environment the shell started
/// from. No `Debug` derive: see the module doc.
pub(crate) struct ShellEnv {
    vars: Vec<(String, String)>,
    path: Option<String>,
    /// A `ZDOTDIR` the startup files set: where zsh's rc files really are.
    zdotdir: Option<PathBuf>,
}

impl std::fmt::Debug for ShellEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellEnv")
            .field("vars", &self.vars.len())
            .field("path", &self.path.is_some())
            .finish()
    }
}

/// Never taken from the capture: the capturing shell's own process state, the
/// terminal (it had none, so an rc's `export GPG_TTY=$(tty)` recorded "not a
/// tty"), and chimaera's variables, which a spawn sets itself.
fn spawn_owned(name: &str) -> bool {
    matches!(
        name,
        "_" | "SHLVL" | "PWD" | "OLDPWD" | "TERM" | "COLUMNS" | "LINES" | "TTY" | "GPG_TTY"
    ) || name.starts_with("CHIMAERA_")
}

impl ShellEnv {
    /// Keep what the startup files added or changed. `base` is what the shell
    /// started from, so a variable the daemon already has unchanged is left to
    /// plain inheritance.
    fn from_capture(captured: Vec<(String, String)>, base: &HashMap<String, String>) -> Self {
        let mut env = ShellEnv {
            vars: Vec::new(),
            path: None,
            zdotdir: None,
        };
        for (name, value) in captured {
            if name == "PATH" {
                env.path = Some(value).filter(|p| !p.is_empty());
                continue;
            }
            if name == "ZDOTDIR" && !value.is_empty() {
                env.zdotdir = Some(PathBuf::from(&value));
            }
            if spawn_owned(&name) || base.get(&name) == Some(&value) {
                continue;
            }
            env.vars.push((name, value));
        }
        env
    }

    /// Add the captured variables to a spawn's `env`. Precedence, highest
    /// first: what the spawn sets itself (already in `env`), the capture, the
    /// daemon's inherited environment. Call it after every explicit entry is
    /// in and before `api::spawn_env_remove`, which then leaves a scrubbed
    /// name alone when the user's own rc sets it.
    ///
    /// PATH is not set here: the login profile runs after the spawn's
    /// environment is in place and reorders it (macOS `path_helper` moves the
    /// system directories to the front). The wrap restores the captured order
    /// from [`SHELL_PATH_ENV`] afterwards. Directories the spawn itself put
    /// on PATH — anything beyond what every session starts with
    /// (`api::spawn_path`: the shim dir, which the wrap prepends, then
    /// `daemon_path` or the system default) — keep their place in front.
    fn overlay(&self, env: &mut Vec<(String, String)>, daemon_path: &str) {
        for (name, value) in &self.vars {
            if !env.iter().any(|(k, _)| k == name) {
                env.push((name.clone(), value.clone()));
            }
        }
        let Some(path) = &self.path else {
            return;
        };
        if env.iter().any(|(k, _)| k == SHELL_PATH_ENV) {
            return;
        }
        let value_of = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
        let every_session =
            crate::api::spawn_path(value_of("CHIMAERA_SHIMS").unwrap_or_default(), daemon_path);
        let mut entries: Vec<&str> = value_of("PATH")
            .unwrap_or_default()
            .split(':')
            .filter(|dir| {
                !dir.is_empty()
                    && !every_session.split(':').any(|d| d == *dir)
                    && !path.split(':').any(|d| d == *dir)
            })
            .collect();
        entries.push(path);
        env.push((SHELL_PATH_ENV.to_string(), entries.join(":")));
    }
}

/// Why a capture produced nothing. Carries no output: the shell's words may
/// hold the very values this module must not log.
#[derive(Debug, PartialEq)]
enum CaptureError {
    Start(std::io::ErrorKind),
    Read,
    Timeout,
    TooLarge,
    /// Startup never reached the dump, or `env` printed nothing.
    Incomplete,
    /// The dump is not NUL-separated (an `env` without `-0`).
    Malformed,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Start(kind) => write!(f, "the shell did not start ({kind})"),
            Self::Read => f.write_str("the shell's output could not be read"),
            Self::Timeout => f.write_str("the shell took too long to start"),
            Self::TooLarge => f.write_str("the shell printed more than the size limit"),
            Self::Incomplete => f.write_str("the shell stopped before printing its environment"),
            Self::Malformed => f.write_str("the shell's `env` has no NUL-separated output"),
        }
    }
}

fn shell_name(shell: &str) -> &str {
    shell.rsplit('/').next().unwrap_or(shell)
}

/// A login bash reads only the profile; a terminal reads `~/.bashrc`. So the
/// capture sources it, unless the profile bash just read already names it
/// (the stock ones do), which keeps a slow rc from running twice inside one
/// budget. The match is the file name `.bashrc` outside a comment, whole:
/// `/etc/bashrc`, `/etc/bash.bashrc` and `~/.bashrc.d` are other files. A
/// wrong guess is mild either way: sourced twice is what a terminal opened
/// inside a login session does, not at all is how agents started before the
/// capture existed.
const BASHRC_STEP: &str = r#"for __chimaera_p in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do [ -r "$__chimaera_p" ] && break; __chimaera_p=; done; if [ -r "$HOME/.bashrc" ] && ! { [ -n "$__chimaera_p" ] && command grep -Eq '^[^#]*[^[:alnum:]]\.bashrc([^[:alnum:]_.-]|$)' "$__chimaera_p"; }; then . "$HOME/.bashrc"; fi; unset __chimaera_p; "#;

/// The `-c` text: the environment, NUL-separated, between two nonce lines so
/// nothing a startup file prints can be mistaken for it. Semicolons, `printf`
/// and `command` read the same in POSIX shells and fish.
fn capture_script(shell: &str, start: &str, end: &str) -> String {
    let dump = format!("printf '\\n{start}\\n'; command env -0; printf '\\n{end}\\n'");
    if shell_name(shell) == "bash" {
        format!("{BASHRC_STEP}{dump}")
    } else {
        dump
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The variables between the markers. Everything before the start line is
/// startup chatter and ignored; values keep their newlines (NUL separates
/// entries). An entry that isn't UTF-8 or has no name is skipped: a spawn's
/// environment is a list of strings. So is one whose name holds whitespace
/// or a control character: that is a background job's output landing in the
/// middle of the dump, not a variable.
fn parse_capture(
    bytes: &[u8],
    start: &str,
    end: &str,
) -> Result<Vec<(String, String)>, CaptureError> {
    if bytes.len() > OUTPUT_CAP {
        return Err(CaptureError::TooLarge);
    }
    let start = format!("\n{start}\n");
    let end = format!("\n{end}\n");
    let from = find(bytes, start.as_bytes()).ok_or(CaptureError::Incomplete)? + start.len();
    let len = bytes[from..]
        .windows(end.len())
        .rposition(|w| w == end.as_bytes())
        .ok_or(CaptureError::Incomplete)?;
    let block = &bytes[from..from + len];
    if block.is_empty() {
        return Err(CaptureError::Incomplete);
    }
    // `env -0` ends every entry with NUL. Lines instead mean an `env` that
    // ignored the flag, and splitting those would glue variables together.
    if block.last() != Some(&0) {
        return Err(CaptureError::Malformed);
    }
    Ok(block
        .split(|b| *b == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .filter_map(|entry| entry.split_once('='))
        .filter(|(name, _)| {
            !name.is_empty() && !name.chars().any(|c| c.is_whitespace() || c.is_control())
        })
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect())
}

/// Whose environment to read: the login shell, and the environment it starts
/// from — the daemon's, minus what every spawn scrubs, so a launcher marker
/// the daemon inherited is never mistaken for something the user's rc set.
#[derive(Clone)]
struct Source {
    shell: String,
    base: Arc<HashMap<String, String>>,
}

impl Source {
    fn daemon() -> Self {
        let mut base: HashMap<String, String> = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        for name in crate::api::spawn_env_remove(&[]) {
            base.remove(&name);
        }
        Self {
            shell: crate::launcher::login_shell(),
            base: Arc::new(base),
        }
    }

    fn dir(&self, name: &str) -> Option<PathBuf> {
        self.base
            .get(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }
}

/// Run the shell and read its environment. The markers decide success, not
/// the exit status (that is the rc's last command's, or a logout file's).
async fn capture(source: &Source, budget: Duration) -> Result<Vec<(String, String)>, CaptureError> {
    use tokio::io::AsyncReadExt;
    let nonce = chimaera_core::generate_token();
    let start = format!("chimaera-env-{nonce}-start");
    let end = format!("chimaera-env-{nonce}-end");
    let mut child = tokio::process::Command::new(&source.shell)
        .arg("-ilc")
        .arg(capture_script(&source.shell, &start, &end))
        .env_clear()
        .envs(source.base.iter())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| CaptureError::Start(e.kind()))?;
    // Dropped unless the shell is reaped first: an rc's children die with a
    // capture that timed out or was abandoned.
    let mut group = crate::launcher::ShellProbeGroup(
        child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32)),
    );
    let mut stdout = child.stdout.take().expect("piped stdout");
    let end_line = format!("\n{end}\n");
    let work = async {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        // Stop at the end marker rather than at EOF: a background job an rc
        // started can hold the pipe open long after the shell has answered.
        loop {
            let n = stdout
                .read(&mut chunk)
                .await
                .map_err(|_| CaptureError::Read)?;
            if n == 0 {
                break;
            }
            let scan_from = bytes.len().saturating_sub(end_line.len());
            bytes.extend_from_slice(&chunk[..n]);
            if bytes.len() > OUTPUT_CAP {
                return Err(CaptureError::TooLarge);
            }
            if find(&bytes[scan_from..], end_line.as_bytes()).is_some() {
                break;
            }
        }
        if tokio::time::timeout(Duration::from_secs(1), child.wait())
            .await
            .is_ok()
        {
            // The leader has been reaped: never signal this group id again.
            group.0 = None;
        }
        parse_capture(&bytes, &start, &end)
    };
    tokio::time::timeout(budget, work)
        .await
        .unwrap_or(Err(CaptureError::Timeout))
}

/// The startup files a shell reads, with each one's mtime and size (`None` =
/// absent). The capture is as fresh as this list is unchanged.
type Stamp = Vec<(PathBuf, Option<(SystemTime, u64)>)>;

fn startup_files(source: &Source, captured_zdotdir: Option<PathBuf>) -> Vec<PathBuf> {
    let home = source.dir("HOME");
    match shell_name(&source.shell) {
        "zsh" => {
            // Both: `ZDOTDIR` unset reads `$HOME`, a `~/.zshenv` may re-home
            // the rest, and a chimaera terminal's shim `ZDOTDIR` forwards to
            // the user's own.
            let mut dirs: Vec<PathBuf> = Vec::new();
            for dir in [home, source.dir("ZDOTDIR"), captured_zdotdir]
                .into_iter()
                .flatten()
            {
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
            dirs.iter()
                .flat_map(|dir| {
                    [".zshenv", ".zprofile", ".zshrc", ".zlogin"].map(|file| dir.join(file))
                })
                .collect()
        }
        "bash" => home
            .iter()
            .flat_map(|dir| {
                [".bash_profile", ".bash_login", ".profile", ".bashrc"].map(|file| dir.join(file))
            })
            .collect(),
        "fish" => source
            .dir("XDG_CONFIG_HOME")
            .or_else(|| home.map(|dir| dir.join(".config")))
            .map(|dir| dir.join("fish/config.fish"))
            .into_iter()
            .collect(),
        _ => home.map(|dir| dir.join(".profile")).into_iter().collect(),
    }
}

fn stamp(files: Vec<PathBuf>) -> Stamp {
    files
        .into_iter()
        .map(|path| {
            let seen = std::fs::metadata(&path)
                .ok()
                .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
            (path, seen)
        })
        .collect()
}

async fn startup_stamp(source: &Source, captured_zdotdir: Option<PathBuf>) -> Stamp {
    let files = startup_files(source, captured_zdotdir);
    tokio::task::spawn_blocking(move || stamp(files))
        .await
        .unwrap_or_default()
}

/// What the last finished capture saw: it stands while all of this still
/// holds (and, for a failure, until [`RETRY_FAILED`] passes).
struct Settled {
    shell: String,
    stamp: Stamp,
    epoch: u64,
    ok: bool,
    at: Instant,
}

type Flight = futures::future::Shared<futures::future::BoxFuture<'static, ()>>;

#[derive(Default)]
struct Inner {
    /// The last capture that worked. It outlives a later failed one: a slow
    /// shell on a loaded host must not take variables away from agents.
    current: Option<Arc<ShellEnv>>,
    settled: Option<Settled>,
    /// The capture running now, shared by every spawn that needs it.
    flight: Option<(u64, Flight)>,
    /// Bumped by [`ShellEnvCache::invalidate`]: a capture from before it no
    /// longer stands.
    epoch: u64,
    flights: u64,
}

#[derive(Default)]
struct ShellEnvCache {
    inner: Mutex<Inner>,
}

impl ShellEnvCache {
    fn current(&self) -> Option<Arc<ShellEnv>> {
        crate::lock(&self.inner).current.clone()
    }

    fn invalidate(&self) {
        crate::lock(&self.inner).epoch += 1;
    }

    /// Make sure a capture for `source` stands, waiting at most `wait` for
    /// one that is running. The shell runs as its own task with its own
    /// budget, so a caller that gives up or goes away never strands it, and
    /// any number of spawns at once share one shell.
    async fn ready(self: &Arc<Self>, source: Source, wait: Duration, budget: Duration) {
        let zdotdir = self.current().and_then(|env| env.zdotdir.clone());
        // Under the same bound as the wait: a stalled home directory must
        // not hold a spawn here. What is captured stays in use meanwhile.
        let Ok(stamp) = tokio::time::timeout(wait, startup_stamp(&source, zdotdir.clone())).await
        else {
            return;
        };
        let (id, flight) = {
            let mut inner = crate::lock(&self.inner);
            if let Some(running) = &inner.flight {
                running.clone()
            } else {
                let stands = inner.settled.as_ref().is_some_and(|s| {
                    s.shell == source.shell
                        && s.stamp == stamp
                        && s.epoch == inner.epoch
                        && (s.ok || s.at.elapsed() < RETRY_FAILED)
                });
                if stands {
                    return;
                }
                inner.flights += 1;
                let id = inner.flights;
                let epoch = inner.epoch;
                let cache = Arc::clone(self);
                let task = tokio::spawn(async move {
                    let result = capture(&source, budget).await;
                    let (env, stamp) = match result {
                        Ok(captured) => {
                            let env = ShellEnv::from_capture(captured, &source.base);
                            // The startup files re-homed zsh: stamp the files
                            // it really read, or the next spawn would see a
                            // different list and capture again.
                            let stamp = if env.zdotdir != zdotdir {
                                startup_stamp(&source, env.zdotdir.clone()).await
                            } else {
                                stamp
                            };
                            tracing::info!(
                                shell = %source.shell,
                                variables = env.vars.len(),
                                "read the login shell's environment for agents"
                            );
                            (Some(env), stamp)
                        }
                        Err(reason) => {
                            tracing::warn!(
                                shell = %source.shell,
                                %reason,
                                "could not read the login shell's environment; agents start without what its rc files export"
                            );
                            (None, stamp)
                        }
                    };
                    let mut inner = crate::lock(&cache.inner);
                    inner.flight = None;
                    inner.settled = Some(Settled {
                        shell: source.shell.clone(),
                        stamp,
                        epoch,
                        ok: env.is_some(),
                        at: Instant::now(),
                    });
                    if let Some(env) = env {
                        inner.current = Some(Arc::new(env));
                    }
                });
                let flight: Flight = async move {
                    let _ = task.await;
                }
                .boxed()
                .shared();
                inner.flight = Some((id, flight.clone()));
                (id, flight)
            }
        };
        if tokio::time::timeout(wait, flight).await.is_ok() {
            // The task clears its own flight; this covers one that panicked.
            let mut inner = crate::lock(&self.inner);
            if inner
                .flight
                .as_ref()
                .is_some_and(|(running, _)| *running == id)
            {
                inner.flight = None;
            }
        }
    }
}

static CACHE: LazyLock<Arc<ShellEnvCache>> = LazyLock::new(Default::default);

/// Make sure the login shell's environment has been read, waiting a bounded
/// time for it. Cheap when it stands: a handful of stats off the reactor.
pub(crate) async fn ready() {
    // Tests never read the developer's own rc files: it would make their
    // spawns depend on the machine, and cost a login shell per test binary.
    if cfg!(test) {
        return;
    }
    CACHE.ready(Source::daemon(), WAIT, BUDGET).await;
}

/// Lay what is already captured over `env`, without waiting — for command
/// builders that can't await; their callers run [`ready`] first.
pub(crate) fn overlay_current(env: &mut Vec<(String, String)>) {
    if let Some(shell_env) = CACHE.current() {
        shell_env.overlay(env, &std::env::var("PATH").unwrap_or_default());
    }
}

/// [`ready`], then [`overlay_current`]: what every agent spawn calls once its
/// own environment is assembled.
pub(crate) async fn overlay(env: &mut Vec<(String, String)>) {
    ready().await;
    overlay_current(env);
}

/// The user asked to look again (Extensions' re-check, a detection retry,
/// the check after an install): the next spawn reads the shell afresh. Not
/// for the catalog's routine `?refresh=true`, which the launcher sends every
/// time it opens.
pub(crate) fn invalidate() {
    CACHE.invalidate();
}

#[cfg(test)]
#[path = "shell_env_tests.rs"]
mod tests;
