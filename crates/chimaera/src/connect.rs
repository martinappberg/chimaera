use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use chimaera_core::Manifest;
use tokio::process::{Child, Command};

pub async fn run(
    host: &str,
    local_port: Option<u16>,
    binary: Option<&Path>,
    no_open: bool,
) -> anyhow::Result<()> {
    let manifest = match remote_manifest(host).await? {
        Some(m) if remote_alive(host, m.pid).await? => {
            tracing::info!("daemon already running on {host} (pid {})", m.pid);
            m
        }
        _ => {
            ensure_remote_binary(host, binary).await?;
            start_remote(host).await?
        }
    };

    let local = pick_local_port(local_port, manifest.port)?;
    let mut tunnel = spawn_tunnel(host, local, manifest.port)?;
    let mux_delegated = wait_for_port(local, &mut tunnel).await?;
    tracing::info!("tunnel up: 127.0.0.1:{local} -> {host}:{}", manifest.port);

    let url = format!("http://127.0.0.1:{local}/#token={}", manifest.token);
    println!("{url}");
    if !no_open {
        if let Err(e) = open::that(&url) {
            tracing::warn!("failed to open browser: {e}");
        }
    }

    if mux_delegated {
        tracing::info!("forward held by ssh ControlMaster; press Ctrl-C to disconnect");
        tokio::signal::ctrl_c()
            .await
            .context("failed to listen for ctrl-c")?;
    } else {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result.context("failed to listen for ctrl-c")?;
                tracing::info!("shutting down tunnel");
            }
            status = tunnel.wait() => {
                let status = status.context("failed waiting on ssh tunnel")?;
                if status.success() {
                    tracing::info!("forward held by ssh ControlMaster; press Ctrl-C to disconnect");
                    tokio::signal::ctrl_c()
                        .await
                        .context("failed to listen for ctrl-c")?;
                } else {
                    bail!("ssh tunnel exited unexpectedly: {status}");
                }
            }
        }
    }
    tunnel.kill().await.ok();
    // A master-held forward outlives our child; cancel it so ports don't leak.
    let _ = Command::new("ssh")
        .args(["-O", "cancel", "-L"])
        .arg(format!("{local}:127.0.0.1:{}", manifest.port))
        .arg(host)
        .output()
        .await;
    Ok(())
}

/// Fetch and parse the remote manifest, if any.
async fn remote_manifest(host: &str) -> anyhow::Result<Option<Manifest>> {
    let output = Command::new("ssh")
        .arg(host)
        .arg("cat ~/.chimaera/manifest.json 2>/dev/null")
        .output()
        .await
        .context("failed to run ssh")?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(serde_json::from_str(text.trim()).ok())
}

async fn remote_alive(host: &str, pid: u32) -> anyhow::Result<bool> {
    ssh_check(host, &format!("kill -0 {pid} 2>/dev/null")).await
}

/// Verify `$HOME/.chimaera/bin/chimaera` exists on the host, installing it
/// from `binary` if given, erroring with instructions otherwise.
async fn ensure_remote_binary(host: &str, binary: Option<&Path>) -> anyhow::Result<()> {
    if ssh_check(host, "test -x $HOME/.chimaera/bin/chimaera").await? {
        return Ok(());
    }
    let Some(path) = binary else {
        bail!(
            "chimaera is not installed on {host} (expected $HOME/.chimaera/bin/chimaera).\n\
             Install it with one of:\n\
             \x20 chimaera connect {host} --binary /path/to/chimaera-built-for-{host}\n\
             \x20 ssh {host} 'mkdir -p ~/.chimaera/bin' && scp ./chimaera {host}:.chimaera/bin/ \
             && ssh {host} 'chmod +x ~/.chimaera/bin/chimaera'"
        );
    };
    tracing::info!("installing {} on {host}", path.display());
    ssh_run(host, "mkdir -p $HOME/.chimaera/bin").await?;
    let output = Command::new("scp")
        .arg(path)
        .arg(format!("{host}:.chimaera/bin/chimaera"))
        .output()
        .await
        .context("failed to run scp")?;
    if !output.status.success() {
        bail!(
            "scp to {host} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    ssh_run(host, "chmod +x $HOME/.chimaera/bin/chimaera").await?;
    Ok(())
}

/// Start the daemon on the host and poll until its manifest reports alive.
async fn start_remote(host: &str) -> anyhow::Result<Manifest> {
    tracing::info!("starting chimaera daemon on {host}");
    ssh_run(
        host,
        // `;` not `&&` before setsid: with `&&`, the trailing `&` backgrounds the whole
        // list and the daemon runs as the foreground child of a subshell whose
        // stdout/stderr are the ssh channel — sshd then never closes the session and
        // `connect` hangs forever. Found the hard way on a real cluster.
        "mkdir -p $HOME/.chimaera/logs; \
         setsid nohup $HOME/.chimaera/bin/chimaera serve \
         >> $HOME/.chimaera/logs/serve.log 2>&1 < /dev/null & disown",
    )
    .await?;
    for _ in 0..15 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some(m) = remote_manifest(host).await? {
            if remote_alive(host, m.pid).await? {
                return Ok(m);
            }
        }
    }
    bail!("daemon on {host} did not start within 15s (check ~/.chimaera/logs/serve.log there)");
}

/// Choose the local tunnel port: explicit flag, else the remote port if
/// locally free, else an OS-assigned free port.
fn pick_local_port(requested: Option<u16>, remote_port: u16) -> anyhow::Result<u16> {
    if let Some(port) = requested {
        return Ok(port);
    }
    if std::net::TcpListener::bind(("127.0.0.1", remote_port)).is_ok() {
        return Ok(remote_port);
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .context("failed to find a free local port")?;
    Ok(listener.local_addr()?.port())
}

fn spawn_tunnel(host: &str, local: u16, remote: u16) -> anyhow::Result<Child> {
    Command::new("ssh")
        .arg("-N")
        .arg("-L")
        .arg(format!("{local}:127.0.0.1:{remote}"))
        .arg(host)
        .kill_on_drop(true)
        .spawn()
        .context("failed to spawn ssh tunnel")
}

/// Poll the local tunnel port until it accepts connections (15s timeout).
/// Returns true if the forward was delegated to an ssh ControlMaster: the mux
/// client registers the forward with the master and exits 0 immediately, so a
/// zero exit here is success, not failure.
async fn wait_for_port(port: u16, tunnel: &mut Child) -> anyhow::Result<bool> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut mux_delegated = false;
    loop {
        if !mux_delegated {
            if let Some(status) = tunnel.try_wait()? {
                if status.success() {
                    mux_delegated = true;
                } else {
                    bail!("ssh tunnel exited early: {status}");
                }
            }
        }
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return Ok(mux_delegated);
        }
        if tokio::time::Instant::now() > deadline {
            bail!("tunnel did not come up on 127.0.0.1:{port} within 15s");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Run a remote command, treating its exit status as a boolean.
async fn ssh_check(host: &str, cmd: &str) -> anyhow::Result<bool> {
    let output = Command::new("ssh")
        .arg(host)
        .arg(cmd)
        .output()
        .await
        .context("failed to run ssh")?;
    Ok(output.status.success())
}

/// Run a remote command, failing loudly if it does not exit 0.
async fn ssh_run(host: &str, cmd: &str) -> anyhow::Result<()> {
    let output = Command::new("ssh")
        .arg(host)
        .arg(cmd)
        .output()
        .await
        .context("failed to run ssh")?;
    if !output.status.success() {
        bail!(
            "ssh {host} {cmd:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
