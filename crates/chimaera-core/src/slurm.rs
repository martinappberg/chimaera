//! The scheduler vocabulary shared by everything that talks to a cluster: the
//! daemon (its own allocation, the queue strip) and the clients that drive a
//! cluster over ssh (the app and the CLI). Parsers for the format-string
//! output of `squeue`/`sinfo`/`sacctmgr`/`scontrol`, Slurm's time grammar, a
//! launch spec with its validation, and the job argv. Pure — no processes, no
//! I/O — so every rule is unit-tested here and the transports stay thin.
//!
//! Site-agnostic by rule: nothing here knows a partition name, a site's helper
//! command, a site path, or a hostname. What a cluster is like comes from
//! standard Slurm output or from the cluster's own refusal text.
//!
//! Format strings, not `--json`: the format flags are stable across the old
//! Slurm versions real clusters run, and the output is bounded by
//! construction.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Which batch scheduler a host's login shell can reach. Only Slurm can be
/// driven today; the others still make a host a cluster (nothing of ours runs
/// on its login node) without the launcher.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheduler {
    #[default]
    None,
    Slurm,
    Pbs,
    Lsf,
}

impl Scheduler {
    /// Whether the host is a cluster: some scheduler's client tools are on
    /// its login PATH.
    pub fn is_cluster(self) -> bool {
        self != Scheduler::None
    }

    /// The wire tag (`"slurm"`, …) — also what the detection script prints.
    pub fn tag(self) -> &'static str {
        match self {
            Scheduler::None => "none",
            Scheduler::Slurm => "slurm",
            Scheduler::Pbs => "pbs",
            Scheduler::Lsf => "lsf",
        }
    }

    pub fn from_tag(tag: &str) -> Scheduler {
        match tag.trim() {
            "slurm" => Scheduler::Slurm,
            "pbs" => Scheduler::Pbs,
            "lsf" => Scheduler::Lsf,
            _ => Scheduler::None,
        }
    }
}

/// Row caps (a strip and a page, not a dashboard); `truncated` says so.
pub const MAX_JOBS: usize = 50;
pub const MAX_PARTITIONS: usize = 50;

/// `squeue -u <user> --noheader -o SQUEUE_FORMAT`. `%j` goes LAST: job names
/// are the one user-controlled field and may contain the `|` delimiter, which
/// `splitn` can only fold into the final field.
pub const SQUEUE_FORMAT: &str = "%i|%P|%T|%L|%N|%C|%m|%M|%Z|%r|%j";

/// `sinfo --noheader -o SINFO_FORMAT`.
pub const SINFO_FORMAT: &str = "%P|%a|%D|%l|%c|%m|%G";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub partition: String,
    pub state: String,
    pub time_left: String,
    pub nodes: String,
    /// Allocated (or requested, while pending) CPUs and minimum memory —
    /// squeue's own resource truth. "" when the row lacked them.
    pub cpus: String,
    pub mem: String,
    /// Elapsed run time (`%M`) and working directory (`%Z`). Omitted on the
    /// wire when squeue gave nothing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub elapsed: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workdir: String,
    /// Why a pending job waits (`%r`: `Priority`, `Resources`, a QOS limit…).
    /// "" or `None` when there is nothing to say.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// Terminal squeue states — a job seen in one of these has ended.
pub const TERMINAL_STATES: [&str; 9] = [
    "COMPLETED",
    "FAILED",
    "CANCELLED",
    "TIMEOUT",
    "OUT_OF_MEMORY",
    "NODE_FAIL",
    "PREEMPTED",
    "BOOT_FAIL",
    "DEADLINE",
];

/// The terminal state a squeue/sacct state string names, if any
/// (`CANCELLED by 123` is CANCELLED).
pub fn terminal_state(state: &str) -> Option<&'static str> {
    TERMINAL_STATES
        .iter()
        .find(|t| state.trim().starts_with(**t))
        .copied()
}

/// Still queued or running (not yet ended).
pub fn is_live_state(state: &str) -> bool {
    terminal_state(state).is_none()
}

/// `squeue … -o SQUEUE_FORMAT` → jobs. Unparseable lines are skipped, never
/// fatal (Slurm banners and warnings sometimes precede output). The mid-row
/// fields are tolerated missing — older rows degrade to "".
pub fn parse_squeue(out: &str) -> (Vec<Job>, bool) {
    let mut jobs = Vec::new();
    let mut truncated = false;
    for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut f = line.splitn(11, '|').map(str::trim);
        let (Some(id), Some(partition), Some(state), Some(time_left)) =
            (f.next(), f.next(), f.next(), f.next())
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
        // %N is empty while pending — an empty string is the honest value.
        let nodes = f.next().unwrap_or("").to_string();
        let cpus = f.next().unwrap_or("").to_string();
        let mem = f.next().unwrap_or("").to_string();
        let elapsed = f.next().unwrap_or("").to_string();
        // %Z prints "(null)"/"n/a" on some builds when unknown.
        let workdir = f
            .next()
            .filter(|w| w.starts_with('/'))
            .unwrap_or("")
            .to_string();
        let reason = f
            .next()
            .filter(|r| !matches!(*r, "None" | "(null)" | "n/a"))
            .unwrap_or("")
            .to_string();
        jobs.push(Job {
            id: id.to_string(),
            name: f.next().unwrap_or("").to_string(),
            partition: partition.to_string(),
            state: state.to_string(),
            time_left: time_left.to_string(),
            nodes,
            cpus,
            mem,
            elapsed,
            workdir,
            reason,
        });
    }
    (jobs, truncated)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Partition {
    pub name: String,
    /// Per-partition ceilings straight from sinfo (`%l` walltime, `%c`
    /// cpus/node, `%m` MB/node — "+" suffixes mean "varies upward"). Raw
    /// strings, "" when the row lacked them.
    pub time_limit: String,
    pub cpus_per_node: String,
    pub mem_per_node: String,
    pub default: bool,
    pub avail: bool,
    pub nodes: u64,
    /// Some node in the partition advertises a GPU gres (`%G`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gpus: bool,
}

/// `sinfo --noheader -o SINFO_FORMAT` → partitions. sinfo groups rows by the
/// non-numeric fields, so a partition may span several rows (one per avail
/// state or gres shape): they merge — nodes summed, "up" wins, first non-empty
/// limits stick, any GPU row marks the partition. The default partition
/// carries a `*` suffix on its name.
pub fn parse_sinfo(out: &str) -> (Vec<Partition>, bool) {
    let mut partitions: Vec<Partition> = Vec::new();
    let mut truncated = false;
    for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut f = line.splitn(7, '|').map(str::trim);
        let (Some(raw_name), Some(avail)) = (f.next(), f.next()) else {
            continue;
        };
        if raw_name.is_empty() || raw_name.contains(char::is_whitespace) {
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
        let gpus = f
            .next()
            .is_some_and(|g| g.to_ascii_lowercase().contains("gpu"));
        if let Some(existing) = partitions.iter_mut().find(|p| p.name == name) {
            existing.nodes += nodes;
            existing.avail |= avail == "up";
            existing.default |= default;
            existing.gpus |= gpus;
            if existing.time_limit.is_empty() {
                existing.time_limit = time_limit;
            }
            if existing.cpus_per_node.is_empty() {
                existing.cpus_per_node = cpus_per_node;
            }
            if existing.mem_per_node.is_empty() {
                existing.mem_per_node = mem_per_node;
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
            gpus,
        });
    }
    (partitions, truncated)
}

/// Parse Slurm's duration grammar (`squeue %L`, `sinfo %l`, `--time`):
/// `days-hours:minutes:seconds` with zero leading components omitted, so
/// `9:54` is min:sec and `8:00:00` is h:min:sec. `UNLIMITED`, `NOT_SET`,
/// `INVALID`, and bare numbers (ambiguous) are `None`.
pub fn parse_duration(s: &str) -> Option<Duration> {
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

/// A duration as Slurm's `--time` wants it: `D-HH:MM:SS`, or `HH:MM:SS`
/// under a day. Seconds round down to the minute — walltimes are minutes.
pub fn format_duration(d: Duration) -> String {
    let mins = d.as_secs() / 60;
    let (days, h, m) = (mins / 1440, (mins % 1440) / 60, mins % 60);
    if days > 0 {
        format!("{days}-{h:02}:{m:02}:00")
    } else {
        format!("{h:02}:{m:02}:00")
    }
}

/// `SystemTime` → `"YYYY-MM-DD HH:MM UTC"`, minute precision (walltime ends
/// are estimates; seconds would be false precision). Hand-rolled civil-date
/// conversion (Howard Hinnant's `civil_from_days`) because the workspace
/// deliberately carries no date-time dependency. `None` only for pre-epoch
/// input.
pub fn format_utc_minute(t: SystemTime) -> Option<String> {
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

/// Slurm's stderr text, made presentable: `<tool>: error: ` prefixes
/// stripped, ASCII ruler lines dropped, whitespace collapsed, length capped.
/// What remains is the admin-authored message — the most cluster-specific,
/// user-actionable text we will ever have, so it is shown verbatim.
pub fn clean_tool_stderr(raw: &str, tool: &str) -> String {
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

// --- What the user may run ----------------------------------------------------

/// One `sacctmgr -nP show assoc user=<u> format=account,partition,qos` row:
/// an account the user belongs to, the partition it is limited to (`""` =
/// any), and the QOS it may use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Association {
    pub account: String,
    pub partition: String,
    pub qos: Vec<String>,
}

/// `sacctmgr -nP … format=account,partition,qos` → associations. Rows that
/// don't parse are skipped (sacctmgr prints nothing at all on clusters that
/// keep no accounting — then there are simply no associations).
pub fn parse_associations(out: &str) -> Vec<Association> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let mut f = l.split('|').map(str::trim);
            let account = f.next().filter(|a| valid_name(a))?;
            let partition = f.next().unwrap_or("");
            let qos = f
                .next()
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|q| valid_name(q))
                .map(str::to_string)
                .collect();
            Some(Association {
                account: account.to_string(),
                partition: if valid_name(partition) {
                    partition.to_string()
                } else {
                    String::new()
                },
                qos,
            })
        })
        .collect()
}

/// A partition's access and policy lines from
/// `scontrol show partition --oneliner`. `None` lists mean "not restricted".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartitionPolicy {
    pub name: String,
    pub allow_accounts: Option<Vec<String>>,
    pub deny_accounts: Vec<String>,
    pub allow_groups: Option<Vec<String>>,
    pub allow_qos: Option<Vec<String>>,
    /// The partition's own `PreemptMode` says jobs in it may be stopped early
    /// for higher-priority work.
    pub preemptible: bool,
    /// `MaxTime` as Slurm printed it ("" when absent).
    pub max_time: String,
}

/// `scontrol show partition --oneliner` → one policy per partition. Each
/// line is space-separated `Key=Value` pairs; unknown keys are ignored.
pub fn parse_partition_policies(out: &str) -> Vec<PartitionPolicy> {
    let list = |v: &str| -> Option<Vec<String>> {
        if v.is_empty() || v.eq_ignore_ascii_case("ALL") {
            None
        } else {
            Some(
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
            )
        }
    };
    let mut out_policies = Vec::new();
    for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut p = PartitionPolicy::default();
        for pair in line.split_whitespace() {
            let Some((k, v)) = pair.split_once('=') else {
                continue;
            };
            match k {
                "PartitionName" => p.name = v.to_string(),
                "AllowAccounts" => p.allow_accounts = list(v),
                "DenyAccounts" => p.deny_accounts = list(v).unwrap_or_default(),
                "AllowGroups" => p.allow_groups = list(v),
                "AllowQos" => p.allow_qos = list(v),
                "MaxTime" => p.max_time = v.to_string(),
                // OFF / CANCEL-free modes keep a job running; REQUEUE,
                // CANCEL and SUSPEND all take a running job away.
                "PreemptMode" => {
                    let v = v.to_ascii_uppercase();
                    p.preemptible = ["REQUEUE", "CANCEL", "SUSPEND"]
                        .iter()
                        .any(|m| v.contains(m));
                }
                _ => {}
            }
        }
        if valid_name(&p.name) {
            out_policies.push(p);
        }
    }
    out_policies
}

/// A partition the user can pick, with everything the start sheet shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartitionChoice {
    pub name: String,
    pub default: bool,
    /// `sinfo`'s time limit, raw (`7-00:00:00`, `UNLIMITED`) and in seconds
    /// (`None` = unlimited or unknown).
    pub max_time: String,
    pub max_time_secs: Option<u64>,
    pub cpus_per_node: String,
    pub mem_per_node: String,
    pub gpus: bool,
    pub preemptible: bool,
    pub up: bool,
    /// Accounts that may submit here (empty when the cluster uses none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<String>,
}

/// The partitions this user can submit to: sinfo's list, filtered by the
/// partitions' `Allow*`/`Deny*` lists against the user's groups and
/// accounting associations. Clusters that keep no associations (or hide
/// `scontrol`) degrade to "everything sinfo lists"; a refusal at submit time
/// then teaches the rest.
pub fn usable_partitions(
    sinfo: &[Partition],
    policies: &[PartitionPolicy],
    associations: &[Association],
    groups: &[String],
) -> Vec<PartitionChoice> {
    sinfo
        .iter()
        .filter_map(|p| {
            let policy = policies.iter().find(|x| x.name == p.name);
            if let Some(allowed) = policy.and_then(|x| x.allow_groups.as_ref()) {
                if !allowed.iter().any(|g| groups.contains(g)) {
                    return None;
                }
            }
            let mut accounts: Vec<String> = Vec::new();
            if !associations.is_empty() {
                for a in associations {
                    if !a.partition.is_empty() && a.partition != p.name {
                        continue;
                    }
                    if let Some(policy) = policy {
                        if policy.deny_accounts.contains(&a.account) {
                            continue;
                        }
                        if let Some(allow) = &policy.allow_accounts {
                            if !allow.contains(&a.account) {
                                continue;
                            }
                        }
                    }
                    if !accounts.contains(&a.account) {
                        accounts.push(a.account.clone());
                    }
                }
                if accounts.is_empty() {
                    return None;
                }
            }
            let max_time = if p.time_limit.is_empty() {
                policy.map(|x| x.max_time.clone()).unwrap_or_default()
            } else {
                p.time_limit.clone()
            };
            Some(PartitionChoice {
                name: p.name.clone(),
                default: p.default,
                max_time_secs: parse_duration(&max_time).map(|d| d.as_secs()),
                max_time,
                cpus_per_node: p.cpus_per_node.clone(),
                mem_per_node: p.mem_per_node.clone(),
                gpus: p.gpus,
                preemptible: policy.is_some_and(|x| x.preemptible),
                up: p.avail,
                accounts,
            })
        })
        .collect()
}

/// What a scheduler's refusal says is wrong with a submission, read loosely
/// from its own words (which are always shown verbatim as well). Used to
/// remember per-cluster facts — "this partition only takes interactive
/// jobs", "every job names an account" — so the next start doesn't fail the
/// same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    BatchNotAllowed,
    AccountRequired,
    QosRequired,
    ConstraintRequired,
    Other,
}

pub fn classify_refusal(message: &str) -> Refusal {
    let m = message.to_ascii_lowercase();
    let says = |words: &[&str]| words.iter().any(|w| m.contains(w));
    if m.contains("batch") && says(&["not allowed", "not permitted", "interactive"]) {
        Refusal::BatchNotAllowed
    } else if m.contains("account")
        && says(&["must specify", "required", "no default", "invalid account"])
    {
        Refusal::AccountRequired
    } else if m.contains("qos") && says(&["must specify", "required", "invalid qos"]) {
        Refusal::QosRequired
    } else if says(&["constraint", "feature"]) && says(&["must specify", "required", "invalid"]) {
        Refusal::ConstraintRequired
    } else {
        Refusal::Other
    }
}

// --- Launching ----------------------------------------------------------------

/// What a workspace job asks Slurm for. Every field but `time` is optional:
/// absent means the cluster's own default (no flag at all).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    /// Walltime in Slurm's grammar (`2-00:00:00`, `04:00:00`). Required:
    /// every job states its limit.
    pub time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qos: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    /// Memory per node, Slurm's spelling (`16G`, `4000M`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpus: Option<u32>,
}

/// A Slurm object name (partition, account, QOS): what Slurm itself accepts,
/// minus anything a shell or argv could misread.
fn valid_name(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && !v.starts_with('-')
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

impl LaunchSpec {
    /// Empty optional fields become `None` (the form sends `""` for "cluster
    /// default").
    pub fn normalized(mut self) -> Self {
        let blank = |v: &mut Option<String>| {
            if v.as_deref().is_some_and(|s| s.trim().is_empty()) {
                *v = None;
            } else if let Some(s) = v {
                *s = s.trim().to_string();
            }
        };
        blank(&mut self.partition);
        blank(&mut self.account);
        blank(&mut self.qos);
        blank(&mut self.constraint);
        blank(&mut self.mem);
        self.time = self.time.trim().to_string();
        self.cpus = self.cpus.filter(|c| *c > 0);
        self.gpus = self.gpus.filter(|g| *g > 0);
        self
    }

    /// Reject anything Slurm can't take or a shell could misread, in the
    /// words a form can show next to the field.
    pub fn validate(&self) -> Result<(), String> {
        if parse_duration(&self.time).is_none_or(|d| d.as_secs() < 60) {
            return Err("Enter a time limit of at least a minute".into());
        }
        for (field, v) in [
            ("partition", &self.partition),
            ("account", &self.account),
            ("QOS", &self.qos),
        ] {
            if let Some(v) = v {
                if !valid_name(v) {
                    return Err(format!("That {field} name isn't valid"));
                }
            }
        }
        if let Some(c) = &self.constraint {
            let ok = !c.is_empty()
                && c.len() <= 128
                && c.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "_-.&|,:[]*()".contains(ch));
            if !ok {
                return Err("That constraint isn't valid".into());
            }
        }
        if let Some(m) = &self.mem {
            let digits = m.trim_end_matches(['K', 'M', 'G', 'T', 'k', 'm', 'g', 't']);
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return Err("Write memory like 16G or 4000M".into());
            }
        }
        if self.cpus.is_some_and(|c| c > 4096) {
            return Err("That's more CPUs than any node has".into());
        }
        if self.gpus.is_some_and(|g| g > 64) {
            return Err("That's more GPUs than any node has".into());
        }
        Ok(())
    }

    /// The resource flags shared by `sbatch` and `srun`. Values are already
    /// validated; each flag is one argv element.
    fn resource_args(&self, gpus_flag: GpuFlag) -> Vec<String> {
        let mut a = vec![format!("--time={}", self.time)];
        if let Some(p) = &self.partition {
            a.push(format!("--partition={p}"));
        }
        if let Some(acct) = &self.account {
            a.push(format!("--account={acct}"));
        }
        if let Some(q) = &self.qos {
            a.push(format!("--qos={q}"));
        }
        if let Some(c) = &self.constraint {
            a.push(format!("--constraint={c}"));
        }
        if let Some(c) = self.cpus {
            a.push(format!("--cpus-per-task={c}"));
        }
        if let Some(m) = &self.mem {
            a.push(format!("--mem={m}"));
        }
        if let Some(g) = self.gpus {
            a.push(match gpus_flag {
                GpuFlag::Gpus => format!("--gpus={g}"),
                GpuFlag::Gres => format!("--gres=gpu:{g}"),
            });
        }
        a
    }

    /// `sbatch` argv after the program name. `--parsable` makes sbatch print
    /// just the job id; `--no-requeue` keeps a preempted or failed job from
    /// being started again by the scheduler with nobody there.
    pub fn sbatch_args(&self, job_name: &str, output: &str, gpus: GpuFlag) -> Vec<String> {
        let mut a = vec![
            "--parsable".to_string(),
            format!("--job-name={job_name}"),
            "--no-requeue".to_string(),
            format!("--output={output}"),
        ];
        a.extend(self.resource_args(gpus));
        a
    }

    /// `srun` argv (before the command) for a job held in the foreground by
    /// the caller's own connection — partitions that refuse batch jobs.
    pub fn srun_args(&self, job_name: &str, gpus: GpuFlag) -> Vec<String> {
        let mut a = vec![format!("--job-name={job_name}")];
        a.extend(self.resource_args(gpus));
        a
    }
}

/// How this cluster's Slurm spells a GPU request: `--gpus` (19.05+) or the
/// older `--gres=gpu:N`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuFlag {
    #[default]
    Gpus,
    Gres,
}

/// `sinfo --version` (`slurm 23.02.7`) → the GPU flag that version takes.
pub fn gpu_flag_for(version_line: &str) -> GpuFlag {
    let v = version_line.split_whitespace().last().unwrap_or("");
    let mut it = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
    if major == 0 || (major, minor) >= (19, 5) {
        GpuFlag::Gpus
    } else {
        GpuFlag::Gres
    }
}

/// The job name every chimaera workspace job wears: `chimaera-<slug>~<tok>`.
/// The prefix is how the queue shows our jobs apart from the user's own; the
/// token makes each launch's name unique so an attached (foreground) job can
/// be found by exact name.
pub fn job_name(workspace_name: &str, token: &str) -> String {
    format!("chimaera-{}~{token}", slug(workspace_name))
}

/// Whether a queue row's name is one of ours.
pub fn is_chimaera_job(name: &str) -> bool {
    name.starts_with("chimaera-")
}

/// Lowercase alnum/dash, bounded.
pub fn slug(name: &str) -> String {
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
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let mut s = s.trim_matches('-').to_string();
    if s.is_empty() {
        s = "workspace".to_string();
    }
    s.truncate(32);
    s.trim_end_matches('-').to_string()
}

/// One value single-quoted for a POSIX shell line.
pub fn sh_quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_squeue_rows_caps_and_skips_noise() {
        let out =
            "34022541|batch|RUNNING|9:54|n058|4|16G|1:02:03|/home/u/proj|None|chimaera-test\n\
                   34022542|batch|PENDING|8:00:00||||0:00|(null)|Priority|align.sh\n\
                   34022543|batch|RUNNING|1:00|n1|2|4G|5:00|/scratch/x|None|my|weird|name\n\
                   slurm_load_jobs: Warning: something\n";
        let (jobs, truncated) = parse_squeue(out);
        assert!(!truncated);
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[0].id, "34022541");
        assert_eq!(jobs[0].name, "chimaera-test");
        assert_eq!(jobs[0].nodes, "n058");
        assert_eq!(jobs[0].cpus, "4");
        assert_eq!(jobs[0].mem, "16G");
        assert_eq!(jobs[0].elapsed, "1:02:03");
        assert_eq!(jobs[0].workdir, "/home/u/proj");
        assert_eq!(jobs[0].reason, "", "None is no reason");
        assert_eq!(jobs[1].workdir, "", "(null) is not a path");
        assert_eq!(jobs[1].nodes, "", "a pending job has no nodes yet");
        assert_eq!(jobs[1].reason, "Priority");
        assert_eq!(jobs[1].name, "align.sh");
        assert_eq!(
            jobs[2].name, "my|weird|name",
            "embedded delimiters fold into the trailing name, never shift fields"
        );

        let many: String = (0..60)
            .map(|i| format!("{i}|p|RUNNING|1:00|n{i}|1|1G|0:10|/w|None|j{i}\n"))
            .collect();
        let (jobs, truncated) = parse_squeue(&many);
        assert_eq!(jobs.len(), MAX_JOBS);
        assert!(truncated);
    }

    #[test]
    fn parse_sinfo_default_star_dedupe_limits_and_gpus() {
        let out = "batch*|up|10|7-00:00:00|20+|128000+|(null)\n\
                   batch*|down|2|7-00:00:00|20+|128000+|(null)\n\
                   gpu|up|4|2-00:00:00|32|256000|gpu:4(S:0-1)\n\
                   gpu|up|2|2-00:00:00|32|256000|(null)\n\
                   interactive|up|3|2:00:00|24|192000|(null)\n";
        let (parts, truncated) = parse_sinfo(out);
        assert!(!truncated);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].name, "batch");
        assert!(parts[0].default);
        assert!(parts[0].avail, "up wins");
        assert_eq!(parts[0].nodes, 12);
        assert_eq!(parts[0].time_limit, "7-00:00:00");
        assert!(!parts[0].gpus);
        assert!(parts[1].gpus, "any gpu row marks the partition");
        assert_eq!(parts[1].nodes, 6);
        assert_eq!(parts[2].time_limit, "2:00:00");
    }

    #[test]
    fn clean_tool_stderr_keeps_the_admin_message() {
        let raw = "sbatch: error: ================================================\n\
                   sbatch: error:  ERROR: batch job not allowed\n\
                   sbatch: error: Batch jobs are not allowed in this partition, which is\n\
                   sbatch: error: reserved for interactive sessions.\n\
                   sbatch: error: ------------------------------------------------\n\
                   sbatch: error: Batch job submission failed: Invalid partition name specified\n";
        let msg = clean_tool_stderr(raw, "sbatch");
        assert!(msg.starts_with("ERROR: batch job not allowed"));
        assert!(!msg.contains("sbatch:"), "tool prefixes stripped");
        assert!(!msg.contains("====="), "rulers dropped");
        assert_eq!(classify_refusal(&msg), Refusal::BatchNotAllowed);
        assert_eq!(
            clean_tool_stderr("", "sbatch"),
            "the command failed without a message"
        );
    }

    #[test]
    fn refusals_are_read_loosely() {
        assert_eq!(
            classify_refusal("Batch job submission failed: Invalid account or account/partition combination specified"),
            Refusal::AccountRequired
        );
        assert_eq!(
            classify_refusal("You must specify an account for your job"),
            Refusal::AccountRequired
        );
        assert_eq!(
            classify_refusal("Batch job submission failed: Invalid qos specification"),
            Refusal::QosRequired
        );
        assert_eq!(
            classify_refusal("Job submit/allocate failed: Invalid feature specification; a constraint is required"),
            Refusal::ConstraintRequired
        );
        assert_eq!(
            classify_refusal("Requested time limit is invalid (missing or exceeds some limit)"),
            Refusal::Other
        );
    }

    #[test]
    fn durations_parse_and_format() {
        assert_eq!(parse_duration("9:54"), Some(Duration::from_secs(594)));
        assert_eq!(parse_duration("8:00:00"), Some(Duration::from_secs(28_800)));
        assert_eq!(
            parse_duration("7-00:00:00"),
            Some(Duration::from_secs(604_800))
        );
        assert_eq!(
            parse_duration("1-12:30:05"),
            Some(Duration::from_secs(131_405))
        );
        assert_eq!(parse_duration("2-12"), Some(Duration::from_secs(216_000)));
        for junk in ["UNLIMITED", "NOT_SET", "INVALID", "", "42", "a:b"] {
            assert_eq!(parse_duration(junk), None, "{junk}");
        }
        assert_eq!(format_duration(Duration::from_secs(7_200)), "02:00:00");
        assert_eq!(format_duration(Duration::from_secs(604_800)), "7-00:00:00");
        assert_eq!(format_duration(Duration::from_secs(90_061)), "1-01:01:00");
        assert_eq!(
            parse_duration(&format_duration(Duration::from_secs(183_600))),
            Some(Duration::from_secs(183_600))
        );
    }

    #[test]
    fn format_utc_minute_matches_date_u() {
        let at = |s: u64| UNIX_EPOCH + Duration::from_secs(s);
        assert_eq!(
            format_utc_minute(at(0)).as_deref(),
            Some("1970-01-01 00:00 UTC")
        );
        assert_eq!(
            format_utc_minute(at(1_784_118_840)).as_deref(),
            Some("2026-07-15 12:34 UTC")
        );
        assert_eq!(
            format_utc_minute(at(1_709_251_140)).as_deref(),
            Some("2024-02-29 23:59 UTC")
        );
    }

    #[test]
    fn partitions_are_filtered_by_groups_and_accounts() {
        let (sinfo, _) = parse_sinfo(
            "batch*|up|10|14-00:00:00|20|128000|(null)\n\
             labA|up|4|120-00:00:00|32|256000|(null)\n\
             labB|up|4|120-00:00:00|32|256000|(null)\n\
             grp|up|2|1-00:00:00|8|64000|(null)\n\
             gpu|up|2|2-00:00:00|32|256000|gpu:4\n",
        );
        let policies = parse_partition_policies(
            "PartitionName=batch AllowGroups=ALL AllowAccounts=ALL Default=YES MaxTime=14-00:00:00 PreemptMode=OFF\n\
             PartitionName=labA AllowGroups=ALL AllowAccounts=lab_a MaxTime=120-00:00:00\n\
             PartitionName=labB AllowGroups=ALL AllowAccounts=lab_b MaxTime=120-00:00:00\n\
             PartitionName=grp AllowGroups=special AllowAccounts=ALL\n\
             PartitionName=gpu AllowGroups=ALL AllowAccounts=ALL PreemptMode=REQUEUE\n",
        );
        let assoc = parse_associations(
            "default|batch|normal\nlab_a||normal,long\njunk line without pipes\n",
        );
        assert_eq!(assoc.len(), 2, "the junk line is skipped");
        assert_eq!(assoc[1].qos, vec!["normal", "long"]);
        let groups = vec!["users".to_string()];
        let usable = usable_partitions(&sinfo, &policies, &assoc, &groups);
        let names: Vec<&str> = usable.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["batch", "labA", "gpu"]);
        assert_eq!(usable[0].accounts, vec!["default", "lab_a"]);
        assert_eq!(usable[1].accounts, vec!["lab_a"]);
        assert!(usable[2].preemptible);
        assert!(usable[2].gpus);
        assert_eq!(usable[0].max_time_secs, Some(14 * 86_400));

        // No accounting at all: groups alone decide, and no account field.
        let open = usable_partitions(&sinfo, &policies, &[], &groups);
        assert_eq!(open.len(), 4, "everything but the group-restricted one");
        assert!(open.iter().all(|p| p.accounts.is_empty()));
    }

    #[test]
    fn launch_spec_validates_and_builds_argv() {
        let spec = LaunchSpec {
            time: " 2-00:00:00 ".into(),
            partition: Some("batch".into()),
            account: Some("".into()),
            mem: Some("16G".into()),
            cpus: Some(4),
            gpus: Some(0),
            ..Default::default()
        }
        .normalized();
        assert_eq!(spec.account, None, "blank fields mean the cluster default");
        assert_eq!(spec.gpus, None);
        spec.validate().unwrap();
        let a = spec.sbatch_args("chimaera-x~ab12", "$HOME/out.log", GpuFlag::Gpus);
        assert_eq!(
            a,
            vec![
                "--parsable",
                "--job-name=chimaera-x~ab12",
                "--no-requeue",
                "--output=$HOME/out.log",
                "--time=2-00:00:00",
                "--partition=batch",
                "--cpus-per-task=4",
                "--mem=16G",
            ]
        );
        let gpu = LaunchSpec {
            time: "1:00:00".into(),
            gpus: Some(2),
            ..Default::default()
        };
        assert!(gpu
            .srun_args("n", GpuFlag::Gres)
            .contains(&"--gres=gpu:2".to_string()));
        assert!(gpu
            .srun_args("n", GpuFlag::Gpus)
            .contains(&"--gpus=2".to_string()));

        for bad in [
            LaunchSpec {
                time: "".into(),
                ..Default::default()
            },
            LaunchSpec {
                time: "0:30".into(),
                ..Default::default()
            },
            LaunchSpec {
                time: "1:00:00".into(),
                partition: Some("a b".into()),
                ..Default::default()
            },
            LaunchSpec {
                time: "1:00:00".into(),
                account: Some("-x".into()),
                ..Default::default()
            },
            LaunchSpec {
                time: "1:00:00".into(),
                mem: Some("lots".into()),
                ..Default::default()
            },
            LaunchSpec {
                time: "1:00:00".into(),
                constraint: Some("a;rm".into()),
                ..Default::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn gpu_flag_follows_the_slurm_version() {
        assert_eq!(gpu_flag_for("slurm 24.05.4"), GpuFlag::Gpus);
        assert_eq!(gpu_flag_for("slurm 19.05.0"), GpuFlag::Gpus);
        assert_eq!(gpu_flag_for("slurm 18.08.9"), GpuFlag::Gres);
        assert_eq!(gpu_flag_for(""), GpuFlag::Gpus, "unknown assumes modern");
    }

    #[test]
    fn job_names_are_ours_and_unique() {
        assert_eq!(
            job_name("CRC joint/fold!", "ab12"),
            "chimaera-crc-joint-fold~ab12"
        );
        assert_eq!(job_name("", "z"), "chimaera-workspace~z");
        assert!(is_chimaera_job("chimaera-x~1"));
        assert!(!is_chimaera_job("align.sh"));
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn scheduler_tags_round_trip() {
        for s in [
            Scheduler::None,
            Scheduler::Slurm,
            Scheduler::Pbs,
            Scheduler::Lsf,
        ] {
            assert_eq!(Scheduler::from_tag(s.tag()), s);
        }
        assert!(!Scheduler::None.is_cluster());
        assert!(Scheduler::Pbs.is_cluster());
    }
}
