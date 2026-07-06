use std::time::Duration;

use anyhow::Context;
use chimaera_core::Manifest;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

pub async fn run() -> anyhow::Result<()> {
    let Some(manifest) = Manifest::load()? else {
        println!("not running");
        return Ok(());
    };
    if !manifest.is_alive() {
        Manifest::remove()?;
        println!("stale manifest (pid {} dead), removed", manifest.pid);
        return Ok(());
    }

    kill(Pid::from_raw(manifest.pid as i32), Signal::SIGTERM)
        .with_context(|| format!("failed to signal pid {}", manifest.pid))?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut died = false;
    while tokio::time::Instant::now() < deadline {
        if !manifest.is_alive() {
            died = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Manifest::remove()?;
    if died {
        println!("stopped: pid {}", manifest.pid);
    } else {
        println!(
            "pid {} still running 5s after SIGTERM (manifest removed anyway)",
            manifest.pid
        );
    }
    Ok(())
}
