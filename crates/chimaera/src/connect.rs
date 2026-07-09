use std::path::Path;

use anyhow::{bail, Context};
use chimaera_remote::{connect, hosts::HostsStore, ConnectOpts, Phase};

pub async fn run(
    host: &str,
    local_port: Option<u16>,
    binary: Option<&Path>,
    no_open: bool,
    update_daemon: bool,
) -> anyhow::Result<()> {
    // Dev is dev: an unstamped build always targets ~/.chimaera-dev on the
    // host (and defaults its own state there) — say so, since the real
    // daemon next to it stays untouched and unreported.
    if chimaera_core::is_dev_build() {
        tracing::info!(
            "dev build: targeting the isolated ~/.chimaera-dev daemon on {host} \
             (the real ~/.chimaera daemon is left untouched)"
        );
    }
    let opts = ConnectOpts {
        local_port,
        binary: binary.map(Path::to_path_buf),
        update_daemon,
    };
    let mut tunnel = connect(host, opts, |phase| match phase {
        Phase::Probing => tracing::info!("probing {host} for a running daemon"),
        Phase::Updating => tracing::info!("updating the daemon on {host}"),
        Phase::Downloading { target } => {
            tracing::info!("downloading the {target} daemon for {host}");
        }
        Phase::Installing { binary } => {
            tracing::info!("installing {} on {host}", binary.display());
        }
        Phase::Starting => tracing::info!("starting chimaera daemon on {host}"),
        Phase::Tunneling { local_port } => {
            tracing::info!("forwarding 127.0.0.1:{local_port} to {host}");
        }
    })
    .await?;

    if tunnel.outdated {
        let sessions = match tunnel.live_sessions {
            Some(n) => format!("{n} session{} running", if n == 1 { "" } else { "s" }),
            None => "session count unknown".to_string(),
        };
        tracing::warn!(
            "remote daemon build {} is older than yours ({}); {sessions} — \
             rerun with --update-daemon to replace it",
            tunnel.remote_build.as_deref().unwrap_or("pre-build-id"),
            chimaera_core::BUILD_ID,
        );
    }

    // Remember the host so the native shell's home screen can offer it.
    if let Err(e) = HostsStore::load_default().record_connected(host) {
        tracing::debug!("could not record host {host}: {e}");
    }

    let url = tunnel.url();
    println!("{url}");
    if !no_open {
        if let Err(e) = open::that(&url) {
            tracing::warn!("failed to open browser: {e}");
        }
    }

    if tunnel.mux_delegated {
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
    tunnel.close().await;
    Ok(())
}
