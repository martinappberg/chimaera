//! Detach `serve` into its own session so the daemon outlives the shell — or
//! the one-shot SSH channel — that launched it. The portable replacement for a
//! Linux-only `setsid nohup` shell prefix.
//!
//! `connect` starts a remote daemon over a single SSH command. For the daemon
//! to survive that command returning it must (a) stop holding the SSH channel's
//! stdio, so sshd closes the session, (b) ignore SIGHUP, which sshd sends the
//! instant the command returns, and (c) escape the session sshd tears down on
//! exit. The old start line delegated (b)+(c) to util-linux `setsid` + `nohup`,
//! which are absent on macOS/BSD and on minimal Linux containers — so a daemon
//! could only be started on a GNU/Linux host. `setsid(2)` and ignoring SIGHUP
//! are POSIX and behave identically everywhere, so doing the detach in-process
//! lets `connect` bring a daemon up on ANY POSIX remote with no host binaries.
//! (a) stays the caller's job: the start line still redirects
//! `>> log 2>&1 < /dev/null`, and a foreground `chimaera serve` (no
//! `--daemonize`) keeps the terminal on purpose.

use anyhow::Context;
use nix::sys::signal::{signal, SigHandler, Signal};
use nix::unistd::{fork, setsid, ForkResult};

/// Ignore SIGHUP, fork, exit the parent, and put the surviving child in a fresh
/// session.
///
/// MUST run while the process is still single-threaded — before the tokio
/// runtime spawns its workers. `fork(2)` clones only the calling thread, so a
/// fork after the runtime is up would strand any lock another thread held and
/// the child would deadlock on its next allocation. `main` calls this first,
/// before it builds the runtime, for exactly that reason.
pub fn detach() -> anyhow::Result<()> {
    // Ignore SIGHUP BEFORE forking so the daemon is immune from the instant it
    // exists. sshd sends SIGHUP the moment the launch command returns, and it
    // can land in the gap after the parent exits but before the child has
    // `setsid`'d out of the session — where the default disposition would kill
    // the daemon on startup and `connect` would time out waiting for a manifest
    // that never appears. This is the `nohup` half of the old `setsid nohup`
    // incantation, in-process: an ignored disposition survives `fork`, so the
    // child inherits it and is covered through the whole detach.
    //
    // SAFETY: single-threaded call site (see the doc comment); `SigIgn` installs
    // no Rust handler and is async-signal-safe.
    unsafe { signal(Signal::SIGHUP, SigHandler::SigIgn) }
        .context("ignore SIGHUP before detaching the daemon")?;

    // Fork so the survivor is guaranteed NOT to be a process-group leader:
    // `setsid` fails with EPERM for a group leader, which is what an interactive
    // shell makes of a foreground `chimaera serve`.
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
            // Also escape the session (SIGHUP is already ignored above): the new
            // session has no controlling terminal, so nothing sshd does on
            // teardown reaches us.
            setsid().context("setsid to detach the daemon")?;
            Ok(())
        }
    }
}
