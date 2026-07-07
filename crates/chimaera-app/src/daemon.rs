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

use std::os::unix::process::CommandExt;
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
    let runtime = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
    if let Err(e) = runtime.block_on(chimaera_server::run(chimaera_server::ServerConfig {
        port: None,
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

/// Explicit local-daemon update (the home screen affordance): gracefully
/// stop whatever is running — regardless of session count; the affordance
/// says what it ends — and bring up a fresh daemon of our build.
pub async fn update_local_daemon() -> anyhow::Result<LocalDaemon> {
    if let Some(m) = probe().await {
        if chimaera_core::builds_match(chimaera_core::BUILD_ID, m.build.as_deref()) {
            // Already ours (updated elsewhere since the UI last looked).
            return Ok(attached(m, false, None));
        }
        stop_local(&m).await?;
    }
    ensure_local_daemon().await
}

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
    ureq::get(&format!("http://127.0.0.1:{port}/api/v1/health"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(Duration::from_secs(2))
        .call()
        .is_ok()
}

/// Live session count straight off the local daemon (loopback + manifest
/// token, no ssh). `None` = could not determine; callers treat that as
/// busy, never as zero.
async fn live_session_count(port: u16, token: &str) -> Option<usize> {
    let token = token.to_string();
    tokio::task::spawn_blocking(move || {
        let body = ureq::get(&format!("http://127.0.0.1:{port}/api/v1/sessions"))
            .set("Authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(5))
            .call()
            .ok()?
            .into_string()
            .ok()?;
        chimaera_remote::count_alive_sessions(&body)
    })
    .await
    .unwrap_or(None)
}

/// Gracefully stop the local daemon: SIGTERM, then poll for exit for up to
/// ~10s. Never escalates to SIGKILL — a daemon that will not die may be
/// holding sessions that must not be torn out from under their owner.
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

fn log_path() -> std::path::PathBuf {
    chimaera_core::data_dir().join("logs").join("serve.log")
}

/// Spawn our own executable as `--daemon`, in a new session with stdio on
/// the serve log, so it survives the shell quitting.
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
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--daemon")
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0);
    cmd.spawn()?;
    Ok(())
}
