//! Actual keeper events -> original route prompt guard -> synthetic UI answer.
//! No prompt prose/type inference, PAM or keyboard-interactive claim.
use crate::ssh_agent::{
    connect,
    lifecycle::{Attempt, Registry},
    route::RouteSelection,
};
use chimaera_link::ssh_auth::SshAuthFailure as Failure;
use chimaera_link::{Client, Event, EventCommand, HostStatus, SshRouteMode};
use std::{
    io::Write,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{io::AsyncReadExt, time::Instant};

async fn command() -> Result<Vec<u8>, ()> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        tokio::io::stdin()
            .read_exact(&mut byte)
            .await
            .map_err(|_| ())?;
        if byte[0] == b'\n' {
            return Ok(bytes);
        }
        if bytes.len() >= 7 || !byte[0].is_ascii_uppercase() {
            return Err(());
        }
        bytes.push(byte[0]);
    }
}
pub(super) async fn authenticate(
    client: Client,
    selection: RouteSelection,
    registry: Registry,
    attempt: Attempt,
    deadline: Instant,
    action: &str,
) -> Result<(), ()> {
    let legs = &selection.request.legs;
    if legs[0].mode != SshRouteMode::Key
        || legs[1].mode != SshRouteMode::Interactive
        || !legs[1].user_keys.is_empty()
        || legs[0].user_keys.is_empty()
        || legs.iter().any(|leg| leg.policy.is_none())
    {
        return Err(());
    }
    let target = legs[1].destination.clone();
    let prompts = Arc::new(AtomicU8::new(0));
    let observed = prompts.clone();
    // Production Link parses frames and owns connection-local answer aliases.
    // The same Registry used by Connect alone grants exact-leg UI authority.
    let mut connection = client.events();
    let consume = async {
        while let Some(event) = connection.events.recv().await {
            let event = event.map_err(|_| Failure::Unavailable)?;
            if let Event::Prompt {
                id,
                host_id,
                ssh_route_auth,
                echo,
                ..
            } = event
            {
                let auth = ssh_route_auth.ok_or(Failure::InvalidBinding)?;
                let guard = registry
                    .route_prompt(0, &host_id, &auth)
                    .ok_or(Failure::Revoked)?;
                if host_id != "fixture-host"
                    || auth.leg != 1
                    || auth.mode != SshRouteMode::Interactive
                    || echo
                    || auth.destination != target
                    || !guard.active()
                {
                    return Err(Failure::InvalidBinding);
                }
                let count = prompts.fetch_add(1, Ordering::SeqCst);
                if count >= 3 {
                    return Err(Failure::Unavailable);
                }
                let value = if count == 0 {
                    println!("KEEPER_REMOTE_PROMPT");
                    std::io::stdout()
                        .flush()
                        .map_err(|_| Failure::Unavailable)?;
                    // Every action supplies a complete fixed command and closes
                    // stdin. No uncancelable read is started for a withheld answer.
                    let receipt = command().await.map_err(|_| Failure::Unavailable)?;
                    match (action, receipt.as_slice()) {
                        ("password-accept", b"GOOD") => Some("fixture-password-only".into()),
                        ("password-wrong", b"BAD") => Some("fixture-wrong-password".into()),
                        ("password-decline", b"DECLINE") => None,
                        ("password-cancel", b"CANCEL") => {
                            registry.advance(1);
                            std::future::pending::<Option<String>>().await
                        }
                        ("password-deadline", b"HOLD") => {
                            guard.stopped().await;
                            std::future::pending::<Option<String>>().await
                        }
                        _ => return Err(Failure::InvalidRequest),
                    }
                } else {
                    None
                };
                if !guard.active() {
                    return Err(Failure::Revoked);
                }
                connection
                    .commands
                    .send(EventCommand::Answer { id, value })
                    .await
                    .map_err(|_| Failure::Unavailable)?;
            }
        }
        Err(Failure::Unavailable)
    };
    let authenticate =
        connect::authenticate_route(&client, "fixture-host", selection, attempt, || async {
            loop {
                let hosts = client.hosts().await.map_err(|_| Failure::Unavailable)?;
                let row = hosts
                    .iter()
                    .find(|host| host.id == "fixture-host")
                    .ok_or(Failure::InvalidBinding)?;
                if row.status == HostStatus::Connected
                    && row.daemon.is_none()
                    && row.cluster.is_some()
                {
                    return Ok(());
                }
                if observed.load(Ordering::SeqCst) > 0 && row.status == HostStatus::Offline {
                    return Err(Failure::AgentRefused);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
    let result = tokio::select! {
        biased;
        result = authenticate => result,
        result = consume => result,
    };
    if prompts.load(Ordering::SeqCst) == 0 {
        return Err(());
    }
    match (action, result) {
        ("password-accept", Ok(())) => println!("KEEPER_AUTHENTICATED"),
        ("password-wrong" | "password-decline", Err(Failure::AgentRefused)) => {
            println!("KEEPER_REFUSED password")
        }
        ("password-cancel", Err(Failure::Revoked)) => println!("KEEPER_REFUSED cancelled"),
        ("password-deadline", Err(Failure::Expired)) if Instant::now() >= deadline => {
            println!("KEEPER_REFUSED expired")
        }
        _ => return Err(()),
    }
    // The authentication owner may clean up detached HTTP leases, but it must
    // eventually return the actual shared route capacity after each outcome.
    let generation = u64::from(action == "password-cancel");
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
