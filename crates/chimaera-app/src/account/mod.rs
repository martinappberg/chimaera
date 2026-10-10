//! Optional native account contributions. The free assembly has no owner or credential initializer.
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};
pub mod commands;
pub mod host;
pub mod types;
pub type Task<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub type BorrowedTask<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub(crate) const ABSENT: &str = "Not available in this build.";
/// The factory runs once, after the original shell is managed and before restoration.
pub type Factory = Arc<dyn Fn(tauri::AppHandle) -> Arc<dyn AccountExtension> + Send + Sync>;
#[derive(Default)]
pub(crate) struct OptionalAccount(OnceLock<Arc<dyn AccountExtension>>);
impl OptionalAccount {
    pub(crate) fn install(&self, owner: Arc<dyn AccountExtension>) {
        assert!(self.0.set(owner).is_ok(), "account owner already installed");
    }
    pub(crate) fn owner(&self) -> Option<&Arc<dyn AccountExtension>> {
        self.0.get()
    }
    pub(crate) fn is_device(&self, alias: &str) -> bool {
        self.owner().is_some_and(|a| a.is_device(alias))
    }
    pub(crate) fn is_cloud(&self, alias: &str) -> bool {
        self.owner().is_some_and(|a| a.is_cloud(alias))
    }
    pub(crate) fn active(&self) -> bool {
        self.owner().is_some_and(|a| a.active())
    }
    pub(crate) fn hosts(&self) -> Vec<chimaera_link::Host> {
        self.owner().map_or_else(Vec::new, |a| a.hosts())
    }
}
/// A private assembly supplies one retained owner. These methods are named,
/// typed operations; there is no token, URL, arbitrary HTTP or invoke broker.
pub trait AccountExtension: Send + Sync {
    fn start(&self, app: tauri::AppHandle);
    fn stop(&self, app: tauri::AppHandle) -> Task<()>;
    fn reconfigure(&self, app: tauri::AppHandle);
    fn is_device(&self, alias: &str) -> bool;
    fn is_cloud(&self, alias: &str) -> bool;
    fn active(&self) -> bool;
    fn hosts(&self) -> Vec<chimaera_link::Host>;
    fn connect(
        &self,
        app: tauri::AppHandle,
        alias: String,
        reconnect: bool,
        intent: host::ConnectIntent,
    ) -> Task<Result<Option<host::HostState>, String>>;
    fn select_cluster(
        &self,
        alias: String,
    ) -> Task<Result<Option<Arc<dyn DelegatedCluster>>, String>>;
    fn cluster_selected(&self, alias: &str) -> bool;
    /// A fixed code the cluster page turns into a quiet line under the
    /// login-node terminal action (for example, that it signs in on its own
    /// although the cluster is held elsewhere). None says nothing.
    fn cluster_terminal_note(&self, _alias: &str) -> Option<&'static str> {
        None
    }
    fn close_cluster_links(&self);
    fn close_cluster_key(&self, key: &str);
    fn reconcile_cluster(&self, alias: &str, overview: &chimaera_remote::cluster::ClusterOverview);
    fn cluster_endpoints(&self) -> Vec<(String, u16, String)>;
    fn cluster_current(&self, key: &str, port: u16, token: &str) -> bool;
    fn pro_status(&self, app: tauri::AppHandle) -> Task<Result<types::account::Status, String>>;
    fn pro_refresh_account(
        &self,
        app: tauri::AppHandle,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_sign_in(
        &self,
        app: tauri::AppHandle,
        window: tauri::WebviewWindow,
        screen_hint: Option<types::auth::ScreenHint>,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_take_return(
        &self,
        app: tauri::AppHandle,
        window: tauri::WebviewWindow,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<bool, String>>;
    fn pro_cancel_sign_in(
        &self,
        app: tauri::AppHandle,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_sign_out(
        &self,
        app: tauri::AppHandle,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_sign_out_everywhere(
        &self,
        app: tauri::AppHandle,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_hosts(
        &self,
        app: tauri::AppHandle,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<Vec<types::account::KeptHost>, String>>;
    fn pro_set_host_kept(
        &self,
        app: tauri::AppHandle,
        alias: String,
        kept: bool,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_devices(
        &self,
        app: tauri::AppHandle,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<Vec<chimaera_link::Device>, String>>;
    fn pro_revoke_device(
        &self,
        app: tauri::AppHandle,
        device_id: String,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_mirror_status(
        &self,
        app: tauri::AppHandle,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<serde_json::Value, String>>;
    fn pro_set_never_mirror(
        &self,
        app: tauri::AppHandle,
        workspace_id: String,
        never_mirror: bool,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_billing_checkout(
        &self,
        app: tauri::AppHandle,
        window: tauri::WebviewWindow,
        plan: chimaera_link::Plan,
        interval: chimaera_link::BillingInterval,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_billing_portal(
        &self,
        app: tauri::AppHandle,
        window: tauri::WebviewWindow,
        target: Option<chimaera_link::BillingPortalTarget>,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_cancel_billing(
        &self,
        app: tauri::AppHandle,
        attempt_id: Option<u64>,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_personal_provider_mode(
        &self,
        app: tauri::AppHandle,
        window: tauri::WebviewWindow,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<chimaera_link::providers::ModeReply, String>>;
    fn pro_personal_provider_catalog(
        &self,
        app: tauri::AppHandle,
        window: tauri::WebviewWindow,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<chimaera_link::providers::CatalogPage, String>>;
    fn pro_personal_provider_command(
        &self,
        app: tauri::AppHandle,
        window: tauri::WebviewWindow,
        original: chimaera_link::providers::Original,
        payload: String,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<chimaera_link::providers::CommandResult, String>>;
    fn pro_personal_provider_operation(
        &self,
        app: tauri::AppHandle,
        window: tauri::WebviewWindow,
        original: chimaera_link::providers::Original,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<chimaera_link::providers::CommandResult, String>>;
    fn pro_personal_provider_open(
        &self,
        app: tauri::AppHandle,
        window: tauri::WebviewWindow,
        original: chimaera_link::providers::Original,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_cloud_projects(
        &self,
        app: tauri::AppHandle,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<Vec<types::projects::CloudProject>, String>>;
    fn pro_copy_project(
        &self,
        app: tauri::AppHandle,
        window: tauri::WebviewWindow,
        workspace_id: String,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<Option<types::projects::CloudProjectOpen>, String>>;
    fn pro_take_over_project(
        &self,
        app: tauri::AppHandle,
        workspace_id: String,
        expected_epoch: u64,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<(), String>>;
    fn pro_cloud_status(
        &self,
        app: tauri::AppHandle,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<types::cloud::CloudStatus, String>>;
    fn pro_cloud_request(
        &self,
        app: tauri::AppHandle,
        request: types::cloud::Request,
        expected_account_lifetime: Option<String>,
    ) -> Task<Result<serde_json::Value, String>>;
}
/// Each delegate retains its original account generation and operation owner.
pub trait DelegatedCluster: Send + Sync {
    fn ensure_cluster(&self) -> Result<(), String>;
    fn read(
        &self,
        operation: chimaera_link::ClusterOperation,
    ) -> Task<Result<chimaera_link::ClusterReply, String>>;
    fn effect(
        &self,
        app: tauri::AppHandle,
        operation: chimaera_link::ClusterOperation,
    ) -> Task<Result<chimaera_link::ClusterReply, String>>;
    fn overview(
        &self,
        refresh: bool,
    ) -> Task<Result<chimaera_remote::cluster::ClusterOverview, String>>;
    /// The overview a job action decides on. `overview(false)` may answer
    /// from a page's recent read; this one asks the keeper (a read already
    /// in flight may be shared). Defaults to `overview(false)` for delegates
    /// that keep no page cache.
    fn current_overview(&self) -> Task<Result<chimaera_remote::cluster::ClusterOverview, String>> {
        self.overview(false)
    }
    fn port(
        &self,
        alias: String,
        key: String,
        job: String,
        workspace: Option<String>,
        token: String,
    ) -> Task<Result<u16, String>>;
    /// `port`, answering also the token the route's daemon answers to. A
    /// page's listing can name an older token than the route it opens, so a
    /// window uses this one. Defaults to `port` with the caller's token for
    /// delegates that match routes by token.
    fn open_route(
        &self,
        alias: String,
        key: String,
        job: String,
        workspace: Option<String>,
        token: String,
    ) -> Task<Result<(u16, String), String>> {
        let port = self.port(alias, key, job, workspace, token.clone());
        Box::pin(async move { Ok((port.await?, token)) })
    }
    fn policy(
        &self,
        app: tauri::AppHandle,
        alias: String,
        change: ClusterPolicy,
    ) -> Task<Result<chimaera_remote::hosts::HostEntry, String>>;
}
#[derive(Clone, Copy)]
pub enum ClusterPolicy {
    LoginServe(bool),
    NotCluster(bool),
}
/// The retained delegated transport owns actual close, not a detached observer.
pub trait DelegatedTransport: Send + Sync {
    fn close(self: Box<Self>);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn free_assembly_has_no_account_owner_or_routable_account_snapshot() {
        let account = OptionalAccount::default();
        assert!(account.owner().is_none());
        assert!(!account.active());
        assert!(!account.is_cloud("saved-cloud"));
        assert!(!account.is_device("saved-device"));
        assert!(account.hosts().is_empty());
        let status = serde_json::to_value(types::account::Status::absent()).unwrap();
        assert_eq!(status["available"], false);
        assert_eq!(status["signed_in"], false);
        assert_eq!(status["initializing"], false);
        assert!(status["plan"].is_null());
    }
}
