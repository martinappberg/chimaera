//! Optional account connection. Tokens live in the OS credential store; only
//! the endpoint is persisted in app.json. No endpoint means no work or sockets.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Mutex;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use anyhow::{Context, Result};
use chimaera_link::{Account, Client, Device, Event, Host, HostKind, HostStatus, Tokens};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use super::{lock, Shell};

mod auth;

pub(super) struct Pro {
    endpoint: Option<String>,
    client: Mutex<Option<Client>>,
    account: Mutex<Option<Account>>,
    pub hosts: Mutex<HashMap<String, Host>>,
    device_aliases: Mutex<HashSet<String>>,
    error: Mutex<Option<String>>,
    sign_in: auth::SignIn,
    delegation: Mutex<Option<chimaera_link::Delegation>>,
    daemon_stamp: Mutex<Option<(u16, String, bool)>>,
    worker_links: tokio::sync::Mutex<HashMap<String, chimaera_link::LinkTunnel>>,
    runtime: tokio::sync::Mutex<Option<Runtime>>,
    operation: tokio::sync::Mutex<()>,
    ready: tokio::sync::watch::Sender<bool>,
    credential_generation: Arc<AtomicU64>,
}

struct Runtime {
    events: Option<tokio::task::JoinHandle<()>>,
    tokens: tokio::task::JoinHandle<()>,
    reconcile: tokio::task::JoinHandle<()>,
    serve: Option<chimaera_link::Serve>,
}

#[derive(Serialize)]
pub struct Status {
    available: bool,
    signed_in: bool,
    email: Option<String>,
    plan: Option<chimaera_link::Plan>,
    error: Option<String>,
    sign_in: Option<auth::Status>,
}

#[derive(Serialize)]
pub struct KeptHost {
    alias: String,
    kept: bool,
    status: String,
    kind: String,
}

impl Pro {
    pub fn load() -> Self {
        let endpoint = read_endpoint();
        let ready = endpoint.is_none();
        Self {
            endpoint,
            client: Mutex::new(None),
            account: Mutex::new(None),
            hosts: Mutex::new(HashMap::new()),
            device_aliases: Mutex::new(HashSet::new()),
            error: Mutex::new(None),
            sign_in: auth::SignIn::default(),
            delegation: Mutex::new(None),
            daemon_stamp: Mutex::new(None),
            worker_links: tokio::sync::Mutex::new(HashMap::new()),
            runtime: tokio::sync::Mutex::new(None),
            operation: tokio::sync::Mutex::new(()),
            ready: tokio::sync::watch::channel(ready).0,
            credential_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn is_device(&self, alias: &str) -> bool {
        if let Some(host) = lock(&self.hosts).values().find(|host| host.alias == alias) {
            return host.kind != HostKind::Ssh;
        }
        lock(&self.device_aliases).contains(alias)
    }

    fn remember_device(&self, host: &Host) {
        if host.kind != HostKind::Ssh {
            let mut devices = lock(&self.device_aliases);
            if devices.len() < 256 {
                devices.insert(host.alias.clone());
            }
        }
    }

    pub fn generation(&self) -> u64 {
        self.credential_generation.load(Ordering::SeqCst)
    }

    fn has_keeper(&self) -> bool {
        lock(&self.account).as_ref().is_some_and(keeper_available)
    }

    pub async fn client(&self) -> Option<Client> {
        self.client_snapshot().await.map(|(client, _)| client)
    }

    async fn client_snapshot(&self) -> Option<(Client, u64)> {
        let mut ready = self.ready.subscribe();
        let _ = ready.wait_for(|ready| *ready).await;
        let client = lock(&self.client);
        client.clone().map(|client| (client, self.generation()))
    }
}

fn keeper_available(account: &Account) -> bool {
    account.plan != chimaera_link::Plan::None && !account.keeper_url.is_empty()
}

fn read_endpoint() -> Option<String> {
    let file = std::fs::File::open(chimaera_core::config_dir().join("app.json")).ok()?;
    let mut bytes = Vec::new();
    file.take(16_385).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 16_384 {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json["pro"]["endpoint"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn credential(endpoint: &str) -> Result<keyring::Entry> {
    let service = if chimaera_core::is_dev_build() {
        "chimaera.dev.pro"
    } else {
        "chimaera.pro"
    };
    Ok(keyring::Entry::new(service, endpoint)?)
}

fn load_tokens(endpoint: &str) -> Result<Option<Tokens>> {
    match credential(endpoint)?.get_password() {
        Ok(value) => Ok(Some(
            serde_json::from_str(&value).context("invalid saved account credential")?,
        )),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn save_tokens(endpoint: &str, tokens: Option<&Tokens>) -> Result<()> {
    let _serialized = lock(&KEYCHAIN_IO);
    save_tokens_locked(endpoint, tokens)
}

static KEYCHAIN_IO: Mutex<()> = Mutex::new(());

fn save_tokens_locked(endpoint: &str, tokens: Option<&Tokens>) -> Result<()> {
    let entry = credential(endpoint)?;
    if let Some(tokens) = tokens {
        entry.set_password(&serde_json::to_string(tokens)?)?;
    } else {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<Shell>();
        let Some(endpoint) = state.pro.endpoint.clone() else {
            return;
        };
        let _operation = state.pro.operation.lock().await;
        let read_endpoint = endpoint.clone();
        let loaded = tokio::task::spawn_blocking(move || load_tokens(&read_endpoint)).await;
        let result = async {
            let Some(tokens) = loaded?? else {
                return Ok::<_, anyhow::Error>(());
            };
            let client = Client::new(&endpoint, Some(tokens))?;
            activate(&app, client).await
        }
        .await;
        if let Err(error) = result {
            *lock(&state.pro.error) = Some(error.to_string());
        }
        state.pro.ready.send_replace(true);
        let _ = app.emit("pro-changed", ());
    });
}

async fn account_snapshot(client: &Client) -> Result<(Account, Vec<Host>, Option<String>)> {
    let account = client.me().await?;
    // Account authentication is independent of keeper provisioning. A valid
    // login must remain signed in while that optional connection comes online.
    let (hosts, connection_error) = if !keeper_available(&account) {
        (Vec::new(), None)
    } else {
        match client.hosts().await {
            Ok(hosts) => (hosts, None),
            Err(_) => (Vec::new(), Some("You're signed in. Your Pro connection is preparing; Chimaera will reconnect automatically.".into())),
        }
    };
    Ok((account, hosts, connection_error))
}

async fn activate(app: &AppHandle, client: Client) -> Result<()> {
    let state = app.state::<Shell>();
    // Subscribe before any request can rotate the refresh token.
    let mut updates = client.token_updates();
    let snapshot = account_snapshot(&client).await;
    let endpoint = state.pro.endpoint.clone().context("endpoint unavailable")?;
    let tokens = client.tokens().await;
    let authenticated = tokens.is_some();
    let stored_endpoint = endpoint.clone();
    if snapshot.is_ok() {
        let previous = {
            let mut current = lock(&state.pro.client);
            state
                .pro
                .credential_generation
                .fetch_add(1, Ordering::SeqCst);
            current.take()
        };
        stop(&state).await;
        *lock(&state.pro.delegation) = None;
        *lock(&state.pro.daemon_stamp) = None;
        if let Some(previous) = previous {
            previous.clear_tokens().await;
        }
    }
    // A request can rotate credentials and then fail on its next hop. Preserve
    // that rotation even when the keeper is unavailable during activation.
    tokio::task::spawn_blocking(move || save_tokens(&stored_endpoint, tokens.as_ref())).await??;
    let (account, hosts, connection_error) = snapshot?;
    anyhow::ensure!(authenticated, "sign in required");
    let has_keeper = keeper_available(&account);
    *lock(&state.pro.account) = Some(account);
    for host in &hosts {
        state.pro.remember_device(host);
    }
    *lock(&state.pro.hosts) = hosts
        .into_iter()
        .take(256)
        .map(|host| (host.id.clone(), host))
        .collect();
    *lock(&state.pro.client) = Some(client.clone());
    *lock(&state.pro.error) = connection_error;

    let token_app = app.clone();
    let generation = state.pro.credential_generation.clone();
    let expected = generation.load(Ordering::SeqCst);
    let tokens = tokio::spawn(async move {
        while updates.changed().await.is_ok() {
            let tokens = updates.borrow_and_update().clone();
            let revoked = tokens.is_none();
            let endpoint = endpoint.clone();
            let generation = generation.clone();
            let result = tokio::task::spawn_blocking(move || {
                let _serialized = lock(&KEYCHAIN_IO);
                if generation.load(Ordering::SeqCst) != expected {
                    return Ok(());
                }
                save_tokens_locked(&endpoint, tokens.as_ref())
            })
            .await;
            if revoked || !matches!(result, Ok(Ok(()))) {
                let app = token_app.clone();
                tokio::spawn(async move {
                    let _ = sign_out(&app, false, Some(expected)).await;
                    let state = app.state::<Shell>();
                    if state.pro.generation() == expected + 1 {
                        *lock(&state.pro.error) = Some(if revoked {
                            "Your account session expired. Sign in again."
                        } else {
                            "Account credentials could not be saved in the system keychain. Sign in again."
                        }.into());
                        let _ = app.emit("pro-changed", ());
                    }
                });
                break;
            }
        }
    });

    super::power::install(app);
    if let Err(error) = configure_daemon(&state, &client).await {
        *lock(&state.pro.error) = Some(error.to_string());
    }
    let (events, serve) = if has_keeper {
        let (events, serve) = start_keeper(app, client.clone());
        (Some(events), Some(serve))
    } else {
        (None, None)
    };
    let reconcile_app = app.clone();
    let reconcile = tokio::spawn(async move {
        // Events carry prompt latency; this bounded, slow reconciliation also
        // discovers provisioning and removals missed while a stream was down.
        let mut ticks = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(30),
            Duration::from_secs(30),
        );
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            if let Err(error) = reconcile_account(&reconcile_app, &client, expected).await {
                let state = reconcile_app.state::<Shell>();
                if state.pro.generation() == expected {
                    *lock(&state.pro.error) = Some(error.to_string());
                    let _ = reconcile_app.emit("pro-changed", ());
                }
            }
        }
    });
    *state.pro.runtime.lock().await = Some(Runtime {
        events,
        tokens,
        reconcile,
        serve,
    });
    Ok(())
}

fn start_keeper(
    app: &AppHandle,
    client: Client,
) -> (tokio::task::JoinHandle<()>, chimaera_link::Serve) {
    let state = app.state::<Shell>();
    let mut connection = client.events();
    let event_app = app.clone();
    let events = tokio::spawn(async move {
        while let Some(event) = connection.events.recv().await {
            match event {
                Ok(Event::Host { host }) => apply_host(&event_app, host).await,
                Ok(Event::HostRemoved { host_id }) => {
                    let removed = lock(&event_app.state::<Shell>().pro.hosts).remove(&host_id);
                    if let Some(mut host) = removed {
                        host.status = HostStatus::Offline;
                        host.daemon = None;
                        host_signal(&event_app, &host).await;
                    }
                    let _ = event_app.emit("pro-changed", ());
                }
                Ok(Event::Prompt {
                    id,
                    host_id,
                    prompt,
                    echo: _,
                }) => {
                    let host = lock(&event_app.state::<Shell>().pro.hosts)
                        .get(&host_id)
                        .cloned();
                    if let Some(host) = host {
                        crate::askpass::relay_keeper(
                            &event_app,
                            host.alias,
                            host_id,
                            id,
                            prompt,
                            connection.commands.clone(),
                        );
                    }
                }
                Ok(Event::PromptClosed { id }) => {
                    crate::askpass::close_keeper(&event_app, Some(&id))
                }
                Err(error) => {
                    crate::askpass::close_keeper(&event_app, None);
                    *lock(&event_app.state::<Shell>().pro.error) = Some(error);
                    let _ = event_app.emit("pro-changed", ());
                }
            }
        }
    });
    let local = lock(&state.local).clone();
    let alias = machine_name();
    let serve = chimaera_link::Serve::start(
        client,
        local.port,
        alias,
        chimaera_link::Daemon {
            token: local.token,
            build: local.build.unwrap_or_default(),
            sessions: local.live_sessions.unwrap_or(0),
        },
    );
    (events, serve)
}

pub(super) async fn reconcile_account(
    app: &AppHandle,
    client: &Client,
    generation: u64,
) -> Result<()> {
    let account = client.me().await?;
    let hosts = if !keeper_available(&account) {
        Vec::new()
    } else {
        client.hosts().await?
    };
    let state = app.state::<Shell>();
    // Blocking HTTP requests cannot be cancelled by aborting the reconcile task.
    // Serialize mutations through completion so sign-out's final DELETE wins.
    let _operation = state.pro.operation.lock().await;
    let mut runtime = state.pro.runtime.lock().await;
    if state.pro.generation() != generation {
        return Ok(());
    }
    let changed = lock(&state.pro.account).as_ref().is_none_or(|old| {
        old.keeper_url != account.keeper_url || keeper_available(old) != keeper_available(&account)
    });
    if let Some(runtime) = runtime.as_mut() {
        if changed {
            if let Some(events) = runtime.events.take() {
                events.abort();
            }
            if let Some(serve) = runtime.serve.take() {
                serve.close();
            }
            crate::askpass::close_keeper(app, None);
        }
        if keeper_available(&account) && runtime.events.is_none() {
            let (events, serve) = start_keeper(app, client.clone());
            runtime.events = Some(events);
            runtime.serve = Some(serve);
        }
    }
    *lock(&state.pro.account) = Some(account);
    replace_hosts(app, hosts).await;
    drop(runtime);
    configure_daemon(&state, client).await?;
    reconcile_placements(&state, client).await?;
    *lock(&state.pro.error) = None;
    let _ = app.emit("pro-changed", ());
    Ok(())
}

async fn replace_hosts(app: &AppHandle, hosts: Vec<Host>) {
    for host in &hosts {
        app.state::<Shell>().pro.remember_device(host);
    }
    let current: HashMap<_, _> = hosts
        .into_iter()
        .take(256)
        .map(|host| (host.id.clone(), host))
        .collect();
    let previous = std::mem::replace(&mut *lock(&app.state::<Shell>().pro.hosts), current.clone());
    for (_, mut host) in previous {
        if !current.contains_key(&host.id) {
            host.status = HostStatus::Offline;
            host.daemon = None;
            host_signal(app, &host).await;
        }
    }
    for host in current.values() {
        host_signal(app, host).await;
    }
}

fn machine_name() -> String {
    chimaera_core::this_node().unwrap_or_else(|| "My computer".into())
}

pub(super) async fn apply_host(app: &AppHandle, host: Host) {
    let state = app.state::<Shell>();
    state.pro.remember_device(&host);
    {
        let mut hosts = lock(&state.pro.hosts);
        if hosts.len() >= 256 && !hosts.contains_key(&host.id) {
            return;
        }
        hosts.insert(host.id.clone(), host.clone());
    }
    *lock(&state.pro.error) = None;
    host_signal(app, &host).await;
    let _ = app.emit("pro-changed", ());
}

async fn host_signal(app: &AppHandle, host: &Host) {
    let state = app.state::<Shell>();
    let endpoint = {
        let mut tunnels = state.tunnels.lock().await;
        let Some(tunnel) = tunnels
            .get_mut(&host.alias)
            .filter(|tunnel| tunnel.link_id() == Some(host.id.as_str()))
        else {
            return;
        };
        tunnel.update_link(host);
        (
            tunnel.local_port,
            tunnel.manifest.token.clone(),
            tunnel.manifest.build.clone(),
        )
    };
    let connected = host.status == HostStatus::Connected && host.daemon.is_some();
    if connected {
        lock(&state.unhealthy_tunnels).remove(&host.alias);
    } else {
        lock(&state.unhealthy_tunnels).insert(host.alias.clone());
    }
    let _ = app.emit(
        "host-status",
        super::connect::HostStatus {
            alias: host.alias.clone(),
            status: if connected { "connected" } else { "down" },
            local_port: Some(endpoint.0),
            token: connected.then_some(endpoint.1),
            error: host.error.clone(),
            reason: (!connected).then(|| "Your Pro connection is reconnecting.".into()),
            build: endpoint.2,
            node: None,
        },
    );
}

pub(super) async fn stop(state: &Shell) {
    let links = std::mem::take(&mut *state.pro.worker_links.lock().await);
    for (host, link) in links {
        let _ = daemon_request(
            state,
            "DELETE",
            &format!("/pro/placements?host_id={host}"),
            None,
        )
        .await;
        link.close();
    }
    if let Some(runtime) = state.pro.runtime.lock().await.take() {
        if let Some(events) = runtime.events {
            events.abort();
        }
        runtime.tokens.abort();
        runtime.reconcile.abort();
        if let Some(serve) = runtime.serve {
            serve.close();
        }
    }
}

pub(super) async fn refresh_serve(state: &Shell) {
    let client = lock(&state.pro.client).clone();
    let Some(client) = client else {
        return;
    };
    let local = lock(&state.local).clone();
    if let Some(runtime) = state
        .pro
        .runtime
        .lock()
        .await
        .as_mut()
        .filter(|runtime| runtime.serve.is_some())
    {
        if let Some(serve) = runtime.serve.take() {
            serve.close();
        }
        runtime.serve = Some(chimaera_link::Serve::start(
            client,
            local.port,
            machine_name(),
            chimaera_link::Daemon {
                token: local.token,
                build: local.build.unwrap_or_default(),
                sessions: local.live_sessions.unwrap_or(0),
            },
        ));
    }
}

#[tauri::command]
pub async fn pro_status(state: tauri::State<'_, Shell>) -> Result<Status, String> {
    let client = state.pro.client().await;
    let signed_in = if let Some(client) = client {
        client.tokens().await.is_some()
    } else {
        false
    };
    let account = lock(&state.pro.account).clone();
    Ok(Status {
        available: state.pro.endpoint.is_some(),
        signed_in,
        email: account.as_ref().map(|account| account.email.clone()),
        plan: account.map(|account| account.plan),
        error: lock(&state.pro.error).clone(),
        sign_in: state.pro.sign_in.status(),
    })
}

#[tauri::command]
pub async fn pro_sign_in(app: AppHandle, window: tauri::WebviewWindow) -> Result<(), String> {
    let state = app.state::<Shell>();
    let endpoint = state
        .pro
        .endpoint
        .clone()
        .ok_or("Chimaera Pro isn't available in this build")?;
    let client = Client::new(&endpoint, None).map_err(|error| error.to_string())?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| "Could not open the local sign-in listener. Try again.".to_string())?;
    let redirect = format!(
        "http://127.0.0.1:{}/callback",
        listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port()
    );
    let pkce = chimaera_link::Pkce::new();
    let url = pkce
        .authorization_url(&endpoint, &redirect)
        .map_err(|error| error.to_string())?;
    let mut attempt = state
        .pro
        .sign_in
        .begin()
        .map_err(|error| error.to_string())?;
    *lock(&state.pro.error) = None;
    let _ = app.emit("pro-changed", ());
    let sign_in_app = app.clone();
    tokio::spawn(async move {
        let app = sign_in_app;
        let state = app.state::<Shell>();
        let deadline = tokio::time::Instant::now() + auth::WINDOW;
        let mut accepted = None;
        let outcome = async {
            tokio::select! {
                _ = attempt.cancelled() => anyhow::bail!("Sign-in cancelled"),
                result = tokio::task::spawn_blocking(move || open::that(url.as_str())) => {
                    result?.context("Could not open your browser. Try again from Chimaera.")?;
                }
            }
            let callback =
                auth::wait_callback(listener, &redirect, &pkce, &mut attempt, deadline).await?;
            accepted = Some(callback);
            // Waiting for the browser never locks sign-out, sleep handling or
            // account reconciliation. Only the bounded credential commit does.
            let _operation = tokio::select! {
                _ = attempt.cancelled() => anyhow::bail!("Sign-in cancelled"),
                result = tokio::time::timeout_at(deadline, state.pro.operation.lock()) => {
                    result.context("Sign-in expired. Choose Try again.")?
                }
            };
            anyhow::ensure!(state.pro.sign_in.finishing(attempt.id), "Sign-in cancelled");
            let _ = app.emit("pro-changed", ());
            client
                .exchange_code(chimaera_link::TokenRequest {
                    grant_type: "authorization_code".into(),
                    code: accepted.as_ref().unwrap().code.clone(),
                    redirect_uri: redirect,
                    code_verifier: pkce.verifier,
                    device_name: machine_name(),
                })
                .await
                .context("Sign-in could not be completed. Choose Try again.")?;
            activate(&app, client).await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        let current = state.pro.sign_in.complete(attempt.id);
        let success = current && outcome.is_ok();
        if current {
            if let Err(error) = outcome {
                *lock(&state.pro.error) = Some(error.to_string());
            }
            let _ = app.emit("pro-changed", ());
        }
        if let Some(callback) = accepted {
            callback.finish(success).await;
        }
        if success {
            let _ = window.set_focus();
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn pro_cancel_sign_in(app: AppHandle) -> Result<(), String> {
    let state = app.state::<Shell>();
    state
        .pro
        .sign_in
        .cancel_waiting()
        .map_err(|error| error.to_string())?;
    *lock(&state.pro.error) = None;
    let _ = app.emit("pro-changed", ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_hosts_never_fall_back_to_ssh_without_an_account_route() {
        assert!(device_fallback::<()>(true).is_err());
        assert_eq!(device_fallback::<()>(false).unwrap(), None);
    }

    #[tokio::test]
    async fn keeper_unavailability_does_not_reject_a_valid_account_login() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let keeper = endpoint.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 8192];
                let n = stream.read(&mut bytes).await.unwrap();
                let me = std::str::from_utf8(&bytes[..n])
                    .unwrap()
                    .starts_with("GET /v1/me ");
                let (status, body) = if me {
                    ("200 OK", serde_json::json!({"account_id":"fixture","email":"fixture@example.invalid","plan":"pro","device_id":"fixture-device","protocol":0,"keeper_url":keeper,"limits":{"cloud_hours":100,"storage_bytes":20000000000u64},"usage":{"cloud_hours":0,"storage_bytes":0},"hours_exhausted":false}).to_string())
                } else {
                    ("503 Service Unavailable", "{}".into())
                };
                stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        let client = Client::new(
            &endpoint,
            Some(Tokens {
                access_token: "fixture-access".into(),
                refresh_token: "fixture-refresh".into(),
                token_type: "Bearer".into(),
                expires_in: 3600,
            }),
        )
        .unwrap();
        let (account, hosts, warning) = account_snapshot(&client).await.unwrap();
        assert_eq!(account.plan, chimaera_link::Plan::Pro);
        assert!(hosts.is_empty());
        assert!(warning.unwrap().starts_with("You're signed in."));
        assert!(client.tokens().await.is_some());
        server.await.unwrap();
    }
}

#[tauri::command]
pub async fn pro_sign_out(app: AppHandle) -> Result<(), String> {
    sign_out(&app, false, None).await
}

#[tauri::command]
pub async fn pro_sign_out_everywhere(app: AppHandle) -> Result<(), String> {
    sign_out(&app, true, None).await
}

async fn sign_out(app: &AppHandle, everywhere: bool, expected: Option<u64>) -> Result<(), String> {
    async {
        let state = app.state::<Shell>();
        let _operation = state.pro.operation.lock().await;
        if expected.is_some_and(|expected| expected != state.pro.generation()) {
            return Ok(());
        }
        state.pro.sign_in.cancel();
        if everywhere {
            state
                .pro
                .client()
                .await
                .context("Sign in first")?
                .sign_out_everywhere()
                .await?;
        }
        let client = {
            let mut client = lock(&state.pro.client);
            let previous = client.take();
            state
                .pro
                .credential_generation
                .fetch_add(1, Ordering::SeqCst);
            previous
        };
        stop(&state).await;
        let _ = daemon_request(&state, "DELETE", "/pro/configure", None).await;
        *lock(&state.pro.delegation) = None;
        *lock(&state.pro.daemon_stamp) = None;
        crate::askpass::close_keeper(app, None);
        if let Some(client) = client {
            client.clear_tokens().await;
        }
        *lock(&state.pro.account) = None;
        lock(&state.pro.hosts).clear();
        let removed: Vec<_> = {
            let mut tunnels = state.tunnels.lock().await;
            let aliases: Vec<_> = tunnels
                .iter()
                .filter(|(_, tunnel)| tunnel.link_id().is_some())
                .map(|(alias, _)| alias.clone())
                .collect();
            aliases
                .into_iter()
                .filter_map(|alias| tunnels.remove(&alias).map(|tunnel| (alias, tunnel)))
                .collect()
        };
        for (alias, tunnel) in removed {
            let port = tunnel.local_port;
            tunnel.close().await;
            let _ = app.emit(
                "host-status",
                super::connect::HostStatus {
                    alias,
                    status: "down",
                    local_port: Some(port),
                    token: None,
                    error: None,
                    reason: Some("Signed out of Chimaera Pro".into()),
                    build: None,
                    node: None,
                },
            );
        }
        if let Some(endpoint) = state.pro.endpoint.clone() {
            tokio::task::spawn_blocking(move || save_tokens(&endpoint, None)).await??;
        }
        *lock(&state.pro.error) = None;
        let _ = app.emit("pro-changed", ());
        Ok::<_, anyhow::Error>(())
    }
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn pro_hosts(state: tauri::State<'_, Shell>) -> Result<Vec<KeptHost>, String> {
    let client = state.pro.client().await.ok_or("Sign in first")?;
    let hosts = if state.pro.has_keeper() {
        client.hosts().await.map_err(|error| error.to_string())?
    } else {
        Vec::new()
    };
    let saved = super::connect::with_hosts(|hosts| Ok(hosts.list())).await?;
    let mut out: Vec<_> = saved
        .iter()
        .map(|entry| {
            let host = hosts.iter().find(|host| host.alias == entry.alias);
            KeptHost {
                alias: entry.alias.clone(),
                kept: host.is_some(),
                status: host
                    .map(|host| status_name(&host.status))
                    .unwrap_or("offline")
                    .into(),
                kind: "ssh".into(),
            }
        })
        .collect();
    for host in hosts {
        if !out.iter().any(|entry| entry.alias == host.alias) {
            out.push(KeptHost {
                alias: host.alias,
                kept: true,
                status: status_name(&host.status).into(),
                kind: match host.kind {
                    HostKind::Device => "device",
                    HostKind::Worker => "worker",
                    HostKind::Ssh => "ssh",
                }
                .into(),
            });
        }
    }
    Ok(out)
}

fn status_name(status: &HostStatus) -> &'static str {
    match status {
        HostStatus::Connected => "connected",
        HostStatus::Connecting => "connecting",
        HostStatus::Prompting => "prompting",
        HostStatus::Offline => "offline",
    }
}

/// Keeper rows are authoritative while signed in; the cached preference only
/// selects the SSH fallback when account credentials are absent.
pub(super) async fn connection(
    state: &Shell,
    alias: &str,
) -> Result<Option<(Client, Host, u64)>, String> {
    let device = state.pro.is_device(alias) || lock(&state.registry).is_link_device(alias);
    let Some((client, generation)) = state.pro.client_snapshot().await else {
        return device_fallback(device);
    };
    if !state.pro.has_keeper() {
        return device_fallback(device);
    }
    let saved_alias = alias.to_string();
    let kept = super::connect::with_hosts(move |hosts| {
        Ok(hosts.get(&saved_alias).is_some_and(|host| host.kept))
    })
    .await?;
    let known = lock(&state.pro.hosts)
        .values()
        .any(|host| host.alias == alias);
    if !kept && !known {
        return device_fallback(device);
    }
    let hosts = client.hosts().await.map_err(|error| error.to_string())?;
    let host = hosts.iter().find(|host| host.alias == alias).cloned();
    {
        let _current = lock(&state.pro.client);
        if state.pro.generation() != generation {
            return Err("Account changed while connecting".into());
        }
        *lock(&state.pro.hosts) = hosts
            .into_iter()
            .take(256)
            .map(|host| (host.id.clone(), host))
            .collect();
    }
    match host {
        Some(host) => Ok(Some((client, host, generation))),
        None => device_fallback(device),
    }
}

fn device_fallback<T>(device: bool) -> Result<Option<T>, String> {
    if device {
        Err("Sign in to Chimaera Pro to reconnect this device".into())
    } else {
        Ok(None)
    }
}

#[tauri::command]
pub async fn pro_set_host_kept(app: AppHandle, alias: String, kept: bool) -> Result<(), String> {
    let state = app.state::<Shell>();
    let _operation = state.pro.operation.lock().await;
    let client = state.pro.client().await.ok_or("Sign in first")?;
    let alias =
        chimaera_remote::hosts::normalize_alias(&alias).map_err(|error| error.to_string())?;
    if kept {
        let (hostname, user, port) = chimaera_remote::ssh_destination(&alias)
            .await
            .map_err(|error| error.to_string())?;
        let host = client
            .add_host_with_ssh(
                &alias,
                Some(chimaera_link::SshTarget {
                    hostname,
                    user,
                    port,
                }),
            )
            .await
            .map_err(|error| error.to_string())?;
        apply_host(&app, host).await;
    } else {
        let hosts = client.hosts().await.map_err(|error| error.to_string())?;
        for host in hosts.iter().filter(|host| host.alias == alias) {
            if host.kind != HostKind::Ssh {
                return Err("Device connections are managed by signing out that device".into());
            }
            client
                .delete_host(&host.id)
                .await
                .map_err(|error| error.to_string())?;
            lock(&state.pro.hosts).remove(&host.id);
        }
    }
    let cached = alias.clone();
    let entry = super::connect::with_hosts(move |hosts| hosts.set_kept(&cached, kept)).await?;
    lock(&state.host_entries).insert(alias.clone(), entry);
    let connected = state.tunnels.lock().await.contains_key(&alias);
    let _ = app.emit("pro-changed", ());
    drop(_operation);
    if connected {
        super::connect::do_connect(&app, alias, false).await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn pro_devices(state: tauri::State<'_, Shell>) -> Result<Vec<Device>, String> {
    state
        .pro
        .client()
        .await
        .ok_or("Sign in first")?
        .devices()
        .await
        .map_err(|error| error.to_string())
}

/// The native account panel always targets the laptop daemon, including when
/// it is opened from a workspace currently displayed through another host.
pub(super) async fn daemon_request(
    state: &Shell,
    method: &str,
    suffix: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    anyhow::ensure!(
        suffix.starts_with("/pro/") && !suffix.contains(['\r', '\n']),
        "invalid local Pro route"
    );
    let local = lock(&state.local).clone();
    let method = method.to_string();
    let suffix = suffix.to_string();
    tokio::task::spawn_blocking(move || {
        let url = format!("http://127.0.0.1:{}/api/v1{suffix}", local.port);
        let authorization = format!("Bearer {}", local.token);
        let mut response = match method.as_str() {
            "GET" => crate::http::agent()
                .get(&url)
                .header("Authorization", &authorization)
                .config()
                .timeout_global(Some(Duration::from_secs(15)))
                .max_redirects(0)
                .build()
                .call()?,
            "DELETE" => crate::http::agent()
                .delete(&url)
                .header("Authorization", &authorization)
                .config()
                .timeout_global(Some(Duration::from_secs(15)))
                .max_redirects(0)
                .build()
                .call()?,
            "PUT" => crate::http::agent()
                .put(&url)
                .header("Authorization", &authorization)
                .config()
                .timeout_global(Some(Duration::from_secs(30)))
                .max_redirects(0)
                .build()
                .send_json(body.unwrap_or(serde_json::Value::Null))?,
            "POST" => crate::http::agent()
                .post(&url)
                .header("Authorization", &authorization)
                .config()
                .timeout_global(Some(Duration::from_secs(30)))
                .max_redirects(0)
                .build()
                .send_json(body.unwrap_or(serde_json::Value::Null))?,
            _ => anyhow::bail!("invalid local Pro method"),
        };
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= 2 * 1024 * 1024,
            "local Pro response exceeds limit"
        );
        if bytes.is_empty() {
            Ok(serde_json::Value::Null)
        } else {
            Ok(serde_json::from_slice(&bytes)?)
        }
    })
    .await?
}
async fn configure_daemon(state: &Shell, client: &Client) -> Result<()> {
    let Some(account) = lock(&state.pro.account).clone() else {
        return Ok(());
    };
    let suitable = tokio::task::spawn_blocking(super::power::suitable).await?;
    daemon_request(
        state,
        "PUT",
        "/pro/power",
        Some(serde_json::json!({"suitable":suitable})),
    )
    .await?;
    if account.keeper_url.is_empty() || account.plan == chimaera_link::Plan::None {
        let cleared = (lock(&state.local).port, String::new(), false);
        if lock(&state.pro.daemon_stamp).as_ref() != Some(&cleared) {
            // The daemon outlives the GUI. A downgrade must revoke its local
            // runtime even when this app process never configured that runtime.
            // Keep the stamp unchanged on failure so reconciliation retries.
            daemon_request(state, "DELETE", "/pro/configure", None).await?;
            let mut links = state.pro.worker_links.lock().await;
            for link in links.values() {
                link.close();
            }
            let ids: Vec<_> = links.keys().cloned().collect();
            for id in ids {
                daemon_request(
                    state,
                    "DELETE",
                    &format!("/pro/placements?host_id={id}"),
                    None,
                )
                .await?;
                links.remove(&id);
            }
            *lock(&state.pro.delegation) = None;
            *lock(&state.pro.daemon_stamp) = Some(cleared);
        }
        return Ok(());
    }
    let local = lock(&state.local).clone();
    let stamp = (
        local.port,
        account.keeper_url.clone(),
        account.hours_exhausted,
    );
    if lock(&state.pro.daemon_stamp).as_ref() == Some(&stamp) {
        return Ok(());
    }
    let cached = lock(&state.pro.delegation).clone();
    let delegation = if let Some(delegation) = cached {
        delegation
    } else {
        client.delegate_daemon().await?
    };
    let endpoint = state
        .pro
        .endpoint
        .clone()
        .context("Pro endpoint unavailable")?;
    daemon_request(state,"POST","/pro/configure",Some(serde_json::json!({"endpoint":endpoint,"keeper_url":account.keeper_url,"delegation":delegation,"role":"device","hours_exhausted":account.hours_exhausted}))).await?;
    *lock(&state.pro.delegation) = Some(delegation);
    *lock(&state.pro.daemon_stamp) = Some(stamp);
    Ok(())
}
async fn reconcile_placements(state: &Shell, client: &Client) -> Result<()> {
    if lock(&state.pro.account).as_ref().is_none_or(|account| {
        account.plan == chimaera_link::Plan::None || account.keeper_url.is_empty()
    }) {
        return Ok(());
    }
    let status = daemon_request(state, "GET", "/pro/status", None).await?;
    let hosts: Vec<_> = lock(&state.pro.hosts)
        .values()
        .filter(|host| host.kind == HostKind::Worker)
        .cloned()
        .collect();
    let mut links = state.pro.worker_links.lock().await;
    let retired: Vec<_> = links
        .keys()
        .filter(|id| !hosts.iter().any(|host| &host.id == *id))
        .cloned()
        .collect();
    for id in retired {
        if let Some(link) = links.remove(&id) {
            daemon_request(
                state,
                "DELETE",
                &format!("/pro/placements?host_id={id}"),
                None,
            )
            .await?;
            link.close();
        }
    }
    for host in hosts {
        let Some(daemon) = host.daemon.as_ref() else {
            continue;
        };
        if !links.contains_key(&host.id) {
            links.insert(
                host.id.clone(),
                chimaera_link::LinkTunnel::bind(client.clone(), host.id.clone()).await?,
            );
        }
        let port = links
            .get(&host.id)
            .expect("inserted worker link")
            .local_port;
        for workspace in status["workspaces"].as_array().into_iter().flatten() {
            if workspace["ownership"]["state"] != "remote"
                || workspace["ownership"]["holder"] != host.id
            {
                continue;
            }
            daemon_request(state,"POST","/pro/placements",Some(serde_json::json!({"host_id":host.id,"endpoint":format!("http://127.0.0.1:{port}"),"token":daemon.token,"workspace_id":workspace["workspace_id"],"epoch":workspace["ownership"]["epoch"]}))).await?;
        }
    }
    Ok(())
}
#[tauri::command]
pub async fn pro_mirror_status(
    state: tauri::State<'_, Shell>,
) -> Result<serde_json::Value, String> {
    daemon_request(&state, "GET", "/pro/status", None)
        .await
        .map_err(|error| error.to_string())
}
#[tauri::command]
pub async fn pro_set_never_mirror(
    state: tauri::State<'_, Shell>,
    workspace_id: String,
    never_mirror: bool,
) -> Result<(), String> {
    async {
        anyhow::ensure!(
            !workspace_id.is_empty()
                && workspace_id.len() <= 128
                && workspace_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
            "invalid workspace"
        );
        let _operation = state.pro.operation.lock().await;
        if !never_mirror {
            state
                .pro
                .client()
                .await
                .context("Sign in to enable the cloud mirror")?
                .enable_mirror(&workspace_id)
                .await?;
        }
        // Stop local publishing first. If account deletion fails, the local
        // privacy flag remains true and the UI reports that cloud policy could
        // not yet be disabled; it must never silently re-enable local copying.
        daemon_request(
            &state,
            "PUT",
            "/pro/privacy",
            Some(serde_json::json!({"workspace_id":workspace_id,"never_mirror":never_mirror})),
        )
        .await?;
        if never_mirror {
            state
                .pro
                .client()
                .await
                .context("Sign in to disable the cloud mirror")?
                .disable_handoff_policy(&workspace_id)
                .await?;
            daemon_request(&state, "PUT", "/pro/privacy", Some(serde_json::json!({"workspace_id":workspace_id,"never_mirror":true,"confirmed":true}))).await?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await
    .map_err(|error| error.to_string())
}

#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum MirrorPreference {
    Projects {
        root: String,
    },
    Profile {
        workspace_id: String,
        profile: serde_json::Value,
    },
    Pin {
        session_id: String,
        keep_running: bool,
    },
}
#[tauri::command]
pub async fn pro_mirror_preference(
    state: tauri::State<'_, Shell>,
    request: MirrorPreference,
) -> Result<(), String> {
    async {
        if let MirrorPreference::Projects { root } = &request {
            daemon_request(
                &state,
                "PUT",
                "/pro/projects",
                Some(serde_json::json!({"root":root})),
            )
            .await?;
            return Ok(());
        }
        let (id, path, body) = match request {
            MirrorPreference::Projects { .. } => unreachable!("projects handled above"),
            MirrorPreference::Profile {
                workspace_id,
                profile,
            } => {
                let path = format!("/pro/profile?workspace_id={workspace_id}");
                (workspace_id, path, profile)
            }
            MirrorPreference::Pin {
                session_id,
                keep_running,
            } => (
                session_id.clone(),
                "/pro/keep-running".into(),
                serde_json::json!({"session_id":session_id,"keep_running":keep_running}),
            ),
        };
        anyhow::ensure!(
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
            "invalid mirror identity"
        );
        daemon_request(&state, "PUT", &path, Some(body)).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await
    .map_err(|error| error.to_string())
}
