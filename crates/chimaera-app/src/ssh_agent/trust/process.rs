//! Native probe children remain foreground; their owned group is killed before
//! reaping the leader, including observer cancellation and inherited askpass.
use super::super::selection::SelectionFailure;
use std::{
    os::unix::process::CommandExt,
    process::Stdio,
    sync::{Arc, LazyLock},
};
use tokio::{
    process::{Child, Command},
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

static SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));
pub(super) struct Process(Option<Child>, Option<OwnedSemaphorePermit>);
impl Process {
    pub(super) fn pid(&self) -> Result<u32, SelectionFailure> {
        self.0
            .as_ref()
            .and_then(Child::id)
            .ok_or(SelectionFailure::Unavailable)
    }
    pub(super) fn spawn(mut command: Command) -> Result<Self, SelectionFailure> {
        let permit = SLOTS
            .clone()
            .try_acquire_owned()
            .map_err(|_| SelectionFailure::Unavailable)?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        command
            .spawn()
            .map(|child| Self(Some(child), Some(permit)))
            .map_err(|_| SelectionFailure::Unavailable)
    }
    pub(super) fn exited(&self) -> Result<bool, SelectionFailure> {
        let id = self
            .0
            .as_ref()
            .and_then(Child::id)
            .ok_or(SelectionFailure::Unavailable)?;
        // nix exposes waitid only on selected platforms; both supported
        // native Unix targets provide the libc WNOWAIT receipt. The leader is
        // deliberately not reaped before its process-group cleanup.
        let mut info = std::mem::MaybeUninit::<nix::libc::siginfo_t>::zeroed();
        let result = unsafe {
            nix::libc::waitid(
                nix::libc::P_PID,
                id,
                info.as_mut_ptr(),
                nix::libc::WEXITED | nix::libc::WNOHANG | nix::libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(SelectionFailure::Unavailable);
        }
        let info = unsafe { info.assume_init() };
        if info.si_signo == 0 {
            return Ok(false);
        }
        if info.si_signo != nix::libc::SIGCHLD || unsafe { info.si_pid() } != id as i32 {
            return Err(SelectionFailure::Unavailable);
        }
        Ok(true)
    }
    fn kill(child: &mut Child) {
        if let Some(id) = child.id() {
            unsafe {
                nix::libc::kill(-(id as i32), nix::libc::SIGKILL);
            }
        }
        let _ = child.start_kill();
    }
    pub(super) async fn stop(mut self, deadline: Instant) -> Result<(), SelectionFailure> {
        let mut child = self.0.take().ok_or(SelectionFailure::Unavailable)?;
        Self::kill(&mut child);
        let permit = self.1.take();
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _permit = permit;
            let result = child.wait().await;
            let _ = send.send(result);
        });
        // A failed/aborted observer cannot free capacity before actual reaping.
        tokio::time::timeout_at(deadline, receive)
            .await
            .map_err(|_| SelectionFailure::Unavailable)?
            .map_err(|_| SelectionFailure::Unavailable)?
            .map_err(|_| SelectionFailure::Unavailable)?;
        Ok(())
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            Self::kill(&mut child);
            let permit = self.1.take();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _permit = permit;
                    let _ = child.wait().await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn exited_foreground_leader_remains_pinned_until_descendant_group_cleanup() {
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let marker = root.join(format!(
            "cx-native-probe-child-{}",
            &chimaera_core::generate_token()[..20]
        ));
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "/bin/sleep 30 & printf '%s' \"$!\" > \"$1\"; exit 0",
                "fixture",
            ])
            .arg(&marker)
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        let process = Process::spawn(command).unwrap();
        let leader = process.pid().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        while !marker.exists() || !process.exited().unwrap() {
            assert!(Instant::now() < deadline, "owned probe did not exit");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let child = std::fs::read_to_string(&marker)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        // WNOWAIT keeps the exited leader's identity unavailable for reuse.
        assert_eq!(unsafe { nix::libc::kill(leader as i32, 0) }, 0);
        assert_eq!(unsafe { nix::libc::kill(child, 0) }, 0);
        process.stop(deadline).await.unwrap();
        let gone = unsafe { nix::libc::kill(leader as i32, 0) } != 0;
        assert!(gone);
        // The descendant may briefly remain a nonrunning zombie awaiting the
        // system reaper. Its live execution is forbidden by the group SIGKILL.
        #[cfg(target_os = "macos")]
        {
            while unsafe { nix::libc::kill(child, 0) } == 0 {
                assert!(Instant::now() < deadline, "owned descendant was not reaped");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        std::fs::remove_file(marker).unwrap();
    }
}
