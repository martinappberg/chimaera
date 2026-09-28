//! Native-only stable installation proof. Neither daemon nor webview receives it.
use super::{daemon_request, lock, store, KEYCHAIN_IO};
use crate::shell::Shell;
use anyhow::{ensure, Context, Result};
use chimaera_link::{
    Account, Client, ExecutionRecoveryAck, ExecutionRecoveryRequest, InstallationIdentity,
};

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
    for workspace in workspaces {
        let Some(id) = workspace["workspace_id"].as_str() else {
            continue;
        };
        let placement = client.workspace_placement(id).await?;
        let Some(holder) = placement.holder_id.as_deref() else {
            continue;
        };
        if holder == account.device_id
            || placement.preferred_installation_id.as_deref() != Some(&identity.installation_id)
        {
            continue;
        }
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
        ExecutionRecoveryAck::decode(200, &serde_json::to_vec(&ack)?, &request.recovery)?;
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

#[cfg(test)]
mod tests {
    use super::*;
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
