//! Clusters in the native shell: the `cluster_*` commands behind the cluster
//! page. You start Slurm **jobs**, and open **workspaces** inside them; a
//! workspace keeps its chats in its own folder, so it moves between jobs.
//! Also here: workspace windows (one per open workspace), the notifications
//! that tell the user when a job is ready, about to end, or gone, and windows
//! following a workspace that moved to another job.
//!
//! Nothing here keeps anything running on a cluster's login node. Every call
//! is a short ssh exec through the ControlMaster (`chimaera_remote::cluster`)
//! or a request to the job's own job-host over a plain `ssh -L`; the only
//! long-lived things are this process's own forwards to the user's jobs and,
//! for partitions that take only interactive jobs, the foreground `srun` the
//! user asked for — both end with this app.
//!
//! Ports and tokens never reach a page: the overview's endpoints stay here,
//! and a workspace window's URL is the only carrier of its token.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chimaera_core::cluster::HostedState;
use chimaera_core::slurm::LaunchSpec;
use chimaera_remote::cluster::{
    self, ClusterOverview, HostOpen, JobStart, StartOutcome, StartRefused,
};
use chimaera_remote::RemoteHome;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};

use super::connect::{do_connect, state_for, with_hosts, HostState, HostStatus};
use super::restore::open_compute_window;
use super::{authorize_scope_origin, lock, Shell};
use crate::windows::{ComputeScope, WindowRecord};

/// Live, per-cluster state of this process.
#[derive(Default)]
pub(crate) struct ClusterLive {
    /// What the last connect found (None until a connect ran this process).
    pub(crate) info: Option<ClusterInfo>,
    /// Open workspaces' chimaeras, from the last overview.
    endpoints: HashMap<String, cluster::Endpoint>,
    /// Running jobs' job-hosts, from the last overview.
    hosts: HashMap<String, cluster::HostEndpoint>,
    ws_names: HashMap<String, String>,
    /// Last seen state per job — the notification diff.
    last_jobs: HashMap<String, &'static str>,
    /// Last seen job per open workspace — a window follows its workspace.
    last_ws: HashMap<String, String>,
    /// Workspaces on their way from the job in `last_ws` to another (a move,
    /// or a job continuing in a new one): their windows wait, then follow.
    moving: HashSet<String>,
    /// One notification per (kind, job).
    notified: HashSet<String>,
    /// Slurm start estimates for waiting jobs, asked at most every 5 min.
    estimates: HashMap<String, (Instant, Option<u64>)>,
    /// Jobs whose end reason was already asked of accounting.
    ended_asked: HashSet<String>,
    /// This build's binary is on the cluster (checked once per run).
    binary_ok: bool,
    /// The background watcher is running.
    watching: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ClusterInfo {
    pub(crate) scheduler: chimaera_core::slurm::Scheduler,
    pub(crate) login_daemon: Option<chimaera_remote::LoginDaemon>,
}

fn home() -> RemoteHome {
    RemoteHome::current()
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// A workspace window's tunnel key: one per open workspace (a job may host
/// several), naming the job so a move is a new key.
fn ws_key(alias: &str, slurm_job_id: &str, wid: &str) -> String {
    format!("{alias}#job{slurm_job_id}~{wid}")
}

/// A job-host's tunnel key.
fn host_key(alias: &str, jid: &str) -> String {
    format!("{alias}#host{jid}")
}

fn valid_ws(id: &str) -> Result<(), String> {
    if chimaera_core::cluster::valid_workspace_id(id) {
        Ok(())
    } else {
        Err("unknown workspace".to_string())
    }
}

fn valid_job(id: &str) -> Result<(), String> {
    if chimaera_core::cluster::valid_job_id(id) {
        Ok(())
    } else {
        Err("unknown job".to_string())
    }
}

/// Make sure this process has probed `alias` (the scheduler and the PATH it
/// lives on) and holds its ControlMaster — a connect, which on a cluster
/// starts nothing.
async fn ensure_cluster(app: &AppHandle, alias: &str) -> Result<(), String> {
    if chimaera_remote::scheduler_of(alias).is_some_and(|s| s.kind.is_cluster()) {
        return Ok(());
    }
    do_connect(app, alias.to_string(), false).await?;
    match chimaera_remote::scheduler_of(alias) {
        Some(s) if s.kind == chimaera_core::slurm::Scheduler::Slurm => Ok(()),
        Some(s) if s.kind.is_cluster() => Err(format!(
            "{alias} runs {}; jobs are only supported on Slurm so far",
            s.kind.tag()
        )),
        _ => Err(format!("{alias} has no batch scheduler on its login PATH")),
    }
}

/// How a job ended, as a sentence.
fn ended_sentence(state: &str) -> &'static str {
    match state {
        "TIMEOUT" => "It hit its time limit.",
        "CANCELLED" => "It was cancelled.",
        "FAILED" => "It failed.",
        "PREEMPTED" => "It was preempted.",
        "NODE_FAIL" => "Its node failed.",
        "OUT_OF_MEMORY" => "It ran out of memory.",
        "COMPLETED" => "It finished.",
        _ => "It ended.",
    }
}

fn toast(alias: &str, jid: &str, kind: &str, title: String, job_name: &str, body: String) {
    crate::notify::post(crate::notify::Toast {
        // Not a `chimaera` route id: a click just brings the app forward.
        id: format!("cluster-{alias}-{jid}-{kind}"),
        thread: format!("cluster/{alias}"),
        title,
        subtitle: job_name.to_string(),
        body,
        sound: false,
    });
}

/// "crc is", "crc and atlas are", "3 workspaces are".
fn ready_words(names: &[String]) -> String {
    match names {
        [] => "Open a workspace in it.".to_string(),
        [one] => format!("{one} is ready to open."),
        [a, b] => format!("{a} and {b} are ready to open."),
        more => format!("{} workspaces are ready to open.", more.len()),
    }
}

/// Close a tunnel by key and tell its window(s) why.
async fn end_tunnel(app: &AppHandle, key: String, reason: &str) {
    let shell = app.state::<Shell>();
    let tunnel = shell.compute_tunnels.lock().await.remove(&key);
    lock(&shell.unhealthy_tunnels).remove(&key);
    if let Some(t) = tunnel {
        t.close().await;
    }
    let _ = app.emit(
        "host-status",
        HostStatus {
            alias: key,
            status: "ended",
            local_port: None,
            token: None,
            error: None,
            reason: Some(reason.to_string()),
            build: None,
            node: None,
        },
    );
}

/// Take a fresh overview into this process's state: endpoints, the
/// notification diff, windows of workspaces that closed, moved or whose job
/// ended, one accounting question per ended job, and the watcher when
/// anything is still alive.
async fn absorb(app: &AppHandle, alias: &str, ov: &ClusterOverview) {
    let shell = app.state::<Shell>();
    // (window key, reason) to end; (wid, new endpoint) to follow.
    let mut ended: Vec<(String, String)> = Vec::new();
    let mut moved: Vec<(String, String, cluster::Endpoint)> = Vec::new();
    let mut ask_end: Vec<chimaera_core::cluster::JobRecord> = Vec::new();
    let mut live = false;
    {
        let mut clusters = lock(&shell.clusters);
        let c = clusters.entry(alias.to_string()).or_default();
        c.endpoints = ov.endpoints.clone();
        c.hosts = ov.hosts.clone();
        for w in &ov.workspaces {
            c.ws_names.insert(w.id.clone(), w.name.clone());
        }
        let slurm_of = |jid: &str| {
            ov.jobs
                .iter()
                .find(|j| j.id == jid)
                .and_then(|j| j.slurm_job_id.clone())
                .unwrap_or_default()
        };
        // How a job's windows are told it ended: "stopped" for the user's
        // stop, else Slurm's word.
        let end_reason = |jid: &str| {
            ov.jobs
                .iter()
                .find(|j| j.id == jid)
                .map(|j| {
                    if j.stopped_by_user {
                        "stopped".to_string()
                    } else {
                        j.ended.clone().unwrap_or_else(|| "ENDED".into())
                    }
                })
                .unwrap_or_else(|| "ENDED".into())
        };
        // Where each workspace waits to open (a job continuing another opens
        // its workspaces once that one lets go).
        let queued_in: HashMap<&str, &str> = ov
            .workspaces
            .iter()
            .filter(|w| w.state == "queued")
            .filter_map(|w| Some((w.id.as_str(), w.job.as_deref()?)))
            .collect();
        let heading_elsewhere = |moving: &HashSet<String>, wid: &str, from: &str| {
            moving.contains(wid) || queued_in.get(wid).is_some_and(|to| *to != from)
        };
        for j in &ov.jobs {
            live |= j.state != "ended";
            let prev = c.last_jobs.insert(j.id.clone(), j.state);
            let names: Vec<String> = ov
                .workspaces
                .iter()
                .filter(|w| w.job.as_deref() == Some(j.id.as_str()))
                .map(|w| w.name.clone())
                .collect();
            if j.state == "running" {
                if prev.is_some_and(|p| p != "running")
                    && c.notified.insert(format!("ready:{}", j.id))
                {
                    toast(
                        alias,
                        &j.id,
                        "ready",
                        format!("Your job on {alias} is ready"),
                        &j.name,
                        ready_words(&names),
                    );
                }
                if let Some(end) = j.ends_at_ms {
                    let left = end.saturating_sub(ov.now_ms);
                    let body = "Continue in a new job to keep working.".to_string();
                    if left < 10 * 60_000 {
                        if c.notified.insert(format!("10m:{}", j.id)) {
                            toast(
                                alias,
                                &j.id,
                                "10m",
                                format!("Your job on {alias} ends in 10 minutes"),
                                &j.name,
                                body,
                            );
                        }
                    } else if left < 60 * 60_000 && c.notified.insert(format!("1h:{}", j.id)) {
                        toast(
                            alias,
                            &j.id,
                            "1h",
                            format!("Your job on {alias} ends in 1 hour"),
                            &j.name,
                            body,
                        );
                    }
                }
            }
            if j.state == "ended" {
                if prev.is_some_and(|p| p != "ended") {
                    let state = j.ended.clone().unwrap_or_else(|| "ENDED".into());
                    if !j.stopped_by_user && c.notified.insert(format!("ended:{}", j.id)) {
                        toast(
                            alias,
                            &j.id,
                            "ended",
                            format!("Your job on {alias} stopped"),
                            &j.name,
                            format!("{} Chats are saved.", ended_sentence(&state)),
                        );
                    }
                    let reason = end_reason(&j.id);
                    // Every window of a workspace this job held — except one
                    // on its way to another job (told below; it follows).
                    let slurm = j.slurm_job_id.clone().unwrap_or_default();
                    for (wid, jid) in c.last_ws.iter() {
                        if jid == &j.id && !heading_elsewhere(&c.moving, wid, jid) {
                            ended.push((ws_key(alias, &slurm, wid), reason.clone()));
                        }
                    }
                    ended.push((host_key(alias, &j.id), reason));
                }
                if j.ended.is_none() && !j.stopped_by_user && c.ended_asked.insert(j.id.clone()) {
                    if let Some(r) = ov.records.get(&j.id) {
                        ask_end.push(r.clone());
                    }
                }
            }
        }
        // Workspaces that left the job a window shows: moved (open in
        // another job now — the window follows), on their way there (the
        // window says so and waits), closed or failed (its job still runs),
        // or a move that never arrived.
        let now_open: HashMap<String, String> = ov
            .workspaces
            .iter()
            .filter(|w| w.state == "open")
            .filter_map(|w| Some((w.id.clone(), w.job.clone()?)))
            .collect();
        let mut next_ws = now_open.clone();
        let before: Vec<(String, String)> = c
            .last_ws
            .iter()
            .map(|(w, j)| (w.clone(), j.clone()))
            .collect();
        for (wid, old_job) in before {
            let old_running = ov
                .jobs
                .iter()
                .any(|j| j.id == old_job && j.state != "ended");
            let key = ws_key(alias, &slurm_of(&old_job), &wid);
            let told_moving = format!("moving:{wid}:{old_job}");
            match now_open.get(&wid) {
                Some(new_job) if new_job != &old_job => {
                    c.notified.remove(&told_moving);
                    if let Some(e) = ov.endpoints.get(&wid) {
                        moved.push((wid.clone(), slurm_of(&old_job), e.clone()));
                    }
                }
                Some(_) => {}
                None if heading_elsewhere(&c.moving, &wid, &old_job) => {
                    next_ws.insert(wid.clone(), old_job.clone());
                    if c.notified.insert(told_moving) {
                        ended.push((key, "moving".into()));
                    }
                }
                None if old_running => {
                    let failed = ov
                        .workspaces
                        .iter()
                        .any(|w| w.id == wid && w.failed.is_some());
                    let why = if failed { "workspace-failed" } else { "closed" };
                    ended.push((key, why.into()));
                }
                None if c.notified.remove(&told_moving) => {
                    // Its new job never opened it (stopped, or it ended).
                    ended.push((key, end_reason(&old_job)));
                }
                None => {}
            }
        }
        c.last_ws = next_ws;
    }
    for (key, reason) in ended {
        end_tunnel(app, key, &reason).await;
    }
    for (wid, old_slurm, endpoint) in moved {
        follow_move(app, alias, &wid, &old_slurm, &endpoint).await;
    }
    if !ask_end.is_empty() {
        let app = app.clone();
        let alias = alias.to_string();
        tauri::async_runtime::spawn(async move {
            for rec in ask_end {
                if let Err(e) = cluster::record_end(&alias, home(), &rec).await {
                    tracing::debug!("could not ask how {alias}'s job ended: {e:#}");
                }
            }
            let _ = app.emit("cluster-changed", json!({ "alias": alias }));
        });
    }
    if live {
        ensure_watcher(app, alias);
    }
}

/// A workspace that a window shows moved to another job: open its window
/// there, then close the old one (whose chimaera already let go).
async fn follow_move(
    app: &AppHandle,
    alias: &str,
    wid: &str,
    old_slurm: &str,
    endpoint: &cluster::Endpoint,
) {
    let shell = app.state::<Shell>();
    let old_key = ws_key(alias, old_slurm, wid);
    let label = super::commands::find_by_alias(&shell.windows, &old_key);
    let name = {
        let clusters = lock(&shell.clusters);
        clusters
            .get(alias)
            .and_then(|c| c.ws_names.get(wid).cloned())
            .unwrap_or_else(|| "workspace".into())
    };
    let tunnel = shell.compute_tunnels.lock().await.remove(&old_key);
    if let Some(t) = tunnel {
        t.close().await;
    }
    if label.is_some() {
        if let Err(e) = open_ws_window(app, alias, wid, endpoint, &name).await {
            tracing::warn!("{alias}/{wid}: opening its window in the new job: {e}");
        }
    }
    if let Some(win) = label.and_then(|l| app.get_webview_window(&l)) {
        let _ = win.close();
    }
}

/// Watch a cluster while a job this app knows of is alive: the queue at most
/// once a minute while a job waits or starts, every five minutes while jobs
/// only run (an early end — failure, preemption — still gets told), and not
/// at all once nothing is alive. It ends with the app.
fn ensure_watcher(app: &AppHandle, alias: &str) {
    let shell = app.state::<Shell>();
    {
        let mut clusters = lock(&shell.clusters);
        let c = clusters.entry(alias.to_string()).or_default();
        if c.watching {
            return;
        }
        c.watching = true;
    }
    let app = app.clone();
    let alias = alias.to_string();
    tauri::async_runtime::spawn(async move {
        let mut interval = cluster::SQUEUE_FLOOR;
        let mut failures = 0u32;
        loop {
            tokio::time::sleep(interval).await;
            let ov = match cluster::overview(&alias, home()).await {
                Ok(ov) => {
                    failures = 0;
                    ov
                }
                Err(e) => {
                    failures += 1;
                    tracing::debug!("cluster watch {alias}: {e:#}");
                    if failures >= 5 {
                        break;
                    }
                    interval = Duration::from_secs(300);
                    continue;
                }
            };
            absorb(&app, &alias, &ov).await;
            let _ = app.emit("cluster-changed", json!({ "alias": alias }));
            let waiting = ov
                .jobs
                .iter()
                .any(|j| j.state == "waiting" || j.state == "starting");
            let alive = ov.jobs.iter().any(|j| j.state != "ended");
            if !alive {
                break;
            }
            interval = if waiting {
                cluster::SQUEUE_FLOOR
            } else {
                Duration::from_secs(300)
            };
            // Wake at the next "ends in 1 hour / 10 minutes" mark, so the
            // notice comes when it says.
            for end in ov
                .jobs
                .iter()
                .filter(|j| j.state == "running")
                .filter_map(|j| j.ends_at_ms)
            {
                for mark in [60 * 60_000, 10 * 60_000] {
                    let at = end.saturating_sub(mark);
                    if at > ov.now_ms {
                        let wait = Duration::from_millis(at - ov.now_ms + 5_000);
                        interval = interval.min(wait.max(cluster::SQUEUE_FLOOR));
                    }
                }
            }
        }
        let shell = app.state::<Shell>();
        let mut clusters = lock(&shell.clusters);
        if let Some(c) = clusters.get_mut(&alias) {
            c.watching = false;
        }
    });
}

/// An overview, clearing a wedged ControlMaster once (after laptop sleep
/// the master often survives its dead link and every exec queues on it).
async fn fresh_overview(alias: &str) -> Result<ClusterOverview, String> {
    match cluster::overview(alias, home()).await {
        Ok(ov) => Ok(ov),
        Err(first) => {
            if chimaera_remote::clear_wedged_master(alias, 15).await {
                cluster::overview(alias, home()).await.map_err(err)
            } else {
                Err(err(first))
            }
        }
    }
}

/// The overview as a page gets it: no endpoints, plus Slurm's start
/// estimate for waiting jobs (asked at most every five minutes per job) and
/// what's happening in each open workspace (its job-host's count of working
/// agents, when the job's forward is up).
async fn page_overview(app: &AppHandle, alias: &str) -> Result<serde_json::Value, String> {
    let ov = fresh_overview(alias).await?;
    absorb(app, alias, &ov).await;
    let shell = app.state::<Shell>();
    let mut wanted: Vec<String> = Vec::new();
    let mut estimates: HashMap<String, Option<u64>> = HashMap::new();
    {
        let clusters = lock(&shell.clusters);
        let c = clusters.get(alias);
        for j in ov.jobs.iter().filter(|j| j.state == "waiting") {
            let Some(slurm) = &j.slurm_job_id else {
                continue;
            };
            match c.and_then(|c| c.estimates.get(slurm)) {
                Some((at, est)) if at.elapsed() < Duration::from_secs(300) => {
                    estimates.insert(slurm.clone(), *est);
                }
                _ => wanted.push(slurm.clone()),
            }
        }
    }
    for slurm in wanted {
        let est = cluster::start_estimate(alias, &slurm).await.ok().flatten();
        estimates.insert(slurm.clone(), est);
        lock(&shell.clusters)
            .entry(alias.to_string())
            .or_default()
            .estimates
            .insert(slurm, (Instant::now(), est));
    }
    // Activity per open workspace, from the jobs whose forward this app
    // already holds (a page view never builds one just to count), and a
    // failed workspace's last words.
    let mut working: HashMap<String, u32> = HashMap::new();
    let mut failed: HashMap<String, String> = HashMap::new();
    for (jid, h) in &ov.hosts {
        let key = host_key(alias, jid);
        let port = shell
            .compute_tunnels
            .lock()
            .await
            .get(&key)
            .map(|t| t.local_port);
        if let Some(port) = port {
            if let Ok(status) = cluster::host_status(port, &h.token).await {
                for w in status.workspaces {
                    if w.state == HostedState::Failed {
                        failed.insert(w.id.clone(), w.detail.clone());
                    }
                    working.insert(w.id, w.working);
                }
            }
        }
    }
    let mut v = serde_json::to_value(&ov).map_err(|e| e.to_string())?;
    if let Some(list) = v.get_mut("jobs").and_then(|w| w.as_array_mut()) {
        for j in list {
            let est = j
                .get("slurm_job_id")
                .and_then(|s| s.as_str())
                .and_then(|s| estimates.get(s).copied().flatten());
            if let (Some(obj), Some(est)) = (j.as_object_mut(), est) {
                obj.insert("start_estimate_ms".into(), json!(est));
            }
        }
    }
    if let Some(list) = v.get_mut("workspaces").and_then(|w| w.as_array_mut()) {
        for w in list {
            let id = w
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(obj) = w.as_object_mut() {
                if let Some(n) = working.get(&id) {
                    obj.insert("working".into(), json!(n));
                }
                if let Some(detail) = failed.get(&id).filter(|_| obj.contains_key("failed")) {
                    obj.insert("failed".into(), json!(detail));
                }
            }
        }
    }
    Ok(v)
}

#[tauri::command]
pub(super) async fn cluster_overview(
    app: AppHandle,
    alias: String,
) -> Result<serde_json::Value, String> {
    ensure_cluster(&app, &alias).await?;
    page_overview(&app, &alias).await
}

#[tauri::command]
pub(super) async fn cluster_facts(
    app: AppHandle,
    alias: String,
    refresh: Option<bool>,
) -> Result<chimaera_core::cluster::ClusterFacts, String> {
    ensure_cluster(&app, &alias).await?;
    cluster::facts(&alias, refresh.unwrap_or(false))
        .await
        .map_err(err)
}

#[tauri::command]
pub(super) async fn cluster_add_workspace(
    app: AppHandle,
    alias: String,
    path: String,
    name: String,
) -> Result<chimaera_core::cluster::ClusterWorkspace, String> {
    ensure_cluster(&app, &alias).await?;
    let ws = cluster::add_workspace(&alias, home(), &path, &name)
        .await
        .map_err(err)?;
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    Ok(ws)
}

#[tauri::command]
pub(super) async fn cluster_remove_workspace(
    app: AppHandle,
    alias: String,
    workspace_id: String,
) -> Result<(), String> {
    valid_ws(&workspace_id)?;
    ensure_cluster(&app, &alias).await?;
    cluster::remove_workspace(&alias, home(), &workspace_id)
        .await
        .map_err(err)?;
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    Ok(())
}

/// One folder's subfolders, for choosing a workspace (the folder picker).
#[tauri::command]
pub(super) async fn cluster_list_dir(
    app: AppHandle,
    alias: String,
    path: String,
) -> Result<chimaera_core::cluster::DirListing, String> {
    ensure_cluster(&app, &alias).await?;
    ensure_binary(&app, &alias).await?;
    cluster::list_dir(&alias, home(), &path).await.map_err(err)
}

/// Put this build's binary on the cluster (a checksum compare, a copy only
/// when it differs) once per run — the progress rides the host row like a
/// connect's.
async fn ensure_binary(app: &AppHandle, alias: &str) -> Result<(), String> {
    {
        let shell = app.state::<Shell>();
        let clusters = lock(&shell.clusters);
        if clusters.get(alias).is_some_and(|c| c.binary_ok) {
            return Ok(());
        }
    }
    let entry = {
        let alias = alias.to_string();
        with_hosts(move |hosts| Ok(hosts.get(&alias))).await?
    };
    let progress_app = app.clone();
    let progress_alias = alias.to_string();
    cluster::ensure_cluster_binary(
        alias,
        home(),
        entry.and_then(|e| e.binary).as_deref(),
        &move |phase| {
            let label = match phase {
                chimaera_remote::Phase::Downloading { .. } => "downloading",
                chimaera_remote::Phase::Installing { .. } => "installing",
                _ => return,
            };
            let _ = progress_app.emit(
                "connect-progress",
                json!({ "alias": progress_alias, "phase": label }),
            );
        },
    )
    .await
    .map_err(err)?;
    let shell = app.state::<Shell>();
    lock(&shell.clusters)
        .entry(alias.to_string())
        .or_default()
        .binary_ok = true;
    Ok(())
}

/// Submit a job (or hold it in the foreground) and translate the outcome for
/// the page. A refusal is an answer, not an error: its words are the
/// cluster's own, and what it teaches is remembered.
#[allow(clippy::too_many_arguments)] // one per start-sheet field
async fn submit_job(
    app: &AppHandle,
    alias: &str,
    name: Option<&str>,
    spec: &LaunchSpec,
    open: &[String],
    run_startup: &str,
    attached: bool,
    replaces: Option<&str>,
    save_as: Option<&str>,
) -> Result<serde_json::Value, String> {
    let spec = spec.clone().normalized();
    spec.validate()?;
    let (config, _) = cluster::read_config(alias, home()).await.map_err(err)?;
    let facts = cluster::facts(alias, false).await.map_err(err)?;
    ensure_binary(app, alias).await?;
    let req = JobStart {
        name,
        spec: &spec,
        open,
        run_startup,
        attached,
        replaces,
        facts: &facts,
    };
    let outcome = match cluster::start_job(alias, home(), &config, &req).await {
        Ok(outcome) => outcome,
        Err(e) => {
            return match e.downcast_ref::<StartRefused>() {
                Some(refused) => {
                    if let Err(e) = cluster::learn_refusal(
                        alias,
                        home(),
                        spec.partition.as_deref(),
                        refused.kind,
                    )
                    .await
                    {
                        tracing::debug!("could not remember what {alias} refused: {e:#}");
                    }
                    Ok(json!({
                        "kind": "refused",
                        "message": refused.message,
                        "refusal": refused.kind,
                    }))
                }
                None => Err(err(e)),
            };
        }
    };
    if let Err(e) = cluster::remember_spec(alias, home(), &spec, save_as).await {
        tracing::debug!("could not remember the setup on {alias}: {e:#}");
    }
    let answer = match outcome {
        StartOutcome::Submitted { job, slurm_job_id } => {
            json!({ "kind": "submitted", "job": job, "slurm_job_id": slurm_job_id })
        }
        StartOutcome::Attached { job, job_name } => {
            let child =
                cluster::spawn_attached(alias, home(), &job, &spec, &job_name, facts.gpu_flag)
                    .map_err(err)?;
            hold_attached(app, alias, &job, child);
            json!({ "kind": "attached", "job": job })
        }
    };
    {
        let shell = app.state::<Shell>();
        let mut clusters = lock(&shell.clusters);
        // Seen waiting from the start, so the ready notification fires.
        if let Some(job) = answer.get("job").and_then(|j| j.as_str()) {
            clusters
                .entry(alias.to_string())
                .or_default()
                .last_jobs
                .insert(job.to_string(), "waiting");
        }
    }
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    ensure_watcher(app, alias);
    Ok(answer)
}

/// Hold an attached job in the foreground for as long as this app runs (or
/// until the user stops it). Dropping the child — app quit included — ends
/// the job.
fn hold_attached(app: &AppHandle, alias: &str, jid: &str, mut child: tokio::process::Child) {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let shell = app.state::<Shell>();
    let key = (alias.to_string(), jid.to_string());
    if let Some(old) = lock(&shell.attached_jobs).insert(key.clone(), tx) {
        let _ = old.send(());
    }
    let app = app.clone();
    let alias = alias.to_string();
    tauri::async_runtime::spawn(async move {
        tokio::select! {
            _ = child.wait() => {}
            _ = rx => {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
        let shell = app.state::<Shell>();
        lock(&shell.attached_jobs).remove(&key);
        cluster::invalidate_queue(&alias);
        let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    });
}

/// Start a job. `open` lists the workspaces to open once it runs.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // the page's start sheet, one field each
pub(super) async fn cluster_start_job(
    app: AppHandle,
    alias: String,
    spec: LaunchSpec,
    open: Vec<String>,
    run_startup: String,
    name: Option<String>,
    save_as: Option<String>,
    attached: bool,
) -> Result<serde_json::Value, String> {
    for w in &open {
        valid_ws(w)?;
    }
    ensure_cluster(&app, &alias).await?;
    tracing::info!(
        "ipc: cluster_start_job {alias} (opens {}, attached: {attached})",
        open.len()
    );
    // A workspace opens in one job at a time: ones already open (or waiting
    // to) stay where they are.
    let ov = cluster::overview(&alias, home()).await.map_err(err)?;
    let open: Vec<String> = open
        .into_iter()
        .filter(|w| {
            ov.workspaces
                .iter()
                .any(|v| &v.id == w && v.state == "closed")
        })
        .collect();
    submit_job(
        &app,
        &alias,
        name.as_deref().or(save_as.as_deref()),
        &spec,
        &open,
        &run_startup,
        attached,
        None,
        save_as.as_deref(),
    )
    .await
}

/// Continue a running job in a new one: the same setup (or the one given),
/// the same workspaces. When the new job runs, its job-host stops this one
/// (every workspace saves its chats) and opens them; their windows follow.
#[tauri::command]
pub(super) async fn cluster_continue_job(
    app: AppHandle,
    alias: String,
    job_id: Option<String>,
    workspace_id: Option<String>,
    spec: Option<LaunchSpec>,
    run_startup: Option<String>,
) -> Result<serde_json::Value, String> {
    if let Some(j) = &job_id {
        valid_job(j)?;
    }
    if let Some(w) = &workspace_id {
        valid_ws(w)?;
    }
    ensure_cluster(&app, &alias).await?;
    let ov = cluster::overview(&alias, home()).await.map_err(err)?;
    // A workspace window names its workspace; the job is the one it's in.
    let job_id = match (job_id, workspace_id) {
        (Some(j), _) => j,
        (None, Some(w)) => ov
            .workspaces
            .iter()
            .find(|v| v.id == w && v.state == "open")
            .and_then(|v| v.job.clone())
            .ok_or("this workspace isn't open in a running job")?,
        (None, None) => return Err("which job?".into()),
    };
    tracing::info!("ipc: cluster_continue_job {alias} {job_id}");
    let job = ov
        .jobs
        .iter()
        .find(|j| j.id == job_id)
        .ok_or("unknown job")?
        .clone();
    if job.state != "running" {
        return Err(format!("{} isn't running", job.name));
    }
    if job.attached {
        return Err(format!(
            "{} stops when this app disconnects; start a new job when it ends",
            job.name
        ));
    }
    if let Some(next) = ov
        .jobs
        .iter()
        .find(|j| j.state != "ended" && j.replaces.as_deref() == Some(job_id.as_str()))
    {
        return Err(format!(
            "{} already continues in a new job ({})",
            job.name, next.name
        ));
    }
    let open: Vec<String> = ov
        .workspaces
        .iter()
        .filter(|w| w.job.as_deref() == Some(job_id.as_str()) && w.state == "open")
        .map(|w| w.id.clone())
        .collect();
    let spec = spec.unwrap_or_else(|| job.spec.clone());
    let startup = run_startup.unwrap_or_else(|| job.startup.clone());
    submit_job(
        &app,
        &alias,
        Some(&job.name),
        &spec,
        &open,
        &startup,
        false,
        Some(&job_id),
        None,
    )
    .await
}

#[tauri::command]
pub(super) async fn cluster_stop_job(
    app: AppHandle,
    state: State<'_, Shell>,
    alias: String,
    job_id: String,
) -> Result<(), String> {
    valid_job(&job_id)?;
    ensure_cluster(&app, &alias).await?;
    tracing::info!("ipc: cluster_stop_job {alias} {job_id}");
    let ov = cluster::overview(&alias, home()).await.map_err(err)?;
    let record = ov.records.get(&job_id).ok_or("unknown job")?;
    cluster::stop_job(&alias, home(), record)
        .await
        .map_err(err)?;
    if let Some(tx) = lock(&state.attached_jobs).remove(&(alias.clone(), job_id.clone())) {
        let _ = tx.send(());
    }
    // The windows learn at once (the next overview would, a minute later).
    let slurm = record.slurm_job_id.clone().unwrap_or_default();
    let keys: Vec<String> = {
        let mut clusters = lock(&state.clusters);
        let c = clusters.entry(alias.clone()).or_default();
        c.last_jobs.insert(job_id.clone(), "ended");
        let wids: Vec<String> = c
            .last_ws
            .iter()
            .filter(|(_, j)| **j == job_id)
            .map(|(w, _)| w.clone())
            .collect();
        for w in &wids {
            c.last_ws.remove(w);
            c.endpoints.remove(w);
        }
        c.hosts.remove(&job_id);
        wids.iter().map(|w| ws_key(&alias, &slurm, w)).collect()
    };
    for key in keys {
        end_tunnel(&app, key, "stopped").await;
    }
    end_tunnel(&app, host_key(&alias, &job_id), "stopped").await;
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    Ok(())
}

/// Forget an ended job (its line on the page).
#[tauri::command]
pub(super) async fn cluster_dismiss_job(
    app: AppHandle,
    alias: String,
    job_id: String,
) -> Result<(), String> {
    valid_job(&job_id)?;
    ensure_cluster(&app, &alias).await?;
    let ov = cluster::overview(&alias, home()).await.map_err(err)?;
    if let Some(j) = ov.jobs.iter().find(|j| j.id == job_id) {
        if j.state != "ended" {
            return Err(format!("{} is still {}", j.name, j.state));
        }
    }
    cluster::dismiss_job(&alias, home(), &job_id)
        .await
        .map_err(err)?;
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    Ok(())
}

/// Reuse or build the forward to a running job's job-host.
async fn host_port(app: &AppHandle, alias: &str, jid: &str) -> Result<(u16, String), String> {
    let shell = app.state::<Shell>();
    let endpoint = {
        let clusters = lock(&shell.clusters);
        clusters.get(alias).and_then(|c| c.hosts.get(jid).cloned())
    };
    let endpoint = match endpoint {
        Some(e) => e,
        None => {
            let ov = fresh_overview(alias).await?;
            absorb(app, alias, &ov).await;
            ov.hosts
                .get(jid)
                .cloned()
                .ok_or("that job isn't running yet")?
        }
    };
    let key = host_key(alias, jid);
    let existing = {
        let tunnels = shell.compute_tunnels.lock().await;
        tunnels.get(&key).map(|t| t.local_port)
    };
    if let Some(port) = existing {
        if chimaera_remote::http_alive_authed(port, &endpoint.token).await {
            return Ok((port, endpoint.token));
        }
        if let Some(t) = shell.compute_tunnels.lock().await.remove(&key) {
            t.close().await;
        }
    }
    let tunnel = chimaera_remote::connect_compute_node(
        alias,
        &endpoint.node,
        &endpoint.slurm_job_id,
        endpoint.port,
        &endpoint.token,
    )
    .await
    .map_err(err)?;
    let port = tunnel.local_port;
    shell.compute_tunnels.lock().await.insert(key, tunnel);
    Ok((port, endpoint.token))
}

/// Ask job `jid` to open workspace `wid` and wait (bounded) until it's open.
async fn open_in_job(app: &AppHandle, alias: &str, jid: &str, wid: &str) -> Result<(), String> {
    let (port, token) = host_port(app, alias, jid).await?;
    match cluster::host_open(port, &token, wid).await.map_err(err)? {
        HostOpen::Opened(_) => {}
        HostOpen::Held(_) => {
            return Err("It's open in another job — move it from there instead".into())
        }
        HostOpen::Refused(why) => return Err(why),
    }
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    let started = Instant::now();
    loop {
        let status = cluster::host_status(port, &token).await.map_err(err)?;
        match status.workspaces.iter().find(|w| w.id == wid) {
            Some(w) if w.state == HostedState::Open && w.port.is_some() => return Ok(()),
            Some(w) if w.state == HostedState::Failed => {
                return Err(format!(
                    "It didn't start:\n{}",
                    w.detail.lines().last().unwrap_or(&w.detail)
                ))
            }
            None => return Err("It closed while opening".into()),
            _ => {}
        }
        if started.elapsed() > Duration::from_secs(300) {
            return Err("It's taking too long to open — try again in a moment".into());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Reuse or build the forward to an open workspace's chimaera and open (or
/// raise) its window.
async fn open_ws_window(
    app: &AppHandle,
    alias: &str,
    wid: &str,
    endpoint: &cluster::Endpoint,
    name: &str,
) -> Result<(), String> {
    let shell = app.state::<Shell>();
    let key = ws_key(alias, &endpoint.slurm_job_id, wid);
    let existing = {
        let tunnels = shell.compute_tunnels.lock().await;
        tunnels.get(&key).map(|t| (t.local_port, t.token.clone()))
    };
    if let Some((port, token)) = existing {
        if !chimaera_remote::http_alive_authed(port, &token).await {
            let tunnel = shell.compute_tunnels.lock().await.remove(&key);
            if let Some(tunnel) = tunnel {
                tunnel.close().await;
            }
        }
    }
    if shell.compute_tunnels.lock().await.get(&key).is_none() {
        {
            let mut connecting = lock(&shell.compute_connecting);
            if !connecting.insert(key.clone()) {
                return Err(format!("already connecting to {name} — hold on"));
            }
        }
        let built = chimaera_remote::connect_compute_node(
            alias,
            &endpoint.node,
            &endpoint.slurm_job_id,
            endpoint.port,
            &endpoint.token,
        )
        .await
        .map_err(err);
        let result = match built {
            Ok(tunnel) => {
                shell
                    .compute_tunnels
                    .lock()
                    .await
                    .insert(key.clone(), tunnel);
                Ok(())
            }
            Err(e) => Err(e),
        };
        lock(&shell.compute_connecting).remove(&key);
        result?;
    }
    let (url, node, local_port) = {
        let tunnels = shell.compute_tunnels.lock().await;
        let t = tunnels
            .get(&key)
            .ok_or_else(|| format!("{name} disconnected while connecting"))?;
        (
            format!("{}&cws={wid}", t.url()),
            t.node.clone(),
            t.local_port,
        )
    };
    authorize_scope_origin(app, Some(&key), local_port)
        .map_err(|e| format!("could not authorize {key}'s daemon origin: {e}"))?;
    let raised = super::commands::find_by_alias(&shell.windows, &key)
        .and_then(|label| app.get_webview_window(&label));
    match raised {
        Some(win) => {
            win.set_focus()
                .map_err(|e| format!("could not focus window: {e}"))?;
        }
        None => {
            let mut record = WindowRecord::new(Some(alias.to_string()), None);
            record.compute = Some(ComputeScope {
                job_id: endpoint.slurm_job_id.clone(),
                node: node.clone(),
            });
            let title = format!("{name} — {alias}");
            open_compute_window(app, &url, &title, &record, &key)
                .map_err(|e| format!("could not open window: {e}"))?;
        }
    }
    lock(&shell.unhealthy_tunnels).remove(&key);
    {
        let mut clusters = lock(&shell.clusters);
        let c = clusters.entry(alias.to_string()).or_default();
        c.last_ws.insert(wid.to_string(), endpoint.job.clone());
    }
    let _ = app.emit(
        "host-status",
        HostStatus {
            alias: key,
            status: "connected",
            local_port: Some(local_port),
            token: None,
            error: None,
            reason: None,
            build: None,
            node: None,
        },
    );
    Ok(())
}

/// Open a workspace's window: where it's open, else in job `job_id` (the
/// page decides which — none, one, or a choice). A workspace waiting to open
/// in a job that hasn't started says so.
#[tauri::command]
pub(super) async fn cluster_open(
    app: AppHandle,
    alias: String,
    workspace_id: String,
    job_id: Option<String>,
) -> Result<(), String> {
    valid_ws(&workspace_id)?;
    if let Some(j) = &job_id {
        valid_job(j)?;
    }
    ensure_cluster(&app, &alias).await?;
    tracing::info!("ipc: cluster_open {alias} {workspace_id} in {job_id:?}");
    // Always a fresh read: a cached endpoint may name an earlier job.
    let ov = fresh_overview(&alias).await?;
    absorb(&app, &alias, &ov).await;
    let view = ov
        .workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .ok_or("unknown workspace")?
        .clone();
    // Already opening in a running job: wait for that one.
    let job_id = match (view.opening, &view.job) {
        (true, Some(j)) => Some(j.clone()),
        _ => job_id,
    };
    let endpoint = match (view.state, ov.endpoints.get(&workspace_id), job_id) {
        ("open", Some(e), _) => e.clone(),
        ("queued", _, _) if !view.opening => {
            return Err(format!("{} opens when its job starts", view.name));
        }
        (_, _, Some(jid)) => {
            open_in_job(&app, &alias, &jid, &workspace_id).await?;
            let ov = fresh_overview(&alias).await?;
            absorb(&app, &alias, &ov).await;
            ov.endpoints
                .get(&workspace_id)
                .cloned()
                .ok_or_else(|| format!("{} isn't open yet — try again in a moment", view.name))?
        }
        (_, _, None) => return Err(format!("{} isn't open in a job", view.name)),
    };
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    open_ws_window(&app, &alias, &workspace_id, &endpoint, &view.name).await
}

/// Close a workspace in its job (its chimaera saves the chats and exits).
#[tauri::command]
pub(super) async fn cluster_close(
    app: AppHandle,
    alias: String,
    workspace_id: String,
) -> Result<(), String> {
    valid_ws(&workspace_id)?;
    ensure_cluster(&app, &alias).await?;
    tracing::info!("ipc: cluster_close {alias} {workspace_id}");
    let ov = fresh_overview(&alias).await?;
    let view = ov
        .workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .ok_or("unknown workspace")?;
    let jid = view
        .job
        .clone()
        .filter(|_| view.state == "open")
        .ok_or_else(|| format!("{} isn't open in a job", view.name))?;
    let slurm = ov
        .endpoints
        .get(&workspace_id)
        .map(|e| e.slurm_job_id.clone());
    let (port, token) = host_port(&app, &alias, &jid).await?;
    cluster::host_close(port, &token, &workspace_id)
        .await
        .map_err(err)?;
    if let Some(slurm) = slurm {
        {
            let shell = app.state::<Shell>();
            let mut clusters = lock(&shell.clusters);
            if let Some(c) = clusters.get_mut(&alias) {
                c.last_ws.remove(&workspace_id);
                c.endpoints.remove(&workspace_id);
            }
        }
        end_tunnel(&app, ws_key(&alias, &slurm, &workspace_id), "closed").await;
    }
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    Ok(())
}

/// Move an open workspace to another running job: close it there (its
/// chimaera saves the chats), open it here; its window follows.
#[tauri::command]
pub(super) async fn cluster_move(
    app: AppHandle,
    alias: String,
    workspace_id: String,
    job_id: String,
) -> Result<(), String> {
    valid_ws(&workspace_id)?;
    valid_job(&job_id)?;
    ensure_cluster(&app, &alias).await?;
    tracing::info!("ipc: cluster_move {alias} {workspace_id} → {job_id}");
    let ov = fresh_overview(&alias).await?;
    absorb(&app, &alias, &ov).await;
    let view = ov
        .workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .ok_or("unknown workspace")?
        .clone();
    let old = ov.endpoints.get(&workspace_id).cloned();
    let from = view.job.clone().filter(|_| view.state == "open");
    if from.as_deref() == Some(job_id.as_str()) {
        return Ok(());
    }
    // Its window waits (and follows) instead of reading "closed" while the
    // workspace is between jobs.
    let shell = app.state::<Shell>();
    lock(&shell.clusters)
        .entry(alias.clone())
        .or_default()
        .moving
        .insert(workspace_id.clone());
    let moved = async {
        if let Some(from) = &from {
            let (port, token) = host_port(&app, &alias, from).await?;
            cluster::host_close(port, &token, &workspace_id)
                .await
                .map_err(err)?;
        }
        let _ = app.emit("cluster-changed", json!({ "alias": alias }));
        open_in_job(&app, &alias, &job_id, &workspace_id).await?;
        let ov = fresh_overview(&alias).await?;
        let endpoint = ov
            .endpoints
            .get(&workspace_id)
            .cloned()
            .ok_or("it moved but isn't reachable yet — open it again in a moment")?;
        Ok::<_, String>((ov, endpoint))
    }
    .await;
    if let Some(c) = lock(&shell.clusters).get_mut(&alias) {
        c.moving.remove(&workspace_id);
    }
    let (ov, endpoint) = match moved {
        Ok(done) => done,
        Err(e) => {
            // Its window learns where it stands from a fresh look.
            if let Ok(ov) = fresh_overview(&alias).await {
                absorb(&app, &alias, &ov).await;
            }
            let _ = app.emit("cluster-changed", json!({ "alias": alias }));
            return Err(e);
        }
    };
    // Its window (if any) follows; the old key's tunnel goes.
    if let Some(old) = old {
        follow_move(&app, &alias, &workspace_id, &old.slurm_job_id, &endpoint).await;
    }
    absorb(&app, &alias, &ov).await;
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    Ok(())
}

/// Set the cluster's (`workspace_id: None`) or one workspace's startup
/// commands — the cluster's Environment settings, edited from here.
#[tauri::command]
pub(super) async fn cluster_set_startup(
    app: AppHandle,
    alias: String,
    workspace_id: Option<String>,
    text: String,
) -> Result<(), String> {
    if let Some(id) = &workspace_id {
        valid_ws(id)?;
    }
    ensure_cluster(&app, &alias).await?;
    cluster::set_startup(&alias, home(), workspace_id.as_deref(), &text)
        .await
        .map_err(err)?;
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    Ok(())
}

#[tauri::command]
pub(super) async fn cluster_forget_setup(
    app: AppHandle,
    alias: String,
    name: String,
) -> Result<(), String> {
    ensure_cluster(&app, &alias).await?;
    cluster::forget_setup(&alias, home(), &name)
        .await
        .map_err(err)
}
#[tauri::command]
pub(super) async fn cluster_set_agent_rules(
    app: AppHandle,
    alias: String,
    rules: chimaera_core::cluster::AgentRules,
) -> Result<(), String> {
    ensure_cluster(&app, &alias).await?;
    cluster::set_agent_rules(&alias, home(), &rules)
        .await
        .map_err(err)
}

#[tauri::command]
pub(super) async fn cluster_set_login_serve(
    state: State<'_, Shell>,
    alias: String,
    on: bool,
) -> Result<HostState, String> {
    tracing::info!("ipc: cluster_set_login_serve {alias} {on}");
    let owned = alias.clone();
    let entry = with_hosts(move |hosts| hosts.set_login_serve(&owned, on)).await?;
    lock(&state.host_entries).insert(alias.clone(), entry.clone());
    let info = lock(&state.clusters)
        .get(&alias)
        .and_then(|c| c.info.clone());
    Ok(state_for(&entry, "disconnected", None).with_cluster(&entry, info.as_ref()))
}

/// SIGTERM a daemon an earlier connect left on the cluster's login node
/// (never -9: it may still be saving sessions).
#[tauri::command]
pub(super) async fn cluster_stop_login_daemon(
    state: State<'_, Shell>,
    alias: String,
) -> Result<(), String> {
    tracing::info!("ipc: cluster_stop_login_daemon {alias}");
    if let Some((manifest, true)) = chimaera_remote::locate_daemon(&alias, home())
        .await
        .map_err(err)?
    {
        chimaera_remote::stop_remote(&alias, manifest.pid)
            .await
            .map_err(err)?;
    }
    if let Some(c) = lock(&state.clusters).get_mut(&alias) {
        if let Some(info) = &mut c.info {
            info.login_daemon = None;
        }
    }
    Ok(())
}

/// A blocking JSON request to the LOCAL daemon (bounded).
fn local_api(
    port: u16,
    token: &str,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let agent = crate::http::agent();
    let auth = format!("Bearer {token}");
    let timeout = Some(Duration::from_secs(20));
    let sent = match method {
        "POST" => agent
            .post(&url)
            .header("Authorization", &auth)
            .config()
            .timeout_global(timeout)
            .http_status_as_error(false)
            .build()
            .send_json(body),
        "GET" | "DELETE" => {
            let request = if method == "GET" {
                agent.get(&url)
            } else {
                agent.delete(&url)
            };
            request
                .header("Authorization", &auth)
                .config()
                .timeout_global(timeout)
                .http_status_as_error(false)
                .build()
                .call()
        }
        _ => return Err(format!("unsupported method {method}")),
    };
    let mut response = sent.map_err(|e| format!("the local daemon didn't answer: {e}"))?;
    let status = response.status();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("the local daemon's answer was unreadable: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(json!({}));
    if !status.is_success() {
        return Err(v
            .get("error")
            .and_then(|e| e.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("the local daemon answered HTTP {}", status.as_u16())));
    }
    Ok(v)
}

/// Where the login-node terminals' hidden workspace is rooted: an empty
/// folder of the app's own, so nothing ever scans a real one for it.
fn terminals_dir() -> std::path::PathBuf {
    chimaera_core::data_dir().join("cluster-terminals")
}

/// The local daemon's hidden workspace for login-node terminals (created if
/// needed): never listed on Home or anywhere else, and its sessions never
/// come back after a daemon restart.
fn terminals_workspace(port: u16, token: &str) -> Result<String, String> {
    let dir = terminals_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let v = local_api(
        port,
        token,
        "POST",
        "/api/v1/workspaces",
        json!({ "root": dir.to_string_lossy(), "hidden": true }),
    )?;
    v.get("id")
        .and_then(|i| i.as_str())
        .map(str::to_string)
        .ok_or_else(|| "the local daemon returned no workspace id".to_string())
}

/// Open a terminal-only window with an interactive `ssh <alias>` on the
/// cluster's login node — the user's own session over the app's connection.
/// The session lives in the hidden terminals workspace and only as long as
/// its window: closing the window ends it ([`terminal_window_closed`]), the
/// window is never restored, and one a crash left behind is ended at the
/// next launch ([`sweep_terminals`]).
#[tauri::command]
pub(super) async fn cluster_open_terminal(
    app: AppHandle,
    state: State<'_, Shell>,
    alias: String,
) -> Result<(), String> {
    let alias = chimaera_remote::hosts::normalize_alias(&alias).map_err(err)?;
    tracing::info!("ipc: cluster_open_terminal {alias}");
    let (port, token) = {
        let local = lock(&state.local);
        (local.port, local.token.clone())
    };
    let argv = chimaera_remote::interactive_ssh_argv(&alias);
    let prelude = format!(
        "exec {}",
        argv.iter()
            .map(|a| chimaera_core::slurm::sh_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let name = format!("{alias} · login node");
    let (ws, session) = {
        let (token, name) = (token.clone(), name.clone());
        tokio::task::spawn_blocking(move || {
            let ws = terminals_workspace(port, &token)?;
            let v = local_api(
                port,
                &token,
                "POST",
                "/api/v1/sessions",
                json!({ "workspace_id": ws, "kind": "shell", "name": name, "prelude": prelude }),
            )?;
            let session = v
                .get("id")
                .and_then(|i| i.as_str())
                .map(str::to_string)
                .ok_or("the local daemon returned no session id")?;
            Ok::<_, String>((ws, session))
        })
        .await
        .map_err(|e| e.to_string())??
    };
    let mut record = WindowRecord::new(None, Some(ws.clone()));
    record.width = Some(900.0);
    record.height = Some(580.0);
    let appearance = lock(&state.appearance).get(None);
    let url = super::restore::daemon_window_url(
        port,
        &token,
        &record.id,
        None,
        None,
        super::restore::WindowUrlKind::Workbench,
        appearance.as_ref(),
    )
    .map_err(|e| e.to_string())?;
    let url = format!("{url}&term={}", urlencoding::encode(&session));
    // Detached: never the target of an "open this workspace" raise, and no
    // Home duty. The tray names it before the page could.
    let mut scope = super::WindowScope::new_detached(None, Some(ws), record.id.clone());
    scope.label = name.clone();
    lock(&state.terminal_windows).insert(record.id.clone(), session.clone());
    if let Err(e) = super::restore::open_shell_window(&app, &url, &name, &record, scope) {
        lock(&state.terminal_windows).remove(&record.id);
        end_terminal_session(port, token, session);
        return Err(format!("could not open window: {e}"));
    }
    // Never restored: its session ends with the window.
    lock(&state.registry).remove(&record.id);
    Ok(())
}

/// End one login-node terminal session (its ssh) on the local daemon.
fn end_terminal_session(port: u16, token: String, session: String) {
    let path = format!("/api/v1/sessions/{}", urlencoding::encode(&session));
    if let Err(e) = local_api(port, &token, "DELETE", &path, json!(null)) {
        tracing::warn!("could not end login-node terminal {session}: {e}");
    }
}

/// A window closed: if it was a login-node terminal, end its session — it
/// exists only for that window. Inline while the app quits (a spawned thread
/// would die with the process), on a thread otherwise.
pub(crate) fn terminal_window_closed(app: &AppHandle, stable_id: &str, quitting: bool) {
    let shell = app.state::<Shell>();
    let Some(session) = lock(&shell.terminal_windows).remove(stable_id) else {
        return;
    };
    let (port, token) = {
        let local = lock(&shell.local);
        (local.port, local.token.clone())
    };
    if quitting {
        end_terminal_session(port, token, session);
    } else {
        std::thread::spawn(move || end_terminal_session(port, token, session));
    }
}

/// The app is exiting: end every open login-node terminal, inline.
pub(crate) fn end_all_terminals(app: &AppHandle) {
    let shell = app.state::<Shell>();
    let sessions: Vec<String> = lock(&shell.terminal_windows)
        .drain()
        .map(|(_, s)| s)
        .collect();
    if sessions.is_empty() {
        return;
    }
    let (port, token) = {
        let local = lock(&shell.local);
        (local.port, local.token.clone())
    };
    for session in sessions {
        end_terminal_session(port, token.clone(), session);
    }
}

/// At launch, end login-node terminal sessions a crash or force quit left
/// without a window (their windows are never restored, so every live one is
/// an orphan). Nothing when no terminal was ever opened here.
pub(crate) fn sweep_terminals(port: u16, token: String) {
    if !terminals_dir().is_dir() {
        return;
    }
    let ws = match terminals_workspace(port, &token) {
        Ok(ws) => ws,
        Err(e) => {
            tracing::warn!("login-node terminal sweep skipped: {e}");
            return;
        }
    };
    let sessions = match local_api(port, &token, "GET", "/api/v1/sessions", json!(null)) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("login-node terminal sweep skipped: {e}");
            return;
        }
    };
    for s in sessions.as_array().into_iter().flatten() {
        if s.get("workspace_id").and_then(|w| w.as_str()) != Some(ws.as_str())
            || s.get("alive").and_then(|a| a.as_bool()) != Some(true)
        {
            continue;
        }
        if let Some(id) = s.get("id").and_then(|i| i.as_str()) {
            tracing::info!("ending login-node terminal {id} left without a window");
            end_terminal_session(port, token.clone(), id.to_string());
        }
    }
}
