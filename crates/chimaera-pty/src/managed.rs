//! Opt-in process-group lifecycle. The direct child's PID stays unreaped while
//! groups are signalled, so it cannot be recycled into an unrelated process.
use crate::lock_unpoisoned;
use portable_pty::{Child, ExitStatus, MasterPty};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

pub(crate) struct Managed {
    state: Mutex<State>,
    pub closed: AtomicBool,
}
struct State {
    pid: Option<u32>,
    stopped: Option<Instant>,
}
impl Managed {
    pub fn new(pid: Option<u32>) -> Self {
        Self {
            state: Mutex::new(State { pid, stopped: None }),
            closed: AtomicBool::new(false),
        }
    }
    pub fn fence(&self, master: &Mutex<Box<dyn MasterPty + Send>>) {
        self.closed.store(true, Ordering::Release);
        let mut state = lock_unpoisoned(&self.state);
        let at = *state.stopped.get_or_insert_with(Instant::now);
        if let Some(pid) = state.pid {
            signal(pid, master, at.elapsed() >= Duration::from_secs(2));
        }
    }
    pub fn wait(
        &self,
        child: &mut dyn Child,
        master: &Mutex<Box<dyn MasterPty + Send>>,
    ) -> std::io::Result<ExitStatus> {
        loop {
            {
                let mut state = lock_unpoisoned(&self.state);
                if let Some(pid) = state.pid {
                    if exited_unreaped(pid)? {
                        self.closed.store(true, Ordering::Release);
                        signal(pid, master, true);
                        // WNOWAIT proved completion without releasing the PID;
                        // this final wait is nonblocking and shares the fence lock.
                        let result = child.wait();
                        state.pid = None;
                        return result;
                    }
                    if state
                        .stopped
                        .is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
                    {
                        signal(pid, master, true);
                    }
                } else {
                    return child.wait();
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
#[cfg(unix)]
fn signal(pid: u32, master: &Mutex<Box<dyn MasterPty + Send>>, force: bool) {
    use nix::{
        sys::signal::{killpg, Signal},
        unistd::{getsid, Pid},
    };
    let owner = Pid::from_raw(pid as i32);
    let signal = if force {
        Signal::SIGKILL
    } else {
        Signal::SIGTERM
    };
    if let Some(foreground) = lock_unpoisoned(master)
        .process_group_leader()
        .filter(|pid| *pid > 0)
    {
        let foreground = Pid::from_raw(foreground);
        if getsid(Some(foreground)).ok() == Some(owner) {
            let _ = killpg(foreground, signal);
        }
    }
    let _ = killpg(owner, signal);
}
#[cfg(not(unix))]
fn signal(_: u32, _: &Mutex<Box<dyn MasterPty + Send>>, _: bool) {}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exited_unreaped(pid: u32) -> std::io::Result<bool> {
    let mut info = std::mem::MaybeUninit::<nix::libc::siginfo_t>::zeroed();
    // Only our direct child is queried; WNOWAIT deliberately retains its PID.
    let result = unsafe {
        nix::libc::waitid(
            nix::libc::P_PID,
            pid,
            info.as_mut_ptr(),
            nix::libc::WEXITED | nix::libc::WNOHANG | nix::libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let info = unsafe { info.assume_init() };
    #[cfg(target_os = "macos")]
    let observed = info.si_pid;
    #[cfg(target_os = "linux")]
    let observed = unsafe { info.si_pid() };
    Ok(observed == pid as i32)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exited_unreaped(_: u32) -> std::io::Result<bool> {
    Err(std::io::Error::other(
        "managed execution unsupported on this platform",
    ))
}
