use std::time::Duration;

use anyhow::Context;
use chimaera_core::Manifest;
#[cfg(unix)]
use nix::sys::signal::{kill, Signal};
#[cfg(unix)]
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

    request_stop(&manifest).await?;
    let died = wait_for_exit(&manifest, Duration::from_secs(5)).await;
    #[cfg(windows)]
    let died = died || force_stop(&manifest).await;

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
        #[cfg(unix)]
        println!(
            "pid {} still running 5s after SIGTERM; leaving its manifest in place",
            manifest.pid
        );
        #[cfg(windows)]
        println!(
            "pid {} still running after it was ended; leaving its manifest in place",
            manifest.pid
        );
    }
    Ok(())
}

async fn wait_for_exit(manifest: &Manifest, grace: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if !manifest.is_alive() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[cfg(unix)]
async fn request_stop(manifest: &Manifest) -> anyhow::Result<()> {
    kill(Pid::from_raw(manifest.pid as i32), Signal::SIGTERM)
        .with_context(|| format!("failed to signal pid {}", manifest.pid))
}

/// Windows has no SIGTERM: the daemon's own stop route takes the same
/// graceful, session-preserving path. A daemon that can't take it (an older
/// build without the route, a wedged listener) is ended at once rather than
/// after a grace it will never use.
#[cfg(windows)]
async fn request_stop(manifest: &Manifest) -> anyhow::Result<()> {
    if let Err(e) = post_stop(manifest.port, &manifest.token).await {
        println!(
            "the daemon did not take the stop request ({e:#}); ending pid {}",
            manifest.pid
        );
        chimaera_core::proc::kill_pid(manifest.pid);
    }
    Ok(())
}

/// The fallback once the grace has passed: `TerminateProcess`, which hands
/// nothing off, so its sessions don't come back on the next start.
#[cfg(windows)]
async fn force_stop(manifest: &Manifest) -> bool {
    println!(
        "pid {} still running 5s after the stop request; ending it (its sessions are lost)",
        manifest.pid
    );
    chimaera_core::proc::kill_pid(manifest.pid);
    wait_for_exit(manifest, Duration::from_secs(2)).await
}

/// `POST /api/v1/stop` on the local daemon, answered 2xx. Plain HTTP/1.1 over
/// loopback: one bodiless request needs no client library, and the token
/// travels in a header, never argv.
#[cfg(any(windows, test))]
async fn post_stop(port: u16, token: &str) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        let request = format!(
            "POST /api/v1/stop HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
             Authorization: Bearer {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await?;
        // The status line is all that matters; cap the read so a misbehaving
        // listener can't make it unbounded.
        let mut head = Vec::new();
        stream.take(1024).read_to_end(&mut head).await?;
        anyhow::Ok(head)
    };
    let head = tokio::time::timeout(Duration::from_secs(5), exchange)
        .await
        .context("no answer within 5s")??;
    let head = String::from_utf8_lossy(&head);
    let code = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .context("not an HTTP answer")?;
    if !(200..300).contains(&code) {
        anyhow::bail!("the daemon answered {code}");
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
    #[cfg(unix)]
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
            slurm_job_id: None,
            runtime_leases: false,
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

    /// A one-shot loopback daemon stand-in: answers the first request with
    /// `status` and hands back what it was sent.
    async fn stub(status: &str) -> (u16, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let reply = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let served = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 512];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = conn.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..n]);
            }
            conn.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8(seen).unwrap()
        });
        (port, served)
    }

    #[tokio::test]
    async fn post_stop_sends_the_bearer_and_takes_a_2xx() {
        let (port, served) = stub("200 OK").await;
        post_stop(port, "tok").await.expect("a 200 is a stop");
        let request = served.await.unwrap();
        assert!(
            request.starts_with("POST /api/v1/stop HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(
            request.contains("\r\nAuthorization: Bearer tok\r\n"),
            "{request}"
        );

        let (port, _served) = stub("401 Unauthorized").await;
        let err = post_stop(port, "wrong")
            .await
            .expect_err("a 401 is no stop");
        assert!(err.to_string().contains("401"), "{err:#}");
    }

    /// Windows: the stop request goes to the daemon's route; a daemon that
    /// takes it but doesn't exit is ended after the grace and its manifest
    /// removed, and one that refuses it is ended at once. Another node's
    /// record is never touched. One test: all cases relocate `CHIMAERA_HOME`.
    #[cfg(windows)]
    #[tokio::test]
    async fn kill_stops_through_the_route_then_ends_the_pid() {
        let home = std::env::temp_dir().join(format!("chimaera-kill-test-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("CHIMAERA_HOME", &home);
        let manifest_for = |pid: u32, port: u16, hostname: String| Manifest {
            hostname,
            port,
            token: "t".into(),
            pid,
            version: "0.0.0".into(),
            started_at: 0,
            build: None,
            slurm_job_id: None,
            runtime_leases: false,
        };
        let sleeper = || {
            std::process::Command::new("ping")
                .args(["-n", "60", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("spawn ping")
        };
        let here = chimaera_core::this_node().expect("the test host has a name");

        let mut bystander = sleeper();
        manifest_for(bystander.id(), 1, "chimaera-other-login-node".into())
            .write()
            .unwrap();
        run().await.expect("kill run");
        assert!(
            bystander.try_wait().unwrap().is_none(),
            "another node's pid ended"
        );
        assert!(
            Manifest::load().unwrap().is_some(),
            "another node's manifest removed"
        );
        let _ = bystander.kill();
        let _ = bystander.wait();

        // Takes the request, never exits: ended after the grace.
        let mut daemon = sleeper();
        let (port, served) = stub("200 OK").await;
        manifest_for(daemon.id(), port, here.clone())
            .write()
            .unwrap();
        let started = std::time::Instant::now();
        run().await.expect("kill run");
        assert!(served.await.unwrap().starts_with("POST /api/v1/stop "));
        assert!(
            started.elapsed() >= Duration::from_secs(5),
            "ended before the grace"
        );
        assert!(daemon.wait().is_ok());
        assert!(
            Manifest::load().unwrap().is_none(),
            "a dead daemon's manifest stays"
        );

        // Refuses the request: ended at once.
        let mut daemon = sleeper();
        let (port, _served) = stub("404 Not Found").await;
        manifest_for(daemon.id(), port, here).write().unwrap();
        let started = std::time::Instant::now();
        run().await.expect("kill run");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited out a refused stop"
        );
        assert!(daemon.wait().is_ok());
        assert!(Manifest::load().unwrap().is_none());

        std::env::remove_var("CHIMAERA_HOME");
        std::fs::remove_dir_all(&home).ok();
    }
}
