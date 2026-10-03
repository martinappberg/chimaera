//! One inherited, protected project capability. Consuming it is not Ready:
//! execution remains fenced until an exact runtime attachment is implemented.
use anyhow::{ensure, Result};
use chimaera_core::{project_secret_idle::Binding, provider_runtime as wire};
use std::{
    io::Read,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::FileTypeExt,
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// No Clone/Debug/serialization: the secret is moved once and never joins the
/// cleanup receipt, health response, environment or durable project state.
#[expect(
    dead_code,
    reason = "exact Ready transport is the next disabled integration gate"
)]
pub(super) struct Pending {
    payload: wire::StartupPayload,
    launch: Binding,
    protection: super::maintenance_startup::Protection,
}
impl Pending {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(super) fn transferred(
        descriptor: OwnedFd,
        control: wire::StartupDescriptor,
        launch: Binding,
        deadline: Instant,
        protection: super::maintenance_startup::Protection,
    ) -> Result<Self> {
        control
            .validate(None)
            .map_err(|_| anyhow::anyhow!("invalid provider startup descriptor"))?;
        ensure!(
            descriptor.as_raw_fd() == control.fd,
            "provider startup descriptor mismatch"
        );
        protection.current()?;
        let bytes = read_pipe(descriptor, deadline, &protection)?;
        let payload = wire::StartupPayload::decode(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid provider startup payload"))?;
        ensure!(
            payload.binding.account_id == launch.account_id
                && payload.binding.workspace_id == launch.workspace_id
                && payload.binding.project_revision == launch.registration_revision
                && payload.binding.launch_generation == launch.launch_generation,
            "provider startup launch mismatch"
        );
        protection.current()?;
        ensure!(
            Instant::now() < deadline,
            "provider startup channel timed out"
        );
        Ok(Self {
            payload,
            launch,
            protection,
        })
    }
}

/// This fixed-launch project cannot fall back to credentials from legacy HOME
/// while the provider attachment is inert, missing or unverified. Configure
/// and health remain available so the supervisor can establish exact admission.
pub(in crate::pro) fn allows(state: &crate::AppState) -> bool {
    crate::lock(&state.pro.execution.provider_pending).is_none()
}

fn read_pipe(
    descriptor: OwnedFd,
    deadline: Instant,
    protection: &super::maintenance_startup::Protection,
) -> Result<Zeroizing<Vec<u8>>> {
    let raw = descriptor.as_raw_fd();
    let flags = unsafe { nix::libc::fcntl(raw, nix::libc::F_GETFD) };
    ensure!(
        flags >= 0
            && unsafe { nix::libc::fcntl(raw, nix::libc::F_SETFD, flags | nix::libc::FD_CLOEXEC) }
                == 0,
        "could not protect provider startup channel"
    );
    let mut file = std::fs::File::from(descriptor);
    ensure!(
        file.metadata()?.file_type().is_fifo(),
        "provider startup channel is not a pipe"
    );
    let flags = unsafe { nix::libc::fcntl(raw, nix::libc::F_GETFL) };
    ensure!(
        flags >= 0
            && flags & nix::libc::O_ACCMODE == nix::libc::O_RDONLY
            && unsafe { nix::libc::fcntl(raw, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) }
                == 0,
        "could not bound provider startup channel"
    );
    // Allocate once before the first secret byte, including the over-limit
    // sentinel. Growth/reallocation must not leave an old capability buffer.
    let mut bytes = Zeroizing::new(vec![0; wire::STARTUP_MAX + 1]);
    let mut length = 0;
    loop {
        protection.current()?;
        ensure!(
            Instant::now() < deadline,
            "provider startup channel timed out"
        );
        match file.read(&mut bytes[length..]) {
            Ok(0) => break,
            Ok(count) => {
                length += count;
                ensure!(
                    length <= wire::STARTUP_MAX,
                    "provider startup payload exceeds limit"
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(
                    Duration::from_millis(5)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => anyhow::bail!("provider startup channel unavailable"),
        }
    }
    bytes.truncate(length);
    Ok(bytes)
}

#[cfg(test)]
#[path = "provider_startup_tests.rs"]
mod tests;
