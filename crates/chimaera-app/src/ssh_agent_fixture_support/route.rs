//! Production native selection/authentication against a simulated loopback keeper.
//! No sshd, Tauri UI, signing-source or held-job acceptance is claimed here.
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
    let home = config.parent().ok_or(())?.to_path_buf();
    let context = ConfigContext::fixture(config, home).map_err(|_| ())?;
    let registry = Registry::default();
    let attempt = registry.admit(0).map_err(|_| ())?;
    // The original owner starts before capability discovery and encrypted load;
    // selection, grant, Ready and reconnect may never manufacture a new lifetime.
    let deadline = Instant::now() + Duration::from_secs(8);
    let owner = Owner {
        alias: "route-fixture".into(),
        guard: attempt.native_prompt(deadline),
        account: Arc::default(),
        current: Arc::new(|| true),
        prompt: Arc::new(|prompt, guard| {
            Box::pin(async move {
                if prompt.host_key.is_some() || !guard.active() {
                    return None;
                }
                println!("ROUTE_PROMPT");
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
    let selection = trust::resolve_fixture("route-fixture", caps.keeper_boot, owner, context).await;
    if action == "config-change" {
        if selection.is_ok() {
            return Err(());
        }
        println!("ROUTE_REFUSED config");
        return Ok(());
    }
    let selection = selection.map_err(|_| ())?;
    if selection.fixture_deadline() != Some(deadline)
        || selection.request.legs.len() != 1
        || selection.request.legs[0].mode != chimaera_link::SshRouteMode::Key
    {
        return Err(());
    }
    println!("ROUTE_SELECTED");
    std::io::stdout().flush().map_err(|_| ())?;
    let authenticate =
        connect::authenticate_route(&client, "fixture-host", selection, attempt, || async {
            Ok(())
        });
    // Tokio stdin owns a blocking read that dropping a future cannot cancel.
    // Only the action whose runner supplies and closes CANCEL may create it.
    let result = if action == "grant-cancel" {
        let cancel = async {
            let mut receipt = [0; 7];
            if tokio::io::stdin().read_exact(&mut receipt).await.is_err() || &receipt != b"CANCEL\n"
            {
                return Failure::Unavailable;
            }
            registry.advance(1);
            std::future::pending().await
        };
        tokio::select! {
            biased;
            failure = cancel => Err(failure),
            result = authenticate => result,
        }
    } else {
        authenticate.await
    };
    match (action.as_str(), result) {
        ("accept", Ok(())) => println!("ROUTE_ACCEPTED"),
        ("grant-expiry" | "ready-expiry", Err(Failure::Expired)) => {
            // A newly started grant lifetime would expire later than this owner.
            if Instant::now() < deadline {
                return Err(());
            }
            println!("ROUTE_REFUSED expiry");
        }
        ("grant-cancel", Err(Failure::Revoked)) => println!("ROUTE_REFUSED revoked"),
        _ => return Err(()),
    }
    // Detached DELETE owns the original slot until the actual HTTP reply. Fill
    // the other slots, then positively wait for that precise capacity to return.
    let generation = if action == "grant-cancel" { 1 } else { 0 };
    let held = (0..3)
        .map(|_| registry.admit(generation).map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()?;
    let restored = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(attempt) = registry.admit(generation) {
                return attempt;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| ())?;
    drop((held, restored));
    Ok(())
}
