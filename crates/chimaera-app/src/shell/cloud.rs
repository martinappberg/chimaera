//! Explicit cloud setup actions. Account and daemon credentials stay in Rust.
use super::{pro, Shell};
use chimaera_link::{Client, Host, HostKind, LinkTunnel};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{io::Read, time::Duration};
use tauri::{AppHandle, Manager};

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Request {
    Info,
    Start,
    Onboard { agent: String },
    Project { url: String, name: Option<String> },
}

async fn worker(
    app: &AppHandle,
    client: &Client,
    generation: u64,
    wake: bool,
) -> Result<Option<Host>, String> {
    if wake {
        client.wake_worker().await.map_err(|e| e.to_string())?;
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
                pro::apply_host(app, host.clone()).await;
                return Ok(Some(host));
            }
        }
        if !wake {
            hosts.map_err(|e| e.to_string())?;
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
    let state = app.state::<Shell>();
    let client = state.pro.client().await.ok_or("Sign in first")?;
    let generation = state.pro.generation();
    let wake = !matches!(request, Request::Info);
    let Some(host) = worker(&app, &client, generation, wake).await? else {
        return Ok(json!({"available":false}));
    };
    let (route, body) = match request {
        Request::Info | Request::Start => ("cloud", None),
        Request::Onboard { agent } if matches!(agent.as_str(), "claude" | "codex" | "github") => {
            ("cloud/onboard", Some(json!({"agent":agent})))
        }
        Request::Onboard { .. } => return Err("Choose Claude, Codex or GitHub".into()),
        Request::Project { url, name }
            if url.len() <= 4096 && name.as_ref().is_none_or(|name| name.len() <= 80) =>
        {
            ("cloud/project", Some(json!({"url":url,"name":name})))
        }
        Request::Project { .. } => return Err("Repository URL or project name is too long".into()),
    };
    let tunnel = LinkTunnel::bind(client, host.id.clone())
        .await
        .map_err(|e| e.to_string())?;
    let url = format!("http://127.0.0.1:{}/api/v1/pro/{route}", tunnel.local_port);
    let token = host
        .daemon
        .as_ref()
        .ok_or("Cloud machine is not ready")?
        .token
        .clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(310)))
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
        .map_err(|e| e.to_string())?;
        let status = response.status();
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 64 * 1024 {
            return Err("Cloud setup response exceeds limit".into());
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Cloud setup response unavailable")?;
        if !status.is_success() {
            return Err(value["error"]
                .as_str()
                .unwrap_or("Cloud setup failed")
                .to_owned());
        }
        Ok(value)
    })
    .await
    .map_err(|e| e.to_string())?;
    drop(tunnel);
    if state.pro.generation() != generation {
        return Err("Account changed during cloud setup".into());
    }
    let mut value = result?;
    value["host_alias"] = Value::String(host.alias);
    Ok(value)
}
