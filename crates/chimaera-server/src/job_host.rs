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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::watch;

use crate::lifecycle::{lease_holder, LEASE_FILE};

/// How long a closing workspace gets to save its chats before it is killed.
/// Under Slurm's usual kill wait (30 s) so a job's end still lets every
/// workspace finish.
const CLOSE_GRACE: Duration = Duration::from_secs(25);
/// The last lines of a workspace's output kept for a `failed` report.
const TAIL_LINES: usize = 6;
/// One line of a workspace's output, at most; the rest of a longer line is
/// dropped (a child printing without newlines must not grow job-host).
const MAX_LINE: usize = 16 * 1024;
/// `serve.log` lives on the shared filesystem for the whole job: past this
/// it stops growing (the `failed` tail still sees every line).
const SERVE_LOG_MAX: u64 = 8 * 1024 * 1024;
/// How long a holder's liveness answer is reused across opens: at most one
/// scheduler question per holder a minute, however often the page retries.
const HOLDER_ASK_FLOOR: Duration = Duration::from_secs(60);

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
    /// `CHIMAERA_SLURM_BINDIR`, read once (tests point it at stand-ins).
    bindir: Option<PathBuf>,
    /// The last answer about each other job holding a workspace here.
    holder_asks: Mutex<HashMap<String, (Instant, bool)>>,
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
    // Its workspaces' shell scripts, agent settings and preludes (which may
    // export secrets) live here, on a node other users share: a name they
    // can't predict, created by us, private — or job-host doesn't start.
    let runtime_base = {
        let (root, uid, slurm) = (PathBuf::from("/tmp"), uid.clone(), own_slurm.clone());
        tokio::task::spawn_blocking(move || private_runtime_base(&root, &uid, &slurm))
            .await?
            .context("could not make a private runtime folder")?
    };
    let bindir = std::env::var_os("CHIMAERA_SLURM_BINDIR").map(PathBuf::from);

    // "Continue in a new job": stop the job this one replaces, as the user
    // asked when they started it — its workspaces save their chats and let
    // go, and the ones opened below take them over (each waits on its
    // workspace's lease). Never anything but the one job named.
    let replaced_slurm = stop_replaced(&cluster_dir, &record, &own_slurm, bindir.as_deref()).await;

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
        bindir,
        holder_asks: Mutex::new(HashMap::new()),
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
    // Every workspace's chimaera has exited (or was killed past the grace).
    let base = host.runtime_base.clone();
    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(base)).await;
    Ok(())
}

/// `<root>/chimaera-<uid>-<slurm id>-<random>`, created private (see
/// [`chimaera_core::ensure_private_dir`]). The random part keeps another user
/// from creating it first, which a name built only from the uid and the
/// job id (both public in the queue) never could.
fn private_runtime_base(root: &Path, uid: &str, slurm: &str) -> std::io::Result<PathBuf> {
    let base = root.join(format!(
        "chimaera-{uid}-{slurm}-{}",
        &chimaera_core::generate_token()[..16]
    ));
    chimaera_core::ensure_private_dir(&base)?;
    Ok(base)
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
/// record), once — only after the queue shows that id as this user's job
/// under the name the record gave it (Slurm reuses ids). Returns that Slurm
/// id.
async fn stop_replaced(
    cluster_dir: &Path,
    record: &JobRecord,
    own: &str,
    bindir: Option<&Path>,
) -> Option<String> {
    let old = record.replaces.as_deref().filter(|j| valid_job_id(j))?;
    let old_record: JobRecord = read_json(&cluster_dir.join("j").join(old).join("job.json"))?;
    let slurm = old_record
        .slurm_job_id
        .filter(|s| chimaera_core::cluster::valid_slurm_job_id(s) && s != own)?;
    let Some(crate::compute::Detection::Slurm {
        scancel, squeue, ..
    }) = crate::compute::detect_tools(bindir).await
    else {
        tracing::warn!("no scancel found; the replaced job keeps running until it ends");
        return Some(slurm);
    };
    let row = crate::compute::own_queue_row(&squeue, &slurm).await;
    match replaced_job_action(row.as_ref(), &old_record.job_name) {
        ReplacedAction::Gone => {}
        ReplacedAction::Unknown => {
            tracing::warn!(job = %slurm, "could not ask Slurm about the replaced job; not stopping it — its workspaces open here once it ends")
        }
        ReplacedAction::NotOurs => {
            tracing::warn!(job = %slurm, "the replaced job's id now names another job; not stopping it")
        }
        ReplacedAction::Cancel => {
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
    }
    Some(slurm)
}

#[derive(Debug, PartialEq, Eq)]
enum ReplacedAction {
    Cancel,
    /// Not in this user's queue: ended already.
    Gone,
    /// Listed under another name: the id was reused.
    NotOurs,
    /// The queue didn't answer: nothing is cancelled on a guess.
    Unknown,
}

/// What to do about the replaced job, from its queue row (`None` =
/// unanswered, `Some(None)` = not listed) and the name its record holds
/// (empty in records that predate names: any chimaera job name then).
fn replaced_job_action(
    row: Option<&Option<crate::compute::QueueRow>>,
    job_name: &str,
) -> ReplacedAction {
    match row {
        None => ReplacedAction::Unknown,
        Some(None) => ReplacedAction::Gone,
        Some(Some(row)) if !crate::compute::is_live_state(&row.state) => ReplacedAction::Gone,
        Some(Some(row)) => {
            let ours = if job_name.is_empty() {
                chimaera_core::slurm::is_chimaera_job(&row.name)
            } else {
                row.name == job_name
            };
            if ours {
                ReplacedAction::Cancel
            } else {
                ReplacedAction::NotOurs
            }
        }
    }
}

async fn auth(State(host): State<Arc<Host>>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .is_some_and(|v| bearer_matches(v.as_bytes(), &host.token));
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

/// `Bearer <token>`, compared in time that depends only on the lengths: the
/// listener is routable on a shared node, and a byte-by-byte early exit
/// would time out the token a prefix at a time.
fn bearer_matches(header: &[u8], token: &str) -> bool {
    let Some(given) = header.strip_prefix(b"Bearer ") else {
        return false;
    };
    let want = token.as_bytes();
    if given.len() != want.len() {
        return false;
    }
    given
        .iter()
        .zip(want)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
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
                let token = manifest_token(&host.cluster_dir, &w.id).await;
                let working = match &token {
                    Some(token) => working_agents(port, token).await,
                    None => 0,
                };
                (working, token)
            }
            _ => (0, None),
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
            .map(|(mut w, (working, token))| {
                w.working = working;
                w.token = token;
                w
            })
            .collect(),
    }
}

/// An open workspace's token, from the manifest its chimaera wrote on this
/// node.
async fn manifest_token(cluster_dir: &Path, wid: &str) -> Option<String> {
    let manifest = cluster_dir
        .join("w")
        .join(wid)
        .join("data")
        .join("manifest.json");
    tokio::task::spawn_blocking(move || {
        read_json::<chimaera_core::Manifest>(&manifest).map(|m| m.token)
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
    let (config, holder) = {
        let cluster_json = host.cluster_dir.join("cluster.json");
        let data = ws_dir.join("data");
        tokio::task::spawn_blocking(move || {
            // The lease names the holder; a manifest alone is a holder from a
            // build that predates the lease file.
            let holder = lease_holder(&data.join(LEASE_FILE))
                .map(|h| h.slurm_job_id)
                .or_else(|| {
                    read_json::<chimaera_core::Manifest>(&data.join("manifest.json"))
                        .and_then(|m| m.slurm_job_id)
                });
            (read_json::<ClusterConfig>(&cluster_json), holder)
        })
        .await
        .map_err(|e| OpenError::Failed(e.to_string()))?
    };
    let Some(ws) = config.and_then(|c| c.workspaces.into_iter().find(|w| w.id == wid)) else {
        return Err(OpenError::Unknown);
    };
    if let Some(holder) =
        holder.filter(|j| *j != host.own_slurm && Some(j) != host.replaced_slurm.as_ref())
    {
        if holder_is_live(host, &holder).await == Some(true) {
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

/// Whether the other job `job` still holds a workspace, asked of Slurm at
/// most once per [`HOLDER_ASK_FLOOR`]. Advisory: the workspace's chimaera
/// takes its lease exclusively either way, so a stale "ended" only means it
/// waits there instead of a 409 here.
async fn holder_is_live(host: &Host, job: &str) -> Option<bool> {
    {
        let mut asks = host.holder_asks.lock().unwrap_or_else(|p| p.into_inner());
        asks.retain(|_, (at, _)| at.elapsed() < HOLDER_ASK_FLOOR);
        if let Some((_, live)) = asks.get(job) {
            return Some(*live);
        }
    }
    let live = match crate::compute::detect_tools(host.bindir.as_deref()).await {
        Some(crate::compute::Detection::Slurm { squeue, .. }) => {
            crate::compute::job_is_live(&squeue, job).await
        }
        _ => None,
    };
    if let Some(live) = live {
        host.holder_asks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(job.to_string(), (Instant::now(), live));
    }
    live
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
    let log = Arc::new(tokio::sync::Mutex::new(LogSink::new(log)));
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
            let mut out = BufReader::new(out);
            let mut buf = Vec::new();
            while let Ok(Some(line)) = next_line_bounded(&mut out, &mut buf).await {
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
                log.lock().await.line(&line).await;
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let (log, tail) = (log.clone(), tail.clone());
        tokio::spawn(async move {
            let mut err = BufReader::new(err);
            let mut buf = Vec::new();
            while let Ok(Some(line)) = next_line_bounded(&mut err, &mut buf).await {
                keep(&tail, &line);
                log.lock().await.line(&line).await;
            }
        });
    }
    {
        let (host, wid) = (host.clone(), wid.to_string());
        tokio::spawn(async move {
            let status = child.wait().await;
            let data = host.cluster_dir.join("w").join(&wid).join("data");
            let own = host.own_slurm.clone();
            let _ = tokio::task::spawn_blocking(move || release_lease(&data, &own, pid)).await;
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

/// `serve.log`, capped at [`SERVE_LOG_MAX`]: one marker line, then nothing.
struct LogSink<W> {
    out: W,
    written: u64,
    capped: bool,
}

impl<W: AsyncWrite + Unpin> LogSink<W> {
    fn new(out: W) -> Self {
        LogSink {
            out,
            written: 0,
            capped: false,
        }
    }

    async fn line(&mut self, line: &str) {
        if self.capped {
            return;
        }
        let len = line.len() as u64 + 1;
        if self.written + len > SERVE_LOG_MAX {
            self.capped = true;
            let _ = self
                .out
                .write_all(
                    b"[chimaera: serve.log reached its size limit; later lines are not kept]\n",
                )
                .await;
            return;
        }
        self.written += len;
        let _ = self.out.write_all(line.as_bytes()).await;
        let _ = self.out.write_all(b"\n").await;
    }
}

/// The next line (newline and a trailing `\r` dropped), keeping at most
/// [`MAX_LINE`] bytes of it — the rest is read and discarded, so memory
/// stays bounded however long the child's line is. `None` at EOF.
async fn next_line_bounded<R: AsyncBufRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<Option<String>> {
    buf.clear();
    let mut seen_any = false;
    loop {
        let chunk = r.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(seen_any.then(|| finish_line(buf)));
        }
        seen_any = true;
        let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i, true),
            None => (chunk.len(), false),
        };
        let room = MAX_LINE.saturating_sub(buf.len());
        buf.extend_from_slice(&chunk[..take.min(room)]);
        r.consume(if done { take + 1 } else { take });
        if done {
            return Ok(Some(finish_line(buf)));
        }
    }
}

fn finish_line(buf: &mut Vec<u8>) -> String {
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8_lossy(buf).into_owned()
}

/// A workspace's chimaera that crashed or was killed leaves its manifest and
/// its lease — what keeps the workspace in this job. Drop them when they are
/// that very process's, so the workspace can open again anywhere.
fn release_lease(data: &Path, own: &str, pid: Option<u32>) {
    let mine = |rec_pid: u32, job: Option<&str>| Some(rec_pid) == pid && job == Some(own);
    let manifest = data.join("manifest.json");
    if read_json::<chimaera_core::Manifest>(&manifest)
        .is_some_and(|m| mine(m.pid, m.slurm_job_id.as_deref()))
    {
        let _ = std::fs::remove_file(&manifest);
    }
    crate::lifecycle::release_lease(&data.join(LEASE_FILE), |h| {
        mine(h.pid, Some(h.slurm_job_id.as_str()))
    });
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
            bindir: None,
            holder_asks: Mutex::new(HashMap::new()),
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

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-job-{label}-{}",
            &chimaera_core::generate_token()[..12]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The runtime folder's name is not guessable from the queue, and it is
    /// private: another user can neither pre-create it nor read it.
    #[cfg(unix)]
    #[test]
    fn the_runtime_folder_is_unpredictable_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("runtime");
        let a = private_runtime_base(&root, "1000", "77").unwrap();
        let b = private_runtime_base(&root, "1000", "77").unwrap();
        assert_ne!(a, b);
        for dir in [&a, &b] {
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                name.starts_with("chimaera-1000-77-") && name.len() > 20,
                "{name}"
            );
            let mode = std::fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A child printing a huge line without a newline costs job-host at
    /// most [`MAX_LINE`], and the lines after it still come through whole.
    #[tokio::test]
    async fn a_line_without_newline_is_bounded() {
        let mut input = vec![b'x'; 1024 * 1024];
        input.extend_from_slice(b"\r\nchimaera daemon listening on 0.0.0.0:41234\nlast");
        let mut r = BufReader::with_capacity(4096, input.as_slice());
        let mut buf = Vec::new();
        let first = next_line_bounded(&mut r, &mut buf).await.unwrap().unwrap();
        assert_eq!(first.len(), MAX_LINE);
        assert!(buf.capacity() < 2 * MAX_LINE + 4096, "{}", buf.capacity());
        let second = next_line_bounded(&mut r, &mut buf).await.unwrap().unwrap();
        assert_eq!(listening_port(&second), Some(41234));
        assert_eq!(
            next_line_bounded(&mut r, &mut buf)
                .await
                .unwrap()
                .as_deref(),
            Some("last")
        );
        assert_eq!(next_line_bounded(&mut r, &mut buf).await.unwrap(), None);
    }

    /// serve.log stops at its cap with one marker line, however much the
    /// workspace prints over a multi-day job.
    #[tokio::test]
    async fn serve_log_stops_growing_at_its_cap() {
        let mut sink = LogSink::new(Vec::new());
        let line = "y".repeat(1023);
        for _ in 0..(SERVE_LOG_MAX / 1024 + 100) {
            sink.line(&line).await;
        }
        let written = sink.out.len() as u64;
        assert!(written <= SERVE_LOG_MAX + 200, "{written}");
        assert!(sink.capped);
        assert!(String::from_utf8_lossy(&sink.out).ends_with("are not kept]\n"));
        sink.line("more").await;
        assert_eq!(sink.out.len() as u64, written);
    }

    #[test]
    fn the_bearer_must_match_exactly() {
        assert!(bearer_matches(b"Bearer abc123", "abc123"));
        assert!(!bearer_matches(b"Bearer abc124", "abc123"));
        assert!(!bearer_matches(b"Bearer abc12", "abc123"));
        assert!(!bearer_matches(b"Bearer abc1234", "abc123"));
        assert!(!bearer_matches(b"bearer abc123", "abc123"));
        assert!(!bearer_matches(b"abc123", "abc123"));
    }

    #[test]
    fn a_replaced_job_is_stopped_only_when_the_queue_shows_it_ours() {
        use crate::compute::QueueRow;
        let row = |state: &str, name: &str| {
            Some(Some(QueueRow {
                state: state.into(),
                name: name.into(),
            }))
        };
        let name = "chimaera-long~abc123";
        let act = |r: Option<Option<QueueRow>>, n: &str| replaced_job_action(r.as_ref(), n);
        assert_eq!(act(row("RUNNING", name), name), ReplacedAction::Cancel);
        assert_eq!(act(row("PENDING", name), name), ReplacedAction::Cancel);
        assert_eq!(
            act(row("RUNNING", "chimaera-other~x"), name),
            ReplacedAction::NotOurs
        );
        assert_eq!(
            act(row("RUNNING", "someone-elses-job"), name),
            ReplacedAction::NotOurs
        );
        assert_eq!(act(row("CANCELLED by 1", name), name), ReplacedAction::Gone);
        assert_eq!(act(Some(None), name), ReplacedAction::Gone);
        assert_eq!(act(None, name), ReplacedAction::Unknown);
        // A record from before names were kept: any chimaera job name.
        assert_eq!(
            act(row("RUNNING", "chimaera-x~1"), ""),
            ReplacedAction::Cancel
        );
        assert_eq!(act(row("RUNNING", "mine"), ""), ReplacedAction::NotOurs);
    }

    /// End to end with stand-in tools: a reused id that the queue shows
    /// under another name is never scancelled; the real one is, once.
    #[tokio::test]
    async fn stop_replaced_cancels_only_the_job_it_continues() {
        let dir = scratch("replace");
        let cluster = dir.join("cluster");
        let old_dir = cluster.join("j").join("j-0000aaaa");
        std::fs::create_dir_all(&old_dir).unwrap();
        let old = JobRecord {
            id: "j-0000aaaa".into(),
            job_name: "chimaera-long~abc123".into(),
            slurm_job_id: Some("555".into()),
            ..Default::default()
        };
        std::fs::write(old_dir.join("job.json"), serde_json::to_vec(&old).unwrap()).unwrap();
        let record = JobRecord {
            id: "j-0000bbbb".into(),
            replaces: Some("j-0000aaaa".into()),
            ..Default::default()
        };
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = dir.join("scancel.args");
        for (name, body) in [
            (
                "scancel",
                format!(
                    "#!/bin/sh\n[ -n \"$1\" ] && echo \"$@\" >> '{}'\nexit 0\n",
                    log.display()
                ),
            ),
            (
                "squeue",
                "#!/bin/sh\n. \"$(dirname \"$0\")/case.sh\"\n".to_string(),
            ),
            ("sinfo", "#!/bin/sh\nexit 0\n".to_string()),
        ] {
            let p = bin.join(name);
            std::fs::write(&p, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let case = |body: &str| std::fs::write(bin.join("case.sh"), body).unwrap();
        case("exit 0\n");
        // Freshly written scripts can meet "Text file busy" on Linux while
        // another test forks; run each bare until the kernel lets it.
        for tool in ["scancel", "squeue"] {
            for _ in 0..100 {
                match std::process::Command::new(bin.join(tool)).output() {
                    Err(e) if e.raw_os_error() == Some(26) => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    _ => break,
                }
            }
        }

        case("echo 'RUNNING|someone-elses-job'\n");
        let got = stop_replaced(&cluster, &record, "777", Some(&bin)).await;
        assert_eq!(got.as_deref(), Some("555"));
        assert!(!log.exists(), "a reused id is never cancelled");

        case("echo 'RUNNING|chimaera-long~abc123'\n");
        stop_replaced(&cluster, &record, "777", Some(&bin)).await;
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "555");
        std::fs::remove_dir_all(&dir).ok();
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
