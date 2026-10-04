//! Host admission for optional repository policy, plus free branch recovery.
pub(super) use super::transfer_types::{Described, Prepared, Snapshot, StagingStatus};
use super::{protocol::MirrorCredentials, transfer_dispatch as dispatch, transport};
use anyhow::{bail, ensure, Context, Result};
use dispatch::{TransferReply as Reply, TransferRequest as Request};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) mod staging;
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
pub(super) async fn describe(
    pro: &super::ProState,
    workspace: &str,
    root: &Path,
) -> Result<Described> {
    ensure!(
        dispatch::owner()?.host.belongs_to(pro, workspace),
        "Transfer project changed"
    );
    match dispatch::call(Request::Describe { root }).await? {
        Reply::Described(answer) => Ok(answer),
        _ => bail!("optional transfer runtime returned an invalid result"),
    }
}
pub(super) async fn prepare_receive(
    original: &Path,
    checkout: &Path,
    stage: &Path,
    incoming: Incoming<'_>,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<Prepared> {
    let owner = dispatch::owner()?;
    owner.host.current()?;
    let grant = owner.host.grant(incoming.credentials.clone())?;
    let incoming = super::transfer_types::Incoming {
        cache: incoming.cache,
        credentials: &grant,
        branch: incoming.branch,
        origin: incoming.origin,
        snapshot: incoming.snapshot,
        staging: incoming
            .staging
            .map(|staging| super::transfer_types::StagingIncoming {
                handoff: staging.handoff,
                baseline: staging.baseline,
            }),
    };
    match dispatch::call(Request::PrepareReceive {
        original,
        checkout,
        stage,
        incoming,
        check,
    })
    .await?
    {
        Reply::Prepared(answer) => Ok(answer),
        _ => bail!("optional transfer runtime returned an invalid result"),
    }
}
async fn project_git(root: &Path) -> Result<tokio::process::Command> {
    transport::git(root, None).await
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
pub(super) fn cloud_branch_name(branch: &str) -> bool {
    branch.rsplit_once("@cloud-").is_some_and(|(base, commit)| {
        !base.is_empty()
            && commit.len() == 12
            && commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
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
