//! Fixed-launch provider process protection; no maintenance channel or idle proof.
/// Sealed to trusted startup. Only tests and the explicit nondefault host
/// fixture constructor can mint synthetic evidence; ordinary builds cannot.
#[cfg(any(
    all(unix, feature = "provider-authority-prototype"),
    all(test, target_os = "linux")
))]
pub(super) struct Protection {
    pid: u32,
    #[cfg(any(test, feature = "daemon-extension-fixture"))]
    synthetic: bool,
}
#[cfg(any(
    all(unix, feature = "provider-authority-prototype"),
    all(test, target_os = "linux")
))]
impl Protection {
    #[cfg(all(target_os = "linux", feature = "provider-authority-prototype"))]
    pub(super) fn startup() -> anyhow::Result<Self> {
        protect_process()
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
    #[cfg(all(
        feature = "provider-authority-prototype",
        any(test, feature = "daemon-extension-fixture")
    ))]
    pub(super) fn synthetic() -> Self {
        Self {
            pid: std::process::id(),
            synthetic: true,
        }
    }
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
#[cfg(any(
    all(unix, feature = "provider-authority-prototype"),
    all(test, target_os = "linux")
))]
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
#[path = "provider_protection_tests.rs"]
mod tests;
