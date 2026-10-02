//! Native-only destination-bound authentication. Validation here bounds the wire;
//! it is not host trust or cryptographic permission to ask an agent to sign.
use anyhow::{ensure, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

pub const SSH_AUTH_VERSION: u32 = 1;
pub const SSH_AUTH_FRAME_MAX: usize = 128 * 1024;
pub const SSH_AUTH_PACKET_MAX: usize = 64 * 1024;
pub const SSH_AUTH_KEY_MAX: usize = 16 * 1024;
pub const SSH_AUTH_KEYS_MAX: usize = 8;
pub const SSH_AUTH_LIFETIME: u32 = 180;
pub const SSH_AUTH_PENDING_MAX: usize = 8;
pub const SSH_AUTH_CONNECTIONS_MAX: usize = 32;
pub const SSH_AUTH_GRANT_HEADER: &str = "x-chimaera-ssh-auth-grant";

/// Boot identity is not authority without the authenticated capability response.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SshAuthCapabilities {
    pub version: u32,
    #[serde(default)]
    pub hostbound_v1: bool,
    #[serde(default)]
    pub register_only_v1: bool,
    #[serde(default)]
    pub keeper_boot: String,
}
impl SshAuthCapabilities {
    pub fn supported(&self) -> bool {
        self.version == SSH_AUTH_VERSION && self.hostbound_v1 && opaque(&self.keeper_boot)
    }
    pub fn registration_supported(&self) -> bool {
        self.supported() && self.register_only_v1
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SshAuthDestination {
    pub hostname: String,
    pub user: String,
    pub port: u16,
}
impl SshAuthDestination {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            atom(&self.hostname) && atom(&self.user) && self.port > 0,
            "invalid SSH authentication destination"
        );
        Ok(())
    }
    pub fn matches(&self, target: &crate::SshTarget) -> bool {
        self.hostname == target.hostname
            && Some(self.user.as_str()) == target.user.as_deref()
            && self.port == target.port
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SshAuthHostKey {
    pub key: String,
    pub is_ca: bool,
}
/// Enclosing grant/packet types deliberately omit Debug, including public keys.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshAuthGrantRequest {
    pub version: u32,
    pub keeper_boot: String,
    pub destination: SshAuthDestination,
    pub host_keys: Vec<SshAuthHostKey>,
    pub user_keys: Vec<String>,
}
impl SshAuthGrantRequest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == SSH_AUTH_VERSION && opaque(&self.keeper_boot),
            "unsupported SSH authentication grant"
        );
        self.destination.validate()?;
        ensure!(
            !self.host_keys.is_empty()
                && self.host_keys.len() <= SSH_AUTH_KEYS_MAX
                && !self.user_keys.is_empty()
                && self.user_keys.len() <= SSH_AUTH_KEYS_MAX,
            "invalid SSH authentication key count"
        );
        let mut hosts = std::collections::BTreeSet::new();
        for key in &self.host_keys {
            let bytes = decode_packet(&key.key, SSH_AUTH_KEY_MAX)?;
            ensure!(hosts.insert(bytes), "duplicate SSH authentication host key");
        }
        let mut users = std::collections::BTreeSet::new();
        for key in &self.user_keys {
            let bytes = decode_packet(key, SSH_AUTH_KEY_MAX)?;
            ensure!(users.insert(bytes), "duplicate SSH authentication user key");
        }
        // Packet/key cryptographic validity is checked by the SSH verifier, not
        // inferred from base64 or the caller's declared algorithm/trust.
        ensure!(
            serde_json::to_vec(self)?.len() <= SSH_AUTH_FRAME_MAX,
            "SSH authentication grant too large"
        );
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct SshAuthGrant {
    pub version: u32,
    pub grant_id: String,
    pub expires_in: u32,
}
impl SshAuthGrant {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == SSH_AUTH_VERSION
                && opaque(&self.grant_id)
                && self.expires_in > 0
                && self.expires_in <= SSH_AUTH_LIFETIME,
            "invalid SSH authentication grant response"
        );
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SshAuthHello {
    Ready {
        version: u32,
        grant_id: String,
        keeper_boot: String,
    },
}
impl SshAuthHello {
    pub fn from_frame(bytes: &[u8], grant: &SshAuthGrant, boot: &str) -> Result<Self> {
        let hello: Self = frame(bytes)?;
        let Self::Ready {
            version,
            grant_id,
            keeper_boot,
        } = &hello;
        ensure!(
            *version == SSH_AUTH_VERSION && grant_id == &grant.grant_id && keeper_boot == boot,
            "SSH authentication readiness mismatch"
        );
        Ok(hello)
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SshAuthFailure {
    Unsupported,
    InvalidBinding,
    InvalidRequest,
    KeyUnavailable,
    AgentRefused,
    Expired,
    Revoked,
    Unavailable,
    #[serde(other)]
    Unknown,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SshAuthRequest {
    SessionBind {
        connection_id: String,
        request_id: u64,
        packet: String,
    },
    Sign {
        connection_id: String,
        request_id: u64,
        packet: String,
    },
    ConnectionClosed {
        connection_id: String,
    },
}
impl SshAuthRequest {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::SessionBind {
                connection_id,
                request_id,
                packet,
            }
            | Self::Sign {
                connection_id,
                request_id,
                packet,
            } => {
                correlation(connection_id, *request_id)?;
                decode_packet(packet, SSH_AUTH_PACKET_MAX)?;
            }
            Self::ConnectionClosed { connection_id } => {
                ensure!(
                    opaque(connection_id),
                    "invalid SSH authentication connection"
                );
            }
        }
        Ok(())
    }
    pub fn from_frame(bytes: &[u8]) -> Result<Self> {
        let request: Self = frame(bytes)?;
        request.validate()?;
        Ok(request)
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SshAuthReply {
    Bound {
        connection_id: String,
        request_id: u64,
    },
    Signature {
        connection_id: String,
        request_id: u64,
        packet: String,
    },
    Failure {
        connection_id: String,
        request_id: u64,
        error: SshAuthFailure,
    },
}
impl SshAuthReply {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Bound {
                connection_id,
                request_id,
            } => correlation(connection_id, *request_id)?,
            Self::Signature {
                connection_id,
                request_id,
                packet,
            } => {
                correlation(connection_id, *request_id)?;
                decode_packet(packet, SSH_AUTH_PACKET_MAX)?;
            }
            Self::Failure {
                connection_id,
                request_id,
                error,
            } => {
                correlation(connection_id, *request_id)?;
                ensure!(
                    *error != SshAuthFailure::Unknown,
                    "unknown SSH authentication failure"
                );
            }
        }
        Ok(())
    }
    pub fn from_frame(bytes: &[u8]) -> Result<Self> {
        let reply: Self = frame(bytes)?;
        reply.validate()?;
        Ok(reply)
    }
    /// A correlated envelope still needs the native cryptographic verifier.
    pub fn matches(&self, request: &SshAuthRequest) -> bool {
        let (id, sequence, bind) = match request {
            SshAuthRequest::SessionBind {
                connection_id,
                request_id,
                ..
            } => (connection_id, request_id, true),
            SshAuthRequest::Sign {
                connection_id,
                request_id,
                ..
            } => (connection_id, request_id, false),
            SshAuthRequest::ConnectionClosed { .. } => return false,
        };
        match self {
            Self::Bound {
                connection_id,
                request_id,
            } => bind && connection_id == id && request_id == sequence,
            Self::Signature {
                connection_id,
                request_id,
                ..
            } => !bind && connection_id == id && request_id == sequence,
            Self::Failure {
                connection_id,
                request_id,
                error,
            } => *error != SshAuthFailure::Unknown && connection_id == id && request_id == sequence,
        }
    }
}
fn atom(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:@[]".contains(&b))
}
fn opaque(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn correlation(connection_id: &str, request_id: u64) -> Result<()> {
    ensure!(
        opaque(connection_id) && request_id > 0,
        "invalid SSH authentication correlation"
    );
    Ok(())
}
pub fn decode_packet(value: &str, maximum: usize) -> Result<Vec<u8>> {
    ensure!(
        value.len() <= maximum.div_ceil(3) * 4,
        "SSH authentication packet too large"
    );
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| anyhow::anyhow!("invalid SSH authentication packet"))?;
    ensure!(
        !bytes.is_empty() && bytes.len() <= maximum,
        "invalid SSH authentication packet size"
    );
    Ok(bytes)
}
fn frame<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    ensure!(
        bytes.len() <= SSH_AUTH_FRAME_MAX,
        "SSH authentication frame too large"
    );
    serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!("invalid SSH authentication frame"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_ceiling_unknown_kinds_and_parser_diagnostics_fail_closed() {
        for bytes in [
            br#"{"type":"forward","packet":"sensitive"}"#.as_slice(),
            br#"{"type":"sign","connection_id":"c","request_id":1,"packet":"%%%sensitive"}"#,
            br#"{"type":"sign","connection_id":"c","request_id":1,"packet":"AQ==","extra":true}"#,
        ] {
            let error = SshAuthRequest::from_frame(bytes).err().unwrap();
            assert!(!format!("{error:?}").contains("sensitive"));
        }
        assert!(SshAuthRequest::from_frame(&vec![b' '; SSH_AUTH_FRAME_MAX + 1]).is_err());
        assert!(decode_packet(
            &STANDARD.encode(vec![1; SSH_AUTH_PACKET_MAX + 1]),
            SSH_AUTH_PACKET_MAX
        )
        .is_err());
        assert!(decode_packet(
            &STANDARD.encode(vec![1; SSH_AUTH_PACKET_MAX]),
            SSH_AUTH_PACKET_MAX
        )
        .is_ok());
        assert!(SshAuthReply::from_frame(
            br#"{"type":"failure","connection_id":"c","request_id":1,"error":"invented"}"#
        )
        .is_err());
    }
    #[test]
    fn exact_kind_and_live_request_identity_are_required() {
        let request = SshAuthRequest::SessionBind {
            connection_id: "c".into(),
            request_id: 7,
            packet: "AQ==".into(),
        };
        let good = SshAuthReply::Bound {
            connection_id: "c".into(),
            request_id: 7,
        };
        assert!(good.matches(&request));
        for reply in [
            SshAuthReply::Bound {
                connection_id: "d".into(),
                request_id: 7,
            },
            SshAuthReply::Bound {
                connection_id: "c".into(),
                request_id: 8,
            },
            SshAuthReply::Signature {
                connection_id: "c".into(),
                request_id: 7,
                packet: "AQ==".into(),
            },
        ] {
            assert!(!reply.matches(&request));
        }
    }
    #[test]
    fn grant_key_counts_duplicates_scope_and_expiry_are_bounded() {
        let mut grant = SshAuthGrantRequest {
            version: 1,
            keeper_boot: "boot".into(),
            destination: SshAuthDestination {
                hostname: "login.example.invalid".into(),
                user: "person".into(),
                port: 22,
            },
            host_keys: vec![SshAuthHostKey {
                key: "AQ==".into(),
                is_ca: false,
            }],
            user_keys: vec!["Ag==".into()],
        };
        grant.validate().unwrap();
        grant.user_keys.push("Ag==".into());
        assert!(grant.validate().is_err());
        grant.user_keys = (1..=9).map(|v| STANDARD.encode([v])).collect();
        assert!(grant.validate().is_err());
        grant.user_keys = vec!["Ag==".into()];
        grant.destination.user = "person\nProxyCommand bad".into();
        assert!(grant.validate().is_err());
        for expires_in in [0, SSH_AUTH_LIFETIME + 1] {
            assert!(SshAuthGrant {
                version: 1,
                grant_id: "g".into(),
                expires_in
            }
            .validate()
            .is_err());
        }
        assert!(!SshAuthCapabilities::default().supported());
    }
    #[test]
    fn readiness_requires_exact_grant_boot_version_and_cannot_reappear_as_a_request() {
        let grant = SshAuthGrant {
            version: 1,
            grant_id: "grant".into(),
            expires_in: 180,
        };
        let good = br#"{"type":"ready","version":1,"grant_id":"grant","keeper_boot":"boot"}"#;
        assert!(SshAuthHello::from_frame(good, &grant, "boot").is_ok());
        assert!(SshAuthHello::from_frame(good, &grant, "other-boot").is_err());
        assert!(SshAuthHello::from_frame(
            br#"{"type":"ready","version":2,"grant_id":"grant","keeper_boot":"boot"}"#,
            &grant,
            "boot"
        )
        .is_err());
        assert!(SshAuthHello::from_frame(
            br#"{"type":"ready","version":1,"grant_id":"wrong","keeper_boot":"boot"}"#,
            &grant,
            "boot"
        )
        .is_err());
        assert!(SshAuthHello::from_frame(
            br#"{"type":"ready","version":1,"grant_id":"grant","keeper_boot":"boot","extra":true}"#,
            &grant,
            "boot"
        )
        .is_err());
        assert!(
            SshAuthRequest::from_frame(good).is_err(),
            "later Ready is not a signing request"
        );
        let caps = SshAuthCapabilities {
            version: 1,
            hostbound_v1: true,
            register_only_v1: false,
            keeper_boot: "boot".into(),
        };
        assert!(caps.supported());
        assert!(!caps.registration_supported());
    }
}
