//! Mode 2 — chimaera sessions AS Slurm jobs. The login-node daemon is the
//! control plane: it owns the DETACHED srun clients (via `compute::Detection`
//! — sbatch is gone, maintainer decision 2026-07-16: one mechanism that works
//! on every partition, tmux-grade persistence via setsid/nohup), the environment
//! preludes, its own deployed binary (`current_exe`, visible on compute
//! nodes over the shared FS — no redeploy), and the per-job homes. Launch,
//! discovery, and cancel are daemon routes so the feature is identical for
//! the app, the browser, and the CLI; the laptop side only builds the node
//! tunnel and opens the window.
//!
//! The registry is stateless by construction: `squeue` rows (job names
//! prefixed `chimaera-`) joined with per-job manifests under
//! `data_dir()/compute/<jobid>/` on the shared FS. Nothing here persists
//! daemon-side state; a card the home screen shows is reconstructed on
//! every list call, survives the laptop closing, and disappears when the
//! job ends — walltime death is honest (login daemon = forever, compute
//! daemon = until walltime).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::compute::Detection;
use crate::AppState;

/// A launch request. Everything lands in srun argv (and a script srun runs
/// on the node), so every field is validated against a strict charset —
/// never interpolate free text into flags (the prelude body is the one
/// deliberate exception: it is the user's own shell text, same trust as
/// their rc, and it lives in the script BODY, never in argv).
#[derive(Deserialize)]
pub(crate) struct LaunchSpec {
    /// Display name; slugged into the job name (`chimaera-<slug>`).
    name: String,
    /// Walltime, e.g. "4:00:00" or "1-00:00:00". Required — the allocation's
    /// lifespan is the contract the user is explicitly choosing.
    time: String,
    #[serde(default)]
    partition: Option<String>,
    #[serde(default)]
    cpus: Option<u32>,
    #[serde(default)]
    mem: Option<String>,
    #[serde(default)]
    gres: Option<String>,
    /// Workspace whose prelude scope applies (and whose folder the user
    /// will typically open on the node).
    #[serde(default)]
    workspace_id: Option<String>,
    /// Launch-scope prelude text, concatenated after host ⊕ workspace.
    #[serde(default)]
    prelude: Option<String>,
    /// Launch the job daemon with a routable bind (`--bind-routable`) for
    /// clusters whose ladder needs rung A (no ssh-to-node). Default off:
    /// loopback + the ssh-adopt tunnel is the secure path.
    #[serde(default)]
    routable: bool,
}

/// One discovered compute session: a `chimaera-` squeue row joined with its
/// job-home manifest (when the daemon inside has come up) and launch record.
#[derive(Serialize)]
pub(crate) struct ComputeSession {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) node: String,
    pub(crate) partition: String,
    pub(crate) time_left: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cpus: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mem: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gres: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) workspace_id: Option<String>,
    /// The job daemon's loopback port + token, from its manifest on the
    /// shared FS (same-user trust domain: 0600 under their own $HOME,
    /// served over the bearer-authed login daemon). The app shell keeps
    /// these in Rust; the home-screen JS never sees them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) token: Option<String>,
    /// Whether the job was launched with a routable bind (rung A capable).
    pub(crate) routable: bool,
    /// Compute-node egress to the agent API, probed at job start (None =
    /// not probed yet / probe tooling missing — "couldn't verify", not
    /// "blocked").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) egress: Option<bool>,
    /// RUNNING + manifest present: the tunnel can be built now.
    pub(crate) ready: bool,
}

/// `<data_dir>/compute` — job homes, scripts, logs, and launch records all
/// live under the daemon's own state root (dev daemon → dev compute jobs,
/// real daemon → real ones; disjoint by construction, shared-FS visible).
fn compute_root() -> PathBuf {
    chimaera_core::data_dir().join("compute")
}

/// Job-name slug: lowercase alnum/dash, bounded — goes into srun's
/// `--job-name=chimaera-<slug>` and the discovery prefix match.
fn slug(name: &str) -> String {
    let mut s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    s = s.trim_matches('-').to_string();
    if s.is_empty() {
        s = "session".to_string();
    }
    s.truncate(32);
    s
}

/// Charset gate for values that land in srun argv (and shell lines).
fn safe_directive(v: &str, extra: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// The launch script srun runs ON THE NODE (`bash -l <script>` so the
/// prelude sees profile functions — `ml`, conda — exactly like the agent
/// login-wrapper); `exec` so the daemon IS the job's main process —
/// walltime/scancel kill the whole tree. The job daemon gets an isolated
/// CHIMAERA_HOME keyed by jobid: own manifest, token, sessions — multi-job
/// and multi-user safe by construction. Resources live in [`srun_args`],
/// not here (no #SBATCH lines: sbatch is gone — see the launch handler).
fn render_launch_script(bin: &Path, root: &Path, prelude: &str, routable: bool) -> String {
    let mut s = String::from("#!/bin/bash -l\n");
    s.push_str(&format!(
        "# Owner-only: the job home lands on a SHARED filesystem and holds\n\
         # the daemon manifest (bearer token) and caps probe.\n\
         umask 077\n\
         export CHIMAERA_HOME=\"{}/${{SLURM_JOB_ID}}\"\n\
         mkdir -p \"$CHIMAERA_HOME\"\n\
         # Clean prelude slate: the job daemon materializes its own for the\n\
         # sessions IT spawns (same scrub the daemon does at every spawn).\n\
         unset CHIMAERA_PRELUDE CHIMAERA_PRELUDE_DONE\n\n",
        root.display()
    ));
    if !prelude.is_empty() {
        s.push_str(
            "# --- environment prelude (host + workspace + launch; opaque, never parsed) ---\n",
        );
        s.push_str(prelude);
        if !prelude.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(
            "# ------------------------------------------------------------------------------\n\n",
        );
    }
    s.push_str(
        "# Per-cluster fact, probed where it matters (THIS node), recorded for the UI.\n\
         # curl prints 000 AND exits non-zero on failure; digits-only + base-10\n\
         # normalization keeps the emitted value a valid JSON number (a leading-\n\
         # zero \"000\" would make caps.json unparseable exactly on the blocked-\n\
         # egress nodes the probe exists for).\n\
         code=$(curl -sS -m 8 -o /dev/null -w '%{http_code}' https://api.anthropic.com/ 2>/dev/null) || true\n\
         code=$(printf '%s' \"$code\" | tr -cd '0-9')\n\
         code=$((10#${code:-0}))\n\
         printf '{\"egress\":%s,\"http_code\":%s,\"probed_at\":%s}\\n' \\\n\
         \x20 \"$([ \"$code\" -ge 200 ] && echo true || echo false)\" \"$code\" \"$(date +%s)\" \\\n\
         \x20 > \"$CHIMAERA_HOME/caps.json\"\n\n",
    );
    let flag = if routable { " --bind-routable" } else { "" };
    s.push_str(&format!("exec \"{}\" serve{flag}\n", bin.display()));
    s
}

/// The srun argv for a launch — resources as flags (they lived in #SBATCH
/// directives in the sbatch era). Every value is charset-validated by the
/// handler before this runs; the returned args are also safe to
/// single-quote into a shell line (the charsets exclude quotes).
/// `job_name` is the launch's UNIQUE name (`chimaera-<slug>~<tok>`): queue
/// adoption matches it exactly, so two concurrent launches sharing a slug —
/// or a dev and a real daemon sharing one user's queue — can never adopt
/// each other's job (and cancel can never scancel the wrong allocation).
fn srun_args(spec: &LaunchSpec, job_name: &str, script: &Path) -> Vec<String> {
    let mut a = vec![
        format!("--job-name={job_name}"),
        format!("--time={}", spec.time),
    ];
    if let Some(p) = &spec.partition {
        a.push(format!("--partition={p}"));
    }
    if let Some(c) = spec.cpus {
        a.push(format!("--cpus-per-task={c}"));
    }
    if let Some(m) = &spec.mem {
        a.push(format!("--mem={m}"));
    }
    if let Some(g) = spec.gres.as_deref().filter(|g| !g.is_empty()) {
        a.push(format!("--gres={g}"));
    }
    a.push("bash".to_string());
    a.push("-l".to_string());
    a.push(script.to_string_lossy().into_owned());
    a
}

/// One shell line that DETACHES srun from this daemon: `setsid`, `nohup`,
/// and `&` together orphan the client onto init, so the allocation has
/// tmux-grade persistence — it survives daemon restarts and ends only at
/// walltime, scancel, or a login-node reboot (the maintainer's model:
/// "most people have tmux sessions going there"). srun's own output
/// (including Slurm's refusal messages) lands in `log`, which the handler
/// tails when the job never appears in the queue.
fn detached_srun_line(srun: &Path, args: &[String], log: &Path) -> String {
    let quote = |s: &str| format!("'{s}'");
    let argv: Vec<String> = std::iter::once(srun.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .map(|a| quote(&a))
        .collect();
    // `unset SLURM_JOB_ID SLURM_JOBID`: srun inherits the daemon's env, and
    // with a job id set it attaches a STEP inside that existing allocation
    // instead of submitting a new job — a daemon started from within an
    // allocation (an sdev shell, or the job daemon's own route) would launch
    // an invisible, unadoptable daemon into the old job. `umask 077`: the
    // log lands on the shared FS and echoes the srun argv + daemon stderr.
    format!(
        "unset SLURM_JOB_ID SLURM_JOBID; umask 077; setsid nohup {} >> {} 2>&1 < /dev/null & echo detached",
        argv.join(" "),
        quote(&log.to_string_lossy())
    )
}

/// POST /api/v1/compute/sessions — submit a chimaera daemon as a Slurm job.
/// Returns `{"job_id": "..."}`; everything else is discovered statelessly.
pub(crate) async fn launch_compute_session(
    State(state): State<Arc<AppState>>,
    Json(spec): Json<LaunchSpec>,
) -> Response {
    let bad = |m: String| (StatusCode::BAD_REQUEST, Json(json!({"error": m}))).into_response();
    // Directive-line validation: strict charsets, never free text.
    if !safe_directive(&spec.time, ":-") {
        return bad(format!("invalid time {:?}", spec.time));
    }
    if let Some(p) = &spec.partition {
        if !safe_directive(p, "_-.") {
            return bad(format!("invalid partition {p:?}"));
        }
    }
    if let Some(m) = &spec.mem {
        if !safe_directive(m, "") {
            return bad(format!("invalid mem {m:?}"));
        }
    }
    if let Some(g) = spec.gres.as_deref().filter(|g| !g.is_empty()) {
        if !safe_directive(g, ":_,-.") {
            return bad(format!("invalid gres {g:?}"));
        }
    }
    if let Some(c) = spec.cpus {
        if c == 0 || c > 1024 {
            return bad(format!("invalid cpus {c}"));
        }
    }
    if let Some(p) = &spec.prelude {
        if p.len() > crate::environment::MAX_SCOPE_BYTES || p.contains('\0') {
            return bad("invalid prelude text".to_string());
        }
    }

    let Detection::Slurm { srun, .. } = state.compute.detection().await else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "no scheduler detected on this host"})),
        )
            .into_response();
    };
    let Ok(bin) = std::env::current_exe() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "cannot resolve the daemon binary path"})),
        )
            .into_response();
    };

    let prelude = crate::lock(&state.env_preludes).current().effective(
        spec.workspace_id.as_deref().unwrap_or(""),
        spec.prelude.as_deref(),
    );
    let job_slug = slug(&spec.name);
    // Per-launch token: suffixes the job NAME (queue adoption then matches
    // exactly — concurrent same-slug launches, foreign chimaera-* jobs, and
    // a dev daemon sharing this user's queue can never be mis-adopted) and
    // names the script/log files.
    let launch_tok = chimaera_core::generate_token()[..6].to_string();
    let job_name = format!("chimaera-{job_slug}~{launch_tok}");
    let root = compute_root();
    let script = render_launch_script(&bin, &root, &prelude, spec.routable);

    // Blocking fs (shared FS!) off the reactor: dirs + the script file.
    // Everything under the compute root is owner-only — the script embeds
    // the user's prelude (people export API keys there on egress-limited
    // clusters) and the job homes carry daemon tokens; a traversable $HOME
    // on a shared login node must not expose either.
    let script_path = {
        let root = root.clone();
        let tok = launch_tok.clone();
        let res = tokio::task::spawn_blocking(move || -> anyhow::Result<PathBuf> {
            for dir in [
                root.clone(),
                root.join("logs"),
                root.join("scripts"),
                root.join("pending"),
            ] {
                std::fs::create_dir_all(&dir)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
                }
            }
            let path = root
                .join("scripts")
                .join(format!("{}-{}.sh", chrono_free_ts(), tok));
            std::fs::write(&path, &script)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(path)
        })
        .await;
        match res.map_err(anyhow::Error::from).and_then(|r| r) {
            Ok(p) => p,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("cannot stage launch script: {e}")})),
                )
                    .into_response()
            }
        }
    };

    // DETACHED srun, not sbatch (maintainer decision, 2026-07-16): one
    // launch mechanism that works on EVERY partition — including
    // interactive-only ones whose job_submit policy refuses batch (found
    // live: Sherlock's `dev`). setsid+nohup+& orphans the srun client onto
    // init, so the allocation persists like a tmux session: daemon restarts
    // don't touch it; walltime, scancel, or a login-node reboot end it.
    // Slurm can't hand us the job id this way, so the id comes from the
    // queue itself (the same adoption used for ghost recovery), and srun's
    // own words (refusals, limits) land in a log we tail on failure.
    let log_path = root
        .join("logs")
        .join(format!("srun-{}-{}.log", chrono_free_ts(), launch_tok));
    let line = detached_srun_line(&srun, &srun_args(&spec, &job_name, &script_path), &log_path);
    let detached = crate::compute::run_capped("bash", &["-c".into(), line]).await;
    if detached.is_none() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "could not start srun on this host"})),
        )
            .into_response();
    }

    // Everything after the detach runs in a SPAWNED task: srun has already
    // fired, and a client that gives up (timeout, closed window) drops this
    // handler future — aborting adoption mid-flight would leave a live job
    // with no record and no workspace seed. The spawned task finishes
    // regardless; the handler merely awaits its outcome.
    let post = tokio::spawn(async move {
        // The job registers in squeue within moments (PENDING while it
        // waits for resources). Adopt the row wearing this launch's UNIQUE
        // name; a refusal never registers, so after the retries the log
        // tail is the diagnosis — Slurm's own words.
        let mut job_id: Option<String> = None;
        for _ in 0..4 {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            job_id = adopt_submitted(&state, &job_name, &root).await;
            if job_id.is_some() {
                break;
            }
        }
        let Some(job_id) = job_id else {
            let tail = {
                let log_path = log_path.clone();
                tokio::task::spawn_blocking(move || read_log_tail(&log_path))
                    .await
                    .ok()
                    .flatten()
            };
            let msg = match tail {
                Some(t) if !t.is_empty() => format!("srun: {t}"),
                _ => "srun started but the job never appeared in the queue — try again".to_string(),
            };
            return Err(msg);
        };

        // The launch record FIRST (it is what makes the job adopted — the
        // next same-name launch must not see this row as unclaimed), then
        // the workspace seed. The record's card carries the resource
        // numbers while Slurm is the only other truth; the script path
        // doubles as the launch's PROCESS-TABLE FINGERPRINT — unique per
        // launch and verbatim in the detached srun's argv, which is what
        // lets the fast-twitch layer map live srun clients back to
        // sessions without asking the controller. Atomic write: the list
        // poller reads this file concurrently over the shared FS, and a
        // torn read would get the record swept as corrupt.
        let record = json!({
            "name": job_name,
            "display_name": spec.name,
            "partition": spec.partition,
            "cpus": spec.cpus,
            "mem": spec.mem,
            "gres": spec.gres,
            "time": spec.time,
            "workspace_id": spec.workspace_id,
            "routable": spec.routable,
            "script": script_path.to_string_lossy(),
            "log": log_path.to_string_lossy(),
        });
        let rec_path = root.join("pending").join(format!("{job_id}.json"));
        if let Err(e) = tokio::task::spawn_blocking(move || {
            crate::persist::atomic_write_json(
                &rec_path,
                serde_json::to_vec_pretty(&record).unwrap_or_default(),
            )
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
        {
            tracing::warn!(%e, %job_id, "launch record write failed (card shows without resources)");
        }

        // Seed the job daemon's workspace registry over the shared FS: the
        // job will `mkdir -p` this same home, and its WorkspaceStore then
        // boots with this host's WHOLE workspace list already registered
        // (shared-FS roots are equally valid on the node) — the compute
        // window lands on the same ready-to-open workspaces as the login
        // window, instead of a bare "open a folder" page (the maintainer
        // hit exactly that dead end on first use; launches from the host
        // page carry no workspace_id, so seeding only the launch workspace
        // left the window empty on second use).
        let seed: Vec<serde_json::Value> = {
            let launch_ws = spec.workspace_id.as_deref();
            crate::lock(&state.workspaces)
                .list()
                .into_iter()
                .map(|ws| {
                    json!({
                        "id": format!("w-{}", &chimaera_core::generate_token()[..8]),
                        "root": ws.root,
                        "name": ws.name,
                        // The launch's workspace (when there is one) sorts first.
                        "last_opened_at": if launch_ws == Some(ws.id.as_str()) {
                            now_secs() + 1
                        } else {
                            now_secs()
                        },
                    })
                })
                .collect()
        };
        if !seed.is_empty() {
            let seed_dir = root.join(&job_id).join("data");
            let res = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                std::fs::create_dir_all(&seed_dir)?;
                crate::persist::atomic_write_json(
                    &seed_dir.join("workspaces.json"),
                    serde_json::to_vec_pretty(&seed)?,
                )
            })
            .await;
            if let Err(e) = res.map_err(anyhow::Error::from).and_then(|r| r) {
                tracing::warn!(%e, %job_id, "workspace seed failed (compute window starts empty)");
            }
        }
        Ok(job_id)
    });
    match post.await {
        Ok(Ok(job_id)) => {
            tracing::info!(%job_id, slug = %job_slug, "compute session submitted");
            Json(json!({ "job_id": job_id })).into_response()
        }
        Ok(Err(msg)) => (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("launch bookkeeping failed: {e}")})),
        )
            .into_response(),
    }
}

/// The last ~4KB of a detached srun's log, cleaned into the admin-authored
/// message ("Batch jobs are not allowed…"-grade text) — the diagnosis when
/// a launch never reached the queue. Blocking fs: call off the reactor.
fn read_log_tail(log: &Path) -> Option<String> {
    let bytes = std::fs::read(log).ok()?;
    let start = bytes.len().saturating_sub(4096);
    let tail = String::from_utf8_lossy(&bytes[start..]);
    let cleaned = crate::compute::clean_tool_stderr(&tail, "srun");
    (cleaned != "the command failed without a message").then_some(cleaned)
}

/// The queue-side id discovery every launch relies on (srun can't hand us
/// the job id the way `sbatch --parsable` did): refetch the queue and pick
/// the job wearing this launch's UNIQUE name (`chimaera-<slug>~<tok>`) that
/// no launch record claims. The unique suffix makes adoption a lookup, not
/// a heuristic; newest-unrecorded remains only as a tiebreak against a
/// pathological name collision. The fs check runs off the reactor
/// (shared FS).
async fn adopt_submitted(state: &Arc<AppState>, job_name: &str, root: &Path) -> Option<String> {
    // invalidate + snapshot(false): drop the cache so this round refetches
    // the queue, WITHOUT `refresh=true` — that would also discard scheduler
    // detection and re-run the login-shell probe on every retry (seconds
    // each under the single-flight lock, and a transient probe failure
    // mid-launch would misreport a queued job as never-appeared).
    state.compute.invalidate().await;
    let snap = state.compute.snapshot(false).await;
    let candidates: Vec<String> = snap
        .jobs
        .iter()
        .filter(|j| j.name == job_name)
        .map(|j| j.id.clone())
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let recorded: std::collections::HashSet<String> = candidates
            .iter()
            .filter(|id| root.join("pending").join(format!("{id}.json")).is_file())
            .cloned()
            .collect();
        newest_unrecorded(&candidates, &recorded)
    })
    .await
    .ok()
    .flatten()
}

/// Highest-numbered candidate id not already claimed by a record — job ids
/// are monotonically increasing, so the newest submission wins.
fn newest_unrecorded(
    candidates: &[String],
    recorded: &std::collections::HashSet<String>,
) -> Option<String> {
    candidates
        .iter()
        .filter(|id| !recorded.contains(*id))
        .max_by_key(|id| id.parse::<u64>().unwrap_or(0))
        .cloned()
}

/// GET /api/v1/compute/sessions — the stateless registry: chimaera-named
/// squeue rows ⋈ per-job manifests ⋈ launch records. Also carries the
/// scheduler tag and the partitions list so ONE call feeds the home-screen
/// group and the launch dialog.
pub(crate) async fn list_compute_sessions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<crate::compute::ComputeQuery>,
) -> Response {
    let snap = state.compute.snapshot(q.refresh).await;
    if snap.scheduler != "slurm" {
        return Json(json!({"scheduler": snap.scheduler, "sessions": [], "partitions": []}))
            .into_response();
    }
    let root = compute_root();
    let candidates: Vec<crate::compute::Job> = snap
        .jobs
        .iter()
        .filter(|j| j.name.starts_with("chimaera-"))
        .cloned()
        .collect();
    let degraded = snap.degraded;
    // The fast-twitch layer: the squeue snapshot above may be ≤30s stale
    // (controller politeness), but two LOCAL signals are read fresh on
    // every call at zero controller cost — the process table (is a
    // launch's detached srun client still alive? its script path is the
    // per-launch argv fingerprint) and the shared-FS manifests (has the
    // job daemon booted?). Together they flip cards the moment reality
    // changes; squeue reconciles everything on its own cadence.
    let clients = live_srun_clients().await;
    // Blocking joins (manifest/caps/record on possibly-NFS) off the reactor.
    let sessions = tokio::task::spawn_blocking(move || {
        let mut sessions = candidates
            .into_iter()
            .map(|j| join_session(&root, j, clients.as_deref()))
            .collect::<Vec<_>>();
        // A stale-jobs snapshot (squeue failed) must not turn live jobs
        // into tombstones — skip the orphan sweep on degraded rounds.
        if !degraded {
            append_ended_sessions(&root, &mut sessions, clients.as_deref());
        }
        sessions
    })
    .await
    .unwrap_or_default();
    Json(json!({
        "scheduler": snap.scheduler,
        "sessions": sessions,
        "partitions": snap.partitions,
        // Stale-jobs marker (squeue failed; rows carried forward): the UI
        // keeps its old countdown baseline instead of rewinding to these
        // rows' pre-outage time_left strings on every poll.
        "degraded": degraded,
    }))
    .into_response()
}

/// The listing's second half: launch records whose job has LEFT the queue.
/// A walltime death otherwise erases the card mid-session with zero trace —
/// "the job sometimes disappears?" (the maintainer, live). Ended cards are
/// dismissable tombstones: DELETE marks the record cancelled, and this sweep
/// removes marked or aged-out (48h) records — self-cleaning, no daemon state.
fn append_ended_sessions(
    root: &Path,
    sessions: &mut Vec<ComputeSession>,
    clients: Option<&[String]>,
) {
    const ENDED_CAP: usize = 20;
    let live: std::collections::HashSet<&str> =
        sessions.iter().map(|s| s.job_id.as_str()).collect();
    let Ok(dir) = std::fs::read_dir(root.join("pending")) else {
        return;
    };
    let mut fresh: Vec<(std::time::SystemTime, ComputeSession)> = Vec::new();
    let mut ended: Vec<(std::time::SystemTime, ComputeSession)> = Vec::new();
    for entry in dir.flatten() {
        let path = entry.path();
        let Some(job_id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json"))
            .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
        else {
            continue;
        };
        if live.contains(job_id) {
            continue;
        }
        let record: Option<serde_json::Value> = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let dismissed = record
            .as_ref()
            .and_then(|r| r.get("cancelled").and_then(|v| v.as_bool()))
            .unwrap_or(false);
        // A clock-skewed (future) mtime reads as age zero: brand new.
        let age = mtime.elapsed().unwrap_or(Duration::ZERO);
        let state = orphan_state(age, client_alive(record.as_ref(), clients));
        if dismissed || state.is_none() || record.is_none() {
            prune_launch_artifacts(root, job_id, record.as_ref());
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let state = state.expect("checked above");
        let rec = |k: &str| record.as_ref().and_then(|r| r.get(k).cloned());
        let card = ComputeSession {
            ready: false,
            name: rec("display_name")
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| job_id.to_string()),
            cpus: rec("cpus").and_then(|v| v.as_u64()).map(|v| v as u32),
            mem: rec("mem").and_then(|v| v.as_str().map(str::to_string)),
            gres: rec("gres").and_then(|v| v.as_str().map(str::to_string)),
            workspace_id: rec("workspace_id").and_then(|v| v.as_str().map(str::to_string)),
            routable: false,
            egress: None,
            port: None,
            token: None,
            job_id: job_id.to_string(),
            state: state.to_string(),
            node: String::new(),
            partition: rec("partition")
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            // A just-submitted card shows its requested walltime, exactly
            // like a squeue-visible PENDING row does.
            time_left: if state == "PENDING" {
                rec("time")
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            } else {
                String::new()
            },
        };
        if state == "PENDING" {
            fresh.push((mtime, card));
        } else {
            ended.push((mtime, card));
        }
    }
    // Just-submitted cards lead (that click deserves an instant card);
    // tombstones trail the live cards, newest deaths first, bounded.
    fresh.sort_by_key(|e| std::cmp::Reverse(e.0));
    for (_, s) in fresh.into_iter().rev() {
        sessions.insert(0, s);
    }
    ended.sort_by_key(|e| std::cmp::Reverse(e.0));
    sessions.extend(ended.into_iter().take(ENDED_CAP).map(|(_, s)| s));
}

/// A retired record takes its launch litter with it: the staged script, the
/// srun log, and the per-job daemon home. Only the record had a sweep before
/// — every launch leaked all three onto the user's (quota'd) shared home
/// forever. Paths from the record are honored only inside the compute root
/// (the record is ours, but a tampered one must not become an arbitrary
/// delete); the job id is digits-only by the caller's filename filter.
fn prune_launch_artifacts(root: &Path, job_id: &str, record: Option<&serde_json::Value>) {
    if let Some(rec) = record {
        for key in ["script", "log"] {
            if let Some(p) = rec
                .get(key)
                .and_then(|v| v.as_str())
                .map(Path::new)
                .filter(|p| p.starts_with(root))
            {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let home = root.join(job_id);
    if home.is_dir() {
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// What an orphaned launch record (no squeue row for its job) means. The
/// process table outranks age when it can speak: a LIVING srun client
/// means the launch is still queued/held however stale the squeue cache is
/// (PENDING past any grace window), and a dead client on a young record
/// means it already failed. Age decides only when the client is unknowable
/// (pre-fingerprint records): a seconds-old record is a JUST-submitted job
/// squeue hasn't shown yet — PENDING, not dead (found live: a fresh launch
/// briefly wore an "ended" card). Hours old is a session that ended
/// without an explicit cancel — the tombstone. Two days old is litter
/// (None = remove the record).
fn orphan_state(age: Duration, client_alive: Option<bool>) -> Option<&'static str> {
    const SUBMIT_GRACE: Duration = Duration::from_secs(120);
    const ENDED_MAX_AGE: Duration = Duration::from_secs(48 * 3600);
    if age >= ENDED_MAX_AGE {
        return None;
    }
    match client_alive {
        Some(true) => Some("PENDING"),
        Some(false) => Some("ENDED"),
        None if age < SUBMIT_GRACE => Some("PENDING"),
        None => Some("ENDED"),
    }
}

/// Whether a launch's detached srun client is still in the process table —
/// matched by the record's script path, the per-launch argv fingerprint.
/// `None` = unknowable (no scan, or a pre-fingerprint record); the caller
/// falls back to squeue/age truth.
fn client_alive(record: Option<&serde_json::Value>, clients: Option<&[String]>) -> Option<bool> {
    let clients = clients?;
    let script = record?.get("script")?.as_str()?;
    if script.is_empty() {
        return None;
    }
    Some(clients.iter().any(|line| line.contains(script)))
}

/// One `ps` pass over the user's processes → the argv lines of live
/// chimaera srun clients. Local and controller-free (the whole point);
/// `None` when the scan itself is unavailable, so callers infer nothing.
async fn live_srun_clients() -> Option<Vec<String>> {
    let user = std::env::var("USER").ok()?;
    let out =
        crate::compute::run_capped("ps", &["-u".into(), user, "-o".into(), "args=".into()]).await?;
    Some(
        out.lines()
            .filter(|l| l.contains("srun") && l.contains("--job-name=chimaera-"))
            .map(str::to_string)
            .collect(),
    )
}

fn join_session(root: &Path, j: crate::compute::Job, clients: Option<&[String]>) -> ComputeSession {
    let home = root.join(&j.id);
    let manifest: Option<chimaera_core::Manifest> = std::fs::read(home.join("data/manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let caps: Option<serde_json::Value> = std::fs::read(home.join("caps.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let record: Option<serde_json::Value> =
        std::fs::read(root.join("pending").join(format!("{}.json", j.id)))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());

    // Fast-twitch corrections over the (≤30s stale) squeue row, from the
    // two fresh local signals. Order matters: death outranks boot.
    let mut state = j.state.clone();
    let mut node = j.nodes.clone();
    let mut time_left = j.time_left.clone();
    if client_alive(record.as_ref(), clients) == Some(false) {
        // The detached srun client is GONE: scancel'd, walltime-killed, or
        // failed — however lively the cached row still looks. Present the
        // truth now; the next squeue round agrees and the tombstone flow
        // takes over.
        state = "ENDED".to_string();
        node = String::new();
        time_left = String::new();
    } else if state == "PENDING" && manifest.is_some() {
        // The job daemon has BOOTED (manifest on the shared FS) — the job
        // is running whatever the cached row says. Node from the manifest's
        // own hostname (first label matches squeue's %N form); time_left
        // estimated from the requested walltime minus the daemon's uptime,
        // squeue-corrected within a round.
        state = "RUNNING".to_string();
        if node.is_empty() {
            if let Some(m) = &manifest {
                node = m
                    .hostname
                    .split('.')
                    .next()
                    .unwrap_or(&m.hostname)
                    .to_string();
            }
        }
        if let (Some(m), Some(requested)) = (
            &manifest,
            record
                .as_ref()
                .and_then(|r| r.get("time"))
                .and_then(|v| v.as_str()),
        ) {
            if let Some(estimate) = estimate_time_left(requested, m.started_at) {
                time_left = estimate;
            }
        }
    }

    let rec = |k: &str| record.as_ref().and_then(|r| r.get(k).cloned());
    let running = state == "RUNNING";
    ComputeSession {
        ready: running && manifest.is_some(),
        name: rec("display_name")
            .and_then(|v| v.as_str().map(str::to_string))
            // Record-less fallback: the squeue name, shorn of the chimaera-
            // prefix and the per-launch `~tok` uniqueness suffix.
            .unwrap_or_else(|| {
                let base = j.name.trim_start_matches("chimaera-");
                base.split('~').next().unwrap_or(base).to_string()
            }),
        // Resources: the launch record's request first, squeue's own %C/%m
        // as the fallback — a record-less job (adopted late submission,
        // launched outside chimaera) still shows what it holds.
        cpus: rec("cpus")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .or_else(|| j.cpus.parse().ok()),
        mem: rec("mem")
            .and_then(|v| v.as_str().map(str::to_string))
            .or_else(|| (!j.mem.is_empty()).then(|| j.mem.clone())),
        gres: rec("gres").and_then(|v| v.as_str().map(str::to_string)),
        workspace_id: rec("workspace_id").and_then(|v| v.as_str().map(str::to_string)),
        routable: rec("routable").and_then(|v| v.as_bool()).unwrap_or(false),
        egress: caps.and_then(|c| c.get("egress").and_then(|v| v.as_bool())),
        port: manifest.as_ref().map(|m| m.port),
        token: manifest.map(|m| m.token),
        job_id: j.id,
        state,
        node,
        partition: j.partition,
        time_left,
    }
}

/// Requested walltime minus the job daemon's uptime → a Slurm-style
/// remaining-time string, for the gap between the daemon booting and the
/// next squeue round confirming it. None when the request isn't a duration
/// or the clock says nonsense — then the raw squeue value stands.
fn estimate_time_left(requested: &str, daemon_started_at: u64) -> Option<String> {
    let m = regex_lite_time(requested)?;
    let elapsed = now_secs().checked_sub(daemon_started_at)?;
    let left = m.saturating_sub(elapsed);
    let days = left / 86_400;
    let hours = (left % 86_400) / 3_600;
    let mins = (left % 3_600) / 60;
    let secs = left % 60;
    Some(if days > 0 {
        format!("{days}-{hours:02}:{mins:02}:{secs:02}")
    } else if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{mins:02}:{secs:02}")
    })
}

/// Slurm walltime grammar (`[days-]hours:minutes:seconds`, `MM:SS`, bare
/// minutes) → seconds. Non-durations (UNLIMITED, …) → None.
fn regex_lite_time(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return s.parse::<u64>().ok().map(|m| m * 60);
    }
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let nums: Vec<u64> = parts
        .iter()
        .map(|p| p.parse::<u64>())
        .collect::<Result<_, _>>()
        .ok()?;
    let (h, m, sec) = match nums.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] if days == 0 => (0, *m, *s),
        [h, m] => (*h, *m, 0),
        _ => return None,
    };
    Some(((days * 24 + h) * 60 + m) * 60 + sec)
}

/// DELETE /api/v1/compute/sessions/{job_id} — scancel. Idempotent: a job
/// that already ended still answers 204 (the card is gone either way).
pub(crate) async fn cancel_compute_session(
    State(state): State<Arc<AppState>>,
    AxPath(job_id): AxPath<String>,
) -> Response {
    if job_id.is_empty() || !job_id.chars().all(|c| c.is_ascii_digit()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid job id"})),
        )
            .into_response();
    }
    let Detection::Slurm { scancel, .. } = state.compute.detection().await else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "no scheduler detected on this host"})),
        )
            .into_response();
    };
    // scancel's outcome matters: answering 204 on a FAILED cancel would mark
    // the record cancelled while the job keeps running — the card would
    // revert to RUNNING next poll, and the eventual walltime death would be
    // silently swept instead of raising its tombstone. Idempotency is kept
    // where it belongs: a job that already ended fails with Slurm's
    // "Invalid job id" words, which IS success here.
    match crate::compute::run_checked(&scancel.to_string_lossy(), std::slice::from_ref(&job_id))
        .await
    {
        Some(out) if out.success || out.stderr.contains("Invalid job id") => {}
        Some(out) => {
            let msg = crate::compute::clean_tool_stderr(&out.stderr, "scancel");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("scancel: {msg}")})),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "scancel did not answer (controller busy?) — the job was NOT cancelled"})),
            )
                .into_response();
        }
    }
    // Same reasoning as launch: the instant post-cancel refresh should see
    // the queue's new truth, not the pre-cancel cache.
    state.compute.invalidate().await;
    // Mark the launch record cancelled: while the job drains, the live card
    // keeps its name/resources from the record; once the squeue row is gone
    // the listing sweep removes marked records instead of raising an "ended"
    // tombstone — an explicit cancel was watched, only surprise deaths
    // deserve one. The same DELETE is also how a tombstone is dismissed.
    let rec_path = compute_root()
        .join("pending")
        .join(format!("{job_id}.json"));
    let _ = tokio::task::spawn_blocking(move || {
        let Ok(bytes) = std::fs::read(&rec_path) else {
            return;
        };
        let Ok(mut record) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            let _ = std::fs::remove_file(&rec_path);
            return;
        };
        record["cancelled"] = serde_json::Value::Bool(true);
        let _ = crate::persist::atomic_write_json(
            &rec_path,
            serde_json::to_vec_pretty(&record).unwrap_or_default(),
        );
    })
    .await;
    tracing::info!(%job_id, "compute session cancel requested");
    StatusCode::NO_CONTENT.into_response()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Sortable second-resolution stamp for script filenames without pulling a
/// date crate: unix seconds, zero-padded.
fn chrono_free_ts() -> String {
    format!("{:012}", now_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> LaunchSpec {
        LaunchSpec {
            name: "My Align Run!".to_string(),
            time: "4:00:00".to_string(),
            partition: Some("normal".to_string()),
            cpus: Some(4),
            mem: Some("16G".to_string()),
            gres: None,
            workspace_id: Some("w-abc".to_string()),
            prelude: None,
            routable: false,
        }
    }

    #[test]
    fn slug_bounds_and_cleans() {
        assert_eq!(slug("My Align Run!"), "my-align-run");
        assert_eq!(slug("  --weird--  "), "weird");
        assert_eq!(slug("!!!"), "session");
        assert!(slug(&"x".repeat(99)).len() <= 32);
    }

    #[test]
    fn adoption_picks_the_newest_unrecorded_submission() {
        let ids = ["34109903", "34109906", "34109801"].map(String::from);
        let mut recorded = std::collections::HashSet::new();
        // Nothing recorded → the numerically newest id wins.
        assert_eq!(
            newest_unrecorded(&ids, &recorded).as_deref(),
            Some("34109906")
        );
        // The newest already has a record (a normal launch raced in) → the
        // ghost is the older unrecorded one.
        recorded.insert("34109906".to_string());
        assert_eq!(
            newest_unrecorded(&ids, &recorded).as_deref(),
            Some("34109903")
        );
        // Everything claimed → nothing to adopt.
        recorded.insert("34109903".to_string());
        recorded.insert("34109801".to_string());
        assert_eq!(newest_unrecorded(&ids, &recorded), None);
    }

    #[test]
    fn orphan_record_age_maps_to_submitted_then_tombstone_then_litter() {
        // Client unknowable → age decides. Seconds old = just submitted,
        // squeue hasn't caught up — PENDING.
        assert_eq!(orphan_state(Duration::from_secs(5), None), Some("PENDING"));
        assert_eq!(
            orphan_state(Duration::from_secs(119), None),
            Some("PENDING")
        );
        // Past the grace window it ended without a cancel — the tombstone.
        assert_eq!(orphan_state(Duration::from_secs(121), None), Some("ENDED"));
        assert_eq!(
            orphan_state(Duration::from_secs(47 * 3600), None),
            Some("ENDED")
        );
        // Two days on, the record is litter to sweep.
        assert_eq!(orphan_state(Duration::from_secs(49 * 3600), None), None);
    }

    #[test]
    fn process_table_outranks_age_for_orphans() {
        // A living srun client keeps the launch PENDING past any grace —
        // the squeue cache is just stale.
        assert_eq!(
            orphan_state(Duration::from_secs(3600), Some(true)),
            Some("PENDING")
        );
        // A dead client on a young record means it already failed.
        assert_eq!(
            orphan_state(Duration::from_secs(5), Some(false)),
            Some("ENDED")
        );
        // Litter is litter regardless of what the table says.
        assert_eq!(
            orphan_state(Duration::from_secs(49 * 3600), Some(true)),
            None
        );
    }

    #[test]
    fn client_alive_matches_by_script_fingerprint() {
        let rec = json!({"script": "/r/scripts/a.sh"});
        let lines = vec![
            "/usr/bin/srun --job-name=chimaera-x --time=1:00:00 bash -l /r/scripts/a.sh"
                .to_string(),
        ];
        assert_eq!(client_alive(Some(&rec), Some(&lines)), Some(true));
        assert_eq!(client_alive(Some(&rec), Some(&[])), Some(false));
        // Unknowable: no scan, no record, or a pre-fingerprint record.
        assert_eq!(client_alive(Some(&rec), None), None);
        assert_eq!(client_alive(None, Some(&lines)), None);
        assert_eq!(client_alive(Some(&json!({})), Some(&lines)), None);
    }

    #[test]
    fn walltime_grammar_and_estimates() {
        assert_eq!(regex_lite_time("2:00:00"), Some(7200));
        assert_eq!(regex_lite_time("1-00:00:00"), Some(86_400));
        assert_eq!(regex_lite_time("30:00"), Some(1800));
        assert_eq!(regex_lite_time("45"), Some(2700));
        assert_eq!(regex_lite_time("UNLIMITED"), None);
        // Daemon booted 60s ago on a 20-minute request → ~19:00 left.
        let est = estimate_time_left("0:20:00", now_secs() - 60).unwrap();
        assert!(est.starts_with("19:") || est.starts_with("18:5"), "{est}");
    }

    #[test]
    fn join_fast_forwards_pending_with_manifest_and_ends_dead_clients() {
        let dir = std::env::temp_dir().join(format!("chimaera-join-{}", std::process::id()));
        let home = dir.join("77").join("data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(dir.join("pending")).unwrap();
        std::fs::write(
            home.join("manifest.json"),
            format!(
                "{{\"hostname\":\"nodeX.int\",\"port\":4000,\"token\":\"t\",\"pid\":1,\"version\":\"0.0.1\",\"started_at\":{},\"build\":\"x\"}}",
                now_secs() - 30
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("pending").join("77.json"),
            "{\"display_name\":\"t\",\"time\":\"0:20:00\",\"script\":\"/r/s/77.sh\"}",
        )
        .unwrap();
        let job = |state: &str| crate::compute::Job {
            id: "77".to_string(),
            name: "chimaera-t".to_string(),
            partition: "normal".to_string(),
            state: state.to_string(),
            time_left: "20:00".to_string(),
            nodes: String::new(),
            cpus: String::new(),
            mem: String::new(),
        };
        // Cached PENDING + manifest on disk + living client → RUNNING/ready
        // with the manifest's node and an estimated countdown.
        let lines = vec!["srun --job-name=chimaera-t bash -l /r/s/77.sh".to_string()];
        let s = join_session(&dir, job("PENDING"), Some(&lines));
        assert_eq!(s.state, "RUNNING");
        assert!(s.ready);
        assert_eq!(s.node, "nodeX");
        assert!(!s.time_left.is_empty());
        // Client verifiably gone → ENDED now, however lively the cache row.
        let s = join_session(&dir, job("RUNNING"), Some(&[]));
        assert_eq!(s.state, "ENDED");
        assert!(!s.ready);
        // No scan → squeue truth stands untouched.
        let s = join_session(&dir, job("PENDING"), None);
        assert_eq!(s.state, "RUNNING", "manifest fast-forward needs no scan");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn render_launch_script_prelude_and_exec() {
        let script = render_launch_script(
            Path::new("/home/u/.chimaera-dev/bin/chimaera"),
            Path::new("/home/u/.chimaera-dev/data/compute"),
            "ml biology bcftools\nexport X=1",
            false,
        );
        for needle in [
            "#!/bin/bash -l",
            "CHIMAERA_HOME=\"/home/u/.chimaera-dev/data/compute/${SLURM_JOB_ID}\"",
            "unset CHIMAERA_PRELUDE CHIMAERA_PRELUDE_DONE",
            "ml biology bcftools",
            "caps.json",
            "exec \"/home/u/.chimaera-dev/bin/chimaera\" serve\n",
        ] {
            assert!(script.contains(needle), "missing {needle:?} in:\n{script}");
        }
        // Resources live in srun argv, never in the script; prelude precedes
        // exec; loopback default.
        assert!(!script.contains("#SBATCH"));
        assert!(script.find("bcftools").unwrap() < script.find("exec \"").unwrap());
        assert!(!script.contains("--bind-routable"));

        let script = render_launch_script(Path::new("/b"), Path::new("/r"), "", true);
        assert!(script.ends_with("serve --bind-routable\n"));
        // Empty prelude: no prelude banner at all.
        assert!(!script.contains("environment prelude"));
    }

    #[test]
    fn srun_args_carry_resources_and_detached_line_quotes() {
        let s = spec();
        // The handler passes the UNIQUE per-launch name (slug + token).
        let job_name = format!("chimaera-{}~ab12cd", slug(&s.name));
        let args = srun_args(&s, &job_name, Path::new("/r/scripts/a.sh"));
        assert_eq!(
            args,
            vec![
                "--job-name=chimaera-my-align-run~ab12cd".to_string(),
                "--time=4:00:00".to_string(),
                "--partition=normal".to_string(),
                "--cpus-per-task=4".to_string(),
                "--mem=16G".to_string(),
                "bash".to_string(),
                "-l".to_string(),
                "/r/scripts/a.sh".to_string(),
            ]
        );
        let mut g = spec();
        g.gres = Some("gpu:1".to_string());
        assert!(
            srun_args(&g, "chimaera-x~t0", Path::new("/s")).contains(&"--gres=gpu:1".to_string())
        );

        // The detached line: setsid+nohup+& (tmux-grade persistence), every
        // token single-quoted, output into the launch log. The env scrub
        // comes FIRST — an inherited SLURM_JOB_ID would turn srun into a
        // step of the surrounding allocation instead of a new job — and the
        // umask keeps the shared-FS log owner-only.
        let line = detached_srun_line(
            Path::new("/usr/bin/srun"),
            &args,
            Path::new("/r/logs/x.log"),
        );
        assert!(line.starts_with(
            "unset SLURM_JOB_ID SLURM_JOBID; umask 077; setsid nohup '/usr/bin/srun' '--job-name=chimaera-my-align-run~ab12cd'"
        ));
        assert!(line.contains(">> '/r/logs/x.log' 2>&1 < /dev/null &"));
    }

    #[test]
    fn directive_charsets_reject_injection() {
        assert!(safe_directive("4:00:00", ":-"));
        assert!(safe_directive("1-00:00:00", ":-"));
        assert!(!safe_directive("4:00:00\n#SBATCH --uid=0", ":-"));
        assert!(!safe_directive("normal; rm -rf /", "_-."));
        assert!(!safe_directive("", ":-"));
        assert!(safe_directive("gpu:a100:2", ":_,-."));
    }
}
