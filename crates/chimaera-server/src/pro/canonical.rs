//! Keeping both versions when a return meets a local edit: the incoming file
//! takes the path and the user's own version is saved right beside it as
//! `<name>.mine-<yyyymmdd-hhmm>`, where they can see it. These copies stay on
//! this computer: the mirror policy never publishes them (see
//! `policy::allowed_path`), so an unpublished local edit never comes back as a
//! new canonical project file.
use anyhow::{ensure, Context, Result};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};
/// One return keeps at most this much beside the files it replaced.
const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_FILES: usize = 4096;
const MARKER: &str = ".mine-";

/// Whether a file name is a kept copy (`<name>.mine-<yyyymmdd-hhmm>`, with an
/// optional `-<n>` when two land in the same minute).
pub(super) fn kept_copy_name(name: &str) -> bool {
    let Some(index) = name.rfind(MARKER) else {
        return false;
    };
    let stamp = &name[index + MARKER.len()..];
    let Some((stamp, suffix)) = stamp.split_at_checked(13) else {
        return false;
    };
    stamp.len() == 13
        && stamp.as_bytes()[8] == b'-'
        && stamp
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 8 || b.is_ascii_digit())
        && (suffix.is_empty()
            || suffix
                .strip_prefix('-')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
}

/// The name of the file a kept copy was saved beside (`notes.md` for
/// `notes.md.mine-20260929-1412`), or `None` when `name` is not a kept copy.
/// A copy whose name was shortened to fit (see [`KeptCopies::keep`]) names a
/// shortened file too; the review finds no such file and says so.
pub(super) fn original_name(name: &str) -> Option<&str> {
    if !kept_copy_name(name) {
        return None;
    }
    let original = &name[..name.rfind(MARKER)?];
    (!original.is_empty()).then_some(original)
}

/// The local time now; `None` when the C library cannot say.
pub(super) fn local_now() -> Option<nix::libc::tm> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        .try_into()
        .ok()?;
    let mut tm = std::mem::MaybeUninit::<nix::libc::tm>::zeroed();
    // SAFETY: `localtime_r` writes into the provided buffer and is thread safe.
    let converted = unsafe { !nix::libc::localtime_r(&now, tm.as_mut_ptr()).is_null() };
    // SAFETY: filled by the successful call above.
    converted.then(|| unsafe { tm.assume_init() })
}

/// The local minute, for a name people read.
fn stamp() -> String {
    let Some(tm) = local_now() else {
        return "00000000-0000".into();
    };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

/// The kept copies of one return, bounded.
pub(super) struct KeptCopies {
    stamp: String,
    bytes: u64,
    files: usize,
}
impl KeptCopies {
    pub fn new() -> Self {
        Self {
            stamp: stamp(),
            bytes: 0,
            files: 0,
        }
    }
    /// Copies the user's version at `target` (project-relative `relative`)
    /// to a new sibling and returns the sibling's project-relative path.
    /// Never overwrites an earlier copy; fails before touching the original
    /// when the bounds are reached.
    pub fn keep(&mut self, target: &Path, relative: &Path) -> Result<PathBuf> {
        self.keep_captured(target, relative, &|| Ok(()))
    }
    fn keep_captured(
        &mut self,
        target: &Path,
        relative: &Path,
        after_capture: &dyn Fn() -> Result<()>,
    ) -> Result<PathBuf> {
        ensure!(
            super::policy::allowed_path(relative),
            "unsafe conflict path"
        );
        let directory =
            super::install::directory(target.parent().context("conflict parent unavailable")?)?;
        let source = crate::download::open_beneath(
            &directory,
            Path::new(target.file_name().context("conflict name unavailable")?),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
        )?;
        let metadata = source.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() <= super::policy::MAX_FILE_BYTES,
            "local conflict file exceeds limit"
        );
        after_capture()?;
        ensure!(
            self.files < MAX_FILES
                && self
                    .bytes
                    .checked_add(metadata.len())
                    .is_some_and(|v| v <= MAX_BYTES),
            "local conflict storage exceeds limit"
        );
        let name = relative
            .file_name()
            .and_then(|name| name.to_str())
            .context("invalid conflict path")?;
        let mut output = None;
        let mut kept = PathBuf::new();
        for attempt in 0..100 {
            let candidate = if attempt == 0 {
                format!("{name}{MARKER}{}", self.stamp)
            } else {
                format!("{name}{MARKER}{}-{}", self.stamp, attempt + 1)
            };
            // Leave room under NAME_MAX; a name that long keeps its copy
            // truncated at a character boundary.
            let candidate: String = if candidate.len() > 250 {
                let keep = candidate.len() - 250;
                let mut cut = name.len().saturating_sub(keep);
                while cut > 0 && !name.is_char_boundary(cut) {
                    cut -= 1;
                }
                candidate.replacen(name, &name[..cut], 1)
            } else {
                candidate
            };
            use std::os::unix::fs::PermissionsExt;
            match rustix::fs::openat(
                &directory,
                candidate.as_str(),
                rustix::fs::OFlags::WRONLY
                    | rustix::fs::OFlags::CREATE
                    | rustix::fs::OFlags::EXCL
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::from_raw_mode((metadata.permissions().mode() & 0o777) as _),
            ) {
                Ok(fd) => {
                    output = Some(std::fs::File::from(fd));
                    kept = relative.with_file_name(candidate);
                    break;
                }
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let mut output = output.context("too many kept copies of one file")?;
        let mut input = source.take(super::policy::MAX_FILE_BYTES + 1);
        let copied = std::io::copy(&mut input, &mut output)?;
        ensure!(
            copied == metadata.len() && copied <= super::policy::MAX_FILE_BYTES,
            "local conflict changed while keeping it"
        );
        output.flush()?;
        output.sync_all()?;
        self.files += 1;
        self.bytes += copied;
        Ok(kept)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kept_copies_sit_beside_the_file_and_never_replace_each_other() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-kept-copies-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let root = root.canonicalize().unwrap();
        let file = root.join("docs/notes.md");
        std::fs::write(&file, "first local edit").unwrap();
        let mut copies = KeptCopies::new();
        let first = copies.keep(&file, Path::new("docs/notes.md")).unwrap();
        std::fs::write(&file, "second local edit").unwrap();
        let second = copies.keep(&file, Path::new("docs/notes.md")).unwrap();
        assert_ne!(first, second);
        for (kept, body) in [(&first, "first local edit"), (&second, "second local edit")] {
            assert!(kept.starts_with("docs"));
            let name = kept.file_name().unwrap().to_str().unwrap();
            assert!(
                name.starts_with("notes.md.mine-") && kept_copy_name(name),
                "{name}"
            );
            assert_eq!(std::fs::read_to_string(root.join(kept)).unwrap(), body);
        }
        copies.bytes = MAX_BYTES;
        assert!(copies.keep(&file, Path::new("docs/notes.md")).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second local edit");
        for name in [
            "notes.md",
            "notes.mine-",
            "a.mine-2026092-1200",
            "a.mine-20260928x1200",
            "a.mine-123456789012é",
            "a.mine-ååååååå",
            "a.mine-20260928-1200é",
        ] {
            assert!(!kept_copy_name(name), "{name}");
        }
        assert!(kept_copy_name("a.mine-20260928-1200-3"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn kept_copy_reads_the_captured_file_when_its_name_is_replaced() {
        const CHILD_ROOT: &str = "CHIMAERA_TEST_KEPT_FIFO_ROOT";
        let Some(root) = std::env::var_os(CHILD_ROOT) else {
            struct Root(PathBuf);
            impl Drop for Root {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            let root = std::env::temp_dir().join(format!(
                "chimaera-kept-race-{}",
                chimaera_core::generate_token()
            ));
            std::fs::create_dir(&root).unwrap();
            let root = Root(root.canonicalize().unwrap());
            struct Child(std::process::Child);
            impl Drop for Child {
                fn drop(&mut self) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            let test = format!(
                "{}::kept_copy_reads_the_captured_file_when_its_name_is_replaced",
                module_path!().split_once("::").unwrap().1
            );
            let mut child = Child(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &test])
                    .env(CHILD_ROOT, &root.0)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    assert!(status.success(), "kept-copy child failed");
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "kept-copy read stalled"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(
                std::fs::read(root.0.join("captured-and-refused")).unwrap(),
                b"both checked"
            );
            return;
        };
        let root = PathBuf::from(root);
        let target = root.join("notes.md");
        let replacement = root.join("different.md");
        std::fs::write(&target, b"original local bytes").unwrap();
        std::fs::write(&replacement, b"other local content").unwrap();
        let saved = KeptCopies::new()
            .keep_captured(&target, Path::new("notes.md"), &|| {
                std::fs::rename(&target, root.join("original.md"))?;
                std::os::unix::fs::symlink(&replacement, &target)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            std::fs::read(root.join(saved)).unwrap(),
            b"original local bytes"
        );
        assert_eq!(std::fs::read(&replacement).unwrap(), b"other local content");
        std::fs::remove_file(&target).unwrap();
        nix::unistd::mkfifo(
            &target,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        let before = std::fs::read_dir(&root).unwrap().count();
        assert!(KeptCopies::new()
            .keep(&target, Path::new("notes.md"))
            .is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), before);
        std::fs::write(root.join("captured-and-refused"), b"both checked").unwrap();
    }
}
