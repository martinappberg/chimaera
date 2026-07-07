//! Remote daemon orchestration over the system `ssh`: discovery, binary
//! install, daemon start, and port-forward tunnels. Shared by the CLI
//! (`chimaera connect`) and the native shell, so both speak the exact same
//! protocol to a host — including inheriting the user's `~/.ssh/config`
//! (ProxyJump, ControlMaster, 2FA) by never reimplementing the ssh client.

pub mod hosts;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use chimaera_core::Manifest;
use tokio::process::{Child, Command};

/// Where the connect flow currently is; consumers surface these however
/// fits (tracing lines in the CLI, progress events in the shell).
#[derive(Clone, Debug)]
pub enum Phase {
    /// Probing the host for a running daemon.
    Probing,
    /// Copying a chimaera binary to the host.
    Installing { binary: PathBuf },
    /// Starting the daemon on the host.
    Starting,
    /// Waiting for the local port-forward to come up.
    Tunneling { local_port: u16 },
}

/// Options for [`connect`].
#[derive(Default)]
pub struct ConnectOpts {
    /// Local port for the tunnel (defaults to the remote port if free).
    pub local_port: Option<u16>,
    /// Explicit binary to install on the host if chimaera is missing;
    /// otherwise `~/.chimaera/dist/` is searched for a matching build.
    pub binary: Option<PathBuf>,
}

/// A live port-forward to a remote daemon. Dropping it kills the ssh child
/// (but a ControlMaster-held forward survives — call [`Tunnel::close`] to
/// cancel it explicitly).
pub struct Tunnel {
    pub host: String,
    pub local_port: u16,
    pub manifest: Manifest,
    /// The forward was registered with an ssh ControlMaster and our child
    /// exited 0; the master holds the port, not `child`.
    pub mux_delegated: bool,
    child: Child,
}

impl Tunnel {
    /// The UI url for this tunnel. The host alias rides along so the UI can
    /// label the window with the name the user actually calls this machine.
    pub fn url(&self) -> String {
        format!(
            "http://127.0.0.1:{}/#token={}&host={}",
            self.local_port, self.manifest.token, self.host
        )
    }

    /// Wait for the tunnel child to exit (never returns for a healthy
    /// direct forward; returns quickly when delegated to a ControlMaster).
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    /// Whether the forwarded local port currently accepts connections.
    pub async fn is_up(&self) -> bool {
        tokio::net::TcpStream::connect(("127.0.0.1", self.local_port))
            .await
            .is_ok()
    }

    /// Kill the tunnel child and cancel any master-held forward so local
    /// ports don't leak past the session that opened them.
    pub async fn close(mut self) {
        self.child.kill().await.ok();
        let _ = Command::new("ssh")
            .args(["-O", "cancel", "-L"])
            .arg(format!(
                "{}:127.0.0.1:{}",
                self.local_port, self.manifest.port
            ))
            .arg(&self.host)
            .output()
            .await;
    }
}

/// Connect to the daemon on `host`, installing and starting it if needed,
/// and bring up a local port-forward. `progress` fires as phases begin.
pub async fn connect(
    host: &str,
    opts: ConnectOpts,
    progress: impl Fn(Phase),
) -> anyhow::Result<Tunnel> {
    progress(Phase::Probing);
    let manifest = match remote_manifest(host).await? {
        Some(m) if remote_alive(host, m.pid).await? => {
            tracing::info!("daemon already running on {host} (pid {})", m.pid);
            m
        }
        _ => {
            ensure_remote_binary(host, opts.binary.as_deref(), &progress).await?;
            progress(Phase::Starting);
            start_remote(host).await?
        }
    };

    let local_port = pick_local_port(opts.local_port, manifest.port)?;
    progress(Phase::Tunneling { local_port });
    let mut child = spawn_tunnel(host, local_port, manifest.port)?;
    let mux_delegated = wait_for_port(local_port, &mut child).await?;
    tracing::info!(
        "tunnel up: 127.0.0.1:{local_port} -> {host}:{}",
        manifest.port
    );

    Ok(Tunnel {
        host: host.to_string(),
        local_port,
        manifest,
        mux_delegated,
        child,
    })
}

/// Fetch and parse the remote manifest, if any.
pub async fn remote_manifest(host: &str) -> anyhow::Result<Option<Manifest>> {
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

/// Whether `pid` is alive on the host (signal 0 probe over ssh).
pub async fn remote_alive(host: &str, pid: u32) -> anyhow::Result<bool> {
    ssh_check(host, &format!("kill -0 {pid} 2>/dev/null")).await
}

/// `uname -sm` on the host, lowercased: e.g. `("linux", "x86_64")`.
pub async fn remote_target(host: &str) -> anyhow::Result<(String, String)> {
    let output = Command::new("ssh")
        .arg(host)
        .arg("uname -sm")
        .output()
        .await
        .context("failed to run ssh")?;
    if !output.status.success() {
        bail!(
            "could not detect the OS/arch of {host}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_lowercase();
    let mut parts = text.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(os), Some(arch)) => Ok((os.to_string(), arch.to_string())),
        _ => bail!("unexpected `uname -sm` output from {host}: {text:?}"),
    }
}

/// The local stash of deployable builds: `~/.chimaera/dist/`. Populated by
/// `just dist` (or by hand); searched when no explicit binary is given.
pub fn dist_dir() -> PathBuf {
    chimaera_core::data_dir().join("dist")
}

/// The expected dist file name for a remote target: static musl for linux
/// (no glibc roulette on clusters), plain names elsewhere.
pub fn dist_name(os: &str, arch: &str) -> String {
    match os {
        "linux" => format!("chimaera-{arch}-linux-musl"),
        other => format!("chimaera-{arch}-{other}"),
    }
}

/// Verify `$HOME/.chimaera/bin/chimaera` exists on the host, installing it
/// from the explicit `binary`, else from `~/.chimaera/dist/` by detected
/// target, erroring with instructions otherwise.
async fn ensure_remote_binary(
    host: &str,
    binary: Option<&Path>,
    progress: &impl Fn(Phase),
) -> anyhow::Result<()> {
    if ssh_check(host, "test -x $HOME/.chimaera/bin/chimaera").await? {
        return Ok(());
    }
    let path = match binary {
        Some(p) => {
            if !p.is_file() {
                bail!("binary {} does not exist", p.display());
            }
            p.to_path_buf()
        }
        None => {
            let (os, arch) = remote_target(host).await?;
            let candidate = dist_dir().join(dist_name(&os, &arch));
            if !candidate.is_file() {
                bail!(
                    "chimaera is not installed on {host} ({os}/{arch}) and no deployable \
                     build was found at {}.\n\
                     Provide one with either:\n\
                     \x20 just dist                 (in the chimaera repo: builds musl binaries \
                     into ~/.chimaera/dist)\n\
                     \x20 chimaera connect {host} --binary /path/to/chimaera-built-for-{host}",
                    candidate.display()
                );
            }
            candidate
        }
    };
    progress(Phase::Installing {
        binary: path.clone(),
    });
    tracing::info!("installing {} on {host}", path.display());
    ssh_run(host, "mkdir -p $HOME/.chimaera/bin").await?;
    let output = Command::new("scp")
        .arg(&path)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dist_names_map_targets() {
        assert_eq!(dist_name("linux", "x86_64"), "chimaera-x86_64-linux-musl");
        assert_eq!(dist_name("linux", "aarch64"), "chimaera-aarch64-linux-musl");
        assert_eq!(dist_name("darwin", "arm64"), "chimaera-arm64-darwin");
    }

    #[test]
    fn local_port_prefers_remote_port_when_free() {
        // Bind a port to force the fallback path.
        let holder = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let held = holder.local_addr().unwrap().port();
        let picked = pick_local_port(None, held).unwrap();
        assert_ne!(picked, held, "held port must not be picked");
        assert_eq!(pick_local_port(Some(4321), held).unwrap(), 4321);
    }
}
