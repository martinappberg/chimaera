//! Durable launch evidence. A fresh daemon cannot turn an empty in-memory
//! registry into proof that children from a crashed daemon have stopped.
use super::*;
use std::{collections::HashSet, io::Read, path::Path};
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Latch {
    version: u16,
    workspaces: Vec<String>,
}
impl State {
    pub(in crate::pro) fn restore(
        root: &Path,
        preferences: &HashMap<String, super::super::Preference>,
        worker: bool,
        state_unknown: bool,
    ) -> Self {
        let loaded = (|| -> Result<HashSet<String>> {
            let file = match std::fs::File::open(root.join("execution-authority.json")) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
                Err(e) => return Err(e.into()),
            };
            let mut bytes = Vec::new();
            file.take(32769).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 32768, "execution latch exceeds limit");
            let latch: Latch = serde_json::from_slice(&bytes)?;
            ensure!(
                latch.version == 1
                    && latch.workspaces.len() <= 128
                    && latch.workspaces.iter().all(|id| crate::pro::valid_id(id)),
                "invalid execution latch"
            );
            Ok(latch.workspaces.into_iter().collect())
        })();
        // A project is uncertain when the enrollment latch names it but its
        // policy is gone (the ordinary state was lost). Only those projects
        // stop publishing until the account confirms their policy again; an
        // unenrolled project never becomes managed. A cloud machine whose
        // latch or state cannot be read at all cannot tell which of its
        // projects were enrolled, and it runs only cloud projects, so there
        // each project with local mirror data or preferences waits too. (A
        // device needs no such guess: the account itself refuses a legacy
        // downgrade of an enrolled project.)
        let unknown = loaded.is_err() || state_unknown;
        let mut latched = loaded.unwrap_or_default();
        let lacking = |id: &String| preferences.get(id).is_none_or(|p| p.continuity.is_none());
        let mut uncertain: HashSet<String> =
            latched.iter().filter(|id| lacking(id)).cloned().collect();
        if unknown && worker {
            uncertain.extend(preferences.keys().filter(|id| lacking(id)).cloned());
            if let Ok(entries) = std::fs::read_dir(root) {
                uncertain.extend(
                    entries
                        .filter_map(std::result::Result::ok)
                        .take(4096)
                        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
                        .filter_map(|entry| entry.file_name().into_string().ok())
                        .filter(|id| crate::pro::valid_id(id) && lacking(id)),
                );
            }
        }
        let uncertain: HashSet<String> = uncertain.into_iter().take(128).collect();
        latched.extend(
            preferences
                .iter()
                .filter(|(_, p)| p.continuity.is_some())
                .map(|(id, _)| id.clone()),
        );
        let boot = boot_id();
        let unclean = preferences
            .iter()
            .filter(|(_, p)| {
                p.execution_active
                    && (boot.is_none()
                        || p.execution_boot.as_ref() == boot.as_ref()
                        || p.execution_boot.is_none())
            })
            .filter_map(|(id, p)| {
                // A truncated prefix cannot prove all old work stopped. Empty
                // evidence deliberately remains unknown across reprobes; only
                // a cold boot or trusted supervisor cleanup proves otherwise.
                if p.execution_groups_overflow || p.execution_launch_pending {
                    return Some((id.clone(), Vec::new()));
                }
                // Probe what the previous life recorded instead of fencing
                // forever. With no recorded group a device proceeds (laptop
                // first); a worker cannot prove anything and stays strict.
                let recorded: Vec<(u32, u64)> = p
                    .execution_groups
                    .iter()
                    .enumerate()
                    .map(|(index, group)| {
                        (*group, p.execution_starts.get(index).copied().unwrap_or(0))
                    })
                    .collect();
                let alive = surviving(&recorded);
                (!alive.is_empty() || (p.execution_groups.is_empty() && worker))
                    .then(|| (id.clone(), alive))
            })
            .collect();
        Self {
            proofs: Mutex::default(),
            commits: mutation::Commits::default(),
            setups: Mutex::default(),
            launches: Mutex::default(),
            latched: Mutex::new(latched),
            unclean: Mutex::new(unclean),
            uncertain: Mutex::new(uncertain),
            boot,
            supervisor_state_invalid: unknown,
            supervisor_pending: Mutex::default(),
            #[cfg(unix)]
            maintenance_pending: Mutex::default(),
            #[cfg(all(unix, feature = "provider-authority-prototype"))]
            provider_pending: Mutex::default(),
            supervisor_ack: Mutex::default(),
            tick: Mutex::default(),
            changed: tokio::sync::Notify::new(),
        }
    }
}
/// Recorded process groups that still run the recorded work. A group that
/// vanished (ESRCH) can never run old work again; one owned by another user
/// (EPERM) was never ours; one whose leader started at a different time than
/// recorded is a reused id, not the old group. With no recorded start (0) the
/// group's existence alone counts; a group whose leader already exited but
/// whose members live still counts (its id cannot be reused meanwhile).
pub(super) fn surviving(groups: &[(u32, u64)]) -> Vec<(u32, u64)> {
    groups
        .iter()
        .copied()
        .filter(|&(group, start)| {
            let Ok(id) = i32::try_from(group) else {
                return false;
            };
            if id <= 1 || nix::sys::signal::killpg(nix::unistd::Pid::from_raw(id), None).is_err() {
                return false;
            }
            start == 0 || leader_start(id).is_none_or(|now| now == start)
        })
        .collect()
}
/// When a process started, in an OS-specific monotonic unit (Linux: clock
/// ticks since boot; macOS: microseconds since the epoch). `None` when the
/// process is gone or unreadable.
pub(in crate::pro) fn leader_start(pid: i32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Fields after the parenthesized command: state is the first, the
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
                pid,
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
/// Re-probe previous-life evidence; a workspace is released as soon as none
/// of its recorded groups remains. Unprovable (empty) records stay.
pub(in crate::pro) fn reprobe(state: &AppState) {
    lock(&state.pro.execution.unclean).retain(|_, groups| {
        if groups.is_empty() {
            return true;
        }
        *groups = surviving(groups);
        !groups.is_empty()
    });
}
/// Process groups of this life's live managed agents and their leaders'
/// start times, recorded with every state write (and right after a managed
/// launch, see `prepare_launch`) so a crash leaves probe-able evidence.
pub(in crate::pro) fn record_groups(state: &AppState) {
    let sessions: Vec<(String, String)> = lock(&state.session_workspaces)
        .iter()
        .map(|(session, workspace)| (session.clone(), workspace.clone()))
        .collect();
    let mut by_workspace: HashMap<String, GroupEvidence> = HashMap::new();
    for (session, workspace) in sessions {
        let agent = lock(&state.agents).contains_key(&session);
        let group = state.chat.process_group(&session).or_else(|| {
            agent
                .then(|| state.sessions.get(&session).filter(|s| s.alive)?.pid)
                .flatten()
        });
        if let Some(group) = group {
            by_workspace.entry(workspace).or_default().record(group);
        }
    }
    for (workspace, entry) in lock(&state.pro.execution.setups).iter() {
        if let Some((group, _)) = entry.group {
            by_workspace
                .entry(workspace.clone())
                .or_default()
                .record(group);
        }
    }
    let unknown: HashSet<_> = lock(&state.pro.execution.unclean)
        .iter()
        .filter(|(_, groups)| groups.is_empty())
        .map(|(workspace, _)| workspace.clone())
        .collect();
    let mut preferences = lock(&state.pro.preferences);
    for (workspace, preference) in preferences.iter_mut() {
        if preference.execution_active {
            let mut evidence = by_workspace.remove(workspace).unwrap_or_default();
            evidence.groups.sort_unstable();
            preference.execution_groups_overflow = evidence.overflow
                || (preference.execution_groups_overflow && unknown.contains(workspace));
            preference.execution_groups = evidence.groups.iter().map(|(group, _)| *group).collect();
            preference.execution_starts = evidence.groups.iter().map(|(_, start)| *start).collect();
        } else {
            preference.execution_groups.clear();
            preference.execution_starts.clear();
            preference.execution_groups_overflow = false;
        }
    }
}

#[derive(Default)]
struct GroupEvidence {
    groups: Vec<(u32, u64)>,
    overflow: bool,
}
impl GroupEvidence {
    fn record(&mut self, group: u32) {
        if self.groups.len() == 64 {
            self.overflow = true;
            return;
        }
        let start = i32::try_from(group)
            .ok()
            .and_then(leader_start)
            .unwrap_or(0);
        self.groups.push((group, start));
    }
}
/// Graceful shutdown: stop this life's managed agents and, once none remains,
/// clear the evidence so a same-boot successor starts clean. Anything still
/// running keeps its recorded groups for the successor's probe.
pub(in crate::pro) async fn shutdown(state: &std::sync::Arc<AppState>) -> Result<()> {
    let workspaces: Vec<String> = lock(&state.pro.preferences)
        .iter()
        .filter(|(_, p)| p.execution_active)
        .map(|(id, _)| id.clone())
        .collect();
    if workspaces.is_empty() {
        return Ok(());
    }
    let managed: Vec<String> = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, workspace)| workspaces.contains(workspace))
        .map(|(id, _)| id.clone())
        .filter(|id| super::managed_session(state, id))
        .collect();
    for id in &managed {
        if state.chat.get(id).is_some_and(|s| s.alive) {
            state.chat.fence(id);
        } else if state.sessions.get(id).is_some_and(|s| s.alive) {
            let _ = state.sessions.kill(id);
        }
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(4), async {
        while managed.iter().any(|id| {
            state.chat.get(id).is_some_and(|s| s.alive)
                || state.sessions.get(id).is_some_and(|s| s.alive)
        }) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    let running: std::collections::HashSet<String> = managed
        .iter()
        .filter(|id| {
            state.chat.get(id).is_some_and(|s| s.alive)
                || state.sessions.get(id).is_some_and(|s| s.alive)
        })
        .filter_map(|id| lock(&state.session_workspaces).get(id).cloned())
        .collect();
    let unproven: std::collections::HashSet<String> = workspaces
        .iter()
        .filter(|workspace| unclean(state, workspace))
        .cloned()
        .collect();
    {
        let mut preferences = lock(&state.pro.preferences);
        for workspace in &workspaces {
            if let Some(preference) = preferences.get_mut(workspace) {
                if !running.contains(workspace) && !unproven.contains(workspace) {
                    preference.execution_active = false;
                    preference.execution_groups.clear();
                    preference.execution_starts.clear();
                }
            }
        }
    }
    crate::pro::persist(state).await
}
pub(in crate::pro) async fn persist_latch(state: &AppState) -> Result<()> {
    let mut workspaces: Vec<_> = lock(&state.pro.execution.latched).iter().cloned().collect();
    if workspaces.is_empty() {
        return Ok(());
    }
    ensure!(workspaces.len() <= 128, "execution workspace limit");
    workspaces.sort();
    let bytes = serde_json::to_vec(&Latch {
        version: 1,
        workspaces,
    })?;
    let path = state.pro.root.join("execution-authority.json");
    tokio::task::spawn_blocking(move || crate::persist::atomic_write_json_durable(&path, bytes))
        .await??;
    Ok(())
}
/// The OS boot identifier changes only on a new kernel boot, where old local
/// processes cannot survive. Suspend/hibernation keeps the same identifier.
fn boot_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let mut value = String::new();
        std::fs::File::open("/proc/sys/kernel/random/boot_id")
            .ok()?
            .take(65)
            .read_to_string(&mut value)
            .ok()?;
        let value = value.trim();
        (value.len() == 36 && value.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
            .then(|| value.into())
    }
    #[cfg(target_os = "macos")]
    {
        let mut value = [0u8; 37];
        let mut size = value.len();
        // A boot-session UUID is independent of wall-clock corrections. Boot
        // time is not: using it could mistake a clock adjustment for a reboot.
        let result = unsafe {
            nix::libc::sysctlbyname(
                c"kern.bootsessionuuid".as_ptr(),
                value.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 || size != value.len() || value[36] != 0 {
            return None;
        }
        let value = std::str::from_utf8(&value[..36]).ok()?;
        value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'-')
            .then(|| value.into())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn omitted_live_group_keeps_restart_evidence_unknown_across_reprobes_and_writes() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-evidence-overflow-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let mut evidence = GroupEvidence::default();
        for offset in 0..64 {
            evidence.record(u32::MAX - offset);
        }
        evidence.record(child.id());
        assert!(evidence.overflow);
        assert_eq!(evidence.groups.len(), 64);
        assert!(!evidence
            .groups
            .iter()
            .any(|(group, _)| *group == child.id()));
        assert!(surviving(&evidence.groups).is_empty());
        assert!(!surviving(&[(child.id(), 0)]).is_empty());

        let mut state = AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        );
        super::super::install_fixture(&state, "w-a", 4).unwrap();
        {
            let mut preferences = lock(&state.pro.preferences);
            let preference = preferences.get_mut("w-a").unwrap();
            preference.execution_active = true;
            preference.execution_boot = state.pro.execution.boot.clone();
            preference.execution_groups = evidence.groups.iter().map(|(group, _)| *group).collect();
            preference.execution_starts = evidence.groups.iter().map(|(_, start)| *start).collect();
            preference.execution_groups_overflow = evidence.overflow;
        }
        let disk = serde_json::to_vec(&*lock(&state.pro.preferences)).unwrap();
        let preferences = serde_json::from_slice(&disk).unwrap();
        state.pro.execution = State::restore(&root, &preferences, true, false);
        state.pro.worker.store(true, Ordering::Release);
        assert!(lock(&state.pro.execution.unclean)["w-a"].is_empty());
        reprobe(&state);
        assert!(!super::super::quiescent(&state, "w-a"));
        assert!(!super::super::allows(&state, "w-a"));
        // An ordinary state flush must not erase the overflow just because
        // this successor's process registry has no old child in it.
        record_groups(&state);
        assert!(lock(&state.pro.preferences)["w-a"].execution_groups_overflow);
        let disk = serde_json::to_vec(&*lock(&state.pro.preferences)).unwrap();
        let preferences = serde_json::from_slice(&disk).unwrap();
        state.pro.execution = State::restore(&root, &preferences, true, false);
        reprobe(&state);
        assert!(!super::super::quiescent(&state, "w-a"));
        child.kill().unwrap();
        child.wait().unwrap();
        reprobe(&state);
        assert!(
            !super::super::quiescent(&state, "w-a"),
            "the omitted set remains unprovable"
        );
        // Laptop execution remains available; publication still requires
        // proving the previous workload stopped.
        state.pro.worker.store(false, Ordering::Release);
        assert!(super::super::allows(&state, "w-a"));
        assert!(!super::super::quiescent(&state, "w-a"));
        if state.pro.execution.boot.is_some() {
            lock(&state.pro.preferences)
                .get_mut("w-a")
                .unwrap()
                .execution_boot = Some("different-previous-boot".into());
            let preferences = lock(&state.pro.preferences).clone();
            state.pro.execution = State::restore(&root, &preferences, true, false);
            assert!(
                super::super::quiescent(&state, "w-a"),
                "a cold boot proves old groups cannot survive"
            );
            record_groups(&state);
            assert!(!lock(&state.pro.preferences)["w-a"].execution_groups_overflow);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
