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

#[tokio::test]
async fn closed_control_metadata_is_bounded_and_cannot_replace_launch_identity() {
    let f = super::super::maintenance::tests::Fixture::new().await;
    let good = serde_json::json!({"version":1,"fd":3,"channel_nonce":"A".repeat(43)});
    let control: Control = serde_json::from_value(good.clone()).unwrap();
    control.validate(&f.binding).unwrap();
    for key in ["version", "fd", "channel_nonce"] {
        let mut missing = good.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<Control>(missing).is_err());
    }
    for bad in [
        serde_json::json!({"version":2,"fd":3,"channel_nonce":"A".repeat(43)}),
        serde_json::json!({"version":1,"fd":2,"channel_nonce":"A".repeat(43)}),
        serde_json::json!({"version":1,"fd":3,"channel_nonce":"A".repeat(42)}),
    ] {
        assert!(serde_json::from_value::<Control>(bad)
            .unwrap()
            .validate(&f.binding)
            .is_err());
    }
    let mut extra = good;
    extra["workspace_id"] = serde_json::json!("another");
    assert!(serde_json::from_value::<Control>(extra).is_err());
    assert!(serde_json::from_str::<Control>(r#"{"version":1,"version":1,"fd":3,"channel_nonce":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn startup_socket_gate_refuses_other_fd_kinds_without_mutating_process() {
    use std::os::unix::net::{UnixDatagram, UnixStream};
    let (socket, _) = UnixStream::pair().unwrap();
    let descriptor: OwnedFd = socket.into();
    validate_socket(&descriptor).unwrap();
    assert_ne!(
        unsafe { nix::libc::fcntl(descriptor.as_raw_fd(), nix::libc::F_GETFD) }
            & nix::libc::FD_CLOEXEC,
        0
    );
    let (datagram, _) = UnixDatagram::pair().unwrap();
    assert!(validate_socket(&datagram.into()).is_err());
    let (reader, _) = nix::unistd::pipe().unwrap();
    assert!(validate_socket(&reader).is_err());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_server, _) = listener.accept().unwrap();
    assert!(validate_socket(&client.into()).is_err());
}

/// Explicit root Linux fixture: only a disposable helper changes UID/protection;
/// neither this parent harness nor unrelated local work is hardened or stopped.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires disposable Linux root fixture; changes only helper credentials"]
fn linux_process_protection_and_child_exec_descriptor_exclusion() {
    use std::os::unix::{net::UnixStream, process::CommandExt};
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
            "pro::execution::maintenance_startup::tests::linux_protection_child",
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
    use std::os::fd::FromRawFd;
    let raw: i32 = std::env::var("CHIMAERA_IDLE_PROTECTION_TEST_FD")
        .unwrap()
        .parse()
        .unwrap();
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
    let binding = Binding {
        account_id: "a-fixture".into(),
        workspace_id: "w-fixture".into(),
        root_identity: chimaera_core::project_secret_idle::RootIdentity {
            device: 1,
            inode: 1,
        },
        registration_revision: 1,
        launch_generation: 1,
        os_boot_id: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
            .into(),
    };
    let pending = Pending::transferred(
        descriptor,
        Control {
            version: 1,
            fd: raw,
            channel_nonce: "A".repeat(43),
        },
        binding,
    )
    .unwrap();
    assert_eq!(pending.descriptor.as_raw_fd(), raw);
    let pid = std::process::id();
    // Same-UID child exec must neither inherit the exact socket nor inspect the
    // daemon's descriptor directory or memory through proc/ptrace permissions.
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "pro::execution::maintenance_startup::tests::linux_same_uid_access_child",
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
