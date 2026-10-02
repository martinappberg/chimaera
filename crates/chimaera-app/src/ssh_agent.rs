//! Native-only SSH signing. A keeper packet is never authority: every signature
//! is checked against the native caller's original destination/trust selection.
//! The owner must drop this verifier on signout or control-channel loss.
use base64::{engine::general_purpose::STANDARD, Engine};
use chimaera_link::ssh_auth::{
    decode_packet, SshAuthFailure as Failure, SshAuthGrantRequest, SshAuthReply, SshAuthRequest,
    SSH_AUTH_CONNECTIONS_MAX, SSH_AUTH_KEY_MAX, SSH_AUTH_LIFETIME, SSH_AUTH_PACKET_MAX,
};
use signature::Verifier;
use ssh_key::{
    certificate::CertType, public::KeyData, Algorithm, Certificate, HashAlg, PublicKey, Signature,
};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

#[path = "ssh_agent/packet.rs"]
mod packet;
use packet::Reader;
#[cfg(unix)]
#[path = "ssh_agent/unix.rs"]
pub(crate) mod unix;

pub(crate) trait AgentConnection: Send {
    fn exchange(&mut self, packet: &[u8]) -> impl Future<Output = Result<Vec<u8>, Failure>> + Send;
}
pub(crate) trait LocalAgent: Send {
    type Connection: AgentConnection;
    fn connect(&self) -> impl Future<Output = Result<Self::Connection, Failure>> + Send;
}

struct Key {
    blob: Vec<u8>,
    data: KeyData,
    certificate: Option<Certificate>,
}
impl Key {
    fn parse(blob: Vec<u8>) -> Result<Self, Failure> {
        if blob.len() > SSH_AUTH_KEY_MAX {
            return Err(Failure::Unsupported);
        }
        let (data, certificate) = match PublicKey::from_bytes(&blob) {
            Ok(key) => (key.key_data().clone(), None),
            Err(_) => {
                let cert = Certificate::from_bytes(&blob).map_err(|_| Failure::Unsupported)?;
                supported(cert.signature_key())?;
                (cert.public_key().clone(), Some(cert))
            }
        };
        supported(&data)?;
        Ok(Self {
            blob,
            data,
            certificate,
        })
    }
    fn user_algorithm(&self, algorithm: &[u8], flags: u32) -> Result<Algorithm, Failure> {
        let raw = self.data.algorithm();
        let signature = match raw {
            Algorithm::Rsa { .. } => match flags {
                2 => Algorithm::Rsa {
                    hash: Some(HashAlg::Sha256),
                },
                4 => Algorithm::Rsa {
                    hash: Some(HashAlg::Sha512),
                },
                _ => return Err(Failure::InvalidRequest),
            },
            _ if flags == 0 => raw,
            _ => return Err(Failure::InvalidRequest),
        };
        let expected = if self.certificate.is_some() {
            match signature {
                Algorithm::Rsa {
                    hash: Some(HashAlg::Sha256),
                } => "rsa-sha2-256-cert-v01@openssh.com".into(),
                Algorithm::Rsa {
                    hash: Some(HashAlg::Sha512),
                } => "rsa-sha2-512-cert-v01@openssh.com".into(),
                _ => signature.to_certificate_type(),
            }
        } else {
            signature.as_str().to_owned()
        };
        if algorithm != expected.as_bytes() {
            return Err(Failure::InvalidRequest);
        }
        Ok(signature)
    }
}
fn supported(key: &KeyData) -> Result<(), Failure> {
    match key {
        KeyData::Ed25519(_)
        | KeyData::Ecdsa(_)
        | KeyData::SkEd25519(_)
        | KeyData::SkEcdsaSha2NistP256(_) => Ok(()),
        KeyData::Rsa(key)
            if key
                .n
                .as_positive_bytes()
                .is_some_and(|n| (256..=1024).contains(&n.len()))
                && key
                    .e
                    .as_positive_bytes()
                    .is_some_and(|e| !e.is_empty() && e.len() <= 8) =>
        {
            Ok(())
        }
        _ => Err(Failure::Unsupported),
    }
}
fn now() -> Result<u64, Failure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|n| n.as_secs())
        .map_err(|_| Failure::Unavailable)
}
fn valid_time(cert: &Certificate, time: u64) -> bool {
    cert.valid_after() <= time && time < cert.valid_before()
}

struct Policy {
    user: String,
    hostname: String,
    hosts: Vec<(Key, bool)>,
    users: Vec<Key>,
}
impl Policy {
    fn new(selected: &SshAuthGrantRequest) -> Result<Self, Failure> {
        selected.validate().map_err(|_| Failure::InvalidRequest)?;
        let mut hosts = Vec::new();
        for entry in &selected.host_keys {
            let key = Key::parse(
                decode_packet(&entry.key, SSH_AUTH_KEY_MAX).map_err(|_| Failure::InvalidRequest)?,
            )?;
            if entry.is_ca && key.certificate.is_some() {
                return Err(Failure::Unsupported);
            }
            hosts.push((key, entry.is_ca));
        }
        let mut users = Vec::new();
        for entry in &selected.user_keys {
            let key = Key::parse(
                decode_packet(entry, SSH_AUTH_KEY_MAX).map_err(|_| Failure::InvalidRequest)?,
            )?;
            if let Some(cert) = &key.certificate {
                if cert.cert_type() != CertType::User || !valid_time(cert, now()?) {
                    return Err(Failure::KeyUnavailable);
                }
            }
            users.push(key);
        }
        Ok(Self {
            user: selected.destination.user.clone(),
            hostname: selected.destination.hostname.clone(),
            hosts,
            users,
        })
    }
    fn bind(&self, packet: &[u8]) -> Result<(Vec<u8>, Vec<u8>), Failure> {
        let mut r = Reader::new(packet)?;
        r.byte_is(27)?;
        r.string_is(b"session-bind@openssh.com")?;
        let host = r.string()?;
        let session = r.string()?;
        let signature = packet::signature(r.string()?)?;
        r.byte_is(0)?;
        r.end()?;
        // Current SSH KEX hashes are bounded, nonempty binary identifiers.
        if session.is_empty() || session.len() > 128 {
            return Err(Failure::InvalidBinding);
        }
        let key = Key::parse(host.to_vec())?;
        match &key.certificate {
            Some(cert) => {
                if cert.cert_type() != CertType::Host
                    || !cert.critical_options().is_empty()
                    || !cert.valid_principals().iter().any(|p| p == &self.hostname)
                    || !valid_time(cert, now()?)
                {
                    return Err(Failure::InvalidBinding);
                }
                let trust = self.hosts.iter().any(|(trusted, ca)| {
                    if *ca {
                        trusted.data == *cert.signature_key()
                    } else {
                        trusted.blob == host
                    }
                });
                if !trust {
                    return Err(Failure::InvalidBinding);
                }
                // CA identity/principal/type/time were checked above. Validate
                // the certificate signature before trusting its leaf host key.
                cert.verify_signature()
                    .map_err(|_| Failure::InvalidBinding)?;
            }
            None if !self
                .hosts
                .iter()
                .any(|(trusted, ca)| !ca && trusted.blob == host) =>
            {
                return Err(Failure::InvalidBinding);
            }
            None => {}
        }
        key.data
            .verify(session, &signature)
            .map_err(|_| Failure::InvalidBinding)?;
        Ok((host.to_vec(), session.to_vec()))
    }
    fn sign<'a>(
        &'a self,
        packet: &'a [u8],
        host: &[u8],
        session: &[u8],
    ) -> Result<Signing<'a>, Failure> {
        if Key::parse(host.to_vec())?
            .certificate
            .as_ref()
            .is_some_and(|certificate| !valid_time(certificate, now().unwrap_or(u64::MAX)))
        {
            return Err(Failure::InvalidBinding);
        }
        let mut r = Reader::new(packet)?;
        r.byte_is(13)?;
        let blob = r.string()?;
        let data = r.string()?;
        let flags = r.u32()?;
        r.end()?;
        let key = self
            .users
            .iter()
            .find(|key| key.blob == blob)
            .ok_or(Failure::KeyUnavailable)?;
        if key
            .certificate
            .as_ref()
            .is_some_and(|c| !valid_time(c, now().unwrap_or(u64::MAX)))
        {
            return Err(Failure::KeyUnavailable);
        }
        let mut d = Reader::new(data)?;
        d.string_is(session)?;
        d.byte_is(50)?;
        d.string_is(self.user.as_bytes())?;
        d.string_is(b"ssh-connection")?;
        d.string_is(b"publickey-hostbound-v00@openssh.com")?;
        d.byte_is(1)?;
        let algorithm = key.user_algorithm(d.string()?, flags)?;
        d.string_is(blob)?;
        d.string_is(host)?;
        d.end()?;
        Ok(Signing {
            key: &key.data,
            data,
            algorithm,
        })
    }
}
struct Signing<'a> {
    key: &'a KeyData,
    data: &'a [u8],
    algorithm: Algorithm,
}
impl Signing<'_> {
    fn verify(&self, reply: &[u8]) -> Result<(), Failure> {
        let parse = || {
            let mut r = Reader::new(reply)?;
            r.byte_is(14)?;
            let signature = packet::signature(r.string()?)?;
            r.end()?;
            Ok::<_, Failure>(signature)
        };
        let signature = parse().map_err(|_| Failure::AgentRefused)?;
        if signature.algorithm() != self.algorithm {
            return Err(Failure::AgentRefused);
        }
        self.key
            .verify(self.data, &signature)
            .map_err(|_| Failure::AgentRefused)
    }
}
struct Connection<C> {
    host: Vec<u8>,
    session: Vec<u8>,
    agent: C,
}

/// Construct only from the native-owned original grant selection. No received
/// packet, keeper response or webview object may replace it. One owned control
/// loop serializes handle calls; cancellation drops that loop and this verifier.
pub(crate) struct GrantVerifier<A: LocalAgent> {
    policy: Policy,
    agent: A,
    deadline: Instant,
    last_request: u64,
    connections: HashMap<String, Connection<A::Connection>>,
    // Tombstones remain bounded by the grant's total connection budget. Closing
    // a socket cannot make its identity or a bound session reusable.
    seen_connections: HashSet<String>,
    seen_sessions: HashSet<Vec<u8>>,
}
impl<A: LocalAgent> GrantVerifier<A> {
    pub(crate) fn new(
        selected: &SshAuthGrantRequest,
        deadline: Instant,
        agent: A,
    ) -> Result<Self, Failure> {
        let current = Instant::now();
        if deadline <= current
            || deadline.duration_since(current) > Duration::from_secs(SSH_AUTH_LIFETIME.into())
        {
            return Err(Failure::Expired);
        }
        Ok(Self {
            policy: Policy::new(selected)?,
            agent,
            deadline,
            last_request: 0,
            connections: HashMap::new(),
            seen_connections: HashSet::new(),
            seen_sessions: HashSet::new(),
        })
    }
    pub(crate) async fn handle(
        &mut self,
        request: SshAuthRequest,
    ) -> Result<Option<SshAuthReply>, Failure> {
        request.validate().map_err(|_| Failure::InvalidRequest)?;
        if Instant::now() >= self.deadline {
            self.connections.clear();
            return Err(Failure::Expired);
        }
        let (connection_id, request_id, encoded, binding) = match request {
            SshAuthRequest::ConnectionClosed { connection_id } => {
                if !self.seen_connections.contains(&connection_id) {
                    return Err(Failure::InvalidRequest);
                }
                self.connections.remove(&connection_id);
                return Ok(None);
            }
            SshAuthRequest::SessionBind {
                connection_id,
                request_id,
                packet,
            } => (connection_id, request_id, packet, true),
            SshAuthRequest::Sign {
                connection_id,
                request_id,
                packet,
            } => (connection_id, request_id, packet, false),
        };
        if request_id <= self.last_request {
            return Err(Failure::InvalidRequest);
        }
        self.last_request = request_id;
        let packet =
            decode_packet(&encoded, SSH_AUTH_PACKET_MAX).map_err(|_| Failure::InvalidRequest)?;
        let deadline = self.deadline.min(Instant::now() + Duration::from_secs(30));
        let result = if binding {
            self.bind(&connection_id, &packet, deadline)
                .await
                .map(|_| None)
        } else {
            self.sign(&connection_id, &packet, deadline).await.map(Some)
        };
        // A result cannot race the absolute lifetime, even if a backend ignored
        // cancellation until its final write/signature returned.
        if Instant::now() >= deadline {
            self.connections.clear();
            return Err(Failure::Expired);
        }
        Ok(Some(match result {
            Ok(None) => SshAuthReply::Bound {
                connection_id,
                request_id,
            },
            Ok(Some(reply)) => SshAuthReply::Signature {
                connection_id,
                request_id,
                packet: STANDARD.encode(reply),
            },
            Err(error) => SshAuthReply::Failure {
                connection_id,
                request_id,
                error,
            },
        }))
    }
    async fn bind(&mut self, id: &str, packet: &[u8], deadline: Instant) -> Result<(), Failure> {
        if self.seen_connections.len() >= SSH_AUTH_CONNECTIONS_MAX
            || !self.seen_connections.insert(id.into())
        {
            self.connections.remove(id);
            return Err(Failure::InvalidBinding);
        }
        let (host, session) = self.policy.bind(packet)?;
        if !self.seen_sessions.insert(session.clone()) {
            return Err(Failure::InvalidBinding);
        }
        let agent = tokio::time::timeout_at(deadline, async {
            let mut agent = self.agent.connect().await?;
            // Requiring an explicit successful bind preserves agent-side key
            // destination constraints. Unsupported agents never downgrade.
            if agent.exchange(packet).await? != [6] {
                return Err(Failure::AgentRefused);
            }
            Ok(agent)
        })
        .await
        .map_err(|_| Failure::Expired)??;
        self.connections.insert(
            id.into(),
            Connection {
                host,
                session,
                agent,
            },
        );
        Ok(())
    }
    async fn sign(
        &mut self,
        id: &str,
        packet: &[u8],
        deadline: Instant,
    ) -> Result<Vec<u8>, Failure> {
        // Removal before awaiting makes canceled/failed work permanently lose
        // this connection; it cannot retry a half-completed local agent request.
        let mut connection = self.connections.remove(id).ok_or(Failure::InvalidBinding)?;
        let signing = self
            .policy
            .sign(packet, &connection.host, &connection.session)?;
        let reply = tokio::time::timeout_at(deadline, connection.agent.exchange(packet))
            .await
            .map_err(|_| Failure::Expired)??;
        signing.verify(&reply)?;
        self.connections.insert(id.into(), connection);
        Ok(reply)
    }
}

#[cfg(all(test, unix))]
#[path = "ssh_agent/live_tests.rs"]
mod live_tests;
#[cfg(test)]
#[path = "ssh_agent/tests.rs"]
mod tests;
