//! Bounded subprocesses: auth helpers (whose output never reaches logs or
//! HTTP errors) and short detection probes (login-shell `command -v`,
//! `--version`). Each runs in its own process group, killed whole.
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

pub const LIMIT: usize = 64 * 1024;
pub const TIMEOUT: Duration = Duration::from_secs(8);
pub struct Child {
    /// The extension drives the inner child itself (stdin, wait, kill), so
    /// this stays public: it is part of the contract the private side builds on.
    pub child: tokio::process::Child,
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
}
impl Child {
    pub fn spawn(command: &mut tokio::process::Command) -> Result<Self, &'static str> {
        #[cfg(unix)]
        command.process_group(0);
        Self::start(command)
    }

    /// Like [`Child::spawn`], but in a new session with no controlling
    /// terminal. An interactive (`-i`) shell opens `/dev/tty` and starts job
    /// control: outside the terminal's foreground group bash stops itself
    /// with SIGTTIN until the timeout, and zsh may take the terminal away
    /// from a daemon running in the foreground. A session leader's group id
    /// is its pid, so the group kill below is unchanged.
    pub fn spawn_session(command: &mut tokio::process::Command) -> Result<Self, &'static str> {
        #[cfg(unix)]
        // SAFETY: setsid is a single async-signal-safe syscall; nothing is
        // allocated or locked between fork and exec.
        unsafe {
            command.pre_exec(|| {
                rustix::process::setsid()
                    .map(|_| ())
                    .map_err(std::io::Error::from)
            });
        }
        Self::start(command)
    }

    fn start(command: &mut tokio::process::Command) -> Result<Self, &'static str> {
        let child = command
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "start_failed")?;
        #[cfg(unix)]
        let group = child
            .id()
            .and_then(|id| rustix::process::Pid::from_raw(id as i32));
        Ok(Self {
            child,
            #[cfg(unix)]
            group,
        })
    }

    /// Keep the leader waitable until the final group signal: reaping first
    /// would permit its PID to be reused by an unrelated process group.
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(unix)]
        {
            use rustix::process::{waitid, WaitId, WaitIdOptions};
            if let Some(group) = self.group {
                let mut changed =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
                loop {
                    match waitid(
                        WaitId::Pid(group),
                        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                    ) {
                        Ok(Some(_)) => break,
                        Ok(None) => {}
                        Err(rustix::io::Errno::INTR) => continue,
                        Err(error) => return Err(error.into()),
                    }
                    tokio::select! {
                        _ = changed.recv() => {},
                        _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                    }
                }
                // No await between observing completion, signalling, and clearing
                // the identity. Cancellation afterward cannot signal a reaped PID.
                self.stop_group();
            }
        }
        self.child.wait().await
    }

    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.child.stderr.take()
    }

    #[cfg(unix)]
    fn stop_group(&mut self) {
        if let Some(group) = self.group.take() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        #[cfg(unix)]
        self.stop_group();
        let _ = self.child.start_kill();
    }
}
pub fn command(bin: &Path, args: &[&str], cwd: &Path) -> tokio::process::Command {
    let argv = crate::launcher::wrap_login_shell(
        &crate::launcher::login_shell(),
        std::iter::once(bin.to_string_lossy().into_owned())
            .chain(args.iter().map(|s| s.to_string()))
            .collect(),
    );
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        // The worker's explicit provider home owns auth. A different ambient
        // desktop HOME/config override must never redirect an auth mutation.
        // Claude keeps its default layout under that HOME (`~/.claude.json`
        // beside `~/.claude/`), exactly what its sessions read: a
        // CLAUDE_CONFIG_DIR here would move its global config to
        // `~/.claude/.claude.json`, so sign-in and sessions would disagree.
        .env("HOME", cwd)
        .env("CODEX_HOME", cwd.join(".codex"))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("GH_CONFIG_DIR", cwd.join(".config/gh"))
        .env("XDG_CONFIG_HOME", cwd.join(".config"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in crate::api::launcher_context_env() {
        cmd.env_remove(name);
    }
    cmd
}
pub struct Output {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
}
pub async fn output(cmd: &mut tokio::process::Command) -> Result<Output, &'static str> {
    output_tracked(cmd, |_| {}).await
}
pub async fn output_tracked(
    cmd: &mut tokio::process::Command,
    started: impl FnOnce(u32),
) -> Result<Output, &'static str> {
    let child = Child::spawn(cmd)?;
    if let Some(pid) = child.id() {
        started(pid);
    }
    collect(child).await
}

/// One short probe (a login shell's `command -v`, a CLI's `--version`) run to
/// completion or `limit`, `None` on any failure. It runs in a new session
/// ([`Child::spawn_session`]), killed whole when its leader exits or when
/// this future is dropped (timeout, daemon shutdown): an interactive rc (nvm,
/// prompt frameworks) or a node-backed CLI leaves helpers behind that
/// `kill_on_drop` (the leader alone) never reaches. stderr is discarded:
/// nothing reads it, and a chatty rc must not fail detection on the cap.
pub async fn probe_output(cmd: &mut tokio::process::Command, limit: Duration) -> Option<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = Child::spawn_session(cmd).ok()?;
    tokio::time::timeout(limit, collect(child)).await.ok()?.ok()
}

async fn collect(mut child: Child) -> Result<Output, &'static str> {
    let stdout = child.take_stdout().ok_or("start_failed")?;
    let stderr = child.take_stderr();
    let work = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut stdout = stdout.take((LIMIT + 1) as u64);
        let drain = async {
            let read_err = async {
                match stderr {
                    Some(stderr) => stderr.take((LIMIT + 1) as u64).read_to_end(&mut err).await,
                    None => Ok(0),
                }
            };
            let (a, b) = tokio::join!(stdout.read_to_end(&mut out), read_err);
            a.map_err(|_| "probe_failed")?;
            b.map_err(|_| "probe_failed")?;
            if out.len() > LIMIT || err.len() > LIMIT {
                return Err("output_limit");
            }
            Ok(())
        };
        let (_, status) = tokio::try_join!(drain, async {
            child.wait().await.map_err(|_| "probe_failed")
        })?;
        Ok(Output {
            success: status.success(),
            code: status.code(),
            stdout: out,
        })
    };
    tokio::time::timeout(TIMEOUT, work)
        .await
        .map_err(|_| "probe_timeout")?
}

/// Only used for a child process group that this service created and killed.
/// A reused PID makes cleanup conservatively fail closed rather than releasing
/// the provider's single-writer reservation while its status is uncertain.
pub fn group_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        rustix::process::Pid::from_raw(pid as i32).is_some_and(|pid| {
            !matches!(
                rustix::process::test_kill_process_group(pid),
                Err(rustix::io::Errno::SRCH)
            )
        })
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exited_leader_releases_descendant_pipes_before_the_output_deadline() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "sleep 60 & printf done; exit 7"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = tokio::time::timeout(Duration::from_secs(3), output(&mut command))
            .await
            .expect("the leader's exit must close inherited pipes")
            .unwrap();
        assert!(!output.success);
        assert_eq!(output.code, Some(7));
        assert_eq!(output.stdout, b"done");
    }

    #[tokio::test]
    async fn wait_clears_group_identity_before_reaping_and_supports_repeated_wait() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "exit 3"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = Child::spawn(&mut command).unwrap();
        assert!(child.group.is_some());
        assert_eq!(child.wait().await.unwrap().code(), Some(3));
        assert!(
            child.group.is_none(),
            "Drop must not signal a reaped process group"
        );
        assert!(child.child.id().is_none());
        assert_eq!(child.wait().await.unwrap().code(), Some(3));
    }

    #[tokio::test]
    async fn a_probe_runs_in_its_own_session_without_a_controlling_terminal() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "sleep 5"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = Child::spawn_session(&mut command).unwrap();
        let pid = rustix::process::Pid::from_raw(child.id().unwrap() as i32).unwrap();
        let own = rustix::process::getsid(None).unwrap();
        let probe = rustix::process::getsid(Some(pid)).unwrap();
        assert_ne!(probe, own, "a probe must not share the daemon's session");
        assert_eq!(probe, pid, "the probe leads its own session");
        assert_eq!(rustix::process::getpgid(Some(pid)).unwrap(), pid);
        drop(child);

        // Whether or not this test runs under a terminal, a probe cannot open
        // one: an interactive rc has no terminal to fight the daemon for.
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "if (: </dev/tty) 2>/dev/null; then echo tty; else echo none; fi",
        ]);
        let out = probe_output(&mut command, Duration::from_secs(3))
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "none");
    }

    #[tokio::test]
    async fn a_probe_ignores_stderr_past_the_output_cap() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "i=0; while [ $i -lt 4096 ]; do printf '%0100d\\n' 0 >&2; i=$((i+1)); done; echo /bin/found",
        ]);
        let out = probe_output(&mut command, Duration::from_secs(5))
            .await
            .expect("a noisy rc on stderr must not fail the probe");
        assert!(out.success);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "/bin/found");
    }
}
