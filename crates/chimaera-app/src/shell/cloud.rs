//! Explicit cloud setup actions. Account and daemon credentials stay in Rust.
use super::{pro, Shell};
use chimaera_core::cloud_providers::{provider_auth_origins, provider_definition};
use chimaera_link::{Client, Host, HostKind, LinkTunnel};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{io::Read, time::Duration};
use tauri::{AppHandle, Manager};

#[tauri::command]
pub async fn pro_cloud_status(app: AppHandle) -> Result<chimaera_link::WorkerStatus, String> {
    let state = app.state::<Shell>();
    let (client, generation) = state
        .pro
        .client_snapshot()
        .await
        .ok_or("Sign in to see your cloud status.")?;
    let status = client.worker_status().await.map_err(|_| {
        "Couldn't check cloud availability. Check your connection and try again.".to_string()
    })?;
    if state.pro.generation() != generation {
        return Err("Your account changed. Refresh your cloud status.".into());
    }
    Ok(status)
}

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
    OpenProviderTerminal {
        connection_id: String,
    },
    ResumeHandoff {
        workspace_id: String,
        expected_epoch: u64,
    },
    Onboard {
        agent: String,
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
        client
            .wake_worker()
            .await
            .map_err(|_| "The cloud is unavailable right now. Try again shortly.")?;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(if wake { 90 } else { 5 });
    loop {
        let state = app.state::<Shell>();
        if state.pro.generation() != generation {
            return Err("Account changed during cloud setup".into());
        }
        if wake {
            let _ = pro::reconcile_account(app, client, generation).await;
        }
        let hosts = client.hosts().await;
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
        }
        if !wake {
            hosts.map_err(|_| "Cloud status is unavailable right now.")?;
            return Ok(None);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("Cloud machine is still starting. Try again shortly.".into());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

// Cached metadata survives VM suspension; require an authenticated live
// response before opening a login terminal against a possibly stale bearer.
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
            | Request::Onboard { .. }
            | Request::Project { .. }
            | Request::ProviderConnect { .. }
            | Request::ProviderDisconnect { .. }
            | Request::OpenProviderTerminal { .. }
            | Request::ResumeHandoff { .. }
    );
    let account_mutation = matches!(
        request,
        Request::ProviderSubmit { .. } | Request::ProviderDisconnect { .. }
    );
    let open_browser = matches!(request, Request::OpenProviderBrowser { .. });
    let open_terminal = matches!(request, Request::OpenProviderTerminal { .. });
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
    let _account_operation = if account_mutation {
        Some(state.pro.operation.lock().await)
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
        | Request::OpenProviderTerminal { connection_id }
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
        Request::Onboard { agent } if provider_definition(&agent).is_some() => {
            ("cloud/onboard".into(), Some(json!({"agent":agent})))
        }
        Request::Onboard { .. } => return Err("This provider is not supported yet.".into()),
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
        .map_err(|_| SETUP_FAILED)?;
    let url = format!("http://127.0.0.1:{}/api/v1/pro/{route}", tunnel.local_port);
    let token = host
        .daemon
        .as_ref()
        .ok_or("Cloud machine is not ready")?
        .token
        .clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
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
        .map_err(|_| SETUP_FAILED)?;
        let status = response.status();
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
                _ => SETUP_FAILED,
            }
            .into());
        }
        Ok(value)
    })
    .await
    .map_err(|_| SETUP_FAILED)?;
    drop(tunnel);
    if state.pro.generation() != generation {
        return Err("Account changed during cloud setup".into());
    }
    let mut value = result?;
    if open_browser || open_terminal {
        let connection = active_connection(&value)?;
        if open_terminal {
            let action = &connection["action"];
            if action["type"] != "terminal" {
                return Err("This connection doesn't need a terminal.".into());
            }
            let workspace = action["workspace_id"]
                .as_str()
                .filter(|id| valid_id(id))
                .ok_or("The connection workspace is unavailable.")?
                .to_owned();
            let session = action["session_id"]
                .as_str()
                .filter(|id| valid_id(id))
                .ok_or("The connection session is unavailable.")?
                .to_owned();
            let _operation = state.pro.operation.lock().await;
            if state.pro.generation() != generation {
                return Err("Your account changed. Start the connection again.".into());
            }
            // Resolving the alias may reconnect saved windows. Bind that side
            // effect to the same account as the server-owned login session.
            tokio::time::timeout(
                Duration::from_secs(20),
                super::connect::do_connect(&app, host.alias.clone(), false),
            )
            .await
            .map_err(|_| "The cloud connection timed out. Try again.")??;
            super::notices::open_session(&app, host.alias.clone(), workspace, session).await?;
        } else {
            let url = connection_browser_url(connection)?;
            let _operation = state.pro.operation.lock().await;
            if state.pro.generation() != generation {
                return Err("Your account changed. Start the connection again.".into());
            }
            tokio::task::spawn_blocking(move || open::that(url.as_str()))
                .await
                .map_err(|_| "Couldn't open your browser. Try again.")?
                .map_err(|_| "Couldn't open your browser. Try again.")?;
        }
    }
    value["host_alias"] = Value::String(host.alias);
    Ok(value)
}

const SETUP_FAILED: &str = "Couldn't complete cloud setup. Try again shortly.";

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

    #[test]
    fn successful_handoff_without_content_is_not_a_parse_failure() {
        assert_eq!(response_value(204, &[]).unwrap(), json!({}));
        assert!(response_value(200, &[]).is_err());
        assert!(response_value(503, &[]).is_err());
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
