use crate::{
    bridge,
    handoff::*,
    protocol::*,
    transport::{endpoint, path, Socket},
    websocket_config,
};
use anyhow::{anyhow, bail, Context, Result};
use futures::{SinkExt, StreamExt};
use reqwest::Method;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch, Mutex, RwLock, Semaphore},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
    Connector,
};
use url::Url;

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}
struct Inner {
    account: Url,
    keeper: RwLock<Option<Url>>,
    http: reqwest::Client,
    tls: Arc<rustls::ClientConfig>,
    tokens: Arc<Mutex<Option<Tokens>>>,
    token_updates: watch::Sender<Option<Tokens>>,
    /// One device's concurrent streams (forward tunnels and reverse serve
    /// together), matching the keeper's per-device quota.
    streams: Arc<Semaphore>,
}
impl Client {
    /// Does not contact the endpoint. No background work starts before the
    /// caller explicitly signs in or requests an operation.
    pub fn new(base: &str, tokens: Option<Tokens>) -> Result<Self> {
        let account = endpoint(base)?;
        let (token_updates, _) = watch::channel(tokens.clone());
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            inner: Arc::new(Inner {
                account,
                keeper: RwLock::new(None),
                http: reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .tls_backend_preconfigured(tls.clone())
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(Duration::from_secs(30))
                    .build()?,
                tls: Arc::new(tls),
                tokens: Arc::new(Mutex::new(tokens)),
                token_updates,
                streams: Arc::new(Semaphore::new(MAX_STREAMS)),
            }),
        })
    }
    /// Persist each replacement token pair in the OS keychain, including refresh
    /// rotations. Consumers must never write these values to application state.
    pub fn token_updates(&self) -> watch::Receiver<Option<Tokens>> {
        self.inner.token_updates.subscribe()
    }
    pub async fn tokens(&self) -> Option<Tokens> {
        self.inner.tokens.lock().await.clone()
    }
    pub async fn clear_tokens(&self) {
        *self.inner.tokens.lock().await = None;
        self.inner.token_updates.send_replace(None);
        *self.inner.keeper.write().await = None;
    }
    async fn install_tokens(&self, tokens: Tokens) -> Result<Tokens> {
        if tokens.token_type != "Bearer"
            || tokens.access_token.is_empty()
            || tokens.refresh_token.is_empty()
        {
            bail!("invalid token response");
        }
        *self.inner.tokens.lock().await = Some(tokens.clone());
        self.inner.token_updates.send_replace(Some(tokens.clone()));
        Ok(tokens)
    }
    pub async fn exchange_code(&self, request: TokenRequest) -> Result<Tokens> {
        crate::oauth::validate_redirect(&request.redirect_uri)?;
        let response = self
            .inner
            .http
            .post(path(&self.inner.account, &["v1", "oauth", "token"]))
            .json(&request)
            .send()
            .await?;
        self.install_tokens(json_response(response).await?).await
    }
    async fn access_token(&self) -> Result<String> {
        self.inner
            .tokens
            .lock()
            .await
            .as_ref()
            .map(|t| t.access_token.clone())
            .context("sign in required")
    }
    async fn refresh_if_current(&self, rejected: &str) -> Result<()> {
        // Take ownership before spawning: concurrent 401s wait here rather than
        // launching duplicate rotations. A canceled account read must not lose
        // a one-use refresh token already consumed by the server. Only this
        // HTTP-timeout-bounded rotation survives its caller; clear_tokens waits
        // for it and then clears the result, so sign-out cannot be resurrected.
        let mut guard = self.inner.tokens.clone().lock_owned().await;
        let old = guard.as_ref().context("sign in required")?;
        if old.access_token != rejected {
            return Ok(());
        }
        let refresh_token = old.refresh_token.clone();
        let client = self.clone();
        tokio::spawn(async move {
            // The account rotates on receipt and treats a later reuse of the
            // old token as theft (it revokes the device). Only a connection
            // that was never established provably left the token unused, so
            // that is the one case retried with the same token. A timeout, a
            // reset after sending or a server error may follow a committed
            // rotation: report it as transient and keep the session instead
            // of presenting the token again at once.
            let mut retried = false;
            let response = loop {
                let sent = client
                    .inner
                    .http
                    .post(path(&client.inner.account, &["v1", "oauth", "refresh"]))
                    .timeout(REFRESH_TIMEOUT)
                    .json(&RefreshRequest {
                        refresh_token: refresh_token.clone(),
                    })
                    .send()
                    .await;
                match sent {
                    Err(error) if refresh_never_sent(&error) && !retried => {
                        retried = true;
                        tokio::time::sleep(REFRESH_RETRY_DELAY).await;
                    }
                    sent => break sent.context("credential refresh unavailable")?,
                }
            };
            if refresh_revoked(response.status().as_u16()) {
                *guard = None;
                client.inner.token_updates.send_replace(None);
                *client.inner.keeper.write().await = None;
                return Err(crate::AuthorizationRevoked.into());
            }
            let status = response.status();
            if !status.is_success() {
                bail!("credential refresh unavailable ({})", status.as_u16());
            }
            let tokens: Tokens = json_response_body(response).await?;
            if tokens.token_type != "Bearer"
                || tokens.access_token.is_empty()
                || tokens.refresh_token.is_empty()
            {
                bail!("invalid refresh response");
            }
            *guard = Some(tokens.clone());
            client.inner.token_updates.send_replace(Some(tokens));
            Ok(())
        })
        .await
        .context("credential refresh task stopped")?
    }
    /// A keeper can refuse a valid access token for its own reasons (account
    /// outage, stale revocation cache). Rotating the one-use refresh token then
    /// is pure risk, so ask the account itself. This read has no side effects.
    async fn account_rejects(&self, token: &str) -> Result<bool> {
        let response = self
            .inner
            .http
            .get(path(&self.inner.account, &["v1", "devices"]))
            .bearer_auth(token)
            .timeout(ACCOUNT_CHECK_TIMEOUT)
            .send()
            .await?;
        match response.status().as_u16() {
            401 => Ok(true),
            status if (200..300).contains(&status) => Ok(false),
            status => bail!("account unavailable ({status})"),
        }
    }
    async fn request_raw(
        &self,
        method: Method,
        url: Url,
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response> {
        for attempt in 0..2 {
            let token = self.access_token().await?;
            let mut request = self
                .inner
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(&token);
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = request.send().await?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                self.refresh_if_current(&token).await?;
            } else {
                return Ok(response);
            }
        }
        bail!("sign in required")
    }
    async fn request(
        &self,
        method: Method,
        url: Url,
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response> {
        let response = self.request_raw(method, url, body).await?;
        if !response.status().is_success() {
            bail!("account request rejected ({})", response.status().as_u16());
        }
        Ok(response)
    }
    async fn baton_request(
        &self,
        method: Method,
        workspace: &str,
        action: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> Result<Baton> {
        let mut parts = vec!["v1", "baton", workspace];
        if let Some(action) = action {
            parts.push(action);
        }
        let response = self
            .request_raw(method, path(&self.inner.account, &parts), body)
            .await?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            // Preserve CAS state for callers; a generic transport error must
            // never be interpreted as losing the baton.
            let conflict: BatonConflict = json_response_body(response).await?;
            return Err(conflict.into());
        }
        json_response(response).await
    }
    pub async fn delegate_daemon(&self) -> Result<Delegation> {
        json_response(
            self.request(
                Method::POST,
                path(&self.inner.account, &["v1", "delegations"]),
                Some(serde_json::json!({})),
            )
            .await?,
        )
        .await
    }
    /// Renewal uses only the scoped credential, never the device refresh token.
    pub async fn renew_delegation(&self, token: &str) -> Result<Delegation> {
        json_response(
            self.inner
                .http
                .post(path(&self.inner.account, &["v1", "delegations", "renew"]))
                .bearer_auth(token)
                .json(&serde_json::json!({}))
                .send()
                .await?,
        )
        .await
    }
    /// `ServiceUnsupported` when the service lacks the route or shares no
    /// capability with this client; never a fallback to legacy execution.
    pub async fn execution_capabilities(&self) -> Result<crate::ExecutionCapabilities> {
        let response = self
            .request_raw(
                Method::GET,
                path(&self.inner.account, &["v2", "capabilities"]),
                None,
            )
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(crate::ServiceUnsupported.into());
        }
        let value: crate::ExecutionCapabilities = json_response(response).await?;
        if !value.supported() {
            return Err(crate::ServiceUnsupported.into());
        }
        Ok(value)
    }
    pub async fn installation_recovery(
        &self,
        identity: &crate::InstallationIdentity,
        workspace: &str,
        holder: &str,
        expected_epoch: u64,
    ) -> Result<crate::ExecutionRecoveryGrant> {
        identity.validate()?;
        anyhow::ensure!(
            crate::placement::valid_id(workspace)
                && crate::placement::valid_id(holder)
                && expected_epoch > 0,
            "invalid recovery identity"
        );
        let value: crate::ExecutionRecoveryGrant = json_response(self.request(Method::POST,
            path(&self.inner.account, &["v2", "installations", &identity.installation_id, "recovery"]),
            Some(serde_json::json!({"installation_proof":identity.installation_proof,"workspace_id":workspace,"expected_epoch":expected_epoch}))).await?).await?;
        anyhow::ensure!(
            value.workspace_id == workspace
                && value.holder_id == holder
                && value.epoch == expected_epoch
                && value.scope.len() == 2
                && value.scope.iter().any(|s| s == "mirror")
                && value.scope.iter().any(|s| s == "release")
                && !value.access_token.is_empty()
                && value.access_token.len() <= 8192
                && !value.access_token.chars().any(char::is_control)
                && !value.expires_at.is_empty()
                && value.expires_at.len() <= 64,
            "recovery grant mismatch"
        );
        Ok(value)
    }
    /// Read the current owner without acquiring, waking or transferring work.
    /// A project the account has never recorded is simply unowned.
    pub async fn workspace_placement(&self, workspace: &str) -> Result<crate::WorkspacePlacement> {
        anyhow::ensure!(
            crate::placement::valid_id(workspace),
            "invalid workspace identity"
        );
        let response = self
            .request_raw(
                Method::GET,
                path(
                    &self.inner.account,
                    &["v2", "workspaces", workspace, "placement"],
                ),
                None,
            )
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            let error: Option<ApiError> = json_response_body(response).await.ok();
            if error.is_some_and(|error| error.error == "workspace_not_found") {
                return Ok(crate::WorkspacePlacement::unowned(workspace));
            }
            return Err(crate::ServiceUnsupported.into());
        }
        let value: crate::WorkspacePlacement = json_response(response).await?;
        value.validate(workspace)?;
        Ok(value)
    }
    pub async fn bind_installation(
        &self,
        identity: &crate::InstallationIdentity,
    ) -> Result<crate::InstallationBinding> {
        self.bind_installation_named(identity, None).await
    }
    pub async fn bind_installation_named(
        &self,
        identity: &crate::InstallationIdentity,
        display_name: Option<&str>,
    ) -> Result<crate::InstallationBinding> {
        identity.validate()?;
        let mut body = serde_json::to_value(identity)?;
        if let Some(name) = display_name {
            anyhow::ensure!(
                !name.trim().is_empty() && name.len() <= 120 && !name.chars().any(char::is_control),
                "invalid installation display name"
            );
            body["display_name"] = name.into();
        }
        let response = self
            .request_raw(
                Method::POST,
                path(&self.inner.account, &["v2", "installations", "bind"]),
                Some(body),
            )
            .await?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            let error: serde_json::Value = json_response_body(response).await?;
            if error["error"] == "clean_release_required" {
                return Err(crate::CleanReleaseRequired.into());
            }
            bail!("installation binding rejected");
        }
        let value: crate::InstallationBinding = json_response(response).await?;
        anyhow::ensure!(
            value.installation_id == identity.installation_id
                && crate::placement::valid_id(&value.device_id),
            "installation binding identity mismatch"
        );
        Ok(value)
    }
    pub async fn set_workspace_home(
        &self,
        workspace: &str,
        installation: &str,
        expected_policy_revision: u64,
    ) -> Result<crate::WorkspaceHome> {
        anyhow::ensure!(
            crate::placement::valid_id(workspace) && crate::placement::valid_id(installation),
            "invalid workspace home identity"
        );
        let value: crate::WorkspaceHome = json_response(self.request(Method::PUT, path(&self.inner.account, &["v2", "workspaces", workspace, "home"]), Some(serde_json::json!({"installation_id":installation,"expected_policy_revision":expected_policy_revision}))).await?).await?;
        anyhow::ensure!(
            value.workspace_id == workspace
                && value.preferred_installation_id == installation
                && value.policy_revision >= expected_policy_revision,
            "workspace home identity mismatch"
        );
        Ok(value)
    }
    pub async fn baton(&self, workspace: &str) -> Result<Baton> {
        self.baton_request(Method::GET, workspace, None, None).await
    }
    pub async fn acquire_baton(&self, workspace: &str, request: &AcquireBaton) -> Result<Baton> {
        self.baton_request(
            Method::POST,
            workspace,
            Some("acquire"),
            Some(serde_json::to_value(request)?),
        )
        .await
    }
    pub async fn renew_baton(&self, workspace: &str, request: &HeldBaton) -> Result<Baton> {
        self.baton_request(
            Method::POST,
            workspace,
            Some("renew"),
            Some(serde_json::to_value(request)?),
        )
        .await
    }
    pub async fn release_baton(&self, workspace: &str, request: &HeldBaton) -> Result<Baton> {
        self.baton_request(
            Method::POST,
            workspace,
            Some("release"),
            Some(serde_json::to_value(request)?),
        )
        .await
    }
    pub async fn wake_worker(&self) -> Result<WorkerWake> {
        json_response(
            self.request(
                Method::POST,
                path(&self.inner.account, &["v1", "worker", "wake"]),
                Some(serde_json::json!({})),
            )
            .await?,
        )
        .await
    }
    pub async fn worker_status(&self) -> Result<WorkerStatus> {
        json_response(
            self.request(
                Method::GET,
                path(&self.inner.account, &["v1", "worker", "status"]),
                None,
            )
            .await?,
        )
        .await
    }
    pub async fn billing_checkout(
        &self,
        plan: Plan,
        interval: BillingInterval,
    ) -> Result<BillingSession> {
        self.billing_checkout_request(plan, interval, None).await
    }
    pub async fn billing_checkout_with_callback(
        &self,
        plan: Plan,
        interval: BillingInterval,
        callback: &DesktopBillingCallback,
    ) -> Result<BillingSession> {
        callback.validate()?;
        self.billing_checkout_request(plan, interval, Some(callback))
            .await
    }
    async fn billing_checkout_request(
        &self,
        plan: Plan,
        interval: BillingInterval,
        callback: Option<&DesktopBillingCallback>,
    ) -> Result<BillingSession> {
        if !matches!(plan, Plan::Pro | Plan::Max) {
            bail!("choose Pro or Max");
        }
        let mut body =
            serde_json::json!({"plan": plan, "interval": interval, "return_to": "desktop"});
        if let Some(callback) = callback {
            body["desktop_callback"] = serde_json::to_value(callback)?;
        }
        let response = self
            .request_raw(
                Method::POST,
                path(&self.inner.account, &["v1", "billing", "checkout"]),
                Some(body),
            )
            .await?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            return Err(crate::AlreadySubscribed.into());
        }
        json_response(response).await
    }
    pub async fn billing_portal(&self) -> Result<BillingSession> {
        self.billing_portal_request(None, None).await
    }
    pub async fn billing_portal_with_callback(
        &self,
        callback: &DesktopBillingCallback,
    ) -> Result<BillingSession> {
        callback.validate()?;
        self.billing_portal_request(Some(callback), None).await
    }
    pub async fn billing_portal_review_with_callback(
        &self,
        target: &BillingPortalTarget,
        callback: &DesktopBillingCallback,
    ) -> Result<BillingSession> {
        target.validate()?;
        callback.validate()?;
        self.billing_portal_request(Some(callback), Some(target))
            .await
    }
    async fn billing_portal_request(
        &self,
        callback: Option<&DesktopBillingCallback>,
        target: Option<&BillingPortalTarget>,
    ) -> Result<BillingSession> {
        let mut body = serde_json::json!({"return_to": "desktop"});
        if let Some(callback) = callback {
            body["desktop_callback"] = serde_json::to_value(callback)?;
        }
        if let Some(target) = target {
            body["target"] = serde_json::to_value(target)?;
        }
        json_response(
            self.request(
                Method::POST,
                path(&self.inner.account, &["v1", "billing", "portal"]),
                Some(body),
            )
            .await?,
        )
        .await
    }
    pub async fn set_handoff_policy(&self, workspace: &str, policy: &HandoffPolicy) -> Result<()> {
        self.request(
            Method::PUT,
            path(&self.inner.account, &["v1", "baton", workspace, "policy"]),
            Some(serde_json::to_value(policy)?),
        )
        .await?;
        Ok(())
    }
    pub async fn disable_handoff_policy(&self, workspace: &str) -> Result<()> {
        self.request(
            Method::DELETE,
            path(&self.inner.account, &["v1", "baton", workspace, "policy"]),
            None,
        )
        .await?;
        Ok(())
    }
    /// Re-enable grants after an explicit account-device privacy choice.
    pub async fn enable_mirror(&self, workspace: &str) -> Result<()> {
        self.request(
            Method::POST,
            path(
                &self.inner.account,
                &["v1", "baton", workspace, "enable-mirror"],
            ),
            Some(serde_json::json!({})),
        )
        .await?;
        Ok(())
    }
    pub async fn mirror_credentials(&self, request: &MirrorRequest) -> Result<MirrorCredentials> {
        let credentials: MirrorCredentials = json_response(
            self.request(
                Method::POST,
                path(&self.inner.account, &["v1", "mirror", "credentials"]),
                Some(serde_json::to_value(request)?),
            )
            .await?,
        )
        .await?;
        for raw in [&credentials.repository_url, &credentials.working_tree_url] {
            let url = Url::parse(raw)?;
            if !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                bail!("invalid mirror URL");
            }
            let loopback = url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
            if url.scheme() != "https" && (!loopback || self.inner.account.scheme() == "https") {
                bail!("mirror TLS downgrade");
            }
        }
        if credentials.workspace_id != request.workspace_id
            || credentials.password.is_empty()
            || credentials.read_only != request.epoch.is_none()
        {
            bail!("invalid mirror credentials");
        }
        Ok(credentials)
    }
    pub async fn me(&self) -> Result<Account> {
        let account: Account = json_response(
            self.request(Method::GET, path(&self.inner.account, &["v1", "me"]), None)
                .await?,
        )
        .await?;
        if account.protocol != PROTOCOL_VERSION {
            return Err(crate::ServiceUnsupported.into());
        }
        let keeper = if account.keeper_url.is_empty() {
            None
        } else {
            Some(endpoint(&account.keeper_url)?)
        };
        // A TLS account must never downgrade its bearer to a cleartext keeper.
        if self.inner.account.scheme() == "https"
            && keeper
                .as_ref()
                .is_some_and(|keeper| keeper.scheme() != "https")
        {
            bail!("keeper TLS downgrade");
        }
        *self.inner.keeper.write().await = keeper;
        Ok(account)
    }
    /// The public plan catalog, `GET /v1/plans` (`{"plans": [...] | null}`). It
    /// carries no credentials (no bearer is sent even when this client holds
    /// tokens), so it answers before anyone has signed in, and it never touches
    /// the token or keeper state. `Ok(None)` means there are no offers to show:
    /// the service has no catalog yet, or an older account has no such route
    /// (404). Any other failure is an error the caller may retry later; it is
    /// never a sign-in problem.
    pub async fn plans(&self) -> Result<Option<Vec<PlanPrice>>> {
        self.plans_within(PLANS_TIMEOUT).await
    }
    async fn plans_within(&self, timeout: Duration) -> Result<Option<Vec<PlanPrice>>> {
        let response = self
            .inner
            .http
            .get(path(&self.inner.account, &["v1", "plans"]))
            .timeout(timeout)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(json_response::<PlanCatalog>(response).await?.plans)
    }
    async fn keeper(&self) -> Result<Url> {
        if let Some(keeper) = self.inner.keeper.read().await.clone() {
            return Ok(keeper);
        }
        self.me().await?;
        self.inner
            .keeper
            .read()
            .await
            .clone()
            .context("Your connection is being prepared; no keeper is assigned yet")
    }
    /// A keeper HTTP route. A keeper can refuse a token the account still
    /// accepts (a stale revocation cache) and answers 503 `account_unavailable`
    /// while the account is down; neither rotates the one-use refresh token or
    /// signs out. Only an account that itself answers 401 does.
    async fn keeper_request_raw(
        &self,
        method: Method,
        segments: &[&str],
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response> {
        self.keeper_request_raw_grant(method, segments, body, None)
            .await
    }
    async fn keeper_request_raw_grant(
        &self,
        method: Method,
        segments: &[&str],
        body: Option<serde_json::Value>,
        grant: Option<&str>,
    ) -> Result<reqwest::Response> {
        let url = path(&self.keeper().await?, segments);
        for attempt in 0..2 {
            let token = self.access_token().await?;
            let mut request = self
                .inner
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(&token);
            if let Some(grant) = grant {
                // This helper is private; only the exact reconnect method below
                // selects a grant. No mutable global/header inheritance exists.
                request = request.header(crate::SSH_AUTH_GRANT_HEADER, grant);
            }
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = request.send().await?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED
                && attempt == 0
                && self.account_rejects(&token).await?
            {
                self.refresh_if_current(&token).await?;
                continue;
            }
            return Ok(response);
        }
        bail!("sign in required")
    }
    async fn keeper_request(
        &self,
        method: Method,
        segments: &[&str],
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response> {
        let response = self.keeper_request_raw(method, segments, body).await?;
        if !response.status().is_success() {
            bail!("keeper request rejected ({})", response.status().as_u16());
        }
        Ok(response)
    }
    pub async fn ssh_auth_capabilities(&self) -> Result<crate::SshAuthCapabilities> {
        let response = self
            .keeper_request_raw(Method::GET, &["v1", "ssh", "auth", "capabilities"], None)
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(crate::ServiceUnsupported.into());
        }
        let value: crate::SshAuthCapabilities = json_response(response).await?;
        if !value.supported() {
            return Err(crate::ServiceUnsupported.into());
        }
        Ok(value)
    }
    /// Native key-only Connect first saves an inert host. Never send an unknown
    /// additive field to an old keeper and hope it did not start authentication.
    pub async fn register_ssh_auth_host(
        &self,
        alias: &str,
        ssh: crate::SshTarget,
        policy: Option<crate::ClusterPolicy>,
    ) -> Result<crate::Host> {
        let destination = crate::SshAuthDestination {
            hostname: ssh.hostname.clone(),
            user: ssh.user.clone().unwrap_or_default(),
            port: ssh.port,
        };
        destination.validate()?;
        let caps = self.ssh_auth_capabilities().await?;
        if !caps.registration_supported() {
            return Err(crate::ServiceUnsupported.into());
        }
        if policy.is_some() {
            self.cluster_capabilities().await?;
        }
        let value: crate::Host = json_response(
            self.keeper_request(
                Method::POST,
                &["v1", "hosts"],
                Some(serde_json::to_value(crate::AddHost {
                    alias: alias.into(),
                    ssh: Some(ssh),
                    cluster_policy: policy,
                    register_only: true,
                })?),
            )
            .await?,
        )
        .await?;
        anyhow::ensure!(
            crate::placement::valid_id(&value.id)
                && value.alias == alias
                && value.kind == crate::HostKind::Ssh
                && value.status == crate::HostStatus::Offline
                && value.daemon.is_none(),
            "SSH registration was not inert"
        );
        Ok(value)
    }
    pub async fn create_ssh_auth_grant(
        &self,
        host: &str,
        request: &crate::SshAuthGrantRequest,
    ) -> Result<crate::SshAuthGrant> {
        anyhow::ensure!(
            crate::placement::valid_id(host),
            "invalid SSH authentication host"
        );
        request.validate()?;
        let caps = self.ssh_auth_capabilities().await?;
        anyhow::ensure!(
            caps.keeper_boot == request.keeper_boot,
            "SSH authentication boot changed"
        );
        let value: crate::SshAuthGrant = json_response(
            self.keeper_request(
                Method::POST,
                &["v1", "hosts", host, "ssh", "auth", "grants"],
                Some(serde_json::to_value(request)?),
            )
            .await?,
        )
        .await?;
        value.validate()?;
        Ok(value)
    }
    /// The socket is caller-owned and never reconnects/replays authentication.
    pub async fn ssh_auth_socket(
        &self,
        host: &str,
        grant: &crate::SshAuthGrant,
        expected_boot: &str,
    ) -> Result<crate::Socket> {
        anyhow::ensure!(
            crate::placement::valid_id(host),
            "invalid SSH authentication host"
        );
        grant.validate()?;
        let caps = self.ssh_auth_capabilities().await?;
        anyhow::ensure!(
            caps.keeper_boot == expected_boot,
            "SSH authentication boot changed"
        );
        let mut socket = self
            .open_socket(
                &[
                    "v1",
                    "hosts",
                    host,
                    "ssh",
                    "auth",
                    "grants",
                    &grant.grant_id,
                    "ws",
                ],
                true,
            )
            .await?;
        let ready = tokio::time::timeout(
            Duration::from_secs(30.min(grant.expires_in.into())),
            socket.next(),
        )
        .await
        .map_err(|_| anyhow!("SSH authentication readiness expired"))?;
        match ready {
            Some(Ok(Message::Text(value))) => {
                crate::SshAuthHello::from_frame(value.as_bytes(), grant, expected_boot)?;
            }
            _ => bail!("SSH authentication readiness failed"),
        }
        Ok(socket)
    }
    pub async fn delete_ssh_auth_grant(
        &self,
        host: &str,
        grant: &crate::SshAuthGrant,
    ) -> Result<()> {
        anyhow::ensure!(
            crate::placement::valid_id(host),
            "invalid SSH authentication host"
        );
        grant.validate()?;
        self.keeper_request(
            Method::DELETE,
            &[
                "v1",
                "hosts",
                host,
                "ssh",
                "auth",
                "grants",
                &grant.grant_id,
            ],
            None,
        )
        .await
        .map_err(|error| {
            if error.is::<crate::AuthorizationRevoked>() {
                error
            } else {
                anyhow!("SSH authentication grant deletion failed")
            }
        })?;
        Ok(())
    }
    pub async fn reconnect_host_with_ssh_auth(
        &self,
        host: &str,
        grant: &crate::SshAuthGrant,
    ) -> Result<()> {
        anyhow::ensure!(
            crate::placement::valid_id(host),
            "invalid SSH authentication host"
        );
        grant.validate()?;
        self.ssh_auth_capabilities().await?;
        let response = self
            .keeper_request_raw_grant(
                Method::POST,
                &["v1", "hosts", host, "reconnect"],
                None,
                Some(&grant.grant_id),
            )
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "SSH authentication reconnect rejected ({})",
            response.status().as_u16()
        );
        Ok(())
    }
    pub async fn cluster_capabilities(&self) -> Result<crate::ClusterCapabilities> {
        let response = self
            .keeper_request_raw(Method::GET, &["v1", "cluster", "capabilities"], None)
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(crate::ServiceUnsupported.into());
        }
        let capabilities: crate::ClusterCapabilities = cluster_response(response).await?;
        if !capabilities.control_supported() {
            return Err(crate::ServiceUnsupported.into());
        }
        Ok(capabilities)
    }
    pub async fn cluster_operation(
        &self,
        host_id: &str,
        operation: &crate::ClusterOperation,
    ) -> Result<crate::ClusterReply> {
        operation.validate()?;
        let capabilities = self.cluster_capabilities().await?;
        if matches!(operation, crate::ClusterOperation::StartJob { .. })
            && !capabilities.jobs_supported()
        {
            return Err(crate::ServiceUnsupported.into());
        }
        let body =
            serde_json::to_value(operation).map_err(|_| anyhow!("invalid cluster request"))?;
        let reply: crate::ClusterReply = cluster_response(
            self.keeper_request_raw(
                Method::POST,
                &["v1", "hosts", host_id, "cluster", "operations"],
                Some(body),
            )
            .await?,
        )
        .await?;
        reply.validate()?;
        if !operation.accepts(&reply) {
            bail!("cluster reply does not match operation");
        }
        if let crate::ClusterReply::Host { host } = &reply {
            if host.id != host_id {
                bail!("cluster host reply does not match operation");
            }
        }
        Ok(reply)
    }
    /// Retain the original operation across a lost reply; this never submits it.
    pub async fn cluster_operation_state(
        &self,
        host_id: &str,
        operation: &crate::ClusterOperation,
    ) -> Result<crate::ClusterOperationState> {
        operation.validate()?;
        let id = operation
            .operation_id()
            .context("cluster read has no operation id")?;
        self.cluster_capabilities().await?;
        let state: crate::ClusterOperationState = cluster_response(
            self.keeper_request_raw(
                Method::GET,
                &["v1", "hosts", host_id, "cluster", "operations", id],
                None,
            )
            .await?,
        )
        .await?;
        state.validate_for(operation)?;
        if let crate::ClusterOperationState::Completed { reply } = &state {
            if let crate::ClusterReply::Host { host } = reply.as_ref() {
                if host.id != host_id {
                    bail!("cluster host reply does not match operation");
                }
            }
        }
        Ok(state)
    }
    pub async fn cluster_tcp(
        &self,
        host_id: &str,
        job_id: &str,
        workspace_id: Option<&str>,
    ) -> Result<Socket> {
        anyhow::ensure!(
            chimaera_core::cluster::valid_job_id(job_id),
            "invalid cluster job id"
        );
        if let Some(id) = workspace_id {
            anyhow::ensure!(
                chimaera_core::cluster::valid_workspace_id(id),
                "invalid cluster workspace id"
            );
        }
        if !self.cluster_capabilities().await?.jobs_supported() {
            return Err(crate::ServiceUnsupported.into());
        }
        if let Some(id) = workspace_id {
            self.open_socket(
                &[
                    "v1",
                    "hosts",
                    host_id,
                    "jobs",
                    job_id,
                    "workspaces",
                    id,
                    "tcp",
                ],
                false,
            )
            .await
        } else {
            self.open_socket(&["v1", "hosts", host_id, "jobs", job_id, "tcp"], false)
                .await
        }
    }
    /// Rows of a host kind this client does not know are dropped.
    pub async fn hosts(&self) -> Result<Vec<Host>> {
        let hosts: Vec<Host> = json_response(
            self.keeper_request(Method::GET, &["v1", "hosts"], None)
                .await?,
        )
        .await?;
        Ok(hosts
            .into_iter()
            .filter(|host| host.kind != HostKind::Unknown)
            .collect())
    }
    pub async fn add_host(&self, alias: &str) -> Result<Host> {
        self.add_host_with_ssh(alias, None).await
    }
    pub async fn add_host_with_ssh(&self, alias: &str, ssh: Option<SshTarget>) -> Result<Host> {
        self.add_host_with_policy(alias, ssh, None).await
    }
    pub async fn add_host_with_policy(
        &self,
        alias: &str,
        ssh: Option<SshTarget>,
        cluster_policy: Option<crate::ClusterPolicy>,
    ) -> Result<Host> {
        if cluster_policy.is_some() {
            self.cluster_capabilities().await?;
        }
        json_response(
            self.keeper_request(
                Method::POST,
                &["v1", "hosts"],
                Some(serde_json::to_value(AddHost {
                    alias: alias.into(),
                    register_only: false,
                    ssh,
                    cluster_policy,
                })?),
            )
            .await?,
        )
        .await
    }
    pub async fn delete_host(&self, id: &str) -> Result<()> {
        self.keeper_request(Method::DELETE, &["v1", "hosts", id], None)
            .await?;
        Ok(())
    }
    pub async fn reconnect_host(&self, id: &str) -> Result<()> {
        self.keeper_request(Method::POST, &["v1", "hosts", id, "reconnect"], None)
            .await?;
        Ok(())
    }
    pub async fn devices(&self) -> Result<Vec<Device>> {
        json_response(
            self.request(
                Method::GET,
                path(&self.inner.account, &["v1", "devices"]),
                None,
            )
            .await?,
        )
        .await
    }
    pub async fn revoke_device(&self, id: &str) -> Result<()> {
        self.request(
            Method::DELETE,
            path(&self.inner.account, &["v1", "devices", id]),
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn sign_out_everywhere(&self) -> Result<()> {
        self.request(
            Method::POST,
            path(&self.inner.account, &["v1", "sign-out-everywhere"]),
            None,
        )
        .await?;
        self.clear_tokens().await;
        Ok(())
    }
    pub async fn open_socket(&self, segments: &[&str], control: bool) -> Result<Socket> {
        let mut url = path(&self.keeper().await?, segments);
        url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
            .map_err(|_| anyhow!("invalid websocket URL"))?;
        for attempt in 0..2 {
            let token = self.access_token().await?;
            let mut request = url.as_str().into_client_request()?;
            request.headers_mut().insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
            match tokio::time::timeout(
                Duration::from_secs(15),
                connect_async_tls_with_config(
                    request,
                    Some(websocket_config(control)),
                    false,
                    Some(Connector::Rustls(self.inner.tls.clone())),
                ),
            )
            .await?
            {
                Ok((socket, _)) => return Ok(socket),
                Err(tokio_tungstenite::tungstenite::Error::Http(response))
                    if response.status().as_u16() == 401 && attempt == 0 =>
                {
                    if !self.account_rejects(&token).await? {
                        bail!("websocket upgrade rejected (401)");
                    }
                    self.refresh_if_current(&token).await?
                }
                // Handshake error bodies may include echoed credentials: report
                // only the status, never the response headers or body.
                Err(tokio_tungstenite::tungstenite::Error::Http(response)) => bail!(
                    "websocket upgrade rejected ({})",
                    response.status().as_u16()
                ),
                Err(_) => bail!("websocket connection failed"),
            }
        }
        bail!("sign in required")
    }
    pub async fn tcp(&self, host_id: &str) -> Result<Socket> {
        self.open_socket(&["v1", "hosts", host_id, "tcp"], false)
            .await
    }
    pub fn events(&self) -> EventConnection {
        let (out, events) = mpsc::channel(64);
        let (commands, mut incoming) = mpsc::channel(16);
        let client = self.clone();
        let task = tokio::spawn(async move {
            let mut attempts = 0;
            loop {
                if out.is_closed() {
                    break;
                }
                let result = async {
                    let socket = client.open_socket(&["v1", "events"], true).await?;
                    let (mut tx, mut rx) = socket.split();
                    let mut prompts = ConnectionPrompts::default();
                    let mut ping = tokio::time::interval_at(Instant::now() + Duration::from_secs(20), Duration::from_secs(20));
                    let mut last_pong = Instant::now();
                    let connected = Instant::now();
                    loop {
                        for id in prompts.expire() {
                            out.try_send(Ok(Event::PromptClosed { id })).map_err(|_| anyhow!("events consumer slow"))?;
                        }
                        tokio::select! {
                            _ = out.closed() => return Ok(()),
                            command = incoming.recv() => match command {
                                Some(command) => { if let Some(command) = prompts.answer(command) { send_json(&mut tx, &command).await?; } },
                                None => return Ok(()),
                            },
                            message = rx.next() => match message {
                                Some(Ok(Message::Text(text))) => {
                                    let event = match serde_json::from_str::<Event>(&text) {
                                        Ok(Event::Unknown) | Err(_) => continue,
                                        Ok(Event::Host { host }) if host.kind == HostKind::Unknown => continue,
                                        Ok(event) => event,
                                    };
                                    let Some(event) = prompts.receive(event)? else { continue; };
                                    match tokio::time::timeout(Duration::from_secs(10), out.send(Ok(event))).await {
                                        Ok(Ok(())) => {}
                                        Ok(Err(_)) => return Ok(()),
                                        // Never abandon a slow consumer: drop this
                                        // connection instead; the reconnect snapshot
                                        // replaces whatever it missed.
                                        Err(_) => bail!("events consumer slow"),
                                    }
                                }
                                Some(Ok(Message::Ping(data))) => { tokio::time::timeout(Duration::from_secs(10), tx.send(Message::Pong(data))).await??; }
                                Some(Ok(Message::Pong(_))) => { last_pong = Instant::now(); }
                                Some(Ok(Message::Close(_))) | None => bail!("events disconnected"),
                                Some(Ok(_)) => bail!("invalid events frame"),
                                Some(Err(_)) => bail!("events read failed"),
                            },
                            _ = ping.tick() => {
                                if last_pong.elapsed() >= Duration::from_secs(60) { bail!("events pong timeout"); }
                                tokio::time::timeout(Duration::from_secs(10), tx.send(Message::Ping(Vec::new().into()))).await??;
                            }
                        }
                        if connected.elapsed() >= Duration::from_secs(20) { attempts = 0; }
                    }
                }.await;
                if out.is_closed() {
                    break;
                }
                // Only a closed consumer ends this task. A full queue skips the
                // report; the reconnect snapshot tells the consumer the rest.
                if let Err(error) = result {
                    let _ = out.try_send(Err(error.to_string()));
                }
                // Password answers belong to their connection, never to a later
                // prompt with a reused identifier after a reconnect.
                while incoming.try_recv().is_ok() {}
                tokio::select! { _ = out.closed() => break, _ = tokio::time::sleep(backoff(attempts)) => {} }
                attempts = attempts.saturating_add(1);
            }
        });
        EventConnection {
            events,
            commands,
            task,
        }
    }
}

/// Caller-visible prompt identifiers belong only to the socket that received
/// them. Native callers may hold a cloned command sender through reconnect;
/// its old answer must not match a keeper's reused prompt identifier.
#[derive(Default)]
struct ConnectionPrompts {
    by_wire: HashMap<String, LocalPrompt>,
}
struct LocalPrompt {
    id: String,
    expires: Instant,
    answered: bool,
}
impl ConnectionPrompts {
    fn receive(&mut self, event: Event) -> Result<Option<Event>> {
        Ok(Some(match event {
            Event::Prompt {
                id,
                host_id,
                prompt,
                echo,
            } => {
                if !self.by_wire.contains_key(&id) && self.by_wire.len() >= 64 {
                    bail!("events prompt limit");
                }
                let local = self.by_wire.entry(id).or_insert_with(|| LocalPrompt {
                    id: base64::Engine::encode(
                        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                        rand::random::<[u8; 32]>(),
                    ),
                    expires: Instant::now() + Duration::from_secs(180),
                    answered: false,
                });
                Event::Prompt {
                    id: local.id.clone(),
                    host_id,
                    prompt,
                    echo,
                }
            }
            Event::PromptClosed { id } => {
                let Some(local) = self.by_wire.remove(&id) else {
                    return Ok(None);
                };
                Event::PromptClosed { id: local.id }
            }
            other => other,
        }))
    }
    fn answer(&mut self, command: EventCommand) -> Option<EventCommand> {
        let EventCommand::Answer { id, value } = command;
        let (wire, local) = self.by_wire.iter_mut().find(|(_, local)| local.id == id)?;
        if local.answered || local.expires <= Instant::now() {
            return None;
        }
        local.answered = true;
        Some(EventCommand::Answer {
            id: wire.clone(),
            value,
        })
    }
    fn expire(&mut self) -> Vec<String> {
        let mut expired = Vec::new();
        self.by_wire.retain(|_, local| {
            if local.expires > Instant::now() {
                true
            } else {
                expired.push(local.id.clone());
                false
            }
        });
        expired
    }
}

pub struct EventConnection {
    pub events: mpsc::Receiver<Result<Event, String>>,
    /// Answer IDs come from this connection's received Prompt events, never a
    /// keeper API response; queued stale IDs are discarded on reconnect.
    pub commands: mpsc::Sender<EventCommand>,
    task: JoinHandle<()>,
}
impl EventConnection {
    pub async fn answer(&self, id: String, value: Option<String>) -> Result<()> {
        self.commands
            .send(EventCommand::Answer { id, value })
            .await
            .context("events closed")
    }
    pub fn close(&self) {
        self.task.abort();
    }
}
impl Drop for EventConnection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A stable loopback listener; per-request socket failures never change its port.
pub struct LinkTunnel {
    pub local_port: u16,
    task: JoinHandle<()>,
}
impl LinkTunnel {
    pub async fn bind(client: Client, host_id: String) -> Result<Self> {
        Self::bind_resource(client, host_id, None).await
    }
    pub async fn bind_cluster(
        client: Client,
        host_id: String,
        job_id: String,
        workspace_id: Option<String>,
    ) -> Result<Self> {
        anyhow::ensure!(
            chimaera_core::cluster::valid_job_id(&job_id),
            "invalid cluster job id"
        );
        if let Some(id) = workspace_id.as_deref() {
            anyhow::ensure!(
                chimaera_core::cluster::valid_workspace_id(id),
                "invalid cluster workspace id"
            );
        }
        if !client.cluster_capabilities().await?.jobs_supported() {
            return Err(crate::ServiceUnsupported.into());
        }
        Self::bind_resource(client, host_id, Some((job_id, workspace_id))).await
    }
    async fn bind_resource(
        client: Client,
        host_id: String,
        job: Option<(String, Option<String>)>,
    ) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let local_port = listener.local_addr()?.port();
        let task = tokio::spawn(async move {
            let limits = client.inner.streams.clone();
            let mut streams = JoinSet::new();
            let mut failures = 0_u32;
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        // Descriptor exhaustion (EMFILE) or a connection reset
                        // before accept must not close the stable port or its
                        // other streams; back off so it cannot spin.
                        let Ok((tcp, _)) = result else {
                            failures = failures.saturating_add(1);
                            tokio::time::sleep(Duration::from_millis(25 << failures.min(6))).await;
                            continue;
                        };
                        failures = 0;
                        let Ok(permit) = limits.clone().try_acquire_owned() else { drop(tcp); continue; };
                        let client = client.clone(); let host_id = host_id.clone(); let job=job.clone();
                        streams.spawn(async move {
                            let _permit = permit;
                            let socket=match job {
                                Some((job_id,workspace_id))=>client.cluster_tcp(&host_id,&job_id,workspace_id.as_deref()).await,
                                None=>client.tcp(&host_id).await,
                            };
                            if let Ok(socket) = socket { let _ = bridge(tcp, socket).await; }
                        });
                    }
                    _ = streams.join_next(), if !streams.is_empty() => {}
                }
            }
        });
        Ok(Self { local_port, task })
    }
    pub fn close(&self) {
        self.task.abort();
    }
}
impl Drop for LinkTunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct Serve {
    task: JoinHandle<()>,
}
impl Serve {
    pub fn start(client: Client, local_port: u16, alias: String, daemon: Daemon) -> Self {
        let task = tokio::spawn(async move {
            let mut attempts = 0;
            loop {
                let started = Instant::now();
                let _ = serve_connection(&client, local_port, &alias, &daemon).await;
                if started.elapsed() >= Duration::from_secs(20) {
                    attempts = 0;
                }
                tokio::time::sleep(backoff(attempts)).await;
                attempts = attempts.saturating_add(1);
            }
        });
        Self { task }
    }
    pub fn close(&self) {
        self.task.abort();
    }
}
impl Drop for Serve {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve_connection(client: &Client, port: u16, alias: &str, daemon: &Daemon) -> Result<()> {
    let mut socket = client.open_socket(&["v1", "serve"], true).await?;
    send_json(
        &mut socket,
        &ServeCommand::Register {
            alias: alias.to_string(),
            daemon: daemon.clone(),
        },
    )
    .await?;
    let mut streams = JoinSet::new();
    let mut active = std::collections::HashMap::new();
    let limits = client.inner.streams.clone();
    let mut ping = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(20),
        Duration::from_secs(20),
    );
    let mut last_pong = Instant::now();
    loop {
        tokio::select! {
            message = socket.next() => match message {
                // Anything this client cannot act on concerns at most one
                // stream: an unknown or malformed message, a duplicate id or an
                // open beyond the per-device quota is ignored (the keeper
                // expires an unanswered open), never the whole connection.
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServeEvent>(&text).unwrap_or(ServeEvent::Unknown) {
                    ServeEvent::Open { stream_id } => {
                        if active.contains_key(&stream_id) { continue; }
                        let Ok(permit) = limits.clone().try_acquire_owned() else { continue; };
                        let client = client.clone(); let key = stream_id.clone();
                        let abort = streams.spawn(async move {
                            let _permit = permit;
                            let result = async {
                                let socket = client.open_socket(&["v1", "serve", &stream_id], false).await?;
                                let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))).await??;
                                bridge(tcp, socket).await
                            }.await;
                            (stream_id, result)
                        });
                        active.insert(key, abort);
                    }
                    ServeEvent::Close { stream_id } => { if let Some(task) = active.remove(&stream_id) { task.abort(); } }
                    ServeEvent::Registered { .. } | ServeEvent::Unknown => {}
                },
                Some(Ok(Message::Ping(data))) => { tokio::time::timeout(Duration::from_secs(10), socket.send(Message::Pong(data))).await??; }
                Some(Ok(Message::Pong(_))) => { last_pong = Instant::now(); }
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                _ => bail!("serve disconnected"),
            },
            Some(result) = streams.join_next(), if !streams.is_empty() => {
                if let Ok((id, _)) = result { active.remove(&id); }
            }
            _ = ping.tick() => {
                if last_pong.elapsed() >= Duration::from_secs(60) { bail!("serve pong timeout"); }
                tokio::time::timeout(Duration::from_secs(10), socket.send(Message::Ping(Vec::new().into()))).await??;
            }
        }
    }
}
/// Longer than an ordinary account request so a slow but successful rotation
/// is still received; an abandoned one may already have consumed the token.
/// The token mutex is held for at most one refused connect (the client's
/// connect timeout), one pause and this.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(45);
const REFRESH_RETRY_DELAY: Duration = Duration::from_millis(500);
/// Bounds the side-effect-free account check behind a keeper `401`.
const ACCOUNT_CHECK_TIMEOUT: Duration = Duration::from_secs(20);
/// The public catalog is presentation only: a slow answer is just no prices.
const PLANS_TIMEOUT: Duration = Duration::from_secs(10);
/// Whether a refresh request provably never reached the account: the
/// connection itself (DNS, TCP or TLS) was never established. Anything later,
/// including a timeout, may follow a committed rotation.
fn refresh_never_sent(error: &reqwest::Error) -> bool {
    error.is_connect()
}
/// The account answers revoked, expired and replayed refresh tokens with
/// 400 `invalid_grant`; 401/403 mean the same. A missing route (404, e.g. a
/// deploy in progress), a request timeout and rate limiting say nothing about
/// the token, and neither does any 5xx.
fn refresh_revoked(status: u16) -> bool {
    (400..500).contains(&status) && !matches!(status, 404 | 408 | 429)
}
fn backoff(attempt: u32) -> Duration {
    let ceiling = (500_u64.saturating_mul(1 << attempt.min(5))).min(10_000);
    Duration::from_millis(ceiling / 2 + rand::random_range(0..=ceiling / 2))
}
async fn cluster_response<T: DeserializeOwned>(mut response: reqwest::Response) -> Result<T> {
    let status = response.status();
    let limit = if status.is_success() {
        crate::CLUSTER_REPLY_MAX
    } else {
        4096
    };
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            bail!("cluster response exceeds limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let code = serde_json::from_slice::<ClusterErrorBody>(&bytes)
            .ok()
            .map(|body| body.error)
            .unwrap_or(crate::ClusterErrorCode::Unknown);
        return Err(crate::ClusterRequestError {
            status: status.as_u16(),
            code,
        }
        .into());
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid cluster JSON response"))
}
#[derive(serde::Deserialize)]
struct ClusterErrorBody {
    error: crate::ClusterErrorCode,
}
async fn json_response<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    if !response.status().is_success() {
        bail!("request rejected ({})", response.status().as_u16());
    }
    json_response_body(response).await
}
async fn json_response_body<T: DeserializeOwned>(mut response: reqwest::Response) -> Result<T> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            bail!("REST response exceeds 1 MiB");
        }
        bytes.extend_from_slice(&chunk);
    }
    // Typed deserialization errors can include token values from the body.
    serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid JSON response"))
}
async fn send_json<S, T>(socket: &mut S, value: &T) -> Result<()>
where
    S: futures::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
    T: Serialize,
{
    let json = serde_json::to_string(value)?;
    if json.len() > MAX_CONTROL_FRAME {
        bail!("control frame too large");
    }
    tokio::time::timeout(
        Duration::from_secs(10),
        socket.send(Message::Text(json.into())),
    )
    .await?
    .map_err(|_| anyhow!("websocket send failed"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn refresh_error(endpoint: &str) -> reqwest::Error {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::new(endpoint, None).unwrap();
        client
            .inner
            .http
            .post(path(&client.inner.account, &["v1", "oauth", "refresh"]))
            .timeout(Duration::from_millis(400))
            .json(&RefreshRequest {
                refresh_token: "fixture".into(),
            })
            .send()
            .await
            .unwrap_err()
    }

    /// The same-token retry rests on this: only a refused connection proves
    /// the account never saw the one-use token.
    #[tokio::test]
    async fn only_a_connection_never_established_counts_as_unsent() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let refused = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert!(refresh_never_sent(&refresh_error(&refused).await));

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let dropped = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await;
        });
        assert!(!refresh_never_sent(&refresh_error(&dropped).await));
        server.await.unwrap();

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let silent = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(socket);
        });
        let error = refresh_error(&silent).await;
        assert!(error.is_timeout(), "{error}");
        assert!(!refresh_never_sent(&error));
        server.abort();
    }

    /// One canned answer on a loopback port; yields the request head it saw.
    async fn answering(
        status: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::AsyncWriteExt;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let read = socket.read(&mut request).await.unwrap();
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
            String::from_utf8_lossy(&request[..read]).into_owned()
        });
        (endpoint, server)
    }

    #[tokio::test]
    async fn malformed_token_responses_do_not_expose_or_install_secrets() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        const BODY: &str = r#"{"access_token":"synthetic-access","refresh_token":"synthetic-refresh","token_type":"Bearer","expires_in":"synthetic-private-expiry"}"#;
        for refresh in [false, true] {
            let (endpoint, server) = answering("200 OK", BODY).await;
            let original = Tokens {
                access_token: "before-access".into(),
                refresh_token: "before-refresh".into(),
                token_type: "Bearer".into(),
                expires_in: 900,
            };
            let client = Client::new(&endpoint, Some(original)).unwrap();
            let updates = client.token_updates();
            let error = if refresh {
                client
                    .refresh_if_current("before-access")
                    .await
                    .unwrap_err()
            } else {
                client
                    .exchange_code(TokenRequest {
                        grant_type: "authorization_code".into(),
                        code: "fixture".into(),
                        redirect_uri: "http://127.0.0.1:43210/callback".into(),
                        code_verifier: "fixture-verifier".into(),
                        device_name: "fixture".into(),
                    })
                    .await
                    .unwrap_err()
            };
            let request = server.await.unwrap();
            assert!(request.starts_with(if refresh {
                "POST /v1/oauth/refresh "
            } else {
                "POST /v1/oauth/token "
            }));
            for rendered in std::iter::once(format!("{error:?} {error:#}"))
                .chain(error.chain().map(|cause| format!("{cause:?} {cause}")))
            {
                assert!(
                    !rendered.contains("synthetic-"),
                    "response leaked through error: {rendered}"
                );
            }
            assert_eq!(client.tokens().await.unwrap().access_token, "before-access");
            assert_eq!(
                client.tokens().await.unwrap().refresh_token,
                "before-refresh"
            );
            assert!(!updates.has_changed().unwrap());
        }
    }

    /// The catalog answers before anyone signs in: no credentials go out (not
    /// even a held token), and it reads exactly like `/v1/me`'s `plans`.
    #[tokio::test]
    async fn the_public_catalog_needs_no_credentials_and_reads_like_the_account_list() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (endpoint, server) = answering(
            "200 OK",
            r#"{"plans":[{"plan":"pro","interval":"month","amount_cents":111,"currency":"USD"},
                {"plan":"team","interval":"month","amount_cents":1,"currency":"usd"},
                {"plan":"max","interval":"year","amount_cents":"lots","currency":"usd"}]}"#,
        )
        .await;
        let client = Client::new(
            &endpoint,
            Some(Tokens {
                access_token: "held-access".into(),
                refresh_token: "held-refresh".into(),
                token_type: "Bearer".into(),
                expires_in: 900,
            }),
        )
        .unwrap();
        let plans = client.plans().await.unwrap();
        assert_eq!(
            plans,
            Some(vec![PlanPrice {
                plan: Plan::Pro,
                interval: BillingInterval::Month,
                amount_cents: 111,
                currency: "usd".into(),
                cloud_time_multiple: None,
                storage_multiple: None,
            }])
        );
        let head = server.await.unwrap().to_ascii_lowercase();
        assert!(head.starts_with("get /v1/plans "), "{head}");
        assert!(!head.contains("authorization"), "{head}");
        assert!(client.tokens().await.is_some(), "no token state is touched");
    }

    /// The catalog's multiples survive the HTTP read when present and valid; an
    /// older service without them, and a malformed value, leave the price row
    /// standing with the multiple unstated.
    #[tokio::test]
    async fn the_public_catalog_reads_multiples_present_absent_and_malformed() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (endpoint, server) = answering(
            "200 OK",
            r#"{"plans":[
                {"plan":"pro","interval":"month","amount_cents":111,"currency":"usd",
                 "cloud_time_multiple":1,"storage_multiple":1},
                {"plan":"pro","interval":"year","amount_cents":1110,"currency":"usd"},
                {"plan":"max","interval":"month","amount_cents":222,"currency":"usd",
                 "cloud_time_multiple":"lots","storage_multiple":-5},
                {"plan":"max","interval":"year","amount_cents":2220,"currency":"usd",
                 "cloud_time_multiple":5,"storage_multiple":null}]}"#,
        )
        .await;
        let plans = Client::new(&endpoint, None)
            .unwrap()
            .plans()
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
        let multiples: Vec<_> = plans
            .iter()
            .map(|price| {
                (
                    price.amount_cents,
                    price.cloud_time_multiple,
                    price.storage_multiple,
                )
            })
            .collect();
        assert_eq!(
            multiples,
            vec![
                (111, Some(1), Some(1)),
                (1110, None, None),
                (222, None, None),
                (2220, Some(5), None),
            ]
        );
    }

    /// An older account (no route) and an empty catalog are both "no prices";
    /// only a real failure is an error, and none of them is a sign-in problem.
    #[tokio::test]
    async fn no_route_or_an_empty_catalog_is_no_prices_and_a_failure_is_an_error() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        for (status, body) in [
            ("404 Not Found", r#"{"error":"not_found"}"#),
            ("200 OK", r#"{"plans":null}"#),
            ("200 OK", "{}"),
            ("200 OK", r#"{"plans":{"pro":1}}"#),
        ] {
            let (endpoint, server) = answering(status, body).await;
            let plans = Client::new(&endpoint, None).unwrap().plans().await;
            assert_eq!(plans.unwrap(), None, "{status} {body}");
            server.await.unwrap();
        }
        // An empty list is a catalog that offers nothing, not an error.
        let (endpoint, server) = answering("200 OK", r#"{"plans":[]}"#).await;
        let plans = Client::new(&endpoint, None).unwrap().plans().await;
        assert_eq!(plans.unwrap(), Some(Vec::new()));
        server.await.unwrap();
        for (status, body) in [
            ("500 Internal Server Error", "{}"),
            ("302 Found", "{}"),
            ("200 OK", "not json"),
        ] {
            let (endpoint, server) = answering(status, body).await;
            let plans = Client::new(&endpoint, None).unwrap().plans().await;
            assert!(plans.is_err(), "{status} {body}");
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_silent_catalog_times_out_instead_of_hanging() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(socket);
        });
        let client = Client::new(&endpoint, None).unwrap();
        let error = client
            .plans_within(Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(
            error
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout),
            "{error}"
        );
        server.abort();
    }

    #[test]
    fn only_token_answers_revoke() {
        for status in [400, 401, 403, 410] {
            assert!(refresh_revoked(status), "{status}");
        }
        for status in [200, 404, 408, 429, 500, 502, 503, 504] {
            assert!(!refresh_revoked(status), "{status}");
        }
    }
}

#[cfg(test)]
mod connection_prompt_tests {
    use super::*;
    fn event(id: &str) -> Event {
        Event::Prompt {
            id: id.into(),
            host_id: "host".into(),
            prompt: "Synthetic?".into(),
            echo: false,
        }
    }
    #[test]
    fn prompt_map_is_bounded_expires_and_answers_only_once() {
        let mut map = ConnectionPrompts::default();
        let Some(Event::Prompt { id, .. }) = map.receive(event("wire")).unwrap() else {
            unreachable!()
        };
        assert!(
            matches!(map.answer(EventCommand::Answer { id: id.clone(), value: None }), Some(EventCommand::Answer { id, value: None }) if id == "wire")
        );
        assert!(map
            .answer(EventCommand::Answer {
                id: id.clone(),
                value: None
            })
            .is_none());
        for index in 1..64 {
            map.receive(event(&format!("wire-{index}"))).unwrap();
        }
        assert!(map.receive(event("overflow")).is_err());
        map.by_wire.get_mut("wire").unwrap().expires = Instant::now();
        assert_eq!(map.expire(), vec![id]);
        assert!(map.receive(event("overflow")).is_ok());
    }
}
