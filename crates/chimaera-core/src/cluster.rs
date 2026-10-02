//! A cluster's own chimaera folder: the records the app and the CLI keep on a
//! cluster's shared home (its workspaces, saved setups, rules for agents,
//! learned facts, and each job), the job script that starts a job's
//! `chimaera job-host`, and the shapes job-host and `chimaera browse` speak.
//! Pure types and text — the ssh transport lives in `chimaera-remote`, the
//! in-job processes in `chimaera-server`.
//!
//! The model (docs/design/hpc-portal-plan.md): you start **jobs**, and open
//! **workspaces** inside them. A workspace keeps its chats in its own folder,
//! so it can be open in at most one job at a time and move between jobs.
//!
//! Layout under the cluster folder (owner-only):
//!
//! ```text
//! cluster.json            ClusterConfig
//! w/<wid>/data/           the workspace's chimaera data dir (manifest = lease)
//! w/<wid>/workspace.json  WorkspaceSeed: what its chimaera registers
//! w/<wid>/serve.log       its chimaera's output, last run only
//! j/<jid>/job.json        JobRecord
//! j/<jid>/job.sh, job.log the script and Slurm's output
//! j/<jid>/startup.sh      this job's startup commands
//! j/<jid>/agent-rules.md  the user's rules for agents, as of the start
//! j/<jid>/facts.json      ClusterFacts, as of the start (what agents are told)
//! j/<jid>/host.json       HostRecord: where job-host listens (written in the job)
//! j/<jid>/caps.json       the job's egress probe
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::slurm::{GpuFlag, LaunchSpec, PartitionChoice, Scheduler};

/// A workspace chimaera's data dir (manifest, ledger, journals, timeline) —
/// the workspace's own folder on the shared filesystem.
pub const ENV_DATA_DIR: &str = "CHIMAERA_DATA_DIR";
/// A workspace chimaera's runtime dir — node-local, gone with the job.
pub const ENV_RUNTIME_DIR: &str = "CHIMAERA_RUNTIME_DIR";
/// Set on a daemon the user started on a host they said is not a cluster
/// (though Slurm is on its PATH): its agents aren't told they are on a
/// shared login node.
pub const ENV_NOT_A_CLUSTER: &str = "CHIMAERA_NOT_A_CLUSTER";

/// Set (to `1`) on a workspace's chimaera when its job is attached: held by
/// the app that started it, it can't continue in a new job.
pub const ENV_JOB_ATTACHED: &str = "CHIMAERA_JOB_ATTACHED";

/// Path of the [`WorkspaceSeed`] a workspace chimaera registers at boot.
pub const ENV_CLUSTER_WORKSPACE: &str = "CHIMAERA_CLUSTER_WORKSPACE";
/// A file of startup commands (this job's) applied as the outermost prelude
/// scope of every shell and agent a workspace chimaera spawns.
pub const ENV_HOST_PRELUDE_FILE: &str = "CHIMAERA_HOST_PRELUDE_FILE";
/// Rules-for-agents text the user wrote for this cluster.
pub const ENV_AGENT_RULES_FILE: &str = "CHIMAERA_AGENT_RULES_FILE";
/// A file ON the cluster the user pointed at as its rules for agents (read
/// at bake time, so it's always the cluster's current text).
pub const ENV_AGENT_RULES_SOURCE: &str = "CHIMAERA_AGENT_RULES_SOURCE";
/// The cluster's Slurm setup as discovered ([`ClusterFacts`]), for what
/// agents are told about submitting their own jobs.
pub const ENV_CLUSTER_FACTS_FILE: &str = "CHIMAERA_CLUSTER_FACTS_FILE";

/// `cluster.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterConfig {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub workspaces: Vec<ClusterWorkspace>,
    /// Named setups the user saved.
    #[serde(default)]
    pub setups: Vec<Setup>,
    /// The setup the last job on this cluster started with — the start
    /// sheet's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_spec: Option<LaunchSpec>,
    #[serde(default)]
    pub agent_rules: AgentRules,
    #[serde(default)]
    pub learned: Learned,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterWorkspace {
    /// `w-<8 hex>`: names the folder and the workspace chimaera's id.
    pub id: String,
    pub name: String,
    /// Absolute path on the cluster's shared filesystem.
    pub path: String,
    #[serde(default)]
    pub created_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    pub name: String,
    pub spec: LaunchSpec,
}

/// What agents on this cluster are told about its rules. Neither set → a
/// short generic paragraph (explicit time limits, polite queue checks,
/// nothing left running on login nodes).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRules {
    /// A file on the cluster holding its published rules for agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Text the user wrote or pasted (always on the wire: pages read it as
    /// a string).
    #[serde(default)]
    pub text: String,
}

/// Per-cluster facts learned from the cluster's own refusals, so the next
/// start doesn't fail the same way.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Learned {
    /// Partitions that refused a batch job: started in the foreground,
    /// held by the app's connection, instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interactive_only: Vec<String>,
    /// Fields the cluster insisted on: `"account"`, `"qos"`, `"constraint"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

/// What the cluster's scheduler offers this user — the start sheet's
/// choices, and the table agents get about submitting their own jobs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterFacts {
    #[serde(default)]
    pub scheduler: Scheduler,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub partitions: Vec<PartitionChoice>,
    /// The user's accounts (empty when the cluster keeps none).
    #[serde(default)]
    pub accounts: Vec<String>,
    #[serde(default)]
    pub default_account: Option<String>,
    #[serde(default)]
    pub gpu_flag: GpuFlag,
    #[serde(default)]
    pub fetched_ms: u64,
}

/// `j/<jid>/job.json` — one job: what it was started with, what opens when
/// it starts, and how it ended.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    /// `j-<8 hex>`, minted before `sbatch`: names the folder.
    pub id: String,
    /// What the page calls it: the saved setup's name, else
    /// `{partition} · {time}`.
    pub name: String,
    /// The name Slurm knows it by (`chimaera-<slug>~<token>`).
    pub job_name: String,
    pub spec: LaunchSpec,
    /// Workspaces to open when it starts.
    #[serde(default)]
    pub open: Vec<String>,
    /// This job's own startup commands (also in `startup.sh`) — what a
    /// "continue in a new job" starts with.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub startup: String,
    /// The job (`jid`) this one continues: once it runs, it stops that one
    /// and takes its workspaces over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    /// `None` until Slurm handed out an id (an attached start learns it from
    /// the queue by its unique name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slurm_job_id: Option<String>,
    /// Held in the foreground by an app's connection (an interactive-only
    /// partition): it ends when that connection does.
    #[serde(default)]
    pub attached: bool,
    pub submitted_ms: u64,
    /// The user stopped it — the ended line says so instead of a reason.
    #[serde(default)]
    pub stopped_by_user: bool,
    /// How it ended, asked of `sacct` once and kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<Ended>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ended {
    /// Slurm's terminal state (`TIMEOUT`, `CANCELLED`, `FAILED`,
    /// `PREEMPTED`, `NODE_FAIL`, …) or `ENDED` when accounting can't say.
    pub state: String,
    pub at_ms: u64,
}

/// `j/<jid>/host.json` — where a running job's job-host listens. Written
/// atomically, owner-only (it carries the token), by job-host itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRecord {
    pub job: String,
    pub slurm_job_id: String,
    pub node: String,
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub started_ms: u64,
    #[serde(default)]
    pub build: String,
}

/// `w/<wid>/workspace.json` — what a workspace chimaera registers at boot,
/// under the cluster's id so every job it opens in shares one identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSeed {
    pub id: String,
    pub name: String,
    pub path: String,
}

// --- job-host's API --------------------------------------------------------------

/// A workspace inside a job, as job-host reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedState {
    /// Its chimaera is starting (or waiting for the job it moves from to
    /// let go).
    #[default]
    Starting,
    Open,
    Closing,
    /// Its chimaera exited on its own; `detail` carries its last words.
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedWorkspace {
    pub id: String,
    pub state: HostedState,
    /// Its chimaera's port on the node, once it listens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Agents working right now (the page's "2 chats working").
    #[serde(default)]
    pub working: u32,
    /// Its chimaera's token, once open: job-host reads it where it was
    /// written, so a client never waits for the shared filesystem to show
    /// the new manifest on the login node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// `j/<jid>/workspaces.json`: what a job's job-host holds right now —
/// opening, open, closing or failed — so the cluster page tells a workspace
/// that job is still opening (or one closed in it since) from the files alone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostingRecord {
    #[serde(default)]
    pub workspaces: BTreeMap<String, HostedState>,
}

/// `GET /api/v1/job`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobHostStatus {
    pub job: String,
    pub slurm_job_id: String,
    pub node: String,
    pub workspaces: Vec<HostedWorkspace>,
}

/// `409` from `POST /api/v1/job/workspaces/{wid}/open`: another live job
/// holds the workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldElsewhere {
    pub error: String,
    pub slurm_job_id: String,
}

// --- chimaera browse --------------------------------------------------------------

/// `chimaera browse --state`: the cluster folder in one read. Tokens ride
/// along (`host`, manifests): the output goes only to the user's own ssh
/// client, which keeps them out of every page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BrowseState {
    #[serde(default)]
    pub config: ClusterConfig,
    #[serde(default)]
    pub jobs: Vec<JobFiles>,
    /// Each workspace's manifest, when its chimaera runs (or left one).
    #[serde(default)]
    pub manifests: BTreeMap<String, crate::Manifest>,
    /// When each workspace was last open (its chimaera's log, written while
    /// it ran), epoch ms.
    #[serde(default)]
    pub last_open_ms: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct JobFiles {
    pub record: JobRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostRecord>,
    /// The job's node reaches the internet (agents can work), when probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<bool>,
    /// What its job-host holds, once it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosting: Option<HostingRecord>,
}

/// `chimaera browse --dir`: one folder's subfolders, for choosing a
/// workspace. Folders only — files belong in a workspace window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirListing {
    /// The folder, resolved (absolute, symlinks followed).
    pub path: String,
    /// Its parent, `None` at `/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The folder itself is already a workspace on this cluster: its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub folders: Vec<Folder>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    /// Holds a `.git` (a repository's root).
    #[serde(default)]
    pub git: bool,
    /// Already a workspace on this cluster: its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// Expand a path the user typed — `~`, `~/x`, `$VAR/x`, `${VAR}/x` — from
/// `env`, never through a shell. A variable that isn't set is an error the
/// user can act on, not an empty segment.
pub fn expand_path(raw: &str, env: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("Enter a folder on the cluster".into());
    }
    if raw.len() > 4096 || raw.contains(['\0', '\n']) {
        return Err("That isn't a path".into());
    }
    let home = || env("HOME").ok_or_else(|| "HOME isn't set on the cluster".to_string());
    let mut out = match raw.strip_prefix('~') {
        Some("") => home()?,
        Some(rest) if rest.starts_with('/') => format!("{}{rest}", home()?),
        Some(_) => return Err("Only your own ~ can be expanded".into()),
        None => String::new(),
    };
    let rest = if raw.starts_with('~') { "" } else { raw };
    let mut chars = rest.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let (name, end) = if rest[i + 1..].starts_with('{') {
            let close = rest[i + 2..]
                .find('}')
                .ok_or_else(|| "A ${ is missing its }".to_string())?;
            (&rest[i + 2..i + 2 + close], i + 2 + close + 1)
        } else {
            let len = rest[i + 1..]
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                .unwrap_or(rest.len() - i - 1);
            (&rest[i + 1..i + 1 + len], i + 1 + len)
        };
        if name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            return Err(format!("${name} isn't a variable name"));
        }
        let value = env(name).ok_or_else(|| format!("${name} isn't set on the cluster"))?;
        out.push_str(&value);
        while chars.peek().is_some_and(|(j, _)| *j < end) {
            chars.next();
        }
    }
    if !out.starts_with('/') {
        return Err(format!("{raw} isn't a full path (start with / or ~)"));
    }
    Ok(out)
}

/// The most job folders `browse --state` reads (newest first): ended jobs
/// stay until dismissed, and a forgotten pile must not slow every refresh.
pub const BROWSE_MAX_JOBS: usize = 100;
/// The most folders one `browse --dir` lists.
pub const BROWSE_MAX_FOLDERS: usize = 2000;
/// Folders checked for a `.git` (one stat each, on a network filesystem).
const BROWSE_GIT_CHECKS: usize = 500;

fn read_json_file<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(1024 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// `chimaera browse --state`: read the cluster folder — never write, create
/// or lock anything in it.
pub fn browse_state(cluster_dir: &std::path::Path) -> BrowseState {
    let config: ClusterConfig =
        read_json_file(&cluster_dir.join("cluster.json")).unwrap_or_default();
    let mut jobs: Vec<(std::time::SystemTime, JobFiles)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(cluster_dir.join("j")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !valid_job_id(&name) {
                continue;
            }
            let dir = e.path();
            let Some(record) = read_json_file::<JobRecord>(&dir.join("job.json")) else {
                continue;
            };
            if record.id != name {
                continue;
            }
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            let egress = read_json_file::<serde_json::Value>(&dir.join("caps.json"))
                .and_then(|c| c.get("egress").and_then(|v| v.as_bool()));
            jobs.push((
                modified,
                JobFiles {
                    record,
                    host: read_json_file(&dir.join("host.json")),
                    egress,
                    hosting: read_json_file(&dir.join("workspaces.json")),
                },
            ));
        }
    }
    jobs.sort_by(|a, b| {
        b.1.record
            .submitted_ms
            .cmp(&a.1.record.submitted_ms)
            .then(b.0.cmp(&a.0))
    });
    jobs.truncate(BROWSE_MAX_JOBS);
    let mut manifests = BTreeMap::new();
    let mut last_open_ms = BTreeMap::new();
    for ws in &config.workspaces {
        if !valid_workspace_id(&ws.id) {
            continue;
        }
        let dir = cluster_dir.join("w").join(&ws.id);
        if let Some(m) = read_json_file::<crate::Manifest>(&dir.join("data").join("manifest.json"))
        {
            manifests.insert(ws.id.clone(), m);
        }
        let last = std::fs::metadata(dir.join("serve.log"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);
        if let Some(last) = last {
            last_open_ms.insert(ws.id.clone(), last);
        }
    }
    BrowseState {
        config,
        jobs: jobs.into_iter().map(|(_, j)| j).collect(),
        manifests,
        last_open_ms,
    }
}

/// `chimaera browse --dir`: one folder's subfolders, for choosing a
/// workspace — dot-folders left out, each marked `git` or already a
/// workspace (`cluster_dir`'s `cluster.json`). Read-only.
pub fn browse_dir(raw: &str, cluster_dir: Option<&std::path::Path>) -> Result<DirListing, String> {
    let expanded = expand_path(raw, |name| std::env::var(name).ok())?;
    let path = std::fs::canonicalize(&expanded)
        .map_err(|_| format!("{raw} isn't a folder you can open"))?;
    if !path.is_dir() {
        return Err(format!("{raw} isn't a folder"));
    }
    let workspaces: BTreeMap<String, String> = cluster_dir
        .and_then(|c| read_json_file::<ClusterConfig>(&c.join("cluster.json")))
        .map(|c| c.workspaces.into_iter().map(|w| (w.path, w.id)).collect())
        .unwrap_or_default();
    let entries = std::fs::read_dir(&path).map_err(|_| format!("{raw} can't be read"))?;
    let mut folders: Vec<Folder> = Vec::new();
    let mut truncated = false;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let child = e.path();
        // Follows symlinks: a linked project folder is a folder.
        if !std::fs::metadata(&child).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        if folders.len() >= BROWSE_MAX_FOLDERS {
            truncated = true;
            break;
        }
        let full = child.to_string_lossy().to_string();
        folders.push(Folder {
            workspace: workspaces.get(&full).cloned(),
            git: false,
            name,
        });
    }
    folders.sort_by_key(|f| f.name.to_lowercase());
    for f in folders.iter_mut().take(BROWSE_GIT_CHECKS) {
        f.git = path.join(&f.name).join(".git").exists();
    }
    let here = path.to_string_lossy().to_string();
    Ok(DirListing {
        parent: path.parent().map(|p| p.to_string_lossy().to_string()),
        workspace: workspaces.get(&here).cloned(),
        path: here,
        folders,
        truncated,
    })
}

// --- Ids ---------------------------------------------------------------------------

/// A fresh workspace id: `w-` + 8 hex.
pub fn new_workspace_id() -> String {
    format!("w-{}", &crate::generate_token()[..8])
}

/// A fresh job id: `j-` + 8 hex.
pub fn new_job_id() -> String {
    format!("j-{}", &crate::generate_token()[..8])
}

fn valid_id(prefix: &str, id: &str) -> bool {
    id.len() == 10 && id.starts_with(prefix) && id[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Whether `id` is one this module minted — the only shape allowed into a
/// path or a shell line.
pub fn valid_workspace_id(id: &str) -> bool {
    valid_id("w-", id)
}

/// [`valid_workspace_id`] for jobs.
pub fn valid_job_id(id: &str) -> bool {
    valid_id("j-", id)
}

/// A Slurm job id as printed (`12345`, `12345_7` for an array task) — the
/// only shape allowed into a scheduler command line.
pub fn valid_slurm_job_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_digit() || c == '_')
}

/// What the page calls a job without a saved setup's name:
/// `{partition} · {time}` in plain words (`batch · 3 days`).
pub fn job_display_name(spec: &LaunchSpec) -> String {
    let time = crate::slurm::parse_duration(&spec.time)
        .map(|d| {
            let secs = d.as_secs();
            let (days, hours, mins) = (secs / 86_400, (secs % 86_400) / 3_600, (secs % 3_600) / 60);
            let unit = |n: u64, one: &str| {
                if n == 1 {
                    format!("1 {one}")
                } else {
                    format!("{n} {one}s")
                }
            };
            if days > 0 && hours == 0 {
                unit(days, "day")
            } else if days > 0 {
                format!("{days}d {hours}h")
            } else if hours > 0 && mins == 0 {
                unit(hours, "hour")
            } else if hours > 0 {
                format!("{hours}h {mins}m")
            } else {
                unit(mins.max(1), "minute")
            }
        })
        .unwrap_or_else(|| spec.time.clone());
    match spec.partition.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => format!("{p} · {time}"),
        None => time,
    }
}

// --- The job script ----------------------------------------------------------------

/// Where a job script finds its pieces. Every path is `$HOME`-anchored text
/// expanded by bash on the compute node (the same `$HOME` the login node
/// has).
pub struct JobScript<'a> {
    /// The chimaera binary on the shared filesystem.
    pub binary: &'a str,
    /// The job's folder (`…/cluster/j/<jid>`).
    pub job_dir: &'a str,
    /// A dev build's state home (`$HOME/.chimaera-dev`), so the job's
    /// chimaeras keep their config and caches apart from a release's.
    /// `None` for a release.
    pub state_home: Option<&'a str>,
    /// A file on the cluster holding its rules for agents, if the user named
    /// one.
    pub rules_source: Option<&'a str>,
}

/// The script a job runs: probe whether this node reaches the internet
/// (agents need it), then `exec chimaera job-host`, so job-host IS the job —
/// on the compute node Slurm gave it, never the login node — and walltime
/// and `scancel` stop exactly it, gracefully (SIGTERM first). job-host opens
/// workspaces inside the job; each workspace's chimaera gets its own env.
///
/// Startup commands are NOT run here: every workspace chimaera applies them
/// to every shell and agent it spawns, like every other prelude scope.
pub fn job_script(p: &JobScript) -> String {
    let mut s = String::from("#!/bin/bash -l\n");
    s.push_str(
        "# A chimaera job, written by the chimaera app. It runs chimaera's\n\
         # job-host on this compute node; workspaces open inside it.\n\
         # Owner-only: these folders are on a shared filesystem, and the\n\
         # records in them carry the tokens that guard the job.\n\
         umask 077\n",
    );
    s.push_str(&format!("J=\"{}\"\n", p.job_dir));
    if let Some(home) = p.state_home {
        s.push_str(&format!("export CHIMAERA_HOME=\"{home}\"\n"));
    }
    if let Some(src) = p.rules_source {
        s.push_str(&format!(
            "export {ENV_AGENT_RULES_SOURCE}={}\n",
            crate::slurm::sh_quote(src)
        ));
    }
    s.push_str(&format!(
        "unset CHIMAERA_PRELUDE CHIMAERA_PRELUDE_DONE {ENV_DATA_DIR} {ENV_RUNTIME_DIR} \
         {ENV_CLUSTER_WORKSPACE} {ENV_HOST_PRELUDE_FILE}\n\n"
    ));
    s.push_str(
        "# Whether agents can reach their API from this node: a per-cluster fact,\n\
         # probed where it matters. curl prints 000 and exits non-zero on failure;\n\
         # the digits-only, base-10 cleanup keeps caps.json valid JSON.\n\
         code=$(curl -sS -m 8 -o /dev/null -w '%{http_code}' https://api.anthropic.com/ 2>/dev/null) || true\n\
         code=$(printf '%s' \"$code\" | tr -cd '0-9')\n\
         code=$((10#${code:-0}))\n\
         printf '{\"egress\":%s,\"http_code\":%s,\"probed_at\":%s}\\n' \\\n\
         \x20 \"$([ \"$code\" -ge 200 ] && echo true || echo false)\" \"$code\" \"$(date +%s)\" \\\n\
         \x20 > \"$J/caps.json\"\n\n",
    );
    s.push_str(&format!(
        "exec \"{}\" job-host --job-dir \"$J\"\n",
        p.binary
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_and_tolerates_missing_and_old_fields() {
        let c: ClusterConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, ClusterConfig::default());
        // A revision-1 file (per-workspace startup and setups) still reads.
        let c: ClusterConfig = serde_json::from_str(
            r#"{"version":1,"startup":"ml x","workspaces":[{"id":"w-0000abcd","name":"x","path":"/p","startup":"y","last_spec":{"time":"1:00:00"}}],"unknown":true}"#,
        )
        .unwrap();
        assert_eq!(c.workspaces[0].path, "/p");
        let back: ClusterConfig =
            serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn ids_are_strict() {
        let w = new_workspace_id();
        let j = new_job_id();
        assert!(valid_workspace_id(&w) && !valid_job_id(&w), "{w}");
        assert!(valid_job_id(&j) && !valid_workspace_id(&j), "{j}");
        for bad in [
            "w-1234567",
            "w-12345678x",
            "x-12345678",
            "w-1234567g",
            "w-../../x",
        ] {
            assert!(!valid_workspace_id(bad), "{bad}");
        }
        assert!(valid_slurm_job_id("123") && valid_slurm_job_id("123_4"));
        for bad in ["", "12;3", "-1", "1 2", "$(x)"] {
            assert!(!valid_slurm_job_id(bad), "{bad}");
        }
    }

    #[test]
    fn job_names_read_as_plain_words() {
        let spec = |p: Option<&str>, t: &str| LaunchSpec {
            partition: p.map(str::to_string),
            time: t.into(),
            ..Default::default()
        };
        assert_eq!(
            job_display_name(&spec(Some("batch"), "3-00:00:00")),
            "batch · 3 days"
        );
        assert_eq!(
            job_display_name(&spec(Some("gpu"), "4:00:00")),
            "gpu · 4 hours"
        );
        assert_eq!(job_display_name(&spec(None, "1-12:00:00")), "1d 12h");
        assert_eq!(
            job_display_name(&spec(Some("dev"), "1:30:00")),
            "dev · 1h 30m"
        );
        assert_eq!(
            job_display_name(&spec(Some("dev"), "1:00:00")),
            "dev · 1 hour"
        );
        assert_eq!(
            job_display_name(&spec(Some("dev"), "30:00")),
            "dev · 30 minutes"
        );
    }

    fn env(name: &str) -> Option<String> {
        match name {
            "HOME" => Some("/home/u".into()),
            "SCRATCH" => Some("/scratch/u".into()),
            "GROUP_HOME" => Some("/groups/g".into()),
            _ => None,
        }
    }

    #[test]
    fn paths_expand_without_a_shell() {
        assert_eq!(expand_path("~", env).unwrap(), "/home/u");
        assert_eq!(expand_path("~/proj", env).unwrap(), "/home/u/proj");
        assert_eq!(expand_path("$SCRATCH/crc", env).unwrap(), "/scratch/u/crc");
        assert_eq!(expand_path("${GROUP_HOME}/x", env).unwrap(), "/groups/g/x");
        assert_eq!(
            expand_path("/abs/$SCRATCH", env).unwrap(),
            "/abs//scratch/u"
        );
        assert_eq!(expand_path("  /a b/c  ", env).unwrap(), "/a b/c");
        assert!(expand_path("$NOPE/x", env)
            .unwrap_err()
            .contains("isn't set"));
        assert!(expand_path("~other/x", env).is_err());
        assert!(expand_path("relative", env).is_err());
        assert!(expand_path("${OPEN", env).is_err());
        assert!(expand_path("", env).is_err());
        // Nothing a shell would run is ever evaluated: it's just text.
        assert_eq!(
            expand_path("/x/$(id)", env).unwrap_err(),
            "$ isn't a variable name"
        );
    }

    #[test]
    fn job_script_execs_job_host_in_the_job() {
        let s = job_script(&JobScript {
            binary: "$HOME/.chimaera/bin/chimaera",
            job_dir: "$HOME/.chimaera/cluster/j/j-0000abcd",
            state_home: None,
            rules_source: Some("/etc/some rules.md"),
        });
        assert!(s.starts_with("#!/bin/bash -l\n"));
        assert!(s.contains("umask 077"));
        assert!(s.contains("export CHIMAERA_AGENT_RULES_SOURCE='/etc/some rules.md'"));
        assert!(
            !s.contains("CHIMAERA_HOME="),
            "a release keeps its normal config"
        );
        assert!(s.contains("> \"$J/caps.json\""));
        assert!(s
            .trim_end()
            .ends_with("exec \"$HOME/.chimaera/bin/chimaera\" job-host --job-dir \"$J\""));
        let dev = job_script(&JobScript {
            binary: "$HOME/.chimaera-dev/bin/chimaera",
            job_dir: "$HOME/.chimaera-dev/data/cluster/j/j-0000abcd",
            state_home: Some("$HOME/.chimaera-dev"),
            rules_source: None,
        });
        assert!(dev.contains("export CHIMAERA_HOME=\"$HOME/.chimaera-dev\""));
        assert!(!dev.contains("AGENT_RULES_SOURCE"));
    }

    #[test]
    fn job_script_is_valid_bash() {
        let s = job_script(&JobScript {
            binary: "$HOME/bin/chimaera",
            job_dir: "$HOME/j",
            state_home: Some("$HOME/.chimaera-dev"),
            rules_source: Some("/x/it's.md"),
        });
        let out = std::process::Command::new("bash")
            .args(["-n", "-c", &s])
            .output();
        if let Ok(out) = out {
            assert!(
                out.status.success(),
                "bash -n: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    fn scratch(label: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("chimaera-browse-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(&d).unwrap()
    }

    #[test]
    fn browse_dir_lists_folders_marking_git_and_workspaces() {
        let root = scratch("dir");
        let projects = root.join("projects");
        for d in ["b-proj/.git", "a-proj", ".hidden", "c-data"] {
            std::fs::create_dir_all(projects.join(d)).unwrap();
        }
        std::fs::write(projects.join("notes.txt"), "x").unwrap();
        let cluster = root.join("cluster");
        std::fs::create_dir_all(&cluster).unwrap();
        let config = ClusterConfig {
            workspaces: vec![ClusterWorkspace {
                id: "w-0000abcd".into(),
                name: "a".into(),
                path: projects.join("a-proj").to_string_lossy().to_string(),
                created_ms: 0,
            }],
            ..Default::default()
        };
        std::fs::write(
            cluster.join("cluster.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();
        let l = browse_dir(&projects.to_string_lossy(), Some(&cluster)).unwrap();
        let names: Vec<&str> = l.folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a-proj", "b-proj", "c-data"],
            "folders only, no dot-folders"
        );
        assert_eq!(l.folders[0].workspace.as_deref(), Some("w-0000abcd"));
        assert!(l.folders[1].git && !l.folders[0].git);
        assert_eq!(l.parent.as_deref(), Some(root.to_string_lossy().as_ref()));
        assert_eq!(l.workspace, None);
        let inside =
            browse_dir(&projects.join("a-proj").to_string_lossy(), Some(&cluster)).unwrap();
        assert_eq!(
            inside.workspace.as_deref(),
            Some("w-0000abcd"),
            "the folder itself is marked"
        );
        assert!(browse_dir(&projects.join("notes.txt").to_string_lossy(), None).is_err());
        assert!(browse_dir("/no/such/place", None).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn browse_state_reads_jobs_newest_first_and_manifests() {
        let c = scratch("state");
        let config = ClusterConfig {
            workspaces: vec![ClusterWorkspace {
                id: "w-0000abcd".into(),
                name: "a".into(),
                path: "/p".into(),
                created_ms: 0,
            }],
            ..Default::default()
        };
        std::fs::write(
            c.join("cluster.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();
        for (id, at) in [("j-0000aaaa", 1u64), ("j-0000bbbb", 2)] {
            let dir = c.join("j").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            let r = JobRecord {
                id: id.into(),
                submitted_ms: at,
                ..Default::default()
            };
            std::fs::write(dir.join("job.json"), serde_json::to_string(&r).unwrap()).unwrap();
        }
        std::fs::write(
            c.join("j/j-0000bbbb/caps.json"),
            r#"{"egress":true,"http_code":200}"#,
        )
        .unwrap();
        // A folder whose record names another job is ignored.
        std::fs::create_dir_all(c.join("j/j-0000cccc")).unwrap();
        std::fs::write(c.join("j/j-0000cccc/job.json"), r#"{"id":"j-0000aaaa","name":"","job_name":"","spec":{"time":"1:00"},"submitted_ms":9}"#).unwrap();
        let data = c.join("w/w-0000abcd/data");
        std::fs::create_dir_all(&data).unwrap();
        let m = crate::Manifest {
            hostname: "n1".into(),
            port: 4,
            token: "t".into(),
            pid: 1,
            version: "0".into(),
            started_at: 0,
            build: None,
            slurm_job_id: Some("7".into()),
            runtime_leases: false,
        };
        std::fs::write(
            data.join("manifest.json"),
            serde_json::to_string(&m).unwrap(),
        )
        .unwrap();
        let s = browse_state(&c);
        let ids: Vec<&str> = s.jobs.iter().map(|j| j.record.id.as_str()).collect();
        assert_eq!(ids, vec!["j-0000bbbb", "j-0000aaaa"]);
        assert_eq!(s.jobs[0].egress, Some(true));
        assert_eq!(s.manifests["w-0000abcd"].slurm_job_id.as_deref(), Some("7"));
        assert_eq!(browse_state(&c.join("missing")), BrowseState::default());
        let _ = std::fs::remove_dir_all(&c);
    }

    #[test]
    fn job_records_round_trip() {
        let r = JobRecord {
            id: "j-0000abcd".into(),
            name: "Long".into(),
            job_name: "chimaera-long~abc123".into(),
            open: vec!["w-0000abcd".into()],
            replaces: Some("j-1111abcd".into()),
            submitted_ms: 1,
            ..Default::default()
        };
        let back: JobRecord = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        let s: JobHostStatus = serde_json::from_str(
            r#"{"job":"j-0000abcd","slurm_job_id":"7","node":"n1","workspaces":[{"id":"w-0000abcd","state":"open","port":4000}]}"#,
        )
        .unwrap();
        assert_eq!(s.workspaces[0].state, HostedState::Open);
        assert_eq!(s.workspaces[0].working, 0);
    }
}
