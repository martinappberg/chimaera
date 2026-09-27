//! The shell owns transport choice. chimaera-remote stays SSH-only so the
//! optional TLS/link dependency can never enter the daemon's static build.

pub(super) struct Endpoint {
    pub token: String,
    pub build: Option<String>,
}

pub(super) struct Tunnel {
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
        tunnel: chimaera_link::LinkTunnel,
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
        tunnel: chimaera_link::LinkTunnel,
    ) -> anyhow::Result<Self> {
        let daemon = host
            .daemon
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("host daemon is unavailable"))?;
        Ok(Self {
            local_port: tunnel.local_port,
            manifest: Endpoint {
                token: daemon.token.clone(),
                build: Some(daemon.build.clone()),
            },
            outdated: !chimaera_core::builds_match(chimaera_core::BUILD_ID, Some(&daemon.build)),
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
            self.outdated =
                !chimaera_core::builds_match(chimaera_core::BUILD_ID, Some(&daemon.build));
        }
    }

    pub async fn close(self) {
        match self.transport {
            Transport::Ssh(tunnel) => tunnel.close().await,
            Transport::Link { tunnel, .. } => tunnel.close(),
        }
    }
}
