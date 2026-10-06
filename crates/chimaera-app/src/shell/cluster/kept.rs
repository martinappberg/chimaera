//! Fixed optional cluster contribution; ordinary Direct Slurm remains in its original host owner.
use super::*;
pub(super) use crate::account::ClusterPolicy as PolicyChange;
use std::sync::Arc;
#[derive(Clone)]
pub(super) struct Selected(Arc<dyn crate::account::DelegatedCluster>);

pub(super) fn close_key(shell: &Shell, key: &str) {
    if let Some(owner) = shell.pro.owner() {
        owner.close_cluster_key(key);
    }
}
pub(super) fn reconcile(shell: &Shell, alias: &str, overview: &ClusterOverview) {
    if let Some(owner) = shell.pro.owner() {
        owner.reconcile_cluster(alias, overview);
    }
}
pub(super) async fn select(shell: &Shell, alias: &str) -> Result<Option<Selected>, String> {
    match shell.pro.owner() {
        Some(owner) => owner
            .select_cluster(alias.to_owned())
            .await
            .map(|s| s.map(Selected)),
        // Without an owner every cluster is a direct one: nothing to read.
        None => Ok(None),
    }
}
pub(super) fn endpoints(shell: &Shell) -> Vec<(String, u16, String)> {
    shell
        .pro
        .owner()
        .map_or_else(Vec::new, |a| a.cluster_endpoints())
}
pub(super) fn current(shell: &Shell, key: &str, port: u16, token: &str) -> bool {
    shell
        .pro
        .owner()
        .is_some_and(|a| a.cluster_current(key, port, token))
}
pub(super) fn operation_id() -> String {
    chimaera_core::generate_token()
}
impl Selected {
    pub(super) fn ensure_cluster(&self) -> Result<(), String> {
        self.0.ensure_cluster()
    }
    pub(super) async fn read(&self, _shell: &Shell, op: Op) -> Result<Reply, String> {
        self.0.read(op).await
    }
    pub(super) async fn effect(&self, app: &AppHandle, op: Op) -> Result<Reply, String> {
        self.0.effect(app.clone(), op).await
    }
    pub(super) async fn overview(
        &self,
        _shell: &Shell,
        refresh: bool,
    ) -> Result<ClusterOverview, String> {
        self.0.overview(refresh).await
    }
    pub(super) async fn port(
        &self,
        _shell: &Shell,
        alias: &str,
        key: &str,
        job: &str,
        workspace: Option<&str>,
        token: &str,
    ) -> Result<u16, String> {
        self.0
            .port(
                alias.into(),
                key.into(),
                job.into(),
                workspace.map(str::to_owned),
                token.into(),
            )
            .await
    }
    pub(super) async fn policy(
        &self,
        app: &AppHandle,
        alias: &str,
        change: PolicyChange,
    ) -> Result<chimaera_remote::hosts::HostEntry, String> {
        self.0.policy(app.clone(), alias.into(), change).await
    }
}
