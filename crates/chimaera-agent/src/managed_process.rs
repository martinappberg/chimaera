//! Exact Linux child parking. A pidfd is obtained under the owned Child lock;
//! namespace PID/start ticks are presentation to the trusted supervisor only.
//! Neither a numeric PID nor process-group membership grants signal authority.
use anyhow::{ensure, Context, Result};
use std::{
    fs::File,
    io::Read,
    os::fd::{AsRawFd, FromRawFd},
};

pub struct ManagedProcess {
    descriptor: File,
    pid: u32,
    start_ticks: u64,
    stopped: bool,
}
impl ManagedProcess {
    pub(crate) fn pin(pid: u32) -> Result<Self> {
        ensure!(pid > 0, "managed child unavailable");
        let raw = unsafe { nix::libc::syscall(nix::libc::SYS_pidfd_open, pid, 0) } as i32;
        ensure!(raw >= 0, "managed child identity unavailable");
        let descriptor = unsafe { File::from_raw_fd(raw) };
        let (_, start_ticks) = status(pid)?;
        let process = Self {
            descriptor,
            pid,
            start_ticks,
            stopped: false,
        };
        ensure!(process.present()?, "managed child exited");
        Ok(process)
    }
    pub fn identity(&self) -> (u32, u64) {
        (self.pid, self.start_ticks)
    }
    pub fn present(&self) -> Result<bool> {
        let mut poll = nix::libc::pollfd {
            fd: self.descriptor.as_raw_fd(),
            events: nix::libc::POLLIN,
            revents: 0,
        };
        ensure!(
            unsafe { nix::libc::poll(&mut poll, 1, 0) } >= 0,
            "managed child probe unavailable"
        );
        Ok(poll.revents == 0)
    }
    fn signal(&self, signal: i32) -> Result<()> {
        ensure!(
            unsafe {
                nix::libc::syscall(
                    nix::libc::SYS_pidfd_send_signal,
                    self.descriptor.as_raw_fd(),
                    signal,
                    std::ptr::null::<nix::libc::siginfo_t>(),
                    0,
                )
            } == 0,
            "managed child signal unavailable"
        );
        Ok(())
    }
    /// Call only after separately proving strict idle under ingress guards.
    /// Confirmation is separate: delivering SIGSTOP is not a stopped receipt.
    pub fn request_stop(&mut self) -> Result<()> {
        let (state, ticks) = status(self.pid)?;
        ensure!(
            self.present()?
                && ticks == self.start_ticks
                && !matches!(state, b'T' | b't' | b'Z' | b'X'),
            "managed child state unknown"
        );
        self.signal(nix::libc::SIGSTOP)?;
        self.stopped = true;
        Ok(())
    }
    pub fn is_stopped(&self) -> Result<bool> {
        let (state, ticks) = status(self.pid)?;
        ensure!(
            self.present()? && ticks == self.start_ticks,
            "managed child identity changed"
        );
        Ok(self.stopped && state == b'T')
    }
    /// Never creates a replacement child. Exit proves there is nothing to
    /// resume; unknown identity/signal failure must retain recovery fencing.
    pub fn resume(&mut self) -> Result<()> {
        if self.stopped {
            if self.present()? {
                self.signal(nix::libc::SIGCONT)?;
            }
            self.stopped = false;
        }
        Ok(())
    }
}

fn status(pid: u32) -> Result<(u8, u64)> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/{pid}/stat"))
        .and_then(|file| file.take(16 * 1024 + 1).read_to_end(&mut bytes))
        .context("managed child status unavailable")?;
    ensure!(bytes.len() <= 16 * 1024, "managed child status oversized");
    let text = std::str::from_utf8(&bytes).context("managed child status invalid")?;
    let tail = text
        .rsplit_once(") ")
        .context("managed child status invalid")?
        .1;
    let fields: Vec<_> = tail.split_ascii_whitespace().take(21).collect();
    ensure!(
        fields.len() >= 20 && fields[0].len() == 1,
        "managed child status invalid"
    );
    let ticks: u64 = fields[19].parse().context("managed child start invalid")?;
    ensure!(ticks > 0, "managed child start unavailable");
    Ok((fields[0].as_bytes()[0], ticks))
}
