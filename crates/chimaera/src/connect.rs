use std::path::Path;

use anyhow::{bail, Context};
use chimaera_remote::{
    connect, hosts::HostsStore, ClusterHost, ConnectOpts, DeploymentSource, Phase,
};

pub async fn run(
    host: &str,
    local_port: Option<u16>,
    binary: Option<&Path>,
    no_open: bool,
    update_daemon: bool,
    login_node: bool,
    deployment_source: DeploymentSource,
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
    // The login-node override is per host and shared with the app: a flag
    // given here is remembered, and one set in the app applies here too.
    let entry = HostsStore::load_default().get(host);
    let saved = entry.as_ref().is_some_and(|h| h.login_serve);
    // "Not a cluster" (set in the app) holds here too.
    let not_cluster = entry.as_ref().is_some_and(|h| h.not_cluster);
    if login_node && !saved {
        if let Err(e) = HostsStore::load_default().set_login_serve(host, true) {
            tracing::debug!("could not remember the login-node override for {host}: {e}");
        }
    }
    let opts = ConnectOpts {
        local_port,
        binary: binary.map(Path::to_path_buf),
        deployment_source,
        update_daemon,
        login_serve: login_node || saved,
        not_cluster,
    };
    let connected = connect(host, opts, |phase| match phase {
        Phase::Probing => tracing::info!("probing {host} for a running daemon"),
        Phase::Routing { node } => tracing::info!(
            "{host}'s daemon runs on login node {node}, not the one this connection landed on; \
             reaching {node} (it may ask you to authenticate there)"
        ),
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
    .await;
    let mut tunnel = match connected {
        Ok(tunnel) => tunnel,
        Err(e) => {
            let Some(cluster) = e.downcast_ref::<ClusterHost>() else {
                return Err(e);
            };
            if let Err(e) = HostsStore::load_default().record_scheduler(host, cluster.scheduler) {
                tracing::debug!("could not record {host}'s scheduler: {e}");
            }
            let mut msg = format!(
                "{host} is a {} cluster, so chimaera doesn't run on its login nodes — \
                 each workspace runs as its own job instead:\n  \
                 chimaera compute list {host}\n  \
                 chimaera compute add {host} <path>\n  \
                 chimaera compute start {host} <workspace> --time 4:00:00\n  \
                 chimaera compute open {host} <workspace>\n\
                 If your cluster's admins allow servers on login nodes, rerun with --login-node.",
                cluster.scheduler.tag()
            );
            if let Some(d) = &cluster.login_daemon {
                msg.push_str(&format!(
                    "\nA chimaera daemon from before is registered on {} (pid {}); stop it there \
                     with `chimaera kill`.",
                    d.node, d.pid
                ));
            }
            bail!(msg);
        }
    };

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
