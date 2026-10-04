//! Fixed native host effects. The account extension never receives the shell's mutable tables.
pub use crate::daemon::LocalDaemon;
pub use crate::shell::connect::{ConnectIntent, HostState};
/// Read-only presentation of a window captured from the original host registry.
/// Constructing this value never inserts or authorizes a native window.
#[derive(Clone)]
pub struct WindowScope {
    pub alias: Option<String>,
    pub ws: Option<String>,
    pub home_hub: bool,
    pub navigation_pending: bool,
    pub detached: bool,
}
impl WindowScope {
    pub fn home_hub(&self) -> bool {
        self.home_hub
    }
    pub fn navigation_pending(&self) -> bool {
        self.navigation_pending
    }
}
use crate::shell::{self, lock, Shell};
use tauri::{AppHandle, Emitter, Manager};
#[derive(Clone)]
pub struct AccountHost {
    app: AppHandle,
}
impl AccountHost {
    /// Complete a quit already admitted by the original unsaved guard. The
    /// private quit policy marks its one phase Settled before calling back.
    pub fn finish_quit(&self) {
        shell::finish_quit(&self.app);
    }
    pub fn finish_last_close(&self, original_label: &str) {
        if let Some(window) = self.app.get_webview_window(original_label) {
            let _ = window.destroy();
        }
    }
    pub fn close_handoff_window(&self) {
        if let Some(window) = self.app.get_webview_window("cloud-handoff") {
            let _ = window.destroy();
        }
    }
    pub fn raise_quit_question(&self, original_close: Option<&str>) {
        shell::raise_quit_question(&self.app, original_close);
    }
    pub fn install_power(&self) {
        shell::power::install(&self.app);
    }
    pub fn suitable_power() -> bool {
        shell::power::suitable()
    }
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
    pub fn local(&self) -> LocalDaemon {
        lock(&self.app.state::<Shell>().local).clone()
    }
    pub fn window_scope(&self, label: &str) -> Option<WindowScope> {
        self.app
            .state::<Shell>()
            .window_scope(label)
            .map(|scope| WindowScope {
                alias: scope.alias.clone(),
                ws: scope.ws.clone(),
                home_hub: scope.home_hub(),
                navigation_pending: scope.navigation_pending(),
                detached: scope.detached,
            })
    }
    pub fn is_link_device(&self, alias: &str) -> bool {
        lock(&self.app.state::<Shell>().registry).is_link_device(alias)
    }
    pub fn cache_host_entry(&self, alias: String, entry: chimaera_remote::hosts::HostEntry) {
        lock(&self.app.state::<Shell>().host_entries).insert(alias, entry);
    }
    pub async fn connected(&self, alias: &str) -> bool {
        self.app
            .state::<Shell>()
            .tunnels
            .lock()
            .await
            .contains_key(alias)
    }
    pub fn return_window(&self, origin: &str) -> Option<tauri::WebviewWindow> {
        let state = self.app.state::<Shell>();
        let original = lock(&state.windows)
            .contains_key(origin)
            .then(|| self.app.get_webview_window(origin))
            .flatten();
        let home = || {
            let label = lock(&state.windows)
                .iter()
                .find(|(_, s)| s.home_hub())
                .map(|(l, _)| l.clone())?;
            self.app.get_webview_window(&label)
        };
        original.or_else(home).or_else(|| {
            shell::show_local_home(&self.app, None).ok()?;
            home()
        })
    }
    pub fn forget_cloud_windows(&self, alias: &str) {
        shell::connect::forget_cloud_windows(&self.app.state::<Shell>(), alias);
    }
    pub async fn existing_link(&self, host: &chimaera_link::Host) -> Option<(u16, String)> {
        let state = self.app.state::<Shell>();
        let mut tunnels = state.tunnels.lock().await;
        let tunnel = tunnels
            .get_mut(&host.alias)
            .filter(|t| t.link_id() == Some(host.id.as_str()))?;
        tunnel.update_link(host);
        Some((tunnel.local_port, tunnel.manifest.token.clone()))
    }
    pub async fn host_signal(&self, host: &chimaera_link::Host) {
        let state = self.app.state::<Shell>();
        let endpoint = {
            let mut tunnels = state.tunnels.lock().await;
            let Some(t) = tunnels
                .get_mut(&host.alias)
                .filter(|t| t.link_id() == Some(host.id.as_str()))
            else {
                return;
            };
            t.update_link(host);
            (
                t.local_port,
                t.manifest.token.clone(),
                t.manifest.build.clone(),
            )
        };
        let connected =
            host.status == chimaera_link::HostStatus::Connected && host.daemon.is_some();
        if connected {
            lock(&state.unhealthy_tunnels).remove(&host.alias);
        } else {
            lock(&state.unhealthy_tunnels).insert(host.alias.clone());
        }
        let _ = self.app.emit(
            "host-status",
            shell::connect::HostStatus {
                alias: host.alias.clone(),
                status: if connected { "connected" } else { "down" },
                local_port: Some(endpoint.0),
                token: connected.then_some(endpoint.1),
                error: host.error.clone(),
                reason: (!connected).then(|| "Your Pro connection is reconnecting.".into()),
                build: endpoint.2,
                node: None,
            },
        );
    }
    pub async fn close_delegated_tunnels(&self) {
        let state = self.app.state::<Shell>();
        let removed: Vec<_> = {
            let mut tunnels = state.tunnels.lock().await;
            let aliases: Vec<_> = tunnels
                .iter()
                .filter(|(_, t)| t.link_id().is_some())
                .map(|(a, _)| a.clone())
                .collect();
            aliases
                .into_iter()
                .filter_map(|a| tunnels.remove(&a).map(|t| (a, t)))
                .collect()
        };
        for (alias, tunnel) in removed {
            let port = tunnel.local_port;
            tunnel.close().await;
            let _ = self.app.emit(
                "host-status",
                shell::connect::HostStatus {
                    alias,
                    status: "down",
                    local_port: Some(port),
                    token: None,
                    error: None,
                    reason: Some("Signed out of Chimaera Pro".into()),
                    build: None,
                    node: None,
                },
            );
        }
    }
    pub async fn land_cluster(
        &self,
        host: &chimaera_link::Host,
        authority: std::sync::Arc<tokio::sync::OwnedMutexGuard<()>>,
        current: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<HostState, String> {
        let state = self.app.state::<Shell>();
        let previous =
            shell::connect::remove_current_keeper_tunnel(&state.tunnels, &host.alias, current)
                .await?;
        if let Some(previous) = previous {
            previous.close().await;
        }
        if !current() {
            return Err("Account changed while connecting".into());
        }
        let found = chimaera_remote::ClusterHost {
            host: host.alias.clone(),
            scheduler: chimaera_core::slurm::Scheduler::Slurm,
            login_daemon: None,
        };
        Ok(
            shell::connect::landed_on_cluster(&self.app, &host.alias, &found, Some(authority))
                .await
                .mark_kept(),
        )
    }
    pub async fn publish_link(
        &self,
        host: chimaera_link::Host,
        port: u16,
        transport: Option<Box<dyn super::DelegatedTransport>>,
        current: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<HostState, String> {
        let state = self.app.state::<Shell>();
        shell::authorize_scope_origin(&self.app, Some(&host.alias), port)
            .map_err(|e| e.to_string())?;
        let old = {
            let mut tunnels = state.tunnels.lock().await;
            if !current() {
                return Err("Account changed while connecting".into());
            }
            transport
                .map(|transport| shell::tunnel::Tunnel::link(&host, port, transport))
                .transpose()
                .map_err(|e| e.to_string())?
                .and_then(|t| tunnels.insert(host.alias.clone(), t))
        };
        if let Some(old) = old {
            old.close().await;
        }
        let mut entry = if host.kind == chimaera_link::HostKind::Ssh {
            let alias = host.alias.clone();
            with_hosts(move |hosts| {
                hosts.set_kept(&alias, true)?;
                hosts.record_connected(&alias)
            })
            .await?
        } else {
            shell::connect::host_entry(&host.alias).await
        };
        entry.kept = true;
        lock(&state.host_entries).insert(host.alias.clone(), entry);
        let reply = shell::connect::publish_connected_state(&self.app, &state, &host.alias)
            .await
            .ok_or("Host disconnected while connecting")?;
        let token = host
            .daemon
            .as_ref()
            .ok_or("host daemon is unavailable")?
            .token
            .clone();
        shell::connect::reopen_windows(&self.app, &host.alias, port, &token);
        Ok(reply)
    }
    pub async fn connect(&self, alias: String, reconnect: bool) -> Result<HostState, String> {
        shell::connect::do_connect(&self.app, alias, reconnect).await
    }
}
/// The existing serialized, capped host-store worker is shared by both assemblies.
/// This is native Rust integration, never an IPC, HTTP, filesystem-path or token broker.
pub async fn with_hosts<T: Send + 'static>(
    work: impl FnOnce(&mut chimaera_remote::hosts::HostsStore) -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    shell::connect::with_hosts(work).await
}
