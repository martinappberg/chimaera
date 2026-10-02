//! Disposable OpenSSH agent only; never consult the user's SSH_AUTH_SOCK.
use super::*;
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
};
use tokio::net::UnixStream;

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
    let selected = SshAuthGrantRequest {
        version: 1,
        keeper_boot: "synthetic-boot".into(),
        destination: chimaera_link::SshAuthDestination {
            hostname: "hpc.example.invalid".into(),
            user: "alice".into(),
            port: 22,
        },
        host_keys: vec![chimaera_link::SshAuthHostKey {
            key: STANDARD.encode(host.public_key().to_bytes().unwrap()),
            is_ca: false,
        }],
        user_keys: vec![STANDARD.encode(user.public_key().to_bytes().unwrap())],
    };
    let mut verifier = GrantVerifier::new(
        &selected,
        Instant::now() + Duration::from_secs(120),
        unix::UnixAgent::new(socket.clone()).ok().unwrap(),
    )
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
