//! Explicit personal-cloud sign-out through the provider's official CLI.
//! We never open credential files or claim vendor-wide token revocation.
use super::{
    connect::{Attempt, Phase},
    home, process, ProviderDefinition, ProviderState,
};
use crate::AppState;
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};

// Adding a catalog/connect adapter must not advertise logout until its official
// removal contract and authoritative negative probe have been implemented.
pub(super) fn supported(id: &str) -> bool {
    matches!(id, "claude" | "codex" | "github")
}

async fn output(
    state: &AppState,
    attempt: &Attempt,
    bin: &Path,
    args: &[&str],
) -> Result<process::Output, &'static str> {
    process::output_tracked(&mut process::command(bin, args, &home(state)), |pid| {
        *crate::lock(&attempt.process) = Some(pid);
    })
    .await
}

/// Only the official status schema's account names are used as arguments. Names
/// never leave this adapter, and malformed/external auth fails closed.
fn github_accounts(output: &process::Output) -> Result<Vec<String>, &'static str> {
    if !output.success {
        return Err("invalid_status");
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|_| "invalid_status")?;
    let hosts = value["hosts"].as_object().ok_or("invalid_status")?;
    let Some(entries) = hosts.get("github.com") else {
        return Ok(Vec::new());
    };
    let entries = entries.as_array().ok_or("invalid_status")?;
    if entries.len() > 8 {
        return Err("provider_limit");
    }
    let mut names = Vec::new();
    for entry in entries {
        // Environment-supplied tokens are not owned by gh's local auth store.
        let source = entry["tokenSource"].as_str().ok_or("invalid_status")?;
        if source.ends_with("_TOKEN") {
            return Err("external_auth_unverified");
        }
        let name = entry["login"].as_str().ok_or("invalid_status")?;
        if name.is_empty()
            || name.len() > 39
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || name.starts_with('-')
        {
            return Err("invalid_status");
        }
        if !names.iter().any(|known| known == name) {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

pub(super) async fn run(
    state: &Arc<AppState>,
    def: &ProviderDefinition,
    attempt: &Attempt,
) -> Result<(), &'static str> {
    // Disconnect never installs a runtime. Unknown/missing runtimes cannot prove
    // removal of a saved credential; they need an honest recoverable failure.
    let bin = super::binary(state, def, false).await?;
    run_with_bin(state, def, attempt, &bin).await
}

/// The executable has already been resolved by the provider catalog. Keeping
/// this boundary explicit also lets tests exercise adapters without PATH edits.
pub(super) async fn run_with_bin(
    state: &Arc<AppState>,
    def: &ProviderDefinition,
    attempt: &Attempt,
    bin: &Path,
) -> Result<(), &'static str> {
    match def.id {
        "claude" => {
            if !output(state, attempt, bin, &["auth", "logout"])
                .await?
                .success
            {
                return Err("disconnect_failed");
            }
        }
        "codex" => {
            let mut rpc = process::Rpc::open(bin, &home(state)).await?;
            *crate::lock(&attempt.process) = rpc.process_id();
            rpc.request("account/logout", json!({})).await?;
        }
        "github" => {
            let args = &[
                "auth",
                "status",
                "--hostname",
                "github.com",
                "--json",
                "hosts",
            ];
            let accounts = github_accounts(&output(state, attempt, bin, args).await?)?;
            for account in accounts {
                if !output(
                    state,
                    attempt,
                    bin,
                    &[
                        "auth",
                        "logout",
                        "--hostname",
                        "github.com",
                        "--user",
                        &account,
                    ],
                )
                .await?
                .success
                {
                    return Err("disconnect_failed");
                }
            }
            attempt.update(Phase::Verifying, None, None);
            // A timeout or invalid-token row is not a logged-out account. Only
            // the official successful empty-store response establishes removal.
            return if github_accounts(&output(state, attempt, bin, args).await?)?.is_empty() {
                Ok(())
            } else {
                Err("disconnect_not_confirmed")
            };
        }
        _ => return Err("unsupported_provider"),
    }
    attempt.update(Phase::Verifying, None, None);
    // Bypass the public cache: this exact writer must see an authoritative
    // negative result after logout, not an outage, stale data or another adapter.
    let status = super::probe(state, def.id).await;
    if status.state == ProviderState::NeedsSignIn {
        Ok(())
    } else {
        Err("disconnect_not_confirmed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn status(value: Value, success: bool) -> process::Output {
        process::Output {
            success,
            code: Some(if success { 0 } else { 1 }),
            stdout: serde_json::to_vec(&value).unwrap(),
        }
    }
    #[test]
    fn github_disconnect_requires_empty_confirmed_store_not_failed_auth() {
        assert!(github_accounts(&status(json!({"hosts":{}}), true))
            .unwrap()
            .is_empty());
        assert!(github_accounts(&status(json!({"hosts":{}}), false)).is_err());
        let account = json!({"state":"timeout","active":true,"login":"fixture-user","tokenSource":"keyring","token":"must-not-leave-adapter"});
        let accounts =
            github_accounts(&status(json!({"hosts":{"github.com":[account]}}), true)).unwrap();
        assert_eq!(accounts, ["fixture-user"]);
        assert!(github_accounts(&status(
            json!({"hosts":{"github.com":[{"login":"fixture","tokenSource":"GH_TOKEN"}]}}),
            true
        ))
        .is_err());
        assert!(github_accounts(&status(
            json!({"hosts":{"github.com":[{"login":"--hostile","tokenSource":"keyring"}]}}),
            true
        ))
        .is_err());
    }
}
