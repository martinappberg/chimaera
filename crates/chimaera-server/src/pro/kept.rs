//! Choosing between the two versions a return kept (`report_return`). When
//! both sides changed a file while apart, the cloud's version took the path
//! and this computer's version is the recorded `<name>.mine-<stamp>` sibling
//! beside it. The review lists those pairs (and the cloud's diverged
//! branches, read live from the project's refs), shows both texts, and
//! settles a pair one of three ways:
//!
//! - `use_mine`: this computer's version replaces the file (the sibling is
//!   renamed over it; a file the cloud deleted comes back);
//! - `use_cloud`: the cloud's version stays and the sibling moves to the
//!   Trash (`trash`), or is deleted where its drive has no Trash (a file the
//!   cloud deleted stays deleted);
//! - `keep_both`: nothing moves; the pair only stops waiting for a choice.
//!
//! Only siblings the return recorded are ever touched, only inside the
//! project folder, and never through a symlink: every component is opened
//! `O_NOFOLLOW` beneath the folder's descriptor, and the rename (to the file,
//! or out to the Trash) or removal runs against the pair's own directory
//! descriptor. A recorded sibling that is gone (renamed or deleted by hand)
//! or no longer a plain file counts as settled. Choices update `kept_both`/`kept_paths` and persist with the rest
//! of the Pro state, so the project's row and the chat stop asking.
use super::{
    canonical, policy,
    trash::{self, Discarded},
};
use crate::AppState;
use axum::{
    extract::{Path as UrlPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use rustix::{
    fs::{Mode, OFlags},
    io::Errno,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// Each version's text is shown up to this size; a larger or binary file
/// shows its size and the choices only.
const TEXT_MAX: u64 = 512 * 1024;
/// A kept copy's name this long may have been shortened to fit the file-name
/// limit (`canonical::KeptCopies::keep`), so the name it points back to may
/// not be the original's: such a pair never replaces any file by inference.
const SHORTENED_AT: usize = 240;
/// How long a choice waits for a mirror pass or another choice on the same
/// project before answering `busy`.
const LOCK_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Choice {
    UseMine,
    UseCloud,
    KeepBoth,
}
#[derive(Deserialize)]
pub(crate) struct ResolveOne {
    mine_path: String,
    choice: Choice,
}
#[derive(Deserialize)]
pub(crate) struct ResolveAll {
    choice: Choice,
}
#[derive(Deserialize)]
pub(crate) struct FileQuery {
    mine_path: String,
}

/// One pair, paths relative to the project folder: `path` holds the cloud's
/// version (`size`/`changed_at` null when the cloud deleted the file),
/// `mine_path` this computer's. Times are Unix ms.
#[derive(Debug, Serialize)]
struct Pair {
    path: String,
    mine_path: String,
    size: Option<u64>,
    mine_size: u64,
    changed_at: Option<u64>,
    mine_changed_at: Option<u64>,
    can_use_mine: bool,
}

#[derive(Debug)]
enum Refusal {
    UnknownProject,
    NotKept,
    Unsafe,
    Gone,
    NotHere,
    Busy,
    Unavailable,
    Failed(String),
}
impl Refusal {
    /// Status, stable code, and a plain sentence (diagnostic; the UI words
    /// each code itself).
    fn parts(&self) -> (StatusCode, &'static str, &'static str) {
        match self {
            Self::UnknownProject => (
                StatusCode::NOT_FOUND,
                "unknown_project",
                "This project isn't on this computer.",
            ),
            Self::NotKept => (
                StatusCode::NOT_FOUND,
                "not_kept",
                "That file isn't one of the versions kept for review.",
            ),
            Self::Unsafe => (
                StatusCode::BAD_REQUEST,
                "unsafe_path",
                "That file can't be changed from here: it is a link or not a plain file.",
            ),
            Self::Gone => (
                StatusCode::CONFLICT,
                "gone",
                "This computer's version is no longer in the folder.",
            ),
            Self::NotHere => (
                StatusCode::CONFLICT,
                "not_here",
                "This project isn't open on this computer right now.",
            ),
            Self::Busy => (
                StatusCode::CONFLICT,
                "busy",
                "The project is being saved. Try again in a moment.",
            ),
            Self::Unavailable => (
                StatusCode::CONFLICT,
                "folder_unavailable",
                "The project folder isn't available.",
            ),
            Self::Failed(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed",
                "The change didn't go through.",
            ),
        }
    }
}
impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        if let Self::Failed(detail) = &self {
            tracing::warn!(%detail, "a kept-version choice failed");
        }
        let (status, code, error) = self.parts();
        (status, Json(json!({"error": error, "error_code": code}))).into_response()
    }
}
fn failed(error: impl std::fmt::Display) -> Refusal {
    Refusal::Failed(error.to_string())
}

/// The project's folder and the kept copies its last return recorded.
struct Report {
    root: PathBuf,
    recorded: Vec<PathBuf>,
}
fn report(state: &AppState, workspace: &str) -> Result<Report, Refusal> {
    if super::authority::workspace(state, workspace).is_err() {
        return Err(Refusal::UnknownProject);
    }
    let root = crate::lock(&state.workspaces)
        .get(workspace)
        .map(|w| w.root)
        .ok_or(Refusal::UnknownProject)?;
    let recorded = crate::lock(&state.pro().status)
        .get(workspace)
        .filter(|status| status.kept_both.is_some())
        .map(|status| status.kept_paths.clone())
        .unwrap_or_default();
    Ok(Report { root, recorded })
}

/// A kept copy's project-relative path, split into its folder, its own name
/// and the name of the file it sits beside. Refuses anything but plain
/// relative components, and an original the mirror policy would never
/// install (`.git`, credentials, staging names).
fn split(mine_path: &Path) -> Result<(PathBuf, String, String), Refusal> {
    if mine_path.as_os_str().is_empty()
        || mine_path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(Refusal::Unsafe);
    }
    let name = mine_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Refusal::Unsafe)?;
    let original = canonical::original_name(name).ok_or(Refusal::NotKept)?;
    let parent = mine_path.parent().unwrap_or(Path::new("")).to_path_buf();
    if !policy::allowed_path(&parent.join(original)) {
        return Err(Refusal::Unsafe);
    }
    Ok((parent, name.to_owned(), original.to_owned()))
}

/// The pair's folder, walked from the project folder with every component
/// `O_NOFOLLOW`. `Ok(None)`: the folder is gone.
fn open_dir(root: &Path, parent: &Path) -> Result<Option<File>, Refusal> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let anchor = File::open("/").map_err(|_| Refusal::Unavailable)?;
    let relative = root.strip_prefix("/").map_err(|_| Refusal::Unavailable)?;
    let root_dir = crate::download::open_beneath(&anchor, relative, flags)
        .map_err(|_| Refusal::Unavailable)?;
    open_parent(&root_dir, parent)
}

fn open_parent(root_dir: &File, parent: &Path) -> Result<Option<File>, Refusal> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match crate::download::open_beneath(root_dir, parent, flags) {
        Ok(dir) => Ok(Some(dir)),
        Err(error) => match Errno::from_io_error(&error) {
            Some(Errno::NOENT) => Ok(None),
            Some(Errno::LOOP | Errno::NOTDIR | Errno::MLINK) => Err(Refusal::Unsafe),
            _ => Err(failed(error)),
        },
    }
}

/// One name in a pair's folder, never followed.
enum Entry {
    Missing,
    File(File, std::fs::Metadata),
    /// A symlink, a folder, or anything else that is not a plain file.
    Other,
}
fn entry(dir: &File, name: &str) -> Result<Entry, Refusal> {
    // Non-blocking: a FIFO planted under the name must not stall the open.
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    match rustix::fs::openat(dir, name, flags, Mode::empty()) {
        Ok(fd) => {
            let file = File::from(fd);
            let meta = file.metadata().map_err(failed)?;
            Ok(if meta.is_file() {
                Entry::File(file, meta)
            } else {
                Entry::Other
            })
        }
        Err(Errno::NOENT) => Ok(Entry::Missing),
        Err(Errno::LOOP | Errno::NOTDIR | Errno::MLINK) => Ok(Entry::Other),
        Err(error) => Err(failed(error)),
    }
}
fn millis(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}
fn shown(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A recorded pair as it stands now; `Ok(None)` when it no longer needs a
/// choice (its kept copy is gone or no longer a plain file, or the file's
/// own path became a folder or a link).
fn inspect(root: &Path, mine_path: &Path) -> Result<Option<Pair>, Refusal> {
    let (parent, mine, original) = match split(mine_path) {
        Ok(parts) => parts,
        Err(Refusal::NotKept | Refusal::Unsafe) => return Ok(None),
        Err(other) => return Err(other),
    };
    let dir = match open_dir(root, &parent) {
        Ok(Some(dir)) => dir,
        Ok(None) | Err(Refusal::Unsafe) => return Ok(None),
        Err(other) => return Err(other),
    };
    let Entry::File(_, mine_meta) = entry(&dir, &mine)? else {
        return Ok(None);
    };
    let cloud = match entry(&dir, &original)? {
        Entry::File(_, meta) => Some(meta),
        Entry::Missing => None,
        Entry::Other => return Ok(None),
    };
    Ok(Some(Pair {
        path: shown(&parent.join(&original)),
        mine_path: shown(mine_path),
        size: cloud.as_ref().map(|meta| meta.len()),
        mine_size: mine_meta.len(),
        changed_at: cloud.as_ref().and_then(millis),
        mine_changed_at: millis(&mine_meta),
        can_use_mine: mine.len() < SHORTENED_AT,
    }))
}

/// Apply one choice to one recorded pair; for `use_cloud`, where the kept
/// copy went. A kept copy already gone settles the pair for every choice but
/// `use_mine`, which has nothing to bring back. `home` is the home Trash
/// (`ProState::trash`).
fn apply(
    root: &Path,
    mine_path: &Path,
    choice: Choice,
    home: Option<&Path>,
    current: &dyn Fn() -> Result<File, Refusal>,
) -> Result<Option<Discarded>, Refusal> {
    let (parent, mine, original) = split(mine_path)?;
    let verified_root = current()?;
    let Some(dir) = open_parent(&verified_root, &parent)? else {
        return match choice {
            Choice::UseMine => Err(Refusal::Gone),
            _ => Ok(None),
        };
    };
    match entry(&dir, &mine)? {
        Entry::File(..) => {}
        Entry::Missing if choice == Choice::UseMine => return Err(Refusal::Gone),
        Entry::Missing => return Ok(None),
        Entry::Other => return Err(Refusal::Unsafe),
    }
    match (entry(&dir, &original)?, choice) {
        (Entry::Other, _) => return Err(Refusal::Unsafe),
        (_, Choice::UseMine) if mine.len() >= SHORTENED_AT => return Err(Refusal::Unsafe),
        _ => {}
    }
    let verified_root = current()?;
    let verified_parent = open_parent(&verified_root, &parent)?.ok_or(Refusal::Unavailable)?;
    {
        use std::os::unix::fs::MetadataExt;
        let before = dir.metadata().map_err(failed)?;
        let after = verified_parent.metadata().map_err(failed)?;
        if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
            return Err(Refusal::Unavailable);
        }
    }
    match choice {
        Choice::KeepBoth => Ok(None),
        Choice::UseCloud => {
            let origin = root.join(&parent).join(&mine);
            trash::discard(&dir, &mine, &origin, root, home)
                .map(Some)
                .map_err(failed)
        }
        // A rename follows neither name, and replaces the cloud's version in
        // one step (never a moment without a file at its path).
        Choice::UseMine => rustix::fs::renameat(&dir, mine.as_str(), &dir, original.as_str())
            .map(|()| None)
            .map_err(failed),
    }
}

/// One version for the side-by-side view: `{size, changed_at, text,
/// binary?, too_large?}`, text null past [`TEXT_MAX`] or when not UTF-8
/// text; null when the file is not there.
fn side(dir: &File, name: &str) -> Result<Value, Refusal> {
    let (file, meta) = match entry(dir, name)? {
        Entry::File(file, meta) => (file, meta),
        Entry::Missing => return Ok(Value::Null),
        Entry::Other => return Err(Refusal::Unsafe),
    };
    let size = meta.len();
    let changed_at = millis(&meta);
    let mut bytes = Vec::new();
    if size <= TEXT_MAX {
        file.take(TEXT_MAX + 1)
            .read_to_end(&mut bytes)
            .map_err(failed)?;
    }
    if size > TEXT_MAX || bytes.len() as u64 > TEXT_MAX {
        return Ok(
            json!({"size": size, "changed_at": changed_at, "text": null, "too_large": true}),
        );
    }
    let text = if bytes.iter().take(8192).any(|b| *b == 0) {
        None
    } else {
        String::from_utf8(bytes).ok()
    };
    Ok(json!({
        "size": size,
        "changed_at": changed_at,
        "binary": text.is_none(),
        "text": text,
    }))
}

/// Drop settled pairs from the report. The report ends once nothing waits
/// for a choice; kept copies past the listed ones (a return names up to 32)
/// keep the count up until `resolve_all` settles the rest (`everything`).
fn settle(state: &AppState, workspace: &str, settled: &[PathBuf], everything: bool) {
    let mut statuses = crate::lock(&state.pro().status);
    let Some(status) = statuses.get_mut(workspace) else {
        return;
    };
    let before = status.kept_paths.len();
    status.kept_paths.retain(|path| !settled.contains(path));
    let removed = before - status.kept_paths.len();
    let open = if everything {
        status.kept_paths.len()
    } else {
        status
            .kept_both
            .unwrap_or(0)
            .saturating_sub(removed)
            .max(status.kept_paths.len())
    };
    if open == 0 {
        status.kept_both = None;
        status.kept_paths.clear();
        status.kept_at = None;
        status.kept_total = None;
    } else {
        status.kept_both = Some(open);
    }
}

/// The review's answer: the pairs still waiting, how many kept copies the
/// return did not name, the cloud's branches, whether a choice can be made
/// here now (`here`: the project is this computer's to change), and whether
/// a copy discarded here goes to the Trash (`trash`; otherwise it is
/// deleted).
async fn listing(state: &Arc<AppState>, workspace: &str) -> Result<Value, Refusal> {
    let generation = super::mutation::generation(state);
    let cache = state.pro().cache(workspace).map_err(|_| Refusal::Busy)?;
    let held = tokio::time::timeout(LOCK_WAIT, cache.lock_owned())
        .await
        .map_err(|_| Refusal::Busy)?;
    let configuration = state.pro().configuration.clone().lock_owned().await;
    if generation != super::mutation::generation(state) {
        return Err(Refusal::NotHere);
    }
    let owner = state.clone();
    let workspace = workspace.to_owned();
    tokio::spawn(async move {
        let _held = held;
        let _configuration = configuration;
        listing_reserved(&owner, &workspace).await
    })
    .await
    .map_err(failed)?
}

/// Caller retains cache/configuration ownership through scan and settlement;
/// a newer return cannot replace this report while old paths are classified.
async fn listing_reserved(state: &Arc<AppState>, workspace: &str) -> Result<Value, Refusal> {
    let Report { root, recorded } = report(state, workspace)?;
    let scan_root = root.clone();
    let home = state.pro().trash.clone();
    let (pairs, settled, to_trash) = tokio::task::spawn_blocking(move || {
        let mut pairs = Vec::new();
        let mut settled = Vec::new();
        for path in recorded {
            match inspect(&scan_root, &path)? {
                Some(pair) => pairs.push(pair),
                None => settled.push(path),
            }
        }
        let to_trash = trash::available(&scan_root, home.as_deref());
        Ok::<_, Refusal>((pairs, settled, to_trash))
    })
    .await
    .map_err(failed)??;
    if !settled.is_empty() {
        settle(state, workspace, &settled, false);
        if let Err(error) = super::persist(state).await {
            tracing::warn!(%error, "the kept-version report could not be saved");
        }
        state.changes.notify_waiters();
    }
    // Git runs only for a project that has something from the cloud to
    // show: a report, or branches its last return kept (then read live, so
    // one merged and deleted since drops off).
    let kept_branches = crate::lock(&state.pro().preferences)
        .get(workspace)
        .is_some_and(|preference| !preference.git_branches.is_empty());
    let open = crate::lock(&state.pro().status)
        .get(workspace)
        .is_some_and(|status| status.kept_both.is_some());
    let branches = if kept_branches || open {
        super::repository::cloud_branches(&root).await
    } else {
        Vec::new()
    };
    let (files, total, at) = crate::lock(&state.pro().status)
        .get(workspace)
        .map(|status| {
            (
                status.kept_both.unwrap_or(0),
                status.kept_total.unwrap_or(0),
                status.kept_at,
            )
        })
        .unwrap_or_default();
    Ok(json!({
        "workspace_id": workspace,
        "files": files,
        "total": total.max(files),
        "returned_at": at,
        "unlisted": files.saturating_sub(pairs.len()),
        "pairs": pairs,
        "branches": branches,
        "here": super::may_write(state, workspace),
        "trash": to_trash,
    }))
}

/// `GET /pro/projects/{workspace}/kept`: what the last return kept in both
/// versions and still waits for a choice.
pub(crate) async fn list(
    State(state): State<Arc<AppState>>,
    UrlPath(workspace): UrlPath<String>,
) -> Response {
    match listing(&state, &workspace).await {
        Ok(value) => Json(value).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// `GET /pro/projects/{workspace}/kept/file?mine_path=`: both versions of one
/// recorded pair for the side-by-side view (`mine`, `cloud`; `cloud` null
/// when the cloud deleted the file).
pub(crate) async fn file(
    State(state): State<Arc<AppState>>,
    UrlPath(workspace): UrlPath<String>,
    Query(query): Query<FileQuery>,
) -> Response {
    let result = async {
        let Report { root, recorded } = report(&state, &workspace)?;
        let mine_path = PathBuf::from(&query.mine_path);
        if !recorded.contains(&mine_path) {
            return Err(Refusal::NotKept);
        }
        tokio::task::spawn_blocking(move || {
            let (parent, mine, original) = split(&mine_path)?;
            let dir = open_dir(&root, &parent)?.ok_or(Refusal::Gone)?;
            let mine_side = side(&dir, &mine)?;
            if mine_side.is_null() {
                return Err(Refusal::Gone);
            }
            Ok(json!({
                "path": shown(&parent.join(&original)),
                "mine_path": shown(&mine_path),
                "mine": mine_side,
                "cloud": side(&dir, &original)?,
            }))
        })
        .await
        .map_err(failed)?
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// What a choice did: the pairs it settled, the ones it could not
/// (`resolve_all` only; `{mine_path, error_code}`), and where the kept copies
/// it discarded went.
#[derive(Default)]
struct Outcome {
    settled: Vec<PathBuf>,
    failures: Vec<Value>,
    trashed: usize,
    deleted: usize,
}
impl Outcome {
    fn discarded(&mut self, went: Option<Discarded>) {
        match went {
            Some(Discarded::Trash) => self.trashed += 1,
            Some(Discarded::Deleted) => self.deleted += 1,
            None => {}
        }
    }
}

/// Run a choice serialized with mirror passes and other choices on the
/// project, and only while the project is this computer's to change; answer
/// with the updated review, plus `failed` and `discarded` (`{trash,
/// deleted}`: how many kept copies went to the Trash, and how many were
/// deleted because no Trash could take them).
async fn choose(
    state: &Arc<AppState>,
    workspace: &str,
    everything: bool,
    work: impl FnOnce(
            &Path,
            Vec<PathBuf>,
            Option<&Path>,
            &dyn Fn() -> Result<File, Refusal>,
        ) -> Result<Outcome, Refusal>
        + Send
        + 'static,
) -> Result<Value, Refusal> {
    let generation = super::mutation::generation(state);
    report(state, workspace)?;
    if !super::may_write(state, workspace) {
        return Err(Refusal::NotHere);
    }
    let cache = state.pro().cache(workspace).map_err(|_| Refusal::Busy)?;
    let held = tokio::time::timeout(LOCK_WAIT, cache.lock_owned())
        .await
        .map_err(|_| Refusal::Busy)?;
    // Again under the lock: a return may have replaced the report, or taken
    // the project away, while this waited.
    let configuration = state.pro().configuration.clone().lock_owned().await;
    let Report { root, recorded } = report(state, workspace)?;
    if generation != super::mutation::generation(state) || !super::may_write(state, workspace) {
        return Err(Refusal::NotHere);
    }
    // This existing final-mutation reservation counts local managed work and
    // requires a live worker proof; free/unconfigured local work stays inert.
    let reservation =
        super::mutation::begin_launch(state, workspace).map_err(|_| Refusal::NotHere)?;
    let ownership = crate::lock(&state.pro().ownership).get(workspace).cloned();
    let folder = root.clone();
    let identity = tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::MetadataExt;
        let dir = open_dir(&folder, Path::new(""))?.ok_or(Refusal::Unavailable)?;
        let meta = dir.metadata().map_err(failed)?;
        Ok::<_, Refusal>((meta.dev(), meta.ino()))
    })
    .await
    .map_err(failed)??;
    let owner = state.clone();
    let workspace = workspace.to_owned();
    tokio::spawn(async move {
        let _held = held;
        let _configuration = configuration;
        let _reservation = reservation;
        let state = &owner;
        let workspace = workspace.as_str();
        let folder = root.clone();
        let worker_state = owner.clone();
        let worker_workspace = workspace.to_owned();
        let expected_ownership = ownership.clone();
        let home = state.pro().trash.clone();
        let Outcome {
            settled,
            failures,
            trashed,
            deleted,
        } = tokio::task::spawn_blocking(move || {
            let current = || {
                use std::os::unix::fs::MetadataExt;
                if generation != super::mutation::generation(&worker_state)
                    || !super::may_write(&worker_state, &worker_workspace)
                    || crate::lock(&worker_state.pro().ownership)
                        .get(&worker_workspace)
                        .cloned()
                        != ownership
                {
                    return Err(Refusal::NotHere);
                }
                let dir = open_dir(&folder, Path::new(""))?.ok_or(Refusal::Unavailable)?;
                let meta = dir.metadata().map_err(failed)?;
                if (meta.dev(), meta.ino()) != identity {
                    return Err(Refusal::Unavailable);
                }
                Ok(dir)
            };
            current()?;
            work(&folder, recorded, home.as_deref(), &current)
        })
        .await
        .map_err(failed)??;
        if generation != super::mutation::generation(state)
            || !super::may_write(state, workspace)
            || crate::lock(&state.pro().ownership).get(workspace).cloned() != expected_ownership
        {
            return Err(Refusal::NotHere);
        }
        settle(
            state,
            workspace,
            &settled,
            everything && failures.is_empty(),
        );
        super::persist(state).await.map_err(failed)?;
        state.changes.notify_waiters();
        for path in &settled {
            if let Ok((parent, _, original)) = split(path) {
                let file = root.join(parent).join(original);
                crate::git::mark_path_dirty(state, &file.to_string_lossy()).await;
            }
        }
        let mut value = listing_reserved(state, workspace).await?;
        value["failed"] = Value::Array(failures);
        value["discarded"] = json!({"trash": trashed, "deleted": deleted});
        Ok(value)
    })
    .await
    .map_err(failed)?
}

/// `POST /pro/projects/{workspace}/kept/resolve` `{mine_path, choice}`: settle
/// one recorded pair; answers with the updated review.
pub(crate) async fn resolve(
    State(state): State<Arc<AppState>>,
    UrlPath(workspace): UrlPath<String>,
    Json(request): Json<ResolveOne>,
) -> Response {
    let mine_path = PathBuf::from(&request.mine_path);
    let choice = request.choice;
    let result = choose(
        &state,
        &workspace,
        false,
        move |root, recorded, home, current| {
            if !recorded.contains(&mine_path) {
                return Err(Refusal::NotKept);
            }
            let mut outcome = Outcome::default();
            outcome.discarded(apply(root, &mine_path, choice, home, current)?);
            outcome.settled.push(mine_path);
            Ok(outcome)
        },
    )
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// `POST /pro/projects/{workspace}/kept/resolve_all` `{choice}`: settle every
/// recorded pair the same way. A pair that cannot take the choice stays and
/// is named in `failed`; when none failed the report ends, including kept
/// copies the return did not name (they keep their `.mine-…` names).
pub(crate) async fn resolve_all(
    State(state): State<Arc<AppState>>,
    UrlPath(workspace): UrlPath<String>,
    Json(request): Json<ResolveAll>,
) -> Response {
    let choice = request.choice;
    let result = choose(
        &state,
        &workspace,
        true,
        move |root, recorded, home, current| {
            let mut outcome = Outcome::default();
            for path in recorded {
                match apply(root, &path, choice, home, current) {
                    Ok(went) => {
                        outcome.discarded(went);
                        outcome.settled.push(path);
                    }
                    Err(refusal) => outcome.failures.push(json!({
                        "mine_path": shown(&path),
                        "error_code": refusal.parts().1,
                    })),
                }
            }
            Ok(outcome)
        },
    )
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

#[cfg(test)]
mod tests;
