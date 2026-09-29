//! Native-only stable installation proof. Neither daemon nor webview receives it.
use super::{daemon_request, lock, store, KEYCHAIN_IO};
use crate::shell::Shell;
use anyhow::{ensure, Context, Result};
use chimaera_link::{
    Account, Client, ExecutionRecoveryAck, ExecutionRecoveryRequest, InstallationIdentity,
};
use std::collections::HashSet;

fn load_or_create(endpoint: &str, account: &str) -> Result<InstallationIdentity> {
    let _serialized = lock(&KEYCHAIN_IO);
    let config = chimaera_core::config_dir()
        .canonicalize()
        .context("could not resolve installation scope")?;
    let key = store::installation_key(endpoint, account, &config)?;
    let service = if chimaera_core::is_dev_build() {
        "chimaera.dev.pro.installation.v1"
    } else {
        "chimaera.pro.installation.v1"
    };
    let entry = keyring::Entry::new(service, &key)?;
    saved_identity(
        || match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        },
        |value| entry.set_password(value).map_err(Into::into),
    )
}
fn saved_identity(
    read: impl FnOnce() -> Result<Option<String>>,
    save: impl FnOnce(&str) -> Result<()>,
) -> Result<InstallationIdentity> {
    if let Some(value) = read()? {
        ensure!(value.len() <= 4096, "invalid installation credential");
        let identity: InstallationIdentity =
            serde_json::from_str(&value).context("invalid installation credential")?;
        identity.validate()?;
        return Ok(identity);
    }
    // Persist before any bind; replacing an unreadable proof would strand its owner.
    let identity = InstallationIdentity::generate();
    save(&serde_json::to_string(&identity)?)?;
    Ok(identity)
}

pub(super) async fn bind(
    state: &Shell,
    client: &Client,
    endpoint: &str,
    account: &Account,
) -> Result<InstallationIdentity> {
    let owned_endpoint = endpoint.to_owned();
    let account_id = account.account_id.clone();
    let identity =
        tokio::task::spawn_blocking(move || load_or_create(&owned_endpoint, &account_id)).await??;
    let display_name = super::machine_name();
    match client
        .bind_installation_named(&identity, Some(&display_name))
        .await
    {
        Ok(binding) => {
            ensure!(
                binding.device_id == account.device_id,
                "installation device mismatch"
            );
            return Ok(identity);
        }
        Err(error) if error.is::<chimaera_link::CleanReleaseRequired>() => {}
        Err(error) => return Err(error),
    }
    // Re-signing in must not revoke the old holder before its processes stop
    // and its latest snapshot is durable. Try its existing authority first.
    let _ = daemon_request(state, "POST", "/pro/sleep", None).await;
    match client
        .bind_installation_named(&identity, Some(&display_name))
        .await
    {
        Ok(binding) => {
            ensure!(
                binding.device_id == account.device_id,
                "installation device mismatch"
            );
            return Ok(identity);
        }
        Err(error) if error.is::<chimaera_link::CleanReleaseRequired>() => {}
        Err(error) => return Err(error),
    }
    let status = daemon_request(state, "GET", "/pro/status", None).await?;
    let workspaces = status["workspaces"]
        .as_array()
        .context("project recovery status unavailable")?;
    ensure!(workspaces.len() <= 128, "project recovery limit");
    let workers: HashSet<String> = lock(&state.pro.hosts)
        .values()
        .filter(|host| host.kind == chimaera_link::HostKind::Worker)
        .filter_map(|host| host.id.strip_prefix("worker-").map(str::to_owned))
        .collect();
    for workspace in workspaces {
        let Some(id) = workspace["workspace_id"].as_str() else {
            continue;
        };
        let recovered = async {
            let placement = client.workspace_placement(id).await?;
            let Some(holder) = recoverable_holder(
                &placement,
                &account.device_id,
                &identity.installation_id,
                &workers,
            ) else {
                return Ok(());
            };
            // The account additionally proves this holder belongs to this exact
            // installation. The local daemon verifies its old account/config too.
            let grant = client
                .installation_recovery(&identity, id, holder, placement.epoch)
                .await?;
            let request = ExecutionRecoveryRequest {
                endpoint: endpoint.to_owned(),
                account_id: account.account_id.clone(),
                installation_id: identity.installation_id.clone(),
                recovery: grant,
            };
            let ack = daemon_request(
                state,
                "POST",
                "/pro/execution/recover",
                Some(serde_json::to_value(&request)?),
            )
            .await?;
            ExecutionRecoveryAck::decode(200, &serde_json::to_vec(&ack)?, &request.recovery)
                .map(|_| ())
        }
        .await;
        // One project that cannot be recovered must not keep every other
        // project from continuing; the bind below decides whether any
        // release is still missing.
        if let Err(error) = recovered {
            tracing::warn!("project {id} could not be released for sign-in: {error:#}");
        }
    }
    let binding = client
        .bind_installation_named(&identity, Some(&display_name))
        .await?;
    ensure!(
        binding.device_id == account.device_id,
        "installation device mismatch"
    );
    Ok(identity)
}

/// Only this installation's previous *device* sign-in can be recovered with
/// its proof. A cloud machine holding a project homed here hands it back
/// through its own release; asking the account to recover it only fails.
fn recoverable_holder<'a>(
    placement: &'a chimaera_link::WorkspacePlacement,
    this_device: &str,
    installation: &str,
    workers: &HashSet<String>,
) -> Option<&'a str> {
    let holder = placement.holder_id.as_deref()?;
    let worker = workers.contains(holder)
        || placement
            .route_host_id
            .as_deref()
            .is_some_and(|route| route.starts_with("worker-"));
    (holder != this_device
        && !worker
        && placement.preferred_installation_id.as_deref() == Some(installation))
    .then_some(holder)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn placement(
        holder: &str,
        route: Option<&str>,
        home: &str,
    ) -> chimaera_link::WorkspacePlacement {
        serde_json::from_value(serde_json::json!({"workspace_id":"w-a","holder_id":holder,"route_host_id":route,"epoch":4,"policy_revision":1,"availability":"owned","preferred_installation_id":home,"checkpoint_id":null,"server_now":"2026-09-28T19:00:00Z","expires_at":"2026-09-28T19:01:30Z"})).unwrap()
    }
    #[test]
    fn only_this_installations_old_device_is_recovered() {
        let workers = HashSet::from(["m-cloud".to_owned()]);
        let old = placement("d-old", Some("device-d-old"), "i-home");
        assert_eq!(
            recoverable_holder(&old, "d-new", "i-home", &workers),
            Some("d-old")
        );
        // A revoked old device has no route but is still recoverable.
        let revoked = placement("d-old", None, "i-home");
        assert_eq!(
            recoverable_holder(&revoked, "d-new", "i-home", &workers),
            Some("d-old")
        );
        for skipped in [
            placement("m-cloud", None, "i-home"),
            placement("m-other", Some("worker-m-other"), "i-home"),
            placement("d-new", Some("device-d-new"), "i-home"),
            placement("d-old", Some("device-d-old"), "i-elsewhere"),
        ] {
            assert_eq!(
                recoverable_holder(&skipped, "d-new", "i-home", &workers),
                None
            );
        }
    }
    #[test]
    fn existing_installation_is_reused_without_rewriting_its_proof() {
        let existing = InstallationIdentity::generate();
        let restored = saved_identity(
            || Ok(Some(serde_json::to_string(&existing).unwrap())),
            |_| panic!("must not rewrite"),
        )
        .unwrap();
        assert_eq!(restored.installation_id, existing.installation_id);
        assert_eq!(restored.installation_proof, existing.installation_proof);
    }
    #[test]
    fn unavailable_or_damaged_store_never_creates_a_replacement_identity() {
        for raw in [None, Some("invalid".to_owned())] {
            let result = saved_identity(
                || {
                    raw.map(Some)
                        .ok_or_else(|| anyhow::anyhow!("synthetic locked store"))
                },
                |_| panic!("must not save"),
            );
            assert!(result.is_err());
        }
    }
    #[test]
    fn failed_first_save_never_returns_an_identity_to_bind() {
        assert!(saved_identity(
            || Ok(None),
            |_| Err(anyhow::anyhow!("synthetic write failure"))
        )
        .is_err());
        let captured = std::cell::RefCell::new(String::new());
        let identity = saved_identity(
            || Ok(None),
            |raw| {
                *captured.borrow_mut() = raw.to_owned();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<InstallationIdentity>(&captured.into_inner())
                .unwrap()
                .installation_id,
            identity.installation_id
        );
    }
}
