//! Portable Git state is an allowlist, never a copy of a host's executable
//! configuration. Ref updates use compare-and-swap and never force local work.
use super::{mirror, policy, protocol::MirrorCredentials, transport};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Path, time::Duration};

const MAX_REFS: usize = 4096;
const MAX_CONFIG: usize = 256;
const SOURCE_HEAD: &str = "refs/chimaera/source-head";
// A transaction keeps one of the two transport child slots while read-tree
// needs the other. Reserve transaction ownership before taking its child slot.
static REF_TRANSACTIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Snapshot {
    pub head: Option<String>,
    pub config: Vec<Entry>,
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
fn reference(value: &str) -> bool {
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
async fn optional(root: &Path, args: &[&str]) -> Result<Option<String>> {
    let mut command = transport::git(root, None).await?;
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
pub(super) async fn capture(root: &Path) -> Result<Option<Snapshot>> {
    if optional(root, &["rev-parse", "--git-dir"]).await?.is_none() {
        return Ok(None);
    }
    let head = optional(root, &["rev-parse", "--verify", "HEAD"]).await?;
    let bytes = transport::git_output(
        transport::git(root, None).await?,
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
    Ok(Some(Snapshot { head, config }))
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
                transport::git(root, None).await?,
                &["config", "--local", "--add", &entry.key, &value.value],
                vec![],
            )
            .await?;
        }
    }
    Ok(())
}
/// Fetch into a private temporary namespace, then publish each destination ref
/// independently. Linked worktree branches are never advanced behind their backs.
pub(super) async fn receive(
    root: &Path,
    cache: &Path,
    credentials: &MirrorCredentials,
    branch: Option<&str>,
    origin: Option<&str>,
    snapshot: Option<&Snapshot>,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<Vec<String>> {
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
        transport::git_output(
            transport::git(root, None).await?,
            &["init", "--quiet", "."],
            vec![],
        )
        .await?;
    }
    check()?;
    let namespace = format!(
        "refs/chimaera-transfer/{}/",
        chimaera_core::generate_token()
    );
    transport::git_output(
        transport::git(root, None).await?,
        &[
            "fetch",
            "--no-write-fetch-head",
            "--no-tags",
            cache.to_str().context("invalid repository cache")?,
            &format!("+refs/*:{namespace}*"),
        ],
        vec![],
    )
    .await?;
    let result = publish_refs(root, &namespace, existing, check).await;
    // Remove only this operation's private namespace, even if publishing failed.
    let refs = transport::git_output(
        transport::git(root, None).await?,
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
            transport::git(root, None).await?,
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
                transport::git(root, None).await?,
                &["symbolic-ref", "HEAD", branch],
                vec![],
            )
            .await?;
        } else if let Some(head) = snapshot.and_then(|snapshot| snapshot.head.as_deref()) {
            transport::git_output(
                transport::git(root, None).await?,
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
            transport::git_output(
                transport::git(root, None).await?,
                &["read-tree", "HEAD"],
                vec![],
            )
            .await?;
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
                transport::git(root, None).await?,
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
) -> Result<Vec<String>> {
    let refs = transport::git_output(
        transport::git(root, None).await?,
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
        transport::git(root, None).await?,
        &["worktree", "list", "--porcelain"],
        vec![],
    )
    .await?;
    let checked: HashSet<_> = std::str::from_utf8(&worktrees)?
        .lines()
        .filter_map(|line| line.strip_prefix("branch "))
        .collect();
    let clean = transport::git_output(
        transport::git(root, None).await?,
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
        if reference == SOURCE_HEAD || reference.starts_with("refs/chimaera-transfer/") {
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
                transport::git(root, None).await?,
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
        let mut ancestor = transport::git(root, None).await?;
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
                    transport::git(root, None).await?,
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
        transport::git(root, None).await?,
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
            transport::git(root, None).await?,
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
    let mut command = transport::git(root, None).await?;
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
        transport::git(root, None).await?,
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
    tokio::spawn(async move {
        let index_lock = transaction
            .index_lock
            .as_ref()
            .and_then(|reservation| reservation.path.as_ref())
            .context("Git index reservation unavailable")?
            .clone();
        let mut command = transport::git(&root, None).await?;
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
            let mut command = transport::git(root, None).await?;
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
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(crate::persist::PROJECT_STAGING_PREFIX)));
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
        let snapshot = capture(&laptop).await.unwrap().unwrap();
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
        std::fs::write(laptop.join(".git/FETCH_HEAD"), "user fetch marker\n").unwrap();
        let preserved = publish_refs(&laptop, namespace, true, &|| Ok(()))
            .await
            .unwrap();
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
