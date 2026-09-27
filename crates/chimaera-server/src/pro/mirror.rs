//! Separate repositories hold snapshots. User indexes, branches and remotes are
//! never used for snapshot commits; all network Git runs from our clean cache.
use super::{policy, protocol::MirrorCredentials, transport};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Default, Serialize)]
pub(super) struct Report {
    pub files: usize,
    pub bytes: u64,
    pub excluded: usize,
    pub too_large: usize,
}

pub(super) async fn initialize(path: &Path) -> Result<()> {
    tokio::fs::create_dir_all(path).await?;
    if !tokio::fs::try_exists(path.join("HEAD")).await? {
        transport::git_output(
            transport::git(path, None),
            &["init", "--bare", "--quiet", "."],
            vec![],
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn inventory(root: &Path, shadow: &Path) -> Result<Vec<PathBuf>> {
    let mut command = transport::git(root, None);
    command.args(["rev-parse", "--is-inside-work-tree"]);
    let repo = transport::run(command, vec![], std::time::Duration::from_secs(5), 256)
        .await?
        .success;
    let mut command = transport::git(root, None);
    if !repo {
        command.env("GIT_DIR", shadow).env("GIT_WORK_TREE", root);
    }
    let bytes = transport::git_output(
        command,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--exclude-from=.chimaeraignore",
        ],
        vec![],
    )
    .await;
    // --exclude-from refuses a missing file, so add it only when present.
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) if !tokio::fs::try_exists(root.join(".chimaeraignore")).await? => {
            let mut command = transport::git(root, None);
            if !repo {
                command.env("GIT_DIR", shadow).env("GIT_WORK_TREE", root);
            }
            transport::git_output(
                command,
                &[
                    "ls-files",
                    "-z",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                ],
                vec![],
            )
            .await
            .map_err(|_| error)?
        }
        Err(error) => return Err(error),
    };
    let paths: BTreeSet<PathBuf> = bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| std::str::from_utf8(p).map(PathBuf::from))
        .collect::<std::result::Result<_, _>>()?;
    ensure!(
        paths.len() <= policy::MAX_PATHS,
        "workspace contains too many mirror paths"
    );
    // check-ignore --no-index also applies the privacy file to tracked files.
    let mut ignored = BTreeSet::new();
    if tokio::fs::try_exists(root.join(".chimaeraignore")).await? {
        let mut input = Vec::new();
        for path in &paths {
            input.extend_from_slice(path.to_str().context("invalid project path")?.as_bytes());
            input.push(0);
        }
        let mut command = transport::git(root, None);
        if !repo {
            command.env("GIT_DIR", shadow).env("GIT_WORK_TREE", root);
        }
        command.args([
            "-c",
            "core.excludesFile=.chimaeraignore",
            "check-ignore",
            "--no-index",
            "-z",
            "--stdin",
        ]);
        let output = transport::run(
            command,
            input,
            std::time::Duration::from_secs(10),
            transport::PATH_CAP,
        )
        .await?;
        for p in output.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            ignored.insert(PathBuf::from(std::str::from_utf8(p)?));
        }
    }
    Ok(paths
        .into_iter()
        .filter(|path| !ignored.contains(path))
        .collect())
}

pub(super) fn copy_tree(
    root: &Path,
    destination: &Path,
    paths: Vec<PathBuf>,
    budget: u64,
    max_file: u64,
) -> Result<Report> {
    fs::create_dir_all(destination)?;
    let mut report = Report::default();
    for relative in paths {
        if !policy::allowed_path(&relative) {
            report.excluded += 1;
            continue;
        }
        let source = root.join(&relative);
        // Every ancestor is checked: a directory swapped to a symlink cannot
        // turn an allowlisted project path into a home credential read.
        let mut ancestor = root.to_path_buf();
        let mut safe = true;
        for part in relative.components() {
            ancestor.push(part);
            if fs::symlink_metadata(&ancestor).is_ok_and(|m| m.file_type().is_symlink()) {
                safe = false;
                break;
            }
        }
        if !safe {
            report.excluded += 1;
            continue;
        }
        let metadata = match fs::symlink_metadata(&source) {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };
        if metadata.len() > max_file.min(policy::MAX_FILE_BYTES) {
            report.too_large += 1;
            continue;
        }
        ensure!(
            report.bytes.saturating_add(metadata.len()) <= budget,
            "workspace exceeds mirror storage quota"
        );
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            );
        }
        let mut input = options.open(source)?;
        ensure!(
            input.metadata()?.is_file(),
            "project file changed during mirror"
        );
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&target)?;
        let mut chunk = [0u8; 65536];
        let mut tail = Vec::new();
        let mut length = 0u64;
        let mut secret = false;
        loop {
            let count = input.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            length += count as u64;
            ensure!(
                length <= max_file.min(policy::MAX_FILE_BYTES)
                    && report.bytes.saturating_add(length) <= budget,
                "project grew beyond mirror limits"
            );
            tail.extend_from_slice(&chunk[..count]);
            if policy::contains_credential(&tail) {
                secret = true;
                break;
            }
            output.write_all(&chunk[..count])?;
            if tail.len() > 128 {
                tail.drain(..tail.len() - 128);
            }
        }
        drop(output);
        if secret {
            fs::remove_file(target)?;
            report.excluded += 1;
            continue;
        }
        fs::set_permissions(&target, metadata.permissions())?;
        report.bytes += length;
        report.files += 1;
    }
    Ok(report)
}

pub(super) async fn commit_tree(repository: &Path, tree: &Path, branch: &str) -> Result<String> {
    ensure!(
        matches!(branch, "main" | "config" | "handoff"),
        "invalid mirror branch"
    );
    let index = repository.join(format!("index-{}", chimaera_core::generate_token()));
    let result = async {
        let command = || {
            let mut command = transport::git(repository, None);
            command
                .env("GIT_DIR", repository)
                .env("GIT_WORK_TREE", tree)
                .env("GIT_INDEX_FILE", &index)
                .env("GIT_AUTHOR_NAME", "chimaera")
                .env("GIT_AUTHOR_EMAIL", "mirror@localhost")
                .env("GIT_COMMITTER_NAME", "chimaera")
                .env("GIT_COMMITTER_EMAIL", "mirror@localhost");
            command
        };
        transport::git_output(command(), &["read-tree", "--empty"], vec![]).await?;
        transport::git_output(command(), &["add", "--all", "--force", "--", "."], vec![]).await?;
        let tree =
            String::from_utf8(transport::git_output(command(), &["write-tree"], vec![]).await?)?
                .trim()
                .to_string();
        let reference = format!("refs/heads/{branch}");
        let mut lookup = command();
        lookup.args(["rev-parse", "--verify", &reference]);
        let old = transport::run(lookup, vec![], std::time::Duration::from_secs(5), 128).await?;
        let parent = if old.success {
            Some(String::from_utf8(old.stdout)?.trim().to_string())
        } else {
            None
        };
        if let Some(parent) = &parent {
            let old_tree = transport::git_output(
                command(),
                &["rev-parse", &format!("{parent}^{{tree}}")],
                vec![],
            )
            .await?;
            if old_tree == format!("{tree}\n").as_bytes() {
                return Ok(parent.clone());
            }
        }
        let mut args = vec!["commit-tree", &tree];
        if let Some(parent) = &parent {
            args.extend(["-p", parent]);
        }
        let commit = String::from_utf8(
            transport::git_output(command(), &args, b"Workspace mirror\n".to_vec()).await?,
        )?
        .trim()
        .to_string();
        transport::git_output(command(), &["update-ref", &reference, &commit], vec![]).await?;
        Ok::<_, anyhow::Error>(commit)
    }
    .await;
    let _ = tokio::fs::remove_file(index).await;
    result
}

pub(super) async fn push(
    repository: &Path,
    credentials: &MirrorCredentials,
    branches: &[&str],
) -> Result<()> {
    ensure!(!credentials.read_only, "mirror credential is read-only");
    let url = transport::endpoint(&credentials.working_tree_url)?;
    ensure!(
        !credentials.username.contains(['\n', '\r', '\0'])
            && !credentials.password.contains(['\n', '\r', '\0']),
        "invalid mirror credentials"
    );
    let mut args = vec!["push", "--atomic", &url];
    args.extend_from_slice(branches);
    transport::git_output(
        transport::git(
            repository,
            Some((&credentials.username, &credentials.password)),
        ),
        &args,
        vec![],
    )
    .await?;
    Ok(())
}

pub(super) async fn mirror_repository(
    root: &Path,
    cache: &Path,
    credentials: &MirrorCredentials,
) -> Result<()> {
    let mut check = transport::git(root, None);
    check.args(["rev-parse", "--git-dir"]);
    if !transport::run(check, vec![], std::time::Duration::from_secs(5), 4096)
        .await?
        .success
    {
        return Ok(());
    }
    initialize(cache).await?;
    transport::git_output(
        transport::git(cache, None),
        &[
            "fetch",
            "--prune",
            "--no-tags",
            root.to_str().context("invalid workspace path")?,
            "+refs/*:refs/*",
        ],
        vec![],
    )
    .await?;
    let objects = transport::git_output(
        transport::git(cache, None),
        &["rev-list", "--objects", "--all"],
        vec![],
    )
    .await?;
    for line in objects.split(|b| *b == b'\n') {
        if let Some(index) = line.iter().position(|b| *b == b' ') {
            let path = &line[index + 1..];
            ensure!(
                policy::allowed_path(Path::new(std::str::from_utf8(path)?)),
                "repository history contains a credential path; mirror withheld"
            );
        }
    }
    let url = transport::endpoint(&credentials.repository_url)?;
    transport::git_output(
        transport::git(cache, Some((&credentials.username, &credentials.password))),
        &["push", "--mirror", &url],
        vec![],
    )
    .await?;
    Ok(())
}

/// Validate the complete tree before checkout creates any remote-controlled
/// paths. The server's quota is repeated locally, including aggregate bytes.
pub(super) async fn validate_tree(
    repository: &Path,
    branch: &str,
    budget: u64,
    max_file: u64,
) -> Result<()> {
    let bytes = transport::git_output(
        transport::git(repository, None),
        &["ls-tree", "-r", "-z", "-l", branch],
        vec![],
    )
    .await?;
    let mut count = 0;
    let mut total = 0u64;
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        count += 1;
        ensure!(
            count <= policy::MAX_PATHS,
            "remote mirror contains too many files"
        );
        let split = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("invalid remote tree")?;
        let header = std::str::from_utf8(&record[..split])?;
        let mut fields = header.split_whitespace();
        let mode = fields.next().context("missing tree mode")?;
        ensure!(
            matches!(mode, "100644" | "100755"),
            "remote mirror contains a link or special file"
        );
        ensure!(
            fields.next() == Some("blob"),
            "invalid remote mirror object"
        );
        let _object = fields.next();
        let size = fields
            .next()
            .context("missing object size")?
            .parse::<u64>()?;
        total = total
            .checked_add(size)
            .context("remote mirror size overflow")?;
        ensure!(
            size <= max_file.min(policy::MAX_FILE_BYTES) && total <= budget,
            "remote mirror exceeds storage limits"
        );
        ensure!(
            policy::allowed_path(Path::new(std::str::from_utf8(&record[split + 1..])?)),
            "remote mirror contains a credential or unsafe path"
        );
    }
    Ok(())
}

pub(super) async fn receive_repository(
    root: &Path,
    cache: &Path,
    credentials: &MirrorCredentials,
    branch: Option<&str>,
    origin: Option<&str>,
) -> Result<()> {
    let Some(branch) = branch else {
        return Ok(());
    };
    ensure!(
        branch.starts_with("refs/heads/")
            && !branch.contains(['\n', '\r', ' ', ':', '~', '^', '?', '*', '[', '\\'])
            && !branch.contains(".."),
        "invalid project branch"
    );
    initialize(cache).await?;
    let url = transport::endpoint(&credentials.repository_url)?;
    transport::git_output(
        transport::git(cache, Some((&credentials.username, &credentials.password))),
        &["fetch", "--prune", "--no-tags", &url, "+refs/*:refs/*"],
        vec![],
    )
    .await?;
    let mut detect = transport::git(root, None);
    detect.args(["rev-parse", "--git-dir"]);
    let existing = transport::run(detect, vec![], std::time::Duration::from_secs(5), 4096)
        .await?
        .success;
    if !existing {
        transport::git_output(
            transport::git(root, None),
            &["init", "--quiet", "."],
            vec![],
        )
        .await?;
        transport::git_output(
            transport::git(root, None),
            &[
                "fetch",
                "--no-tags",
                cache.to_str().context("invalid repository cache")?,
                "+refs/*:refs/*",
            ],
            vec![],
        )
        .await?;
        transport::git_output(
            transport::git(root, None),
            &["symbolic-ref", "HEAD", branch],
            vec![],
        )
        .await?;
        transport::git_output(transport::git(root, None), &["read-tree", branch], vec![]).await?;
        if let Some(origin) = origin.filter(|origin| safe_origin(origin)) {
            transport::git_output(
                transport::git(root, None),
                &["remote", "add", "origin", origin],
                vec![],
            )
            .await?;
        }
    } else {
        let remote = format!(
            "refs/remotes/chimaera-cloud/{}",
            branch.trim_start_matches("refs/heads/")
        );
        transport::git_output(
            transport::git(root, None),
            &[
                "fetch",
                "--no-tags",
                cache.to_str().context("invalid repository cache")?,
                &format!("+{branch}:{remote}"),
            ],
            vec![],
        )
        .await?;
        let status = transport::git_output(
            transport::git(root, None),
            &["status", "--porcelain", "--untracked-files=no"],
            vec![],
        )
        .await?;
        let head = transport::git_output(
            transport::git(root, None),
            &["symbolic-ref", "-q", "HEAD"],
            vec![],
        )
        .await
        .unwrap_or_default();
        let mut ancestor = transport::git(root, None);
        ancestor.args(["merge-base", "--is-ancestor", branch, &remote]);
        if status.is_empty()
            && head == format!("{branch}\n").as_bytes()
            && transport::run(ancestor, vec![], std::time::Duration::from_secs(5), 256)
                .await?
                .success
        {
            transport::git_output(
                transport::git(root, None),
                &["merge", "--ff-only", &remote],
                vec![],
            )
            .await?;
        } else {
            let target = String::from_utf8(
                transport::git_output(transport::git(root, None), &["rev-parse", &remote], vec![])
                    .await?,
            )?;
            let preserved = format!("{branch}@cloud-{}", super::now());
            transport::git_output(
                transport::git(root, None),
                &[
                    "update-ref",
                    &preserved,
                    target.trim(),
                    "0000000000000000000000000000000000000000",
                ],
                vec![],
            )
            .await?;
        }
    }
    Ok(())
}

fn safe_origin(value: &str) -> bool {
    if value.len() > 2048
        || value.chars().any(char::is_control)
        || policy::contains_credential(value.as_bytes())
    {
        return false;
    }
    if let Ok(uri) = value.parse::<axum::http::Uri>() {
        if uri.scheme_str() == Some("https")
            && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
            && uri.query().is_none()
            && !value.contains('#')
        {
            return true;
        }
    }
    value
        .strip_prefix("git@")
        .and_then(|v| v.split_once(':'))
        .is_some_and(|(host, path)| {
            !host.is_empty()
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
                && !path.is_empty()
                && path
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
                && !path.contains("..")
        })
}
pub(super) async fn repository_origin(root: &Path) -> Option<String> {
    let bytes = transport::git_output(
        transport::git(root, None),
        &["config", "--get", "remote.origin.url"],
        vec![],
    )
    .await
    .ok()?;
    let origin = String::from_utf8(bytes).ok()?.trim().to_string();
    safe_origin(&origin).then_some(origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn snapshots_preserve_user_history_and_filter_private_paths() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-mirror-{}",
            chimaera_core::generate_token()
        ));
        let project = root.join("project");
        let shadow = root.join("shadow");
        let stage = root.join("stage");
        fs::create_dir_all(&project).unwrap();
        initialize(&shadow).await.unwrap();
        fs::write(project.join("safe.txt"), "hello").unwrap();
        fs::write(project.join(".env"), "SECRET=yes").unwrap();
        fs::write(project.join("ignored.txt"), "private").unwrap();
        fs::write(project.join(".chimaeraignore"), "ignored.txt\n").unwrap();
        let paths = inventory(&project, &shadow).await.unwrap();
        let report = copy_tree(&project, &stage, paths, 10000, 1000).unwrap();
        assert_eq!(report.files, 2);
        assert!(!stage.join(".env").exists());
        assert!(!stage.join("ignored.txt").exists());
        let first = commit_tree(&shadow, &stage, "main").await.unwrap();
        assert_eq!(commit_tree(&shadow, &stage, "main").await.unwrap(), first);
        assert!(!project.join(".git").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
