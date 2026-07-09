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

    // Only clean up the manifest once the daemon is confirmed dead. Removing it
    // while the daemon is still running (ignored/slow SIGTERM) would make every
    // other client read "not running" while the daemon is alive and still
    // holding its port — and a fresh start could then collide on that port. The
    // manifest is the single source of truth for "is a local daemon running";
    // never remove it out from under a live daemon.
    if died {
        Manifest::remove()?;
        println!("stopped: pid {}", manifest.pid);
    } else {
        println!(
            "pid {} still running 5s after SIGTERM; leaving its manifest in place",
            manifest.pid
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A daemon that outlives SIGTERM must keep its manifest — removing it while
    /// the daemon is alive would make every other client read "not running"
    /// while it still holds its port.
    #[tokio::test]
    async fn kill_leaves_the_manifest_when_the_daemon_survives() {
        // Isolate all per-user state under a tmp CHIMAERA_HOME.
        let home = std::env::temp_dir().join(format!("chimaera-kill-test-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("CHIMAERA_HOME", &home);

        // A "daemon" that ignores SIGTERM (trap), so its pid stays alive.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "trap '' TERM; sleep 60"])
            .spawn()
            .expect("spawn sh");

        let manifest = Manifest {
            hostname: "testhost".into(),
            port: 59999,
            token: "t".into(),
            pid: child.id(),
            version: "0.0.0".into(),
            started_at: 0,
            build: None,
        };
        manifest.write().expect("write manifest");
        assert!(Manifest::load().unwrap().is_some());

        // kill SIGTERMs the (ignoring) pid, waits ~5s, and must LEAVE the
        // manifest because the daemon never died.
        run().await.expect("kill run");
        assert!(
            Manifest::load().unwrap().is_some(),
            "manifest must survive when the daemon ignores SIGTERM"
        );

        let _ = child.kill();
        let _ = child.wait();
        Manifest::remove().ok();
        std::env::remove_var("CHIMAERA_HOME");
        std::fs::remove_dir_all(&home).ok();
    }
}
