//! Path forms shared by the daemon, its clients and its executables lookup.
//!
//! The daemon↔UI wire carries one path form on every platform: forward
//! slashes, and on Windows a drive path without the `\\?\` verbatim prefix
//! (`C:/Users/me/proj`). Windows accepts forward slashes on the way back in,
//! so the UI never has to know about backslashes. Unix paths pass through
//! unchanged (a backslash is an ordinary filename byte there).

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// The wire form of `path` (see the module docs).
pub fn wire_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        wire_path_windows(&path.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
    }
}

/// The Windows half of [`wire_path`] over a string, so its rules are testable
/// on any host: strips the verbatim prefix (`\\?\C:\x` → `C:/x`,
/// `\\?\UNC\srv\share` → `//srv/share`), turns every `\` into `/`, and
/// upper-cases a drive letter.
pub fn wire_path_windows(raw: &str) -> String {
    let rest = if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(local) = raw.strip_prefix(r"\\?\") {
        local.to_string()
    } else {
        raw.to_string()
    };
    let mut out = rest.replace('\\', "/");
    // One spelling per drive: `c:/x` and `C:/x` are the same folder, and the
    // UI compares paths as strings.
    if out.as_bytes().get(1) == Some(&b':') && out.as_bytes()[0].is_ascii_lowercase() {
        out[..1].make_ascii_uppercase();
    }
    out
}

/// `std::fs::canonicalize` without the Windows verbatim prefix, so a
/// canonical path compares equal (`starts_with`) to one git, a shell or the
/// user wrote. Identical to `std::fs::canonicalize` on Unix.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    dunce::canonicalize(path)
}

/// The user's home directory. Unix: `$HOME` when set and non-empty (what
/// the user's shells and agents see), else the passwd entry. Windows: the
/// profile folder (`%USERPROFILE%`); `HOME` is not a Windows convention and
/// is usually unset.
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
    }
    #[cfg(not(unix))]
    {
        dirs::home_dir()
    }
}

/// Expand a leading `~` or `~/` (and on Windows `~\`) against `home`.
/// Anything else, including `~user`, is returned unchanged.
pub fn expand_tilde_with(raw: &str, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return PathBuf::from(raw);
    };
    if raw == "~" {
        return home.to_path_buf();
    }
    let rest = raw
        .strip_prefix("~/")
        .or_else(|| cfg!(windows).then(|| raw.strip_prefix("~\\")).flatten());
    match rest {
        Some(rest) => home.join(rest),
        None => PathBuf::from(raw),
    }
}

/// [`expand_tilde_with`] against [`home_dir`].
pub fn expand_tilde(raw: &str) -> PathBuf {
    expand_tilde_with(raw, home_dir().as_deref())
}

/// Whether `path` is a file this platform would run as a program. Unix: a
/// regular file with an execute bit. Windows: a regular file whose extension
/// is in `PATHEXT` (`.exe`, `.cmd`, `.bat`, `.com` when unset).
pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
            executable_extensions()
                .iter()
                .any(|e| e.eq_ignore_ascii_case(ext))
        })
    }
}

/// The executable extensions, without dots, from `PATHEXT`. Empty on Unix,
/// where a program name is used as written.
pub fn executable_extensions() -> Vec<String> {
    if cfg!(unix) {
        return Vec::new();
    }
    let raw = std::env::var("PATHEXT").unwrap_or_default();
    parse_pathext(&raw)
}

/// Pure core of [`executable_extensions`]: `.EXE;.CMD` → `["EXE", "CMD"]`,
/// with Windows' own default when the variable is empty.
pub fn parse_pathext(raw: &str) -> Vec<String> {
    let list: Vec<String> = raw
        .split(';')
        .map(|e| e.trim().trim_start_matches('.').to_string())
        .filter(|e| !e.is_empty())
        .collect();
    if list.is_empty() {
        ["COM", "EXE", "BAT", "CMD"].map(String::from).to_vec()
    } else {
        list
    }
}

/// Find `name` on a `PATH`-style list (the process's own `PATH` when
/// `path_var` is `None`). A name with a directory part is checked as given.
/// On Windows a bare name is also tried with each `PATHEXT` extension, the
/// way `cmd.exe` resolves it.
pub fn find_on_path(name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.components().count() > 1 {
        return candidates(direct).into_iter().find(|p| is_executable(p));
    }
    let owned;
    let path_var = match path_var {
        Some(p) => p,
        None => {
            owned = std::env::var_os("PATH")?;
            owned.as_os_str()
        }
    };
    std::env::split_paths(path_var)
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| candidates(&dir.join(name)))
        .find(|p| is_executable(p))
}

fn candidates(path: &Path) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    if path.extension().is_none() {
        for ext in executable_extensions() {
            let mut with = path.as_os_str().to_owned();
            with.push(".");
            // PATHEXT spells extensions upper-case; the file on disk (and
            // the path shown to the user) is almost always lower-case.
            with.push(ext.to_ascii_lowercase());
            out.push(PathBuf::from(with));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_wire_form() {
        assert_eq!(wire_path_windows(r"C:\Users\me\proj"), "C:/Users/me/proj");
        assert_eq!(wire_path_windows(r"\\?\C:\Users\me"), "C:/Users/me");
        assert_eq!(wire_path_windows(r"\\?\UNC\srv\share\x"), "//srv/share/x");
        assert_eq!(wire_path_windows(r"\\srv\share"), "//srv/share");
        assert_eq!(wire_path_windows("C:/already/fine"), "C:/already/fine");
        assert_eq!(wire_path_windows(r"c:\lower"), "C:/lower");
    }

    #[test]
    fn tilde_expansion() {
        let home = Path::new("/h");
        assert_eq!(expand_tilde_with("~", Some(home)), PathBuf::from("/h"));
        assert_eq!(
            expand_tilde_with("~/a/b", Some(home)),
            PathBuf::from("/h/a/b")
        );
        assert_eq!(
            expand_tilde_with("~bob/a", Some(home)),
            PathBuf::from("~bob/a")
        );
        assert_eq!(expand_tilde_with("/abs", Some(home)), PathBuf::from("/abs"));
        assert_eq!(expand_tilde_with("~/x", None), PathBuf::from("~/x"));
    }

    #[test]
    fn pathext_parsing() {
        assert_eq!(parse_pathext(".EXE;.CMD;"), vec!["EXE", "CMD"]);
        assert_eq!(parse_pathext(""), vec!["COM", "EXE", "BAT", "CMD"]);
    }

    #[cfg(unix)]
    #[test]
    fn finds_sh_on_path() {
        let found = find_on_path("sh", Some(OsStr::new("/nonexistent:/bin"))).expect("sh");
        assert_eq!(found, PathBuf::from("/bin/sh"));
        assert!(find_on_path("definitely-not-a-program", Some(OsStr::new("/bin"))).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn finds_cmd_on_path() {
        let found = find_on_path("cmd", None).expect("cmd.exe on PATH");
        assert!(found
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("cmd.exe"));
    }
}
