//! Claude's callback prompt requires terminal input AND output. This transport
//! has no session, grid or log; nonblocking I/O makes cancellation drop it without
//! leaving a blocking-pool read behind.
//!
//! Windows has no pty a `tokio::process::Command` can be given as stdio (a
//! ConPTY owns the child's creation), so the Claude sign-in there is not
//! offered from this flow: [`AuthPty::prepare`] fails with [`AuthPty::UNAVAILABLE`]
//! as its user-facing reason.
#[cfg(unix)]
use std::{io, os::fd::OwnedFd, process::Stdio};
#[cfg(unix)]
use tokio::io::{unix::AsyncFd, Interest};

#[cfg(unix)]
pub(super) struct AuthPty(AsyncFd<OwnedFd>);

#[cfg(unix)]
impl AuthPty {
    pub(super) const UNAVAILABLE: &'static str = "Couldn't prepare the agent's sign-in terminal.";

    pub(super) fn prepare(command: &mut tokio::process::Command) -> io::Result<Self> {
        use nix::{fcntl, sys::termios};
        let pty = nix::pty::openpty(None, None)?;
        for fd in [&pty.master, &pty.slave] {
            fcntl::fcntl(fd, fcntl::FcntlArg::F_SETFD(fcntl::FdFlag::FD_CLOEXEC))?;
        }
        fcntl::fcntl(
            &pty.master,
            fcntl::FcntlArg::F_SETFL(fcntl::OFlag::O_NONBLOCK),
        )?;
        // Disable kernel echo and the canonical line limit. The CLI can still
        // render its own input; all output remains private and bounded in auth.rs.
        let mut term = termios::tcgetattr(&pty.slave)?;
        termios::cfmakeraw(&mut term);
        termios::tcsetattr(&pty.slave, termios::SetArg::TCSANOW, &term)?;
        command.stdin(Stdio::from(pty.slave.try_clone()?));
        command.stdout(Stdio::from(pty.slave.try_clone()?));
        command.stderr(Stdio::from(pty.slave));
        // SAFETY: OwnedFd keeps this nonblocking PTY descriptor valid and
        // unchanged until AsyncFd deregisters it before dropping the owner.
        Ok(Self(unsafe { AsyncFd::register(pty.master) }?))
    }

    pub(super) async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.0
            .async_io(Interest::READABLE, |fd| match nix::unistd::read(fd, buf) {
                // Linux reports EIO after the last slave closes; macOS returns 0.
                Err(nix::errno::Errno::EIO) => Ok(0),
                result => result.map_err(io::Error::from),
            })
            .await
    }

    pub(super) async fn write_all(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            // macOS PTYs need not announce initial write readiness. Try the
            // nonblocking write first; wait only after actual backpressure.
            let n = match nix::unistd::write(self.0.get_ref(), bytes) {
                Err(nix::errno::Errno::EAGAIN) => {
                    self.0
                        .async_io(Interest::WRITABLE, |fd| {
                            nix::unistd::write(fd, bytes).map_err(io::Error::from)
                        })
                        .await?
                }
                result => result?,
            };
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = &bytes[n..];
        }
        Ok(())
    }
}

/// Never constructed: [`AuthPty::prepare`] always fails on Windows.
#[cfg(windows)]
pub(super) enum AuthPty {}

#[cfg(windows)]
impl AuthPty {
    pub(super) const UNAVAILABLE: &'static str = "Claude's connector sign-in isn't available here on Windows yet. Use /mcp in a Claude session to sign in.";

    pub(super) fn prepare(_command: &mut tokio::process::Command) -> std::io::Result<Self> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    pub(super) async fn read(&self, _buf: &mut [u8]) -> std::io::Result<usize> {
        match *self {}
    }

    pub(super) async fn write_all(&self, _bytes: &[u8]) -> std::io::Result<()> {
        match *self {}
    }
}
