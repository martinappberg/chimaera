use super::*;
use crate::{SshRoute, SshRouteGrant, SshRouteGrantRequest, SshRouteHost};

impl Client {
    async fn ssh_route_capabilities(&self) -> Result<crate::SshAuthCapabilities> {
        let caps = self.ssh_auth_capabilities().await?;
        if !caps.route_supported() {
            return Err(crate::ServiceUnsupported.into());
        }
        Ok(caps)
    }

    /// Save only after explicit route support, then require a positive exact
    /// resolved-route acknowledgment. No route resolution or SSH starts here.
    pub async fn register_ssh_route_host(
        &self,
        alias: &str,
        ssh: crate::SshTarget,
        route: SshRoute,
        policy: Option<crate::ClusterPolicy>,
    ) -> Result<SshRouteHost> {
        self.register_ssh_route_host_inner(alias, ssh, route, policy, false)
            .await
    }

    /// Policy-aware native Connect must negotiate before its inert save too.
    pub async fn register_ssh_policy_route_host(
        &self,
        alias: &str,
        ssh: crate::SshTarget,
        route: SshRoute,
        policy: Option<crate::ClusterPolicy>,
    ) -> Result<SshRouteHost> {
        self.register_ssh_route_host_inner(alias, ssh, route, policy, true)
            .await
    }

    async fn register_ssh_route_host_inner(
        &self,
        alias: &str,
        ssh: crate::SshTarget,
        route: SshRoute,
        policy: Option<crate::ClusterPolicy>,
        require_policy: bool,
    ) -> Result<SshRouteHost> {
        let destination = crate::SshAuthDestination {
            hostname: ssh.hostname.clone(),
            user: ssh.user.clone().unwrap_or_default(),
            port: ssh.port,
        };
        route.validate(&destination)?;
        anyhow::ensure!(
            !alias.is_empty() && alias.len() <= 255 && !alias.chars().any(char::is_control),
            "invalid SSH route alias"
        );
        let mut body = serde_json::to_value(crate::AddHost {
            alias: alias.into(),
            ssh: Some(ssh.clone()),
            register_only: true,
            cluster_policy: policy,
        })?;
        body["ssh_route"] = serde_json::to_value(&route)?;
        anyhow::ensure!(
            serde_json::to_vec(&body)?.len() <= crate::SSH_AUTH_FRAME_MAX,
            "SSH route registration too large"
        );
        let caps = self.ssh_route_capabilities().await?;
        if require_policy && !caps.route_policy_supported() {
            return Err(crate::ServiceUnsupported.into());
        }
        if policy.is_some() {
            self.cluster_capabilities().await?;
        }
        let value: SshRouteHost = json_response(
            self.keeper_request(Method::POST, &["v1", "hosts"], Some(body))
                .await?,
        )
        .await?;
        anyhow::ensure!(
            crate::placement::valid_id(&value.host.id)
                && value.host.alias == alias
                && value.host.kind == crate::HostKind::Ssh
                && value.host.status == crate::HostStatus::Offline
                && value.host.daemon.is_none()
                && value.ssh == ssh
                && value.ssh_route == route,
            "SSH route registration acknowledgment mismatch"
        );
        Ok(value)
    }

    pub async fn create_ssh_route_grant(
        &self,
        host: &str,
        request: &SshRouteGrantRequest,
    ) -> Result<SshRouteGrant> {
        anyhow::ensure!(crate::placement::valid_id(host), "invalid SSH route host");
        request.validate()?;
        let caps = self.ssh_route_capabilities().await?;
        if request.legs.iter().any(|leg| leg.policy.is_some()) && !caps.route_policy_supported() {
            return Err(crate::ServiceUnsupported.into());
        }
        anyhow::ensure!(
            caps.keeper_boot == request.keeper_boot,
            "SSH route boot changed"
        );
        let response = self
            .keeper_request_raw(
                Method::POST,
                &["v1", "hosts", host, "ssh", "auth", "route-grants"],
                Some(serde_json::to_value(request)?),
            )
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(crate::ServiceUnsupported.into());
        }
        let grant: SshRouteGrant = json_response(response).await?;
        grant.validate()?;
        anyhow::ensure!(
            grant.matches(request),
            "SSH route grant acknowledgment mismatch"
        );
        Ok(grant)
    }

    /// Caller-owned, server-first control socket; no reconnect or packet replay.
    pub async fn ssh_route_socket(
        &self,
        host: &str,
        grant: &SshRouteGrant,
        expected_boot: &str,
    ) -> Result<crate::Socket> {
        anyhow::ensure!(crate::placement::valid_id(host), "invalid SSH route host");
        grant.validate()?;
        let caps = self.ssh_route_capabilities().await?;
        if grant.policies.is_some() && !caps.route_policy_supported() {
            return Err(crate::ServiceUnsupported.into());
        }
        anyhow::ensure!(caps.keeper_boot == expected_boot, "SSH route boot changed");
        let mut socket = self
            .open_socket(
                &[
                    "v1",
                    "hosts",
                    host,
                    "ssh",
                    "auth",
                    "route-grants",
                    &grant.grant_id,
                    "ws",
                ],
                true,
            )
            .await?;
        let ready = tokio::time::timeout(Duration::from_secs(30), socket.next())
            .await
            .map_err(|_| anyhow!("SSH route readiness expired"))?;
        match ready {
            Some(Ok(Message::Text(value))) => {
                crate::SshRouteHello::from_frame(value.as_bytes(), grant, expected_boot)?;
            }
            _ => bail!("SSH route readiness failed"),
        }
        Ok(socket)
    }

    pub async fn delete_ssh_route_grant(&self, host: &str, grant: &SshRouteGrant) -> Result<()> {
        anyhow::ensure!(crate::placement::valid_id(host), "invalid SSH route host");
        grant.validate()?;
        self.keeper_request(
            Method::DELETE,
            &[
                "v1",
                "hosts",
                host,
                "ssh",
                "auth",
                "route-grants",
                &grant.grant_id,
            ],
            None,
        )
        .await
        .map_err(|error| {
            if error.is::<crate::AuthorizationRevoked>() {
                error
            } else {
                anyhow!("SSH route grant deletion failed")
            }
        })?;
        Ok(())
    }

    pub async fn reconnect_host_with_ssh_route(
        &self,
        host: &str,
        grant: &SshRouteGrant,
        expected_boot: &str,
    ) -> Result<()> {
        anyhow::ensure!(crate::placement::valid_id(host), "invalid SSH route host");
        grant.validate()?;
        let caps = self.ssh_route_capabilities().await?;
        if grant.policies.is_some() && !caps.route_policy_supported() {
            return Err(crate::ServiceUnsupported.into());
        }
        anyhow::ensure!(caps.keeper_boot == expected_boot, "SSH route boot changed");
        let response = self
            .keeper_request_raw_grant(
                Method::POST,
                &["v1", "hosts", host, "reconnect"],
                None,
                Some(SshGrantHeader::Route(&grant.grant_id)),
            )
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "SSH route reconnect rejected ({})",
            response.status().as_u16()
        );
        Ok(())
    }
}
