//! Separate repositories hold snapshots. User indexes, branches and remotes are
//! never used for snapshot commits; all network Git runs from our clean cache.
use super::{policy, protocol::MirrorCredentials, transport};
use anyhow::{ensure, Context, Result};
use rustix::fs::OFlags;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Default, Serialize)]
pub(super) struct Report {
    pub files: usize,
    pub bytes: u64,
    pub excluded: usize,
    pub too_large: usize,
    /// Paths present in the project that this snapshot left out. A receiver
    /// deletes nothing on their account; `None` once the list would exceed
    /// its bound, which makes every absence ambiguous.
    #[serde(skip)]
    pub left_out: Option<Vec<PathBuf>>,
}
const LEFT_OUT_LIMIT: usize = 4096;
impl Report {
    fn leave_out(&mut self, path: &Path) {
        if let Some(list) = self.left_out.as_mut() {
            if list.len() >= LEFT_OUT_LIMIT {
                self.left_out = None;
            } else {
                list.push(path.to_path_buf());
            }
        }
    }
}

pub(super) async fn initialize(path: &Path) -> Result<()> {
    initialize_format(path, "sha1").await
}

/// Repository object formats cannot be mixed or repaired by rewriting a cache.
pub(super) async fn initialize_format(path: &Path, format: &str) -> Result<()> {
    ensure!(
        matches!(format, "sha1" | "sha256"),
        "unsupported Git object format"
    );
    if path
        .file_name()
        .is_some_and(|name| name == "working-tree.git")
        && !tokio::fs::try_exists(path).await?
        && tokio::fs::try_exists(path.with_file_name("working-tree.quarantine")).await?
    {
        anyhow::bail!("An interrupted shadow recovery must finish through hydration");
    }
    tokio::fs::create_dir_all(path).await?;
    if !tokio::fs::try_exists(path.join("HEAD")).await? {
        transport::git_output(
            transport::git(path, None).await?,
            &[
                "init",
                "--bare",
                "--quiet",
                &format!("--object-format={format}"),
                ".",
            ],
            vec![],
        )
        .await?;
    } else {
        let actual = transport::git_output(
            transport::git(path, None).await?,
            &["rev-parse", "--show-object-format"],
            vec![],
        )
        .await?;
        ensure!(
            actual == format!("{format}\n").as_bytes(),
            "Git cache object format differs; existing cache retained"
        );
    }
    Ok(())
}

const REPOSITORIES: [&str; 4] = [
    "working-tree.git",
    "repository.git",
    "incoming.git",
    "incoming-repository.git",
];

/// Leftovers of an interrupted helper or transfer (a killed daemon, a
/// SIGKILLed Git child): ref and index locks, temporary indexes and packs,
/// and staging copies. Callers hold the project's cache guard with no helper
/// running, so nothing live can own them; Git refuses to proceed past them.
pub(super) fn clear_interrupted(project: &Path) -> Result<()> {
    let entries = match fs::read_dir(project) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries.take(4096) {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (name.starts_with("stage-") || name.starts_with("hydrate-"))
            && entry.file_type()?.is_dir()
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    for repository in REPOSITORIES.map(|name| project.join(name)) {
        let Ok(entries) = fs::read_dir(&repository) else {
            continue;
        };
        for entry in entries.take(4096) {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type()?.is_file()
                && (name.starts_with("index-") || name.ends_with(".lock"))
            {
                fs::remove_file(entry.path())?;
            }
        }
        if let Ok(entries) = fs::read_dir(repository.join("objects/pack")) {
            for entry in entries.take(4096) {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with("tmp_")
                    && entry.file_type()?.is_file()
                {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        let mut pending = vec![repository.join("refs")];
        let mut seen = 0;
        while let Some(directory) = pending.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries {
                seen += 1;
                ensure!(seen <= 16_384, "mirror references exceed limit");
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    pending.push(entry.path());
                } else if kind.is_file() && entry.file_name().to_string_lossy().ends_with(".lock") {
                    fs::remove_file(entry.path())?;
                }
            }
        }
    }
    Ok(())
}

/// Every existing outgoing tip must still name a readable tree before a new
/// snapshot builds on it. On real damage the shadow is set aside (one bounded
/// slot) and rebuilt from the published remote, the only history that matters
/// for the next fast-forward push.
pub(super) async fn set_aside_damaged(shadow: &Path) -> Result<bool> {
    let mut damaged = false;
    for branch in ["main", "config", "handoff"] {
        let reference = format!("refs/heads/{branch}");
        let mut exists = transport::git(shadow, None).await?;
        exists.args(["rev-parse", "--verify", "--quiet", &reference]);
        if !transport::run(exists, vec![], std::time::Duration::from_secs(5), 256)
            .await?
            .success
        {
            continue;
        }
        let mut tree = transport::git(shadow, None).await?;
        tree.args(["cat-file", "-e", &format!("{reference}^{{tree}}")]);
        if !transport::run(tree, vec![], std::time::Duration::from_secs(10), 256)
            .await?
            .success
        {
            damaged = true;
            break;
        }
    }
    if !damaged {
        return Ok(false);
    }
    let aside = shadow.with_file_name("working-tree.damaged");
    let shadow = shadow.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if aside.exists() {
            fs::remove_dir_all(&aside)?;
        }
        fs::rename(&shadow, &aside)?;
        Ok(())
    })
    .await??;
    Ok(true)
}
pub(super) async fn fetch_published(shadow: &Path, credentials: &MirrorCredentials) -> Result<()> {
    let url = transport::endpoint(&credentials.working_tree_url)?;
    transport::git_output(
        transport::git(shadow, Some((&credentials.username, &credentials.password))).await?,
        &["fetch", "--no-tags", &url, "+refs/heads/*:refs/heads/*"],
        vec![],
    )
    .await?;
    Ok(())
}

/// Paths to copy, plus paths the project's own `.chimaeraignore` keeps out.
pub(super) async fn inventory(root: &Path, shadow: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut command = transport::git(root, None).await?;
    command.args(["rev-parse", "--is-inside-work-tree"]);
    let repo = transport::run(command, vec![], std::time::Duration::from_secs(5), 256)
        .await?
        .success;
    let list = |ignore_file: bool| async move {
        let mut command = transport::git(root, None).await?;
        if !repo {
            command.env("GIT_DIR", shadow).env("GIT_WORK_TREE", root);
        }
        let mut args: Vec<String> = [
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ]
        .map(String::from)
        .to_vec();
        // `--exclude` only skips UNTRACKED paths: what the project tracks
        // under one of these names still travels.
        args.extend(
            policy::REBUILT_DIRS
                .iter()
                .map(|dir| format!("--exclude={dir}/")),
        );
        if ignore_file {
            args.push("--exclude-from=.chimaeraignore".into());
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        transport::git_output(command, &args, vec![]).await
    };
    // `--exclude-from` refuses a missing file (a failed helper, and a
    // warning, on every pass), so it is passed only when the file is there;
    // one removed in between is listed again without it.
    let ignore_file = root.join(".chimaeraignore");
    let bytes = if tokio::fs::try_exists(&ignore_file).await? {
        match list(true).await {
            Ok(bytes) => bytes,
            Err(error) if !tokio::fs::try_exists(&ignore_file).await? => {
                list(false).await.map_err(|_| error)?
            }
            Err(error) => return Err(error),
        }
    } else {
        list(false).await?
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
    let (kept, ignored): (Vec<_>, Vec<_>) =
        paths.into_iter().partition(|path| !ignored.contains(path));
    Ok((kept, ignored))
}

pub(super) fn copy_tree(
    root: &Path,
    destination: &Path,
    (paths, ignored): (Vec<PathBuf>, Vec<PathBuf>),
    budget: u64,
    max_file: u64,
) -> Result<Report> {
    let directory = pin_project(root)?;
    copy_tree_pinned(
        root,
        &directory,
        destination,
        (paths, ignored),
        budget,
        max_file,
    )
}

fn pin_project(root: &Path) -> Result<fs::File> {
    let expected = fs::symlink_metadata(root)?;
    ensure!(expected.is_dir(), "project folder changed during mirror");
    // Registered workspace roots are canonical already. Resolving again would
    // accept an ancestor replaced with a symlink before this copy started.
    let directory = crate::download::open_beneath(
        &fs::File::open("/")?,
        root.strip_prefix("/")?,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
    )?;
    let pinned = directory.metadata()?;
    ensure!(
        expected.dev() == pinned.dev() && expected.ino() == pinned.ino(),
        "project folder changed during mirror"
    );
    Ok(directory)
}

fn check_project(root: &Path, directory: &fs::File) -> Result<()> {
    let current = fs::symlink_metadata(root)?;
    let pinned = directory.metadata()?;
    ensure!(
        current.is_dir() && current.dev() == pinned.dev() && current.ino() == pinned.ino(),
        "project folder changed during mirror"
    );
    Ok(())
}

fn copy_tree_pinned(
    root: &Path,
    directory: &fs::File,
    destination: &Path,
    (paths, ignored): (Vec<PathBuf>, Vec<PathBuf>),
    budget: u64,
    max_file: u64,
) -> Result<Report> {
    check_project(root, directory)?;
    fs::create_dir_all(destination)?;
    let mut report = Report {
        left_out: Some(Vec::new()),
        ..Report::default()
    };
    for relative in &ignored {
        report.leave_out(relative);
    }
    for relative in paths {
        if !policy::allowed_path(&relative) {
            report.excluded += 1;
            report.leave_out(&relative);
            continue;
        }
        // Open every component relative to the pinned project descriptor.
        // Checking ancestor paths before a full-path open leaves a race where
        // a replacement symlink can export an unrelated private directory.
        let mut input = match crate::download::open_beneath(
            directory,
            &relative,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        ) {
            Ok(file) => file,
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(nix::libc::ELOOP | nix::libc::ENOTDIR)
                ) =>
            {
                report.excluded += 1;
                report.leave_out(&relative);
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let metadata = input.metadata()?;
        if !metadata.is_file() {
            report.leave_out(&relative);
            continue;
        }
        if metadata.len() > max_file.min(policy::MAX_FILE_BYTES) {
            report.too_large += 1;
            report.leave_out(&relative);
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
            report.leave_out(&relative);
            continue;
        }
        fs::set_permissions(&target, metadata.permissions())?;
        report.bytes += length;
        report.files += 1;
    }
    check_project(root, directory)?;
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
            // A handoff commit identifies this publication, even if an idle
            // project has not changed. Reusing it makes Git skip receive-pack
            // and lets an old receipt masquerade as the pre-sleep checkpoint.
            if branch != "handoff" && old_tree == format!("{tree}\n").as_bytes() {
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
    let format = transport::git_output(
        transport::git(root, None).await?,
        &["rev-parse", "--show-object-format"],
        vec![],
    )
    .await?;
    let format = std::str::from_utf8(&format)?.trim();
    ensure!(
        format == "sha1",
        "Cloud Git transfer currently requires a SHA-1 repository; SHA-256 service support is unavailable"
    );
    initialize_format(cache, format).await?;
    cache_repository_refs(root, cache).await?;
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

async fn cache_repository_refs(root: &Path, cache: &Path) -> Result<()> {
    transport::git_output(
        transport::git(cache, None).await?,
        &[
            "fetch",
            "--prune",
            "--no-tags",
            root.to_str().context("invalid workspace path")?,
            "+refs/*:refs/*",
            "^refs/chimaera/staging/*",
        ],
        vec![],
    )
    .await?;
    // Recovery trees are local-only. Also retire any accidentally cached old
    // private refs before --mirror can publish them.
    let private_refs = transport::git_output(
        transport::git(cache, None).await?,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/chimaera/staging/",
        ],
        vec![],
    )
    .await?;
    let private_refs = std::str::from_utf8(&private_refs)?;
    ensure!(
        private_refs.lines().count() <= 4096,
        "staging recovery ref count exceeds limit"
    );
    for reference in private_refs.lines() {
        ensure!(
            reference.starts_with("refs/chimaera/staging/")
                && super::repository::reference(reference),
            "invalid staging recovery ref"
        );
        transport::git_output(
            transport::git(cache, None).await?,
            &["update-ref", "-d", reference],
            vec![],
        )
        .await?;
    }
    super::repository::keep_source_head(root, cache).await?;
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
    validate_tree_bytes(repository, branch, budget, max_file).await?;
    Ok(())
}
pub(super) async fn validate_tree_bytes(
    repository: &Path,
    branch: &str,
    budget: u64,
    max_file: u64,
) -> Result<u64> {
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
    Ok(total)
}

pub(super) async fn repository_origin(root: &Path) -> Option<String> {
    // `git config --get` exits 1 when the key is absent: a project without an
    // origin is an ordinary empty answer, not a failed Git helper to warn about.
    let mut command = transport::git(root, None).await.ok()?;
    command.args(["config", "--get", "remote.origin.url"]);
    let output = transport::run(
        command,
        vec![],
        std::time::Duration::from_secs(15),
        transport::JSON_CAP,
    )
    .await
    .ok()?;
    if !output.success {
        return None;
    }
    let origin = String::from_utf8(output.stdout).ok()?.trim().to_string();
    super::repository::safe_url(&origin).then_some(origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_snapshot_refuses_replaced_roots_and_never_follows_parent_or_file_links() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "chimaera-mirror-confined-{}",
            chimaera_core::generate_token()
        ));
        let project = root.join("project");
        let private = root.join("private");
        fs::create_dir_all(project.join("sub")).unwrap();
        fs::create_dir_all(&private).unwrap();
        let project = project.canonicalize().unwrap();
        fs::write(project.join("sub/file.txt"), "project bytes").unwrap();
        fs::write(private.join("file.txt"), "unrelated private bytes").unwrap();
        let directory = pin_project(&project).unwrap();
        let paths = || (vec![PathBuf::from("sub/file.txt")], Vec::new());
        fs::rename(project.join("sub"), project.join("old-sub")).unwrap();
        symlink(&private, project.join("sub")).unwrap();
        let stage = root.join("parent-link");
        let report = copy_tree_pinned(&project, &directory, &stage, paths(), 1000, 1000).unwrap();
        assert_eq!(report.files, 0);
        assert_eq!(
            report.left_out.unwrap(),
            vec![PathBuf::from("sub/file.txt")]
        );
        assert!(!stage.join("sub/file.txt").exists());
        fs::remove_file(project.join("sub")).unwrap();
        fs::rename(project.join("old-sub"), project.join("sub")).unwrap();
        fs::remove_file(project.join("sub/file.txt")).unwrap();
        symlink(private.join("file.txt"), project.join("sub/file.txt")).unwrap();
        let stage = root.join("file-link");
        let report = copy_tree_pinned(&project, &directory, &stage, paths(), 1000, 1000).unwrap();
        assert_eq!(report.files, 0);
        assert!(!stage.join("sub/file.txt").exists());
        fs::rename(&project, root.join("original-project")).unwrap();
        fs::create_dir_all(project.join("sub")).unwrap();
        fs::write(project.join("sub/file.txt"), "replacement project bytes").unwrap();
        let stage = root.join("replacement-root");
        assert!(copy_tree_pinned(&project, &directory, &stage, paths(), 1000, 1000).is_err());
        assert!(!stage.exists());
        fs::remove_dir_all(&project).unwrap();
        symlink(&private, &project).unwrap();
        assert!(pin_project(&project).is_err());
        // A preexisting ancestor link must not nominate a different root.
        fs::create_dir(private.join("nested")).unwrap();
        assert!(pin_project(&project.join("nested")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn staging_recovery_refs_never_enter_the_outbound_mirror_cache() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-private-refs-{}",
            chimaera_core::generate_token()
        ));
        fs::create_dir_all(root.join("source")).unwrap();
        let root = root.canonicalize().unwrap();
        let source = root.join("source");
        let cache = root.join("cache.git");
        transport::git_output(
            transport::git(&source, None).await.unwrap(),
            &["init", "--quiet", "--initial-branch=main"],
            vec![],
        )
        .await
        .unwrap();
        fs::write(source.join("file"), "ordinary").unwrap();
        transport::git_output(
            transport::git(&source, None).await.unwrap(),
            &["add", "file"],
            vec![],
        )
        .await
        .unwrap();
        let mut command = transport::git(&source, None).await.unwrap();
        command
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid");
        transport::git_output(command, &["commit", "--quiet", "-m", "base"], vec![])
            .await
            .unwrap();
        let reference = "refs/chimaera/staging/private-fixture";
        transport::git_output(
            transport::git(&source, None).await.unwrap(),
            &["update-ref", reference, "HEAD^{tree}"],
            vec![],
        )
        .await
        .unwrap();
        initialize(&cache).await.unwrap();
        // Seed a stale cache ref to prove it cannot survive the push preparation.
        transport::git_output(
            transport::git(&cache, None).await.unwrap(),
            &[
                "fetch",
                "--no-tags",
                source.to_str().unwrap(),
                &format!("+{reference}:{reference}"),
            ],
            vec![],
        )
        .await
        .unwrap();
        cache_repository_refs(&source, &cache).await.unwrap();
        let refs = transport::git_output(
            transport::git(&cache, None).await.unwrap(),
            &["for-each-ref", "--format=%(refname)"],
            vec![],
        )
        .await
        .unwrap();
        let refs = std::str::from_utf8(&refs).unwrap();
        assert!(refs.lines().any(|name| name == "refs/heads/main"));
        assert!(!refs
            .lines()
            .any(|name| name.starts_with("refs/chimaera/staging/")));
        assert!(!transport::git_output(
            transport::git(&source, None).await.unwrap(),
            &["ls-tree", reference],
            vec![]
        )
        .await
        .unwrap()
        .is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    /// Counts WARN events on this thread (the test's current-thread runtime).
    struct Warnings(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl tracing::Subscriber for Warnings {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() == tracing::Level::WARN {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// A project without an origin has no origin: no failed-helper warning on
    /// every snapshot.
    #[tokio::test]
    async fn a_project_without_an_origin_is_a_quiet_empty_answer() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-mirror-origin-{}",
            chimaera_core::generate_token()
        ));
        fs::create_dir_all(&root).unwrap();
        transport::git_output(
            transport::git(&root, None).await.unwrap(),
            &["init", "-q"],
            vec![],
        )
        .await
        .unwrap();
        let warnings = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let guard = tracing::subscriber::set_default(Warnings(warnings.clone()));
        assert_eq!(repository_origin(&root).await, None);
        drop(guard);
        assert_eq!(warnings.load(std::sync::atomic::Ordering::SeqCst), 0);
        transport::git_output(
            transport::git(&root, None).await.unwrap(),
            &["config", "remote.origin.url", "https://example.test/repo"],
            vec![],
        )
        .await
        .unwrap();
        assert_eq!(
            repository_origin(&root).await.as_deref(),
            Some("https://example.test/repo")
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A plain folder has no `.gitignore`: its dependency and cache folders
    /// still stay home, at any depth, while ordinary files travel.
    #[tokio::test]
    async fn rebuilt_folders_never_travel_as_untracked_content() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-mirror-{}",
            chimaera_core::generate_token()
        ));
        let project = root.join("project");
        let shadow = root.join("shadow");
        fs::create_dir_all(project.join("node_modules/dep")).unwrap();
        fs::create_dir_all(project.join("app/target/debug")).unwrap();
        fs::create_dir_all(project.join("app/src")).unwrap();
        initialize(&shadow).await.unwrap();
        fs::write(project.join("node_modules/dep/index.js"), "x").unwrap();
        fs::write(project.join("app/target/debug/out"), "x").unwrap();
        fs::write(project.join("app/src/main.rs"), "fn main() {}").unwrap();
        fs::write(project.join("notes.txt"), "hello").unwrap();
        let (paths, ignored) = inventory(&project, &shadow).await.unwrap();
        assert_eq!(
            paths,
            vec![PathBuf::from("app/src/main.rs"), PathBuf::from("notes.txt")]
        );
        assert!(ignored.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn unchanged_idle_handoff_requires_a_new_publication() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-idle-publication-{}",
            chimaera_core::generate_token()
        ));
        let shadow = root.join("shadow");
        let stage = root.join("stage");
        fs::create_dir_all(&stage).unwrap();
        fs::write(stage.join("manifest.json"), br#"{"continuation":"idle"}"#).unwrap();
        initialize(&shadow).await.unwrap();
        let first = commit_tree(&shadow, &stage, "handoff").await.unwrap();
        let second = commit_tree(&shadow, &stage, "handoff").await.unwrap();
        assert_ne!(
            first, second,
            "an old acknowledged OID cannot confirm a new drain"
        );
        let objects = transport::git_output(
            transport::git(&shadow, None).await.unwrap(),
            &[
                "rev-parse",
                &format!("{second}^"),
                &format!("{first}^{{tree}}"),
                &format!("{second}^{{tree}}"),
            ],
            vec![],
        )
        .await
        .unwrap();
        let objects = std::str::from_utf8(&objects)
            .unwrap()
            .lines()
            .collect::<Vec<_>>();
        assert_eq!(objects[0], first);
        assert_eq!(
            objects[1], objects[2],
            "the publication identity changes without altering project bytes"
        );
        for branch in ["main", "config"] {
            let first = commit_tree(&shadow, &stage, branch).await.unwrap();
            assert_eq!(commit_tree(&shadow, &stage, branch).await.unwrap(), first);
        }
        fs::remove_dir_all(root).unwrap();
    }

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
        let report =
            copy_tree(&project.canonicalize().unwrap(), &stage, paths, 10000, 1000).unwrap();
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
            assert!(paths.0.iter().any(|path| path == Path::new(&staged_name)));
            let first = root.join("first");
            copy_tree(&project.canonicalize().unwrap(), &first, paths, 10000, 1000).unwrap();
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
            copy_tree(
                &project.canonicalize().unwrap(),
                &second,
                paths,
                10000,
                1000,
            )
            .unwrap();
            assert_eq!(
                fs::read(second.join("notes.md")).unwrap(),
                b"unfinished now complete"
            );
            assert!(!second.join(&staged_name).exists());
            fs::remove_dir_all(root).unwrap();
        }
    }
}
