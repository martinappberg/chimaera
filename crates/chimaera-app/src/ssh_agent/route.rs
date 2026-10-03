//! Ordered native configuration and original per-leg cryptographic selections.
//! Every leg retains native host trust and its immutable initial mode; missing
//! keys can select interactive only before any authentication has been attempted.
use super::{
    selection::{self, Selection, SelectionFailure},
    unix::UnixAgent,
    Failure, GrantVerifier, SshAuthReply, SshAuthRequest,
};
use chimaera_link::{
    SshAuthDestination, SshRoute, SshRouteAuthLeg, SshRouteGrantRequest, SshRouteMode,
    SshRouteReply, SshRouteRequest,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{process::Command, time::Instant};

type Result<T> = std::result::Result<T, SelectionFailure>;

#[derive(Clone)]
struct Jump {
    host: String,
    user: Option<String>,
    port: Option<u16>,
    original: String,
}
fn atom(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:@[]".contains(&byte))
}
impl Jump {
    fn parse(value: &str) -> Result<Self> {
        if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_whitespace) {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        let (host, user, port) = if value.starts_with("ssh://") {
            let uri =
                url::Url::parse(value).map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
            if uri.password().is_some()
                || !matches!(uri.path(), "" | "/")
                || uri.query().is_some()
                || uri.fragment().is_some()
            {
                return Err(SelectionFailure::UnsupportedConfiguration);
            }
            (
                uri.host_str()
                    .ok_or(SelectionFailure::UnsupportedConfiguration)?
                    .to_string(),
                (!uri.username().is_empty()).then(|| uri.username().to_string()),
                uri.port(),
            )
        } else {
            let (user, rest) = match value.rsplit_once('@') {
                Some((user, host)) => (Some(user.to_string()), host),
                None => (None, value),
            };
            let (host, port) = if rest.starts_with('[') {
                let end = rest
                    .find(']')
                    .ok_or(SelectionFailure::UnsupportedConfiguration)?;
                let suffix = &rest[end + 1..];
                let port = if suffix.is_empty() {
                    None
                } else {
                    Some(
                        suffix
                            .strip_prefix(':')
                            .ok_or(SelectionFailure::UnsupportedConfiguration)?
                            .parse()
                            .map_err(|_| SelectionFailure::UnsupportedConfiguration)?,
                    )
                };
                (rest[..=end].to_string(), port)
            } else if let Some((host, port)) = rest.split_once(':') {
                (
                    host.to_string(),
                    Some(
                        port.parse()
                            .map_err(|_| SelectionFailure::UnsupportedConfiguration)?,
                    ),
                )
            } else {
                (rest.to_string(), None)
            };
            (host, user, port)
        };
        if !atom(&host)
            || user
                .as_deref()
                .is_some_and(|value| !atom(value) || value.contains('@'))
            || port == Some(0)
        {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        Ok(Self {
            host,
            user,
            port,
            original: value.into(),
        })
    }
}

struct Effective {
    text: String,
    destination: SshAuthDestination,
}
struct Resolver {
    calls: usize,
    #[cfg(test)]
    config: Option<PathBuf>,
}
impl Resolver {
    async fn config(&mut self, jump: &Jump, prefix: Option<&str>) -> Result<String> {
        if self.calls >= 4 {
            return Err(SelectionFailure::UnsupportedConfiguration);
        }
        self.calls += 1;
        let mut command = Command::new("/usr/bin/ssh");
        command.arg("-G");
        #[cfg(test)]
        if let Some(config) = &self.config {
            command.arg("-F").arg(config);
        }
        if let Some(prefix) = prefix {
            command.args(["-J", prefix]);
        }
        if let Some(user) = &jump.user {
            command.args(["-l", user]);
        }
        if let Some(port) = jump.port {
            command.arg("-p").arg(port.to_string());
        }
        command.args(["--", &jump.host]);
        // As in the original native selector, trusted local Match exec may run.
        // ProxyCommand is never executed by -G and never crosses the wire.
        selection::bounded_output(command, 0, None).await
    }
    async fn walk(
        &mut self,
        jump: Jump,
        prefix: Option<String>,
        output: &mut Vec<Effective>,
    ) -> Result<()> {
        let text = self.config(&jump, prefix.as_deref()).await?;
        let (destination, route) = selection::route_settings(&text)?;
        if route != "none" {
            let mut jumps = route
                .split(',')
                // A fourth leg proves refusal without allocating a config-sized list.
                .take(4)
                .map(Jump::parse)
                .collect::<Result<Vec<_>>>()?;
            if jumps.is_empty() || jumps.len() > 3 {
                return Err(SelectionFailure::UnsupportedConfiguration);
            }
            let last = jumps
                .pop()
                .ok_or(SelectionFailure::UnsupportedConfiguration)?;
            // OpenSSH implements a multi-jump route by giving the final jump
            // an explicit -J prefix. That overrides its own nested setting;
            // only the first hop without a prefix can add its local route.
            let prefix = (!jumps.is_empty()).then(|| {
                jumps
                    .iter()
                    .map(|jump| jump.original.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            });
            Box::pin(self.walk(last, prefix, output)).await?;
        }
        output.push(Effective { text, destination });
        Ok(())
    }
}

pub(crate) struct RouteSelection {
    pub(crate) request: SshRouteGrantRequest,
    legs: Vec<Option<Selection>>,
}
impl RouteSelection {
    pub(crate) fn verifier(
        self,
        deadline: Instant,
    ) -> std::result::Result<RouteVerifier<UnixAgent>, Failure> {
        let mut legs = Vec::new();
        for leg in self.legs {
            legs.push(match leg {
                Some(leg) => Some(leg.verifier(deadline)?.1),
                None => None,
            });
        }
        RouteVerifier::new_selected(self.request, legs, deadline)
    }
}
pub(crate) async fn resolve(alias: &str, boot: String) -> Result<RouteSelection> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(SelectionFailure::Unavailable)?;
    let agent = std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from);
    tokio::time::timeout(Duration::from_secs(30), async {
        let effective = effective(alias).await?;
        select(effective, &home, agent, boot).await
    })
    .await
    .map_err(|_| SelectionFailure::Unavailable)?
}
async fn effective(alias: &str) -> Result<Vec<Effective>> {
    let alias = chimaera_remote::hosts::normalize_alias(alias)
        .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
    let mut resolver = Resolver {
        calls: 0,
        #[cfg(test)]
        config: None,
    };
    let mut effective = Vec::new();
    resolver
        .walk(Jump::parse(&alias)?, None, &mut effective)
        .await?;
    Ok(effective)
}
/// Inert save resolves tuples only; it never enumerates keys, asks for trust,
/// creates a grant, or starts a network authentication attempt.
pub(crate) async fn registration(alias: &str) -> Result<(SshAuthDestination, SshRoute)> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let effective = effective(alias).await?;
        let destination = effective
            .last()
            .ok_or(SelectionFailure::UnsupportedConfiguration)?
            .destination
            .clone();
        let route = SshRoute {
            version: 1,
            jumps: effective
                .iter()
                .take(effective.len() - 1)
                .map(|leg| leg.destination.clone())
                .collect(),
        };
        route
            .validate(&destination)
            .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
        Ok((destination, route))
    })
    .await
    .map_err(|_| SelectionFailure::Unavailable)?
}
async fn select(
    effective: Vec<Effective>,
    home: &Path,
    agent: Option<PathBuf>,
    boot: String,
) -> Result<RouteSelection> {
    let destination = effective
        .last()
        .ok_or(SelectionFailure::UnsupportedConfiguration)?
        .destination
        .clone();
    let route = SshRoute {
        version: 1,
        jumps: effective
            .iter()
            .take(effective.len() - 1)
            .map(|leg| leg.destination.clone())
            .collect(),
    };
    route
        .validate(&destination)
        .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
    let mut legs = Vec::new();
    let mut requests = Vec::new();
    for leg in effective {
        match selection::from_routed_config(&leg.text, home, agent.clone(), boot.clone()).await {
            Ok(selected) => {
                requests.push(SshRouteAuthLeg {
                    policy: None,
                    destination: selected.request.destination.clone(),
                    mode: SshRouteMode::Key,
                    host_keys: selected.request.host_keys.clone(),
                    user_keys: selected.request.user_keys.clone(),
                });
                legs.push(Some(selected));
            }
            Err(SelectionFailure::NoKeys) => {
                requests.push(selection::routed_interactive(&leg.text, home).await?);
                legs.push(None);
            }
            Err(SelectionFailure::AgentUnavailable)
                if selection::no_agent_selected(&leg.text, &agent)? =>
            {
                requests.push(selection::routed_interactive(&leg.text, home).await?);
                legs.push(None);
            }
            Err(error) => return Err(error),
        }
    }
    let request = SshRouteGrantRequest {
        version: 1,
        keeper_boot: boot,
        destination,
        route,
        legs: requests,
    };
    request
        .validate()
        .map_err(|_| SelectionFailure::UnsupportedConfiguration)?;
    Ok(RouteSelection { request, legs })
}

pub(crate) struct RouteVerifier<A: super::LocalAgent> {
    pub(crate) request: SshRouteGrantRequest,
    pub(crate) deadline: Instant,
    legs: Vec<Option<GrantVerifier<A>>>,
    last_request: u64,
    connections: HashMap<String, u8>,
    sessions: HashSet<Vec<u8>>,
    prompt_proof: Option<super::lifecycle::RoutePromptProof>,
}
impl<A: super::LocalAgent> RouteVerifier<A> {
    fn new(
        request: SshRouteGrantRequest,
        legs: Vec<GrantVerifier<A>>,
        deadline: Instant,
    ) -> std::result::Result<Self, Failure> {
        Self::new_selected(request, legs.into_iter().map(Some).collect(), deadline)
    }
    fn new_selected(
        request: SshRouteGrantRequest,
        legs: Vec<Option<GrantVerifier<A>>>,
        deadline: Instant,
    ) -> std::result::Result<Self, Failure> {
        request.validate().map_err(|_| Failure::InvalidRequest)?;
        if request.legs.len() != legs.len()
            || request
                .legs
                .iter()
                .zip(&legs)
                .any(|(leg, verifier)| (leg.mode == SshRouteMode::Key) != verifier.is_some())
        {
            return Err(Failure::Unsupported);
        }
        Ok(Self {
            request,
            deadline,
            legs,
            last_request: 0,
            connections: HashMap::new(),
            sessions: HashSet::new(),
            prompt_proof: None,
        })
    }
    pub(crate) fn attach_prompts(&mut self, proof: super::lifecycle::RoutePromptProof) {
        self.prompt_proof = Some(proof);
    }
    pub(crate) async fn handle(
        &mut self,
        request: SshRouteRequest,
    ) -> std::result::Result<Option<SshRouteReply>, Failure> {
        if Instant::now() >= self.deadline {
            self.legs.clear();
            return Err(Failure::Expired);
        }
        let (leg, ordinary) = match request {
            SshRouteRequest::SessionBind {
                leg,
                connection_id,
                request_id,
                packet,
            } => {
                if request_id <= self.last_request {
                    return Err(Failure::InvalidRequest);
                }
                self.last_request = request_id;
                if self.connections.len() >= chimaera_link::SSH_AUTH_CONNECTIONS_MAX
                    || self.connections.contains_key(&connection_id)
                {
                    return Err(Failure::InvalidBinding);
                }
                let verifier = self
                    .legs
                    .get(usize::from(leg))
                    .and_then(Option::as_ref)
                    .ok_or(Failure::InvalidRequest)?;
                let bytes =
                    chimaera_link::decode_packet(&packet, chimaera_link::SSH_AUTH_PACKET_MAX)
                        .map_err(|_| Failure::InvalidRequest)?;
                let (_, session) = verifier.policy.bind(&bytes)?;
                if !self.sessions.insert(session) {
                    return Err(Failure::InvalidBinding);
                }
                self.connections.insert(connection_id.clone(), leg);
                (
                    leg,
                    SshAuthRequest::SessionBind {
                        connection_id,
                        request_id,
                        packet,
                    },
                )
            }
            SshRouteRequest::Sign {
                leg,
                connection_id,
                request_id,
                packet,
            } => {
                if request_id <= self.last_request
                    || self.connections.get(&connection_id) != Some(&leg)
                {
                    return Err(Failure::InvalidRequest);
                }
                self.last_request = request_id;
                (
                    leg,
                    SshAuthRequest::Sign {
                        connection_id,
                        request_id,
                        packet,
                    },
                )
            }
            SshRouteRequest::ConnectionClosed { leg, connection_id } => {
                if self.connections.get(&connection_id) != Some(&leg) {
                    return Err(Failure::InvalidRequest);
                }
                (leg, SshAuthRequest::ConnectionClosed { connection_id })
            }
        };
        let verifier = self
            .legs
            .get_mut(usize::from(leg))
            .and_then(Option::as_mut)
            .ok_or(Failure::InvalidRequest)?;
        verifier.deadline = self.deadline;
        let reply = verifier.handle(ordinary).await?;
        if matches!(reply, Some(SshAuthReply::Signature { .. })) {
            if let Some(proof) = &self.prompt_proof {
                proof.signed(leg);
            }
        }
        Ok(reply.map(|reply| match reply {
            SshAuthReply::Bound {
                connection_id,
                request_id,
            } => SshRouteReply::Bound {
                leg,
                connection_id,
                request_id,
            },
            SshAuthReply::Signature {
                connection_id,
                request_id,
                packet,
            } => SshRouteReply::Signature {
                leg,
                connection_id,
                request_id,
                packet,
            },
            SshAuthReply::Failure {
                connection_id,
                request_id,
                error,
            } => SshRouteReply::Failure {
                leg,
                connection_id,
                request_id,
                error,
            },
        }))
    }
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod tests;
