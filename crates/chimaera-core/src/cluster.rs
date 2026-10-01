//! A cluster's own chimaera folder: the records the app and the CLI keep on a
//! cluster's shared home (its workspaces, saved setups, startup commands,
//! learned facts, and each workspace's last job), and the job script that runs
//! a workspace's chimaera inside Slurm. Pure types and text — the ssh
//! transport lives in `chimaera-remote`, the daemon side reads the same env
//! names from here.
//!
//! Layout under the cluster folder (owner-only):
//!
//! ```text
//! cluster.json            ClusterConfig
//! w/<id>/data/            the workspace's chimaera data dir (manifest = lease)
//! w/<id>/launch.json      LaunchRecord: the current or last job
//! w/<id>/workspace.json   WorkspaceSeed: what the job's daemon registers
//! w/<id>/startup.sh       startup commands composed for the current job
//! w/<id>/agent-rules.md   rules-for-agents text composed for the current job
//! w/<id>/caps.json        the job's egress probe
//! w/<id>/job.sh, job.log  the script and Slurm's output, last run only
//! ```

use serde::{Deserialize, Serialize};

use crate::slurm::LaunchSpec;

/// The job daemon's data dir (manifest, ledger, journals, timeline) — the
/// workspace's own folder on the shared filesystem, moving from job to job.
pub const ENV_DATA_DIR: &str = "CHIMAERA_DATA_DIR";
/// The job daemon's runtime dir — node-local, gone with the job.
pub const ENV_RUNTIME_DIR: &str = "CHIMAERA_RUNTIME_DIR";
/// Path of the [`WorkspaceSeed`] the job's daemon registers at boot.
pub const ENV_CLUSTER_WORKSPACE: &str = "CHIMAERA_CLUSTER_WORKSPACE";
/// A file of startup commands applied as the outermost prelude scope of every
/// shell and agent the job's daemon spawns.
pub const ENV_HOST_PRELUDE_FILE: &str = "CHIMAERA_HOST_PRELUDE_FILE";
/// Rules-for-agents text the user wrote for this cluster.
pub const ENV_AGENT_RULES_FILE: &str = "CHIMAERA_AGENT_RULES_FILE";
/// A file ON the cluster the user pointed at as its rules for agents (read
/// at bake time, so it's always the cluster's current text).
pub const ENV_AGENT_RULES_SOURCE: &str = "CHIMAERA_AGENT_RULES_SOURCE";

/// `cluster.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterConfig {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub workspaces: Vec<ClusterWorkspace>,
    /// Startup commands every workspace job on this cluster runs first (the
    /// cluster-default scope).
    #[serde(default)]
    pub startup: String,
    /// Named setups the user saved.
    #[serde(default)]
    pub setups: Vec<Setup>,
    /// The last setup started on this cluster — the start sheet's default
    /// for a workspace that has never run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_spec: Option<LaunchSpec>,
    #[serde(default)]
    pub agent_rules: AgentRules,
    #[serde(default)]
    pub learned: Learned,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterWorkspace {
    /// `w-<8 hex>`: names the folder and the job daemon's workspace id.
    pub id: String,
    pub name: String,
    /// Absolute path on the cluster's shared filesystem.
    pub path: String,
    /// Startup commands this workspace adds after the cluster's.
    #[serde(default)]
    pub startup: String,
    /// The setup this workspace last started with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_spec: Option<LaunchSpec>,
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
    /// Text the user wrote or pasted.
    #[serde(default, skip_serializing_if = "String::is_empty")]
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

/// `w/<id>/launch.json` — the workspace's current or last job.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LaunchRecord {
    /// `None` until Slurm handed out an id (an attached start learns it from
    /// the queue by its unique name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub job_name: String,
    pub spec: LaunchSpec,
    /// Held in the foreground by an app's connection (an interactive-only
    /// partition): it ends when that connection does.
    #[serde(default)]
    pub attached: bool,
    pub submitted_ms: u64,
    /// The user stopped it — the stopped line says so instead of a reason.
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

/// `w/<id>/workspace.json` — what the job's daemon registers at boot, under
/// the cluster's id so every job of the workspace shares one identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSeed {
    pub id: String,
    pub name: String,
    pub path: String,
}

/// A fresh workspace id: `w-` + 8 hex.
pub fn new_workspace_id() -> String {
    format!("w-{}", &crate::generate_token()[..8])
}

/// Whether `id` is one this module minted — the only shape allowed into a
/// path or a shell line.
pub fn valid_workspace_id(id: &str) -> bool {
    id.len() == 10 && id.starts_with("w-") && id[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Startup commands for one job, in scope order: the cluster's, the
/// workspace's, then this run's.
pub fn compose_startup(cluster: &str, workspace: &str, run: &str) -> String {
    let mut out = String::new();
    for (label, text) in [
        ("cluster default", cluster),
        ("this workspace", workspace),
        ("this run", run),
    ] {
        let text = text.trim_end();
        if text.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("# --- {label} ---\n{text}\n"));
    }
    out
}

/// Where a job script finds its pieces. Every path is `$HOME`-anchored text
/// expanded by bash on the compute node (the same `$HOME` the login node
/// has).
pub struct JobScript<'a> {
    /// The chimaera binary on the shared filesystem.
    pub binary: &'a str,
    /// The workspace's folder (`…/cluster/w/<id>`).
    pub workspace_dir: &'a str,
    /// A dev build's state home (`$HOME/.chimaera-dev`), so the job's
    /// daemon keeps its config and caches apart from a release's. `None`
    /// for a release.
    pub state_home: Option<&'a str>,
    /// A file on the cluster holding its rules for agents, if the user named
    /// one.
    pub rules_source: Option<&'a str>,
}

/// The script a workspace job runs: point the daemon's data dir at the
/// workspace folder (so its chats and history move from job to job), keep its
/// sockets node-local, probe whether this node reaches the internet (agents
/// need it), and `exec chimaera serve` so the daemon IS the job — walltime
/// and `scancel` stop exactly it, gracefully (SIGTERM first).
///
/// `--bind-routable`: the daemon listens on the node's address, token-gated,
/// so a plain `ssh -L <port>:<node>:<port> <login>` reaches it with nothing
/// extra running on the login node.
///
/// Startup commands are NOT run here: the daemon applies them to every shell
/// and agent it spawns (`ENV_HOST_PRELUDE_FILE`), once per spawn, like every
/// other prelude scope.
pub fn job_script(p: &JobScript) -> String {
    let mut s = String::from("#!/bin/bash -l\n");
    s.push_str(
        "# A chimaera workspace job, written by the chimaera app.\n\
         # Owner-only: this folder is on a shared filesystem, and the daemon's\n\
         # manifest in it carries the token that guards the workspace.\n\
         umask 077\n",
    );
    s.push_str(&format!("W=\"{}\"\n", p.workspace_dir));
    if let Some(home) = p.state_home {
        s.push_str(&format!("export CHIMAERA_HOME=\"{home}\"\n"));
    }
    s.push_str(&format!(
        "export {ENV_DATA_DIR}=\"$W/data\"\n\
         export {ENV_RUNTIME_DIR}=\"/tmp/chimaera-$(id -u)-${{SLURM_JOB_ID:-0}}\"\n\
         export {ENV_CLUSTER_WORKSPACE}=\"$W/workspace.json\"\n\
         export {ENV_HOST_PRELUDE_FILE}=\"$W/startup.sh\"\n\
         export {ENV_AGENT_RULES_FILE}=\"$W/agent-rules.md\"\n"
    ));
    if let Some(src) = p.rules_source {
        s.push_str(&format!(
            "export {ENV_AGENT_RULES_SOURCE}={}\n",
            crate::slurm::sh_quote(src)
        ));
    }
    s.push_str(&format!(
        "unset CHIMAERA_PRELUDE CHIMAERA_PRELUDE_DONE\n\
         mkdir -p \"${ENV_DATA_DIR}\" \"${ENV_RUNTIME_DIR}\"\n\n"
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
         \x20 > \"$W/caps.json\"\n\n",
    );
    s.push_str(&format!("exec \"{}\" serve --bind-routable\n", p.binary));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_and_tolerates_missing_fields() {
        let c: ClusterConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, ClusterConfig::default());
        let c: ClusterConfig = serde_json::from_str(
            r#"{"version":1,"workspaces":[{"id":"w-0000abcd","name":"x","path":"/p"}],"unknown":true}"#,
        )
        .unwrap();
        assert_eq!(c.workspaces[0].startup, "");
        let back: ClusterConfig =
            serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn workspace_ids_are_strict() {
        let id = new_workspace_id();
        assert!(valid_workspace_id(&id), "{id}");
        for bad in [
            "w-1234567",
            "w-12345678x",
            "x-12345678",
            "w-1234567g",
            "w-../../x",
        ] {
            assert!(!valid_workspace_id(bad), "{bad}");
        }
    }

    #[test]
    fn startup_composes_in_scope_order_and_skips_blanks() {
        let s = compose_startup("ml python\n", "  \n", "export DEBUG=1");
        assert_eq!(
            s,
            "# --- cluster default ---\nml python\n# --- this run ---\nexport DEBUG=1\n"
        );
        assert_eq!(compose_startup("", "", ""), "");
    }

    #[test]
    fn job_script_points_the_daemon_at_the_workspace_and_execs_serve() {
        let s = job_script(&JobScript {
            binary: "$HOME/.chimaera/bin/chimaera",
            workspace_dir: "$HOME/.chimaera/cluster/w/w-0000abcd",
            state_home: None,
            rules_source: Some("/etc/some rules.md"),
        });
        assert!(s.starts_with("#!/bin/bash -l\n"));
        assert!(s.contains("umask 077"));
        assert!(s.contains("export CHIMAERA_DATA_DIR=\"$W/data\""));
        assert!(s.contains("export CHIMAERA_AGENT_RULES_SOURCE='/etc/some rules.md'"));
        assert!(
            !s.contains("CHIMAERA_HOME="),
            "a release keeps its normal config"
        );
        assert!(s
            .trim_end()
            .ends_with("exec \"$HOME/.chimaera/bin/chimaera\" serve --bind-routable"));
        let dev = job_script(&JobScript {
            binary: "$HOME/.chimaera-dev/bin/chimaera",
            workspace_dir: "$HOME/.chimaera-dev/data/cluster/w/w-0000abcd",
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
            workspace_dir: "$HOME/w",
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
}
