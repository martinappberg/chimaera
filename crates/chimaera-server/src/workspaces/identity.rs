//! A folder carries its workspace id.
//!
//! A workspace id is minted per daemon registry, and the cloud copy of a
//! project (Pro continuity) is keyed by it. A reinstall, a state reset or a
//! second daemon would mint a NEW id for the same folder, and the cloud copy
//! could never be matched again. The folder therefore records the id it was
//! registered under, and registration reuses it (see
//! [`super::WorkspaceStore::add_identified`]). The id follows the folder: a
//! copy of the folder on another computer (a synced folder, an archive) is
//! the same project there.
//!
//! The marker is one JSON line, always where git and the user's own files
//! never see it: `<root>/.git/chimaera-workspace` when the root has a `.git`
//! directory (never committed, so a plain `git clone` is a separate project);
//! `<git dir>/chimaera-workspace` when `.git` is a FILE (a linked worktree or
//! a submodule: `gitdir: …` names its private git directory, which is deleted
//! with the worktree, so the folder never shows an untracked file and
//! `git worktree remove` is never blocked by one); else the dotfile
//! `<root>/.chimaera-workspace`. None of them is ever mirrored to the cloud
//! (`pro::policy::allowed_path`).
//!
//! Everything here is best effort: a read-only folder or a network
//! filesystem that refuses the write only means the folder carries no
//! identity, never that registration fails.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const DOTFILE: &str = ".chimaera-workspace";
const IN_GIT: &str = "chimaera-workspace";
/// A marker is one short line; anything bigger is not ours.
const MAX_BYTES: u64 = 4096;

/// What a folder says about itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// The workspace id the folder was registered under.
    pub id: String,
    /// Unix seconds; informational.
    #[serde(default)]
    pub written_at: u64,
}

/// The marker's path for `root`. Nothing is followed through a `.git`
/// symlink: only a real `.git` directory, or a `.git` file naming a real git
/// directory, hosts it.
pub fn marker_path(root: &Path) -> PathBuf {
    let git = root.join(".git");
    match std::fs::symlink_metadata(&git) {
        Ok(meta) if meta.is_dir() => return git.join(IN_GIT),
        Ok(meta) if meta.is_file() => {
            if let Some(dir) = linked_git_dir(root, &git) {
                return dir.join(IN_GIT);
            }
        }
        _ => {}
    }
    root.join(DOTFILE)
}

/// The private git directory a worktree's or submodule's `.git` FILE names
/// (`gitdir: <path>`, relative to the root). Only a directory that looks like
/// one (it has a `HEAD`) counts: a `.git` file must not steer a write into an
/// arbitrary directory.
fn linked_git_dir(root: &Path, file: &Path) -> Option<PathBuf> {
    let (file, meta) = crate::fs::open_regular(file).ok()?;
    if meta.len() > MAX_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_BYTES).read_to_string(&mut text).ok()?;
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let dir = root.join(target);
    std::fs::metadata(dir.join("HEAD"))
        .is_ok_and(|head| head.is_file())
        .then_some(dir)
}

/// Same rule as the ids the account accepts (`pro::valid_id`): a marker
/// naming anything else is treated as absent.
pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

/// The folder's marker; `None` when it is missing, not a regular file,
/// over 4 KiB, not parseable, or names an id that is not well formed.
/// Blocking filesystem work: call off the reactor.
pub fn read(root: &Path) -> Option<Marker> {
    // `open_regular` never parks on a FIFO swapped in for the marker and
    // refuses devices.
    let (file, meta) = crate::fs::open_regular(&marker_path(root)).ok()?;
    if meta.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let marker: Marker = serde_json::from_slice(&bytes).ok()?;
    valid_id(&marker.id).then_some(marker)
}

/// Make the folder say it is workspace `id`. Atomic (a temp file beside the
/// marker, then a rename) and best effort: any failure is logged at debug
/// level and reported as `false`, never raised. Returns whether the folder
/// now carries this id. A marker that already names it is left alone (no
/// rewrite per open). Blocking filesystem work.
pub fn write(root: &Path, id: &str) -> bool {
    match try_write(root, id) {
        Ok(now_carries) => now_carries,
        Err(error) => {
            tracing::debug!(root = %root.display(), %error, "could not record the workspace identity in its folder");
            false
        }
    }
}

fn try_write(root: &Path, id: &str) -> std::io::Result<bool> {
    use std::io::Write;
    if !valid_id(id) {
        return Ok(false);
    }
    if read(root).is_some_and(|current| current.id == id) {
        return Ok(true);
    }
    let path = marker_path(root);
    let marker = Marker {
        id: id.to_owned(),
        written_at: super::unix_now(),
    };
    let mut line = serde_json::to_vec(&marker).map_err(std::io::Error::other)?;
    line.push(b'\n');
    // The `.chimaera-staging-` prefix keeps an unfinished temp file out of a
    // snapshot even where the marker sits in the project root.
    let temp = path.with_file_name(crate::persist::project_temp_name(
        path.file_name().unwrap_or_default(),
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let written = file.write_all(&line);
    drop(file);
    // A marker path that is a symlink or a directory is replaced or refused
    // by the rename itself; never written through.
    if let Err(error) = written.and_then(|()| std::fs::rename(&temp, &path)) {
        std::fs::remove_file(&temp).ok();
        return Err(error);
    }
    Ok(true)
}

/// Write the marker only when the folder has none (a stat): existing users
/// gain an identity over time without ever rewriting one. Blocking.
pub(crate) fn backfill(root: &Path, id: &str) -> bool {
    let missing = matches!(
        std::fs::symlink_metadata(marker_path(root)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    missing && write(root, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-identity-{label}-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_marker_lives_in_git_when_there_is_a_git_directory_else_in_a_dotfile() {
        let plain = folder("plain");
        assert_eq!(marker_path(&plain), plain.join(".chimaera-workspace"));

        let repo = folder("repo");
        std::fs::create_dir(repo.join(".git")).unwrap();
        assert_eq!(marker_path(&repo), repo.join(".git/chimaera-workspace"));
        assert!(write(&repo, "w-aaaa1111"));
        assert!(repo.join(".git/chimaera-workspace").is_file());
        assert!(!repo.join(".chimaera-workspace").exists());

        // A `.git` FILE that names no git directory: the dotfile, never a
        // path under a file.
        let worktree = folder("worktree");
        std::fs::write(worktree.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(marker_path(&worktree), worktree.join(".chimaera-workspace"));
        assert!(write(&worktree, "w-bbbb2222"));
        assert_eq!(read(&worktree).unwrap().id, "w-bbbb2222");
    }

    /// A linked worktree's marker lives in its private git directory: the
    /// folder never shows an untracked file (and `git worktree remove` is
    /// never blocked by one).
    #[test]
    fn a_linked_worktrees_marker_lives_in_its_private_git_directory() {
        let main = folder("linked-main");
        let admin = main.join(".git/worktrees/feature");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/feature\n").unwrap();

        // Absolute `gitdir:` (what `git worktree add` writes).
        let worktree = folder("linked-worktree");
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        assert_eq!(marker_path(&worktree), admin.join("chimaera-workspace"));
        assert!(write(&worktree, "w-linked001"));
        assert_eq!(read(&worktree).unwrap().id, "w-linked001");
        assert!(!worktree.join(".chimaera-workspace").exists());
        let names: Vec<_> = std::fs::read_dir(&worktree)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [".git"],
            "the folder itself gains nothing: {names:?}"
        );

        // Relative `gitdir:` (a submodule's).
        let sub = main.join("vendor/lib");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../../.git/worktrees/feature\n").unwrap();
        assert_eq!(
            read(&sub).unwrap().id,
            "w-linked001",
            "same private directory"
        );

        // A directory that is not a git directory (no HEAD) is never steered into.
        let decoy = folder("linked-decoy");
        let hostile = folder("linked-hostile");
        std::fs::write(
            hostile.join(".git"),
            format!("gitdir: {}\n", decoy.display()),
        )
        .unwrap();
        assert_eq!(marker_path(&hostile), hostile.join(".chimaera-workspace"));
        assert!(write(&hostile, "w-linked002"));
        assert!(std::fs::read_dir(&decoy).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_git_symlink_does_not_host_the_marker() {
        let target = folder("symlink-target");
        let root = folder("symlink");
        std::os::unix::fs::symlink(&target, root.join(".git")).unwrap();
        assert_eq!(marker_path(&root), root.join(".chimaera-workspace"));
    }

    #[test]
    fn a_marker_round_trips_and_an_identical_one_is_not_rewritten() {
        let root = folder("roundtrip");
        assert!(read(&root).is_none());
        assert!(write(&root, "w-cccc3333"));
        let first = read(&root).unwrap();
        assert_eq!(first.id, "w-cccc3333");
        assert!(first.written_at > 0);
        // One line of JSON with nothing but the id and its stamp.
        let raw = std::fs::read_to_string(marker_path(&root)).unwrap();
        assert_eq!(raw.lines().count(), 1, "{raw}");
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 2, "{raw}");
        // Same id: untouched (the file would be replaced on a rewrite).
        let modified = std::fs::metadata(marker_path(&root))
            .unwrap()
            .modified()
            .unwrap();
        assert!(write(&root, "w-cccc3333"));
        assert_eq!(
            std::fs::metadata(marker_path(&root))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
        // A different id replaces it, and no temp file is left behind.
        assert!(write(&root, "w-dddd4444"));
        assert_eq!(read(&root).unwrap().id, "w-dddd4444");
        let names: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [".chimaera-workspace"], "no leftovers: {names:?}");
    }

    #[test]
    fn an_unreadable_oversized_or_malformed_marker_is_no_marker() {
        let root = folder("bad");
        let path = marker_path(&root);
        for content in [
            String::new(),
            "not json".to_owned(),
            r#"{"id":"w-x","written_at":1"#.to_owned(),
            // An id the account would refuse.
            r#"{"id":"../evil","written_at":1}"#.to_owned(),
            r#"{"id":"","written_at":1}"#.to_owned(),
            format!(r#"{{"id":"{}","written_at":1}}"#, "a".repeat(129)),
            r#"{"written_at":1}"#.to_owned(),
            // Valid JSON, but over 4 KiB.
            format!(
                r#"{{"id":"w-ok","written_at":1,"pad":"{}"}}"#,
                "x".repeat(5000)
            ),
        ] {
            std::fs::write(&path, &content).unwrap();
            assert!(read(&root).is_none(), "{content:.60}");
        }
        // A directory where the marker should be is not a marker, and a
        // write cannot replace it.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(read(&root).is_none());
        assert!(!write(&root, "w-eeee5555"));
        assert!(path.is_dir());
        // Well formed with extra fields (a newer daemon's): still ours.
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, r#"{"id":"w-ok","written_at":1,"future":true}"#).unwrap();
        assert_eq!(read(&root).unwrap().id, "w-ok");
        // An id the account would refuse is never written.
        assert!(!write(&root, "not an id"));
        assert_eq!(read(&root).unwrap().id, "w-ok");
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_the_marker_never_blocks_a_read() {
        let root = folder("fifo");
        nix::unistd::mkfifo(
            &root.join(".chimaera-workspace"),
            nix::sys::stat::Mode::S_IRWXU,
        )
        .unwrap();
        assert!(read(&root).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_folder_never_fails_the_caller() {
        use std::os::unix::fs::PermissionsExt;
        let root = folder("readonly");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        let writable = std::fs::write(root.join("probe"), b"x").is_ok();
        let written = write(&root, "w-ffff6666");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !writable {
            // Not running as root: the folder really refused the write.
            assert!(!written);
            assert!(read(&root).is_none());
        }
    }

    #[test]
    fn backfill_writes_only_where_there_is_no_marker() {
        let root = folder("backfill");
        assert!(backfill(&root, "w-aaaa0001"));
        assert!(!backfill(&root, "w-aaaa0002"), "an existing marker stays");
        assert_eq!(read(&root).unwrap().id, "w-aaaa0001");
    }
}
