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
        "export CHIMAERA_HOME=\"{}/${{SLURM_JOB_ID}}\"\n\
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
         code=$(curl -sS -m 8 -o /dev/null -w '%{http_code}' https://api.anthropic.com/ 2>/dev/null || echo 0)\n\
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
fn srun_args(spec: &LaunchSpec, job_slug: &str, script: &Path) -> Vec<String> {
    let mut a = vec![
        format!("--job-name=chimaera-{job_slug}"),
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
    format!(
        "setsid nohup {} >> {} 2>&1 < /dev/null & echo detached",
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
    let root = compute_root();
    let script = render_launch_script(&bin, &root, &prelude, spec.routable);

    // Blocking fs (shared FS!) off the reactor: dirs + the script file.
    let script_path = {
        let root = root.clone();
        let res = tokio::task::spawn_blocking(move || -> anyhow::Result<PathBuf> {
            std::fs::create_dir_all(root.join("logs"))?;
            std::fs::create_dir_all(root.join("scripts"))?;
            std::fs::create_dir_all(root.join("pending"))?;
            let path = root.join("scripts").join(format!(
                "{}-{}.sh",
                chrono_free_ts(),
                &chimaera_core::generate_token()[..6]
            ));
            std::fs::write(&path, &script)?;
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
    let log_path = root.join("logs").join(format!(
        "srun-{}-{}.log",
        chrono_free_ts(),
        &chimaera_core::generate_token()[..6]
    ));
    let line = detached_srun_line(&srun, &srun_args(&spec, &job_slug, &script_path), &log_path);
    let detached = crate::compute::run_capped("bash", &["-c".into(), line]).await;
    if detached.is_none() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "could not start srun on this host"})),
        )
            .into_response();
    }
    // The job registers in squeue within moments (PENDING while it waits
    // for resources). Adopt the newest unrecorded row wearing this launch's
    // name; a refusal never registers, so after the retries the log tail is
    // the diagnosis — Slurm's own words, the round-6 promise kept.
    let mut job_id: Option<String> = None;
    for _ in 0..4 {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        job_id = adopt_submitted(&state, &format!("chimaera-{job_slug}"), &root).await;
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
        return (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response();
    };

    // Seed the job daemon's workspace registry over the shared FS: the job
    // will `mkdir -p` this same home, and its WorkspaceStore then boots with
    // this host's WHOLE workspace list already registered (shared-FS roots
    // are equally valid on the node) — the compute window lands on the same
    // ready-to-open workspaces as the login window, instead of a bare "open
    // a folder" page (the maintainer hit exactly that dead end on first
    // use; launches from the host page carry no workspace_id, so seeding
    // only the launch workspace left the window empty on second use).
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

    // The launch record: the card's resource numbers while Slurm is the
    // only other truth (and after; squeue doesn't report mem/gres cheaply).
    let record = json!({
        "name": format!("chimaera-{job_slug}"),
        "display_name": spec.name,
        "partition": spec.partition,
        "cpus": spec.cpus,
        "mem": spec.mem,
        "gres": spec.gres,
        "time": spec.time,
        "workspace_id": spec.workspace_id,
        "routable": spec.routable,
    });
    let rec_path = root.join("pending").join(format!("{job_id}.json"));
    if let Err(e) = tokio::task::spawn_blocking(move || {
        std::fs::write(
            rec_path,
            serde_json::to_vec_pretty(&record).unwrap_or_default(),
        )
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r.map_err(Into::into))
    {
        tracing::warn!(%e, %job_id, "launch record write failed (card shows without resources)");
    }

    tracing::info!(%job_id, slug = %job_slug, "compute session submitted");
    Json(json!({ "job_id": job_id })).into_response()
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
/// the job id the way `sbatch --parsable` did): force-refresh the queue and
/// pick the newest job wearing `job_name` that no launch record claims. The
/// fs check runs off the reactor (shared FS).
async fn adopt_submitted(state: &Arc<AppState>, job_name: &str, root: &Path) -> Option<String> {
    state.compute.invalidate().await;
    let snap = state.compute.snapshot(true).await;
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
    // Blocking joins (manifest/caps/record on possibly-NFS) off the reactor.
    let sessions = tokio::task::spawn_blocking(move || {
        let mut sessions = candidates
            .into_iter()
            .map(|j| join_session(&root, j))
            .collect::<Vec<_>>();
        // A stale-jobs snapshot (squeue failed) must not turn live jobs
        // into tombstones — skip the orphan sweep on degraded rounds.
        if !degraded {
            append_ended_sessions(&root, &mut sessions);
        }
        sessions
    })
    .await
    .unwrap_or_default();
    Json(json!({
        "scheduler": snap.scheduler,
        "sessions": sessions,
        "partitions": snap.partitions,
    }))
    .into_response()
}

/// The listing's second half: launch records whose job has LEFT the queue.
/// A walltime death otherwise erases the card mid-session with zero trace —
/// "the job sometimes disappears?" (the maintainer, live). Ended cards are
/// dismissable tombstones: DELETE marks the record cancelled, and this sweep
/// removes marked or aged-out (48h) records — self-cleaning, no daemon state.
fn append_ended_sessions(root: &Path, sessions: &mut Vec<ComputeSession>) {
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
        let state = orphan_state(age);
        if dismissed || state.is_none() || record.is_none() {
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

/// What an orphaned launch record (no squeue row for its job) means, by
/// age. A seconds-old record is a JUST-submitted job that (a possibly
/// cached) squeue hasn't shown yet — PENDING, not dead (found live: a
/// fresh launch briefly wore an "ended" card). Hours old is a session that
/// ended without an explicit cancel — the tombstone. Two days old is
/// litter (None = remove the record).
fn orphan_state(age: Duration) -> Option<&'static str> {
    const SUBMIT_GRACE: Duration = Duration::from_secs(120);
    const ENDED_MAX_AGE: Duration = Duration::from_secs(48 * 3600);
    if age < SUBMIT_GRACE {
        Some("PENDING")
    } else if age < ENDED_MAX_AGE {
        Some("ENDED")
    } else {
        None
    }
}

fn join_session(root: &Path, j: crate::compute::Job) -> ComputeSession {
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
    let rec = |k: &str| record.as_ref().and_then(|r| r.get(k).cloned());
    let running = j.state == "RUNNING";
    ComputeSession {
        ready: running && manifest.is_some(),
        name: rec("display_name")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| j.name.trim_start_matches("chimaera-").to_string()),
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
        state: j.state,
        node: j.nodes,
        partition: j.partition,
        time_left: j.time_left,
    }
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
    let _ =
        crate::compute::run_capped(&scancel.to_string_lossy(), std::slice::from_ref(&job_id)).await;
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
        // Seconds old = just submitted, squeue hasn't caught up — PENDING.
        assert_eq!(orphan_state(Duration::from_secs(5)), Some("PENDING"));
        assert_eq!(orphan_state(Duration::from_secs(119)), Some("PENDING"));
        // Past the grace window it ended without a cancel — the tombstone.
        assert_eq!(orphan_state(Duration::from_secs(121)), Some("ENDED"));
        assert_eq!(orphan_state(Duration::from_secs(47 * 3600)), Some("ENDED"));
        // Two days on, the record is litter to sweep.
        assert_eq!(orphan_state(Duration::from_secs(49 * 3600)), None);
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
        let args = srun_args(&s, &slug(&s.name), Path::new("/r/scripts/a.sh"));
        assert_eq!(
            args,
            vec![
                "--job-name=chimaera-my-align-run".to_string(),
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
        assert!(srun_args(&g, "x", Path::new("/s")).contains(&"--gres=gpu:1".to_string()));

        // The detached line: setsid+nohup+& (tmux-grade persistence), every
        // token single-quoted, output into the launch log.
        let line = detached_srun_line(
            Path::new("/usr/bin/srun"),
            &args,
            Path::new("/r/logs/x.log"),
        );
        assert!(line.starts_with("setsid nohup '/usr/bin/srun' '--job-name=chimaera-my-align-run'"));
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
