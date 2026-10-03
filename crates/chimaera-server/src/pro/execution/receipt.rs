//! Immutable receipt selection: moving Git branch names never choose hydration.
use super::wire::{Checkpoint, Continuation};
use crate::pro::{
    engine,
    protocol::{Baton, Configure},
    transport,
};
use anyhow::{ensure, Context, Result};
use std::{path::Path, time::Duration};

pub(in crate::pro) fn validate(receipt: &Checkpoint) -> Result<()> {
    let oid = |value: &str| {
        (value.len() == 40 || value.len() == 64)
            && value
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    };
    ensure!(
        crate::pro::valid_id(&receipt.id)
            && receipt.sequence > 0
            && crate::pro::valid_id(&receipt.source_holder_id)
            && receipt.source_epoch > 0
            && oid(&receipt.working_tree_oid)
            && oid(&receipt.config_oid)
            && oid(&receipt.handoff_oid),
        "invalid durable checkpoint"
    );
    Ok(())
}
pub(in crate::pro) fn revision<'a>(
    receipt: Option<&'a Checkpoint>,
    branch: &str,
) -> Result<&'a str> {
    match (receipt, branch) {
        (Some(receipt), "main") => Ok(&receipt.working_tree_oid),
        (Some(receipt), "config") => Ok(&receipt.config_oid),
        (Some(receipt), "handoff") => Ok(&receipt.handoff_oid),
        (None, "main") => Ok("refs/heads/main"),
        (None, "config") => Ok("refs/heads/config"),
        (None, "handoff") => Ok("refs/heads/handoff"),
        _ => anyhow::bail!("invalid checkpoint branch"),
    }
}
pub(in crate::pro) async fn pin(cache: &Path, receipt: &Checkpoint) -> Result<()> {
    validate(receipt)?;
    for (branch, oid) in [
        ("main", &receipt.working_tree_oid),
        ("config", &receipt.config_oid),
        ("handoff", &receipt.handoff_oid),
    ] {
        let bytes = transport::git_output(
            transport::git(cache, None).await?,
            &["rev-parse", "--verify", &format!("{oid}^{{commit}}")],
            vec![],
        )
        .await?;
        ensure!(
            std::str::from_utf8(&bytes)?.trim() == oid,
            "checkpoint object mismatch"
        );
        transport::git_output(
            transport::git(cache, None).await?,
            &["update-ref", &format!("refs/heads/{branch}"), oid],
            vec![],
        )
        .await?;
    }
    Ok(())
}
pub(in crate::pro) async fn published(
    config: &Configure,
    workspace: &str,
    epoch: u64,
    oids: [&str; 3],
    continuation: Continuation,
) -> Result<Checkpoint> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let receipt = if config.recovery {
                let response = engine::account(
                    config,
                    "/v2/recovery/checkpoint",
                    "POST",
                    Some(&serde_json::json!({"workspace_id":workspace,"epoch":epoch})),
                )
                .await?;
                #[derive(serde::Deserialize)]
                struct Reply {
                    checkpoint: Option<Checkpoint>,
                }
                response.json::<Reply>()?.checkpoint
            } else {
                let baton: Baton =
                    engine::account(config, &super::path(config, workspace, ""), "GET", None)
                        .await?
                        .json()?;
                ensure!(
                    baton.workspace_id == workspace
                        && baton.holder_id.as_deref() == Some(&config.delegation.device_id)
                        && baton.epoch == epoch,
                    "ownership changed before checkpoint acknowledgment"
                );
                baton.checkpoint
            };
            if let Some(receipt) = receipt {
                validate(&receipt)?;
                if receipt.source_epoch == epoch
                    && receipt.source_holder_id == config.delegation.device_id
                    && [
                        &receipt.working_tree_oid,
                        &receipt.config_oid,
                        &receipt.handoff_oid,
                    ]
                    .into_iter()
                    .zip(oids)
                    .all(|(got, want)| got == want)
                {
                    ensure!(
                        receipt.continuation == continuation,
                        "checkpoint continuation mismatch"
                    );
                    return Ok(receipt);
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("durable checkpoint acknowledgment is pending")?
}
