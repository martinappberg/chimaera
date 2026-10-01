//! `chimaera compute …` — cluster workspaces from the CLI: list, add, start,
//! open, stop. Every command is a short ssh exec through the ControlMaster
//! (`chimaera_remote::cluster`); nothing runs on the login node, and each
//! workspace's chimaera runs inside its own Slurm job. Thin by design: the
//! verification harness and the app-parity surface, not a second
//! implementation.

use anyhow::{bail, Context};
use chimaera_core::slurm::{LaunchSpec, Scheduler};
use chimaera_remote::cluster::{self, StartOutcome, StartRefused, StartRequest};
use chimaera_remote::RemoteHome;

/// Reach the host (may ask to authenticate) and require Slurm on it.
async fn slurm_host(host: &str) -> anyhow::Result<String> {
    let host = chimaera_remote::hosts::normalize_alias(host)?;
    let info = chimaera_remote::detect_scheduler(&host, RemoteHome::current()).await?;
    match info.kind {
        Scheduler::Slurm => Ok(host),
        Scheduler::None => bail!("{host} has no batch scheduler on its login PATH"),
        other => bail!(
            "{host} runs {}; starting workspaces is only supported on Slurm so far",
            other.tag()
        ),
    }
}

/// A workspace by id or (case-insensitive) name.
fn pick<'a>(
    ov: &'a cluster::ClusterOverview,
    which: &str,
) -> anyhow::Result<&'a cluster::WorkspaceView> {
    ov.workspaces
        .iter()
        .find(|w| w.id == which)
        .or_else(|| {
            ov.workspaces
                .iter()
                .find(|w| w.name.eq_ignore_ascii_case(which))
        })
        .with_context(|| {
            format!("no workspace {which:?} on this cluster — `chimaera compute list` shows them")
        })
}

fn left(ends_at_ms: Option<u64>, now_ms: u64) -> String {
    match ends_at_ms {
        Some(end) if end > now_ms => {
            let mins = (end - now_ms) / 60_000;
            match (mins / 1440, (mins % 1440) / 60, mins % 60) {
                (0, 0, m) => format!("{m}m left"),
                (0, h, m) => format!("{h}h {m}m left"),
                (d, h, _) => format!("{d}d {h}h left"),
            }
        }
        _ => String::new(),
    }
}

pub async fn list(host: &str) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let ov = cluster::overview(&host, RemoteHome::current()).await?;
    if ov.workspaces.is_empty() {
        println!("no workspaces on {host} yet — add one with `chimaera compute add {host} <path>`");
    }
    for w in &ov.workspaces {
        let detail = match w.state {
            "running" => format!(
                "on {} · {} CPU · {} · {}",
                w.node,
                w.cpus,
                w.mem,
                left(w.ends_at_ms, ov.now_ms)
            ),
            "starting" => format!("starting on {}", w.node),
            "waiting" if !w.reason.is_empty() => format!("waiting for a node ({})", w.reason),
            "waiting" => "waiting for a node".to_string(),
            _ if w.fresh => "not started yet".to_string(),
            _ if w.stopped_by_user => "stopped by you".to_string(),
            _ => format!(
                "stopped ({})",
                w.ended.as_deref().unwrap_or("ended").to_lowercase()
            ),
        };
        println!(
            "{}  {:<20} {:<9} {}  [{}]{}",
            w.id,
            w.name,
            w.state,
            detail,
            w.path,
            if w.attached {
                " · stops when its connection ends"
            } else {
                ""
            }
        );
    }
    if ov.other_jobs.running + ov.other_jobs.waiting > 0 {
        println!(
            "your other jobs on {host}: {} running · {} waiting",
            ov.other_jobs.running, ov.other_jobs.waiting
        );
    }
    if ov.degraded {
        println!("(the queue didn't answer; states are from the last read)");
    }
    Ok(())
}

pub async fn add(host: &str, path: &str, name: Option<&str>) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let ws = cluster::add_workspace(&host, RemoteHome::current(), path, name.unwrap_or("")).await?;
    println!("added {} ({}) at {}", ws.name, ws.id, ws.path);
    Ok(())
}

#[allow(clippy::too_many_arguments)] // one flag per LaunchSpec field
pub async fn start(
    host: &str,
    which: &str,
    spec: LaunchSpec,
    run_startup: Option<&str>,
    attached: bool,
    save_as: Option<&str>,
) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let home = RemoteHome::current();
    let ov = cluster::overview(&host, home).await?;
    let view = pick(&ov, which)?;
    anyhow::ensure!(
        view.state == "stopped",
        "{} is already {} — stop it first",
        view.name,
        view.state
    );
    let ws = ov
        .config
        .workspaces
        .iter()
        .find(|w| w.id == view.id)
        .context("the workspace list changed meanwhile")?
        .clone();
    let spec = spec.normalized();
    spec.validate().map_err(anyhow::Error::msg)?;
    let facts = cluster::facts(&host, false).await?;
    cluster::ensure_cluster_binary(&host, home, None, &|phase| {
        if let chimaera_remote::Phase::Downloading { target } = phase {
            tracing::info!("downloading the {target} chimaera binary");
        }
    })
    .await?;
    let req = StartRequest {
        workspace: &ws,
        spec: &spec,
        run_startup: run_startup.unwrap_or(""),
        attached,
        gpu_flag: facts.gpu_flag,
    };
    match cluster::start(&host, home, &ov.config, &req).await {
        Ok(StartOutcome::Submitted { job_id }) => {
            cluster::remember_spec(&host, home, &ws.id, &spec, save_as).await?;
            println!(
                "submitted {} as job {job_id} — `chimaera compute list {host}` to watch, \
                 `chimaera compute open {host} {}` once it runs",
                ws.name, ws.name
            );
            Ok(())
        }
        Ok(StartOutcome::Attached { job_name }) => {
            cluster::remember_spec(&host, home, &ws.id, &spec, save_as).await?;
            let mut child =
                cluster::spawn_attached(&host, home, &ws.id, &spec, &job_name, facts.gpu_flag)?;
            println!(
                "{} starts attached to this terminal: it stops when you press Ctrl-C or this \
                 connection ends. Open it from another terminal with `chimaera compute open {host} {}`.",
                ws.name, ws.name
            );
            tokio::select! {
                r = tokio::signal::ctrl_c() => { r.context("failed to listen for ctrl-c")?; }
                status = child.wait() => {
                    let status = status.context("the attached job's connection failed")?;
                    println!("the attached job ended ({status})");
                }
            }
            Ok(())
        }
        Err(e) => match e.downcast_ref::<StartRefused>() {
            Some(refused) => {
                let partition = spec.partition.as_deref();
                cluster::learn_refusal(&host, home, partition, refused.kind)
                    .await
                    .ok();
                let hint = match refused.kind {
                    chimaera_core::slurm::Refusal::BatchNotAllowed => {
                        "\nthat partition takes only interactive jobs — rerun with --attached \
                         (it stops when this terminal's connection ends)"
                    }
                    chimaera_core::slurm::Refusal::AccountRequired => "\nrerun with --account",
                    chimaera_core::slurm::Refusal::QosRequired => "\nrerun with --qos",
                    chimaera_core::slurm::Refusal::ConstraintRequired => {
                        "\nrerun with --constraint"
                    }
                    chimaera_core::slurm::Refusal::Other => "",
                };
                bail!("the cluster refused the job: {}{hint}", refused.message)
            }
            None => Err(e),
        },
    }
}

pub async fn open(host: &str, which: &str, no_open: bool) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let ov = cluster::overview(&host, RemoteHome::current()).await?;
    let view = pick(&ov, which)?;
    let endpoint = ov.endpoints.get(&view.id).with_context(|| {
        format!(
            "{} is {} — it can be opened once it's running",
            view.name, view.state
        )
    })?;
    let tunnel = chimaera_remote::connect_compute_node(
        &host,
        &endpoint.node,
        &endpoint.job_id,
        endpoint.port,
        &endpoint.token,
    )
    .await?;
    let url = format!("{}&cws={}", tunnel.url(), view.id);
    println!("{url}");
    println!(
        "{} on {} (job {}) — Ctrl-C closes this connection; the job keeps running",
        view.name, tunnel.node, tunnel.job_id
    );
    if !no_open {
        let _ = open::that(&url);
    }
    let mut tunnel = tunnel;
    tokio::select! {
        r = tokio::signal::ctrl_c() => { r.context("failed to listen for ctrl-c")?; }
        _ = tunnel.wait() => {
            // A forward the ControlMaster holds outlives this child: keep
            // holding until the user leaves.
            tokio::signal::ctrl_c().await.context("failed to listen for ctrl-c")?;
        }
    }
    tunnel.close().await;
    Ok(())
}

pub async fn stop(host: &str, which: &str) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let home = RemoteHome::current();
    let ov = cluster::overview(&host, home).await?;
    let view = pick(&ov, which)?;
    let record = ov
        .records
        .get(&view.id)
        .with_context(|| format!("{} has never been started", view.name))?;
    cluster::stop(&host, home, &view.id, record).await?;
    println!(
        "stopping {} — its chimaera saves the chats, then the job ends",
        view.name
    );
    Ok(())
}
