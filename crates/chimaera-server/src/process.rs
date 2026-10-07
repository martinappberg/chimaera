//! Auth subprocesses never forward their output to logs or HTTP errors.
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

pub const LIMIT: usize = 64 * 1024;
pub const TIMEOUT: Duration = Duration::from_secs(8);
pub struct Child {
    pub child: tokio::process::Child,
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
}
impl Child {
    pub fn spawn(command: &mut tokio::process::Command) -> Result<Self, &'static str> {
        #[cfg(unix)]
        command.process_group(0);
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
pub(crate) fn command(bin: &Path, args: &[&str], cwd: &Path) -> tokio::process::Command {
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
    let mut child = Child::spawn(cmd)?;
    if let Some(pid) = child.child.id() {
        started(pid);
    }
    let stdout = child.child.stdout.take().ok_or("start_failed")?;
    let stderr = child.child.stderr.take().ok_or("start_failed")?;
    let work = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut stdout = stdout.take((LIMIT + 1) as u64);
        let mut stderr = stderr.take((LIMIT + 1) as u64);
        let drain = async {
            let (a, b) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
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
}
