//! Resolved ordered routes only. OpenSSH configuration and cryptographic trust
//! remain native responsibilities; these types cannot flatten a ProxyJump chain.
use super::*;
use std::collections::BTreeSet;

pub const SSH_AUTH_JUMPS_MAX: usize = 3;
pub const SSH_AUTH_ROUTE_GRANT_HEADER: &str = "x-chimaera-ssh-auth-route-grant";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SshRoute {
    pub version: u32,
    pub jumps: Vec<SshAuthDestination>,
}
impl SshRoute {
    pub fn validate(&self, destination: &SshAuthDestination) -> Result<()> {
        ensure!(
            self.version == SSH_AUTH_VERSION && self.jumps.len() <= SSH_AUTH_JUMPS_MAX,
            "invalid SSH route"
        );
        let mut seen = BTreeSet::new();
        for leg in self.jumps.iter().chain(std::iter::once(destination)) {
            leg.validate()?;
            ensure!(
                seen.insert((leg.hostname.to_ascii_lowercase(), &leg.user, leg.port)),
                "repeated SSH route destination"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SshRouteMode {
    Key,
    Interactive,
}

/// Prompt provenance is checked again by the original native Connect owner;
/// syntactic validity never grants permission to display credential UI.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshRoutePromptAuth {
    pub grant_id: String,
    pub keeper_boot: String,
    pub leg: u8,
    pub mode: SshRouteMode,
    pub destination: SshAuthDestination,
}
impl std::fmt::Debug for SshRoutePromptAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SshRoutePromptAuth(..)")
    }
}
impl SshRoutePromptAuth {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            opaque(&self.grant_id)
                && opaque(&self.keeper_boot)
                && usize::from(self.leg) <= SSH_AUTH_JUMPS_MAX,
            "invalid SSH route prompt identity"
        );
        self.destination.validate()
    }
    pub fn matches(&self, grant: &SshRouteGrant, keeper_boot: &str) -> bool {
        self.validate().is_ok()
            && grant.validate().is_ok()
            && self.grant_id == grant.grant_id
            && self.keeper_boot == keeper_boot
            && grant.modes.get(usize::from(self.leg)) == Some(&self.mode)
            && grant
                .route
                .jumps
                .iter()
                .chain(std::iter::once(&grant.destination))
                .nth(usize::from(self.leg))
                == Some(&self.destination)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshRouteAuthLeg {
    pub destination: SshAuthDestination,
    pub mode: SshRouteMode,
    pub host_keys: Vec<SshAuthHostKey>,
    pub user_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<SshRoutePolicy>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SshRouteMethod {
    Publickey,
    KeyboardInteractive,
    Password,
}

/// Resolved native policy only; no configuration expressions or directives.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshRoutePolicy {
    pub version: u32,
    pub methods: Vec<SshRouteMethod>,
    pub host_key_algorithms: Vec<String>,
    pub ca_signature_algorithms: Vec<String>,
    pub pubkey_accepted_algorithms: Vec<String>,
}
impl SshRoutePolicy {
    pub fn validate(&self, mode: SshRouteMode) -> Result<()> {
        ensure!(
            self.version == 1 && !self.methods.is_empty() && self.methods.len() <= 3,
            "invalid SSH route policy"
        );
        for (index, method) in self.methods.iter().enumerate() {
            ensure!(
                !self.methods[..index].contains(method),
                "duplicate SSH route method"
            );
        }
        match mode {
            SshRouteMode::Key => ensure!(
                self.methods.first() == Some(&SshRouteMethod::Publickey),
                "key SSH route policy order mismatch"
            ),
            SshRouteMode::Interactive => ensure!(
                !self.methods.contains(&SshRouteMethod::Publickey),
                "interactive SSH route policy carries key method"
            ),
        }
        for list in [
            &self.host_key_algorithms,
            &self.ca_signature_algorithms,
            &self.pubkey_accepted_algorithms,
        ] {
            ensure!(
                !list.is_empty() && list.len() <= 64,
                "invalid SSH route algorithm count"
            );
            let mut seen = BTreeSet::new();
            for name in list {
                ensure!(
                    !name.is_empty()
                        && name.len() <= 128
                        && !name.starts_with(['+', '-'])
                        && name.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || b"-@._+".contains(&byte)
                        })
                        && seen.insert(name),
                    "invalid SSH route algorithm"
                );
            }
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= 8 * 1024,
            "SSH route policy too large"
        );
        Ok(())
    }
    pub fn permits_interaction(&self) -> bool {
        self.methods.iter().any(|method| {
            matches!(
                method,
                SshRouteMethod::KeyboardInteractive | SshRouteMethod::Password
            )
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshRouteGrantRequest {
    pub version: u32,
    pub keeper_boot: String,
    pub destination: SshAuthDestination,
    pub route: SshRoute,
    pub legs: Vec<SshRouteAuthLeg>,
}
impl SshRouteGrantRequest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == SSH_AUTH_VERSION && opaque(&self.keeper_boot),
            "unsupported SSH route grant"
        );
        self.route.validate(&self.destination)?;
        ensure!(
            self.legs.len() == self.route.jumps.len() + 1,
            "SSH route leg count mismatch"
        );
        ensure!(
            self.legs.iter().all(|leg| leg.policy.is_some())
                || self.legs.iter().all(|leg| leg.policy.is_none()),
            "partial SSH route policy"
        );
        for (leg, destination) in self.legs.iter().zip(
            self.route
                .jumps
                .iter()
                .chain(std::iter::once(&self.destination)),
        ) {
            ensure!(leg.destination == *destination, "SSH route leg mismatch");
            if let Some(policy) = &leg.policy {
                policy.validate(leg.mode)?;
            }
            ensure!(
                !leg.host_keys.is_empty() && leg.host_keys.len() <= SSH_AUTH_KEYS_MAX,
                "invalid SSH route host key count"
            );
            let mut hosts = BTreeSet::new();
            for key in &leg.host_keys {
                ensure!(
                    hosts.insert(decode_packet(&key.key, SSH_AUTH_KEY_MAX)?),
                    "duplicate SSH route host key"
                );
            }
            match leg.mode {
                SshRouteMode::Key => {
                    ensure!(
                        !leg.user_keys.is_empty() && leg.user_keys.len() <= SSH_AUTH_KEYS_MAX,
                        "invalid SSH route user key count"
                    );
                    let mut users = BTreeSet::new();
                    for key in &leg.user_keys {
                        ensure!(
                            users.insert(decode_packet(key, SSH_AUTH_KEY_MAX)?),
                            "duplicate SSH route user key"
                        );
                    }
                }
                SshRouteMode::Interactive => ensure!(
                    leg.user_keys.is_empty(),
                    "interactive SSH route carries user keys"
                ),
            }
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= SSH_AUTH_FRAME_MAX,
            "SSH route grant too large"
        );
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SshRouteGrant {
    pub version: u32,
    pub grant_id: String,
    pub expires_in: u32,
    pub destination: SshAuthDestination,
    pub route: SshRoute,
    pub modes: Vec<SshRouteMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policies: Option<Vec<SshRoutePolicy>>,
}
impl SshRouteGrant {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == SSH_AUTH_VERSION
                && opaque(&self.grant_id)
                && self.expires_in == SSH_AUTH_LIFETIME,
            "invalid SSH route grant response"
        );
        self.route.validate(&self.destination)?;
        ensure!(
            self.modes.len() == self.route.jumps.len() + 1,
            "SSH route grant mode count mismatch"
        );
        if let Some(policies) = &self.policies {
            ensure!(
                policies.len() == self.modes.len(),
                "SSH route policy count mismatch"
            );
            for (policy, mode) in policies.iter().zip(&self.modes) {
                policy.validate(*mode)?;
            }
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= SSH_AUTH_FRAME_MAX,
            "SSH route grant too large"
        );
        Ok(())
    }
    pub fn matches(&self, request: &SshRouteGrantRequest) -> bool {
        self.destination == request.destination
            && self.route == request.route
            && self
                .modes
                .iter()
                .copied()
                .eq(request.legs.iter().map(|leg| leg.mode))
            && self.policies
                == request
                    .legs
                    .iter()
                    .map(|leg| leg.policy.clone())
                    .collect::<Option<Vec<_>>>()
    }
    fn key_leg(&self, leg: u8) -> Result<()> {
        ensure!(
            self.modes.get(usize::from(leg)) == Some(&SshRouteMode::Key),
            "SSH route authentication leg mismatch"
        );
        Ok(())
    }
}

/// The host row must positively echo the resolved route, even for zero jumps.
#[derive(Clone, Serialize, Deserialize)]
pub struct SshRouteHost {
    #[serde(flatten)]
    pub host: crate::Host,
    pub ssh: crate::SshTarget,
    pub ssh_route: SshRoute,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SshRouteHello {
    Ready {
        version: u32,
        grant_id: String,
        keeper_boot: String,
        legs: u8,
    },
}
impl SshRouteHello {
    pub fn from_frame(bytes: &[u8], grant: &SshRouteGrant, boot: &str) -> Result<Self> {
        grant.validate()?;
        ensure!(opaque(boot), "invalid SSH route boot");
        let hello: Self = frame(bytes)?;
        let Self::Ready {
            version,
            grant_id,
            keeper_boot,
            legs,
        } = &hello;
        ensure!(
            *version == SSH_AUTH_VERSION
                && grant_id == &grant.grant_id
                && keeper_boot == boot
                && usize::from(*legs) == grant.modes.len(),
            "SSH route readiness mismatch"
        );
        Ok(hello)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SshRouteRequest {
    SessionBind {
        leg: u8,
        connection_id: String,
        request_id: u64,
        packet: String,
    },
    Sign {
        leg: u8,
        connection_id: String,
        request_id: u64,
        packet: String,
    },
    ConnectionClosed {
        leg: u8,
        connection_id: String,
    },
}
impl SshRouteRequest {
    pub fn from_frame(bytes: &[u8], grant: &SshRouteGrant) -> Result<Self> {
        let request: Self = frame(bytes)?;
        request.validate(grant)?;
        Ok(request)
    }
    pub fn validate(&self, grant: &SshRouteGrant) -> Result<()> {
        grant.validate()?;
        match self {
            Self::SessionBind {
                leg,
                connection_id,
                request_id,
                packet,
            }
            | Self::Sign {
                leg,
                connection_id,
                request_id,
                packet,
            } => {
                grant.key_leg(*leg)?;
                correlation(connection_id, *request_id)?;
                decode_packet(packet, SSH_AUTH_PACKET_MAX)?;
            }
            Self::ConnectionClosed { leg, connection_id } => {
                grant.key_leg(*leg)?;
                ensure!(opaque(connection_id), "invalid SSH route connection");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SshRouteReply {
    Bound {
        leg: u8,
        connection_id: String,
        request_id: u64,
    },
    Signature {
        leg: u8,
        connection_id: String,
        request_id: u64,
        packet: String,
    },
    Failure {
        leg: u8,
        connection_id: String,
        request_id: u64,
        error: SshAuthFailure,
    },
}
impl SshRouteReply {
    pub fn from_frame(bytes: &[u8], grant: &SshRouteGrant) -> Result<Self> {
        let reply: Self = frame(bytes)?;
        reply.validate(grant)?;
        Ok(reply)
    }
    pub fn validate(&self, grant: &SshRouteGrant) -> Result<()> {
        grant.validate()?;
        let (leg, id, sequence) = match self {
            Self::Bound {
                leg,
                connection_id,
                request_id,
            } => (*leg, connection_id, *request_id),
            Self::Signature {
                leg,
                connection_id,
                request_id,
                packet,
            } => {
                decode_packet(packet, SSH_AUTH_PACKET_MAX)?;
                (*leg, connection_id, *request_id)
            }
            Self::Failure {
                leg,
                connection_id,
                request_id,
                error,
            } => {
                ensure!(
                    *error != SshAuthFailure::Unknown,
                    "unknown SSH route failure"
                );
                (*leg, connection_id, *request_id)
            }
        };
        grant.key_leg(leg)?;
        correlation(id, sequence)
    }
    pub fn matches(&self, request: &SshRouteRequest) -> bool {
        let (leg, id, seq, bind) = match request {
            SshRouteRequest::SessionBind {
                leg,
                connection_id,
                request_id,
                ..
            } => (leg, connection_id, request_id, true),
            SshRouteRequest::Sign {
                leg,
                connection_id,
                request_id,
                ..
            } => (leg, connection_id, request_id, false),
            SshRouteRequest::ConnectionClosed { .. } => return false,
        };
        match self {
            Self::Bound {
                leg: other,
                connection_id,
                request_id,
            } => bind && other == leg && connection_id == id && request_id == seq,
            Self::Signature {
                leg: other,
                connection_id,
                request_id,
                ..
            } => !bind && other == leg && connection_id == id && request_id == seq,
            Self::Failure {
                leg: other,
                connection_id,
                request_id,
                error,
            } => {
                *error != SshAuthFailure::Unknown
                    && other == leg
                    && connection_id == id
                    && request_id == seq
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn destination(name: &str) -> SshAuthDestination {
        SshAuthDestination {
            hostname: name.into(),
            user: "person".into(),
            port: 22,
        }
    }
    fn request() -> SshRouteGrantRequest {
        let jump = destination("jump.example.invalid");
        let destination = destination("target.example.invalid");
        SshRouteGrantRequest {
            version: 1,
            keeper_boot: "boot".into(),
            route: SshRoute {
                version: 1,
                jumps: vec![jump.clone()],
            },
            legs: vec![
                SshRouteAuthLeg {
                    policy: None,
                    destination: jump,
                    mode: SshRouteMode::Interactive,
                    host_keys: vec![SshAuthHostKey {
                        key: "AQ==".into(),
                        is_ca: false,
                    }],
                    user_keys: vec![],
                },
                SshRouteAuthLeg {
                    policy: None,
                    destination: destination.clone(),
                    mode: SshRouteMode::Key,
                    host_keys: vec![SshAuthHostKey {
                        key: "Ag==".into(),
                        is_ca: false,
                    }],
                    user_keys: vec!["Aw==".into()],
                },
            ],
            destination,
        }
    }
    fn grant(request: &SshRouteGrantRequest) -> SshRouteGrant {
        SshRouteGrant {
            policies: None,
            version: 1,
            grant_id: "grant".into(),
            expires_in: 180,
            destination: request.destination.clone(),
            route: request.route.clone(),
            modes: request.legs.iter().map(|leg| leg.mode).collect(),
        }
    }
    fn policy(mode: SshRouteMode) -> SshRoutePolicy {
        SshRoutePolicy {
            version: 1,
            methods: if mode == SshRouteMode::Key {
                vec![SshRouteMethod::Publickey, SshRouteMethod::Password]
            } else {
                vec![SshRouteMethod::KeyboardInteractive]
            },
            host_key_algorithms: vec!["ssh-ed25519".into()],
            ca_signature_algorithms: vec!["ssh-ed25519".into()],
            pubkey_accepted_algorithms: vec!["ssh-ed25519".into()],
        }
    }
    #[test]
    fn policies_are_all_leg_exact_and_method_algorithm_bounds_fail_closed() {
        let mut request = request();
        request.legs[0].policy = Some(policy(request.legs[0].mode));
        assert!(request.validate().is_err());
        request.legs[1].policy = Some(policy(request.legs[1].mode));
        request.validate().unwrap();
        let mut receipt = grant(&request);
        assert!(
            !receipt.matches(&request),
            "absent policy is no acknowledgment"
        );
        receipt.policies = request.legs.iter().map(|leg| leg.policy.clone()).collect();
        receipt.validate().unwrap();
        assert!(receipt.matches(&request));
        receipt.policies.as_mut().unwrap().swap(0, 1);
        assert!(receipt.validate().is_err());
        assert!(!receipt.matches(&request));
        let mut key_only = policy(SshRouteMode::Key);
        key_only.methods = vec![SshRouteMethod::Publickey];
        key_only.validate(SshRouteMode::Key).unwrap();
        assert!(!key_only.permits_interaction());
        for methods in [
            vec![SshRouteMethod::Password],
            vec![
                SshRouteMethod::Password,
                SshRouteMethod::KeyboardInteractive,
            ],
            vec![SshRouteMethod::KeyboardInteractive],
        ] {
            let mut interactive = policy(SshRouteMode::Interactive);
            interactive.methods = methods;
            interactive.validate(SshRouteMode::Interactive).unwrap();
            assert!(interactive.permits_interaction());
        }
        for methods in [
            vec![],
            vec![SshRouteMethod::Password, SshRouteMethod::Publickey],
            vec![SshRouteMethod::Publickey, SshRouteMethod::Publickey],
        ] {
            let mut invalid = key_only.clone();
            invalid.methods = methods;
            assert!(invalid.validate(SshRouteMode::Key).is_err());
        }
        for algorithm in [
            "+ssh-ed25519",
            "-ssh-rsa",
            "ssh-*",
            "ssh-ed25519,ssh-rsa",
            "ssh-ed25519\nProxyCommand",
            "",
        ] {
            let mut invalid = key_only.clone();
            invalid.host_key_algorithms = vec![algorithm.into()];
            assert!(invalid.validate(SshRouteMode::Key).is_err());
        }
        let mut invalid = key_only.clone();
        invalid.ca_signature_algorithms = vec!["ssh-ed25519".into(); 2];
        assert!(invalid.validate(SshRouteMode::Key).is_err());
        invalid.ca_signature_algorithms =
            (0..65).map(|index| format!("algorithm-{index}")).collect();
        assert!(invalid.validate(SshRouteMode::Key).is_err());
        invalid.ca_signature_algorithms = (0..64)
            .map(|index| format!("{}-{index}", "a".repeat(122)))
            .collect();
        invalid.host_key_algorithms = invalid.ca_signature_algorithms.clone();
        assert!(
            invalid.validate(SshRouteMode::Key).is_err(),
            "total policy bytes remain bounded"
        );
        let mut old = grant(&request);
        old.policies = None;
        let value = serde_json::to_value(old).unwrap();
        assert!(value.get("policies").is_none());
    }
    #[test]
    fn exact_order_mode_cycle_and_aggregate_bounds() {
        let good = request();
        good.validate().unwrap();
        let mut changed = good.clone();
        changed.legs.swap(0, 1);
        assert!(changed.validate().is_err());
        let mut changed = good.clone();
        changed.legs[0].user_keys.push("AQ==".into());
        assert!(changed.validate().is_err());
        let mut changed = good.clone();
        changed.legs[1].user_keys.clear();
        assert!(changed.validate().is_err());
        let mut changed = good.clone();
        changed.route.jumps.push(changed.destination.clone());
        assert!(changed.validate().is_err());
        let mut changed = good.clone();
        changed.route.jumps[0].hostname = changed.destination.hostname.to_uppercase();
        assert!(changed.route.validate(&changed.destination).is_err());
        let mut changed = good.clone();
        changed.route.jumps = (0..4).map(|n| destination(&format!("jump{n}"))).collect();
        assert!(changed.route.validate(&changed.destination).is_err());
        let mut changed = good;
        for leg in &mut changed.legs {
            leg.host_keys = (0..8)
                .map(|n| SshAuthHostKey {
                    key: STANDARD.encode(vec![n; SSH_AUTH_KEY_MAX]),
                    is_ca: false,
                })
                .collect();
        }
        assert!(
            changed.validate().is_err(),
            "aggregate ceiling, not just per-key ceiling"
        );
    }
    #[test]
    fn prompt_metadata_requires_exact_grant_boot_leg_mode_and_destination() {
        let request = request();
        let grant = grant(&request);
        let good = SshRoutePromptAuth {
            grant_id: grant.grant_id.clone(),
            keeper_boot: request.keeper_boot.clone(),
            leg: 1,
            mode: grant.modes[1],
            destination: grant.destination.clone(),
        };
        assert!(good.matches(&grant, &request.keeper_boot));
        assert!(!good.matches(&grant, "changed-boot"));
        for changed in [
            SshRoutePromptAuth {
                grant_id: "other".into(),
                ..good.clone()
            },
            SshRoutePromptAuth {
                keeper_boot: "other".into(),
                ..good.clone()
            },
            SshRoutePromptAuth {
                leg: 0,
                ..good.clone()
            },
            SshRoutePromptAuth {
                mode: SshRouteMode::Interactive,
                ..good.clone()
            },
            SshRoutePromptAuth {
                destination: destination("other"),
                ..good.clone()
            },
        ] {
            assert!(!changed.matches(&grant, &request.keeper_boot));
        }
        let mut changed = good.clone();
        changed.leg = 4;
        assert!(changed.validate().is_err());
        changed = good;
        changed.grant_id = "x".repeat(129);
        assert!(changed.validate().is_err());
    }
    #[test]
    fn receipt_and_ready_do_not_accept_changed_identity_mode_or_legacy_hello() {
        let request = request();
        let mut receipt = grant(&request);
        assert!(receipt.matches(&request));
        receipt.modes.swap(0, 1);
        assert!(!receipt.matches(&request));
        receipt = grant(&request);
        receipt.expires_in = 179;
        assert!(receipt.validate().is_err());
        let receipt = grant(&request);
        let ready =
            br#"{"type":"ready","version":1,"grant_id":"grant","keeper_boot":"boot","legs":2}"#;
        SshRouteHello::from_frame(ready, &receipt, "boot").unwrap();
        for bytes in [
            br#"{"type":"ready","version":1,"grant_id":"grant","keeper_boot":"boot"}"#.as_slice(),
            br#"{"type":"ready","version":1,"grant_id":"grant","keeper_boot":"boot","legs":1}"#,
            br#"{"type":"ready","version":1,"grant_id":"grant","keeper_boot":"other","legs":2}"#,
        ] {
            assert!(SshRouteHello::from_frame(bytes, &receipt, "boot").is_err());
        }
    }
    #[test]
    fn key_packets_correlate_leg_and_kind_and_cannot_target_interactive_leg() {
        let receipt = grant(&request());
        let bytes = br#"{"type":"session_bind","leg":1,"connection_id":"c","request_id":7,"packet":"AQ=="}"#;
        let request = SshRouteRequest::from_frame(bytes, &receipt).unwrap();
        let reply = SshRouteReply::Bound {
            leg: 1,
            connection_id: "c".into(),
            request_id: 7,
        };
        reply.validate(&receipt).unwrap();
        assert!(reply.matches(&request));
        for reply in [
            SshRouteReply::Bound {
                leg: 0,
                connection_id: "c".into(),
                request_id: 7,
            },
            SshRouteReply::Bound {
                leg: 1,
                connection_id: "c".into(),
                request_id: 8,
            },
            SshRouteReply::Signature {
                leg: 1,
                connection_id: "c".into(),
                request_id: 7,
                packet: "AQ==".into(),
            },
        ] {
            assert!(!reply.matches(&request));
        }
        for bytes in [
            br#"{"type":"sign","leg":0,"connection_id":"c","request_id":1,"packet":"AQ=="}"#.as_slice(),
            br#"{"type":"sign","leg":2,"connection_id":"c","request_id":1,"packet":"AQ=="}"#,
            br#"{"type":"sign","connection_id":"c","request_id":1,"packet":"secret"}"#,
            br#"{"type":"sign","leg":1,"connection_id":"c","request_id":1,"packet":"secret","extra":true}"#,
        ] {
            let error = SshRouteRequest::from_frame(bytes, &receipt).err().unwrap();
            assert!(!format!("{error:?}").contains("secret"));
        }
        assert!(
            SshRouteRequest::from_frame(&vec![b' '; SSH_AUTH_FRAME_MAX + 1], &receipt).is_err()
        );
    }
}
