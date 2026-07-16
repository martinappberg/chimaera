//! `chimaera compute …` — Mode 2 from the CLI: launch/list/connect/cancel
//! chimaera sessions running AS Slurm jobs, via the LOGIN daemon's routes
//! (curl-over-ssh; the daemon owns the detached srun clients and the preludes). Thin by
//! design: this is the verification harness and the app-parity surface,
//! not a second implementation.

use anyhow::Context;
use chimaera_remote::RemoteHome;

async fn login_manifest(host: &str) -> anyhow::Result<chimaera_core::Manifest> {
    chimaera_remote::remote_manifest(host, RemoteHome::current())
        .await?
        .with_context(|| {
            format!("no chimaera daemon on {host} — run `chimaera connect {host}` first")
        })
}

pub async fn list(host: &str) -> anyhow::Result<()> {
    let manifest = login_manifest(host).await?;
    let v = chimaera_remote::compute_sessions(host, &manifest).await?;
    if v.get("scheduler").and_then(|s| s.as_str()) != Some("slurm") {
        println!("no scheduler detected on {host}");
        return Ok(());
    }
    let sessions = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    if sessions.is_empty() {
        println!("no compute sessions on {host} (launch one with `chimaera compute launch`)");
        return Ok(());
    }
    for s in sessions {
        let g = |k: &str| {
            s.get(k)
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default()
        };
        println!(
            "{}  {:<20} {:<10} {:<14} {:<9} left {:<10} cpus {:<3} mem {:<6} ready={}",
            g("job_id"),
            g("name"),
            g("state"),
            if g("node").is_empty() {
                "(queued)".into()
            } else {
                g("node")
            },
            g("partition"),
            g("time_left"),
            g("cpus"),
            g("mem"),
            g("ready"),
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // mirrors the launch spec, one flag each
pub async fn launch(
    host: &str,
    name: &str,
    time: &str,
    partition: Option<&str>,
    cpus: Option<u32>,
    mem: Option<&str>,
    gres: Option<&str>,
    workspace: Option<&str>,
    routable: bool,
) -> anyhow::Result<()> {
    let manifest = login_manifest(host).await?;
    let mut spec = serde_json::json!({ "name": name, "time": time, "routable": routable });
    for (k, v) in [
        ("partition", partition),
        ("mem", mem),
        ("gres", gres),
        ("workspace_id", workspace),
    ] {
        if let Some(v) = v {
            spec[k] = serde_json::Value::String(v.to_string());
        }
    }
    if let Some(c) = cpus {
        spec["cpus"] = serde_json::json!(c);
    }
    let job_id = chimaera_remote::compute_launch(host, &manifest, &spec).await?;
    println!("submitted: job {job_id} ({name}) — `chimaera compute list {host}` to watch, `chimaera compute connect {host} {job_id}` once running");
    Ok(())
}

pub async fn connect(host: &str, job_id: &str, no_open: bool) -> anyhow::Result<()> {
    let manifest = login_manifest(host).await?;
    let v = chimaera_remote::compute_sessions(host, &manifest).await?;
    let session = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|s| s.get("job_id").and_then(|j| j.as_str()) == Some(job_id))
        })
        .with_context(|| {
            format!("job {job_id} not found on {host} (ended, or not a chimaera session?)")
        })?
        .clone();
    let s = |k: &str| {
        session
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let ready = session
        .get("ready")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    anyhow::ensure!(
        ready,
        "job {job_id} is {} on {} — not ready to connect yet (state must be RUNNING with the daemon up)",
        s("state"),
        if s("node").is_empty() { "no node yet".to_string() } else { s("node") },
    );
    let port = session
        .get("port")
        .and_then(|v| v.as_u64())
        .context("session has no port (manifest missing?)")? as u16;
    let token = s("token");
    let routable = session
        .get("routable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let tunnel =
        chimaera_remote::connect_compute_node(host, &s("node"), job_id, port, &token, routable)
            .await?;
    let url = tunnel.url();
    println!("compute session up: {url}");
    println!(
        "rung: {} · node {} · job {} — Ctrl-C closes the tunnel (the job keeps running)",
        match tunnel.rung {
            chimaera_remote::ComputeRung::SshAdopt => "B (ssh-adopt, loopback daemon)",
            chimaera_remote::ComputeRung::Chained => "B (chained via login node, loopback daemon)",
            chimaera_remote::ComputeRung::Direct => "A (direct routable forward)",
        },
        tunnel.node,
        tunnel.job_id
    );
    if !no_open {
        let _ = open::that(&url);
    }
    // Hold like `connect`: the tunnel lives as long as this process.
    let mut tunnel = tunnel;
    let _ = tunnel.wait().await;
    Ok(())
}

pub async fn cancel(host: &str, job_id: &str) -> anyhow::Result<()> {
    let manifest = login_manifest(host).await?;
    chimaera_remote::compute_cancel(host, &manifest, job_id).await?;
    println!(
        "cancel requested for job {job_id} (scancel; the card disappears once slurm reaps it)"
    );
    Ok(())
}
