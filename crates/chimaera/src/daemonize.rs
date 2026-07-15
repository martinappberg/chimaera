//! Detach `serve` into its own session so the daemon outlives the shell — or
//! the one-shot SSH channel — that launched it. The portable replacement for a
//! Linux-only `setsid nohup` shell prefix.
//!
//! `connect` starts a remote daemon over a single SSH command. For the daemon
//! to survive that command returning it must (a) stop holding the SSH channel's
//! stdio, so sshd closes the session, and (b) escape the session sshd tears
//! down on exit. The old start line delegated (b) to util-linux `setsid` +
//! `nohup`, which are absent on macOS/BSD and on minimal Linux containers — so
//! a daemon could only be started on a GNU/Linux host. `setsid(2)` is POSIX and
//! behaves identically everywhere, so doing the detach in-process lets `connect`
//! bring a daemon up on ANY POSIX remote with no host binaries. (a) stays the
//! caller's job: the start line still redirects `>> log 2>&1 < /dev/null`, and a
//! foreground `chimaera serve` (no `--daemonize`) keeps the terminal on purpose.

use anyhow::Context;
use nix::unistd::{fork, setsid, ForkResult};

/// Fork, exit the parent, and put the surviving child in a fresh session.
///
/// MUST run while the process is still single-threaded — before the tokio
/// runtime spawns its workers. `fork(2)` clones only the calling thread, so a
/// fork after the runtime is up would strand any lock another thread held and
/// the child would deadlock on its next allocation. `main` calls this first,
/// before it builds the runtime, for exactly that reason.
pub fn detach() -> anyhow::Result<()> {
    // Fork first so the survivor is guaranteed NOT to be a process-group
    // leader: `setsid` fails with EPERM for a group leader, which is what an
    // interactive shell makes of a foreground `chimaera serve`.
    //
    // SAFETY: the call site is single-threaded (see the doc comment); the child
    // does nothing between fork and `setsid` but call `setsid`.
    match unsafe { fork() }.context("fork to detach the daemon")? {
        ForkResult::Parent { .. } => {
            // The instant we exit, sshd sees `connect`'s launch command finish
            // and moves on to polling the manifest. Nothing of ours needs
            // flushing and the child owns the daemon now, so exit straight away.
            std::process::exit(0);
        }
        ForkResult::Child => {
            // A new session with no controlling terminal: the SIGHUP sshd sends
            // on teardown goes to the OLD session and never reaches us.
            setsid().context("setsid to detach the daemon")?;
            Ok(())
        }
    }
}
