use super::*;
use serde_json::json;

const BOOT: &str = "01234567-89ab-4def-8012-3456789abcde";
const OPERATION: &str = "01234567-89ab-4def-8012-3456789abcd0";
const NONCE: &str = "01234567-89ab-4def-8012-3456789abcd1";
const CAPABILITY: &str = "synthetic-private-control-capability-not-runtime-00000";
// The standard library supplies close-on-exec pipes on both macOS and Linux;
// raw pipe() readers can otherwise survive in parallel CLI fixture children.
fn private_pipe() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
    let (reader, writer) = std::io::pipe().unwrap();
    (reader.into(), writer.into())
}
fn startup() -> Vec<u8> {
    serde_json::to_vec(&json!({"version":1,"account_id":"a-one","holder_id":"worker-one","process_boot":BOOT,"registration_generation":3,"worker_credential_digest":"0000000000000000000000000000000000000000000000000000000000000000","capability":CAPABILITY})).unwrap()
}
fn binding() -> ControlBinding {
    ControlBinding::parse(&startup()).unwrap()
}
fn command(action: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"version":1,"operation_id":OPERATION,"provider":"claude","expected_connection_generation":2,"command":action})).unwrap()
}
fn consume(action: serde_json::Value) -> Result<ControlCommand, Error> {
    let b = binding();
    b.consume(
        CAPABILITY,
        &b.acknowledgment(),
        "device-one",
        command(action),
    )
}

#[tokio::test]
async fn private_pipe_requires_exact_binding_and_eof() {
    let (reader, writer) = private_pipe();
    let bytes = startup();
    assert_eq!(rustix::io::write(&writer, &bytes).unwrap(), bytes.len());
    drop(writer);
    let b = read_control_startup(reader).await.unwrap();
    assert_eq!(b.acknowledgment().account_id, "a-one");
    assert_eq!(b.acknowledgment().registration_generation, 3);
    assert!(!serde_json::to_string(&b.acknowledgment())
        .unwrap()
        .contains(CAPABILITY));
}

#[tokio::test]
async fn startup_refuses_regular_files_and_oversized_pipes() {
    let input: OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
    assert!(matches!(
        read_control_startup(input).await,
        Err(Error::InvalidStartup)
    ));
    let (reader, writer) = private_pipe();
    assert_eq!(
        rustix::io::write(&writer, &vec![b' '; STARTUP_BYTES + 1]).unwrap(),
        STARTUP_BYTES + 1
    );
    drop(writer);
    assert!(matches!(
        read_control_startup(reader).await,
        Err(Error::InvalidStartup)
    ));
}

#[tokio::test]
async fn startup_deadline_closes_a_pipe_whose_writer_never_finishes() {
    let (reader, writer) = private_pipe();
    let bytes = startup();
    assert_eq!(rustix::io::write(&writer, &bytes).unwrap(), bytes.len());
    let before = std::time::Instant::now();
    assert!(matches!(
        read_control_startup(reader).await,
        Err(Error::InvalidStartup)
    ));
    assert!(before.elapsed() < Duration::from_secs(5));
    assert_eq!(
        rustix::io::write(&writer, b"x"),
        Err(rustix::io::Errno::PIPE)
    );
}

#[test]
fn runtime_bearer_and_every_changed_registration_refuse() {
    let b = binding();
    let registration = b.acknowledgment();
    assert!(matches!(
        b.consume(
            "runtime-project-capability",
            &registration,
            "device-one",
            command(json!({"type":"connect"}))
        ),
        Err(Error::Unauthorized)
    ));
    for changed in [
        Registration {
            account_id: "a-two".into(),
            ..registration.clone()
        },
        Registration {
            holder_id: "worker-two".into(),
            ..registration.clone()
        },
        Registration {
            process_boot: OPERATION.into(),
            ..registration.clone()
        },
        Registration {
            registration_generation: 4,
            ..registration.clone()
        },
        Registration {
            version: 2,
            ..registration.clone()
        },
        Registration {
            worker_credential_digest: "f".repeat(64),
            ..registration.clone()
        },
    ] {
        assert!(matches!(
            b.consume(
                CAPABILITY,
                &changed,
                "device-one",
                command(json!({"type":"connect"}))
            ),
            Err(Error::Changed)
        ));
    }
}

#[test]
fn command_allowlist_rejects_unknowns_without_retaining_parser_secrets() {
    for action in [
        json!({"type":"shell","command":"synthetic-secret"}),
        json!({"type":"connect","cwd":"/project"}),
        json!({"type":"connect","helper":"/project/attack"}),
        json!({"type":"disconnect","acknowledge_cloud_work":false}),
    ] {
        assert!(matches!(consume(action), Err(Error::InvalidCommand)));
    }
    let b = binding();
    let malformed = b"{\"provider\":\"synthetic-secret\"}".to_vec();
    let error = b
        .consume(CAPABILITY, &b.acknowledgment(), "device-one", malformed)
        .err()
        .unwrap();
    assert!(!format!("{error:?} {error}").contains("synthetic-secret"));
    assert!(matches!(
        b.consume(
            CAPABILITY,
            &b.acknowledgment(),
            "device-one",
            vec![b'x'; COMMAND_BYTES + 1]
        ),
        Err(Error::InvalidCommand)
    ));
}

#[test]
fn submission_digest_excludes_every_secret_byte() {
    let a = consume(json!({"type":"submit","attempt_id":OPERATION,"submission_nonce":NONCE,"code":"first#value"})).unwrap();
    let b = consume(json!({"type":"submit","attempt_id":OPERATION,"submission_nonce":NONCE,"code":"different#secret"})).unwrap();
    assert_eq!(a.nonsensitive_digest(), b.nonsensitive_digest());
    let c = consume(json!({"type":"submit","attempt_id":OPERATION,"submission_nonce":BOOT,"code":"first#value"})).unwrap();
    assert_ne!(a.nonsensitive_digest(), c.nonsensitive_digest());
    assert!(matches!(
        consume(
            json!({"type":"submit","attempt_id":OPERATION,"submission_nonce":NONCE,"code":"missing-state"})
        ),
        Err(Error::InvalidCommand)
    ));
    assert!(matches!(
        consume(
            json!({"type":"submit","attempt_id":OPERATION,"submission_nonce":NONCE,"code":"secret\n#state"})
        ),
        Err(Error::InvalidCommand)
    ));
}

#[test]
fn startup_and_command_versions_never_fall_back() {
    for field in ["version", "registration_generation"] {
        let mut value: serde_json::Value = serde_json::from_slice(&startup()).unwrap();
        value[field] = json!(0);
        assert!(matches!(
            ControlBinding::parse(&serde_json::to_vec(&value).unwrap()),
            Err(Error::InvalidStartup)
        ));
    }
    let b = binding();
    let mut value: serde_json::Value =
        serde_json::from_slice(&command(json!({"type":"connect"}))).unwrap();
    value["version"] = json!(2);
    assert!(matches!(
        b.consume(
            CAPABILITY,
            &b.acknowledgment(),
            "device-one",
            serde_json::to_vec(&value).unwrap()
        ),
        Err(Error::InvalidCommand)
    ));
}
