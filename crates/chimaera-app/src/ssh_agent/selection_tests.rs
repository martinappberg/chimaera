use super::*;
use ssh_key::{
    private::{Ed25519Keypair, KeypairData},
    PrivateKey,
};

fn key(seed: u8) -> ssh_key::PublicKey {
    PrivateKey::new(
        KeypairData::Ed25519(Ed25519Keypair::from_seed(&[seed; 32])),
        "",
    )
    .unwrap()
    .public_key()
    .clone()
}
fn config() -> String {
    "hostname hpc.example.invalid\nuser alice\nport 2222\npubkeyauthentication true\nidentitiesonly no\n".into()
}
fn line(host: &str, seed: u8) -> String {
    format!("{host} {}\n", key(seed).to_openssh().unwrap())
}
fn encoded(seed: u8) -> String {
    STANDARD.encode(key(seed).to_bytes().unwrap())
}
#[test]
fn resolved_route_policy_preserves_key_only_and_ordered_methods_and_six_crypto_lists() {
    use chimaera_link::{SshRouteMethod::*, SshRouteMode};
    let base = format!("{}hostkeyalgorithms ssh-ed25519,ecdsa-sha2-nistp256\npubkeyacceptedalgorithms ssh-ed25519\ncasignaturealgorithms ssh-ed25519\nkexalgorithms curve25519-sha256\nciphers chacha20-poly1305@openssh.com\nmacs hmac-sha2-256-etm@openssh.com\n", config());
    for (setting, expected) in [
        (
            "kbdinteractiveauthentication no\npasswordauthentication no",
            vec![Publickey],
        ),
        (
            "passwordauthentication no",
            vec![Publickey, KeyboardInteractive],
        ),
        (
            "preferredauthentications publickey,password,keyboard-interactive",
            vec![Publickey, Password, KeyboardInteractive],
        ),
    ] {
        let policy = resolved_policy(&format!("{base}{setting}\n"), SshRouteMode::Key).unwrap();
        assert!(policy.methods == expected);
        assert_eq!(
            policy.host_key_algorithms,
            ["ssh-ed25519", "ecdsa-sha2-nistp256"]
        );
        assert_eq!(policy.kex_algorithms, ["curve25519-sha256"]);
        assert_eq!(policy.ciphers, ["chacha20-poly1305@openssh.com"]);
        assert_eq!(policy.macs, ["hmac-sha2-256-etm@openssh.com"]);
    }
    for setting in [
        "preferredauthentications password,publickey",
        "kbdinteractiveauthentication invalid",
        "gssapiauthentication yes",
    ] {
        assert!(resolved_policy(&format!("{base}{setting}\n"), SshRouteMode::Key).is_err());
    }
    let modified = base.replace(
        "kexalgorithms curve25519-sha256",
        "kexalgorithms +curve25519-sha256",
    );
    assert!(resolved_policy(&modified, SshRouteMode::Key).is_err());
}
fn string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

#[test]
fn destination_lookup_respects_port_and_verbatim_host_key_alias() {
    let value = config();
    let config = Config::parse(&value).unwrap();
    let destination = config.destination().unwrap();
    assert_eq!(
        config.lookup(&destination).unwrap(),
        "[hpc.example.invalid]:2222"
    );
    let value = format!("{value}hostkeyalias saved-name\n");
    assert_eq!(
        Config::parse(&value).unwrap().lookup(&destination).unwrap(),
        "saved-name"
    );
    assert_eq!(destination.user, "alice");
}

#[test]
fn local_routing_trust_commands_revocation_files_and_duplicates_do_not_silently_disappear() {
    for option in [
        "proxyjump jump",
        "proxycommand arbitrary",
        "knownhostscommand command",
        "revokedhostkeys /revocations",
        "checkhostip yes",
        "verifyhostkeydns ask",
        "pubkeyauthentication false",
    ] {
        assert!(
            Config::parse(&format!("{}{option}\n", config())).is_err(),
            "{option}"
        );
    }
    let duplicate = format!("{}hostname other\n", config());
    assert!(Config::parse(&duplicate).unwrap().destination().is_err());
    for path in ["relative", "/tmp/%h", "/tmp/${OTHER}", "/tmp/a\nb"] {
        assert!(local_path(path, Path::new("/synthetic/home")).is_err());
    }
    assert_eq!(
        local_path("~/.ssh/known_hosts", Path::new("/synthetic/home")).unwrap(),
        PathBuf::from("/synthetic/home/.ssh/known_hosts")
    );
}

#[test]
fn complete_agent_packet_is_bounded_and_comments_never_select_authority() {
    let mut packet = vec![12];
    packet.extend_from_slice(&2u32.to_be_bytes());
    for comment in [b"first".as_slice(), b"different-comment"] {
        string(&mut packet, &key(1).to_bytes().unwrap());
        string(&mut packet, comment);
    }
    assert_eq!(identity_reply(&packet).unwrap(), vec![encoded(1)]);
    packet.push(0);
    assert!(identity_reply(&packet).is_err());
    assert!(identity_reply(&[5]).is_err());
    assert_eq!(
        identity_reply(&[12, 0, 0, 0, 0]),
        Err(SelectionFailure::NoKeys)
    );
    assert!(identity_reply(&[12, 0, 0, 1, 1]).is_err());
}

#[test]
fn host_key_markers_algorithms_and_revocation_remain_authoritative() {
    let raw = line("hpc.example.invalid", 1);
    let ca = format!("@cert-authority {raw}");
    let entries = matching_trust(&format!("# lookup comment\n{raw}{ca}")).unwrap();
    assert!(!entries[0].is_ca && entries[1].is_ca);
    assert_eq!(entries[0].key, encoded(1));
    assert!(matches!(
        matching_trust(&format!("{raw}@revoked {raw}")),
        Err(SelectionFailure::RevokedHost)
    ));
    assert!(matching_trust(&raw.replace("ssh-ed25519", "ssh-rsa")).is_err());
    assert!(matching_trust(&format!("@unknown {raw}")).is_err());
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        // macOS's per-user TMPDIR plus a descriptive prefix exceeds SUN_LEN.
        let path =
            std::env::temp_dir().join(format!("cs-{}", &chimaera_core::generate_token()[..16]));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn identities_only_reads_public_siblings_and_never_the_private_identity_file() {
    let fixture = Fixture::new();
    let private = fixture.0.join("identity");
    let mut fifo = Command::new("/usr/bin/mkfifo");
    fifo.arg(&private);
    bounded_output(fifo, 0, None).await.unwrap();
    let text = format!("{}identityfile {}\n", config(), private.display());
    let parsed = Config::parse(&text).unwrap();
    assert!(parsed
        .public_identities(&fixture.0)
        .await
        .unwrap()
        .is_empty());
    std::fs::write(fixture.0.join("identity.pub"), key(1).to_openssh().unwrap()).unwrap();
    assert_eq!(
        parsed.public_identities(&fixture.0).await.unwrap(),
        BTreeSet::from([encoded(1)])
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), public_file(&private, 1024))
            .await
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn public_file_refuses_symlinks_oversize_and_changed_key_types() {
    let fixture = Fixture::new();
    let file = fixture.0.join("key.pub");
    std::fs::write(&file, key(1).to_openssh().unwrap()).unwrap();
    let link = fixture.0.join("link.pub");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(public_identity(&link).await.is_err());
    assert!(public_file(&file, 2).await.is_err());
    std::fs::write(&file, "not a public key").unwrap();
    assert!(public_identity(&file).await.is_err());
}

#[tokio::test]
async fn real_openssh_lookup_matches_hashed_and_negated_entries_from_exact_snapshot() {
    let fixture = Fixture::new();
    let file = fixture.0.join("known_hosts");
    std::fs::write(&file, line("[hpc.example.invalid]:2222", 1)).unwrap();
    let mut hash = Command::new("/usr/bin/ssh-keygen");
    hash.args(["-q", "-H", "-f"]).arg(&file);
    bounded_output(hash, 0, None).await.unwrap();
    let mut snapshot = public_file(&file, 1024 * 1024).await.unwrap().unwrap();
    snapshot.extend_from_slice(line("*.invalid,!hpc.example.invalid", 2).as_bytes());
    snapshot
        .extend_from_slice(format!("@cert-authority {}", line("*.example.invalid", 3)).as_bytes());
    let mut command = Command::new("/usr/bin/ssh-keygen");
    command.args(["-F", "[hpc.example.invalid]:2222", "-f", "/dev/stdin"]);
    let output = bounded_output(command, 1, Some(snapshot)).await.unwrap();
    let entries = matching_trust(&output).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, encoded(1));
    let snapshot = format!(
        "{}@cert-authority {}",
        line("*.invalid,!hpc.example.invalid", 2),
        line("*.example.invalid", 3)
    )
    .into_bytes();
    let mut command = Command::new("/usr/bin/ssh-keygen");
    command.args(["-F", "hpc.example.invalid", "-f", "/dev/stdin"]);
    let output = bounded_output(command, 1, Some(snapshot)).await.unwrap();
    let entries = matching_trust(&output).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_ca);
    assert_eq!(entries[0].key, encoded(3));
}

#[tokio::test]
async fn effective_ed25519_policy_does_not_offer_loaded_or_known_rsa_keys() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = Fixture::new();
    let socket = fixture.0.join("agent");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut rsa = Vec::new();
    string(&mut rsa, b"ssh-rsa");
    string(&mut rsa, &[1, 0, 1]);
    let mut modulus = vec![0];
    modulus.extend_from_slice(&[255; 256]);
    string(&mut rsa, &modulus);
    assert!(Key::parse(rsa.clone()).is_ok());
    let mut reply = vec![12];
    reply.extend_from_slice(&2u32.to_be_bytes());
    for blob in [rsa.clone(), key(1).to_bytes().unwrap()] {
        string(&mut reply, &blob);
        string(&mut reply, b"not authority");
    }
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert_eq!(stream.read_u32().await.unwrap(), 1);
        assert_eq!(stream.read_u8().await.unwrap(), 11);
        stream.write_u32(reply.len() as u32).await.unwrap();
        stream.write_all(&reply).await.unwrap();
    });
    let hosts = fixture.0.join("known_hosts");
    std::fs::write(
        &hosts,
        format!(
            "hpc.example.invalid ssh-rsa {}\n{}",
            STANDARD.encode(rsa),
            line("hpc.example.invalid", 1)
        ),
    )
    .unwrap();
    let config=format!("hostname hpc.example.invalid\nuser alice\nport 22\npubkeyauthentication true\nidentitiesonly no\nhostkeyalgorithms ssh-ed25519\npubkeyacceptedalgorithms ssh-ed25519\ncasignaturealgorithms ssh-ed25519\nidentityagent {}\nuserknownhostsfile {}\nglobalknownhostsfile none\n",socket.display(),hosts.display());
    let selection = from_native_config(&config, &fixture.0, None, "boot".into())
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(selection.request.user_keys, vec![encoded(1)]);
    assert_eq!(selection.request.host_keys.len(), 1);
    assert_eq!(selection.request.host_keys[0].key, encoded(1));
}
