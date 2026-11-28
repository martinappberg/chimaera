//! Local daemon lifecycle: reuse the machine's running daemon when its
//! manifest checks out, else spawn our own executable detached in `--daemon`
//! mode. The daemon deliberately outlives the app — sessions are tmux-grade
//! and quitting the shell must never kill an agent mid-task.
//!
//! Build parity (the local half of daemon self-update on connect): a running
//! daemon whose manifest build differs from this app's is replaced at
//! startup when it is provably idle — graceful stop, respawn — and attached
//! as `outdated` otherwise, so the home screen can offer the explicit
//! update. Same rules as the remote flow, no ssh: the manifest is on disk
//! and the session count comes straight off 127.0.0.1.

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::time::Duration;

use anyhow::{bail, Context};
use chimaera_core::Manifest;
use chimaera_remote::Decision;

/// A reachable local daemon.
#[derive(Clone, Debug)]
pub struct LocalDaemon {
    pub port: u16,
    pub token: String,
    /// Build id from the daemon's manifest; `None` = a pre-build-id daemon.
    pub build: Option<String>,
    /// The daemon is an older build than this app, left running because
    /// live sessions (or an unknown count) made replacing it unsafe.
    pub outdated: bool,
    /// Live sessions counted when `outdated` was decided.
    pub live_sessions: Option<usize>,
}

/// Headless entry point for `chimaera-app --daemon`.
pub fn run_headless() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    // Same shape as `chimaera serve` (crates/chimaera/src/main.rs): the
    // daemon measures <1 core steady-state, so four workers carry the async
    // side and the blocking pool takes file walks, renders and NFS stats.
    // Two daemons on two runtime shapes would make starvation bugs reproduce
    // on one and not the other.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(128)
        .enable_all()
        .build()
        .expect("failed to start tokio runtime");
    if let Err(e) = runtime.block_on(chimaera_server::run(chimaera_server::ServerConfig {
        port: None,
        // The app's local daemon is never a compute-node daemon.
        routable_bind: false,
    })) {
        eprintln!("daemon exited with error: {e:#}");
        std::process::exit(1);
    }
}

/// Find a live local daemon or start one, returning its port and token.
/// A running daemon of a different build is replaced when idle (same
/// decision policy as the remote connect flow) and attached as `outdated`
/// when live sessions make replacing it unsafe.
pub async fn ensure_local_daemon() -> anyhow::Result<LocalDaemon> {
    if let Some(m) = probe().await {
        let local_build = chimaera_core::BUILD_ID;
        // The session count only matters when the builds differ.
        let sessions = if chimaera_core::builds_match(local_build, m.build.as_deref()) {
            None
        } else {
            live_session_count(m.port, &m.token).await
        };
        match chimaera_remote::update_decision(local_build, m.build.as_deref(), sessions, false) {
            Decision::Reuse => {
                tracing::info!("attaching to running daemon on 127.0.0.1:{}", m.port);
                return Ok(attached(m, false, sessions));
            }
            Decision::Update => {
                tracing::info!(
                    "local daemon (build {}) is not ours ({local_build}) and has no live \
                     sessions — replacing it",
                    m.build.as_deref().unwrap_or("pre-build-id"),
                );
                stop_local(&m).await?;
                // Fall through to the spawn below.
            }
            Decision::ConnectOutdated => {
                tracing::warn!(
                    "local daemon (build {}) is older than this app ({local_build}) but has {} — \
                     attaching to it; the home screen offers the update",
                    m.build.as_deref().unwrap_or("pre-build-id"),
                    sessions.map_or("an unknown session count".to_string(), |n| format!(
                        "{n} live session{}",
                        if n == 1 { "" } else { "s" }
                    )),
                );
                return Ok(attached(m, true, sessions));
            }
        }
    }
    spawn_detached().context("failed to spawn the local daemon")?;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Some(m) = probe().await {
            tracing::info!("local daemon up on 127.0.0.1:{}", m.port);
            return Ok(attached(m, false, None));
        }
    }
    bail!(
        "the local daemon did not come up within 15s — check {}",
        log_path().display()
    )
}

/// Why an explicit local-daemon update failed, which decides what is left
/// running: the old daemon (the stop failed), or nothing yet.
#[derive(Debug)]
pub enum UpdateFailure {
    /// The old daemon refused to stop and is still serving.
    StopFailed(anyhow::Error),
    /// The old daemon stopped (or was never found) and no new one came up.
    RespawnFailed(anyhow::Error),
}

/// A failed update, after recovery: `kept_old` says the app's current
/// `LocalDaemon` is still the live one; otherwise no local daemon answered
/// even after one more start, and the old port is dead.
#[derive(Debug)]
pub struct UpdateError {
    pub kept_old: bool,
    pub message: String,
}

/// Explicit local-daemon update (the home screen affordance): gracefully
/// stop whatever is running — regardless of session count; the affordance
/// says what it ends — and bring up a fresh daemon of our build. A respawn
/// that fails after the stop gets one more `ensure_local_daemon`, so the app
/// is never left pointing at a stopped daemon's port without saying so.
pub async fn update_local_daemon() -> Result<LocalDaemon, UpdateError> {
    recover_update(update_once(), ensure_local_daemon).await
}

/// The recovery policy, over injectable steps (tested with fakes).
pub(crate) async fn recover_update<U, E, F>(
    update: U,
    ensure: E,
) -> Result<LocalDaemon, UpdateError>
where
    U: std::future::Future<Output = Result<LocalDaemon, UpdateFailure>>,
    E: FnOnce() -> F,
    F: std::future::Future<Output = anyhow::Result<LocalDaemon>>,
{
    match update.await {
        Ok(fresh) => Ok(fresh),
        Err(UpdateFailure::StopFailed(e)) => Err(UpdateError {
            kept_old: true,
            message: format!("{e:#}"),
        }),
        Err(UpdateFailure::RespawnFailed(first)) => {
            tracing::warn!("local daemon did not come back after the update ({first:#}); retrying");
            ensure().await.map_err(|second| UpdateError {
                kept_old: false,
                message: format!(
                    "the local daemon stopped for the update and could not be started again \
                     ({second:#}); quit and reopen chimaera"
                ),
            })
        }
    }
}

async fn update_once() -> Result<LocalDaemon, UpdateFailure> {
    if let Some(m) = probe().await {
        if chimaera_core::builds_match(chimaera_core::BUILD_ID, m.build.as_deref()) {
            // Already ours (updated elsewhere since the UI last looked).
            return Ok(attached(m, false, None));
        }
        stop_local(&m).await.map_err(UpdateFailure::StopFailed)?;
    }
    ensure_local_daemon()
        .await
        .map_err(UpdateFailure::RespawnFailed)
}

/// Manifest → LocalDaemon.
fn attached(m: Manifest, outdated: bool, live_sessions: Option<usize>) -> LocalDaemon {
    LocalDaemon {
        port: m.port,
        token: m.token,
        build: m.build,
        outdated,
        live_sessions,
    }
}

/// Manifest → live pid → authenticated health check, all three or nothing.
async fn probe() -> Option<Manifest> {
    let m = Manifest::load().ok()??;
    if !m.is_alive() {
        return None;
    }
    let port = m.port;
    let token = m.token.clone();
    let ok = tokio::task::spawn_blocking(move || health_ok(port, &token))
        .await
        .unwrap_or(false);
    ok.then_some(m)
}

/// GET /api/v1/health with the manifest token; any 200 counts.
fn health_ok(port: u16, token: &str) -> bool {
    crate::http::agent()
        .get(&format!("http://127.0.0.1:{port}/api/v1/health"))
        .header("Authorization", &format!("Bearer {token}"))
        .config()
        .timeout_global(Some(Duration::from_secs(2)))
        .build()
        .call()
        .is_ok()
}

/// Live session count straight off the local daemon (loopback + manifest
/// token, no ssh). `None` = could not determine; callers treat that as
/// busy, never as zero.
pub(crate) async fn live_session_count(port: u16, token: &str) -> Option<usize> {
    let token = token.to_string();
    tokio::task::spawn_blocking(move || {
        let mut response = crate::http::agent()
            .get(&format!("http://127.0.0.1:{port}/api/v1/sessions"))
            .header("Authorization", &format!("Bearer {token}"))
            .config()
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .call()
            .ok()?;
        let body = response.body_mut().read_to_string().ok()?;
        chimaera_remote::count_alive_sessions(&body)
    })
    .await
    .unwrap_or(None)
}

/// Gracefully stop the local daemon: SIGTERM, then poll for exit for up to
/// ~10s. Never escalates to SIGKILL — a daemon that will not die may be
/// holding sessions that must not be torn out from under their owner.
#[cfg(unix)]
async fn stop_local(m: &Manifest) -> anyhow::Result<()> {
    tracing::info!("stopping local daemon (pid {})", m.pid);
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(m.pid as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .with_context(|| format!("failed to signal pid {}", m.pid))?;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !m.is_alive() {
            return Ok(());
        }
    }
    bail!(
        "local daemon (pid {}) is still running 10s after SIGTERM — refusing to kill -9 it; \
         something is keeping it busy (open browser tabs hold its sockets). Close them and retry.",
        m.pid
    )
}

/// Windows: no signal reaches a console-less daemon, so the stop is asked
/// for in-band — `POST /api/v1/stop`, the same graceful path SIGTERM takes
/// (sessions handed off for the next start) — then polled for exit like the
/// unix one. A daemon still alive after that, or one that refused the
/// request, is terminated: unlike a unix daemon it cannot be left running
/// while its executable is replaced, and every caller has already decided
/// that its sessions may end.
#[cfg(windows)]
async fn stop_local(m: &Manifest) -> anyhow::Result<()> {
    tracing::info!("stopping local daemon (pid {})", m.pid);
    let (port, token) = (m.port, m.token.clone());
    let asked = tokio::task::spawn_blocking(move || request_stop(port, &token))
        .await
        .unwrap_or(false);
    if asked && exited_within(m, Duration::from_secs(10)).await {
        return Ok(());
    }
    tracing::warn!(
        "local daemon (pid {}) did not stop {}; terminating it",
        m.pid,
        if asked { "within 10s" } else { "on request" }
    );
    chimaera_core::proc::kill_pid(m.pid);
    if exited_within(m, Duration::from_secs(5)).await {
        return Ok(());
    }
    bail!(
        "local daemon (pid {}) is still running after it was terminated",
        m.pid
    )
}

/// `POST /api/v1/stop` with the manifest token; true when the daemon took it.
#[cfg(windows)]
fn request_stop(port: u16, token: &str) -> bool {
    crate::http::agent()
        .post(&format!("http://127.0.0.1:{port}/api/v1/stop"))
        .header("Authorization", &format!("Bearer {token}"))
        .config()
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .send_empty()
        .is_ok()
}

#[cfg(windows)]
async fn exited_within(m: &Manifest, limit: Duration) -> bool {
    let step = Duration::from_millis(100);
    for _ in 0..(limit.as_millis() / step.as_millis()) {
        if !m.is_alive() {
            return true;
        }
        tokio::time::sleep(step).await;
    }
    !m.is_alive()
}

/// Stop the running local daemon, if any, so its executable can be replaced
/// (Windows locks a running image; the installer would fail or kill it
/// without the handoff).
#[cfg(windows)]
pub async fn stop_for_install() -> anyhow::Result<()> {
    match probe().await {
        Some(m) => stop_local(&m).await,
        None => Ok(()),
    }
}

fn log_path() -> std::path::PathBuf {
    chimaera_core::data_dir().join("logs").join("serve.log")
}

/// Spawn our own executable as `--daemon`, in a new session (Windows: a new
/// process group with no console) with stdio on the serve log, so it
/// survives the shell quitting.
fn spawn_detached() -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let log = log_path();
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("failed to open {}", log.display()))?;
    let err = out.try_clone()?;
    tracing::info!("starting local daemon: {} --daemon", exe.display());
    let mut cmd = daemon_command(&exe, crate::askpass::user_askpass_env());
    cmd.stdout(out).stderr(err);
    #[cfg(unix)]
    cmd.spawn()?;
    #[cfg(windows)]
    chimaera_core::proc::spawn_detached(&mut cmd)?;
    Ok(())
}

/// The daemon's command line and environment. The app exports its askpass
/// wiring for its OWN ssh children; the daemon (and every terminal and agent
/// it starts) must not inherit it, or their ssh would prompt through the
/// app's modal and any agent could raise a native prompt for a real host.
/// The user's own askpass settings from before the app's (`user`) go back.
fn daemon_command(exe: &std::path::Path, user: &[(String, String)]) -> std::process::Command {
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--daemon").stdin(std::process::Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(chimaera_core::proc::DETACHED_CREATION_FLAGS);
    for key in crate::askpass::APP_ASKPASS_ENV {
        cmd.env_remove(key);
    }
    for (key, value) in user {
        cmd.env(key, value);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon(port: u16) -> LocalDaemon {
        LocalDaemon {
            port,
            token: "t".into(),
            build: None,
            outdated: false,
            live_sessions: None,
        }
    }

    #[test]
    fn the_daemon_never_inherits_the_apps_askpass_wiring() {
        let cmd = daemon_command(std::path::Path::new("/opt/chimaera"), &[]);
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();
        for key in crate::askpass::APP_ASKPASS_ENV {
            assert_eq!(
                envs.get(std::ffi::OsStr::new(key)),
                Some(&None),
                "{key} must be removed"
            );
        }
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["--daemon"]);
    }

    #[test]
    fn the_users_own_askpass_reaches_the_daemon_instead_of_the_apps() {
        let user = [
            (
                "SSH_ASKPASS".to_string(),
                "/usr/libexec/ssh-askpass".to_string(),
            ),
            ("SSH_ASKPASS_REQUIRE".to_string(), "prefer".to_string()),
        ];
        let cmd = daemon_command(std::path::Path::new("/opt/chimaera"), &user);
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();
        let get = |k: &str| envs.get(std::ffi::OsStr::new(k)).cloned();
        assert_eq!(
            get("SSH_ASKPASS"),
            Some(Some(std::ffi::OsStr::new("/usr/libexec/ssh-askpass")))
        );
        assert_eq!(
            get("SSH_ASKPASS_REQUIRE"),
            Some(Some(std::ffi::OsStr::new("prefer")))
        );
        for key in crate::askpass::APP_ASKPASS_ENV
            .iter()
            .filter(|k| !k.starts_with("SSH_ASKPASS"))
        {
            assert_eq!(get(key), Some(None), "{key} must be removed");
        }
    }

    #[tokio::test]
    async fn a_failed_respawn_is_retried_once_and_a_failed_stop_keeps_the_old_daemon() {
        // Stop refused: the old daemon still serves; nothing is restarted.
        let ensured = std::cell::Cell::new(false);
        let out = recover_update(
            async { Err(UpdateFailure::StopFailed(anyhow::anyhow!("busy"))) },
            || {
                ensured.set(true);
                async { Ok(daemon(2)) }
            },
        )
        .await
        .unwrap_err();
        assert!(out.kept_old);
        assert!(!ensured.get(), "a running old daemon is not replaced again");

        // Stopped, respawn failed, the retry brings one up: use it.
        let out = recover_update(
            async { Err(UpdateFailure::RespawnFailed(anyhow::anyhow!("slow"))) },
            || async { Ok(daemon(3)) },
        )
        .await
        .unwrap();
        assert_eq!(out.port, 3);

        // Stopped and nothing comes back: say so, never "the old one serves".
        let out = recover_update(
            async { Err(UpdateFailure::RespawnFailed(anyhow::anyhow!("slow"))) },
            || async { Err(anyhow::anyhow!("still down")) },
        )
        .await
        .unwrap_err();
        assert!(!out.kept_old);
        assert!(
            out.message.contains("could not be started again"),
            "{}",
            out.message
        );

        let out = recover_update(async { Ok(daemon(4)) }, || async {
            Err(anyhow::anyhow!("unused"))
        })
        .await
        .unwrap();
        assert_eq!(out.port, 4);
    }
}
