//! Compute-scheduler awareness (M5 HPC layer, detection slice): detect Slurm
//! on THIS host and serve a bounded snapshot of the user's queue — the
//! daemon-side, git-service-style answer to "is this an HPC, and what do I
//! have running?". A remote daemon detects its own cluster locally, so the
//! feature lights up on HPC and is a no-op on a laptop; nothing is probed
//! over ssh at connect time.
//!
//! Resource discipline is the design (shared login nodes; sysadmins ban
//! tools that hammer `squeue`): detection runs once per daemon lifetime
//! (`?refresh=true` re-detects), every child process gets a hard
//! kill-on-timeout and an output cap, snapshots are cached ~30s, and
//! concurrent requests coalesce on one refresh. Nothing is persisted.
//!
//! Command output is format-string based (`squeue -o`, `sinfo -o`), not
//! `--json`: the format flags are stable across the old Slurm versions real
//! clusters run, and the output is bounded by construction.
//!
//! Test knob: `CHIMAERA_SLURM_BINDIR` points at a directory of stand-in
//! `sbatch`/`squeue`/`sinfo` executables so the whole surface can be driven
//! live without a cluster (the `CHIMAERA_RELEASES_API` pattern).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Hard runtime cap per scheduler command (squeue on a busy cluster can be
/// slow, but a wedged controller must never wedge the daemon).
const CMD_TIMEOUT: Duration = Duration::from_secs(5);
/// Output cap per command; queues are line-capped far below this anyway.
const MAX_OUTPUT: usize = 256 * 1024;
/// Row caps (the UI shows a strip, not a dashboard); `truncated` says so.
const MAX_JOBS: usize = 50;
const MAX_PARTITIONS: usize = 50;
/// Snapshot TTL: fresh enough for a queue strip, polite to the controller.
const SNAPSHOT_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Job {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) partition: String,
    pub(crate) state: String,
    pub(crate) time_left: String,
    pub(crate) nodes: String,
    /// Requested/allocated CPUs (`%C`) and min memory (`%m`) — squeue's own
    /// resource truth, so a job with no launch record (an sbatch that timed
    /// out after submitting, or one launched outside chimaera) still shows
    /// what it holds. "" when the wire lacked them.
    pub(crate) cpus: String,
    pub(crate) mem: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Partition {
    pub(crate) name: String,
    /// Per-partition ceilings straight from sinfo (`%l` walltime,
    /// `%c` cpus/node, `%m` MB/node — "+" suffixes mean "varies upward").
    /// Raw strings, "" when the wire lacked them: the launch dialog shows
    /// them as hints and pre-flights the walltime, because "what can I
    /// request here" is standard Slurm the UI should know (maintainer ask).
    pub(crate) time_limit: String,
    pub(crate) cpus_per_node: String,
    pub(crate) mem_per_node: String,
    pub(crate) default: bool,
    pub(crate) avail: bool,
    pub(crate) nodes: u64,
}

/// The daemon's OWN allocation, when it runs inside a Slurm job (a Mode 2
/// compute-node daemon): the window's bottom bar wears `time_left` — the
/// honest "this workspace lives until walltime" indicator.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SelfAllocation {
    pub(crate) job_id: String,
    pub(crate) node: String,
    pub(crate) partition: String,
    pub(crate) state: String,
    pub(crate) time_left: String,
    /// Allocated resources (squeue %C/%m/%b) — the window's allocation
    /// strip shows what you actually have on this node.
    pub(crate) cpus: String,
    pub(crate) mem: String,
    pub(crate) gres: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ComputeSnapshot {
    /// "slurm" | "none". The extensibility seam: a future scheduler adds a
    /// tag here, not a new route.
    pub(crate) scheduler: String,
    pub(crate) jobs: Vec<Job>,
    pub(crate) partitions: Vec<Partition>,
    /// Present only when the daemon itself runs inside an allocation.
    #[serde(rename = "self", skip_serializing_if = "Option::is_none")]
    pub(crate) self_alloc: Option<SelfAllocation>,
    pub(crate) fetched_at_ms: u64,
    pub(crate) truncated: bool,
    /// True when this refresh's `squeue` failed (timeout/error) and `jobs`
    /// carries the previous snapshot forward — "may be stale", not "empty".
    /// One wedged controller call must not make every card vanish for a
    /// poll cycle (and must not turn live jobs into "ended" tombstones).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) degraded: bool,
}

impl ComputeSnapshot {
    fn none() -> Self {
        ComputeSnapshot {
            scheduler: "none".to_string(),
            jobs: Vec::new(),
            partitions: Vec::new(),
            self_alloc: None,
            fetched_at_ms: now_ms(),
            truncated: false,
            degraded: false,
        }
    }
}

/// Where the scheduler binaries live once detected. All four client tools
/// are required together: the snapshot needs squeue/sinfo, Mode 2's
/// launch/cancel routes need sbatch/scancel — a cluster with a partial
/// toolset is not one we can operate on.
#[derive(Clone, Debug)]
pub(crate) enum Detection {
    None,
    Slurm {
        sbatch: PathBuf,
        scancel: PathBuf,
        squeue: PathBuf,
        sinfo: PathBuf,
    },
}

pub(crate) struct ComputeService {
    /// One async lock covers detection + the snapshot cache: refreshes are
    /// single-flight (concurrent GETs await the first refresher and then
    /// read its cache), and nothing here is hot enough to shard.
    inner: tokio::sync::Mutex<Inner>,
    /// Test knob dir (`CHIMAERA_SLURM_BINDIR`), read once at construction.
    bindir: Option<PathBuf>,
    /// Set when THIS daemon runs inside a Slurm allocation (a Mode 2
    /// compute-node daemon): `SLURM_JOB_ID` at construction. Drives the
    /// snapshot's `self` block.
    self_job: Option<String>,
}

#[derive(Default)]
struct Inner {
    detection: Option<Detection>,
    cache: Option<(Instant, ComputeSnapshot)>,
}

impl ComputeService {
    pub(crate) fn new() -> Self {
        ComputeService {
            inner: tokio::sync::Mutex::new(Inner::default()),
            bindir: std::env::var_os("CHIMAERA_SLURM_BINDIR").map(PathBuf::from),
            self_job: std::env::var("SLURM_JOB_ID").ok().filter(|s| !s.is_empty()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_bindir(dir: PathBuf) -> Self {
        ComputeService {
            inner: tokio::sync::Mutex::new(Inner::default()),
            bindir: Some(dir),
            self_job: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_bindir_and_job(dir: PathBuf, job: &str) -> Self {
        ComputeService {
            inner: tokio::sync::Mutex::new(Inner::default()),
            bindir: Some(dir),
            self_job: Some(job.to_string()),
        }
    }

    /// The detected scheduler toolset (for the Mode 2 launch/cancel routes);
    /// runs detection if it hasn't happened yet.
    pub(crate) async fn detection(&self) -> Detection {
        let mut inner = self.inner.lock().await;
        if inner.detection.is_none() {
            inner.detection = Some(self.detect().await);
        }
        inner.detection.clone().expect("just set")
    }

    /// The current snapshot: cached, single-flight, never an error (a
    /// cluster hiccup degrades to an empty-but-tagged snapshot, not a 500 —
    /// the strip is orientation, not ground truth).
    pub(crate) async fn snapshot(&self, refresh: bool) -> ComputeSnapshot {
        let mut inner = self.inner.lock().await;
        if refresh {
            inner.detection = None;
            inner.cache = None;
        }
        if inner.detection.is_none() {
            inner.detection = Some(self.detect().await);
        }
        let detection = inner.detection.clone().expect("just set");
        let (squeue, sinfo) = match &detection {
            Detection::None => return ComputeSnapshot::none(),
            Detection::Slurm { squeue, sinfo, .. } => (squeue.clone(), sinfo.clone()),
        };
        if let Some((at, snap)) = &inner.cache {
            if at.elapsed() < SNAPSHOT_TTL {
                return snap.clone();
            }
        }
        // Holding the lock across the fetch IS the single-flight: concurrent
        // requests queue here briefly instead of stampeding the controller.
        let mut snap = fetch_snapshot(&squeue, &sinfo, self.self_job.as_deref()).await;
        if snap.degraded {
            // squeue failed this round: carry the previous jobs forward
            // (tagged) rather than serving a false "queue is empty".
            if let Some((_, prev)) = &inner.cache {
                snap.jobs = prev.jobs.clone();
            }
        }
        inner.cache = Some((Instant::now(), snap.clone()));
        snap
    }

    /// Drop the cached snapshot (detection stays): the next list refetches
    /// the queue NOW. Launch/cancel call this so the instant refresh the UI
    /// fires right after sees the queue change instead of a ≤30s-stale
    /// cache — a fresh launch otherwise reads as an orphaned record and
    /// briefly wears an "ended" card (found live, maintainer's 4th round).
    pub(crate) async fn invalidate(&self) {
        self.inner.lock().await.cache = None;
    }

    /// Find the Slurm client tools. The knob dir wins (tests / unusual
    /// installs); otherwise ask the user's login shell for its PATH and walk
    /// it here — the profile-managed-PATH reasoning of the git resolution,
    /// WITHOUT `command -v`: real clusters wrap the tools in profile shell
    /// functions (Sherlock's login rc does — found live: `command -v squeue`
    /// prints the bare function name, not a path), and a PATH walk is also
    /// the only form that works identically under bash/zsh/fish.
    async fn detect(&self) -> Detection {
        if let Some(dir) = &self.bindir {
            let tools = ["sbatch", "scancel", "squeue", "sinfo"].map(|n| dir.join(n));
            if tools.iter().all(|p| p.is_file()) {
                tracing::info!(dir = %dir.display(), "slurm tools from CHIMAERA_SLURM_BINDIR");
                let [sbatch, scancel, squeue, sinfo] = tools;
                return Detection::Slurm {
                    sbatch,
                    scancel,
                    squeue,
                    sinfo,
                };
            }
            tracing::warn!(dir = %dir.display(), "CHIMAERA_SLURM_BINDIR set but sbatch/scancel/squeue/sinfo not all present");
            return Detection::None;
        }
        let shell = chimaera_core::login_shell();
        let out = run_capped(&shell, &["-lc".into(), "printf %s \"$PATH\"".into()]).await;
        let Some(path) = out else {
            return Detection::None;
        };
        // The walk stats PATH entries that may live on slow network mounts —
        // off the reactor with it (detection runs once per daemon lifetime).
        let found = tokio::task::spawn_blocking(move || {
            ["sbatch", "scancel", "squeue", "sinfo"].map(|n| find_on_path(path.trim(), n))
        })
        .await
        .unwrap_or([None, None, None, None]);
        match found {
            [Some(sbatch), Some(scancel), Some(squeue), Some(sinfo)] => {
                tracing::info!(squeue = %squeue.display(), "slurm detected");
                Detection::Slurm {
                    sbatch,
                    scancel,
                    squeue,
                    sinfo,
                }
            }
            _ => Detection::None,
        }
    }
}

/// First executable `name` on the colon-separated `path` (the login shell's
/// PATH, resolved fresh). Empty PATH members are skipped — searching the cwd
/// is sh legacy the daemon must not inherit.
fn find_on_path(path: &str, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}

async fn fetch_snapshot(
    squeue: &std::path::Path,
    sinfo: &std::path::Path,
    self_job: Option<&str>,
) -> ComputeSnapshot {
    // The daemon's own allocation, when it has one: one bounded squeue -j.
    let self_alloc = match self_job {
        Some(id) => run_capped(
            &squeue.to_string_lossy(),
            &[
                "-j".into(),
                id.to_string(),
                "--noheader".into(),
                "-o".into(),
                "%i|%P|%T|%L|%N|%C|%m|%b".into(),
            ],
        )
        .await
        .as_deref()
        .and_then(parse_self_allocation),
        None => None,
    };
    // `-u <user>`, not `--me`: --me is newer than the Slurm versions real
    // clusters still run. No USER (exotic) → skip the queue rather than
    // listing the whole cluster's jobs.
    let jobs_out = match std::env::var("USER") {
        Ok(user) => {
            run_capped(
                &squeue.to_string_lossy(),
                &[
                    "-u".into(),
                    user,
                    "--noheader".into(),
                    "-o".into(),
                    "%i|%j|%P|%T|%L|%N|%C|%m".into(),
                ],
            )
            .await
        }
        Err(_) => None,
    };
    let parts_out = run_capped(
        &sinfo.to_string_lossy(),
        &["--noheader".into(), "-o".into(), "%P|%a|%D|%l|%c|%m".into()],
    )
    .await;

    let (jobs, jobs_truncated) = parse_squeue(jobs_out.as_deref().unwrap_or(""));
    let (partitions, parts_truncated) = parse_sinfo(parts_out.as_deref().unwrap_or(""));
    ComputeSnapshot {
        scheduler: "slurm".to_string(),
        jobs,
        partitions,
        self_alloc,
        fetched_at_ms: now_ms(),
        truncated: jobs_truncated || parts_truncated,
        // None = the squeue CALL failed (timeout/exit), distinct from an
        // empty queue (Some("")) — the caller substitutes last-good jobs.
        degraded: jobs_out.is_none(),
    }
}

/// `squeue -j <id> --noheader -o "%i|%P|%T|%L|%N|%C|%m|%b"` → the daemon's
/// own allocation. None on noise/absence (job already gone = no block).
/// `%b` (gres) prints "N/A" when none — normalized to empty.
fn parse_self_allocation(out: &str) -> Option<SelfAllocation> {
    let line = out.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut f = line.splitn(8, '|').map(str::trim);
    let (id, partition, state, time_left) = (f.next()?, f.next()?, f.next()?, f.next()?);
    if !id.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let node = f.next().unwrap_or("").to_string();
    let cpus = f.next().unwrap_or("").to_string();
    let mem = f.next().unwrap_or("").to_string();
    let gres = f.next().unwrap_or("").trim().to_string();
    Some(SelfAllocation {
        job_id: id.to_string(),
        node,
        partition: partition.to_string(),
        state: state.to_string(),
        time_left: time_left.to_string(),
        cpus,
        mem,
        gres: if gres.eq_ignore_ascii_case("n/a") {
            String::new()
        } else {
            gres
        },
    })
}

/// `squeue --noheader -o "%i|%j|%P|%T|%L|%N|%C|%m"` → jobs. Unparseable
/// lines are skipped, never fatal (Slurm banners/warnings sometimes precede
/// output). The trailing resource fields are tolerated missing — older rows
/// or exotic formats degrade to "".
fn parse_squeue(out: &str) -> (Vec<Job>, bool) {
    let mut jobs = Vec::new();
    let mut truncated = false;
    for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut f = line.splitn(8, '|').map(str::trim);
        let (Some(id), Some(name), Some(partition), Some(state), Some(time_left)) =
            (f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        if id.is_empty() || !id.starts_with(|c: char| c.is_ascii_digit()) {
            continue; // not a job row
        }
        if jobs.len() >= MAX_JOBS {
            truncated = true;
            break;
        }
        jobs.push(Job {
            id: id.to_string(),
            name: name.to_string(),
            partition: partition.to_string(),
            state: state.to_string(),
            time_left: time_left.to_string(),
            // %N is empty while pending — an empty string is the honest value.
            nodes: f.next().unwrap_or("").to_string(),
            cpus: f.next().unwrap_or("").to_string(),
            mem: f.next().unwrap_or("").to_string(),
        });
    }
    (jobs, truncated)
}

/// `sinfo --noheader -o "%P|%a|%D|%l|%c|%m"` → partitions. sinfo groups
/// rows by the non-numeric format fields, so this yields one row per
/// (partition, avail); the default partition carries a `*` suffix on its
/// name. The limit tail (%l walltime, %c cpus/node, %m MB/node) is
/// tolerated missing.
fn parse_sinfo(out: &str) -> (Vec<Partition>, bool) {
    let mut partitions: Vec<Partition> = Vec::new();
    let mut truncated = false;
    for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut f = line.splitn(6, '|').map(str::trim);
        let (Some(raw_name), Some(avail)) = (f.next(), f.next()) else {
            continue;
        };
        if raw_name.is_empty() {
            continue;
        }
        let (name, default) = match raw_name.strip_suffix('*') {
            Some(base) => (base, true),
            None => (raw_name, false),
        };
        let nodes: u64 = f.next().and_then(|n| n.trim().parse().ok()).unwrap_or(0);
        let time_limit = f.next().unwrap_or("").to_string();
        let cpus_per_node = f.next().unwrap_or("").to_string();
        let mem_per_node = f.next().unwrap_or("").to_string();
        // A partition briefly listed under two avail states: keep one row,
        // sum the nodes, "up" wins, first non-empty limits stick
        // (orientation, not accounting).
        if let Some(existing) = partitions.iter_mut().find(|p| p.name == name) {
            existing.nodes += nodes;
            existing.avail |= avail == "up";
            existing.default |= default;
            if existing.time_limit.is_empty() {
                existing.time_limit = time_limit;
            }
            continue;
        }
        if partitions.len() >= MAX_PARTITIONS {
            truncated = true;
            break;
        }
        partitions.push(Partition {
            name: name.to_string(),
            time_limit,
            cpus_per_node,
            mem_per_node,
            default,
            avail: avail == "up",
            nodes,
        });
    }
    (partitions, truncated)
}

/// Run one child with the default timeout + output cap; None on spawn
/// failure, non-zero exit, or timeout (the caller degrades, never errors).
/// Shared with `compute_jobs` (scancel rides the same discipline).
pub(crate) async fn run_capped(bin: &str, args: &[String]) -> Option<String> {
    run_capped_within(bin, args, CMD_TIMEOUT).await
}

/// [`run_capped`] with a caller-chosen deadline. sbatch needs a longer one
/// than the 5s poll discipline: a busy controller can take longer to answer,
/// and killing sbatch mid-flight does NOT unsubmit — the tight cap turned a
/// slow submission into "sbatch failed" plus a ghost job in the queue
/// (found live, maintainer's 5th round).
pub(crate) async fn run_capped_within(
    bin: &str,
    args: &[String],
    timeout: Duration,
) -> Option<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Timeout drops the future, dropping the child; this reaps it.
        .kill_on_drop(true);
    let fut = async {
        let out = cmd.output().await.ok()?;
        out.status.success().then_some(out.stdout)
    };
    let bytes = tokio::time::timeout(timeout, fut).await.ok()??;
    let mut s = String::from_utf8_lossy(&bytes).into_owned();
    if s.len() > MAX_OUTPUT {
        s.truncate(MAX_OUTPUT);
    }
    Some(s)
}

/// How a [`run_reported`] child failed — the caller's branch point: a tool
/// that SPOKE gets its words surfaced; a timeout may have half-succeeded
/// (sbatch submits before it answers) and needs reconciliation.
pub(crate) enum ExecFailure {
    Timeout,
    /// Spawn failure or non-zero exit; carries the tool's own (cleaned)
    /// stderr — for sbatch that text is the whole diagnosis ("Batch jobs
    /// are not allowed in the 'dev' partition…" — found live; the generic
    /// error hid it).
    Tool(String),
}

/// [`run_capped_within`] for user-initiated actions: same timeout + caps,
/// but stderr is kept and returned on failure instead of swallowed.
pub(crate) async fn run_reported(
    bin: &str,
    args: &[String],
    timeout: Duration,
) -> Result<String, ExecFailure> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let fut = async {
        match cmd.output().await {
            Ok(out) if out.status.success() => {
                let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
                if s.len() > MAX_OUTPUT {
                    s.truncate(MAX_OUTPUT);
                }
                Ok(s)
            }
            Ok(out) => {
                let tool = std::path::Path::new(bin)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Err(ExecFailure::Tool(clean_tool_stderr(
                    &String::from_utf8_lossy(&out.stderr),
                    &tool,
                )))
            }
            Err(e) => Err(ExecFailure::Tool(format!("could not run {bin}: {e}"))),
        }
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(res) => res,
        Err(_) => Err(ExecFailure::Timeout),
    }
}

/// Slurm's stderr, made presentable: `<tool>: error: ` prefixes stripped,
/// ASCII ruler lines dropped, whitespace collapsed, length capped. What
/// remains is the admin-authored message — the most cluster-specific,
/// user-actionable text we will ever have.
fn clean_tool_stderr(raw: &str, tool: &str) -> String {
    let tool_prefix = format!("{tool}: ");
    let mut cleaned: Vec<String> = Vec::new();
    for line in raw.lines() {
        let mut line = line.trim();
        if !tool.is_empty() {
            while let Some(rest) = line.strip_prefix(&tool_prefix) {
                line = rest.trim_start();
            }
        }
        while let Some(rest) = line.strip_prefix("error:") {
            line = rest.trim_start();
        }
        if line.is_empty() || line.chars().all(|c| c == '=' || c == '-') {
            continue;
        }
        cleaned.push(line.to_string());
    }
    let mut s = cleaned.join(" ");
    if s.is_empty() {
        s = "the command failed without a message".to_string();
    }
    if s.len() > 400 {
        let cut = s
            .char_indices()
            .take_while(|(i, _)| *i < 400)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(400);
        s.truncate(cut);
        s.push('…');
    }
    s
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Deserialize)]
pub(crate) struct ComputeQuery {
    #[serde(default)]
    pub(crate) refresh: bool,
}

/// GET /api/v1/compute — scheduler detection + the user's queue snapshot.
pub(crate) async fn get_compute(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ComputeQuery>,
) -> Response {
    Json(state.compute.snapshot(q.refresh).await).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("chimaera-compute-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_squeue_rows_caps_and_skips_noise() {
        // Shapes measured on Sherlock 2026-07-14 (`%i %j %N %T %L`), plus
        // the %C|%m resource tail (2026-07-16) — tolerated missing.
        let out = "34022541|chimaera-test|normal|RUNNING|9:54|sh02-01n58|4|16G\n\
                   34022542|align.sh|owners|PENDING|8:00:00|\n\
                   slurm_load_jobs: Warning: something\n";
        let (jobs, truncated) = parse_squeue(out);
        assert!(!truncated);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].id, "34022541");
        assert_eq!(jobs[0].name, "chimaera-test");
        assert_eq!(jobs[0].state, "RUNNING");
        assert_eq!(jobs[0].time_left, "9:54");
        assert_eq!(jobs[0].nodes, "sh02-01n58");
        assert_eq!(jobs[0].cpus, "4");
        assert_eq!(jobs[0].mem, "16G");
        assert_eq!(jobs[1].nodes, "", "pending job has no nodes yet");
        assert_eq!(jobs[1].cpus, "", "short rows degrade to empty resources");

        let many: String = (0..60)
            .map(|i| format!("{i}|j{i}|p|RUNNING|1:00|n{i}\n"))
            .collect();
        let (jobs, truncated) = parse_squeue(&many);
        assert_eq!(jobs.len(), MAX_JOBS);
        assert!(truncated);
    }

    #[test]
    fn parse_sinfo_default_star_dedupe_avail_and_limits() {
        // Sherlock-shaped rows with the %l|%c|%m limit tail (2026-07-16);
        // short rows (older callers / exotic sinfo) degrade to "".
        let out = "normal*|up|123|7-00:00:00|20+|128000+\n\
                   owners|up|1200\n\
                   gpu|down|48|2-00:00:00|20+|191000+\n\
                   normal*|up|7|7-00:00:00|20+|128000+\n";
        let (parts, truncated) = parse_sinfo(out);
        assert!(!truncated);
        assert_eq!(parts.len(), 3);
        let normal = &parts[0];
        assert_eq!(normal.name, "normal");
        assert!(normal.default);
        assert!(normal.avail);
        assert_eq!(normal.nodes, 130, "duplicate avail-state rows sum");
        assert_eq!(normal.time_limit, "7-00:00:00");
        assert_eq!(normal.cpus_per_node, "20+");
        assert_eq!(normal.mem_per_node, "128000+");
        assert_eq!(parts[1].time_limit, "", "short rows degrade to empty");
        assert!(!parts[2].avail);
        assert!(!parts[1].default);
    }

    #[test]
    fn clean_tool_stderr_keeps_the_admin_message() {
        // The real Sherlock dev-partition refusal, verbatim shape.
        let raw = "sbatch: error: =============================================================================\n\
                   sbatch: error:  ERROR: batch job not allowed\n\
                   sbatch: error: =============================================================================\n\
                   sbatch: error:  Batch jobs are not allowed in the 'dev' partition, which is reserved for\n\
                   sbatch: error:  interactive sessions (salloc/srun/sdev and OnDemand). Please submit\n\
                   sbatch: error:  batch jobs to another partition (e.g. 'normal').\n\
                   sbatch: error: -----------------------------------------------------------------------------\n\
                   sbatch: error: Batch job submission failed: Invalid partition name specified\n";
        let msg = clean_tool_stderr(raw, "sbatch");
        assert!(msg.starts_with("ERROR: batch job not allowed"));
        assert!(msg.contains("reserved for interactive sessions"));
        assert!(msg.contains("Please submit batch jobs to another partition"));
        assert!(!msg.contains("====="), "ruler lines dropped");
        assert!(!msg.contains("sbatch:"), "tool prefixes stripped");
        assert_eq!(
            clean_tool_stderr("", "sbatch"),
            "the command failed without a message"
        );
    }

    #[test]
    fn find_on_path_walks_skips_and_requires_exec() {
        let dir = test_dir("path");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("squeue");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Plain file, not executable: must not count (the Sherlock lesson —
        // detection must find real binaries, not whatever a profile says).
        std::fs::write(bin.join("sinfo"), "not a binary").unwrap();

        let path = format!("/nonexistent::{}", bin.display());
        assert_eq!(find_on_path(&path, "squeue"), Some(exe));
        #[cfg(unix)]
        assert_eq!(find_on_path(&path, "sinfo"), None);
        assert_eq!(find_on_path("", "squeue"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn snapshot_none_without_slurm_and_slurm_with_bindir() {
        // A bindir missing the binaries → none (and no login-shell probe).
        let empty = test_dir("empty");
        let svc = ComputeService::with_bindir(empty.clone());
        assert_eq!(svc.snapshot(false).await.scheduler, "none");

        // Fake tools → slurm, parsed snapshot, cached second read. squeue
        // answers both forms: -u (the queue) and -j (the self allocation).
        let dir = test_dir("fake");
        for (name, body) in [
            ("sbatch", "#!/bin/sh\nexit 0\n"),
            ("scancel", "#!/bin/sh\nexit 0\n"),
            (
                "squeue",
                "#!/bin/sh\nif [ \"$1\" = \"-j\" ]; then echo \"$2|gpu|RUNNING|3:59:00|node7\"; else echo '1|myjob|normal|RUNNING|59:00|node1'; fi\n",
            ),
            ("sinfo", "#!/bin/sh\necho 'normal*|up|10'\n"),
        ] {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let svc = ComputeService::with_bindir(dir.clone());
        let snap = svc.snapshot(false).await;
        assert_eq!(snap.scheduler, "slurm");
        assert_eq!(snap.jobs.len(), 1);
        assert_eq!(snap.jobs[0].name, "myjob");
        assert_eq!(snap.partitions.len(), 1);
        assert!(snap.partitions[0].default);
        // No SLURM_JOB_ID → no self block, and the wire omits the key.
        assert!(snap.self_alloc.is_none());
        assert!(!serde_json::to_string(&snap).unwrap().contains("\"self\""));
        // Cached: a second call inside the TTL returns the same fetch.
        let again = svc.snapshot(false).await;
        assert_eq!(again.fetched_at_ms, snap.fetched_at_ms);

        // Inside an allocation: the self block rides the snapshot.
        let svc = ComputeService::with_bindir_and_job(dir.clone(), "4242");
        let snap = svc.snapshot(false).await;
        assert!(serde_json::to_string(&snap).unwrap().contains("\"self\""));
        let own = snap.self_alloc.expect("self allocation");
        assert_eq!(own.job_id, "4242");
        assert_eq!(own.node, "node7");
        assert_eq!(own.time_left, "3:59:00");

        std::fs::remove_dir_all(&empty).ok();
        std::fs::remove_dir_all(&dir).ok();
    }
}
