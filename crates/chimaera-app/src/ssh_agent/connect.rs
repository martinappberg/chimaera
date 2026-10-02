//! Explicit native Connect owns the short authentication channel. Job/master
//! lifetime is owned by the keeper and does not depend on this grant or task.
use super::{control, lifecycle::Attempt, selection::Selection, Failure};
use chimaera_link::{Client, SshAuthGrant};
use std::{future::Future, time::Duration};
use tokio::time::{timeout_at, Instant};

struct GrantLease {
    client: Client,
    host: String,
    grant: SshAuthGrant,
    attempt: Option<Attempt>,
}
impl Drop for GrantLease {
    fn drop(&mut self) {
        // Only inert/authentication authority is removed here. A lost HTTP
        // reply is bounded by the server's absolute grant lifetime; no retry
        // can start SSH or select another host. The socket is separately owned
        // by the Connect future and is dropped synchronously on cancellation.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let client = self.client.clone();
        let host = self.host.clone();
        let grant = self.grant.clone();
        let attempt = self.attempt.take();
        runtime.spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                client.delete_ssh_auth_grant(&host, &grant),
            )
            .await;
            drop(attempt);
        });
    }
}
fn failure(error: anyhow::Error) -> Failure {
    if error.is::<chimaera_link::ServiceUnsupported>() {
        Failure::Unsupported
    } else if error.is::<chimaera_link::AuthorizationRevoked>() {
        Failure::Revoked
    } else {
        Failure::Unavailable
    }
}

/// `finish` waits for the same keeper-owned Connect result. It is invoked only
/// after exact Ready and accepted grant-bound Reconnect, never on passive reads.
/// The attempt remains owned here through that outcome; account change or owner
/// loss cancels pending local signing without touching established SSH/jobs.
pub(crate) async fn authenticate<T, F, Fut>(
    client: &Client,
    host: &str,
    selection: Selection,
    attempt: Attempt,
    finish: F,
) -> Result<T, Failure>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, Failure>>,
{
    let started = Instant::now();
    let max_deadline = started + Duration::from_secs(chimaera_link::SSH_AUTH_LIFETIME.into());
    let (request, mut verifier) = selection.verifier(max_deadline)?;
    let mut cancellation = attempt.cancellation();
    let control_cancellation = attempt.cancellation();
    let effect = async {
        let grant = client
            .create_ssh_auth_grant(host, &request)
            .await
            .map_err(failure)?;
        let deadline = started + Duration::from_secs(grant.expires_in.into());
        let lease = GrantLease {
            client: client.clone(),
            host: host.into(),
            grant,
            attempt: Some(attempt),
        };
        verifier.deadline = deadline;
        let socket = timeout_at(
            deadline,
            client.ssh_auth_socket(host, &lease.grant, &request.keeper_boot),
        )
        .await
        .map_err(|_| Failure::Expired)?
        .map_err(failure)?;
        let reconnect = async {
            client
                .reconnect_host_with_ssh_auth(host, &lease.grant)
                .await
                .map_err(failure)?;
            finish().await
        };
        tokio::select! {
            biased;
            result=control::run(verifier,socket,control_cancellation)=>{
                Err(result.err().unwrap_or(Failure::Unavailable))
            },
            result=reconnect=>result,
        }
    };
    tokio::select! {
        biased;
        _=cancellation.wait_for(|value|*value)=>Err(Failure::Revoked),
        _=tokio::time::sleep_until(max_deadline)=>Err(Failure::Expired),
        result=effect=>result,
    }
}

#[cfg(test)]
#[path = "connect_tests.rs"]
mod tests;
