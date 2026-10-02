//! Cluster jobs and workspaces, driven from the client. On a cluster (a host
//! whose login shell reaches a batch scheduler) nothing of ours keeps running
//! on the login node: the app and the CLI run short commands over the
//! existing ControlMaster — `squeue`, `sbatch`, `scancel`, and the read-only
//! `chimaera browse` — and Chimaera runs inside Slurm **jobs** the user
//! starts. Workspaces open inside a job (`chimaera job-host`, reached over
//! the same `ssh -L` as the workspaces themselves); a workspace keeps its
//! chats in its own folder, so it moves between jobs.
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
}

/// Where an open workspace's chimaera listens — kept by the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub job: String,
    pub slurm_job_id: String,
    pub node: String,
    pub port: u16,
    pub token: String,
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
                        && !j.stopping
                        && j.slurm_job_id.is_some()
                        && j.slurm_job_id == m.slurm_job_id
                        && held_in(&j.id) != Some(HostedState::Failed)
                }) {
                    v.state = "open";
                    v.job = Some(job.id.clone());
                    v.closing = held_in(&job.id) == Some(HostedState::Closing);
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
                        },
                    );
                    return v;
                }
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
/// way).
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
    let jid = new_job_id();
    let name = req
        .name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| n.chars().take(60).collect::<String>())
        .unwrap_or_else(|| job_display_name(&spec));
    let token = &chimaera_core::generate_token()[..6];
    let job_name = slurm::job_name(&name, token);
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
    s.push_str(&format!("D=\"{dir}\"\nmkdir -p \"$D\" || exit 3\n"));
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
        s.push_str(&format!(
            "out=$(sbatch {} \"$D/job.sh\" 2>\"$D/.sbatch.err\"); rc=$?\n\
             id=${{out%%;*}}\n\
             case \"$id\" in ''|*[!0-9_]*) [ \"$rc\" -eq 0 ] && rc=97 ;; esac\n\
             if [ \"$rc\" -eq 0 ]; then\n\
             \x20 sed \"s/__CHIMAERA_JOB_ID__/$id/\" \"$D/job.pending\" > \"$D/job.json.tmp\" && mv -f \"$D/job.json.tmp\" \"$D/job.json\"\n\
             fi\n\
             rm -f \"$D/job.pending\"\n\
             printf '===rc %s\\n===id %s\\n===err\\n' \"$rc\" \"$id\"; cat \"$D/.sbatch.err\" 2>/dev/null; rm -f \"$D/.sbatch.err\"\n\
             [ \"$rc\" -eq 0 ] || rm -rf \"$D\"\n\
             printf '===end\\n'\n",
            args.join(" ")
        ));
    }
    let out = run_script(host, &s, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    if section(&secs, "end").is_none() {
        bail!(
            "starting a job on {host} failed: {}",
            super::ssh_failure_line(&out.stderr, &out.status)
        );
    }
    invalidate_queue(host);
    if req.attached {
        return Ok(StartOutcome::Attached { job: jid, job_name });
    }
    let rc = marker_arg(&secs, "rc").unwrap_or("1");
    if rc != "0" {
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
    anyhow::ensure!(valid_job_id(jid), "unknown job");
    let args: Vec<String> = spec
        .srun_args(job_name, gpu_flag)
        .iter()
        .map(|a| sh_quote(a))
        .collect();
    // One line, so each statement needs its `;`: the PATH line ends in a
    // newline, and a space there once made it `export PATH exec srun …`.
    let script = format!(
        "unset SLURM_JOB_ID SLURM_JOBID; {}exec srun {} /bin/bash \"{}/job.sh\"",
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
    use tokio::io::{AsyncBufReadExt, BufReader};
    const KEEP: usize = 8;
    let tail = Arc::new(Mutex::new(VecDeque::with_capacity(KEEP)));
    let pipes: [Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>; 2] = [
        child.stdout.take().map(|p| Box::new(p) as _),
        child.stderr.take().map(|p| Box::new(p) as _),
    ];
    for pipe in pipes.into_iter().flatten() {
        let tail = tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = plain_line(&line);
                if line.is_empty() || line.starts_with("Shared connection to") {
                    continue;
                }
                if echo {
                    eprintln!("{line}");
                }
                let mut t = tail.lock().unwrap_or_else(|p| p.into_inner());
                if t.len() == KEEP {
                    t.pop_front();
                }
                t.push_back(line.chars().take(300).collect());
            }
        });
    }
    tail
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
        }
    }

    const NOW: u64 = 1_000_000_000;

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
        let state = BrowseState {
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
