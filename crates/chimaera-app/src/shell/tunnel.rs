//! The shell owns transport choice. chimaera-remote stays SSH-only so the
//! optional TLS/link dependency can never enter the daemon's static build.

use chimaera_link::HostKind;

/// Whether a host is one the app opens, connects to and updates as a host:
/// SSH hosts and the user's other computers. The account's cloud never is: no
/// window shows its own page, and the service keeps its daemon current.
pub(super) fn app_host(kind: &HostKind) -> bool {
    *kind != HostKind::Worker
}

/// Whether a connected host's daemon reads as outdated, the signal behind the
/// app's offer to update it. Never for the cloud, which the service updates.
pub(super) fn offers_daemon_update(kind: &HostKind, build: &str) -> bool {
    app_host(kind) && !chimaera_core::builds_match(chimaera_core::BUILD_ID, Some(build))
}

pub(crate) struct Endpoint {
    pub token: String,
    pub build: Option<String>,
}

pub(crate) struct Tunnel {
    pub local_port: u16,
    pub manifest: Endpoint,
    pub outdated: bool,
    pub remote_build: Option<String>,
    pub live_sessions: Option<usize>,
    transport: Transport,
}

enum Transport {
    Ssh(Box<chimaera_remote::Tunnel>),
    Link {
        host_id: String,
        tunnel: Box<dyn crate::account::DelegatedTransport>,
    },
}

impl From<chimaera_remote::Tunnel> for Tunnel {
    fn from(tunnel: chimaera_remote::Tunnel) -> Self {
        Self {
            local_port: tunnel.local_port,
            manifest: Endpoint {
                token: tunnel.manifest.token.clone(),
                build: tunnel.manifest.build.clone(),
            },
            outdated: tunnel.outdated,
            remote_build: tunnel.remote_build.clone(),
            live_sessions: tunnel.live_sessions,
            transport: Transport::Ssh(Box::new(tunnel)),
        }
    }
}

impl Tunnel {
    pub fn link(
        host: &chimaera_link::Host,
        local_port: u16,
        tunnel: Box<dyn crate::account::DelegatedTransport>,
    ) -> anyhow::Result<Self> {
        let daemon = host
            .daemon
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("host daemon is unavailable"))?;
        Ok(Self {
            local_port,
            manifest: Endpoint {
                token: daemon.token.clone(),
                build: Some(daemon.build.clone()),
            },
            outdated: offers_daemon_update(&host.kind, &daemon.build),
            remote_build: Some(daemon.build.clone()),
            live_sessions: Some(daemon.sessions),
            transport: Transport::Link {
                host_id: host.id.clone(),
                tunnel,
            },
        })
    }

    pub fn link_id(&self) -> Option<&str> {
        match &self.transport {
            Transport::Link { host_id, .. } => Some(host_id),
            _ => None,
        }
    }

    pub fn node(&self) -> Option<&str> {
        match &self.transport {
            Transport::Ssh(tunnel) => tunnel.route.node(),
            _ => None,
        }
    }

    pub fn update_link(&mut self, host: &chimaera_link::Host) {
        if self.link_id() != Some(host.id.as_str()) {
            return;
        }
        if let Some(daemon) = &host.daemon {
            self.manifest.token = daemon.token.clone();
            self.manifest.build = Some(daemon.build.clone());
            self.remote_build = Some(daemon.build.clone());
            self.live_sessions = Some(daemon.sessions);
            self.outdated = offers_daemon_update(&host.kind, &daemon.build);
        }
    }

    pub async fn close(self) {
        match self.transport {
            Transport::Ssh(tunnel) => tunnel.close().await,
            Transport::Link { tunnel, .. } => tunnel.close(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_cloud_is_never_offered_a_daemon_update() {
        let older = "0000000.1";
        // An SSH host (direct or kept through Pro) and another computer keep
        // the outdated note and its update offer.
        assert!(offers_daemon_update(&HostKind::Ssh, older));
        assert!(offers_daemon_update(&HostKind::Device, older));
        // The service updates the cloud's daemon; the app never offers to.
        assert!(!offers_daemon_update(&HostKind::Worker, older));
        // A daemon on this app's build is never outdated.
        for kind in [HostKind::Ssh, HostKind::Device, HostKind::Worker] {
            assert!(!offers_daemon_update(&kind, chimaera_core::BUILD_ID));
        }
    }

    #[test]
    fn the_cloud_is_never_one_of_the_apps_hosts() {
        assert!(app_host(&HostKind::Ssh));
        assert!(app_host(&HostKind::Device));
        assert!(!app_host(&HostKind::Worker));
    }
}
