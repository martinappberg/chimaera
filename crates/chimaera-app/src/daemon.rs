//! Local daemon lifecycle: reuse the machine's running daemon when its
//! manifest checks out, else spawn our own executable detached in `--daemon`
//! mode. The daemon deliberately outlives the app — sessions are tmux-grade
//! and quitting the shell must never kill an agent mid-task.

use std::os::unix::process::CommandExt;
use std::time::Duration;

use anyhow::{bail, Context};
use chimaera_core::Manifest;

/// A reachable local daemon.
#[derive(Clone, Debug)]
pub struct LocalDaemon {
    pub port: u16,
    pub token: String,
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
pub async fn ensure_local_daemon() -> anyhow::Result<LocalDaemon> {
    if let Some(d) = probe().await {
        tracing::info!("attaching to running daemon on 127.0.0.1:{}", d.port);
        return Ok(d);
    }
    spawn_detached().context("failed to spawn the local daemon")?;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Some(d) = probe().await {
            tracing::info!("local daemon up on 127.0.0.1:{}", d.port);
            return Ok(d);
        }
    }
    bail!(
        "the local daemon did not come up within 15s — check {}",
        log_path().display()
    )
}

/// Manifest → live pid → authenticated health check, all three or nothing.
async fn probe() -> Option<LocalDaemon> {
    let m = Manifest::load().ok()??;
    if !m.is_alive() {
        return None;
    }
    let port = m.port;
    let token = m.token.clone();
    let ok = tokio::task::spawn_blocking(move || health_ok(port, &token))
        .await
        .unwrap_or(false);
    ok.then_some(LocalDaemon {
        port: m.port,
        token: m.token,
    })
}

/// GET /api/v1/health with the manifest token; any 200 counts.
fn health_ok(port: u16, token: &str) -> bool {
    ureq::get(&format!("http://127.0.0.1:{port}/api/v1/health"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(Duration::from_secs(2))
        .call()
        .is_ok()
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
