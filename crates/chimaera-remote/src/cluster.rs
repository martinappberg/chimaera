//! Cluster workspaces, driven from the client. On a cluster (a host whose
//! login shell reaches a batch scheduler) nothing of ours keeps running on
//! the login node: the app and the CLI run short commands over the existing
//! ControlMaster — `squeue`, `sbatch`, `scancel`, reading the cluster folder
//! (`chimaera_core::cluster`) — and each workspace's chimaera runs inside its
//! own Slurm job.
//!
//! Every exec is one bounded `ssh host sh -s` with the script on STDIN, never
//! in argv: startup commands may carry secrets (an `export API_KEY=…` on an
//! egress-limited cluster), and argv is visible to every user of a shared
//! login node. Scripts are POSIX sh; the login shell only ever runs `sh -s`.
//!
//! Politeness is part of the contract: the queue is asked at most once a
//! minute per host (`SQUEUE_FLOOR`), everything else is a file read.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use chimaera_core::cluster::{
    compose_startup, job_script, valid_workspace_id, AgentRules, ClusterConfig, ClusterWorkspace,
    Ended, JobScript, LaunchRecord, Setup, WorkspaceSeed,
};
use chimaera_core::slurm::{
    self, classify_refusal, clean_tool_stderr, is_chimaera_job, parse_duration, sh_quote, GpuFlag,
    Job, LaunchSpec, PartitionChoice, Refusal, Scheduler,
};
use chimaera_core::Manifest;
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
/// A fetched file's ceiling (the file peek opens it in a preview).
pub const FETCH_MAX_BYTES: u64 = 64 * 1024 * 1024;
const LIST_MAX_ENTRIES: usize = 2000;

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
}

fn ws_dir(home: RemoteHome, id: &str) -> String {
    format!("{}/w/{id}", home.cluster_dir())
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

// --- Overview -----------------------------------------------------------------

/// One workspace as the cluster page shows it. No port or token: those stay
/// in the client process ([`Endpoint`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct WorkspaceView {
    pub id: String,
    pub name: String,
    pub path: String,
    /// `running` | `starting` | `waiting` | `stopped`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
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
    /// How the last job ended (Slurm's terminal state), when stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    pub stopped_by_user: bool,
    /// The job's node reaches the internet (agents can work), when probed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress: Option<bool>,
    /// Never started.
    pub fresh: bool,
    pub startup: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_spec: Option<LaunchSpec>,
}

/// Where a running workspace's chimaera listens — kept by the client, never
/// sent to a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub job_id: String,
    pub node: String,
    pub port: u16,
    pub token: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OtherJobs {
    pub running: usize,
    pub waiting: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ClusterOverview {
    pub scheduler: Scheduler,
    /// The login node these commands ran on.
    pub login_node: String,
    pub now_ms: u64,
    pub workspaces: Vec<WorkspaceView>,
    pub other_jobs: OtherJobs,
    /// The queue couldn't be read this round; states carry the last good
    /// read forward.
    pub degraded: bool,
    /// When the queue was last actually asked (epoch ms, client clock).
    pub queue_at_ms: u64,
    pub config: ClusterConfig,
    /// cksum of `cluster.json` as read — the optimistic-concurrency token
    /// for a write.
    #[serde(skip)]
    pub config_sum: String,
    #[serde(skip)]
    pub endpoints: HashMap<String, Endpoint>,
    #[serde(skip)]
    pub records: HashMap<String, LaunchRecord>,
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
/// cached read is older than [`SQUEUE_FLOOR`]), the cluster folder's config,
/// and each workspace's launch record, manifest and egress probe.
pub async fn overview(host: &str, home: RemoteHome) -> anyhow::Result<ClusterOverview> {
    let lock = flight(host);
    let _one = lock.lock().await;
    let cached = cached_queue(host);
    let ask_queue = cached
        .as_ref()
        .is_none_or(|c| c.at.elapsed() >= SQUEUE_FLOOR);
    let c = home.cluster_dir();
    let mut script = String::from("umask 077\n");
    script.push_str(&path_line(host));
    script.push_str(&format!("C=\"{c}\"\n"));
    script.push_str(
        "printf '===now %s\\n' \"$(date +%s)\"\nprintf '===node %s\\n' \"$(uname -n)\"\n",
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
         for d in \"$C\"/w/w-*; do\n\
         \x20 [ -d \"$d\" ] || continue\n\
         \x20 printf '===launch %s\\n' \"${d##*/}\"; cat \"$d/launch.json\" 2>/dev/null; printf '\\n'\n\
         \x20 printf '===manifest %s\\n' \"${d##*/}\"; cat \"$d/data/manifest.json\" 2>/dev/null; printf '\\n'\n\
         \x20 printf '===caps %s\\n' \"${d##*/}\"; cat \"$d/caps.json\" 2>/dev/null; printf '\\n'\n\
         done\n\
         printf '===end\\n'\n",
    );
    let out = run_script(host, &script, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    if section(&secs, "end").is_none() {
        bail!(
            "reading {host}'s workspaces failed: {}",
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

    let mut records = HashMap::new();
    let mut manifests = HashMap::new();
    let mut caps = HashMap::new();
    for (k, v) in &secs {
        let mut w = k.split_whitespace();
        let (Some(kind), Some(id)) = (w.next(), w.next()) else {
            continue;
        };
        if !valid_workspace_id(id) {
            continue;
        }
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        match kind {
            "launch" => {
                if let Ok(r) = serde_json::from_str::<LaunchRecord>(v) {
                    records.insert(id.to_string(), r);
                }
            }
            "manifest" => {
                if let Ok(m) = serde_json::from_str::<Manifest>(v) {
                    manifests.insert(id.to_string(), m);
                }
            }
            "caps" => {
                if let Ok(c) = serde_json::from_str::<serde_json::Value>(v) {
                    if let Some(e) = c.get("egress").and_then(|e| e.as_bool()) {
                        caps.insert(id.to_string(), e);
                    }
                }
            }
            _ => {}
        }
    }

    let jobs = queue.as_ref().map(|q| q.jobs.as_slice()).unwrap_or(&[]);
    let mut endpoints = HashMap::new();
    let workspaces = config
        .workspaces
        .iter()
        .map(|ws| {
            let (view, endpoint) = workspace_view(
                ws,
                records.get(&ws.id),
                manifests.get(&ws.id),
                caps.get(&ws.id).copied(),
                jobs,
                queue.is_some(),
                remote_now_ms,
            );
            if let Some(e) = endpoint {
                endpoints.insert(ws.id.clone(), e);
            }
            view
        })
        .collect();
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
        now_ms: remote_now_ms,
        workspaces,
        other_jobs: other,
        degraded,
        queue_at_ms: queue.as_ref().map(|q| q.at_ms).unwrap_or(0),
        config,
        config_sum,
        endpoints,
        records,
    })
}

/// One workspace's state from its record, its daemon's manifest and the
/// queue — pure, so every transition is unit-tested.
fn workspace_view(
    ws: &ClusterWorkspace,
    record: Option<&LaunchRecord>,
    manifest: Option<&Manifest>,
    egress: Option<bool>,
    jobs: &[Job],
    queue_known: bool,
    now_ms: u64,
) -> (WorkspaceView, Option<Endpoint>) {
    let mut v = WorkspaceView {
        id: ws.id.clone(),
        name: ws.name.clone(),
        path: ws.path.clone(),
        state: "stopped",
        startup: ws.startup.clone(),
        last_spec: ws.last_spec.clone(),
        egress,
        ..Default::default()
    };
    let Some(record) = record else {
        v.fresh = true;
        return (v, None);
    };
    v.attached = record.attached;
    v.job_id = record.job_id.clone();
    v.gpus = record.spec.gpus;
    v.stopped_by_user = record.stopped_by_user;
    v.partition = record.spec.partition.clone().unwrap_or_default();
    let row = jobs.iter().find(|j| match &record.job_id {
        Some(id) => &j.id == id,
        None => j.name == record.job_name,
    });
    match row {
        Some(job) if slurm::is_live_state(&job.state) => {
            v.job_id = Some(job.id.clone());
            v.partition = job.partition.clone();
            v.cpus = job.cpus.clone();
            v.mem = job.mem.clone();
            v.ends_at_ms =
                parse_duration(&job.time_left).map(|left| now_ms + left.as_millis() as u64);
            if job.state.starts_with("PENDING") || job.state.starts_with("CONFIGURING") {
                v.state = "waiting";
                if job.reason != "Priority" {
                    v.reason = job.reason.clone();
                }
                return (v, None);
            }
            v.node = job.nodes.clone();
            // Ready only when THIS job's daemon wrote the manifest: an older
            // job's leftover record must never be mistaken for the new one.
            match manifest.filter(|m| m.slurm_job_id.as_deref() == Some(job.id.as_str())) {
                Some(m) => {
                    v.state = "running";
                    if !m.hostname.is_empty() {
                        v.node = m.hostname.clone();
                    }
                    let endpoint = Endpoint {
                        job_id: job.id.clone(),
                        node: v.node.clone(),
                        port: m.port,
                        token: m.token.clone(),
                    };
                    (v, Some(endpoint))
                }
                None => {
                    v.state = "starting";
                    (v, None)
                }
            }
        }
        Some(job) => {
            v.ended = Some(
                slurm::terminal_state(&job.state)
                    .unwrap_or("ENDED")
                    .to_string(),
            );
            (v, None)
        }
        None => {
            // Not in the queue: just submitted (the queue lags a submit), or
            // gone. Without a queue read at all nothing can be said, so the
            // record's own word stands.
            let young = now_ms.saturating_sub(record.submitted_ms) < SUBMIT_GRACE_MS;
            if record.ended.is_none() && (young || !queue_known) {
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

/// What the start sheet offers on this cluster.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ClusterFacts {
    pub scheduler: Scheduler,
    pub version: String,
    pub partitions: Vec<PartitionChoice>,
    /// The user's accounts (empty when the cluster keeps none).
    pub accounts: Vec<String>,
    pub default_account: Option<String>,
    pub gpu_flag: GpuFlag,
    pub fetched_ms: u64,
}

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
        "umask 077\nC=\"{c}\"\nmkdir -p \"$C/w\" || exit 3\n\
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
        next.version = 1;
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
/// can be expanded by the cluster's shell (`$SCRATCH/x`, `~/x`) without that
/// shell running anything else.
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
/// record it in `cluster.json`.
pub async fn add_workspace(
    host: &str,
    home: RemoteHome,
    raw_path: &str,
    name: &str,
) -> anyhow::Result<ClusterWorkspace> {
    let input = plain_path_input(raw_path)?;
    let script = format!(
        "raw={}\n\
         p=$(sh -lc 'eval \"cd -- \\\"$1\\\"\" 2>/dev/null && pwd -P' sh \"$raw\" </dev/null 2>/dev/null | tail -n 1)\n\
         [ -n \"$p\" ] || p=$(eval \"cd -- \\\"$raw\\\"\" 2>/dev/null && pwd -P)\n\
         printf '===path %s\\n===end\\n' \"$p\"\n",
        sh_quote(&input)
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
    update_config(host, home, |config| {
        if let Some(existing) = config.workspaces.iter().find(|w| w.path == resolved) {
            return Ok(existing.clone());
        }
        let ws = ClusterWorkspace {
            id: chimaera_core::cluster::new_workspace_id(),
            name: name.clone(),
            path: resolved.clone(),
            created_ms: now_ms(),
            ..Default::default()
        };
        config.workspaces.push(ws.clone());
        Ok(ws)
    })
    .await
}

/// Forget a workspace: drop it from `cluster.json` and remove its chimaera
/// folder (chat journals, history — the project folder itself is never
/// touched). Refused while its job is in the queue.
pub async fn remove_workspace(host: &str, home: RemoteHome, id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(valid_workspace_id(id), "unknown workspace");
    let ov = overview(host, home).await?;
    if let Some(v) = ov.workspaces.iter().find(|w| w.id == id) {
        anyhow::ensure!(
            v.state == "stopped",
            "Stop {} first — its job is still in the queue",
            v.name
        );
    }
    update_config(host, home, |config| {
        config.workspaces.retain(|w| w.id != id);
        Ok(())
    })
    .await?;
    let script = format!("rm -rf -- \"{}\"\n", ws_dir(home, id));
    run_script(host, &script, EXEC_SECS).await?;
    Ok(())
}

// --- Starting and stopping ----------------------------------------------------------

/// How a start went.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartOutcome {
    /// `sbatch` took it.
    Submitted { job_id: String },
    /// The partition takes only interactive jobs: the caller holds the job
    /// in the foreground ([`spawn_attached`]).
    Attached { job_name: String },
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

/// What one start needs besides the spec.
pub struct StartRequest<'a> {
    pub workspace: &'a ClusterWorkspace,
    pub spec: &'a LaunchSpec,
    /// Lines for this run only (after the cluster's and the workspace's).
    pub run_startup: &'a str,
    /// Hold the job in the foreground instead of submitting it (a partition
    /// that refuses batch jobs).
    pub attached: bool,
    pub gpu_flag: GpuFlag,
}

/// Write the workspace's job files and submit (or, for an attached start,
/// only write them). The binary must already be on the cluster
/// (`ensure_cluster_binary`).
pub async fn start(
    host: &str,
    home: RemoteHome,
    config: &ClusterConfig,
    req: &StartRequest<'_>,
) -> anyhow::Result<StartOutcome> {
    let ws = req.workspace;
    anyhow::ensure!(valid_workspace_id(&ws.id), "unknown workspace");
    let spec = req.spec.clone().normalized();
    spec.validate().map_err(anyhow::Error::msg)?;
    let token = &chimaera_core::generate_token()[..6];
    let job_name = slurm::job_name(&ws.name, token);
    let dir = ws_dir(home, &ws.id);
    let record = LaunchRecord {
        job_id: if req.attached {
            None
        } else {
            Some("__CHIMAERA_JOB_ID__".into())
        },
        job_name: job_name.clone(),
        spec: spec.clone(),
        attached: req.attached,
        submitted_ms: now_ms(),
        ..Default::default()
    };
    let seed = WorkspaceSeed {
        id: ws.id.clone(),
        name: ws.name.clone(),
        path: ws.path.clone(),
    };
    let rules_source = config
        .agent_rules
        .file
        .as_deref()
        .filter(|f| f.starts_with('/'));
    let script_text = job_script(&JobScript {
        binary: &home.bin_path(),
        workspace_dir: &dir,
        state_home: home.job_state_home(),
        rules_source,
    });

    let mut s = String::from("umask 077\nunset SLURM_JOB_ID SLURM_JOBID\n");
    s.push_str(&path_line(host));
    s.push_str(&format!("D=\"{dir}\"\nmkdir -p \"$D/data\" || exit 3\n"));
    s.push_str(&write_file_lines("$D/job.sh", &script_text));
    s.push_str(&write_file_lines(
        "$D/startup.sh",
        &compose_startup(&config.startup, &ws.startup, req.run_startup),
    ));
    s.push_str(&write_file_lines(
        "$D/agent-rules.md",
        &config.agent_rules.text,
    ));
    s.push_str(&write_file_lines(
        "$D/workspace.json",
        &serde_json::to_string(&seed)?,
    ));
    s.push_str("rm -f \"$D/caps.json\"\n");
    let record_json = serde_json::to_string_pretty(&record)?;
    if req.attached {
        s.push_str(&write_file_lines("$D/launch.json", &record_json));
        s.push_str("printf '===rc 0\\n===end\\n'\n");
    } else {
        s.push_str(&write_file_lines("$D/launch.pending", &record_json));
        let args: Vec<String> = spec
            .sbatch_args(&job_name, "@OUTPUT@", req.gpu_flag)
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
             \x20 sed \"s/__CHIMAERA_JOB_ID__/$id/\" \"$D/launch.pending\" > \"$D/launch.json.tmp\" && mv -f \"$D/launch.json.tmp\" \"$D/launch.json\"\n\
             fi\n\
             rm -f \"$D/launch.pending\"\n\
             printf '===rc %s\\n===id %s\\n===err\\n' \"$rc\" \"$id\"; cat \"$D/.sbatch.err\" 2>/dev/null; rm -f \"$D/.sbatch.err\"\n\
             printf '===end\\n'\n",
            args.join(" ")
        ));
    }
    let out = run_script(host, &s, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    if section(&secs, "end").is_none() {
        bail!(
            "starting {} on {host} failed: {}",
            ws.name,
            super::ssh_failure_line(&out.stderr, &out.status)
        );
    }
    invalidate_queue(host);
    if req.attached {
        return Ok(StartOutcome::Attached { job_name });
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
    Ok(StartOutcome::Submitted { job_id: id })
}

/// Hold an attached job in the foreground: `ssh -tt host srun … job.sh`. The
/// pty makes the login node hang the job up when this connection ends —
/// app quit, laptop sleep, a dropped link — so nothing outlives the user's
/// session there. Dropping the returned child ends the job.
pub fn spawn_attached(
    host: &str,
    home: RemoteHome,
    workspace_id: &str,
    spec: &LaunchSpec,
    job_name: &str,
    gpu_flag: GpuFlag,
) -> anyhow::Result<Child> {
    anyhow::ensure!(valid_workspace_id(workspace_id), "unknown workspace");
    let args: Vec<String> = spec
        .srun_args(job_name, gpu_flag)
        .iter()
        .map(|a| sh_quote(a))
        .collect();
    let script = format!(
        "unset SLURM_JOB_ID SLURM_JOBID; {}exec srun {} /bin/bash \"{}/job.sh\"",
        path_line(host).replace('\n', " "),
        args.join(" "),
        ws_dir(home, workspace_id)
    );
    let mut cmd = super::ssh_base(host);
    cmd.env(ASKPASS_ALIAS_ENV, host)
        .arg("-tt")
        .arg(host)
        .arg(super::sh_wrap(&script))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    cmd.spawn().context("failed to start the attached job")
}

/// Stop a workspace's job (`scancel`, which signals the daemon first so it
/// saves its chats) and mark the record as stopped by the user. "Invalid job
/// id" means it's already gone — success.
pub async fn stop(
    host: &str,
    home: RemoteHome,
    workspace_id: &str,
    record: &LaunchRecord,
) -> anyhow::Result<()> {
    anyhow::ensure!(valid_workspace_id(workspace_id), "unknown workspace");
    let target = match &record.job_id {
        Some(id) if id.chars().all(|c| c.is_ascii_digit() || c == '_') && !id.is_empty() => {
            sh_quote(id)
        }
        _ => format!("--name={} -u \"$(id -un)\"", sh_quote(&record.job_name)),
    };
    let mut marked = record.clone();
    marked.stopped_by_user = true;
    let mut s = path_line(host);
    s.push_str(&format!(
        "umask 077\nD=\"{}\"\n",
        ws_dir(home, workspace_id)
    ));
    s.push_str(&format!(
        "err=$(scancel {target} 2>&1); rc=$?\nprintf '===rc %s\\n===err\\n%s\\n' \"$rc\" \"$err\"\n"
    ));
    s.push_str(&write_file_lines(
        "$D/launch.json",
        &serde_json::to_string_pretty(&marked)?,
    ));
    s.push_str("printf '===end\\n'\n");
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
    let rc = marker_arg(&secs, "rc").unwrap_or("1");
    let err = section(&secs, "err").unwrap_or("");
    if rc != "0" && !err.to_ascii_lowercase().contains("invalid job id") {
        bail!("{}", clean_tool_stderr(err, "scancel"));
    }
    Ok(())
}

/// Ask accounting once how a job ended and keep the answer in its record.
/// Clusters without accounting answer "ENDED".
pub async fn record_end(
    host: &str,
    home: RemoteHome,
    workspace_id: &str,
    record: &LaunchRecord,
) -> anyhow::Result<Ended> {
    anyhow::ensure!(valid_workspace_id(workspace_id), "unknown workspace");
    let id = record.job_id.clone().unwrap_or_default();
    let mut s = path_line(host);
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit() || c == '_') {
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
        ws_dir(home, workspace_id)
    );
    w.push_str(&write_file_lines(
        "$D/launch.json",
        &serde_json::to_string_pretty(&kept)?,
    ));
    run_script(host, &w, EXEC_SECS).await?;
    Ok(ended)
}

/// Slurm's own estimate of when a waiting job starts (epoch ms, cluster
/// clock), when it has one.
pub async fn start_estimate(host: &str, job_id: &str) -> anyhow::Result<Option<u64>> {
    anyhow::ensure!(
        !job_id.is_empty() && job_id.chars().all(|c| c.is_ascii_digit() || c == '_'),
        "invalid job id"
    );
    let mut s = path_line(host);
    s.push_str(&format!(
        "printf '===start\\n'; squeue --start -h -j {} -o '%S' 2>/dev/null | head -n 1\n\
         printf '===now %s\\n===tz %s\\n===end\\n' \"$(date +%s)\" \"$(date +%z)\"\n",
        sh_quote(job_id)
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

/// Remember what a workspace (and the cluster) last started with, and save a
/// named setup when asked.
pub async fn remember_spec(
    host: &str,
    home: RemoteHome,
    workspace_id: &str,
    spec: &LaunchSpec,
    save_as: Option<&str>,
) -> anyhow::Result<()> {
    let spec = spec.clone().normalized();
    update_config(host, home, |config| {
        config.last_spec = Some(spec.clone());
        if let Some(ws) = config.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            ws.last_spec = Some(spec.clone());
        }
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

pub async fn set_startup(
    host: &str,
    home: RemoteHome,
    workspace_id: Option<&str>,
    text: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        text.len() <= 32 * 1024,
        "Startup commands are limited to 32 KB"
    );
    anyhow::ensure!(!text.contains('\0'), "Startup commands can't contain NUL");
    update_config(host, home, |config| {
        match workspace_id {
            None => config.startup = text.to_string(),
            Some(id) => {
                let ws = config
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == id)
                    .context("unknown workspace")?;
                ws.startup = text.to_string();
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

// --- Files ------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    pub mtime_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

fn checked_remote_path(path: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.starts_with('/') && path.len() <= 4096 && !path.contains(['\n', '\0']),
        "not a path on the cluster"
    );
    Ok(())
}

/// One folder's entries — one `find -maxdepth 1` per click, like `ls` at a
/// prompt; nothing runs between clicks.
pub async fn list_dir(host: &str, path: &str) -> anyhow::Result<Listing> {
    checked_remote_path(path)?;
    let script = format!(
        "p={}\ncd -- \"$p\" 2>/dev/null || {{ printf '===missing\\n===end\\n'; exit 0; }}\n\
         printf '===pwd %s\\n' \"$(pwd -P)\"\n\
         find . -mindepth 1 -maxdepth 1 -printf '%y|%s|%T@|%f\\n' 2>/dev/null | head -n {}\n\
         printf '===end\\n'\n",
        sh_quote(path),
        LIST_MAX_ENTRIES + 1
    );
    let out = run_script(host, &script, EXEC_SECS).await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let secs = sections(&stdout);
    anyhow::ensure!(section(&secs, "end").is_some(), "listing {path} failed");
    anyhow::ensure!(
        section(&secs, "missing").is_none(),
        "{path} isn't a folder you can open"
    );
    let pwd = marker_arg(&secs, "pwd").unwrap_or(path).to_string();
    let body = secs
        .iter()
        .find(|(k, _)| k.starts_with("pwd"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let mut entries: Vec<Entry> = Vec::new();
    let mut truncated = false;
    for line in body.lines() {
        let mut f = line.splitn(4, '|');
        let (Some(kind), Some(size), Some(mtime), Some(name)) =
            (f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        if entries.len() >= LIST_MAX_ENTRIES {
            truncated = true;
            break;
        }
        entries.push(Entry {
            name: name.to_string(),
            dir: kind == "d",
            size: size.parse().unwrap_or(0),
            mtime_ms: mtime
                .split('.')
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .map(|s| s * 1000)
                .unwrap_or(0),
        });
    }
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(Listing {
        path: pwd,
        entries,
        truncated,
    })
}

/// Copy one file from the cluster to `dest` (capped at [`FETCH_MAX_BYTES`]),
/// streamed — the file peek opens the copy in the ordinary preview.
pub async fn fetch_file(host: &str, path: &str, dest: &std::path::Path) -> anyhow::Result<u64> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    checked_remote_path(path)?;
    let script = format!(
        "p={}\n[ -f \"$p\" ] && [ -r \"$p\" ] || exit 7\n\
         s=$(wc -c < \"$p\" | tr -d ' ')\n[ \"$s\" -le {} ] || exit 8\n\
         exec cat -- \"$p\"\n",
        sh_quote(path),
        FETCH_MAX_BYTES
    );
    let mut cmd = ssh_cmd(host);
    cmd.arg("sh -s")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("failed to run ssh")?;
    let mut stdin = child.stdin.take().context("ssh stdin was not piped")?;
    stdin.write_all(script.as_bytes()).await.ok();
    drop(stdin);
    let mut stdout = child.stdout.take().context("ssh stdout was not piped")?;
    let tmp = dest.with_extension("partial");
    let copy = async {
        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut total: u64 = 0;
        loop {
            let n = stdout.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            total += n as u64;
            anyhow::ensure!(total <= FETCH_MAX_BYTES, "the file is larger than 64 MB");
            file.write_all(&buf[..n]).await?;
        }
        file.flush().await?;
        Ok::<u64, anyhow::Error>(total)
    };
    let total = match tokio::time::timeout(Duration::from_secs(EXEC_SECS), copy).await {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!("copying {path} timed out")),
    };
    let status = child.wait().await?;
    let total = match (total, status.code()) {
        (Ok(t), Some(0)) => t,
        (_, Some(7)) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            bail!("{path} isn't a file you can read")
        }
        (_, Some(8)) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            bail!("{path} is larger than 64 MB — open it from a running workspace instead")
        }
        (Err(e), _) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
        (Ok(_), _) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            bail!("copying {path} failed")
        }
    };
    tokio::fs::rename(&tmp, dest).await?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> ClusterWorkspace {
        ClusterWorkspace {
            id: "w-0000abcd".into(),
            name: "crc".into(),
            path: "/scratch/u/crc".into(),
            ..Default::default()
        }
    }

    fn job(id: &str, state: &str, node: &str, left: &str, reason: &str) -> Job {
        Job {
            id: id.into(),
            name: "chimaera-crc~ab12cd".into(),
            partition: "batch".into(),
            state: state.into(),
            time_left: left.into(),
            nodes: node.into(),
            cpus: "4".into(),
            mem: "16G".into(),
            elapsed: String::new(),
            workdir: String::new(),
            reason: reason.into(),
        }
    }

    fn record(id: Option<&str>, submitted_ms: u64) -> LaunchRecord {
        LaunchRecord {
            job_id: id.map(str::to_string),
            job_name: "chimaera-crc~ab12cd".into(),
            spec: LaunchSpec {
                time: "1-00:00:00".into(),
                ..Default::default()
            },
            submitted_ms,
            ..Default::default()
        }
    }

    fn manifest(job: &str) -> Manifest {
        Manifest {
            hostname: "n042".into(),
            port: 41234,
            token: "tok".into(),
            pid: 7,
            version: "0.0.1".into(),
            started_at: 0,
            build: None,
            slurm_job_id: Some(job.into()),
        }
    }

    const NOW: u64 = 1_800_000_000_000;

    #[test]
    fn a_workspace_that_never_ran_is_fresh() {
        let (v, e) = workspace_view(&ws(), None, None, None, &[], true, NOW);
        assert_eq!(v.state, "stopped");
        assert!(v.fresh);
        assert!(e.is_none());
    }

    #[test]
    fn waiting_starting_running_follow_the_queue_and_this_jobs_manifest() {
        let r = record(Some("101"), NOW - 10_000);
        let pending = [job("101", "PENDING", "", "1-00:00:00", "Priority")];
        let (v, _) = workspace_view(&ws(), Some(&r), None, None, &pending, true, NOW);
        assert_eq!(v.state, "waiting");
        assert_eq!(v.reason, "", "plain priority isn't worth saying");
        let held = [job(
            "101",
            "PENDING",
            "",
            "1-00:00:00",
            "QOSMaxJobsPerUserLimit",
        )];
        let (v, _) = workspace_view(&ws(), Some(&r), None, None, &held, true, NOW);
        assert_eq!(v.reason, "QOSMaxJobsPerUserLimit");

        let running = [job("101", "RUNNING", "n042", "23:59:00", "None")];
        let (v, e) = workspace_view(&ws(), Some(&r), None, None, &running, true, NOW);
        assert_eq!(v.state, "starting", "no manifest yet");
        assert!(e.is_none());
        let stale = manifest("99");
        let (v, e) = workspace_view(&ws(), Some(&r), Some(&stale), None, &running, true, NOW);
        assert_eq!(
            v.state, "starting",
            "an older job's manifest is not this job's daemon"
        );
        assert!(e.is_none());

        let m = manifest("101");
        let (v, e) = workspace_view(&ws(), Some(&r), Some(&m), Some(true), &running, true, NOW);
        assert_eq!(v.state, "running");
        assert_eq!(v.node, "n042");
        assert_eq!(v.ends_at_ms, Some(NOW + 86_340_000));
        assert_eq!(v.egress, Some(true));
        let e = e.unwrap();
        assert_eq!(
            (e.port, e.token.as_str(), e.job_id.as_str()),
            (41234, "tok", "101")
        );
    }

    #[test]
    fn a_fresh_submit_waits_through_queue_lag_then_reads_as_stopped() {
        let young = record(Some("101"), NOW - 5_000);
        let (v, _) = workspace_view(&ws(), Some(&young), None, None, &[], true, NOW);
        assert_eq!(v.state, "waiting");
        let old = record(Some("101"), NOW - 3_600_000);
        let (v, _) = workspace_view(&ws(), Some(&old), None, None, &[], true, NOW);
        assert_eq!(v.state, "stopped");
        assert_eq!(v.ended, None, "asked of sacct once, then kept");
        let (v, _) = workspace_view(&ws(), Some(&old), None, None, &[], false, NOW);
        assert_eq!(
            v.state, "waiting",
            "no queue read at all: nothing to conclude"
        );
        let mut ended = old.clone();
        ended.ended = Some(Ended {
            state: "TIMEOUT".into(),
            at_ms: NOW - 1000,
        });
        let (v, _) = workspace_view(&ws(), Some(&ended), None, None, &[], true, NOW);
        assert_eq!(v.ended.as_deref(), Some("TIMEOUT"));
    }

    #[test]
    fn an_attached_job_is_found_by_its_unique_name() {
        let mut r = record(None, NOW - 3_600_000);
        r.attached = true;
        let running = [job("555", "RUNNING", "n7", "1:00:00", "None")];
        let m = manifest("555");
        let (v, e) = workspace_view(&ws(), Some(&r), Some(&m), None, &running, true, NOW);
        assert_eq!(v.state, "running");
        assert_eq!(v.job_id.as_deref(), Some("555"));
        assert!(v.attached);
        assert!(e.is_some());
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
        assert_eq!(plain_path_input("/oak/lab data").unwrap(), "/oak/lab data");
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
}
