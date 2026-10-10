//! Agent processes never outlive their daemon. Each agent runs in its own
//! process group (`ndjson`) and a graceful daemon stop ends them, but a daemon
//! that is killed outright (SIGKILL, a crash) runs no cleanup: its agents, and
//! the shells they started, would keep running and writing to the project.
//!
//! So a small watcher process (`/bin/sh`, in its own process group) holds the
//! read end of a pipe from the daemon and the list of live agents. When the
//! pipe closes, the daemon is gone however it went: the watcher asks each
//! agent to stop (SIGTERM, which the agent CLIs answer by ending their own
//! detached background work), then kills every process group of the agents
//! and of their descendants. The list is mirrored to a per-host file, so a
//! daemon that starts after the watcher died too ends those agents before
//! anything resumes.
//!
//! Only a daemon calls [`install`]; without it registration is a no-op (tests,
//! CLI probes).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The watcher. Argument-free and free of product words, so a `pkill -f`
/// aimed at the daemon never matches it; it ignores the signals such a kill
/// sends. Input lines are `+PID:START` (an agent started) and `-PID` (it was
/// reaped). At EOF the process tree is read before any agent stops (their
/// children are re-parented once they exit), the agents get SIGTERM, and two
/// seconds later every recorded group gets SIGKILL. `REAPER_SPARE` is the
/// daemon's own group, never signalled.
const WATCHER: &str = r#"trap '' HUP INT TERM PIPE
live=
save() {
  if [ -n "$live" ]; then
    printf '%s\n' $live >"$REAPER_RECORD.tmp" && mv -f "$REAPER_RECORD.tmp" "$REAPER_RECORD"
  else
    rm -f "$REAPER_RECORD"
  fi
}
while IFS= read -r line; do
  case $line in
    +*) live="$live ${line#+}" ;;
    -*) keep=
        for e in $live; do [ "${e%%:*}" = "${line#-}" ] || keep="$keep $e"; done
        live=$keep ;;
    *) continue ;;
  esac
  save
done
leaders=
for e in $live; do leaders="$leaders ${e%%:*}"; done
[ -n "$leaders" ] || exit 0
groups=$(ps -A -o pid= -o ppid= -o pgid= 2>/dev/null | awk -v roots="$leaders" '
  BEGIN { n = split(roots, r, " "); for (i = 1; i <= n; i++) mine[r[i]] = 1 }
  { pid[NR] = $1; parent[NR] = $2; group[NR] = $3 }
  END {
    do {
      grew = 0
      for (i = 1; i <= NR; i++) if (!(pid[i] in mine) && (parent[i] in mine)) { mine[pid[i]] = 1; grew = 1 }
    } while (grew)
    for (i = 1; i <= NR; i++) if (pid[i] in mine) print group[i]
  }')
kill -TERM $leaders 2>/dev/null
sleep 2
for g in $leaders $groups; do
  case $g in ''|0|1|"$REAPER_SPARE"|"$$") ;; *) kill -KILL -- "-$g" 2>/dev/null ;; esac
done
rm -f "$REAPER_RECORD"
"#;

/// Bounds on what the daemon reads back from a previous life's record.
const RECORD_MAX_BYTES: u64 = 64 * 1024;
const RECORD_MAX_ENTRIES: usize = 512;

struct Reaper {
    record: PathBuf,
    live: std::collections::HashMap<u32, u64>,
    watcher: Option<(std::process::Child, std::process::ChildStdin)>,
}

static REAPER: Mutex<Option<Reaper>> = Mutex::new(None);

/// Arm agent cleanup for this daemon. Blocking (it may wait a few seconds for
/// a previous life's agents to stop): call off the reactor, once, before any
/// agent starts or resumes. `record` must be specific to this daemon's state
/// directory and host.
pub fn install(record: PathBuf) {
    #[cfg(unix)]
    {
        end_previous_life(&record);
        *REAPER.lock().expect("reaper lock") = Some(Reaper {
            record,
            live: Default::default(),
            watcher: None,
        });
    }
    #[cfg(not(unix))]
    let _ = record;
}

/// An agent process (the leader of its own group) started.
pub(crate) fn register(pid: u32) {
    #[cfg(unix)]
    {
        let start = leader_start(pid).unwrap_or(0);
        let mut guard = REAPER.lock().expect("reaper lock");
        let Some(reaper) = guard.as_mut() else {
            return;
        };
        reaper.live.insert(pid, start);
        reaper.send(&format!("+{pid}:{start}\n"));
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// The agent started as `pid` was stopped and is about to be (or was) reaped.
pub(crate) fn unregister(pid: u32) {
    let mut guard = REAPER.lock().expect("reaper lock");
    let Some(reaper) = guard.as_mut() else {
        return;
    };
    if reaper.live.remove(&pid).is_some() {
        reaper.send(&format!("-{pid}\n"));
    }
}

impl Reaper {
    /// Pass one change to the watcher. A watcher that is gone is replaced by
    /// a new one that is given the whole current list instead.
    fn send(&mut self, line: &str) {
        use std::io::Write;
        if let Some((_, stdin)) = self.watcher.as_mut() {
            if stdin.write_all(line.as_bytes()).is_ok() {
                return;
            }
        }
        if let Some((mut child, _)) = self.watcher.take() {
            let _ = child.try_wait();
            tracing::warn!("agent watcher exited; starting another");
        }
        let entries: Vec<(u32, u64)> = self.live.iter().map(|(p, s)| (*p, *s)).collect();
        match start_watcher(&self.record, &entries) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(error) => {
                tracing::warn!(%error, "could not start the agent watcher; a killed daemon may leave agents running");
            }
        }
    }
}

fn start_watcher(
    record: &Path,
    entries: &[(u32, u64)],
) -> std::io::Result<(std::process::Child, std::process::ChildStdin)> {
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", WATCHER])
        .env("REAPER_RECORD", record)
        .env("REAPER_SPARE", nix::unistd::getpgrp().as_raw().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own group: a signal to the daemon's group (a terminal's ^C,
        // a launcher stopping its child's group) must not take it along.
        .process_group(0);
    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    for (pid, start) in entries {
        stdin.write_all(format!("+{pid}:{start}\n").as_bytes())?;
    }
    Ok((child, stdin))
}

/// Agents a previous daemon on this host recorded and that still run. Each
/// entry is checked against its leader's start time, so a reused process id
/// is never taken for an old agent; a group whose leader already exited but
/// whose members still run is still the old one (its id cannot be reused
/// while the group lives). The survivors go through the same stop as a
/// watcher's, waited for.
#[cfg(unix)]
fn end_previous_life(record: &Path) {
    use std::io::Read;
    let mut text = String::new();
    let read = std::fs::File::open(record)
        .and_then(|file| file.take(RECORD_MAX_BYTES).read_to_string(&mut text));
    if read.is_err() {
        return;
    }
    let survivors: Vec<(u32, u64)> = text
        .lines()
        .take(RECORD_MAX_ENTRIES)
        .filter_map(|line| {
            let (pid, start) = line.trim().split_once(':')?;
            Some((pid.parse().ok()?, start.parse().ok()?))
        })
        .filter(|&(pid, start)| still_running(pid, start))
        .collect();
    if survivors.is_empty() {
        let _ = std::fs::remove_file(record);
        return;
    }
    tracing::warn!(
        count = survivors.len(),
        "agents of a previous daemon still run; stopping them before anything resumes"
    );
    match start_watcher(record, &survivors) {
        Ok((mut child, stdin)) => {
            drop(stdin);
            let until = std::time::Instant::now() + std::time::Duration::from_secs(6);
            while std::time::Instant::now() < until {
                if !matches!(child.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            tracing::warn!("previous agents are still stopping");
        }
        Err(error) => tracing::warn!(%error, "could not stop a previous daemon's agents"),
    }
}

#[cfg(unix)]
fn still_running(pid: u32, start: u64) -> bool {
    let Ok(id) = i32::try_from(pid) else {
        return false;
    };
    if id <= 1 || start == 0 {
        return false;
    }
    if nix::sys::signal::killpg(nix::unistd::Pid::from_raw(id), None).is_err() {
        return false;
    }
    leader_start(pid).is_none_or(|now| now == start)
}

/// When a process started, in an OS-specific unit (Linux: clock ticks since
/// boot; macOS: microseconds since the epoch). `None` when it is gone.
#[cfg(unix)]
fn leader_start(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // After the parenthesized command: the state is the first field, the
        // start time the twentieth.
        let rest = stat.get(stat.rfind(')')? + 1..)?;
        rest.split_whitespace().nth(19)?.parse().ok()
    }
    #[cfg(target_os = "macos")]
    {
        let mut info = std::mem::MaybeUninit::<nix::libc::proc_bsdinfo>::zeroed();
        let size = std::mem::size_of::<nix::libc::proc_bsdinfo>() as i32;
        // SAFETY: the buffer is exactly one `proc_bsdinfo`, as requested.
        let written = unsafe {
            nix::libc::proc_pidinfo(
                i32::try_from(pid).ok()?,
                nix::libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: fully written by the successful call above.
        let info = unsafe { info.assume_init() };
        Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn alive(pid: u32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
    }

    /// An "agent" (its own group) whose loop runs detached in a new session,
    /// as claude starts its background shells. Returns the agent and the
    /// loop's pid (read from a file the loop writes).
    // Every caller waits for the returned child.
    #[allow(clippy::zombie_processes)]
    fn agent_with_detached_loop(dir: &Path) -> (std::process::Child, u32) {
        let pidfile = dir.join(format!("loop-{}", rand::random::<u32>()));
        let script = format!(
            "perl -e 'use POSIX; setsid(); exec @ARGV' sh -c 'echo $$ > {p}; while :; do sleep 1; done' & \
             while :; do sleep 1; done",
            p = pidfile.display()
        );
        let mut agent = Command::new("/bin/sh")
            .args(["-c", &script])
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pid) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                return (agent, pid);
            }
            if Instant::now() >= until {
                let _ = agent.kill();
                let _ = agent.wait();
                panic!("loop never started");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn gone_within(pids: &[u32], limit: Duration) -> bool {
        let until = Instant::now() + limit;
        while Instant::now() < until {
            if pids.iter().all(|pid| !alive(*pid)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn a_closed_pipe_ends_the_agents_and_their_detached_work() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("agents");
        let (mut agent, detached) = agent_with_detached_loop(dir.path());
        let leader = agent.id();
        let start = leader_start(leader).unwrap();
        let (mut watcher, stdin) = start_watcher(&record, &[(leader, start)]).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while !record.exists() {
            assert!(
                Instant::now() < until,
                "the watcher never recorded the agent"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            std::fs::read_to_string(&record).unwrap().trim(),
            format!("{leader}:{start}")
        );
        // The daemon dies: its end of the pipe closes.
        drop(stdin);
        let _ = agent.wait();
        assert!(
            gone_within(&[detached], Duration::from_secs(5)),
            "the detached loop outlived the daemon"
        );
        assert!(watcher.wait().unwrap().success());
        assert!(!record.exists(), "a finished stop leaves no record");
    }

    #[test]
    fn a_reaped_agent_leaves_the_record_and_is_never_signalled() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("agents");
        let (mut watcher, mut stdin) = start_watcher(&record, &[(4_000_000, 1)]).unwrap();
        use std::io::Write;
        stdin.write_all(b"-4000000\n").unwrap();
        drop(stdin);
        assert!(watcher.wait().unwrap().success());
        assert!(!record.exists());
    }

    #[test]
    fn a_previous_life_is_ended_only_for_entries_that_are_still_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("agents");
        let (mut agent, detached) = agent_with_detached_loop(dir.path());
        let leader = agent.id();
        let start = leader_start(leader).unwrap();
        // A second process whose recorded start does not match: a reused id.
        let mut bystander = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        std::fs::write(
            &record,
            format!("{leader}:{start}\n{}:{}\n", bystander.id(), 1),
        )
        .unwrap();
        end_previous_life(&record);
        let _ = agent.wait();
        assert!(gone_within(&[detached], Duration::from_secs(1)));
        assert!(alive(bystander.id()), "a reused id was signalled");
        assert!(!record.exists());
        let _ = bystander.kill();
        let _ = bystander.wait();
    }
}
