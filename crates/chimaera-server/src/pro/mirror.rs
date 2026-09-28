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
            transport::git(path, None).await?,
            &["init", "--bare", "--quiet", "."],
            vec![],
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn inventory(root: &Path, shadow: &Path) -> Result<Vec<PathBuf>> {
    let mut command = transport::git(root, None).await?;
    command.args(["rev-parse", "--is-inside-work-tree"]);
    let repo = transport::run(command, vec![], std::time::Duration::from_secs(5), 256)
        .await?
        .success;
    let mut command = transport::git(root, None).await?;
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
            let mut command = transport::git(root, None).await?;
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
        let mut command = transport::git(root, None).await?;
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
        let command = async || {
            let mut command = transport::git(repository, None).await?;
            command
                .env("GIT_DIR", repository)
                .env("GIT_WORK_TREE", tree)
                .env("GIT_INDEX_FILE", &index)
                .env("GIT_AUTHOR_NAME", "chimaera")
                .env("GIT_AUTHOR_EMAIL", "mirror@localhost")
                .env("GIT_COMMITTER_NAME", "chimaera")
                .env("GIT_COMMITTER_EMAIL", "mirror@localhost");
            Ok::<_, anyhow::Error>(command)
        };
        transport::git_output(command().await?, &["read-tree", "--empty"], vec![]).await?;
        transport::git_output(
            command().await?,
            &["add", "--all", "--force", "--", "."],
            vec![],
        )
        .await?;
        let tree = String::from_utf8(
            transport::git_output(command().await?, &["write-tree"], vec![]).await?,
        )?
        .trim()
        .to_string();
        let reference = format!("refs/heads/{branch}");
        let mut lookup = command().await?;
        lookup.args(["rev-parse", "--verify", &reference]);
        let old = transport::run(lookup, vec![], std::time::Duration::from_secs(5), 128).await?;
        let parent = if old.success {
            Some(String::from_utf8(old.stdout)?.trim().to_string())
        } else {
            None
        };
        if let Some(parent) = &parent {
            let old_tree = transport::git_output(
                command().await?,
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
            transport::git_output(command().await?, &args, b"Workspace mirror\n".to_vec()).await?,
        )?
        .trim()
        .to_string();
        transport::git_output(
            command().await?,
            &["update-ref", &reference, &commit],
            vec![],
        )
        .await?;
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
        )
        .await?,
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
    let mut check = transport::git(root, None).await?;
    check.args(["rev-parse", "--git-dir"]);
    if !transport::run(check, vec![], std::time::Duration::from_secs(5), 4096)
        .await?
        .success
    {
        return Ok(());
    }
    initialize(cache).await?;
    transport::git_output(
        transport::git(cache, None).await?,
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
    super::repository::keep_source_head(root, cache).await?;
    let objects = transport::git_output(
        transport::git(cache, None).await?,
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
        transport::git(cache, Some((&credentials.username, &credentials.password))).await?,
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
        transport::git(repository, None).await?,
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

pub(super) async fn repository_origin(root: &Path) -> Option<String> {
    let bytes = transport::git_output(
        transport::git(root, None).await.ok()?,
        &["config", "--get", "remote.origin.url"],
        vec![],
    )
    .await
    .ok()?;
    let origin = String::from_utf8(bytes).ok()?.trim().to_string();
    super::repository::safe_url(&origin).then_some(origin)
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

    #[tokio::test]
    async fn snapshots_exclude_in_progress_writes_and_include_completed_rename() {
        for repository in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "chimaera-mirror-staging-{}",
                chimaera_core::generate_token()
            ));
            let project = root.join("project");
            let shadow = root.join("shadow");
            fs::create_dir_all(&project).unwrap();
            initialize(&shadow).await.unwrap();
            if repository {
                transport::git_output(
                    transport::git(&project, None).await.unwrap(),
                    &["init", "--quiet", "."],
                    vec![],
                )
                .await
                .unwrap();
            }
            let target = project.join("notes.md");
            fs::write(&target, "previous complete version").unwrap();
            fs::write(project.join(".notes.backup.tmp"), "user backup").unwrap();
            fs::write(project.join(".chimaeraignore"), "!.chimaera-staging-*\n").unwrap();
            let staged_name = crate::persist::project_temp_name(target.file_name().unwrap());
            let staged = project.join(&staged_name);
            let mut upload = fs::File::create(&staged).unwrap();
            upload.write_all(b"unfinished").unwrap();
            upload.flush().unwrap();
            if repository {
                // A mistakenly tracked temporary file is still never transferable.
                transport::git_output(
                    transport::git(&project, None).await.unwrap(),
                    &["add", "--force", staged_name.to_str().unwrap()],
                    vec![],
                )
                .await
                .unwrap();
            }
            let paths = inventory(&project, &shadow).await.unwrap();
            assert!(paths.iter().any(|path| path == Path::new(&staged_name)));
            let first = root.join("first");
            copy_tree(&project, &first, paths, 10000, 1000).unwrap();
            assert!(!first.join(&staged_name).exists());
            assert_eq!(
                fs::read(first.join("notes.md")).unwrap(),
                b"previous complete version"
            );
            assert!(first.join(".notes.backup.tmp").exists());
            upload.write_all(b" now complete").unwrap();
            upload.sync_all().unwrap();
            drop(upload);
            fs::rename(&staged, &target).unwrap();
            let paths = inventory(&project, &shadow).await.unwrap();
            let second = root.join("second");
            copy_tree(&project, &second, paths, 10000, 1000).unwrap();
            assert_eq!(
                fs::read(second.join("notes.md")).unwrap(),
                b"unfinished now complete"
            );
            assert!(!second.join(&staged_name).exists());
            fs::remove_dir_all(root).unwrap();
        }
    }
}
