//! Real Linux keeper/OpenSSH, synthetic local prompt consumer and account.
#[path = "keeper_password.rs"]
mod password;

use crate::ssh_agent::{
    connect,
    lifecycle::Registry,
    route::ConfigContext,
    trust::{self, Owner},
};
use chimaera_link::ssh_auth::SshAuthFailure as Failure;
use std::{io::Write, path::PathBuf, sync::Arc, time::Duration};
use tokio::{io::AsyncReadExt, time::Instant};

pub(super) async fn run(config: PathBuf, endpoint: String, action: String) -> Result<(), ()> {
    let mixed = matches!(
        action.as_str(),
        "password-accept"
            | "password-wrong"
            | "password-decline"
            | "password-cancel"
            | "password-deadline"
    );
    if !mixed && !matches!(action.as_str(), "accept" | "refuse" | "cancel" | "deadline") {
        return Err(());
    }
    let url = url::Url::parse(&endpoint).map_err(|_| ())?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    let home = config.parent().ok_or(())?.to_owned();
    let context = ConfigContext::fixture(config, home).map_err(|_| ())?;
    let registry = Registry::default();
    let attempt = registry.admit(0).map_err(|_| ())?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let owner = Owner {
        alias: "keeper-route-fixture".into(),
        guard: attempt.native_prompt(deadline),
        account: Arc::default(),
        current: Arc::new(|| true),
        prompt: Arc::new(|prompt, guard| {
            Box::pin(async move {
                if prompt.host_key.is_some() || !guard.active() {
                    return None;
                }
                println!("KEEPER_PROMPT");
                std::io::stdout().flush().ok()?;
                let mut receipt = [0; 9];
                if tokio::io::stdin().read_exact(&mut receipt).await.ok()? != 9
                    || &receipt != b"CONTINUE\n"
                    || !guard.active()
                {
                    return None;
                }
                Some("fixture-only-passphrase".into())
            })
        }),
    };
    let client = chimaera_link::Client::new(
        &endpoint,
        Some(chimaera_link::Tokens {
            access_token: "synthetic-route-device-token".into(),
            refresh_token: "synthetic-route-refresh-token".into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
        }),
    )
    .map_err(|_| ())?;
    let caps = tokio::time::timeout_at(deadline, async {
        client.me().await?;
        client.ssh_auth_capabilities().await
    })
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    if !caps.route_policy_supported() {
        return Err(());
    }
    let selection =
        trust::resolve_fixture("keeper-route-fixture", caps.keeper_boot, owner, context)
            .await
            .map_err(|_| ())?;
    if selection.fixture_deadline() != Some(deadline) || selection.request.legs.len() != 2 {
        return Err(());
    }
    if mixed {
        return password::authenticate(client, selection, registry, attempt, deadline, &action)
            .await;
    }
    if selection
        .request
        .legs
        .iter()
        .any(|leg| leg.mode != chimaera_link::SshRouteMode::Key)
        || selection.request.legs[0].user_keys != selection.request.legs[1].user_keys
    {
        return Err(());
    }
    println!("KEEPER_SELECTED");
    std::io::stdout().flush().map_err(|_| ())?;
    let authenticate =
        connect::authenticate_route(&client, "fixture-host", selection, attempt, || async {
            loop {
                let hosts = client.hosts().await.map_err(|_| Failure::Unavailable)?;
                let row = hosts
                    .iter()
                    .find(|h| h.id == "fixture-host")
                    .ok_or(Failure::InvalidBinding)?;
                if row.status == chimaera_link::HostStatus::Connected
                    && row.daemon.is_none()
                    && row.cluster.is_some()
                {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
    // Only this action receives CANCEL. The runner closes input on timeout and
    // after its final receipt, so no blocking Tokio stdin worker is abandoned.
    let result = if action == "cancel" {
        let cancel = async {
            let mut bytes = [0; 7];
            if tokio::io::stdin().read_exact(&mut bytes).await.is_err() || &bytes != b"CANCEL\n" {
                return Failure::Unavailable;
            }
            registry.advance(1);
            std::future::pending().await
        };
        tokio::select! {biased;failure=cancel=>Err(failure),result=authenticate=>result}
    } else {
        authenticate.await
    };
    println!(
        "{}",
        match &result {
            Ok(()) => "KEEPER_OUTCOME success",
            Err(Failure::Unsupported) => "KEEPER_OUTCOME unsupported",
            Err(Failure::InvalidBinding) => "KEEPER_OUTCOME invalid_binding",
            Err(Failure::InvalidRequest) => "KEEPER_OUTCOME invalid_request",
            Err(Failure::KeyUnavailable) => "KEEPER_OUTCOME key_unavailable",
            Err(Failure::AgentRefused) => "KEEPER_OUTCOME agent_refused",
            Err(Failure::Expired) => "KEEPER_OUTCOME expired",
            Err(Failure::Revoked) => "KEEPER_OUTCOME revoked",
            Err(Failure::Unavailable) => "KEEPER_OUTCOME unavailable",
            Err(Failure::Unknown) => "KEEPER_OUTCOME unknown",
        }
    );
    match (action.as_str(), result) {
        ("accept", Ok(())) => println!("KEEPER_AUTHENTICATED"),
        ("refuse", Err(Failure::AgentRefused)) => println!("KEEPER_REFUSED external"),
        ("cancel", Err(Failure::Revoked)) => println!("KEEPER_REFUSED cancelled"),
        ("deadline", Err(Failure::Expired)) if Instant::now() >= deadline => {
            println!("KEEPER_REFUSED expired")
        }
        _ => return Err(()),
    }
    let generation = if action == "cancel" { 1 } else { 0 };
    let held = (0..3)
        .map(|_| registry.admit(generation).map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()?;
    let returned = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(attempt) = registry.admit(generation) {
                return attempt;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| ())?;
    drop((held, returned));
    Ok(())
}
