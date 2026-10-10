//! Cluster jobs and workspaces, driven from the client. On a cluster (a host
//! whose login shell reaches a batch scheduler) nothing of ours keeps running
//! on the login node: the app and the CLI run short commands over the
//! existing ControlMaster — `squeue`, `sbatch`, `scancel`, and the read-only
//! `chimaera browse` — and Chimaera runs inside Slurm **jobs** the user
//! starts. Workspaces open inside a job (`chimaera job-host`, reached over
//! the same `ssh -L` as the workspaces themselves); a workspace keeps its
//! chats in its own folder, so it moves between jobs.
//!
//! The optional keeper uses the same helpers, owning job connections itself.
//! `start_job_with_id` and `spawn_attached_once` add remote, non-expiring claims
//! for its durable operation journal; uncertainties require reconciliation, never
//! a second scheduler effect. A stable batch start checks Slurm --test-only once
//! before actual submission; only a fresh distinct refusal phase proves that
//! the real submit was never invoked. Direct app/CLI lifecycle is unchanged.
//!
//! Every exec is one bounded `ssh host sh -s` with the script on STDIN, never
//! in argv: startup commands may carry secrets (an `export API_KEY=…` on an
//! egress-limited cluster), and argv is visible to every user of a shared
//! login node. Scripts are POSIX sh; the login shell only ever runs `sh -s`.
//!
//! Politeness is part of the contract: the queue is asked at most once a
//! minute per host (`SQUEUE_FLOOR`), everything else is a file read.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use chimaera_core::cluster::{
    job_display_name, job_script, new_job_id, valid_job_id, valid_slurm_job_id, valid_workspace_id,
    AgentRules, BrowseState, ClusterConfig, ClusterFacts, ClusterWorkspace, DirListing, Ended,
    HeldElsewhere, HostRecord, HostedState, HostedWorkspace, JobHostStatus, JobRecord, JobScript,
    Setup,
};
use chimaera_core::slurm::{
    self, classify_refusal, clean_tool_stderr, is_chimaera_job, parse_duration, sh_quote, GpuFlag,
    Job, LaunchSpec, Refusal, Scheduler,
};
use serde::Serialize;
use tokio::process::Child;

use super::{collect_child_bounded, ssh_cmd, RemoteHome, ASKPASS_ALIAS_ENV};

/// The queue is asked at most this often per host — the scheduler-etiquette
/// floor HPC centers publish ("at least 60 seconds between status checks").
pub const SQUEUE_FLOOR: Duration = Duration::from_secs(60);
/// Discovery (`sinfo`, `sacctmgr`, `scontrol`) changes rarely.
const FACTS_TTL: Duration = Duration::from_secs(24 * 3600);
/// A submit that hasn't reached the queue yet within this long is shown as
/// waiting, not ended (sbatch → squeue lag).
const SUBMIT_GRACE_MS: u64 = 120_000;
/// Per-exec deadline: generous for a loaded login node, bounded for a
/// wedged one. The first exec of a session may also sit in a password/2FA
/// prompt raised by the ControlMaster, hence the connect-sized ceiling.
const EXEC_SECS: u64 = super::SSH_ONESHOT_SECS;
/// Startup commands per scope (the daemon's own cap).
const STARTUP_MAX: usize = 32 * 1024;

// --- Paths --------------------------------------------------------------------

impl RemoteHome {
    /// The cluster folder on the shared home: under the release's data dir,
    /// or the dev home's (`<home>/data`) — disjoint by construction.
    pub fn cluster_dir(self) -> &'static str {
        match self {
            RemoteHome::Real => "$HOME/.chimaera/cluster",
            RemoteHome::Dev => "$HOME/.chimaera-dev/data/cluster",
        }
    }

    /// What a dev build's jobs export as `CHIMAERA_HOME` so their config and
    /// caches stay apart from a release's.
    fn job_state_home(self) -> Option<&'static str> {
        match self {
            RemoteHome::Real => None,
            RemoteHome::Dev => Some("$HOME/.chimaera-dev"),
        }
    }

    /// The config dir every chimaera on the cluster reads (settings, the
    /// Environment settings' `env-profiles.json`) — `chimaera_core::config_dir`
    /// as a shell fragment.
    fn config_dir_sh(self) -> &'static str {
        match self {
            RemoteHome::Real => "${XDG_CONFIG_HOME:-$HOME/.config}/chimaera",
            RemoteHome::Dev => "$HOME/.chimaera-dev/config",
        }
    }
}

fn job_dir(home: RemoteHome, jid: &str) -> String {
    format!("{}/j/{jid}", home.cluster_dir())
}

// --- Exec ---------------------------------------------------------------------

/// Run `script` on `host` as `sh -s` (script on stdin), bounded. Stdin is
/// written concurrently with reading the output: `sh -s` executes as it
/// reads, so a script whose early commands fill the stdout pipe would
/// otherwise deadlock against our still-pending write.
async fn run_script(host: &str, script: &str, secs: u64) -> anyhow::Result<std::process::Output> {
    let mut cmd = ssh_cmd(host);
    cmd.arg("sh -s")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("failed to run ssh")?;
    let mut stdin = child.stdin.take().context("ssh stdin was not piped")?;
    let body = script.as_bytes().to_vec();
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(&body).await;
        let _ = stdin.shutdown().await;
    });
    let out = collect_child_bounded(child, secs, "ssh").await;
    writer.abort();
    out
}

/// Split framed script output into `(marker line, body)` sections. Markers
/// are lines starting with `===`; noise an echoing rc file prints before the
/// first marker is dropped.
fn sections(stdout: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("===") {
            out.push((rest.trim_end().to_string(), String::new()));
        } else if let Some((_, body)) = out.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    out
}

fn section<'a>(secs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    secs.iter().find_map(|(k, v)| {
        (k == key || k.split_whitespace().next() == Some(key)).then_some(v.as_str())
    })
}

/// The marker's argument (`===now 1700000000` → `1700000000`).
fn marker_arg<'a>(secs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    secs.iter().find_map(|(k, _)| {
        let mut w = k.split_whitespace();
        (w.next() == Some(key)).then(|| w.next()).flatten()
    })
}

/// A heredoc delimiter no file content can contain by accident.
fn eof_token() -> String {
    format!("CHIMAERA_EOF_{}", &chimaera_core::generate_token()[..16])
}

/// Shell lines that atomically write `content` to `path` (a `$`-anchored,
/// double-quotable path) under the script's `umask 077`.
fn write_file_lines(path: &str, content: &str) -> String {
    let eof = eof_token();
    let mut body = content.to_string();
    if !body.ends_with('\n') {
        body.push('\n');
    }
    format!("cat > \"{path}.tmp\" <<'{eof}'\n{body}{eof}\nmv -f \"{path}.tmp\" \"{path}\"\n")
}

/// `PATH` prefix so a plain `sh -s` finds the scheduler the login shell
/// found (clusters often put it on a profile-managed PATH only).
fn path_line(host: &str) -> String {
    match super::scheduler_of(host) {
        Some(info) if !info.bindir.is_empty() => {
            format!("PATH=\"{}:$PATH\"; export PATH\n", info.bindir)
        }
        _ => String::new(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Run one `scancel`, treating "Invalid job id" (already gone) as success.
fn scancel_lines(target: &str) -> String {
    format!(
        "err=$(scancel {target} 2>&1); rc=$?\n\
         if [ \"$rc\" -eq 0 ] || printf '%s' \"$err\" | grep -qi 'invalid job id'; then ok=1; else ok=0; fi\n\
         printf '===rc %s\\n===ok %s\\n===err\\n%s\\n' \"$rc\" \"$ok\" \"$err\"\n"
    )
}

// --- Overview -----------------------------------------------------------------

/// One job as the cluster page shows it. No port or token: those stay in
/// the client process ([`HostEndpoint`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct JobView {
    pub id: String,
    pub name: String,
    /// `waiting` | `starting` | `running` | `ended`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slurm_job_id: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub node: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub partition: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cpus: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpus: Option<u32>,
    /// When the job's time runs out, epoch ms (cluster clock).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ends_at_ms: Option<u64>,
    /// Why a waiting job waits (Slurm's own word), when it isn't plain
    /// priority.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// Held in the foreground by an app's connection.
    pub attached: bool,
    /// How it ended (Slurm's terminal state), once ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    pub stopped_by_user: bool,
    /// Cancellation was accepted, or Slurm is completing the allocation.
    /// Additive to `state` so older clients can still decode the overview.
    pub stopping: bool,
    /// The job's node reaches the internet (agents can work), when probed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress: Option<bool>,
    /// Workspaces it opens when it starts.
    pub open: Vec<String>,
    /// The job it continues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    pub spec: LaunchSpec,
    /// Its own startup commands.
    pub startup: String,
    pub submitted_ms: u64,
}

/// One workspace as the cluster page shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct WorkspaceView {
    pub id: String,
    pub name: String,
    pub path: String,
    /// `open` (in `job`) | `queued` (opens when `job` starts, or `job` is
    /// opening it now) | `closed`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_open_ms: Option<u64>,
    /// `job` runs and is opening it right now.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub opening: bool,
    /// It is saving its chats and closing in `job`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub closing: bool,
    /// Its chimaera in `job` exited on its own (closed now; Open starts it
    /// again). The client adds the last lines it printed when it can.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// Agents working in it right now, when its job-host was asked
    /// ([`apply_hosting`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working: Option<u32>,
}

/// Where a running job's job-host listens — kept by the client, never sent
/// to a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostEndpoint {
    pub job: String,
    pub slurm_job_id: String,
    pub node: String,
    pub port: u16,
    pub token: String,
    pub build: String,
}

/// Where an open workspace's chimaera listens — kept by the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub job: String,
    pub slurm_job_id: String,
    pub node: String,
    pub port: u16,
    pub token: String,
    pub build: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OtherJobs {
    pub running: usize,
    pub waiting: usize,
}

/// Startup commands at the cluster and workspace levels — the Environment
/// settings' own `env-profiles.json` on the cluster.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StartupView {
    pub cluster: String,
    pub workspaces: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ClusterOverview {
    pub scheduler: Scheduler,
    /// The login node these commands ran on.
    pub login_node: String,
    /// The user's home folder there (the page shows paths under it as `~/…`).
    pub home: String,
    pub now_ms: u64,
    pub jobs: Vec<JobView>,
    pub workspaces: Vec<WorkspaceView>,
    pub other_jobs: OtherJobs,
    /// The queue couldn't be read this round; states carry the last good
    /// read forward.
    pub degraded: bool,
    /// When the queue was last actually asked (epoch ms, client clock).
    pub queue_at_ms: u64,
    pub config: ClusterConfig,
    pub startup: StartupView,
    /// cksum of `cluster.json` as read — the optimistic-concurrency token
    /// for a write.
    #[serde(skip)]
    pub config_sum: String,
    /// The cluster's chimaera couldn't read its folder (missing, or a build
    /// from before `browse`): jobs may be missing until it's replaced.
    #[serde(skip)]
    pub state_unreadable: bool,
    #[serde(skip)]
    pub hosts: HashMap<String, HostEndpoint>,
    #[serde(skip)]
    pub endpoints: HashMap<String, Endpoint>,
    #[serde(skip)]
    pub records: HashMap<String, JobRecord>,
}

#[derive(Clone)]
struct QueueCache {
    at: Instant,
    at_ms: u64,
    jobs: Vec<Job>,
}

static QUEUES: LazyLock<Mutex<HashMap<String, QueueCache>>> = LazyLock::new(Default::default);
/// One overview exec per host at a time; concurrent callers share it.
static FLIGHTS: LazyLock<Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);

fn flight(host: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    FLIGHTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(host.to_string())
        .or_default()
        .clone()
}

fn cached_queue(host: &str) -> Option<QueueCache> {
    QUEUES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(host)
        .cloned()
}

/// Forget `host`'s cached queue so the next overview asks again — after the
/// user started or stopped something, they expect to see it. The floor still
/// holds against polls: only an explicit action resets it.
pub fn invalidate_queue(host: &str) {
    QUEUES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(host);
}

/// Everything the cluster page shows, in one exec: the queue (only when the
/// cached read is older than [`SQUEUE_FLOOR`]), `cluster.json`, the cluster
/// folder as `chimaera browse --state` reads it, and the startup commands.
pub async fn overview(host: &str, home: RemoteHome) -> anyhow::Result<ClusterOverview> {
    let lock = flight(host);
    let _one = lock.lock().await;
    let cached = cached_queue(host);
    let ask_queue = cached
        .as_ref()
        .is_none_or(|c| c.at.elapsed() >= SQUEUE_FLOOR);
    let mut script = String::from("umask 077\n");
    script.push_str(&path_line(host));
    script.push_str(&format!(
        "C=\"{}\"\nB=\"{}\"\nG=\"{}\"\n",
        home.cluster_dir(),
        home.bin_path(),
        home.config_dir_sh()
    ));
    script.push_str(
        "printf '===now %s\\n' \"$(date +%s)\"\nprintf '===node %s\\n' \"$(uname -n)\"\n\
         printf '===home %s\\n' \"$HOME\"\n",
    );
    if ask_queue {
        script.push_str(&format!(
            "q=$(squeue -u \"$(id -un)\" --noheader -o '{}' 2>/dev/null); rc=$?\n\
             printf '===squeue %s\\n' \"$rc\"\nprintf '%s\\n' \"$q\"\n",
            slurm::SQUEUE_FORMAT
        ));
    }
    script.push_str(
        "printf '===config %s\\n' \"$(cksum < \"$C/cluster.json\" 2>/dev/null | cut -d' ' -f1)\"\n\
         cat \"$C/cluster.json\" 2>/dev/null; printf '\\n'\n\
         printf '===state\\n'\n\
         [ -x \"$B\" ] && \"$B\" browse --state --cluster-dir \"$C\" 2>/dev/null\n\
         printf '===startup %s\\n' \"$(cksum < \"$G/env-profiles.json\" 2>/dev/null | cut -d' ' -f1)\"\n\
         cat \"$G/env-profiles.json\" 2>/dev/null; printf '\\n'\n\
         printf '===end\\n'\n",
    );
    let out = run_script(host, &script, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    if section(&secs, "end").is_none() {
        bail!(
            "reading {host}'s jobs failed: {}",
            super::ssh_failure_line(&out.stderr, &out.status)
        );
    }

    let mut degraded = false;
    let queue = if ask_queue {
        let rc = marker_arg(&secs, "squeue").unwrap_or("1");
        if rc == "0" {
            let (jobs, _) = slurm::parse_squeue(section(&secs, "squeue").unwrap_or(""));
            let fresh = QueueCache {
                at: Instant::now(),
                at_ms: now_ms(),
                jobs,
            };
            QUEUES
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(host.to_string(), fresh.clone());
            Some(fresh)
        } else {
            degraded = true;
            cached
        }
    } else {
        cached
    };

    let remote_now_ms = marker_arg(&secs, "now")
        .and_then(|n| n.parse::<u64>().ok())
        .map(|s| s * 1000)
        .unwrap_or_else(now_ms);
    let config_sum = marker_arg(&secs, "config").unwrap_or("").to_string();
    let config: ClusterConfig = section(&secs, "config")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let state_text = section(&secs, "state").unwrap_or("").trim();
    let state: Option<BrowseState> = serde_json::from_str(state_text).ok();
    let state_unreadable = state.is_none();
    let startup = parse_startup(section(&secs, "startup").unwrap_or(""));

    let jobs = queue.as_ref().map(|q| q.jobs.as_slice()).unwrap_or(&[]);
    // The queue's time-left counts from when it was read (up to a minute
    // ago), on the cluster's clock.
    let queue_age_ms = queue
        .as_ref()
        .map(|q| q.at.elapsed().as_millis() as u64)
        .unwrap_or(0);
    let built = build(
        &config,
        &state.unwrap_or_default(),
        jobs,
        queue.is_some(),
        remote_now_ms.saturating_sub(queue_age_ms),
    );
    let mut other = OtherJobs::default();
    for j in jobs.iter().filter(|j| !is_chimaera_job(&j.name)) {
        if j.state.starts_with("PENDING") {
            other.waiting += 1;
        } else if slurm::is_live_state(&j.state) {
            other.running += 1;
        }
    }
    Ok(ClusterOverview {
        scheduler: super::scheduler_of(host)
            .map(|s| s.kind)
            .unwrap_or(Scheduler::Slurm),
        login_node: marker_arg(&secs, "node").unwrap_or("").to_string(),
        home: secs
            .iter()
            .find_map(|(k, _)| k.strip_prefix("home "))
            .unwrap_or("")
            .trim()
            .to_string(),
        now_ms: remote_now_ms,
        jobs: built.jobs,
        workspaces: built.workspaces,
        other_jobs: other,
        degraded,
        queue_at_ms: queue.as_ref().map(|q| q.at_ms).unwrap_or(0),
        config,
        startup,
        config_sum,
        state_unreadable,
        hosts: built.hosts,
        endpoints: built.endpoints,
        records: built.records,
    })
}

/// `env-profiles.json` → the cluster (host) and per-workspace texts.
fn parse_startup(text: &str) -> StartupView {
    let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap_or_default();
    let text_of = |e: &serde_json::Value| {
        e.get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };
    StartupView {
        cluster: v.get("host").map(text_of).unwrap_or_default(),
        workspaces: v
            .get("workspaces")
            .and_then(|w| w.as_object())
            .map(|m| {
                m.iter()
                    .filter(|(k, _)| valid_workspace_id(k))
                    .map(|(k, e)| (k.clone(), text_of(e)))
                    .filter(|(_, t)| !t.trim().is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

struct Built {
    jobs: Vec<JobView>,
    workspaces: Vec<WorkspaceView>,
    hosts: HashMap<String, HostEndpoint>,
    endpoints: HashMap<String, Endpoint>,
    records: HashMap<String, JobRecord>,
}

/// The page's jobs and workspaces from the cluster folder and the queue —
/// pure, so every transition is unit-tested. `now_ms` is when the queue was
/// read, on the cluster's clock.
fn build(
    config: &ClusterConfig,
    state: &BrowseState,
    queue: &[Job],
    queue_known: bool,
    now_ms: u64,
) -> Built {
    let mut jobs = Vec::new();
    let mut hosts = HashMap::new();
    let mut records = HashMap::new();
    // What each running job's job-host holds (absent: a job-host too old to
    // say — its start list stands in).
    let mut hosting: HashMap<String, &chimaera_core::cluster::HostingRecord> = HashMap::new();
    for files in &state.jobs {
        let (view, host) = job_view(
            &files.record,
            files.host.as_ref(),
            files.egress,
            queue,
            queue_known,
            now_ms,
        );
        if let Some(h) = host {
            hosts.insert(view.id.clone(), h);
        }
        if let (Some(held), "running") = (&files.hosting, view.state) {
            hosting.insert(view.id.clone(), held);
        }
        records.insert(view.id.clone(), files.record.clone());
        jobs.push(view);
    }
    let mut endpoints = HashMap::new();
    let workspaces = config
        .workspaces
        .iter()
        .map(|ws| {
            let mut v = WorkspaceView {
                id: ws.id.clone(),
                name: ws.name.clone(),
                path: ws.path.clone(),
                state: "closed",
                job: None,
                last_open_ms: state.last_open_ms.get(&ws.id).copied(),
                opening: false,
                closing: false,
                failed: None,
                working: None,
            };
            let held_in = |jid: &str| {
                hosting
                    .get(jid)
                    .and_then(|h| h.workspaces.get(&ws.id))
                    .copied()
            };
            // Open where its chimaera's manifest says — but only in a job
            // that is running now (a manifest a crashed job left proves
            // nothing), and not where it just failed.
            if let Some(m) = state.manifests.get(&ws.id) {
                if let Some(job) = jobs.iter().find(|j| {
                    j.state == "running"
                        && j.slurm_job_id.is_some()
                        && j.slurm_job_id == m.slurm_job_id
                        && held_in(&j.id) != Some(HostedState::Failed)
                }) {
                    v.state = "open";
                    v.job = Some(job.id.clone());
                    v.closing = job.stopping || held_in(&job.id) == Some(HostedState::Closing);
                    // Keep ownership visible while Slurm finishes cancellation,
                    // but never offer a connection into a stopping allocation.
                    if job.stopping {
                        return v;
                    }
                    endpoints.insert(
                        ws.id.clone(),
                        Endpoint {
                            job: job.id.clone(),
                            slurm_job_id: job.slurm_job_id.clone().unwrap_or_default(),
                            node: if m.hostname.is_empty() {
                                job.node.clone()
                            } else {
                                m.hostname.clone()
                            },
                            port: m.port,
                            token: m.token.clone(),
                            build: m.build.clone().unwrap_or_default(),
                        },
                    );
                    return v;
                }
            }
            // Graceful shutdown removes manifests before Slurm finishes.
            // A failed row already released ownership, even if this snapshot
            // predates job-host's final closing rows.
            if let Some(job) = jobs.iter().find(|j| {
                j.stopping
                    && matches!(
                        held_in(&j.id),
                        Some(HostedState::Starting | HostedState::Open | HostedState::Closing)
                    )
            }) {
                v.state = "open";
                v.job = Some(job.id.clone());
                v.closing = true;
                return v;
            }
            // Opening in a running job, or opening when a job starts. A
            // running job's start list says nothing once its job-host
            // reports — a workspace closed there since is closed.
            if let Some(job) = jobs.iter().find(|j| match j.state {
                "running" => match hosting.get(&j.id) {
                    Some(h) => h.workspaces.get(&ws.id) == Some(&HostedState::Starting),
                    None => j.open.contains(&ws.id),
                },
                "waiting" | "starting" => j.open.contains(&ws.id),
                _ => false,
            }) {
                v.state = "queued";
                v.job = Some(job.id.clone());
                v.opening = job.state == "running";
                return v;
            }
            // Its chimaera exited on its own in a running job: closed, and
            // saying so.
            if let Some(job) = jobs
                .iter()
                .find(|j| held_in(&j.id) == Some(HostedState::Failed))
            {
                v.job = Some(job.id.clone());
                v.failed = Some(String::new());
            }
            v
        })
        .collect();
    Built {
        jobs,
        workspaces,
        hosts,
        endpoints,
        records,
    }
}

/// One job's state from its record, its job-host's record and the queue.
fn job_view(
    record: &JobRecord,
    host: Option<&HostRecord>,
    egress: Option<bool>,
    queue: &[Job],
    queue_known: bool,
    now_ms: u64,
) -> (JobView, Option<HostEndpoint>) {
    let mut v = JobView {
        id: record.id.clone(),
        name: record.name.clone(),
        state: "ended",
        slurm_job_id: record.slurm_job_id.clone(),
        partition: record.spec.partition.clone().unwrap_or_default(),
        gpus: record.spec.gpus,
        attached: record.attached,
        stopped_by_user: record.stopped_by_user,
        stopping: record.stopped_by_user,
        egress,
        open: record.open.clone(),
        replaces: record.replaces.clone(),
        spec: record.spec.clone(),
        startup: record.startup.clone(),
        submitted_ms: record.submitted_ms,
        ..Default::default()
    };
    let row = queue.iter().find(|j| match &record.slurm_job_id {
        Some(id) => &j.id == id,
        None => j.name == record.job_name,
    });
    let endpoint = |v: &JobView, h: &HostRecord| HostEndpoint {
        job: record.id.clone(),
        slurm_job_id: v.slurm_job_id.clone().unwrap_or_default(),
        node: if h.node.is_empty() {
            v.node.clone()
        } else {
            h.node.clone()
        },
        port: h.port,
        token: h.token.clone(),
        build: h.build.clone(),
    };
    match row {
        Some(job) if slurm::is_live_state(&job.state) => {
            v.slurm_job_id = Some(job.id.clone());
            v.partition = job.partition.clone();
            v.cpus = job.cpus.clone();
            v.mem = job.mem.clone();
            v.ends_at_ms =
                parse_duration(&job.time_left).map(|left| now_ms + left.as_millis() as u64);
            v.stopping |= job.state.starts_with("COMPLETING");
            if v.stopping {
                v.state = "running";
                v.node = job.nodes.clone();
                return (v, None);
            }
            if job.state.starts_with("PENDING") || job.state.starts_with("CONFIGURING") {
                v.state = "waiting";
                v.ends_at_ms = None;
                if job.reason != "Priority" && job.reason != "None" {
                    v.reason = job.reason.clone();
                }
                return (v, None);
            }
            v.node = job.nodes.clone();
            // Running once THIS job's job-host wrote its record: an older
            // record left in the folder must never be mistaken for it.
            match host.filter(|h| h.slurm_job_id == job.id) {
                Some(h) => {
                    v.state = "running";
                    let e = endpoint(&v, h);
                    (v, Some(e))
                }
                None => {
                    v.state = "starting";
                    (v, None)
                }
            }
        }
        Some(job) => {
            v.ended = Some(
                record
                    .ended
                    .as_ref()
                    .map(|e| e.state.clone())
                    .unwrap_or_else(|| {
                        slurm::terminal_state(&job.state)
                            .unwrap_or("ENDED")
                            .to_string()
                    }),
            );
            v.ended_at_ms = record.ended.as_ref().map(|e| e.at_ms);
            (v, None)
        }
        None => {
            // Not in the queue: just submitted (the queue lags a submit), or
            // gone. Without a queue read nothing can be said, so the files
            // stand: a job-host record means it ran.
            let young = now_ms.saturating_sub(record.submitted_ms) < SUBMIT_GRACE_MS;
            if record.ended.is_none() && !queue_known {
                if record.stopped_by_user {
                    v.state = "running";
                    return (v, None);
                }
                if let Some(h) = host {
                    v.state = "running";
                    v.node = h.node.clone();
                    v.slurm_job_id = Some(h.slurm_job_id.clone());
                    let e = endpoint(&v, h);
                    return (v, Some(e));
                }
                v.state = "waiting";
                return (v, None);
            }
            if record.ended.is_none() && young && !record.stopped_by_user {
                v.state = "waiting";
                return (v, None);
            }
            if let Some(e) = &record.ended {
                v.ended = Some(e.state.clone());
                v.ended_at_ms = Some(e.at_ms);
            }
            (v, None)
        }
    }
}

// --- Discovery ----------------------------------------------------------------

static FACTS: LazyLock<Mutex<HashMap<String, (Instant, ClusterFacts)>>> =
    LazyLock::new(Default::default);

/// Discover partitions, limits and accounts (cached a day per host; `refresh`
/// asks again). One exec; every command is standard Slurm, and each one that
/// a cluster hides or lacks just degrades what the sheet can say.
pub async fn facts(host: &str, refresh: bool) -> anyhow::Result<ClusterFacts> {
    if !refresh {
        if let Some((at, f)) = FACTS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(host)
            .cloned()
        {
            if at.elapsed() < FACTS_TTL {
                return Ok(f);
            }
        }
    }
    let mut script = path_line(host);
    script.push_str(&format!(
        "u=$(id -un)\n\
         printf '===version\\n'; sinfo --version 2>/dev/null\n\
         printf '===sinfo\\n'; sinfo --noheader -o '{}' 2>/dev/null\n\
         printf '===assoc\\n'; sacctmgr -nP show assoc user=\"$u\" format=account,partition,qos 2>/dev/null\n\
         printf '===defacct\\n'; sacctmgr -nP show user \"$u\" format=defaultaccount 2>/dev/null\n\
         printf '===policy\\n'; scontrol show partition --oneliner 2>/dev/null\n\
         printf '===groups\\n'; id -Gn 2>/dev/null\n\
         printf '===end\\n'\n",
        slurm::SINFO_FORMAT
    ));
    let out = run_script(host, &script, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    if section(&secs, "end").is_none() {
        bail!(
            "asking {host}'s scheduler failed: {}",
            super::ssh_failure_line(&out.stderr, &out.status)
        );
    }
    let f = facts_from_sections(&secs);
    FACTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(host.to_string(), (Instant::now(), f.clone()));
    Ok(f)
}

fn facts_from_sections(secs: &[(String, String)]) -> ClusterFacts {
    let version = section(secs, "version").unwrap_or("").trim().to_string();
    let (sinfo, _) = slurm::parse_sinfo(section(secs, "sinfo").unwrap_or(""));
    let assoc = slurm::parse_associations(section(secs, "assoc").unwrap_or(""));
    let policies = slurm::parse_partition_policies(section(secs, "policy").unwrap_or(""));
    let groups: Vec<String> = section(secs, "groups")
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let partitions = slurm::usable_partitions(&sinfo, &policies, &assoc, &groups);
    let mut accounts: Vec<String> = Vec::new();
    for a in &assoc {
        if !accounts.contains(&a.account) {
            accounts.push(a.account.clone());
        }
    }
    let default_account = section(secs, "defacct")
        .unwrap_or("")
        .lines()
        .map(|l| l.trim().trim_end_matches('|'))
        .find(|l| !l.is_empty())
        .map(str::to_string)
        .filter(|a| accounts.contains(a));
    ClusterFacts {
        scheduler: Scheduler::Slurm,
        gpu_flag: slurm::gpu_flag_for(&version),
        version,
        partitions,
        accounts,
        default_account,
        fetched_ms: now_ms(),
    }
}

// --- The cluster folder ---------------------------------------------------------

/// Write `cluster.json` if it still reads as `expected_sum` (the cksum the
/// caller read it at; empty = it didn't exist). Two clients editing at once
/// can't silently drop each other's change: the loser gets an error and
/// re-reads.
pub async fn write_config(
    host: &str,
    home: RemoteHome,
    config: &ClusterConfig,
    expected_sum: &str,
) -> anyhow::Result<()> {
    let c = home.cluster_dir();
    let json = serde_json::to_string_pretty(config)?;
    let mut script = format!(
        "umask 077\nC=\"{c}\"\nmkdir -p \"$C/w\" \"$C/j\" || exit 3\n\
         now=$(cksum < \"$C/cluster.json\" 2>/dev/null | cut -d' ' -f1)\n\
         [ \"$now\" = {} ] || {{ printf 'changed\\n'; exit 4; }}\n",
        sh_quote(expected_sum)
    );
    script.push_str(&write_file_lines("$C/cluster.json", &json));
    script.push_str("printf 'ok\\n'\n");
    let out = run_script(host, &script, EXEC_SECS).await?;
    match out.status.code() {
        Some(0) => Ok(()),
        Some(4) => bail!("the cluster's workspace list changed meanwhile — reload and try again"),
        _ => bail!(
            "saving {host}'s workspace list failed: {}",
            super::ssh_failure_line(&out.stderr, &out.status)
        ),
    }
}

/// Read-modify-write `cluster.json` with one retry on a concurrent change.
pub async fn update_config<T>(
    host: &str,
    home: RemoteHome,
    mut f: impl FnMut(&mut ClusterConfig) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    for attempt in 0..2 {
        let (config, sum) = read_config(host, home).await?;
        let mut next = config;
        let value = f(&mut next)?;
        next.version = 2;
        match write_config(host, home, &next, &sum).await {
            Ok(()) => return Ok(value),
            Err(e) if attempt == 0 && e.to_string().contains("changed meanwhile") => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("the loop returns on its second pass")
}

pub async fn read_config(host: &str, home: RemoteHome) -> anyhow::Result<(ClusterConfig, String)> {
    let script = format!(
        "C=\"{}\"\nprintf '===config %s\\n' \"$(cksum < \"$C/cluster.json\" 2>/dev/null | cut -d' ' -f1)\"\n\
         cat \"$C/cluster.json\" 2>/dev/null; printf '\\n===end\\n'\n",
        home.cluster_dir()
    );
    let out = run_script(host, &script, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    if section(&secs, "end").is_none() {
        bail!(
            "reading {host}'s workspace list failed: {}",
            super::ssh_failure_line(&out.stderr, &out.status)
        );
    }
    let sum = marker_arg(&secs, "config").unwrap_or("").to_string();
    let config = section(&secs, "config")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(serde_json::from_str::<ClusterConfig>)
        .transpose()
        .context("the cluster's workspace list is not valid JSON")?
        .unwrap_or_default();
    Ok((config, sum))
}

/// A path the user typed, checked for the characters a path needs — so it
/// can be expanded by the cluster's login shell (`$SCRATCH/x`, `~/x`)
/// without that shell running anything else.
fn plain_path_input(raw: &str) -> anyhow::Result<String> {
    let raw = raw.trim();
    anyhow::ensure!(!raw.is_empty(), "Enter a folder on the cluster");
    anyhow::ensure!(raw.len() <= 1024, "That path is too long");
    let ok = raw
        .chars()
        .all(|c| c.is_alphanumeric() || " /._-+~${}@,=:%".contains(c))
        && !raw.contains("$(");
    anyhow::ensure!(
        ok,
        "Use a plain path — letters, digits, / . _ - and $VARIABLES"
    );
    Ok(match raw.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("$HOME{rest}"),
        _ => raw.to_string(),
    })
}

/// Add a workspace: resolve the path in the cluster's login environment
/// (where `$SCRATCH`-style variables are set), require a directory, and
/// record it in `cluster.json`. A folder that was a workspace before (taken
/// off the list) gets its old id back — and with it, its chats.
pub async fn add_workspace(
    host: &str,
    home: RemoteHome,
    raw_path: &str,
    name: &str,
) -> anyhow::Result<ClusterWorkspace> {
    let input = plain_path_input(raw_path)?;
    let script = format!(
        "raw={}\nC=\"{}\"\n\
         p=$(sh -lc 'eval \"cd -- \\\"$1\\\"\" 2>/dev/null && pwd -P' sh \"$raw\" </dev/null 2>/dev/null | tail -n 1)\n\
         [ -n \"$p\" ] || p=$(eval \"cd -- \\\"$raw\\\"\" 2>/dev/null && pwd -P)\n\
         printf '===path %s\\n' \"$p\"\n\
         for f in \"$C\"/w/w-*/workspace.json; do [ -r \"$f\" ] || continue; printf '===seed\\n'; cat \"$f\"; printf '\\n'; done\n\
         printf '===end\\n'\n",
        sh_quote(&input),
        home.cluster_dir()
    );
    let out = run_script(host, &script, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    let resolved = secs
        .iter()
        .find_map(|(k, _)| k.strip_prefix("path ").map(str::trim))
        .unwrap_or("")
        .to_string();
    anyhow::ensure!(
        resolved.starts_with('/'),
        "{raw_path} isn't a folder on {host}"
    );
    let name = match name.trim() {
        "" => resolved
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or("workspace")
            .to_string(),
        n => n.chars().take(80).collect(),
    };
    // A folder this cluster knew before keeps its id (its chats live under
    // it): the seed its last chimaera registered names the path.
    let earlier: Option<String> = secs
        .iter()
        .filter(|(k, _)| k == "seed")
        .filter_map(|(_, v)| {
            serde_json::from_str::<chimaera_core::cluster::WorkspaceSeed>(v.trim()).ok()
        })
        .find(|seed| seed.path == resolved && valid_workspace_id(&seed.id))
        .map(|seed| seed.id);
    update_config(host, home, |config| {
        if let Some(existing) = config.workspaces.iter().find(|w| w.path == resolved) {
            return Ok(existing.clone());
        }
        let id = earlier
            .clone()
            .filter(|id| !config.workspaces.iter().any(|w| &w.id == id))
            .unwrap_or_else(chimaera_core::cluster::new_workspace_id);
        let ws = ClusterWorkspace {
            id,
            name: name.clone(),
            path: resolved.clone(),
            created_ms: now_ms(),
        };
        config.workspaces.push(ws.clone());
        Ok(ws)
    })
    .await
}

/// Take a workspace off this cluster's list. Nothing is deleted: the
/// project folder is never touched, and its chimaera folder (chats, history)
/// stays, so adding the same folder again brings them back. Refused while
/// it is open in a job or waiting to open.
pub async fn remove_workspace(host: &str, home: RemoteHome, id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(valid_workspace_id(id), "unknown workspace");
    let ov = overview(host, home).await?;
    if let Some(v) = ov.workspaces.iter().find(|w| w.id == id) {
        anyhow::ensure!(
            v.state == "closed",
            "Close {} first — it's open in a job",
            v.name
        );
    }
    update_config(host, home, |config| {
        config.workspaces.retain(|w| w.id != id);
        Ok(())
    })
    .await
}

/// One folder's subfolders for choosing a workspace — `chimaera browse --dir`
/// on the login node, read-only. A path with `$VARS` is listed from a login
/// shell, where clusters set them.
pub async fn list_dir(host: &str, home: RemoteHome, raw: &str) -> anyhow::Result<DirListing> {
    let raw = raw.trim();
    anyhow::ensure!(
        !raw.is_empty() && raw.len() <= 4096 && !raw.contains(['\n', '\0']),
        "Enter a folder on the cluster"
    );
    let browse = if raw.contains('$') {
        "sh -lc '\"$0\" browse --dir \"$1\" --cluster-dir \"$2\"' \"$B\" \"$p\" \"$C\" </dev/null"
    } else {
        "\"$B\" browse --dir \"$p\" --cluster-dir \"$C\""
    };
    // browse prints JSON on success and only its error otherwise, so both
    // ride stdout and the exit status tells them apart.
    let script = format!(
        "C=\"{}\"\nB=\"{}\"\np={}\n[ -x \"$B\" ] || {{ printf '===missing\\n===end\\n'; exit 0; }}\n\
         out=$({browse} 2>&1); rc=$?\n\
         printf '===rc %s\\n' \"$rc\"; printf '%s\\n' \"$out\"\n\
         printf '===end\\n'\n",
        home.cluster_dir(),
        home.bin_path(),
        sh_quote(raw)
    );
    let out = run_script(host, &script, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    anyhow::ensure!(
        section(&secs, "end").is_some(),
        "listing {raw} failed: {}",
        super::ssh_failure_line(&out.stderr, &out.status)
    );
    anyhow::ensure!(
        section(&secs, "missing").is_none(),
        "chimaera isn't on {host} yet"
    );
    if marker_arg(&secs, "rc") != Some("0") {
        let msg = section(&secs, "rc").unwrap_or("").trim();
        bail!(
            "{}",
            if msg.is_empty() {
                format!("{raw} can't be listed")
            } else {
                msg.lines().last().unwrap_or(msg).to_string()
            }
        );
    }
    // A login shell may print its own banner first: the listing is browse's
    // one JSON line, the last.
    let body = section(&secs, "rc").unwrap_or("");
    let json = body
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or("");
    serde_json::from_str(json.trim())
        .with_context(|| format!("listing {raw} gave an unreadable answer"))
}

// --- Startup commands -----------------------------------------------------------

/// Set the cluster's (`workspace: None`) or one workspace's startup commands
/// in the cluster's `env-profiles.json` — the very scopes a workspace
/// window's Environment settings edit. Read-modify-write with the same
/// cksum guard as `cluster.json`, other fields kept as they are.
pub async fn set_startup(
    host: &str,
    home: RemoteHome,
    workspace: Option<&str>,
    text: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        text.len() <= STARTUP_MAX,
        "Startup commands are limited to 32 KB"
    );
    anyhow::ensure!(!text.contains('\0'), "Startup commands can't contain NUL");
    if let Some(id) = workspace {
        anyhow::ensure!(valid_workspace_id(id), "unknown workspace");
    }
    let g = home.config_dir_sh();
    for attempt in 0..2 {
        let read = format!(
            "G=\"{g}\"\nprintf '===sum %s\\n' \"$(cksum < \"$G/env-profiles.json\" 2>/dev/null | cut -d' ' -f1)\"\n\
             printf '===file\\n'; cat \"$G/env-profiles.json\" 2>/dev/null; printf '\\n===end\\n'\n"
        );
        let out = run_script(host, &read, EXEC_SECS).await?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let secs = sections(&stdout);
        anyhow::ensure!(
            section(&secs, "end").is_some(),
            "reading {host}'s startup commands failed: {}",
            super::ssh_failure_line(&out.stderr, &out.status)
        );
        let sum = marker_arg(&secs, "sum").unwrap_or("").to_string();
        let current = section(&secs, "file").unwrap_or("").trim();
        let mut v: serde_json::Value = if current.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(current).context(
                "the cluster's env-profiles.json isn't valid JSON — fix it by hand first",
            )?
        };
        anyhow::ensure!(
            v.is_object(),
            "the cluster's env-profiles.json isn't an object"
        );
        set_scope(&mut v, workspace, text);
        let json = serde_json::to_string_pretty(&v)?;
        let mut write = format!(
            "umask 077\nG=\"{g}\"\nmkdir -p \"$G\" || exit 3\n\
             now=$(cksum < \"$G/env-profiles.json\" 2>/dev/null | cut -d' ' -f1)\n\
             [ \"$now\" = {} ] || exit 4\n",
            sh_quote(&sum)
        );
        write.push_str(&write_file_lines("$G/env-profiles.json", &json));
        let out = run_script(host, &write, EXEC_SECS).await?;
        match out.status.code() {
            Some(0) => return Ok(()),
            Some(4) if attempt == 0 => continue,
            Some(4) => bail!("the startup commands changed meanwhile — reload and try again"),
            _ => bail!(
                "saving {host}'s startup commands failed: {}",
                super::ssh_failure_line(&out.stderr, &out.status)
            ),
        }
    }
    unreachable!("the loop returns on its second pass")
}

/// One scope of `env-profiles.json` set (empty text removes it) — the
/// daemon's own shape: `{host: {text}, workspaces: {<id>: {text}}}`.
fn set_scope(v: &mut serde_json::Value, workspace: Option<&str>, text: &str) {
    let entry = serde_json::json!({ "text": text });
    let empty = text.trim().is_empty();
    let obj = v.as_object_mut().expect("checked to be an object");
    match workspace {
        None => {
            if empty {
                obj.remove("host");
            } else {
                obj.insert("host".into(), entry);
            }
        }
        Some(id) => {
            let map = obj
                .entry("workspaces")
                .or_insert_with(|| serde_json::json!({}));
            if !map.is_object() {
                *map = serde_json::json!({});
            }
            let map = map.as_object_mut().expect("just made an object");
            if empty {
                map.remove(id);
            } else {
                map.insert(id.into(), entry);
            }
            if map.is_empty() {
                obj.remove("workspaces");
            }
        }
    }
}

// --- Jobs -----------------------------------------------------------------------------

/// How a start went.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartOutcome {
    /// `sbatch` took it.
    Submitted { job: String, slurm_job_id: String },
    /// The partition takes only interactive jobs: the caller holds the job
    /// in the foreground ([`spawn_attached`]).
    Attached { job: String, job_name: String },
}

/// The scheduler said no. `message` is its own text, cleaned; `kind` is
/// what it seems to say (remembered so the next start doesn't fail the same
/// way). A stable submission additionally requires an intact empty-ID refusal
/// frame; any emitted allocation identity remains [`StartUncertain`].
#[derive(Clone, Debug, Serialize)]
pub struct StartRefused {
    pub message: String,
    pub kind: Refusal,
}

impl std::fmt::Display for StartRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StartRefused {}

/// What one job start needs.
pub struct JobStart<'a> {
    /// A saved setup's name; else the job is called by its partition and
    /// time.
    pub name: Option<&'a str>,
    pub spec: &'a LaunchSpec,
    /// Workspaces to open when it starts.
    pub open: &'a [String],
    /// Startup commands for this job only (after the cluster's and each
    /// workspace's).
    pub run_startup: &'a str,
    /// Hold the job in the foreground instead of submitting it (a partition
    /// that refuses batch jobs).
    pub attached: bool,
    /// The job this one continues (its workspaces move over when it starts).
    pub replaces: Option<&'a str>,
    /// The cluster's Slurm setup — what the job's agents are told.
    pub facts: &'a ClusterFacts,
}

/// Write a new job's folder and submit it (or, for an attached start, only
/// write it). The binary must already be on the cluster
/// (`ensure_cluster_binary`).
pub async fn start_job(
    host: &str,
    home: RemoteHome,
    config: &ClusterConfig,
    req: &JobStart<'_>,
) -> anyhow::Result<StartOutcome> {
    start_job_inner(host, home, config, req, new_job_id(), false).await
}

/// Submit using a caller's durable identity. An existing remote claim is never
/// submitted again, even when its reply or local keeper journal was lost.
/// The caller reconciles records/queue/accounting; absence is not terminal proof.
/// Claims cover process/SSH/reply loss. After storage loss, reconcile the exact
/// deterministic scheduler name before any effect; mkdir alone proves no fsync.
pub async fn start_job_with_id(
    host: &str,
    home: RemoteHome,
    config: &ClusterConfig,
    req: &JobStart<'_>,
    job_id: &str,
) -> anyhow::Result<StartOutcome> {
    anyhow::ensure!(valid_job_id(job_id), "unknown job");
    start_job_inner(host, home, config, req, job_id.to_owned(), true).await
}

#[derive(Debug)]
pub struct StartUncertain {
    pub job: String,
}
impl std::fmt::Display for StartUncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("job submission requires reconciliation; it was not repeated")
    }
}
impl std::error::Error for StartUncertain {}

async fn start_job_inner(
    host: &str,
    home: RemoteHome,
    config: &ClusterConfig,
    req: &JobStart<'_>,
    jid: String,
    stable: bool,
) -> anyhow::Result<StartOutcome> {
    let spec = req.spec.clone().normalized();
    spec.validate().map_err(anyhow::Error::msg)?;
    for wid in req.open {
        anyhow::ensure!(
            config.workspaces.iter().any(|w| &w.id == wid),
            "unknown workspace"
        );
    }
    if let Some(old) = req.replaces {
        anyhow::ensure!(valid_job_id(old), "unknown job");
    }
    anyhow::ensure!(
        req.run_startup.len() <= STARTUP_MAX,
        "Startup commands are limited to 32 KB"
    );
    let name = req
        .name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| n.chars().take(60).collect::<String>())
        .unwrap_or_else(|| job_display_name(&spec));
    let token = chimaera_core::generate_token();
    let job_name = slurm::job_name(&name, if stable { &jid[2..] } else { &token[..6] });
    let dir = job_dir(home, &jid);
    let record = JobRecord {
        id: jid.clone(),
        name,
        job_name: job_name.clone(),
        spec: spec.clone(),
        open: req.open.to_vec(),
        startup: req.run_startup.to_string(),
        replaces: req.replaces.map(str::to_string),
        slurm_job_id: (!req.attached).then(|| "__CHIMAERA_JOB_ID__".to_string()),
        attached: req.attached,
        submitted_ms: now_ms(),
        ..Default::default()
    };
    // What agents in this job may submit to: never a partition this cluster
    // already refused a batch job on.
    let mut facts = req.facts.clone();
    facts
        .partitions
        .retain(|p| !config.learned.interactive_only.contains(&p.name));
    let rules_source = config
        .agent_rules
        .file
        .as_deref()
        .filter(|f| f.starts_with('/'));
    let script_text = job_script(&JobScript {
        binary: &home.bin_path(),
        job_dir: &dir,
        state_home: home.job_state_home(),
        rules_source,
    });

    let mut s = String::from("umask 077\nunset SLURM_JOB_ID SLURM_JOBID\n");
    s.push_str(&path_line(host));
    let receipt = stable.then(chimaera_core::generate_token);
    if let Some(nonce) = &receipt {
        s.push_str(&format!("printf '===begin {nonce}\\n'\n"));
    }
    s.push_str(&submission_claim(&dir, stable));
    s.push_str(&write_file_lines("$D/job.sh", &script_text));
    s.push_str(&write_file_lines("$D/startup.sh", req.run_startup));
    s.push_str(&write_file_lines(
        "$D/agent-rules.md",
        &config.agent_rules.text,
    ));
    s.push_str(&write_file_lines(
        "$D/facts.json",
        &serde_json::to_string(&facts)?,
    ));
    let record_json = serde_json::to_string_pretty(&record)?;
    if req.attached {
        s.push_str(&write_file_lines("$D/job.json", &record_json));
        s.push_str("printf '===rc 0\\n===end\\n'\n");
    } else {
        s.push_str(&write_file_lines("$D/job.pending", &record_json));
        let args: Vec<String> = spec
            .sbatch_args(&job_name, "@OUTPUT@", req.facts.gpu_flag)
            .into_iter()
            .map(|a| {
                if a == "--output=@OUTPUT@" {
                    "\"--output=$D/job.log\"".to_string()
                } else {
                    sh_quote(&a)
                }
            })
            .collect();
        s.push_str(&submission_lines(&args.join(" "), stable));
    }
    let out = match run_script(host, &s, EXEC_SECS).await {
        Ok(out) => out,
        Err(_) if stable => return Err(StartUncertain { job: jid }.into()),
        Err(error) => return Err(error),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = if let Some(receipt) = &receipt {
        match submission_receipt(&stdout, receipt, req.attached) {
            Some(secs) => secs,
            None => return Err(StartUncertain { job: jid }.into()),
        }
    } else {
        sections(&stdout)
    };
    if stable
        && (section(&secs, "end").is_none()
            || matches!(marker_arg(&secs, "rc"), Some("96" | "97" | "98")))
    {
        return Err(StartUncertain { job: jid }.into());
    }
    if section(&secs, "end").is_none() {
        bail!(
            "starting a job on {host} failed: {}",
            super::ssh_failure_line(&out.stderr, &out.status)
        );
    }
    invalidate_queue(host);
    if req.attached {
        if stable && marker_arg(&secs, "rc") != Some("0") {
            return Err(StartUncertain { job: jid }.into());
        }
        return Ok(StartOutcome::Attached { job: jid, job_name });
    }
    let rc = marker_arg(&secs, "rc")
        .or_else(|| marker_arg(&secs, "preflight_rc"))
        .unwrap_or("1");
    if rc != "0" {
        if stable && !stable_batch_non_submission(&secs) {
            return Err(StartUncertain { job: jid }.into());
        }
        let message = clean_tool_stderr(section(&secs, "err").unwrap_or(""), "sbatch");
        return Err(StartRefused {
            kind: classify_refusal(&message),
            message,
        }
        .into());
    }
    let id = marker_arg(&secs, "id").unwrap_or("").to_string();
    Ok(StartOutcome::Submitted {
        job: jid,
        slurm_job_id: id,
    })
}

fn submission_receipt(stdout: &str, nonce: &str, attached: bool) -> Option<Vec<(String, String)>> {
    let begin = format!("===begin {nonce}");
    let mut lines = stdout.lines();
    lines.find(|line| *line == begin)?;
    let mut secs: Vec<(String, String)> = Vec::with_capacity(4);
    for line in lines {
        if let Some(key) = line.strip_prefix("===") {
            if secs.len() >= 4 {
                return None;
            }
            secs.push((key.trim_end().to_owned(), String::new()));
        } else if let Some((key, body)) = secs.last_mut() {
            if key != "err" && !line.trim().is_empty() {
                return None;
            }
            if body.len().saturating_add(line.len() + 1) > STARTUP_MAX {
                return None;
            }
            body.push_str(line);
            body.push('\n');
        }
    }
    let preflight = !attached
        && secs
            .first()
            .is_some_and(|(key, _)| key.starts_with("preflight_rc "));
    let status = if preflight { "preflight_rc" } else { "rc" };
    let expected = if attached {
        vec!["rc", "end"]
    } else {
        vec![status, "id", "err", "end"]
    };
    // An existing claim's rc98 receipt has no batch id; it is uncertainty.
    if secs.len() == 2 && secs[0].0 == "rc 98" && secs[1].0 == "end" {
        return Some(secs);
    }
    if secs.len() != expected.len()
        || secs
            .iter()
            .zip(expected)
            .any(|((key, _), want)| key.split_whitespace().next() != Some(want))
    {
        return None;
    }
    let rc = marker_arg(&secs, status)?.parse::<u16>().ok()?;
    if secs[0].0 != format!("{status} {rc}")
        || secs.last()?.0 != "end"
        || (preflight && (rc == 0 || rc > 255))
    {
        return None;
    }
    if !attached && rc == 0 {
        let id = marker_arg(&secs, "id")?;
        if !valid_slurm_job_id(id) || secs[1].0 != format!("id {id}") {
            return None;
        }
    }
    Some(secs)
}

/// A scheduler command can print an allocation ID and still fail afterward.
/// Its nonzero exit then cannot prove that no job was accepted. Called only
/// after the fresh nonce/exact frame parser succeeds, never on SSH diagnostics.
/// Shell execution errors and signal termination are not scheduler refusals.
fn stable_batch_non_submission(sections: &[(String, String)]) -> bool {
    if marker_arg(sections, "preflight_rc").is_some() {
        // The exact nonce phase is emitted only on the branch that exits before
        // real submission. Classification is guidance, not stderr authority.
        return sections.get(1).is_some_and(|(marker, _)| marker == "id");
    }
    let rejected = marker_arg(sections, "rc")
        .and_then(|rc| rc.parse::<u8>().ok())
        .is_some_and(|rc| (1..126).contains(&rc) && !matches!(rc, 96..=98));
    rejected
        && sections.get(1).is_some_and(|(marker, _)| marker == "id")
        && positive_scheduler_refusal(section(sections, "err").unwrap_or(""))
}

fn positive_scheduler_refusal(stderr: &str) -> bool {
    // SchedMD sbatch reports both controller rejection and send/receive failure
    // through the same nonzero exit. Only exact known controller policy codes
    // are evidence here; the broad UI classifier is deliberately not authority.
    // See src/sbatch/sbatch.c, src/api/submit.c and src/common/slurm_errno.c.
    let Some(reason) = stderr
        .trim()
        .strip_prefix("sbatch: error: Batch job submission failed: ")
    else {
        return false;
    };
    matches!(
        reason,
        "Invalid account or account/partition combination specified"
            | "Invalid qos specification"
            | "Invalid feature specification"
    )
}

fn submission_claim(dir: &str, stable: bool) -> String {
    if stable {
        // mkdir is the remote, non-expiring claim, before any sbatch effect.
        // Retain it on every uncertainty; never remove it merely to retry.
        format!("D=\"{dir}\"\nmkdir -p \"${{D%/*}}\" || exit 3\nif ! mkdir \"$D\"; then printf '===rc 98\\n===end\\n'; exit 0; fi\nset -e\n")
    } else {
        format!("D=\"{dir}\"\nmkdir -p \"$D\" || exit 3\n")
    }
}
fn submission_lines(args: &str, stable: bool) -> String {
    let before = if stable { "set +e\n" } else { "" };
    let after = if stable { "set -e\n" } else { "" };
    let locale = if stable { "LC_ALL=C LANG=C " } else { "" };
    let preflight = if stable {
        format!("{before}test_out=$(LC_ALL=C LANG=C sbatch --test-only {args} \"$D/job.sh\" 2>\"$D/.sbatch.err\"); test_rc=$?\n\
            if [ \"$test_rc\" -ne 0 ] || [ -n \"$test_out\" ]; then\n\
             printf '===preflight_rc %s\\n===id %s\\n===err\\n' \"$test_rc\" \"$test_out\"; cat \"$D/.sbatch.err\" 2>/dev/null || true; rm -f \"$D/.sbatch.err\"\n\
             printf '===end\\n'; exit 0\n\
            fi\n")
    } else {
        String::new()
    };
    let remove = if stable {
        ""
    } else {
        "rm -f \"$D/job.pending\"\n[ \"$rc\" -eq 0 ] || rm -rf \"$D\"\n"
    };
    // Only a stable submission turns a failed record write into its own
    // outcome. An ordinary one keeps main's behaviour: the job was queued,
    // so it is reported started and its folder (its job.sh) is never
    // removed under it.
    let record_failed = if stable { " || rc=96" } else { "" };
    format!("{preflight}{before}out=$({locale}sbatch {args} \"$D/job.sh\" 2>\"$D/.sbatch.err\"); rc=$?\n{after}\
             id=${{out%%;*}}\n\
             case \"$id\" in ''|*[!0-9_]*) if [ \"$rc\" -eq 0 ]; then rc=97; fi ;; esac\n\
             if [ \"$rc\" -eq 0 ]; then\n\
              sed \"s/__CHIMAERA_JOB_ID__/$id/\" \"$D/job.pending\" > \"$D/job.json.tmp\" && mv -f \"$D/job.json.tmp\" \"$D/job.json\"{record_failed}\n\
             fi\n\
             printf '===rc %s\\n===id %s\\n===err\\n' \"$rc\" \"$id\"; cat \"$D/.sbatch.err\" 2>/dev/null || true; rm -f \"$D/.sbatch.err\"\n\
             {remove}printf '===end\\n'\n")
}

/// Hold an attached job in the foreground: `ssh -tt host srun … job.sh`. The
/// pty makes the login node hang the job up when this connection ends —
/// app quit, laptop sleep, a dropped link — so nothing outlives the user's
/// session there. Dropping the returned child ends the job.
pub fn spawn_attached(
    host: &str,
    home: RemoteHome,
    jid: &str,
    spec: &LaunchSpec,
    job_name: &str,
    gpu_flag: GpuFlag,
) -> anyhow::Result<Child> {
    spawn_attached_inner(host, home, jid, spec, job_name, gpu_flag, false)
}

/// Keeper-owned interactive launch. A remote one-shot claim refuses a replay
/// after lost SSH/restart; the caller retains its hold until terminal proof.
pub fn spawn_attached_once(
    host: &str,
    home: RemoteHome,
    jid: &str,
    spec: &LaunchSpec,
    job_name: &str,
    gpu_flag: GpuFlag,
) -> anyhow::Result<Child> {
    anyhow::ensure!(valid_job_id(jid), "unknown job");
    anyhow::ensure!(
        job_name.ends_with(&format!("~{}", &jid[2..])),
        "job identity does not match"
    );
    spec.validate()
        .map_err(|_| anyhow::anyhow!("invalid launch specification"))?;
    spawn_attached_inner(host, home, jid, spec, job_name, gpu_flag, true)
}

fn attached_claim(dir: &str) -> String {
    format!("[ -f \"{dir}/job.json\" ] && mkdir \"{dir}/attached.started\" || exit 98; ")
}

fn spawn_attached_inner(
    host: &str,
    home: RemoteHome,
    jid: &str,
    spec: &LaunchSpec,
    job_name: &str,
    gpu_flag: GpuFlag,
    once: bool,
) -> anyhow::Result<Child> {
    anyhow::ensure!(valid_job_id(jid), "unknown job");
    let args: Vec<String> = spec
        .srun_args(job_name, gpu_flag)
        .iter()
        .map(|a| sh_quote(a))
        .collect();
    // One line, so each statement needs its `;`: the PATH line ends in a
    // newline, and a space there once made it `export PATH exec srun …`.
    let claim = if once {
        attached_claim(&job_dir(home, jid))
    } else {
        String::new()
    };
    let script = format!(
        "unset SLURM_JOB_ID SLURM_JOBID; {claim}{}exec srun {} /bin/bash \"{}/job.sh\"",
        path_line(host).replace('\n', "; "),
        args.join(" "),
        job_dir(home, jid)
    );
    let mut cmd = super::ssh_base(host);
    cmd.env(ASKPASS_ALIAS_ENV, host)
        .arg("-tt")
        .arg(host)
        .arg(super::sh_wrap(&script))
        .stdin(std::process::Stdio::piped())
        // The pty merges srun's errors into stdout; the caller keeps the
        // last lines to say why a job never started.
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    cmd.spawn().context("failed to start the attached job")
}

/// The last lines an attached job printed (srun's own words included — the
/// pty merges them into stdout), kept while it runs so an early end can say
/// why. Reads both pipes to their end (a full pipe would stall srun); `echo`
/// also passes each line to this process's stderr (the CLI's terminal).
pub fn attached_output(child: &mut Child, echo: bool) -> Arc<Mutex<VecDeque<String>>> {
    let tail = Arc::new(Mutex::new(VecDeque::with_capacity(ATTACHED_TAIL_LINES)));
    let pipes: [Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>; 2] = [
        child.stdout.take().map(|p| Box::new(p) as _),
        child.stderr.take().map(|p| Box::new(p) as _),
    ];
    for pipe in pipes.into_iter().flatten() {
        let tail = tail.clone();
        tokio::spawn(drain_attached(pipe, tail, echo));
    }
    tail
}

/// How many of an attached job's last lines `attached_output` keeps.
const ATTACHED_TAIL_LINES: usize = 8;
/// The most of one line that is kept; the rest of it is still drained.
const ATTACHED_LINE_BYTES: usize = 4096;

/// Read `pipe` to its end, keeping its last `ATTACHED_TAIL_LINES` lines in
/// `tail`. An attached process may never print a newline, may print invalid
/// UTF-8, or may redraw one line with carriage returns, so this reads raw
/// blocks rather than `lines()`: a line keeps at most `ATTACHED_LINE_BYTES`,
/// bytes decode lossily, and `\r` ends a line like `\n` does (so the last
/// redraw is the one kept, and a pty's `\r\n` still yields one line). Nothing
/// here may panic or return while the pipe is open: an unread pipe fills and
/// stalls srun.
async fn drain_attached<R>(mut pipe: R, tail: Arc<Mutex<VecDeque<String>>>, echo: bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use std::io::Write as _;
    use tokio::io::AsyncReadExt;
    let mut block = [0; ATTACHED_LINE_BYTES];
    let mut line = Vec::with_capacity(ATTACHED_LINE_BYTES);
    let emit = |line: &[u8]| {
        let text = plain_line(&String::from_utf8_lossy(line));
        if text.is_empty() || text.starts_with("Shared connection to") {
            return;
        }
        if echo {
            // Not `eprintln!`: with stderr's reader gone (`2>&1 | head`) it
            // panics, which would end this task and leave the pipe unread.
            let _ = writeln!(std::io::stderr().lock(), "{text}");
        }
        let mut tail = tail.lock().unwrap_or_else(|p| p.into_inner());
        if tail.len() >= ATTACHED_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(text.chars().take(300).collect());
    };
    loop {
        let count = match pipe.read(&mut block).await {
            Ok(count) => count,
            Err(err) => {
                tracing::debug!(%err, "an attached job's output pipe failed before its end");
                0
            }
        };
        if count == 0 {
            if !line.is_empty() {
                emit(&line);
            }
            return;
        }
        for byte in &block[..count] {
            if matches!(byte, b'\n' | b'\r') {
                emit(&line);
                line.clear();
            } else if line.len() < ATTACHED_LINE_BYTES {
                line.push(*byte);
            }
        }
    }
}

/// A terminal line without its colors and carriage returns.
fn plain_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                if chars.next() == Some('[') {
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            }
            '\r' => {}
            c => out.push(c),
        }
    }
    out.trim().to_string()
}

/// Stop a job (`scancel`, which tells job-host first so every workspace in
/// it saves its chats) and mark it stopped by the user. "Invalid job id"
/// means it's already gone — success.
pub async fn stop_job(host: &str, home: RemoteHome, record: &JobRecord) -> anyhow::Result<()> {
    anyhow::ensure!(valid_job_id(&record.id), "unknown job");
    let target = match &record.slurm_job_id {
        Some(id) if valid_slurm_job_id(id) => sh_quote(id),
        _ => format!("--name={} -u \"$(id -un)\"", sh_quote(&record.job_name)),
    };
    let mut marked = record.clone();
    marked.stopped_by_user = true;
    let mut s = path_line(host);
    s.push_str(&format!("umask 077\nD=\"{}\"\n", job_dir(home, &record.id)));
    s.push_str(&scancel_lines(&target));
    // Marked only when the stop took (or the job was already gone): a stop
    // that failed must not make a job that later ends on its own read as
    // "stopped by you".
    s.push_str("if [ \"$ok\" = 1 ] && [ -d \"$D\" ]; then\n");
    s.push_str(&write_file_lines(
        "$D/job.json",
        &serde_json::to_string_pretty(&marked)?,
    ));
    s.push_str("fi\nprintf '===end\\n'\n");
    let out = run_script(host, &s, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    invalidate_queue(host);
    if section(&secs, "end").is_none() {
        bail!(
            "stopping the job on {host} failed: {}",
            super::ssh_failure_line(&out.stderr, &out.status)
        );
    }
    if marker_arg(&secs, "ok") != Some("1") {
        bail!(
            "{}",
            clean_tool_stderr(section(&secs, "err").unwrap_or(""), "scancel")
        );
    }
    Ok(())
}

/// Add a workspace to a job's start list while the job waits: its job-host
/// opens it when the job starts. A job-host that read its record a moment
/// before is the caller's to cover (open it through the job-host once the
/// job runs).
pub async fn queue_open(
    host: &str,
    home: RemoteHome,
    record: &JobRecord,
    wid: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(valid_job_id(&record.id), "unknown job");
    anyhow::ensure!(valid_workspace_id(wid), "unknown workspace");
    if record.open.iter().any(|w| w == wid) {
        return Ok(());
    }
    let mut next = record.clone();
    next.open.push(wid.to_string());
    let mut s = format!(
        "umask 077\nD=\"{}\"\n[ -f \"$D/job.json\" ] || exit 3\n",
        job_dir(home, &record.id)
    );
    s.push_str(&write_file_lines(
        "$D/job.json",
        &serde_json::to_string_pretty(&next)?,
    ));
    s.push_str("printf '===end\\n'\n");
    let out = run_script(host, &s, EXEC_SECS).await?;
    anyhow::ensure!(
        section(&sections(&String::from_utf8_lossy(&out.stdout)), "end").is_some(),
        "couldn't update the job on {host}: {}",
        super::ssh_failure_line(&out.stderr, &out.status)
    );
    invalidate_queue(host);
    Ok(())
}

/// Ask accounting once how a job ended and keep the answer in its record.
/// Clusters without accounting answer "ENDED".
pub async fn record_end(host: &str, home: RemoteHome, record: &JobRecord) -> anyhow::Result<Ended> {
    anyhow::ensure!(valid_job_id(&record.id), "unknown job");
    let id = record.slurm_job_id.clone().unwrap_or_default();
    let mut s = path_line(host);
    if valid_slurm_job_id(&id) {
        s.push_str(&format!(
            "printf '===sacct\\n'; sacct -nX -j {} -o State 2>/dev/null | head -n 1\n",
            sh_quote(&id)
        ));
    }
    s.push_str("printf '===now %s\\n===end\\n' \"$(date +%s)\"\n");
    let out = run_script(host, &s, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    let state = section(&secs, "sacct")
        .and_then(|t| slurm::terminal_state(t.trim()))
        .unwrap_or("ENDED")
        .to_string();
    let at_ms = marker_arg(&secs, "now")
        .and_then(|n| n.parse::<u64>().ok())
        .map(|n| n * 1000)
        .unwrap_or_else(now_ms);
    let ended = Ended { state, at_ms };
    let mut kept = record.clone();
    kept.ended = Some(ended.clone());
    let mut w = format!(
        "umask 077\nD=\"{}\"\n[ -d \"$D\" ] || exit 0\n",
        job_dir(home, &record.id)
    );
    w.push_str(&write_file_lines(
        "$D/job.json",
        &serde_json::to_string_pretty(&kept)?,
    ));
    run_script(host, &w, EXEC_SECS).await?;
    Ok(ended)
}

/// Positive scheduler evidence for one keeper-stable submission, independent
/// of queue absence and the presentation-only `ENDED` fallback. Unknown,
/// failed, malformed, mismatched or conflicting observations never prove idle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalEvidence {
    pub job_id: String,
    pub slurm_job_id: String,
    pub state: &'static str,
    pub source: TerminalSource,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalSource {
    Accounting,
    Controller,
}

/// Uses routed SSH, bounded output/deadline and exact stable scheduler name.
/// `observed_id` is the last positively observed interactive allocation id;
/// omission still permits exact-name accounting recovery, never name guessing.
/// Call at the existing scheduler polling floor, never once per viewer.
pub async fn terminal_evidence(
    host: &str,
    record: &JobRecord,
    observed_id: Option<&str>,
) -> anyhow::Result<Option<TerminalEvidence>> {
    let expected = terminal_identity(record, observed_id)?;
    let age = terminal_query_age(record.submitted_ms, now_ms());
    if age.is_none() && expected.is_none() {
        return Ok(None);
    }
    let nonce = chimaera_core::generate_token();
    let script = format!(
        "{}{}",
        path_line(host),
        terminal_script(&record.job_name, expected, age, &nonce)
    );
    let output = run_script(host, &script, EXEC_SECS).await?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(terminal_receipt(&output.stdout, &nonce, record, expected))
}
fn terminal_identity<'a>(
    record: &'a JobRecord,
    observed_id: Option<&'a str>,
) -> anyhow::Result<Option<&'a str>> {
    anyhow::ensure!(
        valid_job_id(&record.id)
            && record.job_name.starts_with("chimaera-")
            && record.job_name.ends_with(&format!("~{}", &record.id[2..]))
            && record.job_name.len() <= 128
            && record
                .job_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-~".contains(&b)),
        "invalid stable job identity"
    );
    if let (Some(a), Some(b)) = (record.slurm_job_id.as_deref(), observed_id) {
        anyhow::ensure!(a == b, "scheduler job identity changed");
    }
    let id = record.slurm_job_id.as_deref().or(observed_id);
    anyhow::ensure!(
        id.is_none_or(|id| valid_slurm_job_id(id) && id.starts_with(|c: char| c.is_ascii_digit())),
        "invalid scheduler job identity"
    );
    Ok(id)
}
// Ten minutes allows clock skew; a month bounds accounting history even for
// a damaged/ancient journal. Older entries still permit exact controller proof.
fn terminal_query_age(submitted_ms: u64, now: u64) -> Option<u64> {
    const MONTH: u64 = 31 * 24 * 3600;
    if submitted_ms == 0 || submitted_ms > now.saturating_add(600_000) {
        return None;
    }
    let age = now.saturating_sub(submitted_ms) / 1000;
    (age <= MONTH - 600).then_some(age + 600)
}
fn terminal_script(name: &str, id: Option<&str>, age: Option<u64>, nonce: &str) -> String {
    let select = id
        .map(|id| format!("-j {}", sh_quote(id)))
        .unwrap_or_default();
    let accounting = age.map(|age| format!("sacct -nPXD {select} -S now-{age}seconds --name={} -o JobID%64,JobName%128,State%64,UID 2>/dev/null; rc=$?", sh_quote(name))).unwrap_or_else(|| "rc=1".into());
    let controller = id
        .map(|id| format!("scontrol -o show job {} 2>/dev/null; rc=$?", sh_quote(id)))
        .unwrap_or_else(|| "rc=1".into());
    format!(
        "printf '===begin {nonce}\n===sacct\n'\n\
        {accounting}\n\
        printf '\n===sacct_rc %s\n===scontrol\n' \"$rc\"\n\
        {controller}\n\
        printf '\n===scontrol_rc %s\n===uid %s\n===end\n' \"$rc\" \"$(id -u)\"\n"
    )
}
#[derive(Clone, Debug)]
enum TerminalObservation {
    Unknown,
    Live,
    Terminal(TerminalEvidence),
    Conflict,
}
fn terminal_receipt(
    bytes: &[u8],
    nonce: &str,
    record: &JobRecord,
    expected: Option<&str>,
) -> Option<TerminalEvidence> {
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let stdout = std::str::from_utf8(bytes).ok()?;
    let begin = format!("===begin {nonce}\n");
    let start = stdout.lines().position(|line| line == begin.trim_end())?;
    let framed = stdout.lines().skip(start).collect::<Vec<_>>().join("\n");
    let secs = sections(&framed);
    if secs.len() != 7
        || secs[0].0 != format!("begin {nonce}")
        || secs[1].0 != "sacct"
        || !secs[2].0.starts_with("sacct_rc ")
        || secs[3].0 != "scontrol"
        || !secs[4].0.starts_with("scontrol_rc ")
        || !secs[5].0.starts_with("uid ")
        || secs[6].0 != "end"
        || secs
            .iter()
            .enumerate()
            .any(|(i, (_, body))| i != 1 && i != 3 && !body.trim().is_empty())
    {
        return None;
    }
    let rc = |index: usize| secs[index].0.split_once(' ')?.1.parse::<u8>().ok();
    let uid = secs[5].0.strip_prefix("uid ")?.parse::<u32>().ok()?;
    let accounting = if rc(2)? == 0 {
        terminal_accounting(&secs[1].1, record, expected, uid)
    } else {
        TerminalObservation::Unknown
    };
    let controller = if rc(4)? == 0 {
        terminal_controller(&secs[3].1, record, expected, uid)
    } else {
        TerminalObservation::Unknown
    };
    match (accounting, controller) {
        (TerminalObservation::Live | TerminalObservation::Conflict, _)
        | (_, TerminalObservation::Live | TerminalObservation::Conflict) => None,
        (TerminalObservation::Terminal(a), TerminalObservation::Terminal(b))
            if a.slurm_job_id != b.slurm_job_id || a.state != b.state =>
        {
            None
        }
        (TerminalObservation::Terminal(a), _) => Some(a),
        (_, TerminalObservation::Terminal(b)) => Some(b),
        _ => None,
    }
}
fn terminal_accounting(
    body: &str,
    record: &JobRecord,
    expected: Option<&str>,
    uid: u32,
) -> TerminalObservation {
    let mut evidence: Option<TerminalEvidence> = None;
    for (n, line) in body.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        if n >= 1 {
            return TerminalObservation::Conflict;
        }
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if fields.len() != 4
            || !valid_slurm_job_id(fields[0])
            || !fields[0].starts_with(|c: char| c.is_ascii_digit())
            || expected.is_some_and(|id| id != fields[0])
            || fields[1] != record.job_name
            || fields[3].parse::<u32>().ok() != Some(uid)
        {
            return TerminalObservation::Conflict;
        }
        let Some(state) = exact_terminal_state(fields[2]) else {
            return TerminalObservation::Live;
        };
        let next = TerminalEvidence {
            job_id: record.id.clone(),
            slurm_job_id: fields[0].into(),
            state,
            source: TerminalSource::Accounting,
        };
        if evidence
            .as_ref()
            .is_some_and(|e| e.slurm_job_id != next.slurm_job_id || e.state != next.state)
        {
            return TerminalObservation::Conflict;
        }
        evidence = Some(next);
    }
    evidence
        .map(TerminalObservation::Terminal)
        .unwrap_or(TerminalObservation::Unknown)
}
fn terminal_controller(
    body: &str,
    record: &JobRecord,
    expected: Option<&str>,
    uid: u32,
) -> TerminalObservation {
    let mut lines = body.lines().filter(|l| !l.trim().is_empty());
    let Some(line) = lines.next() else {
        return TerminalObservation::Unknown;
    };
    if lines.next().is_some() {
        return TerminalObservation::Conflict;
    }
    let mut fields = std::collections::BTreeMap::new();
    for part in line.split_whitespace() {
        if let Some((key, value)) = part.split_once('=') {
            if fields.insert(key, value).is_some() {
                return TerminalObservation::Conflict;
            }
        }
    }
    let Some(id) = fields.get("JobId") else {
        return TerminalObservation::Conflict;
    };
    let owner = fields
        .get("UserId")
        .and_then(|u| u.rsplit_once('('))
        .and_then(|(_, n)| n.strip_suffix(')'))
        .and_then(|n| n.parse::<u32>().ok());
    if expected != Some(*id)
        || fields.get("JobName").copied() != Some(record.job_name.as_str())
        || owner != Some(uid)
    {
        return TerminalObservation::Conflict;
    }
    let Some(state) = fields.get("JobState").and_then(|s| exact_terminal_state(s)) else {
        return TerminalObservation::Live;
    };
    TerminalObservation::Terminal(TerminalEvidence {
        job_id: record.id.clone(),
        slurm_job_id: (*id).into(),
        state,
        source: TerminalSource::Controller,
    })
}
fn exact_terminal_state(state: &str) -> Option<&'static str> {
    slurm::TERMINAL_STATES
        .iter()
        .find(|candidate| {
            state == **candidate
                || (**candidate == "CANCELLED"
                    && state
                        .strip_prefix("CANCELLED by ")
                        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())))
        })
        .copied()
}

/// Forget an ended job: remove its folder (its script and Slurm's output).
/// Workspaces keep their chats in their own folders.
pub async fn dismiss_job(host: &str, home: RemoteHome, jid: &str) -> anyhow::Result<()> {
    anyhow::ensure!(valid_job_id(jid), "unknown job");
    let script = format!("rm -rf -- \"{}\"\n", job_dir(home, jid));
    run_script(host, &script, EXEC_SECS).await?;
    Ok(())
}

/// Slurm's own estimate of when a waiting job starts (epoch ms, cluster
/// clock), when it has one.
pub async fn start_estimate(host: &str, slurm_job_id: &str) -> anyhow::Result<Option<u64>> {
    anyhow::ensure!(valid_slurm_job_id(slurm_job_id), "invalid job id");
    let mut s = path_line(host);
    s.push_str(&format!(
        "printf '===start\\n'; squeue --start -h -j {} -o '%S' 2>/dev/null | head -n 1\n\
         printf '===now %s\\n===tz %s\\n===end\\n' \"$(date +%s)\" \"$(date +%z)\"\n",
        sh_quote(slurm_job_id)
    ));
    let out = run_script(host, &s, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    let text = section(&secs, "start").unwrap_or("").trim();
    let tz = marker_arg(&secs, "tz").unwrap_or("+0000");
    Ok(parse_slurm_timestamp(text, tz))
}

/// `2026-10-03T14:20:00` in the cluster's local zone (`+hhmm`) → epoch ms.
fn parse_slurm_timestamp(text: &str, tz: &str) -> Option<u64> {
    let (date, time) = text.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (h, mi, sec) = (t.next()??, t.next()??, t.next().flatten().unwrap_or(0));
    // days_from_civil (Howard Hinnant), the inverse of the formatter in core.
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let local = days * 86_400 + h * 3_600 + mi * 60 + sec;
    let sign = if tz.starts_with('-') { -1 } else { 1 };
    let digits: String = tz.chars().filter(|c| c.is_ascii_digit()).collect();
    let offset = if digits.len() == 4 {
        let hh: i64 = digits[..2].parse().ok()?;
        let mm: i64 = digits[2..].parse().ok()?;
        sign * (hh * 3_600 + mm * 60)
    } else {
        0
    };
    let utc = local - offset;
    (utc > 0).then_some(utc as u64 * 1000)
}

// --- Setups and rules -------------------------------------------------------------

/// Remember what the last job on this cluster started with, and save a
/// named setup when asked.
pub async fn remember_spec(
    host: &str,
    home: RemoteHome,
    spec: &LaunchSpec,
    save_as: Option<&str>,
) -> anyhow::Result<()> {
    let spec = spec.clone().normalized();
    update_config(host, home, |config| {
        config.last_spec = Some(spec.clone());
        if let Some(name) = save_as.map(str::trim).filter(|n| !n.is_empty()) {
            let name: String = name.chars().take(60).collect();
            config.setups.retain(|s| s.name != name);
            config.setups.push(Setup {
                name,
                spec: spec.clone(),
            });
        }
        Ok(())
    })
    .await
}

/// Forget a saved setup.
pub async fn forget_setup(host: &str, home: RemoteHome, name: &str) -> anyhow::Result<()> {
    update_config(host, home, |config| {
        config.setups.retain(|s| s.name != name);
        Ok(())
    })
    .await
}

/// Remember what a refusal taught about this cluster.
pub async fn learn_refusal(
    host: &str,
    home: RemoteHome,
    partition: Option<&str>,
    kind: Refusal,
) -> anyhow::Result<()> {
    let field = match kind {
        Refusal::AccountRequired => Some("account"),
        Refusal::QosRequired => Some("qos"),
        Refusal::ConstraintRequired => Some("constraint"),
        Refusal::BatchNotAllowed | Refusal::Other => None,
    };
    update_config(host, home, |config| {
        if kind == Refusal::BatchNotAllowed {
            if let Some(p) = partition {
                if !config.learned.interactive_only.iter().any(|x| x == p) {
                    config.learned.interactive_only.push(p.to_string());
                }
            }
        }
        if let Some(field) = field {
            if !config.learned.requires.iter().any(|x| x == field) {
                config.learned.requires.push(field.to_string());
            }
        }
        Ok(())
    })
    .await
}

pub async fn set_agent_rules(
    host: &str,
    home: RemoteHome,
    rules: &AgentRules,
) -> anyhow::Result<()> {
    if let Some(f) = &rules.file {
        anyhow::ensure!(
            f.starts_with('/') && !f.contains('\n') && f.len() <= 1024,
            "Point at an absolute path on the cluster"
        );
    }
    anyhow::ensure!(rules.text.len() <= 32 * 1024, "Rules are limited to 32 KB");
    update_config(host, home, |config| {
        config.agent_rules = AgentRules {
            file: rules.file.clone().filter(|f| !f.trim().is_empty()),
            text: rules.text.clone(),
        };
        Ok(())
    })
    .await
}

// --- The binary -------------------------------------------------------------------

/// Make sure the chimaera binary jobs run is on the cluster's shared home and
/// is exactly this build's — a file copy, nothing started. Compared by
/// sha256, so an unchanged binary is never copied again; a replaced one
/// takes the name atomically (`mv`), so a running job keeps the inode it
/// started from.
pub async fn ensure_cluster_binary(
    host: &str,
    home: RemoteHome,
    binary: Option<&std::path::Path>,
    progress: &impl Fn(super::Phase),
) -> anyhow::Result<()> {
    let local = super::resolve_local_binary(host, binary, home, progress).await?;
    let want = super::sha256_file(&local).await?;
    let script = format!(
        "b=\"{}\"\n[ -x \"$b\" ] || exit 0\n\
         if command -v sha256sum >/dev/null 2>&1; then printf '===sum %s\\n' \"$(sha256sum < \"$b\" | cut -c1-64)\"; \
         elif command -v shasum >/dev/null 2>&1; then printf '===sum %s\\n' \"$(shasum -a 256 < \"$b\" | cut -c1-64)\"; fi\n\
         printf '===end\\n'\n",
        home.bin_path()
    );
    let out = run_script(host, &script, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    if marker_arg(&secs, "sum").is_some_and(|have| have.eq_ignore_ascii_case(&want)) {
        return Ok(());
    }
    super::deploy_binary(host, &local, home, progress).await
}

// --- job-host's API, over the job's forward ---------------------------------------------

/// What an open request got.
#[derive(Clone, Debug, PartialEq)]
pub enum HostOpen {
    /// Opening (or already open) here.
    Opened(HostedWorkspace),
    /// Another live job holds it.
    Held(HeldElsewhere),
    /// job-host said no (its words).
    Refused(String),
}

/// One request to job-host through the local end of its forward: status
/// and JSON body. Bounded; `Connection: close`, no keep-alive to manage.
async fn host_request(
    local_port: u16,
    token: &str,
    method: &str,
    path: &str,
    secs: u64,
) -> anyhow::Result<(u16, serde_json::Value)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let go = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", local_port))
            .await
            .context("the job's forward is down")?;
        stream
            .write_all(
                format!(
                    "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{local_port}\r\n\
                     Authorization: Bearer {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await?;
        let mut buf = Vec::new();
        stream
            .take(1024 * 1024)
            .read_to_end(&mut buf)
            .await
            .context("the job didn't answer")?;
        let split = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .context("the job's answer was cut short")?;
        let head = String::from_utf8_lossy(&buf[..split]);
        let status: u16 = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .context("the job's answer had no status")?;
        let body = serde_json::from_slice(&buf[split + 4..]).unwrap_or(serde_json::Value::Null);
        Ok::<_, anyhow::Error>((status, body))
    };
    tokio::time::timeout(Duration::from_secs(secs), go)
        .await
        .map_err(|_| anyhow::anyhow!("the job didn't answer in time"))?
}

/// `GET /api/v1/job`.
/// Where workspace `wid` listens in job `jid`, from that job's own job-host
/// (never the shared filesystem, which can show a new manifest on the login
/// node only a minute later). `None` until it is open.
pub fn endpoint_from(status: &JobHostStatus, jid: &str, wid: &str) -> Option<Endpoint> {
    let w = status
        .workspaces
        .iter()
        .find(|w| w.id == wid && w.state == HostedState::Open)?;
    Some(Endpoint {
        job: jid.to_string(),
        slurm_job_id: status.slurm_job_id.clone(),
        node: status.node.clone(),
        port: w.port?,
        token: w.token.clone()?,
        build: w.build.clone().unwrap_or_default(),
    })
}

/// Overlay what running job `jid`'s own job-host says it holds now. The
/// cluster folder can show an open or a close on the login node only a
/// minute later; job-host's word can't be stale. A workspace it doesn't
/// list isn't open there, whatever the folder still says (startup opens are
/// listed before it answers at all).
pub fn apply_hosting(ov: &mut ClusterOverview, jid: &str, status: &JobHostStatus) {
    if !ov
        .jobs
        .iter()
        .any(|j| j.id == jid && j.state == "running" && !j.stopping)
    {
        return;
    }
    let endpoints = &mut ov.endpoints;
    for v in &mut ov.workspaces {
        let held = status.workspaces.iter().find(|w| w.id == v.id);
        let here = v.job.as_deref() == Some(jid);
        match held.map(|w| w.state) {
            Some(HostedState::Starting) => {
                v.state = "queued";
                v.opening = true;
                v.closing = false;
                v.failed = None;
            }
            Some(state @ (HostedState::Open | HostedState::Closing)) => {
                v.state = "open";
                v.opening = false;
                v.closing = state == HostedState::Closing;
                v.failed = None;
                if let Some(e) = endpoint_from(status, jid, &v.id) {
                    endpoints.insert(v.id.clone(), e);
                }
            }
            Some(HostedState::Failed) => {
                v.state = "closed";
                v.opening = false;
                v.closing = false;
                v.failed = held.map(|w| w.detail.clone());
            }
            None if here => {
                v.state = "closed";
                v.job = None;
                v.opening = false;
                v.closing = false;
                v.failed = None;
            }
            None => continue,
        }
        if held.is_some() {
            v.job = Some(jid.to_string());
        }
        v.working = held
            .filter(|w| w.state == HostedState::Open)
            .map(|w| w.working);
        if v.state == "closed" && endpoints.get(&v.id).is_some_and(|e| e.job == jid) {
            endpoints.remove(&v.id);
        }
    }
}

pub async fn host_status(local_port: u16, token: &str) -> anyhow::Result<JobHostStatus> {
    let (status, body) = host_request(local_port, token, "GET", "/api/v1/job", 8).await?;
    anyhow::ensure!(status == 200, "the job answered HTTP {status}");
    Ok(serde_json::from_value(body)?)
}

/// Open workspace `wid` in the job behind `local_port`.
pub async fn host_open(local_port: u16, token: &str, wid: &str) -> anyhow::Result<HostOpen> {
    anyhow::ensure!(valid_workspace_id(wid), "unknown workspace");
    let (status, body) = host_request(
        local_port,
        token,
        "POST",
        &format!("/api/v1/job/workspaces/{wid}/open"),
        15,
    )
    .await?;
    Ok(match status {
        200 => HostOpen::Opened(serde_json::from_value(body)?),
        409 if body.get("slurm_job_id").is_some() => HostOpen::Held(serde_json::from_value(body)?),
        _ => HostOpen::Refused(
            body.get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("the job couldn't open it")
                .to_string(),
        ),
    })
}

/// Close workspace `wid` in the job behind `local_port`; returns once its
/// chimaera saved its chats and exited (job-host waits up to 25 s).
pub async fn host_close(local_port: u16, token: &str, wid: &str) -> anyhow::Result<()> {
    anyhow::ensure!(valid_workspace_id(wid), "unknown workspace");
    let (status, body) = host_request(
        local_port,
        token,
        "POST",
        &format!("/api/v1/job/workspaces/{wid}/close"),
        40,
    )
    .await?;
    anyhow::ensure!(
        status == 200,
        "{}",
        body.get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("the job couldn't close it")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chimaera_core::cluster::JobFiles;
    use chimaera_core::Manifest;

    fn queue_row(id: &str, state: &str, node: &str, left: &str, reason: &str) -> Job {
        Job {
            id: id.into(),
            name: "chimaera-long~ab12cd".into(),
            partition: "batch".into(),
            state: state.into(),
            time_left: left.into(),
            nodes: node.into(),
            cpus: "8".into(),
            mem: "32G".into(),
            elapsed: String::new(),
            workdir: String::new(),
            reason: reason.into(),
        }
    }

    fn record(jid: &str, slurm: Option<&str>, submitted_ms: u64) -> JobRecord {
        JobRecord {
            id: jid.into(),
            name: "Long".into(),
            job_name: "chimaera-long~ab12cd".into(),
            spec: LaunchSpec {
                time: "7-00:00:00".into(),
                ..Default::default()
            },
            open: vec!["w-0000abcd".into()],
            slurm_job_id: slurm.map(str::to_string),
            submitted_ms,
            ..Default::default()
        }
    }

    fn host(slurm: &str) -> HostRecord {
        HostRecord {
            job: "j-0000aaaa".into(),
            slurm_job_id: slurm.into(),
            node: "n042".into(),
            port: 41000,
            token: "host-token".into(),
            pid: 1,
            started_ms: 0,
            build: String::new(),
        }
    }

    fn manifest(slurm: &str) -> Manifest {
        Manifest {
            hostname: "n042".into(),
            port: 42000,
            token: "ws-token".into(),
            pid: 2,
            version: "0".into(),
            started_at: 0,
            build: None,
            slurm_job_id: Some(slurm.into()),
            runtime_leases: false,
            daemon_extension: false,
        }
    }

    const NOW: u64 = 1_000_000_000;
    #[test]
    fn original_records_and_job_host_preserve_each_endpoint_build() {
        let config = ClusterConfig {
            workspaces: vec![ClusterWorkspace {
                id: "w-0000abcd".into(),
                name: "fixture".into(),
                path: "/fixture/project".into(),
                created_ms: 0,
            }],
            ..Default::default()
        };
        let mut original_host = host("77");
        original_host.build = "abcdef1.123".into();
        let mut original_workspace = manifest("77");
        original_workspace.build = Some("abcdef1.124".into());
        let state = BrowseState {
            config: config.clone(),
            jobs: vec![JobFiles {
                record: record("j-0000aaaa", Some("77"), NOW - 500_000),
                host: Some(original_host),
                egress: None,
                hosting: None,
            }],
            manifests: [("w-0000abcd".into(), original_workspace)]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let queue = [queue_row("77", "RUNNING", "n042", "1:00:00", "None")];
        let observed = build(&config, &state, &queue, true, NOW);
        assert_eq!(observed.hosts["j-0000aaaa"].build, "abcdef1.123");
        assert_eq!(observed.endpoints["w-0000abcd"].build, "abcdef1.124");
        let mut live = JobHostStatus {
            job: "j-0000aaaa".into(),
            slurm_job_id: "77".into(),
            node: "n042".into(),
            workspaces: vec![HostedWorkspace {
                id: "w-0000abcd".into(),
                state: HostedState::Open,
                port: Some(43000),
                token: Some("successor-token".into()),
                build: Some("abcdef1.125".into()),
                ..Default::default()
            }],
        };
        let mut overview = ClusterOverview {
            jobs: observed.jobs,
            workspaces: observed.workspaces,
            endpoints: observed.endpoints,
            ..Default::default()
        };
        apply_hosting(&mut overview, "j-0000aaaa", &live);
        let endpoint = &overview.endpoints["w-0000abcd"];
        assert_eq!(
            (
                endpoint.port,
                endpoint.token.as_str(),
                endpoint.build.as_str()
            ),
            (43000, "successor-token", "abcdef1.125")
        );
        live.workspaces[0].build = None;
        assert_eq!(
            endpoint_from(&live, "j-0000aaaa", "w-0000abcd")
                .unwrap()
                .build,
            ""
        );
    }

    #[test]
    fn a_job_waits_starts_and_runs_with_the_queue_and_its_host_record() {
        let r = record("j-0000aaaa", Some("77"), NOW - 500_000);
        let (v, e) = job_view(
            &r,
            None,
            None,
            &[queue_row("77", "PENDING", "", "7-00:00:00", "Resources")],
            true,
            NOW,
        );
        assert_eq!(
            (v.state, v.reason.as_str(), v.ends_at_ms),
            ("waiting", "Resources", None)
        );
        assert!(e.is_none());
        let (v, _) = job_view(
            &r,
            None,
            None,
            &[queue_row("77", "PENDING", "", "1:00", "Priority")],
            true,
            NOW,
        );
        assert_eq!(v.reason, "", "plain priority isn't worth saying");
        let running = [queue_row("77", "RUNNING", "n042", "1:00:00", "None")];
        let (v, e) = job_view(&r, None, None, &running, true, NOW);
        assert_eq!(v.state, "starting", "no job-host record yet");
        assert!(e.is_none());
        let (v, e) = job_view(&r, Some(&host("76")), None, &running, true, NOW);
        assert_eq!(
            v.state, "starting",
            "an older job's record is not this job's"
        );
        assert!(e.is_none());
        let (v, e) = job_view(&r, Some(&host("77")), Some(true), &running, true, NOW);
        assert_eq!(v.state, "running");
        assert_eq!(v.ends_at_ms, Some(NOW + 3_600_000));
        assert_eq!(
            (v.node.as_str(), v.cpus.as_str(), v.egress),
            ("n042", "8", Some(true))
        );
        let e = e.unwrap();
        assert_eq!(
            (e.port, e.token.as_str(), e.slurm_job_id.as_str()),
            (41000, "host-token", "77")
        );
    }

    #[test]
    fn stopping_jobs_never_become_starting_or_offer_an_endpoint() {
        for state in ["RUNNING", "PENDING", "COMPLETING"] {
            let mut record = record("j-0000aaaa", Some("77"), NOW - 10_000);
            record.stopped_by_user = state != "COMPLETING";
            let queue = [queue_row("77", state, "n042", "1:00:00", "None")];
            for host in [None, Some(host("77"))] {
                let (view, endpoint) = job_view(&record, host.as_ref(), None, &queue, true, NOW);
                assert!(view.stopping);
                assert_ne!(view.state, "starting");
                assert!(endpoint.is_none());
            }
            record.stopped_by_user = true;
            assert_eq!(
                job_view(&record, None, None, &[], true, NOW).0.state,
                "ended"
            );
        }
    }

    #[test]
    fn a_job_out_of_the_queue_ended_unless_just_submitted() {
        let fresh = record("j-0000aaaa", Some("77"), NOW - 10_000);
        assert_eq!(
            job_view(&fresh, None, None, &[], true, NOW).0.state,
            "waiting"
        );
        let old = record("j-0000aaaa", Some("77"), NOW - 10_000_000);
        let (v, _) = job_view(&old, None, None, &[], true, NOW);
        assert_eq!((v.state, v.ended.as_deref()), ("ended", None));
        let mut kept = old.clone();
        kept.ended = Some(Ended {
            state: "TIMEOUT".into(),
            at_ms: 5,
        });
        let (v, _) = job_view(&kept, None, None, &[], true, NOW);
        assert_eq!(
            (v.ended.as_deref(), v.ended_at_ms),
            (Some("TIMEOUT"), Some(5))
        );
        // The queue says how it ended when it still lists it.
        let (v, _) = job_view(
            &old,
            None,
            None,
            &[queue_row("77", "CANCELLED by 1", "", "0:00", "")],
            true,
            NOW,
        );
        assert_eq!(v.ended.as_deref(), Some("CANCELLED"));
        // No queue read at all: a job-host record means it ran.
        let (v, e) = job_view(&old, Some(&host("77")), None, &[], false, NOW);
        assert_eq!(v.state, "running");
        assert!(e.is_some());
    }

    #[test]
    fn an_attached_job_is_found_by_its_unique_name() {
        let mut r = record("j-0000aaaa", None, NOW);
        r.attached = true;
        let (v, _) = job_view(
            &r,
            Some(&host("90")),
            None,
            &[queue_row("90", "RUNNING", "n9", "1:00", "")],
            true,
            NOW,
        );
        assert_eq!(
            (v.state, v.slurm_job_id.as_deref()),
            ("running", Some("90"))
        );
        assert!(v.attached);
    }

    #[test]
    fn workspaces_are_open_where_a_running_job_holds_them() {
        let config = ClusterConfig {
            workspaces: vec![
                ClusterWorkspace {
                    id: "w-0000abcd".into(),
                    name: "crc".into(),
                    path: "/s/crc".into(),
                    created_ms: 0,
                },
                ClusterWorkspace {
                    id: "w-1111abcd".into(),
                    name: "atlas".into(),
                    path: "/s/atlas".into(),
                    created_ms: 0,
                },
                ClusterWorkspace {
                    id: "w-2222abcd".into(),
                    name: "paper".into(),
                    path: "/s/paper".into(),
                    created_ms: 0,
                },
            ],
            ..Default::default()
        };
        let mut waiting = record("j-0000bbbb", Some("78"), NOW);
        waiting.open = vec!["w-2222abcd".into()];
        let mut state = BrowseState {
            config: config.clone(),
            jobs: vec![
                JobFiles {
                    record: record("j-0000aaaa", Some("77"), NOW - 10_000_000),
                    host: Some(host("77")),
                    egress: None,
                    hosting: None,
                },
                JobFiles {
                    record: waiting,
                    host: None,
                    egress: None,
                    hosting: None,
                },
            ],
            manifests: [
                ("w-0000abcd".to_string(), manifest("77")),
                // A manifest a gone job left proves nothing.
                ("w-1111abcd".to_string(), manifest("12")),
            ]
            .into_iter()
            .collect(),
            last_open_ms: [("w-1111abcd".to_string(), 9)].into_iter().collect(),
        };
        let queue = [
            queue_row("77", "RUNNING", "n042", "1:00:00", "None"),
            queue_row("78", "PENDING", "", "4:00:00", "Priority"),
        ];
        let b = build(&config, &state, &queue, true, NOW);
        let by_id = |id: &str| b.workspaces.iter().find(|w| w.id == id).unwrap();
        assert_eq!(
            (
                by_id("w-0000abcd").state,
                by_id("w-0000abcd").job.as_deref()
            ),
            ("open", Some("j-0000aaaa"))
        );
        assert_eq!(by_id("w-1111abcd").state, "closed");
        assert_eq!(by_id("w-1111abcd").last_open_ms, Some(9));
        assert_eq!(
            (
                by_id("w-2222abcd").state,
                by_id("w-2222abcd").job.as_deref()
            ),
            ("queued", Some("j-0000bbbb"))
        );
        let e = &b.endpoints["w-0000abcd"];
        assert_eq!(
            (e.port, e.token.as_str(), e.job.as_str()),
            (42000, "ws-token", "j-0000aaaa")
        );
        assert_eq!(b.hosts["j-0000aaaa"].port, 41000);
        assert!(!b.hosts.contains_key("j-0000bbbb"));
        for (cancelled, slurm_state) in [(true, "RUNNING"), (false, "COMPLETING")] {
            state.jobs[0].record.stopped_by_user = cancelled;
            let queue = [queue_row("77", slurm_state, "n042", "1:00:00", "None")];
            let stopping = build(&config, &state, &queue, true, NOW);
            let workspace = &stopping.workspaces[0];
            assert_eq!(workspace.state, "open");
            assert_eq!(workspace.job.as_deref(), Some("j-0000aaaa"));
            assert!(workspace.closing);
            assert!(!workspace.opening);
            assert!(!stopping.endpoints.contains_key("w-0000abcd"));
            assert!(!stopping.hosts.contains_key("j-0000aaaa"));
        }
        // Both live endpoint records disappear on graceful shutdown. The
        // durable closing row still owns the workspace until Slurm is done.
        let previous_manifests = std::mem::take(&mut state.manifests);
        state.jobs[0].host = None;
        state.jobs[0].hosting = Some(chimaera_core::cluster::HostingRecord {
            workspaces: [("w-0000abcd".into(), HostedState::Closing)]
                .into_iter()
                .collect(),
        });
        for held in [
            HostedState::Starting,
            HostedState::Open,
            HostedState::Closing,
        ] {
            state.jobs[0]
                .hosting
                .as_mut()
                .unwrap()
                .workspaces
                .insert("w-0000abcd".into(), held);
            for (cancelled, slurm_state) in [(true, "RUNNING"), (false, "COMPLETING")] {
                state.jobs[0].record.stopped_by_user = cancelled;
                let queue = [queue_row("77", slurm_state, "n042", "1:00:00", "None")];
                let stopping = build(&config, &state, &queue, true, NOW);
                let workspace = &stopping.workspaces[0];
                assert_eq!(workspace.job.as_deref(), Some("j-0000aaaa"));
                assert!(workspace.closing);
                assert!(stopping.endpoints.is_empty());
                assert!(stopping.hosts.is_empty());
            }
        }
        // The final snapshot may not have landed yet (or job-host crashed).
        // A stale failed row must never reclaim a released workspace, even
        // if its old daemon manifest also remains on disk.
        for manifests in [Default::default(), previous_manifests] {
            state.manifests = manifests;
            state.jobs[0]
                .hosting
                .as_mut()
                .unwrap()
                .workspaces
                .insert("w-0000abcd".into(), HostedState::Failed);
            for (cancelled, slurm_state) in [(true, "RUNNING"), (false, "COMPLETING")] {
                state.jobs[0].record.stopped_by_user = cancelled;
                let queue = [queue_row("77", slurm_state, "n042", "1:00:00", "None")];
                let stopping = build(&config, &state, &queue, true, NOW);
                let workspace = &stopping.workspaces[0];
                assert_eq!(workspace.state, "closed");
                assert!(!workspace.closing);
                assert!(!workspace.opening);
                assert!(workspace.failed.is_some());
                assert!(stopping.endpoints.is_empty());
            }
        }
        let ended = build(&config, &state, &[], true, NOW);
        assert_eq!(ended.workspaces[0].state, "closed");
        assert!(ended.workspaces[0].job.is_none());
    }

    /// A running job's job-host says what it holds: a workspace it is
    /// opening, one closing, one that failed, and one closed there since its
    /// start (its start list no longer counts).
    #[test]
    fn a_running_jobs_own_record_says_what_it_holds() {
        let ws = |id: &str| ClusterWorkspace {
            id: id.into(),
            name: id.into(),
            path: format!("/s/{id}"),
            created_ms: 0,
        };
        let config = ClusterConfig {
            workspaces: vec![
                ws("w-0000000a"),
                ws("w-0000000b"),
                ws("w-0000000c"),
                ws("w-0000000d"),
                ws("w-0000000e"),
            ],
            ..Default::default()
        };
        let mut a = record("j-0000aaaa", Some("77"), NOW - 10_000_000);
        a.open = vec!["w-0000000a".into(), "w-0000000d".into()];
        let held = |pairs: &[(&str, HostedState)]| chimaera_core::cluster::HostingRecord {
            workspaces: pairs.iter().map(|(w, s)| (w.to_string(), *s)).collect(),
        };
        let state = BrowseState {
            config: config.clone(),
            jobs: vec![
                JobFiles {
                    record: a,
                    host: Some(host("77")),
                    egress: None,
                    hosting: Some(held(&[
                        ("w-0000000b", HostedState::Starting),
                        ("w-0000000c", HostedState::Closing),
                        ("w-0000000e", HostedState::Failed),
                    ])),
                },
                JobFiles {
                    record: record("j-0000bbbb", Some("78"), NOW - 10_000_000),
                    host: Some(host("78")),
                    egress: None,
                    hosting: Some(held(&[("w-0000000d", HostedState::Open)])),
                },
            ],
            manifests: [
                ("w-0000000c".to_string(), manifest("77")),
                // Failed in 77 once, open in 78 now.
                ("w-0000000d".to_string(), manifest("78")),
            ]
            .into_iter()
            .collect(),
            last_open_ms: Default::default(),
        };
        let queue = [
            queue_row("77", "RUNNING", "n1", "1:00:00", "None"),
            queue_row("78", "RUNNING", "n2", "1:00:00", "None"),
        ];
        let b = build(&config, &state, &queue, true, NOW);
        let by_id = |id: &str| b.workspaces.iter().find(|w| w.id == id).unwrap();
        // On 77's start list, closed there since.
        assert_eq!(by_id("w-0000000a").state, "closed");
        assert!(by_id("w-0000000a").job.is_none());
        let opening = by_id("w-0000000b");
        assert_eq!(
            (opening.state, opening.job.as_deref(), opening.opening),
            ("queued", Some("j-0000aaaa"), true)
        );
        let closing = by_id("w-0000000c");
        assert_eq!((closing.state, closing.closing), ("open", true));
        let moved = by_id("w-0000000d");
        assert_eq!(
            (moved.state, moved.job.as_deref()),
            ("open", Some("j-0000bbbb"))
        );
        let failed = by_id("w-0000000e");
        assert_eq!(
            (failed.state, failed.job.as_deref(), failed.failed.is_some()),
            ("closed", Some("j-0000aaaa"), true)
        );
        let json = serde_json::to_value(by_id("w-0000000a")).unwrap();
        assert!(json.get("opening").is_none() && json.get("closing").is_none());
    }

    /// The folder lags (a new manifest, a close, a failure); the running
    /// job's job-host says what it holds now, and that wins.
    #[test]
    fn a_running_jobs_job_host_overrides_a_lagging_folder() {
        let ws = |id: &str| ClusterWorkspace {
            id: id.into(),
            name: id.into(),
            path: format!("/s/{id}"),
            created_ms: 0,
        };
        let config = ClusterConfig {
            workspaces: ["a", "b", "c", "d", "e"]
                .iter()
                .map(|s| ws(&format!("w-0000000{s}")))
                .collect(),
            ..Default::default()
        };
        let files = |jid: &str, slurm: &str| JobFiles {
            record: record(jid, Some(slurm), NOW - 10_000_000),
            host: Some(host(slurm)),
            egress: None,
            hosting: None,
        };
        let state = BrowseState {
            config: config.clone(),
            jobs: vec![files("j-0000aaaa", "77"), files("j-0000bbbb", "78")],
            manifests: [
                ("w-0000000b".to_string(), manifest("77")),
                ("w-0000000e".to_string(), manifest("78")),
            ]
            .into_iter()
            .collect(),
            last_open_ms: Default::default(),
        };
        let queue = [
            queue_row("77", "RUNNING", "n1", "1:00:00", "None"),
            queue_row("78", "RUNNING", "n2", "1:00:00", "None"),
        ];
        let b = build(&config, &state, &queue, true, NOW);
        let mut ov = ClusterOverview {
            jobs: b.jobs,
            workspaces: b.workspaces,
            endpoints: b.endpoints,
            ..Default::default()
        };
        assert!(ov.endpoints.contains_key("w-0000000b"));
        let held =
            |id: &str, state: HostedState, port: Option<u16>, detail: &str| HostedWorkspace {
                id: id.into(),
                state,
                port,
                working: 2,
                token: port.map(|_| "tok".to_string()),
                build: port.map(|_| "abcdef1.124".to_string()),
                detail: detail.into(),
            };
        let status = JobHostStatus {
            job: "j-0000aaaa".into(),
            slurm_job_id: "77".into(),
            node: "n1".into(),
            workspaces: vec![
                held("w-0000000a", HostedState::Open, Some(45000), ""),
                held("w-0000000c", HostedState::Starting, None, ""),
                held("w-0000000d", HostedState::Failed, None, "address in use"),
            ],
        };
        apply_hosting(&mut ov, "j-0000aaaa", &status);
        let by_id = |id: &str| ov.workspaces.iter().find(|w| w.id == id).unwrap().clone();
        // Just opened: no manifest on the login node yet.
        let a = by_id("w-0000000a");
        assert_eq!(
            (a.state, a.job.as_deref(), a.working),
            ("open", Some("j-0000aaaa"), Some(2))
        );
        assert_eq!(ov.endpoints["w-0000000a"].port, 45000);
        // Closed since: its manifest still names 77.
        let b = by_id("w-0000000b");
        assert_eq!((b.state, b.job.as_deref()), ("closed", None));
        assert!(!ov.endpoints.contains_key("w-0000000b"));
        let c = by_id("w-0000000c");
        assert_eq!(
            (c.state, c.job.as_deref(), c.opening, c.working),
            ("queued", Some("j-0000aaaa"), true, None)
        );
        let d = by_id("w-0000000d");
        assert_eq!(
            (d.state, d.job.as_deref(), d.failed.as_deref()),
            ("closed", Some("j-0000aaaa"), Some("address in use"))
        );
        // Open in the other job: not this job-host's to say.
        let e = by_id("w-0000000e");
        assert_eq!((e.state, e.job.as_deref()), ("open", Some("j-0000bbbb")));
        assert!(ov.endpoints.contains_key("w-0000000e"));

        // A job that isn't running has nothing to say.
        let before = ov.clone();
        apply_hosting(&mut ov, "j-0000cccc", &status);
        assert_eq!(ov, before);
    }

    #[test]
    fn startup_reads_and_writes_the_environment_settings_shape() {
        let v = parse_startup(
            r#"{"host":{"text":"ml R"},"workspaces":{"w-0000abcd":{"text":"ml py"},"w-1111abcd":{"text":"  "},"bad":{"text":"x"}}}"#,
        );
        assert_eq!(v.cluster, "ml R");
        assert_eq!(v.workspaces.len(), 1);
        assert_eq!(v.workspaces["w-0000abcd"], "ml py");
        assert_eq!(parse_startup(""), StartupView::default());

        let mut f = serde_json::json!({"host": {"text": "old"}, "other": 1});
        set_scope(&mut f, None, "ml R");
        set_scope(&mut f, Some("w-0000abcd"), "ml py");
        assert_eq!(f["host"]["text"], "ml R");
        assert_eq!(f["workspaces"]["w-0000abcd"]["text"], "ml py");
        assert_eq!(f["other"], 1, "fields we don't own are kept");
        set_scope(&mut f, Some("w-0000abcd"), "");
        assert!(f.get("workspaces").is_none(), "an empty scope goes away");
        set_scope(&mut f, None, " ");
        assert!(f.get("host").is_none());
    }

    #[test]
    fn stable_receipts_require_fresh_begin_and_exact_positive_completion() {
        let nonce = "fresh_attempt";
        assert!(submission_receipt("===rc 0\n===end\n", nonce, true).is_none());
        assert!(
            submission_receipt("===rc 0\n===end\n===begin fresh_attempt\n", nonce, true).is_none()
        );
        assert!(submission_receipt("===begin fresh_attempt\n===end\n", nonce, true).is_none());
        assert!(submission_receipt(
            "===begin fresh_attempt\n===rc 0 extra\n===end\n",
            nonce,
            true
        )
        .is_none());
        assert!(submission_receipt(
            "===begin fresh_attempt\n===rc 0\n===end\n===end\n",
            nonce,
            true
        )
        .is_none());
        assert!(submission_receipt(
            "===begin fresh_attempt\n===rc 0\n===id bad\n===err\n===end\n",
            nonce,
            false
        )
        .is_none());
        let attached = submission_receipt(
            "===rc 9\n===end\n===begin fresh_attempt\n===rc 0\n===end\n",
            nonce,
            true,
        )
        .unwrap();
        assert_eq!(marker_arg(&attached, "rc"), Some("0"));
        let batch = submission_receipt(
            "noise\n===begin fresh_attempt\n===rc 0\n===id 12345\n===err\n===end\n",
            nonce,
            false,
        )
        .unwrap();
        assert_eq!(marker_arg(&batch, "id"), Some("12345"));
    }

    fn terminal_record() -> JobRecord {
        JobRecord {
            id: "j-1234abcd".into(),
            job_name: "chimaera-fixture~1234abcd".into(),
            slurm_job_id: Some("123".into()),
            ..Default::default()
        }
    }
    fn terminal_frame(accounting: &str, acct_rc: u8, controller: &str, control_rc: u8) -> Vec<u8> {
        format!("noise\n===begin fresh\n===sacct\n{accounting}\n===sacct_rc {acct_rc}\n===scontrol\n{controller}\n===scontrol_rc {control_rc}\n===uid 1000\n===end\n").into_bytes()
    }
    #[test]
    fn terminal_proof_requires_exact_name_id_uid_and_successful_fresh_complete_frame() {
        let record = terminal_record();
        let row = "123|chimaera-fixture~1234abcd|COMPLETED|1000";
        let valid = terminal_frame(row, 0, "", 1);
        let proof = terminal_receipt(&valid, "fresh", &record, Some("123")).unwrap();
        assert_eq!(
            (
                proof.job_id.as_str(),
                proof.slurm_job_id.as_str(),
                proof.state,
                proof.source
            ),
            ("j-1234abcd", "123", "COMPLETED", TerminalSource::Accounting)
        );
        for bad in [
            "124|chimaera-fixture~1234abcd|COMPLETED|1000",
            "123|chimaera-other~1234abcd|COMPLETED|1000",
            "123|chimaera-fixture~1234abcd|COMPLETED|1001",
            "123.batch|chimaera-fixture~1234abcd|COMPLETED|1000",
            "123|chimaera-fixture~1234abcd|COMPLETED+|1000",
            "123|chimaera-fixture~1234abcd|RUNNING|1000",
            "123|chimaera-fixture~1234abcd|ENDED|1000",
            "123|chimaera-fixture~1234abcd|CANCELLED_REQUEUED|1000",
            "123|chimaera-fixture~1234abcd|COMPLETED|1000|extra",
        ] {
            assert!(terminal_receipt(
                &terminal_frame(bad, 0, "", 1),
                "fresh",
                &record,
                Some("123")
            )
            .is_none());
        }
        assert!(terminal_receipt(
            &terminal_frame(row, 1, "", 1),
            "fresh",
            &record,
            Some("123")
        )
        .is_none());
        assert!(terminal_receipt(&valid, "different", &record, Some("123")).is_none());
        for damaged in [
            valid[..valid.len() - 8].to_vec(),
            [valid.clone(), b"===end\n".to_vec()].concat(),
            [valid.clone(), b"unexpected\n".to_vec()].concat(),
            vec![b'x'; 65537],
        ] {
            assert!(terminal_receipt(&damaged, "fresh", &record, Some("123")).is_none());
        }
        let mut wrong = record.clone();
        wrong.job_name = "chimaera-other~99999999".into();
        assert!(terminal_identity(&wrong, None).is_err());
        assert!(terminal_identity(&record, Some("124")).is_err());
    }
    #[test]
    fn terminal_controller_fallback_refuses_live_conflicting_and_unknown_observations() {
        let record = terminal_record();
        let control =
            "JobId=123 JobName=chimaera-fixture~1234abcd UserId=fixture(1000) JobState=CANCELLED";
        let proof = terminal_receipt(
            &terminal_frame("", 1, control, 0),
            "fresh",
            &record,
            Some("123"),
        )
        .unwrap();
        assert_eq!(
            (proof.state, proof.source),
            ("CANCELLED", TerminalSource::Controller)
        );
        for bad in [
            control.replace("JobId=123", "JobId=124"),
            control.replace("UserId=fixture(1000)", "UserId=fixture(1001)"),
            control.replace("CANCELLED", "RUNNING"),
            control.replace("CANCELLED", "UNKNOWN"),
            format!("{control} JobId=123"),
        ] {
            assert!(terminal_receipt(
                &terminal_frame("", 1, &bad, 0),
                "fresh",
                &record,
                Some("123")
            )
            .is_none());
        }
        let completed = "123|chimaera-fixture~1234abcd|COMPLETED|1000";
        for contradiction in [control.replace("CANCELLED", "RUNNING"), control.to_string()] {
            assert!(terminal_receipt(
                &terminal_frame(completed, 0, &contradiction, 0),
                "fresh",
                &record,
                Some("123")
            )
            .is_none());
        }
        assert!(
            terminal_receipt(&terminal_frame("", 0, "", 1), "fresh", &record, Some("123"))
                .is_none()
        );
        assert!(
            terminal_receipt(&terminal_frame("", 1, "", 1), "fresh", &record, Some("123"))
                .is_none()
        );
        assert!(terminal_receipt(
            &terminal_frame(
                "123|chimaera-fixture~1234abcd|CANCELLED by 1000|1000",
                0,
                "",
                1
            ),
            "fresh",
            &record,
            Some("123")
        )
        .is_some());
        assert!(terminal_receipt(&terminal_frame("123|chimaera-fixture~1234abcd|COMPLETED|1000\n124|chimaera-fixture~1234abcd|COMPLETED|1000",0,"",1),"fresh",&record,None).is_none());
    }
    #[test]
    fn terminal_accounting_window_is_bounded_by_submission_time_not_epoch_history() {
        let now = 1_900_000_000_000u64;
        assert_eq!(terminal_query_age(now - 5_000, now), Some(605));
        assert_eq!(terminal_query_age(now + 5_000, now), Some(600));
        assert!(terminal_query_age(0, now).is_none());
        assert!(terminal_query_age(now + 600_001, now).is_none());
        assert!(terminal_query_age(now - 31 * 24 * 3600 * 1000, now).is_none());
        let script = terminal_script("chimaera-fixture~1234abcd", None, Some(605), "fresh");
        assert!(script.contains("-S now-605seconds"));
        assert!(script.contains("-nPXD"));
        assert!(!script.contains("1970"));
        let record = terminal_record();
        let row = "123|chimaera-fixture~1234abcd|COMPLETED|1000";
        assert!(terminal_receipt(
            &terminal_frame(&format!("{row}\n{row}"), 0, "", 1),
            "fresh",
            &record,
            Some("123")
        )
        .is_none());
    }
    #[test]
    fn terminal_probe_real_shell_retains_status_and_controller_fallback_without_queue_guessing() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = SubmissionFixture::new("exit 0");
        let record = terminal_record();
        for (accounting,controller,source) in [
            ("printf '123|chimaera-fixture~1234abcd|COMPLETED|%s\\n' \"$(id -u)\"","exit 1",Some(TerminalSource::Accounting)),
            ("exit 1","printf 'JobId=123 JobName=chimaera-fixture~1234abcd UserId=fixture(%s) JobState=TIMEOUT\\n' \"$(id -u)\"",Some(TerminalSource::Controller)),
            ("exit 1","exit 1",None),
            ("printf '123|chimaera-fixture~1234abcd|RUNNING|%s\\n' \"$(id -u)\"","exit 1",None),
        ] {
            for (name,body) in [("sacct",accounting),("scontrol",controller)] {
                let path=fixture.0.join("bin").join(name);std::fs::write(&path,format!("#!/bin/sh\n{body}\n")).unwrap();std::fs::set_permissions(path,std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let output=fixture.run(&terminal_script(&record.job_name,Some("123"),Some(600),"fresh"));
            assert!(output.status.success());
            assert_eq!(terminal_receipt(&output.stdout,"fresh",&record,Some("123")).map(|p|p.source),source);
        }
    }

    #[test]
    fn terminal_old_or_missing_submission_still_allows_exact_controller_proof() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = SubmissionFixture::new("exit 0");
        let control = fixture.0.join("bin/scontrol");
        std::fs::write(&control,"#!/bin/sh\nprintf 'JobId=123 JobName=chimaera-fixture~1234abcd UserId=fixture(%s) JobState=COMPLETED\\n' \"$(id -u)\"\n").unwrap();
        std::fs::set_permissions(control, std::fs::Permissions::from_mode(0o700)).unwrap();
        let record = terminal_record();
        let now = 1_900_000_000_000u64;
        for submitted in [0, now - 32 * 24 * 3600 * 1000, now + 600_001] {
            let age = terminal_query_age(submitted, now);
            assert!(age.is_none());
            let script = terminal_script(&record.job_name, Some("123"), age, "fresh");
            assert!(!script.contains("sacct -"));
            let output = fixture.run(&script);
            assert!(output.status.success());
            let evidence = terminal_receipt(&output.stdout, "fresh", &record, Some("123")).unwrap();
            assert_eq!(
                (evidence.state, evidence.source),
                ("COMPLETED", TerminalSource::Controller)
            );
        }
    }

    struct SubmissionFixture(std::path::PathBuf);
    impl SubmissionFixture {
        fn new(sbatch: &str) -> Self {
            Self::with_preflight(sbatch, "exit 0")
        }
        fn with_preflight(sbatch: &str, preflight: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = std::env::temp_dir().join(format!(
                "chimaera-submit-{}",
                chimaera_core::generate_token()
            ));
            std::fs::create_dir_all(root.join("bin")).unwrap();
            let path = root.join("bin/sbatch");
            std::fs::write(
                &path,
                format!("#!/bin/sh\nif [ \"$1\" = --test-only ]; then\n printf 'called\\n' >> \"$HOME/preflight-calls\"\n {preflight}\nfi\nprintf 'called\\n' >> \"$HOME/calls\"\n{sbatch}\n"),
            )
            .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }
        fn run(&self, script: &str) -> std::process::Output {
            std::process::Command::new("sh")
                .args(["-c", script])
                .env("HOME", &self.0)
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin", self.0.join("bin").display()),
                )
                .output()
                .unwrap()
        }
        fn submit_script(&self) -> String {
            let mut script = String::from("umask 077\n");
            script.push_str(&submission_claim("$HOME/cluster/j/j-1234abcd", true));
            script.push_str(&write_file_lines(
                "$D/job.pending",
                "{\"slurm_job_id\":\"__CHIMAERA_JOB_ID__\"}",
            ));
            script.push_str(&write_file_lines("$D/job.sh", "exit 0"));
            script.push_str(&submission_lines("--parsable", true));
            script
        }
        fn calls(&self) -> usize {
            std::fs::read_to_string(self.0.join("calls")).map_or(0, |s| s.lines().count())
        }
    }
    impl Drop for SubmissionFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An ordinary start whose record cannot be written (the folder is full
    /// or read-only) still reports the job it queued, and never removes the
    /// folder holding that job's script, as on main.
    #[test]
    fn ordinary_submission_keeps_a_queued_job_when_its_record_fails() {
        let f =
            SubmissionFixture::new("chmod 500 \"$HOME/cluster/j/j-1234abcd\"; printf '12345\\n'");
        let mut script = String::from("umask 077\n");
        script.push_str(&submission_claim("$HOME/cluster/j/j-1234abcd", false));
        script.push_str(&write_file_lines("$D/job.pending", "{}"));
        script.push_str(&write_file_lines("$D/job.sh", "exit 0"));
        script.push_str(&submission_lines("--parsable", false));
        let out = f.run(&script);
        let dir = f.0.join("cluster/j/j-1234abcd");
        let restore = || {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let kept = dir.join("job.sh").is_file();
        restore();
        assert!(
            stdout.contains("===rc 0") && stdout.contains("===id 12345"),
            "{stdout}"
        );
        assert!(kept, "the queued job's script stays");
        assert!(!dir.join("job.json").exists());
    }
    #[test]
    fn stable_submission_claim_prevents_duplicate_after_lost_reply() {
        let f = SubmissionFixture::new("printf '12345;cluster\\n'");
        let script = f.submit_script();
        let first = f.run(&script);
        assert!(first.status.success());
        assert!(String::from_utf8_lossy(&first.stdout).contains("===id 12345"));
        let record = std::fs::read_to_string(f.0.join("cluster/j/j-1234abcd/job.json")).unwrap();
        assert!(record.contains("12345"));
        // Forget the first reply; the remote claim remains and retry makes no effect.
        let retry = f.run(&script);
        assert!(String::from_utf8_lossy(&retry.stdout).contains("===rc 98"));
        assert_eq!(f.calls(), 1);
        assert_eq!(
            std::fs::read_to_string(f.0.join("cluster/j/j-1234abcd/job.json")).unwrap(),
            record
        );
    }
    #[test]
    fn stable_submission_preserves_refused_and_unparseable_evidence() {
        for (sbatch, rc) in [
            ("printf 'invalid partition\\n' >&2; exit 1", "===rc 1"),
            ("printf 'invalid id\\n'", "===rc 97"),
        ] {
            let f = SubmissionFixture::new(sbatch);
            let script = f.submit_script();
            let first = f.run(&script);
            assert!(
                String::from_utf8_lossy(&first.stdout).contains(rc),
                "{}",
                String::from_utf8_lossy(&first.stdout)
            );
            assert!(f.0.join("cluster/j/j-1234abcd/job.pending").is_file());
            f.run(&script);
            assert_eq!(f.calls(), 1);
        }
    }
    #[test]
    fn stable_refusal_requires_nonzero_empty_id_frame_and_never_clears_claim() {
        for (command, positive) in [
            ("printf 'sbatch: error: Batch job submission failed: Socket timed out on send/recv operation\n' >&2; exit 1", false),
            ("printf 'sbatch: error: Batch job submission failed: Message receive failure\n' >&2; exit 1", false),
            ("printf 'account required but controller unavailable\n' >&2; exit 1", false),
            ("printf 'Batch jobs not allowed here\n' >&2; exit 1", false),
            ("printf 'sbatch: error: Batch job submission failed: Invalid qos specification\n' >&2; exit 1", true),
            ("printf 'sbatch: error: Batch job submission failed: Invalid account or account/partition combination specified\n' >&2; exit 1", true),
            ("printf 'sbatch: error: Batch job submission failed: Invalid feature specification\n' >&2; exit 1", true),
            ("printf 'sbatch: error: Batch job submission failed: Invalid qos specification\nsbatch: error: Message receive failure\n' >&2; exit 1", false),
            ("printf '12345\n'; printf 'sbatch: error: Batch job submission failed: Invalid qos specification\n' >&2; exit 1", false),
            (
                "printf '12345;cluster\n'; printf 'post-submit error\n' >&2; exit 1",
                false,
            ),
            ("printf 'unrecognized scheduler output\n'; exit 1", false),
            ("printf '12345\n'", false),
        ] {
            let fixture = SubmissionFixture::new(command);
            let script = format!(
                "printf '===begin refusal_probe\n'\n{}",
                fixture.submit_script()
            );
            let output = fixture.run(&script);
            assert!(output.status.success());
            let sections = submission_receipt(
                &String::from_utf8_lossy(&output.stdout),
                "refusal_probe",
                false,
            )
            .unwrap();
            assert_eq!(stable_batch_non_submission(&sections), positive);
            // Claim survives both the positive refusal and ambiguous ID/error.
            // A later retry can't turn old evidence into another submission.
            let retry = fixture.run(&script);
            let sections = submission_receipt(
                &String::from_utf8_lossy(&retry.stdout),
                "refusal_probe",
                false,
            )
            .unwrap();
            assert!(!stable_batch_non_submission(&sections));
            assert_eq!(fixture.calls(), 1);
        }
        for frame in [
            "===begin refusal_probe\n===rc 256\n===id\n===err\n===end\n",
            "===begin refusal_probe\n===rc 137\n===id\n===err\n===end\n",
            "===begin refusal_probe\n===rc 126\n===id\n===err\n===end\n",
            "===begin refusal_probe\n===rc 127\n===id\n===err\n===end\n",
            "===begin refusal_probe\n===rc 96\n===id\n===err\n===end\n",
            "===begin refusal_probe\n===rc 97\n===id\n===err\n===end\n",
        ] {
            let sections = submission_receipt(frame, "refusal_probe", false).unwrap();
            assert!(!stable_batch_non_submission(&sections));
        }
    }
    #[test]
    fn stable_preflight_refusal_proves_zero_real_submission_and_never_replays() {
        for (preflight, positive, kind) in [
            (
                "printf 'sbatch: error: Batch jobs not allowed; use interactive\n' >&2; exit 1",
                true,
                Refusal::BatchNotAllowed,
            ),
            (
                "printf 'sbatch: error: Message receive failure\n' >&2; exit 1",
                true,
                Refusal::Other,
            ),
            (
                "printf '12345\n'; printf 'site failure\n' >&2; exit 1",
                false,
                Refusal::Other,
            ),
        ] {
            let fixture = SubmissionFixture::with_preflight("printf '12345\n'", preflight);
            let script = format!(
                "printf '===begin preflight_probe\n'\n{}",
                fixture.submit_script()
            );
            let result = fixture.run(&script);
            assert!(result.status.success());
            let output = String::from_utf8_lossy(&result.stdout);
            let sections = submission_receipt(&output, "preflight_probe", false).unwrap();
            assert!(sections[0].0.starts_with("preflight_rc "));
            assert_eq!(stable_batch_non_submission(&sections), positive);
            assert_eq!(classify_refusal(section(&sections, "err").unwrap()), kind);
            assert_eq!(fixture.calls(), 0);
            assert_eq!(
                std::fs::read_to_string(fixture.0.join("preflight-calls"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
            assert!(submission_receipt(&output, "stale_nonce", false).is_none());
            assert!(
                submission_receipt(&output.replace("===end", ""), "preflight_probe", false)
                    .is_none()
            );
            assert!(submission_receipt(&output, "preflight_probe", true).is_none());
            let retry = fixture.run(&script);
            let retry = submission_receipt(
                &String::from_utf8_lossy(&retry.stdout),
                "preflight_probe",
                false,
            )
            .unwrap();
            assert!(!stable_batch_non_submission(&retry));
            assert_eq!(fixture.calls(), 0);
            assert_eq!(
                std::fs::read_to_string(fixture.0.join("preflight-calls"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
        }
    }
    #[test]
    fn stable_preflight_stdout_cannot_hide_a_nonconforming_submission_wrapper() {
        let fixture =
            SubmissionFixture::with_preflight("printf '54321\n'", "printf '12345\n'; exit 0");
        let script = format!(
            "printf '===begin preflight_probe\n'\n{}",
            fixture.submit_script()
        );
        let result = fixture.run(&script);
        let output = String::from_utf8_lossy(&result.stdout);
        assert!(output.contains("===preflight_rc 0"));
        assert!(submission_receipt(&output, "preflight_probe", false).is_none());
        assert_eq!(fixture.calls(), 0);
        fixture.run(&script);
        assert_eq!(fixture.calls(), 0);
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("preflight-calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
    #[test]
    fn stable_preflight_uses_same_arguments_then_one_actual_submission() {
        let fixture = SubmissionFixture::with_preflight(
            "printf '%s\n' \"$*\" >> \"$HOME/args\"; printf '12345\n'",
            "printf '%s\n' \"$*\" >> \"$HOME/args\"; exit 0",
        );
        let script = format!(
            "printf '===begin preflight_probe\n'\n{}",
            fixture.submit_script()
        );
        let result = fixture.run(&script);
        let sections = submission_receipt(
            &String::from_utf8_lossy(&result.stdout),
            "preflight_probe",
            false,
        )
        .unwrap();
        assert_eq!(marker_arg(&sections, "rc"), Some("0"));
        assert_eq!(marker_arg(&sections, "id"), Some("12345"));
        let args = std::fs::read_to_string(fixture.0.join("args")).unwrap();
        let args: Vec<_> = args.lines().collect();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].strip_prefix("--test-only "), Some(args[1]));
        assert_eq!(fixture.calls(), 1);
        fixture.run(&script);
        assert_eq!(fixture.calls(), 1);
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("args"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        for frame in [
            "===begin preflight_probe\n===preflight_rc 0\n===id\n===err\n===end\n",
            "===begin preflight_probe\n===preflight_rc 256\n===id\n===err\n===end\n",
        ] {
            assert!(submission_receipt(frame, "preflight_probe", false).is_none());
        }
    }
    #[test]
    fn stable_submission_partial_prepare_never_resubmits() {
        let f = SubmissionFixture::new("printf '12345\\n'");
        assert!(f
            .run(&submission_claim("$HOME/cluster/j/j-1234abcd", true))
            .status
            .success());
        let retry = f.run(&f.submit_script());
        assert!(String::from_utf8_lossy(&retry.stdout).contains("===rc 98"));
        assert_eq!(f.calls(), 0);
    }
    #[test]
    fn attached_remote_claim_allows_only_one_effect_even_after_exit() {
        let f = SubmissionFixture::new("exit 0");
        let dir = "$HOME/cluster/j/j-1234abcd";
        let prepare = format!(
            "{}{}",
            submission_claim(dir, true),
            write_file_lines("$D/job.json", "{}")
        );
        assert!(f.run(&prepare).status.success());
        let script = format!(
            "{}printf 'called\\n' >> \"$HOME/calls\"",
            attached_claim(dir)
        );
        assert!(f.run(&script).status.success());
        assert_eq!(f.run(&script).status.code(), Some(98));
        assert_eq!(f.calls(), 1);
    }

    #[test]
    fn sections_split_on_markers_and_drop_rc_noise() {
        let out = "Welcome to the cluster!\n===now 1700000000\n===squeue 0\n1|p|RUNNING|1:00|n1|1|1G|0:01|/w|None|x\n===config 1234\n{\"version\":1}\n===end\n";
        let secs = sections(out);
        assert_eq!(marker_arg(&secs, "now"), Some("1700000000"));
        assert_eq!(marker_arg(&secs, "squeue"), Some("0"));
        assert!(section(&secs, "squeue").unwrap().starts_with("1|p|"));
        assert_eq!(marker_arg(&secs, "config"), Some("1234"));
        assert!(section(&secs, "end").is_some());
        assert!(!secs.iter().any(|(k, _)| k.contains("Welcome")));
    }

    #[test]
    fn discovery_sections_become_facts() {
        let out = "===version\nslurm 18.08.9\n===sinfo\nbatch*|up|10|7-00:00:00|20|128000|(null)\nlab|up|2|1-00:00:00|8|64000|(null)\n===assoc\nacct1|batch|normal\n===defacct\nacct1\n===policy\nPartitionName=batch AllowAccounts=ALL\nPartitionName=lab AllowAccounts=other\n===groups\nusers lab\n===end\n";
        let f = facts_from_sections(&sections(out));
        assert_eq!(f.gpu_flag, GpuFlag::Gres);
        assert_eq!(f.partitions.len(), 1);
        assert_eq!(f.partitions[0].name, "batch");
        assert_eq!(f.accounts, vec!["acct1"]);
        assert_eq!(f.default_account.as_deref(), Some("acct1"));
    }

    #[test]
    fn typed_paths_expand_but_never_execute() {
        assert_eq!(plain_path_input("~/x").unwrap(), "$HOME/x");
        assert_eq!(plain_path_input("$SCRATCH/crc").unwrap(), "$SCRATCH/crc");
        assert_eq!(
            plain_path_input("/data/lab data").unwrap(),
            "/data/lab data"
        );
        for bad in ["", "$(rm -rf ~)", "a;b", "`x`", "a|b", "a&b", "x\"y", "x'y"] {
            assert!(plain_path_input(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn slurm_timestamps_respect_the_cluster_zone() {
        assert_eq!(
            parse_slurm_timestamp("1970-01-01T01:00:00", "+0100"),
            None,
            "the epoch itself is not a start"
        );
        assert_eq!(
            parse_slurm_timestamp("2026-07-15T12:34:00", "+0000"),
            Some(1_784_118_840_000)
        );
        assert_eq!(
            parse_slurm_timestamp("2026-07-15T05:34:00", "-0700"),
            Some(1_784_118_840_000)
        );
        assert_eq!(parse_slurm_timestamp("N/A", "+0000"), None);
    }

    #[test]
    fn attached_output_lines_lose_colors_and_carriage_returns() {
        assert_eq!(
            plain_line("\u{1b}[31msrun: error: Invalid partition\u{1b}[0m\r"),
            "srun: error: Invalid partition"
        );
        assert_eq!(plain_line("  \r"), "");
    }

    #[tokio::test]
    async fn attached_output_drains_oversized_invalid_and_unterminated_lines() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(1024);
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let output = tail.clone();
        let drain = tokio::spawn(drain_attached(reader, output, false));
        let producer = tokio::spawn(async move {
            // A megabyte without a newline must not stall the child or grow
            // the current-line buffer; invalid bytes must not stop draining.
            let block = [b'x'; 8192];
            for _ in 0..128 {
                writer.write_all(&block).await.unwrap();
            }
            writer.write_all(b"\n\xff\xfe invalid\n").await.unwrap();
            for _ in 0..9 {
                writer.write_all(b"old\n").await.unwrap();
            }
            writer
                .write_all(b"\x1b[31mfinal valid\x1b[0m\r")
                .await
                .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(3), producer)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), drain)
            .await
            .unwrap()
            .unwrap();
        let tail = tail.lock().unwrap();
        assert_eq!(tail.len(), 8);
        assert_eq!(tail.back().unwrap(), "final valid");
        assert!(tail.iter().all(|line| line.chars().count() <= 300));
    }

    #[tokio::test]
    async fn attached_output_retains_a_bounded_prefix_and_reads_after_invalid_utf8() {
        let mut bytes = vec![b'x'; ATTACHED_LINE_BYTES * 32];
        bytes.extend_from_slice(b"\n\xff malformed\nnext line");
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        drain_attached(bytes.as_slice(), tail.clone(), false).await;
        let tail = tail.lock().unwrap();
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0], "x".repeat(300));
        assert_eq!(tail[1], "\u{fffd} malformed");
        assert_eq!(tail[2], "next line");
    }

    #[tokio::test]
    async fn attached_output_keeps_the_last_redraw_of_a_carriage_return_line() {
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        drain_attached(
            &b"srun: job 7 queued\r\n 10%\r 50%\r100%\r\nsrun: error: x\r\n"[..],
            tail.clone(),
            false,
        )
        .await;
        let tail = tail.lock().unwrap();
        assert_eq!(
            tail.iter().cloned().collect::<Vec<_>>(),
            ["srun: job 7 queued", "10%", "50%", "100%", "srun: error: x"]
        );
    }

    #[test]
    fn write_file_lines_never_lets_content_end_the_heredoc() {
        let lines = write_file_lines("$D/x", "EOF\nCHIMAERA_EOF\n$HOME `x` 'y'");
        let delim = lines
            .lines()
            .next()
            .unwrap()
            .split("<<'")
            .nth(1)
            .unwrap()
            .trim_end_matches('\'');
        assert!(delim.starts_with("CHIMAERA_EOF_") && delim.len() > 20);
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "D=$(mktemp -d)\n{lines}cat \"$D/x\"; rm -rf \"$D\"\n"
            ))
            .output();
        if let Ok(out) = out {
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "EOF\nCHIMAERA_EOF\n$HOME `x` 'y'\n",
                "the quoted heredoc keeps content literal"
            );
        }
    }

    /// The scripts that run on the login node parse as POSIX sh.
    #[test]
    fn scancel_lines_are_valid_sh() {
        let out = std::process::Command::new("sh")
            .args(["-n", "-c", &scancel_lines("'123'")])
            .output();
        if let Ok(out) = out {
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
