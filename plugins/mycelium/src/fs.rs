//! The reader's filesystem: workspace-relative paths, answered in the
//! component by the host (`HostFs`, in `lib.rs`) and — in native unit tests
//! only — by `std::fs` under the host's rules (`StdFs`). The reader never
//! sees which, so its tests exercise the same logic the component runs.

use chimaera_plugin_api::{Entry, Stat};

/// What the reader asks of a filesystem. Paths are workspace-relative; a
/// path with a symlink in any component is refused, never followed.
pub(crate) trait Fs {
    /// A regular file's bytes, at most `cap`.
    fn read(&self, rel: &str, cap: u32) -> Result<Vec<u8>, String>;
    /// Size, mtime (ms since the epoch) and kind.
    fn stat(&self, rel: &str) -> Result<Stat, String>;
    /// A directory's entries, at most `cap`, sorted by name.
    fn list(&self, rel: &str, cap: u32) -> Result<Vec<Entry>, String>;
}

/// The host names a refused symlink in its error — "`<path>: <component> is
/// a symlink — refused (…)`" (`hostfns.rs`) — and that wording is how the
/// reader tells a refusal (warned about, and stamped) from a missing file
/// (silently absent).
const SYMLINK_REFUSED: &str = " is a symlink — refused";

/// The host's error for a read of something that isn't a regular file.
pub(crate) const NOT_REGULAR: &str = ": not a regular file";

pub(crate) fn is_symlink_refusal(err: &str) -> bool {
    err.contains(SYMLINK_REFUSED)
}

#[cfg(test)]
pub(crate) use std_fs::StdFs;

/// `std::fs` with the host's confinement, for native tests: the same
/// refusals (`..`, absolute paths, a symlink in any component) in the same
/// words, the same caps and the same sorted listings.
#[cfg(test)]
mod std_fs {
    use std::io::Read;
    use std::path::{Component, Path, PathBuf};

    use chimaera_plugin_api::{Entry, Stat};

    use super::{Fs, NOT_REGULAR, SYMLINK_REFUSED};

    pub(crate) struct StdFs {
        root: PathBuf,
    }

    fn shown(rel: &Path) -> String {
        if rel.as_os_str().is_empty() {
            ".".to_string()
        } else {
            rel.display().to_string()
        }
    }

    impl StdFs {
        pub(crate) fn new(root: &Path) -> Self {
            StdFs {
                root: root.to_path_buf(),
            }
        }

        /// `rel` beneath the root, every component checked without
        /// following a link — the host's `O_NOFOLLOW` walk.
        fn resolve(&self, rel: &str) -> Result<(PathBuf, PathBuf), String> {
            let mut clean = PathBuf::new();
            for part in Path::new(rel).components() {
                match part {
                    Component::Normal(name) => clean.push(name),
                    Component::CurDir => {}
                    Component::ParentDir => {
                        return Err(format!(
                            "{rel}: `..` is refused — paths stay inside the workspace"
                        ))
                    }
                    Component::RootDir | Component::Prefix(_) => {
                        return Err(format!(
                            "{rel}: absolute paths are refused — paths are workspace-relative"
                        ))
                    }
                }
            }
            let mut at = self.root.clone();
            let mut walked = PathBuf::new();
            for part in clean.components() {
                at.push(part);
                walked.push(part);
                match std::fs::symlink_metadata(&at) {
                    Ok(meta) if meta.is_symlink() => {
                        return Err(format!(
                            "{}: {}{SYMLINK_REFUSED} (no component of a path may be one)",
                            shown(&clean),
                            walked.display()
                        ))
                    }
                    Ok(_) => {}
                    Err(err) => return Err(format!("{}: {err}", shown(&clean))),
                }
            }
            Ok((at, clean))
        }
    }

    impl Fs for StdFs {
        fn read(&self, rel: &str, cap: u32) -> Result<Vec<u8>, String> {
            let (path, clean) = self.resolve(rel)?;
            // Checked before the open: opening a FIFO would block.
            let meta =
                std::fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", shown(&clean)))?;
            if !meta.is_file() {
                return Err(format!("{}{NOT_REGULAR}", shown(&clean)));
            }
            let file = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", shown(&clean)))?;
            let mut bytes = Vec::new();
            file.take(u64::from(cap))
                .read_to_end(&mut bytes)
                .map_err(|e| format!("{}: {e}", shown(&clean)))?;
            Ok(bytes)
        }

        fn stat(&self, rel: &str) -> Result<Stat, String> {
            let (path, clean) = self.resolve(rel)?;
            let meta =
                std::fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", shown(&clean)))?;
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64);
            Ok(Stat {
                size: meta.len(),
                mtime_ms,
                is_dir: meta.is_dir(),
            })
        }

        fn list(&self, rel: &str, cap: u32) -> Result<Vec<Entry>, String> {
            let (path, clean) = self.resolve(rel)?;
            let entries =
                std::fs::read_dir(&path).map_err(|e| format!("{}: {e}", shown(&clean)))?;
            let mut out = Vec::new();
            for entry in entries.take(cap as usize) {
                let entry = entry.map_err(|e| format!("{}: {e}", shown(&clean)))?;
                let kind = entry
                    .file_type()
                    .map_err(|e| format!("{}: {e}", shown(&clean)))?;
                out.push(Entry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    is_dir: kind.is_dir(),
                    is_symlink: kind.is_symlink(),
                });
            }
            out.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(out)
        }
    }
}
