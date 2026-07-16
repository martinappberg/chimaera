//! Detach `serve` into its own session so the daemon outlives the shell — or
//! the one-shot SSH channel — that launched it. The portable replacement for a
//! Linux-only `setsid nohup` shell prefix.
//!
//! `connect` starts a remote daemon over a single SSH command. For the daemon
//! to survive that command returning it must (a) stop holding the SSH channel's
//! stdio, so sshd closes the session, (b) ignore SIGHUP, which sshd sends the
//! instant the command returns, and (c) escape the session sshd tears down on
//! exit. The old start line delegated (b)+(c) to util-linux `setsid` + `nohup`,
//! absent on macOS/BSD and on minimal Linux containers — so a daemon could only
//! be started on GNU/Linux. Doing it in-process with POSIX primitives lets
//! `connect` bring a daemon up on ANY POSIX remote with no host binaries. (a)
//! is the child's job too: it re-points every stdio fd that is NOT a regular
//! file at `/dev/null`, keeping a caller's log redirect (`connect`'s start
//! line does `>> log 2>&1 < /dev/null`) while dropping a terminal or an ssh
//! channel's pipes. Trusting the caller to redirect proved unsafe: a
//! hand-started `serve --daemonize` over plain ssh keeps the channel's pipes
//! as stdio, and once that channel dies every tracing write returns
//! EPIPE/EIO — which poisoned request handlers that log (routes that log gave
//! empty replies while silent routes answered; found live on a cluster). A
//! foreground `chimaera serve` (no `--daemonize`) keeps the terminal on
//! purpose.
//!
//! The child **re-exec**s a fresh `chimaera serve` rather than continuing in the
//! forked image. `fork(2)` clones only the calling thread, so any lock another
//! thread held at fork time — the global mimalloc allocator's background purge
//! thread is the concrete one — is frozen in the child, and its next allocation
//! could deadlock on it. `execve` discards that inherited image for a clean,
//! single-threaded one with a fresh allocator; the ignored SIGHUP, the
//! redirected stdio, the new session, the environment, and the pid all survive
//! it, so the re-exec'd daemon lands fully detached and connect can still stop
//! it by the pid in its manifest.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;

use anyhow::Context;
use nix::fcntl::OFlag;
use nix::sys::signal::{signal, SigHandler, Signal};
use nix::sys::stat::{fstat, Mode, SFlag};
use nix::unistd::{close, dup2, execv, fork, setsid, ForkResult};

/// Detach the current process and re-exec a fresh `chimaera serve` (without
/// `--daemonize`) as the daemon, in its own session with SIGHUP ignored.
///
/// MUST run while the process is still single-threaded — before the tokio
/// runtime spawns its workers — so the `fork` clones a quiescent process and the
/// child touches nothing an async-signal-unsafe path (allocation included) would
/// need.
pub fn detach() -> anyhow::Result<()> {
    // Prepare EVERYTHING the child needs here, in the parent, before the fork:
    // the child may not allocate (an inherited allocator lock could deadlock it),
    // so the re-exec target and argv are built now.
    //
    // The exec target is our resolved own path (argv[0] may be a bare name, and
    // `execv` does not search `$PATH`). The argv is the real args with only
    // `--daemonize` dropped, so every other serve flag — `--port`, anything
    // added later — is forwarded verbatim, and the re-exec'd `serve` does NOT
    // re-enter this detach.
    let exe = std::env::current_exe().context("locate own executable to re-exec")?;
    let exe_c = CString::new(exe.as_os_str().as_bytes()).context("executable path has a NUL")?;
    let argv = std::env::args_os()
        .filter(|arg| arg.as_bytes() != b"--daemonize")
        .map(|arg| CString::new(arg.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .context("a command-line argument has a NUL")?;

    // Ignore SIGHUP BEFORE forking so the daemon is immune from the instant it
    // exists. sshd sends SIGHUP the moment the launch command returns, and it
    // can land after the parent exits but before the child re-execs — where the
    // default disposition would kill the daemon on startup and `connect` would
    // time out on a manifest that never appears. This is the `nohup` half of the
    // old `setsid nohup`, in-process; an ignored disposition survives both `fork`
    // and `execve`, so the child is covered throughout. Installed before the
    // stdio prep below to keep the default-disposition window minimal.
    //
    // SAFETY: single-threaded call site (see the doc comment); `SigIgn` installs
    // no Rust handler and is async-signal-safe.
    unsafe { signal(Signal::SIGHUP, SigHandler::SigIgn) }
        .context("ignore SIGHUP before detaching the daemon")?;

    // Stdio policy, also decided in the parent: the child re-points every
    // stdio fd that is NOT a regular file at /dev/null — drop anything that
    // can die with the launching session (the module doc has the failure
    // story), keep a caller's log-file redirect (connect's start line appends
    // to one; file writes stay valid whatever happens to the launcher). A
    // closed fd (fstat EBADF) is redirected too, so the daemon's next open()
    // can't land on fd 0/1/2 and receive stray tracing writes. /dev/null is
    // opened WITHOUT CLOEXEC: if a stdio fd was closed at launch, the new fd
    // may itself be 0/1/2, where the self-dup2 below is a no-op that would
    // NOT clear a close-on-exec flag — instead the child closes the fd after
    // the dup2s iff it landed above the stdio range.
    let devnull = nix::fcntl::open("/dev/null", OFlag::O_RDWR, Mode::empty())
        .context("open /dev/null for the daemon's stdio")?;
    let is_regular_file = |fd: RawFd| matches!(fstat(fd), Ok(st) if SFlag::from_bits_truncate(st.st_mode) & SFlag::S_IFMT == SFlag::S_IFREG);
    let redirect = [0, 1, 2].map(|fd| !is_regular_file(fd));

    // Fork so the survivor is NOT a process-group leader (`setsid` EPERMs for a
    // leader — what an interactive shell makes of a foreground `chimaera serve`).
    //
    // SAFETY: single-threaded (see above); the child calls only async-signal-safe
    // `setsid` / `dup2` / `close` / `execv` on state prepared above — no
    // allocation (error contexts allocate only on the already-doomed error
    // path, as before), no locks.
    match unsafe { fork() }.context("fork to detach the daemon")? {
        ForkResult::Parent { .. } => {
            // The instant we exit, sshd sees `connect`'s launch command finish
            // and moves on to polling the manifest. The child owns the daemon
            // now; nothing of ours needs flushing, so exit straight away.
            std::process::exit(0);
        }
        ForkResult::Child => {
            // New session (no controlling terminal), then swap in a clean image.
            setsid().context("setsid to detach the daemon")?;
            // Drop the launcher's channel per the stdio policy above. `dup2`
            // clears close-on-exec on the copy, so the re-pointed fds survive
            // the `execv`.
            for (fd, must_redirect) in redirect.iter().enumerate() {
                if *must_redirect {
                    dup2(devnull, fd as RawFd).context("point daemon stdio at /dev/null")?;
                }
            }
            if devnull > 2 {
                let _ = close(devnull);
            }
            // `execv` returns only on failure; on success the fresh `serve`
            // takes over this pid and never comes back here.
            let errno = execv(&exe_c, &argv).expect_err("execv returns only on failure");
            Err(anyhow::Error::new(errno).context("re-exec the detached daemon"))
        }
    }
}
