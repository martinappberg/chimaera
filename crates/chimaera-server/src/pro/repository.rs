//! Portable Git state is an allowlist, never a copy of a host's executable
//! configuration. Ref updates use compare-and-swap and never force local work.
use super::{mirror, policy, protocol::MirrorCredentials, transport};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_REFS: usize = 4096;
const MAX_CONFIG: usize = 256;
const SOURCE_HEAD: &str = "refs/chimaera/source-head";
// A transaction keeps one of the two transport child slots while read-tree
// needs the other. Reserve transaction ownership before taking its child slot.
static REF_TRANSACTIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
pub(super) mod staging;
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Snapshot {
    pub head: Option<String>,
    pub config: Vec<Entry>,
    /// Absent on older snapshots: unknown staging, never an empty index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging: Option<staging::Descriptor>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Entry {
    key: String,
    value: String,
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.contains("..")
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-/@".contains(&c))
}
pub(super) fn reference(value: &str) -> bool {
    value.starts_with("refs/")
        && value.len() <= 1024
        && !value.chars().any(char::is_control)
        && !value.contains([' ', '~', '^', ':', '?', '*', '[', '\\'])
        && !value.contains("..")
        && !value.contains("@{")
        && value.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with('.')
                && !part.ends_with(".lock")
        })
}
pub(super) fn safe_url(value: &str) -> bool {
    if value.len() > 2048
        || value.chars().any(char::is_control)
        || policy::contains_credential(value.as_bytes())
        || value.contains(['?', '#'])
    {
        return false;
    }
    if let Ok(uri) = value.parse::<axum::http::Uri>() {
        if uri.scheme_str() == Some("https")
            && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
        {
            return true;
        }
        if uri.scheme_str() == Some("ssh") {
            return uri.authority().is_some_and(|a| {
                let (user, host) = a.as_str().split_once('@').unwrap_or(("git", a.as_str()));
                name(user)
                    && !user.contains('/')
                    && !host.is_empty()
                    && host
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b".:-[]".contains(&c))
            });
        }
    }
    value.split_once('@').is_some_and(|(user, rest)| {
        name(user)
            && !user.contains('/')
            && rest.split_once(':').is_some_and(|(host, path)| {
                !host.is_empty()
                    && host
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
                    && name(path)
            })
    })
}
fn allowed(entry: &Entry) -> bool {
    let key = &entry.key;
    let value = &entry.value;
    if key.len() > 512
        || value.len() > 2048
        || value.chars().any(char::is_control)
        || policy::contains_credential(value.as_bytes())
    {
        return false;
    }
    if let Some((remote, field)) = key
        .strip_prefix("remote.")
        .and_then(|rest| rest.rsplit_once('.'))
    {
        if !name(remote) || remote.contains('/') {
            return false;
        }
        return match field {
            "url" | "pushurl" => safe_url(value),
            "fetch" => value.starts_with("+refs/") || value.starts_with("refs/"),
            _ => false,
        } && (field != "fetch"
            || (value.len() <= 1024
                && !value.contains("..")
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"/+*:._-".contains(&c))));
    }
    if let Some((branch, field)) = key
        .strip_prefix("branch.")
        .and_then(|rest| rest.rsplit_once('.'))
    {
        if !reference(&format!("refs/heads/{branch}")) {
            return false;
        }
        return match field {
            "remote" | "pushremote" => name(value) && !value.contains('/'),
            "merge" => value.starts_with("refs/heads/") && reference(value),
            "rebase" => matches!(value.as_str(), "true" | "false" | "merges" | "interactive"),
            _ => false,
        };
    }
    match key.as_str() {
        "core.autocrlf" => matches!(value.as_str(), "true" | "false" | "input"),
        "core.eol" => matches!(value.as_str(), "lf" | "crlf" | "native"),
        "remote.pushdefault" => name(value) && !value.contains('/'),
        "push.default" => matches!(
            value.as_str(),
            "nothing" | "current" | "upstream" | "simple" | "matching"
        ),
        _ => false,
    }
}
tokio::task_local! {static STAGED_CHECKOUT: PathBuf;}
async fn project_git(root: &Path) -> Result<tokio::process::Command> {
    let mut command = transport::git(root, None).await?;
    if STAGED_CHECKOUT
        .try_with(|checkout| checkout == root)
        .unwrap_or(false)
    {
        // A fresh staged checkout has no repository yet. Git requires an
        // explicit Git directory whenever GIT_WORK_TREE is supplied, including
        // init; binding both also prevents discovery of an enclosing repo.
        command
            .env("GIT_WORK_TREE", root)
            .env("GIT_DIR", root.join(".git"));
    }
    Ok(command)
}

async fn optional(root: &Path, args: &[&str]) -> Result<Option<String>> {
    let mut command = project_git(root).await?;
    command.args(args);
    let output = transport::run(
        command,
        vec![],
        Duration::from_secs(15),
        transport::JSON_CAP,
    )
    .await?;
    Ok(output
        .success
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string()))
}
/// What a snapshot records of the project's own Git repository.
#[derive(Default)]
pub(super) struct Described {
    /// The checked-out branch (`refs/heads/…`); none on a detached HEAD.
    pub branch: Option<String>,
    /// A safe `origin` URL (`mirror::repository_origin`).
    pub origin: Option<String>,
    /// HEAD and the portable configuration; none for a plain folder.
    pub snapshot: Option<Snapshot>,
}

/// The repository step of a snapshot. A plain folder (not a Git repository)
/// is an ordinary project whose working files alone are copied: the log says
/// so once, at info level, when a snapshot first finds it so (and again only
/// if it has been a repository in between), never as a failure. An actual
/// repository's failures still warn and fail the snapshot.
pub(super) async fn describe(
    pro: &super::ProState,
    workspace: &str,
    root: &Path,
) -> Result<Described> {
    let repository = optional(root, &["rev-parse", "--git-dir"]).await?.is_some();
    let first_seen = {
        let mut plain = crate::lock(&pro.plain_folders);
        if repository {
            plain.remove(workspace);
            false
        } else {
            plain.insert(workspace.to_owned())
        }
    };
    if !repository {
        if first_seen {
            tracing::info!("no git repository; only the working files are copied");
        }
        return Ok(Described::default());
    }
    // `symbolic-ref -q` exits 1 without a word on a detached HEAD: no
    // branch, not a failure.
    let branch = optional(root, &["symbolic-ref", "-q", "HEAD"])
        .await
        .ok()
        .flatten()
        .filter(|branch| !branch.is_empty());
    Ok(Described {
        branch,
        origin: mirror::repository_origin(root).await,
        snapshot: Some(capture(root).await?),
    })
}

async fn capture(root: &Path) -> Result<Snapshot> {
    let format = transport::git_output(
        project_git(root).await?,
        &["rev-parse", "--show-object-format"],
        vec![],
    )
    .await?;
    ensure!(
        format == b"sha1\n",
        "Cloud Git transfer currently requires a SHA-1 repository; SHA-256 service support is unavailable"
    );
    let head = optional(root, &["rev-parse", "--verify", "HEAD"]).await?;
    let bytes = transport::git_output(
        project_git(root).await?,
        &["config", "--local", "--no-includes", "--null", "--list"],
        vec![],
    )
    .await?;
    let mut config = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let text = std::str::from_utf8(record)?;
        let Some((key, value)) = text.split_once('\n') else {
            continue;
        };
        let entry = Entry {
            key: key.to_string(),
            value: value.to_string(),
        };
        if allowed(&entry) {
            ensure!(
                config.len() < MAX_CONFIG,
                "portable Git configuration exceeds limit"
            );
            config.push(entry);
        }
    }
    Ok(Snapshot {
        head,
        config,
        staging: None,
    })
}
pub(super) async fn keep_source_head(root: &Path, cache: &Path) -> Result<()> {
    if optional(root, &["rev-parse", "--verify", "HEAD"])
        .await?
        .is_some()
    {
        transport::git_output(
            transport::git(cache, None).await?,
            &[
                "fetch",
                "--no-tags",
                root.to_str().context("invalid workspace path")?,
                &format!("+HEAD:{SOURCE_HEAD}"),
            ],
            vec![],
        )
        .await?;
    }
    Ok(())
}
async fn install_config(
    root: &Path,
    config: &[Entry],
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<()> {
    ensure!(
        config.len() <= MAX_CONFIG && config.iter().all(allowed),
        "unsafe portable Git configuration"
    );
    let mut existed = HashSet::new();
    for entry in config {
        if !existed.insert(entry.key.clone()) {
            continue;
        }
        if optional(root, &["config", "--local", "--get-all", &entry.key])
            .await?
            .is_some()
        {
            continue;
        }
        for value in config.iter().filter(|value| value.key == entry.key) {
            check()?;
            transport::git_output(
                project_git(root).await?,
                &["config", "--local", "--add", &entry.key, &value.value],
                vec![],
            )
            .await?;
        }
    }
    Ok(())
}
/// Whether `branch` is one `receive` kept for the other machine's diverged
/// work: `<branch>@cloud-<12 hex>`.
pub(super) fn cloud_branch_name(branch: &str) -> bool {
    branch.rsplit_once("@cloud-").is_some_and(|(base, commit)| {
        !base.is_empty()
            && commit.len() == 12
            && commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// The `<branch>@cloud-<commit>` branches in the project's repository, read
/// live from its refs, so one the user merged and deleted drops off. Only a
/// folder with its own `.git`; any failure reads as none. The child runs
/// outside the transfer slots (a long mirror fetch never delays a listing),
/// on a short deadline, with bounded output.
pub(super) async fn cloud_branches(root: &Path) -> Vec<String> {
    use tokio::io::AsyncReadExt;
    const OUTPUT_MAX: u64 = 256 * 1024;
    if tokio::fs::symlink_metadata(root.join(".git"))
        .await
        .is_err()
    {
        return Vec::new();
    }
    let Ok(mut command) = project_git(root).await else {
        return Vec::new();
    };
    command
        .args(["for-each-ref", "--format=%(refname)", "refs/heads/"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return Vec::new();
    };
    let Some(stdout) = child.stdout.take() else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    let read = async {
        stdout.take(OUTPUT_MAX).read_to_end(&mut bytes).await?;
        child.wait().await
    };
    // A listing cut at the cap (Git then stops on a closed pipe) still names
    // whole refs up to its last line; only that line may be partial.
    let capped = match tokio::time::timeout(Duration::from_secs(5), read).await {
        Ok(Ok(status)) => !status.success() && bytes.len() as u64 >= OUTPUT_MAX,
        _ => return Vec::new(),
    };
    if capped {
        let whole = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |at| at + 1);
        bytes.truncate(whole);
    }
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| line.strip_prefix("refs/heads/"))
        .filter(|branch| cloud_branch_name(branch))
        .take(32)
        .map(str::to_owned)
        .collect()
}
/// Run repository adoption against a private checkout. Object data reads from
/// bounded descriptor-anchored staging, including staged-only Git objects; only
/// changed files are contributed to the installation transaction. The real
/// index (including staged-only contents) never participates in a Git write.
pub(super) struct Incoming<'a> {
    pub cache: &'a Path,
    pub credentials: &'a MirrorCredentials,
    pub branch: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub snapshot: Option<&'a Snapshot>,
    pub staging: Option<StagingIncoming<'a>>,
}
pub(super) struct StagingIncoming<'a> {
    pub handoff: &'a Path,
    pub baseline: Option<(&'a Path, &'a staging::Descriptor)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum StagingStatus {
    Uncaptured,
    Synced,
    Conflicts {
        paths: Vec<String>,
        total: usize,
        recovery: String,
    },
}
pub(super) struct Prepared {
    pub branches: Vec<String>,
    pub writes: Vec<super::install::Write>,
    pub staging: StagingStatus,
}
pub(super) async fn install_roots(root: &Path) -> Result<Vec<PathBuf>> {
    let Some(actual) = optional(root, &["rev-parse", "--absolute-git-dir"]).await? else {
        return Ok(Vec::new());
    };
    let common = optional(root, &["rev-parse", "--git-common-dir"])
        .await?
        .context("Git common directory unavailable")?;
    let common = root.join(common);
    tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>> {
        let actual = std::fs::canonicalize(actual)?;
        let common = std::fs::canonicalize(common)?;
        super::install::directory(&actual)?;
        super::install::directory(&common)?;
        let mut roots = vec![actual];
        if !roots.contains(&common) {
            roots.push(common);
        }
        Ok(roots)
    })
    .await?
}

pub(super) async fn prepare_receive(
    original: &Path,
    checkout: &Path,
    stage: &Path,
    incoming: Incoming<'_>,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<Prepared> {
    let credentials = incoming.credentials;
    let branch = incoming.branch;
    let snapshot = incoming.snapshot;
    if branch.is_none() && snapshot.is_none() {
        return Ok(Prepared {
            branches: Vec::new(),
            writes: Vec::new(),
            staging: StagingStatus::Uncaptured,
        });
    }
    let git_dir = optional(original, &["rev-parse", "--absolute-git-dir"]).await?;
    let existed = git_dir.is_some();
    let mut checked = HashSet::new();
    let mut common_dir = None;
    let mut worktree_dir = None;
    let before = stage.join("before");
    let staged_git = checkout.join(".git");
    if let Some(git_dir) = git_dir {
        let common = optional(original, &["rev-parse", "--git-common-dir"])
            .await?
            .context("Git common directory unavailable")?;
        let common = original.join(common);
        let actual = PathBuf::from(git_dir);
        let (common, actual) = tokio::task::spawn_blocking(move || -> Result<_> {
            Ok((
                std::fs::canonicalize(common)?,
                std::fs::canonicalize(actual)?,
            ))
        })
        .await??;
        let occupancy = transport::git_output(
            transport::git(original, None).await?,
            &["worktree", "list", "--porcelain"],
            vec![],
        )
        .await?;
        for line in std::str::from_utf8(&occupancy)?.lines() {
            if let Some(branch) = line.strip_prefix("branch ") {
                checked.insert(branch.to_owned());
            }
        }
        let budget = credentials.storage_limit_bytes.min(1024 * 1024 * 1024);
        let (source, destination, staged) = (common.clone(), before.clone(), staged_git.clone());
        tokio::task::spawn_blocking(move || -> Result<()> {
            super::install::snapshot(
                &source,
                &destination,
                &|path| {
                    !path.starts_with("worktrees")
                        && !path.starts_with("hooks")
                        && !path.to_string_lossy().ends_with(".lock")
                },
                budget,
            )?;
            super::install::snapshot(&destination, &staged, &|_| true, budget)?;
            Ok(())
        })
        .await??;
        if actual != common {
            let (project, pointer) = (original.to_path_buf(), stage.join("pointer-before"));
            tokio::task::spawn_blocking(move || {
                super::install::snapshot(
                    &project,
                    &pointer,
                    &|path| path == Path::new(".git"),
                    4096,
                )
            })
            .await??;
            let (worktree_source, staged) = (actual.clone(), staged_git.clone());
            tokio::task::spawn_blocking(move || -> Result<()> {
                let temporary = staged.with_file_name("worktree-before");
                super::install::snapshot(
                    &worktree_source,
                    &temporary,
                    &|path| {
                        matches!(path.to_str(), Some("HEAD" | "index" | "config.worktree"))
                            || path.starts_with("refs")
                            || shared_index_name(path)
                    },
                    budget,
                )?;
                for entry in std::fs::read_dir(&temporary)? {
                    let entry = entry?;
                    if entry.file_type()?.is_file() {
                        std::fs::copy(entry.path(), staged.join(entry.file_name()))?;
                    }
                }
                Ok(())
            })
            .await??;
            worktree_dir = Some(actual);
        }
        common_dir = Some(common);
    } else {
        tokio::fs::create_dir_all(&before).await?;
    }
    check()?;
    let local_staging = if existed
        && snapshot
            .and_then(|snapshot| snapshot.staging.as_ref())
            .is_some()
    {
        Some(staging::local_index(checkout, &stage.join("destination.index")).await?)
    } else {
        None
    };
    let preserved = STAGED_CHECKOUT
        .scope(
            checkout.to_path_buf(),
            receive_inner(checkout, &incoming, check, Some(&checked)),
        )
        .await?;
    let staging = if let Some(descriptor) = snapshot.and_then(|snapshot| snapshot.staging.as_ref())
    {
        let transfer = incoming
            .staging
            .as_ref()
            .context("Portable Git staging artifact is unavailable")?;
        STAGED_CHECKOUT
            .scope(
                checkout.to_path_buf(),
                staging::receive(
                    checkout,
                    local_staging.as_ref(),
                    staging::Receiving {
                        handoff: transfer.handoff,
                        descriptor,
                        baseline: transfer.baseline,
                        budget: credentials.storage_limit_bytes,
                        max_file: credentials.max_file_bytes,
                    },
                    check,
                ),
            )
            .await?
    } else {
        StagingStatus::Uncaptured
    };
    let target = common_dir.unwrap_or_else(|| original.join(".git"));
    let target_exists = tokio::fs::try_exists(&target).await?;
    if !target_exists {
        // The transaction creates .git descriptor-relatively beneath the
        // already-bound project root instead of pre-creating a live repository.
        let (root, before, after) = (original.to_path_buf(), before.clone(), staged_git.clone());
        let writes = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut writes = super::install::changes(&root, &before, &after)?;
            for write in &mut writes {
                write.relative = Path::new(".git").join(&write.relative);
            }
            Ok(writes)
        })
        .await??;
        return Ok(Prepared {
            branches: preserved,
            writes,
            staging,
        });
    }
    let (target, before, after, actual, project, pointer) = (
        target.clone(),
        before.clone(),
        staged_git,
        worktree_dir,
        original.to_path_buf(),
        stage.join("pointer-before/.git"),
    );
    let staging_recovery = match &staging {
        StagingStatus::Conflicts { recovery, .. } => Some(recovery.clone()),
        _ => None,
    };
    let writes = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut writes = super::install::changes(&target, &before, &after)?;
        // Even an unchanged HEAD/index is a checkout invariant: a concurrent
        // switch or staging operation must not install this plan into another
        // branch merely because its working bytes happened to be identical.
        for name in ["HEAD", "index", "config"] {
            writes.retain(|write| write.relative != Path::new(name));
            let old = before.join(name);
            let new = after.join(name);
            writes.push(super::install::Write {
                root: target.clone(),
                relative: name.into(),
                before: old.is_file().then_some(old),
                after: new.is_file().then_some(new),
            });
        }
        if let Some(actual) = actual {
            ensure!(pointer.is_file(), "linked worktree pointer disappeared");
            writes.push(super::install::Write {
                root: project,
                relative: ".git".into(),
                before: Some(pointer.clone()),
                after: Some(pointer),
            });
            // HEAD/index belong to the linked worktree, refs/config to common.
            let worktree_before = after.with_file_name("worktree-before");
            if let Some(recovery) = &staging_recovery {
                for write in &mut writes {
                    if write.relative.starts_with(recovery) {
                        write.root = actual.clone();
                    }
                }
            }
            writes.retain(|write| {
                !matches!(
                    write.relative.to_str(),
                    Some("HEAD" | "index" | "config.worktree")
                )
            });
            let mut private_names = vec![
                PathBuf::from("HEAD"),
                PathBuf::from("index"),
                PathBuf::from("config.worktree"),
            ];
            for entry in std::fs::read_dir(&worktree_before)? {
                let entry = entry?;
                let name = PathBuf::from(entry.file_name());
                if shared_index_name(&name) {
                    writes.retain(|write| write.relative != name);
                    private_names.push(name);
                }
            }
            for name in private_names {
                let old = worktree_before.join(&name);
                let new = after.join(&name);
                if old.is_file() || new.is_file() {
                    writes.push(super::install::Write {
                        root: actual.clone(),
                        relative: name,
                        before: old.is_file().then_some(old),
                        after: new.is_file().then_some(new),
                    });
                }
            }
        }
        // Objects become durable before refs can point at them.
        writes.sort_by_key(|write| !write.relative.starts_with("objects"));
        Ok(writes)
    })
    .await??;
    Ok(Prepared {
        branches: preserved,
        writes,
        staging,
    })
}

// Split indexes resolve their backing file relative to the actual worktree Git
// directory. Preserve that bounded dependency in the private transaction copy.
fn shared_index_name(path: &Path) -> bool {
    path.components().count() == 1
        && path
            .to_str()
            .and_then(|name| name.strip_prefix("sharedindex."))
            .is_some_and(|oid| {
                matches!(oid.len(), 40 | 64)
                    && oid
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
}

async fn receive_inner(
    root: &Path,
    incoming: &Incoming<'_>,
    check: &(dyn Fn() -> Result<()> + Sync),
    occupied: Option<&HashSet<String>>,
) -> Result<Vec<String>> {
    let Incoming {
        cache,
        credentials,
        branch,
        origin,
        snapshot,
        staging: _,
    } = *incoming;
    if branch.is_none() && snapshot.is_none() {
        return Ok(Vec::new());
    }
    if let Some(branch) = branch {
        ensure!(
            branch.starts_with("refs/heads/") && reference(branch),
            "invalid project branch"
        );
    }
    if let Some(snapshot) = snapshot {
        ensure!(
            snapshot.config.len() <= MAX_CONFIG && snapshot.config.iter().all(allowed),
            "unsafe portable Git configuration"
        );
        ensure!(
            snapshot
                .head
                .as_ref()
                .is_none_or(|head| matches!(head.len(), 40 | 64)
                    && head.bytes().all(|c| c.is_ascii_hexdigit())),
            "invalid repository head"
        );
    }
    if let Some(snapshot) = snapshot {
        ensure!(
            snapshot.head.as_ref().is_none_or(|head| head.len() == 40),
            "Cloud Git transfer currently requires a SHA-1 repository; SHA-256 service support is unavailable"
        );
    }
    if let Some(descriptor) = snapshot.and_then(|snapshot| snapshot.staging.as_ref()) {
        let artifact = incoming
            .staging
            .as_ref()
            .context("Portable Git staging artifact is unavailable")?;
        staging::require_service_format(artifact.handoff, descriptor).await?;
    }
    mirror::initialize(cache).await?;
    let url = transport::endpoint(&credentials.repository_url)?;
    transport::git_output(
        transport::git(cache, Some((&credentials.username, &credentials.password))).await?,
        &["fetch", "--prune", "--no-tags", &url, "+refs/*:refs/*"],
        vec![],
    )
    .await?;
    // A network fetch can take minutes. Recheck the account before touching
    // the selected repository, even when the caller was current at entry.
    check()?;
    let existing = optional(root, &["rev-parse", "--git-dir"]).await?.is_some();
    if !existing {
        check()?;
        transport::git_output(project_git(root).await?, &["init", "--quiet", "."], vec![]).await?;
    }
    check()?;
    let namespace = format!(
        "refs/chimaera-transfer/{}/",
        chimaera_core::generate_token()
    );
    transport::git_output(
        project_git(root).await?,
        &[
            "fetch",
            "--no-write-fetch-head",
            "--no-tags",
            cache.to_str().context("invalid repository cache")?,
            &format!("+refs/*:{namespace}*"),
        ],
        vec![],
    )
    .await
    .context("could not prepare repository refs")?;
    if existing
        && STAGED_CHECKOUT
            .try_with(|checkout| checkout == root)
            .unwrap_or(false)
    {
        // Copying a checkout changes stat-cache timestamps. Refresh only its
        // private index before read-tree's up-to-date check; exit 1 merely
        // reports a genuinely dirty file, which the usual kept-branch path
        // handles. The real index is still the journal's untouched before-image.
        let mut refresh = project_git(root).await?;
        refresh.args(["update-index", "--refresh"]);
        let _ = transport::run(
            refresh,
            vec![],
            Duration::from_secs(15),
            transport::PATH_CAP,
        )
        .await?;
    }
    let result = publish_refs(root, &namespace, existing, check, occupied)
        .await
        .context("could not plan repository ref adoption");
    // Remove only this operation's private namespace, even if publishing failed.
    let refs = transport::git_output(
        project_git(root).await?,
        &["for-each-ref", "--format=%(refname)", &namespace],
        vec![],
    )
    .await?;
    let mut cleanup = Vec::new();
    for name in refs
        .split(|byte| *byte == b'\n')
        .filter(|name| !name.is_empty())
    {
        cleanup.extend_from_slice(b"delete ");
        cleanup.extend_from_slice(name);
        cleanup.push(b'\n');
    }
    if !cleanup.is_empty() {
        transport::git_output(
            project_git(root).await?,
            &["update-ref", "--stdin"],
            cleanup,
        )
        .await?;
    }
    let preserved = result?;
    check()?;
    if !existing {
        if let Some(branch) = branch {
            transport::git_output(
                project_git(root).await?,
                &["symbolic-ref", "HEAD", branch],
                vec![],
            )
            .await?;
        } else if let Some(head) = snapshot.and_then(|snapshot| snapshot.head.as_deref()) {
            transport::git_output(
                project_git(root).await?,
                &["update-ref", "--no-deref", "HEAD", head],
                vec![],
            )
            .await?;
        }
        if optional(root, &["rev-parse", "--verify", "HEAD"])
            .await?
            .is_some()
        {
            check()?;
            transport::git_output(project_git(root).await?, &["read-tree", "HEAD"], vec![]).await?;
        }
    }
    if let Some(snapshot) = snapshot {
        install_config(root, &snapshot.config, check).await?;
    }
    if let Some(origin) = origin.filter(|url| safe_url(url)) {
        if optional(root, &["config", "--local", "--get", "remote.origin.url"])
            .await?
            .is_none()
        {
            check()?;
            transport::git_output(
                project_git(root).await?,
                &["remote", "add", "origin", origin],
                vec![],
            )
            .await?;
        }
    }
    Ok(preserved)
}
async fn publish_refs(
    root: &Path,
    namespace: &str,
    existing: bool,
    check: &(dyn Fn() -> Result<()> + Sync),
    occupied: Option<&HashSet<String>>,
) -> Result<Vec<String>> {
    let refs = transport::git_output(
        project_git(root).await?,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            namespace,
        ],
        vec![],
    )
    .await?;
    let records: Vec<_> = std::str::from_utf8(&refs)?.lines().collect();
    ensure!(
        records.len() <= MAX_REFS,
        "repository ref count exceeds limit"
    );
    let current = optional(root, &["symbolic-ref", "-q", "HEAD"]).await?;
    let worktrees = transport::git_output(
        project_git(root).await?,
        &["worktree", "list", "--porcelain"],
        vec![],
    )
    .await?;
    let mut checked: HashSet<_> = std::str::from_utf8(&worktrees)?
        .lines()
        .filter_map(|line| line.strip_prefix("branch "))
        .collect();
    if let Some(occupied) = occupied {
        checked.extend(occupied.iter().map(String::as_str));
    }
    let clean = transport::git_output(
        project_git(root).await?,
        &["status", "--porcelain", "--untracked-files=no"],
        vec![],
    )
    .await?
    .is_empty();
    let mut preserved = Vec::new();
    for record in records {
        let (temporary, incoming) = record.split_once(' ').context("invalid fetched ref")?;
        let reference = format!(
            "refs/{}",
            temporary
                .strip_prefix(namespace)
                .context("foreign fetched ref")?
        );
        if reference == SOURCE_HEAD
            || reference.starts_with("refs/chimaera-transfer/")
            || reference.starts_with("refs/chimaera/staging/")
        {
            continue;
        }
        ensure!(self::reference(&reference), "invalid fetched ref name");
        let previous = optional(root, &["rev-parse", "--verify", &reference]).await?;
        if previous.as_deref() == Some(incoming) {
            continue;
        }
        if previous.is_none() || !existing {
            check()?;
            transport::git_output(
                project_git(root).await?,
                &[
                    "update-ref",
                    &reference,
                    incoming,
                    &"0".repeat(incoming.len()),
                ],
                vec![],
            )
            .await?;
            continue;
        }
        let previous = previous.unwrap();
        let mut ancestor = project_git(root).await?;
        ancestor.args(["merge-base", "--is-ancestor", &previous, incoming]);
        let advance = reference.starts_with("refs/heads/")
            && transport::run(ancestor, vec![], Duration::from_secs(15), 1024)
                .await?
                .success;
        check()?;
        if advance
            && !checked.contains(reference.as_str())
            && advance_unchecked_branch(root, &reference, incoming).await?
        {
            // Fetch checks linked-worktree occupancy and refuses non-fast-forward
            // changes against the live ref, not our earlier inventory.
        } else if advance
            && current.as_deref() == Some(&reference)
            && clean
            && adopt_identical_untracked(root, &previous, incoming, async {
                fast_forward_current(root, &reference, &previous, incoming, check).await
            })
            .await?
        {
            // The prepared ref transaction adopted this exact branch.
        } else if !reference.starts_with("refs/heads/") {
            // Remote-tracking refs and tags are the user's own view of other
            // remotes; a divergent copy from the cloud is not worth keeping.
        } else {
            let kept = format!("{reference}@cloud-{}", &incoming[..12]);
            let existing = optional(root, &["rev-parse", "--verify", &kept]).await?;
            if existing.as_deref() != Some(incoming) {
                ensure!(existing.is_none(), "cloud preservation ref already differs");
                check()?;
                transport::git_output(
                    project_git(root).await?,
                    &["update-ref", &kept, incoming, &"0".repeat(incoming.len())],
                    vec![],
                )
                .await?;
            }
            if reference.starts_with("refs/heads/") {
                preserved.push(kept.trim_start_matches("refs/heads/").to_string());
            }
        }
    }
    Ok(preserved)
}

/// A fast-forward refuses to overwrite untracked files. Untracked files
/// identical to the incoming ones are set aside (same filesystem, reserved
/// staging name) for the fast-forward to write the same bytes, and put back
/// if it does not happen; any other collision keeps the cloud branch
/// separate instead of failing the return every pass.
async fn adopt_identical_untracked(
    root: &Path,
    previous: &str,
    incoming: &str,
    fast_forward: impl std::future::Future<Output = Result<bool>>,
) -> Result<bool> {
    let untracked = transport::git_output(
        project_git(root).await?,
        &["ls-files", "-z", "--others", "--exclude-standard"],
        vec![],
    )
    .await?;
    let untracked: HashSet<&[u8]> = untracked
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .collect();
    let mut identical = Vec::new();
    if !untracked.is_empty() {
        let added = transport::git_output(
            project_git(root).await?,
            &[
                "diff",
                "-z",
                "--no-renames",
                "--name-only",
                "--diff-filter=AM",
                previous,
                incoming,
            ],
            vec![],
        )
        .await?;
        for path in added.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            if !untracked.contains(path) {
                continue;
            }
            let path = std::str::from_utf8(path).context("invalid incoming path")?;
            let wanted = optional(root, &["rev-parse", &format!("{incoming}:{path}")]).await?;
            let have = optional(root, &["hash-object", "--no-filters", "--", path]).await?;
            if wanted.is_none() || wanted != have || identical.len() >= 4096 {
                return Ok(false);
            }
            identical.push(std::path::PathBuf::from(path));
        }
    }
    if identical.is_empty() {
        return fast_forward.await;
    }
    let aside = root.join(format!(
        "{}untracked-{}",
        crate::persist::PROJECT_STAGING_PREFIX,
        &chimaera_core::generate_token()[..16]
    ));
    let moved = {
        let (root, aside, identical) = (root.to_path_buf(), aside.clone(), identical.clone());
        tokio::task::spawn_blocking(move || -> Result<()> {
            for relative in &identical {
                let target = aside.join(relative);
                std::fs::create_dir_all(target.parent().context("invalid untracked path")?)?;
                std::fs::rename(root.join(relative), target)?;
            }
            Ok(())
        })
    };
    let restore = {
        let (root, aside) = (root.to_path_buf(), aside.clone());
        move |adopted: bool| {
            tokio::task::spawn_blocking(move || -> Result<()> {
                if !adopted {
                    for relative in &identical {
                        let source = aside.join(relative);
                        if source.exists() && !root.join(relative).exists() {
                            std::fs::rename(source, root.join(relative))?;
                        }
                    }
                }
                std::fs::remove_dir_all(&aside)?;
                Ok(())
            })
        }
    };
    if let Err(error) = moved.await? {
        restore(false).await??;
        return Err(error);
    }
    let result = fast_forward.await;
    restore(matches!(result, Ok(true))).await??;
    result
}

async fn advance_unchecked_branch(root: &Path, reference: &str, incoming: &str) -> Result<bool> {
    let mut command = project_git(root).await?;
    command.args([
        "fetch",
        "--no-write-fetch-head",
        "--no-tags",
        ".",
        &format!("{incoming}:{reference}"),
    ]);
    Ok(transport::run(
        command,
        vec![],
        Duration::from_secs(45),
        transport::PATH_CAP,
    )
    .await?
    .success)
}

/// Locks the real index before reading the current checkout. A prepared Git ref
/// transaction then locks the named branch and HEAD. `read-tree` writes through
/// our reserved index file, so a concurrent checkout cannot change its target.
async fn fast_forward_current(
    root: &Path,
    reference: &str,
    previous: &str,
    incoming: &str,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<bool> {
    let index = optional(root, &["rev-parse", "--git-path", "index"])
        .await?
        .context("Git index unavailable")?;
    check()?;
    let index = root.join(index);
    let index_lock = index.with_file_name(format!(
        "{}.lock",
        index
            .file_name()
            .context("invalid Git index")?
            .to_string_lossy()
    ));
    let index_copy = index.clone();
    let reserve = index_lock.clone();
    let reservation = tokio::task::spawn_blocking(move || -> Result<IndexReservation> {
        use std::io::{Read, Write};
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(&reserve).context("Git index is busy")?;
        let copied = (|| -> Result<()> {
            let (input, metadata) = crate::fs::open_regular(&index_copy)?;
            ensure!(
                metadata.len() <= 64 * 1024 * 1024,
                "Git index exceeds transfer limit"
            );
            let count = std::io::copy(&mut input.take(64 * 1024 * 1024 + 1), &mut output)?;
            ensure!(
                count <= 64 * 1024 * 1024,
                "Git index grew beyond transfer limit"
            );
            output.flush()?;
            Ok(())
        })();
        if let Err(error) = copied {
            let _ = std::fs::remove_file(&reserve);
            return Err(error);
        }
        Ok(IndexReservation {
            path: Some(reserve),
            preserve: false,
        })
    })
    .await?;
    let Ok(reservation) = reservation else {
        return Ok(false);
    };
    check()?;
    let mut transaction = RefTransaction::begin(root, reservation).await?;
    let current = optional(root, &["symbolic-ref", "-q", "HEAD"]).await?;
    if current.as_deref() != Some(reference) {
        return Ok(false);
    }
    transaction
        .send(&format!(
            "start\nupdate {reference} {incoming} {previous}\nprepare\n"
        ))
        .await?;
    if transaction.confirm("start: ok").await.is_err()
        || transaction.confirm("prepare: ok").await.is_err()
    {
        // Older Git can reject prepared transactions. Preserve the cloud ref
        // without adopting the checkout; no working files have changed yet.
        return Ok(false);
    }
    // Git has now taken HEAD.lock as well as the explicitly named ref lock.
    if optional(root, &["symbolic-ref", "-q", "HEAD"])
        .await?
        .as_deref()
        != Some(reference)
    {
        return Ok(false);
    }
    let clean = transport::git_output(
        project_git(root).await?,
        &["status", "--porcelain", "--untracked-files=no"],
        vec![],
    )
    .await?
    .is_empty();
    if !clean {
        return Ok(false);
    }
    check()?;
    finalize_current(
        root.to_owned(),
        index,
        reference.to_owned(),
        previous.to_owned(),
        incoming.to_owned(),
        transaction,
        #[cfg(test)]
        None,
    )
    .await
}

#[cfg(test)]
struct FinalizationPause {
    committed: tokio::sync::oneshot::Sender<()>,
    proceed: tokio::sync::oneshot::Receiver<()>,
}

/// Once working files can change, cancellation of the caller must not split
/// the ref commit from its matching index. This owned task runs only bounded
/// children; dropping its JoinHandle leaves finalization running to completion.
async fn finalize_current(
    root: std::path::PathBuf,
    index: std::path::PathBuf,
    reference: String,
    previous: String,
    incoming: String,
    mut transaction: RefTransaction,
    #[cfg(test)] pause: Option<FinalizationPause>,
) -> Result<bool> {
    let staged = STAGED_CHECKOUT
        .try_with(|checkout| checkout == &root)
        .unwrap_or(false);
    tokio::spawn(async move {
        let index_lock = transaction
            .index_lock
            .as_ref()
            .and_then(|reservation| reservation.path.as_ref())
            .context("Git index reservation unavailable")?
            .clone();
        let mut command = project_git(&root).await?;
        if staged {
            command.env("GIT_WORK_TREE", &root);
        }
        command.env("GIT_INDEX_FILE", &index_lock);
        transport::git_output(
            command,
            &["read-tree", "-m", "-u", &previous, &incoming],
            vec![],
        )
        .await?;

        let mut reservation = transaction.index_lock.take().unwrap();
        // Even a missing commit acknowledgment can mean the ref was written.
        // Keep the prepared index on ambiguous failure, for explicit recovery.
        reservation.preserve = true;
        let committed = async {
            transaction.send("commit\n").await?;
            transaction.confirm("commit: ok").await
        }
        .await;
        #[cfg(test)]
        if let Some(pause) = pause {
            let _ = pause.committed.send(());
            let _ = tokio::time::timeout(Duration::from_secs(5), pause.proceed).await;
        }
        transaction.stdin.take();
        let exited = tokio::time::timeout(
            Duration::from_secs(5),
            transaction.child.as_mut().unwrap().wait(),
        )
        .await;
        let finished = matches!(exited, Ok(Ok(_)));
        if finished {
            transaction.child.take();
        }
        if let Err(error) = committed {
            // A closed pipe or failed acknowledgment alone cannot decide
            // whether commit happened. Resolve the live ref before cleanup.
            let actual = optional(&root, &["rev-parse", "--verify", &reference]).await?;
            if actual.as_deref() != Some(&incoming) {
                reservation.preserve = !finished || actual.as_deref() != Some(&previous);
                return Err(error.context("Git ref adoption did not complete"));
            }
        }
        tokio::task::spawn_blocking(move || -> Result<()> {
            // Ownership and rename stay in one synchronous closure: a late
            // cleanup can never remove a subsequent Git operation's lock.
            let path = reservation.path.take().unwrap();
            std::fs::rename(&path, &index).with_context(|| {
                format!(
                    "Git index adoption failed; prepared index retained at {}",
                    path.display()
                )
            })?;
            Ok(())
        })
        .await??;
        Ok(true)
    })
    .await?
}
struct IndexReservation {
    path: Option<std::path::PathBuf>,
    preserve: bool,
}
impl Drop for IndexReservation {
    fn drop(&mut self) {
        if self.preserve {
            return;
        }
        if let Some(path) = self.path.take() {
            let cleanup = move || {
                // read-tree uses our reservation as its alternate index. A
                // cancelled child may leave that alternate index's own lock.
                let nested = path.with_file_name(format!(
                    "{}.lock",
                    path.file_name().unwrap().to_string_lossy()
                ));
                let _ = std::fs::remove_file(nested);
                let _ = std::fs::remove_file(path);
            };
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(cleanup);
            } else {
                cleanup();
            }
        }
    }
}
struct RefTransaction {
    child: Option<tokio::process::Child>,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
    index_lock: Option<IndexReservation>,
    permit: Option<tokio::sync::SemaphorePermit<'static>>,
    transaction_permit: Option<tokio::sync::SemaphorePermit<'static>>,
}
impl RefTransaction {
    async fn begin(root: &Path, index_lock: IndexReservation) -> Result<Self> {
        async {
            let transaction_permit = REF_TRANSACTIONS.acquire().await?;
            let mut command = project_git(root).await?;
            let permit = transport::child_permit().await?;
            let mut child = command
                .args(["update-ref", "--stdin"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(false)
                .spawn()?;
            let stdin = child
                .stdin
                .take()
                .context("Git transaction input unavailable")?;
            let stdout = tokio::io::BufReader::new(
                child
                    .stdout
                    .take()
                    .context("Git transaction output unavailable")?,
            );
            Ok::<_, anyhow::Error>(Self {
                child: Some(child),
                stdin: Some(stdin),
                stdout,
                index_lock: Some(index_lock),
                permit: Some(permit),
                transaction_permit: Some(transaction_permit),
            })
        }
        .await
    }
    async fn send(&mut self, command: &str) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let input = self.stdin.as_mut().context("Git transaction closed")?;
        tokio::time::timeout(Duration::from_secs(5), input.write_all(command.as_bytes())).await??;
        Ok(())
    }
    async fn confirm(&mut self, expected: &str) -> Result<()> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        let mut line = String::new();
        let mut limited = (&mut self.stdout).take(128);
        tokio::time::timeout(Duration::from_secs(5), limited.read_line(&mut line)).await??;
        ensure!(line.trim() == expected, "Git ref transaction was refused");
        Ok(())
    }
}
impl Drop for RefTransaction {
    fn drop(&mut self) {
        use tokio::io::AsyncWriteExt;
        let child = self.child.take();
        let stdin = self.stdin.take();
        let path = self.index_lock.take();
        let permit = self.permit.take();
        let transaction_permit = self.transaction_permit.take();
        // EOF/abort makes Git remove its own ref locks. Do not kill it merely
        // because the surrounding mirror was cancelled: SIGKILL leaves locks.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _permit = permit;
                let _transaction_permit = transaction_permit;
                if let Some(mut child) = child {
                    if let Some(mut stdin) = stdin {
                        let _ = tokio::time::timeout(
                            Duration::from_secs(1),
                            stdin.write_all(b"abort\n"),
                        )
                        .await;
                    }
                    if tokio::time::timeout(Duration::from_secs(3), child.wait())
                        .await
                        .is_err()
                    {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                    }
                }
                drop(path);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn staged_return_preserves_three_git_versions_and_linked_worktree_state_until_commit() {
        use axum::{
            extract::{Path as UrlPath, State},
            http::StatusCode,
            routing::get,
            Router,
        };
        async fn serve(
            State(root): State<PathBuf>,
            UrlPath(path): UrlPath<String>,
        ) -> Result<Vec<u8>, StatusCode> {
            if path.split('/').any(|part| part == "..") {
                return Err(StatusCode::BAD_REQUEST);
            }
            tokio::fs::read(root.join(path))
                .await
                .map_err(|_| StatusCode::NOT_FOUND)
        }
        for (dirty, linked, fresh) in [
            (true, false, false),
            (false, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let temporary = std::env::temp_dir().join(format!(
                "chimaera-staged-return-{}",
                chimaera_core::generate_token()
            ));
            std::fs::create_dir_all(temporary.join("main")).unwrap();
            let temporary = temporary.canonicalize().unwrap();
            let main = temporary.join("main");
            git(&main, &["init", "--quiet", "--initial-branch=main"]).await;
            std::fs::write(main.join("file.txt"), b"head version").unwrap();
            git(&main, &["add", "."]).await;
            git(&main, &["commit", "-qm", "base"]).await;
            let old = git(&main, &["rev-parse", "HEAD"]).await;
            git(&main, &["switch", "--quiet", "-c", "cloud"]).await;
            std::fs::write(main.join("file.txt"), b"cloud version").unwrap();
            git(&main, &["add", "."]).await;
            git(&main, &["commit", "-qm", "cloud"]).await;
            let incoming = git(&main, &["rev-parse", "HEAD"]).await;
            git(&main, &["switch", "--quiet", "main"]).await;
            let (project, branch) = if fresh {
                let project = temporary.join("fresh");
                std::fs::create_dir(&project).unwrap();
                (project, "refs/heads/main")
            } else if linked {
                let project = temporary.join("linked");
                git(
                    &main,
                    &[
                        "worktree",
                        "add",
                        "--quiet",
                        "-b",
                        "work",
                        project.to_str().unwrap(),
                    ],
                )
                .await;
                (project, "refs/heads/work")
            } else {
                (main.clone(), "refs/heads/main")
            };
            if dirty {
                std::fs::write(project.join("file.txt"), b"staged-only version").unwrap();
                git(&project, &["add", "file.txt"]).await;
                std::fs::write(project.join("file.txt"), b"working version").unwrap();
            }
            if !fresh {
                git(
                    &project,
                    &["config", "credential.helper", "local-secret-helper"],
                )
                .await;
            }
            let index = if fresh {
                project.join(".git/index")
            } else {
                PathBuf::from(
                    git(
                        &project,
                        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
                    )
                    .await,
                )
            };
            let old_index = std::fs::read(&index).ok();
            let config_path = main.join(".git/config");
            let old_config = std::fs::read(&config_path).unwrap();
            git(
                &temporary,
                &[
                    "clone",
                    "--quiet",
                    "--bare",
                    main.to_str().unwrap(),
                    "repository.git",
                ],
            )
            .await;
            let mirror = temporary.join("repository.git");
            git(&mirror, &["update-ref", branch, &incoming]).await;
            git(&mirror, &["update-server-info"]).await;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(
                    listener,
                    Router::new()
                        .route("/{*path}", get(serve))
                        .with_state(mirror),
                )
                .await
                .unwrap()
            });
            let credentials = MirrorCredentials {
                workspace_id: "w-fixture".into(),
                repository_url: url.clone(),
                working_tree_url: url,
                username: "fixture".into(),
                password: "fixture".into(),
                read_only: true,
                storage_limit_bytes: 16 * 1024 * 1024,
                max_file_bytes: 1024 * 1024,
            };
            let before = temporary.join("tree-before");
            let checkout = temporary.join("checkout");
            super::super::install::snapshot(
                &project,
                &before,
                &|path| !path.starts_with(".git"),
                16 * 1024 * 1024,
            )
            .unwrap();
            super::super::install::snapshot(&before, &checkout, &|_| true, 16 * 1024 * 1024)
                .unwrap();
            let Prepared {
                branches: kept,
                mut writes,
                staging: _,
            } = prepare_receive(
                &project,
                &checkout,
                &temporary.join("repository-stage"),
                Incoming {
                    cache: &temporary.join("incoming.git"),
                    credentials: &credentials,
                    branch: Some(branch),
                    origin: None,
                    snapshot: None,
                    staging: None,
                },
                &|| Ok(()),
            )
            .await
            .unwrap();
            assert_eq!(std::fs::read(&index).ok(), old_index);
            assert_eq!(std::fs::read(&config_path).unwrap(), old_config);
            if !fresh {
                assert_eq!(git(&project, &["rev-parse", "HEAD"]).await, old);
                assert_eq!(
                    git(&project, &["show", ":file.txt"]).await,
                    if dirty {
                        "staged-only version"
                    } else {
                        "head version"
                    }
                );
            } else {
                assert!(!project.join(".git").exists());
                // Hydration overlays the received working tree after Git planning.
                std::fs::write(checkout.join("file.txt"), b"cloud version").unwrap();
            }
            if dirty {
                assert!(kept.iter().any(|branch| branch.contains("@cloud-")));
            } else {
                assert_eq!(git(&checkout, &["rev-parse", "HEAD"]).await, incoming);
            }
            writes.extend(
                super::super::install::changes(&project, &before, &checkout)
                    .unwrap()
                    .into_iter()
                    .filter(|write| !write.relative.starts_with(".git")),
            );
            let binding = super::super::install::Binding {
                endpoint: "https://fixture.invalid".into(),
                account: Some("a-fixture".into()),
                workspace: "w-fixture".into(),
                epoch: 3,
                receipt: Some("receipt-fixture".into()),
            };
            let journal = temporary.join("return-install");
            let mut transaction = super::super::install::Transaction::prepare(
                &journal,
                binding.clone(),
                writes,
                16 * 1024 * 1024,
            )
            .unwrap();
            transaction
                .reserve_git(install_roots(&project).await.unwrap(), &|| Ok(()))
                .unwrap();
            transaction
                .apply(&|| {
                    if fresh {
                        return Ok(());
                    }
                    let output = std::process::Command::new("git")
                        .args(["checkout", "-b", "external-during-return"])
                        .current_dir(&project)
                        .output()?;
                    ensure!(
                        !output.status.success(),
                        "external checkout bypassed installation locks"
                    );
                    Ok(())
                })
                .unwrap();
            // A late session/config finalization failure leaves the operation
            // uncommitted. Restart reuses the original file intents exactly.
            drop(transaction);
            let mut retry = super::super::install::Transaction::open(&journal, &binding)
                .unwrap()
                .unwrap();
            retry.reserve_git(Vec::new(), &|| Ok(())).unwrap();
            retry.apply(&|| Ok(())).unwrap();
            retry.commit(&|| Ok(())).unwrap();
            retry.cleanup().unwrap();
            if dirty {
                assert_eq!(git(&project, &["rev-parse", "HEAD"]).await, old);
                assert_eq!(
                    git(&project, &["show", ":file.txt"]).await,
                    "staged-only version"
                );
                assert_eq!(
                    std::fs::read(project.join("file.txt")).unwrap(),
                    b"working version"
                );
            } else {
                assert_eq!(git(&project, &["rev-parse", "HEAD"]).await, incoming);
                assert_eq!(git(&project, &["show", ":file.txt"]).await, "cloud version");
                assert_eq!(
                    std::fs::read(project.join("file.txt")).unwrap(),
                    b"cloud version"
                );
            }
            assert_eq!(std::fs::read(&config_path).unwrap(), old_config);
            if linked {
                assert_eq!(git(&main, &["rev-parse", "HEAD"]).await, old);
                assert_eq!(
                    std::fs::read(main.join("file.txt")).unwrap(),
                    b"head version"
                );
            }
            server.abort();
            std::fs::remove_dir_all(temporary).unwrap();
        }
    }
    #[tokio::test]
    async fn portable_staging_joins_repository_transaction_and_restart_for_fresh_and_linked_destinations(
    ) {
        use axum::{
            extract::{Path as HttpPath, State},
            routing::get,
            Router,
        };
        async fn serve(
            State(root): State<PathBuf>,
            HttpPath(path): HttpPath<String>,
        ) -> Result<Vec<u8>, axum::http::StatusCode> {
            if path.contains("..") {
                return Err(axum::http::StatusCode::BAD_REQUEST);
            }
            tokio::fs::read(root.join(path))
                .await
                .map_err(|_| axum::http::StatusCode::NOT_FOUND)
        }
        for linked in [false, true] {
            let temporary = std::env::temp_dir().join(format!(
                "chimaera-index-transaction-{}",
                chimaera_core::generate_token()
            ));
            std::fs::create_dir_all(temporary.join("source")).unwrap();
            let temporary = temporary.canonicalize().unwrap();
            let source = temporary.join("source");
            git(&source, &["init", "--quiet", "--initial-branch=main"]).await;
            std::fs::write(source.join("partial"), b"HEAD").unwrap();
            git(&source, &["add", "."]).await;
            git(&source, &["commit", "-qm", "base"]).await;
            let base_artifact = temporary.join("base-artifact");
            let (base_descriptor, _) =
                staging::capture(&source, &base_artifact, 16 * 1024 * 1024, 1024 * 1024)
                    .await
                    .unwrap();
            let destination = if linked {
                git(
                    &temporary,
                    &["clone", "--quiet", source.to_str().unwrap(), "main"],
                )
                .await;
                let root = temporary.join("linked");
                git(
                    &temporary.join("main"),
                    &[
                        "worktree",
                        "add",
                        "--quiet",
                        "-b",
                        "work",
                        root.to_str().unwrap(),
                    ],
                )
                .await;
                std::fs::write(root.join("partial"), b"LOCAL INDEX").unwrap();
                git(&root, &["add", "partial"]).await;
                std::fs::write(root.join("partial"), b"LOCAL WORKTREE").unwrap();
                git(&root, &["update-index", "--split-index"]).await;
                root
            } else {
                let root = temporary.join("fresh");
                std::fs::create_dir(&root).unwrap();
                root
            };
            std::fs::write(source.join("partial"), b"INDEX").unwrap();
            std::fs::write(source.join("only"), b"STAGED ONLY").unwrap();
            git(&source, &["add", "."]).await;
            std::fs::write(source.join("partial"), b"WORKTREE").unwrap();
            std::fs::remove_file(source.join("only")).unwrap();
            let artifact = temporary.join("artifact");
            let (descriptor, _) =
                staging::capture(&source, &artifact, 16 * 1024 * 1024, 1024 * 1024)
                    .await
                    .unwrap();
            let mut snapshot = capture(&source).await.unwrap();
            snapshot.staging = Some(descriptor);
            git(
                &temporary,
                &[
                    "clone",
                    "--quiet",
                    "--bare",
                    source.to_str().unwrap(),
                    "repository.git",
                ],
            )
            .await;
            let mirror = temporary.join("repository.git");
            git(&mirror, &["update-server-info"]).await;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(
                    listener,
                    Router::new()
                        .route("/{*path}", get(serve))
                        .with_state(mirror),
                )
                .await
                .unwrap()
            });
            let credentials = MirrorCredentials {
                workspace_id: "w-fixture".into(),
                repository_url: url.clone(),
                working_tree_url: url,
                username: "fixture".into(),
                password: "fixture".into(),
                read_only: true,
                storage_limit_bytes: 16 * 1024 * 1024,
                max_file_bytes: 1024 * 1024,
            };
            let before = temporary.join("tree-before");
            let checkout = temporary.join("checkout");
            let stage = temporary.join("repository-stage");
            super::super::install::snapshot(
                &destination,
                &before,
                &|path| !path.starts_with(".git"),
                16 * 1024 * 1024,
            )
            .unwrap();
            super::super::install::snapshot(&before, &checkout, &|_| true, 16 * 1024 * 1024)
                .unwrap();
            let actual_index = if linked {
                Some(PathBuf::from(
                    git(
                        &destination,
                        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
                    )
                    .await,
                ))
            } else {
                None
            };
            let original_index = actual_index
                .as_ref()
                .map(|path| std::fs::read(path).unwrap());
            let Prepared {
                branches: _,
                mut writes,
                staging: report,
            } = prepare_receive(
                &destination,
                &checkout,
                &stage,
                Incoming {
                    cache: &temporary.join("incoming.git"),
                    credentials: &credentials,
                    branch: Some("refs/heads/main"),
                    origin: None,
                    snapshot: Some(&snapshot),
                    staging: Some(StagingIncoming {
                        handoff: &artifact,
                        baseline: Some((&base_artifact, &base_descriptor)),
                    }),
                },
                &|| Ok(()),
            )
            .await
            .unwrap();
            if let Some(index) = &actual_index {
                assert_eq!(
                    std::fs::read(index).unwrap(),
                    *original_index.as_ref().unwrap()
                );
            } else {
                assert!(!destination.join(".git").exists());
                std::fs::write(checkout.join("partial"), b"WORKTREE").unwrap();
            }
            let recovery = match report {
                StagingStatus::Conflicts {
                    recovery, paths, ..
                } => {
                    assert!(linked);
                    assert_eq!(paths, ["partial"]);
                    Some(recovery)
                }
                StagingStatus::Synced => {
                    assert!(!linked);
                    None
                }
                StagingStatus::Uncaptured => panic!("captured staging became unknown"),
            };
            writes.extend(
                super::super::install::changes(&destination, &before, &checkout)
                    .unwrap()
                    .into_iter()
                    .filter(|write| !write.relative.starts_with(".git")),
            );
            let binding = super::super::install::Binding {
                endpoint: "https://fixture.invalid".into(),
                account: Some("a-fixture".into()),
                workspace: "w-fixture".into(),
                epoch: 3,
                receipt: Some("fixture-receipt".into()),
            };
            let journal = temporary.join("journal");
            let mut transaction = super::super::install::Transaction::prepare(
                &journal,
                binding.clone(),
                writes,
                16 * 1024 * 1024,
            )
            .unwrap();
            transaction
                .reserve_git(install_roots(&destination).await.unwrap(), &|| Ok(()))
                .unwrap();
            transaction.apply(&|| Ok(())).unwrap();
            drop(transaction);
            let mut resumed = super::super::install::Transaction::open(&journal, &binding)
                .unwrap()
                .unwrap();
            resumed.reserve_git(Vec::new(), &|| Ok(())).unwrap();
            resumed.apply(&|| Ok(())).unwrap();
            resumed.commit(&|| Ok(())).unwrap();
            resumed.cleanup().unwrap();
            assert_eq!(git(&destination, &["show", "HEAD:partial"]).await, "HEAD");
            assert_eq!(
                git(&destination, &["show", ":partial"]).await,
                if linked { "LOCAL INDEX" } else { "INDEX" }
            );
            assert_eq!(git(&destination, &["show", ":only"]).await, "STAGED ONLY");
            assert!(!destination.join("only").exists());
            assert_eq!(
                std::fs::read(destination.join("partial")).unwrap(),
                if linked {
                    b"LOCAL WORKTREE".as_slice()
                } else {
                    b"WORKTREE".as_slice()
                }
            );
            if let Some(recovery) = recovery {
                let path = PathBuf::from(
                    git(
                        &destination,
                        &[
                            "rev-parse",
                            "--path-format=absolute",
                            "--git-path",
                            &recovery,
                        ],
                    )
                    .await,
                );
                assert!(path.join("local.index").is_file());
                assert!(path.join("incoming.index").is_file());
                assert!(!temporary.join("main/.git").join(&recovery).exists());
                git(&destination, &["gc", "--prune=now"]).await;
                let oid = transport::git_output(
                    transport::git(&destination, None).await.unwrap(),
                    &["hash-object", "--stdin"],
                    b"INDEX".to_vec(),
                )
                .await
                .unwrap();
                let oid = std::str::from_utf8(&oid).unwrap().trim();
                assert_eq!(git(&destination, &["cat-file", "blob", oid]).await, "INDEX");
                assert_eq!(
                    git(&destination, &["show", ":partial"]).await,
                    "LOCAL INDEX"
                );
            }
            server.abort();
            std::fs::remove_dir_all(temporary).unwrap();
        }
    }

    async fn git(root: &Path, args: &[&str]) -> String {
        let mut command = transport::git(root, None).await.unwrap();
        command
            .env("GIT_AUTHOR_NAME", "Mirror fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Mirror fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid");
        String::from_utf8(transport::git_output(command, args, vec![]).await.unwrap())
            .unwrap()
            .trim()
            .to_string()
    }
    /// Every event on this thread (the test's current-thread runtime): its
    /// level and message.
    struct Recorder(std::sync::Arc<std::sync::Mutex<Vec<(tracing::Level, String)>>>);
    impl tracing::Subscriber for Recorder {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            let mut message = Message(String::new());
            event.record(&mut message);
            self.0
                .lock()
                .unwrap()
                .push((*event.metadata().level(), message.0));
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// A project folder that is not a Git repository is ordinary: the parts
    /// of a mirror pass that read the folder (the inventory, the repository
    /// step, the history mirror's check) never warn about it, pass after
    /// pass; the log says once that only its working files are copied, and
    /// again only after it has been a repository in between.
    #[tokio::test]
    async fn a_plain_folder_is_said_once_and_never_warned_about() {
        const SAID: &str = "no git repository; only the working files are copied";
        let root = std::env::temp_dir().join(format!(
            "chimaera-plain-folder-{}",
            chimaera_core::generate_token()
        ));
        let project = root.join("project");
        let shadow = root.join("shadow");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("notes.txt"), "plain").unwrap();
        mirror::initialize(&shadow).await.unwrap();
        let pro = super::super::ProState::new(root.join("pro"));
        let credentials = MirrorCredentials {
            workspace_id: "w-plain".into(),
            repository_url: "https://mirror.invalid/repository.git".into(),
            working_tree_url: "https://mirror.invalid/working-tree.git".into(),
            username: "fixture".into(),
            password: "fixture".into(),
            read_only: false,
            storage_limit_bytes: 1 << 20,
            max_file_bytes: 1 << 20,
        };
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let guard = tracing::subscriber::set_default(Recorder(events.clone()));
        let pass = || async {
            let (paths, _) = mirror::inventory(&project, &shadow).await.unwrap();
            assert_eq!(paths, vec![std::path::PathBuf::from("notes.txt")]);
            let described = describe(&pro, "w-plain", &project).await.unwrap();
            assert!(described.branch.is_none());
            assert!(described.origin.is_none());
            assert!(described.snapshot.is_none());
            mirror::mirror_repository(&project, &root.join("repository.git"), &credentials)
                .await
                .unwrap();
        };
        let count = |level: tracing::Level, text: Option<&str>| {
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|(at, message)| *at == level && text.is_none_or(|text| message == text))
                .count()
        };
        pass().await;
        pass().await;
        assert_eq!(
            count(tracing::Level::WARN, None),
            0,
            "{:?}",
            events.lock().unwrap()
        );
        assert_eq!(count(tracing::Level::INFO, Some(SAID)), 1);

        // It becomes a repository (on a detached HEAD, which has no branch):
        // described, and still quiet.
        git(&project, &["init", "-q", "-b", "main"]).await;
        git(&project, &["add", "notes.txt"]).await;
        git(&project, &["commit", "-q", "-m", "notes"]).await;
        git(&project, &["checkout", "-q", "--detach"]).await;
        let described = describe(&pro, "w-plain", &project).await.unwrap();
        assert!(described
            .snapshot
            .is_some_and(|snapshot| snapshot.head.is_some()));
        assert_eq!(described.branch, None);
        git(&project, &["checkout", "-q", "main"]).await;
        let described = describe(&pro, "w-plain", &project).await.unwrap();
        assert_eq!(described.branch.as_deref(), Some("refs/heads/main"));
        assert_eq!(count(tracing::Level::INFO, Some(SAID)), 1);

        // And a plain folder again: said once more, then quiet.
        std::fs::remove_dir_all(project.join(".git")).unwrap();
        pass().await;
        pass().await;
        drop(guard);
        assert_eq!(
            count(tracing::Level::WARN, None),
            0,
            "{:?}",
            events.lock().unwrap()
        );
        assert_eq!(count(tracing::Level::INFO, Some(SAID)), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn portable_config_rejects_credentials_helpers_and_executable_overrides() {
        for (key, value) in [
            ("remote.origin.url", "https://token@example.test/repo"),
            ("remote.origin.url", "file:///private/project"),
            ("remote.origin.uploadpack", "run-something"),
            ("core.hookspath", "/foreign/hooks"),
            ("credential.helper", "external-helper"),
            ("include.path", "/private/config"),
            ("remote.origin.fetch", "+refs/heads/*:refs/../../bad"),
        ] {
            assert!(
                !allowed(&Entry {
                    key: key.into(),
                    value: value.into()
                }),
                "{key}"
            );
        }
        for url in [
            "https://github.com/owner/project.git",
            "git@example.test:owner/project.git",
            "ssh://dev@example.test:2222/owner/project.git",
        ] {
            assert!(safe_url(url), "{url}");
        }
    }
    #[tokio::test]
    async fn untracked_files_collide_with_a_return_only_when_they_differ() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-untracked-return-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet", "--initial-branch=main"]).await;
        std::fs::write(root.join("kept.txt"), "base").unwrap();
        git(&root, &["add", "kept.txt"]).await;
        git(&root, &["commit", "-qm", "base"]).await;
        let previous = git(&root, &["rev-parse", "HEAD"]).await;
        std::fs::write(root.join("created.txt"), "same in both").unwrap();
        git(&root, &["add", "created.txt"]).await;
        git(&root, &["commit", "-qm", "cloud"]).await;
        let incoming = git(&root, &["rev-parse", "HEAD"]).await;
        git(&root, &["reset", "--quiet", "--hard", &previous]).await;
        // The same file created on both sides: adopted around the fast-forward,
        // and put back when the fast-forward does not happen.
        std::fs::write(root.join("created.txt"), "same in both").unwrap();
        assert!(
            !adopt_identical_untracked(&root, &previous, &incoming, async {
                assert!(!root.join("created.txt").exists(), "set aside for checkout");
                Ok(false)
            })
            .await
            .unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(root.join("created.txt")).unwrap(),
            "same in both"
        );
        // A different local file keeps the cloud's branch separate instead.
        std::fs::write(root.join("created.txt"), "local version").unwrap();
        assert!(
            !adopt_identical_untracked(&root, &previous, &incoming, async {
                panic!("a differing untracked file must not be fast-forwarded over")
            })
            .await
            .unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(root.join("created.txt")).unwrap(),
            "local version"
        );
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(crate::persist::PROJECT_STAGING_PREFIX)
        }));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn prepared_adoption_blocks_checkout_and_cancellation_releases_our_locks() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-ref-lock-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet", "--initial-branch=main"]).await;
        std::fs::write(root.join("file.txt"), "local").unwrap();
        git(&root, &["add", "."]).await;
        git(&root, &["commit", "-qm", "local"]).await;
        let old = git(&root, &["rev-parse", "HEAD"]).await;
        git(&root, &["switch", "--quiet", "-c", "cloud"]).await;
        std::fs::write(root.join("file.txt"), "cloud").unwrap();
        git(&root, &["add", "."]).await;
        git(&root, &["commit", "-qm", "cloud"]).await;
        let incoming = git(&root, &["rev-parse", "HEAD"]).await;
        git(&root, &["switch", "--quiet", "main"]).await;
        let index_lock = root.join(".git/index.lock");
        std::fs::copy(root.join(".git/index"), &index_lock).unwrap();
        let mut transaction = RefTransaction::begin(
            &root,
            IndexReservation {
                path: Some(index_lock.clone()),
                preserve: false,
            },
        )
        .await
        .unwrap();
        transaction
            .send(&format!(
                "start\nupdate refs/heads/main {incoming} {old}\nprepare\n"
            ))
            .await
            .unwrap();
        transaction.confirm("start: ok").await.unwrap();
        transaction.confirm("prepare: ok").await.unwrap();
        let mut checkout = transport::git(&root, None).await.unwrap();
        checkout.args(["switch", "cloud"]);
        assert!(
            !transport::run(checkout, vec![], Duration::from_secs(5), 4096)
                .await
                .unwrap()
                .success
        );
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "local"
        );
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).await, old);
        std::fs::write(
            root.join(".git/index.lock.lock"),
            "cancelled alternate index",
        )
        .unwrap();
        drop(transaction);
        for _ in 0..100 {
            if !index_lock.exists() && !root.join(".git/HEAD.lock").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!index_lock.exists());
        assert!(!root.join(".git/index.lock.lock").exists());
        assert!(!root.join(".git/HEAD.lock").exists());
        assert!(!root.join(".git/refs/heads/main.lock").exists());
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).await, old);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn cancellation_after_ref_commit_finishes_index_adoption() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-ref-finalize-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet", "--initial-branch=main"]).await;
        std::fs::write(root.join("local.txt"), "local").unwrap();
        git(&root, &["add", "."]).await;
        git(&root, &["commit", "-qm", "local"]).await;
        let previous = git(&root, &["rev-parse", "HEAD"]).await;
        git(&root, &["switch", "--quiet", "-c", "cloud"]).await;
        std::fs::write(root.join("cloud.txt"), "cloud").unwrap();
        git(&root, &["add", "."]).await;
        git(&root, &["commit", "-qm", "cloud"]).await;
        let incoming = git(&root, &["rev-parse", "HEAD"]).await;
        git(&root, &["switch", "--quiet", "main"]).await;
        let index = root.join(".git/index");
        let index_lock = root.join(".git/index.lock");
        std::fs::copy(&index, &index_lock).unwrap();
        let mut transaction = RefTransaction::begin(
            &root,
            IndexReservation {
                path: Some(index_lock.clone()),
                preserve: false,
            },
        )
        .await
        .unwrap();
        transaction
            .send(&format!(
                "start\nupdate refs/heads/main {incoming} {previous}\nprepare\n"
            ))
            .await
            .unwrap();
        transaction.confirm("start: ok").await.unwrap();
        transaction.confirm("prepare: ok").await.unwrap();
        let (committed, reached_commit) = tokio::sync::oneshot::channel();
        let (proceed, finish) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(finalize_current(
            root.clone(),
            index,
            "refs/heads/main".into(),
            previous,
            incoming.clone(),
            transaction,
            Some(FinalizationPause {
                committed,
                proceed: finish,
            }),
        ));
        // The finalizer's helper shares the process-wide two-slot Git budget
        // with every concurrently running test; only reaching the commit
        // matters here, not how fast.
        tokio::time::timeout(Duration::from_secs(60), reached_commit)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).await, incoming);
        assert!(!git(&root, &["diff", "--cached", "--name-only"])
            .await
            .is_empty());
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        proceed.send(()).unwrap();
        for _ in 0..100 {
            if !index_lock.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!index_lock.exists());
        assert!(git(&root, &["status", "--porcelain"]).await.is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("cloud.txt")).unwrap(),
            "cloud"
        );
        assert!(!root.join(".git/HEAD.lock").exists());
        assert!(!root.join(".git/refs/heads/main.lock").exists());
        // Finalizer cleanup must never unlink a later Git operation's lock.
        std::fs::write(&index_lock, "next Git operation").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            std::fs::read_to_string(&index_lock).unwrap(),
            "next Git operation"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn return_advances_all_safe_branches_preserves_divergence_and_keeps_remote_config() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-repository-{}",
            chimaera_core::generate_token()
        ));
        let laptop = root.join("laptop");
        let cloud = root.join("cloud");
        std::fs::create_dir_all(&laptop).unwrap();
        git(&laptop, &["init", "--quiet", "--initial-branch=main"]).await;
        std::fs::write(laptop.join("base.txt"), "base").unwrap();
        git(&laptop, &["add", "."]).await;
        git(&laptop, &["commit", "-qm", "base"]).await;
        for branch in ["feature", "diverged", "checked-elsewhere"] {
            git(&laptop, &["branch", branch]).await;
        }
        git(
            &laptop,
            &[
                "remote",
                "add",
                "origin",
                "https://example.test/team/project.git",
            ],
        )
        .await;
        git(
            &laptop,
            &[
                "remote",
                "add",
                "upstream",
                "git@example.test:upstream/project.git",
            ],
        )
        .await;
        git(&laptop, &["config", "branch.main.remote", "origin"]).await;
        git(&laptop, &["config", "branch.main.merge", "refs/heads/main"]).await;
        git(
            &laptop,
            &["config", "credential.helper", "MUST_NOT_TRANSFER"],
        )
        .await;
        let snapshot = capture(&laptop).await.unwrap();
        assert!(snapshot
            .config
            .iter()
            .any(|entry| entry.key == "remote.upstream.url"));
        assert!(snapshot
            .config
            .iter()
            .all(|entry| entry.key != "credential.helper"));
        git(
            &root,
            &[
                "clone",
                "--quiet",
                laptop.to_str().unwrap(),
                cloud.to_str().unwrap(),
            ],
        )
        .await;
        for branch in ["feature", "diverged", "checked-elsewhere"] {
            git(&cloud, &["switch", "--quiet", branch]).await;
            std::fs::write(cloud.join(format!("{branch}.txt")), "cloud").unwrap();
            git(&cloud, &["add", "."]).await;
            git(&cloud, &["commit", "-qm", "cloud change"]).await;
        }
        git(&cloud, &["switch", "--quiet", "main"]).await;
        std::fs::write(cloud.join("main-cloud.txt"), "main advanced").unwrap();
        git(&cloud, &["add", "."]).await;
        git(&cloud, &["commit", "-qm", "main advanced"]).await;
        git(&cloud, &["switch", "--quiet", "-c", "cloud-created"]).await;
        std::fs::write(cloud.join("new.txt"), "new branch").unwrap();
        git(&cloud, &["add", "."]).await;
        git(&cloud, &["commit", "-qm", "new branch"]).await;
        git(&laptop, &["switch", "--quiet", "diverged"]).await;
        std::fs::write(laptop.join("local.txt"), "local").unwrap();
        git(&laptop, &["add", "."]).await;
        git(&laptop, &["commit", "-qm", "local change"]).await;
        let local_tip = git(&laptop, &["rev-parse", "diverged"]).await;
        git(&laptop, &["switch", "--quiet", "main"]).await;
        let linked = root.join("linked");
        git(
            &laptop,
            &[
                "worktree",
                "add",
                "--quiet",
                linked.to_str().unwrap(),
                "checked-elsewhere",
            ],
        )
        .await;
        let linked_tip = git(&laptop, &["rev-parse", "checked-elsewhere"]).await;
        let namespace = "refs/chimaera-transfer/test/";
        git(
            &laptop,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                cloud.to_str().unwrap(),
                "+refs/heads/*:refs/chimaera-transfer/test/heads/*",
            ],
        )
        .await;
        git(
            &laptop,
            &[
                "update-ref",
                &format!("{namespace}chimaera/staging/fixture"),
                "HEAD^{tree}",
            ],
        )
        .await;
        std::fs::write(laptop.join(".git/FETCH_HEAD"), "user fetch marker\n").unwrap();
        let preserved = publish_refs(&laptop, namespace, true, &|| Ok(()), None)
            .await
            .unwrap();
        assert!(optional(
            &laptop,
            &["rev-parse", "--verify", "refs/chimaera/staging/fixture"]
        )
        .await
        .unwrap()
        .is_none());
        assert_eq!(
            std::fs::read_to_string(laptop.join(".git/FETCH_HEAD")).unwrap(),
            "user fetch marker\n"
        );
        assert_eq!(
            git(&laptop, &["rev-parse", "feature"]).await,
            git(&cloud, &["rev-parse", "feature"]).await
        );
        assert_eq!(
            git(&laptop, &["rev-parse", "cloud-created"]).await,
            git(&cloud, &["rev-parse", "cloud-created"]).await
        );
        assert_eq!(git(&laptop, &["rev-parse", "diverged"]).await, local_tip);
        assert_eq!(
            git(&laptop, &["rev-parse", "checked-elsewhere"]).await,
            linked_tip
        );
        assert!(preserved
            .iter()
            .any(|branch| branch.starts_with("diverged@cloud-")));
        assert!(preserved
            .iter()
            .any(|branch| branch.starts_with("checked-elsewhere@cloud-")));
        assert_eq!(
            git(&laptop, &["rev-parse", "main"]).await,
            git(&cloud, &["rev-parse", "main"]).await
        );
        assert_eq!(
            std::fs::read_to_string(laptop.join("main-cloud.txt")).unwrap(),
            "main advanced"
        );
        assert!(git(&laptop, &["status", "--porcelain"]).await.is_empty());
        let fresh = root.join("fresh");
        std::fs::create_dir(&fresh).unwrap();
        git(&fresh, &["init", "--quiet"]).await;
        install_config(&fresh, &snapshot.config, &|| Ok(()))
            .await
            .unwrap();
        assert_eq!(
            git(&fresh, &["config", "remote.upstream.url"]).await,
            "git@example.test:upstream/project.git"
        );
        assert_eq!(
            git(&fresh, &["config", "branch.main.merge"]).await,
            "refs/heads/main"
        );
        git(
            &fresh,
            &[
                "config",
                "remote.origin.url",
                "https://local.example/project",
            ],
        )
        .await;
        install_config(&fresh, &snapshot.config, &|| Ok(()))
            .await
            .unwrap();
        assert_eq!(
            git(&fresh, &["config", "remote.origin.url"]).await,
            "https://local.example/project"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
