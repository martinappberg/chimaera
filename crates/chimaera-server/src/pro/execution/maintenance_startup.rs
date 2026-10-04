//! Disabled inherited idle-channel bootstrap. Only a fixed launch receipt may
//! move an owned descriptor here; this is neither an idle proof nor a Ready
//! producer. The launcher must establish the private project UID/namespace.
use chimaera_core::project_secret_idle::{Binding, Ready, Reply};
use std::os::fd::{AsRawFd, OwnedFd};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Control {
    pub(super) version: u16,
    pub(super) fd: i32,
    pub(super) channel_nonce: String,
}
impl Control {
    pub(super) fn validate(&self, binding: &Binding) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == 1 && self.fd > 2,
            "invalid maintenance startup descriptor"
        );
        Reply::Ready(Ready {
            version: 1,
            binding: binding.clone(),
            channel_nonce: self.channel_nonce.clone(),
            project_secrets_idle: 1,
        })
        .validate()
        .map_err(|_| anyhow::anyhow!("invalid maintenance startup binding"))
    }
}

/// No Clone: one startup owns the descriptor, separately from cloneable cleanup
/// metadata. No public constructor or field lets an HTTP claim select a socket.
/// Its protection proof is not a positive process census or idle receipt.
pub(super) struct Pending {
    descriptor: OwnedFd,
    binding: Binding,
    channel_nonce: String,
    _protected: Protection,
}
/// Sealed to trusted startup. Only tests and the explicit nondefault host
/// fixture constructor can mint synthetic evidence; ordinary builds cannot.
pub(super) struct Protection {
    pid: u32,
    #[cfg(any(test, feature = "daemon-extension-fixture"))]
    synthetic: bool,
}
impl Protection {
    #[cfg(all(target_os = "linux", feature = "provider-authority-prototype"))]
    pub(super) fn startup() -> anyhow::Result<Self> {
        protect_process()
    }
    #[cfg(all(target_os = "linux", feature = "provider-authority-prototype"))]
    fn checked_copy(&self) -> anyhow::Result<Self> {
        self.current()?;
        Ok(Self {
            pid: self.pid,
            #[cfg(any(test, feature = "daemon-extension-fixture"))]
            synthetic: self.synthetic,
        })
    }
    pub(super) fn current(&self) -> anyhow::Result<()> {
        #[cfg(any(test, feature = "daemon-extension-fixture"))]
        if self.synthetic {
            return Ok(());
        }
        anyhow::ensure!(
            self.pid == std::process::id(),
            "maintenance process changed"
        );
        verify_process()
    }
    #[cfg(any(test, feature = "daemon-extension-fixture"))]
    pub(super) fn synthetic() -> Self {
        Self {
            pid: std::process::id(),
            synthetic: true,
        }
    }
}
impl Pending {
    #[cfg(all(target_os = "linux", feature = "provider-authority-prototype"))]
    pub(super) fn protection(&self) -> anyhow::Result<Protection> {
        self._protected.checked_copy()
    }
    pub(super) fn transferred(
        descriptor: OwnedFd,
        control: Control,
        binding: Binding,
    ) -> anyhow::Result<Self> {
        control.validate(&binding)?;
        anyhow::ensure!(
            descriptor.as_raw_fd() == control.fd,
            "maintenance startup descriptor mismatch"
        );
        #[cfg(target_os = "linux")]
        {
            validate_socket(&descriptor)?;
            let protected = protect_process()?;
            Ok(Self {
                descriptor,
                binding,
                channel_nonce: control.channel_nonce,
                _protected: protected,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = descriptor;
            anyhow::bail!("maintenance startup requires Linux protections")
        }
    }
    pub(super) fn into_channel(
        self,
        state: &crate::AppState,
    ) -> anyhow::Result<super::maintenance_channel::Channel> {
        super::maintenance_channel::Channel::from_protected(
            state,
            self.descriptor,
            self.binding,
            self.channel_nonce,
            self._protected,
        )
        .map_err(|_| anyhow::anyhow!("maintenance accepted launch changed"))
    }
}

#[cfg(target_os = "linux")]
fn validate_socket(descriptor: &OwnedFd) -> anyhow::Result<()> {
    let raw = descriptor.as_raw_fd();
    let mut kind: nix::libc::c_int = 0;
    let mut len = std::mem::size_of_val(&kind) as nix::libc::socklen_t;
    anyhow::ensure!(
        unsafe {
            nix::libc::getsockopt(
                raw,
                nix::libc::SOL_SOCKET,
                nix::libc::SO_TYPE,
                (&mut kind as *mut nix::libc::c_int).cast(),
                &mut len,
            )
        } == 0
            && len as usize == std::mem::size_of_val(&kind)
            && kind == nix::libc::SOCK_STREAM,
        "maintenance startup requires a stream socket"
    );
    let mut address: nix::libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut address_len = std::mem::size_of_val(&address) as nix::libc::socklen_t;
    anyhow::ensure!(
        unsafe {
            nix::libc::getsockname(
                raw,
                (&mut address as *mut nix::libc::sockaddr_storage).cast(),
                &mut address_len,
            )
        } == 0
            && address.ss_family as nix::libc::c_int == nix::libc::AF_UNIX,
        "maintenance startup requires a Unix stream"
    );
    // UnixStream's address decoder refuses another socket family; only both
    // unnamed endpoints are eligible. Inspect a CLOEXEC duplicate, never take
    // ownership of the same raw descriptor twice.
    let duplicate = descriptor
        .try_clone()
        .map_err(|_| anyhow::anyhow!("maintenance startup socket unavailable"))?;
    let socket = std::os::unix::net::UnixStream::from(duplicate);
    for address in [socket.local_addr(), socket.peer_addr()] {
        anyhow::ensure!(
            address
                .map_err(|_| anyhow::anyhow!("maintenance startup socket unavailable"))?
                .is_unnamed(),
            "maintenance startup socket must be unnamed"
        );
    }
    let flags = unsafe { nix::libc::fcntl(raw, nix::libc::F_GETFD) };
    anyhow::ensure!(
        flags >= 0
            && unsafe { nix::libc::fcntl(raw, nix::libc::F_SETFD, flags | nix::libc::FD_CLOEXEC) }
                == 0,
        "maintenance startup descriptor protection failed"
    );
    socket
        .set_nonblocking(true)
        .map_err(|_| anyhow::anyhow!("maintenance startup socket unavailable"))?;
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn unprivileged_status(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > 16 * 1024 {
        return false;
    }
    let Ok(status) = std::str::from_utf8(bytes) else {
        return false;
    };
    // Bounding capabilities alone is insufficient: no_new_privs must already
    // come from the fixed launcher and prevent a child exec from gaining them.
    // Nonzero equal real/effective/saved/fs IDs exclude a privileged daemon.
    let mut seen = std::collections::BTreeSet::new();
    for line in status.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !matches!(
            name,
            "Uid" | "Gid" | "Groups" | "CapInh" | "CapPrm" | "CapEff" | "CapAmb" | "NoNewPrivs"
        ) {
            continue;
        }
        if !seen.insert(name) {
            return false;
        }
        let fields: Vec<_> = value.split_ascii_whitespace().collect();
        let valid = match name {
            "Uid" | "Gid" => {
                fields.len() == 4
                    && fields[0].parse::<u32>().is_ok_and(|id| id != 0)
                    && fields.iter().all(|id| *id == fields[0])
            }
            "Groups" => fields.is_empty(),
            "NoNewPrivs" => fields == ["1"],
            _ => fields.len() == 1 && fields[0].len() == 16 && fields[0].bytes().all(|b| b == b'0'),
        };
        if !valid {
            return false;
        }
    }
    seen.len() == 8
}

#[cfg(target_os = "linux")]
fn protect_process() -> anyhow::Result<Protection> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open("/proc/self/status")
        .and_then(|file| file.take(16 * 1024 + 1).read_to_end(&mut bytes))
        .map_err(|_| anyhow::anyhow!("maintenance process protections unavailable"))?;
    anyhow::ensure!(
        unprivileged_status(&bytes),
        "maintenance requires unprivileged fixed launch"
    );
    anyhow::ensure!(
        unsafe { nix::libc::prctl(nix::libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } == 1,
        "maintenance privilege gate unavailable"
    );
    anyhow::ensure!(
        unsafe { nix::libc::prctl(nix::libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } == 0
            && unsafe { nix::libc::prctl(nix::libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } == 0,
        "maintenance process protections unavailable"
    );
    verify_process()?;
    Ok(Protection {
        pid: std::process::id(),
        #[cfg(any(test, feature = "daemon-extension-fixture"))]
        synthetic: false,
    })
}

/// Never repairs changed protections. A Prepared receipt must come from the
/// same protected process, independently of the supervisor's kernel census.
fn verify_process() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    anyhow::ensure!(
        unsafe { nix::libc::prctl(nix::libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } == 0
            && unsafe { nix::libc::prctl(nix::libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } == 1,
        "maintenance process protections changed"
    );
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("maintenance process protections require Linux");
    #[cfg(target_os = "linux")]
    Ok(())
}

#[cfg(test)]
#[path = "maintenance_startup_tests.rs"]
mod tests;
