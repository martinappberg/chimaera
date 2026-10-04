use super::*;

#[test]
fn fixed_launcher_status_requires_complete_unprivileged_evidence() {
    const GOOD: &str = "Uid:\t61000 61000 61000 61000\nGid:\t61000 61000 61000 61000\nGroups:\t\nCapInh:\t0000000000000000\nCapPrm:\t0000000000000000\nCapEff:\t0000000000000000\nCapAmb:\t0000000000000000\nNoNewPrivs:\t1\n";
    assert!(unprivileged_status(GOOD.as_bytes()));
    for bad in [
        GOOD.replace("61000", "0"),
        GOOD.replace("Groups:\t", "Groups:\t61000"),
        GOOD.replace("CapEff:\t0000000000000000", "CapEff:\t0000000000080000"),
        GOOD.replace("CapPrm:\t0000000000000000", "CapPrm:\t0000000000000001"),
        GOOD.replace("CapAmb:\t0000000000000000", "CapAmb:\t0000000000000001"),
        GOOD.replace("NoNewPrivs:\t1", "NoNewPrivs:\t0"),
        GOOD.replace(
            "Uid:\t61000 61000 61000 61000",
            "Uid:\t61000 61001 61000 61000",
        ),
        GOOD.replace("NoNewPrivs:\t1\n", ""),
        format!("{GOOD}CapEff:\t0000000000000000\n"),
    ] {
        assert!(!unprivileged_status(bad.as_bytes()));
    }
    assert!(!unprivileged_status(&vec![b'x'; 16 * 1024 + 1]));
}

/// Explicit root Linux fixture: only a disposable helper changes UID/protection;
/// neither this parent harness nor unrelated local work is hardened or stopped.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires disposable Linux root fixture; changes only helper credentials"]
fn linux_process_protection_and_child_exec_descriptor_exclusion() {
    use std::os::{
        fd::AsRawFd,
        unix::{net::UnixStream, process::CommandExt},
    };
    assert_eq!(
        unsafe { nix::libc::geteuid() },
        0,
        "run only in disposable root Linux fixture"
    );
    let (descriptor, _peer) = UnixStream::pair().unwrap();
    let raw = descriptor.as_raw_fd();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "pro::execution::provider_protection::tests::linux_protection_child",
            "--ignored",
            "--nocapture",
        ])
        .env("CHIMAERA_IDLE_PROTECTION_TEST_FD", raw.to_string());
    unsafe {
        command.pre_exec(move || {
            for result in [
                nix::libc::setgroups(0, std::ptr::null()),
                nix::libc::setresgid(61000, 61000, 61000),
                nix::libc::setresuid(61000, 61000, 61000),
                nix::libc::prctl(nix::libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0),
                nix::libc::fcntl(raw, nix::libc::F_SETFD, 0),
            ] {
                if result < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("protection fixture deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // The helper's protections are process-local; parent privilege is unchanged.
    assert_eq!(unsafe { nix::libc::geteuid() }, 0);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "only launched by linux_process_protection_and_child_exec_descriptor_exclusion"]
fn linux_protection_child() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let raw: i32 = std::env::var("CHIMAERA_IDLE_PROTECTION_TEST_FD")
        .unwrap()
        .parse()
        .unwrap();
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
    let flags = unsafe { nix::libc::fcntl(raw, nix::libc::F_GETFD) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { nix::libc::fcntl(raw, nix::libc::F_SETFD, flags | nix::libc::FD_CLOEXEC) },
        0
    );
    let protected = protect_process().unwrap();
    assert_eq!(descriptor.as_raw_fd(), raw);
    let pid = std::process::id();
    // Same-UID child exec must neither inherit the exact socket nor inspect the
    // daemon's descriptor directory or memory through proc/ptrace permissions.
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "pro::execution::provider_protection::tests::linux_same_uid_access_child",
            "--ignored",
            "--nocapture",
        ])
        .env("CHIMAERA_IDLE_PROTECTION_TEST_PID", pid.to_string());
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("access fixture deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        unsafe { nix::libc::prctl(nix::libc::PR_GET_DUMPABLE, 0, 0, 0, 0) },
        0
    );
    assert_eq!(
        unsafe { nix::libc::prctl(nix::libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) },
        1
    );
    protected.current().unwrap();
    assert_eq!(
        unsafe { nix::libc::prctl(nix::libc::PR_SET_DUMPABLE, 1, 0, 0, 0) },
        0
    );
    assert!(protected.current().is_err());
    // Verification must refuse without silently resetting dumpability.
    assert_eq!(
        unsafe { nix::libc::prctl(nix::libc::PR_GET_DUMPABLE, 0, 0, 0, 0) },
        1
    );
    assert_eq!(
        unsafe { nix::libc::prctl(nix::libc::PR_SET_DUMPABLE, 0, 0, 0, 0) },
        0
    );
    protected.current().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "only launched by linux_protection_child"]
fn linux_same_uid_access_child() {
    let pid: u32 = std::env::var("CHIMAERA_IDLE_PROTECTION_TEST_PID")
        .unwrap()
        .parse()
        .unwrap();
    let raw: i32 = std::env::var("CHIMAERA_IDLE_PROTECTION_TEST_FD")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { nix::libc::geteuid() }, 61000);
    assert_eq!(
        std::fs::read_link(format!("/proc/self/fd/{raw}"))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(
        std::fs::read_dir(format!("/proc/{pid}/fd"))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        std::fs::File::open(format!("/proc/{pid}/mem"))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let attached = unsafe {
        nix::libc::ptrace(
            nix::libc::PTRACE_ATTACH,
            pid as nix::libc::pid_t,
            std::ptr::null_mut::<nix::libc::c_void>(),
            std::ptr::null_mut::<nix::libc::c_void>(),
        )
    };
    let error = std::io::Error::last_os_error();
    if attached == 0 {
        unsafe {
            nix::libc::ptrace(
                nix::libc::PTRACE_DETACH,
                pid as nix::libc::pid_t,
                std::ptr::null_mut::<nix::libc::c_void>(),
                std::ptr::null_mut::<nix::libc::c_void>(),
            );
        }
    }
    assert!(attached < 0 && error.raw_os_error() == Some(nix::libc::EPERM));
    assert_eq!(
        unsafe { nix::libc::prctl(nix::libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) },
        1
    );
}
