use super::*;
use chimaera_link::{SshAuthDestination, SshAuthHostKey};
use signature::Signer;
use ssh_key::private::Ed25519Keypair;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn key(seed: u8) -> ssh_key::PrivateKey {
    ssh_key::PrivateKey::new(
        ssh_key::private::KeypairData::Ed25519(Ed25519Keypair::from_seed(&[seed; 32])),
        "synthetic fixture",
    )
    .unwrap()
}
fn public(key: &ssh_key::PrivateKey) -> Vec<u8> {
    PublicKey::from(KeyData::from(key)).to_bytes().unwrap()
}
fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}
fn sig(key: &ssh_key::PrivateKey, data: &[u8]) -> Vec<u8> {
    Vec::try_from(key.try_sign(data).unwrap()).unwrap()
}
fn bind_packet(host: &[u8], signer: &ssh_key::PrivateKey, session: &[u8]) -> Vec<u8> {
    let mut p = vec![27];
    for value in [
        b"session-bind@openssh.com".as_slice(),
        host,
        session,
        &sig(signer, session),
    ] {
        string(&mut p, value);
    }
    p.push(0);
    p
}
fn auth_data(user: &str, key: &[u8], host: &[u8], session: &[u8], method: &[u8]) -> Vec<u8> {
    let mut p = vec![];
    string(&mut p, session);
    p.push(50);
    for value in [user.as_bytes(), b"ssh-connection", method] {
        string(&mut p, value);
    }
    p.push(1);
    string(&mut p, b"ssh-ed25519");
    string(&mut p, key);
    string(&mut p, host);
    p
}
fn sign_packet(key: &[u8], data: &[u8], flags: u32) -> Vec<u8> {
    let mut p = vec![13];
    string(&mut p, key);
    string(&mut p, data);
    p.extend_from_slice(&flags.to_be_bytes());
    p
}
fn selected() -> SshAuthGrantRequest {
    SshAuthGrantRequest {
        version: 1,
        keeper_boot: "synthetic-boot".into(),
        destination: SshAuthDestination {
            hostname: "hpc.example.invalid".into(),
            user: "alice".into(),
            port: 22,
        },
        host_keys: vec![SshAuthHostKey {
            key: STANDARD.encode(public(&key(1))),
            is_ca: false,
        }],
        user_keys: vec![STANDARD.encode(public(&key(2)))],
    }
}
fn bind_request(id: &str, n: u64, packet: Vec<u8>) -> SshAuthRequest {
    SshAuthRequest::SessionBind {
        connection_id: id.into(),
        request_id: n,
        packet: STANDARD.encode(packet),
    }
}
fn sign_request(id: &str, n: u64, packet: Vec<u8>) -> SshAuthRequest {
    SshAuthRequest::Sign {
        connection_id: id.into(),
        request_id: n,
        packet: STANDARD.encode(packet),
    }
}
fn valid_bind(session: &[u8]) -> Vec<u8> {
    bind_packet(&public(&key(1)), &key(1), session)
}
fn valid_sign(session: &[u8]) -> Vec<u8> {
    let user = public(&key(2));
    sign_packet(
        &user,
        &auth_data(
            "alice",
            &user,
            &public(&key(1)),
            session,
            b"publickey-hostbound-v00@openssh.com",
        ),
        0,
    )
}
struct Mock {
    calls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
    refuse_bind: bool,
    stall_sign: bool,
    wrong_signature: bool,
}
struct MockConnection {
    calls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
    refuse_bind: bool,
    stall_sign: bool,
    wrong_signature: bool,
}
impl Drop for MockConnection {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl Mock {
    fn new() -> Self {
        Self {
            calls: Arc::default(),
            drops: Arc::default(),
            refuse_bind: false,
            stall_sign: false,
            wrong_signature: false,
        }
    }
}
impl LocalAgent for Mock {
    type Connection = MockConnection;
    async fn connect(&self) -> Result<MockConnection, Failure> {
        Ok(MockConnection {
            calls: self.calls.clone(),
            drops: self.drops.clone(),
            refuse_bind: self.refuse_bind,
            stall_sign: self.stall_sign,
            wrong_signature: self.wrong_signature,
        })
    }
}
impl AgentConnection for MockConnection {
    async fn exchange(&mut self, bytes: &[u8]) -> Result<Vec<u8>, Failure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if bytes[0] == 27 {
            return Ok(vec![if self.refuse_bind { 5 } else { 6 }]);
        }
        if self.stall_sign {
            std::future::pending::<()>().await;
        }
        let mut r = Reader::new(bytes)?;
        r.byte_is(13)?;
        r.string()?;
        let data = r.string()?;
        let mut reply = vec![14];
        string(
            &mut reply,
            &sig(&key(if self.wrong_signature { 3 } else { 2 }), data),
        );
        Ok(reply)
    }
}
fn verifier(mock: Mock) -> GrantVerifier<Mock> {
    GrantVerifier::new(&selected(), Instant::now() + Duration::from_secs(120), mock)
        .ok()
        .unwrap()
}

#[tokio::test]
async fn legitimate_hostbound_auth_signs_only_after_verified_local_agent_binding() {
    let mock = Mock::new();
    let calls = mock.calls.clone();
    let mut v = verifier(mock);
    assert!(matches!(
        v.handle(bind_request("a", 1, valid_bind(b"session-a")))
            .await,
        Ok(Some(SshAuthReply::Bound { .. }))
    ));
    assert!(matches!(
        v.handle(sign_request("a", 2, valid_sign(b"session-a")))
            .await,
        Ok(Some(SshAuthReply::Signature { .. }))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn malicious_keeper_cannot_sign_another_user_key_host_session_or_unbound_bytes() {
    let user = public(&key(2));
    let host = public(&key(1));
    let session = b"session-a";
    let good = auth_data(
        "alice",
        &user,
        &host,
        session,
        b"publickey-hostbound-v00@openssh.com",
    );
    let mut trailing = good.clone();
    trailing.push(0);
    let attempts = [
        sign_packet(
            &user,
            &auth_data(
                "bob",
                &user,
                &host,
                session,
                b"publickey-hostbound-v00@openssh.com",
            ),
            0,
        ),
        sign_packet(
            &user,
            &auth_data(
                "alice",
                &user,
                &public(&key(3)),
                session,
                b"publickey-hostbound-v00@openssh.com",
            ),
            0,
        ),
        sign_packet(
            &user,
            &auth_data(
                "alice",
                &user,
                &host,
                b"different-session",
                b"publickey-hostbound-v00@openssh.com",
            ),
            0,
        ),
        sign_packet(
            &user,
            &auth_data("alice", &user, &host, session, b"publickey"),
            0,
        ),
        sign_packet(&public(&key(3)), &good, 0),
        sign_packet(&user, &good, 2),
        sign_packet(&user, &trailing, 0),
        sign_packet(&user, b"arbitrary-signature-request", 0),
    ];
    for attempt in attempts {
        let mock = Mock::new();
        let calls = mock.calls.clone();
        let mut v = verifier(mock);
        assert!(matches!(
            v.handle(bind_request("a", 1, valid_bind(session))).await,
            Ok(Some(SshAuthReply::Bound { .. }))
        ));
        assert!(matches!(
            v.handle(sign_request("a", 2, attempt)).await,
            Ok(Some(SshAuthReply::Failure { .. }))
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "refused before consulting agent"
        );
    }
}

#[tokio::test]
async fn untrusted_forged_forwarded_or_trailing_binding_never_reaches_local_agent() {
    let mut forwarded = valid_bind(b"s");
    *forwarded.last_mut().unwrap() = 1;
    let mut trailing = valid_bind(b"s");
    trailing.push(0);
    for packet in [
        forwarded,
        trailing,
        bind_packet(&public(&key(3)), &key(3), b"s"),
        bind_packet(&public(&key(1)), &key(3), b"s"),
    ] {
        let mock = Mock::new();
        let calls = mock.calls.clone();
        let mut v = verifier(mock);
        assert!(matches!(
            v.handle(bind_request("a", 1, packet)).await,
            Ok(Some(SshAuthReply::Failure { .. }))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn closed_connection_session_and_request_ids_never_replay() {
    let mut v = verifier(Mock::new());
    assert!(matches!(
        v.handle(bind_request("a", 1, valid_bind(b"s"))).await,
        Ok(Some(SshAuthReply::Bound { .. }))
    ));
    assert!(v
        .handle(SshAuthRequest::ConnectionClosed {
            connection_id: "a".into()
        })
        .await
        .is_ok());
    for (id, session, n) in [("a", b"new".as_slice(), 2), ("b", b"s".as_slice(), 3)] {
        assert!(matches!(
            v.handle(bind_request(id, n, valid_bind(session))).await,
            Ok(Some(SshAuthReply::Failure { .. }))
        ));
    }
    assert!(v
        .handle(bind_request("c", 3, valid_bind(b"fresh")))
        .await
        .is_err());
    assert!(matches!(
        v.handle(sign_request("a", 4, valid_sign(b"s"))).await,
        Ok(Some(SshAuthReply::Failure { .. }))
    ));
}

#[tokio::test]
async fn refused_agent_binding_or_wrong_local_signature_never_downgrades() {
    let mut mock = Mock::new();
    mock.refuse_bind = true;
    let calls = mock.calls.clone();
    let mut v = verifier(mock);
    assert!(matches!(
        v.handle(bind_request("a", 1, valid_bind(b"s"))).await,
        Ok(Some(SshAuthReply::Failure {
            error: Failure::AgentRefused,
            ..
        }))
    ));
    assert!(matches!(
        v.handle(sign_request("a", 2, valid_sign(b"s"))).await,
        Ok(Some(SshAuthReply::Failure { .. }))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut mock = Mock::new();
    mock.wrong_signature = true;
    let mut v = verifier(mock);
    assert!(matches!(
        v.handle(bind_request("a", 1, valid_bind(b"s"))).await,
        Ok(Some(SshAuthReply::Bound { .. }))
    ));
    assert!(matches!(
        v.handle(sign_request("a", 2, valid_sign(b"s"))).await,
        Ok(Some(SshAuthReply::Failure {
            error: Failure::AgentRefused,
            ..
        }))
    ));
}

#[tokio::test]
async fn expiry_and_caller_cancellation_close_pending_local_agent_connections() {
    for expired in [false, true] {
        let mut mock = Mock::new();
        mock.stall_sign = true;
        let drops = mock.drops.clone();
        let mut v = verifier(mock);
        assert!(matches!(
            v.handle(bind_request("a", 1, valid_bind(b"s"))).await,
            Ok(Some(SshAuthReply::Bound { .. }))
        ));
        if expired {
            v.deadline = Instant::now() + Duration::from_millis(25);
        }
        let result = tokio::time::timeout(
            Duration::from_millis(75),
            v.handle(sign_request("a", 2, valid_sign(b"s"))),
        )
        .await;
        if expired {
            assert!(matches!(result, Ok(Err(Failure::Expired))));
        } else {
            assert!(result.is_err());
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(v.connections.is_empty());
    }
}

fn certificate(principal: &str, kind: CertType, after: u64, before: u64) -> Vec<u8> {
    let mut b =
        ssh_key::certificate::Builder::new(vec![0; 16], KeyData::from(&key(1)), after, before)
            .unwrap();
    b.cert_type(kind)
        .unwrap()
        .valid_principal(principal)
        .unwrap();
    b.sign(&key(4)).unwrap().to_bytes().unwrap()
}
#[test]
fn host_ca_requires_exact_principal_type_validity_and_trusted_signer() {
    let mut s = selected();
    s.host_keys = vec![SshAuthHostKey {
        key: STANDARD.encode(public(&key(4))),
        is_ca: true,
    }];
    let policy = Policy::new(&s).ok().unwrap();
    let time = now().ok().unwrap();
    for (principal, kind, after, before, allowed) in [
        (
            "hpc.example.invalid",
            CertType::Host,
            time - 60,
            time + 60,
            true,
        ),
        (
            "different.example.invalid",
            CertType::Host,
            time - 60,
            time + 60,
            false,
        ),
        (
            "hpc.example.invalid",
            CertType::User,
            time - 60,
            time + 60,
            false,
        ),
        (
            "hpc.example.invalid",
            CertType::Host,
            time - 120,
            time - 60,
            false,
        ),
        (
            "hpc.example.invalid",
            CertType::Host,
            time + 60,
            time + 120,
            false,
        ),
    ] {
        let blob = certificate(principal, kind, after, before);
        assert_eq!(
            policy.bind(&bind_packet(&blob, &key(1), b"s")).is_ok(),
            allowed
        );
    }
    let blob = certificate("hpc.example.invalid", CertType::Host, time - 60, time + 60);
    let untrusted = Policy::new(&selected()).ok().unwrap();
    assert!(untrusted.bind(&bind_packet(&blob, &key(1), b"s")).is_err());
}

#[test]
fn wire_parsers_refuse_truncation_trailing_and_oversized_lengths() {
    let policy = Policy::new(&selected()).ok().unwrap();
    let binding = valid_bind(b"s");
    for n in 0..binding.len() {
        assert!(policy.bind(&binding[..n]).is_err());
    }
    let signature = sig(&key(1), b"s");
    let mut trailing = signature.clone();
    trailing.push(0);
    assert!(packet::signature(&signature).is_ok());
    assert!(packet::signature(&trailing).is_err());
    assert!(Reader::new(&vec![0; SSH_AUTH_PACKET_MAX + 1]).is_err());
    assert!(Reader::new(&[255; 4]).ok().unwrap().string().is_err());
}

#[tokio::test]
async fn grant_connection_budget_cannot_be_reset_by_closing_connections() {
    let mut v = verifier(Mock::new());
    for n in 0..SSH_AUTH_CONNECTIONS_MAX {
        let id = format!("connection-{n}");
        assert!(matches!(
            v.handle(bind_request(&id, n as u64 + 1, valid_bind(id.as_bytes())))
                .await,
            Ok(Some(SshAuthReply::Bound { .. }))
        ));
        assert!(v
            .handle(SshAuthRequest::ConnectionClosed { connection_id: id })
            .await
            .is_ok());
    }
    assert!(matches!(
        v.handle(bind_request("overflow", 100, valid_bind(b"overflow")))
            .await,
        Ok(Some(SshAuthReply::Failure { .. }))
    ));
    assert_eq!(v.seen_connections.len(), SSH_AUTH_CONNECTIONS_MAX);
}
