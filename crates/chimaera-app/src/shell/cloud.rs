//! Explicit cloud setup actions. Account and daemon credentials stay in Rust.
use super::{pro, Shell};
use chimaera_core::cloud_providers::{provider_auth_origins, provider_definition};
use chimaera_link::{Client, Host, HostKind, LinkTunnel};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{io::Read, time::Duration};
use tauri::{AppHandle, Manager};

#[derive(Serialize)]
pub struct CloudStatus {
    #[serde(flatten)]
    worker: chimaera_link::WorkerStatus,
    /// Additive: whether an agent was connected in the cloud at the last
    /// provider catalog read. Remembered, never probed, so a sleeping cloud
    /// is not woken to answer it. Absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    agents_connected: Option<bool>,
    /// Additive: this account's cloud has been ready before, so `preparing`
    /// now (a service update, say) is not its first setup and the page keeps
    /// the calm available state. Remembered per account.
    cloud_ready_once: bool,
    /// Additive: the provider rows of the last catalog read, which the page
    /// shows at once and replaces when a live read answers. Absent when none
    /// are remembered.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    remembered_providers: Vec<pro::agents::Row>,
}

#[tauri::command]
pub async fn pro_cloud_status(app: AppHandle) -> Result<CloudStatus, String> {
    let state = app.state::<Shell>();
    let (client, generation) = state
        .pro
        .client_snapshot()
        .await
        .ok_or("Sign in to see your cloud status.")?;
    let worker = client.worker_status().await.map_err(|_| {
        "Couldn't check cloud availability. Check your connection and try again.".to_string()
    })?;
    if state.pro.generation() != generation {
        return Err("Your account changed. Refresh your cloud status.".into());
    }
    let remembered = state
        .pro
        .account_id()
        .map(|account| {
            // Ready or asleep, or a registered cloud daemon (another computer
            // may have seen it ready first), means the cloud is set up.
            if set_up(&worker.state, super::lock(&state.pro.hosts).values()) {
                state.pro.agents.mark_ready(&account);
            }
            state.pro.agents.remembered(&account)
        })
        .unwrap_or_default();
    Ok(CloudStatus {
        worker,
        agents_connected: remembered.agents_connected,
        cloud_ready_once: remembered.ready_once,
        remembered_providers: remembered.providers,
    })
}

/// Whether the account's cloud exists: the account says it is ready or
/// asleep, or lists a cloud daemon for it.
fn set_up<'a>(
    state: &chimaera_link::WorkerState,
    mut hosts: impl Iterator<Item = &'a Host>,
) -> bool {
    matches!(
        state,
        chimaera_link::WorkerState::Ready | chimaera_link::WorkerState::Sleeping
    ) || hosts.any(|host| host.kind == HostKind::Worker && host.daemon.is_some())
}

/// What the page may ask of the account's cloud. There is no terminal
/// operation: an older cloud still answers GitHub's connect with a login
/// terminal, which the app never opens (it never shows the cloud's own page);
/// the panel says the cloud is being updated instead, so an
/// `open_provider_terminal` request fails to parse.
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Request {
    Info,
    Start,
    Providers,
    ProviderConnect {
        provider_id: String,
    },
    ProviderDisconnect {
        provider_id: String,
        acknowledge_cloud_work: bool,
    },
    ProviderConnection {
        connection_id: String,
    },
    ProviderCancel {
        connection_id: String,
    },
    ProviderSubmit {
        connection_id: String,
        code: String,
    },
    OpenProviderBrowser {
        connection_id: String,
    },
    ResumeHandoff {
        workspace_id: String,
        expected_epoch: u64,
    },
    Project {
        url: String,
        name: Option<String>,
    },
}

async fn worker(
    app: &AppHandle,
    client: &Client,
    generation: u64,
    wake: bool,
) -> Result<Option<Host>, String> {
    if wake {
        // The status alone reaches the log: the page shows a fixed sentence.
        client.wake_worker().await.map_err(|error| {
            tracing::warn!(%error, "cloud wake request refused");
            "The cloud is unavailable right now. Try again shortly."
        })?;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(if wake { 90 } else { 5 });
    let mut pause = Duration::from_millis(500);
    let mut reconciled: Option<tokio::time::Instant> = None;
    let mut live_check_failed = false;
    loop {
        let state = app.state::<Shell>();
        if state.pro.generation() != generation {
            return Err("Account changed during cloud setup".into());
        }
        // Only an account without a keeper yet needs the account read: it
        // starts the keeper's event stream once one is assigned. At most
        // every 10 s, never a full reconciliation per poll.
        if wake
            && !state.pro.has_keeper()
            && reconciled.is_none_or(|at| at.elapsed() >= Duration::from_secs(10))
        {
            let _ = pro::reconcile_account(app, client, generation).await;
            reconciled = Some(tokio::time::Instant::now());
        }
        // Keeper events keep these rows current; the REST read covers a
        // keeper that is still being assigned or an event stream that is down.
        let cached = super::lock(&state.pro.hosts)
            .values()
            .find(|host| host.kind == HostKind::Worker && host.daemon.is_some())
            .cloned();
        let hosts = worker_rows(cached, live_check_failed, || client.hosts()).await;
        if let Some(host) = hosts
            .as_ref()
            .ok()
            .and_then(|hosts| {
                hosts
                    .iter()
                    .find(|host| host.kind == HostKind::Worker && host.daemon.is_some())
            })
            .cloned()
        {
            if state.pro.generation() != generation {
                return Err("Account changed during cloud setup".into());
            }
            if !wake || live_worker(client, &host).await {
                if !pro::apply_current_host(app, host.clone(), generation).await {
                    return Err("Account changed during cloud setup".into());
                }
                return Ok(Some(host));
            }
            live_check_failed = true;
        }
        if !wake {
            hosts.map_err(|_| "Cloud status is unavailable right now.")?;
            return Ok(None);
        }
        if tokio::time::Instant::now() >= deadline {
            // Still waking: a state the page shows quietly, not a failure.
            tracing::info!(
                live_check_failed,
                listed = hosts.is_ok(),
                "cloud wake wait ran out; the page keeps it as still connecting"
            );
            return Err(CLOUD_ASLEEP.into());
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_secs(5));
    }
}

/// The worker rows for one poll of the wake wait. The event-fed cache saves an
/// account read, but with the event stream down a cached row can be stale (a
/// restarted worker has a new daemon token) and would fail every live check
/// until the wait times out. After one failed check the account's own list is
/// read first; the cached row is only a fallback when that read fails.
async fn worker_rows<F, Fut>(
    cached: Option<Host>,
    live_check_failed: bool,
    read: F,
) -> anyhow::Result<Vec<Host>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Vec<Host>>>,
{
    match cached {
        Some(host) if !live_check_failed => Ok(vec![host]),
        cached => match read().await {
            Ok(hosts) => Ok(hosts),
            Err(error) => cached.map(|host| vec![host]).ok_or(error),
        },
    }
}

// Cached metadata survives VM suspension; require an authenticated live
// response before sending a woken request against a possibly stale bearer.
async fn live_worker(client: &Client, host: &Host) -> bool {
    let Some(daemon) = &host.daemon else {
        return false;
    };
    let Ok(tunnel) = LinkTunnel::bind(client.clone(), host.id.clone()).await else {
        return false;
    };
    let url = format!("http://127.0.0.1:{}/api/v1/health", tunnel.local_port);
    let token = daemon.token.clone();
    let result = tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(3)))
            .max_redirects(0)
            .build()
            .into();
        agent
            .get(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("X-Chimaera-Wake", "interaction")
            .call()
            .is_ok_and(|response| {
                response
                    .headers()
                    .get("x-chimaera-worker-state")
                    .is_none_or(|state| state != "sleeping")
            })
    })
    .await
    .unwrap_or(false);
    drop(tunnel);
    result
}

#[tauri::command]
pub async fn pro_cloud_request(app: AppHandle, request: Request) -> Result<Value, String> {
    if let Request::ProviderDisconnect {
        provider_id,
        acknowledge_cloud_work,
    } = &request
    {
        if !acknowledge_cloud_work || provider_definition(provider_id).is_none() {
            return Err("Confirm which cloud connection to disconnect.".into());
        }
    }
    let state = app.state::<Shell>();
    let (client, generation) = state.pro.client_snapshot().await.ok_or("Sign in first")?;
    let wake = matches!(
        request,
        Request::Start
            | Request::Project { .. }
            | Request::ProviderConnect { .. }
            | Request::ProviderDisconnect { .. }
            | Request::ResumeHandoff { .. }
    );
    let account_mutation = matches!(
        request,
        Request::ProviderSubmit { .. } | Request::ProviderDisconnect { .. }
    );
    let open_browser = matches!(request, Request::OpenProviderBrowser { .. });
    let catalog = matches!(request, Request::Providers);
    let timeout = match &request {
        Request::ResumeHandoff { .. } => 1200,
        Request::Project { .. } => 310,
        _ => 40,
    };
    let Some(host) = worker(&app, &client, generation, wake).await? else {
        return Ok(json!({"available":false}));
    };
    // Credential submission and removal must stay bound to the same account
    // while the blocking HTTP client sends the explicitly authorized operation.
    let account_operation = if account_mutation {
        Some(state.pro.operation.clone().lock_owned().await)
    } else {
        None
    };
    if account_mutation && state.pro.generation() != generation {
        return Err("Your account changed. Start sign-in again.".into());
    }
    let (route, body): (String, Option<Value>) = match request {
        Request::Info | Request::Start => ("cloud".into(), None),
        Request::Providers => ("cloud/providers".into(), None),
        Request::ProviderConnect { provider_id } if provider_definition(&provider_id).is_some() => {
            (
                format!("cloud/providers/{provider_id}/connect"),
                Some(json!({})),
            )
        }
        Request::ProviderConnect { .. } => return Err("This provider is not supported yet.".into()),
        Request::ProviderDisconnect {
            provider_id,
            acknowledge_cloud_work: true,
        } if provider_definition(&provider_id).is_some() => (
            format!("cloud/providers/{provider_id}/disconnect"),
            Some(json!({"acknowledge_cloud_work":true})),
        ),
        Request::ProviderDisconnect { .. } => {
            return Err("Confirm which cloud connection to disconnect.".into())
        }
        Request::ProviderConnection { connection_id }
        | Request::OpenProviderBrowser { connection_id }
            if valid_id(&connection_id) =>
        {
            (format!("cloud/connections/{connection_id}"), None)
        }
        Request::ProviderCancel { connection_id } if valid_id(&connection_id) => (
            format!("cloud/connections/{connection_id}/cancel"),
            Some(json!({})),
        ),
        Request::ProviderSubmit {
            connection_id,
            code,
        } if valid_id(&connection_id)
            && !code.is_empty()
            && code.len() <= 4096
            && !code.chars().any(|c| c.is_control() || c.is_whitespace()) =>
        {
            (
                format!("cloud/connections/{connection_id}/input"),
                Some(json!({"code":code})),
            )
        }
        Request::ResumeHandoff {
            workspace_id,
            expected_epoch,
        } if valid_id(&workspace_id) => (
            "hydrate".into(),
            Some(json!({"workspace_id":workspace_id,"expected_epoch":expected_epoch})),
        ),
        Request::Project { url, name }
            if url.len() <= 4096 && name.as_ref().is_none_or(|name| name.len() <= 80) =>
        {
            ("cloud/project".into(), Some(json!({"url":url,"name":name})))
        }
        Request::Project { .. } => return Err("Repository URL or project name is too long".into()),
        _ => return Err("This connection is no longer available. Start again.".into()),
    };
    let tunnel = LinkTunnel::bind(client, host.id.clone())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "cloud request could not open its tunnel");
            SETUP_FAILED
        })?;
    let url = format!("http://127.0.0.1:{}/api/v1/pro/{route}", tunnel.local_port);
    let token = host
        .daemon
        .as_ref()
        .ok_or("Cloud machine is not ready")?
        .token
        .clone();
    let result = blocking_request(
        account_operation,
        tunnel,
        move || -> Result<Value, String> {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(timeout)))
                .max_redirects(0)
                .http_status_as_error(false)
                .build()
                .into();
            let mut response = if let Some(body) = body {
                agent
                    .post(&url)
                    .header("Authorization", &format!("Bearer {token}"))
                    .send_json(body)
            } else {
                agent
                    .get(&url)
                    .header("Authorization", &format!("Bearer {token}"))
                    .call()
            }
            .map_err(|error| {
                // A transport failure only; the daemon's answer, when there is
                // one, is read below and never logged.
                tracing::warn!(%route, %error, "cloud request failed in transit");
                SETUP_FAILED
            })?;
            let status = response.status();
            if !status.is_success() {
                tracing::info!(%route, status = status.as_u16(), "cloud request refused");
            }
            let sleeping = response
                .headers()
                .get("x-chimaera-worker-state")
                .is_some_and(|state| state == "sleeping");
            let mut bytes = Vec::new();
            response
                .body_mut()
                .as_reader()
                .take(64 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| SETUP_FAILED)?;
            if bytes.len() > 64 * 1024 {
                return Err("Cloud setup response exceeds limit".into());
            }
            if asleep_answer(status.as_u16(), sleeping, &bytes) {
                return Err(CLOUD_ASLEEP.into());
            }
            let value = response_value(status.as_u16(), &bytes)?;
            if !status.is_success() {
                return Err(match value["error"].as_str() {
                    Some("cloud_provider_not_ready") => {
                        "Connect the agent required by this project before continuing."
                    }
                    Some("provider_unavailable") => "This provider cannot connect here yet.",
                    // This fixed code is mapped to copy by the typed UI. Never
                    // forward arbitrary provider errors or CLI output.
                    Some("provider_busy") => "provider_busy",
                    // Half a pasted `code#state`: the attempt stays open, the UI
                    // asks for the whole code.
                    Some("authorization_code_incomplete") => "authorization_code_incomplete",
                    _ => SETUP_FAILED,
                }
                .into());
            }
            Ok(value)
        },
    )
    .await
    .map_err(|_| SETUP_FAILED)?;
    if state.pro.generation() != generation {
        return Err("Account changed during cloud setup".into());
    }
    let mut value = result?;
    if catalog {
        if let Some(account) = state.pro.account_id() {
            state.pro.agents.record(&account, &value);
        }
    }
    if open_browser {
        // Only a provider's validated sign-in page, in the user's own
        // browser. Nothing here ever opens a window on the cloud.
        let url = connection_browser_url(active_connection(&value)?)?;
        let _operation = state.pro.operation.lock().await;
        if state.pro.generation() != generation {
            return Err("Your account changed. Start the connection again.".into());
        }
        tokio::task::spawn_blocking(move || open::that(url.as_str()))
            .await
            .map_err(|_| "Couldn't open your browser. Try again.")?
            .map_err(|_| "Couldn't open your browser. Try again.")?;
    }
    value["host_alias"] = Value::String(host.alias);
    Ok(value)
}

const SETUP_FAILED: &str = "Couldn't complete cloud setup. Try again shortly.";
/// The fixed code for a cloud machine that is asleep or still starting. The
/// page maps it to a quiet line ("waking up" / "asleep"), never an error.
const CLOUD_ASLEEP: &str = "cloud_asleep";

/// A canceled IPC future cannot release the account fence or close the route
/// while its already-admitted blocking request is still sending credentials.
async fn blocking_request<T: Send + 'static, R: Send + 'static>(
    account_operation: Option<tokio::sync::OwnedMutexGuard<()>>,
    tunnel: T,
    send: impl FnOnce() -> R + Send + 'static,
) -> Result<R, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        let _account_operation = account_operation;
        let _tunnel = tunnel;
        send()
    })
    .await
}

/// A sleeping or starting cloud machine answers through its transport: 503
/// `worker_asleep` / `worker_unavailable`, or an answer marked
/// `X-Chimaera-Worker-State: sleeping` (a cache reply, never live setup data).
fn asleep_answer(status: u16, sleeping: bool, body: &[u8]) -> bool {
    sleeping
        || status == 503
            && serde_json::from_slice::<Value>(body).is_ok_and(|value| {
                matches!(
                    value["error"].as_str(),
                    Some("worker_asleep" | "worker_unavailable")
                )
            })
}

fn response_value(status: u16, bytes: &[u8]) -> Result<Value, String> {
    if status == 204 && bytes.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(bytes).map_err(|_| "Cloud setup response unavailable".into())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn active_connection(value: &Value) -> Result<&Value, String> {
    let connection = &value["connection"];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if connection["operation"].as_str() == Some("disconnect")
        || !matches!(
            connection["phase"].as_str(),
            Some("preparing" | "waiting" | "verifying")
        )
        || connection["expires_at"]
            .as_u64()
            .is_none_or(|expiry| expiry <= now)
        || connection["provider_id"]
            .as_str()
            .and_then(provider_definition)
            .is_none()
    {
        return Err("This connection has ended. Start again if needed.".into());
    }
    Ok(connection)
}

fn connection_browser_url(connection: &Value) -> Result<url::Url, String> {
    let action = &connection["action"];
    let value = match action["type"].as_str() {
        Some("device_code") => action["verification_url"].as_str(),
        Some("browser") => action["url"].as_str(),
        _ => None,
    }
    .ok_or("This connection doesn't have a browser step.")?;
    let provider = connection["provider_id"].as_str().unwrap_or_default();
    let url = url::Url::parse(value).map_err(|_| "The provider link couldn't be verified.")?;
    if value.len() > 4096
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port().is_some_and(|port| port != 443)
        || !provider_auth_origins(provider).contains(&url.origin().ascii_serialization())
    {
        return Err("The provider link couldn't be verified.".into());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn canceled_submission_retains_account_fence_and_route_until_http_finishes() {
        use std::io::Write;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };

        struct Route(Arc<AtomicBool>);
        impl Drop for Route {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/submit", listener.local_addr().unwrap());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = tokio::task::spawn_blocking(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 4096);
            }
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let operation = Arc::new(tokio::sync::Mutex::new(()));
        let guard = operation.clone().lock_owned().await;
        let route_closed = Arc::new(AtomicBool::new(false));
        let route = Route(route_closed.clone());
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let command = tokio::spawn(async move {
            blocking_request(Some(guard), route, move || {
                let agent: ureq::Agent = ureq::Agent::config_builder()
                    .timeout_global(Some(Duration::from_secs(10)))
                    .build()
                    .into();
                let response = agent.post(&url).send_empty().unwrap();
                assert_eq!(response.status(), 204);
                let _ = finished_tx.send(());
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), entered_rx)
            .await
            .unwrap()
            .unwrap();
        command.abort();
        assert!(command.await.unwrap_err().is_cancelled());
        assert!(operation.try_lock().is_err(), "sign-out must still wait");
        assert!(!route_closed.load(Ordering::Acquire));
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), finished_rx)
            .await
            .unwrap()
            .unwrap();
        let _replacement = tokio::time::timeout(Duration::from_secs(5), operation.lock())
            .await
            .unwrap();
        assert!(route_closed.load(Ordering::Acquire));
        server.await.unwrap();
    }

    fn worker(token: &str) -> Host {
        Host {
            id: "w-1".into(),
            alias: "cloud".into(),
            kind: HostKind::Worker,
            status: chimaera_link::HostStatus::Connected,
            daemon: Some(chimaera_link::Daemon {
                token: token.into(),
                build: "b".into(),
                sessions: 0,
            }),
            error: None,
            cluster: None,
        }
    }

    fn token(rows: anyhow::Result<Vec<Host>>) -> String {
        rows.unwrap()[0].daemon.as_ref().unwrap().token.clone()
    }

    #[tokio::test]
    async fn a_cached_worker_row_that_failed_its_live_check_yields_to_the_account() {
        let unread = || async { unreachable!("a fresh cached row needs no account read") };
        assert_eq!(
            token(worker_rows(Some(worker("cached")), false, unread).await),
            "cached"
        );
        let fresh = || async { Ok(vec![worker("fresh")]) };
        assert_eq!(
            token(worker_rows(Some(worker("cached")), true, fresh).await),
            "fresh"
        );
        let down = || async { Err(anyhow::anyhow!("account unavailable")) };
        assert_eq!(
            token(worker_rows(Some(worker("cached")), true, down).await),
            "cached"
        );
        let fresh = || async { Ok(vec![worker("fresh")]) };
        assert_eq!(token(worker_rows(None, false, fresh).await), "fresh");
        let down = || async { Err(anyhow::anyhow!("account unavailable")) };
        assert!(worker_rows(None, true, down).await.is_err());
    }

    #[test]
    fn a_cloud_is_set_up_once_ready_asleep_or_registered() {
        use chimaera_link::WorkerState;
        for ready in [WorkerState::Ready, WorkerState::Sleeping] {
            assert!(set_up(&ready, [].iter()));
        }
        for other in [
            WorkerState::Preparing,
            WorkerState::NoPlan,
            WorkerState::Unavailable,
            WorkerState::Limited,
            WorkerState::Error,
            WorkerState::Unknown,
        ] {
            assert!(!set_up(&other, [].iter()));
        }
        // A service update can say `preparing` for a cloud that exists.
        assert!(set_up(&WorkerState::Preparing, [worker("t")].iter()));
        let mut unregistered = worker("t");
        unregistered.daemon = None;
        assert!(!set_up(&WorkerState::Preparing, [unregistered].iter()));
    }

    #[test]
    fn successful_handoff_without_content_is_not_a_parse_failure() {
        assert_eq!(response_value(204, &[]).unwrap(), json!({}));
        assert!(response_value(200, &[]).is_err());
        assert!(response_value(503, &[]).is_err());
    }

    #[test]
    fn a_sleeping_or_starting_cloud_machine_is_told_apart_from_a_failure() {
        for body in [
            r#"{"error":"worker_asleep"}"#,
            r#"{"error":"worker_unavailable"}"#,
        ] {
            assert!(asleep_answer(503, false, body.as_bytes()));
            assert!(!asleep_answer(500, false, body.as_bytes()));
        }
        assert!(asleep_answer(503, true, b""));
        assert!(asleep_answer(200, true, br#"{"available":true}"#));
        for body in [
            &br#"{"error":"provider_busy"}"#[..],
            b"<html>bad gateway</html>",
            b"",
        ] {
            assert!(!asleep_answer(503, false, body));
        }
        assert!(!asleep_answer(409, false, br#"{"error":"provider_busy"}"#));
    }

    #[test]
    fn provider_browser_policy_is_exact_and_provider_scoped() {
        let mut connection = json!({"provider_id":"codex","action":{"type":"device_code","verification_url":"https://auth.openai.com/codex/device"}});
        assert!(connection_browser_url(&connection).is_ok());
        for invalid in [
            "http://auth.openai.com/device",
            "https://auth.openai.com.evil.test/",
            "https://user@auth.openai.com/",
            "https://auth.openai.com:444/",
            "https://auth.openai.com/#code",
            "file:///tmp/auth",
            "javascript:alert(1)",
        ] {
            connection["action"]["verification_url"] = json!(invalid);
            assert!(connection_browser_url(&connection).is_err());
        }
        connection["action"]["verification_url"] = json!("https://auth.openai.com/device");
        connection["provider_id"] = json!("future-provider");
        assert!(connection_browser_url(&connection).is_err());
        // GitHub's one-time code opens only GitHub's own page.
        let mut github = json!({"provider_id":"github","action":{"type":"device_code","verification_url":"https://github.com/login/device"}});
        assert!(connection_browser_url(&github).is_ok());
        for invalid in [
            "https://github.com.evil.test/login/device",
            "https://auth.openai.com/codex/device",
            "https://user@github.com/login/device",
        ] {
            github["action"]["verification_url"] = json!(invalid);
            assert!(connection_browser_url(&github).is_err());
        }
    }

    #[test]
    fn an_older_clouds_terminal_sign_in_is_never_opened() {
        // The page has no terminal operation to ask for; the request is
        // refused before anything reaches the cloud.
        assert!(serde_json::from_value::<Request>(
            json!({"operation":"open_provider_terminal","connection_id":"attempt-1"})
        )
        .is_err());
        assert!(matches!(
            serde_json::from_value::<Request>(
                json!({"operation":"open_provider_browser","connection_id":"attempt-1"})
            ),
            Ok(Request::OpenProviderBrowser { .. })
        ));
        // Nor does the browser step open anything for an older cloud's
        // GitHub sign-in, which answers with a login terminal.
        let older = json!({"provider_id":"github","phase":"waiting","expires_at":u64::MAX,
            "action":{"type":"terminal","workspace_id":"setup","session_id":"login"}});
        assert!(connection_browser_url(&older).is_err());
    }

    #[test]
    fn stale_and_finished_connections_cannot_open_actions() {
        for phase in ["connected", "disconnected", "failed", "canceled", "expired"] {
            assert!(active_connection(
                &json!({"connection":{"provider_id":"codex","phase":phase,"expires_at":u64::MAX}})
            )
            .is_err());
        }
        assert!(active_connection(
            &json!({"connection":{"provider_id":"codex","phase":"waiting","expires_at":1}})
        )
        .is_err());
        assert!(active_connection(
            &json!({"connection":{"provider_id":"codex","phase":"waiting","expires_at":u64::MAX}})
        )
        .is_ok());
        assert!(active_connection(
            &json!({"connection":{"provider_id":"codex","operation":"disconnect","phase":"verifying","expires_at":u64::MAX}})
        ).is_err());
        for id in ["", "../sessions", "connection?token", "one/two"] {
            assert!(!valid_id(id));
        }
    }
}
