//! Session display names — naming rule zero: a session's name is the most
//! specific thing known about what it is DOING, never where it merely lives.
//!
//! Shells resolve as: foreground command while one is running ("snakemake")
//! -> workspace-relative cwd while idle ("results/qc") -> shell name at the
//! workspace root ("zsh"). An OSC title a running *program* sets (captured as
//! `SessionInfo::title`) wins over polled values — but a shell's own prompt
//! title ("user@host:~/dir") is ignored: it names where the shell lives, not
//! what it does, and duplicates the location the row already shows. An
//! explicitly pinned name (`SessionInfo::renamed`) wins over everything.
//! Agents resolve in `agents.rs` (customTitle > aiTitle > first prompt >
//! "claude").
//!
//! A per-shell-session watcher polls the PTY's foreground process (~2s):
//! the fg pid comes from `tcgetpgrp` on the PTY master (via portable-pty,
//! portable across unixes); its name and cwd come from `/proc` on Linux and
//! from libproc (`proc_name` / `PROC_PIDVNODEPATHINFO`) on macOS. Resolved
//! names land in `AppState::display_names`, the polled cwd in
//! `AppState::current_cwds` (the `cwd_current` session field, for the context
//! bridge); both nudge the events bus, which already throttles snapshot
//! pushes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::AppState;

/// Poll interval for the shell naming watcher: 2s in production, fast in
/// tests (same policy as the agent transcript tail).
fn poll_interval() -> Duration {
    if cfg!(test) {
        Duration::from_millis(50)
    } else {
        Duration::from_secs(2)
    }
}

/// Watch one shell session for its lifetime: poll the foreground process,
/// publish the resolved display name and current working directory on
/// change, and clean up (broadcasting a change) once the underlying PTY
/// session is gone.
pub(crate) fn spawn_shell_watch(state: Arc<AppState>, session_id: String) {
    tokio::spawn(async move {
        loop {
            let Some(info) = state.sessions.get(&session_id) else {
                let named = crate::lock(&state.display_names)
                    .remove(&session_id)
                    .is_some();
                let tracked = crate::lock(&state.current_cwds)
                    .remove(&session_id)
                    .is_some();
                if named || tracked {
                    state.changes.notify_waiters();
                }
                return;
            };

            let mut changed = false;

            // The shell's cwd, polled once per tick: feeds both the idle name
            // and the `cwd_current` field on session JSON (context bridge).
            let cwd = info
                .pid
                .and_then(|pid| i32::try_from(pid).ok())
                .and_then(proc_info::cwd);
            if let Some(cwd) = &cwd {
                let mut cwds = crate::lock(&state.current_cwds);
                if cwds.get(&session_id) != Some(cwd) {
                    cwds.insert(session_id.clone(), cwd.clone());
                    changed = true;
                }
            }

            if let Some(name) = resolve_shell_name(&state, &info, cwd) {
                let mut names = crate::lock(&state.display_names);
                if names.get(&session_id).map(String::as_str) != Some(name.as_str()) {
                    names.insert(session_id.clone(), name);
                    changed = true;
                }
            }

            if changed {
                state.changes.notify_waiters();
            }

            tokio::time::sleep(poll_interval()).await;
        }
    });
}

/// Effective display name for a shell session: OSC title when a running
/// program set a meaningful one, else the watcher's polled value, else the
/// spawn name. (`renamed` pins are applied by the caller, for all kinds.)
pub(crate) fn shell_display_name(info: &chimaera_pty::SessionInfo, polled: Option<&str>) -> String {
    // A program that titles itself ("vim README.md", "htop") names the session
    // better than we can poll — honor it. But an interactive shell also emits a
    // title from its prompt ("user@host:~/dir"); that is pure location (naming
    // rule zero says never name by that) and duplicates the row's cwd, so skip
    // it and let the polled foreground-command / cwd-relative name win.
    if let Some(title) = info
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .filter(|t| !is_shell_prompt_title(t))
    {
        return title.to_string();
    }
    polled
        .map(str::to_string)
        .unwrap_or_else(|| info.name.clone())
}

/// A shell prompt's default window title — "user@host", "user@host:~/dir", or
/// "user@host: ~/dir" — set by the shell itself, not by a running program.
/// Matched by the canonical "user@host" shape (no whitespace flanking the
/// `@`), which real program titles essentially never take; a command line that
/// happens to contain an address ("ssh user@host") keeps its title because the
/// user part then holds a space. A false negative only costs a
/// slightly-less-specific name for one poll cycle.
fn is_shell_prompt_title(title: &str) -> bool {
    let Some((user, rest)) = title.split_once('@') else {
        return false;
    };
    // The host runs up to the cwd separator (":"), a space, or end-of-title.
    let host = rest.split([':', ' ']).next().unwrap_or("");
    !user.is_empty()
        && !user.contains(char::is_whitespace)
        && !host.is_empty()
        && !host.contains(char::is_whitespace)
}

/// One poll: resolve what the shell is doing right now, per naming rule zero.
/// Executable name of a pid (exec-policy checks reuse the naming lookup).
pub(crate) fn comm_name(pid: i32) -> Option<String> {
    proc_info::comm(pid)
}

/// `polled_cwd` is the watcher's fresh cwd reading for this tick, if any.
fn resolve_shell_name(
    state: &AppState,
    info: &chimaera_pty::SessionInfo,
    polled_cwd: Option<PathBuf>,
) -> Option<String> {
    let child = i32::try_from(info.pid?).ok()?;
    let fg = state.sessions.foreground_pid(&info.id).unwrap_or(child);

    // A foreground process group other than the shell's own means a command
    // is running — its name is the most specific thing we know. But trust it
    // ONLY when `fg` is genuinely a descendant of this shell: `tcgetpgrp` can
    // transiently (right after spawn) report a pgid that, read back as a pid,
    // lands on an unrelated process — including one of the daemon's OWN tokio
    // worker THREADS (on Linux tids and pids share a namespace, so a stray
    // tid reads a "tokio-runtime-worker" comm). The descendant check keeps a
    // real foreground job ("snakemake") while rejecting those aliases, which
    // otherwise leaked as the session title.
    if fg != child && is_descendant(fg, child) {
        if let Some(comm) = proc_info::comm(fg) {
            return Some(comm);
        }
    }

    // Idle shell: name it by where it sits relative to the workspace root.
    let cwd = polled_cwd.unwrap_or_else(|| info.cwd.clone());
    let shell = proc_info::comm(child).unwrap_or_else(default_shell_name);
    match workspace_root(state, &info.id) {
        Some(root) => match cwd.strip_prefix(&root) {
            Ok(rel) if !rel.as_os_str().is_empty() => Some(rel.to_string_lossy().into_owned()),
            Ok(_) => Some(shell),               // at the root: the shell itself
            Err(_) => Some(display_path(&cwd)), // wandered outside the workspace
        },
        None => Some(shell),
    }
}

/// Is `pid` the shell `child`, or a descendant of it? Walk the ppid chain up
/// from `pid` (bounded, so a cycle from a recycled pid can't loop). A real
/// foreground job always traces back to the shell; a stray pid/tid the tty
/// briefly reported (e.g. a daemon tokio worker thread) does not. When ppid
/// lookups are unavailable (unsupported platform), assume true so naming
/// degrades to today's behavior rather than losing every command name.
fn is_descendant(pid: i32, child: i32) -> bool {
    if pid == child {
        return true;
    }
    let mut cur = pid;
    for _ in 0..32 {
        match proc_info::ppid(cur) {
            Some(parent) if parent == child => return true,
            Some(parent) if parent <= 1 => return false, // reached init/kernel
            Some(parent) => cur = parent,
            None => return true, // unknowable → don't discard a real name
        }
    }
    false
}

/// Workspace root for a session, via the session -> workspace map.
fn workspace_root(state: &AppState, session_id: &str) -> Option<PathBuf> {
    let ws_id = crate::lock(&state.session_workspaces)
        .get(session_id)
        .cloned()?;
    crate::lock(&state.workspaces).get(&ws_id).map(|w| w.root)
}

/// Basename of `$SHELL`, the same binary shell sessions spawn by default.
pub(crate) fn default_shell_name() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|s| {
            PathBuf::from(s)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "shell".to_string())
}

/// Human path for a cwd outside the workspace: home-relative when possible.
fn display_path(path: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if let Ok(rel) = path.strip_prefix(&home) {
            return if rel.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", rel.display())
            };
        }
    }
    path.display().to_string()
}

/// Process name + cwd lookups, Linux flavor: straight out of `/proc`.
#[cfg(target_os = "linux")]
mod proc_info {
    use std::path::PathBuf;

    /// Executable name (`/proc/<pid>/comm`) of `pid`.
    pub(super) fn comm(pid: i32) -> Option<String> {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        let comm = comm.trim();
        (!comm.is_empty()).then(|| comm.to_string())
    }

    /// Current working directory (`/proc/<pid>/cwd`) of `pid`.
    pub(super) fn cwd(pid: i32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    /// Parent pid (`/proc/<pid>/stat` field 4). The comm field (2) is
    /// parenthesized and may itself contain spaces/parens, so split on the
    /// LAST ')' before reading the space-separated fields after it.
    pub(super) fn ppid(pid: i32) -> Option<i32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after = &stat[stat.rfind(')')? + 1..];
        // Fields after comm: state ppid ... — ppid is the 2nd token here.
        after.split_whitespace().nth(1)?.parse().ok()
    }
}

/// Process name + cwd lookups, macOS flavor: libproc (`proc_name` for the
/// name, `proc_pidinfo(PROC_PIDVNODEPATHINFO)` for the cwd).
#[cfg(target_os = "macos")]
mod proc_info {
    use std::path::PathBuf;

    use libproc::libproc::bsd_info::BSDInfo;
    use libproc::libproc::proc_pid::{name, pidinfo, PIDInfo, PidInfoFlavor};

    const MAXPATHLEN: usize = 1024;

    /// Darwin `struct vnode_info_path`: an opaque `struct vnode_info`
    /// (a 136-byte `vinfo_stat` + type/pad/fsid, 152 bytes total) followed
    /// by the path. Layout verified against `<sys/proc_info.h>`
    /// (sizeof 1176, path offset 152).
    #[repr(C)]
    struct VnodeInfoPath {
        _vi: [u8; 152],
        vip_path: [u8; MAXPATHLEN],
    }

    /// Darwin `struct proc_vnodepathinfo` (flavor `PROC_PIDVNODEPATHINFO`):
    /// cwd + root dir vnodes. Layout verified: sizeof 2352.
    #[repr(C)]
    struct VnodePathInfo {
        pvi_cdir: VnodeInfoPath,
        _pvi_rdir: VnodeInfoPath,
    }

    impl PIDInfo for VnodePathInfo {
        fn flavor() -> PidInfoFlavor {
            PidInfoFlavor::VNodePathInfo
        }
    }

    /// Executable name of `pid` (libproc `proc_name`).
    pub(super) fn comm(pid: i32) -> Option<String> {
        let comm = name(pid).ok()?;
        let comm = comm.trim();
        (!comm.is_empty()).then(|| comm.to_string())
    }

    /// Current working directory of `pid` (`PROC_PIDVNODEPATHINFO`).
    pub(super) fn cwd(pid: i32) -> Option<PathBuf> {
        let info = pidinfo::<VnodePathInfo>(pid, 0).ok()?;
        let path = &info.pvi_cdir.vip_path;
        let len = path.iter().position(|&b| b == 0).unwrap_or(path.len());
        let path = std::str::from_utf8(&path[..len]).ok()?;
        (!path.is_empty()).then(|| PathBuf::from(path))
    }

    /// Parent pid of `pid` (libproc `PROC_PIDTBSDINFO` → `pbi_ppid`).
    pub(super) fn ppid(pid: i32) -> Option<i32> {
        let info = pidinfo::<BSDInfo>(pid, 0).ok()?;
        i32::try_from(info.pbi_ppid).ok()
    }
}

/// Fallback for other platforms: no foreground introspection; the watcher
/// then keeps the title/name fallbacks from `shell_display_name`.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod proc_info {
    use std::path::PathBuf;

    pub(super) fn comm(_pid: i32) -> Option<String> {
        None
    }

    pub(super) fn cwd(_pid: i32) -> Option<PathBuf> {
        None
    }

    pub(super) fn ppid(_pid: i32) -> Option<i32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(title: Option<&str>) -> chimaera_pty::SessionInfo {
        chimaera_pty::SessionInfo {
            id: "s1".to_string(),
            name: "zsh".to_string(),
            cwd: PathBuf::from("/tmp"),
            cols: 80,
            rows: 24,
            created_at: 0,
            alive: true,
            exit_status: None,
            title: title.map(str::to_string),
            pid: None,
            renamed: false,
            phase: chimaera_pty::ShellPhase::Unknown,
            last_output_at: 0,
        }
    }

    #[test]
    fn prompt_titles_are_recognized() {
        for t in [
            "mkjellbe@smsh11dsu-srcf-d15-37:~",
            "mkjellbe@smsh11dsu-srcf-d15-37:~/pd_project/notes",
            "user@host: ~/dir",
            "me@laptop",
        ] {
            assert!(is_shell_prompt_title(t), "{t:?} should read as a prompt");
        }
    }

    #[test]
    fn program_titles_are_not_prompts() {
        // Program-set titles (and command lines carrying an address) are the
        // most specific signal we have — never mistaken for a prompt.
        for t in ["vim README.md", "htop", "npm run dev", "ssh user@host", ""] {
            assert!(!is_shell_prompt_title(t), "{t:?} should be kept");
        }
    }

    #[test]
    fn shell_display_name_drops_the_prompt_title() {
        // A prompt title is ignored: the polled cwd-relative name wins...
        assert_eq!(
            shell_display_name(
                &info(Some("mkjellbe@host:~/pd_project/notes")),
                Some("notes")
            ),
            "notes"
        );
        // ...but a program's own title still outranks the polled value.
        assert_eq!(
            shell_display_name(&info(Some("vim README.md")), Some("vim")),
            "vim README.md"
        );
        // With no usable title and no poll yet, fall back to the spawn name.
        assert_eq!(shell_display_name(&info(Some("me@box:~")), None), "zsh");
    }
}
