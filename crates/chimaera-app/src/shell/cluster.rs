//! Cluster workspaces in the native shell: the `cluster_*` commands behind the
//! cluster page, job windows, the "continue on a new node" handoff, and the
//! notifications that tell the user when a job they started is ready, about
//! to stop, or gone.
//!
//! Nothing here keeps anything running on a cluster's login node. Every call
//! is a short ssh exec through the ControlMaster (`chimaera_remote::cluster`);
//! the only long-lived things are this process's own `ssh -L` forwards to the
//! user's jobs and, for partitions that take only interactive jobs, the
//! foreground `srun` the user asked for — both end with this app.
//!
//! Ports and tokens never reach a page: the overview's endpoints stay here,
//! and a job window's URL is the only carrier of its token.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chimaera_core::slurm::LaunchSpec;
use chimaera_remote::cluster::{self, ClusterOverview, StartOutcome, StartRefused, StartRequest};
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
    /// Running workspaces' endpoints, from the last overview.
    endpoints: HashMap<String, cluster::Endpoint>,
    names: HashMap<String, String>,
    /// Last seen (state, job) per workspace — the notification diff.
    last: HashMap<String, (String, Option<String>)>,
    /// One notification per (kind, job).
    notified: HashSet<String>,
    /// Slurm start estimates for waiting jobs, asked at most every 5 min.
    estimates: HashMap<String, (Instant, Option<u64>)>,
    /// Jobs whose end reason was already asked of accounting.
    ended_asked: HashSet<String>,
    /// Workspaces mid-handoff: workspace → the old job being replaced.
    handoff: HashMap<String, String>,
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

fn compute_key(alias: &str, job_id: &str) -> String {
    format!("{alias}#job{job_id}")
}

fn valid_ws(id: &str) -> Result<(), String> {
    if chimaera_core::cluster::valid_workspace_id(id) {
        Ok(())
    } else {
        Err("unknown workspace".to_string())
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
            "{alias} runs {}; starting workspaces is only supported on Slurm so far",
            s.kind.tag()
        )),
        _ => Err(format!("{alias} has no batch scheduler on its login PATH")),
    }
}

/// Words for a Slurm terminal state, for notifications and the ended event.
fn ended_words(state: &str) -> &'static str {
    match state {
        "TIMEOUT" => "hit its time limit",
        "CANCELLED" => "was cancelled",
        "FAILED" => "failed",
        "PREEMPTED" => "was preempted",
        "NODE_FAIL" => "lost its node",
        "OUT_OF_MEMORY" => "ran out of memory",
        "COMPLETED" => "finished",
        _ => "ended",
    }
}

fn toast(alias: &str, ws: &str, kind: &str, job: &str, title: String, body: String) {
    crate::notify::post(crate::notify::Toast {
        // Not a `chimaera` route id: a click just brings the app forward.
        id: format!("cluster-{alias}-{ws}-{kind}-{job}"),
        thread: format!("cluster/{alias}"),
        title,
        subtitle: alias.to_string(),
        body,
        sound: false,
    });
}

/// Take a fresh overview into this process's state: endpoints, the
/// notification diff, ended jobs' windows, one accounting question per
/// ended job, and the watcher when anything is still alive.
async fn absorb(app: &AppHandle, alias: &str, ov: &ClusterOverview) {
    let shell = app.state::<Shell>();
    let mut ended_events: Vec<(String, String)> = Vec::new();
    let mut ask_end: Vec<(String, chimaera_core::cluster::LaunchRecord)> = Vec::new();
    let mut live = false;
    {
        let mut clusters = lock(&shell.clusters);
        let c = clusters.entry(alias.to_string()).or_default();
        c.endpoints = ov.endpoints.clone();
        for w in &ov.workspaces {
            c.names.insert(w.id.clone(), w.name.clone());
            live |= w.state != "stopped";
            let now = (w.state.to_string(), w.job_id.clone());
            let prev = c.last.insert(w.id.clone(), now.clone());
            let job = w.job_id.clone().unwrap_or_default();
            if let Some((prev_state, prev_job)) = prev {
                // Ready: a job this process saw waiting or starting runs.
                if w.state == "running" && prev_state != "running" {
                    let key = format!("ready:{job}");
                    if c.notified.insert(key) {
                        toast(
                            alias,
                            &w.id,
                            "ready",
                            &job,
                            format!("{} is ready", w.name),
                            format!("Running on {}.", w.node),
                        );
                    }
                }
                // Gone: the job a window may show left the queue — unless a
                // handoff is replacing it (that window moves, it doesn't end).
                if prev_state == "running" || prev_state == "starting" {
                    if let Some(old) = prev_job.filter(|old| Some(old) != now.1.as_ref()) {
                        if c.handoff.get(&w.id) != Some(&old) {
                            ended_events.push((old.clone(), "ENDED".into()));
                        }
                    } else if w.state == "stopped" {
                        let reason = if w.stopped_by_user {
                            "stopped".to_string()
                        } else {
                            w.ended.clone().unwrap_or_else(|| "ENDED".into())
                        };
                        if !w.stopped_by_user && c.notified.insert(format!("ended:{job}")) {
                            toast(
                                alias,
                                &w.id,
                                "ended",
                                &job,
                                format!("{} stopped", w.name),
                                format!("Its job {}. Chats are saved.", ended_words(&reason)),
                            );
                        }
                        ended_events.push((job.clone(), reason));
                    }
                }
            }
            if w.state == "running" {
                if let Some(end) = w.ends_at_ms {
                    let left = end.saturating_sub(ov.now_ms);
                    if left < 10 * 60_000 {
                        if c.notified.insert(format!("10m:{job}")) {
                            toast(
                                alias,
                                &w.id,
                                "10m",
                                &job,
                                format!("{} stops in 10 minutes", w.name),
                                "Its job's time is almost up.".into(),
                            );
                        }
                    } else if left < 60 * 60_000 && c.notified.insert(format!("1h:{job}")) {
                        toast(
                            alias,
                            &w.id,
                            "1h",
                            &job,
                            format!(
                                "{} stops in {}",
                                w.name,
                                if left >= 50 * 60_000 {
                                    "about an hour".to_string()
                                } else {
                                    format!("{} minutes", left / 60_000)
                                }
                            ),
                            "Continue on a new node from its window to keep working.".into(),
                        );
                    }
                }
            }
            if w.state == "stopped" && w.ended.is_none() && !w.stopped_by_user && !w.fresh {
                if let Some(rec) = ov.records.get(&w.id) {
                    if let Some(id) = &rec.job_id {
                        if c.ended_asked.insert(id.clone()) {
                            ask_end.push((w.id.clone(), rec.clone()));
                        }
                    }
                }
            }
        }
    }
    for (job, reason) in ended_events {
        let key = compute_key(alias, &job);
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
                reason: Some(reason),
                build: None,
                node: None,
            },
        );
    }
    if !ask_end.is_empty() {
        let app = app.clone();
        let alias = alias.to_string();
        tauri::async_runtime::spawn(async move {
            for (ws, rec) in ask_end {
                if let Err(e) = cluster::record_end(&alias, home(), &ws, &rec).await {
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

/// Watch a cluster while a job this app knows of is alive: the queue at most
/// once a minute while something waits or starts, every five minutes while
/// things only run (an early end — failure, preemption — still gets told),
/// and not at all once nothing is alive. It ends with the app.
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
            let waiting = ov
                .workspaces
                .iter()
                .any(|w| w.state == "waiting" || w.state == "starting");
            let alive = ov.workspaces.iter().any(|w| w.state != "stopped");
            if !alive {
                break;
            }
            interval = if waiting {
                cluster::SQUEUE_FLOOR
            } else {
                Duration::from_secs(300)
            };
        }
        let shell = app.state::<Shell>();
        let mut clusters = lock(&shell.clusters);
        if let Some(c) = clusters.get_mut(&alias) {
            c.watching = false;
        }
    });
}

/// The overview as a page gets it: no endpoints, plus Slurm's start
/// estimate for waiting jobs (asked at most every five minutes per job).
async fn page_overview(app: &AppHandle, alias: &str) -> Result<serde_json::Value, String> {
    let ov = match cluster::overview(alias, home()).await {
        Ok(ov) => ov,
        // After laptop sleep the ControlMaster often survives its dead link
        // and every exec queues on it; clear it (when it really is wedged)
        // and try once more on a fresh one.
        Err(first) => {
            if chimaera_remote::clear_wedged_master(alias, 15).await {
                cluster::overview(alias, home()).await.map_err(err)?
            } else {
                return Err(err(first));
            }
        }
    };
    absorb(app, alias, &ov).await;
    let shell = app.state::<Shell>();
    let mut wanted: Vec<String> = Vec::new();
    let mut estimates: HashMap<String, Option<u64>> = HashMap::new();
    {
        let clusters = lock(&shell.clusters);
        let c = clusters.get(alias);
        for w in ov.workspaces.iter().filter(|w| w.state == "waiting") {
            let Some(job) = &w.job_id else { continue };
            match c.and_then(|c| c.estimates.get(job)) {
                Some((at, est)) if at.elapsed() < Duration::from_secs(300) => {
                    estimates.insert(job.clone(), *est);
                }
                _ => wanted.push(job.clone()),
            }
        }
    }
    for job in wanted {
        let est = cluster::start_estimate(alias, &job).await.ok().flatten();
        estimates.insert(job.clone(), est);
        lock(&shell.clusters)
            .entry(alias.to_string())
            .or_default()
            .estimates
            .insert(job, (Instant::now(), est));
    }
    let mut v = serde_json::to_value(&ov).map_err(|e| e.to_string())?;
    if let Some(list) = v.get_mut("workspaces").and_then(|w| w.as_array_mut()) {
        for w in list {
            let est = w
                .get("job_id")
                .and_then(|j| j.as_str())
                .and_then(|j| estimates.get(j).copied().flatten());
            if let (Some(obj), Some(est)) = (w.as_object_mut(), est) {
                obj.insert("start_estimate_ms".into(), json!(est));
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
) -> Result<cluster::ClusterFacts, String> {
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

/// Put this build's binary on the cluster (a checksum compare, a copy only
/// when it differs) — the progress rides the host row like a connect's.
async fn ensure_binary(app: &AppHandle, alias: &str) -> Result<(), String> {
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
    .map_err(err)
}

/// Start (or continue) a workspace's job and translate the outcome for the
/// page. A refusal is an answer, not an error: its words are the cluster's
/// own, and what it teaches is remembered.
async fn start_job(
    app: &AppHandle,
    alias: &str,
    workspace_id: &str,
    spec: &LaunchSpec,
    run_startup: &str,
    attached: bool,
) -> Result<(StartOutcome, chimaera_core::slurm::GpuFlag), serde_json::Value> {
    let fail = |m: String| json!({ "kind": "error", "message": m });
    let (config, _) = cluster::read_config(alias, home())
        .await
        .map_err(|e| fail(err(e)))?;
    let ws = config
        .workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .cloned()
        .ok_or_else(|| fail("unknown workspace".into()))?;
    let facts = cluster::facts(alias, false)
        .await
        .map_err(|e| fail(err(e)))?;
    ensure_binary(app, alias).await.map_err(fail)?;
    let req = StartRequest {
        workspace: &ws,
        spec,
        run_startup,
        attached,
        gpu_flag: facts.gpu_flag,
    };
    match cluster::start(alias, home(), &config, &req).await {
        Ok(outcome) => Ok((outcome, facts.gpu_flag)),
        Err(e) => match e.downcast_ref::<StartRefused>() {
            Some(refused) => {
                if let Err(e) =
                    cluster::learn_refusal(alias, home(), spec.partition.as_deref(), refused.kind)
                        .await
                {
                    tracing::debug!("could not remember what {alias} refused: {e:#}");
                }
                Err(json!({
                    "kind": "refused",
                    "message": refused.message,
                    "refusal": refused.kind,
                }))
            }
            None => Err(fail(err(e))),
        },
    }
}

/// Hold an attached job in the foreground for as long as this app runs (or
/// until the user stops it). Dropping the child — app quit included — ends
/// the job.
fn hold_attached(
    app: &AppHandle,
    alias: &str,
    workspace_id: &str,
    mut child: tokio::process::Child,
) {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let shell = app.state::<Shell>();
    let key = (alias.to_string(), workspace_id.to_string());
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

#[tauri::command]
#[allow(clippy::too_many_arguments)] // the page's start sheet, one field each
pub(super) async fn cluster_start(
    app: AppHandle,
    alias: String,
    workspace_id: String,
    spec: LaunchSpec,
    run_startup: String,
    save_as: Option<String>,
    attached: bool,
) -> Result<serde_json::Value, String> {
    valid_ws(&workspace_id)?;
    ensure_cluster(&app, &alias).await?;
    tracing::info!("ipc: cluster_start {alias} {workspace_id} (attached: {attached})");
    let spec = spec.normalized();
    spec.validate()?;
    let ov = cluster::overview(&alias, home()).await.map_err(err)?;
    if let Some(v) = ov.workspaces.iter().find(|w| w.id == workspace_id) {
        if v.state != "stopped" {
            return Err(format!("{} is already {}", v.name, v.state));
        }
    }
    let outcome = match start_job(&app, &alias, &workspace_id, &spec, &run_startup, attached).await
    {
        Ok(outcome) => outcome,
        Err(answer) if answer["kind"] == "error" => {
            return Err(answer["message"]
                .as_str()
                .unwrap_or("start failed")
                .to_string())
        }
        Err(answer) => return Ok(answer),
    };
    if let Err(e) =
        cluster::remember_spec(&alias, home(), &workspace_id, &spec, save_as.as_deref()).await
    {
        tracing::debug!("could not remember the setup on {alias}: {e:#}");
    }
    let answer = match outcome {
        (StartOutcome::Submitted { job_id }, _) => json!({ "kind": "submitted", "job_id": job_id }),
        (StartOutcome::Attached { job_name }, gpu_flag) => {
            let child =
                cluster::spawn_attached(&alias, home(), &workspace_id, &spec, &job_name, gpu_flag)
                    .map_err(err)?;
            hold_attached(&app, &alias, &workspace_id, child);
            json!({ "kind": "attached", "job_name": job_name })
        }
    };
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    ensure_watcher(&app, &alias);
    Ok(answer)
}

#[tauri::command]
pub(super) async fn cluster_stop(
    app: AppHandle,
    state: State<'_, Shell>,
    alias: String,
    workspace_id: String,
) -> Result<(), String> {
    valid_ws(&workspace_id)?;
    ensure_cluster(&app, &alias).await?;
    tracing::info!("ipc: cluster_stop {alias} {workspace_id}");
    let ov = cluster::overview(&alias, home()).await.map_err(err)?;
    let view = ov
        .workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .ok_or("unknown workspace")?;
    let record = ov
        .records
        .get(&workspace_id)
        .ok_or_else(|| format!("{} has never been started", view.name))?;
    cluster::stop(&alias, home(), &workspace_id, record)
        .await
        .map_err(err)?;
    if let Some(tx) = lock(&state.attached_jobs).remove(&(alias.clone(), workspace_id.clone())) {
        let _ = tx.send(());
    }
    if let Some(job) = &view.job_id {
        {
            let mut clusters = lock(&state.clusters);
            let c = clusters.entry(alias.clone()).or_default();
            c.last.insert(
                workspace_id.clone(),
                ("stopped".into(), view.job_id.clone()),
            );
            c.endpoints.remove(&workspace_id);
        }
        let key = compute_key(&alias, job);
        let tunnel = state.compute_tunnels.lock().await.remove(&key);
        lock(&state.unhealthy_tunnels).remove(&key);
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
                reason: Some("stopped".into()),
                build: None,
                node: None,
            },
        );
    }
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    Ok(())
}

/// Reuse or build the forward to a running workspace's job and open (or
/// raise) its window.
async fn open_job_window(
    app: &AppHandle,
    alias: &str,
    workspace_id: &str,
    endpoint: &cluster::Endpoint,
    name: &str,
) -> Result<(), String> {
    let shell = app.state::<Shell>();
    let key = compute_key(alias, &endpoint.job_id);
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
            &endpoint.job_id,
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
            format!("{}&cws={workspace_id}", t.url()),
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
                job_id: endpoint.job_id.clone(),
                node: node.clone(),
            });
            let title = format!("{alias} › {node} — {name}");
            open_compute_window(app, &url, &title, &record, &key)
                .map_err(|e| format!("could not open window: {e}"))?;
        }
    }
    lock(&shell.unhealthy_tunnels).remove(&key);
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

#[tauri::command]
pub(super) async fn cluster_open(
    app: AppHandle,
    alias: String,
    workspace_id: String,
) -> Result<(), String> {
    valid_ws(&workspace_id)?;
    ensure_cluster(&app, &alias).await?;
    tracing::info!("ipc: cluster_open {alias} {workspace_id}");
    // Always a fresh read: a cached endpoint may name an earlier job.
    cluster::invalidate_queue(&alias);
    let ov = cluster::overview(&alias, home()).await.map_err(err)?;
    absorb(&app, &alias, &ov).await;
    let view = ov
        .workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .ok_or("unknown workspace")?;
    let endpoint = ov
        .endpoints
        .get(&workspace_id)
        .ok_or_else(|| match view.state {
            "waiting" => format!("{} is still waiting for a node", view.name),
            "starting" => format!("{} is starting — it opens in a moment", view.name),
            _ => format!("{} isn't running", view.name),
        })?;
    open_job_window(&app, &alias, &workspace_id, endpoint, &view.name).await
}

/// Continue a running workspace on a new node: queue the next job with the
/// same setup now; once it has a node, stop the old job (its chimaera saves
/// the chats; the new one waits for that, then resumes them); once the new
/// chimaera answers, move the window over.
#[tauri::command]
pub(super) async fn cluster_continue(
    app: AppHandle,
    alias: String,
    workspace_id: String,
) -> Result<serde_json::Value, String> {
    valid_ws(&workspace_id)?;
    ensure_cluster(&app, &alias).await?;
    tracing::info!("ipc: cluster_continue {alias} {workspace_id}");
    let ov = cluster::overview(&alias, home()).await.map_err(err)?;
    let view = ov
        .workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .ok_or("unknown workspace")?
        .clone();
    if view.state != "running" {
        return Err(format!("{} isn't running", view.name));
    }
    let record = ov
        .records
        .get(&workspace_id)
        .cloned()
        .ok_or("no job record")?;
    if record.attached {
        return Err(format!(
            "{} runs attached to this app; start a new one when it stops",
            view.name
        ));
    }
    let old_job = view.job_id.clone().ok_or("no job id")?;
    let spec = view.last_spec.clone().unwrap_or(record.spec.clone());
    let new_job = match start_job(&app, &alias, &workspace_id, &spec, "", false).await {
        Ok((StartOutcome::Submitted { job_id }, _)) => job_id,
        Ok((StartOutcome::Attached { .. }, _)) => return Err("unexpected attached start".into()),
        Err(answer) if answer["kind"] == "error" => {
            return Err(answer["message"]
                .as_str()
                .unwrap_or("start failed")
                .to_string())
        }
        Err(answer) => return Ok(answer),
    };
    {
        let shell = app.state::<Shell>();
        lock(&shell.clusters)
            .entry(alias.clone())
            .or_default()
            .handoff
            .insert(workspace_id.clone(), old_job.clone());
    }
    let task_app = app.clone();
    let task_alias = alias.clone();
    let task_ws = workspace_id.clone();
    let task_new = new_job.clone();
    tauri::async_runtime::spawn(async move {
        handoff(
            &task_app,
            &task_alias,
            &task_ws,
            &old_job,
            &task_new,
            &view.name,
        )
        .await;
        {
            let shell = task_app.state::<Shell>();
            let mut clusters = lock(&shell.clusters);
            if let Some(c) = clusters.get_mut(&task_alias) {
                c.handoff.remove(&task_ws);
            }
        }
        let _ = task_app.emit("cluster-changed", json!({ "alias": task_alias }));
    });
    let _ = app.emit("cluster-changed", json!({ "alias": alias }));
    Ok(json!({ "kind": "submitted", "job_id": new_job }))
}

async fn handoff(app: &AppHandle, alias: &str, ws: &str, old_job: &str, new_job: &str, name: &str) {
    let mut stopped_old = false;
    // Bounded by the old job's own walltime in practice; the cap only keeps a
    // stuck queue from holding this task forever.
    let deadline = Instant::now() + Duration::from_secs(7 * 24 * 3600);
    while Instant::now() < deadline {
        tokio::time::sleep(cluster::SQUEUE_FLOOR).await;
        let ov = match cluster::overview(alias, home()).await {
            Ok(ov) => ov,
            Err(e) => {
                tracing::debug!("handoff {alias}/{ws}: {e:#}");
                continue;
            }
        };
        absorb(app, alias, &ov).await;
        let Some(view) = ov.workspaces.iter().find(|w| w.id == ws) else {
            return;
        };
        if view.job_id.as_deref() != Some(new_job) && view.state != "waiting" {
            // The record moved on (stopped, or another start replaced it).
            return;
        }
        match view.state {
            "waiting" => {}
            // The new job has a node; its chimaera waits for the old one.
            "starting" if !stopped_old => match cluster::cancel_job(alias, old_job).await {
                Ok(()) => stopped_old = true,
                Err(e) => tracing::warn!("handoff {alias}/{ws}: stopping job {old_job}: {e:#}"),
            },
            "starting" => {}
            "running" => {
                if !stopped_old {
                    let _ = cluster::cancel_job(alias, old_job).await;
                }
                let shell = app.state::<Shell>();
                let old_key = compute_key(alias, old_job);
                let label = super::commands::find_by_alias(&shell.windows, &old_key);
                let tunnel = shell.compute_tunnels.lock().await.remove(&old_key);
                if let Some(t) = tunnel {
                    t.close().await;
                }
                if let Some(endpoint) = ov.endpoints.get(ws) {
                    if label.is_some() {
                        if let Err(e) = open_job_window(app, alias, ws, endpoint, name).await {
                            tracing::warn!("handoff {alias}/{ws}: opening the new window: {e}");
                        }
                    }
                }
                if let Some(win) = label.and_then(|l| app.get_webview_window(&l)) {
                    let _ = win.close();
                }
                toast(
                    alias,
                    ws,
                    "moved",
                    new_job,
                    format!("{name} continues on {}", view.node),
                    "Your chats moved with it.".into(),
                );
                return;
            }
            _ => {
                toast(
                    alias,
                    ws,
                    "handoff-failed",
                    new_job,
                    format!("{name}'s new job ended before it started"),
                    "The current one keeps running until its time is up.".into(),
                );
                return;
            }
        }
    }
}

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
    let request = match method {
        "POST" => crate::http::agent().post(&url),
        _ => return Err(format!("unsupported method {method}")),
    };
    let mut response = request
        .header("Authorization", &format!("Bearer {token}"))
        .config()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .send_json(body)
        .map_err(|e| format!("the local daemon didn't answer: {e}"))?;
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

/// A local workspace for `dir` (created if needed) — the place a login-node
/// terminal or a peeked file opens in.
fn local_workspace(port: u16, token: &str, dir: &std::path::Path) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let v = local_api(
        port,
        token,
        "POST",
        "/api/v1/workspaces",
        json!({ "root": dir.to_string_lossy() }),
    )?;
    v.get("id")
        .and_then(|i| i.as_str())
        .map(str::to_string)
        .ok_or_else(|| "the local daemon returned no workspace id".to_string())
}

/// Open a window with an interactive `ssh <alias>` terminal on the cluster's
/// login node — the user's own session over the app's connection, ending
/// when its tab closes.
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
    let home_dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .ok_or("no home folder")?;
    let name = format!("{alias} · login node");
    let (ws, session) = tokio::task::spawn_blocking(move || {
        let ws = local_workspace(port, &token, &home_dir)?;
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
    .map_err(|e| e.to_string())??;
    super::notices::expect_focus(&app, None, ws.clone(), session);
    let token = lock(&state.local).token.clone();
    let record = WindowRecord::new(None, Some(ws));
    super::open_ui_window(&app, port, &token, &record)
        .map_err(|e| format!("could not open window: {e}"))
}

#[tauri::command]
pub(super) async fn cluster_list(
    app: AppHandle,
    alias: String,
    path: String,
) -> Result<cluster::Listing, String> {
    ensure_cluster(&app, &alias).await?;
    cluster::list_dir(&alias, &path).await.map_err(err)
}

/// Copy one file from the cluster into a local peek folder and open it in a
/// window on that folder (`open=` asks the page to show it).
#[tauri::command]
pub(super) async fn cluster_peek(
    app: AppHandle,
    state: State<'_, Shell>,
    alias: String,
    path: String,
) -> Result<(), String> {
    ensure_cluster(&app, &alias).await?;
    let name = path
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("file")
        .chars()
        .map(|c| if c == '/' || c == '\0' { '_' } else { c })
        .collect::<String>();
    if name == "." || name == ".." {
        return Err(format!("{path} isn't a file"));
    }
    let safe_alias: String = alias
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = chimaera_core::data_dir().join("peek").join(safe_alias);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = dir.join(&name);
    cluster::fetch_file(&alias, &path, &dest)
        .await
        .map_err(err)?;
    // The daemon registers workspace roots canonically, and the page accepts
    // `open=` only inside the root as written — both must agree.
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let dest = std::fs::canonicalize(&dest).unwrap_or(dest);
    let (port, token) = {
        let local = lock(&state.local);
        (local.port, local.token.clone())
    };
    let ws = {
        let (dir, token) = (dir.clone(), token.clone());
        tokio::task::spawn_blocking(move || local_workspace(port, &token, &dir))
            .await
            .map_err(|e| e.to_string())??
    };
    let record = WindowRecord::new(None, Some(ws.clone()));
    let url = super::restore::daemon_window_url(
        port,
        &token,
        &record.id,
        Some(&ws),
        None,
        super::restore::WindowUrlKind::Workbench,
        None,
    )
    .map_err(|e| e.to_string())?;
    let url = format!(
        "{url}&open={}",
        urlencoding::encode(&dest.to_string_lossy())
    );
    let scope = super::WindowScope::new(None, Some(ws), record.id.clone());
    super::restore::open_shell_window(&app, &url, &format!("{name} — {alias}"), &record, scope)
        .map_err(|e| format!("could not open window: {e}"))
}
