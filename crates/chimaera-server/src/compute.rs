//! Compute-scheduler awareness: detect Slurm on THIS host and serve a bounded
//! snapshot of the user's queue, plus — when this daemon IS a cluster
//! workspace job — its own allocation, the context its agents are told, and
//! the one verb it offers about itself (`DELETE /compute/self`). A daemon
//! detects its own scheduler locally, so the feature lights up on a cluster
//! and is a no-op on a laptop; nothing is probed over ssh.
//!
//! The scheduler vocabulary (row structs, parsers, the time grammar, format
//! strings) is `chimaera_core::slurm`, shared with the clients that drive a
//! cluster over ssh; this module owns only the daemon's processes and caches.
//!
//! Resource discipline is the design (shared clusters; schedulers are shared
//! too): detection runs once per daemon lifetime (`?refresh=true`
//! re-detects), every child process gets a hard kill-on-timeout and an output
//! cap, snapshots are cached 60 s (at least a minute between scheduler status
//! checks), the in-job `squeue -j` is floored at once a minute even across
//! refreshes, and concurrent requests coalesce on one refresh. Nothing is
//! persisted.
//!
//! Site-agnostic by rule: nothing here — code or the text agents are told —
//! names a cluster, a partition, a site command or a site path. What a
//! cluster is like comes from standard Slurm output and environment, or from
//! files the user pointed at.
//!
//! Test knob: `CHIMAERA_SLURM_BINDIR` points at a directory of stand-in
//! `scancel`/`squeue`/`sinfo` executables so the whole surface can be driven
//! live without a cluster (the `CHIMAERA_RELEASES_API` pattern).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chimaera_core::slurm::{self, Job, Partition};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;

pub(crate) use chimaera_core::slurm::is_live_state;

/// Hard runtime cap per scheduler command (squeue on a busy cluster can be
/// slow, but a wedged controller must never wedge the daemon).
const CMD_TIMEOUT: Duration = Duration::from_secs(5);
/// Output cap per command; queues are line-capped far below this anyway.
const MAX_OUTPUT: usize = 256 * 1024;
/// Snapshot TTL: at least a minute between scheduler status checks — the
/// etiquette shared clusters ask of every tool that polls their controller.
const SNAPSHOT_TTL: Duration = Duration::from_secs(60);
/// The in-job `squeue -j $SLURM_JOB_ID` is asked at most once per this, even
/// when `?refresh=true` drops the snapshot cache; in between, the last answer
/// is served with its time left aged locally.
const SELF_ASK_FLOOR: Duration = Duration::from_secs(60);
/// Rules-for-agents text up to this size rides the agent context verbatim;
/// a longer file is named instead, for the agent to read itself (the hook
/// carrier and codex's argv are not the place for a manual).
const RULES_INLINE_MAX: u64 = 12 * 1024;

/// A job that left the queue (or reached a terminal state) since the last
/// good snapshot — drained by the Timeline's job task.
#[derive(Clone, Debug)]
pub(crate) struct JobEnd {
    pub(crate) job: Job,
    /// The terminal state when squeue showed one, else "ENDED" (squeue
    /// forgets finished jobs; we never guess success).
    pub(crate) state: String,
}

/// Ended-job reports buffered for the Timeline, and ids already reported
/// (a job seen COMPLETING, then gone, must yield one entry, not two).
const ENDED_QUEUE_MAX: usize = 64;
const ENDED_SEEN_MAX: usize = 256;

#[derive(Default)]
struct EndedJobs {
    queue: std::collections::VecDeque<JobEnd>,
    reported: std::collections::VecDeque<String>,
}

impl EndedJobs {
    fn report(&mut self, job: &Job, state: &str) {
        if self.reported.iter().any(|id| id == &job.id) {
            return;
        }
        if self.reported.len() >= ENDED_SEEN_MAX {
            self.reported.pop_front();
        }
        self.reported.push_back(job.id.clone());
        if self.queue.len() >= ENDED_QUEUE_MAX {
            self.queue.pop_front();
        }
        self.queue.push_back(JobEnd {
            job: job.clone(),
            state: state.to_string(),
        });
    }
}

/// Diff two GOOD snapshots: jobs that vanished, and jobs that newly show a
/// terminal state. A vanish only means "ended" when `next` lists the whole
/// queue — past the `MAX_JOBS` cap a still-running job (a big array's
/// reshuffle) can simply fall off the page, and the Timeline can't retract.
fn note_transitions(ended: &mut EndedJobs, prev: &[Job], next: &[Job], next_complete: bool) {
    for old in prev {
        match next.iter().find(|j| j.id == old.id) {
            None if !next_complete => {}
            None => {
                let state = slurm::terminal_state(&old.state).unwrap_or("ENDED");
                ended.report(old, state);
            }
            Some(now) => {
                if let Some(state) = slurm::terminal_state(&now.state) {
                    ended.report(now, state);
                }
            }
        }
    }
}

/// The daemon's OWN allocation, when it runs inside a Slurm job (a cluster
/// workspace job): the window's bottom bar wears `time_left` — the honest
/// "this workspace lives until its time limit" indicator.
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
    /// The job is attached (held by the app that started it): it ends when
    /// that app disconnects, and can't continue in a new job.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) attached: bool,
}

impl SelfAllocation {
    /// This answer as it stands `elapsed` after squeue gave it: the time
    /// left counts down locally instead of asking the controller again.
    /// Sentinels (`UNLIMITED`, …) and unparseable values stay as they were.
    fn aged(mut self, elapsed: Duration) -> Self {
        if let Some(left) = slurm::parse_duration(&self.time_left) {
            self.time_left = slurm_clock(left.saturating_sub(elapsed));
        }
        self
    }
}

/// A duration in squeue's own `%L` shape — `M:SS`, `H:MM:SS`,
/// `D-HH:MM:SS` — so an aged self block reads like a fresh one.
fn slurm_clock(d: Duration) -> String {
    let s = d.as_secs();
    let (days, h, m, sec) = (s / 86_400, (s % 86_400) / 3_600, (s % 3_600) / 60, s % 60);
    if days > 0 {
        format!("{days}-{h:02}:{m:02}:{sec:02}")
    } else if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
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
    /// poll cycle (and must not read as every job having ended).
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

/// Where the scheduler binaries live once detected. The snapshot needs
/// squeue/sinfo and the in-job self-stop needs scancel — a cluster with a
/// partial toolset is not one we can read.
#[derive(Clone, Debug)]
pub(crate) enum Detection {
    None,
    Slurm {
        scancel: PathBuf,
        squeue: PathBuf,
        sinfo: PathBuf,
    },
}

/// [`ComputeService::detected`]: what the cached detection said, readable
/// without the async lock (the agent-context path must stay a single atomic
/// load off-cluster).
const DET_UNKNOWN: u8 = 0;
const DET_NONE: u8 = 1;
const DET_SLURM: u8 = 2;

pub struct ComputeService {
    /// One async lock covers detection + the snapshot cache: refreshes are
    /// single-flight (concurrent GETs await the first refresher and then
    /// read its cache), and nothing here is hot enough to shard.
    inner: tokio::sync::Mutex<Inner>,
    /// Test knob dir (`CHIMAERA_SLURM_BINDIR`), read once at construction.
    bindir: Option<PathBuf>,
    /// Set when THIS daemon runs inside a Slurm allocation: `SLURM_JOB_ID`
    /// at construction. Drives the snapshot's `self` block, the in-job agent
    /// context and `DELETE /compute/self`.
    self_job: Option<String>,
    /// This daemon is a cluster workspace job: in a Slurm allocation AND
    /// started by a workspace job script (`CHIMAERA_CLUSTER_WORKSPACE`).
    cluster_job: bool,
    /// The user said this host isn't a cluster (`CHIMAERA_NOT_A_CLUSTER`,
    /// set by connect): Slurm on its PATH doesn't make it a login node.
    not_a_cluster: bool,
    /// Mirror of the cached detection (`DET_*`), updated wherever detection
    /// is (re)run. Unknown until the first `/compute` (every window asks once
    /// at boot) — never triggered from the agent-context path.
    detected: AtomicU8,
    /// Jobs that ended between good snapshots, for the Timeline (std lock:
    /// never held across an await).
    ended: std::sync::Mutex<EndedJobs>,
}

#[derive(Default)]
struct Inner {
    detection: Option<Detection>,
    cache: Option<(Instant, ComputeSnapshot)>,
    /// When the in-job self block was last asked, and what squeue said
    /// (None = the call failed or the job wasn't listed) — the
    /// [`SELF_ASK_FLOOR`] gate.
    self_asked: Option<(Instant, Option<SelfAllocation>)>,
    /// The rendered in-job agent context, baked once per daemon lifetime
    /// (see [`ComputeService::agent_context`]).
    agent_context: Option<String>,
}

impl ComputeService {
    pub(crate) fn new() -> Self {
        let self_job = env_nonempty("SLURM_JOB_ID");
        let cluster_job = self_job.is_some()
            && env_nonempty(chimaera_core::cluster::ENV_CLUSTER_WORKSPACE).is_some();
        ComputeService {
            inner: tokio::sync::Mutex::new(Inner::default()),
            bindir: std::env::var_os("CHIMAERA_SLURM_BINDIR").map(PathBuf::from),
            self_job,
            cluster_job,
            not_a_cluster: env_nonempty(chimaera_core::cluster::ENV_NOT_A_CLUSTER).is_some(),
            detected: AtomicU8::new(DET_UNKNOWN),
            ended: std::sync::Mutex::new(EndedJobs::default()),
        }
    }

    #[cfg(any(test, feature = "daemon-extension-fixture"))]
    pub(crate) fn with_bindir(dir: PathBuf) -> Self {
        ComputeService {
            inner: tokio::sync::Mutex::new(Inner::default()),
            bindir: Some(dir),
            self_job: None,
            cluster_job: false,
            not_a_cluster: false,
            detected: AtomicU8::new(DET_UNKNOWN),
            ended: std::sync::Mutex::new(EndedJobs::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_bindir_and_job(dir: PathBuf, job: &str) -> Self {
        ComputeService {
            self_job: Some(job.to_string()),
            ..Self::with_bindir(dir)
        }
    }

    /// Whether this daemon runs as a cluster workspace job — a job may start
    /// hours after the click that queued it, with nobody watching (the
    /// restart pick-up message is withheld there).
    pub(crate) fn is_cluster_job(&self) -> bool {
        self.cluster_job
    }

    /// The detected scheduler toolset; runs detection if it hasn't happened
    /// yet. A TRANSIENT probe failure (the login shell timing out under
    /// load) is not cached — the next call retries — while a definitive "no
    /// tools" answer is; otherwise one slow boot-time profile would brand a
    /// real cluster "no scheduler" for the daemon's whole lifetime (the UI
    /// then stops polling and never asks again).
    pub(crate) async fn detection(&self) -> Detection {
        let mut inner = self.inner.lock().await;
        if inner.detection.is_none() {
            inner.detection = self.detect().await;
        }
        inner.detection.clone().unwrap_or(Detection::None)
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
            // None (transient probe failure) stays uncached: this round
            // answers "none" but the next call re-probes — see detection().
            inner.detection = self.detect().await;
        }
        let (squeue, sinfo) = match &inner.detection {
            None | Some(Detection::None) => return ComputeSnapshot::none(),
            Some(Detection::Slurm { squeue, sinfo, .. }) => (squeue.clone(), sinfo.clone()),
        };
        if let Some((at, snap)) = &inner.cache {
            if at.elapsed() < SNAPSHOT_TTL {
                return snap.clone();
            }
        }
        // The self block has its own floor: a refresh drops the snapshot
        // cache, never the once-a-minute promise for `squeue -j`.
        let ask_self = self.self_job.as_deref().filter(|_| {
            inner
                .self_asked
                .as_ref()
                .is_none_or(|(at, _)| at.elapsed() >= SELF_ASK_FLOOR)
        });
        let asked_at = Instant::now();
        // Holding the lock across the fetch IS the single-flight: concurrent
        // requests queue here briefly instead of stampeding the controller.
        let mut snap = fetch_snapshot(&squeue, &sinfo, ask_self).await;
        if let Some(own) = snap.self_alloc.as_mut() {
            own.attached = env_nonempty(chimaera_core::cluster::ENV_JOB_ATTACHED).is_some();
        }
        if ask_self.is_some() {
            inner.self_asked = Some((asked_at, snap.self_alloc.clone()));
        } else if let Some((at, alloc)) = &inner.self_asked {
            snap.self_alloc = alloc.clone().map(|a| a.aged(at.elapsed()));
        }
        // Only two GOOD reads say a job ended: a failed squeue carries the
        // old jobs forward and must never read as "everything finished".
        if !snap.degraded {
            if let Some((_, prev)) = &inner.cache {
                if !prev.degraded {
                    note_transitions(
                        &mut crate::lock(&self.ended),
                        &prev.jobs,
                        &snap.jobs,
                        !snap.truncated,
                    );
                }
            }
        }
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

    /// Jobs that ended since the last drain (the Timeline's job task).
    pub(crate) fn drain_ended(&self) -> Vec<JobEnd> {
        crate::lock(&self.ended).queue.drain(..).collect()
    }

    /// The cached snapshot WITHOUT waiting: a refresh in flight holds the
    /// lock across a squeue (up to its deadline), and a status answer must
    /// not stall behind it — None means "refreshing or never fetched".
    pub(crate) fn peek(&self) -> Option<(Instant, ComputeSnapshot)> {
        let inner = self.inner.try_lock().ok()?;
        inner.cache.clone()
    }

    /// What this daemon's agents are told about where they run, delivered
    /// once per session (claude: the hook response, `agents::ingest`; codex
    /// chat: `developer_instructions`). `None` for a normal daemon.
    ///
    /// - Inside a Slurm job: the job, its node and resources, its absolute
    ///   end, how to use it, and the cluster's rules for agents.
    /// - Not in a job, on a host where Slurm was detected (a login-node
    ///   daemon the user allowed): the short login-node version — unless
    ///   the user said the host isn't a cluster.
    ///
    /// Off-cluster this is one `Option` check and one atomic load — no lock,
    /// no subprocess, no file read: the login-node branch reads only the
    /// CACHED detection (which every window's first `/compute` fills) and
    /// never runs one itself.
    pub(crate) async fn agent_context(&self) -> Option<String> {
        if self.self_job.is_none() {
            return (!self.not_a_cluster && self.detected.load(Ordering::Acquire) == DET_SLURM)
                .then(|| LOGIN_NODE_CONTEXT.to_string());
        }
        // Baked ONCE per daemon lifetime, at first use: the allocation's
        // facts are constant for the job's life, and the end is stored as
        // an ABSOLUTE instant (bake-time now + squeue's time left) rather
        // than re-derived per spawn — one bake keeps every session's text
        // identical, and a relative "3:59 left" would go stale in the
        // transcript.
        if let Some(ctx) = &self.inner.lock().await.agent_context {
            return Some(ctx.clone());
        }
        // Not baked yet. The snapshot is cached + single-flight (worst case
        // one 5 s-capped squeue); a failed squeue yields no self block, and
        // the bake simply retries on a later call rather than caching an
        // absence forever.
        let alloc = self.snapshot(false).await.self_alloc?;
        let (rules, facts) =
            tokio::task::spawn_blocking(|| (AgentRules::from_env(), read_cluster_facts()))
                .await
                .unwrap_or_default();
        let text = job_context_text(
            &alloc,
            env_nonempty("SLURM_CLUSTER_NAME").as_deref(),
            SystemTime::now(),
            &rules,
            facts.as_ref(),
        );
        let mut inner = self.inner.lock().await;
        // get_or_insert: a concurrent first bake wins and both callers hand
        // out the SAME string (the two candidates differ only by seconds).
        Some(inner.agent_context.get_or_insert(text).clone())
    }

    /// `DELETE /compute/self`: end this daemon's own job. Slurm then
    /// SIGTERMs the daemon, whose graceful stop saves the ledger — the
    /// workspace's chats come back with its next job.
    async fn cancel_self(&self) -> (StatusCode, serde_json::Value) {
        // Digits only (an array task's id is still numeric in SLURM_JOB_ID):
        // it lands in argv, and a dash-led value would read as an option.
        let Some(job) = self
            .self_job
            .as_deref()
            .filter(|j| !j.is_empty() && j.bytes().all(|b| b.is_ascii_digit()))
        else {
            return (
                StatusCode::NOT_FOUND,
                json!({"error": "this daemon does not run inside a Slurm job"}),
            );
        };
        let Detection::Slurm { scancel, .. } = self.detection().await else {
            return (
                StatusCode::CONFLICT,
                json!({"error": "no Slurm tools found on this node"}),
            );
        };
        // A job already on its way out answers "Invalid job id" — the stop
        // asked for has happened, so that is success too.
        match run_checked(&scancel.to_string_lossy(), &[job.to_string()]).await {
            Some(out) if out.success || out.stderr.contains("Invalid job id") => {
                tracing::info!(job, "stop of this daemon's own job requested");
                (StatusCode::ACCEPTED, json!({"job_id": job}))
            }
            Some(out) => (
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("scancel: {}", slurm::clean_tool_stderr(&out.stderr, "scancel"))}),
            ),
            None => (
                StatusCode::BAD_GATEWAY,
                json!({"error": "scancel did not answer (controller busy?) — the job was NOT stopped"}),
            ),
        }
    }

    /// Detect, mirroring the verdict into [`Self::detected`].
    async fn detect(&self) -> Option<Detection> {
        let found = detect_tools(self.bindir.as_deref()).await;
        let mirror = match &found {
            Some(Detection::Slurm { .. }) => DET_SLURM,
            Some(Detection::None) => DET_NONE,
            None => DET_UNKNOWN,
        };
        self.detected.store(mirror, Ordering::Release);
        found
    }
}

/// Find the Slurm client tools. The knob dir wins (tests / unusual
/// installs); otherwise ask the user's login shell for its PATH and walk it
/// here — the profile-managed-PATH reasoning of the git resolution, WITHOUT
/// `command -v`: some clusters wrap the tools in profile shell functions
/// (`command -v squeue` then prints the bare function name, not a path), and
/// a PATH walk is also the only form that works identically under
/// bash/zsh/fish. `None` = the probe itself failed (login shell timed out —
/// transient, don't cache); `Some(Detection::None)` = the shell answered and
/// the tools are genuinely absent (definitive, cache it).
pub(crate) async fn detect_tools(bindir: Option<&Path>) -> Option<Detection> {
    const TOOLS: [&str; 3] = ["scancel", "squeue", "sinfo"];
    if let Some(dir) = bindir {
        let tools = TOOLS.map(|n| dir.join(n));
        if tools.iter().all(|p| p.is_file()) {
            tracing::info!(dir = %dir.display(), "slurm tools from CHIMAERA_SLURM_BINDIR");
            let [scancel, squeue, sinfo] = tools;
            return Some(Detection::Slurm {
                scancel,
                squeue,
                sinfo,
            });
        }
        tracing::warn!(dir = %dir.display(), "CHIMAERA_SLURM_BINDIR set but scancel/squeue/sinfo not all present");
        return Some(Detection::None);
    }
    let shell = chimaera_core::login_shell();
    let mut cmd = tokio::process::Command::new(&shell);
    cmd.args(["-lc", "printf %s \"$PATH\""]);
    // A login shell in its own session, killed whole: rc helpers must not
    // outlive the probe (see `process::probe_output`).
    let out = crate::process::probe_output(&mut cmd, CMD_TIMEOUT)
        .await
        .filter(|out| out.success)
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
    let Some(path) = out else {
        tracing::warn!("login-shell PATH probe failed; scheduler detection will retry");
        return None;
    };
    // The walk stats PATH entries that may live on slow network mounts —
    // off the reactor with it (detection runs once per daemon lifetime).
    let found = tokio::task::spawn_blocking(move || TOOLS.map(|n| find_on_path(path.trim(), n)))
        .await
        .unwrap_or([None, None, None]);
    match found {
        [Some(scancel), Some(squeue), Some(sinfo)] => {
            tracing::info!(squeue = %squeue.display(), "slurm detected");
            Some(Detection::Slurm {
                scancel,
                squeue,
                sinfo,
            })
        }
        _ => Some(Detection::None),
    }
}

/// Whether Slurm job `job` is still queued or running: `squeue -h -j <id>
/// -o %T`, one bounded call. `Some(false)` when the controller no longer
/// lists it (finished long ago: "Invalid job id"; recently: a terminal
/// state, or nothing), `None` when the question went unanswered (timeout,
/// any other failure) — the caller keeps waiting rather than guessing.
pub(crate) async fn job_is_live(squeue: &Path, job: &str) -> Option<bool> {
    let out = run_checked(
        &squeue.to_string_lossy(),
        &[
            "-h".into(),
            "-j".into(),
            job.to_string(),
            "-o".into(),
            "%T".into(),
        ],
    )
    .await?;
    if !out.success {
        return out.stderr.contains("Invalid job id").then_some(false);
    }
    Some(
        out.stdout
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .is_some_and(is_live_state),
    )
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
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// One refresh: the user's queue, the partitions, and — when `self_job` is
/// given (the floor allows it) — the daemon's own allocation.
async fn fetch_snapshot(squeue: &Path, sinfo: &Path, self_job: Option<&str>) -> ComputeSnapshot {
    // The daemon's own allocation, when it has one: one bounded squeue -j.
    let self_fut = async {
        match self_job {
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
        }
    };
    // `-u <user>`, not `--me`: --me is newer than the Slurm versions real
    // clusters still run. No USER (exotic) → skip the queue rather than
    // listing the whole cluster's jobs.
    let jobs_fut = async {
        match std::env::var("USER") {
            Ok(user) => {
                run_capped(
                    &squeue.to_string_lossy(),
                    &[
                        "-u".into(),
                        user,
                        "--noheader".into(),
                        "-o".into(),
                        slurm::SQUEUE_FORMAT.into(),
                    ],
                )
                .await
            }
            Err(_) => None,
        }
    };
    let parts_fut = async {
        run_capped(
            &sinfo.to_string_lossy(),
            &["--noheader".into(), "-o".into(), slurm::SINFO_FORMAT.into()],
        )
        .await
    };
    // Independent controller queries; running them together caps the
    // single-flight lock hold at ONE command deadline instead of stacking
    // three (a wedged controller otherwise blocks every /compute caller for
    // the sum).
    let (self_alloc, jobs_out, parts_out) = tokio::join!(self_fut, jobs_fut, parts_fut);

    let (jobs, jobs_truncated) = slurm::parse_squeue(jobs_out.as_deref().unwrap_or(""));
    let (partitions, parts_truncated) = slurm::parse_sinfo(parts_out.as_deref().unwrap_or(""));
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
        gres: if gres.eq_ignore_ascii_case("n/a") || gres.eq_ignore_ascii_case("(null)") {
            String::new()
        } else {
            gres
        },
        attached: false,
    })
}

/// What a login-node daemon's agents are told (a daemon the user allowed on
/// a cluster's login node, not in a job). Short on purpose: the point is
/// where they are and what that place is for.
const LOGIN_NODE_CONTEXT: &str = "\
You are running on a shared login node of a Slurm cluster, not inside a job. \
Login nodes are for light work: editing, inspecting files and preparing jobs. \
Submit anything heavy (builds, test suites, data processing, long runs) as a job \
with sbatch and an explicit --time, and check on it at most once a minute. Leave \
nothing running here: no background processes, servers or polling loops.";

/// Used when the cluster has no rules for agents of its own (neither a rules
/// file on the cluster nor text from the user).
const GENERIC_RULES: &str = "\
Rules for agents on shared clusters: every job you submit states an explicit \
--time. Check the queue no more than once a minute, and never in a loop. Leave \
nothing running on login nodes. Never start anything that keeps itself alive or \
resubmits itself.";

/// The cluster's Slurm setup as the app discovered it, from the job's
/// `CHIMAERA_CLUSTER_FACTS_FILE`. Blocking (a shared filesystem): run off
/// the reactor.
fn read_cluster_facts() -> Option<chimaera_core::cluster::ClusterFacts> {
    let path = env_nonempty(chimaera_core::cluster::ENV_CLUSTER_FACTS_FILE)?;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(256 * 1024).read_to_end(&mut bytes).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// What agents may submit to: one line per partition that is up (at most
/// 12), its time limit and largest node, then accounts and the GPU flag.
fn facts_text(f: &chimaera_core::cluster::ClusterFacts) -> Option<String> {
    const ROWS: usize = 12;
    let up: Vec<&slurm::PartitionChoice> = f.partitions.iter().filter(|p| p.up).collect();
    if up.is_empty() {
        return None;
    }
    let mut t = String::from(
        "What you can submit to on this cluster (partition — time limit — largest node):",
    );
    for p in up.iter().take(ROWS) {
        let time = if p.max_time.eq_ignore_ascii_case("UNLIMITED") || p.max_time.is_empty() {
            "no limit".to_string()
        } else {
            p.max_time.clone()
        };
        let mut node: Vec<String> = Vec::new();
        let cpus = p.cpus_per_node.trim_end_matches('+');
        if !cpus.is_empty() {
            node.push(format!("{cpus} CPUs"));
        }
        if let Some(mem) = mem_words(&p.mem_per_node) {
            node.push(mem);
        }
        if p.gpus {
            node.push("GPUs".to_string());
        }
        let mut line = format!(
            "\n- {}{} — {time}",
            p.name,
            if p.default { " (default)" } else { "" }
        );
        if !node.is_empty() {
            line.push_str(&format!(" — {}", node.join(", ")));
        }
        if p.preemptible {
            line.push_str(" — can be preempted");
        }
        t.push_str(&line);
    }
    if up.len() > ROWS {
        t.push_str(&format!(
            "\n- and {} more (sinfo lists them)",
            up.len() - ROWS
        ));
    }
    match (&f.default_account, f.accounts.as_slice()) {
        (_, []) => t.push_str("\nNo --account is needed here."),
        (Some(d), accounts) => t.push_str(&format!(
            "\nAccounts: {} (default {d}); pass --account=… for another.",
            accounts.join(", ")
        )),
        (None, accounts) => t.push_str(&format!(
            "\nPass --account= one of: {}.",
            accounts.join(", ")
        )),
    }
    Some(t)
}

/// sinfo's `%m` (megabytes, maybe `+`-suffixed) in plain words.
fn mem_words(raw: &str) -> Option<String> {
    let mb: u64 = raw.trim_end_matches('+').parse().ok()?;
    Some(if mb >= 1000 {
        format!("{} GB", (mb + 500) / 1000)
    } else {
        format!("{mb} MB")
    })
}

/// One rules-for-agents source, as read at bake time.
#[derive(Clone, Debug, PartialEq)]
enum RulesText {
    /// Short enough to ride the context verbatim.
    Inline(String),
    /// Too long to inline: the agent is told to read it.
    TooLong(PathBuf),
}

/// The cluster's rules for agents, from the workspace job's environment:
/// a file on the cluster the user pointed at (`CHIMAERA_AGENT_RULES_SOURCE`,
/// read now so it is always the cluster's current text) and the user's own
/// text (`CHIMAERA_AGENT_RULES_FILE`).
#[derive(Clone, Debug, Default, PartialEq)]
struct AgentRules {
    source: Option<RulesText>,
    user: Option<RulesText>,
}

impl AgentRules {
    /// Blocking (file reads on a shared filesystem): run off the reactor.
    fn from_env() -> Self {
        let read = |var: &str| env_nonempty(var).and_then(|p| read_rules(Path::new(&p)));
        AgentRules {
            source: read(chimaera_core::cluster::ENV_AGENT_RULES_SOURCE),
            user: read(chimaera_core::cluster::ENV_AGENT_RULES_FILE),
        }
    }
}

/// A rules file, read with a cap: `None` unless it is a readable regular
/// file with something in it.
fn read_rules(path: &Path) -> Option<RulesText> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        tracing::warn!(path = %path.display(), "rules for agents: not a regular file; skipped");
        return None;
    }
    let file = std::fs::File::open(path)
        .inspect_err(|err| {
            tracing::warn!(path = %path.display(), %err, "rules for agents: unreadable; skipped");
        })
        .ok()?;
    let mut bytes = Vec::new();
    file.take(RULES_INLINE_MAX + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > RULES_INLINE_MAX {
        return Some(RulesText::TooLong(path.to_path_buf()));
    }
    let text = String::from_utf8_lossy(&bytes).trim().to_string();
    (!text.is_empty()).then_some(RulesText::Inline(text))
}

/// Render the in-job agent context for one [`SelfAllocation`]: where the
/// agent runs (which nothing else tells it — it may assume a login node),
/// what the allocation provides, when it ends, how to use it, and the
/// cluster's rules for agents. `now` is the bake time (threaded in so tests
/// can pin it); the end lands as an absolute UTC instant. Missing fields
/// (older squeue rows degrade to "") are omitted, never rendered empty.
fn job_context_text(
    alloc: &SelfAllocation,
    cluster: Option<&str>,
    now: SystemTime,
    rules: &AgentRules,
    facts: Option<&chimaera_core::cluster::ClusterFacts>,
) -> String {
    let mut place = format!("Slurm job {}", alloc.job_id);
    if !alloc.node.is_empty() {
        place.push_str(&format!(" on node {}", alloc.node));
    }
    let mut known = Vec::new();
    if let Some(cluster) = cluster.filter(|c| !c.is_empty()) {
        known.push(format!("cluster {cluster}"));
    }
    if !alloc.partition.is_empty() {
        known.push(format!("partition {}", alloc.partition));
    }
    if !known.is_empty() {
        place.push_str(&format!(" ({})", known.join(", ")));
    }
    let mut resources = Vec::new();
    if !alloc.cpus.is_empty() {
        resources.push(format!("{} CPUs", alloc.cpus));
    }
    if !alloc.mem.is_empty() {
        resources.push(format!("{} of memory", alloc.mem));
    }
    if !alloc.gres.is_empty() {
        resources.push(alloc.gres.clone());
    }
    let resources = match resources.split_last() {
        None => String::new(),
        Some((last, [])) => format!(" with {last}"),
        Some((last, rest)) => format!(" with {} and {last}", rest.join(", ")),
    };
    // "UNLIMITED"/"NOT_SET"/noise parse to None: state the rule without
    // inventing an end time.
    let ends = match slurm::parse_duration(&alloc.time_left)
        .and_then(|left| slurm::format_utc_minute(now + left))
    {
        Some(end) => format!("around {end}"),
        None => "at its time limit".to_string(),
    };
    // A chimaera job (its job-host passes the cluster's facts) may host
    // several workspaces; a daemon started by hand in an allocation is alone.
    let shared = if facts.is_some() {
        " Other workspaces may be open in this job and share its resources."
    } else {
        ""
    };
    let gpu_arg = match facts {
        Some(f) if f.partitions.iter().any(|p| p.gpus) => match f.gpu_flag {
            slurm::GpuFlag::Gpus => " [--gpus=N]",
            slurm::GpuFlag::Gres => " [--gres=gpu:N]",
        },
        _ => "",
    };
    let mut text = format!(
        "You are running inside {place}{resources}. The job ends {ends}; anything \
         still running then is stopped.{shared}\n\n\
         - Commands you run execute inside this job. Use its CPUs and memory fully \
         (match thread counts to $SLURM_CPUS_PER_TASK).\n\
         - For work that needs more time than is left, more resources, or a GPU \
         this job doesn't have, submit your own job with sbatch and an explicit \
         --time; it keeps running after this one ends: `sbatch --time=… \
         [--partition=…] [--cpus-per-task=…] [--mem=…]{gpu_arg} job.sh`. Chain steps \
         with --dependency=afterok:<id>. Check on your jobs at most once a minute \
         (squeue -j <id>, sacct -j <id>), never in a loop, or tell the user the job \
         id and stop."
    );
    if let Some(table) = facts.and_then(facts_text) {
        text.push_str("\n\n");
        text.push_str(&table);
    }
    let mut any_rules = false;
    if let Some(source) = &rules.source {
        any_rules = true;
        text.push_str("\n\n");
        match source {
            RulesText::Inline(rules) => {
                text.push_str("This cluster's rules for agents:\n");
                text.push_str(rules);
            }
            RulesText::TooLong(path) => text.push_str(&format!(
                "This cluster's rules for agents are in {}. Read that file before \
                 doing significant work.",
                path.display()
            )),
        }
    }
    if let Some(user) = &rules.user {
        any_rules = true;
        text.push_str("\n\n");
        match user {
            RulesText::Inline(rules) => {
                text.push_str("The user's rules for agents on this cluster:\n");
                text.push_str(rules);
            }
            RulesText::TooLong(path) => text.push_str(&format!(
                "The user's rules for agents on this cluster are in {}. Read that \
                 file before doing significant work.",
                path.display()
            )),
        }
    }
    if !any_rules {
        text.push_str("\n\n");
        text.push_str(GENERIC_RULES);
    }
    text
}

/// One child's outcome under the poll discipline. `None` from
/// [`run_checked`] means the child never ran to completion (spawn failure
/// or the deadline); an exit is always `Some` — success or not — with both
/// streams captured (capped), so callers can tell "the tool said no" from
/// "the tool never answered" (scancel of an already-gone job fails with
/// words, a wedged controller fails silently).
pub(crate) struct CmdResult {
    pub(crate) success: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// Run one child with the default timeout + output cap; None on spawn
/// failure, non-zero exit, or timeout (the caller degrades, never errors).
pub(crate) async fn run_capped(bin: &str, args: &[String]) -> Option<String> {
    let out = run_checked(bin, args).await?;
    out.success.then_some(out.stdout)
}

/// [`run_capped`]'s status-aware form: a completed child is `Some` even on
/// non-zero exit.
pub(crate) async fn run_checked(bin: &str, args: &[String]) -> Option<CmdResult> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Timeout drops the future, dropping the child; this reaps it.
        .kill_on_drop(true);
    let out = tokio::time::timeout(CMD_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    Some(CmdResult {
        success: out.status.success(),
        stdout: capped_lossy(out.stdout),
        stderr: capped_lossy(out.stderr),
    })
}

/// Byte-capped lossy decode. The BYTES are truncated first: cutting a
/// decoded `String` at a fixed byte offset panics off a char boundary
/// (a multi-byte job name straddling the cap would take the request task
/// down), while a byte cut costs at most a trailing U+FFFD.
fn capped_lossy(mut bytes: Vec<u8>) -> String {
    if bytes.len() > MAX_OUTPUT {
        bytes.truncate(MAX_OUTPUT);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn env_nonempty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.trim().is_empty())
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

/// DELETE /api/v1/compute/self — stop this daemon's own Slurm job (202), or
/// 404 when the daemon doesn't run inside one.
pub(crate) async fn delete_self(State(state): State<Arc<AppState>>) -> Response {
    let (status, body) = state.compute.cancel_self().await;
    (status, Json(body)).into_response()
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

    /// Write executable stand-ins into `dir`.
    fn tools(dir: &Path, scripts: &[(&str, &str)]) {
        for (name, body) in scripts {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }

    /// `attached` is additive: absent unless set, so the window's offer to
    /// continue is withheld only for an attached job.
    #[test]
    fn the_self_block_says_attached_only_when_it_is() {
        let plain = serde_json::to_value(alloc()).unwrap();
        assert!(plain.get("attached").is_none());
        let held = serde_json::to_value(SelfAllocation {
            attached: true,
            ..alloc()
        })
        .unwrap();
        assert_eq!(held["attached"], true);
    }

    fn alloc() -> SelfAllocation {
        SelfAllocation {
            job_id: "4242".into(),
            node: "n042".into(),
            partition: "batch".into(),
            state: "RUNNING".into(),
            time_left: "3:59:00".into(),
            cpus: "8".into(),
            mem: "64G".into(),
            gres: "gpu:1".into(),
            attached: false,
        }
    }

    #[test]
    fn ended_jobs_are_reported_once_and_only_from_good_reads() {
        let job = |id: &str, state: &str| Job {
            id: id.into(),
            name: format!("job{id}"),
            partition: "p".into(),
            state: state.into(),
            time_left: String::new(),
            nodes: String::new(),
            cpus: String::new(),
            mem: String::new(),
            elapsed: "1:00".into(),
            workdir: "/w".into(),
            reason: String::new(),
        };
        let mut ended = EndedJobs::default();
        let a = vec![
            job("1", "RUNNING"),
            job("2", "RUNNING"),
            job("3", "PENDING"),
        ];
        let b = vec![job("2", "COMPLETING"), job("3", "FAILED")];
        note_transitions(&mut ended, &a, &b, true);
        let states: Vec<(String, String)> = ended
            .queue
            .iter()
            .map(|e| (e.job.id.clone(), e.state.clone()))
            .collect();
        assert_eq!(
            states,
            vec![("1".into(), "ENDED".into()), ("3".into(), "FAILED".into())]
        );
        // 3 vanishing next time must not report twice; 2 finishing does.
        let c: Vec<Job> = Vec::new();
        note_transitions(&mut ended, &b, &c, true);
        assert_eq!(ended.queue.len(), 3);
        assert_eq!(ended.queue[2].job.id, "2");
        // A truncated read (past MAX_JOBS) proves nothing about the jobs it
        // no longer lists — only a reported terminal state still counts.
        let mut ended = EndedJobs::default();
        let d = vec![job("7", "RUNNING"), job("8", "RUNNING")];
        note_transitions(&mut ended, &a, &d, false);
        assert!(ended.queue.is_empty());
        note_transitions(&mut ended, &d, &[job("8", "TIMEOUT")], false);
        assert_eq!(ended.queue.len(), 1);
        assert_eq!(ended.queue[0].job.id, "8");
    }

    #[test]
    fn job_context_bakes_facts_absolute_end_and_generic_rules() {
        // Bake at 2026-07-15 12:34 UTC; 3:59:00 left → ends 16:33 UTC.
        let now = UNIX_EPOCH + Duration::from_secs(1_784_118_840);
        let text = job_context_text(&alloc(), Some("c1"), now, &AgentRules::default(), None);
        assert!(
            text.starts_with(
                "You are running inside Slurm job 4242 on node n042 (cluster c1, \
                 partition batch) with 8 CPUs, 64G of memory and gpu:1."
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "The job ends around 2026-07-15 16:33 UTC; anything still running then is stopped."
            ),
            "{text}"
        );
        assert!(text.contains("$SLURM_CPUS_PER_TASK"), "{text}");
        assert!(text.contains("sbatch and an explicit --time"), "{text}");
        assert!(text.contains("--dependency"), "{text}");
        // No rules of the cluster's own → the generic paragraph.
        assert!(text.ends_with(GENERIC_RULES), "{text}");

        // Sparse allocation (short squeue row): omitted fields never render
        // empty, and an unparseable time left states the rule without an
        // invented end time.
        let sparse = SelfAllocation {
            job_id: "7".into(),
            node: String::new(),
            partition: String::new(),
            state: "RUNNING".into(),
            time_left: "UNLIMITED".into(),
            cpus: "2".into(),
            mem: String::new(),
            gres: String::new(),
            attached: false,
        };
        let text = job_context_text(&sparse, None, now, &AgentRules::default(), None);
        assert!(
            text.starts_with("You are running inside Slurm job 7 with 2 CPUs."),
            "{text}"
        );
        assert!(text.contains("The job ends at its time limit;"), "{text}");
        assert!(!text.contains("around"), "{text}");
        assert!(!text.contains("  "), "no double spaces: {text}");
    }

    #[test]
    fn job_context_appends_the_clusters_rules_instead_of_the_generic_ones() {
        let now = UNIX_EPOCH + Duration::from_secs(1_784_118_840);
        let rules = AgentRules {
            source: Some(RulesText::Inline("No jobs over a day.".into())),
            user: Some(RulesText::Inline("Use the scratch space.".into())),
        };
        let text = job_context_text(&alloc(), None, now, &rules, None);
        assert!(
            text.contains("This cluster's rules for agents:\nNo jobs over a day.\n\nThe user's rules for agents on this cluster:\nUse the scratch space."),
            "{text}"
        );
        assert!(!text.contains(GENERIC_RULES), "{text}");

        let long = AgentRules {
            source: Some(RulesText::TooLong(PathBuf::from("/shared/rules.md"))),
            user: None,
        };
        let text = job_context_text(&alloc(), None, now, &long, None);
        assert!(
            text.ends_with(
                "This cluster's rules for agents are in /shared/rules.md. Read that file \
                 before doing significant work."
            ),
            "{text}"
        );
        assert!(!text.contains(GENERIC_RULES), "{text}");
    }

    #[test]
    fn a_chimaera_job_tells_agents_what_they_can_submit_to() {
        use chimaera_core::cluster::ClusterFacts;
        let now = UNIX_EPOCH + Duration::from_secs(1_784_118_840);
        let part = |name: &str, default: bool, time: &str, gpus: bool, preempt: bool| {
            slurm::PartitionChoice {
                name: name.into(),
                default,
                max_time: time.into(),
                max_time_secs: None,
                cpus_per_node: "64+".into(),
                mem_per_node: "256000".into(),
                gpus,
                preemptible: preempt,
                up: true,
                accounts: Vec::new(),
            }
        };
        let facts = ClusterFacts {
            partitions: vec![
                part("batch", true, "3-00:00:00", false, false),
                part("accel", false, "1-12:00:00", true, true),
                slurm::PartitionChoice {
                    up: false,
                    ..part("down", false, "1:00:00", false, false)
                },
            ],
            accounts: vec!["lab".into(), "other".into()],
            default_account: Some("lab".into()),
            ..Default::default()
        };
        let text = job_context_text(&alloc(), None, now, &AgentRules::default(), Some(&facts));
        assert!(
            text.contains("still running then is stopped. Other workspaces may be open in this job and share its resources."),
            "{text}"
        );
        assert!(text.contains("[--mem=…] [--gpus=N] job.sh"), "{text}");
        assert!(
            text.contains("- batch (default) — 3-00:00:00 — 64 CPUs, 256 GB\n- accel — 1-12:00:00 — 64 CPUs, 256 GB, GPUs — can be preempted"),
            "{text}"
        );
        assert!(
            !text.contains("- down"),
            "a partition that is down isn't offered: {text}"
        );
        assert!(
            text.contains("Accounts: lab, other (default lab)"),
            "{text}"
        );
        assert!(text.ends_with(GENERIC_RULES), "{text}");

        let alone = job_context_text(&alloc(), None, now, &AgentRules::default(), None);
        assert!(!alone.contains("Other workspaces"), "{alone}");
        assert!(!alone.contains("--gpus=N"), "{alone}");
    }

    #[test]
    fn rules_files_inline_under_the_cap_and_point_past_it() {
        let dir = test_dir("rules");
        let short = dir.join("short.md");
        std::fs::write(&short, "  Be kind to the scheduler.\n").unwrap();
        assert_eq!(
            read_rules(&short),
            Some(RulesText::Inline("Be kind to the scheduler.".into()))
        );
        let exact = dir.join("exact.md");
        std::fs::write(&exact, "x".repeat(RULES_INLINE_MAX as usize)).unwrap();
        assert!(matches!(read_rules(&exact), Some(RulesText::Inline(_))));
        let long = dir.join("long.md");
        std::fs::write(&long, "x".repeat(RULES_INLINE_MAX as usize + 1)).unwrap();
        assert_eq!(read_rules(&long), Some(RulesText::TooLong(long.clone())));
        let blank = dir.join("blank.md");
        std::fs::write(&blank, " \n\n").unwrap();
        assert_eq!(read_rules(&blank), None, "nothing to say");
        assert_eq!(read_rules(&dir), None, "a directory is not a rules file");
        assert_eq!(read_rules(&dir.join("missing.md")), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn aged_self_blocks_count_down_in_squeues_shape() {
        let a = alloc().aged(Duration::from_secs(90));
        assert_eq!(a.time_left, "3:57:30");
        let mut b = alloc();
        b.time_left = "1-00:00:30".into();
        assert_eq!(b.aged(Duration::from_secs(60)).time_left, "23:59:30");
        let mut c = alloc();
        c.time_left = "0:30".into();
        assert_eq!(c.aged(Duration::from_secs(90)).time_left, "0:00");
        let mut d = alloc();
        d.time_left = "UNLIMITED".into();
        assert_eq!(d.aged(Duration::from_secs(90)).time_left, "UNLIMITED");
        assert_eq!(
            slurm_clock(Duration::from_secs(2 * 86_400 + 61)),
            "2-00:01:01"
        );
    }

    #[test]
    fn find_on_path_walks_skips_and_requires_exec() {
        let dir = test_dir("path");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        tools(&bin, &[("squeue", "#!/bin/sh\n")]);
        let exe = bin.join("squeue");
        // Plain file, not executable: must not count — detection must find
        // real binaries, not whatever a profile says.
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
        assert_eq!(svc.agent_context().await, None, "no scheduler, no context");

        // Fake tools → slurm, parsed snapshot, cached second read. squeue
        // answers both forms: -u (the queue) and -j (the self allocation),
        // and counts its -j calls.
        let dir = test_dir("fake");
        let asks = dir.join("self-asks");
        tools(
            &dir,
            &[
                ("scancel", "#!/bin/sh\nexit 0\n"),
                (
                    "squeue",
                    &format!(
                        "#!/bin/sh\nif [ \"$1\" = \"-j\" ]; then echo x >> '{}'; echo \"$2|batch|RUNNING|3:59:00|node7\"; else echo '1|batch|PENDING|59:00||4|8G|0:00|/home/u|Priority|myjob'; fi\n",
                        asks.display()
                    ),
                ),
                ("sinfo", "#!/bin/sh\necho 'batch*|up|10|1-00:00:00|8|64000|gpu:2'\n"),
            ],
        );
        let svc = ComputeService::with_bindir(dir.clone());
        let snap = svc.snapshot(false).await;
        assert_eq!(snap.scheduler, "slurm");
        assert_eq!(snap.jobs.len(), 1);
        assert_eq!(snap.jobs[0].name, "myjob");
        assert_eq!(snap.jobs[0].reason, "Priority", "the %r field rides along");
        assert_eq!(snap.partitions.len(), 1);
        assert!(snap.partitions[0].default);
        assert!(snap.partitions[0].gpus);
        // No SLURM_JOB_ID → no self block, and the wire omits the key.
        assert!(snap.self_alloc.is_none());
        assert!(!serde_json::to_string(&snap).unwrap().contains("\"self\""));
        // Cached: a second call inside the TTL returns the same fetch.
        let again = svc.snapshot(false).await;
        assert_eq!(again.fetched_at_ms, snap.fetched_at_ms);
        // Not in a job, Slurm detected: the login-node context.
        assert_eq!(
            svc.agent_context().await.as_deref(),
            Some(LOGIN_NODE_CONTEXT)
        );
        // ...unless the user said this host isn't a cluster.
        let workstation = ComputeService {
            not_a_cluster: true,
            ..ComputeService::with_bindir(dir.clone())
        };
        workstation.snapshot(false).await;
        assert_eq!(workstation.agent_context().await, None);

        // Inside an allocation: the self block rides the snapshot.
        let svc = ComputeService::with_bindir_and_job(dir.clone(), "4242");
        let snap = svc.snapshot(false).await;
        assert!(serde_json::to_string(&snap).unwrap().contains("\"self\""));
        let own = snap.self_alloc.expect("self allocation");
        assert_eq!(own.job_id, "4242");
        assert_eq!(own.node, "node7");
        assert_eq!(own.time_left, "3:59:00");
        // A forced refresh refetches the queue but NOT `squeue -j`: once a
        // minute, whatever the UI asks.
        let refreshed = svc.snapshot(true).await;
        assert!(refreshed.self_alloc.is_some(), "the last answer is served");
        assert_eq!(
            std::fs::read_to_string(&asks).unwrap().lines().count(),
            1,
            "squeue -j asked once"
        );

        // The agent context bakes from that self block, once: a second call
        // returns the SAME string (end included — it must not re-derive and
        // drift).
        let ctx = svc.agent_context().await.expect("agent context");
        assert!(ctx.contains("Slurm job 4242 on node node7"), "{ctx}");
        assert!(ctx.contains("partition batch"), "{ctx}");
        assert!(ctx.contains("around"), "{ctx}");
        assert_eq!(svc.agent_context().await.as_deref(), Some(ctx.as_str()));

        std::fs::remove_dir_all(&empty).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Off-cluster (no SLURM_JOB_ID at construction, no cached Slurm
    /// detection) the agent context is None without touching detection or
    /// the snapshot — the inertness guarantee for normal daemons (an
    /// unreachable bindir would otherwise make this probe something).
    #[tokio::test]
    async fn agent_context_is_none_without_a_self_job_or_a_cached_detection() {
        let svc = ComputeService::with_bindir(PathBuf::from("/nonexistent"));
        assert_eq!(svc.agent_context().await, None);
        // Nothing was detected or cached as a side effect.
        assert!(svc.inner.lock().await.detection.is_none());
        assert!(svc.inner.lock().await.agent_context.is_none());
    }

    #[tokio::test]
    async fn cancel_self_stops_only_its_own_job() {
        // Not in a job → 404, without detecting anything.
        let svc = ComputeService::with_bindir(PathBuf::from("/nonexistent"));
        assert_eq!(svc.cancel_self().await.0, StatusCode::NOT_FOUND);
        assert!(svc.inner.lock().await.detection.is_none());

        let dir = test_dir("cancel");
        let log = dir.join("scancel.args");
        tools(
            &dir,
            &[
                (
                    "scancel",
                    &format!("#!/bin/sh\necho \"$@\" > '{}'\n", log.display()),
                ),
                ("squeue", "#!/bin/sh\nexit 0\n"),
                ("sinfo", "#!/bin/sh\nexit 0\n"),
            ],
        );
        let svc = ComputeService::with_bindir_and_job(dir.clone(), "4242");
        let (status, body) = svc.cancel_self().await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "4242");

        // A job already gone: Slurm's "Invalid job id" is still success.
        tools(
            &dir,
            &[(
                "scancel",
                "#!/bin/sh\necho 'scancel: error: Kill job error on job id 4242: Invalid job id specified' >&2\nexit 1\n",
            )],
        );
        assert_eq!(svc.cancel_self().await.0, StatusCode::ACCEPTED);

        // Any other refusal is surfaced, Slurm's words cleaned.
        tools(
            &dir,
            &[(
                "scancel",
                "#!/bin/sh\necho 'scancel: error: Access/permission denied' >&2\nexit 1\n",
            )],
        );
        let (status, body) = svc.cancel_self().await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"], "scancel: Access/permission denied");

        // A non-numeric SLURM_JOB_ID never reaches scancel's argv.
        let odd = ComputeService::with_bindir_and_job(dir.clone(), "--user=x");
        assert_eq!(odd.cancel_self().await.0, StatusCode::NOT_FOUND);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn job_is_live_reads_state_absence_and_refusal() {
        let dir = test_dir("live");
        let squeue = dir.join("squeue");
        // One stand-in, written once; each case is a file it sources. A
        // script rewritten and run at once can fail with "Text file busy"
        // on Linux when another test forks meanwhile (the child briefly
        // holds the write descriptor) — which reads as "unanswered".
        tools(
            &dir,
            &[("squeue", "#!/bin/sh\n. \"$(dirname \"$0\")/case.sh\"\n")],
        );
        let case = |body: &str| std::fs::write(dir.join("case.sh"), body).unwrap();
        case("exit 0\n");
        runnable(&squeue);
        case("echo RUNNING\n");
        assert_eq!(job_is_live(&squeue, "1").await, Some(true));
        case("echo COMPLETING\n");
        assert_eq!(
            job_is_live(&squeue, "1").await,
            Some(true),
            "still draining"
        );
        case("echo 'CANCELLED by 1000'\n");
        assert_eq!(job_is_live(&squeue, "1").await, Some(false));
        case("exit 0\n");
        assert_eq!(job_is_live(&squeue, "1").await, Some(false), "not listed");
        case("echo 'slurm_load_jobs error: Invalid job id specified' >&2\nexit 1\n");
        assert_eq!(job_is_live(&squeue, "1").await, Some(false));
        case("echo 'Socket timed out' >&2\nexit 1\n");
        assert_eq!(job_is_live(&squeue, "1").await, None, "unanswered");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Run a just-written stand-in until the kernel lets it (a fork in
    /// another test can hold it busy for a moment), so the asserted calls
    /// that follow never meet "Text file busy".
    fn runnable(script: &Path) {
        for _ in 0..100 {
            match std::process::Command::new(script).output() {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => return,
            }
        }
    }
}
