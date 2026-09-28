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
        let mut invalid = loaded.is_err();
        let mut latched = loaded.unwrap_or_default();
        invalid |= latched
            .iter()
            .any(|id| preferences.get(id).is_none_or(|p| p.continuity.is_none()));
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
            .map(|(id, _)| id.clone())
            .collect();
        Self {
            proofs: Mutex::default(),
            commits: mutation::Commits::default(),
            latched: Mutex::new(latched),
            unclean,
            invalid,
            boot,
        }
    }
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
    tokio::task::spawn_blocking(move || crate::persist::atomic_write_json(&path, bytes)).await??;
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
        return (value.len() == 36 && value.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
            .then(|| value.into());
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
