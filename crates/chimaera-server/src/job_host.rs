//! `chimaera job-host`: the main process of a cluster job. It runs on the
//! compute node Slurm gave the job — never on a login node — and lives
//! exactly as long as the job (the job script `exec`s it, so walltime and
//! `scancel` stop it). It opens and closes **workspaces** inside the job:
//! one `chimaera serve` per open workspace, each over that workspace's own
//! data folder (its chats travel with it, and its manifest is the lease that
//! keeps it open in one job at a time).
//!
//! The app reaches it with a plain `ssh -L` through the login node, like a
//! workspace's own chimaera, and drives it over a small token-gated API:
//!
//! - `GET  /api/v1/health` — the tunnel's proof of life;
//! - `GET  /api/v1/job` — [`JobHostStatus`];
//! - `POST /api/v1/job/workspaces/{wid}/open` — open (409 when another live
//!   job holds it);
//! - `POST /api/v1/job/workspaces/{wid}/close` — close (its chimaera saves
//!   its chats and exits).
//!
//! Nothing here restarts on its own: a workspace whose chimaera exits is
//! reported `failed` until the user opens it again. No polling — job-host
//! waits on its children and its socket.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chimaera_core::cluster::{
    valid_job_id, valid_workspace_id, ClusterConfig, HeldElsewhere, HostRecord, HostedState,
    HostedWorkspace, HostingRecord, JobHostStatus, JobRecord, WorkspaceSeed, ENV_AGENT_RULES_FILE,
    ENV_CLUSTER_FACTS_FILE, ENV_CLUSTER_WORKSPACE, ENV_DATA_DIR, ENV_HOST_PRELUDE_FILE,
    ENV_JOB_ATTACHED, ENV_RUNTIME_DIR,
};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::watch;

/// How long a closing workspace gets to save its chats before it is killed.
/// Under Slurm's usual kill wait (30 s) so a job's end still lets every
/// workspace finish.
const CLOSE_GRACE: Duration = Duration::from_secs(25);
/// The last lines of a workspace's output kept for a `failed` report.
const TAIL_LINES: usize = 6;

struct Hosted {
    state: HostedState,
    port: Option<u16>,
    pid: Option<u32>,
    detail: String,
    tail: Arc<Mutex<VecDeque<String>>>,
    /// Flips to true when its chimaera exits. `None` while the slot is only
    /// reserved: an open is preparing it and nothing runs yet.
    exited: Option<watch::Receiver<bool>>,
}

impl Hosted {
    fn reserved() -> Self {
        Hosted {
            state: HostedState::Starting,
            port: None,
            pid: None,
            detail: String::new(),
            tail: Arc::new(Mutex::new(VecDeque::new())),
            exited: None,
        }
    }
}

struct Host {
    job_dir: PathBuf,
    cluster_dir: PathBuf,
    record: JobRecord,
    /// This job's Slurm id ("local" outside Slurm — tests).
    own_slurm: String,
    /// The Slurm id of the job this one replaces, once stopped.
    replaced_slurm: Option<String>,
    token: String,
    exe: PathBuf,
    runtime_base: PathBuf,
    workspaces: Mutex<HashMap<String, Hosted>>,
    stopping: AtomicBool,
    /// Wakes the writer of `workspaces.json` after every change.
    changed: tokio::sync::Notify,
    /// Every workspace closed: the writer removes the endpoint and stops.
    finished: AtomicBool,
}

impl Host {
    fn hosting(&self) -> HostingRecord {
        let map = self.workspaces.lock().unwrap_or_else(|p| p.into_inner());
        HostingRecord {
            workspaces: map.iter().map(|(id, h)| (id.clone(), h.state)).collect(),
        }
    }
}

fn write_hosting(job_dir: &Path, record: &HostingRecord) {
    if let Ok(bytes) = serde_json::to_vec(record) {
        if let Err(e) =
            chimaera_core::atomic_write_private(&job_dir.join("workspaces.json"), &bytes)
        {
            tracing::warn!("could not write workspaces.json: {e:#}");
        }
    }
}

/// Keep `workspaces.json` current: one writer, so a newer state never lands
/// under an older one; a burst of changes is one write. At the end it
/// removes the endpoint itself. The final closing rows remain on disk until
/// the allocation ends, so a fresh browser still knows which job owns them.
async fn publish_hosting(host: Arc<Host>) {
    loop {
        host.changed.notified().await;
        let job_dir = host.job_dir.clone();
        if host.finished.load(Ordering::Relaxed) {
            let record = host.hosting();
            let _ = tokio::task::spawn_blocking(move || {
                write_hosting(&job_dir, &record);
                let _ = std::fs::remove_file(job_dir.join("host.json"));
            })
            .await;
            return;
        }
        let record = host.hosting();
        let _ = tokio::task::spawn_blocking(move || write_hosting(&job_dir, &record)).await;
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Run job-host for the job whose folder is `job_dir` (`…/cluster/j/<jid>`)
/// until SIGTERM, then close every workspace and exit.
pub async fn run(job_dir: PathBuf) -> anyhow::Result<()> {
    // The app writes job.json the moment sbatch answers (job.pending until
    // then), so a job Slurm starts at once can get here first: wait for it.
    let mut record: Option<JobRecord> = read_json(&job_dir.join("job.json"));
    for _ in 0..60 {
        if record.is_some() || !job_dir.join("job.pending").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        record = read_json(&job_dir.join("job.json"));
    }
    let record =
        record.with_context(|| format!("no readable job.json in {}", job_dir.display()))?;
    anyhow::ensure!(valid_job_id(&record.id), "job.json names no valid job");
    let cluster_dir = job_dir
        .parent()
        .and_then(Path::parent)
        .context("the job folder isn't inside a cluster folder")?
        .to_path_buf();
    let own_slurm = std::env::var("SLURM_JOB_ID")
        .ok()
        .filter(|j| !j.trim().is_empty())
        .unwrap_or_else(|| "local".to_string());
    let exe = std::env::current_exe().context("cannot find this chimaera binary")?;
    let uid = {
        #[cfg(unix)]
        {
            nix::unistd::getuid().as_raw().to_string()
        }
        #[cfg(not(unix))]
        {
            "0".to_string()
        }
    };
    let runtime_base = PathBuf::from(format!("/tmp/chimaera-{uid}-{own_slurm}"));

    // "Continue in a new job": stop the job this one replaces, as the user
    // asked when they started it — its workspaces save their chats and let
    // go, and the ones opened below take them over (each waits on its
    // workspace's lease). Never anything but the one job named.
    let replaced_slurm = stop_replaced(&cluster_dir, &record, &own_slurm).await;

    // What opens at start is "opening" from the first moment the page can
    // see this job running (host.json, below).
    write_hosting(
        &job_dir,
        &HostingRecord {
            workspaces: record
                .open
                .iter()
                .filter(|w| valid_workspace_id(w))
                .map(|w| (w.clone(), HostedState::Starting))
                .collect(),
        },
    );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:0")
        .await
        .context("job-host could not listen")?;
    let port = listener.local_addr()?.port();
    let token = chimaera_core::generate_token();
    let node = chimaera_core::this_node().unwrap_or_default();
    let host_record = HostRecord {
        job: record.id.clone(),
        slurm_job_id: own_slurm.clone(),
        node: node.clone(),
        port,
        token: token.clone(),
        pid: std::process::id(),
        started_ms: now_ms(),
        build: chimaera_core::BUILD_ID.to_string(),
    };
    chimaera_core::atomic_write_private(
        &job_dir.join("host.json"),
        &serde_json::to_vec_pretty(&host_record)?,
    )
    .context("could not write host.json")?;
    tracing::info!(job = %record.id, slurm = %own_slurm, %node, port, "job-host up");

    let host = Arc::new(Host {
        job_dir: job_dir.clone(),
        cluster_dir,
        record: record.clone(),
        own_slurm,
        replaced_slurm,
        token,
        exe,
        runtime_base,
        workspaces: Mutex::new(HashMap::new()),
        stopping: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
        finished: AtomicBool::new(false),
    });
    let publisher = tokio::spawn(publish_hosting(host.clone()));

    for wid in &record.open {
        match open(&host, wid).await {
            Ok(_) => {}
            Err(e) => tracing::warn!(workspace = %wid, "could not open at start: {}", e.message()),
        }
    }

    let app = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/job", get(status_route))
        .route("/api/v1/job/workspaces/{wid}/open", post(open_route))
        .route("/api/v1/job/workspaces/{wid}/close", post(close_route))
        .layer(middleware::from_fn_with_state(host.clone(), auth))
        .with_state(host.clone());
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    server.await.context("job-host server failed")?;

    tracing::info!("job-host stopping: closing every workspace");
    close_all(&host).await;
    host.finished.store(true, Ordering::Relaxed);
    host.changed.notify_one();
    let _ = publisher.await;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(term) => term,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        // An attached job's session hanging up (the app that held it quit
        // or lost its connection) is a stop too: close every workspace, as
        // for walltime or scancel, instead of dying mid-write.
        let mut hangup = signal(SignalKind::hangup()).ok();
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
            Some(_) = async {
                match hangup.as_mut() {
                    Some(h) => h.recv().await,
                    None => std::future::pending().await,
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// `scancel` the job `record.replaces` names (by its Slurm id from its own
/// record), once. Returns that Slurm id.
async fn stop_replaced(cluster_dir: &Path, record: &JobRecord, own: &str) -> Option<String> {
    let old = record.replaces.as_deref().filter(|j| valid_job_id(j))?;
    let old_record: JobRecord = read_json(&cluster_dir.join("j").join(old).join("job.json"))?;
    let slurm = old_record
        .slurm_job_id
        .filter(|s| chimaera_core::cluster::valid_slurm_job_id(s) && s != own)?;
    let bindir = std::env::var_os("CHIMAERA_SLURM_BINDIR").map(PathBuf::from);
    match crate::compute::detect_tools(bindir.as_deref()).await {
        Some(crate::compute::Detection::Slurm { scancel, .. }) => {
            tracing::info!(job = %slurm, "this job continues another; stopping that one");
            match crate::compute::run_checked(
                &scancel.to_string_lossy(),
                std::slice::from_ref(&slurm),
            )
            .await
            {
                Some(out) if out.success || out.stderr.contains("Invalid job id") => {}
                Some(out) => {
                    tracing::warn!(job = %slurm, err = %out.stderr.trim(), "stopping the replaced job failed; its workspaces open here once it ends")
                }
                None => tracing::warn!(job = %slurm, "scancel did not answer"),
            }
        }
        _ => tracing::warn!("no scancel found; the replaced job keeps running until it ends"),
    }
    Some(slurm)
}

async fn auth(State(host): State<Arc<Host>>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {}", host.token));
    if ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response()
    }
}

async fn health(State(host): State<Arc<Host>>) -> Json<serde_json::Value> {
    Json(json!({
        "name": "chimaera-job-host",
        "job": host.record.id,
        "build": chimaera_core::BUILD_ID,
    }))
}

async fn status_route(State(host): State<Arc<Host>>) -> Json<JobHostStatus> {
    Json(status(&host).await)
}

async fn status(host: &Host) -> JobHostStatus {
    let snapshot: Vec<HostedWorkspace> = {
        let map = host.workspaces.lock().unwrap_or_else(|p| p.into_inner());
        let mut list: Vec<HostedWorkspace> = map
            .iter()
            .map(|(id, h)| HostedWorkspace {
                id: id.clone(),
                state: h.state,
                port: h.port,
                detail: h.detail.clone(),
                ..Default::default()
            })
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    };
    let counts = futures::future::join_all(snapshot.iter().map(|w| async {
        match (w.state, w.port) {
            (HostedState::Open, Some(port)) => {
                let identity = manifest_identity(&host.cluster_dir, &w.id).await;
                let (token, build) = identity
                    .map(|(token, build)| (Some(token), build))
                    .unwrap_or_default();
                let working = match &token {
                    Some(token) => working_agents(port, token).await,
                    None => 0,
                };
                (working, token, build)
            }
            _ => (0, None, None),
        }
    }))
    .await;
    JobHostStatus {
        job: host.record.id.clone(),
        slurm_job_id: host.own_slurm.clone(),
        node: chimaera_core::this_node().unwrap_or_default(),
        workspaces: snapshot
            .into_iter()
            .zip(counts)
            .map(|(mut w, (working, token, build))| {
                w.working = working;
                w.token = token;
                w.build = build;
                w
            })
            .collect(),
    }
}

/// An open workspace's token and build come from one original manifest read.
async fn manifest_identity(cluster_dir: &Path, wid: &str) -> Option<(String, Option<String>)> {
    let manifest = cluster_dir
        .join("w")
        .join(wid)
        .join("data")
        .join("manifest.json");
    tokio::task::spawn_blocking(move || {
        read_json::<chimaera_core::Manifest>(&manifest).map(|m| (m.token, m.build))
    })
    .await
    .ok()
    .flatten()
}

/// Agents working right now in one open workspace — asked of its own
/// chimaera on this node, bounded; 0 when it doesn't answer.
async fn working_agents(port: u16, token: &str) -> u32 {
    let body = tokio::time::timeout(
        Duration::from_millis(1500),
        local_get(port, "/api/v1/sessions", token),
    )
    .await
    .ok()
    .flatten();
    body.and_then(|b| serde_json::from_slice::<Vec<serde_json::Value>>(&b).ok())
        .map(|sessions| {
            sessions
                .iter()
                .filter(|s| s.get("agent_state").and_then(|v| v.as_str()) == Some("running"))
                .count() as u32
        })
        .unwrap_or(0)
}

/// A bounded HTTP/1.1 GET to a chimaera on this node; the body on 200.
async fn local_get(port: u16, path: &str, token: &str) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .ok()?;
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .ok()?;
    let mut buf = Vec::new();
    stream
        .take(4 * 1024 * 1024)
        .read_to_end(&mut buf)
        .await
        .ok()?;
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&buf[..split]).ok()?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return None;
    }
    Some(buf[split + 4..].to_vec())
}

/// Why an open didn't happen.
enum OpenError {
    Unknown,
    Held(String),
    Closing,
    Stopping,
    Failed(String),
}

impl OpenError {
    fn message(&self) -> String {
        match self {
            OpenError::Unknown => "that isn't a workspace on this cluster".into(),
            OpenError::Held(job) => format!("it's open in another job ({job})"),
            OpenError::Closing => "it's still closing — try again in a moment".into(),
            OpenError::Stopping => "this job is stopping".into(),
            OpenError::Failed(e) => e.clone(),
        }
    }
}

async fn open_route(State(host): State<Arc<Host>>, UrlPath(wid): UrlPath<String>) -> Response {
    match open(&host, &wid).await {
        Ok(w) => (StatusCode::OK, Json(w)).into_response(),
        Err(OpenError::Held(job)) => (
            StatusCode::CONFLICT,
            Json(HeldElsewhere {
                error: OpenError::Held(job.clone()).message(),
                slurm_job_id: job,
            }),
        )
            .into_response(),
        Err(e @ OpenError::Unknown) => {
            (StatusCode::NOT_FOUND, Json(json!({"error": e.message()}))).into_response()
        }
        Err(e) => (StatusCode::CONFLICT, Json(json!({"error": e.message()}))).into_response(),
    }
}

async fn close_route(State(host): State<Arc<Host>>, UrlPath(wid): UrlPath<String>) -> Response {
    if !valid_workspace_id(&wid) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown workspace"})),
        )
            .into_response();
    }
    close(&host, &wid).await;
    (StatusCode::OK, Json(json!({"closed": wid}))).into_response()
}

/// Open workspace `wid` in this job: refuse when another live job holds it,
/// else start its chimaera (which itself waits for a previous holder that is
/// still letting go).
async fn open(host: &Arc<Host>, wid: &str) -> Result<HostedWorkspace, OpenError> {
    if !valid_workspace_id(wid) {
        return Err(OpenError::Unknown);
    }
    if host.stopping.load(Ordering::Relaxed) {
        return Err(OpenError::Stopping);
    }
    // Reserve the slot before anything awaits, so two opens at once can't
    // start two chimaeras over one data folder.
    {
        let mut map = host.workspaces.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(h) = map.get(wid) {
            match h.state {
                HostedState::Starting | HostedState::Open => {
                    return Ok(HostedWorkspace {
                        id: wid.to_string(),
                        state: h.state,
                        port: h.port,
                        detail: h.detail.clone(),
                        ..Default::default()
                    })
                }
                HostedState::Closing => return Err(OpenError::Closing),
                HostedState::Failed => {}
            }
        }
        map.insert(wid.to_string(), Hosted::reserved());
    }
    host.changed.notify_one();
    let opened = prepare_and_spawn(host, wid).await;
    if opened.is_err() {
        let mut map = host.workspaces.lock().unwrap_or_else(|p| p.into_inner());
        if map.get(wid).is_some_and(|h| h.exited.is_none()) {
            map.remove(wid);
        }
        drop(map);
        host.changed.notify_one();
    }
    opened
}

async fn prepare_and_spawn(host: &Arc<Host>, wid: &str) -> Result<HostedWorkspace, OpenError> {
    let ws_dir = host.cluster_dir.join("w").join(wid);
    let (config, manifest) = {
        let cluster_json = host.cluster_dir.join("cluster.json");
        let manifest = ws_dir.join("data").join("manifest.json");
        tokio::task::spawn_blocking(move || {
            (
                read_json::<ClusterConfig>(&cluster_json),
                read_json::<chimaera_core::Manifest>(&manifest),
            )
        })
        .await
        .map_err(|e| OpenError::Failed(e.to_string()))?
    };
    let Some(ws) = config.and_then(|c| c.workspaces.into_iter().find(|w| w.id == wid)) else {
        return Err(OpenError::Unknown);
    };
    if let Some(holder) = manifest
        .and_then(|m| m.slurm_job_id)
        .filter(|j| *j != host.own_slurm && Some(j) != host.replaced_slurm.as_ref())
    {
        if holder_is_live(&holder).await == Some(true) {
            return Err(OpenError::Held(holder));
        }
    }
    let seed = WorkspaceSeed {
        id: ws.id.clone(),
        name: ws.name.clone(),
        path: ws.path.clone(),
    };
    let seed_path = ws_dir.join("workspace.json");
    {
        let (ws_dir, seed_path) = (ws_dir.clone(), seed_path.clone());
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            std::fs::create_dir_all(ws_dir.join("data"))?;
            chimaera_core::atomic_write_private(&seed_path, &serde_json::to_vec(&seed)?)
        })
        .await
        .map_err(|e| OpenError::Failed(e.to_string()))?
        .map_err(|e| OpenError::Failed(format!("could not prepare {}: {e}", ws.name)))?;
    }
    spawn_workspace(host, wid, &ws_dir, &seed_path)
        .await
        .map_err(|e| OpenError::Failed(format!("could not start {}: {e:#}", ws.name)))
}

async fn holder_is_live(job: &str) -> Option<bool> {
    let bindir = std::env::var_os("CHIMAERA_SLURM_BINDIR").map(PathBuf::from);
    match crate::compute::detect_tools(bindir.as_deref()).await {
        Some(crate::compute::Detection::Slurm { squeue, .. }) => {
            crate::compute::job_is_live(&squeue, job).await
        }
        _ => None,
    }
}

async fn spawn_workspace(
    host: &Arc<Host>,
    wid: &str,
    ws_dir: &Path,
    seed_path: &Path,
) -> anyhow::Result<HostedWorkspace> {
    let log_path = ws_dir.join("serve.log");
    let log = {
        let mut opts = tokio::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        opts.open(&log_path).await.context("opening serve.log")?
    };
    let log = Arc::new(tokio::sync::Mutex::new(log));
    let mut cmd = tokio::process::Command::new(&host.exe);
    cmd.arg("serve")
        .arg("--bind-routable")
        .env(ENV_DATA_DIR, ws_dir.join("data"))
        .env(ENV_RUNTIME_DIR, host.runtime_base.join(wid))
        .env(ENV_CLUSTER_WORKSPACE, seed_path)
        .env(ENV_HOST_PRELUDE_FILE, host.job_dir.join("startup.sh"))
        .env(ENV_AGENT_RULES_FILE, host.job_dir.join("agent-rules.md"))
        .env(ENV_CLUSTER_FACTS_FILE, host.job_dir.join("facts.json"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if host.record.attached {
        cmd.env(ENV_JOB_ATTACHED, "1");
    } else {
        cmd.env_remove(ENV_JOB_ATTACHED);
    }
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let (exited_tx, exited_rx) = watch::channel(false);
    // Spawned under the lock, into the reservation: a close that came while
    // this open was preparing removed it, and nothing starts.
    let (mut child, pid) = {
        let mut map = host.workspaces.lock().unwrap_or_else(|p| p.into_inner());
        let Some(slot) = map.get_mut(wid).filter(|h| h.exited.is_none()) else {
            anyhow::bail!("it was closed while opening");
        };
        let child = cmd.spawn().context("spawning chimaera serve")?;
        let pid = child.id();
        *slot = Hosted {
            state: HostedState::Starting,
            port: None,
            pid,
            detail: String::new(),
            tail: tail.clone(),
            exited: Some(exited_rx),
        };
        (child, pid)
    };
    tracing::info!(workspace = %wid, pid = pid.unwrap_or(0), "opening workspace");

    if let Some(out) = child.stdout.take() {
        let (host, wid, log, tail) = (host.clone(), wid.to_string(), log.clone(), tail.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(port) = listening_port(&line) {
                    {
                        let mut map = host.workspaces.lock().unwrap_or_else(|p| p.into_inner());
                        if let Some(h) = map.get_mut(&wid) {
                            if h.state == HostedState::Starting {
                                h.state = HostedState::Open;
                            }
                            h.port = Some(port);
                        }
                    }
                    host.changed.notify_one();
                }
                keep(&tail, &line);
                let mut f = log.lock().await;
                let _ = f.write_all(line.as_bytes()).await;
                let _ = f.write_all(b"\n").await;
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let (log, tail) = (log.clone(), tail.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                keep(&tail, &line);
                let mut f = log.lock().await;
                let _ = f.write_all(line.as_bytes()).await;
                let _ = f.write_all(b"\n").await;
            }
        });
    }
    {
        let (host, wid) = (host.clone(), wid.to_string());
        tokio::spawn(async move {
            let status = child.wait().await;
            let manifest = host
                .cluster_dir
                .join("w")
                .join(&wid)
                .join("data")
                .join("manifest.json");
            let own = host.own_slurm.clone();
            let _ = tokio::task::spawn_blocking(move || release_lease(&manifest, &own, pid)).await;
            let mut map = host.workspaces.lock().unwrap_or_else(|p| p.into_inner());
            let closing = map
                .get(&wid)
                .is_some_and(|h| h.state == HostedState::Closing)
                || host.stopping.load(Ordering::Relaxed);
            if closing {
                finish_close(&mut map, &wid, host.stopping.load(Ordering::Relaxed));
                tracing::info!(workspace = %wid, "workspace closed");
            } else if let Some(h) = map.get_mut(&wid) {
                h.state = HostedState::Failed;
                h.port = None;
                let last: Vec<String> = h
                    .tail
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .iter()
                    .cloned()
                    .collect();
                h.detail = last.join("\n");
                tracing::warn!(workspace = %wid, ?status, "workspace's chimaera exited on its own");
            }
            drop(map);
            host.changed.notify_one();
            let _ = exited_tx.send(true);
        });
    }
    Ok(HostedWorkspace {
        id: wid.to_string(),
        state: HostedState::Starting,
        ..Default::default()
    })
}

/// A workspace's chimaera that crashed or was killed leaves its manifest —
/// the lease that keeps the workspace in this job. Drop it when it is that
/// very process's, so the workspace can open again anywhere.
fn release_lease(path: &Path, own: &str, pid: Option<u32>) {
    let Some(m) = read_json::<chimaera_core::Manifest>(path) else {
        return;
    };
    if Some(m.pid) == pid && m.slurm_job_id.as_deref() == Some(own) {
        let _ = std::fs::remove_file(path);
    }
}

/// `chimaera daemon listening on 0.0.0.0:41234` → 41234.
fn listening_port(line: &str) -> Option<u16> {
    let rest = line.split("listening on ").nth(1)?;
    rest.trim().rsplit(':').next()?.parse().ok()
}

fn keep(tail: &Mutex<VecDeque<String>>, line: &str) {
    // The serve prints its token URL; a `failed` report never carries it.
    if line.contains("#token=") {
        return;
    }
    let mut t = tail.lock().unwrap_or_else(|p| p.into_inner());
    if t.len() == TAIL_LINES {
        t.pop_front();
    }
    t.push_back(plain_text(line).chars().take(300).collect());
}

/// A log line without its terminal colors (`ESC [ … letter`).
fn plain_text(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Keep a closing ownership row through allocation shutdown; an individual
/// workspace close during normal operation releases its row immediately.
/// Failed workspaces already released their leases and must stay available.
fn finish_close(map: &mut HashMap<String, Hosted>, wid: &str, stopping: bool) {
    if let Some(held) = map
        .get_mut(wid)
        .filter(|h| stopping && h.state != HostedState::Failed)
    {
        held.state = HostedState::Closing;
        held.port = None;
        held.pid = None;
        held.exited = None;
    } else {
        map.remove(wid);
    }
}

/// Close one workspace: SIGTERM (its chimaera saves its chats and removes
/// its manifest), then SIGKILL past [`CLOSE_GRACE`].
async fn close(host: &Host, wid: &str) {
    let (pid, mut exited) = {
        let mut map = host.workspaces.lock().unwrap_or_else(|p| p.into_inner());
        let Some(h) = map.get_mut(wid) else {
            return;
        };
        let Some(exited) = h.exited.clone().filter(|_| h.state != HostedState::Failed) else {
            // Failed, or only reserved (its open sees the slot gone and
            // starts nothing).
            finish_close(&mut map, wid, host.stopping.load(Ordering::Relaxed));
            drop(map);
            host.changed.notify_one();
            return;
        };
        h.state = HostedState::Closing;
        (h.pid, exited)
    };
    host.changed.notify_one();
    tracing::info!(workspace = %wid, "closing workspace");
    signal(pid, false);
    let done = tokio::time::timeout(CLOSE_GRACE, exited.wait_for(|e| *e)).await;
    if done.is_err() {
        tracing::warn!(workspace = %wid, "workspace didn't close in time; killing it");
        signal(pid, true);
    }
}

async fn close_all(host: &Host) {
    host.stopping.store(true, Ordering::Relaxed);
    let ids: Vec<String> = host
        .workspaces
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .keys()
        .cloned()
        .collect();
    futures::future::join_all(ids.iter().map(|id| close(host, id))).await;
}

fn signal(pid: Option<u32>, kill: bool) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        use nix::sys::signal::{kill as send, Signal};
        let sig = if kill {
            Signal::SIGKILL
        } else {
            Signal::SIGTERM
        };
        let _ = send(nix::unistd::Pid::from_raw(pid), sig);
    }
    #[cfg(not(unix))]
    let _ = (pid, kill);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn workspace_token_and_build_are_captured_from_the_same_manifest() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-job-identity-{}",
            chimaera_core::generate_token()
        ));
        let data = root.join("w/w-0000abcd/data");
        std::fs::create_dir_all(&data).unwrap();
        let path = data.join("manifest.json");
        let mut manifest = chimaera_core::Manifest {
            hostname: "fixture".into(),
            port: 1234,
            token: "original-token".into(),
            pid: 1,
            version: "0.0.1".into(),
            started_at: 0,
            build: Some("abcdef1.123".into()),
            slurm_job_id: Some("77".into()),
            runtime_leases: false,
            daemon_extension: false,
        };
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            manifest_identity(&root, "w-0000abcd").await,
            Some(("original-token".into(), Some("abcdef1.123".into())))
        );
        manifest.token = "successor-token".into();
        manifest.build = Some("abcdef1.124".into());
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            manifest_identity(&root, "w-0000abcd").await,
            Some(("successor-token".into(), Some("abcdef1.124".into())))
        );
        manifest.build = None;
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            manifest_identity(&root, "w-0000abcd").await,
            Some(("successor-token".into(), None))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn allocation_shutdown_keeps_durable_ownership_without_endpoints() {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-job-close-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("host.json"), "{}").unwrap();
        let mut map = HashMap::from([("w-0000abcd".to_string(), Hosted::reserved())]);
        finish_close(&mut map, "w-0000abcd", false);
        assert!(map.is_empty());
        map.insert("w-0000abcd".into(), Hosted::reserved());
        let mut failed = Hosted::reserved();
        failed.state = HostedState::Failed;
        failed.exited = Some(tokio::sync::watch::channel(true).1);
        map.insert("w-0000dead".into(), failed);
        let host = Arc::new(Host {
            job_dir: dir.clone(),
            cluster_dir: dir.join("cluster"),
            record: JobRecord::default(),
            own_slurm: "77".into(),
            replaced_slurm: None,
            token: "token".into(),
            exe: dir.join("chimaera"),
            runtime_base: dir.join("runtime"),
            workspaces: Mutex::new(map),
            stopping: AtomicBool::new(true),
            changed: tokio::sync::Notify::new(),
            finished: AtomicBool::new(true),
        });
        close_all(&host).await;
        host.changed.notify_one();
        publish_hosting(host).await;
        assert!(!dir.join("host.json").exists());
        let record: HostingRecord = read_json(&dir.join("workspaces.json")).unwrap();
        assert_eq!(
            record.workspaces.get("w-0000abcd"),
            Some(&HostedState::Closing)
        );
        assert!(!record.workspaces.contains_key("w-0000dead"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_listening_line_gives_the_port() {
        assert_eq!(
            listening_port("chimaera daemon listening on 127.0.0.1:41234"),
            Some(41234)
        );
        assert_eq!(
            listening_port("chimaera daemon listening on [::]:7"),
            Some(7)
        );
        assert_eq!(listening_port("something else"), None);
    }

    #[test]
    fn the_tail_keeps_the_last_lines() {
        let tail = Mutex::new(VecDeque::new());
        for i in 0..10 {
            keep(&tail, &format!("line {i}"));
        }
        let t = tail.lock().unwrap();
        assert_eq!(t.len(), TAIL_LINES);
        assert_eq!(t.back().map(String::as_str), Some("line 9"));
    }

    #[test]
    fn the_tail_drops_colors_and_the_token_line() {
        let tail = Mutex::new(VecDeque::new());
        keep(
            &tail,
            "\u{1b}[2m2026\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m ready",
        );
        keep(&tail, "http://127.0.0.1:1/#token=secret");
        let t = tail.lock().unwrap();
        assert_eq!(t.iter().cloned().collect::<Vec<_>>(), ["2026  INFO ready"]);
    }
}
