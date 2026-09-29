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
        damaged: bool,
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
        // A project is uncertain when the enrollment record may once have
        // covered it but its policy is gone: a latched project without a
        // policy, or, when the latch or the state itself is unreadable, every
        // project this daemon has local mirror data or preferences for. Only
        // those projects stop publishing until the account confirms their
        // policy again; everything else keeps its ordinary behavior.
        let unknown = loaded.is_err() || damaged;
        let mut latched = loaded.unwrap_or_default();
        let lacking = |id: &String| preferences.get(id).is_none_or(|p| p.continuity.is_none());
        let mut uncertain: HashSet<String> =
            latched.iter().filter(|id| lacking(id)).cloned().collect();
        if unknown {
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
                // Probe what the previous life recorded instead of fencing
                // forever. With no recorded group a device proceeds (laptop
                // first); a worker cannot prove anything and stays strict.
                let alive = surviving(&p.execution_groups);
                (!alive.is_empty() || (p.execution_groups.is_empty() && worker))
                    .then(|| (id.clone(), alive))
            })
            .collect();
        Self {
            proofs: Mutex::default(),
            commits: mutation::Commits::default(),
            latched: Mutex::new(latched),
            unclean: Mutex::new(unclean),
            uncertain: Mutex::new(uncertain),
            boot,
        }
    }
}
/// Recorded process groups that still exist. A group that has vanished can
/// never run old work again; a reused group id errs toward waiting.
fn surviving(groups: &[u32]) -> Vec<u32> {
    groups
        .iter()
        .copied()
        .filter(|group| {
            i32::try_from(*group).is_ok_and(|group| {
                group > 1
                    && !matches!(
                        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(group), None),
                        Err(nix::errno::Errno::ESRCH)
                    )
            })
        })
        .collect()
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
/// Process groups of this life's live managed agents, recorded with every
/// state write so a crash leaves probe-able evidence behind.
pub(in crate::pro) fn record_groups(state: &AppState) {
    let sessions: Vec<(String, String)> = lock(&state.session_workspaces)
        .iter()
        .map(|(session, workspace)| (session.clone(), workspace.clone()))
        .collect();
    let mut by_workspace: HashMap<String, Vec<u32>> = HashMap::new();
    for (session, workspace) in sessions {
        let agent = lock(&state.agents).contains_key(&session);
        let group = state.chat.process_group(&session).or_else(|| {
            agent
                .then(|| state.sessions.get(&session).filter(|s| s.alive)?.pid)
                .flatten()
        });
        if let Some(group) = group {
            let groups = by_workspace.entry(workspace.clone()).or_default();
            if groups.len() < 64 {
                groups.push(group);
            }
        }
    }
    let mut preferences = lock(&state.pro.preferences);
    for (workspace, preference) in preferences.iter_mut() {
        if preference.execution_active {
            let mut groups = by_workspace.remove(workspace).unwrap_or_default();
            groups.sort_unstable();
            preference.execution_groups = groups;
        } else {
            preference.execution_groups.clear();
        }
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
