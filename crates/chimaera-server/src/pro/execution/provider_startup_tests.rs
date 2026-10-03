use super::*;
use std::{io::Write, path::PathBuf, sync::Arc};

fn private_pipe() -> (OwnedFd, OwnedFd) {
    let (reader, writer) = std::io::pipe().unwrap();
    (reader.into(), writer.into())
}

fn launch() -> Binding {
    Binding {
        account_id: "a-fixture".into(),
        workspace_id: "w-fixture".into(),
        root_identity: chimaera_core::project_secret_idle::RootIdentity {
            device: 1,
            inode: 2,
        },
        registration_revision: 3,
        launch_generation: 4,
        os_boot_id: "00000000-0000-4000-8000-000000000001".into(),
    }
}
fn payload() -> wire::StartupPayload {
    wire::StartupPayload {
        version: 1,
        binding: wire::Binding {
            version: 1,
            account_id: "a-fixture".into(),
            workspace_id: "w-fixture".into(),
            project_revision: 3,
            launch_generation: 4,
            enrollment: chimaera_core::personal_providers::Registration {
                version: 1,
                account_id: "a-fixture".into(),
                holder_id: "worker-fixture".into(),
                process_boot: "00000000-0000-4000-8000-000000000002".into(),
                registration_generation: 5,
                worker_credential_digest: "a".repeat(64),
            },
        },
        capability: wire::Capability::new("A".repeat(43)).unwrap(),
    }
}
fn consume(reader: OwnedFd, binding: Binding, deadline: Instant) -> Result<Pending> {
    let control = wire::StartupDescriptor {
        version: 1,
        fd: reader.as_raw_fd(),
    };
    Pending::transferred(
        reader,
        control,
        binding,
        deadline,
        super::super::maintenance_startup::Protection::synthetic(),
    )
}
fn pipe(bytes: &[u8]) -> OwnedFd {
    let (reader, writer) = private_pipe();
    let mut writer = std::fs::File::from(writer);
    writer.write_all(bytes).unwrap();
    drop(writer);
    reader
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

#[test]
fn inherited_payload_requires_eof_exact_launch_and_closed_schema() {
    let encoded = payload().encode().unwrap();
    let pending = consume(pipe(&encoded), launch(), deadline()).unwrap();
    pending.ready.inspect_fixture(|value| {
        assert!(value.binding == payload().binding);
        assert_eq!(value.capability.expose(), payload().capability.expose());
    });
    for field in 0..4 {
        let mut changed = launch();
        match field {
            0 => changed.account_id = "a-other".into(),
            1 => changed.workspace_id = "w-other".into(),
            2 => changed.registration_revision += 1,
            _ => changed.launch_generation += 1,
        }
        assert!(consume(pipe(&encoded), changed, deadline()).is_err());
    }
    for bytes in [
        b"{}".to_vec(),
        [b"{\"version\":1,".as_slice(), &encoded[1..]].concat(),
        [b"{\"extra\":true,".as_slice(), &encoded[1..]].concat(),
        vec![b' '; wire::STARTUP_MAX + 1],
    ] {
        assert!(consume(pipe(&bytes), launch(), deadline()).is_err());
    }
}

#[test]
fn provider_read_uses_original_deadline_and_closes_rejected_channel() {
    let encoded = payload().encode().unwrap();
    let (reader, writer) = private_pipe();
    let mut writer = std::fs::File::from(writer);
    writer.write_all(&encoded).unwrap();
    let began = Instant::now();
    assert!(consume(reader, launch(), began + Duration::from_millis(30)).is_err());
    assert!(began.elapsed() < Duration::from_secs(1));
    // Positive peer evidence: a timed-out consumer retained no read endpoint.
    assert_eq!(
        nix::unistd::write(&writer, b"x"),
        Err(nix::errno::Errno::EPIPE)
    );
    let before = Instant::now();
    assert!(consume(pipe(&encoded), launch(), before - Duration::from_millis(1)).is_err());
    assert!(before.elapsed() < Duration::from_millis(100));
    let (reader, writer) = private_pipe();
    assert!(consume(writer, launch(), deadline()).is_err());
    let mut reader = std::fs::File::from(reader);
    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    assert!(consume(
        std::fs::File::open("/dev/null").unwrap().into(),
        launch(),
        deadline()
    )
    .is_err());
    assert!(consume(
        std::fs::File::open("/").unwrap().into(),
        launch(),
        deadline()
    )
    .is_err());
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(consume(socket.into(), launch(), deadline()).is_err());
}

#[test]
fn unverified_provider_startup_cannot_restore_or_launch_through_local_fallback() {
    struct Root(PathBuf);
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Root(std::env::temp_dir().join(format!(
        "chimaera-provider-startup-{}",
        chimaera_core::generate_token()
    )));
    std::fs::create_dir_all(&root.0).unwrap();
    let state = Arc::new(crate::AppState::new(
        "fixture".into(),
        "fixture".into(),
        4242,
        0,
        root.0.clone(),
        root.0.join("config"),
    ));
    state
        .stopping
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(crate::pro::may_execute(&state, "w-fixture"));
    *crate::lock(&state.pro.execution.provider_pending) = Some(Arc::new(
        consume(pipe(&payload().encode().unwrap()), launch(), deadline()).unwrap(),
    ));
    assert!(!crate::pro::may_execute(&state, "w-fixture"));
    assert!(!crate::pro::may_execute(&state, "w-other"));
    assert!(!crate::pro::may_restore(&state, "w-fixture"));
    assert!(super::super::mutation::begin_launch(&state, "w-fixture").is_err());
    assert!(state.sessions.list().is_empty());
    assert!(state.chat.list().is_empty());
}
