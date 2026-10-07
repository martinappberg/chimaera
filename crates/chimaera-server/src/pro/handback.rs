//! Waking an idle worker can replace its expired ownership epoch. Coordinate
//! that transition within one return attempt, without replaying ambiguous work.
use super::*;

pub(super) async fn prepare(
    state: &Arc<AppState>,
    config: &Configure,
    workspace: &str,
    host: &super::super::protocol::Host,
    holder: &str,
    initial_epoch: u64,
    force: bool,
) -> Result<Option<u64>> {
    let owner = super::project_host::policy_host::HandbackOwner::capture(
        state.clone(),
        config.clone(),
        workspace.into(),
        host.clone(),
        holder.into(),
        initial_epoch,
        force,
    )?;
    state
        .pro()
        .runtime()
        .context("optional_runtime_unavailable")?
        .prepare_handback(owner)
        .await
}
