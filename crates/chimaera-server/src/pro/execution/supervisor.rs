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
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_runtime: Option<chimaera_core::provider_runtime::StartupDescriptor>,
}
/// Startup moves once; provider credentials stay separate from cleanup metadata.
pub(crate) struct Startup {
    receipt: CleanupReceipt,
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    provider: Option<super::provider_startup::Pending>,
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
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    if let Some(provider) = &receipt.provider_runtime {
        provider
            .validate(None)
            .map_err(|_| anyhow::anyhow!("invalid provider startup descriptor"))?;
    }
    Ok(receipt)
}
#[cfg(all(target_os = "linux", feature = "provider-authority-prototype"))]
impl CleanupReceipt {
    fn provider_binding(&self) -> chimaera_core::project_secret_idle::Binding {
        use chimaera_core::project_secret_idle::{Binding, RootIdentity};
        Binding {
            account_id: self.account_id.clone(),
            workspace_id: self.workspace_id.clone(),
            root_identity: RootIdentity {
                device: self.root_identity.device,
                inode: self.root_identity.inode,
            },
            registration_revision: self.registration_revision,
            launch_generation: self.launch_generation,
            os_boot_id: self.os_boot_id.clone(),
        }
    }
}
#[cfg(target_os = "linux")]
fn own_startup_until(receipt: CleanupReceipt, deadline: std::time::Instant) -> Result<Startup> {
    #[cfg(feature = "provider-authority-prototype")]
    use std::os::fd::FromRawFd;
    #[cfg(feature = "provider-authority-prototype")]
    let mut receipt = receipt;
    #[cfg(not(feature = "provider-authority-prototype"))]
    let _ = deadline;
    #[cfg(feature = "provider-authority-prototype")]
    let provider = match receipt.provider_runtime.take() {
        None => None,
        Some(provider) => {
            provider
                .validate(None)
                .map_err(|_| anyhow::anyhow!("invalid provider startup descriptor"))?;
            ensure!(
                unsafe { nix::libc::fcntl(provider.fd, nix::libc::F_GETFD) } >= 0,
                "provider startup descriptor unavailable"
            );
            // Take ownership before any later protection/read failure. This
            // startup-only path runs before children, descriptor clones or IO.
            let descriptor = unsafe { std::os::fd::OwnedFd::from_raw_fd(provider.fd) };
            Some((descriptor, provider))
        }
    };
    #[cfg(feature = "provider-authority-prototype")]
    let provider = match provider {
        None => None,
        Some((descriptor, control)) => {
            let protection = super::provider_protection::Protection::startup()?;
            Some(super::provider_startup::Pending::transferred(
                descriptor,
                control,
                receipt.provider_binding(),
                deadline,
                protection,
            )?)
        }
    };
    Ok(Startup {
        receipt,
        #[cfg(feature = "provider-authority-prototype")]
        provider,
    })
}
#[cfg(all(test, target_os = "linux"))]
fn own_startup(receipt: CleanupReceipt) -> Result<Startup> {
    own_startup_until(receipt, std::time::Instant::now() + Duration::from_secs(3))
}
#[cfg(any(target_os = "linux", test))]
fn read_pipe_until(
    fd: std::os::fd::OwnedFd,
    deadline: std::time::Instant,
) -> Result<CleanupReceipt> {
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
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4097];
    loop {
        ensure!(
            std::time::Instant::now() < deadline,
            "supervisor cleanup channel timed out"
        );
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
#[cfg(test)]
fn read_pipe(fd: std::os::fd::OwnedFd) -> Result<CleanupReceipt> {
    read_pipe_until(fd, std::time::Instant::now() + Duration::from_secs(3))
}
pub(crate) async fn read_startup() -> Result<Option<Startup>> {
    let Some(fd) = std::env::var_os("CHIMAERA_SUPERVISOR_CLEANUP_FD") else {
        return Ok(None);
    };
    ensure!(fd == "0", "invalid supervisor cleanup descriptor");
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::FromRawFd;
        static READ: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        ensure!(
            !READ.swap(true, Ordering::AcqRel),
            "supervisor startup already consumed"
        );
        // The opt-in launcher transfers ownership of exactly descriptor 0.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(0) };
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        tokio::task::spawn_blocking(move || {
            own_startup_until(read_pipe_until(fd, deadline)?, deadline)
        })
        .await?
        .map(Some)
    }
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("supervisor cleanup channel is only supported on Linux")
}
pub(crate) fn stage_startup(state: &AppState, startup: Option<Startup>) -> Result<()> {
    let Some(startup) = startup else {
        return Ok(());
    };
    let mut pending = lock(&state.pro.execution.supervisor_pending);
    ensure!(pending.is_none(), "supervisor startup already staged");
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    let mut provider = {
        let provider = lock(&state.pro.execution.provider_pending);
        ensure!(provider.is_none(), "provider startup already staged");
        provider
    };
    #[cfg(all(unix, feature = "provider-authority-prototype"))]
    {
        *provider = startup.provider.map(std::sync::Arc::new);
    }
    *pending = Some(startup.receipt);
    Ok(())
}
#[cfg(any(
    test,
    all(
        feature = "daemon-extension-fixture",
        feature = "provider-authority-prototype"
    )
))]
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
/// Compare the accepted provider launch; this is not a process census.
#[cfg(all(unix, feature = "provider-authority-prototype"))]
pub(super) fn matches_provider_launch(
    state: &AppState,
    binding: &chimaera_core::project_secret_idle::Binding,
) -> bool {
    if !state.pro.configured.load(Ordering::Acquire)
        || state.pro.execution.boot.as_deref() != Some(&binding.os_boot_id)
    {
        return false;
    }
    let runtime_matches = {
        let runtime = lock(&state.pro.runtime);
        runtime.as_ref().is_some_and(|config| {
            config.role == crate::pro::protocol::Role::Worker
                && config.account_id.as_deref() == Some(&binding.account_id)
        })
    };
    let ack_matches = {
        let ack = lock(&state.pro.execution.supervisor_ack);
        ack.as_ref().is_some_and(|ack| {
            ack.workspace_id == binding.workspace_id
                && ack.registration_revision == binding.registration_revision
                && ack.launch_generation == binding.launch_generation
        })
    };
    let authority_matches = {
        let authority = lock(&state.pro.authority);
        matches!(&*authority, crate::pro::authority::Authority::Bound(accepted)
            if accepted.cleanup_binding(&binding.account_id, &binding.workspace_id,
                binding.registration_revision,(binding.root_identity.device,binding.root_identity.inode)))
    };
    runtime_matches && ack_matches && authority_matches
}
/// Shared startup admission. It has no effects and does not consume the pipe
/// receipt; both authority preparation and durable application use it.
pub(in crate::pro) fn validate(
    state: &AppState,
    accepted: &crate::pro::authority::Accepted,
) -> Result<()> {
    let receipt = lock(&state.pro.execution.supervisor_pending)
        .clone()
        .context("supervisor cleanup receipt required")?;
    validate_receipt(state, accepted, &receipt)
}
fn validate_receipt(
    state: &AppState,
    accepted: &crate::pro::authority::Accepted,
    receipt: &CleanupReceipt,
) -> Result<()> {
    ensure!(
        !state.pro.configured.load(Ordering::Acquire)
            && lock(&state.pro.runtime).is_none()
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
    ensure!(
        mutation::idle(state, &receipt.workspace_id)
            && !setup::active(state, &receipt.workspace_id)
            && !state.chat.list().iter().any(|session| session.alive)
            && !state.sessions.list().iter().any(|session| session.alive),
        "supervisor cleanup cannot replace live local work"
    );
    let preferences = lock(&state.pro.preferences);
    ensure!(
        preferences.len() < 128 || preferences.contains_key(&receipt.workspace_id),
        "supervisor workspace limit"
    );
    let last = preferences
        .get(&receipt.workspace_id)
        .and_then(|entry| entry.supervisor_generation)
        .unwrap_or(0);
    ensure!(
        receipt.launch_generation > last && receipt.previous_generation >= last,
        "supervisor cleanup generation replay"
    );
    Ok(())
}
pub(in crate::pro) async fn apply(
    state: &AppState,
    accepted: Option<&crate::pro::authority::Accepted>,
) -> Result<()> {
    let Some(receipt) = lock(&state.pro.execution.supervisor_pending).clone() else {
        return Ok(());
    };
    let accepted = accepted.context("supervisor cleanup requires workspace-bound configuration")?;
    validate_receipt(state, accepted, &receipt)?;
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
pub(in crate::pro) fn fixture_boot(state: &AppState) -> Option<String> {
    state.pro.execution.boot.clone()
}
#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
