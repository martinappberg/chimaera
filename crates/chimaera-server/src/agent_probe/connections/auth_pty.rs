//! Claude's callback prompt requires terminal input AND output. This transport
//! has no session, grid or log; nonblocking I/O makes cancellation drop it without
//! leaving a blocking-pool read behind.
use std::{io, os::fd::OwnedFd, process::Stdio};
use tokio::io::{unix::AsyncFd, Interest};

pub(super) struct AuthPty(AsyncFd<OwnedFd>);

impl AuthPty {
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
        Ok(Self(AsyncFd::new(pty.master)?))
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
