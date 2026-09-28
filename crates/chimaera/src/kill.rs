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
    // A home shared across nodes (HPC login nodes) shows every node the same
    // manifest, but its pid names a process on the node that wrote it: here
    // it is an unrelated process (or none), so neither signal it nor call the
    // record stale.
    if !manifest.written_here() {
        println!(
            "the daemon is registered on {} (pid {}), not this node — stop it there, \
             or remove {} if that node is gone for good",
            manifest.hostname,
            manifest.pid,
            Manifest::path().display()
        );
        return Ok(());
    }
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
    /// while it still holds its port. And a manifest another node wrote (a
    /// home shared across login nodes) is neither signalled nor removed: its
    /// pid names a process there, not here. One test: both cases relocate
    /// `CHIMAERA_HOME`, which is process-global.
    #[tokio::test]
    async fn kill_leaves_the_manifest_when_the_daemon_survives() {
        // Isolate all per-user state under a tmp CHIMAERA_HOME.
        let home = std::env::temp_dir().join(format!("chimaera-kill-test-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("CHIMAERA_HOME", &home);
        let manifest_for = |pid: u32, hostname: String| Manifest {
            hostname,
            port: 59999,
            token: "t".into(),
            pid,
            version: "0.0.0".into(),
            started_at: 0,
            build: None,
        };

        // Another node's record whose pid happens to be a live process here
        // that SIGTERM would end.
        let mut bystander = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        manifest_for(bystander.id(), "chimaera-other-login-node".into())
            .write()
            .expect("write manifest");
        run().await.expect("kill run");
        // A signal would end it asynchronously: give it time to show.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            bystander.try_wait().unwrap().is_none(),
            "another node's pid must never be signalled here"
        );
        assert!(
            Manifest::load().unwrap().is_some(),
            "another node's manifest must never be removed here"
        );
        let _ = bystander.kill();
        let _ = bystander.wait();

        // A "daemon" that ignores SIGTERM (trap), so its pid stays alive.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "trap '' TERM; sleep 60"])
            .spawn()
            .expect("spawn sh");

        let here = chimaera_core::this_node().expect("the test host has a name");
        let manifest = manifest_for(child.id(), here);
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
