//! Disposable OpenSSH agent only; never consult the user's SSH_AUTH_SOCK.
use super::*;
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
};
use tokio::net::UnixStream;

#[test]
#[ignore = "verifies the bounded synthetic native empty-agent probe receipt supplied by the explicit fixture harness"]
fn native_empty_agent_kex_receipt_requires_positive_host_signature_and_publickey_offer() {
    let receipt = std::env::var_os("CHIMAERA_SYNTHETIC_SSH_PROBE_RECEIPT")
        .expect("explicit synthetic probe receipt path required");
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(receipt)
        .unwrap()
        .take(32 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 32 * 1024);
    let receipt: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let host = ssh_key::PublicKey::from_openssh(receipt["public"].as_str().unwrap()).unwrap();
    let selected = SshAuthGrantRequest {
        version: 1,
        keeper_boot: "synthetic-empty-agent-probe".into(),
        destination: chimaera_link::SshAuthDestination {
            hostname: "probe.invalid".into(),
            user: "fixture-no-such-ssh-user".into(),
            port: 22,
        },
        host_keys: vec![chimaera_link::SshAuthHostKey {
            key: STANDARD.encode(host.to_bytes().unwrap()),
            is_ca: false,
        }],
        // The parser requires a selected user key; this synthetic key is never
        // supplied to the probe's empty agent or used for authentication.
        user_keys: vec![STANDARD.encode(key(99).public_key().to_bytes().unwrap())],
    };
    let policy =
        Policy::new(&selected).unwrap_or_else(|_| panic!("synthetic probe policy refused"));
    let results = receipt["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for result in results {
        assert_eq!(result["exit"], 255);
        assert_eq!(result["stdout_bytes"], 0);
        assert_eq!(result["sign_requests"], 0);
        let packets = result["binds"].as_array().unwrap();
        if result["offered_pubkey"] == true {
            assert_eq!(packets.len(), 1);
            let packet = STANDARD.decode(packets[0].as_str().unwrap()).unwrap();
            let (verified_host, session) = policy
                .bind(&packet)
                .unwrap_or_else(|_| panic!("synthetic probe host signature refused"));
            assert_eq!(verified_host, host.to_bytes().unwrap());
            assert!(!session.is_empty());
            let mut tampered = packet;
            let index = tampered.len() - 2;
            tampered[index] ^= 1;
            assert!(policy.bind(&tampered).is_err());
        } else {
            assert!(
                packets.is_empty(),
                "a password-only server provides no proof"
            );
        }
    }
}

struct Fixture {
    directory: PathBuf,
    agent: Child,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.agent.kill();
        let _ = self.agent.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn write(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}
fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}
fn key(seed: u8) -> ssh_key::PrivateKey {
    ssh_key::PrivateKey::new(
        ssh_key::private::KeypairData::Ed25519(ssh_key::private::Ed25519Keypair::from_seed(
            &[seed; 32],
        )),
        "disposable fixture",
    )
    .unwrap()
}
fn bind(host: &ssh_key::PrivateKey, session: &[u8]) -> Vec<u8> {
    use signature::Signer;
    let mut packet = vec![27];
    for value in [
        b"session-bind@openssh.com".as_slice(),
        &host.public_key().to_bytes().unwrap(),
        session,
        &Vec::<u8>::try_from(host.try_sign(session).unwrap()).unwrap(),
    ] {
        put_string(&mut packet, value);
    }
    packet.push(0);
    packet
}

#[tokio::test]
#[ignore = "starts a disposable installed OpenSSH agent with synthetic constrained keys"]
async fn real_openssh_agent_preserves_destination_constraints_and_lock_refusal() {
    let dir = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "cx-signing-{}-{}",
        std::process::id(),
        now().ok().unwrap()
    ));
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.join("agent");
    let user = key(91);
    let host = key(92);
    let agent = Command::new("/usr/bin/ssh-agent")
        .args(["-D", "-a"])
        .arg(&socket)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let fixture = Fixture {
        directory: dir,
        agent,
    };
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(socket.exists(), "disposable agent did not start");
    let private = fixture.directory.join("synthetic-key");
    write(
        &private,
        user.to_openssh(ssh_key::LineEnding::LF).unwrap().as_bytes(),
    );
    let hosts = fixture.directory.join("known_hosts");
    write(
        &hosts,
        format!(
            "hpc.example.invalid {}\n",
            host.public_key().to_openssh().unwrap()
        )
        .as_bytes(),
    );
    let status = Command::new("/usr/bin/ssh-add")
        .args(["-q", "-H"])
        .arg(&hosts)
        .args(["-h", "alice@hpc.example.invalid"])
        .arg(&private)
        .env_clear()
        .env("SSH_AUTH_SOCK", &socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "installed agent must support destination constraints for this gate"
    );
    write(
        &fixture.directory.join("synthetic-key.pub"),
        user.public_key().to_openssh().unwrap().as_bytes(),
    );
    let config_file = fixture.directory.join("config");
    write(&config_file,format!("Host fixture\n HostName hpc.example.invalid\n User alice\n Port 22\n IdentityAgent {}\n IdentitiesOnly yes\n IdentityFile {}\n UserKnownHostsFile {}\n GlobalKnownHostsFile none\n",socket.display(),private.display(),hosts.display()).as_bytes());
    let effective = Command::new("/usr/bin/ssh")
        .args(["-F"])
        .arg(&config_file)
        .args(["-G", "fixture"])
        .output()
        .unwrap();
    assert!(effective.status.success());
    let selection = selection::from_native_config(
        std::str::from_utf8(&effective.stdout).unwrap(),
        &fixture.directory,
        None,
        "synthetic-boot".into(),
    )
    .await
    .unwrap();
    let selected = &selection.request;
    assert_eq!(
        selected.user_keys,
        vec![STANDARD.encode(user.public_key().to_bytes().unwrap())]
    );
    assert_eq!(
        selected.host_keys[0].key,
        STANDARD.encode(host.public_key().to_bytes().unwrap())
    );
    let (_, mut verifier) = selection
        .verifier(Instant::now() + Duration::from_secs(120))
        .ok()
        .unwrap();
    let session = b"disposable-openssh-session";
    let reply = verifier
        .handle(SshAuthRequest::SessionBind {
            connection_id: "a".into(),
            request_id: 1,
            packet: STANDARD.encode(bind(&host, session)),
        })
        .await;
    assert!(matches!(reply, Ok(Some(SshAuthReply::Bound { .. }))));
    let user_blob = user.public_key().to_bytes().unwrap();
    let host_blob = host.public_key().to_bytes().unwrap();
    let mut data = vec![];
    put_string(&mut data, session);
    data.push(50);
    for value in [
        b"alice".as_slice(),
        b"ssh-connection",
        b"publickey-hostbound-v00@openssh.com",
    ] {
        put_string(&mut data, value);
    }
    data.push(1);
    for value in [b"ssh-ed25519".as_slice(), &user_blob, &host_blob] {
        put_string(&mut data, value);
    }
    let mut packet = vec![13];
    put_string(&mut packet, &user_blob);
    put_string(&mut packet, &data);
    packet.extend_from_slice(&0u32.to_be_bytes());
    assert!(matches!(
        verifier
            .handle(SshAuthRequest::Sign {
                connection_id: "a".into(),
                request_id: 2,
                packet: STANDARD.encode(&packet)
            })
            .await,
        Ok(Some(SshAuthReply::Signature { .. }))
    ));
    // Bypass the native policy only inside this fixture to prove the real
    // agent itself retains its constrained username on that same connection.
    let connection = verifier.connections.get_mut("a").unwrap();
    let mut changed = data.clone();
    let at = changed
        .windows(5)
        .position(|value| value == b"alice")
        .unwrap();
    changed[at..at + 5].copy_from_slice(b"other");
    let mut attack = vec![13];
    put_string(&mut attack, &user_blob);
    put_string(&mut attack, &changed);
    attack.extend_from_slice(&0u32.to_be_bytes());
    assert!(matches!(connection.agent.exchange(&attack).await, Ok(bytes) if bytes == [5]));
    let mut lock = vec![22];
    put_string(&mut lock, b"synthetic-test-lock");
    let mut control = UnixStream::connect(&socket).await.unwrap();
    assert!(matches!(control.exchange(&lock).await, Ok(bytes) if bytes == [6]));
    assert!(matches!(
        verifier
            .handle(SshAuthRequest::Sign {
                connection_id: "a".into(),
                request_id: 3,
                packet: STANDARD.encode(packet)
            })
            .await,
        Ok(Some(SshAuthReply::Failure { .. }))
    ));
    drop(verifier);
    drop(control);
    drop(fixture);
}
