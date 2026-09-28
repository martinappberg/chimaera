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
            let response = client
                .inner
                .http
                .post(path(&client.inner.account, &["v1", "oauth", "refresh"]))
                .json(&RefreshRequest { refresh_token })
                .send()
                .await?;
            if matches!(response.status().as_u16(), 401 | 403) {
                *guard = None;
                client.inner.token_updates.send_replace(None);
                *client.inner.keeper.write().await = None;
                bail!("device authorization revoked");
            }
            let tokens: Tokens = json_response(response).await?;
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
    pub async fn execution_capabilities(&self) -> Result<crate::ExecutionCapabilities> {
        let value: crate::ExecutionCapabilities = json_response(
            self.request(
                Method::GET,
                path(&self.inner.account, &["v2", "capabilities"]),
                None,
            )
            .await?,
        )
        .await?;
        anyhow::ensure!(value.supported(), "managed execution is unavailable");
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
    pub async fn workspace_placement(&self, workspace: &str) -> Result<crate::WorkspacePlacement> {
        anyhow::ensure!(
            crate::placement::valid_id(workspace),
            "invalid workspace identity"
        );
        let value: crate::WorkspacePlacement = json_response(
            self.request(
                Method::GET,
                path(
                    &self.inner.account,
                    &["v2", "workspaces", workspace, "placement"],
                ),
                None,
            )
            .await?,
        )
        .await?;
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
        if plan == Plan::None {
            bail!("choose Pro or Max");
        }
        let mut body =
            serde_json::json!({"plan": plan, "interval": interval, "return_to": "desktop"});
        if let Some(callback) = callback {
            body["desktop_callback"] = serde_json::to_value(callback)?;
        }
        json_response(
            self.request(
                Method::POST,
                path(&self.inner.account, &["v1", "billing", "checkout"]),
                Some(body),
            )
            .await?,
        )
        .await
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
            bail!("unsupported link protocol {}", account.protocol);
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
    pub async fn hosts(&self) -> Result<Vec<Host>> {
        json_response(
            self.request(
                Method::GET,
                path(&self.keeper().await?, &["v1", "hosts"]),
                None,
            )
            .await?,
        )
        .await
    }
    pub async fn add_host(&self, alias: &str) -> Result<Host> {
        self.add_host_with_ssh(alias, None).await
    }
    pub async fn add_host_with_ssh(&self, alias: &str, ssh: Option<SshTarget>) -> Result<Host> {
        json_response(
            self.request(
                Method::POST,
                path(&self.keeper().await?, &["v1", "hosts"]),
                Some(serde_json::to_value(AddHost {
                    alias: alias.into(),
                    ssh,
                })?),
            )
            .await?,
        )
        .await
    }
    pub async fn delete_host(&self, id: &str) -> Result<()> {
        self.request(
            Method::DELETE,
            path(&self.keeper().await?, &["v1", "hosts", id]),
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn reconnect_host(&self, id: &str) -> Result<()> {
        self.request(
            Method::POST,
            path(&self.keeper().await?, &["v1", "hosts", id, "reconnect"]),
            None,
        )
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
                    let mut ping = tokio::time::interval_at(Instant::now() + Duration::from_secs(20), Duration::from_secs(20));
                    let mut last_pong = Instant::now();
                    let connected = Instant::now();
                    loop {
                        tokio::select! {
                            _ = out.closed() => return Ok(()),
                            command = incoming.recv() => match command {
                                Some(command) => send_json(&mut tx, &command).await?,
                                None => return Ok(()),
                            },
                            message = rx.next() => match message {
                                Some(Ok(Message::Text(text))) => {
                                    if let Ok(event) = serde_json::from_str::<Event>(&text) {
                                        tokio::time::timeout(Duration::from_secs(10), out.send(Ok(event))).await?.map_err(|_| anyhow!("events consumer closed"))?;
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
                if let Err(error) = result {
                    if !matches!(
                        tokio::time::timeout(
                            Duration::from_secs(10),
                            out.send(Err(error.to_string()))
                        )
                        .await,
                        Ok(Ok(()))
                    ) {
                        break;
                    }
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

pub struct EventConnection {
    pub events: mpsc::Receiver<Result<Event, String>>,
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
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let local_port = listener.local_addr()?.port();
        let task = tokio::spawn(async move {
            let limits = Arc::new(Semaphore::new(MAX_STREAMS));
            let mut streams = JoinSet::new();
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        let Ok((tcp, _)) = result else { break; };
                        let Ok(permit) = limits.clone().try_acquire_owned() else { drop(tcp); continue; };
                        let client = client.clone(); let host_id = host_id.clone();
                        streams.spawn(async move {
                            let _permit = permit;
                            if let Ok(socket) = client.tcp(&host_id).await { let _ = bridge(tcp, socket).await; }
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
    let mut ping = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(20),
        Duration::from_secs(20),
    );
    let mut last_pong = Instant::now();
    loop {
        tokio::select! {
            message = socket.next() => match message {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServeEvent>(&text)? {
                    ServeEvent::Open { stream_id } => {
                        if active.len() >= MAX_STREAMS || active.contains_key(&stream_id) { bail!("serve stream limit or duplicate id"); }
                        let client = client.clone(); let key = stream_id.clone();
                        let abort = streams.spawn(async move {
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
                    ServeEvent::Registered { .. } => {}
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
fn backoff(attempt: u32) -> Duration {
    let ceiling = (500_u64.saturating_mul(1 << attempt.min(5))).min(10_000);
    Duration::from_millis(ceiling / 2 + rand::random_range(0..=ceiling / 2))
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
    serde_json::from_slice(&bytes).context("invalid JSON response")
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
