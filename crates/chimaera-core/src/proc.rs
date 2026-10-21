//! Process trees and liveness, one implementation per platform.
//!
//! Unix: a child spawned with `process_group(0)` leads its own process group,
//! and the tree is signalled through the group. Windows has neither process
//! groups that can be signalled nor SIGTERM: the tree is a Job Object the
//! child is assigned to right after spawn, and ending the job ends every
//! process in it, grandchildren included.
//!
//! Call sites keep their spawn line per platform:
//!
//! ```ignore
//! #[cfg(unix)]
//! cmd.process_group(0);
//! #[cfg(windows)]
//! cmd.creation_flags(chimaera_core::proc::TREE_CREATION_FLAGS);
//! let child = cmd.spawn()?;
//! let tree = chimaera_core::proc::ProcessTree::adopt(child.id().unwrap())?;
//! ```

use std::io;

/// Creation flags for a child that will own its own kill tree on Windows: no
/// console window flashes for a console-subsystem child of a windowless
/// daemon, and the child leads a new process group, so a console control
/// event aimed at the daemon never reaches it.
#[cfg(windows)]
pub const TREE_CREATION_FLAGS: u32 = crate::CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;

/// `CREATE_NEW_PROCESS_GROUP` (winbase.h).
#[cfg(windows)]
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// `DETACHED_PROCESS` (winbase.h): the child gets no console at all, for a
/// process that must outlive the console that started it (the daemon).
#[cfg(windows)]
pub const DETACHED_PROCESS: u32 = 0x0000_0008;

/// `CREATE_BREAKAWAY_FROM_JOB` (winbase.h).
#[cfg(windows)]
pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Creation flags for a process that must outlive whoever started it (the
/// daemon): no console, its own process group, no window.
#[cfg(windows)]
pub const DETACHED_CREATION_FLAGS: u32 =
    DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | crate::CREATE_NO_WINDOW;

/// Spawn `cmd` detached ([`DETACHED_CREATION_FLAGS`]), outside its parent's
/// job when that is allowed: a terminal, IDE or ssh session that runs its
/// children in a kill-on-close job would otherwise take the daemon down
/// when it closes. A job that forbids breakaway refuses the flag with
/// `ERROR_ACCESS_DENIED`; the spawn is then retried inside it.
#[cfg(windows)]
pub fn spawn_detached(cmd: &mut std::process::Command) -> io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    /// `ERROR_ACCESS_DENIED` (winerror.h).
    const ERROR_ACCESS_DENIED: i32 = 5;
    cmd.creation_flags(DETACHED_CREATION_FLAGS | CREATE_BREAKAWAY_FROM_JOB);
    match cmd.spawn() {
        Err(err) if err.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
            cmd.creation_flags(DETACHED_CREATION_FLAGS);
            cmd.spawn()
        }
        spawned => spawned,
    }
}

/// The kill tree of one spawned child.
///
/// Dropping it never kills anything, on either platform: the call sites that
/// need a kill-on-drop guard say so explicitly, as they did with process
/// groups. On Windows the job is created without kill-on-close for the same
/// reason; closing the handle only releases it.
pub struct ProcessTree {
    #[cfg(unix)]
    pgid: i32,
    #[cfg(windows)]
    job: windows::OwnedHandle,
}

impl ProcessTree {
    /// Take the tree rooted at `pid`, a child spawned a moment ago with the
    /// platform's tree spawn line (see the module docs).
    ///
    /// Windows assigns the child to a fresh job here, so a grandchild it
    /// started before this call is outside the job. Call this straight after
    /// `spawn`.
    pub fn adopt(pid: u32) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let pgid = i32::try_from(pid).map_err(|_| io::Error::other("pid out of range"))?;
            Ok(Self { pgid })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                job: windows::job_for(pid)?,
            })
        }
    }

    /// Ask the tree to stop. Unix: SIGTERM to the group, so handlers such as
    /// an installer's EXIT trap or an agent's cleanup run. Windows has no
    /// polite signal that reaches a console-less child, so this returns
    /// `false` and does nothing; the caller's grace (stdin closed, a bounded
    /// wait) is the only notice the tree gets before [`ProcessTree::kill`].
    pub fn terminate(&self) -> bool {
        #[cfg(unix)]
        {
            signal_group(self.pgid, nix::sys::signal::Signal::SIGTERM)
        }
        #[cfg(windows)]
        {
            false
        }
    }

    /// End every process in the tree now. Unix: SIGKILL to the group.
    /// Windows: terminate the job.
    pub fn kill(&self) {
        #[cfg(unix)]
        {
            signal_group(self.pgid, nix::sys::signal::Signal::SIGKILL);
        }
        #[cfg(windows)]
        {
            windows::terminate_job(&self.job);
        }
    }
}

/// Resource limits for a whole tree, applied by [`ProcessTree::limit`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobLimits {
    /// Committed memory any one process in the tree may hold, in bytes (the
    /// counterpart of `ulimit -v`, which is per process too).
    pub memory_bytes: Option<u64>,
    /// User-mode CPU time the whole tree may use, in seconds. Unlike
    /// `ulimit -t` it is shared by every process in the tree, so a child
    /// can't reset the budget by forking.
    pub cpu_seconds: Option<u64>,
    /// Run every process in the tree at below-normal priority (`nice`).
    pub below_normal_priority: bool,
}

impl ProcessTree {
    /// Put `limits` on every process in the tree, the ones started later
    /// included. Windows only: the job's extended limits, and a process that
    /// passes one is ended by the system. Unix sets its limits in the child
    /// before the program starts (a `ulimit` preamble), so this is a no-op
    /// there. There is no per-file size limit on Windows.
    ///
    /// The tree runs unlimited between spawn and this call; call it straight
    /// after [`ProcessTree::adopt`].
    pub fn limit(&self, limits: JobLimits) -> io::Result<()> {
        #[cfg(unix)]
        {
            let _ = limits;
            Ok(())
        }
        #[cfg(windows)]
        {
            windows::limit_job(&self.job, limits)
        }
    }

    /// End the tree when this handle goes away — dropped, or its owner
    /// exits or crashes. That is what a terminal's processes get on Unix
    /// from the PTY hangup when the daemon closes or loses the master; a
    /// ConPTY client has no hangup and would outlive the daemon. Windows
    /// only (the job's kill-on-close limit); a no-op on Unix. Overrides the
    /// "dropping never kills" default, so only a tree whose life is its
    /// owner's should ask for it.
    pub fn end_with_owner(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            Ok(())
        }
        #[cfg(windows)]
        {
            windows::kill_on_close(&self.job)
        }
    }
}

#[cfg(unix)]
fn signal_group(pgid: i32, signal: nix::sys::signal::Signal) -> bool {
    nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), signal).is_ok()
}

/// Whether a process with this pid exists. Unix: a signal-0 probe, where
/// EPERM (someone else's process) still means alive. Windows: the process can
/// be opened and has not exited. A recycled pid reads as alive on both;
/// callers that care compare a start time as well.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
            Ok(()) => true,
            Err(nix::errno::Errno::EPERM) => true,
            Err(_) => false,
        }
    }
    #[cfg(windows)]
    {
        windows::pid_alive(pid)
    }
}

/// Ask one process (not its tree) to stop. Unix: SIGTERM. Windows: no polite
/// signal exists for a console-less process, so this returns `false`.
pub fn terminate_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGTERM,
        )
        .is_ok()
    }
    #[cfg(windows)]
    {
        let _ = pid;
        false
    }
}

/// End one process (not its tree) now. Unix: SIGKILL. Windows:
/// `TerminateProcess`. Returns whether the request was delivered.
pub fn kill_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        )
        .is_ok()
    }
    #[cfg(windows)]
    {
        windows::kill_pid(pid)
    }
}

#[cfg(windows)]
mod windows {
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_JOB_TIME,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PRIORITY_CLASS,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, TerminateProcess, BELOW_NORMAL_PRIORITY_CLASS,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// `STILL_ACTIVE` (minwinbase.h): the exit code of a running process.
    const STILL_ACTIVE: u32 = 259;

    /// A Win32 handle closed on drop.
    pub(super) struct OwnedHandle(HANDLE);

    // SAFETY: a kernel object handle is a process-wide value; the job and
    // process APIs used here are thread-safe on it.
    unsafe impl Send for OwnedHandle {}
    unsafe impl Sync for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful open/create call and
            // is closed exactly once, here.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open_process(pid: u32, access: u32) -> Option<OwnedHandle> {
        // SAFETY: plain call; a null return is the failure case.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        (!handle.is_null()).then_some(OwnedHandle(handle))
    }

    pub(super) fn job_for(pid: u32) -> io::Result<OwnedHandle> {
        let process = open_process(pid, PROCESS_SET_QUOTA | PROCESS_TERMINATE)
            .ok_or_else(io::Error::last_os_error)?;
        // SAFETY: default security, unnamed job; null is the failure case.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle(job);
        // SAFETY: both handles are open for the duration of the call.
        if unsafe { AssignProcessToJobObject(job.0, process.0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub(super) fn limit_job(job: &OwnedHandle, limits: super::JobLimits) -> io::Result<()> {
        if limits.memory_bytes.is_none()
            && limits.cpu_seconds.is_none()
            && !limits.below_normal_priority
        {
            return Ok(());
        }
        // Start from the job's current limits so one set earlier (kill on
        // close) survives this write.
        let mut info = query_limits(job)?;
        let basic = &mut info.BasicLimitInformation;
        if let Some(bytes) = limits.memory_bytes {
            basic.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            info.ProcessMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
        }
        if let Some(secs) = limits.cpu_seconds {
            basic.LimitFlags |= JOB_OBJECT_LIMIT_JOB_TIME;
            // In 100 ns ticks.
            basic.PerJobUserTimeLimit =
                i64::try_from(secs.saturating_mul(10_000_000)).unwrap_or(i64::MAX);
        }
        if limits.below_normal_priority {
            basic.LimitFlags |= JOB_OBJECT_LIMIT_PRIORITY_CLASS;
            basic.PriorityClass = BELOW_NORMAL_PRIORITY_CLASS;
        }
        // SAFETY: the job handle is open; `info` is the struct this class
        // takes and outlives the call.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The job's current extended limits, so a write can add to them.
    fn query_limits(job: &OwnedHandle) -> io::Result<JOBOBJECT_EXTENDED_LIMIT_INFORMATION> {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: the job handle is open; `info` is the struct this class
        // fills and outlives the call.
        let ok = unsafe {
            QueryInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_mut(&mut info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info)
    }

    pub(super) fn kill_on_close(job: &OwnedHandle) -> io::Result<()> {
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        // Read first so limits set earlier (`limit_job`) survive the write.
        let mut info = query_limits(job)?;
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: as above.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn terminate_job(job: &OwnedHandle) {
        // SAFETY: the job handle is open; a failure (already empty) is moot.
        unsafe { TerminateJobObject(job.0, 1) };
    }

    pub(super) fn pid_alive(pid: u32) -> bool {
        let Some(process) = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
            return false;
        };
        let mut code = 0u32;
        // SAFETY: the handle is open and `code` outlives the call.
        let ok = unsafe { GetExitCodeProcess(process.0, &mut code) } != 0;
        ok && code == STILL_ACTIVE
    }

    pub(super) fn kill_pid(pid: u32) -> bool {
        let Some(process) = open_process(pid, PROCESS_TERMINATE) else {
            return false;
        };
        // SAFETY: the handle is open with terminate rights.
        unsafe { TerminateProcess(process.0, 1) != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_pid_is_alive() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn a_reaped_child_is_not_alive() {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "exit 0"]);
            c
        } else {
            std::process::Command::new("true")
        };
        let mut child = cmd.spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("wait");
        assert!(!pid_alive(pid));
    }

    #[cfg(windows)]
    #[test]
    fn a_tree_that_ends_with_its_owner_dies_on_drop() {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "ping -n 30 127.0.0.1 >NUL"]);
        cmd.creation_flags(TREE_CREATION_FLAGS);
        let mut child = cmd.spawn().expect("spawn");
        let tree = ProcessTree::adopt(child.id()).expect("adopt");
        tree.end_with_owner().expect("kill on close");
        let started = std::time::Instant::now();
        drop(tree);
        // A kill-on-close job ends its processes with exit code 0, so the
        // proof is that ping's 29 s never ran.
        child.wait().expect("wait");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn killing_a_tree_ends_its_leader() {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "ping -n 30 127.0.0.1 >NUL"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            c
        };
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(TREE_CREATION_FLAGS);
        }
        let mut child = cmd.spawn().expect("spawn");
        let tree = ProcessTree::adopt(child.id()).expect("adopt");
        tree.kill();
        let status = child.wait().expect("wait");
        assert!(!status.success());
    }

    #[test]
    fn limits_are_a_no_op_off_windows_and_accepted_on_it() {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/C", "exit 0"][..]
            } else {
                &[][..]
            })
            .spawn()
            .expect("spawn");
        if let Ok(tree) = ProcessTree::adopt(child.id()) {
            tree.limit(JobLimits::default()).expect("no limits");
        }
        let _ = child.wait();
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    fn powershell(script: &str) -> Command {
        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(TREE_CREATION_FLAGS);
        cmd
    }

    /// Wait for `child` at most `limit`, killing it if it overruns.
    /// Windows PowerShell's cold start alone can pass a minute on a loaded,
    /// emulated host; the limits under test act in seconds once it runs.
    const CHILD_DEADLINE: Duration = Duration::from_secs(300);

    fn wait_within(child: &mut std::process::Child, limit: Duration) -> Option<i32> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                return status.code();
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("the limited process outlived {limit:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_cpu_limit_ends_a_busy_tree() {
        // The 2 s budget covers powershell's own startup as well.
        let mut child = powershell("while ($true) { }").spawn().expect("spawn");
        let tree = ProcessTree::adopt(child.id()).expect("adopt");
        tree.limit(JobLimits {
            cpu_seconds: Some(2),
            ..JobLimits::default()
        })
        .expect("limit");
        let code = wait_within(&mut child, CHILD_DEADLINE);
        assert_ne!(code, Some(0));
    }

    #[test]
    fn a_memory_limit_fails_an_oversized_allocation() {
        let mut child = powershell("try { $b = New-Object byte[] (1GB); exit 0 } catch { exit 7 }")
            .spawn()
            .expect("spawn");
        let tree = ProcessTree::adopt(child.id()).expect("adopt");
        tree.limit(JobLimits {
            memory_bytes: Some(512 << 20),
            ..JobLimits::default()
        })
        .expect("limit");
        let code = wait_within(&mut child, CHILD_DEADLINE);
        assert_ne!(code, Some(0), "a 1 GiB allocation passed a 512 MiB limit");
    }

    #[test]
    fn below_normal_priority_reaches_a_process_started_later() {
        let mut child = powershell(
            "$c = Start-Process -PassThru -NoNewWindow -FilePath ping.exe -ArgumentList '-n','3','127.0.0.1'; \
             Start-Sleep -Milliseconds 300; \
             if ((Get-Process -Id $c.Id).PriorityClass -eq 'BelowNormal') { exit 0 } else { exit 9 }",
        )
        .spawn()
        .expect("spawn");
        let tree = ProcessTree::adopt(child.id()).expect("adopt");
        tree.limit(JobLimits {
            below_normal_priority: true,
            ..JobLimits::default()
        })
        .expect("limit");
        assert_eq!(wait_within(&mut child, CHILD_DEADLINE), Some(0));
    }
}
