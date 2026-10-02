//! One-shot inherited supervisor evidence. This is never an HTTP stop assertion.
//! It clears crash uncertainty only after exact scoped configuration, and cannot
//! create an execution lease or authorize remote takeover.
use super::*;
#[cfg(any(target_os = "linux", test))]
use std::io::Read;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RootIdentity {
    device: u64,
    inode: u64,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CleanupReceipt {
    version: u16,
    workspace_id: String,
    account_id: String,
    root_identity: RootIdentity,
    registration_revision: u64,
    launch_generation: u64,
    previous_generation: u64,
    os_boot_id: String,
}
#[derive(Clone, serde::Serialize)]
pub(crate) struct CleanupAck {
    execution_cleanup: u16,
    workspace_id: String,
    registration_revision: u64,
    launch_generation: u64,
}
#[cfg(any(target_os = "linux", test))]
fn decode(bytes: &[u8]) -> Result<CleanupReceipt> {
    ensure!(
        bytes.len() <= 4096,
        "supervisor cleanup receipt exceeds limit"
    );
    let receipt: CleanupReceipt = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("invalid supervisor cleanup receipt"))?;
    ensure!(
        receipt.version == 1
            && crate::pro::valid_id(&receipt.workspace_id)
            && crate::pro::valid_id(&receipt.account_id)
            && receipt.registration_revision > 0
            && receipt.launch_generation > receipt.previous_generation
            && receipt.root_identity.inode > 0
            && receipt.os_boot_id.len() == 36
            && receipt
                .os_boot_id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b'-'),
        "invalid supervisor cleanup binding"
    );
    Ok(receipt)
}
#[cfg(any(target_os = "linux", test))]
fn read_pipe(fd: std::os::fd::OwnedFd) -> Result<CleanupReceipt> {
    use std::os::fd::AsRawFd;
    let raw = fd.as_raw_fd();
    // The descriptor belongs to this function and is closed on every exit.
    let flags = unsafe { nix::libc::fcntl(raw, nix::libc::F_GETFD) };
    ensure!(
        flags >= 0
            && unsafe { nix::libc::fcntl(raw, nix::libc::F_SETFD, flags | nix::libc::FD_CLOEXEC) }
                == 0,
        "could not protect supervisor channel"
    );
    let mut file = std::fs::File::from(fd);
    use std::os::unix::fs::FileTypeExt;
    ensure!(
        file.metadata()?.file_type().is_fifo(),
        "supervisor channel is not a pipe"
    );
    let flags = unsafe { nix::libc::fcntl(raw, nix::libc::F_GETFL) };
    ensure!(
        flags >= 0
            && flags & nix::libc::O_ACCMODE == nix::libc::O_RDONLY
            && unsafe { nix::libc::fcntl(raw, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) }
                == 0,
        "could not bound supervisor channel"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4097];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                ensure!(
                    bytes.len() + count <= 4096,
                    "supervisor cleanup receipt exceeds limit"
                );
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                ensure!(
                    std::time::Instant::now() < deadline,
                    "supervisor cleanup channel timed out"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                ensure!(
                    std::time::Instant::now() < deadline,
                    "supervisor cleanup channel timed out"
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
    decode(&bytes)
}
pub(crate) async fn read_startup() -> Result<Option<CleanupReceipt>> {
    let Some(fd) = std::env::var_os("CHIMAERA_SUPERVISOR_CLEANUP_FD") else {
        return Ok(None);
    };
    ensure!(fd == "0", "invalid supervisor cleanup descriptor");
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::FromRawFd;
        // The opt-in launcher transfers ownership of exactly descriptor 0.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(0) };
        tokio::task::spawn_blocking(move || read_pipe(fd))
            .await?
            .map(Some)
    }
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("supervisor cleanup channel is only supported on Linux")
}
pub(crate) fn stage(state: &AppState, receipt: Option<CleanupReceipt>) {
    *lock(&state.pro.execution.supervisor_pending) = receipt;
}
pub(in crate::pro) fn pending(state: &AppState) -> bool {
    lock(&state.pro.execution.supervisor_pending).is_some()
}
pub(crate) fn ack(state: &AppState) -> Option<CleanupAck> {
    lock(&state.pro.execution.supervisor_ack).clone()
}
pub(super) fn supervised(state: &AppState) -> bool {
    lock(&state.pro.execution.supervisor_ack).is_some()
}
pub(in crate::pro) async fn apply(
    state: &AppState,
    accepted: Option<&crate::pro::authority::Accepted>,
) -> Result<()> {
    let Some(receipt) = lock(&state.pro.execution.supervisor_pending).clone() else {
        return Ok(());
    };
    let accepted = accepted.context("supervisor cleanup requires workspace-bound configuration")?;
    ensure!(
        !state.pro.configured.load(Ordering::Acquire)
            && lock(&state.pro.execution.proofs).is_empty(),
        "supervisor cleanup is startup-only"
    );
    ensure!(
        accepted.cleanup_binding(
            &receipt.account_id,
            &receipt.workspace_id,
            receipt.registration_revision,
            (receipt.root_identity.device, receipt.root_identity.inode)
        ),
        "supervisor cleanup identity mismatch"
    );
    ensure!(
        state.pro.execution.boot.as_deref() == Some(&receipt.os_boot_id),
        "supervisor cleanup boot mismatch"
    );
    ensure!(
        !state.pro.execution.supervisor_state_invalid,
        "managed state requires recovery"
    );
    let ids = lock(&state.session_workspaces)
        .iter()
        .filter(|(_, w)| *w == &receipt.workspace_id)
        .map(|(s, _)| s.clone())
        .collect::<Vec<_>>();
    ensure!(
        mutation::idle(state, &receipt.workspace_id)
            && !setup::active(state, &receipt.workspace_id)
            && ids
                .iter()
                .all(|id| !state.chat.get(id).is_some_and(|v| v.alive)
                    && !state.sessions.get(id).is_some_and(|v| v.alive)),
        "supervisor cleanup cannot replace live local work"
    );
    let previous = {
        let mut preferences = lock(&state.pro.preferences);
        ensure!(
            preferences.len() < 128 || preferences.contains_key(&receipt.workspace_id),
            "supervisor workspace limit"
        );
        let previous = preferences.get(&receipt.workspace_id).cloned();
        let entry = preferences.entry(receipt.workspace_id.clone()).or_default();
        let last = entry.supervisor_generation.unwrap_or(0);
        ensure!(
            receipt.launch_generation > last && receipt.previous_generation >= last,
            "supervisor cleanup generation replay"
        );
        entry.supervisor_generation = Some(receipt.launch_generation);
        entry.execution_active = false;
        entry.execution_launch_pending = false;
        entry.execution_groups_overflow = false;
        entry.execution_groups.clear();
        entry.execution_starts.clear();
        previous
    };
    // The strict worker role, generation and cleared evidence share one write.
    // A later failure configuring authority cannot turn this into a free daemon.
    let previous_worker = state.pro.worker.swap(true, Ordering::AcqRel);
    if let Err(error) = crate::pro::persist(state).await {
        state.pro.worker.store(previous_worker, Ordering::Release);
        let mut preferences = lock(&state.pro.preferences);
        if let Some(previous) = previous {
            preferences.insert(receipt.workspace_id.clone(), previous);
        } else {
            preferences.remove(&receipt.workspace_id);
        }
        return Err(error);
    }
    // The supervisor proved the complete UID tree dead. Do not weaken missing
    // enrollment policy; only an authoritative account observation repairs it.
    lock(&state.pro.execution.unclean).remove(&receipt.workspace_id);
    *lock(&state.pro.execution.supervisor_ack) = Some(CleanupAck {
        execution_cleanup: 1,
        workspace_id: receipt.workspace_id,
        registration_revision: receipt.registration_revision,
        launch_generation: receipt.launch_generation,
    });
    *lock(&state.pro.execution.supervisor_pending) = None;
    Ok(())
}
#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
