//! `chimaera compute …` — cluster jobs and workspaces from the CLI: list
//! them, add a workspace, start a job (opening workspaces when it runs),
//! open, close or move a workspace, continue or stop a job. Every command is
//! a short ssh exec through the ControlMaster (`chimaera_remote::cluster`),
//! or a request to the job's own job-host over a plain `ssh -L`; nothing
//! runs on the login node. Thin by design: the verification harness and the
//! app-parity surface, not a second implementation.

use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use chimaera_core::cluster::HostedState;
use chimaera_core::slurm::{LaunchSpec, Scheduler};
use chimaera_remote::cluster::{
    self, ClusterOverview, HostOpen, JobStart, StartOutcome, StartRefused,
};
use chimaera_remote::{ComputeTunnel, RemoteHome};

/// Reach the host (may ask to authenticate) and require Slurm on it.
async fn slurm_host(host: &str) -> anyhow::Result<String> {
    let host = chimaera_remote::hosts::normalize_alias(host)?;
    let info = chimaera_remote::detect_scheduler(&host, RemoteHome::current()).await?;
    match info.kind {
        Scheduler::Slurm => Ok(host),
        Scheduler::None => bail!("{host} has no batch scheduler on its login PATH"),
        other => bail!(
            "{host} runs {}; jobs are only supported on Slurm so far",
            other.tag()
        ),
    }
}

/// A workspace by id or (case-insensitive) name.
fn pick_ws<'a>(ov: &'a ClusterOverview, which: &str) -> anyhow::Result<&'a cluster::WorkspaceView> {
    ov.workspaces
        .iter()
        .find(|w| w.id == which)
        .or_else(|| {
            ov.workspaces
                .iter()
                .find(|w| w.name.eq_ignore_ascii_case(which))
        })
        .with_context(|| {
            format!("no workspace {which:?} on this cluster — `chimaera compute jobs` shows them")
        })
}

/// A job by id or (case-insensitive) name, newest first.
fn pick_job<'a>(ov: &'a ClusterOverview, which: &str) -> anyhow::Result<&'a cluster::JobView> {
    ov.jobs
        .iter()
        .find(|j| j.id == which || j.slurm_job_id.as_deref() == Some(which))
        .or_else(|| ov.jobs.iter().find(|j| j.name.eq_ignore_ascii_case(which)))
        .with_context(|| {
            format!("no job {which:?} on this cluster — `chimaera compute jobs` shows them")
        })
}

/// The running job to open a workspace in: the one named, else the only one.
fn running_job<'a>(
    ov: &'a ClusterOverview,
    which: Option<&str>,
    host: &str,
) -> anyhow::Result<&'a cluster::JobView> {
    if let Some(which) = which {
        let job = pick_job(ov, which)?;
        anyhow::ensure!(job.state == "running", "{} is {}", job.name, job.state);
        return Ok(job);
    }
    let running: Vec<&cluster::JobView> = ov.jobs.iter().filter(|j| j.state == "running").collect();
    match running.as_slice() {
        [one] => Ok(one),
        [] => bail!(
            "no job is running on {host} — start one with `chimaera compute start {host} --time …`"
        ),
        _ => bail!(
            "{} jobs are running — say which with --job ({})",
            running.len(),
            running
                .iter()
                .map(|j| j.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn left(ends_at_ms: Option<u64>, now_ms: u64) -> String {
    match ends_at_ms {
        Some(end) if end > now_ms => {
            let mins = (end - now_ms) / 60_000;
            match (mins / 1440, (mins % 1440) / 60, mins % 60) {
                (0, 0, m) => format!("ends in {m}m"),
                (0, h, m) => format!("ends in {h}h {m}m"),
                (d, h, _) => format!("ends in {d}d {h}h"),
            }
        }
        _ => String::new(),
    }
}

/// The forward to a running job's job-host.
async fn host_tunnel(
    host: &str,
    ov: &ClusterOverview,
    jid: &str,
) -> anyhow::Result<(ComputeTunnel, String)> {
    let h = ov.hosts.get(jid).context("that job isn't running yet")?;
    let tunnel =
        chimaera_remote::connect_compute_node(host, &h.node, &h.slurm_job_id, h.port, &h.token)
            .await?;
    Ok((tunnel, h.token.clone()))
}

/// Ask a job to open a workspace and wait (bounded) until it's open.
/// Ask job `jid` to open workspace `wid` and wait until it listens; where it
/// listens comes from job-host itself.
async fn open_in(
    host: &str,
    ov: &ClusterOverview,
    jid: &str,
    wid: &str,
    name: &str,
) -> anyhow::Result<cluster::Endpoint> {
    let (tunnel, token) = host_tunnel(host, ov, jid).await?;
    let result = async {
        match cluster::host_open(tunnel.local_port, &token, wid).await? {
            HostOpen::Opened(_) => {}
            HostOpen::Held(held) => bail!("{name} is open in another job ({})", held.slurm_job_id),
            HostOpen::Refused(why) => bail!("{name} didn't open: {why}"),
        }
        let started = Instant::now();
        let mut told = false;
        loop {
            let status = cluster::host_status(tunnel.local_port, &token).await?;
            if let Some(endpoint) = cluster::endpoint_from(&status, jid, wid) {
                return Ok(endpoint);
            }
            match status.workspaces.iter().find(|w| w.id == wid) {
                Some(w) if w.state == HostedState::Failed => {
                    bail!("{name} didn't start:\n{}", w.detail)
                }
                None => bail!("{name} closed while opening"),
                _ => {}
            }
            if !told && started.elapsed() > Duration::from_secs(10) {
                println!("{name} is starting (it waits for any job it's leaving to let go)…");
                told = true;
            }
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(600),
                "{name} took too long to open"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    .await;
    tunnel.close().await;
    result
}

pub async fn jobs(host: &str) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let ov = cluster::overview(&host, RemoteHome::current()).await?;
    let live: Vec<&cluster::JobView> = ov.jobs.iter().filter(|j| j.state != "ended").collect();
    if live.is_empty() {
        println!("no jobs running on {host}");
    }
    for j in &live {
        let detail = match j.state {
            "running" => format!(
                "on {} · {} CPU{} · {} · {}",
                j.node,
                j.cpus,
                if j.cpus == "1" { "" } else { "s" },
                j.mem,
                left(j.ends_at_ms, ov.now_ms)
            ),
            "starting" => format!("starting on {}", j.node),
            _ if !j.reason.is_empty() => format!("waiting for a node ({})", j.reason),
            _ => "waiting for a node".to_string(),
        };
        println!(
            "{}  {:<20} {}{}{}",
            j.id,
            j.name,
            detail,
            j.slurm_job_id
                .as_deref()
                .map(|s| format!("  [Slurm {s}]"))
                .unwrap_or_default(),
            if j.attached {
                " · stops when its connection ends"
            } else {
                ""
            }
        );
        for w in ov
            .workspaces
            .iter()
            .filter(|w| w.job.as_deref() == Some(j.id.as_str()) && w.state != "closed")
        {
            let what = match (w.state, w.opening, w.closing) {
                ("queued", true, _) => "opening…",
                ("queued", false, _) => "opens when it starts",
                (_, _, true) => "closing…",
                _ => "open",
            };
            println!("    {}  {:<20} {what}  [{}]", w.id, w.name, w.path);
        }
    }
    let closed: Vec<&cluster::WorkspaceView> = ov
        .workspaces
        .iter()
        .filter(|w| w.state == "closed")
        .collect();
    if !closed.is_empty() {
        println!("not open:");
        for w in closed {
            let note = if w.failed.is_some() {
                "  (stopped unexpectedly)"
            } else {
                ""
            };
            println!("    {}  {:<20} [{}]{note}", w.id, w.name, w.path);
        }
    }
    if ov.workspaces.is_empty() {
        println!("no workspaces on {host} yet — add one with `chimaera compute add {host} <path>`");
    }
    for j in ov.jobs.iter().filter(|j| j.state == "ended") {
        let why = if j.stopped_by_user {
            "stopped by you".to_string()
        } else {
            j.ended.as_deref().unwrap_or("ended").to_lowercase()
        };
        println!("ended: {} {} ({why})", j.id, j.name);
    }
    if ov.other_jobs.running + ov.other_jobs.waiting > 0 {
        println!(
            "your other Slurm jobs on {host}: {} running · {} waiting",
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

/// What `start` and `continue` share.
pub struct StartArgs<'a> {
    pub spec: LaunchSpec,
    pub name: Option<&'a str>,
    pub open: Vec<String>,
    pub run_startup: Option<&'a str>,
    pub attached: bool,
    pub save_as: Option<&'a str>,
}

pub async fn start(host: &str, args: StartArgs<'_>) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let home = RemoteHome::current();
    let ov = cluster::overview(&host, home).await?;
    let mut open = Vec::new();
    for which in &args.open {
        let w = pick_ws(&ov, which)?;
        anyhow::ensure!(
            w.state == "closed",
            "{} is already {} — close or move it instead",
            w.name,
            if w.state == "open" {
                "open in a job"
            } else {
                "waiting to open in a job"
            }
        );
        open.push(w.id.clone());
    }
    submit(&host, &ov, &args, open, None).await
}

pub async fn continue_job(host: &str, which: &str, time: Option<String>) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let ov = cluster::overview(&host, RemoteHome::current()).await?;
    let job = pick_job(&ov, which)?;
    anyhow::ensure!(job.state == "running", "{} is {}", job.name, job.state);
    anyhow::ensure!(
        !job.attached,
        "{} runs attached to a terminal; start a new job when it stops",
        job.name
    );
    if let Some(next) = ov
        .jobs
        .iter()
        .find(|j| j.state != "ended" && j.replaces.as_deref() == Some(job.id.as_str()))
    {
        bail!(
            "{} already continues in {} ({})",
            job.name,
            next.id,
            next.name
        );
    }
    let mut spec = job.spec.clone();
    if let Some(t) = time {
        spec.time = t;
    }
    let open: Vec<String> = ov
        .workspaces
        .iter()
        .filter(|w| w.job.as_deref() == Some(job.id.as_str()) && w.state == "open")
        .map(|w| w.id.clone())
        .collect();
    let args = StartArgs {
        spec,
        name: Some(&job.name),
        open,
        run_startup: Some(&job.startup),
        attached: false,
        save_as: None,
    };
    submit(&host, &ov, &args, args.open.clone(), Some(&job.id)).await
}

async fn submit(
    host: &str,
    ov: &ClusterOverview,
    args: &StartArgs<'_>,
    open: Vec<String>,
    replaces: Option<&str>,
) -> anyhow::Result<()> {
    let home = RemoteHome::current();
    let spec = args.spec.clone().normalized();
    spec.validate().map_err(anyhow::Error::msg)?;
    let facts = cluster::facts(host, false).await?;
    cluster::ensure_cluster_binary(host, home, None, &|phase| {
        if let chimaera_remote::Phase::Downloading { target } = phase {
            tracing::info!("downloading the {target} chimaera binary");
        }
    })
    .await?;
    let req = JobStart {
        name: args.name,
        spec: &spec,
        open: &open,
        run_startup: args.run_startup.unwrap_or(""),
        attached: args.attached,
        replaces,
        facts: &facts,
    };
    match cluster::start_job(host, home, &ov.config, &req).await {
        Ok(StartOutcome::Submitted { job, slurm_job_id }) => {
            cluster::remember_spec(host, home, &spec, args.save_as).await?;
            println!(
                "submitted job {job} (Slurm {slurm_job_id}) — `chimaera compute jobs {host}` to watch{}",
                if replaces.is_some() {
                    "; when it starts it stops the job it replaces and takes its workspaces over"
                } else {
                    ""
                }
            );
            Ok(())
        }
        Ok(StartOutcome::Attached { job, job_name }) => {
            cluster::remember_spec(host, home, &spec, args.save_as).await?;
            let mut child =
                cluster::spawn_attached(host, home, &job, &spec, &job_name, facts.gpu_flag)?;
            let _output = cluster::attached_output(&mut child, true);
            println!(
                "job {job} starts attached to this terminal: it stops when you press Ctrl-C or \
                 this connection ends. Open workspaces in it from another terminal."
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
                cluster::learn_refusal(host, home, partition, refused.kind)
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

pub async fn open(host: &str, which: &str, job: Option<&str>, no_open: bool) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let home = RemoteHome::current();
    let ov = cluster::overview(&host, home).await?;
    let view = pick_ws(&ov, which)?.clone();
    let opened;
    let endpoint = match ov.endpoints.get(&view.id) {
        Some(e) if view.state == "open" => e,
        _ => {
            anyhow::ensure!(
                view.state == "closed" || view.opening,
                "{} opens when its job starts — `chimaera compute jobs {host}` to watch",
                view.name
            );
            let target = match (view.opening, &view.job) {
                (true, Some(j)) => j.clone(),
                _ => running_job(&ov, job, &host)?.id.clone(),
            };
            opened = open_in(&host, &ov, &target, &view.id, &view.name).await?;
            &opened
        }
    };
    let tunnel = chimaera_remote::connect_compute_node(
        &host,
        &endpoint.node,
        &endpoint.slurm_job_id,
        endpoint.port,
        &endpoint.token,
    )
    .await?;
    let url = format!("{}&ws={id}&cws={id}", tunnel.url(), id = view.id);
    println!("{url}");
    println!(
        "{} on {} — Ctrl-C closes this connection; the job keeps running",
        view.name, tunnel.node
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

pub async fn close(host: &str, which: &str) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let ov = cluster::overview(&host, RemoteHome::current()).await?;
    let view = pick_ws(&ov, which)?;
    let jid = view
        .job
        .as_deref()
        .filter(|_| view.state == "open")
        .with_context(|| format!("{} isn't open in a job", view.name))?;
    let (tunnel, token) = host_tunnel(&host, &ov, jid).await?;
    let r = cluster::host_close(tunnel.local_port, &token, &view.id).await;
    tunnel.close().await;
    r?;
    println!("closed {} — its chats are saved", view.name);
    Ok(())
}

pub async fn move_ws(host: &str, which: &str, to: &str) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let ov = cluster::overview(&host, RemoteHome::current()).await?;
    let view = pick_ws(&ov, which)?.clone();
    let target = pick_job(&ov, to)?.clone();
    anyhow::ensure!(
        target.state == "running",
        "{} is {}",
        target.name,
        target.state
    );
    if let Some(from) = view.job.as_deref().filter(|_| view.state == "open") {
        anyhow::ensure!(
            from != target.id,
            "{} is already open in {}",
            view.name,
            target.name
        );
        let (tunnel, token) = host_tunnel(&host, &ov, from).await?;
        let r = cluster::host_close(tunnel.local_port, &token, &view.id).await;
        tunnel.close().await;
        r?;
    }
    open_in(&host, &ov, &target.id, &view.id, &view.name).await?;
    println!(
        "moved {} to {} — its chats came with it",
        view.name, target.name
    );
    Ok(())
}

pub async fn stop(host: &str, which: &str) -> anyhow::Result<()> {
    let host = slurm_host(host).await?;
    let home = RemoteHome::current();
    let ov = cluster::overview(&host, home).await?;
    let job = pick_job(&ov, which)?;
    let record = ov
        .records
        .get(&job.id)
        .context("that job's record is gone")?;
    cluster::stop_job(&host, home, record).await?;
    println!(
        "stopping {} — every workspace in it saves its chats, then the job ends",
        job.name
    );
    Ok(())
}
