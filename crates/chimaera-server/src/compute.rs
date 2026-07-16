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
//! `srun`/`scancel`/`squeue`/`sinfo` executables so the whole surface can be driven
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
    /// resource truth, so a job with no launch record (a launch whose id
    /// was never adopted, or one launched outside chimaera) still shows
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
/// launch/cancel routes need srun/scancel — a cluster with a partial
/// toolset is not one we can operate on.
#[derive(Clone, Debug)]
pub(crate) enum Detection {
    None,
    Slurm {
        srun: PathBuf,
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
    /// The rendered compute-session agent context, baked once per daemon
    /// lifetime (see [`ComputeService::agent_context`]).
    agent_context: Option<String>,
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

    /// The context block a compute-node daemon injects into its agent
    /// sessions (delivered via the claude hook response — see
    /// `agents::ingest`). `None` for a normal daemon: the `self_job` check
    /// is the whole off-cluster path — no lock, no subprocess, no allocation
    /// — so this is provably inert unless `SLURM_JOB_ID` was set at boot.
    ///
    /// Baked ONCE per daemon lifetime, at first use: the allocation's facts
    /// (job/node/partition/resources) are constant for the job's life, and
    /// the walltime end is stored as an ABSOLUTE estimate (bake-time now +
    /// squeue's time_left) rather than re-derived per spawn — any bake
    /// moment yields the same end instant (modulo squeue's minute rounding),
    /// one bake keeps every session's text identical, and a relative
    /// "3:59 left" would go stale in the transcript.
    pub(crate) async fn agent_context(&self) -> Option<String> {
        self.self_job.as_ref()?;
        if let Some(ctx) = &self.inner.lock().await.agent_context {
            return Some(ctx.clone());
        }
        // Not baked yet. The snapshot is cached + single-flight (worst case
        // one 5s-capped squeue); a failed squeue yields no self block, and
        // the bake simply retries on the next call rather than caching
        // an absence forever.
        let alloc = self.snapshot(false).await.self_alloc?;
        let text = self_context_text(&alloc, SystemTime::now());
        let mut inner = self.inner.lock().await;
        // get_or_insert: a concurrent first bake wins and both callers hand
        // out the SAME string (the two candidates differ only by seconds).
        Some(inner.agent_context.get_or_insert(text).clone())
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
            let tools = ["srun", "scancel", "squeue", "sinfo"].map(|n| dir.join(n));
            if tools.iter().all(|p| p.is_file()) {
                tracing::info!(dir = %dir.display(), "slurm tools from CHIMAERA_SLURM_BINDIR");
                let [srun, scancel, squeue, sinfo] = tools;
                return Detection::Slurm {
                    srun,
                    scancel,
                    squeue,
                    sinfo,
                };
            }
            tracing::warn!(dir = %dir.display(), "CHIMAERA_SLURM_BINDIR set but srun/scancel/squeue/sinfo not all present");
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
            ["srun", "scancel", "squeue", "sinfo"].map(|n| find_on_path(path.trim(), n))
        })
        .await
        .unwrap_or([None, None, None, None]);
        match found {
            [Some(srun), Some(scancel), Some(squeue), Some(sinfo)] => {
                tracing::info!(squeue = %squeue.display(), "slurm detected");
                Detection::Slurm {
                    srun,
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

/// Render the compute-session context for one [`SelfAllocation`]: what a
/// model on this node needs to know that nothing else tells it — it is on a
/// compute node (not the login node it may assume), what the allocation
/// provides, and that everything here dies at walltime. `now` is the bake
/// time (threaded in so tests can pin it); the walltime end lands in the
/// text as an absolute UTC instant. Missing resource fields (older squeue
/// rows degrade to "") are simply omitted, never rendered empty.
fn self_context_text(alloc: &SelfAllocation, now: SystemTime) -> String {
    let mut place = format!("Slurm job {}", alloc.job_id);
    if !alloc.node.is_empty() {
        place.push_str(&format!(" on node {}", alloc.node));
    }
    if !alloc.partition.is_empty() {
        place.push_str(&format!(" (partition {})", alloc.partition));
    }
    let mut resources = Vec::new();
    if !alloc.cpus.is_empty() {
        resources.push(format!("{} CPUs", alloc.cpus));
    }
    if !alloc.mem.is_empty() {
        resources.push(format!("{} memory", alloc.mem));
    }
    if !alloc.gres.is_empty() {
        resources.push(format!("gres {}", alloc.gres));
    }
    let resources = if resources.is_empty() {
        String::new()
    } else {
        format!(" The allocation provides {}.", resources.join(", "))
    };
    // "UNLIMITED"/"NOT_SET"/noise parse to None: state the walltime rule
    // without inventing an end time.
    let ends = match parse_slurm_time_left(&alloc.time_left)
        .and_then(|left| format_utc_minute(now + left))
    {
        Some(end) => format!("approximately {end}"),
        None => "its walltime".to_string(),
    };
    format!(
        "This session runs on a Slurm COMPUTE NODE inside an allocation \
         ({place}), not on a login node.{resources} Heavy computation \
         belongs here and can run directly (no need to submit it as a \
         separate job). The allocation ends at {ends}, and everything \
         running on this node — including this session — is killed then. \
         Tools or services that exist only on login nodes (for example \
         outbound network access or job submission on some clusters) may \
         behave differently or be unavailable here."
    )
}

/// Parse Slurm's elapsed/remaining time format (`squeue %L` / `sinfo %l`):
/// `days-hours:minutes:seconds` with zero leading components omitted, so
/// `9:54` is min:sec and `8:00:00` is h:min:sec. `UNLIMITED`, `NOT_SET`,
/// `INVALID`, and bare numbers (ambiguous) are `None`.
fn parse_slurm_time_left(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (days, rest) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, s),
    };
    let parts: Vec<u64> = rest
        .split(':')
        .map(|p| p.parse::<u64>().ok())
        .collect::<Option<_>>()?;
    let (h, m, sec) = match (days > 0 || s.contains('-'), parts.as_slice()) {
        // With a days prefix Slurm writes H:M:S; tolerate truncated forms.
        (true, [h]) => (*h, 0, 0),
        (true, [h, m]) => (*h, *m, 0),
        // Without days the shortest real form is min:sec.
        (false, [m, s]) => (0, *m, *s),
        (_, [h, m, s]) => (*h, *m, *s),
        _ => return None,
    };
    Some(Duration::from_secs(((days * 24 + h) * 60 + m) * 60 + sec))
}

/// `SystemTime` → `"YYYY-MM-DD HH:MM UTC"`, minute precision (walltime ends
/// are estimates; seconds would be false precision). Hand-rolled civil-date
/// conversion (Howard Hinnant's `civil_from_days`) because the workspace
/// deliberately carries no date-time dependency. Valid for any post-1970
/// time; `None` only for pre-epoch input.
fn format_utc_minute(t: SystemTime) -> Option<String> {
    let secs = t.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let (h, min) = ((secs % 86_400) / 3_600, (secs % 3_600) / 60);
    let z = secs / 86_400 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    Some(format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02} UTC"))
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

/// [`run_capped`] with a caller-chosen deadline, for callers whose child
/// legitimately outlives the 5s poll discipline.
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

/// Slurm's stderr/log text, made presentable: `<tool>: error: ` prefixes
/// stripped, ASCII ruler lines dropped, whitespace collapsed, length
/// capped. What remains is the admin-authored message — the most
/// cluster-specific, user-actionable text we will ever have (a detached
/// srun's refusals land in its log file; `compute_jobs` tails it through
/// this when a launch never reaches the queue).
pub(crate) fn clean_tool_stderr(raw: &str, tool: &str) -> String {
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
    fn parse_slurm_time_left_covers_squeue_forms() {
        // Real %L shapes: min:sec, h:min:sec, days-h:min:sec.
        assert_eq!(
            parse_slurm_time_left("9:54"),
            Some(Duration::from_secs(9 * 60 + 54))
        );
        assert_eq!(
            parse_slurm_time_left("8:00:00"),
            Some(Duration::from_secs(8 * 3600))
        );
        assert_eq!(
            parse_slurm_time_left("7-00:00:00"),
            Some(Duration::from_secs(7 * 86_400))
        );
        assert_eq!(
            parse_slurm_time_left("1-12:30:05"),
            Some(Duration::from_secs(86_400 + 12 * 3600 + 30 * 60 + 5))
        );
        // Truncated day forms (sinfo %l on some clusters) parse as H / H:M.
        assert_eq!(
            parse_slurm_time_left("2-12"),
            Some(Duration::from_secs(2 * 86_400 + 12 * 3600))
        );
        // Sentinels and ambiguous bare numbers stay None.
        for junk in ["UNLIMITED", "NOT_SET", "INVALID", "", "45", "a:b"] {
            assert_eq!(parse_slurm_time_left(junk), None, "{junk}");
        }
    }

    #[test]
    fn format_utc_minute_matches_date_u() {
        // Vectors verified with `date -u -r <epoch>` on this machine.
        let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(
            format_utc_minute(at(0)).as_deref(),
            Some("1970-01-01 00:00 UTC")
        );
        assert_eq!(
            format_utc_minute(at(1_784_118_840)).as_deref(),
            Some("2026-07-15 12:34 UTC")
        );
        // Leap-day, end of day (rollover boundaries).
        assert_eq!(
            format_utc_minute(at(1_709_251_140)).as_deref(),
            Some("2024-02-29 23:59 UTC")
        );
    }

    #[test]
    fn self_context_text_bakes_facts_and_absolute_end() {
        let alloc = SelfAllocation {
            job_id: "4242".into(),
            node: "sh03-01n52".into(),
            partition: "gpu".into(),
            state: "RUNNING".into(),
            time_left: "3:59:00".into(),
            cpus: "8".into(),
            mem: "64G".into(),
            gres: "gpu:1".into(),
        };
        // Bake at 2026-07-15 12:34 UTC; 3:59:00 left → ends 16:33 UTC.
        let now = UNIX_EPOCH + Duration::from_secs(1_784_118_840);
        let text = self_context_text(&alloc, now);
        assert!(text.contains("COMPUTE NODE"), "{text}");
        assert!(text.contains("Slurm job 4242 on node sh03-01n52"), "{text}");
        assert!(text.contains("(partition gpu)"), "{text}");
        assert!(text.contains("8 CPUs, 64G memory, gres gpu:1"), "{text}");
        assert!(
            text.contains("approximately 2026-07-15 16:33 UTC"),
            "{text}"
        );
        assert!(text.contains("killed then"), "{text}");
        assert!(text.contains("login node"), "{text}");

        // Sparse allocation (short squeue row): omitted fields never render
        // empty, and an unparseable time_left states the rule without an
        // invented end time.
        let sparse = SelfAllocation {
            job_id: "7".into(),
            node: String::new(),
            partition: String::new(),
            state: "RUNNING".into(),
            time_left: "UNLIMITED".into(),
            cpus: String::new(),
            mem: String::new(),
            gres: String::new(),
        };
        let text = self_context_text(&sparse, now);
        assert!(text.contains("(Slurm job 7)"), "{text}");
        assert!(!text.contains("The allocation provides"), "{text}");
        assert!(text.contains("ends at its walltime"), "{text}");
        assert!(!text.contains("approximately"), "{text}");
        assert!(!text.contains("  "), "no double spaces: {text}");
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
            ("srun", "#!/bin/sh\nexit 0\n"),
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

        // The agent context bakes from that self block, once: a second call
        // returns the SAME string (walltime end included — it must not
        // re-derive and drift).
        let ctx = svc.agent_context().await.expect("agent context");
        assert!(ctx.contains("Slurm job 4242 on node node7"), "{ctx}");
        assert!(ctx.contains("(partition gpu)"), "{ctx}");
        assert!(ctx.contains("approximately"), "{ctx}");
        assert_eq!(svc.agent_context().await.as_deref(), Some(ctx.as_str()));

        std::fs::remove_dir_all(&empty).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Off-cluster (no SLURM_JOB_ID at construction) the agent context is
    /// None without touching detection or the snapshot — the inertness
    /// guarantee for normal daemons (an empty bindir would otherwise make
    /// this probe the login shell).
    #[tokio::test]
    async fn agent_context_is_none_without_a_self_job() {
        let svc = ComputeService::with_bindir(PathBuf::from("/nonexistent"));
        assert_eq!(svc.agent_context().await, None);
        // Nothing was detected or cached as a side effect.
        assert!(svc.inner.lock().await.detection.is_none());
        assert!(svc.inner.lock().await.agent_context.is_none());
    }
}
