//! Mode 2 — chimaera sessions AS Slurm jobs. The login-node daemon is the
//! control plane: it owns sbatch (via `compute::Detection`), the environment
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

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::compute::Detection;
use crate::AppState;

/// A launch request. Everything lands in an sbatch script, so every field
/// is validated against a strict charset — never interpolate free text into
/// directives (the prelude body is the one deliberate exception: it is the
/// user's own shell text, same trust as their rc, and it lives in the
/// script BODY, not in `#SBATCH` lines).
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

/// Job-name slug: lowercase alnum/dash, bounded — goes into `#SBATCH
/// --job-name=chimaera-<slug>` and the discovery prefix match.
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

/// Charset gate for values that land in `#SBATCH` directive lines.
fn safe_directive(v: &str, extra: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// The sbatch script. `bash -l` so the prelude sees profile functions
/// (`ml`, conda) exactly like the agent login-wrapper; `exec` so the daemon
/// IS the job's main process — walltime/scancel kill the whole tree. The
/// job daemon gets an isolated CHIMAERA_HOME keyed by jobid: own manifest,
/// token, sessions — multi-job and multi-user safe by construction.
fn render_sbatch(
    spec: &LaunchSpec,
    job_slug: &str,
    bin: &Path,
    root: &Path,
    prelude: &str,
) -> String {
    let mut s = String::from("#!/bin/bash -l\n");
    s.push_str(&format!("#SBATCH --job-name=chimaera-{job_slug}\n"));
    s.push_str(&format!("#SBATCH --time={}\n", spec.time));
    if let Some(p) = &spec.partition {
        s.push_str(&format!("#SBATCH --partition={p}\n"));
    }
    if let Some(c) = spec.cpus {
        s.push_str(&format!("#SBATCH --cpus-per-task={c}\n"));
    }
    if let Some(m) = &spec.mem {
        s.push_str(&format!("#SBATCH --mem={m}\n"));
    }
    if let Some(g) = spec.gres.as_deref().filter(|g| !g.is_empty()) {
        s.push_str(&format!("#SBATCH --gres={g}\n"));
    }
    s.push_str(&format!(
        "#SBATCH --output={}/logs/%j.log\n\n",
        root.display()
    ));
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
    let flag = if spec.routable {
        " --bind-routable"
    } else {
        ""
    };
    s.push_str(&format!("exec \"{}\" serve{flag}\n", bin.display()));
    s
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

    let Detection::Slurm { sbatch, .. } = state.compute.detection().await else {
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
    let script = render_sbatch(&spec, &job_slug, &bin, &root, &prelude);

    // Blocking fs (shared FS!) off the reactor: dirs + the script file.
    let script_path = {
        let root = root.clone();
        let res = tokio::task::spawn_blocking(move || -> anyhow::Result<PathBuf> {
            std::fs::create_dir_all(root.join("logs"))?;
            std::fs::create_dir_all(root.join("scripts"))?;
            std::fs::create_dir_all(root.join("pending"))?;
            let path = root.join("scripts").join(format!(
                "{}-{}.sbatch",
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
                    Json(json!({"error": format!("cannot stage sbatch script: {e}")})),
                )
                    .into_response()
            }
        }
    };

    let out = crate::compute::run_capped(
        &sbatch.to_string_lossy(),
        &[
            "--parsable".into(),
            script_path.to_string_lossy().into_owned(),
        ],
    )
    .await;
    // `--parsable` prints `jobid[;cluster]`.
    let job_id = out
        .as_deref()
        .and_then(|o| o.lines().next())
        .map(|l| l.split(';').next().unwrap_or(l).trim().to_string())
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()));
    let Some(job_id) = job_id else {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "sbatch failed — check partition/resources and cluster health"})),
        )
            .into_response();
    };

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
    // Blocking joins (manifest/caps/record on possibly-NFS) off the reactor.
    let sessions = tokio::task::spawn_blocking(move || {
        candidates
            .into_iter()
            .map(|j| join_session(&root, j))
            .collect::<Vec<_>>()
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
        cpus: rec("cpus").and_then(|v| v.as_u64()).map(|v| v as u32),
        mem: rec("mem").and_then(|v| v.as_str().map(str::to_string)),
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
    tracing::info!(%job_id, "compute session cancel requested");
    StatusCode::NO_CONTENT.into_response()
}

/// Sortable second-resolution stamp for script filenames without pulling a
/// date crate: unix seconds, zero-padded.
fn chrono_free_ts() -> String {
    format!(
        "{:012}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    )
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
    fn render_sbatch_directives_prelude_and_exec() {
        let s = spec();
        let script = render_sbatch(
            &s,
            &slug(&s.name),
            Path::new("/home/u/.chimaera-dev/bin/chimaera"),
            Path::new("/home/u/.chimaera-dev/data/compute"),
            "ml biology bcftools\nexport X=1",
        );
        for needle in [
            "#!/bin/bash -l",
            "#SBATCH --job-name=chimaera-my-align-run",
            "#SBATCH --time=4:00:00",
            "#SBATCH --partition=normal",
            "#SBATCH --cpus-per-task=4",
            "#SBATCH --mem=16G",
            "CHIMAERA_HOME=\"/home/u/.chimaera-dev/data/compute/${SLURM_JOB_ID}\"",
            "unset CHIMAERA_PRELUDE CHIMAERA_PRELUDE_DONE",
            "ml biology bcftools",
            "caps.json",
            "exec \"/home/u/.chimaera-dev/bin/chimaera\" serve\n",
        ] {
            assert!(script.contains(needle), "missing {needle:?} in:\n{script}");
        }
        // No gres → no gres line; prelude precedes exec; loopback default.
        assert!(!script.contains("--gres"));
        assert!(script.find("bcftools").unwrap() < script.find("exec \"").unwrap());
        assert!(!script.contains("--bind-routable"));

        let mut routable = spec();
        routable.gres = Some("gpu:1".to_string());
        routable.routable = true;
        let script = render_sbatch(&routable, "x", Path::new("/b"), Path::new("/r"), "");
        assert!(script.contains("#SBATCH --gres=gpu:1"));
        assert!(script.ends_with("serve --bind-routable\n"));
        // Empty prelude: no prelude banner at all.
        assert!(!script.contains("environment prelude"));
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
